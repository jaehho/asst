//! The window's own preferences, `~/.config/asst/window.toml`. The daemon's
//! settings are in `config.toml` and go through D-Bus.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asst_core::api::ListView;
use serde::{Deserialize, Serialize};

use crate::model::{Nav, PlanSlot, ViewOpts};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Which one-time rewrites of this file have run (see `migrate`). 0
    /// for a file from before the key.
    pub schema: u32,
    /// `Nav::key` of the view shown last.
    pub view: String,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    pub show_counts: bool,
    /// Checking a task strikes it through first, and it goes a moment later.
    pub complete_later: bool,
    /// The week starts on Sunday instead of Monday.
    pub sunday_first: bool,
    /// A tray icon, and closing the window leaves asst in it.
    pub background: bool,
    /// Lists in the sidebar by name, instead of in the order dragged into.
    pub lists_by_name: bool,
    /// The order lists were dragged into, as hrefs. Lists not in it follow,
    /// in name order.
    pub list_order: Vec<String>,
    /// A list's icon (a Lucide name, see `ui::list_icon_names`), by href.
    /// Window-only: the server has nowhere to keep one.
    pub list_icons: BTreeMap<String, String>,
    /// Lists put away (hrefs): out of the sidebar, the views and the counts.
    /// They still sync, and their reminders still ring.
    pub archived: Vec<String>,
    /// Lists with nothing left to do are put away with the archived ones,
    /// and come back by themselves when a task lands in them (see
    /// `split_lists`).
    pub hide_finished_lists: bool,
    /// Quick add starts in the list it added to last.
    pub remember_list: bool,
    /// A new task's priority when none is given, 1 to 4 (none).
    pub default_priority: u8,
    /// A date or repeat typed in a new task's title becomes its own.
    pub read_dates: bool,
    /// Where checkouts of linked repos live: a folder holding one folder
    /// per repo, named for it. `~` is the home folder.
    pub projects_dir: String,
    /// What "Start work" on a GitHub issue runs, through `sh`: `{dir}` is
    /// the repo's checkout, `{number}` the issue, `{repo}` and `{url}`.
    pub work_command: String,
    /// The views in the sidebar, in order, as `Nav::key`s. Unset: the default.
    pub sidebar: Option<Vec<String>>,
    /// Before `sidebar`: views shown besides Inbox, Today, Scheduled and
    /// Completed, which always were. Read once, then written as `sidebar`.
    #[serde(skip_serializing)]
    filters: Vec<String>,
    /// The `sidebar` that `migrate` replaced on this load, for the window
    /// to offer back. Not saved.
    #[serde(skip)]
    pub migrated_from: Option<Vec<String>>,
    /// Day-plan slots, window-local: the iPhone has no planned slot, so
    /// nothing of this goes near the VTODO. Keyed by task UID + date, with
    /// the start (minutes from midnight) and the duration in minutes.
    /// Cleared when the task goes.
    pub plan: Vec<PlanSlot>,
    /// Day-grid zoom in pixels per hour. Clamped to a usable range on load
    /// and on change. Window-only, like the rest of this file.
    pub day_zoom: f64,
    pub views: BTreeMap<String, ViewOpts>,
}

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs {
            schema: 0,
            view: Nav::Inbox.key(),
            width: 1080,
            height: 720,
            maximized: false,
            show_counts: true,
            complete_later: true,
            sunday_first: false,
            background: true,
            lists_by_name: false,
            list_order: Vec::new(),
            list_icons: BTreeMap::new(),
            archived: Vec::new(),
            hide_finished_lists: false,
            remember_list: false,
            default_priority: 4,
            read_dates: true,
            projects_dir: "~/projects".into(),
            work_command: "kitty --directory {dir} gh-co {number}".into(),
            sidebar: None,
            filters: Vec::new(),
            migrated_from: None,
            plan: Vec::new(),
            day_zoom: crate::model::PLAN_PX_PER_HOUR,
            views: BTreeMap::new(),
        }
    }
}

/// Why a list is out of the sidebar's way, under Hidden lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Archived,
    /// Nothing left to do in it, and "Hide finished lists" is on.
    Finished,
}

impl Why {
    /// The words beside its row, in the sidebar and in Quick Find.
    pub fn caption(self) -> &'static str {
        match self {
            Why::Archived => "Archived",
            Why::Finished => "All done",
        }
    }
}

/// The lists as the sidebar has them: the rows, and what sits under Hidden
/// lists, each in the order the sidebar keeps them in.
#[derive(Debug, Default, PartialEq)]
pub struct SplitLists {
    pub shown: Vec<ListView>,
    pub hidden: Vec<(ListView, Why)>,
}

/// The views a new sidebar has. The rest are one menu away.
const DEFAULT_SIDEBAR: [Nav; 2] = [Nav::Inbox, Nav::Day];

/// What the sidebar showed before it was two views, and so what a sidebar
/// untouched since then still holds. Only the upgrade of an old file reads
/// it (`migrate`).
const LEGACY_STOCK: [Nav; 6] = [
    Nav::Inbox,
    Nav::Today,
    Nav::Day,
    Nav::Matrix,
    Nav::Scheduled,
    Nav::Completed,
];

/// The `schema` this build writes. 1: the sidebar starts with Inbox and Day.
const SCHEMA: u32 = 1;

fn path() -> PathBuf {
    asst_core::config::window_path()
}

/// Rewrite a file from before `schema`, once. The older upgrades of the
/// sidebar run here too, so they stand down for a sidebar chosen since:
/// exactly the old four, five or six views would otherwise grow back on
/// every load.
///
/// A sidebar showing all ten views or exactly the old six was never
/// arranged, so it becomes Inbox and Day, and `migrated_from` keeps the old
/// keys for the window to offer back. Anything else (a subset, an empty
/// `sidebar = []`) stays as the user left it.
fn migrate(p: &mut Prefs) {
    if p.schema >= SCHEMA {
        return;
    }
    upgrade_sidebar(p);
    let stock: Vec<String> = LEGACY_STOCK.iter().map(Nav::key).collect();
    let unarranged = p.sidebar.as_ref().is_some_and(|keys| {
        *keys == stock || Nav::filters().iter().all(|n| keys.contains(&n.key()))
    });
    if unarranged {
        p.migrated_from = p.sidebar.take();
        p.sidebar = Some(DEFAULT_SIDEBAR.iter().map(Nav::key).collect());
    }
    p.schema = SCHEMA;
}

/// The sidebar of an older file, brought up to the stock six it would have
/// had: each new view is put in once, without touching a sidebar the user
/// arranged.
fn upgrade_sidebar(p: &mut Prefs) {
    if p.sidebar.is_none() && !p.filters.is_empty() {
        let mut keys: Vec<String> = LEGACY_STOCK.iter().map(Nav::key).collect();
        keys.extend(std::mem::take(&mut p.filters));
        p.sidebar = Some(keys);
    }
    // The stock sidebar as it was before Matrix.
    const BEFORE_MATRIX: [&str; 4] = ["inbox", "today", "scheduled", "completed"];
    if let Some(keys) = &p.sidebar
        && keys.iter().map(String::as_str).eq(BEFORE_MATRIX)
    {
        p.sidebar = Some(LEGACY_STOCK.iter().map(Nav::key).collect());
    }
    // Same for Time after the stock five: insert it before Completed.
    if let Some(keys) = &mut p.sidebar {
        let has_time = keys.iter().any(|k| k == "time");
        let stock: Vec<&str> = keys.iter().map(String::as_str).collect();
        const BEFORE_TIME: [&str; 5] = ["inbox", "today", "matrix", "scheduled", "completed"];
        if !has_time && stock == BEFORE_TIME {
            *keys = LEGACY_STOCK.iter().map(Nav::key).collect();
        }
    }
    // Same for Plan after the stock six: insert it after Today.
    if let Some(keys) = &mut p.sidebar {
        let has_plan = keys.iter().any(|k| k == "plan");
        let stock: Vec<&str> = keys.iter().map(String::as_str).collect();
        const BEFORE_PLAN: [&str; 6] =
            ["inbox", "today", "matrix", "scheduled", "time", "completed"];
        if !has_plan && stock == BEFORE_PLAN {
            *keys = LEGACY_STOCK.iter().map(Nav::key).collect();
        }
    }
    // Time and Plan are one Day view now: keep one `day` where either
    // was, so a sidebar the user arranged does not grow a second tile.
    if let Some(keys) = &mut p.sidebar {
        let first = keys
            .iter()
            .position(|k| k == "time" || k == "plan" || k == "day");
        if let Some(at) = first {
            keys.retain(|k| k != "time" && k != "plan" && k != "day");
            keys.insert(at, "day".into());
        }
    }
}

