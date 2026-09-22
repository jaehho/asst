//! A task as asst sees it, read from a VTODO, and the edits asst makes to
//! one. Edits patch the stored object in place (see `ical`): a property asst
//! does not own is never touched.

use chrono::{DateTime, Duration, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::ical::{Component, Ical, Property};
use crate::recur;
use crate::time::{Trigger, When, format_utc, parse_datetime, resolve_tzid, vtimezone};

pub const PRODID: &str = "-//jaehho//asst//EN";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    NeedsAction,
    InProcess,
    Completed,
    Cancelled,
}

impl Status {
    pub fn is_open(self) -> bool {
        matches!(self, Status::NeedsAction | Status::InProcess)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::NeedsAction => "needs-action",
            Status::InProcess => "in-process",
            Status::Completed => "completed",
            Status::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<DateTime<Utc>>,
}

/// A reminder that fires on arrival at a place, the way iOS writes it: a
/// VALARM with `X-APPLE-PROXIMITY` and an `X-APPLE-STRUCTURED-LOCATION`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationAlarm {
    /// The VALARM's UID, the key the reminder state is kept under.
    pub uid: String,
    /// The place's name, from `X-TITLE`.
    pub title: String,
    /// The address as written, from `X-ADDRESS` (often the same as the title).
    pub address: String,
    pub lat: f64,
    pub lon: f64,
    /// How close counts as arrived, in metres (Apple's default: 100).
    pub radius: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub uid: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<DateTime<Utc>>,
    /// iCalendar PRIORITY: 0 none, 1 highest … 9 lowest.
    pub priority: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<When>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alarms: Vec<Alarm>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub location_alarms: Vec<LocationAlarm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Where the task came from (`steno:<session>/<key>`), asst's own X- property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime<Utc>>,
}

pub const SOURCE: &str = "X-ASST-SOURCE";
/// The manual order iOS keeps within a list.
const SORT_ORDER: &str = "X-APPLE-SORT-ORDER";
/// A location alarm's place, on a VALARM.
const STRUCTURED_LOCATION: &str = "X-APPLE-STRUCTURED-LOCATION";

/// 1976-04-01T00:55:45Z, Apple's founding: iOS's placeholder trigger.
const IOS_NO_ALARM: DateTime<Utc> = match DateTime::from_timestamp(197_168_145, 0) {
    Some(t) => t,
    None => panic!("valid timestamp"),
};

/// Priority as the UI shows it: p1 (high) … p4 (none). iOS writes 1/5/9.
pub fn priority_level(priority: u8) -> u8 {
    match priority {
        1..=4 => 1,
        5 => 2,
        6..=9 => 3,
        _ => 4,
    }
}

pub fn priority_from_level(level: u8) -> u8 {
    match level {
        1 => 1,
        2 => 5,
        3 => 9,
        _ => 0,
    }
}

impl Task {
    pub fn from_ical(ical: &Ical) -> Option<Task> {
        let todo = ical.todo()?;
        let text = |name| todo.property(name).map(Property::text_value);
        let instant = |name| {
            todo.property(name)
                .and_then(|p| parse_datetime(p.value().trim()))
                .map(|(at, _)| at.and_utc())
        };
        let completed = instant("COMPLETED");
        let status = match todo
            .property("STATUS")
            .map(|p| p.value().trim().to_ascii_uppercase())
        {
            Some(s) if s == "COMPLETED" => Status::Completed,
            Some(s) if s == "CANCELLED" => Status::Cancelled,
            Some(s) if s == "IN-PROCESS" => Status::InProcess,
            Some(_) => Status::NeedsAction,
            None if completed.is_some() => Status::Completed,
            None => Status::NeedsAction,
        };
        let parent = todo
            .properties_named("RELATED-TO")
            .find(|p| {
                p.param("RELTYPE")
                    .is_none_or(|r| r.eq_ignore_ascii_case("PARENT"))
            })
            .map(|p| p.value().trim().to_string())
            .filter(|v| !v.is_empty());
        Some(Task {
            uid: todo.property("UID")?.value().trim().to_string(),
            summary: text("SUMMARY").unwrap_or_default(),
            description: text("DESCRIPTION").filter(|d| !d.is_empty()),
            status,
            completed,
            priority: todo
                .property("PRIORITY")
                .and_then(|p| p.value().trim().parse::<u8>().ok())
                .filter(|p| *p <= 9)
                .unwrap_or(0),
            due: todo.property("DUE").and_then(When::from_property),
            rrule: todo.property("RRULE").map(|p| p.value().trim().to_string()),
            alarms: todo
                .components()
                .filter(|c| c.is("VALARM"))
                .filter_map(|a| {
                    Some(Alarm {
                        trigger: timed_trigger(a)?,
                        acknowledged: a
                            .property("ACKNOWLEDGED")
                            .and_then(|p| parse_datetime(p.value().trim()))
                            .map(|(at, _)| at.and_utc()),
                    })
                })
                .collect(),
            location_alarms: todo
                .components()
                .filter(|c| c.is("VALARM"))
                .filter_map(location_alarm)
                .collect(),
            parent,
            source: text(SOURCE),
            sort_order: todo
                .property(SORT_ORDER)
                .and_then(|p| p.value().trim().parse().ok()),
            created: instant("CREATED"),
            modified: instant("LAST-MODIFIED"),
        })
    }

    pub fn is_open(&self) -> bool {
        self.status.is_open()
    }

