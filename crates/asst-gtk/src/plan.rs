//! Plan slots for the Day view's right lane: placed blocks on the hour
//! timeline, and a tray of tasks to place to drag from. The day's shape is
//! computed in `model::day_plan`; this module only draws it and turns
//! drags into slot edits. A drag also draws a ghost of the block the drop
//! would create, at the same place a real drop would take. The grid chrome
//! (hour rows, now-line, day header, scroller) is `daygrid`, shared with
//! the tracked-time lane.

use std::cell::RefCell;
use std::collections::HashMap;

use asst_core::api::TaskView;
use chrono::NaiveDate;
use relm4::gtk::{self, gdk, glib, prelude::*};

use crate::daygrid;
use crate::model::{
    self, DayPlan, PLAN_DEFAULT_DURATION, PLAN_MIN_DURATION, PLAN_SNAP, PlanBlock, clock_label,
    place, unplace,
};
use crate::row::DRAG_PREFIX;
use crate::window::{Msg, Tx};
use crate::{content, row, ui};

/// Height floor so a short slot is still clickable. Below half an hour a
/// lower floor keeps 15 min visibly shorter than 30.
const MIN_BLOCK_H: i32 = 22;
const MIN_BLOCK_H_SHORT: i32 = 12;

/// The tint a block wears: its priority's color, like the row's checkbox.
fn tint_for(priority: u8) -> &'static str {
    match asst_core::task::priority_level(priority) {
        1 => "tint-orange",
        2 => "tint-purple",
        3 => "tint-blue",
        _ => "tint-green",
    }
}

fn block_top(b: &PlanBlock, px_per_hour: f64) -> i32 {
    daygrid::y_at(f64::from(b.start), px_per_hour) as i32
}

fn block_height(b: &PlanBlock, px_per_hour: f64) -> i32 {
    natural_height(b, px_per_hour).max(f64::from(min_block_h(b))) as i32
}

fn natural_height(b: &PlanBlock, px_per_hour: f64) -> f64 {
    f64::from(b.duration) / 60.0 * px_per_hour
}

fn min_block_h(b: &PlanBlock) -> i32 {
    if b.duration < 30 {
        MIN_BLOCK_H_SHORT
    } else {
        MIN_BLOCK_H
    }
}

