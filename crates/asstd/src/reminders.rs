//! Alarms become desktop notifications with Complete and Snooze buttons,
//! and a Snooze… that opens the window's chooser for any other time; a
//! click on one opens its task in the window.
//!
//! Timers sleep at most a minute and re-read the wall clock, so an alarm
//! that came due while the laptop was suspended fires on wake (monotonic
//! sleeps stop during suspend).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asst_core::fmt;
use asst_core::store::Row;
use asst_core::task::priority_level;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use zbus::zvariant::Value;

use crate::daemon::Daemon;

/// An alarm missed by more than this (laptop off) is not worth a notification.
const GRACE: chrono::Duration = chrono::Duration::hours(12);

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;

    /// Sent just before `ActionInvoked`: what lets the window take focus.
    #[zbus(signal)]
    fn activation_token(&self, id: u32, activation_token: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

type Shown = Arc<Mutex<HashMap<u32, String>>>;

/// Alarms due now, each with the instant it was due.
type Due = Vec<(Row, DateTime<Utc>)>;

pub async fn run(daemon: Arc<Daemon>, conn: zbus::Connection) {
    let proxy = match NotificationsProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            log::warn!("no notification service, reminders are off: {e}");
            return;
        }
    };
    let shown: Shown = Arc::default();
    tokio::spawn(buttons(daemon.clone(), proxy.clone(), shown.clone()));
    let _ = daemon
        .store()
        .prune_fired(Utc::now() - chrono::Duration::days(30));
    loop {
        let now = Utc::now();
        let (due, next) = collect(&daemon, now);
        for (row, at) in due {
            if let Err(e) = daemon.store().mark_fired(&row.href, at) {
                log::warn!("recording a reminder: {e}");
            }
            // One notification per task: a new one replaces the last.
            let replaces = shown
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .find(|(_, h)| **h == row.href)
                .map(|(id, _)| *id);
            match show(&proxy, &daemon, &row, replaces.unwrap_or(0)).await {
                Ok(id) => {
                    let mut s = shown.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(old) = replaces {
                        s.remove(&old);
                    }
                    s.insert(id, row.href.clone());
                }
                Err(e) => log::warn!("showing a reminder: {e}"),
            }
        }
        let wait = next
            .and_then(|n| (n - Utc::now()).to_std().ok())
            .unwrap_or(Duration::from_secs(60))
            .clamp(Duration::from_millis(200), Duration::from_secs(60));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = daemon.wake_reminders.notified() => {}
        }
    }
}

/// Alarms due now, and when the next one is.
fn collect(daemon: &Daemon, now: DateTime<Utc>) -> (Due, Option<DateTime<Utc>>) {
    let store = daemon.store();
    let mut due: HashMap<String, (Row, DateTime<Utc>)> = HashMap::new();
    let mut next: Option<DateTime<Utc>> = None;
    let mut later = |at: DateTime<Utc>| next = Some(next.map_or(at, |n| n.min(at)));
    for row in store.with_alarms().unwrap_or_default() {
        for at in row.task.unacknowledged_alarms(daemon.zone) {
            if at > now {
                later(at);
            } else if now - at <= GRACE && !store.fired(&row.href, at).unwrap_or(true) {
                let keep = due.get(&row.href).is_none_or(|(_, prev)| *prev < at);
                if keep {
                    due.insert(row.href.clone(), (row.clone(), at));
                }
            }
        }
    }
    for (href, until) in store.snoozed().unwrap_or_default() {
        if until > now {
            later(until);
            continue;
        }
        let _ = store.unsnooze(&href);
        if let Ok(Some(row)) = store.get(&href)
            && row.task.is_open()
        {
            due.insert(href, (row, until));
        }
    }
    (due.into_values().collect(), next)
}

