//! Notes a task links to: files in the folder Nextcloud Notes keeps, as the
//! Nextcloud client puts it on this disk (`~/Nextcloud/Notes`, or `notes` in
//! `config.toml`). A task keeps each link as the note's path in that folder,
//! in an `X-ASST-NOTE` property, which the iPhone leaves alone. Notes are
//! never read or changed, except that a new one gets its title.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What counts as a note when listing the folder.
const EXTENSIONS: [&str; 3] = ["md", "txt", "markdown"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Within the notes folder, `/` between folders.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime<Utc>>,
}

/// Where the Nextcloud client keeps the Notes app's folder.
pub fn default_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join("Nextcloud")
        .join("Notes")
}

pub fn is_note(file_name: &str) -> bool {
    file_name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    })
}

/// A note as a person names it: the file name, without a note's extension.
pub fn name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, _)) if is_note(file) => stem,
        _ => file,
    }
}

/// The folder a note is in, within the notes folder: `""` at the top.
pub fn folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// A link as it is stored: `/` between parts, inside the folder.
pub fn clean(path: &str) -> Option<String> {
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    Some(parts.join("/"))
}

/// A file's path within `dir`, when it is in there.
pub fn relative(dir: &Path, file: &Path) -> Option<String> {
    let within = |dir: &Path, file: &Path| {
        let rest = file.strip_prefix(dir).ok()?;
        let parts = rest
            .components()
            .map(|c| match c {
                Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect::<Option<Vec<&str>>>()?;
        (!parts.is_empty()).then(|| parts.join("/"))
    };
    within(dir, file).or_else(|| {
        // Either may reach the folder through a symlink.
        within(
            &std::fs::canonicalize(dir).ok()?,
            &std::fs::canonicalize(file).ok()?,
        )
    })
}

/// The notes in `dir` and the folders in it, newest first. Hidden files and
/// folders (the Nextcloud client's own) are left out.
pub fn list(dir: &Path) -> Vec<Note> {
    let mut notes = Vec::new();
    walk(dir, "", 0, &mut notes);
    notes.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    notes
}

fn walk(dir: &Path, prefix: &str, depth: usize, notes: &mut Vec<Note>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = match prefix {
            "" => name.clone(),
            _ => format!("{prefix}/{name}"),
        };
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            if depth < 8 {
                walk(&entry.path(), &path, depth + 1, notes);
            }
        } else if is_note(&name) {
            notes.push(Note {
                path,
                modified: meta.modified().ok().map(DateTime::<Utc>::from),
            });
        }
    }
}

/// A file name for a note with this title, without the characters Nextcloud
/// or another system turns away.
pub fn file_name(title: &str) -> String {
    let spaced: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = words
        .trim_start_matches('.')
        .trim_start()
        .chars()
        .take(80)
        .collect();
    let short = short.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    if short.is_empty() {
        "Untitled".into()
    } else {
        short.into()
    }
}

/// Make a note titled `title` in `dir`: `Title.md`, or `Title (2).md` when
/// that is taken. It starts with the title as a heading, since Nextcloud
/// Notes reads a note's first line as its title. Returns its path in `dir`.
pub fn create(dir: &Path, title: &str) -> std::io::Result<String> {
    let base = file_name(title);
    let heading = match title.split_whitespace().collect::<Vec<_>>().join(" ") {
        t if t.is_empty() => base.clone(),
        t => t,
    };
    for n in 1..1000 {
        let name = match n {
            1 => format!("{base}.md"),
            n => format!("{base} ({n}).md"),
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))
        {
            Ok(mut file) => {
                file.write_all(format!("# {heading}\n\n").as_bytes())?;
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("too many notes named {base:?}"),
    ))
}

