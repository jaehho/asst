//! The snooze chooser (#20): when to ring a reminder again, for the times
//! a notification's own Snooze buttons (10, 30, 60 min by default) don't
//! cover. A reminder's `Snooze…` button asks the window for it
//! (`snooze-task`). It is a small popup like quick add's: a layer surface
//! over everything, so the window need not be showing, with a few presets
//! and a field that reads `2h`, `45m`, `5pm` or `tomorrow 8am`.

use asst_core::fmt;
use asst_core::quickadd;
use asst_core::time::{When, resolve};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::client;
use crate::model::{capitalize, parse_time};
use crate::pickers;

/// When "morning" is: where a wait of a day or more lands.
const MORNING: u32 = 9;
/// And "this evening".
const EVENING: u32 = 18;
/// Longest wait the daemon takes.
const LONGEST: Duration = Duration::days(366);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub at: DateTime<Utc>,
}

/// The presets from `now`, earliest first: soon, later today, then the next
/// morning and the next Monday. Each one is a different moment.
pub fn choices(now: DateTime<Tz>) -> Vec<Choice> {
    let zone = now.timezone();
    let today = now.date_naive();
    let at = |date: NaiveDate, hour: u32| {
        resolve(
            zone,
            date.and_time(NaiveTime::from_hms_opt(hour, 0, 0).expect("an hour")),
        )
    };
    let from_now = |label: &str, d: Duration| Choice {
        label: label.into(),
        at: now.with_timezone(&Utc) + d,
    };
    let mut out = vec![
        from_now("In 15 minutes", Duration::minutes(15)),
        from_now("In 1 hour", Duration::hours(1)),
        from_now("In 3 hours", Duration::hours(3)),
    ];
    // "This evening" only where it is not just another few hours.
    let evening = at(today, EVENING);
    if evening - now.with_timezone(&Utc) > Duration::hours(3) {
        out.push(Choice {
            label: "This evening".into(),
            at: evening,
        });
    }
    // Before dawn, the morning is the one coming.
    let (label, morning) = if now.time() < NaiveTime::from_hms_opt(6, 0, 0).expect("6am") {
        ("This morning", today)
    } else {
        ("Tomorrow morning", today.succ_opt().unwrap_or(today))
    };
    out.push(Choice {
        label: label.into(),
        at: at(morning, MORNING),
    });
    out.push(Choice {
        label: "Next Monday".into(),
        at: at(next_monday(today), MORNING),
    });
    // Before dawn the morning comes before the evening; on a Sunday night
    // tomorrow morning is the next Monday, which keeps its first name.
    out.sort_by_key(|c| c.at);
    out.dedup_by_key(|c| c.at);
    out
}

fn next_monday(from: NaiveDate) -> NaiveDate {
    let ahead = (7 - from.weekday().days_since(Weekday::Mon)) % 7;
    from + Duration::days(if ahead == 0 { 7 } else { i64::from(ahead) })
}

/// What the field means: a wait (`45m`, `2h`, `1h30m`, `3 days`, a bare
/// number is minutes), a time of day (`5pm`, `17:30`; tomorrow's if it has
/// gone), or a date phrase (`tomorrow 8am`, `fri`; a bare day is that
/// morning). None for anything else, the past, or more than a year.
pub fn parse(text: &str, now: DateTime<Tz>) -> Option<DateTime<Utc>> {
    let lower = text.trim().to_lowercase();
    let t = lower.strip_prefix("in ").unwrap_or(&lower).trim();
    if t.is_empty() {
        return None;
    }
    let zone = now.timezone();
    let now_utc = now.with_timezone(&Utc);
    let at = if let Some(wait) = duration(t) {
        now_utc.checked_add_signed(wait)?
    } else if let Some(time) = parse_time(t) {
        let today = resolve(zone, now.date_naive().and_time(time));
        if today > now_utc {
            today
        } else {
            let tomorrow = now.date_naive().succ_opt()?;
            resolve(zone, tomorrow.and_time(time))
        }
    } else {
        match quickadd::parse_when(t, now)? {
            When::Date { date } => resolve(
                zone,
                date.and_time(NaiveTime::from_hms_opt(MORNING, 0, 0).expect("an hour")),
            ),
            other => other.instant(zone),
        }
    };
    (at > now_utc && at - now_utc <= LONGEST).then_some(at)
}

