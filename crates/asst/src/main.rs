//! asst: tasks from the terminal. Everything goes through asstd.

mod bar;
mod out;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use asst_core::api::{
    AddSpec, Added, AsstProxy, Change, LinkView, ListChange, ListSpec, ListView, NewNote, Settings,
    SettingsChange, StatusView, SyncState, TaskView,
};
use asst_core::store::{Query, View};
use asst_core::sync::Report;
use asst_core::task::priority_level;
use asst_core::{fmt, note_files};
use chrono::Utc;
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use out::Style;

#[derive(Parser)]
#[command(
    name = "asst",
    version,
    about = "Tasks on your CalDAV server",
    disable_help_subcommand = true
)]
struct Cli {
    /// Print JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Overdue and due today (the default)
    Today,
    /// Due after today
    Upcoming,
    /// Open tasks, in one list or all
    Ls {
        list: Option<String>,
        /// Only tasks linking this note
        #[arg(long, value_name = "NOTE")]
        attached: Option<PathBuf>,
    },
    /// Completed tasks, newest first
    Completed {
        list: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        /// Delete them all instead, on every device (asks first)
        #[arg(long)]
        clear: bool,
        #[arg(long, requires = "clear")]
        yes: bool,
    },
    /// Open tasks whose title or notes contain the text
    Find {
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Add a task, e.g. `asst add Call the bank #errands tomorrow 5pm p1`
    Add {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(short, long)]
        list: Option<String>,
        /// A date in words: `fri 9am`, `sep 20`
        #[arg(short, long)]
        due: Option<String>,
        /// 1 (high) to 4 (none)
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=4))]
        priority: Option<u8>,
        #[arg(short, long)]
        note: Option<String>,
        /// `every monday`, `daily`, or an RRULE
        #[arg(long)]
        repeat: Option<String>,
        #[arg(long)]
        url: Option<String>,
        /// Where it came from (`kind:id`); adding the same source again is a no-op
        #[arg(long)]
        source: Option<String>,
        /// Link a note, a file in the notes folder (again for more)
        #[arg(long, value_name = "NOTE")]
        attach: Vec<PathBuf>,
        /// Take the text as the title; don't read dates, #list or p1 in it
        #[arg(long)]
        literal: bool,
    },
    /// Link notes (files in the notes folder) to a task, or make a new one for it
    Attach {
        id: String,
        #[arg(required_unless_present = "new")]
        notes: Vec<PathBuf>,
        /// A new note named after the task; prints its path
        #[arg(long, conflicts_with = "notes")]
        new: bool,
        /// The new note's title instead
        #[arg(long, requires = "new")]
        title: Option<String>,
    },
    /// Unlink notes from a task: their paths, or their names
    Detach {
        id: String,
        #[arg(required = true)]
        notes: Vec<String>,
    },
    /// Tie a list to a GitHub repo: its issues are synced both ways;
    /// without arguments, show the ties
    Link {
        list: Option<String>,
        #[arg(requires = "list", value_name = "OWNER/REPO")]
        repo: Option<String>,
    },
    /// Drop the tie to a repo; its issues and the list are left as they stand
    Unlink {
        #[arg(value_name = "OWNER/REPO")]
        repo: String,
    },
    /// Show a task in the window
    Open { id: String },
    /// Show one task
    Show {
        id: String,
        /// The iCalendar object instead
        #[arg(long)]
        raw: bool,
    },
    /// Complete tasks (a repeating task moves to its next date)
    Done {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Mark completed tasks open again
    Reopen {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Change a task
    Edit {
        id: String,
        #[arg(short, long)]
        title: Option<String>,
        /// A date in words, or `none`
        #[arg(short, long)]
        due: Option<String>,
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=4))]
        priority: Option<u8>,
        /// The notes; an empty string clears them
        #[arg(short, long)]
        note: Option<String>,
        /// Move it to another list
        #[arg(short, long)]
        list: Option<String>,
        /// `every monday`, an RRULE, or `none`
        #[arg(long)]
        repeat: Option<String>,
    },
    /// Delete tasks
    Rm {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Copy a task: everything kept, with a new id
    Dup { id: String },
    /// Lists, with their open tasks
    Lists,
    /// Make, rename, recolor or delete a list
    List {
        #[command(subcommand)]
        action: ListCmd,
    },
    /// Show asstd's settings, or change them
    Settings {
        /// The list tasks go to when none is named; `default` for the usual pick
        #[arg(long)]
        inbox: Option<String>,
        /// Seconds between checks for changes on the server (at least 15)
        #[arg(long)]
        interval: Option<u64>,
        /// Minutes for a reminder's Snooze buttons, one to three: `10,30,60`
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        snooze: Option<Vec<u64>>,
        /// Give a task with a due time a reminder (yes/no)
        #[arg(long, value_parser = clap::builder::BoolishValueParser::new())]
        alarm_at_due: Option<bool>,
        /// Minutes before the due time that reminder rings (0: at the due time)
        #[arg(long)]
        alarm_before: Option<u32>,
        /// The folder of notes tasks link to; `default` for ~/Nextcloud/Notes
        #[arg(long, value_name = "FOLDER")]
        notes: Option<String>,
    },
    /// Sync with the server now
    Sync,
    /// Account and sync state
    Status,
    /// Sign in to a Nextcloud server (opens the browser)
    Login {
        server: String,
        /// Use an app password you made yourself, read from stdin
        #[arg(long, requires = "password_stdin")]
        username: Option<String>,
        #[arg(long, requires = "username")]
        password_stdin: bool,
        /// Print the sign-in URL instead of opening it
        #[arg(long)]
        no_browser: bool,
    },
    /// Sign out, and forget the tasks stored here
    Logout,
    /// A waybar module: prints a JSON line whenever tasks change
    Bar,
}

