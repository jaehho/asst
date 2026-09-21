//! The window's reading of the daemon's data: the views, their sections,
//! the sidebar counts, and the date and repeat arithmetic behind the
//! pickers. No GTK here, so it is tested like the core.

use std::collections::HashMap;

use asst_core::api::{ListView, StatusView, SyncState, TaskView};
use asst_core::fmt;
use asst_core::task::{Task, priority_level};
use asst_core::time::{Trigger, When, resolve, weekday_code};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

// -- views ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Nav {
    Inbox,
    Today,
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
    pub fn filters() -> [Nav; 8] {
        [
            Nav::Inbox,
            Nav::Today,
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
            Nav::Today => "tint-green",
            Nav::Scheduled | Nav::Tomorrow | Nav::Anytime | Nav::Repeating => "tint-purple",
            Nav::Completed => "tint-orange",
        }
    }

    /// A stable name for settings: `today`, `list:/cal/x/`.
    pub fn key(&self) -> String {
        match self {
            Nav::Inbox => "inbox".into(),
            Nav::Today => "today".into(),
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
    pub lists: HashMap<String, usize>,
}

impl Counts {
    pub fn new(data: &Data, now: DateTime<Tz>) -> Counts {
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
                Some(d) if d == today => c.today += 1,
                Some(d) => {
                    c.scheduled += 1;
                    if d == today + Duration::days(1) {
                        c.tomorrow += 1;
                    }
                }
            }
        }
        c
    }

    /// What a sidebar entry shows; nothing for Completed.
    pub fn of(&self, nav: &Nav) -> Option<usize> {
        match nav {
            Nav::Inbox => Some(self.inbox),
            Nav::Today => Some(self.today),
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

/// A due date as a row shows it, and the class that colors it.
pub fn due_chip(when: &When, now: DateTime<Tz>) -> (String, &'static str) {
    let date = when.local_date(now.timezone());
    let class = match date.cmp(&now.date_naive()) {
        std::cmp::Ordering::Less => "overdue",
        std::cmp::Ordering::Equal => "today",
        std::cmp::Ordering::Greater => "upcoming",
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
            "At due time".into()
        }
        Trigger::Absolute { at } => capitalize(&fmt::due_label(&When::Utc { at: *at }, now)),
        Trigger::Relative { offset, from_due } => {
            let what = if *from_due { "due time" } else { "start" };
            match offset.num_seconds() {
                0 => format!("At {what}"),
                s if s < 0 => format!("{} before", span(*offset)),
                _ => format!("{} after", span(*offset)),
            }
        }
    }
}

/// When a trigger goes off, for ordering and for "already past".
pub fn trigger_at(trigger: &Trigger, task: &Task, zone: Tz) -> Option<DateTime<Utc>> {
    match trigger {
        Trigger::Absolute { at } => Some(*at),
        Trigger::Relative { offset, from_due } => {
            let base = if *from_due {
                task.due.as_ref()
            } else {
                task.start.as_ref().or(task.due.as_ref())
            }?;
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
            task: Task {
                uid: uid.into(),
                summary: uid.into(),
                description: None,
                status: Status::NeedsAction,
                completed: None,
                priority: 0,
                due,
                start: None,
                rrule: None,
                alarms: Vec::new(),
                categories: Vec::new(),
                parent: None,
                url: None,
                source: None,
                linked_notes: Vec::new(),
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

        let c = Counts::new(&data, now());
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
        assert_eq!(reminder_label(&at, &t, now()), "At due time");
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
        assert_eq!(reminder_label(&absolute, &t, now()), "At due time");
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
}
