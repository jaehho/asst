//! Linked GitHub repos, one pass at a time in this one task: at start, when
//! links or tasks change, and every sync interval. Each pass begins by
//! looking at the repo itself (the id a rename leaves alone, its name, its
//! archived flag) and reads its issues only when that is the repo that was
//! linked. The first page's ETag is kept per repo, so a quiet repo costs
//! requests that GitHub doesn't count against the rate limit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use asst_core::api::LinkState;
use asst_core::caldav::RemoteError;
use asst_core::github::{
    self, Change, GitHub, Issue, Observed, Op, Pair, Ref, RepoInfo, Target, Verdict,
};
use asst_core::store::{GhLinkRow, NewLog, Store};
use asst_core::task::Edit;
use chrono::{DateTime, Utc};

use crate::daemon::{Daemon, Error, Result};

/// A refusal that says nothing about when to come back is waited out this
/// long.
const REFUSED_WAIT: chrono::Duration = chrono::Duration::minutes(15);

/// A repo whose id GitHub cannot find is asked for by its name this often
/// at most, to tell a name taken by another repo from a repo that is gone.
const NAME_LOOKUP_EVERY: chrono::Duration = chrono::Duration::hours(1);

/// Who made a log row.
const AUTO: &str = "auto";

/// What the GitHub task remembers between rounds. None of it survives a
/// restart: it only saves requests, and what a restart forgets is looked at
/// again.
#[derive(Default)]
struct Memory {
    /// repo → (the first page's ETag, every issue as of it)
    issues: HashMap<String, (String, Vec<Issue>)>,
    /// repo id → (the ETag of its record, the record)
    records: HashMap<u64, (String, RepoInfo)>,
    /// repo → no request before then: GitHub asked to be left alone
    waits: HashMap<String, DateTime<Utc>>,
    /// repo → when its name was last asked for after its id was not found
    lookups: HashMap<String, DateTime<Utc>>,
}

/// How a round ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Round {
    /// Every link was looked at.
    Done,
    /// The connection was lost; the rest wait for the next round.
    Offline,
    /// GitHub rejected the token.
    Rejected,
}

/// What the look at a link's repo lets its pass do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// It is the linked repo: read its issues.
    Go,
    /// Not now: it moved, is not the linked repo, or could not be told.
    Stop,
}

