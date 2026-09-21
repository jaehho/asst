//! Quick-add syntax: `Call the bank #Errands p1 tomorrow 5pm !30m`.
//!
//! Recognized anywhere in the text, and removed from the title:
//! - `#list`: a list, by name without spaces, any case, or a unique prefix
//! - `p1`…`p4`: priority
//! - a date and/or time: `today`, `tomorrow`, `fri`, `next monday`,
//!   `in 3 days`, `in 2h`, `sep 20`, `20 sep`, `9/20`, `2026-09-20`,
//!   `5pm`, `17:30`, `at 9`, `noon`, `midnight`, `tonight`, `this evening`,
//!   `tomorrow morning`
//! - a repeat: `every day`, `daily`, `every weekday`, `every 2 weeks`,
//!   `every mon, thu`, `every 15th`, `monthly`, with an optional time
//! - `!`: remind at the due time; `!30m`, `!2h`, `!1d` before it; `!9am` at a time
//!
//! The first date wins; a later date-like word stays in the title.

use std::ops::Range;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Timelike, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::time::{Trigger, When, resolve, weekday_code};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    List,
    Priority,
    Date,
    Repeat,
    Alarm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parsed {
    pub summary: String,
    /// The list's name as the caller gave it in `lists`.
    pub list: Option<String>,
    /// p1 (high) … p4 (none).
    pub priority: Option<u8>,
    pub due: Option<When>,
    pub rrule: Option<String>,
    pub alarm: Option<Trigger>,
    /// Byte ranges of what was recognized, for highlighting as you type.
    pub spans: Vec<(Range<usize>, Kind)>,
}

struct Tok<'a> {
    text: &'a str,
    lower: String,
    span: Range<usize>,
}

fn tokenize(text: &str) -> Vec<Tok<'_>> {
    let mut toks = Vec::new();
    let mut start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                let t = &text[s..i];
                toks.push(Tok {
                    text: t,
                    lower: t.to_lowercase(),
                    span: s..i,
                });
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    toks
}

/// What a phrase at a position means; `n` tokens long.
enum Hit {
    List(String),
    Priority(u8),
    Alarm(AlarmSpec),
    Date {
        day: Option<NaiveDate>,
        time: Option<NaiveTime>,
        exact: Option<DateTime<Tz>>,
    },
    Repeat {
        rule: String,
        first: NaiveDate,
        time: Option<NaiveTime>,
    },
}

enum AlarmSpec {
    AtDue,
    Before(Duration),
    At(NaiveTime),
}

pub fn parse(text: &str, now: DateTime<Tz>, lists: &[String]) -> Parsed {
    parse_with(text, now, lists, true)
}

