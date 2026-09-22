//! The card for adding tasks in place: type with quick-add syntax and see
//! what was understood, or set the date, priority, reminders and list with
//! buttons; notes go under the title, and notes to link under those (quick
//! add started from a note). Typing `#` suggests lists. Enter adds and keeps
//! the card open for the next one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use asst_core::api::{AddSpec, ListView};
use asst_core::fmt;
use asst_core::quickadd::{self, Kind};
use asst_core::task::{Status, Task};
use asst_core::time::{Trigger, When};
use chrono::NaiveDate;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::model::{self, capitalize};
use crate::pickers::{self, DateOpts, Schedule};
use crate::window::{Msg, Tx};
use crate::{notes, ui};

/// Where a task added from the card goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    List(String),
    Day(NaiveDate),
    /// The inbox, or whatever the text says.
    Anywhere,
}

#[derive(Default)]
struct Chosen {
    schedule: Option<Schedule>,
    priority: Option<u8>,
    list: Option<String>,
    alarms: Option<Vec<Trigger>>,
}

pub struct AddCard {
    pub root: gtk::Box,
    entry: gtk::Entry,
    notes: gtk::TextView,
    /// Notes the next tasks link to.
    chips: adw::WrapBox,
    date_button: gtk::MenuButton,
    date_label: gtk::Label,
    priority_icon: gtk::Image,
    list_label: gtk::Label,
    list_ring: gtk::Box,
    reminders: gtk::MenuButton,
    reminder_dot: gtk::Widget,
    keep: gtk::ToggleButton,
    /// Lists matching the `#word` being typed.
    suggest: gtk::Popover,
    suggest_rows: gtk::ListBox,
    /// The names in `suggest_rows`, in order.
    suggested: RefCell<Vec<String>>,
    chosen: RefCell<Chosen>,
    target: RefCell<Target>,
    lists: RefCell<Vec<ListView>>,
    sunday_first: Cell<bool>,
    /// 1 to 4: what a task gets when neither the text nor a button says.
    default_priority: Cell<u8>,
    read_dates: Cell<bool>,
    on_add: Box<dyn Fn(AddSpec)>,
    on_cancel: Rc<dyn Fn()>,
}

impl AddCard {
    /// The window's card sends its tasks as messages.
    pub fn for_window(tx: Tx) -> Rc<AddCard> {
        let cancel = tx.clone();
        AddCard::new(
            move |spec| tx.emit(Msg::Add(Box::new(spec))),
            move || cancel.emit(Msg::HideAdd),
        )
    }

    pub fn new(on_add: impl Fn(AddSpec) + 'static, on_cancel: impl Fn() + 'static) -> Rc<AddCard> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("card");
        root.add_css_class("add-card");

        let entry = gtk::Entry::builder()
            .placeholder_text("Task name   tomorrow 5pm  #list  p1  !30m  every mon")
            .hexpand(true)
            .build();
        entry.add_css_class("flat");
        entry.add_css_class("add-entry");
        root.append(&entry);

        let chips = adw::WrapBox::builder()
            .child_spacing(6)
            .line_spacing(6)
            .visible(false)
            .build();
        root.append(&chips);
        let notes = ui::text_view("Notes");
        notes.add_css_class("add-notes");
        notes::enhance(&notes);
        root.append(&notes);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        let date_label = gtk::Label::builder()
            .label("Date")
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let date_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        date_content.append(&gtk::Image::from_icon_name("month-symbolic"));
        date_content.append(&date_label);
        let date_button = gtk::MenuButton::builder().child(&date_content).build();
        date_button.add_css_class("flat");
        ui::tip(&date_button, "Date", "Ctrl+D");
        actions.append(&date_button);

        let priority_icon = gtk::Image::from_icon_name("flag-outline-thick-symbolic");
        let priority_button = gtk::MenuButton::builder().child(&priority_icon).build();
        priority_button.add_css_class("flat");
        priority_button.set_tooltip_text(Some("Priority"));
        actions.append(&priority_button);

