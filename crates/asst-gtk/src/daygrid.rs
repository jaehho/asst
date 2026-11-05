//! The shared day grid both Time and Plan draw on: hour rows and labels
//! for 00:00–24:00, the minute→y mapping, the now-line, the day header
//! (title, date, step buttons, note), and the scroller with scroll-to-now.
//! What lands on the grid (tracked chips, plan blocks, the tray) stays in
//! the view that owns it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use asst_core::api::BlockView;
use chrono::{NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use relm4::adw;
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
    model::clock_label(hour * 60)
}

/// The hour-label gutter for the full day.
fn gutter(px_per_hour: f64, reflow: &Reflow) -> gtk::Box {
    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // Width only: its height is the zoom's (`sized`), and a height fixed at
    // build would keep the day from ever shrinking below where it opened.
    gutter.set_width_request(GUTTER);
    reflow.sized(&gutter);
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
        reflow.hour(&slot);
        gutter.append(&slot);
    }
    gutter
}

/// Hour rules for the full day, as stacked rows (reliable allocation).
/// Spans the same extent as the now-line: the full grid width, gutter
/// included (the calendar convention), from one width source below.
fn rules(px_per_hour: f64, reflow: &Reflow) -> gtk::Box {
    let hours = HOUR_END - HOUR_START;
    let rules = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rules.set_hexpand(true);
    rules.set_width_request(MIN_GRID_W);
    rules.add_css_class("time-grid");
    let hour_h = px_per_hour as i32;
    for _ in 0..hours {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_hexpand(true);
        slot.set_height_request(hour_h);
        slot.add_css_class("time-hour-slot");
        reflow.hour(&slot);
        rules.append(&slot);
    }
    rules
}

/// The now-line: a red rule on a drawn dot. Both centers sit on the same
/// minute's line center, so the dot's bottom edge is not what the rule
/// rides. Only when `now` falls on `day`. The dot is a sized circle, not a
/// glyph: a Label's box is not its ink. Sized: a 2px box measures to zero.
/// The line is as wide as the grid, the dot sits at its left end, and both
/// hang from the top, where `center_at` sets their margin. The caller
/// draws it only when the grid shows `now`'s day.
fn now_line(now: chrono::DateTime<Tz>) -> (gtk::Box, gtk::Box, f64) {
    let mins = f64::from(now.time().hour()) * 60.0
        + f64::from(now.time().minute())
        + f64::from(now.time().second()) / 60.0;
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("time-now");
    line.set_size_request(MIN_GRID_W, LINE_H as i32);
    line.set_valign(gtk::Align::Start);
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("time-now-dot");
    dot.set_size_request(DOT_SIZE as i32, DOT_SIZE as i32);
    dot.set_halign(gtk::Align::Start);
    dot.set_valign(gtk::Align::Start);
    (line, dot, mins)
}

/// Place `child` on `grid` so its vertical center is `y`, using its
/// measured size when it has one and `nominal` before allocation. Adds the
/// child the first time; later calls only move it, and a child that has
/// gone elsewhere is left alone. A margin cannot be negative, so in the
/// day's first minute the line sits flush with the top.
fn center_at(grid: &gtk::Overlay, child: &gtk::Widget, y: f64, nominal: f64) {
    let h = child.height();
    let h = if h > 0 { h as f64 } else { nominal };
    child.set_margin_top((y - h / 2.0).round().max(0.0) as i32);
    if child.parent().is_none() {
        grid.add_overlay(child);
    }
}

/// Two content lanes on one time axis: tracked time on the left, the plan
/// and the day's calendar events on the right. Each is its own Overlay so
/// chips and blocks stack only within their half.
pub struct Lanes {
    pub time: gtk::Overlay,
    pub plan: gtk::Overlay,
}

/// Overlap columns in a lane. Each child set here gets its column's share
/// of the lane's width whenever the lane lays out, so a chip beside another
/// is as wide as its column and takes the pointer only there. A full-width
/// child padded with spacers took it across the lane, over the chip beside
/// it, whose hover and press went nowhere. Nothing here asks for a size,
/// so the lane still narrows with the window.
#[derive(Clone, Default)]
pub struct Columns(Rc<RefCell<Vec<Column>>>);

/// A lane child and its `col` of `cols`.
type Column = (glib::WeakRef<gtk::Widget>, usize, usize);