/// `parse`; with `dates` off, a date or a repeat stays part of the title
/// (`#list`, `p1` and `!` still count).
pub fn parse_with(text: &str, now: DateTime<Tz>, lists: &[String], dates: bool) -> Parsed {
    let toks = tokenize(text);
    let mut used = vec![false; toks.len()];
    let mut out = Parsed {
        summary: String::new(),
        list: None,
        priority: None,
        due: None,
        rrule: None,
        alarm: None,
        spans: Vec::new(),
    };
    let mut alarm = None;
    let mut have_date = false;
    let mut i = 0;
    while i < toks.len() {
        let Some((n, hit)) = match_at(&toks, i, now, lists) else {
            i += 1;
            continue;
        };
        let kind = match hit {
            Hit::List(name) if out.list.is_none() => {
                out.list = Some(name);
                Kind::List
            }
            Hit::Priority(p) if out.priority.is_none() => {
                out.priority = Some(p);
                Kind::Priority
            }
            Hit::Alarm(a) if alarm.is_none() => {
                alarm = Some(a);
                Kind::Alarm
            }
            Hit::Date { day, time, exact } if dates && !have_date => {
                have_date = true;
                out.due = Some(match (exact, day, time) {
                    (Some(at), _, _) => When::local(at.naive_local(), now.timezone()),
                    (None, Some(d), None) => When::Date { date: d },
                    (None, day, Some(t)) => {
                        // A bare time means its next occurrence.
                        let d = day.unwrap_or_else(|| {
                            if t > now.time() {
                                now.date_naive()
                            } else {
                                now.date_naive() + Duration::days(1)
                            }
                        });
                        When::local(d.and_time(t), now.timezone())
                    }
                    (None, None, None) => unreachable!("a date hit has a day or a time"),
                });
                Kind::Date
            }
            Hit::Repeat { rule, first, time } if dates && !have_date => {
                have_date = true;
                out.rrule = Some(rule);
                out.due = Some(match time {
                    Some(t) => When::local(first.and_time(t), now.timezone()),
                    None => When::Date { date: first },
                });
                Kind::Repeat
            }
            _ => {
                i += 1;
                continue;
            }
        };
        for u in &mut used[i..i + n] {
            *u = true;
        }
        out.spans
            .push((toks[i].span.start..toks[i + n - 1].span.end, kind));
        i += n;
    }
    out.summary = toks
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|(t, _)| t.text)
        .collect::<Vec<_>>()
        .join(" ");
    out.alarm = alarm.and_then(|a| match a {
        AlarmSpec::AtDue => out
            .due
            .as_ref()
            .filter(|d| d.has_time())
            .map(|d| Trigger::Absolute {
                at: d.instant(now.timezone()),
            }),
        AlarmSpec::Before(d) => out.due.as_ref().map(|_| Trigger::Relative {
            offset: -d,
            from_due: false,
        }),
        AlarmSpec::At(t) => {
            let day = match &out.due {
                Some(due) => due.local_date(now.timezone()),
                None if t > now.time() => now.date_naive(),
                None => now.date_naive() + Duration::days(1),
            };
            Some(Trigger::Absolute {
                at: resolve(now.timezone(), day.and_time(t)),
            })
        }
    });
    out
}

/// A whole string as a date and/or time (`tomorrow 5pm`, `fri`, `none` is not one).
pub fn parse_when(text: &str, now: DateTime<Tz>) -> Option<When> {
    let parsed = parse(text, now, &[]);
    let whole =
        parsed.summary.is_empty() && parsed.spans.len() == 1 && parsed.spans[0].1 == Kind::Date;
    whole.then_some(parsed.due).flatten()
}

fn match_at(toks: &[Tok], i: usize, now: DateTime<Tz>, lists: &[String]) -> Option<(usize, Hit)> {
    let t = &toks[i];
    if let Some(name) = t.text.strip_prefix('#').filter(|n| !n.is_empty()) {
        return find_list(name, lists).map(|l| (1, Hit::List(l)));
    }
    if let Some(p) = t
        .lower
        .strip_prefix('p')
        .and_then(|d| d.parse::<u8>().ok())
        .filter(|p| (1..=4).contains(p))
        && t.lower.len() == 2
    {
        return Some((1, Hit::Priority(p)));
    }
    if let Some(rest) = t.text.strip_prefix('!') {
        if rest.is_empty() {
            return Some((1, Hit::Alarm(AlarmSpec::AtDue)));
        }
        if let Some(d) = short_duration(&rest.to_lowercase()) {
            return Some((1, Hit::Alarm(AlarmSpec::Before(d))));
        }
        if let Some((_, time)) = time_at(
            &[Tok {
                text: rest,
                lower: rest.to_lowercase(),
                span: 0..0,
            }],
            0,
            false,
        ) {
            return Some((1, Hit::Alarm(AlarmSpec::At(time))));
        }
        return None;
    }
    if let Some(hit) = repeat_at(toks, i, now) {
        return Some(hit);
    }
    date_phrase_at(toks, i, now)
}

fn find_list(name: &str, lists: &[String]) -> Option<String> {
    let squash = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase()
    };
    let want = squash(name);
    if let Some(l) = lists.iter().find(|l| squash(l) == want) {
        return Some(l.clone());
    }
    let mut prefixed = lists.iter().filter(|l| squash(l).starts_with(&want));
    match (prefixed.next(), prefixed.next()) {
        (Some(l), None) => Some(l.clone()),
        _ => None,
    }
}

