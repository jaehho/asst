//! `asst projects`: what the links between lists and GitHub repos need, in
//! one report that reads and never changes anything.
//!
//! The daemon knows the links (`links()`, which already says what each one
//! needs a look for) and keeps the log of what it changed by itself. The
//! window knows which lists are put away, and the database which labels the
//! tracked time carries. `gather` reads those; `build` turns them into a
//! report and is pure, so the report is tested from structs.
//!
//! Nothing here asks GitHub or Nextcloud. What the report proposes, it
//! proposes from the log's `repo-renamed` rows and from the state the
//! daemon already holds, and each proposal is the exact command that does
//! it. The daemon makes the one change that is made without being asked
//! (`follow.rs`); everything else is for the owner to run.

use asst_core::api::{AsstProxy, LinkView, ListView, Settings};
use asst_core::config;
use asst_core::github::{FollowRename, follow_list_name, limits, short};
use asst_core::store::{GH_LOG_KEEP, LabelTotal, LogRow, Store};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

/// What the report is made of, as it was read.
pub struct Facts {
    pub links: Vec<LinkView>,
    pub lists: Vec<ListView>,
    /// The lists (hrefs) the window has put away.
    pub archived_in_window: Vec<String>,
    /// The decision log, newest first.
    pub log: Vec<LogRow>,
    /// Actual tracked time by label.
    pub labels: Vec<LabelTotal>,
    pub follow_rename: FollowRename,
    /// What could not be read, in words. The report is made of the rest.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub links: Vec<ProjectLink>,
    /// Changes the owner can make; nothing has been changed.
    pub proposals: Vec<Proposal>,
    /// Labels on tracked time that are an old name of a project.
    pub labels: Vec<StaleLabel>,
    pub settings: Rules,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectLink {
    pub repo: String,
    pub repo_id: Option<u64>,
    /// The list's href, which never changes, and its name.
    pub list: String,
    pub name: String,
    /// `ok`, or `archived`, `renamed`, `unreachable`, `replaced`, `conflict`.
    pub state: String,
    pub note: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub open_tasks: Option<usize>,
    pub archived_in_window: bool,
    pub pending_rename: Option<Pending>,
}

/// A rename the list has not followed yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pending {
    /// The repo's name before.
    pub from: String,
    pub since: DateTime<Utc>,
    /// When asstd gives up on it.
    pub expires: DateTime<Utc>,
    /// The rename was begun on the server and not finished.
    pub begun: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub kind: &'static str,
    pub repo: String,
    /// The list's href.
    pub list: String,
    /// The list's name now, and the name it would take.
    pub from: String,
    pub to: String,
    pub command: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaleLabel {
    pub label: String,
    /// What the project is called now.
    pub now: String,
    pub repo: String,
    pub blocks: usize,
    pub seconds: i64,
}

/// What asstd holds itself to, for tools that need to know.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rules {
    pub follow_rename: FollowRename,
    pub renames_per_hour: usize,
    pub marker_expires_days: i64,
    pub log_keep: i64,
    pub retry: Retry,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Retry {
    /// A refusal with no `Retry-After` and no rate-limit reset.
    pub refused_wait_minutes: i64,
    /// What a refusal that names a time is waited at the most.
    pub longest_wait_minutes: i64,
    /// A repo GitHub cannot find by its id is asked for by name this often.
    pub name_lookup_minutes: i64,
    /// A repo GitHub cannot find gets a hint at `asst unlink` after this.
    pub unlink_hint_days: i64,
}

impl Rules {
    pub fn new(follow_rename: FollowRename) -> Rules {
        Rules {
            follow_rename,
            renames_per_hour: limits::RENAMES_PER_HOUR,
            marker_expires_days: limits::MARKER_EXPIRES_DAYS,
            log_keep: GH_LOG_KEEP,
            retry: Retry {
                refused_wait_minutes: limits::REFUSED_WAIT_MINUTES,
                longest_wait_minutes: limits::LONGEST_WAIT_MINUTES,
                name_lookup_minutes: limits::NAME_LOOKUP_MINUTES,
                unlink_hint_days: limits::UNLINK_HINT_DAYS,
            },
        }
    }
}

/// What to say when asstd has no `LinkLog`: it was started before this asst
/// was built, which is how it is after a merge and before a restart.
pub const OLD_DAEMON: &str =
    "this asstd is older than asst and keeps no log; restart it (systemctl --user restart asstd)";

/// Whether the bus said asstd has no such method.
pub fn is_unknown_method(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => {
            name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
        }
        zbus::Error::FDO(fdo) => matches!(**fdo, zbus::fdo::Error::UnknownMethod(_)),
        _ => false,
    }
}

