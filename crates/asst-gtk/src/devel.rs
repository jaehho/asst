//! Development builds (`ASST_DEVEL=1`): a separate app ID, a striped header,
//! and two app actions for driving the window from a script on the session
//! bus without focusing it:
//!
//! - `snapshot(path)` saves the window as a PNG; `snapshot-popover(path)`
//!   saves the open popover.
//! - `drive(command)` does what a click or a key would (see `Window::drive`).
//!
//! A popover in a window without keyboard focus closes as soon as it opens;
//! `ASST_NO_AUTOHIDE=1` keeps them up for these checks. The quick-add popup
//! takes no keyboard in development builds.

use relm4::gtk::prelude::*;
use relm4::gtk::{self, gio, glib, graphene};

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
    drive.connect_activate(move |_, command| {
        if let Some(c) = command.and_then(|c| c.str()) {
            tx.emit(Msg::Drive(c.to_string()));
        }
    });
    app.add_action(&drive);
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