impl Prefs {
    pub fn load() -> Prefs {
        Prefs::load_from(&path())
    }

    /// A file's worth of defaults, as a new install writes them: already at
    /// the current `schema`, so no migration looks at it later.
    fn fresh() -> Prefs {
        Prefs::parse("").unwrap_or_default()
    }

    /// The prefs in `file`; the defaults when there is no file. A file that
    /// cannot be read or parsed is moved to `<file>.bad` first and said so on
    /// stderr. The window writes the file whole, so its next save would
    /// otherwise replace everything the file held, not only the bad key, with
    /// the defaults.
    fn load_from(file: &Path) -> Prefs {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Prefs::fresh(),
            Err(e) => return Prefs::set_aside(file, &e.to_string()),
        };
        Prefs::parse(&text).unwrap_or_else(|e| {
            // `Display` draws the offending line; the log wants one line.
            let line = e
                .span()
                .and_then(|at| text.get(..at.start))
                .map_or(String::new(), |before| {
                    format!("line {}: ", before.matches('\n').count() + 1)
                });
            Prefs::set_aside(file, &format!("{line}{}", e.message()))
        })
    }

    /// Keep an unusable `file` as `<file>.bad` (replacing an older one) and
    /// start from the defaults.
    fn set_aside(file: &Path, why: &str) -> Prefs {
        let bad = file.with_extension("toml.bad");
        match std::fs::rename(file, &bad) {
            Ok(()) => eprintln!(
                "asst-gtk: {}: {why}; kept as {}, starting from defaults",
                file.display(),
                bad.display()
            ),
            Err(e) => eprintln!(
                "asst-gtk: {}: {why}; could not move it to {}: {e}; starting from defaults",
                file.display(),
                bad.display()
            ),
        }
        Prefs::fresh()
    }

    fn parse(text: &str) -> Result<Prefs, toml::de::Error> {
        let mut p: Prefs = toml::from_str(text)?;
        p.day_zoom = crate::model::clamp_px_per_hour(p.day_zoom);
        migrate(&mut p);
        Ok(p)
    }

    /// Keep the file as it stands as `<file>.v0` before the first rewrite of
    /// it by a build that migrates, unless a copy is there already: the
    /// oldest copy is the one worth having.
    pub fn back_up_v0() {
        Prefs::back_up_v0_of(&path());
    }

    fn back_up_v0_of(file: &Path) {
        let v0 = file.with_extension("toml.v0");
        if v0.exists() {
            return;
        }
        if let Err(e) = std::fs::copy(file, &v0) {
            eprintln!(
                "asst-gtk: {}: could not keep a copy as {}: {e}",
                file.display(),
                v0.display()
            );
        }
    }

    pub fn save(&self) {
        let p = path();
        let Ok(text) = toml::to_string(self) else {
            return;
        };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("toml.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    pub fn view_opts(&self, nav: &Nav) -> ViewOpts {
        self.views.get(&nav.key()).cloned().unwrap_or_default()
    }

    /// The views in the sidebar, top to bottom. A view named twice counts
    /// once, at its first place.
    pub fn sidebar_views(&self) -> Vec<Nav> {
        let mut views: Vec<Nav> = match &self.sidebar {
            Some(keys) => keys
                .iter()
                .filter_map(|k| Nav::from_key(k).filter(|n| !matches!(n, Nav::List(_))))
                .collect(),
            None => DEFAULT_SIDEBAR.to_vec(),
        };
        let mut seen = Vec::new();
        views.retain(|n| {
            let new = !seen.contains(n);
            if new {
                seen.push(n.clone());
            }
            new
        });
        views
    }

    /// The views that are not in the sidebar, in `Nav::filters` order.
    pub fn hidden_views(&self) -> Vec<Nav> {
        let shown = self.sidebar_views();
        Nav::filters()
            .into_iter()
            .filter(|n| !shown.contains(n))
            .collect()
    }

    /// Where the window starts when the view last shown is unknown, and
    /// where it goes when the open list is deleted: the first tile, else the
    /// Inbox, which is always there (Ctrl+I) even with its tile hidden.
    pub fn home(&self) -> Nav {
        self.sidebar_views()
            .into_iter()
            .next()
            .unwrap_or(Nav::Inbox)
    }

    pub fn set_sidebar_views(&mut self, views: &[Nav]) {
        self.sidebar = Some(views.iter().map(Nav::key).collect());
    }

    /// Take a view out of the sidebar. The place it had, for `show_view` to
    /// put it back at; `None` (and no change) when it was not shown.
    pub fn hide_view(&mut self, nav: &Nav) -> Option<usize> {
        let mut views = self.sidebar_views();
        let at = views.iter().position(|n| n == nav)?;
        views.remove(at);
        self.set_sidebar_views(&views);
        Some(at)
    }

    /// Put a view in the sidebar at `at`, else last. A place past the end is
    /// the end, and a view already shown stays where it is.
    pub fn show_view(&mut self, nav: Nav, at: Option<usize>) {
        let mut views = self.sidebar_views();
        if views.contains(&nav) {
            return;
        }
        views.insert(at.map_or(views.len(), |at| at.min(views.len())), nav);
        self.set_sidebar_views(&views);
    }

    /// Move a shown view `delta` places up (negative) or down, stopping at
    /// the ends.
    pub fn move_view(&mut self, nav: &Nav, delta: i32) {
        let mut views = self.sidebar_views();
        let Some(from) = views.iter().position(|n| n == nav) else {
            return;
        };
        let to = from
            .saturating_add_signed(delta as isize)
            .min(views.len() - 1);
        if to != from {
            let moved = views.remove(from);
            views.insert(to, moved);
            self.set_sidebar_views(&views);
        }
    }

    /// Back to the default sidebar. What `sidebar` held, as saved, for the
    /// Undo to write back.
    pub fn reset_sidebar(&mut self) -> Option<Vec<String>> {
        self.sidebar.take()
    }

    /// Whether the list is out of sight. The inbox list never is: it may
    /// have been archived before it became the inbox (`asst settings
    /// --inbox`), and the entry is left alone, so the list is hidden again
    /// should it stop being the inbox.
    pub fn is_archived(&self, href: &str, lists: &[ListView]) -> bool {
        self.archived.iter().any(|a| a == href)
            && !crate::model::inbox(lists).is_some_and(|l| l.href == href)
    }

    /// Archive `href`, or bring it back. The inbox list is where new tasks
    /// go, so it cannot be archived: `false`, and nothing changed.
    pub fn set_archived(&mut self, href: &str, on: bool, lists: &[ListView]) -> bool {
        if on && crate::model::inbox(lists).is_some_and(|l| l.href == href) {
            return false;
        }
        self.archived.retain(|h| h != href);
        if on {
            self.archived.push(href.to_string());
        }
        true
    }

    /// Forget what is kept for a list that was deleted: its place in the
    /// dragged order, its icon, its archive entry. `prefs.save` on `true`,
    /// since the window writes this file whole. Only for a delete that
    /// happened: a list missing from one response may be back in the next.
    pub fn forget_list(&mut self, href: &str) -> bool {
        let before = (
            self.list_order.len(),
            self.list_icons.len(),
            self.archived.len(),
        );
        self.list_order.retain(|h| h != href);
        self.list_icons.remove(href);
        self.archived.retain(|h| h != href);
        before
            != (
                self.list_order.len(),
                self.list_icons.len(),
                self.archived.len(),
            )
    }

    /// Drop day-plan slots whose task is no longer open. `prefs.save` on
    /// `true`, since the window writes this file whole.
    pub fn prune_plan(&mut self, live: &std::collections::HashSet<&str>) -> bool {
        crate::model::prune_slots(&mut self.plan, live)
    }

    /// Lists as the sidebar shows them: archived ones out, in the order
    /// dragged into or by name.
    pub fn arrange(&self, lists: &[ListView]) -> Vec<ListView> {
        let mut out = self.ordered(lists);
        out.retain(|l| !self.is_archived(&l.href, lists));
        out
    }

    /// Every list, archived ones too, in the order dragged into or by name.
    fn ordered(&self, lists: &[ListView]) -> Vec<ListView> {
        let mut out: Vec<ListView> = lists.to_vec();
        if self.lists_by_name {
            out.sort_by(|a, b| crate::model::natural_cmp(&a.name, &b.name));
        } else {
            let place = |l: &ListView| {
                self.list_order
                    .iter()
                    .position(|h| *h == l.href)
                    .unwrap_or(usize::MAX)
            };
            // Lists never dragged follow by name, not in the order the
            // server happens to answer in.
            out.sort_by(|a, b| {
                place(a)
                    .cmp(&place(b))
                    .then_with(|| crate::model::natural_cmp(&a.name, &b.name))
            });
        }
        out
    }

    /// The inbox list's href while the Inbox tile is in the sidebar: the
    /// tile is that list, so it has no row of its own then.
    pub fn inbox_on_tile<'a>(&self, lists: &'a [ListView]) -> Option<&'a str> {
        let tile = self.sidebar_views().contains(&Nav::Inbox);
        crate::model::inbox(lists)
            .filter(|_| tile)
            .map(|l| l.href.as_str())
    }

    /// The lists that have a row in the sidebar: `arrange`, less the inbox
    /// list while the Inbox tile shows it (hiding the tile brings the row),
    /// and less the finished ones when those are hidden. `current` is the
    /// view on screen.
    pub fn sidebar_lists(&self, lists: &[ListView], current: &Nav) -> Vec<ListView> {
        self.split_lists(lists, current).shown
    }

    /// Whether the list has nothing left to do: tasks once, none open now.
    /// A list with no tasks at all is new, not finished. Never the inbox
    /// list or an archived one.
    fn is_finished(&self, l: &ListView, all: &[ListView]) -> bool {
        l.open == 0
            && l.done > 0
            && !self.is_archived(&l.href, all)
            && !crate::model::inbox(all).is_some_and(|i| i.href == l.href)
    }

    /// How many lists are finished right now, whether or not they are put
    /// away: what the toggle offers to hide. The one on screen counts, as
    /// it is not kept out of `split_lists` for being any less finished.
    pub fn finished_count(&self, all: &[ListView]) -> usize {
        all.iter().filter(|l| self.is_finished(l, all)).count()
    }

    /// The sidebar's lists, in its order, split into the rows and what goes
    /// under Hidden lists: the archived ones, and the finished ones while
    /// `hide_finished_lists` is on, but for the one on screen (completing
    /// its last task would take the row from under the cursor). A finished
    /// list returns by itself when a task lands in it, a new GitHub issue
    /// too. The inbox list has no row while its tile shows, and is never
    /// hidden.
    pub fn split_lists(&self, all: &[ListView], current: &Nav) -> SplitLists {
        let on_tile = self.inbox_on_tile(all);
        let mut split = SplitLists::default();
        for l in self.ordered(all) {
            if Some(l.href.as_str()) == on_tile {
                continue;
            }
            let why = if self.is_archived(&l.href, all) {
                Some(Why::Archived)
            } else {
                (self.hide_finished_lists
                    && self.is_finished(&l, all)
                    && *current != Nav::List(l.href.clone()))
                .then_some(Why::Finished)
            };
            match why {
                Some(why) => split.hidden.push((l, why)),
                None => split.shown.push(l),
            }
        }
        split
    }

    /// Dragging made `shown_before` into `shown_after` (hrefs, top to
    /// bottom). `list_order` also holds lists that are not on screen
    /// (archived ones, a list that a response left out) and so must not be
    /// replaced by what is: each of those keeps its place, and the lists
    /// shown trade the places they held among themselves. Nothing is
    /// pruned, since a list missing from one response may be back in the
    /// next; `forget_list` is for a delete that happened.
    ///
    /// `list_order` becomes every list there is, so that the places exist to
    /// be kept: the ones it held, then the rest in name order, which is the
    /// order they were shown in. A drag writes the lists by order, so
    /// `lists_by_name` is off.
    pub fn reorder_lists(
        &mut self,
        all: &[ListView],
        shown_before: &[String],
        shown_after: &[String],
    ) {
        let mut rest: Vec<&ListView> = all.iter().collect();
        rest.sort_by(|a, b| crate::model::natural_cmp(&a.name, &b.name));
        let mut full: Vec<String> = Vec::new();
        for href in self
            .list_order
            .iter()
            .cloned()
            .chain(rest.into_iter().map(|l| l.href.clone()))
        {
            if !full.contains(&href) {
                full.push(href);
            }
        }
        // A response that came in during the drag may have added a list or
        // taken one away or archived it. What it did not know is not
        // moved: the new order is cut down to lists that are on screen now
        // and hold a place, and every other place stays.
        let mut moved: Vec<String> = Vec::new();
        for href in shown_after {
            if shown_before.contains(href) && full.contains(href) && !moved.contains(href) {
                moved.push(href.clone());
            }
        }
        let mut next = moved.clone().into_iter();
        for place in &mut full {
            if moved.contains(place)
                && let Some(href) = next.next()
            {
                *place = href;
            }
        }
        self.list_order = full;
        self.lists_by_name = false;
    }

    /// Sort A-Z: the lists in `shown` by name, as people read it (case
    /// aside, "Work 9" before "Work 10"). The places of lists out of sight
    /// stay, as in a drag, and the order is the sidebar's own from now on.
    pub fn sort_lists_by_name(&mut self, all: &[ListView], shown: &[ListView]) {
        let before: Vec<String> = shown.iter().map(|l| l.href.clone()).collect();
        let mut sorted: Vec<&ListView> = shown.iter().collect();
        sorted.sort_by(|a, b| crate::model::natural_cmp(&a.name, &b.name));
        let after: Vec<String> = sorted.into_iter().map(|l| l.href.clone()).collect();
        self.reorder_lists(all, &before, &after);
    }
}