    /// When each alarm goes off. A relative alarm needs the date it hangs on;
    /// with neither DTSTART nor DUE it never fires.
    pub fn alarm_instants(&self, local: Tz) -> Vec<DateTime<Utc>> {
        self.alarms
            .iter()
            .filter_map(|a| self.alarm_instant(a, local))
            .collect()
    }

    /// Alarms still to be seen: not acknowledged (on any device) since they fired.
    pub fn unacknowledged_alarms(&self, local: Tz) -> Vec<DateTime<Utc>> {
        self.alarms
            .iter()
            .filter_map(|a| {
                self.alarm_instant(a, local)
                    .filter(|at| a.acknowledged.is_none_or(|ack| ack < *at))
            })
            .collect()
    }

    fn alarm_instant(&self, a: &Alarm, local: Tz) -> Option<DateTime<Utc>> {
        trigger_instant(&a.trigger, self.due.as_ref(), local)
    }
}

/// A VALARM's trigger, when it goes off at a time: location alarms fire on
/// arrival instead, and iOS parks a relative alarm on a dateless reminder at
/// `IOS_NO_ALARM`, meaning "no alarm".
fn timed_trigger(alarm: &Component) -> Option<Trigger> {
    if alarm.property("X-APPLE-PROXIMITY").is_some() {
        return None;
    }
    Trigger::from_property(alarm.property("TRIGGER")?)
        .filter(|t| *t != (Trigger::Absolute { at: IOS_NO_ALARM }))
}

/// A VALARM's place: `X-APPLE-PROXIMITY:ARRIVE` and an
/// `X-APPLE-STRUCTURED-LOCATION` holding `geo:lat,lon?u=radius`.
fn location_alarm(alarm: &Component) -> Option<LocationAlarm> {
    if !alarm
        .property("X-APPLE-PROXIMITY")?
        .value()
        .trim()
        .eq_ignore_ascii_case("ARRIVE")
    {
        return None;
    }
    let loc = alarm.property(STRUCTURED_LOCATION)?;
    let (lat, lon, radius) = {
        let geo = loc.value().trim().strip_prefix("geo:")?;
        let (xy, query) = geo.split_once('?').unwrap_or((geo, ""));
        let (lat, lon) = xy.split_once(',')?;
        let radius = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("u="))
            .and_then(|u| u.trim_end_matches('m').parse::<u32>().ok())
            .unwrap_or(100);
        (
            lat.trim().parse().ok()?,
            lon.trim().parse().ok()?,
            radius,
        )
    };
    Some(LocationAlarm {
        uid: alarm
            .property("UID")
            .map(|p| p.value().trim().to_string())
            .unwrap_or_default(),
        title: loc.param("X-TITLE").unwrap_or_default().to_string(),
        address: loc.param("X-ADDRESS").unwrap_or_default().to_string(),
        lat,
        lon,
        radius,
    })
}

/// A location alarm as a VALARM iOS reads back: it matches on the UID.
fn location_valarm(a: &LocationAlarm) -> Component {
    let uid = if a.uid.is_empty() {
        uuid::Uuid::new_v4().to_string().to_uppercase()
    } else {
        a.uid.clone()
    };
    let mut alarm = Component::new("VALARM");
    alarm.set(Property::new("ACTION", "DISPLAY"));
    alarm.set(Property::text(
        "DESCRIPTION",
        if a.title.is_empty() {
            "Reminder"
        } else {
            &a.title
        },
    ));
    alarm.set(Trigger::Absolute { at: IOS_NO_ALARM }.to_property());
    alarm.set(Property::new("UID", uid.as_str()));
    alarm.set(Property::new("X-WR-ALARMUID", uid.as_str()));
    alarm.set(Property::new("X-APPLE-PROXIMITY", "ARRIVE"));
    let loc = Property::new(
        STRUCTURED_LOCATION,
        format!("geo:{},{}?u={}", a.lat, a.lon, a.radius),
    )
    .with_param("VALUE", "URI")
    .with_param("X-ADDRESS", if a.address.is_empty() { &a.title } else { &a.address })
    .with_param("X-TITLE", if a.title.is_empty() { "Location" } else { &a.title });
    alarm.set(loc);
    alarm
}

/// When a trigger goes off, given the date a relative one hangs on.
fn trigger_instant(trigger: &Trigger, due: Option<&When>, local: Tz) -> Option<DateTime<Utc>> {
    match trigger {
        Trigger::Absolute { at } => Some(*at),
        Trigger::Relative { offset, .. } => due.map(|w| alarm_base(w, local) + *offset),
    }
}

/// A date-only task's relative alarm counts from 9:00 that day, the hour iOS
/// uses for all-day reminders.
fn alarm_base(w: &When, local: Tz) -> DateTime<Utc> {
    match w {
        When::Date { date } => crate::time::resolve(
            local,
            date.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
        ),
        other => other.instant(local),
    }
}

/// One change asst makes to a task. Edits are stored while offline and
/// re-applied to whatever the server holds when they are finally sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", content = "value", rename_all = "kebab-case")]
pub enum Edit {
    Summary(String),
    Description(Option<String>),
    Due(Option<When>),
    Priority(u8),
    /// Complete it; a recurring task moves to its next occurrence instead.
    Complete(DateTime<Utc>),
    Reopen,
    Rrule(Option<String>),
    /// Exactly these alarms: matching VALARMs are kept as they are.
    Alarms(Vec<Trigger>),
    /// Exactly these location alarms, in this order.
    LocationAlarms(Vec<LocationAlarm>),
    Parent(Option<String>),
    Source(Option<String>),
    /// X-APPLE-SORT-ORDER: the manual order within a list, ascending.
    SortOrder(Option<i64>),
}

