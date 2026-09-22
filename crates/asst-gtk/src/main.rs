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

const USAGE: &str = "usage: asst-gtk [--background | quick-add]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => window::run(window::Start::Window),
        Some("--background") => window::run(window::Start::Background),
        // What the D-Bus service file runs.
        Some("--gapplication-service") => window::run(window::Start::Service),
        Some("quick-add") => quick_add::run(),
        Some("-h" | "--help") => println!("{USAGE}"),
        Some(other) => {
            eprintln!("asst-gtk: unknown command {other:?}; {USAGE}");
            std::process::exit(2);
        }
    }
}
