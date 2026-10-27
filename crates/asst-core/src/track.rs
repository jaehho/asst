//! Project tags and ActivityWatch ingest for `asst track`.
//!
//! Sensors stay in aw-watchers. Stock AW does raw AFK-joined rollups
//! (`aw query`, webui). This is the higher-level layer: map signals to a
//! project tag (+ optional task_uid) and land classified spans in `blocks`.

use crate::config;
use crate::store::{Block, Store, StoreError};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http: {0}")]
    Http(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Msg(String),
}

pub type Result<T> = std::result::Result<T, TrackError>;

fn http_err(e: impl std::fmt::Display) -> TrackError {
    TrackError::Http(e.to_string())
}

/// Half-open interval in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Span {
    pub fn seconds(&self) -> f64 {
        (self.end - self.start).num_milliseconds() as f64 / 1000.0
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// One ActivityWatch event (read shape).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwEvent {
    pub timestamp: String,
    pub duration: f64,
    pub data: serde_json::Value,
}

pub fn event_span(e: &AwEvent) -> Option<Span> {
    let start = DateTime::parse_from_rfc3339(&e.timestamp)
        .ok()?
        .with_timezone(&Utc);
    let end = start + Duration::milliseconds((e.duration * 1000.0) as i64);
    if end <= start {
        return None;
    }
    Some(Span { start, end })
}

/// Intersect `span` with a sorted list of allowed intervals.
pub fn clip_to_allowed(span: Span, allowed: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    for a in allowed {
        let start = span.start.max(a.start);
        let end = span.end.min(a.end);
        if end > start {
            out.push(Span { start, end });
        }
    }
    out
}

/// Merge overlapping/touching spans into a minimal set.
pub fn merge_spans(mut spans: Vec<Span>) -> Vec<Span> {
    spans.retain(|s| !s.is_empty());
    spans.sort_by_key(|s| s.start);
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        match out.last_mut() {
            Some(last) if s.start <= last.end => {
                if s.end > last.end {
                    last.end = s.end;
                }
            }
            _ => out.push(s),
        }
    }
    out
}

/// Domain from a URL (host without leading www.).
pub fn url_domain(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

/// `~/.config/asst/tags.toml`, with a one-time fallback to the old
/// aw-watchers path so existing rules are not lost.
pub fn rules_path() -> PathBuf {
    let primary = config::config_dir().join("tags.toml");
    if primary.exists() {
        return primary;
    }
    let legacy = config::xdg_config_dir()
        .join("aw-watchers")
        .join("tags.toml");
    if legacy.exists() {
        return legacy;
    }
    primary
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct TagsConfig {
    #[serde(default)]
    pub rule: Vec<Rule>,
    #[serde(default)]
    pub default_tag: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Rule {
    pub tag: String,
    #[serde(default)]
    pub task_uid: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub cwd_prefix: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub app: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSource {
    Explicit,
    Rule,
    Field,
    Untagged,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub project: String,
    pub task_uid: Option<String>,
    pub source: TagSource,
}

/// Which bucket a span came from. Lower wins when spans overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceKind {
    Claude = 0,
    Neovim = 1,
    Obsidian = 2,
    Terminal = 3,
    Web = 4,
    Window = 5,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signals {
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub domain: Option<String>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub task_uid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalKind {
    Project,
    Cwd,
    Domain,
    App,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::Project => "project",
            SignalKind::Cwd => "cwd_prefix",
            SignalKind::Domain => "domain",
            SignalKind::App => "app",
        }
    }
}

impl TagsConfig {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let cfg: TagsConfig = toml::from_str(&text)?;
        Ok(cfg)
    }

    pub fn load_default() -> Result<Self> {
        Self::load(&rules_path())
    }
}

/// True when `path` is `prefix` or under it (path-boundary aware).
pub fn path_has_prefix(path: &str, prefix: &str) -> bool {
    let path = path.trim_end_matches('/');
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return false;
    }
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn rule_matches(rule: &Rule, sig: &Signals) -> bool {
    let mut any = false;
    if let Some(p) = &rule.project {
        any = true;
        if sig.project.as_deref() != Some(p.as_str()) {
            return false;
        }
    }
    if let Some(prefix) = &rule.cwd_prefix {
        any = true;
        let cwd = sig.cwd.as_deref().unwrap_or("");
        if !path_has_prefix(cwd, prefix) {
            return false;
        }
    }
    if let Some(d) = &rule.domain {
        any = true;
        if sig.domain.as_deref() != Some(d.as_str()) {
            return false;
        }
    }
    if let Some(a) = &rule.app {
        any = true;
        if sig.app.as_deref() != Some(a.as_str()) {
            return false;
        }
    }
    any
}

pub fn signals_of(e: &AwEvent) -> Signals {
    let project = e
        .data
        .get("project")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .map(|p| {
            // Obsidian's project field is always the product name.
            if p == "Obsidian" {
                e.data
                    .get("file")
                    .and_then(|v| v.as_str())
                    .and_then(project_from_vault_file)
                    .unwrap_or_else(|| "notes".into())
            } else {
                p
            }
        });
    let cwd = e
        .data
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let file = e
                .data
                .get("file")
                .and_then(|v| v.as_str())
                .filter(|s| s.starts_with('/'))?;
            file.rsplit_once('/').map(|(dir, _)| dir.to_string())
        });
    let domain = e
        .data
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(url_domain)
        .filter(|s| !s.is_empty());
    let app = e
        .data
        .get("app")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let title = e
        .data
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let task_uid = e
        .data
        .get("task_uid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Signals {
        project,
        cwd,
        domain,
        app,
        title,
        task_uid,
    }
}

/// Project name implied by a cwd, when the watcher did not stamp `project`.
pub fn project_from_cwd(cwd: &str) -> Option<String> {
    let path = cwd.trim_end_matches('/');
    if path.is_empty() || path.starts_with("/tmp/claude") {
        return None;
    }
    if let Some((_, rest)) = path.rsplit_once("/projects/") {
        let name = rest.split('/').next().filter(|s| !s.is_empty())?;
        return Some(name.to_string());
    }
    let leaf = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    if leaf == "jaeho" || leaf == "home" {
        return None;
    }
    Some(leaf.to_string())
}

/// Obsidian stamps `project = "Obsidian"` for every note; the vault folder
/// is the useful name (`/lab/types/README.md` → `lab`).
pub fn project_from_vault_file(file: &str) -> Option<String> {
    let f = file.trim_start_matches('/');
    let top = f.split('/').next().filter(|s| !s.is_empty())?;
    if top.contains('.') {
        // A note at the vault root (`Home.md`) is not a project.
        return Some("notes".into());
    }
    Some(top.to_string())
}

/// tmux `#S:#W` titles on kitty: session name when it is not a bare index.
pub fn project_from_title(app: Option<&str>, title: &str) -> Option<String> {
    let app = app.unwrap_or_default();
    if app != "kitty" && !app.contains("tmux") {
        return None;
    }
    let (session, _) = title.split_once(':')?;
    let session = session.trim();
    if session.is_empty() || session.contains(' ') || session.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(session.to_string())
}

/// Precedence: explicit task_uid > first matching rule > project field > cwd/title > untagged.
pub fn resolve_tag(cfg: &TagsConfig, sig: &Signals) -> Tag {
    if let Some(uid) = &sig.task_uid {
        return Tag {
            project: sig
                .project
                .clone()
                .or_else(|| sig.cwd.as_deref().and_then(project_from_cwd))
                .unwrap_or_else(|| "untagged".into()),
            task_uid: Some(uid.clone()),
            source: TagSource::Explicit,
        };
    }
    for rule in &cfg.rule {
        if rule_matches(rule, sig) {
            return Tag {
                project: rule.tag.clone(),
                task_uid: rule.task_uid.clone(),
                source: TagSource::Rule,
            };
        }
    }
    if let Some(p) = &sig.project {
        return Tag {
            project: p.clone(),
            task_uid: None,
            source: TagSource::Field,
        };
    }
    if let Some(p) = sig.cwd.as_deref().and_then(project_from_cwd).or_else(|| {
        sig.title
            .as_deref()
            .and_then(|t| project_from_title(sig.app.as_deref(), t))
    }) {
        return Tag {
            project: p,
            task_uid: None,
            source: TagSource::Field,
        };
    }
    if let Some(d) = &cfg.default_tag {
        return Tag {
            project: d.clone(),
            task_uid: None,
            source: TagSource::Untagged,
        };
    }
    Tag {
        project: "untagged".into(),
        task_uid: None,
        source: TagSource::Untagged,
    }
}

