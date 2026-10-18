//! A renamed repo renames its list, when the list is named exactly for the
//! old repo and nothing is in the way. It is the one thing asst does by
//! itself that reaches other devices (a list's name shows on the phone), so
//! it is done in small checked steps, each one written down.
//!
//! The repo's link and pairs have already moved by the time this runs (see
//! `github::rename`), and left a marker on the link: the repo's old name and
//! when it was seen. This step turns the marker into a list rename, or into a
//! reason the list keeps its name, and clears it. A marker that cannot be
//! dealt with yet (the server cannot be reached, three lists were already
//! renamed this hour) stays for the next pass, and goes after 30 days.
//!
//! Nothing here reads GitHub, and nothing here can end a GitHub round: the
//! step returns nothing, so a failure on the list side (a lost connection is
//! the likely one) is logged here and goes no further. The only thing written
//! to the server is the list's display name, after the intent to write it is
//! in the log; a crash in between is told apart on the next pass by reading
//! the name again.

use asst_core::caldav::{RemoteError, RemoteList};
use asst_core::github::{self, FollowRename, KeepWhy, limits};
use asst_core::store::{Ended, GH_LOG_KEEP, GhLinkRow, LogRow, NewLog};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::daemon::{Daemon, Result};
use crate::github::AUTO;

/// The lists renamed this way in the last hour may not come to more than
/// this, whatever the repos do. A rename past it waits.
const RENAMES_PER_HOUR: usize = limits::RENAMES_PER_HOUR;

/// A marker this old is dropped: a name that has waited this long to change
/// has had time to mean something else.
const MARKER_EXPIRES: chrono::Duration = chrono::Duration::days(limits::MARKER_EXPIRES_DAYS);

type Remote<T> = std::result::Result<T, RemoteError>;

/// What following a rename asks of the server, and nothing more: the
/// lists as they are now, and a list's name changed. `Daemon` is the real
/// one; tests use one that records what it was asked.
pub trait ListNames {
    /// The lists as the server has them now, never the cache.
    async fn fresh(&self) -> Remote<Vec<RemoteList>>;

    /// Give the list at `href` this display name, and nothing else of it.
    async fn rename(&self, href: &str, name: &str) -> Remote<()>;
}

impl ListNames for Daemon {
    async fn fresh(&self) -> Remote<Vec<RemoteList>> {
        self.server().await?.lists().await
    }

    async fn rename(&self, href: &str, name: &str) -> Remote<()> {
        let server = self.server().await?;
        server.update_list(href, Some(name), None).await?;
        // It is done on the server. The cache hears of it now, or with the
        // next sync, which the failure asks for.
        match server.lists().await {
            Ok(lists) => {
                if let Err(e) = self.adopt_lists(&lists) {
                    log::warn!("keeping the lists after a rename: {e}");
                }
            }
            Err(e) => {
                log::info!("reading the lists after a rename: {e}");
                self.request_sync();
            }
        }
        Ok(())
    }
}

/// What `list-renaming` (the intent) and `list-renamed` (the deed) say in
/// their `data`: enough to finish the rename after a crash and to undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Intent {
    /// The list, by what never changes.
    href: String,
    /// Its name before and after.
    from: String,
    to: String,
    /// The repo's name before and after, and its id.
    repo_from: String,
    repo_to: String,
    #[serde(default)]
    id: Option<u64>,
}

/// How far a rename has got, for what its row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Written down, not yet asked of the server.
    Intended,
    Done,
    /// Found done on the server after an interruption.
    Resumed,
}

impl Intent {
    fn text(&self, stage: Stage) -> String {
        let id = self
            .id
            .map_or(String::new(), |id| format!(" (GitHub id {id})"));
        let what = format!("list {:?} to {:?}", self.from, self.to);
        let repo = format!("{} is now {}{id}", self.repo_from, self.repo_to);
        let undo = format!("Undo: asst list rename {} {}", self.to, self.from);
        match stage {
            Stage::Intended => format!("renaming {what}: {repo}"),
            Stage::Done => format!("renamed {what}: {repo}. {undo}"),
            Stage::Resumed => {
                format!("renamed {what} (completed after an interruption): {repo}. {undo}")
            }
        }
    }

