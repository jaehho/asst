//! Sync when the machine wakes up or gets back online, and follow it to
//! another time zone.

use std::sync::Arc;

use futures_util::StreamExt;

use crate::daemon::Daemon;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1 {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;
}

/// NM_STATE_CONNECTED_GLOBAL
const CONNECTED: u32 = 70;

pub async fn run(daemon: Arc<Daemon>) {
    let system = match zbus::Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            log::info!("no system bus, so no wake or network triggers: {e}");
            return;
        }
    };
    let mut sleep = match Login1Proxy::new(&system).await {
        Ok(p) => p.receive_prepare_for_sleep().await.ok(),
        Err(_) => None,
    };
    let mut network = match NetworkManagerProxy::new(&system).await {
        Ok(p) => p.receive_state_changed().await.ok(),
        Err(_) => None,
    };
    let mut timedate = match asst_core::timedate::changes(&system).await {
        Ok(s) => {
            // main read the zone before this match existed, so a move in
            // between went unheard; read it again now that one would not.
            daemon.set_zone(asst_core::time::local_zone());
            Some(s)
        }
        Err(e) => {
            log::info!("not following time zone changes: {e}");
            None
        }
    };
    loop {
        tokio::select! {
            Some(signal) = async { sleep.as_mut()?.next().await } => {
                if signal.args().is_ok_and(|a| !a.start) {
                    log::info!("woke up; syncing");
                    daemon.wake_reminders.notify_one();
                    daemon.request_sync();
                }
            }
            Some(signal) = async { network.as_mut()?.next().await } => {
                if signal.args().is_ok_and(|a| a.state == CONNECTED) {
                    log::info!("back online; syncing");
                    daemon.request_sync();
                }
            }
            Some(signal) = async { timedate.as_mut()?.next().await } => {
                if signal.args().is_ok_and(|a| asst_core::timedate::moves_zone(&a)) {
                    daemon.set_zone(asst_core::time::local_zone());
                }
            }
            else => break,
        }
    }
}