/// Read what the report is made of. A daemon too old to keep a log, or a
/// database that cannot be read, leaves that part out and says so; the
/// daemon itself being out of reach is an error, as it is for every command.
pub async fn gather(proxy: &AsstProxy<'_>) -> anyhow::Result<Facts> {
    let links: Vec<LinkView> = serde_json::from_str(&proxy.links().await?)?;
    let lists: Vec<ListView> = serde_json::from_str(&proxy.lists().await?)?;
    let settings: Settings = serde_json::from_str(&proxy.settings().await?)?;
    let mut warnings = Vec::new();

    let kept = u32::try_from(GH_LOG_KEEP).unwrap_or(u32::MAX);
    let log = match proxy.link_log("", kept).await {
        Ok(raw) => serde_json::from_str(&raw)?,
        Err(e) => {
            let why = if is_unknown_method(&e) {
                OLD_DAEMON.to_string()
            } else {
                format!("asstd did not give its log: {}", crate::message(&e.into()))
            };
            warnings.push(format!(
                "{why}. Without it there are no renames to propose and no old labels"
            ));
            Vec::new()
        }
    };
    let db = config::state_dir().join("asst.db");
    let labels = Store::label_totals(&db).unwrap_or_else(|e| {
        warnings.push(format!(
            "the block labels in {} were not read: {e}",
            db.display()
        ));
        Vec::new()
    });
    Ok(Facts {
        links,
        lists,
        archived_in_window: config::archived_in_window(),
        log,
        labels,
        follow_rename: settings.follow_rename,
        warnings,
    })
}

pub fn build(facts: Facts) -> Report {
    let Facts {
        links,
        lists,
        archived_in_window,
        log,
        labels,
        follow_rename,
        warnings,
    } = facts;
    let rows = links
        .iter()
        .map(|l| ProjectLink {
            repo: l.repo.clone(),
            repo_id: l.repo_id,
            list: l.list.clone(),
            name: l.name.clone(),
            state: l.state.map_or("ok", |s| s.as_str()).to_string(),
            note: l.note.clone(),
            since: l.since,
            open_tasks: lists.iter().find(|x| x.href == l.list).map(|x| x.open),
            archived_in_window: archived_in_window.contains(&l.list),
            pending_rename: l.pending_rename.as_ref().map(|p| Pending {
                from: p.from.clone(),
                since: p.since,
                expires: p.since + Duration::days(limits::MARKER_EXPIRES_DAYS),
                begun: begun(&log, l, &p.from),
            }),
        })
        .collect();
    Report {
        proposals: proposals(&links, &lists, &log),
        labels: stale_labels(&links, &lists, &log, &labels),
        links: rows,
        settings: Rules::new(follow_rename),
        warnings,
    }
}

/// The words `from` and `to` of a `repo-renamed` row.
fn renamed(row: &LogRow) -> Option<(String, String)> {
    if row.kind != "repo-renamed" {
        return None;
    }
    let data: serde_json::Value = serde_json::from_str(row.data.as_deref()?).ok()?;
    let word = |key: &str| data.get(key)?.as_str().map(str::to_string);
    Some((word("from")?, word("to")?))
}

