//! Place search on Nominatim (OpenStreetMap), for location reminders: a name
//! in, coordinates out. Their usage policy asks for a user agent that names
//! the app, and for no more than a request a second; a search is one request.

use http::{Method, Request};
use serde::Serialize;
use serde::Deserialize;

use crate::caldav::{self, RemoteError};

/// A place a search found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    /// What Nominatim calls it, with the address around it.
    pub display_name: String,
    /// The short name it is known by.
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Deserialize)]
struct RawPlace {
    display_name: String,
    #[serde(default)]
    name: String,
    lat: String,
    lon: String,
}

const AGENT: &str = concat!("asst/", env!("CARGO_PKG_VERSION"), " (github.com/jaehho/asst)");

pub async fn search(query: &str) -> Result<Vec<Place>, RemoteError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "https://nominatim.openstreetmap.org/search?q={}&format=jsonv2&limit=5&addressdetails=0",
        urlencoding_lite(query)
    );
    let req = Request::builder()
        .method(Method::GET)
        .uri(url)
        .body(String::new())
        .map_err(|e| RemoteError::Protocol(e.to_string()))?;
    let (status, _, body) = caldav::send_as(req, AGENT).await?;
    if !status.is_success() {
        return Err(caldav::status_error(status));
    }
    let raw: Vec<RawPlace> = serde_json::from_slice(&body)
        .map_err(|e| RemoteError::Protocol(e.to_string()))?;
    Ok(raw
        .into_iter()
        .filter_map(|p| {
            let short = p.name.is_empty();
            Some(Place {
                name: if short {
                    p.display_name.split(',').next().unwrap_or("").trim().to_string()
                } else {
                    p.name
                },
                display_name: p.display_name,
                lat: p.lat.parse().ok()?,
                lon: p.lon.parse().ok()?,
            })
        })
        .collect())
}

/// The few characters of a query that must not go into a URL raw.
fn urlencoding_lite(query: &str) -> String {
    let mut out = String::new();
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_escape_but_stay_readable() {
        assert_eq!(urlencoding_lite("Main St 5, Springfield"), "Main+St+5%2C+Springfield");
        assert_eq!(urlencoding_lite("a.b~c_d-e"), "a.b~c_d-e");
    }
}
