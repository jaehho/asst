//! Opening the steno meeting a task came from (#28). steno's GApplication
//! (`dev.jaeho.Steno`) answers `open-session` with a meeting's name,
//! through `org.freedesktop.Application.ActivateAction`, the way a
//! reminder's click opens a task here; the activation token lets its
//! window take focus.
//!
//! steno ships no D-Bus service file, so the bus cannot start it. When
//! nothing owns its name, asst starts `steno gui --background` (no window
//! of its own) and asks again once the name is on the bus. Should steno
//! ship a service file, the first call starts it through the bus instead,
//! and the start here goes unused.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use relm4::gtk::prelude::*;
use relm4::gtk::{self, gio, glib};

pub const APP_ID: &str = "dev.jaeho.Steno";
/// The command that is steno on this machine, and starts it.
const COMMAND: &str = "steno";
/// How long a start has to put steno's name on the bus, and a call to be
/// answered: steno's own wait for a start (`START_WAIT_S`, `control.py`).
/// A started app takes its name before its main loop runs, and the call
/// waits for that loop.
const WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);

/// A start in flight, so a second click waits for it rather than start a
/// second steno.
static STARTING: AtomicBool = AtomicBool::new(false);

/// Whether steno is on this machine: its command is on `PATH`, as the
/// contract has a consumer tell (`~/dotfiles/docs/app-contract.md`).
pub fn here() -> bool {
    glib::find_program_in_path(COMMAND).is_some()
}

/// A token for steno's window to take focus with, from the compositor
/// (xdg-activation on Wayland), tied to this window's latest input. None
/// where the display has none to give (Broadway).
pub fn activation_token(widget: &impl IsA<gtk::Widget>) -> Option<String> {
    widget
        .display()
        .app_launch_context()
        .startup_notify_id(None::<&gio::AppInfo>, &[])
        .map(String::from)
}

/// Open meeting `session` in steno, starting steno first when it isn't
/// running; `starting` is called then, for the window to say so. An error
/// is a sentence to show.
pub async fn open(
    session: &str,
    token: Option<String>,
    starting: impl FnOnce(),
) -> Result<(), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("Could not reach Steno: {e}"))?;
    match ask(&conn, session, token.clone()).await {
        Asked::Opened => return Ok(()),
        Asked::Failed(why) => return Err(why),
        Asked::NotRunning => {}
    }
    starting();
    start(&conn).await?;
    match ask(&conn, session, token).await {
        Asked::Opened => Ok(()),
        Asked::Failed(why) => Err(why),
        Asked::NotRunning => Err("Steno quit before it opened the meeting".into()),
    }
}

/// What became of one `open-session` call.
enum Asked {
    Opened,
    /// Nothing owns steno's name, and the bus could not start it.
    NotRunning,
    /// Why not, as a sentence.
    Failed(String),
}

async fn ask(conn: &zbus::Connection, session: &str, token: Option<String>) -> Asked {
    let call = asst_core::api::activate_action(conn, APP_ID, "open-session", session, token);
    match tokio::time::timeout(WAIT, call).await {
        Ok(Ok(())) => Asked::Opened,
        Ok(Err(e)) if not_running(&e) => Asked::NotRunning,
        Ok(Err(zbus::Error::MethodError(_, Some(text), _))) => {
            Asked::Failed(format!("Steno did not open the meeting: {text}"))
        }
        Ok(Err(e)) => Asked::Failed(format!("Steno did not open the meeting: {e}")),
        Err(_) => Asked::Failed("Steno did not answer".into()),
    }
}

/// The bus's answer for a name that nothing owns and no service file
/// starts.
fn not_running(e: &zbus::Error) -> bool {
    const NAMES: [&str; 2] = [
        "org.freedesktop.DBus.Error.ServiceUnknown",
        "org.freedesktop.DBus.Error.NameHasNoOwner",
    ];
    match e {
        zbus::Error::MethodError(name, _, _) => NAMES.contains(&name.as_str()),
        zbus::Error::FDO(fdo) => matches!(
            fdo.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        ),
        _ => false,
    }
}

/// Start steno in the background and wait for its name on the bus.
async fn start(conn: &zbus::Connection) -> Result<(), String> {
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| format!("Could not reach Steno: {e}"))?;
    let name = zbus::names::BusName::try_from(APP_ID).expect("a valid bus name");
    let mut child = None;
    if !STARTING.swap(true, Ordering::SeqCst) {
        let spawned = Command::new(COMMAND)
            .args(["gui", "--background"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own process group, so a Ctrl+C meant for an asst run
            // from a terminal does not reach it.
            .process_group(0)
            .spawn();
        match spawned {
            Ok(c) => child = Some(c),
            Err(e) => {
                STARTING.store(false, Ordering::SeqCst);
                return Err(format!("Could not start Steno: {e}"));
            }
        }
    }
    let deadline = Instant::now() + WAIT;
    let up = loop {
        if dbus.name_has_owner(name.clone()).await.unwrap_or(false) {
            break Ok(());
        }
        // steno says itself why it would not start (a notification).
        if let Some(c) = child.as_mut()
            && c.try_wait().ok().flatten().is_some()
        {
            break Err("Steno did not start".to_string());
        }
        if Instant::now() >= deadline {
            break Err(format!("Steno did not start in {} seconds", WAIT.as_secs()));
        }
        tokio::time::sleep(POLL).await;
    };
    if let Some(mut c) = child {
        STARTING.store(false, Ordering::SeqCst);
        // Reaped when it ends, so it doesn't linger as a zombie.
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
    up
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error a call gets back when the bus answers with `name`.
    fn answered(name: &str) -> zbus::Error {
        let call = zbus::Message::method_call("/dev/jaeho/Steno", "ActivateAction")
            .unwrap()
            .build(&())
            .unwrap();
        let reply = zbus::Message::error(&call.header(), name)
            .unwrap()
            .build(&("why",))
            .unwrap();
        zbus::Error::from(reply)
    }

    #[test]
    fn steno_with_no_owner_is_started_and_a_refusal_is_said() {
        assert!(not_running(&answered(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        )));
        assert!(not_running(&answered(
            "org.freedesktop.DBus.Error.NameHasNoOwner"
        )));
        // steno is up but has no such action, or said no: starting another
        // would not help.
        assert!(!not_running(&answered(
            "org.freedesktop.DBus.Error.UnknownMethod"
        )));
        assert!(!not_running(&answered("org.freedesktop.DBus.Error.Failed")));
    }
}