pub fn untagged_key(sig: &Signals) -> (SignalKind, String) {
    if let Some(p) = &sig.project {
        return (SignalKind::Project, p.clone());
    }
    if let Some(c) = &sig.cwd {
        if c.starts_with('/') {
            return (SignalKind::Cwd, c.clone());
        }
        let leaf = c.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(c);
        return (SignalKind::Project, leaf.to_string());
    }
    if let Some(d) = &sig.domain {
        return (SignalKind::Domain, d.clone());
    }
    if let Some(a) = &sig.app {
        return (SignalKind::App, a.clone());
    }
    (SignalKind::Project, "(none)".into())
}

/// True when `t` is a Claude kind string (`user_prompt`, `tool`, …) rather
/// than a session title.
fn is_kind_word(t: &str) -> bool {
    matches!(
        t,
        "user_prompt" | "assistant" | "tool" | "user" | "system" | "kind"
    )
}

/// A UUID (`3b65c44c-…`) is an identifier, not a name.
fn looks_like_uuid(t: &str) -> bool {
    let t = t.trim();
    let parts: Vec<&str> = t.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let lens = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(lens)
        .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Drop a title that only names the product or a blank tab.
fn weak_title(t: &str) -> bool {
    let t = t.trim();
    t.is_empty()
        || t == "New Tab"
        || t == "about:blank"
        || t == "about:newtab"
        || t == "Problem loading page"
        || t == "YouTube"
        || t == "Obsidian"
        || looks_like_uuid(t)
}

/// Browser chrome suffixes that are not part of the page name.
fn strip_browser_chrome(title: &str) -> &str {
    let mut t = title.trim();
    for sep in [" — ", " - "] {
        if let Some((head, tail)) = t.rsplit_once(sep) {
            let tail_l = tail.to_ascii_lowercase();
            if tail_l.contains("firefox")
                || tail_l.contains("chrome")
                || tail_l.contains("private browsing")
            {
                t = head.trim();
            }
        }
    }
    t
}

/// Human name for a span, when the event has one better than its tag.
/// Claude `ai_title`, then a cleaned web/window title, then a file name.
pub fn detail_of(e: &AwEvent) -> Option<String> {
    if let Some(t) = e.data.get("ai_title").and_then(|v| v.as_str()) {
        let t = t.trim();
        if !t.is_empty() && !t.split('+').all(is_kind_word) && !weak_title(t) {
            return Some(t.to_string());
        }
    }
    if let Some(t) = e.data.get("title").and_then(|v| v.as_str()) {
        let t = strip_browser_chrome(t);
        if !weak_title(t)
            && !t
                .split(':')
                .next()
                .is_some_and(|h| h.chars().all(|c| c.is_ascii_digit()) && t.contains(':'))
        {
            return Some(t.to_string());
        }
    }
    if let Some(f) = e.data.get("file").and_then(|v| v.as_str())
        && let Some((_, name)) = f.rsplit_once('/')
        && !name.is_empty()
        && !weak_title(name)
    {
        return Some(name.to_string());
    }
    None
}

/// Classified, AFK-clipped span from one merged timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedSpan {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tag: String,
    pub task_uid: Option<String>,
    pub source: String,
    pub label: String,
    /// Human name (page title, Claude `ai_title`, file). Day shows this
    /// when present; `label` stays the project/rule identity.
    pub detail: Option<String>,
    /// Other projects that overlapped this run, as `lab 40% · mail 30%`.
    pub composition: Option<String>,
    pub untagged_kind: Option<SignalKind>,
}

impl TaggedSpan {
    pub fn seconds(&self) -> f64 {
        (self.end - self.start).num_milliseconds() as f64 / 1000.0
    }
}

#[derive(Debug, Clone)]
struct Clip {
    span: Span,
    tag: Tag,
    label: String,
    detail: Option<String>,
    untagged_kind: Option<SignalKind>,
    kind: SourceKind,
}

/// Identity of a named span: project tag plus the words on its chip.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SpanId {
    tag: String,
    task_uid: Option<String>,
    label: String,
    detail: Option<String>,
}

/// One tagged timeline from several buckets.
///
/// A minute can belong to more than one project: Claude on `lab` while a
/// paper is open in Firefox is both, not "whatever SourceKind ranks first".
/// Each distinct identity that covers a slice gets its own span (the Day
/// lane already packs overlaps into columns). Within one identity the
/// lowest `SourceKind` names it.
pub fn tagged_timeline(
    sources: &[(SourceKind, &[AwEvent])],
    allowed: &[Span],
    cfg: &TagsConfig,
) -> Vec<TaggedSpan> {
    let mut clips: Vec<Clip> = Vec::new();
    for (src_kind, events) in sources {
        for e in *events {
            let Some(sp) = event_span(e) else { continue };
            let sig = signals_of(e);
            let tag = resolve_tag(cfg, &sig);
            let (unkind, label) = if tag.source == TagSource::Untagged {
                let (k, s) = untagged_key(&sig);
                (Some(k), s)
            } else {
                (None, tag.project.clone())
            };
            let detail = detail_of(e);
            for c in clip_to_allowed(sp, allowed) {
                clips.push(Clip {
                    span: c,
                    tag: tag.clone(),
                    label: label.clone(),
                    detail: detail.clone(),
                    untagged_kind: unkind,
                    kind: *src_kind,
                });
            }
        }
    }
    if clips.is_empty() {
        return Vec::new();
    }

    // Merge clips of one identity first so a long Firefox tab is not
    // shattered by every Claude heartbeat edge.
    clips.sort_by(|a, b| {
        (
            a.tag.project.clone(),
            a.label.clone(),
            a.detail.clone(),
            a.span.start,
        )
            .cmp(&(
                b.tag.project.clone(),
                b.label.clone(),
                b.detail.clone(),
                b.span.start,
            ))
    });
    let mut merged: Vec<Clip> = Vec::new();
    for c in clips {
        match merged.last_mut() {
            Some(last)
                if last.tag.project == c.tag.project
                    && last.label == c.label
                    && last.detail == c.detail
                    && last.kind == c.kind
                    && c.span.start <= last.span.end =>
            {
                last.span.end = last.span.end.max(c.span.end);
            }
            _ => merged.push(c),
        }
    }
    let clips = merged;

    let mut edges: BTreeSet<DateTime<Utc>> = BTreeSet::new();
    for c in &clips {
        edges.insert(c.span.start);
        edges.insert(c.span.end);
    }
    let edges: Vec<_> = edges.into_iter().collect();

    // id → running span, so consecutive slices of one identity join up.
    let mut open: BTreeMap<SpanId, TaggedSpan> = BTreeMap::new();
    let mut out: Vec<TaggedSpan> = Vec::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue;
        }
        // Best clip per identity in this slice.
        let mut best: BTreeMap<SpanId, &Clip> = BTreeMap::new();
        for c in &clips {
            if !(c.span.start < b && a < c.span.end) {
                continue;
            }
            let id = SpanId {
                tag: c.tag.project.clone(),
                task_uid: c.tag.task_uid.clone(),
                label: c.label.clone(),
                detail: c.detail.clone(),
            };
            let take = match best.get(&id) {
                None => true,
                Some(prev) => {
                    c.kind < prev.kind
                        || (c.kind == prev.kind && c.tag.source == TagSource::Explicit)
                }
            };
            if take {
                best.insert(id, c);
            }
        }

        // Close identities that did not appear in this slice.
        let live: BTreeSet<_> = best.keys().cloned().collect();
        let gone: Vec<_> = open
            .keys()
            .filter(|k| !live.contains(*k))
            .cloned()
            .collect();
        for id in gone {
            if let Some(s) = open.remove(&id) {
                out.push(s);
            }
        }

        for (id, c) in best {
            match open.get_mut(&id) {
                Some(last) if a <= last.end => {
                    last.end = b;
                }
                _ => {
                    open.insert(
                        id.clone(),
                        TaggedSpan {
                            start: a,
                            end: b,
                            tag: c.tag.project.clone(),
                            task_uid: c.tag.task_uid.clone(),
                            source: format!(
                                "aw:{}:{}:{}:{}",
                                a.timestamp_millis(),
                                c.tag.project,
                                c.label,
                                c.detail.as_deref().unwrap_or("")
                            ),
                            label: c.label.clone(),
                            detail: c.detail.clone(),
                            composition: None,
                            untagged_kind: c.untagged_kind,
                        },
                    );
                }
            }
        }
    }
    out.extend(open.into_values());
    out.sort_by_key(|s| (s.start, s.end, s.tag.clone(), s.label.clone()));
    out
}

