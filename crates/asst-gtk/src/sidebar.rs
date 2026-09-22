//! The sidebar, after Planify's: colored tiles for the views chosen in
//! Preferences, then the account's lists with their color rings and counts.
//! Tasks dragged onto a tile or a list land there; lists dragged among
//! themselves take a new order.

use std::cell::RefCell;
use std::rc::Rc;

use asst_core::api::{ListView, StatusView, SyncState};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::model::{self, Counts, Nav};
use crate::pickers;
use crate::prefs::Prefs;
use crate::row::DRAG_PREFIX;
use crate::ui::{self, Item};
use crate::window::{Msg, Tx};

struct Tile {
    nav: Nav,
    child: gtk::FlowBoxChild,
    count: gtk::Label,
    dot: gtk::Widget,
}

struct ListRow {
    href: String,
    row: gtk::ListBoxRow,
    count: gtk::Label,
}

/// What a dragged list carries.
const LIST_PREFIX: &str = "asst-list:";

/// A list's menu, from its row in the sidebar and from the view's ⋮.
pub fn list_menu(list: &ListView, archived: bool, tx: &Tx, select: bool) -> gtk::Popover {
    let mut items: Vec<gtk::Widget> = Vec::new();
    let href = list.href.clone();
    if select {
        let tx = tx.clone();
        items.push(
            Item::new(Some("list-large-symbolic"), "Select tasks")
                .secondary("v")
                .build(move || tx.emit(Msg::SelectMode(true)))
                .upcast(),
        );
        items.push(ui::separator());
    }
    let edit = {
        let (tx, h) = (tx.clone(), href.clone());
        Item::new(Some("edit-symbolic"), "Edit list…")
            .build(move || tx.emit(Msg::EditList(h.clone())))
    };
    edit.set_sensitive(list.writable);
    items.push(edit.upcast());
    {
        let (tx, h) = (tx.clone(), href.clone());
        items.push(
            Item::new(Some("clipboard-symbolic"), "Copy as Markdown")
                .build(move || tx.emit(Msg::CopyList(h.clone())))
                .upcast(),
        );
    }
    {
        let (tx, h) = (tx.clone(), href.clone());
        let title = if archived { "Unarchive" } else { "Archive" };
        items.push(
            Item::new(Some("shoe-box-symbolic"), title)
                .build(move || tx.emit(Msg::Archive(h.clone(), !archived)))
                .upcast(),
        );
    }
    items.push(ui::separator());
    let clear = {
        let (tx, h) = (tx.clone(), href.clone());
        Item::new(
            Some("check-round-outline-whole-symbolic"),
            "Delete completed tasks…",
        )
        .secondary(&list.done.to_string())
        .build(move || tx.emit(Msg::AskDeleteCompleted(Some(h.clone()))))
    };
    clear.set_sensitive(list.writable && list.done > 0);
    items.push(clear.upcast());
    let delete = {
        let tx = tx.clone();
        Item::new(Some("user-trash-symbolic"), "Delete list…")
            .danger()
            .build(move || tx.emit(Msg::AskDeleteList(href.clone())))
    };
    delete.set_sensitive(list.writable);
    items.push(delete.upcast());
    ui::menu(&items)
}

pub struct Sidebar {
    pub root: adw::ToolbarView,
    tx: Tx,
    tiles_box: gtk::FlowBox,
    tiles: RefCell<Vec<Tile>>,
    tiles_key: RefCell<String>,
    lists_box: gtk::ListBox,
    rows: RefCell<Vec<ListRow>>,
    lists_key: RefCell<String>,
    account: gtk::Label,
    account_sub: gtk::Label,
    add_list: gtk::Button,
    sync: gtk::Button,
}

pub struct State<'a> {
    pub nav: &'a Nav,
    pub lists: &'a [ListView],
    pub counts: &'a Counts,
    pub status: Option<&'a StatusView>,
    pub prefs: &'a Prefs,
}