async fn show(
    proxy: &NotificationsProxy<'_>,
    daemon: &Daemon,
    row: &Row,
    replaces: u32,
) -> zbus::Result<u32> {
    let now = Utc::now().with_timezone(&daemon.zone);
    let mut body = Vec::new();
    if let Some(due) = &row.task.due {
        let when = fmt::due_label(due, now);
        // An alarm at the due time lands a few ms after it; that is "due", not late.
        let grace = now - chrono::Duration::minutes(1);
        body.push(if fmt::is_overdue(due, grace) {
            format!("Overdue, due {when}")
        } else {
            format!("Due {when}")
        });
    }
    if let Some(list) = daemon
        .store()
        .lists()
        .ok()
        .and_then(|ls| ls.into_iter().find(|l| l.href == row.list))
    {
        body.push(list.name);
    }
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from(asst_core::config::APP_ID));
    if priority_level(row.task.priority) == 1 {
        hints.insert("urgency", Value::U8(2));
    }
    let buttons = actions_for(&row.task.uid, &daemon.config().snooze);
    let actions: Vec<&str> = buttons
        .iter()
        .flat_map(|(key, label)| [key.as_str(), label.as_str()])
        .collect();
    proxy
        .notify(
            "asst",
            replaces,
            asst_core::config::APP_ID,
            &row.task.summary,
            &body.join(" · "),
            &actions,
            hints,
            -1,
        )
        .await
}

/// A notification's actions as (key, label), in the order they show:
/// "default" (Open), Complete, a Snooze for each length, then the
/// chooser for any other time (#20).
///
/// Every key but "default" names its task, so a button still works after
/// asstd restarts and no longer knows which notification is which.
/// "default" cannot: a notification server fires it for a click on the
/// notification itself and knows no other name for it (swaync turns
/// anything else into a button, so `default:<uid>` would leave the click
/// doing nothing).
fn actions_for(uid: &str, snooze: &[u64]) -> Vec<(String, String)> {
    let mut actions = vec![
        ("default".to_string(), "Open".to_string()),
        (format!("done:{uid}"), "Complete".to_string()),
    ];
    for m in snooze {
        actions.push((
            format!("snooze:{m}:{uid}"),
            format!("Snooze {}", fmt::minutes(*m)),
        ));
    }
    actions.push((format!("snooze-more:{uid}"), "Snooze…".to_string()));
    actions
}

/// One thing a button does to its task.
#[derive(Debug, PartialEq)]
enum Step {
    /// Show it in the window.
    Open,
    Complete,
    Snooze(u64),
    /// The window's chooser for when to ring it again.
    Chooser,
}

/// What an action key asks for.
#[derive(Debug, PartialEq)]
struct Plan<'a> {
    /// The task the key names; none for "default", a click on the
    /// notification itself.
    uid: Option<&'a str>,
    steps: Vec<Step>,
}

/// The plan for an action key, or `None` for a key that is not ours:
/// another app's, or one made without a UID by an asstd from before this,
/// which no running asstd could map to a task anyway.
fn plan_action(key: &str) -> Option<Plan<'_>> {
    if key == "default" {
        return Some(Plan {
            uid: None,
            steps: vec![Step::Open],
        });
    }
    // The UID is the rest of the key, colons and all.
    let (verb, rest) = key.split_once(':')?;
    let (step, uid) = match verb {
        "done" => (Step::Complete, rest),
        "snooze-more" => (Step::Chooser, rest),
        "snooze" => {
            let (minutes, uid) = rest.split_once(':')?;
            (Step::Snooze(minutes.parse().ok()?), uid)
        }
        _ => return None,
    };
    (!uid.is_empty()).then(|| Plan {
        uid: Some(uid),
        steps: vec![step],
    })
}

/// The href of the task with exactly this UID. `Store::find` also takes
/// a prefix, so a deleted task's button could reach another task whose
/// UID starts the same.
fn href_of(daemon: &Daemon, uid: &str) -> Option<String> {
    let row = daemon.store().find(uid).ok()?;
    row.task.uid.eq_ignore_ascii_case(uid).then_some(row.href)
}

