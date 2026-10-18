//! The window's reading of the daemon's data: the views, their sections,
//! the sidebar counts, and the date and repeat arithmetic behind the
//! pickers. No GTK here, so it is tested like the core.

use std::collections::{HashMap, HashSet};

use asst_core::api::{Change, LinkView, ListView, StatusView, SyncState, TaskView};
use asst_core::fmt;
use asst_core::task::{Task, priority_level};
use asst_core::time::{Trigger, When, resolve, weekday_code};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

// -- views ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Nav {
    Inbox,
    Today,
    /// One day on an hour timeline: tracked time on the left, the plan on
    /// the right, one cursor and one zoom.
    Day,
    /// Eisenhower's four boxes: important × urgent.
    Matrix,
    Scheduled,
    Completed,
    Tomorrow,
    Anytime,
    Repeating,
    All,
    List(String),
}

impl Nav {
    /// The smart views, in sidebar order.
    pub fn filters() -> [Nav; 10] {
        [
            Nav::Inbox,
            Nav::Today,
            Nav::Day,
            Nav::Matrix,
            Nav::Scheduled,
            Nav::Completed,
            Nav::Tomorrow,
            Nav::Anytime,
            Nav::Repeating,
            Nav::All,
        ]
    }

    pub fn title(&self, lists: &[ListView]) -> String {
        match self {
            Nav::Inbox => "Inbox".into(),
            Nav::Today => "Today".into(),
            Nav::Day => "Day".into(),
            Nav::Matrix => "Matrix".into(),
            Nav::Scheduled => "Scheduled".into(),
            Nav::Completed => "Completed".into(),
            Nav::Tomorrow => "Tomorrow".into(),
            Nav::Anytime => "Anytime".into(),
            Nav::Repeating => "Repeating".into(),
            Nav::All => "All Tasks".into(),
            Nav::List(href) => lists
                .iter()
                .find(|l| &l.href == href)
                .map_or_else(|| "List".into(), |l| l.name.clone()),
        }
    }

    /// Other words Quick Find knows a view by.
    pub fn keywords(&self) -> &'static [&'static str] {
        match self {
            Nav::Today => &["overdue"],
            Nav::Day => &[
                "timebox", "day plan", "agenda", "timeline", "schedule", "tracked", "activity",
                "blocks", "plan", "time",
            ],
            Nav::Matrix => &["eisenhower", "urgent", "important", "quadrant"],
            Nav::Scheduled => &["upcoming", "later"],
            Nav::Completed => &["done", "logbook", "finished"],
            Nav::Anytime => &["no date", "someday", "unscheduled"],
            Nav::Repeating => &["recurring"],
            Nav::All => &["everything"],
            Nav::Inbox | Nav::Tomorrow | Nav::List(_) => &[],
        }
    }

    /// Where a due-date filter makes sense: views not made of dates already.
    pub fn filters_by_date(&self) -> bool {
        matches!(self, Nav::Inbox | Nav::List(_) | Nav::All | Nav::Repeating)
    }

    /// What a smart view holds, in a few words.
    pub fn about(&self) -> &'static str {
        match self {
            Nav::Inbox => "The inbox list",
            Nav::Today => "Due today, and overdue",
            Nav::Day => "Tracked time and the plan, side by side",
            Nav::Matrix => "Important and urgent, in four boxes",
            Nav::Scheduled => "Everything with a date, by day",
            Nav::Completed => "Done, by the day",
            Nav::Tomorrow => "Due tomorrow",
            Nav::Anytime => "Tasks with no date",
            Nav::Repeating => "Tasks that repeat",
            Nav::All => "Every open task, by list",
            Nav::List(_) => "A list",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Nav::Inbox => "mailbox-symbolic",
            Nav::Today => "star-outline-thick-symbolic",
            Nav::Day => "work-week-symbolic",
            Nav::Matrix => "flag-outline-thick-symbolic",
            Nav::Scheduled => "month-symbolic",
            Nav::Completed => "check-round-outline-symbolic",
            Nav::Tomorrow => "today-calendar-symbolic",
            Nav::Anytime => "grid-large-symbolic",
            Nav::Repeating => "arrow-circular-top-right-symbolic",
            Nav::All => "check-round-outline-whole-symbolic",
            Nav::List(_) => "list-large-symbolic",
        }
    }

    /// The CSS class that colors its tile and its title icon.
    pub fn tint(&self) -> &'static str {
        match self {
            Nav::Inbox | Nav::All | Nav::List(_) => "tint-blue",
            Nav::Today | Nav::Day => "tint-green",
            Nav::Matrix => "tint-orange",
            Nav::Scheduled | Nav::Tomorrow | Nav::Anytime | Nav::Repeating => "tint-purple",
            Nav::Completed => "tint-orange",
        }
    }

    /// A stable name for settings: `today`, `list:/cal/x/`.
    pub fn key(&self) -> String {
        match self {
            Nav::Inbox => "inbox".into(),
            Nav::Today => "today".into(),
            Nav::Day => "day".into(),
            Nav::Matrix => "matrix".into(),
            Nav::Scheduled => "scheduled".into(),
            Nav::Completed => "completed".into(),
            Nav::Tomorrow => "tomorrow".into(),
            Nav::Anytime => "anytime".into(),
            Nav::Repeating => "repeating".into(),
            Nav::All => "all".into(),
            Nav::List(href) => format!("list:{href}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Nav> {
        if let Some(href) = key.strip_prefix("list:") {
            return Some(Nav::List(href.to_string()));
        }
        Nav::filters().into_iter().find(|n| n.key() == key)
    }

    /// Where a task added here goes: a list, a date, or neither.
    pub fn is_list(&self) -> bool {
        matches!(self, Nav::Inbox | Nav::List(_))
    }

    fn default_sort(&self) -> Sort {
        match self {
            Nav::Inbox | Nav::List(_) => Sort::Custom,
            _ => Sort::Due,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sort {
    #[default]
    Custom,
    Due,
    Priority,
    Name,
    Added,
    Modified,
}

impl Sort {
    pub const ALL: [Sort; 6] = [
        Sort::Custom,
        Sort::Due,
        Sort::Priority,
        Sort::Name,
        Sort::Added,
        Sort::Modified,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Custom => "Custom order",
            Sort::Due => "Due date",
            Sort::Priority => "Priority",
            Sort::Name => "Name",
            Sort::Added => "Date added",
            Sort::Modified => "Date modified",
        }
    }
}

/// Which due dates a view shows. Each span takes in what is overdue too:
/// "due this week" is everything to be done by its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DueFilter {
    #[default]
    Any,
    Today,
    Week,
    Seven,
    Month,
    NoDate,
}

impl DueFilter {
    pub const ALL: [DueFilter; 6] = [
        DueFilter::Any,
        DueFilter::Today,
        DueFilter::Week,
        DueFilter::Seven,
        DueFilter::Month,
        DueFilter::NoDate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DueFilter::Any => "Any date",
            DueFilter::Today => "Due today",
            DueFilter::Week => "Due this week",
            DueFilter::Seven => "Due in the next 7 days",
            DueFilter::Month => "Due this month",
            DueFilter::NoDate => "No date",
        }
    }

    pub fn keeps(self, due: Option<NaiveDate>, today: NaiveDate, sunday_first: bool) -> bool {
        let by = |last: NaiveDate| due.is_some_and(|d| d <= last);
        match self {
            DueFilter::Any => true,
            DueFilter::NoDate => due.is_none(),
            DueFilter::Today => by(today),
            DueFilter::Week => {
                let first = if sunday_first {
                    Weekday::Sun
                } else {
                    Weekday::Mon
                };
                let into =
                    (7 + today.weekday().num_days_from_monday() - first.num_days_from_monday()) % 7;
                by(today + Duration::days(6 - i64::from(into)))
            }
            DueFilter::Seven => by(today + Duration::days(6)),
            DueFilter::Month => by(last_day(today.with_day(1).unwrap_or(today))),
        }
    }
}

/// How one view is shown, remembered per view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewOpts {
    /// Unset: the view's own default.
    pub sort: Option<Sort>,
    /// Last first, for any order but the custom one.
    pub descending: bool,
    pub show_completed: bool,
    /// Priorities 1–4 shown.
    pub priorities: [bool; 4],
    pub due: DueFilter,
    /// Lists left out (hrefs): Completed can show just some.
    pub hide_lists: Vec<String>,
}

impl Default for ViewOpts {
    fn default() -> ViewOpts {
        ViewOpts {
            sort: None,
            descending: false,
            show_completed: false,
            priorities: [true; 4],
            due: DueFilter::Any,
            hide_lists: Vec::new(),
        }
    }
}

impl ViewOpts {
    pub fn sort_for(&self, nav: &Nav) -> Sort {
        self.sort.unwrap_or_else(|| nav.default_sort())
    }

    /// Something keeps tasks out of the view.
    pub fn filtered(&self) -> bool {
        self.priorities.contains(&false)
            || self.due != DueFilter::Any
            || !self.hide_lists.is_empty()
    }
}

// -- sections ---------------------------------------------------------------

/// Eisenhower's four boxes. Importance is priority p1–p2; urgency is a due
/// date on or before today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quadrant {
    /// Important and urgent.
    Do,
    /// Important, not urgent.
    Schedule,
    /// Urgent, not important.
    Delegate,
    /// Neither.
    Drop,
}

impl Quadrant {
    pub const ALL: [Quadrant; 4] = [
        Quadrant::Do,
        Quadrant::Schedule,
        Quadrant::Delegate,
        Quadrant::Drop,
    ];

    pub fn important(self) -> bool {
        matches!(self, Quadrant::Do | Quadrant::Schedule)
    }

    pub fn urgent(self) -> bool {
        matches!(self, Quadrant::Do | Quadrant::Delegate)
    }

    pub fn title(self) -> &'static str {
        match self {
            Quadrant::Do => "Do",
            Quadrant::Schedule => "Schedule",
            Quadrant::Delegate => "Delegate",
            Quadrant::Drop => "Drop",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Quadrant::Do => "Important · Urgent",
            Quadrant::Schedule => "Important · Later",
            Quadrant::Delegate => "Not important · Urgent",
            Quadrant::Drop => "Not important · Later",
        }
    }

    /// CSS class on the quadrant card.
    pub fn css(self) -> &'static str {
        match self {
            Quadrant::Do => "quadrant-do",
            Quadrant::Schedule => "quadrant-schedule",
            Quadrant::Delegate => "quadrant-delegate",
            Quadrant::Drop => "quadrant-drop",
        }
    }
}

/// Which box a task is in: p1–p2 is important, due ≤ today is urgent.
pub fn quadrant_of(priority: u8, due: Option<&When>, today: NaiveDate, zone: Tz) -> Quadrant {
    let important = matches!(priority_level(priority), 1 | 2);
    let urgent = due.is_some_and(|d| d.local_date(zone) <= today);
    match (important, urgent) {
        (true, true) => Quadrant::Do,
        (true, false) => Quadrant::Schedule,
        (false, true) => Quadrant::Delegate,
        (false, false) => Quadrant::Drop,
    }
}

/// The edit that lands `task` in `q`. Only the axes on the wrong side move:
/// across importance, priority steps to p2 or p3; across urgency, the due
/// date becomes today or goes away (and a repeat with it).
pub fn retarget(task: &Task, q: Quadrant, today: NaiveDate, zone: Tz) -> Change {
    let mut c = Change::default();
    let is_important = matches!(priority_level(task.priority), 1 | 2);
    if q.important() != is_important {
        c.priority = Some(if q.important() { 2 } else { 3 });
    }
    let is_urgent = task
        .due
        .as_ref()
        .is_some_and(|d| d.local_date(zone) <= today);
    if q.urgent() != is_urgent {
        if q.urgent() {
            c.due = Some(Some(on_date(task.due.as_ref(), today, zone)));
        } else {
            c.due = Some(None);
            if task.rrule.is_some() {
                c.rrule = Some(None);
            }
        }
    }
    c
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Due before today; these can be moved together.
    Overdue,
    /// One day; a task added here gets that date.
    Day(NaiveDate),
    /// The rest of a month, or a whole one.
    Range,
    /// One list's tasks; a task added here goes to it.
    List(String),
    /// One of the matrix's four boxes; dropping retargets the task.
    Quadrant(Quadrant),
    Completed,
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub kind: Kind,
    pub title: Option<String>,
    /// Dim words after the title: a weekday, a range, a count.
    pub note: Option<String>,
    pub tasks: Vec<TaskView>,
    /// Shown with no tasks too, like the days of the coming week.
    pub keep: bool,
}

impl Section {
    fn new(kind: Kind, title: Option<String>, tasks: Vec<TaskView>) -> Section {
        Section {
            kind,
            title,
            note: None,
            tasks,
            keep: false,
        }
    }

    fn note(mut self, note: impl Into<String>) -> Section {
        self.note = Some(note.into());
        self
    }

    fn keep(mut self) -> Section {
        self.keep = true;
        self
    }
}

pub struct Data<'a> {
    pub lists: &'a [ListView],
    pub open: &'a [TaskView],
    /// Loaded only when a view needs it.
    pub completed: Option<&'a [TaskView]>,
    /// Where "this week" ends.
    pub sunday_first: bool,
}

pub fn inbox(lists: &[ListView]) -> Option<&ListView> {
    lists
        .iter()
        .find(|l| l.inbox)
        .or_else(|| lists.iter().find(|l| l.writable))
}