/// A drop of a task onto `widget` sends `DropTask` with `nav`.
fn drop_target(widget: &impl IsA<gtk::Widget>, nav: Nav, tx: &Tx) {
    let target = gtk::DropTarget::new(
        glib::Type::STRING,
        gdk::DragAction::MOVE | gdk::DragAction::COPY,
    );
    let tx = tx.clone();
    target.connect_accept(|_, drop| {
        drop.formats().contain_mime_type("text/plain;charset=utf-8")
            || drop.formats().contains_type(glib::Type::STRING)
    });
    target.connect_drop(move |_, value, _, _| {
        let Ok(text) = value.get::<String>() else {
            return false;
        };
        let Some(href) = text.strip_prefix(DRAG_PREFIX) else {
            return false;
        };
        tx.emit(Msg::DropTask(href.to_string(), nav.clone()));
        true
    });
    widget.add_controller(target);
}

/// The sync button's tooltip, worked out when it shows so the time is right.
fn sync_tooltip(status: Option<&StatusView>) -> String {
    let Some(s) = status else {
        return "Sync now".into();
    };
    let keys = "<span size=\"small\" alpha=\"70%\">s</span>";
    match s.state {
        SyncState::Offline => format!(
            "<b>Can't reach the server</b>\nChanges made now are sent when it's back.{}",
            match s.pending {
                0 => String::new(),
                1 => "\n1 change waiting to be sent".into(),
                n => format!("\n{n} changes waiting to be sent"),
            }
        ),
        _ => format!(
            "{}\n{keys}",
            glib::markup_escape_text(&model::sync_state(s, pickers::now()))
        ),
    }
}

