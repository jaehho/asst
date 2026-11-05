//! The Week view: the week around the Day view's day, seven columns of
//! calendar events and planned tasks on the Day grid's hour axis. It is for
//! seeing the week. A date opens that day in Day, where planning happens,
//! and a planned task opens in place. No tracked time and no tray: Day has
//! both.

use std::rc::Rc;

use asst_core::api::EventView;
use chrono::NaiveDate;
use relm4::gtk::{self, prelude::*};

use crate::daygrid::{self, DayStep};
use crate::model::{self, DayPlan};
use crate::window::{Msg, Tx};
use crate::{content, events, plan, row};

/// One day of the week on view: its plan, laid out around its timed
/// events (`DayPlan::beside_events`), and all its events.
pub struct WeekDay {
    pub plan: DayPlan,
    pub events: Vec<EventView>,
}

/// The Week column: header, a row of dates with their all-day events, and
/// the seven-lane grid.
pub fn build_column(
    o: &content::Opts<'_>,
    week: &[WeekDay],
    add_card: &gtk::Box,
    tx: &Tx,
) -> content::Built {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("view-column");
    root.add_css_class("day-view");
    root.add_css_class("week-view");
    if o.fresh.view {
        root.add_css_class("view-enter");
    }
    if o.editing.is_some() {
        root.add_css_class("focusing");
    }

    let today = o.now.date_naive();
    let zone = o.now.timezone();
    let days: Vec<NaiveDate> = week.iter().map(|d| d.plan.day).collect();
    let (Some(&first), Some(&last)) = (days.first(), days.last()) else {
        return content::Built {
            root,
            rows: Vec::new(),
        };
    };
    let planned: u32 = week
        .iter()
        .flat_map(|d| &d.plan.blocks)
        .map(|b| b.duration)
        .sum();
    let n_events: usize = week.iter().map(|d| d.events.len()).sum();
    let range = model::week_label(first, last);
    root.append(&daygrid::day_header(
        daygrid::Header {
            title: "Week",
            date: &range,
            note: &model::week_note(planned, n_events),
            day: o.plan.day,
            today,
            step: DayStep::Week,
            actions: vec![daygrid::span_switch(true, tx).upcast()],
        },
        tx,
    ));

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

    // The dates' spacer and the grid's hour gutter are one width, however
    // wide the hour labels make the gutter, so each date sits over its lane.
    let gutter_size = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    root.append(&day_heads(week, today, &gutter_size, tx));

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_min_content_height(400);
    scroll.add_css_class("time-scroll");
    scroll.add_css_class("week-scroll");
    let px = o.px_per_hour;
    // One reflow for the whole week: a zoom moves every lane through it.
    let reflow = daygrid::Reflow::default();
    let timed: Vec<Vec<events::Timed<'_>>> = week
        .iter()
        .map(|d| events::timed(&d.events, d.plan.day, zone))
        .collect();
    scroll.set_child(Some(&daygrid::week_grid(
        &days,
        o.now,
        px,
        &reflow,
        &gutter_size,
        |lanes| {
            for ((d, lane), timed) in week.iter().zip(lanes).zip(&timed) {
                // A day's events and planned tasks share its lane, side by
                // side where they overlap, as in Day's plan lane.
                let columns = daygrid::Columns::on(lane);
                events::fill_lane(timed, &d.plan.events, lane, &columns, &reflow);
                fill_blocks(&d.plan, lane, &columns, &reflow, tx);
            }
        },
    )));
    root.append(&scroll);

    // Now on a week that holds today, else the first thing in it.
    let first_min = week
        .iter()
        .flat_map(|d| d.plan.blocks.iter().map(|b| f64::from(b.start)))
        .chain(timed.iter().flatten().map(|e| e.start_min))
        .fold(None, |acc: Option<f64>, m| {
            Some(acc.map_or(m, |a| a.min(m)))
        });
    daygrid::attach_scroll(
        &scroll,
        daygrid::ScrollRest {
            step: DayStep::Week,
            day: first,
            shows_today: days.contains(&today),
            now_min: week.iter().find_map(|d| d.plan.now_min).map(f64::from),
            first_min,
            fresh_view: o.fresh.view,
            px_per_hour: px,
        },
    );
    // The clock moves the now-line in place; the week has no tracked lane.
    let refresh: daygrid::Refresh = {
        let reflow = reflow.clone();
        Rc::new(move |_blocks, _tracked_changed, now| {
            reflow.set_now(daygrid::now_minutes(now));
        })
    };
    daygrid::live(&reflow, &scroll, None, Some(refresh));
    daygrid::attach_zoom(&scroll, tx, px);

    content::Built { root, rows }
}

