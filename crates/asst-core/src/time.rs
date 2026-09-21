//! Dates and times as iCalendar writes them, and the local zone they are
//! read in.

use std::str::FromStr;

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
    Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::ical::{Component, Property};

/// A DUE or DTSTART value, in the form it was (or will be) written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum When {
    /// `VALUE=DATE`: a day, no time.
    Date { date: NaiveDate },
    /// No zone at all: the same wall-clock time wherever you are.
    Floating { at: NaiveDateTime },
    /// `Z` suffix.
    Utc { at: DateTime<Utc> },
    /// `TZID=` parameter.
    Zoned { at: NaiveDateTime, tzid: String },
}

impl When {
    pub fn from_property(p: &Property) -> Option<When> {
        let v = p.value().trim();
        let is_date = p
            .param("VALUE")
            .is_some_and(|t| t.eq_ignore_ascii_case("DATE"))
            || (v.len() == 8 && !v.contains('T'));
        if is_date {
            return parse_date(v).map(|date| When::Date { date });
        }
        let (at, utc) = parse_datetime(v)?;
        Some(match (utc, p.param("TZID")) {
            (true, _) => When::Utc { at: at.and_utc() },
            (false, Some(tzid)) => When::Zoned {
                at,
                tzid: tzid.to_string(),
            },
            (false, None) => When::Floating { at },
        })
    }

    pub fn to_property(&self, name: &str) -> Property {
        match self {
            When::Date { date } => {
                Property::new(name, date.format("%Y%m%d").to_string()).with_param("VALUE", "DATE")
            }
            When::Floating { at } => Property::new(name, format_local(at)),
            When::Utc { at } => Property::new(name, format_utc(at)),
            When::Zoned { at, tzid } => {
                Property::new(name, format_local(at)).with_param("TZID", tzid)
            }
        }
    }

    pub fn has_time(&self) -> bool {
        !matches!(self, When::Date { .. })
    }

    /// The instant this names. A date is its first moment in `local`; a
    /// floating time, or a zone nobody has heard of, is read in `local`.
    pub fn instant(&self, local: Tz) -> DateTime<Utc> {
        match self {
            When::Date { date } => resolve(local, date.and_time(NaiveTime::MIN)),
            When::Floating { at } => resolve(local, *at),
            When::Utc { at } => *at,
            When::Zoned { at, tzid } => resolve(resolve_tzid(tzid).unwrap_or(local), *at),
        }
    }

    /// The calendar day it falls on, seen from `local`.
    pub fn local_date(&self, local: Tz) -> NaiveDate {
        match self {
            When::Date { date } => *date,
            When::Floating { at } => at.date(),
            _ => self.instant(local).with_timezone(&local).date_naive(),
        }
    }

    /// The same kind of value, moved to a new instant: a date stays a date,
    /// a zoned time keeps its zone.
    pub fn with_instant(&self, instant: DateTime<Utc>, local: Tz) -> When {
        match self {
            When::Date { .. } => When::Date {
                date: instant.with_timezone(&local).date_naive(),
            },
            When::Floating { .. } => When::Floating {
                at: instant.with_timezone(&local).naive_local(),
            },
            When::Utc { .. } => When::Utc { at: instant },
            When::Zoned { tzid, .. } => {
                let tz = resolve_tzid(tzid).unwrap_or(local);
                When::Zoned {
                    at: instant.with_timezone(&tz).naive_local(),
                    tzid: tzid.clone(),
                }
            }
        }
    }

    /// A wall-clock time in the local zone, written the way iOS writes one.
    pub fn local(at: NaiveDateTime, local: Tz) -> When {
        When::Zoned {
            at,
            tzid: local.name().to_string(),
        }
    }
}

/// An alarm trigger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Trigger {
    Absolute {
        at: DateTime<Utc>,
    },
    /// Offset from DTSTART, or from DUE when `from_due` (RELATED=END).
    Relative {
        #[serde(with = "seconds")]
        offset: Duration,
        from_due: bool,
    },
}

impl Trigger {
    pub fn from_property(p: &Property) -> Option<Trigger> {
        let v = p.value().trim();
        let absolute = p
            .param("VALUE")
            .is_some_and(|t| t.eq_ignore_ascii_case("DATE-TIME"))
            || (v.contains('T') && !v.starts_with(['P', '+', '-']));
        if absolute {
            let (at, _) = parse_datetime(v)?;
            return Some(Trigger::Absolute { at: at.and_utc() });
        }
        let from_due = p
            .param("RELATED")
            .is_some_and(|r| r.eq_ignore_ascii_case("END"));
        Some(Trigger::Relative {
            offset: parse_duration(v)?,
            from_due,
        })
    }