/// Longest gap still considered the same work session.
///
/// Short AFK flickers and source handoffs should not split a task into
/// dozens of slivers. Distinct tagged projects stay separate.
pub const SESSION_GAP: Duration = Duration::minutes(5);

/// Spans shorter than this are sensor noise (about:blank flicker, focus
/// races, tab thrash) and are dropped when they stand alone. High enough
/// that a Day lane is not a hundred 6-second slivers.
pub const MIN_SPAN: Duration = Duration::seconds(30);

/// Collapse fragmented spans into calendar-like session blocks.
///
/// One chip per **project tag** run (gap-bounded). Inside a run the chip
/// takes the longest-running `detail` as its title; the other names show
/// up in `composition`. Concurrent projects still have different tags, so
/// they stay separate — but a tab flicker is not a new chip.
///
/// Untagged browsing keys on the label (domain / app) so Gmail and
/// YouTube do not become one gray bar, while every Gmail tab does.
pub fn coalesce_sessions(spans: &[TaggedSpan], max_gap: Duration) -> Vec<TaggedSpan> {
    let mut sorted: Vec<TaggedSpan> = spans.to_vec();
    sorted.retain(|s| s.end > s.start && s.seconds() >= MIN_SPAN.num_seconds() as f64);
    sorted.sort_by_key(|s| (s.start, s.end, s.tag.clone(), s.label.clone()));
    let mut out: Vec<TaggedSpan> = Vec::new();
    let mut open: BTreeMap<(String, String), Vec<TaggedSpan>> = BTreeMap::new();

    for s in sorted {
        let key = (
            s.tag.clone(),
            if s.tag == "untagged" {
                s.label.clone()
            } else {
                // Named projects share one run regardless of ai_title.
                s.tag.clone()
            },
        );
        let gap = match open.get(&key) {
            None => true,
            Some(members) => {
                let run_end = members.iter().map(|m| m.end).max();
                run_end.map(|e| s.start <= e + max_gap).unwrap_or(true)
            }
        };
        if gap {
            open.entry(key).or_default().push(s);
        } else {
            if let Some(members) = open.remove(&key) {
                out.push(session_block(members));
            }
            open.insert(key, vec![s]);
        }
    }
    for (_, members) in open {
        out.push(session_block(members));
    }
    out.sort_by_key(|s| (s.start, s.end, s.tag.clone(), s.label.clone()));
    out
}

/// Outer span of a session, labeled by where the time actually went.
fn session_block(members: impl IntoIterator<Item = TaggedSpan>) -> TaggedSpan {
    let mut start: Option<DateTime<Utc>> = None;
    let mut end: Option<DateTime<Utc>> = None;
    let mut first: Option<TaggedSpan> = None;
    let mut by_tag: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_label: BTreeMap<String, f64> = BTreeMap::new();
    // (tag, detail) → seconds, so the detail follows the winning tag
    // instead of whatever concurrent source happened to name itself.
    let mut by_detail: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut label_kind: BTreeMap<String, Option<SignalKind>> = BTreeMap::new();
    for m in members {
        let secs = m.seconds();
        *by_tag.entry(m.tag.clone()).or_default() += secs;
        *by_label.entry(m.label.clone()).or_default() += secs;
        if let Some(d) = &m.detail {
            *by_detail.entry((m.tag.clone(), d.clone())).or_default() += secs;
        }
        label_kind.insert(m.label.clone(), m.untagged_kind);
        start = Some(match start {
            None => m.start,
            Some(t) => t.min(m.start),
        });
        end = Some(match end {
            None => m.end,
            Some(t) => t.max(m.end),
        });
        if first.is_none() {
            first = Some(m);
        }
    }
    let first = first.expect("session has members");
    let start = start.expect("session has members");
    let end = end.expect("session has members");
    // Prefer a real project tag over untagged when both appear.
    let tag = by_tag
        .iter()
        .max_by(|(ka, a), (kb, b)| {
            let rank = |k: &String| if k.as_str() == "untagged" { 0u8 } else { 1u8 };
            rank(ka)
                .cmp(&rank(kb))
                .then(a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        })
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| first.tag.clone());
    let label = if tag == "untagged" {
        by_label
            .iter()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| first.label.clone())
    } else {
        tag.clone()
    };
    // Longest-running human name among the spans that carry the winning tag.
    let detail = by_detail
        .iter()
        .filter(|((t, _), _)| *t == tag)
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|((_, d), _)| d.clone());
    TaggedSpan {
        start,
        end,
        source: format!("aw:{}:{}", start.timestamp_millis(), tag),
        tag,
        task_uid: first.task_uid.clone(),
        untagged_kind: label_kind.get(&label).copied().flatten(),
        label,
        detail,
        composition: None,
    }
}

/// Parts of `span` that no `taken` span covers.
fn subtract_spans(span: Span, taken: &[Span]) -> Vec<Span> {
    let mut holes = vec![span];
    for t in taken {
        let mut next = Vec::new();
        for h in holes {
            if t.end <= h.start || t.start >= h.end {
                next.push(h);
                continue;
            }
            if h.start < t.start {
                next.push(Span {
                    start: h.start,
                    end: t.start.min(h.end),
                });
            }
            if t.end < h.end {
                next.push(Span {
                    start: t.end.max(h.start),
                    end: h.end,
                });
            }
        }
        holes = next;
    }
    holes.retain(|s| !s.is_empty());
    holes
}

/// Prefer the web tab over the bare window app when both claim the same
/// untagged minute: `disneyplus.com` beats `firefox-developer-edition`.
pub fn drop_redundant_window(spans: Vec<TaggedSpan>) -> Vec<TaggedSpan> {
    let web: Vec<Span> = spans
        .iter()
        .filter(|s| s.untagged_kind == Some(SignalKind::Domain))
        .map(|s| Span {
            start: s.start,
            end: s.end,
        })
        .collect();
    let web = merge_spans(web);
    spans
        .into_iter()
        .filter(|s| {
            if s.untagged_kind != Some(SignalKind::App) {
                return true;
            }
            let leftover = subtract_spans(
                Span {
                    start: s.start,
                    end: s.end,
                },
                &web,
            );
            // Keep only the app-name slices no web tab covered.
            !leftover.is_empty()
        })
        .flat_map(|s| {
            if s.untagged_kind == Some(SignalKind::App) {
                subtract_spans(
                    Span {
                        start: s.start,
                        end: s.end,
                    },
                    &web,
                )
                .into_iter()
                .map(|hole| {
                    let mut k = s.clone();
                    k.start = hole.start;
                    k.end = hole.end;
                    k
                })
                .collect::<Vec<_>>()
            } else {
                vec![s]
            }
        })
        .collect()
}