    /// The notification for a rename that is done: what happened to which
    /// list, why, and the one command that undoes it. The log row has the
    /// repo's id too; a notification has no use for it.
    fn notice(&self) -> (String, String) {
        (
            format!("List {:?} renamed to {:?}", self.from, self.to),
            format!(
                "{} is now {} on GitHub, so its list is renamed on the server too. \
                 Undo: asst list rename {} {}",
                self.repo_from, self.repo_to, self.to, self.from
            ),
        )
    }
}

/// A rename the log says was begun and not finished.
struct Pending {
    id: i64,
    intent: Intent,
}

/// Why a rename is not followed.
enum Keep<'a> {
    /// The marker is older than it may be.
    Expired,
    /// The list's name or standing rules it out (with the list's name).
    Why(KeepWhy, &'a str),
    /// The server said no.
    Refused(String),
    /// The list was renamed by someone else since the rename was begun
    /// (with its name now).
    Changed(&'a str),
}

impl Keep<'_> {
    fn slug(&self) -> &'static str {
        match self {
            Keep::Expired => "expired",
            Keep::Why(KeepWhy::Custom, _) => "custom",
            Keep::Why(KeepWhy::AlreadyNamed, _) => "already-named",
            Keep::Why(KeepWhy::SameName, _) => "same-name",
            Keep::Why(KeepWhy::Clash, _) => "clash",
            Keep::Why(KeepWhy::ReadOnly, _) => "read-only",
            Keep::Why(KeepWhy::Inbox, _) => "inbox",
            Keep::Why(KeepWhy::Off, _) => "off",
            Keep::Why(KeepWhy::Empty, _) => "empty",
            Keep::Refused(_) => "refused",
            Keep::Changed(_) => "changed",
        }
    }

    /// The reason, in words, for a repo that was `old` and is `new`.
    fn words(&self, old: &str, new: &str) -> String {
        match self {
            Keep::Expired => format!(
                "asstd could not do it for {} days and gave up",
                limits::MARKER_EXPIRES_DAYS
            ),
            Keep::Why(KeepWhy::Custom, list) => {
                format!("the list is called {list:?}, not {:?}", github::short(old))
            }
            Keep::Why(KeepWhy::AlreadyNamed, list) => {
                format!("the list is already named {list:?}")
            }
            Keep::Why(KeepWhy::SameName, _) => "the repo's name did not change".into(),
            Keep::Why(KeepWhy::Clash, _) => {
                format!("another list is already named {:?}", github::short(new))
            }
            Keep::Why(KeepWhy::ReadOnly, _) => "the list is read-only".into(),
            Keep::Why(KeepWhy::Inbox, _) => "the list is the inbox".into(),
            Keep::Why(KeepWhy::Off, _) => "follow_rename is off".into(),
            Keep::Why(KeepWhy::Empty, _) => format!("{new} has no name to give a list"),
            Keep::Refused(why) => format!("the server refused it ({why})"),
            Keep::Changed(now) => format!("the list was renamed to {now:?} meanwhile"),
        }
    }
}

/// The server said no to the rename itself, as it does for a list shared
/// read-only. Any other failure says nothing of the request: it waits.
fn refused(e: &RemoteError) -> bool {
    matches!(e, RemoteError::Status(403 | 405) | RemoteError::NotFound)
}

/// Follow the rename the link of the list at `href` is waiting to follow, if
/// there is one. Never fails: what goes wrong is logged here, and a marker
/// that could not be dealt with stays for the next pass.
pub async fn follow(daemon: &Daemon, lists: &impl ListNames, href: &str, now: DateTime<Utc>) {
    if let Err(e) = try_follow(daemon, lists, href, now).await {
        log::warn!("following a rename: {e}");
    }
}

