# asst design

Tasks and reminders on Nextcloud CalDAV for one setup: Arch + Hyprland laptop, iPhone (Apple Reminders), projects in `~/projects`.

## Shape

One Cargo workspace, app ID `dev.jaeho.Asst` (matches `dev.jaeho.Steno`).

| Piece | Role |
|---|---|
| `asst-core` | iCalendar, task model and edits, SQLite cache, CalDAV client, sync, quick-add parser, D-Bus API types. No UI. |
| `asstd` | systemd user service. Owns sync and reminders; everything else is its client over D-Bus (`dev.jaeho.Asst.Daemon`, interface `dev.jaeho.Asst1`, JSON arguments, see `api.rs`). |
| `asst` | CLI, including `asst bar` (waybar JSON stream) and `--json` everywhere. |
| `asst-gtk` | relm4 + libadwaita window (GApplication `dev.jaeho.Asst`, which the bus can start for its `open-task` action), and `asst-gtk quick-add`, a gtk4-layer-shell popup bound to `Super+Shift+A`. |

The daemon, not the window, keeps sync and reminders alive, so closing the window loses nothing. D-Bus activation starts `asstd` for the first client.

- **The window still lives in the tray**, as the desktop's other background apps do: started with `--background` at login, back with a click. A waybar module was proposed instead and turned down, because a tray icon works in any bar and is what every other app there does.
  - The icon is `ksni` (StatusNotifierItem and its dbusmenu over zbus). GTK4 has no tray, and libayatana-appindicator is GTK3-only.
  - A tray host looks icons up by name, so they are written to `$XDG_RUNTIME_DIR/asst/icons` with a theme index, without which waybar doesn't search the folder (steno learned this first).
  - Closing hides the window only while a tray shows the icon; with none, it quits rather than leave nothing to click. Hiding sends what the undo toasts were holding, since the toasts go with the window.
  - Start at login is steno's: a per-user XDG autostart entry, and a line in the compositor's config counts as on, left for the user to remove.
- The daemon's bus name is not the app ID, because GApplication claims its own ID on the session bus for single-instance handling.
- **A click on a reminder opens its task.** asstd calls `org.freedesktop.Application.ActivateAction("open-task", href)` on the window's app ID, passing on the notification server's activation token so the window may take focus. The window's D-Bus service file (`--gapplication-service`) lets the bus start it when it isn't running: started by the bus, it lives outside asstd's cgroup, so restarting asstd doesn't take the window along, as a child process would be.
- **Reminders ring by default** for a task with a time, as iOS gives one: at the due time as an absolute trigger (what iOS writes), or earlier as a relative one (`-PT30M`, like quick add's `!30m`), so it moves with the date. Notifications carry up to three Snooze buttons, and a Snooze… that asks the window for a chooser (`snooze-task`, started the way a click's `open-task` is) for the times those don't cover.
  - The chooser is a small layer popup like quick add's, so it shows over whatever is open without the window; in development builds it is a plain window, so the harness can keep it off screen. It offers a few presets (15 min, 1 h, 3 h, this evening, tomorrow morning, next Monday; the evening only when it is more than three hours off) and a field for a wait, a time of day or a date phrase, read by `snooze.rs` with quick add's date reader. It calls `Snooze(id, until)`; the daemon takes an open task and a time up to a year ahead, and stores it as it does a button's snooze (`snoozed.until`), so a notification's own button and the chooser end the same way.
  - Snooze… closes the notification like any button, so Esc in the chooser leaves the reminder unrepeated, as dismissing it would.
  - A reminder counts as fired only once the notification server has taken it. With none answering (swaync restarting, a login before it is up) the reminder stays due and the daemon tries again on its next pass, at most a minute later, until it is 12 hours old; a snooze that will not show is dropped 12 hours after it was due or asstd started, whichever is later. The journal says why once for each kind of failure, and again when the server is back.
- The quick-add popup is a layer surface, so it needs no Hyprland window rule. Its bind belongs in the user's `hyprland.lua` (the keybind cheatsheet reads binds from there).

## Window

