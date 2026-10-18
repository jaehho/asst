//! Preferences: one page of what is set once and left alone. The daemon's
//! settings and the window's own apply as they are changed, so there is
//! nothing to confirm and Esc loses nothing. The sidebar and its lists are
//! edited on the sidebar, and how often to sync is `asst settings`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use asst_core::api::{Settings, SettingsChange, StatusView, SyncState};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gio, glib};

use crate::model;
use crate::prefs::{PrefPatch, Prefs};
use crate::window::{Msg, Tx};
use crate::{autostart, pickers};

pub struct Ctx {
    /// The daemon's settings as just fetched, else the window's last copy;
    /// none when it has never answered.
    pub settings: Option<Settings>,
    /// The daemon's status as the window last heard it, kept current.
    pub status: Rc<RefCell<Option<StatusView>>>,
    pub prefs: Prefs,
    /// A list is tied to a GitHub repo, which is what the GitHub rows are
    /// for.
    pub linked: bool,
}

/// Sends one change to the window, which applies it to its own `Prefs`. The
/// dialog's copy (`Ctx::prefs`) only says what the controls start at.
type Patch = Rc<dyn Fn(PrefPatch)>;

/// Sends one change to the daemon's settings.
type Set = Rc<dyn Fn(SettingsChange)>;

/// Sets controls to these settings without sending them.
type Show = Box<dyn Fn(&Settings)>;

/// The dialog as the window keeps it, to say in it why the daemon refused a
/// change and to put its controls back.
pub struct Open {
    dialog: glib::WeakRef<adw::PreferencesDialog>,
    /// Puts the daemon's controls to what it has.
    show: Show,
}

impl Open {
    /// Whether the dialog is still on screen.
    pub fn is_up(&self) -> bool {
        self.dialog.upgrade().is_some_and(|d| d.root().is_some())
    }

    /// Says `why` in the dialog, where the person is looking, and sets the
    /// controls to `daemon`'s settings. False when the dialog has gone, and
    /// the caller says it somewhere else.
    pub fn refused(&self, why: &str, daemon: Option<&Settings>) -> bool {
        let Some(dialog) = self.dialog.upgrade().filter(|d| d.root().is_some()) else {
            return false;
        };
        say(&dialog, why);
        if let Some(settings) = daemon {
            (self.show)(settings);
        }
        true
    }
}

fn say(dialog: &adw::PreferencesDialog, text: &str) {
    dialog.add_toast(adw::Toast::new(&glib::markup_escape_text(text)));
}

pub fn open(parent: &impl IsA<gtk::Widget>, ctx: Ctx, tx: Tx) -> Open {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(false);
    let patch: Patch = {
        let tx = tx.clone();
        Rc::new(move |p| tx.emit(Msg::PatchPrefs(p)))
    };
    let set: Set = {
        let tx = tx.clone();
        Rc::new(move |c| tx.emit(Msg::SetSettings(Box::new(c))))
    };
    // Up while the controls are put back to what the daemon has, so that
    // doing it does not send it again.
    let syncing = Rc::new(Cell::new(false));

    let page = adw::PreferencesPage::new();
    let signed_in = ctx
        .status
        .borrow()
        .clone()
        .filter(|s| s.state != SyncState::NoAccount);
    if signed_in.is_none() {
        page.add(&sign_in_group(&dialog, &tx));
    }
    let (reminders, show) = reminders_group(ctx.settings.as_ref(), &set, &syncing);
    page.add(&reminders);
    page.add(&tasks_group(&ctx.prefs, &patch));
    page.add(&app_group(&dialog, ctx.prefs.background, &patch, &syncing));
    page.add(&more_group(&ctx.prefs, &patch, ctx.linked));
    if let Some(status) = signed_in {
        page.add(&signed_in_group(&dialog, &status, &ctx.status, &tx));
    }
    dialog.add(&page);
    dialog.present(Some(parent));
    Open {
        dialog: dialog.downgrade(),
        show,
    }
}

fn switch_row(
    (title, subtitle): (&str, &str),
    on: bool,
    patch: &Patch,
    make: fn(bool) -> PrefPatch,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(on)
        .build();
    let patch = patch.clone();
    row.connect_active_notify(move |r| patch(make(r.is_active())));
    row
}

