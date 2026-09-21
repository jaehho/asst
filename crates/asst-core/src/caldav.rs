//! The CalDAV side: libdav for the standard requests, plus the two it lacks
//! (a calendar listing with names and tokens, and RFC 6578 sync-collection).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use http::header::{AUTHORIZATION, HeaderValue, USER_AGENT};
use http::{Method, Request, Response, StatusCode, Uri};
use hyper::body::Incoming;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use libdav::CalDavClient;
use libdav::caldav::{FindCalendarHomeSet, GetCalendarResources};
use libdav::dav::{Delete, PutResource, RequestError, SetProperty, WebDavClient, WebDavError};
use libdav::encoding::normalise_percent_encoded;
use libdav::requests::{DavRequest, ParseResponseError, xml_content_type_header};
use tower_service::Service;

pub const USER_AGENT_VALUE: &str = concat!("asst/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(30);
const MULTIGET_BATCH: usize = 50;

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("offline: {0}")]
    Network(String),
    #[error("the server rejected the credentials")]
    Unauthorized,
    #[error("changed on the server since it was read")]
    Conflict,
    #[error("not found on the server")]
    NotFound,
    #[error("the sync token is no longer valid")]
    InvalidToken,
    #[error("the server answered {0}")]
    Status(u16),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

impl RemoteError {
    /// Worth stopping the whole sync for: nothing else will get through.
    pub fn is_fatal(&self) -> bool {
        matches!(self, RemoteError::Network(_) | RemoteError::Unauthorized)
    }
}

/// A calendar collection that can hold tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteList {
    pub href: String,
    pub name: String,
    pub color: Option<String>,
    pub order: Option<i64>,
    pub sync_token: Option<String>,
    pub ctag: Option<String>,
    pub writable: bool,
}

/// What changed in a list since a token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub token: Option<String>,
    /// (href, etag) of every resource added or changed.
    pub changed: Vec<(String, String)>,
    pub removed: Vec<String>,
    /// `changed` is the whole list, not a delta (there was no token).
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub href: String,
    pub etag: String,
    pub data: String,
}

#[derive(Clone)]
pub struct Account {
    /// The DAV root, e.g. `https://cloud.example/remote.php/dav/`.
    pub dav_url: String,
    pub username: String,
    pub password: String,
}

// ---------------------------------------------------------------------------
// HTTP client: hyper + rustls with the system roots, basic auth, a timeout.

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("{}", chain(.0))]
    Client(hyper_util::client::legacy::Error),
    #[error("timed out")]
    Timeout,
}

/// hyper's errors say "client error (Connect)" and keep the useful part
/// (refused, certificate, DNS) in their sources.
fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        let text = s.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        cur = s.source();
    }
    out
}

type HttpsClient = Client<HttpsConnector<HttpConnector>, String>;

