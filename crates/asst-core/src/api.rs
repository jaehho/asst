//! The D-Bus interface between `asstd` and its clients. Arguments and
//! results are JSON strings of the types below: easy to evolve, and readable
//! with `busctl --user call dev.jaeho.Asst.Daemon /dev/jaeho/Asst dev.jaeho.Asst1 Status`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::github::FollowRename;
use crate::store::{Block, Query, Row};
use crate::task::Task;
use crate::time::{Trigger, When};

/// The app itself (GApplication, desktop entry) owns `dev.jaeho.Asst`.
pub const BUS_NAME: &str = "dev.jaeho.Asst.Daemon";
pub const OBJECT_PATH: &str = "/dev/jaeho/Asst";

#[zbus::proxy(
    interface = "dev.jaeho.Asst1",
    default_service = "dev.jaeho.Asst.Daemon",
    default_path = "/dev/jaeho/Asst"
)]
pub trait Asst {
    /// → `StatusView`
    fn status(&self) -> zbus::Result<String>;
    /// → `[ListView]`
    fn lists(&self) -> zbus::Result<String>;
    /// `Query` → `[TaskView]`
    fn tasks(&self, query: &str) -> zbus::Result<String>;
    /// `BlockRange` → `[Block]`: tracked (and later planned) time.
    fn blocks(&self, range: &str) -> zbus::Result<String>;
    /// Pull one local day from ActivityWatch into `blocks` → `BlockRange` written.
    fn track_ingest(&self, date: &str) -> zbus::Result<String>;
    /// id → `TaskView`
    fn get(&self, id: &str) -> zbus::Result<String>;
    /// `AddSpec` → `Added`
    fn add(&self, spec: &str) -> zbus::Result<String>;
    /// quick-add text → `quickadd::Parsed`
    fn parse(&self, text: &str) -> zbus::Result<String>;
    /// id, `Change` → `TaskView`
    fn edit(&self, id: &str, change: &str) -> zbus::Result<String>;
    /// id → `TaskView` (a repeating task comes back open, moved on)
    fn complete(&self, id: &str) -> zbus::Result<String>;
    fn reopen(&self, id: &str) -> zbus::Result<String>;
    /// id, an RFC 3339 instant: the task's reminder rings again then. In
    /// the future, within a year, and the task open.
    fn snooze(&self, id: &str, until: &str) -> zbus::Result<()>;
    fn delete(&self, id: &str) -> zbus::Result<()>;
    /// id → the iCalendar object as asst would send it
    fn ics(&self, id: &str) -> zbus::Result<String>;
    /// Sync now and wait for it → `sync::Report`
    fn sync(&self) -> zbus::Result<String>;
    /// server → the URL to open; `LoginDone` follows.
    fn login(&self, server: &str) -> zbus::Result<String>;
    /// With an app password made by hand → `StatusView`
    fn login_password(&self, server: &str, username: &str, password: &str) -> zbus::Result<String>;
    fn logout(&self) -> zbus::Result<()>;
    /// id → `TaskView`: a copy with a new UID in the same list, every
    /// property kept.
    fn duplicate(&self, id: &str) -> zbus::Result<String>;
    /// `ListSpec` → `ListView`. Needs the server.
    fn create_list(&self, spec: &str) -> zbus::Result<String>;
    /// href, `ListChange` → `ListView`. Needs the server.
    fn update_list(&self, href: &str, change: &str) -> zbus::Result<String>;
    /// href. Needs the server, which keeps the list in its trash bin.
    fn delete_list(&self, href: &str) -> zbus::Result<()>;
    /// A list's href, or `""` for every list → how many completed tasks went.
    fn delete_completed(&self, list: &str) -> zbus::Result<u32>;
    /// list, `owner/repo` → `LinkView`: the list and the repo's issues
    /// are synced both ways.
    fn link(&self, list: &str, repo: &str) -> zbus::Result<String>;
    /// `owner/repo` → whether there was a link to drop.
    fn unlink(&self, repo: &str) -> zbus::Result<bool>;
    /// → `[LinkView]`
    fn links(&self) -> zbus::Result<String>;
    /// `owner/repo` (any case; `""` for every repo), how many rows at most
    /// (the log keeps 500) → `[store::LogRow]`, newest first: what asstd
    /// changed or decided about a link by itself, and why.
    fn link_log(&self, repo: &str, limit: u32) -> zbus::Result<String>;
    /// → `Settings`
    fn settings(&self) -> zbus::Result<String>;
    /// `SettingsChange` → `Settings`, saved to `config.toml`.
    fn set_settings(&self, change: &str) -> zbus::Result<String>;