fn account_shell() -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder()
        .title("Account")
        .description("Tasks live on Nextcloud, which Apple Reminders on the iPhone syncs with too.")
        .build()
}

fn sign_in_group(dialog: &adw::PreferencesDialog, tx: &Tx) -> adw::PreferencesGroup {
    let group = account_shell();
    let server = adw::EntryRow::builder()
        .title("Server, like cloud.example.com")
        .show_apply_button(true)
        .build();
    let (tx, dialog) = (tx.clone(), dialog.downgrade());
    server.connect_apply(move |e| {
        let s = e.text().trim().to_string();
        if !s.is_empty() {
            tx.emit(Msg::SignIn(s));
            if let Some(d) = dialog.upgrade() {
                d.close();
            }
        }
    });
    group.add(&server);
    group
}

/// Who is signed in and how sync is going, with Sign Out.
fn signed_in_group(
    dialog: &adw::PreferencesDialog,
    status: &StatusView,
    live: &Rc<RefCell<Option<StatusView>>>,
    tx: &Tx,
) -> adw::PreferencesGroup {
    let group = account_shell();
    let who = adw::ActionRow::builder()
        .title(status.server.as_deref().unwrap_or("Nextcloud"))
        .subtitle(account_line(status))
        .build();
    who.add_prefix(&gtk::Image::from_icon_name("cloud-outline-thick-symbolic"));
    let sign_out = gtk::Button::with_label("Sign Out");
    sign_out.set_valign(gtk::Align::Center);
    sign_out.add_css_class("destructive-action");
    {
        let tx = tx.clone();
        let dialog = dialog.downgrade();
        sign_out.connect_clicked(move |b| {
            let confirm = adw::AlertDialog::builder()
                .heading("Sign out?")
                .body("asst forgets its app password and its copy of your tasks. They stay on Nextcloud and your other devices. The app password stays valid on the server: revoke it under Settings → Security (asst logout says the same).")
                .close_response("cancel")
                .default_response("cancel")
                .build();
            confirm.add_response("cancel", "Cancel");
            confirm.add_response("out", "Sign Out");
            confirm.set_response_appearance("out", adw::ResponseAppearance::Destructive);
            let (tx, dialog) = (tx.clone(), dialog.clone());
            confirm.connect_response(None, move |_, r| {
                if r == "out" {
                    tx.emit(Msg::SignOut);
                    if let Some(d) = dialog.upgrade() {
                        d.close();
                    }
                }
            });
            confirm.present(Some(b));
        });
    }
    who.add_suffix(&sign_out);
    group.add(&who);
    // How long ago keeps counting, and the daemon's rounds show, until the
    // dialog is closed.
    let live = live.clone();
    let row = who.downgrade();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let Some(row) = row.upgrade().filter(|r| r.root().is_some()) else {
            return glib::ControlFlow::Break;
        };
        if let Some(s) = live.borrow().as_ref() {
            row.set_subtitle(&account_line(s));
        }
        glib::ControlFlow::Continue
    });
    group
}

/// The user, then what sync is doing.
fn account_line(s: &StatusView) -> String {
    let state = model::sync_state(s, pickers::now());
    let line = match s.username.as_deref() {
        Some(user) => format!("{user} · {state}"),
        None => state,
    };
    glib::markup_escape_text(&line).to_string()
}

/// What rings, and the buttons on it.
fn reminders_group(
    settings: Option<&Settings>,
    set: &Set,
    syncing: &Rc<Cell<bool>>,
) -> (adw::PreferencesGroup, Show) {
    let group = adw::PreferencesGroup::builder().title("Reminders").build();
    let Some(settings) = settings else {
        group.set_description(Some(
            "asstd isn't answering, so its settings can't be shown.",
        ));
        return (group, Box::new(|_| {}));
    };
    let (reminder, show_reminder) = reminder_row(settings, set, syncing);
    let (snooze, show_snooze) = snooze_row(settings, set, syncing);
    group.add(&reminder);
    group.add(&snooze);
    let show = move |s: &Settings| {
        show_reminder(s);
        show_snooze(s);
    };
    (group, Box::new(show))
}

