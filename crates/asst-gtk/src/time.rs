//! Tracked ActivityWatch chips for the Day view's left lane. The shared
//! chrome lives in `daygrid`; this module only lays the chips on it.

use asst_core::api::BlockView;
use chrono::NaiveDate;
use chrono_tz::Tz;
use relm4::gtk::{self, prelude::*};

use crate::daygrid;
use crate::model;

/// A span in the same words the plan's popover uses: `1 h 6 min`, `25 min`.
fn dur_label(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 60 {
        asst_core::fmt::minutes((s + 30) / 60)
    } else {
        format!("{s} s")
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

/// Whether any of `b` falls on `day`. Blocks the day stepped away from stay
/// in the window until the new day's arrive, and are not on it.
fn touches(b: &BlockView, day: NaiveDate, zone: Tz) -> bool {
    let start_of = |d: NaiveDate| {
        asst_core::time::resolve(zone, d.and_time(chrono::NaiveTime::MIN)).timestamp_millis()
    };
    b.end > start_of(day) && b.start < start_of(day.succ_opt().unwrap_or(day))
}

/// Earliest tracked minute on `day`, for scroll-to-first on other days.
pub fn first_min(blocks: &[BlockView], day: NaiveDate, zone: Tz) -> Option<f64> {
    blocks
        .iter()
        .filter(|b| !b.planned && touches(b, day, zone))
        .map(|b| daygrid::minutes_from_midnight(b.start, day, zone))
        .fold(None, |acc: Option<f64>, m| {
            Some(acc.map_or(m, |a| a.min(m)))
        })
}

/// Tracked chips over the rules in the time lane. Each chip is its own
/// overlay (the Plan blocks' pattern) so a sibling overlay like the
/// now-line still measures. Overlaps stack in columns inside the lane.
pub fn fill_lane(
    blocks: &[BlockView],
    day: NaiveDate,
    zone: Tz,
    overlay: &gtk::Overlay,
    px_per_hour: f64,
) {
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
    let spans: Vec<(f64, f64)> = placed.iter().map(|(s, e, _)| (*s, *e)).collect();
    let cols = model::span_columns(&spans);

    for (i, (s, e, b)) in placed.iter().enumerate() {
        let (col, ncol) = cols[i];
        let top = s / 60.0 * px_per_hour;
        let height_px = ((e - s) / 60.0 * px_per_hour).max(18.0);
        let label = b.label.as_deref().unwrap_or("untagged");
        // Prefer the human name (page title, Claude session) on the chip;
        // `label` stays the project tint and the rollup key.
        let title_text = b
            .detail
            .as_deref()
            .filter(|d| !d.is_empty())
            .unwrap_or(label);
        let start_s = asst_core::fmt::time_label(local_of(b.start, zone).time());
        let end_s = asst_core::fmt::time_label(local_of(b.end, zone).time());
        let dur = dur_label(duration(b));

        let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chip.add_css_class("time-block");
        chip.add_css_class(tint_for(label));
        chip.set_hexpand(true);
        chip.set_height_request(height_px as i32);
        chip.set_margin_start(4);
        chip.set_margin_end(4);

        let title = gtk::Label::new(Some(title_text));
        title.add_css_class("time-block-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&title);
        if height_px >= 34.0 {
            let meta_text = match &b.composition {
                Some(c) if !c.is_empty() => {
                    format!("{start_s} – {end_s} · {dur} · {c}")
                }
                _ => format!("{start_s} – {end_s} · {dur}"),
            };
            let meta = gtk::Label::new(Some(&meta_text));
            meta.add_css_class("time-block-meta");
            meta.set_xalign(0.0);
            meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.append(&meta);
        }

        // Equal-expand spacers put the chip in its overlap column.
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_hexpand(true);
        for c in 0..ncol.max(1) {
            if c == col {
                row.append(&chip);
            } else {
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_hexpand(true);
                row.append(&spacer);
            }
        }

        // One overlay per chip, positioned by margin like Plan's blocks.
        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.set_margin_top(top as i32);
        wrap.append(&row);
        overlay.add_overlay(&wrap);
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn block(from: (u32, u32, u32), to: (u32, u32, u32)) -> BlockView {
        let ms = |(d, h, m): (u32, u32, u32)| {
            ny().with_ymd_and_hms(2026, 9, d, h, m, 0)
                .unwrap()
                .timestamp_millis()
        };
        BlockView {
            id: None,
            task_uid: None,
            start: ms(from),
            end: ms(to),
            label: None,
            detail: None,
            composition: None,
            planned: false,
            source: None,
            created_at: 0,
        }
    }

    #[test]
    fn the_first_minute_ignores_blocks_of_other_days() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let yesterday = block((14, 9, 0), (14, 10, 0));
        let today = block((15, 13, 30), (15, 14, 0));
        // Stale blocks of the day just left must not pull the day to
        // midnight, the minute a clamped edge reads as.
        assert_eq!(first_min(std::slice::from_ref(&yesterday), day, ny()), None);
        assert_eq!(
            first_min(&[yesterday, today], day, ny()),
            Some(13.0 * 60.0 + 30.0)
        );
        // A block that runs in from the night before starts the day.
        let overnight = block((14, 23, 0), (15, 1, 0));
        assert_eq!(first_min(&[overnight], day, ny()), Some(0.0));
        // One that ended at midnight sharp, or starts the day after, is not
        // on it.
        let ended = block((14, 23, 0), (15, 0, 0));
        let later = block((16, 0, 0), (16, 1, 0));
        assert_eq!(first_min(&[ended, later], day, ny()), None);
    }
}
