//! A list linked to a GitHub repo: each open issue is a task, each open task
//! an issue. Which issue is which task is kept in the store, with the title
//! and state both sides last agreed on; a pass compares each side with that
//! base, so it works the same whichever side changed, and a push that
//! failed is simply found again next time. `plan` decides, the daemon does.
//! `judge` and `follow_list_name` decide the same way about the repo itself:
//! whether it is still the one linked, and whether its list follows a rename.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use http::{Method, Request, StatusCode};
use serde::Deserialize;

use crate::caldav::{self, RemoteError};

/// An issue or a task as the sync sees it: `id` is the issue's number or the
/// task's uid. A task's `body` is its description and its `labels` are empty;
/// an issue's `priority` is read off its `P1`–`P3` labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item<Id> {
    pub id: Id,
    pub title: String,
    pub open: bool,
    pub body: Option<String>,
    pub priority: u8,
    pub labels: Vec<String>,
}

pub type Issue = Item<u32>;
pub type Ref = Item<String>;

/// The issue label of a raw priority; "none" has none.
fn label_of(priority: u8) -> Option<&'static str> {
    match priority {
        1 => Some("P1"),
        5 => Some("P2"),
        9 => Some("P3"),
        _ => None,
    }
}

/// The raw priority an issue's labels carry; the strongest wins if several do.
fn priority_of(labels: &[String]) -> u8 {
    for (p, label) in [(1, "P1"), (5, "P2"), (9, "P3")] {
        if labels.iter().any(|l| l == label) {
            return p;
        }
    }
    0
}

/// The labels an issue should carry for `priority`: its other labels kept,
/// the P1–P3 ones replaced.
fn labels_for(labels: &[String], priority: u8) -> Vec<String> {
    let mut out: Vec<String> = labels
        .iter()
        .filter(|l| !matches!(l.as_str(), "P1" | "P2" | "P3"))
        .cloned()
        .collect();
    if let Some(p) = label_of(priority) {
        out.push(p.to_string());
    }
    out
}

/// An issue and its task, with what the two last agreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub number: u32,
    pub uid: String,
    pub title: String,
    pub open: bool,
    pub body: Option<String>,
    pub priority: u8,
}

/// What to change on one side; `None` leaves that field. The issue's labels
/// are given whole, its other labels already kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub title: Option<String>,
    pub open: Option<bool>,
    pub body: Option<Option<String>>,
    pub priority: Option<u8>,
    pub labels: Option<Vec<String>>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.open.is_none()
            && self.body.is_none()
            && self.priority.is_none()
            && self.labels.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A pair where a side moved: change the task and the issue, then
    /// remember the agreed fields.
    Merge {
        number: u32,
        uid: String,
        task: Change,
        issue: Change,
        title: String,
        open: bool,
        body: Option<String>,
        priority: u8,
    },
    /// The task left the list: close the issue as not planned, unpair.
    CloseIssue { number: u32 },
    /// The issue left GitHub (deleted, transferred): complete the task,
    /// unpair.
    CompleteTask { number: u32, uid: String },
    /// Both sides are done with each other; nothing to change.
    Unpair { number: u32 },
    /// An open issue and an open task with the same title: pair them.
    Adopt {
        number: u32,
        uid: String,
        title: String,
        body: Option<String>,
        priority: u8,
    },
    /// An open issue with no task: make one, pair them.
    NewTask {
        number: u32,
        title: String,
        body: Option<String>,
        priority: u8,
    },
    /// An open task with no issue: open one, pair them.
    NewIssue {
        uid: String,
        title: String,
        body: Option<String>,
        priority: u8,
    },
}

/// Everything one pass changes, from every issue of the repo (a full
/// listing, pull requests left out), every task of the list, and the pairs.
pub fn plan(issues: &[Issue], tasks: &[Ref], pairs: &[Pair]) -> Vec<Op> {
    let mut ops = Vec::new();
    let paired_numbers: HashSet<u32> = pairs.iter().map(|p| p.number).collect();
    let paired_uids: HashSet<&str> = pairs.iter().map(|p| p.uid.as_str()).collect();
    for p in pairs {
        let issue = issues.iter().find(|i| i.id == p.number);
        let task = tasks.iter().find(|t| t.id == p.uid);
        let op = match (issue, task) {
            (Some(i), Some(t)) => merge(p, i, t),
            (Some(i), None) if i.open => Some(Op::CloseIssue { number: p.number }),
            (None, Some(t)) if t.open => Some(Op::CompleteTask {
                number: p.number,
                uid: p.uid.clone(),
            }),
            (Some(_), None) | (None, _) => Some(Op::Unpair { number: p.number }),
        };
        ops.extend(op);
    }
    let mut free: Vec<&Ref> = tasks
        .iter()
        .filter(|t| t.open && !paired_uids.contains(t.id.as_str()))
        .collect();
    for i in issues
        .iter()
        .filter(|i| i.open && !paired_numbers.contains(&i.id))
    {
        match free.iter().position(|t| t.title == i.title) {
            Some(at) => ops.push(Op::Adopt {
                number: i.id,
                uid: free.remove(at).id.clone(),
                title: i.title.clone(),
                body: i.body.clone(),
                priority: i.priority,
            }),
            None => ops.push(Op::NewTask {
                number: i.id,
                title: i.title.clone(),
                body: i.body.clone(),
                priority: i.priority,
            }),
        }
    }
    ops.extend(free.into_iter().map(|t| Op::NewIssue {
        uid: t.id.clone(),
        title: t.title.clone(),
        body: t.body.clone(),
        priority: t.priority,
    }));
    ops
}

/// A pair's three-way merge, field by field: a side that moved from the
/// base wins, the issue when both did. `None` when nothing moved.
fn merge(p: &Pair, i: &Issue, t: &Ref) -> Option<Op> {
    fn pick<T: Clone + PartialEq>(base: &T, issue: &T, task: &T) -> T {
        if issue != base { issue } else { task }.clone()
    }
    fn side<Id>(
        now: &Item<Id>,
        title: &str,
        open: bool,
        body: &Option<String>,
        priority: u8,
    ) -> Change {
        Change {
            title: (now.title.as_str() != title).then(|| title.to_string()),
            open: (now.open != open).then_some(open),
            body: (now.body != *body).then(|| body.clone()),
            priority: (now.priority != priority).then_some(priority),
            labels: None,
        }
    }
    let title = pick(&p.title, &i.title, &t.title);
    let open = pick(&p.open, &i.open, &t.open);
    let body = pick(&p.body, &i.body, &t.body);
    let priority = pick(&p.priority, &i.priority, &t.priority);
    let task = side(t, &title, open, &body, priority);
    let mut issue = side(i, &title, open, &body, priority);
    if i.priority != priority {
        issue.labels = Some(labels_for(&i.labels, priority));
    }
    let agreed = title != p.title || open != p.open || body != p.body || priority != p.priority;
    (!task.is_empty() || !issue.is_empty() || agreed).then(|| Op::Merge {
        number: p.number,
        uid: p.uid.clone(),
        task,
        issue,
        title,
        open,
        body,
        priority,
    })
}