/// `45m`, `2h`, `1h30m`, `1 h 30 min`, `3 days`, `2w`; a bare number is
/// minutes.
fn duration(t: &str) -> Option<Duration> {
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    let mut rest = t.as_str();
    let mut total = Duration::zero();
    let mut first = true;
    while !rest.is_empty() {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let n: i64 = rest.get(..digits)?.parse().ok()?;
        rest = &rest[digits..];
        let letters = rest.chars().take_while(char::is_ascii_alphabetic).count();
        let unit = &rest[..letters];
        rest = &rest[letters..];
        let part = match unit {
            "" if first && rest.is_empty() => Duration::try_minutes(n),
            "m" | "min" | "mins" | "minute" | "minutes" => Duration::try_minutes(n),
            "h" | "hr" | "hrs" | "hour" | "hours" => Duration::try_hours(n),
            "d" | "day" | "days" => Duration::try_days(n),
            "w" | "wk" | "wks" | "week" | "weeks" => Duration::try_weeks(n),
            _ => return None,
        }?;
        total = total.checked_add(&part)?;
        first = false;
    }
    (!first).then_some(total)
}

/// `Today 3:41pm`, `Tomorrow 9am`, `Mon 9am`: when a choice lands.
pub fn when_label(at: DateTime<Utc>, now: DateTime<Tz>) -> String {
    let local = at.with_timezone(&now.timezone());
    format!(
        "{} {}",
        capitalize(&fmt::day_label(local.date_naive(), now.date_naive())),
        fmt::time_label(local.time())
    )
}

/// What choosing a time does: ask the daemon, and say how it went.
pub type Pick = Box<dyn Fn(DateTime<Utc>, Box<dyn FnOnce(client::Result<()>)>)>;

