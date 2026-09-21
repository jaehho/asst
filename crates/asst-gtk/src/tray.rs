//! The tray icon, as the other apps in the bar have one: a StatusNotifierItem,
//! which waybar's tray, KDE, XFCE and GNOME's AppIndicator extension show.
//! Click shows or hides the window, middle-click adds a task, and the menu has
//! the rest. It is up while "Run in background" is on, and closing the window
//! then leaves asst here instead of quitting.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ksni::menu::StandardItem;
use ksni::{MenuItem, ToolTip, TrayMethods};
use relm4::gtk::glib;

use crate::window::{Msg, Tx};

/// A tray host looks icons up by name, in the folder the item points it to.
const ICONS: [(&str, &str); 2] = [
    ("asst-tray", include_str!("../tray/asst-tray.svg")),
    (
        "asst-tray-overdue",
        include_str!("../tray/asst-tray-overdue.svg"),
    ),
];

/// Waybar only searches a folder with a theme index in it.
const INDEX: &str = "[Icon Theme]
Name=Hicolor
Comment=asst's tray icons
Directories=scalable/status

[scalable/status]
Size=16
MinSize=8
MaxSize=512
Type=Scalable
Context=Status
";

/// What the icon says: a dot for overdue tasks, and counts in the tooltip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub overdue: usize,
    /// Due today, not counting the overdue ones.
    pub today: usize,
}

impl State {
    fn summary(&self) -> String {
        let today = match self.today {
            1 => "1 task due today".to_string(),
            n => format!("{n} tasks due today"),
        };
        match (self.overdue, self.today) {
            (0, 0) => "Nothing due today".into(),
            (0, _) => today,
            (o, 0) => format!("{o} overdue"),
            (o, n) => format!("{o} overdue · {n} due today"),
        }
    }
}

pub struct Item {
    tx: Tx,
    online: Arc<AtomicBool>,
    theme: String,
    state: State,
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "asst".into()
    }

    fn title(&self) -> String {
        "asst".into()
    }

    fn icon_name(&self) -> String {
        ICONS[usize::from(self.state.overdue > 0)].0.into()
    }

    fn icon_theme_path(&self) -> String {
        self.theme.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "asst".into(),
            description: self.state.summary(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.tx.emit(Msg::TrayClick);
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        quick_add();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, activate: fn(&mut Item)| {
            StandardItem {
                label: label.into(),
                activate: Box::new(activate),
                ..StandardItem::default()
            }
            .into()
        };
        vec![
            item("Open asst", |t| t.tx.emit(Msg::ShowWindow)),
            item("Add a task…", |_| quick_add()),
            item("Sync now", |t| t.tx.emit(Msg::SyncNow)),
            MenuItem::Separator,
            item("Quit", |t| t.tx.emit(Msg::Quit)),
        ]
    }

    fn watcher_online(&self) {
        self.online.store(true, Ordering::Relaxed);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // Waybar restarting, or not up yet at login: keep waiting for it.
        self.online.store(false, Ordering::Relaxed);
        true
    }
}

pub struct Tray {
    handle: RefCell<Option<ksni::Handle<Item>>>,
    online: Arc<AtomicBool>,
    state: RefCell<State>,
}

impl Tray {
    pub fn start(tx: Tx, state: State) -> Rc<Tray> {
        // Registering either works or tells the item the watcher is away.
        let online = Arc::new(AtomicBool::new(true));
        let tray = Rc::new(Tray {
            handle: RefCell::default(),
            online: online.clone(),
            state: RefCell::new(state.clone()),
        });
        let theme = write_icons().map_or_else(
            |e| {
                eprintln!("asst-gtk: tray icons: {e}");
                String::new()
            },
            |dir| dir.display().to_string(),
        );
        let item = Item {
            tx,
            online: online.clone(),
            theme,
            state,
        };
        // Started with the session, asst can be up before the bar is.
        let join = relm4::spawn(async move { item.assume_sni_available(true).spawn().await });
        let weak = Rc::downgrade(&tray);
        glib::spawn_future_local(async move {
            match join.await {
                Ok(Ok(handle)) => match weak.upgrade() {
                    Some(tray) => {
                        *tray.handle.borrow_mut() = Some(handle);
                        tray.push();
                    }
                    None => {
                        handle.shutdown();
                    }
                },
                Ok(Err(e)) => {
                    online.store(false, Ordering::Relaxed);
                    eprintln!("asst-gtk: no tray icon: {e}");
                }
                Err(_) => online.store(false, Ordering::Relaxed),
            }
        });
        tray
    }

    /// Whether a tray is showing the icon, so closing the window has
    /// somewhere to leave asst.
    pub fn online(&self) -> bool {
        self.handle.borrow().is_some() && self.online.load(Ordering::Relaxed)
    }

    pub fn set(&self, state: State) {
        if *self.state.borrow() != state {
            *self.state.borrow_mut() = state;
            self.push();
        }
    }

    fn push(&self) {
        let Some(handle) = self.handle.borrow().clone() else {
            return;
        };
        let state = self.state.borrow().clone();
        relm4::spawn(async move { handle.update(|item| item.state = state).await });
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
        }
    }
}

/// The popup for adding a task, as its own process like the bind starts it.
fn quick_add() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // After `make install` replaced the binary under us, the link names the
    // old file as deleted; the new one is at the same path.
    let exe = PathBuf::from(
        exe.to_string_lossy()
            .trim_end_matches(" (deleted)")
            .to_string(),
    );
    match std::process::Command::new(exe).arg("quick-add").spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("asst-gtk: quick add: {e}"),
    }
}

/// The icons as files, where a tray host can read them.
fn write_icons() -> std::io::Result<PathBuf> {
    let name = if crate::devel::enabled() {
        "asst-devel"
    } else {
        "asst"
    };
    let base = glib::user_runtime_dir().join(name).join("icons");
    let status = base.join("hicolor/scalable/status");
    std::fs::create_dir_all(&status)?;
    std::fs::write(base.join("hicolor/index.theme"), INDEX)?;
    for (icon, svg) in ICONS {
        std::fs::write(status.join(format!("{icon}.svg")), svg)?;
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_counts_overdue_apart_from_today() {
        let say = |overdue, today| State { overdue, today }.summary();
        assert_eq!(say(0, 0), "Nothing due today");
        assert_eq!(say(0, 1), "1 task due today");
        assert_eq!(say(2, 0), "2 overdue");
        assert_eq!(say(2, 3), "2 overdue · 3 due today");
    }
}
