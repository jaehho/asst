//! Signing in: Nextcloud's Login Flow v2 gives asst its own app password,
//! revocable on its own under Settings → Security.

use serde::Deserialize;

use crate::caldav::{Account, CalDav, RemoteError, post_form};
use crate::config::AccountConfig;

pub struct Flow {
    /// Open this in the browser.
    pub login_url: String,
    endpoint: String,
    token: String,
}

pub struct Granted {
    pub server: String,
    pub login_name: String,
    pub app_password: String,
}

/// `cloud.example`, `https://cloud.example/`, or a pasted DAV URL → `https://cloud.example`.
pub fn normalize_server(input: &str) -> String {
    let mut s = input.trim().trim_end_matches('/').to_string();
    if !s.starts_with("http://") && !s.starts_with("https://") {
        s = format!("https://{s}");
    }
    for tail in ["/remote.php/dav", "/remote.php/webdav", "/index.php"] {
        if let Some(at) = s.find(tail) {
            s.truncate(at);
        }
    }
    s.trim_end_matches('/').to_string()
}

#[derive(Deserialize)]
struct StartResponse {
    poll: Poll,
    login: String,
}

#[derive(Deserialize)]
struct Poll {
    token: String,
    endpoint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PollResponse {
    server: String,
    login_name: String,
    app_password: String,
}

pub async fn start(server: &str) -> Result<Flow, RemoteError> {
    let server = normalize_server(server);
    // Servers with pretty URLs answer both; some only the second.
    let mut last = RemoteError::Protocol("no login flow".into());
    for path in ["/index.php/login/v2", "/login/v2"] {
        match post_form(&format!("{server}{path}"), "").await {
            Ok((200, body)) => {
                let r: StartResponse = serde_json::from_slice(&body)
                    .map_err(|e| RemoteError::Protocol(format!("login flow answer: {e}")))?;
                return Ok(Flow {
                    login_url: r.login,
                    endpoint: r.poll.endpoint,
                    token: r.poll.token,
                });
            }
            Ok((status, _)) => last = RemoteError::Status(status),
            Err(e) => return Err(e),
        }
    }
    Err(match last {
        RemoteError::Status(404) => {
            RemoteError::Protocol(format!("{server} does not look like a Nextcloud server"))
        }
        other => other,
    })
}

/// `None` until the user has approved it in the browser.
pub async fn poll(flow: &Flow) -> Result<Option<Granted>, RemoteError> {
    let body = format!("token={}", form_escape(&flow.token));
    match post_form(&flow.endpoint, &body).await? {
        (200, body) => {
            let r: PollResponse = serde_json::from_slice(&body)
                .map_err(|e| RemoteError::Protocol(format!("login poll answer: {e}")))?;
            Ok(Some(Granted {
                server: r.server,
                login_name: r.login_name,
                app_password: r.app_password,
            }))
        }
        (404, _) => Ok(None),
        (status, _) => Err(RemoteError::Status(status)),
    }
}

fn form_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Check the credentials and find the calendar home.
pub async fn account(
    server: &str,
    username: &str,
    password: &str,
) -> Result<AccountConfig, RemoteError> {
    let server = normalize_server(server);
    let dav_url = format!("{server}/remote.php/dav/");
    let dav = CalDav::discover(&Account {
        dav_url: dav_url.clone(),
        username: username.into(),
        password: password.into(),
    })
    .await?;
    Ok(AccountConfig {
        server,
        dav_url,
        username: username.into(),
        home: dav.home().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_forms() {
        assert_eq!(normalize_server("cloud.example"), "https://cloud.example");
        assert_eq!(
            normalize_server("https://cloud.example/"),
            "https://cloud.example"
        );
        assert_eq!(
            normalize_server("https://cloud.example/nc/remote.php/dav/calendars/me/"),
            "https://cloud.example/nc"
        );
        assert_eq!(
            normalize_server("http://10.0.0.2:8080/index.php/apps/tasks"),
            "http://10.0.0.2:8080"
        );
        assert_eq!(form_escape("a+b/c="), "a%2Bb%2Fc%3D");
    }
}