fn https_client() -> Result<HttpsClient, RemoteError> {
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .map_err(|e| RemoteError::Network(format!("loading system certificates: {e}")))?
        .https_or_http()
        .enable_http1()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

/// A plain form POST, for the Nextcloud login flow. Returns status and body.
pub async fn post_form(url: &str, body: &str) -> Result<(u16, Vec<u8>), RemoteError> {
    let req = Request::builder()
        .method(Method::POST)
        .uri(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body.to_string())
        .map_err(|e| RemoteError::Protocol(e.to_string()))?;
    let (status, _, body) = send(req).await?;
    Ok((status.as_u16(), body))
}

/// One request with the user agent and the timeout, read to the end: the
/// status, the headers and the body.
pub async fn send(
    mut req: Request<String>,
) -> Result<(StatusCode, http::HeaderMap, Vec<u8>), RemoteError> {
    use http_body_util::BodyExt;
    let client = https_client()?;
    req.headers_mut()
        .insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    let resp = tokio::time::timeout(TIMEOUT, client.request(req))
        .await
        .map_err(|_| RemoteError::Network("timed out".into()))?
        .map_err(|e| RemoteError::Network(chain(&e)))?;
    let (parts, body) = resp.into_parts();
    let bytes = body
        .collect()
        .await
        .map_err(|e| RemoteError::Network(chain(&e)))?
        .to_bytes();
    Ok((parts.status, parts.headers, bytes.to_vec()))
}

#[derive(Clone)]
pub struct Authed {
    inner: Client<HttpsConnector<HttpConnector>, String>,
    auth: HeaderValue,
    agent: HeaderValue,
}

impl Authed {
    pub fn new(username: &str, password: &str) -> Result<Authed, RemoteError> {
        use base64_engine::encode;
        let mut auth = HeaderValue::from_str(&format!(
            "Basic {}",
            encode(&format!("{username}:{password}"))
        ))
        .map_err(|_| RemoteError::Protocol("credentials contain invalid characters".into()))?;
        auth.set_sensitive(true);
        Ok(Authed {
            inner: https_client()?,
            auth,
            agent: HeaderValue::from_static(USER_AGENT_VALUE),
        })
    }
}

impl Service<Request<String>> for Authed {
    type Response = Response<Incoming>;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Incoming>, HttpError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), HttpError>> {
        Service::poll_ready(&mut self.inner, cx).map_err(HttpError::Client)
    }

    fn call(&mut self, mut req: Request<String>) -> Self::Future {
        req.headers_mut().insert(AUTHORIZATION, self.auth.clone());
        req.headers_mut().insert(USER_AGENT, self.agent.clone());
        let fut = Service::call(&mut self.inner, req);
        Box::pin(async move {
            match tokio::time::timeout(TIMEOUT, fut).await {
                Ok(r) => r.map_err(HttpError::Client),
                Err(_) => Err(HttpError::Timeout),
            }
        })
    }
}

/// Just enough base64 for a Basic auth header.
mod base64_engine {
    pub fn encode(input: &str) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = input.as_bytes();
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn known_values() {
        assert_eq!(encode("jaeho:secret"), "amFlaG86c2VjcmV0");
        assert_eq!(encode("a"), "YQ==");
        assert_eq!(encode("ab"), "YWI=");
    }
}

// ---------------------------------------------------------------------------

pub struct CalDav {
    client: CalDavClient<Authed>,
    home: String,
}

fn map_err(e: WebDavError<HttpError>) -> RemoteError {
    match e {
        WebDavError::Request(RequestError::Client(HttpError::Timeout)) => {
            RemoteError::Network("timed out".into())
        }
        WebDavError::Request(RequestError::Client(e)) => RemoteError::Network(e.to_string()),
        WebDavError::Request(RequestError::Http(e)) => RemoteError::Network(chain(e.as_ref())),
        WebDavError::BadStatusCode(s) => status_error(s),
        WebDavError::Xml(e) => RemoteError::Protocol(e.to_string()),
        WebDavError::InvalidResponse(e) => RemoteError::Protocol(e.to_string()),
        WebDavError::InvalidInput(e) => RemoteError::Protocol(e.to_string()),
        WebDavError::PreconditionFailed(p) => {
            RemoteError::Protocol(format!("precondition failed: {p}"))
        }
    }
}

pub fn status_error(s: StatusCode) -> RemoteError {
    match s.as_u16() {
        401 => RemoteError::Unauthorized,
        404 | 410 => RemoteError::NotFound,
        412 => RemoteError::Conflict,
        n => RemoteError::Status(n),
    }
}

impl CalDav {
    /// A client for a known calendar home.
    pub fn new(account: &Account, home: &str) -> Result<CalDav, RemoteError> {
        let base: Uri = account
            .dav_url
            .parse()
            .map_err(|e| RemoteError::Protocol(format!("bad server URL: {e}")))?;
        let http = Authed::new(&account.username, &account.password)?;
        Ok(CalDav {
            client: CalDavClient::new(WebDavClient::new(base, http)),
            home: home.to_string(),
        })
    }

