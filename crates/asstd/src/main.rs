//! asstd: keeps tasks in sync with the CalDAV server, raises reminders, and
//! serves `dev.jaeho.Asst1` to the CLI, the window and quick add.

mod daemon;
mod github;
mod location;
mod reminders;
mod service;
mod system;

use anyhow::Context;
use asst_core::api::{BUS_NAME, OBJECT_PATH};
use asst_core::config::{self, Config};
use asst_core::store::Store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut log = env_logger::Builder::new();
    // zbus logs every method call at info; only asst's own lines are news.
    log.filter_level(log::LevelFilter::Warn)
        .filter_module("asstd", log::LevelFilter::Info)
        .filter_module("asst_core", log::LevelFilter::Info)
        .parse_env("ASST_LOG");
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        log.format_timestamp(None);
    }
    log.init();

    let config = Config::load()?;
    let zone = asst_core::time::local_zone();
    let dir = config::state_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let store = Store::open(&dir.join("asst.db"), zone).context("opening the task cache")?;
    let (daemon, events) = daemon::Daemon::new(config, store, zone);

    let conn = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            service::Service {
                daemon: daemon.clone(),
            },
        )?
        .build()
        .await
        .with_context(|| {
            format!("claiming {BUS_NAME} on the session bus (is asstd already running?)")
        })?;
    log::info!("serving {BUS_NAME} in {}", zone.name());

    tokio::spawn(service::pump(conn.clone(), events));
    tokio::spawn(daemon.clone().sync_loop());
    tokio::spawn(reminders::run(daemon.clone(), conn.clone()));
    tokio::spawn(location::run(daemon.clone(), conn.clone()));
    tokio::spawn(system::run(daemon.clone()));
    tokio::spawn(github::run(daemon.clone()));

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    log::info!("stopping");
    daemon.flush().await;
    Ok(())
}