/// `30m`, `2h`, `1d`, `15min`, `1w`.
fn short_duration(s: &str) -> Option<Duration> {
    let digits = s.find(|c: char| !c.is_ascii_digit())?;
    let n: i64 = s[..digits].parse().ok()?;
    Some(match &s[digits..] {
        "m" | "min" | "mins" => Duration::minutes(n),
        "h" | "hr" | "hrs" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        _ => return None,
    })
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s.trim_end_matches(',') {
        "monday" | "mon" => Weekday::Mon,
        "tuesday" | "tue" | "tues" => Weekday::Tue,
        "wednesday" | "wed" => Weekday::Wed,
        "thursday" | "thu" | "thur" | "thurs" => Weekday::Thu,
        "friday" | "fri" => Weekday::Fri,
        "saturday" | "sat" => Weekday::Sat,
        "sunday" | "sun" => Weekday::Sun,
        _ => return None,
    })
}

/// Abbreviations that are also ordinary words need a nudge (`on sat`).
fn ambiguous_weekday(s: &str) -> bool {
    matches!(s, "sat" | "sun" | "wed")
}

/// A part of the day, after a day that names it (`tomorrow morning`, `this
/// evening`): alone, "morning" is too often just a word.
fn part_of_day(s: &str) -> Option<NaiveTime> {
    let hour = match s.trim_end_matches(',') {
        "morning" => 9,
        "afternoon" => 14,
        "evening" => 18,
        "night" => 20,
        _ => return None,
    };
    NaiveTime::from_hms_opt(hour, 0, 0)
}

/// 8pm, or once that has gone by, the next hour (11:59pm at the latest).
fn tonight(now: DateTime<Tz>) -> NaiveTime {
    let eight = NaiveTime::from_hms_opt(20, 0, 0).expect("a time");
    match now.time().hour() + 1 {
        _ if now.time() < eight => eight,
        24 => NaiveTime::from_hms_opt(23, 59, 0).expect("a time"),
        next => NaiveTime::from_hms_opt(next, 0, 0).expect("a time"),
    }
}

fn month(s: &str) -> Option<u32> {
    let s = s.trim_end_matches([',', '.']);
    const NAMES: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    const FULL: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    (0..12)
        .find(|&m| s == NAMES[m] || s == FULL[m] || (m == 8 && s == "sept"))
        .map(|m| m as u32 + 1)
}

fn day_number(s: &str) -> Option<u32> {
    let s = s.trim_end_matches(',');
    let digits = s
        .trim_end_matches("st")
        .trim_end_matches("nd")
        .trim_end_matches("rd")
        .trim_end_matches("th");
    let n: u32 = digits.parse().ok()?;
    (1..=31).contains(&n).then_some(n)
}

fn next_weekday(from: NaiveDate, wd: Weekday, include_today: bool) -> NaiveDate {
    let ahead =
        (7 + wd.num_days_from_monday() as i64 - from.weekday().num_days_from_monday() as i64) % 7;
    let ahead = if ahead == 0 && !include_today {
        7
    } else {
        ahead
    };
    from + Duration::days(ahead)
}

/// A month/day, this year or, once passed, next year.
fn upcoming(today: NaiveDate, month: u32, day: u32, year: Option<i32>) -> Option<NaiveDate> {
    if let Some(y) = year {
        return NaiveDate::from_ymd_opt(y, month, day);
    }
    let this = NaiveDate::from_ymd_opt(today.year(), month, day)?;
    if this >= today {
        Some(this)
    } else {
        NaiveDate::from_ymd_opt(today.year() + 1, month, day)
    }
}

