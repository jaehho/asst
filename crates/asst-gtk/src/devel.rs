//! Development builds (`ASST_DEVEL=1`): a separate app ID, a striped header,
//! and app actions for driving the window from a script on the session bus
//! without focusing it:
//!
//! - `snapshot(path)` saves the window as a PNG; `snapshot-popover(path)`
//!   saves the open popover.
//! - `drive(command)` does what a click or a key would (see `Window::drive`).
//! - `dump(path)` writes the machine-readable state agents assert on
//!   (see [`dump_json`]); empty path prints it on `window.log`.
//! - `texts(path)` writes what the window, an open dialog and the open
//!   popover show, widget by widget (see [`texts_json`]): labels, entries,
//!   buttons, icons and tooltips. For checks on words, with no pixels.
//!
//! A popover in a window without keyboard focus closes as soon as it opens;
//! `ASST_NO_AUTOHIDE=1` keeps them up for these checks. The quick-add popup
//! takes no keyboard in development builds.

use chrono::NaiveDate;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gio, glib, graphene};
use serde::Serialize;

use crate::model::{DayPlan, Kind, Nav, Section};
use crate::window::{Msg, Tx};

pub fn enabled() -> bool {
    std::env::var_os("ASST_DEVEL").is_some()
}

pub fn app_id() -> &'static str {
    if enabled() {
        "dev.jaeho.Asst.Devel"
    } else {
        asst_core::config::APP_ID
    }
}

pub fn install(window: &impl IsA<gtk::Window>, tx: Tx) {
    let app = relm4::main_application();
    let snapshot = gio::SimpleAction::new("snapshot", Some(glib::VariantTy::STRING));
    {
        let window = window.as_ref().clone();
        snapshot.connect_activate(move |_, path| {
            if let (Some(path), Some(content)) = (path.and_then(|p| p.str()), window.child()) {
                save_in(window.upcast_ref(), &content, path);
            }
        });
    }
    app.add_action(&snapshot);
    let popover = gio::SimpleAction::new("snapshot-popover", Some(glib::VariantTy::STRING));
    popover.connect_activate(|_, path| {
        match (path.and_then(|p| p.str()), crate::ui::last_popover()) {
            (Some(path), Some(p)) => save(p.upcast_ref(), path),
            _ => eprintln!("snapshot-popover: no popover open"),
        }
    });
    app.add_action(&popover);
    let drive = gio::SimpleAction::new("drive", Some(glib::VariantTy::STRING));
    {
        let tx = tx.clone();
        drive.connect_activate(move |_, command| {
            if let Some(c) = command.and_then(|c| c.str()) {
                tx.emit(Msg::Drive(c.to_string()));
            }
        });
    }
    app.add_action(&drive);
    let dump = gio::SimpleAction::new("dump", Some(glib::VariantTy::STRING));
    dump.connect_activate(move |_, path| {
        tx.emit(Msg::Dump(
            path.and_then(|p| p.str()).unwrap_or_default().into(),
        ));
    });
    app.add_action(&dump);
    let texts = gio::SimpleAction::new("texts", Some(glib::VariantTy::STRING));
    {
        let window = window.as_ref().clone();
        texts.connect_activate(move |_, path| {
            let Some(path) = path.and_then(|p| p.str()) else {
                return;
            };
            let mut roots = vec![("window", window.clone().upcast::<gtk::Widget>())];
            if let Some(dialog) = window.downcast_ref::<adw::ApplicationWindow>()
                && let Some(d) = dialog.visible_dialog()
            {
                roots.push(("dialog", d.upcast()));
            }
            if let Some(p) = crate::ui::last_popover().filter(|p| p.is_visible()) {
                roots.push(("popover", p.upcast()));
            }
            if let Err(e) = std::fs::write(path, texts_json(&roots)) {
                eprintln!("texts {path}: {e}");
            }
        });
    }
    app.add_action(&texts);
}

/// One thing the window shows, for [`texts_json`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shown {
    /// `window`, `dialog` or `popover`: which root it was found under.
    pub root: &'static str,
    /// `label`, `entry`, `text`, `button` or `icon`.
    pub kind: &'static str,
    /// The words (a button's own label, an entry's text, a label's text).
    pub text: String,
    /// The icon's name, for `icon` and icon-only `button`s.
    pub icon: Option<String>,
    pub tooltip: Option<String>,
    pub classes: Vec<String>,
    /// `[x, y, width, height]` in pixels, relative to the root it was found
    /// under.
    pub bounds: Option<[f32; 4]>,
}