/// What one look at a repo means for its link, given what is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Still the repo that is linked, nothing changed.
    Fine,
    /// Nothing is stored for this link yet and the name found the repo:
    /// remember its id (and its archived flag, without a log row).
    LearnId(u64),
    /// The repo is called `to` now. `witnessed` is that the link had an id
    /// and the record under it changed: this is an event. Without an id the
    /// old name merely redirects, the move is found after the fact, and the
    /// caller learns the record's id as it moves the link.
    Renamed { to: String, witnessed: bool },
    /// Archived or disabled since the last look.
    Archived,
    /// No longer archived or disabled.
    Unarchived,
    /// The name belongs to another repo (`found` is its id) than the one
    /// whose id is stored: deleted and made again, or the name was reused.
    /// Nothing on that repo is the linked project's.
    Replaced { found: u64 },
    /// The look settles nothing: no finding, no change.
    Unknown,
}

/// Judge one look at the repo linked as `linked`, whose stored id (if any)
/// and archived baseline (`None`: never observed) are given. One finding
/// per look, the one that matters most: another repo, then a new name, then
/// the archived flag.
///
/// A redirect names only an id, not the repo's new name, so a `Moved` alone
/// is not enough to rename by. The caller follows it with a look by that id
/// and judges the record, which is the "301 then record" case: with an id
/// stored, a `Moved` to another one is `Replaced` all the same.
pub fn judge(
    stored_id: Option<u64>,
    linked: &str,
    archived: Option<bool>,
    seen: &Observed,
) -> Verdict {
    let info = match seen {
        Observed::Record(info, _) => info,
        // The record behind the ETag is the one already judged.
        Observed::Unchanged if stored_id.is_some() => return Verdict::Fine,
        Observed::Moved { id } => {
            return match stored_id {
                Some(stored) if stored != *id => Verdict::Replaced { found: *id },
                _ => Verdict::Unknown,
            };
        }
        Observed::Unchanged | Observed::Missing | Observed::Denied { .. } | Observed::Failed => {
            return Verdict::Unknown;
        }
    };
    if let Some(stored) = stored_id
        && stored != info.id
    {
        return Verdict::Replaced { found: info.id };
    }
    if !info.full_name.eq_ignore_ascii_case(linked) {
        return Verdict::Renamed {
            to: info.full_name.clone(),
            witnessed: stored_id.is_some(),
        };
    }
    if stored_id.is_none() {
        return Verdict::LearnId(info.id);
    }
    let retired = info.archived || info.disabled;
    match archived {
        Some(false) if retired => Verdict::Archived,
        Some(true) if !retired => Verdict::Unarchived,
        // No baseline yet: first sight, stored without a finding.
        _ => Verdict::Fine,
    }
}

/// Whether a renamed repo renames its list too (`follow_rename` in
/// `config.toml`): `List` only for a list named exactly for the old repo,
/// `Off` reports it and leaves the list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FollowRename {
    #[default]
    List,
    Off,
}

/// Why a list keeps its name when its repo is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepWhy {
    /// The list is not named exactly for the old repo: someone named it.
    Custom,
    /// The list already has the new repo's name.
    AlreadyNamed,
    /// Only the owner changed; the repo's name, and so the list's, did not.
    SameName,
    /// Another list has the new name (in any case).
    Clash,
    /// A shared list the user cannot write.
    ReadOnly,
    /// `config.inbox` is a list name; renaming it would lose the inbox.
    Inbox,
    /// `follow_rename = "off"`.
    Off,
    /// The new repo name has no name part to give a list.
    Empty,
}

/// The name part of `owner/repo`.
fn short(repo: &str) -> &str {
    repo.split_once('/').map_or(repo, |(_, name)| name)
}

/// The name the list takes when its repo `old` is now `new`, or why it
/// keeps its own. The list follows only if it is named exactly (byte for
/// byte) for the old repo; `other_names` are the other lists' names, and
/// the new name may not be any of them in any case, since Nextcloud lets
/// two lists share one. The first reason that applies is the one given:
/// `Off`, `Empty` and `SameName` (nothing to do or to say), then
/// `AlreadyNamed` and `Custom` (the list is not the repo's to rename), then
/// `Inbox`, `ReadOnly` and `Clash` (it is, but cannot be renamed).
pub fn follow_list_name(
    list_name: &str,
    old: &str,
    new: &str,
    other_names: &[impl AsRef<str>],
    is_inbox: bool,
    writable: bool,
    setting: FollowRename,
) -> Result<String, KeepWhy> {
    let (old, new) = (short(old), short(new));
    if setting == FollowRename::Off {
        return Err(KeepWhy::Off);
    }
    if old.is_empty() || new.is_empty() {
        return Err(KeepWhy::Empty);
    }
    if old.eq_ignore_ascii_case(new) {
        return Err(KeepWhy::SameName);
    }
    if list_name.to_lowercase() == new.to_lowercase() {
        return Err(KeepWhy::AlreadyNamed);
    }
    if list_name != old {
        return Err(KeepWhy::Custom);
    }
    if is_inbox {
        return Err(KeepWhy::Inbox);
    }
    if !writable {
        return Err(KeepWhy::ReadOnly);
    }
    if other_names
        .iter()
        .any(|n| n.as_ref().to_lowercase() == new.to_lowercase())
    {
        return Err(KeepWhy::Clash);
    }
    Ok(new.to_string())
}

/// `owner/repo`, as GitHub allows the two names.
pub fn valid_repo(repo: &str) -> bool {
    let name = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split_once('/'), Some((owner, name_)) if name(owner) && name(name_))
}

// ---------------------------------------------------------------------------
// The REST API.

const API: &str = "https://api.github.com";

pub struct GitHub {
    auth: HeaderValue,
    base: String,
}

/// A repository, named or numbered: the name changes with a rename, the id
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    Name(&'a str),
    Id(u64),
}

/// What a repository's record says, as far as asst reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub id: u64,
    pub full_name: String,
    pub archived: bool,
    pub disabled: bool,
}

/// One look at a repository: what GitHub answered, not what it means (that
/// is `judge`'s). An answer that settles nothing (a refusal, an error page, a
/// body that is not a record) stays one of the last three, never a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// 200, with the record and its ETag.
    Record(RepoInfo, Option<String>),
    /// 304: the record is as it was when the ETag was given.
    Unchanged,
    /// 301 to `/repositories/<id>`: the name belongs to that repository now.
    Moved { id: u64 },
    /// 404 or 410. GitHub also says 404 to a private repo the token cannot
    /// see, so this is never evidence that the repository is gone.
    Missing,
    /// 403 or 429. `retry` is how long GitHub asked to wait, if it did: the
    /// `Retry-After` header, else until the window resets when none is left.
    Denied { retry: Option<Duration> },
    /// 5xx or an answer that is not a record. A dropped connection or a
    /// timeout is an `Err`, not this.
    Failed,
}

