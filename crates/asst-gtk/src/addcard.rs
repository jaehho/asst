//! The card for adding tasks in place: type with quick-add syntax and see
//! what was understood, or set the date, priority, reminders and list with
//! buttons; notes go under the title, and notes to link under those (quick
//! add started from a note). Typing `#` suggests lists. Enter adds and keeps
//! the card open for the next one; in the notes, Enter adds too and
//! Shift+Enter starts a new line. In the window, what is typed is added when
//! the card is left (Esc, another view, the window closing); the cancel
//! button is what throws it away. Text with nothing left once the date, list
//! and priority are read out is no task: Add stays off and says so, and
//! nothing is sent, so what was typed stays in the card, to finish the next
//! time it opens.

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

/// The Add button's words: ready, and what is missing.
const ADD: &str = "Add Task";
const ADD_NEEDS_TITLE: &str = "Add a title";

/// Starts making a list of the name given.
pub type MakeList = Rc<dyn Fn(String)>;

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
    add: gtk::Button,
    /// Lists matching the `#word` being typed.
    suggest: gtk::Popover,
    suggest_rows: gtk::ListBox,
    /// The names in `suggest_rows`, in order.
    suggested: RefCell<Vec<String>>,
    /// The `#word` the last suggestion row would make a list of.
    suggest_new: RefCell<Option<String>>,
    /// Makes a list nothing is named for. Without it, no row offers one.
    on_create: RefCell<Option<MakeList>>,
    /// The list being made, and an add waiting on it.
    making: RefCell<Option<String>>,
    add_when_made: Cell<bool>,
    chosen: RefCell<Chosen>,
    target: RefCell<Target>,
    lists: RefCell<Vec<ListView>>,
    sunday_first: Cell<bool>,
    /// 1 to 4: what a task gets when neither the text nor a button says.
    default_priority: Cell<u8>,
    read_dates: Cell<bool>,
    /// The task, and whether it was added by leaving the card.
    on_add: Box<dyn Fn(AddSpec, bool)>,
    leaving: Cell<bool>,
    on_cancel: Rc<dyn Fn()>,
    /// Leaving the card with a title typed adds the task (the window's
    /// card; quick add's popup throws it away).
    save_on_leave: bool,
}

impl AddCard {
    /// The window's card sends its tasks as messages.
    pub fn for_window(tx: Tx) -> Rc<AddCard> {
        let cancel = tx.clone();
        AddCard::build(
            move |spec, implicit| tx.emit(Msg::Add(Box::new(spec), implicit)),
            move || cancel.emit(Msg::HideAdd),
            true,
        )
    }

    pub fn new(on_add: impl Fn(AddSpec) + 'static, on_cancel: impl Fn() + 'static) -> Rc<AddCard> {
        AddCard::build(move |spec, _| on_add(spec), on_cancel, false)
    }

