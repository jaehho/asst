//! Due dates in words, the same in the CLI, the bar and notifications.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Timelike};
use chrono_tz::Tz;

use crate::time::When;

pub fn day_label(d: NaiveDate, today: NaiveDate) -> String {
    let days = (d - today).num_days();
    match days {
        0 => "today".into(),
        1 => "tomorrow".into(),
        -1 => "yesterday".into(),
        2..=6 => d.format("%a").to_string(),
        _ if d.year() == today.year() => d.format("%b %-d").to_string(),
        _ => d.format("%b %-d %Y").to_string(),
    }
}

pub fn time_label(t: NaiveTime) -> String {
    let (pm, h) = t.hour12();
    let suffix = if pm { "pm" } else { "am" };
    match t.minute() {
        0 => format!("{h}{suffix}"),
        m => format!("{h}:{m:02}{suffix}"),
    }
}

pub fn due_label(w: &When, now: DateTime<Tz>) -> String {
    let zone = now.timezone();
    let today = now.date_naive();
    match w {
        When::Date { date } => day_label(*date, today),
        other => {
            let local = other.instant(zone).with_timezone(&zone);
            format!(
                "{} {}",
                day_label(local.date_naive(), today),
                time_label(local.time())
            )
        }
    }
}

/// Past due: a date before today, or a time already gone.
pub fn is_overdue(w: &When, now: DateTime<Tz>) -> bool {
    match w {
        When::Date { date } => *date < now.date_naive(),
        other => other.instant(now.timezone()) < now,
    }
}

pub fn is_today(w: &When, now: DateTime<Tz>) -> bool {
    w.local_date(now.timezone()) == now.date_naive()
}

/// `10 min`, `1 h`, `1 h 30 min`, `1 day`, `2 days`.
pub fn minutes(m: u64) -> String {
    match m {
        m if m >= 1440 && m % 1440 == 0 => match m / 1440 {
            1 => "1 day".into(),
            d => format!("{d} days"),
        },
        m if m >= 60 && m % 60 == 0 => format!("{} h", m / 60),
        m if m > 60 => format!("{} h {} min", m / 60, m % 60),
        m => format!("{m} min"),
    }
}

/// `in 25 min`, `in 3 h`, for a notification body.
pub fn until_label(at: DateTime<Tz>, now: DateTime<Tz>) -> Option<String> {
    let d = at - now;
    (d > Duration::zero() && d < Duration::hours(12)).then(|| {
        if d < Duration::hours(1) {
            format!("in {} min", d.num_minutes().max(1))
        } else {
            format!("in {} h", (d.num_minutes() + 30) / 60)
        }
    })
}

/// `daily`, `every 2 weeks`, `every Mon, Thu`, `monthly on the 15th`; the
/// rule itself when it says something more unusual.
pub fn repeat_label(rrule: &str) -> String {
    let mut freq = "";
    let (mut interval, mut byday, mut bymonthday, mut count, mut until) =
        (1u32, None, None, None, None);
    let mut other = false;
    for part in rrule.trim().split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => freq = v,
            "INTERVAL" => interval = v.parse().unwrap_or(1),
            "BYDAY" => byday = Some(v),
            "BYMONTHDAY" => bymonthday = Some(v),
            "COUNT" => count = Some(v),
            "UNTIL" => until = crate::time::parse_date(v),
            "WKST" => {}
            _ => other = true,
        }
    }
    let unit = |one: &str, many: &str| {
        if interval == 1 {
            one.to_string()
        } else {
            format!("every {interval} {many}")
        }
    };
    let day_name = |code: &str| match code {
        "MO" => Some("Mon"),
        "TU" => Some("Tue"),
        "WE" => Some("Wed"),
        "TH" => Some("Thu"),
        "FR" => Some("Fri"),
        "SA" => Some("Sat"),
        "SU" => Some("Sun"),
        _ => None,
    };
    let mut label = match (freq.to_ascii_uppercase().as_str(), byday, bymonthday) {
        ("DAILY", None, None) => unit("daily", "days"),
        ("WEEKLY", None, None) => unit("weekly", "weeks"),
        ("WEEKLY", Some("MO,TU,WE,TH,FR"), None) if interval == 1 => "weekdays".into(),
        ("WEEKLY", Some("SA,SU"), None) if interval == 1 => "weekends".into(),
        ("WEEKLY", Some(days), None) => {
            let names: Option<Vec<&str>> = days.split(',').map(day_name).collect();
            match names {
                Some(n) if interval == 1 => format!("every {}", n.join(", ")),
                Some(n) => format!("every {interval} weeks on {}", n.join(", ")),
                None => return rrule.to_string(),
            }
        }
        ("MONTHLY", None, None) => unit("monthly", "months"),
        ("MONTHLY", None, Some(d)) => match monthday_words(d) {
            Some(w) => format!("{} on the {}", unit("monthly", "months"), w),
            None => return rrule.to_string(),
        },
        ("MONTHLY", Some(days), None) => match nth_weekday_words(days, &day_name) {
            Some(w) => format!("{} on the {}", unit("monthly", "months"), w),
            None => return rrule.to_string(),
        },
        ("YEARLY", None, None) => unit("yearly", "years"),
        _ => return rrule.to_string(),
    };
    if other {
        return rrule.to_string();
    }
    if let Some(c) = count {
        label.push_str(&format!(", {c} times"));
    }
    if let Some(u) = until {
        label.push_str(&format!(" until {}", u.format("%b %-d %Y")));
    }
    label
}

