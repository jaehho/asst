//! Recurrence: where a repeating task goes when it is completed.

use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rrule::{RRule, Unvalidated};

use crate::time::{When, resolve, resolve_tzid};

pub fn validate(rule: &str) -> Result<(), String> {
    rule.trim()
        .parse::<RRule<Unvalidated>>()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The first occurrence strictly after `after`, for a series anchored at
/// `anchor`, in the same form as the anchor.
pub fn next(
    rule: &str,
    anchor: &When,
    after: DateTime<Utc>,
    local: Tz,
) -> Result<Option<When>, String> {
    let zone = match anchor {
        When::Utc { .. } => Tz::UTC,
        When::Zoned { tzid, .. } => resolve_tzid(tzid).unwrap_or(local),
        _ => local,
    };
    let start = match anchor {
        When::Date { date } => resolve(zone, date.and_time(NaiveTime::MIN)),
        other => other.instant(local),
    };
    let start = rrule::Tz::Tz(zone).from_utc_datetime(&start.naive_utc());
    let parsed: RRule<Unvalidated> = normalize_until(rule, anchor)
        .parse()
        .map_err(|e: rrule::RRuleError| e.to_string())?;
    let set = parsed.build(start).map_err(|e| e.to_string())?;

    Ok(set
        .into_iter()
        .take(100_000)
        .map(|occurrence| occurrence.with_timezone(&Utc))
        .find(|at| *at > after)
        .map(|at| anchor.with_instant(at, zone)))
}

/// The rule with `n` fewer occurrences left, when it counts them.
pub fn consume_count(rule: &str, n: u32) -> Option<String> {
    let mut changed = false;
    let parts: Vec<String> = rule
        .trim()
        .split(';')
        .map(|part| match part.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("COUNT") => {
                changed = true;
                let left = v
                    .trim()
                    .parse::<u32>()
                    .unwrap_or(1)
                    .saturating_sub(n)
                    .max(1);
                format!("{k}={left}")
            }
            _ => part.to_string(),
        })
        .collect();
    changed.then(|| parts.join(";"))
}

/// The rrule crate insists UNTIL match DTSTART's form (UTC for a zoned
/// start, a date for a date); clients are looser, so convert rather than fail.
fn normalize_until(rule: &str, anchor: &When) -> String {
    rule.trim()
        .split(';')
        .map(|part| match part.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("UNTIL") => {
                let v = v.trim();
                let fixed = match anchor {
                    When::Date { .. } => v.get(..8).map(str::to_string),
                    _ if v.len() == 8 => Some(format!("{v}T235959Z")),
                    When::Floating { .. } => Some(v.trim_end_matches('Z').to_string()),
                    _ if !v.ends_with('Z') => Some(format!("{v}Z")),
                    _ => None,
                };
                format!("{k}={}", fixed.unwrap_or_else(|| v.to_string()))
            }
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn zoned(y: i32, m: u32, d: u32, h: u32) -> When {
        When::Zoned {
            at: NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(h, 0, 0)
                .unwrap(),
            tzid: "America/New_York".into(),
        }
    }

    #[test]
    fn keeps_wall_time_across_dst() {
        let anchor = zoned(2026, 10, 31, 9);
        let after = anchor.instant(ny());
        let n = next("FREQ=DAILY", &anchor, after, ny()).unwrap().unwrap();
        assert_eq!(n, zoned(2026, 11, 1, 9));
    }

    #[test]
    fn monthly_by_setpos() {
        // Last weekday of the month.
        let anchor = zoned(2026, 9, 30, 9);
        let n = next(
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
            &anchor,
            anchor.instant(ny()),
            ny(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(n, zoned(2026, 10, 30, 9));
    }

    #[test]
    fn until_in_a_date_form_is_accepted() {
        let anchor = zoned(2026, 9, 14, 9);
        let n = next(
            "FREQ=DAILY;UNTIL=20260915",
            &anchor,
            anchor.instant(ny()),
            ny(),
        )
        .unwrap();
        assert_eq!(n.unwrap(), zoned(2026, 9, 15, 9));
        let over = next(
            "FREQ=DAILY;UNTIL=20260914",
            &anchor,
            anchor.instant(ny()),
            ny(),
        )
        .unwrap();
        assert!(over.is_none());
    }

    #[test]
    fn counts() {
        assert_eq!(
            consume_count("FREQ=DAILY;COUNT=5", 2).as_deref(),
            Some("FREQ=DAILY;COUNT=3")
        );
        assert_eq!(consume_count("FREQ=DAILY", 2), None);
        let anchor = zoned(2026, 9, 14, 9);
        assert!(
            next("FREQ=DAILY;COUNT=1", &anchor, anchor.instant(ny()), ny())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(validate("FREQ=SOMETIMES").is_err());
        assert!(validate("FREQ=WEEKLY;BYDAY=MO").is_ok());
    }
}
