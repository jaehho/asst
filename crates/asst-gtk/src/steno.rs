//! Opening the steno meeting a task came from (#28). steno's GApplication
//! (`dev.jaeho.Steno`) answers `open-session` with a meeting's name,
//! through `org.freedesktop.Application.ActivateAction`, the way a
//! reminder's click opens a task here; the activation token lets its
//! window take focus.
//!
//! A steno that isn't running is not started. steno listens for meetings
//! whenever it runs, and quitting it is how that stops, so a click here
//! must not bring the listening back. steno ships no D-Bus service file,
//! so the bus does not start it either; should it ship one, starting is
//! its choice, made there.

use std::time::Duration;

use relm4::gtk::prelude::*;
use relm4::gtk::{self, gio, glib};

pub const APP_ID: &str = "dev.jaeho.Steno";
/// The command that is steno on this machine.
const COMMAND: &str = "steno";
/// How long steno has to answer a call: the call waits for its main loop.
const WAIT: Duration = Duration::from_secs(20);

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

/// Open meeting `session` in a running steno. An error is a sentence to
/// show.
pub async fn open(session: &str, token: Option<String>) -> Result<(), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("Could not reach Steno: {e}"))?;
    let call = asst_core::api::activate_action(&conn, APP_ID, "open-session", session, token);
    match tokio::time::timeout(WAIT, call).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) if not_running(&e) => Err("Steno is not running".into()),
        Ok(Err(zbus::Error::MethodError(_, Some(text), _))) => {
            Err(format!("Steno did not open the meeting: {text}"))
        }
        Ok(Err(e)) => Err(format!("Steno did not open the meeting: {e}")),
        Err(_) => Err("Steno did not answer".into()),
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
    fn steno_with_no_owner_is_not_running_and_a_refusal_is_said() {
        assert!(not_running(&answered(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        )));
        assert!(not_running(&answered(
            "org.freedesktop.DBus.Error.NameHasNoOwner"
        )));
        // steno is up but has no such action, or said no.
        assert!(!not_running(&answered(
            "org.freedesktop.DBus.Error.UnknownMethod"
        )));
        assert!(!not_running(&answered("org.freedesktop.DBus.Error.Failed")));
    }
}