    /// Tasks or lists changed.
    #[zbus(signal)]
    fn changed(&self) -> zbus::Result<()>;
    /// `StatusView`
    #[zbus(signal)]
    fn status_changed(&self, status: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn login_done(&self, ok: bool, message: &str) -> zbus::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncState {
    NoAccount,
    Idle,
    Syncing,
    Offline,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusView {
    pub state: SyncState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,
    /// Local changes not yet on the server.
    pub pending: usize,
    pub zone: String,
    /// Why location reminders cannot ring on this machine, once asstd has
    /// found out (no position source, GeoClue missing); gone again when a
    /// position arrives or no task has a location reminder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListView {
    pub href: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub writable: bool,
    /// Where tasks go when no list is named.
    pub inbox: bool,
    pub open: usize,
    /// Completed tasks, as far back as the server keeps them.
    #[serde(default)]
    pub done: usize,
}

/// Why a link is worth a look beyond whether its last pass worked. A link
/// with none of these is fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkState {
    /// GitHub says the repo is archived or disabled.
    Archived,
    /// The repo was renamed and the list has not followed.
    Renamed,
    /// GitHub cannot find the repo: deleted, or the token cannot see it.
    Unreachable,
    /// The name belongs to another repo than the one that was linked.
    Replaced,
    /// Two links claim one repo.
    Conflict,
}

impl LinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkState::Archived => "archived",
            LinkState::Renamed => "renamed",
            LinkState::Unreachable => "unreachable",
            LinkState::Replaced => "replaced",
            LinkState::Conflict => "conflict",
        }
    }

    /// The link's issues are not read while this holds, so nothing syncs.
    pub fn stalls(self) -> bool {
        matches!(
            self,
            LinkState::Unreachable | LinkState::Replaced | LinkState::Conflict
        )
    }
}

/// A repo rename the repo's list has not followed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRename {
    /// The repo's name before the rename.
    pub from: String,
    /// When asstd saw the rename.
    pub since: DateTime<Utc>,
}

/// A GitHub repo whose issues are synced with a list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkView {
    /// `owner/repo`.
    pub repo: String,
    /// The list's href.
    pub list: String,
    pub name: String,
    /// When asstd last read the repo's issues without trouble. Not kept
    /// across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced: Option<DateTime<Utc>>,
    /// Why the last pass failed, until one succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// GitHub's id for the repo, which a rename leaves alone; learned on
    /// the first look at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<u64>,
    /// Absent when the link is fine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<LinkState>,
    /// The state in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the state began, if asst knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// The repo was renamed and its list has yet to follow. Kept apart from
    /// `state`, which names only the most pressing thing about the link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rename: Option<PendingRename>,
}

/// The GitHub issue a task is paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueLink {
    pub number: u32,
    pub url: String,
}

/// Half-open range in epoch milliseconds (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRange {
    pub start: i64,
    pub end: i64,
}

/// What `track_ingest` wrote for one day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackIngested {
    pub date: String,
    pub blocks: usize,
    pub focused_seconds: f64,
}

/// A tracked or planned span, as the window draws it.
pub type BlockView = Block;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskView {
    /// Shortest unambiguous UID prefix (at least 4 characters).
    pub id: String,
    pub href: String,
    pub list: String,
    pub list_name: String,
    pub pending: bool,
    /// The paired GitHub issue, when the list is linked to a repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueLink>,
    /// Where the task came from: `steno`, `github`, `asst`, `phone`
    /// (another CalDAV client, no stamp), or `other` (a stamp of a kind
    /// not listed). Derived from `source` and the issue pair; not stored
    /// twice.
    pub origin: String,
    #[serde(flatten)]
    pub task: Task,
}

impl TaskView {
    pub fn new(row: Row, id: String, list_name: String) -> TaskView {
        let origin = origin_of(row.task.source.as_deref(), false).to_string();
        TaskView {
            id,
            href: row.href,
            list: row.list,
            list_name,
            pending: row.pending,
            issue: None,
            origin,
            task: row.task,
        }
    }

    /// Refresh `origin` after the issue pair is filled in.
    pub fn set_issue(&mut self, issue: Option<IssueLink>) {
        self.origin = origin_of(self.task.source.as_deref(), issue.is_some()).to_string();
        self.issue = issue;
    }
}

/// `steno` / `github` / `asst` / `phone` / `other` — creation site, not the
/// last editor. `source` wins when present; a GitHub pair is the fallback
/// for issues that arrived without a stamp.
pub fn origin_of(source: Option<&str>, has_issue: bool) -> &'static str {
    if let Some(kind) = source.and_then(|s| s.split(':').next()) {
        return match kind {
            "steno" => "steno",
            "asst" => "asst",
            "gh" | "github" => "github",
            _ => "other",
        };
    }
    if has_issue {
        return "github";
    }
    "phone"
}