#[derive(Subcommand)]
enum ListCmd {
    /// A new list on the server
    New {
        #[arg(required = true)]
        name: Vec<String>,
        /// `#rrggbb`
        #[arg(long)]
        color: Option<String>,
    },
    /// Rename a list
    Rename {
        list: String,
        #[arg(required = true)]
        name: Vec<String>,
    },
    /// Change a list's color (`#rrggbb`)
    Color { list: String, color: String },
    /// Delete a list and every task in it
    Rm {
        list: String,
        /// Don't ask first
        #[arg(short, long)]
        yes: bool,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("asst: {}", message(&e));
            ExitCode::FAILURE
        }
    }
}

/// D-Bus errors read `org.freedesktop.DBus.Error.Failed: …`; keep the message.
fn message(e: &anyhow::Error) -> String {
    if let Some(zbus::Error::MethodError(name, Some(text), _)) = e.downcast_ref::<zbus::Error>() {
        if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" {
            return "asstd is not running (systemctl --user start asstd)".into();
        }
        return text.clone();
    }
    if let Some(zbus::Error::FDO(fdo)) = e.downcast_ref::<zbus::Error>() {
        return match fdo.as_ref() {
            zbus::fdo::Error::ServiceUnknown(_) => {
                "asstd is not running (systemctl --user start asstd)".into()
            }
            zbus::fdo::Error::Failed(m) | zbus::fdo::Error::InvalidArgs(m) => m.clone(),
            other => other.to_string(),
        };
    }
    format!("{e:#}")
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    if let Some(Cmd::Bar) = cli.command {
        return bar::run().await;
    }
    let conn = zbus::Connection::session()
        .await
        .context("connecting to the session bus")?;
    let proxy = AsstProxy::new(&conn).await?;
    let style = Style::detect();
    let json = cli.json;
    let zone = asst_core::time::local_zone();
    let now = Utc::now().with_timezone(&zone);

    let show_json = |raw: &str| -> anyhow::Result<()> {
        if style.is_terminal() {
            let v: serde_json::Value = serde_json::from_str(raw)?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            println!("{raw}");
        }
        Ok(())
    };

    let list_tasks = |raw: &str, show_list: bool, empty: &str| -> anyhow::Result<()> {
        if json {
            return show_json(raw);
        }
        let tasks: Vec<TaskView> = serde_json::from_str(raw)?;
        if tasks.is_empty() {
            println!("{}", style.dim(empty));
        }
        for t in &tasks {
            println!("{}", out::task_line(&style, t, now, show_list));
        }
        Ok(())
    };

    let q = |view: View, list: Option<String>, text: Option<String>, limit: Option<u32>| {
        serde_json::to_string(&Query {
            view,
            list,
            text,
            limit,
            linked_note: None,
        })
        .expect("query serializes")
    };

    match cli.command.unwrap_or(Cmd::Today) {
        Cmd::Today => list_tasks(
            &proxy.tasks(&q(View::Today, None, None, None)).await?,
            true,
            "Nothing due today.",
        )?,
        Cmd::Upcoming => list_tasks(
            &proxy.tasks(&q(View::Upcoming, None, None, None)).await?,
            true,
            "Nothing scheduled.",
        )?,
        Cmd::Ls { list, attached } => {
            let one = list.is_some();
            let query = Query {
                list,
                linked_note: attached.map(|p| full_path(&p)).transpose()?,
                ..asst_core::api::query(View::Open)
            };
            list_tasks(
                &proxy.tasks(&serde_json::to_string(&query)?).await?,
                !one,
                "No open tasks.",
            )?
        }
        Cmd::Completed {
            list,
            clear: true,
            yes,
            ..
        } => {
            let lists: Vec<ListView> = serde_json::from_str(&proxy.lists().await?)?;
            let target = match &list {
                Some(key) => Some(
                    lists
                        .iter()
                        .find(|l| &l.href == key || l.name.eq_ignore_ascii_case(key.trim()))
                        .ok_or_else(|| anyhow::anyhow!("no list is named {key:?}"))?,
                ),
                None => None,
            };
            let (n, from) = match target {
                Some(l) => (l.done, format!("“{}”", l.name)),
                None => (
                    lists.iter().filter(|l| l.writable).map(|l| l.done).sum(),
                    "every list".to_string(),
                ),
            };
            if n == 0 {
                println!("Nothing completed.");
                return Ok(());
            }
            if !yes
                && !confirm(&format!(
                    "Delete {n} completed task{} from {from}, on every device?",
                    if n == 1 { "" } else { "s" }
                ))?
            {
                println!("Kept.");
                return Ok(());
            }
            let gone = proxy
                .delete_completed(target.map_or("", |l| l.href.as_str()))
                .await?;
            if json {
                println!("{gone}");
            } else {
                println!("{} {gone}", style.dim("Deleted:"));
            }
        }
        Cmd::Completed { list, limit, .. } => {
            let one = list.is_some();
            list_tasks(
                &proxy
                    .tasks(&q(View::Completed, list, None, Some(limit)))
                    .await?,
                !one,
                "Nothing completed.",
            )?
        }
        Cmd::Find { text } => list_tasks(
            &proxy
                .tasks(&q(View::Open, None, Some(text.join(" ")), None))
                .await?,
            true,
            "No matches.",
        )?,

        Cmd::Add {
            text,
            list,
            due,
            priority,
            note,
            repeat,
            url,
            source,
            attach,
            literal,
        } => {
            let spec = AddSpec {
                text: text.join(" "),
                parse: !literal,
                list,
                due_text: due,
                priority,
                description: note,
                repeat_text: repeat,
                url,
                source,
                linked_notes: attach
                    .iter()
                    .map(|p| full_path(p))
                    .collect::<anyhow::Result<_>>()?,
                ..AddSpec::default()
            };
            let raw = proxy.add(&serde_json::to_string(&spec)?).await?;
            if json {
                return show_json(&raw);
            }
            let added: Added = serde_json::from_str(&raw)?;
            let verb = if added.existed {
                "Already there:"
            } else {
                "Added"
            };
            println!(
                "{} {}",
                style.dim(verb),
                out::task_line(&style, &added.task, now, true)
            );
        }

        Cmd::Show { id, raw } => {
            if raw {
                print!("{}", proxy.ics(&id).await?);
                return Ok(());
            }
            let text = proxy.get(&id).await?;
            if json {
                return show_json(&text);
            }
            let t: TaskView = serde_json::from_str(&text)?;
            println!("{}", style.bold(&t.task.summary));
            let row = |k: &str, v: String| println!("  {:<10} {v}", style.dim(k));
            row("id", t.task.uid.clone());
            row("list", t.list_name.clone());
            row("status", t.task.status.as_str().to_string());
            if let Some(d) = &t.task.due {
                row("due", fmt::due_label(d, now));
            }
            if let Some(r) = &t.task.rrule {
                row("repeats", r.clone());
            }
            if t.task.priority > 0 {
                row("priority", format!("p{}", priority_level(t.task.priority)));
            }
            for at in t.task.alarm_instants(zone) {
                row(
                    "reminder",
                    fmt::due_label(&asst_core::time::When::Utc { at }, now),
                );
            }
            if let Some(u) = &t.task.url {
                row("url", u.clone());
            }
            if let Some(s) = &t.task.source {
                row("source", s.clone());
            }
            if !t.task.linked_notes.is_empty() {
                let dir = notes_dir(&proxy).await?;
                for link in &t.task.linked_notes {
                    let file = dir.join(link);
                    let missing = if file.exists() { "" } else { " (missing)" };
                    row("note", format!("{}{}", file.display(), style.dim(missing)));
                }
            }
            if t.pending {
                row("sync", "waiting to be sent".into());
            }
            if let Some(d) = &t.task.description {
                println!();
                for line in d.lines() {
                    println!("  {line}");
                }
            }
        }

        Cmd::Done { ids } => {
            for id in ids {
                let raw = proxy.complete(&id).await?;
                if json {
                    println!("{raw}");
                    continue;
                }
                let t: TaskView = serde_json::from_str(&raw)?;
                match (&t.task.rrule, &t.task.due) {
                    (Some(_), Some(next)) if t.task.is_open() => {
                        println!(
                            "{} {}; next {}",
                            style.dim("Done:"),
                            t.task.summary,
                            fmt::due_label(next, now)
                        )
                    }
                    _ => println!("{} {}", style.dim("Done:"), t.task.summary),
                }
            }
        }
        Cmd::Reopen { ids } => {
            for id in ids {
                let raw = proxy.reopen(&id).await?;
                if json {
                    println!("{raw}");
                    continue;
                }
                let t: TaskView = serde_json::from_str(&raw)?;
                println!("{} {}", style.dim("Reopened:"), t.task.summary);
            }
        }
        Cmd::Edit {
            id,
            title,
            due,
            priority,
            note,
            list,
            repeat,
        } => {
            let change = Change {
                summary: title,
                description: note.map(|n| Some(n).filter(|n| !n.is_empty())),
                due_text: due,
                priority,
                list,
                repeat_text: repeat,
                ..Change::default()
            };
            if change == Change::default() {
                bail!("nothing to change; see `asst edit --help`");
            }
            let raw = proxy.edit(&id, &serde_json::to_string(&change)?).await?;
            if json {
                return show_json(&raw);
            }
            let t: TaskView = serde_json::from_str(&raw)?;
            println!("{}", out::task_line(&style, &t, now, true));
        }
        Cmd::Attach {
            id,
            new: true,
            title,
            ..
        } => {
            let raw = proxy.new_note(&id, title.as_deref().unwrap_or("")).await?;
            if json {
                return show_json(&raw);
            }
            let made: NewNote = serde_json::from_str(&raw)?;
            println!("{}", made.file);
        }
        Cmd::Attach { id, notes, .. } => {
            let t: TaskView = serde_json::from_str(&proxy.get(&id).await?)?;
            let mut links: Vec<String> = t.task.linked_notes.clone();
            for note in &notes {
                links.push(full_path(note)?);
            }
            let change = Change {
                linked_notes: Some(links),
                ..Change::default()
            };
            let raw = proxy
                .edit(&t.href, &serde_json::to_string(&change)?)
                .await?;
            if json {
                return show_json(&raw);
            }
            let t: TaskView = serde_json::from_str(&raw)?;
            println!("{}", out::task_line(&style, &t, now, true));
            print_links(&style, &notes_dir(&proxy).await?, &t);
        }
        Cmd::Detach { id, notes } => {
            let t: TaskView = serde_json::from_str(&proxy.get(&id).await?)?;
            let dir = notes_dir(&proxy).await?;
            let mut links = t.task.linked_notes.clone();
            for given in &notes {
                let path = std::path::absolute(given)
                    .ok()
                    .and_then(|p| note_files::relative(&dir, &p));
                let before = links.len();
                links.retain(|l| {
                    Some(l) != path.as_ref()
                        && l != given
                        && !note_files::name(l).eq_ignore_ascii_case(given.trim())
                });
                if links.len() == before {
                    bail!("“{}” doesn't link a note {given:?}", t.task.summary);
                }
            }
            let change = Change {
                linked_notes: Some(links),
                ..Change::default()
            };
            let raw = proxy
                .edit(&t.href, &serde_json::to_string(&change)?)
                .await?;
            if json {
                return show_json(&raw);
            }
            let t: TaskView = serde_json::from_str(&raw)?;
            println!("{}", out::task_line(&style, &t, now, true));
            print_links(&style, &dir, &t);
        }
        Cmd::Link { list, repo } => match (list, repo) {
            (Some(list), Some(repo)) => {
                let raw = proxy.link(list.trim(), repo.trim()).await?;
                if json {
                    return show_json(&raw);
                }
                let l: LinkView = serde_json::from_str(&raw)?;
                println!("{} {} ⇄ {}", style.dim("Linked:"), l.name, l.repo);
            }
            _ => {
                let raw = proxy.links().await?;
                if json {
                    return show_json(&raw);
                }
                let links: Vec<LinkView> = serde_json::from_str(&raw)?;
                if links.is_empty() {
                    println!(
                        "{}",
                        style.dim("no list is linked; `asst link <list> <owner/repo>`")
                    );
                }
                for l in links {
                    println!("{} ⇄ {}", l.name, l.repo);
                }
            }
        },
        Cmd::Unlink { repo } => {
            if !proxy.unlink(repo.trim()).await? {
                bail!("{repo} isn't linked");
            }
            if !json {
                println!("{} {repo}", style.dim("Unlinked:"));
            }
        }
        Cmd::Open { id } => {
            let t: TaskView = serde_json::from_str(&proxy.get(&id).await?)?;
            // Not `?`: an unknown name here is the window's, not asstd's.
            if let Err(e) = asst_core::api::open_in_window(&conn, &t.href, None).await {
                let why = match &e {
                    zbus::Error::MethodError(name, _, _)
                        if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
                    {
                        "nothing on the bus starts it".to_string()
                    }
                    zbus::Error::MethodError(_, Some(text), _) => text.clone(),
                    other => other.to_string(),
                };
                bail!(
                    "the window didn't open: {why} (is asst-gtk installed, with its D-Bus service file?)"
                );
            }
        }
        Cmd::Rm { ids } => {
            for id in ids {
                let t: TaskView = serde_json::from_str(&proxy.get(&id).await?)?;
                proxy.delete(&id).await?;
                if !json {
                    println!("{} {}", style.dim("Deleted:"), t.task.summary);
                }
            }
        }

        Cmd::Dup { id } => {
            let raw = proxy.duplicate(&id).await?;
            if json {
                return show_json(&raw);
            }
            let t: TaskView = serde_json::from_str(&raw)?;
            println!(
                "{} {}",
                style.dim("Copied:"),
                out::task_line(&style, &t, now, true)
            );
        }

        Cmd::List { action } => match action {
            ListCmd::New { name, color } => {
                let spec = ListSpec {
                    name: name.join(" "),
                    color,
                };
                let raw = proxy.create_list(&serde_json::to_string(&spec)?).await?;
                if json {
                    return show_json(&raw);
                }
                let l: ListView = serde_json::from_str(&raw)?;
                println!("{} {}", style.dim("Created:"), l.name);
            }
            ListCmd::Rename { list, name } => {
                let change = ListChange {
                    name: Some(name.join(" ")),
                    ..ListChange::default()
                };
                let raw = proxy
                    .update_list(&list, &serde_json::to_string(&change)?)
                    .await?;
                if json {
                    return show_json(&raw);
                }
                let l: ListView = serde_json::from_str(&raw)?;
                println!("{} {}", style.dim("Renamed:"), l.name);
            }
            ListCmd::Color { list, color } => {
                let change = ListChange {
                    color: Some(color),
                    ..ListChange::default()
                };
                let raw = proxy
                    .update_list(&list, &serde_json::to_string(&change)?)
                    .await?;
                if json {
                    return show_json(&raw);
                }
                let l: ListView = serde_json::from_str(&raw)?;
                println!(
                    "{} {} is {}",
                    style.dim("Recolored:"),
                    l.name,
                    l.color.unwrap_or_default()
                );
            }
            ListCmd::Rm { list, yes } => {
                let lists: Vec<ListView> = serde_json::from_str(&proxy.lists().await?)?;
                let named: Vec<&ListView> = lists
                    .iter()
                    .filter(|l| l.href == list || l.name.eq_ignore_ascii_case(list.trim()))
                    .collect();
                let target = match named.as_slice() {
                    [one] => *one,
                    [] => bail!("no list is named {list:?} (give its full name)"),
                    _ => bail!(
                        "{} lists are named {list:?}; give its href (see `asst --json lists`)",
                        named.len()
                    ),
                };
                if !yes
                    && !confirm(&format!(
                        "Delete “{}” and all its tasks ({} open)? Nextcloud keeps it in its trash bin.",
                        target.name, target.open
                    ))?
                {
                    println!("Kept.");
                    return Ok(());
                }
                proxy.delete_list(&target.href).await?;
                if !json {
                    println!("{} {}", style.dim("Deleted:"), target.name);
                }
            }
        },

        Cmd::Settings {
            inbox,
            interval,
            snooze,
            alarm_at_due,
            alarm_before,
            notes,
        } => {
            let notes = match notes {
                Some(n) if n.eq_ignore_ascii_case("default") => Some(None),
                Some(n) => Some(Some(full_path(Path::new(&n))?)),
                None => None,
            };
            let change = SettingsChange {
                inbox: inbox.map(|i| Some(i).filter(|i| !i.eq_ignore_ascii_case("default"))),
                interval,
                snooze,
                alarm_at_due,
                alarm_before,
                notes,
            };
            let raw = if change == SettingsChange::default() {
                proxy.settings().await?
            } else {
                proxy.set_settings(&serde_json::to_string(&change)?).await?
            };
            if json {
                return show_json(&raw);
            }
            let s: Settings = serde_json::from_str(&raw)?;
            let lists: Vec<ListView> = serde_json::from_str(&proxy.lists().await?)?;
            let inbox = match (s.inbox, lists.iter().find(|l| l.inbox)) {
                (Some(name), _) => name,
                (None, Some(l)) => format!("{} {}", l.name, style.dim("(default)")),
                (None, None) => style.dim("(default)"),
            };
            let row = |k: &str, v: String| println!("{k:<13} {v}");
            row("inbox", inbox);
            row("interval", format!("{} s", s.interval));
            let snooze: Vec<String> = s.snooze.iter().map(|m| fmt::minutes(*m)).collect();
            row("snooze", snooze.join(", "));
            row(
                "alarm-at-due",
                if s.alarm_at_due { "yes" } else { "no" }.into(),
            );
            row(
                "alarm-before",
                match s.alarm_before {
                    0 => "at the due time".into(),
                    m => format!("{} before", fmt::minutes(u64::from(m))),
                },
            );
            row("notes", s.notes);
        }

        Cmd::Lists => {
            let raw = proxy.lists().await?;
            if json {
                return show_json(&raw);
            }
            let lists: Vec<ListView> = serde_json::from_str(&raw)?;
            for l in lists {
                let mut line = format!("{:>4}  {}", l.open, l.name);
                if l.inbox {
                    line.push_str(&style.dim("  (inbox)"));
                }
                if !l.writable {
                    line.push_str(&style.dim("  (read-only)"));
                }
                println!("{line}");
            }
        }
        Cmd::Sync => {
            let raw = proxy.sync().await?;
            if json {
                return show_json(&raw);
            }
            let r: Report = serde_json::from_str(&raw)?;
            if !r.changed() {
                println!("Up to date.");
            } else {
                let mut parts = Vec::new();
                if r.pushed > 0 {
                    parts.push(format!("{} sent", r.pushed));
                }
                if r.fetched > 0 {
                    parts.push(format!("{} received", r.fetched));
                }
                if r.removed > 0 {
                    parts.push(format!("{} removed", r.removed));
                }
                if parts.is_empty() {
                    parts.push("lists updated".into());
                }
                println!("Synced: {}.", parts.join(", "));
            }
            for p in r.problems {
                eprintln!("{}", style.yellow(&p));
            }
        }
        Cmd::Status => {
            let raw = proxy.status().await?;
            if json {
                return show_json(&raw);
            }
            print_status(&style, &serde_json::from_str(&raw)?);
        }

        Cmd::Login {
            server,
            username,
            password_stdin,
            no_browser,
        } => {
            if let (Some(user), true) = (username, password_stdin) {
                let mut password = String::new();
                std::io::stdin().read_to_string(&mut password)?;
                let raw = proxy
                    .login_password(&server, &user, password.trim())
                    .await?;
                if json {
                    return show_json(&raw);
                }
                print_status(&style, &serde_json::from_str(&raw)?);
                return Ok(());
            }
            // Subscribe first: the answer can come before we'd be listening.
            let mut done = proxy.receive_login_done().await?;
            let url = proxy.login(&server).await?;
            if no_browser {
                println!("Open this to sign in:\n  {url}");
            } else {
                println!(
                    "Opening the sign-in page in your browser. If nothing opens, go to:\n  {url}"
                );
                let _ = std::process::Command::new("xdg-open")
                    .arg(&url)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
            let signal = done
                .next()
                .await
                .ok_or_else(|| anyhow!("asstd went away during sign-in"))?;
            let args = signal.args()?;
            if !args.ok {
                bail!("sign-in failed: {}", args.message);
            }
            let status: StatusView = serde_json::from_str(&proxy.status().await?)?;
            print_status(&style, &status);
        }
        Cmd::Logout => {
            proxy.logout().await?;
            println!(
                "Signed out; the app password is gone from the keyring. Revoke it on the server under Settings → Security."
            );
        }
        Cmd::Bar => unreachable!("handled above"),
    }
    Ok(())
}

