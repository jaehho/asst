//! Starting at login, as something the user turns on, the way steno does it:
//! one XDG autostart entry, written when "Start at login" is switched on and
//! removed when it is switched off. Hyprland, sway and niri don't run those
//! unless the session was started through systemd (uwsm), so a line in the
//! compositor's own config that starts `asst-gtk --background` counts as on
//! and is left for the user to remove.

use std::path::{Path, PathBuf};

/// What a compositor config runs to start asst in the tray.
pub const COMMAND: &str = "asst-gtk --background";

const COMPOSITOR_CONFIGS: [&str; 4] = [
    "hypr/hyprland.lua",
    "hypr/hyprland.conf",
    "sway/config",
    "niri/config.kdl",
];

/// Desktops whose session manager reads `~/.config/autostart` by itself.
const READS_AUTOSTART: [&str; 12] = [
    "GNOME",
    "KDE",
    "XFCE",
    "X-Cinnamon",
    "MATE",
    "LXQt",
    "LXDE",
    "Budgie",
    "Pantheon",
    "Unity",
    "Deepin",
    "COSMIC",
];

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The compositor config that starts asst, if one does.
pub fn started_by_compositor() -> Option<PathBuf> {
    COMPOSITOR_CONFIGS
        .iter()
        .map(|rel| config_home().join(rel))
        .find(|path| std::fs::read_to_string(path).is_ok_and(|text| starts_asst(&text)))
}

fn starts_asst(config: &str) -> bool {
    config.lines().any(|line| {
        let line = line.trim_start();
        // Whole-line comments only: Lua's comment marker can follow the command.
        line.contains(COMMAND) && !["--", "#", "//"].iter().any(|c| line.starts_with(c))
    })
}

pub fn entry() -> PathBuf {
    config_home()
        .join("autostart")
        .join(format!("{}.desktop", crate::devel::app_id()))
}

/// An absolute path when there is one: a session's startup commands often
/// run without `~/.local/bin` on PATH.
fn command() -> String {
    let installed = [
        home().join(".local/bin/asst-gtk"),
        "/usr/bin/asst-gtk".into(),
    ];
    let exe = installed
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok())
        .map_or_else(|| "asst-gtk".to_string(), |p| quote(&p));
    format!("{exe} --background")
}

fn quote(path: &Path) -> String {
    let s = path.display().to_string();
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s
    }
}

fn entry_text() -> String {
    format!(
        "[Desktop Entry]
Type=Application
Name=asst
Comment=Tasks and reminders, from the tray
Exec={}
Icon={}
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
",
        command(),
        asst_core::config::APP_ID
    )
}

pub fn is_enabled() -> bool {
    if started_by_compositor().is_some() {
        return true;
    }
    std::fs::read_to_string(entry()).is_ok_and(|text| {
        !text
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("hidden=true"))
    })
}

pub fn set(on: bool) -> std::io::Result<()> {
    let path = entry();
    if on {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, entry_text())
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Whether this session runs autostart entries. False means "not sure".
pub fn session_reads_autostart() -> bool {
    // A session started through systemd runs them through this target.
    let by_systemd = std::process::Command::new("systemctl")
        .args([
            "--user",
            "is-active",
            "--quiet",
            "xdg-desktop-autostart.target",
        ])
        .status()
        .is_ok_and(|s| s.success());
    by_systemd
        || std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .any(|d| READS_AUTOSTART.contains(&d))
}

/// `~/…` for a path in the home folder.
pub fn tilde(path: &Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) if !home().as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compositor_line_counts_unless_commented_out() {
        assert!(starts_asst(
            "hl.exec_cmd(\"asst-gtk --background\")  -- tasks\n"
        ));
        assert!(starts_asst("exec-once = asst-gtk --background\n"));
        assert!(!starts_asst("-- hl.exec_cmd(\"asst-gtk --background\")\n"));
        assert!(!starts_asst("# exec-once = asst-gtk --background\n"));
        assert!(!starts_asst("hl.exec_cmd(\"asst-gtk\")\n"));
    }
}
