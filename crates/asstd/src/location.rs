//! Location alarms: asstd follows GeoClue and rings a reminder on arrival,
//! the way the phone does for an iPhone-made one. Each alarm rings once per
//! arrival: it re-arms when the position leaves its radius. GeoClue missing
//! (or refusing us) is logged once and retried on the next wake. With no
//! location alarm on any open task asstd does not talk to GeoClue at all.
//! A GeoClue that cannot be reached, or reports no position for two
//! minutes, is no silent failure (#12): the status carries why (the window
//! and `asst status` show it), and one notification names the reminders
//! that cannot ring here, once for each reminder, ever.
//!
//! # IP or timezone fallback: not done on purpose. An alarm's radius is
//! about 100 m; an IP guess is off by kilometres, so it would ring at the
//! wrong place or never, which is worse than staying quiet.
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

#[zbus::proxy(
    interface = "org.freedesktop.GeoClue2.Client",
    default_service = "org.freedesktop.GeoClue2"
)]
trait Client {
    #[zbus(property)]
    fn set_desktop_id(&self, id: String) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_requested_accuracy_level(&self, level: u32) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_distance_threshold(&self, metres: u32) -> zbus::Result<()>;
    fn start(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn location_updated(&self, old: OwnedObjectPath, new: OwnedObjectPath) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.GeoClue2.Location",
    default_service = "org.freedesktop.GeoClue2"
)]
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
        // With no alarm to watch for, leave GeoClue alone: it would be
        // started every round just to say nothing.
        if has_alarms(&daemon) {
            match one_round(&daemon, &conn, &inside).await {
                Err(e) => {
                    if !warned {
                        log::info!("location reminders wait for GeoClue: {e}");
                        warned = true;
                    }
                }
                Ok(()) => warned = false,
            }
        } else {
            daemon.set_location_problem(None);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(10 * 60)) => {}
            _ = daemon.wake_reminders.notified() => {}
        }
    }
}

fn has_alarms(daemon: &Daemon) -> bool {
    daemon
        .store()
        .with_location_alarms()
        .map(|r| !r.is_empty())
        .unwrap_or(false)
}

/// How long a started client may stay silent before we say so.
const NO_FIX_AFTER: Duration = Duration::from_secs(120);

/// What the status says when GeoClue is there but never answers.
const NO_POSITION: &str = "This computer has no way to tell where it is (GeoClue \
    reports no position: no GPS, and no working wifi source), so location \
    reminders can't ring here.";

/// What the status says when GeoClue cannot be talked to at all.
fn geoclue_down(error: &str) -> String {
    format!(
        "GeoClue is not working on this computer ({error}), so location reminders can't ring here."
    )
}

/// A location reminder, for saying which cannot ring.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alarmed {
    /// Task href and alarm, to know it again.
    key: String,
    task: String,
    place: String,
}

fn alarmed(daemon: &Daemon) -> Vec<Alarmed> {
    let mut out = Vec::new();
    for row in daemon.store().with_location_alarms().unwrap_or_default() {
        for a in &row.task.location_alarms {
            out.push(Alarmed {
                key: format!("{}#{}", row.href, alarm_key(a)),
                task: row.task.summary.clone(),
                place: a.title.clone(),
            });
        }
    }
    out
}

/// Location reminders cannot ring here, for `why`: say so in the status,
/// and in one notification for the reminders not yet told about.
async fn unavailable(daemon: &Daemon, session: &zbus::Connection, why: String) {
    daemon.set_location_problem(Some(why.clone()));
    let all = alarmed(daemon);
    let path = told_path();
    let told = read_told(&path);
    let fresh = untold(&all, &told);
    if fresh.is_empty() {
        return;
    }
    let (summary, body) = notice(&fresh, &why);
    match tell(session, &summary, &body).await {
        // What is known now is all that was told; a reminder taken away
        // and made again is told about again.
        Ok(()) => write_told(&path, &all),
        Err(e) => log::warn!("telling that location reminders cannot ring: {e}"),
    }
}

/// The reminders in `all` nobody has been told about.
fn untold<'a>(all: &'a [Alarmed], told: &HashSet<String>) -> Vec<&'a Alarmed> {
    all.iter().filter(|a| !told.contains(&a.key)).collect()
}

/// The notification: the reason, and which reminders (the first, and how
/// many more) the phone will ring instead.
fn notice(fresh: &[&Alarmed], why: &str) -> (String, String) {
    let first = fresh.first().expect("something to tell");
    let more = match fresh.len() - 1 {
        0 => String::new(),
        n => format!(" and {n} more"),
    };
    (
        "Location reminders can't ring on this computer".to_string(),
        format!(
            "{why}\n“{}” (at {}){more}: your phone still rings them.",
            first.task, first.place
        ),
    )
}

fn told_path() -> std::path::PathBuf {
    asst_core::config::state_dir().join("location-told")
}

fn read_told(path: &std::path::Path) -> HashSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn write_told(path: &std::path::Path, all: &[Alarmed]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = all.iter().map(|a| format!("{}\n", a.key)).collect();
    if let Err(e) = std::fs::write(path, text) {
        log::warn!("remembering which location reminders were told about: {e}");
    }
}

