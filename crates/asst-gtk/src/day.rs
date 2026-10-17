//! The Day view: one time axis, tracked ActivityWatch blocks in the left
//! lane, plan slots in the right. The grid chrome (hour rows, now-line,
//! day header, scroller, zoom) is `daygrid`. The unscheduled tray sits in
//! a third column beside the grid so it never steals the time axis.

use relm4::gtk::{self, prelude::*};

use crate::daygrid::{self, DayStep};
use crate::{content, plan, row, time};

/// The Day column: header, two-lane grid, and the tray.
pub fn build_column(
    o: &content::Opts<'_>,
    _sections: &[crate::model::Section],
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
    let note = format!(
        "{} planned · {} unscheduled · {}",
        p.blocks.len(),
        p.tray.len(),
        time::day_note(o.blocks)
    );
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
    root.append(&daygrid::day_header(
        "Day",
        day,
        &note,
        today,
        DayStep::Day,
        carry,
        tx,
    ));

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
    scroll.set_child(Some(&daygrid::day_grid(day, o.now, px, |lanes| {
        time::fill_lane(blocks, tday, zone, &lanes.time, px);
        plan::fill_lane(plan, tx, &mut rows, px, &lanes.plan);
    })));
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
            hold: o.hold,
            px_per_hour: px,
        },
    );
    daygrid::attach_zoom(&scroll, tx, px);

    content::Built { root, rows }
}
