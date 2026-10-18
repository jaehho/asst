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
    /// in the server's order.
    pub list_order: Vec<String>,
    /// A list's icon (a Lucide name, see `ui::list_icon_names`), by href.
    /// Window-only: the server has nowhere to keep one.
    pub list_icons: BTreeMap<String, String>,
    /// Lists put away (hrefs): out of the sidebar, the views and the counts.
    /// They still sync, and their reminders still ring.
    pub archived: Vec<String>,
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
            view: Nav::Today.key(),
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
            remember_list: false,
            default_priority: 4,
            read_dates: true,
            projects_dir: "~/projects".into(),
            work_command: "kitty --directory {dir} gh-co {number}".into(),
            sidebar: None,
            filters: Vec::new(),
            plan: Vec::new(),
            day_zoom: crate::model::PLAN_PX_PER_HOUR,
            views: BTreeMap::new(),
        }
    }
}

/// The views a new sidebar has.
const DEFAULT_SIDEBAR: [Nav; 6] = [
    Nav::Inbox,
    Nav::Today,
    Nav::Day,
    Nav::Matrix,
    Nav::Scheduled,
    Nav::Completed,
];

fn path() -> PathBuf {
    asst_core::config::window_path()
}

impl Prefs {
    pub fn load() -> Prefs {
        Prefs::load_from(&path())
    }

    /// The prefs in `file`; the defaults when there is no file. A file that
    /// cannot be read or parsed is moved to `<file>.bad` first and said so on
    /// stderr. The window writes the file whole, so its next save would
    /// otherwise replace everything the file held, not only the bad key, with
    /// the defaults.
    fn load_from(file: &Path) -> Prefs {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Prefs::default(),
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
        Prefs::default()
    }

