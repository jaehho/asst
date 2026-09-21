//! Alarms become desktop notifications with Complete and Snooze buttons; a
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
    // "default" is the click on the notification itself, not a button.
    let snoozes: Vec<(String, String)> = daemon
        .config()
        .snooze
        .iter()
        .map(|m| {
            (
                format!("snooze:{m}"),
                format!("Snooze {}", fmt::minutes(*m)),
            )
        })
        .collect();
    let mut actions = vec!["default", "Open", "done", "Complete"];
    for (key, label) in &snoozes {
        actions.extend([key.as_str(), label.as_str()]);
    }
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
                let href = shown.lock().unwrap_or_else(|p| p.into_inner()).get(&args.id).cloned();
                let Some(href) = href else { continue };
                let token = token.take().filter(|(id, _)| *id == args.id).map(|(_, t)| t);
                let snooze = |minutes: u64| daemon.snooze(&href, minutes);
                let result = match args.action_key {
                    "default" => {
                        let conn = proxy.inner().connection();
                        if let Err(e) = asst_core::api::open_in_window(conn, &href, token).await {
                            log::info!("opening a task from its notification: {e}");
                        }
                        Ok(())
                    }
                    "done" => daemon.complete(&href).map(|_| ()),
                    // A notification from before there were several.
                    "snooze" => snooze(daemon.config().snooze.first().copied().unwrap_or(10)),
                    key => match key.strip_prefix("snooze:").and_then(|m| m.parse().ok()) {
                        Some(minutes) => snooze(minutes),
                        None => Ok(()),
                    },
                };
                if let Err(e) = result {
                    log::warn!("{} from a notification: {e}", args.action_key);
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