impl Columns {
    /// Place `lane`'s children that are given a column; the rest keep the
    /// overlay's own placement.
    pub fn on(lane: &gtk::Overlay) -> Self {
        let columns = Self::default();
        let placed = columns.clone();
        lane.connect_get_child_position(move |lane, child| {
            let (col, cols) = placed.of(child)?;
            let (min, nat) = child.preferred_size();
            let (x, width) = model::column_span(lane.width(), col, cols);
            // The overlay's own rule for a top-aligned child: its natural
            // height, at least its minimum; the top margin sets its time.
            let height = min.height().max(lane.height().min(nat.height()));
            Some(gtk::gdk::Rectangle::new(
                x,
                0,
                width.max(min.width()),
                height,
            ))
        });
        columns
    }

    /// Put `child`, an overlay child of the lane, in `col` of `cols`.
    pub fn set(&self, child: &impl IsA<gtk::Widget>, col: usize, cols: usize) {
        let child = child.upcast_ref::<gtk::Widget>();
        {
            let mut all = self.0.borrow_mut();
            all.retain(|(w, ..)| w.upgrade().is_some_and(|w| &w != child));
            all.push((child.downgrade(), col, cols));
        }
        child.queue_resize();
    }

    fn of(&self, child: &gtk::Widget) -> Option<(usize, usize)> {
        self.0
            .borrow()
            .iter()
            .find(|(w, ..)| w.upgrade().as_ref() == Some(child))
            .map(|&(_, col, cols)| (col, cols))
    }
}

/// A chip on the grid, remembered so a zoom can move it without rebuilding
/// the tree. `wrap` is the overlay child that carries the top margin; `body`
/// is the chip whose height follows the span. `meta` is a second line that
/// only reads when the chip is tall enough for it.
pub struct Placed {
    wrap: glib::WeakRef<gtk::Widget>,
    body: glib::WeakRef<gtk::Widget>,
    span: Span,
    title: Option<glib::WeakRef<gtk::Widget>>,
    meta: Option<glib::WeakRef<gtk::Widget>>,
}

/// The now-line, its dot and the minute they ride.
struct NowLine {
    grid: glib::WeakRef<gtk::Overlay>,
    line: glib::WeakRef<gtk::Widget>,
    dot: glib::WeakRef<gtk::Widget>,
    mins: f64,
}

/// Where a chip sits on the axis and when its second line shows.
#[derive(Clone, Copy)]
pub struct Span {
    pub start_min: f64,
    pub end_min: f64,
    /// Never shorter than this, so a short span stays clickable.
    pub min_h: f64,
    /// The title shows once the chip is this tall (0: always).
    pub title_at: f64,
    /// The meta line shows once the chip is this tall.
    pub meta_at: f64,
}

#[derive(Default)]
struct ReflowInner {
    /// The zoom the grid is drawn at now. Widgets are placed against this
    /// when they are built and every time it moves.
    px: Cell<f64>,
    hours: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    sized: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    now_line: RefCell<Option<NowLine>>,
    placed: RefCell<Vec<Placed>>,
}

/// Everything a zoom has to move, gathered while the grid and its chips are
/// built. Cheap to clone (`Rc`), so the gesture handlers and the window's
/// zoom commands both reach the same grid.
///
/// A zoom is a geometry change: chips slide to new places and text
/// re-renders at the new size. It is not a rebuild. Rebuilding the tree is
/// what made the lane flash, so this only sets margins and heights — GTK
/// relays out from there.
#[derive(Clone, Default)]
pub struct Reflow(Rc<ReflowInner>);

impl Reflow {
    /// An hour row in the rules or the gutter: its height is one hour.
    pub fn hour(&self, slot: &impl IsA<gtk::Widget>) {
        // Rules and gutter each register 24 rows, in order.
        let at = self.0.hours.borrow().len() % (HOUR_END - HOUR_START) as usize;
        slot.set_height_request(self.0.hour_h(at));
        self.0.hours.borrow_mut().push(slot.as_ref().downgrade());
    }

    /// A container that spans the whole day (rules, the lane row, the Fixed):
    /// its height follows the zoom.
    pub fn sized(&self, w: &impl IsA<gtk::Widget>) {
        w.set_height_request(self.0.grid_h());
        self.0.sized.borrow_mut().push(w.as_ref().downgrade());
    }

    /// The now-line and its dot, centered on `mins` minutes from midnight.
    pub fn now_line(
        &self,
        grid: &gtk::Overlay,
        line: &impl IsA<gtk::Widget>,
        dot: &impl IsA<gtk::Widget>,
        mins: f64,
    ) {
        let y = y_at(mins, self.0.px());
        center_at(grid, line.as_ref(), y, LINE_H);
        center_at(grid, dot.as_ref(), y, DOT_SIZE);
        *self.0.now_line.borrow_mut() = Some(NowLine {
            grid: grid.downgrade(),
            line: line.as_ref().downgrade(),
            dot: dot.as_ref().downgrade(),
            mins,
        });
    }

