//! Preferences: the account, what the daemon does (inbox, reminders, how
//! often it checks, where linked notes are), how the window behaves, and
//! which views the sidebar shows in what order.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use asst_core::api::{ListView, Settings, SettingsChange, StatusView, SyncState};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, gio, glib};

use crate::model::{self, Nav};
use crate::prefs::Prefs;
use crate::window::{Msg, Tx};
use crate::{autostart, linked, pickers};

pub struct Ctx {
    pub settings: Option<Settings>,
    pub lists: Vec<ListView>,
    /// The daemon's status as the window last heard it, kept current.
    pub status: Rc<RefCell<Option<StatusView>>>,
    pub prefs: Prefs,
    /// `sidebar` to open on that page.
    pub page: Option<&'static str>,
}

/// Sets one of the window's preferences from a switch.
type SetPref = Box<dyn Fn(&mut Prefs, bool)>;

const INTERVALS: [(u64, &str); 5] = [
    (30, "Every 30 seconds"),
    (60, "Every minute"),
    (120, "Every 2 minutes"),
    (300, "Every 5 minutes"),
    (900, "Every 15 minutes"),
];

/// What a dragged view row carries.
const VIEW_PREFIX: &str = "asst-view:";

pub fn open(parent: &impl IsA<gtk::Widget>, ctx: Ctx, tx: Tx) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(false);
    let prefs = Rc::new(RefCell::new(ctx.prefs.clone()));
    let save: Rc<dyn Fn()> = {
        let (prefs, tx) = (prefs.clone(), tx.clone());
        Rc::new(move || tx.emit(Msg::SetPrefs(Box::new(prefs.borrow().clone()))))
    };
    dialog.add(&general_page(&dialog, &ctx, &tx, &prefs, &save));
    dialog.add(&sidebar_page(&ctx.lists, &prefs, &save));
    if let Some(page) = ctx.page {
        dialog.set_visible_page_name(page);
    }
    dialog.present(Some(parent));
}

fn switch_row(
    (title, subtitle): (&str, &str),
    on: bool,
    prefs: &Rc<RefCell<Prefs>>,
    save: &Rc<dyn Fn()>,
    set: SetPref,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(on)
        .build();
    let (prefs, save) = (prefs.clone(), save.clone());
    row.connect_active_notify(move |r| {
        set(&mut prefs.borrow_mut(), r.is_active());
        save();
    });
    row
}

