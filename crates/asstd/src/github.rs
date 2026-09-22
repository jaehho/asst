//! Linked GitHub repos, one pass at a time in this one task: at start, when
//! links or tasks change, and every sync interval. The first page's ETag is
//! kept per repo, so a quiet repo costs one request that GitHub doesn't
//! count against the rate limit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use asst_core::caldav::RemoteError;
use asst_core::github::{self, Change, GitHub, Issue, Op, Pair, Ref};
use asst_core::store::Store;
use asst_core::task::Edit;
use chrono::Utc;

use crate::daemon::{Daemon, Error, Result};

pub async fn run(daemon: Arc<Daemon>) {
    let mut client: Option<GitHub> = None;
    // repo → (the first page's ETag, every issue as of it)
    let mut cache: HashMap<String, (String, Vec<Issue>)> = HashMap::new();
    let mut warned = false;
    loop {
        let links = daemon.store().gh_links().unwrap_or_default();
        if !links.is_empty() && client.is_none() {
            client = match token().await.map(|t| GitHub::new(&t)) {
                Some(Ok(gh)) => Some(gh),
                Some(Err(e)) => {
                    log::warn!("{e}");
                    None
                }
                None => {
                    if !warned {
                        log::warn!(
                            "no GitHub token (GITHUB_TOKEN or `gh auth login`); linked repos wait"
                        );
                    }
                    warned = true;
                    None
                }
            };
        }
        if let Some(gh) = &client {
            for (repo, list) in &links {
                match pass(&daemon, gh, &mut cache, repo, list).await {
                    Ok(()) => {}
                    Err(Error::Remote(RemoteError::Unauthorized)) => {
                        log::warn!("GitHub rejected the token; asking for it again next round");
                        client = None;
                        break;
                    }
                    Err(Error::Remote(RemoteError::Network(m))) => {
                        log::info!("GitHub offline: {m}");
                        break;
                    }
                    Err(e) => log::warn!("syncing {repo}: {e}"),
                }
            }
        }
        let interval = Duration::from_secs(daemon.config().interval.max(15));
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = daemon.wake_github.notified() => {}
        }
    }
}

/// `$GITHUB_TOKEN`, or what `gh` is signed in with.
async fn token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    let out = tokio::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .ok()?;
    let t = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// One repo against its list. An op that fails is logged and left for the
/// next pass, its pair as it was; a lost connection stops the pass.
async fn pass(
    daemon: &Daemon,
    gh: &GitHub,
    cache: &mut HashMap<String, (String, Vec<Issue>)>,
    repo: &str,
    list: &str,
) -> Result<()> {
    let etag = cache.get(repo).map(|(e, _)| e.as_str());
    let issues = match gh.issues(repo, etag).await? {
        Some((issues, Some(etag))) => {
            cache.insert(repo.to_string(), (etag, issues.clone()));
            issues
        }
        Some((issues, None)) => {
            cache.remove(repo);
            issues
        }
        None => cache[repo].1.clone(),
    };
    let (ops, hrefs) = {
        let store = daemon.store();
        let rows = store.list_tasks(list)?;
        let tasks: Vec<Ref> = rows
            .iter()
            .map(|r| Ref {
                id: r.task.uid.clone(),
                title: r.task.summary.clone(),
                open: r.task.is_open(),
                body: r.task.description.clone(),
                priority: r.task.priority,
                labels: Vec::new(),
            })
            .collect();
        let hrefs: HashMap<String, String> =
            rows.into_iter().map(|r| (r.task.uid, r.href)).collect();
        (github::plan(&issues, &tasks, &store.gh_pairs(repo)?), hrefs)
    };
    if ops.is_empty() {
        return Ok(());
    }
    // What this pass writes changes the listing: read it whole next time.
    cache.remove(repo);
    let mut changed = false;
    let mut out = Ok(());
    for op in ops {
        match apply(daemon, gh, repo, list, &hrefs, op, &mut changed).await {
            Ok(()) => {}
            Err(Error::Remote(e)) if e.is_fatal() => {
                out = Err(e.into());
                break;
            }
            Err(e) => log::warn!("syncing {repo}: {e}"),
        }
    }
    if changed {
        daemon.changed_from_github();
    }
    out
}

