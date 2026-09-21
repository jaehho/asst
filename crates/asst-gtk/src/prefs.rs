//! The window's own preferences, `~/.config/asst/window.toml`. The daemon's
//! settings are in `config.toml` and go through D-Bus.

use std::collections::BTreeMap;
use std::path::PathBuf;

use asst_core::api::ListView;
use serde::{Deserialize, Serialize};

use crate::model::{Nav, ViewOpts};

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
    /// Lists put away (hrefs): out of the sidebar, the views and the counts.
    /// They still sync, and their reminders still ring.
    pub archived: Vec<String>,
    /// Quick add starts in the list it added to last.
    pub remember_list: bool,
    /// A new task's priority when none is given, 1 to 4 (none).
    pub default_priority: u8,
    /// A date or repeat typed in a new task's title becomes its own.
    pub read_dates: bool,
    /// The views in the sidebar, in order, as `Nav::key`s. Unset: the default.
    pub sidebar: Option<Vec<String>>,
    /// Before `sidebar`: views shown besides Inbox, Today, Scheduled and
    /// Completed, which always were. Read once, then written as `sidebar`.
    #[serde(skip_serializing)]
    filters: Vec<String>,
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
            archived: Vec::new(),
            remember_list: false,
            default_priority: 4,
            read_dates: true,
            sidebar: None,
            filters: Vec::new(),
            views: BTreeMap::new(),
        }
    }
}

/// The views a new sidebar has.
const DEFAULT_SIDEBAR: [Nav; 4] = [Nav::Inbox, Nav::Today, Nav::Scheduled, Nav::Completed];

fn path() -> PathBuf {
    asst_core::config::config_path().with_file_name("window.toml")
}

impl Prefs {
    pub fn load() -> Prefs {
        std::fs::read_to_string(path())
            .ok()
            .map(|s| Prefs::parse(&s))
            .unwrap_or_default()
    }

    fn parse(text: &str) -> Prefs {
        let mut p: Prefs = toml::from_str(text).unwrap_or_default();
        if p.sidebar.is_none() && !p.filters.is_empty() {
            let mut keys: Vec<String> = DEFAULT_SIDEBAR.iter().map(Nav::key).collect();
            keys.extend(std::mem::take(&mut p.filters));
            p.sidebar = Some(keys);
        }
        p
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_starts_with_four_and_keeps_old_extras() {
        assert_eq!(Prefs::parse("").sidebar_views(), DEFAULT_SIDEBAR);
        let old = Prefs::parse("filters = [\"tomorrow\"]\n");
        assert_eq!(
            old.sidebar_views(),
            [
                Nav::Inbox,
                Nav::Today,
                Nav::Scheduled,
                Nav::Completed,
                Nav::Tomorrow
            ]
        );
        let text = toml::to_string(&old).unwrap();
        assert!(!text.contains("filters"));
        assert_eq!(Prefs::parse(&text).sidebar_views(), old.sidebar_views());

        let mut none = Prefs::default();
        none.set_sidebar_views(&[]);
        let text = toml::to_string(&none).unwrap();
        assert!(Prefs::parse(&text).sidebar_views().is_empty());
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
}