/// What a view shows, top to bottom.
pub fn plan(nav: &Nav, data: &Data, opts: &ViewOpts, now: DateTime<Tz>) -> Vec<Section> {
    let zone = now.timezone();
    let today = now.date_naive();
    let sort = opts.sort_for(nav);
    let day = |t: &TaskView| t.task.due.as_ref().map(|d| d.local_date(zone));
    let shown = |t: &TaskView| {
        opts.priorities[usize::from(priority_level(t.task.priority) - 1)]
            && opts.due.keeps(day(t), today, data.sunday_first)
            && !opts.hide_lists.contains(&t.list)
    };
    let sorted = |tasks: Vec<TaskView>, s: Sort, zone: Tz| {
        let mut v = sorted(tasks, s, zone);
        if opts.descending && s != Sort::Custom && s == sort {
            v.reverse();
        }
        v
    };
    let open: Vec<&TaskView> = data.open.iter().filter(|t| shown(t)).collect();
    let pick = |keep: &dyn Fn(&TaskView) -> bool| -> Vec<TaskView> {
        open.iter()
            .filter(|t| keep(t))
            .map(|t| (*t).clone())
            .collect()
    };
    let done = |keep: &dyn Fn(&TaskView) -> bool| -> Vec<TaskView> {
        data.completed
            .unwrap_or_default()
            .iter()
            .filter(|t| shown(t) && keep(t))
            .cloned()
            .collect()
    };
    let mut out = Vec::new();
    match nav {
        Nav::Today => {
            let overdue = sorted(
                pick(&|t| day(t).is_some_and(|d| d < today)),
                Sort::Due,
                zone,
            );
            let due = sorted(pick(&|t| day(t) == Some(today)), sort, zone);
            let has_overdue = !overdue.is_empty();
            if has_overdue {
                out.push(Section::new(Kind::Overdue, Some("Overdue".into()), overdue));
            }
            out.push(
                Section::new(Kind::Day(today), has_overdue.then(|| "Today".into()), due).keep(),
            );
            if opts.show_completed {
                let finished = done(&|t| completed_on(&t.task, zone) == Some(today));
                if !finished.is_empty() {
                    let n = finished.len();
                    out.push(
                        Section::new(Kind::Completed, Some("Completed".into()), finished)
                            .note(n.to_string()),
                    );
                }
            }
        }
        Nav::Scheduled => {
            let overdue = sorted(
                pick(&|t| day(t).is_some_and(|d| d < today)),
                Sort::Due,
                zone,
            );
            if !overdue.is_empty() {
                out.push(Section::new(Kind::Overdue, Some("Overdue".into()), overdue));
            }
            for i in 0..7 {
                let d = today + Duration::days(i);
                let note = match i {
                    0 => "Today".to_string(),
                    1 => "Tomorrow".to_string(),
                    _ => d.format("%A").to_string(),
                };
                let tasks = sorted(pick(&|t| day(t) == Some(d)), sort, zone);
                out.push(
                    Section::new(Kind::Day(d), Some(d.day().to_string()), tasks)
                        .note(note)
                        .keep(),
                );
            }
            let week_end = today + Duration::days(6);
            let later = sorted(pick(&|t| day(t).is_some_and(|d| d > week_end)), sort, zone);
            let mut months: Vec<((i32, u32), Vec<TaskView>)> = Vec::new();
            for t in later {
                let d = day(&t).expect("has a date");
                let key = (d.year(), d.month());
                match months.last_mut() {
                    Some((k, tasks)) if *k == key => tasks.push(t),
                    _ => months.push((key, vec![t])),
                }
            }
            for ((year, month), tasks) in months {
                let first = NaiveDate::from_ymd_opt(year, month, 1).expect("a month");
                let mut title = first.format("%B").to_string();
                if year != today.year() {
                    title = format!("{title} {year}");
                }
                let mut section = Section::new(Kind::Range, Some(title), tasks);
                if (year, month) == (week_end.year(), week_end.month()) {
                    let next = week_end + Duration::days(1);
                    section = section.note(format!("{} – {}", next.day(), last_day(first).day()));
                }
                out.push(section);
            }
        }
        Nav::Inbox | Nav::List(_) => {
            let href = match nav {
                Nav::List(h) => Some(h.clone()),
                _ => inbox(data.lists).map(|l| l.href.clone()),
            };
            let Some(href) = href else { return out };
            let tasks = sorted(pick(&|t| t.list == href), sort, zone);
            out.push(Section::new(Kind::List(href.clone()), None, tasks).keep());
            if opts.show_completed {
                let finished = done(&|t| t.list == href);
                if !finished.is_empty() {
                    let n = finished.len();
                    out.push(
                        Section::new(Kind::Completed, Some("Completed".into()), finished)
                            .note(n.to_string()),
                    );
                }
            }
        }
        Nav::Tomorrow => {
            let d = today + Duration::days(1);
            let tasks = sorted(pick(&|t| day(t) == Some(d)), sort, zone);
            out.push(Section::new(Kind::Day(d), None, tasks).keep());
        }
        Nav::Anytime => out.extend(by_list(
            data.lists,
            pick(&|t| t.task.due.is_none()),
            &|tasks| sorted(tasks, sort, zone),
        )),
        Nav::Day => {
            // The timeline is drawn from `blocks` (tracked) and `day_plan`
            // (placed slots), not task sections. One kept section so the
            // column still has a header in the shared shape.
            out.push(Section::new(Kind::Day(today), Some("Day".into()), Vec::new()).keep());
        }
        Nav::Matrix => {
            let mut boxes: [Vec<TaskView>; 4] = Default::default();
            for t in pick(&|_| true) {
                let q = quadrant_of(t.task.priority, t.task.due.as_ref(), today, zone);
                boxes[Quadrant::ALL.iter().position(|x| *x == q).unwrap()].push(t);
            }
            for (q, tasks) in Quadrant::ALL.into_iter().zip(boxes) {
                out.push(
                    Section::new(
                        Kind::Quadrant(q),
                        Some(q.title().into()),
                        sorted(tasks, sort, zone),
                    )
                    .note(q.about())
                    .keep(),
                );
            }
        }
        Nav::Repeating => {
            let tasks = sorted(pick(&|t| t.task.rrule.is_some()), sort, zone);
            out.push(Section::new(Kind::Plain, None, tasks).keep());
        }
        Nav::All => out.extend(by_list(data.lists, pick(&|_| true), &|tasks| {
            sorted(tasks, sort, zone)
        })),
        Nav::Completed => {
            let mut days: Vec<(Option<NaiveDate>, Vec<TaskView>)> = Vec::new();
            for t in done(&|_| true) {
                let d = completed_on(&t.task, zone);
                match days.last_mut() {
                    Some((k, tasks)) if *k == d => tasks.push(t),
                    _ => days.push((d, vec![t])),
                }
            }
            for (d, tasks) in days {
                let title = d.map_or_else(|| "Earlier".into(), |d| day_title(d, today));
                let n = tasks.len();
                out.push(Section::new(Kind::Completed, Some(title), tasks).note(n.to_string()));
            }
        }
    }
    out
}

fn by_list(
    lists: &[ListView],
    tasks: Vec<TaskView>,
    order: &dyn Fn(Vec<TaskView>) -> Vec<TaskView>,
) -> Vec<Section> {
    let mut out = Vec::new();
    for l in lists {
        let mine: Vec<TaskView> = tasks.iter().filter(|t| t.list == l.href).cloned().collect();
        if mine.is_empty() {
            continue;
        }
        let n = mine.len();
        out.push(
            Section::new(
                Kind::List(l.href.clone()),
                Some(l.name.clone()),
                order(mine),
            )
            .note(n.to_string()),
        );
    }
    out
}

fn completed_on(t: &Task, zone: Tz) -> Option<NaiveDate> {
    t.completed
        .or(t.modified)
        .map(|at| at.with_timezone(&zone).date_naive())
}

fn last_day(first: NaiveDate) -> NaiveDate {
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
    };
    next.expect("a month") - Duration::days(1)
}

/// `Today`, `Yesterday`, `Tuesday, Sep 8`, with the year when it isn't this one.
pub fn day_title(d: NaiveDate, today: NaiveDate) -> String {
    match (d - today).num_days() {
        0 => "Today".into(),
        -1 => "Yesterday".into(),
        1 => "Tomorrow".into(),
        _ if d.year() == today.year() => d.format("%A, %b %-d").to_string(),
        _ => d.format("%A, %b %-d %Y").to_string(),
    }
}

pub fn sorted(mut tasks: Vec<TaskView>, sort: Sort, zone: Tz) -> Vec<TaskView> {
    let due = |t: &TaskView| {
        t.task
            .due
            .as_ref()
            .map(|d| (d.local_date(zone), d.instant(zone)))
    };
    let custom = |t: &TaskView| {
        (
            t.task.sort_order.is_none(),
            t.task.sort_order,
            t.task.created,
        )
    };
    let level = |t: &TaskView| priority_level(t.task.priority);
    match sort {
        Sort::Custom => tasks.sort_by_key(|t| custom(t)),
        Sort::Due => tasks.sort_by_key(|t| (due(t).is_none(), due(t), level(t), custom(t))),
        Sort::Priority => tasks.sort_by_key(|t| (level(t), due(t).is_none(), due(t), custom(t))),
        Sort::Name => tasks.sort_by(|a, b| {
            natural_cmp(&a.task.summary, &b.task.summary).then_with(|| custom(a).cmp(&custom(b)))
        }),
        Sort::Added => tasks.sort_by_key(|t| (t.task.created.is_none(), t.task.created)),
        Sort::Modified => tasks.sort_by_key(|t| {
            let at = t.task.modified.or(t.task.created);
            (at.is_none(), at)
        }),
    }
    tasks
}

/// Names as people read them: case aside, and runs of digits by their value,
/// so "Task 2" comes before "Task 10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    let digits = |s: &mut std::iter::Peekable<std::str::Chars>| {
        let mut n = String::new();
        while let Some(c) = s.next_if(char::is_ascii_digit) {
            n.push(c);
        }
        n
    };
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (na, nb) = (digits(&mut a), digits(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let order = ta
                    .len()
                    .cmp(&tb.len())
                    .then_with(|| ta.cmp(tb))
                    .then_with(|| na.len().cmp(&nb.len()));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Tasks as Markdown checkboxes, each with its date, and its notes indented
/// under it: for the clipboard.
pub fn tasks_markdown(tasks: &[TaskView], now: DateTime<Tz>) -> String {
    let mut out = String::new();
    for t in tasks {
        out.push_str(if t.task.is_open() { "- [ ] " } else { "- [x] " });
        out.push_str(&t.task.summary);
        if let Some(d) = t.task.due.as_ref().filter(|_| t.task.is_open()) {
            out.push_str(&format!(" ({})", fmt::due_label(d, now)));
        }
        out.push('\n');
        for line in t.task.description.as_deref().unwrap_or("").lines() {
            if !line.trim().is_empty() {
                out.push_str("  ");
                out.push_str(line);
            }
            out.push('\n');
        }
    }
    out
}

/// New X-APPLE-SORT-ORDER values that put `moved` just before `target` (or
/// after it) in `tasks`, a list shown in custom order. One number when there
/// is room between the new neighbors, or at either end; otherwise the whole
/// list is renumbered, touching only what changes.
pub fn reorder(tasks: &[TaskView], moved: &str, target: &str, after: bool) -> Vec<(String, i64)> {
    const STEP: i64 = 1024;
    if moved == target {
        return Vec::new();
    }
    let mut order: Vec<&TaskView> = tasks.iter().filter(|t| t.href != moved).collect();
    let (Some(m), Some(at)) = (
        tasks.iter().find(|t| t.href == moved),
        order.iter().position(|t| t.href == target),
    ) else {
        return Vec::new();
    };
    let at = at + usize::from(after);
    order.insert(at, m);
    let value = |i: usize| order.get(i).and_then(|t| t.task.sort_order);
    let (prev, next) = (at.checked_sub(1).and_then(value), value(at + 1));
    let numbered = order
        .iter()
        .enumerate()
        .all(|(i, t)| i == at || t.task.sort_order.is_some());
    if numbered {
        let single = match (prev, next, at == 0, at + 1 == order.len()) {
            (Some(p), Some(n), _, _) if n - p >= 2 => Some(p + (n - p) / 2),
            (None, Some(n), true, _) if n > STEP => Some(n - STEP),
            (Some(p), None, _, true) => Some(p + STEP),
            _ => None,
        };
        if let Some(v) = single {
            return vec![(moved.to_string(), v)];
        }
    }
    order
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let v = (i as i64 + 1) * STEP;
            (t.task.sort_order != Some(v)).then(|| (t.href.clone(), v))
        })
        .collect()
}

/// Where the cursor goes when its row has left the view (checked off, moved
/// to another day): the row that followed it, else the nearest one before.
/// `before` and `after` are the rows' hrefs, top to bottom, of the build the
/// cursor was in and of the one that replaced it. Nothing when the cursor
/// was not in `before`, which is a view change, not a row leaving.
pub fn cursor_after_leaving<'a>(
    before: &[&str],
    cursor: &str,
    after: &[&'a str],
) -> Option<&'a str> {
    let at = before.iter().position(|h| *h == cursor)?;
    let (behind, ahead) = before.split_at(at);
    ahead[1..]
        .iter()
        .chain(behind.iter().rev())
        .find_map(|h| after.iter().find(|a| *a == h).copied())
}

// -- day plan ----------------------------------------------------------------

/// First hour row of the Plan grid (inclusive).
pub const PLAN_HOUR_START: u32 = 0;
/// End of the Plan grid (exclusive): rows run `PLAN_HOUR_START..PLAN_HOUR_END`.
/// The whole day, so an early or late hour is on the grid and the now-line
/// always has a place; the column scrolls rather than cropping the day.
pub const PLAN_HOUR_END: u32 = 24;
/// Minutes a newly placed task holds until its edge is dragged.
pub const PLAN_DEFAULT_DURATION: u32 = 30;
/// Shortest slot the plan keeps. The grid may still draw one shorter higher.
pub const PLAN_MIN_DURATION: u32 = 15;
/// Default pixels per hour on the shared day grid (Time and Plan). The
/// zoomable range is `PX_PER_HOUR_MIN..=PX_PER_HOUR_MAX`; this is what the
/// views open at and what `window.toml` falls back to.
pub const PLAN_PX_PER_HOUR: f64 = 52.0;
/// Smallest usable zoom: a full day still fits a few scrolls.
pub const PX_PER_HOUR_MIN: f64 = 12.0;
/// Largest usable zoom: a single hour fills a short column.
pub const PX_PER_HOUR_MAX: f64 = 120.0;
/// Minutes a drop or a drag snaps to.
pub const PLAN_SNAP: u32 = 15;

/// Clamp a zoom into the usable range, and keep it finite.
pub fn clamp_px_per_hour(px: f64) -> f64 {
    if !px.is_finite() {
        return PLAN_PX_PER_HOUR;
    }
    px.clamp(PX_PER_HOUR_MIN, PX_PER_HOUR_MAX)
}

/// A window-local planned slot. The iPhone has no such thing, so this never
/// reaches the VTODO: `window.toml` holds it, keyed by task UID + date, with
/// the start (minutes from midnight) and the duration in minutes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSlot {
    pub uid: String,
    pub date: NaiveDate,
    pub start: u32,
    pub duration: u32,
}

