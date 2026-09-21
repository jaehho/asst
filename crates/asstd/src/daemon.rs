//! The daemon's state and everything it does with it. The D-Bus service,
//! notification buttons, and system events all come through here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use asst_core::api::{
    AddSpec, Added, Change, LinkView, ListChange, ListSpec, ListView, NewNote, Settings,
    SettingsChange, StatusView, SyncState, TaskView, short_ids,
};
use asst_core::caldav::{self, Account, CalDav, RemoteError};
use asst_core::config::{self, AccountConfig, Config};
use asst_core::note_files;
use asst_core::quickadd;
use asst_core::store::{Query, Row, Store, StoreError};
use asst_core::sync::{self, Report, SyncError};
use asst_core::task::{Edit, priority_from_level};
use asst_core::time::{Trigger, When};
use asst_core::todo_md;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use tokio::sync::{Notify, watch};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("this needs a connection to the server")]
    Offline,
}

/// A failed list request, in the words a person needs.
fn remote_error(e: RemoteError) -> Error {
    match e {
        RemoteError::Network(_) => Error::Offline,
        RemoteError::Unauthorized => {
            Error::Invalid("the server rejected the app password; run `asst login`".into())
        }
        RemoteError::Status(403) => Error::Invalid("the server does not allow that".into()),
        RemoteError::Status(405) => {
            Error::Invalid("the server does not support that for this list".into())
        }
        other => Error::Remote(other),
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Daemon {
    store: Mutex<Store>,
    config: RwLock<Config>,
    pub zone: Tz,
    remote: tokio::sync::Mutex<Option<Arc<CalDav>>>,
    status: Mutex<StatusView>,
    wake_sync: Notify,
    /// The sync interval changed: the loop's current sleep is recomputed.
    settings_changed: Notify,
    requested: AtomicU64,
    completed: watch::Sender<(u64, std::result::Result<Report, String>)>,
    /// Reminders recompute when tasks change.
    pub wake_reminders: Notify,
    /// The notes folder setting changed: watch the new one.
    pub notes_dir_changed: Notify,
    /// A linked `TODO.md` settled after changing on disk.
    pub wake_files: Notify,
    /// Tasks changed, here or on the server: rewrite linked `TODO.md`s.
    pub wake_todos: Notify,
    /// The links between directories and lists changed: watch anew.
    pub links_changed: Notify,
    events: tokio::sync::mpsc::UnboundedSender<Event>,
}

/// What the D-Bus side should tell clients.
pub enum Event {
    Changed,
    Status(StatusView),
    LoginDone(bool, String),
}

/// Which side of a linked `TODO.md` changed: the file's pass takes the
/// file's changes to the store, the store's writes the store's changes
/// back. One cause per pass, or the two would undo each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    File,
    Store,
}

impl Daemon {
    pub fn new(
        config: Config,
        store: Store,
        zone: Tz,
    ) -> (Arc<Daemon>, tokio::sync::mpsc::UnboundedReceiver<Event>) {
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let state = if config.account.is_some() {
            SyncState::Idle
        } else {
            SyncState::NoAccount
        };
        let status = StatusView {
            state,
            message: None,
            server: config.account.as_ref().map(|a| a.server.clone()),
            username: config.account.as_ref().map(|a| a.username.clone()),
            last_sync: None,
            pending: store.pending().map(|p| p.len()).unwrap_or(0),
            zone: zone.name().to_string(),
        };
        let daemon = Daemon {
            store: Mutex::new(store),
            config: RwLock::new(config),
            zone,
            remote: tokio::sync::Mutex::new(None),
            status: Mutex::new(status),
            wake_sync: Notify::new(),
            settings_changed: Notify::new(),
            requested: AtomicU64::new(0),
            completed: watch::channel((0, Ok(Report::default()))).0,
            wake_reminders: Notify::new(),
            notes_dir_changed: Notify::new(),
            wake_files: Notify::new(),
            wake_todos: Notify::new(),
            links_changed: Notify::new(),
            events,
        };
        (Arc::new(daemon), rx)
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn config(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn now_local(&self) -> DateTime<Tz> {
        Utc::now().with_timezone(&self.zone)
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    /// Tasks changed here: tell clients, rearm reminders, send soon.
    fn changed_locally(&self) {
        self.emit(Event::Changed);
        self.wake_reminders.notify_one();
        self.wake_sync.notify_one();
        self.wake_todos.notify_one();
        self.refresh_pending();
    }

    fn refresh_pending(&self) {
        let pending = self.store().pending().map(|p| p.len()).unwrap_or(0);
        let mut s = self.status.lock().unwrap_or_else(|p| p.into_inner());
        if s.pending != pending {
            s.pending = pending;
            self.emit(Event::Status(s.clone()));
        }
    }

    fn set_status(&self, state: SyncState, message: Option<String>, synced: bool) {
        let pending = self.store().pending().map(|p| p.len()).unwrap_or(0);
        let cfg = self.config();
        let mut s = self.status.lock().unwrap_or_else(|p| p.into_inner());
        let before = s.clone();
        s.state = state;
        s.message = message;
        if synced {
            s.last_sync = Some(Utc::now());
        }
        s.server = cfg.account.as_ref().map(|a| a.server.clone());
        s.username = cfg.account.as_ref().map(|a| a.username.clone());
        s.pending = pending;
        if (
            before.state,
            &before.message,
            before.pending,
            &before.server,
        ) != (s.state, &s.message, s.pending, &s.server)
        {
            self.emit(Event::Status(s.clone()));
        }
    }

    pub fn status(&self) -> StatusView {
        self.refresh_pending();
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    // -- sync ---------------------------------------------------------------

    pub fn request_sync(&self) {
        self.wake_sync.notify_one();
    }

    /// Sync now and wait for that round (or a later one) to finish.
    pub async fn sync_and_wait(&self) -> std::result::Result<Report, String> {
        let want = self.requested.fetch_add(1, Ordering::SeqCst) + 1;
        let mut rx = self.completed.subscribe();
        self.wake_sync.notify_one();
        match rx.wait_for(|(generation, _)| *generation >= want).await {
            Ok(done) => done.1.clone(),
            Err(_) => Err("the daemon is shutting down".into()),
        }
    }

    pub async fn sync_loop(self: Arc<Self>) {
        loop {
            let generation = self.requested.load(Ordering::SeqCst);
            let result = self.sync_once().await;
            let _ = self.completed.send_replace((generation, result));
            let slept = tokio::time::Instant::now();
            loop {
                let interval = Duration::from_secs(self.config().interval.max(15));
                tokio::select! {
                    _ = tokio::time::sleep_until(slept + interval) => break,
                    _ = self.wake_sync.notified() => {
                        // Let a burst of edits land in one round.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        break;
                    }
                    // A new interval counts from the last round, not from now.
                    _ = self.settings_changed.notified() => {}
                }
            }
        }
    }

    async fn remote(&self) -> std::result::Result<Option<Arc<CalDav>>, String> {
        let mut cached = self.remote.lock().await;
        if let Some(r) = cached.as_ref() {
            return Ok(Some(r.clone()));
        }
        let Some(account) = self.config().account else {
            return Ok(None);
        };
        let password = config::password(&account)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no app password in the keyring; run `asst login`".to_string())?;
        let dav = CalDav::new(
            &Account {
                dav_url: account.dav_url.clone(),
                username: account.username.clone(),
                password,
            },
            &account.home,
        )
        .map_err(|e| e.to_string())?;
        let dav = Arc::new(dav);
        *cached = Some(dav.clone());
        Ok(Some(dav))
    }

    async fn sync_once(&self) -> std::result::Result<Report, String> {
        let remote = match self.remote().await {
            Ok(Some(r)) => r,
            Ok(None) => {
                self.set_status(
                    SyncState::NoAccount,
                    Some("not signed in; run `asst login`".into()),
                    false,
                );
                return Err("not signed in; run `asst login`".into());
            }
            Err(e) => {
                self.set_status(SyncState::Error, Some(e.clone()), false);
                return Err(e);
            }
        };
        self.set_status(SyncState::Syncing, None, false);
        match sync::sync(&self.store, &*remote).await {
            Ok(report) => {
                if report.changed() {
                    self.emit(Event::Changed);
                    self.wake_reminders.notify_one();
                    self.wake_todos.notify_one();
                }
                if report.lists_changed {
                    self.links_changed.notify_one();
                }
                let problems = (!report.problems.is_empty()).then(|| report.problems.join("; "));
                for p in &report.problems {
                    log::warn!("{p}");
                }
                self.set_status(SyncState::Idle, problems, true);
                Ok(report)
            }
            Err(SyncError::Remote(RemoteError::Network(m))) => {
                log::info!("offline: {m}");
                self.set_status(SyncState::Offline, Some(m.clone()), false);
                Err(format!("offline: {m}"))
            }
            Err(SyncError::Remote(RemoteError::Unauthorized)) => {
                let m = "the server rejected the app password; run `asst login`".to_string();
                *self.remote.lock().await = None;
                self.set_status(SyncState::Error, Some(m.clone()), false);
                Err(m)
            }
            Err(e) => {
                log::warn!("sync failed: {e}");
                self.set_status(SyncState::Error, Some(e.to_string()), false);
                Err(e.to_string())
            }
        }
    }

    /// A last push before exiting, so an edit made just now isn't stranded.
    pub async fn flush(&self) {
        if self.store().pending().map(|p| p.is_empty()).unwrap_or(true) {
            return;
        }
        if let Ok(Some(remote)) = self.remote().await {
            let mut report = Report::default();
            let push = sync::push(&self.store, &*remote, &mut report);
            let _ = tokio::time::timeout(Duration::from_secs(5), push).await;
        }
    }

    // -- reads ------------------------------------------------------------------

    fn inbox(&self, store: &Store) -> Option<String> {
        let lists = store.lists().ok()?;
        let writable = || lists.iter().filter(|l| l.writable);
        match self.config().inbox {
            Some(name) => store.find_list(&name).ok().map(|l| l.href),
            None => None,
        }
        .or_else(|| {
            writable()
                .find(|l| l.name.eq_ignore_ascii_case("inbox"))
                .map(|l| l.href.clone())
        })
        .or_else(|| writable().next().map(|l| l.href.clone()))
    }

    pub fn lists(&self) -> Result<Vec<ListView>> {
        let store = self.store();
        let inbox = self.inbox(&store);
        let counts = store.counts()?;
        Ok(store
            .lists()?
            .into_iter()
            .map(|l| {
                let (open, done) = counts.get(&l.href).copied().unwrap_or_default();
                ListView {
                    open,
                    done,
                    inbox: inbox.as_deref() == Some(l.href.as_str()),
                    href: l.href,
                    name: l.name,
                    color: l.color,
                    writable: l.writable,
                }
            })
            .collect())
    }

    fn views(&self, store: &Store, rows: Vec<Row>) -> Result<Vec<TaskView>> {
        let ids = short_ids(&store.uids()?);
        let names: std::collections::HashMap<String, String> = store
            .lists()?
            .into_iter()
            .map(|l| (l.href, l.name))
            .collect();
        Ok(rows
            .into_iter()
            .map(|r| {
                let id = ids
                    .get(&r.task.uid)
                    .cloned()
                    .unwrap_or_else(|| r.task.uid.to_lowercase());
                let list_name = names.get(&r.list).cloned().unwrap_or_default();
                TaskView::new(r, id, list_name)
            })
            .collect())
    }

    fn view(&self, store: &Store, row: Row) -> Result<TaskView> {
        Ok(self.views(store, vec![row])?.remove(0))
    }

    pub fn tasks(&self, query: &Query) -> Result<Vec<TaskView>> {
        let mut query = query.clone();
        if let Some(note) = &query.linked_note {
            query.linked_note = Some(self.note_link(note)?);
        }
        let store = self.store();
        let rows = store.query(&query, self.now_local().date_naive())?;
        self.views(&store, rows)
    }

    /// A linked note as it is stored: its path in the notes folder. A full
    /// path (from the CLI or an editor) has to be a file in that folder.
    fn note_link(&self, given: &str) -> Result<String> {
        let given = given.trim();
        let dir = self.config().notes_dir();
        if Path::new(given).is_absolute() {
            return note_files::relative(&dir, Path::new(given)).ok_or_else(|| {
                Error::Invalid(format!(
                    "{given} isn't in the notes folder, {}",
                    dir.display()
                ))
            });
        }
        note_files::clean(given).ok_or_else(|| Error::Invalid(format!("not a note: {given:?}")))
    }

    /// Links as `AddSpec` and `Change` take them, stored once each.
    fn note_links(&self, given: &[String]) -> Result<Vec<String>> {
        let mut links: Vec<String> = Vec::new();
        for g in given {
            let link = self.note_link(g)?;
            if !links.contains(&link) {
                links.push(link);
            }
        }
        Ok(links)
    }

    pub fn get(&self, id: &str) -> Result<TaskView> {
        let store = self.store();
        let row = store.find(id)?;
        self.view(&store, row)
    }

    pub fn ics(&self, id: &str) -> Result<String> {
        let store = self.store();
        let row = store.find(id)?;
        store
            .ics(&row.href)?
            .ok_or_else(|| Error::Invalid(format!("{id} has no stored object")))
    }

    pub fn parse(&self, text: &str) -> Result<quickadd::Parsed> {
        self.parse_with(text, true)
    }

    fn parse_with(&self, text: &str, dates: bool) -> Result<quickadd::Parsed> {
        let names: Vec<String> = self.store().lists()?.into_iter().map(|l| l.name).collect();
        Ok(quickadd::parse_with(text, self.now_local(), &names, dates))
    }

    // -- writes -----------------------------------------------------------------

    fn when_from_text(&self, text: &str) -> Result<Option<When>> {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        quickadd::parse_when(t, self.now_local())
            .map(Some)
            .ok_or_else(|| Error::Invalid(format!("not a date: {t:?}")))
    }

    /// `every mon`, `daily`, `2 weeks`, an RRULE, or `none`.
    fn rrule_from_text(&self, text: &str) -> Result<Option<String>> {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("none") || t.eq_ignore_ascii_case("never") {
            return Ok(None);
        }
        if t.to_ascii_uppercase().starts_with("FREQ=") {
            asst_core::recur::validate(t).map_err(|e| Error::Invalid(format!("bad RRULE: {e}")))?;
            return Ok(Some(t.to_string()));
        }
        let lower = t.to_lowercase();
        let words = if lower.starts_with("every ")
            || ["daily", "weekly", "monthly", "yearly"].contains(&lower.as_str())
        {
            lower
        } else {
            format!("every {lower}")
        };
        // No list names needed, and no store lock: callers may hold it.
        quickadd::parse(&format!("x {words}"), self.now_local(), &[])
            .rrule
            .map(Some)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "not a repeat: {t:?} (try `every monday` or `every 2 weeks`)"
                ))
            })
    }

    /// The reminder a task with a due time gets unless it has its own: at
    /// that time, as iOS gives one, or the set minutes before it.
    fn due_alarm(&self, due: &Option<When>) -> Option<Trigger> {
        let due = due.as_ref().filter(|d| d.has_time())?;
        let cfg = self.config();
        cfg.alarm_at_due.then(|| match cfg.alarm_before {
            0 => Trigger::Absolute {
                at: due.instant(self.zone),
            },
            // Relative, as quick add's `!30m` is, so it moves with the date.
            minutes => Trigger::Relative {
                offset: -chrono::Duration::minutes(i64::from(minutes)),
                from_due: false,
            },
        })
    }

    pub fn add(&self, spec: &AddSpec) -> Result<Added> {
        let now = Utc::now();
        if let Some(source) = &spec.source {
            let store = self.store();
            if let Some(row) = store.by_source(source)? {
                return Ok(Added {
                    task: self.view(&store, row)?,
                    existed: true,
                });
            }
        }
        let parsed = if spec.parse {
            Some(self.parse_with(&spec.text, !spec.keep_dates)?)
        } else {
            None
        };
        let summary = parsed
            .as_ref()
            .map_or(spec.text.trim(), |p| p.summary.as_str())
            .to_string();
        if summary.is_empty() {
            return Err(Error::Invalid("a task needs a title".into()));
        }
        let due = match (&spec.due, &spec.due_text) {
            (Some(d), _) => Some(d.clone()),
            (None, Some(t)) => self.when_from_text(t)?,
            (None, None) => parsed.as_ref().and_then(|p| p.due.clone()),
        };
        let mut edits = vec![Edit::Summary(summary)];
        if let Some(d) = &due {
            edits.push(Edit::Due(Some(d.clone())));
        }
        if let Some(level) = spec.priority.or(parsed.as_ref().and_then(|p| p.priority)) {
            edits.push(Edit::Priority(priority_from_level(level)));
        }
        if let Some(d) = spec.description.as_ref().filter(|d| !d.trim().is_empty()) {
            edits.push(Edit::Description(Some(d.clone())));
        }
        let repeat = match &spec.repeat_text {
            Some(t) => self.rrule_from_text(t)?,
            None => None,
        };
        if let Some(r) = spec
            .rrule
            .clone()
            .or(repeat)
            .or_else(|| parsed.as_ref().and_then(|p| p.rrule.clone()))
        {
            if due.is_none() {
                return Err(Error::Invalid("a repeating task needs a due date".into()));
            }
            edits.push(Edit::Rrule(Some(r)));
        }
        let alarms = match &spec.alarms {
            Some(a) => a.clone(),
            None => parsed
                .as_ref()
                .and_then(|p| p.alarm.clone())
                .or_else(|| self.due_alarm(&due))
                .into_iter()
                .collect(),
        };
        if !alarms.is_empty() {
            edits.push(Edit::Alarms(alarms));
        }
        if let Some(u) = &spec.url {
            edits.push(Edit::Url(Some(u.clone())));
        }
        if let Some(s) = &spec.source {
            edits.push(Edit::Source(Some(s.clone())));
        }
        let links = self.note_links(&spec.linked_notes)?;
        if !links.is_empty() {
            edits.push(Edit::LinkedNotes(links));
        }
        let mut store = self.store();
        let list = match spec
            .list
            .clone()
            .or_else(|| parsed.as_ref().and_then(|p| p.list.clone()))
        {
            Some(l) => store.find_list(&l)?.href,
            None => self
                .inbox(&store)
                .ok_or_else(|| Error::Invalid("there are no lists yet; sync first".into()))?,
        };
        let row = store.create(&list, &edits, now)?;
        let view = self.view(&store, row)?;
        drop(store);
        self.changed_locally();
        Ok(Added {
            task: view,
            existed: false,
        })
    }

    pub fn edit(&self, id: &str, change: &Change) -> Result<TaskView> {
        let now = Utc::now();
        let repeat = match (&change.rrule, &change.repeat_text) {
            (Some(r), _) => Some(r.clone()),
            (None, Some(t)) => Some(self.rrule_from_text(t)?),
            (None, None) => None,
        };
        // Before the store's lock: this reads the config.
        let links = match &change.linked_notes {
            Some(given) => Some(self.note_links(given)?),
            None => None,
        };
        let mut store = self.store();
        let row = store.find(id)?;
        let mut edits = Vec::new();
        if let Some(s) = &change.summary {
            if s.trim().is_empty() {
                return Err(Error::Invalid("a task needs a title".into()));
            }
            edits.push(Edit::Summary(s.trim().to_string()));
        }
        if let Some(d) = &change.description {
            edits.push(Edit::Description(d.clone()));
        }
        let due = match (&change.due, &change.due_text) {
            (Some(d), _) => Some(d.clone()),
            (None, Some(t)) => Some(self.when_from_text(t)?),
            (None, None) => None,
        };
        if let Some(d) = &due {
            edits.push(Edit::Due(d.clone()));
            if change.alarms.is_none()
                && row.task.alarms.is_empty()
                && let Some(a) = self.due_alarm(d)
            {
                edits.push(Edit::Alarms(vec![a]));
            }
        }
        if let Some(level) = change.priority {
            edits.push(Edit::Priority(priority_from_level(level)));
        }
        if let Some(r) = &repeat {
            let due_after = match &due {
                Some(d) => d.clone(),
                None => row.task.due.clone(),
            };
            if r.is_some() && due_after.is_none() {
                return Err(Error::Invalid("a repeating task needs a due date".into()));
            }
            edits.push(Edit::Rrule(r.clone()));
        }
        if let Some(a) = &change.alarms {
            edits.push(Edit::Alarms(a.clone()));
        }
        if let Some(u) = &change.url {
            edits.push(Edit::Url(u.clone()));
        }
        if let Some(s) = &change.source {
            edits.push(Edit::Source(s.clone()));
        }
        if let Some(o) = change.sort_order {
            edits.push(Edit::SortOrder(o));
        }
        if let Some(links) = links.filter(|l| *l != row.task.linked_notes) {
            edits.push(Edit::LinkedNotes(links));
        }
        let mut row = if edits.is_empty() {
            row
        } else {
            store.edit(&row.href, &edits, now)?
        };
        if let Some(list) = &change.list {
            row = store.move_to(&row.href, list)?;
        }
        let view = self.view(&store, row)?;
        drop(store);
        self.changed_locally();
        Ok(view)
    }

    pub fn complete(&self, id: &str) -> Result<TaskView> {
        let mut store = self.store();
        let row = store.find(id)?;
        let row = store.complete(&row.href, Utc::now())?;
        store.unsnooze(&row.href)?;
        let view = self.view(&store, row)?;
        drop(store);
        self.changed_locally();
        Ok(view)
    }

    pub fn reopen(&self, id: &str) -> Result<TaskView> {
        let mut store = self.store();
        let row = store.find(id)?;
        let row = store.edit(&row.href, &[Edit::Reopen], Utc::now())?;
        let view = self.view(&store, row)?;
        drop(store);
        self.changed_locally();
        Ok(view)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut store = self.store();
        let row = store.find(id)?;
        store.delete(&row.href)?;
        drop(store);
        self.changed_locally();
        Ok(())
    }

    /// Every completed task in a list (an href or a name as `find_list`
    /// takes it), or in all of them. Returns how many went.
    pub fn delete_completed(&self, list: Option<&str>) -> Result<usize> {
        let mut store = self.store();
        let href = match list {
            Some(key) => {
                let l = store.find_list(key)?;
                if !l.writable {
                    return Err(StoreError::ReadOnly(l.name).into());
                }
                Some(l.href)
            }
            None => None,
        };
        let n = store.delete_completed(href.as_deref())?;
        drop(store);
        if n > 0 {
            self.changed_locally();
        }
        Ok(n)
    }

    pub fn duplicate(&self, id: &str) -> Result<TaskView> {
        let mut store = self.store();
        let row = store.find(id)?;
        let row = store.duplicate(&row.href, Utc::now())?;
        let view = self.view(&store, row)?;
        drop(store);
        self.changed_locally();
        Ok(view)
    }

    /// A new note in the notes folder, titled after the task unless `title`
    /// says otherwise, and linked to it.
    pub fn new_note(&self, id: &str, title: &str) -> Result<NewNote> {
        let dir = self.config().notes_dir();
        if !dir.is_dir() {
            return Err(Error::Invalid(format!(
                "the notes folder, {}, doesn't exist; choose another in Preferences or with `asst settings --notes`",
                dir.display()
            )));
        }
        let task = self.get(id)?;
        let title = Some(title.trim())
            .filter(|t| !t.is_empty())
            .unwrap_or(&task.task.summary);
        let path = note_files::create(&dir, title)
            .map_err(|e| Error::Invalid(format!("making a note in {}: {e}", dir.display())))?;
        let file = dir.join(&path);
        let mut links = task.task.linked_notes.clone();
        links.push(path.clone());
        let change = Change {
            linked_notes: Some(links),
            ..Change::default()
        };
        match self.edit(&task.href, &change) {
            Ok(task) => Ok(NewNote {
                path,
                file: file.to_string_lossy().into_owned(),
                task,
            }),
            Err(e) => {
                // Nothing links it (a read-only list): take back the file made just now.
                let _ = std::fs::remove_file(&file);
                Err(e)
            }
        }
    }

    /// Notes, or folders of them, moved within the notes folder (paths in
    /// it, in order): the tasks linking them follow. Returns how many changed.
    pub fn follow_moves(&self, moves: &[(String, String)]) -> Result<usize> {
        let dir = self.config().notes_dir();
        let exists = |path: &str| dir.join(path).exists();
        let now = Utc::now();
        let mut store = self.store();
        let mut changed = 0;
        for row in store.with_linked_notes()? {
            let Some(links) = note_files::follow(&row.task.linked_notes, moves, exists) else {
                continue;
            };
            match store.edit(&row.href, &[Edit::LinkedNotes(links)], now) {
                Ok(_) => changed += 1,
                Err(StoreError::ReadOnly(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        drop(store);
        if changed > 0 {
            self.changed_locally();
        }
        Ok(changed)
    }

    pub fn snooze(&self, href: &str, minutes: u64) -> Result<()> {
        let until = Utc::now() + chrono::Duration::minutes(minutes as i64);
        self.store().snooze(href, until)?;
        self.wake_reminders.notify_one();
        Ok(())
    }

    // -- linked TODO.md files ---------------------------------------------------

    /// The links between project directories and lists, as `asst link`
    /// shows them.
    pub fn links(&self) -> Result<Vec<LinkView>> {
        let store = self.store();
        store
            .links()?
            .into_iter()
            .map(|(dir, href)| {
                let name = store
                    .find_list(&href)
                    .map(|l| l.name)
                    .unwrap_or_else(|_| href.clone());
                Ok(LinkView {
                    dir,
                    list: href,
                    name,
                })
            })
            .collect()
    }

    /// Tie a list to a project directory: its `TODO.md` is synced both
    /// ways. The directory has to be there; `list` is taken as `find_list`
    /// does (an href, a name, an unambiguous prefix).
    pub fn link(&self, list: &str, dir: &str) -> Result<LinkView> {
        let full = Self::project_dir(dir)?;
        let store = self.store();
        let l = store.find_list(list)?;
        if !l.writable {
            return Err(StoreError::ReadOnly(l.name).into());
        }
        store.link(&full, &l.href)?;
        drop(store);
        self.links_changed.notify_one();
        Ok(LinkView {
            dir: full,
            list: l.href,
            name: l.name,
        })
    }

    /// Drop the tie; the file is left as it stands. Returns whether there
    /// was one.
    pub fn unlink(&self, dir: &str) -> Result<bool> {
        let full = Self::project_dir(dir)?;
        let gone = self.store().unlink(&full)?;
        if gone {
            self.links_changed.notify_one();
        }
        Ok(gone)
    }

    /// The directory a link names, canonical so two spellings of one
    /// directory are one link.
    fn project_dir(dir: &str) -> Result<String> {
        let full = std::fs::canonicalize(dir)
            .map_err(|e| Error::Invalid(format!("{}: {e}", Path::new(dir).display())))?;
        if !full.is_dir() {
            return Err(Error::Invalid(format!(
                "{} is not a directory",
                full.display()
            )));
        }
        Ok(full.to_string_lossy().into_owned())
    }

    /// Every linked `TODO.md`, one pass. An error on one doesn't stop the
    /// rest.
    pub fn reconcile_todos(&self, cause: Cause) {
        let links = self.store().links().unwrap_or_default();
        // ponytail: each pass reads every link's tasks; index by list when
        // there are enough links to notice.
        for (dir, list) in links {
            self.reconcile_todo(&dir, &list, cause);
        }
    }

    /// One linked `TODO.md` against its list. The file's pass changes only
    /// the store, the store's only the file, so the two never fight over a
    /// task; each leaves the pair matched, and the echo of its own write
    /// finds nothing to do.
    fn reconcile_todo(&self, dir: &str, list_href: &str, cause: Cause) {
        let path = Path::new(dir).join("TODO.md");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // A missing file is not every line gone; the store's pass makes
            // a new one with what the list holds.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if cause == Cause::Store {
                    String::new()
                } else {
                    return;
                }
            }
            Err(e) => {
                log::warn!("reading {}: {e}", path.display());
                return;
            }
        };
        let lines = todo_md::parse(&text);
        let (edits, changed) = match self.todo_pass(dir, list_href, cause, Utc::now(), &lines) {
            Ok(out) => out,
            Err(e) => {
                log::warn!("syncing {}: {e}", path.display());
                return;
            }
        };
        if !edits.is_empty()
            && let Err(e) =
                todo_md::write_atomic(&path, &todo_md::render(&todo_md::apply(lines, &edits)))
        {
            log::warn!("writing {}: {e}", path.display());
        }
        if changed {
            self.changed_locally();
        }
    }

    /// One direction of the diff, under the store's lock: the edits for
    /// the file, and whether the store changed.
    fn todo_pass(
        &self,
        dir: &str,
        list_href: &str,
        cause: Cause,
        now: DateTime<Utc>,
        lines: &[todo_md::Parsed],
    ) -> Result<(Vec<todo_md::Edit>, bool)> {
        let mut store = self.store();
        let rows = store.list_tasks(list_href)?;
        let refs: Vec<todo_md::Ref> = rows
            .iter()
            .map(|r| todo_md::Ref {
                uid: r.task.uid.clone(),
                title: r.task.summary.clone(),
                open: r.task.is_open(),
            })
            .collect();
        let seen = store.seen(dir)?;
        let by_uid = |uid: &str| rows.iter().find(|r| r.task.uid == uid);
        let mut edits = Vec::new();
        let changed;
        match cause {
            Cause::File => {
                let ops = todo_md::file_ops(lines, &refs, &seen);
                changed = !ops.is_empty();
                for op in ops {
                    match op {
                        todo_md::FileOp::Add {
                            line,
                            title,
                            checked,
                        } => {
                            let row =
                                store.create(list_href, &[Edit::Summary(title.clone())], now)?;
                            if checked {
                                store.complete(&row.href, now)?;
                                store.unsnooze(&row.href)?;
                            }
                            edits.push(todo_md::Edit::Set {
                                line,
                                uid: row.task.uid.clone(),
                                checked,
                                title,
                            });
                        }
                        todo_md::FileOp::Complete { uid } => {
                            let row = by_uid(&uid).expect("file_ops names a task");
                            store.complete(&row.href, now)?;
                            store.unsnooze(&row.href)?;
                        }
                        todo_md::FileOp::Reopen { uid } => {
                            let row = by_uid(&uid).expect("file_ops names a task");
                            store.edit(&row.href, &[Edit::Reopen], now)?;
                        }
                        todo_md::FileOp::Retitle { uid, title } => {
                            let row = by_uid(&uid).expect("file_ops names a task");
                            store.edit(&row.href, &[Edit::Summary(title)], now)?;
                        }
                    }
                }
            }
            Cause::Store => {
                edits = todo_md::store_edits(lines, &refs, &seen);
                changed = false;
            }
        }
        // The uids that have a line after this pass — only tasks', so a
        // foreign marker is never taken for a line gone from here.
        let known: HashSet<&str> = refs.iter().map(|r| r.uid.as_str()).collect();
        let mut present: HashSet<String> = lines
            .iter()
            .filter_map(|p| match &p.line {
                todo_md::Line::Task { uid, .. } => Some(uid.clone()),
                _ => None,
            })
            .filter(|uid| known.contains(uid.as_str()))
            .collect();
        for uid in edits.iter().filter_map(|e| match e {
            todo_md::Edit::Set { uid, .. } | todo_md::Edit::Append { uid, .. } => Some(uid),
            todo_md::Edit::Remove { .. } => None,
        }) {
            present.insert(uid.clone());
        }
        store.see(dir, &present.into_iter().collect::<Vec<_>>())?;
        Ok((edits, changed))
    }

    // -- lists ------------------------------------------------------------------

    /// The server, for changes that can't wait for a sync round.
    async fn connected(&self) -> Result<Arc<CalDav>> {
        match self.remote().await {
            Ok(Some(remote)) => Ok(remote),
            Ok(None) => Err(Error::Invalid("not signed in; run `asst login`".into())),
            Err(e) => Err(Error::Invalid(e)),
        }
    }

    /// After a change to lists on the server: take its listing, tell clients.
    async fn lists_changed(&self, remote: &CalDav) -> Result<()> {
        let lists = remote.lists().await.map_err(remote_error)?;
        self.store().apply_lists(&lists)?;
        self.emit(Event::Changed);
        self.wake_reminders.notify_one();
        self.refresh_pending();
        self.request_sync();
        Ok(())
    }

    fn list_view(&self, href: &str) -> Result<ListView> {
        self.lists()?
            .into_iter()
            .find(|l| l.href == href)
            .ok_or_else(|| Error::Invalid(format!("the server did not list {href} afterwards")))
    }

    pub async fn create_list(&self, spec: &ListSpec) -> Result<ListView> {
        let name = spec.name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("a list needs a name".into()));
        }
        let color = spec.color.as_deref().map(color_arg).transpose()?;
        let remote = self.connected().await?;
        let href = remote
            .create_list(&caldav::list_slug(name), name, color.as_deref())
            .await
            .map_err(remote_error)?;
        self.lists_changed(&remote).await?;
        self.list_view(&href)
    }

    /// `key` is an href, or a name as `find_list` takes it.
    pub async fn update_list(&self, key: &str, change: &ListChange) -> Result<ListView> {
        let list = self.store().find_list(key)?;
        if !list.writable {
            return Err(StoreError::ReadOnly(list.name).into());
        }
        let name = match change.name.as_deref().map(str::trim) {
            Some("") => return Err(Error::Invalid("a list needs a name".into())),
            other => other.filter(|n| *n != list.name),
        };
        let color = match change.color.as_deref().map(color_arg).transpose()? {
            Some(c)
                if list
                    .color
                    .as_deref()
                    .is_some_and(|old| old.eq_ignore_ascii_case(&c)) =>
            {
                None
            }
            other => other,
        };
        if name.is_none() && color.is_none() {
            return self.list_view(&list.href);
        }
        let remote = self.connected().await?;
        remote
            .update_list(&list.href, name, color.as_deref())
            .await
            .map_err(remote_error)?;
        self.lists_changed(&remote).await?;
        self.list_view(&list.href)
    }

    /// Delete a list and its tasks. The server keeps it in its trash bin.
    /// `key` is an href or an exact name: never a prefix, for a delete.
    pub async fn delete_list(&self, key: &str) -> Result<()> {
        let mut matches: Vec<_> = self
            .store()
            .lists()?
            .into_iter()
            .filter(|l| l.href == key || l.name.eq_ignore_ascii_case(key.trim()))
            .collect();
        let list = match matches.len() {
            0 => return Err(StoreError::NoList(key.to_string()).into()),
            1 => matches.remove(0),
            n => {
                return Err(Error::Invalid(format!(
                    "{n} lists are named {key:?}; name one by its href"
                )));
            }
        };
        if !list.writable {
            return Err(StoreError::ReadOnly(list.name).into());
        }
        let remote = self.connected().await?;
        match remote.delete_list(&list.href, false).await {
            Ok(()) | Err(RemoteError::NotFound) => {}
            Err(e) => return Err(remote_error(e)),
        }
        self.store().remove_list(&list.href)?;
        self.links_changed.notify_one();
        if let Err(e) = self.lists_changed(&remote).await {
            // It is gone on the server; the next sync brings the rest.
            log::warn!("reading lists after deleting {}: {e}", list.name);
            self.emit(Event::Changed);
            self.refresh_pending();
        }
        Ok(())
    }

    // -- settings ---------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        settings_of(&self.config())
    }

    pub fn set_settings(&self, change: &SettingsChange) -> Result<Settings> {
        // Resolve the list before taking the config lock: the config is read
        // while the store is locked, so the other order could deadlock.
        let inbox = match &change.inbox {
            None => None,
            Some(key) => match key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
                None => Some(None),
                Some(key) => {
                    let list = self.store().find_list(key)?;
                    if !list.writable {
                        return Err(StoreError::ReadOnly(list.name).into());
                    }
                    Some(Some(list.name))
                }
            },
        };
        let before = self.config();
        let cfg = self.update_config(|cfg| apply_settings(cfg, change, inbox))?;
        if cfg.interval != before.interval {
            self.settings_changed.notify_one();
        }
        if cfg.notes_dir() != before.notes_dir() {
            self.notes_dir_changed.notify_one();
        }
        Ok(settings_of(&cfg))
    }

    /// Change the config and save it, under the lock so concurrent changes
    /// don't undo each other. `f` must not lock the store.
    fn update_config(&self, f: impl FnOnce(&mut Config) -> Result<()>) -> Result<Config> {
        let mut current = self.config.write().unwrap_or_else(|p| p.into_inner());
        let mut cfg = current.clone();
        f(&mut cfg)?;
        cfg.save()?;
        *current = cfg.clone();
        Ok(cfg)
    }

    // -- account ----------------------------------------------------------------

    /// Take a signed-in account: remember it, forget the old one's data.
    pub async fn adopt_account(&self, account: AccountConfig, password: &str) -> Result<()> {
        config::store_password(&account, password).await?;
        let mut switched = false;
        self.update_config(|cfg| {
            switched = cfg
                .account
                .as_ref()
                .is_some_and(|a| a.dav_url != account.dav_url || a.username != account.username);
            cfg.account = Some(account);
            Ok(())
        })?;
        *self.remote.lock().await = None;
        if switched {
            self.reset_store()?;
        }
        self.set_status(SyncState::Idle, None, false);
        self.request_sync();
        Ok(())
    }

    fn reset_store(&self) -> Result<()> {
        let mut store = self.store();
        store.apply_lists(&[])?;
        drop(store);
        self.links_changed.notify_one();
        self.emit(Event::Changed);
        Ok(())
    }

    pub async fn logout(&self) -> Result<()> {
        if let Some(account) = self.config().account
            && let Err(e) = config::forget_password(&account).await
        {
            log::warn!("removing the app password from the keyring: {e}");
        }
        self.update_config(|cfg| {
            cfg.account = None;
            Ok(())
        })?;
        *self.remote.lock().await = None;
        self.reset_store()?;
        self.set_status(SyncState::NoAccount, None, false);
        Ok(())
    }

    pub async fn login_with_password(
        &self,
        server: &str,
        username: &str,
        password: &str,
    ) -> Result<()> {
        let account = asst_core::login::account(server, username, password).await?;
        self.adopt_account(account, password).await
    }

    /// Start the browser login; the result arrives as a LoginDone event.
    pub async fn start_login(self: &Arc<Self>, server: &str) -> Result<String> {
        let flow = asst_core::login::start(server).await?;
        let url = flow.login_url.clone();
        let this = self.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
            let outcome = loop {
                if tokio::time::Instant::now() > deadline {
                    break Err("the login page was not approved within 20 minutes".to_string());
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                match asst_core::login::poll(&flow).await {
                    Ok(None) => continue,
                    Ok(Some(granted)) => {
                        break this
                            .login_with_password(
                                &granted.server,
                                &granted.login_name,
                                &granted.app_password,
                            )
                            .await
                            .map_err(|e| e.to_string());
                    }
                    Err(RemoteError::Network(e)) => log::info!("login poll: {e}"),
                    Err(e) => break Err(e.to_string()),
                }
            };
            match outcome {
                Ok(()) => this.emit(Event::LoginDone(true, String::new())),
                Err(e) => this.emit(Event::LoginDone(false, e)),
            }
        });
        Ok(url)
    }
}

