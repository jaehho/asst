//! Notes linked to a task, in the window: their names under the task's own
//! notes, each opening its file in whatever the desktop opens Markdown with,
//! and the picker that links a note from the notes folder or makes a new one.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use asst_core::note_files;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk::{self, gio, pango};

use crate::{model, pickers, ui};

/// What was chosen in the picker.
pub enum Pick {
    /// A new note, titled after the task, or this.
    New(Option<String>),
    Attach(String),
    Detach(String),
}

/// Open a note. Development builds only say which, so a scripted check
/// doesn't put an editor on the screen.
pub fn open(file: &Path, parent: Option<&gtk::Window>) {
    if crate::devel::enabled() {
        eprintln!("open-note: {}", file.display());
        return;
    }
    gtk::FileLauncher::new(Some(&gio::File::for_path(file))).launch(
        parent,
        None::<&gio::Cancellable>,
        |result| {
            if let Err(e) = result {
                eprintln!("opening a note: {e}");
            }
        },
    );
}

/// The file a link names: a path in `dir`, or already a full path (quick
/// add, started from an editor).
fn file_of(dir: Option<&Path>, link: &str) -> Option<PathBuf> {
    if Path::new(link).is_absolute() {
        return Some(PathBuf::from(link));
    }
    dir.map(|d| d.join(link))
}

/// A task's linked notes as a row of names in `pills`: a click opens one,
/// and its cross unlinks it when `unlink` is given. A note that is no longer
/// in the notes folder is shown struck through.
pub fn fill(
    pills: &adw::WrapBox,
    dir: Option<&Path>,
    links: &[String],
    unlink: Option<Rc<dyn Fn(String)>>,
) {
    pills.remove_all();
    for link in links {
        let file = file_of(dir, link);
        let missing = file.as_ref().is_some_and(|f| !f.exists());
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("note-pill");
        if missing {
            pill.add_css_class("missing");
        }
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&ui::icon("paper-symbolic", 14));
        content.append(
            &gtk::Label::builder()
                .label(note_files::name(link))
                .max_width_chars(28)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        let button = gtk::Button::builder().child(&content).build();
        button.add_css_class("flat");
        button.set_tooltip_text(Some(&if missing {
            format!("Not in the notes folder any more: {link}")
        } else {
            format!("Open {link}")
        }));
        button.connect_clicked(move |b| {
            if let Some(f) = file.as_ref().filter(|f| f.exists()) {
                open(f, b.root().and_downcast::<gtk::Window>().as_ref());
            }
        });
        pill.append(&button);
        if let Some(unlink) = &unlink {
            button.add_css_class("with-unlink");
            let cross = gtk::Button::from_icon_name("window-close-symbolic");
            cross.add_css_class("flat");
            cross.add_css_class("circular");
            cross.add_css_class("note-pill-unlink");
            cross.set_valign(gtk::Align::Center);
            cross.set_tooltip_text(Some("Unlink"));
            let (unlink, link) = (unlink.clone(), link.clone());
            cross.connect_clicked(move |_| unlink(link.clone()));
            pill.append(&cross);
        }
        pills.append(&pill);
    }
    pills.set_visible(!links.is_empty());
}

