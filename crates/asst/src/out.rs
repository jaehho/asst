//! Text output: one line per task, colored on a terminal.

use std::io::IsTerminal;

use asst_core::api::TaskView;
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
