//! The window: the sidebar, and a view's tasks, one of them open in place.
//! What it shows comes from asstd, and every change goes back to it over
//! D-Bus.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

use asst_core::api::{
    AddSpec, Added, Change, ListChange, ListSpec, ListView, Settings, SettingsChange,
    StatusView, SyncState, TaskView, query,
};
use asst_core::fmt;
use asst_core::store::{Query, View};
use asst_core::time::When;
use chrono::{DateTime, Duration as Days, NaiveDate, Utc};
use chrono_tz::Tz;
use futures_util::StreamExt;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk, gio, glib};
use relm4::{Component, ComponentParts, ComponentSender, RelmApp, adw};

use crate::addcard::{AddCard, Target};
use crate::content::{self, Opts};
use crate::editor::Editor;
use crate::model::{self, Counts, Data, Nav, Sort, ViewOpts};
use crate::motion;
use crate::pickers::{self, DateOpts};
use crate::prefs::Prefs;
use crate::sidebar::{self, Sidebar};
use crate::tray::{self, Tray};
use crate::ui::{self, Item};
use crate::{client, dialogs, find, settings};

pub type Tx = relm4::Sender<Msg>;

/// How long a repeating task's new date stays lit (`.due-chip.rolled`).
const ROLLED_MS: u32 = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Window,
    /// In the tray, with no window, as login does.
    Background,
    /// Started by the bus for an action, such as opening a task from a
    /// reminder: the action shows the window.
    Service,
}

pub fn run(start: Start) {
    let app = RelmApp::new(crate::devel::app_id())
        .with_args(Vec::new())
        .visible_on_activate(false);
    let gapp = relm4::main_application();
    // Starting asst again shows the window it already has, except from a
    // login script, which shouldn't open one. (Registering to find out
    // would start the app before relm4 is listening for it.)
    if start == Start::Background && running(crate::devel::app_id()) {
        return;
    }
    if start == Start::Service {
        gapp.set_flags(gapp.flags() | gio::ApplicationFlags::IS_SERVICE);
    }
    // Without the tray to come back from, starting hidden would leave no way in.
    let quiet = Cell::new(start == Start::Background && Prefs::load().background);
    gapp.connect_activate(move |app| {
        if quiet.replace(false) {
            return;
        }
        if let Some(window) = app
            .windows()
            .into_iter()
            .find(|w| w.is::<adw::ApplicationWindow>())
        {
            window.present();
        }
    });
    relm4::set_global_css(crate::CSS);
    app.run::<Window>(());
}

/// Whether an instance owns the app's name on the session bus.
fn running(app_id: &str) -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return false;
    };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(app_id,).to_variant()),
        Some(glib::VariantTy::new("(b)").expect("valid type")),
        gio::DBusCallFlags::NONE,
        2000,
        None::<&gio::Cancellable>,
    )
    .ok()
    .and_then(|reply| reply.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}

#[derive(Debug, Clone)]
pub enum Batch {
    Complete,
    Delete,
    Priority(u8),
    Date(NaiveDate),
    Move(String),
    Copy,
}

#[derive(Debug)]
pub enum Msg {
    Navigate(Nav),
    ToggleSidebar,
    Refresh,
    Tick,
    Focused,
    SyncNow,
    /// A row was clicked or got Enter.
    Activate(String),
    /// Open a task in place.
    Open(String),
    /// Close it, folding the card back to a row first.
    CloseEditor,
    /// The card has folded: the row can take its place.
    Folded(String),
    /// A row's checkbox.
    Check(String, bool),
    CompleteNow(String, u64),
    Complete(String),
    Reopen(String),
    Edit(String, Box<Change>),
    EditMany(Vec<(String, Change)>),
    /// New custom-order numbers after a drag.
    Reorder(Vec<(String, i64)>),
    Delete(String),
    Undelete(String),
    /// The undo toast is gone: delete for real unless undone.
    DeleteNow(String),
    Duplicate(String),
    Copy(String),
    Move(String, String),
    DropTask(String, Nav),
    Add(Box<AddSpec>),
    ShowAdd(Option<Target>),
    HideAdd,
    /// The add card has folded away.
    AddFolded,
    SelectMode(bool),
    SelectToggle(String),
    Batch(Batch),
    SetViewOpts(ViewOpts),
    NewList,
    EditList(String),
    SaveList(Option<String>, String, String),
    AskDeleteList(String),
    DeleteList(String),
    /// A list's completed tasks, or every list's: ask first.
    AskDeleteCompleted(Option<String>),
    DeleteCompleted(Option<String>),
    /// Lists dragged into a new order (hrefs, top to bottom).
    ReorderLists(Vec<String>),
    /// Put a list away, or bring it back.
    Archive(String, bool),
    /// A list's open tasks to the clipboard, as Markdown.
    CopyList(String),
    /// A task's iCalendar object, to a file.
    SaveIcs(String),
    /// Search Nominatim for a place for a location reminder.
    PlaceSearch(String),
    /// Ctrl+V: a task from the clipboard's text.
    Paste,
    /// The add card with this text in it.
    AddText(String),
    /// Show the window at a task: the `open-task` action, as a reminder's
    /// notification sends it.
    OpenTask(String),
    /// Light or dark changed.
    ThemeChanged,
    QuickFind,
    /// Go to where a task lives and open it.
    Reveal(String),
    /// On the page named, or the first.
    Preferences(Option<&'static str>),
    Shortcuts,
    About,
    SetPrefs(Box<Prefs>),
    /// Take a view out of the sidebar.
    HideView(Nav),
    SetSidebar(Vec<Nav>),
    SetSettings(Box<SettingsChange>),
    SignIn(String),
    SignOut,
    CancelSignIn,
    /// A popover closed or typing left the editor: a rebuild held back for
    /// it can happen now.
    Resume,
    Key(Key),
    /// The window's close button, Ctrl+W or the compositor: into the tray
    /// when running in the background, otherwise quit.
    CloseWindow,
    /// Send what is waiting (typing, deletes held for undo), then quit.
    Quit,
    ShowWindow,
    /// A click on the tray icon: show the window, or put it away.
    TrayClick,
    /// A scripted click or key, in development builds.
    Drive(String),
}

#[derive(Debug, Clone, Copy)]
pub enum Key {
    Down,
    Up,
    First,
    Last,
    Open,
    Toggle,
    Add,
    Find,
    Delete,
    Priority(u8),
    Today,
    Tomorrow,
    NextWeek,
    PrevView,
    NextView,
    Sync,
    Select,
    Escape,
    NewList,
    Shortcuts,
    Preferences,
    Sidebar,
    Close,
    Quit,
    Paste,
    Go(Nav0),
}

#[derive(Debug, Clone, Copy)]
pub enum Nav0 {
    Inbox,
    Today,
    Scheduled,
    List(usize),
}

pub struct Loaded {
    lists: Vec<ListView>,
    open: Vec<TaskView>,
    status: StatusView,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Loaded({} lists, {} open)",
            self.lists.len(),
            self.open.len()
        )
    }
}

#[derive(Debug)]
pub enum Cmd {
    Loaded(u64, client::Result<Loaded>),
    CompletedLoaded(u64, client::Result<Vec<TaskView>>),
    Changed,
    Status(StatusView),
    LoginDone(bool, String),
    LoginUrl(client::Result<String>),
    Settings(client::Result<Settings>),
    Toast(client::Result<String>),
    Completed(String, client::Result<TaskView>),
    Added(client::Result<Added>),
    Opened(client::Result<TaskView>),
    ListSaved(client::Result<ListView>),
    /// A task's iCalendar object, to save.
    Ics(String, client::Result<String>),
    /// Places found for a location reminder's search.
    Places(Result<Vec<asst_core::nominatim::Place>, String>),
    Closed,
}

/// Where the keyboard goes after the next rebuild of the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refocus {
    /// Where it was.
    Keep,
    /// The cursor's row.
    Row,
    /// The title of the task open in place.
    Editor,
}

/// The last build of the view, in the terms animations compare against.
#[derive(Debug, Default)]
struct Built {
    /// `Nav::key`; none before the first build with data.
    nav: Option<String>,
    completing: HashSet<String>,
    adding: bool,
    selecting: bool,
    /// A task was open in place: the other rows were dimmed.
    editing: bool,
}

/// A checked task on its way out.
#[derive(Debug, Clone)]
enum Completing {
    /// Struck through, waiting out the moment to undo.
    Waiting(u64),
    /// Sent; shown checked until the daemon's data moves on. Holds the due
    /// date it had, to notice a repeating task moving to its next one.
    Sent(Option<When>),
}

pub struct Window {
    nav: Nav,
    lists: Vec<ListView>,
    open: Vec<TaskView>,
    completed: Option<Vec<TaskView>>,
    status: Option<StatusView>,
    settings: Option<Settings>,
    error: Option<String>,
    loaded: bool,
    prefs: Prefs,
    zone: Tz,
    cursor: Option<String>,
    /// The task open in place.
    editing: Option<String>,
    /// Grow the editor into its card once the next rebuild has placed it.
    expand: bool,
    /// The open task's card is folding; its row comes back after.
    folding: Option<String>,
    refocus: Refocus,
    adding: Option<Target>,
    /// The add card is folding away.
    add_folding: bool,
    select_mode: bool,
    selected: HashSet<String>,
    completing: HashMap<String, Completing>,
    /// Repeating tasks checked and moved on to their next date, whose date
    /// lights up in the next build.
    rolled: HashSet<String>,
    /// What the view was last built with, to tell what is new in the next.
    built: Built,
    /// Rebuilds for the daemon's news wait until then, so rows opening or
    /// folding finish on the widgets they started on.
    settle_until: Option<Instant>,
    serial: u64,
    hidden: HashSet<String>,
    load_generation: u64,
    completed_generation: u64,
    dirty: bool,
    shown: u64,
    pending_d: Option<Instant>,
    last_focus_sync: Option<Instant>,
    signing_in: Option<String>,
    sign_in_error: Option<String>,
}

pub struct SignIn {
    entry: gtk::Entry,
    button: gtk::Button,
    waiting: gtk::Box,
    again: gtk::LinkButton,
    error: gtk::Label,
}

pub struct Widgets {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    outer: adw::OverlaySplitView,
    sidebar: Rc<Sidebar>,
    header_title: gtk::Label,
    stack: gtk::Stack,
    clamp: adw::Clamp,
    rows: Vec<(String, gtk::ListBoxRow)>,
    editor: Rc<Editor>,
    /// The daemon's status for what counts the time since a sync as it
    /// passes: Preferences and the sync button's tooltip.
    status: Rc<RefCell<Option<StatusView>>>,
    add_card: Rc<AddCard>,
    select_revealer: gtk::Revealer,
    select_label: gtk::Label,
    error_page: adw::StatusPage,
    signin: SignIn,
    view_button: gtk::MenuButton,
    more_button: gtk::MenuButton,
    /// Up while running in the background.
    tray: Option<Rc<Tray>>,
    /// Set once what was waiting has been sent, so the window can go.
    quitting: Rc<Cell<bool>>,
}

/// Whether a click on `target` is on nothing in particular: not a row, a
/// button, a field, a scrollbar or a popover.
fn on_background(target: &gtk::Widget) -> bool {
    ![
        gtk::ListBoxRow::static_type(),
        gtk::Button::static_type(),
        gtk::Text::static_type(),
        gtk::TextView::static_type(),
        gtk::Scrollbar::static_type(),
        gtk::Popover::static_type(),
    ]
    .into_iter()
    .any(|t| target.ancestor(t).is_some())
}

fn toast(w: &Widgets, text: &str) {
    w.toasts.add_toast(
        adw::Toast::builder()
            .title(glib::markup_escape_text(text))
            .timeout(3)
            .build(),
    );
}

fn quoted(s: &str) -> String {
    let short: String = s.chars().take(40).collect();
    if short.len() < s.len() {
        format!("“{short}…”")
    } else {
        format!("“{short}”")
    }
}

impl Component for Window {
    type Init = ();
    type Input = Msg;
    type Output = ();
    type CommandOutput = Cmd;
    type Root = adw::ApplicationWindow;
    type Widgets = Widgets;

    fn init_root() -> adw::ApplicationWindow {
        crate::load_icons();
        let prefs = Prefs::load();
        let window = adw::ApplicationWindow::builder()
            .title("asst")
            .default_width(prefs.width)
            .default_height(prefs.height)
            .width_request(360)
            .height_request(300)
            .build();
        if prefs.maximized {
            window.maximize();
        }
        if crate::devel::enabled() {
            window.add_css_class("devel");
        }
        // The palette in style.css has a lighter set for dark mode.
        let style = adw::StyleManager::default();
        let set_dark = {
            let window = window.clone();
            move |s: &adw::StyleManager| {
                if s.is_dark() {
                    window.add_css_class("dark");
                } else {
                    window.remove_css_class("dark");
                }
            }
        };
        set_dark(&style);
        style.connect_dark_notify(set_dark);
        window
    }