/// The chip body both a placed block and the drag ghost wear. The meta line
/// is built either way and gated by height, so a zoom that grows the chip
/// reveals it without a rebuild.
fn block_chip(
    b: &PlanBlock,
    now_min: Option<u32>,
    px_per_hour: f64,
) -> (gtk::Box, Option<gtk::Label>) {
    let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chip.add_css_class("plan-block");
    chip.add_css_class(tint_for(b.task.task.priority));
    if now_min.is_some_and(|n| b.start <= n && n < b.start + b.duration) {
        chip.add_css_class("running");
    }
    chip.set_height_request(block_height(b, px_per_hour));
    chip.set_hexpand(true);
    chip.set_tooltip_text(Some(&format!(
        "{} – {} · {} · {}",
        clock_label(b.start),
        clock_label(b.start + b.duration),
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
        let meta = gtk::Label::new(Some(&asst_core::fmt::minutes(u64::from(b.duration))));
        meta.add_css_class("plan-block-meta");
        meta.set_xalign(0.0);
        chip.append(&meta);
        (chip, None)
    } else {
        let meta = gtk::Label::new(Some(&format!(
            "{} – {}",
            clock_label(b.start),
            clock_label(b.start + b.duration)
        )));
        meta.add_css_class("plan-block-meta");
        meta.set_xalign(0.0);
        chip.append(&meta);
        meta.set_visible(block_height(b, px_per_hour) >= 36);
        (chip, Some(meta))
    }
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
fn block_widget(
    b: &PlanBlock,
    now_min: Option<u32>,
    ghost: &Ghost,
    tx: &Tx,
    px_per_hour: f64,
) -> (gtk::Widget, gtk::Box, Option<gtk::Label>) {
    let (chip, meta) = block_chip(b, now_min, px_per_hour);

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
                let end = model::edge_end(start, dur0, dy, daygrid::live_px(px_per_hour));
                let duration = end.saturating_sub(start).max(PLAN_MIN_DURATION);
                ghost.show(&task, start, duration);
            });
        }
        drag.connect_drag_end(move |_, _, dy| {
            ghost.hide();
            let end = model::edge_end(start, dur0, dy, daygrid::live_px(px_per_hour));
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
                ghost.show(
                    &task,
                    model::move_start(start, dy, daygrid::live_px(px_per_hour)),
                    dur,
                );
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
            let to = model::move_start(start, dy, daygrid::live_px(px_per_hour));
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
    (wrap.upcast(), chip, meta)
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
    px_per_hour: f64,
}

thread_local! {
    /// The Plan grid's ghost, so the drive verbs can pose one.
    static GHOST: RefCell<Option<Ghost>> = const { RefCell::new(None) };
}

impl Ghost {
    /// Build the preview host. `attach` puts it on the grid last, so it
    /// draws over the blocks it previews against.
    fn new(p: &DayPlan, px_per_hour: f64) -> Self {
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
        for t in p.tray.iter().chain(&p.no_date) {
            tasks.insert(t.href.clone(), t.clone());
        }
        let g = Ghost {
            wrap,
            blocks: p.blocks.clone(),
            tasks,
            px_per_hour,
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
        let px = daygrid::live_px(self.px_per_hour);
        let (chip, _meta) = block_chip(&b, None, px);
        chip.add_css_class("ghost");
        let row = block_row(&chip, b.col, b.cols);
        self.wrap.append(&row);
        self.wrap.set_margin_top(block_top(&b, px));
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

/// Plan's blocks and drop target on the right lane. The ghost goes last so
/// it draws over what it previews against. Blocks register with `reflow`
/// so a zoom slides and resizes them without a rebuild.
pub fn fill_lane(
    p: &DayPlan,
    tx: &Tx,
    rows: &mut Vec<(String, gtk::Widget)>,
    px_per_hour: f64,
    overlay: &gtk::Overlay,
    reflow: &daygrid::Reflow,
) {
    let ghost = Ghost::new(p, px_per_hour);
    for b in &p.blocks {
        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        let (row, chip, meta) = block_widget(b, p.now_min, &ghost, tx, px_per_hour);
        rows.push((b.task.href.clone(), row.clone()));
        wrap.append(&row);
        overlay.add_overlay(&wrap);
        let span = daygrid::Span {
            start_min: f64::from(b.start),
            end_min: f64::from(b.start + b.duration),
            min_h: f64::from(min_block_h(b)),
            meta_at: 36.0,
        };
        reflow.place(&wrap, &chip, span, meta.as_ref());
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
                        model::minutes_at(y, daygrid::HOUR_START, daygrid::live_px(px_per_hour)),
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
                model::minutes_at(y, daygrid::HOUR_START, daygrid::live_px(px_per_hour)),
                PLAN_DEFAULT_DURATION,
            ));
            true
        });
    }
    overlay.add_controller(target);
}

/// The tray of tasks to place beside the day grid: what is due on or before
/// the day on view and not yet on its plan, then a collapsed No date
/// section for the tasks nobody dated, which can be planned all the same.
pub fn tray(
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
    head.append(&ui::label("To place", &["section-title"]));
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

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if p.tray.is_empty() {
        let empty = if p.day == o.now.date_naive() {
            "Every task due today is on the timeline."
        } else {
            "Nothing left to place on this day."
        };
        body.append(&ui::label(empty, &["dim-label", "caption"]));
    } else {
        body.append(&task_list(&p.tray, o, tx, rows));
    }
    if !p.no_date.is_empty() {
        body.append(&no_date_head(p.no_date.len(), o.no_date_open, tx));
        if o.no_date_open {
            body.append(&task_list(&p.no_date, o, tx, rows));
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&body)
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

/// The No date row's widget name, so a rebuild can hand the keyboard back to it.
pub const NO_DATE_TOGGLE: &str = "no-date-toggle";

/// The row that folds the No date section open and shut.
fn no_date_head(n: usize, open: bool, tx: &Tx) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Start);
    content.append(&ui::icon(
        if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        },
        12,
    ));
    content.append(&ui::label("No date", &["section-title"]));
    content.append(&ui::label(&n.to_string(), &["dim-label", "section-note"]));
    let head = gtk::Button::builder().child(&content).build();
    head.set_widget_name(NO_DATE_TOGGLE);
    head.add_css_class("flat");
    head.add_css_class("plan-no-date");
    head.set_tooltip_text(Some(if open {
        "Hide the tasks with no date"
    } else {
        "Show the tasks with no date, to place on this day"
    }));
    let tx = tx.clone();
    head.connect_clicked(move |_| tx.emit(Msg::ToggleNoDate));
    head
}

/// The tray's rows: compact task rows that drag onto the grid.
fn task_list(
    tasks: &[TaskView],
    o: &content::Opts<'_>,
    tx: &Tx,
    rows: &mut Vec<(String, gtk::Widget)>,
) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("task-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    {
        let tx = tx.clone();
        list.connect_row_activated(move |_, row| {
            tx.emit(Msg::Activate(row.widget_name().to_string()));
        });
    }
    for t in tasks {
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
                show_list: false,
                compact: true,
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
    list
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