    /// A chip: `wrap` sits at the span's start, `body` is as tall as the
    /// span, and `meta` shows once the chip is tall enough for it.
    pub fn place(
        &self,
        wrap: &impl IsA<gtk::Widget>,
        body: &impl IsA<gtk::Widget>,
        span: Span,
        title: Option<&impl IsA<gtk::Widget>>,
        meta: Option<&impl IsA<gtk::Widget>>,
    ) {
        let p = Placed {
            wrap: wrap.as_ref().downgrade(),
            body: body.as_ref().downgrade(),
            span,
            title: title.map(|t| t.as_ref().downgrade()),
            meta: meta.map(|m| m.as_ref().downgrade()),
        };
        p.apply(self.0.px());
        self.0.placed.borrow_mut().push(p);
    }

    /// Move the whole grid to `px_per_hour`. No widget is created or
    /// destroyed, so a pinch tracks the fingers frame by frame.
    pub fn apply(&self, px_per_hour: f64) {
        self.0.px.set(model::clamp_px_per_hour(px_per_hour));
        let rows = (HOUR_END - HOUR_START) as usize;
        for (i, w) in self.0.hours.borrow().iter().enumerate() {
            if let Some(w) = w.upgrade() {
                w.set_height_request(self.0.hour_h(i % rows));
            }
        }
        let grid_h = self.0.grid_h();
        for w in self.0.sized.borrow().iter() {
            if let Some(w) = w.upgrade() {
                w.set_height_request(grid_h);
            }
        }
        if let Some(n) = self.0.now_line.borrow().as_ref()
            && let (Some(grid), Some(line), Some(dot)) =
                (n.grid.upgrade(), n.line.upgrade(), n.dot.upgrade())
        {
            let y = y_at(n.mins, self.0.px());
            center_at(&grid, &line, y, LINE_H);
            center_at(&grid, &dot, y, DOT_SIZE);
        }
        for p in self.0.placed.borrow().iter() {
            p.apply(self.0.px());
        }
    }

    /// The now-line moves to `mins` minutes from midnight.
    pub fn set_now(&self, mins: f64) {
        if let Some(n) = self.0.now_line.borrow_mut().as_mut() {
            n.mins = mins;
        }
        self.apply(self.px());
    }

    /// Stop moving these chips (their widgets are leaving).
    pub fn forget(&self, wraps: &[gtk::Widget]) {
        self.0
            .placed
            .borrow_mut()
            .retain(|p| p.wrap.upgrade().is_none_or(|w| !wraps.contains(&w)));
    }

    /// Where the grid is zoomed to now.
    pub fn px(&self) -> f64 {
        self.0.px()
    }
}

impl Placed {
    fn apply(&self, px_per_hour: f64) {
        let Some(wrap) = self.wrap.upgrade() else {
            return;
        };
        let sp = self.span;
        wrap.set_margin_top(y_at(sp.start_min, px_per_hour) as i32);
        let h = ((sp.end_min - sp.start_min) / 60.0 * px_per_hour).max(sp.min_h);
        if let Some(body) = self.body.upgrade() {
            body.set_height_request(h as i32);
        }
        // A sliver has no room for words: they appear as the chip grows.
        if let Some(title) = self.title.as_ref().and_then(|t| t.upgrade()) {
            title.set_visible(h >= sp.title_at);
        }
        if let Some(meta) = self.meta.as_ref().and_then(|m| m.upgrade()) {
            meta.set_visible(h >= sp.meta_at);
        }
    }
}

impl ReflowInner {
    fn px(&self) -> f64 {
        self.px.get()
    }
    /// Height of hour row `at`: cumulative rounding, so the rows add up to
    /// the grid and a rule never drifts from the chips placed by minute.
    fn hour_h(&self, at: usize) -> i32 {
        let edge = |i: usize| (i as f64 * self.px()).round() as i32;
        edge(at + 1) - edge(at)
    }
    fn grid_h(&self) -> i32 {
        ((HOUR_END - HOUR_START) as f64 * self.px()).round() as i32
    }
}

/// A lane at least `min_w` wide: an Overlay the view adds its chips to.
fn lane(min_w: i32) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.set_hexpand(true);
    box_.set_size_request(min_w, 1);
    overlay.set_child(Some(&box_));
    overlay
}

