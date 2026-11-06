//! Calendar events, read-only: what a stored VEVENT says about one day.
//! Nothing here writes iCalendar; the server's copy stays as it came.
//!
//! A repeating event's RRULE is followed. EXDATE and RECURRENCE-ID overrides
//! are not read yet (#26's later slice), so a cancelled or moved occurrence
//! of a series still shows where the rule puts it.
//!
//! A timed event's alarms are read for asstd to ring (#33); nothing marks
//! them acknowledged on the server.

use chrono::{DateTime, Days, Duration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::ical::{Component, Ical};
use crate::recur;
use crate::task::{Alarm, timed_alarm};
use crate::time::{Trigger, When, parse_duration, resolve};

/// One occurrence on a day, as the window draws it. Times are epoch
/// milliseconds, unclipped: an event from yesterday starts yesterday.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventView {
    pub href: String,
    pub calendar: String,
    pub calendar_name: String,
    pub color: Option<String>,
    pub summary: String,
    pub location: Option<String>,
    pub all_day: bool,
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub all_day: bool,
    pub start: i64,
    pub end: i64,
}

/// One alarm of one occurrence: when it rings, and the occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ring {
    pub at: DateTime<Utc>,
    pub occurrence: Occurrence,
}

/// An alarm as a notification shows it: when it rings, and the occurrence
/// it is for, as the window draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRing {
    pub at: DateTime<Utc>,
    pub event: EventView,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub summary: String,
    pub location: Option<String>,
    start: When,
    /// How long each occurrence lasts: whole days when `start` is a date.
    length: Duration,
    rule: Option<String>,
    alarms: Vec<Alarm>,
}

impl Event {
    /// The event or series in a resource. None without a VEVENT that has a
    /// start, or when it is cancelled.
    pub fn from_ical(ical: &Ical) -> Option<Event> {
        let event = ical.event()?;
        let cancelled = event
            .property("STATUS")
            .is_some_and(|s| s.value().trim().eq_ignore_ascii_case("CANCELLED"));
        if cancelled {
            return None;
        }
        let start = When::from_property(event.property("DTSTART")?)?;
        Some(Event {
            summary: event
                .property("SUMMARY")
                .map(|s| s.text_value())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Untitled event".to_string()),
            location: event
                .property("LOCATION")
                .map(|l| l.text_value())
                .filter(|l| !l.trim().is_empty()),
            length: length_of(event, &start),
            start,
            rule: event.property("RRULE").map(|r| r.value().to_string()),
            alarms: event
                .components()
                .filter(|c| c.is("VALARM"))
                // RFC 9074's ACTION:NONE holds an alarm's place; it never rings.
                .filter(|a| {
                    !a.property("ACTION")
                        .is_some_and(|p| p.value().trim().eq_ignore_ascii_case("NONE"))
                })
                .filter_map(timed_alarm)
                .collect(),
        })
    }

    /// Epoch milliseconds to find it by: its first start, and its end unless
    /// it repeats, when there is no telling here.
    pub fn bounds(&self, zone: Tz) -> (i64, Option<i64>) {
        let start = self.start.instant(zone);
        let end = self.rule.is_none().then(|| start + self.length);
        (start.timestamp_millis(), end.map(|e| e.timestamp_millis()))
    }

    /// Its occurrences that touch the local day `day`, in `zone`.
    pub fn on(&self, day: NaiveDate, zone: Tz) -> Vec<Occurrence> {
        let (day_start, day_end) = day_bounds(day, zone);
        let starts = match &self.rule {
            None => vec![self.start.clone()],
            // An occurrence that began before the day can reach into it.
            Some(rule) => recur::between(rule, &self.start, day_start - self.length, day_end, zone)
                .unwrap_or_else(|e| {
                    log::debug!("reading the rule {rule:?}: {e}");
                    vec![self.start.clone()]
                }),
        };
        starts
            .iter()
            .filter_map(|start| self.occurrence(start, day_start, day_end, zone))
            .collect()
    }

