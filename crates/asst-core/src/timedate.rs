//! The machine's time zone changing under a running program (#34).
//! `timedatectl set-timezone` goes through timedated, which says so with
//! `PropertiesChanged` on the system bus; the program then reads its zone
//! again with `time::local_zone`, as a restart would.

use zbus::fdo::{PropertiesChangedArgs, PropertiesChangedStream, PropertiesProxy};
use zbus::proxy::CacheProperties;

const SERVICE: &str = "org.freedesktop.timedate1";
const PATH: &str = "/org/freedesktop/timedate1";

/// What timedated says changed. The bus starts timedated for a call and
/// it quits when idle, so nothing is asked of it here, which would start
/// it: the stream follows whoever holds its name when a change comes.
pub async fn changes(system: &zbus::Connection) -> zbus::Result<PropertiesChangedStream> {
    let proxy = PropertiesProxy::builder(system)
        .destination(SERVICE)?
        .path(PATH)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    proxy
        .receive_properties_changed_with_args(&[(0, SERVICE)])
        .await
}

/// Whether a change timedated announced moves the time zone, rather than
/// the RTC or NTP setting.
pub fn moves_zone(change: &PropertiesChangedArgs<'_>) -> bool {
    change.interface_name() == SERVICE
        && (change.changed_properties().contains_key("Timezone")
            || change.invalidated_properties().contains(&"Timezone"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::fdo::PropertiesChanged;
    use zbus::zvariant::Value;

    use super::*;

    /// A `PropertiesChanged` as timedated sends it, through zbus's own
    /// reading of the message.
    fn signal(interface: &str, changed: &[(&str, Value<'_>)], invalidated: &[&str]) -> bool {
        let changed: HashMap<&str, &Value<'_>> = changed.iter().map(|(k, v)| (*k, v)).collect();
        let msg =
            zbus::Message::signal(PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged")
                .unwrap()
                .build(&(interface, changed, invalidated))
                .unwrap();
        let signal = PropertiesChanged::from_message(msg).unwrap();
        moves_zone(&signal.args().unwrap())
    }

    #[test]
    fn set_timezone_moves_the_zone() {
        let zone = Value::from("Europe/Lisbon");
        assert!(signal(SERVICE, &[("Timezone", zone)], &[]));
        assert!(
            signal(SERVICE, &[], &["Timezone"]),
            "a property only invalidated is read again too"
        );
    }

    #[test]
    fn other_changes_leave_it() {
        assert!(!signal(SERVICE, &[("LocalRTC", Value::from(true))], &[]));
        assert!(!signal(SERVICE, &[("NTP", Value::from(false))], &[]));
        let zone = Value::from("Europe/Lisbon");
        assert!(
            !signal("org.freedesktop.locale1", &[("Timezone", zone)], &[]),
            "another interface on the object"
        );
    }
}
