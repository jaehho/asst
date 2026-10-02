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
    let legacy = config::xdg_config_dir().join("aw-watchers").join("tags.toml");
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
    Terminal = 2,
    Web = 3,
    Window = 4,
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
        .map(str::to_string);
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
    if let Some(p) = sig
        .cwd
        .as_deref()
        .and_then(project_from_cwd)
        .or_else(|| {
            sig.title
                .as_deref()
                .and_then(|t| project_from_title(sig.app.as_deref(), t))
        })
    {
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

/// Classified, AFK-clipped span from one merged timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedSpan {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tag: String,
    pub task_uid: Option<String>,
    pub source: String,
    pub label: String,
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
    untagged_kind: Option<SignalKind>,
    kind: SourceKind,
}

/// One non-overlapping tagged timeline from several buckets.
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
            for c in clip_to_allowed(sp, allowed) {
                clips.push(Clip {
                    span: c,
                    tag: tag.clone(),
                    label: label.clone(),
                    untagged_kind: unkind,
                    kind: *src_kind,
                });
            }
        }
    }
    if clips.is_empty() {
        return Vec::new();
    }

    let mut edges: BTreeSet<DateTime<Utc>> = BTreeSet::new();
    for c in &clips {
        edges.insert(c.span.start);
        edges.insert(c.span.end);
    }
    let edges: Vec<_> = edges.into_iter().collect();

    let mut out: Vec<TaggedSpan> = Vec::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue;
        }
        let mut best: Option<&Clip> = None;
        for c in &clips {
            if c.span.start < b && a < c.span.end {
                best = match best {
                    None => Some(c),
                    Some(prev) if c.kind < prev.kind => Some(c),
                    Some(prev) if c.kind == prev.kind && c.tag.source == TagSource::Explicit => {
                        Some(c)
                    }
                    Some(prev) => Some(prev),
                };
            }
        }
        let Some(best) = best else { continue };

        match out.last_mut() {
            Some(last)
                if last.tag == best.tag.project
                    && last.task_uid == best.tag.task_uid
                    && last.label == best.label
                    && a <= last.end =>
            {
                if b > last.end {
                    last.end = b;
                }
            }
            _ => out.push(TaggedSpan {
                start: a,
                end: b,
                tag: best.tag.project.clone(),
                task_uid: best.tag.task_uid.clone(),
                source: format!(
                    "aw:{}:{}",
                    a.timestamp_millis(),
                    best.tag.project
                ),
                label: best.label.clone(),
                untagged_kind: best.untagged_kind,
            }),
        }
    }
    out
}

/// Longest gap still considered the same work session.
///
/// Short AFK flickers and source handoffs should not split a task into
/// dozens of slivers. Distinct tagged projects stay separate.
pub const SESSION_GAP: Duration = Duration::minutes(5);

/// Spans shorter than this are sensor noise (about:blank flicker, focus
/// races) and are dropped when they stand alone.
pub const MIN_SPAN: Duration = Duration::seconds(5);

/// Collapse fragmented spans into calendar-like session blocks.
///
/// `tagged_timeline` is intentionally fine-grained (correct boundaries and
/// rollups). The day planner wants one event per stretch of work: a gap is
/// what starts a new event, not a tab switch or a project flicker.
pub fn coalesce_sessions(spans: &[TaggedSpan], max_gap: Duration) -> Vec<TaggedSpan> {
    let mut sorted: Vec<TaggedSpan> = spans.to_vec();
    sorted.retain(|s| s.end > s.start && s.seconds() >= MIN_SPAN.num_seconds() as f64);
    sorted.sort_by_key(|s| (s.start, s.end));
    let mut out: Vec<TaggedSpan> = Vec::new();
    let mut members: Vec<TaggedSpan> = Vec::new();

    let flush = |members: &mut Vec<TaggedSpan>, out: &mut Vec<TaggedSpan>| {
        if members.is_empty() {
            return;
        }
        out.push(session_block(members.drain(..)));
    };

    for s in sorted {
        let joins = match members.last() {
            None => true,
            Some(last) => s.start <= last.end + max_gap,
        };
        if joins {
            members.push(s);
        } else {
            flush(&mut members, &mut out);
            members.push(s);
        }
    }
    flush(&mut members, &mut out);
    out
}