/// Drop untagged spans that sit under a named project. A Gmail tab and a
/// `firefox-developer-edition` bar over the same minute are one thing
/// (`email`), not two chips; the page title rides along in `composition`.
pub fn drop_covered_noise(spans: Vec<TaggedSpan>) -> Vec<TaggedSpan> {
    let named: Vec<Span> = spans
        .iter()
        .filter(|s| s.tag != "untagged")
        .map(|s| Span {
            start: s.start,
            end: s.end,
        })
        .collect();
    let named = merge_spans(named);
    let mut out = Vec::new();
    for s in spans {
        if s.tag != "untagged" {
            out.push(s);
            continue;
        }
        let span = Span {
            start: s.start,
            end: s.end,
        };
        for hole in subtract_spans(span, &named) {
            let mut kept = s.clone();
            kept.start = hole.start;
            kept.end = hole.end;
            if kept.seconds() >= MIN_SPAN.num_seconds() as f64 {
                out.push(kept);
            }
        }
    }
    out
}

/// A readable name for a concurrent span: the project, or for untagged
/// web/app the domain or title rather than the word `untagged`. The
/// window shell is never worth a mention.
fn composition_name(s: &TaggedSpan) -> String {
    if s.tag != "untagged" {
        return s.tag.clone();
    }
    s.detail
        .clone()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| s.label.clone())
}

fn composition_noise(name: &str) -> bool {
    matches!(
        name,
        "kitty"
            | "untagged"
            | "firefox-developer-edition"
            | "firefox-nvidia"
            | "xdg-desktop-portal-gtk"
            | "floating-tui"
    ) || name.is_empty()
}

/// Write `lab 40% · Gmail 30%` on each session from the fine spans that
/// overlap it. Display-only: the rollup already split the minutes.
/// Percentages are of the session's own length (may sum over 100% when
/// several sources cover the same minute).
pub fn annotate_composition(sessions: &mut [TaggedSpan], fine: &[TaggedSpan]) {
    for s in sessions.iter_mut() {
        let run_secs = s.seconds();
        if run_secs <= 0.0 {
            s.composition = None;
            continue;
        }
        let mut other: BTreeMap<String, f64> = BTreeMap::new();
        for f in fine {
            if f.tag == s.tag && f.label == s.label && f.detail == s.detail {
                continue;
            }
            let a = f.start.max(s.start);
            let b = f.end.min(s.end);
            if b <= a {
                continue;
            }
            let secs = (b - a).num_milliseconds() as f64 / 1000.0;
            let name = composition_name(f);
            // Skip the chip's own words and the window shell.
            if name == s.label || composition_noise(&name) || Some(&name) == s.detail.as_ref() {
                continue;
            }
            *other.entry(name).or_default() += secs;
        }
        if other.is_empty() {
            s.composition = None;
            continue;
        }
        let mut parts: Vec<(f64, String)> = other
            .into_iter()
            .map(|(name, secs)| {
                let pct = (100.0 * secs / run_secs).round().clamp(1.0, 99.0) as i64;
                (secs, format!("{name} {pct}%"))
            })
            .filter(|(secs, _)| *secs / run_secs >= 0.08)
            .collect();
        parts.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        parts.truncate(3);
        if parts.is_empty() {
            s.composition = None;
            continue;
        }
        s.composition = Some(
            parts
                .into_iter()
                .map(|(_, s)| s)
                .collect::<Vec<_>>()
                .join(" · "),
        );
    }
}

/// Keys that are not worth a rule: ids, internal pages, the shell.
fn rule_noise(kind: SignalKind, key: &str) -> bool {
    if key.is_empty() || key == "(none)" {
        return true;
    }
    if looks_like_uuid(key) {
        return true;
    }
    match kind {
        SignalKind::Domain => {
            key.starts_with("about:")
                || key.starts_with("127.0.0.1")
                || key.starts_with("localhost")
                || key.ends_with(".local")
        }
        SignalKind::App => matches!(
            key,
            "kitty"
                | "firefox-developer-edition"
                | "firefox-nvidia"
                | "xdg-desktop-portal-gtk"
                | "floating-tui"
        ),
        SignalKind::Cwd => key == "/home/jaeho" || key == "/tmp" || key.starts_with("/tmp/claude"),
        SignalKind::Project => matches!(key, "home" | "jaeho" | "tmp" | "bin" | "Downloads"),
    }
}

/// A project-ish name for an unmatched key, not the key itself.
fn rule_tag(kind: SignalKind, key: &str) -> String {
    let k = key.to_ascii_lowercase();
    if k.contains("mail.google") || k.contains("outlook") || k.starts_with("inbox") {
        return "email".into();
    }
    if k.contains("zoom.us") || k.contains("meet.google") {
        return "meetings".into();
    }
    if k.contains("youtube") || k.contains("reddit") || k.contains("disneyplus") {
        return "leisure".into();
    }
    if k.contains("wonhomelab")
        || k.contains("jellyfin")
        || k.contains("radarr")
        || k.contains("sonarr")
        || k.contains("qbittorrent")
    {
        return "homelab".into();
    }
    if k.contains("github.com") || k.contains("gitlab") {
        return "coding".into();
    }
    if k.contains("chase.com") || k.contains("robinhood") || k.contains("bcbs") {
        return "finance".into();
    }
    if k.contains("obsidian") {
        return "notes".into();
    }
    if k.contains("steno") {
        return "steno".into();
    }
    if k.contains("zoom") {
        return "meetings".into();
    }
    if k.contains("libreoffice") || k.contains("writer") || k.contains("calc") {
        return "docs".into();
    }
    if k.contains("sharepoint") || k.contains("mtsinai") {
        return "lab".into();
    }
    if k.contains("betterbird") || k.contains("thunderbird") {
        return "email".into();
    }
    match kind {
        SignalKind::Domain => {
            // Bare host as a tag is what made the timeline unreadable.
            key.split('.').next().unwrap_or(key).to_string()
        }
        SignalKind::App => key.to_string(),
        SignalKind::Cwd => key.rsplit('/').next().unwrap_or(key).to_string(),
        SignalKind::Project => key.to_string(),
    }
}

/// One day line for `asst track report`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DayLine {
    pub date: String,
    pub focused_hours: f64,
    pub projects: BTreeMap<String, f64>,
    pub stretches: Vec<Stretch>,
    pub switches: u32,
    pub thrash: u32,
}

/// An uninterrupted run of one project tag.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Stretch {
    pub start: String,
    pub end: String,
    pub minutes: f64,
    pub tag: String,
    pub detail: Option<String>,
    pub composition: Option<String>,
}

