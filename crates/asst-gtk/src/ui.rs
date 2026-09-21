//! Small widgets the window is made of, in Planify's idiom: menu items,
//! due chips, list color rings, text fields with a placeholder.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, glib, pango};

/// Planify's project colors, by name.
pub const COLORS: [(&str, &str); 20] = [
    ("Berry Red", "#c42d78"),
    ("Red", "#e23d3d"),
    ("Orange", "#ff8a2a"),
    ("Yellow", "#f5c400"),
    ("Olive Green", "#9cab3a"),
    ("Lime Green", "#70c741"),
    ("Green", "#27983a"),
    ("Mint Green", "#55cbb0"),
    ("Teal", "#1492b2"),
    ("Sky Blue", "#139ef7"),
    ("Light Blue", "#7fb9e8"),
    ("Blue", "#3c6dff"),
    ("Grape", "#7b44e6"),
    ("Violet", "#a02adb"),
    ("Lavender", "#d89ae8"),
    ("Magenta", "#d6458d"),
    ("Salmon", "#f77c70"),
    ("Charcoal", "#666666"),
    ("Grey", "#a0a0a0"),
    ("Taupe", "#b99780"),
];

pub fn icon(name: &str, px: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(px);
    image
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

pub fn heading(text: &str) -> gtk::Label {
    label(text, &["heading"])
}

pub fn clear(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

/// Take a widget out of whatever box or row it was put in. Through the
/// container, so a row that goes away later doesn't take the widget from
/// its next parent.
pub fn detach(w: &impl IsA<gtk::Widget>) {
    if let Some(parent) = w.parent() {
        if let Some(b) = parent.downcast_ref::<gtk::Box>() {
            b.remove(w);
        } else if let Some(r) = parent.downcast_ref::<gtk::ListBoxRow>() {
            r.set_child(None::<&gtk::Widget>);
        } else if let Some(r) = parent.downcast_ref::<gtk::Revealer>() {
            r.set_child(None::<&gtk::Widget>);
        } else {
            w.unparent();
        }
    }
}

/// A wrapping text view without the view background, showing dim words
/// while it is empty.
pub fn text_view(placeholder: &str) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .hexpand(true)
        .build();
    view.remove_css_class("view");
    let hint = gtk::Label::builder()
        .label(placeholder)
        .can_target(false)
        .build();
    hint.add_css_class("dim-label");
    view.add_overlay(&hint, 0, 0);
    let buffer = view.buffer();
    hint.set_visible(buffer.char_count() == 0);
    buffer.connect_changed(move |b| hint.set_visible(b.char_count() == 0));
    view
}

// -- popovers ---------------------------------------------------------------

thread_local! {
    static OPEN: Cell<u32> = const { Cell::new(0) };
    static WHEN_CLOSED: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    static LAST: RefCell<glib::WeakRef<gtk::Popover>> = RefCell::new(glib::WeakRef::new());
}

/// The popover opened last, while it lives.
pub fn last_popover() -> Option<gtk::Popover> {
    LAST.with(|l| l.borrow().upgrade())
}

/// Count this popover as open while it is mapped: the window holds off
/// rebuilding rows under an open menu.
pub fn track(p: &gtk::Popover) {
    p.connect_map(|p| {
        OPEN.with(|o| o.set(o.get() + 1));
        LAST.with(|l| l.borrow().set(Some(p)));
    });
    p.connect_unmap(|_| {
        let left = OPEN.with(|o| {
            o.set(o.get().saturating_sub(1));
            o.get()
        });
        if left == 0 {
            // After the click that closed it has been handled.
            glib::idle_add_local_once(|| {
                WHEN_CLOSED.with(|f| {
                    if let Some(f) = f.borrow().as_ref() {
                        f();
                    }
                })
            });
        }
    });
}

pub fn popovers_open() -> bool {
    OPEN.with(|o| o.get() > 0)
}

pub fn when_popovers_close(f: impl Fn() + 'static) {
    WHEN_CLOSED.with(|w| *w.borrow_mut() = Some(Box::new(f)));
}

pub fn popover(child: &impl IsA<gtk::Widget>) -> gtk::Popover {
    let p = gtk::Popover::builder()
        .has_arrow(false)
        .child(child)
        .build();
    p.add_css_class("menu-popover");
    if std::env::var_os("ASST_NO_AUTOHIDE").is_some() {
        p.set_autohide(false);
    }
    track(&p);
    p
}

/// A popover of menu items, 250 px wide as Planify's are.
pub fn menu(items: &[gtk::Widget]) -> gtk::Popover {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_margin_top(3);
    b.set_margin_bottom(3);
    for i in items {
        b.append(i);
    }
    let p = popover(&b);
    p.set_width_request(250);
    p
}

pub fn separator() -> gtk::Widget {
    let s = gtk::Separator::new(gtk::Orientation::Horizontal);
    s.set_margin_top(3);
    s.set_margin_bottom(3);
    s.add_css_class("menu-separator");
    s.upcast()
}

pub fn popdown(w: &impl IsA<gtk::Widget>) {
    if let Some(p) = w
        .ancestor(gtk::Popover::static_type())
        .and_downcast::<gtk::Popover>()
    {
        p.popdown();
    }
}

pub struct Item<'a> {
    pub icon: Option<&'a str>,
    /// A CSS class for the icon (a priority color).
    pub tint: Option<&'a str>,
    pub title: &'a str,
    pub secondary: Option<&'a str>,
    pub checked: bool,
    pub arrow: bool,
    pub danger: bool,
    /// Close the popover it is in when clicked.
    pub close: bool,
}