/// The names the repo had before, as the log saw them change, most recent
/// first: `repo-renamed` rows say from and to, and are followed back from
/// the name it has now. `log` is newest first.
fn former_names(repo: &str, log: &[LogRow]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut now = repo.to_string();
    loop {
        let from = log
            .iter()
            .filter_map(renamed)
            .find(|(_, to)| to.eq_ignore_ascii_case(&now))
            .map(|(from, _)| from);
        match from {
            // Back to a name already seen: the log goes round, and ends here
            Some(from)
                if !from.eq_ignore_ascii_case(repo)
                    && !names.iter().any(|n| n.eq_ignore_ascii_case(&from)) =>
            {
                names.push(from.clone());
                now = from;
            }
            _ => return names,
        }
    }
}

/// A rename of the list was begun on the server for this link's marker and
/// not finished: the log still has its intent.
fn begun(log: &[LogRow], link: &LinkView, from: &str) -> bool {
    log.iter().any(|r| {
        let data: Option<serde_json::Value> =
            r.data.as_deref().and_then(|d| serde_json::from_str(d).ok());
        let word = |key: &str| data.as_ref()?.get(key)?.as_str().map(str::to_string);
        r.kind == "list-renaming"
            && r.repo.eq_ignore_ascii_case(&link.repo)
            && word("href").as_deref() == Some(link.list.as_str())
            && word("repo_from").as_deref() == Some(from)
    })
}

/// The list was given its name back by hand after asstd renamed it: the
/// newest `list-renamed` row for it says it was `name` before. That is a
/// decision, and it is not proposed away.
fn undone(log: &[LogRow], href: &str, name: &str) -> bool {
    let newest = log
        .iter()
        .find(|r| r.kind == "list-renamed" && r.list.as_deref() == Some(href));
    let from = newest
        .and_then(|r| serde_json::from_str::<serde_json::Value>(r.data.as_deref()?).ok())
        .and_then(|d| d.get("from")?.as_str().map(str::to_string));
    from.as_deref() == Some(name)
}

/// A word as a shell takes it: in double quotes when that is all it needs,
/// else in single quotes.
pub fn quoted(word: &str) -> String {
    if word.contains(['"', '$', '`', '\\', '!', '\'']) {
        format!("'{}'", word.replace('\'', "'\\''"))
    } else {
        format!("\"{word}\"")
    }
}

/// Lists still named for a repo's old name, where the repo's new name is
/// free to take: what asstd would have done had it seen the rename happen.
/// The same judgement it makes (`follow_list_name`), so the report never
/// proposes what the daemon would refuse. A link whose rename asstd still
/// means to follow is left to it.
fn proposals(links: &[LinkView], lists: &[ListView], log: &[LogRow]) -> Vec<Proposal> {
    let mut found = Vec::new();
    for link in links.iter().filter(|l| l.pending_rename.is_none()) {
        let Some(list) = lists.iter().find(|l| l.href == link.list) else {
            continue;
        };
        let others: Vec<&str> = lists
            .iter()
            .filter(|l| l.href != list.href)
            .map(|l| l.name.as_str())
            .collect();
        for former in former_names(&link.repo, log) {
            let judged = follow_list_name(
                &list.name,
                &former,
                &link.repo,
                &others,
                list.inbox,
                list.writable,
                FollowRename::List,
            );
            let Ok(to) = judged else {
                continue;
            };
            if undone(log, &list.href, &list.name) {
                break;
            }
            // Two lists called alike are told apart by what never changes
            let alike = others.iter().any(|n| n.eq_ignore_ascii_case(&list.name));
            let which = if alike { &list.href } else { &list.name };
            found.push(Proposal {
                kind: "rename-list",
                repo: link.repo.clone(),
                list: list.href.clone(),
                from: list.name.clone(),
                command: format!("asst list rename {} {}", quoted(which), quoted(&to)),
                why: format!(
                    "{former} is now {}; the list is still called {:?}",
                    link.repo, list.name
                ),
                to,
            });
            break;
        }
    }
    found
}