    /// The alarms of its occurrences that ring from `from` up to but not
    /// including `to`, soonest first, leaving out any acknowledged (on any
    /// device) since it rang. An all-day event's alarms stay silent: #33
    /// asks for a timed event's.
    pub fn alarms_between(&self, from: DateTime<Utc>, to: DateTime<Utc>, zone: Tz) -> Vec<Ring> {
        if !self.start.has_time() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for alarm in &self.alarms {
            let rings = match &alarm.trigger {
                Trigger::Absolute { at } if from <= *at && *at < to => {
                    vec![Ring {
                        at: *at,
                        occurrence: self.timed(&self.occurrence_at(*at, zone), zone),
                    }]
                }
                Trigger::Absolute { .. } => Vec::new(),
                Trigger::Relative { offset, from_due } => {
                    // RELATED=END counts from each occurrence's end.
                    let shift = *offset
                        + if *from_due {
                            self.length
                        } else {
                            Duration::zero()
                        };
                    self.starts_between(from - shift, to - shift, zone)
                        .iter()
                        .map(|start| Ring {
                            at: start.instant(zone) + shift,
                            occurrence: self.timed(start, zone),
                        })
                        .collect()
                }
            };
            out.extend(
                rings
                    .into_iter()
                    .filter(|r| alarm.acknowledged.is_none_or(|ack| ack < r.at)),
            );
        }
        out.sort_by_key(|r| r.at);
        out
    }

    /// The starts of its occurrences from `from` up to but not including `to`.
    fn starts_between(&self, from: DateTime<Utc>, to: DateTime<Utc>, zone: Tz) -> Vec<When> {
        let first = || {
            let at = self.start.instant(zone);
            (from <= at && at < to)
                .then(|| self.start.clone())
                .into_iter()
                .collect()
        };
        match &self.rule {
            None => first(),
            Some(rule) => recur::between(rule, &self.start, from, to, zone).unwrap_or_else(|e| {
                log::debug!("reading the rule {rule:?}: {e}");
                first()
            }),
        }
    }

    /// The start of the occurrence an alarm at a fixed time is for: the one
    /// under way then or the next, else the first. A series rarely has one.
    fn occurrence_at(&self, at: DateTime<Utc>, zone: Tz) -> When {
        if self.rule.is_none() {
            return self.start.clone();
        }
        self.starts_between(at - self.length, at + Duration::days(366), zone)
            .into_iter()
            .next()
            .unwrap_or_else(|| self.start.clone())
    }

    /// The timed occurrence starting at `start`.
    fn timed(&self, start: &When, zone: Tz) -> Occurrence {
        let from = start.instant(zone);
        Occurrence {
            all_day: false,
            start: from.timestamp_millis(),
            end: (from + self.length).timestamp_millis(),
        }
    }

    fn occurrence(
        &self,
        start: &When,
        day_start: DateTime<Utc>,
        day_end: DateTime<Utc>,
        zone: Tz,
    ) -> Option<Occurrence> {
        let (from, to) = match start {
            When::Date { date } => {
                let days = self.length.num_days().max(1) as u64;
                let last = date.checked_add_days(Days::new(days))?;
                (start.instant(zone), day_bounds(last, zone).0)
            }
            timed => {
                let from = timed.instant(zone);
                (from, from + self.length)
            }
        };
        // A timed event with no length is at its moment, and still shows.
        let touches = from < day_end && (to > day_start || (from == to && from >= day_start));
        touches.then(|| Occurrence {
            all_day: !start.has_time(),
            start: from.timestamp_millis(),
            end: to.timestamp_millis(),
        })
    }
}

fn day_bounds(day: NaiveDate, zone: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    let next = day.succ_opt().unwrap_or(day);
    (
        resolve(zone, day.and_time(NaiveTime::MIN)),
        resolve(zone, next.and_time(NaiveTime::MIN)),
    )
}