impl<'a> Item<'a> {
    pub fn new(icon: Option<&'a str>, title: &'a str) -> Item<'a> {
        Item {
            icon,
            tint: None,
            title,
            secondary: None,
            checked: false,
            arrow: false,
            danger: false,
            close: true,
        }
    }

    pub fn tint(mut self, class: &'a str) -> Self {
        self.tint = Some(class);
        self
    }

    pub fn secondary(mut self, s: &'a str) -> Self {
        self.secondary = Some(s);
        self
    }

    pub fn checked(mut self, c: bool) -> Self {
        self.checked = c;
        self
    }

    pub fn arrow(mut self) -> Self {
        self.arrow = true;
        self.close = false;
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn stay(mut self) -> Self {
        self.close = false;
        self
    }

    pub fn build(self, on: impl Fn() + 'static) -> gtk::Button {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        if let Some(i) = self.icon {
            let image = gtk::Image::from_icon_name(i);
            if let Some(t) = self.tint {
                image.add_css_class(t);
            }
            content.append(&image);
        }
        content.append(
            &gtk::Label::builder()
                .label(self.title)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        if let Some(s) = self.secondary {
            content.append(&label(s, &["dim-label"]));
        }
        if self.checked {
            content.append(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        if self.arrow {
            content.append(&gtk::Image::from_icon_name("go-next-symbolic"));
        }
        let b = gtk::Button::builder().child(&content).build();
        b.add_css_class("flat");
        b.add_css_class("menu-item");
        if self.danger {
            b.add_css_class("menu-item-danger");
        }
        let close = self.close;
        b.connect_clicked(move |b| {
            if close {
                popdown(b);
            }
            on();
        });
        b
    }
}

// -- chips, rings -----------------------------------------------------------

/// A due date in a colored lozenge (`today`, `overdue`, `upcoming`, `done`).
pub fn chip(text: &str, class: &str, repeat: bool) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    b.add_css_class("due-chip");
    b.add_css_class(class);
    b.set_valign(gtk::Align::Center);
    b.append(&gtk::Label::builder().label(text).build());
    if repeat {
        b.append(&icon("playlist-repeat-symbolic", 12));
    }
    b
}

/// A list's color as Planify draws a project: a ring around a dot.
pub fn ring(color: Option<&str>, size: i32) -> gtk::DrawingArea {
    progress_ring(color, size, Rc::new(Cell::new(1.0)))
}

/// The ring with its dot filled like a pie, as far as `done` (0 to 1) says:
/// Planify's picture of how much of a list is done. Queue a draw after
/// changing it.
pub fn progress_ring(color: Option<&str>, size: i32, done: Rc<Cell<f64>>) -> gtk::DrawingArea {
    let rgba = color
        .and_then(|c| gdk::RGBA::parse(c).ok())
        .unwrap_or(gdk::RGBA::new(0.6, 0.6, 0.6, 1.0));
    let area = gtk::DrawingArea::builder()
        .content_width(size)
        .content_height(size)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |_, cr, w, h| {
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let r = f64::from(w.min(h)) / 2.0 - 1.25;
        cr.set_source_rgba(
            rgba.red().into(),
            rgba.green().into(),
            rgba.blue().into(),
            rgba.alpha().into(),
        );
        cr.set_line_width(2.0);
        cr.arc(cx, cy, r, 0.0, 2.0 * PI);
        let _ = cr.stroke();
        let done = done.get().clamp(0.0, 1.0);
        let inner = (r - 3.0).max(1.0);
        if done >= 1.0 {
            cr.arc(cx, cy, inner, 0.0, 2.0 * PI);
        } else if done > 0.0 {
            // From twelve o'clock, clockwise.
            cr.move_to(cx, cy);
            cr.arc(cx, cy, inner, -PI / 2.0, -PI / 2.0 + 2.0 * PI * done);
            cr.close_path();
        }
        let _ = cr.fill();
    });
    area
}

/// A tooltip with its key: `Search` and `Ctrl+F` under it.
pub fn tip(w: &impl IsA<gtk::Widget>, text: &str, keys: &str) {
    w.set_tooltip_markup(Some(&format!(
        "{}\n<span size=\"small\" alpha=\"70%\">{}</span>",
        glib::markup_escape_text(text),
        glib::markup_escape_text(keys)
    )));
}