/// A path from the command line, made whole against the current folder, as
/// the daemon takes files.
fn full_path(path: &Path) -> anyhow::Result<String> {
    let full = std::path::absolute(path).with_context(|| format!("reading {}", path.display()))?;
    full.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{} isn't valid UTF-8", full.display()))
}

async fn notes_dir(proxy: &AsstProxy<'_>) -> anyhow::Result<PathBuf> {
    let s: Settings = serde_json::from_str(&proxy.settings().await?)?;
    Ok(PathBuf::from(s.notes))
}

fn print_links(style: &Style, dir: &Path, t: &TaskView) {
    for link in &t.task.linked_notes {
        let file = dir.join(link);
        let missing = if file.exists() { "" } else { " (missing)" };
        println!(
            "  {} {}{}",
            style.dim("note"),
            file.display(),
            style.dim(missing)
        );
    }
}

/// Ask on the terminal; anything but yes is no.
fn confirm(question: &str) -> anyhow::Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!("not asking without a terminal; pass --yes");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn print_status(style: &Style, s: &StatusView) {
    match (&s.username, &s.server) {
        (Some(u), Some(server)) => println!("Signed in as {u} on {server}"),
        _ => println!("Not signed in. Run `asst login <server>`."),
    }
    let state = match s.state {
        SyncState::Idle => style.green("up to date"),
        SyncState::Syncing => "syncing".to_string(),
        SyncState::Offline => style.yellow("offline"),
        SyncState::Error => style.red("error"),
        SyncState::NoAccount => style.dim("no account"),
    };
    let last = s
        .last_sync
        .map(|t| format!(", last sync {}", out::ago(t)))
        .unwrap_or_default();
    println!("Sync: {state}{last}");
    if let Some(m) = &s.message {
        println!("  {}", style.dim(m));
    }
    if s.pending > 0 {
        println!(
            "{} change{} waiting to be sent",
            s.pending,
            if s.pending == 1 { "" } else { "s" }
        );
    }
}
