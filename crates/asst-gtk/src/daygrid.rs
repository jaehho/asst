//! The shared day grid both Time and Plan draw on: hour rows and labels
//! for 00:00–24:00, the minute→y mapping, the now-line, the day header
//! (title, date, step buttons, note), and the scroller with scroll-to-now.
//! What lands on the grid (tracked chips, plan blocks, the tray) stays in
//! the view that owns it.

use chrono::{NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use relm4::gtk::{self, glib, prelude::*};

use crate::model::{self, PLAN_HOUR_END, PLAN_HOUR_START, scroll_to_min};
use crate::window::{Msg, Tx};

/// Width of the hour-label gutter.
pub const GUTTER: i32 = 56;

/// Minimum width of the day grid (gutter + a usable lane).
pub const MIN_GRID_W: i32 = GUTTER + 240;

/// Hours the grid shows: the whole day, so an early or late hour has a row
/// and the now-line always has a place.
pub const HOUR_START: u32 = PLAN_HOUR_START;
pub const HOUR_END: u32 = PLAN_HOUR_END;

/// Nominal now-line thickness and dot diameter. Placement uses the widgets'
/// measured allocation, so these are the sizes CSS also asks for.
const LINE_H: f64 = 3.0;
const DOT_SIZE: f64 = 10.0;

/// y on the shared full-day grid for `mins` minutes from midnight at
/// `px_per_hour`. Delegates to `model::y_at` so drawing and placement share
/// one mapping. The value is a line's center, not its top edge.
pub fn y_at(mins: f64, px_per_hour: f64) -> f64 {
    model::y_at(mins, HOUR_START, px_per_hour)
}

/// Minutes from midnight for `ms`, clipped to `day`'s edges.
pub fn minutes_from_midnight(ms: i64, day: NaiveDate, zone: Tz) -> f64 {
    let t = chrono::Utc
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(chrono::Utc::now)
        .with_timezone(&zone);
    if t.date_naive() < day {
        return 0.0;
    }
    if t.date_naive() > day {
        return 24.0 * 60.0;
    }
    let time = t.time();
    f64::from(time.hour()) * 60.0 + f64::from(time.minute()) + f64::from(time.second()) / 60.0
}

pub fn hour_label(hour: u32) -> String {
    match hour {
        0 => "12 AM".into(),
        12 => "12 PM".into(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

/// The hour-label gutter for the full day.
fn gutter(height: i32, px_per_hour: f64) -> gtk::Box {
    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(GUTTER, height);
    gutter.add_css_class("time-gutter");
    let hour_h = px_per_hour as i32;
    for h in HOUR_START..HOUR_END {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_size_request(GUTTER, hour_h);
        let lab = gtk::Label::new(Some(&hour_label(h)));
        lab.add_css_class("time-hour");
        lab.add_css_class("dim-label");
        lab.set_xalign(1.0);
        lab.set_yalign(0.0);
        // Natural height: the label sits on the hour rule, not crushed
        // into it by a negative margin.
        lab.set_margin_end(8);
        slot.append(&lab);
        gutter.append(&slot);
    }
    gutter
}

/// Hour rules for the full day, as stacked rows (reliable allocation).
/// Spans the same extent as the now-line: the full grid width, gutter
/// included (the calendar convention), from one width source below.
fn rules(height: i32, px_per_hour: f64) -> gtk::Box {
    let hours = HOUR_END - HOUR_START;
    let rules = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rules.set_hexpand(true);
    rules.set_size_request(MIN_GRID_W, height);
    rules.add_css_class("time-grid");
    let hour_h = px_per_hour as i32;
    for _ in 0..hours {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_hexpand(true);
        slot.set_height_request(hour_h);
        slot.add_css_class("time-hour-slot");
        rules.append(&slot);
    }
    rules
}

/// The now-line: a red rule on a drawn dot. Both centers sit on `y` (the
/// minute's line center), so the dot's bottom edge is not what the rule
/// rides. Only when `now` falls on `day`. The dot is a sized circle, not a
/// glyph: a Label's box is not its ink. Sized Fixed children: a 2px box
/// measures to zero here.
fn now_line(
    now: chrono::DateTime<Tz>,
    day: NaiveDate,
    px_per_hour: f64,
) -> Option<(gtk::Box, gtk::Box, f64)> {
    if now.date_naive() != day {
        return None;
    }
    let mins = f64::from(now.time().hour()) * 60.0
        + f64::from(now.time().minute())
        + f64::from(now.time().second()) / 60.0;
    let y = y_at(mins, px_per_hour);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("time-now");
    line.set_size_request(MIN_GRID_W, LINE_H as i32);
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("time-now-dot");
    dot.set_size_request(DOT_SIZE as i32, DOT_SIZE as i32);
    Some((line, dot, y))
}

/// Place `child` so its vertical center is `y`, using its measured size
/// when it has one and `nominal` before allocation. Adds the child the
/// first time; later calls only move it, and a child that has left the
/// Fixed (a rebuild) is left alone.
fn center_at(fixed: &gtk::Fixed, child: &gtk::Widget, y: f64, nominal: f64) {
    let h = child.height();
    let h = if h > 0 { h as f64 } else { nominal };
    let at = y - h / 2.0;
    if child.parent().as_ref() == Some(fixed.upcast_ref()) {
        fixed.move_(child, 0.0, at);
    } else {
        fixed.put(child, 0.0, at);
    }
}

/// One width for the rules and the now-line, so they cannot drift apart.
/// Full grid width, gutter included. After allocation that is the Fixed's
/// width; before it, the minimum.
fn grid_width(fixed: &gtk::Fixed) -> f64 {
    f64::from(fixed.width().max(MIN_GRID_W))
}

/// Two content lanes on one time axis: tracked time on the left, the plan
/// on the right. Each is its own Overlay so chips and blocks stack only
/// within their half.
pub struct Lanes {
    pub time: gtk::Overlay,
    pub plan: gtk::Overlay,
}

fn lane() -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.set_hexpand(true);
    box_.set_size_request(120, 1);
    overlay.set_child(Some(&box_));
    overlay
}

/// The hour grid, now-line, and whatever the view draws on its two lanes.
/// Rules sit under the view's overlays and span the full width (gutter
/// included, the calendar convention); the now-line is a Fixed child
/// beside the column so it always paints over those overlays at a real y.
/// Both take their width from `grid_width`. Placement is reapplied after
/// allocation: the first build's put lands, a rebuild's does not until the
/// Fixed has been sized once.
pub fn day_grid(
    day: NaiveDate,
    now: chrono::DateTime<Tz>,
    px_per_hour: f64,
    content: impl FnOnce(&Lanes),
) -> gtk::Widget {
    let px_per_hour = model::clamp_px_per_hour(px_per_hour);
    let height = ((HOUR_END - HOUR_START) as f64 * px_per_hour).round() as i32;

    // Rules under everything, including the gutter, so the hour ticks and
    // the now-line share one extent.
    let rules = rules(height, px_per_hour);

    // Time on the left, plan on the right: one axis, two lanes.
    let lanes = Lanes {
        time: lane(),
        plan: lane(),
    };
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("day-lanes");
    row.set_hexpand(true);
    row.append(&lanes.time);
    row.append(&lanes.plan);
    content(&lanes);

    let column = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    column.add_css_class("time-planner");
    column.set_hexpand(true);
    column.set_size_request(MIN_GRID_W, height);
    column.append(&gutter(height, px_per_hour));
    column.append(&row);

    let fixed = gtk::Fixed::new();
    fixed.set_size_request(MIN_GRID_W, height);
    fixed.put(&rules, 0.0, 0.0);
    fixed.put(&column, 0.0, 0.0);
    let line_dot = now_line(now, day, px_per_hour);
    if let Some((_, _, y)) = &line_dot {
        // Both centers on `y`: the rule's and the dot's midpoints meet.
        center_at(
            &fixed,
            line_dot.as_ref().unwrap().0.upcast_ref(),
            *y,
            LINE_H,
        );
        center_at(
            &fixed,
            line_dot.as_ref().unwrap().1.upcast_ref(),
            *y,
            DOT_SIZE,
        );
    }
    let place = {
        let (weak, line_dot, rules, column) = (
            fixed.downgrade(),
            line_dot.clone(),
            rules.clone(),
            column.clone(),
        );
        move || {
            let Some(fixed) = weak.upgrade() else { return };
            // One width for rules, line, and the lane row: they cannot
            // drift apart, and the lanes fill the same extent.
            let w = grid_width(&fixed) as i32;
            rules.set_size_request(w, height);
            column.set_size_request(w, height);
            if let Some((line, dot, y)) = &line_dot {
                line.set_size_request(w, LINE_H as i32);
                center_at(&fixed, line.upcast_ref(), *y, LINE_H);
                center_at(&fixed, dot.upcast_ref(), *y, DOT_SIZE);
                line.queue_draw();
                dot.queue_draw();
            }
        }
    };
    glib::timeout_add_local_once(std::time::Duration::from_millis(50), {
        let place = place.clone();
        move || place()
    });
    // Map and the 50 ms timer can both come before the window has handed
    // the Fixed a width (a view shown at startup): the lanes would stay at
    // the minimum. Place again whenever the allocated width changes.
    let applied = std::rc::Rc::new(std::cell::Cell::new(0));
    {
        let (place, applied) = (place.clone(), applied.clone());
        fixed.add_tick_callback(move |f, _| {
            let w = f.width();
            if w > 0 && w != applied.get() {
                applied.set(w);
                place();
            }
            glib::ControlFlow::Continue
        });
    }
    // Map runs after the first real allocation, so a rebuild of an
    // already-sized Fixed still re-centers.
    fixed.connect_map(move |_| place());
    fixed.upcast()
}

/// The one day cursor the header steps.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DayStep {
    Day,
}

impl DayStep {
    fn msg(self, delta: i32) -> Msg {
        match self {
            DayStep::Day => Msg::DayStep(delta),
        }
    }
}

fn step_button(icon: &str, tip: &str, delta: i32, step: DayStep, tx: &Tx) -> gtk::Button {
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(16);
    let b = gtk::Button::builder().child(&image).build();
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.set_focus_on_click(false);
    let tx = tx.clone();
    b.connect_clicked(move |_| {
        let _ = tx.send(step.msg(delta));
    });
    b
}

/// The day header both views wear: title and date on the left, an optional
/// action of the view's, prev/next and Today on the right, and the view's note line under them.
pub fn day_header(
    title: &str,
    day: NaiveDate,
    note: &str,
    today: NaiveDate,
    step: DayStep,
    action: Option<gtk::Button>,
    tx: &Tx,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let head = gtk::CenterBox::new();
    head.add_css_class("time-head");
    let left = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title_lab = gtk::Label::new(Some(title));
    title_lab.add_css_class("view-title");
    left.append(&title_lab);
    let date_lab = gtk::Label::new(Some(&day.format("%B %e, %Y").to_string()));
    date_lab.add_css_class("time-date");
    date_lab.set_xalign(0.0);
    left.append(&date_lab);
    head.set_start_widget(Some(&left));

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    controls.add_css_class("time-controls");
    // The view's own action, ahead of the stepping buttons.
    if let Some(action) = action {
        action.add_css_class("time-action");
        controls.append(&action);
    }
    let today_btn = gtk::Button::with_label("Today");
    today_btn.add_css_class("pill");
    {
        let tx = tx.clone();
        let showing = day;
        today_btn.connect_clicked(move |_| {
            let delta = showing.signed_duration_since(today).num_days() as i32;
            if delta != 0 {
                let _ = tx.send(step.msg(-delta));
            }
        });
    }
    controls.append(&step_button(
        "go-previous-symbolic",
        "Previous day",
        -1,
        step,
        tx,
    ));
    controls.append(&step_button("go-next-symbolic", "Next day", 1, step, tx));
    controls.append(&today_btn);
    head.set_end_widget(Some(&controls));
    root.append(&head);

    let note_lab = gtk::Label::new(Some(note));
    note_lab.add_css_class("dim-label");
    note_lab.add_css_class("caption");
    note_lab.set_xalign(0.0);
    root.append(&note_lab);
    root
}

// Where a view's timeline scroller sits, and for which local day. A
// rebuild keeps the reader's place; opening the view or stepping onto a
// new day re-centers instead. Keyed by view so Time and Plan do not share
// a resting place.
thread_local! {
    static SCROLL: std::cell::RefCell<Vec<(DayStep, f64, Option<NaiveDate>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn scroll_slot(step: DayStep) -> (f64, Option<NaiveDate>) {
    SCROLL.with(|s| {
        s.borrow()
            .iter()
            .find(|(k, _, _)| *k == step)
            .map(|&(_, y, d)| (y, d))
            .unwrap_or((f64::NAN, None))
    })
}

fn scroll_set(step: DayStep, y: f64, day: Option<NaiveDate>) {
    SCROLL.with(|s| {
        let mut v = s.borrow_mut();
        match v.iter_mut().find(|(k, _, _)| *k == step) {
            Some(slot) => {
                slot.1 = y;
                slot.2 = day;
            }
            None => v.push((step, y, day)),
        }
    });
}

/// The scroll offset kept for `step`, if the scroller has one yet.
pub fn scroll_kept(step: DayStep) -> Option<f64> {
    let (y, _) = scroll_slot(step);
    y.is_finite().then_some(y)
}

/// Top-visible minute for `step` at `px_per_hour`, to pin across a zoom.
pub fn scroll_top_minute(step: DayStep, px_per_hour: f64) -> Option<(f64, f64)> {
    let y = scroll_kept(step)?;
    Some((y / px_per_hour * 60.0, 0.0))
}

/// Ctrl+scroll over the day grid zooms, keeping the minute under the
/// pointer at the same viewport y. Plain scroll is left to the scroller.
pub fn attach_zoom(scroll: &gtk::ScrolledWindow, tx: &Tx, px_per_hour: f64) {
    let ctrl = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let (scroll, tx) = (scroll.clone(), tx.clone());
        ctrl.connect_scroll(move |ctl, _dx, dy| {
            if !ctl
                .current_event_state()
                .contains(relm4::gtk::gdk::ModifierType::CONTROL_MASK)
            {
                return glib::Propagation::Proceed;
            }
            let y_in_view = ctl
                .current_event()
                .and_then(|ev| ev.position().map(|(_, y)| y))
                .unwrap_or_else(|| scroll.vadjustment().page_size() / 3.0);
            let hold = minute_under(&scroll, y_in_view, px_per_hour);
            let factor = if dy > 0.0 { 1.0 / 1.2 } else { 1.2 };
            tx.emit(Msg::ZoomBy(factor, hold));
            glib::Propagation::Stop
        });
    }
    scroll.add_controller(ctrl);
}

/// Where a day scroller rests on attach.
pub struct ScrollRest {
    pub step: DayStep,
    pub day: NaiveDate,
    pub today: NaiveDate,
    pub now_min: Option<f64>,
    pub first_min: Option<f64>,
    pub fresh_view: bool,
    /// A minute pinned at `hold.1` from the top of the viewport (a zoom).
    pub hold: Option<(f64, f64)>,
    pub px_per_hour: f64,
}

/// Attach scroll-to-now (today) or scroll-to-first-content (other days) to
/// `scroll`. `first_min` is the earliest minute the view drew, if any; on a
/// non-today day the scroller opens an hour above it, or at 7 AM when the
/// day is empty. Rebuilds of the same day keep the reader's place. `hold` is a
/// minute to pin at `hold_y` from the top of the viewport (a zoom), and
/// wins over both.
pub fn attach_scroll(scroll: &gtk::ScrolledWindow, rest: ScrollRest) {
    let ScrollRest {
        step,
        day,
        today,
        now_min,
        first_min,
        fresh_view,
        hold,
        px_per_hour: px,
    } = rest;
    let recenter = fresh_view || scroll_slot(step).1 != Some(day);
    let placed = std::rc::Rc::new(std::cell::Cell::new(false));
    let place = {
        let (scroll, placed) = (scroll.clone(), placed.clone());
        move || {
            if placed.get() {
                return;
            }
            let vadj = scroll.vadjustment();
            let view_h = vadj.page_size();
            // A recenter needs the real page; restoring does too, so
            // that the value lands in the same coordinate space.
            if view_h <= 0.0 {
                return;
            }
            let y = if let Some((mins, keep_y)) = hold {
                model::scroll_keep_min(mins as u32, HOUR_START, HOUR_END, view_h, keep_y, px)
            } else if recenter && day == today {
                now_min.map_or(0.0, |m| {
                    scroll_to_min(m as u32, HOUR_START, HOUR_END, view_h, px)
                })
            } else if recenter {
                let top = model::day_rest_min(first_min);
                model::scroll_keep_min(top, HOUR_START, HOUR_END, view_h, 0.0, px)
            } else {
                let kept = scroll_slot(step).0;
                if kept.is_finite() { kept } else { 0.0 }
            };
            vadj.set_value(y);
            scroll_set(step, y, Some(day));
            placed.set(true);
        }
    };
    {
        let place = place.clone();
        scroll.vadjustment().connect_upper_notify(move |_| place());
    }
    {
        let place = place.clone();
        scroll.connect_map(move |_| place());
    }
    glib::idle_add_local_once(place);
    scroll.vadjustment().connect_value_changed(move |a| {
        scroll_set(step, a.value(), Some(day));
    });
}

/// The minute at `y` from the top of `scroll`'s viewport, and the y to
/// keep it at while zooming. Returns none when the viewport is not sized.
pub fn minute_under(
    scroll: &gtk::ScrolledWindow,
    y_in_view: f64,
    px_per_hour: f64,
) -> Option<(f64, f64)> {
    let vadj = scroll.vadjustment();
    let view_h = vadj.page_size();
    if view_h <= 0.0 {
        return None;
    }
    let y = vadj.value() + y_in_view;
    let mins = y / px_per_hour * 60.0;
    Some((mins, y_in_view))
}
