//! The Day view: one time axis, tracked ActivityWatch blocks in the left
//! lane, plan slots in the right. The grid chrome (hour rows, now-line,
//! day header, scroller, zoom) is `daygrid`. The tray of tasks to place sits
//! in a third column beside the grid so it never steals the time axis.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use asst_core::api::BlockView;
use chrono::NaiveDate;
use chrono_tz::Tz;
use relm4::gtk::{self, glib, prelude::*};

use crate::daygrid::{self, DayStep};
use crate::{content, plan, row, time};

/// The tracked lane once built: enough to draw it again at another zoom
/// level or for new data, in place.
struct TimeLane {
    lane: Option<glib::WeakRef<gtk::Overlay>>,
    reflow: daygrid::Reflow,
    day: NaiveDate,
    zone: Tz,
    blocks: RefCell<Vec<BlockView>>,
    chips: RefCell<Vec<gtk::Widget>>,
    shown: Cell<time::Detail>,
}

impl TimeLane {
    /// Draw the chips again for `px`. With `fade` the old ones dissolve out
    /// as the new dissolve in; without, they swap inside one frame, which
    /// is what keeps a block growing by a minute from being seen at all.
    fn redraw(&self, px: f64, fade: bool) {
        let Some(lane) = self.lane.as_ref().and_then(|l| l.upgrade()) else {
            return;
        };
        self.shown.set(time::detail_for(px));
        let old = self.chips.take();
        self.reflow.forget(&old);
        if fade {
            for w in &old {
                w.set_can_target(false);
                w.add_css_class("chip-out");
            }
            glib::timeout_add_local_once(std::time::Duration::from_millis(200), {
                let lane = lane.clone();
                move || {
                    for w in &old {
                        lane.remove_overlay(w);
                    }
                }
            });
        } else {
            for w in &old {
                lane.remove_overlay(w);
            }
        }
        let fresh = time::fill_lane(
            &self.blocks.borrow(),
            self.day,
            self.zone,
            &lane,
            px,
            &self.reflow,
        );
        if fade {
            for w in &fresh {
                w.add_css_class("chip-in");
            }
        }
        *self.chips.borrow_mut() = fresh;
    }
}