    /// Find the calendar home from the DAV root (principal → home set).
    pub async fn discover(account: &Account) -> Result<CalDav, RemoteError> {
        let mut dav = CalDav::new(account, "")?;
        let principal = dav
            .client
            .find_current_user_principal()
            .await
            .map_err(|e| match e {
                libdav::dav::FindCurrentUserPrincipalError::RequestError(e) => map_err(e),
                libdav::dav::FindCurrentUserPrincipalError::InvalidInput(e) => {
                    RemoteError::Protocol(e.to_string())
                }
            })?
            .ok_or_else(|| {
                RemoteError::Protocol("the server named no principal for this user".into())
            })?;
        let homes = dav
            .client
            .request(FindCalendarHomeSet::new(principal.path()))
            .await
            .map_err(map_err)?;
        let home = homes
            .home_sets
            .first()
            .ok_or_else(|| RemoteError::Protocol("the principal has no calendar home".into()))?;
        dav.home = home.path().to_string();
        Ok(dav)
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    pub async fn lists(&self) -> Result<Vec<RemoteList>, RemoteError> {
        self.client
            .request(ListCalendars { home: &self.home })
            .await
            .map_err(map_err)
    }

    pub async fn changes(&self, list: &str, token: Option<&str>) -> Result<Changes, RemoteError> {
        let mut all = Changes {
            complete: token.is_none(),
            ..Changes::default()
        };
        let mut token = token.map(str::to_string);
        // A server may truncate a big report (507) and expects another round.
        for _ in 0..100 {
            let page = self
                .client
                .request(SyncCollection {
                    href: list,
                    token: token.as_deref(),
                })
                .await
                .map_err(|e| match e {
                    SyncError::InvalidToken => RemoteError::InvalidToken,
                    SyncError::Dav(e) => map_err(e),
                })?;
            all.changed.extend(page.changes.changed);
            all.removed.extend(page.changes.removed);
            all.token = page.changes.token.clone();
            if !page.truncated || page.changes.token.is_none() || page.changes.token == token {
                break;
            }
            token = page.changes.token;
        }
        Ok(all)
    }

    pub async fn fetch(&self, list: &str, hrefs: &[String]) -> Result<Vec<Fetched>, RemoteError> {
        let mut out = Vec::new();
        for batch in hrefs.chunks(MULTIGET_BATCH) {
            let resp = self
                .client
                .request(GetCalendarResources::new(list).with_hrefs(batch))
                .await
                .map_err(map_err)?;
            for r in resp.resources {
                if let Ok(content) = r.content {
                    out.push(Fetched {
                        href: r.href,
                        etag: content.etag,
                        data: content.data,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Returns the new ETag when the server stored the body unchanged.
    pub async fn create(&self, href: &str, ics: &str) -> Result<Option<String>, RemoteError> {
        let req = PutResource::new(href).create(ics, "text/calendar; charset=utf-8");
        Ok(self.client.request(req).await.map_err(map_err)?.etag)
    }

    pub async fn update(
        &self,
        href: &str,
        ics: &str,
        etag: &str,
    ) -> Result<Option<String>, RemoteError> {
        let req = PutResource::new(href).update(ics, "text/calendar; charset=utf-8", etag);
        Ok(self.client.request(req).await.map_err(map_err)?.etag)
    }

    pub async fn delete(&self, href: &str, etag: &str) -> Result<(), RemoteError> {
        self.client
            .request(Delete::new(href).with_etag(etag))
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    /// A new task list under the calendar home; returns its href.
    pub async fn create_list(
        &self,
        slug: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<String, RemoteError> {
        let href = format!("{}{}/", self.home, slug);
        let components = [libdav::caldav::CalendarComponent::VTodo];
        let mut req = libdav::caldav::CreateCalendar::new(&href)
            .with_display_name(name)
            .with_components(&components);
        if let Some(c) = color {
            req = req.with_colour(c);
        }
        self.client.request(req).await.map_err(map_err)?;
        Ok(href)
    }

    /// Rename or recolor a list (a PROPPATCH per property). The color is
    /// written as `#rrggbb`, as Nextcloud's web UI and Planify do.
    pub async fn update_list(
        &self,
        href: &str,
        name: Option<&str>,
        color: Option<&str>,
    ) -> Result<(), RemoteError> {
        let changes = [
            (&libdav::names::DISPLAY_NAME, name),
            (&libdav::names::CALENDAR_COLOUR, color),
        ];
        for (property, value) in changes {
            if let Some(value) = value {
                self.client
                    .request(SetProperty::new(href, property, Some(value)))
                    .await
                    .map_err(map_err)?;
            }
        }
        Ok(())
    }

    /// Delete a whole list. Nextcloud keeps it in its trash bin unless
    /// `purge`, which skips the bin (what its web UI's "delete permanently" does).
    pub async fn delete_list(&self, href: &str, purge: bool) -> Result<(), RemoteError> {
        self.client
            .request(DeleteCollection { href, purge })
            .await
            .map_err(map_err)
    }
}

/// A new list's path segment: the name in lowercase ASCII letters, digits
/// and dashes, plus a random suffix so two lists of one name don't collide.
pub fn list_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if slug.len() >= 40 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let base = if slug.is_empty() { "list" } else { slug };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{base}-{}", &suffix[..6])
}

/// `#rrggbb` from `#rrggbb`, `rrggbb`, or Apple's `#rrggbbaa`.
pub fn parse_color(color: &str) -> Option<String> {
    let hex = color.trim().trim_start_matches('#');
    (matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex[..6].to_ascii_lowercase()))
}

struct DeleteCollection<'a> {
    href: &'a str,
    purge: bool,
}

impl DavRequest for DeleteCollection<'_> {
    type Response = ();
    type ParseError = ParseResponseError;
    type Error<E> = WebDavError<E>;

    fn prepare_request(&self, base_url: Uri) -> Result<Request<String>, http::Error> {
        let mut req = Request::builder()
            .method(Method::DELETE)
            .uri(libdav::dav::make_relative_url(base_url, self.href)?);
        if self.purge {
            req = req.header("X-NC-CalDAV-No-Trashbin", "1");
        }
        req.body(String::new())
    }

    fn parse_response(
        &self,
        parts: &http::response::Parts,
        _body: &[u8],
    ) -> Result<(), ParseResponseError> {
        if parts.status.is_success() {
            Ok(())
        } else {
            Err(ParseResponseError::BadStatusCode(parts.status))
        }
    }
}

// ---------------------------------------------------------------------------
// PROPFIND on the calendar home.

const NS_DAV: &str = "DAV:";
const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const NS_CS: &str = "http://calendarserver.org/ns/";
const NS_APPLE: &str = "http://apple.com/ns/ical/";

struct ListCalendars<'a> {
    home: &'a str,
}

impl DavRequest for ListCalendars<'_> {
    type Response = Vec<RemoteList>;
    type ParseError = ParseResponseError;
    type Error<E> = WebDavError<E>;

    fn prepare_request(&self, base_url: Uri) -> Result<Request<String>, http::Error> {
        let (ct, ctv) = xml_content_type_header();
        Request::builder()
            .method(Method::from_bytes(b"PROPFIND")?)
            .uri(libdav::dav::make_relative_url(base_url, self.home)?)
            .header("Depth", "1")
            .header(ct, ctv)
            .body(
                concat!(
                    r#"<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" "#,
                    r#"xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/">"#,
                    "<d:prop><d:resourcetype/><d:displayname/><a:calendar-color/><a:calendar-order/>",
                    "<c:supported-calendar-component-set/><d:sync-token/><cs:getctag/>",
                    "<d:current-user-privilege-set/></d:prop></d:propfind>"
                )
                .to_string(),
            )
    }

    fn parse_response(
        &self,
        parts: &http::response::Parts,
        body: &[u8],
    ) -> Result<Vec<RemoteList>, ParseResponseError> {
        if !parts.status.is_success() {
            return Err(ParseResponseError::BadStatusCode(parts.status));
        }
        parse_lists(std::str::from_utf8(body)?)
    }
}

fn child<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    ns: &str,
    name: &str,
) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|n| n.has_tag_name((ns, name)))
}

