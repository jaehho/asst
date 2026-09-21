//! Popovers that set a task's properties, after Planify's: dates (with time
//! and repeat), priority, list, reminders. Each takes the current value and
//! a callback; the window turns the callback into an edit.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use asst_core::api::ListView;
use asst_core::fmt;
use asst_core::quickadd;
use asst_core::task::Task;
use asst_core::time::{Trigger, When};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib, pango};

use crate::model::{self, End, Freq, REPEAT_PRESETS, Repeat, WEEK, capitalize};
use crate::ui::{self, Item};

pub fn zone() -> Tz {
    static ZONE: OnceLock<Tz> = OnceLock::new();
    *ZONE.get_or_init(asst_core::time::local_zone)
}

pub fn now() -> DateTime<Tz> {
    Utc::now().with_timezone(&zone())
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Schedule {
    pub due: Option<When>,
    pub rrule: Option<String>,
}

impl Schedule {
    pub fn of(task: &Task) -> Schedule {
        Schedule {
            due: task.due.clone(),
            rrule: task.rrule.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DateOpts {
    pub sunday_first: bool,
    /// Time and repeat rows, and "No date" when there is a date.
    pub full: bool,
    /// "No date" whatever the start: for a date given to several tasks.
    pub clear: bool,
}

fn to_date(d: &glib::DateTime) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)
}

fn from_date(d: NaiveDate) -> Option<glib::DateTime> {
    glib::DateTime::from_local(d.year(), d.month() as i32, d.day() as i32, 0, 0, 0.0).ok()
}

/// Focus a field in a popover, if its window has the keyboard: an unfocused
/// window (a scripted check) would take the grab as a reason to close it.
pub fn focus_in(p: &gtk::Popover, w: &impl IsA<gtk::Widget>) {
    if p.root()
        .and_downcast::<gtk::Window>()
        .is_some_and(|win| win.is_active())
    {
        w.grab_focus();
    }
}

fn page_header(title: &str, back: impl Fn() + 'static) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let button = gtk::Button::from_icon_name("go-previous-symbolic");
    button.add_css_class("flat");
    button.set_tooltip_text(Some("Back"));
    button.connect_clicked(move |_| back());
    b.append(&button);
    b.append(&ui::label(title, &["heading"]));
    b
}

fn suggestion(icon: &str, text: &str, on: impl Fn() + 'static) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(
        &gtk::Label::builder()
            .label(text)
            .use_markup(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build(),
    );
    let b = gtk::Button::builder().child(&content).build();
    b.add_css_class("suggestion-chip");
    b.connect_clicked(move |_| on());
    b
}

/// Chips that wrap onto more lines.
fn chip_box() -> adw::WrapBox {
    adw::WrapBox::builder()
        .child_spacing(6)
        .line_spacing(6)
        .natural_line_length(270)
        .build()
}

/// A day's icon, as Planify picks it.
fn day_icon(d: NaiveDate, today: NaiveDate) -> &'static str {
    match (d - today).num_days() {
        0 => "star-outline-thick-symbolic",
        1 => "today-calendar-symbolic",
        _ => "month-symbolic",
    }
}

/// The Time and Repeat rows: a button with a label, and a clear button
/// once it has a value.
struct OptionRow {
    root: gtk::Box,
    button: gtk::Button,
    label: gtk::Label,
    add: gtk::Image,
    clear: gtk::Button,
}

fn option_row(icon: &str, text: &str) -> OptionRow {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&gtk::Image::from_icon_name(icon));
    let label = ui::label(text, &["option-label"]);
    content.append(&label);
    let add = gtk::Image::from_icon_name("plus-large-symbolic");
    add.set_hexpand(true);
    add.set_halign(gtk::Align::End);
    add.add_css_class("dim-label");
    content.append(&add);
    let button = gtk::Button::builder().child(&content).hexpand(true).build();
    button.add_css_class("flat");
    button.add_css_class("menu-item");
    let clear = gtk::Button::from_icon_name("cross-large-circle-filled-symbolic");
    clear.add_css_class("flat");
    clear.add_css_class("circular");
    clear.set_valign(gtk::Align::Center);
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.append(&button);
    root.append(&clear);
    OptionRow {
        root,
        button,
        label,
        add,
        clear,
    }
}

impl OptionRow {
    /// The label, the plus and the clear button, cloned for a closure.
    fn parts(&self) -> (gtk::Label, gtk::Image, gtk::Button) {
        (self.label.clone(), self.add.clone(), self.clear.clone())
    }
}

fn set_option(parts: &(gtk::Label, gtk::Image, gtk::Button), value: Option<String>, empty: &str) {
    let (label, add, clear) = parts;
    label.set_text(value.as_deref().unwrap_or(empty));
    add.set_visible(value.is_none());
    clear.set_visible(value.is_some());
}

// -- dates ------------------------------------------------------------------

type Changed = Rc<dyn Fn(Schedule)>;
type Redraw = Rc<dyn Fn()>;

