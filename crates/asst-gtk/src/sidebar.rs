//! The sidebar: colored tiles for the views chosen here (a tile's menu hides
//! it and adds the hidden ones back), then the account's lists with their
//! color rings and counts. The tiles and the Lists heading stay put and only
//! the lists scroll; the heading's `...` menu has the views as a checklist
//! and what the lists do. Tasks dragged onto a tile or a list land there;
//! lists dragged among themselves take a new order. The inbox list is the
//! Inbox tile's, and has a row of its own only while that tile is hidden.
//! Archived lists, and finished ones if asked, sit under one `Hidden lists`
//! row at the foot of the lists.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use asst_core::api::{LinkView, ListView, StatusView, SyncState};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

use crate::model::{self, Counts, Nav};
use crate::pickers;
use crate::prefs::{PrefPatch, Prefs, Why};
use crate::row::DRAG_PREFIX;
use crate::ui::{self, Item};
use crate::window::{Msg, Tx};

struct Tile {
    nav: Nav,
    child: gtk::FlowBoxChild,
    count: gtk::Label,
    dot: gtk::Widget,
    /// The views not in the sidebar, for the menu's "Add to sidebar". The
    /// tiles are built again whenever these change.
    hidden: Rc<Vec<Nav>>,
}

struct ListRow {
    href: String,
    /// The list as the row was built, for its menu.
    list: ListView,
    row: gtk::ListBoxRow,
    /// The task count, or for a hidden list the words saying why it is.
    count: gtk::Label,
    /// Shown while the list is tied to a GitHub repo.
    link: gtk::Image,
    /// Set for a row under Hidden lists.
    hidden: Option<Why>,
}

/// A place the keyboard stops in the sidebar.
struct Stop {
    /// `tile:<view>`, `list:<href>` or `hidden` (the Hidden lists row).
    id: String,
    widget: gtk::Widget,
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
    if list.writable && !list.inbox {
        let (tx, h) = (tx.clone(), href.clone());
        items.push(
            Item::new(Some(Nav::Inbox.icon()), "Use as Inbox")
                .build(move || tx.emit(Msg::UseAsInbox(h.clone())))
                .upcast(),
        );
    }
    {
        let (tx, h) = (tx.clone(), href.clone());
        let title = if archived { "Unarchive" } else { "Archive" };
        let archive = Item::new(Some("shoe-box-symbolic"), title)
            .build(move || tx.emit(Msg::Archive(h.clone(), !archived)));
        keep_inbox(&archive, list);
        items.push(archive.upcast());
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
    keep_inbox(&delete, list);
    items.push(delete.upcast());
    ui::menu(&items)
}

/// The inbox list can be neither archived nor deleted from the window: it
/// is where new tasks go. Another list takes over with "Use as Inbox".
fn keep_inbox(item: &gtk::Button, list: &ListView) {
    if list.inbox {
        item.set_sensitive(false);
        item.set_tooltip_text(Some("New tasks go here"));
    }
}

/// One row for each hidden view: a click appends it to the sidebar.
pub fn add_view_items(hidden: &[Nav], tx: &Tx) -> Vec<gtk::Widget> {
    hidden
        .iter()
        .map(|nav| {
            let (tx, nav) = (tx.clone(), nav.clone());
            Item::new(Some(nav.icon()), &nav.title(&[]))
                .tint(nav.tint())
                .build(move || tx.emit(Msg::ShowView(nav.clone(), None)))
                .upcast()
        })
        .collect()
}

/// A tile's menu: hide it, add a hidden view, start over.
fn tile_menu(nav: &Nav, hidden: &[Nav], tx: &Tx) -> gtk::Popover {
    let mut items: Vec<gtk::Widget> = Vec::new();
    {
        let (tx, nav) = (tx.clone(), nav.clone());
        items.push(
            Item::new(
                Some("cross-large-circle-outline-symbolic"),
                &format!("Hide {}", nav.title(&[])),
            )
            .secondary("Del")
            .build(move || tx.emit(Msg::HideView(nav.clone())))
            .upcast(),
        );
    }
    if !hidden.is_empty() {
        items.push(ui::separator());
        items.push(ui::label("Add to sidebar", &["caption-heading", "menu-heading"]).upcast());
        items.extend(add_view_items(hidden, tx));
    }
    items.push(ui::separator());
    {
        let tx = tx.clone();
        items.push(
            Item::new(Some("dock-left-symbolic"), "Reset sidebar")
                .secondary("Inbox, Day")
                .build(move || tx.emit(Msg::ResetSidebar))
                .upcast(),
        );
    }
    ui::menu(&items)
}

fn show_tile_menu(
    tile: &gtk::FlowBoxChild,
    nav: &Nav,
    hidden: &[Nav],
    tx: &Tx,
    at: gdk::Rectangle,
) {
    crate::row::show_at(&tile_menu(nav, hidden, tx), tile, at);
}

fn show_list_menu(
    row: &gtk::ListBoxRow,
    list: &ListView,
    archived: bool,
    tx: &Tx,
    at: gdk::Rectangle,
) {
    crate::row::show_at(&list_menu(list, archived, tx, false), row, at);
}

/// Where a menu opened from the keyboard points: the middle of the tile or
/// row.
fn middle(widget: &impl IsA<gtk::Widget>) -> gdk::Rectangle {
    gdk::Rectangle::new(widget.width() / 2, widget.height() / 2, 1, 1)
}

/// What a key does on a focused tile.
#[derive(Debug, PartialEq)]
enum TileKey {
    /// Alt+Left, Alt+Right: a place earlier or later.
    Move(i32),
    Hide,
    Menu,
}

/// The modifiers that count in a sidebar key. Caps Lock does not.
fn key_mods(state: gdk::ModifierType) -> gdk::ModifierType {
    state
        & (gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK)
}

/// Menu, or Shift+F10: the keys that open a context menu.
fn menu_key(key: gdk::Key, mods: gdk::ModifierType) -> bool {
    match key {
        gdk::Key::Menu => mods.is_empty(),
        gdk::Key::F10 => mods == gdk::ModifierType::SHIFT_MASK,
        _ => false,
    }
}

fn tile_key(key: gdk::Key, state: gdk::ModifierType) -> Option<TileKey> {
    let mods = key_mods(state);
    match key {
        gdk::Key::Left if mods == gdk::ModifierType::ALT_MASK => Some(TileKey::Move(-1)),
        gdk::Key::Right if mods == gdk::ModifierType::ALT_MASK => Some(TileKey::Move(1)),
        gdk::Key::Delete if mods.is_empty() => Some(TileKey::Hide),
        _ if menu_key(key, mods) => Some(TileKey::Menu),
        _ => None,
    }
}

/// What a key does on a focused list row. There is no Delete: a list goes
/// only through its menu.
#[derive(Debug, PartialEq)]
enum RowKey {
    /// Alt+Up, Alt+Down: a place earlier or later.
    Move(i32),
    Menu,
}

fn row_key(key: gdk::Key, state: gdk::ModifierType) -> Option<RowKey> {
    let mods = key_mods(state);
    match key {
        gdk::Key::Up if mods == gdk::ModifierType::ALT_MASK => Some(RowKey::Move(-1)),
        gdk::Key::Down if mods == gdk::ModifierType::ALT_MASK => Some(RowKey::Move(1)),
        _ if menu_key(key, mods) => Some(RowKey::Menu),
        _ => None,
    }
}

/// `order` with `href` `by` places on (negative: earlier), or `None` when
/// that is past an end or `href` is not in it.
fn moved(order: &[String], href: &str, by: i32) -> Option<Vec<String>> {
    let at = order.iter().position(|h| h == href)?;
    let to = at
        .checked_add_signed(by as isize)
        .filter(|to| *to < order.len())?;
    let mut new = order.to_vec();
    let h = new.remove(at);
    new.insert(to, h);
    Some(new)
}

/// Where a key sends the keyboard among the sidebar's stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Next,
    Prev,
    First,
    Last,
}