fn general_page(
    dialog: &adw::PreferencesDialog,
    ctx: &Ctx,
    tx: &Tx,
    prefs: &Rc<RefCell<Prefs>>,
    save: &Rc<dyn Fn()>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .name("general")
        .icon_name("settings-symbolic")
        .build();

    // -- account
    let account = adw::PreferencesGroup::builder()
        .title("Account")
        .description("Tasks live on Nextcloud, which Apple Reminders on the iPhone syncs with too.")
        .build();
    let status = ctx.status.borrow().clone();
    match status.filter(|s| s.state != SyncState::NoAccount) {
        Some(s) => {
            let who = adw::ActionRow::builder()
                .title(s.server.as_deref().unwrap_or("Nextcloud"))
                .subtitle(s.username.as_deref().unwrap_or(""))
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
                        .body("asst forgets its app password and its copy of your tasks. They stay on Nextcloud and your other devices.")
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
            account.add(&who);

            let sync = adw::ActionRow::builder().title("Sync").build();
            let now = gtk::Button::with_label("Sync Now");
            now.set_valign(gtk::Align::Center);
            let tx2 = tx.clone();
            now.connect_clicked(move |_| tx2.emit(Msg::SyncNow));
            sync.add_suffix(&now);
            account.add(&sync);
            // How long ago keeps counting, and the daemon's rounds show,
            // until the dialog is closed.
            sync.set_subtitle(&model::sync_state(&s, pickers::now()));
            let status = ctx.status.clone();
            let row = sync.downgrade();
            glib::timeout_add_local(Duration::from_secs(1), move || {
                let Some(row) = row.upgrade().filter(|r| r.root().is_some()) else {
                    return glib::ControlFlow::Break;
                };
                if let Some(s) = status.borrow().as_ref() {
                    row.set_subtitle(&model::sync_state(s, pickers::now()));
                }
                glib::ControlFlow::Continue
            });
        }
        None => {
            let server = adw::EntryRow::builder()
                .title("Server, like cloud.example.com")
                .show_apply_button(true)
                .build();
            let (tx2, dialog2) = (tx.clone(), dialog.downgrade());
            server.connect_apply(move |e| {
                let s = e.text().trim().to_string();
                if !s.is_empty() {
                    tx2.emit(Msg::SignIn(s));
                    if let Some(d) = dialog2.upgrade() {
                        d.close();
                    }
                }
            });
            account.add(&server);
        }
    }
    page.add(&account);

    // -- the daemon's settings
    let tasks = adw::PreferencesGroup::builder().title("Tasks").build();
    let send = {
        let tx = tx.clone();
        move |c: SettingsChange| tx.emit(Msg::SetSettings(Box::new(c)))
    };
    if let Some(settings) = ctx.settings.clone() {
        let writable: Vec<&ListView> = ctx.lists.iter().filter(|l| l.writable).collect();
        if !writable.is_empty() {
            let names: Vec<&str> = writable.iter().map(|l| l.name.as_str()).collect();
            let current = writable.iter().position(|l| l.inbox).unwrap_or(0);
            let inbox = adw::ComboRow::builder()
                .title("Inbox")
                .subtitle("Where a task goes when no list is named")
                .model(&gtk::StringList::new(&names))
                .selected(current as u32)
                .build();
            let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
            let send = send.clone();
            inbox.connect_selected_notify(move |c| {
                if let Some(name) = names.get(c.selected() as usize) {
                    send(SettingsChange {
                        inbox: Some(Some(name.clone())),
                        ..SettingsChange::default()
                    });
                }
            });
            tasks.add(&inbox);
        }

        tasks.add(&reminder_row(&settings, send.clone()));

        let labels: Vec<&str> = INTERVALS.iter().map(|(_, l)| *l).collect();
        let current = INTERVALS
            .iter()
            .position(|(s, _)| *s == settings.interval)
            .unwrap_or(1);
        let interval = adw::ComboRow::builder()
            .title("Check for changes")
            .subtitle("How often to look for edits made on other devices")
            .model(&gtk::StringList::new(&labels))
            .selected(current as u32)
            .build();
        {
            let send = send.clone();
            interval.connect_selected_notify(move |c| {
                if let Some((secs, _)) = INTERVALS.get(c.selected() as usize) {
                    send(SettingsChange {
                        interval: Some(*secs),
                        ..SettingsChange::default()
                    });
                }
            });
        }
        tasks.add(&interval);
        page.add(&tasks);
        page.add(&snooze_group(&settings, send.clone()));
        page.add(&notes_group(&settings, send));
    } else {
        tasks.set_description(Some(
            "asstd isn't answering, so its settings can't be shown.",
        ));
        page.add(&tasks);
    }

    // -- the window's own
    let p = prefs.borrow().clone();
    let adding = adw::PreferencesGroup::builder().title("New Tasks").build();
    adding.add(&switch_row(
        (
            "Read dates in titles",
            "Words like “tomorrow 5pm” or “every monday” set the date; off, they stay in the title",
        ),
        p.read_dates,
        prefs,
        save,
        Box::new(|p, on| p.read_dates = on),
    ));
    let names: Vec<&str> = (1..=4).map(model::priority_name).collect();
    let priority = adw::ComboRow::builder()
        .title("Priority")
        .subtitle("When a task is added without one")
        .model(&gtk::StringList::new(&names))
        .selected(u32::from(p.default_priority.clamp(1, 4) - 1))
        .build();
    {
        let (prefs, save) = (prefs.clone(), save.clone());
        priority.connect_selected_notify(move |c| {
            prefs.borrow_mut().default_priority = c.selected() as u8 + 1;
            save();
        });
    }
    adding.add(&priority);
    adding.add(&switch_row(
        (
            "Quick add remembers the list",
            "Start in the list the last task went to, instead of the inbox",
        ),
        p.remember_list,
        prefs,
        save,
        Box::new(|p, on| p.remember_list = on),
    ));
    page.add(&adding);

    let window_group = adw::PreferencesGroup::builder().title("Window").build();
    window_group.add(&switch_row(
        (
            "Complete after a moment",
            "A checked task is struck through first, so a slip can be unchecked",
        ),
        p.complete_later,
        prefs,
        save,
        Box::new(|p, on| p.complete_later = on),
    ));
    window_group.add(&switch_row(
        ("Week starts on Sunday", "In the date picker"),
        p.sunday_first,
        prefs,
        save,
        Box::new(|p, on| p.sunday_first = on),
    ));
    page.add(&window_group);
    page.add(&background_group(dialog, p.background, prefs, save));
    page
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

fn reminder_row(settings: &Settings, send: impl Fn(SettingsChange) + 'static) -> adw::ComboRow {
    let mut times: Vec<(Option<u32>, String)> = REMINDER_TIMES
        .iter()
        .map(|(m, l)| (*m, l.to_string()))
        .collect();
    let current = settings.alarm_at_due.then_some(settings.alarm_before);
    if !times.iter().any(|(m, _)| *m == current) {
        // Set some other way (`asst settings`): shown as it is.
        let minutes = settings.alarm_before;
        times.push((
            current,
            format!("{} before", asst_core::fmt::minutes(u64::from(minutes))),
        ));
    }
    let labels: Vec<&str> = times.iter().map(|(_, l)| l.as_str()).collect();
    let row = adw::ComboRow::builder()
        .title("Reminder")
        .subtitle("For a task with a time, as on the iPhone")
        .model(&gtk::StringList::new(&labels))
        .selected(times.iter().position(|(m, _)| *m == current).unwrap_or(0) as u32)
        .build();
    row.connect_selected_notify(move |c| {
        let Some((minutes, _)) = times.get(c.selected() as usize) else {
            return;
        };
        send(SettingsChange {
            alarm_at_due: Some(minutes.is_some()),
            alarm_before: *minutes,
            ..SettingsChange::default()
        });
    });
    row
}

/// A reminder's Snooze buttons: lengths to pick, up to three.
fn snooze_group(
    settings: &Settings,
    send: impl Fn(SettingsChange) + 'static,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Snooze")
        .description(format!(
            "The buttons on a reminder, up to {}",
            asst_core::config::MAX_SNOOZE
        ))
        .build();
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
    let send = Rc::new(send);
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
        let (chosen, send, limit) = (chosen.clone(), send.clone(), limit.clone());
        b.connect_toggled(move |b| {
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
            send(SettingsChange {
                snooze: Some(v),
                ..SettingsChange::default()
            });
        });
        chips.append(&b);
        buttons.borrow_mut().push((m, b));
    }
    limit();
    group.add(&chips);
    group
}