/// A time at `i`. With `needs_marker`, a bare hour (`5`) is not a time.
fn time_at(toks: &[Tok], i: usize, needs_marker: bool) -> Option<(usize, NaiveTime)> {
    let t = toks.get(i)?;
    match t.lower.as_str() {
        "noon" | "midday" => return Some((1, NaiveTime::from_hms_opt(12, 0, 0)?)),
        // "By midnight" means before the day is out, not the next one.
        "midnight" => return Some((1, NaiveTime::from_hms_opt(23, 59, 0)?)),
        _ => {}
    }
    let s = t.lower.trim_end_matches(',');
    let (body, mut meridiem) = if let Some(b) = s.strip_suffix("am").or_else(|| s.strip_suffix('a'))
    {
        (b, Some(false))
    } else if let Some(b) = s.strip_suffix("pm").or_else(|| s.strip_suffix('p')) {
        (b, Some(true))
    } else {
        (s, None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        Some(_) => return None,
        None => (body.parse::<u32>().ok()?, 0),
    };
    if body.is_empty() || !body.chars().next()?.is_ascii_digit() {
        return None;
    }
    let mut n = 1;
    if meridiem.is_none() {
        match toks.get(i + 1).map(|t| t.lower.trim_end_matches(',')) {
            Some("am") => (meridiem, n) = (Some(false), 2),
            Some("pm") => (meridiem, n) = (Some(true), 2),
            _ => {}
        }
    }
    let hour = match meridiem {
        Some(pm) if (1..=12).contains(&h) => (h % 12) + if pm { 12 } else { 0 },
        Some(_) => return None,
        None if body.contains(':') => h,
        // "at 5" is a time of day people mean in waking hours.
        None if !needs_marker && (1..=12).contains(&h) => {
            if h <= 7 {
                h + 12
            } else {
                h
            }
        }
        None => return None,
    };
    Some((n, NaiveTime::from_hms_opt(hour, m, 0)?))
}

/// A day at `i`: returns tokens used and the date.
fn day_at(toks: &[Tok], i: usize, today: NaiveDate) -> Option<(usize, NaiveDate)> {
    let t = &toks.get(i)?.lower;
    let next = toks.get(i + 1).map(|t| t.lower.as_str());
    match t.as_str() {
        "today" | "tod" | "tdy" => return Some((1, today)),
        "tomorrow" | "tmr" | "tmrw" | "tom" => return Some((1, today + Duration::days(1))),
        "weekend" => return Some((1, next_weekday(today, Weekday::Sat, true))),
        "next" => {
            if next == Some("week") {
                return Some((2, next_weekday(today, Weekday::Mon, false)));
            }
            if next == Some("month") {
                let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
                return Some((2, first.checked_add_months(chrono::Months::new(1))?));
            }
            let wd = weekday(next?)?;
            return Some((2, next_weekday(today, wd, false)));
        }
        "this" | "on" => {
            let wd = weekday(next?)?;
            return Some((2, next_weekday(today, wd, t == "this")));
        }
        "in" => {
            let n: i64 = match next? {
                "a" | "an" | "one" => 1,
                other => other.parse().ok()?,
            };
            let unit = toks.get(i + 2)?.lower.trim_end_matches(',').to_string();
            let date = match unit.as_str() {
                "day" | "days" | "d" => today + Duration::days(n),
                "week" | "weeks" | "w" => today + Duration::weeks(n),
                "month" | "months" => today.checked_add_months(chrono::Months::new(n as u32))?,
                "year" | "years" => today.with_year(today.year() + n as i32)?,
                _ => return None,
            };
            return Some((3, date));
        }
        _ => {}
    }
    if let Some(wd) = weekday(t) {
        if !ambiguous_weekday(t) {
            return Some((1, next_weekday(today, wd, false)));
        }
        return None;
    }
    // sep 20 [2027] / 20 sep [2027]
    let year_at = |j: usize| {
        toks.get(j)
            .and_then(|t| t.lower.trim_end_matches(',').parse::<i32>().ok())
            .filter(|y| (2000..2200).contains(y))
    };
    if let (Some(m), Some(d)) = (month(t), next.and_then(day_number)) {
        let y = year_at(i + 2);
        return Some((2 + usize::from(y.is_some()), upcoming(today, m, d, y)?));
    }
    if let (Some(d), Some(m)) = (day_number(t), next.and_then(month))
        && (t.chars().all(|c| c.is_ascii_digit()) || t.ends_with(['t', 'd', 'h']))
    {
        let y = year_at(i + 2);
        return Some((2 + usize::from(y.is_some()), upcoming(today, m, d, y)?));
    }
    // 2026-09-20
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Some((1, d));
    }
    // 9/20 or 9/20/2027 (month first)
    let parts: Vec<&str> = t.split('/').collect();
    if (2..=3).contains(&parts.len()) {
        let m: u32 = parts[0].parse().ok()?;
        let d: u32 = parts[1].parse().ok()?;
        let y = match parts.get(2) {
            Some(y) if y.len() == 2 => Some(2000 + y.parse::<i32>().ok()?),
            Some(y) => Some(y.parse::<i32>().ok()?),
            None => None,
        };
        return Some((1, upcoming(today, m, d, y)?));
    }
    None
}