/// When a task with a time rings by default, as minutes before it.
const REMINDER_TIMES: [(Option<u32>, &str); 9] = [
    (None, "None"),
    (Some(0), "At due time"),
    (Some(5), "5 min before"),
    (Some(10), "10 min before"),
    (Some(15), "15 min before"),
    (Some(30), "30 min before"),
    (Some(60), "1 h before"),
    (Some(120), "2 h before"),
    (Some(1440), "1 day before"),
];

type Times = Rc<RefCell<Vec<(Option<u32>, String)>>>;

/// The entry for what the daemon has, added after the nine when it is
/// another time (set by `asst settings`), as it is written.
fn reminder_index(times: &Times, labels: &gtk::StringList, settings: &Settings) -> u32 {
    let current = settings.alarm_at_due.then_some(settings.alarm_before);
    let mut times = times.borrow_mut();
    let at = times
        .iter()
        .position(|(m, _)| *m == current)
        .unwrap_or_else(|| {
            let label = format!(
                "{} before",
                asst_core::fmt::minutes(u64::from(settings.alarm_before))
            );
            labels.append(&label);
            times.push((current, label));
            times.len() - 1
        });
    at as u32
}

fn reminder_row(settings: &Settings, set: &Set, syncing: &Rc<Cell<bool>>) -> (adw::ComboRow, Show) {
    let times: Times = Rc::new(RefCell::new(
        REMINDER_TIMES
            .iter()
            .map(|(m, l)| (*m, l.to_string()))
            .collect(),
    ));
    let labels = gtk::StringList::new(&REMINDER_TIMES.map(|(_, l)| l));
    let selected = reminder_index(&times, &labels, settings);
    let row = adw::ComboRow::builder()
        .title("Default reminder")
        .subtitle("Added to new tasks that have a time. A task with only a date gets none.")
        .model(&labels)
        .selected(selected)
        .build();
    {
        let (times, set, syncing) = (times.clone(), set.clone(), syncing.clone());
        row.connect_selected_notify(move |c| {
            if syncing.get() {
                return;
            }
            let Some((minutes, _)) = times.borrow().get(c.selected() as usize).cloned() else {
                return;
            };
            set(SettingsChange {
                alarm_at_due: Some(minutes.is_some()),
                alarm_before: minutes,
                ..SettingsChange::default()
            });
        });
    }
    let show = {
        let (row, syncing) = (row.downgrade(), syncing.clone());
        move |s: &Settings| {
            let Some(row) = row.upgrade() else {
                return;
            };
            syncing.set(true);
            row.set_selected(reminder_index(&times, &labels, s));
            syncing.set(false);
        }
    };
    (row, Box::new(show))
}