    fn parse(text: &str) -> Result<Prefs, toml::de::Error> {
        let mut p: Prefs = toml::from_str(text)?;
        p.day_zoom = crate::model::clamp_px_per_hour(p.day_zoom);
        if p.sidebar.is_none() && !p.filters.is_empty() {
            let mut keys: Vec<String> = DEFAULT_SIDEBAR.iter().map(Nav::key).collect();
            keys.extend(std::mem::take(&mut p.filters));
            p.sidebar = Some(keys);
        }
        // The default sidebar as it was before Matrix: put the new view in,
        // once, without touching a sidebar the user arranged.
        const BEFORE_MATRIX: [&str; 4] = ["inbox", "today", "scheduled", "completed"];
        if let Some(keys) = &p.sidebar
            && keys.iter().map(String::as_str).eq(BEFORE_MATRIX)
        {
            p.sidebar = Some(DEFAULT_SIDEBAR.iter().map(Nav::key).collect());
        }
        // Same for Time after the stock five: insert it before Completed.
        if let Some(keys) = &mut p.sidebar {
            let has_time = keys.iter().any(|k| k == "time");
            let stock: Vec<&str> = keys.iter().map(String::as_str).collect();
            const BEFORE_TIME: [&str; 5] = ["inbox", "today", "matrix", "scheduled", "completed"];
            if !has_time && stock == BEFORE_TIME {
                *keys = DEFAULT_SIDEBAR.iter().map(Nav::key).collect();
            }
        }
        // Same for Plan after the stock six: insert it after Today.
        if let Some(keys) = &mut p.sidebar {
            let has_plan = keys.iter().any(|k| k == "plan");
            let stock: Vec<&str> = keys.iter().map(String::as_str).collect();
            const BEFORE_PLAN: [&str; 6] =
                ["inbox", "today", "matrix", "scheduled", "time", "completed"];
            if !has_plan && stock == BEFORE_PLAN {
                *keys = DEFAULT_SIDEBAR.iter().map(Nav::key).collect();
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
        Ok(p)
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

    /// The views in the sidebar, top to bottom.
    pub fn sidebar_views(&self) -> Vec<Nav> {
        match &self.sidebar {
            Some(keys) => keys
                .iter()
                .filter_map(|k| Nav::from_key(k).filter(|n| !matches!(n, Nav::List(_))))
                .collect(),
            None => DEFAULT_SIDEBAR.to_vec(),
        }
    }

    pub fn set_sidebar_views(&mut self, views: &[Nav]) {
        self.sidebar = Some(views.iter().map(Nav::key).collect());
    }

    pub fn is_archived(&self, href: &str) -> bool {
        self.archived.iter().any(|a| a == href)
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
        let mut out: Vec<ListView> = lists
            .iter()
            .filter(|l| !self.is_archived(&l.href))
            .cloned()
            .collect();
        if self.lists_by_name {
            out.sort_by(|a, b| crate::model::natural_cmp(&a.name, &b.name));
        } else {
            let place = |l: &ListView| {
                self.list_order
                    .iter()
                    .position(|h| *h == l.href)
                    .unwrap_or(usize::MAX)
            };
            // Stable: lists never dragged keep the server's order among them.
            out.sort_by_key(place);
        }
        out
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
    ListsByName(bool),
    /// Take a list out of `archived`, by href.
    Unarchive(String),
    /// The views in the sidebar, in order.
    SidebarViews(Vec<Nav>),
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
            PrefPatch::ListsByName(on) => p.lists_by_name = *on,
            PrefPatch::Unarchive(href) => p.archived.retain(|h| h != href),
            PrefPatch::SidebarViews(views) => p.set_sidebar_views(views),
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

    #[test]
    fn sidebar_starts_with_the_defaults_and_keeps_old_extras() {
        assert_eq!(parse("").sidebar_views(), DEFAULT_SIDEBAR);
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
        // A sidebar arranged before Matrix stays as it was; the stock four
        // gain Matrix (and then Day).
        let custom = parse("sidebar = [\"today\", \"inbox\"]\n");
        assert_eq!(custom.sidebar_views(), [Nav::Today, Nav::Inbox]);
        let stock = parse("sidebar = [\"inbox\", \"today\", \"scheduled\", \"completed\"]\n");
        assert_eq!(stock.sidebar_views(), DEFAULT_SIDEBAR);
        // Stock five with Matrix gains Day once.
        let with_matrix =
            parse("sidebar = [\"inbox\", \"today\", \"matrix\", \"scheduled\", \"completed\"]\n");
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

        // No file: the defaults, and nothing made.
        assert_eq!(Prefs::load_from(&file), Prefs::default());
        assert!(!kept.exists());

        // A good file loads as it is.
        std::fs::write(&file, "view = \"matrix\"\n").unwrap();
        assert_eq!(Prefs::load_from(&file).view, "matrix");
        assert!(!kept.exists());

        // A bad one is moved, whole, before the defaults take its place.
        std::fs::write(&file, bad).unwrap();
        assert_eq!(Prefs::load_from(&file), Prefs::default());
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), bad);

        // A second bad file replaces the first copy rather than failing.
        std::fs::write(&file, "height = true\n").unwrap();
        assert_eq!(Prefs::load_from(&file), Prefs::default());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "height = true\n");
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

    #[test]
    fn lists_in_dragged_order_or_by_name() {
        let list = |href: &str, name: &str| ListView {
            href: href.into(),
            name: name.into(),
            color: None,
            writable: true,
            inbox: false,
            open: 0,
            done: 0,
        };
        let lists = [
            list("/a/", "Work 10"),
            list("/b/", "Home"),
            list("/c/", "Work 9"),
            list("/d/", "Old"),
        ];
        let names = |v: Vec<ListView>| v.into_iter().map(|l| l.name).collect::<Vec<_>>();
        let mut p = Prefs {
            archived: vec!["/d/".into()],
            ..Prefs::default()
        };
        assert_eq!(names(p.arrange(&lists)), ["Work 10", "Home", "Work 9"]);
        p.list_order = vec!["/c/".into(), "/a/".into()];
        assert_eq!(names(p.arrange(&lists)), ["Work 9", "Work 10", "Home"]);
        p.lists_by_name = true;
        assert_eq!(names(p.arrange(&lists)), ["Home", "Work 9", "Work 10"]);
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
                PrefPatch::ListsByName(true),
                Prefs {
                    lists_by_name: true,
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::Unarchive("/a/".into()),
                Prefs {
                    archived: vec!["/b/".into()],
                    ..base.clone()
                },
                true,
            ),
            (
                PrefPatch::SidebarViews(vec![Nav::Inbox, Nav::Day]),
                Prefs {
                    sidebar: Some(vec!["inbox".into(), "day".into()]),
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
