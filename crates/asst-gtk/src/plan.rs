//! The Plan view: today's tasks on an hour timeline, and an unscheduled
//! tray to drag from. The day's shape is computed in `model::day_plan`;
//! this module only draws it and turns drags into slot edits.

use relm4::gtk::{self, gdk, glib, prelude::*};

use crate::model::{DayPlan, PLAN_DEFAULT_DURATION, PLAN_MIN_DURATION, PlanBlock, place, unplace};
use crate::row::DRAG_PREFIX;
use crate::window::{Msg, Tx};
use crate::{content, row, ui};

/// Pixels per hour on the day grid (as in Time).
const HOUR_H: i32 = 52;
/// Width of the hour-label gutter.
const GUTTER: i32 = 56;
/// Height floor so a short slot is still clickable.
const MIN_BLOCK_H: i32 = 22;
/// Minutes a drop or a drag snaps to.
const SNAP: u32 = 15;

fn hour_label(hour: u32) -> String {
    match hour {
        0 => "12 AM".into(),
        12 => "12 PM".into(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

fn clock(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn snap(min: i32) -> u32 {
    let s = SNAP as i32;
    ((min + s / 2) / s * s).clamp(0, 24 * 60 - 1) as u32
}

/// The tint a block wears: its priority's color, like the row's checkbox.
fn tint_for(priority: u8) -> &'static str {
    match asst_core::task::priority_level(priority) {
        1 => "tint-orange",
        2 => "tint-purple",
        3 => "tint-blue",
        _ => "tint-green",
    }
}

fn block_top(b: &PlanBlock, hour_start: u32) -> i32 {
    (f64::from(b.start) / 60.0 * f64::from(HOUR_H) - f64::from(hour_start) * f64::from(HOUR_H))
        as i32
}

fn block_height(b: &PlanBlock) -> i32 {
    (b.duration as f64 / 60.0 * f64::from(HOUR_H)).max(MIN_BLOCK_H as f64) as i32
}

/// One placed task: click opens the editor, drag moves, the edge resizes,
/// right-click sets start and duration by hand. Equal-expand spacers put
/// the chip in `col` of `cols` across the grid.
fn block_widget(b: &PlanBlock, now_min: Option<u32>, tx: &Tx) -> gtk::Widget {
    let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chip.add_css_class("plan-block");
    chip.add_css_class(tint_for(b.task.task.priority));
    if now_min.is_some_and(|n| b.start <= n && n < b.start + b.duration) {
        chip.add_css_class("running");
    }
    chip.set_height_request(block_height(b));
    chip.set_hexpand(true);
    chip.set_tooltip_text(Some(&format!(
        "{} – {} · {} · {}",
        clock(b.start),
        clock(b.start + b.duration),
        b.task.list_name,
        b.task.task.summary
    )));

    let title = gtk::Label::new(Some(&b.task.task.summary));
    title.add_css_class("plan-block-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    chip.append(&title);
    if block_height(b) >= 36 {
        let meta = gtk::Label::new(Some(&format!(
            "{} – {}",
            clock(b.start),
            clock(b.start + b.duration)
        )));
        meta.add_css_class("plan-block-meta");
        meta.set_xalign(0.0);
        chip.append(&meta);
    }

    // Bottom edge: drag to change the duration.
    let edge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    edge.add_css_class("plan-block-edge");
    edge.set_height_request(6);
    edge.set_valign(gtk::Align::End);
    edge.set_cursor_from_name(Some("ns-resize"));
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let href = b.task.href.clone();
        let start = b.start;
        let dur0 = b.duration;
        let tx = tx.clone();
        drag.connect_drag_end(move |_, _, dy| {
            let end = snap(start as i32 + dur0 as i32 + (dy / f64::from(HOUR_H) * 60.0) as i32);
            let duration = end.saturating_sub(start).max(PLAN_MIN_DURATION);
            tx.emit(Msg::PlanSet(href.clone(), start, duration));
        });
        edge.add_controller(drag);
    }
    chip.append(&edge);

    let moved = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let href = b.task.href.clone();
        let start = b.start;
        let dur = b.duration;
        let moved = moved.clone();
        let tx = tx.clone();
        drag.connect_drag_end(move |_, _, dy| {
            if dy.abs() < 3.0 {
                moved.set(false);
                return;
            }
            moved.set(true);
            let to = snap(start as i32 + (dy / f64::from(HOUR_H) * 60.0) as i32);
            tx.emit(Msg::PlanSet(href.clone(), to, dur));
        });
        chip.add_controller(drag);
    }
    {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        let href = b.task.href.clone();
        let moved = moved.clone();
        let tx = tx.clone();
        click.connect_released(move |_, _, _, _| {
            if !moved.get() {
                tx.emit(Msg::Open(href.clone()));
            }
            moved.set(false);
        });
        chip.add_controller(click);
    }
    {
        let click = gtk::GestureClick::new();
        click.set_button(3);
        let href = b.task.href.clone();
        let start = b.start;
        let duration = b.duration;
        let tx = tx.clone();
        click.connect_pressed(move |gesture, _, _, _| {
            let pop = slot_popover(&href, start, duration, &tx);
            gesture.set_state(gtk::EventSequenceState::Claimed);
            pop.popup();
        });
        chip.add_controller(click);
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("plan-block-row");
    row.set_hexpand(true);
    let cols = b.cols.max(1);
    for i in 0..cols {
        if i == b.col {
            row.append(&chip);
        } else {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_hexpand(true);
            row.append(&spacer);
        }
    }
    row.upcast()
}

/// Start, duration and Take off the plan for one slot.
fn slot_popover(href: &str, start: u32, duration: u32, tx: &Tx) -> gtk::Popover {
    let href = href.to_string();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_margin_top(8);
    box_.set_margin_bottom(8);
    box_.set_margin_start(12);
    box_.set_margin_end(12);

    let start_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    start_row.append(&ui::label("Start", &["dim-label"]));
    let start_spin = gtk::SpinButton::with_range(0.0, 23.0 * 60.0 + 45.0, 15.0);
    start_spin.set_value(f64::from(start));
    start_spin.set_numeric(true);
    start_row.append(&start_spin);

    let dur_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    dur_row.append(&ui::label("Duration", &["dim-label"]));
    let dur_spin = gtk::SpinButton::with_range(f64::from(PLAN_MIN_DURATION), 12.0 * 60.0, 15.0);
    dur_spin.set_value(f64::from(duration));
    dur_spin.set_numeric(true);
    dur_row.append(&dur_spin);

    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("pill");
    apply.add_css_class("suggested-action");
    let unplace_btn = gtk::Button::with_label("Take off the plan");
    unplace_btn.add_css_class("pill");
    unplace_btn.add_css_class("destructive-action");

    box_.append(&start_row);
    box_.append(&dur_row);
    box_.append(&apply);
    box_.append(&unplace_btn);

    let pop = ui::popover(&box_);
    {
        let (href, tx) = (href.clone(), tx.clone());
        let (start_spin, dur_spin) = (start_spin.clone(), dur_spin.clone());
        let pop = pop.clone();
        apply.connect_clicked(move |_| {
            let s = start_spin.value() as u32;
            let d = (dur_spin.value() as u32).max(PLAN_MIN_DURATION);
            tx.emit(Msg::PlanSet(href.clone(), s, d));
            pop.popdown();
        });
    }
    {
        let (href, tx) = (href.clone(), tx.clone());
        let pop = pop.clone();
        unplace_btn.connect_clicked(move |_| {
            tx.emit(Msg::PlanUnplace(href.clone()));
            pop.popdown();
        });
    }
    pop
}

/// Minutes from midnight for a y position on the grid.
fn minutes_at(y: f64, hour_start: u32) -> u32 {
    let mins = f64::from(hour_start) * 60.0 + y / f64::from(HOUR_H) * 60.0;
    snap(mins as i32)
}

fn day_grid(p: &DayPlan, tx: &Tx) -> gtk::Widget {
    let hours = p.hour_end.saturating_sub(p.hour_start).max(1);
    let height = i32::try_from(hours).unwrap_or(15) * HOUR_H;

    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(GUTTER, height);
    gutter.add_css_class("time-gutter");
    for h in p.hour_start..p.hour_end {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_size_request(GUTTER, HOUR_H);
        let lab = gtk::Label::new(Some(&hour_label(h)));
        lab.add_css_class("time-hour");
        lab.add_css_class("dim-label");
        lab.set_xalign(1.0);
        lab.set_yalign(0.0);
        lab.set_margin_top(-8);
        lab.set_margin_end(8);
        slot.append(&lab);
        gutter.append(&slot);
    }

    let rules = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rules.set_hexpand(true);
    rules.set_size_request(240, height);
    rules.add_css_class("time-grid");
    for _ in 0..hours {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_hexpand(true);
        slot.set_height_request(HOUR_H);
        slot.add_css_class("time-hour-slot");
        rules.append(&slot);
    }

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&rules));
    for b in &p.blocks {
        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.set_margin_top(block_top(b, p.hour_start));
        wrap.append(&block_widget(b, p.now_min, tx));
        overlay.add_overlay(&wrap);
    }

    // Dropping a tray task (or a block dragged out and back) lands it at
    // the pointer's time.
    let target = gtk::DropTarget::new(
        glib::Type::STRING,
        gdk::DragAction::MOVE | gdk::DragAction::COPY,
    );
    {
        let tx = tx.clone();
        let hour_start = p.hour_start;
        target.connect_drop(move |_, value, _, y| {
            let Ok(text) = value.get::<String>() else {
                return false;
            };
            let Some(href) = text.strip_prefix(DRAG_PREFIX) else {
                return false;
            };
            tx.emit(Msg::PlanSet(
                href.to_string(),
                minutes_at(y, hour_start),
                PLAN_DEFAULT_DURATION,
            ));
            true
        });
    }
    overlay.add_controller(target);

    if let Some(mins) = p.now_min {
        let y = f64::from(mins) / 60.0 * f64::from(HOUR_H)
            - f64::from(p.hour_start) * f64::from(HOUR_H);
        if y >= 0.0 && y <= f64::from(height) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.add_css_class("time-now-row");
            row.set_halign(gtk::Align::Fill);
            row.set_valign(gtk::Align::Start);
            row.set_margin_top((y - 7.0) as i32);
            let dot = gtk::Label::new(Some("●"));
            dot.add_css_class("time-now-dot");
            row.append(&dot);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            line.add_css_class("time-now");
            line.set_valign(gtk::Align::Center);
            line.set_hexpand(true);
            line.set_height_request(2);
            row.append(&line);
            overlay.add_overlay(&row);
        }
    }

    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.add_css_class("plan-planner");
    outer.set_size_request(GUTTER + 240, height);
    outer.append(&gutter);
    outer.append(&overlay);
    outer.upcast()
}