/// A reminder's Snooze buttons: lengths to pick, up to three.
fn snooze_row(
    settings: &Settings,
    set: &Set,
    syncing: &Rc<Cell<bool>>,
) -> (adw::PreferencesRow, Show) {
    let mut lengths: Vec<u64> = vec![5, 10, 15, 30, 60, 120, 1440];
    lengths.extend(settings.snooze.iter().copied());
    lengths.sort_unstable();
    lengths.dedup();
    let chosen = Rc::new(RefCell::new(settings.snooze.clone()));
    let chips = adw::WrapBox::builder()
        .child_spacing(6)
        .line_spacing(6)
        .build();
    chips.add_css_class("snooze-lengths");
    let buttons: Rc<RefCell<Vec<(u64, gtk::ToggleButton)>>> = Rc::default();
    // Past the limit, the rest wait until one is let go.
    let limit = {
        let (buttons, chosen) = (buttons.clone(), chosen.clone());
        move || {
            let full = chosen.borrow().len() >= asst_core::config::MAX_SNOOZE;
            for (m, b) in buttons.borrow().iter() {
                b.set_sensitive(!full || chosen.borrow().contains(m));
            }
        }
    };
    for m in lengths {
        let b = gtk::ToggleButton::with_label(&asst_core::fmt::minutes(m));
        b.add_css_class("pill");
        b.set_active(settings.snooze.contains(&m));
        let (chosen, set, limit, syncing) =
            (chosen.clone(), set.clone(), limit.clone(), syncing.clone());
        b.connect_toggled(move |b| {
            if syncing.get() {
                return;
            }
            let mut v = chosen.borrow().clone();
            if b.is_active() {
                if !v.contains(&m) {
                    v.push(m);
                }
            } else {
                v.retain(|x| *x != m);
            }
            if v.is_empty() {
                // One is the least a reminder has.
                b.set_active(true);
                return;
            }
            v.sort_unstable();
            *chosen.borrow_mut() = v.clone();
            limit();
            set(SettingsChange {
                snooze: Some(v),
                ..SettingsChange::default()
            });
        });
        chips.append(&b);
        buttons.borrow_mut().push((m, b));
    }
    limit();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(
        &gtk::Label::builder()
            .label("Snooze buttons")
            .xalign(0.0)
            .build(),
    );
    body.append(
        &gtk::Label::builder()
            .label(format!(
                "The buttons on a reminder, up to {}. Snooze… beside them asks for any other time.",
                asst_core::config::MAX_SNOOZE
            ))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    body.append(&chips);
    let row = adw::PreferencesRow::builder()
        .title("Snooze buttons")
        .activatable(false)
        .child(&body)
        .build();

    let show = {
        let syncing = syncing.clone();
        move |s: &Settings| {
            syncing.set(true);
            for (m, b) in buttons.borrow().iter() {
                b.set_active(s.snooze.contains(m));
            }
            *chosen.borrow_mut() = s.snooze.clone();
            limit();
            syncing.set(false);
        }
    };
    (row, Box::new(show))
}

fn tasks_group(p: &Prefs, patch: &Patch) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Tasks").build();
    group.add(&switch_row(
        (
            "Complete after a moment",
            "A checked task is struck through first, so a slip can be unchecked",
        ),
        p.complete_later,
        patch,
        PrefPatch::CompleteLater,
    ));
    group.add(&switch_row(
        ("Week starts on Sunday", "In the date picker"),
        p.sunday_first,
        patch,
        PrefPatch::SundayFirst,
    ));
    group
}

/// "Run in background" and "Start at login", in the tray.
fn app_group(
    dialog: &adw::PreferencesDialog,
    on: bool,
    patch: &Patch,
    syncing: &Rc<Cell<bool>>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("App")
        .description("Sync and reminders don't need the window: asstd runs on its own.")
        .build();
    let background = switch_row(
        (
            "Run in background",
            "Keep asst in the tray when the window closes",
        ),
        on,
        patch,
        PrefPatch::Background,
    );
    group.add(&background);

    // A line in the compositor's config is the user's: it is reported, with
    // nothing to switch.
    if let Some(config) = autostart::started_by_compositor() {
        group.add(
            &adw::ActionRow::builder()
                .title("Start at login")
                .subtitle(format!(
                    "Started by {}",
                    glib::markup_escape_text(&autostart::tilde(&config))
                ))
                .build(),
        );
        return group;
    }
    let hint = if autostart::session_reads_autostart() {
        "Open in the tray when you log in".to_string()
    } else {
        format!(
            "This desktop doesn't run autostart entries: start <tt>{}</tt> from its config",
            autostart::COMMAND
        )
    };
    let login = adw::SwitchRow::builder()
        .title("Start at login")
        .active(autostart::is_enabled())
        .build();
    // It runs the tray, so it waits for the background switch.
    let wait_for_background = {
        let login = login.downgrade();
        move |background: bool| {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(background);
                login.set_subtitle(if background {
                    &hint
                } else {
                    "Needs “Run in background”"
                });
            }
        }
    };
    wait_for_background(on);
    {
        let (dialog, syncing) = (dialog.downgrade(), syncing.clone());
        login.connect_active_notify(move |row| {
            if syncing.get() {
                return;
            }
            if let Err(e) = autostart::set(row.is_active()) {
                if let Some(d) = dialog.upgrade() {
                    say(
                        &d,
                        &format!(
                            "Couldn't change {}: {e}",
                            autostart::tilde(&autostart::entry())
                        ),
                    );
                }
                // The switch says what the entry is, not what was asked.
                syncing.set(true);
                row.set_active(autostart::is_enabled());
                syncing.set(false);
            }
        });
    }
    background.connect_active_notify(move |b| wait_for_background(b.is_active()));
    group.add(&login);
    group
}

