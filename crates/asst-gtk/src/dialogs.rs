//! Dialogs: a list's name and color, deleting a list, keyboard shortcuts,
//! about.

use std::cell::RefCell;
use std::rc::Rc;

use asst_core::api::ListView;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gdk};

use crate::ui;

/// New list (`list` is None) or edit one. `on_save` gets the name and color.
pub fn list_dialog(
    parent: &impl IsA<gtk::Widget>,
    list: Option<&ListView>,
    on_save: impl Fn(String, String) + 'static,
) {
    let editing = list.is_some();
    let dialog = adw::Dialog::builder()
        .title(if editing { "Edit List" } else { "New List" })
        .content_width(420)
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label(if editing { "Save" } else { "Add List" });
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let name = adw::EntryRow::builder()
        .title("Name")
        .text(list.map_or("", |l| l.name.as_str()))
        .build();
    group.add(&name);
    page.add(&group);

    let colors = adw::PreferencesGroup::builder().title("Color").build();
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(10)
        .min_children_per_line(5)
        .row_spacing(6)
        .column_spacing(6)
        .homogeneous(true)
        .build();
    let chosen = Rc::new(RefCell::new(
        list.and_then(|l| l.color.clone())
            .unwrap_or_else(|| ui::COLORS[11].1.to_string()),
    ));
    let mut first: Option<gtk::ToggleButton> = None;
    for (label, hex) in ui::COLORS {
        let swatch = gtk::DrawingArea::builder()
            .content_width(24)
            .content_height(24)
            .build();
        let rgba = gdk::RGBA::parse(hex).expect("a color");
        swatch.set_draw_func(move |_, cr, w, h| {
            let r = f64::from(w.min(h)) / 2.0;
            cr.set_source_rgba(
                rgba.red().into(),
                rgba.green().into(),
                rgba.blue().into(),
                1.0,
            );
            cr.arc(
                f64::from(w) / 2.0,
                f64::from(h) / 2.0,
                r,
                0.0,
                2.0 * std::f64::consts::PI,
            );
            let _ = cr.fill();
        });
        let b = gtk::ToggleButton::builder()
            .child(&swatch)
            .tooltip_text(label)
            .build();
        b.add_css_class("flat");
        b.add_css_class("circular");
        b.add_css_class("color-swatch");
        match &first {
            Some(f) => b.set_group(Some(f)),
            None => first = Some(b.clone()),
        }
        if chosen.borrow().eq_ignore_ascii_case(hex) {
            b.set_active(true);
        }
        let chosen = chosen.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                *chosen.borrow_mut() = hex.to_string();
            }
        });
        flow.append(&b);
    }
    colors.add(&flow);
    page.add(&colors);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));

    let submit = {
        let dialog = dialog.clone();
        let name = name.clone();
        move || {
            let text = name.text().trim().to_string();
            if text.is_empty() {
                name.add_css_class("error");
                return;
            }
            on_save(text, chosen.borrow().clone());
            dialog.close();
        }
    };
    let submit = Rc::new(submit);
    {
        let submit = submit.clone();
        save.connect_clicked(move |_| submit());
    }
    {
        let submit = submit.clone();
        name.connect_entry_activated(move |_| submit());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    dialog.present(Some(parent));
    name.grab_focus();
}

pub fn delete_list_dialog(
    parent: &impl IsA<gtk::Widget>,
    list: &ListView,
    open: usize,
    on_delete: impl Fn() + 'static,
) {
    let body = match open {
        0 => "Nextcloud keeps deleted lists in its trash bin for a while, where they can be restored from the web.".to_string(),
        1 => "Its 1 open task goes with it, on every device. Nextcloud keeps deleted lists in its trash bin for a while.".to_string(),
        n => format!("Its {n} open tasks go with it, on every device. Nextcloud keeps deleted lists in its trash bin for a while."),
    };
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Delete “{}”?", list.name))
        .body(body)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete List");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            on_delete();
        }
    });
    dialog.present(Some(parent));
}

/// `from` names where they are: a quoted list name, or "every list".
pub fn delete_completed_dialog(
    parent: &impl IsA<gtk::Widget>,
    n: usize,
    from: &str,
    on_delete: impl Fn() + 'static,
) {
    let heading = match n {
        1 => "Delete 1 completed task?".to_string(),
        n => format!("Delete {n} completed tasks?"),
    };
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(format!(
            "They go from {from} on every device, the iPhone too. Nextcloud keeps deleted tasks in its trash bin for a while."
        ))
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            on_delete();
        }
    });
    dialog.present(Some(parent));
}

pub fn shortcuts(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::ShortcutsDialog::new();
    let sections: [(&str, &[(&str, &str)]); 4] = [
        (
            "Moving around",
            &[
                ("Next / previous task", "j k"),
                ("First / last task", "g G"),
                ("Previous / next view", "bracketleft bracketright"),
                ("Inbox", "<Control>i"),
                ("Today", "<Control>t"),
                ("Scheduled", "<Control>u"),
                ("A list in the sidebar", "<Control>1...<Control>9"),
                ("Quick Find", "<Control>f slash"),
                ("Show or hide the sidebar", "<Control>b"),
            ],
        ),
        (
            "Tasks",
            &[
                ("Add a task", "a"),
                ("Open the selected task", "e Return"),
                ("Complete or reopen", "x space"),
                ("Delete", "d+d"),
                ("Priority 1–4", "1...4"),
                ("Due today / tomorrow", "t m"),
                ("Next week", "w"),
                ("Select tasks", "v"),
                ("From the title to the notes", "Tab"),
                (
                    "Pick a date or reminders for the open task",
                    "<Control>d <Control>r",
                ),
                ("Close the task or selection", "Escape"),
            ],
        ),
        (
            "Adding",
            &[
                ("Add and keep going", "Return"),
                ("Add from the notes", "<Control>Return"),
                ("Add a task from the clipboard", "<Control>v"),
                ("Pick a date", "<Control>d"),
                ("Pick reminders", "<Control>r"),
                ("Keep quick add open", "<Control>k"),
                ("Stop adding", "Escape"),
            ],
        ),
        (
            "General",
            &[
                ("New list", "p"),
                ("Sync now", "s"),
                ("Preferences", "<Control>comma"),
                ("Keyboard shortcuts", "question"),
                ("Close the window", "<Control>w"),
                ("Quit", "<Control>q"),
            ],
        ),
    ];
    for (title, items) in sections {
        let section = adw::ShortcutsSection::new(Some(title));
        for (what, keys) in items {
            section.add(adw::ShortcutsItem::new(what, keys));
        }
        dialog.add(section);
    }
    dialog.present(Some(parent));
}

pub fn about(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::AboutDialog::builder()
        .application_name("asst")
        .application_icon(asst_core::config::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Jaeho Cho")
        .license_type(gtk::License::MitX11)
        .comments("Tasks and reminders on Nextcloud, in step with Apple Reminders on the iPhone.")
        .build();
    dialog.add_credit_section(
        Some("Inspired by"),
        &["Planify by Alain M. https://github.com/alainm23/planify"],
    );
    dialog.present(Some(parent));
}