/// Words the quick-add reader turns back into this rule, for an edit field;
/// the rule itself when there are none.
pub fn repeat_phrase(rrule: &str) -> String {
    let label = repeat_label(rrule);

    match label.as_str() {
        "daily" => "every day".to_string(),
        "weekly" => "every week".to_string(),
        "monthly" => "every month".to_string(),
        "yearly" => "every year".to_string(),
        "weekdays" => "every weekday".to_string(),
        "weekends" => "every weekend".to_string(),
        l if l.contains("times")
            || l.contains("until")
            || l.contains(" on ") && !l.starts_with("monthly on the ") =>
        {
            rrule.to_string()
        }
        l if l.starts_with("monthly on the ") => {
            let rest = &l["monthly on the ".len()..];
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            // quick-add reads back only a plain day of month: "every 15th"
            if digits > 0
                && rest[digits..].chars().all(|c| c.is_ascii_alphabetic())
                && rest[digits..].len() <= 2
            {
                format!("every {rest}")
            } else {
                rrule.to_string()
            }
        }
        l if l.starts_with("every ") => l.to_lowercase(),
        _ => rrule.to_string(),
    }
}

fn ordinal_suffix(n: u32) -> &'static str {
    match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

/// "a", "a and b", "a, b and c".
fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => {
            let (head, last) = parts.split_at(parts.len() - 1);
            format!("{} and {}", head.join(", "), last[0])
        }
    }
}

/// `1,15,-1` → "1st, 15th and last day"; None when a part isn't a plain day.
fn monthday_words(d: &str) -> Option<String> {
    let items: Option<Vec<String>> = d
        .split(',')
        .map(|s| match s.trim().parse::<i32>() {
            Ok(-1) => Some("last day".to_string()),
            Ok(n) if (1..=31).contains(&n) => Some(format!("{n}{}", ordinal_suffix(n as u32))),
            _ => None,
        })
        .collect();
    Some(join_and(&items?))
}