    fn init(
        _: (),
        window: adw::ApplicationWindow,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let tx: Tx = sender.input_sender().clone();
        let prefs = Prefs::load();

        // -- sidebar
        let status: Rc<RefCell<Option<StatusView>>> = Rc::default();
        let sidebar = Sidebar::new(tx.clone(), status.clone());

        // -- content header
        let header = adw::HeaderBar::new();
        header.add_css_class("flat");
        let sidebar_toggle = gtk::Button::from_icon_name("dock-left-symbolic");
        sidebar_toggle.add_css_class("flat");
        ui::tip(&sidebar_toggle, "Show or hide the sidebar", "Ctrl+B");
        {
            let tx = tx.clone();
            sidebar_toggle.connect_clicked(move |_| tx.emit(Msg::ToggleSidebar));
        }
        header.pack_start(&sidebar_toggle);
        let header_title = gtk::Label::new(None);
        header_title.add_css_class("heading");
        let title_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .child(&header_title)
            .build();
        header.set_title_widget(Some(&title_revealer));

        let more_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("List menu")
            .build();
        more_button.add_css_class("flat");
        header.pack_end(&more_button);
        let view_button = gtk::MenuButton::builder()
            .icon_name("view-sort-descending-rtl-symbolic")
            .tooltip_text("View options")
            .build();
        view_button.add_css_class("flat");
        header.pack_end(&view_button);

        // -- the column of tasks
        let clamp = adw::Clamp::builder()
            .maximum_size(864)
            .tightening_threshold(600)
            .build();
        clamp.add_css_class("view-clamp");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&clamp)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        {
            let title_revealer = title_revealer.clone();
            scroller
                .vadjustment()
                .connect_value_changed(move |a| title_revealer.set_reveal_child(a.value() > 48.0));
        }
        scroll_while_dragging(&scroller);
        // A click on the column's background closes the open task, as a
        // click outside Planify's card does. Rows, buttons and fields do
        // their own thing (another row opens instead).
        {
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let tx = tx.clone();
            click.connect_pressed(move |g, _, x, y| {
                if g.widget()
                    .and_then(|w| w.pick(x, y, gtk::PickFlags::DEFAULT))
                    .is_some_and(|target| on_background(&target))
                {
                    tx.emit(Msg::CloseEditor);
                }
            });
            scroller.add_controller(click);
        }

