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

/// Header line for the Day view. Plan and tracked are kept apart (two
/// lanes); this is the only summary that names both, so you can compare
/// what you meant to do with what the sensors saw.
pub fn day_note(blocks: &[BlockView], planned_secs: f64) -> String {
    let tracked = total_seconds(blocks);
    match (planned_secs > 0.0, tracked > 0.0) {
        (false, false) => "no plan · no tracked time".into(),
        (false, true) => format!("{} tracked", dur_label(tracked)),
        (true, false) => format!("{} planned", dur_label(planned_secs)),
        (true, true) => {
            let delta = tracked - planned_secs;
            let sign = if delta >= 0.0 { "+" } else { "−" };
            format!(
                "{} planned · {} tracked · {sign}{}",
                dur_label(planned_secs),
                dur_label(tracked),
                dur_label(delta.abs())
            )
        }
    }
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

/// How much of the timeline to show at this zoom.
///
/// Far out, a day is a few project bars. Closer in, sessions and their
/// titles. Closest, the composition line. Concurrent chips only get
/// side-by-side columns when there is room for them to be readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Project bars only: one column, name only.
    Day,
    /// One chip per session, title on it, at most two columns.
    Session,
    /// Full chips with times and composition, up to four columns.
    Fine,
}

pub fn detail_for(px_per_hour: f64) -> Detail {
    if px_per_hour < 36.0 {
        Detail::Day
    } else if px_per_hour < 72.0 {
        Detail::Session
    } else {
        Detail::Fine
    }
}

/// A chip to draw: already merged for this zoom, in minutes from midnight.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySpan {
    pub start_min: f64,
    pub end_min: f64,
    pub label: String,
    pub title: String,
    pub meta: Option<String>,
}

/// Merge tracked blocks into what this zoom can actually show.
///
/// One primary chip owns each minute (the longest span covering it).
/// Concurrent work is named on that chip's meta line, never a second
/// overlapping box. At `Day` the title is just the project; `Session`
/// adds the time range; `Fine` adds composition.
pub fn display_spans(
    blocks: &[BlockView],
    day: NaiveDate,
    zone: Tz,
    detail: Detail,
) -> Vec<DisplaySpan> {
    let mut placed: Vec<(f64, f64, &BlockView)> = Vec::new();
    for b in blocks {
        if b.planned {
            continue;
        }
        let s = daygrid::minutes_from_midnight(b.start, day, zone);
        let e = daygrid::minutes_from_midnight(b.end, day, zone).max(s);
        if e - s < 0.5 {
            continue;
        }
        placed.push((s, e, b));
    }
    if placed.is_empty() {
        return Vec::new();
    }
    // Minute-resolution primary track: at each slice the longest block
    // wins. Adjacent slices with the same winner become one chip.
    let mut edges: Vec<f64> = Vec::new();
    for (s, e, _) in &placed {
        edges.push(*s);
        edges.push(*e);
    }
    edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-6);

    let mut runs: Vec<(f64, f64, &BlockView, Vec<String>)> = Vec::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a < 0.05 {
            continue;
        }
        let mut cover: Vec<(f64, &BlockView)> = Vec::new();
        for (s, e, blk) in &placed {
            if *s < b && a < *e {
                let ov = e.min(b) - s.max(a);
                cover.push((ov, blk));
            }
        }
        cover.sort_by(|x, y| {
            y.0.partial_cmp(&x.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| x.1.start.cmp(&y.1.start))
        });
        let Some((_, win)) = cover.first() else {
            continue;
        };
        let losers: Vec<String> = cover
            .iter()
            .skip(1)
            .filter_map(|(_, blk)| {
                let lab = blk.label.as_deref().unwrap_or("untagged");
                if lab == win.label.as_deref().unwrap_or("untagged") {
                    None
                } else {
                    Some(lab.to_string())
                }
            })
            .collect();
        match runs.last_mut() {
            Some(last) if last.2.label == win.label && a <= last.1 => {
                last.1 = last.1.max(b);
                for l in losers {
                    if !last.3.contains(&l) {
                        last.3.push(l);
                    }
                }
            }
            _ => runs.push((a, b, *win, losers)),
        }
    }

    let mut out: Vec<DisplaySpan> = Vec::new();
    for (s, e, b, losers) in runs {
        let label = b.label.as_deref().unwrap_or("untagged").to_string();
        let title = match detail {
            Detail::Day => label.clone(),
            _ => b
                .detail
                .as_deref()
                .filter(|d| !d.is_empty())
                .unwrap_or(&label)
                .to_string(),
        };
        let start_s = asst_core::fmt::time_label(local_of(b.start, zone).time());
        let end_s = asst_core::fmt::time_label(local_of(b.end, zone).time());
        let dur = dur_label(duration(b));
        let mut extras: Vec<String> = losers;
        if detail == Detail::Fine
            && let Some(c) = b.composition.as_deref().filter(|c| !c.is_empty())
        {
            extras.push(c.to_string());
        }
        let meta = match detail {
            Detail::Day => None,
            _ => {
                let extra = if extras.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", extras.join(" · "))
                };
                Some(format!("{start_s} – {end_s} · {dur}{extra}"))
            }
        };
        out.push(DisplaySpan {
            start_min: s,
            end_min: e,
            label,
            title,
            meta,
        });
    }
    out
}

