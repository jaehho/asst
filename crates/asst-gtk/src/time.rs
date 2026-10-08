//! The Time view: a Google Calendar–style day planner of tracked blocks.

use asst_core::api::BlockView;
use chrono::{NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use relm4::gtk::{self, glib, prelude::*};

/// Pixels per hour on the day grid.
const HOUR_H: i32 = 52;
/// Width of the hour-label gutter.
const GUTTER: i32 = 56;

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

fn step_button(icon: &str, tip: &str, delta: i32, tx: &crate::window::Tx) -> gtk::Button {
    use crate::window::Msg;
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(16);
    let b = gtk::Button::builder().child(&image).build();
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.set_focus_on_click(false);
    let tx = tx.clone();
    b.connect_clicked(move |_| {
        let _ = tx.send(Msg::TimeStep(delta));
    });
    b
}

/// Hour rows as the grid background (always allocates), chips on top.
fn day_grid(
    blocks: &[BlockView],
    day: NaiveDate,
    zone: Tz,
    now: chrono::DateTime<Tz>,
) -> gtk::Widget {
    let height = 24 * HOUR_H;

    // Hour labels, one row each — never clipped away.
    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(GUTTER, height);
    gutter.add_css_class("time-gutter");
    for h in 0..24u32 {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_size_request(GUTTER, HOUR_H);
        let lab = gtk::Label::new(Some(&hour_label(h)));
        lab.add_css_class("time-hour");
        lab.add_css_class("dim-label");
        lab.set_xalign(1.0);
        lab.set_yalign(0.0);
        lab.set_margin_top(-8);
        lab.set_margin_end(8);
        slot.append(&lab);
        gutter.append(&slot);
    }

    // Hour rules as stacked rows (reliable allocation).
    let rules = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rules.set_hexpand(true);
    rules.set_size_request(240, height);
    rules.add_css_class("time-grid");
    for _ in 0..24 {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_hexpand(true);
        slot.set_height_request(HOUR_H);
        slot.add_css_class("time-hour-slot");
        rules.append(&slot);
    }

    // Event chips over the rules. A normal box (not Fixed) so chips take
    // the column width and keep both rounded corners.
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&rules));

    let mut placed: Vec<(f64, f64, &BlockView)> = Vec::new();
    for b in blocks {
        if b.planned {
            continue;
        }
        let s = minutes_from_midnight(b.start, day, zone);
        // Real end for packing: inflating short spans stacked a rainbow of
        // overlapping lanes. Height gets a floor below.
        let e = minutes_from_midnight(b.end, day, zone).max(s);
        placed.push((s, e, b));
    }
    placed.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });

    let stack = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.set_halign(gtk::Align::Fill);
    stack.set_valign(gtk::Align::Start);
    stack.set_hexpand(true);
    let mut prev_bottom = 0.0f64;
    for (s, e, b) in placed.iter() {
        let top = s / 60.0 * f64::from(HOUR_H);
        let height_px = ((e - s) / 60.0 * f64::from(HOUR_H)).max(18.0);
        let label = b.label.as_deref().unwrap_or("untagged");
        let start_s = local_of(b.start, zone).format("%H:%M").to_string();
        let end_s = local_of(b.end, zone).format("%H:%M").to_string();
        let dur = dur_label(duration(b));

        let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chip.add_css_class("time-block");
        chip.add_css_class(tint_for(label));
        chip.set_hexpand(true);
        chip.set_height_request(height_px as i32);
        chip.set_margin_top((top - prev_bottom).max(0.0) as i32);
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

        stack.append(&chip);
        prev_bottom = top + height_px;
    }
    overlay.add_overlay(&stack);

    if now.date_naive() == day {
        let mins = f64::from(now.time().hour()) * 60.0
            + f64::from(now.time().minute())
            + f64::from(now.time().second()) / 60.0;
        let y = mins / 60.0 * f64::from(HOUR_H);
        // Dot and rule in one row so the rule sits on the dot's center.
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("time-now-row");
        row.set_halign(gtk::Align::Fill);
        row.set_valign(gtk::Align::Start);
        row.set_margin_top((y - 7.0) as i32);
        let dot = gtk::Label::new(Some("●"));
        dot.add_css_class("time-now-dot");
        row.append(&dot);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line.add_css_class("time-now");
        line.set_valign(gtk::Align::Center);
        line.set_hexpand(true);
        line.set_height_request(2);
        row.append(&line);
        overlay.add_overlay(&row);
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("time-planner");
    row.set_size_request(GUTTER + 240, height);
    row.append(&gutter);
    row.append(&overlay);
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

    // Header: title + date on the left, day controls pinned right.
    let head = gtk::CenterBox::new();
    head.add_css_class("time-head");
    let left = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some("Time"));
    title.add_css_class("view-title");
    left.append(&title);
    let date_lab = gtk::Label::new(Some(&o.time_day.format("%B %e, %Y").to_string()));
    date_lab.add_css_class("time-date");
    date_lab.set_xalign(0.0);
    left.append(&date_lab);
    head.set_start_widget(Some(&left));

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    controls.add_css_class("time-controls");
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
    controls.append(&step_button("go-previous-symbolic", "Previous day", -1, tx));
    controls.append(&step_button("go-next-symbolic", "Next day", 1, tx));
    controls.append(&today);
    head.set_end_widget(Some(&controls));
    root.append(&head);

    let note = gtk::Label::new(Some(&day_note(o.blocks)));
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    note.set_xalign(0.0);
    root.append(&note);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.add_css_class("time-scroll");
    scroll.set_child(Some(&day_grid(
        o.blocks,
        o.time_day,
        o.now.timezone(),
        o.now,
    )));
    root.append(&scroll);

    {
        let scroll = scroll.clone();
        glib::idle_add_local_once(move || {
            scroll.vadjustment().set_value(7.5 * f64::from(HOUR_H));
        });
    }

    crate::content::Built {
        root,
        rows: Vec::new(),
    }
}
