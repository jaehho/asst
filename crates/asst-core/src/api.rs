//! The D-Bus interface between `asstd` and its clients. Arguments and
//! results are JSON strings of the types below: easy to evolve, and readable
//! with `busctl --user call dev.jaeho.Asst.Daemon /dev/jaeho/Asst dev.jaeho.Asst1 Status`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::store::{Query, Row};
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
    /// id, a title (`""`: the task's) → `NewNote`: a note in the notes
    /// folder, linked to the task.
    fn new_note(&self, id: &str, title: &str) -> zbus::Result<String>;
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

/// A GitHub repo whose issues are synced with a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkView {
    /// `owner/repo`.
    pub repo: String,
    /// The list's href.
    pub list: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskView {
    /// Shortest unambiguous UID prefix (at least 4 characters).
    pub id: String,
    pub href: String,
    pub list: String,
    pub list_name: String,
    pub pending: bool,
    #[serde(flatten)]
    pub task: Task,
}

impl TaskView {
    pub fn new(row: Row, id: String, list_name: String) -> TaskView {
        TaskView {
            id,
            href: row.href,
            list: row.list,
            list_name,
            pending: row.pending,
            task: row.task,
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `kind:id`. Adding the same source twice returns the first task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Notes to link: paths in the notes folder, or full paths of files in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_notes: Vec<String>,
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
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Option<Option<String>>,
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
    /// Exactly these linked notes, as `AddSpec` takes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_notes: Option<Vec<String>>,
}

/// A note made for a task and linked to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewNote {
    /// Within the notes folder.
    pub path: String,
    /// The file on this disk.
    pub file: String,
    pub task: TaskView,
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
    /// The notes folder tasks link notes in, as a full path.
    #[serde(default)]
    pub notes: String,
}

/// Absent means unchanged; `inbox: null` and `notes: null` go back to the
/// default.
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
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub notes: Option<Option<String>>,
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
        linked_note: None,
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
        &("open-task", vec![Value::from(href)], platform),
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
}