/// A new, empty task object.
pub fn new_ical(uid: &str, now: DateTime<Utc>) -> Ical {
    let mut cal = Component::new("VCALENDAR");
    cal.set(Property::new("VERSION", "2.0"));
    cal.set(Property::new("PRODID", PRODID));
    let mut todo = Component::new("VTODO");
    todo.set(Property::new("UID", uid));
    todo.set(Property::new("CREATED", format_utc(&now)));
    todo.set(Property::new("DTSTAMP", format_utc(&now)));
    todo.set(Property::new("LAST-MODIFIED", format_utc(&now)));
    todo.set(Property::new("SUMMARY", ""));
    todo.set(Property::new("STATUS", "NEEDS-ACTION"));
    cal.push(todo);
    Ical::new(cal)
}

/// A copy to add as a new task: every property kept, with a new identity.
/// The source link stays with the original, since `add --source` finds a
/// task by it; alarms get UIDs of their own, since iOS matches alarms by UID.
pub fn duplicate(src: &Ical, now: DateTime<Utc>) -> Option<Ical> {
    let mut copy = src.clone();
    let todo = copy.todo_mut()?;
    todo.set(Property::new("UID", uuid::Uuid::new_v4().to_string()));
    for name in ["CREATED", "DTSTAMP", "LAST-MODIFIED"] {
        todo.set(Property::new(name, format_utc(&now)));
    }
    if todo.property("SEQUENCE").is_some() {
        todo.set(Property::new("SEQUENCE", "0"));
    }
    todo.remove(SOURCE);
    for alarm in todo.components_mut().filter(|c| c.is("VALARM")) {
        let uid = uuid::Uuid::new_v4().to_string().to_uppercase();
        for name in ["UID", "X-WR-ALARMUID"] {
            if alarm.property(name).is_some() {
                alarm.set(Property::new(name, uid.as_str()));
            }
        }
    }
    Some(copy)
}

/// What iOS leaves behind when a repeating reminder is completed: this
/// occurrence, done, as its own task with a new UID and no RRULE. `before` is
/// the task as it was before completing.
pub fn completed_copy(before: &Ical, at: DateTime<Utc>) -> Option<Ical> {
    let mut copy = before.clone();
    let todo = copy.todo_mut()?;
    todo.property("RRULE")?;
    for name in ["RRULE", "RDATE", "EXDATE", SOURCE, "PERCENT-COMPLETE"] {
        todo.remove(name);
    }
    todo.retain_components(|c| !c.is("VALARM"));
    todo.set(Property::new("UID", uuid::Uuid::new_v4().to_string()));
    for name in ["CREATED", "DTSTAMP", "LAST-MODIFIED"] {
        todo.set(Property::new(name, format_utc(&at)));
    }
    todo.set(Property::new("STATUS", "COMPLETED"));
    todo.set(Property::new("COMPLETED", format_utc(&at)));
    todo.set(Property::new("PERCENT-COMPLETE", "100"));
    Some(copy)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditError {
    #[error("the object has no VTODO")]
    NoTodo,
    #[error("invalid recurrence rule: {0}")]
    BadRrule(String),
}

/// Apply edits in order. `now` stamps LAST-MODIFIED; `local` reads floating
/// and date-only values.
pub fn apply(
    ical: &mut Ical,
    edits: &[Edit],
    now: DateTime<Utc>,
    local: Tz,
) -> Result<(), EditError> {
    for edit in edits {
        apply_one(ical, edit, local)?;
    }
    if !edits.is_empty() {
        let todo = ical.todo_mut().ok_or(EditError::NoTodo)?;
        todo.set(Property::new("LAST-MODIFIED", format_utc(&now)));
        todo.set(Property::new("DTSTAMP", format_utc(&now)));
    }
    Ok(())
}

fn apply_one(ical: &mut Ical, edit: &Edit, local: Tz) -> Result<(), EditError> {
    if let Edit::Due(Some(w)) = edit {
        ensure_vtimezone(ical, w);
    }
    let todo = ical.todo_mut().ok_or(EditError::NoTodo)?;
    match edit {
        Edit::Summary(s) => todo.set(Property::text("SUMMARY", s)),
        Edit::Description(d) => set_text(todo, "DESCRIPTION", d.as_deref()),
        Edit::Due(due) => set_due(todo, due.as_ref(), local),
        // iOS writes 1/5/9 and leaves the property out for "none".
        Edit::Priority(0) => {
            todo.remove("PRIORITY");
        }
        Edit::Priority(p) => todo.set(Property::new("PRIORITY", p.min(&9).to_string())),
        Edit::Complete(at) => complete(todo, *at, local)?,
        Edit::Reopen => {
            todo.set(Property::new("STATUS", "NEEDS-ACTION"));
            todo.remove("COMPLETED");
            todo.remove("PERCENT-COMPLETE");
        }
        Edit::Rrule(rule) => match rule {
            Some(r) => {
                recur::validate(r).map_err(EditError::BadRrule)?;
                todo.set(Property::new("RRULE", r.trim()));
            }
            None => {
                todo.remove("RRULE");
            }
        },
        Edit::Alarms(triggers) => set_alarms(todo, triggers),
        Edit::LocationAlarms(alarms) => {
            let now: Vec<LocationAlarm> = todo
                .components()
                .filter(|c| c.is("VALARM"))
                .filter_map(location_alarm)
                .collect();
            if now != *alarms {
                // The others stay: matching is by UID, so iOS keeps its own.
                todo.retain_components(|c| {
                    !(c.is("VALARM") && c.property("X-APPLE-PROXIMITY").is_some())
                });
                for a in alarms {
                    todo.push(location_valarm(a));
                }
            }
        }
        Edit::Parent(parent) => {
            let others: Vec<Property> = todo
                .properties_named("RELATED-TO")
                .filter(|p| {
                    p.param("RELTYPE")
                        .is_some_and(|r| !r.eq_ignore_ascii_case("PARENT"))
                })
                .cloned()
                .collect();
            todo.remove("RELATED-TO");
            for p in others {
                todo.add(p);
            }
            if let Some(uid) = parent {
                todo.add(Property::new("RELATED-TO", uid.as_str()));
            }
        }
        Edit::Source(s) => set_text(todo, SOURCE, s.as_deref()),
        Edit::SortOrder(order) => match order {
            Some(n) => todo.set(Property::new(SORT_ORDER, n.to_string())),
            None => {
                todo.remove(SORT_ORDER);
            }
        },
    }
    Ok(())
}

fn set_text(todo: &mut Component, name: &str, value: Option<&str>) {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => todo.set(Property::text(name, v)),
        None => {
            todo.remove(name);
        }
    }
}

