//! One sync round: send what changed here, then take what changed there.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Mutex, MutexGuard};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::caldav::{CalDav, Changes, Fetched, Home, RemoteError};
use crate::store::{ListRow, Op, Pending, Store, StoreError};

/// The server operations sync needs; `CalDav` in the app, a fake in tests.
pub trait Remote: Send + Sync {
    fn collections(&self) -> impl Future<Output = Result<Home, RemoteError>> + Send;
    fn changes(
        &self,
        list: &str,
        token: Option<&str>,
    ) -> impl Future<Output = Result<Changes, RemoteError>> + Send;
    fn fetch(
        &self,
        list: &str,
        hrefs: &[String],
    ) -> impl Future<Output = Result<Vec<Fetched>, RemoteError>> + Send;
    fn create(
        &self,
        href: &str,
        ics: &str,
    ) -> impl Future<Output = Result<Option<String>, RemoteError>> + Send;
    fn update(
        &self,
        href: &str,
        ics: &str,
        etag: &str,
    ) -> impl Future<Output = Result<Option<String>, RemoteError>> + Send;
    fn delete(
        &self,
        href: &str,
        etag: &str,
    ) -> impl Future<Output = Result<(), RemoteError>> + Send;
}

impl Remote for CalDav {
    fn collections(&self) -> impl Future<Output = Result<Home, RemoteError>> + Send {
        CalDav::collections(self)
    }
    fn changes(
        &self,
        list: &str,
        token: Option<&str>,
    ) -> impl Future<Output = Result<Changes, RemoteError>> + Send {
        CalDav::changes(self, list, token)
    }
    fn fetch(
        &self,
        list: &str,
        hrefs: &[String],
    ) -> impl Future<Output = Result<Vec<Fetched>, RemoteError>> + Send {
        CalDav::fetch(self, list, hrefs)
    }
    fn create(
        &self,
        href: &str,
        ics: &str,
    ) -> impl Future<Output = Result<Option<String>, RemoteError>> + Send {
        CalDav::create(self, href, ics)
    }
    fn update(
        &self,
        href: &str,
        ics: &str,
        etag: &str,
    ) -> impl Future<Output = Result<Option<String>, RemoteError>> + Send {
        CalDav::update(self, href, ics, etag)
    }
    fn delete(
        &self,
        href: &str,
        etag: &str,
    ) -> impl Future<Output = Result<(), RemoteError>> + Send {
        CalDav::delete(self, href, etag)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub pushed: usize,
    pub fetched: usize,
    pub removed: usize,
    pub lists_changed: bool,
    /// Items that failed without stopping the round; they are retried next time.
    pub problems: Vec<String>,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.pushed + self.fetched + self.removed > 0 || self.lists_changed
    }
}

fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub async fn sync<R: Remote>(store: &Mutex<Store>, remote: &R) -> Result<Report, SyncError> {
    let mut report = Report::default();
    push(store, remote, &mut report).await?;
    pull(store, remote, &mut report).await?;
    Ok(report)
}

/// Only send local changes (right after an edit, when a full pull can wait).
pub async fn push<R: Remote>(
    store: &Mutex<Store>,
    remote: &R,
    report: &mut Report,
) -> Result<(), SyncError> {
    let pending = lock(store).pending()?;
    for p in pending {
        match push_one(store, remote, &p, true).await {
            Ok(()) => report.pushed += 1,
            Err(SyncError::Remote(e)) if e.is_fatal() => return Err(e.into()),
            Err(e) => report.problems.push(format!("sending {}: {e}", p.href)),
        }
    }
    Ok(())
}

async fn fetch_one<R: Remote>(remote: &R, p: &Pending) -> Result<Option<Fetched>, RemoteError> {
    Ok(remote
        .fetch(&p.list, std::slice::from_ref(&p.href))
        .await?
        .into_iter()
        .next())
}