/// The stop of `len` that `to` reaches from stop `at`. The ends hold: `j`
/// on the last stop stays there, as it does in the task column.
fn reach_index(len: usize, at: usize, to: Reach) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match to {
        Reach::First => 0,
        Reach::Last => last,
        Reach::Next => (at + 1).min(last),
        Reach::Prev => at.saturating_sub(1),
    })
}

/// One line of the options menu's Views list.
#[derive(Debug, PartialEq)]
struct ViewChoice {
    nav: Nav,
    shown: bool,
    /// The key that opens it: `Alt+N` for the Nth tile, else its Ctrl chord.
    hint: Option<String>,
}

/// Every view for the checklist: the tiles in their order, then the hidden
/// views in `Nav::filters` order.
fn view_choices(shown: &[Nav], hidden: &[Nav]) -> Vec<ViewChoice> {
    let tiles = shown.iter().enumerate().map(|(at, nav)| ViewChoice {
        nav: nav.clone(),
        shown: true,
        hint: model::alt_chord(at).or_else(|| nav.chord().map(String::from)),
    });
    let rest = hidden.iter().map(|nav| ViewChoice {
        nav: nav.clone(),
        shown: false,
        hint: nav.chord().map(String::from),
    });
    tiles.chain(rest).collect()
}

/// What the `...` menu is built from, as of the last `update`: the menu
/// itself is made each time it opens.
#[derive(Default)]
struct Options {
    views: Vec<ViewChoice>,
    counts: bool,
    signed_in: bool,
    /// How many lists have a row, for Sort A-Z.
    lists: usize,
    /// How many lists are finished right now, and whether those are put
    /// away.
    finished: usize,
    hide_finished: bool,
}

/// A dim caption over a part of the `...` menu.
fn menu_heading(text: &str) -> gtk::Widget {
    ui::label(text, &["caption-heading", "menu-heading"]).upcast()
}