/// `-1FR,2MO` → "last Friday and second Monday"; None unless every day
/// carries an ordinal in words and they all share it.
fn nth_weekday_words(d: &str, day_name: &impl Fn(&str) -> Option<&'static str>) -> Option<String> {
    let items: Option<Vec<(&'static str, &str)>> = d
        .split(',')
        .map(|item| {
            let item = item.trim();
            let at = item.find(|c: char| c.is_ascii_alphabetic())?;
            let (n, code) = item.split_at(at);
            let word = match n.parse::<i32>().ok()? {
                -1 => "last",
                1 => "first",
                2 => "second",
                3 => "third",
                4 => "fourth",
                5 => "fifth",
                _ => return None,
            };
            Some((word, day_name(code)?))
        })
        .collect();
    let list = items.filter(|l| !l.is_empty() && l.iter().all(|(w, _)| *w == l[0].0))?;
    let names: Vec<String> = list.iter().map(|(_, d)| d.to_string()).collect();
    Some(format!("{} {}", list[0].0, join_and(&names)))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn labels() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let now = ny.with_ymd_and_hms(2026, 9, 14, 10, 0, 0).unwrap();
        let day = |m, d| When::Date {
            date: NaiveDate::from_ymd_opt(2026, m, d).unwrap(),
        };
        assert_eq!(due_label(&day(9, 14), now), "today");
        assert_eq!(due_label(&day(9, 18), now), "Fri");
        assert_eq!(due_label(&day(9, 30), now), "Sep 30");
        let at = When::local(
            NaiveDate::from_ymd_opt(2026, 9, 15)
                .unwrap()
                .and_hms_opt(17, 30, 0)
                .unwrap(),
            ny,
        );
        assert_eq!(due_label(&at, now), "tomorrow 5:30pm");
        assert!(is_overdue(&day(9, 13), now));
        assert!(!is_overdue(&day(9, 14), now));
        assert_eq!(
            until_label(now + Duration::minutes(25), now).as_deref(),
            Some("in 25 min")
        );
        let words: Vec<String> = [10, 60, 90, 1440, 2880].map(minutes).into();
        assert_eq!(words, ["10 min", "1 h", "1 h 30 min", "1 day", "2 days"]);
    }

    #[test]
    fn repeats() {
        assert_eq!(repeat_label("FREQ=DAILY"), "daily");
        assert_eq!(repeat_label("FREQ=WEEKLY;INTERVAL=2"), "every 2 weeks");
        assert_eq!(repeat_label("FREQ=WEEKLY;BYDAY=MO,TH"), "every Mon, Thu");
        assert_eq!(repeat_label("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"), "weekdays");
        assert_eq!(
            repeat_label("FREQ=MONTHLY;BYMONTHDAY=22"),
            "monthly on the 22nd"
        );
        assert_eq!(repeat_label("FREQ=DAILY;COUNT=5"), "daily, 5 times");
        assert_eq!(
            repeat_label("FREQ=MONTHLY;BYDAY=-1FR"),
            "monthly on the last Fri"
        );
        assert_eq!(
            repeat_label("FREQ=MONTHLY;BYMONTHDAY=1,15"),
            "monthly on the 1st and 15th"
        );
        assert_eq!(
            repeat_label("FREQ=MONTHLY;BYDAY=-1FR,2MO"),
            "FREQ=MONTHLY;BYDAY=-1FR,2MO"
        );
        assert_eq!(
            repeat_label("FREQ=MONTHLY;BYDAY=FR"),
            "FREQ=MONTHLY;BYDAY=FR"
        );
    }

    #[test]
    fn repeat_phrases_read_back() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let now = ny.with_ymd_and_hms(2026, 9, 14, 10, 0, 0).unwrap();
        for rule in [
            "FREQ=DAILY",
            "FREQ=WEEKLY",
            "FREQ=WEEKLY;INTERVAL=2",
            "FREQ=WEEKLY;BYDAY=MO,TH",
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
            "FREQ=MONTHLY;BYMONTHDAY=22",
            "FREQ=YEARLY",
        ] {
            let phrase = repeat_phrase(rule);
            let back = crate::quickadd::parse(&format!("x {phrase}"), now, &[]).rrule;
            assert_eq!(back.as_deref(), Some(rule), "{rule} -> {phrase}");
        }
        assert_eq!(repeat_phrase("FREQ=DAILY;COUNT=3"), "FREQ=DAILY;COUNT=3");
        // no words quick-add could read back: keep the rule
        assert_eq!(
            repeat_phrase("FREQ=MONTHLY;BYDAY=-1FR"),
            "FREQ=MONTHLY;BYDAY=-1FR"
        );
        assert_eq!(
            repeat_phrase("FREQ=MONTHLY;BYMONTHDAY=1,15"),
            "FREQ=MONTHLY;BYMONTHDAY=1,15"
        );
    }
}