/// Roll a few days of spans into the numbers the report shows.
pub fn build_report(days: &[(String, IngestReport)]) -> Vec<DayLine> {
    days.iter()
        .map(|(date, rep)| {
            let mut projects = tag_rollup(&rep.spans);
            for v in projects.values_mut() {
                *v /= 60.0;
            }
            // Longest same-tag runs, already coalesced.
            let mut stretches: Vec<Stretch> = rep
                .spans
                .iter()
                .map(|s| Stretch {
                    start: s.start.to_rfc3339(),
                    end: s.end.to_rfc3339(),
                    minutes: s.seconds() / 60.0,
                    tag: s.tag.clone(),
                    detail: s.detail.clone(),
                    composition: s.composition.clone(),
                })
                .collect();
            stretches.sort_by(|a, b| {
                b.minutes
                    .partial_cmp(&a.minutes)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            stretches.truncate(8);
            // Topic switches on a single primary track: one tag per slice
            // (the non-untagged one covering most of it), so concurrent
            // lab+web is not a "switch".
            let mut edges: BTreeSet<DateTime<Utc>> = BTreeSet::new();
            for s in &rep.spans {
                edges.insert(s.start);
                edges.insert(s.end);
            }
            let edges: Vec<_> = edges.into_iter().collect();
            let mut run_secs = 0.0;
            let mut switches = 0u32;
            let mut thrash = 0u32;
            let mut prev: Option<String> = None;
            for w in edges.windows(2) {
                let (a, b) = (w[0], w[1]);
                let mut cover: BTreeMap<String, f64> = BTreeMap::new();
                for s in &rep.spans {
                    if s.start < b && a < s.end {
                        let secs =
                            (s.end.min(b) - s.start.max(a)).num_milliseconds() as f64 / 1000.0;
                        *cover.entry(s.tag.clone()).or_default() += secs;
                    }
                }
                let Some(tag) = cover
                    .iter()
                    .max_by(|(ka, x), (kb, y)| {
                        let rank = |k: &String| if k.as_str() == "untagged" { 0u8 } else { 1u8 };
                        rank(ka)
                            .cmp(&rank(kb))
                            .then(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
                    })
                    .map(|(k, _)| k.clone())
                else {
                    continue;
                };
                let slice = (b - a).num_milliseconds() as f64 / 1000.0;
                match &prev {
                    Some(p) if *p == tag => run_secs += slice,
                    Some(_) => {
                        switches += 1;
                        if run_secs < 120.0 {
                            thrash += 1;
                        }
                        run_secs = slice;
                    }
                    None => run_secs = slice,
                }
                prev = Some(tag);
            }
            if run_secs > 0.0 && run_secs < 120.0 && prev.is_some() {
                // Trailing run under two minutes counts as thrash too.
                thrash += 1;
            }
            DayLine {
                date: date.clone(),
                focused_hours: rep.focused_seconds / 3600.0,
                projects,
                stretches,
                switches,
                thrash,
            }
        })
        .collect()
}

/// Printable `[[rule]]` stubs for unmatched signals, ranked by minutes.
/// Does not write. Names the project, not the raw domain.
pub fn suggest_rules(spans: &[TaggedSpan]) -> String {
    let mut mins: BTreeMap<(SignalKind, String), f64> = BTreeMap::new();
    for s in spans {
        if s.tag != "untagged" {
            continue;
        }
        let Some(kind) = s.untagged_kind else {
            continue;
        };
        if rule_noise(kind, &s.label) {
            continue;
        }
        *mins.entry((kind, s.label.clone())).or_default() += s.seconds() / 60.0;
    }
    let mut ranked: Vec<_> = mins.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = String::new();
    for ((kind, key), minutes) in ranked.into_iter().take(25) {
        let tag = rule_tag(kind, &key);
        out.push_str(&format!("# {:.0} min\n", minutes));
        out.push_str("[[rule]]\n");
        out.push_str(&format!("tag = \"{tag}\"\n"));
        out.push_str(&format!("{} = \"{key}\"\n\n", kind.as_str()));
    }
    out
}

/// Minutes by project tag, splitting every moment among the tags that
/// cover it. Concurrent `lab` and web browsing is 50/50 for that minute,
/// not a double count — the rollup stays a partition of focused time.
pub fn tag_rollup(spans: &[TaggedSpan]) -> BTreeMap<String, f64> {
    if spans.is_empty() {
        return BTreeMap::new();
    }
    let mut edges: BTreeSet<DateTime<Utc>> = BTreeSet::new();
    for s in spans {
        edges.insert(s.start);
        edges.insert(s.end);
    }
    let edges: Vec<_> = edges.into_iter().collect();
    let mut acc: BTreeMap<String, f64> = BTreeMap::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        let secs = (b - a).num_milliseconds() as f64 / 1000.0;
        if secs <= 0.0 {
            continue;
        }
        let mut tags: BTreeSet<&str> = BTreeSet::new();
        for s in spans {
            if s.start < b && a < s.end {
                tags.insert(s.tag.as_str());
            }
        }
        if tags.is_empty() {
            continue;
        }
        let share = secs / tags.len() as f64;
        for t in tags {
            *acc.entry(t.to_string()).or_default() += share;
        }
    }
    acc
}

// ---------------------------------------------------------------------------
// ActivityWatch client (stock /api/0/query and /api/0/buckets)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AwClient {
    pub host: String,
    pub port: u16,
}

impl Default for AwClient {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 5600,
        }
    }
}

impl AwClient {
    fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("http://{}:{}{}", self.host, self.port, path);
        let resp = ureq::get(&url).call().map_err(http_err)?;
        Ok(serde_json::from_reader(resp.into_reader())?)
    }

    fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("http://{}:{}{}", self.host, self.port, path);
        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(http_err)?;
        Ok(serde_json::from_reader(resp.into_reader())?)
    }

    pub fn bucket_ids(&self) -> Result<Vec<String>> {
        let v = self.get_json("/api/0/buckets")?;
        let obj = v.as_object().cloned().unwrap_or_default();
        Ok(obj.keys().cloned().collect())
    }

    /// Every bucket whose id starts with `prefix`, sorted. A prefix can
    /// match more than one (the Firefox web watcher wrote both
    /// `aw-watcher-web-firefox` and `aw-watcher-web-firefox_<host>`).
    pub fn find_buckets(&self, prefix: &str) -> Result<Vec<String>> {
        let mut ids = self.bucket_ids()?;
        ids.sort();
        Ok(ids
            .into_iter()
            .filter(|id| id.starts_with(prefix))
            .collect())
    }

    pub fn find_bucket(&self, prefix: &str) -> Result<Option<String>> {
        Ok(self.find_buckets(prefix)?.into_iter().next())
    }

    /// Stock query language: one statement list per time period.
    /// Returns the first period's event list (flat).
    pub fn query_events(&self, query: &str, start: &str, end: &str) -> Result<Vec<AwEvent>> {
        let period = format!("{start}/{end}");
        let body = serde_json::json!({
            "query": [query],
            "timeperiods": [period],
        });
        let v = self.post_json("/api/0/query", &body)?;
        let arr = v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(serde_json::from_value(serde_json::Value::Array(arr))?)
    }
}

/// Stock AFK-join query for one bucket id (not a prefix: `find_bucket`
/// returns a single id, which drops sibling buckets that share a prefix).
pub fn afk_joined_query(bucket_id: &str) -> String {
    format!(
        r#"events = flood(query_bucket("{bucket_id}"));
not_afk = flood(query_bucket(find_bucket("aw-watcher-afk_")));
return filter_period_intersect(events, filter_keyvals(not_afk, "status", ["not-afk"]));"#
    )
}

/// not-afk spans for the day (for callers that clip in-process).
pub fn not_afk_events(client: &AwClient, start: &str, end: &str) -> Result<Vec<AwEvent>> {
    let bucket = client
        .find_bucket("aw-watcher-afk_")?
        .unwrap_or_else(|| "aw-watcher-afk_".into());
    client.query_events(&format!("return query_bucket(\"{bucket}\");"), start, end)
}

