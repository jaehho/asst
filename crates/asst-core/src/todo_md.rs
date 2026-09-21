//! A linked project's `TODO.md`: top-level checkbox lines are tasks, each
//! ending in a `<!-- asst:<uid> -->` marker. Headings, prose and nested
//! bullets are never touched. Diffing is stateless and one-way per pass —
//! `file_ops` for what the file asks of the store, `store_edits` for the
//! other way — plus a set of uids that have had a line here, which tells a
//! deleted line (complete the task) from a task that never had one.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A task as the file sync sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ref {
    pub uid: String,
    pub title: String,
    pub open: bool,
}

/// One line of a `TODO.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A top-level checkbox with a marker.
    Task {
        uid: String,
        checked: bool,
        title: String,
    },
    /// A top-level checkbox, no marker yet.
    New { checked: bool, title: String },
    /// A heading, prose, a nested bullet, blank.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The line as it stands, without its newline; kept unless edited.
    pub raw: String,
    pub line: Line,
}

pub fn parse(text: &str) -> Vec<Parsed> {
    text.split('\n')
        .map(|raw| Parsed {
            raw: raw.to_string(),
            line: classify(raw),
        })
        .collect()
}

/// A top-level box and what follows it; `None` for anything else. Indented
/// boxes are nested bullets, which the sync never touches.
fn box_rest(raw: &str) -> Option<(bool, &str)> {
    let (checked, rest) = if let Some(r) = raw.strip_prefix("- [ ]") {
        (false, r)
    } else {
        let r = raw
            .strip_prefix("- [x]")
            .or_else(|| raw.strip_prefix("- [X]"))?;
        (true, r)
    };
    Some((checked, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// The marker at the end of a title: `(title, uid)`. A marker anywhere else
/// is title text.
fn split_marker(rest: &str) -> Option<(&str, &str)> {
    let trimmed = rest.trim_end();
    let with_start = trimmed.strip_suffix("-->")?;
    let at = with_start.rfind("<!-- asst:")?;
    let uid = with_start[at + "<!-- asst:".len()..].trim_end();
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if uid.is_empty() || !uid.chars().all(plain) {
        return None;
    }
    Some((with_start[..at].trim_end(), uid))
}

fn classify(raw: &str) -> Line {
    let Some((checked, rest)) = box_rest(raw) else {
        return Line::Other;
    };
    match split_marker(rest) {
        Some((title, uid)) => Line::Task {
            uid: uid.to_string(),
            checked,
            title: title.trim().to_string(),
        },
        None => Line::New {
            checked,
            title: rest.trim().to_string(),
        },
    }
}

/// What the file asks of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Add {
        line: usize,
        title: String,
        checked: bool,
    },
    Complete {
        uid: String,
    },
    Reopen {
        uid: String,
    },
    Retitle {
        uid: String,
        title: String,
    },
}

/// The changes the file asks for, in line order. `seen` is the uids that
/// have had a line in this file: only they are completed when their line is
/// gone, so a task that arrived from the server is not taken for deleted.
pub fn file_ops(lines: &[Parsed], tasks: &[Ref], seen: &HashSet<String>) -> Vec<FileOp> {
    let mut ops = Vec::new();
    let mut present: HashSet<&str> = HashSet::new();
    for (i, p) in lines.iter().enumerate() {
        match &p.line {
            Line::New { checked, title } if !title.is_empty() => {
                ops.push(FileOp::Add {
                    line: i,
                    title: title.clone(),
                    checked: *checked,
                });
            }
            Line::Task {
                uid,
                checked,
                title,
            } => {
                // A copy-pasted line repeats a uid: the first one is it.
                if !present.insert(uid) {
                    continue;
                }
                let Some(t) = tasks.iter().find(|t| &t.uid == uid) else {
                    continue;
                };
                if *checked && t.open {
                    ops.push(FileOp::Complete { uid: uid.clone() });
                } else if !*checked && !t.open {
                    ops.push(FileOp::Reopen { uid: uid.clone() });
                } else if title != &t.title && !title.is_empty() {
                    ops.push(FileOp::Retitle {
                        uid: uid.clone(),
                        title: title.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    // A removed line completes the task: people delete finished items.
    for t in tasks {
        if t.open && !present.contains(t.uid.as_str()) && seen.contains(&t.uid) {
            ops.push(FileOp::Complete { uid: t.uid.clone() });
        }
    }
    ops
}

/// What the store asks of the file, as line edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Rewrite a checkbox line: adopt, mark, flip the box, retitle.
    Set {
        line: usize,
        uid: String,
        checked: bool,
        title: String,
    },
    Remove {
        line: usize,
    },
    /// Under `## Inbox`, which is made at the end when the file has none.
    Append {
        uid: String,
        title: String,
    },
}

/// The changes the store asks for. A task without a line takes over an
/// unmarked one by its title (linking a file that already lists its work)
/// or is appended under the Inbox — unless it had a line (`seen`), which
/// means it was just deleted here and the file's own pass completes it.
pub fn store_edits(lines: &[Parsed], tasks: &[Ref], seen: &HashSet<String>) -> Vec<Edit> {
    let mut edits = Vec::new();
    // The first marked line of each uid; a repeated one is inert.
    let mut first: HashMap<&str, usize> = HashMap::new();
    for (i, p) in lines.iter().enumerate() {
        if let Line::Task { uid, .. } = &p.line {
            first.entry(uid.as_str()).or_insert(i);
        }
    }
    let mut adopted: HashSet<usize> = HashSet::new();
    for t in tasks {
        if let Some(&i) = first.get(t.uid.as_str()) {
            if let Line::Task { checked, title, .. } = &lines[i].line
                && (*checked != !t.open || *title != t.title)
            {
                edits.push(Edit::Set {
                    line: i,
                    uid: t.uid.clone(),
                    checked: !t.open,
                    title: t.title.clone(),
                });
            }
            continue;
        }
        let draft = (0..lines.len()).find(|&i| {
            matches!(&lines[i].line, Line::New { title, .. } if title == &t.title)
                && adopted.insert(i)
        });
        match draft {
            Some(i) => edits.push(Edit::Set {
                line: i,
                uid: t.uid.clone(),
                checked: !t.open,
                title: t.title.clone(),
            }),
            None if t.open && !seen.contains(&t.uid) => edits.push(Edit::Append {
                uid: t.uid.clone(),
                title: t.title.clone(),
            }),
            None => {}
        }
    }
    // A task gone from the store takes its line with it — a uid that had a
    // line, that is; a foreign marker is left alone.
    for (i, p) in lines.iter().enumerate() {
        if let Line::Task { uid, .. } = &p.line
            && Some(&i) == first.get(uid.as_str())
            && seen.contains(uid)
            && !tasks.iter().any(|t| &t.uid == uid)
        {
            edits.push(Edit::Remove { line: i });
        }
    }
    edits
}

pub fn apply(mut lines: Vec<Parsed>, edits: &[Edit]) -> Vec<Parsed> {
    let mut fixed: Vec<(usize, &Edit)> = edits
        .iter()
        .filter_map(|e| match e {
            Edit::Set { line, .. } | Edit::Remove { line } => Some((*line, e)),
            Edit::Append { .. } => None,
        })
        .collect();
    // From the bottom up, so an edit's line number still holds.
    fixed.sort_unstable_by_key(|(line, _)| *line);
    for (i, e) in fixed.iter().rev() {
        match *e {
            Edit::Set {
                uid,
                checked,
                title,
                ..
            } => {
                let cr = lines[*i].raw.ends_with('\r');
                lines[*i] = task_line(uid, *checked, title);
                if cr {
                    lines[*i].raw.push('\r');
                }
            }
            Edit::Remove { .. } => {
                lines.remove(*i);
            }
            Edit::Append { .. } => unreachable!(),
        }
    }
    let appends: Vec<(&String, &String)> = edits
        .iter()
        .filter_map(|e| match e {
            Edit::Append { uid, title } => Some((uid, title)),
            _ => None,
        })
        .collect();
    insert_inbox(&mut lines, &appends);
    lines
}

fn task_line(uid: &str, checked: bool, title: &str) -> Parsed {
    Parsed {
        raw: format!(
            "- [{}] {title} <!-- asst:{uid} -->",
            if checked { "x" } else { " " }
        ),
        line: Line::Task {
            uid: uid.to_string(),
            checked,
            title: title.to_string(),
        },
    }
}

/// Put `appends` in the `## Inbox` section, after what it holds, before the
/// heading that follows it or at the end; a file without one gains it at
/// the end. The end sits before a trailing empty line, which is the file's
/// final newline.
fn insert_inbox(lines: &mut Vec<Parsed>, appends: &[(&String, &String)]) {
    if appends.is_empty() {
        return;
    }
    let eof = if lines.last().is_some_and(|p| p.raw.is_empty()) {
        lines.len() - 1
    } else {
        lines.len()
    };
    let at = match lines.iter().position(|p| p.raw.trim() == "## Inbox") {
        Some(h) => {
            let mut at = lines[h + 1..]
                .iter()
                .position(|p| p.raw.trim_start().starts_with('#'))
                .map_or(eof, |n| h + 1 + n);
            while at > h + 1 && lines[at - 1].raw.trim().is_empty() {
                at -= 1;
            }
            at
        }
        None => {
            let has_text = lines[..eof].iter().any(|p| !p.raw.trim().is_empty());
            let ends_blank = lines[..eof].last().is_some_and(|p| p.raw.trim().is_empty());
            let mut at = eof;
            if has_text && !ends_blank {
                lines.insert(
                    at,
                    Parsed {
                        raw: String::new(),
                        line: Line::Other,
                    },
                );
                at += 1;
            }
            lines.insert(
                at,
                Parsed {
                    raw: "## Inbox".into(),
                    line: Line::Other,
                },
            );
            at + 1
        }
    };
    for (n, (uid, title)) in appends.iter().enumerate() {
        lines.insert(at + n, task_line(uid, false, title));
    }
}

pub fn render(lines: &[Parsed]) -> String {
    let mut out = String::new();
    for (i, p) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&p.raw);
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Replace the file by writing beside it and renaming over it, so a reader
/// never sees it half-written.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_file_name(".TODO.md.asst.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(uid: &str, title: &str, open: bool) -> Ref {
        Ref {
            uid: uid.into(),
            title: title.into(),
            open,
        }
    }

    fn seen(uids: &[&str]) -> HashSet<String> {
        uids.iter().map(|u| u.to_string()).collect()
    }

    fn marked(uid: &str, checked: bool, title: &str) -> String {
        format!(
            "- [{}] {title} <!-- asst:{uid} -->",
            if checked { "x" } else { " " }
        )
    }

    #[test]
    fn lines_are_classified() {
        let text = "prose\n- [ ] plain\n  - [ ] nested\n- [x] done\n- [X] cap\n\
                    - [ ] <!-- asst:u1 -->\n- [ ] mid <!-- asst:u1 --> line\n- [ ]\n## Heading\n";
        let lines = parse(text);
        let kinds: Vec<&Line> = lines.iter().map(|p| &p.line).collect();
        assert_eq!(
            kinds,
            vec![
                &Line::Other,
                &Line::New {
                    checked: false,
                    title: "plain".into()
                },
                &Line::Other, // indented: nested, never touched
                &Line::New {
                    checked: true,
                    title: "done".into()
                },
                &Line::New {
                    checked: true,
                    title: "cap".into()
                },
                &Line::Task {
                    uid: "u1".into(),
                    checked: false,
                    title: String::new()
                },
                &Line::New {
                    checked: false,
                    title: "mid <!-- asst:u1 --> line".into()
                },
                &Line::New {
                    checked: false,
                    title: String::new()
                },
                &Line::Other,
                &Line::Other, // the final newline's empty line
            ]
        );
    }

    #[test]
    fn uid_must_be_plain() {
        assert!(matches!(
            classify("- [ ] x <!-- asst:a b -->"),
            Line::New { .. }
        ));
        assert!(matches!(
            classify("- [ ] x <!-- asst: -->"),
            Line::New { .. }
        ));
    }

    #[test]
    fn file_asks() {
        let text = format!(
            "- [ ] new\n- [x] done and gone\n{}\n{}\n{}",
            marked("a", true, "kept"),
            marked("b", false, "old title"),
            marked("x", false, "a stranger's uid"),
        );
        let lines = parse(&text);
        let tasks = [
            task("a", "kept", true),
            task("b", "old title", false),
            task("c", "no line", true),
        ];
        assert_eq!(
            file_ops(&lines, &tasks, &seen(&["c"])),
            vec![
                FileOp::Add {
                    line: 0,
                    title: "new".into(),
                    checked: false
                },
                FileOp::Add {
                    line: 1,
                    title: "done and gone".into(),
                    checked: true
                },
                FileOp::Complete { uid: "a".into() },
                FileOp::Reopen { uid: "b".into() },
                // c had a line and doesn't now: complete, never delete
                FileOp::Complete { uid: "c".into() },
            ]
        );
        // c never had a line: it arrived from the server, not deleted here
        assert_eq!(file_ops(&lines, &tasks, &seen(&[])).len(), 4);
        // a copy-pasted repeat of a uid is inert
        let dup = format!("{text}\n{}", marked("a", false, "kept"));
        assert_eq!(file_ops(&parse(&dup), &tasks, &seen(&[])).len(), 4);
    }

    #[test]
    fn file_retitles() {
        let lines = parse(&marked("a", false, "new words"));
        assert_eq!(
            file_ops(&lines, &[task("a", "old words", true)], &seen(&["a"])),
            vec![FileOp::Retitle {
                uid: "a".into(),
                title: "new words".into()
            }]
        );
    }

    #[test]
    fn store_appends_and_adopts() {
        let lines = parse("- [ ] read this\nprose\n");
        let tasks = [
            task("a", "read this", true),
            task("b", "from the phone", true),
        ];
        assert_eq!(
            store_edits(&lines, &tasks, &seen(&[])),
            vec![
                Edit::Set {
                    line: 0,
                    uid: "a".into(),
                    checked: false,
                    title: "read this".into()
                },
                Edit::Append {
                    uid: "b".into(),
                    title: "from the phone".into()
                },
            ]
        );
        // A completed task is adopted too, but never appended
        let done = [task("c", "read this", false)];
        assert_eq!(
            store_edits(&lines, &done, &seen(&[])),
            vec![Edit::Set {
                line: 0,
                uid: "c".into(),
                checked: true,
                title: "read this".into()
            }]
        );
        assert!(store_edits(&parse("prose\n"), &done, &seen(&[])).is_empty());
        // An open task whose line was just deleted: the file's pass completes it
        assert!(
            store_edits(
                &parse("prose\n"),
                &[task("d", "deleted", true)],
                &seen(&["d"])
            )
            .is_empty()
        );
    }

    #[test]
    fn store_fixes_and_removes() {
        let text = format!(
            "{}\n{}",
            marked("a", false, "stale"),
            marked("g", false, "gone")
        );
        let lines = parse(&text);
        let tasks = [task("a", "renamed", false)];
        assert_eq!(
            store_edits(&lines, &tasks, &seen(&["a", "g"])),
            vec![
                Edit::Set {
                    line: 0,
                    uid: "a".into(),
                    checked: true,
                    title: "renamed".into()
                },
                Edit::Remove { line: 1 },
            ]
        );
        // A foreign marker, never seen here, is left alone
        assert_eq!(store_edits(&lines, &tasks, &seen(&["a"])), edits_set_only());
        // Matching lines are left alone
        let ok = format!(
            "{}\n{}",
            marked("a", true, "renamed"),
            marked("g", false, "gone")
        );
        assert_eq!(store_edits(&parse(&ok), &tasks, &seen(&["a"])), vec![]);
    }

    fn edits_set_only() -> Vec<Edit> {
        vec![Edit::Set {
            line: 0,
            uid: "a".into(),
            checked: true,
            title: "renamed".into(),
        }]
    }

    #[test]
    fn applied_edits_keep_the_rest_verbatim() {
        let text = "## Plan\r\n\r\n- [ ] adopted  \r\n- [ ] kept\r\n";
        let edits = vec![Edit::Set {
            line: 2,
            uid: "u".into(),
            checked: true,
            title: "adopted".into(),
        }];
        let out = render(&apply(parse(text), &edits));
        assert_eq!(
            out,
            format!(
                "## Plan\r\n\r\n{}\r\n- [ ] kept\r\n",
                marked("u", true, "adopted")
            )
        );
    }

    #[test]
    fn appends_land_in_the_inbox() {
        // A section already there: after what it holds, before what follows
        let a = Edit::Append {
            uid: "n".into(),
            title: "new".into(),
        };
        let text = "## Plan\n- [ ] p\n\n## Inbox\n- [ ] old\n\n## Later\n";
        assert_eq!(
            render(&apply(parse(text), std::slice::from_ref(&a))),
            format!(
                "## Plan\n- [ ] p\n\n## Inbox\n- [ ] old\n{}\n\n## Later\n",
                marked("n", false, "new")
            )
        );
        // No section: one is made at the end
        assert_eq!(
            render(&apply(parse("- [ ] a\n"), std::slice::from_ref(&a))),
            format!("- [ ] a\n\n## Inbox\n{}\n", marked("n", false, "new"))
        );
        // An empty file starts with the inbox
        assert_eq!(
            render(&apply(parse(""), std::slice::from_ref(&a))),
            format!("## Inbox\n{}\n", marked("n", false, "new"))
        );
        // A file that ends blank doesn't get another blank line
        assert_eq!(
            render(&apply(parse("- [ ] a\n\n"), std::slice::from_ref(&a))),
            format!("- [ ] a\n\n## Inbox\n{}\n", marked("n", false, "new"))
        );
    }

    #[test]
    fn removals_keep_line_numbers() {
        let text = format!("- [ ] a\n{}\n- [ ] b\n", marked("x", false, "gone"));
        assert_eq!(
            render(&apply(parse(&text), &[Edit::Remove { line: 1 }])),
            "- [ ] a\n- [ ] b\n"
        );
    }

    #[test]
    fn a_round_trip_changes_nothing() {
        let text = format!(
            "# T\n\n{}\nprose\n\n## Inbox\n{}\n",
            marked("a", false, "one"),
            marked("b", true, "two")
        );
        let lines = parse(&text);
        let tasks = [task("a", "one", true), task("b", "two", false)];
        let seen = seen(&["a", "b"]);
        assert_eq!(file_ops(&lines, &tasks, &seen), vec![]);
        assert_eq!(store_edits(&lines, &tasks, &seen), vec![]);
        assert_eq!(render(&lines), text);
    }

    /// A folder of its own under the system's temporary directory, gone
    /// when the test ends.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("asst-todo-md-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if self.0.starts_with(std::env::temp_dir()) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn atomic_write_replaces() {
        let dir = Scratch::new("write");
        let path = dir.0.join("TODO.md");
        write_atomic(&path, "one\n").unwrap();
        write_atomic(&path, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\n");
        assert!(!dir.0.join(".TODO.md.asst.tmp").exists());
    }
}
