//! The Time view: a Google Calendar–style day planner of tracked blocks.
//! The grid chrome lives in `daygrid`; this module only lays the tracked
//! chips on it.

use asst_core::api::BlockView;
use chrono::NaiveDate;
use chrono_tz::Tz;
use relm4::gtk::{self, prelude::*};

use crate::daygrid::{self, DayStep};

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
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(chrono::Utc::now)
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

/// Earliest tracked minute on `day`, for scroll-to-first on other days.
fn first_min(blocks: &[BlockView], day: NaiveDate, zone: Tz) -> Option<f64> {
    blocks
        .iter()
        .filter(|b| !b.planned)
        .map(|b| daygrid::minutes_from_midnight(b.start, day, zone))
        .fold(None, |acc: Option<f64>, m| {
            Some(acc.map_or(m, |a| a.min(m)))
        })
}

/// Event chips over the rules. Each chip is its own overlay (the Plan
/// blocks' pattern) so a sibling overlay like the now-line still measures.
fn chips(blocks: &[BlockView], day: NaiveDate, zone: Tz, overlay: &gtk::Overlay) {
    let mut placed: Vec<(f64, f64, &BlockView)> = Vec::new();
    for b in blocks {
        if b.planned {
            continue;
        }
        let s = daygrid::minutes_from_midnight(b.start, day, zone);
        // Real end for packing: inflating short spans stacked a rainbow of
        // overlapping lanes. Height gets a floor below.
        let e = daygrid::minutes_from_midnight(b.end, day, zone).max(s);
        placed.push((s, e, b));
    }
    placed.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let hour_h = daygrid::px_per_hour();
    for (s, e, b) in placed.iter() {
        let top = s / 60.0 * hour_h;
        let height_px = ((e - s) / 60.0 * hour_h).max(18.0);
        let label = b.label.as_deref().unwrap_or("untagged");
        let start_s = local_of(b.start, zone).format("%H:%M").to_string();
        let end_s = local_of(b.end, zone).format("%H:%M").to_string();
        let dur = dur_label(duration(b));

        let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chip.add_css_class("time-block");
        chip.add_css_class(tint_for(label));
        chip.set_hexpand(true);
        chip.set_height_request(height_px as i32);
        chip.set_margin_start(4);
        chip.set_margin_end(4);

        let title = gtk::Label::new(Some(label));
        title.add_css_class("time-block-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&title);
        if height_px >= 34.0 {
            let meta = gtk::Label::new(Some(&format!("{start_s}–{end_s} · {dur}")));
            meta.add_css_class("time-block-meta");
            meta.set_xalign(0.0);
            chip.append(&meta);
        }

        // One overlay per chip, positioned by margin like Plan's blocks.
        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.set_margin_top(top as i32);
        wrap.append(&chip);
        overlay.add_overlay(&wrap);
    }
}

/// The Time view column: shared day header + planner.
pub fn build_column(
    o: &crate::content::Opts<'_>,
    _sections: &[crate::model::Section],
    tx: &crate::window::Tx,
) -> crate::content::Built {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("view-column");
    if o.fresh.view {
        root.add_css_class("view-enter");
    }

    let today = o.now.date_naive();
    let note = day_note(o.blocks);
    root.append(&daygrid::day_header(
        "Time",
        o.time_day,
        &note,
        today,
        DayStep::Time,
        tx,
    ));

    let zone = o.now.timezone();
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.add_css_class("time-scroll");
    scroll.set_child(Some(&daygrid::day_grid(o.time_day, o.now, |overlay| {
        chips(o.blocks, o.time_day, zone, overlay);
    })));

    let now_min = (o.time_day == today)
        .then(|| daygrid::minutes_from_midnight(o.now.timestamp_millis(), o.time_day, zone));
    daygrid::attach_scroll(
        &scroll,
        DayStep::Time,
        o.time_day,
        today,
        now_min,
        first_min(o.blocks, o.time_day, zone),
        o.fresh.view,
    );
    root.append(&scroll);

    crate::content::Built {
        root,
        rows: Vec::new(),
    }
}
