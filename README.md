# asst

Tasks and reminders on a CalDAV server (built against Nextcloud, interoperating with Apple Reminders), for a Linux desktop. A daemon keeps a local copy in sync and raises reminders; a CLI, a GTK window, a quick-add popup and a waybar module are its clients.

## Install

```sh
make install                       # live wrappers in ~/.local/bin, user unit, D-Bus services, desktop entry
systemctl --user enable --now asstd
asst login cloud.example.com       # opens the browser; approves an app password for asst
```

`make install` puts a **wrapper** at `~/.local/bin/{asst,asst-gtk,asstd}` (one script, three names). It runs the binary from the checkout that installed it right away and rebuilds in the background, so opening the window never waits on cargo. The next launch picks up source changes. With no binary yet, a failed build refuses to start and points at `~/.local/state/asst-live.log`; `ASST_SRC` runs a different tree. `make test-live` pins that behavior.

`packaging/PKGBUILD` builds `asst-git` for Arch. Needs gtk4, libadwaita, gtk4-layer-shell, and a Secret Service (gnome-keyring, KeePassXC) for the app password.

## Use

```sh
asst                                   # overdue and due today
asst add Call the bank #errands tomorrow 5pm p1 !30m
asst add Stretch every day at 8am
asst upcoming | asst ls [LIST] | asst completed [LIST] [--clear] | asst find TEXT
asst done ID | asst edit ID --due "fri 9am" --repeat "every fri" | asst rm ID
asst show ID [--raw] | asst dup ID | asst open ID | asst sync | asst status
asst lists | asst list new NAME [--color '#rrggbb'] | asst list rename LIST NAME
asst list color LIST '#rrggbb' | asst list rm LIST [--yes]
asst settings [--inbox LIST|default] [--interval SECONDS] [--snooze MINUTES,…] [--alarm-at-due yes|no] [--alarm-before MINUTES]
asst track ingest [--date YYYY-MM-DD] [--dry-run] | asst track tags | asst track suggest
```

IDs are the shortest unambiguous UID prefix shown in listings. `--json` works everywhere; `asst add --source kind:id` is idempotent, for tools that file tasks.

**ActivityWatch:** sensors live in `~/projects/aw-watchers`; raw rollups are stock AW (`aw` / webui). The **Time** sidebar view shows one day of tagged blocks. `asst track ingest` (CLI, for tools) tags AFK-clipped spans (`~/.config/asst/tags.toml`) into `blocks` (`planned=0`); `suggest` prints rule stubs for untagged sources.

**GitHub issues:** `asst link <list> <owner/repo>` keeps a list and a repo's issues in step: title, notes (as the issue body), open/closed and priority (`P1`–`P3` labels) both ways; a task's other labels are left alone. `asst link <owner/repo>` does it with the list named for the repo, making it if needed; a renamed repo is followed. `asst link` shows the links, `asst unlink <owner/repo>` drops one. The window does the same in a list's New List / Edit List dialog: the Repository field shows the tie and takes `owner/repo` or a GitHub URL; clearing it unlinks. asstd uses `$GITHUB_TOKEN`, or the token `gh auth login` stored. A linked list shows a link mark in the sidebar (hover for when its issues were last read, or what went wrong). A task paired with an issue shows its number (`github #12` on its row, `#12` on its open card) and has Start work (its ⋮ menu, the arrow beside its list, or right-click): it runs Preferences → GitHub's command (default `kitty --directory {dir} gh-co {number}`) in the repo's checkout under the projects folder (`~/projects/<repo>`), and Open on GitHub.

**Quick-add syntax** (CLI, popup and window): `#list`, `p1`…`p4`, dates (`today`, `tomorrow`, `fri`, `next week`, `in 3 days`, `sep 20`, `9/20`, `5pm`, `at 9`, `in 2h`, `tonight`, `this evening`, `tomorrow morning`, `midnight`), repeats (`every day`, `every weekday`, `every mon, thu`, `every 2 weeks`, `every 15th`), reminders (`!` at the due time, `!30m` before, `!9am` at a time). Preferences → New Tasks → Read dates in titles off leaves dates and repeats in the title.