/// The dates over the lanes, each a button that opens its day in Day, with
/// that day's all-day events under it. A spacer in `gutter_size` comes
/// first, as wide as the grid's hour gutter.
fn day_heads(
    week: &[WeekDay],
    today: NaiveDate,
    gutter_size: &gtk::SizeGroup,
    tx: &Tx,
) -> gtk::Box {
    let heads = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    heads.add_css_class("week-heads");
    let pad = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    // The gutter's own class, for its margin too.
    pad.add_css_class("time-gutter");
    gutter_size.add_widget(&pad);
    heads.append(&pad);
    let cells = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cells.set_homogeneous(true);
    cells.set_hexpand(true);
    for d in week {
        let day = d.plan.day;
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
        cell.add_css_class("week-day");
        let face = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let weekday = gtk::Label::new(Some(&day.format("%a").to_string()));
        weekday.add_css_class("week-day-name");
        let date = gtk::Label::new(Some(&day.format("%-d").to_string()));
        date.add_css_class("week-day-date");
        face.append(&weekday);
        face.append(&date);
        let button = gtk::Button::builder().child(&face).build();
        button.add_css_class("flat");
        button.add_css_class("week-day-head");
        if day == today {
            button.add_css_class("today");
        }
        button.set_focus_on_click(false);
        button.set_tooltip_text(Some(&format!("Open {} in Day", day.format("%A, %B %-d"))));
        {
            let tx = tx.clone();
            button.connect_clicked(move |_| {
                let _ = tx.send(Msg::DayOpen(day));
            });
        }
        cell.append(&button);
        if let Some(strip) = events::all_day_strip(&d.events) {
            cell.append(&strip);
        }
        cells.append(&cell);
    }
    heads.append(&cells);
    heads
}

/// A day's planned tasks, each in its column beside the day's events. A
/// click opens the task in place; moving and resizing stay in Day.
fn fill_blocks(
    p: &DayPlan,
    lane: &gtk::Overlay,
    columns: &daygrid::Columns,
    reflow: &daygrid::Reflow,
    tx: &Tx,
) {
    for b in &p.blocks {
        let (chip, meta) = plan::block_chip(b, p.now_min, reflow.px());
        let click = gtk::GestureClick::new();
        click.set_button(1);
        {
            let (href, tx) = (b.task.href.clone(), tx.clone());
            click.connect_released(move |_, _, _, _| tx.emit(Msg::Open(href.clone())));
        }
        chip.add_controller(click);
        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.set_halign(gtk::Align::Fill);
        wrap.set_valign(gtk::Align::Start);
        wrap.set_hexpand(true);
        wrap.append(&plan::block_row(&chip));
        lane.add_overlay(&wrap);
        columns.set(&wrap, b.col, b.cols);
        let span = daygrid::Span {
            start_min: f64::from(b.start),
            end_min: f64::from(b.start + b.duration),
            min_h: f64::from(plan::min_block_h(b)),
            title_at: 0.0,
            meta_at: 36.0,
        };
        reflow.place(&wrap, &chip, span, None::<&gtk::Label>, meta.as_ref());
    }
}
