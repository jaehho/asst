# asst

Tasks and reminders on a CalDAV server (built against Nextcloud, interoperating with Apple Reminders), for a Linux desktop. A daemon keeps a local copy in sync and raises reminders; a CLI, a GTK window, a quick-add popup and a waybar module are its clients.

## Install

```sh
make install                       # live wrappers in ~/.local/bin, user unit, D-Bus services, desktop entry
systemctl --user enable --now asstd
asst login cloud.example.com       # opens the browser; approves an app password for asst
```

`make install` puts a **wrapper** at `~/.local/bin/{asst,asst-gtk,asstd}` (one script, three names). It runs the binary from the checkout that installed it right away and rebuilds in the background, so opening the window never waits on cargo. The next launch picks up source changes; `ASST_NO_REBUILD=1` skips that rebuild for a script that calls `asst` often. With no binary yet, a failed build refuses to start and points at `~/.local/state/asst-live.log`; `ASST_SRC` runs a different tree. `make test-live` pins that behavior.

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
asst settings [--inbox LIST|default] [--interval SECONDS] [--snooze MINUTES,…] [--alarm-at-due yes|no] [--alarm-before MINUTES] [--follow-rename list|off]
asst link [LIST] [OWNER/REPO] | asst unlink OWNER/REPO | asst projects | asst projects log [-n N] [--repo OWNER/REPO]
asst track ingest [--date YYYY-MM-DD] [--dry-run] | asst track tags | asst track suggest
```

IDs are the shortest unambiguous UID prefix shown in listings. `--json` works everywhere; `asst add --source kind:id` is idempotent, for tools that file tasks.

**ActivityWatch:** sensors live in `~/projects/aw-watchers`; raw rollups are stock AW (`aw` / webui). The **Day** sidebar view shows one day of tagged blocks and re-pulls ActivityWatch every minute while it is open (also on open, day step, and sync). `asst track ingest` (CLI, for tools) tags AFK-clipped spans (`~/.config/asst/tags.toml`) into `blocks` (`planned=0`); `suggest` prints rule stubs for untagged sources.

**GitHub issues:** `asst link <list> <owner/repo>` keeps a list and a repo's issues in step: title, notes (as the issue body), open/closed and priority (`P1`–`P3` labels) both ways; a task's other labels are left alone. `asst link <owner/repo>` does it with the list the repo is linked to, else the list named for it, making that if needed; a repo linked to one list is not moved to another (`asst unlink` it first); a renamed repo is followed (see Renaming a project). `asst link` shows the links, `asst unlink <owner/repo>` drops one. The window does the same in a list's New List / Edit List dialog: the Repository field shows the tie and takes `owner/repo` or a GitHub URL; clearing it unlinks. asstd uses `$GITHUB_TOKEN`, or the token `gh auth login` stored. A linked list shows a link mark in the sidebar (hover for when its issues were last read, or what went wrong). A task paired with an issue shows its number (`github #12` on its row, `#12` on its open card) and has Start work (its ⋮ menu, the arrow beside its list, or right-click): it runs Preferences → More options → Start work command (default `kitty --directory {dir} gh-co {number}`) in the repo's checkout under the Projects folder (`~/projects/<repo>`), and Open on GitHub. Those two rows are in Preferences only once a list is linked.

**Quick-add syntax** (CLI, popup and window): `#list`, `p1`…`p4`, dates (`today`, `tomorrow`, `fri`, `next week`, `in 3 days`, `sep 20`, `9/20`, `5pm`, `at 9`, `in 2h`, `tonight`, `this evening`, `tomorrow morning`, `midnight`), repeats (`every day`, `every weekday`, `every mon, thu`, `every 2 weeks`, `every 15th`), reminders (`!` at the due time, `!30m` before, `!9am` at a time). Preferences → More options → Read dates in titles off leaves dates and repeats in the title.