/// The Day column: header, two-lane grid, and the tray.
pub fn build_column(
    o: &content::Opts<'_>,
    _sections: &[crate::model::Section],
    add_card: &gtk::Box,
    tx: &crate::window::Tx,
) -> content::Built {
    let p = o.plan;
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("view-column");
    root.add_css_class("day-view");
    if o.fresh.view {
        root.add_css_class("view-enter");
    }
    if o.editing.is_some() {
        root.add_css_class("focusing");
    }

    let today = o.now.date_naive();
    let day = p.day;
    // Plan and actual stay two lanes; the note is the only place they
    // meet, so a day reads "3h planned · 2.5h tracked · −30m".
    let planned_secs: f64 = p.blocks.iter().map(|b| f64::from(b.duration) * 60.0).sum();
    let (to_place, placed) = (p.tray.len(), p.blocks.len());
    let note_for = move |blocks: &[asst_core::api::BlockView]| {
        format!(
            "{} · {to_place} to place · {}",
            time::day_note(blocks, planned_secs),
            if placed == 0 {
                "nothing placed".to_string()
            } else {
                format!("{placed} placed")
            }
        )
    };
    let note = note_for(o.blocks);
    // Unfinished work on this day can be carried to the next: one button,
    // there only while there is something to carry.
    let carry = (!p.blocks.is_empty()).then(|| {
        let to = crate::model::carry_target(day, today);
        let button = gtk::Button::with_label(&format!(
            "Carry {} to {}",
            p.blocks.len(),
            asst_core::fmt::day_label(to, today)
        ));
        button.add_css_class("pill");
        button.set_tooltip_text(Some(
            "Move this day's unfinished blocks to the next day, at the same times",
        ));
        let tx = tx.clone();
        button.connect_clicked(move |_| tx.emit(crate::window::Msg::PlanCarry));
        button
    });
    let header = daygrid::day_header("Day", day, &note, today, DayStep::Day, carry, tx);
    // The header's last child is the note line: it follows the tracked
    // total without the column being rebuilt.
    let note_label = header.last_child().and_downcast::<gtk::Label>();
    root.append(&header);

    if o.adding.is_some() {
        root.append(&content::card(o, add_card));
    }

    let mut rows: Vec<(String, gtk::Widget)> = Vec::new();
    if let Some(href) = o.editing {
        let list = gtk::ListBox::new();
        list.add_css_class("task-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let r = row::editor_row(href, o.editor);
        list.append(&r);
        rows.push((href.to_string(), r.into()));
        root.append(&list);
    }

    let cols = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    cols.add_css_class("day-columns");
    cols.set_vexpand(true);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    // The day grid is taller than a short column; ask for most of a window
    // so the overlays (chips, blocks, the now-line) get the full grid.
    scroll.set_min_content_height(400);
    scroll.add_css_class("time-scroll");
    scroll.add_css_class("day-scroll");
    let zone = o.now.timezone();
    let (blocks, plan, px, tday) = (o.blocks, p, o.px_per_hour, day);
    // One reflow for the whole grid: a zoom moves the hour rows, the
    // now-line and every chip through it without rebuilding the tree.
    let reflow = daygrid::Reflow::default();
    let mut time_lane: Option<gtk::Overlay> = None;
    let mut chips: Vec<gtk::Widget> = Vec::new();
    scroll.set_child(Some(&daygrid::day_grid(day, o.now, px, &reflow, |lanes| {
        chips = time::fill_lane(blocks, tday, zone, &lanes.time, px, &reflow);
        time_lane = Some(lanes.time.clone());
        plan::fill_lane(plan, tx, &mut rows, px, &lanes.plan, &reflow);
    })));
    // What the grid redraws without a rebuild of the column: the tracked
    // chips when the zoom crosses into another level (a dissolve) or when
    // fresh data arrives (a plain swap, in one frame), the header note, and
    // the now-line.
    let lane = Rc::new(TimeLane {
        lane: time_lane.map(|l| l.downgrade()),
        reflow: reflow.clone(),
        day: tday,
        zone,
        blocks: RefCell::new(o.blocks.to_vec()),
        chips: RefCell::new(chips),
        shown: Cell::new(time::detail_for(px)),
    });
    let refill: Rc<dyn Fn(f64)> = {
        let lane = lane.clone();
        Rc::new(move |px| {
            if time::detail_for(px) != lane.shown.get() {
                lane.redraw(px, true);
            }
        })
    };
    let refresh: daygrid::Refresh = {
        let note_label = note_label.map(|l| l.downgrade());
        Rc::new(move |blocks, tracked_changed, now| {
            if tracked_changed {
                *lane.blocks.borrow_mut() = blocks.to_vec();
                lane.redraw(lane.reflow.px(), false);
                if let Some(l) = note_label.as_ref().and_then(|l| l.upgrade()) {
                    l.set_text(&note_for(blocks));
                }
            }
            lane.reflow.set_now(daygrid::now_minutes(now));
        })
    };
    cols.append(&scroll);
    cols.append(&plan::tray(p, o, tx, &mut rows));
    root.append(&cols);

    // Scroll to now on today, else to the first thing on the grid.
    let now_min = p.now_min.map(f64::from).or_else(|| {
        (day == today)
            .then(|| time::first_min(o.blocks, day, zone))
            .flatten()
    });
    let first_min = p
        .blocks
        .iter()
        .map(|b| f64::from(b.start))
        .chain(time::first_min(o.blocks, day, zone))
        .fold(None, |acc: Option<f64>, m| {
            Some(acc.map_or(m, |a| a.min(m)))
        });
    daygrid::attach_scroll(
        &scroll,
        daygrid::ScrollRest {
            step: DayStep::Day,
            day,
            today,
            now_min,
            first_min,
            fresh_view: o.fresh.view,
            px_per_hour: px,
        },
    );
    daygrid::live(&reflow, &scroll, Some(refill), Some(refresh));
    daygrid::attach_zoom(&scroll, tx, px);

    content::Built { root, rows }
}
