//! Text output: one line per task, colored on a terminal.

use std::io::IsTerminal;

use asst_core::api::{LinkView, TaskView};
use asst_core::fmt;
use asst_core::github::{FollowRename, short};
use asst_core::store::LogRow;
use asst_core::task::priority_level;
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;

use crate::projects::{Pending, ProjectLink, Report};

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

/// The last line of `asst link` when any link needs a look, and where to
/// look.
pub fn links_hint(s: &Style, links: &[LinkView]) -> Option<String> {
    needing_a_look(links).map(|hint| s.dim(&format!("{hint}: asst projects")))
}

/// `asst link` with no arguments: each link, the note of one that needs a
/// look under it, and the hint at `asst projects` after the last.
pub fn links_text(s: &Style, links: &[LinkView]) -> String {
    if links.is_empty() {
        return s.dim("no list is linked; `asst link <list> <owner/repo>`");
    }
    let mut lines: Vec<String> = Vec::new();
    for l in links {
        lines.push(format!("{} ⇄ {}", l.name, l.repo));
        lines.extend(link_note(s, l));
    }
    lines.extend(links_hint(s, links));
    lines.join("\n")
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

/// What `asst settings` says of `follow_rename`: the word, and what it does.
pub fn follow_rename_words(f: FollowRename) -> String {
    match f {
        FollowRename::List => "list (a renamed repo renames its list)".into(),
        FollowRename::Off => "off (a rename is only reported)".into(),
    }
}

/// A day as the person reads it, in their zone.
fn day(at: DateTime<Utc>, zone: Tz) -> String {
    at.with_timezone(&zone).format("%Y-%m-%d").to_string()
}

/// `asst projects`: the links as a table, then what is pending, what could
/// be done about it, and which labels are behind. A section with nothing in
/// it is left out.
pub fn projects_text(s: &Style, r: &Report, zone: Tz) -> String {
    let mut out: Vec<String> = Vec::new();
    if r.links.is_empty() {
        out.push(s.dim("no list is linked; `asst link <list> <owner/repo>`"));
    } else {
        out.extend(links_table(s, &r.links));
    }

    let pending: Vec<String> = r
        .links
        .iter()
        .filter_map(|l| Some(pending_line(l, l.pending_rename.as_ref()?, r, zone)))
        .collect();
    if !pending.is_empty() {
        out.push(String::new());
        out.push(s.bold("Pending"));
        out.extend(pending);
    }

    if !r.proposals.is_empty() {
        out.push(String::new());
        out.push(s.bold("Proposals (nothing has been changed)"));
        for p in &r.proposals {
            out.push(format!("  {}", p.why));
            out.push(format!("    {}", p.command));
        }
    }

    if !r.labels.is_empty() {
        out.push(String::new());
        out.push(s.bold("Labels"));
        for l in &r.labels {
            let blocks = match l.blocks {
                1 => "1 block".to_string(),
                n => format!("{n} blocks"),
            };
            out.push(format!(
                "  {blocks} ({:.1} h) still say {:?}",
                l.seconds as f64 / 3600.0,
                l.label
            ));
            out.push(format!(
                "    new days: add [[rule]] project = {:?} tag = {:?} to ~/.config/asst/tags.toml",
                l.label, l.now
            ));
            out.push("    history: the relabel recipe in README, \"Renaming a project\"".into());
        }
    }

    if !r.warnings.is_empty() {
        out.push(String::new());
        out.extend(r.warnings.iter().map(|w| s.yellow(w)));
    }
    out.join("\n")
}

/// How far back `log_pointer` looks.
const RECENT_DAYS: i64 = 7;

/// The last line of `asst projects` when asstd has logged something
/// lately: that there is a record of what it did by itself, and where. A
/// list renamed on the phone is otherwise a change with nowhere to be read
/// about it. Nothing when the log is quiet. `rows` is newest first.
pub fn log_pointer(s: &Style, rows: &[LogRow], now: DateTime<Utc>, zone: Tz) -> Option<String> {
    let since = now - Duration::days(RECENT_DAYS);
    let recent: Vec<DateTime<Utc>> = rows
        .iter()
        .filter_map(|r| DateTime::from_timestamp(r.at, 0))
        .filter(|at| *at > since)
        .collect();
    let latest = recent.iter().max()?;
    let count = match recent.len() {
        1 => "1 row".to_string(),
        n => format!("{n} rows"),
    };
    Some(s.dim(&format!(
        "asstd logged {count} in the last {RECENT_DAYS} days, the latest on {}: asst projects log",
        day(*latest, zone)
    )))
}

/// `LIST  REPO  STATE  NOTE`, each column as wide as what is in it.
fn links_table(s: &Style, links: &[ProjectLink]) -> Vec<String> {
    let width = |head: &str, cell: fn(&ProjectLink) -> &str| {
        links
            .iter()
            .map(|l| cell(l).chars().count())
            .chain([head.len()])
            .max()
            .unwrap_or(0)
    };
    let (list_w, repo_w, state_w) = (
        width("LIST", |l| &l.name),
        width("REPO", |l| &l.repo),
        width("STATE", |l| &l.state),
    );
    let mut rows = vec![s.dim(&format!(
        "{:<list_w$}  {:<repo_w$}  {:<state_w$}  NOTE",
        "LIST", "REPO", "STATE"
    ))];
    for l in links {
        let note = table_note(l);
        let cell = if note.is_empty() {
            l.state.clone()
        } else {
            format!("{:<state_w$}", l.state)
        };
        let cell = match l.state.as_str() {
            "ok" => cell,
            "archived" | "renamed" => s.dim(&cell),
            _ => s.yellow(&cell),
        };
        let line = format!("{:<list_w$}  {:<repo_w$}  {cell}  {note}", l.name, l.repo);
        rows.push(line.trim_end().to_string());
    }
    rows
}

/// The daemon's note, and what only this machine knows: whether the
/// window has put the list away, which is the one thing left to do about a
/// repo that is archived, and the owner's to do.
fn table_note(l: &ProjectLink) -> String {
    let note = l.note.clone().unwrap_or_default();
    if l.state != "archived" {
        note
    } else if l.archived_in_window {
        format!("{note}; already archived in the window")
    } else if l.open_tasks == Some(0) {
        format!("{note}, so the list can be archived in the window")
    } else {
        note
    }
}

/// A list that waits to be renamed, and until when.
fn pending_line(l: &ProjectLink, p: &Pending, r: &Report, zone: Tz) -> String {
    // Who renamed what, and when, is in the table's row for the link
    let what = format!("list {:?} waits to be renamed {:?}", l.name, short(&l.repo));
    let begun = if p.begun {
        "; the rename was begun on the server and is not finished"
    } else {
        ""
    };
    match r.settings.follow_rename {
        FollowRename::List => format!(
            "  {what}{begun}; asstd tries on every pass until {}, and waits while the server \
             is out of reach or after {} renames in an hour",
            day(p.expires, zone),
            r.settings.renames_per_hour
        ),
        FollowRename::Off => {
            format!("  {what}{begun}; follow_rename is off, so asstd will leave the list as it is")
        }
    }
}

/// `asst projects log`: newest first, one row per line. What a row did, and
/// why, is its text; the undo is in it where there is one. `repo` is the
/// filter it was asked for, to say what an empty answer is the answer to.
/// Who made a row is a column only when it was not asstd: `asst projects
/// log` is what asstd did, and a column of the same word is no news.
pub fn log_text(s: &Style, rows: &[LogRow], zone: Tz, repo: Option<&str>) -> String {
    if rows.is_empty() {
        return s.dim(&match repo {
            Some(repo) => format!("nothing logged for {repo}"),
            None => "asstd has not changed or decided anything by itself yet".into(),
        });
    }
    let width = |cell: fn(&LogRow) -> &str| rows.iter().map(|r| cell(r).chars().count()).max();
    let kind_w = width(|r| &r.kind).unwrap_or(0);
    let by_w = rows
        .iter()
        .any(|r| r.by != "auto")
        .then(|| width(|r| &r.by).unwrap_or(0));
    rows.iter()
        .map(|r| {
            let when = DateTime::from_timestamp(r.at, 0)
                .map(|t| t.with_timezone(&zone).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "?".into());
            let by = by_w.map_or(String::new(), |w| s.dim(&format!("{:<w$}  ", r.by)));
            format!("{}  {:<kind_w$}  {by}{}", s.dim(&when), r.kind, r.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
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
            "archived on GitHub, first seen 2026-10-18; no open tasks here",
        );
        let links = [link("jaehho/lab", None, ""), archived.clone()];
        assert_eq!(
            link_note(&plain(), &archived).as_deref(),
            Some("  archived on GitHub, first seen 2026-10-18; no open tasks here")
        );
        assert_eq!(
            links_hint(&plain(), &links).as_deref(),
            Some("1 link needs a look: asst projects")
        );
        assert_eq!(
            links_status(&plain(), &links).as_deref(),
            Some("GitHub: 1 link needs a look (archived: jaehho/mail-digest)")
        );

        let replaced = link("jaehho/x", Some(LinkState::Replaced), "another repository");
        let links = [archived, replaced];
        assert_eq!(
            links_hint(&plain(), &links).as_deref(),
            Some("2 links need a look: asst projects")
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

    #[test]
    fn the_listing_of_links_as_the_terminal_shows_it() {
        let mut links: Vec<LinkView> = [
            ("asst", "jaehho/asst", None, ""),
            (
                "battery-log",
                "jaehho/battery-log",
                Some(LinkState::Archived),
                "archived on GitHub, first seen 2026-10-18; no open tasks here",
            ),
            (
                "wallhelper",
                "jaehho/wallhelper",
                Some(LinkState::Unreachable),
                "GitHub cannot find it: deleted, or the token cannot see it",
            ),
            ("wombat", "jaehho/wombat", None, ""),
        ]
        .into_iter()
        .map(|(name, repo, state, note)| LinkView {
            name: name.into(),
            ..link(repo, state, note)
        })
        .collect();
        assert_eq!(
            links_text(&plain(), &links),
            "\
asst ⇄ jaehho/asst
battery-log ⇄ jaehho/battery-log
  archived on GitHub, first seen 2026-10-18; no open tasks here
wallhelper ⇄ jaehho/wallhelper
  GitHub cannot find it: deleted, or the token cannot see it
wombat ⇄ jaehho/wombat
2 links need a look: asst projects"
        );

        // All well: the links and nothing else
        links.retain(|l| l.state.is_none());
        assert_eq!(
            links_text(&plain(), &links),
            "asst ⇄ jaehho/asst\nwombat ⇄ jaehho/wombat"
        );
        assert_eq!(
            links_text(&plain(), &[]),
            "no list is linked; `asst link <list> <owner/repo>`"
        );
    }

    // -- asst projects ---------------------------------------------------

    use crate::projects::{Proposal, Rules, StaleLabel};
    use chrono::TimeZone;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn at(month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, month, day, 16, 0, 0).unwrap()
    }

    fn row(repo: &str, name: &str, state: &str, note: &str) -> ProjectLink {
        ProjectLink {
            repo: repo.into(),
            repo_id: None,
            list: format!("/cal/{name}/"),
            name: name.into(),
            state: state.into(),
            note: Some(note).filter(|n| !n.is_empty()).map(str::to_string),
            since: None,
            open_tasks: Some(0),
            archived_in_window: false,
            pending_rename: None,
        }
    }

    fn report(links: Vec<ProjectLink>) -> Report {
        Report {
            links,
            proposals: Vec::new(),
            labels: Vec::new(),
            settings: Rules::new(FollowRename::List),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn the_links_are_a_table_with_a_note_where_there_is_one() {
        let mut md = row(
            "jaehho/mail-digest",
            "mail-digest",
            "archived",
            "archived on GitHub, first seen 2026-10-18; no open tasks here",
        );
        md.archived_in_window = true;
        let r = report(vec![
            row("jaehho/lab", "lab", "ok", ""),
            md,
            row("jaehho/dash", "dash", "conflict", "two links claim it"),
        ]);
        assert_eq!(
            projects_text(&plain(), &r, ny()),
            "\
LIST         REPO                STATE     NOTE
lab          jaehho/lab          ok
mail-digest  jaehho/mail-digest  archived  archived on GitHub, first seen 2026-10-18; no open tasks here; already archived in the window
dash         jaehho/dash         conflict  two links claim it"
        );

        // Archived in the window says nothing of a repo that is live
        let mut live = row("jaehho/lab", "lab", "ok", "");
        live.archived_in_window = true;
        assert_eq!(
            projects_text(&plain(), &report(vec![live]), ny()),
            "LIST  REPO        STATE  NOTE\nlab   jaehho/lab  ok"
        );

        assert_eq!(
            projects_text(&plain(), &report(vec![]), ny()),
            "no list is linked; `asst link <list> <owner/repo>`"
        );
    }

    #[test]
    fn the_state_is_painted_in_its_own_cell_and_not_the_row() {
        let color = Style { color: true };
        let mut r = report(vec![
            row("jaehho/lab", "lab", "ok", ""),
            row("jaehho/a", "a", "archived", "n"),
            row("jaehho/b", "b", "unreachable", "n"),
        ]);
        r.links[1].open_tasks = Some(2);
        let text = projects_text(&color, &r, ny());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "\x1b[2mLIST  REPO        STATE        NOTE\x1b[0m"
        );
        assert_eq!(lines[1], "lab   jaehho/lab  ok");
        assert_eq!(lines[2], "a     jaehho/a    \x1b[2marchived   \x1b[0m  n");
        assert_eq!(lines[3], "b     jaehho/b    \x1b[33munreachable\x1b[0m  n");
    }

    #[test]
    fn a_list_that_waits_for_its_name_says_until_when() {
        let mut waiting = row("jaehho/lab", "champalimaud", "renamed", "n");
        waiting.pending_rename = Some(Pending {
            from: "jaehho/champalimaud".into(),
            since: at(10, 18),
            expires: at(11, 17),
            begun: false,
        });
        let mut r = report(vec![waiting]);
        let text = projects_text(&plain(), &r, ny());
        assert_eq!(
            text.lines().skip(3).collect::<Vec<_>>().join("\n"),
            "\
Pending
  list \"champalimaud\" waits to be renamed \"lab\"; asstd tries on every pass until 2026-11-17, and waits while the server is out of reach or after 3 renames in an hour"
        );

        // Begun on the server and not finished
        r.links[0].pending_rename.as_mut().unwrap().begun = true;
        let text = projects_text(&plain(), &r, ny());
        assert!(
            text.contains(
                "to be renamed \"lab\"; the rename was begun on the server and is not finished; \
                 asstd tries"
            ),
            "{text}"
        );

        // With following off, asstd will not do it
        r.settings = Rules::new(FollowRename::Off);
        let text = projects_text(&plain(), &r, ny());
        assert!(
            text.ends_with(
                "is not finished; follow_rename is off, so asstd will leave the list as it is"
            ),
            "{text}"
        );
        assert!(!text.contains("until"));
    }

    #[test]
    fn a_proposal_is_the_command_and_why() {
        let mut r = report(vec![row("jaehho/lab", "champalimaud", "ok", "")]);
        r.proposals.push(Proposal {
            kind: "rename-list",
            repo: "jaehho/lab".into(),
            list: "/cal/champalimaud/".into(),
            from: "champalimaud".into(),
            to: "lab".into(),
            command: "asst list rename \"champalimaud\" \"lab\"".into(),
            why: "jaehho/champalimaud is now jaehho/lab; the list is still called \"champalimaud\""
                .into(),
        });
        assert_eq!(
            projects_text(&plain(), &r, ny()),
            "\
LIST          REPO        STATE  NOTE
champalimaud  jaehho/lab  ok

Proposals (nothing has been changed)
  jaehho/champalimaud is now jaehho/lab; the list is still called \"champalimaud\"
    asst list rename \"champalimaud\" \"lab\""
        );
    }

    #[test]
    fn old_labels_say_how_to_stop_and_how_to_catch_up() {
        let mut r = report(vec![row("jaehho/lab", "lab", "ok", "")]);
        r.labels.push(StaleLabel {
            label: "champalimaud".into(),
            now: "lab".into(),
            repo: "jaehho/lab".into(),
            blocks: 127,
            seconds: 152_424,
        });
        r.labels.push(StaleLabel {
            label: "Champalimaud".into(),
            now: "lab".into(),
            repo: "jaehho/lab".into(),
            blocks: 1,
            seconds: 60,
        });
        assert_eq!(
            projects_text(&plain(), &r, ny()),
            "\
LIST  REPO        STATE  NOTE
lab   jaehho/lab  ok

Labels
  127 blocks (42.3 h) still say \"champalimaud\"
    new days: add [[rule]] project = \"champalimaud\" tag = \"lab\" to ~/.config/asst/tags.toml
    history: the relabel recipe in README, \"Renaming a project\"
  1 block (0.0 h) still say \"Champalimaud\"
    new days: add [[rule]] project = \"Champalimaud\" tag = \"lab\" to ~/.config/asst/tags.toml
    history: the relabel recipe in README, \"Renaming a project\""
        );
    }

    #[test]
    fn what_could_not_be_read_is_said_after_the_rest() {
        let mut r = report(vec![row("jaehho/lab", "lab", "ok", "")]);
        r.warnings.push("asstd did not give its log".into());
        assert_eq!(
            projects_text(&plain(), &r, ny()),
            "LIST  REPO        STATE  NOTE\nlab   jaehho/lab  ok\n\nasstd did not give its log"
        );
    }

    #[test]
    fn an_archived_repo_says_whether_the_list_is_put_away_and_can_be() {
        let note = "archived on GitHub, first seen 2026-10-18; no open tasks here";
        let table = |tweak: fn(&mut ProjectLink)| {
            let mut l = row("jaehho/md", "md", "archived", note);
            tweak(&mut l);
            projects_text(&plain(), &report(vec![l]), ny())
                .lines()
                .nth(1)
                .unwrap()
                .to_string()
        };
        // Nothing open and not put away: the one thing left, said as such
        assert_eq!(
            table(|_| {}),
            format!("md    jaehho/md  archived  {note}, so the list can be archived in the window")
        );
        // Put away already
        assert_eq!(
            table(|l| l.archived_in_window = true),
            format!("md    jaehho/md  archived  {note}; already archived in the window")
        );
        // Tasks still open: the daemon's note says what that means, and
        // archiving is not suggested
        assert_eq!(
            table(|l| l.open_tasks = Some(3)),
            format!("md    jaehho/md  archived  {note}")
        );
        // No count (the list is not one asstd knows): no suggestion
        assert_eq!(
            table(|l| l.open_tasks = None),
            format!("md    jaehho/md  archived  {note}")
        );
    }

    #[test]
    fn the_follow_rename_setting_says_what_each_word_does() {
        assert_eq!(
            follow_rename_words(FollowRename::List),
            "list (a renamed repo renames its list)"
        );
        assert_eq!(
            follow_rename_words(FollowRename::Off),
            "off (a rename is only reported)"
        );
    }

    #[test]
    fn the_whole_report_as_the_terminal_shows_it() {
        // What asst projects printed on a copy of the live database, with
        // fake GitHub answering every way it can
        let mut md = row(
            "jaehho/mail-digest",
            "mail-digest",
            "archived",
            "archived on GitHub, first seen 2026-10-18; no open tasks here",
        );
        md.archived_in_window = true;
        let mut steno = row(
            "jaehho/steno-notes",
            "steno",
            "renamed",
            "renamed from jaehho/steno on 2026-10-18; the list is still called \"steno\"",
        );
        steno.pending_rename = Some(Pending {
            from: "jaehho/steno".into(),
            since: at(10, 18),
            expires: at(11, 17),
            begun: false,
        });
        let mut thesis = row(
            "jaehho/thesis",
            "thesis",
            "archived",
            "archived on GitHub, first seen 2026-10-18; 3 tasks are still open here \
             (a list archived in the window still syncs and rings)",
        );
        thesis.open_tasks = Some(3);
        let mut r = report(vec![
            row("jaehho/asst", "asst", "ok", ""),
            row(
                "jaehho/battery-log",
                "battery-log",
                "archived",
                "archived on GitHub, first seen 2026-10-18; no open tasks here",
            ),
            row(
                "jaehho/dash",
                "dash",
                "replaced",
                "jaehho/dash is a different repository now (GitHub id 1379999002, not \
                 1379100002), so its issues are not read. To follow the new one, unlink \
                 jaehho/dash and link it again; the list's open tasks then become issues there",
            ),
            md,
            steno,
            thesis,
            row(
                "jaehho/wallhelper",
                "wallhelper",
                "unreachable",
                "GitHub cannot find it: deleted, or the token cannot see it",
            ),
            row(
                "jaehho/zotero-tray",
                "zotero-tray",
                "conflict",
                "jaehho/zotero-tray is now jaehho/itin, but jaehho/itin is already linked to \
                 \"itin\". Nothing was changed, and this link's issues are not read until one \
                 of the two links is removed (asst unlink OWNER/REPO).",
            ),
        ]);
        r.proposals.push(Proposal {
            kind: "rename-list",
            repo: "jaehho/new-one".into(),
            list: "/cal/old-one/".into(),
            from: "old-one".into(),
            to: "new-one".into(),
            command: "asst list rename \"old-one\" \"new-one\"".into(),
            why: "jaehho/old-one is now jaehho/new-one; the list is still called \"old-one\" \
                  (asstd did not see the rename happen)"
                .into(),
        });
        r.labels.push(StaleLabel {
            label: "champalimaud".into(),
            now: "lab".into(),
            repo: "jaehho/lab".into(),
            blocks: 127,
            seconds: 152_424,
        });
        assert_eq!(
            projects_text(&plain(), &r, ny()),
            r#"LIST         REPO                STATE        NOTE
asst         jaehho/asst         ok
battery-log  jaehho/battery-log  archived     archived on GitHub, first seen 2026-10-18; no open tasks here, so the list can be archived in the window
dash         jaehho/dash         replaced     jaehho/dash is a different repository now (GitHub id 1379999002, not 1379100002), so its issues are not read. To follow the new one, unlink jaehho/dash and link it again; the list's open tasks then become issues there
mail-digest  jaehho/mail-digest  archived     archived on GitHub, first seen 2026-10-18; no open tasks here; already archived in the window
steno        jaehho/steno-notes  renamed      renamed from jaehho/steno on 2026-10-18; the list is still called "steno"
thesis       jaehho/thesis       archived     archived on GitHub, first seen 2026-10-18; 3 tasks are still open here (a list archived in the window still syncs and rings)
wallhelper   jaehho/wallhelper   unreachable  GitHub cannot find it: deleted, or the token cannot see it
zotero-tray  jaehho/zotero-tray  conflict     jaehho/zotero-tray is now jaehho/itin, but jaehho/itin is already linked to "itin". Nothing was changed, and this link's issues are not read until one of the two links is removed (asst unlink OWNER/REPO).

Pending
  list "steno" waits to be renamed "steno-notes"; asstd tries on every pass until 2026-11-17, and waits while the server is out of reach or after 3 renames in an hour

Proposals (nothing has been changed)
  jaehho/old-one is now jaehho/new-one; the list is still called "old-one" (asstd did not see the rename happen)
    asst list rename "old-one" "new-one"

Labels
  127 blocks (42.3 h) still say "champalimaud"
    new days: add [[rule]] project = "champalimaud" tag = "lab" to ~/.config/asst/tags.toml
    history: the relabel recipe in README, "Renaming a project""#
        );
    }

    fn log_row(at: DateTime<Utc>, kind: &str, by: &str, text: &str) -> LogRow {
        LogRow {
            id: 1,
            at: at.timestamp(),
            repo: "jaehho/lab".into(),
            list: None,
            kind: kind.into(),
            text: text.into(),
            data: None,
            by: by.into(),
        }
    }

    #[test]
    fn the_log_is_a_line_a_row_in_the_persons_time() {
        let rows = [
            log_row(
                Utc.with_ymd_and_hms(2026, 10, 18, 1, 4, 0).unwrap(),
                "list-renamed",
                "auto",
                "renamed list \"champalimaud\" to \"lab\": jaehho/champalimaud is now \
                 jaehho/lab (GitHub id 1379104005). Undo: asst list rename lab champalimaud",
            ),
            log_row(
                at(10, 17),
                "repo-archived",
                "cli",
                "jaehho/mail-digest is archived on GitHub",
            ),
        ];
        // 01:04 UTC is the evening before in New York
        assert_eq!(
            log_text(&plain(), &rows, ny(), None),
            "\
2026-10-17 21:04  list-renamed   auto  renamed list \"champalimaud\" to \"lab\": jaehho/champalimaud is now jaehho/lab (GitHub id 1379104005). Undo: asst list rename lab champalimaud
2026-10-17 12:00  repo-archived  cli   jaehho/mail-digest is archived on GitHub"
        );
        let color = Style { color: true };
        assert!(log_text(&color, &rows, ny(), None).starts_with("\x1b[2m2026-10-17 21:04\x1b[0m"));
    }

    #[test]
    fn a_recent_log_is_pointed_to_from_the_report_and_an_old_one_is_not() {
        let now = at(10, 20);
        let row = |when: DateTime<Utc>| log_row(when, "list-renamed", "auto", "x");
        // Newest first, as the daemon gives them
        let rows = [row(at(10, 19)), row(at(10, 18)), row(at(10, 2))];
        assert_eq!(
            log_pointer(&plain(), &rows, now, ny()).as_deref(),
            Some(
                "asstd logged 2 rows in the last 7 days, the latest on 2026-10-19: asst projects log"
            )
        );
        assert_eq!(
            log_pointer(&plain(), &rows[1..2], now, ny()).as_deref(),
            Some(
                "asstd logged 1 row in the last 7 days, the latest on 2026-10-18: asst projects log"
            )
        );
        // Nothing lately, or nothing at all: nothing to say
        assert_eq!(log_pointer(&plain(), &rows[2..], now, ny()), None);
        assert_eq!(log_pointer(&plain(), &[], now, ny()), None);
        let color = Style { color: true };
        assert!(
            log_pointer(&color, &rows, now, ny())
                .unwrap()
                .starts_with("\x1b[2masstd logged")
        );
    }

    #[test]
    fn what_asstd_did_is_not_labelled_as_asstds_on_every_line() {
        // The rows of a run on a private bus: what it did, and what it kept
        let rows = [
            log_row(
                at(10, 18),
                "list-kept",
                "auto",
                "jaehho/work is now jaehho/job; its list was not renamed: the list is called \
                 \"Work stuff\", not \"work\"",
            ),
            log_row(
                at(10, 18),
                "repo-renamed",
                "auto",
                "jaehho/work is now jaehho/job (GitHub id 2001); the link moved with it",
            ),
            log_row(
                at(10, 18),
                "list-renamed",
                "auto",
                "renamed list \"champalimaud\" to \"lab\": jaehho/champalimaud is now jaehho/lab \
                 (GitHub id 1379104005). Undo: asst list rename lab champalimaud",
            ),
        ];
        assert_eq!(
            log_text(&plain(), &rows, ny(), None),
            r#"2026-10-18 12:00  list-kept     jaehho/work is now jaehho/job; its list was not renamed: the list is called "Work stuff", not "work"
2026-10-18 12:00  repo-renamed  jaehho/work is now jaehho/job (GitHub id 2001); the link moved with it
2026-10-18 12:00  list-renamed  renamed list "champalimaud" to "lab": jaehho/champalimaud is now jaehho/lab (GitHub id 1379104005). Undo: asst list rename lab champalimaud"#
        );
    }

    #[test]
    fn an_empty_log_says_which_question_it_answers() {
        assert_eq!(
            log_text(&plain(), &[], ny(), None),
            "asstd has not changed or decided anything by itself yet"
        );
        assert_eq!(
            log_text(&plain(), &[], ny(), Some("jaehho/lab")),
            "nothing logged for jaehho/lab"
        );
    }
}
