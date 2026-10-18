//! Text output: one line per task, colored on a terminal.

use std::io::IsTerminal;

use asst_core::api::{LinkView, TaskView};
use asst_core::fmt;
use asst_core::task::priority_level;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

pub struct Style {
    color: bool,
}

impl Style {
    pub fn detect() -> Style {
        Style {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", text)
    }

    pub fn blue(&self, text: &str) -> String {
        self.paint("34", text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn is_terminal(&self) -> bool {
        self.color
    }
}

/// `a1b2  !! Call the bank  tomorrow 5pm ↻  #Errands`
pub fn task_line(s: &Style, t: &TaskView, now: DateTime<Tz>, show_list: bool) -> String {
    let marker = match priority_level(t.task.priority) {
        1 => s.red("!!!"),
        2 => s.yellow("!! "),
        3 => s.blue("!  "),
        _ => "   ".to_string(),
    };
    let mut line = format!("{}  {} ", s.dim(&format!("{:<6}", t.id)), marker);
    if t.task.is_open() {
        line.push_str(&t.task.summary);
    } else {
        line.push_str(&s.dim(&format!("✓ {}", t.task.summary)));
    }
    if let Some(due) = &t.task.due {
        let label = fmt::due_label(due, now);
        let label = if !t.task.is_open() {
            s.dim(&label)
        } else if fmt::is_overdue(due, now) {
            s.red(&label)
        } else if fmt::is_today(due, now) {
            s.yellow(&label)
        } else {
            s.green(&label)
        };
        line.push_str("  ");
        line.push_str(&label);
    }
    if t.task.rrule.is_some() {
        line.push_str(&s.dim(" ↻"));
    }
    if show_list {
        line.push_str(&s.dim(&format!("  #{}", t.list_name.replace(' ', ""))));
    }
    if t.pending {
        line.push_str(&s.dim(" *"));
    }
    line
}

pub fn ago(at: DateTime<Utc>) -> String {
    let secs = (Utc::now() - at).num_seconds().max(0);
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// The line under a link that needs a look: what it is, in a sentence. A
/// link whose issues are not being read is yellow.
pub fn link_note(s: &Style, l: &LinkView) -> Option<String> {
    let (state, note) = (l.state?, l.note.as_deref()?);
    let line = format!("  {note}");
    Some(if state.stalls() {
        s.yellow(&line)
    } else {
        s.dim(&line)
    })
}

/// How many links need a look, in a sentence: `1 link needs a look`.
fn needing_a_look(links: &[LinkView]) -> Option<String> {
    match links.iter().filter(|l| l.state.is_some()).count() {
        0 => None,
        1 => Some("1 link needs a look".into()),
        n => Some(format!("{n} links need a look")),
    }
}

/// The last line of `asst link` when any link needs a look.
pub fn links_hint(s: &Style, links: &[LinkView]) -> Option<String> {
    needing_a_look(links).map(|hint| s.dim(&hint))
}

/// The GitHub line of `asst status`, when any link needs a look: how many,
/// and which (`archived: jaehho/mail-digest`).
pub fn links_status(s: &Style, links: &[LinkView]) -> Option<String> {
    let hint = needing_a_look(links)?;
    let which: Vec<String> = links
        .iter()
        .filter_map(|l| Some(format!("{}: {}", l.state?.as_str(), l.repo)))
        .collect();
    let line = format!("GitHub: {hint} ({})", which.join("; "));
    Some(
        if links.iter().any(|l| l.state.is_some_and(|st| st.stalls())) {
            s.yellow(&line)
        } else {
            line
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use asst_core::api::LinkState;

    fn plain() -> Style {
        Style { color: false }
    }

    fn link(repo: &str, state: Option<LinkState>, note: &str) -> LinkView {
        LinkView {
            repo: repo.into(),
            list: "/cal/x/".into(),
            name: "x".into(),
            state,
            note: state.map(|_| note.to_string()),
            ..LinkView::default()
        }
    }

    #[test]
    fn a_link_that_is_fine_says_nothing() {
        let links = [link("jaehho/lab", None, "")];
        assert_eq!(link_note(&plain(), &links[0]), None);
        assert_eq!(links_hint(&plain(), &links), None);
        assert_eq!(links_status(&plain(), &links), None);
        assert_eq!(links_hint(&plain(), &[]), None);
    }

    #[test]
    fn a_link_that_needs_a_look_has_its_note_and_the_links_a_hint() {
        let archived = link(
            "jaehho/mail-digest",
            Some(LinkState::Archived),
            "first seen archived 2026-10-18; 0 open tasks",
        );
        let links = [link("jaehho/lab", None, ""), archived.clone()];
        assert_eq!(
            link_note(&plain(), &archived).as_deref(),
            Some("  first seen archived 2026-10-18; 0 open tasks")
        );
        assert_eq!(
            links_hint(&plain(), &links).as_deref(),
            Some("1 link needs a look")
        );
        assert_eq!(
            links_status(&plain(), &links).as_deref(),
            Some("GitHub: 1 link needs a look (archived: jaehho/mail-digest)")
        );

        let replaced = link("jaehho/x", Some(LinkState::Replaced), "another repository");
        let links = [archived, replaced];
        assert_eq!(
            links_hint(&plain(), &links).as_deref(),
            Some("2 links need a look")
        );
        assert_eq!(
            links_status(&plain(), &links).as_deref(),
            Some(
                "GitHub: 2 links need a look \
                 (archived: jaehho/mail-digest; replaced: jaehho/x)"
            )
        );
    }

    #[test]
    fn a_link_whose_issues_are_not_read_is_yellow() {
        let color = Style { color: true };
        let archived = link("jaehho/a", Some(LinkState::Archived), "n");
        let stuck = link("jaehho/b", Some(LinkState::Conflict), "n");
        assert_eq!(link_note(&color, &archived).unwrap(), "\x1b[2m  n\x1b[0m");
        assert_eq!(link_note(&color, &stuck).unwrap(), "\x1b[33m  n\x1b[0m");
        assert!(
            links_status(&color, std::slice::from_ref(&archived))
                .unwrap()
                .starts_with("GitHub:")
        );
        assert!(
            links_status(&color, &[archived, stuck])
                .unwrap()
                .starts_with("\x1b[33m")
        );
    }
}