fn ensure_vtimezone(ical: &mut Ical, w: &When) {
    let When::Zoned { tzid, at } = w else { return };
    let Some(tz) = resolve_tzid(tzid) else { return };
    let Some(cal) = ical.calendar_mut() else {
        return;
    };
    let defined = cal
        .components()
        .any(|c| c.is("VTIMEZONE") && c.property("TZID").is_some_and(|p| p.value().trim() == tzid));
    if !defined {
        use chrono::Datelike;
        let vtz = vtimezone(tz, at.year());
        // Before the VTODO, where clients expect definitions to be.
        let mut rest: Vec<Component> = Vec::new();
        cal.retain_components(|c| {
            if c.is("VTIMEZONE") {
                true
            } else {
                rest.push(c.clone());
                false
            }
        });
        cal.push(vtz);
        for c in rest {
            cal.push(c);
        }
    }
}

/// DUE. DTSTART is other clients' business: iOS keeps it equal to DUE and
/// ignores it otherwise, so asst never reads or writes it and it rides along.
fn set_due(todo: &mut Component, due: Option<&When>, local: Tz) {
    let old_due = todo.property("DUE").and_then(When::from_property);
    let at_old_due = |t: &Option<Trigger>, old: &Option<When>| match (t, old) {
        (Some(Trigger::Absolute { at }), Some(old)) => *at == old.instant(local),
        _ => false,
    };
    match due {
        None => {
            todo.remove("DUE");
            retain_alarms(todo, |t| !at_old_due(&t, &old_due));
        }
        Some(new) => {
            todo.set(new.to_property("DUE"));
            if !new.has_time() {
                // "Remind me when it's due" has no time to go off at any more.
                retain_alarms(todo, |t| !at_old_due(&t, &old_due));
            }
            // An alarm at the old due time was "remind me when it's due".
            if let Some(old) = &old_due {
                let (from, to) = (old.instant(local), new.instant(local));
                if new.has_time() && from != to {
                    for alarm in todo.components_mut().filter(|c| c.is("VALARM")) {
                        let at_due = alarm
                            .property("TRIGGER")
                            .and_then(Trigger::from_property)
                            .is_some_and(|t| matches!(t, Trigger::Absolute { at } if at == from));
                        if at_due {
                            alarm.set(Trigger::Absolute { at: to }.to_property());
                            alarm.remove("ACKNOWLEDGED");
                        }
                    }
                }
            }
        }
    }
}

fn retain_alarms(todo: &mut Component, mut keep: impl FnMut(Option<Trigger>) -> bool) {
    todo.retain_components(|c| {
        !c.is("VALARM")
            || c.property("X-APPLE-PROXIMITY").is_some()
            || keep(c.property("TRIGGER").and_then(Trigger::from_property))
    });
}

fn set_alarms(todo: &mut Component, triggers: &[Trigger]) {
    let mut wanted: Vec<&Trigger> = triggers.iter().collect();
    retain_alarms(todo, |t| {
        match t.and_then(|t| wanted.iter().position(|w| **w == t)) {
            Some(i) => {
                wanted.remove(i);
                true
            }
            None => false,
        }
    });
    for trigger in wanted {
        let uid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let mut alarm = Component::new("VALARM");
        alarm.set(Property::new("UID", uid.as_str()));
        alarm.set(Property::new("X-WR-ALARMUID", uid.as_str()));
        alarm.set(trigger.to_property());
        alarm.set(Property::new("ACTION", "DISPLAY"));
        alarm.set(Property::new("DESCRIPTION", "Reminder"));
        todo.push(alarm);
    }
}