        let error_page = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title("Can't reach asstd")
            .build();
        {
            let retry = gtk::Button::with_label("Try Again");
            retry.add_css_class("pill");
            retry.set_halign(gtk::Align::Center);
            let tx = tx.clone();
            retry.connect_clicked(move |_| tx.emit(Msg::Refresh));
            error_page.set_child(Some(&retry));
        }
        let (signin_page, signin) = sign_in_page(&tx);
        let loading = adw::StatusPage::new();
        loading.set_child(Some(&adw::Spinner::new()));

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&scroller, Some("tasks"));
        stack.add_named(&error_page, Some("error"));
        stack.add_named(&signin_page, Some("signin"));

        let editor = Editor::new(tx.clone());

        // -- selection bar
        let select_bar = gtk::ActionBar::new();
        select_bar.add_css_class("select-bar");
        let select_label = gtk::Label::new(Some("0 selected"));
        select_label.add_css_class("heading");
        select_bar.set_center_widget(Some(&select_label));
        {
            let complete = gtk::Button::from_icon_name("check-round-outline-symbolic");
            complete.set_tooltip_text(Some("Complete"));
            let tx2 = tx.clone();
            complete.connect_clicked(move |_| tx2.emit(Msg::Batch(Batch::Complete)));
            select_bar.pack_start(&complete);

            let date = gtk::MenuButton::builder()
                .icon_name("month-symbolic")
                .tooltip_text("Date")
                .build();
            let tx2 = tx.clone();
            let sunday_first = prefs.sunday_first;
            date.set_create_popup_func(move |mb| {
                let tx2 = tx2.clone();
                mb.set_popover(Some(&pickers::date_popover(
                    Default::default(),
                    DateOpts {
                        sunday_first,
                        full: false,
                        clear: false,
                    },
                    move |s| {
                        if let Some(d) = s.due {
                            tx2.emit(Msg::Batch(Batch::Date(d.local_date(pickers::zone()))));
                        }
                    },
                )));
            });
            select_bar.pack_start(&date);

            let priority = gtk::MenuButton::builder()
                .icon_name("flag-outline-thick-symbolic")
                .tooltip_text("Priority")
                .build();
            let tx2 = tx.clone();
            priority.set_create_popup_func(move |mb| {
                let tx2 = tx2.clone();
                mb.set_popover(Some(&pickers::priority_popover(0, move |l| {
                    tx2.emit(Msg::Batch(Batch::Priority(l)))
                })));
            });
            select_bar.pack_start(&priority);

            let move_to = gtk::MenuButton::builder()
                .icon_name("arrow3-right-symbolic")
                .tooltip_text("Move to a list")
                .build();
            let tx2 = tx.clone();
            move_to.set_create_popup_func(move |mb| {
                let tx2 = tx2.clone();
                let lists = CURRENT_LISTS.with(|l| l.borrow().clone());
                mb.set_popover(Some(&pickers::list_popover(&lists, None, move |to| {
                    tx2.emit(Msg::Batch(Batch::Move(to)))
                })));
            });
            select_bar.pack_start(&move_to);

            let copy = gtk::Button::from_icon_name("clipboard-symbolic");
            copy.set_tooltip_text(Some("Copy to clipboard"));
            let tx2 = tx.clone();
            copy.connect_clicked(move |_| tx2.emit(Msg::Batch(Batch::Copy)));
            select_bar.pack_start(&copy);

            let delete = gtk::Button::from_icon_name("user-trash-symbolic");
            delete.set_tooltip_text(Some("Delete"));
            delete.add_css_class("destructive-action");
            let tx2 = tx.clone();
            delete.connect_clicked(move |_| tx2.emit(Msg::Batch(Batch::Delete)));
            select_bar.pack_end(&delete);

            let done = gtk::Button::with_label("Done");
            let tx2 = tx.clone();
            done.connect_clicked(move |_| tx2.emit(Msg::SelectMode(false)));
            select_bar.pack_end(&done);
        }
        let select_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .child(&select_bar)
            .build();

        let main_view = adw::ToolbarView::new();
        main_view.add_top_bar(&header);
        main_view.set_content(Some(&stack));
        main_view.add_bottom_bar(&select_revealer);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&main_view));

        let outer = adw::OverlaySplitView::builder()
            .sidebar(&sidebar.root)
            .content(&toasts)
            .min_sidebar_width(260.0)
            .max_sidebar_width(300.0)
            .build();
        window.set_content(Some(&outer));

        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            720.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&outer, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);

        let add_card = AddCard::for_window(tx.clone());
        add_card.set_defaults(prefs.default_priority, prefs.read_dates);

        // View options: sort, show completed, priorities.
        {
            let tx = tx.clone();
            view_button.set_create_popup_func(move |mb| {
                let tx = tx.clone();
                mb.set_popover(Some(&view_menu(tx)));
            });
        }
        {
            let tx = tx.clone();
            more_button.set_create_popup_func(move |mb| {
                let tx = tx.clone();
                mb.set_popover(Some(&more_menu(tx)));
            });
        }

        // -- keys
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let tx = tx.clone();
            let window = window.clone();
            keys.connect_key_pressed(move |_, key, _, state| key_action(&window, key, state, &tx));
        }
        window.add_controller(keys);

        {
            let tx = tx.clone();
            window.connect_is_active_notify(move |w| {
                if w.is_active() {
                    tx.emit(Msg::Focused);
                }
            });
        }
        if crate::devel::enabled() {
            crate::devel::install(&window, tx.clone());
        }
        {
            let tx = tx.clone();
            adw::StyleManager::default().connect_dark_notify(move |_| tx.emit(Msg::ThemeChanged));
        }
        {
            // On the bus, for a reminder's notification (and anything else)
            // to show a task.
            let open = gio::SimpleAction::new("open-task", Some(glib::VariantTy::STRING));
            let tx = tx.clone();
            open.connect_activate(move |_, id| {
                if let Some(id) = id.and_then(|v| v.str()) {
                    tx.emit(Msg::OpenTask(id.to_string()));
                }
            });
            relm4::main_application().add_action(&open);
        }
        let quitting = Rc::new(Cell::new(false));
        {
            let tx = tx.clone();
            let quitting = quitting.clone();
            window.connect_close_request(move |_| {
                if quitting.get() {
                    return glib::Propagation::Proceed;
                }
                tx.emit(Msg::CloseWindow);
                glib::Propagation::Stop
            });
        }
        let tray = prefs
            .background
            .then(|| Tray::start(tx.clone(), tray::State::default()));
        {
            let tx = tx.clone();
            glib::timeout_add_seconds_local(60, move || {
                tx.emit(Msg::Tick);
                glib::ControlFlow::Continue
            });
        }
        {
            let tx = tx.clone();
            ui::when_popovers_close(move || tx.emit(Msg::Resume));
        }

        // Follow the daemon: Changed, StatusChanged, LoginDone, restarts.
        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    loop {
                        if let Ok(proxy) = client::proxy().await
                            && let (Ok(mut changed), Ok(mut status), Ok(mut login), Ok(mut owner)) = (
                                proxy.receive_changed().await,
                                proxy.receive_status_changed().await,
                                proxy.receive_login_done().await,
                                proxy.inner().receive_owner_changed().await,
                            )
                        {
                            loop {
                                tokio::select! {
                                    Some(_) = changed.next() => { out.emit(Cmd::Changed); }
                                    Some(s) = status.next() => {
                                        if let Some(v) = s.args().ok().and_then(|a| serde_json::from_str(a.status).ok()) {
                                            out.emit(Cmd::Status(v));
                                        }
                                    }
                                    Some(l) = login.next() => {
                                        if let Ok(a) = l.args() {
                                            out.emit(Cmd::LoginDone(a.ok, a.message.to_string()));
                                        }
                                    }
                                    Some(_) = owner.next() => { out.emit(Cmd::Changed); }
                                    else => break,
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                })
                .drop_on_shutdown()
        });

        let nav = Nav::from_key(&prefs.view).unwrap_or(Nav::Today);
        let mut model = Window {
            nav,
            lists: Vec::new(),
            open: Vec::new(),
            completed: None,
            status: None,
            settings: None,
            error: None,
            loaded: false,
            prefs,
            zone: pickers::zone(),
            cursor: None,
            editing: None,
            expand: false,
            folding: None,
            refocus: Refocus::Keep,
            adding: None,
            add_folding: false,
            select_mode: false,
            selected: HashSet::new(),
            completing: HashMap::new(),
            rolled: HashSet::new(),
            built: Built::default(),
            settle_until: None,
            serial: 0,
            hidden: HashSet::new(),
            load_generation: 0,
            completed_generation: 0,
            dirty: false,
            shown: 0,
            pending_d: None,
            last_focus_sync: None,
            signing_in: None,
            sign_in_error: None,
        };
        let widgets = Widgets {
            window: window.clone(),
            toasts,
            outer,
            sidebar,
            header_title,
            stack,
            clamp,
            rows: Vec::new(),
            editor,
            status,
            add_card,
            select_revealer,
            select_label,
            error_page,
            signin,
            view_button,
            more_button,
            tray,
            quitting,
        };
        model.reload(&sender);
        sender.oneshot_command(async { Cmd::Settings(client::settings().await) });
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        w: &mut Widgets,
        msg: Msg,
        sender: ComponentSender<Self>,
        _root: &adw::ApplicationWindow,
    ) {
        let tx: Tx = sender.input_sender().clone();
        // Not something done in the window: a rebuild for it can wait out
        // an animation.
        let passive = matches!(msg, Msg::Resume | Msg::Tick | Msg::Focused);
        match msg {
            Msg::Navigate(nav) => {
                if nav != self.nav {
                    self.nav = nav;
                    self.prefs.view = self.nav.key();
                    self.prefs.save();
                    self.adding = None;
                    self.cursor = None;
                    if self.select_mode {
                        self.select_mode = false;
                        self.selected.clear();
                    }
                    if self.needs_completed() && self.completed.is_none() {
                        self.reload_completed(&sender);
                    }
                    self.close_editor(w, &sender);
                }
                if w.outer.is_collapsed() {
                    w.outer.set_show_sidebar(false);
                }
                w.clamp.grab_focus();
            }
            Msg::ToggleSidebar => w.outer.set_show_sidebar(!w.outer.shows_sidebar()),
            Msg::Refresh => self.reload(&sender),
            Msg::Tick => {}
            Msg::Focused => {
                let stale = self
                    .last_focus_sync
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(30));
                if stale && self.loaded {
                    self.last_focus_sync = Some(Instant::now());
                    sender.oneshot_command(async move {
                        let _ = client::sync().await;
                        Cmd::Changed
                    });
                }
            }
            Msg::SyncNow => {
                sender.oneshot_command(async move {
                    match client::sync().await {
                        Ok(()) => Cmd::Changed,
                        Err(e) => Cmd::Toast(Err(e)),
                    }
                });
            }
            Msg::Activate(href) => {
                if self.select_mode {
                    self.toggle_selected(&href);
                } else {
                    self.open_editor(w, &sender, href);
                }
            }
            Msg::Open(href) => self.open_editor(w, &sender, href),
            Msg::CloseEditor => self.fold_editor(w, &sender),
            Msg::Folded(href) => {
                if self.folding.as_ref() == Some(&href) && self.close_editor(w, &sender) {
                    self.refocus = Refocus::Row;
                }
            }
            Msg::Check(href, done) => {
                if done && self.editing.as_deref() == Some(href.as_str()) {
                    self.fold_editor(w, &sender);
                }
                self.check(&sender, href, done)
            }
            Msg::CompleteNow(href, serial) => {
                if matches!(self.completing.get(&href), Some(Completing::Waiting(s)) if *s == serial)
                {
                    self.send_complete(&sender, href);
                }
            }
            Msg::Complete(href) => {
                if self.open.iter().any(|t| t.href == href) && !self.completing.contains_key(&href)
                {
                    if self.editing.as_deref() == Some(href.as_str()) {
                        self.close_editor(w, &sender);
                    }
                    self.send_complete(&sender, href);
                }
            }
            Msg::Reopen(href) => {
                sender.oneshot_command(async move {
                    Cmd::Toast(
                        client::reopen(&href)
                            .await
                            .map(|t| format!("Reopened {}", quoted(&t.task.summary))),
                    )
                });
            }
            Msg::Edit(href, change) => {
                self.apply_typed(&href, &change);
                sender.oneshot_command(async move {
                    Cmd::Toast(client::edit(&href, &change).await.map(|_| String::new()))
                });
            }
            Msg::Reorder(orders) => {
                // Move the rows now; the daemon's copy catches up.
                for (href, n) in &orders {
                    if let Some(t) = self.open.iter_mut().find(|t| &t.href == href) {
                        t.task.sort_order = Some(*n);
                    }
                }
                sender.oneshot_command(async move {
                    for (href, n) in orders {
                        let change = Change {
                            sort_order: Some(Some(n)),
                            ..Change::default()
                        };
                        if let Err(e) = client::edit(&href, &change).await {
                            return Cmd::Toast(Err(e));
                        }
                    }
                    Cmd::Toast(Ok(String::new()))
                });
            }
            Msg::EditMany(edits) => {
                let n = edits.len();
                sender.oneshot_command(async move {
                    for (href, change) in edits {
                        if let Err(e) = client::edit(&href, &change).await {
                            return Cmd::Toast(Err(e));
                        }
                    }
                    Cmd::Toast(Ok(match n {
                        1 => "Moved 1 task".to_string(),
                        n => format!("Moved {n} tasks"),
                    }))
                });
            }
            Msg::Delete(href) => {
                let Some(t) = self.find(&href).cloned() else {
                    return;
                };
                if self.editing.as_deref() == Some(href.as_str()) {
                    self.close_editor(w, &sender);
                    self.refocus = Refocus::Row;
                }
                self.hidden.insert(href.clone());
                let toast = adw::Toast::builder()
                    .title(glib::markup_escape_text(&format!(
                        "Deleted {}",
                        quoted(&t.task.summary)
                    )))
                    .button_label("Undo")
                    .timeout(4)
                    .build();
                {
                    let (tx, h) = (tx.clone(), href.clone());
                    toast.connect_button_clicked(move |_| tx.emit(Msg::Undelete(h.clone())));
                }
                {
                    let (tx, h) = (tx.clone(), href.clone());
                    toast.connect_dismissed(move |_| tx.emit(Msg::DeleteNow(h.clone())));
                }
                w.toasts.add_toast(toast);
            }
            Msg::Undelete(href) => {
                self.hidden.remove(&href);
            }
            Msg::DeleteNow(href) => {
                if self.hidden.contains(&href)
                    && self
                        .open
                        .iter()
                        .chain(self.completed.iter().flatten())
                        .any(|t| t.href == href)
                {
                    sender.oneshot_command(async move {
                        Cmd::Toast(client::delete(&href).await.map(|()| String::new()))
                    });
                }
            }
            Msg::Duplicate(href) => {
                sender.oneshot_command(async move {
                    Cmd::Toast(
                        client::duplicate(&href)
                            .await
                            .map(|t| format!("Duplicated {}", quoted(&t.task.summary))),
                    )
                });
            }
            Msg::Copy(href) => {
                if let Some(t) = self.find(&href) {
                    let mut text = t.task.summary.clone();
                    if let Some(d) = &t.task.description {
                        text = format!("{text}\n\n{d}");
                    }
                    w.window.clipboard().set_text(&text);
                    toast(w, "Copied to the clipboard");
                }
            }
            Msg::Move(href, list) => {
                let Some(t) = self.find(&href) else { return };
                if t.list == list {
                    return;
                }
                let name = self
                    .lists
                    .iter()
                    .find(|l| l.href == list)
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                let summary = t.task.summary.clone();
                sender.oneshot_command(async move {
                    let change = Change {
                        list: Some(list),
                        ..Change::default()
                    };
                    Cmd::Toast(
                        client::edit(&href, &change)
                            .await
                            .map(|_| format!("Moved {} to {name}", quoted(&summary))),
                    )
                });
            }
            Msg::DropTask(href, nav) => {
                let Some(t) = self.find(&href).cloned() else {
                    return;
                };
                let today = self.now().date_naive();
                match nav {
                    Nav::List(list) => sender.input(Msg::Move(href, list)),
                    Nav::Inbox => {
                        if let Some(inbox) = model::inbox(&self.lists) {
                            sender.input(Msg::Move(href, inbox.href.clone()));
                        }
                    }
                    Nav::Today | Nav::Tomorrow => {
                        let date = if nav == Nav::Today {
                            today
                        } else {
                            today + Days::days(1)
                        };
                        let due = model::on_date(t.task.due.as_ref(), date, self.zone);
                        sender.input(Msg::Edit(
                            href,
                            Box::new(Change {
                                due: Some(Some(due)),
                                ..Change::default()
                            }),
                        ));
                    }
                    Nav::Completed => sender.input(Msg::Complete(href)),
                    _ => {}
                }
            }
            Msg::Add(spec) => {
                sender.oneshot_command(async move { Cmd::Added(client::add(&spec).await) });
            }
            Msg::ShowAdd(target) => {
                self.show_add(w, &sender, target);
                return;
            }
            Msg::AddText(text) => {
                self.show_add(w, &sender, None);
                w.add_card.fill(&text);
                return;
            }
            Msg::Paste => {
                let clipboard = w.window.clipboard();
                glib::spawn_future_local(async move {
                    if let Ok(Some(text)) = clipboard.read_text_future().await
                        && !text.trim().is_empty()
                    {
                        tx.emit(Msg::AddText(text.to_string()));
                    }
                });
            }
            Msg::HideAdd => {
                w.clamp.grab_focus();
                let card = w
                    .add_card
                    .root
                    .parent()
                    .and_downcast::<gtk::Revealer>()
                    .filter(|r| r.is_mapped());
                let wait = motion::lasts(motion::ROW_MS);
                match card {
                    Some(card) if self.adding.is_some() && !wait.is_zero() => {
                        card.set_reveal_child(false);
                        self.add_folding = true;
                        self.hold(&sender, wait);
                        glib::timeout_add_local_once(wait, move || tx.emit(Msg::AddFolded));
                    }
                    _ => self.adding = None,
                }
            }
            Msg::AddFolded => {
                if std::mem::take(&mut self.add_folding) {
                    self.adding = None;
                }
            }
            Msg::SelectMode(on) => {
                self.select_mode = on;
                if on {
                    self.close_editor(w, &sender);
                } else {
                    self.selected.clear();
                }
            }
            Msg::SelectToggle(href) => {
                self.close_editor(w, &sender);
                self.select_mode = true;
                self.toggle_selected(&href);
            }
            Msg::Batch(b) => self.batch(w, &sender, b),
            Msg::SetViewOpts(opts) => {
                self.prefs.views.insert(self.nav.key(), opts);
                self.prefs.save();
                if self.needs_completed() && self.completed.is_none() {
                    self.reload_completed(&sender);
                }
            }
            Msg::NewList => {
                let tx = tx.clone();
                dialogs::list_dialog(&w.window, None, move |name, color| {
                    tx.emit(Msg::SaveList(None, name, color))
                });
            }
            Msg::EditList(href) => {
                if let Some(l) = self.lists.iter().find(|l| l.href == href) {
                    let tx = tx.clone();
                    dialogs::list_dialog(&w.window, Some(l), move |name, color| {
                        tx.emit(Msg::SaveList(Some(href.clone()), name, color))
                    });
                }
            }
            Msg::SaveList(href, name, color) => {
                sender.oneshot_command(async move {
                    Cmd::ListSaved(match href {
                        None => {
                            client::create_list(&ListSpec {
                                name,
                                color: Some(color),
                            })
                            .await
                        }
                        Some(h) => {
                            client::update_list(
                                &h,
                                &ListChange {
                                    name: Some(name),
                                    color: Some(color),
                                },
                            )
                            .await
                        }
                    })
                });
            }
            Msg::AskDeleteList(href) => {
                if let Some(l) = self.lists.iter().find(|l| l.href == href) {
                    let open = self.open.iter().filter(|t| t.list == href).count();
                    let tx = tx.clone();
                    dialogs::delete_list_dialog(&w.window, l, open, move || {
                        tx.emit(Msg::DeleteList(href.clone()))
                    });
                }
            }
            Msg::DeleteList(href) => {
                let name = self
                    .lists
                    .iter()
                    .find(|l| l.href == href)
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                if self.nav == Nav::List(href.clone()) {
                    self.nav = Nav::Today;
                }
                sender.oneshot_command(async move {
                    Cmd::Toast(
                        client::delete_list(&href)
                            .await
                            .map(|()| format!("Deleted {}", quoted(&name))),
                    )
                });
            }
            Msg::AskDeleteCompleted(list) => {
                let (n, from) = match &list {
                    Some(href) => match self.lists.iter().find(|l| &l.href == href) {
                        Some(l) => (l.done, quoted(&l.name)),
                        None => return,
                    },
                    None => (
                        self.lists
                            .iter()
                            .filter(|l| l.writable)
                            .map(|l| l.done)
                            .sum(),
                        "every list".to_string(),
                    ),
                };
                if n == 0 {
                    toast(w, "Nothing completed to delete");
                } else {
                    dialogs::delete_completed_dialog(&w.window, n, &from, move || {
                        tx.emit(Msg::DeleteCompleted(list.clone()))
                    });
                }
            }
            Msg::DeleteCompleted(list) => {
                sender.oneshot_command(async move {
                    Cmd::Toast(
                        client::delete_completed(list.as_deref())
                            .await
                            .map(|n| match n {
                                0 => String::new(),
                                1 => "Deleted 1 completed task".into(),
                                n => format!("Deleted {n} completed tasks"),
                            }),
                    )
                });
            }
            Msg::ReorderLists(order) => {
                self.prefs.list_order = order;
                self.prefs.save();
            }
            Msg::Archive(href, on) => {
                let Some(name) = self
                    .lists
                    .iter()
                    .find(|l| l.href == href)
                    .map(|l| l.name.clone())
                else {
                    return;
                };
                self.prefs.archived.retain(|h| *h != href);
                if on {
                    self.prefs.archived.push(href.clone());
                }
                self.prefs.save();
                let undo = adw::Toast::builder()
                    .title(glib::markup_escape_text(&format!(
                        "{} {}",
                        if on { "Archived" } else { "Unarchived" },
                        quoted(&name)
                    )))
                    .button_label("Undo")
                    .timeout(4)
                    .build();
                undo.connect_button_clicked(move |_| tx.emit(Msg::Archive(href.clone(), !on)));
                w.toasts.add_toast(undo);
            }
            Msg::CopyList(href) => {
                if let Some(l) = self.lists.iter().find(|l| l.href == href) {
                    let nav = Nav::List(href.clone());
                    let sort = self.prefs.view_opts(&nav).sort_for(&nav);
                    let tasks = model::sorted(
                        self.visible_open()
                            .into_iter()
                            .filter(|t| t.list == href)
                            .collect(),
                        sort,
                        self.zone,
                    );
                    let text = format!(
                        "## {}\n\n{}",
                        l.name,
                        model::tasks_markdown(&tasks, self.now())
                    );
                    w.window.clipboard().set_text(&text);
                    toast(w, &format!("Copied {} as Markdown", quoted(&l.name)));
                }
            }
            Msg::SaveIcs(href) => {
                sender.oneshot_command(async move {
                    let ics = client::ics(&href).await;
                    Cmd::Ics(href, ics)
                });
            }
            Msg::PlaceSearch(query) => {
                sender.oneshot_command(async move {
                    Cmd::Places(
                        asst_core::nominatim::search(&query)
                            .await
                            .map_err(|e| e.to_string()),
                    )
                });
            }
            Msg::OpenTask(id) => {
                w.window.present();
                sender.input(Msg::Reveal(id));
            }
            Msg::ThemeChanged => {
                // GTK draws a text view shown for the first time after a
                // switch between light and dark in the old text color, as
                // the closed editor's notes would be: build it again instead.
                if self.editing.is_none() {
                    w.editor = Editor::new(tx.clone());
                    self.shown = 0;
                }
            }
            Msg::QuickFind => {
                find::open(
                    &w.window,
                    &self.lists,
                    &self.visible_open(),
                    &self.prefs,
                    "",
                    tx.clone(),
                );
            }
            Msg::Reveal(href) => {
                if let Some(t) = self.find(&href).cloned() {
                    self.close_editor(w, &sender);
                    self.nav = Nav::List(t.list.clone());
                    self.prefs.view = self.nav.key();
                    self.adding = None;
                    if !t.task.is_open() {
                        let mut opts = self.prefs.view_opts(&self.nav);
                        opts.show_completed = true;
                        self.prefs.views.insert(self.nav.key(), opts);
                    }
                    self.prefs.save();
                    self.open_editor(w, &sender, href);
                } else {
                    sender.oneshot_command(async move { Cmd::Opened(client::get(&href).await) });
                }
            }
            Msg::Preferences(page) => {
                settings::open(
                    &w.window,
                    settings::Ctx {
                        settings: self.settings.clone(),
                        lists: self.lists.clone(),
                        status: w.status.clone(),
                        prefs: self.prefs.clone(),
                        page,
                    },
                    tx.clone(),
                );
            }
            Msg::Shortcuts => dialogs::shortcuts(&w.window),
            Msg::About => dialogs::about(&w.window),
            Msg::SetPrefs(prefs) => {
                self.prefs = *prefs;
                self.prefs.save();
                self.shown = 0;
                w.add_card
                    .set_defaults(self.prefs.default_priority, self.prefs.read_dates);
                if self.prefs.background != w.tray.is_some() {
                    w.tray = self
                        .prefs
                        .background
                        .then(|| Tray::start(tx.clone(), tray::State::default()));
                }
            }
            Msg::HideView(nav) => {
                let before = self.prefs.sidebar_views();
                let mut views = before.clone();
                views.retain(|n| *n != nav);
                self.prefs.set_sidebar_views(&views);
                self.prefs.save();
                let undo = adw::Toast::builder()
                    .title(format!("Took {} out of the sidebar", nav.title(&[])))
                    .button_label("Undo")
                    .timeout(4)
                    .build();
                let tx = tx.clone();
                undo.connect_button_clicked(move |_| tx.emit(Msg::SetSidebar(before.clone())));
                w.toasts.add_toast(undo);
            }
            Msg::SetSidebar(views) => {
                self.prefs.set_sidebar_views(&views);
                self.prefs.save();
            }
            Msg::SetSettings(change) => {
                sender.oneshot_command(async move {
                    Cmd::Settings(client::set_settings(&change).await)
                });
            }
            Msg::SignIn(server) => {
                self.sign_in_error = None;
                self.signing_in = Some(String::new());
                sender.oneshot_command(async move { Cmd::LoginUrl(client::login(&server).await) });
            }
            Msg::CancelSignIn => {
                self.signing_in = None;
            }
            Msg::SignOut => {
                sender.oneshot_command(async move {
                    Cmd::Toast(client::logout().await.map(|()| "Signed out".to_string()))
                });
            }
            Msg::Resume => {
                if !self.dirty {
                    return;
                }
            }
            Msg::Key(key) => self.key(w, key, &sender),
            Msg::Drive(command) => self.drive(w, &sender, &command),
            Msg::CloseWindow => {
                if self.prefs.background && w.tray.as_ref().is_some_and(|t| t.online()) {
                    self.hide(w, &sender);
                } else {
                    sender.input(Msg::Quit);
                }
            }
            Msg::Quit => {
                self.remember_size(w);
                let waiting = w.editor.has_pending();
                self.close_editor(w, &sender);
                let deletes: Vec<String> = self.hidden.iter().cloned().collect();
                sender.oneshot_command(async move {
                    for href in &deletes {
                        let _ = client::delete(href).await;
                    }
                    if waiting || !deletes.is_empty() {
                        // Let edits sent a moment ago land before the loop goes.
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    Cmd::Closed
                });
                return;
            }
            Msg::ShowWindow => w.window.present(),
            Msg::TrayClick => {
                if w.window.is_visible() && w.window.is_active() {
                    self.hide(w, &sender);
                } else {
                    w.window.present();
                }
            }
        }
        self.render(w, &sender, passive);
    }

    fn update_cmd_with_view(
        &mut self,
        w: &mut Widgets,
        cmd: Cmd,
        sender: ComponentSender<Self>,
        _root: &adw::ApplicationWindow,
    ) {
        let tx: Tx = sender.input_sender().clone();
        match cmd {
            Cmd::Loaded(generation, result) => {
                if generation != self.load_generation {
                    return;
                }
                match result {
                    Ok(l) => {
                        self.error = None;
                        self.loaded = true;
                        self.lists = l.lists;
                        self.open = l.open;
                        if let Some(Settings { .. }) = &self.settings {}
                        self.status = Some(l.status);
                        self.settle();
                        w.add_card.set_lists(&self.lists, self.prefs.sunday_first);
                        if let Nav::List(href) = &self.nav
                            && !self.lists.iter().any(|x| &x.href == href)
                        {
                            self.nav = Nav::Today;
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Cmd::CompletedLoaded(generation, result) => {
                if generation != self.completed_generation {
                    return;
                }
                match result {
                    Ok(c) => {
                        self.completed = Some(c);
                        self.settle();
                    }
                    Err(e) => toast(w, &e),
                }
            }
            Cmd::Changed => {
                self.reload(&sender);
                return;
            }
            Cmd::Status(s) => {
                let was_signed_out = self
                    .status
                    .as_ref()
                    .is_some_and(|x| x.state == SyncState::NoAccount);
                if was_signed_out && s.state != SyncState::NoAccount {
                    self.reload(&sender);
                }
                self.status = Some(s);
            }
            Cmd::LoginDone(ok, message) => {
                self.signing_in = None;
                if ok {
                    toast(w, "Signed in");
                    self.sign_in_error = None;
                    self.reload(&sender);
                    sender.oneshot_command(async { Cmd::Settings(client::settings().await) });
                } else {
                    self.sign_in_error = Some(message);
                }
            }
            Cmd::LoginUrl(result) => match result {
                Ok(url) => {
                    self.signing_in = Some(url.clone());
                    gtk::UriLauncher::new(&url).launch(
                        Some(&w.window),
                        None::<&gio::Cancellable>,
                        |_| {},
                    );
                }
                Err(e) => {
                    self.signing_in = None;
                    self.sign_in_error = Some(e);
                }
            },
            Cmd::Settings(result) => match result {
                Ok(s) => {
                    self.settings = Some(s);
                }
                Err(e) => {
                    if self.settings.is_some() {
                        toast(w, &e);
                    }
                }
            },
            Cmd::Toast(result) => match result {
                Ok(text) if !text.is_empty() => toast(w, &text),
                Ok(_) => {}
                Err(e) => {
                    toast(w, &e);
                    self.reload(&sender);
                }
            },
            Cmd::Completed(href, result) => match result {
                Ok(t) if t.task.is_open() => {
                    let next = t
                        .task
                        .due
                        .as_ref()
                        .map(|d| fmt::due_label(d, self.now()))
                        .unwrap_or_default();
                    toast(
                        w,
                        &format!("Completed {} · next {next}", quoted(&t.task.summary)),
                    );
                }
                Ok(t) => {
                    let toast_widget = adw::Toast::builder()
                        .title(glib::markup_escape_text(&format!(
                            "Completed {}",
                            quoted(&t.task.summary)
                        )))
                        .button_label("Undo")
                        .timeout(4)
                        .build();
                    let href = t.href.clone();
                    toast_widget
                        .connect_button_clicked(move |_| tx.emit(Msg::Reopen(href.clone())));
                    w.toasts.add_toast(toast_widget);
                }
                Err(e) => {
                    self.completing.remove(&href);
                    toast(w, &e);
                }
            },
            Cmd::Added(result) => match result {
                Ok(a) if a.existed => toast(
                    w,
                    &format!("Already there: {}", quoted(&a.task.task.summary)),
                ),
                Ok(a) => {
                    // Show where it went when that isn't this view.
                    let t = &a.task;
                    let here = match &self.nav {
                        Nav::List(h) => &t.list == h,
                        Nav::Inbox => model::inbox(&self.lists).is_some_and(|l| l.href == t.list),
                        Nav::Today => t
                            .task
                            .due
                            .as_ref()
                            .is_some_and(|d| d.local_date(self.zone) <= self.now().date_naive()),
                        _ => true,
                    };
                    if !here {
                        let where_ = match &t.task.due {
                            Some(d) => {
                                format!("{}, {}", t.list_name, fmt::due_label(d, self.now()))
                            }
                            None => t.list_name.clone(),
                        };
                        toast(w, &format!("Added to {where_}"));
                    }
                }
                Err(e) => toast(w, &e),
            },
            Cmd::Opened(result) => match result {
                Ok(t) => {
                    let href = t.href.clone();
                    if !self
                        .open
                        .iter()
                        .chain(self.completed.iter().flatten())
                        .any(|x| x.href == href)
                    {
                        if t.task.is_open() {
                            self.open.push(t.clone());
                        } else if let Some(c) = self.completed.as_mut() {
                            c.push(t.clone());
                        }
                    }
                    if !self.in_view(&href) {
                        self.close_editor(w, &sender);
                        self.nav = Nav::List(t.list.clone());
                        self.prefs.view = self.nav.key();
                        if !t.task.is_open() {
                            let mut opts = self.prefs.view_opts(&self.nav);
                            opts.show_completed = true;
                            self.prefs.views.insert(self.nav.key(), opts);
                        }
                        self.prefs.save();
                    }
                    self.open_editor(w, &sender, href);
                }
                Err(e) => toast(w, &e),
            },
            Cmd::ListSaved(result) => match result {
                Ok(l) => {
                    if !self.lists.iter().any(|x| x.href == l.href) {
                        self.lists.push(l.clone());
                        self.nav = Nav::List(l.href.clone());
                    }
                    self.reload(&sender);
                }
                Err(e) => toast(w, &e),
            },
            Cmd::Places(result) => match result {
                Ok(places) => w.editor.show_places(places),
                Err(e) => toast(w, &e),
            },
            Cmd::Ics(href, result) => match result {
                Ok(ics) => {
                    let name = self
                        .find(&href)
                        .map_or_else(|| "task".to_string(), |t| file_name(&t.task.summary));
                    let filter = gtk::FileFilter::new();
                    filter.set_name(Some("iCalendar"));
                    filter.add_pattern("*.ics");
                    let filters = gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    let dialog = gtk::FileDialog::builder()
                        .title("Save as iCalendar")
                        .initial_name(format!("{name}.ics"))
                        .filters(&filters)
                        .build();
                    let sender = sender.clone();
                    dialog.save(Some(&w.window), None::<&gio::Cancellable>, move |file| {
                        let Some(path) = file.ok().and_then(|f| f.path()) else {
                            return;
                        };
                        let said = std::fs::write(&path, ics.as_bytes())
                            .map(|()| format!("Saved {}", path.display()))
                            .map_err(|e| format!("Couldn't save {}: {e}", path.display()));
                        sender.oneshot_command(async move { Cmd::Toast(said) });
                    });
                }
                Err(e) => toast(w, &e),
            },
            Cmd::Closed => {
                self.hidden.clear();
                w.quitting.set(true);
                w.tray = None;
                w.window.close();
                relm4::main_application().quit();
                return;
            }
        }
        self.render(w, &sender, true);
    }
}

impl Window {
    fn now(&self) -> DateTime<Tz> {
        Utc::now().with_timezone(&self.zone)
    }

    /// What the views are made of: the lists, the sidebar's in its order
    /// first and archived ones after, and tasks not deleted a moment ago
    /// nor in an archived list, unless it is the list on view.
    fn view_data(&self) -> (Vec<ListView>, Vec<TaskView>, Option<Vec<TaskView>>) {
        let mut lists = self.prefs.arrange(&self.lists);
        lists.extend(
            self.lists
                .iter()
                .filter(|l| self.prefs.is_archived(&l.href))
                .cloned(),
        );
        let kept = |t: &&TaskView| {
            !self.hidden.contains(&t.href)
                && (!self.prefs.is_archived(&t.list) || self.nav == Nav::List(t.list.clone()))
        };
        let open = self.open.iter().filter(kept).cloned().collect();
        let completed = self
            .completed
            .as_ref()
            .map(|c| c.iter().filter(kept).cloned().collect());
        (lists, open, completed)
    }

    /// Open tasks, but for those deleted a moment ago.
    fn visible_open(&self) -> Vec<TaskView> {
        self.open
            .iter()
            .filter(|t| !self.hidden.contains(&t.href))
            .cloned()
            .collect()
    }

    fn find(&self, href: &str) -> Option<&TaskView> {
        self.open
            .iter()
            .chain(self.completed.iter().flatten())
            .find(|t| t.href == href)
    }

    fn in_view(&self, href: &str) -> bool {
        let (lists, open, completed) = self.view_data();
        let data = Data {
            lists: &lists,
            open: &open,
            completed: completed.as_deref(),
            sunday_first: self.prefs.sunday_first,
        };
        model::plan(
            &self.nav,
            &data,
            &self.prefs.view_opts(&self.nav),
            self.now(),
        )
        .iter()
        .any(|s| s.tasks.iter().any(|t| t.href == href))
    }

    fn needs_completed(&self) -> bool {
        self.nav == Nav::Completed || self.prefs.view_opts(&self.nav).show_completed
    }

    fn reload(&mut self, sender: &ComponentSender<Self>) {
        self.load_generation += 1;
        let generation = self.load_generation;
        sender.oneshot_command(async move {
            let result = async {
                Ok(Loaded {
                    lists: client::lists().await?,
                    open: client::tasks(&query(View::Open)).await?,
                    status: client::status().await?,
                })
            }
            .await;
            Cmd::Loaded(generation, result)
        });
        if self.needs_completed() || self.completed.is_some() {
            self.reload_completed(sender);
        }
    }

    fn reload_completed(&mut self, sender: &ComponentSender<Self>) {
        self.completed_generation += 1;
        let generation = self.completed_generation;
        sender.oneshot_command(async move {
            let q = Query {
                limit: Some(500),
                ..query(View::Completed)
            };
            Cmd::CompletedLoaded(generation, client::tasks(&q).await)
        });
    }

    /// Drop bookkeeping the new data has made moot.
    fn settle(&mut self) {
        let open: HashMap<&str, &TaskView> =
            self.open.iter().map(|t| (t.href.as_str(), t)).collect();
        let rolled = &mut self.rolled;
        self.completing.retain(|href, c| match c {
            Completing::Waiting(_) => open.contains_key(href.as_str()),
            Completing::Sent(due) => {
                let now = open.get(href.as_str());
                if now.is_some_and(|t| t.task.due != *due && t.task.is_open()) {
                    // A repeating task, on to its next date.
                    rolled.insert(href.clone());
                }
                now.is_some_and(|t| t.task.due == *due && t.task.is_open())
            }
        });
        let known: HashSet<&str> = self
            .open
            .iter()
            .chain(self.completed.iter().flatten())
            .map(|t| t.href.as_str())
            .collect();
        self.hidden.retain(|h| known.contains(h.as_str()));
        self.selected.retain(|h| known.contains(h.as_str()));
    }

    /// The window's size for next time, while it has one.
    fn remember_size(&mut self, w: &Widgets) {
        if !w.window.is_visible() {
            return;
        }
        self.prefs.maximized = w.window.is_maximized();
        if !self.prefs.maximized {
            let (width, height) = w.window.default_size();
            self.prefs.width = width;
            self.prefs.height = height;
        }
        self.prefs.save();
    }

    /// Into the tray. Typing is saved, and deletes held for undo go through
    /// now, since their toasts go with the window.
    fn hide(&mut self, w: &Widgets, sender: &ComponentSender<Self>) {
        self.remember_size(w);
        self.close_editor(w, sender);
        w.toasts.dismiss_all();
        w.window.set_visible(false);
    }

    /// The add card, open where `target` says (or where this view adds).
    fn show_add(
        &mut self,
        w: &mut Widgets,
        sender: &ComponentSender<Self>,
        target: Option<Target>,
    ) {
        self.close_editor(w, sender);
        let target = target.unwrap_or_else(|| self.default_target());
        if self.adding.as_ref() != Some(&target) {
            w.add_card.open(target.clone());
        }
        self.adding = Some(target);
        self.add_folding = false;
        self.shown = 0;
        self.render(w, sender, false);
        w.add_card.focus();
    }

    fn default_target(&self) -> Target {
        let today = self.now().date_naive();
        match &self.nav {
            Nav::List(h) => Target::List(h.clone()),
            Nav::Inbox => {
                model::inbox(&self.lists).map_or(Target::Anywhere, |l| Target::List(l.href.clone()))
            }
            Nav::Today | Nav::Scheduled => Target::Day(today),
            Nav::Tomorrow => Target::Day(today + Days::days(1)),
            _ => Target::Anywhere,
        }
    }

    /// Open a task in place, closing the one open before.
    fn open_editor(&mut self, w: &Widgets, sender: &ComponentSender<Self>, href: String) {
        self.cursor = Some(href.clone());
        if self.editing.as_deref() == Some(href.as_str()) {
            // Opened again while folding: grow back.
            if self.folding.take().is_some() {
                w.editor.expand();
            }
            return;
        }
        let Some(t) = self.find(&href).cloned() else {
            return;
        };
        if self.close_editor(w, sender) && w.editor.typing() {
            // Still in the last task's title: let go, or the view would hold
            // off moving the editor to this one.
            gtk::prelude::GtkWindowExt::set_focus(&w.window, None::<&gtk::Widget>);
        }
        w.editor.fold();
        w.editor.show(&t, &self.lists, self.prefs.sunday_first);
        self.editing = Some(href);
        self.expand = true;
        self.refocus = Refocus::Editor;
        self.shown = 0;
    }

    /// Close the open task the way it opened: the card shrinks to a row,
    /// then the row takes its place.
    fn fold_editor(&mut self, w: &Widgets, sender: &ComponentSender<Self>) {
        let Some(href) = self.editing.clone() else {
            return;
        };
        if self.folding.is_some() {
            return;
        }
        let wait = w.editor.collapse();
        if wait.is_zero() {
            if self.close_editor(w, sender) {
                self.refocus = Refocus::Row;
            }
            return;
        }
        self.folding = Some(href.clone());
        let tx = sender.input_sender().clone();
        glib::timeout_add_local_once(wait, move || tx.emit(Msg::Folded(href)));
    }

    /// Rebuilds for the daemon's news wait `d`, while something animates.
    fn hold(&mut self, sender: &ComponentSender<Self>, d: Duration) {
        let until = Instant::now() + d;
        if d.is_zero() || self.settle_until.is_some_and(|u| u >= until) {
            return;
        }
        self.settle_until = Some(until);
        let tx = sender.input_sender().clone();
        glib::timeout_add_local_once(d, move || tx.emit(Msg::Resume));
    }

    /// Close the task open in place, keeping what was typed. Whether one was.
    fn close_editor(&mut self, w: &Widgets, sender: &ComponentSender<Self>) -> bool {
        self.folding = None;
        self.expand = false;
        let Some(href) = self.editing.take() else {
            return false;
        };
        for (h, change) in w.editor.close() {
            // Into the row now, to the daemon next.
            self.apply_typed(&h, &change);
            sender.input(Msg::Edit(h, Box::new(change)));
        }
        self.cursor = Some(href);
        self.shown = 0;
        true
    }

    /// Typed text into the copy rows are drawn from, ahead of the daemon's
    /// answer: a row rebuilt in between would flash the old words.
    fn apply_typed(&mut self, href: &str, change: &Change) {
        let Some(t) = self
            .open
            .iter_mut()
            .chain(self.completed.iter_mut().flatten())
            .find(|t| t.href == href)
        else {
            return;
        };
        if let Some(s) = &change.summary {
            t.task.summary = s.clone();
        }
        if let Some(d) = &change.description {
            t.task.description = d.clone();
        }
    }

    fn toggle_selected(&mut self, href: &str) {
        if !self.selected.remove(href) {
            self.selected.insert(href.to_string());
        }
    }

    fn check(&mut self, sender: &ComponentSender<Self>, href: String, done: bool) {
        if done {
            if self.completing.contains_key(&href) || !self.open.iter().any(|t| t.href == href) {
                return;
            }
            if self.prefs.complete_later {
                self.serial += 1;
                let serial = self.serial;
                self.completing
                    .insert(href.clone(), Completing::Waiting(serial));
                let tx: Tx = sender.input_sender().clone();
                glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                    tx.emit(Msg::CompleteNow(href, serial))
                });
            } else {
                self.send_complete(sender, href);
            }
        } else if matches!(self.completing.get(&href), Some(Completing::Waiting(_))) {
            self.completing.remove(&href);
        } else if !self.open.iter().any(|t| t.href == href) {
            sender.input(Msg::Reopen(href));
        }
    }

    fn send_complete(&mut self, sender: &ComponentSender<Self>, href: String) {
        let due = self
            .open
            .iter()
            .find(|t| t.href == href)
            .and_then(|t| t.task.due.clone());
        self.completing.insert(href.clone(), Completing::Sent(due));
        sender.oneshot_command(async move {
            let r = client::complete(&href).await;
            Cmd::Completed(href, r)
        });
    }

    fn batch(&mut self, w: &Widgets, sender: &ComponentSender<Self>, b: Batch) {
        let hrefs: Vec<String> = self.selected.iter().cloned().collect();
        if hrefs.is_empty() {
            return;
        }
        let n = hrefs.len();
        let tasks: Vec<TaskView> = hrefs.iter().filter_map(|h| self.find(h).cloned()).collect();
        match b {
            Batch::Delete => {
                for h in &hrefs {
                    self.hidden.insert(h.clone());
                }
                let toast_widget = adw::Toast::builder()
                    .title(if n == 1 {
                        "Deleted 1 task".to_string()
                    } else {
                        format!("Deleted {n} tasks")
                    })
                    .button_label("Undo")
                    .timeout(5)
                    .build();
                let tx: Tx = sender.input_sender().clone();
                {
                    let (tx, hrefs) = (tx.clone(), hrefs.clone());
                    toast_widget.connect_button_clicked(move |_| {
                        for h in &hrefs {
                            tx.emit(Msg::Undelete(h.clone()));
                        }
                    });
                }
                {
                    let hrefs = hrefs.clone();
                    toast_widget.connect_dismissed(move |_| {
                        for h in &hrefs {
                            tx.emit(Msg::DeleteNow(h.clone()));
                        }
                    });
                }
                w.toasts.add_toast(toast_widget);
            }
            Batch::Complete => {
                sender.oneshot_command(async move {
                    for h in hrefs {
                        if let Err(e) = client::complete(&h).await {
                            return Cmd::Toast(Err(e));
                        }
                    }
                    Cmd::Toast(Ok(if n == 1 {
                        "Completed 1 task".into()
                    } else {
                        format!("Completed {n} tasks")
                    }))
                });
            }
            Batch::Priority(level) => {
                let edits: Vec<(String, Change)> = tasks
                    .iter()
                    .map(|t| {
                        (
                            t.href.clone(),
                            Change {
                                priority: Some(level),
                                ..Change::default()
                            },
                        )
                    })
                    .collect();
                sender.oneshot_command(async move {
                    for (h, c) in edits {
                        if let Err(e) = client::edit(&h, &c).await {
                            return Cmd::Toast(Err(e));
                        }
                    }
                    Cmd::Toast(Ok(String::new()))
                });
            }
            Batch::Date(date) => {
                let zone = self.zone;
                let edits = tasks
                    .iter()
                    .map(|t| {
                        (
                            t.href.clone(),
                            Change {
                                due: Some(Some(model::on_date(t.task.due.as_ref(), date, zone))),
                                ..Change::default()
                            },
                        )
                    })
                    .collect();
                sender.input(Msg::EditMany(edits));
            }
            Batch::Move(list) => {
                sender.oneshot_command(async move {
                    for h in hrefs {
                        let c = Change {
                            list: Some(list.clone()),
                            ..Change::default()
                        };
                        if let Err(e) = client::edit(&h, &c).await {
                            return Cmd::Toast(Err(e));
                        }
                    }
                    Cmd::Toast(Ok(if n == 1 {
                        "Moved 1 task".into()
                    } else {
                        format!("Moved {n} tasks")
                    }))
                });
            }
            Batch::Copy => {
                let mut tasks = tasks;
                // As they are on screen, not as they were picked.
                let place = |t: &TaskView| w.rows.iter().position(|(h, _)| *h == t.href);
                tasks.sort_by_key(|t| place(t));
                w.window
                    .clipboard()
                    .set_text(&model::tasks_markdown(&tasks, self.now()));
                toast(
                    w,
                    &if n == 1 {
                        "Copied 1 task".to_string()
                    } else {
                        format!("Copied {n} tasks")
                    },
                );
            }
        }
        self.select_mode = false;
        self.selected.clear();
    }

    /// The view is a list that was archived.
    fn nav_archived(&self) -> bool {
        matches!(&self.nav, Nav::List(h) if self.prefs.is_archived(h))
    }

    /// The list a view shows, for a list or the inbox.
    fn shown_list(&self) -> Option<&ListView> {
        match &self.nav {
            Nav::List(href) => self.lists.iter().find(|l| &l.href == href),
            Nav::Inbox => model::inbox(&self.lists),
            _ => None,
        }
    }

    fn cursor_task(&self) -> Option<TaskView> {
        let href = self.cursor.as_ref()?;
        self.find(href).cloned()
    }

    fn nav_order(&self) -> Vec<Nav> {
        let mut navs = self.prefs.sidebar_views();
        navs.extend(
            self.prefs
                .arrange(&self.lists)
                .into_iter()
                .map(|l| Nav::List(l.href)),
        );
        navs
    }

    fn key(&mut self, w: &mut Widgets, key: Key, sender: &ComponentSender<Self>) {
        if !matches!(key, Key::Delete) {
            self.pending_d = None;
        }
        let index = self
            .cursor
            .as_ref()
            .and_then(|c| w.rows.iter().position(|(h, _)| h == c));
        let count = w.rows.len();
        // Moving on closes the task open in place; the rebuild then puts
        // the keyboard on the row moved to.
        let move_to = |i: usize, this: &mut Window| {
            if let Some((href, row)) = w.rows.get(i) {
                if this.close_editor(w, sender) {
                    this.refocus = Refocus::Row;
                } else {
                    row.grab_focus();
                }
                this.cursor = Some(href.clone());
            }
        };
        match key {
            Key::Down if count > 0 => move_to(index.map_or(0, |i| (i + 1).min(count - 1)), self),
            Key::Up if count > 0 => move_to(index.map_or(0, |i| i.saturating_sub(1)), self),
            Key::First if count > 0 => move_to(0, self),
            Key::Last if count > 0 => move_to(count - 1, self),
            Key::Down | Key::Up | Key::First | Key::Last => {}
            Key::Add => sender.input(Msg::ShowAdd(None)),
            Key::Find => sender.input(Msg::QuickFind),
            Key::Sync => sender.input(Msg::SyncNow),
            Key::NewList => sender.input(Msg::NewList),
            Key::Shortcuts => sender.input(Msg::Shortcuts),
            Key::Preferences => sender.input(Msg::Preferences(None)),
            Key::Sidebar => sender.input(Msg::ToggleSidebar),
            Key::Close => sender.input(Msg::CloseWindow),
            Key::Quit => sender.input(Msg::Quit),
            Key::Paste => sender.input(Msg::Paste),
            Key::Select => sender.input(Msg::SelectMode(!self.select_mode)),
            Key::Escape => {
                if self.select_mode {
                    sender.input(Msg::SelectMode(false));
                } else if self.editing.is_some() {
                    sender.input(Msg::CloseEditor);
                } else if self.adding.is_some() {
                    sender.input(Msg::HideAdd);
                }
            }
            Key::PrevView | Key::NextView => {
                let order = self.nav_order();
                if order.is_empty() {
                    return;
                }
                let at = order.iter().position(|n| *n == self.nav).unwrap_or(0) as i32;
                let step = if matches!(key, Key::NextView) { 1 } else { -1 };
                let next = (at + step).rem_euclid(order.len() as i32) as usize;
                sender.input(Msg::Navigate(order[next].clone()));
            }
            Key::Go(target) => {
                let nav = match target {
                    Nav0::Inbox => Some(Nav::Inbox),
                    Nav0::Today => Some(Nav::Today),
                    Nav0::Scheduled => Some(Nav::Scheduled),
                    Nav0::List(i) => self
                        .prefs
                        .arrange(&self.lists)
                        .get(i)
                        .map(|l| Nav::List(l.href.clone())),
                };
                if let Some(nav) = nav {
                    sender.input(Msg::Navigate(nav));
                }
            }
            Key::Open
            | Key::Toggle
            | Key::Delete
            | Key::Priority(_)
            | Key::Today
            | Key::Tomorrow
            | Key::NextWeek => {
                let Some(t) = self.cursor_task() else { return };
                let today = self.now().date_naive();
                let on = |d: NaiveDate| Change {
                    due: Some(Some(model::on_date(t.task.due.as_ref(), d, self.zone))),
                    ..Change::default()
                };
                match key {
                    Key::Open => sender.input(Msg::Open(t.href)),
                    Key::Toggle => sender.input(Msg::Check(
                        t.href.clone(),
                        t.task.is_open() && !self.completing.contains_key(&t.href),
                    )),
                    Key::Delete => {
                        if self
                            .pending_d
                            .is_some_and(|p| p.elapsed() < Duration::from_millis(800))
                        {
                            self.pending_d = None;
                            if let Some(i) = index {
                                let next = w
                                    .rows
                                    .get(i + 1)
                                    .or_else(|| i.checked_sub(1).and_then(|j| w.rows.get(j)));
                                self.cursor = next.map(|(h, _)| h.clone());
                            }
                            sender.input(Msg::Delete(t.href));
                        } else {
                            self.pending_d = Some(Instant::now());
                        }
                    }
                    Key::Priority(level) => sender.input(Msg::Edit(
                        t.href,
                        Box::new(Change {
                            priority: Some(level),
                            ..Change::default()
                        }),
                    )),
                    Key::Today => sender.input(Msg::Edit(t.href, Box::new(on(today)))),
                    Key::Tomorrow => {
                        sender.input(Msg::Edit(t.href, Box::new(on(today + Days::days(1)))))
                    }
                    Key::NextWeek => {
                        sender.input(Msg::Edit(t.href, Box::new(on(today + Days::days(7)))))
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// `passive`: not for something done in the window, so a rebuild can
    /// wait for rows still opening or folding.
    fn render(&mut self, w: &mut Widgets, sender: &ComponentSender<Self>, passive: bool) {
        *w.status.borrow_mut() = self.status.clone();
        if ui::popovers_open() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        let tx: Tx = sender.input_sender().clone();
        let now = self.now();
        let (lists, open, completed) = self.view_data();
        let data = Data {
            lists: &lists,
            open: &open,
            completed: completed.as_deref(),
            sunday_first: self.prefs.sunday_first,
        };
        let counts = Counts::new(&data, now);
        if let Some(tray) = &w.tray {
            tray.set(tray::State {
                overdue: counts.overdue,
                today: counts.today - counts.overdue,
            });
        }
        w.sidebar.update(&sidebar::State {
            nav: &self.nav,
            lists: &self.prefs.arrange(&self.lists),
            counts: &counts,
            status: self.status.as_ref(),
            prefs: &self.prefs,
        });
        w.header_title.set_text(&self.nav.title(&self.lists));
        // Planify marks the view button while a filter hides tasks.
        if self.prefs.view_opts(&self.nav).filtered() {
            w.view_button.add_css_class("filtered");
        } else {
            w.view_button.remove_css_class("filtered");
        }

        let signed_out = self
            .status
            .as_ref()
            .is_some_and(|s| s.state == SyncState::NoAccount);
        let page = if let Some(e) = &self.error {
            w.error_page
                .set_description(Some(&glib::markup_escape_text(e)));
            "error"
        } else if signed_out {
            self.render_sign_in(w);
            "signin"
        } else if !self.loaded {
            "loading"
        } else {
            "tasks"
        };
        w.stack.set_visible_child_name(page);
        w.view_button.set_visible(page == "tasks");
        // The ⋮ menu is a list's: select, edit, delete.
        w.more_button
            .set_visible(page == "tasks" && self.shown_list().is_some());

        let opts = self.prefs.view_opts(&self.nav);
        CURRENT_VIEW.with(|c| *c.borrow_mut() = Some((self.nav.clone(), opts.clone())));
        CURRENT_LISTS.with(|c| *c.borrow_mut() = lists.clone());
        CURRENT_ARCHIVED.with(|c| *c.borrow_mut() = self.prefs.archived.clone());
        let sections = model::plan(&self.nav, &data, &opts, now);
        let typing = self.editing.is_some() && w.editor.typing();

        // The open task follows its edits, and closes once it leaves the
        // view (done elsewhere, moved, filtered out, deleted).
        if let Some(href) = self.editing.clone() {
            if let Some(t) = self.find(&href).cloned() {
                w.editor.show(&t, &self.lists, self.prefs.sunday_first);
            }
            let here = sections
                .iter()
                .any(|s| s.tasks.iter().any(|t| t.href == href));
            if !here && !typing {
                self.close_editor(w, sender);
            }
        }

        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.nav.key().hash(&mut h);
            now.date_naive().hash(&mut h);
            format!(
                "{:?}{:?}{:?}{:?}{:?}{:?}{}{}",
                self.adding,
                self.select_mode,
                opts,
                self.editing,
                self.prefs.archived,
                self.prefs.list_order,
                self.prefs.lists_by_name,
                self.prefs.sunday_first,
            )
            .hash(&mut h);
            let mut sel: Vec<&String> = self.selected.iter().collect();
            sel.sort();
            sel.hash(&mut h);
            let mut comp: Vec<&String> = self.completing.keys().collect();
            comp.sort();
            comp.hash(&mut h);
            for l in &self.lists {
                (&l.href, &l.name, &l.color).hash(&mut h);
            }
            counts.of(&self.nav).hash(&mut h);
            for s in &sections {
                format!("{:?}{:?}{:?}", s.kind, s.title, s.note).hash(&mut h);
                for t in &s.tasks {
                    if self.editing.as_deref() == Some(t.href.as_str()) {
                        // The editor shows its own changes; only where it sits matters.
                        t.href.hash(&mut h);
                    } else {
                        serde_json::to_string(t).unwrap_or_default().hash(&mut h);
                    }
                }
            }
            h.finish()
        };
        let settling = passive && self.settle_until.is_some_and(|u| Instant::now() < u);
        if key != self.shown && (typing || settling || self.folding.is_some()) {
            // A rebuild takes the field from under the keyboard, and an input
            // method's composition with it: wait until typing moves on. It
            // would also cut short a card folding or rows coming and going.
            self.dirty = true;
        } else if key != self.shown {
            self.shown = key;
            let focus = gtk::prelude::GtkWindowExt::focus(&w.window);
            let had_focus = focus.as_ref().is_some_and(|f| f.is_ancestor(&w.clamp));
            // The editor and the add card outlive rebuilds; so does the
            // keyboard in them.
            let kept =
                focus.filter(|f| f.is_ancestor(&w.editor.root) || f.is_ancestor(&w.add_card.root));

            // What changed since this view was last built moves; another
            // view, or the first with data, just fades in.
            let view = self.nav.key();
            let same = self.built.nav.as_ref() == Some(&view) && motion::enabled();
            let before: HashSet<&str> = w.rows.iter().map(|(h, _)| h.as_str()).collect();
            let hrefs: HashSet<&str> = sections
                .iter()
                .flat_map(|s| s.tasks.iter().map(|t| t.href.as_str()))
                .collect();
            let arriving: HashSet<String> = hrefs
                .iter()
                .filter(|h| !before.contains(*h))
                .map(|h| h.to_string())
                .collect();
            let leaving = before.iter().filter(|h| !hrefs.contains(*h)).count();
            let rows_move = same && motion::few(arriving.len(), leaving);
            let checked: HashSet<String> = self
                .completing
                .keys()
                .filter(|h| !self.built.completing.contains(*h))
                .cloned()
                .collect();
            let fresh = content::Fresh {
                rows: rows_move.then_some(&arriving),
                checked: same.then_some(&checked),
                rolled: same.then_some(&self.rolled),
                adding: same && self.adding.is_some() && !self.built.adding,
                selecting: same && self.select_mode && !self.built.selecting,
                view: !same && self.built.nav.is_some() && motion::enabled(),
                focus: same && self.editing.is_some() != self.built.editing,
            };
            // The date of a repeating task that moved on lights up for a while;
            // the sync that follows shouldn't rebuild it away before then.
            let flashing = same && !self.rolled.is_empty();
            let moves = (rows_move && (leaving > 0 || !arriving.is_empty()))
                || fresh.adding
                || fresh.selecting
                || fresh.view;
            let built = content::build(
                &sections,
                &Opts {
                    nav: &self.nav,
                    lists: &self.lists,
                    archived: self.nav_archived(),
                    now,
                    completing: &self.completing.keys().cloned().collect(),
                    select_mode: self.select_mode,
                    selected: &self.selected,
                    sunday_first: self.prefs.sunday_first,
                    adding: self.adding.clone(),
                    custom_order: self.nav.is_list() && opts.sort_for(&self.nav) == Sort::Custom,
                    editing: self.editing.as_deref(),
                    editor: &w.editor.root,
                    fresh,
                },
                &w.add_card.root,
                &tx,
            );
            w.clamp.set_child(Some(&built.root));
            // On screen now, so what opens can slide.
            motion::open_new();
            if rows_move {
                motion::leave(&w.rows, &built.rows);
            }
            if moves {
                self.hold(sender, motion::lasts(motion::ROW_MS));
            }
            if flashing {
                self.hold(sender, motion::lasts(ROLLED_MS));
            }
            w.rows = built.rows;
            self.built = Built {
                nav: self.loaded.then_some(view),
                completing: self.completing.keys().cloned().collect(),
                adding: self.adding.is_some(),
                selecting: self.select_mode,
                editing: self.editing.is_some(),
            };
            self.rolled.clear();
            if std::mem::take(&mut self.expand) && self.editing.is_some() {
                w.editor.expand();
            }
            let refocus = std::mem::replace(&mut self.refocus, Refocus::Keep);
            if let Some(c) = &self.cursor {
                match w.rows.iter().find(|(h, _)| h == c) {
                    Some((_, row)) => {
                        row.add_css_class("cursor");
                        if self.editing.is_none()
                            && self.adding.is_none()
                            && (had_focus || refocus == Refocus::Row)
                        {
                            row.grab_focus();
                        }
                    }
                    None => self.cursor = None,
                }
            }
            if refocus == Refocus::Editor && self.editing.is_some() {
                w.editor.focus();
            } else if let Some(f) = kept.filter(|f| f.root().is_some()) {
                match f.downcast_ref::<gtk::Text>() {
                    // Plain grab_focus would select what was typed.
                    Some(text) => {
                        text.grab_focus_without_selecting();
                    }
                    None => {
                        f.grab_focus();
                    }
                }
            }
        }

        w.select_revealer.set_reveal_child(self.select_mode);
        w.select_label.set_text(&match self.selected.len() {
            0 => "Select tasks".to_string(),
            1 => "1 selected".to_string(),
            n => format!("{n} selected"),
        });
    }

    /// Development builds: `nav today`, `key j`, `open <words>`, `reveal <words>`, `close`,
    /// `menu <words>`, `popup date|list|link|priority|reminders|menu|view|more`,
    /// `popdown`, `title <text>`, `notes <text>`, `add`, `type <text>`,
    /// `submit`, `select <words>`, `prefs [sidebar]`, `hide <view>`, `find [text]`,
    /// `shortcuts`, `about`, `newlist`, `scheme dark|light`, `size W H`,
    /// `click X Y`, `focus`, `unfocus`, `switch N`, `scroll PX`, `close-dialog`, `close-window`, `state`,
    /// `clear [list]` (asks), `clear! [list]`, `archive <list>`, `unarchive <list>`,
    /// `order <list>,<list>`, `opts sort=name|desc=true|due=week|hide=<list>|reset`,
    /// `addtext <text, \n for a new line>`, `newline` (typed in the notes), `open-task <words>`.
    fn drive(&mut self, w: &mut Widgets, sender: &ComponentSender<Self>, command: &str) {
        let (verb, arg) = command.split_once(' ').unwrap_or((command, ""));
        let task = |text: &str| {
            let text = text.to_lowercase();
            self.open
                .iter()
                .chain(self.completed.iter().flatten())
                .find(|t| t.task.summary.to_lowercase().contains(&text))
                .map(|t| t.href.clone())
        };
        let list = |name: &str| {
            self.lists
                .iter()
                .find(|l| l.name.eq_ignore_ascii_case(name.trim()))
                .map(|l| l.href.clone())
        };
        match verb {
            "clear" | "clear!" => {
                let target = (!arg.is_empty()).then(|| list(arg)).flatten();
                if verb == "clear" {
                    sender.input(Msg::AskDeleteCompleted(target));
                } else {
                    sender.input(Msg::DeleteCompleted(target));
                }
            }
            "archive" | "unarchive" => {
                if let Some(h) = list(arg) {
                    sender.input(Msg::Archive(h, verb == "archive"));
                }
            }
            "order" => {
                let order: Vec<String> = arg.split(',').filter_map(list).collect();
                sender.input(Msg::ReorderLists(order));
            }
            "opts" => {
                let mut opts = self.prefs.view_opts(&self.nav);
                match arg.split_once('=') {
                    Some(("sort", v)) => {
                        opts.sort = Sort::ALL
                            .into_iter()
                            .find(|s| s.label().to_lowercase().starts_with(v))
                    }
                    Some(("desc", v)) => opts.descending = v == "true",
                    Some(("due", v)) => {
                        opts.due = model::DueFilter::ALL
                            .into_iter()
                            .find(|d| d.label().to_lowercase().contains(v))
                            .unwrap_or_default()
                    }
                    Some(("hide", v)) => opts.hide_lists.extend(list(v)),
                    _ => opts = ViewOpts::default(),
                }
                sender.input(Msg::SetViewOpts(opts));
            }
            "addtext" => sender.input(Msg::AddText(arg.replace("\\n", "\n"))),
            "newline" => w.editor.type_newline(),
            "open-task" => {
                if let Some(h) = task(arg) {
                    sender.input(Msg::OpenTask(h));
                }
            }
            "nav" => {
                let nav = Nav::from_key(arg).or_else(|| {
                    self.lists
                        .iter()
                        .find(|l| l.name.eq_ignore_ascii_case(arg))
                        .map(|l| Nav::List(l.href.clone()))
                });
                if let Some(n) = nav {
                    sender.input(Msg::Navigate(n));
                }
            }
            "key" => {
                let key = match arg {
                    "j" => Some(Key::Down),
                    "k" => Some(Key::Up),
                    "x" => Some(Key::Toggle),
                    "e" => Some(Key::Open),
                    "a" => Some(Key::Add),
                    "d" => Some(Key::Delete),
                    "t" => Some(Key::Today),
                    "m" => Some(Key::Tomorrow),
                    "v" => Some(Key::Select),
                    "Escape" => Some(Key::Escape),
                    "ctrl+w" => Some(Key::Close),
                    "ctrl+q" => Some(Key::Quit),
                    "1" | "2" | "3" | "4" => arg.parse().ok().map(Key::Priority),
                    _ => None,
                };
                if let Some(k) = key {
                    sender.input(Msg::Key(k));
                }
            }
            "open" => {
                if let Some(h) = task(arg) {
                    sender.input(Msg::Open(h));
                }
            }
            // As Quick Find does: go to the task's list and open it.
            "reveal" => {
                if let Some(h) = task(arg) {
                    sender.input(Msg::Reveal(h));
                }
            }
            "cursor" => {
                if let Some(h) = task(arg) {
                    self.cursor = Some(h);
                    self.shown = 0;
                }
            }
            "check" => {
                if let Some(h) = task(arg) {
                    sender.input(Msg::Check(h, true));
                }
            }
            "select" => {
                if let Some(h) = task(arg) {
                    sender.input(Msg::SelectToggle(h));
                }
            }
            "menu" => {
                let Some(h) = task(arg) else { return };
                let Some((_, row)) = w.rows.iter().find(|(x, _)| *x == h) else {
                    return;
                };
                let Some(t) = self.find(&h).cloned() else {
                    return;
                };
                let at = gdk::Rectangle::new(120, 16, 1, 1);
                let tx: Tx = sender.input_sender().clone();
                let menu = crate::row::task_menu(
                    &t,
                    &self.lists,
                    self.now(),
                    self.prefs.sunday_first,
                    row.upcast_ref(),
                    at,
                    &tx,
                );
                crate::row::show_at(&menu, row, at);
            }
            "popup" => match arg {
                "view" => w.view_button.popup(),
                "more" => w.more_button.popup(),
                button => {
                    if !w.editor.popup(button) {
                        eprintln!("drive: no {button:?} button in the editor");
                    }
                }
            },
            // `drop <moved words> before|after <target words>`
            "drop" => {
                let words: Vec<&str> = arg.split_whitespace().collect();
                let Some(i) = words.iter().position(|w| *w == "before" || *w == "after") else {
                    return;
                };
                let (Some(moved), Some(target)) =
                    (task(&words[..i].join(" ")), task(&words[i + 1..].join(" ")))
                else {
                    return;
                };
                let list = self
                    .find(&moved)
                    .map(|t| t.list.clone())
                    .unwrap_or_default();
                let tasks = model::sorted(
                    self.open
                        .iter()
                        .filter(|t| t.list == list)
                        .cloned()
                        .collect(),
                    Sort::Custom,
                    self.zone,
                );
                let orders = model::reorder(&tasks, &moved, &target, words[i] == "after");
                sender.input(Msg::Reorder(orders));
            }
            "popdown" => {
                if let Some(p) = ui::last_popover() {
                    p.popdown();
                }
            }
            "add" => sender.input(Msg::ShowAdd(None)),
            // Choose a row or a button in the open popover by its words.
            "pick" => {
                let Some(p) = ui::last_popover().filter(|p| p.is_visible()) else {
                    eprintln!("drive: no popover open");
                    return;
                };
                let want = arg.to_lowercase();
                let words = |w: &gtk::Widget| {
                    let mut text = String::new();
                    let mut stack = vec![w.clone()];
                    while let Some(w) = stack.pop() {
                        if let Some(l) = w.downcast_ref::<gtk::Label>() {
                            text.push_str(&l.text().to_lowercase());
                            text.push(' ');
                        }
                        let mut child = w.first_child();
                        while let Some(c) = child {
                            child = c.next_sibling();
                            stack.push(c);
                        }
                    }
                    text
                };
                let mut queue: std::collections::VecDeque<gtk::Widget> =
                    p.child().into_iter().collect();
                while let Some(widget) = queue.pop_front() {
                    let hit = (widget.is::<gtk::ListBoxRow>() || widget.is::<gtk::Button>())
                        && widget.is_visible()
                        && words(&widget).contains(&want);
                    if hit {
                        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                            row.activate();
                        } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                            button.emit_clicked();
                        }
                        return;
                    }
                    let mut child = widget.first_child();
                    while let Some(c) = child {
                        child = c.next_sibling();
                        queue.push_back(c);
                    }
                }
                eprintln!("drive: nothing in the popover says {arg:?}");
            }
            "type" => w.add_card.set_text(arg),
            "submit" => w.add_card.submit(),
            "title" => w.editor.type_title(arg),
            "notes" => w.editor.type_notes(arg),
            "close" => sender.input(Msg::CloseEditor),
            "prefs" => sender.input(Msg::Preferences((arg == "sidebar").then_some("sidebar"))),
            "hide" => {
                if let Some(n) = Nav::from_key(arg) {
                    sender.input(Msg::HideView(n));
                }
            }
            "unfocus" => gtk::prelude::GtkWindowExt::set_focus(&w.window, None::<&gtk::Widget>),
            // Flip the Nth switch on the dialog page showing.
            "switch" => {
                let page = w
                    .window
                    .visible_dialog()
                    .and_downcast::<adw::PreferencesDialog>()
                    .and_then(|d| d.visible_page());
                let mut switches = Vec::new();
                let mut stack: Vec<gtk::Widget> = page.into_iter().map(|p| p.upcast()).collect();
                while let Some(widget) = stack.pop() {
                    if let Ok(s) = widget.clone().downcast::<gtk::Switch>() {
                        switches.push(s);
                    }
                    let mut child = widget.last_child();
                    while let Some(c) = child {
                        child = c.prev_sibling();
                        stack.push(c);
                    }
                }
                if let Some(s) = arg.parse::<usize>().ok().and_then(|n| switches.get(n)) {
                    s.set_active(!s.is_active());
                }
            }
            // What the compositor's close does.
            "close-window" => {
                w.window.close();
            }
            // Whether the window shows and the tray is up, to window.log.
            "state" => eprintln!(
                "state: visible {} tray {}",
                w.window.is_visible(),
                w.tray
                    .as_ref()
                    .map_or("off", |t| if t.online() { "online" } else { "offline" }),
            ),
            // What has the keyboard, to window.log.
            "focus" => {
                let focus = gtk::prelude::GtkWindowExt::focus(&w.window);
                eprintln!(
                    "focus: {:?} in editor: {} in add card: {}",
                    focus.as_ref().map(|f| f.type_().name()),
                    focus
                        .as_ref()
                        .is_some_and(|f| f.is_ancestor(&w.editor.root)),
                    focus
                        .as_ref()
                        .is_some_and(|f| f.is_ancestor(&w.add_card.root)),
                );
            }
            // A click on the column's background at a point in the window.
            "click" => {
                let mut it = arg.split_whitespace().filter_map(|n| n.parse::<f64>().ok());
                if let (Some(x), Some(y)) = (it.next(), it.next())
                    && let Some(target) = w.window.pick(x, y, gtk::PickFlags::DEFAULT)
                {
                    eprintln!("click: {}", target.type_().name());
                    if target.is_ancestor(&w.clamp) && on_background(&target) {
                        sender.input(Msg::CloseEditor);
                    }
                }
            }
            "find" => find::open(
                &w.window,
                &self.lists,
                &self.visible_open(),
                &self.prefs,
                arg,
                sender.input_sender().clone(),
            ),
            "shortcuts" => sender.input(Msg::Shortcuts),
            "about" => sender.input(Msg::About),
            "newlist" => sender.input(Msg::NewList),
            "scheme" => adw::StyleManager::default().set_color_scheme(match arg {
                "dark" => adw::ColorScheme::ForceDark,
                "light" => adw::ColorScheme::ForceLight,
                _ => adw::ColorScheme::Default,
            }),
            "size" => {
                let mut it = arg.split_whitespace().filter_map(|n| n.parse::<i32>().ok());
                if let (Some(width), Some(height)) = (it.next(), it.next()) {
                    w.window.set_default_size(width, height);
                }
            }
            // Scroll what the open dialog (or else the view) shows to a place.
            "scroll" => {
                let root: gtk::Widget = w
                    .window
                    .visible_dialog()
                    .map_or_else(|| w.clamp.clone().upcast(), |d| d.upcast());
                let mut stack = vec![root];
                while let Some(widget) = stack.pop() {
                    if let Ok(s) = widget.clone().downcast::<gtk::ScrolledWindow>()
                        && s.is_mapped()
                    {
                        s.vadjustment().set_value(arg.parse().unwrap_or(0.0));
                        break;
                    }
                    let mut child = widget.first_child();
                    while let Some(c) = child {
                        child = c.next_sibling();
                        stack.push(c);
                    }
                }
            }
            "close-dialog" => {
                if let Some(d) = w.window.visible_dialog() {
                    d.close();
                }
            }
            _ => eprintln!("drive: unknown command {command:?}"),
        }
    }

    fn render_sign_in(&self, w: &Widgets) {
        let s = &w.signin;
        let waiting = self.signing_in.is_some();
        s.waiting.set_visible(waiting);
        s.button.set_sensitive(!waiting);
        s.entry.set_sensitive(!waiting);
        if let Some(url) = self.signing_in.as_ref().filter(|u| !u.is_empty()) {
            s.again.set_uri(url);
            s.again.set_visible(true);
        } else {
            s.again.set_visible(false);
        }
        s.error.set_visible(self.sign_in_error.is_some());
        s.error
            .set_text(self.sign_in_error.as_deref().unwrap_or(""));
    }
}

fn sign_in_page(tx: &Tx) -> (adw::StatusPage, SignIn) {
    let page = adw::StatusPage::builder()
        .icon_name("cloud-outline-thick-symbolic")
        .title("Sign in to Nextcloud")
        .description("asst keeps your tasks on Nextcloud, where Apple Reminders on your iPhone finds them too.")
        .build();
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.set_halign(gtk::Align::Center);
    b.set_width_request(320);
    let entry = gtk::Entry::builder()
        .placeholder_text("cloud.example.com")
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    b.append(&entry);
    let button = gtk::Button::with_label("Sign In");
    button.add_css_class("pill");
    button.add_css_class("suggested-action");
    b.append(&button);
    let submit = {
        let tx = tx.clone();
        let entry = entry.clone();
        move || {
            let server = entry.text().trim().to_string();
            if !server.is_empty() {
                tx.emit(Msg::SignIn(server));
            }
        }
    };
    {
        let submit = submit.clone();
        entry.connect_activate(move |_| submit());
    }
    button.connect_clicked(move |_| submit());

    let waiting = gtk::Box::new(gtk::Orientation::Vertical, 6);
    waiting.append(&adw::Spinner::new());
    let note = gtk::Label::new(Some("Approve asst in the browser, then come back."));
    note.set_wrap(true);
    note.add_css_class("dim-label");
    waiting.append(&note);
    let again = gtk::LinkButton::with_label("https://nextcloud.com", "Open the page again");
    waiting.append(&again);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    {
        let tx = tx.clone();
        cancel.connect_clicked(move |_| tx.emit(Msg::CancelSignIn));
    }
    waiting.append(&cancel);
    waiting.set_visible(false);
    b.append(&waiting);
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_visible(false);
    b.append(&error);
    page.set_child(Some(&b));
    (
        page,
        SignIn {
            entry,
            button,
            waiting,
            again,
            error,
        },
    )
}

/// Sort, dates, completed tasks, priorities, lists: remembered per view.
fn view_menu(tx: Tx) -> gtk::Popover {
    let current = CURRENT_VIEW.with(|c| c.borrow().clone());
    let Some((nav, opts)) = current else {
        return ui::menu(&[]);
    };
    let heading = |text: &str| -> gtk::Widget {
        ui::label(text, &["caption-heading", "menu-heading"]).upcast()
    };
    let set = |next: ViewOpts| {
        let tx = tx.clone();
        move || tx.emit(Msg::SetViewOpts(next.clone()))
    };
    let mut items: Vec<gtk::Widget> = Vec::new();
    if nav != Nav::Completed {
        items.push(heading("Sort by"));
        let sort_now = opts.sort_for(&nav);
        for sort in Sort::ALL {
            if sort == Sort::Custom && !nav.is_list() {
                continue;
            }
            let mut next = opts.clone();
            next.sort = Some(sort);
            items.push(
                Item::new(None, sort.label())
                    .checked(sort_now == sort)
                    .build(set(next))
                    .upcast(),
            );
        }
        if sort_now != Sort::Custom {
            let mut next = opts.clone();
            next.descending = !opts.descending;
            items.push(ui::separator());
            items.push(
                Item::new(Some("view-sort-descending-rtl-symbolic"), "Descending")
                    .checked(opts.descending)
                    .build(set(next))
                    .upcast(),
            );
        }
    }
    if nav.filters_by_date() {
        items.push(ui::separator());
        items.push(heading("Due date"));
        for due in model::DueFilter::ALL {
            let mut next = opts.clone();
            next.due = due;
            items.push(
                Item::new(None, due.label())
                    .checked(opts.due == due)
                    .build(set(next))
                    .upcast(),
            );
        }
    }
    if nav == Nav::Completed {
        let lists = CURRENT_LISTS.with(|l| l.borrow().clone());
        items.push(heading("Lists"));
        for l in &lists {
            let hidden = opts.hide_lists.contains(&l.href);
            let mut next = opts.clone();
            next.hide_lists.retain(|h| *h != l.href);
            if !hidden {
                if next.hide_lists.len() + 1 >= lists.len() {
                    // Not the last one left.
                    continue;
                }
                next.hide_lists.push(l.href.clone());
            }
            let ring = ui::ring(l.color.as_deref(), 12);
            let item = Item::new(None, &l.name).checked(!hidden).build(set(next));
            if let Some(content) = item.child().and_downcast::<gtk::Box>() {
                content.prepend(&ring);
            }
            items.push(item.upcast());
        }
        items.push(ui::separator());
        let tx = tx.clone();
        items.push(
            Item::new(Some("user-trash-symbolic"), "Delete all completed tasks…")
                .danger()
                .build(move || tx.emit(Msg::AskDeleteCompleted(None)))
                .upcast(),
        );
    }
    if matches!(nav, Nav::Inbox | Nav::List(_) | Nav::Today) {
        items.push(ui::separator());
        let tx = tx.clone();
        let mut next = opts.clone();
        next.show_completed = !opts.show_completed;
        items.push(
            Item::new(Some("check-round-outline-symbolic"), "Show completed")
                .checked(opts.show_completed)
                .build(move || tx.emit(Msg::SetViewOpts(next.clone())))
                .upcast(),
        );
    }
    if nav != Nav::Completed {
        items.push(ui::separator());
        items.push(ui::label("Priority", &["caption-heading", "menu-heading"]).upcast());
        for level in 1..=4u8 {
            let tx = tx.clone();
            let mut next = opts.clone();
            let i = usize::from(level - 1);
            next.priorities[i] = !opts.priorities[i];
            if !next.priorities.contains(&true) {
                continue;
            }
            let tint = [
                "priority-1-icon",
                "priority-2-icon",
                "priority-3-icon",
                "priority-4-icon",
            ][i];
            items.push(
                Item::new(
                    Some("flag-outline-thick-symbolic"),
                    model::priority_name(level),
                )
                .tint(tint)
                .checked(opts.priorities[i])
                .build(move || tx.emit(Msg::SetViewOpts(next.clone())))
                .upcast(),
            );
        }
    }
    ui::menu(&items)
}

thread_local! {
    /// The view shown and its options, for menus built on demand.
    static CURRENT_VIEW: std::cell::RefCell<Option<(Nav, ViewOpts)>> = const { std::cell::RefCell::new(None) };
    static CURRENT_LISTS: std::cell::RefCell<Vec<ListView>> = const { std::cell::RefCell::new(Vec::new()) };
    static CURRENT_ARCHIVED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A list's menu, as Planify's project menu: select, edit, copy, archive,
/// delete.
fn more_menu(tx: Tx) -> gtk::Popover {
    let Some((nav, _)) = CURRENT_VIEW.with(|c| c.borrow().clone()) else {
        return ui::menu(&[]);
    };
    let lists = CURRENT_LISTS.with(|l| l.borrow().clone());
    let list = match &nav {
        Nav::List(href) => lists.iter().find(|l| &l.href == href),
        Nav::Inbox => model::inbox(&lists),
        _ => None,
    };
    let Some(list) = list else {
        return ui::menu(&[]);
    };
    let archived = CURRENT_ARCHIVED.with(|a| a.borrow().contains(&list.href));
    sidebar::list_menu(list, archived, &tx, true)
}

/// Scroll the view while something dragged over it nears its top or bottom
/// edge, faster the nearer, so a task can be dragged past what's on screen.
fn scroll_while_dragging(scroller: &gtk::ScrolledWindow) {
    const EDGE: f64 = 56.0;
    let speed = Rc::new(Cell::new(0.0));
    let ticking: Rc<Cell<bool>> = Rc::default();
    let motion = gtk::DropControllerMotion::new();
    {
        let (speed, ticking, scroller) = (speed.clone(), ticking.clone(), scroller.clone());
        motion.connect_motion(move |_, _, y| {
            let height = f64::from(scroller.height());
            speed.set(if y < EDGE {
                -(EDGE - y) / 3.0
            } else if y > height - EDGE {
                (y - (height - EDGE)) / 3.0
            } else {
                0.0
            });
            // A frame clock only while scrolling, not for the whole drag.
            if speed.get() != 0.0 && !ticking.replace(true) {
                let (speed, ticking) = (speed.clone(), ticking.clone());
                scroller.add_tick_callback(move |s, _| {
                    let step = speed.get();
                    let adj = s.vadjustment();
                    let before = adj.value();
                    adj.set_value(before + step);
                    if step == 0.0 || adj.value() == before {
                        ticking.set(false);
                        return glib::ControlFlow::Break;
                    }
                    glib::ControlFlow::Continue
                });
            }
        });
    }
    motion.connect_leave(move |_| speed.set(0.0));
    scroller.add_controller(motion);
}

/// A task's title as a file name: no slashes or control characters, not too long.
fn file_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| if c == '/' || c.is_control() { '-' } else { c })
        .take(60)
        .collect();
    let name = name.trim().trim_start_matches('.');
    if name.is_empty() {
        "task".into()
    } else {
        name.into()
    }
}

/// Vim keys and Planify's shortcuts; typing in a field is left alone.
fn key_action(
    window: &adw::ApplicationWindow,
    key: gdk::Key,
    state: gdk::ModifierType,
    tx: &Tx,
) -> glib::Propagation {
    let focus = gtk::prelude::GtkWindowExt::focus(window);
    let inside = |t: glib::Type| focus.as_ref().is_some_and(|f| f.ancestor(t).is_some());
    if inside(adw::Dialog::static_type()) || inside(gtk::Popover::static_type()) {
        return glib::Propagation::Proceed;
    }
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let action = if ctrl {
        match key {
            gdk::Key::f => Some(Key::Find),
            gdk::Key::i => Some(Key::Go(Nav0::Inbox)),
            gdk::Key::t => Some(Key::Go(Nav0::Today)),
            gdk::Key::u => Some(Key::Go(Nav0::Scheduled)),
            gdk::Key::comma => Some(Key::Preferences),
            gdk::Key::b => Some(Key::Sidebar),
            gdk::Key::w => Some(Key::Close),
            gdk::Key::q => Some(Key::Quit),
            gdk::Key::Page_Up => Some(Key::PrevView),
            gdk::Key::Page_Down => Some(Key::NextView),
            k => k
                .to_unicode()
                .and_then(|c| c.to_digit(10))
                .filter(|d| (1..=9).contains(d))
                .map(|d| Key::Go(Nav0::List(d as usize - 1))),
        }
    } else {
        None
    };
    if let Some(a) = action {
        tx.emit(Msg::Key(a));
        return glib::Propagation::Stop;
    }
    // A field, or anything in the task open in place: its buttons take
    // Space and Enter, and letters only act on tasks once it is closed.
    let typing = focus.as_ref().is_some_and(|f| {
        f.is::<gtk::Text>()
            || f.is::<gtk::TextView>()
            || std::iter::successors(Some(f.clone()), |w| w.parent())
                .any(|w| w.has_css_class("task-editor"))
    });
    if typing {
        // With a popover over the field (the add card's list suggestions),
        // Esc closes that first.
        if key == gdk::Key::Escape && !ui::popovers_open() {
            tx.emit(Msg::Key(Key::Escape));
            gtk::prelude::GtkWindowExt::set_focus(window, None::<&gtk::Widget>);
            return glib::Propagation::Stop;
        }
        return glib::Propagation::Proceed;
    }
    // Outside a field, what is pasted becomes a task.
    if ctrl && matches!(key, gdk::Key::v | gdk::Key::V) {
        tx.emit(Msg::Key(Key::Paste));
        return glib::Propagation::Stop;
    }
    if ctrl || state.contains(gdk::ModifierType::ALT_MASK) {
        return glib::Propagation::Proceed;
    }
    let action = match key {
        gdk::Key::j => Key::Down,
        gdk::Key::k => Key::Up,
        gdk::Key::g | gdk::Key::Home => Key::First,
        gdk::Key::G | gdk::Key::End => Key::Last,
        gdk::Key::e | gdk::Key::l => Key::Open,
        gdk::Key::x | gdk::Key::space => Key::Toggle,
        gdk::Key::a | gdk::Key::o => Key::Add,
        gdk::Key::slash => Key::Find,
        gdk::Key::d | gdk::Key::Delete => Key::Delete,
        gdk::Key::_1 => Key::Priority(1),
        gdk::Key::_2 => Key::Priority(2),
        gdk::Key::_3 => Key::Priority(3),
        gdk::Key::_4 => Key::Priority(4),
        gdk::Key::t => Key::Today,
        gdk::Key::m => Key::Tomorrow,
        gdk::Key::w => Key::NextWeek,
        gdk::Key::bracketleft | gdk::Key::H => Key::PrevView,
        gdk::Key::bracketright | gdk::Key::L => Key::NextView,
        gdk::Key::s => Key::Sync,
        gdk::Key::v => Key::Select,
        gdk::Key::p => Key::NewList,
        gdk::Key::question | gdk::Key::F1 => Key::Shortcuts,
        gdk::Key::Escape => Key::Escape,
        _ => return glib::Propagation::Proceed,
    };
    tx.emit(Msg::Key(action));
    glib::Propagation::Stop
}