/// Link a note or make one: a new note first, then the notes in the folder,
/// newest first, found by name or folder. Those linked already are checked,
/// and choosing one unlinks it.
pub fn popover(
    dir: Option<&Path>,
    linked: &[String],
    on_pick: impl Fn(Pick) + 'static,
) -> gtk::Popover {
    let on_pick: Rc<dyn Fn(Pick)> = Rc::new(on_pick);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(9);
    root.set_margin_bottom(9);
    root.set_margin_start(9);
    root.set_margin_end(9);

    let exists = dir.is_some_and(Path::is_dir);
    let new_label = ui::label("New note", &[]);
    new_label.set_hexpand(true);
    let new_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    new_content.append(&gtk::Image::from_icon_name("plus-large-symbolic"));
    new_content.append(&new_label);
    let new = gtk::Button::builder().child(&new_content).build();
    new.add_css_class("flat");
    new.add_css_class("menu-item");
    new.set_sensitive(exists);
    new.set_tooltip_text(Some("A note in the notes folder, named after the task"));
    root.append(&new);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Link a note…")
        .build();
    let listbox = gtk::ListBox::new();
    listbox.add_css_class("picker-list");
    listbox.set_selection_mode(gtk::SelectionMode::None);
    let notes = dir.map(note_files::list).unwrap_or_default();
    let now = pickers::now();
    for note in &notes {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.set_margin_top(4);
        line.set_margin_bottom(4);
        line.set_margin_start(6);
        line.set_margin_end(6);
        let icon = ui::icon("paper-symbolic", 16);
        icon.set_valign(gtk::Align::Center);
        line.append(&icon);
        let words = gtk::Box::new(gtk::Orientation::Vertical, 0);
        words.set_hexpand(true);
        words.append(&ui::label(note_files::name(&note.path), &[]));
        let mut about: Vec<String> = Vec::new();
        let folder = note_files::folder(&note.path);
        if !folder.is_empty() {
            about.push(folder.to_string());
        }
        if let Some(m) = note.modified {
            about.push(model::ago(m, now));
        }
        if !about.is_empty() {
            words.append(&ui::label(&about.join(" · "), &["caption", "dim-label"]));
        }
        line.append(&words);
        if linked.contains(&note.path) {
            line.append(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        row.set_child(Some(&line));
        row.set_tooltip_text(Some(&note.path));
        row.set_widget_name(&note.path.to_lowercase());
        listbox.append(&row);
    }
    if exists && !notes.is_empty() {
        root.append(&search);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&listbox)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .build();
        root.append(&scroller);
    } else {
        let text = match dir {
            Some(d) if exists => format!("No notes in {} yet.", pretty(d)),
            Some(d) => format!(
                "{} doesn't exist. Choose the notes folder in Preferences.",
                pretty(d)
            ),
            None => "The notes folder isn't known: asstd isn't answering.".into(),
        };
        let empty = ui::label(&text, &["dim-label"]);
        empty.set_wrap(true);
        empty.set_ellipsize(pango::EllipsizeMode::None);
        empty.set_margin_start(6);
        empty.set_margin_end(6);
        root.append(&empty);
    }

    let popover = ui::popover(&root);
    popover.set_width_request(320);
    let paths: Rc<Vec<String>> = Rc::new(notes.into_iter().map(|n| n.path).collect());
    {
        let (on_pick, search, popover) = (on_pick.clone(), search.clone(), popover.clone());
        new.connect_clicked(move |_| {
            let title = Some(search.text().trim().to_string()).filter(|t| !t.is_empty());
            popover.popdown();
            on_pick(Pick::New(title));
        });
    }
    {
        let (paths, popover) = (paths.clone(), popover.clone());
        let linked = linked.to_vec();
        listbox.connect_row_activated(move |_, row| {
            let Some(path) = paths.get(row.index() as usize) else {
                return;
            };
            popover.popdown();
            on_pick(if linked.contains(path) {
                Pick::Detach(path.clone())
            } else {
                Pick::Attach(path.clone())
            });
        });
    }
    {
        let listbox = listbox.clone();
        search.connect_search_changed(move |e| {
            let q = e.text().trim().to_lowercase();
            if q.is_empty() {
                new_label.set_text("New note");
            } else {
                new_label.set_text(&format!("New note “{}”", e.text().trim()));
            }
            let mut i = 0;
            while let Some(row) = listbox.row_at_index(i) {
                row.set_visible(row.widget_name().contains(q.as_str()));
                i += 1;
            }
        });
    }
    {
        let listbox = listbox.clone();
        let new = new.clone();
        search.connect_activate(move |_| {
            let mut i = 0;
            while let Some(row) = listbox.row_at_index(i) {
                if row.is_visible() {
                    row.activate();
                    return;
                }
                i += 1;
            }
            // Nothing by that name: make it.
            new.emit_clicked();
        });
    }
    {
        let (search, new) = (search.clone(), new.clone());
        popover.connect_map(move |p| {
            search.set_text("");
            if search.parent().is_some() {
                pickers::focus_in(p, &search);
            } else {
                pickers::focus_in(p, &new);
            }
        });
    }
    popover
}

/// A folder as a person reads it: `~/Nextcloud/Notes`.
pub fn pretty(dir: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => dir.display().to_string(),
    }
}