/// Outer span of a session, labeled by where the time actually went.
fn session_block(members: impl IntoIterator<Item = TaggedSpan>) -> TaggedSpan {
    let mut start: Option<DateTime<Utc>> = None;
    let mut end: Option<DateTime<Utc>> = None;
    let mut first: Option<TaggedSpan> = None;
    let mut by_tag: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_label: BTreeMap<String, f64> = BTreeMap::new();
    let mut label_kind: BTreeMap<String, Option<SignalKind>> = BTreeMap::new();
    for m in members {
        let secs = m.seconds();
        *by_tag.entry(m.tag.clone()).or_default() += secs;
        *by_label.entry(m.label.clone()).or_default() += secs;
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
    TaggedSpan {
        start,
        end,
        source: format!("aw:{}:{}", start.timestamp_millis(), tag),
        tag,
        task_uid: first.task_uid.clone(),
        untagged_kind: label_kind.get(&label).copied().flatten(),
        label,
    }
}

/// Printable `[[rule]]` stubs for unmatched signals. Does not write.
pub fn suggest_rules(spans: &[TaggedSpan]) -> String {
    let mut seen: BTreeMap<(SignalKind, String), ()> = BTreeMap::new();
    for s in spans {
        if s.tag != "untagged" {
            continue;
        }
        let Some(kind) = s.untagged_kind else { continue };
        if s.label.is_empty() || s.label == "(none)" {
            continue;
        }
        seen.insert((kind, s.label.clone()), ());
    }
    let mut out = String::new();
    for (kind, key) in seen.keys() {
        out.push_str("[[rule]]\n");
        out.push_str(&format!("tag = \"{key}\"\n"));
        out.push_str(&format!("{} = \"{key}\"\n\n", kind.as_str()));
    }
    out
}

pub fn tag_rollup(spans: &[TaggedSpan]) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, f64> = BTreeMap::new();
    for s in spans {
        *acc.entry(s.tag.clone()).or_default() += s.seconds();
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

    pub fn find_bucket(&self, prefix: &str) -> Result<Option<String>> {
        let mut ids = self.bucket_ids()?;
        ids.sort();
        Ok(ids.into_iter().find(|id| id.starts_with(prefix)))
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

/// Stock AFK-join query for one bucket (prefix-matched).
pub fn afk_joined_query(bucket_prefix: &str) -> String {
    format!(
        r#"events = flood(query_bucket(find_bucket("{bucket_prefix}")));
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

    let fetch = |prefix: &str| -> Result<Vec<AwEvent>> {
        if client.find_bucket(prefix)?.is_none() {
            return Ok(Vec::new());
        }
        client.query_events(&afk_joined_query(prefix), &start, &end)
    };

    let claude = fetch("aw-watcher-claude_")?;
    let neovim = fetch("aw-watcher-neovim_")?;
    let terminal = fetch("aw-watcher-terminal_")?;
    let window = fetch("aw-watcher-window_")?;
    let web = fetch("aw-watcher-web-")?;

    // Focused time from not-afk (stock events, not the join).
    let afk_raw = not_afk_events(client, &start, &end)?;
    let allowed = not_afk_spans(&afk_raw);
    let focused: f64 = allowed.iter().map(|s| s.seconds()).sum();

    let spans = coalesce_sessions(
        &tagged_timeline(
            &[
                (SourceKind::Claude, &claude),
                (SourceKind::Neovim, &neovim),
                (SourceKind::Terminal, &terminal),
                (SourceKind::Window, &window),
                (SourceKind::Web, &web),
            ],
            &allowed,
            &cfg,
        ),
        SESSION_GAP,
    );
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
pub fn ingest_spans(store: &mut Store, start: DateTime<Utc>, end: DateTime<Utc>, spans: &[TaggedSpan]) -> Result<usize> {
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
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Utc)
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
    fn overlap_counts_once_preferring_claude() {
        let allowed = vec![span("2026-09-30T12:00:00Z", "2026-09-30T13:00:00Z")];
        let cfg = TagsConfig::default();
        let window = vec![ev(
            "2026-09-30T12:00:00Z",
            3600.0,
            json!({"app": "kitty", "title": "x"}),
        )];
        let claude = vec![ev(
            "2026-09-30T12:00:00Z",
            3600.0,
            json!({"project": "champalimaud"}),
        )];
        let spans = tagged_timeline(
            &[(SourceKind::Claude, &claude), (SourceKind::Window, &window)],
            &allowed,
            &cfg,
        );
        let totals = tag_rollup(&spans);
        assert_eq!(totals["champalimaud"], 3600.0);
        assert_eq!(totals.values().sum::<f64>(), 3600.0);
    }

    #[test]
    fn load_missing_rules_is_empty() {
        let cfg = TagsConfig::load(std::path::Path::new("/nope/tags.toml")).unwrap();
        assert!(cfg.rule.is_empty());
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
    fn coalesce_browsing_is_one_block() {
        let mk = |start: &str, end: &str, label: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: "untagged".into(),
            task_uid: None,
            source: format!("aw:{start}:{label}"),
            label: label.into(),
            untagged_kind: Some(SignalKind::Domain),
        };
        let out = coalesce_sessions(
            &[
                mk(
                    "2026-10-02T12:00:00Z",
                    "2026-10-02T12:02:00Z",
                    "nytimes.com",
                ),
                mk(
                    "2026-10-02T12:02:00Z",
                    "2026-10-02T12:05:00Z",
                    "duckduckgo.com",
                ),
                mk(
                    "2026-10-02T12:05:30Z",
                    "2026-10-02T12:08:00Z",
                    "nytimes.com",
                ),
            ],
            SESSION_GAP,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start, ts("2026-10-02T12:00:00Z"));
        assert_eq!(out[0].end, ts("2026-10-02T12:08:00Z"));
        assert_eq!(out[0].label, "nytimes.com");
    }

    #[test]
    fn coalesce_splits_only_on_real_gaps() {
        let mk = |start: &str, end: &str, tag: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: tag.into(),
            task_uid: None,
            source: format!("aw:{start}:{tag}"),
            label: tag.into(),
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
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].start, ts("2026-10-02T12:00:00Z"));
        assert_eq!(out[0].end, ts("2026-10-02T12:30:00Z"));
        assert_eq!(out[0].label, "champalimaud");
        assert_eq!(out[1].label, "champalimaud");
    }

    #[test]
    fn coalesce_absorbs_web_blips_into_project() {
        let mk = |start: &str, end: &str, tag: &str, label: &str| TaggedSpan {
            start: ts(start),
            end: ts(end),
            tag: tag.into(),
            task_uid: None,
            source: format!("aw:{start}:{label}"),
            label: label.into(),
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
                mk(
                    "2026-10-02T12:10:00Z",
                    "2026-10-02T12:10:20Z",
                    "untagged",
                    "github.com",
                ),
                mk(
                    "2026-10-02T12:10:20Z",
                    "2026-10-02T12:15:00Z",
                    "champalimaud",
                    "champalimaud",
                ),
            ],
            SESSION_GAP,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "champalimaud");
        assert_eq!(out[0].start, ts("2026-10-02T12:00:00Z"));
        assert_eq!(out[0].end, ts("2026-10-02T12:15:00Z"));
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