fn date_phrase_at(toks: &[Tok], i: usize, now: DateTime<Tz>) -> Option<(usize, Hit)> {
    let today = now.date_naive();
    // "in 2h" / "in 30 minutes": an exact moment.
    if toks[i].lower == "in" {
        if let Some(d) = toks
            .get(i + 1)
            .and_then(|t| short_duration(&t.lower))
            .filter(|d| *d < Duration::days(1))
        {
            return Some((
                2,
                Hit::Date {
                    day: None,
                    time: None,
                    exact: Some(now + d),
                },
            ));
        }
        if let (Some(n), Some(unit)) = (
            toks.get(i + 1).and_then(|t| t.lower.parse::<i64>().ok()),
            toks.get(i + 2),
        ) {
            let d = match unit.lower.as_str() {
                "minute" | "minutes" | "min" | "mins" => Some(Duration::minutes(n)),
                "hour" | "hours" | "hr" | "hrs" => Some(Duration::hours(n)),
                _ => None,
            };
            if let Some(d) = d {
                return Some((
                    3,
                    Hit::Date {
                        day: None,
                        time: None,
                        exact: Some(now + d),
                    },
                ));
            }
        }
    }
    // tonight, this morning
    let part = |j: usize| toks.get(j).and_then(|t| part_of_day(&t.lower));
    match toks[i].lower.as_str() {
        "tonight" => {
            return Some((
                1,
                Hit::Date {
                    day: Some(today),
                    time: Some(tonight(now)),
                    exact: None,
                },
            ));
        }
        "this" if part(i + 1).is_some() => {
            return Some((
                2,
                Hit::Date {
                    day: Some(today),
                    time: part(i + 1),
                    exact: None,
                },
            ));
        }
        _ => {}
    }
    // DAY [at] [TIME], DAY morning
    if let Some((n, day)) = day_at(toks, i, today) {
        let j = i + n;
        if let Some(time) = part(j) {
            return Some((
                n + 1,
                Hit::Date {
                    day: Some(day),
                    time: Some(time),
                    exact: None,
                },
            ));
        }
        let at = usize::from(toks.get(j).is_some_and(|t| t.lower == "at"));
        if let Some((m, time)) = time_at(toks, j + at, at == 0) {
            return Some((
                n + at + m,
                Hit::Date {
                    day: Some(day),
                    time: Some(time),
                    exact: None,
                },
            ));
        }
        return Some((
            n,
            Hit::Date {
                day: Some(day),
                time: None,
                exact: None,
            },
        ));
    }
    // [at] TIME [DAY]
    let at = usize::from(toks[i].lower == "at");
    let (m, time) = time_at(toks, i + at, at == 0)?;
    if let Some((n, day)) = day_at(toks, i + at + m, today) {
        return Some((
            at + m + n,
            Hit::Date {
                day: Some(day),
                time: Some(time),
                exact: None,
            },
        ));
    }
    Some((
        at + m,
        Hit::Date {
            day: None,
            time: Some(time),
            exact: None,
        },
    ))
}

