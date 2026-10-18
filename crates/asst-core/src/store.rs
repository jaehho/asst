//! The SQLite cache: lists and tasks as last seen on the server, plus the
//! edits not yet sent. Every task keeps three things: `base` (the server's
//! copy), `edits` (what asst changed since), and `ics` (the two combined,
//! which is what gets shown and sent).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rusqlite::{
    Connection, OptionalExtension, Row as SqlRow, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};

use crate::caldav::RemoteList;
use crate::github::Pair;
use crate::ical::Ical;
use crate::task::{self, Edit, EditError, Task};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("no task matches {0:?}")]
    NotFound(String),
    #[error("{0:?} matches {1} tasks; use more of the id")]
    Ambiguous(String, usize),
    #[error("no list matches {0:?}")]
    NoList(String),
    #[error("list {0:?} is read-only")]
    ReadOnly(String),
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
    #[error("backing up the database before a migration: {0}")]
    Backup(String),
}

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRow {
    pub href: String,
    pub name: String,
    pub color: Option<String>,
    pub order: Option<i64>,
    pub writable: bool,
    pub sync_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub href: String,
    pub list: String,
    pub etag: Option<String>,
    /// Not yet on the server as shown.
    pub pending: bool,
    pub task: Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Create,
    Update,
    Delete,
}

/// One task that has to go to the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub href: String,
    pub list: String,
    pub etag: Option<String>,
    pub op: Op,
    pub body: String,
    /// How many of the stored edits `body` includes.
    pub sent_edits: usize,
}

/// A time block: actual tracked time (`planned = 0`) or a planned slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uid: Option<String>,
    /// Epoch milliseconds.
    pub start: i64,
    /// Epoch milliseconds.
    pub end: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub planned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    /// Overdue and due today.
    Today,
    /// Due after today.
    Upcoming,
    /// Everything open.
    Open,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub view: View,
    #[serde(default)]
    pub list: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// A linked repo, with what is known about it beyond the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhLinkRow {
    pub repo: String,
    pub list: String,
    /// The repo's GitHub id, learned once; `None` until the first look.
    pub repo_id: Option<u64>,
    /// `None`: never looked at. `Some(0)`: looked at, not archived or
    /// disabled. Otherwise when (epoch seconds) it was first seen so.
    pub archived: Option<i64>,
    /// The name the repo had before the rename that the list has yet to
    /// follow, and when (epoch seconds) that rename was seen.
    pub follow_from: Option<String>,
    pub follow_at: Option<i64>,
}

/// A row of the decision log: what asst changed or decided about a link
/// by itself, and why. `by` is `auto` or `cli`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRow {
    pub id: i64,
    pub at: i64,
    pub repo: String,
    pub list: Option<String>,
    pub kind: String,
    pub text: String,
    pub data: Option<String>,
    pub by: String,
}

/// A decision log row to write.
#[derive(Debug, Clone, Copy)]
pub struct NewLog<'a> {
    pub at: DateTime<Utc>,
    pub repo: &'a str,
    pub list: Option<&'a str>,
    pub kind: &'a str,
    pub text: &'a str,
    /// JSON for whatever an undo needs.
    pub data: Option<&'a str>,
    pub by: &'a str,
}

/// How many actual blocks carry a label, and how long they last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelTotal {
    pub label: String,
    pub blocks: usize,
    pub seconds: i64,
}

/// The decision log keeps this many rows, the newest.
pub const GH_LOG_KEEP: i64 = 500;