/// A week lane's least width: seven of them and the gutter fit a window
/// on half a laptop screen beside the sidebar.
const WEEK_LANE_W: i32 = 40;

/// The hour grid, now-line, and whatever the view draws on its two lanes.
/// Rules sit under the view's overlays and span the full width (gutter
/// included, the calendar convention); the now-line is added after the
/// column so it always paints over those overlays at a real y.
///
/// One Overlay holds them all and stretches each to its own width, so
/// rules, lanes and line cannot drift apart and nothing sets a width from
/// an allocation. That is what a Fixed needed, and a width taken from the
/// last allocation only ever grew: a window made narrower again kept the
/// grid wide and cut off the tray. The rules and the column are measured,
/// so the grid is never narrower than either needs.
///
/// Everything this builds registers with `reflow`, so a zoom can move the
/// hour rows, the sized containers and the now-line without tearing the
/// tree down.
pub fn day_grid(
    day: NaiveDate,
    now: chrono::DateTime<Tz>,
    px_per_hour: f64,
    reflow: &Reflow,
    content: impl FnOnce(&Lanes),
) -> gtk::Widget {
    // Time on the left, the plan and the day's events on the right: one
    // axis, two lanes.
    let lanes = Lanes {
        time: lane(120),
        plan: lane(120),
    };
    content(&lanes);
    timeline(
        &[lanes.time, lanes.plan],
        "day-lanes",
        (now.date_naive() == day).then_some(now),
        px_per_hour,
        reflow,
        None,
    )
}

/// The week's grid: one lane per day of `days`, equal widths, on the same
/// hour axis as the Day grid, with the now-line across it when one of
/// them is today. `content` fills each day's lane. The hour gutter joins
/// `gutter_size`, so a row above the grid can line up with the lanes.
pub fn week_grid(
    days: &[NaiveDate],
    now: chrono::DateTime<Tz>,
    px_per_hour: f64,
    reflow: &Reflow,
    gutter_size: &gtk::SizeGroup,
    content: impl FnOnce(&[gtk::Overlay]),
) -> gtk::Widget {
    let lanes: Vec<gtk::Overlay> = days.iter().map(|_| lane(WEEK_LANE_W)).collect();
    content(&lanes);
    timeline(
        &lanes,
        "week-lanes",
        days.contains(&now.date_naive()).then_some(now),
        px_per_hour,
        reflow,
        Some(gutter_size),
    )
}

/// Rules, the gutter and `lanes` in a row of class `row_class`, and the
/// now-line at `now` when given. The gutter joins `gutter_size` if given.
fn timeline(
    lanes: &[gtk::Overlay],
    row_class: &str,
    now: Option<chrono::DateTime<Tz>>,
    px_per_hour: f64,
    reflow: &Reflow,
    gutter_size: Option<&gtk::SizeGroup>,
) -> gtk::Widget {
    reflow.apply(px_per_hour);

    // Rules under everything, including the gutter, so the hour ticks and
    // the now-line share one extent.
    let rules = rules(reflow.px(), reflow);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class(row_class);
    row.set_hexpand(true);
    row.set_homogeneous(true);
    for lane in lanes {
        row.append(lane);
    }

    let column = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    column.add_css_class("time-planner");
    column.set_hexpand(true);
    let gutter = gutter(reflow.px(), reflow);
    if let Some(group) = gutter_size {
        group.add_widget(&gutter);
    }
    column.append(&gutter);
    column.append(&row);

    let grid = gtk::Overlay::new();
    grid.set_hexpand(true);
    grid.set_child(Some(&rules));
    grid.add_overlay(&column);
    grid.set_measure_overlay(&column, true);
    reflow.sized(&rules);
    reflow.sized(&column);
    reflow.sized(&grid);
    if let Some(now) = now {
        // Both centers on the same y: the rule's and the dot's midpoints meet.
        let (line, dot, mins) = now_line(now);
        reflow.now_line(&grid, &line, &dot, mins);
    }
    grid.upcast()
}

/// What the header's arrows step: the day on view, or its week. Each keeps
/// its own scroll place.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DayStep {
    Day,
    Week,
}

impl DayStep {
    /// The day cursor moved `delta` days or weeks.
    fn msg(self, delta: i32) -> Msg {
        match self {
            DayStep::Day => Msg::DayStep(delta),
            DayStep::Week => Msg::DayStep(delta * 7),
        }
    }

