//! The notes field's bit of Markdown. Enter carries a list on: `- `, `* `,
//! `• `, `- [ ] ` and `1. ` (numbered on), and on an item left empty it ends
//! the list instead. Links look like links and open with Ctrl+click. The text
//! itself stays plain, since the iPhone shows notes as they are.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use relm4::adw;
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gdk, pango};

/// What a newline typed after `line` (the text before it on its line) does.
#[derive(Debug, PartialEq, Eq)]
pub enum Enter {
    /// Start the next item with this.
    Continue(String),
    /// The item was left empty: take its marker away, and the newline.
    End,
}

pub fn on_enter(line: &str) -> Option<Enter> {
    let rest = line.trim_start_matches([' ', '\t']);
    let indent = &line[..line.len() - rest.len()];
    let bullet = ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "• "]
        .into_iter()
        .find(|m| rest.starts_with(m));
    let (next, body) = match bullet {
        // A checked item's next one starts unchecked.
        Some(m) if m.starts_with("- [") => ("- [ ] ".to_string(), &rest[m.len()..]),
        Some(m) => (m.to_string(), &rest[m.len()..]),
        None => {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let sep = rest.get(digits..digits + 2)?;
            if !(1..=3).contains(&digits) || !matches!(sep, ". " | ") ") {
                return None;
            }
            let n: u32 = rest[..digits].parse().ok()?;
            (format!("{}{sep}", n + 1), &rest[digits + 2..])
        }
    };
    Some(if body.trim().is_empty() {
        Enter::End
    } else {
        Enter::Continue(format!("{indent}{next}"))
    })
}

/// Where the links in `text` are, as byte ranges: runs starting `https://`,
/// `http://`, `mailto:` or `www.`, without the punctuation a sentence puts
/// after one.
pub fn links(text: &str) -> Vec<Range<usize>> {
    const STARTS: [&str; 4] = ["https://", "http://", "mailto:", "www."];
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        let word_start = !text[..i]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        let prefix = STARTS.iter().find(|s| {
            rest.get(..s.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(s))
        });
        let Some(prefix) = prefix.filter(|_| word_start) else {
            i += c.len_utf8();
            continue;
        };
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
            .unwrap_or(rest.len());
        let mut link = &rest[..end];
        loop {
            let mut trimmed = link.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '*', '_']);
            // A bracket closing what the link didn't open, as in "(see https://x)".
            for (open, close) in [('(', ')'), ('[', ']')] {
                if trimmed.ends_with(close)
                    && trimmed.matches(open).count() < trimmed.matches(close).count()
                {
                    trimmed = &trimmed[..trimmed.len() - 1];
                }
            }
            if trimmed.len() == link.len() {
                break;
            }
            link = trimmed;
        }
        if link.len() > prefix.len() {
            out.push(i..i + link.len());
        }
        i += end.max(c.len_utf8());
    }
    out
}

/// What to open for a link as written.
pub fn target(link: &str) -> String {
    if link
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("www."))
    {
        format!("https://{link}")
    } else {
        link.to_string()
    }
}

/// Lists and links in a notes field.
pub fn enhance(view: &gtk::TextView) {
    let buffer = view.buffer();
    let tag = gtk::TextTag::builder()
        .name("link")
        .underline(pango::Underline::Single)
        .build();
    buffer.tag_table().add(&tag);
    let style = adw::StyleManager::default();
    let tint = {
        let tag = tag.clone();
        move |s: &adw::StyleManager| {
            let color = if s.is_dark() { "#78aeed" } else { "#1c71d8" };
            tag.set_foreground_rgba(gdk::RGBA::parse(color).ok().as_ref());
        }
    };
    tint(&style);
    style.connect_dark_notify(tint);

    // Where a newline was just typed, for `changed` to carry the list on:
    // the buffer can't be changed from inside the insert itself.
    let newline: Rc<Cell<Option<i32>>> = Rc::default();
    {
        let newline = newline.clone();
        buffer.connect_insert_text(move |_, at, text| {
            newline.set((text == "\n").then(|| at.offset()));
        });
    }
    {
        let tag = tag.clone();
        buffer.connect_changed(move |b| {
            if let Some(at) = newline.take() {
                continue_list(b, at);
            }
            mark_links(b, &tag);
        });
    }

    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let view = view.clone();
        click.connect_pressed(move |g, _, x, y| {
            if !g
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                return;
            }
            if let Some(link) = link_at(&view, x, y) {
                g.set_state(gtk::EventSequenceState::Claimed);
                let parent = view.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(&target(&link)).launch(
                    parent.as_ref(),
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            }
        });
    }
    view.add_controller(click);

    let pointer = gtk::EventControllerMotion::new();
    {
        let view = view.clone();
        pointer.connect_motion(move |m, x, y| {
            let ctrl = m
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            let cursor = if ctrl && link_at(&view, x, y).is_some() {
                "pointer"
            } else {
                "text"
            };
            view.set_cursor_from_name(Some(cursor));
        });
    }
    view.add_controller(pointer);
    view.set_has_tooltip(true);
    view.connect_query_tooltip(|view, x, y, _, tip| {
        if link_at(view, f64::from(x), f64::from(y)).is_none() {
            return false;
        }
        tip.set_text(Some("Ctrl+click to open"));
        true
    });
}

