//! Alarms become desktop notifications with Complete and Snooze buttons,
//! and a Snooze… that snoozes the first length, then opens the window's
//! chooser for any other time; a click on one opens its task in the window.
//!
//! Timers sleep at most a minute and re-read the wall clock, so an alarm
//! that came due while the laptop was suspended fires on wake (monotonic
//! sleeps stop during suspend).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asst_core::config::first_snooze;
use asst_core::fmt;
use asst_core::store::Row;
use asst_core::task::{Task, priority_level};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use futures_util::StreamExt;
use zbus::zvariant::Value;

use crate::daemon::Daemon;

/// An alarm missed by more than this (laptop off) is not worth a notification.
const GRACE: chrono::Duration = chrono::Duration::hours(12);

/// How much of a task's notes a notification carries.
const NOTE_CHARS: usize = 120;

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

/// A reminder to show now.
struct Ring {
    row: Row,
    /// When it was due.
    at: DateTime<Utc>,
    /// A snooze running out, not one of the task's own alarms.
    snoozed: bool,
}

type Due = Vec<Ring>;

/// What the notification server has refused since it last took a
/// reminder, so an outage is said once and not on every pass.
#[derive(Default)]
struct Outage(HashSet<String>);

impl Outage {
    /// Whether this failure is news: its kind is new since the last success.
    fn failed(&mut self, e: &zbus::Error) -> bool {
        self.0.insert(match e {
            zbus::Error::MethodError(name, ..) => name.to_string(),
            other => other.to_string(),
        })
    }

    /// Whether the server was refusing until now.
    fn over(&mut self) -> bool {
        let was = !self.0.is_empty();
        self.0.clear();
        was
    }
}

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
    let started = Utc::now();
    let mut outage = Outage::default();
    loop {
        let now = Utc::now();
        let (due, next) = collect(&daemon, now);
        let notify = |row: Row, replaces| {
            let (proxy, daemon) = (&proxy, &daemon);
            async move { show(proxy, daemon, &row, replaces).await }
        };
        ring(&daemon, due, &shown, &mut outage, started, notify).await;
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

/// Show what is due. A reminder counts as fired only once the
/// notification server has taken it. With none answering (swaync
/// restarting, a login before it is up) it stays due and the next pass
/// tries again, until GRACE has gone by; passes are at most a minute
/// apart, so an outage costs one failed call per reminder a minute.
async fn ring<F>(
    daemon: &Daemon,
    due: Due,
    shown: &Shown,
    outage: &mut Outage,
    started: DateTime<Utc>,
    show: impl Fn(Row, u32) -> F,
) where
    F: Future<Output = zbus::Result<u32>>,
{
    for Ring { row, at, snoozed } in due {
        // One notification per task: a new one replaces the last.
        let replaces = shown
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(_, h)| **h == row.href)
            .map(|(id, _)| *id);
        match show(row.clone(), replaces.unwrap_or(0)).await {
            Ok(id) => {
                if outage.over() {
                    log::info!("the notification server is back; reminders show again");
                }
                {
                    let store = daemon.store();
                    if let Err(e) = store.mark_fired(&row.href, at) {
                        log::warn!("recording a reminder: {e}");
                    }
                    if snoozed {
                        let _ = store.unsnooze_at(&row.href, at);
                    }
                }
                let mut s = shown.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(old) = replaces {
                    s.remove(&old);
                }
                s.insert(id, row.href.clone());
            }
            Err(e) => {
                if outage.failed(&e) {
                    log::warn!("showing a reminder, will try again: {e}");
                }
                // An alarm falls out of `collect` after GRACE. A snooze
                // has no such limit when it rings late (a laptop that was
                // off for days), so a failing one is dropped GRACE after
                // it was due or asstd started, whichever is later.
                if snoozed && Utc::now() - at.max(started) > GRACE {
                    log::warn!("dropping a snoozed reminder that would not show");
                    let _ = daemon.store().unsnooze_at(&row.href, at);
                }
            }
        }
    }
}