/// Every mapped label, entry, text view, labelled or icon button and icon
/// under `roots`, in tree order.
pub fn shown(roots: &[(&'static str, gtk::Widget)]) -> Vec<Shown> {
    let mut out = Vec::new();
    for (root, widget) in roots {
        let mut stack = vec![widget.clone()];
        while let Some(w) = stack.pop() {
            if !w.is_mapped() {
                continue;
            }
            let item = |kind, text: String, icon: Option<String>| Shown {
                root,
                kind,
                text,
                icon,
                tooltip: w.tooltip_text().map(|t| t.to_string()),
                classes: w.css_classes().iter().map(|c| c.to_string()).collect(),
                bounds: w
                    .compute_bounds(widget)
                    .map(|b| [b.x(), b.y(), b.width(), b.height()]),
            };
            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                out.push(item("label", l.text().to_string(), None));
            } else if let Some(b) = w.downcast_ref::<gtk::Button>() {
                let text = b.label().map(|t| t.to_string()).unwrap_or_default();
                let icon = b.icon_name().map(|i| i.to_string());
                if !text.is_empty() || icon.is_some() {
                    out.push(item("button", text, icon));
                }
            } else if let Some(i) = w.downcast_ref::<gtk::Image>() {
                out.push(item(
                    "icon",
                    String::new(),
                    i.icon_name().map(|n| n.to_string()),
                ));
            } else if let Some(e) = w.downcast_ref::<gtk::Text>() {
                out.push(item("entry", e.text().to_string(), None));
            } else if let Some(t) = w.downcast_ref::<gtk::TextView>() {
                let b = t.buffer();
                let text = b.text(&b.start_iter(), &b.end_iter(), false);
                out.push(item("text", text.to_string(), None));
            }
            // Last child first, so the stack pops them in tree order.
            let mut child = w.last_child();
            while let Some(c) = child {
                child = c.prev_sibling();
                stack.push(c);
            }
        }
    }
    out
}

/// The JSON `texts` writes.
pub fn texts_json(roots: &[(&'static str, gtk::Widget)]) -> String {
    serde_json::to_string_pretty(&shown(roots)).unwrap_or_else(|_| "[]".into())
}

/// One visible row: what j/k walks. `kind` is the section it sits in
/// (`overdue`, `day:2026-10-09`, `list:…`, `plan`, `tray`, …).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DumpRow {
    pub id: String,
    pub summary: String,
    pub kind: String,
}

/// A placed day-plan block, after overlap layout.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DumpBlock {
    pub href: String,
    /// Minutes from midnight.
    pub start: u32,
    /// Minutes.
    pub duration: u32,
    /// 0-based column among the blocks this one overlaps.
    pub col: usize,
    /// Columns the whole overlap group shares; the block is `1/cols` wide.
    pub cols: usize,
}

/// What an agent can assert on without pixels: the view, the day cursor,
/// zoom, scroll, rows, the plan and tray, and the editor/toast/cursor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dump {
    /// `Nav::key`: `today`, `day`, `list:/cal/x/`, …
    pub nav: String,
    /// The Day view's cursor (the local day the grid is looking at).
    pub day: NaiveDate,
    /// Day-grid zoom, pixels per hour.
    pub px_per_hour: f64,
    /// Top-visible minute of the day scroller, if one is open.
    pub top_visible_minute: Option<u32>,
    /// Sidebar selection, as a `Nav::key`.
    pub sidebar: String,
    pub rows: Vec<DumpRow>,
    pub blocks: Vec<DumpBlock>,
    /// Open and overdue backlog not placed on `day`.
    pub tray: Vec<DumpRow>,
    /// The open editor card's task.
    pub editor: Option<String>,
    /// A visible toast's text.
    pub toast: Option<String>,
    /// The keyboard cursor's task.
    pub cursor: Option<String>,
}

/// Section kind as a stable string for the dump.
pub fn kind_label(kind: &Kind) -> String {
    match kind {
        Kind::Overdue => "overdue".into(),
        Kind::Day(d) => format!("day:{d}"),
        Kind::Range => "range".into(),
        Kind::List(h) => format!("list:{h}"),
        Kind::Quadrant(q) => format!("quadrant:{}", q.title().to_lowercase()),
        Kind::Completed => "completed".into(),
        Kind::Plain => "plain".into(),
    }
}