/// The sidebar's options: which views are tiles, then what the lists do.
fn options_menu(o: &Options, tx: &Tx) -> gtk::Popover {
    let mut items: Vec<gtk::Widget> = vec![menu_heading("Views")];
    for v in &o.views {
        let (tx, nav, shown) = (tx.clone(), v.nav.clone(), v.shown);
        let title = v.nav.title(&[]);
        let mut item = Item::new(Some(v.nav.icon()), &title)
            .tint(v.nav.tint())
            .checked(v.shown);
        if let Some(hint) = &v.hint {
            item = item.secondary(hint);
        }
        items.push(
            item.build(move || {
                tx.emit(if shown {
                    Msg::HideView(nav.clone())
                } else {
                    Msg::ShowView(nav.clone(), None)
                })
            })
            .upcast(),
        );
    }
    {
        let tx = tx.clone();
        items.push(
            Item::new(Some("dock-left-symbolic"), "Reset sidebar")
                .secondary("Inbox, Day")
                .build(move || tx.emit(Msg::ResetSidebar))
                .upcast(),
        );
    }
    items.push(ui::separator());
    items.push(menu_heading("Lists"));
    {
        let tx = tx.clone();
        let sort = Item::new(Some("view-sort-ascending-symbolic"), "Sort A-Z")
            .build(move || tx.emit(Msg::SortLists));
        sort.set_sensitive(o.lists > 1);
        items.push(sort.upcast());
    }
    // Offered while there is something to hide, and while it is on, so it
    // can be turned off.
    if o.finished > 0 || o.hide_finished {
        let (tx, on) = (tx.clone(), o.hide_finished);
        let count = format!("({})", o.finished);
        items.push(
            Item::new(
                Some("check-round-outline-whole-symbolic"),
                "Hide finished lists",
            )
            .secondary(&count)
            .checked(on)
            .build(move || tx.emit(Msg::PatchPrefs(PrefPatch::HideFinishedLists(!on))))
            .upcast(),
        );
    }
    {
        let (tx, on) = (tx.clone(), o.counts);
        items.push(
            Item::new(Some("view-list-ordered-symbolic"), "Show task counts")
                .checked(on)
                .build(move || tx.emit(Msg::PatchPrefs(PrefPatch::ShowCounts(!on))))
                .upcast(),
        );
    }
    {
        let tx = tx.clone();
        let new = Item::new(Some("plus-large-symbolic"), "New list…")
            .secondary("p")
            .build(move || tx.emit(Msg::NewList));
        new.set_sensitive(o.signed_in);
        items.push(new.upcast());
    }
    ui::menu(&items)
}

pub struct Sidebar {
    pub root: adw::ToolbarView,
    tx: Tx,
    tiles_box: gtk::FlowBox,
    tiles: RefCell<Vec<Tile>>,
    tiles_key: RefCell<String>,
    lists_box: gtk::ListBox,
    /// The `Hidden lists` row, and the rows it opens onto.
    hidden_head: gtk::Button,
    hidden_count: gtk::Label,
    hidden_chevron: gtk::Image,
    hidden_box: gtk::ListBox,
    /// Whether the hidden lists are open: for this session only.
    hidden_open: Cell<bool>,
    /// The view `update` saw last. Moving onto a hidden list opens the
    /// Hidden lists row once, so it can be shut again while that list is
    /// on screen.
    opened_for: RefCell<Nav>,
    /// Every row, the shown ones first and then those under Hidden lists.
    rows: RefCell<Vec<ListRow>>,
    lists_key: RefCell<String>,
    /// `Not signed in`, under the Lists heading while that is so.
    caption: gtk::Label,
    add_list: gtk::Button,
    lists_menu: gtk::MenuButton,
    options: Rc<RefCell<Options>>,
    sync: gtk::Button,
}

pub struct State<'a> {
    pub nav: &'a Nav,
    /// The lists that have a row (`Prefs::sidebar_lists`): the inbox is not
    /// among them while its tile shows.
    pub lists: &'a [ListView],
    /// The lists under Hidden lists, and why each is.
    pub hidden: &'a [(ListView, Why)],
    /// How many lists are finished now (`Prefs::finished_count`).
    pub finished: usize,
    /// The inbox list's href, which the Inbox view highlights if it has a row.
    pub inbox: Option<&'a str>,
    pub counts: &'a Counts,
    pub status: Option<&'a StatusView>,
    pub prefs: &'a Prefs,
    pub links: &'a [LinkView],
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

/// Who is signed in and where: `jaeho on cloud.example.com`.
fn account_line(s: &StatusView) -> Option<String> {
    let host = s
        .server
        .as_deref()
        .map(|h| {
            h.trim_start_matches("https://")
                .trim_start_matches("http://")
        })
        .unwrap_or_default();
    match (&s.username, host) {
        (Some(u), h) if !h.is_empty() => Some(format!("{u} on {h}")),
        (Some(u), _) => Some(u.clone()),
        (None, h) if !h.is_empty() => Some(h.to_string()),
        (None, _) => None,
    }
}