/// Alarms due now, and when the next one is.
fn collect(daemon: &Daemon, now: DateTime<Utc>) -> (Due, Option<DateTime<Utc>>) {
    let store = daemon.store();
    let mut due: HashMap<String, Ring> = HashMap::new();
    let mut next: Option<DateTime<Utc>> = None;
    let mut later = |at: DateTime<Utc>| next = Some(next.map_or(at, |n| n.min(at)));
    for row in store.with_alarms().unwrap_or_default() {
        for at in row.task.unacknowledged_alarms(daemon.zone) {
            if at > now {
                later(at);
            } else if now - at <= GRACE && !store.fired(&row.href, at).unwrap_or(true) {
                let keep = due.get(&row.href).is_none_or(|prev| prev.at < at);
                if keep {
                    let ring = Ring {
                        row: row.clone(),
                        at,
                        snoozed: false,
                    };
                    due.insert(row.href.clone(), ring);
                }
            }
        }
    }
    for (href, until) in store.snoozed().unwrap_or_default() {
        if until > now {
            later(until);
            continue;
        }
        // The snooze stays until `ring` has shown it.
        match store.get(&href) {
            Ok(Some(row)) if row.task.is_open() => {
                due.insert(
                    href,
                    Ring {
                        row,
                        at: until,
                        snoozed: true,
                    },
                );
            }
            // Completed or gone meanwhile.
            Ok(_) => {
                let _ = store.unsnooze(&href);
            }
            // The store would not answer: try again next pass.
            Err(_) => {}
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
    let list = daemon
        .store()
        .lists()
        .ok()
        .and_then(|ls| ls.into_iter().find(|l| l.href == row.list));
    let body = body(&row.task, list.as_ref().map(|l| l.name.as_str()), now);
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
            &body,
            &actions,
            hints,
            -1,
        )
        .await
}

/// The lines under a notification's title: when it is due and in which
/// list, then the first line of its notes.
fn body(task: &Task, list: Option<&str>, now: DateTime<Tz>) -> String {
    let mut facts = Vec::new();
    if let Some(due) = &task.due {
        let when = fmt::due_label(due, now);
        // An alarm at the due time lands a few ms after it; that is "due", not late.
        let grace = now - chrono::Duration::minutes(1);
        facts.push(if fmt::is_overdue(due, grace) {
            format!("Overdue, due {when}")
        } else {
            // A reminder set ahead says how long is left: "in 30 min" tells
            // more than "today 12:08am". A date has no time to count to.
            let zone = now.timezone();
            let left = if due.has_time() {
                fmt::until_label(due.instant(zone).with_timezone(&zone), now)
            } else {
                None
            };
            format!("Due {}", left.unwrap_or(when))
        });
    }
    facts.extend(list.map(str::to_string));
    let mut body = facts.join(" · ");
    let first = task
        .description
        .as_deref()
        .and_then(|d| d.lines().map(str::trim).find(|l| !l.is_empty()));
    if let Some(line) = first {
        if !body.is_empty() {
            body.push('\n');
        }
        body.extend(line.chars().take(NOTE_CHARS));
        if line.chars().count() > NOTE_CHARS {
            body.push('…');
        }
    }
    body
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
        actions.push((format!("snooze:{m}:{uid}"), fmt::snooze_button(*m)));
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
    /// The window's chooser for when to ring it again. Closing it leaves
    /// the snooze before it, so the reminder is never lost.
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
///
/// `Snooze…` snoozes the first of the `snooze` lengths before it asks. It
/// closes the notification like any button, so a chooser that is closed
/// with Esc, or never gets to show, would otherwise leave a reminder that
/// never rings again.
fn plan_action<'a>(key: &'a str, snooze: &[u64]) -> Option<Plan<'a>> {
    if key == "default" {
        return Some(Plan {
            uid: None,
            steps: vec![Step::Open],
        });
    }
    // The UID is the rest of the key, colons and all.
    let (verb, rest) = key.split_once(':')?;
    let (steps, uid) = match verb {
        "done" => (vec![Step::Complete], rest),
        "snooze-more" => (
            vec![Step::Snooze(first_snooze(snooze)), Step::Chooser],
            rest,
        ),
        "snooze" => {
            let (minutes, uid) = rest.split_once(':')?;
            (vec![Step::Snooze(minutes.parse().ok()?)], uid)
        }
        _ => return None,
    };
    (!uid.is_empty()).then_some(Plan {
        uid: Some(uid),
        steps,
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
                let Some(plan) = plan_action(args.action_key, &daemon.config().snooze) else { continue };
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
    use std::cell::Cell;

    use asst_core::api::AddSpec;
    use asst_core::caldav::RemoteList;
    use asst_core::config::Config;
    use asst_core::store::Store;
    use asst_core::time::Trigger;
    use chrono::TimeZone;

    use super::*;

    /// A daemon with one open task that has these alarms, and its href.
    fn with_task(alarms: Vec<Trigger>) -> (Arc<Daemon>, String) {
        let zone: Tz = "America/New_York".parse().unwrap();
        let mut store = Store::in_memory(zone).unwrap();
        store
            .apply_lists(&[RemoteList {
                href: "/cal/work/".into(),
                name: "Work".into(),
                color: None,
                order: None,
                sync_token: Some("t1".into()),
                ctag: None,
                writable: true,
            }])
            .unwrap();
        let (daemon, _events) = Daemon::new(Config::default(), store, zone);
        let spec = AddSpec {
            text: "Call the bank".into(),
            list: Some("work".into()),
            alarms: Some(alarms),
            ..AddSpec::default()
        };
        let href = daemon.add(&spec).unwrap().task.href;
        (daemon, href)
    }

    /// One pass of `run` against a notification server that is up or not.
    /// Returns how many reminders were due.
    async fn pass(
        daemon: &Daemon,
        server_up: &Cell<bool>,
        shown: &Shown,
        outage: &mut Outage,
        started: DateTime<Utc>,
    ) -> usize {
        let (due, _) = collect(daemon, Utc::now());
        let count = due.len();
        let notify = |_row: Row, _replaces| async move {
            if server_up.get() {
                Ok(7)
            } else {
                Err(zbus::Error::Failure("no such name".into()))
            }
        };
        ring(daemon, due, shown, outage, started, notify).await;
        count
    }

    /// A whole second, as the store keeps it.
    fn ago(minutes: i64) -> DateTime<Utc> {
        let at = Utc::now() - chrono::Duration::minutes(minutes);
        DateTime::from_timestamp(at.timestamp(), 0).unwrap()
    }

    #[tokio::test]
    async fn an_alarm_the_server_will_not_take_rings_when_it_will() {
        let at = ago(5);
        let (daemon, href) = with_task(vec![Trigger::Absolute { at }]);
        let (up, shown, mut outage) = (Cell::new(false), Shown::default(), Outage::default());
        let started = ago(60);

        for _ in 0..3 {
            assert_eq!(pass(&daemon, &up, &shown, &mut outage, started).await, 1);
            assert!(!daemon.store().fired(&href, at).unwrap(), "still pending");
            assert!(shown.lock().unwrap().is_empty());
        }

        up.set(true);
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, started).await, 1);
        assert!(daemon.store().fired(&href, at).unwrap());
        assert_eq!(shown.lock().unwrap().get(&7), Some(&href));
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, started).await, 0);
    }

    #[tokio::test]
    async fn an_alarm_older_than_the_grace_is_not_retried() {
        let at = Utc::now() - GRACE - chrono::Duration::minutes(1);
        let (daemon, href) = with_task(vec![Trigger::Absolute { at }]);
        let (up, shown, mut outage) = (Cell::new(true), Shown::default(), Outage::default());
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, ago(60)).await, 0);
        assert!(!daemon.store().fired(&href, at).unwrap());
    }

    #[tokio::test]
    async fn a_snooze_the_server_will_not_take_waits_for_it_within_the_grace() {
        let (daemon, href) = with_task(vec![]);
        let (up, shown, mut outage) = (Cell::new(false), Shown::default(), Outage::default());
        let until = ago(1);
        let snoozed = || daemon.store().snoozed().unwrap();
        daemon.store().snooze(&href, until).unwrap();

        let started = ago(60);
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, started).await, 1);
        assert_eq!(snoozed(), [(href.clone(), until)]);

        up.set(true);
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, started).await, 1);
        assert!(snoozed().is_empty());
        assert!(daemon.store().fired(&href, until).unwrap());
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, started).await, 0);
    }

    #[tokio::test]
    async fn a_snooze_that_never_shows_is_dropped_after_the_grace() {
        let (daemon, href) = with_task(vec![]);
        let (up, shown, mut outage) = (Cell::new(false), Shown::default(), Outage::default());
        let snoozed = || daemon.store().snoozed().unwrap().len();
        let long_ago = Utc::now() - GRACE - chrono::Duration::hours(1);

        // Due while the laptop was off, and asstd has only just started:
        // one more GRACE of tries, not none.
        daemon.store().snooze(&href, long_ago).unwrap();
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, ago(1)).await, 1);
        assert_eq!(snoozed(), 1);

        // Failing since before GRACE ago.
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, long_ago).await, 1);
        assert_eq!(snoozed(), 0);

        // Late but within it, however long asstd has been failing.
        daemon.store().snooze(&href, ago(1)).unwrap();
        assert_eq!(pass(&daemon, &up, &shown, &mut outage, long_ago).await, 1);
        assert_eq!(snoozed(), 1);
    }

    #[test]
    fn an_outage_is_said_once_per_kind() {
        let down = zbus::Error::Failure("no such name".into());
        let other = zbus::Error::Failure("timed out".into());
        let mut outage = Outage::default();
        assert!(!outage.over());
        assert!(outage.failed(&down));
        assert!(!outage.failed(&down));
        assert!(outage.failed(&other));
        assert!(!outage.failed(&down));
        assert!(outage.over(), "the server took one");
        assert!(!outage.over());
        assert!(outage.failed(&down), "the next outage is said again");
    }

    /// 12:08am on Sunday, 18 October 2026.
    fn midnight_ish() -> DateTime<Tz> {
        let zone: Tz = "America/New_York".parse().unwrap();
        zone.with_ymd_and_hms(2026, 10, 18, 0, 8, 0).unwrap()
    }

    /// An open task due `minutes` after `midnight_ish()`, or on a date.
    fn due_task(due: Option<(i64, bool)>, notes: Option<&str>) -> Task {
        let at = midnight_ish();
        let due = due.map(|(minutes, timed)| {
            let at = at + chrono::Duration::minutes(minutes);
            if timed {
                serde_json::json!({
                    "kind": "zoned",
                    "at": at.naive_local().to_string().replace(' ', "T"),
                    "tzid": "America/New_York",
                })
            } else {
                serde_json::json!({ "kind": "date", "date": at.date_naive() })
            }
        });
        serde_json::from_value(serde_json::json!({
            "uid": "u1",
            "summary": "Call the bank",
            "status": "needs-action",
            "priority": 0,
            "due": due,
            "description": notes,
        }))
        .unwrap()
    }

    #[test]
    fn the_body_says_how_long_is_left_and_the_notes_first_line() {
        let now = midnight_ish();
        let body = |due, notes, list| body(&due_task(due, notes), list, now);

        // An alarm at the due time rings a moment after it: due, not late.
        assert_eq!(
            body(Some((0, true)), None, Some("Work")),
            "Due today 12:08am · Work"
        );
        // Ahead of it, how long is left; further than that, when.
        assert_eq!(body(Some((30, true)), None, None), "Due in 30 min");
        assert_eq!(body(Some((3 * 60, true)), None, None), "Due in 3 h");
        assert_eq!(body(Some((13 * 60, true)), None, None), "Due today 1:08pm");
        // A date has no time to count to, however near its midnight.
        assert_eq!(body(Some((60, false)), None, None), "Due today");
        // Late by more than the minute an alarm may be.
        assert_eq!(
            body(Some((-90, true)), None, None),
            "Overdue, due yesterday 10:38pm"
        );

        // Only the first line of the notes, under the rest.
        let notes = Some("\n  Ask about the fee \nthen the card");
        assert_eq!(
            body(Some((30, true)), notes, Some("Work")),
            "Due in 30 min · Work\nAsk about the fee"
        );
        assert_eq!(body(None, notes, None), "Ask about the fee");
        assert_eq!(body(None, Some(" \n "), None), "");
        let long = "x".repeat(NOTE_CHARS + 1);
        let cut = body(None, Some(&long), None);
        assert_eq!(cut.chars().count(), NOTE_CHARS + 1);
        assert!(cut.ends_with('…'));
        let fits = "x".repeat(NOTE_CHARS);
        assert_eq!(body(None, Some(&fits), None), fits);
    }

    fn plan(uid: Option<&str>, step: Step) -> Plan<'_> {
        Plan {
            uid,
            steps: vec![step],
        }
    }

    /// The default Snooze buttons, as a notification has them.
    const BUTTONS: [u64; 3] = asst_core::config::DEFAULT_SNOOZE;

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
            (
                &more,
                Some(Plan {
                    uid: Some(uid),
                    steps: vec![Step::Snooze(10), Step::Chooser],
                }),
            ),
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
            assert_eq!(plan_action(key, &BUTTONS), want, "{key:?}");
        }
    }

    #[test]
    fn snooze_more_snoozes_the_first_length_then_opens_the_chooser() {
        let key = "snooze-more:task-1";
        for (buttons, first) in [(vec![10, 30, 60], 10), (vec![15, 60], 15), (vec![60], 60)] {
            assert_eq!(
                plan_action(key, &buttons),
                Some(Plan {
                    uid: Some("task-1"),
                    steps: vec![Step::Snooze(first), Step::Chooser],
                }),
                "{buttons:?}"
            );
        }
        // The other buttons are one step, whatever the config says.
        assert_eq!(
            plan_action("snooze:30:task-1", &[15]),
            Some(plan(Some("task-1"), Step::Snooze(30)))
        );
    }

    #[test]
    fn snooze_more_with_no_config_uses_ten() {
        // `snooze = []` leaves a notification with only Snooze….
        assert_eq!(
            plan_action("snooze-more:task-1", &[]),
            Some(Plan {
                uid: Some("task-1"),
                steps: vec![Step::Snooze(10), Step::Chooser],
            })
        );
    }

    #[test]
    fn the_buttons_keep_their_order_and_every_key_reads_back() {
        let uid = "task-1";
        let actions = actions_for(uid, &BUTTONS);
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
            .map(|(key, _)| plan_action(key, &BUTTONS).expect(key))
            .collect();
        assert_eq!(
            plans,
            [
                plan(None, Step::Open),
                plan(Some(uid), Step::Complete),
                plan(Some(uid), Step::Snooze(10)),
                plan(Some(uid), Step::Snooze(30)),
                plan(Some(uid), Step::Snooze(60)),
                Plan {
                    uid: Some(uid),
                    steps: vec![Step::Snooze(10), Step::Chooser],
                },
            ]
        );
        // A server fires "default" for the click on the notification and
        // for nothing else, so that key stays bare.
        assert_eq!(actions[0].0, "default");
    }
}