/// One task on the day grid, after overlap layout.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanBlock {
    pub task: TaskView,
    pub start: u32,
    pub duration: u32,
    /// 0-based column among the blocks this one overlaps.
    pub col: usize,
    /// Columns the whole overlap group shares; the block is `1/cols` wide.
    pub cols: usize,
}

/// What the Day view draws: placed blocks, the tray, and the now-line.
#[derive(Debug, Clone, PartialEq)]
pub struct DayPlan {
    /// The local day this plan is for (the day the view is showing).
    pub day: NaiveDate,
    pub hour_start: u32,
    pub hour_end: u32,
    pub blocks: Vec<PlanBlock>,
    /// Open tasks due on or before `day` and not placed on it: the ones
    /// that want a slot.
    pub tray: Vec<TaskView>,
    /// Open tasks with no date, not placed on `day`: the tray's collapsed
    /// No date section, so a task nobody dated can still be planned.
    pub no_date: Vec<TaskView>,
    /// Minutes from midnight, only when `day` is the wall-clock today.
    pub now_min: Option<u32>,
}

/// A clock time in the app's one voice, `9am` or `5:30pm`: the hour rules,
/// the blocks and the toasts all say it this way. `24 * 60` reads as the
/// next midnight.
pub fn clock_label(min: u32) -> String {
    let t = NaiveTime::from_hms_opt(min / 60, min % 60, 0).unwrap_or(NaiveTime::MIN);
    fmt::time_label(t)
}

/// Snap `min` (minutes from midnight) to the plan grid.
pub fn snap(min: i32) -> u32 {
    let s = PLAN_SNAP as i32;
    ((min + s / 2) / s * s).clamp(0, 24 * 60 - 1) as u32
}

/// Minutes `dy` pixels stand for on the plan grid at `px_per_hour`.
/// May be negative.
fn dy_minutes(dy: f64, px_per_hour: f64) -> i32 {
    (dy / px_per_hour * 60.0) as i32
}

/// Minutes from midnight for a y position on the grid at `px_per_hour`.
pub fn minutes_at(y: f64, hour_start: u32, px_per_hour: f64) -> u32 {
    let mins = f64::from(hour_start) * 60.0 + y / px_per_hour * 60.0;
    snap(mins as i32)
}

/// y on the grid for `mins` minutes from midnight at `px_per_hour`. The
/// inverse of `minutes_at` before snapping; widgets draw from this so the
/// grid and the placement math cannot drift apart. `hour_start` is the
/// grid's first hour (0 on the shared full-day grid).
pub fn y_at(mins: f64, hour_start: u32, px_per_hour: f64) -> f64 {
    mins / 60.0 * px_per_hour - f64::from(hour_start) * px_per_hour
}

/// Start a block at `start` would move to, `dy` pixels down the grid.
pub fn move_start(start: u32, dy: f64, px_per_hour: f64) -> u32 {
    snap(start as i32 + dy_minutes(dy, px_per_hour))
}

/// End minute a duration drag from `start`+`dur0` reaches, `dy` down.
pub fn edge_end(start: u32, dur0: u32, dy: f64, px_per_hour: f64) -> u32 {
    snap(start as i32 + dur0 as i32 + dy_minutes(dy, px_per_hour))
}

/// Duration a slot at `start` keeps: floored and kept inside the day.
pub fn slot_duration(start: u32, duration: u32) -> u32 {
    let start = start.min(24 * 60 - 1);
    let room = (24 * 60 - start).max(1);
    duration.clamp(PLAN_MIN_DURATION.min(room), room)
}

/// Start a block takes when the keyboard nudges it by `delta` minutes.
pub fn nudge_start(start: u32, delta: i32) -> u32 {
    snap(start as i32 + delta)
}

/// Where a keyboard place starts looking for room: the morning, not the
/// grid's first row at midnight.
pub const PLAN_PLACE_FROM: u32 = 9 * 60;

/// Where a keyboard place lands on `day`: the task's own time of day when
/// it has one, else the first free slot from 9:00. Today's search starts at
/// now when that is later, so a plan never begins in the past.
pub fn place_start(task: &Task, slots: &[PlanSlot], day: NaiveDate, now: DateTime<Tz>) -> u32 {
    let zone = now.timezone();
    if let Some(due) = task.due.as_ref().filter(|d| d.has_time()) {
        let at = due.instant(zone).with_timezone(&zone).time();
        return at.hour() * 60 + at.minute();
    }
    let mut from = PLAN_PLACE_FROM;
    if day == now.date_naive() {
        from = from.max(snap((now.hour() * 60 + now.minute()) as i32));
    }
    first_free(slots, day, from, PLAN_DEFAULT_DURATION)
}

/// The first start at or after `from`, on the grid, where `duration`
/// minutes fit between `day`'s slots. With no room before midnight, `from`:
/// blocks that overlap are laid side by side.
fn first_free(slots: &[PlanSlot], day: NaiveDate, from: u32, duration: u32) -> u32 {
    let mut busy: Vec<(u32, u32)> = slots
        .iter()
        .filter(|s| s.date == day)
        .map(|s| (s.start, s.start + s.duration))
        .collect();
    busy.sort_unstable();
    let mut start = from;
    for (s, e) in busy {
        if e <= start {
            continue;
        }
        if s >= start + duration {
            break;
        }
        start = e.div_ceil(PLAN_SNAP) * PLAN_SNAP;
    }
    if start + duration <= 24 * 60 {
        start
    } else {
        from
    }
}

/// The minute a day other than today opens at the top of the view: an hour
/// before its first block, else 7 AM.
pub fn day_rest_min(first_min: Option<f64>) -> u32 {
    first_min.map_or(7 * 60, |m| (m as u32).saturating_sub(60))
}

/// `now` in minutes if it falls on the grid `hour_start..hour_end`, else
/// none: the now-line must not pin itself to a rule it is not on. The
/// minute is capped at 24:00 − 1 so a rounding past midnight still lands
/// on the last rule of a full-day grid.
pub fn now_on_grid(now: DateTime<Tz>, hour_start: u32, hour_end: u32) -> Option<u32> {
    let mins = now.time().hour() * 60 + now.time().minute() + u32::from(now.time().second() > 30);
    let mins = mins.min(24 * 60 - 1);
    (mins >= hour_start * 60 && mins < hour_end * 60).then_some(mins)
}

/// Grid height in pixels for hours `hour_start..hour_end` at `px_per_hour`.
pub fn grid_height(hour_start: u32, hour_end: u32, px_per_hour: f64) -> f64 {
    f64::from(hour_end.saturating_sub(hour_start).max(1)) * px_per_hour
}

/// Scroll offset that keeps `mins` in a viewport of `view_h` pixels,
/// slightly above center so the hours after it stay visible. Clamped to
/// the grid. `hour_start` is the grid's first hour, as in `minutes_at`.
pub fn scroll_to_min(
    mins: u32,
    hour_start: u32,
    hour_end: u32,
    view_h: f64,
    px_per_hour: f64,
) -> f64 {
    let y = f64::from(mins) / 60.0 * px_per_hour - f64::from(hour_start) * px_per_hour;
    let max = (grid_height(hour_start, hour_end, px_per_hour) - view_h.max(0.0)).max(0.0);
    (y - view_h / 3.0).clamp(0.0, max)
}

/// Scroll offset that holds `mins` at `keep_y` from the top of a viewport
/// of `view_h` pixels. A zoom re-scrolls with this so the minute under the
/// pointer (or the top-visible one) does not jump. Clamped to the grid.
pub fn scroll_keep_min(
    mins: u32,
    hour_start: u32,
    hour_end: u32,
    view_h: f64,
    keep_y: f64,
    px_per_hour: f64,
) -> f64 {
    let y = y_at(f64::from(mins), hour_start, px_per_hour);
    let max = (grid_height(hour_start, hour_end, px_per_hour) - view_h.max(0.0)).max(0.0);
    (y - keep_y).clamp(0.0, max)
}

/// The block a drop of `task` at `start` for `duration` would create.
/// `blocks` are the ones already on the grid; `task`'s own block among
/// them is left out, so a move previews without the old home. The ghost
/// wears the same start, duration and overlap column the drop would.
pub fn ghost_block(blocks: &[PlanBlock], task: &TaskView, start: u32, duration: u32) -> PlanBlock {
    let start = start.min(24 * 60 - 1);
    let duration = slot_duration(start, duration);
    let mut with: Vec<PlanBlock> = blocks
        .iter()
        .filter(|b| b.task.task.uid != task.task.uid)
        .cloned()
        .collect();
    with.push(PlanBlock {
        task: task.clone(),
        start,
        duration,
        col: 0,
        cols: 1,
    });
    with.sort_by_key(|b| (b.start, b.duration, b.task.task.uid.clone()));
    layout(&mut with);
    with.into_iter()
        .find(|b| b.task.task.uid == task.task.uid)
        .expect("ghost is in the layout")
}

/// Upsert `uid`'s slot on `date`. One slot per task per day.
pub fn place(slots: &mut Vec<PlanSlot>, uid: &str, date: NaiveDate, start: u32, duration: u32) {
    let start = start.min(24 * 60 - 1);
    let duration = slot_duration(start, duration);
    match slots.iter_mut().find(|s| s.uid == uid && s.date == date) {
        Some(s) => {
            s.start = start;
            s.duration = duration;
        }
        None => slots.push(PlanSlot {
            uid: uid.to_string(),
            date,
            start,
            duration,
        }),
    }
}

/// Take `uid` off `date`'s plan.
pub fn unplace(slots: &mut Vec<PlanSlot>, uid: &str, date: NaiveDate) {
    slots.retain(|s| !(s.uid == uid && s.date == date));
}

/// Drop slots whose task is gone from the open set. Leaves the rest alone.
pub fn prune_slots(slots: &mut Vec<PlanSlot>, live: &HashSet<&str>) -> bool {
    let before = slots.len();
    slots.retain(|s| live.contains(s.uid.as_str()));
    slots.len() != before
}

/// Where carrying a day's unfinished blocks forward takes them: the next
/// day, but never a day that is already over.
pub fn carry_target(viewed: NaiveDate, today: NaiveDate) -> NaiveDate {
    viewed.succ_opt().unwrap_or(viewed).max(today)
}

/// What a carry did, so that it can be taken back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Carried {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// The slots as they were on `from`.
    pub moved: Vec<PlanSlot>,
    /// Tasks that had nothing on `to` before, and so have a slot there now.
    pub created: Vec<String>,
}

/// Move the slots on `from` whose tasks are `open` onto `to`, at the same
/// time of day. A task already planned on `to` keeps that slot and just
/// leaves `from`. Slots of tasks that are not open stay as they are (the
/// day view does not show them, and pruning is not this).
pub fn carry_forward(
    slots: &mut Vec<PlanSlot>,
    open: &HashSet<&str>,
    from: NaiveDate,
    to: NaiveDate,
) -> Carried {
    let mut out = Carried::default();
    if from == to {
        return out;
    }
    let moving: Vec<PlanSlot> = slots
        .iter()
        .filter(|s| s.date == from && open.contains(s.uid.as_str()))
        .cloned()
        .collect();
    for s in &moving {
        if !slots.iter().any(|o| o.uid == s.uid && o.date == to) {
            place(slots, &s.uid, to, s.start, s.duration);
            out.created.push(s.uid.clone());
        }
        unplace(slots, &s.uid, from);
    }
    if !moving.is_empty() {
        out.from = Some(from);
        out.to = Some(to);
    }
    out.moved = moving;
    out
}

/// Put a carry back: the slots return to their day, and the ones it made
/// on the target day go.
pub fn uncarry(slots: &mut Vec<PlanSlot>, carried: &Carried) {
    let (Some(from), Some(to)) = (carried.from, carried.to) else {
        return;
    };
    for uid in &carried.created {
        unplace(slots, uid, to);
    }
    for s in &carried.moved {
        place(slots, &s.uid, from, s.start, s.duration);
    }
}

/// The day plan for `viewed`. Blocks are the open tasks with a slot on that
/// day; the tray is the open tasks due on or before `viewed` (what is
/// overdue, and what falls due by then) minus the tasks placed on it, and
/// `no_date` the open tasks with no date, likewise. Tasks due after `viewed`
/// wait for their own day. The same task may sit on several days' plans at
/// once, so a slot on another day does not empty it from the tray here.
///
/// Overlaps sit side by side: each block takes the leftmost column that no
/// overlapping block already holds, and a group of blocks linked by overlap
/// shares one width (`cols` columns). A drop or a move only changes the
/// slot; the layout is recomputed here every time.
pub fn day_plan(
    open: &[TaskView],
    slots: &[PlanSlot],
    now: DateTime<Tz>,
    viewed: NaiveDate,
) -> DayPlan {
    let zone = now.timezone();
    let today = now.date_naive();
    let day_slots: Vec<&PlanSlot> = slots.iter().filter(|s| s.date == viewed).collect();
    let placed: HashSet<&str> = day_slots.iter().map(|s| s.uid.as_str()).collect();
    let mut blocks: Vec<PlanBlock> = Vec::new();
    for s in &day_slots {
        if let Some(t) = open.iter().find(|t| t.task.uid == s.uid) {
            blocks.push(PlanBlock {
                task: t.clone(),
                start: s.start,
                duration: s.duration,
                col: 0,
                cols: 1,
            });
        }
    }
    blocks.sort_by_key(|b| (b.start, b.duration, b.task.task.uid.clone()));
    layout(&mut blocks);

    let due = |t: &TaskView| t.task.due.as_ref().map(|d| d.local_date(zone));
    let unplaced = open
        .iter()
        .filter(|t| !placed.contains(t.task.uid.as_str()));
    let mut tray: Vec<TaskView> = unplaced
        .clone()
        .filter(|t| due(t).is_some_and(|d| d <= viewed))
        .cloned()
        .collect();
    tray.sort_by_key(|t| (due(t), priority_level(t.task.priority), t.task.uid.clone()));
    let no_date = sorted(
        unplaced.filter(|t| due(t).is_none()).cloned().collect(),
        Sort::Priority,
        zone,
    );

    DayPlan {
        day: viewed,
        hour_start: PLAN_HOUR_START,
        hour_end: PLAN_HOUR_END,
        blocks,
        tray,
        no_date,
        now_min: (viewed == today)
            .then(|| now_on_grid(now, PLAN_HOUR_START, PLAN_HOUR_END))
            .flatten(),
    }
}