fn repeat_at(toks: &[Tok], i: usize, now: DateTime<Tz>) -> Option<(usize, Hit)> {
    let today = now.date_naive();
    let t = toks[i].lower.as_str();
    let simple = |freq: &str| (format!("FREQ={freq}"), today);
    let (mut n, (rule, first)) = match t {
        "daily" => (1, simple("DAILY")),
        "weekly" => (1, simple("WEEKLY")),
        "monthly" => (1, simple("MONTHLY")),
        "yearly" | "annually" => (1, simple("YEARLY")),
        "every" => {
            let next = toks.get(i + 1)?.lower.trim_end_matches(',').to_string();
            match next.as_str() {
                "day" => (2, simple("DAILY")),
                "week" => (2, simple("WEEKLY")),
                "month" => (2, simple("MONTHLY")),
                "year" => (2, simple("YEARLY")),
                "weekday" => {
                    let first = (0..7)
                        .map(|k| today + Duration::days(k))
                        .find(|d| d.weekday().num_days_from_monday() < 5)?;
                    (2, ("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_string(), first))
                }
                "weekend" => (
                    2,
                    (
                        "FREQ=WEEKLY;BYDAY=SA,SU".to_string(),
                        next_weekday(today, Weekday::Sat, true).min(next_weekday(
                            today,
                            Weekday::Sun,
                            true,
                        )),
                    ),
                ),
                other => {
                    if let Ok(k) = other.parse::<u32>() {
                        let unit = toks.get(i + 2)?.lower.trim_end_matches(',').to_string();
                        let freq = match unit.as_str() {
                            "days" | "day" => "DAILY",
                            "weeks" | "week" => "WEEKLY",
                            "months" | "month" => "MONTHLY",
                            "years" | "year" => "YEARLY",
                            _ => return None,
                        };
                        (3, (format!("FREQ={freq};INTERVAL={k}"), today))
                    } else if let Some(d) =
                        day_number(other).filter(|_| other.ends_with(['t', 'd', 'h']))
                    {
                        let first = (0..62)
                            .map(|k| today + Duration::days(k))
                            .find(|x| x.day() == d)?;
                        (2, (format!("FREQ=MONTHLY;BYMONTHDAY={d}"), first))
                    } else {
                        // every mon, wed and fri
                        let mut days = Vec::new();
                        let mut j = i + 1;
                        while let Some(tok) = toks.get(j) {
                            let w = tok.lower.trim_end_matches(',');
                            if let Some(wd) = weekday(w) {
                                if !days.contains(&wd) {
                                    days.push(wd);
                                }
                                j += 1;
                            } else if (w == "and" || w == "&")
                                && toks.get(j + 1).is_some_and(|t| weekday(&t.lower).is_some())
                            {
                                j += 1;
                            } else {
                                break;
                            }
                        }
                        if days.is_empty() {
                            return None;
                        }
                        days.sort_by_key(|d| d.num_days_from_monday());
                        let first = days.iter().map(|d| next_weekday(today, *d, true)).min()?;
                        let codes: Vec<&str> = days.iter().map(|d| weekday_code(*d)).collect();
                        (
                            j - i,
                            (format!("FREQ=WEEKLY;BYDAY={}", codes.join(",")), first),
                        )
                    }
                }
            }
        }
        _ => return None,
    };
    let at = usize::from(toks.get(i + n).is_some_and(|t| t.lower == "at"));
    let time = time_at(toks, i + n + at, at == 0).map(|(m, time)| {
        n += at + m;
        time
    });
    // Today's occurrence already went by: start from the next day that fits.
    let first = match time {
        Some(t) if first == today && t <= now.time() => {
            let rule_days = crate::recur::next(
                &rule,
                &When::Date { date: first },
                resolve(now.timezone(), first.and_time(NaiveTime::MIN)),
                now.timezone(),
            )
            .ok()
            .flatten()
            .map(|n| n.local_date(now.timezone()));
            rule_days.unwrap_or(first + Duration::days(1))
        }
        _ => first,
    };
    Some((n, Hit::Repeat { rule, first, time }))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Tz> {
        // Monday, September 14 2026, 10:00 in New York.
        let ny: Tz = "America/New_York".parse().unwrap();
        ny.with_ymd_and_hms(2026, 9, 14, 10, 0, 0).unwrap()
    }

    fn lists() -> Vec<String> {
        vec!["Inbox".into(), "Errands".into(), "Work stuff".into()]
    }

    fn date(m: u32, d: u32) -> When {
        When::Date {
            date: NaiveDate::from_ymd_opt(2026, m, d).unwrap(),
        }
    }

    fn at(m: u32, d: u32, h: u32, min: u32) -> When {
        When::local(
            NaiveDate::from_ymd_opt(2026, m, d)
                .unwrap()
                .and_hms_opt(h, min, 0)
                .unwrap(),
            now().timezone(),
        )
    }

    fn p(text: &str) -> Parsed {
        parse(text, now(), &lists())
    }

    #[test]
    fn the_full_example() {
        let r = p("Call the bank #errands p1 tomorrow 5pm !30m");
        assert_eq!(r.summary, "Call the bank");
        assert_eq!(r.list.as_deref(), Some("Errands"));
        assert_eq!(r.priority, Some(1));
        assert_eq!(r.due, Some(at(9, 15, 17, 0)));
        assert_eq!(
            r.alarm,
            Some(Trigger::Relative {
                offset: Duration::minutes(-30),
                from_due: false
            })
        );
        assert_eq!(r.spans.len(), 4);
        assert_eq!(
            &"Call the bank #errands p1 tomorrow 5pm !30m"[r.spans[2].0.clone()],
            "tomorrow 5pm"
        );
    }

    #[test]
    fn days() {
        assert_eq!(p("x today").due, Some(date(9, 14)));
        assert_eq!(p("x tmr").due, Some(date(9, 15)));
        assert_eq!(p("x friday").due, Some(date(9, 18)));
        assert_eq!(
            p("x monday").due,
            Some(date(9, 21)),
            "a bare weekday is never today"
        );
        assert_eq!(p("x this monday").due, Some(date(9, 14)));
        assert_eq!(p("x next week").due, Some(date(9, 21)));
        assert_eq!(p("x in 3 days").due, Some(date(9, 17)));
        assert_eq!(p("x sep 20").due, Some(date(9, 20)));
        assert_eq!(p("x 20th sep").due, Some(date(9, 20)));
        assert_eq!(
            p("x 9/1").due,
            Some(When::Date {
                date: NaiveDate::from_ymd_opt(2027, 9, 1).unwrap()
            }),
            "passed: next year"
        );
        assert_eq!(p("x 2026-12-25").due, Some(date(12, 25)));
    }

    #[test]
    fn times() {
        assert_eq!(p("x 5pm").due, Some(at(9, 14, 17, 0)));
        assert_eq!(
            p("x 9am").due,
            Some(at(9, 15, 9, 0)),
            "passed today: tomorrow"
        );
        assert_eq!(p("x at 5").due, Some(at(9, 14, 17, 0)));
        assert_eq!(p("x fri at 9:30").due, Some(at(9, 18, 9, 30)));
        assert_eq!(p("x 17:45 tomorrow").due, Some(at(9, 15, 17, 45)));
        assert_eq!(p("x noon").due, Some(at(9, 14, 12, 0)));
        assert_eq!(p("x 5 pm").due, Some(at(9, 14, 17, 0)));
        assert_eq!(p("x in 2h").due, Some(at(9, 14, 12, 0)));
    }

    #[test]
    fn parts_of_the_day() {
        assert_eq!(p("x tonight").due, Some(at(9, 14, 20, 0)));
        let late = now().with_hour(21).unwrap().with_minute(30).unwrap();
        assert_eq!(
            parse("x tonight", late, &[]).due,
            Some(at(9, 14, 22, 0)),
            "8pm went by: the next hour"
        );
        assert_eq!(p("x this evening").due, Some(at(9, 14, 18, 0)));
        assert_eq!(p("x tomorrow morning").due, Some(at(9, 15, 9, 0)));
        assert_eq!(p("x fri afternoon").due, Some(at(9, 18, 14, 0)));
        assert_eq!(p("x tomorrow night").due, Some(at(9, 15, 20, 0)));
        assert_eq!(p("Pay by midnight").due, Some(at(9, 14, 23, 59)));
        let r = p("Morning run");
        assert_eq!((r.summary.as_str(), r.due), ("Morning run", None));
        assert_eq!(p("Good night").due, None);
    }

    #[test]
    fn dates_can_stay_in_the_title() {
        let r = parse_with(
            "Watch Friday Night Lights p2 #errands",
            now(),
            &lists(),
            false,
        );
        assert_eq!(r.summary, "Watch Friday Night Lights");
        assert_eq!((r.priority, r.due), (Some(2), None));
        assert_eq!(r.list.as_deref(), Some("Errands"));
        let r = parse_with("Stretch every day", now(), &lists(), false);
        assert_eq!((r.summary.as_str(), r.rrule), ("Stretch every day", None));
    }

    #[test]
    fn repeats() {
        let r = p("Stretch every day at 8am");
        assert_eq!(
            (r.summary.as_str(), r.rrule.as_deref()),
            ("Stretch", Some("FREQ=DAILY"))
        );
        assert_eq!(r.due, Some(at(9, 15, 8, 0)), "8am today already passed");
        assert_eq!(
            p("x every weekday").rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")
        );
        let r = p("Trash every thu and mon");
        assert_eq!(r.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,TH"));
        assert_eq!(r.due, Some(date(9, 14)));
        assert_eq!(
            p("x every 2 weeks").rrule.as_deref(),
            Some("FREQ=WEEKLY;INTERVAL=2")
        );
        let r = p("Rent every 1st");
        assert_eq!(
            (r.rrule.as_deref(), r.due),
            (Some("FREQ=MONTHLY;BYMONTHDAY=1"), Some(date(10, 1)))
        );
        assert_eq!(p("Review monthly").rrule.as_deref(), Some("FREQ=MONTHLY"));
    }

    #[test]
    fn alarms() {
        let r = p("x tomorrow 5pm !");
        assert_eq!(
            r.alarm,
            Some(Trigger::Absolute {
                at: at(9, 15, 17, 0).instant(now().timezone())
            })
        );
        let r = p("x tomorrow !9am");
        assert_eq!(
            r.alarm,
            Some(Trigger::Absolute {
                at: at(9, 15, 9, 0).instant(now().timezone())
            })
        );
        assert_eq!(p("x !30m").alarm, None, "nothing to be 30 minutes before");
        assert_eq!(p("Wow!").summary, "Wow!");
    }

    #[test]
    fn leaves_ordinary_words_alone() {
        assert_eq!(p("Buy sun screen").summary, "Buy sun screen");
        assert_eq!(p("Sat on the report").due, None);
        assert_eq!(p("Read chapter 5").summary, "Read chapter 5");
        assert_eq!(p("Email #nosuchlist").summary, "Email #nosuchlist");
        assert_eq!(p("Plan may trip").due, None);
        assert_eq!(p("on sat buy paint").due, Some(date(9, 19)));
        assert_eq!(
            p("from monday to tuesday").summary,
            "from to tuesday",
            "first date wins"
        );
        assert_eq!(p("#work Fix the bug").list.as_deref(), Some("Work stuff"));
    }

    #[test]
    fn whole_string_dates() {
        assert_eq!(parse_when("tomorrow 5pm", now()), Some(at(9, 15, 17, 0)));
        assert_eq!(parse_when("none", now()), None);
        assert_eq!(parse_when("tomorrow and more", now()), None);
    }
}