/// The one serializer for `dump`. Pure: no widgets, no clock. `sections` is
/// the current view's plan (empty tasks on Day); `plan` is the day cursor's
/// placed blocks and tray.
#[allow(clippy::too_many_arguments)]
pub fn build_dump(
    nav: &Nav,
    day: NaiveDate,
    px_per_hour: f64,
    top_visible_minute: Option<u32>,
    sections: &[Section],
    plan: &DayPlan,
    editor: Option<&str>,
    toast: Option<&str>,
    cursor: Option<&str>,
) -> Dump {
    let rows = if *nav == Nav::Day {
        plan.blocks
            .iter()
            .map(|b| DumpRow {
                id: b.task.id.clone(),
                summary: b.task.task.summary.clone(),
                kind: "plan".into(),
            })
            .chain(plan.tray.iter().map(|t| DumpRow {
                id: t.id.clone(),
                summary: t.task.summary.clone(),
                kind: "tray".into(),
            }))
            .collect()
    } else {
        sections
            .iter()
            .flat_map(|s| {
                let kind = kind_label(&s.kind);
                s.tasks.iter().map(move |t| DumpRow {
                    id: t.id.clone(),
                    summary: t.task.summary.clone(),
                    kind: kind.clone(),
                })
            })
            .collect()
    };
    Dump {
        nav: nav.key(),
        day,
        px_per_hour,
        top_visible_minute,
        sidebar: nav.key(),
        rows,
        blocks: plan
            .blocks
            .iter()
            .map(|b| DumpBlock {
                href: b.task.href.clone(),
                start: b.start,
                duration: b.duration,
                col: b.col,
                cols: b.cols,
            })
            .collect(),
        tray: plan
            .tray
            .iter()
            .map(|t| DumpRow {
                id: t.id.clone(),
                summary: t.task.summary.clone(),
                kind: "tray".into(),
            })
            .collect(),
        editor: editor.map(str::to_string),
        toast: toast.map(str::to_string),
        cursor: cursor.map(str::to_string),
    }
}

/// Pretty JSON for `build_dump`'s result.
pub fn dump_json(state: &Dump) -> String {
    serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".into())
}

/// The toast the overlay is showing, if any: its title label's text.
pub(crate) fn visible_toast(overlay: &gtk::Widget) -> Option<String> {
    let mut stack = vec![overlay.clone()];
    while let Some(w) = stack.pop() {
        if w.type_().name() == "AdwToastWidget" {
            let mut labels = vec![w.clone()];
            while let Some(l) = labels.pop() {
                if let Some(text) = l.downcast_ref::<gtk::Label>() {
                    return Some(text.text().to_string());
                }
                let mut c = l.first_child();
                while let Some(child) = c {
                    c = child.next_sibling();
                    labels.push(child);
                }
            }
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            stack.push(child);
        }
    }
    None
}

/// The part of `outer` that `inner` covers: the window's own background
/// shows through the content, so draw the window and crop.
fn save_in(outer: &gtk::Widget, inner: &gtk::Widget, path: &str) {
    let Some(bounds) = inner.compute_bounds(outer) else {
        return save(inner, path);
    };
    let (w, h) = (outer.width(), outer.height());
    let paintable = gtk::WidgetPaintable::new(Some(outer));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let (Some(node), Some(renderer)) = (
        snapshot.to_node(),
        outer.native().and_then(|n| n.renderer()),
    ) else {
        return save(inner, path);
    };
    let texture = renderer.render_texture(&node, Some(&bounds));
    if let Err(e) = texture.save_to_png(path) {
        eprintln!("snapshot {path}: {e}");
    }
}

