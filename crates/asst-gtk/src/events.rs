//! Calendar events on the Day view: all-day chips above the grid, timed ones
//! as fixed blocks in the plan's lane, beside any block at the same hour.
//! They are the server's, so there is nothing to drag, resize or open here.

use asst_core::api::EventView;
use chrono::NaiveDate;
use chrono_tz::Tz;
use relm4::gtk::{self, prelude::*};

use crate::model::{EventSpan, clock_label};
use crate::{daygrid, ui};

/// A timed event, clipped to the day on view.
pub struct Timed<'a> {
    pub event: &'a EventView,
    pub start_min: f64,
    pub end_min: f64,
}

/// Timed events on the grid, in the order given. An event that runs on past
/// midnight is clipped to `day` (`minutes_from_midnight`).
pub fn timed(events: &[EventView], day: NaiveDate, zone: Tz) -> Vec<Timed<'_>> {
    events
        .iter()
        .filter(|e| !e.all_day)
        .map(|event| Timed {
            event,
            start_min: daygrid::minutes_from_midnight(event.start, day, zone),
            end_min: daygrid::minutes_from_midnight(event.end, day, zone),
        })
        .collect()
}

/// The spans `DayPlan::beside_events` lays out with the blocks.
pub fn spans(events: &[Timed<'_>]) -> Vec<(f64, f64)> {
    events.iter().map(|e| (e.start_min, e.end_min)).collect()
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

/// The timed events on the plan's lane, each in the column `places` gives
/// it (`DayPlan::events`, laid out from these same events in this order).
pub fn fill_lane(
    events: &[Timed<'_>],
    places: &[EventSpan],
    overlay: &gtk::Overlay,
    columns: &daygrid::Columns,
    reflow: &daygrid::Reflow,
) {
    debug_assert_eq!(events.len(), places.len(), "one place per event");
    for (e, place) in events.iter().zip(places) {
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

        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.append(&chip);
        overlay.add_overlay(&wrap);
        columns.set(&wrap, place.col, place.cols);
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
    fn timed_events_are_clipped_to_the_day() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let events = [
            event(true, 0, 24),
            event(false, 9, 10),
            event(false, 9, 11),
            event(false, 23, 26),
        ];
        let timed = timed(&events, day, ny());
        assert_eq!(
            spans(&timed),
            [(540.0, 600.0), (540.0, 660.0), (1380.0, 1440.0)]
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