async fn push_one<R: Remote>(
    store: &Mutex<Store>,
    remote: &R,
    p: &Pending,
    retry: bool,
) -> Result<(), SyncError> {
    match p.op {
        Op::Delete => {
            if let Some(etag) = &p.etag {
                match remote.delete(&p.href, etag).await {
                    Ok(()) | Err(RemoteError::NotFound) => {}
                    // Changed elsewhere since it was read; the delete still stands.
                    Err(RemoteError::Conflict) => {
                        if let Some(current) = fetch_one(remote, p).await? {
                            match remote.delete(&p.href, &current.etag).await {
                                Ok(()) | Err(RemoteError::NotFound) => {}
                                Err(e) => return Err(e.into()),
                            }
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            lock(store).forget(&p.href)?;
        }
        Op::Create | Op::Update => {
            let sent = match (&p.op, &p.etag) {
                (Op::Update, Some(etag)) => remote.update(&p.href, &p.body, etag).await,
                _ => remote.create(&p.href, &p.body).await,
            };
            match sent {
                Ok(Some(etag)) => lock(store).pushed(p, &etag, &p.body, Utc::now())?,
                // Stored, but not byte for byte: the server's version is the base.
                Ok(None) => match fetch_one(remote, p).await? {
                    Some(f) => lock(store).pushed(p, &f.etag, &f.data, Utc::now())?,
                    None => return Err(RemoteError::NotFound.into()),
                },
                Err(RemoteError::Conflict) => {
                    // Written elsewhere first: rebase the edits on that copy.
                    let Some(f) = fetch_one(remote, p).await? else {
                        return Err(RemoteError::NotFound.into());
                    };
                    lock(store).put_server(&p.list, &f.href, &f.etag, &f.data, Utc::now())?;
                    let again = lock(store)
                        .pending()?
                        .into_iter()
                        .find(|q| q.href == p.href);
                    if let (true, Some(q)) = (retry, again) {
                        return Box::pin(push_one(store, remote, &q, false)).await;
                    }
                }
                // Deleted on the server while edited here: the delete wins.
                Err(RemoteError::NotFound) if p.op == Op::Update => lock(store).forget(&p.href)?,
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

pub async fn pull<R: Remote>(
    store: &Mutex<Store>,
    remote: &R,
    report: &mut Report,
) -> Result<(), SyncError> {
    let home = remote.collections().await?;
    let (stale, changed) = lock(store).apply_lists(&home.lists)?;
    report.lists_changed |= changed;
    for list in stale {
        match pull_list(store, remote, &list, report).await {
            Ok(()) => {}
            Err(SyncError::Remote(e)) if e.is_fatal() => return Err(e.into()),
            Err(e) => report.problems.push(format!("reading {}: {e}", list.name)),
        }
    }
    let (stale, changed) = lock(store).apply_calendars(&home.calendars)?;
    report.lists_changed |= changed;
    for calendar in stale {
        match pull_calendar(store, remote, &calendar, report).await {
            Ok(()) => {}
            Err(SyncError::Remote(e)) if e.is_fatal() => return Err(e.into()),
            Err(e) => report
                .problems
                .push(format!("reading {}: {e}", calendar.name)),
        }
    }
    Ok(())
}

/// How long a list goes on sync-token deltas before a full comparison.
/// Nextcloud prunes old change history without invalidating the tokens that
/// point into it, so a long-stale token yields a delta with holes.
const FULL_EVERY: chrono::Duration = chrono::Duration::hours(24);

async fn pull_list<R: Remote>(
    store: &Mutex<Store>,
    remote: &R,
    list: &ListRow,
    report: &mut Report,
) -> Result<(), SyncError> {
    let full_key = format!("full-sync:{}", list.href);
    let changes = read_changes(store, remote, list, &full_key).await?;
    let known = lock(store).etags(&list.href)?;
    let wanted: Vec<String> = changes
        .changed
        .iter()
        .filter(|(href, etag)| known.get(href) != Some(etag))
        .map(|(href, _)| href.clone())
        .collect();
    if !wanted.is_empty() {
        let fetched = remote.fetch(&list.href, &wanted).await?;
        let now = Utc::now();
        let mut s = lock(store);
        for f in &fetched {
            if s.put_server(&list.href, &f.href, &f.etag, &f.data, now)? {
                report.fetched += 1;
            }
        }
    }
    let s = lock(store);
    for href in &changes.removed {
        if known.contains_key(href) {
            s.forget(href)?;
            report.removed += 1;
        }
    }
    if changes.complete {
        let present: HashSet<&String> = changes.changed.iter().map(|(h, _)| h).collect();
        for href in known.keys().filter(|h| !present.contains(h)) {
            s.forget(href)?;
            report.removed += 1;
        }
    }
    s.set_token(&list.href, changes.token.as_deref())?;
    if changes.complete {
        s.set_meta(&full_key, &Utc::now().timestamp().to_string())?;
    }
    Ok(())
}

/// What changed in a collection since its token, or all of it when the last
/// full listing (kept under `full_key`) is too old or the token is refused.
async fn read_changes<R: Remote>(
    store: &Mutex<Store>,
    remote: &R,
    list: &ListRow,
    full_key: &str,
) -> Result<Changes, SyncError> {
    let last_full = lock(store)
        .meta(full_key)?
        .and_then(|t| t.parse::<i64>().ok())
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0));
    let token = list
        .sync_token
        .as_ref()
        .filter(|_| last_full.is_some_and(|t| Utc::now() - t < FULL_EVERY));
    Ok(match token {
        Some(token) => match remote.changes(&list.href, Some(token)).await {
            Err(RemoteError::InvalidToken) => {
                log::info!(
                    "sync token for {} expired; reading the whole list",
                    list.name
                );
                remote.changes(&list.href, None).await?
            }
            other => other?,
        },
        None => remote.changes(&list.href, None).await?,
    })
}

/// `pull_list` for events. Read-only: what the server holds is stored as it
/// is, there are no edits to rebase, and nothing here writes back.
async fn pull_calendar<R: Remote>(
    store: &Mutex<Store>,
    remote: &R,
    calendar: &ListRow,
    report: &mut Report,
) -> Result<(), SyncError> {
    let full_key = format!("full-sync-events:{}", calendar.href);
    let changes = read_changes(store, remote, calendar, &full_key).await?;
    let known = lock(store).event_etags(&calendar.href)?;
    let wanted: Vec<String> = changes
        .changed
        .iter()
        .filter(|(href, etag)| known.get(href) != Some(etag))
        .map(|(href, _)| href.clone())
        .collect();
    if !wanted.is_empty() {
        let fetched = remote.fetch(&calendar.href, &wanted).await?;
        let s = lock(store);
        for f in &fetched {
            if s.put_event(&calendar.href, &f.href, &f.etag, &f.data)? {
                report.fetched += 1;
            }
        }
    }
    let s = lock(store);
    for href in &changes.removed {
        if known.contains_key(href) {
            s.forget_event(href)?;
            report.removed += 1;
        }
    }
    if changes.complete {
        let present: HashSet<&String> = changes.changed.iter().map(|(h, _)| h).collect();
        for href in known.keys().filter(|h| !present.contains(h)) {
            s.forget_event(href)?;
            report.removed += 1;
        }
    }
    s.set_calendar_token(&calendar.href, changes.token.as_deref())?;
    if changes.complete {
        s.set_meta(&full_key, &Utc::now().timestamp().to_string())?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fake {
    //! A CalDAV server in memory, with sync tokens and ETags that behave
    //! like sabre's.

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::caldav::RemoteList;

    #[derive(Default)]
    struct Collection {
        name: String,
        token: u64,
        objects: BTreeMap<String, (String, String)>,
        /// (token, href) for every change, deletions included.
        log: Vec<(u64, String)>,
    }

    #[derive(Default)]
    pub struct Server {
        lists: Mutex<BTreeMap<String, Collection>>,
        /// Hrefs of the collections that hold events, not tasks.
        calendars: Mutex<Vec<String>>,
        etag_counter: Mutex<u64>,
        /// Apply the next write but report a network failure.
        pub lose_next_response: Mutex<bool>,
        pub requests: Mutex<Vec<String>>,
    }

    impl Server {
        pub fn with_list(href: &str, name: &str) -> Server {
            let s = Server::default();
            s.lists.lock().unwrap().insert(
                href.into(),
                Collection {
                    name: name.into(),
                    token: 1,
                    ..Default::default()
                },
            );
            s
        }

        /// A calendar beside the lists: it holds events, so no list shows it.
        pub fn add_calendar(&self, href: &str, name: &str) {
            self.lists.lock().unwrap().insert(
                href.into(),
                Collection {
                    name: name.into(),
                    token: 1,
                    ..Default::default()
                },
            );
            self.calendars.lock().unwrap().push(href.into());
        }

        /// The calendar is gone from the home, as a delete elsewhere leaves it.
        pub fn remove_calendar(&self, href: &str) {
            self.lists.lock().unwrap().remove(href);
            self.calendars.lock().unwrap().retain(|c| c != href);
        }

        fn list_of(href: &str) -> String {
            format!("{}/", href.rsplit_once('/').map_or("", |(l, _)| l))
        }

        /// A write from another client (the phone).
        pub fn put_external(&self, href: &str, data: &str) {
            let etag = self.next_etag();
            let mut lists = self.lists.lock().unwrap();
            let c = lists.get_mut(&Server::list_of(href)).unwrap();
            c.token += 1;
            c.objects.insert(href.into(), (etag, data.into()));
            c.log.push((c.token, href.into()));
        }

        pub fn remove_external(&self, href: &str) {
            let mut lists = self.lists.lock().unwrap();
            let c = lists.get_mut(&Server::list_of(href)).unwrap();
            c.token += 1;
            c.objects.remove(href);
            c.log.push((c.token, href.into()));
        }

        pub fn data(&self, href: &str) -> Option<String> {
            let lists = self.lists.lock().unwrap();
            lists
                .get(&Server::list_of(href))?
                .objects
                .get(href)
                .map(|(_, d)| d.clone())
        }

        fn next_etag(&self) -> String {
            let mut n = self.etag_counter.lock().unwrap();
            *n += 1;
            format!("\"etag-{n}\"")
        }

        fn log(&self, what: String) {
            self.requests.lock().unwrap().push(what);
        }

        fn lose(&self) -> bool {
            std::mem::take(&mut *self.lose_next_response.lock().unwrap())
        }
    }

    impl Remote for Server {
        fn collections(&self) -> impl Future<Output = Result<Home, RemoteError>> + Send {
            self.log("PROPFIND home".into());
            let calendars = self.calendars.lock().unwrap().clone();
            let (events, tasks): (Vec<RemoteList>, Vec<RemoteList>) = self
                .lists
                .lock()
                .unwrap()
                .iter()
                .map(|(href, c)| RemoteList {
                    href: href.clone(),
                    name: c.name.clone(),
                    color: None,
                    order: None,
                    sync_token: Some(format!("sync/{}", c.token)),
                    ctag: None,
                    writable: true,
                })
                .partition(|l| calendars.contains(&l.href));
            async move {
                Ok(Home {
                    lists: tasks,
                    calendars: events,
                })
            }
        }

        fn changes(
            &self,
            list: &str,
            token: Option<&str>,
        ) -> impl Future<Output = Result<Changes, RemoteError>> + Send {
            self.log(format!("REPORT {list} {token:?}"));
            let lists = self.lists.lock().unwrap();
            let c = &lists[list];
            let result = match token
                .map(|t| t.strip_prefix("sync/").and_then(|n| n.parse::<u64>().ok()))
            {
                Some(None) => Err(RemoteError::InvalidToken),
                Some(Some(since)) => {
                    let mut changes = Changes {
                        token: Some(format!("sync/{}", c.token)),
                        ..Default::default()
                    };
                    let touched: std::collections::BTreeSet<&String> = c
                        .log
                        .iter()
                        .filter(|(t, _)| *t > since)
                        .map(|(_, h)| h)
                        .collect();
                    for href in touched {
                        match c.objects.get(href) {
                            Some((etag, _)) => changes.changed.push((href.clone(), etag.clone())),
                            None => changes.removed.push(href.clone()),
                        }
                    }
                    Ok(changes)
                }
                None => Ok(Changes {
                    token: Some(format!("sync/{}", c.token)),
                    changed: c
                        .objects
                        .iter()
                        .map(|(h, (e, _))| (h.clone(), e.clone()))
                        .collect(),
                    removed: Vec::new(),
                    complete: true,
                }),
            };
            async move { result }
        }

        fn fetch(
            &self,
            list: &str,
            hrefs: &[String],
        ) -> impl Future<Output = Result<Vec<Fetched>, RemoteError>> + Send {
            self.log(format!("MULTIGET {}", hrefs.len()));
            let lists = self.lists.lock().unwrap();
            let c = &lists[list];
            let out = hrefs
                .iter()
                .filter_map(|h| {
                    c.objects.get(h).map(|(e, d)| Fetched {
                        href: h.clone(),
                        etag: e.clone(),
                        data: d.clone(),
                    })
                })
                .collect();
            async move { Ok(out) }
        }

        fn create(
            &self,
            href: &str,
            ics: &str,
        ) -> impl Future<Output = Result<Option<String>, RemoteError>> + Send {
            self.log(format!("PUT create {href}"));
            let exists = self.data(href).is_some();
            let result = if exists {
                Err(RemoteError::Conflict)
            } else {
                self.put_external(href, ics);
                let etag = self.lists.lock().unwrap()[&Server::list_of(href)].objects[href]
                    .0
                    .clone();
                if self.lose() {
                    Err(RemoteError::Network("connection reset".into()))
                } else {
                    Ok(Some(etag))
                }
            };
            async move { result }
        }

        fn update(
            &self,
            href: &str,
            ics: &str,
            etag: &str,
        ) -> impl Future<Output = Result<Option<String>, RemoteError>> + Send {
            self.log(format!("PUT update {href}"));
            let current = self.lists.lock().unwrap()[&Server::list_of(href)]
                .objects
                .get(href)
                .map(|(e, _)| e.clone());
            let result = match current {
                None => Err(RemoteError::NotFound),
                Some(e) if e != etag => Err(RemoteError::Conflict),
                Some(_) => {
                    self.put_external(href, ics);
                    let etag = self.lists.lock().unwrap()[&Server::list_of(href)].objects[href]
                        .0
                        .clone();
                    if self.lose() {
                        Err(RemoteError::Network("connection reset".into()))
                    } else {
                        Ok(Some(etag))
                    }
                }
            };
            async move { result }
        }

        fn delete(
            &self,
            href: &str,
            etag: &str,
        ) -> impl Future<Output = Result<(), RemoteError>> + Send {
            self.log(format!("DELETE {href}"));
            let current = self.lists.lock().unwrap()[&Server::list_of(href)]
                .objects
                .get(href)
                .map(|(e, _)| e.clone());
            let result = match current {
                None => Err(RemoteError::NotFound),
                Some(e) if e != etag => Err(RemoteError::Conflict),
                Some(_) => {
                    self.remove_external(href);
                    Ok(())
                }
            };
            async move { result }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use chrono_tz::Tz;

    use super::fake::Server;
    use super::*;
    use crate::ical::Ical;
    use crate::task::{Edit, Task};
    use crate::time::When;

    const LIST: &str = "/cal/inbox/";

    fn phone_task(uid: &str, summary: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//iOS 18.6//EN\r\nBEGIN:VTODO\r\nUID:{uid}\r\n\
SUMMARY:{summary}\r\nDUE;VALUE=DATE:20260914\r\nX-APPLE-SORT-ORDER:7\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        )
    }

    fn setup() -> (Mutex<Store>, Server) {
        let server = Server::with_list(LIST, "Inbox");
        server.put_external("/cal/inbox/A.ics", &phone_task("A", "From the phone"));
        (
            Mutex::new(Store::in_memory("America/New_York".parse::<Tz>().unwrap()).unwrap()),
            server,
        )
    }

    fn row(store: &Mutex<Store>, href: &str) -> crate::store::Row {
        store.lock().unwrap().get(href).unwrap().unwrap()
    }

    #[tokio::test]
    async fn first_sync_reads_everything_and_the_next_one_reads_nothing() {
        let (store, server) = setup();
        let r = sync(&store, &server).await.unwrap();
        assert_eq!((r.fetched, r.pushed), (1, 0));
        assert_eq!(
            row(&store, "/cal/inbox/A.ics").task.summary,
            "From the phone"
        );

        server.requests.lock().unwrap().clear();
        let r = sync(&store, &server).await.unwrap();
        assert!(!r.changed());
        assert_eq!(
            *server.requests.lock().unwrap(),
            vec!["PROPFIND home".to_string()],
            "one request when idle"
        );
    }

    #[tokio::test]
    async fn edits_on_both_sides_merge() {
        let (store, server) = setup();
        sync(&store, &server).await.unwrap();
        // Here: priority. There (first): the title.
        store
            .lock()
            .unwrap()
            .edit("/cal/inbox/A.ics", &[Edit::Priority(1)], Utc::now())
            .unwrap();
        server.put_external("/cal/inbox/A.ics", &phone_task("A", "Renamed on the phone"));

        let r = sync(&store, &server).await.unwrap();
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        let on_server =
            Task::from_ical(&Ical::parse(&server.data("/cal/inbox/A.ics").unwrap())).unwrap();
        assert_eq!(on_server.summary, "Renamed on the phone");
        assert_eq!(on_server.priority, 1);
        assert!(
            server
                .data("/cal/inbox/A.ics")
                .unwrap()
                .contains("X-APPLE-SORT-ORDER:7")
        );
        let local = row(&store, "/cal/inbox/A.ics");
        assert!(!local.pending);
        assert_eq!(local.task, on_server);
    }

    #[tokio::test]
    async fn creates_and_deletes_travel_both_ways() {
        let (store, server) = setup();
        sync(&store, &server).await.unwrap();
        let new = store
            .lock()
            .unwrap()
            .create("Inbox", &[Edit::Summary("Made here".into())], Utc::now())
            .unwrap();
        sync(&store, &server).await.unwrap();
        assert!(
            server
                .data(&new.href)
                .unwrap()
                .contains("SUMMARY:Made here")
        );
        assert!(!row(&store, &new.href).pending);

        server.remove_external("/cal/inbox/A.ics");
        store.lock().unwrap().delete(&new.href).unwrap();
        let r = sync(&store, &server).await.unwrap();
        assert_eq!((r.pushed, r.removed), (1, 1));
        assert!(server.data(&new.href).is_none());
        assert!(
            store
                .lock()
                .unwrap()
                .get("/cal/inbox/A.ics")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_edit_to_a_task_deleted_elsewhere_is_dropped() {
        let (store, server) = setup();
        sync(&store, &server).await.unwrap();
        store
            .lock()
            .unwrap()
            .edit(
                "/cal/inbox/A.ics",
                &[Edit::Summary("too late".into())],
                Utc::now(),
            )
            .unwrap();
        server.remove_external("/cal/inbox/A.ics");
        let r = sync(&store, &server).await.unwrap();
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert!(
            store
                .lock()
                .unwrap()
                .get("/cal/inbox/A.ics")
                .unwrap()
                .is_none()
        );
        assert!(store.lock().unwrap().pending().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_lost_response_is_not_applied_twice() {
        let (store, server) = setup();
        let daily = phone_task("A", "Stretch").replace(
            "X-APPLE-SORT-ORDER:7\r\n",
            "X-APPLE-SORT-ORDER:7\r\nRRULE:FREQ=DAILY\r\n",
        );
        server.put_external("/cal/inbox/A.ics", &daily);
        sync(&store, &server).await.unwrap();

        let done_at = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 9, 14, 16, 0, 0).unwrap();
        store
            .lock()
            .unwrap()
            .edit("/cal/inbox/A.ics", &[Edit::Complete(done_at)], done_at)
            .unwrap();
        *server.lose_next_response.lock().unwrap() = true;
        assert!(matches!(
            sync(&store, &server).await,
            Err(SyncError::Remote(RemoteError::Network(_)))
        ));

        let r = sync(&store, &server).await.unwrap();
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        let task = row(&store, "/cal/inbox/A.ics").task;
        assert_eq!(
            task.due,
            Some(When::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
            })
        );
        assert!(!row(&store, "/cal/inbox/A.ics").pending);
    }

    #[tokio::test]
    async fn an_expired_token_falls_back_to_a_full_read() {
        let (store, server) = setup();
        sync(&store, &server).await.unwrap();
        store
            .lock()
            .unwrap()
            .set_token(LIST, Some("garbage"))
            .unwrap();
        server.put_external("/cal/inbox/B.ics", &phone_task("B", "Second"));
        let r = sync(&store, &server).await.unwrap();
        assert_eq!(r.fetched, 1);
        assert!(
            store
                .lock()
                .unwrap()
                .get("/cal/inbox/B.ics")
                .unwrap()
                .is_some()
        );
    }

    const CALENDAR: &str = "/cal/personal/";
    const MEETING: &str = "/cal/personal/M.ics";

    fn phone_event(summary: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//iOS 18.6//EN\r\nBEGIN:VEVENT\r\nUID:M\r\n\
DTSTART:20260914T140000Z\r\nDTEND:20260914T150000Z\r\nSUMMARY:{summary}\r\nX-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    fn sept(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, day).unwrap()
    }

    fn summaries(store: &Mutex<Store>, day: NaiveDate) -> Vec<String> {
        let events = store.lock().unwrap().events_on(day).unwrap();
        events.into_iter().map(|e| e.summary).collect()
    }

    #[tokio::test]
    async fn events_come_down_their_own_path_and_nothing_goes_back() {
        let (store, server) = setup();
        server.add_calendar(CALENDAR, "Personal");
        server.put_external(MEETING, &phone_event("Dentist"));

        let r = sync(&store, &server).await.unwrap();
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(r.fetched, 2, "the task and the event");
        assert_eq!(summaries(&store, sept(14)), ["Dentist"]);
        assert!(summaries(&store, sept(15)).is_empty());
        // Task lists stay VTODO: the calendar is no list, its event no task.
        let lists = store.lock().unwrap().lists().unwrap();
        assert_eq!(lists.len(), 1);
        assert!(store.lock().unwrap().get(MEETING).unwrap().is_none());

        // The stored copy is the server's, byte for byte.
        assert_eq!(
            store.lock().unwrap().event_ics(MEETING).unwrap().as_deref(),
            server.data(MEETING).as_deref()
        );

        // Idle costs the one PROPFIND it always did.
        server.requests.lock().unwrap().clear();
        let r = sync(&store, &server).await.unwrap();
        assert!(!r.changed());
        assert_eq!(*server.requests.lock().unwrap(), ["PROPFIND home"]);

        // A change on the phone arrives; a removal leaves.
        server.put_external(MEETING, &phone_event("Dentist moved"));
        assert_eq!(sync(&store, &server).await.unwrap().fetched, 1);
        assert_eq!(summaries(&store, sept(14)), ["Dentist moved"]);
        server.remove_external(MEETING);
        assert_eq!(sync(&store, &server).await.unwrap().removed, 1);
        assert!(summaries(&store, sept(14)).is_empty());

        let writes = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("PUT") || r.starts_with("DELETE"))
            .count();
        assert_eq!(writes, 0, "events are only read");
    }

    #[tokio::test]
    async fn a_calendar_gone_from_the_server_takes_its_events() {
        let (store, server) = setup();
        server.add_calendar(CALENDAR, "Personal");
        server.put_external(MEETING, &phone_event("Dentist"));
        sync(&store, &server).await.unwrap();
        assert_eq!(summaries(&store, sept(14)).len(), 1);

        server.remove_calendar(CALENDAR);
        let r = sync(&store, &server).await.unwrap();
        assert!(r.lists_changed);
        assert!(summaries(&store, sept(14)).is_empty());
        assert_eq!(row(&store, "/cal/inbox/A.ics").task.uid, "A", "tasks stay");
    }

    #[tokio::test]
    async fn a_collection_of_both_keeps_tasks_and_events_apart() {
        let store =
            Mutex::new(Store::in_memory("America/New_York".parse::<Tz>().unwrap()).unwrap());
        let server = Server::with_list(LIST, "Both");
        server.put_external("/cal/inbox/A.ics", &phone_task("A", "A task"));
        server.put_external("/cal/inbox/M.ics", &phone_event("A meeting"));
        // Listed twice, as a home with no component set is.
        struct Twice(Server);
        impl Remote for Twice {
            async fn collections(&self) -> Result<Home, RemoteError> {
                let mut home = self.0.collections().await?;
                home.calendars = home.lists.clone();
                Ok(home)
            }
            async fn changes(&self, l: &str, t: Option<&str>) -> Result<Changes, RemoteError> {
                self.0.changes(l, t).await
            }
            async fn fetch(&self, l: &str, h: &[String]) -> Result<Vec<Fetched>, RemoteError> {
                self.0.fetch(l, h).await
            }
            async fn create(&self, h: &str, i: &str) -> Result<Option<String>, RemoteError> {
                self.0.create(h, i).await
            }
            async fn update(
                &self,
                h: &str,
                i: &str,
                e: &str,
            ) -> Result<Option<String>, RemoteError> {
                self.0.update(h, i, e).await
            }
            async fn delete(&self, h: &str, e: &str) -> Result<(), RemoteError> {
                self.0.delete(h, e).await
            }
        }
        let r = sync(&store, &Twice(server)).await.unwrap();
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(summaries(&store, sept(14)), ["A meeting"]);
        assert!(
            store
                .lock()
                .unwrap()
                .get("/cal/inbox/M.ics")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .lock()
                .unwrap()
                .get("/cal/inbox/A.ics")
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .lock()
                .unwrap()
                .event_ics("/cal/inbox/A.ics")
                .unwrap()
                .is_none()
        );
    }
}
