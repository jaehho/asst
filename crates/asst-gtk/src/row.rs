//! A task as a row, as Planify draws one: a checkbox in its priority's
//! color, the due date in a colored chip, the title, small icons for notes,
//! reminders, a link and linked notes, and the list's name in views that
//! span lists.
//! Right-click for the task menu; drag it onto a list or a view.

use asst_core::api::{Change, ListView, TaskView};
use asst_core::fmt;
use asst_core::task::priority_level;
use chrono::{DateTime, Duration};
use chrono_tz::Tz;
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::pickers::{self, DateOpts, Schedule};
use crate::ui::{self, Item};
use crate::window::{Msg, Tx};
use crate::{model, motion};

/// What a drag of a task carries.
pub const DRAG_PREFIX: &str = "asst-task:";

pub struct RowOpts<'a> {
    pub now: DateTime<Tz>,
    pub show_list: bool,
    pub completing: bool,
    /// Checked since the row was last drawn: the box pops.
    pub just_checked: bool,
    /// A repeating task checked and moved on: its new date lights up.
    pub rolled: bool,
    pub select_mode: bool,
    /// Select mode just began: the box slides in.
    pub selecting: bool,
    pub selected: bool,
    pub lists: &'a [ListView],
    pub sunday_first: bool,
    /// In a list shown in custom order: a task dropped on this row moves to
    /// just before it, or after it when dropped on its lower half.
    pub on_drop: Option<Dropped>,
}

/// `(moved, target, after)`
pub type Dropped = std::rc::Rc<dyn Fn(&str, &str, bool)>;

/// The change a schedule picker's answer makes, or none.
pub fn schedule_change(before: &TaskView, s: &Schedule) -> Option<Change> {
    let mut c = Change::default();
    if s.due != before.task.due {
        c.due = Some(s.due.clone());
    }
    if s.rrule != before.task.rrule {
        c.rrule = Some(s.rrule.clone());
    }
    (c != Change::default()).then_some(c)
}