fn continue_list(b: &gtk::TextBuffer, at: i32) {
    let end = b.iter_at_offset(at);
    let mut start = end;
    start.set_line_offset(0);
    let line = b.text(&start, &end, false);
    match on_enter(&line) {
        Some(Enter::Continue(marker)) => {
            let mut cursor = b.iter_at_offset(at + 1);
            b.insert(&mut cursor, &marker);
        }
        Some(Enter::End) => {
            let mut from = b.iter_at_offset(at - line.chars().count() as i32);
            let mut to = b.iter_at_offset(at + 1);
            b.delete(&mut from, &mut to);
        }
        None => {}
    }
}

fn mark_links(b: &gtk::TextBuffer, tag: &gtk::TextTag) {
    let (start, end) = b.bounds();
    b.remove_tag(tag, &start, &end);
    let text = b.text(&start, &end, false);
    for range in links(&text) {
        let offset = |byte: usize| text[..byte].chars().count() as i32;
        b.apply_tag(
            tag,
            &b.iter_at_offset(offset(range.start)),
            &b.iter_at_offset(offset(range.end)),
        );
    }
}

/// The link under a point in the view, if there is one.
fn link_at(view: &gtk::TextView, x: f64, y: f64) -> Option<String> {
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    let iter = view.iter_at_location(bx, by)?;
    let buffer = view.buffer();
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, false);
    let at = text
        .char_indices()
        .nth(iter.offset() as usize)
        .map_or(text.len(), |(i, _)| i);
    links(&text)
        .into_iter()
        .find(|r| r.contains(&at))
        .map(|r| text[r].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_carry_on() {
        let go = |s: &str| Enter::Continue(s.into());
        assert_eq!(on_enter("- milk"), Some(go("- ")));
        assert_eq!(on_enter("  * eggs"), Some(go("  * ")));
        assert_eq!(on_enter("- [x] call"), Some(go("- [ ] ")));
        assert_eq!(on_enter("9. step"), Some(go("10. ")));
        assert_eq!(on_enter("2) step"), Some(go("3) ")));
        assert_eq!(on_enter("- "), Some(Enter::End));
        assert_eq!(on_enter("3. "), Some(Enter::End));
        assert_eq!(on_enter("Just a line"), None);
        assert_eq!(on_enter("2026. A year"), None);
        assert_eq!(on_enter("-dash"), None);
    }

    #[test]
    fn links_without_their_punctuation() {
        let found =
            |t: &str| -> Vec<String> { links(t).into_iter().map(|r| t[r].to_string()).collect() };
        assert_eq!(
            found("See https://example.com/a?b=1, then www.rust-lang.org."),
            ["https://example.com/a?b=1", "www.rust-lang.org"]
        );
        assert_eq!(
            found("(docs at https://en.wikipedia.org/wiki/Rust_(programming_language))"),
            ["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
        assert_eq!(
            found("[site](https://a.b/c) mailto:me@x.org"),
            ["https://a.b/c", "mailto:me@x.org"]
        );
        assert_eq!(
            found("nothttps://x.y https:// é https://ü.de"),
            ["https://ü.de"]
        );
        assert_eq!(target("www.a.org"), "https://www.a.org");
    }
}
