//! Location alarms: asstd follows GeoClue and rings a reminder on arrival,
//! the way the phone does for an iPhone-made one. Each alarm rings once per
//! arrival: it re-arms when the position leaves its radius. GeoClue missing
//! (or refusing us) is logged once and retried on the next wake.
//!
//! # ponytail: the arrived-set is memory only, so a restart while inside a
//! radius rings that reminder once more; move it into the `fired` table if
//! that ever annoys anyone.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asst_core::task::LocationAlarm;
use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::daemon::Daemon;

#[zbus::proxy(
    interface = "org.freedesktop.GeoClue2.Manager",
    default_service = "org.freedesktop.GeoClue2",
    default_path = "/org/freedesktop/GeoClue2/Manager"
)]
trait Manager {
    fn get_client(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "org.freedesktop.GeoClue2.Client", default_service = "org.freedesktop.GeoClue2")]
trait Client {
    #[zbus(property)]
    fn set_desktop_id(&self, id: String) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_requested_accuracy_level(&self, level: u32) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_distance_threshold(&self, metres: u32) -> zbus::Result<()>;
    fn start(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn location_updated(
        &self,
        old: OwnedObjectPath,
        new: OwnedObjectPath,
    ) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.GeoClue2.Location", default_service = "org.freedesktop.GeoClue2")]
trait Location {
    #[zbus(property)]
    fn latitude(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn longitude(&self) -> zbus::Result<f64>;
}

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
    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;
}

/// (href, alarm key) pairs whose radius the position is in now.
type Inside = Arc<Mutex<HashSet<(String, String)>>>;

pub async fn run(daemon: Arc<Daemon>, conn: zbus::Connection) {
    let inside: Inside = Arc::default();
    let mut warned = false;
    loop {
        if let Err(e) = one_round(&daemon, &conn, &inside).await {
            if !warned {
                log::info!("location reminders wait for GeoClue: {e}");
                warned = true;
            }
        } else {
            warned = false;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(10 * 60)) => {}
            _ = daemon.wake_reminders.notified() => {}
        }
    }
}

/// Subscribe and follow the position until woken; an error means GeoClue is
/// not there (or says no). Returns when the subscription ends.
async fn one_round(
    daemon: &Arc<Daemon>,
    session: &zbus::Connection,
    inside: &Inside,
) -> Result<(), String> {
    // GeoClue sits on the system bus; the session one carries notifications.
    let conn = zbus::Connection::system().await.map_err(|e| e.to_string())?;
    let manager = ManagerProxy::new(&conn).await.map_err(|e| e.to_string())?;
    let path = manager
        .get_client()
        .await
        .map_err(|e| e.to_string())?;
    let client = ClientProxy::builder(&conn)
        .path(path)
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    client.set_desktop_id("asst".into()).await.map_err(|e| e.to_string())?;
    client
        .set_requested_accuracy_level(4)
        .await
        .map_err(|e| e.to_string())?;
    client
        .set_distance_threshold(25)
        .await
        .map_err(|e| e.to_string())?;
    let mut updates = client
        .receive_location_updated()
        .await
        .map_err(|e| e.to_string())?;
    client.start().await.map_err(|e| e.to_string())?;
    loop {
        let position;
        tokio::select! {
            signal = updates.next() => {
                let Some(args) = signal else { return Err("geoclue went away".into()) };
                let args = match args.args() {
                    Ok(a) => a,
                    Err(e) => {
                        log::warn!("reading a position update: {e}");
                        continue;
                    }
                };
                let (lat, lon) = match position_of(&conn, &args.new).await {
                    Ok(at) => at,
                    Err(e) => {
                        log::warn!("reading a position: {e}");
                        continue;
                    }
                };
                position = (lat, lon);
            }
            _ = daemon.wake_reminders.notified() => {
                // Keep the subscription unless the last location alarm went.
                if daemon
                    .store()
                    .with_location_alarms()
                    .map(|r| r.is_empty())
                    .unwrap_or(true)
                {
                    return Ok(());
                }
                continue;
            }
        }
        let (lat, lon) = position;
        check(daemon, session, inside, lat, lon);
    }
}

/// The coordinates of a GeoClue location object.
async fn position_of(
    conn: &zbus::Connection,
    path: &OwnedObjectPath,
) -> Result<(f64, f64), String> {
    let location = LocationProxy::builder(conn)
        .path(path.clone())
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())?;
    let lat = location.latitude().await.map_err(|e| e.to_string())?;
    let lon = location.longitude().await.map_err(|e| e.to_string())?;
    Ok((lat, lon))
}

/// Ring every open task's alarm whose radius now holds the position, once
/// per arrival.
fn check(daemon: &Daemon, conn: &zbus::Connection, inside: &Inside, lat: f64, lon: f64) {
    let mut guard = inside.lock().unwrap_or_else(|p| p.into_inner());
    for row in daemon.store().with_location_alarms().unwrap_or_default() {
        for a in &row.task.location_alarms {
            let key = alarm_key(a);
            if metres(lat, lon, a.lat, a.lon) > f64::from(a.radius) {
                guard.remove(&(row.href.clone(), key));
                continue;
            }
            if !guard.insert((row.href.clone(), key)) {
                continue; // already inside: seen
            }
            let (conn, href, title) = (conn.clone(), row.href.clone(), a.title.clone());
            let summary = row.task.summary.clone();
            tokio::spawn(async move {
                if let Err(e) = arrived(&conn, &summary, &title, &href).await {
                    log::warn!("showing a location reminder: {e}");
                }
            });
        }
    }
}

fn alarm_key(a: &LocationAlarm) -> String {
    if a.uid.is_empty() {
        format!("{},{},{}", a.lat, a.lon, a.radius)
    } else {
        a.uid.clone()
    }
}

async fn arrived(
    conn: &zbus::Connection,
    summary: &str,
    title: &str,
    href: &str,
) -> zbus::Result<()> {
    let proxy = NotificationsProxy::new(conn).await?;
    let mut hints = HashMap::new();
    hints.insert("desktop-entry", Value::from(asst_core::config::APP_ID));
    let id = proxy
        .notify(
            "asst",
            0,
            asst_core::config::APP_ID,
            summary,
            &format!("Arrived at {title}"),
            &["default", "Open"],
            hints,
            -1,
        )
        .await?;
    let mut actions = proxy.receive_action_invoked().await?;
    while let Some(signal) = actions.next().await {
        if let Ok(args) = signal.args()
            && args.id == id
            && args.action_key == "default"
        {
            let _ = asst_core::api::open_in_window(conn, href, None).await;
            break;
        }
    }
    Ok(())
}

/// Metres between two points, on a sphere close enough.
fn metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let (lat1, lat2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = lat2 - lat1;
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_radius_of_100_m_holds_the_same_point() {
        assert!(metres(40.7, -74.0, 40.7, -74.0) < f64::EPSILON);
        // A tenth of a degree of latitude is 11.1 km.
        assert!(metres(40.7, -74.0, 40.8, -74.0) > 11_000.0);
    }
}