async fn buttons(daemon: Arc<Daemon>, proxy: NotificationsProxy<'static>, shown: Shown) {
    let (Ok(mut actions), Ok(mut tokens), Ok(mut closed)) = (
        proxy.receive_action_invoked().await,
        proxy.receive_activation_token().await,
        proxy.receive_notification_closed().await,
    ) else {
        log::warn!("cannot listen for notification buttons");
        return;
    };
    let mut token: Option<(u32, String)> = None;
    loop {
        tokio::select! {
            Some(signal) = tokens.next() => {
                if let Ok(args) = signal.args() {
                    token = Some((args.id, args.activation_token.to_string()));
                }
            }
            Some(signal) = actions.next() => {
                let Ok(args) = signal.args() else { continue };
                let Some(plan) = plan_action(args.action_key) else { continue };
                let href = match plan.uid {
                    Some(uid) => href_of(&daemon, uid),
                    None => shown.lock().unwrap_or_else(|p| p.into_inner()).get(&args.id).cloned(),
                };
                let Some(href) = href else {
                    // A click on a notification we cannot match to a task:
                    // another app's, or ours from before a restart. A
                    // button for a task that is gone closes its
                    // notification rather than sit there doing nothing.
                    if plan.uid.is_some() {
                        log::info!("{} from a notification: no such task", args.action_key);
                        let _ = proxy.close_notification(args.id).await;
                    }
                    continue;
                };
                let mut token = token.take().filter(|(id, _)| *id == args.id).map(|(_, t)| t);
                for step in plan.steps {
                    let conn = proxy.inner().connection();
                    match step {
                        Step::Open => {
                            if let Err(e) = asst_core::api::open_in_window(conn, &href, token.take()).await {
                                log::info!("opening a task from its notification: {e}");
                            }
                        }
                        Step::Chooser => {
                            if let Err(e) = asst_core::api::snooze_in_window(conn, &href, token.take()).await {
                                log::info!("opening the snooze chooser from a notification: {e}");
                            }
                        }
                        Step::Complete => {
                            if let Err(e) = daemon.complete(&href) {
                                log::warn!("{} from a notification: {e}", args.action_key);
                            }
                        }
                        Step::Snooze(minutes) => {
                            if let Err(e) = daemon.snooze(&href, minutes) {
                                log::warn!("{} from a notification: {e}", args.action_key);
                            }
                        }
                    }
                }
                let _ = proxy.close_notification(args.id).await;
            }
            Some(signal) = closed.next() => {
                if let Ok(args) = signal.args() {
                    shown.lock().unwrap_or_else(|p| p.into_inner()).remove(&args.id);
                }
            }
            else => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(uid: Option<&str>, step: Step) -> Plan<'_> {
        Plan {
            uid,
            steps: vec![step],
        }
    }

    #[test]
    fn a_key_names_its_task_and_what_to_do() {
        let uid = "5E9C0D4A-7B2F-4F6E-9C1A-0A3B4C5D6E7F";
        let done = format!("done:{uid}");
        let snooze = format!("snooze:10:{uid}");
        let day = format!("snooze:1440:{uid}");
        let more = format!("snooze-more:{uid}");
        for (key, want) in [
            ("default", Some(plan(None, Step::Open))),
            (&done, Some(plan(Some(uid), Step::Complete))),
            (&snooze, Some(plan(Some(uid), Step::Snooze(10)))),
            (&day, Some(plan(Some(uid), Step::Snooze(1440)))),
            (&more, Some(plan(Some(uid), Step::Chooser))),
            // A UID from another client can hold colons.
            ("done:a:b:c", Some(plan(Some("a:b:c"), Step::Complete))),
            ("snooze:5:a:b", Some(plan(Some("a:b"), Step::Snooze(5)))),
            // Keys made before they named a task, and keys not ours.
            ("done", None),
            ("snooze", None),
            ("snooze:10", None),
            ("snooze-more", None),
            ("default:abc", None),
            ("done:", None),
            ("snooze-more:", None),
            ("snooze:10:", None),
            ("snooze:ten:abc", None),
            ("snooze:-5:abc", None),
            ("snooze::abc", None),
            ("reply:abc", None),
            ("", None),
        ] {
            assert_eq!(plan_action(key), want, "{key:?}");
        }
    }

    #[test]
    fn the_buttons_keep_their_order_and_every_key_reads_back() {
        let uid = "task-1";
        let actions = actions_for(uid, &[10, 30, 60]);
        let labels: Vec<_> = actions.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Open",
                "Complete",
                "Snooze 10 min",
                "Snooze 30 min",
                "Snooze 1 h",
                "Snooze…"
            ]
        );
        let plans: Vec<_> = actions
            .iter()
            .map(|(key, _)| plan_action(key).expect(key))
            .collect();
        assert_eq!(
            plans,
            [
                plan(None, Step::Open),
                plan(Some(uid), Step::Complete),
                plan(Some(uid), Step::Snooze(10)),
                plan(Some(uid), Step::Snooze(30)),
                plan(Some(uid), Step::Snooze(60)),
                plan(Some(uid), Step::Chooser),
            ]
        );
        // A server fires "default" for the click on the notification and
        // for nothing else, so that key stays bare.
        assert_eq!(actions[0].0, "default");
    }
}
