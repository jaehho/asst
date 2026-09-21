//! The column of tasks for a view: a big title, then sections with a header
//! and a rule, their rows, and a way to add a task where it belongs.

use std::collections::HashSet;

use asst_core::api::{Change, ListView};
use chrono::DateTime;
use chrono_tz::Tz;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::addcard::Target;
use crate::model::{Kind, Nav, Section};
use crate::pickers::{self, DateOpts};
use crate::row::{self, RowOpts};
use crate::window::{Msg, Tx};
use crate::{motion, ui};

pub struct Opts<'a> {
    pub nav: &'a Nav,
    pub lists: &'a [ListView],
    /// The view is a list that was archived.
    pub archived: bool,
    pub now: DateTime<Tz>,
    pub completing: &'a HashSet<String>,
    pub select_mode: bool,
    pub selected: &'a HashSet<String>,
    pub sunday_first: bool,
    /// Where the add card is open, if it is.
    pub adding: Option<Target>,
    /// The view is sorted by the list's own order, so dragging rows reorders it.
    pub custom_order: bool,
    /// The task open in place, and the editor that goes in its row.
    pub editing: Option<&'a str>,
    pub editor: &'a gtk::Box,
    /// What is new since the last build of this view, to animate in.
    pub fresh: Fresh<'a>,
}

#[derive(Default)]
pub struct Fresh<'a> {
    /// Tasks that weren't there: their rows open up.
    pub rows: Option<&'a HashSet<String>>,
    /// Tasks just checked: the box pops and the row lights up.
    pub checked: Option<&'a HashSet<String>>,
    /// Repeating tasks checked and moved on: their new date lights up.
    pub rolled: Option<&'a HashSet<String>>,
    /// The add card, just opened.
    pub adding: bool,
    /// Select mode, just turned on: the boxes slide in.
    pub selecting: bool,
    /// Another view: the column fades in.
    pub view: bool,
    /// A task just opened in place, or closed: the other rows dim, or
    /// come back.
    pub focus: bool,
}

pub struct Built {
    pub root: gtk::Box,
    /// Every row, top to bottom, for moving through them with keys.
    pub rows: Vec<(String, gtk::ListBoxRow)>,
}

/// Where adding in a section puts the task.
pub fn section_target(kind: &Kind) -> Option<Target> {
    match kind {
        Kind::Day(d) => Some(Target::Day(*d)),
        Kind::List(h) => Some(Target::List(h.clone())),
        _ => None,
    }
}

pub fn build(sections: &[Section], o: &Opts, add_card: &gtk::Box, tx: &Tx) -> Built {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("view-column");
    if o.fresh.view {
        root.add_css_class("view-enter");
    }
    // Planify's focus on the task open in place: the rest steps back.
    if o.editing.is_some() {
        root.add_css_class("focusing");
    }
    if o.fresh.focus {
        root.add_css_class(if o.editing.is_some() {
            "focus-enter"
        } else {
            "focus-leave"
        });
    }
    // In a revealer either way, so closing the card can fold it.
    let card = || {
        ui::detach(add_card);
        if o.fresh.adding {
            motion::opening(add_card, gtk::RevealerTransitionType::SlideDown)
        } else {
            gtk::Revealer::builder()
                .reveal_child(true)
                .transition_duration(motion::ROW_MS)
                .child(add_card)
                .build()
        }
    };
    let open = sections
        .iter()
        .filter(|s| s.kind != Kind::Completed)
        .map(|s| s.tasks.len())
        .sum();
    root.append(&title(o, open));

    let mut rows = Vec::new();
    let any = sections.iter().any(|s| !s.tasks.is_empty());
    let mut placed = false;
    for s in sections {
        if s.tasks.is_empty() && !s.keep {
            continue;
        }
        let target = section_target(&s.kind);
        let adding_here = target.is_some() && target == o.adding;
        root.append(&section(s, o, tx, &mut rows));
        if adding_here {
            root.append(&card());
            placed = true;
        } else if let Some(t) = target.filter(|_| s.title.is_none() && any) {
            root.append(&add_button(t, tx));
        }
    }
    if !placed && o.adding.is_some() {
        root.append(&card());
    }
    if !any && o.adding.is_none() {
        root.append(&empty(o, tx));
    }
    Built { root, rows }
}