/// Stamp for a task created in asst itself. Unique, so two native adds are
/// two tasks, while `--source` from a tool stays idempotent.
pub fn local_source() -> String {
    format!("asst:{}", uuid::Uuid::new_v4().simple())
}

/// A new task. With `parse`, `text` is quick-add syntax; fields given
/// explicitly win over what it says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AddSpec {
    pub text: String,
    #[serde(default)]
    pub parse: bool,
    /// With `parse`: dates and repeats in the text stay part of the title.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_dates: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<When>,
    /// A date in words (`tomorrow 5pm`), read in the daemon's zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_text: Option<String>,
    /// A repeat in words (`every mon`, `daily`) or an RRULE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_text: Option<String>,
    /// p1 … p4
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarms: Option<Vec<Trigger>>,
    /// `kind:id`. Adding the same source twice returns the first task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Added {
    #[serde(flatten)]
    pub task: TaskView,
    /// The source was already there; nothing was created.
    pub existed: bool,
}

/// Changes to a task. Absent means unchanged; `null` clears.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Change {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub due: Option<Option<When>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_text: Option<String>,
    /// A repeat in words or an RRULE; `none` clears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub rrule: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarms: Option<Vec<Trigger>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_alarms: Option<Vec<crate::task::LocationAlarm>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub source: Option<Option<String>>,
    /// X-APPLE-SORT-ORDER, the manual order within a list (ascending).
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub sort_order: Option<Option<i64>>,
}

/// A new list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSpec {
    pub name: String,
    /// `#rrggbb`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Changes to a list. Absent means unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `#rrggbb`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// What clients may read and change of `config.toml` (the account is
/// `login`'s business).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The list name tasks go to when none is named; unset means the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<String>,
    pub interval: u64,
    /// Minutes, one Snooze button each.
    #[serde(deserialize_with = "crate::config::snooze_lengths")]
    pub snooze: Vec<u64>,
    pub alarm_at_due: bool,
    /// Minutes before the due time that reminder rings.
    #[serde(default)]
    pub alarm_before: u32,
    /// Whether a list named for a repo is renamed when the repo is.
    #[serde(default)]
    pub follow_rename: FollowRename,
}

/// Absent means unchanged; `inbox: null` goes back to the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsChange {
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub inbox: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarm_at_due: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarm_before: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_rename: Option<FollowRename>,
}

fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

pub fn query(view: crate::store::View) -> Query {
    Query {
        view,
        list: None,
        text: None,
        limit: None,
    }
}

/// Show a task in the window, over D-Bus: the window's app ID answers
/// `org.freedesktop.Application.ActivateAction("open-task", href)`, and the
/// bus starts the window first when it isn't running (its service file).
/// A notification server's activation token lets it take focus.
pub async fn open_in_window(
    conn: &zbus::Connection,
    href: &str,
    token: Option<String>,
) -> zbus::Result<()> {
    activate_in_window(conn, "open-task", href, token).await
}

/// The window's chooser for when to ring a task's reminder again
/// (`snooze-task`), started the same way. The caller snoozes the task
/// first, so closing the chooser leaves the reminder ringing again.
pub async fn snooze_in_window(
    conn: &zbus::Connection,
    href: &str,
    token: Option<String>,
) -> zbus::Result<()> {
    activate_in_window(conn, "snooze-task", href, token).await
}

async fn activate_in_window(
    conn: &zbus::Connection,
    action: &str,
    href: &str,
    token: Option<String>,
) -> zbus::Result<()> {
    use zbus::zvariant::Value;
    let app = crate::config::gui_app_id();
    let path = format!("/{}", app.replace('.', "/"));
    let mut platform: std::collections::HashMap<&str, Value<'_>> = std::collections::HashMap::new();
    if let Some(t) = token {
        platform.insert("activation-token", Value::from(t));
    }
    conn.call_method(
        Some(app),
        path.as_str(),
        Some("org.freedesktop.Application"),
        "ActivateAction",
        &(action, vec![Value::from(href)], platform),
    )
    .await
    .map(|_| ())
}