/// One change from the Preferences dialog, applied to the window's current
/// `Prefs`. The dialog's copy dates from when it opened, and sending it back
/// whole would undo what the window changed since (the day zoom, a view's
/// sort). An enum rather than a closure because `Msg` is `Debug`.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefPatch {
    ReadDates(bool),
    DefaultPriority(u8),
    RememberList(bool),
    CompleteLater(bool),
    SundayFirst(bool),
    Background(bool),
    ShowCounts(bool),
    HideFinishedLists(bool),
    ProjectsDir(String),
    WorkCommand(String),
}

impl PrefPatch {
    pub fn apply(&self, p: &mut Prefs) {
        match self {
            PrefPatch::ReadDates(on) => p.read_dates = *on,
            PrefPatch::DefaultPriority(n) => p.default_priority = *n,
            PrefPatch::RememberList(on) => p.remember_list = *on,
            PrefPatch::CompleteLater(on) => p.complete_later = *on,
            PrefPatch::SundayFirst(on) => p.sunday_first = *on,
            PrefPatch::Background(on) => p.background = *on,
            PrefPatch::ShowCounts(on) => p.show_counts = *on,
            PrefPatch::HideFinishedLists(on) => p.hide_finished_lists = *on,
            PrefPatch::ProjectsDir(dir) => p.projects_dir.clone_from(dir),
            PrefPatch::WorkCommand(command) => p.work_command.clone_from(command),
        }
    }