    pub fn to_property(&self) -> Property {
        match self {
            Trigger::Absolute { at } => {
                Property::new("TRIGGER", format_utc(at)).with_param("VALUE", "DATE-TIME")
            }
            Trigger::Relative { offset, from_due } => {
                let p = Property::new("TRIGGER", format_duration(*offset));
                if *from_due {
                    p.with_param("RELATED", "END")
                } else {
                    p
                }
            }
        }
    }
}

mod seconds {
    use chrono::Duration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(d.num_seconds())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::seconds(i64::deserialize(d)?))
    }
}

pub fn parse_date(v: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d").ok()
}

/// `YYYYMMDDTHHMMSS[Z]` → (wall time, is UTC).
pub fn parse_datetime(v: &str) -> Option<(NaiveDateTime, bool)> {
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let at = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M"))
        .ok()?;
    Some((at, utc))
}

pub fn format_utc(at: &DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

pub fn format_local(at: &NaiveDateTime) -> String {
    at.format("%Y%m%dT%H%M%S").to_string()
}

/// RFC 5545 DURATION: `[+-]P[nW][nD][T[nH][nM][nS]]`.
pub fn parse_duration(v: &str) -> Option<Duration> {
    let (sign, rest) = match v.as_bytes().first()? {
        b'-' => (-1, &v[1..]),
        b'+' => (1, &v[1..]),
        _ => (1, v),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in rest.chars() {
        match c.to_ascii_uppercase() {
            d if d.is_ascii_digit() => num.push(d),
            'T' if num.is_empty() => in_time = true,
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                any = true;
                total += n * match (unit, in_time) {
                    ('W', false) => 7 * 86400,
                    ('D', false) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    (any && num.is_empty()).then(|| Duration::seconds(sign * total))
}

pub fn format_duration(d: Duration) -> String {
    let mut secs = d.num_seconds();
    let sign = if secs < 0 { "-" } else { "" };
    secs = secs.abs();
    if secs == 0 {
        return "PT0S".to_string();
    }
    let (days, rem) = (secs / 86400, secs % 86400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let mut out = format!("{sign}P");
    if days > 0 {
        if days % 7 == 0 && rem == 0 {
            return format!("{sign}P{}W", days / 7);
        }
        out.push_str(&format!("{days}D"));
    }
    if rem > 0 {
        out.push('T');
        if h > 0 {
            out.push_str(&format!("{h}H"));
        }
        if m > 0 {
            out.push_str(&format!("{m}M"));
        }
        if s > 0 {
            out.push_str(&format!("{s}S"));
        }
    }
    out
}

/// A TZID as an IANA zone. Accepts the `/vendor/prefix/Area/City` form some
/// clients write.
pub fn resolve_tzid(tzid: &str) -> Option<Tz> {
    let tzid = tzid.trim().trim_matches('"');
    if let Ok(tz) = Tz::from_str(tzid) {
        return Some(tz);
    }
    let parts: Vec<&str> = tzid.split('/').filter(|s| !s.is_empty()).collect();
    (1..=parts.len().min(3))
        .rev()
        .find_map(|n| Tz::from_str(&parts[parts.len() - n..].join("/")).ok())
}

/// Wall time in a zone to an instant: the earlier reading when a fall-back
/// hour repeats, the hour after when spring-forward skipped it.
pub fn resolve(tz: Tz, at: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&at) {
        LocalResult::Single(t) => t.with_timezone(&Utc),
        LocalResult::Ambiguous(early, _) => early.with_timezone(&Utc),
        LocalResult::None => resolve(tz, at + Duration::hours(1)),
    }
}

/// The machine's zone: `$TZ` if it names one, else `/etc/localtime`, else UTC.
pub fn local_zone() -> Tz {
    std::env::var("TZ")
        .ok()
        .and_then(|tz| resolve_tzid(tz.trim_start_matches(':')))
        .or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .and_then(|n| Tz::from_str(&n).ok())
        })
        .unwrap_or(Tz::UTC)
}

/// A VTIMEZONE for `tz`, so a TZID written by asst is defined inside the
/// object as RFC 5545 requires. It describes the rules in force this year:
/// enough for any client to read the times asst writes.
pub fn vtimezone(tz: Tz, year: i32) -> Component {
    let mut vtz = Component::new("VTIMEZONE");
    vtz.set(Property::new("TZID", tz.name()));

    let offset_at = |t: DateTime<Utc>| {
        let local = t.with_timezone(&tz);
        (local.naive_local() - t.naive_utc()).num_seconds()
    };
    let start = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap();
    let mut transitions = Vec::new();
    let mut prev = offset_at(start);
    let mut t = start;
    while t.year() == year {
        let next = t + Duration::hours(1);
        let off = offset_at(next);
        if off != prev {
            // Zones like America/St_Johns change on the half hour.
            let exact = (1..=60)
                .map(|m| t + Duration::minutes(m))
                .find(|m| offset_at(*m) == off)
                .unwrap_or(next);
            transitions.push((exact, prev, off));
            prev = off;
        }
        t = next;
    }

    let fmt_offset = |secs: i64| {
        let sign = if secs < 0 { '-' } else { '+' };
        let secs = secs.abs();
        format!("{sign}{:02}{:02}", secs / 3600, secs % 3600 / 60)
    };
    let abbrev = |t: DateTime<Utc>| t.with_timezone(&tz).format("%Z").to_string();

    if transitions.len() != 2 {
        let mut std = Component::new("STANDARD");
        std.set(Property::new("DTSTART", "19700101T000000"));
        std.set(Property::new("TZNAME", abbrev(start)));
        std.set(Property::new("TZOFFSETFROM", fmt_offset(prev)));
        std.set(Property::new("TZOFFSETTO", fmt_offset(prev)));
        vtz.push(std);
        return vtz;
    }

    for (at, from, to) in transitions {
        let kind = if to > from { "DAYLIGHT" } else { "STANDARD" };
        // The wall-clock moment of the change, in the offset before it.
        let wall = at.naive_utc() + Duration::seconds(from);
        let day = wall.date();
        let nth = (day.day() as i64 - 1) / 7 + 1;
        let is_last = (day + Duration::days(7)).month() != day.month();
        let ord = if is_last && nth >= 4 { -1 } else { nth };
        // DTSTART is the rule's first onset; 1970 keeps it before any task.
        let onset = nth_weekday(1970, day.month(), day.weekday(), ord).and_time(wall.time());
        let mut c = Component::new(kind);
        c.set(Property::new("DTSTART", format_local(&onset)));
        c.set(Property::new(
            "RRULE",
            format!(
                "FREQ=YEARLY;BYMONTH={};BYDAY={}{}",
                day.month(),
                ord,
                weekday_code(day.weekday())
            ),
        ));
        c.set(Property::new("TZNAME", abbrev(at)));
        c.set(Property::new("TZOFFSETFROM", fmt_offset(from)));
        c.set(Property::new("TZOFFSETTO", fmt_offset(to)));
        vtz.push(c);
    }
    vtz
}

/// The `ord`th `weekday` of a month; negative counts from the end.
fn nth_weekday(year: i32, month: u32, weekday: Weekday, ord: i64) -> NaiveDate {
    if ord > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
        let ahead = (7 + weekday.num_days_from_monday() as i64
            - first.weekday().num_days_from_monday() as i64)
            % 7;
        first + Duration::days(ahead + 7 * (ord - 1))
    } else {
        let next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .expect("valid month");
        let last = next_month - Duration::days(1);
        let back = (7 + last.weekday().num_days_from_monday() as i64
            - weekday.num_days_from_monday() as i64)
            % 7;
        last - Duration::days(back + 7 * (-ord - 1))
    }
}

pub fn weekday_code(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::Ical;

    fn prop(line: &str) -> Property {
        let ical = Ical::parse(&format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\n{line}\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        ));
        ical.todo().unwrap().properties().next().unwrap().clone()
    }

    #[test]
    fn reads_every_form() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let date = When::from_property(&prop("DUE;VALUE=DATE:20260915")).unwrap();
        assert_eq!(
            date,
            When::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
            }
        );
        assert!(!date.has_time());

        let zoned =
            When::from_property(&prop("DUE;TZID=America/New_York:20260915T090000")).unwrap();
        assert_eq!(
            zoned.instant(Tz::UTC),
            Utc.with_ymd_and_hms(2026, 9, 15, 13, 0, 0).unwrap()
        );

        let utc = When::from_property(&prop("DUE:20260915T130000Z")).unwrap();
        assert_eq!(
            utc.local_date(ny),
            NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
        );

        let floating = When::from_property(&prop("DTSTART:20260915T090000")).unwrap();
        assert_eq!(
            floating.instant(ny),
            Utc.with_ymd_and_hms(2026, 9, 15, 13, 0, 0).unwrap()
        );
    }

    #[test]
    fn writes_back_what_it_read() {
        for line in [
            "DUE;VALUE=DATE:20260915",
            "DUE;TZID=America/New_York:20260915T090000",
            "DUE:20260915T130000Z",
            "DUE:20260915T090000",
        ] {
            let p = prop(line);
            let w = When::from_property(&p).unwrap();
            let mut out = String::new();
            let mut cal = Component::new("VCALENDAR");
            let mut todo = Component::new("VTODO");
            todo.set(w.to_property("DUE"));
            cal.push(todo);
            out.push_str(&Ical::new(cal).to_string());
            assert!(out.contains(&format!("{line}\r\n")), "{line} -> {out}");
        }
    }

    #[test]
    fn vendor_prefixed_tzids_resolve() {
        assert_eq!(
            resolve_tzid("/mozilla.org/20050126_1/America/New_York").map(|t| t.name()),
            Some("America/New_York")
        );
        assert_eq!(resolve_tzid("Nowhere/Special"), None);
    }

    #[test]
    fn dst_gaps_and_repeats() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let gap = NaiveDate::from_ymd_opt(2026, 3, 8)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(
            resolve(ny, gap),
            Utc.with_ymd_and_hms(2026, 3, 8, 7, 30, 0).unwrap()
        );
        let repeat = NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        assert_eq!(
            resolve(ny, repeat),
            Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap()
        );
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("P2W"), Some(Duration::days(14)));
        assert_eq!(parse_duration("PT"), None);
        assert_eq!(parse_duration("P1H"), None);
        for d in [
            Duration::minutes(-15),
            Duration::hours(26),
            Duration::days(14),
            Duration::zero(),
            Duration::seconds(90),
        ] {
            assert_eq!(parse_duration(&format_duration(d)), Some(d));
        }
    }

    #[test]
    fn triggers() {
        assert_eq!(
            Trigger::from_property(&prop("TRIGGER;VALUE=DATE-TIME:20260915T130000Z")),
            Some(Trigger::Absolute {
                at: Utc.with_ymd_and_hms(2026, 9, 15, 13, 0, 0).unwrap()
            })
        );
        assert_eq!(
            Trigger::from_property(&prop("TRIGGER;RELATED=END:-PT30M")),
            Some(Trigger::Relative {
                offset: Duration::minutes(-30),
                from_due: true
            })
        );
        assert_eq!(
            Trigger::from_property(&prop("TRIGGER:PT0S")),
            Some(Trigger::Relative {
                offset: Duration::zero(),
                from_due: false
            })
        );
    }

    #[test]
    fn vtimezone_for_new_york() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let mut cal = Component::new("VCALENDAR");
        cal.push(vtimezone(ny, 2026));
        let out = Ical::new(cal).to_string();
        assert!(out.contains("BEGIN:DAYLIGHT\r\nDTSTART:19700308T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nTZNAME:EDT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\n"), "{out}");
        assert!(out.contains("RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nTZNAME:EST\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\n"), "{out}");
    }

    #[test]
    fn vtimezone_with_last_sunday_and_half_hour_rules() {
        let render = |name: &str| {
            let mut cal = Component::new("VCALENDAR");
            cal.push(vtimezone(name.parse().unwrap(), 2026));
            Ical::new(cal).to_string()
        };
        let berlin = render("Europe/Berlin");
        assert!(
            berlin
                .contains("DTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\n"),
            "{berlin}"
        );
        assert!(
            berlin
                .contains("DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\n"),
            "{berlin}"
        );
        let st_johns = render("America/St_Johns");
        assert!(
            st_johns
                .contains("DTSTART:19700308T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n"),
            "{st_johns}"
        );
        assert!(
            st_johns.contains("TZOFFSETFROM:-0330\r\nTZOFFSETTO:-0230\r\n"),
            "{st_johns}"
        );
    }

    #[test]
    fn vtimezone_without_dst() {
        let seoul: Tz = "Asia/Seoul".parse().unwrap();
        let mut cal = Component::new("VCALENDAR");
        cal.push(vtimezone(seoul, 2026));
        let out = Ical::new(cal).to_string();
        assert!(out.contains("BEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZNAME:KST\r\nTZOFFSETFROM:+0900\r\nTZOFFSETTO:+0900\r\nEND:STANDARD\r\n"), "{out}");
    }
}
