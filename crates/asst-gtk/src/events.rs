//! Calendar events on the Day view: all-day chips above the grid, timed ones
//! as fixed blocks in a lane of their own beside the plan. They are the
//! server's, so there is nothing to drag, resize or open here.

use asst_core::api::EventView;
use chrono::NaiveDate;
use chrono_tz::Tz;
use relm4::gtk::{self, prelude::*};

use crate::model::{clock_label, span_columns};
use crate::{daygrid, ui};

/// A timed event, clipped to the day on view, with its overlap column.
pub struct Timed<'a> {
    pub event: &'a EventView,
    pub start_min: f64,
    pub end_min: f64,
    col: usize,
    cols: usize,
}

/// Timed events on the grid, and where each sits. An event that runs on past
/// midnight is clipped to `day` (`minutes_from_midnight`).
pub fn timed(events: &[EventView], day: NaiveDate, zone: Tz) -> Vec<Timed<'_>> {
    let spans: Vec<(&EventView, f64, f64)> = events
        .iter()
        .filter(|e| !e.all_day)
        .map(|e| {
            (
                e,
                daygrid::minutes_from_midnight(e.start, day, zone),
                daygrid::minutes_from_midnight(e.end, day, zone),
            )
        })
        .collect();
    let boxes: Vec<(f64, f64)> = spans.iter().map(|(_, s, e)| (*s, *e)).collect();
    spans
        .iter()
        .zip(span_columns(&boxes))
        .map(|((event, start_min, end_min), (col, cols))| Timed {
            event,
            start_min: *start_min,
            end_min: *end_min,
            col,
            cols,
        })
        .collect()
}

/// The words a chip's second line and tooltip share.
fn when_and_where(e: &Timed<'_>) -> String {
    let mut text = format!(
        "{} – {}",
        clock_label(e.start_min as u32),
        clock_label((e.end_min as u32).min(24 * 60 - 1))
    );
    if let Some(place) = &e.event.location {
        text.push_str(" · ");
        text.push_str(place);
    }
    text
}

fn tinted(widget: &impl IsA<gtk::Widget>, event: &EventView) {
    if let Some(class) = event.color.as_deref().and_then(ui::tint) {
        widget.add_css_class(&class);
    }
}

pub fn fill_lane(events: &[Timed<'_>], overlay: &gtk::Overlay, reflow: &daygrid::Reflow) {
    for e in events {
        let chip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chip.add_css_class("event-block");
        tinted(&chip, e.event);
        chip.set_hexpand(true);
        chip.set_margin_start(2);
        chip.set_margin_end(2);
        let details = when_and_where(e);
        chip.set_tooltip_text(Some(&format!(
            "{} · {} · {}",
            e.event.summary, details, e.event.calendar_name
        )));

        let title = gtk::Label::new(Some(&e.event.summary));
        title.add_css_class("event-block-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&title);
        let meta = gtk::Label::new(Some(&details));
        meta.add_css_class("event-block-meta");
        meta.set_xalign(0.0);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&meta);

        // Spacers put the chip in its overlap column. Homogeneous, or a long
        // title would widen its column over the next.
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_hexpand(true);
        row.set_homogeneous(true);
        for c in 0..e.cols.max(1) {
            if c == e.col {
                row.append(&chip);
            } else {
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_hexpand(true);
                row.append(&spacer);
            }
        }

        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.append(&row);
        overlay.add_overlay(&wrap);
        let span = daygrid::Span {
            start_min: e.start_min,
            end_min: e.end_min,
            min_h: 18.0,
            title_at: 0.0,
            meta_at: 34.0,
        };
        reflow.place(&wrap, &chip, span, Some(&title), Some(&meta));
    }
}

/// A row of the day's all-day events above the grid, when it has any. They
/// hold no place on the hour axis.
pub fn all_day_strip(events: &[EventView]) -> Option<gtk::FlowBox> {
    let all_day: Vec<&EventView> = events.iter().filter(|e| e.all_day).collect();
    if all_day.is_empty() {
        return None;
    }
    let strip = gtk::FlowBox::new();
    strip.add_css_class("event-strip");
    strip.set_selection_mode(gtk::SelectionMode::None);
    strip.set_homogeneous(false);
    strip.set_max_children_per_line(6);
    strip.set_column_spacing(4);
    strip.set_row_spacing(4);
    strip.set_focusable(false);
    for e in all_day {
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        chip.add_css_class("event-all-day");
        tinted(&chip, e);
        chip.set_tooltip_text(Some(&format!("{} · {}", e.summary, e.calendar_name)));
        let title = gtk::Label::new(Some(&e.summary));
        title.add_css_class("event-block-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&title);
        strip.insert(&chip, -1);
        if let Some(child) = strip.last_child() {
            child.set_focusable(false);
        }
    }
    Some(strip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn event(all_day: bool, start_h: u32, end_h: u32) -> EventView {
        let at = |h| {
            ny().with_ymd_and_hms(2026, 9, 14, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
                + i64::from(h) * 3_600_000
        };
        EventView {
            href: "/c/e.ics".into(),
            calendar: "/c/".into(),
            calendar_name: "Personal".into(),
            color: None,
            summary: "It".into(),
            location: None,
            all_day,
            start: at(start_h),
            end: at(end_h),
        }
    }

    #[test]
    fn timed_events_are_clipped_to_the_day_and_share_width_when_they_overlap() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let events = [
            event(true, 0, 24),
            event(false, 9, 10),
            event(false, 9, 11),
            event(false, 23, 26),
        ];
        let timed = timed(&events, day, ny());
        let at: Vec<(f64, f64, usize, usize)> = timed
            .iter()
            .map(|t| (t.start_min, t.end_min, t.col, t.cols))
            .collect();
        assert_eq!(
            at,
            [
                (540.0, 600.0, 0, 2),
                (540.0, 660.0, 1, 2),
                (1380.0, 1440.0, 0, 1),
            ]
        );
    }

    #[test]
    fn the_clock_never_reads_past_the_last_minute_of_the_day() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let events = [event(false, 23, 26)];
        let timed = timed(&events, day, ny());
        assert_eq!(when_and_where(&timed[0]), "11pm – 11:59pm");
    }
}