/// Planify's date picker: suggestions, a typed date, three weeks to click,
/// a full calendar, and rows for the time and the repeat.
pub fn date_popover(
    start: Schedule,
    opts: DateOpts,
    on_change: impl Fn(Schedule) + 'static,
) -> gtk::Popover {
    let now = now();
    let zone = now.timezone();
    let today = now.date_naive();
    let state = Rc::new(RefCell::new(start));
    let on_change: Changed = Rc::new(on_change);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .vhomogeneous(false)
        .interpolate_size(true)
        .build();
    let popover = ui::popover(&stack);
    popover.set_width_request(300);
    popover.add_css_class("picker");

    let main = gtk::Box::new(gtk::Orientation::Vertical, 6);
    main.set_margin_top(9);
    main.set_margin_bottom(9);
    main.set_margin_start(9);
    main.set_margin_end(9);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Type a date…")
        .build();
    main.append(&search);
    let chips = chip_box();
    main.append(&chips);
    main.append(&ui::separator());

    // Picking a day keeps the time the task had.
    let pick_day: Rc<dyn Fn(NaiveDate)> = {
        let state = state.clone();
        let on_change = on_change.clone();
        let popover = popover.clone();
        Rc::new(move |d| {
            let mut s = state.borrow().clone();
            s.due = Some(model::on_date(s.due.as_ref(), d, zone));
            *state.borrow_mut() = s.clone();
            on_change(s);
            popover.popdown();
        })
    };

    let days = Rc::new(RefCell::new(Vec::<(NaiveDate, gtk::Button)>::new()));
    main.append(&week_grid(
        today,
        opts.sunday_first,
        &days,
        pick_day.clone(),
    ));
    let select_day = {
        let days = days.clone();
        move |d: Option<NaiveDate>| {
            for (day, b) in days.borrow().iter() {
                if Some(*day) == d {
                    b.add_css_class("selected");
                } else {
                    b.remove_css_class("selected");
                }
            }
        }
    };
    select_day(state.borrow().due.as_ref().map(|d| d.local_date(zone)));

    let choose = {
        let stack = stack.clone();
        Item::new(Some("month-symbolic"), "Choose a date")
            .arrow()
            .build(move || stack.set_visible_child_name("calendar"))
    };
    main.append(&choose);

    let time_row = option_row("clock-symbolic", "Time");
    let repeat_row = option_row("playlist-repeat-symbolic", "Repeat");
    let refresh: Rc<dyn Fn()> = {
        let state = state.clone();
        let (time_parts, repeat_parts) = (time_row.parts(), repeat_row.parts());
        Rc::new(move || {
            let s = state.borrow().clone();
            let time = s
                .due
                .as_ref()
                .filter(|d| d.has_time())
                .map(|d| fmt::time_label(d.instant(zone).with_timezone(&zone).time()));
            set_option(&time_parts, time, "Time");
            set_option(
                &repeat_parts,
                s.rrule
                    .as_deref()
                    .map(|r| capitalize(&fmt::repeat_label(r))),
                "Repeat",
            );
            select_day(s.due.as_ref().map(|d| d.local_date(zone)));
        })
    };

    // Changes that keep the popover open.
    let apply: Rc<dyn Fn(Schedule)> = {
        let state = state.clone();
        let on_change = on_change.clone();
        let refresh = refresh.clone();
        Rc::new(move |s| {
            *state.borrow_mut() = s.clone();
            on_change(s);
            refresh();
        })
    };

    if opts.full {
        main.append(&ui::separator());
        main.append(&time_row.root);
        main.append(&repeat_row.root);
    }

    // Suggestions, or what the typed text says.
    let typed: Rc<RefCell<Option<Schedule>>> = Rc::default();
    let fill_defaults: Rc<dyn Fn()> = {
        let chips = chips.clone();
        let pick_day = pick_day.clone();
        let state = state.clone();
        let on_change = on_change.clone();
        let popover = popover.clone();
        Rc::new(move || {
            chips.remove_all();
            let next_week = today + Duration::days(7);
            for (text, d) in [
                ("Today", today),
                ("Tomorrow", today + Duration::days(1)),
                ("Next week", next_week),
            ] {
                let pick = pick_day.clone();
                chips.append(&suggestion(day_icon(d, today), text, move || pick(d)));
            }
            if opts.clear || (opts.full && state.borrow().due.is_some()) {
                let on_change = on_change.clone();
                let state = state.clone();
                let popover = popover.clone();
                chips.append(&suggestion(
                    "cross-large-circle-filled-symbolic",
                    "No date",
                    move || {
                        let s = Schedule::default();
                        *state.borrow_mut() = s.clone();
                        on_change(s);
                        popover.popdown();
                    },
                ));
            }
        })
    };
    fill_defaults();
    {
        let chips = chips.clone();
        let typed = typed.clone();
        let fill_defaults = fill_defaults.clone();
        let state = state.clone();
        let on_change = on_change.clone();
        let popover = popover.clone();
        search.connect_search_changed(move |e| {
            let text = e.text().trim().to_string();
            if text.is_empty() {
                *typed.borrow_mut() = None;
                fill_defaults();
                return;
            }
            let p = quickadd::parse(&format!("x {text}"), now, &[]);
            chips.remove_all();
            let Some(due) = p.due.clone() else {
                *typed.borrow_mut() = None;
                chips.append(&ui::label("Not a date I know", &["dim-label", "caption"]));
                return;
            };
            let s = Schedule {
                due: Some(due.clone()),
                rrule: p.rrule.clone().or_else(|| state.borrow().rrule.clone()),
            };
            let mut text = capitalize(&fmt::due_label(&due, now));
            if let Some(r) = &p.rrule {
                text = format!(
                    "{text}, <small>{}</small>",
                    glib::markup_escape_text(&fmt::repeat_label(r))
                );
            }
            *typed.borrow_mut() = Some(s.clone());
            let (state, on_change, popover) = (state.clone(), on_change.clone(), popover.clone());
            chips.append(&suggestion(
                day_icon(due.local_date(zone), today),
                &text,
                move || {
                    *state.borrow_mut() = s.clone();
                    on_change(s.clone());
                    popover.popdown();
                },
            ));
        });
    }
    {
        let typed = typed.clone();
        let state = state.clone();
        let on_change = on_change.clone();
        let popover = popover.clone();
        search.connect_activate(move |_| {
            if let Some(s) = typed.borrow().clone() {
                *state.borrow_mut() = s.clone();
                on_change(s);
                popover.popdown();
            }
        });
    }

    stack.add_named(&main, Some("main"));

    // The full calendar.
    let calendar_page = gtk::Box::new(gtk::Orientation::Vertical, 6);
    calendar_page.set_margin_top(6);
    calendar_page.set_margin_bottom(9);
    calendar_page.set_margin_start(9);
    calendar_page.set_margin_end(9);
    {
        let stack = stack.clone();
        calendar_page.append(&page_header("Choose a date", move || {
            stack.set_visible_child_name("main")
        }));
    }
    let calendar = gtk::Calendar::new();
    if let Some(d) = state
        .borrow()
        .due
        .as_ref()
        .and_then(|d| from_date(d.local_date(zone)))
    {
        calendar.set_date(&d);
    }
    {
        let pick_day = pick_day.clone();
        calendar.connect_day_selected(move |c| {
            if let Some(d) = to_date(&c.date()) {
                pick_day(d);
            }
        });
    }
    calendar_page.append(&calendar);
    stack.add_named(&calendar_page, Some("calendar"));

    if opts.full {
        // Time.
        let time_page = gtk::Box::new(gtk::Orientation::Vertical, 6);
        time_page.set_margin_top(6);
        time_page.set_margin_bottom(9);
        time_page.set_margin_start(9);
        time_page.set_margin_end(9);
        {
            let stack = stack.clone();
            time_page.append(&page_header("Time", move || {
                stack.set_visible_child_name("main")
            }));
        }
        let entry = gtk::Entry::builder()
            .placeholder_text("5pm, 17:30")
            .activates_default(false)
            .build();
        time_page.append(&entry);
        let times = chip_box();
        let set_time: Rc<dyn Fn(NaiveTime)> = {
            let state = state.clone();
            let apply = apply.clone();
            let stack = stack.clone();
            Rc::new(move |t| {
                let mut s = state.borrow().clone();
                s.due = Some(model::at_time(s.due.as_ref(), t, today, zone));
                apply(s);
                stack.set_visible_child_name("main");
            })
        };
        for (text, h) in [
            ("9am", 9),
            ("12pm", 12),
            ("3pm", 15),
            ("6pm", 18),
            ("8pm", 20),
        ] {
            let set_time = set_time.clone();
            times.append(&suggestion("clock-symbolic", text, move || {
                set_time(NaiveTime::from_hms_opt(h, 0, 0).expect("an hour"))
            }));
        }
        time_page.append(&times);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        let error = ui::label("", &["error", "caption"]);
        error.set_visible(false);
        time_page.append(&error);
        time_page.append(&save);
        let submit = {
            let entry = entry.clone();
            let error = error.clone();
            let set_time = set_time.clone();
            move || match model::parse_time(&entry.text()) {
                Some(t) => {
                    error.set_visible(false);
                    set_time(t);
                }
                None => {
                    error.set_text("Try 5pm or 17:30");
                    error.set_visible(true);
                }
            }
        };
        {
            let submit = submit.clone();
            entry.connect_activate(move |_| submit());
        }
        save.connect_clicked(move |_| submit());
        stack.add_named(&time_page, Some("time"));
        {
            let stack = stack.clone();
            let entry = entry.clone();
            let state = state.clone();
            time_row.button.connect_clicked(move |_| {
                let current = state
                    .borrow()
                    .due
                    .as_ref()
                    .filter(|d| d.has_time())
                    .map(|d| fmt::time_label(d.instant(zone).with_timezone(&zone).time()));
                entry.set_text(current.as_deref().unwrap_or(""));
                stack.set_visible_child_name("time");
                entry.grab_focus();
            });
        }
        {
            let state = state.clone();
            let apply = apply.clone();
            time_row.clear.connect_clicked(move |_| {
                let mut s = state.borrow().clone();
                s.due = s.due.map(|d| When::Date {
                    date: d.local_date(zone),
                });
                apply(s);
            });
        }

        // Repeat.
        let repeat_page = repeat_pages(&stack, state.clone(), apply.clone(), opts, "main");
        let _ = repeat_page;
        {
            let stack = stack.clone();
            repeat_row
                .button
                .connect_clicked(move |_| stack.set_visible_child_name("repeat"));
        }
        {
            let state = state.clone();
            let apply = apply.clone();
            repeat_row.clear.connect_clicked(move |_| {
                let mut s = state.borrow().clone();
                s.rrule = None;
                apply(s);
            });
        }
    }
    refresh();

    {
        let stack = stack.clone();
        let search = search.clone();
        popover.connect_map(move |p| {
            stack.set_visible_child_name("main");
            focus_in(p, &search);
        });
    }
    popover
}

