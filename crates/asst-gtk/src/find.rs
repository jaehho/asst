//! Quick Find: jump to a view or a list, or find a task by its words,
//! completed ones included. Views answer to other words too (`upcoming`,
//! `no date`), `p1` to `p4` list tasks by priority, and what matched shows
//! in bold, with the line of notes it was found in.

use std::cell::Cell;
use std::rc::Rc;

use asst_core::api::{ListView, TaskView, query};
use asst_core::fmt;
use asst_core::store::{Query, View};
use asst_core::task::priority_level;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::model::{self, Nav, capitalize};
use crate::pickers;
use crate::prefs::Prefs;
use crate::ui;
use crate::window::{Msg, Tx};

enum Hit {
    Nav(Nav),
    Task(String),
}

/// `text` as markup, with each place `q` (lowercase) occurs in bold.
fn bold(text: &str, q: &str) -> String {
    let lower = text.to_lowercase();
    // Lowercasing changed where characters are: no bold rather than wrong bold.
    if q.is_empty() || lower.len() != text.len() {
        return glib::markup_escape_text(text).to_string();
    }
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find(q).map(|i| i + at) {
        out.push_str(&glib::markup_escape_text(&text[at..i]));
        out.push_str("<b>");
        out.push_str(&glib::markup_escape_text(&text[i..i + q.len()]));
        out.push_str("</b>");
        at = i + q.len();
    }
    out.push_str(&glib::markup_escape_text(&text[at..]));
    out
}

/// The line of `notes` where `q` first occurs, cut to a few words either
/// side of it.
fn excerpt(notes: &str, q: &str) -> Option<String> {
    let line = notes.lines().find(|l| l.to_lowercase().contains(q))?.trim();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= 70 {
        return Some(line.to_string());
    }
    let hit = line
        .to_lowercase()
        .find(q)
        .and_then(|b| line.get(..b))
        .map_or(0, |before| before.chars().count());
    let start = hit.saturating_sub(25);
    let end = (start + 70).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out.insert(0, '…');
    }
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

/// `p1` … `p4`, or a priority's name.
fn priority_query(q: &str) -> Option<u8> {
    match q {
        "p1" | "high" => Some(1),
        "p2" | "medium" => Some(2),
        "p3" | "low" => Some(3),
        "p4" => Some(4),
        _ => None,
    }
}

fn header(text: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .build();
    let l = ui::label(text, &["caption-heading", "dim-label"]);
    l.set_margin_top(9);
    l.set_margin_start(9);
    l.set_margin_bottom(3);
    row.set_child(Some(&l));
    row
}

/// A result: `title` is markup; under it a plain subtitle and a line of
/// notes (markup).
fn result_row(
    icon: gtk::Widget,
    title: &str,
    subtitle: Option<&str>,
    notes: Option<&str>,
) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.set_margin_top(6);
    line.set_margin_bottom(6);
    line.set_margin_start(9);
    line.set_margin_end(9);
    line.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);
    let t = gtk::Label::builder()
        .label(title)
        .use_markup(true)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    text.append(&t);
    if let Some(s) = subtitle {
        text.append(&ui::label(s, &["caption", "dim-label"]));
    }
    if let Some(n) = notes {
        let l = ui::label("", &["caption", "dim-label", "find-excerpt"]);
        l.set_markup(n);
        text.append(&l);
    }
    line.append(&text);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&line));
    row
}

/// A task as a result: its list and date under it, and the notes line that
/// matched when the title didn't.
fn task_row(t: &TaskView, q: &str, sub: &str) -> gtk::ListBoxRow {
    let in_title = t.task.summary.to_lowercase().contains(q);
    let notes = (!in_title && !q.is_empty())
        .then(|| t.task.description.as_deref().and_then(|d| excerpt(d, q)))
        .flatten()
        .map(|e| bold(&e, q))
        .or_else(|| {
            // Or the name of a note it links to.
            let name = t
                .task
                .linked_notes
                .iter()
                .map(|n| asst_core::note_files::name(n))
                .find(|n| !in_title && !q.is_empty() && n.to_lowercase().contains(q))?;
            Some(format!("Linked note: {}", bold(name, q)))
        });
    let check = if t.task.is_open() {
        "check-round-outline-symbolic"
    } else {
        "check-round-outline-whole-symbolic"
    };
    let icon = ui::icon(check, 16);
    icon.add_css_class(&format!(
        "priority-{}-icon",
        priority_level(t.task.priority)
    ));
    result_row(
        icon.upcast(),
        &bold(&t.task.summary, q),
        Some(sub),
        notes.as_deref(),
    )
}