pub fn task_row(t: &TaskView, o: &RowOpts, tx: &Tx) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("task-row");
    row.set_widget_name(&t.href);

    let item = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    item.add_css_class("task-item");
    let finished = !t.task.is_open();
    if finished || o.completing {
        item.add_css_class("done");
    }
    if o.completing {
        item.add_css_class("completing");
    }
    if o.just_checked {
        item.add_css_class("just-checked");
    }
    if o.selected {
        item.add_css_class("selected");
    }

    let level = priority_level(t.task.priority);
    let check = gtk::CheckButton::new();
    check.set_active(finished || o.completing);
    check.set_valign(gtk::Align::Center);
    check.add_css_class("task-check");
    check.add_css_class(&format!("priority-{level}"));
    check.set_tooltip_text(Some(if finished { "Reopen" } else { "Complete" }));
    {
        let tx = tx.clone();
        let href = t.href.clone();
        check.connect_toggled(move |c| tx.emit(Msg::Check(href.clone(), c.is_active())));
    }
    item.append(&check);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    body.add_css_class("task-body");
    body.set_hexpand(true);
    if finished {
        if let Some(at) = t.task.completed {
            let day = at.with_timezone(&o.now.timezone()).date_naive();
            let text = model::capitalize(&fmt::day_label(day, o.now.date_naive()));
            body.append(&ui::chip(&text, "done", false));
        }
    } else if let Some(due) = &t.task.due {
        let (text, class) = model::due_chip(due, o.now);
        let chip = ui::chip(&text, class, t.task.rrule.is_some());
        if o.rolled {
            chip.add_css_class("rolled");
        }
        if let Some(r) = &t.task.rrule {
            chip.set_tooltip_text(Some(&model::capitalize(&fmt::repeat_label(r))));
        }
        body.append(&chip);
    }
    let title = gtk::Label::builder()
        .label(&t.task.summary)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    title.add_css_class("task-title");
    title.set_tooltip_text(Some(&t.task.summary));
    body.append(&title);

    if let Some(notes) = t
        .task
        .description
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        let i = ui::icon("paper-symbolic", 12);
        i.add_css_class("dim-label");
        let first: String = notes
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect();
        i.set_tooltip_text(Some(&first));
        body.append(&i);
    }
    if !t.task.alarms.is_empty() {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        b.add_css_class("dim-label");
        b.append(&ui::icon("alarm-symbolic", 12));
        let n = gtk::Label::new(Some(&t.task.alarms.len().to_string()));
        n.add_css_class("caption");
        b.append(&n);
        body.append(&b);
    }
    if t.task.url.is_some() {
        let i = ui::icon("chain-link-loose-symbolic", 12);
        i.add_css_class("dim-label");
        i.set_tooltip_text(t.task.url.as_deref());
        body.append(&i);
    }
    if !t.task.linked_notes.is_empty() {
        let i = ui::icon("mail-attachment-symbolic", 12);
        i.add_css_class("dim-label");
        let names: Vec<&str> = t
            .task
            .linked_notes
            .iter()
            .map(|l| asst_core::note_files::name(l))
            .collect();
        i.set_tooltip_text(Some(&names.join("\n")));
        body.append(&i);
    }
    if t.pending {
        let i = ui::icon("update-symbolic", 12);
        i.add_css_class("dim-label");
        i.set_tooltip_text(Some("Not on the server yet"));
        body.append(&i);
    }
    if o.show_list {
        let l = gtk::Label::builder()
            .label(&t.list_name)
            .hexpand(true)
            .halign(gtk::Align::End)
            .max_width_chars(16)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        l.add_css_class("caption");
        l.add_css_class("dim-label");
        body.append(&l);
    }
    item.append(&body);

    if o.select_mode {
        let sel = gtk::CheckButton::new();
        sel.set_active(o.selected);
        sel.add_css_class("selection-mode");
        sel.set_valign(gtk::Align::Center);
        let tx = tx.clone();
        let href = t.href.clone();
        sel.connect_toggled(move |_| tx.emit(Msg::SelectToggle(href.clone())));
        if o.selecting {
            item.append(&motion::opening(
                &sel,
                gtk::RevealerTransitionType::SlideLeft,
            ));
        } else {
            item.append(&sel);
        }
    }
    row.set_child(Some(&item));

    // Ctrl+click selects, as in Planify.
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let tx = tx.clone();
        let href = t.href.clone();
        click.connect_pressed(move |g, _, _, _| {
            if g.current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                g.set_state(gtk::EventSequenceState::Claimed);
                tx.emit(Msg::SelectToggle(href.clone()));
            }
        });
    }
    row.add_controller(click);

    let menu_click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    {
        let tx = tx.clone();
        let t = t.clone();
        let lists = o.lists.to_vec();
        let (now, sunday_first) = (o.now, o.sunday_first);
        let row_weak = row.downgrade();
        menu_click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            if let Some(row) = row_weak.upgrade() {
                let at = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                let menu = task_menu(&t, &lists, now, sunday_first, row.upcast_ref(), at, &tx);
                show_at(&menu, &row, at);
            }
        });
    }
    row.add_controller(menu_click);

    if !finished {
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE | gdk::DragAction::COPY);
        let payload = format!("{DRAG_PREFIX}{}", t.href);
        drag.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(&payload.to_value()))
        });
        let item_weak = item.downgrade();
        drag.connect_drag_begin(move |src, _| {
            if let Some(item) = item_weak.upgrade() {
                src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&item))), 12, 12);
            }
        });
        row.add_controller(drag);
    }
    if let Some(on_drop) = o.on_drop.clone() {
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
        let href = t.href.clone();
        target.connect_drop(move |t, value, _, y| {
            if let Some(w) = t.widget() {
                w.remove_css_class("drop-above");
                w.remove_css_class("drop-below");
            }
            let Some(moved) = value
                .get::<String>()
                .ok()
                .and_then(|v| v.strip_prefix(DRAG_PREFIX).map(str::to_string))
            else {
                return false;
            };
            on_drop(&moved, &href, lower_half(t, y));
            true
        });
        row.add_controller(target);
    }
    row
}

/// The row of the task open in place: the editor, moved in from wherever
/// it was. Clicks in it edit rather than open, and it doesn't drag.
pub fn editor_row(href: &str, editor: &gtk::Box) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("task-row");
    row.add_css_class("editing");
    row.set_widget_name(href);
    row.set_activatable(false);
    row.set_focusable(false);
    ui::detach(editor);
    row.set_child(Some(editor));
    row
}

/// Pop a popover up at a point in `parent`, and let go of it once closed.
pub fn show_at(p: &gtk::Popover, parent: &impl IsA<gtk::Widget>, at: gdk::Rectangle) {
    p.set_parent(parent);
    p.set_pointing_to(Some(&at));
    p.set_position(gtk::PositionType::Bottom);
    p.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || {
            if p.parent().is_some() {
                p.unparent();
            }
        });
    });
    p.popup();
}

