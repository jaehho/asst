//! The Time view: a Google Calendar–style day planner of tracked blocks.

use asst_core::api::BlockView;
use chrono::{NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use relm4::gtk::{self, glib, prelude::*};

/// Pixels per hour on the day grid.
const HOUR_H: f64 = 52.0;
/// Width of the hour-label gutter.
const GUTTER: f64 = 56.0;

fn dur_label(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m", (s + 30) / 60)
    } else {
        format!("{s}s")
    }
}

fn local_of(ms: i64, zone: Tz) -> chrono::DateTime<Tz> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Utc::now)
        .with_timezone(&zone)
}

fn tint_for(label: &str) -> &'static str {
    let mut h = 0u32;
    for b in label.bytes() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(b));
    }
    match h % 4 {
        0 => "tint-blue",
        1 => "tint-green",
        2 => "tint-purple",
        _ => "tint-orange",
    }
}

pub fn duration(b: &BlockView) -> f64 {
    (b.end - b.start) as f64 / 1000.0
}

pub fn total_seconds(blocks: &[BlockView]) -> f64 {
    blocks.iter().filter(|b| !b.planned).map(duration).sum()
}

pub fn day_note(blocks: &[BlockView]) -> String {
    let total = total_seconds(blocks);
    if total <= 0.0 {
        return "no tracked time".into();
    }
    format!("{} tracked", dur_label(total))
}

/// Minutes from local midnight for an instant on `day` (clamped).
fn minutes_from_midnight(ms: i64, day: NaiveDate, zone: Tz) -> f64 {
    let t = local_of(ms, zone);
    if t.date_naive() < day {
        return 0.0;
    }
    if t.date_naive() > day {
        return 24.0 * 60.0;
    }
    let time = t.time();
    f64::from(time.hour()) * 60.0 + f64::from(time.minute()) + f64::from(time.second()) / 60.0
}