pub fn not_afk_spans(events: &[AwEvent]) -> Vec<Span> {
    let mut out = Vec::new();
    for e in events {
        let active = e
            .data
            .get("status")
            .and_then(|s| s.as_str())
            .map(|s| s == "not-afk")
            .unwrap_or(false);
        if !active {
            continue;
        }
        if let Some(sp) = event_span(e) {
            out.push(sp);
        }
    }
    merge_spans(out)
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

/// Local calendar day → (label, start RFC3339, end RFC3339) in UTC.
pub fn day_window(date: &str) -> Result<(String, String, String)> {
    use chrono::Local;
    let d: chrono::NaiveDate = date
        .parse()
        .map_err(|e| TrackError::Msg(format!("bad date {date:?}: {e}")))?;
    let start_local = Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .ok_or_else(|| TrackError::Msg("local midnight".into()))?;
    let end_local = Local
        .from_local_datetime(&(d + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap())
        .single()
        .ok_or_else(|| TrackError::Msg("next local midnight".into()))?;
    Ok((
        date.to_string(),
        start_local
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        end_local
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    ))
}

pub fn today() -> String {
    chrono::Local::now().date_naive().to_string()
}

pub struct IngestReport {
    pub date: String,
    pub focused_seconds: f64,
    pub spans: Vec<TaggedSpan>,
    pub tags: BTreeMap<String, f64>,
}

/// Fetch AFK-clipped events from each sensor bucket, tag, merge timeline.
pub fn collect_day(client: &AwClient, date: &str) -> Result<IngestReport> {
    let cfg = TagsConfig::load_default()?;
    let (_, start, end) = day_window(date)?;

    // Every bucket under the prefix: the web watcher has left a second id
    // behind after a client rename, and `find_bucket` only names one.
    let fetch = |prefix: &str| -> Result<Vec<AwEvent>> {
        let mut out = Vec::new();
        for id in client.find_buckets(prefix)? {
            out.extend(client.query_events(&afk_joined_query(&id), &start, &end)?);
        }
        Ok(out)
    };

    let claude = fetch("aw-watcher-claude_")?;
    let neovim = fetch("aw-watcher-neovim_")?;
    let obsidian = fetch("aw-watcher-obsidian_")?;
    let terminal = fetch("aw-watcher-terminal_")?;
    let window = fetch("aw-watcher-window_")?;
    let web = fetch("aw-watcher-web-")?;

    // Focused time from not-afk (stock events, not the join).
    let afk_raw = not_afk_events(client, &start, &end)?;
    let allowed = not_afk_spans(&afk_raw);
    let focused: f64 = allowed.iter().map(|s| s.seconds()).sum();

    let all = tagged_timeline(
        &[
            (SourceKind::Claude, &claude),
            (SourceKind::Neovim, &neovim),
            (SourceKind::Obsidian, &obsidian),
            (SourceKind::Terminal, &terminal),
            (SourceKind::Web, &web),
            (SourceKind::Window, &window),
        ],
        &allowed,
        &cfg,
    );
    // Chips stay quiet; composition still names what we hid.
    let fine = drop_covered_noise(drop_redundant_window(all.clone()));
    let mut spans = coalesce_sessions(&fine, SESSION_GAP);
    annotate_composition(&mut spans, &all);
    let tags = tag_rollup(&spans);
    Ok(IngestReport {
        date: date.to_string(),
        focused_seconds: focused,
        spans,
        tags,
    })
}

/// Replace actual (`planned=0`) blocks in [start, end) with `spans`.
///
/// Window replace, not per-row upsert: AFK late data can move boundaries.
pub fn ingest_spans(
    store: &mut Store,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    spans: &[TaggedSpan],
) -> Result<usize> {
    let rows: Vec<Block> = spans
        .iter()
        .map(|s| Block {
            id: None,
            task_uid: s.task_uid.clone(),
            start: s.start.timestamp_millis(),
            end: s.end.timestamp_millis(),
            label: Some(if s.tag == "untagged" {
                s.label.clone()
            } else {
                s.tag.clone()
            }),
            detail: s.detail.clone(),
            composition: s.composition.clone(),
            planned: false,
            source: Some(s.source.clone()),
            created_at: Utc::now().timestamp_millis(),
        })
        .collect();
    Ok(store.replace_actual_blocks(start.timestamp_millis(), end.timestamp_millis(), &rows)?)
}

/// One-time: drop the old noisy raw window import (`aw:e:*`).
pub fn purge_legacy_import(store: &Store) -> Result<usize> {
    Ok(store.delete_blocks_by_source_prefix("aw:e:")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn span(a: &str, b: &str) -> Span {
        Span {
            start: ts(a),
            end: ts(b),
        }
    }

    fn ev(start: &str, dur: f64, data: serde_json::Value) -> AwEvent {
        AwEvent {
            timestamp: start.into(),
            duration: dur,
            data,
        }
    }

    #[test]
    fn path_prefix_respects_boundary() {
        assert!(path_has_prefix(
            "/home/jaeho/projects/aw-watchers/src/x.rs",
            "/home/jaeho/projects/aw-watchers"
        ));
        assert!(!path_has_prefix(
            "/home/jaeho/projects/aw-watchers-old/x",
            "/home/jaeho/projects/aw-watchers"
        ));
    }

    #[test]
    fn resolve_precedence() {
        let cfg = TagsConfig {
            rule: vec![Rule {
                tag: "from-rule".into(),
                task_uid: Some("uid-1".into()),
                project: Some("raw".into()),
                cwd_prefix: None,
                domain: None,
                app: None,
            }],
            default_tag: None,
        };
        let explicit = resolve_tag(
            &cfg,
            &Signals {
                task_uid: Some("uid-9".into()),
                project: Some("raw".into()),
                ..Default::default()
            },
        );
        assert_eq!(explicit.source, TagSource::Explicit);
        let by_rule = resolve_tag(
            &cfg,
            &Signals {
                project: Some("raw".into()),
                ..Default::default()
            },
        );
        assert_eq!(by_rule.source, TagSource::Rule);
        assert_eq!(by_rule.project, "from-rule");
        let bare = resolve_tag(&cfg, &Signals::default());
        assert_eq!(bare.project, "untagged");
    }

    #[test]
    fn project_from_paths_and_titles() {
        assert_eq!(
            project_from_cwd("/home/jaeho/projects/aw-watchers").as_deref(),
            Some("aw-watchers")
        );
        assert_eq!(project_from_cwd("/tmp/claude-prompt-x.md"), None);
        assert_eq!(
            project_from_title(Some("kitty"), "champalimaud:claude").as_deref(),
            Some("champalimaud")
        );
        assert_eq!(
            project_from_title(Some("firefox-developer-edition"), "Chapter 11: x"),
            None
        );
    }

    #[test]
    fn concurrent_projects_are_both_kept_and_share_the_minute() {
        let allowed = vec![span("2026-09-30T12:00:00Z", "2026-09-30T13:00:00Z")];
        let cfg = TagsConfig::default();
        // A paper in the browser while Claude works on champalimaud.
        let window = vec![ev(
            "2026-09-30T12:00:00Z",
            3600.0,
            json!({"app": "firefox-developer-edition",
                   "title": "Walking Modulates Speed Sensitivity — Firefox Developer Edition"}),
        )];
        let web = vec![ev(
            "2026-09-30T12:00:00Z",
            3600.0,
            json!({"url": "https://www.cell.com/current-biology/x",
                   "title": "Walking Modulates Speed Sensitivity — Firefox Developer Edition"}),
        )];
        let claude = vec![ev(
            "2026-09-30T12:00:00Z",
            3600.0,
            json!({"project": "champalimaud", "ai_title": "lc-strength-retinotopy-clusters"}),
        )];
        let spans = tagged_timeline(
            &[
                (SourceKind::Claude, &claude),
                (SourceKind::Web, &web),
                (SourceKind::Window, &window),
            ],
            &allowed,
            &cfg,
        );
        // Both projects survive; the paper is not eaten by Claude.
        let tags: BTreeSet<_> = spans.iter().map(|s| s.tag.as_str()).collect();
        assert!(tags.contains("champalimaud"), "spans: {spans:?}");
        assert!(tags.contains("untagged"), "spans: {spans:?}");
        // Rollup is a partition: 1h of wall clock, not 3h of double count.
        let totals = tag_rollup(&spans);
        let sum: f64 = totals.values().sum();
        assert!((sum - 3600.0).abs() < 1.0, "rollup sum {sum}");
        assert!(totals["champalimaud"] > 0.0);
        assert!(totals["untagged"] > 0.0);
    }

    #[test]
    fn load_missing_rules_is_empty() {
        let cfg = TagsConfig::load(std::path::Path::new("/nope/tags.toml")).unwrap();
        assert!(cfg.rule.is_empty());
    }

    #[test]
    fn detail_prefers_ai_title_then_page_title() {
        let claude = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({
                "project": "ergo",
                "ai_title": "3d-stick-figure-joint-angles",
                "kind": "user_prompt",
            }),
        };
        assert_eq!(
            detail_of(&claude).as_deref(),
            Some("3d-stick-figure-joint-angles")
        );
        // Kind words alone are not a title.
        let kind_only = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({"project": "lab", "ai_title": "user_prompt+tool", "kind": "tool"}),
        };
        assert_eq!(detail_of(&kind_only), None);

        let web = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({
                "url": "https://www.cell.com/current-biology/x",
                "title": "Walking Modulates Speed Sensitivity — Firefox Developer Edition",
            }),
        };
        assert_eq!(
            detail_of(&web).as_deref(),
            Some("Walking Modulates Speed Sensitivity")
        );

        let blank = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({"url": "about:newtab", "title": "New Tab"}),
        };
        assert_eq!(detail_of(&blank), None);

        // tmux `1:claude` is an index, not a name.
        let tmux = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({"app": "kitty", "title": "1:claude"}),
        };
        assert_eq!(detail_of(&tmux), None);

        // A session id is not a name either.
        let uuid = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({"ai_title": "3b65c44c-55cc-4cd8-8071-d010881602f7"}),
        };
        assert_eq!(detail_of(&uuid), None);
    }

    #[test]
    fn obsidian_project_comes_from_the_vault_folder() {
        assert_eq!(
            project_from_vault_file("/lab/types/README.md").as_deref(),
            Some("lab")
        );
        assert_eq!(
            project_from_vault_file("/Home.md").as_deref(),
            Some("notes")
        );
        let e = AwEvent {
            timestamp: "2026-10-02T12:00:00Z".into(),
            duration: 0.0,
            data: json!({
                "project": "Obsidian",
                "file": "/literatures/wu2016.md",
                "projectPath": "/home/jaeho/Obsidian",
            }),
        };
        let sig = signals_of(&e);
        assert_eq!(sig.project.as_deref(), Some("literatures"));
    }

    #[test]
    fn untagged_under_named_work_disappears_but_orphan_stays() {
        let mk = |start: &str, end: &str, tag: &str, detail: Option<&str>| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: tag.into(),
            task_uid: None,
            source: format!("aw:{start}:{tag}:{detail:?}"),
            label: tag.into(),
            detail: detail.map(str::to_string),
            composition: None,
            untagged_kind: None,
        };
        let fine = vec![
            mk(
                "2026-10-02T12:00:00Z",
                "2026-10-02T12:30:00Z",
                "lab",
                Some("lc_output_strength.py"),
            ),
            // Browser bar over the same work, even with a title.
            mk(
                "2026-10-02T12:00:00Z",
                "2026-10-02T12:30:00Z",
                "untagged",
                Some("lc_output_strength.py"),
            ),
            // Untagged alone before anything named starts.
            mk(
                "2026-10-02T11:00:00Z",
                "2026-10-02T11:20:00Z",
                "untagged",
                Some("Zipf's law - Wikipedia"),
            ),
        ];
        let kept = drop_covered_noise(fine);
        assert_eq!(kept.len(), 2, "kept: {kept:?}");
        assert!(kept.iter().any(|s| s.tag == "lab"));
        let orphan = kept.iter().find(|s| s.tag == "untagged").unwrap();
        assert_eq!(orphan.start, ts("2026-10-02T11:00:00Z"));
        assert_eq!(orphan.end, ts("2026-10-02T11:20:00Z"));
    }

    #[test]
    fn composition_names_the_other_projects_in_the_run() {
        let mk =
            |start: &str, end: &str, tag: &str, label: &str, detail: Option<&str>| TaggedSpan {
                start: ts(start),
                end: ts(end),
                tag: tag.into(),
                task_uid: None,
                source: format!("aw:{start}:{tag}:{label}"),
                label: label.into(),
                detail: detail.map(str::to_string),
                composition: None,
                untagged_kind: None,
            };
        // An hour of lab with gmail open the whole time.
        let fine = vec![
            mk(
                "2026-10-02T12:00:00Z",
                "2026-10-02T13:00:00Z",
                "lab",
                "lab",
                Some("lc_output_strength.py"),
            ),
            mk(
                "2026-10-02T12:00:00Z",
                "2026-10-02T13:00:00Z",
                "untagged",
                "mail.google.com",
                Some("Inbox - Gmail"),
            ),
        ];
        let mut sessions = coalesce_sessions(&fine, SESSION_GAP);
        annotate_composition(&mut sessions, &fine);
        let lab = sessions.iter().find(|s| s.tag == "lab").unwrap();
        assert_eq!(lab.composition.as_deref(), Some("Inbox - Gmail 99%"));
    }

    #[test]
    fn suggest_names_projects_not_raw_domains() {
        let mk = |start: &str, end: &str, kind: SignalKind, label: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: "untagged".into(),
            task_uid: None,
            source: format!("aw:{start}:{label}"),
            label: label.into(),
            detail: None,
            composition: None,
            untagged_kind: Some(kind),
        };
        let out = suggest_rules(&[
            mk(
                "2026-10-02T12:00:00Z",
                "2026-10-02T12:30:00Z",
                SignalKind::Domain,
                "mail.google.com",
            ),
            mk(
                "2026-10-02T13:00:00Z",
                "2026-10-02T13:10:00Z",
                SignalKind::Domain,
                "youtube.com",
            ),
            // Noise: no rule stub.
            mk(
                "2026-10-02T14:00:00Z",
                "2026-10-02T14:05:00Z",
                SignalKind::Domain,
                "about:newtab",
            ),
            mk(
                "2026-10-02T15:00:00Z",
                "2026-10-02T15:05:00Z",
                SignalKind::App,
                "kitty",
            ),
        ]);
        assert!(out.contains("tag = \"email\""), "out:\n{out}");
        assert!(out.contains("domain = \"mail.google.com\""), "out:\n{out}");
        assert!(out.contains("tag = \"leisure\""), "out:\n{out}");
        assert!(!out.contains("about:newtab"), "out:\n{out}");
        assert!(!out.contains("kitty"), "out:\n{out}");
        // Ranked: email (30m) before leisure (10m).
        let email = out.find("email").unwrap();
        let leisure = out.find("leisure").unwrap();
        assert!(email < leisure);
    }

    #[test]
    fn report_counts_a_switch_on_the_primary_track_not_on_overlap() {
        let mk = |start: &str, end: &str, tag: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: tag.into(),
            task_uid: None,
            source: format!("aw:{start}:{tag}"),
            label: tag.into(),
            detail: None,
            composition: None,
            untagged_kind: None,
        };
        // lab runs an hour; gmail is open the whole time (not a switch).
        // Then a real switch to ergo.
        let spans = vec![
            mk("2026-10-02T12:00:00Z", "2026-10-02T13:00:00Z", "lab"),
            mk("2026-10-02T12:00:00Z", "2026-10-02T13:00:00Z", "email"),
            mk("2026-10-02T13:00:00Z", "2026-10-02T13:30:00Z", "ergo"),
        ];
        let report = IngestReport {
            date: "2026-10-02".into(),
            focused_seconds: 5400.0,
            spans,
            tags: BTreeMap::new(),
        };
        let lines = build_report(&[("2026-10-02".into(), report)]);
        // Primary track is lab (beats email) then ergo: one switch.
        assert_eq!(lines[0].switches, 1, "line: {:?}", lines[0]);
    }

    #[test]
    fn coalesce_joins_same_tag_across_short_gaps() {
        let a = TaggedSpan {
            start: ts("2026-10-02T12:00:00Z"),
            end: ts("2026-10-02T12:10:00Z"),
            tag: "champalimaud".into(),
            task_uid: None,
            source: "aw:1:champalimaud".into(),
            label: "champalimaud".into(),
            detail: None,
            composition: None,
            untagged_kind: None,
        };
        let mut b = a.clone();
        b.start = ts("2026-10-02T12:12:00Z");
        b.end = ts("2026-10-02T12:40:00Z");
        b.source = "aw:2:champalimaud".into();
        let mut far = a.clone();
        far.start = ts("2026-10-02T13:00:00Z");
        far.end = ts("2026-10-02T13:05:00Z");
        far.source = "aw:3:champalimaud".into();
        let out = coalesce_sessions(&[a, b, far], SESSION_GAP);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].start, ts("2026-10-02T12:00:00Z"));
        assert_eq!(out[0].end, ts("2026-10-02T12:40:00Z"));
        assert_eq!(out[0].label, "champalimaud");
    }

    #[test]
    fn coalesce_is_quiet_one_chip_per_site_not_per_page() {
        let mk = |start: &str, end: &str, label: &str, detail: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: "untagged".into(),
            task_uid: None,
            source: format!("aw:{start}:{label}:{detail}"),
            label: label.into(),
            detail: Some(detail.into()),
            composition: None,
            untagged_kind: Some(SignalKind::Domain),
        };
        let out = coalesce_sessions(
            &[
                mk(
                    "2026-10-02T12:00:00Z",
                    "2026-10-02T12:02:00Z",
                    "nytimes.com",
                    "Zipf's law - Wikipedia",
                ),
                mk(
                    "2026-10-02T12:02:00Z",
                    "2026-10-02T12:05:00Z",
                    "nytimes.com",
                    "Power Law: A Pattern Behind Extreme Events",
                ),
                // Other site: its own chip.
                mk(
                    "2026-10-02T12:05:30Z",
                    "2026-10-02T12:08:00Z",
                    "youtube.com",
                    "Some video",
                ),
            ],
            SESSION_GAP,
        );
        assert_eq!(out.len(), 2, "spans: {out:?}");
        // Two NYT pages are one nytimes.com chip (longest detail wins).
        let nyt = out.iter().find(|s| s.label == "nytimes.com").unwrap();
        assert_eq!(nyt.start, ts("2026-10-02T12:00:00Z"));
        assert_eq!(nyt.end, ts("2026-10-02T12:05:00Z"));
        assert_eq!(
            nyt.detail.as_deref(),
            Some("Power Law: A Pattern Behind Extreme Events"),
            "longest-running page is the title"
        );
    }

    #[test]
    fn coalesce_is_one_chip_per_project_across_ai_titles() {
        let mk = |start: &str, end: &str, detail: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: "lab".into(),
            task_uid: None,
            source: format!("aw:{start}:{detail}"),
            label: "lab".into(),
            detail: Some(detail.into()),
            composition: None,
            untagged_kind: None,
        };
        let out = coalesce_sessions(
            &[
                mk(
                    "2026-10-02T12:00:00Z",
                    "2026-10-02T12:10:00Z",
                    "Dashed edges",
                ),
                mk(
                    "2026-10-02T12:10:00Z",
                    "2026-10-02T12:25:00Z",
                    "Type tracking database note",
                ),
            ],
            SESSION_GAP,
        );
        assert_eq!(out.len(), 1, "spans: {out:?}");
        assert_eq!(out[0].tag, "lab");
        assert_eq!(out[0].end, ts("2026-10-02T12:25:00Z"));
        assert_eq!(
            out[0].detail.as_deref(),
            Some("Type tracking database note"),
            "longest title wins"
        );
    }

    #[test]
    fn coalesce_keeps_projects_apart_even_when_they_alternate() {
        let mk = |start: &str, end: &str, tag: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: tag.into(),
            task_uid: None,
            source: format!("aw:{start}:{tag}"),
            label: tag.into(),
            detail: None,
            composition: None,
            untagged_kind: None,
        };
        let out = coalesce_sessions(
            &[
                mk(
                    "2026-10-02T12:00:00Z",
                    "2026-10-02T12:10:00Z",
                    "champalimaud",
                ),
                mk("2026-10-02T12:11:00Z", "2026-10-02T12:20:00Z", "dotfiles"),
                mk(
                    "2026-10-02T12:21:00Z",
                    "2026-10-02T12:30:00Z",
                    "champalimaud",
                ),
                // Walked away: the next work is a new event.
                mk(
                    "2026-10-02T13:30:00Z",
                    "2026-10-02T13:40:00Z",
                    "champalimaud",
                ),
            ],
            SESSION_GAP,
        );
        // Two projects side by side are two chips, not one majority label.
        let mut by_tag: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
        for s in &out {
            by_tag
                .entry(s.tag.as_str())
                .or_default()
                .push((s.label.as_str(), s.detail.as_deref().unwrap_or("")));
        }
        assert_eq!(out.len(), 4, "spans: {out:?}");
        let lab: Vec<_> = out
            .iter()
            .filter(|s| s.tag == "champalimaud")
            .map(|s| (s.start, s.end))
            .collect();
        assert_eq!(lab.len(), 3);
        // The two near stretches are separate only when their gap exceeds
        // SESSION_GAP (12:10 → 12:21 is 11 minutes).
        assert_eq!(
            lab[0],
            (ts("2026-10-02T12:00:00Z"), ts("2026-10-02T12:10:00Z"))
        );
        assert_eq!(
            lab[1],
            (ts("2026-10-02T12:21:00Z"), ts("2026-10-02T12:30:00Z"))
        );
        assert_eq!(out.iter().filter(|s| s.tag == "dotfiles").count(), 1);
    }

    #[test]
    fn coalesce_absorbs_web_blips_into_the_same_project_run() {
        let mk = |start: &str, end: &str, tag: &str, label: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: tag.into(),
            task_uid: None,
            source: format!("aw:{start}:{label}"),
            label: label.into(),
            detail: None,
            composition: None,
            untagged_kind: None,
        };
        let out = coalesce_sessions(
            &[
                mk(
                    "2026-10-02T12:00:00Z",
                    "2026-10-02T12:10:00Z",
                    "champalimaud",
                    "champalimaud",
                ),
                // A 40s github peek is its own untagged chip…
                mk(
                    "2026-10-02T12:10:00Z",
                    "2026-10-02T12:10:40Z",
                    "untagged",
                    "github.com",
                ),
                // …while the project run continues underneath.
                mk(
                    "2026-10-02T12:10:40Z",
                    "2026-10-02T12:15:00Z",
                    "champalimaud",
                    "champalimaud",
                ),
            ],
            SESSION_GAP,
        );
        assert_eq!(out.len(), 2, "spans: {out:?}");
        let proj = out.iter().find(|s| s.tag == "champalimaud").unwrap();
        assert_eq!(proj.start, ts("2026-10-02T12:00:00Z"));
        assert_eq!(proj.end, ts("2026-10-02T12:15:00Z"));
        let peek = out.iter().find(|s| s.tag == "untagged").unwrap();
        assert_eq!(peek.label, "github.com");
    }

    #[test]
    fn store_ingest_idempotent_on_source() {
        let mut store = Store::in_memory(chrono_tz::UTC).unwrap();
        let spans = vec![TaggedSpan {
            start: ts("2026-09-30T12:00:00Z"),
            end: ts("2026-09-30T12:30:00Z"),
            tag: "dotfiles".into(),
            task_uid: None,
            source: "aw:1:dotfiles".into(),
            label: "dotfiles".into(),
            detail: None,
            composition: None,
            untagged_kind: None,
        }];
        let a = ingest_spans(
            &mut store,
            ts("2026-09-30T00:00:00Z"),
            ts("2026-10-01T00:00:00Z"),
            &spans,
        )
        .unwrap();
        assert_eq!(a, 1);
        let b = ingest_spans(
            &mut store,
            ts("2026-09-30T00:00:00Z"),
            ts("2026-10-01T00:00:00Z"),
            &spans,
        )
        .unwrap();
        assert_eq!(b, 1);
        let rows = store
            .blocks_between(
                ts("2026-09-30T00:00:00Z").timestamp_millis(),
                ts("2026-10-01T00:00:00Z").timestamp_millis(),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label.as_deref(), Some("dotfiles"));
        assert!(!rows[0].planned);
    }
}
