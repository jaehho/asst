//! `asst autostart` against a scratch home: it writes and removes only its
//! own entry, never edits the compositor's line, and needs no bus.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch HOME, its `.config`, and an empty folder for PATH, so the
/// session check finds no `systemctl` and only `XDG_CURRENT_DESKTOP`
/// decides it.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Home {
        let root =
            std::env::temp_dir().join(format!("asst-autostart-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".config")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        Home(root)
    }

    fn config(&self) -> PathBuf {
        self.0.join(".config")
    }

    fn entry(&self) -> PathBuf {
        self.config().join("autostart/dev.jaeho.Asst.desktop")
    }

    /// `asst ARGS` here; its stdout, after checking it succeeded.
    fn asst(&self, desktop: &str, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_asst"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.config())
            .env("PATH", self.0.join("bin"))
            .env("XDG_CURRENT_DESKTOP", desktop)
            // A dead bus: the command must not need one, and nothing it
            // runs may reach the desktop.
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/asst-test",
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "asst {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn exec_line(entry: &Path) -> String {
    std::fs::read_to_string(entry)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("Exec=").map(String::from))
        .unwrap()
}

#[test]
fn on_writes_the_entry_and_off_removes_it() {
    let home = Home::new("toggle");
    assert_eq!(home.asst("GNOME", &["autostart"]), "Start at login: off\n");
    assert!(!home.entry().exists());

    assert_eq!(
        home.asst("GNOME", &["autostart", "on"]),
        "Start at login: on\n"
    );
    let exec = exec_line(&home.entry());
    assert!(exec.ends_with("asst-gtk --background"), "{exec}");
    assert_eq!(home.asst("GNOME", &["autostart"]), "Start at login: on\n");

    assert_eq!(
        home.asst("GNOME", &["autostart", "off"]),
        "Start at login: off\n"
    );
    assert!(!home.entry().exists());
    // Off again is no error.
    assert_eq!(
        home.asst("GNOME", &["autostart", "off"]),
        "Start at login: off\n"
    );
}

#[test]
fn a_session_that_may_not_run_entries_is_told_what_to_do() {
    let home = Home::new("hint");
    assert_eq!(
        home.asst("Hyprland", &["autostart", "on"]),
        "Start at login: on\n\
         This session may not run autostart entries: \
         start `asst-gtk --background` from its config.\n"
    );
}

#[test]
fn a_compositor_line_counts_as_on_and_is_left_alone() {
    let home = Home::new("compositor");
    let lua = home.config().join("hypr/hyprland.lua");
    std::fs::create_dir_all(lua.parent().unwrap()).unwrap();
    let config = "hl.on(\"hyprland.start\", function()\n  \
                  hl.exec_cmd(\"asst-gtk --background\")\nend)\n";
    std::fs::write(&lua, config).unwrap();

    assert_eq!(
        home.asst("Hyprland", &["autostart"]),
        "Start at login: on\nStarted by ~/.config/hypr/hyprland.lua\n"
    );
    // On already: no entry beside the line, which would start it twice.
    assert_eq!(
        home.asst("Hyprland", &["autostart", "on"]),
        "Start at login: on\nStarted by ~/.config/hypr/hyprland.lua\n"
    );
    assert!(!home.entry().exists());
    assert_eq!(
        home.asst("Hyprland", &["autostart", "off"]),
        "Start at login: on\n\
         Still started by ~/.config/hypr/hyprland.lua; remove that line to stop it.\n"
    );
    assert_eq!(std::fs::read_to_string(&lua).unwrap(), config);
}

#[test]
fn json_says_which_of_the_two_starts_it() {
    let home = Home::new("json");
    let read = |args: &[&str]| -> serde_json::Value {
        serde_json::from_str(&home.asst("GNOME", args)).unwrap()
    };
    assert_eq!(
        read(&["--json", "autostart"]),
        serde_json::json!({"on": false, "entry": null, "compositor": null})
    );
    assert_eq!(
        read(&["--json", "autostart", "on"]),
        serde_json::json!({
            "on": true,
            "entry": home.entry().display().to_string(),
            "compositor": null,
        })
    );
    let sway = home.config().join("sway/config");
    std::fs::create_dir_all(sway.parent().unwrap()).unwrap();
    std::fs::write(&sway, "exec asst-gtk --background\n").unwrap();
    assert_eq!(
        read(&["--json", "autostart", "off"]),
        serde_json::json!({
            "on": true,
            "entry": null,
            "compositor": sway.display().to_string(),
        })
    );
}
