//! A task opened in place, as Planify opens one: its row grows into a card
//! with the title to edit, notes under it, and a bar of buttons for the
//! date, list, priority and reminders. There is one editor, and each rebuild
//! of the view moves it into the open task's row, so what is being typed
//! survives. Typing saves after a pause; buttons save as you pick. It starts
//! out the size of the row it replaces, then grows (`expand`), and shrinks
//! back before the row returns (`collapse`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use asst_core::api::{Change, ListView, TaskView};
use asst_core::nominatim::Place;
use asst_core::task::priority_level;
use asst_core::task::LocationAlarm;
use asst_core::fmt;
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::model::{self, capitalize};
use crate::motion;
use crate::pickers::{self, DateOpts, Schedule};
use crate::row::schedule_change;
use crate::ui::{self, Item};
use crate::window::{Msg, Tx};

pub struct Editor {
    pub root: gtk::Box,
    /// The notes and the buttons, under the title.
    details: gtk::Revealer,
    /// Counts openings, so a grow still waiting for its frame doesn't undo
    /// a close that came first.
    opened: Rc<Cell<u64>>,
    tx: Tx,
    task: RefCell<Option<TaskView>>,
    lists: RefCell<Vec<ListView>>,
    sunday_first: Cell<bool>,
    check: gtk::CheckButton,
    title: gtk::TextView,
    notes: gtk::TextView,
    date: gtk::MenuButton,
    date_icon: gtk::Image,
    date_label: gtk::Label,
    date_clear: gtk::Revealer,
    list: gtk::MenuButton,
    list_ring: gtk::Box,
    list_label: gtk::Label,
    priority: gtk::MenuButton,
    priority_icon: gtk::Image,
    reminders: gtk::MenuButton,
    reminder_dot: gtk::Widget,
    menu: gtk::MenuButton,
    /// The place picker's shared state: its results list, and the places
    /// found so far.
    place_results: pickers::PlaceSlot,
    places: Rc<RefCell<Vec<Place>>>,
    /// Setting fields from the task, not the user changing them.
    loading: Cell<bool>,
    title_timer: RefCell<Option<glib::SourceId>>,
    notes_timer: RefCell<Option<glib::SourceId>>,
}

fn text_of(view: &gtk::TextView) -> String {
    let b = view.buffer();
    b.text(&b.start_iter(), &b.end_iter(), false).to_string()
}

/// A flat icon button with Planify's dot for "this has a value".
fn dotted(icon: &str, tooltip: &str) -> (gtk::MenuButton, gtk::Overlay, gtk::Widget) {
    let button = gtk::MenuButton::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build();
    button.add_css_class("flat");
    let dot = gtk::Box::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .can_target(false)
        .visible(false)
        .build();
    dot.add_css_class("indicator");
    let overlay = gtk::Overlay::builder().child(&button).build();
    overlay.add_overlay(&dot);
    (button, overlay, dot.upcast())
}

