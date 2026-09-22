# Gotchas

- **GeoClue on omnibook has no position source**: no GPS, and its WiFi positioning relied on Mozilla Location Service, dead upstream. `LocationUpdated` never fires, so location reminders cannot ring here even though the D-Bus flow works (asstd subscribes fine; geoclue's default config authorizes the `asst` desktop id with no conf edit). It also drops clients and idles out after 60 s; asstd retries every ≤10 min and logs `location reminders wait for GeoClue`. Checked 2026-09-21; if a source appears (USB GPS, a working provider), alarms should fire with no asst change.