    /// Whether the open view must be rebuilt. The two strings are only read
    /// when Start work runs.
    pub fn redraws(&self) -> bool {
        !matches!(self, PrefPatch::ProjectsDir(_) | PrefPatch::WorkCommand(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Prefs {
        Prefs::parse(text).unwrap()
    }

    fn keys(views: &[Nav]) -> Vec<String> {
        views.iter().map(Nav::key).collect()
    }

    #[test]
    fn a_fresh_file_has_inbox_and_day() {
        let p = parse("");
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Day]);
        assert_eq!(p.home(), Nav::Inbox);
        assert_eq!(p.view, "inbox");
        // Already current, so nothing rewrites it later, and nothing to offer
        // back.
        assert_eq!(p.schema, SCHEMA);
        assert_eq!(p.migrated_from, None);
        // What a new install saves, read again.
        let again = parse(&toml::to_string(&p).unwrap());
        assert_eq!(again, p);
    }

    #[test]
    fn sidebar_starts_with_the_defaults_and_keeps_old_extras() {
        assert_eq!(parse("").sidebar_views(), DEFAULT_SIDEBAR);
        // Extras from before `sidebar` was a key come after the old six, and
        // a sidebar that is the six and more was arranged.
        let old = parse("filters = [\"tomorrow\"]\n");
        assert_eq!(
            old.sidebar_views(),
            [
                Nav::Inbox,
                Nav::Today,
                Nav::Day,
                Nav::Matrix,
                Nav::Scheduled,
                Nav::Completed,
                Nav::Tomorrow
            ]
        );
        assert_eq!(old.migrated_from, None);
        // A sidebar arranged before Matrix stays as it was.
        let custom = parse("sidebar = [\"today\", \"inbox\"]\n");
        assert_eq!(custom.sidebar_views(), [Nav::Today, Nav::Inbox]);
        // The stock four, five and six are all the old stock, which gained
        // Matrix and Day on the way: the shims bring each to the six, and
        // `migrate` then takes the six to the new default.
        let stock = parse("sidebar = [\"inbox\", \"today\", \"scheduled\", \"completed\"]\n");
        assert_eq!(stock.migrated_from, Some(keys(&LEGACY_STOCK)));
        assert_eq!(stock.sidebar_views(), DEFAULT_SIDEBAR);
        let with_matrix =
            parse("sidebar = [\"inbox\", \"today\", \"matrix\", \"scheduled\", \"completed\"]\n");
        assert_eq!(with_matrix.migrated_from, Some(keys(&LEGACY_STOCK)));
        assert_eq!(with_matrix.sidebar_views(), DEFAULT_SIDEBAR);
        // Time and Plan become one Day tile at the first of the two.
        let merged = parse(
            "sidebar = [\"inbox\", \"today\", \"time\", \"plan\", \"scheduled\", \"completed\"]\n",
        );
        assert_eq!(
            merged.sidebar_views(),
            [
                Nav::Inbox,
                Nav::Today,
                Nav::Day,
                Nav::Scheduled,
                Nav::Completed
            ]
        );
        let text = toml::to_string(&old).unwrap();
        assert!(!text.contains("filters"));
        assert_eq!(parse(&text).sidebar_views(), old.sidebar_views());

        let mut none = Prefs::default();
        none.set_sidebar_views(&[]);
        let text = toml::to_string(&none).unwrap();
        assert!(parse(&text).sidebar_views().is_empty());
    }

    /// `~/.config/asst/window.toml` as it was when the sidebar went to two
    /// views, copied as it is: stale `/tasks/` in `list_order`, an archived
    /// list, all ten views in an order of their own.
    const THE_USERS_FILE: &str = r#"view = "list:/remote.php/dav/calendars/jaeho/champalimaud-21eefd/"
width = 1080
height = 720
maximized = true
show_counts = true
complete_later = true
sunday_first = false
background = true
lists_by_name = false
list_order = ["/remote.php/dav/calendars/jaeho/5f55a575-75e6-4d30-89b8-280d684e437c/", "/remote.php/dav/calendars/jaeho/tasks/"]
archived = ["/remote.php/dav/calendars/jaeho/mail-digest-d1e268/"]
remember_list = false
default_priority = 4
read_dates = true
projects_dir = "~/projects"
work_command = "kitty --directory {dir} gh-co {number}"
sidebar = ["inbox", "today", "scheduled", "all", "matrix", "day", "completed", "tomorrow", "anytime", "repeating"]
plan = []
day_zoom = 52.0

[list_icons]

[views]
"#;

    #[test]
    fn the_users_ten_view_sidebar_becomes_inbox_and_day_once() {
        // The file as serde reads it, before anything is rewritten.
        let raw: Prefs = toml::from_str(THE_USERS_FILE).unwrap();
        assert_eq!(raw.schema, 0);
        assert_eq!(raw.sidebar_views().len(), 10);

        let p = parse(THE_USERS_FILE);
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Day]);
        assert_eq!(p.schema, 1);
        assert_eq!(p.migrated_from, raw.sidebar);
        // Nothing else moved: the archive, the order with its stale entry,
        // the counts, the open list, the lot.
        assert_eq!(
            p,
            Prefs {
                schema: 1,
                sidebar: Some(keys(&DEFAULT_SIDEBAR)),
                migrated_from: raw.sidebar.clone(),
                ..raw.clone()
            }
        );
        assert_eq!(
            p.archived,
            ["/remote.php/dav/calendars/jaeho/mail-digest-d1e268/"]
        );
        assert_eq!(p.list_order.len(), 2);
        assert!(p.list_order[1].ends_with("/tasks/"));

        // What the window saves straight after, read again: stable, with
        // nothing left to migrate or offer back.
        let saved = toml::to_string(&p).unwrap();
        assert!(saved.contains("schema = 1"));
        assert!(!saved.contains("migrated_from"));
        let again = parse(&saved);
        assert_eq!(again.migrated_from, None);
        assert_eq!(
            again,
            Prefs {
                migrated_from: None,
                ..p.clone()
            }
        );