/// Greedy side-by-side columns, then each block's group width.
fn layout(blocks: &mut [PlanBlock]) {
    // Columns used by the blocks each one already overlaps.
    let mut col_of: Vec<usize> = vec![0; blocks.len()];
    for i in 0..blocks.len() {
        let (s, e) = (blocks[i].start, blocks[i].start + blocks[i].duration);
        let mut taken = HashSet::new();
        for j in 0..i {
            let (os, oe) = (blocks[j].start, blocks[j].start + blocks[j].duration);
            if s < oe && os < e {
                taken.insert(col_of[j]);
            }
        }
        let mut col = 0;
        while taken.contains(&col) {
            col += 1;
        }
        col_of[i] = col;
    }
    // Connected components of the overlap graph share one width.
    let mut group = vec![usize::MAX; blocks.len()];
    for i in 0..blocks.len() {
        if group[i] != usize::MAX {
            continue;
        }
        let mut stack = vec![i];
        let mut members = Vec::new();
        group[i] = i;
        while let Some(k) = stack.pop() {
            members.push(k);
            let (s, e) = (blocks[k].start, blocks[k].start + blocks[k].duration);
            for j in 0..blocks.len() {
                if group[j] != usize::MAX {
                    continue;
                }
                let (os, oe) = (blocks[j].start, blocks[j].start + blocks[j].duration);
                if s < oe && os < e {
                    group[j] = i;
                    stack.push(j);
                }
            }
        }
        let cols = members.iter().map(|&m| col_of[m]).max().unwrap_or(0) + 1;
        for m in members {
            blocks[m].col = col_of[m];
            blocks[m].cols = cols;
        }
    }
}

/// Overlap columns for half-open minute spans `(start, end)`, in input
/// order. Each span takes the leftmost free column among the ones before
/// it that it overlaps; a connected group shares one width. Time's tracked
/// chips and Plan's blocks both pack this way inside their lane.
pub fn span_columns(spans: &[(f64, f64)]) -> Vec<(usize, usize)> {
    let n = spans.len();
    let mut col_of = vec![0usize; n];
    for i in 0..n {
        let (s, e) = spans[i];
        let mut taken = HashSet::new();
        for j in 0..i {
            let (os, oe) = spans[j];
            if s < oe && os < e {
                taken.insert(col_of[j]);
            }
        }
        let mut col = 0;
        while taken.contains(&col) {
            col += 1;
        }
        col_of[i] = col;
    }
    let mut group = vec![usize::MAX; n];
    let mut out = vec![(0, 1); n];
    for i in 0..n {
        if group[i] != usize::MAX {
            continue;
        }
        let mut stack = vec![i];
        let mut members = Vec::new();
        group[i] = i;
        while let Some(k) = stack.pop() {
            members.push(k);
            let (s, e) = spans[k];
            for j in 0..n {
                if group[j] != usize::MAX {
                    continue;
                }
                let (os, oe) = spans[j];
                if s < oe && os < e {
                    group[j] = i;
                    stack.push(j);
                }
            }
        }
        let cols = members.iter().map(|&m| col_of[m]).max().unwrap_or(0) + 1;
        for m in members {
            out[m] = (col_of[m], cols);
        }
    }
    out
}

// -- counts -----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub inbox: usize,
    pub today: usize,
    pub overdue: usize,
    pub scheduled: usize,
    pub tomorrow: usize,
    pub anytime: usize,
    pub repeating: usize,
    pub all: usize,
    /// Open tasks with a planned slot today.
    pub day: usize,
    pub lists: HashMap<String, usize>,
}

impl Counts {
    pub fn new(data: &Data, slots: &[PlanSlot], now: DateTime<Tz>) -> Counts {
        let zone = now.timezone();
        let today = now.date_naive();
        let mut c = Counts::default();
        let inbox = inbox(data.lists).map(|l| l.href.as_str());
        for t in data.open {
            c.all += 1;
            *c.lists.entry(t.list.clone()).or_default() += 1;
            if Some(t.list.as_str()) == inbox {
                c.inbox += 1;
            }
            if t.task.rrule.is_some() {
                c.repeating += 1;
            }
            match t.task.due.as_ref().map(|d| d.local_date(zone)) {
                None => c.anytime += 1,
                Some(d) if d < today => {
                    c.overdue += 1;
                    c.today += 1;
                }
                Some(d) if d == today => {
                    c.today += 1;
                    // Due earlier today: late, though still among today's.
                    if t.task.due.as_ref().is_some_and(|d| fmt::is_overdue(d, now)) {
                        c.overdue += 1;
                    }
                }
                Some(d) => {
                    c.scheduled += 1;
                    if d == today + Duration::days(1) {
                        c.tomorrow += 1;
                    }
                }
            }
        }
        c.day = day_plan(data.open, slots, now, today).blocks.len();
        c
    }

    /// What a sidebar entry shows; nothing for Completed.
    pub fn of(&self, nav: &Nav) -> Option<usize> {
        match nav {
            Nav::Inbox => Some(self.inbox),
            Nav::Today => Some(self.today),
            Nav::Day => Some(self.day),
            // Every open task sits in some box.
            Nav::Matrix => Some(self.all),
            Nav::Scheduled => Some(self.scheduled),
            Nav::Completed => None,
            Nav::Tomorrow => Some(self.tomorrow),
            Nav::Anytime => Some(self.anytime),
            Nav::Repeating => Some(self.repeating),
            Nav::All => Some(self.all),
            Nav::List(href) => Some(self.lists.get(href).copied().unwrap_or(0)),
        }
    }
}

// -- dates ------------------------------------------------------------------

/// What sync is doing, as Preferences and the sync button's tooltip say it.
pub fn sync_state(s: &StatusView, now: DateTime<Tz>) -> String {
    let mut state = match s.state {
        SyncState::Idle => s.last_sync.map_or("Not synced yet".to_string(), |t| {
            format!("Synced {}", ago(t, now))
        }),
        SyncState::Syncing => "Syncing…".into(),
        SyncState::Offline => "Offline; changes wait until the server is back".into(),
        SyncState::Error | SyncState::NoAccount => {
            s.message.clone().unwrap_or_else(|| "Sync failed".into())
        }
    };
    match s.pending {
        0 => {}
        1 => state.push_str(" · 1 change waiting to be sent"),
        n => state.push_str(&format!(" · {n} changes waiting to be sent")),
    }
    state
}

/// `owner/repo` of an issue's URL.
pub fn issue_repo(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}"))
}

/// Where a repo is checked out: its name under the projects folder
/// (`~` in it is the home folder).
pub fn checkout_dir(projects: &str, repo: &str, home: &str) -> std::path::PathBuf {
    let projects = match projects.trim() {
        "" => "~/projects",
        p => p,
    };
    let projects = match projects.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => projects.to_string(),
    };
    let name = repo.rsplit('/').next().unwrap_or(repo);
    std::path::Path::new(&projects).join(name)
}

/// The command to run for "start work": `template` with `{dir}`, `{repo}`,
/// `{number}` and `{url}` replaced, each quoted for `sh`.
pub fn work_command(template: &str, dir: &str, repo: &str, number: u32, url: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    template
        .replace("{dir}", &quote(dir))
        .replace("{repo}", &quote(repo))
        .replace("{number}", &number.to_string())
        .replace("{url}", &quote(url))
}

/// What saving a list's dialog does to its GitHub tie, from the repo
/// typed there (#17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEdit {
    /// The field was left as it was.
    Keep,
    Link(String),
    Unlink(String),
    /// A list holds one repo, so the old tie goes before the new one.
    Move {
        from: String,
        to: String,
    },
}

/// `owner/repo` from what was typed or pasted: the name itself, or the
/// repo's page, clone URL or ssh address. None when it is none of those.
pub fn repo_from_text(text: &str) -> Option<String> {
    let t = text.trim().trim_end_matches('/');
    let t = [
        "https://github.com/",
        "http://github.com/",
        "github.com/",
        "git@github.com:",
    ]
    .iter()
    .find_map(|p| t.strip_prefix(p))
    .unwrap_or(t);
    let t = t.strip_suffix(".git").unwrap_or(t);
    // A page inside the repo (…/issues/3) is still that repo.
    let mut parts = t.split('/');
    let repo = format!("{}/{}", parts.next()?, parts.next()?);
    let rest_ok = text.trim().contains("github.com") || parts.next().is_none();
    (rest_ok && asst_core::github::valid_repo(&repo)).then_some(repo)
}

/// The change from `current` (the list's tie now) to what the dialog's
/// field says. An empty field unlinks; anything else must name a repo.
pub fn link_edit(current: Option<&str>, typed: &str) -> Result<LinkEdit, String> {
    if typed.trim().is_empty() {
        return Ok(current.map_or(LinkEdit::Keep, |c| LinkEdit::Unlink(c.to_string())));
    }
    let Some(wanted) = repo_from_text(typed) else {
        return Err(format!("“{}” is not owner/repo", typed.trim()));
    };
    Ok(match current {
        Some(c) if c.eq_ignore_ascii_case(&wanted) => LinkEdit::Keep,
        Some(c) => LinkEdit::Move {
            from: c.to_string(),
            to: wanted,
        },
        None => LinkEdit::Link(wanted),
    })
}

/// A list's GitHub link, as its sidebar mark's tooltip says it: when its
/// issues were read, or what went wrong, then on a line of its own what the
/// daemon found about the repo (archived, renamed, not the same repo).
pub fn link_state(l: &LinkView, now: DateTime<Tz>) -> String {
    let read = match (&l.error, l.synced) {
        (Some(e), Some(t)) => format!("{}: {e} (last read {})", l.repo, ago(t, now)),
        (Some(e), None) => format!("{}: {e}", l.repo),
        (None, Some(t)) => format!("Issues of {} read {}", l.repo, ago(t, now)),
        (None, None) => format!("Issues of {}: not read yet", l.repo),
    };
    match (&l.state, &l.note) {
        (Some(_), Some(note)) => format!("{read}\n{note}"),
        _ => read,
    }
}

/// Whether the link's mark shows a warning: its last pass failed, or its
/// issues are not being read at all.
pub fn link_warns(l: &LinkView) -> bool {
    l.error.is_some() || l.state.is_some_and(|s| s.stalls())
}