async fn try_follow(
    daemon: &Daemon,
    lists: &impl ListNames,
    href: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(link) = link_of(daemon, href)? else {
        return Ok(());
    };
    let (Some(old), Some(at)) = (link.follow_from.as_deref(), link.follow_at) else {
        return Ok(());
    };
    let started = DateTime::from_timestamp(at, 0).unwrap_or(now);
    let pending = unfinished(daemon, &link, old)?;
    let setting = daemon.config().follow_rename;
    // What needs no word from the server comes first. With a rename begun,
    // the server may have it done: that is read before anything is decided.
    if pending.is_none() {
        if now - started > MARKER_EXPIRES {
            return keep(daemon, &link, old, None, &Keep::Expired, now);
        }
        if setting == FollowRename::Off {
            return keep(daemon, &link, old, None, &Keep::Why(KeepWhy::Off, ""), now);
        }
    }

    // The lists as they are now, not as the cache last had them
    let fresh = match lists.fresh().await {
        Ok(fresh) => fresh,
        Err(e) => {
            log::info!("{old} is now {}; its list waits: {e}", link.repo);
            return Ok(());
        }
    };
    daemon.adopt_lists(&fresh)?;
    // The link goes with its list if the server no longer has it
    let Some(link) = link_of(daemon, href)? else {
        return Ok(());
    };
    let rows = daemon.store().lists()?;
    let Some(list) = rows.iter().find(|l| l.href == href) else {
        return Ok(());
    };

    let pending_id = pending.as_ref().map(|p| p.id);
    if let Some(p) = &pending {
        if list.name == p.intent.to {
            return finish(daemon, &link, p.id, &p.intent, Stage::Resumed);
        }
        if list.name != p.intent.from {
            let changed = Keep::Changed(&list.name);
            return keep(daemon, &link, old, pending_id, &changed, now);
        }
    }
    // A rename that has been tried for a month is not tried again. It is
    // read for being done first, above: only one that is not done lapses.
    if pending.is_some() && now - started > MARKER_EXPIRES {
        return keep(daemon, &link, old, pending_id, &Keep::Expired, now);
    }
    let others: Vec<&str> = rows
        .iter()
        .filter(|l| l.href != href)
        .map(|l| l.name.as_str())
        .collect();
    let judged = github::follow_list_name(
        &list.name,
        old,
        &link.repo,
        &others,
        daemon.is_inbox(href),
        list.writable,
        setting,
    );
    let name = match judged {
        Ok(name) => name,
        // Only the owner changed: nothing to follow, nothing to say
        Err(KeepWhy::SameName) => {
            daemon.store().gh_clear_follow(&link.repo)?;
            return Ok(());
        }
        Err(why) => {
            return keep(
                daemon,
                &link,
                old,
                pending_id,
                &Keep::Why(why, &list.name),
                now,
            );
        }
    };

    if renamed_lately(daemon, now)? >= RENAMES_PER_HOUR {
        log::info!(
            "{old} is now {}; its list waits, since {RENAMES_PER_HOUR} lists were renamed in the last hour",
            link.repo
        );
        return Ok(());
    }

    // The intent is written before the server is asked, so a crash between
    // the two is found out and told truthfully on the next pass
    let intent = Intent {
        href: href.to_string(),
        from: list.name.clone(),
        to: name.clone(),
        repo_from: old.to_string(),
        repo_to: link.repo.clone(),
        id: link.repo_id,
    };
    if pending_id.is_none() {
        close_superseded(daemon, href)?;
    }
    let row = match pending_id {
        Some(id) => id,
        None => daemon.store().gh_log_add(&NewLog {
            at: now,
            repo: &link.repo,
            list: Some(href),
            kind: "list-renaming",
            text: &intent.text(Stage::Intended),
            data: Some(&serde_json::to_string(&intent).expect("an intent serializes")),
            by: AUTO,
        })?,
    };
    match lists.rename(href, &name).await {
        Ok(()) => finish(daemon, &link, row, &intent, Stage::Done),
        Err(e) if refused(&e) => {
            let refusal = Keep::Refused(e.to_string());
            keep(daemon, &link, old, Some(row), &refusal, now)
        }
        // Not a word about the request: the intent and the marker wait
        Err(e) => {
            log::info!("{old} is now {}; its list waits: {e}", link.repo);
            Ok(())
        }
    }
}

/// Intents of this list that nothing will finish. The repo was renamed
/// again before the server was reached, and the marker kept the first old
/// name; the list still has it (or this would not be going ahead), so the
/// earlier rename was never carried out, and its row says so.
fn close_superseded(daemon: &Daemon, href: &str) -> Result<()> {
    let store = daemon.store();
    let kept = u32::try_from(GH_LOG_KEEP).unwrap_or(u32::MAX);
    for row in store.gh_log_recent(None, kept)? {
        let ours = row
            .data
            .as_deref()
            .and_then(|d| serde_json::from_str::<Intent>(d).ok())
            .is_some_and(|intent| intent.href == href);
        if row.kind == "list-renaming" && ours {
            let text = "rename not followed: the repo was renamed again before it was done";
            store.gh_log_retag(row.id, "list-kept", text)?;
        }
    }
    Ok(())
}