        // All ten views back on purpose is the user's own, kept for good.
        let mut chosen = again;
        chosen.set_sidebar_views(&Nav::filters());
        let reread = parse(&toml::to_string(&chosen).unwrap());
        assert_eq!(reread.sidebar_views(), Nav::filters());
        assert_eq!(reread.migrated_from, None);
    }

    #[test]
    fn the_old_stock_six_becomes_minimal_a_curated_sidebar_does_not() {
        let stock = keys(&LEGACY_STOCK);
        let six = parse(&format!("sidebar = {stock:?}\n"));
        assert_eq!(six.sidebar_views(), DEFAULT_SIDEBAR);
        assert_eq!(six.migrated_from, Some(stock));

        // Arranged: left as it is, nothing to offer back.
        for (text, shown) in [
            (
                "sidebar = [\"today\", \"inbox\"]\n",
                vec![Nav::Today, Nav::Inbox],
            ),
            ("sidebar = []\n", vec![]),
            (
                "sidebar = [\"inbox\", \"day\"]\n",
                vec![Nav::Inbox, Nav::Day],
            ),
            ("sidebar = [\"matrix\"]\n", vec![Nav::Matrix]),
        ] {
            let p = parse(text);
            assert_eq!(p.sidebar_views(), shown, "{text}");
            assert_eq!(p.migrated_from, None, "{text}");
            assert_eq!(p.schema, 1, "{text}");
        }

        // After the migration the upgrade shims stand down: a sidebar of
        // exactly the old four is somebody's choice now, not a stock one that
        // lacks Matrix and Day.
        let four =
            parse("schema = 1\nsidebar = [\"inbox\", \"today\", \"scheduled\", \"completed\"]\n");
        assert_eq!(
            four.sidebar_views(),
            [Nav::Inbox, Nav::Today, Nav::Scheduled, Nav::Completed]
        );
        assert_eq!(four.migrated_from, None);
    }

    #[test]
    fn home_is_the_first_tile_else_the_inbox() {
        assert_eq!(Prefs::default().home(), Nav::Inbox);
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::Day, Nav::Inbox]);
        assert_eq!(p.home(), Nav::Day);
        p.set_sidebar_views(&[]);
        assert_eq!(p.home(), Nav::Inbox);
    }

    #[test]
    fn hide_returns_the_index_and_show_puts_it_back() {
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::Inbox, Nav::Today, Nav::Day]);
        assert_eq!(p.hide_view(&Nav::Today), Some(1));
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Day]);

        // Not shown: nothing to hide, nothing changed.
        let before = p.clone();
        assert_eq!(p.hide_view(&Nav::Today), None);
        assert_eq!(p, before);

        p.show_view(Nav::Today, Some(1));
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Today, Nav::Day]);
        // No place given, or one past the end: last.
        p.show_view(Nav::Matrix, None);
        p.show_view(Nav::Tomorrow, Some(99));
        assert_eq!(
            p.sidebar_views(),
            [Nav::Inbox, Nav::Today, Nav::Day, Nav::Matrix, Nav::Tomorrow]
        );

        // The default sidebar has no key yet: the first hide writes it, and
        // the last tile may go too.
        let mut d = Prefs::default();
        assert_eq!(d.sidebar, None);
        assert_eq!(d.hide_view(&Nav::Day), Some(1));
        assert_eq!(d.sidebar, Some(keys(&[Nav::Inbox])));
        assert_eq!(d.hide_view(&Nav::Inbox), Some(0));
        assert!(d.sidebar_views().is_empty());
        assert_eq!(d.home(), Nav::Inbox);
        // What was saved is what comes back.
        assert!(
            parse(&toml::to_string(&d).unwrap())
                .sidebar_views()
                .is_empty()
        );
    }

    #[test]
    fn show_is_a_noop_for_a_present_view() {
        let mut p = Prefs::default();
        let before = p.clone();
        p.show_view(Nav::Inbox, Some(1));
        p.show_view(Nav::Day, None);
        // Not even the key is written.
        assert_eq!(p, before);

        p.set_sidebar_views(&[Nav::Today, Nav::Inbox]);
        let before = p.clone();
        p.show_view(Nav::Inbox, Some(0));
        assert_eq!(p, before);
    }

    #[test]
    fn move_view_clamps_at_the_ends() {
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::Inbox, Nav::Today, Nav::Day]);
        p.move_view(&Nav::Today, 1);
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Day, Nav::Today]);
        p.move_view(&Nav::Today, 1);
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Day, Nav::Today]);
        p.move_view(&Nav::Today, -1);
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Today, Nav::Day]);
        // Further than there is room for stops at the end.
        p.move_view(&Nav::Day, -5);
        assert_eq!(p.sidebar_views(), [Nav::Day, Nav::Inbox, Nav::Today]);
        p.move_view(&Nav::Day, i32::MAX);
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Today, Nav::Day]);
        p.move_view(&Nav::Inbox, i32::MIN);
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Today, Nav::Day]);

        // A view that is not shown stays where it is, and a move that goes
        // nowhere does not write the default sidebar down.
        let before = p.clone();
        p.move_view(&Nav::Matrix, 1);
        assert_eq!(p, before);
        let mut d = Prefs::default();
        d.move_view(&Nav::Inbox, -1);
        d.move_view(&Nav::Day, 1);
        assert_eq!(d, Prefs::default());
    }

    #[test]
    fn reset_returns_the_old_raw_value_and_restore_roundtrips() {
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::Today, Nav::Inbox, Nav::Matrix]);
        let old = p.reset_sidebar();
        assert_eq!(old, Some(keys(&[Nav::Today, Nav::Inbox, Nav::Matrix])));
        assert_eq!(p.sidebar, None);
        assert_eq!(p.sidebar_views(), DEFAULT_SIDEBAR);
        // What the window saves after a reset is the default, and stays it.
        let again = parse(&toml::to_string(&p).unwrap());
        assert_eq!(again.sidebar_views(), DEFAULT_SIDEBAR);
        assert_eq!(again.migrated_from, None);
        // The Undo writes the raw value back, whatever it was.
        p.sidebar = old;
        assert_eq!(p.sidebar_views(), [Nav::Today, Nav::Inbox, Nav::Matrix]);

        // The default has nothing saved, and Undo puts back nothing saved.
        let mut d = Prefs::default();
        let old = d.reset_sidebar();
        assert_eq!(old, None);
        d.sidebar = old;
        assert_eq!(d, Prefs::default());

        // All ten, put there on purpose, come back as ten and are not
        // taken for a sidebar nobody arranged.
        let mut all = parse("schema = 1\n");
        all.set_sidebar_views(&Nav::filters());
        let old = all.reset_sidebar();
        all.sidebar = old;
        let reread = parse(&toml::to_string(&all).unwrap());
        assert_eq!(reread.sidebar_views(), Nav::filters());
        assert_eq!(reread.migrated_from, None);
    }

    #[test]
    fn each_hide_undo_restores_only_its_own_view() {
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::Inbox, Nav::Today, Nav::Day, Nav::Matrix]);
        // The toast carries the place the view had when it was hidden.
        let a = p.hide_view(&Nav::Today).unwrap();
        let b = p.hide_view(&Nav::Matrix).unwrap();
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Day]);

        // Undo of the first: B stays hidden.
        p.show_view(Nav::Today, Some(a));
        assert_eq!(p.sidebar_views(), [Nav::Inbox, Nav::Today, Nav::Day]);
        // Undo of the second, the other way round: A stays hidden.
        let mut q = Prefs::default();
        q.set_sidebar_views(&[Nav::Inbox, Nav::Day]);
        q.show_view(Nav::Matrix, Some(b));
        assert_eq!(q.sidebar_views(), [Nav::Inbox, Nav::Day, Nav::Matrix]);
    }

    #[test]
    fn hidden_views_follow_filters_order() {
        assert_eq!(
            Prefs::default().hidden_views(),
            [
                Nav::Today,
                Nav::Matrix,
                Nav::Scheduled,
                Nav::Completed,
                Nav::Tomorrow,
                Nav::Anytime,
                Nav::Repeating,
                Nav::All
            ]
        );
        // The order in the sidebar does not matter, only what is in it.
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::All, Nav::Day, Nav::Today, Nav::Inbox]);
        assert_eq!(
            p.hidden_views(),
            [
                Nav::Matrix,
                Nav::Scheduled,
                Nav::Completed,
                Nav::Tomorrow,
                Nav::Anytime,
                Nav::Repeating
            ]
        );
        p.set_sidebar_views(&Nav::filters());
        assert!(p.hidden_views().is_empty());
        p.set_sidebar_views(&[]);
        assert_eq!(p.hidden_views(), Nav::filters());
    }

    #[test]
    fn a_view_named_twice_is_shown_once() {
        let p = parse("schema = 1\nsidebar = [\"day\", \"inbox\", \"day\"]\n");
        assert_eq!(p.sidebar_views(), [Nav::Day, Nav::Inbox]);
        assert_eq!(p.hidden_views().len(), 8);
    }

    #[test]
    fn a_file_that_does_not_parse_is_kept_not_overwritten() {
        // One key of the wrong type: the whole file is refused.
        let bad = "width = \"wide\"\nview = \"matrix\"\nlist_order = [\"/b/\"]\n";
        assert!(Prefs::parse(bad).is_err());

        let dir = std::env::temp_dir().join(format!("asst-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("window.toml");
        let kept = file.with_extension("toml.bad");

        // No file: the defaults, as a new install has them, and nothing made.
        assert_eq!(Prefs::load_from(&file), Prefs::fresh());
        assert_eq!(Prefs::load_from(&file).schema, SCHEMA);
        assert!(!kept.exists());

        // A good file loads as it is.
        std::fs::write(&file, "view = \"matrix\"\n").unwrap();
        assert_eq!(Prefs::load_from(&file).view, "matrix");
        assert!(!kept.exists());

        // A bad one is moved, whole, before the defaults take its place.
        std::fs::write(&file, bad).unwrap();
        assert_eq!(Prefs::load_from(&file), Prefs::fresh());
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), bad);

        // A second bad file replaces the first copy rather than failing.
        std::fs::write(&file, "height = true\n").unwrap();
        assert_eq!(Prefs::load_from(&file), Prefs::fresh());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "height = true\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_v0_copy_is_the_oldest_one() {
        let dir = std::env::temp_dir().join(format!("asst-prefs-v0-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("window.toml");
        let v0 = file.with_extension("toml.v0");

        std::fs::write(&file, "width = 1\n").unwrap();
        Prefs::back_up_v0_of(&file);
        assert_eq!(std::fs::read_to_string(&v0).unwrap(), "width = 1\n");

        // A later migration (a restored file, a second machine's copy) must
        // not replace the file as it was before any of them.
        std::fs::write(&file, "width = 2\n").unwrap();
        Prefs::back_up_v0_of(&file);
        assert_eq!(std::fs::read_to_string(&v0).unwrap(), "width = 1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_slots_round_trip_and_prune() {
        use crate::model::place;
        use chrono::NaiveDate;
        let mut p = Prefs::default();
        let day = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        place(&mut p.plan, "abc", day, 9 * 60, 30);
        place(&mut p.plan, "def", day, 10 * 60, 60);
        let text = toml::to_string(&p).unwrap();
        let back = parse(&text);
        assert_eq!(back.plan, p.plan);
        assert!(text.contains("uid = \"abc\""));
        let live: std::collections::HashSet<&str> = ["abc"].into_iter().collect();
        assert!(p.prune_plan(&live));
        assert_eq!(p.plan.len(), 1);
        assert!(!p.prune_plan(&live));
    }

    #[test]
    fn the_archive_is_written_where_asst_projects_reads_it() {
        // `asst projects` reads the key by name; the window writes the file.
        let p = Prefs {
            archived: vec!["/cal/mail-digest-d1e268/".into(), "/cal/old/".into()],
            ..Prefs::default()
        };
        let text = toml::to_string(&p).unwrap();
        assert_eq!(
            asst_core::config::archived_lists(&text),
            ["/cal/mail-digest-d1e268/", "/cal/old/"]
        );
        let none = toml::to_string(&Prefs::default()).unwrap();
        assert!(asst_core::config::archived_lists(&none).is_empty());
    }

    #[test]
    fn a_deleted_list_is_forgotten_and_the_others_kept() {
        let mut p = Prefs {
            list_order: vec!["/a/".into(), "/gone/".into(), "/b/".into()],
            list_icons: [("/gone/", "flask"), ("/b/", "home")]
                .map(|(h, i)| (h.to_string(), i.to_string()))
                .into(),
            archived: vec!["/gone/".into(), "/a/".into()],
            ..Prefs::default()
        };
        assert!(p.forget_list("/gone/"));
        assert_eq!(p.list_order, ["/a/", "/b/"]);
        assert_eq!(p.list_icons.len(), 1);
        assert_eq!(p.list_icons.get("/b/").map(String::as_str), Some("home"));
        assert_eq!(p.archived, ["/a/"]);
        // Nothing left to forget, nothing to save; an unknown list too
        assert!(!p.forget_list("/gone/"));
        assert!(!p.forget_list("/never/"));
        // An entry in only one of the three is still found
        p.archived.push("/c/".into());
        assert!(p.forget_list("/c/"));
        assert_eq!(p.archived, ["/a/"]);
    }

    fn list(href: &str, name: &str) -> ListView {
        ListView {
            href: href.into(),
            name: name.into(),
            color: None,
            writable: true,
            inbox: false,
            open: 0,
            done: 0,
        }
    }

    fn names(lists: Vec<ListView>) -> Vec<String> {
        lists.into_iter().map(|l| l.name).collect()
    }

    fn hrefs(lists: &[ListView]) -> Vec<String> {
        lists.iter().map(|l| l.href.clone()).collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn lists_in_dragged_order_or_by_name() {
        let lists = [
            list("/a/", "Work 10"),
            list("/b/", "Home"),
            list("/c/", "Work 9"),
            list("/d/", "Old"),
        ];
        let mut p = Prefs {
            archived: vec!["/d/".into()],
            ..Prefs::default()
        };
        // Never dragged: by name, "Work 9" before "Work 10".
        assert_eq!(names(p.arrange(&lists)), ["Home", "Work 9", "Work 10"]);
        p.list_order = vec!["/c/".into(), "/a/".into()];
        assert_eq!(names(p.arrange(&lists)), ["Work 9", "Work 10", "Home"]);
        p.lists_by_name = true;
        assert_eq!(names(p.arrange(&lists)), ["Home", "Work 9", "Work 10"]);
    }

    #[test]
    fn inbox_shows_once_and_returns_when_its_tile_is_hidden() {
        let lists = [
            list("/w/", "Work"),
            ListView {
                inbox: true,
                ..list("/i/", "Inbox")
            },
            list("/h/", "Home"),
        ];
        let mut p = Prefs::default();
        assert!(p.sidebar_views().contains(&Nav::Inbox));
        // The tile is the inbox: no row for it.
        assert_eq!(p.inbox_on_tile(&lists), Some("/i/"));
        assert_eq!(
            names(p.sidebar_lists(&lists, &Nav::Inbox)),
            ["Home", "Work"]
        );
        // Without the tile the row is the only way in, in its own place.
        p.hide_view(&Nav::Inbox);
        assert_eq!(p.inbox_on_tile(&lists), None);
        assert_eq!(
            names(p.sidebar_lists(&lists, &Nav::Inbox)),
            ["Home", "Inbox", "Work"]
        );
        p.show_view(Nav::Inbox, None);
        assert_eq!(
            names(p.sidebar_lists(&lists, &Nav::Inbox)),
            ["Home", "Work"]
        );
        // A drag among the rows on screen leaves the inbox's place alone.
        p.reorder_lists(
            &lists,
            &hrefs(&p.sidebar_lists(&lists, &Nav::Inbox)),
            &strings(&["/w/", "/h/"]),
        );
        assert_eq!(
            names(p.sidebar_lists(&lists, &Nav::Inbox)),
            ["Work", "Home"]
        );
        p.hide_view(&Nav::Inbox);
        assert_eq!(
            names(p.sidebar_lists(&lists, &Nav::Inbox)),
            ["Work", "Inbox", "Home"]
        );
        // Archived lists stay out either way.
        p.archived.push("/w/".into());
        assert_eq!(
            names(p.sidebar_lists(&lists, &Nav::Inbox)),
            ["Inbox", "Home"]
        );
    }

    /// Work, Inbox and Home, with `inbox` the list new tasks go to.
    fn lists_with_inbox(inbox: &str) -> Vec<ListView> {
        [("/w/", "Work"), ("/i/", "Inbox"), ("/h/", "Home")]
            .map(|(href, name)| ListView {
                inbox: href == inbox,
                ..list(href, name)
            })
            .to_vec()
    }

    #[test]
    fn the_inbox_list_cannot_be_archived() {
        let lists = lists_with_inbox("/i/");
        let mut p = Prefs::default();
        assert!(!p.set_archived("/i/", true, &lists));
        assert!(p.archived.is_empty());
        // Any other list can, once however often it is asked.
        assert!(p.set_archived("/w/", true, &lists));
        assert!(p.set_archived("/w/", true, &lists));
        assert_eq!(p.archived, ["/w/"]);
        assert!(p.set_archived("/w/", false, &lists));
        assert!(p.archived.is_empty());
        // Bringing the inbox back is always allowed: it clears an entry
        // made before the list was the inbox.
        p.archived.push("/i/".into());
        assert!(p.set_archived("/i/", false, &lists));
        assert!(p.archived.is_empty());
    }

    #[test]
    fn an_archived_inbox_href_is_ignored() {
        let p = Prefs {
            archived: strings(&["/i/", "/w/"]),
            ..Prefs::default()
        };
        // The inbox was archived, then `asst settings --inbox` chose it.
        let lists = lists_with_inbox("/i/");
        assert!(!p.is_archived("/i/", &lists));
        assert!(p.is_archived("/w/", &lists));
        assert_eq!(names(p.arrange(&lists)), ["Home", "Inbox"]);
        // Read, not rewritten: the entry waits, and counts again once
        // another list is the inbox.
        assert_eq!(p.archived, ["/i/", "/w/"]);
        let moved = lists_with_inbox("/h/");
        assert!(p.is_archived("/i/", &moved));
        assert_eq!(names(p.arrange(&moved)), ["Home"]);
        // Before any list is known, an entry is what it says.
        assert!(p.is_archived("/i/", &[]));
    }

    /// A list with `open` tasks to do and `done` completed.
    fn counted(href: &str, name: &str, open: usize, done: usize) -> ListView {
        ListView {
            open,
            done,
            ..list(href, name)
        }
    }

    /// What `split_lists` makes of `lists`, by name.
    fn split_names(
        p: &Prefs,
        lists: &[ListView],
        current: &Nav,
    ) -> (Vec<String>, Vec<(String, Why)>) {
        let split = p.split_lists(lists, current);
        (
            names(split.shown),
            split
                .hidden
                .into_iter()
                .map(|(l, why)| (l.name, why))
                .collect(),
        )
    }

    fn away(items: &[(&str, Why)]) -> Vec<(String, Why)> {
        items
            .iter()
            .map(|(n, why)| ((*n).to_string(), *why))
            .collect()
    }

    #[test]
    fn finished_and_archived_lists_tuck_away_but_the_open_one_stays() {
        let mut lists = vec![
            ListView {
                inbox: true,
                ..counted("/i/", "Inbox", 0, 5)
            },
            counted("/a/", "asst", 6, 2),
            counted("/b/", "battery-log", 0, 4),
            counted("/n/", "new", 0, 0),
            counted("/m/", "mail-digest", 0, 3),
            counted("/w/", "Work", 1, 0),
        ];
        let mut p = Prefs {
            archived: strings(&["/m/"]),
            ..Prefs::default()
        };
        let nowhere = Nav::Day;

        // The toggle is off: only the archived list is away. The inbox is
        // the tile's, finished or not.
        assert_eq!(
            split_names(&p, &lists, &nowhere),
            (
                strings(&["asst", "battery-log", "new", "Work"]),
                away(&[("mail-digest", Why::Archived)])
            )
        );
        // It still says how many it would hide.
        assert_eq!(p.finished_count(&lists), 1);

        // On: the finished list joins it, in the sidebar's order. A list
        // with no tasks yet is new, not finished.
        p.hide_finished_lists = true;
        assert_eq!(
            split_names(&p, &lists, &nowhere),
            (
                strings(&["asst", "new", "Work"]),
                away(&[
                    ("battery-log", Why::Finished),
                    ("mail-digest", Why::Archived)
                ])
            )
        );
        assert_eq!(p.finished_count(&lists), 1);

        // The list on screen stays, so completing its last task does not
        // take the row from under the cursor. It is still finished, so the
        // count does not change with where you are.
        let open = Nav::List("/b/".into());
        assert_eq!(
            split_names(&p, &lists, &open).0,
            ["asst", "battery-log", "new", "Work"]
        );
        assert_eq!(p.finished_count(&lists), 1);

        // An archived list is archived however finished it is, and open.
        let open = Nav::List("/m/".into());
        assert_eq!(
            split_names(&p, &lists, &open).1,
            away(&[
                ("battery-log", Why::Finished),
                ("mail-digest", Why::Archived)
            ])
        );

        // A task lands in the finished list (a new GitHub issue does the
        // same): it is back, in its place.
        lists[2].open = 1;
        assert_eq!(
            split_names(&p, &lists, &nowhere),
            (
                strings(&["asst", "battery-log", "new", "Work"]),
                away(&[("mail-digest", Why::Archived)])
            )
        );
        // The new list gets its first task and finishes it: now it goes.
        lists[3].done = 1;
        assert_eq!(
            split_names(&p, &lists, &nowhere).1,
            away(&[("mail-digest", Why::Archived), ("new", Why::Finished)])
        );

        // With the inbox tile hidden the inbox has a row, and no state
        // takes it away.
        p.hide_view(&Nav::Inbox);
        assert!(
            split_names(&p, &lists, &nowhere)
                .0
                .contains(&"Inbox".into())
        );
    }

    #[test]
    fn an_archived_inbox_is_shown() {
        let lists = lists_with_inbox("/i/");
        let mut p = Prefs {
            archived: strings(&["/i/", "/w/"]),
            hide_finished_lists: true,
            ..Prefs::default()
        };
        p.hide_view(&Nav::Inbox);
        // Archived before it became the inbox: the entry waits, the row is
        // there, and only Work is under Hidden lists.
        assert_eq!(
            split_names(&p, &lists, &Nav::Day),
            (
                strings(&["Home", "Inbox"]),
                away(&[("Work", Why::Archived)])
            )
        );
        // The tile has it instead: no row, hidden or not.
        p.show_view(Nav::Inbox, None);
        assert_eq!(
            split_names(&p, &lists, &Nav::Day),
            (strings(&["Home"]), away(&[("Work", Why::Archived)]))
        );
        // Once another list is the inbox the old entry counts again.
        assert_eq!(
            split_names(&p, &lists_with_inbox("/h/"), &Nav::Day),
            (
                strings(&[]),
                away(&[("Inbox", Why::Archived), ("Work", Why::Archived)])
            )
        );
    }

    #[test]
    fn unsaved_lists_follow_in_name_order() {
        // The order Radicale answers in: capitals first.
        let lists = [
            list("/errands/", "Errands"),
            list("/groceries/", "Groceries"),
            list("/inbox/", "Inbox"),
            list("/lab/", "Lab"),
            list("/work/", "Work"),
            list("/asst/", "asst"),
            list("/dotfiles/", "dotfiles"),
        ];
        let mut p = Prefs::default();
        assert_eq!(
            names(p.arrange(&lists)),
            [
                "asst",
                "dotfiles",
                "Errands",
                "Groceries",
                "Inbox",
                "Lab",
                "Work"
            ]
        );
        // The ones with a place come first, in it; the rest follow by name.
        p.list_order = strings(&["/work/", "/gone/", "/lab/"]);
        assert_eq!(
            names(p.arrange(&lists)),
            [
                "Work",
                "Lab",
                "asst",
                "dotfiles",
                "Errands",
                "Groceries",
                "Inbox"
            ]
        );
    }

    #[test]
    fn lists_by_name_true_still_orders_by_name() {
        let lists = [list("/z/", "Zed"), list("/a/", "Ant"), list("/m/", "mid")];
        let mut p = Prefs {
            lists_by_name: true,
            list_order: strings(&["/z/", "/m/", "/a/"]),
            ..Prefs::default()
        };
        assert_eq!(names(p.arrange(&lists)), ["Ant", "mid", "Zed"]);
        // The key is still read and written as it was.
        let again = parse(&toml::to_string(&p).unwrap());
        assert!(again.lists_by_name);
        assert_eq!(again.list_order, p.list_order);
        // A drag sets the order down and turns the flag off, whatever the
        // order the lists were on show in.
        let shown = p.arrange(&lists);
        let after = strings(&["/z/", "/a/", "/m/"]);
        p.reorder_lists(&lists, &hrefs(&shown), &after);
        assert!(!p.lists_by_name);
        assert_eq!(names(p.arrange(&lists)), ["Zed", "Ant", "mid"]);
    }

    #[test]
    fn the_splice_trades_the_places_of_the_lists_shown() {
        // `X` is not on show (archived); the others are dragged around it.
        let lists = [
            list("/a/", "a"),
            list("/x/", "X"),
            list("/b/", "b"),
            list("/c/", "c"),
        ];
        let mut p = Prefs {
            list_order: strings(&["/a/", "/x/", "/b/", "/c/"]),
            archived: strings(&["/x/"]),
            ..Prefs::default()
        };
        p.reorder_lists(
            &lists,
            &strings(&["/a/", "/b/", "/c/"]),
            &strings(&["/c/", "/a/", "/b/"]),
        );
        assert_eq!(p.list_order, ["/c/", "/x/", "/a/", "/b/"]);
        assert_eq!(names(p.arrange(&lists)), ["c", "a", "b"]);
    }

    #[test]
    fn a_drag_keeps_the_places_of_lists_not_shown() {
        // The shape of the real file: an order that names one list and a
        // stale entry no list has, an archived list.
        let lists = [
            list("/inbox/", "Inbox"),
            list("/mail/", "mail-digest"),
            list("/work/", "Work"),
            list("/asst/", "asst"),
            list("/dot/", "dotfiles"),
        ];
        let mut p = Prefs {
            list_order: strings(&["/inbox/", "/tasks/"]),
            archived: strings(&["/mail/"]),
            ..Prefs::default()
        };
        let shown = p.arrange(&lists);
        assert_eq!(names(shown.clone()), ["Inbox", "asst", "dotfiles", "Work"]);

        // Work to the top.
        let after = strings(&["/work/", "/inbox/", "/asst/", "/dot/"]);
        p.reorder_lists(&lists, &hrefs(&shown), &after);
        assert_eq!(
            p.list_order,
            ["/work/", "/tasks/", "/inbox/", "/asst/", "/mail/", "/dot/"]
        );
        assert_eq!(
            names(p.arrange(&lists)),
            ["Work", "Inbox", "asst", "dotfiles"]
        );

        // The archived list is where it was left: bring it back and it is
        // in its place, between asst and dotfiles.
        p.archived.clear();
        assert_eq!(
            names(p.arrange(&lists)),
            ["Work", "Inbox", "asst", "mail-digest", "dotfiles"]
        );
        // The stale entry stays too, never pruned.
        assert!(p.list_order.contains(&"/tasks/".to_string()));
    }

    #[test]
    fn a_drag_that_lost_a_race_moves_only_what_is_still_shown() {
        let lists = [
            list("/a/", "a"),
            list("/b/", "b"),
            list("/c/", "c"),
            list("/n/", "new"),
        ];
        let mut p = Prefs {
            list_order: strings(&["/a/", "/b/", "/c/", "/n/"]),
            ..Prefs::default()
        };
        // Dragged from a sidebar that did not have `new` yet, and that had
        // `gone`; `b` is named twice. The order stays a permutation: no list
        // lost or doubled, and `new` keeps its place.
        p.reorder_lists(
            &lists,
            &strings(&["/a/", "/b/", "/c/", "/n/"]),
            &strings(&["/c/", "/gone/", "/b/", "/a/", "/b/"]),
        );
        assert_eq!(p.list_order, ["/c/", "/b/", "/a/", "/n/"]);

        // A list archived since is not moved either.
        let mut q = Prefs {
            list_order: strings(&["/a/", "/b/", "/c/"]),
            archived: strings(&["/b/"]),
            ..Prefs::default()
        };
        q.reorder_lists(
            &lists,
            &strings(&["/a/", "/c/"]),
            &strings(&["/c/", "/b/", "/a/"]),
        );
        assert_eq!(&q.list_order[..3], ["/c/", "/b/", "/a/"]);
    }

    #[test]
    fn a_deleted_list_leaves_the_places_of_the_others() {
        let lists = [list("/a/", "a"), list("/b/", "b"), list("/c/", "c")];
        let mut p = Prefs::default();
        p.reorder_lists(
            &lists,
            &strings(&["/a/", "/b/", "/c/"]),
            &strings(&["/c/", "/a/", "/b/"]),
        );
        assert_eq!(p.list_order, ["/c/", "/a/", "/b/"]);
        // A delete that happened takes the place with it, and the others
        // stay as they were.
        assert!(p.forget_list("/a/"));
        assert_eq!(p.list_order, ["/c/", "/b/"]);
        let rest = [list("/b/", "b"), list("/c/", "c")];
        assert_eq!(names(p.arrange(&rest)), ["c", "b"]);
    }

    #[test]
    fn sort_a_to_z_is_case_blind_natural_and_clears_the_by_name_flag() {
        let lists = [
            list("/w10/", "Work 10"),
            list("/e/", "Errands"),
            list("/w9/", "work 9"),
            list("/a/", "asst"),
            list("/x/", "Archived one"),
        ];
        for by_name in [false, true] {
            let mut p = Prefs {
                lists_by_name: by_name,
                list_order: strings(&["/w10/", "/x/", "/e/", "/w9/", "/a/"]),
                archived: strings(&["/x/"]),
                ..Prefs::default()
            };
            let shown = p.arrange(&lists);
            p.sort_lists_by_name(&lists, &shown);
            assert!(!p.lists_by_name);
            // "asst" before "Errands" though a capital sorts first as bytes,
            // and "work 9" before "Work 10".
            assert_eq!(
                names(p.arrange(&lists)),
                ["asst", "Errands", "work 9", "Work 10"]
            );
            // The archived list kept its place among the slots.
            assert_eq!(p.list_order, ["/a/", "/x/", "/e/", "/w9/", "/w10/"]);
        }
        // Sorting what is sorted changes nothing.
        let mut p = Prefs::default();
        let shown = p.arrange(&lists);
        p.sort_lists_by_name(&lists, &shown);
        let sorted = p.list_order.clone();
        let shown = p.arrange(&lists);
        p.sort_lists_by_name(&lists, &shown);
        assert_eq!(p.list_order, sorted);
    }

    #[test]
    fn a_patch_changes_one_field_and_only_strings_skip_the_redraw() {
        // Every field away from its default, so a patch that wrote the
        // dialog's copy back would show.
        let base = Prefs {
            view: "matrix".into(),
            width: 1234,
            height: 567,
            maximized: true,
            show_counts: false,
            complete_later: false,
            sunday_first: false,
            background: false,
            lists_by_name: false,
            list_order: vec!["/b/".into(), "/a/".into()],
            list_icons: [("/a/".to_string(), "flask".to_string())].into(),
            archived: vec!["/a/".into(), "/b/".into()],
            hide_finished_lists: false,
            remember_list: false,
            default_priority: 2,
            read_dates: false,
            projects_dir: "/srv".into(),
            work_command: "true".into(),
            sidebar: Some(vec!["today".into()]),
            plan: vec![PlanSlot {
                uid: "abc".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
                start: 540,
                duration: 30,
            }],
            day_zoom: 100.0,
            views: [("today".to_string(), ViewOpts::default())].into(),
            ..Prefs::default()
        };
        let table: Vec<(PrefPatch, Prefs, bool)> = vec![
            (
                PrefPatch::ReadDates(true),
                Prefs {
                    read_dates: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::DefaultPriority(4),
                Prefs {
                    default_priority: 4,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::RememberList(true),
                Prefs {
                    remember_list: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::CompleteLater(true),
                Prefs {
                    complete_later: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::SundayFirst(true),
                Prefs {
                    sunday_first: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::Background(true),
                Prefs {
                    background: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::ShowCounts(true),
                Prefs {
                    show_counts: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::HideFinishedLists(true),
                Prefs {
                    hide_finished_lists: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::ProjectsDir("~/code".into()),
                Prefs {
                    projects_dir: "~/code".into(),
                    ..base.clone()
                },
                false,
            ),
            (
                PrefPatch::WorkCommand("echo {dir}".into()),
                Prefs {
                    work_command: "echo {dir}".into(),
                    ..base.clone()
                },
                false,
            ),
        ];
        for (patch, expected, redraws) in table {
            let mut p = base.clone();
            patch.apply(&mut p);
            assert_ne!(p, base, "{patch:?} changed nothing");
            assert_eq!(p, expected, "{patch:?} touched more than its field");
            assert_eq!(patch.redraws(), redraws, "{patch:?}");
        }
    }
}