/// What is set once: behind one row, so the page is short.
fn more_group(p: &Prefs, patch: &Patch, linked: bool) -> adw::PreferencesGroup {
    let more = adw::ExpanderRow::builder().title("More options").build();
    more.add_row(&switch_row(
        (
            "Read dates in titles",
            "Words like “tomorrow 5pm” or “every monday” set the date; off, they stay in the title",
        ),
        p.read_dates,
        patch,
        PrefPatch::ReadDates,
    ));
    let names: Vec<&str> = (1..=4).map(model::priority_name).collect();
    let priority = adw::ComboRow::builder()
        .title("Default priority")
        .subtitle("For a task added without one")
        .model(&gtk::StringList::new(&names))
        .selected(u32::from(p.default_priority.clamp(1, 4) - 1))
        .build();
    {
        let patch = patch.clone();
        priority.connect_selected_notify(move |c| {
            patch(PrefPatch::DefaultPriority(c.selected() as u8 + 1));
        });
    }
    more.add_row(&priority);
    more.add_row(&switch_row(
        (
            "Quick add remembers the list",
            "Start in the list the last task went to, instead of the inbox",
        ),
        p.remember_list,
        patch,
        PrefPatch::RememberList,
    ));
    if linked {
        github_rows(&more, p, patch);
    }
    let group = adw::PreferencesGroup::new();
    group.add(&more);
    group
}

const FOLDER_NOTE: &str = "A folder holding one checkout per repo, named for it";

/// A dim line under an entry row, which has no subtitle of its own.
fn note_row(text: &str) -> (gtk::ListBoxRow, gtk::Label) {
    let note = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .margin_top(6)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["caption", "dim-label"])
        .build();
    let row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&note)
        .build();
    (row, note)
}

/// Where checkouts are, and what Start work runs for a task's issue; for
/// the lists tied to a repo (`asst link`). Both apply as they are typed.
fn github_rows(more: &adw::ExpanderRow, p: &Prefs, patch: &Patch) {
    let folder = adw::EntryRow::builder()
        .title("Projects folder")
        .text(&p.projects_dir)
        .build();
    let (folder_note_row, folder_note) = note_row(FOLDER_NOTE);
    // A folder that isn't there is said, but kept: it may be a mount.
    let check = {
        let (folder, note) = (folder.downgrade(), folder_note.clone());
        move || {
            let Some(folder) = folder.upgrade() else {
                return;
            };
            let home = std::env::var("HOME").unwrap_or_default();
            let found = model::projects_dir(&folder.text(), &home).is_dir();
            for w in [folder.upcast_ref::<gtk::Widget>(), note.upcast_ref()] {
                if found {
                    w.remove_css_class("error");
                } else {
                    w.add_css_class("error");
                }
            }
            note.set_label(if found {
                FOLDER_NOTE
            } else {
                "Folder not found"
            });
        }
    };
    check();
    {
        let patch = patch.clone();
        folder.connect_changed(move |e| {
            check();
            patch(PrefPatch::ProjectsDir(e.text().trim().to_string()));
        });
    }
    let choose = gtk::Button::from_icon_name("folder-symbolic");
    choose.set_valign(gtk::Align::Center);
    choose.add_css_class("flat");
    choose.set_tooltip_text(Some("Choose a folder"));
    {
        let folder = folder.downgrade();
        choose.connect_clicked(move |b| {
            let Some(folder) = folder.upgrade() else {
                return;
            };
            let home = std::env::var("HOME").unwrap_or_default();
            let dialog = gtk::FileDialog::builder().title("Projects folder").build();
            dialog.set_initial_folder(Some(&gio::File::for_path(model::projects_dir(
                &folder.text(),
                &home,
            ))));
            let window = b.root().and_downcast::<gtk::Window>();
            dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |picked| {
                if let Some(path) = picked.ok().and_then(|f| f.path()) {
                    folder.set_text(&autostart::tilde(&path));
                }
            });
        });
    }
    folder.add_suffix(&choose);
    more.add_row(&folder);
    more.add_row(&folder_note_row);

    let command = adw::EntryRow::builder()
        .title("Start work command")
        .text(&p.work_command)
        .build();
    {
        let patch = patch.clone();
        command.connect_changed(move |e| {
            patch(PrefPatch::WorkCommand(e.text().trim().to_string()));
        });
    }
    more.add_row(&command);
    more.add_row(&note_row("Placeholders: {dir} {number} {repo} {url}").0);
}