/// The folder of notes tasks link to.
fn notes_group(
    settings: &Settings,
    send: impl Fn(SettingsChange) + 'static,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Notes")
        .description("A task can link to notes in the folder Nextcloud Notes keeps, as the Nextcloud client puts it on this computer.")
        .build();
    let dir = PathBuf::from(&settings.notes);
    let row = adw::ActionRow::builder().title("Notes folder").build();
    row.set_subtitle(&if dir.is_dir() {
        linked::pretty(&dir)
    } else {
        format!("{} doesn't exist", linked::pretty(&dir))
    });
    let choose = gtk::Button::with_label("Choose…");
    choose.set_valign(gtk::Align::Center);
    let send = Rc::new(send);
    {
        let weak = row.downgrade();
        choose.connect_clicked(move |b| {
            let picker = gtk::FileDialog::builder()
                .title("Notes Folder")
                .modal(true)
                .build();
            if dir.is_dir() {
                picker.set_initial_folder(Some(&gio::File::for_path(&dir)));
            }
            let (send, weak) = (send.clone(), weak.clone());
            picker.select_folder(
                b.root().and_downcast::<gtk::Window>().as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    let Some(path) = result.ok().and_then(|f| f.path()) else {
                        return;
                    };
                    if let Some(row) = weak.upgrade() {
                        row.set_subtitle(&linked::pretty(&path));
                    }
                    send(SettingsChange {
                        notes: Some(Some(path.to_string_lossy().into_owned())),
                        ..SettingsChange::default()
                    });
                },
            );
        });
    }
    row.add_suffix(&choose);
    row.set_activatable_widget(Some(&choose));
    group.add(&row);
    group
}

