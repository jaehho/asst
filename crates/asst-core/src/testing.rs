//! A fake GitHub for tests: a listener on the loopback interface that
//! answers canned replies and keeps every request it saw, so a test can
//! assert what the client was told and what it asked. `GitHub::with_base`
//! points a real client at it, so the real request code runs. It speaks just
//! enough HTTP/1.1 for hyper: one request per connection, then close.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A request as the fake received it. Header names are lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path and query as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    /// The target without its query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// What the fake answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Take the request and never answer it.
    pub hang: bool,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Reply {
        Reply {
            status,
            headers: vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            )],
            body: body.into(),
            hang: false,
        }
    }

    /// No body: a 304, a bare 500.
    pub fn status(status: u16) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: String::new(),
            hang: false,
        }
    }

    /// Read the request, then say nothing: the client waits.
    pub fn hang() -> Reply {
        Reply {
            hang: true,
            ..Reply::status(200)
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }
}

type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

struct Route {
    method: String,
    path: String,
    handler: Handler,
}

#[derive(Default)]
struct Shared {
    routes: Mutex<Vec<Route>>,
    seen: Mutex<Vec<Seen>>,
}

impl Shared {
    fn answer(&self, req: Seen) -> Reply {
        let handler = self
            .routes
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.method == req.method && r.path == req.path())
            .map(|r| r.handler.clone());
        let reply = match handler {
            Some(h) => h(&req),
            None => Reply::json(404, r#"{"message":"Not Found","status":"404"}"#),
        };
        // Kept before the reply goes out: a client that has its answer
        // can count on the request being listed.
        self.seen.lock().unwrap().push(req);
        reply
    }
}

pub struct FakeGitHub {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl FakeGitHub {
    /// Listen on a free loopback port. A path with no route answers 404.
    pub async fn start() -> FakeGitHub {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn({
            let shared = shared.clone();
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, shared.clone()));
                }
            }
        });
        FakeGitHub { addr, shared, task }
    }

    /// Where `GitHub::with_base` points: no trailing slash.
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A base on a port nobody listens on: the connection is refused.
    pub async fn refused_base() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        drop(listener);
        format!("http://{addr}")
    }

    /// Answer `method path` (no query) with whatever `handler` makes of the
    /// request. A route for the same method and path is replaced.
    pub fn on(
        &self,
        method: &str,
        path: &str,
        handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static,
    ) {
        let mut routes = self.shared.routes.lock().unwrap();
        routes.retain(|r| !(r.method == method && r.path == path));
        routes.push(Route {
            method: method.into(),
            path: path.into(),
            handler: Arc::new(handler),
        });
    }

    /// Always the same reply.
    pub fn reply(&self, method: &str, path: &str, reply: Reply) {
        self.on(method, path, move |_| reply.clone());
    }

    /// The replies in turn; the last one repeats.
    pub fn script(&self, method: &str, path: &str, replies: Vec<Reply>) {
        assert!(!replies.is_empty(), "a script needs a reply");
        let next = AtomicUsize::new(0);
        self.on(method, path, move |_| {
            let at = next.fetch_add(1, Ordering::SeqCst).min(replies.len() - 1);
            replies[at].clone()
        });
    }

    /// A GET that GitHub would give an ETag: 200 with `body`, or a bare 304
    /// when the request's `If-None-Match` is `etag`.
    pub fn etagged(&self, path: &str, etag: &str, body: &str) {
        let (etag, body) = (etag.to_string(), body.to_string());
        self.on("GET", path, move |req| {
            if req.header("if-none-match") == Some(etag.as_str()) {
                Reply::status(304).header("etag", &etag)
            } else {
                Reply::json(200, &body).header("etag", &etag)
            }
        });
    }

    /// Every request so far, oldest first.
    pub fn seen(&self) -> Vec<Seen> {
        self.shared.seen.lock().unwrap().clone()
    }

    /// How many requests named `path` (query ignored), with any method.
    pub fn count(&self, path: &str) -> usize {
        self.seen().iter().filter(|r| r.path() == path).count()
    }
}

impl Drop for FakeGitHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const MAX_HEAD: usize = 64 * 1024;

async fn serve(mut stream: TcpStream, shared: Arc<Shared>) {
    let Some(req) = read_request(&mut stream).await else {
        return;
    };
    let reply = shared.answer(req);
    if reply.hang {
        // Until the client gives up and the connection drops.
        let _ = stream.read(&mut [0u8; 1]).await;
        return;
    }
    let mut out = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
    for (name, value) in &reply.headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    // A 304 has no body, whatever the handler gave it.
    let body = if reply.status == 304 { "" } else { &reply.body };
    out.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ));
    let _ = stream.write_all(out.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Seen {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        301 => "Moved Permanently",
        304 => "Not Modified",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
