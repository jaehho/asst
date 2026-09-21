//! Animations, as Planify has them: rows open up as they arrive and fold
//! away as they go, a task grows into its card, a checked box pops.
//!
//! The view is rebuilt from scratch on every change, so nothing moves by
//! itself: a row that is new since the last build is made closed and opened
//! once it is on screen, and a row that is gone keeps its old widget a moment
//! longer, folding shut next to a neighbor that stayed. The window holds off
//! the next rebuild until these have played. GTK's own setting (reduced
//! motion) turns all of it off.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use relm4::gtk::prelude::*;
use relm4::gtk::{self, glib};

/// A row opening or folding, and a task growing into its card.
pub const ROW_MS: u32 = 220;

/// More rows than this arriving or leaving at once (a sync, a filter) just
/// show up, instead of a crowd of them sliding.
const MOST: usize = 12;

thread_local! {
    static OPENING: RefCell<Vec<glib::WeakRef<gtk::Revealer>>> = const { RefCell::new(Vec::new()) };
}

pub fn enabled() -> bool {
    gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations())
}

/// How long `ms` of animation takes here: nothing when animations are off,
/// longer under GTK_SLOWDOWN, which GTK stretches its own by.
pub fn lasts(ms: u32) -> Duration {
    if !enabled() {
        return Duration::ZERO;
    }
    let slowdown = std::env::var("GTK_SLOWDOWN")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|f| *f > 0.0)
        .unwrap_or(1.0);
    Duration::from_millis((f64::from(ms) * slowdown) as u64)
}

pub fn few(arriving: usize, leaving: usize) -> bool {
    arriving + leaving <= MOST
}

/// Run `f` once `widget` has been drawn, so a style changed then transitions
/// from what was on screen.
pub fn after_frame(widget: &impl IsA<gtk::Widget>, f: impl FnOnce() + 'static) {
    let f = RefCell::new(Some(f));
    let ticks = std::cell::Cell::new(0);
    widget.add_tick_callback(move |_, _| {
        ticks.set(ticks.get() + 1);
        if ticks.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        if let Some(f) = f.take() {
            f();
        }
        glib::ControlFlow::Break
    });
}

/// A closed revealer around `child` that opens once the view it is built
/// into is on screen (`open_new`).
pub fn opening(child: &impl IsA<gtk::Widget>, kind: gtk::RevealerTransitionType) -> gtk::Revealer {
    let revealer = gtk::Revealer::builder()
        .transition_type(kind)
        .transition_duration(ROW_MS)
        .child(child)
        .build();
    OPENING.with(|o| o.borrow_mut().push(revealer.downgrade()));
    revealer
}

/// Open what `opening` made, now that it is mapped and can slide.
pub fn open_new() {
    for revealer in OPENING.with(|o| std::mem::take(&mut *o.borrow_mut())) {
        if let Some(r) = revealer.upgrade() {
            r.set_reveal_child(true);
        }
    }
}

/// A row that is new since the last build: made closed, it opens and fades in.
pub fn arrive(row: &gtk::ListBoxRow) {
    let Some(child) = row.child() else {
        return;
    };
    row.set_child(None::<&gtk::Widget>);
    child.add_css_class("arriving");
    row.set_child(Some(&opening(
        &child,
        gtk::RevealerTransitionType::SlideDown,
    )));
}

/// Rows of the last build that the new one doesn't have fold shut where they
/// were: after the nearest row above them that stayed, or before the one
/// below. A row with no neighbor left in its list (the whole section went)
/// just goes. Whether any fold.
pub fn leave(old: &[(String, gtk::ListBoxRow)], new: &[(String, gtk::ListBoxRow)]) -> bool {
    let new: HashMap<&str, &gtk::ListBoxRow> = new.iter().map(|(h, r)| (h.as_str(), r)).collect();
    // Where each goes, decided while the old lists are whole: a run of rows
    // leaving together keeps its order by following the one before it.
    let mut plan: Vec<(&str, &gtk::ListBoxRow, bool, String)> = Vec::new();
    for (href, row) in old {
        // The open task's row holds the editor, which the new build has taken.
        if new.contains_key(href.as_str()) || row.has_css_class("editing") {
            continue;
        }
        let name_of = |w: gtk::Widget| w.widget_name().to_string();
        let above = std::iter::successors(row.prev_sibling(), |w| w.prev_sibling())
            .map(name_of)
            .find(|n| new.contains_key(n.as_str()) || plan.iter().any(|(h, ..)| h == n));
        let anchor = above.map(|n| (true, n)).or_else(|| {
            std::iter::successors(row.next_sibling(), |w| w.next_sibling())
                .map(name_of)
                .find(|n| new.contains_key(n.as_str()))
                .map(|n| (false, n))
        });
        if let Some((after, neighbor)) = anchor {
            plan.push((href, row, after, neighbor));
        }
    }
    let mut placed: HashMap<&str, &gtk::ListBoxRow> = HashMap::new();
    for (href, row, after, neighbor) in plan {
        let Some(neighbor) = new
            .get(neighbor.as_str())
            .or_else(|| placed.get(neighbor.as_str()))
        else {
            continue;
        };
        let (Some(from), Some(to)) = (
            row.parent().and_downcast::<gtk::ListBox>(),
            neighbor.parent().and_downcast::<gtk::ListBox>(),
        ) else {
            continue;
        };
        from.remove(row);
        fold(row, &to, neighbor.index() + i32::from(after));
        placed.insert(href, row);
    }
    !placed.is_empty()
}

/// Put a departing row back into `list` at `at`, and fold it away.
fn fold(row: &gtk::ListBoxRow, list: &gtk::ListBox, at: i32) {
    row.set_activatable(false);
    row.set_focusable(false);
    row.set_can_target(false);
    row.remove_css_class("cursor");
    let Some(child) = row.child() else {
        return;
    };
    row.set_child(None::<&gtk::Widget>);
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(ROW_MS)
        .reveal_child(true)
        .child(&child)
        .build();
    row.set_child(Some(&revealer));
    list.insert(row, at);
    child.add_css_class("leaving");
    let weak = row.downgrade();
    revealer.connect_child_revealed_notify(move |r| {
        if !r.is_child_revealed()
            && let Some(row) = weak.upgrade()
            && let Some(list) = row.parent().and_downcast::<gtk::ListBox>()
        {
            list.remove(&row);
        }
    });
    revealer.set_reveal_child(false);
}