impl Sidebar {
    pub fn new(tx: Tx, status: Rc<RefCell<Option<StatusView>>>) -> Rc<Sidebar> {
        let header = adw::HeaderBar::builder().show_title(false).build();
        header.add_css_class("flat");
        let search = gtk::Button::from_icon_name("edit-find-symbolic");
        search.add_css_class("flat");
        ui::tip(&search, "Quick Find", "Ctrl+F or /");
        {
            let tx = tx.clone();
            search.connect_clicked(move |_| tx.emit(Msg::QuickFind));
        }
        header.pack_start(&search);

        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main menu")
            .build();
        menu.add_css_class("flat");
        {
            let tx = tx.clone();
            menu.set_create_popup_func(move |mb| {
                let (a, b, c) = (tx.clone(), tx.clone(), tx.clone());
                mb.set_popover(Some(&ui::menu(&[
                    Item::new(Some("settings-symbolic"), "Preferences")
                        .secondary("Ctrl+,")
                        .build(move || a.emit(Msg::Preferences(None)))
                        .upcast(),
                    Item::new(None, "Keyboard Shortcuts")
                        .secondary("?")
                        .build(move || b.emit(Msg::Shortcuts))
                        .upcast(),
                    Item::new(None, "About asst")
                        .build(move || c.emit(Msg::About))
                        .upcast(),
                ])));
            });
        }
        header.pack_end(&menu);
        let sync = gtk::Button::from_icon_name("update-symbolic");
        sync.add_css_class("flat");
        sync.add_css_class("sync-button");
        {
            let tx = tx.clone();
            sync.connect_clicked(move |_| tx.emit(Msg::SyncNow));
        }
        sync.set_has_tooltip(true);
        sync.connect_query_tooltip(move |_, _, _, _, tip| {
            tip.set_markup(Some(&sync_tooltip(status.borrow().as_ref())));
            true
        });
        header.pack_end(&sync);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("sidebar-content");

        let tiles_box = gtk::FlowBox::builder()
            .homogeneous(true)
            .row_spacing(9)
            .column_spacing(9)
            .min_children_per_line(2)
            .max_children_per_line(2)
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .build();
        tiles_box.add_css_class("filter-tiles");
        content.append(&tiles_box);

        let source = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        source.add_css_class("source-header");
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        names.set_hexpand(true);
        let account = ui::label("Lists", &["heading"]);
        let account_sub = ui::label("", &["caption", "dim-label"]);
        names.append(&account);
        names.append(&account_sub);
        source.append(&names);
        let add_list = gtk::Button::from_icon_name("plus-large-symbolic");
        add_list.add_css_class("flat");
        add_list.set_valign(gtk::Align::Center);
        ui::tip(&add_list, "New list", "p");
        {
            let tx = tx.clone();
            add_list.connect_clicked(move |_| tx.emit(Msg::NewList));
        }
        source.append(&add_list);
        content.append(&source);
        let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
        rule.add_css_class("source-rule");
        content.append(&rule);

        let lists_box = gtk::ListBox::new();
        lists_box.add_css_class("sidebar-lists");
        lists_box.set_selection_mode(gtk::SelectionMode::None);
        content.append(&lists_box);

        let scroller = gtk::ScrolledWindow::builder()
            .child(&content)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let root = adw::ToolbarView::new();
        root.add_css_class("app-sidebar");
        root.add_top_bar(&header);
        root.set_content(Some(&scroller));

        let s = Rc::new(Sidebar {
            root,
            tx,
            tiles_box,
            tiles: RefCell::default(),
            tiles_key: RefCell::default(),
            lists_box,
            rows: RefCell::default(),
            lists_key: RefCell::default(),
            account,
            account_sub,
            add_list,
            sync,
        });
        {
            let weak = Rc::downgrade(&s);
            s.tiles_box.connect_child_activated(move |_, child| {
                if let Some(s) = weak.upgrade() {
                    let nav = s
                        .tiles
                        .borrow()
                        .iter()
                        .find(|t| &t.child == child)
                        .map(|t| t.nav.clone());
                    if let Some(nav) = nav {
                        s.tx.emit(Msg::Navigate(nav));
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(&s);
            s.lists_box.connect_row_activated(move |_, row| {
                if let Some(s) = weak.upgrade() {
                    let href = s
                        .rows
                        .borrow()
                        .iter()
                        .find(|r| &r.row == row)
                        .map(|r| r.href.clone());
                    if let Some(href) = href {
                        s.tx.emit(Msg::Navigate(Nav::List(href)));
                    }
                }
            });
        }
        s
    }

    pub fn update(&self, st: &State) {
        let shown = st.prefs.sidebar_views();
        let key = format!("{shown:?}");
        if *self.tiles_key.borrow() != key {
            *self.tiles_key.borrow_mut() = key;
            self.build_tiles(&shown);
            self.tiles_box.set_visible(!shown.is_empty());
        }
        for t in self.tiles.borrow().iter() {
            let n = st
                .counts
                .of(&t.nav)
                .filter(|n| *n > 0 && st.prefs.show_counts);
            t.count
                .set_text(&n.map(|n| n.to_string()).unwrap_or_default());
            t.dot
                .set_visible(t.nav == Nav::Today && st.counts.overdue > 0);
            if &t.nav == st.nav {
                t.child.add_css_class("selected");
            } else {
                t.child.remove_css_class("selected");
            }
        }

        let signed_in = st.status.is_some_and(|s| s.state != SyncState::NoAccount);
        let lists_key = format!(
            "{:?}{}",
            st.lists
                .iter()
                .map(|l| (&l.href, &l.name, &l.color, l.writable))
                .collect::<Vec<_>>(),
            st.prefs.lists_by_name
        );
        if *self.lists_key.borrow() != lists_key {
            *self.lists_key.borrow_mut() = lists_key;
            self.build_lists(st.lists, !st.prefs.lists_by_name);
        }
        for r in self.rows.borrow().iter() {
            let n = st.counts.lists.get(&r.href).copied().unwrap_or(0);
            r.count.set_text(&if n > 0 && st.prefs.show_counts {
                n.to_string()
            } else {
                String::new()
            });
            if *st.nav == Nav::List(r.href.clone()) {
                r.row.add_css_class("selected");
            } else {
                r.row.remove_css_class("selected");
            }
        }

        match st.status {
            Some(s) if signed_in => {
                self.account.set_text("Nextcloud");
                let host = s
                    .server
                    .as_deref()
                    .map(|h| {
                        h.trim_start_matches("https://")
                            .trim_start_matches("http://")
                    })
                    .unwrap_or_default();
                let who = match (&s.username, host) {
                    (Some(u), h) if !h.is_empty() => format!("{u}@{h}"),
                    (Some(u), _) => u.clone(),
                    (None, h) => h.to_string(),
                };
                self.account_sub.set_text(&who);
                self.account_sub.set_visible(true);
            }
            _ => {
                self.account.set_text("Lists");
                self.account_sub.set_text("Not signed in");
            }
        }
        self.add_list.set_sensitive(signed_in);

        let Some(s) = st.status else { return };
        self.sync.remove_css_class("spinning");
        self.sync.remove_css_class("warning");
        match s.state {
            SyncState::Idle => self.sync.set_icon_name("update-symbolic"),
            SyncState::Syncing => {
                self.sync.set_icon_name("update-symbolic");
                self.sync.add_css_class("spinning");
            }
            SyncState::Offline => {
                self.sync.set_icon_name("network-offline-symbolic");
                self.sync.add_css_class("warning");
            }
            SyncState::Error | SyncState::NoAccount => {
                self.sync.set_icon_name("dialog-warning-symbolic");
                self.sync.add_css_class("warning");
            }
        }
    }

    fn build_tiles(&self, shown: &[Nav]) {
        self.tiles_box.remove_all();
        let mut tiles = Vec::new();
        for nav in shown {
            let grid = gtk::Grid::builder()
                .column_spacing(6)
                .row_spacing(6)
                .margin_start(3)
                .margin_end(3)
                .margin_top(3)
                .margin_bottom(3)
                .build();
            let icon = gtk::Image::from_icon_name(nav.icon());
            icon.set_halign(gtk::Align::Start);
            grid.attach(&icon, 0, 0, 1, 1);
            let count = gtk::Label::builder()
                .hexpand(true)
                .halign(gtk::Align::End)
                .build();
            count.add_css_class("tile-count");
            grid.attach(&count, 1, 0, 1, 1);
            let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let title = gtk::Label::builder()
                .label(nav.title(&[]))
                .ellipsize(pango::EllipsizeMode::End)
                .xalign(0.0)
                .build();
            title.add_css_class("tile-title");
            title_box.append(&title);
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("overdue-dot");
            dot.set_hexpand(true);
            dot.set_halign(gtk::Align::End);
            dot.set_valign(gtk::Align::Center);
            dot.set_visible(false);
            title_box.append(&dot);
            grid.attach(&title_box, 0, 1, 2, 1);

            let child = gtk::FlowBoxChild::new();
            child.set_child(Some(&grid));
            child.add_css_class("filter-tile");
            child.add_css_class(nav.tint());
            let keys = match nav {
                Nav::Inbox => Some("Ctrl+I"),
                Nav::Today => Some("Ctrl+T"),
                Nav::Scheduled => Some("Ctrl+U"),
                _ => None,
            };
            if let Some(k) = keys {
                ui::tip(&child, &nav.title(&[]), k);
            }
            if !matches!(
                nav,
                Nav::Scheduled | Nav::Anytime | Nav::Repeating | Nav::All
            ) {
                drop_target(&child, nav.clone(), &self.tx);
            }
            let menu_click = gtk::GestureClick::builder()
                .button(gdk::BUTTON_SECONDARY)
                .build();
            {
                let tx = self.tx.clone();
                let nav = nav.clone();
                let child_weak = child.downgrade();
                menu_click.connect_pressed(move |g, _, x, y| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let Some(child) = child_weak.upgrade() else {
                        return;
                    };
                    let (a, b, nav) = (tx.clone(), tx.clone(), nav.clone());
                    let menu = ui::menu(&[
                        Item::new(
                            Some("cross-large-circle-outline-symbolic"),
                            "Hide from sidebar",
                        )
                        .build(move || a.emit(Msg::HideView(nav.clone())))
                        .upcast(),
                        Item::new(Some("dock-left-symbolic"), "Edit sidebar…")
                            .build(move || b.emit(Msg::Preferences(Some("sidebar"))))
                            .upcast(),
                    ]);
                    crate::row::show_at(
                        &menu,
                        &child,
                        gdk::Rectangle::new(x as i32, y as i32, 1, 1),
                    );
                });
            }
            child.add_controller(menu_click);
            self.tiles_box.append(&child);
            tiles.push(Tile {
                nav: nav.clone(),
                child,
                count,
                dot: dot.upcast(),
            });
        }
        *self.tiles.borrow_mut() = tiles;
    }

    /// `movable`: lists can be dragged into a new order (not sorted by name).
    fn build_lists(&self, lists: &[ListView], movable: bool) {
        self.lists_box.remove_all();
        let mut rows = Vec::new();
        let order: Rc<Vec<String>> = Rc::new(lists.iter().map(|l| l.href.clone()).collect());
        for l in lists {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            line.add_css_class("list-line");
            line.append(&ui::ring(l.color.as_deref(), 18));
            let name = ui::label(&l.name, &[]);
            name.set_hexpand(true);
            line.append(&name);
            if !l.writable {
                let lock = ui::icon("permissions-generic-symbolic", 12);
                lock.add_css_class("dim-label");
                lock.set_tooltip_text(Some("Read-only"));
                line.append(&lock);
            }
            let count = ui::label("", &["caption", "dim-label"]);
            line.append(&count);
            let row = gtk::ListBoxRow::new();
            row.add_css_class("list-row");
            row.set_child(Some(&line));
            if movable {
                let drag = gtk::DragSource::new();
                drag.set_actions(gdk::DragAction::MOVE);
                let payload = format!("{LIST_PREFIX}{}", l.href);
                drag.connect_prepare(move |_, _, _| {
                    Some(gdk::ContentProvider::for_value(&payload.to_value()))
                });
                drag.connect_drag_begin(|src, _| {
                    if let Some(w) = src.widget() {
                        src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&w))), 12, 12);
                    }
                });
                row.add_controller(drag);
            }
            self.list_drops(&row, l, order.clone());
            let menu_click = gtk::GestureClick::builder()
                .button(gdk::BUTTON_SECONDARY)
                .build();
            {
                let tx = self.tx.clone();
                let list = l.clone();
                let row_weak = row.downgrade();
                menu_click.connect_pressed(move |g, _, x, y| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let Some(row) = row_weak.upgrade() else {
                        return;
                    };
                    let menu = list_menu(&list, false, &tx, false);
                    crate::row::show_at(&menu, &row, gdk::Rectangle::new(x as i32, y as i32, 1, 1));
                });
            }
            row.add_controller(menu_click);
            self.lists_box.append(&row);
            rows.push(ListRow {
                href: l.href.clone(),
                row,
                count,
            });
        }
        *self.rows.borrow_mut() = rows;
    }

    /// A list's row takes a task (it moves there) or another list (which
    /// goes above or below it, by the half it's dropped on).
    fn list_drops(&self, row: &gtk::ListBoxRow, list: &ListView, order: Rc<Vec<String>>) {
        let target = gtk::DropTarget::new(
            glib::Type::STRING,
            gdk::DragAction::MOVE | gdk::DragAction::COPY,
        );
        target.set_preload(true);
        let dragged = |t: &gtk::DropTarget| {
            t.value()
                .and_then(|v| v.get::<String>().ok())
                .unwrap_or_default()
        };
        let lower_half = |t: &gtk::DropTarget, y: f64| {
            t.widget().is_some_and(|w| y > f64::from(w.height()) / 2.0)
        };
        let unmark = |t: &gtk::DropTarget| {
            if let Some(w) = t.widget() {
                w.remove_css_class("drop-above");
                w.remove_css_class("drop-below");
            }
        };
        let writable = list.writable;
        target.connect_motion(move |t, _, y| {
            let payload = dragged(t);
            unmark(t);
            if payload.starts_with(LIST_PREFIX) {
                if let Some(w) = t.widget() {
                    w.add_css_class(if lower_half(t, y) {
                        "drop-below"
                    } else {
                        "drop-above"
                    });
                }
                gdk::DragAction::MOVE
            } else if payload.starts_with(DRAG_PREFIX) && writable {
                gdk::DragAction::MOVE
            } else {
                gdk::DragAction::empty()
            }
        });
        target.connect_leave(move |t| unmark(t));
        let (tx, href) = (self.tx.clone(), list.href.clone());
        target.connect_drop(move |t, value, _, y| {
            unmark(t);
            let Ok(text) = value.get::<String>() else {
                return false;
            };
            if let Some(moved) = text.strip_prefix(LIST_PREFIX) {
                if moved == href {
                    return false;
                }
                let mut new: Vec<String> = order.iter().filter(|h| *h != moved).cloned().collect();
                let at = new.iter().position(|h| *h == href).unwrap_or(new.len());
                new.insert(at + usize::from(lower_half(t, y)), moved.to_string());
                tx.emit(Msg::ReorderLists(new));
                return true;
            }
            match text.strip_prefix(DRAG_PREFIX) {
                Some(task) if writable => {
                    tx.emit(Msg::DropTask(task.to_string(), Nav::List(href.clone())));
                    true
                }
                _ => false,
            }
        });
        row.add_controller(target);
    }
}