pub struct Store {
    conn: Connection,
    zone: Tz,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS lists (
  href TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  color TEXT,
  sort INTEGER,
  writable INTEGER NOT NULL DEFAULT 1,
  sync_token TEXT
);
CREATE TABLE IF NOT EXISTS tasks (
  href TEXT PRIMARY KEY,
  list TEXT NOT NULL REFERENCES lists(href) ON DELETE CASCADE,
  uid TEXT NOT NULL,
  etag TEXT,
  base TEXT NOT NULL,
  edits TEXT NOT NULL DEFAULT '[]',
  ics TEXT NOT NULL,
  deleted INTEGER NOT NULL DEFAULT 0,
  summary TEXT NOT NULL,
  description TEXT,
  status TEXT NOT NULL,
  priority INTEGER NOT NULL,
  due_at INTEGER,
  due_date TEXT,
  due_timed INTEGER,
  completed_at INTEGER,
  source TEXT,
  sort_order INTEGER,
  created_at INTEGER,
  json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_uid ON tasks(uid);
CREATE INDEX IF NOT EXISTS tasks_list ON tasks(list);
CREATE INDEX IF NOT EXISTS tasks_source ON tasks(source);
CREATE TABLE IF NOT EXISTS fired (href TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (href, at));
CREATE TABLE IF NOT EXISTS snoozed (href TEXT PRIMARY KEY, until INTEGER NOT NULL);
DROP TABLE IF EXISTS todo_seen;
DROP TABLE IF EXISTS links;
CREATE TABLE IF NOT EXISTS gh_links (
  repo TEXT PRIMARY KEY,
  list TEXT NOT NULL UNIQUE,
  repo_id INTEGER,
  archived INTEGER,
  follow_from TEXT,
  follow_at INTEGER
);
CREATE TABLE IF NOT EXISTS gh_log (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  repo TEXT NOT NULL,
  list TEXT,
  kind TEXT NOT NULL,
  text TEXT NOT NULL,
  data TEXT,
  by TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gh_pairs (
  repo TEXT NOT NULL REFERENCES gh_links(repo) ON DELETE CASCADE,
  number INTEGER NOT NULL,
  uid TEXT NOT NULL,
  title TEXT NOT NULL,
  open INTEGER NOT NULL,
  body TEXT,
  priority INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (repo, number)
);
CREATE TABLE IF NOT EXISTS blocks (
  id INTEGER PRIMARY KEY,
  task_uid TEXT,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  label TEXT,
  planned INTEGER NOT NULL DEFAULT 0,
  source TEXT UNIQUE,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS blocks_task ON blocks(task_uid);
CREATE INDEX IF NOT EXISTS blocks_start ON blocks(start);
";

/// The columns a link gained for the repo's identity, in the order they
/// were added.
const IDENTITY_COLUMNS: [(&str, &str); 4] = [
    ("repo_id", "INTEGER"),
    ("archived", "INTEGER"),
    ("follow_from", "TEXT"),
    ("follow_at", "INTEGER"),
];

/// Whether `table` has `column`.
fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// A link made before repos had an identity has no `repo_id` and its
/// neighbours: CREATE TABLE IF NOT EXISTS leaves the old table as it is.
/// Every column starts out NULL, which means "not looked at yet"; nothing
/// is read from GitHub or Nextcloud and nothing is logged. The check runs
/// again inside the write lock, since asst, asstd and asst-gtk rebuild and
/// start independently and any of them may be the first to migrate.
fn add_identity_columns(conn: &Connection) -> Result<()> {
    if has_column(conn, "gh_links", "repo_id")? {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for (name, kind) in IDENTITY_COLUMNS {
        if !has_column(&tx, "gh_links", name)? {
            tx.execute_batch(&format!("ALTER TABLE gh_links ADD COLUMN {name} {kind}"))?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Where the copy of a database made before the identity columns goes.
fn identity_backup_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".pre-identity");
    PathBuf::from(name)
}

/// Before the first migration to the identity columns, keep a copy of the
/// database beside it as `<db>.pre-identity`: links and their pairs cannot
/// be rebuilt from either server. Taken once, when a `gh_links` table
/// exists without `repo_id` and the copy does not; a new database has
/// nothing to lose. asst never deletes it.
///
/// `VACUUM INTO` writes a consistent snapshot even while another process
/// has the file open in WAL mode. It goes to a name of this process's own
/// and is checked before it is linked into place, so a crash or a full disk
/// never leaves a half copy that later opens would take for the real one,
/// and two processes that start together keep whichever finished first.
/// A copy that cannot be made stops the open: the migration waits for it.
fn back_up_before_identity(conn: &Connection, db: &Path) -> Result<()> {
    let backup = identity_backup_path(db);
    let old_table =
        has_column(conn, "gh_links", "repo")? && !has_column(conn, "gh_links", "repo_id")?;
    if !old_table || backup.exists() {
        return Ok(());
    }
    let mut tmp = backup.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    let copied = (|| -> Result<()> {
        conn.execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])?;
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let check: String =
            Connection::open_with_flags(&tmp, flags)?
                .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(StoreError::Backup(format!("the copy is damaged: {check}")));
        }
        // A hard link fails when the name is taken, as a rename does not.
        match std::fs::hard_link(&tmp, &backup) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(_) if !backup.exists() => std::fs::rename(&tmp, &backup)
                .map_err(|e| StoreError::Backup(format!("{}: {e}", backup.display()))),
            Err(e) => Err(StoreError::Backup(format!("{}: {e}", backup.display()))),
        }
    })();
    let _ = std::fs::remove_file(&tmp);
    copied
}

const TASK_COLS: &str = "href, list, etag, (etag IS NULL OR edits != '[]' OR deleted = 1), json";

fn row_from_sql(r: &SqlRow) -> rusqlite::Result<(String, String, Option<String>, bool, String)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn to_row(t: (String, String, Option<String>, bool, String)) -> Result<Row> {
    let task = serde_json::from_str(&t.4).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(Row {
        href: t.0,
        list: t.1,
        etag: t.2,
        pending: t.3,
        task,
    })
}

impl Store {
    pub fn open(path: &Path, zone: Tz) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        back_up_before_identity(&conn, path)?;
        Store::init(conn, zone)
    }

    pub fn in_memory(zone: Tz) -> Result<Store> {
        Store::init(Connection::open_in_memory()?, zone)
    }

    fn init(conn: Connection, zone: Tz) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        // A store made before the GitHub merge carried body and priority:
        // CREATE TABLE IF NOT EXISTS leaves the old columns as they are.
        let has_body: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('gh_pairs') WHERE name = 'body'",
            [],
            |r| r.get(0),
        )?;
        if has_body == 0 {
            conn.execute_batch(
                "ALTER TABLE gh_pairs ADD COLUMN body TEXT;
                 ALTER TABLE gh_pairs ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        add_identity_columns(&conn)?;
        let mut store = Store { conn, zone };
        let stored: Option<String> = store.meta("zone")?;
        if stored.as_deref() != Some(zone.name()) {
            // Due dates are stored as local days; a new zone moves them.
            store.reindex()?;
            store.set_meta("zone", zone.name())?;
        }
        Ok(store)
    }

    pub fn zone(&self) -> Tz {
        self.zone
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    fn reindex(&mut self) -> Result<()> {
        let rows: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare("SELECT href, ics FROM tasks")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let tx = self.conn.transaction()?;
        for (href, ics) in rows {
            if let Some(t) = Task::from_ical(&Ical::parse(&ics)) {
                write_derived(&tx, &href, &t, self.zone)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // -- lists -------------------------------------------------------------

    pub fn lists(&self) -> Result<Vec<ListRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT href, name, color, sort, writable, sync_token FROM lists ORDER BY sort IS NULL, sort, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ListRow {
                href: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                order: r.get(3)?,
                writable: r.get(4)?,
                sync_token: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A list by href, exact name, or unambiguous name prefix (any case).
    pub fn find_list(&self, key: &str) -> Result<ListRow> {
        let lists = self.lists()?;
        if let Some(l) = lists
            .iter()
            .find(|l| l.href == key || l.name.eq_ignore_ascii_case(key))
        {
            return Ok(l.clone());
        }
        let lower = key.to_lowercase();
        let matches: Vec<&ListRow> = lists
            .iter()
            .filter(|l| l.name.to_lowercase().starts_with(&lower))
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            _ => Err(StoreError::NoList(key.to_string())),
        }
    }

    /// Take the server's list of lists. Returns the lists whose contents
    /// changed since the last pull (their token moved), and whether any list
    /// itself appeared, went, or was renamed.
    pub fn apply_lists(&mut self, remote: &[RemoteList]) -> Result<(Vec<ListRow>, bool)> {
        let known: HashMap<String, ListRow> = self
            .lists()?
            .into_iter()
            .map(|l| (l.href.clone(), l))
            .collect();
        let tx = self.conn.transaction()?;
        let mut changed_meta = false;
        for l in remote {
            match known.get(&l.href) {
                Some(k)
                    if k.name == l.name
                        && k.color == l.color
                        && k.order == l.order
                        && k.writable == l.writable => {}
                _ => changed_meta = true,
            }
            tx.execute(
                "INSERT INTO lists(href, name, color, sort, writable) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(href) DO UPDATE SET name = excluded.name, color = excluded.color,
                   sort = excluded.sort, writable = excluded.writable",
                params![l.href, l.name, l.color, l.order, l.writable],
            )?;
        }
        for href in known.keys() {
            if !remote.iter().any(|l| &l.href == href) {
                remove_list_rows(&tx, href)?;
                changed_meta = true;
            }
        }
        tx.commit()?;
        let stale = self
            .lists()?
            .into_iter()
            .filter(|l| {
                let server = remote
                    .iter()
                    .find(|r| r.href == l.href)
                    .and_then(|r| r.sync_token.as_ref());
                l.sync_token.is_none() || server.is_none() || server != l.sync_token.as_ref()
            })
            .collect();
        Ok((stale, changed_meta))
    }

    /// Forget a list that is gone from the server, with its tasks and any
    /// edits to them still waiting to be sent. Returns whether it was known.
    pub fn remove_list(&mut self, href: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let known = remove_list_rows(&tx, href)?;
        tx.commit()?;
        Ok(known)
    }

    pub fn set_token(&self, list: &str, token: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE lists SET sync_token = ?2 WHERE href = ?1",
            params![list, token],
        )?;
        Ok(())
    }

    // -- sync --------------------------------------------------------------

    /// href → etag for tasks the server has.
    pub fn etags(&self, list: &str) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT href, etag FROM tasks WHERE list = ?1 AND etag IS NOT NULL")?;
        let rows = stmt.query_map([list], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Store the server's copy of a resource. Unsent edits are re-applied on
    /// top of it, unless the server copy already shows them (a write whose
    /// response was lost). Returns false when the resource is not a task.
    pub fn put_server(
        &mut self,
        list: &str,
        href: &str,
        etag: &str,
        data: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let ical = Ical::parse(data);
        let Some(server_task) = Task::from_ical(&ical) else {
            self.forget(href)?;
            return Ok(false);
        };
        let existing: Option<(String, String, bool)> = self
            .conn
            .query_row(
                "SELECT base, edits, deleted FROM tasks WHERE href = ?1",
                [href],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (edits, ics, task) = match existing {
            Some((base, edits_json, _)) if edits_json != "[]" => {
                let edits: Vec<Edit> = serde_json::from_str(&edits_json)
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?;
                let mut expected = Ical::parse(&base);
                let applied = task::apply(&mut expected, &edits, now, self.zone)
                    .ok()
                    .and_then(|()| Task::from_ical(&expected))
                    .is_some_and(|exp| already_applied(&exp, &server_task, &edits));
                if applied {
                    (Vec::new(), data.to_string(), server_task)
                } else {
                    let mut rebased = ical.clone();
                    match task::apply(&mut rebased, &edits, now, self.zone) {
                        Ok(()) => {
                            let t = Task::from_ical(&rebased).unwrap_or(server_task);
                            (edits, rebased.to_string(), t)
                        }
                        // The edits no longer make sense on this copy: the server wins.
                        Err(_) => (Vec::new(), data.to_string(), server_task),
                    }
                }
            }
            _ => (Vec::new(), data.to_string(), server_task),
        };
        let edits_json = serde_json::to_string(&edits).expect("edits serialize");
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO tasks(href, list, uid, etag, base, edits, ics, summary, status, priority, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', '', 0, '{}')
             ON CONFLICT(href) DO UPDATE SET list = excluded.list, uid = excluded.uid, etag = excluded.etag,
               base = excluded.base, edits = excluded.edits, ics = excluded.ics",
            params![href, list, task.uid, etag, data, edits_json, ics],
        )?;
        write_derived(&tx, href, &task, self.zone)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn forget(&self, href: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tasks WHERE href = ?1", [href])?;
        self.conn
            .execute("DELETE FROM fired WHERE href = ?1", [href])?;
        self.conn
            .execute("DELETE FROM snoozed WHERE href = ?1", [href])?;
        Ok(())
    }

    pub fn pending(&self) -> Result<Vec<Pending>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT href, list, etag, ics, edits, deleted FROM tasks
             WHERE etag IS NULL OR edits != '[]' OR deleted = 1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (href, list, etag, ics, edits, deleted) = row?;
            let sent_edits = serde_json::from_str::<Vec<Edit>>(&edits)
                .map(|e| e.len())
                .unwrap_or(0);
            let op = if deleted {
                Op::Delete
            } else if etag.is_none() {
                Op::Create
            } else {
                Op::Update
            };
            out.push(Pending {
                href,
                list,
                etag,
                op,
                body: ics,
                sent_edits,
            });
        }
        Ok(out)
    }

    /// A create or update went through: `server` is now the base, and any
    /// edits made while it was in flight stay queued on top of it.
    pub fn pushed(
        &mut self,
        p: &Pending,
        etag: &str,
        server: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let edits_json: Option<String> = self
            .conn
            .query_row("SELECT edits FROM tasks WHERE href = ?1", [&p.href], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(edits_json) = edits_json else {
            return Ok(());
        };
        let edits: Vec<Edit> =
            serde_json::from_str(&edits_json).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let remaining: Vec<Edit> = edits.into_iter().skip(p.sent_edits).collect();
        let mut ical = Ical::parse(server);
        task::apply(&mut ical, &remaining, now, self.zone)?;
        let task = Task::from_ical(&ical)
            .ok_or_else(|| StoreError::Corrupt(format!("{} is not a task", p.href)))?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE tasks SET etag = ?2, base = ?3, edits = ?4, ics = ?5 WHERE href = ?1",
            params![
                p.href,
                etag,
                server,
                serde_json::to_string(&remaining).expect("serialize"),
                ical.to_string()
            ],
        )?;
        write_derived(&tx, &p.href, &task, self.zone)?;
        tx.commit()?;
        Ok(())
    }

    // -- local changes -------------------------------------------------------

    pub fn create(&mut self, list: &str, edits: &[Edit], now: DateTime<Utc>) -> Result<Row> {
        let l = self.find_list(list)?;
        if !l.writable {
            return Err(StoreError::ReadOnly(l.name));
        }
        let uid = uuid::Uuid::new_v4().to_string();
        let href = format!("{}{}.ics", l.href, uid);
        let base = task::new_ical(&uid, now);
        let mut ical = base.clone();
        task::apply(&mut ical, edits, now, self.zone)?;
        let task = Task::from_ical(&ical).expect("a new task parses");
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO tasks(href, list, uid, etag, base, edits, ics, summary, status, priority, json)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, '', '', 0, '{}')",
            params![href, l.href, uid, base.to_string(), serde_json::to_string(edits).expect("serialize"), ical.to_string()],
        )?;
        write_derived(&tx, &href, &task, self.zone)?;
        tx.commit()?;
        self.get(&href)?.ok_or(StoreError::NotFound(href))
    }

    pub fn edit(&mut self, href: &str, edits: &[Edit], now: DateTime<Utc>) -> Result<Row> {
        let (list, ics, stored): (String, String, String) = self
            .conn
            .query_row(
                "SELECT list, ics, edits FROM tasks WHERE href = ?1 AND deleted = 0",
                [href],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(href.to_string()))?;
        if let Some(l) = self.lists()?.into_iter().find(|l| l.href == list)
            && !l.writable
        {
            return Err(StoreError::ReadOnly(l.name));
        }
        let mut ical = Ical::parse(&ics);
        task::apply(&mut ical, edits, now, self.zone)?;
        let task = Task::from_ical(&ical)
            .ok_or_else(|| StoreError::Corrupt(format!("{href} is not a task")))?;
        let mut all: Vec<Edit> =
            serde_json::from_str(&stored).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        all.extend_from_slice(edits);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE tasks SET edits = ?2, ics = ?3 WHERE href = ?1",
            params![
                href,
                serde_json::to_string(&all).expect("serialize"),
                ical.to_string()
            ],
        )?;
        write_derived(&tx, href, &task, self.zone)?;
        tx.commit()?;
        self.get(href)?
            .ok_or_else(|| StoreError::NotFound(href.to_string()))
    }

    /// Complete a task. A repeating one moves on to its next occurrence and,
    /// as iOS does, leaves a completed copy of this one behind.
    pub fn complete(&mut self, href: &str, at: DateTime<Utc>) -> Result<Row> {
        let before = self
            .ics(href)?
            .ok_or_else(|| StoreError::NotFound(href.to_string()))?;
        let before = Ical::parse(&before);
        let row = self.edit(href, &[Edit::Complete(at)], at)?;
        if row.task.is_open()
            && let Some(copy) = task::completed_copy(&before, at)
        {
            self.insert_new(&row.list, &copy)?;
        }
        Ok(row)
    }

    /// A copy of a task as a new one in the same list (see `task::duplicate`).
    pub fn duplicate(&mut self, href: &str, now: DateTime<Utc>) -> Result<Row> {
        let row = self
            .get(href)?
            .ok_or_else(|| StoreError::NotFound(href.to_string()))?;
        let list = self
            .lists()?
            .into_iter()
            .find(|l| l.href == row.list)
            .ok_or_else(|| StoreError::NoList(row.list.clone()))?;
        if !list.writable {
            return Err(StoreError::ReadOnly(list.name));
        }
        let ics = self
            .ics(href)?
            .ok_or_else(|| StoreError::NotFound(href.to_string()))?;
        let copy = task::duplicate(&Ical::parse(&ics), now)
            .ok_or_else(|| StoreError::Corrupt(format!("{href} is not a task")))?;
        self.insert_new(&row.list, &copy)
    }

    /// A whole object, new to the server.
    fn insert_new(&mut self, list: &str, ical: &Ical) -> Result<Row> {
        let task = Task::from_ical(ical).ok_or_else(|| StoreError::Corrupt("not a task".into()))?;
        let href = format!("{list}{}.ics", file_safe(&task.uid));
        let text = ical.to_string();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO tasks(href, list, uid, etag, base, edits, ics, summary, status, priority, json)
             VALUES (?1, ?2, ?3, NULL, ?4, '[]', ?4, '', '', 0, '{}')",
            params![href, list, task.uid, text],
        )?;
        write_derived(&tx, &href, &task, self.zone)?;
        tx.commit()?;
        self.get(&href)?.ok_or(StoreError::NotFound(href))
    }

    pub fn delete(&mut self, href: &str) -> Result<()> {
        let etag: Option<Option<String>> = self
            .conn
            .query_row("SELECT etag FROM tasks WHERE href = ?1", [href], |r| {
                r.get(0)
            })
            .optional()?;
        match etag {
            None => Err(StoreError::NotFound(href.to_string())),
            // Never reached the server: nothing to tell it.
            Some(None) => self.forget(href),
            Some(Some(_)) => {
                self.conn
                    .execute("UPDATE tasks SET deleted = 1 WHERE href = ?1", [href])?;
                Ok(())
            }
        }
    }

    /// Delete the completed tasks of one list, or of every list that can be
    /// written to. Returns how many.
    pub fn delete_completed(&mut self, list: Option<&str>) -> Result<usize> {
        let done = "deleted = 0 AND status IN ('completed', 'cancelled')
            AND (?1 IS NULL OR list = ?1) AND list IN (SELECT href FROM lists WHERE writable = 1)";
        let tx = self.conn.transaction()?;
        // Never on the server: nothing to tell it.
        let unsent = format!("SELECT href FROM tasks WHERE etag IS NULL AND {done}");
        for table in ["fired", "snoozed"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE href IN ({unsent})"),
                params![list],
            )?;
        }
        let forgotten = tx.execute(
            &format!("DELETE FROM tasks WHERE etag IS NULL AND {done}"),
            params![list],
        )?;
        let marked = tx.execute(
            &format!("UPDATE tasks SET deleted = 1 WHERE {done}"),
            params![list],
        )?;
        tx.commit()?;
        Ok(forgotten + marked)
    }