#[derive(Deserialize)]
struct RawRepo {
    id: u64,
    full_name: String,
    // Both are required: a record without them says nothing about either,
    // and a missing flag read as `false` would look like an un-archive.
    archived: bool,
    disabled: bool,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawIssue {
    number: u32,
    title: String,
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

impl RawIssue {
    fn issue(self) -> Issue {
        let labels: Vec<String> = self.labels.into_iter().map(|l| l.name).collect();
        Issue {
            id: self.number,
            title: self.title,
            open: self.state == "open",
            // An empty body is no body, or the merge would chase its tail.
            body: self.body.filter(|b| !b.trim().is_empty()),
            priority: priority_of(&labels),
            labels,
        }
    }
}

impl GitHub {
    pub fn new(token: &str) -> Result<GitHub, RemoteError> {
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| RemoteError::Protocol("the GitHub token has invalid characters".into()))?;
        auth.set_sensitive(true);
        Ok(GitHub {
            auth,
            base: API.into(),
        })
    }

    /// A client for another server, e.g. `testing::FakeGitHub`. Only tests
    /// have it: nothing the daemon runs can send the token elsewhere.
    #[cfg(any(test, feature = "testing"))]
    pub fn with_base(token: &str, base: &str) -> Result<GitHub, RemoteError> {
        Ok(GitHub {
            base: base.trim_end_matches('/').into(),
            ..GitHub::new(token)?
        })
    }

    // ponytail: each request builds its own client (and reads the system
    // certificates); keep one if polling many repos gets slow.
    async fn call(
        &self,
        method: Method,
        url: &str,
        body: Option<serde_json::Value>,
        etag: Option<&str>,
    ) -> Result<(StatusCode, http::HeaderMap, Vec<u8>), RemoteError> {
        let out = self.raw(method, url, body, etag).await?;
        match out.0 {
            s if s.is_success() || s == StatusCode::NOT_MODIFIED => Ok(out),
            s => Err(caldav::status_error(s)),
        }
    }

    /// `call` without judging the status.
    async fn raw(
        &self,
        method: Method,
        url: &str,
        body: Option<serde_json::Value>,
        etag: Option<&str>,
    ) -> Result<(StatusCode, http::HeaderMap, Vec<u8>), RemoteError> {
        let mut req = Request::builder()
            .method(method)
            .uri(url)
            .header(AUTHORIZATION, self.auth.clone())
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(e) = etag {
            req = req.header("If-None-Match", e);
        }
        let req = req
            .body(body.map(|b| b.to_string()).unwrap_or_default())
            .map_err(|e| RemoteError::Protocol(e.to_string()))?;
        caldav::send(req).await
    }

    /// One look at a repository: by name, or by the id a rename leaves
    /// alone. With the `etag` of an earlier record, GitHub answers 304
    /// while it is unchanged, which it does not count against the rate
    /// limit. A dropped connection, a timeout and a rejected token are
    /// errors, as for every call here; every other answer is an `Observed`.
    pub async fn repo(
        &self,
        target: Target<'_>,
        etag: Option<&str>,
    ) -> Result<Observed, RemoteError> {
        let url = match target {
            Target::Name(name) => format!("{}/repos/{name}", self.base),
            Target::Id(id) => format!("{}/repositories/{id}", self.base),
        };
        let (status, headers, body) = self.raw(Method::GET, &url, None, etag).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        observe(status, &headers, &body, now)
    }

    /// Where `repo` is called now, if it was renamed or moved to another
    /// owner. GitHub answers the old name with a redirect to the
    /// repository's id, and the id's record has the name.
    pub async fn renamed(&self, repo: &str) -> Result<Option<String>, RemoteError> {
        let Observed::Moved { id } = self.repo(Target::Name(repo), None).await? else {
            return Ok(None);
        };
        match self.repo(Target::Id(id), None).await? {
            Observed::Record(info, _) => Ok(valid_repo(&info.full_name)
                .then_some(info.full_name)
                .filter(|n| n != repo)),
            Observed::Missing => Err(RemoteError::NotFound),
            Observed::Denied { .. } => Err(RemoteError::Status(403)),
            Observed::Unchanged | Observed::Moved { .. } | Observed::Failed => Err(
                RemoteError::Protocol("a redirect to a repository GitHub will not describe".into()),
            ),
        }
    }

    /// Every issue of `repo`, open and closed, pull requests left out, and
    /// the first page's ETag; `None` when `etag` still matches it.
    pub async fn issues(
        &self,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Option<(Vec<Issue>, Option<String>)>, RemoteError> {
        let mut url = format!(
            "{}/repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=100",
            self.base
        );
        let mut out = Vec::new();
        let mut first_etag = None;
        let mut first = true;
        loop {
            let (status, headers, body) = self
                .call(Method::GET, &url, None, etag.filter(|_| first))
                .await?;
            if status == StatusCode::NOT_MODIFIED {
                return Ok(None);
            }
            if first {
                first_etag = headers
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                first = false;
            }
            let page: Vec<RawIssue> = parse(&body)?;
            out.extend(
                page.into_iter()
                    .filter(|r| r.pull_request.is_none())
                    .map(RawIssue::issue),
            );
            match headers
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(next_page)
            {
                Some(next) => url = next,
                None => return Ok(Some((out, first_etag))),
            }
        }
    }

    pub async fn create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        priority: u8,
    ) -> Result<Issue, RemoteError> {
        let url = format!("{}/repos/{repo}/issues", self.base);
        let mut body_json = serde_json::json!({ "title": title });
        if let Some(b) = body.filter(|b| !b.trim().is_empty()) {
            body_json["body"] = b.into();
        }
        if let Some(p) = label_of(priority) {
            body_json["labels"] = serde_json::json!([p]);
        }
        let (_, _, body) = self.call(Method::POST, &url, Some(body_json), None).await?;
        Ok(parse::<RawIssue>(&body)?.issue())
    }

    /// Change an issue's title, body, labels or state. Closing says why:
    /// `completed`, or `not_planned` for a task deleted rather than done.
    pub async fn update(
        &self,
        repo: &str,
        number: u32,
        change: &Change,
        not_planned: bool,
    ) -> Result<(), RemoteError> {
        let url = format!("{}/repos/{repo}/issues/{number}", self.base);
        let mut body = serde_json::Map::new();
        if let Some(t) = &change.title {
            body.insert("title".into(), t.clone().into());
        }
        if let Some(b) = &change.body {
            // An empty body clears the issue's.
            body.insert("body".into(), b.clone().unwrap_or_default().into());
        }
        if let Some(labels) = &change.labels {
            body.insert("labels".into(), serde_json::json!(labels));
        }
        match change.open {
            Some(true) => {
                body.insert("state".into(), "open".into());
            }
            Some(false) => {
                body.insert("state".into(), "closed".into());
                let why = if not_planned {
                    "not_planned"
                } else {
                    "completed"
                };
                body.insert("state_reason".into(), why.into());
            }
            None => {}
        }
        self.call(Method::PATCH, &url, Some(body.into()), None)
            .await?;
        Ok(())
    }

    /// Whether the issue is still in `repo`: a transferred one answers
    /// with a redirect, a deleted one 404 or 410.
    pub async fn exists(&self, repo: &str, number: u32) -> Result<bool, RemoteError> {
        let url = format!("{}/repos/{repo}/issues/{number}", self.base);
        match self.call(Method::GET, &url, None, None).await {
            Ok((s, _, _)) => Ok(s == StatusCode::OK),
            Err(RemoteError::NotFound | RemoteError::Status(301)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, RemoteError> {
    serde_json::from_slice(body).map_err(|e| RemoteError::Protocol(e.to_string()))
}

/// The answer to a repository lookup, read. `now` is the time in epoch
/// seconds, for a rate limit that says when it resets and not how long.
fn observe(
    status: StatusCode,
    headers: &HeaderMap,
    body: &[u8],
    now: u64,
) -> Result<Observed, RemoteError> {
    Ok(match status.as_u16() {
        200 => match parse::<RawRepo>(body) {
            Ok(r) if valid_repo(&r.full_name) => Observed::Record(
                RepoInfo {
                    id: r.id,
                    full_name: r.full_name,
                    archived: r.archived,
                    disabled: r.disabled,
                },
                header(headers, "etag").map(str::to_string),
            ),
            _ => {
                log::info!("GitHub's repository record is not one asst can read");
                Observed::Failed
            }
        },
        304 => Observed::Unchanged,
        301 => match header(headers, "location").and_then(repository_id) {
            Some(id) => Observed::Moved { id },
            None => {
                log::info!("GitHub redirected a repository to somewhere that is not an id");
                Observed::Failed
            }
        },
        404 | 410 => Observed::Missing,
        403 | 429 => Observed::Denied {
            retry: retry_after(headers, now),
        },
        // As for every other call: the daemon asks for the token again.
        401 => return Err(RemoteError::Unauthorized),
        n => {
            log::info!("GitHub answered {n} for a repository");
            Observed::Failed
        }
    })
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// How long a refusal asks to wait: `Retry-After` when it has one, else
/// until the window resets when no requests are left in it. A 403 that
/// shows only the window's headers (the token cannot see the repo) says
/// nothing about waiting.
fn retry_after(headers: &HeaderMap, now: u64) -> Option<Duration> {
    let number = |name| header(headers, name).and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(secs) = number("retry-after") {
        return Some(Duration::from_secs(secs));
    }
    (number("x-ratelimit-remaining") == Some(0))
        .then(|| number("x-ratelimit-reset"))
        .flatten()
        .map(|reset| Duration::from_secs(reset.saturating_sub(now)))
}

/// The id in a redirect's `Location`: `…/repositories/<id>/…`.
fn repository_id(location: &str) -> Option<u64> {
    let rest = location.split_once("/repositories/")?.1;
    let digits = rest.split(['/', '?']).next()?;
    digits.parse().ok()
}

/// The `rel="next"` URL of a `Link` header.
fn next_page(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u32, title: &str, open: bool) -> Issue {
        Item {
            id: number,
            title: title.into(),
            open,
            body: None,
            priority: 0,
            labels: vec![],
        }
    }

    fn task(uid: &str, title: &str, open: bool) -> Ref {
        Item {
            id: uid.into(),
            title: title.into(),
            open,
            body: None,
            priority: 0,
            labels: vec![],
        }
    }

    fn pair(number: u32, uid: &str, title: &str, open: bool) -> Pair {
        Pair {
            number,
            uid: uid.into(),
            title: title.into(),
            open,
            body: None,
            priority: 0,
        }
    }

    fn merged(task: Change, issue: Change, title: &str, open: bool) -> Op {
        Op::Merge {
            number: 1,
            uid: "a".into(),
            task,
            issue,
            title: title.into(),
            open,
            body: None,
            priority: 0,
        }
    }

    fn title(t: &str) -> Change {
        Change {
            title: Some(t.into()),
            ..Change::default()
        }
    }

    fn state(open: bool) -> Change {
        Change {
            open: Some(open),
            ..Change::default()
        }
    }

    #[test]
    fn an_agreed_pair_is_left_alone() {
        let base = [pair(1, "a", "Fix it", true)];
        assert_eq!(
            plan(
                &[issue(1, "Fix it", true)],
                &[task("a", "Fix it", true)],
                &base
            ),
            []
        );
    }

    #[test]
    fn the_side_that_moved_wins() {
        let base = [pair(1, "a", "Fix it", true)];
        // Retitled on GitHub
        assert_eq!(
            plan(
                &[issue(1, "Fix", true)],
                &[task("a", "Fix it", true)],
                &base
            ),
            [merged(title("Fix"), Change::default(), "Fix", true)]
        );
        // Completed here
        assert_eq!(
            plan(
                &[issue(1, "Fix it", true)],
                &[task("a", "Fix it", false)],
                &base
            ),
            [merged(Change::default(), state(false), "Fix it", false)]
        );
        // Each side moved a different field: both go across
        assert_eq!(
            plan(
                &[issue(1, "Fix", true)],
                &[task("a", "Fix it", false)],
                &base
            ),
            [merged(title("Fix"), state(false), "Fix", false)]
        );
        // Both moved one field: the issue wins
        assert_eq!(
            plan(
                &[issue(1, "GitHub", true)],
                &[task("a", "Here", true)],
                &base
            ),
            [merged(title("GitHub"), Change::default(), "GitHub", true)]
        );
        // Both moved to the same place: only the base catches up
        assert_eq!(
            plan(&[issue(1, "Fix", false)], &[task("a", "Fix", false)], &base),
            [merged(Change::default(), Change::default(), "Fix", false)]
        );
    }

    #[test]
    fn a_side_gone_ends_the_pair() {
        let base = [pair(1, "a", "One", true), pair(2, "b", "Two", true)];
        // Task deleted: close its issue; closed already: just unpair
        assert_eq!(
            plan(&[issue(1, "One", true), issue(2, "Two", false)], &[], &base),
            [Op::CloseIssue { number: 1 }, Op::Unpair { number: 2 }]
        );
        // Issue gone: complete the open task; a completed one just unpairs
        assert_eq!(
            plan(
                &[],
                &[task("a", "One", true), task("b", "Two", false)],
                &base
            ),
            [
                Op::CompleteTask {
                    number: 1,
                    uid: "a".into()
                },
                Op::Unpair { number: 2 }
            ]
        );
        assert_eq!(plan(&[], &[], &base[..1]), [Op::Unpair { number: 1 }]);
    }

    #[test]
    fn unpaired_open_items_cross_over() {
        let issues = [
            issue(1, "Same", true),
            issue(2, "Only on GitHub", true),
            issue(3, "Closed", false),
        ];
        let tasks = [
            task("x", "Only here", true),
            task("y", "Same", true),
            task("z", "Done", false),
            task("w", "Same", true),
        ];
        assert_eq!(
            plan(&issues, &tasks, &[]),
            [
                Op::Adopt {
                    number: 1,
                    uid: "y".into(),
                    title: "Same".into(),
                    body: None,
                    priority: 0
                },
                Op::NewTask {
                    number: 2,
                    title: "Only on GitHub".into(),
                    body: None,
                    priority: 0
                },
                Op::NewIssue {
                    uid: "x".into(),
                    title: "Only here".into(),
                    body: None,
                    priority: 0
                },
                // One task per issue: the second "Same" gets its own
                Op::NewIssue {
                    uid: "w".into(),
                    title: "Same".into(),
                    body: None,
                    priority: 0
                },
            ]
        );
    }

    #[test]
    fn a_paired_task_is_not_adopted_again() {
        let base = [pair(1, "a", "Same", true)];
        let issues = [issue(1, "Same", true), issue(2, "Same", true)];
        assert_eq!(
            plan(&issues, &[task("a", "Same", true)], &base),
            [Op::NewTask {
                number: 2,
                title: "Same".into(),
                body: None,
                priority: 0
            }]
        );
    }

    #[test]
    fn notes_and_priority_cross_over_like_the_title() {
        let mut base = pair(1, "a", "Fix it", true);
        let mut i = issue(1, "Fix it", true);
        let mut t = task("a", "Fix it", true);
        // Notes written here, priority set on GitHub: both go across, and
        // the issue's P2 label is already what was agreed.
        t.body = Some("step one".into());
        i.priority = 5;
        i.labels = vec!["bug".into(), "P2".into()];
        assert_eq!(
            plan(&[i.clone()], &[t.clone()], &[base.clone()]),
            [Op::Merge {
                number: 1,
                uid: "a".into(),
                task: Change {
                    priority: Some(5),
                    ..Change::default()
                },
                issue: Change {
                    body: Some(Some("step one".into())),
                    ..Change::default()
                },
                title: "Fix it".into(),
                open: true,
                body: Some("step one".into()),
                priority: 5,
            }]
        );
        // Priority set here: the issue's labels are replaced whole, its
        // other labels kept.
        base.priority = 0;
        i.priority = 0;
        i.labels = vec!["bug".into()];
        t.priority = 5;
        t.body = None;
        assert_eq!(
            plan(&[i.clone()], &[t.clone()], &[base.clone()]),
            [Op::Merge {
                number: 1,
                uid: "a".into(),
                task: Change::default(),
                issue: Change {
                    priority: Some(5),
                    labels: Some(vec!["bug".into(), "P2".into()]),
                    ..Change::default()
                },
                title: "Fix it".into(),
                open: true,
                body: None,
                priority: 5,
            }]
        );
        // A body edited on GitHub comes back into the notes; the label
        // dropped there drops the priority here.
        i.body = Some("from the issue".into());
        i.labels = vec![];
        t.priority = 0;
        assert_eq!(
            plan(&[i], &[t], &[base]),
            [Op::Merge {
                number: 1,
                uid: "a".into(),
                task: Change {
                    body: Some(Some("from the issue".into())),
                    ..Change::default()
                },
                issue: Change::default(),
                title: "Fix it".into(),
                open: true,
                body: Some("from the issue".into()),
                priority: 0,
            }]
        );
    }

    #[test]
    fn repo_names() {
        for good in ["jaehho/asst", "a-b/c.d_e", "o/.github"] {
            assert!(valid_repo(good), "{good}");
        }
        for bad in ["asst", "/asst", "jaehho/", "a/b/c", "a/..", "a b/c", ""] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn a_redirect_names_the_repository_by_id() {
        assert_eq!(
            repository_id("https://api.github.com/repositories/1379104135/issues?state=all"),
            Some(1379104135)
        );
        assert_eq!(
            repository_id("https://api.github.com/repositories/42"),
            Some(42)
        );
        assert_eq!(repository_id("https://api.github.com/repos/a/b"), None);
        assert_eq!(
            repository_id("https://api.github.com/repositories/x/y"),
            None
        );
    }

    #[test]
    fn next_links() {
        let link = r#"<https://api.github.com/repositories/1/issues?page=2>; rel="next", <https://api.github.com/repositories/1/issues?page=5>; rel="last""#;
        assert_eq!(
            next_page(link).as_deref(),
            Some("https://api.github.com/repositories/1/issues?page=2")
        );
        assert_eq!(next_page(r#"<https://x/?page=1>; rel="prev""#), None);
    }
}

#[cfg(test)]
mod repo_tests {
    use super::*;
    use crate::testing::{FakeGitHub, Reply};

    // Answers recorded from api.github.com with a read-only GET and the
    // owner's token, then cut to the fields asst reads (no timestamps,
    // counts, descriptions or URL templates) and the headers it uses.
    const LAB: &str = include_str!("../tests/fixtures/github/lab-by-id.http");
    const ARCHIVED: &str = include_str!("../tests/fixtures/github/mail-digest-archived.http");
    const MOVED: &str = include_str!("../tests/fixtures/github/champalimaud-moved.http");
    const NOT_FOUND: &str = include_str!("../tests/fixtures/github/not-found.http");

    const LAB_ID: u64 = 1379104005;
    const LAB_ETAG: &str = "\"45affe4b62fa7f4bcef84f988e8f0267fd85a1ef28e90592f8fcbe38f24ecb2e\"";

    /// A recorded answer: the status line, headers, a blank line, the body.
    fn recorded(text: &str) -> (StatusCode, HeaderMap, String) {
        let (head, body) = text
            .split_once("\n\n")
            .expect("a blank line before the body");
        let mut lines = head.lines();
        let code = lines.next().unwrap().split(' ').nth(1).unwrap();
        let mut headers = HeaderMap::new();
        for line in lines {
            let (name, value) = line.split_once(": ").unwrap();
            headers.append(
                http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        (code.parse().unwrap(), headers, body.to_string())
    }

    fn observed(text: &str) -> Observed {
        let (status, headers, body) = recorded(text);
        observe(status, &headers, body.as_bytes(), 0).unwrap()
    }

    fn lab() -> RepoInfo {
        RepoInfo {
            id: LAB_ID,
            full_name: "jaehho/lab".into(),
            archived: false,
            disabled: false,
        }
    }

    fn answer(
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
        now: u64,
    ) -> Result<Observed, RemoteError> {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            map.append(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        observe(
            StatusCode::from_u16(status).unwrap(),
            &map,
            body.as_bytes(),
            now,
        )
    }

    #[test]
    fn the_recorded_answers_read_as_observations() {
        assert_eq!(
            observed(LAB),
            Observed::Record(lab(), Some(LAB_ETAG.into()))
        );
        let Observed::Record(archived, _) = observed(ARCHIVED) else {
            panic!("a record");
        };
        assert_eq!(
            archived,
            RepoInfo {
                id: 1379104203,
                full_name: "jaehho/mail-digest".into(),
                archived: true,
                disabled: false,
            }
        );
        // The old name answers with a redirect to the id, nothing more
        assert_eq!(observed(MOVED), Observed::Moved { id: LAB_ID });
        assert_eq!(observed(NOT_FOUND), Observed::Missing);
    }

    #[test]
    fn an_answer_that_settles_nothing_is_not_a_finding() {
        let none = |status| answer(status, &[], "", 0).unwrap();
        assert_eq!(none(304), Observed::Unchanged);
        for gone in [404, 410] {
            assert_eq!(none(gone), Observed::Missing);
        }
        for broken in [500, 502, 503, 418, 451, 307, 302] {
            assert_eq!(none(broken), Observed::Failed, "{broken}");
        }
        // The same error as every other call: the daemon asks for a token
        assert!(matches!(
            answer(401, &[], "", 0),
            Err(RemoteError::Unauthorized)
        ));
        // A 301 that is not to an id, a body that is not a record
        let elsewhere = [("location", "https://api.github.com/repos/jaehho/lab")];
        assert_eq!(answer(301, &elsewhere, "", 0).unwrap(), Observed::Failed);
        assert_eq!(answer(301, &[], "", 0).unwrap(), Observed::Failed);
        for body in [
            "",
            "<html>",
            "[]",
            r#"{"id":1,"full_name":"jaehho/lab","archived":false}"#,
            r#"{"id":1,"full_name":"jaehho/lab","disabled":false}"#,
            r#"{"id":1,"full_name":"lab","archived":false,"disabled":false}"#,
            r#"{"id":"1","full_name":"jaehho/lab","archived":false,"disabled":false}"#,
        ] {
            assert_eq!(
                answer(200, &[], body, 0).unwrap(),
                Observed::Failed,
                "{body}"
            );
        }
        // Disabled counts as a flag of its own
        let disabled = r#"{"id":1,"full_name":"a/b","archived":false,"disabled":true}"#;
        let Observed::Record(info, etag) = answer(200, &[], disabled, 0).unwrap() else {
            panic!("a record");
        };
        assert!(info.disabled && !info.archived && etag.is_none());
    }

    #[test]
    fn a_refusal_says_when_to_come_back_only_when_it_does() {
        let denied = |status, headers: &[(&str, &str)]| match answer(status, headers, "", 1000) {
            Ok(Observed::Denied { retry }) => retry,
            other => panic!("{other:?}"),
        };
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(denied(429, &[("retry-after", "60")]), secs(60));
        assert_eq!(denied(403, &[("retry-after", "7")]), secs(7));
        // No requests left: wait for the window
        let spent = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1090"),
        ];
        assert_eq!(denied(403, &spent), secs(90));
        assert_eq!(denied(429, &spent), secs(90));
        // Retry-After is the more exact of the two
        let both = [
            ("retry-after", "5"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1090"),
        ];
        assert_eq!(denied(403, &both), secs(5));
        // A reset in the past: come back now
        let past = [("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "900")];
        assert_eq!(denied(403, &past), secs(0));
        // A plain 403 with the window's headers (requests left) is the
        // token not seeing the repo, not a reason to wait
        let plain = [
            ("x-ratelimit-remaining", "4990"),
            ("x-ratelimit-reset", "1090"),
        ];
        assert_eq!(denied(403, &plain), None);
        assert_eq!(denied(403, &[]), None);
        assert_eq!(denied(429, &[("retry-after", "soon")]), None);
    }

    fn lab_body() -> String {
        recorded(LAB).2
    }

    #[tokio::test]
    async fn a_repo_is_read_by_name_and_by_id() {
        let fake = FakeGitHub::start().await;
        fake.reply("GET", "/repos/jaehho/lab", Reply::json(200, &lab_body()));
        fake.reply(
            "GET",
            "/repositories/1379104005",
            Reply::json(200, &lab_body()),
        );
        let gh = GitHub::with_base("tok", &fake.base()).unwrap();

        let by_name = gh.repo(Target::Name("jaehho/lab"), None).await.unwrap();
        assert_eq!(by_name, Observed::Record(lab(), None));
        let by_id = gh.repo(Target::Id(LAB_ID), None).await.unwrap();
        assert_eq!(by_id, Observed::Record(lab(), None));

        let seen = fake.seen();
        assert_eq!(
            seen.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(),
            ["/repos/jaehho/lab", "/repositories/1379104005"]
        );
        // The token goes to the base it was given, and no ETag was sent
        assert!(seen.iter().all(|r| r.method == "GET"
            && r.header("authorization") == Some("Bearer tok")
            && r.header("if-none-match").is_none()));
    }

    #[tokio::test]
    async fn an_etag_that_still_matches_costs_a_304() {
        let fake = FakeGitHub::start().await;
        fake.etagged("/repositories/1379104005", LAB_ETAG, &lab_body());
        let gh = GitHub::with_base("tok", &fake.base()).unwrap();

        let first = gh.repo(Target::Id(LAB_ID), None).await.unwrap();
        assert_eq!(first, Observed::Record(lab(), Some(LAB_ETAG.into())));
        let second = gh.repo(Target::Id(LAB_ID), Some(LAB_ETAG)).await.unwrap();
        assert_eq!(second, Observed::Unchanged);
        // A stale ETag gets the record again
        let stale = gh.repo(Target::Id(LAB_ID), Some("\"old\"")).await.unwrap();
        assert_eq!(stale, Observed::Record(lab(), Some(LAB_ETAG.into())));

        let sent: Vec<Option<&str>> = fake
            .seen()
            .iter()
            .map(|r| r.header("if-none-match").map(|_| "sent"))
            .collect();
        assert_eq!(sent, [None, Some("sent"), Some("sent")]);
        let seen = fake.seen();
        assert_eq!(seen[1].header("if-none-match"), Some(LAB_ETAG));
        assert_eq!(fake.count("/repositories/1379104005"), 3);
    }

    #[tokio::test]
    async fn the_old_name_redirects_to_the_id_and_the_id_has_the_name() {
        let fake = FakeGitHub::start().await;
        let (_, headers, body) = recorded(MOVED);
        let location = headers["location"]
            .to_str()
            .unwrap()
            .replace(API, &fake.base());
        fake.reply(
            "GET",
            "/repos/jaehho/champalimaud",
            Reply::json(301, &body).header("location", &location),
        );
        fake.reply(
            "GET",
            "/repositories/1379104005",
            Reply::json(200, &lab_body()),
        );
        fake.reply("GET", "/repos/jaehho/lab", Reply::json(200, &lab_body()));
        let gh = GitHub::with_base("tok", &fake.base()).unwrap();

        let moved = gh
            .repo(Target::Name("jaehho/champalimaud"), None)
            .await
            .unwrap();
        assert_eq!(moved, Observed::Moved { id: LAB_ID });
        assert_eq!(fake.seen().len(), 1);

        // renamed() asks the name, then the id it was sent to
        assert_eq!(
            gh.renamed("jaehho/champalimaud").await.unwrap().as_deref(),
            Some("jaehho/lab")
        );
        assert_eq!(fake.count("/repos/jaehho/champalimaud"), 2);
        assert_eq!(fake.count("/repositories/1379104005"), 1);

        // A repo that was not renamed is one request, and no answer
        assert_eq!(gh.renamed("jaehho/lab").await.unwrap(), None);
        assert_eq!(fake.count("/repos/jaehho/lab"), 1);
        assert_eq!(fake.count("/repositories/1379104005"), 1);
    }

    #[tokio::test]
    async fn renamed_is_an_error_when_the_id_cannot_be_read() {
        let fake = FakeGitHub::start().await;
        let location = format!("{}/repositories/42", fake.base());
        fake.reply(
            "GET",
            "/repos/a/old",
            Reply::status(301).header("location", &location),
        );
        let gh = GitHub::with_base("tok", &fake.base()).unwrap();
        // The id answers 404 (nothing routed)
        assert!(matches!(
            gh.renamed("a/old").await,
            Err(RemoteError::NotFound)
        ));
        fake.reply("GET", "/repositories/42", Reply::status(500));
        assert!(matches!(
            gh.renamed("a/old").await,
            Err(RemoteError::Protocol(_))
        ));
        fake.reply("GET", "/repositories/42", Reply::status(403));
        assert!(matches!(
            gh.renamed("a/old").await,
            Err(RemoteError::Status(403))
        ));
    }

    #[tokio::test]
    async fn refusals_and_errors_come_through_as_observations() {
        let fake = FakeGitHub::start().await;
        fake.reply(
            "GET",
            "/repos/a/gone",
            Reply::json(404, &recorded(NOT_FOUND).2),
        );
        fake.reply("GET", "/repos/a/hidden", Reply::status(410));
        fake.reply(
            "GET",
            "/repos/a/limited",
            Reply::json(403, "{}").header("retry-after", "30"),
        );
        fake.reply(
            "GET",
            "/repos/a/slow",
            Reply::status(429).header("retry-after", "120"),
        );
        fake.reply("GET", "/repos/a/broken", Reply::json(500, "oops"));
        fake.reply("GET", "/repos/a/denied", Reply::status(401));
        let gh = GitHub::with_base("tok", &fake.base()).unwrap();
        let look = |name: &'static str| {
            let gh = &gh;
            async move { gh.repo(Target::Name(name), None).await }
        };

        assert_eq!(look("a/gone").await.unwrap(), Observed::Missing);
        assert_eq!(look("a/hidden").await.unwrap(), Observed::Missing);
        assert_eq!(
            look("a/limited").await.unwrap(),
            Observed::Denied {
                retry: Some(Duration::from_secs(30))
            }
        );
        assert_eq!(
            look("a/slow").await.unwrap(),
            Observed::Denied {
                retry: Some(Duration::from_secs(120))
            }
        );
        assert_eq!(look("a/broken").await.unwrap(), Observed::Failed);
        assert!(matches!(
            look("a/denied").await,
            Err(RemoteError::Unauthorized)
        ));
        // One request each: nothing here retries by itself
        assert_eq!(fake.seen().len(), 6);
    }

    #[tokio::test]
    async fn a_refused_connection_is_an_error_not_a_finding() {
        let gh = GitHub::with_base("tok", &FakeGitHub::refused_base().await).unwrap();
        let err = gh.repo(Target::Id(LAB_ID), None).await.unwrap_err();
        assert!(matches!(err, RemoteError::Network(_)), "{err:?}");
        // renamed() cannot tell either, and says so the same way
        assert!(matches!(
            gh.renamed("a/b").await,
            Err(RemoteError::Network(_))
        ));
    }

    #[test]
    fn the_real_endpoint_is_the_default() {
        let gh = GitHub::new("tok").unwrap();
        assert_eq!(gh.base, "https://api.github.com");
        let local = GitHub::with_base("tok", "http://127.0.0.1:9/").unwrap();
        assert_eq!(local.base, "http://127.0.0.1:9");
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn rec(id: u64, name: &str, archived: bool, disabled: bool) -> Observed {
        Observed::Record(
            RepoInfo {
                id,
                full_name: name.into(),
                archived,
                disabled,
            },
            None,
        )
    }

    fn live(id: u64, name: &str) -> Observed {
        rec(id, name, false, false)
    }

    fn renamed(to: &str, witnessed: bool) -> Verdict {
        Verdict::Renamed {
            to: to.into(),
            witnessed,
        }
    }

    /// One row of the table: the stored id, the linked name and the
    /// archived baseline, what was seen, and the verdict.
    struct Row {
        why: &'static str,
        id: Option<u64>,
        archived: Option<bool>,
        seen: Observed,
        verdict: Verdict,
    }

    fn row(
        why: &'static str,
        id: Option<u64>,
        archived: Option<bool>,
        seen: Observed,
        verdict: Verdict,
    ) -> Row {
        Row {
            why,
            id,
            archived,
            seen,
            verdict,
        }
    }

    #[test]
    fn a_look_at_a_repo_is_judged_against_what_is_stored() {
        let moved = Observed::Moved { id: 7 };
        let denied = Observed::Denied {
            retry: Some(Duration::from_secs(60)),
        };
        let rows = [
            // Fine
            row(
                "same id, same name, same flag",
                Some(7),
                Some(false),
                live(7, "jaehho/lab"),
                Verdict::Fine,
            ),
            row(
                "the name in another case is the same name",
                Some(7),
                Some(false),
                live(7, "Jaehho/LAB"),
                Verdict::Fine,
            ),
            row(
                "a 304 is the record already judged",
                Some(7),
                Some(false),
                Observed::Unchanged,
                Verdict::Fine,
            ),
            row(
                "archived before and still",
                Some(7),
                Some(true),
                rec(7, "jaehho/lab", true, false),
                Verdict::Fine,
            ),
            // Learning
            row(
                "found by name with nothing stored",
                None,
                None,
                live(7, "jaehho/lab"),
                Verdict::LearnId(7),
            ),
            row(
                "found by name, archived, with nothing stored",
                None,
                None,
                rec(7, "jaehho/lab", true, false),
                Verdict::LearnId(7),
            ),
            // Renamed
            row(
                "same id, another full name",
                Some(7),
                Some(false),
                live(7, "jaehho/lab"),
                renamed("jaehho/lab", true),
            ),
            row(
                "301 then the record, no id stored",
                None,
                None,
                live(7, "jaehho/lab"),
                renamed("jaehho/lab", false),
            ),
            row(
                "301 to the stored id then the record",
                Some(7),
                None,
                live(7, "other/lab"),
                renamed("other/lab", true),
            ),
            // Archived
            row(
                "archived since the last look",
                Some(7),
                Some(false),
                rec(7, "jaehho/lab", true, false),
                Verdict::Archived,
            ),
            row(
                "disabled counts as archived",
                Some(7),
                Some(false),
                rec(7, "jaehho/lab", false, true),
                Verdict::Archived,
            ),
            row(
                "unarchived since the last look",
                Some(7),
                Some(true),
                live(7, "jaehho/lab"),
                Verdict::Unarchived,
            ),
            row(
                "first sight of an archived repo is not an event",
                Some(7),
                None,
                rec(7, "jaehho/lab", true, false),
                Verdict::Fine,
            ),
            row(
                "first sight of a live repo is not either",
                Some(7),
                None,
                live(7, "jaehho/lab"),
                Verdict::Fine,
            ),
            // Replaced: the name lookup made after the id was Missing
            row(
                "the name answers a record with another id",
                Some(7),
                Some(false),
                live(9, "jaehho/lab"),
                Verdict::Replaced { found: 9 },
            ),
            row(
                "the name answers a 301 to another id",
                Some(7),
                Some(false),
                Observed::Moved { id: 9 },
                Verdict::Replaced { found: 9 },
            ),
            row(
                "another id outranks a new name",
                Some(7),
                Some(false),
                live(9, "jaehho/elsewhere"),
                Verdict::Replaced { found: 9 },
            ),
            // Unknown
            row(
                "404 with an id",
                Some(7),
                Some(false),
                Observed::Missing,
                Verdict::Unknown,
            ),
            row(
                "404 without",
                None,
                None,
                Observed::Missing,
                Verdict::Unknown,
            ),
            row(
                "403 or 429 with an id",
                Some(7),
                Some(true),
                denied.clone(),
                Verdict::Unknown,
            ),
            row("403 without", None, None, denied, Verdict::Unknown),
            row(
                "5xx with an id",
                Some(7),
                Some(false),
                Observed::Failed,
                Verdict::Unknown,
            ),
            row(
                "5xx without",
                None,
                None,
                Observed::Failed,
                Verdict::Unknown,
            ),
            row(
                "a 304 with nothing stored has no record behind it",
                None,
                None,
                Observed::Unchanged,
                Verdict::Unknown,
            ),
            row(
                "a bare 301 to the stored id cannot name the repo",
                Some(7),
                Some(false),
                moved.clone(),
                Verdict::Unknown,
            ),
            row(
                "a bare 301 with nothing stored either",
                None,
                None,
                moved,
                Verdict::Unknown,
            ),
        ];
        for r in rows {
            // The link is called "jaehho/lab" in every row but the renames,
            // which name the repo it was before.
            let linked = match r.verdict {
                Verdict::Renamed { .. } => "jaehho/champalimaud",
                _ => "jaehho/lab",
            };
            assert_eq!(
                judge(r.id, linked, r.archived, &r.seen),
                r.verdict,
                "{}",
                r.why
            );
        }
    }

    #[test]
    fn a_repo_that_comes_back_is_a_finding_once_per_flip() {
        let flag =
            |archived, baseline| judge(Some(7), "a/b", baseline, &rec(7, "a/b", archived, false));
        assert_eq!(flag(true, Some(false)), Verdict::Archived);
        // The baseline moves with the finding, so the next look is Fine
        assert_eq!(flag(true, Some(true)), Verdict::Fine);
        assert_eq!(flag(false, Some(true)), Verdict::Unarchived);
        assert_eq!(flag(false, Some(false)), Verdict::Fine);
    }

    fn follow(list: &str, old: &str, new: &str) -> Result<String, KeepWhy> {
        follow_list_name(
            list,
            old,
            new,
            &["Work", "Home"],
            false,
            true,
            FollowRename::List,
        )
    }

    #[test]
    fn an_exactly_named_list_follows_its_repo() {
        assert_eq!(
            follow("champalimaud", "jaehho/champalimaud", "jaehho/lab"),
            Ok("lab".to_string())
        );
        // To another owner at the same time
        assert_eq!(
            follow("asst", "jaehho/asst", "org/tasks"),
            Ok("tasks".into())
        );
        // A new name that only looks like another list's
        assert_eq!(follow("a", "o/a", "o/Workshop"), Ok("Workshop".into()));
    }

    #[test]
    fn a_list_keeps_its_name_and_says_why() {
        let (old, new) = ("jaehho/champalimaud", "jaehho/lab");
        let cases = [
            (
                "a name someone chose",
                follow("Lab work", old, new),
                KeepWhy::Custom,
            ),
            (
                "a case someone chose",
                follow("Champalimaud", old, new),
                KeepWhy::Custom,
            ),
            ("a prefix of it", follow("champ", old, new), KeepWhy::Custom),
            (
                "a name with a space",
                follow("champalimaud ", old, new),
                KeepWhy::Custom,
            ),
            (
                "the list is not the repo's at all",
                follow("Work", old, new),
                KeepWhy::Custom,
            ),
            (
                "already the new name",
                follow("lab", old, new),
                KeepWhy::AlreadyNamed,
            ),
            (
                "already the new name in another case",
                follow("Lab", old, new),
                KeepWhy::AlreadyNamed,
            ),
            (
                "an owner-only transfer",
                follow("champalimaud", "jaehho/champalimaud", "org/champalimaud"),
                KeepWhy::SameName,
            ),
            (
                "an owner-only transfer, list already named for it",
                follow("lab", "jaehho/lab", "org/lab"),
                KeepWhy::SameName,
            ),
            (
                "an owner-only transfer, custom list",
                follow("Lab work", "jaehho/lab", "org/lab"),
                KeepWhy::SameName,
            ),
            (
                "a clash",
                follow_list_name(
                    "champalimaud",
                    old,
                    new,
                    &["lab"],
                    false,
                    true,
                    FollowRename::List,
                ),
                KeepWhy::Clash,
            ),
            (
                "a clash in any case",
                follow_list_name(
                    "champalimaud",
                    old,
                    new,
                    &["Work", "LAB"],
                    false,
                    true,
                    FollowRename::List,
                ),
                KeepWhy::Clash,
            ),
            (
                "the inbox",
                follow_list_name(
                    "champalimaud",
                    old,
                    new,
                    &["Work"],
                    true,
                    true,
                    FollowRename::List,
                ),
                KeepWhy::Inbox,
            ),
            (
                "a list the user cannot write",
                follow_list_name(
                    "champalimaud",
                    old,
                    new,
                    &["Work"],
                    false,
                    false,
                    FollowRename::List,
                ),
                KeepWhy::ReadOnly,
            ),
            (
                "the setting is off",
                follow_list_name(
                    "champalimaud",
                    old,
                    new,
                    &["Work"],
                    false,
                    true,
                    FollowRename::Off,
                ),
                KeepWhy::Off,
            ),
            (
                "no name part",
                follow("champalimaud", old, "jaehho/"),
                KeepWhy::Empty,
            ),
            (
                "no name at all",
                follow("champalimaud", old, ""),
                KeepWhy::Empty,
            ),
            (
                "no old name part",
                follow("", "jaehho/", new),
                KeepWhy::Empty,
            ),
        ];
        for (why, got, want) in cases {
            assert_eq!(got, Err(want), "{why}");
        }
    }

    #[test]
    fn the_first_reason_that_applies_is_the_one_given() {
        let (old, new) = ("jaehho/champalimaud", "jaehho/lab");
        let ask = |list, others: &[&str], inbox, writable, setting| {
            follow_list_name(list, old, new, others, inbox, writable, setting)
        };
        // Off outranks everything
        assert_eq!(
            ask("Custom", &["lab"], true, false, FollowRename::Off),
            Err(KeepWhy::Off)
        );
        // A list that is not the repo's to rename is Custom before it is
        // read-only, the inbox or in a clash
        assert_eq!(
            ask("Custom", &["lab"], true, false, FollowRename::List),
            Err(KeepWhy::Custom)
        );
        // Of the three that stop a list that is the repo's: inbox, then
        // read-only, then the clash
        assert_eq!(
            ask("champalimaud", &["lab"], true, false, FollowRename::List),
            Err(KeepWhy::Inbox)
        );
        assert_eq!(
            ask("champalimaud", &["lab"], false, false, FollowRename::List),
            Err(KeepWhy::ReadOnly)
        );
        assert_eq!(
            ask("champalimaud", &["lab"], false, true, FollowRename::List),
            Err(KeepWhy::Clash)
        );
        // The list's own name among the others changes nothing
        assert_eq!(
            ask(
                "champalimaud",
                &["champalimaud"],
                false,
                true,
                FollowRename::List
            ),
            Ok("lab".to_string())
        );
    }

    #[test]
    fn the_names_are_the_repo_halves() {
        assert_eq!(short("jaehho/lab"), "lab");
        assert_eq!(short("lab"), "lab");
        assert_eq!(short("jaehho/"), "");
        assert_eq!(short(""), "");
        assert_eq!(FollowRename::default(), FollowRename::List);
    }
}