**Window** (`asst-gtk`):
- **Sidebar:** tiles for the views you choose, in the order you drag them to in Preferences: Inbox, Today (a red dot when something is overdue), Matrix, Scheduled and Completed to start, and Tomorrow, Anytime, Repeating and All Tasks. Right-click a tile to take it out. Below the tiles are your lists, each with a color ring. Drag a list to move it (or sort them by name in Preferences). Right-click a list to edit it, copy it as Markdown, archive it, delete its completed tasks, or delete it. An archived list leaves the sidebar, the views and the counts, but still syncs and rings; Preferences → Sidebar brings it back, and Quick Find still finds it.
- **Views:** Today splits off Overdue, with a Reschedule button for all of it (a date, or none). Matrix is Eisenhower's four boxes: p1–p2 counts as important, a due date on or before today as urgent, so every open task sits in Do, Schedule, Delegate or Drop. Each box is the same size and scrolls on its own, so a full Drop does not stretch the grid. Drag a task onto a box to move it there (priority steps to p2 or p3, the due date becomes today or goes away). Scheduled shows the coming week day by day, then month by month. Completed groups tasks by the day they were done, can leave lists out, and can delete them all. Each view remembers its sort order (descending too), priority filter, due-date filter (lists, All Tasks, Repeating) and whether to show completed tasks.
- **Tasks:** click one to open it in place: the row becomes a card with the title and notes to edit, and buttons for the date (with time and repeat), list, priority and reminders, while the other rows step back. Edits save as you go. Tab goes from the title to the notes, where Enter carries on a `- ` or `1.` list, Ctrl+Enter is done, and Ctrl+click opens a link. Ctrl+D and Ctrl+R open the date and reminders; ⋮ → Save as .ics writes the task to a file. Esc, Enter in the title, or a click beside the card closes it. Right-click a task for its menu, or drag it onto a list, Inbox, Today or Completed; near the top or bottom, the list scrolls. In a list sorted by custom order, dragging a task up or down reorders it. The order is saved as iOS's `X-APPLE-SORT-ORDER`. Checking a repeating task lights up its next date.
- **Lists:** + in the sidebar makes one (name, color, and an icon from a set of about a hundred, kept on this machine; the iPhone shows the list's color only). A list that is not there yet can also be made from the add card's `#word` suggestions, its list picker, or Move to.
- **Adding:** Add Task, or `a`, opens a card that takes quick-add syntax, with notes under the title and buttons for the date, priority, reminders and list. Typing `#` suggests lists, and offers to make one that doesn't exist; the list picker does too. Enter in the notes adds (Shift+Enter is a new line). Leaving the card with Esc, another view, or by closing the window adds what was typed (a toast offers Undo); the ✕ button throws it away. Ctrl+V outside a text field opens it with what was copied: the first line as the title, the rest as notes. Preferences → New Tasks sets a default priority.
- **More:** Ctrl+F or `/` for Quick Find: views (hidden ones too, and by other words, like `upcoming` or `no date`), lists, and tasks by their title or notes, completed ones too, with what matched in bold. `p1` to `p4` list tasks by priority. `v` or Ctrl+click to select several tasks, to complete, date, prioritize, move, copy or delete. A list's ⋮ menu has the list's menu and Select. Preferences hold the account, inbox, reminder, snooze, new task, window, background and sidebar settings. Rows slide in and fold away as tasks come and go; GTK's animation setting turns that off.
- **Tray:** an icon in the system tray (waybar's `tray`, KDE, XFCE, GNOME with the AppIndicator extension), with a dot while something is overdue. Click shows or hides the window, middle-click opens quick add, and the menu has Sync now and Quit. Closing the window leaves asst there; Ctrl+Q quits. Preferences → Background turns this off (Run in background) and starts asst in the tray at login (Start at login).
- **Reminders** come from asstd as notifications with Complete and up to three Snooze buttons (Preferences → Snooze). Clicking one opens its task in the window, starting the window if it isn't running. Preferences → Reminder sets when a task with a time rings by default: at the due time, as on the iPhone, or earlier.
- **Keys:** `j`/`k` move, `e` open, `x` complete, `dd` delete, `1`–`4` priority, `t`/`m`/`w` today/tomorrow/next week, `[`/`]` previous/next view, Ctrl+I/T/U Inbox/Today/Scheduled, Ctrl+1–9 lists, `p` new list, `s` sync, Ctrl+V add from the clipboard, Ctrl+W close, Ctrl+Q quit, `?` all shortcuts.

**Start at login** writes an XDG autostart entry, which Hyprland, sway and niri don't run unless the session was started through systemd (uwsm). There, start it from the compositor's config, which the switch then shows as on:

```lua
hl.exec_cmd("asst-gtk --background") -- in hl.on("hyprland.start", ...)
```

**Quick add** (`asst-gtk quick-add`): a layer-shell popup with the same card. Enter adds, Shift+Enter (or the Keep adding button, Ctrl+K) adds and stays open, Esc closes, and running it again closes it. It can start in the list it added to last (Preferences → New Tasks). Bind it in Hyprland:

```lua
hl.bind(mainMod .. " + SHIFT + A", hl.dsp.exec_cmd("asst-gtk quick-add")) -- quick add task
```


**Waybar:**

```jsonc
"custom/asst": {
  "exec": "asst bar",
  "return-type": "json",
  "format": "{}",
  "on-click": "asst-gtk"
}
```

Classes: `overdue`, `due`, `clear`, `offline`, `error`.

## Configure

`~/.config/asst/config.toml` (written by `asst login` and `asst settings`):

```toml
inbox = "Inbox"        # where tasks go when no #list is given
interval = 60          # seconds between checks for server changes
snooze = [10, 30, 60]  # minutes, a Snooze button each (up to three)
alarm_at_due = true    # a timed task gets a reminder, as on the iPhone
alarm_before = 0       # minutes before the due time that reminder rings
```

The cache is `~/.local/state/asst/asst.db`; deleting it only costs a full re-sync.