Libadwaita dressed up, because the first plain libadwaita list felt bare:
- **Sidebar:** colored view tiles with counts, then lists with color rings.
- **Views:** a big title, then sections under a rule. Two exceptions to the plain list:
  - **Day** is one hour timeline with two lanes on a shared time axis: tracked ActivityWatch blocks on the left, plan slots on the right, and an unscheduled tray in a third column so it never steals the hour labels. One day cursor (prev/next/Today) and one zoom (Ctrl+scroll or Ctrl+=/−, kept in `window.toml`) cover both lanes. Plan slots stay window-local: the iPhone has no planned slot. Work left undone is carried by hand, never automatically: a day with planned blocks has a `Carry N to tomorrow` button (to today when the day is over, so a past day's work comes to now, never to another past day) that moves its blocks' slots, not the tasks' dates, to that day at the same times, with an Undo toast.
  - **Matrix** is four cards in a 2×2 (Do / Schedule / Delegate / Drop), from TickTick's Eisenhower view. Each card's list is a scroller capped at a fixed height, so the four stay equal without growing to the longest box. Placement is pure model: p1–p2 is important, due ≤ today is urgent. Dropping on a box edits only the axes on the wrong side (priority → p2/p3, due → today or none), never rewriting the task.
- **Tasks open in place:** the row grows into a card with the title, notes and a bar of buttons. A pane on the right came first; the card was preferred.
- **Pickers:** a date picker (suggestions, three weeks, time, repeat), plus priority, list and reminder pickers.
- **Also:** the task menu, the add card, Quick Find, selection, and colors and icons (GNOME Icon Development Kit, bundled as a GResource).
- **List icons:** a list wears a Lucide icon in its color instead of the ring, chosen in the list dialog and kept in `window.toml` by href, since CalDAV has nowhere to put one. Lucide draws with strokes, which GTK's symbolic recoloring fills into blobs, so `scripts/lucide-icons.py` fetches a pinned release and writes each icon as a filled outline; the picker offers that curated set.

- **One model, no widget state.** `model.rs` works out views, sections and counts from the daemon's data, so it is unit-tested. The window rebuilds a view's rows when what it shows changes, but not while a popover is open over them.
- **The editor and the add card outlive rebuilds.** Each is one widget moved into the new rows, keeping its text, cursor and the keyboard. The open task's own edits don't rebuild the view, and nothing does while its title or notes have the keyboard: taking the field away would end an input method's composition mid-word.
- **Animations are made per build** (`motion.rs`), since a rebuilt view has nothing that could move by itself.
  - Rows that are new since the last build of the same view are built closed and opened once on screen.
  - Rows that are gone keep their old widget and fold shut beside a neighbor that stayed.
  - The editor lands row-sized and grows a frame later, so its CSS transitions have a start to run from; closing shrinks it before the row comes back.
  - A rebuild for the daemon's news waits out an animation (220 ms), and anything done in the window rebuilds at once.
  - More than twelve rows at once (a sync, a filter) just appear.
- **Open tasks are fetched whole.** Views and counts are computed locally, so switching views makes no D-Bus call. Completed tasks (at most 500) are fetched only for views that show them.
- **Undo instead of confirmation.**
  - A checked task is struck through for a moment and can be unchecked.
  - A deleted one hides at once and goes when its Undo toast does, or before the window closes.
  - The add card is left with what was typed added, not lost (Esc, another view, closing the window; its ✕ discards), and a toast offers Undo. Enter in its notes adds; Shift+Enter is the new line. The notes' newline is stopped where it is inserted, after the input method has had the key, so Enter that only confirms a composition adds nothing.
  - Deleting a list asks first, because it takes its tasks on every device. So does deleting completed tasks, which the daemon does in one transaction, however many (the window only ever loads 500).
- **Lists are ordered and archived in the window:** `window.toml`, not the server's `calendar-order`, which shared and read-only lists couldn't take anyway. An archived list still syncs and its reminders still ring; it is only out of sight (sidebar, views, counts, the tray). Deleting a list forgets its place, icon and archive entry there once the server has taken the delete; a list merely missing from one response is never pruned. The ring beside a list is its color, not progress: these lists are long-term projects, so a completed share would only grow toward 100% and mean nothing.
- **Keys follow focus.** The vim keys act on the task cursor from the task column only. In a field or the open task they are typed; in the sidebar or on a button, Space, the arrows and the task letters (`x d t m w e l n 1-4`) go to the focused widget, so Space on a tile opens its view. Selecting, Space and `x` pick the task rather than finish it.
- **Notes stay plain text,** since the iPhone shows them as they are: Enter carries on `- `, `* `, `- [ ] ` and numbered lists, and links are drawn as links and open with Ctrl+click. No bold, headings or hidden markers, which would turn to clutter on the phone.
- **Small conveniences:** Quick Find reads notes and words for views; the add card has notes, reminders and `#` suggestions; Ctrl+V adds the clipboard; other rows step back while a task is open; a repeating task's new date lights up; a drag near the edge scrolls. Quick add remembers its last list in the state directory, apart from `window.toml`, which the window writes whole.
- **Window-only preferences** (sort, filters, sidebar views, completion delay, size, day-grid zoom) live in `window.toml` next to `config.toml`. The daemon's settings go through `Settings`/`SetSettings`.
- **Development builds** (`ASST_DEVEL=1`) use the ID `dev.jaeho.Asst.Devel` and export `drive`/`snapshot` app actions. `scripts/dev` uses them to check the window without focusing it.

## Sync

- Nextcloud is the source of truth. SQLite keeps, per task, the server's copy, the edits made here since, and the two combined (what is shown and sent).
- **Idle cost is one request a minute:** a PROPFIND on the calendar home returns every list's sync token. A changed token triggers an RFC 6578 sync-collection report (libdav lacks it; `caldav.rs` adds it), then a multiget of what changed.
- **Push first, then pull.** Every write carries `If-Match` (or `If-None-Match: *` for a new task). On 412 the server copy is fetched and the stored edits are re-applied on top of it, so both sides' changes survive.
- **Edits are operations, not diffs.** A lost response (write applied, answer never arrived) is detected by checking whether the server copy already shows the edit, so a repeating task is never advanced twice.
- **A full listing once a day** per changed list: Nextcloud prunes old change history without invalidating tokens that point into it.
- **Never rewrite a VTODO from scratch.** `ical.rs` round-trips byte for byte and edits touch only their own lines, so what other clients wrote survives.
- Skipped collections: Nextcloud's trash bin (`deleted-calendar`) and Deck boards.
- **List changes go to the server at once** (MKCOL, PROPPATCH, DELETE), not through the queue: making, renaming, recoloring or deleting a list needs a connection. A deleted list lands in Nextcloud's trash bin, and its tasks and unsent edits leave the cache with it.
- **Credentials:** Nextcloud Login Flow v2 creates an app password for asst alone, kept in the Secret Service (`oo7`). `ASST_PASSWORD` overrides it for tests.

## Apple Reminders interop

From decompiled iOS 18/26 ReminderKit (via research, not a device test):

- iOS writes DTSTART equal to DUE and ignores DTSTART otherwise, so asst moves DTSTART with DUE.
- Priority is 1/5/9, and "none" leaves the property out.
- Alarms carry `UID` and `X-WR-ALARMUID`; iOS keeps what it can match by UID. asst writes both on new alarms.
- A timed reminder on iOS comes with an alarm at that time, so asst adds one by default (`alarm_at_due`).
- iOS parks dateless relative alarms at `19760401T005545Z`. asst ignores that alarm; it reads proximity alarms as location alarms (X-APPLE-STRUCTURED-LOCATION: title, geo coordinates, radius) and asstd rings them on arrival via GeoClue, once per arrival — re-armed when the position leaves the radius. A location alarm asst wrote (placeholder trigger, `geo:…?u=` value, `X-TITLE`/`X-ADDRESS`) shows up on the iPhone as a proper arrival reminder (device-tested).
- **Completing a repeating task** advances the original (same UID, alarms moved, COUNT reduced) and leaves a completed copy with a new UID and no RRULE, as iOS does.
  - It moves one occurrence, even when that date has passed too: missed dates are not skipped. Moved alarms already in the past get ACKNOWLEDGED, so completing doesn't ring them.
- Survive an iOS edit: DESCRIPTION, RELATED-TO, unknown properties (so `X-ASST-SOURCE` is safe; CATEGORIES and URL are no longer read or written, and ride along).
- Not synced by iOS over CalDAV: subtasks, flags. iOS shows IN-PROCESS and CANCELLED as open. asst's URL (device-tested) didn't show on the phone either, so the field is gone; links live in the notes text.
- iOS fetches on its Fetch New Data schedule (no push from Nextcloud), so the phone lags.

## Features

**v1:** CalDAV lists as projects, title, notes (plain text, with lists that carry on and links), linked notes (files in Nextcloud Notes' folder), due date/time, recurrence (RRULE), reminders (VALARM → notifications with snooze/complete/open), priority p1–p4 (iOS high/medium/low = 1/5/9), Today (with overdue), Matrix (Eisenhower 2×2), Scheduled, Inbox (a Nextcloud list), a few smart filters, search, completed tasks, archived lists, location reminders (made here or on the iPhone, fired on arrival), quick-add syntax (`#project p1 !reminder`, natural-language dates, `every …`; see `quickadd.rs`), CLI, vim-style keys, waybar module, a Hyprland bind for quick add (a layer surface, so no window rules).

**Deferred (decide later):** subtasks (they cannot match the phone over CalDAV), spell checking, productivity stats, completion sound.

**Cut:** pinned tasks / Pinboard, Todoist, Deck, local-only projects, GNOME Online Accounts, Evolution calendars, calendar sync, Planner import, sections, deadlines, attachments, change history, board view, magic button, layout prefs, PDF export, backups, GNOME search provider, in-app theming, update check, donate, translations, Flatpak/portals.

## Integrations

- **Location reminders.** A VALARM with `X-APPLE-PROXIMITY:ARRIVE` and an `X-APPLE-STRUCTURED-LOCATION` (`geo:lat,lon?u=radius`, `X-TITLE`, `X-ADDRESS`): asst reads, keeps and writes the same shape the phone does, matching its own by UID. The reminder popover searches Nominatim (one request per search) and adds a place with a 100 m radius; asstd subscribes to GeoClue2 while any open task has one (and does not talk to it otherwise), and notifies on arrival. DTSTART is the phone's business: asst never reads or writes it.
  - **When this machine cannot tell where it is** (omnibook has no GPS and no working wifi source; #12) the failure is said, not silent: GeoClue that cannot be reached, or gives no position for two minutes, puts the reason in the daemon's status (`location`; `asst status` prints it and the reminder picker shows it under each place reminder) until a position arrives, and one notification names the reminders that cannot ring here and says the phone still rings them. It is sent once for each reminder (kept in `location-told` in the state directory), so a login never repeats it. There is no IP or timezone fallback on purpose: an alarm's radius is about 100 m, and a guess off by kilometres would ring in the wrong place or never.
- **Source links.** A task can carry where it came from (a steno meeting, a file) in `X-ASST-SOURCE`. `asst add --source <kind>:<id>` is idempotent on that key, so a re-run never duplicates.
- **steno (two-way, asst owns the state):**
  - After summarizing, steno calls `asst add --source steno:<session>/<key>` for each "You" item in the meeting's project list and stores the returned UID in `todos.json`.
  - Its checkbox calls `asst done`/`asst reopen`, and its view reads state back from asst, so checking either side shows on both (and on the phone).
  - Without asst running, steno falls back to its own `todos.json`.
  - This is a change in steno, not a file asst watches: two programs writing `todos.json` would race.
- **GitHub issues (two-way):** `asst link <list> <owner/repo>` ties a list to a repo's issues, one repo per list; nothing is linked implicitly. The window's list dialog has the same three moves (link, unlink, show the tie) as one Repository field; changing it to another repo unlinks the old one first, since the daemon holds one repo per list. `asst link <owner/repo>` uses the list the repo is already linked to, else the list named for it, and makes that when there is none. A repo linked to one list is never moved to another by a link call (the move would drop its pairs): unlink it first. Project tasks live in issues; the list keeps them in the window, quick add and on the phone.
  - Synced, both ways: the title, the notes (as the issue body), open/closed, and priority as `P1`–`P3` labels (p4/none = no label, an issue's other labels kept). Everything else — due dates, repeats, reminders — has no issue equivalent and stays on the CalDAV side. Pull requests are left out. An open issue with no task gets one, an open task with no issue gets one; on linking, an open issue and an open task with the same title are paired instead of duplicated. Closed issues and completed tasks nobody paired are left alone. A paired task shows its issue (`asst show --json`) built from the pair, not stored; the window prints its number after the origin on the row and at the end of the open card's title line.
  - Which issue is which task lives in the cache (`gh_pairs`), not as markers in issue bodies, along with what the two last agreed on (title, state, body, priority). Each pass is a three-way merge against that base, field by field: the side that moved wins, the issue when both did. A push that fails leaves the base, so the next pass finds the same difference and retries.
  - A task deleted, or moved to another list, closes its issue as not planned; an issue deleted or transferred completes its task (asked for by number first, since a listing right after a write can lag).
  - **A repo's identity is its numeric id** (decision #22), learned the first time asstd looks at it and never overwritten. `owner/repo`, the list's name and the checkout's directory are labels. A rename answers 301 with a redirect to the id, and asstd moves the link and its pairs to the new name (`gh_rename`, in one transaction) before anything else, so GitHub sync never waits on Nextcloud. "A rename is not an event for the user" holds for the link, which keys the list's href; it does not hold for the list's name on the phone, which is what the next two points are about.
  - **A list follows its repo only when asstd saw the rename happen** (the link already had the id) **and the list is named exactly, byte for byte, for the old repo.** It is the one thing asst does by itself that reaches other devices: one `displayname` PROPPATCH, the href unchanged, so tasks, pairs, snoozes and the window's order, icon and archive entry stay valid. Never the inbox or a read-only list, never onto a name another list has (in any case), never for an owner-only transfer, at most three an hour, and `follow_rename = "off"` makes it a report. A rename that could not be tried (the server out of reach, three already this hour) waits on the link, as a marker holding the old name and the day, for up to 30 days; a second rename while it waits keeps the first old name (the list is still called for that) and takes the newer day. A log row written before the PROPPATCH is the intent, so a crash in between is told truthfully on the next pass.
  - **Everything else is detected, reported, and left to a command the owner runs.** A rename found after the fact still moves the link, but the list is only proposed a new name. An archived or disabled repo is state `archived` with the day it was first seen; the list keeps syncing, and archiving it stays a decision in the window. A name made again by another repo is `replaced`: the pass is skipped, since nothing it holds is the project's and the old tasks must not be completed against a stranger's issues. Two links to one repo are a `conflict`. `asst projects` shows these, with the exact command for each fix; `asst projects log` is the log.
  - **A look that settles nothing changes nothing.** 304 without a record, 401, 403, 404, 410, 429, 5xx, a timeout and "offline" are no verdict: that link's pass is skipped for the round and nothing is written. A refusal that names a time is waited out, at most an hour. A 404 is never taken for a deletion (GitHub answers it for a repo the token cannot see). State met on first sight (the upgrade day, a restored database) is reported, never acted on.
  - **Nothing here deletes**: no unlink, list delete, task completion or un-archive, and no new href. The daemon never writes `window.toml`. A test reads the source for the words that delete. Every change asstd makes leaves a row in `gh_log` (the newest 500; no foreign key, so it outlives an unlink) with its reason and its undo command, and the one that reaches the phone also sends one notification. History is not rewritten: ActivityWatch events, steno sessions and closed issues keep the old name. `blocks.label` is a derived cache, relabeled by hand (README, "Renaming a project").
  - asstd polls at the sync interval, and after a change to tasks or links. Each pass starts by asking after the repo itself (`GET /repositories/{id}` with the last ETag; before the id is known, by name), and reads its issues only when that is the repo that was linked. The first page's ETag is kept too, so a quiet repo costs two 304s, which GitHub does not count. The token is `$GITHUB_TOKEN`, or `gh auth token`; without one, linked repos wait.
