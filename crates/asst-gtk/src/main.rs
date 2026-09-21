//! asst-gtk: the window (default; `--background` starts it in the tray) and
//! `quick-add`, both clients of asstd.

mod addcard;
mod autostart;
mod client;
mod content;
mod devel;
mod dialogs;
mod editor;
mod find;
mod linked;
mod model;
mod motion;
mod notes;
mod pickers;
mod prefs;
mod quick_add;
mod row;
mod settings;
mod sidebar;
mod tray;
mod ui;
mod window;

pub const CSS: &str = include_str!("style.css");

/// The symbolic icons bundled from GNOME's Icon Development Kit.
pub fn load_icons() {
    use relm4::gtk::{gdk, gio};
    gio::resources_register_include!("asst.gresource").expect("bundled icons");
    if let Some(display) = gdk::Display::default() {
        relm4::gtk::IconTheme::for_display(&display).add_resource_path("/dev/jaeho/Asst/icons");
    }
}

const USAGE: &str = "usage: asst-gtk [--background | quick-add [--attach NOTE]...]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => window::run(window::Start::Window),
        Some("--background") => window::run(window::Start::Background),
        // What the D-Bus service file runs.
        Some("--gapplication-service") => window::run(window::Start::Service),
        Some("quick-add") => match attachments(&args[1..]) {
            Ok(linked) => quick_add::run(linked),
            Err(e) => {
                eprintln!("asst-gtk: {e}; {USAGE}");
                std::process::exit(2);
            }
        },
        Some("-h" | "--help") => println!("{USAGE}"),
        Some(other) => {
            eprintln!("asst-gtk: unknown command {other:?}; {USAGE}");
            std::process::exit(2);
        }
    }
}

/// `--attach NOTE` pairs, as full paths.
fn attachments(args: &[String]) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let note = match arg.strip_prefix("--attach=") {
            Some(note) => note,
            None if arg == "--attach" => rest.next().ok_or("--attach needs a note")?,
            None => return Err(format!("unknown option {arg:?}")),
        };
        let full = std::path::absolute(note).map_err(|e| format!("{note}: {e}"))?;
        notes.push(full.to_string_lossy().into_owned());
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_add_takes_notes_to_link() {
        let args = |a: &[&str]| attachments(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            args(&["--attach", "/n/Trip.md", "--attach=Plan.md"]).unwrap(),
            [
                "/n/Trip.md".to_string(),
                here.join("Plan.md").to_string_lossy().into_owned()
            ]
        );
        assert!(args(&["--attach"]).is_err());
        assert!(args(&["--note", "x"]).is_err());
        assert!(args(&[]).unwrap().is_empty());
    }
}