fn tray(p: &DayPlan, o: &content::Opts<'_>, tx: &Tx) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("plan-tray");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.add_css_class("section-header");
    head.append(&ui::label("Unscheduled", &["section-title"]));
    let n = p.tray.len();
    head.append(&ui::label(
        &match n {
            0 => "empty".to_string(),
            1 => "1 task".to_string(),
            n => format!("{n} tasks"),
        },
        &["dim-label", "section-note"],
    ));
    b.append(&head);
    let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
    rule.add_css_class("section-rule");
    b.append(&rule);
    if p.tray.is_empty() {
        b.append(&ui::label(
            "Every task due today is on the timeline.",
            &["dim-label", "caption"],
        ));
        return b;
    }
    let list = gtk::ListBox::new();
    list.add_css_class("task-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    {
        let tx = tx.clone();
        list.connect_row_activated(move |_, row| {
            tx.emit(Msg::Activate(row.widget_name().to_string()));
        });
    }
    for t in &p.tray {
        if o.editing == Some(t.href.as_str()) {
            list.append(&row::editor_row(&t.href, o.editor));
            continue;
        }
        let r = row::task_row(
            t,
            &row::RowOpts {
                now: o.now,
                show_list: true,
                completing: o.completing.contains(&t.href),
                just_checked: false,
                rolled: false,
                select_mode: o.select_mode,
                selecting: false,
                selected: o.selected.contains(&t.href),
                lists: o.lists,
                sunday_first: o.sunday_first,
                on_drop: None,
            },
            tx,
        );
        list.append(&r);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .propagate_natural_height(true)
        .max_content_height(360)
        .build();
    scroll.add_css_class("plan-tray-scroll");
    b.append(&scroll);
    b
}

/// The Plan column: timeline on the left, unscheduled tray on the right,
/// and the open task's card under the header when one is open.
pub fn build_column(
    o: &content::Opts<'_>,
    _sections: &[crate::model::Section],
    tx: &Tx,
) -> content::Built {
    let p = o.plan;
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("view-column");
    root.add_css_class("plan-view");
    if o.fresh.view {
        root.add_css_class("view-enter");
    }
    if o.editing.is_some() {
        root.add_css_class("focusing");
    }

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    head.add_css_class("view-title");
    let icon = ui::icon(crate::model::Nav::Plan.icon(), 18);
    icon.add_css_class("view-icon");
    icon.add_css_class(crate::model::Nav::Plan.tint());
    icon.set_valign(gtk::Align::Center);
    head.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    text.append(&ui::label("Plan", &["title-2"]));
    let planned = p.blocks.len();
    text.append(&ui::label(
        &format!(
            "{} · {} planned · {} unscheduled",
            o.now.format("%A, %B %-d"),
            planned,
            p.tray.len()
        ),
        &["caption", "dim-label"],
    ));
    head.append(&text);
    root.append(&head);

    if let Some(href) = o.editing {
        let list = gtk::ListBox::new();
        list.add_css_class("task-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.append(&row::editor_row(href, o.editor));
        root.append(&list);
    }

    let cols = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    cols.add_css_class("plan-columns");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.add_css_class("time-scroll");
    scroll.add_css_class("plan-scroll");
    scroll.set_child(Some(&day_grid(p, tx)));
    cols.append(&scroll);
    cols.append(&tray(p, o, tx));
    root.append(&cols);

    {
        let scroll = scroll.clone();
        let start = p.hour_start;
        glib::idle_add_local_once(move || {
            // Center the morning, or follow the now-line when it is on screen.
            let y = f64::from(start) * f64::from(HOUR_H);
            scroll.vadjustment().set_value(y);
        });
    }

    content::Built {
        root,
        rows: Vec::new(),
    }
}

/// Place or move `href`'s slot for `date` in the window's prefs.
pub fn apply_slot(
    prefs: &mut crate::prefs::Prefs,
    open: &[asst_core::api::TaskView],
    href: &str,
    date: chrono::NaiveDate,
    start: u32,
    duration: u32,
) -> bool {
    let Some(t) = open.iter().find(|t| t.href == href) else {
        return false;
    };
    place(&mut prefs.plan, &t.task.uid, date, start, duration);
    true
}

/// Take `href` off `date`'s plan.
pub fn drop_slot(
    prefs: &mut crate::prefs::Prefs,
    open: &[asst_core::api::TaskView],
    href: &str,
    date: chrono::NaiveDate,
) -> bool {
    let Some(t) = open.iter().find(|t| t.href == href) else {
        return false;
    };
    unplace(&mut prefs.plan, &t.task.uid, date);
    true
}