/// One op. `changed` is set once it changes the store, even if the op
/// then fails.
async fn apply(
    daemon: &Daemon,
    gh: &GitHub,
    repo: &str,
    list: &str,
    hrefs: &HashMap<String, String>,
    op: Op,
    changed: &mut bool,
) -> Result<()> {
    let href = |uid: &str| {
        hrefs
            .get(uid)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("no task {uid} in the list")))
    };
    match op {
        Op::Merge {
            number,
            uid,
            task,
            issue,
            title,
            open,
            body,
            priority,
        } => {
            if !task.is_empty() {
                edit_task(&mut daemon.store(), &href(&uid)?, &task)?;
                *changed = true;
            }
            if !issue.is_empty() {
                gh.update(repo, number, &issue, false).await?;
            }
            let pair = Pair {
                number,
                uid,
                title,
                open,
                body,
                priority,
            };
            daemon.store().gh_pair(repo, &pair)?;
        }
        Op::CloseIssue { number } => {
            let close = Change {
                open: Some(false),
                ..Change::default()
            };
            match gh.update(repo, number, &close, true).await {
                Ok(()) | Err(RemoteError::NotFound) => {}
                Err(e) => return Err(e.into()),
            }
            daemon.store().gh_unpair(repo, number)?;
        }
        Op::CompleteTask { number, uid } => {
            // Missing from a listing right after a write may just be the
            // listing lagging; ask for the issue itself before completing.
            if gh.exists(repo, number).await? {
                return Ok(());
            }
            let done = Change {
                open: Some(false),
                ..Change::default()
            };
            let mut store = daemon.store();
            edit_task(&mut store, &href(&uid)?, &done)?;
            *changed = true;
            store.gh_unpair(repo, number)?;
        }
        Op::Unpair { number } => {
            daemon.store().gh_unpair(repo, number)?;
        }
        Op::Adopt {
            number,
            uid,
            title,
            body,
            priority,
        } => {
            let pair = Pair {
                number,
                uid,
                title,
                open: true,
                body,
                priority,
            };
            daemon.store().gh_pair(repo, &pair)?;
        }
        Op::NewTask {
            number,
            title,
            body,
            priority,
        } => {
            let mut edits = vec![Edit::Summary(title.clone())];
            if let Some(b) = &body {
                edits.push(Edit::Description(Some(b.clone())));
            }
            if priority > 0 {
                edits.push(Edit::Priority(priority));
            }
            let mut store = daemon.store();
            let row = store.create(list, &edits, Utc::now())?;
            *changed = true;
            let pair = Pair {
                number,
                uid: row.task.uid,
                title,
                open: true,
                body,
                priority,
            };
            store.gh_pair(repo, &pair)?;
        }
        Op::NewIssue {
            uid,
            title,
            body,
            priority,
        } => {
            let issue = gh.create(repo, &title, body.as_deref(), priority).await?;
            let pair = Pair {
                number: issue.id,
                uid,
                title: issue.title,
                open: true,
                body,
                priority,
            };
            daemon.store().gh_pair(repo, &pair)?;
        }
    }
    Ok(())
}

fn edit_task(store: &mut Store, href: &str, change: &Change) -> Result<()> {
    let now = Utc::now();
    if let Some(t) = &change.title {
        store.edit(href, &[Edit::Summary(t.clone())], now)?;
    }
    if let Some(b) = &change.body {
        store.edit(href, &[Edit::Description(b.clone())], now)?;
    }
    if let Some(p) = change.priority {
        store.edit(href, &[Edit::Priority(p)], now)?;
    }
    match change.open {
        Some(true) => {
            store.edit(href, &[Edit::Reopen], now)?;
        }
        Some(false) => {
            store.complete(href, now)?;
            store.unsnooze(href)?;
        }
        None => {}
    }
    Ok(())
}
