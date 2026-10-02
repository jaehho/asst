//! The Time view's day timeline: one row per tracked block.

use asst_core::api::BlockView;
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use relm4::gtk::{self, prelude::*};

/// `1h 05m` / `42m` / `30s`.
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

/// Clock times in the window's zone, `14:05 – 14:40`.
fn clock_range(b: &BlockView, zone: Tz) -> String {
    let start = Utc
        .timestamp_millis_opt(b.start)
        .single()
        .unwrap_or_else(Utc::now);
    let end = Utc
        .timestamp_millis_opt(b.end)
        .single()
        .unwrap_or_else(Utc::now);
    let s = start.with_timezone(&zone).format("%H:%M").to_string();
    let e = end.with_timezone(&zone).format("%H:%M").to_string();
    format!("{s} – {e}")
}

/// Stable CSS tint class from the tag, so the same project keeps its color.
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

/// Sum of block seconds (actual only).
pub fn total_seconds(blocks: &[BlockView]) -> f64 {
    blocks
        .iter()
        .filter(|b| !b.planned)
        .map(duration)
        .sum()
}

/// One tracked block: tag, clock range, duration.
pub fn time_row(b: &BlockView, zone: Tz) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("time-row");
    row.set_selectable(true);
    row.set_activatable(false);

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.add_css_class("time-item");

    let swatch = gtk::Label::new(Some("●"));
    swatch.add_css_class("time-swatch");
    swatch.add_css_class(tint_for(b.label.as_deref().unwrap_or("untagged")));
    line.append(&swatch);

    let mid = gtk::Box::new(gtk::Orientation::Vertical, 0);
    mid.set_hexpand(true);
    let title = gtk::Label::new(Some(b.label.as_deref().unwrap_or("untagged")));
    title.add_css_class("time-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let when = gtk::Label::new(Some(&clock_range(b, zone)));
    when.add_css_class("dim-label");
    when.add_css_class("caption");
    when.set_xalign(0.0);
    mid.append(&title);
    mid.append(&when);
    line.append(&mid);

    let secs = duration(b);
    let dur = gtk::Label::new(Some(&dur_label(secs)));
    dur.add_css_class("time-duration");
    dur.add_css_class("dim-label");
    line.append(&dur);

    row.set_child(Some(&line));
    row
}

/// Day timeline, morning first (by start).
pub fn timeline(blocks: &[BlockView], zone: Tz) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("time-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let mut sorted = blocks.to_vec();
    sorted.sort_by_key(|b| (b.start, b.end));
    for b in &sorted {
        list.append(&time_row(b, zone));
    }
    list
}

/// Header note for the day: total tracked time.
pub fn day_note(blocks: &[BlockView]) -> String {
    let total = total_seconds(blocks);
    if total <= 0.0 {
        return "no tracked time".into();
    }
    format!("{} tracked", dur_label(total))
}

/// The Time view column: title, day stepper, note, then the block list.
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
    title.set_hexpand(true);
    head.append(&title);

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
    today.add_css_class("flat");
    {
        let tx = tx.clone();
        let showing = o.time_day;
        today.connect_clicked(move |_| {
            let delta = showing.signed_duration_since(chrono::Local::now().date_naive())
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

    let day_label = o
        .time_day
        .format("%Y-%m-%d (%a)")
        .to_string();
    let note = gtk::Label::new(Some(&format!("{day_label} · {}", day_note(o.blocks))));
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    note.set_xalign(0.0);
    root.append(&note);

    let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
    rule.add_css_class("section-rule");
    root.append(&rule);

    if o.blocks.is_empty() {
        let empty = gtk::Label::new(Some("No tracked time for this day."));
        empty.add_css_class("dim-label");
        empty.set_justify(gtk::Justification::Center);
        empty.set_margin_top(24);
        root.append(&empty);
    } else {
        root.append(&timeline(o.blocks, o.now.timezone()));
    }

    crate::content::Built {
        root,
        rows: Vec::new(),
    }
}
