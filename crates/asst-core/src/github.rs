//! A list linked to a GitHub repo: each open issue is a task, each open task
//! an issue. Which issue is which task is kept in the store, with the title
//! and state both sides last agreed on; a pass compares each side with that
//! base, so it works the same whichever side changed, and a push that
//! failed is simply found again next time. `plan` decides, the daemon does.

use std::collections::HashSet;

use http::header::{ACCEPT, AUTHORIZATION, HeaderValue};
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
    let agreed =
        title != p.title || open != p.open || body != p.body || priority != p.priority;
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
        Ok(GitHub { auth })
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
        let out = caldav::send(req).await?;
        match out.0 {
            s if s.is_success() || s == StatusCode::NOT_MODIFIED => Ok(out),
            s => Err(caldav::status_error(s)),
        }
    }

    /// Every issue of `repo`, open and closed, pull requests left out, and
    /// the first page's ETag; `None` when `etag` still matches it.
    pub async fn issues(
        &self,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Option<(Vec<Issue>, Option<String>)>, RemoteError> {
        let mut url =
            format!("{API}/repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=100");
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
        let url = format!("{API}/repos/{repo}/issues");
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
        let url = format!("{API}/repos/{repo}/issues/{number}");
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
        let url = format!("{API}/repos/{repo}/issues/{number}");
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
    fn next_links() {
        let link = r#"<https://api.github.com/repositories/1/issues?page=2>; rel="next", <https://api.github.com/repositories/1/issues?page=5>; rel="last""#;
        assert_eq!(
            next_page(link).as_deref(),
            Some("https://api.github.com/repositories/1/issues?page=2")
        );
        assert_eq!(next_page(r#"<https://x/?page=1>; rel="prev""#), None);
    }
}
