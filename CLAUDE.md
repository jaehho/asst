# asst

Rust tasks app on Nextcloud CalDAV. Decisions and architecture: `DESIGN.md`.

- **Planify is a reference, never a dependency.** Its clone is `~/projects/forks/planify`; read it for CalDAV/iCal edge cases, re-implement in Rust, and log ported fixes in `UPSTREAM.md` (`scripts/upstream-review`).
- **Never rewrite a VTODO from scratch.** Patch the stored server copy so properties from iOS or other clients survive; `ical.rs` tests pin byte-for-byte round trips.
- **The daemon owns sync and reminders.** The GUI, CLI and quick add are D-Bus clients; none of them talk to Nextcloud.
- **Test against the real server only in a throwaway list.** `make nextcloud-test` creates and purges its own. The user's lists sync to their iPhone.
- **Gotchas live in `ISSUES.md`** (e.g. geoclue has no position source on omnibook).
- **Try changes in `scripts/dev`, never on the session bus.** asst is installed and running, and a plain `dbus-run-session` can still start the installed asstd with the real config and cache. The script redirects the bus's service lookup, and runs a dev asstd against a local Radicale.
- **Check the GUI without taking the screen.** `scripts/dev window` opens it on a hidden Hyprland workspace, unfocused. `scripts/dev drive`/`snap` click and capture through app actions (`devel.rs`); read the PNGs.