/// Planify's task menu: complete, dates, priority, move, duplicate, delete.
pub fn task_menu(
    t: &TaskView,
    lists: &[ListView],
    now: DateTime<Tz>,
    sunday_first: bool,
    anchor: &gtk::Widget,
    at: gdk::Rectangle,
    tx: &Tx,
) -> gtk::Popover {
    let zone = now.timezone();
    let today = now.date_naive();
    let href = t.href.clone();
    let mut items: Vec<gtk::Widget> = Vec::new();
    let send = {
        let tx = tx.clone();
        move |m: Msg| tx.emit(m)
    };

    if t.task.is_open() {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("check-round-outline-symbolic"), "Complete")
                .secondary("x")
                .build(move || send(Msg::Complete(h.clone())))
                .upcast(),
        );
    } else {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("check-round-outline-symbolic"), "Reopen")
                .secondary("x")
                .build(move || send(Msg::Reopen(h.clone())))
                .upcast(),
        );
    }
    {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("edit-symbolic"), "Edit")
                .secondary("e")
                .build(move || send(Msg::Open(h.clone())))
                .upcast(),
        );
    }
    items.push(ui::separator());

    let move_to = |date: chrono::NaiveDate| Change {
        due: Some(Some(model::on_date(t.task.due.as_ref(), date, zone))),
        ..Change::default()
    };
    for (icon, title, date, key) in [
        ("star-outline-thick-symbolic", "Today", today, "t"),
        (
            "today-calendar-symbolic",
            "Tomorrow",
            today + Duration::days(1),
            "m",
        ),
        (
            "month-symbolic",
            "Next week",
            today + Duration::days(7),
            "w",
        ),
    ] {
        let (send, h) = (send.clone(), href.clone());
        let change = move_to(date);
        let secondary = if key == "w" {
            date.format("%a %b %-d").to_string()
        } else {
            date.format("%a").to_string()
        };
        items.push(
            Item::new(Some(icon), title)
                .secondary(&secondary)
                .build(move || send(Msg::Edit(h.clone(), Box::new(change.clone()))))
                .upcast(),
        );
    }
    {
        let tx = tx.clone();
        let t2 = t.clone();
        let anchor = anchor.clone();
        items.push(
            Item::new(Some("month-symbolic"), "Pick a date…")
                .build(move || {
                    let (tx, t3) = (tx.clone(), t2.clone());
                    let picker = pickers::date_popover(
                        Schedule::of(&t2.task),
                        DateOpts {
                            sunday_first,
                            full: true,
                            clear: false,
                        },
                        move |s| {
                            if let Some(c) = schedule_change(&t3, &s) {
                                tx.emit(Msg::Edit(t3.href.clone(), Box::new(c)));
                            }
                        },
                    );
                    let anchor = anchor.clone();
                    glib::idle_add_local_once(move || show_at(&picker, &anchor, at));
                })
                .upcast(),
        );
    }
    if t.task.due.is_some() {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("cross-large-circle-filled-symbolic"), "No date")
                .build(move || {
                    send(Msg::Edit(
                        h.clone(),
                        Box::new(Change {
                            due: Some(None),
                            rrule: Some(None),
                            ..Change::default()
                        }),
                    ))
                })
                .upcast(),
        );
    }
    items.push(ui::separator());

    // Priorities side by side.
    let flags = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    flags.set_homogeneous(true);
    flags.set_margin_start(6);
    flags.set_margin_end(6);
    let current = priority_level(t.task.priority);
    for level in 1..=4u8 {
        let b = gtk::Button::from_icon_name("flag-outline-thick-symbolic");
        b.add_css_class("flat");
        b.add_css_class(&format!("priority-{level}-button"));
        if level == current {
            b.add_css_class("current");
        }
        b.set_tooltip_text(Some(model::priority_name(level)));
        let (send, h) = (send.clone(), href.clone());
        b.connect_clicked(move |b| {
            ui::popdown(b);
            send(Msg::Edit(
                h.clone(),
                Box::new(Change {
                    priority: Some(level),
                    ..Change::default()
                }),
            ));
        });
        flags.append(&b);
    }
    items.push(flags.upcast());
    items.push(ui::separator());

    {
        let tx = tx.clone();
        let lists = lists.to_vec();
        let (anchor, h, list) = (anchor.clone(), href.clone(), t.list.clone());
        items.push(
            Item::new(Some("arrow3-right-symbolic"), "Move to…")
                .build(move || {
                    let (tx, h) = (tx.clone(), h.clone());
                    let picker = pickers::list_popover(&lists, Some(&list), move |to| {
                        tx.emit(Msg::Move(h.clone(), to))
                    });
                    let anchor = anchor.clone();
                    glib::idle_add_local_once(move || show_at(&picker, &anchor, at));
                })
                .upcast(),
        );
    }
    {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("tabs-stack-symbolic"), "Duplicate")
                .build(move || send(Msg::Duplicate(h.clone())))
                .upcast(),
        );
    }
    {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("clipboard-symbolic"), "Copy to clipboard")
                .build(move || send(Msg::Copy(h.clone())))
                .upcast(),
        );
    }
    items.push(ui::separator());
    {
        let (send, h) = (send.clone(), href.clone());
        items.push(
            Item::new(Some("user-trash-symbolic"), "Delete task")
                .secondary("dd")
                .danger()
                .build(move || send(Msg::Delete(h.clone())))
                .upcast(),
        );
    }
    let p = ui::menu(&items);
    p.set_width_request(260);
    p
}