    /// The arrows' tooltips and the button that comes back to now.
    fn words(self) -> (&'static str, &'static str, &'static str) {
        match self {
            DayStep::Day => ("Previous day", "Next day", "Today"),
            DayStep::Week => ("Previous week", "Next week", "This week"),
        }
    }
}

/// The Day / Week switch both views wear in their header.
pub fn span_switch(week: bool, tx: &Tx) -> adw::ToggleGroup {
    let group = adw::ToggleGroup::new();
    group.add_css_class("round");
    group.add_css_class("day-span");
    for (name, label) in [("day", "Day"), ("week", "Week")] {
        group.add(adw::Toggle::builder().name(name).label(label).build());
    }
    group.set_active_name(Some(if week { "week" } else { "day" }));
    let tx = tx.clone();
    group.connect_active_name_notify(move |g| {
        let week = g.active_name().is_some_and(|n| n == "week");
        let _ = tx.send(Msg::DayWeek(week));
    });
    group
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

/// What a day header shows.
pub struct Header<'a> {
    pub title: &'a str,
    /// The date or the week beside the title.
    pub date: &'a str,
    /// The line under the title.
    pub note: &'a str,
    /// The day on view, which Today steps back from.
    pub day: NaiveDate,
    pub today: NaiveDate,
    pub step: DayStep,
    /// The view's own controls, ahead of the stepping buttons.
    pub actions: Vec<gtk::Widget>,
}

