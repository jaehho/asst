//! `asst bar`: a waybar custom module (`"return-type": "json"`). One line on
//! start, on every change, and each minute as due times pass.

use std::time::Duration;

use asst_core::api::{AsstProxy, StatusView, SyncState, TaskView, query};
use asst_core::fmt;
use asst_core::store::View;
use chrono::Utc;
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

async fn render(proxy: &AsstProxy<'_>, zone: Tz) -> anyhow::Result<Line> {
    let tasks: Vec<TaskView> = serde_json::from_str(
        &proxy
            .tasks(&serde_json::to_string(&query(View::Today))?)
            .await?,
    )?;
    let status: StatusView = serde_json::from_str(&proxy.status().await?)?;
    let now = Utc::now().with_timezone(&zone);
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
    Ok(Line {
        // Empty text hides the module when nothing is due.
        text: if tasks.is_empty() {
            String::new()
        } else {
            tasks.len().to_string()
        },
        alt: class,
        class,
        tooltip: tooltip.join("\n"),
    })
}

pub async fn run() -> anyhow::Result<()> {
    let zone = asst_core::time::local_zone();
    loop {
        if let Err(e) = follow(zone).await {
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

async fn follow(zone: Tz) -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = AsstProxy::new(&conn).await?;
    let mut changed = proxy.receive_changed().await?;
    let mut status = proxy.receive_status_changed().await?;
    let mut owner = proxy.inner().receive_owner_changed().await?;
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        print(&render(&proxy, zone).await?);
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