/// Up to three weeks from today, laid out under weekday initials.
fn week_grid(
    today: NaiveDate,
    sunday_first: bool,
    days: &Rc<RefCell<Vec<(NaiveDate, gtk::Button)>>>,
    pick: Rc<dyn Fn(NaiveDate)>,
) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .row_spacing(2)
        .build();
    let order: Vec<chrono::Weekday> = if sunday_first {
        std::iter::once(chrono::Weekday::Sun)
            .chain(WEEK.iter().copied().take(6))
            .collect()
    } else {
        WEEK.to_vec()
    };
    for (col, wd) in order.iter().enumerate() {
        let l = gtk::Label::new(Some(&wd.to_string()[..2]));
        l.add_css_class("caption");
        l.add_css_class("dim-label");
        grid.attach(&l, col as i32, 0, 1, 1);
    }
    let start_col = order
        .iter()
        .position(|w| *w == today.weekday())
        .unwrap_or(0);
    let mut out = days.borrow_mut();
    for i in 0..21 {
        let d = today + Duration::days(i);
        let slot = start_col + i as usize;
        let (row, col) = (slot / 7 + 1, slot % 7);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if d.day() == 1 {
            let m = gtk::Label::new(Some(&d.format("%b").to_string()));
            m.add_css_class("caption");
            m.add_css_class("dim-label");
            content.append(&m);
        }
        content.append(&gtk::Label::new(Some(&d.day().to_string())));
        let b = gtk::Button::builder().child(&content).build();
        b.add_css_class("flat");
        b.add_css_class("calendar-day");
        if i == 0 {
            b.add_css_class("today");
        }
        b.set_tooltip_text(Some(&d.format("%A, %B %-d").to_string()));
        let pick = pick.clone();
        b.connect_clicked(move |_| pick(d));
        grid.attach(&b, col as i32, row as i32, 1, 1);
        out.push((d, b));
    }
    grid
}

