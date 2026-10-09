//! The shared day grid both Time and Plan draw on: hour rows and labels
//! for 00:00–24:00, the minute→y mapping, the now-line, the day header
//! (title, date, step buttons, note), and the scroller with scroll-to-now.
//! What lands on the grid (tracked chips, plan blocks, the tray) stays in
//! the view that owns it.

use chrono::{NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use relm4::gtk::{self, glib, prelude::*};

use crate::model::{self, PLAN_HOUR_END, PLAN_HOUR_START, PLAN_PX_PER_HOUR, scroll_to_min};
use crate::window::{Msg, Tx};

/// Width of the hour-label gutter.
pub const GUTTER: i32 = 56;

/// Hours the grid shows: the whole day, so an early or late hour has a row
/// and the now-line always has a place.
pub const HOUR_START: u32 = PLAN_HOUR_START;
pub const HOUR_END: u32 = PLAN_HOUR_END;

/// Pixels per hour. The same scale the placement math in `model` uses, so
/// a y on screen and a minute in a slot cannot drift apart.
pub fn px_per_hour() -> f64 {
    PLAN_PX_PER_HOUR
}

/// y on the shared full-day grid for `mins` minutes from midnight.
/// Delegates to `model::y_at` so drawing and placement share one mapping.
pub fn y_at(mins: f64) -> f64 {
    model::y_at(mins, HOUR_START)
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
fn gutter(height: i32) -> gtk::Box {
    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(GUTTER, height);
    gutter.add_css_class("time-gutter");
    let hour_h = PLAN_PX_PER_HOUR as i32;
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
fn rules(height: i32) -> gtk::Box {
    let hours = HOUR_END - HOUR_START;
    let rules = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rules.set_hexpand(true);
    rules.set_size_request(240, height);
    rules.add_css_class("time-grid");
    let hour_h = PLAN_PX_PER_HOUR as i32;
    for _ in 0..hours {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_hexpand(true);
        slot.set_height_request(hour_h);
        slot.add_css_class("time-hour-slot");
        rules.append(&slot);
    }
    rules
}

/// Time's now-line: a red rule on a dot, so the rule sits on the dot's
/// center. Both views draw this one; the peach overlay is gone. Only when
/// `now` falls on `day`. Sized Fixed children: a 2px box measures to
/// zero here.
fn now_line(now: chrono::DateTime<Tz>, day: NaiveDate) -> Option<(gtk::Box, gtk::Label, f64)> {
    if now.date_naive() != day {
        return None;
    }
    let mins = f64::from(now.time().hour()) * 60.0
        + f64::from(now.time().minute())
        + f64::from(now.time().second()) / 60.0;
    let y = y_at(mins);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("time-now");
    line.set_size_request(GUTTER + 240, 3);
    let dot = gtk::Label::new(Some("●"));
    dot.add_css_class("time-now-dot");
    Some((line, dot, y))
}

/// The hour grid, now-line, and whatever the view draws on top.
/// Rules sit under the view's overlays; the now-line is a Fixed child
/// beside the column so it always paints over those overlays at a real y.
/// Its place is reapplied after allocation: the first build's put lands,
/// a rebuild's does not until the Fixed has been sized once.
pub fn day_grid(
    day: NaiveDate,
    now: chrono::DateTime<Tz>,
    content: impl FnOnce(&gtk::Overlay),
) -> gtk::Widget {
    let height = (HOUR_END - HOUR_START) as i32 * PLAN_PX_PER_HOUR as i32;

    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    overlay.set_child(Some(&rules(height)));
    content(&overlay);

    let column = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    column.add_css_class("time-planner");
    column.set_size_request(GUTTER + 240, height);
    column.append(&gutter(height));
    column.append(&overlay);

    let fixed = gtk::Fixed::new();
    fixed.set_size_request(GUTTER + 240, height);
    fixed.put(&column, 0.0, 0.0);
    if let Some((line, dot, y)) = now_line(now, day) {
        fixed.put(&line, 0.0, y);
        fixed.put(&dot, 0.0, y - 6.0);
        let (fixed, line, dot) = (fixed.clone(), line.clone(), dot.clone());
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            let w = fixed.width().max(GUTTER + 240);
            line.set_size_request(w, 3);
            fixed.move_(&line, 0.0, y);
            fixed.move_(&dot, 0.0, y - 6.0);
            line.queue_draw();
            dot.queue_draw();
        });
    }
    fixed.upcast()
}

/// Which view's day cursor the header steps.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DayStep {
    Time,
    Plan,
}

impl DayStep {
    fn msg(self, delta: i32) -> Msg {
        match self {
            DayStep::Time => Msg::TimeStep(delta),
            DayStep::Plan => Msg::PlanStep(delta),
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

/// The day header both views wear: title and date on the left, prev/next
/// and Today on the right, and the view's note line under them.
pub fn day_header(
    title: &str,
    day: NaiveDate,
    note: &str,
    today: NaiveDate,
    step: DayStep,
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

/// Attach scroll-to-now (today) or scroll-to-first-content (other days) to
/// `scroll`. `first_min` is the earliest minute the view drew, if any; on a
/// non-today day the scroller rests there, or at 07:00 when the day is
/// empty. Rebuilds of the same day keep the reader's place.
pub fn attach_scroll(
    scroll: &gtk::ScrolledWindow,
    step: DayStep,
    day: NaiveDate,
    today: NaiveDate,
    now_min: Option<f64>,
    first_min: Option<f64>,
    fresh_view: bool,
) {
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
            let target = if day == today {
                now_min
            } else {
                first_min.or(Some(7.0 * 60.0))
            };
            let y = if recenter {
                target.map_or(0.0, |m| {
                    scroll_to_min(m as u32, HOUR_START, HOUR_END, view_h)
                })
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
