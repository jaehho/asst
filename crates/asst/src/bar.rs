//! `asst bar`: a waybar custom module (`"return-type": "json"`). One line on
//! start, on every change, and each minute as due times pass.

use std::time::Duration;

use asst_core::api::{AsstProxy, StatusView, SyncState, TaskView, query};
use asst_core::fmt;
use asst_core::store::View;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use futures_util::StreamExt;
use serde::Serialize;

#[derive(Serialize)]
struct Line {
    text: String,
    alt: &'static str,
    class: &'static str,
    tooltip: String,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn print(line: &Line) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let text = serde_json::to_string(line).expect("line serializes");
    // Waybar closed the pipe (reload, exit): nothing left to do.
    if writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
        std::process::exit(0);
    }
}

async fn render(proxy: &AsstProxy<'_>) -> anyhow::Result<Line> {
    let tasks: Vec<TaskView> = serde_json::from_str(
        &proxy
            .tasks(&serde_json::to_string(&query(View::Today))?)
            .await?,
    )?;
    let status: StatusView = serde_json::from_str(&proxy.status().await?)?;
    let fallback = asst_core::time::local_zone();
    Ok(line(&tasks, &status, Utc::now(), fallback))
}

/// The module's line for Today's `tasks`. Overdue and the labels are read
/// in the daemon's zone (`status.zone`), the one its Today was worked out
/// in, so all three move together when the machine does (#34);
/// `fallback` stands in for a zone name this build cannot read.
fn line(tasks: &[TaskView], status: &StatusView, now: DateTime<Utc>, fallback: Tz) -> Line {
    let zone = status.zone.parse().unwrap_or(fallback);
    let now = now.with_timezone(&zone);
    let overdue = tasks
        .iter()
        .filter(|t| t.task.due.as_ref().is_some_and(|d| fmt::is_overdue(d, now)))
        .count();
    let class = match status.state {
        SyncState::NoAccount | SyncState::Error => "error",
        _ if overdue > 0 => "overdue",
        _ if !tasks.is_empty() => "due",
        _ => "clear",
    };
    let mut tooltip: Vec<String> = tasks
        .iter()
        .take(12)
        .map(|t| {
            let due = t
                .task
                .due
                .as_ref()
                .map(|d| format!("  <i>{}</i>", escape(&fmt::due_label(d, now))))
                .unwrap_or_default();
            format!("{}{due}", escape(&t.task.summary))
        })
        .collect();
    if tasks.len() > 12 {
        tooltip.push(format!("and {} more", tasks.len() - 12));
    }
    if tooltip.is_empty() {
        tooltip.push("Nothing due today".into());
    }
    match (status.state, &status.message) {
        (SyncState::Offline, _) => tooltip.push(format!(
            "\n<small>offline, {} unsent</small>",
            status.pending
        )),
        (SyncState::Error | SyncState::NoAccount, Some(m)) => {
            tooltip.push(format!("\n<small>{}</small>", escape(m)))
        }
        _ => {}
    }
    Line {
        // Empty text hides the module when nothing is due.
        text: if tasks.is_empty() {
            String::new()
        } else {
            tasks.len().to_string()
        },
        alt: class,
        class,
        tooltip: tooltip.join("\n"),
    }
}

pub async fn run() -> anyhow::Result<()> {
    loop {
        if let Err(e) = follow().await {
            print(&Line {
                text: String::new(),
                alt: "offline",
                class: "offline",
                tooltip: format!("asst: {}", escape(&e.to_string())),
            });
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

async fn follow() -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = AsstProxy::new(&conn).await?;
    let mut changed = proxy.receive_changed().await?;
    let mut status = proxy.receive_status_changed().await?;
    let mut owner = proxy.inner().receive_owner_changed().await?;
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        print(&render(&proxy).await?);
        tokio::select! {
            Some(_) = changed.next() => {}
            Some(_) = status.next() => {}
            Some(_) = owner.next() => {}
            _ = tick.tick() => {}
        }
        // Coalesce a burst of signals.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(zone: &str) -> StatusView {
        StatusView {
            state: SyncState::Idle,
            message: None,
            server: None,
            username: None,
            last_sync: None,
            pending: 0,
            zone: zone.into(),
            location: None,
        }
    }

    fn nine_am_floating() -> TaskView {
        serde_json::from_value(serde_json::json!({
            "id": "a1b2",
            "href": "/cal/work/a1b2.ics",
            "list": "/cal/work/",
            "list_name": "Work",
            "pending": false,
            "origin": "phone",
            "uid": "a1b2",
            "summary": "Call the bank",
            "status": "needs-action",
            "priority": 0,
            "due": {"kind": "floating", "at": "2026-11-21T09:00:00"},
        }))
        .unwrap()
    }

    /// #34: the machine has moved from New York to Lisbon, where it is
    /// 10:00 (05:00 in New York). asstd's Today holds a floating 9am, due
    /// on Lisbon's day; the bar, started in New York, reads it in Lisbon
    /// too, as the daemon's status says.
    #[test]
    fn the_bar_reads_today_in_the_daemons_zone() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let ten_in_lisbon = "2026-11-21T10:00:00Z".parse().unwrap();
        let tasks = [nine_am_floating()];

        let moved = line(&tasks, &status("Europe/Lisbon"), ten_in_lisbon, ny);
        assert_eq!(moved.class, "overdue", "9am has passed in Lisbon");

        let stayed = line(&tasks, &status("America/New_York"), ten_in_lisbon, ny);
        assert_eq!(stayed.class, "due", "not yet in New York");

        let unread = line(&tasks, &status("Mars/Olympus"), ten_in_lisbon, ny);
        assert_eq!(unread.class, "due", "a name it cannot read: the fallback");
    }
}