/// Tracked chips over the rules in the time lane. Each chip is its own
/// overlay (the Plan blocks' pattern) so a sibling overlay like the
/// now-line still measures. What is drawn, and how much of it, follows
/// `detail_for(px_per_hour)`.
pub fn fill_lane(
    blocks: &[BlockView],
    day: NaiveDate,
    zone: Tz,
    overlay: &gtk::Overlay,
    px_per_hour: f64,
) {
    let detail = detail_for(px_per_hour);
    let spans = display_spans(blocks, day, zone, detail);
    // One column always at Day; at Session at most two; Fine packs a
    // real overlap group into up to four. Past that the text is a lie.
    let max_cols = match detail {
        Detail::Day => 1,
        Detail::Session => 2,
        Detail::Fine => 4,
    };
    let boxes: Vec<(f64, f64)> = spans.iter().map(|d| (d.start_min, d.end_min)).collect();
    let cols = model::span_columns(&boxes);

    for (i, d) in spans.iter().enumerate() {
        let (mut col, ncol) = cols[i];
        let ncol = ncol.min(max_cols).max(1);
        col = col.min(ncol - 1);
        let top = d.start_min / 60.0 * px_per_hour;
        let height_px = ((d.end_min - d.start_min) / 60.0 * px_per_hour).max(18.0);

        let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chip.add_css_class("time-block");
        chip.add_css_class(tint_for(&d.label));
        chip.set_hexpand(true);
        chip.set_height_request(height_px as i32);
        chip.set_margin_start(4);
        chip.set_margin_end(4);

        let title = gtk::Label::new(Some(&d.title));
        title.add_css_class("time-block-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&title);
        if let Some(meta_text) = &d.meta
            && height_px >= 34.0
        {
            let meta = gtk::Label::new(Some(meta_text));
            meta.add_css_class("time-block-meta");
            meta.set_xalign(0.0);
            meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.append(&meta);
        }

        // Equal-expand spacers put the chip in its overlap column.
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_hexpand(true);
        for c in 0..ncol {
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
    #[test]
    fn detail_follows_zoom() {
        assert_eq!(detail_for(20.0), Detail::Day);
        assert_eq!(detail_for(52.0), Detail::Session);
        assert_eq!(detail_for(90.0), Detail::Fine);
    }

    #[test]
    fn day_zoom_shows_one_bar_per_project_not_a_collage() {
        // block() builds September 2026 timestamps.
        let day = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        let lab = block((20, 14, 0), (20, 15, 0));
        let mut mail = block((20, 14, 15), (20, 14, 45));
        mail.label = Some("email".into());
        mail.detail = Some("Inbox".into());
        let mut lab2 = block((20, 15, 0), (20, 16, 0));
        lab2.label = Some("lab".into());
        lab2.detail = Some("lc_pathways.py".into());

        let day_z = display_spans(
            &[lab.clone(), mail.clone(), lab2.clone()],
            day,
            ny(),
            Detail::Day,
        );
        // One primary track: email does not get a second overlapping box.
        assert!(day_z.len() <= 2, "day_z: {day_z:?}");
        assert!(day_z.iter().all(|d| d.meta.is_none()));

        let fine = display_spans(&[lab, mail, lab2], day, ny(), Detail::Fine);
        assert!(!fine.is_empty(), "fine");
        assert!(fine.iter().any(|d| d.meta.is_some()), "fine: {fine:?}");
    }

    #[test]
    fn day_note_compares_plan_to_tracked() {
        let b = block((15, 13, 0), (15, 15, 0));
        assert_eq!(day_note(&[], 0.0), "no plan · no tracked time");
        assert_eq!(day_note(std::slice::from_ref(&b), 0.0), "2 h tracked");
        assert_eq!(day_note(&[], 3600.0), "1 h planned");
        assert_eq!(
            day_note(std::slice::from_ref(&b), 3600.0),
            "1 h planned · 2 h tracked · +1 h"
        );
    }
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