/// The "repeat" presets page and the "custom" editor page, added to `stack`;
/// `back` is the page to return to.
fn repeat_pages(
    stack: &gtk::Stack,
    state: Rc<RefCell<Schedule>>,
    apply: Rc<dyn Fn(Schedule)>,
    opts: DateOpts,
    back: &'static str,
) -> gtk::Box {
    let now = now();
    let zone = now.timezone();
    let today = now.date_naive();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.set_margin_top(6);
    page.set_margin_bottom(6);
    page.set_margin_start(6);
    page.set_margin_end(6);
    if back != "repeat" {
        let stack = stack.clone();
        page.append(&page_header("Repeat", move || {
            stack.set_visible_child_name(back)
        }));
    }
    let set_rule: Rc<dyn Fn(Option<String>)> = {
        let state = state.clone();
        let apply = apply.clone();
        let stack = stack.clone();
        Rc::new(move |rule| {
            let mut s = state.borrow().clone();
            // A repeat needs a date to count from.
            if rule.is_some() && s.due.is_none() {
                s.due = Some(When::Date { date: today });
            }
            s.rrule = rule;
            apply(s);
            if back != "repeat" {
                stack.set_visible_child_name(back);
            }
        })
    };
    let current = state.borrow().rrule.clone();
    for (name, rule) in REPEAT_PRESETS {
        let set_rule = set_rule.clone();
        let checked = current
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(rule));
        page.append(
            &Item::new(None, name)
                .checked(checked)
                .stay()
                .build(move || set_rule(Some(rule.to_string()))),
        );
    }
    {
        let set_rule = set_rule.clone();
        page.append(
            &Item::new(None, "Never")
                .checked(current.is_none())
                .stay()
                .build(move || set_rule(None)),
        );
    }
    page.append(&ui::separator());

    let custom_page = gtk::Box::new(gtk::Orientation::Vertical, 6);
    custom_page.set_margin_top(6);
    custom_page.set_margin_bottom(9);
    custom_page.set_margin_start(9);
    custom_page.set_margin_end(9);
    {
        let stack = stack.clone();
        custom_page.append(&page_header("Custom repeat", move || {
            stack.set_visible_child_name("repeat")
        }));
    }
    let editor_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    custom_page.append(&editor_slot);
    {
        let stack = stack.clone();
        let state = state.clone();
        let set_rule = set_rule.clone();
        let editor_slot = editor_slot.clone();
        let custom = Item::new(Some("settings-symbolic"), "Custom…")
            .arrow()
            .build(move || {
                let s = state.borrow().clone();
                let timed = s.due.as_ref().is_some_and(When::has_time);
                let initial = s
                    .rrule
                    .as_deref()
                    .and_then(Repeat::parse)
                    .unwrap_or_else(|| Repeat {
                        freq: Freq::Weekly,
                        days: vec![
                            s.due
                                .as_ref()
                                .map_or(today, |d| d.local_date(zone))
                                .weekday(),
                        ],
                        ..Repeat::default()
                    });
                ui::clear(&editor_slot);
                let set_rule = set_rule.clone();
                editor_slot.append(&repeat_editor(
                    initial,
                    timed,
                    opts.sunday_first,
                    Rc::new(move |rule| set_rule(Some(rule))),
                ));
                stack.set_visible_child_name("custom");
            });
        // A rule the editor can't show (BYMONTHDAY lists, nth weekdays) opens as
        // weekly and Apply would overwrite it.
        custom.set_sensitive(
            current
                .as_deref()
                .is_none_or(|c| Repeat::parse(c).is_some()),
        );
        page.append(&custom);
    }
    stack.add_named(&page, Some("repeat"));
    stack.add_named(&custom_page, Some("custom"));
    page
}