    /// Open and completed tasks in each list: href → (open, completed).
    pub fn counts(&self) -> Result<HashMap<String, (usize, usize)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT list, SUM(status IN ('needs-action', 'in-process')),
               SUM(status IN ('completed', 'cancelled'))
             FROM tasks WHERE deleted = 0 GROUP BY list",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)? as usize, r.get::<_, i64>(2)? as usize),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Move to another list: a new resource there with the same UID, and a
    /// delete here. CalDAV has MOVE, but not every client copes with it.
    pub fn move_to(&mut self, href: &str, list: &str) -> Result<Row> {
        let target = self.find_list(list)?;
        let row = self
            .get(href)?
            .ok_or_else(|| StoreError::NotFound(href.to_string()))?;
        if row.list == target.href {
            return Ok(row);
        }
        if !target.writable {
            return Err(StoreError::ReadOnly(target.name));
        }
        let ics: String =
            self.conn
                .query_row("SELECT ics FROM tasks WHERE href = ?1", [href], |r| {
                    r.get(0)
                })?;
        let moved = self.insert_new(&target.href, &Ical::parse(&ics))?;
        self.delete(href)?;
        Ok(moved)
    }

    pub fn get(&self, href: &str) -> Result<Option<Row>> {
        let sql = format!("SELECT {TASK_COLS} FROM tasks WHERE href = ?1 AND deleted = 0");
        self.conn
            .query_row(&sql, [href], row_from_sql)
            .optional()?
            .map(to_row)
            .transpose()
    }