fn save(widget: &gtk::Widget, path: &str) {
    let (w, h) = (widget.width(), widget.height());
    if w == 0 || h == 0 {
        eprintln!("snapshot {path}: nothing to draw ({w}x{h})");
        return;
    }
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let Some(node) = snapshot.to_node() else {
        eprintln!("snapshot {path}: drew nothing");
        return;
    };
    let Some(renderer) = widget.native().and_then(|n| n.renderer()) else {
        eprintln!("snapshot {path}: no renderer");
        return;
    };
    let texture = renderer.render_texture(
        &node,
        Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32)),
    );
    if let Err(e) = texture.save_to_png(path) {
        eprintln!("snapshot {path}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use asst_core::api::TaskView;
    use asst_core::task::{Status, Task};
    use chrono::NaiveDate;

    use super::*;
    use crate::model::{PlanBlock, Quadrant};

    fn task(id: &str, summary: &str) -> TaskView {
        TaskView {
            id: id.into(),
            href: format!("/l/{id}.ics"),
            list: "/l/".into(),
            list_name: "L".into(),
            pending: false,
            issue: None,
            origin: "asst".into(),
            task: Task {
                uid: id.into(),
                summary: summary.into(),
                description: None,
                status: Status::NeedsAction,
                completed: None,
                priority: 0,
                due: None,
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

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(kind_label(&Kind::Overdue), "overdue");
        assert_eq!(
            kind_label(&Kind::Day(NaiveDate::from_ymd_opt(2026, 10, 9).unwrap())),
            "day:2026-10-09"
        );
        assert_eq!(kind_label(&Kind::List("/l/".into())), "list:/l/");
        assert_eq!(kind_label(&Kind::Quadrant(Quadrant::Do)), "quadrant:do");
        assert_eq!(kind_label(&Kind::Completed), "completed");
        assert_eq!(kind_label(&Kind::Plain), "plain");
    }

    #[test]
    fn dump_json_carries_rows_blocks_and_flags() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let sections = vec![Section {
            kind: Kind::Overdue,
            title: Some("Overdue".into()),
            note: None,
            tasks: vec![task("aa11", "Pay rent")],
            keep: true,
        }];
        let plan = DayPlan {
            day,
            hour_start: 0,
            hour_end: 24,
            blocks: vec![PlanBlock {
                task: task("bb22", "Standup"),
                start: 9 * 60,
                duration: 30,
                col: 0,
                cols: 1,
            }],
            tray: vec![task("cc33", "Call bank")],
            now_min: Some(10 * 60),
        };
        let d = build_dump(
            &Nav::Today,
            day,
            52.0,
            Some(8 * 60),
            &sections,
            &plan,
            Some("/l/bb22.ics"),
            Some("Copied to the clipboard"),
            Some("/l/aa11.ics"),
        );
        let v: serde_json::Value = serde_json::from_str(&dump_json(&d)).unwrap();
        assert_eq!(v["nav"], "today");
        assert_eq!(v["sidebar"], "today");
        assert_eq!(v["day"], "2026-10-09");
        assert_eq!(v["px_per_hour"], 52.0);
        assert_eq!(v["top_visible_minute"], 480);
        assert_eq!(v["rows"][0]["id"], "aa11");
        assert_eq!(v["rows"][0]["summary"], "Pay rent");
        assert_eq!(v["rows"][0]["kind"], "overdue");
        assert_eq!(v["blocks"][0]["href"], "/l/bb22.ics");
        assert_eq!(v["blocks"][0]["start"], 540);
        assert_eq!(v["blocks"][0]["duration"], 30);
        assert_eq!(v["blocks"][0]["col"], 0);
        assert_eq!(v["blocks"][0]["cols"], 1);
        assert_eq!(v["tray"][0]["id"], "cc33");
        assert_eq!(v["editor"], "/l/bb22.ics");
        assert_eq!(v["toast"], "Copied to the clipboard");
        assert_eq!(v["cursor"], "/l/aa11.ics");
    }

    #[test]
    fn day_dump_rows_are_blocks_and_tray() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let plan = DayPlan {
            day,
            hour_start: 0,
            hour_end: 24,
            blocks: vec![PlanBlock {
                task: task("bb22", "Standup"),
                start: 9 * 60,
                duration: 30,
                col: 0,
                cols: 1,
            }],
            tray: vec![task("cc33", "Call bank")],
            now_min: None,
        };
        let d = build_dump(&Nav::Day, day, 52.0, None, &[], &plan, None, None, None);
        let kinds: Vec<&str> = d.rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["plan", "tray"]);
        assert_eq!(d.rows[0].id, "bb22");
        assert_eq!(d.rows[1].summary, "Call bank");
    }

    #[test]
    fn empty_dump_serializes_nulls() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let plan = DayPlan {
            day,
            hour_start: 0,
            hour_end: 24,
            blocks: Vec::new(),
            tray: Vec::new(),
            now_min: None,
        };
        let d = build_dump(&Nav::Today, day, 52.0, None, &[], &plan, None, None, None);
        let v: serde_json::Value = serde_json::from_str(&dump_json(&d)).unwrap();
        assert!(v["editor"].is_null());
        assert!(v["toast"].is_null());
        assert!(v["cursor"].is_null());
        assert!(v["top_visible_minute"].is_null());
        assert_eq!(v["rows"], serde_json::json!([]));
    }
}
