//! `~/.config/asst/config.toml`, where things live, and the app password.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

pub const APP_ID: &str = "dev.jaeho.Asst";

/// The window's app ID. Development builds (`ASST_DEVEL`) have their own, so
/// a dev asstd opens tasks in the dev window, never the installed one.
pub fn gui_app_id() -> &'static str {
    if std::env::var_os("ASST_DEVEL").is_some() {
        "dev.jaeho.Asst.Devel"
    } else {
        APP_ID
    }
}

/// A reminder's Snooze buttons, in minutes.
pub const DEFAULT_SNOOZE: [u64; 3] = [10, 30, 60];
/// As many Snooze buttons as a notification has room for, beside Complete.
pub const MAX_SNOOZE: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Written by `asst login`.
    pub account: Option<AccountConfig>,
    /// The list a task goes to when none is named; the first list if unset.
    pub inbox: Option<String>,
    /// Seconds between checks for changes on the server.
    pub interval: u64,
    /// Minutes each of a reminder's Snooze buttons waits.
    #[serde(deserialize_with = "snooze_lengths")]
    pub snooze: Vec<u64>,
    /// Give a task with a due time a reminder, as the iPhone does.
    pub alarm_at_due: bool,
    /// Minutes before the due time that reminder rings.
    pub alarm_before: u32,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            account: None,
            inbox: None,
            interval: 60,
            snooze: DEFAULT_SNOOZE.to_vec(),
            alarm_at_due: true,
            alarm_before: 0,
        }
    }
}

/// A list of minutes, or one number: what `snooze` was when a reminder had a
/// single Snooze button. The old default becomes the new one; another length
/// keeps its button, beside an hour.
pub fn snooze_lengths<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lengths {
        One(u64),
        Several(Vec<u64>),
    }
    Ok(match Lengths::deserialize(d)? {
        Lengths::One(10) => DEFAULT_SNOOZE.to_vec(),
        Lengths::One(n) => {
            let mut v = vec![n, 60];
            v.sort_unstable();
            v.dedup();
            v
        }
        Lengths::Several(v) => v,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    /// The server as the user typed it, e.g. `https://cloud.example`.
    pub server: String,
    /// The DAV root, e.g. `https://cloud.example/remote.php/dav/`.
    pub dav_url: String,
    pub username: String,
    /// The calendar home, e.g. `/remote.php/dav/calendars/me/`.
    pub home: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Parse(PathBuf, toml::de::Error),
    #[error("keyring: {0}")]
    Keyring(String),
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback)
        })
}

pub fn config_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
        .join("asst")
        .join("config.toml")
}

/// systemd's StateDirectory= when run as the service, else the XDG path.
pub fn state_dir() -> PathBuf {
    std::env::var_os("STATE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg("XDG_STATE_HOME", ".local/state").join("asst"))
}

impl Config {
    pub fn load() -> Result<Config, ConfigError> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| ConfigError::Parse(path, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(ConfigError::Io(path, e)),
        }
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let path = config_path();
        let dir = path.parent().expect("config path has a parent");
        std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io(dir.to_path_buf(), e))?;
        let text = toml::to_string_pretty(self).expect("config serializes");
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| ConfigError::Io(tmp.clone(), e))?;
        std::fs::rename(&tmp, &path).map_err(|e| ConfigError::Io(path, e))
    }
}

fn attributes(account: &AccountConfig) -> [(&'static str, &str); 3] {
    [
        ("application", APP_ID),
        ("server", account.dav_url.as_str()),
        ("username", account.username.as_str()),
    ]
}

/// The app password: `$ASST_PASSWORD` if set (tests, headless), else the
/// Secret Service.
pub async fn password(account: &AccountConfig) -> Result<Option<String>, ConfigError> {
    if let Ok(p) = std::env::var("ASST_PASSWORD") {
        return Ok(Some(p));
    }
    let keyring = oo7::Keyring::new()
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))?;
    let items = keyring
        .search_items(&attributes(account))
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    if item.is_locked().await.unwrap_or(false) {
        item.unlock()
            .await
            .map_err(|e| ConfigError::Keyring(e.to_string()))?;
    }
    let secret = item
        .secret()
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))?;
    Ok(Some(
        String::from_utf8_lossy(secret.as_bytes()).into_owned(),
    ))
}

pub async fn store_password(account: &AccountConfig, password: &str) -> Result<(), ConfigError> {
    let keyring = oo7::Keyring::new()
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))?;
    let label = format!("asst: {} on {}", account.username, account.server);
    keyring
        .create_item(&label, &attributes(account), password, true)
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))
}

pub async fn forget_password(account: &AccountConfig) -> Result<(), ConfigError> {
    let keyring = oo7::Keyring::new()
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))?;
    keyring
        .delete(&attributes(account))
        .await
        .map_err(|e| ConfigError::Keyring(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_in_a_partial_file() {
        let c: Config = toml::from_str("inbox = \"Inbox\"\n").unwrap();
        assert_eq!(c.interval, 60);
        assert_eq!(c.inbox.as_deref(), Some("Inbox"));
        assert!(c.account.is_none());
        let round: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(round, c);
    }

    #[test]
    fn snooze_was_one_number() {
        let read = |s: &str| toml::from_str::<Config>(s).unwrap().snooze;
        assert_eq!(read("snooze = 10\n"), DEFAULT_SNOOZE);
        assert_eq!(read("snooze = 15\n"), [15, 60]);
        assert_eq!(read("snooze = 60\n"), [60]);
        assert_eq!(read("snooze = [5, 20]\n"), [5, 20]);
        assert_eq!(read(""), DEFAULT_SNOOZE);
    }
}