fn hour_label(hour: u32) -> String {
    match hour {
        0 => "12 AM".into(),
        12 => "12 PM".into(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

/// Place overlapping blocks in side-by-side lanes. Returns (lane, lane_count).
fn lanes(blocks: &[(f64, f64, &BlockView)]) -> Vec<(usize, usize)> {
    let n = blocks.len();
    let mut out = vec![(0usize, 1usize); n];
    if n == 0 {
        return out;
    }
    let mut lane_ends: Vec<f64> = Vec::new();
    for (i, (start, end, _)) in blocks.iter().enumerate() {
        let lane = lane_ends
            .iter()
            .position(|e| *e <= *start + 0.5)
            .unwrap_or(lane_ends.len());
        if lane == lane_ends.len() {
            lane_ends.push(*end);
        } else {
            lane_ends[lane] = *end;
        }
        out[i].0 = lane;
    }
    for i in 0..n {
        let (s, e, _) = blocks[i];
        let mut cluster = 1usize;
        for j in 0..n {
            let (s2, e2, _) = blocks[j];
            if s2 < e && s < e2 {
                cluster = cluster.max(out[j].0 + 1);
            }
        }
        out[i].1 = cluster;
    }
    out
}

/// Hour gutter + rules + event chips + now line.
fn day_grid(blocks: &[BlockView], day: NaiveDate, zone: Tz, now: chrono::DateTime<Tz>) -> gtk::Widget {
    let height = 24.0 * HOUR_H;

    let gutter = gtk::Fixed::new();
    gutter.set_size_request(GUTTER as i32, height as i32);
    gutter.add_css_class("time-gutter");
    for h in 0..24u32 {
        let lab = gtk::Label::new(Some(&hour_label(h)));
        lab.add_css_class("time-hour");
        lab.add_css_class("dim-label");
        lab.set_xalign(1.0);
        lab.set_width_chars(7);
        gutter.put(&lab, 0.0, (f64::from(h) * HOUR_H - 9.0).max(0.0));
    }

    let grid = gtk::Fixed::new();
    grid.set_hexpand(true);
    grid.set_size_request(240, height as i32);
    grid.add_css_class("time-grid");

    for h in 0..24u32 {
        let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
        rule.add_css_class("time-hour-line");
        grid.put(&rule, 0.0, f64::from(h) * HOUR_H);
    }

    let mut placed: Vec<(f64, f64, &BlockView)> = Vec::new();
    for b in blocks {
        if b.planned {
            continue;
        }
        let s = minutes_from_midnight(b.start, day, zone);
        let e = minutes_from_midnight(b.end, day, zone).max(s + 3.0);
        placed.push((s, e, b));
    }
    placed.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    let lays = lanes(&placed);

    for (i, (s, e, b)) in placed.iter().enumerate() {
        let top = s / 60.0 * HOUR_H;
        let height_px = ((e - s) / 60.0 * HOUR_H).max(16.0);
        let (lane, lane_n) = lays[i];
        let label = b.label.as_deref().unwrap_or("untagged");
        let start_s = local_of(b.start, zone).format("%H:%M").to_string();
        let end_s = local_of(b.end, zone).format("%H:%M").to_string();
        let dur = dur_label(duration(b));

        let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chip.add_css_class("time-block");
        chip.add_css_class(tint_for(label));
        chip.set_valign(gtk::Align::Start);

        let title = gtk::Label::new(Some(label));
        title.add_css_class("time-block-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&title);
        if height_px >= 34.0 {
            let meta = gtk::Label::new(Some(&format!("{start_s}–{end_s} · {dur}")));
            meta.add_css_class("time-block-meta");
            meta.add_css_class("dim-label");
            meta.set_xalign(0.0);
            chip.append(&meta);
        }

        // Lane indent; width leaves room for siblings.
        let left = 6.0 + lane as f64 * 10.0;
        let width = (220.0 / lane_n.max(1) as f64).max(56.0);
        chip.set_size_request(width as i32, height_px as i32);
        grid.put(&chip, left, top);
    }

    if now.date_naive() == day {
        let mins = f64::from(now.time().hour()) * 60.0
            + f64::from(now.time().minute())
            + f64::from(now.time().second()) / 60.0;
        let y = mins / 60.0 * HOUR_H;
        let line = gtk::Separator::new(gtk::Orientation::Horizontal);
        line.add_css_class("time-now");
        grid.put(&line, 0.0, y);
        let dot = gtk::Label::new(Some("●"));
        dot.add_css_class("time-now-dot");
        grid.put(&dot, 0.0, y - 9.0);
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("time-planner");
    row.append(&gutter);
    row.append(&grid);
    row.upcast()
}

/// The Time view column: header + planner.
pub fn build_column(
    o: &crate::content::Opts<'_>,
    _sections: &[crate::model::Section],
    tx: &crate::window::Tx,
) -> crate::content::Built {
    use crate::window::Msg;

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("view-column");
    if o.fresh.view {
        root.add_css_class("view-enter");
    }

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = gtk::Label::new(Some("Time"));
    title.add_css_class("view-title");
    title.set_xalign(0.0);
    head.append(&title);

    let date_lab = gtk::Label::new(Some(&o.time_day.format("%B %-d, %Y").to_string()));
    date_lab.add_css_class("time-date");
    date_lab.set_hexpand(true);
    date_lab.set_xalign(0.0);
    head.append(&date_lab);

    let prev = gtk::Button::from_icon_name("go-previous-symbolic");
    prev.add_css_class("flat");
    prev.set_tooltip_text(Some("Previous day"));
    {
        let tx = tx.clone();
        prev.connect_clicked(move |_| {
            let _ = tx.send(Msg::TimeStep(-1));
        });
    }
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    next.add_css_class("flat");
    next.set_tooltip_text(Some("Next day"));
    {
        let tx = tx.clone();
        next.connect_clicked(move |_| {
            let _ = tx.send(Msg::TimeStep(1));
        });
    }
    let today = gtk::Button::with_label("Today");
    today.add_css_class("pill");
    {
        let tx = tx.clone();
        let showing = o.time_day;
        today.connect_clicked(move |_| {
            let delta = showing
                .signed_duration_since(chrono::Local::now().date_naive())
                .num_days() as i32;
            if delta != 0 {
                let _ = tx.send(Msg::TimeStep(-delta));
            }
        });
    }
    head.append(&prev);
    head.append(&next);
    head.append(&today);
    root.append(&head);

    let note = gtk::Label::new(Some(&day_note(o.blocks)));
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    note.set_xalign(0.0);
    root.append(&note);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.add_css_class("time-scroll");
    scroll.set_child(Some(&day_grid(
        o.blocks,
        o.time_day,
        o.now.timezone(),
        o.now,
    )));
    root.append(&scroll);

    // Open around 8am so the workday is in view.
    {
        let scroll = scroll.clone();
        glib::idle_add_local_once(move || {
            scroll.vadjustment().set_value(7.5 * HOUR_H);
        });
    }

    crate::content::Built {
        root,
        rows: Vec::new(),
    }
}