/// Lists put away, each with a way back.
fn archived_group(
    lists: &[ListView],
    prefs: &Rc<RefCell<Prefs>>,
    save: &Rc<dyn Fn()>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Archived Lists")
        .description("Out of the sidebar, the views and the counts. They still sync, and their reminders still ring.")
        .build();
    let archived: Vec<&ListView> = lists
        .iter()
        .filter(|l| prefs.borrow().is_archived(&l.href))
        .collect();
    let none = adw::ActionRow::builder()
        .title("None")
        .subtitle("Archive a list from its menu")
        .visible(archived.is_empty())
        .build();
    none.add_css_class("dim-label");
    group.add(&none);
    for l in archived {
        let row = adw::ActionRow::builder().title(&l.name).build();
        row.add_prefix(&crate::ui::ring(l.color.as_deref(), 16));
        let restore = gtk::Button::with_label("Unarchive");
        restore.set_valign(gtk::Align::Center);
        let (prefs, save, href) = (prefs.clone(), save.clone(), l.href.clone());
        let (row_weak, none) = (row.downgrade(), none.clone());
        restore.connect_clicked(move |_| {
            prefs.borrow_mut().archived.retain(|h| *h != href);
            save();
            if let Some(row) = row_weak.upgrade() {
                row.set_visible(false);
            }
            none.set_visible(prefs.borrow().archived.is_empty());
        });
        row.add_suffix(&restore);
        group.add(&row);
    }
    group
}

/// Planify's "Run in background" and "Run on startup", in the tray.
fn background_group(
    dialog: &adw::PreferencesDialog,
    on: bool,
    prefs: &Rc<RefCell<Prefs>>,
    save: &Rc<dyn Fn()>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Background")
        .description("Sync and reminders don't need the window: asstd runs on its own.")
        .build();
    let background = switch_row(
        (
            "Run in background",
            "Keep asst in the tray when the window closes",
        ),
        on,
        prefs,
        save,
        Box::new(|p, on| p.background = on),
    );
    group.add(&background);

    let by_compositor = autostart::started_by_compositor();
    // A line in the compositor's config is the user's: the switch reports it
    // and leaves it be.
    let switchable = by_compositor.is_none();
    let login = adw::SwitchRow::builder()
        .title("Start at login")
        .active(autostart::is_enabled())
        .sensitive(on && switchable)
        .build();
    match by_compositor {
        Some(config) => login.set_subtitle(&format!(
            "Started by {}",
            glib::markup_escape_text(&autostart::tilde(&config))
        )),
        None if autostart::session_reads_autostart() => {
            login.set_subtitle("Open in the tray when you log in");
        }
        None => login.set_subtitle(&format!(
            "This desktop doesn't run autostart entries: start <tt>{}</tt> from its config",
            autostart::COMMAND
        )),
    }
    {
        let dialog = dialog.downgrade();
        login.connect_active_notify(move |row| {
            if let Err(e) = autostart::set(row.is_active())
                && let Some(d) = dialog.upgrade()
            {
                d.add_toast(adw::Toast::new(&glib::markup_escape_text(&format!(
                    "Couldn't change {}: {e}",
                    autostart::tilde(&autostart::entry())
                ))));
            }
        });
    }
    {
        let login = login.clone();
        background.connect_active_notify(move |b| login.set_sensitive(b.is_active() && switchable));
    }
    group.add(&login);
    group
}

