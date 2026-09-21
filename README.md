# asst

Tasks and reminders on a CalDAV server (built against Nextcloud, interoperating with Apple Reminders), for a Linux desktop. A daemon keeps a local copy in sync and raises reminders; a CLI, a GTK window, a quick-add popup and a waybar module are its clients.

## Install

```sh
make install                       # ~/.local/bin, user unit, D-Bus services, desktop entry
systemctl --user enable --now asstd
asst login cloud.example.com       # opens the browser; approves an app password for asst
```

`packaging/PKGBUILD` builds `asst-git` for Arch. Needs gtk4, libadwaita, gtk4-layer-shell, and a Secret Service (gnome-keyring, KeePassXC) for the app password.

## Use

```sh
asst                                   # overdue and due today
asst add Call the bank #errands tomorrow 5pm p1 !30m
asst add Stretch every day at 8am
asst upcoming | asst ls [LIST] | asst completed [LIST] [--clear] | asst find TEXT
asst done ID | asst edit ID --due "fri 9am" --repeat "every fri" | asst rm ID
asst show ID [--raw] | asst dup ID | asst open ID | asst sync | asst status
asst attach ID NOTE… | asst attach ID --new [--title T] | asst detach ID NOTE… | asst ls --attached NOTE
asst lists | asst list new NAME [--color '#rrggbb'] | asst list rename LIST NAME
asst list color LIST '#rrggbb' | asst list rm LIST [--yes]
asst settings [--inbox LIST|default] [--interval SECONDS] [--snooze MINUTES,…] [--alarm-at-due yes|no] [--alarm-before MINUTES] [--notes FOLDER|default]
```

IDs are the shortest unambiguous UID prefix shown in listings. `--json` works everywhere; `asst add --source kind:id` is idempotent, for tools that file tasks.

**Linked notes:** a task can link to notes in Nextcloud Notes' folder on this computer (`~/Nextcloud/Notes`, or Preferences → Notes). `asst attach` links files in it, `--new` makes a note named after the task and prints its path, `asst add … --attach NOTE` links one to a new task, and `asst ls --attached NOTE` lists the tasks linking a note. Links follow a note that is renamed or moved within the folder while asstd runs.

**Quick-add syntax** (CLI, popup and window): `#list`, `p1`…`p4`, dates (`today`, `tomorrow`, `fri`, `next week`, `in 3 days`, `sep 20`, `9/20`, `5pm`, `at 9`, `in 2h`, `tonight`, `this evening`, `tomorrow morning`, `midnight`), repeats (`every day`, `every weekday`, `every mon, thu`, `every 2 weeks`, `every 15th`), reminders (`!` at the due time, `!30m` before, `!9am` at a time). Preferences → New Tasks → Read dates in titles off leaves dates and repeats in the title.

**Window** (`asst-gtk`), laid out after Planify:
- **Sidebar:** tiles for the views you choose, in the order you drag them to in Preferences: Inbox, Today (a red dot when something is overdue), Scheduled and Completed to start, and Tomorrow, Anytime, Repeating and All Tasks. Right-click a tile to take it out. Below the tiles are your lists, each ring filled as far as its tasks are done. Drag a list to move it (or sort them by name in Preferences). Right-click a list to edit it, copy it as Markdown, archive it, delete its completed tasks, or delete it. An archived list leaves the sidebar, the views and the counts, but still syncs and rings; Preferences → Sidebar brings it back, and Quick Find still finds it.
- **Views:** Today splits off Overdue, with a Reschedule button for all of it (a date, or none). Scheduled shows the coming week day by day, then month by month. Completed groups tasks by the day they were done, can leave lists out, and can delete them all. Each view remembers its sort order (descending too), priority filter, due-date filter (lists, All Tasks, Repeating) and whether to show completed tasks.
- **Tasks:** click one to open it in place, as Planify does: the row becomes a card with the title and notes to edit, and buttons for the date (with time and repeat), list, link, priority and reminders, while the other rows step back. Edits save as you go. Tab goes from the title to the notes, where Enter carries on a `- ` or `1.` list and Ctrl+click opens a link. Ctrl+D and Ctrl+R open the date and reminders; ⋮ → Save as .ics writes the task to a file. The paperclip links a note from the notes folder, or makes a new one and opens it; linked notes sit under the task's notes, and a click opens one. Esc, Enter in the title, or a click beside the card closes it. Right-click a task for its menu, or drag it onto a list, Inbox, Today or Completed; near the top or bottom, the list scrolls. In a list sorted by custom order, dragging a task up or down reorders it. The order is saved as iOS's `X-APPLE-SORT-ORDER`. Checking a repeating task lights up its next date.
- **Adding:** Add Task, or `a`, opens a card that takes quick-add syntax, with notes under the title and buttons for the date, priority, reminders and list. Typing `#` suggests lists. Ctrl+V outside a text field opens it with what was copied: the first line as the title, the rest as notes. Preferences → New Tasks sets a default priority.
- **More:** Ctrl+F or `/` for Quick Find: views (hidden ones too, and by other words, like `upcoming` or `no date`), lists, and tasks by their title or notes, completed ones too, with what matched in bold. `p1` to `p4` list tasks by priority. `v` or Ctrl+click to select several tasks, to complete, date, prioritize, move, copy or delete. A list's ⋮ menu has the list's menu and Select. Preferences hold the account, inbox, reminder, snooze, notes folder, new task, window, background and sidebar settings. Quick Find also finds tasks by the names of their linked notes. Rows slide in and fold away as tasks come and go; GTK's animation setting turns that off.
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

`asst-gtk quick-add --attach NOTE` links what it adds to that note.

**Neovim:** `make install` puts a plugin in `~/.local/share/asst/nvim` (the package: `/usr/share/asst/nvim`). In a note, `:AsstTask` opens quick add with the note attached, `:AsstAttach` links it to an open task (in Telescope when installed), and `:AsstTasks` lists the tasks linking it and shows the one picked in the window. With lazy.nvim:

```lua
{ name = 'asst', dir = vim.fn.expand '~/.local/share/asst/nvim', cmd = { 'AsstTask', 'AsstAttach', 'AsstTasks' } },
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
notes = "~/Nextcloud/Notes"  # the folder linked notes are in (this is the default)
```

The cache is `~/.local/state/asst/asst.db`; deleting it only costs a full re-sync.