fn complete(todo: &mut Component, at: DateTime<Utc>, local: Tz) -> Result<(), EditError> {
    let rule = todo.property("RRULE").map(|p| p.value().trim().to_string());
    let anchor = todo.property("DUE").and_then(When::from_property);
    if let (Some(rule), Some(anchor)) = (rule, anchor) {
        // The occurrence after this one, even when its date has gone by too:
        // as on iOS, each missed date is its own completion.
        let from = anchor.instant(local);
        match recur::next(&rule, &anchor, from, local) {
            Ok(Some(next)) => {
                shift(todo, next.instant(local) - from, at, local);
                if let Some(rest) = recur::consume_count(&rule, 1) {
                    todo.set(Property::new("RRULE", rest));
                }
                todo.remove("COMPLETED");
                todo.remove("PERCENT-COMPLETE");
                todo.set(Property::new("STATUS", "NEEDS-ACTION"));
                return Ok(());
            }
            Ok(None) => {} // The series is over: complete it for good.
            Err(e) => return Err(EditError::BadRrule(e)),
        }
    }
    todo.set(Property::new("STATUS", "COMPLETED"));
    todo.set(Property::new("COMPLETED", format_utc(&at)));
    todo.set(Property::new("PERCENT-COMPLETE", "100"));
    Ok(())
}