**Window** (`asst-gtk`):
- **Sidebar:** Inbox and Day tiles to start, with your lists below, each with a color ring. The other eight views (Today, Matrix, Scheduled, Completed, Tomorrow, Anytime, Repeating, All Tasks) stay a step away: Quick Find, Ctrl+I/T/U, and the menus below. The sidebar is edited on the sidebar, not in Preferences. Right-click a tile (or Menu or Shift+F10 on it) to hide it, add a hidden view back, or reset to Inbox and Day; Alt+Left/Right moves it. The … button beside Lists has the checklist of all ten views, Sort A-Z, Hide finished lists, Show task counts and New list. Each hide, reset and sort has its own Undo. A red dot for something overdue sits on Today, else Day, else Inbox, whichever is in the sidebar first; Day counts what is placed and what waits in its tray. Drag a list to move it (lists out of sight keep their places). Right-click one to edit it, copy it as Markdown, use it as the Inbox, archive it, delete its completed tasks, or delete it. The inbox list has no row while the Inbox tile shows, and can be neither archived nor deleted. An archived list leaves the sidebar, the views, the counts and the list pickers, but still syncs and rings; it waits under one Hidden lists row at the bottom of the sidebar (Unarchive and Delete list… are in its menu), and Quick Find finds it once you type part of its name. Hide finished lists (off to start) puts lists with nothing left to do there too; one returns by itself when a task lands in it.
- **Views:** Today splits off Overdue, with a Reschedule button for all of it (a date, or none). Matrix is Eisenhower's four boxes: p1–p2 counts as important, a due date on or before today as urgent, so every open task sits in Do, Schedule, Delegate or Drop. Each box is the same size and scrolls on its own, so a full Drop does not stretch the grid. Drag a task onto a box to move it there (priority steps to p2 or p3, the due date becomes today or goes away). Scheduled shows the coming week day by day, then month by month. Completed groups tasks by the day they were done, can leave lists out, and can delete them all. Each view remembers its sort order (descending too), priority filter, due-date filter (lists, All Tasks, Repeating) and whether to show completed tasks. Day plans the day on an hour grid (tracked time on the left, your plan on the right, a tray beside it of what is due by that day and not yet placed, with the tasks that have no date folded under No date); a day with planned blocks has a Carry button that moves them to the next day (to today from a day that is over), with Undo.
- **Tasks:** click one to open it in place: the row becomes a card with the title and notes to edit, and buttons for the date (with time and repeat), list, priority and reminders, while the other rows step back. Edits save as you go. Tab goes from the title to the notes, where Enter carries on a `- ` or `1.` list, Ctrl+Enter is done, and Ctrl+click opens a link. Ctrl+D and Ctrl+R open the date and reminders; ⋮ → Save as .ics writes the task to a file. Esc, Enter in the title, or a click beside the card closes it. Right-click a task for its menu, or drag it onto a list, Inbox, Today or Completed; near the top or bottom, the list scrolls. A task filed from a steno meeting has Open meeting in that menu and in ⋮: it shows the meeting in Steno, starting Steno first if it isn't running, and is there only when `steno` is installed. In a list sorted by custom order, dragging a task up or down reorders it. The order is saved as iOS's `X-APPLE-SORT-ORDER`. Checking a repeating task lights up its next date.
- **Lists:** + in the sidebar makes one (name, color, and an icon from a set of about a hundred, kept on this machine; the iPhone shows the list's color only). A list that is not there yet can also be made from the add card's `#word` suggestions, its list picker, or Move to.
- **Adding:** Add Task, or `a`, opens a card that takes quick-add syntax, with notes under the title and buttons for the date, priority, reminders and list. In Day it opens above the grid and adds to the Inbox, due the day on view. Typing `#` suggests lists, and offers to make one that doesn't exist; the list picker does too. Enter in the notes adds (Shift+Enter is a new line). Leaving the card with Esc, another view, or by closing the window adds what was typed (a toast offers Undo); the ✕ button throws it away. Text with only a date, list or priority in it is no task: Add Task reads `Add a title` and stays off, Enter does nothing, and leaving the card adds nothing and keeps the text for the next time it opens. Ctrl+V outside a text field opens it with what was copied: the first line as the title, the rest as notes. Preferences → More options → Default priority sets a priority for tasks added without one.
- **More:** Ctrl+F or `/` for Quick Find (Esc closes it): views (hidden ones too, and by other words, like `upcoming` or `no date`), lists, and tasks by their title or notes, completed ones too, with what matched in bold. `p1` to `p4` list tasks by priority. `v` or Ctrl+click to select several tasks, to complete, date, prioritize, move, copy or delete. A list's ⋮ menu has the list's menu and Select. Preferences is one page of what you set once: the default reminder and the Snooze buttons, completing after a moment, the first day of the week, running in the background and starting at login, and under More options reading dates in titles, a default priority, whether quick add starts in the last list, and for linked repos the projects folder and Start work command; the account comes last (first, until you sign in). Each change applies as you make it, so Esc loses nothing. How often asst checks the server is `asst settings --interval`, and the Inbox is Use as Inbox in a list's menu or `asst settings --inbox`. Rows slide in and fold away as tasks come and go; GTK's animation setting turns that off.
- **Tray:** an icon in the system tray (waybar's `tray`, KDE, XFCE, GNOME with the AppIndicator extension), with a dot while something is overdue. Click shows or hides the window, middle-click opens quick add, and the menu has Sync now and Quit. Closing the window leaves asst there; Ctrl+Q quits. Preferences → App turns this off (Run in background) and starts asst in the tray at login (Start at login).
- **Reminders** come from asstd as notifications with Complete, up to three Snooze buttons, and Snooze…, which opens a small chooser for any other time: in 15 minutes, 1 hour or 3 hours (each left out when a button already waits that long), this evening, tomorrow morning, next Monday, or a field that reads `2h`, `45m`, `5pm`, `tomorrow 8am`. Snooze… snoozes for the first button's length (10 minutes with none) before the chooser opens, so Esc, or never answering it, keeps that snooze. Preferences → Reminders → Snooze buttons picks the buttons from four sets (`10 min, 30 min, 1 h` to start, `15 min, 1 h`, `10 min, 1 h, 1 day`, `10 min`; a set made with `asst settings --snooze` shows as it was written), and says under the row what the buttons will read. Clicking a reminder opens its task in the window, starting the window if it isn't running. Preferences → Reminders → Default reminder sets when a task with a time rings by default: at the due time, as on the iPhone, or earlier.
- **Keys:** `j`/`k` or Down/Up move, `e` open, `x` complete, `dd` or Delete delete, `1`–`4` priority, `t`/`m`/`w` today/tomorrow/next week, `[`/`]` previous/next sidebar view and `{`/`}` previous/next list, Alt+1–9 the Nth tile, Ctrl+I/T/U Inbox/Today/Scheduled, Ctrl+1–9 the Nth list, Ctrl+PgUp/PgDn through the views then the lists, Tab into the sidebar to move with `j`/`k` (Esc goes back to the tasks, Alt+Left/Right and Alt+Up/Down move a tile or a list), `p` new list, `s` sync, Ctrl+V add from the clipboard, Ctrl+W close, Ctrl+Q quit, `?` all shortcuts.

**Start at login** writes an XDG autostart entry, which Hyprland, sway and niri don't run unless the session was started through systemd (uwsm). There, start it from the compositor's config, which Preferences then reports (Started by …) in place of the switch:

```lua
hl.exec_cmd("asst-gtk --background") -- in hl.on("hyprland.start", ...)
```

**Quick add** (`asst-gtk quick-add`): a layer-shell popup with the same card. Enter adds, Shift+Enter (or the Keep adding button, Ctrl+K) adds and stays open, Esc closes, and running it again closes it. It can start in the list it added to last (Preferences → More options → Quick add remembers the list). Bind it in Hyprland:

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

## Renaming a project

A linked repo is known by its GitHub id, which a rename leaves alone, so renaming the repo on GitHub is safe: asstd sees it on its next pass, moves the link and its issue pairs to the new name, and goes on syncing. What it does beyond that is small, and written down.

- **The list** is renamed too, once, only if asstd saw the rename happen and the list is named exactly for the old repo (`champalimaud` for `jaehho/champalimaud`; not `Champalimaud`, not `Lab work`), is not the inbox, can be written, and no other list already has the new name. The server gets one display-name change. The list keeps its place, color, icon and tasks, and the phone shows the new name once it fetches. At most three an hour. A desktop notification says which list was renamed to what and why, with the command that undoes it (`asst list rename lab champalimaud`); `asst projects log` keeps the row. `follow_rename = "off"` in `config.toml`, or `asst settings --follow-rename off`, leaves the list alone and says so there.
- **Everything else is reported, not done.** `asst projects` lists each link with what needs a look (`archived`, `renamed`, `unreachable`, `replaced`, `conflict`), the lists waiting for their new name, and, for a list still named for an old name that asstd did not rename (it did not see the rename happen, or `follow_rename` is off), the command that renames it (`asst list rename "champalimaud" "lab"`) and which of the two it was. It changes nothing. `asst projects log` is every change asstd made by itself, with its reason and the command that undoes it. `--json` on either is for other tools. Nothing is ever deleted, unlinked or archived for you: a repo archived on GitHub is only said to be, and a repo GitHub cannot find is "unreachable", never "gone".
- **Tracked time** keeps the old name until you move it. For days not yet ingested, add two rules to `~/.config/asst/tags.toml` and keep them (sessions started before the move go on saying the old name until they restart):

  ```toml
  [[rule]]
  project = "champalimaud"
  tag = "lab"

  [[rule]]
  cwd_prefix = "/home/you/projects/champalimaud"
  tag = "lab"
  ```

  For blocks already stored, relabel them in place: `asst track ingest` is not a relabel, since it recomputes the hours from ActivityWatch too. `asst projects` says how many blocks still carry a name the log saw change. The recipe (fish), with a backup and the ids kept for undo:

  ```fish
  set db ~/.local/state/asst/asst.db
  set old champalimaud
  set new lab
  sqlite3 $db ".backup '$db.pre-relabel'"
  sqlite3 -readonly $db "select count(*), round(sum(end-start)/3600000.0,2) from blocks where planned=0 and label='$old'"
  sqlite3 -readonly $db "select group_concat(id) from blocks where planned=0 and label='$old'" > $db.relabel-ids
  sqlite3 $db ".timeout 5000" "begin; update blocks set label='$new' where planned=0 and label='$old'; select changes(); commit;"
  sqlite3 -readonly $db "select label, count(*), round(sum(end-start)/3600000.0,2) from blocks where label in ('$old','$new') group by label"
  ```

  Run the update only when the first count is the one you expect (`asst projects` gives it for a name it knows). Undo with `update blocks set label='champalimaud' where id in (...)`, the ids being those in `$db.relabel-ids`. Planned blocks are not touched.
- **Left as they were:** ActivityWatch events, steno sessions and closed issues name the project as it was called then.

## Configure

`~/.config/asst/config.toml` (written by `asst login` and `asst settings`):

```toml
inbox = "Inbox"        # where tasks go when no #list is given
interval = 60          # seconds between checks for server changes
snooze = [10, 30, 60]  # minutes, a Snooze button each (up to three)
alarm_at_due = true    # a timed task gets a reminder, as on the iPhone
alarm_before = 0       # minutes before the due time that reminder rings
follow_rename = "list" # a renamed repo renames its exactly-named list; "off" only reports it
```

The cache is `~/.local/state/asst/asst.db`. A full re-sync brings back the lists and tasks, but not what only this file knows: which GitHub issue is which task (the links and their pairs), the log `asst projects log` reads, and snoozes. Back it up (`sqlite3 asst.db ".backup copy.db"`) before deleting it. The first time a build with repo ids opens an older file it keeps a copy beside it, `asst.db.pre-identity`, and never replaces or removes it.