fn text_of(node: Option<roxmltree::Node>) -> Option<String> {
    node.and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn parse_lists(body: &str) -> Result<Vec<RemoteList>, ParseResponseError> {
    let doc = roxmltree::Document::parse(body)?;
    let mut lists = Vec::new();
    for response in doc
        .root_element()
        .children()
        .filter(|n| n.has_tag_name((NS_DAV, "response")))
    {
        let Some(href) = text_of(child(response, NS_DAV, "href")) else {
            continue;
        };
        // Only the propstat that succeeded carries values.
        let Some(prop) = response
            .children()
            .filter(|n| n.has_tag_name((NS_DAV, "propstat")))
            .find(|ps| text_of(child(*ps, NS_DAV, "status")).is_some_and(|s| s.contains(" 200")))
            .and_then(|ps| child(ps, NS_DAV, "prop"))
        else {
            continue;
        };
        let Some(rt) = child(prop, NS_DAV, "resourcetype") else {
            continue;
        };
        let is_calendar = rt
            .children()
            .any(|c| c.has_tag_name((NS_CALDAV, "calendar")));
        // Nextcloud lists calendars in its trash bin, and Deck boards when
        // Deck is installed; neither is a task list.
        let in_trash = rt
            .children()
            .any(|c| c.tag_name().name() == "deleted-calendar");
        if !is_calendar || in_trash || href.contains("app-generated--deck--board") {
            continue;
        }
        // Absent means every component type is allowed (RFC 4791 5.2.3).
        let holds_tasks = match child(prop, NS_CALDAV, "supported-calendar-component-set") {
            Some(set) => set.children().any(|c| {
                c.has_tag_name((NS_CALDAV, "comp"))
                    && c.attribute("name")
                        .is_some_and(|n| n.eq_ignore_ascii_case("VTODO"))
            }),
            None => true,
        };
        if !holds_tasks {
            continue;
        }
        let writable = match child(prop, NS_DAV, "current-user-privilege-set") {
            Some(set) => set.descendants().any(|p| {
                p.has_tag_name((NS_DAV, "write"))
                    || p.has_tag_name((NS_DAV, "write-content"))
                    || p.has_tag_name((NS_DAV, "all"))
            }),
            None => true,
        };
        let href = normalise_percent_encoded(&href)?.into_owned();
        let name = text_of(child(prop, NS_DAV, "displayname")).unwrap_or_else(|| {
            href.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string()
        });
        lists.push(RemoteList {
            href,
            name,
            // Apple writes #RRGGBBAA; the alpha is noise.
            color: text_of(child(prop, NS_APPLE, "calendar-color")).map(|c| {
                if c.len() == 9 && c.starts_with('#') {
                    c[..7].to_string()
                } else {
                    c
                }
            }),
            order: text_of(child(prop, NS_APPLE, "calendar-order")).and_then(|o| o.parse().ok()),
            sync_token: text_of(child(prop, NS_DAV, "sync-token")),
            ctag: text_of(child(prop, NS_CS, "getctag")),
            writable,
        });
    }
    Ok(lists)
}

// ---------------------------------------------------------------------------
// RFC 6578 sync-collection.

struct SyncCollection<'a> {
    href: &'a str,
    token: Option<&'a str>,
}