/// Tracked time labelled with a name a project used to have, while nothing
/// is called that any more: the list and every link have moved on. Only
/// names the log saw change are known; none is guessed.
fn stale_labels(
    links: &[LinkView],
    lists: &[ListView],
    log: &[LogRow],
    totals: &[LabelTotal],
) -> Vec<StaleLabel> {
    let current: Vec<&str> = links
        .iter()
        .map(|l| short(&l.repo))
        .chain(lists.iter().map(|l| l.name.as_str()))
        .collect();
    let mut found: Vec<StaleLabel> = Vec::new();
    for link in links {
        for former in former_names(&link.repo, log) {
            let old = short(&former);
            if old.is_empty() || current.iter().any(|c| c.eq_ignore_ascii_case(old)) {
                continue;
            }
            for t in totals.iter().filter(|t| t.label.eq_ignore_ascii_case(old)) {
                if found.iter().any(|f| f.label == t.label) {
                    continue;
                }
                found.push(StaleLabel {
                    label: t.label.clone(),
                    now: short(&link.repo).to_string(),
                    repo: link.repo.clone(),
                    blocks: t.blocks,
                    seconds: t.seconds,
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use asst_core::api::{LinkState, PendingRename};
    use chrono::TimeZone;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, 16, 0, 0).unwrap()
    }

    fn link(repo: &str, href: &str, name: &str) -> LinkView {
        LinkView {
            repo: repo.into(),
            list: href.into(),
            name: name.into(),
            ..LinkView::default()
        }
    }

    fn list(href: &str, name: &str, open: usize) -> ListView {
        ListView {
            href: href.into(),
            name: name.into(),
            color: None,
            writable: true,
            inbox: false,
            open,
            done: 0,
        }
    }

    fn row(id: i64, kind: &str, repo: &str, list: &str, data: serde_json::Value) -> LogRow {
        LogRow {
            id,
            at: at(18).timestamp(),
            repo: repo.into(),
            list: Some(list.into()),
            kind: kind.into(),
            text: format!("{kind} {repo}"),
            data: Some(data.to_string()),
            by: "auto".into(),
        }
    }

    fn rename_row(id: i64, from: &str, to: &str, list: &str) -> LogRow {
        row(
            id,
            "repo-renamed",
            to,
            list,
            serde_json::json!({ "from": from, "to": to, "id": 1, "witnessed": false }),
        )
    }

    fn list_renamed(id: i64, href: &str, from: &str, to: &str) -> LogRow {
        row(
            id,
            "list-renamed",
            "jaehho/lab",
            href,
            serde_json::json!({
                "href": href, "from": from, "to": to,
                "repo_from": "jaehho/champalimaud", "repo_to": "jaehho/lab",
            }),
        )
    }

    fn facts(links: Vec<LinkView>, lists: Vec<ListView>, log: Vec<LogRow>) -> Facts {
        Facts {
            links,
            lists,
            archived_in_window: Vec::new(),
            log,
            labels: Vec::new(),
            follow_rename: FollowRename::List,
            warnings: Vec::new(),
        }
    }

    const HREF: &str = "/cal/champalimaud-21eefd/";

    #[test]
    fn names_a_repo_had_are_followed_back_through_the_log() {
        // Newest first, as the daemon gives it
        let log = [
            rename_row(5, "jaehho/b", "jaehho/c", "/cal/x/"),
            rename_row(4, "jaehho/other", "jaehho/y", "/cal/y/"),
            rename_row(2, "jaehho/a", "jaehho/b", "/cal/x/"),
        ];
        assert_eq!(former_names("jaehho/c", &log), ["jaehho/b", "jaehho/a"]);
        assert_eq!(former_names("Jaehho/C", &log), ["jaehho/b", "jaehho/a"]);
        assert_eq!(former_names("jaehho/b", &log), ["jaehho/a"]);
        assert!(former_names("jaehho/a", &log).is_empty());
        assert!(former_names("jaehho/c", &[]).is_empty());

        // A repo renamed back and forth ends the walk, not the program
        let round = [
            rename_row(3, "jaehho/b", "jaehho/a", "/cal/x/"),
            rename_row(2, "jaehho/a", "jaehho/b", "/cal/x/"),
        ];
        assert_eq!(former_names("jaehho/a", &round), ["jaehho/b"]);
        assert_eq!(former_names("jaehho/b", &round), ["jaehho/a"]);

        // Rows that are not renames, or not readable, say nothing
        let mut odd = vec![
            row(
                9,
                "repo-archived",
                "jaehho/c",
                "/cal/x/",
                serde_json::json!({}),
            ),
            rename_row(8, "jaehho/b", "jaehho/c", "/cal/x/"),
        ];
        odd[1].data = Some("not json".into());
        assert!(former_names("jaehho/c", &odd).is_empty());
        odd[1].data = None;
        assert!(former_names("jaehho/c", &odd).is_empty());
    }

    #[test]
    fn a_list_still_named_for_an_old_name_is_proposed_the_new_one() {
        // The repo was renamed while asstd was not looking
        let links = [link("jaehho/lab", HREF, "champalimaud")];
        let lists = [
            list(HREF, "champalimaud", 3),
            list("/cal/inbox/", "Inbox", 1),
        ];
        let log = [rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF)];
        let p = proposals(&links, &lists, &log);
        assert_eq!(
            p,
            [Proposal {
                kind: "rename-list",
                repo: "jaehho/lab".into(),
                list: HREF.into(),
                from: "champalimaud".into(),
                to: "lab".into(),
                command: "asst list rename \"champalimaud\" \"lab\"".into(),
                why: "jaehho/champalimaud is now jaehho/lab; the list is still called \
                      \"champalimaud\""
                    .into(),
            }]
        );

        // Renamed twice, the list never followed: the oldest name is the list's
        let links = [link("jaehho/c", HREF, "a")];
        let lists = [list(HREF, "a", 0)];
        let log = [
            rename_row(2, "jaehho/b", "jaehho/c", HREF),
            rename_row(1, "jaehho/a", "jaehho/b", HREF),
        ];
        let p = proposals(&links, &lists, &log);
        assert_eq!((p[0].from.as_str(), p[0].to.as_str()), ("a", "c"));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn nothing_is_proposed_that_asstd_would_not_do_itself() {
        let log = [rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF)];
        let links = [link("jaehho/lab", HREF, "champalimaud")];
        let named = |name: &str| [list(HREF, name, 0)];
        let none = |links: &[LinkView], lists: &[ListView], log: &[LogRow]| {
            assert_eq!(proposals(links, lists, log), [], "{lists:?}");
        };

        // Given a name of its own, or already named, nothing to follow
        none(&links, &named("Lab work"), &log);
        none(&links, &named("Champalimaud"), &log);
        none(&[link("jaehho/lab", HREF, "lab")], &named("lab"), &log);
        none(&links, &named("lab"), &log);
        // Another list has the new name, in any case
        let clash = [list(HREF, "champalimaud", 0), list("/cal/lab/", "LAB", 0)];
        none(&links, &clash, &log);
        // The inbox is a name that tasks are filed under; a read-only list
        // cannot be renamed
        let mut inbox = list(HREF, "champalimaud", 0);
        inbox.inbox = true;
        none(&links, &[inbox], &log);
        let mut shared = list(HREF, "champalimaud", 0);
        shared.writable = false;
        none(&links, &[shared], &log);
        // Only the owner changed
        let transfer = [rename_row(1, "me/lab", "you/lab", HREF)];
        none(&[link("you/lab", HREF, "lab")], &named("lab"), &transfer);
        // A list the daemon does not know, no rename in the log
        none(&links, &[], &log);
        none(&links, &named("champalimaud"), &[]);

        // A rename asstd means to follow is its to do, not a proposal
        let mut waiting = links[0].clone();
        waiting.pending_rename = Some(PendingRename {
            from: "jaehho/champalimaud".into(),
            since: at(18),
        });
        none(&[waiting], &named("champalimaud"), &log);
    }

    #[test]
    fn a_list_given_back_its_name_by_hand_stays_so() {
        let links = [link("jaehho/lab", HREF, "champalimaud")];
        let lists = [list(HREF, "champalimaud", 0)];
        let renamed_by_hand_back = [
            list_renamed(2, HREF, "champalimaud", "lab"),
            rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF),
        ];
        assert_eq!(proposals(&links, &lists, &renamed_by_hand_back), []);

        // Another list's undo says nothing about this one
        let other = [
            list_renamed(2, "/cal/other/", "champalimaud", "lab"),
            rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF),
        ];
        assert_eq!(proposals(&links, &lists, &other).len(), 1);
    }

    #[test]
    fn two_lists_called_alike_are_told_apart_by_href() {
        let links = [link("jaehho/lab", HREF, "champalimaud")];
        let lists = [
            list(HREF, "champalimaud", 0),
            list("/cal/second/", "Champalimaud", 0),
        ];
        let log = [rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF)];
        let p = proposals(&links, &lists, &log);
        assert_eq!(
            p[0].command,
            "asst list rename \"/cal/champalimaud-21eefd/\" \"lab\""
        );
    }

    #[test]
    fn a_name_is_quoted_for_the_shell_it_is_pasted_in() {
        assert_eq!(quoted("lab"), "\"lab\"");
        assert_eq!(quoted("lab work"), "\"lab work\"");
        assert_eq!(quoted("$HOME"), "'$HOME'");
        assert_eq!(quoted("say \"hi\""), "'say \"hi\"'");
        assert_eq!(quoted("it's"), "'it'\\''s'");
        assert_eq!(quoted("a!b"), "'a!b'");
    }

    #[test]
    fn an_old_name_on_tracked_time_is_reported_once_nothing_is_called_that() {
        let links = [link("jaehho/lab", HREF, "lab")];
        let lists = [list(HREF, "lab", 0)];
        let log = [rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF)];
        let total = |label: &str, blocks, seconds| LabelTotal {
            label: label.into(),
            blocks,
            seconds,
        };
        let totals = [
            total("champalimaud", 127, 152_424),
            total("lab", 4, 3600),
            total("Champalimaud", 2, 60),
            total("dotfiles", 9, 9),
        ];
        let found = stale_labels(&links, &lists, &log, &totals);
        assert_eq!(
            found,
            [
                StaleLabel {
                    label: "champalimaud".into(),
                    now: "lab".into(),
                    repo: "jaehho/lab".into(),
                    blocks: 127,
                    seconds: 152_424
                },
                StaleLabel {
                    label: "Champalimaud".into(),
                    now: "lab".into(),
                    repo: "jaehho/lab".into(),
                    blocks: 2,
                    seconds: 60
                },
            ]
        );

        // No rename in the log, no old name known: a label is only a label
        assert_eq!(stale_labels(&links, &lists, &[], &totals), []);
        // Time already labelled with the new name is not behind
        assert_eq!(
            stale_labels(&links, &lists, &log, &[total("lab", 4, 1)]),
            []
        );
        // While the list is still called the old name, it is not old yet
        let lists = [list(HREF, "champalimaud", 0)];
        assert_eq!(stale_labels(&links, &lists, &log, &totals), []);
        // Nor while another link has that name
        let lists = [list(HREF, "lab", 0)];
        let both = [
            link("jaehho/lab", HREF, "lab"),
            link("jaehho/champalimaud", "/cal/new/", "new"),
        ];
        assert_eq!(stale_labels(&both, &lists, &log, &totals), []);
    }

    #[test]
    fn the_report_is_made_of_what_was_read() {
        let mut archived = link("jaehho/mail-digest", "/cal/md/", "mail-digest");
        archived.state = Some(LinkState::Archived);
        archived.note = Some("first seen archived 2026-10-18; 0 open tasks".into());
        archived.since = Some(at(18));
        archived.repo_id = Some(1379104203);
        let mut waiting = link("jaehho/lab", HREF, "champalimaud");
        waiting.state = Some(LinkState::Renamed);
        waiting.pending_rename = Some(PendingRename {
            from: "jaehho/champalimaud".into(),
            since: at(18),
        });
        let log = vec![
            row(
                2,
                "list-renaming",
                "jaehho/lab",
                HREF,
                serde_json::json!({
                    "href": HREF, "from": "champalimaud", "to": "lab",
                    "repo_from": "jaehho/champalimaud", "repo_to": "jaehho/lab",
                }),
            ),
            rename_row(1, "jaehho/champalimaud", "jaehho/lab", HREF),
        ];
        let mut f = facts(
            vec![waiting, archived],
            vec![
                list(HREF, "champalimaud", 3),
                list("/cal/md/", "mail-digest", 0),
            ],
            log,
        );
        f.archived_in_window = vec!["/cal/md/".into()];
        f.follow_rename = FollowRename::Off;
        let r = build(f);

        assert_eq!(r.links.len(), 2);
        let lab = &r.links[0];
        assert_eq!((lab.state.as_str(), lab.open_tasks), ("renamed", Some(3)));
        assert!(!lab.archived_in_window);
        let pending = lab.pending_rename.as_ref().unwrap();
        assert_eq!(pending.from, "jaehho/champalimaud");
        assert_eq!(pending.expires, at(18) + Duration::days(30));
        assert!(pending.begun, "the intent is in the log");
        let md = &r.links[1];
        assert_eq!((md.state.as_str(), md.open_tasks), ("archived", Some(0)));
        assert!(md.archived_in_window);
        assert_eq!(md.repo_id, Some(1379104203));
        assert_eq!(md.pending_rename, None);
        // The rename asstd will follow is not also proposed
        assert!(r.proposals.is_empty());

        // The contract for other tools
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["links"][1]["state"], "archived");
        assert_eq!(json["links"][1]["archived_in_window"], true);
        assert_eq!(json["links"][0]["pending_rename"]["begun"], true);
        assert_eq!(json["links"][0]["name"], "champalimaud");
        assert_eq!(json["links"][0]["list"], HREF);
        assert_eq!(
            json["settings"],
            serde_json::json!({
                "follow_rename": "off",
                "renames_per_hour": 3,
                "marker_expires_days": 30,
                "log_keep": 500,
                "retry": {
                    "refused_wait_minutes": 15,
                    "longest_wait_minutes": 60,
                    "name_lookup_minutes": 60,
                    "unlink_hint_days": 14,
                },
            })
        );
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            keys,
            ["labels", "links", "proposals", "settings", "warnings"]
        );
    }

    #[test]
    fn a_rename_begun_is_told_from_one_that_was_not() {
        let waiting = LinkView {
            pending_rename: Some(PendingRename {
                from: "jaehho/champalimaud".into(),
                since: at(18),
            }),
            ..link("jaehho/lab", HREF, "champalimaud")
        };
        let from = "jaehho/champalimaud";
        assert!(!begun(&[], &waiting, from));
        let intent = |href: &str, repo_from: &str| {
            row(
                1,
                "list-renaming",
                "jaehho/lab",
                href,
                serde_json::json!({ "href": href, "repo_from": repo_from }),
            )
        };
        assert!(begun(&[intent(HREF, from)], &waiting, from));
        // Another list's, or another rename's, is not this one's
        assert!(!begun(&[intent("/cal/x/", from)], &waiting, from));
        assert!(!begun(&[intent(HREF, "jaehho/older")], &waiting, from));
        // One that was finished is no longer an intent
        let done = LogRow {
            kind: "list-renamed".into(),
            ..intent(HREF, from)
        };
        assert!(!begun(&[done], &waiting, from));
    }
}