fn color_arg(color: &str) -> Result<String> {
    caldav::parse_color(color)
        .ok_or_else(|| Error::Invalid(format!("not a color: {color:?} (use #rrggbb)")))
}

fn settings_of(cfg: &Config) -> Settings {
    Settings {
        inbox: cfg.inbox.clone(),
        interval: cfg.interval,
        snooze: cfg.snooze.clone(),
        alarm_at_due: cfg.alarm_at_due,
        alarm_before: cfg.alarm_before,
        notes: cfg.notes_dir().to_string_lossy().into_owned(),
    }
}

/// A settings change, checked as a whole before any of it applies. `inbox`
/// is the change's list, already resolved to its name.
fn apply_settings(
    cfg: &mut Config,
    change: &SettingsChange,
    inbox: Option<Option<String>>,
) -> Result<()> {
    if change.interval.is_some_and(|i| i < 15) {
        return Err(Error::Invalid(
            "the sync interval must be at least 15 seconds".into(),
        ));
    }
    let snooze = change.snooze.as_ref().map(|lengths| {
        let mut v = lengths.clone();
        v.sort_unstable();
        v.dedup();
        v
    });
    if let Some(s) = &snooze {
        if s.is_empty() || s.len() > config::MAX_SNOOZE {
            return Err(Error::Invalid(format!(
                "a reminder has 1 to {} Snooze buttons",
                config::MAX_SNOOZE
            )));
        }
        if s.iter().any(|m| !(1..=24 * 60).contains(m)) {
            return Err(Error::Invalid(
                "snooze lengths are 1 minute to a day".into(),
            ));
        }
    }
    if change.alarm_before.is_some_and(|m| m > 7 * 24 * 60) {
        return Err(Error::Invalid(
            "the default reminder rings at most a week early".into(),
        ));
    }
    let notes = change.notes.as_ref().map(|path| {
        path.as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    });
    if let Some(Some(path)) = &notes
        && !(path.is_absolute() || path.starts_with("~"))
    {
        return Err(Error::Invalid(format!(
            "the notes folder needs a full path, not {}",
            path.display()
        )));
    }
    if let Some(inbox) = inbox {
        cfg.inbox = inbox;
    }
    if let Some(i) = change.interval {
        cfg.interval = i;
    }
    if let Some(s) = snooze {
        cfg.snooze = s;
    }
    if let Some(a) = change.alarm_at_due {
        cfg.alarm_at_due = a;
    }
    if let Some(m) = change.alarm_before {
        cfg.alarm_before = m;
    }
    if let Some(n) = notes {
        cfg.notes = n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_setting_changes_nothing() {
        let mut cfg = Config::default();
        let change = SettingsChange {
            interval: Some(10),
            snooze: Some(vec![5]),
            ..SettingsChange::default()
        };
        assert!(apply_settings(&mut cfg, &change, Some(Some("Work".into()))).is_err());
        assert_eq!(cfg, Config::default());
        for bad in [vec![0], vec![], vec![5, 10, 15, 20]] {
            let change = SettingsChange {
                snooze: Some(bad),
                ..SettingsChange::default()
            };
            assert!(apply_settings(&mut cfg, &change, None).is_err());
        }

        let relative = SettingsChange {
            notes: Some(Some("Notes".into())),
            ..SettingsChange::default()
        };
        assert!(apply_settings(&mut cfg, &relative, None).is_err());

        let change = SettingsChange {
            inbox: Some(Some("work".into())),
            interval: Some(300),
            snooze: Some(vec![60, 5, 5]),
            alarm_at_due: Some(false),
            alarm_before: Some(30),
            notes: Some(Some("/srv/notes".into())),
        };
        apply_settings(&mut cfg, &change, Some(Some("Work".into()))).unwrap();
        assert_eq!(
            settings_of(&cfg),
            Settings {
                inbox: Some("Work".into()),
                interval: 300,
                snooze: vec![5, 60],
                alarm_at_due: false,
                alarm_before: 30,
                notes: "/srv/notes".into(),
            }
        );
        apply_settings(&mut cfg, &SettingsChange::default(), Some(None)).unwrap();
        assert_eq!(cfg.inbox, None);
        assert_eq!(cfg.interval, 300, "absent fields stay");
        let back = SettingsChange {
            notes: Some(None),
            ..SettingsChange::default()
        };
        apply_settings(&mut cfg, &back, None).unwrap();
        assert_eq!(cfg.notes_dir(), note_files::default_dir());
    }

    /// The checkbox lines a file holds: (uid, checked, title).
    fn marked(text: &str) -> Vec<(String, bool, String)> {
        todo_md::parse(text)
            .into_iter()
            .filter_map(|p| match p.line {
                todo_md::Line::Task {
                    uid,
                    checked,
                    title,
                } => Some((uid, checked, title)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_linked_todo_md_round_trips() {
        let zone: Tz = "America/New_York".parse().unwrap();
        let mut store = Store::in_memory(zone).unwrap();
        store
            .apply_lists(&[caldav::RemoteList {
                href: "/cal/work/".into(),
                name: "Work".into(),
                color: None,
                order: None,
                sync_token: Some("t1".into()),
                ctag: None,
                writable: true,
            }])
            .unwrap();
        let (daemon, _events) = Daemon::new(Config::default(), store, zone);
        let dir = std::env::temp_dir().join(format!("asst-daemon-todos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("TODO.md");
        let open_tasks = |seen: bool| {
            daemon
                .store()
                .list_tasks("/cal/work/")
                .unwrap()
                .into_iter()
                .filter(|r| r.task.is_open() == seen)
                .map(|r| (r.task.summary.clone(), r.task.uid.clone()))
                .collect::<Vec<_>>()
        };

        daemon.link("work", dir.to_str().unwrap()).unwrap();
        // An empty list has nothing to write: no file is made
        daemon.reconcile_todos(Cause::Store);
        assert!(!file.exists());

        // A new line becomes a task, marked in the file
        std::fs::write(&file, "# Plan\n- [ ] from the file\n").unwrap();
        daemon.reconcile_todos(Cause::File);
        let text = std::fs::read_to_string(&file).unwrap();
        let [(uid, false, title)] = &marked(&text)[..] else {
            panic!("one marked line: {text}");
        };
        assert_eq!(title, "from the file");
        assert_eq!(open_tasks(true), [(title.clone(), uid.clone())]);

        // The pass echoes onto itself: nothing changes
        daemon.reconcile_todos(Cause::File);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        daemon.reconcile_todos(Cause::Store);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        assert_eq!(open_tasks(true).len(), 1);

        // A box checked in the file completes the task
        std::fs::write(&file, format!("- [x] from the file <!-- asst:{uid} -->\n")).unwrap();
        daemon.reconcile_todos(Cause::File);
        assert_eq!(open_tasks(true), vec![]);

        // The task comes back open in the file: the store's pass reopens…
        std::fs::write(&file, format!("- [ ] from the file <!-- asst:{uid} -->\n")).unwrap();
        daemon.reconcile_todos(Cause::File);
        // …and a task made elsewhere is appended under the inbox
        let made = daemon
            .store()
            .create(
                "/cal/work/",
                &[Edit::Summary("from the cli".into())],
                Utc::now(),
            )
            .unwrap();
        daemon.reconcile_todos(Cause::Store);
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(marked(&text).len(), 2);
        assert!(text.contains("## Inbox"));
        assert!(text.contains("from the cli"));

        // Completing it elsewhere flips its box
        {
            let mut s = daemon.store();
            s.complete(&made.href, Utc::now()).unwrap();
        }
        daemon.reconcile_todos(Cause::Store);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            marked(&text)
                .iter()
                .any(|(u, c, _)| *u == made.task.uid && *c)
        );

        // Deleting it removes its line; deleting a line completes its task
        daemon.store().delete(&made.href).unwrap();
        daemon.reconcile_todos(Cause::Store);
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            marked(&text),
            [(uid.clone(), false, "from the file".into())]
        );
        std::fs::write(&file, "# Plan\n").unwrap();
        daemon.reconcile_todos(Cause::File);
        assert_eq!(open_tasks(true), vec![]);

        // An unmarked line that names an open task takes it over
        daemon
            .store()
            .create(
                "/cal/work/",
                &[Edit::Summary("adopt me".into())],
                Utc::now(),
            )
            .unwrap();
        std::fs::write(&file, "# Plan\n- [ ] adopt me\n").unwrap();
        daemon.reconcile_todos(Cause::Store);
        daemon.reconcile_todos(Cause::File);
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(marked(&text)[0].2, "adopt me");
        assert!(
            marked(&text)[0]
                .0
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-')
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