/// The view's icon and name; under it the date, or how many open tasks it shows.
fn title(o: &Opts, open: usize) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    b.add_css_class("view-title");
    match o.nav {
        Nav::List(href) => {
            let color = o
                .lists
                .iter()
                .find(|l| &l.href == href)
                .and_then(|l| l.color.as_deref());
            b.append(&ui::ring(color, 24));
        }
        nav => {
            let icon = ui::icon(nav.icon(), 18);
            icon.add_css_class("view-icon");
            icon.add_css_class(nav.tint());
            icon.set_valign(gtk::Align::Center);
            b.append(&icon);
        }
    }
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    text.append(&ui::label(&o.nav.title(o.lists), &["title-2"]));
    let sub = match o.nav {
        Nav::Today => Some(o.now.format("%A, %B %-d").to_string()),
        Nav::Completed => None,
        _ => Some(match open {
            0 => "Nothing to do".to_string(),
            1 => "1 task".to_string(),
            n => format!("{n} tasks"),
        }),
    }
    .map(|s| {
        if o.archived {
            format!("{s} · Archived")
        } else {
            s
        }
    });
    if let Some(sub) = sub {
        text.append(&ui::label(&sub, &["caption", "dim-label"]));
    }
    b.append(&text);
    b
}

fn section(s: &Section, o: &Opts, tx: &Tx, rows: &mut Vec<(String, gtk::ListBoxRow)>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("task-section");
    if let Some(title) = &s.title {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("section-header");
        let t = ui::label(title, &["section-title"]);
        if matches!(s.kind, Kind::Day(_)) && matches!(o.nav, Nav::Scheduled) {
            t.add_css_class("day-number");
        }
        header.append(&t);
        if let Some(note) = &s.note {
            let n = ui::label(note, &["dim-label", "section-note"]);
            header.append(&n);
        }
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        header.append(&spacer);
        match &s.kind {
            Kind::Overdue => header.append(&reschedule(s, o, tx)),
            kind => {
                if let Some(target) = section_target(kind) {
                    let add = gtk::Button::from_icon_name("plus-large-symbolic");
                    add.add_css_class("flat");
                    add.add_css_class("section-add");
                    add.set_tooltip_text(Some("Add a task here"));
                    let tx = tx.clone();
                    add.connect_clicked(move |_| tx.emit(Msg::ShowAdd(Some(target.clone()))));
                    header.append(&add);
                }
            }
        }
        b.append(&header);
        let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
        rule.add_css_class("section-rule");
        b.append(&rule);
    }
    if s.tasks.is_empty() {
        return b;
    }
    let list = gtk::ListBox::new();
    list.add_css_class("task-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    {
        let tx = tx.clone();
        list.connect_row_activated(move |_, row| {
            tx.emit(Msg::Activate(row.widget_name().to_string()))
        });
    }
    let show_list = !o.nav.is_list() && !matches!(s.kind, Kind::List(_));
    // Dragging reorders a list shown in its own custom order.
    let on_drop: Option<row::Dropped> =
        (o.custom_order && matches!(s.kind, Kind::List(_))).then(|| {
            let tasks = s.tasks.clone();
            let tx = tx.clone();
            std::rc::Rc::new(move |moved: &str, target: &str, after: bool| {
                let orders = crate::model::reorder(&tasks, moved, target, after);
                if !orders.is_empty() {
                    tx.emit(Msg::Reorder(orders));
                }
            }) as row::Dropped
        });
    let fresh = |set: Option<&HashSet<String>>, href: &str| set.is_some_and(|s| s.contains(href));
    for t in &s.tasks {
        if o.editing == Some(t.href.as_str()) {
            let r = row::editor_row(&t.href, o.editor);
            list.append(&r);
            rows.push((t.href.clone(), r));
            continue;
        }
        let r = row::task_row(
            t,
            &RowOpts {
                now: o.now,
                show_list,
                completing: o.completing.contains(&t.href),
                just_checked: fresh(o.fresh.checked, &t.href),
                rolled: fresh(o.fresh.rolled, &t.href),
                select_mode: o.select_mode,
                selecting: o.fresh.selecting,
                selected: o.selected.contains(&t.href),
                lists: o.lists,
                sunday_first: o.sunday_first,
                on_drop: on_drop.clone(),
            },
            tx,
        );
        if fresh(o.fresh.rows, &t.href) {
            motion::arrive(&r);
        }
        list.append(&r);
        rows.push((t.href.clone(), r));
    }
    b.append(&list);
    b
}