struct SyncPage {
    changes: Changes,
    truncated: bool,
}

#[derive(Debug, thiserror::Error)]
enum SyncError<E> {
    #[error("invalid sync token")]
    InvalidToken,
    #[error(transparent)]
    Dav(WebDavError<E>),
}

impl<E> From<http::Error> for SyncError<E> {
    fn from(e: http::Error) -> Self {
        SyncError::Dav(WebDavError::InvalidInput(e))
    }
}

impl<E> From<RequestError<E>> for SyncError<E> {
    fn from(e: RequestError<E>) -> Self {
        SyncError::Dav(WebDavError::Request(e))
    }
}

enum SyncParseError {
    InvalidToken,
    Parse(ParseResponseError),
}

impl<E> From<SyncParseError> for SyncError<E> {
    fn from(e: SyncParseError) -> Self {
        match e {
            SyncParseError::InvalidToken => SyncError::InvalidToken,
            SyncParseError::Parse(p) => SyncError::Dav(p.into()),
        }
    }
}

impl DavRequest for SyncCollection<'_> {
    type Response = SyncPage;
    type ParseError = SyncParseError;
    type Error<E> = SyncError<E>;

    fn prepare_request(&self, base_url: Uri) -> Result<Request<String>, http::Error> {
        let token = self.token.map(xml_escape).unwrap_or_default();
        let (ct, ctv) = xml_content_type_header();
        Request::builder()
            .method(Method::from_bytes(b"REPORT")?)
            .uri(libdav::dav::make_relative_url(base_url, self.href)?)
            .header(ct, ctv)
            .body(format!(
                r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#
            ))
    }

    fn parse_response(
        &self,
        parts: &http::response::Parts,
        body: &[u8],
    ) -> Result<SyncPage, SyncParseError> {
        let text = String::from_utf8_lossy(body);
        // RFC 6578 3.2: an unusable token is a 403 (sabre) or 409 carrying
        // DAV:valid-sync-token.
        if matches!(parts.status.as_u16(), 403 | 409) && text.contains("valid-sync-token") {
            return Err(SyncParseError::InvalidToken);
        }
        if !parts.status.is_success() {
            return Err(SyncParseError::Parse(ParseResponseError::BadStatusCode(
                parts.status,
            )));
        }
        parse_sync(&text, self.href).map_err(SyncParseError::Parse)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn parse_sync(body: &str, collection: &str) -> Result<SyncPage, ParseResponseError> {
    let doc = roxmltree::Document::parse(body)?;
    let root = doc.root_element();
    let collection = normalise_percent_encoded(collection)?.into_owned();
    let mut page = SyncPage {
        changes: Changes::default(),
        truncated: false,
    };
    page.changes.token = text_of(child(root, NS_DAV, "sync-token"));
    for response in root
        .children()
        .filter(|n| n.has_tag_name((NS_DAV, "response")))
    {
        let Some(href) = text_of(child(response, NS_DAV, "href")) else {
            continue;
        };
        let href = normalise_percent_encoded(&href)?.into_owned();
        let status = text_of(child(response, NS_DAV, "status"));
        if href == collection {
            if status.is_some_and(|s| s.contains(" 507")) {
                page.truncated = true;
            }
            continue;
        }
        if status
            .as_deref()
            .is_some_and(|s| s.contains(" 404") || s.contains(" 410"))
        {
            page.changes.removed.push(href);
            continue;
        }
        let etag = response
            .children()
            .filter(|n| n.has_tag_name((NS_DAV, "propstat")))
            .filter(|ps| text_of(child(*ps, NS_DAV, "status")).is_some_and(|s| s.contains(" 200")))
            .find_map(|ps| {
                text_of(child(ps, NS_DAV, "prop").and_then(|p| child(p, NS_DAV, "getetag")))
            });
        match etag {
            Some(etag) => page.changes.changed.push((href, etag)),
            // A member without an etag is not a resource asst can use (a
            // sub-collection); a server may also 404 the propstat instead.
            None => {
                let gone = response
                    .descendants()
                    .filter(|n| n.has_tag_name((NS_DAV, "status")))
                    .any(|s| s.text().is_some_and(|t| t.contains(" 404")));
                if gone && !href.ends_with('/') {
                    page.changes.removed.push(href);
                }
            }
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_path_safe_and_distinct() {
        let a = list_slug("Groceries & Home");
        assert!(a.starts_with("groceries-home-"), "{a}");
        assert_eq!(a.len(), "groceries-home-".len() + 6);
        assert_ne!(a, list_slug("Groceries & Home"));
        assert!(list_slug("장보기 🛒").starts_with("list-"));
        assert!(list_slug("  --Work-- ").starts_with("work-"));
        let long = list_slug(&"a".repeat(100));
        assert_eq!(long.len(), 47, "{long}");
    }

    #[test]
    fn colors_become_rrggbb() {
        assert_eq!(parse_color("#3C6DFF").as_deref(), Some("#3c6dff"));
        assert_eq!(parse_color("3c6dff").as_deref(), Some("#3c6dff"));
        assert_eq!(parse_color("#3C6DFFFF").as_deref(), Some("#3c6dff"));
        assert_eq!(parse_color("blue"), None);
        assert_eq!(parse_color("#abc"), None);
    }

    #[test]
    fn parses_nextcloud_calendar_home() {
        let body = r##"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:x1="http://apple.com/ns/ical/">
 <d:response><d:href>/remote.php/dav/calendars/jaeho/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/jaeho/personal/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Personal</d:displayname>
   <cal:supported-calendar-component-set><cal:comp name="VEVENT"/></cal:supported-calendar-component-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/jaeho/5f55a575-75e6-4d30-89b8-280d684e437c/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Inbox</d:displayname>
   <x1:calendar-color>#3C6DFFFF</x1:calendar-color><x1:calendar-order>2</x1:calendar-order>
   <cal:supported-calendar-component-set><cal:comp name="VTODO"/></cal:supported-calendar-component-set>
   <d:sync-token>http://sabre.io/ns/sync/6</d:sync-token><cs:getctag>http://sabre.io/ns/sync/6</cs:getctag>
   <d:current-user-privilege-set><d:privilege><d:write/></d:privilege><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><x1:missing/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/jaeho/shared_ro/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Family</d:displayname>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"##;
        let lists = parse_lists(body).unwrap();
        assert_eq!(lists.len(), 2);
        assert_eq!(
            lists[0],
            RemoteList {
                href: "/remote.php/dav/calendars/jaeho/5f55a575-75e6-4d30-89b8-280d684e437c/"
                    .into(),
                name: "Inbox".into(),
                color: Some("#3C6DFF".into()),
                order: Some(2),
                sync_token: Some("http://sabre.io/ns/sync/6".into()),
                ctag: Some("http://sabre.io/ns/sync/6".into()),
                writable: true,
            }
        );
        assert_eq!(lists[1].name, "Family");
        assert!(!lists[1].writable);
    }

    #[test]
    fn parses_a_sync_report() {
        let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
 <d:response><d:href>/remote.php/dav/calendars/jaeho/tasks/a%20b.ics</d:href>
  <d:propstat><d:prop><d:getetag>"e1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/jaeho/tasks/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
 <d:sync-token>http://sabre.io/ns/sync/9</d:sync-token>
</d:multistatus>"#;
        let page = parse_sync(body, "/remote.php/dav/calendars/jaeho/tasks/").unwrap();
        assert_eq!(
            page.changes.token.as_deref(),
            Some("http://sabre.io/ns/sync/9")
        );
        // Normalized the way libdav normalizes multiget hrefs, so the two match.
        assert_eq!(
            page.changes.changed,
            vec![(
                "/remote.php/dav/calendars/jaeho/tasks/a b.ics".to_string(),
                "\"e1\"".to_string()
            )]
        );
        assert_eq!(
            page.changes.removed,
            vec!["/remote.php/dav/calendars/jaeho/tasks/gone.ics".to_string()]
        );
        assert!(!page.truncated);
    }

    #[test]
    fn invalid_token_is_recognized() {
        let req = SyncCollection {
            href: "/cal/",
            token: Some("http://sabre.io/ns/sync/1"),
        };
        let resp = http::Response::builder().status(403).body(()).unwrap();
        let (parts, ()) = resp.into_parts();
        let body = br#"<?xml version="1.0"?><d:error xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns"><d:valid-sync-token/><s:message>Invalid or unknown sync token</s:message></d:error>"#;
        assert!(matches!(
            req.parse_response(&parts, body),
            Err(SyncParseError::InvalidToken)
        ));
    }
}
