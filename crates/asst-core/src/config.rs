//! `~/.config/asst/config.toml`, where things live, and the app password.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use crate::github::FollowRename;

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

/// How long `Snooze…` snoozes before its chooser opens: the first Snooze
/// button's length, or the default's when there are no buttons. asstd
/// does it and the chooser says it, so both read it here.
pub fn first_snooze(lengths: &[u64]) -> u64 {
    lengths.first().copied().unwrap_or(DEFAULT_SNOOZE[0])
}

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
    /// What a list does when its linked repo is renamed on GitHub: `list`
    /// renames a list that is named exactly for the old repo, `off` only
    /// reports it. Either way the link follows the repo.
    pub follow_rename: FollowRename,
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
            follow_rename: FollowRename::List,
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

/// `~/.config` (or `$XDG_CONFIG_HOME`).
pub fn xdg_config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `~/.config/asst`.
pub fn config_dir() -> PathBuf {
    xdg_config_dir().join("asst")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// The window's own preferences, which the window alone writes.
pub fn window_path() -> PathBuf {
    config_dir().join("window.toml")
}

/// The lists (hrefs) the window has archived, for a tool that only reads
/// them: `asst projects` says which of its lists are put away. A missing
/// or unreadable file puts none away.
pub fn archived_in_window() -> Vec<String> {
    std::fs::read_to_string(window_path())
        .map(|text| archived_lists(&text))
        .unwrap_or_default()
}

/// The `archived` key of `window.toml`. The window writes the file whole and
/// owns its other keys, so nothing else is read.
pub fn archived_lists(text: &str) -> Vec<String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Window {
        archived: Vec<String>,
    }
    toml::from_str::<Window>(text)
        .map(|w| w.archived)
        .unwrap_or_default()
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
    fn a_list_follows_its_repo_unless_told_not_to() {
        let read = |s: &str| toml::from_str::<Config>(s).unwrap().follow_rename;
        assert_eq!(read(""), FollowRename::List, "the default");
        assert_eq!(read("follow_rename = \"list\"\n"), FollowRename::List);
        assert_eq!(read("follow_rename = \"off\"\n"), FollowRename::Off);
        assert!(toml::from_str::<Config>("follow_rename = \"on\"\n").is_err());
        let off = Config {
            follow_rename: FollowRename::Off,
            ..Config::default()
        };
        let text = toml::to_string_pretty(&off).unwrap();
        assert!(text.contains("follow_rename = \"off\""), "{text}");
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), off);
    }

    #[test]
    fn the_windows_archived_lists_are_read_and_nothing_else() {
        let text = "width = 1080\narchived = [\"/cal/a/\", \"/cal/b/\"]\n\
                    [views.x]\nsort = 1\n";
        assert_eq!(archived_lists(text), ["/cal/a/", "/cal/b/"]);
        assert!(archived_lists("").is_empty());
        assert!(archived_lists("width = 1080\n").is_empty());
        // A file that does not parse puts nothing away, and nothing fails
        assert!(archived_lists("archived = [").is_empty());
        assert!(archived_lists("archived = 3\n").is_empty());
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