    fn build(
        on_add: impl Fn(AddSpec, bool) + 'static,
        on_cancel: impl Fn() + 'static,
        save_on_leave: bool,
    ) -> Rc<AddCard> {
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
        if save_on_leave {
            ui::tip(&cancel, "Discard", "Esc keeps what you typed");
        } else {
            ui::tip(&cancel, "Cancel", "Esc");
        }
        actions.append(&cancel);
        let add = gtk::Button::with_label(ADD_NEEDS_TITLE);
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
            add: add.clone(),
            suggest,
            suggest_rows,
            suggested: RefCell::default(),
            suggest_new: RefCell::default(),
            on_create: RefCell::default(),
            making: RefCell::default(),
            add_when_made: Cell::new(false),
            chosen: RefCell::default(),
            target: RefCell::new(Target::Anywhere),
            lists: RefCell::default(),
            sunday_first: Cell::new(false),
            default_priority: Cell::new(4),
            read_dates: Cell::new(true),
            on_add: Box::new(on_add),
            leaving: Cell::new(false),
            on_cancel: Rc::new(on_cancel),
            save_on_leave,
        });
        {
            // Before `enhance`, whose handler carries a list on from a
            // newline: a stopped newline must not reach it.
            let weak = Rc::downgrade(&card);
            notes_enter(&card.notes, move || {
                if let Some(c) = weak.upgrade() {
                    if c.has_title() {
                        c.submit();
                    } else {
                        c.entry.grab_focus();
                    }
                }
            });
            notes::enhance(&card.notes);
        }
        {
            let weak = Rc::downgrade(&card);
            entry.connect_changed(move |_| {
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
            let weak = Rc::downgrade(&card);
            cancel.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.discard();
                    (c.on_cancel)();
                }
            });
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
                let chosen = c.suggest_rows.selected_row().is_some();
                match key {
                    gdk::Key::Down | gdk::Key::Up => {
                        c.move_suggestion(if key == gdk::Key::Down { 1 } else { -1 });
                    }
                    // Nothing is chosen while the only offer is a new list:
                    // `#123` is text, and Enter adds the task.
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab if !chosen => {
                        return glib::Propagation::Proceed;
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
            let weak = Rc::downgrade(&card);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape
                    && let Some(c) = weak.upgrade()
                {
                    c.leave();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            card.entry.add_controller(keys);
        }
        {
            // Ctrl+Enter adds from the notes too.
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
                let create = c.on_create.borrow().is_some().then(|| {
                    let weak = Rc::downgrade(&c);
                    Rc::new(move |name: String| {
                        if let Some(c) = weak.upgrade() {
                            c.make_list(name);
                        }
                    }) as MakeList
                });
                mb.set_popover(Some(&pickers::list_popover_with(
                    &lists,
                    current.as_deref(),
                    move |href| {
                        if let Some(c) = weak.upgrade() {
                            c.chosen.borrow_mut().list = Some(href);
                            c.refresh();
                        }
                    },
                    create,
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
        // A `#word` may name a list now, and then it is not the title.
        self.parse();
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
    /// Unfinished typing (no title, so leaving the card sent nothing) stays,
    /// with the choices made for it.
    pub fn open(&self, target: Target) {
        *self.target.borrow_mut() = target;
        if !self.holds_draft() {
            self.discard();
        }
        self.refresh();
    }

    /// Throw away what is typed and chosen.
    fn discard(&self) {
        *self.chosen.borrow_mut() = Chosen::default();
        self.entry.set_text("");
        self.notes.buffer().set_text("");
    }

    /// Something is typed that is no task yet.
    fn holds_draft(&self) -> bool {
        !self.entry.text().trim().is_empty() || !self.notes_text().trim().is_empty()
    }

    fn notes_text(&self) -> String {
        let buffer = self.notes.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    /// Where tasks go when nothing else says, keeping what was typed.
    pub fn set_target(&self, target: Target) {
        *self.target.borrow_mut() = target;
        self.refresh();
    }

    pub fn focus(&self) {
        self.entry.grab_focus();
        // Plain grab_focus selects the text, and a draft kept from the
        // last time would be typed over.
        self.entry.set_position(-1);
    }

    /// Lets pickers offer to make a list nothing is named for; `make`
    /// starts it, and `list_made` or `list_failed` answers.
    pub fn on_create_list(&self, make: impl Fn(String) + 'static) {
        *self.on_create.borrow_mut() = Some(Rc::new(make));
    }

    fn make_list(&self, name: String) {
        let Some(make) = self.on_create.borrow().clone() else {
            return;
        };
        *self.making.borrow_mut() = Some(name.clone());
        self.refresh();
        make(name);
    }

    /// The list asked for exists now: the task goes there.
    pub fn list_made(&self, list: &ListView) {
        self.making.borrow_mut().take();
        {
            let mut lists = self.lists.borrow_mut();
            if !lists.iter().any(|l| l.href == list.href) {
                lists.push(list.clone());
            }
        }
        self.chosen.borrow_mut().list = Some(list.href.clone());
        self.refresh();
        self.parse();
        if self.add_when_made.take() {
            self.submit();
        }
    }

    pub fn list_failed(&self) {
        self.making.borrow_mut().take();
        self.add_when_made.set(false);
        self.refresh();
    }

    /// Esc: the card closes, and what was typed is added first (in the
    /// window).
    pub fn leave(&self) {
        self.save_draft();
        (self.on_cancel)();
    }

    /// Add the task being typed, if there is one and this card keeps what
    /// is left in it. For leaving the card by any way but its cancel button.
    pub fn save_draft(&self) -> bool {
        let save = self.save_on_leave && self.has_title();
        if save {
            self.leaving.set(true);
            self.submit();
            self.leaving.set(false);
        }
        save
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

    /// Whether anything is left of the text for a title, once the date, the
    /// list and the priority in it are read out.
    fn has_title(&self) -> bool {
        !self.parsed().summary.is_empty()
    }

    fn parse(&self) {
        let p = self.parsed();
        let titled = !p.summary.is_empty();
        self.add.set_sensitive(titled);
        self.add
            .set_label(if titled { ADD } else { ADD_NEEDS_TITLE });
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
        Some((start as i32, end as i32, rest.to_string()))
    }

    fn suggest_lists(&self) {
        let squash = |s: &str| -> String {
            s.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_lowercase()
        };
        let word = self.hash_word().map(|(_, _, w)| w);
        let lower = word.as_deref().map(str::to_lowercase);
        let lists = self.lists.borrow();
        let matches: Vec<ListView> = match &lower {
            Some(word) => lists
                .iter()
                .filter(|l| l.writable && squash(&l.name).contains(word))
                .filter(|l| squash(&l.name) != *word)
                .take(8)
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        // A word nobody is named for can become a list. Not a number:
        // `#123` is an issue, and `#` alone is nothing yet.
        let new = word.filter(|w| {
            self.on_create.borrow().is_some()
                && !w.is_empty()
                && !w.chars().all(|c| c.is_ascii_digit())
                && !lists.iter().any(|l| squash(&l.name) == w.to_lowercase())
        });
        drop(lists);
        if matches.is_empty() && new.is_none() {
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
            line.append(&ui::list_mark(l, 14));
            line.append(&ui::label(&l.name, &[]));
            self.suggest_rows.append(&line);
        }
        if let Some(word) = &new {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            line.set_margin_start(6);
            line.set_margin_end(6);
            line.append(&gtk::Image::from_icon_name("plus-large-symbolic"));
            line.append(&ui::label(&format!("Create list “{word}”"), &[]));
            self.suggest_rows.append(&line);
        }
        let first = !matches.is_empty();
        *self.suggested.borrow_mut() = matches.into_iter().map(|l| l.name).collect();
        *self.suggest_new.borrow_mut() = new;
        // Existing lists first, the first chosen; "create" waits to be
        // reached for.
        self.suggest_rows.select_row(
            first
                .then(|| self.suggest_rows.row_at_index(0))
                .flatten()
                .as_ref(),
        );
        if !self.suggest.is_visible() && self.entry.is_mapped() {
            self.suggest.popup();
        }
    }

    fn move_suggestion(&self, step: i32) {
        let count =
            (self.suggested.borrow().len() + self.suggest_new.borrow().iter().count()) as i32;
        let next = match self.suggest_rows.selected_row() {
            Some(r) => (r.index() + step).rem_euclid(count.max(1)),
            None if step > 0 => 0,
            None => count - 1,
        };
        self.suggest_rows
            .select_row(self.suggest_rows.row_at_index(next).as_ref());
    }

    /// Put the chosen list's name in place of the `#word`.
    fn take_suggestion(&self) {
        let index = self.suggest_rows.selected_row().map_or(0, |r| r.index());
        let existing = self.suggested.borrow().get(index as usize).cloned();
        let new = match existing {
            Some(_) => None,
            None => self.suggest_new.borrow().clone(),
        };
        self.suggest.popdown();
        let name = match (existing, new) {
            (Some(name), _) => name,
            (None, Some(word)) => {
                self.make_list(word.clone());
                word
            }
            (None, None) => return,
        };
        let Some((start, end, _)) = self.hash_word() else {
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
        let making = self.making.borrow();
        self.list_label.set_text(
            making
                .as_deref()
                .or(list.map(|l| l.name.as_str()))
                .unwrap_or("Inbox"),
        );
        ui::clear(&self.list_ring);
        self.list_ring.append(&ui::list_mark_of(list, 14));
    }

    /// Add what is typed. `false` when there was nothing to add: no title,
    /// and then the text stays where it is.
    pub fn submit(&self) -> bool {
        let parsed = self.parsed();
        if parsed.summary.is_empty() {
            return false;
        }
        if self.making.borrow().is_some() {
            // The list this task is going to doesn't exist yet.
            self.add_when_made.set(true);
            return true;
        }
        self.suggest.popdown();
        let text = self.entry.text().trim().to_string();
        let chosen = self.chosen.borrow();
        let notes = self.notes_text().trim_end().to_string();
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
        (self.on_add)(spec, self.leaving.get());
        self.entry.set_text("");
        self.notes.buffer().set_text("");
        {
            let mut chosen = self.chosen.borrow_mut();
            chosen.schedule = None;
            chosen.alarms = None;
        }
        self.refresh();
        self.entry.grab_focus();
        true
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

/// Enter in the notes calls `add` instead of starting a line; Shift+Enter
/// still does. The newline is stopped where it is inserted, after the input
/// method has had the key, so Enter that only confirms a composition adds
/// nothing.
fn notes_enter(view: &gtk::TextView, add: impl Fn() + 'static) {
    let plain = Rc::new(Cell::new(false));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let plain = plain.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let held = state.intersects(
                gdk::ModifierType::SHIFT_MASK
                    | gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::ALT_MASK,
            );
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && !held {
                plain.set(true);
                // The insert, if the key makes one, is within this event.
                let plain = plain.clone();
                glib::idle_add_local_once(move || plain.set(false));
            }
            glib::Propagation::Proceed
        });
    }
    view.add_controller(keys);
    let add = Rc::new(add);
    view.buffer().connect_insert_text(move |b, _, text| {
        if text == "\n" && plain.replace(false) {
            b.stop_signal_emission_by_name("insert-text");
            let add = add.clone();
            glib::idle_add_local_once(move || add());
        }
    });
}