/// Every N units, on these weekdays, until a date or for N times.
fn repeat_editor(
    initial: Repeat,
    timed: bool,
    sunday_first: bool,
    on_apply: Rc<dyn Fn(String)>,
) -> gtk::Box {
    let state = Rc::new(RefCell::new(initial.clone()));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let preview = ui::label("", &["dim-label"]);
    preview.set_wrap(true);
    root.append(&preview);
    let update_preview = {
        let state = state.clone();
        let preview = preview.clone();
        move || {
            preview.set_text(&capitalize(&fmt::repeat_label(
                &state.borrow().to_rule(timed),
            )))
        }
    };

    root.append(&ui::label("Every", &["caption-heading"]));
    let every = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let interval = gtk::SpinButton::with_range(1.0, 99.0, 1.0);
    interval.set_value(f64::from(initial.interval));
    let units = gtk::StringList::new(&["day", "week", "month", "year"]);
    let unit = gtk::DropDown::builder().model(&units).hexpand(true).build();
    unit.set_selected(
        Freq::ALL
            .iter()
            .position(|f| *f == initial.freq)
            .unwrap_or(0) as u32,
    );
    every.append(&interval);
    every.append(&unit);
    root.append(&every);

    let weekdays = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(3)
        .homogeneous(true)
        .build();
    let order: Vec<chrono::Weekday> = if sunday_first {
        std::iter::once(chrono::Weekday::Sun)
            .chain(WEEK.iter().copied().take(6))
            .collect()
    } else {
        WEEK.to_vec()
    };
    for wd in order {
        let t = gtk::ToggleButton::with_label(&wd.to_string()[..2]);
        t.add_css_class("day-toggle");
        t.add_css_class("circular");
        t.set_active(initial.days.contains(&wd));
        let state = state.clone();
        let update_preview = update_preview.clone();
        t.connect_toggled(move |t| {
            let mut s = state.borrow_mut();
            s.days.retain(|d| *d != wd);
            if t.is_active() {
                s.days.push(wd);
            }
            drop(s);
            update_preview();
        });
        weekdays.append(&t);
    }
    let weekdays_revealer = gtk::Revealer::builder()
        .child(&weekdays)
        .reveal_child(initial.freq == Freq::Weekly)
        .build();
    root.append(&weekdays_revealer);

    let set_units = {
        let units = units.clone();
        move |n: u32| {
            let words: [&str; 4] = if n == 1 {
                ["day", "week", "month", "year"]
            } else {
                ["days", "weeks", "months", "years"]
            };
            for (i, w) in words.iter().enumerate() {
                if units.string(i as u32).as_deref() != Some(*w) {
                    units.splice(i as u32, 1, &[w]);
                }
            }
        }
    };
    set_units(initial.interval);
    {
        let state = state.clone();
        let update_preview = update_preview.clone();
        let unit = unit.clone();
        interval.connect_value_changed(move |s| {
            let n = s.value_as_int().max(1) as u32;
            state.borrow_mut().interval = n;
            let selected = unit.selected();
            set_units(n);
            unit.set_selected(selected);
            update_preview();
        });
    }
    {
        let state = state.clone();
        let update_preview = update_preview.clone();
        let weekdays_revealer = weekdays_revealer.clone();
        unit.connect_selected_notify(move |d| {
            let freq = Freq::ALL[(d.selected() as usize).min(3)];
            state.borrow_mut().freq = freq;
            weekdays_revealer.set_reveal_child(freq == Freq::Weekly);
            update_preview();
        });
    }

    root.append(&ui::label("Ends", &["caption-heading"]));
    let ends = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    ends.add_css_class("linked");
    let never = gtk::ToggleButton::with_label("Never");
    let on = gtk::ToggleButton::with_label("On date");
    let after = gtk::ToggleButton::with_label("After");
    on.set_group(Some(&never));
    after.set_group(Some(&never));
    for b in [&never, &on, &after] {
        b.set_hexpand(true);
        ends.append(b);
    }
    root.append(&ends);

    let end_stack = gtk::Stack::builder().vhomogeneous(false).build();
    end_stack.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("never"));
    let today = now().date_naive();
    let until_date = Rc::new(RefCell::new(match initial.end {
        End::Until(d) => d,
        _ => today + Duration::days(30),
    }));
    let until_calendar = gtk::Calendar::new();
    if let Some(d) = from_date(*until_date.borrow()) {
        until_calendar.set_date(&d);
    }
    end_stack.add_named(&until_calendar, Some("on"));
    let count_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let count = gtk::SpinButton::with_range(1.0, 999.0, 1.0);
    count.set_value(match initial.end {
        End::Count(n) => f64::from(n),
        _ => 5.0,
    });
    count_box.append(&count);
    count_box.append(&ui::label("times", &[]));
    end_stack.add_named(&count_box, Some("after"));
    root.append(&end_stack);

    let set_end = {
        let state = state.clone();
        let end_stack = end_stack.clone();
        let (never, on, count) = (never.clone(), on.clone(), count.clone());
        let until_date = until_date.clone();
        let update_preview = update_preview.clone();
        move || {
            let end = if never.is_active() {
                end_stack.set_visible_child_name("never");
                End::Never
            } else if on.is_active() {
                end_stack.set_visible_child_name("on");
                End::Until(*until_date.borrow())
            } else {
                end_stack.set_visible_child_name("after");
                End::Count(count.value_as_int().max(1) as u32)
            };
            state.borrow_mut().end = end;
            update_preview();
        }
    };
    match initial.end {
        End::Never => never.set_active(true),
        End::Until(_) => on.set_active(true),
        End::Count(_) => after.set_active(true),
    }
    set_end();
    for b in [&never, &on, &after] {
        let set_end = set_end.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                set_end();
            }
        });
    }
    {
        let set_end = set_end.clone();
        count.connect_value_changed(move |_| set_end());
    }
    {
        let set_end = set_end.clone();
        until_calendar.connect_day_selected(move |c| {
            if let Some(d) = to_date(&c.date()) {
                *until_date.borrow_mut() = d;
                set_end();
            }
        });
    }

    let done = gtk::Button::with_label("Apply");
    done.add_css_class("suggested-action");
    done.set_margin_top(6);
    {
        let state = state.clone();
        done.connect_clicked(move |_| on_apply(state.borrow().to_rule(timed)));
    }
    root.append(&done);
    update_preview();
    root
}

