//! The Plan view: a day's tasks on an hour timeline, and an unscheduled
//! tray to drag from. The day's shape is computed in `model::day_plan`;
//! this module only draws it and turns drags into slot edits. A drag also
//! draws a ghost of the block the drop would create, at the same place a
//! real drop would take. The grid chrome (hour rows, now-line, day header,
//! scroller) is `daygrid`, shared with Time.

use std::cell::RefCell;
use std::collections::HashMap;

use asst_core::api::TaskView;
use chrono::NaiveDate;
use relm4::gtk::{self, gdk, glib, prelude::*};

use crate::daygrid::{self, DayStep};
use crate::model::{
    self, DayPlan, PLAN_DEFAULT_DURATION, PLAN_MIN_DURATION, PLAN_PX_PER_HOUR, PLAN_SNAP,
    PlanBlock, place, unplace,
};
use crate::row::DRAG_PREFIX;
use crate::window::{Msg, Tx};
use crate::{content, row, ui};

/// Height floor so a short slot is still clickable. Below half an hour a
/// lower floor keeps 15 min visibly shorter than 30.
const MIN_BLOCK_H: i32 = 22;
const MIN_BLOCK_H_SHORT: i32 = 12;

fn clock(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// Clock time in the app's voice: `9am`, `5:30pm`.
pub fn clock_label(min: u32) -> String {
    let t =
        chrono::NaiveTime::from_hms_opt(min / 60, min % 60, 0).unwrap_or(chrono::NaiveTime::MIN);
    asst_core::fmt::time_label(t)
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

fn block_top(b: &PlanBlock) -> i32 {
    daygrid::y_at(f64::from(b.start)) as i32
}

fn block_height(b: &PlanBlock) -> i32 {
    let natural = f64::from(b.duration) / 60.0 * PLAN_PX_PER_HOUR;
    let floor = if b.duration < 30 {
        MIN_BLOCK_H_SHORT
    } else {
        MIN_BLOCK_H
    };
    natural.max(floor as f64) as i32
}

/// The chip body both a placed block and the drag ghost wear.
fn block_chip(b: &PlanBlock, now_min: Option<u32>) -> gtk::Box {
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
    if b.duration < 30 {
        // Short enough that the chip would read the same as a half hour:
        // say the length so the difference is legible either way.
        let meta = gtk::Label::new(Some(&format!("{}m", b.duration)));
        meta.add_css_class("plan-block-meta");
        meta.set_xalign(0.0);
        chip.append(&meta);
    } else if block_height(b) >= 36 {
        let meta = gtk::Label::new(Some(&format!(
            "{} – {}",
            clock(b.start),
            clock(b.start + b.duration)
        )));
        meta.add_css_class("plan-block-meta");
        meta.set_xalign(0.0);
        chip.append(&meta);
    }
    chip
}

/// Equal-expand spacers put `chip` in `col` of `cols` across the grid.
fn block_row(chip: &gtk::Box, col: usize, cols: usize) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("plan-block-row");
    row.set_hexpand(true);
    let cols = cols.max(1);
    for i in 0..cols {
        if i == col {
            row.append(chip);
        } else {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_hexpand(true);
            row.append(&spacer);
        }
    }
    row
}

/// One placed task: click opens the editor, drag moves, the edge resizes,
/// right-click sets start and duration by hand. The wrap is focusable so
/// vim j/k can land on it like a list row.
fn block_widget(b: &PlanBlock, now_min: Option<u32>, ghost: &Ghost, tx: &Tx) -> gtk::Widget {
    let chip = block_chip(b, now_min);

    // Bottom edge: drag to change the duration, with a ghost of the new end.
    // Thinner on a short chip so the floor can keep it short.
    let edge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    edge.add_css_class("plan-block-edge");
    edge.set_height_request(if b.duration < 30 { 3 } else { 6 });
    edge.set_valign(gtk::Align::End);
    edge.set_cursor_from_name(Some("ns-resize"));
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let href = b.task.href.clone();
        let task = b.task.clone();
        let start = b.start;
        let dur0 = b.duration;
        let (ghost, tx) = (ghost.clone(), tx.clone());
        {
            let (ghost, task) = (ghost.clone(), task.clone());
            drag.connect_drag_update(move |_, _, dy| {
                let end = model::edge_end(start, dur0, dy);
                let duration = end.saturating_sub(start).max(PLAN_MIN_DURATION);
                ghost.show(&task, start, duration);
            });
        }
        drag.connect_drag_end(move |_, _, dy| {
            ghost.hide();
            let end = model::edge_end(start, dur0, dy);
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
        let task = b.task.clone();
        let start = b.start;
        let dur = b.duration;
        let (ghost, tx) = (ghost.clone(), tx.clone());
        {
            let (ghost, task, moved) = (ghost.clone(), task.clone(), moved.clone());
            drag.connect_drag_update(move |_, _, dy| {
                if dy.abs() < 3.0 {
                    return;
                }
                moved.set(true);
                ghost.show(&task, model::move_start(start, dy), dur);
            });
        }
        let moved = moved.clone();
        drag.connect_drag_end(move |_, _, dy| {
            ghost.hide();
            if dy.abs() < 3.0 {
                moved.set(false);
                return;
            }
            moved.set(true);
            let to = model::move_start(start, dy);
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
            if let Some(w) = gesture.widget() {
                pop.set_parent(&w);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
            pop.popup();
        });
        chip.add_controller(click);
    }

    let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    wrap.set_widget_name(&b.task.href);
    wrap.set_focusable(true);
    wrap.add_css_class("plan-block-focus");
    wrap.append(&block_row(&chip, b.col, b.cols));
    wrap.upcast()
}

/// Start, duration and Take off the plan for one slot. Start speaks clock
/// time (`9am`), duration stays in minutes and is labeled as such.
fn slot_popover(href: &str, start: u32, duration: u32, tx: &Tx) -> gtk::Popover {
    let href = href.to_string();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_margin_top(8);
    box_.set_margin_bottom(8);
    box_.set_margin_start(12);
    box_.set_margin_end(12);

    // Start and duration as clock time and minutes, never raw 540.
    let start_cell = std::rc::Rc::new(std::cell::Cell::new(start));
    let dur_cell = std::rc::Rc::new(std::cell::Cell::new(duration));

    let start_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    start_row.append(&ui::label("Start", &["dim-label"]));
    let start_lab = ui::label(&clock_label(start), &["plan-time-label"]);
    start_lab.set_widget_name("plan-start-clock");
    let start_minus = gtk::Button::from_icon_name("list-remove-symbolic");
    start_minus.add_css_class("flat");
    let start_plus = gtk::Button::from_icon_name("list-add-symbolic");
    start_plus.add_css_class("flat");
    {
        let (cell, lab) = (start_cell.clone(), start_lab.clone());
        start_minus.connect_clicked(move |_| {
            let v = cell.get().saturating_sub(PLAN_SNAP);
            cell.set(v);
            lab.set_text(&clock_label(v));
        });
    }
    {
        let (cell, lab) = (start_cell.clone(), start_lab.clone());
        start_plus.connect_clicked(move |_| {
            let v = (cell.get() + PLAN_SNAP).min(23 * 60 + 45);
            cell.set(v);
            lab.set_text(&clock_label(v));
        });
    }
    start_row.append(&start_minus);
    start_row.append(&start_lab);
    start_row.append(&start_plus);

    let dur_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    dur_row.append(&ui::label("Duration", &["dim-label"]));
    let dur_lab = ui::label(
        &asst_core::fmt::minutes(u64::from(duration)),
        &["plan-time-label"],
    );
    dur_lab.set_widget_name("plan-duration-label");
    let dur_minus = gtk::Button::from_icon_name("list-remove-symbolic");
    dur_minus.add_css_class("flat");
    let dur_plus = gtk::Button::from_icon_name("list-add-symbolic");
    dur_plus.add_css_class("flat");
    {
        let (cell, lab) = (dur_cell.clone(), dur_lab.clone());
        dur_minus.connect_clicked(move |_| {
            let v = cell.get().saturating_sub(PLAN_SNAP).max(PLAN_MIN_DURATION);
            cell.set(v);
            lab.set_text(&asst_core::fmt::minutes(u64::from(v)));
        });
    }
    {
        let (cell, lab) = (dur_cell.clone(), dur_lab.clone());
        dur_plus.connect_clicked(move |_| {
            let v = (cell.get() + PLAN_SNAP).min(12 * 60);
            cell.set(v);
            lab.set_text(&asst_core::fmt::minutes(u64::from(v)));
        });
    }
    dur_row.append(&dur_minus);
    dur_row.append(&dur_lab);
    dur_row.append(&dur_plus);

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
        let (start_cell, dur_cell) = (start_cell.clone(), dur_cell.clone());
        let pop = pop.clone();
        apply.connect_clicked(move |_| {
            let s = start_cell.get();
            let d = dur_cell.get().max(PLAN_MIN_DURATION);
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

/// Open the slot popover on `parent` (devel drive, and the block's own
/// right-click path shares `slot_popover`).
pub fn open_slot_popover(parent: &gtk::Widget, href: &str, start: u32, duration: u32, tx: &Tx) {
    let pop = slot_popover(href, start, duration, tx);
    pop.set_parent(parent);
    pop.popup();
}

/// The live drop preview: one overlay child the drag callbacks rewrite.
/// It wears the block a drop would create, so the pointer movement is the
/// only thing that changes it. The original stays put while a placed
/// block is dragged; the ghost is the new home.
#[derive(Clone)]
struct Ghost {
    wrap: gtk::Box,
    /// Blocks on the grid when this column was built, for overlap columns.
    blocks: Vec<PlanBlock>,
    /// Tray and placed tasks by href, for what a tray drag carries.
    tasks: HashMap<String, TaskView>,
}

thread_local! {
    /// The Plan grid's ghost, so the drive verbs can pose one.
    static GHOST: RefCell<Option<Ghost>> = const { RefCell::new(None) };
}

impl Ghost {
    /// Build the preview host. `attach` puts it on the grid last, so it
    /// draws over the blocks it previews against.
    fn new(p: &DayPlan) -> Self {
        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.set_visible(false);
        wrap.set_widget_name("plan-ghost");
        let mut tasks = HashMap::new();
        for b in &p.blocks {
            tasks.insert(b.task.href.clone(), b.task.clone());
        }
        for t in &p.tray {
            tasks.insert(t.href.clone(), t.clone());
        }
        let g = Ghost {
            wrap,
            blocks: p.blocks.clone(),
            tasks,
        };
        GHOST.with(|s| *s.borrow_mut() = Some(g.clone()));
        g
    }

    fn attach(&self, overlay: &gtk::Overlay) {
        overlay.add_overlay(&self.wrap);
    }

    fn show(&self, task: &TaskView, start: u32, duration: u32) {
        let b = model::ghost_block(&self.blocks, task, start, duration);
        while let Some(c) = self.wrap.first_child() {
            self.wrap.remove(&c);
        }
        let chip = block_chip(&b, None);
        chip.add_css_class("ghost");
        let row = block_row(&chip, b.col, b.cols);
        self.wrap.append(&row);
        self.wrap.set_margin_top(block_top(&b));
        self.wrap.set_visible(true);
    }

    fn hide(&self) {
        self.wrap.set_visible(false);
        while let Some(c) = self.wrap.first_child() {
            self.wrap.remove(&c);
        }
    }
}

/// Pose the drop preview the drive verbs ask for.
pub fn ghost_at(href: &str, start: u32, duration: u32) {
    GHOST.with(|s| {
        let held = s.borrow();
        let Some(g) = held.as_ref() else { return };
        let Some(task) = g.tasks.get(href) else {
            return;
        };
        g.show(task, start, duration);
    });
}

/// Hide the drop preview (the drive verbs, and a rebuild of the column).
pub fn ghost_off() {
    GHOST.with(|s| {
        if let Some(g) = s.borrow().as_ref() {
            g.hide();
        }
    });
}

/// Plan's blocks and drop target on the shared grid. The ghost goes last
/// so it draws over what it previews against.
fn plan_grid(
    p: &DayPlan,
    now: chrono::DateTime<chrono_tz::Tz>,
    tx: &Tx,
    rows: &mut Vec<(String, gtk::Widget)>,
) -> gtk::Widget {
    daygrid::day_grid(p.day, now, |overlay| {
        let ghost = Ghost::new(p);
        for b in &p.blocks {
            let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
            wrap.set_halign(gtk::Align::Fill);
            wrap.set_valign(gtk::Align::Start);
            wrap.set_hexpand(true);
            wrap.set_margin_top(block_top(b));
            let row = block_widget(b, p.now_min, &ghost, tx);
            rows.push((b.task.href.clone(), row.clone()));
            wrap.append(&row);
            overlay.add_overlay(&wrap);
        }
        ghost.attach(overlay);

        // Dropping a tray task (or a block dragged out and back) lands it
        // at the pointer's time. The ghost tracks that place while the drag
        // is on.
        let target = gtk::DropTarget::new(
            glib::Type::STRING,
            gdk::DragAction::MOVE | gdk::DragAction::COPY,
        );
        {
            let ghost = ghost.clone();
            target.connect_motion(move |_, _, y| {
                match row::dragging().and_then(|href| ghost.tasks.get(&href).cloned()) {
                    Some(task) => {
                        ghost.show(
                            &task,
                            model::minutes_at(y, daygrid::HOUR_START),
                            PLAN_DEFAULT_DURATION,
                        );
                    }
                    None => ghost.hide(),
                }
                // One preferred action: the gesture itself is a move of the slot.
                gdk::DragAction::MOVE
            });
        }
        {
            let ghost = ghost.clone();
            target.connect_leave(move |_| ghost.hide());
        }
        {
            let (ghost, tx) = (ghost.clone(), tx.clone());
            target.connect_drop(move |_, value, _, y| {
                ghost.hide();
                let Ok(text) = value.get::<String>() else {
                    return false;
                };
                let Some(href) = text.strip_prefix(DRAG_PREFIX) else {
                    return false;
                };
                tx.emit(Msg::PlanSet(
                    href.to_string(),
                    model::minutes_at(y, daygrid::HOUR_START),
                    PLAN_DEFAULT_DURATION,
                ));
                true
            });
        }
        overlay.add_controller(target);
    })
}

fn tray(
    p: &DayPlan,
    o: &content::Opts<'_>,
    tx: &Tx,
    rows: &mut Vec<(String, gtk::Widget)>,
) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("plan-tray");
    b.set_size_request(220, -1);
    b.set_hexpand(false);
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
        let empty = if p.day == o.now.date_naive() {
            "Every task due today is on the timeline."
        } else {
            "Nothing left to place on this day."
        };
        b.append(&ui::label(empty, &["dim-label", "caption"]));
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
            let r = row::editor_row(&t.href, o.editor);
            list.append(&r);
            rows.push((t.href.clone(), r.into()));
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
        rows.push((t.href.clone(), r.into()));
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

    let today = o.now.date_naive();
    let note = format!("{} planned · {} unscheduled", p.blocks.len(), p.tray.len());
    root.append(&daygrid::day_header(
        "Plan",
        p.day,
        &note,
        today,
        DayStep::Plan,
        tx,
    ));

    let mut rows: Vec<(String, gtk::Widget)> = Vec::new();
    if let Some(href) = o.editing {
        let list = gtk::ListBox::new();
        list.add_css_class("task-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let r = row::editor_row(href, o.editor);
        list.append(&r);
        rows.push((href.to_string(), r.into()));
        root.append(&list);
    }

    let cols = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    cols.add_css_class("plan-columns");
    cols.set_vexpand(true);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    // The day grid is taller than a short column; ask for most of a window
    // so the overlay (blocks and the now-line) is allocated the full grid.
    scroll.set_min_content_height(400);
    scroll.add_css_class("time-scroll");
    scroll.add_css_class("plan-scroll");
    scroll.set_child(Some(&plan_grid(p, o.now, tx, &mut rows)));
    cols.append(&scroll);
    cols.append(&tray(p, o, tx, &mut rows));
    root.append(&cols);

    let now_min = p.now_min.map(f64::from);
    let first_min = p
        .blocks
        .iter()
        .map(|b| f64::from(b.start))
        .fold(None, |acc: Option<f64>, m| {
            Some(acc.map_or(m, |a| a.min(m)))
        });
    daygrid::attach_scroll(
        &scroll,
        DayStep::Plan,
        p.day,
        today,
        now_min,
        first_min,
        o.fresh.view,
    );

    content::Built { root, rows }
}

/// Place or move `href`'s slot for `date` in the window's prefs.
pub fn apply_slot(
    prefs: &mut crate::prefs::Prefs,
    open: &[asst_core::api::TaskView],
    href: &str,
    date: NaiveDate,
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
    date: NaiveDate,
) -> bool {
    let Some(t) = open.iter().find(|t| t.href == href) else {
        return false;
    };
    unplace(&mut prefs.plan, &t.task.uid, date);
    true
}