/// Planify's sidebar page: every view with a switch, dragged into order;
/// how lists are ordered, and the ones archived.
fn sidebar_page(
    lists: &[ListView],
    prefs: &Rc<RefCell<Prefs>>,
    save: &Rc<dyn Fn()>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Sidebar")
        .name("sidebar")
        .icon_name("dock-left-symbolic")
        .build();

    let general = adw::PreferencesGroup::new();
    let counts = prefs.borrow().show_counts;
    general.add(&switch_row(
        ("Show task counts", "Beside views and lists"),
        counts,
        prefs,
        save,
        Box::new(|p, on| p.show_counts = on),
    ));
    let order = adw::ComboRow::builder()
        .title("Lists")
        .subtitle("Drag a list in the sidebar to move it")
        .model(&gtk::StringList::new(&["In the order dragged", "By name"]))
        .selected(u32::from(prefs.borrow().lists_by_name))
        .build();
    {
        let (prefs, save) = (prefs.clone(), save.clone());
        order.connect_selected_notify(move |c| {
            prefs.borrow_mut().lists_by_name = c.selected() == 1;
            save();
        });
    }
    general.add(&order);
    page.add(&general);
    page.add(&archived_group(lists, prefs, save));

    let group = adw::PreferencesGroup::builder()
        .title("Views")
        .description("Shown above your lists. Drag a view to move it.")
        .build();
    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    listbox.add_css_class("boxed-list");
    listbox.add_css_class("sidebar-views");
    group.add(&listbox);
    page.add(&group);

    let shown = prefs.borrow().sidebar_views();
    let mut order = shown.clone();
    order.extend(Nav::filters().into_iter().filter(|n| !shown.contains(n)));

    type ViewRow = (Nav, adw::ActionRow, gtk::Switch);
    let rows: Rc<RefCell<Vec<ViewRow>>> = Rc::default();
    // The switched-on views, in the order the rows are in.
    let store: Rc<dyn Fn()> = {
        let (listbox, rows, prefs, save) =
            (listbox.clone(), rows.clone(), prefs.clone(), save.clone());
        Rc::new(move || {
            let visible: Vec<Nav> = {
                let rows = rows.borrow();
                let mut out = Vec::new();
                let mut i = 0;
                while let Some(r) = listbox.row_at_index(i) {
                    if let Some((nav, _, switch)) = rows
                        .iter()
                        .find(|(_, row, _)| row.upcast_ref::<gtk::ListBoxRow>() == &r)
                        && switch.is_active()
                    {
                        out.push(nav.clone());
                    }
                    i += 1;
                }
                out
            };
            prefs.borrow_mut().set_sidebar_views(&visible);
            save();
        })
    };

    for nav in order {
        let row = adw::ActionRow::builder()
            .title(nav.title(&[]))
            .subtitle(nav.about())
            .build();
        // Prefixes stack leftward: the handle goes in last to sit first.
        let icon = gtk::Image::from_icon_name(nav.icon());
        icon.add_css_class(nav.tint());
        row.add_prefix(&icon);
        let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        handle.add_css_class("dim-label");
        row.add_prefix(&handle);
        let switch = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(shown.contains(&nav))
            .build();
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        {
            let store = store.clone();
            switch.connect_active_notify(move |_| store());
        }

        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        let payload = format!("{VIEW_PREFIX}{}", nav.key());
        drag.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(&payload.to_value()))
        });
        drag.connect_drag_begin(|src, _| {
            if let Some(w) = src.widget() {
                src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&w))), 24, 24);
            }
        });
        row.add_controller(drag);

        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let lower_half = |t: &gtk::DropTarget, y: f64| {
            t.widget().is_some_and(|w| y > f64::from(w.height()) / 2.0)
        };
        target.connect_motion(move |t, _, y| {
            if let Some(w) = t.widget() {
                let below = lower_half(t, y);
                w.remove_css_class(if below { "drop-above" } else { "drop-below" });
                w.add_css_class(if below { "drop-below" } else { "drop-above" });
            }
            gdk::DragAction::MOVE
        });
        target.connect_leave(|t| {
            if let Some(w) = t.widget() {
                w.remove_css_class("drop-above");
                w.remove_css_class("drop-below");
            }
        });
        {
            let (listbox, rows, store) = (listbox.clone(), rows.clone(), store.clone());
            target.connect_drop(move |t, value, _, y| {
                let Some(on) = t.widget().and_downcast::<gtk::ListBoxRow>() else {
                    return false;
                };
                on.remove_css_class("drop-above");
                on.remove_css_class("drop-below");
                let Some(key) = value
                    .get::<String>()
                    .ok()
                    .and_then(|v| v.strip_prefix(VIEW_PREFIX).map(str::to_string))
                else {
                    return false;
                };
                let moved = rows
                    .borrow()
                    .iter()
                    .find(|(n, _, _)| n.key() == key)
                    .map(|(_, r, _)| r.clone());
                let Some(moved) = moved.filter(|m| m.upcast_ref::<gtk::ListBoxRow>() != &on) else {
                    return false;
                };
                listbox.remove(&moved);
                listbox.insert(&moved, on.index() + i32::from(lower_half(t, y)));
                store();
                true
            });
        }
        row.add_controller(target);
        listbox.append(&row);
        rows.borrow_mut().push((nav, row, switch));
    }
    page
}