/// DTEND or DURATION against the start. With neither, a date lasts the day
/// and a time has no length (RFC 5545 3.6.1). Never negative.
fn length_of(event: &Component, start: &When) -> Duration {
    let end = event.property("DTEND").and_then(When::from_property);
    let length = match (start, end) {
        (When::Date { date }, Some(When::Date { date: last })) => {
            Duration::days(last.signed_duration_since(*date).num_days())
        }
        // Compared in UTC, whatever zone is local: both name instants.
        (_, Some(end)) => end.instant(Tz::UTC) - start.instant(Tz::UTC),
        (_, None) => event
            .property("DURATION")
            .and_then(|d| parse_duration(d.value()))
            .unwrap_or(Duration::zero()),
    };
    match start {
        When::Date { .. } => length.max(Duration::days(1)),
        _ => length.max(Duration::zero()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn event(lines: &str) -> Event {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nSUMMARY:It\r\n{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            lines.replace('\n', "\r\n")
        );
        Event::from_ical(&Ical::parse(&ics)).expect("an event")
    }

    /// Start and end of each occurrence as local `HH:MM`, days apart from `d`
    /// spelled out only when they differ.
    fn clock(e: &Event, d: NaiveDate) -> Vec<(String, String)> {
        let show = |ms: i64| {
            DateTime::from_timestamp_millis(ms)
                .unwrap()
                .with_timezone(&ny())
                .format("%m-%d %H:%M")
                .to_string()
        };
        e.on(d, ny())
            .iter()
            .map(|o| (show(o.start), show(o.end)))
            .collect()
    }

    #[test]
    fn a_zoned_event_is_read_in_its_own_zone() {
        let e = event(
            "DTSTART;TZID=Europe/Lisbon:20260914T150000\nDTEND;TZID=Europe/Lisbon:20260914T160000",
        );
        assert_eq!(
            clock(&e, day(2026, 9, 14)),
            [("09-14 10:00".to_string(), "09-14 11:00".to_string())]
        );
        assert!(e.on(day(2026, 9, 15), ny()).is_empty());
    }

    #[test]
    fn duration_stands_in_for_an_end_and_no_end_is_a_moment() {
        let e = event("DTSTART:20260914T140000Z\nDURATION:PT90M");
        assert_eq!(clock(&e, day(2026, 9, 14))[0].1, "09-14 11:30");
        let e = event("DTSTART:20260914T140000Z");
        assert_eq!(
            clock(&e, day(2026, 9, 14)),
            [("09-14 10:00".to_string(), "09-14 10:00".to_string())]
        );
    }

    #[test]
    fn an_all_day_event_covers_its_days_and_not_the_one_after_its_end() {
        let e = event("DTSTART;VALUE=DATE:20260914\nDTEND;VALUE=DATE:20260916");
        assert!(e.on(day(2026, 9, 13), ny()).is_empty());
        assert!(e.on(day(2026, 9, 14), ny())[0].all_day);
        assert_eq!(e.on(day(2026, 9, 15), ny()).len(), 1);
        assert!(
            e.on(day(2026, 9, 16), ny()).is_empty(),
            "DTEND is exclusive"
        );
        // With no end, one day.
        let e = event("DTSTART;VALUE=DATE:20260914");
        assert_eq!(e.on(day(2026, 9, 14), ny()).len(), 1);
        assert!(e.on(day(2026, 9, 15), ny()).is_empty());
    }

    #[test]
    fn an_event_past_midnight_is_on_both_days() {
        let e = event("DTSTART:20260915T030000Z\nDTEND:20260915T050000Z");
        let d14 = clock(&e, day(2026, 9, 14));
        assert_eq!(
            d14,
            [("09-14 23:00".to_string(), "09-15 01:00".to_string())]
        );
        assert_eq!(clock(&e, day(2026, 9, 15)), d14);
        assert!(e.on(day(2026, 9, 16), ny()).is_empty());
    }

    #[test]
    fn a_birthday_comes_back_every_year() {
        let e = event("DTSTART;VALUE=DATE:19900305\nDTEND;VALUE=DATE:19900306\nRRULE:FREQ=YEARLY");
        assert_eq!(e.on(day(2026, 3, 5), ny()).len(), 1);
        assert!(e.on(day(2026, 3, 5), ny())[0].all_day);
        assert!(e.on(day(2026, 3, 6), ny()).is_empty());
        assert!(e.on(day(1989, 3, 5), ny()).is_empty());
    }

    #[test]
    fn a_weekly_series_keeps_its_wall_clock_across_a_clock_change() {
        let e = event(
            "DTSTART;TZID=America/New_York:20261019T090000\nDTEND;TZID=America/New_York:20261019T093000\nRRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=4",
        );
        let week = |m, d| clock(&e, day(2026, m, d));
        assert_eq!(week(10, 26)[0].0, "10-26 09:00");
        // New York leaves daylight time on 1 November.
        assert_eq!(
            week(11, 2)[0],
            ("11-02 09:00".to_string(), "11-02 09:30".to_string())
        );
        assert!(week(10, 27).is_empty(), "a Tuesday");
        assert!(week(11, 16).is_empty(), "COUNT ran out");
    }

    #[test]
    fn a_series_reaching_into_a_day_from_the_night_before_shows() {
        let e = event("DTSTART:20260915T030000Z\nDTEND:20260915T050000Z\nRRULE:FREQ=DAILY");
        // 23:00 to 01:00 local: the 16th has one that began on the 15th, and
        // its own at 23:00.
        assert_eq!(e.on(day(2026, 9, 16), ny()).len(), 2);
    }

    #[test]
    fn a_rule_that_cannot_be_read_leaves_the_first_occurrence() {
        let e = event("DTSTART:20260914T140000Z\nDTEND:20260914T150000Z\nRRULE:FREQ=SOMETIMES");
        assert_eq!(e.on(day(2026, 9, 14), ny()).len(), 1);
        assert!(e.on(day(2026, 9, 21), ny()).is_empty());
    }

    #[test]
    fn a_cancelled_event_and_a_lone_override_are_not_events() {
        let parse = |body: &str| {
            Event::from_ical(&Ical::parse(&format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20260914T140000Z\r\n{body}END:VEVENT\r\nEND:VCALENDAR\r\n"
            )))
        };
        assert!(parse("").is_some());
        assert!(parse("STATUS:CANCELLED\r\n").is_none());
        assert!(parse("RECURRENCE-ID:20260914T140000Z\r\n").is_none());
        assert!(
            Event::from_ical(&Ical::parse(
                "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:x\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
            ))
            .is_none()
        );
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Each ring from `from` to `to` as (when it rings, the start of its
    /// occurrence), both local `MM-DD HH:MM`.
    fn rings(e: &Event, from: &str, to: &str) -> Vec<(String, String)> {
        let show = |at: DateTime<Utc>| at.with_timezone(&ny()).format("%m-%d %H:%M").to_string();
        e.alarms_between(utc(from), utc(to), ny())
            .iter()
            .map(|r| {
                (
                    show(r.at),
                    show(DateTime::from_timestamp_millis(r.occurrence.start).unwrap()),
                )
            })
            .collect()
    }

    fn alarm(trigger: &str) -> String {
        format!("BEGIN:VALARM\nACTION:DISPLAY\n{trigger}\nEND:VALARM")
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn an_alarm_rings_before_its_event_and_says_which_occurrence() {
        let e = event(&format!(
            "DTSTART:20260914T140000Z\nDTEND:20260914T150000Z\n{}",
            alarm("TRIGGER:-PT15M")
        ));
        let r = rings(&e, "2026-09-14T13:00:00Z", "2026-09-14T14:00:00Z");
        assert_eq!(r, [pair("09-14 09:45", "09-14 10:00")]);
        let ring = &e.alarms_between(
            utc("2026-09-14T13:00:00Z"),
            utc("2026-09-14T14:00:00Z"),
            ny(),
        )[0];
        assert_eq!(ring.occurrence.end - ring.occurrence.start, 3_600_000);
        assert!(!ring.occurrence.all_day);
        // The window is half open.
        assert!(rings(&e, "2026-09-14T13:46:00Z", "2026-09-14T16:00:00Z").is_empty());
        assert_eq!(
            rings(&e, "2026-09-14T13:45:00Z", "2026-09-14T13:46:00Z").len(),
            1
        );
        assert!(rings(&e, "2026-09-14T13:00:00Z", "2026-09-14T13:45:00Z").is_empty());
    }

    #[test]
    fn an_alarm_counts_from_the_end_or_rings_at_a_set_time() {
        let e = event(&format!(
            "DTSTART:20260914T140000Z\nDTEND:20260914T150000Z\n{}\n{}",
            alarm("TRIGGER;RELATED=END:-PT5M"),
            alarm("TRIGGER;VALUE=DATE-TIME:20260914T120000Z"),
        ));
        assert_eq!(
            rings(&e, "2026-09-14T00:00:00Z", "2026-09-15T00:00:00Z"),
            [
                pair("09-14 08:00", "09-14 10:00"),
                pair("09-14 10:55", "09-14 10:00")
            ]
        );
    }

    #[test]
    fn a_series_rings_for_each_occurrence_on_its_wall_clock() {
        let e = event(&format!(
            "DTSTART;TZID=America/New_York:20261026T090000\nDTEND;TZID=America/New_York:20261026T093000\nRRULE:FREQ=WEEKLY;BYDAY=MO\n{}",
            alarm("TRIGGER:-PT10M")
        ));
        // New York leaves daylight time on 1 November.
        assert_eq!(
            rings(&e, "2026-10-26T00:00:00Z", "2026-11-03T00:00:00Z"),
            [
                pair("10-26 08:50", "10-26 09:00"),
                pair("11-02 08:50", "11-02 09:00")
            ]
        );
        // The alarm for Monday's occurrence rings on Monday, whatever
        // window it is asked about.
        assert_eq!(
            rings(&e, "2026-11-02T13:00:00Z", "2026-11-02T14:00:00Z"),
            [pair("11-02 08:50", "11-02 09:00")]
        );
        // A fixed time in a series names the occurrence under way or next.
        let fixed = event(&format!(
            "DTSTART:20260914T140000Z\nDTEND:20260914T150000Z\nRRULE:FREQ=DAILY\n{}",
            alarm("TRIGGER;VALUE=DATE-TIME:20260920T120000Z")
        ));
        assert_eq!(
            rings(&fixed, "2026-09-20T00:00:00Z", "2026-09-21T00:00:00Z"),
            [pair("09-20 08:00", "09-20 10:00")]
        );
    }

    #[test]
    fn an_alarm_acknowledged_since_it_rang_stays_quiet() {
        let series = |ack: &str| {
            event(&format!(
                "DTSTART:20260914T140000Z\nDTEND:20260914T150000Z\nRRULE:FREQ=DAILY\nBEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nACKNOWLEDGED:{ack}\nEND:VALARM"
            ))
        };
        // Dismissed on the phone at 9:50 on the 15th: that day's and the
        // ones before stay quiet, the 16th's rings.
        let e = series("20260915T135000Z");
        assert!(rings(&e, "2026-09-15T00:00:00Z", "2026-09-16T00:00:00Z").is_empty());
        assert_eq!(
            rings(&e, "2026-09-16T00:00:00Z", "2026-09-17T00:00:00Z").len(),
            1
        );
        // Acknowledged before the 15th's rang: it still rings.
        let e = series("20260915T134000Z");
        assert_eq!(
            rings(&e, "2026-09-15T00:00:00Z", "2026-09-16T00:00:00Z").len(),
            1
        );
    }

    #[test]
    fn an_all_day_event_and_a_placeholder_alarm_stay_silent() {
        let window = ("2026-09-13T00:00:00Z", "2026-09-16T00:00:00Z");
        let quiet = |lines: &str| rings(&event(lines), window.0, window.1).is_empty();
        assert!(quiet(&format!(
            "DTSTART;VALUE=DATE:20260914\n{}",
            alarm("TRIGGER:-PT15H")
        )));
        // What iOS writes for "no alert", and RFC 9074's ACTION:NONE.
        assert!(quiet(&format!(
            "DTSTART:20260914T140000Z\n{}",
            alarm("TRIGGER;VALUE=DATE-TIME:19760401T005545Z")
        )));
        assert!(quiet(
            "DTSTART:20260914T140000Z\nBEGIN:VALARM\nACTION:NONE\nTRIGGER:-PT15M\nEND:VALARM"
        ));
        // A place, not a time.
        assert!(quiet(
            "DTSTART:20260914T140000Z\nBEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nX-APPLE-PROXIMITY:ARRIVE\nEND:VALARM"
        ));
        assert!(!quiet(&format!(
            "DTSTART:20260914T140000Z\n{}",
            alarm("TRIGGER:-PT15M")
        )));
    }

    #[test]
    fn text_is_unescaped_and_a_blank_title_is_named() {
        let e = event("DTSTART:20260914T140000Z\nLOCATION:Room 1\\, Main St");
        assert_eq!(e.location.as_deref(), Some("Room 1, Main St"));
        assert_eq!(e.summary, "It");
        let blank = Event::from_ical(&Ical::parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20260914T140000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ))
        .unwrap();
        assert_eq!(blank.summary, "Untitled event");
    }
}