/// Open the chooser for the task named `title`.
pub fn present(title: &str, pick: Pick) -> gtk::Window {
    crate::load_icons();
    let window = gtk::Window::builder()
        .title("Snooze")
        .default_width(380)
        .resizable(false)
        .build();
    window.add_css_class("asst-snooze");
    window.set_application(Some(&relm4::main_application()));
    // A scripted check must not take the keyboard, nor show over whoever
    // is working: in development it is a plain window, which the harness
    // keeps on its hidden workspace.
    if !crate::devel::enabled() {
        window.init_layer_shell();
        window.set_namespace(Some("asst-snooze"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_anchor(Edge::Top, true);
        window.set_margin(Edge::Top, 160);
    }

    let now = pickers::now();
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("card");
    card.add_css_class("snooze-card");
    let heading = gtk::Label::builder()
        .label(format!("Snooze “{title}”"))
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .max_width_chars(40)
        .build();
    heading.add_css_class("heading");
    card.append(&heading);

    let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
    status.add_css_class("caption");
    status.add_css_class("dim-label");
    let pick: std::rc::Rc<Pick> = std::rc::Rc::new(pick);
    // One go at a time: a second click while the daemon answers is ignored.
    let busy = std::rc::Rc::new(std::cell::Cell::new(false));
    let go = {
        let window = window.clone();
        let status = status.clone();
        let busy = busy.clone();
        let pick = pick.clone();
        std::rc::Rc::new(move |at: DateTime<Utc>| {
            if busy.replace(true) {
                return;
            }
            let (window, status, busy) = (window.clone(), status.clone(), busy.clone());
            pick(
                at,
                Box::new(move |result| match result {
                    Ok(()) => window.close(),
                    Err(e) => {
                        busy.set(false);
                        status.add_css_class("error");
                        status.set_text(&e);
                    }
                }),
            );
        })
    };

    let mut first: Option<gtk::Button> = None;
    for choice in choices(now) {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let label = gtk::Label::builder()
            .label(&choice.label)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let when = gtk::Label::new(Some(&when_label(choice.at, now)));
        when.add_css_class("dim-label");
        line.append(&label);
        line.append(&when);
        let button = gtk::Button::builder().child(&line).build();
        button.add_css_class("flat");
        let (go, at) = (go.clone(), choice.at);
        button.connect_clicked(move |_| go(at));
        card.append(&button);
        first.get_or_insert(button);
    }

    let entry = gtk::Entry::builder()
        .placeholder_text("Or type: 2h, 45m, 5pm, tomorrow 8am")
        .build();
    entry.add_css_class("snooze-entry");
    card.append(&entry);
    status.set_text(" ");
    card.append(&status);
    {
        let status = status.clone();
        entry.connect_changed(move |e| {
            status.remove_css_class("error");
            let text = e.text();
            if text.trim().is_empty() {
                status.set_text(" ");
                return;
            }
            let now = pickers::now();
            match parse(&text, now) {
                Some(at) => status.set_text(&when_label(at, now)),
                None => {
                    status.add_css_class("error");
                    status.set_text("Can't read that as a time");
                }
            }
        });
    }
    {
        let go = go.clone();
        entry.connect_activate(move |e| {
            if let Some(at) = parse(&e.text(), pickers::now()) {
                go(at);
            }
        });
    }

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let window = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);
    window.set_child(Some(&card));
    window.present();
    // Typing starts at once; Down from the field walks the presets.
    entry.grab_focus();
    window
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn zone() -> Tz {
        "America/New_York".parse().unwrap()
    }

    /// A local wall-clock time on Wed 2026-10-14 (EDT).
    fn at(h: u32, m: u32) -> DateTime<Tz> {
        zone().with_ymd_and_hms(2026, 10, 14, h, m, 0).unwrap()
    }

    fn local(t: DateTime<Utc>) -> String {
        t.with_timezone(&zone()).format("%a %H:%M").to_string()
    }

    #[test]
    fn presets_are_different_moments_in_order() {
        let labels = |now| {
            choices(now)
                .into_iter()
                .map(|c| c.label)
                .collect::<Vec<_>>()
        };
        // A Wednesday noon: the evening is six hours off.
        assert_eq!(
            labels(at(12, 0)),
            [
                "In 15 minutes",
                "In 1 hour",
                "In 3 hours",
                "This evening",
                "Tomorrow morning",
                "Next Monday"
            ]
        );
        let noon = choices(at(12, 0));
        assert_eq!(local(noon[0].at), "Wed 12:15");
        assert_eq!(local(noon[3].at), "Wed 18:00");
        assert_eq!(local(noon[4].at), "Thu 09:00");
        assert_eq!(local(noon[5].at), "Mon 09:00");
        // Mid-afternoon the evening is inside "in 3 hours"; late, gone.
        assert!(!labels(at(15, 30)).contains(&"This evening".to_string()));
        assert!(!labels(at(21, 0)).contains(&"This evening".to_string()));
        // Before dawn the morning is this one.
        let night = choices(at(1, 30));
        let labels: Vec<_> = night.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "In 15 minutes",
                "In 1 hour",
                "In 3 hours",
                "This morning",
                "This evening",
                "Next Monday"
            ]
        );
        assert_eq!(local(night[3].at), "Wed 09:00");
    }

    #[test]
    fn next_monday_is_not_listed_twice() {
        // Sunday night: tomorrow morning is Monday already.
        let sunday = zone().with_ymd_and_hms(2026, 10, 18, 21, 0, 0).unwrap();
        let labels: Vec<_> = choices(sunday).into_iter().map(|c| c.label).collect();
        assert_eq!(labels.last().map(String::as_str), Some("Tomorrow morning"));
        assert!(!labels.contains(&"Next Monday".to_string()));
        // Monday itself: the next one is a week off.
        let monday = zone().with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap();
        let next = choices(monday).pop().unwrap();
        assert_eq!(local(next.at), "Mon 09:00");
        assert_eq!(next.at.with_timezone(&zone()).day(), 26);
    }

    #[test]
    fn the_field_reads_waits_clock_times_and_dates() {
        let now = at(14, 10);
        let read = |s: &str| parse(s, now).map(local);
        assert_eq!(read("45m").as_deref(), Some("Wed 14:55"));
        assert_eq!(read("2h").as_deref(), Some("Wed 16:10"));
        assert_eq!(read("in 2 hours").as_deref(), Some("Wed 16:10"));
        assert_eq!(read("1h30m").as_deref(), Some("Wed 15:40"));
        assert_eq!(read("1 h 30 min").as_deref(), Some("Wed 15:40"));
        assert_eq!(read("90").as_deref(), Some("Wed 15:40"), "minutes");
        assert_eq!(read("2d").as_deref(), Some("Fri 14:10"));
        assert_eq!(read("5pm").as_deref(), Some("Wed 17:00"));
        assert_eq!(read("17:30").as_deref(), Some("Wed 17:30"));
        assert_eq!(read("9am").as_deref(), Some("Thu 09:00"), "gone today");
        assert_eq!(read("tomorrow 8am").as_deref(), Some("Thu 08:00"));
        assert_eq!(read("friday").as_deref(), Some("Fri 09:00"), "a bare day");
        assert_eq!(read("  Tomorrow  ").as_deref(), Some("Thu 09:00"));
    }

    #[test]
    fn the_field_refuses_what_it_cannot_honor() {
        let now = at(14, 10);
        for bad in [
            "",
            "  ",
            "soon",
            "0",
            "0m",
            "-5m",
            "5x",
            "h",
            "1h30",
            "400d",
            "yesterday",
            "99999999999999999999d",
            "tomorrow and more",
        ] {
            assert_eq!(parse(bad, now), None, "{bad:?}");
        }
    }

    #[test]
    fn a_choice_says_when_it_lands() {
        let now = at(14, 10);
        let label = |t: DateTime<Tz>| when_label(t.with_timezone(&Utc), now);
        assert_eq!(label(at(15, 41)), "Today 3:41pm");
        assert_eq!(label(at(18, 0)), "Today 6pm");
        let tomorrow = zone().with_ymd_and_hms(2026, 10, 15, 9, 0, 0).unwrap();
        assert_eq!(label(tomorrow), "Tomorrow 9am");
        let monday = zone().with_ymd_and_hms(2026, 10, 19, 9, 0, 0).unwrap();
        assert_eq!(label(monday), "Mon 9am");
    }
}