    pub fn ics(&self, href: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT ics FROM tasks WHERE href = ?1", [href], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// A task by href, full UID, or unambiguous UID prefix (any case).
    pub fn find(&self, id: &str) -> Result<Row> {
        let id = id.trim();
        if id.is_empty() {
            return Err(StoreError::NotFound(id.to_string()));
        }
        if let Some(row) = self.get(id)? {
            return Ok(row);
        }
        let sql =
            format!("SELECT {TASK_COLS} FROM tasks WHERE deleted = 0 AND lower(uid) = lower(?1)");
        if let Some(row) = self.conn.query_row(&sql, [id], row_from_sql).optional()? {
            return to_row(row);
        }
        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks WHERE deleted = 0 AND substr(lower(uid), 1, length(?1)) = lower(?1) LIMIT 50"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map([id], row_from_sql)?
            .collect::<rusqlite::Result<_>>()?;
        match rows.len() {
            0 => Err(StoreError::NotFound(id.to_string())),
            1 => to_row(rows.into_iter().next().expect("one")),
            n => Err(StoreError::Ambiguous(id.to_string(), n)),
        }
    }

    pub fn by_source(&self, source: &str) -> Result<Option<Row>> {
        let sql =
            format!("SELECT {TASK_COLS} FROM tasks WHERE deleted = 0 AND source = ?1 LIMIT 1");
        self.conn
            .query_row(&sql, [source], row_from_sql)
            .optional()?
            .map(to_row)
            .transpose()
    }

    pub fn query(&self, q: &Query, today: NaiveDate) -> Result<Vec<Row>> {
        let today = today.format("%Y-%m-%d").to_string();
        let mut sql = format!("SELECT {TASK_COLS} FROM tasks WHERE deleted = 0");
        let open = "status IN ('needs-action', 'in-process')";
        match q.view {
            View::Today => sql.push_str(&format!(" AND {open} AND due_date <= ?1")),
            View::Upcoming => sql.push_str(&format!(" AND {open} AND due_date > ?1")),
            View::Open => sql.push_str(&format!(" AND {open} AND ?1 IS NOT NULL")),
            View::Completed => {
                sql.push_str(" AND status IN ('completed', 'cancelled') AND ?1 IS NOT NULL")
            }
        }
        sql.push_str(" AND (?2 IS NULL OR list = ?2)");
        sql.push_str(" AND (?3 IS NULL OR summary LIKE '%' || ?3 || '%' ESCAPE '\\' OR description LIKE '%' || ?3 || '%' ESCAPE '\\')");
        sql.push_str(match q.view {
            View::Completed => " ORDER BY completed_at DESC",
            _ => {
                " ORDER BY due_date IS NULL, due_date, due_timed = 0, due_at,
                   CASE WHEN priority BETWEEN 1 AND 9 THEN priority ELSE 10 END,
                   sort_order IS NULL, sort_order, created_at"
            }
        });
        sql.push_str(" LIMIT ?4");
        let list = match &q.list {
            Some(l) => Some(self.find_list(l)?.href),
            None => None,
        };
        let text = q.text.as_ref().map(|t| {
            t.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        });
        let limit = q.limit.unwrap_or(match q.view {
            View::Completed => 200,
            _ => 10_000,
        });
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map(params![today, list, text, i64::from(limit)], row_from_sql)?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter().map(to_row).collect()
    }

    pub fn uids(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT uid FROM tasks WHERE deleted = 0")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Open tasks with at least one location alarm.
    pub fn with_location_alarms(&self) -> Result<Vec<Row>> {
        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks WHERE deleted = 0 AND status IN ('needs-action', 'in-process')
             AND json LIKE '%\"location_alarms\":%'"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map([], row_from_sql)?
            .collect::<rusqlite::Result<_>>()?;
        let rows = rows.into_iter().map(to_row).collect::<Result<Vec<Row>>>()?;
        Ok(rows
            .into_iter()
            .filter(|r| !r.task.location_alarms.is_empty())
            .collect())
    }

    /// Open tasks with at least one alarm.
    pub fn with_alarms(&self) -> Result<Vec<Row>> {
        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks WHERE deleted = 0 AND status IN ('needs-action', 'in-process')
             AND json LIKE '%\"alarms\":%'"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map([], row_from_sql)?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter().map(to_row).collect()
    }

    pub fn fired(&self, href: &str, at: DateTime<Utc>) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM fired WHERE href = ?1 AND at = ?2",
                params![href, at.timestamp()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn mark_fired(&self, href: &str, at: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO fired(href, at) VALUES (?1, ?2)",
            params![href, at.timestamp()],
        )?;
        Ok(())
    }

    pub fn prune_fired(&self, before: DateTime<Utc>) -> Result<()> {
        self.conn
            .execute("DELETE FROM fired WHERE at < ?1", [before.timestamp()])?;
        Ok(())
    }

    pub fn snooze(&self, href: &str, until: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snoozed(href, until) VALUES (?1, ?2) ON CONFLICT(href) DO UPDATE SET until = excluded.until",
            params![href, until.timestamp()],
        )?;
        Ok(())
    }

    pub fn unsnooze(&self, href: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM snoozed WHERE href = ?1", [href])?;
        Ok(())
    }

    pub fn snoozed(&self) -> Result<Vec<(String, DateTime<Utc>)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT href, until FROM snoozed")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|(h, t)| DateTime::from_timestamp(t, 0).map(|t| (h, t)))
            .collect())
    }

    // -- GitHub repos ---------------------------------------------------------

    /// (repo, list href) of every linked repo.
    pub fn gh_links(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT repo, list FROM gh_links ORDER BY repo")?;
        Ok(stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Every link with what is known about its repo.
    pub fn gh_link_rows(&self) -> Result<Vec<GhLinkRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT repo, list, repo_id, archived, follow_from, follow_at FROM gh_links ORDER BY repo",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(GhLinkRow {
                    repo: r.get(0)?,
                    list: r.get(1)?,
                    repo_id: r
                        .get::<_, Option<i64>>(2)?
                        .and_then(|n| u64::try_from(n).ok()),
                    archived: r.get(3)?,
                    follow_from: r.get(4)?,
                    follow_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Tie a repo to a list, one repo per list. Tied to another list
    /// before, the repo's pairs go: they name that list's tasks. A link
    /// made anew starts without an id, which is learned on the next look.
    pub fn gh_link(&self, repo: &str, list: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM gh_links WHERE (repo = ?1 OR list = ?2) AND NOT (repo = ?1 AND list = ?2)",
            [repo, list],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO gh_links(repo, list) VALUES (?1, ?2)",
            [repo, list],
        )?;
        Ok(())
    }

    /// Remember the repo's id, once. Nothing is written, and this says so,
    /// when the link already has an id (it is never overwritten: that is
    /// what makes it the identity) or another link has this one.
    pub fn gh_learn_id(&self, repo: &str, id: u64) -> Result<bool> {
        // SQLite integers are signed; a GitHub id is nowhere near the end.
        let id = i64::try_from(id).map_err(|_| StoreError::Corrupt(format!("repo id {id}")))?;
        Ok(self.conn.execute(
            "UPDATE gh_links SET repo_id = ?2
             WHERE repo = ?1 AND repo_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM gh_links WHERE repo_id = ?2)",
            params![repo, id],
        )? > 0)
    }

    /// Record whether the repo is archived or disabled as of `at`. Seen so
    /// again, it keeps the time it was first seen; seen live, it is 0.
    pub fn gh_set_archived(&self, repo: &str, archived: bool, at: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE gh_links SET archived = CASE
                 WHEN NOT ?2 THEN 0
                 WHEN archived > 0 THEN archived
                 ELSE ?3 END
             WHERE repo = ?1",
            params![repo, archived, at.timestamp().max(1)],
        )?;
        Ok(())
    }

    /// The repo is called `new` now: its link and pairs move with it and
    /// nothing is paired again. Its id and archived baseline stay, since
    /// they are the repo's, not the name's. In the same transaction,
    /// `follow_at` marks the list as having a rename to follow (from `old`,
    /// when; a second rename before the first was followed replaces the
    /// marker) and `log` is written. Fails, changing nothing, when `new` is
    /// linked. Returns whether there was a link to move.
    pub fn gh_rename(
        &self,
        old: &str,
        new: &str,
        follow_at: Option<DateTime<Utc>>,
        log: Option<&NewLog>,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        // The pairs name the link by repo, and move with it.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        let moved = tx.execute("UPDATE gh_links SET repo = ?2 WHERE repo = ?1", [old, new])? > 0;
        tx.execute("UPDATE gh_pairs SET repo = ?2 WHERE repo = ?1", [old, new])?;
        if moved {
            if let Some(at) = follow_at {
                tx.execute(
                    "UPDATE gh_links SET follow_from = ?2, follow_at = ?3 WHERE repo = ?1",
                    params![new, old, at.timestamp()],
                )?;
            }
            if let Some(row) = log {
                insert_log(&tx, row)?;
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    /// The list has followed the rename, or will not: forget the marker.
    pub fn gh_clear_follow(&self, repo: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE gh_links SET follow_from = NULL, follow_at = NULL WHERE repo = ?1",
            [repo],
        )?;
        Ok(())
    }

    pub fn gh_unlink(&self, repo: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM gh_links WHERE repo = ?1", [repo])?
            > 0)
    }

    // -- the decision log: no foreign key, so it outlives an unlink --------

    /// Write a row; the log keeps its newest 500. Its id.
    pub fn gh_log_add(&self, row: &NewLog) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let id = insert_log(&tx, row)?;
        tx.commit()?;
        Ok(id)
    }

    /// The newest rows first, of one repo or all.
    pub fn gh_log_recent(&self, repo: Option<&str>, limit: u32) -> Result<Vec<LogRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, at, repo, list, kind, text, data, by FROM gh_log
             WHERE ?1 IS NULL OR repo = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        Ok(stmt
            .query_map(params![repo, limit], |r| {
                Ok(LogRow {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    repo: r.get(2)?,
                    list: r.get(3)?,
                    kind: r.get(4)?,
                    text: r.get(5)?,
                    data: r.get(6)?,
                    by: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Change what a row says it is, e.g. an intent (`list-renaming`) that
    /// has been carried out (`list-renamed`). Whether the row was there.
    pub fn gh_log_retag(&self, id: i64, kind: &str, text: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE gh_log SET kind = ?2, text = ?3 WHERE id = ?1",
            params![id, kind, text],
        )? > 0)
    }

    /// Every task of a list, open and completed, in the order they were
    /// made: the GitHub sync pairs issues with both.
    pub fn list_tasks(&self, list: &str) -> Result<Vec<Row>> {
        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks WHERE list = ?1 AND deleted = 0
             ORDER BY created_at, href"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows: Vec<_> = stmt
            .query_map([list], row_from_sql)?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter().map(to_row).collect()
    }

    pub fn gh_pairs(&self, repo: &str) -> Result<Vec<Pair>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT number, uid, title, open, body, priority FROM gh_pairs WHERE repo = ?1 ORDER BY number",
        )?;
        Ok(stmt
            .query_map([repo], |r| {
                Ok(Pair {
                    number: r.get(0)?,
                    uid: r.get(1)?,
                    title: r.get(2)?,
                    open: r.get(3)?,
                    body: r.get(4)?,
                    priority: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Pair an issue with a task, or remember what a pair agreed on now.
    pub fn gh_pair(&self, repo: &str, p: &Pair) -> Result<()> {
        self.conn.execute(
            "INSERT INTO gh_pairs(repo, number, uid, title, open, body, priority)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(repo, number) DO UPDATE SET uid = excluded.uid, title = excluded.title,
               open = excluded.open, body = excluded.body, priority = excluded.priority",
            params![repo, p.number, p.uid, p.title, p.open, p.body, p.priority],
        )?;
        Ok(())
    }

    pub fn gh_unpair(&self, repo: &str, number: u32) -> Result<()> {
        self.conn.execute(
            "DELETE FROM gh_pairs WHERE repo = ?1 AND number = ?2",
            params![repo, number],
        )?;
        Ok(())
    }

    // --- time blocks (asst track) ---

    pub fn blocks_between(&self, start_ms: i64, end_ms: i64) -> Result<Vec<Block>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_uid, start, end, label, planned, source, created_at
             FROM blocks WHERE start < ?2 AND end > ?1 ORDER BY start",
        )?;
        let rows = stmt.query_map(params![start_ms, end_ms], |r| {
            Ok(Block {
                id: Some(r.get(0)?),
                task_uid: r.get(1)?,
                start: r.get(2)?,
                end: r.get(3)?,
                label: r.get(4)?,
                planned: r.get::<_, i64>(5)? != 0,
                source: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The labels of the actual blocks (`planned = 0`) of the database at
    /// `path`, longest first, for a tool that only reads them. The file is
    /// opened read-only and as it is: nothing is migrated, backed up or
    /// written, so this is safe beside an asstd of any version, and a
    /// database that has no blocks (or no file) has no labels.
    pub fn label_totals(path: &Path) -> Result<Vec<LabelTotal>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let has_blocks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'blocks'",
            [],
            |r| r.get(0),
        )?;
        if has_blocks == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "SELECT label, COUNT(*), SUM(end - start) / 1000 FROM blocks
             WHERE planned = 0 AND label IS NOT NULL
             GROUP BY label ORDER BY SUM(end - start) DESC, label",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LabelTotal {
                label: r.get(0)?,
                blocks: r.get::<_, i64>(1)?.try_into().unwrap_or(0),
                seconds: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Drop actual blocks overlapping [start, end) and insert `rows`.
    /// Window replace: late AFK data can move boundaries.
    pub fn replace_actual_blocks(
        &mut self,
        start_ms: i64,
        end_ms: i64,
        rows: &[Block],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM blocks WHERE planned = 0 AND start < ?2 AND end > ?1",
            params![start_ms, end_ms],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO blocks(task_uid, start, end, label, planned, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)
                 ON CONFLICT(source) DO UPDATE SET
                   task_uid = excluded.task_uid,
                   start = excluded.start,
                   end = excluded.end,
                   label = excluded.label,
                   created_at = excluded.created_at",
            )?;
            for b in rows {
                stmt.execute(params![
                    b.task_uid,
                    b.start,
                    b.end,
                    b.label,
                    b.source,
                    b.created_at
                ])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    pub fn delete_blocks_by_source_prefix(&self, prefix: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM blocks WHERE source LIKE ?1",
            params![format!("{prefix}%")],
        )?)
    }
}

/// A list's row and everything hanging off it. Returns whether it existed.
fn remove_list_rows(conn: &Connection, href: &str) -> Result<bool> {
    let in_list = "SELECT href FROM tasks WHERE list = ?1";
    conn.execute(
        &format!("DELETE FROM fired WHERE href IN ({in_list})"),
        [href],
    )?;
    conn.execute(
        &format!("DELETE FROM snoozed WHERE href IN ({in_list})"),
        [href],
    )?;
    conn.execute("DELETE FROM tasks WHERE list = ?1", [href])?;
    conn.execute("DELETE FROM gh_links WHERE list = ?1", [href])?;
    conn.execute("DELETE FROM meta WHERE key = 'full-sync:' || ?1", [href])?;
    Ok(conn.execute("DELETE FROM lists WHERE href = ?1", [href])? > 0)
}

fn file_safe(uid: &str) -> String {
    uid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Insert a log row and trim the log to its newest `GH_LOG_KEEP`.
fn insert_log(conn: &Connection, row: &NewLog) -> Result<i64> {
    conn.execute(
        "INSERT INTO gh_log(at, repo, list, kind, text, data, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.at.timestamp(),
            row.repo,
            row.list,
            row.kind,
            row.text,
            row.data,
            row.by
        ],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM gh_log WHERE id NOT IN (SELECT id FROM gh_log ORDER BY id DESC LIMIT ?1)",
        [GH_LOG_KEEP],
    )?;
    Ok(id)
}

fn write_derived(conn: &Connection, href: &str, t: &Task, zone: Tz) -> Result<()> {
    let due_at = t.due.as_ref().map(|d| d.instant(zone).timestamp());
    let due_date = t
        .due
        .as_ref()
        .map(|d| d.local_date(zone).format("%Y-%m-%d").to_string());
    conn.execute(
        "UPDATE tasks SET uid = ?2, summary = ?3, description = ?4, status = ?5, priority = ?6, due_at = ?7,
           due_date = ?8, due_timed = ?9, completed_at = ?10, source = ?11, sort_order = ?12,
           created_at = ?13, json = ?14
         WHERE href = ?1",
        params![
            href,
            t.uid,
            t.summary,
            t.description,
            t.status.as_str(),
            i64::from(t.priority),
            due_at,
            due_date,
            t.due.as_ref().map(|d| d.has_time()),
            t.completed.map(|c| c.timestamp()),
            t.source,
            t.sort_order,
            t.created.map(|c| c.timestamp()),
            serde_json::to_string(t).expect("task serializes"),
        ],
    )?;
    Ok(())
}

/// Does the server copy already show what these edits would do?
fn already_applied(expected: &Task, server: &Task, edits: &[Edit]) -> bool {
    edits.iter().all(|e| match e {
        Edit::Summary(_) => expected.summary == server.summary,
        Edit::Description(_) => expected.description == server.description,
        Edit::Due(_) => expected.due == server.due,
        Edit::Priority(_) => expected.priority == server.priority,
        Edit::Complete(_) | Edit::Reopen => {
            expected.status == server.status
                && expected.due == server.due
                && expected.rrule == server.rrule
        }
        Edit::Rrule(_) => expected.rrule == server.rrule,
        Edit::Alarms(_) => {
            let triggers = |t: &Task| {
                t.alarms
                    .iter()
                    .map(|a| a.trigger.clone())
                    .collect::<Vec<_>>()
            };
            triggers(expected) == triggers(server)
        }
        Edit::LocationAlarms(_) => expected.location_alarms == server.location_alarms,
        Edit::Parent(_) => expected.parent == server.parent,
        Edit::Source(_) => expected.source == server.source,
        Edit::SortOrder(_) => expected.sort_order == server.sort_order,
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::time::When;

    fn ny() -> Tz {
        "America/New_York".parse().unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 14, 16, 0, 0).unwrap()
    }

    fn list(href: &str, name: &str, token: &str) -> RemoteList {
        RemoteList {
            href: href.into(),
            name: name.into(),
            color: None,
            order: None,
            sync_token: Some(token.into()),
            ctag: None,
            writable: true,
        }
    }

    const SERVER: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//iOS 18.6//EN\r\nBEGIN:VTODO\r\n\
UID:ABC-123\r\nSUMMARY:Buy milk\r\nDUE;VALUE=DATE:20260914\r\nX-APPLE-SORT-ORDER:3\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

    fn store() -> Store {
        let mut s = Store::in_memory(ny()).unwrap();
        s.apply_lists(&[
            list("/cal/inbox/", "Inbox", "t1"),
            list("/cal/work/", "Work", "t1"),
        ])
        .unwrap();
        s
    }

    #[test]
    fn lists_report_what_needs_a_pull() {
        let mut s = store();
        s.set_token("/cal/inbox/", Some("t1")).unwrap();
        s.set_token("/cal/work/", Some("t1")).unwrap();
        let (stale, changed) = s
            .apply_lists(&[
                list("/cal/inbox/", "Inbox", "t1"),
                list("/cal/work/", "Work", "t2"),
            ])
            .unwrap();
        assert_eq!(
            stale.iter().map(|l| l.href.as_str()).collect::<Vec<_>>(),
            vec!["/cal/work/"]
        );
        assert!(!changed);
        let (_, changed) = s
            .apply_lists(&[list("/cal/inbox/", "Inbox", "t1")])
            .unwrap();
        assert!(changed);
        assert_eq!(s.lists().unwrap().len(), 1);
        assert_eq!(s.find_list("inb").unwrap().name, "Inbox");
    }

    #[test]
    fn server_copy_then_local_edit_then_push() {
        let mut s = store();
        assert!(
            s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", SERVER, now())
                .unwrap()
        );
        assert!(s.pending().unwrap().is_empty());

        let row = s
            .edit(
                "/cal/inbox/a.ics",
                &[Edit::Summary("Buy oat milk".into())],
                now(),
            )
            .unwrap();
        assert!(row.pending);
        let pending = s.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].op, Op::Update);
        assert!(pending[0].body.contains(
            "SUMMARY:Buy oat milk\r\nDUE;VALUE=DATE:20260914\r\nX-APPLE-SORT-ORDER:3\r\n"
        ));

        // Another edit lands while the first is in flight.
        s.edit("/cal/inbox/a.ics", &[Edit::Priority(1)], now())
            .unwrap();
        s.pushed(&pending[0], "\"e2\"", &pending[0].body, now())
            .unwrap();
        let after = s.pending().unwrap();
        assert_eq!(after.len(), 1, "the priority edit is still queued");
        assert_eq!(after[0].sent_edits, 1);
        assert!(after[0].body.contains("PRIORITY:1"));
        assert_eq!(after[0].etag.as_deref(), Some("\"e2\""));
    }

    #[test]
    fn a_newer_server_copy_keeps_unsent_edits() {
        let mut s = store();
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", SERVER, now())
            .unwrap();
        s.edit("/cal/inbox/a.ics", &[Edit::Priority(1)], now())
            .unwrap();
        let changed_elsewhere = SERVER.replace("Buy milk", "Buy milk and eggs");
        s.put_server(
            "/cal/inbox/",
            "/cal/inbox/a.ics",
            "\"e3\"",
            &changed_elsewhere,
            now(),
        )
        .unwrap();
        let row = s.get("/cal/inbox/a.ics").unwrap().unwrap();
        assert_eq!(row.task.summary, "Buy milk and eggs");
        assert_eq!(row.task.priority, 1);
        assert!(row.pending);
    }

    #[test]
    fn a_lost_response_does_not_complete_a_recurring_task_twice() {
        let mut s = store();
        let daily = SERVER.replace(
            "X-APPLE-SORT-ORDER:3\r\n",
            "X-APPLE-SORT-ORDER:3\r\nRRULE:FREQ=DAILY\r\n",
        );
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", &daily, now())
            .unwrap();
        let row = s
            .edit("/cal/inbox/a.ics", &[Edit::Complete(now())], now())
            .unwrap();
        assert_eq!(
            row.task.due,
            Some(When::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
            })
        );
        // The PUT reached the server but its answer didn't reach us; the next
        // pull brings back our own write.
        let written = s.pending().unwrap()[0].body.clone();
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e2\"", &written, now())
            .unwrap();
        let row = s.get("/cal/inbox/a.ics").unwrap().unwrap();
        assert!(!row.pending);
        assert_eq!(
            row.task.due,
            Some(When::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
            })
        );
    }

    #[test]
    fn create_find_query_delete() {
        let mut s = store();
        let due = When::Date {
            date: NaiveDate::from_ymd_opt(2026, 9, 13).unwrap(),
        };
        let row = s
            .create(
                "inbox",
                &[
                    Edit::Summary("Overdue thing".into()),
                    Edit::Due(Some(due)),
                    Edit::Source(Some("steno:x/1".into())),
                ],
                now(),
            )
            .unwrap();
        s.create("work", &[Edit::Summary("Someday".into())], now())
            .unwrap();
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", SERVER, now())
            .unwrap();

        let today = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let q = |view, list: Option<&str>, text: Option<&str>| Query {
            view,
            list: list.map(str::to_string),
            text: text.map(str::to_string),
            limit: None,
        };
        let names = |rows: Vec<Row>| rows.into_iter().map(|r| r.task.summary).collect::<Vec<_>>();
        assert_eq!(
            names(s.query(&q(View::Today, None, None), today).unwrap()),
            vec!["Overdue thing", "Buy milk"]
        );
        assert_eq!(
            names(s.query(&q(View::Open, Some("work"), None), today).unwrap()),
            vec!["Someday"]
        );
        assert_eq!(
            names(s.query(&q(View::Open, None, Some("mil")), today).unwrap()),
            vec!["Buy milk"]
        );
        assert_eq!(s.find(&row.task.uid[..6]).unwrap().href, row.href);
        assert_eq!(s.find("abc-1").unwrap().task.summary, "Buy milk");
        assert!(matches!(s.find("zzz"), Err(StoreError::NotFound(_))));
        assert_eq!(s.by_source("steno:x/1").unwrap().unwrap().href, row.href);

        // A task the server never saw just goes; one it has waits for a DELETE.
        s.delete(&row.href).unwrap();
        assert!(s.get(&row.href).unwrap().is_none());
        s.delete("/cal/inbox/a.ics").unwrap();
        assert!(s.get("/cal/inbox/a.ics").unwrap().is_none());
        let pending = s.pending().unwrap();
        assert!(
            pending
                .iter()
                .any(|p| p.href == "/cal/inbox/a.ics" && p.op == Op::Delete)
        );
    }

    #[test]
    fn a_list_gone_from_the_server_takes_its_tasks_and_unsent_edits() {
        let mut s = store();
        s.put_server("/cal/work/", "/cal/work/a.ics", "\"e1\"", SERVER, now())
            .unwrap();
        let other = SERVER.replace("ABC-123", "DEF-456");
        s.put_server("/cal/work/", "/cal/work/b.ics", "\"e1\"", &other, now())
            .unwrap();
        s.edit("/cal/work/a.ics", &[Edit::Priority(1)], now())
            .unwrap();
        s.delete("/cal/work/b.ics").unwrap();
        let fresh = s
            .create("work", &[Edit::Summary("New".into())], now())
            .unwrap();
        s.create("inbox", &[Edit::Summary("Stays".into())], now())
            .unwrap();
        s.snooze("/cal/work/a.ics", now()).unwrap();
        s.mark_fired(&fresh.href, now()).unwrap();
        s.gh_link("jaehho/asst", "/cal/work/").unwrap();
        s.set_meta("full-sync:/cal/work/", "1").unwrap();
        assert_eq!(s.pending().unwrap().len(), 4);

        let (_, changed) = s
            .apply_lists(&[list("/cal/inbox/", "Inbox", "t1")])
            .unwrap();
        assert!(changed);
        let pending = s.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].list, "/cal/inbox/");
        let rows: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM tasks WHERE list = '/cal/work/'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
        assert!(s.snoozed().unwrap().is_empty());
        assert!(!s.fired(&fresh.href, now()).unwrap());
        assert!(s.gh_links().unwrap().is_empty());
        assert_eq!(s.meta("full-sync:/cal/work/").unwrap(), None);

        assert!(s.remove_list("/cal/inbox/").unwrap());
        assert!(!s.remove_list("/cal/inbox/").unwrap());
        assert!(s.pending().unwrap().is_empty());
    }

    #[test]
    fn pairs_follow_their_link() {
        let mut s = store();
        let one = s
            .create("/cal/work/", &[Edit::Summary("One".into())], now())
            .unwrap();
        let done = s
            .create("/cal/work/", &[Edit::Summary("Two".into())], now())
            .unwrap();
        s.complete(&done.href, now()).unwrap();
        // Both halves of the list, open and completed
        let titles: Vec<(String, bool)> = s
            .list_tasks("/cal/work/")
            .unwrap()
            .into_iter()
            .map(|r| (r.task.summary.clone(), r.task.is_open()))
            .collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&("One".into(), true)));
        assert!(titles.contains(&("Two".into(), false)));

        let pair = |title: &str, open: bool| Pair {
            number: 7,
            uid: one.task.uid.clone(),
            title: title.into(),
            open,
            body: None,
            priority: 0,
        };
        s.gh_link("jaehho/asst", "/cal/work/").unwrap();
        s.gh_pair("jaehho/asst", &pair("One", true)).unwrap();
        // Pairing again updates what was agreed
        s.gh_pair("jaehho/asst", &pair("Uno", false)).unwrap();
        assert_eq!(s.gh_pairs("jaehho/asst").unwrap(), [pair("Uno", false)]);
        // Linking the same list again keeps them; another list drops them
        s.gh_link("jaehho/asst", "/cal/work/").unwrap();
        assert_eq!(s.gh_pairs("jaehho/asst").unwrap().len(), 1);
        s.gh_link("jaehho/asst", "/cal/inbox/").unwrap();
        assert!(s.gh_pairs("jaehho/asst").unwrap().is_empty());
        assert_eq!(
            s.gh_links().unwrap(),
            [("jaehho/asst".to_string(), "/cal/inbox/".to_string())]
        );
        // A renamed repo keeps its link and pairs
        s.gh_pair("jaehho/asst", &pair("One", true)).unwrap();
        s.gh_rename("jaehho/asst", "jaehho/assistant", None, None)
            .unwrap();
        assert_eq!(
            s.gh_links().unwrap(),
            [("jaehho/assistant".to_string(), "/cal/inbox/".to_string())]
        );
        assert_eq!(s.gh_pairs("jaehho/assistant").unwrap(), [pair("One", true)]);
        assert!(s.gh_pairs("jaehho/asst").unwrap().is_empty());
        s.gh_rename("jaehho/assistant", "jaehho/asst", None, None)
            .unwrap();
        // Unlinking forgets them too
        s.gh_pair("jaehho/asst", &pair("One", true)).unwrap();
        s.gh_unpair("jaehho/asst", 7).unwrap();
        assert!(s.gh_pairs("jaehho/asst").unwrap().is_empty());
        s.gh_pair("jaehho/asst", &pair("One", true)).unwrap();
        assert!(s.gh_unlink("jaehho/asst").unwrap());
        assert!(!s.gh_unlink("jaehho/asst").unwrap());
        assert!(s.gh_pairs("jaehho/asst").unwrap().is_empty());
    }

    #[test]
    fn a_duplicate_is_a_new_task_waiting_to_be_sent() {
        let mut s = store();
        let sourced = SERVER.replace(
            "X-APPLE-SORT-ORDER:3\r\n",
            "X-APPLE-SORT-ORDER:3\r\nX-ASST-SOURCE:mail:1\r\n",
        );
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", &sourced, now())
            .unwrap();
        let copy = s.duplicate("/cal/inbox/a.ics", now()).unwrap();
        assert_ne!(copy.href, "/cal/inbox/a.ics");
        assert_eq!(copy.list, "/cal/inbox/");
        assert!(copy.pending);
        assert_eq!(copy.task.summary, "Buy milk");
        assert_eq!(copy.task.source, None);
        let ops: Vec<(String, Op)> = s
            .pending()
            .unwrap()
            .into_iter()
            .map(|p| (p.href, p.op))
            .collect();
        assert_eq!(ops, vec![(copy.href.clone(), Op::Create)]);
        let original = s.get("/cal/inbox/a.ics").unwrap().unwrap();
        assert!(!original.pending);
        assert_eq!(s.by_source("mail:1").unwrap().unwrap().href, original.href);

        s.apply_lists(&[RemoteList {
            writable: false,
            ..list("/cal/inbox/", "Inbox", "t1")
        }])
        .unwrap();
        assert!(matches!(
            s.duplicate("/cal/inbox/a.ics", now()),
            Err(StoreError::ReadOnly(_))
        ));
    }

    #[test]
    fn a_lost_response_to_a_reorder_is_recognized() {
        let mut s = store();
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", SERVER, now())
            .unwrap();
        s.edit("/cal/inbox/a.ics", &[Edit::SortOrder(Some(10))], now())
            .unwrap();
        let written = s.pending().unwrap()[0].body.clone();
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e2\"", &written, now())
            .unwrap();
        let row = s.get("/cal/inbox/a.ics").unwrap().unwrap();
        assert!(!row.pending);
        assert_eq!(row.task.sort_order, Some(10));
    }

    #[test]
    fn moving_creates_there_and_deletes_here() {
        let mut s = store();
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", SERVER, now())
            .unwrap();
        let moved = s.move_to("/cal/inbox/a.ics", "Work").unwrap();
        assert_eq!(moved.href, "/cal/work/ABC-123.ics");
        let ops: Vec<(String, Op)> = s
            .pending()
            .unwrap()
            .into_iter()
            .map(|p| (p.href, p.op))
            .collect();
        assert!(ops.contains(&("/cal/inbox/a.ics".into(), Op::Delete)));
        assert!(ops.contains(&("/cal/work/ABC-123.ics".into(), Op::Create)));
        assert_eq!(s.find("ABC-123").unwrap().list, "/cal/work/");
    }

    #[test]
    fn a_zone_change_moves_local_due_dates() {
        let dir = std::env::temp_dir().join(format!("asst-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("asst.db");
        {
            let mut s = Store::open(&path, ny()).unwrap();
            s.apply_lists(&[list("/cal/inbox/", "Inbox", "t1")])
                .unwrap();
            let late = SERVER.replace("DUE;VALUE=DATE:20260914", "DUE:20260915T023000Z");
            s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e1\"", &late, now())
                .unwrap();
            let today = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
            let q = crate::api::query(View::Today);
            assert_eq!(
                s.query(&q, today).unwrap().len(),
                1,
                "22:30 on the 14th in New York"
            );
        }
        let s = Store::open(&path, "Asia/Seoul".parse().unwrap()).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let q = crate::api::query(View::Today);
        assert_eq!(
            s.query(&q, today).unwrap().len(),
            0,
            "11:30 on the 15th in Seoul"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn completed_tasks_go_together() {
        let mut s = store();
        let done = SERVER.replace("SUMMARY:Buy milk", "SUMMARY:Bought\r\nSTATUS:COMPLETED");
        let other = |uid: &str| done.replace("ABC-123", uid);
        s.put_server("/cal/inbox/", "/cal/inbox/a.ics", "\"e\"", SERVER, now())
            .unwrap();
        s.put_server("/cal/inbox/", "/cal/inbox/b.ics", "\"e\"", &done, now())
            .unwrap();
        s.put_server("/cal/work/", "/cal/work/c.ics", "\"e\"", &other("C"), now())
            .unwrap();
        let local = s
            .create("/cal/work/", &[Edit::Summary("Draft".into())], now())
            .unwrap();
        s.complete(&local.href, now()).unwrap();
        let counts = s.counts().unwrap();
        assert_eq!(counts["/cal/inbox/"], (1, 1));
        assert_eq!(counts["/cal/work/"], (0, 2));

        assert_eq!(s.delete_completed(Some("/cal/inbox/")).unwrap(), 1);
        assert_eq!(s.counts().unwrap()["/cal/inbox/"], (1, 0));
        assert_eq!(s.delete_completed(None).unwrap(), 2);
        assert!(!s.counts().unwrap().contains_key("/cal/work/"));
        let deletes: Vec<String> = s
            .pending()
            .unwrap()
            .into_iter()
            .filter(|p| p.op == Op::Delete)
            .map(|p| p.href)
            .collect();
        assert_eq!(deletes, ["/cal/inbox/b.ics", "/cal/work/c.ics"]);
        assert!(
            s.get(&local.href).unwrap().is_none(),
            "never sent: just gone"
        );
    }

    // -- repo identity: migration, backup, marker, log -----------------------

    /// A scratch directory that goes with the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("asst-store-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn db(&self) -> PathBuf {
            self.0.join("asst.db")
        }

        /// What is in the directory, by name.
        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A database as the build before the identity columns left it: links
    /// with only a repo and a list, and their pairs.
    fn old_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE gh_links (repo TEXT PRIMARY KEY, list TEXT NOT NULL UNIQUE);
             CREATE TABLE gh_pairs (
               repo TEXT NOT NULL REFERENCES gh_links(repo) ON DELETE CASCADE,
               number INTEGER NOT NULL,
               uid TEXT NOT NULL,
               title TEXT NOT NULL,
               open INTEGER NOT NULL,
               body TEXT,
               priority INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (repo, number)
             );
             INSERT INTO gh_links VALUES ('jaehho/lab', '/cal/lab/'), ('jaehho/asst', '/cal/asst/');
             INSERT INTO gh_pairs VALUES
               ('jaehho/lab', 1, 'u1', 'One', 1, NULL, 0),
               ('jaehho/lab', 2, 'u2', 'Two', 0, 'notes', 5),
               ('jaehho/asst', 7, 'u7', 'Seven', 1, NULL, 0);",
        )
        .unwrap();
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn read_only(path: &Path) -> Connection {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap()
    }

    fn new_log<'a>(repo: &'a str, kind: &'a str, text: &'a str) -> NewLog<'a> {
        NewLog {
            at: at(18),
            repo,
            list: Some("/cal/work/"),
            kind,
            text,
            data: None,
            by: "auto",
        }
    }

    #[test]
    fn an_old_database_gains_the_identity_columns_and_a_backup() {
        let dir = Scratch::new("migrate");
        old_database(&dir.db());
        let backup = identity_backup_path(&dir.db());
        assert!(!backup.exists());

        let s = Store::open(&dir.db(), ny()).unwrap();
        // Everything that was there is there, and nothing is known yet
        assert_eq!(
            s.gh_links().unwrap(),
            [
                ("jaehho/asst".to_string(), "/cal/asst/".to_string()),
                ("jaehho/lab".to_string(), "/cal/lab/".to_string()),
            ]
        );
        let rows = s.gh_link_rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.repo_id.is_none()
            && r.archived.is_none()
            && r.follow_from.is_none()
            && r.follow_at.is_none()));
        let pairs = s.gh_pairs("jaehho/lab").unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[1].body.as_deref(), Some("notes"));
        assert_eq!(pairs[1].priority, 5);
        assert_eq!(s.gh_pairs("jaehho/asst").unwrap().len(), 1);
        // The migration reads nobody and says nothing
        assert!(s.gh_log_recent(None, 10).unwrap().is_empty());

        // The copy was taken before the columns were added
        let copy = read_only(&backup);
        assert_eq!(count(&copy, "gh_links"), 2);
        assert_eq!(count(&copy, "gh_pairs"), 3);
        assert!(!has_column(&copy, "gh_links", "repo_id").unwrap());
        drop(copy);
        assert_eq!(
            dir.files(),
            [
                "asst.db",
                "asst.db-shm",
                "asst.db-wal",
                "asst.db.pre-identity"
            ]
        );

        // Opening again, with something learned in between, changes nothing
        // about the copy: it is the only record of the way things were
        let before = std::fs::read(&backup).unwrap();
        assert!(s.gh_learn_id("jaehho/lab", 42).unwrap());
        drop(s);
        let s = Store::open(&dir.db(), ny()).unwrap();
        assert_eq!(s.gh_link_rows().unwrap()[1].repo_id, Some(42));
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        // Not even when it has gone: a migrated database has nothing to save
        drop(s);
        std::fs::remove_file(&backup).unwrap();
        Store::open(&dir.db(), ny()).unwrap();
        assert!(!backup.exists());
    }

    #[test]
    fn a_new_database_has_nothing_to_back_up() {
        let dir = Scratch::new("fresh");
        let s = Store::open(&dir.db(), ny()).unwrap();
        assert!(s.gh_link_rows().unwrap().is_empty());
        assert!(has_column(&s.conn, "gh_links", "repo_id").unwrap());
        assert!(!identity_backup_path(&dir.db()).exists());
        drop(s);
        let s = Store::open(&dir.db(), ny()).unwrap();
        drop(s);
        assert!(!identity_backup_path(&dir.db()).exists());
        // And a store in memory never takes one
        let m = Store::in_memory(ny()).unwrap();
        assert!(has_column(&m.conn, "gh_links", "repo_id").unwrap());
        assert!(has_column(&m.conn, "gh_links", "follow_at").unwrap());
    }

    #[test]
    fn a_copy_already_there_is_never_replaced_and_a_half_copy_never_counts() {
        // A copy the owner made by hand stays, whatever it is
        let dir = Scratch::new("kept");
        old_database(&dir.db());
        let backup = identity_backup_path(&dir.db());
        std::fs::write(&backup, "mine").unwrap();
        let s = Store::open(&dir.db(), ny()).unwrap();
        assert!(has_column(&s.conn, "gh_links", "repo_id").unwrap());
        assert_eq!(std::fs::read(&backup).unwrap(), b"mine");
        drop(s);

        // The leftover of a copy that died half way is not the copy: it is
        // in the way of the next try only until that one starts over
        let dir = Scratch::new("half");
        old_database(&dir.db());
        let backup = identity_backup_path(&dir.db());
        let mut half = backup.as_os_str().to_owned();
        half.push(format!(".tmp{}", std::process::id()));
        std::fs::write(&half, "not a database").unwrap();
        drop(Store::open(&dir.db(), ny()).unwrap());
        let copy = read_only(&backup);
        assert_eq!(count(&copy, "gh_links"), 2);
        assert_eq!(count(&copy, "gh_pairs"), 3);
        assert!(!PathBuf::from(&half).exists());
    }

    #[test]
    fn a_copy_that_cannot_be_made_stops_the_migration() {
        let dir = Scratch::new("nocopy");
        old_database(&dir.db());
        let backup = identity_backup_path(&dir.db());
        // Something in the way of the copy's working name
        let mut blocked = backup.as_os_str().to_owned();
        blocked.push(format!(".tmp{}", std::process::id()));
        let blocked = PathBuf::from(blocked);
        std::fs::create_dir(&blocked).unwrap();

        // The open says so, and the database is as it was: no columns, no
        // copy, nothing migrated without something to go back to
        assert!(Store::open(&dir.db(), ny()).is_err());
        assert!(!has_column(&read_only(&dir.db()), "gh_links", "repo_id").unwrap());
        assert!(!backup.exists());

        // Once it can be made, the migration goes ahead
        std::fs::remove_dir(&blocked).unwrap();
        let s = Store::open(&dir.db(), ny()).unwrap();
        assert!(has_column(&s.conn, "gh_links", "repo_id").unwrap());
        assert!(backup.exists());
    }

    /// The statements of the build before this one, verbatim.
    fn old_build(conn: &Connection) {
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let links: Vec<(String, String)> = conn
            .prepare_cached("SELECT repo, list FROM gh_links ORDER BY repo")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!links.is_empty());
        conn.execute(
            "DELETE FROM gh_links WHERE (repo = ?1 OR list = ?2) AND NOT (repo = ?1 AND list = ?2)",
            ["jaehho/new", "/cal/new/"],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO gh_links(repo, list) VALUES (?1, ?2)",
            ["jaehho/new", "/cal/new/"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gh_pairs(repo, number, uid, title, open, body, priority)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(repo, number) DO UPDATE SET uid = excluded.uid, title = excluded.title,
               open = excluded.open, body = excluded.body, priority = excluded.priority",
            params!["jaehho/new", 1, "u", "T", true, None::<String>, 0],
        )
        .unwrap();
        // A rename
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute_batch("PRAGMA defer_foreign_keys = ON").unwrap();
        tx.execute(
            "UPDATE gh_links SET repo = ?2 WHERE repo = ?1",
            ["jaehho/lab", "jaehho/lab2"],
        )
        .unwrap();
        tx.execute(
            "UPDATE gh_pairs SET repo = ?2 WHERE repo = ?1",
            ["jaehho/lab", "jaehho/lab2"],
        )
        .unwrap();
        tx.commit().unwrap();
        // An unlink takes its pairs
        conn.execute("DELETE FROM gh_links WHERE repo = ?1", ["jaehho/asst"])
            .unwrap();
    }

    #[test]
    fn the_old_build_still_runs_on_the_migrated_database() {
        let dir = Scratch::new("downgrade");
        old_database(&dir.db());
        let s = Store::open(&dir.db(), ny()).unwrap();
        assert!(s.gh_learn_id("jaehho/lab", 42).unwrap());
        assert!(s.gh_learn_id("jaehho/asst", 43).unwrap());
        s.gh_set_archived("jaehho/lab", true, at(17)).unwrap();
        s.gh_rename("jaehho/lab", "jaehho/lab-x", Some(at(17)), None)
            .unwrap();
        s.gh_rename("jaehho/lab-x", "jaehho/lab", None, None)
            .unwrap();

        // The downgraded build opens the file and works as it always did
        let old = Connection::open(dir.db()).unwrap();
        old.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        old_build(&old);

        // What it did is there, and what it knows nothing of was left as
        // it was: the id went with the renamed repo, the new link has none
        let rows = s.gh_link_rows().unwrap();
        let by = |repo: &str| rows.iter().find(|r| r.repo == repo).cloned();
        let lab = by("jaehho/lab2").unwrap();
        assert_eq!(
            (lab.repo_id, lab.archived),
            (Some(42), Some(at(17).timestamp()))
        );
        // (the second of the two renames above set none, so the first stays)
        assert_eq!(lab.follow_from.as_deref(), Some("jaehho/lab"));
        let new = by("jaehho/new").unwrap();
        assert_eq!(
            (new.repo_id, new.archived, new.follow_from),
            (None, None, None)
        );
        assert!(by("jaehho/asst").is_none());
        assert_eq!(s.gh_pairs("jaehho/lab2").unwrap().len(), 2);
        assert!(s.gh_pairs("jaehho/asst").unwrap().is_empty());
        // The new build learns what the old one left blank, and nothing else
        assert!(s.gh_learn_id("jaehho/new", 44).unwrap());
        assert!(!s.gh_learn_id("jaehho/lab2", 99).unwrap());
    }

    #[test]
    fn an_old_build_running_during_the_migration_keeps_going() {
        let dir = Scratch::new("running");
        old_database(&dir.db());
        // asstd of the old build, started before asst migrated the file
        let old = Connection::open(dir.db()).unwrap();
        old.pragma_update(None, "journal_mode", "WAL").unwrap();
        old.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        let first = || -> Vec<String> {
            old.prepare_cached("SELECT repo, list FROM gh_links ORDER BY repo")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(first().len(), 2);

        drop(Store::open(&dir.db(), ny()).unwrap());

        // Its prepared statement meets a table with more columns
        assert_eq!(first().len(), 2);
        old_build(&old);
        assert_eq!(first(), ["jaehho/lab2", "jaehho/new"]);
    }

    #[test]
    fn a_rename_moves_the_link_keeps_the_id_and_marks_the_list() {
        let s = store();
        s.gh_link("jaehho/champalimaud", "/cal/work/").unwrap();
        let pair = Pair {
            number: 3,
            uid: "u".into(),
            title: "T".into(),
            open: true,
            body: None,
            priority: 0,
        };
        s.gh_pair("jaehho/champalimaud", &pair).unwrap();
        assert!(s.gh_learn_id("jaehho/champalimaud", 1379104005).unwrap());
        s.gh_set_archived("jaehho/champalimaud", true, at(10))
            .unwrap();

        let log = new_log(
            "jaehho/lab",
            "repo-renamed",
            "jaehho/champalimaud is now jaehho/lab",
        );
        assert!(
            s.gh_rename(
                "jaehho/champalimaud",
                "jaehho/lab",
                Some(at(18)),
                Some(&log)
            )
            .unwrap()
        );
        assert_eq!(
            s.gh_link_rows().unwrap(),
            [GhLinkRow {
                repo: "jaehho/lab".into(),
                list: "/cal/work/".into(),
                repo_id: Some(1379104005),
                archived: Some(at(10).timestamp()),
                follow_from: Some("jaehho/champalimaud".into()),
                follow_at: Some(at(18).timestamp()),
            }]
        );
        assert_eq!(s.gh_pairs("jaehho/lab").unwrap(), [pair]);
        assert!(s.gh_pairs("jaehho/champalimaud").unwrap().is_empty());
        let rows = s.gh_log_recent(None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                rows[0].repo.as_str(),
                rows[0].kind.as_str(),
                rows[0].by.as_str()
            ),
            ("jaehho/lab", "repo-renamed", "auto")
        );
        assert_eq!(rows[0].list.as_deref(), Some("/cal/work/"));

        // A rename with no marker (found after the fact) leaves the one
        // that is there; clearing it is its own step
        s.gh_rename("jaehho/lab", "jaehho/lab2", None, None)
            .unwrap();
        let row = &s.gh_link_rows().unwrap()[0];
        assert_eq!(row.follow_from.as_deref(), Some("jaehho/champalimaud"));
        s.gh_clear_follow("jaehho/lab2").unwrap();
        let row = &s.gh_link_rows().unwrap()[0];
        assert_eq!((row.follow_from.clone(), row.follow_at), (None, None));
        assert_eq!(row.repo_id, Some(1379104005));
        // A second rename before the list followed the first replaces the
        // marker with the newer one
        s.gh_rename("jaehho/lab2", "jaehho/lab3", Some(at(19)), None)
            .unwrap();
        s.gh_rename("jaehho/lab3", "jaehho/lab4", Some(at(20)), None)
            .unwrap();
        let row = &s.gh_link_rows().unwrap()[0];
        assert_eq!(row.follow_from.as_deref(), Some("jaehho/lab3"));
        assert_eq!(row.follow_at, Some(at(20).timestamp()));
    }

    #[test]
    fn a_rename_that_cannot_happen_changes_nothing() {
        let s = store();
        s.gh_link("jaehho/a", "/cal/work/").unwrap();
        s.gh_link("jaehho/b", "/cal/inbox/").unwrap();
        s.gh_learn_id("jaehho/a", 1).unwrap();
        let before = s.gh_link_rows().unwrap();
        let log = new_log("jaehho/b", "repo-renamed", "x");
        // b is linked already: the link, the marker and the row all stay
        assert!(
            s.gh_rename("jaehho/a", "jaehho/b", Some(at(18)), Some(&log))
                .is_err()
        );
        assert_eq!(s.gh_link_rows().unwrap(), before);
        assert!(s.gh_log_recent(None, 10).unwrap().is_empty());
        // Nothing linked under that name: nothing to mark or to say
        assert!(
            !s.gh_rename("jaehho/none", "jaehho/c", Some(at(18)), Some(&log))
                .unwrap()
        );
        assert_eq!(s.gh_link_rows().unwrap(), before);
        assert!(s.gh_log_recent(None, 10).unwrap().is_empty());
    }

    #[test]
    fn a_link_made_anew_starts_without_an_id() {
        let s = store();
        s.gh_link("jaehho/a", "/cal/work/").unwrap();
        s.gh_learn_id("jaehho/a", 1).unwrap();
        s.gh_set_archived("jaehho/a", true, at(10)).unwrap();
        // The same link again is the link it was
        s.gh_link("jaehho/a", "/cal/work/").unwrap();
        let row = s.gh_link_rows().unwrap().remove(0);
        assert_eq!(
            (row.repo_id, row.archived),
            (Some(1), Some(at(10).timestamp()))
        );
        // Another repo for the list replaces it, and is not that repo
        s.gh_link("jaehho/b", "/cal/work/").unwrap();
        let rows = s.gh_link_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].repo.as_str(), rows[0].repo_id, rows[0].archived),
            ("jaehho/b", None, None)
        );
        // The repo for another list is a new link too
        s.gh_link("jaehho/b", "/cal/inbox/").unwrap();
        assert_eq!(s.gh_link_rows().unwrap()[0].repo_id, None);
    }

    #[test]
    fn an_id_is_learned_once_and_belongs_to_one_link() {
        let s = store();
        s.gh_link("jaehho/a", "/cal/work/").unwrap();
        s.gh_link("jaehho/b", "/cal/inbox/").unwrap();
        let id_of = |repo: &str| {
            s.gh_link_rows()
                .unwrap()
                .into_iter()
                .find(|r| r.repo == repo)
                .unwrap()
                .repo_id
        };
        assert!(s.gh_learn_id("jaehho/a", 1).unwrap());
        // Never overwritten, whatever GitHub says later
        assert!(!s.gh_learn_id("jaehho/a", 2).unwrap());
        assert!(!s.gh_learn_id("jaehho/a", 1).unwrap());
        assert_eq!(id_of("jaehho/a"), Some(1));
        // An id another link holds is not taken
        assert!(!s.gh_learn_id("jaehho/b", 1).unwrap());
        assert_eq!(id_of("jaehho/b"), None);
        assert!(s.gh_learn_id("jaehho/b", 2).unwrap());
        assert!(!s.gh_learn_id("jaehho/nobody", 3).unwrap());
        // The largest id SQLite can hold, and none it cannot
        s.gh_link("jaehho/c", "/cal/c/").unwrap();
        assert!(s.gh_learn_id("jaehho/c", i64::MAX as u64).unwrap());
        assert_eq!(id_of("jaehho/c"), Some(i64::MAX as u64));
        s.gh_link("jaehho/d", "/cal/d/").unwrap();
        assert!(s.gh_learn_id("jaehho/d", u64::MAX).is_err());
        assert_eq!(id_of("jaehho/d"), None);
    }

    #[test]
    fn the_archived_baseline_keeps_when_it_was_first_seen() {
        let s = store();
        s.gh_link("jaehho/a", "/cal/work/").unwrap();
        let archived = || s.gh_link_rows().unwrap()[0].archived;
        assert_eq!(archived(), None);
        s.gh_set_archived("jaehho/a", false, at(10)).unwrap();
        assert_eq!(archived(), Some(0));
        s.gh_set_archived("jaehho/a", true, at(11)).unwrap();
        assert_eq!(archived(), Some(at(11).timestamp()));
        // Seen again later, it has been archived since the first time
        s.gh_set_archived("jaehho/a", true, at(12)).unwrap();
        assert_eq!(archived(), Some(at(11).timestamp()));
        s.gh_set_archived("jaehho/a", false, at(13)).unwrap();
        assert_eq!(archived(), Some(0));
        s.gh_set_archived("jaehho/a", true, at(14)).unwrap();
        assert_eq!(archived(), Some(at(14).timestamp()));
        // A link that is not there takes nothing
        s.gh_set_archived("jaehho/nobody", true, at(14)).unwrap();
        assert_eq!(s.gh_link_rows().unwrap().len(), 1);
    }

    #[test]
    fn the_log_outlives_an_unlink_and_keeps_the_newest_500() {
        let s = store();
        s.gh_link("jaehho/a", "/cal/work/").unwrap();
        let first = s
            .gh_log_add(&new_log("jaehho/a", "list-renaming", "intent"))
            .unwrap();
        s.gh_log_add(&new_log("jaehho/b", "repo-archived", "b"))
            .unwrap();
        s.gh_log_add(&NewLog {
            list: None,
            data: Some(r#"{"from":"x"}"#),
            by: "cli",
            ..new_log("jaehho/a", "list-kept", "kept")
        })
        .unwrap();
        // An unlink takes the link and its pairs, not what was said of it
        assert!(s.gh_unlink("jaehho/a").unwrap());
        let all = s.gh_log_recent(None, 10).unwrap();
        assert_eq!(
            all.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            ["list-kept", "repo-archived", "list-renaming"]
        );
        let mine = s.gh_log_recent(Some("jaehho/a"), 10).unwrap();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].data.as_deref(), Some(r#"{"from":"x"}"#));
        assert_eq!((mine[0].list.clone(), mine[0].by.as_str()), (None, "cli"));
        assert_eq!(s.gh_log_recent(None, 1).unwrap().len(), 1);

        // An intent that was carried out says so
        assert!(s.gh_log_retag(first, "list-renamed", "done").unwrap());
        let row = s
            .gh_log_recent(Some("jaehho/a"), 10)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            (row.kind.as_str(), row.text.as_str()),
            ("list-renamed", "done")
        );
        assert_eq!(row.id, first);
        assert!(!s.gh_log_retag(9999, "x", "y").unwrap());

        // The log is not for ever
        for n in 0..510 {
            s.gh_log_add(&new_log("jaehho/c", "repo-archived", &format!("row {n}")))
                .unwrap();
        }
        let kept = s.gh_log_recent(None, 1000).unwrap();
        assert_eq!(kept.len(), 500);
        assert_eq!(kept[0].text, "row 509");
        assert_eq!(kept[499].text, "row 10");
    }

    #[test]
    fn block_labels_are_read_without_touching_the_database() {
        let dir = Scratch::new("labels");
        // No file: no labels, and none is made
        assert!(Store::label_totals(&dir.db()).unwrap().is_empty());
        assert!(dir.files().is_empty());

        let mut s = Store::open(&dir.db(), ny()).unwrap();
        let hour = 3_600_000;
        let block = |source: &str, label: Option<&str>, from: i64, hours: i64| Block {
            id: None,
            task_uid: None,
            start: from * hour,
            end: (from + hours) * hour,
            label: label.map(str::to_string),
            planned: false,
            source: Some(source.to_string()),
            created_at: 0,
        };
        s.replace_actual_blocks(
            0,
            i64::MAX,
            &[
                block("a", Some("lab"), 0, 2),
                block("b", Some("lab"), 3, 1),
                block("c", Some("champalimaud"), 5, 4),
                block("d", Some("dotfiles"), 10, 1),
                block("e", None, 12, 1),
            ],
        )
        .unwrap();
        // A plan is not time spent
        s.conn
            .execute(
                "INSERT INTO blocks(start, end, label, planned, created_at)
                 VALUES (0, 36000000, 'lab', 1, 0)",
                [],
            )
            .unwrap();
        drop(s);

        let before = std::fs::read(dir.db()).unwrap();
        let total = |label: &str, blocks, hours: i64| LabelTotal {
            label: label.into(),
            blocks,
            seconds: hours * 3600,
        };
        assert_eq!(
            Store::label_totals(&dir.db()).unwrap(),
            [
                total("champalimaud", 1, 4),
                total("lab", 2, 3),
                total("dotfiles", 1, 1)
            ]
        );
        assert_eq!(std::fs::read(dir.db()).unwrap(), before, "not one byte");

        // A database from before blocks and identity is read as it is
        let old = Scratch::new("labels-old");
        old_database(&old.db());
        let before = std::fs::read(old.db()).unwrap();
        assert!(Store::label_totals(&old.db()).unwrap().is_empty());
        assert_eq!(std::fs::read(old.db()).unwrap(), before);
        assert!(
            !identity_backup_path(&old.db()).exists(),
            "nothing migrated"
        );
        let conn = read_only(&old.db());
        assert!(!has_column(&conn, "gh_links", "repo_id").unwrap());
    }

    #[test]
    #[ignore = "needs ASST_TEST_DB, a copy of the live asst.db (sqlite3 asst.db '.backup copy.db')"]
    fn a_copy_of_the_live_database_migrates() {
        let path = PathBuf::from(
            std::env::var("ASST_TEST_DB").expect("ASST_TEST_DB names a copy of asst.db"),
        );
        let copy = read_only(&path);
        let was_old = !has_column(&copy, "gh_links", "repo_id").unwrap();
        let (links, pairs) = (count(&copy, "gh_links"), count(&copy, "gh_pairs"));
        drop(copy);
        println!(
            "before: {links} links, {pairs} pairs, identity columns there: {}",
            !was_old
        );

        let backup = identity_backup_path(&path);
        let had_backup = backup.exists();
        let s = Store::open(&path, ny()).unwrap();
        let rows = s.gh_link_rows().unwrap();
        assert_eq!(rows.len() as i64, links);
        assert_eq!(count(&s.conn, "gh_pairs"), pairs);
        assert!(s.gh_log_recent(None, 10).unwrap().is_empty());
        if was_old {
            assert!(
                rows.iter()
                    .all(|r| r.repo_id.is_none() && r.archived.is_none())
            );
            assert!(backup.exists() || had_backup);
        }
        println!(
            "after: {} links, {pairs} pairs, all ids unknown: {}",
            rows.len(),
            rows.iter().all(|r| r.repo_id.is_none())
        );

        // The second open is a no-op
        let kept = std::fs::read(&backup).ok();
        drop(s);
        let s = Store::open(&path, ny()).unwrap();
        assert_eq!(s.gh_link_rows().unwrap(), rows);
        assert_eq!(count(&s.conn, "gh_pairs"), pairs);
        assert_eq!(std::fs::read(&backup).ok(), kept);
        if !had_backup && was_old {
            let copy = read_only(&backup);
            assert_eq!(
                (count(&copy, "gh_links"), count(&copy, "gh_pairs")),
                (links, pairs)
            );
        }
    }
}