/// Shortest prefixes (≥ 4 chars) that tell these UIDs apart, case-insensitively.
pub fn short_ids(uids: &[String]) -> std::collections::HashMap<String, String> {
    let mut lower: Vec<(String, &String)> = uids.iter().map(|u| (u.to_lowercase(), u)).collect();
    lower.sort();
    lower.dedup_by(|a, b| a.0 == b.0);
    let common = |a: &str, b: &str| a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let mut out = std::collections::HashMap::new();
    for i in 0..lower.len() {
        let prev = if i > 0 {
            common(&lower[i].0, &lower[i - 1].0)
        } else {
            0
        };
        let next = lower.get(i + 1).map_or(0, |n| common(&lower[i].0, &n.0));
        // A UID that is a prefix of another is still found by exact match.
        let len = (prev.max(next) + 1).max(4);
        out.insert(lower[i].1.clone(), lower[i].0.chars().take(len).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_that_is_fine_looks_as_it_did_before_the_state_fields() {
        // What an older asstd sends, and an older client reads
        let old = r#"{"repo":"jaehho/lab","list":"/cal/lab/","name":"lab"}"#;
        let l: LinkView = serde_json::from_str(old).unwrap();
        assert_eq!(
            (l.state, l.note, l.since, l.repo_id, l.pending_rename),
            (None, None, None, None, None)
        );
        let fine = LinkView {
            repo: "jaehho/lab".into(),
            list: "/cal/lab/".into(),
            name: "lab".into(),
            ..LinkView::default()
        };
        assert_eq!(serde_json::to_string(&fine).unwrap(), old);

        // A link with something to say names its state in words
        let archived = LinkView {
            repo_id: Some(1379104203),
            state: Some(LinkState::Archived),
            note: Some("archived on GitHub, first seen 2026-10-18; no open tasks here".into()),
            ..fine
        };
        let json = serde_json::to_value(&archived).unwrap();
        assert_eq!(json["state"], "archived");
        assert_eq!(json["repo_id"], 1379104203);
        assert_eq!(serde_json::from_value::<LinkView>(json).unwrap(), archived);

        // A rename the list has yet to follow is told apart from the state
        let waiting = LinkView {
            pending_rename: Some(PendingRename {
                from: "jaehho/champalimaud".into(),
                since: DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
            }),
            ..archived
        };
        let json = serde_json::to_value(&waiting).unwrap();
        assert_eq!(json["pending_rename"]["from"], "jaehho/champalimaud");
        assert_eq!(json["state"], "archived");
        assert_eq!(serde_json::from_value::<LinkView>(json).unwrap(), waiting);
        for (state, word) in [
            (LinkState::Archived, "archived"),
            (LinkState::Renamed, "renamed"),
            (LinkState::Unreachable, "unreachable"),
            (LinkState::Replaced, "replaced"),
            (LinkState::Conflict, "conflict"),
        ] {
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{word}\"")
            );
            assert_eq!(state.as_str(), word);
        }
    }

    #[test]
    fn following_a_rename_is_a_setting_older_peers_do_not_know() {
        // An older asstd sends settings without it: the default
        let old = r#"{"interval":60,"snooze":[10],"alarm_at_due":true}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.follow_rename, FollowRename::List);
        // A change that leaves it alone does not mention it
        let change = SettingsChange {
            interval: Some(300),
            ..SettingsChange::default()
        };
        assert_eq!(
            serde_json::to_string(&change).unwrap(),
            r#"{"interval":300}"#
        );
        let off = SettingsChange {
            follow_rename: Some(FollowRename::Off),
            ..SettingsChange::default()
        };
        let json = serde_json::to_string(&off).unwrap();
        assert_eq!(json, r#"{"follow_rename":"off"}"#);
        assert_eq!(serde_json::from_str::<SettingsChange>(&json).unwrap(), off);
    }

    #[test]
    fn change_tells_absent_from_null() {
        let c: Change = serde_json::from_str(r#"{"due": null, "summary": "x"}"#).unwrap();
        assert_eq!(c.due, Some(None));
        assert_eq!(c.description, None);
        assert_eq!(c.summary.as_deref(), Some("x"));
    }

    #[test]
    fn short_ids_are_unique_prefixes() {
        let uids: Vec<String> = ["0F276A13-FBF3", "0f27bb00-1111", "abcdef", "ab"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ids = short_ids(&uids);
        assert_eq!(ids["0F276A13-FBF3"], "0f276");
        assert_eq!(ids["0f27bb00-1111"], "0f27b");
        assert_eq!(ids["abcdef"], "abcd");
        assert_eq!(ids["ab"], "ab");
    }

    #[test]
    fn origin_prefers_the_source_kind() {
        assert_eq!(origin_of(Some("steno:2026/send"), false), "steno");
        assert_eq!(origin_of(Some("asst:abc123"), true), "asst");
        assert_eq!(origin_of(Some("gh:7"), false), "github");
        assert_eq!(origin_of(Some("mail:1"), false), "other");
    }

    #[test]
    fn origin_falls_back_to_issue_then_phone() {
        assert_eq!(origin_of(None, true), "github");
        assert_eq!(origin_of(None, false), "phone");
    }

    #[test]
    fn local_sources_are_unique_stamps() {
        assert_ne!(local_source(), local_source());
        assert!(local_source().starts_with("asst:"));
    }
}