/// The sync button's tooltip, worked out when it shows so the time is right.
fn sync_tooltip(status: Option<&StatusView>) -> String {
    let Some(s) = status else {
        return "Sync now".into();
    };
    let keys = "<span size=\"small\" alpha=\"70%\">s</span>";
    let account = account_line(s)
        .map(|a| format!("{}\n", glib::markup_escape_text(&a)))
        .unwrap_or_default();
    match s.state {
        SyncState::Offline => format!(
            "{account}<b>Can't reach the server</b>\nChanges made now are sent when it's back.{}",
            match s.pending {
                0 => String::new(),
                1 => "\n1 change waiting to be sent".into(),
                n => format!("\n{n} changes waiting to be sent"),
            }
        ),
        _ => format!(
            "{account}{}\n{keys}",
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
                        .build(move || a.emit(Msg::Preferences))
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

        // The tiles and the Lists heading stay put; only the lists scroll.
        let pinned = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pinned.add_css_class("sidebar-content");

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
        pinned.append(&tiles_box);

        let source = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        source.add_css_class("source-header");
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        names.set_hexpand(true);
        names.set_valign(gtk::Align::Center);
        names.append(&ui::label("Lists", &["heading"]));
        let caption = ui::label("Not signed in", &["caption", "dim-label"]);
        caption.set_visible(false);
        names.append(&caption);
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
        let options = Rc::new(RefCell::new(Options::default()));
        let lists_menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Sidebar options")
            .valign(gtk::Align::Center)
            .build();
        lists_menu.add_css_class("flat");
        {
            let (tx, options) = (tx.clone(), options.clone());
            lists_menu.set_create_popup_func(move |mb| {
                mb.set_popover(Some(&options_menu(&options.borrow(), &tx)));
            });
        }
        source.append(&lists_menu);
        pinned.append(&source);
        let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
        rule.add_css_class("source-rule");
        pinned.append(&rule);

        let lists_box = gtk::ListBox::new();
        lists_box.add_css_class("sidebar-lists");
        lists_box.set_selection_mode(gtk::SelectionMode::None);

        // Under the lists: the row that opens the hidden ones.
        let hidden_count = ui::label("", &["caption", "dim-label"]);
        let hidden_chevron = ui::icon("pan-end-symbolic", 12);
        hidden_chevron.add_css_class("dim-label");
        let hidden_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hidden_line.add_css_class("list-line");
        let hidden_title = ui::label("Hidden lists", &["dim-label"]);
        hidden_title.set_hexpand(true);
        hidden_line.append(&hidden_title);
        hidden_line.append(&hidden_count);
        hidden_line.append(&hidden_chevron);
        let hidden_head = gtk::Button::builder().child(&hidden_line).build();
        hidden_head.add_css_class("flat");
        hidden_head.add_css_class("hidden-lists");
        hidden_head.set_tooltip_text(Some(
            "Archived lists, and lists with nothing left to do when \"Hide finished lists\" is on.",
        ));
        hidden_head.set_visible(false);
        let hidden_box = gtk::ListBox::new();
        hidden_box.add_css_class("sidebar-lists");
        hidden_box.add_css_class("hidden-lists");
        hidden_box.set_selection_mode(gtk::SelectionMode::None);
        hidden_box.set_visible(false);

        let lists_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lists_body.append(&lists_box);
        lists_body.append(&hidden_head);
        lists_body.append(&hidden_box);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&lists_body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        ui::scroll_while_dragging(&scroller);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&pinned);
        body.append(&scroller);
        let root = adw::ToolbarView::new();
        root.add_css_class("app-sidebar");
        root.add_top_bar(&header);
        root.set_content(Some(&body));

        let s = Rc::new(Sidebar {
            root,
            tx,
            tiles_box,
            tiles: RefCell::default(),
            tiles_key: RefCell::default(),
            lists_box,
            hidden_head,
            hidden_count,
            hidden_chevron,
            hidden_box,
            hidden_open: Cell::new(false),
            opened_for: RefCell::new(Nav::Inbox),
            rows: RefCell::default(),
            lists_key: RefCell::default(),
            caption,
            add_list,
            lists_menu,
            options,
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
        for list in [&s.lists_box, &s.hidden_box] {
            let weak = Rc::downgrade(&s);
            list.connect_row_activated(move |_, row| {
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
        {
            let weak = Rc::downgrade(&s);
            s.hidden_head.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.toggle_hidden();
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
            self.build_tiles(&shown, Rc::new(st.prefs.hidden_views()));
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
            "{:?}{:?}{:?}",
            st.lists
                .iter()
                .map(|l| (&l.href, &l.name, &l.color, l.writable, l.inbox))
                .collect::<Vec<_>>(),
            st.hidden
                .iter()
                .map(|(l, why)| (&l.href, &l.name, &l.color, l.writable, l.inbox, *why))
                .collect::<Vec<_>>(),
            st.prefs.list_icons
        );
        // The keyboard stays on its row through a move or an archive: read
        // before the old rows go, which takes the focus with them.
        let mut kept = None;
        if *self.lists_key.borrow() != lists_key {
            *self.lists_key.borrow_mut() = lists_key;
            kept = self.focused_row();
            self.build_lists(st.lists, st.hidden);
        }
        // Hidden lists open by themselves for a hidden list that has just
        // come on screen, once: the row can still be closed after.
        let on_hidden = st
            .hidden
            .iter()
            .any(|(l, _)| *st.nav == Nav::List(l.href.clone()));
        if on_hidden && *self.opened_for.borrow() != *st.nav {
            self.hidden_open.set(true);
        }
        *self.opened_for.borrow_mut() = st.nav.clone();
        self.show_hidden();
        if let Some(kept) = kept {
            self.refocus_row(kept);
        }
        for r in self.rows.borrow().iter() {
            if r.hidden.is_none() {
                let n = st.counts.lists.get(&r.href).copied().unwrap_or(0);
                r.count.set_text(&if n > 0 && st.prefs.show_counts {
                    n.to_string()
                } else {
                    String::new()
                });
            }
            match st.links.iter().find(|l| l.list == r.href) {
                Some(l) => {
                    r.link.set_visible(true);
                    r.link
                        .set_tooltip_text(Some(&model::link_state(l, pickers::now())));
                    if model::link_warns(l) {
                        r.link.add_css_class("warning");
                    } else {
                        r.link.remove_css_class("warning");
                    }
                }
                None => r.link.set_visible(false),
            }
            if *st.nav == Nav::List(r.href.clone())
                || (*st.nav == Nav::Inbox && st.inbox == Some(r.href.as_str()))
            {
                r.row.add_css_class("selected");
            } else {
                r.row.remove_css_class("selected");
            }
        }

        self.caption
            .set_visible(st.status.is_some_and(|s| s.state == SyncState::NoAccount));
        self.add_list.set_sensitive(signed_in);
        *self.options.borrow_mut() = Options {
            views: view_choices(&shown, &st.prefs.hidden_views()),
            counts: st.prefs.show_counts,
            signed_in,
            lists: st.lists.len(),
            finished: st.finished,
            hide_finished: st.prefs.hide_finished_lists,
        };

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

    /// The menu of the tile for `nav`, as a right-click or the Menu key opens it.
    pub fn open_tile_menu(&self, nav: &Nav) {
        let tiles = self.tiles.borrow();
        if let Some(t) = tiles.iter().find(|t| &t.nav == nav) {
            show_tile_menu(&t.child, &t.nav, &t.hidden, &self.tx, middle(&t.child));
        }
    }

    /// The `...` menu next to Lists, as a click on it opens it.
    pub fn open_lists_menu(&self) {
        self.lists_menu.popup();
    }

    /// Where the keyboard stops, top to bottom: the tiles, the lists, the
    /// Hidden lists row, and the lists under it while it is open.
    fn stops(&self) -> Vec<Stop> {
        let mut stops: Vec<Stop> = self
            .tiles
            .borrow()
            .iter()
            .map(|t| Stop {
                id: format!("tile:{}", t.nav.key()),
                widget: t.child.clone().upcast(),
            })
            .collect();
        stops.extend(self.row_stops());
        stops
    }

    /// The stops below the tiles.
    fn row_stops(&self) -> Vec<Stop> {
        let rows = self.rows.borrow();
        let row = |r: &ListRow| Stop {
            id: format!("list:{}", r.href),
            widget: r.row.clone().upcast(),
        };
        let mut stops: Vec<Stop> = rows
            .iter()
            .filter(|r| r.hidden.is_none())
            .map(row)
            .collect();
        if self.hidden_head.is_visible() {
            stops.push(Stop {
                id: "hidden".into(),
                widget: self.hidden_head.clone().upcast(),
            });
        }
        if self.hidden_box.is_visible() {
            stops.extend(rows.iter().filter(|r| r.hidden.is_some()).map(row));
        }
        stops
    }

    /// The row that has the keyboard: its place among `row_stops` and its id.
    fn focused_row(&self) -> Option<(usize, String)> {
        self.row_stops()
            .into_iter()
            .enumerate()
            .find(|(_, s)| s.widget.is_focus())
            .map(|(at, s)| (at, s.id))
    }

    /// Gives the keyboard back to the row `focused_row` found, or to the one
    /// that has its place now when it is gone.
    fn refocus_row(&self, (at, id): (usize, String)) {
        let stops = self.row_stops();
        let stop = stops
            .iter()
            .find(|s| s.id == id)
            .or_else(|| stops.get(at).or(stops.last()));
        if let Some(stop) = stop {
            stop.widget.grab_focus();
        }
    }

    /// Which stop has the keyboard, by its id (the harness reports it).
    pub fn focused(&self) -> Option<String> {
        self.stops()
            .into_iter()
            .find(|s| s.widget.is_focus())
            .map(|s| s.id)
    }

    /// Moves the keyboard to another stop. From one of them it goes on to
    /// the next, back to the previous, or to either end, across the tiles
    /// and the lists, which the arrow keys do not cross on their own. From
    /// a button in the sidebar, a step goes where the arrow key would.
    pub fn reach(&self, to: Reach) {
        let stops = self.stops();
        let at = stops.iter().position(|s| s.widget.is_focus());
        let target = match (at, to) {
            (Some(at), _) => reach_index(stops.len(), at, to),
            (None, Reach::First | Reach::Last) => reach_index(stops.len(), 0, to),
            (None, Reach::Next | Reach::Prev) => {
                let dir = if to == Reach::Next {
                    gtk::DirectionType::Down
                } else {
                    gtk::DirectionType::Up
                };
                self.root.child_focus(dir);
                return;
            }
        };
        if let Some(stop) = target.and_then(|i| stops.get(i)) {
            stop.widget.grab_focus();
            // A key that this handler took never reached the window's own,
            // which is what turns the focus ring on after a click.
            if let Some(window) = stop.widget.root().and_downcast::<gtk::Window>() {
                window.set_focus_visible(true);
            }
        }
    }

    /// The menu of the row for list `href`, as a right-click opens it.
    pub fn open_list_menu(&self, href: &str) {
        let rows = self.rows.borrow();
        if let Some(r) = rows.iter().find(|r| r.href == href) {
            let archived = r.hidden == Some(Why::Archived);
            show_list_menu(&r.row, &r.list, archived, &self.tx, middle(&r.row));
        }
    }

    fn build_tiles(&self, shown: &[Nav], hidden: Rc<Vec<Nav>>) {
        // The keyboard stays on the tile it was on through a move or a hide:
        // read before the old tiles go, which takes the focus with them.
        let focused = self
            .tiles
            .borrow()
            .iter()
            .enumerate()
            .find(|(_, t)| t.child.is_focus())
            .map(|(at, t)| (t.nav.clone(), at));
        self.tiles_box.remove_all();
        let mut tiles = Vec::new();
        for (at, nav) in shown.iter().enumerate() {
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
            let keys = model::tile_keys(nav, at);
            if !keys.is_empty() {
                ui::tip(&child, &nav.title(&[]), &keys);
            }
            if !matches!(
                nav,
                Nav::Matrix | Nav::Scheduled | Nav::Anytime | Nav::Repeating | Nav::All
            ) {
                drop_target(&child, nav.clone(), &self.tx);
            }
            let menu_click = gtk::GestureClick::builder()
                .button(gdk::BUTTON_SECONDARY)
                .build();
            {
                let (tx, nav, hidden) = (self.tx.clone(), nav.clone(), hidden.clone());
                let child_weak = child.downgrade();
                menu_click.connect_pressed(move |g, _, x, y| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let Some(child) = child_weak.upgrade() else {
                        return;
                    };
                    let at = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                    show_tile_menu(&child, &nav, &hidden, &tx, at);
                });
            }
            let keys = gtk::EventControllerKey::new();
            {
                let (tx, nav, hidden) = (self.tx.clone(), nav.clone(), hidden.clone());
                let child_weak = child.downgrade();
                keys.connect_key_pressed(move |_, key, _, state| {
                    let Some(action) = tile_key(key, state) else {
                        return glib::Propagation::Proceed;
                    };
                    match action {
                        TileKey::Move(by) => tx.emit(Msg::MoveView(nav.clone(), by)),
                        TileKey::Hide => tx.emit(Msg::HideView(nav.clone())),
                        TileKey::Menu => {
                            if let Some(child) = child_weak.upgrade() {
                                show_tile_menu(&child, &nav, &hidden, &tx, middle(&child));
                            }
                        }
                    }
                    glib::Propagation::Stop
                });
            }
            child.add_controller(keys);
            child.add_controller(menu_click);
            self.tiles_box.append(&child);
            tiles.push(Tile {
                nav: nav.clone(),
                child,
                count,
                dot: dot.upcast(),
                hidden: hidden.clone(),
            });
        }
        // The place it had, when the tile itself went.
        if let Some((nav, at)) = focused
            && let Some(t) = tiles
                .iter()
                .find(|t| t.nav == nav)
                .or_else(|| tiles.get(at.min(tiles.len().saturating_sub(1))))
        {
            t.child.grab_focus();
        }
        *self.tiles.borrow_mut() = tiles;
    }

    /// A click on the Hidden lists row: the lists under it open or shut.
    pub fn toggle_hidden(&self) {
        self.hidden_open.set(!self.hidden_open.get());
        self.show_hidden();
    }

    /// The Hidden lists row and the rows under it, as `hidden_open` says. The
    /// row is there while any list is hidden.
    fn show_hidden(&self) {
        let n = self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.hidden.is_some())
            .count();
        let open = self.hidden_open.get() && n > 0;
        self.hidden_head.set_visible(n > 0);
        self.hidden_box.set_visible(open);
        self.hidden_count.set_text(&n.to_string());
        self.hidden_chevron.set_icon_name(Some(if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
    }

    /// A list's line: its mark, name, a lock when read-only, the link to a
    /// repo, and `count` for the task count or the reason it is hidden.
    fn list_line(l: &ListView) -> (gtk::Box, gtk::Label, gtk::Image) {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        line.add_css_class("list-line");
        line.append(&ui::list_mark(l, 18));
        let name = ui::label(&l.name, &[]);
        name.set_hexpand(true);
        line.append(&name);
        if !l.writable {
            let lock = ui::icon("permissions-generic-symbolic", 12);
            lock.add_css_class("dim-label");
            lock.set_tooltip_text(Some("Read-only"));
            line.append(&lock);
        }
        let link = ui::icon("chain-link-loose-symbolic", 12);
        link.add_css_class("dim-label");
        link.set_visible(false);
        line.append(&link);
        let count = ui::label("", &["caption", "dim-label"]);
        line.append(&count);
        (line, count, link)
    }

    /// A right-click opens the list's menu, with Archive or Unarchive as
    /// the row's place says.
    fn list_menu_on(&self, row: &gtk::ListBoxRow, list: &ListView, archived: bool) {
        let menu_click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let tx = self.tx.clone();
        let list = list.clone();
        let row_weak = row.downgrade();
        menu_click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(row) = row_weak.upgrade() else {
                return;
            };
            let at = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            show_list_menu(&row, &list, archived, &tx, at);
        });
        row.add_controller(menu_click);
    }

    /// The keys of a focused row: Menu and Shift+F10 open its menu, and on
    /// a row that is not hidden, Alt+Up and Alt+Down move it. `order` is
    /// the lists that have a row, top to bottom.
    fn list_keys_on(
        &self,
        row: &gtk::ListBoxRow,
        list: &ListView,
        archived: bool,
        order: Option<Rc<Vec<String>>>,
    ) {
        let keys = gtk::EventControllerKey::new();
        let (tx, list) = (self.tx.clone(), list.clone());
        let row_weak = row.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            match row_key(key, state) {
                Some(RowKey::Move(by)) => {
                    let Some(order) = &order else {
                        return glib::Propagation::Proceed;
                    };
                    if let Some(new) = moved(order, &list.href, by) {
                        tx.emit(Msg::ReorderLists(new));
                    }
                }
                Some(RowKey::Menu) => {
                    if let Some(row) = row_weak.upgrade() {
                        show_list_menu(&row, &list, archived, &tx, middle(&row));
                    }
                }
                None => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        row.add_controller(keys);
    }

    /// Dragging a row works whether the lists were last put in order by hand
    /// or by name: a drop sets the order down and the by-name flag off. The
    /// hidden lists have rows too, dimmed and with the reason beside them;
    /// they take a click and a menu, and no drag.
    fn build_lists(&self, lists: &[ListView], hidden: &[(ListView, Why)]) {
        self.lists_box.remove_all();
        self.hidden_box.remove_all();
        let mut rows = Vec::new();
        let order: Rc<Vec<String>> = Rc::new(lists.iter().map(|l| l.href.clone()).collect());
        for l in lists {
            let (line, count, link) = Sidebar::list_line(l);
            let row = gtk::ListBoxRow::new();
            row.add_css_class("list-row");
            row.set_child(Some(&line));
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
            self.list_drops(&row, l, order.clone());
            self.list_menu_on(&row, l, false);
            self.list_keys_on(&row, l, false, Some(order.clone()));
            self.lists_box.append(&row);
            rows.push(ListRow {
                href: l.href.clone(),
                list: l.clone(),
                row,
                count,
                link,
                hidden: None,
            });
        }
        for (l, why) in hidden {
            let (line, count, link) = Sidebar::list_line(l);
            count.set_text(why.caption());
            let row = gtk::ListBoxRow::new();
            row.add_css_class("list-row");
            row.add_css_class("hidden-list");
            row.set_child(Some(&line));
            self.list_menu_on(&row, l, *why == Why::Archived);
            self.list_keys_on(&row, l, *why == Why::Archived, None);
            self.hidden_box.append(&row);
            rows.push(ListRow {
                href: l.href.clone(),
                list: l.clone(),
                row,
                count,
                link,
                hidden: Some(*why),
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

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::ModifierType as M;

    fn choice(nav: Nav, shown: bool, hint: Option<&str>) -> ViewChoice {
        ViewChoice {
            nav,
            shown,
            hint: hint.map(String::from),
        }
    }

    #[test]
    fn sidebar_choices_list_shown_in_order_then_hidden() {
        let mut p = Prefs::default();
        p.set_sidebar_views(&[Nav::Scheduled, Nav::Inbox]);
        let got = view_choices(&p.sidebar_views(), &p.hidden_views());
        // The tiles first, as they stand, each with the key of its place;
        // then the rest as `Nav::filters` has them, a Ctrl chord where
        // one exists.
        assert_eq!(
            got,
            [
                choice(Nav::Scheduled, true, Some("Alt+1")),
                choice(Nav::Inbox, true, Some("Alt+2")),
                choice(Nav::Today, false, Some("Ctrl+T")),
                choice(Nav::Day, false, None),
                choice(Nav::Matrix, false, None),
                choice(Nav::Completed, false, None),
                choice(Nav::Tomorrow, false, None),
                choice(Nav::Anytime, false, None),
                choice(Nav::Repeating, false, None),
                choice(Nav::All, false, None),
            ]
        );
        // Nothing is dropped or doubled, whatever is shown.
        p.set_sidebar_views(&Nav::filters());
        let got = view_choices(&p.sidebar_views(), &p.hidden_views());
        assert_eq!(got.len(), Nav::filters().len());
        assert!(got.iter().all(|c| c.shown));
        // Only nine places have an Alt digit; the tenth falls back to its chord.
        assert_eq!(got[8].hint.as_deref(), Some("Alt+9"));
        assert_eq!(got[9].hint, None);
    }

    #[test]
    fn the_account_line_names_the_user_and_the_host_without_its_scheme() {
        let status = |server: Option<&str>, user: Option<&str>| StatusView {
            state: SyncState::Idle,
            message: None,
            server: server.map(String::from),
            username: user.map(String::from),
            last_sync: None,
            pending: 0,
            zone: "UTC".into(),
            location: None,
        };
        for (server, user, want) in [
            (
                Some("https://cloud.example.com"),
                Some("jaeho"),
                Some("jaeho on cloud.example.com"),
            ),
            (
                Some("http://nas.lan"),
                Some("jaeho"),
                Some("jaeho on nas.lan"),
            ),
            (None, Some("jaeho"), Some("jaeho")),
            (
                Some("https://cloud.example.com"),
                None,
                Some("cloud.example.com"),
            ),
            (None, None, None),
        ] {
            assert_eq!(
                account_line(&status(server, user)).as_deref(),
                want,
                "{server:?} {user:?}"
            );
        }
        // It leads the sync button's tooltip, and an unsigned-in one has
        // nothing to lead with.
        let tip = sync_tooltip(Some(&status(
            Some("https://cloud.example.com"),
            Some("jaeho"),
        )));
        assert!(tip.starts_with("jaeho on cloud.example.com\n"), "{tip}");
        let tip = sync_tooltip(Some(&status(None, None)));
        assert!(!tip.contains(" on "), "{tip}");
    }

    #[test]
    fn a_tile_moves_hides_and_opens_its_menu_from_the_keyboard() {
        let none = M::empty();
        assert_eq!(
            tile_key(gdk::Key::Left, M::ALT_MASK),
            Some(TileKey::Move(-1))
        );
        assert_eq!(
            tile_key(gdk::Key::Right, M::ALT_MASK),
            Some(TileKey::Move(1))
        );
        assert_eq!(tile_key(gdk::Key::Delete, none), Some(TileKey::Hide));
        assert_eq!(tile_key(gdk::Key::Menu, none), Some(TileKey::Menu));
        assert_eq!(tile_key(gdk::Key::F10, M::SHIFT_MASK), Some(TileKey::Menu));
        // Caps Lock changes nothing.
        assert_eq!(
            tile_key(gdk::Key::Right, M::ALT_MASK | M::LOCK_MASK),
            Some(TileKey::Move(1))
        );
        // The plain arrows are the flow box's, and a chord with more in it
        // is somebody else's.
        for (key, mods) in [
            (gdk::Key::Left, none),
            (gdk::Key::Right, M::SHIFT_MASK),
            (gdk::Key::Left, M::ALT_MASK | M::CONTROL_MASK),
            (gdk::Key::Delete, M::SHIFT_MASK),
            (gdk::Key::F10, none),
            (gdk::Key::Up, M::ALT_MASK),
        ] {
            assert_eq!(tile_key(key, mods), None, "{key:?} {mods:?}");
        }
    }

    #[test]
    fn a_list_row_moves_and_opens_its_menu_from_the_keyboard() {
        let none = M::empty();
        assert_eq!(row_key(gdk::Key::Up, M::ALT_MASK), Some(RowKey::Move(-1)));
        assert_eq!(row_key(gdk::Key::Down, M::ALT_MASK), Some(RowKey::Move(1)));
        assert_eq!(row_key(gdk::Key::Menu, none), Some(RowKey::Menu));
        assert_eq!(row_key(gdk::Key::F10, M::SHIFT_MASK), Some(RowKey::Menu));
        // Caps Lock changes nothing.
        assert_eq!(
            row_key(gdk::Key::Down, M::ALT_MASK | M::LOCK_MASK),
            Some(RowKey::Move(1))
        );
        // Delete is no row's: a list goes only through its menu. The plain
        // arrows and Alt+Left/Right are the list's and the tiles'.
        for (key, mods) in [
            (gdk::Key::Delete, none),
            (gdk::Key::Up, none),
            (gdk::Key::Down, M::SHIFT_MASK),
            (gdk::Key::Left, M::ALT_MASK),
            (gdk::Key::Up, M::ALT_MASK | M::CONTROL_MASK),
            (gdk::Key::F10, none),
        ] {
            assert_eq!(row_key(key, mods), None, "{key:?} {mods:?}");
        }
    }

    #[test]
    fn a_moved_list_changes_places_with_its_neighbour_and_not_past_an_end() {
        let order: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let got = |href, by| moved(&order, href, by);
        assert_eq!(
            got("b", -1),
            Some(["b", "a", "c"].map(String::from).to_vec())
        );
        assert_eq!(
            got("b", 1),
            Some(["a", "c", "b"].map(String::from).to_vec())
        );
        assert_eq!(got("a", -1), None);
        assert_eq!(got("c", 1), None);
        assert_eq!(got("zz", 1), None);
    }

    #[test]
    fn the_keyboard_goes_on_to_the_next_stop_and_holds_at_the_ends() {
        // Four stops, say two tiles and two lists.
        let from = |at, to| reach_index(4, at, to);
        assert_eq!(from(0, Reach::Next), Some(1));
        assert_eq!(from(3, Reach::Next), Some(3));
        assert_eq!(from(2, Reach::Prev), Some(1));
        assert_eq!(from(0, Reach::Prev), Some(0));
        assert_eq!(from(2, Reach::First), Some(0));
        assert_eq!(from(1, Reach::Last), Some(3));
        // Nothing to reach with no stops.
        for to in [Reach::Next, Reach::Prev, Reach::First, Reach::Last] {
            assert_eq!(reach_index(0, 0, to), None);
        }
    }
}