// -- priority ---------------------------------------------------------------

pub fn priority_popover(current: u8, on_pick: impl Fn(u8) + 'static) -> gtk::Popover {
    let on_pick = Rc::new(on_pick);
    let items: Vec<gtk::Widget> = (1..=4)
        .map(|level| {
            let on_pick = on_pick.clone();
            let tint = match level {
                1 => "priority-1-icon",
                2 => "priority-2-icon",
                3 => "priority-3-icon",
                _ => "priority-4-icon",
            };
            Item::new(
                Some("flag-outline-thick-symbolic"),
                model::priority_name(level),
            )
            .tint(tint)
            .secondary(&format!("p{level}"))
            .checked(level == current)
            .build(move || on_pick(level))
            .upcast()
        })
        .collect();
    ui::menu(&items)
}

// -- lists ------------------------------------------------------------------

pub fn list_popover(
    lists: &[ListView],
    current: Option<&str>,
    on_pick: impl Fn(String) + 'static,
) -> gtk::Popover {
    let on_pick = Rc::new(on_pick);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(9);
    root.set_margin_bottom(9);
    root.set_margin_start(9);
    root.set_margin_end(9);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Move to…")
        .build();
    root.append(&search);
    let listbox = gtk::ListBox::new();
    listbox.add_css_class("picker-list");
    listbox.set_selection_mode(gtk::SelectionMode::None);
    let mut hrefs = Vec::new();
    for l in lists.iter().filter(|l| l.writable) {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        line.set_margin_start(6);
        line.set_margin_end(6);
        line.append(&ui::ring(l.color.as_deref(), 14));
        let name = ui::label(&l.name, &[]);
        name.set_hexpand(true);
        line.append(&name);
        if Some(l.href.as_str()) == current {
            line.append(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        row.set_child(Some(&line));
        row.set_widget_name(&l.name.to_lowercase());
        listbox.append(&row);
        hrefs.push(l.href.clone());
    }
    let scroller = gtk::ScrolledWindow::builder()
        .child(&listbox)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(320)
        .build();
    root.append(&scroller);
    let popover = ui::popover(&root);
    popover.set_width_request(260);
    let hrefs = Rc::new(hrefs);
    {
        let (hrefs, on_pick, popover) = (hrefs.clone(), on_pick.clone(), popover.clone());
        listbox.connect_row_activated(move |_, row| {
            if let Some(h) = hrefs.get(row.index() as usize) {
                popover.popdown();
                on_pick(h.clone());
            }
        });
    }
    {
        let listbox = listbox.clone();
        search.connect_search_changed(move |e| {
            let q = e.text().to_lowercase();
            let mut i = 0;
            while let Some(row) = listbox.row_at_index(i) {
                row.set_visible(row.widget_name().contains(q.as_str()));
                i += 1;
            }
        });
    }
    {
        let listbox = listbox.clone();
        search.connect_activate(move |_| {
            let mut i = 0;
            while let Some(row) = listbox.row_at_index(i) {
                if row.is_visible() {
                    row.activate();
                    return;
                }
                i += 1;
            }
        });
    }
    {
        let search = search.clone();
        popover.connect_map(move |p| {
            search.set_text("");
            focus_in(p, &search);
        });
    }
    popover
}

// -- link -------------------------------------------------------------------

/// The task's link: type or paste one, open it, or take it off. What is
/// typed is kept when the popover closes.
pub fn link_popover(
    current: Option<&str>,
    on_change: impl Fn(Option<String>) + 'static,
) -> gtk::Popover {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 9);
    root.set_margin_top(9);
    root.set_margin_bottom(9);
    root.set_margin_start(9);
    root.set_margin_end(9);
    let entry = gtk::Entry::builder()
        .placeholder_text("https://…")
        .text(current.unwrap_or(""))
        .input_purpose(gtk::InputPurpose::Url)
        .hexpand(true)
        .build();
    entry.set_icon_from_icon_name(
        gtk::EntryIconPosition::Primary,
        Some("chain-link-loose-symbolic"),
    );
    root.append(&entry);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let open = gtk::Button::with_label("Open");
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("flat");
    remove.set_hexpand(true);
    remove.set_halign(gtk::Align::End);
    buttons.append(&open);
    buttons.append(&remove);
    root.append(&buttons);

    let popover = ui::popover(&root);
    popover.set_width_request(300);
    let has_text = {
        let (open, remove) = (open.clone(), remove.clone());
        move |e: &gtk::Entry| {
            let some = !e.text().trim().is_empty();
            open.set_sensitive(some);
            remove.set_sensitive(some);
        }
    };
    has_text(&entry);
    entry.connect_changed(has_text);
    {
        let popover = popover.clone();
        entry.connect_activate(move |_| popover.popdown());
    }
    {
        let (entry, popover) = (entry.clone(), popover.clone());
        remove.connect_clicked(move |_| {
            entry.set_text("");
            popover.popdown();
        });
    }
    {
        let (entry, popover) = (entry.clone(), popover.clone());
        open.connect_clicked(move |b| {
            let text = entry.text().trim().to_string();
            let url = if text.contains("://") || text.starts_with("mailto:") {
                text
            } else {
                format!("https://{text}")
            };
            let parent = b.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&url).launch(
                parent.as_ref(),
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
            popover.popdown();
        });
    }
    {
        let entry = entry.clone();
        let current = current.map(str::to_string);
        popover.connect_closed(move |_| {
            let new = Some(entry.text().trim().to_string()).filter(|s| !s.is_empty());
            if new != current {
                on_change(new);
            }
        });
    }
    {
        let entry = entry.clone();
        popover.connect_map(move |p| focus_in(p, &entry));
    }
    popover
}

// -- reminders --------------------------------------------------------------

pub fn reminder_popover(task: &Task, on_change: impl Fn(Vec<Trigger>) + 'static) -> gtk::Popover {
    let now = now();
    let zone = now.timezone();
    let today = now.date_naive();
    let task = task.clone();
    let triggers: Rc<RefCell<Vec<Trigger>>> = Rc::new(RefCell::new(
        task.alarms.iter().map(|a| a.trigger.clone()).collect(),
    ));
    let on_change: Rc<dyn Fn(Vec<Trigger>)> = Rc::new(on_change);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .vhomogeneous(false)
        .interpolate_size(true)
        .build();
    let popover = ui::popover(&stack);
    popover.set_width_request(280);
    popover.add_css_class("picker");

    let main = gtk::Box::new(gtk::Orientation::Vertical, 3);
    main.set_margin_top(9);
    main.set_margin_bottom(6);
    main.set_margin_start(6);
    main.set_margin_end(6);
    let title = ui::heading("Reminders");
    title.set_margin_start(6);
    title.set_margin_bottom(3);
    main.append(&title);
    let current = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.append(&current);

    // Set once the list exists; removing a reminder redraws it.
    let rebuild: Rc<RefCell<Option<Redraw>>> = Rc::default();
    let set: Rc<dyn Fn(Vec<Trigger>)> = {
        let triggers = triggers.clone();
        let on_change = on_change.clone();
        let rebuild = rebuild.clone();
        Rc::new(move |v| {
            *triggers.borrow_mut() = v.clone();
            on_change(v);
            if let Some(r) = rebuild.borrow().as_ref() {
                r();
            }
        })
    };
    *rebuild.borrow_mut() = Some({
        let triggers = triggers.clone();
        let current = current.clone();
        let task = task.clone();
        let set = set.clone();
        Rc::new(move || {
            ui::clear(&current);
            let mut list = triggers.borrow().clone();
            list.sort_by_key(|t| model::trigger_at(t, &task, zone));
            if list.is_empty() {
                let none = ui::label("No reminders yet", &["dim-label"]);
                none.set_margin_start(6);
                none.set_margin_top(3);
                none.set_margin_bottom(3);
                current.append(&none);
            }
            for t in list {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                line.set_margin_start(6);
                line.append(&gtk::Image::from_icon_name("alarm-symbolic"));
                let text = ui::label(&model::reminder_label(&t, &task, now), &[]);
                text.set_hexpand(true);
                line.append(&text);
                let remove = gtk::Button::from_icon_name("cross-large-circle-filled-symbolic");
                remove.add_css_class("flat");
                remove.add_css_class("circular");
                remove.set_tooltip_text(Some("Remove"));
                let (set, triggers) = (set.clone(), triggers.clone());
                remove.connect_clicked(move |_| {
                    let mut v = triggers.borrow().clone();
                    v.retain(|x| *x != t);
                    set(v);
                });
                line.append(&remove);
                current.append(&line);
            }
        })
    });
    if let Some(r) = rebuild.borrow().as_ref() {
        r();
    }

    main.append(&ui::separator());
    let add: Rc<dyn Fn(Trigger)> = {
        let triggers = triggers.clone();
        let set = set.clone();
        Rc::new(move |t| {
            let mut v = triggers.borrow().clone();
            if !v.contains(&t) {
                v.push(t);
                set(v);
            }
        })
    };
    let mut quick: Vec<(String, Trigger)> = Vec::new();
    match &task.due {
        Some(d) if d.has_time() => {
            for (text, mins) in [
                ("At due time", 0),
                ("5 min before", 5),
                ("15 min before", 15),
                ("30 min before", 30),
                ("1 h before", 60),
                ("1 day before", 1440),
            ] {
                quick.push((
                    text.into(),
                    Trigger::Relative {
                        offset: Duration::minutes(-mins),
                        from_due: true,
                    },
                ));
            }
        }
        Some(_) => {
            for (text, days) in [("On the day, 9am", 0), ("The day before, 9am", 1)] {
                quick.push((
                    text.into(),
                    Trigger::Relative {
                        offset: Duration::days(-days),
                        from_due: true,
                    },
                ));
            }
        }
        None => {}
    }
    let in_hour = now + Duration::hours(1);
    let in_hour = in_hour
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(in_hour);
    quick.push((
        "In 1 hour".into(),
        Trigger::Absolute {
            at: in_hour.with_timezone(&Utc),
        },
    ));
    if let Some(nine) = NaiveTime::from_hms_opt(9, 0, 0) {
        quick.push((
            "Tomorrow, 9am".into(),
            Trigger::Absolute {
                at: asst_core::time::resolve(zone, (today + Duration::days(1)).and_time(nine)),
            },
        ));
    }
    for (text, trigger) in quick {
        // "At due time" is already there when an alarm rings then.
        let at = model::trigger_at(&trigger, &task, zone);
        if at.is_some()
            && triggers
                .borrow()
                .iter()
                .any(|t| model::trigger_at(t, &task, zone) == at)
        {
            continue;
        }
        let add = add.clone();
        main.append(
            &Item::new(Some("plus-large-symbolic"), &text)
                .stay()
                .build(move || add(trigger.clone())),
        );
    }
    {
        let stack = stack.clone();
        main.append(
            &Item::new(Some("month-symbolic"), "Pick a date and time")
                .arrow()
                .build(move || stack.set_visible_child_name("custom")),
        );
    }
    stack.add_named(&main, Some("main"));

    let custom = gtk::Box::new(gtk::Orientation::Vertical, 6);
    custom.set_margin_top(6);
    custom.set_margin_bottom(9);
    custom.set_margin_start(9);
    custom.set_margin_end(9);
    {
        let stack = stack.clone();
        custom.append(&page_header("Remind me", move || {
            stack.set_visible_child_name("main")
        }));
    }
    let calendar = gtk::Calendar::new();
    if let Some(d) = task
        .due
        .as_ref()
        .and_then(|d| from_date(d.local_date(zone)))
    {
        calendar.set_date(&d);
    }
    custom.append(&calendar);
    let time = gtk::Entry::builder()
        .placeholder_text("9am")
        .text("9am")
        .build();
    custom.append(&time);
    let error = ui::label("", &["error", "caption"]);
    error.set_visible(false);
    custom.append(&error);
    let submit = gtk::Button::with_label("Add Reminder");
    submit.add_css_class("suggested-action");
    custom.append(&submit);
    let go = {
        let (calendar, time, error, stack, add) = (
            calendar.clone(),
            time.clone(),
            error.clone(),
            stack.clone(),
            add.clone(),
        );
        move || {
            let Some(t) = model::parse_time(&time.text()) else {
                error.set_text("Try 9am or 17:30");
                error.set_visible(true);
                return;
            };
            let Some(d) = to_date(&calendar.date()) else {
                return;
            };
            let at = asst_core::time::resolve(zone, d.and_time(t));
            if at <= Utc::now() {
                error.set_text("Choose a time in the future");
                error.set_visible(true);
                return;
            }
            error.set_visible(false);
            add(Trigger::Absolute { at });
            stack.set_visible_child_name("main");
        }
    };
    {
        let go = go.clone();
        time.connect_activate(move |_| go());
    }
    submit.connect_clicked(move |_| go());
    stack.add_named(&custom, Some("custom"));
    {
        let stack = stack.clone();
        popover.connect_map(move |_| stack.set_visible_child_name("main"));
    }
    popover
}
