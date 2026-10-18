//! Preferences: the account, what the daemon does (inbox, reminders, how
//! often it checks, where linked notes are), and how the window behaves. The
//! sidebar is edited on the sidebar, not here.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use asst_core::api::{ListView, Settings, SettingsChange, StatusView, SyncState};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib};

use crate::model;
use crate::prefs::{PrefPatch, Prefs};
use crate::window::{Msg, Tx};
use crate::{autostart, pickers};

pub struct Ctx {
    pub settings: Option<Settings>,
    pub lists: Vec<ListView>,
    /// The daemon's status as the window last heard it, kept current.
    pub status: Rc<RefCell<Option<StatusView>>>,
    pub prefs: Prefs,
}

/// Sends one change to the window, which applies it to its own `Prefs`. The
/// dialog's copy (`Ctx::prefs`) only says what the controls start at.
type Patch = Rc<dyn Fn(PrefPatch)>;

const INTERVALS: [(u64, &str); 5] = [
    (30, "Every 30 seconds"),
    (60, "Every minute"),
    (120, "Every 2 minutes"),
    (300, "Every 5 minutes"),
    (900, "Every 15 minutes"),
];

pub fn open(parent: &impl IsA<gtk::Widget>, ctx: Ctx, tx: Tx) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(false);
    let patch: Patch = {
        let tx = tx.clone();
        Rc::new(move |p| tx.emit(Msg::PatchPrefs(p)))
    };
    dialog.add(&general_page(&dialog, &ctx, &tx, &patch));
    dialog.present(Some(parent));
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

fn general_page(
    dialog: &adw::PreferencesDialog,
    ctx: &Ctx,
    tx: &Tx,
    patch: &Patch,
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
        page.add(&snooze_group(&settings, send));
    } else {
        tasks.set_description(Some(
            "asstd isn't answering, so its settings can't be shown.",
        ));
        page.add(&tasks);
    }

    // -- the window's own
    let p = &ctx.prefs;
    let adding = adw::PreferencesGroup::builder().title("New Tasks").build();
    adding.add(&switch_row(
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
        .title("Priority")
        .subtitle("When a task is added without one")
        .model(&gtk::StringList::new(&names))
        .selected(u32::from(p.default_priority.clamp(1, 4) - 1))
        .build();
    {
        let patch = patch.clone();
        priority.connect_selected_notify(move |c| {
            patch(PrefPatch::DefaultPriority(c.selected() as u8 + 1));
        });
    }
    adding.add(&priority);
    adding.add(&switch_row(
        (
            "Quick add remembers the list",
            "Start in the list the last task went to, instead of the inbox",
        ),
        p.remember_list,
        patch,
        PrefPatch::RememberList,
    ));
    page.add(&adding);

    let window_group = adw::PreferencesGroup::builder().title("Window").build();
    window_group.add(&switch_row(
        (
            "Complete after a moment",
            "A checked task is struck through first, so a slip can be unchecked",
        ),
        p.complete_later,
        patch,
        PrefPatch::CompleteLater,
    ));
    window_group.add(&switch_row(
        ("Week starts on Sunday", "In the date picker"),
        p.sunday_first,
        patch,
        PrefPatch::SundayFirst,
    ));
    page.add(&window_group);
    page.add(&github_group(p, patch));
    page.add(&background_group(dialog, p.background, patch));
    page
}

/// Where checkouts are, and what Start work runs for a task's issue.
fn github_group(p: &Prefs, patch: &Patch) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("GitHub")
        .description("For tasks in lists linked to a repo (asst link)")
        .build();
    let entry = |title: &str, text: &str, make: fn(String) -> PrefPatch| {
        let row = adw::EntryRow::builder()
            .title(title)
            .text(text)
            .show_apply_button(true)
            .build();
        let patch = patch.clone();
        row.connect_apply(move |e| patch(make(e.text().trim().to_string())));
        row
    };
    group.add(&entry(
        "Projects folder (a checkout per repo, named for it)",
        &p.projects_dir,
        PrefPatch::ProjectsDir,
    ));
    group.add(&entry(
        "Start work runs ({dir} {number} {repo} {url})",
        &p.work_command,
        PrefPatch::WorkCommand,
    ));
    group
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
            "The buttons on a reminder, up to {}. Snooze… beside them asks for any other time.",
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

/// "Run in background" and "Run on startup", in the tray.
fn background_group(
    dialog: &adw::PreferencesDialog,
    on: bool,
    patch: &Patch,
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
        patch,
        PrefPatch::Background,
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