/// The header Day and Week wear: title and date on the left, the view's
/// controls, prev/next and Today on the right, and the note under them.
pub fn day_header(h: Header<'_>, tx: &Tx) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let head = gtk::CenterBox::new();
    head.add_css_class("time-head");
    let left = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title_lab = gtk::Label::new(Some(h.title));
    title_lab.add_css_class("view-title");
    left.append(&title_lab);
    let date_lab = gtk::Label::new(Some(h.date));
    date_lab.add_css_class("time-date");
    date_lab.set_xalign(0.0);
    // The controls come first when the window is narrow.
    date_lab.set_ellipsize(gtk::pango::EllipsizeMode::End);
    left.append(&date_lab);
    head.set_start_widget(Some(&left));

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    controls.add_css_class("time-controls");
    for action in h.actions {
        action.add_css_class("time-action");
        controls.append(&action);
    }
    let (prev, next, now) = h.step.words();
    let today_btn = gtk::Button::with_label(now);
    today_btn.add_css_class("pill");
    {
        let tx = tx.clone();
        let delta = h.day.signed_duration_since(h.today).num_days() as i32;
        today_btn.connect_clicked(move |_| {
            if delta != 0 {
                let _ = tx.send(Msg::DayStep(-delta));
            }
        });
    }
    controls.append(&step_button("go-previous-symbolic", prev, -1, h.step, tx));
    controls.append(&step_button("go-next-symbolic", next, 1, h.step, tx));
    controls.append(&today_btn);
    head.set_end_widget(Some(&controls));
    root.append(&head);

    let note_lab = gtk::Label::new(Some(h.note));
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

thread_local! {
    /// The day scroller's page height when it last laid out: what the next
    /// one, built for a rebuild of the same window, will get.
    static PAGE: Cell<f64> = const { Cell::new(0.0) };
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

// The day grid on view now, if any: its reflow and its scroller. Set when a
// Day column is built, so the window's zoom keys and the gesture handlers
// reach the same tree without threading it through every message.
struct Live {
    reflow: Reflow,
    scroll: glib::WeakRef<gtk::ScrolledWindow>,
    /// Bumped by every glide, so a newer one retires the older.
    glide: Cell<u64>,
    /// Redraws what the zoom level decides (which chips, how much text)
    /// when a zoom crosses into another level. Given the new zoom.
    refill: Option<Rc<dyn Fn(f64)>>,
    /// Takes new tracked data and the clock in place, so a minute passing
    /// or a block growing never rebuilds the column.
    refresh: Option<Refresh>,
}

/// Whether the tracked blocks changed, the new blocks and the clock.
pub type Refresh = Rc<dyn Fn(&[BlockView], bool, chrono::DateTime<Tz>)>;

thread_local! {
    static LIVE: RefCell<Option<Rc<Live>>> = const { RefCell::new(None) };
}

/// Put the day grid on view into `LIVE`, so zoom commands can reach it.
pub fn live(
    reflow: &Reflow,
    scroll: &gtk::ScrolledWindow,
    refill: Option<Rc<dyn Fn(f64)>>,
    refresh: Option<Refresh>,
) {
    LIVE.with(|l| {
        let glide = l.borrow().as_ref().map_or(0, |o| o.glide.get() + 1);
        *l.borrow_mut() = Some(Rc::new(Live {
            reflow: reflow.clone(),
            scroll: scroll.downgrade(),
            glide: Cell::new(glide),
            refill,
            refresh,
        }));
    });
}

fn live_now() -> Option<Rc<Live>> {
    LIVE.with(|l| l.borrow().clone())
}

/// Hand new tracked blocks and the clock to the grid on view. False when
/// there is no live grid (the caller rebuilds instead).
pub fn refresh_live(
    blocks: &[BlockView],
    tracked_changed: bool,
    now: chrono::DateTime<Tz>,
) -> bool {
    let Some(l) = live_now() else { return false };
    let (Some(_), Some(refresh)) = (l.scroll.upgrade(), &l.refresh) else {
        return false;
    };
    refresh(blocks, tracked_changed, now);
    true
}

/// The day scroller's allocated height, its viewport's page size and how
/// far it can scroll, for the dev dump.
pub fn viewport() -> Option<(i32, f64, f64)> {
    let scroll = live_now()?.scroll.upgrade()?;
    let vadj = scroll.vadjustment();
    Some((scroll.height(), vadj.page_size(), vadj.upper()))
}

/// Scroll the day grid on view to `y`, as a wheel would.
pub fn scroll_day_to(y: f64) {
    if let Some(scroll) = live_now().and_then(|l| l.scroll.upgrade()) {
        scroll.vadjustment().set_value(y);
    }
}

/// The day scroller's value, upper and page, the y it keeps for a rebuild,
/// and how far up the grid is drawn: `-drawn` equals `value` when the
/// layout has caught up with the adjustment.
pub fn scroll_probe() -> Option<String> {
    let scroll = live_now()?.scroll.upgrade()?;
    let vadj = scroll.vadjustment();
    let grid = scroll.child()?.first_child()?;
    let drawn = grid
        .compute_point(&scroll, &gtk::graphene::Point::new(0.0, 0.0))
        .map_or(f32::NAN, |p| p.y());
    Some(format!(
        "value {:.0} upper {:.0} page {:.0} kept {:.0} drawn {:.0}",
        vadj.value(),
        vadj.upper(),
        vadj.page_size(),
        scroll_slot(DayStep::Day).0,
        drawn
    ))
}

/// Minutes from midnight on `now`'s own day, seconds as a fraction.
pub fn now_minutes(now: chrono::DateTime<Tz>) -> f64 {
    f64::from(now.time().hour()) * 60.0
        + f64::from(now.time().minute())
        + f64::from(now.time().second()) / 60.0
}

/// `px` kept where the day grid is usable: never so far out that the day
/// is shorter than its viewport (empty space under 11 pm), never past the
/// zoom limits.
pub fn clamp_live(px: f64) -> f64 {
    let floor = live_now()
        .and_then(|l| l.scroll.upgrade())
        .map(|s| (s.vadjustment().page_size() / f64::from(HOUR_END - HOUR_START)).ceil())
        .unwrap_or(0.0)
        .min(model::PX_PER_HOUR_MAX);
    model::clamp_px_per_hour(px).max(floor)
}

/// The zoom the grid on view is at, else `fallback`. Drags and drops read
/// this, not the zoom they were built at: a pinch moves the grid without a
/// rebuild.
pub fn live_px(fallback: f64) -> f64 {
    live_now().map_or(fallback, |l| l.reflow.px())
}

/// Scroll so minute `hold.0` sits at `hold.1` from the top of the viewport.
/// Float math (a minute is two pixels at the closest zoom, so truncating it
/// jitters), and the adjustment's upper bound is moved first: the viewport
/// only learns the new height at the next layout, and a value set before
/// that is clamped to the old one.
fn pin_scroll(scroll: &gtk::ScrolledWindow, hold: Option<(f64, f64)>, px_per_hour: f64) {
    let Some((mins, keep_y)) = hold else {
        return;
    };
    let vadj = scroll.vadjustment();
    let view_h = vadj.page_size();
    if view_h <= 0.0 {
        return;
    }
    let grid_h = model::grid_height(HOUR_START, HOUR_END, px_per_hour);
    vadj.set_upper(grid_h.max(view_h));
    let max = (grid_h - view_h).max(0.0);
    vadj.set_value((y_at(mins, px_per_hour) - keep_y).clamp(0.0, max));
}

/// Move the grid on view to `px_per_hour` right now, keeping `hold` put.
/// Geometry only: no widgets are made or dropped, so this is safe to call
/// on every frame of a pinch or a glide.
pub fn zoom_now(px_per_hour: f64, hold: Option<(f64, f64)>) {
    let px = clamp_live(px_per_hour);
    let Some(l) = live_now() else { return };
    l.reflow.apply(px);
    if let Some(scroll) = l.scroll.upgrade() {
        pin_scroll(&scroll, hold, px);
    }
    if let Some(refill) = &l.refill {
        refill(px);
    }
}

/// Glide the grid on view from where it is to `to`, on the frame clock, and
/// call `step` with each zoom on the way and `done` at the end. A newer
/// glide retires this one; `done` then does not run.
pub fn glide(
    to: f64,
    hold: Option<(f64, f64)>,
    ms: u32,
    step: impl Fn(f64) + 'static,
    done: impl FnOnce(f64) + 'static,
) {
    let to = clamp_live(to);
    let Some(l) = live_now() else {
        done(to);
        return;
    };
    let from = l.reflow.px();
    if ms == 0 || (from - to).abs() < 0.01 {
        zoom_now(to, hold);
        step(to);
        done(to);
        return;
    }
    let mine = l.glide.get() + 1;
    l.glide.set(mine);
    let Some(scroll) = l.scroll.upgrade() else {
        done(to);
        return;
    };
    let start = Cell::new(None::<i64>);
    let done = RefCell::new(Some(done));
    scroll.add_tick_callback(move |_, clock| {
        if l.glide.get() != mine {
            return glib::ControlFlow::Break;
        }
        // Frame time, not wall time: the glide keeps pace with what is
        // painted, and a dropped frame catches up instead of stuttering.
        let now = clock.frame_time();
        let t0 = start.get().unwrap_or(now);
        start.set(Some(t0));
        let t = ((now - t0) as f64 / (f64::from(ms) * 1000.0)).clamp(0.0, 1.0);
        // Ease-out quintic: a quick start that settles without a thud.
        let eased = 1.0 - (1.0 - t).powi(5);
        let px = from + (to - from) * eased;
        zoom_now(px, hold);
        step(px);
        if t >= 1.0 {
            if let Some(d) = done.borrow_mut().take() {
                d(to);
            }
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// Ctrl+scroll over the day grid zooms, keeping the minute under the
/// pointer at the same viewport y. Plain scroll is left to the scroller.
///
/// The controller sits on the scroller **and** its child in the capture
/// phase: GTK otherwise hands the event to the overlay packing and the
/// zoom never sees it.
///
/// Every event moves the grid itself (`reflow`) so a pinch or a scroll
/// tracks the hand frame by frame. The window is only told afterwards, to
/// remember the zoom and rebuild if the detail level changed.
pub fn attach_zoom(scroll: &gtk::ScrolledWindow, tx: &Tx, px_per_hour: f64) {
    let make_scroll = || {
        let ctrl = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
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
                let px = live_px(px_per_hour);
                let hold = minute_under(&scroll, y_in_view, px);
                if dy.abs() >= 1.0 {
                    // A wheel click is a step, not a stream: glide to it.
                    let factor = if dy > 0.0 { 1.0 / 1.2 } else { 1.2 };
                    tx.emit(Msg::ZoomBy(factor, hold));
                } else {
                    // A touchpad's two-finger stream tracks the hand.
                    let target = clamp_live(px * (1.0 - dy * 0.12));
                    zoom_now(target, hold);
                    tx.emit(Msg::Zoom(target, hold));
                }
                glib::Propagation::Stop
            });
        }
        ctrl
    };
    // Touchpad pinch is a zoom gesture, not Ctrl+scroll. Attached in the
    // capture phase on the scroller and its child so the overlay cannot
    // swallow it. The scale is absolute from the gesture's start, so the
    // grid tracks the fingers 1:1 with no drift.
    let make_pinch = || {
        let pinch = gtk::GestureZoom::new();
        pinch.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let (scroll, tx) = (scroll.clone(), tx.clone());
            let base = Rc::new(Cell::new(px_per_hour));
            let hold = Rc::new(Cell::new(None::<(f64, f64)>));
            {
                let (scroll, base, hold) = (scroll.clone(), base.clone(), hold.clone());
                pinch.connect_begin(move |g, _seq| {
                    let px = live_px(px_per_hour);
                    base.set(px);
                    let y_in_view = g
                        .bounding_box_center()
                        .map(|(_, y)| y)
                        .unwrap_or_else(|| scroll.vadjustment().page_size() / 3.0);
                    hold.set(minute_under(&scroll, y_in_view, px));
                });
            }
            {
                let (base, hold) = (base.clone(), hold.clone());
                pinch.connect_scale_changed(move |_g, scale| {
                    if !scale.is_finite() || scale <= 0.0 {
                        return;
                    }
                    let target = clamp_live(base.get() * scale);
                    zoom_now(target, hold.get());
                    tx.emit(Msg::Zoom(target, hold.get()));
                });
            }
            {
                let (base, hold) = (base.clone(), hold.clone());
                pinch.connect_end(move |_g, _seq| {
                    base.set(px_per_hour);
                    hold.set(None);
                });
            }
        }
        pinch
    };
    scroll.add_controller(make_scroll());
    scroll.add_controller(make_pinch());
    if let Some(child) = scroll.child() {
        child.add_controller(make_scroll());
        child.add_controller(make_pinch());
    }
    // A taller window (or a saved zoom from a shorter one) can leave the day
    // shorter than its viewport: zoom in just far enough to fill it.
    let tx = tx.clone();
    scroll.vadjustment().connect_page_size_notify(move |_| {
        let px = live_px(px_per_hour);
        let fit = clamp_live(px);
        if fit > px {
            zoom_now(fit, None);
            tx.emit(Msg::Zoom(fit, None));
        }
    });
}

/// Where a day scroller rests on attach.
pub struct ScrollRest {
    pub step: DayStep,
    /// The day on view, or the week's first day: a rebuild of the same one
    /// keeps the reader's place, another re-centers.
    pub day: NaiveDate,
    /// The grid shows today, so a re-center goes to now.
    pub shows_today: bool,
    pub now_min: Option<f64>,
    pub first_min: Option<f64>,
    pub fresh_view: bool,
    pub px_per_hour: f64,
}

/// Attach scroll-to-now (today) or scroll-to-first-content (other days) to
/// `scroll`. `first_min` is the earliest minute the view drew, if any; on a
/// non-today day the scroller opens an hour above it, or at 7 AM when the
/// day is empty. Rebuilds of the same day keep the reader's place.
pub fn attach_scroll(scroll: &gtk::ScrolledWindow, rest: ScrollRest) {
    let ScrollRest {
        step,
        day,
        shows_today,
        now_min,
        first_min,
        fresh_view,
        px_per_hour: px,
    } = rest;
    let (kept, kept_day) = scroll_slot(step);
    let recenter = fresh_view || kept_day != Some(day);
    // Where the scroller rests in a viewport `view_h` tall.
    let rest_y = move |view_h: f64| {
        if recenter && shows_today {
            now_min.map_or(0.0, |m| {
                scroll_to_min(m as u32, HOUR_START, HOUR_END, view_h, px)
            })
        } else if recenter {
            let top = model::day_rest_min(first_min);
            model::scroll_keep_min(top, HOUR_START, HOUR_END, view_h, 0.0, px)
        } else if kept.is_finite() {
            kept
        } else {
            0.0
        }
    };
    let vadj = scroll.vadjustment();
    // The viewport draws the grid at whatever value the adjustment holds
    // when it first lays out, so the value goes in now, against the page
    // the last scroller had. Set later, from a notify the viewport sends
    // during that layout, it moved the scrollbar but not the grid: GTK
    // drops a re-layout asked for mid-layout, so the grid sat at the top
    // until the next scroll jumped it to the kept place.
    let page = PAGE.with(Cell::get);
    if page > 0.0 {
        let y = rest_y(page);
        // Upper first, or the value is clamped to an empty scroller's 0.
        vadj.set_upper(model::grid_height(HOUR_START, HOUR_END, px).max(y + page));
        vadj.set_value(y);
        scroll_set(step, y, Some(day));
    } else {
        // No day scroller has been laid out yet: take this one's page,
        // then move once its layout is over, so the move is laid out too.
        let placed = Rc::new(Cell::new(false));
        vadj.connect_page_size_notify(move |a| {
            if placed.get() || a.page_size() <= 0.0 {
                return;
            }
            placed.set(true);
            let (a, y) = (a.clone(), rest_y(a.page_size()));
            glib::idle_add_local_once(move || {
                a.set_value(y);
                scroll_set(step, y, Some(day));
            });
        });
    }
    vadj.connect_page_size_notify(|a| PAGE.with(|p| p.set(a.page_size())));
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