        let reminders = gtk::MenuButton::builder()
            .icon_name("alarm-symbolic")
            .build();
        reminders.add_css_class("flat");
        ui::tip(&reminders, "Reminders", "Ctrl+R");
        let reminder_dot = gtk::Box::builder()
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .can_target(false)
            .visible(false)
            .build();
        reminder_dot.add_css_class("indicator");
        let reminder_box = gtk::Overlay::builder().child(&reminders).build();
        reminder_box.add_overlay(&reminder_dot);
        actions.append(&reminder_box);

        let list_ring = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let list_label = gtk::Label::builder()
            .label("Inbox")
            .max_width_chars(18)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let list_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        list_content.append(&list_ring);
        list_content.append(&list_label);
        let list_button = gtk::MenuButton::builder().child(&list_content).build();
        list_button.add_css_class("flat");
        list_button.set_tooltip_text(Some("List"));
        actions.append(&list_button);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions.append(&spacer);
        let keep = gtk::ToggleButton::builder()
            .icon_name("playlist-repeat-symbolic")
            .visible(false)
            .build();
        keep.add_css_class("flat");
        ui::tip(&keep, "Keep adding", "Ctrl+K");
        actions.append(&keep);
        // An icon, so the buttons still fit a phone-wide window.
        let cancel = gtk::Button::from_icon_name("window-close-symbolic");
        cancel.add_css_class("flat");
        ui::tip(&cancel, "Cancel", "Esc");
        actions.append(&cancel);
        let add = gtk::Button::with_label("Add Task");
        add.add_css_class("suggested-action");
        add.set_sensitive(false);
        actions.append(&add);
        root.append(&actions);

        let suggest_rows = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .can_focus(false)
            .build();
        suggest_rows.add_css_class("picker-list");
        let suggest = gtk::Popover::builder()
            .child(&suggest_rows)
            .autohide(false)
            .can_focus(false)
            .has_arrow(false)
            .position(gtk::PositionType::Bottom)
            .build();
        suggest.add_css_class("menu-popover");
        suggest.set_parent(&entry);
        // Open, it keeps Esc for itself (see `window::key_action`).
        ui::track(&suggest);

        let card = Rc::new(AddCard {
            root,
            entry: entry.clone(),
            notes,
            chips,
            date_button: date_button.clone(),
            date_label,
            priority_icon,
            list_label,
            list_ring,
            reminders: reminders.clone(),
            reminder_dot: reminder_dot.upcast(),
            keep,
            suggest,
            suggest_rows,
            suggested: RefCell::default(),
            chosen: RefCell::default(),
            target: RefCell::new(Target::Anywhere),
            lists: RefCell::default(),
            sunday_first: Cell::new(false),
            default_priority: Cell::new(4),
            read_dates: Cell::new(true),
            on_add: Box::new(on_add),
            on_cancel: Rc::new(on_cancel),
        });

