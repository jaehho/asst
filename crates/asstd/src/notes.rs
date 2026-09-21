//! Links follow their notes. The notes folder is watched, and a note or a
//! folder of them renamed or moved within it takes its links along: moved
//! here in an editor or a file manager, or elsewhere and brought over by the
//! Nextcloud client, which moves the file on this disk too. A move while
//! asstd isn't running goes unnoticed; the window shows that link as missing.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use asst_core::note_files;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::daemon::Daemon;

/// How long the folder has to be quiet before links follow what moved.
const SETTLE: Duration = Duration::from_secs(1);

pub async fn run(daemon: Arc<Daemon>) {
    loop {
        let dir = daemon.config().notes_dir();
        let watcher = watch(&daemon, &dir);
        if watcher.is_some() {
            daemon.notes_dir_changed.notified().await;
        } else {
            // Maybe the Nextcloud client hasn't made it yet: look again later.
            tokio::select! {
                _ = daemon.notes_dir_changed.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(600)) => {}
            }
        }
        drop(watcher);
    }
}

/// Watch `dir` until the watcher is dropped, which ends the thread taking
/// its events.
fn watch(daemon: &Arc<Daemon>, dir: &Path) -> Option<RecommendedWatcher> {
    if !dir.is_dir() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let started = notify::recommended_watcher(tx).and_then(|mut w| {
        w.watch(dir, RecursiveMode::Recursive)?;
        Ok(w)
    });
    let watcher = match started {
        Ok(w) => w,
        Err(e) => {
            log::warn!("can't watch {}: {e}", dir.display());
            return None;
        }
    };
    let (daemon, root) = (daemon.clone(), dir.to_path_buf());
    std::thread::spawn(move || follow(&daemon, &root, &rx));
    log::info!("following notes moved in {}", dir.display());
    Some(watcher)
}

/// Renames, taken in order once the folder settles: a save that swaps files
/// or a chain of renames has finished by then.
fn follow(daemon: &Daemon, root: &Path, events: &Receiver<notify::Result<Event>>) {
    let mut moves: Vec<(String, String)> = Vec::new();
    loop {
        let event = if moves.is_empty() {
            match events.recv() {
                Ok(e) => e,
                Err(_) => return,
            }
        } else {
            match events.recv_timeout(SETTLE) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => {
                    match daemon.follow_moves(&moves) {
                        Ok(0) => {}
                        Ok(n) => log::info!("{n} task(s) follow notes that moved: {moves:?}"),
                        Err(e) => log::warn!("following notes that moved: {e}"),
                    }
                    moves.clear();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        match event {
            Ok(Event {
                kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                paths,
                ..
            }) => {
                if let [from, to] = paths.as_slice()
                    && let (Some(from), Some(to)) = (
                        note_files::relative(root, from),
                        note_files::relative(root, to),
                    )
                {
                    moves.push((from, to));
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("watching {}: {e}", root.display()),
        }
    }
}