fn link_of(daemon: &Daemon, href: &str) -> Result<Option<GhLinkRow>> {
    Ok(daemon
        .store()
        .gh_link_rows()?
        .into_iter()
        .find(|r| r.list == href))
}

/// The rename the log says was begun for this link's marker and not
/// finished, if any.
fn unfinished(daemon: &Daemon, link: &GhLinkRow, old: &str) -> Result<Option<Pending>> {
    for row in daemon.store().gh_log_recent(Some(&link.repo), 20)? {
        if row.kind != "list-renaming" {
            continue;
        }
        let intent = row
            .data
            .as_deref()
            .and_then(|d| serde_json::from_str::<Intent>(d).ok());
        if let Some(intent) = intent
            && intent.href == link.list
            && intent.repo_from == old
        {
            return Ok(Some(Pending { id: row.id, intent }));
        }
    }
    Ok(None)
}

/// How many lists were renamed this way in the hour before `now`.
fn renamed_lately(daemon: &Daemon, now: DateTime<Utc>) -> Result<usize> {
    let since = (now - chrono::Duration::hours(1)).timestamp();
    let rows: Vec<LogRow> = daemon.store().gh_log_recent(None, 500)?;
    Ok(rows
        .iter()
        .filter(|r| r.kind == "list-renamed" && r.at > since)
        .count())
}

/// The list has its new name on the server: the row says so, the marker
/// goes, and the person is told once.
fn finish(
    daemon: &Daemon,
    link: &GhLinkRow,
    row: i64,
    intent: &Intent,
    stage: Stage,
) -> Result<()> {
    let text = intent.text(stage);
    let ended = Ended::Row {
        id: row,
        kind: "list-renamed",
        text: &text,
    };
    daemon.store().gh_end_follow(&link.repo, ended)?;
    log::info!("{text}");
    let (summary, body) = intent.notice();
    daemon.notify(&summary, &body);
    Ok(())
}