        {
            let weak = Rc::downgrade(&card);
            let add = add.clone();
            entry.connect_changed(move |e| {
                add.set_sensitive(!e.text().trim().is_empty());
                if let Some(c) = weak.upgrade() {
                    c.parse();
                    // Once the cursor has moved past what was typed.
                    let weak = weak.clone();
                    glib::idle_add_local_once(move || {
                        if let Some(c) = weak.upgrade() {
                            c.suggest_lists();
                        }
                    });
                }
            });
        }
        {
            let weak = Rc::downgrade(&card);
            entry.connect_activate(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.submit();
                }
            });
        }
        {
            let weak = Rc::downgrade(&card);
            add.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.submit();
                }
            });
        }
        {
            let on_cancel = card.on_cancel.clone();
            cancel.connect_clicked(move |_| on_cancel());
        }
        {
            // The list suggestions take arrows, Enter, Tab and Esc while up.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(&card);
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(c) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if !c.suggesting() {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gdk::Key::Down | gdk::Key::Up => {
                        c.move_suggestion(if key == gdk::Key::Down { 1 } else { -1 });
                    }
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => c.take_suggestion(),
                    gdk::Key::Escape => c.suggest.popdown(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            card.entry.add_controller(keys);
        }
        {
            let keys = gtk::EventControllerKey::new();
            let on_cancel = card.on_cancel.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    on_cancel();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            card.entry.add_controller(keys);
        }
        {
            // Ctrl+Enter adds from the notes, where Enter is a new line.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(&card);
            keys.connect_key_pressed(move |_, key, _, state| {
                if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                    && state.contains(gdk::ModifierType::CONTROL_MASK)
                    && let Some(c) = weak.upgrade()
                {
                    c.submit();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            card.notes.add_controller(keys);
        }
        {
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(&card);
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(c) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if !state.contains(gdk::ModifierType::CONTROL_MASK) {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gdk::Key::d => c.date_button.popup(),
                    gdk::Key::r => c.reminders.popup(),
                    gdk::Key::k if c.keep.is_visible() => c.keep.set_active(!c.keep.is_active()),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            card.root.add_controller(keys);
        }
        {
            let weak = Rc::downgrade(&card);
            card.suggest_rows.connect_row_activated(move |_, row| {
                if let Some(c) = weak.upgrade() {
                    c.suggest_rows.select_row(Some(row));
                    c.take_suggestion();
                }
            });
        }
        {
            let weak = Rc::downgrade(&card);
            date_button.set_create_popup_func(move |mb| {
                let Some(c) = weak.upgrade() else { return };
                let start = c.chosen.borrow().schedule.clone().unwrap_or_default();
                let weak = Rc::downgrade(&c);
                mb.set_popover(Some(&pickers::date_popover(
                    start,
                    DateOpts {
                        sunday_first: c.sunday_first.get(),
                        full: true,
                        clear: false,
                    },
                    move |s| {
                        if let Some(c) = weak.upgrade() {
                            c.chosen.borrow_mut().schedule = Some(s);
                            c.refresh();
                        }
                    },
                )));
            });
        }
        {
            let weak = Rc::downgrade(&card);
            priority_button.set_create_popup_func(move |mb| {
                let Some(c) = weak.upgrade() else { return };
                let current = c.priority();
                let weak = Rc::downgrade(&c);
                mb.set_popover(Some(&pickers::priority_popover(current, move |level| {
                    if let Some(c) = weak.upgrade() {
                        c.chosen.borrow_mut().priority = Some(level);
                        c.refresh();
                    }
                })));
            });
        }
        {
            let weak = Rc::downgrade(&card);
            reminders.set_create_popup_func(move |mb| {
                let Some(c) = weak.upgrade() else { return };
                let weak = Rc::downgrade(&c);
                mb.set_popover(Some(&pickers::reminder_popover(
                    &c.draft(),
                    move |triggers, _| {
                        if let Some(c) = weak.upgrade() {
                            c.chosen.borrow_mut().alarms = Some(triggers);
                            c.refresh();
                        }
                    },
                    None,
                )));
            });
        }
        {
            let weak = Rc::downgrade(&card);
            list_button.set_create_popup_func(move |mb| {
                let Some(c) = weak.upgrade() else { return };
                let current = c.list_href();
                let lists = c.lists.borrow().clone();
                let weak = Rc::downgrade(&c);
                mb.set_popover(Some(&pickers::list_popover(
                    &lists,
                    current.as_deref(),
                    move |href| {
                        if let Some(c) = weak.upgrade() {
                            c.chosen.borrow_mut().list = Some(href);
                            c.refresh();
                        }
                    },
                )));
            });
        }
        card
    }

    pub fn set_text(&self, text: &str) {
        self.entry.set_text(text);
        self.entry.set_position(-1);
    }

    /// Text from elsewhere (the clipboard): its first line is the title, the
    /// rest goes in the notes.
    pub fn fill(&self, text: &str) {
        let text = text.trim();
        let (title, rest) = text.split_once('\n').unwrap_or((text, ""));
        self.set_text(title.trim());
        self.notes.buffer().set_text(rest.trim());
    }

    pub fn set_lists(&self, lists: &[ListView], sunday_first: bool) {
        *self.lists.borrow_mut() = lists.to_vec();
        self.sunday_first.set(sunday_first);
        self.refresh();
    }

    /// The preferences for new tasks: a priority (1 to 4) and whether dates
    /// in the title count.
    pub fn set_defaults(&self, priority: u8, read_dates: bool) {
        self.default_priority.set(priority.clamp(1, 4));
        self.read_dates.set(read_dates);
        self.parse();
        self.refresh();
    }

    /// Quick add's "Keep adding" button.
    pub fn show_keep(&self) {
        self.keep.set_visible(true);
    }

    pub fn keeps_adding(&self) -> bool {
        self.keep.is_active()
    }

    /// Show it for a new place: what was chosen for the last one goes.
    pub fn open(&self, target: Target) {
        *self.target.borrow_mut() = target;
        *self.chosen.borrow_mut() = Chosen::default();
        self.entry.set_text("");
        self.notes.buffer().set_text("");
        self.refresh();
    }

    /// Where tasks go when nothing else says, keeping what was typed.
    pub fn set_target(&self, target: Target) {
        *self.target.borrow_mut() = target;
        self.refresh();
    }

    pub fn focus(&self) {
        self.entry.grab_focus();
    }

    /// The list suggestions are up, and take Enter and Esc.
    pub fn suggesting(&self) -> bool {
        self.suggest.is_visible()
    }

    fn list_href(&self) -> Option<String> {
        let lists = self.lists.borrow();
        self.chosen
            .borrow()
            .list
            .clone()
            .or_else(|| match &*self.target.borrow() {
                Target::List(h) => Some(h.clone()),
                _ => None,
            })
            .or_else(|| model::inbox(&lists).map(|l| l.href.clone()))
    }

    fn priority(&self) -> u8 {
        self.chosen
            .borrow()
            .priority
            .or(self.parsed().priority)
            .unwrap_or(self.default_priority.get())
    }

    fn parsed(&self) -> quickadd::Parsed {
        let names: Vec<String> = self.lists.borrow().iter().map(|l| l.name.clone()).collect();
        quickadd::parse_with(
            &self.entry.text(),
            pickers::now(),
            &names,
            self.read_dates.get(),
        )
    }

    /// The task as it would be added, for the reminder picker to work from.
    fn draft(&self) -> Task {
        let chosen = self.chosen.borrow();
        let due = match (&chosen.schedule, &*self.target.borrow()) {
            (Some(s), _) => s.due.clone(),
            (None, target) => self.parsed().due.or(match target {
                Target::Day(d) => Some(When::Date { date: *d }),
                _ => None,
            }),
        };
        Task {
            uid: String::new(),
            summary: self.entry.text().to_string(),
            description: None,
            status: Status::NeedsAction,
            completed: None,
            priority: 0,
            due,
            rrule: None,
            location_alarms: Vec::new(),
            alarms: chosen
                .alarms
                .iter()
                .flatten()
                .map(|t| asst_core::task::Alarm {
                    trigger: t.clone(),
                    acknowledged: None,
                })
                .collect(),
            parent: None,
            source: None,
            sort_order: None,
            created: None,
            modified: None,
        }
    }

    fn parse(&self) {
        let p = self.parsed();
        self.chips.remove_all();
        let text = self.entry.text();
        for (label, class) in chips(&p) {
            let chip = gtk::Label::new(Some(&label));
            chip.add_css_class("chip");
            chip.add_css_class(class);
            self.chips.append(&chip);
        }
        self.chips.set_visible(self.chips.first_child().is_some());
        let attrs = pango::AttrList::new();
        for (range, _) in &p.spans {
            if range.end <= text.len() {
                let mut a = pango::AttrColor::new_foreground(0x35 * 257, 0x84 * 257, 0xe4 * 257);
                a.set_start_index(range.start as u32);
                a.set_end_index(range.end as u32);
                attrs.insert(a);
            }
        }
        self.entry.set_attributes(&attrs);
    }

    /// The `#word` at the cursor: where it starts and ends (characters),
    /// and what follows the `#`.
    fn hash_word(&self) -> Option<(i32, i32, String)> {
        let text = self.entry.text();
        let chars: Vec<char> = text.chars().collect();
        let end = usize::try_from(self.entry.position())
            .ok()?
            .min(chars.len());
        let start = chars[..end]
            .iter()
            .rposition(|c| c.is_whitespace())
            .map_or(0, |i| i + 1);
        let word: String = chars[start..end].iter().collect();
        let rest = word.strip_prefix('#')?;
        Some((start as i32, end as i32, rest.to_lowercase()))
    }

    fn suggest_lists(&self) {
        let squash = |s: &str| -> String {
            s.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_lowercase()
        };
        let matches: Vec<ListView> = match self.hash_word() {
            Some((_, _, word)) => self
                .lists
                .borrow()
                .iter()
                .filter(|l| l.writable && squash(&l.name).contains(&word))
                .filter(|l| squash(&l.name) != word)
                .take(8)
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        if matches.is_empty() {
            self.suggest.popdown();
            return;
        }
        self.suggest_rows.remove_all();
        for l in &matches {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            line.set_margin_start(6);
            line.set_margin_end(6);
            line.append(&ui::ring(l.color.as_deref(), 14));
            line.append(&ui::label(&l.name, &[]));
            self.suggest_rows.append(&line);
        }
        *self.suggested.borrow_mut() = matches.into_iter().map(|l| l.name).collect();
        self.suggest_rows
            .select_row(self.suggest_rows.row_at_index(0).as_ref());
        if !self.suggest.is_visible() && self.entry.is_mapped() {
            self.suggest.popup();
        }
    }

    fn move_suggestion(&self, step: i32) {
        let count = self.suggested.borrow().len() as i32;
        let at = self.suggest_rows.selected_row().map_or(0, |r| r.index());
        let next = (at + step).rem_euclid(count.max(1));
        self.suggest_rows
            .select_row(self.suggest_rows.row_at_index(next).as_ref());
    }

    /// Put the chosen list's name in place of the `#word`.
    fn take_suggestion(&self) {
        let index = self.suggest_rows.selected_row().map_or(0, |r| r.index());
        let name = self.suggested.borrow().get(index as usize).cloned();
        self.suggest.popdown();
        let (Some(name), Some((start, end, _))) = (name, self.hash_word()) else {
            return;
        };
        let word: String = name.chars().filter(|c| !c.is_whitespace()).collect();
        self.entry.delete_text(start, end);
        let mut at = start;
        self.entry.insert_text(&format!("#{word} "), &mut at);
        self.entry.set_position(at);
    }

    fn refresh(&self) {
        let now = pickers::now();
        let chosen = self.chosen.borrow();
        let date = match (&chosen.schedule, &*self.target.borrow()) {
            (
                Some(Schedule {
                    due: Some(d),
                    rrule,
                }),
                _,
            ) => {
                let mut s = capitalize(&fmt::due_label(d, now));
                if rrule.is_some() {
                    s.push_str(" ↻");
                }
                s
            }
            (Some(_), _) => "No date".into(),
            (None, Target::Day(d)) => capitalize(&fmt::due_label(&When::Date { date: *d }, now)),
            (None, _) => "Date".into(),
        };
        self.date_label.set_text(&date);
        let alarms = chosen.alarms.as_ref().map_or(0, Vec::len);
        self.reminder_dot.set_visible(alarms > 0);
        drop(chosen);
        for level in 1..=4 {
            self.priority_icon
                .remove_css_class(&format!("priority-{level}-icon"));
        }
        let level = self.priority();
        if level < 4 {
            self.priority_icon
                .add_css_class(&format!("priority-{level}-icon"));
        }
        let href = self.list_href();
        let lists = self.lists.borrow();
        let list = href
            .as_deref()
            .and_then(|h| lists.iter().find(|l| l.href == h));
        self.list_label
            .set_text(list.map_or("Inbox", |l| l.name.as_str()));
        ui::clear(&self.list_ring);
        self.list_ring
            .append(&ui::ring(list.and_then(|l| l.color.as_deref()), 14));
    }

    pub fn submit(&self) {
        let text = self.entry.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.suggest.popdown();
        let parsed = self.parsed();
        let chosen = self.chosen.borrow();
        let buffer = self.notes.buffer();
        let notes = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .trim_end()
            .to_string();
        let mut spec = AddSpec {
            text,
            parse: true,
            keep_dates: !self.read_dates.get(),
            description: Some(notes).filter(|n| !n.trim().is_empty()),
            alarms: chosen.alarms.clone(),
            ..AddSpec::default()
        };
        match &chosen.schedule {
            Some(s) => {
                spec.due = s.due.clone();
                spec.rrule = s.rrule.clone();
            }
            None => {
                if let (None, Target::Day(d)) = (&parsed.due, &*self.target.borrow()) {
                    spec.due = Some(When::Date { date: *d });
                }
            }
        }
        spec.priority = chosen.priority.or_else(|| {
            let default = self.default_priority.get();
            (parsed.priority.is_none() && default < 4).then_some(default)
        });
        spec.list = chosen
            .list
            .clone()
            .or_else(|| match &*self.target.borrow() {
                Target::List(h) if parsed.list.is_none() => Some(h.clone()),
                _ => None,
            });
        drop(chosen);
        (self.on_add)(spec);
        self.entry.set_text("");
        buffer.set_text("");
        {
            let mut chosen = self.chosen.borrow_mut();
            chosen.schedule = None;
            chosen.alarms = None;
        }
        self.refresh();
        self.entry.grab_focus();
    }
}

/// What the parser understood, as small labels.
pub fn chips(p: &quickadd::Parsed) -> Vec<(String, &'static str)> {
    let now = pickers::now();
    let mut out = Vec::new();
    for (_, kind) in &p.spans {
        match kind {
            Kind::List => {
                if let Some(l) = &p.list {
                    out.push((format!("#{l}"), "list"));
                }
            }
            Kind::Priority => {
                if let Some(level) = p.priority {
                    out.push((
                        format!("p{level}"),
                        if level == 1 { "p1" } else { "priority" },
                    ));
                }
            }
            Kind::Date => {
                if let Some(d) = &p.due {
                    out.push((fmt::due_label(d, now), "due"));
                }
            }
            Kind::Repeat => {
                if let (Some(r), Some(d)) = (&p.rrule, &p.due) {
                    out.push((format!("↻ {}", fmt::repeat_label(r)), "repeat"));
                    out.push((format!("from {}", fmt::due_label(d, now)), "due"));
                }
            }
            Kind::Alarm => match &p.alarm {
                Some(Trigger::Relative { offset, .. }) => {
                    let text = match offset.num_minutes() {
                        0 => "at the due time".to_string(),
                        _ => format!("{} before", model::span(*offset)),
                    };
                    out.push((format!("⏰ {text}"), "alarm"))
                }
                Some(Trigger::Absolute { at }) => out.push((
                    format!("⏰ {}", fmt::due_label(&When::Utc { at: *at }, now)),
                    "alarm",
                )),
                None => out.push(("⏰ needs a due date".into(), "alarm")),
            },
        }
    }
    out
}
