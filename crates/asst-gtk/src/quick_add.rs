//! `asst-gtk quick-add`: a layer-shell popup with the window's add card.
//! Type with quick-add syntax or use the buttons; Enter adds and closes,
//! Shift+Enter (or Keep adding, Ctrl+K) adds and stays, Esc closes. Run it
//! again while open to close it. `--attach NOTE` links what it adds to a
//! note, for an editor to start it from the note open in it.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use asst_core::api::{AddSpec, Added, ListView};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, glib};
use relm4::{Component, ComponentParts, ComponentSender, RelmApp};

use crate::addcard::{AddCard, Target};
use crate::client;
use crate::prefs::Prefs;

/// `linked`: full paths of notes the tasks link to.
pub fn run(linked: Vec<String>) {
    let app = RelmApp::new("dev.jaeho.Asst.QuickAdd")
        .with_args(Vec::new())
        .visible_on_activate(false);
    relm4::set_global_css(crate::CSS);
    app.run::<QuickAdd>(linked);
}

/// The list quick add added to last: a file of its own, since the window
/// writes `window.toml` whole and would lose it.
fn last_list_path() -> PathBuf {
    asst_core::config::state_dir().join("quick-add-list")
}

pub struct QuickAdd {
    busy: bool,
    keep_open: Rc<Cell<bool>>,
}

pub struct Widgets {
    card: Rc<AddCard>,
    status: gtk::Label,
}

#[derive(Debug)]
pub enum Msg {
    Add(Box<AddSpec>),
    Close,
}

#[derive(Debug)]
pub enum Cmd {
    Lists(client::Result<Vec<ListView>>),
    Added(client::Result<Box<Added>>),
}

impl Component for QuickAdd {
    type Init = Vec<String>;
    type Input = Msg;
    type Output = ();
    type CommandOutput = Cmd;
    type Root = gtk::Window;
    type Widgets = Widgets;

    fn init_root() -> gtk::Window {
        crate::load_icons();
        let window = gtk::Window::builder()
            .title("Add a task")
            .default_width(680)
            .resizable(false)
            .build();
        window.add_css_class("asst-quick-add");
        window.init_layer_shell();
        window.set_namespace(Some("asst-quick-add"));
        if crate::devel::enabled() {
            // A scripted check must not take the keyboard from whoever is typing.
            window.set_layer(Layer::Top);
            window.set_keyboard_mode(KeyboardMode::None);
        } else {
            window.set_layer(Layer::Overlay);
            window.set_keyboard_mode(KeyboardMode::Exclusive);
        }
        window.set_anchor(Edge::Top, true);
        window.set_margin(Edge::Top, 160);
        window
    }

    fn init(
        linked: Vec<String>,
        root: gtk::Window,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let card = {
            let (add, close) = (sender.clone(), sender.clone());
            AddCard::new(
                move |spec| add.input(Msg::Add(Box::new(spec))),
                move || close.input(Msg::Close),
            )
        };
        card.root.add_css_class("quick-add-card");
        card.open(Target::Anywhere);
        card.set_linked(linked);
        card.show_keep();
        let prefs = Prefs::load();
        card.set_defaults(prefs.default_priority, prefs.read_dates);

        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .margin_start(24)
            .margin_end(24)
            .build();
        status.add_css_class("quick-add-status");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&card.root);
        content.append(&status);
        root.set_child(Some(&content));

        let keep_open = Rc::new(Cell::new(false));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let sender = sender.clone();
            let card = card.clone();
            let keep_open = keep_open.clone();
            let root = root.clone();
            keys.connect_key_pressed(move |_, key, _, state| {
                let in_popover = gtk::prelude::GtkWindowExt::focus(&root)
                    .is_some_and(|f| f.ancestor(gtk::Popover::static_type()).is_some());
                // The list suggestions under the title take their own keys.
                let in_popover = in_popover || card.suggesting();
                match key {
                    gdk::Key::Escape if !in_popover => {
                        sender.input(Msg::Close);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Return | gdk::Key::KP_Enter
                        if !in_popover
                            && gtk::prelude::GtkWindowExt::focus(&root)
                                .is_some_and(|f| f.is::<gtk::Text>()) =>
                    {
                        keep_open.set(
                            state.contains(gdk::ModifierType::SHIFT_MASK) || card.keeps_adding(),
                        );
                        card.submit();
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        root.add_controller(keys);

        // Launched again while open: close (the bind toggles).
        let activations = Cell::new(0u32);
        {
            let sender = sender.clone();
            relm4::main_application().connect_activate(move |_| {
                activations.set(activations.get() + 1);
                if activations.get() > 1 {
                    sender.input(Msg::Close);
                }
            });
        }
        root.present();
        card.focus();
        sender.oneshot_command(async { Cmd::Lists(client::lists().await) });

        ComponentParts {
            model: QuickAdd {
                busy: false,
                keep_open,
            },
            widgets: Widgets { card, status },
        }
    }

    fn update_with_view(
        &mut self,
        _widgets: &mut Widgets,
        msg: Msg,
        sender: ComponentSender<Self>,
        _root: &gtk::Window,
    ) {
        match msg {
            Msg::Add(spec) => {
                if self.busy {
                    return;
                }
                self.busy = true;
                sender.oneshot_command(async move {
                    Cmd::Added(client::add(&spec).await.map(Box::new))
                });
            }
            Msg::Close => relm4::main_application().quit(),
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Widgets,
        cmd: Cmd,
        _sender: ComponentSender<Self>,
        _root: &gtk::Window,
    ) {
        let say = |text: &str, error: bool| {
            widgets.status.set_text(text);
            widgets.status.set_visible(true);
            if error {
                widgets.status.add_css_class("error");
            } else {
                widgets.status.remove_css_class("error");
            }
        };
        match cmd {
            Cmd::Lists(Ok(lists)) => {
                let prefs = Prefs::load();
                widgets.card.set_lists(&lists, prefs.sunday_first);
                let last = std::fs::read_to_string(last_list_path()).unwrap_or_default();
                if prefs.remember_list && lists.iter().any(|l| l.href == last.trim() && l.writable)
                {
                    widgets
                        .card
                        .set_target(Target::List(last.trim().to_string()));
                }
            }
            Cmd::Lists(Err(e)) => say(&e, true),
            Cmd::Added(result) => {
                self.busy = false;
                if let Ok(added) = &result
                    && Prefs::load().remember_list
                {
                    let path = last_list_path();
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&path, &added.task.list);
                }
                match result {
                    Ok(_) if !self.keep_open.get() => relm4::main_application().quit(),
                    Ok(added) if added.existed => say(
                        &format!("Already there: “{}”", added.task.task.summary),
                        false,
                    ),
                    Ok(added) => say(
                        &format!(
                            "Added “{}” to {}",
                            added.task.task.summary, added.task.list_name
                        ),
                        false,
                    ),
                    Err(e) => say(&e, true),
                }
                self.keep_open.set(false);
            }
        }
    }
}