pub async fn run(daemon: Arc<Daemon>) {
    let mut client: Option<GitHub> = None;
    let mut memory = Memory::default();
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
        if client.is_none() {
            for (repo, _) in &links {
                daemon.gh_pass_done(
                    repo,
                    Err("no GitHub token (GITHUB_TOKEN, or `gh auth login`)".into()),
                );
            }
        }
        if let Some(gh) = &client
            && round(&daemon, gh, &mut memory, Utc::now()).await == Round::Rejected
        {
            client = None;
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

/// Every link once: look at its repo, and if that is the linked repo, sync
/// its issues. A link that fails does not stop the ones after it, but a lost
/// connection or a rejected token does, since nothing else will get through.
/// `now` is a parameter so a test can move the clock.
async fn round(daemon: &Daemon, gh: &GitHub, memory: &mut Memory, now: DateTime<Utc>) -> Round {
    let links = daemon.store().gh_link_rows().unwrap_or_default();
    for row in &links {
        match link_round(daemon, gh, memory, row, now).await {
            Ok(true) => daemon.gh_pass_done(&row.repo, Ok(())),
            Ok(false) => {}
            Err(Error::Remote(RemoteError::Unauthorized)) => {
                log::warn!("GitHub rejected the token; asking for it again next round");
                daemon.gh_pass_done(&row.repo, Err("GitHub rejected the token".into()));
                return Round::Rejected;
            }
            Err(Error::Remote(RemoteError::Network(m))) => {
                log::info!("GitHub offline: {m}");
                return Round::Offline;
            }
            Err(e) => {
                log::warn!("syncing {}: {e}", row.repo);
                daemon.gh_pass_done(&row.repo, Err(e.to_string()));
            }
        }
    }
    Round::Done
}

/// One link: whether its issues were synced.
async fn link_round(
    daemon: &Daemon,
    gh: &GitHub,
    memory: &mut Memory,
    row: &GhLinkRow,
    now: DateTime<Utc>,
) -> Result<bool> {
    if identify(daemon, gh, memory, row, now).await? == Gate::Stop {
        return Ok(false);
    }
    pass(daemon, gh, memory, row).await?;
    Ok(true)
}

/// Look at the link's repo and judge it against what is stored. Nothing is
/// read from a repo that is not confirmed to be the linked one: a repo made
/// again under an old name would otherwise have its issues merged with the
/// old tasks, and the tasks whose numbers it lacks completed.
async fn identify(
    daemon: &Daemon,
    gh: &GitHub,
    memory: &mut Memory,
    row: &GhLinkRow,
    now: DateTime<Utc>,
) -> Result<Gate> {
    let repo = row.repo.as_str();
    if memory.waits.get(repo).is_some_and(|until| now < *until) {
        return Ok(Gate::Stop);
    }
    memory.waits.remove(repo);
    let Some(seen) = look(gh, memory, row, now).await? else {
        return Ok(Gate::Stop);
    };
    let baseline = row.archived.map(|t| t > 0);
    let verdict = github::judge(row.repo_id, repo, baseline, &seen);
    apply_verdict(daemon, memory, row, verdict, &seen, now)
}

/// Do what a verdict calls for, and say whether the link's issues may be
/// read. The only things changed are the link itself (its id, its name, its
/// archived baseline, a rename marker) and the log; the list is not touched.
fn apply_verdict(
    daemon: &Daemon,
    memory: &mut Memory,
    row: &GhLinkRow,
    verdict: Verdict,
    seen: &Observed,
    now: DateTime<Utc>,
) -> Result<Gate> {
    let repo = row.repo.as_str();
    let record = match seen {
        Observed::Record(info, _) => Some(info),
        _ => None,
    };
    match (verdict, record) {
        (Verdict::Fine, info) => {
            first_sight(daemon, row, info, now)?;
            daemon.gh_release(repo);
            Ok(Gate::Go)
        }
        (Verdict::LearnId(id), info) => {
            if !daemon.store().gh_learn_id(repo, id)? {
                return duplicate_id(daemon, row, id, now);
            }
            first_sight(daemon, row, info, now)?;
            daemon.gh_release(repo);
            Ok(Gate::Go)
        }
        (Verdict::Archived | Verdict::Unarchived, Some(info)) => {
            retire(daemon, row, info.archived || info.disabled, now)?;
            daemon.gh_release(repo);
            Ok(Gate::Go)
        }
        (Verdict::Renamed { to, witnessed }, Some(info)) => {
            rename(daemon, memory, row, &to, info.id, witnessed, now)
        }
        (Verdict::Replaced { found }, _) => replaced(daemon, row, found, now),
        _ => Ok(unsettled(daemon, memory, row, seen, now)),
    }
}

/// What GitHub says about the linked repo, by its id when it is known, else
/// by its name; `None` when there is nothing new to learn. A 304 is read as
/// the record its ETag came with, a redirect from the old name is followed
/// to the repo's record, and an id GitHub cannot find is checked against the
/// name (at most once an hour), since a repo made again under the name has
/// a different id.
async fn look(
    gh: &GitHub,
    memory: &mut Memory,
    row: &GhLinkRow,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Observed>, RemoteError> {
    let repo = row.repo.as_str();
    let mut seen = match row.repo_id {
        Some(id) => {
            let etag = memory.records.get(&id).map(|(etag, _)| etag.as_str());
            gh.repo(Target::Id(id), etag).await?
        }
        None => gh.repo(Target::Name(repo), None).await?,
    };
    match &seen {
        Observed::Record(info, Some(etag)) => {
            memory.records.insert(info.id, (etag.clone(), info.clone()));
        }
        Observed::Record(info, None) => {
            memory.records.remove(&info.id);
        }
        _ => {}
    }
    let again = match (&seen, row.repo_id) {
        (Observed::Unchanged, Some(id)) => memory
            .records
            .get(&id)
            .map(|(_, info)| Observed::Record(info.clone(), None)),
        (Observed::Moved { id }, None) => Some(gh.repo(Target::Id(*id), None).await?),
        (Observed::Missing, Some(_)) => {
            if memory
                .lookups
                .get(repo)
                .is_some_and(|at| now - *at < NAME_LOOKUP_EVERY)
            {
                return Ok(None);
            }
            let named = gh.repo(Target::Name(repo), None).await?;
            // An answer that settled nothing is asked for again next round.
            if !matches!(named, Observed::Denied { .. } | Observed::Failed) {
                memory.lookups.insert(repo.to_string(), now);
            }
            Some(named)
        }
        _ => None,
    };
    if let Some(next) = again {
        seen = next;
    }
    Ok(Some(seen))
}

/// A link looked at for the first time takes the repo's archived flag as its
/// baseline, without a log row: an archived repo met on the first look is
/// the state of things, not an event.
fn first_sight(
    daemon: &Daemon,
    row: &GhLinkRow,
    info: Option<&RepoInfo>,
    now: DateTime<Utc>,
) -> Result<()> {
    if let (None, Some(info)) = (row.archived, info) {
        let retired = info.archived || info.disabled;
        daemon.store().gh_set_archived(&row.repo, retired, now)?;
    }
    Ok(())
}

/// The repo was archived (or disabled) or is live again since the last look:
/// one log row, then the new baseline. The row is written first, so a crash
/// between the two writes a duplicate that `log_once` leaves out, never
/// loses the row.
fn retire(daemon: &Daemon, row: &GhLinkRow, retired: bool, now: DateTime<Utc>) -> Result<()> {
    let (kind, text) = if retired {
        (
            "repo-archived",
            format!("{} is archived on GitHub", row.repo),
        )
    } else {
        (
            "repo-unarchived",
            format!("{} is no longer archived on GitHub", row.repo),
        )
    };
    let store = daemon.store();
    let new = log_once(
        &store,
        &NewLog {
            at: now,
            repo: &row.repo,
            list: Some(&row.list),
            kind,
            text: &text,
            data: None,
            by: AUTO,
        },
    )?;
    store.gh_set_archived(&row.repo, retired, now)?;
    drop(store);
    if new {
        log::info!("{text}");
    }
    Ok(())
}

/// The repo is called `to` now. Its link and pairs move at once, and the
/// rest of the pass waits for the next round, which is woken. A witnessed
/// rename (the link had the repo's id) leaves a marker that the list has a
/// rename to follow; an unwitnessed one (found after the fact, through the
/// old name's redirect) learns the id as it moves and sets none. Nothing
/// moves onto a link that is already there.
fn rename(
    daemon: &Daemon,
    memory: &mut Memory,
    row: &GhLinkRow,
    to: &str,
    id: u64,
    witnessed: bool,
    now: DateTime<Utc>,
) -> Result<Gate> {
    let repo = row.repo.as_str();
    let store = daemon.store();
    let rows = store.gh_link_rows()?;
    let taken = rows
        .iter()
        .find(|r| r.repo != repo && r.repo.eq_ignore_ascii_case(to));
    let same_id = rows
        .iter()
        .find(|r| r.repo != repo && r.repo_id == Some(id));
    if let Some(other) = taken.or(same_id) {
        let name = store
            .find_list(&other.list)
            .map_or_else(|_| other.list.clone(), |l| l.name);
        drop(store);
        let note = match taken {
            Some(_) => format!("{repo} is now {to}, which is already linked to {name:?}"),
            None => format!(
                "{repo} is now {to}, but the same repository (GitHub id {id}) is linked as {}",
                other.repo
            ),
        };
        let data = serde_json::json!({ "to": to, "id": id, "other": other.repo });
        return conflict(daemon, row, note, &data, now);
    }
    let text = format!("{repo} is now {to} (GitHub id {id})");
    let data = serde_json::json!({
        "from": repo,
        "to": to,
        "id": id,
        "witnessed": witnessed,
    })
    .to_string();
    let log = NewLog {
        at: now,
        repo: to,
        list: Some(&row.list),
        kind: "repo-renamed",
        text: &text,
        data: Some(&data),
        by: AUTO,
    };
    if store.gh_rename(repo, to, witnessed.then_some(now), Some(&log))? && !witnessed {
        store.gh_learn_id(to, id)?;
    }
    drop(store);
    memory.issues.remove(repo);
    memory.waits.remove(repo);
    memory.lookups.remove(repo);
    log::info!("{repo} is now {to}");
    daemon.wake_github.notify_one();
    Ok(Gate::Stop)
}

/// The name now belongs to another repo than the linked one (made again
/// after a delete, or the name reused): nothing it holds is the project's.
fn replaced(daemon: &Daemon, row: &GhLinkRow, found: u64, now: DateTime<Utc>) -> Result<Gate> {
    let repo = row.repo.as_str();
    let note = match row.repo_id {
        Some(stored) => format!(
            "{repo} is another repository now (GitHub id {found}, not {stored}); its issues \
             are not read, and unlinking {repo} and linking it again follows the new one"
        ),
        None => format!("{repo} is another repository now (GitHub id {found})"),
    };
    let data = serde_json::json!({ "found": found, "stored": row.repo_id }).to_string();
    let new = log_once(
        &daemon.store(),
        &NewLog {
            at: now,
            repo,
            list: Some(&row.list),
            kind: "repo-replaced",
            text: &note,
            data: Some(&data),
            by: AUTO,
        },
    )?;
    if new {
        log::warn!("{note}");
    }
    daemon.gh_hold(repo, LinkState::Replaced, note, now);
    Ok(Gate::Stop)
}

/// The repo's id is already another link's: two links to one repo. Neither
/// is changed.
fn duplicate_id(daemon: &Daemon, row: &GhLinkRow, id: u64, now: DateTime<Utc>) -> Result<Gate> {
    let repo = row.repo.as_str();
    let holder = daemon
        .store()
        .gh_link_rows()?
        .into_iter()
        .find(|r| r.repo != repo && r.repo_id == Some(id));
    let Some(other) = holder else {
        // Unlinked meanwhile, or given an id elsewhere: nothing to say.
        return Ok(Gate::Stop);
    };
    let note = format!(
        "{repo} is the same GitHub repository (id {id}) as {}, which is linked too",
        other.repo
    );
    let data = serde_json::json!({ "id": id, "other": other.repo });
    conflict(daemon, row, note, &data, now)
}

/// Two links, one repo: say so once, hold this link, change nothing.
fn conflict(
    daemon: &Daemon,
    row: &GhLinkRow,
    note: String,
    data: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<Gate> {
    let new = log_once(
        &daemon.store(),
        &NewLog {
            at: now,
            repo: &row.repo,
            list: Some(&row.list),
            kind: "rename-conflict",
            text: &note,
            data: Some(&data.to_string()),
            by: AUTO,
        },
    )?;
    if new {
        log::warn!("{note}");
    }
    daemon.gh_hold(&row.repo, LinkState::Conflict, note, now);
    Ok(Gate::Stop)
}

/// A look that settles nothing is no finding and changes nothing stored: the
/// pass is skipped, and the link says why. Not finding the repo is shown as
/// such, but is never taken for a deletion.
fn unsettled(
    daemon: &Daemon,
    memory: &mut Memory,
    row: &GhLinkRow,
    seen: &Observed,
    now: DateTime<Utc>,
) -> Gate {
    let repo = row.repo.as_str();
    match seen {
        Observed::Missing => daemon.gh_hold(
            repo,
            LinkState::Unreachable,
            "GitHub cannot find it: deleted, or the token cannot see it".into(),
            now,
        ),
        Observed::Denied { retry } => {
            let wait = retry
                .and_then(|d| chrono::Duration::from_std(d).ok())
                .unwrap_or(REFUSED_WAIT);
            let until = now + wait;
            memory.waits.insert(repo.to_string(), until);
            let at = until.with_timezone(&daemon.zone).format("%H:%M");
            daemon.gh_pass_done(
                repo,
                Err(format!(
                    "GitHub refused to describe {repo} (a rate limit, or the token cannot see it); trying again at {at}"
                )),
            );
        }
        _ => daemon.gh_pass_done(
            repo,
            Err(format!("GitHub's answer about {repo} could not be used")),
        ),
    }
    Gate::Stop
}

/// Write a log row unless the repo's newest row already says the same: a
/// finding that holds across rounds is logged once, and one that comes back
/// after something else happened is logged again.
fn log_once(store: &Store, row: &NewLog) -> Result<bool> {
    let newest = store.gh_log_recent(Some(row.repo), 1)?;
    if newest
        .first()
        .is_some_and(|n| n.kind == row.kind && n.data.as_deref() == row.data)
    {
        return Ok(false);
    }
    store.gh_log_add(row)?;
    Ok(true)
}

/// One repo against its list. An op that fails is logged and left for the
/// next pass, its pair as it was; a lost connection stops the pass.
async fn pass(daemon: &Daemon, gh: &GitHub, memory: &mut Memory, row: &GhLinkRow) -> Result<()> {
    let (repo, list) = (row.repo.as_str(), row.list.as_str());
    let etag = memory.issues.get(repo).map(|(e, _)| e.as_str());
    let fetched = match gh.issues(repo, etag).await {
        // Moved since the repo was looked at (a 304 can be minutes old):
        // look again next round, with no ETag to be answered by. Not
        // sooner, since nothing has changed that would make it end.
        Err(RemoteError::Status(301)) => {
            if let Some(id) = row.repo_id {
                memory.records.remove(&id);
            }
            return Err(Error::Invalid(
                "GitHub moved the repository while its issues were read; looking again".into(),
            ));
        }
        other => other?,
    };
    let issues = match fetched {
        Some((issues, Some(etag))) => {
            memory
                .issues
                .insert(repo.to_string(), (etag, issues.clone()));
            issues
        }
        Some((issues, None)) => {
            memory.issues.remove(repo);
            issues
        }
        None => memory.issues[repo].1.clone(),
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
    memory.issues.remove(repo);
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

#[cfg(test)]
mod tests {
    use super::*;
    use asst_core::caldav::RemoteList;
    use asst_core::config::Config;
    use asst_core::store::LogRow;
    use asst_core::testing::{FakeGitHub, Reply};
    use chrono::TimeZone;
    use chrono_tz::Tz;

    const LAB: u64 = 1379104005;

    /// The name part of `owner/repo`: the list a test links it to.
    fn short(repo: &str) -> &str {
        repo.split_once('/').map_or(repo, |(_, name)| name)
    }

    /// A repository record as GitHub gives it, cut to what asst reads.
    fn record(id: u64, name: &str, archived: bool) -> String {
        format!(r#"{{"id":{id},"full_name":"{name}","archived":{archived},"disabled":false}}"#)
    }

    /// A record's ETag changes with what the record says.
    fn etag(id: u64, name: &str, archived: bool) -> String {
        format!("\"{id}:{name}:{archived}\"")
    }

    /// Everything a round could have written for the links, short of the
    /// clock: compared before and after to show it wrote nothing.
    #[derive(Debug, PartialEq)]
    struct Snap {
        links: Vec<GhLinkRow>,
        pairs: Vec<(String, Vec<Pair>)>,
        tasks: Vec<(String, String, bool)>,
        log: Vec<LogRow>,
    }

    /// A daemon with an in-memory store, a fake GitHub, and a real client
    /// pointed at it, the way `run` drives them one round at a time.
    struct Rig {
        daemon: Arc<Daemon>,
        fake: FakeGitHub,
        gh: GitHub,
        memory: Memory,
        now: DateTime<Utc>,
    }

    impl Rig {
        /// One list per repo, named for the repo and linked to it, with no
        /// id known. GitHub knows nothing yet; every issue listing is empty.
        async fn new(repos: &[&str]) -> Rig {
            let zone: Tz = "America/New_York".parse().unwrap();
            let mut store = Store::in_memory(zone).unwrap();
            let lists: Vec<RemoteList> = repos
                .iter()
                .map(|repo| RemoteList {
                    href: format!("/cal/{}/", short(repo)),
                    name: short(repo).into(),
                    color: None,
                    order: None,
                    sync_token: Some("t1".into()),
                    ctag: None,
                    writable: true,
                })
                .collect();
            store.apply_lists(&lists).unwrap();
            let rig = Rig::over(store, zone).await;
            for repo in repos {
                rig.daemon.link(short(repo), repo).unwrap();
                rig.fake.reply(
                    "GET",
                    &format!("/repos/{repo}/issues"),
                    Reply::json(200, "[]"),
                );
            }
            rig
        }

        /// A daemon over this store, and a fake GitHub that knows nothing.
        async fn over(store: Store, zone: Tz) -> Rig {
            let (daemon, _events) = Daemon::new(Config::default(), store, zone);
            let fake = FakeGitHub::start().await;
            let gh = GitHub::with_base("tok", &fake.base())
                .unwrap()
                .with_timeout(Duration::from_millis(300));
            Rig {
                daemon,
                fake,
                gh,
                memory: Memory::default(),
                now: Utc.with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap(),
            }
        }

        /// GitHub has the repo `name` as `id`, found by either.
        fn publish(&self, id: u64, name: &str, archived: bool) {
            let (tag, body) = (etag(id, name, archived), record(id, name, archived));
            self.fake.etagged(&format!("/repos/{name}"), &tag, &body);
            self.fake
                .etagged(&format!("/repositories/{id}"), &tag, &body);
        }

        /// GitHub renames the repo: the old name redirects to the id, and
        /// the new name has the same issues.
        fn rename_on_github(&self, id: u64, old: &str, new: &str, issues: &str) {
            let to = format!("{}/repositories/{id}", self.fake.base());
            self.fake.reply(
                "GET",
                &format!("/repos/{old}"),
                Reply::status(301).header("location", &to),
            );
            self.fake.reply(
                "GET",
                &format!("/repos/{old}/issues"),
                Reply::status(301).header("location", &to),
            );
            self.publish(id, new, false);
            self.fake.reply(
                "GET",
                &format!("/repos/{new}/issues"),
                Reply::json(200, issues),
            );
        }

        /// A task, an issue and a pair that agree on them: a pass has
        /// nothing to do for it. The issue listing that has it.
        fn pair_up(&self, repo: &str, number: u32, title: &str) -> String {
            let list = self.row(repo).list;
            let task = self
                .daemon
                .store()
                .create(&list, &[Edit::Summary(title.into())], self.now)
                .unwrap();
            let pair = Pair {
                number,
                uid: task.task.uid,
                title: title.into(),
                open: true,
                body: None,
                priority: 0,
            };
            self.daemon.store().gh_pair(repo, &pair).unwrap();
            let listing = format!(
                r#"[{{"number":{number},"title":"{title}","state":"open","body":null,"labels":[]}}]"#
            );
            self.fake.reply(
                "GET",
                &format!("/repos/{repo}/issues"),
                Reply::json(200, &listing),
            );
            listing
        }

        async fn go(&mut self) -> Round {
            round(&self.daemon, &self.gh, &mut self.memory, self.now).await
        }

        fn later(&mut self, minutes: i64) {
            self.now += chrono::Duration::minutes(minutes);
        }

        /// A new process: nothing remembered, the same store.
        fn restart(&mut self) {
            self.memory = Memory::default();
        }

        fn rows(&self) -> Vec<GhLinkRow> {
            self.daemon.store().gh_link_rows().unwrap()
        }

        fn row(&self, repo: &str) -> GhLinkRow {
            self.rows()
                .into_iter()
                .find(|r| r.repo == repo)
                .unwrap_or_else(|| panic!("{repo} is not linked"))
        }

        fn link(&self, repo: &str) -> asst_core::api::LinkView {
            self.daemon
                .links()
                .unwrap()
                .into_iter()
                .find(|l| l.repo == repo)
                .unwrap_or_else(|| panic!("{repo} is not linked"))
        }

        /// Newest first, as the store gives it.
        fn log(&self) -> Vec<LogRow> {
            self.daemon.store().gh_log_recent(None, 1000).unwrap()
        }

        fn snap(&self) -> Snap {
            let store = self.daemon.store();
            let links = store.gh_link_rows().unwrap();
            Snap {
                pairs: links
                    .iter()
                    .map(|l| (l.repo.clone(), store.gh_pairs(&l.repo).unwrap()))
                    .collect(),
                tasks: links
                    .iter()
                    .flat_map(|l| store.list_tasks(&l.list).unwrap())
                    .map(|r| (r.href, r.task.summary.clone(), r.task.is_open()))
                    .collect(),
                links,
                log: store.gh_log_recent(None, 1000).unwrap(),
            }
        }

        /// How many times the issues of `repo` were asked for.
        fn issues_asked(&self, repo: &str) -> usize {
            self.fake.count(&format!("/repos/{repo}/issues"))
        }

        /// Forget a pass that was asked for before the test began.
        async fn drain_wake(&self) {
            self.woken().await;
        }

        /// Whether a GitHub pass was asked for since this last said no.
        async fn woken(&self) -> bool {
            tokio::time::timeout(
                Duration::from_millis(20),
                self.daemon.wake_github.notified(),
            )
            .await
            .is_ok()
        }
    }

    #[tokio::test]
    async fn the_first_round_learns_every_id_and_writes_no_log() {
        let repos: Vec<String> = (0..14).map(|i| format!("jaehho/p{i:02}")).collect();
        let names: Vec<&str> = repos.iter().map(String::as_str).collect();
        let mut rig = Rig::new(&names).await;
        for (i, repo) in names.iter().enumerate() {
            rig.publish(1000 + i as u64, repo, false);
        }
        let lists = rig.daemon.lists().unwrap();
        assert!(rig.rows().iter().all(|r| r.repo_id.is_none()));

        assert_eq!(rig.go().await, Round::Done);

        let rows = rig.rows();
        assert_eq!(rows.len(), 14);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.repo, names[i]);
            assert_eq!(row.repo_id, Some(1000 + i as u64));
            assert_eq!(row.archived, Some(0), "looked at, live");
            assert_eq!((&row.follow_from, row.follow_at), (&None, None));
        }
        assert!(rig.log().is_empty(), "learning an id is not an event");
        assert_eq!(rig.daemon.lists().unwrap(), lists, "no list changed");
        for repo in &names {
            assert_eq!(rig.fake.count(&format!("/repos/{repo}")), 1);
            assert_eq!(rig.issues_asked(repo), 1, "and then its issues");
        }
        assert!(
            rig.fake
                .seen()
                .iter()
                .all(|r| !r.path().starts_with("/repositories/")),
            "no id was known to ask by"
        );
        for link in rig.daemon.links().unwrap() {
            assert_eq!((link.state, &link.error), (None, &None));
            assert!(link.synced.is_some());
            assert!(link.repo_id.is_some());
        }
    }

    #[tokio::test]
    async fn the_second_round_asks_by_id_with_the_etag_and_writes_nothing() {
        let mut rig = Rig::new(&["jaehho/a", "jaehho/b", "jaehho/c"]).await;
        for (i, repo) in ["jaehho/a", "jaehho/b", "jaehho/c"].iter().enumerate() {
            rig.publish(10 + i as u64, repo, false);
        }
        rig.go().await;
        let before = rig.snap();
        let mark = rig.fake.seen().len();

        assert_eq!(rig.go().await, Round::Done);

        let asked: Vec<_> = rig
            .fake
            .seen()
            .into_iter()
            .skip(mark)
            .filter(|r| r.path().starts_with("/repositories/"))
            .collect();
        assert_eq!(asked.len(), 3, "one identity request a link");
        for (i, name) in ["jaehho/a", "jaehho/b", "jaehho/c"].iter().enumerate() {
            let id = 10 + i as u64;
            let req = asked
                .iter()
                .find(|r| r.path() == format!("/repositories/{id}"))
                .expect("asked by id");
            let tag = etag(id, name, false);
            assert_eq!(req.header("if-none-match"), Some(tag.as_str()));
        }
        assert_eq!(rig.snap(), before, "a 304 changes nothing");
        assert!(rig.log().is_empty());
        assert_eq!(rig.issues_asked("jaehho/a"), 2, "the pass still ran");
    }

    #[tokio::test]
    async fn a_witnessed_rename_moves_the_link_and_its_pairs_and_leaves_a_marker() {
        let mut rig = Rig::new(&["jaehho/champalimaud"]).await;
        rig.publish(LAB, "jaehho/champalimaud", false);
        rig.go().await;
        let issues = rig.pair_up("jaehho/champalimaud", 7, "One");
        rig.go().await;
        let asked = rig.issues_asked("jaehho/champalimaud");
        let pairs = rig.daemon.store().gh_pairs("jaehho/champalimaud").unwrap();
        assert_eq!(pairs.len(), 1);
        rig.rename_on_github(LAB, "jaehho/champalimaud", "jaehho/lab", &issues);
        rig.woken().await;

        assert_eq!(rig.go().await, Round::Done);

        let row = rig.row("jaehho/lab");
        assert_eq!(rig.rows().len(), 1, "moved, not copied");
        assert_eq!(row.repo_id, Some(LAB), "the id is the repo's");
        assert_eq!(row.list, "/cal/champalimaud/");
        assert_eq!(row.follow_from.as_deref(), Some("jaehho/champalimaud"));
        assert_eq!(row.follow_at, Some(rig.now.timestamp()));
        assert_eq!(rig.daemon.store().gh_pairs("jaehho/lab").unwrap(), pairs);
        assert!(
            rig.daemon
                .store()
                .gh_pairs("jaehho/champalimaud")
                .unwrap()
                .is_empty()
        );
        let log = rig.log();
        assert_eq!(log.len(), 1);
        assert_eq!(
            (
                log[0].kind.as_str(),
                log[0].repo.as_str(),
                log[0].by.as_str()
            ),
            ("repo-renamed", "jaehho/lab", "auto")
        );
        assert_eq!(log[0].list.as_deref(), Some("/cal/champalimaud/"));
        assert_eq!(
            log[0].text,
            "jaehho/champalimaud is now jaehho/lab (GitHub id 1379104005)"
        );
        let data: serde_json::Value =
            serde_json::from_str(log[0].data.as_deref().unwrap()).unwrap();
        assert_eq!(data["witnessed"], true);
        assert_eq!(data["from"], "jaehho/champalimaud");
        // The pass ended with the move; the next round is woken for it
        assert_eq!(rig.issues_asked("jaehho/champalimaud"), asked);
        assert_eq!(rig.issues_asked("jaehho/lab"), 0);
        assert!(rig.woken().await);
        // The list keeps its name for now, and the link says so
        assert_eq!(rig.daemon.lists().unwrap()[0].name, "champalimaud");
        let link = rig.link("jaehho/lab");
        assert_eq!(link.state, Some(asst_core::api::LinkState::Renamed));
        assert!(link.note.unwrap().contains("jaehho/champalimaud"));

        // Next round: the id, a 304, and the pass on the new name
        assert_eq!(rig.go().await, Round::Done);
        assert_eq!(rig.issues_asked("jaehho/lab"), 1);
        assert_eq!(rig.log().len(), 1, "nothing more to say");
        assert_eq!(rig.daemon.store().gh_pairs("jaehho/lab").unwrap(), pairs);
    }

    #[tokio::test]
    async fn a_rename_found_through_the_old_name_moves_the_link_and_learns_the_id() {
        let mut rig = Rig::new(&["jaehho/champalimaud"]).await;
        let issues = rig.pair_up("jaehho/champalimaud", 7, "One");
        let pairs = rig.daemon.store().gh_pairs("jaehho/champalimaud").unwrap();
        rig.rename_on_github(LAB, "jaehho/champalimaud", "jaehho/lab", &issues);

        assert_eq!(rig.go().await, Round::Done);

        let row = rig.row("jaehho/lab");
        assert_eq!(rig.rows().len(), 1);
        assert_eq!(row.repo_id, Some(LAB), "learned as it moved");
        assert_eq!(
            (&row.follow_from, row.follow_at),
            (&None, None),
            "unwitnessed: no marker"
        );
        assert_eq!(rig.daemon.store().gh_pairs("jaehho/lab").unwrap(), pairs);
        let log = rig.log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, "repo-renamed");
        let data: serde_json::Value =
            serde_json::from_str(log[0].data.as_deref().unwrap()).unwrap();
        assert_eq!(data["witnessed"], false);
        assert_eq!(rig.issues_asked("jaehho/lab"), 0, "the pass ended there");
        assert_eq!(rig.link("jaehho/lab").state, None);

        assert_eq!(rig.go().await, Round::Done);
        assert_eq!(rig.issues_asked("jaehho/lab"), 1);
        assert_eq!(rig.log().len(), 1);
        assert_eq!(rig.row("jaehho/lab").archived, Some(0));
    }

    #[tokio::test]
    async fn a_repo_made_again_under_the_name_is_not_read() {
        let mut rig = Rig::new(&["jaehho/x"]).await;
        rig.publish(111, "jaehho/x", false);
        rig.go().await;
        rig.pair_up("jaehho/x", 3, "Three");
        let before = rig.snap();
        let asked = rig.issues_asked("jaehho/x");
        // Deleted, then made again: the id is gone, the name has a stranger
        rig.fake.reply(
            "GET",
            "/repositories/111",
            Reply::json(404, r#"{"message":"Not Found"}"#),
        );
        rig.publish(222, "jaehho/x", false);
        rig.fake.reply(
            "GET",
            "/repositories/111",
            Reply::json(404, r#"{"message":"Not Found"}"#),
        );
        rig.fake.reply(
            "GET",
            "/repos/jaehho/x/issues",
            Reply::json(
                200,
                r#"[{"number":1,"title":"Theirs","state":"open","body":null,"labels":[]}]"#,
            ),
        );

        assert_eq!(rig.go().await, Round::Done);

        assert_eq!(
            rig.issues_asked("jaehho/x"),
            asked,
            "their issues are not read"
        );
        let after = rig.snap();
        assert_eq!(after.links, before.links, "links and ids as they were");
        assert_eq!(after.pairs, before.pairs);
        assert_eq!(after.tasks, before.tasks, "no task completed or made");
        assert_eq!(after.log.len(), 1, "one row, for the finding");
        assert_eq!(after.log[0].kind, "repo-replaced");
        assert_eq!(after.log[0].by, "auto");
        let link = rig.link("jaehho/x");
        assert_eq!(link.state, Some(asst_core::api::LinkState::Replaced));
        assert!(link.note.as_deref().unwrap().contains("222"));
        assert_eq!(link.since, Some(rig.now));
        assert_eq!(link.repo_id, Some(111));

        // Within the hour the name is not asked for again; nothing new
        rig.later(10);
        assert_eq!(rig.go().await, Round::Done);
        assert_eq!(rig.fake.count("/repos/jaehho/x"), 2, "the name, once");
        assert_eq!(rig.snap(), after);
        // After it, it is, and the same finding is not logged again
        rig.later(60);
        assert_eq!(rig.go().await, Round::Done);
        assert_eq!(rig.fake.count("/repos/jaehho/x"), 3);
        assert_eq!(rig.log().len(), 1);
        assert_eq!(rig.issues_asked("jaehho/x"), asked);
        let link = rig.link("jaehho/x");
        assert_eq!(link.state, Some(asst_core::api::LinkState::Replaced));
        assert_eq!(link.since.map(|s| s.timestamp()), Some(after.log[0].at));
    }

    #[tokio::test]
    async fn an_id_that_comes_back_confirms_the_link_again() {
        let mut rig = Rig::new(&["jaehho/x"]).await;
        rig.publish(111, "jaehho/x", false);
        rig.go().await;
        rig.fake
            .reply("GET", "/repositories/111", Reply::status(404));
        rig.publish(222, "jaehho/x", false);
        rig.fake
            .reply("GET", "/repositories/111", Reply::status(404));
        rig.go().await;
        assert!(rig.link("jaehho/x").state.is_some());
        // GitHub answers for the id again: the hold is lifted with the pass
        rig.publish(111, "jaehho/x", false);
        rig.later(1);
        assert_eq!(rig.go().await, Round::Done);
        let link = rig.link("jaehho/x");
        assert_eq!((link.state, link.error), (None, None));
        assert!(link.synced.is_some());
    }

    #[tokio::test]
    async fn a_repo_github_cannot_find_is_reported_not_deleted_or_logged() {
        let mut rig = Rig::new(&["jaehho/x"]).await;
        rig.publish(111, "jaehho/x", false);
        rig.go().await;
        rig.pair_up("jaehho/x", 3, "Three");
        let before = rig.snap();
        let asked = rig.issues_asked("jaehho/x");
        rig.fake
            .reply("GET", "/repositories/111", Reply::status(404));
        rig.fake.reply("GET", "/repos/jaehho/x", Reply::status(404));

        assert_eq!(rig.go().await, Round::Done);

        assert_eq!(rig.snap(), before, "404 is not evidence of anything");
        assert_eq!(rig.issues_asked("jaehho/x"), asked);
        let link = rig.link("jaehho/x");
        assert_eq!(link.state, Some(asst_core::api::LinkState::Unreachable));
        assert_eq!(
            link.note.as_deref(),
            Some("GitHub cannot find it: deleted, or the token cannot see it")
        );
        // The id answers again (the token was the trouble): back to normal
        rig.publish(111, "jaehho/x", false);
        rig.later(1);
        rig.go().await;
        assert_eq!(rig.link("jaehho/x").state, None);
    }

    #[tokio::test]
    async fn an_answer_that_settles_nothing_changes_nothing() {
        let cases: Vec<(&str, Reply, bool)> = vec![
            (
                "403 with retry-after",
                Reply::json(403, r#"{"message":"rate limit"}"#).header("retry-after", "60"),
                true,
            ),
            ("403", Reply::json(403, r#"{"message":"no"}"#), true),
            ("429", Reply::json(429, r#"{"message":"slow"}"#), true),
            ("500", Reply::status(500), true),
            (
                "not a record",
                Reply::json(200, r#"{"hello":"world"}"#),
                true,
            ),
            ("a timeout", Reply::hang(), false),
        ];
        for (label, reply, answered) in cases {
            let mut rig = Rig::new(&["jaehho/x"]).await;
            rig.publish(5, "jaehho/x", false);
            rig.go().await;
            rig.pair_up("jaehho/x", 3, "Three");
            let before = rig.snap();
            let asked = rig.issues_asked("jaehho/x");
            rig.fake.reply("GET", "/repositories/5", reply);

            let ended = rig.go().await;

            let expected = if answered {
                Round::Done
            } else {
                Round::Offline
            };
            assert_eq!(ended, expected, "{label}");
            assert_eq!(rig.snap(), before, "{label}: links, pairs, tasks and log");
            assert_eq!(rig.issues_asked("jaehho/x"), asked, "{label}");
            let link = rig.link("jaehho/x");
            assert_eq!(link.state, None, "{label}: no finding");
            assert_eq!(link.error.is_some(), answered, "{label}");
            assert_eq!(link.repo_id, Some(5), "{label}");
        }

        // A refused connection is the same: offline, nothing written
        let mut rig = Rig::new(&["jaehho/x"]).await;
        rig.publish(5, "jaehho/x", false);
        rig.go().await;
        let before = rig.snap();
        rig.gh = GitHub::with_base("tok", &FakeGitHub::refused_base().await).unwrap();
        assert_eq!(rig.go().await, Round::Offline);
        assert_eq!(rig.snap(), before);
        assert_eq!(rig.link("jaehho/x").state, None);
    }

    #[tokio::test]
    async fn a_refusal_that_says_when_to_come_back_is_waited_out() {
        let mut rig = Rig::new(&["jaehho/a", "jaehho/b"]).await;
        rig.publish(1, "jaehho/a", false);
        rig.publish(2, "jaehho/b", false);
        rig.go().await;
        rig.fake.reply(
            "GET",
            "/repositories/1",
            Reply::json(403, "{}").header("retry-after", "120"),
        );
        let (a, b) = (rig.issues_asked("jaehho/a"), rig.issues_asked("jaehho/b"));

        rig.go().await;
        assert_eq!(rig.fake.count("/repositories/1"), 1);
        assert_eq!(rig.issues_asked("jaehho/a"), a);
        assert_eq!(
            rig.issues_asked("jaehho/b"),
            b + 1,
            "the other link goes on"
        );

        // Not before it said; then again
        rig.now += chrono::Duration::seconds(60);
        rig.go().await;
        assert_eq!(rig.fake.count("/repositories/1"), 1, "still waiting");
        rig.now += chrono::Duration::seconds(61);
        rig.go().await;
        assert_eq!(rig.fake.count("/repositories/1"), 2);

        // A refusal that names no time waits a quarter of an hour
        rig.fake.reply("GET", "/repositories/1", Reply::status(429));
        rig.later(3);
        rig.go().await;
        let asked = rig.fake.count("/repositories/1");
        rig.later(14);
        rig.go().await;
        assert_eq!(rig.fake.count("/repositories/1"), asked);
        rig.later(2);
        rig.go().await;
        assert_eq!(rig.fake.count("/repositories/1"), asked + 1);
        assert!(
            rig.link("jaehho/a")
                .error
                .unwrap()
                .contains("trying again at")
        );
    }

    #[tokio::test]
    async fn a_lost_connection_ends_the_round_and_a_trouble_with_one_link_does_not() {
        let mut rig = Rig::new(&["jaehho/a", "jaehho/b"]).await;
        rig.publish(1, "jaehho/a", false);
        rig.publish(2, "jaehho/b", false);
        rig.go().await;
        let b = rig.issues_asked("jaehho/b");

        rig.fake.reply("GET", "/repositories/1", Reply::status(500));
        rig.go().await;
        assert_eq!(rig.issues_asked("jaehho/b"), b + 1);

        rig.fake.reply("GET", "/repositories/1", Reply::hang());
        assert_eq!(rig.go().await, Round::Offline);
        assert_eq!(rig.issues_asked("jaehho/b"), b + 1, "the round ended at a");
    }

    #[tokio::test]
    async fn an_archive_is_logged_once_and_a_restart_does_not_log_it_again() {
        let mut rig = Rig::new(&["jaehho/mail-digest"]).await;
        let repo = "jaehho/mail-digest";
        rig.publish(9, repo, false);
        rig.go().await;
        assert_eq!(rig.row(repo).archived, Some(0));

        rig.publish(9, repo, true);
        rig.later(5);
        assert_eq!(rig.go().await, Round::Done);
        let log = rig.log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, "repo-archived");
        assert_eq!(log[0].repo, repo);
        assert_eq!(log[0].by, "auto");
        assert_eq!(rig.row(repo).archived, Some(rig.now.timestamp()));
        assert_eq!(rig.issues_asked(repo), 2, "an archived repo still syncs");
        let link = rig.link(repo);
        assert_eq!(link.state, Some(asst_core::api::LinkState::Archived));
        assert_eq!(link.since, Some(rig.now));
        assert_eq!(
            link.note.as_deref(),
            Some("first seen archived 2026-10-18; 0 open tasks")
        );

        // Seen again, with the tag and without it (a restart): nothing new
        rig.later(5);
        rig.go().await;
        rig.restart();
        rig.later(5);
        rig.go().await;
        assert_eq!(rig.log().len(), 1);
        assert_eq!(rig.row(repo).archived, Some(rig.now.timestamp() - 600));

        // Live again: logged once, and the baseline is live
        rig.publish(9, repo, false);
        rig.go().await;
        rig.go().await;
        let log = rig.log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].kind, "repo-unarchived");
        assert_eq!(rig.row(repo).archived, Some(0));
        assert_eq!(rig.link(repo).state, None);

        // Archived again after that is an event again
        rig.publish(9, repo, true);
        rig.go().await;
        assert_eq!(rig.log().len(), 3);
        assert_eq!(rig.log()[0].kind, "repo-archived");
    }

    #[tokio::test]
    async fn a_repo_met_archived_is_the_state_of_things_not_an_event() {
        let repo = "jaehho/mail-digest";
        // The upgrade day: the id is learned, and the flag is the baseline
        let mut rig = Rig::new(&[repo]).await;
        rig.publish(9, repo, true);
        assert_eq!(rig.go().await, Round::Done);
        assert!(rig.log().is_empty());
        assert_eq!(rig.row(repo).repo_id, Some(9));
        assert_eq!(rig.row(repo).archived, Some(rig.now.timestamp()));
        assert_eq!(rig.issues_asked(repo), 1);
        assert_eq!(
            rig.link(repo).state,
            Some(asst_core::api::LinkState::Archived)
        );
        rig.restart();
        rig.go().await;
        assert!(rig.log().is_empty());

        // A restored database: an id, and no baseline
        let mut rig = Rig::new(&[repo]).await;
        rig.daemon.store().gh_learn_id(repo, 9).unwrap();
        rig.publish(9, repo, true);
        assert_eq!(rig.go().await, Round::Done);
        assert!(rig.log().is_empty());
        assert_eq!(rig.row(repo).archived, Some(rig.now.timestamp()));
    }

    #[tokio::test]
    async fn two_links_to_one_repo_are_a_conflict_and_change_nothing() {
        // The same repo linked under two spellings of its name: the second
        // finds the first's id
        let mut rig = Rig::new(&["jaehho/lab"]).await;
        rig.publish(5, "jaehho/lab", false);
        rig.go().await;
        let other = {
            let mut store = rig.daemon.store();
            let list = store.lists().unwrap()[0].clone();
            store
                .apply_lists(&[
                    RemoteList {
                        href: list.href.clone(),
                        name: list.name.clone(),
                        color: None,
                        order: None,
                        sync_token: Some("t1".into()),
                        ctag: None,
                        writable: true,
                    },
                    RemoteList {
                        href: "/cal/twice/".into(),
                        name: "twice".into(),
                        color: None,
                        order: None,
                        sync_token: Some("t1".into()),
                        ctag: None,
                        writable: true,
                    },
                ])
                .unwrap();
            store.gh_link("jaehho/LAB", "/cal/twice/").unwrap();
            "jaehho/LAB"
        };
        rig.fake.etagged(
            "/repos/jaehho/LAB",
            "\"x\"",
            &record(5, "jaehho/LAB", false),
        );
        rig.fake
            .reply("GET", "/repos/jaehho/LAB/issues", Reply::json(200, "[]"));
        let before = rig.snap();

        assert_eq!(rig.go().await, Round::Done);

        let after = rig.snap();
        assert_eq!(after.links, before.links, "no id learned, nothing moved");
        assert_eq!(after.pairs, before.pairs);
        assert_eq!(after.log.len(), 1);
        assert_eq!(after.log[0].kind, "rename-conflict");
        assert_eq!(after.log[0].repo, other);
        assert_eq!(rig.issues_asked(other), 0);
        let link = rig.link(other);
        assert_eq!(link.state, Some(asst_core::api::LinkState::Conflict));
        assert_eq!(link.repo_id, None);
        // Told once, however many rounds
        rig.go().await;
        rig.go().await;
        assert_eq!(rig.log().len(), 1);
        assert_eq!(rig.link("jaehho/lab").state, None);
    }

    #[tokio::test]
    async fn a_rename_onto_a_linked_repo_is_a_conflict_and_changes_nothing() {
        // The footgun: the heal found `lab`, and `asst link` had already
        // made a link of that name
        let mut rig = Rig::new(&["jaehho/champalimaud", "jaehho/lab"]).await;
        rig.publish(LAB, "jaehho/lab", false);
        rig.go().await;
        let lab_issues = rig.pair_up("jaehho/lab", 7, "One");
        let issues = rig.pair_up("jaehho/champalimaud", 8, "Two");
        let before = rig.snap();
        rig.rename_on_github(LAB, "jaehho/champalimaud", "jaehho/lab", &issues);
        // lab's own issues, not the ones of the repo that moved onto it
        rig.fake.reply(
            "GET",
            "/repos/jaehho/lab/issues",
            Reply::json(200, &lab_issues),
        );
        let lab = rig.issues_asked("jaehho/lab");

        assert_eq!(rig.go().await, Round::Done);

        let after = rig.snap();
        assert_eq!(after.links, before.links);
        assert_eq!(after.pairs, before.pairs, "no pair dropped or moved");
        assert_eq!(after.tasks, before.tasks);
        assert_eq!(after.log.len(), 1);
        assert_eq!(after.log[0].kind, "rename-conflict");
        assert_eq!(after.log[0].repo, "jaehho/champalimaud");
        assert_eq!(
            rig.link("jaehho/champalimaud").state,
            Some(asst_core::api::LinkState::Conflict)
        );
        assert_eq!(
            rig.issues_asked("jaehho/champalimaud"),
            0,
            "the moved repo's issues are not read onto lab's tasks"
        );
        assert_eq!(
            rig.issues_asked("jaehho/lab"),
            lab + 1,
            "only its own issues"
        );
        rig.go().await;
        assert_eq!(rig.log().len(), 1);
    }

    #[tokio::test]
    async fn a_witnessed_rename_onto_a_linked_name_is_a_conflict_too() {
        let mut rig = Rig::new(&["jaehho/old", "jaehho/new"]).await;
        rig.publish(5, "jaehho/old", false);
        rig.publish(9, "jaehho/new", false);
        rig.go().await;
        let before = rig.snap();
        // The repo with id 5 is called `new` now, and `new` is linked
        rig.publish(5, "jaehho/new", false);

        assert_eq!(rig.go().await, Round::Done);

        let after = rig.snap();
        assert_eq!(after.links, before.links);
        assert_eq!(after.log.len(), 1);
        assert_eq!(after.log[0].kind, "rename-conflict");
        assert_eq!(
            rig.link("jaehho/old").state,
            Some(asst_core::api::LinkState::Conflict)
        );
        assert_eq!(rig.link("jaehho/new").state, None, "the other link is fine");
    }

    #[tokio::test]
    async fn an_issue_listing_that_redirects_makes_the_next_round_look_again() {
        let mut rig = Rig::new(&["jaehho/x"]).await;
        rig.publish(5, "jaehho/x", false);
        rig.go().await;
        rig.fake.reply(
            "GET",
            "/repos/jaehho/x/issues",
            Reply::status(301).header("location", "https://api.github.com/repositories/5/issues"),
        );
        rig.drain_wake().await;
        assert_eq!(rig.go().await, Round::Done);
        assert!(rig.link("jaehho/x").error.is_some(), "not a sync");
        assert!(!rig.woken().await, "no hot loop on a redirect that stays");
        let mark = rig.fake.seen().len();

        rig.fake
            .reply("GET", "/repos/jaehho/x/issues", Reply::json(200, "[]"));
        rig.go().await;
        let again = &rig.fake.seen()[mark..];
        let ask = again
            .iter()
            .find(|r| r.path() == "/repositories/5")
            .expect("asked by id");
        assert_eq!(ask.header("if-none-match"), None, "not answered by a tag");
        assert_eq!(rig.link("jaehho/x").error, None);
    }
    /// Needs `ASST_TEST_DB`, a copy of the live database made with
    /// `sqlite3 <db> ".backup <copy>"`. GitHub is a fake that agrees with
    /// what the copy holds (every repo exists under its name, its issues are
    /// its pairs, mail-digest is archived), so what a first round changes is
    /// what the upgrade changes.
    #[tokio::test]
    #[ignore = "needs ASST_TEST_DB, a copy of the live database"]
    async fn a_copy_of_the_live_database_learns_its_ids_and_nothing_else_changes() {
        let path = std::env::var("ASST_TEST_DB").expect("ASST_TEST_DB");
        let zone: Tz = "America/New_York".parse().unwrap();
        let store = Store::open(std::path::Path::new(&path), zone).unwrap();
        let mut rig = Rig::over(store, zone).await;
        let before = rig.snap();
        assert!(before.links.iter().all(|l| l.repo_id.is_none()));
        let pairs: usize = before.pairs.iter().map(|(_, p)| p.len()).sum();
        println!("{} links, {pairs} pairs", before.links.len());
        for (i, row) in before.links.iter().enumerate() {
            let archived = row.repo.ends_with("/mail-digest");
            rig.publish(7000 + i as u64, &row.repo, archived);
            let issues: Vec<_> = rig
                .daemon
                .store()
                .gh_pairs(&row.repo)
                .unwrap()
                .into_iter()
                .map(|p| {
                    let label = match p.priority {
                        1 => "P1",
                        5 => "P2",
                        9 => "P3",
                        _ => "",
                    };
                    serde_json::json!({
                        "number": p.number,
                        "title": p.title,
                        "state": if p.open { "open" } else { "closed" },
                        "body": p.body,
                        "labels": if label.is_empty() { vec![] } else { vec![serde_json::json!({ "name": label })] },
                    })
                })
                .collect();
            rig.fake.reply(
                "GET",
                &format!("/repos/{}/issues", row.repo),
                Reply::json(200, &serde_json::Value::Array(issues).to_string()),
            );
        }

        for pass in 1..=2 {
            assert_eq!(rig.go().await, Round::Done, "round {pass}");
            let after = rig.snap();
            assert_eq!(after.pairs, before.pairs, "round {pass}: pairs");
            assert_eq!(after.tasks, before.tasks, "round {pass}: tasks");
            assert!(after.log.is_empty(), "round {pass}: no log row");
            assert_eq!(after.links.len(), before.links.len());
            for (was, now) in before.links.iter().zip(&after.links) {
                assert_eq!((&was.repo, &was.list), (&now.repo, &now.list));
                assert!(now.repo_id.is_some(), "{}: id learned", now.repo);
                assert_eq!((&now.follow_from, now.follow_at), (&None, None));
                let archived = now.repo.ends_with("/mail-digest");
                assert_eq!(
                    now.archived.is_some_and(|t| t > 0),
                    archived,
                    "{}",
                    now.repo
                );
            }
            let ids: std::collections::HashSet<_> = after.links.iter().map(|l| l.repo_id).collect();
            assert_eq!(ids.len(), after.links.len(), "ids are distinct");
            let looked: Vec<_> = rig
                .daemon
                .links()
                .unwrap()
                .into_iter()
                .filter(|l| l.state.is_some())
                .map(|l| {
                    (
                        l.repo,
                        l.state.unwrap().as_str(),
                        l.note.unwrap_or_default(),
                    )
                })
                .collect();
            println!("round {pass}: {looked:?}");
            assert!(
                looked
                    .iter()
                    .all(|(repo, state, _)| repo.ends_with("/mail-digest") && *state == "archived"),
                "only mail-digest needs a look: {looked:?}"
            );
        }
    }
}