/// Move DUE and absolute alarms by the same amount. The moved
/// alarms ring again, except those already past at `now`: an occurrence
/// that was missed has nothing left to remind of.
fn shift(todo: &mut Component, delta: Duration, now: DateTime<Utc>, local: Tz) {
    if let Some(w) = todo.property("DUE").and_then(When::from_property) {
        let moved = w.with_instant(w.instant(local) + delta, local);
        todo.set(moved.to_property("DUE"));
    }
    let due = todo.property("DUE").and_then(When::from_property);
    for alarm in todo.components_mut().filter(|c| c.is("VALARM")) {
        let Some(mut trigger) = timed_trigger(alarm) else {
            continue;
        };
        if let Trigger::Absolute { at } = &mut trigger {
            *at += delta;
            alarm.set(trigger.to_property());
        }
        match trigger_instant(&trigger, due.as_ref(), local) {
            Some(rings) if rings <= now => {
                alarm.set(Property::new("ACKNOWLEDGED", format_utc(&now)));
            }
            _ => {
                alarm.remove("ACKNOWLEDGED");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone};

    use super::*;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 14, 16, 0, 0).unwrap()
    }

    const IOS: &str = "BEGIN:VCALENDAR\r\nCALSCALE:GREGORIAN\r\nPRODID:-//Apple Inc.//iOS 18.6//EN\r\nVERSION:2.0\r\n\
BEGIN:VTODO\r\nCREATED:20260901T120000Z\r\nDTSTAMP:20260901T120500Z\r\n\
DTSTART;TZID=America/New_York:20260915T090000\r\nDUE;TZID=America/New_York:20260915T090000\r\n\
LAST-MODIFIED:20260901T120500Z\r\nPRIORITY:5\r\nSEQUENCE:0\r\nSUMMARY:Pack for the trip\r\n\
UID:0F276A13-FBF3-49A1-8369-65EEA9C6F891\r\nX-APPLE-SORT-ORDER:28\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER;VALUE=DATE-TIME:20260915T130000Z\r\n\
UID:6C2F3A0B\r\nX-WR-ALARMUID:6C2F3A0B\r\nEND:VALARM\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

    fn edited(src: &str, edits: &[Edit]) -> (Ical, Task) {
        let mut ical = Ical::parse(src);
        apply(&mut ical, edits, now(), ny()).unwrap();
        let task = Task::from_ical(&ical).unwrap();
        (ical, task)
    }

    #[test]
    fn reads_an_ios_task() {
        let t = Task::from_ical(&Ical::parse(IOS)).unwrap();
        assert_eq!(t.uid, "0F276A13-FBF3-49A1-8369-65EEA9C6F891");
        assert_eq!(t.summary, "Pack for the trip");
        assert_eq!(priority_level(t.priority), 2);
        assert_eq!(t.status, Status::NeedsAction);
        assert_eq!(t.sort_order, Some(28));
        assert_eq!(
            t.alarm_instants(ny()),
            vec![Utc.with_ymd_and_hms(2026, 9, 15, 13, 0, 0).unwrap()]
        );
    }

    #[test]
    fn completing_touches_only_status_lines_and_stamps() {
        let (ical, t) = edited(IOS, &[Edit::Complete(now())]);
        assert_eq!(t.status, Status::Completed);
        let out = ical.to_string();
        let expected = IOS
            .replace("DTSTAMP:20260901T120500Z", "DTSTAMP:20260914T160000Z")
            .replace("LAST-MODIFIED:20260901T120500Z", "LAST-MODIFIED:20260914T160000Z")
            .replace(
                "X-APPLE-SORT-ORDER:28\r\n",
                "X-APPLE-SORT-ORDER:28\r\nSTATUS:COMPLETED\r\nCOMPLETED:20260914T160000Z\r\nPERCENT-COMPLETE:100\r\n",
            );
        assert_eq!(out, expected);

        let (reopened, t) = edited(&out, &[Edit::Reopen]);
        assert_eq!(t.status, Status::NeedsAction);
        assert!(!reopened.to_string().contains("COMPLETED:"));
    }

    #[test]
    fn moving_the_due_date_leaves_dtstart_alone_and_carries_the_due_alarm() {
        let new_due = When::Zoned {
            at: NaiveDate::from_ymd_opt(2026, 9, 16)
                .unwrap()
                .and_hms_opt(17, 30, 0)
                .unwrap(),
            tzid: "America/New_York".into(),
        };
        let (ical, t) = edited(IOS, &[Edit::Due(Some(new_due.clone()))]);
        assert_eq!(t.due, Some(new_due));
        assert_eq!(
            t.alarm_instants(ny()),
            vec![Utc.with_ymd_and_hms(2026, 9, 16, 21, 30, 0).unwrap()]
        );
        let out = ical.to_string();
        assert!(out.contains("DTSTART;TZID=America/New_York:20260915T090000\r\n"));
        assert!(out.contains("DUE;TZID=America/New_York:20260916T173000\r\n"));
        assert!(
            out.contains("UID:6C2F3A0B\r\nX-WR-ALARMUID:6C2F3A0B\r\n"),
            "alarm identity kept: {out}"
        );
        assert!(out.contains("X-APPLE-SORT-ORDER:28\r\n"));
    }

    #[test]
    fn clearing_the_due_date_drops_the_due_alarm_only() {
        let (ical, t) = edited(IOS, &[Edit::Due(None)]);
        assert_eq!(t.due, None);
        assert!(t.alarms.is_empty());
        let out = ical.to_string();
        assert!(!out.contains("VALARM"));
        assert!(out.contains("DTSTART;TZID=America/New_York:20260915T090000\r\n"));
    }

    #[test]
    fn a_date_only_due_leaves_dtstart_as_it_was() {
        let date = When::Date {
            date: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap(),
        };
        let (ical, t) = edited(IOS, &[Edit::Due(Some(date))]);
        assert_eq!(
            t.due,
            Some(When::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 20).unwrap()
            })
        );
        assert!(ical.to_string().contains("DTSTART;TZID=America/New_York:20260915T090000\r\n"));
    }

    #[test]
    fn zoned_due_on_a_new_task_brings_its_vtimezone() {
        let mut ical = new_ical("abc", now());
        let due = When::local(
            NaiveDate::from_ymd_opt(2026, 9, 15)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
            ny(),
        );
        apply(
            &mut ical,
            &[
                Edit::Summary("Call, then write".into()),
                Edit::Due(Some(due)),
            ],
            now(),
            ny(),
        )
        .unwrap();
        let out = ical.to_string();
        let vtz = out.find("BEGIN:VTIMEZONE").expect("vtimezone");
        assert!(vtz < out.find("BEGIN:VTODO").unwrap());
        assert!(out.contains("SUMMARY:Call\\, then write\r\n"));
        assert!(out.contains("DUE;TZID=America/New_York:20260915T090000\r\n"));
        // Setting it again doesn't add a second definition.
        let due2 = When::local(
            NaiveDate::from_ymd_opt(2026, 9, 16)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
            ny(),
        );
        apply(&mut ical, &[Edit::Due(Some(due2))], now(), ny()).unwrap();
        assert_eq!(ical.to_string().matches("BEGIN:VTIMEZONE").count(), 1);
    }

    #[test]
    fn alarms_keep_matching_valarms_verbatim() {
        let keep = Trigger::Absolute {
            at: Utc.with_ymd_and_hms(2026, 9, 15, 13, 0, 0).unwrap(),
        };
        let add = Trigger::Relative {
            offset: Duration::minutes(-30),
            from_due: true,
        };
        let (ical, t) = edited(IOS, &[Edit::Alarms(vec![keep, add])]);
        assert_eq!(t.alarms.len(), 2);
        let out = ical.to_string();
        assert!(out.contains("X-WR-ALARMUID:6C2F3A0B\r\n"));
        assert!(out.contains("TRIGGER;RELATED=END:-PT30M\r\n"));
    }

    #[test]
    fn completing_a_recurring_task_moves_it_to_the_next_occurrence() {
        let src = IOS.replace(
            "SEQUENCE:0\r\n",
            "SEQUENCE:0\r\nRRULE:FREQ=WEEKLY;BYDAY=TU\r\n",
        );
        let (ical, t) = edited(&src, &[Edit::Complete(now())]);
        assert_eq!(t.status, Status::NeedsAction);
        let next = When::Zoned {
            at: NaiveDate::from_ymd_opt(2026, 9, 22)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
            tzid: "America/New_York".into(),
        };
        assert_eq!(t.due, Some(next));
        assert_eq!(
            t.alarm_instants(ny()),
            vec![Utc.with_ymd_and_hms(2026, 9, 22, 13, 0, 0).unwrap()]
        );
        assert!(!ical.to_string().contains("COMPLETED"));
    }

    #[test]
    fn an_overdue_recurring_task_moves_one_occurrence() {
        // Daily at 9:00 since Sep 1, completed Sep 14: next is Sep 2, the
        // first missed date, and COUNT loses one. Its alarm went by long ago,
        // so it is marked seen rather than rung again.
        let src = IOS
            .replace("20260915T090000", "20260901T090000")
            .replace(
                "TRIGGER;VALUE=DATE-TIME:20260915T130000Z",
                "TRIGGER;VALUE=DATE-TIME:20260901T130000Z",
            )
            .replace(
                "SEQUENCE:0\r\n",
                "SEQUENCE:0\r\nRRULE:FREQ=DAILY;COUNT=20\r\n",
            );
        let (_, t) = edited(&src, &[Edit::Complete(now())]);
        assert_eq!(
            t.due.as_ref().unwrap().local_date(ny()),
            NaiveDate::from_ymd_opt(2026, 9, 2).unwrap()
        );
        assert_eq!(t.rrule.as_deref(), Some("FREQ=DAILY;COUNT=19"));
        assert_eq!(
            t.alarm_instants(ny()),
            vec![Utc.with_ymd_and_hms(2026, 9, 2, 13, 0, 0).unwrap()]
        );
        assert!(t.unacknowledged_alarms(ny()).is_empty());
    }

    #[test]
    fn a_date_only_recurring_task_moves_by_its_interval() {
        let src = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:d\r\nSUMMARY:Water plants\r\n\
DUE;VALUE=DATE:20260910\r\nRRULE:FREQ=DAILY;INTERVAL=2\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let (ical, t) = edited(src, &[Edit::Complete(now())]);
        assert_eq!(
            t.due,
            Some(When::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()
            })
        );
        assert!(ical.to_string().contains("DUE;VALUE=DATE:20260912\r\n"));
    }

    #[test]
    fn completing_early_keeps_the_next_alarm_ringing() {
        let src = IOS.replace("SEQUENCE:0\r\n", "SEQUENCE:0\r\nRRULE:FREQ=DAILY\r\n");
        let acked = src.replace(
            "X-WR-ALARMUID:6C2F3A0B\r\n",
            "X-WR-ALARMUID:6C2F3A0B\r\nACKNOWLEDGED:20260915T130100Z\r\n",
        );
        let (_, t) = edited(&acked, &[Edit::Complete(now())]);
        assert_eq!(
            t.unacknowledged_alarms(ny()),
            vec![Utc.with_ymd_and_hms(2026, 9, 16, 13, 0, 0).unwrap()]
        );
    }

    #[test]
    fn the_ios_no_alarm_placeholder_stays_put() {
        let src = IOS
            .replace(
                "TRIGGER;VALUE=DATE-TIME:20260915T130000Z",
                "TRIGGER;VALUE=DATE-TIME:19760401T005545Z",
            )
            .replace("SEQUENCE:0\r\n", "SEQUENCE:0\r\nRRULE:FREQ=DAILY\r\n");
        let (ical, t) = edited(&src, &[Edit::Complete(now())]);
        assert!(
            ical.to_string()
                .contains("TRIGGER;VALUE=DATE-TIME:19760401T005545Z\r\n")
        );
        assert!(t.alarms.is_empty());
    }

    #[test]
    fn a_finished_series_completes() {
        let src = IOS.replace(
            "SEQUENCE:0\r\n",
            "SEQUENCE:0\r\nRRULE:FREQ=DAILY;UNTIL=20260915T235959Z\r\n",
        );
        let (_, t) = edited(
            &src,
            &[Edit::Complete(
                Utc.with_ymd_and_hms(2026, 9, 15, 14, 0, 0).unwrap(),
            )],
        );
        assert_eq!(t.status, Status::Completed);
    }

    #[test]
    fn completed_copy_of_a_repeating_task() {
        let src = IOS.replace(
            "SEQUENCE:0\r\n",
            "SEQUENCE:0\r\nRRULE:FREQ=WEEKLY\r\nX-ASST-SOURCE:steno:a/b\r\n",
        );
        let copy = completed_copy(&Ical::parse(&src), now()).unwrap();
        let t = Task::from_ical(&copy).unwrap();
        assert_ne!(t.uid, "0F276A13-FBF3-49A1-8369-65EEA9C6F891");
        assert_eq!(t.status, Status::Completed);
        assert_eq!((t.rrule, t.source, t.alarms.len()), (None, None, 0));
        assert_eq!(
            t.due,
            Task::from_ical(&Ical::parse(&src)).unwrap().due,
            "keeps the occurrence's date"
        );
        assert!(
            completed_copy(&Ical::parse(IOS), now()).is_none(),
            "only for repeating tasks"
        );
    }

    #[test]
    fn a_location_alarm_is_parsed_but_not_timed() {
        let src = IOS.replace(
            "END:VALARM\r\n",
            "END:VALARM\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Home\r\n\
TRIGGER;VALUE=DATE-TIME:19760401T005545Z\r\nUID:LOC-1\r\nX-WR-ALARMUID:LOC-1\r\n\
X-APPLE-PROXIMITY:ARRIVE\r\n\
X-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-ADDRESS=\"1 Main St, Springfield\";X-TITLE=Home:geo:40.7,-74.0?u=50\r\n\
END:VALARM\r\n",
        );
        let t = Task::from_ical(&Ical::parse(&src)).unwrap();
        assert_eq!(t.alarms.len(), 1, "the timed alarm stays timed");
        assert_eq!(t.alarm_instants(ny()).len(), 1);
        assert_eq!(
            t.location_alarms,
            vec![LocationAlarm {
                uid: "LOC-1".into(),
                title: "Home".into(),
                address: "1 Main St, Springfield".into(),
                lat: 40.7,
                lon: -74.0,
                radius: 50,
            }]
        );
    }

    #[test]
    fn a_location_alarm_without_a_radius_reads_100_m() {
        let src = IOS.replace(
            "END:VALARM\r\n",
            "END:VALARM\r\nBEGIN:VALARM\r\nX-APPLE-PROXIMITY:ARRIVE\r\n\
TRIGGER;VALUE=DATE-TIME:19760401T005545Z\r\nUID:LOC-2\r\n\
X-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-TITLE=Home:geo:40.7,-74.0\r\nEND:VALARM\r\n",
        );
        let t = Task::from_ical(&Ical::parse(&src)).unwrap();
        assert_eq!(t.location_alarms[0].radius, 100);
        assert_eq!(t.location_alarms[0].address, "");
    }

    #[test]
    fn saving_timed_reminders_keeps_location_alarms() {
        let src = IOS.replace(
            "END:VALARM\r\n",
            "END:VALARM\r\nBEGIN:VALARM\r\nX-APPLE-PROXIMITY:ARRIVE\r\n\
TRIGGER;VALUE=DATE-TIME:19760401T005545Z\r\nUID:LOC-2\r\n\
X-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-TITLE=Home:geo:40.7,-74.0\r\nEND:VALARM\r\n",
        );
        let (ical, t) = edited(&src, &[Edit::Alarms(Vec::new())]);
        assert!(t.alarms.is_empty());
        assert_eq!(t.location_alarms.len(), 1);
        let out = ical.to_string();
        assert!(out.contains("X-APPLE-PROXIMITY:ARRIVE\r\n"), "{out}");
        assert!(out.contains("X-APPLE-STRUCTURED-LOCATION"));
    }

    #[test]
    fn location_alarms_are_replaced_in_place_and_new_ones_get_uids() {
        let src = IOS.replace(
            "END:VALARM\r\n",
            "END:VALARM\r\nBEGIN:VALARM\r\nX-APPLE-PROXIMITY:ARRIVE\r\n\
TRIGGER;VALUE=DATE-TIME:19760401T005545Z\r\nUID:LOC-2\r\n\
X-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-TITLE=Home:geo:40.7,-74.0\r\nEND:VALARM\r\n",
        );
        let keep = LocationAlarm {
            uid: "LOC-2".into(),
            title: "Home".into(),
            address: "".into(),
            lat: 40.7,
            lon: -74.0,
            radius: 100,
        };
        let (ical, t) = edited(
            &src,
            &[Edit::LocationAlarms(vec![
                keep,
                LocationAlarm {
                    uid: String::new(),
                    title: "Store".into(),
                    address: "2 Elm St".into(),
                    lat: 41.0,
                    lon: -73.5,
                    radius: 250,
                },
            ])],
        );
        assert_eq!(t.location_alarms.len(), 2);
        assert_eq!(t.location_alarms[0].uid, "LOC-2");
        assert!(t.location_alarms[1].uid.len() == 36, "a new UID");
        let out = ical.to_string();
        assert!(out.contains("u=250"), "{out}");
        assert!(out.contains("UID:6C2F3A0B\r\n"), "the timed alarm stays: {out}");
        assert_eq!(out.matches("X-APPLE-PROXIMITY:ARRIVE\r\n").count(), 2);
        // Saving the same list again changes nothing.
        let (again, _) = edited(&out, &[Edit::LocationAlarms(t.location_alarms)]);
        assert_eq!(again.to_string(), out);
    }

    #[test]
    fn no_priority_is_no_property() {
        let (ical, t) = edited(IOS, &[Edit::Priority(0)]);
        assert_eq!(t.priority, 0);
        assert!(!ical.to_string().contains("PRIORITY"));
    }

    #[test]
    fn source_and_parent_round_trip() {
        let (_, t) = edited(
            IOS,
            &[
                Edit::Source(Some("steno:2026-09-14/abc".into())),
                Edit::Parent(Some("P1".into())),
            ],
        );
        assert_eq!(t.source.as_deref(), Some("steno:2026-09-14/abc"));
        assert_eq!(t.parent.as_deref(), Some("P1"));
    }

    #[test]
    fn sort_order_is_its_one_line() {
        let (ical, t) = edited(IOS, &[Edit::SortOrder(Some(-4))]);
        assert_eq!(t.sort_order, Some(-4));
        let expected = IOS
            .replace("DTSTAMP:20260901T120500Z", "DTSTAMP:20260914T160000Z")
            .replace(
                "LAST-MODIFIED:20260901T120500Z",
                "LAST-MODIFIED:20260914T160000Z",
            )
            .replace("X-APPLE-SORT-ORDER:28\r\n", "X-APPLE-SORT-ORDER:-4\r\n");
        assert_eq!(ical.to_string(), expected);

        let (ical, t) = edited(IOS, &[Edit::SortOrder(None)]);
        assert_eq!(t.sort_order, None);
        assert!(!ical.to_string().contains("X-APPLE-SORT-ORDER"));

        let mut fresh = new_ical("n", now());
        apply(&mut fresh, &[Edit::SortOrder(Some(7))], now(), ny()).unwrap();
        assert_eq!(Task::from_ical(&fresh).unwrap().sort_order, Some(7));
    }

    #[test]
    fn a_duplicate_is_the_same_task_with_a_new_identity() {
        let src = IOS.replace(
            "SEQUENCE:0\r\n",
            "SEQUENCE:3\r\nX-ASST-SOURCE:steno:a/b\r\nX-OTHER-CLIENT:kept\r\n",
        );
        let original = Ical::parse(&src);
        let copy = duplicate(&original, now()).unwrap();
        let (a, b) = (
            Task::from_ical(&original).unwrap(),
            Task::from_ical(&copy).unwrap(),
        );
        assert_ne!(a.uid, b.uid);
        assert_eq!(b.source, None);
        assert_eq!(
            (&b.summary, &b.due, b.priority, b.sort_order, &b.alarms),
            (&a.summary, &a.due, a.priority, a.sort_order, &a.alarms)
        );
        assert_eq!(b.created, Some(now()));
        let out = copy.to_string();
        assert!(out.contains("X-OTHER-CLIENT:kept\r\n"));
        assert!(out.contains("SEQUENCE:0\r\n"));
        assert!(!out.contains("6C2F3A0B"), "alarm UIDs are new: {out}");
        let alarm_uid = out
            .lines()
            .find_map(|l| l.strip_prefix("X-WR-ALARMUID:"))
            .unwrap();
        assert!(out.contains(&format!("\r\nUID:{alarm_uid}\r\n")));
    }
}
