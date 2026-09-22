//! The SQLite cache: lists and tasks as last seen on the server, plus the
//! edits not yet sent. Every task keeps three things: `base` (the server's
//! copy), `edits` (what asst changed since), and `ics` (the two combined,
//! which is what gets shown and sent).

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, Row as SqlRow, params};
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
CREATE TABLE IF NOT EXISTS gh_links (repo TEXT PRIMARY KEY, list TEXT NOT NULL UNIQUE);
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
";

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

    /// Tie a repo to a list, one repo per list. Tied to another list
    /// before, the repo's pairs go: they name that list's tasks.
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

    pub fn gh_unlink(&self, repo: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM gh_links WHERE repo = ?1", [repo])?
            > 0)
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
        s.create(
            "work",
            &[Edit::Summary("Someday".into())],
            now(),
        )
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
}