/// The list keeps its name: the row says why (the intent's, if one was
/// written), and the marker goes.
fn keep(
    daemon: &Daemon,
    link: &GhLinkRow,
    old: &str,
    intent_row: Option<i64>,
    why: &Keep,
    now: DateTime<Utc>,
) -> Result<()> {
    let text = format!(
        "{old} is now {}; its list was not renamed: {}",
        link.repo,
        why.words(old, &link.repo)
    );
    let data = serde_json::json!({
        "reason": why.slug(),
        "href": link.list,
        "repo_from": old,
        "repo_to": link.repo,
    })
    .to_string();
    let ended = match intent_row {
        Some(id) => Ended::Row {
            id,
            kind: "list-kept",
            text: &text,
        },
        None => Ended::New(NewLog {
            at: now,
            repo: &link.repo,
            list: Some(&link.list),
            kind: "list-kept",
            text: &text,
            data: Some(&data),
            by: AUTO,
        }),
    };
    daemon.store().gh_end_follow(&link.repo, ended)?;
    log::info!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use asst_core::store::NewLog;

    fn lab() -> Intent {
        Intent {
            href: "/cal/champalimaud-21eefd/".into(),
            from: "champalimaud".into(),
            to: "lab".into(),
            repo_from: "jaehho/champalimaud".into(),
            repo_to: "jaehho/lab".into(),
            id: Some(1379104005),
        }
    }

    #[test]
    fn a_rename_is_told_in_one_sentence_with_its_undo() {
        let intent = lab();
        assert_eq!(
            intent.text(Stage::Done),
            "renamed list \"champalimaud\" to \"lab\": jaehho/champalimaud is now jaehho/lab \
             (GitHub id 1379104005). Undo: asst list rename lab champalimaud"
        );
        // Written down first, without an undo for what has not happened
        assert_eq!(
            intent.text(Stage::Intended),
            "renaming list \"champalimaud\" to \"lab\": jaehho/champalimaud is now jaehho/lab \
             (GitHub id 1379104005)"
        );
        assert_eq!(
            intent.text(Stage::Resumed),
            "renamed list \"champalimaud\" to \"lab\" (completed after an interruption): \
             jaehho/champalimaud is now jaehho/lab (GitHub id 1379104005). \
             Undo: asst list rename lab champalimaud"
        );
        // The notification: what, why and the undo, without the id
        assert_eq!(
            intent.notice(),
            (
                "List \"champalimaud\" renamed to \"lab\"".to_string(),
                "jaehho/champalimaud is now jaehho/lab on GitHub, so its list is renamed on \
                 the server too. Undo: asst list rename lab champalimaud"
                    .to_string()
            )
        );
        // The row's data is what a restart reads back
        let json = serde_json::to_string(&intent).unwrap();
        assert_eq!(serde_json::from_str::<Intent>(&json).unwrap(), intent);
        let no_id = Intent { id: None, ..intent };
        assert!(!no_id.text(Stage::Done).contains("GitHub id"));
    }

    #[test]
    fn a_list_that_keeps_its_name_says_why_in_words() {
        let (old, new) = ("jaehho/champalimaud", "jaehho/lab");
        let cases = [
            (
                Keep::Expired,
                "expired",
                "asstd could not do it for 30 days and gave up",
            ),
            (
                Keep::Why(KeepWhy::Custom, "Lab work"),
                "custom",
                "the list is called \"Lab work\", not \"champalimaud\"",
            ),
            (
                Keep::Why(KeepWhy::AlreadyNamed, "lab"),
                "already-named",
                "the list is already named \"lab\"",
            ),
            (
                Keep::Why(KeepWhy::Clash, "champalimaud"),
                "clash",
                "another list is already named \"lab\"",
            ),
            (
                Keep::Why(KeepWhy::ReadOnly, "champalimaud"),
                "read-only",
                "the list is read-only",
            ),
            (
                Keep::Why(KeepWhy::Inbox, "champalimaud"),
                "inbox",
                "the list is the inbox",
            ),
            (
                Keep::Why(KeepWhy::Off, "champalimaud"),
                "off",
                "follow_rename is off",
            ),
            (
                Keep::Why(KeepWhy::Empty, "champalimaud"),
                "empty",
                "jaehho/lab has no name to give a list",
            ),
            (
                Keep::Refused("the server answered 403".into()),
                "refused",
                "the server refused it (the server answered 403)",
            ),
            (
                Keep::Changed("Lab notes"),
                "changed",
                "the list was renamed to \"Lab notes\" meanwhile",
            ),
        ];
        for (why, slug, words) in cases {
            assert_eq!(why.slug(), slug);
            assert_eq!(why.words(old, new), words, "{slug}");
        }
    }

    #[test]
    fn only_a_no_from_the_server_is_a_refusal() {
        for refusal in [
            RemoteError::Status(403),
            RemoteError::Status(405),
            RemoteError::NotFound,
        ] {
            assert!(refused(&refusal), "{refusal}");
        }
        // An outage, a bad moment, a login to renew: none says no
        for wait in [
            RemoteError::Network("timed out".into()),
            RemoteError::Unauthorized,
            RemoteError::Status(500),
            RemoteError::Status(503),
            RemoteError::Status(423),
            RemoteError::Conflict,
            RemoteError::Protocol("not xml".into()),
        ] {
            assert!(!refused(&wait), "{wait}");
        }
    }

    /// The real `ListNames` and the whole step, on a real server, in lists
    /// the test makes and removes. Never the real server: a local Radicale
    /// (`scripts/dev up` starts one) will do.
    ///
    ///   ASST_TEST_DAV_URL=http://127.0.0.1:5232/ ASST_TEST_USERNAME=follow-test \
    ///   ASST_PASSWORD=x cargo test -p asstd follow -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a CalDAV server"]
    async fn the_daemon_follows_a_rename_on_a_real_server() {
        use asst_core::caldav::{Account, CalDav};
        use asst_core::config::{AccountConfig, Config};
        use asst_core::store::Store;
        let (Ok(dav_url), Ok(username), Ok(password)) = (
            std::env::var("ASST_TEST_DAV_URL"),
            std::env::var("ASST_TEST_USERNAME"),
            std::env::var("ASST_PASSWORD"),
        ) else {
            eprintln!("ASST_TEST_DAV_URL, ASST_TEST_USERNAME, ASST_PASSWORD not set; skipping");
            return;
        };
        let home = CalDav::discover(&Account {
            dav_url: dav_url.clone(),
            username: username.clone(),
            password,
        })
        .await
        .expect("discovery")
        .home()
        .to_string();
        let account = AccountConfig {
            server: dav_url.trim_end_matches('/').into(),
            dav_url,
            username,
            home,
        };
        let zone: chrono_tz::Tz = "America/New_York".parse().unwrap();
        let config = Config {
            account: Some(account),
            ..Config::default()
        };
        let (daemon, mut events) = Daemon::new(config, Store::in_memory(zone).unwrap(), zone);
        let server = daemon.server().await.expect("the server");

        // Two lists: the one a repo is named for, and one with a name of
        // its own that must not move
        let old = server
            .create_list("follow-test-old", "follow-test-old", None)
            .await
            .expect("MKCOL");
        let other = server
            .create_list("follow-test-other", "Follow test other", None)
            .await
            .expect("MKCOL");
        let outcome = async {
            // Read as the daemon reads it
            let fresh = ListNames::fresh(&*daemon)
                .await
                .map_err(|e| e.to_string())?;
            let name = |lists: &[RemoteList], href: &str| {
                lists
                    .iter()
                    .find(|l| l.href == href)
                    .map(|l| l.name.clone())
            };
            assert_eq!(name(&fresh, &old).as_deref(), Some("follow-test-old"));

            // The step, from a marker as the GitHub pass leaves it
            daemon.adopt_lists(&fresh).map_err(|e| e.to_string())?;
            let now = Utc::now();
            {
                let store = daemon.store();
                store.gh_link("jaehho/follow-test-old", &old).unwrap();
                store.gh_learn_id("jaehho/follow-test-old", 4242).unwrap();
                let log = NewLog {
                    at: now,
                    repo: "jaehho/follow-test-new",
                    list: Some(&old),
                    kind: "repo-renamed",
                    text: "renamed",
                    data: None,
                    by: AUTO,
                };
                store
                    .gh_rename(
                        "jaehho/follow-test-old",
                        "jaehho/follow-test-new",
                        Some(now),
                        Some(&log),
                    )
                    .unwrap();
            }
            follow(&daemon, &*daemon, &old, now).await;
            let after = ListNames::fresh(&*daemon)
                .await
                .map_err(|e| e.to_string())?;
            assert_eq!(name(&after, &old).as_deref(), Some("follow-test-new"));
            assert_eq!(name(&after, &other).as_deref(), Some("Follow test other"));
            // The cache heard of it without a sync
            let cached = daemon.lists().unwrap();
            let cached = cached.iter().find(|l| l.href == old).unwrap();
            assert_eq!(cached.name, "follow-test-new");
            let log = daemon.store().gh_log_recent(None, 10).unwrap();
            let kinds: Vec<&str> = log.iter().map(|r| r.kind.as_str()).collect();
            assert_eq!(kinds, ["list-renamed", "repo-renamed"]);
            assert!(
                daemon.store().gh_link_rows().unwrap()[0]
                    .follow_from
                    .is_none()
            );
            let mut told = 0;
            while let Ok(event) = events.try_recv() {
                told += usize::from(matches!(event, crate::daemon::Event::Notice { .. }));
            }
            assert_eq!(told, 1);

            // A list that is not there: the server says no, and that is a refusal
            let gone = format!(
                "{home}follow-test-nothing/",
                home = daemon.config().account.unwrap().home
            );
            let err = ListNames::rename(&*daemon, &gone, "x").await.unwrap_err();
            println!("renaming a list that is not there: {err:?}");
            assert!(refused(&err), "{err:?}");
            Ok::<(), String>(())
        }
        .await;
        for href in [&old, &other] {
            server.delete_list(href, true).await.expect("remove");
        }
        outcome.expect("the step");
    }

    /// Nothing here deletes: no unlink, no list removed, no row dropped. The
    /// code that judges a repo and follows its rename does not even name
    /// what would.
    #[test]
    fn the_code_that_follows_a_rename_names_nothing_that_deletes() {
        let sources = [
            ("follow.rs", include_str!("follow.rs")),
            ("github.rs", include_str!("github.rs")),
            (
                "asst-core github.rs",
                include_str!("../../asst-core/src/github.rs"),
            ),
        ];
        for (file, text) in sources {
            let code = text.split("\n#[cfg(test)]\nmod ").next().unwrap();
            assert!(code.len() > 1000, "{file}: the code was found");
            for word in ["delete_list", "gh_unlink", "remove_list", "DELETE FROM"] {
                assert!(!code.contains(word), "{file} names {word}");
            }
        }
    }
}
