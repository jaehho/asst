//! Linked projects' `TODO.md`s. What changes on disk settles for a second,
//! then one pass takes the file's changes to the store. The other direction
//! — the store's changes written back — runs in this same task, so the two
//! passes never interleave on one file.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::Event;
use notify::{RecursiveMode, Watcher};

use crate::daemon::{Cause, Daemon};

/// How long a file has to be quiet before its changes are read.
const SETTLE: Duration = Duration::from_secs(1);

pub async fn run(daemon: Arc<Daemon>) {
    loop {
        let dirs: Vec<PathBuf> = daemon
            .store()
            .links()
            .unwrap_or_default()
            .into_iter()
            .map(|(dir, _)| PathBuf::from(dir))
            .filter(|d| d.is_dir())
            .collect();
        let watcher = watch(&daemon, &dirs);
        // Catch up after a start or a link change: adopt what the file
        // already lists, fill in what the list holds.
        daemon.reconcile_todos(Cause::Store);
        daemon.reconcile_todos(Cause::File);
        tokio::select! {
            biased;
            _ = daemon.wake_files.notified() => daemon.reconcile_todos(Cause::File),
            _ = daemon.wake_todos.notified() => daemon.reconcile_todos(Cause::Store),
            _ = daemon.links_changed.notified() => {}
        }
        drop(watcher);
    }
}

/// Watch every linked directory, not the files themselves: a save that
/// renames over a `TODO.md` (asst's own writes among them) leaves a watch
/// on the file behind. The directory is watched shallowly; nothing in it
/// but `TODO.md` is ours to read.
fn watch(daemon: &Arc<Daemon>, dirs: &[PathBuf]) -> Option<notify::RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).ok()?;
    for dir in dirs {
        if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            log::warn!("can't watch {}: {e}", dir.display());
        }
    }
    let daemon = daemon.clone();
    std::thread::spawn(move || settle(&daemon, &rx));
    Some(watcher)
}

/// Wait for a `TODO.md` to be touched, then for it to stay quiet for a
/// second, then call for the file's pass.
fn settle(daemon: &Daemon, events: &Receiver<notify::Result<Event>>) {
    let touched = |event: &Event| {
        event.paths.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n == std::ffi::OsStr::new("TODO.md"))
        })
    };
    loop {
        loop {
            match events.recv() {
                Ok(Ok(event)) if touched(&event) => break,
                Ok(_) => {}
                Err(_) => return,
            }
        }
        loop {
            match events.recv_timeout(SETTLE) {
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        daemon.wake_files.notify_one();
    }
}