impl Editor {
    pub fn new(tx: Tx) -> Rc<Editor> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("task-editor");

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Start)
            .build();
        check.add_css_class("task-check");
        check.add_css_class("editor-check");
        top.append(&check);
        let title = ui::text_view("Task name");
        title.add_css_class("editor-title");
        top.append(&title);
        let collapse = gtk::Button::from_icon_name("go-up-symbolic");
        collapse.add_css_class("flat");
        collapse.add_css_class("editor-collapse");
        collapse.set_valign(gtk::Align::Start);
        ui::tip(&collapse, "Close", "Esc");
        top.append(&collapse);
        root.append(&top);

        let below = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let details = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(motion::ROW_MS)
            .child(&below)
            .build();
        root.append(&details);

        let notes = ui::text_view("Notes");
        notes.add_css_class("editor-notes");
        crate::notes::enhance(&notes);
        below.append(&notes);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        actions.add_css_class("editor-actions");

        let date_icon = gtk::Image::from_icon_name("month-symbolic");
        // When narrow, the list's name gives way before the date does.
        let date_label = gtk::Label::builder()
            .label("Date")
            .width_chars(9)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let date_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        date_content.append(&date_icon);
        date_content.append(&date_label);
        let date = gtk::MenuButton::builder().child(&date_content).build();
        date.add_css_class("flat");
        date.set_tooltip_text(Some("Date"));
        let clear = gtk::Button::from_icon_name("cross-large-circle-filled-symbolic");
        clear.add_css_class("flat");
        clear.add_css_class("circular");
        clear.add_css_class("editor-clear");
        clear.set_valign(gtk::Align::Center);
        clear.set_tooltip_text(Some("Remove the date"));
        // Sliding, so it takes no room until it shows.
        let date_clear = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .child(&clear)
            .build();
        let date_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        date_box.append(&date);
        date_box.append(&date_clear);
        actions.append(&date_box);

        let list_ring = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let list_label = gtk::Label::builder()
            .max_width_chars(18)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let list_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        list_content.append(&list_ring);
        list_content.append(&list_label);
        let list = gtk::MenuButton::builder().child(&list_content).build();
        list.add_css_class("flat");
        list.set_tooltip_text(Some("Move to a list"));

        let right = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        right.set_hexpand(true);
        right.set_halign(gtk::Align::End);
        right.append(&list);
        let priority_icon = gtk::Image::from_icon_name("flag-outline-thick-symbolic");
        let priority = gtk::MenuButton::builder()
            .child(&priority_icon)
            .tooltip_text("Priority")
            .build();
        priority.add_css_class("flat");
        right.append(&priority);
        let (reminders, reminder_box, reminder_dot) = dotted("alarm-symbolic", "Reminders");
        right.append(&reminder_box);
        let menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .build();
        menu.add_css_class("flat");
        right.append(&menu);
        actions.append(&right);
        below.append(&actions);

        let e = Rc::new(Editor {
            root,
            details,
            opened: Rc::default(),
            tx,
            task: RefCell::default(),
            lists: RefCell::default(),
            sunday_first: Cell::new(false),
            check,
            title,
            notes,
            date,
            date_icon,
            date_label,
            date_clear,
            list,
            list_ring,
            list_label,
            priority,
            priority_icon,
            reminders,
            reminder_dot,
            menu,
            place_results: Rc::default(),
            places: Rc::default(),
            loading: Cell::new(false),
            title_timer: RefCell::default(),
            notes_timer: RefCell::default(),
        });
        e.connect(&collapse, &clear, &date_box);
        e
    }

    /// Something for a closure to reach the editor by, without keeping it.
    fn with(self: &Rc<Self>, f: impl Fn(&Rc<Editor>) + 'static) -> impl Fn() + 'static {
        let weak = Rc::downgrade(self);
        move || {
            if let Some(e) = weak.upgrade() {
                f(&e);
            }
        }
    }

    fn connect(self: &Rc<Self>, collapse: &gtk::Button, clear: &gtk::Button, date_box: &gtk::Box) {
        {
            let close = self.with(|e| e.tx.emit(Msg::CloseEditor));
            collapse.connect_clicked(move |_| close());
        }
        {
            let weak = Rc::downgrade(self);
            self.check.connect_toggled(move |c| {
                if let Some(e) = weak.upgrade()
                    && !e.loading.get()
                    && let Some(href) = e.href()
                {
                    e.tx.emit(Msg::Check(href, c.is_active()));
                }
            });
        }

        // Title: saved a moment after typing stops; Enter saves and closes,
        // Tab goes on to the notes.
        {
            let schedule = self.with(|e| {
                if !e.loading.get() {
                    e.schedule_title();
                }
            });
            self.title.buffer().connect_changed(move |_| schedule());
        }
        {
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(e) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match key {
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        e.save_title();
                        e.tx.emit(Msg::CloseEditor);
                    }
                    gdk::Key::Tab if !state.contains(gdk::ModifierType::SHIFT_MASK) => {
                        e.notes.grab_focus();
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            self.title.add_controller(keys);
        }
        {
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, _| {
                // Shift+Tab reads as ISO_Left_Tab.
                if key == gdk::Key::ISO_Left_Tab
                    && let Some(e) = weak.upgrade()
                {
                    e.focus();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            self.notes.add_controller(keys);
        }
        {
            // The pickers, from the keyboard.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(e) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if !state.contains(gdk::ModifierType::CONTROL_MASK) || !e.date.is_sensitive() {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gdk::Key::d => e.date.popup(),
                    gdk::Key::r => e.reminders.popup(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            self.root.add_controller(keys);
        }
        {
            let schedule = self.with(|e| {
                if !e.loading.get() {
                    e.schedule_notes();
                }
            });
            self.notes.buffer().connect_changed(move |_| schedule());
        }
        // The view holds off rebuilding while a field is typed in; leaving
        // one lets it catch up.
        for view in [&self.title, &self.notes] {
            let focus = gtk::EventControllerFocus::new();
            let tx = self.tx.clone();
            focus.connect_leave(move |_| tx.emit(Msg::Resume));
            view.add_controller(focus);
        }

        {
            let clear_date = self.with(|e| {
                if let Some(t) = e.task.borrow().clone() {
                    e.tx.emit(Msg::Edit(
                        t.href.clone(),
                        Box::new(Change {
                            due: Some(None),
                            rrule: Some(None),
                            ..Change::default()
                        }),
                    ));
                }
            });
            clear.connect_clicked(move |_| clear_date());
        }
        {
            // Planify's clear button shows while the pointer is over the date.
            let motion = gtk::EventControllerMotion::new();
            let enter = self.with(|e| {
                let dated = e
                    .task
                    .borrow()
                    .as_ref()
                    .is_some_and(|t| t.task.due.is_some());
                e.date_clear
                    .set_reveal_child(dated && e.date.is_sensitive());
            });
            motion.connect_enter(move |_, _, _| enter());
            let leave = self.with(|e| e.date_clear.set_reveal_child(false));
            motion.connect_leave(move |_| leave());
            date_box.add_controller(motion);
        }

        // Each button opens a fresh picker, seeded with the task as it is.
        {
            let weak = Rc::downgrade(self);
            self.date.set_create_popup_func(move |mb| {
                let Some(e) = weak.upgrade() else { return };
                let Some(t) = e.task.borrow().clone() else {
                    return;
                };
                let weak = Rc::downgrade(&e);
                mb.set_popover(Some(&pickers::date_popover(
                    Schedule::of(&t.task),
                    DateOpts {
                        sunday_first: e.sunday_first.get(),
                        full: true,
                        clear: false,
                    },
                    move |s| {
                        // The picker stays open over several changes; each
                        // one compares with the task as last shown.
                        let Some(e) = weak.upgrade() else { return };
                        let Some(current) = e.task.borrow().clone() else {
                            return;
                        };
                        if let Some(c) = schedule_change(&current, &s) {
                            e.tx.emit(Msg::Edit(current.href.clone(), Box::new(c)));
                        }
                    },
                )));
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.list.set_create_popup_func(move |mb| {
                let Some(e) = weak.upgrade() else { return };
                let Some(t) = e.task.borrow().clone() else {
                    return;
                };
                let tx = e.tx.clone();
                let lists = e.lists.borrow().clone();
                mb.set_popover(Some(&pickers::list_popover(
                    &lists,
                    Some(&t.list),
                    move |to| tx.emit(Msg::Move(t.href.clone(), to)),
                )));
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.priority.set_create_popup_func(move |mb| {
                let Some(e) = weak.upgrade() else { return };
                let Some(t) = e.task.borrow().clone() else {
                    return;
                };
                let tx = e.tx.clone();
                mb.set_popover(Some(&pickers::priority_popover(
                    priority_level(t.task.priority),
                    move |level| {
                        tx.emit(Msg::Edit(
                            t.href.clone(),
                            Box::new(Change {
                                priority: Some(level),
                                ..Change::default()
                            }),
                        ))
                    },
                )));
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.reminders.set_create_popup_func(move |mb| {
                let Some(e) = weak.upgrade() else { return };
                let Some(t) = e.task.borrow().clone() else {
                    return;
                };
                let tx = e.tx.clone();
                let href = t.href.clone();
                mb.set_popover(Some(&pickers::reminder_popover(
                    &t.task,
                    {
                        let tx = tx.clone();
                        move |triggers, locations| {
                            tx.emit(Msg::Edit(
                                href.clone(),
                                Box::new(Change {
                                    alarms: Some(triggers),
                                    location_alarms: locations,
                                    ..Change::default()
                                }),
                            ))
                        }
                    },
                    Some(pickers::PlaceSearch {
                        search: {
                            let tx = tx.clone();
                            Rc::new(move |query: String| tx.emit(Msg::PlaceSearch(query)))
                        },
                        picked: {
                            let weak = weak.clone();
                            Rc::new(move |place: Place| {
                                let Some(e) = weak.upgrade() else { return };
                                let Some(t) = e.task.borrow().clone() else { return };
                                let mut list = t.task.location_alarms.clone();
                                list.push(LocationAlarm {
                                    uid: String::new(),
                                    title: place.name,
                                    address: place.display_name,
                                    lat: place.lat,
                                    lon: place.lon,
                                    radius: 100,
                                });
                                e.tx.emit(Msg::Edit(
                                    t.href,
                                    Box::new(Change {
                                        location_alarms: Some(list),
                                        ..Change::default()
                                    }),
                                ));
                            })
                        },
                        results: e.place_results.clone(),
                        places: e.places.clone(),
                    }),
                )));
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.menu.set_create_popup_func(move |mb| {
                let Some(e) = weak.upgrade() else { return };
                let Some(t) = e.task.borrow().clone() else {
                    return;
                };
                mb.set_popover(Some(&e.task_menu(&t)));
            });
        }
    }

    /// Duplicate, copy, save, delete, and when it was made and changed.
    fn task_menu(&self, t: &TaskView) -> gtk::Popover {
        let tx = self.tx.clone();
        let mut items: Vec<gtk::Widget> = vec![
            {
                let (tx, h) = (tx.clone(), t.href.clone());
                Item::new(Some("tabs-stack-symbolic"), "Duplicate")
                    .build(move || tx.emit(Msg::Duplicate(h.clone())))
                    .upcast()
            },
            {
                let (tx, h) = (tx.clone(), t.href.clone());
                Item::new(Some("clipboard-symbolic"), "Copy to clipboard")
                    .build(move || tx.emit(Msg::Copy(h.clone())))
                    .upcast()
            },
            {
                let (tx, h) = (tx.clone(), t.href.clone());
                Item::new(Some("folder-download-symbolic"), "Save as .ics…")
                    .build(move || tx.emit(Msg::SaveIcs(h.clone())))
                    .upcast()
            },
            ui::separator(),
            {
                let (tx, h) = (tx.clone(), t.href.clone());
                Item::new(Some("user-trash-symbolic"), "Delete task")
                    .secondary("dd")
                    .danger()
                    .build(move || tx.emit(Msg::Delete(h.clone())))
                    .upcast()
            },
        ];
        let info = history(t);
        if !info.is_empty() {
            items.push(ui::separator());
            let label = ui::label(&info, &["caption", "dim-label", "menu-info"]);
            label.set_wrap(true);
            label.set_ellipsize(pango::EllipsizeMode::None);
            items.push(label.upcast());
        }
        ui::menu(&items)
    }

    pub fn href(&self) -> Option<String> {
        self.task.borrow().as_ref().map(|t| t.href.clone())
    }

    /// Whether the keyboard is in the title or the notes.
    pub fn typing(&self) -> bool {
        let focus = self.root.root().and_then(|r| r.focus());
        focus.is_some_and(|f| {
            f == *self.title.upcast_ref::<gtk::Widget>()
                || f == *self.notes.upcast_ref::<gtk::Widget>()
        })
    }

    /// Row-sized, at once: ready to be moved into the next task's row and
    /// grow there.
    pub fn fold(&self) {
        self.opened.set(self.opened.get() + 1);
        self.root.remove_css_class("expanded");
        self.details.set_transition_duration(0);
        self.details.set_reveal_child(false);
        self.details.set_transition_duration(motion::ROW_MS);
    }

    /// Grow into the card, once the row-sized editor has been drawn.
    pub fn expand(&self) {
        let opened = self.opened.get();
        let (root, details) = (self.root.clone(), self.details.clone());
        let now = self.opened.clone();
        motion::after_frame(&self.root, move || {
            if now.get() == opened {
                root.add_css_class("expanded");
                details.set_reveal_child(true);
            }
        });
    }

    /// Shrink back to the row's size. How long that takes, before the row
    /// can take the editor's place.
    pub fn collapse(&self) -> Duration {
        self.opened.set(self.opened.get() + 1);
        let shown = self.root.is_mapped() && self.root.has_css_class("expanded");
        self.root.remove_css_class("expanded");
        self.details.set_reveal_child(false);
        if shown {
            motion::lasts(motion::ROW_MS)
        } else {
            Duration::ZERO
        }
    }

    /// The title, with the cursor after the text.
    pub fn focus(&self) {
        let buffer = self.title.buffer();
        buffer.place_cursor(&buffer.end_iter());
        self.title.grab_focus();
    }

    fn schedule_title(self: &Rc<Self>) {
        if let Some(id) = self.title_timer.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        *self.title_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(800),
            move || {
                if let Some(e) = weak.upgrade() {
                    e.title_timer.take();
                    e.save_title();
                }
            },
        ));
    }

    fn schedule_notes(self: &Rc<Self>) {
        if let Some(id) = self.notes_timer.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        *self.notes_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(1000),
            move || {
                if let Some(e) = weak.upgrade() {
                    e.notes_timer.take();
                    e.save_notes();
                }
            },
        ));
    }

    /// The title as typed, when it differs from the task's.
    fn title_edit(&self) -> Option<(String, Change)> {
        if let Some(id) = self.title_timer.take() {
            id.remove();
        }
        let t = self.task.borrow().clone()?;
        let text = text_of(&self.title).replace('\n', " ").trim().to_string();
        if text.is_empty() || text == t.task.summary {
            return None;
        }
        self.remember(|t| t.task.summary = text.clone());
        let change = Change {
            summary: Some(text),
            ..Change::default()
        };
        Some((t.href, change))
    }

    fn notes_edit(&self) -> Option<(String, Change)> {
        if let Some(id) = self.notes_timer.take() {
            id.remove();
        }
        let t = self.task.borrow().clone()?;
        let text = text_of(&self.notes);
        let new = Some(text.trim_end().to_string()).filter(|s| !s.trim().is_empty());
        if new == t.task.description {
            return None;
        }
        self.remember(|t| t.task.description = new.clone());
        let change = Change {
            description: Some(new),
            ..Change::default()
        };
        Some((t.href, change))
    }

    fn save_title(&self) {
        if let Some((href, change)) = self.title_edit() {
            self.tx.emit(Msg::Edit(href, Box::new(change)));
        }
    }

    fn save_notes(&self) {
        if let Some((href, change)) = self.notes_edit() {
            self.tx.emit(Msg::Edit(href, Box::new(change)));
        }
    }

    /// Note an edit on the shown copy, so a second save doesn't repeat it
    /// before the daemon's answer arrives.
    fn remember(&self, f: impl FnOnce(&mut TaskView)) {
        if let Some(t) = self.task.borrow_mut().as_mut() {
            f(t);
        }
    }

    /// Typing not saved yet.
    pub fn has_pending(&self) -> bool {
        self.title_timer.borrow().is_some() || self.notes_timer.borrow().is_some()
    }

    /// Send what is still waiting on a pause in typing.
    pub fn flush(&self) {
        for (href, change) in self.pending_edits() {
            self.tx.emit(Msg::Edit(href, Box::new(change)));
        }
    }

    /// What is still waiting on a pause in typing, as edits to send. Only
    /// typed text: a field that was merely focused doesn't undo a change
    /// that came from another device meanwhile.
    fn pending_edits(&self) -> Vec<(String, Change)> {
        let mut edits = Vec::new();
        if self.title_timer.borrow().is_some() {
            edits.extend(self.title_edit());
        }
        if self.notes_timer.borrow().is_some() {
            edits.extend(self.notes_edit());
        }
        edits
    }

    /// Let go of the task, handing back the typing not saved yet.
    pub fn close(&self) -> Vec<(String, Change)> {
        let edits = self.pending_edits();
        *self.task.borrow_mut() = None;
        edits
    }

    pub fn show(&self, t: &TaskView, lists: &[ListView], sunday_first: bool) {
        let now = pickers::now();
        let same = self.href().as_deref() == Some(t.href.as_str());
        if !same {
            self.flush();
        }
        *self.lists.borrow_mut() = lists.to_vec();
        self.sunday_first.set(sunday_first);
        *self.task.borrow_mut() = Some(t.clone());
        let open = t.task.is_open();

        self.loading.set(true);
        // Leave text alone while it is being typed in.
        let focus = self.root.root().and_then(|r| r.focus());
        let typing_in = |v: &gtk::TextView| {
            focus
                .as_ref()
                .is_some_and(|f| f == v.upcast_ref::<gtk::Widget>())
        };
        // Unchanged text isn't set again either, which would lose the cursor.
        let notes = t.task.description.as_deref().unwrap_or("");
        if !(same && (typing_in(&self.title) || self.title_timer.borrow().is_some()))
            && text_of(&self.title) != t.task.summary
        {
            self.title.buffer().set_text(&t.task.summary);
        }
        if !(same && (typing_in(&self.notes) || self.notes_timer.borrow().is_some()))
            && text_of(&self.notes) != notes
        {
            self.notes.buffer().set_text(notes);
        }
        self.check.set_active(!open);
        self.loading.set(false);

        let level = priority_level(t.task.priority);
        for l in 1..=4 {
            self.check.remove_css_class(&format!("priority-{l}"));
            self.priority_icon
                .remove_css_class(&format!("priority-{l}-icon"));
        }
        self.check.add_css_class(&format!("priority-{level}"));
        self.check
            .set_tooltip_text(Some(if open { "Complete" } else { "Reopen" }));
        self.priority_icon
            .add_css_class(&format!("priority-{level}-icon"));
        self.priority
            .set_tooltip_text(Some(model::priority_name(level)));

        match &t.task.due {
            Some(d) => {
                let zone = now.timezone();
                let day = d.local_date(zone);
                self.date_label
                    .set_text(&capitalize(&fmt::due_label(d, now)));
                self.date_icon
                    .set_icon_name(Some(if t.task.rrule.is_some() {
                        "playlist-repeat-symbolic"
                    } else if day == now.date_naive() {
                        "star-outline-thick-symbolic"
                    } else if day == now.date_naive().succ_opt().unwrap_or(day) {
                        "today-calendar-symbolic"
                    } else {
                        "month-symbolic"
                    }));
                let tip = match &t.task.rrule {
                    Some(r) => capitalize(&fmt::repeat_label(r)),
                    None => "Date".into(),
                };
                self.date.set_tooltip_text(Some(&tip));
            }
            None => {
                self.date_label.set_text("Date");
                self.date_icon.set_icon_name(Some("month-symbolic"));
                self.date.set_tooltip_text(Some("Date"));
                self.date_clear.set_reveal_child(false);
            }
        }

        let list = lists.iter().find(|l| l.href == t.list);
        ui::clear(&self.list_ring);
        self.list_ring
            .append(&ui::ring(list.and_then(|l| l.color.as_deref()), 14));
        self.list_label.set_text(&t.list_name);

        self.reminder_dot
            .set_visible(!t.task.alarms.is_empty() || !t.task.location_alarms.is_empty());
        let mut reminders = match t.task.alarms.as_slice() {
            [] => "Reminders".to_string(),
            [one] => model::reminder_label(&one.trigger, &t.task, now),
            many => format!("{} reminders", many.len()),
        };
        let places = t.task.location_alarms.len();
        if places > 0 {
            reminders = if reminders == "Reminders" {
                match places {
                    1 => format!("On arrival at {}", t.task.location_alarms[0].title),
                    n => format!("{n} location reminders"),
                }
            } else {
                format!("{reminders}, {places} location")
            };
        }
        self.reminders.set_tooltip_text(Some(&reminders));

        for w in [&self.date, &self.list, &self.priority, &self.reminders] {
            w.set_sensitive(open);
        }
    }

    /// Replace the title or the notes as if typed, for scripted checks.
    pub fn type_title(&self, text: &str) {
        self.title.buffer().set_text(text);
    }

    pub fn type_notes(&self, text: &str) {
        self.notes.buffer().set_text(text);
    }

    /// Enter in the notes, at the end, for scripted checks.
    pub fn type_newline(&self) {
        let buffer = self.notes.buffer();
        buffer.place_cursor(&buffer.end_iter());
        buffer.insert_at_cursor("\n");
    }

    /// Place search results arrived: show them in the picker, if it is open.
    pub fn show_places(&self, places: Vec<Place>) {
        *self.places.borrow_mut() = places;
        if let Some((list, picked)) = self.place_results.borrow().as_ref() {
            pickers::fill_places(list, &self.places.borrow(), picked);
        }
    }

    /// Open a button's picker, for scripted checks.
    pub fn popup(&self, which: &str) -> bool {
        let button = match which {
            "date" => &self.date,
            "list" => &self.list,
            "priority" => &self.priority,
            "reminders" => &self.reminders,
            "menu" => &self.menu,
            _ => return false,
        };
        button.popup();
        true
    }
}

/// When the task was made, changed and done, and where it came from.
fn history(t: &TaskView) -> String {
    let now = pickers::now();
    let mut lines = Vec::new();
    let mut made = Vec::new();
    if let Some(c) = t.task.created {
        made.push(format!("Created {}", model::ago(c, now)));
    }
    if let Some(m) = t.task.modified.filter(|m| Some(*m) != t.task.created) {
        made.push(format!("updated {}", model::ago(m, now)));
    }
    if !made.is_empty() {
        lines.push(capitalize(&made.join(", ")));
    }
    if !t.task.is_open()
        && let Some(c) = t.task.completed
    {
        lines.push(format!("Completed {}", model::ago(c, now)));
    }
    if t.pending {
        lines.push("Not on the server yet".into());
    }
    if let Some(s) = &t.task.source {
        lines.push(format!("From {s}"));
    }
    lines.join("\n")
}