/// One notification with no buttons, from asst.
pub async fn tell(session: &zbus::Connection, summary: &str, body: &str) -> zbus::Result<()> {
    let proxy = NotificationsProxy::new(session).await?;
    let mut hints = HashMap::new();
    hints.insert("desktop-entry", Value::from(asst_core::config::APP_ID));
    proxy
        .notify(
            "asst",
            0,
            asst_core::config::APP_ID,
            summary,
            body,
            &[],
            hints,
            -1,
        )
        .await
        .map(|_| ())
}

/// Subscribe and follow the position until woken; an error means GeoClue is
/// not there (or says no). Returns when the subscription ends.
async fn one_round(
    daemon: &Arc<Daemon>,
    session: &zbus::Connection,
    inside: &Inside,
) -> Result<(), String> {
    // GeoClue sits on the system bus; the session one carries notifications.
    let setup = async {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| e.to_string())?;
        let manager = ManagerProxy::new(&conn).await.map_err(|e| e.to_string())?;
        let path = manager.get_client().await.map_err(|e| e.to_string())?;
        let client = ClientProxy::builder(&conn)
            .path(path)
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| e.to_string())?;
        client
            .set_desktop_id("asst".into())
            .await
            .map_err(|e| e.to_string())?;
        client
            .set_requested_accuracy_level(4)
            .await
            .map_err(|e| e.to_string())?;
        client
            .set_distance_threshold(25)
            .await
            .map_err(|e| e.to_string())?;
        let updates = client
            .receive_location_updated()
            .await
            .map_err(|e| e.to_string())?;
        client.start().await.map_err(|e| e.to_string())?;
        Ok::<_, String>((conn, updates))
    };
    let (conn, mut updates) = match setup.await {
        Ok(started) => started,
        Err(e) => {
            let said = e.replace("org.freedesktop.DBus.Error.", "");
            unavailable(daemon, session, geoclue_down(&said)).await;
            return Err(e);
        }
    };
    let mut fixed = false;
    let silence = tokio::time::sleep(NO_FIX_AFTER);
    tokio::pin!(silence);
    loop {
        let position;
        tokio::select! {
            () = &mut silence, if !fixed => {
                // Once per round; the sleep stays done, so the branch is off.
                fixed = true;
                log::warn!(
                    "GeoClue has reported no position in {}s (no GPS, and no \
                     working wifi/IP source?); location reminders cannot ring \
                     until it has one",
                    NO_FIX_AFTER.as_secs()
                );
                unavailable(daemon, session, NO_POSITION.to_string()).await;
                continue;
            }
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
                fixed = true;
                position = (lat, lon);
            }
            _ = daemon.wake_reminders.notified() => {
                // Keep the subscription unless the last location alarm went.
                if !has_alarms(daemon) {
                    return Ok(());
                }
                continue;
            }
        }
        let (lat, lon) = position;
        daemon.set_location_problem(None);
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

    fn alarm(href: &str, task: &str, place: &str) -> Alarmed {
        Alarmed {
            key: format!("{href}#{place}"),
            task: task.into(),
            place: place.into(),
        }
    }

    #[test]
    fn each_reminder_is_told_about_once_and_again_if_made_anew() {
        let dir = std::env::temp_dir().join(format!("asst-told-{}", std::process::id()));
        let path = dir.join("location-told");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read_told(&path).is_empty(), "no file, nothing told");

        let gym = alarm("/l/a.ics", "Gym", "The gym");
        let shop = alarm("/l/b.ics", "Milk", "The shop");
        let all = vec![gym.clone(), shop.clone()];
        assert_eq!(untold(&all, &read_told(&path)), [&gym, &shop]);

        write_told(&path, &all);
        assert!(untold(&all, &read_told(&path)).is_empty());
        // One more reminder: only it is news.
        let bank = alarm("/l/c.ics", "Cash", "The bank");
        let more = vec![gym.clone(), shop, bank.clone()];
        assert_eq!(untold(&more, &read_told(&path)), [&bank]);
        // The gym one is taken away, and written again later: told again.
        write_told(&path, std::slice::from_ref(&bank));
        assert_eq!(untold(&all, &read_told(&path)), [&gym, &all[1]]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_notice_names_the_reason_and_the_reminders() {
        let one = alarm("/l/a.ics", "Gym", "The gym");
        let two = alarm("/l/b.ics", "Milk", "The shop");
        let (summary, body) = notice(&[&one], NO_POSITION);
        assert_eq!(summary, "Location reminders can't ring on this computer");
        assert!(body.starts_with(NO_POSITION));
        assert!(body.ends_with("“Gym” (at The gym): your phone still rings them."));
        let (_, body) = notice(
            &[&one, &two, &two],
            &geoclue_down("NoReply: Remote peer disconnected"),
        );
        assert!(body.starts_with(
            "GeoClue is not working on this computer (NoReply: Remote peer disconnected), so \
             location reminders can't ring here."
        ));
        assert!(body.contains("“Gym” (at The gym) and 2 more:"));
    }
}