/// Planify's Reschedule: one date for everything overdue, times kept.
fn reschedule(s: &Section, o: &Opts, tx: &Tx) -> gtk::MenuButton {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name("month-symbolic"));
    content.append(&gtk::Label::new(Some("Reschedule")));
    let button = gtk::MenuButton::builder().child(&content).build();
    button.add_css_class("flat");
    button.add_css_class("reschedule");
    let tasks = s.tasks.clone();
    let tx = tx.clone();
    let (zone, sunday_first) = (o.now.timezone(), o.sunday_first);
    button.set_create_popup_func(move |mb| {
        let tasks = tasks.clone();
        let tx = tx.clone();
        mb.set_popover(Some(&pickers::date_popover(
            Default::default(),
            DateOpts {
                sunday_first,
                full: false,
                clear: true,
            },
            move |sched| {
                let date = sched.due.as_ref().map(|d| d.local_date(zone));
                let edits = tasks
                    .iter()
                    .map(|t| {
                        let change = match date {
                            Some(date) => Change {
                                due: Some(Some(crate::model::on_date(
                                    t.task.due.as_ref(),
                                    date,
                                    zone,
                                ))),
                                ..Change::default()
                            },
                            // No date, and so no repeat either.
                            None => Change {
                                due: Some(None),
                                rrule: Some(None),
                                ..Change::default()
                            },
                        };
                        (t.href.clone(), change)
                    })
                    .collect();
                tx.emit(Msg::EditMany(edits));
            },
        )));
    });
    button
}

fn add_button(target: Target, tx: &Tx) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let plus = ui::icon("plus-large-symbolic", 14);
    plus.add_css_class("accent");
    content.append(&plus);
    content.append(&gtk::Label::new(Some("Add Task")));
    let b = gtk::Button::builder()
        .child(&content)
        .halign(gtk::Align::Start)
        .build();
    b.add_css_class("flat");
    b.add_css_class("add-button");
    let tx = tx.clone();
    b.connect_clicked(move |_| tx.emit(Msg::ShowAdd(Some(target.clone()))));
    b
}

fn empty(o: &Opts, tx: &Tx) -> adw::StatusPage {
    let (icon, title, text) = match o.nav {
        Nav::Today => (
            "star-outline-thick-symbolic",
            "All clear",
            "Nothing is due today. Press a to add a task.",
        ),
        Nav::Completed => (
            "check-round-outline-symbolic",
            "Nothing completed yet",
            "Completed tasks show up here.",
        ),
        Nav::Tomorrow => (
            "today-calendar-symbolic",
            "Nothing tomorrow",
            "Press a to add a task for tomorrow.",
        ),
        Nav::Repeating => (
            "arrow-circular-top-right-symbolic",
            "No repeating tasks",
            "Give a task a repeat, like every monday.",
        ),
        _ => (
            "check-round-outline-whole-symbolic",
            "Add some tasks",
            "Press a to add a task.",
        ),
    };
    let page = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(text)
        .build();
    page.add_css_class("compact");
    page.add_css_class("view-empty");
    if !matches!(o.nav, Nav::Completed) {
        let add = gtk::Button::with_label("Add Task");
        add.add_css_class("pill");
        add.add_css_class("suggested-action");
        add.set_halign(gtk::Align::Center);
        let tx = tx.clone();
        add.connect_clicked(move |_| tx.emit(Msg::ShowAdd(None)));
        page.set_child(Some(&add));
    }
    page
}