pub fn open(
    parent: &impl IsA<gtk::Widget>,
    lists: &[ListView],
    open: &[TaskView],
    prefs: &Prefs,
    text: &str,
    tx: Tx,
) {
    let dialog = adw::Dialog::builder()
        .content_width(560)
        .content_height(480)
        .title("Quick Find")
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Find views, lists and tasks")
        .hexpand(true)
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.set_margin_top(12);
    top.set_margin_bottom(6);
    top.set_margin_start(12);
    top.set_margin_end(12);
    top.append(&search);

    let results = gtk::ListBox::new();
    results.add_css_class("quick-find-list");
    results.set_selection_mode(gtk::SelectionMode::Browse);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&results)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&top);
    view.set_content(Some(&scroller));
    dialog.set_child(Some(&view));

    let hits: Rc<std::cell::RefCell<Vec<(gtk::ListBoxRow, Hit)>>> = Rc::default();
    let generation = Rc::new(Cell::new(0u64));
    let lists = lists.to_vec();
    let open = open.to_vec();
    let archived = prefs.archived.clone();
    // The sidebar's views first; typing finds the hidden ones too.
    let shown = prefs.sidebar_views();
    let mut navs = shown.clone();
    navs.extend(Nav::filters().into_iter().filter(|n| !shown.contains(n)));

    let fill = {
        let results = results.clone();
        let hits = hits.clone();
        let generation = generation.clone();
        Rc::new(move |q: String| {
            generation.set(generation.get() + 1);
            let this = generation.get();
            results.remove_all();
            hits.borrow_mut().clear();
            let q = q.trim().to_lowercase();
            let now = pickers::now();
            let matches = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);

            let add = |row: gtk::ListBoxRow, hit: Hit| {
                results.append(&row);
                hits.borrow_mut().push((row, hit));
            };
            // Other words for a view count from two letters: "no" for "no date".
            let known_as = |n: &Nav| {
                q.chars().count() >= 2 && n.keywords().iter().any(|k| k.starts_with(q.as_str()))
            };
            let views: Vec<&Nav> = navs
                .iter()
                .filter(|n| {
                    (matches(&n.title(&[])) || known_as(n)) && (!q.is_empty() || shown.contains(n))
                })
                .collect();
            if !views.is_empty() {
                results.append(&header("Views"));
                for n in views {
                    let icon = ui::icon(n.icon(), 16);
                    icon.add_css_class(n.tint());
                    let title = bold(&n.title(&[]), &q);
                    let also = (!n.title(&[]).to_lowercase().contains(&q))
                        .then(|| n.keywords().iter().find(|k| k.starts_with(q.as_str())))
                        .flatten()
                        .map(|k| format!("“{k}”"));
                    add(
                        result_row(icon.upcast(), &title, also.as_deref(), None),
                        Hit::Nav(n.clone()),
                    );
                }
            }
            let found_lists: Vec<&ListView> = lists.iter().filter(|l| matches(&l.name)).collect();
            if !found_lists.is_empty() {
                results.append(&header("Lists"));
                for l in found_lists {
                    let sub = archived.contains(&l.href).then_some("Archived");
                    add(
                        result_row(
                            ui::ring(l.color.as_deref(), 16).upcast(),
                            &bold(&l.name, &q),
                            sub,
                            None,
                        ),
                        Hit::Nav(Nav::List(l.href.clone())),
                    );
                }
            }
            if q.is_empty() {
                select_first(&results);
                return;
            }
            let where_ = |t: &TaskView| match &t.task.due {
                Some(d) => format!("{} · {}", t.list_name, capitalize(&fmt::due_label(d, now))),
                None => t.list_name.clone(),
            };
            if let Some(level) = priority_query(&q) {
                let mut tasks: Vec<TaskView> = open
                    .iter()
                    .filter(|t| priority_level(t.task.priority) == level)
                    .cloned()
                    .collect();
                tasks = model::sorted(tasks, model::Sort::Due, pickers::zone());
                if !tasks.is_empty() {
                    results.append(&header(model::priority_name(level)));
                    for t in tasks.iter().take(40) {
                        add(task_row(t, "", &where_(t)), Hit::Task(t.href.clone()));
                    }
                }
            }
            let tasks: Vec<&TaskView> = open
                .iter()
                .filter(|t| {
                    matches(&t.task.summary)
                        || t.task.description.as_deref().is_some_and(&matches)
                        || t.task
                            .linked_notes
                            .iter()
                            .any(|n| matches(asst_core::note_files::name(n)))
                })
                .take(40)
                .collect();
            if !tasks.is_empty() {
                results.append(&header("Tasks"));
                for t in tasks {
                    add(task_row(t, &q, &where_(t)), Hit::Task(t.href.clone()));
                }
            }
            select_first(&results);
            if q.chars().count() < 2 {
                return;
            }
            // Completed tasks are the daemon's to search.
            let (results, hits, generation) = (results.clone(), hits.clone(), generation.clone());
            let text = q.clone();
            let q = q.clone();
            crate::client::call(
                async move {
                    let q = Query {
                        text: Some(text),
                        limit: Some(20),
                        ..query(View::Completed)
                    };
                    crate::client::tasks(&q).await
                },
                move |r| {
                    if generation.get() != this {
                        return;
                    }
                    let Ok(done) = r else { return };
                    if done.is_empty() {
                        return;
                    }
                    results.append(&header("Completed"));
                    for t in done {
                        let when = t
                            .task
                            .completed
                            .map(|c| {
                                capitalize(&fmt::day_label(
                                    c.with_timezone(&pickers::zone()).date_naive(),
                                    now.date_naive(),
                                ))
                            })
                            .unwrap_or_default();
                        let row = task_row(&t, &q, &format!("{} · done {when}", t.list_name));
                        results.append(&row);
                        hits.borrow_mut().push((row, Hit::Task(t.href.clone())));
                    }
                    select_first(&results);
                },
            );
        })
    };
    fill(String::new());
    {
        let fill = fill.clone();
        search.connect_search_changed(move |e| fill(e.text().to_string()));
    }
    let activate = {
        let hits = hits.clone();
        let dialog = dialog.clone();
        let tx = tx.clone();
        Rc::new(move |row: &gtk::ListBoxRow| {
            let hits = hits.borrow();
            let Some((_, hit)) = hits.iter().find(|(r, _)| r == row) else {
                return;
            };
            match hit {
                Hit::Nav(n) => tx.emit(Msg::Navigate(n.clone())),
                Hit::Task(h) => tx.emit(Msg::Reveal(h.clone())),
            }
            dialog.close();
        })
    };
    {
        let activate = activate.clone();
        results.connect_row_activated(move |_, row| activate(row));
    }
    {
        let results = results.clone();
        let activate = activate.clone();
        search.connect_activate(move |_| {
            if let Some(row) = results.selected_row() {
                activate(&row);
            }
        });
    }
    {
        // Arrows move through results while typing.
        let keys = gtk::EventControllerKey::new();
        let results = results.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                _ => return glib::Propagation::Proceed,
            };
            let mut i = results.selected_row().map_or(-1, |r| r.index());
            loop {
                i += step;
                match results.row_at_index(i) {
                    Some(r) if r.is_selectable() => {
                        results.select_row(Some(&r));
                        r.grab_focus();
                        search_focus_back(&r);
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
            glib::Propagation::Stop
        });
        search.add_controller(keys);
    }
    search.set_text(text);
    dialog.present(Some(parent));
    search.grab_focus();
}

fn select_first(results: &gtk::ListBox) {
    let mut i = 0;
    while let Some(r) = results.row_at_index(i) {
        if r.is_selectable() {
            results.select_row(Some(&r));
            return;
        }
        i += 1;
    }
}

/// Keep typing in the search field after moving the selection.
fn search_focus_back(row: &gtk::ListBoxRow) {
    if let Some(entry) = row
        .root()
        .and_then(|r| find_search(&r.upcast::<gtk::Widget>()))
    {
        entry.grab_focus();
    }
}

fn find_search(w: &gtk::Widget) -> Option<gtk::SearchEntry> {
    if let Some(s) = w.downcast_ref::<gtk::SearchEntry>() {
        return Some(s.clone());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(s) = find_search(&c) {
            return Some(s);
        }
        child = c.next_sibling();
    }
    None
}