/// A task's links after notes, or folders of them, moved (`from`, `to`, in
/// the order they happened); none when no link changes. A link follows its
/// note through every move, unless something is at its old path again: an
/// editor that saves by renaming the note away and writing a new one.
pub fn follow(
    links: &[String],
    moves: &[(String, String)],
    exists: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(links.len());
    for link in links {
        let mut at = link.clone();
        for (from, to) in moves {
            if at == *from {
                at = to.clone();
            } else if let Some(rest) = at
                .strip_prefix(from.as_str())
                .and_then(|r| r.strip_prefix('/'))
            {
                at = format!("{to}/{rest}");
            }
        }
        if at != *link && exists(link) {
            at = link.clone();
        }
        // Moved onto a note that was linked too: one link.
        if !out.contains(&at) {
            out.push(at);
        }
    }
    (out != links).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own under the system's temporary directory, gone
    /// when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("asst-note-files-{}-{name}", std::process::id()));
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
    fn names_and_folders() {
        assert_eq!(name("Recipes/Pasta.md"), "Pasta");
        assert_eq!(name("Trip.plan.txt"), "Trip.plan");
        assert_eq!(name("scan.pdf"), "scan.pdf");
        assert_eq!(name(".md"), ".md");
        assert_eq!(folder("Recipes/Italian/Pasta.md"), "Recipes/Italian");
        assert_eq!(folder("Pasta.md"), "");
        assert_eq!(
            clean("/Recipes//./Pasta.md").as_deref(),
            Some("Recipes/Pasta.md")
        );
        assert_eq!(clean("../secret.md"), None);
        assert_eq!(clean(""), None);
    }

    #[test]
    fn files_inside_the_folder_only() {
        let dir = Path::new("/home/me/Nextcloud/Notes");
        let rel = |f: &str| relative(dir, Path::new(f));
        assert_eq!(
            rel("/home/me/Nextcloud/Notes/Pasta.md").as_deref(),
            Some("Pasta.md")
        );
        assert_eq!(
            rel("/home/me/Nextcloud/Notes/a/b.md").as_deref(),
            Some("a/b.md")
        );
        assert_eq!(rel("/home/me/Nextcloud/Notes/../x.md"), None);
        assert_eq!(rel("/home/me/Nextcloud/Notes"), None);
        assert_eq!(rel("/home/me/Documents/x.md"), None);
    }

    #[test]
    fn file_names_for_titles() {
        assert_eq!(file_name("Plan: trip / visas?"), "Plan trip visas");
        assert_eq!(file_name("  ...hidden "), "hidden");
        assert_eq!(file_name("Ends with dots..."), "Ends with dots");
        assert_eq!(file_name("\t/"), "Untitled");
        assert_eq!(file_name(&"x".repeat(200)).len(), 80);
    }

    #[test]
    fn a_new_note_takes_a_free_name() {
        let s = Scratch::new("create");
        assert_eq!(create(&s.0, "Plan: trip").unwrap(), "Plan trip.md");
        assert_eq!(create(&s.0, "Plan: trip").unwrap(), "Plan trip (2).md");
        assert_eq!(
            std::fs::read_to_string(s.0.join("Plan trip.md")).unwrap(),
            "# Plan: trip\n\n"
        );
    }

    #[test]
    fn listing_skips_hidden_and_other_files() {
        let s = Scratch::new("list");
        std::fs::create_dir_all(s.0.join("Recipes")).unwrap();
        std::fs::create_dir_all(s.0.join(".trash")).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (path, modified) in [
            ("Old.md", Some(old)),
            ("Recipes/Pasta.txt", None),
            (".hidden.md", None),
            (".trash/Gone.md", None),
            ("scan.pdf", None),
        ] {
            let file = std::fs::File::create(s.0.join(path)).unwrap();
            if let Some(t) = modified {
                file.set_modified(t).unwrap();
            }
        }
        let paths: Vec<String> = list(&s.0).into_iter().map(|n| n.path).collect();
        assert_eq!(paths, ["Recipes/Pasta.txt", "Old.md"]);
        assert!(list(&s.0.join("missing")).is_empty());
    }

    #[test]
    fn links_follow_moves() {
        let links = vec!["Trip.md".to_string(), "Recipes/Pasta.md".to_string()];
        let moves = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let gone = |_: &str| false;
        assert_eq!(
            follow(&links, &moves(&[("Trip.md", "Travel/Japan.md")]), gone),
            Some(vec!["Travel/Japan.md".into(), "Recipes/Pasta.md".into()])
        );
        assert_eq!(
            follow(&links, &moves(&[("Recipes", "Food")]), gone),
            Some(vec!["Trip.md".into(), "Food/Pasta.md".into()])
        );
        assert_eq!(
            follow(
                &links,
                &moves(&[("Recipes/Pasta", "x"), ("Rec", "x")]),
                gone
            ),
            None
        );
        assert_eq!(
            follow(&links, &moves(&[("Trip.md", "Recipes/Pasta.md")]), gone),
            Some(vec!["Recipes/Pasta.md".into()])
        );
        // One after another, and a folder after a note in it.
        assert_eq!(
            follow(
                &links,
                &moves(&[
                    ("Trip.md", "Japan.md"),
                    ("Japan.md", "Trips/Japan.md"),
                    ("Trips", "Travel")
                ]),
                gone
            ),
            Some(vec!["Travel/Japan.md".into(), "Recipes/Pasta.md".into()])
        );
        // Saved by renaming it to a backup and writing it anew.
        let swapped = moves(&[("Trip.md", "Trip.md~")]);
        assert_eq!(follow(&links, &swapped, |p| p == "Trip.md"), None);
    }
}