/// `just now`, `12 seconds ago`, `5 minutes ago`, `3 hours ago`, then the
/// day. Seconds, because the daemon checks every minute: "Synced just now"
/// would be all it ever said.
pub fn ago(then: DateTime<Utc>, now: DateTime<Tz>) -> String {
    let secs = (now.with_timezone(&Utc) - then).num_seconds().max(0);
    match secs {
        0..5 => "just now".into(),
        5..60 => format!("{secs} seconds ago"),
        60..3600 => format!("{} ago", plural(secs / 60, "minute")),
        3600..86400 => format!("{} ago", plural(secs / 3600, "hour")),
        _ => {
            let day = then.with_timezone(&now.timezone()).date_naive();
            if day.year() == now.year() {
                day.format("on %b %-d").to_string()
            } else {
                day.format("on %b %-d, %Y").to_string()
            }
        }
    }
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// A due date as a row shows it, and the class that colors it. A time that
/// has gone by is overdue the same day, as it is in the bar and the
/// notification.
pub fn due_chip(when: &When, now: DateTime<Tz>) -> (String, &'static str) {
    let class = if fmt::is_overdue(when, now) {
        "overdue"
    } else if when.local_date(now.timezone()) == now.date_naive() {
        "today"
    } else {
        "upcoming"
    };
    (capitalize(&fmt::due_label(when, now)), class)
}

/// `when` moved to `date`, keeping its time of day and form.
pub fn on_date(when: Option<&When>, date: NaiveDate, zone: Tz) -> When {
    match when {
        Some(w) if w.has_time() => {
            let time = w.instant(zone).with_timezone(&zone).time();
            w.with_instant(resolve(zone, date.and_time(time)), zone)
        }
        _ => When::Date { date },
    }
}

/// `when` at `time` on its own day, or today when there is none.
pub fn at_time(when: Option<&When>, time: NaiveTime, today: NaiveDate, zone: Tz) -> When {
    let date = when.map_or(today, |w| w.local_date(zone));
    match when {
        Some(w) if w.has_time() => w.with_instant(resolve(zone, date.and_time(time)), zone),
        _ => When::local(date.and_time(time), zone),
    }
}

/// `5pm`, `17:30`, `9`, `930`, `12:15 am`.
pub fn parse_time(text: &str) -> Option<NaiveTime> {
    let t = text.trim().to_lowercase().replace(' ', "");
    let (body, pm) = if let Some(b) = t.strip_suffix("pm").or_else(|| t.strip_suffix('p')) {
        (b.to_string(), Some(true))
    } else if let Some(b) = t.strip_suffix("am").or_else(|| t.strip_suffix('a')) {
        (b.to_string(), Some(false))
    } else {
        (t, None)
    };
    let (h, m) = match body.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if body.len() >= 3 => {
            let split = body.len() - 2;
            (body[..split].parse().ok()?, body[split..].parse().ok()?)
        }
        None => (body.parse().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        Some(_) if h > 12 => return None,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

// -- repeats ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    pub const ALL: [Freq; 4] = [Freq::Daily, Freq::Weekly, Freq::Monthly, Freq::Yearly];

    fn code(self) -> &'static str {
        match self {
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    Never,
    Count(u32),
    Until(NaiveDate),
}

/// The part of RRULE the repeat editor can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repeat {
    pub freq: Freq,
    pub interval: u32,
    pub days: Vec<Weekday>,
    pub end: End,
}

pub const REPEAT_PRESETS: [(&str, &str); 6] = [
    ("Daily", "FREQ=DAILY"),
    ("Weekdays", "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
    ("Weekends", "FREQ=WEEKLY;BYDAY=SA,SU"),
    ("Weekly", "FREQ=WEEKLY"),
    ("Monthly", "FREQ=MONTHLY"),
    ("Yearly", "FREQ=YEARLY"),
];

pub const WEEK: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

impl Default for Repeat {
    fn default() -> Repeat {
        Repeat {
            freq: Freq::Daily,
            interval: 1,
            days: Vec::new(),
            end: End::Never,
        }
    }
}

impl Repeat {
    /// None when the rule says more than the editor shows (BYSETPOS, hours).
    pub fn parse(rule: &str) -> Option<Repeat> {
        let mut r = Repeat::default();
        let mut freq = None;
        for part in rule.trim().split(';').filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=')?;
            match k.to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match v.to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" => r.interval = v.parse().ok().filter(|n| *n > 0)?,
                "BYDAY" => {
                    for code in v.split(',') {
                        let day = WEEK
                            .iter()
                            .find(|d| weekday_code(**d).eq_ignore_ascii_case(code.trim()))?;
                        r.days.push(*day);
                    }
                }
                "COUNT" => r.end = End::Count(v.parse().ok()?),
                "UNTIL" => r.end = End::Until(asst_core::time::parse_date(v.get(..8)?)?),
                "WKST" => {}
                _ => return None,
            }
        }
        r.freq = freq?;
        if r.freq != Freq::Weekly && !r.days.is_empty() {
            return None;
        }
        Some(r)
    }

    /// The RRULE; `timed` says whether the task has a time, which UNTIL must match.
    pub fn to_rule(&self, timed: bool) -> String {
        let mut parts = vec![format!("FREQ={}", self.freq.code())];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        if self.freq == Freq::Weekly && !self.days.is_empty() {
            let mut days = self.days.clone();
            days.sort_by_key(|d| d.num_days_from_monday());
            days.dedup();
            let codes: Vec<&str> = days.iter().map(|d| weekday_code(*d)).collect();
            parts.push(format!("BYDAY={}", codes.join(",")));
        }
        match &self.end {
            End::Never => {}
            End::Count(n) => parts.push(format!("COUNT={n}")),
            End::Until(d) if timed => parts.push(format!("UNTIL={}T235959Z", d.format("%Y%m%d"))),
            End::Until(d) => parts.push(format!("UNTIL={}", d.format("%Y%m%d"))),
        }
        parts.join(";")
    }
}

// -- reminders --------------------------------------------------------------

/// `5 min`, `1 h`, `1 h 30 min`, `2 days`, `1 week`.
pub fn span(d: Duration) -> String {
    let mins = d.num_minutes().abs();
    match mins {
        m if m >= 10080 && m % 10080 == 0 => plural(m / 10080, "week"),
        m if m >= 1440 && m % 1440 == 0 => plural(m / 1440, "day"),
        m if m >= 60 && m % 60 == 0 => format!("{} h", m / 60),
        m if m > 60 => format!("{} h {} min", m / 60, m % 60),
        m => format!("{m} min"),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

pub fn reminder_label(trigger: &Trigger, task: &Task, now: DateTime<Tz>) -> String {
    let zone = now.timezone();
    match trigger {
        // What iOS and asst write for "when it's due".
        Trigger::Absolute { at }
            if task
                .due
                .as_ref()
                .is_some_and(|d| d.has_time() && d.instant(zone) == *at) =>
        {
            "At the due time".into()
        }
        Trigger::Absolute { at } => capitalize(&fmt::due_label(&When::Utc { at: *at }, now)),
        Trigger::Relative { offset, .. } => match offset.num_seconds() {
            0 => "At the due time".to_string(),
            s if s < 0 => format!("{} before", span(*offset)),
            _ => format!("{} after", span(*offset)),
        },
    }
}

/// When a trigger goes off, for ordering and for "already past".
pub fn trigger_at(trigger: &Trigger, task: &Task, zone: Tz) -> Option<DateTime<Utc>> {
    match trigger {
        Trigger::Absolute { at } => Some(*at),
        Trigger::Relative { offset, .. } => {
            let base = task.due.as_ref()?;
            let at = match base {
                // iOS rings an all-day reminder's relative alarms from 9:00.
                When::Date { date } => {
                    resolve(zone, date.and_time(NaiveTime::from_hms_opt(9, 0, 0)?))
                }
                other => other.instant(zone),
            };
            Some(at + *offset)
        }
    }
}

pub fn priority_name(level: u8) -> &'static str {
    match level {
        1 => "Priority 1: High",
        2 => "Priority 2: Medium",
        3 => "Priority 3: Low",
        _ => "Priority 4: None",
    }
}

#[cfg(test)]
mod tests {
    use asst_core::task::Status;
    use chrono::TimeZone;

    use super::*;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn now() -> DateTime<Tz> {
        // Monday, September 14 2026, 10:00.
        ny().with_ymd_and_hms(2026, 9, 14, 10, 0, 0).unwrap()
    }

    fn date(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn task(uid: &str, list: &str, due: Option<When>) -> TaskView {
        TaskView {
            id: uid.into(),
            href: format!("{list}{uid}.ics"),
            list: list.into(),
            list_name: list.trim_matches('/').into(),
            pending: false,
            issue: None,
            origin: "phone".into(),
            task: Task {
                uid: uid.into(),
                summary: uid.into(),
                description: None,
                status: Status::NeedsAction,
                completed: None,
                priority: 0,
                due,
                rrule: None,
                alarms: Vec::new(),
                location_alarms: Vec::new(),
                parent: None,
                source: None,
                sort_order: None,
                created: None,
                modified: None,
            },
        }
    }

    fn lists() -> Vec<ListView> {
        ["/a/", "/b/"]
            .iter()
            .map(|h| ListView {
                href: (*h).into(),
                name: h.trim_matches('/').to_uppercase(),
                color: None,
                writable: true,
                inbox: *h == "/b/",
                open: 0,
                done: 0,
            })
            .collect()
    }

    fn on(d: NaiveDate) -> Option<When> {
        Some(When::Date { date: d })
    }

    fn uids(s: &Section) -> Vec<&str> {
        s.tasks.iter().map(|t| t.task.uid.as_str()).collect()
    }

    #[test]
    fn a_time_gone_by_is_overdue_the_same_day() {
        let zone = ny();
        // now() is Monday 10:00.
        let timed = |h, m| Some(When::local(date(9, 14).and_hms_opt(h, m, 0).unwrap(), zone));
        let chip = |due: Option<When>| due_chip(&due.unwrap(), now());
        assert_eq!(chip(timed(8, 0)), ("Today 8am".to_string(), "overdue"));
        assert_eq!(chip(timed(10, 30)), ("Today 10:30am".to_string(), "today"));
        // A date alone is today's until the day is over.
        assert_eq!(chip(on(date(9, 14))), ("Today".to_string(), "today"));
        assert_eq!(chip(on(date(9, 13))).1, "overdue");
        assert_eq!(chip(on(date(9, 15))).1, "upcoming");
        let tomorrow_early = Some(When::local(date(9, 15).and_hms_opt(8, 0, 0).unwrap(), zone));
        assert_eq!(chip(tomorrow_early).1, "upcoming");

        // The sidebar and the tray count it as overdue, and still as today's.
        let lists = lists();
        let open = vec![
            task("early", "/a/", timed(8, 0)),
            task("later", "/a/", timed(15, 0)),
            task("all-day", "/a/", on(date(9, 14))),
            task("yesterday", "/a/", on(date(9, 13))),
        ];
        let data = Data {
            lists: &lists,
            open: &open,
            completed: None,
            sunday_first: false,
        };
        let c = Counts::new(&data, &[], now());
        assert_eq!((c.today, c.overdue), (4, 2));
    }

    #[test]
    fn clock_times_read_the_same_everywhere() {
        assert_eq!(clock_label(0), "12am");
        assert_eq!(clock_label(9 * 60), "9am");
        assert_eq!(clock_label(12 * 60), "12pm");
        assert_eq!(clock_label(13 * 60 + 30), "1:30pm");
        assert_eq!(clock_label(23 * 60 + 45), "11:45pm");
        // The end of a block that runs to midnight.
        assert_eq!(clock_label(24 * 60), "12am");
    }

    #[test]
    fn today_splits_overdue_and_counts_match() {
        let lists = lists();
        let open = vec![
            task("late", "/a/", on(date(9, 10))),
            task("now", "/a/", on(date(9, 14))),
            task("soon", "/b/", on(date(9, 15))),
            task("someday", "/b/", None),
        ];
        let data = Data {
            lists: &lists,
            open: &open,
            completed: None,
            sunday_first: false,
        };
        let s = plan(&Nav::Today, &data, &ViewOpts::default(), now());
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].kind.clone(), uids(&s[0])),
            (Kind::Overdue, vec!["late"])
        );
        assert_eq!(s[1].title.as_deref(), Some("Today"));
        assert_eq!(uids(&s[1]), vec!["now"]);

        let c = Counts::new(&data, &[], now());
        assert_eq!((c.today, c.overdue, c.scheduled, c.tomorrow), (2, 1, 1, 1));
        assert_eq!((c.anytime, c.all, c.inbox), (1, 4, 2));
    }

    #[test]
    fn scheduled_has_the_week_then_months() {
        let lists = lists();
        let open = vec![
            task("wed", "/a/", on(date(9, 16))),
            task("late-sep", "/a/", on(date(9, 25))),
            task("oct", "/a/", on(date(10, 2))),
        ];
        let data = Data {
            lists: &lists,
            open: &open,
            completed: None,
            sunday_first: false,
        };
        let s = plan(&Nav::Scheduled, &data, &ViewOpts::default(), now());
        let titles: Vec<_> = s.iter().map(|x| x.title.clone().unwrap()).collect();
        assert_eq!(
            titles,
            [
                "14",
                "15",
                "16",
                "17",
                "18",
                "19",
                "20",
                "September",
                "October"
            ]
        );
        assert_eq!(s[0].note.as_deref(), Some("Today"));
        assert_eq!(uids(&s[2]), vec!["wed"]);
        assert_eq!(s[7].note.as_deref(), Some("21 – 30"));
        assert_eq!(uids(&s[8]), vec!["oct"]);
    }

    #[test]
    fn custom_order_then_due() {
        let mut a = task("a", "/a/", None);
        a.task.sort_order = Some(5);
        let mut b = task("b", "/a/", on(date(9, 20)));
        b.task.sort_order = Some(1);
        let c = task("c", "/a/", on(date(9, 18)));
        let got = sorted(vec![a.clone(), b.clone(), c.clone()], Sort::Custom, ny());
        assert_eq!(
            got.iter().map(|t| t.task.uid.as_str()).collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
        let got = sorted(vec![a, b, c], Sort::Due, ny());
        assert_eq!(
            got.iter().map(|t| t.task.uid.as_str()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
    }

    #[test]
    fn reordering_takes_a_gap_or_renumbers() {
        let with = |uid: &str, n: Option<i64>| {
            let mut t = task(uid, "/a/", None);
            t.task.sort_order = n;
            t
        };
        let hrefs = |v: &[(String, i64)]| {
            v.iter()
                .map(|(h, n)| {
                    (
                        h.trim_start_matches("/a/")
                            .trim_end_matches(".ics")
                            .to_string(),
                        *n,
                    )
                })
                .collect::<Vec<_>>()
        };
        let list = vec![
            with("a", Some(1024)),
            with("b", Some(2048)),
            with("c", Some(3072)),
        ];
        // c between a and b: the middle of the gap.
        assert_eq!(
            hrefs(&reorder(&list, "/a/c.ics", "/a/b.ics", false)),
            [("c".to_string(), 1536)]
        );
        // a to the end: past the last.
        assert_eq!(
            hrefs(&reorder(&list, "/a/a.ics", "/a/c.ics", true)),
            [("a".to_string(), 4096)]
        );
        // No room: renumber.
        let tight = vec![with("a", Some(1)), with("b", Some(2)), with("c", Some(3))];
        assert_eq!(
            hrefs(&reorder(&tight, "/a/c.ics", "/a/b.ics", false)),
            [
                ("a".to_string(), 1024),
                ("c".to_string(), 2048),
                ("b".to_string(), 3072)
            ]
        );
        // Unnumbered tasks get numbers too.
        let loose = vec![with("a", None), with("b", None)];
        assert_eq!(
            hrefs(&reorder(&loose, "/a/b.ics", "/a/a.ics", false)),
            [("b".to_string(), 1024), ("a".to_string(), 2048)]
        );
        assert!(reorder(&list, "/a/a.ics", "/a/a.ics", false).is_empty());
    }

    #[test]
    fn the_cursor_carries_on_from_a_row_that_left() {
        let before = ["a", "b", "c", "d"];
        // The row after it, so a run of `x` goes down the list.
        assert_eq!(
            cursor_after_leaving(&before, "b", &["a", "c", "d"]),
            Some("c")
        );
        // The next row left too: the one after that.
        assert_eq!(cursor_after_leaving(&before, "b", &["a", "d"]), Some("d"));
        // The last row: the one above it.
        assert_eq!(
            cursor_after_leaving(&before, "d", &["a", "b", "c"]),
            Some("c")
        );
        // Rows that arrived do not count as following it.
        assert_eq!(
            cursor_after_leaving(&before, "b", &["a", "new", "c", "d"]),
            Some("c")
        );
        // The only row, or one that was not in the last build (another
        // view): nowhere.
        assert_eq!(cursor_after_leaving(&["a"], "a", &[]), None);
        assert_eq!(cursor_after_leaving(&before, "z", &["a", "b"]), None);
    }

    #[test]
    fn repeat_round_trips() {
        let r = Repeat::parse("FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,MO;COUNT=4").unwrap();
        assert_eq!(r.days, vec![Weekday::Tue, Weekday::Mon]);
        assert_eq!(
            r.to_rule(true),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TU;COUNT=4"
        );
        let until = Repeat {
            end: End::Until(date(12, 31)),
            ..Repeat::default()
        };
        assert_eq!(until.to_rule(false), "FREQ=DAILY;UNTIL=20261231");
        assert_eq!(until.to_rule(true), "FREQ=DAILY;UNTIL=20261231T235959Z");
        assert!(Repeat::parse("FREQ=MONTHLY;BYDAY=MO;BYSETPOS=-1").is_none());
    }

    #[test]
    fn dates_keep_their_time() {
        let at = When::local(date(9, 14).and_hms_opt(17, 30, 0).unwrap(), ny());
        let moved = on_date(Some(&at), date(9, 20), ny());
        assert_eq!(
            moved,
            When::local(date(9, 20).and_hms_opt(17, 30, 0).unwrap(), ny())
        );
        assert_eq!(
            on_date(None, date(9, 20), ny()),
            When::Date { date: date(9, 20) }
        );
        let timed = at_time(
            on(date(9, 18)).as_ref(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            date(9, 14),
            ny(),
        );
        assert_eq!(
            timed,
            When::local(date(9, 18).and_hms_opt(9, 0, 0).unwrap(), ny())
        );
    }

    #[test]
    fn times_in_words() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0);
        assert_eq!(parse_time("5pm"), t(17, 0));
        assert_eq!(parse_time("5:30 PM"), t(17, 30));
        assert_eq!(parse_time("17:45"), t(17, 45));
        assert_eq!(parse_time("930"), t(9, 30));
        assert_eq!(parse_time("12am"), t(0, 0));
        assert_eq!(parse_time("noon"), None);
    }

    #[test]
    fn reminder_words() {
        let mut t = task("r", "/a/", None).task;
        let before = Trigger::Relative {
            offset: Duration::minutes(-90),
            from_due: true,
        };
        assert_eq!(reminder_label(&before, &t, now()), "1 h 30 min before");
        let at = Trigger::Relative {
            offset: Duration::zero(),
            from_due: true,
        };
        assert_eq!(reminder_label(&at, &t, now()), "At the due time");
        let day = Trigger::Relative {
            offset: Duration::days(-1),
            from_due: true,
        };
        assert_eq!(reminder_label(&day, &t, now()), "1 day before");
        let due = When::local(date(9, 15).and_hms_opt(17, 0, 0).unwrap(), ny());
        let absolute = Trigger::Absolute {
            at: due.instant(ny()),
        };
        assert_eq!(reminder_label(&absolute, &t, now()), "Tomorrow 5pm");
        t.due = Some(due);
        assert_eq!(reminder_label(&absolute, &t, now()), "At the due time");
    }

    #[test]
    fn ago_counts_seconds_then_minutes() {
        let at = |secs: i64| now().with_timezone(&Utc) - Duration::seconds(secs);
        assert_eq!(ago(at(2), now()), "just now");
        assert_eq!(ago(at(42), now()), "42 seconds ago");
        assert_eq!(ago(at(61), now()), "1 minute ago");
        assert_eq!(ago(at(150), now()), "2 minutes ago");
        assert_eq!(ago(at(7300), now()), "2 hours ago");
        assert_eq!(ago(at(3 * 86400), now()), "on Sep 11");
        assert_eq!(ago(at(400 * 86400), now()), "on Aug 10, 2025");
    }

    #[test]
    fn work_starts_in_the_checkout_of_the_issues_repo() {
        assert_eq!(
            issue_repo("https://github.com/jaehho/asst/issues/12").as_deref(),
            Some("jaehho/asst")
        );
        assert_eq!(issue_repo("https://example.com/a/b/issues/1"), None);
        assert_eq!(
            checkout_dir("~/projects", "jaehho/asst", "/home/jaeho"),
            std::path::PathBuf::from("/home/jaeho/projects/asst")
        );
        assert_eq!(
            checkout_dir("/srv/src", "jaehho/asst", "/home/jaeho"),
            std::path::PathBuf::from("/srv/src/asst")
        );
        assert_eq!(
            work_command(
                "kitty --directory {dir} gh-co {number}",
                "/home/jaeho/my projects/it's",
                "jaehho/asst",
                12,
                "https://github.com/jaehho/asst/issues/12"
            ),
            "kitty --directory '/home/jaeho/my projects/it'\\''s' gh-co 12"
        );
    }

    #[test]
    fn a_lists_repo_field_links_moves_or_unlinks() {
        for typed in [
            "jaehho/asst",
            "  jaehho/asst  ",
            "https://github.com/jaehho/asst",
            "https://github.com/jaehho/asst/",
            "https://github.com/jaehho/asst.git",
            "git@github.com:jaehho/asst.git",
            "github.com/jaehho/asst/issues/12",
        ] {
            assert_eq!(
                repo_from_text(typed).as_deref(),
                Some("jaehho/asst"),
                "{typed}"
            );
        }
        for typed in [
            "asst",
            "jaehho/",
            "/asst",
            "a/b/c",
            "has space/x",
            "https://example.com/a/b",
        ] {
            assert_eq!(repo_from_text(typed), None, "{typed}");
        }
        assert_eq!(link_edit(None, ""), Ok(LinkEdit::Keep));
        assert_eq!(link_edit(None, " \n"), Ok(LinkEdit::Keep));
        assert_eq!(
            link_edit(None, "jaehho/asst"),
            Ok(LinkEdit::Link("jaehho/asst".into()))
        );
        assert_eq!(
            link_edit(Some("jaehho/asst"), ""),
            Ok(LinkEdit::Unlink("jaehho/asst".into()))
        );
        assert_eq!(
            link_edit(Some("jaehho/asst"), "Jaehho/Asst"),
            Ok(LinkEdit::Keep)
        );
        assert_eq!(
            link_edit(Some("jaehho/asst"), "https://github.com/jaehho/dash"),
            Ok(LinkEdit::Move {
                from: "jaehho/asst".into(),
                to: "jaehho/dash".into()
            })
        );
        assert_eq!(
            link_edit(None, "dash"),
            Err("“dash” is not owner/repo".into())
        );
    }

    #[test]
    fn a_link_says_when_it_was_read_or_what_is_wrong() {
        let link = |synced: Option<i64>, error: Option<&str>| LinkView {
            repo: "jaehho/asst".into(),
            list: "/cal/asst/".into(),
            name: "asst".into(),
            synced: synced.map(|s| now().with_timezone(&Utc) - Duration::seconds(s)),
            error: error.map(str::to_string),
            ..LinkView::default()
        };
        assert_eq!(
            link_state(&link(Some(150), None), now()),
            "Issues of jaehho/asst read 2 minutes ago"
        );
        assert_eq!(
            link_state(&link(None, None), now()),
            "Issues of jaehho/asst: not read yet"
        );
        assert_eq!(
            link_state(&link(Some(61), Some("the server answered 403")), now()),
            "jaehho/asst: the server answered 403 (last read 1 minute ago)"
        );
    }

    #[test]
    fn a_link_that_needs_a_look_says_what_was_found_on_its_own_line() {
        use asst_core::api::LinkState;
        let link = |state: Option<LinkState>, error: Option<&str>| LinkView {
            repo: "jaehho/mail-digest".into(),
            list: "/cal/mail-digest/".into(),
            name: "mail-digest".into(),
            synced: Some(now().with_timezone(&Utc) - Duration::seconds(150)),
            error: error.map(str::to_string),
            state,
            note: state.map(|_| {
                "archived on GitHub, first seen 2026-10-18; no open tasks here".to_string()
            }),
            ..LinkView::default()
        };
        let archived = link(Some(LinkState::Archived), None);
        assert_eq!(
            link_state(&archived, now()),
            "Issues of jaehho/mail-digest read 2 minutes ago\n\
             archived on GitHub, first seen 2026-10-18; no open tasks here"
        );
        // A fine link has no second line, even with a stale note
        let fine = LinkView {
            note: Some("old".into()),
            ..link(None, None)
        };
        assert_eq!(
            link_state(&fine, now()),
            "Issues of jaehho/mail-digest read 2 minutes ago"
        );

        // The mark warns when the pass failed or the issues are not read,
        // not for a repo that is merely archived or renamed
        assert!(!link_warns(&link(None, None)));
        assert!(!link_warns(&archived));
        assert!(!link_warns(&link(Some(LinkState::Renamed), None)));
        assert!(link_warns(&link(None, Some("the server answered 403"))));
        for stalled in [
            LinkState::Unreachable,
            LinkState::Replaced,
            LinkState::Conflict,
        ] {
            assert!(link_warns(&link(Some(stalled), None)), "{stalled:?}");
        }
    }

    #[test]
    fn names_in_reading_order() {
        let mut names = vec!["Task 10", "task 2", "Task 1", "Apple", "Task 02", "b"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            ["Apple", "b", "Task 1", "task 2", "Task 02", "Task 10"]
        );
    }

    #[test]
    fn due_filters_take_in_what_is_overdue() {
        let today = date(9, 16); // a Wednesday
        let keeps = |f: DueFilter, d: Option<NaiveDate>| f.keeps(d, today, false);
        assert!(keeps(DueFilter::Today, Some(date(9, 1))));
        assert!(!keeps(DueFilter::Today, Some(date(9, 17))));
        assert!(
            keeps(DueFilter::Week, Some(date(9, 20))),
            "Sunday ends a Monday week"
        );
        assert!(!keeps(DueFilter::Week, Some(date(9, 21))));
        assert!(
            !DueFilter::Week.keeps(Some(date(9, 20)), today, true),
            "Saturday ends a Sunday week"
        );
        assert!(keeps(DueFilter::Seven, Some(date(9, 22))));
        assert!(keeps(DueFilter::Month, Some(date(9, 30))));
        assert!(!keeps(DueFilter::Month, Some(date(10, 1))));
        assert!(keeps(DueFilter::NoDate, None) && !keeps(DueFilter::NoDate, Some(today)));
        assert!(!keeps(DueFilter::Today, None));
    }

    #[test]
    fn views_turn_around_and_filter() {
        let lists = lists();
        let open = vec![
            task("b", "/a/", on(date(9, 20))),
            task("a", "/a/", on(date(9, 14))),
            task("c", "/a/", None),
        ];
        let data = Data {
            lists: &lists,
            open: &open,
            completed: None,
            sunday_first: false,
        };
        let opts = ViewOpts {
            sort: Some(Sort::Name),
            descending: true,
            ..ViewOpts::default()
        };
        let s = plan(&Nav::List("/a/".into()), &data, &opts, now());
        assert_eq!(uids(&s[0]), ["c", "b", "a"]);
        let opts = ViewOpts {
            due: DueFilter::Week,
            ..opts
        };
        let s = plan(&Nav::List("/a/".into()), &data, &opts, now());
        assert_eq!(uids(&s[0]), ["b", "a"]);
    }

    #[test]
    fn tasks_as_markdown() {
        let mut t = task("Buy milk", "/a/", on(date(9, 15)));
        t.task.description = Some("2%\n\nthe big one".into());
        let plain = task("Call", "/a/", None);
        assert_eq!(
            tasks_markdown(&[t, plain], now()),
            "- [ ] Buy milk (tomorrow)\n  2%\n\n  the big one\n- [ ] Call\n"
        );
    }

    #[test]
    fn nav_keys_round_trip() {
        for n in Nav::filters()
            .into_iter()
            .chain([Nav::List("/cal/x/".into())])
        {
            assert_eq!(Nav::from_key(&n.key()), Some(n));
        }
    }

    fn with(task: &TaskView, priority: u8, due: Option<When>) -> TaskView {
        let mut t = task.clone();
        t.task.priority = priority;
        t.task.due = due;
        t
    }

    #[test]
    fn matrix_boxes_by_priority_and_due() {
        // p1–p2 is important; due on or before today is urgent.
        let day = on(date(9, 14));
        let late = on(date(9, 20));
        let early = on(date(9, 1));
        assert_eq!(
            quadrant_of(1, day.as_ref(), date(9, 14), ny()),
            Quadrant::Do
        );
        assert_eq!(
            quadrant_of(5, early.as_ref(), date(9, 14), ny()),
            Quadrant::Do
        );
        assert_eq!(
            quadrant_of(1, late.as_ref(), date(9, 14), ny()),
            Quadrant::Schedule
        );
        assert_eq!(quadrant_of(5, None, date(9, 14), ny()), Quadrant::Schedule);
        assert_eq!(
            quadrant_of(9, day.as_ref(), date(9, 14), ny()),
            Quadrant::Delegate
        );
        assert_eq!(
            quadrant_of(0, early.as_ref(), date(9, 14), ny()),
            Quadrant::Delegate
        );
        assert_eq!(
            quadrant_of(9, late.as_ref(), date(9, 14), ny()),
            Quadrant::Drop
        );
        assert_eq!(quadrant_of(0, None, date(9, 14), ny()), Quadrant::Drop);
    }

    #[test]
    fn matrix_view_fills_all_four_boxes() {
        let lists = lists();
        let open = vec![
            with(&task("do", "/a/", on(date(9, 14))), 1, on(date(9, 14))),
            with(&task("plan", "/a/", on(date(9, 20))), 1, on(date(9, 20))),
            with(&task("rush", "/b/", on(date(9, 10))), 0, on(date(9, 10))),
            with(&task("later", "/b/", None), 9, None),
        ];
        let data = Data {
            lists: &lists,
            open: &open,
            completed: None,
            sunday_first: false,
        };
        let s = plan(&Nav::Matrix, &data, &ViewOpts::default(), now());
        assert_eq!(s.len(), 4);
        assert_eq!(uids(&s[0]), ["do"]);
        assert_eq!(uids(&s[1]), ["plan"]);
        assert_eq!(uids(&s[2]), ["rush"]);
        assert_eq!(uids(&s[3]), ["later"]);
        assert!(s.iter().all(|s| s.keep));
    }

    #[test]
    fn retarget_only_moves_the_wrong_axis() {
        let base = task("x", "/a/", on(date(9, 14)));
        // Already in Do: nothing to do.
        let do_now = with(&base, 1, on(date(9, 14)));
        assert_eq!(
            retarget(&do_now.task, Quadrant::Do, date(9, 14), ny()),
            Change::default()
        );
        // Across importance only (Do → Delegate): p3, date kept.
        let c = retarget(&do_now.task, Quadrant::Delegate, date(9, 14), ny());
        assert_eq!(c.priority, Some(3));
        assert_eq!(c.due, None);
        // Across urgency only (Do → Schedule): no date, priority kept.
        let c = retarget(&do_now.task, Quadrant::Schedule, date(9, 14), ny());
        assert_eq!(c.priority, None);
        assert_eq!(c.due, Some(None));
        // Across both (Do → Drop): p3 and no date.
        let c = retarget(&do_now.task, Quadrant::Drop, date(9, 14), ny());
        assert_eq!(c.priority, Some(3));
        assert_eq!(c.due, Some(None));
        // Into Do from a dated-later, no-priority task: p2 and today.
        let soft = with(&base, 0, on(date(9, 20)));
        let c = retarget(&soft.task, Quadrant::Do, date(9, 14), ny());
        assert_eq!(c.priority, Some(2));
        assert_eq!(c.due, Some(Some(When::Date { date: date(9, 14) })));
        // Dropping a repeat out of urgency clears the rule with the date.
        let mut rep = with(&base, 0, on(date(9, 14)));
        rep.task.rrule = Some("FREQ=WEEKLY".into());
        let c = retarget(&rep.task, Quadrant::Drop, date(9, 14), ny());
        assert_eq!(c.due, Some(None));
        assert_eq!(c.rrule, Some(None));
    }

    fn slot(uid: &str, start: u32, duration: u32) -> PlanSlot {
        PlanSlot {
            uid: uid.into(),
            date: date(9, 14),
            start,
            duration,
        }
    }

    fn placed(p: &DayPlan, uid: &str) -> Option<(u32, u32, usize, usize)> {
        p.blocks
            .iter()
            .find(|b| b.task.task.uid == uid)
            .map(|b| (b.start, b.duration, b.col, b.cols))
    }

    #[test]
    fn plan_places_blocks_and_leaves_the_rest_in_the_tray() {
        let open = vec![
            task("late", "/a/", on(date(9, 10))),
            task("now", "/a/", on(date(9, 14))),
            task("soon", "/b/", on(date(9, 15))),
            task("someday", "/b/", None),
            task("other", "/a/", on(date(9, 14))),
        ];
        let slots = vec![slot("now", 9 * 60, 60), slot("gone", 10 * 60, 30)];
        let p = day_plan(&open, &slots, now(), date(9, 14));
        assert_eq!((p.hour_start, p.hour_end), (PLAN_HOUR_START, PLAN_HOUR_END));
        // `now` sits on the grid; `gone` is not open and is dropped.
        assert_eq!(placed(&p, "now"), Some((9 * 60, 60, 0, 1)));
        assert_eq!(placed(&p, "gone"), None);
        // Tray: overdue and due today, unplaced; a later date stays out and
        // the undated wait in their own section.
        let tray: Vec<&str> = p.tray.iter().map(|t| t.task.uid.as_str()).collect();
        assert_eq!(tray, ["late", "other"]);
        let undated: Vec<&str> = p.no_date.iter().map(|t| t.task.uid.as_str()).collect();
        assert_eq!(undated, ["someday"]);
        // 10:00 on Sep 14 2026.
        assert_eq!(p.now_min, Some(10 * 60));
        // A slot for a task the open set no longer holds does not count.
        let c = Counts::new(
            &Data {
                lists: &lists(),
                open: &open,
                completed: None,
                sunday_first: false,
            },
            &slots,
            now(),
        );
        assert_eq!(c.day, 1);
    }

    #[test]
    fn overlapping_blocks_share_a_width_side_by_side() {
        // A 09:00–11:00, B 09:30–11:00, C 10:00–11:30: every pair overlaps,
        // so three columns. D 12:00–12:30 is alone.
        let open = vec![
            task("a", "/a/", on(date(9, 14))),
            task("b", "/a/", on(date(9, 14))),
            task("c", "/a/", on(date(9, 14))),
            task("d", "/a/", on(date(9, 14))),
        ];
        let slots = vec![
            slot("a", 9 * 60, 120),
            slot("b", 9 * 60 + 30, 90),
            slot("c", 10 * 60, 90),
            slot("d", 12 * 60, 30),
        ];
        let p = day_plan(&open, &slots, now(), date(9, 14));
        assert_eq!(placed(&p, "a"), Some((9 * 60, 120, 0, 3)));
        assert_eq!(placed(&p, "b"), Some((9 * 60 + 30, 90, 1, 3)));
        assert_eq!(placed(&p, "c"), Some((10 * 60, 90, 2, 3)));
        // d does not overlap anyone: its own column and width.
        assert_eq!(placed(&p, "d"), Some((12 * 60, 30, 0, 1)));
    }

    #[test]
    fn overlapping_groups_do_not_share_columns() {
        // Two pairs, an hour apart: each pair packs into two columns of its
        // own, and a block that only spans one of them still joins that
        // group through the overlap.
        let open = vec![
            task("a", "/a/", on(date(9, 14))),
            task("b", "/a/", on(date(9, 14))),
            task("c", "/a/", on(date(9, 14))),
            task("d", "/a/", on(date(9, 14))),
        ];
        let slots = vec![
            slot("a", 9 * 60, 60),
            slot("b", 9 * 60 + 30, 60),
            slot("c", 11 * 60, 60),
            slot("d", 11 * 60 + 30, 60),
        ];
        let p = day_plan(&open, &slots, now(), date(9, 14));
        assert_eq!(placed(&p, "a"), Some((9 * 60, 60, 0, 2)));
        assert_eq!(placed(&p, "b"), Some((9 * 60 + 30, 60, 1, 2)));
        assert_eq!(placed(&p, "c"), Some((11 * 60, 60, 0, 2)));
        assert_eq!(placed(&p, "d"), Some((11 * 60 + 30, 60, 1, 2)));
    }

    #[test]
    fn place_moves_and_prunes_slots() {
        let mut slots = Vec::new();
        let today = date(9, 14);
        place(&mut slots, "a", today, 9 * 60, 30);
        place(&mut slots, "a", today, 10 * 60, 45);
        place(&mut slots, "b", today, 11 * 60, 30);
        place(&mut slots, "c", date(9, 15), 9 * 60, 30);
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0].start, 10 * 60);
        assert_eq!(slots[0].duration, 45);
        // A live set without `b` drops only that slot, including other days.
        let live: HashSet<&str> = ["a", "c"].into_iter().collect();
        assert!(prune_slots(&mut slots, &live));
        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|s| s.uid != "b"));
        // Moving off the day takes the slot out.
        unplace(&mut slots, "a", today);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].uid, "c");
        // Durations are floored at the minimum, and kept inside the day.
        let mut short = Vec::new();
        place(&mut short, "a", today, 23 * 60 + 50, 5);
        assert_eq!(short[0].duration, 10);
        assert_eq!(short[0].start + short[0].duration, 24 * 60);
        let mut floor = Vec::new();
        place(&mut floor, "a", today, 9 * 60, 5);
        assert_eq!(floor[0].duration, PLAN_MIN_DURATION);
    }

    #[test]
    fn carrying_a_day_forward_moves_only_its_open_slots() {
        let (mon, tue, wed) = (date(9, 14), date(9, 15), date(9, 16));
        // Never into a day that is over
        assert_eq!(carry_target(mon, mon), tue);
        assert_eq!(carry_target(tue, mon), wed);
        assert_eq!(carry_target(date(9, 10), mon), mon);
        assert_eq!(carry_target(date(9, 13), mon), mon);

        let mut slots = Vec::new();
        place(&mut slots, "a", mon, 9 * 60, 30);
        place(&mut slots, "b", mon, 10 * 60, 60);
        place(&mut slots, "done", mon, 11 * 60, 30);
        place(&mut slots, "c", tue, 8 * 60, 30);
        // b is on tomorrow already, at its own time
        place(&mut slots, "b", tue, 15 * 60, 45);
        let original = slots.clone();
        let open: HashSet<&str> = ["a", "b", "c"].into_iter().collect();

        let carried = carry_forward(&mut slots, &open, mon, tue);
        assert_eq!(carried.moved.len(), 2);
        assert_eq!(carried.created, ["a"]);
        let on = |d: NaiveDate| {
            let mut v: Vec<(String, u32, u32)> = slots
                .iter()
                .filter(|s| s.date == d)
                .map(|s| (s.uid.clone(), s.start, s.duration))
                .collect();
            v.sort();
            v
        };
        // a keeps its time of day; b keeps the slot it had; the finished
        // task's slot is left where it was
        assert_eq!(on(mon), [("done".to_string(), 11 * 60, 30)]);
        assert_eq!(
            on(tue),
            [
                ("a".to_string(), 9 * 60, 30),
                ("b".to_string(), 15 * 60, 45),
                ("c".to_string(), 8 * 60, 30),
            ]
        );
        // Nothing left to carry, nothing happens
        let again = carry_forward(&mut slots, &open, mon, tue);
        assert_eq!(again, Carried::default());
        assert_eq!(
            carry_forward(&mut slots, &open, tue, tue),
            Carried::default()
        );

        // Taking it back restores every slot, as it was
        uncarry(&mut slots, &carried);
        let key = |v: &mut Vec<PlanSlot>| v.sort_by_key(|s| (s.date, s.uid.clone()));
        let mut restored = slots.clone();
        let mut expected = original;
        key(&mut restored);
        key(&mut expected);
        assert_eq!(restored, expected);
        // An empty carry takes nothing back
        uncarry(&mut slots, &Carried::default());
        assert_eq!(slots.len(), 5);
    }

    #[test]
    fn snap_points() {
        assert_eq!(snap(0), 0);
        assert_eq!(snap(7), 0);
        assert_eq!(snap(8), PLAN_SNAP);
        assert_eq!(snap(14), PLAN_SNAP);
        assert_eq!(snap(15), 15);
        assert_eq!(snap(22), 15);
        assert_eq!(snap(23), 30);
        assert_eq!(snap(9 * 60), 9 * 60);
        assert_eq!(snap(9 * 60 + 7), 9 * 60);
        assert_eq!(snap(9 * 60 + 8), 9 * 60 + 15);
        assert_eq!(snap(-30), 0);
        assert_eq!(snap(24 * 60), 24 * 60 - 1);
    }

    #[test]
    fn now_line_only_claims_a_place_on_the_grid() {
        let on = |h, m, s| ny().with_ymd_and_hms(2026, 9, 14, h, m, s).unwrap();
        assert_eq!(now_on_grid(on(10, 0, 0), 7, 23), Some(10 * 60));
        assert_eq!(now_on_grid(on(7, 0, 0), 7, 23), Some(7 * 60));
        assert_eq!(now_on_grid(on(22, 59, 0), 7, 23), Some(22 * 60 + 59));
        // Outside 07:00–23:00: no claim, not a pin at the top rule.
        assert_eq!(now_on_grid(on(0, 59, 0), 7, 23), None);
        assert_eq!(now_on_grid(on(6, 59, 0), 7, 23), None);
        assert_eq!(now_on_grid(on(23, 0, 0), 7, 23), None);
        // Seconds round the minute the same way the grid does.
        assert_eq!(now_on_grid(on(10, 0, 40), 7, 23), Some(10 * 60 + 1));
        // A full-day grid holds every minute, including the round past midnight.
        assert_eq!(now_on_grid(on(0, 0, 0), 0, 24), Some(0));
        assert_eq!(now_on_grid(on(0, 59, 0), 0, 24), Some(59));
        assert_eq!(now_on_grid(on(23, 59, 0), 0, 24), Some(23 * 60 + 59));
        assert_eq!(now_on_grid(on(23, 59, 40), 0, 24), Some(24 * 60 - 1));
        // day_plan carries the rule through.
        let p = day_plan(&[], &[], on(0, 59, 0), date(9, 14));
        assert_eq!(p.now_min, Some(59));
        let p = day_plan(&[], &[], on(10, 0, 0), date(9, 14));
        assert_eq!(p.now_min, Some(10 * 60));
        // Only the wall-clock today wears a now-line.
        let p = day_plan(&[], &[], on(10, 0, 0), date(9, 15));
        assert_eq!(p.now_min, None);
    }

    #[test]
    fn day_plan_follows_the_viewed_date() {
        let open = vec![
            task("late", "/a/", on(date(9, 10))),
            task("now", "/a/", on(date(9, 14))),
            task("soon", "/b/", on(date(9, 15))),
        ];
        let slots = vec![
            PlanSlot {
                uid: "now".into(),
                date: date(9, 14),
                start: 9 * 60,
                duration: 60,
            },
            PlanSlot {
                uid: "soon".into(),
                date: date(9, 15),
                start: 11 * 60,
                duration: 30,
            },
            PlanSlot {
                uid: "now".into(),
                date: date(9, 15),
                start: 14 * 60,
                duration: 30,
            },
        ];
        // Today: `now` is on the grid; `soon` is due tomorrow so it is not
        // in the backlog tray either.
        let p = day_plan(&open, &slots, now(), date(9, 14));
        assert_eq!(p.day, date(9, 14));
        assert_eq!(placed(&p, "now"), Some((9 * 60, 60, 0, 1)));
        assert_eq!(placed(&p, "soon"), None);
        let tray: Vec<&str> = p.tray.iter().map(|t| t.task.uid.as_str()).collect();
        assert_eq!(tray, ["late"]);
        assert_eq!(p.now_min, Some(10 * 60));
        // Tomorrow: that day's slots only. The tray is what is due by then
        // minus what sits on the viewed day, so `now` (placed tomorrow too)
        // leaves it and `late` stays. A slot on today does not empty the
        // tray for another day.
        let p = day_plan(&open, &slots, now(), date(9, 15));
        assert_eq!(p.day, date(9, 15));
        assert_eq!(placed(&p, "now"), Some((14 * 60, 30, 0, 1)));
        assert_eq!(placed(&p, "soon"), Some((11 * 60, 30, 0, 1)));
        let tray: Vec<&str> = p.tray.iter().map(|t| t.task.uid.as_str()).collect();
        assert_eq!(tray, ["late"]);
        assert_eq!(p.now_min, None);
    }

    #[test]
    fn the_tray_offers_what_is_due_by_the_viewed_day_and_what_has_no_date() {
        let urgent = |mut t: TaskView| {
            t.task.priority = 1;
            t
        };
        let open = vec![
            task("late", "/a/", on(date(9, 10))),
            task("today", "/a/", on(date(9, 14))),
            task("tomorrow", "/a/", on(date(9, 15))),
            task("friday", "/a/", on(date(9, 18))),
            task("plain", "/a/", None),
            urgent(task("urgent", "/b/", None)),
            task("planned", "/b/", None),
        ];
        let uids = |tasks: &[TaskView]| -> Vec<String> {
            tasks.iter().map(|t| t.task.uid.clone()).collect()
        };
        let slots = vec![
            slot("planned", 9 * 60, 30),
            PlanSlot {
                uid: "plain".into(),
                date: date(9, 20),
                start: 9 * 60,
                duration: 30,
            },
        ];
        // Today: what is due by today, nothing dated later, and the undated
        // in priority order. A task placed today is in neither.
        let p = day_plan(&open, &slots, now(), date(9, 14));
        assert_eq!(uids(&p.tray), ["late", "today"]);
        assert_eq!(uids(&p.no_date), ["urgent", "plain"]);
        // A later day takes in what falls due by it, so a task due Friday
        // can be planned on Friday; a slot on another day (plain, on the
        // 20th) does not take the task out of today's list.
        let p = day_plan(&open, &slots, now(), date(9, 15));
        assert_eq!(uids(&p.tray), ["late", "today", "tomorrow"]);
        let p = day_plan(&open, &slots, now(), date(9, 18));
        assert_eq!(uids(&p.tray), ["late", "today", "tomorrow", "friday"]);
        // A day gone by offers what was due by then, not what came after.
        let p = day_plan(&open, &slots, now(), date(9, 12));
        assert_eq!(uids(&p.tray), ["late"]);
        // The day a task is placed on, it leaves No date.
        let p = day_plan(&open, &slots, now(), date(9, 20));
        assert_eq!(uids(&p.no_date), ["urgent", "planned"]);
        // Nothing open, nothing to place.
        let p = day_plan(&[], &[], now(), date(9, 14));
        assert!(p.tray.is_empty() && p.no_date.is_empty());
    }

    #[test]
    fn scroll_keeps_now_in_view_without_leaving_the_grid() {
        let z = PLAN_PX_PER_HOUR;
        // 00:00–24:00 is 24 hours on the shared scale.
        assert_eq!(grid_height(0, 24, z), 24.0 * z);
        assert_eq!(grid_height(7, 23, z), 16.0 * z);
        let day = grid_height(0, 24, z);
        // Morning: already in view from the top, never a negative offset.
        assert_eq!(scroll_to_min(0, 0, 24, 400.0, z), 0.0);
        // The line sits a third down the viewport so the hours after it show.
        let y = scroll_to_min(10 * 60, 0, 24, 400.0, z);
        let want = 10.0 * z - 400.0 / 3.0;
        assert_eq!(y, want);
        // Late enough that the bottom of the day is the limit.
        assert_eq!(scroll_to_min(24 * 60 - 1, 0, 24, 400.0, z), day - 400.0);
        // A viewport taller than the day has nowhere to scroll.
        assert_eq!(scroll_to_min(12 * 60, 0, 24, day + 10.0, z), 0.0);
        // An hour_start offset moves y, as minutes_at does. 10:00 on a
        // 07:00 grid is 3 h in, less than the third-viewport lift, so the
        // top rule is where the scroller rests.
        let y7 = scroll_to_min(10 * 60, 7, 23, 400.0, z);
        assert_eq!(y7, (3.0 * z - 400.0 / 3.0).max(0.0));
        let y7 = scroll_to_min(14 * 60, 7, 23, 400.0, z);
        assert_eq!(y7, 7.0 * z - 400.0 / 3.0);
    }

    #[test]
    fn zoom_scales_the_whole_day_mapping() {
        // Two zooms agree on minutes and disagree on pixels by the ratio.
        let (z0, z1) = (PLAN_PX_PER_HOUR, 2.0 * PLAN_PX_PER_HOUR);
        assert_eq!(minutes_at(y_at(9.0 * 60.0, 0, z1), 0, z1), 9 * 60);
        assert_eq!(y_at(10.0 * 60.0, 0, z1), 2.0 * y_at(10.0 * 60.0, 0, z0));
        assert_eq!(minutes_at(10.0 * z1, 0, z1), 10 * 60);
        assert_eq!(grid_height(0, 24, z1), 2.0 * grid_height(0, 24, z0));
        // A pointer dy covers half the minutes when the grid is twice as tall.
        assert_eq!(move_start(9 * 60, z1, z1), 10 * 60);
        assert_eq!(move_start(9 * 60, z1, z0), 9 * 60 + 120);
        // Clamps: below, above, and non-finite fall back to the default.
        assert_eq!(clamp_px_per_hour(1.0), PX_PER_HOUR_MIN);
        assert_eq!(clamp_px_per_hour(1000.0), PX_PER_HOUR_MAX);
        assert_eq!(clamp_px_per_hour(f64::NAN), PLAN_PX_PER_HOUR);
        assert_eq!(clamp_px_per_hour(PLAN_PX_PER_HOUR), PLAN_PX_PER_HOUR);
    }

    #[test]
    fn span_columns_pack_each_lane_on_its_own() {
        // One span alone: full width.
        assert_eq!(span_columns(&[(0.0, 60.0)]), [(0, 1)]);
        // Two overlapping spans: side by side, shared width.
        assert_eq!(span_columns(&[(0.0, 60.0), (30.0, 90.0)]), [(0, 2), (1, 2)]);
        // Separate groups do not share columns.
        assert_eq!(
            span_columns(&[(0.0, 60.0), (30.0, 90.0), (120.0, 180.0)]),
            [(0, 2), (1, 2), (0, 1)]
        );
    }

    #[test]
    fn zoom_keeps_a_minute_under_the_pointer() {
        // After a zoom, the scroll that holds `mins` at `keep_y` from the
        // top of the viewport is y_at(mins) − keep_y, clamped to the day.
        let (z0, z1) = (PLAN_PX_PER_HOUR, 2.0 * PLAN_PX_PER_HOUR);
        let mins = 10 * 60;
        let keep_y = 80.0;
        let scroll = scroll_keep_min(mins, 0, 24, 400.0, keep_y, z1);
        assert_eq!(scroll, y_at(mins as f64, 0, z1) - keep_y);
        // The minute still reads at keep_y from the new scroll.
        assert_eq!(minutes_at(scroll + keep_y, 0, z1), mins);
        // Clamped at the top and bottom of the day.
        assert_eq!(scroll_keep_min(0, 0, 24, 400.0, keep_y, z1), 0.0);
        let end = scroll_keep_min(24 * 60 - 1, 0, 24, 400.0, 10.0, z1);
        assert_eq!(end, grid_height(0, 24, z1) - 400.0);
        // At the default zoom the same formula holds.
        assert_eq!(
            scroll_keep_min(mins, 0, 24, 400.0, keep_y, z0),
            y_at(mins as f64, 0, z0) - keep_y
        );
    }

    fn at(h: u32, m: u32) -> DateTime<Tz> {
        ny().with_ymd_and_hms(2026, 9, 14, h, m, 0).unwrap()
    }

    #[test]
    fn place_takes_the_first_free_slot_from_nine() {
        let t = task("t", "/a/", None).task;
        let (mon, tue) = (date(9, 14), date(9, 15));
        let slot_on = |d, uid: &str, start, duration| PlanSlot {
            uid: uid.into(),
            date: d,
            start,
            duration,
        };
        // Another day, before nine: 9:00, never the grid's midnight row.
        assert_eq!(place_start(&t, &[], tue, at(7, 0)), 9 * 60);
        // Today, before nine, is the same.
        assert_eq!(place_start(&t, &[], mon, at(2, 19)), 9 * 60);
        // A busy morning pushes it past, with no gap left between.
        let busy = [
            slot_on(tue, "a", 9 * 60, 30),
            slot_on(tue, "b", 9 * 60 + 30, 30),
        ];
        assert_eq!(place_start(&t, &busy, tue, at(7, 0)), 10 * 60);
        // Another day's slots are not in the way.
        assert_eq!(place_start(&t, &busy, mon, at(7, 0)), 9 * 60);
        // A gap too small for the default length is skipped; one that fits
        // exactly is taken.
        let gap = [
            slot_on(tue, "a", 9 * 60, 30),
            slot_on(tue, "b", 9 * 60 + 45, 30),
        ];
        assert_eq!(place_start(&t, &gap, tue, at(7, 0)), 10 * 60 + 15);
        let fits = [
            slot_on(tue, "a", 9 * 60, 30),
            slot_on(tue, "b", 10 * 60, 30),
        ];
        assert_eq!(place_start(&t, &fits, tue, at(7, 0)), 9 * 60 + 30);
        // A slot that ends off the grid hands over on the next rule.
        let odd = [slot_on(tue, "a", 9 * 60, 40)];
        assert_eq!(place_start(&t, &odd, tue, at(7, 0)), 9 * 60 + 45);
        // Slots listed out of order, one covering the others.
        let wide = [
            slot_on(tue, "b", 9 * 60 + 30, 30),
            slot_on(tue, "a", 9 * 60, 120),
        ];
        assert_eq!(place_start(&t, &wide, tue, at(7, 0)), 11 * 60);
        // Today after nine looks from now (snapped), not into the past.
        assert_eq!(place_start(&t, &[], mon, at(14, 8)), 14 * 60 + 15);
        let now_busy = [slot_on(mon, "a", 14 * 60 + 15, 60)];
        assert_eq!(place_start(&t, &now_busy, mon, at(14, 8)), 15 * 60 + 15);
        // Late in the day with no room left: where the search began.
        let full = [slot_on(mon, "a", 22 * 60, 120)];
        assert_eq!(place_start(&t, &full, mon, at(23, 40)), 23 * 60 + 45);
        let packed = [slot_on(tue, "a", 9 * 60, 15 * 60)];
        assert_eq!(place_start(&t, &packed, tue, at(7, 0)), 9 * 60);
    }

    #[test]
    fn place_keeps_the_time_a_task_already_has() {
        let zone = ny();
        let timed = |h, m| {
            let mut t = task("t", "/a/", None).task;
            t.due = Some(When::local(date(9, 14).and_hms_opt(h, m, 0).unwrap(), zone));
            t
        };
        let slots = [PlanSlot {
            uid: "a".into(),
            date: date(9, 14),
            start: 15 * 60,
            duration: 60,
        }];
        // The minute it names, even off the grid and over another slot.
        assert_eq!(
            place_start(&timed(15, 20), &slots, date(9, 14), at(7, 0)),
            15 * 60 + 20
        );
        // On another day (an overdue task brought forward) the clock stays.
        assert_eq!(
            place_start(&timed(15, 20), &[], date(9, 17), at(7, 0)),
            15 * 60 + 20
        );
        // A date alone has no clock: the free slot, as for no date.
        let dated = task("d", "/a/", on(date(9, 14))).task;
        assert_eq!(place_start(&dated, &[], date(9, 14), at(7, 0)), 9 * 60);
    }

    #[test]
    fn a_day_opens_an_hour_before_its_first_block() {
        assert_eq!(day_rest_min(Some(9.0 * 60.0)), 8 * 60);
        assert_eq!(day_rest_min(Some(9.0 * 60.0 + 30.0)), 8 * 60 + 30);
        // Never above midnight.
        assert_eq!(day_rest_min(Some(30.0)), 0);
        // An empty day opens on the morning.
        assert_eq!(day_rest_min(None), 7 * 60);
    }

    #[test]
    fn keyboard_nudge_snaps_to_the_grid() {
        assert_eq!(nudge_start(9 * 60, 15), 9 * 60 + 15);
        assert_eq!(nudge_start(9 * 60, -15), 8 * 60 + 45);
        assert_eq!(nudge_start(9 * 60 + 7, 15), 9 * 60 + 15);
        assert_eq!(nudge_start(0, -15), 0);
    }

    #[test]
    fn pointer_y_snaps_to_the_grid() {
        let z = PLAN_PX_PER_HOUR;
        // 07:00 at the top of a grid that starts at 7.
        assert_eq!(minutes_at(0.0, 7, z), 7 * 60);
        assert_eq!(minutes_at(z, 7, z), 8 * 60);
        // Half an hour, a quarter, and a point that snaps up.
        assert_eq!(minutes_at(z / 2.0, 7, z), 7 * 60 + 30);
        assert_eq!(minutes_at(z / 4.0, 7, z), 7 * 60 + 15);
        assert_eq!(minutes_at(9.0, 7, z), 7 * 60 + 15);
        // The move and edge drags share the same pixel scale.
        assert_eq!(move_start(9 * 60, z, z), 10 * 60);
        assert_eq!(move_start(9 * 60, -z / 2.0, z), 8 * 60 + 30);
        assert_eq!(edge_end(9 * 60, 30, z / 2.0, z), 10 * 60);
        // y_at is the same mapping the widgets use: midnight at y 0 on a
        // full-day grid, and each hour one `z` further down.
        assert_eq!(y_at(0.0, 0, z), 0.0);
        assert_eq!(y_at(10.0 * 60.0, 0, z), 10.0 * z);
        assert_eq!(y_at(10.0 * 60.0, 7, z), 3.0 * z);
        // A snapped minute round-trips through the shared scale.
        assert_eq!(minutes_at(y_at(9.0 * 60.0, 0, z), 0, z), 9 * 60);
    }

    #[test]
    fn ghost_at_each_hour_boundary() {
        let t = task("g", "/a/", on(date(9, 14)));
        for hour in PLAN_HOUR_START..PLAN_HOUR_END {
            let g = ghost_block(&[], &t, hour * 60, PLAN_DEFAULT_DURATION);
            assert_eq!((g.start, g.duration, g.col, g.cols), (hour * 60, 30, 0, 1));
        }
        // The pointer path and the ghost agree at an hour.
        let z = PLAN_PX_PER_HOUR;
        let y = y_at(10.0 * 60.0, PLAN_HOUR_START, z);
        let start = minutes_at(y, PLAN_HOUR_START, z);
        let g = ghost_block(&[], &t, start, PLAN_DEFAULT_DURATION);
        assert_eq!(g.start, 10 * 60);
    }

    #[test]
    fn ghost_over_existing_blocks_takes_the_next_column() {
        let open = vec![
            task("a", "/a/", on(date(9, 14))),
            task("g", "/a/", on(date(9, 14))),
        ];
        let slots = vec![slot("a", 9 * 60, 60)];
        let p = day_plan(&open, &slots, now(), date(9, 14));
        let t = task("g", "/a/", on(date(9, 14)));
        // Overlapping `a` joins its group in the free column.
        let g = ghost_block(&p.blocks, &t, 9 * 60 + 30, 30);
        assert_eq!(
            (g.start, g.duration, g.col, g.cols),
            (9 * 60 + 30, 30, 1, 2)
        );
        // Next to `a`, not on it: alone, full width.
        let g = ghost_block(&p.blocks, &t, 10 * 60, 30);
        assert_eq!((g.start, g.duration, g.col, g.cols), (10 * 60, 30, 0, 1));
        // A move: the old home is not in the layout, so the new one can
        // take column 0 even while the original still sits on the grid.
        let a = open[0].clone();
        let g = ghost_block(&p.blocks, &a, 9 * 60 + 30, 60);
        assert_eq!(
            (g.start, g.duration, g.col, g.cols),
            (9 * 60 + 30, 60, 0, 1)
        );
        // Durations clamp the same way a real drop would.
        let g = ghost_block(&p.blocks, &t, 23 * 60 + 50, 30);
        assert_eq!((g.start, g.duration), (23 * 60 + 50, 10));
    }
}
