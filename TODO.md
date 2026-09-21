# TODO

- [x] iPhone round trip: a reminder made on the phone (time, alarm, repeat, notes, URL), edited in asst, checked back on the phone (needed a Reminders app restart to see edits; asst's URL doesn't show there) <!-- asst:9f120f75-a108-4d77-8f2f-8edf042e91d0 -->
- [x] Replace Planify in dotfiles: the Super+Shift+A bind → `asst-gtk quick-add`, drop the planify float rule, `asst-gtk --background` with the tray apps in `hyprland.lua`, `planify` out of `packages/arch/97-apps.txt` <!-- asst:0c1ab5e1-7855-4134-9c48-5f57cf47b01e -->
- [x] Linked notes: a task links notes in Nextcloud Notes' folder, made from the card or linked from it, links follow renames; from a note, `asst add --attach` and the Neovim plugin <!-- asst:85f9b0ec-5e13-440f-b381-599cf12d4690 -->
- [x] `asst link` + `TODO.md` sync in asstd: a linked list's file gets open tasks under `## Inbox`, its checkbox lines become tasks; checking in either side shows on both, a deleted line completes, a deleted task takes its line away <!-- asst:7463540e-9c78-4b34-a623-481b012b0a68 -->
- [ ] steno: become an asst client (`asst add --source`, UID in `todos.json`, checkbox → `asst done`) <!-- asst:a72c6a5e-1e57-4725-b123-b369a47331ad -->
- [ ] mail-digest: action item → `asst add --source mail:<id>` <!-- asst:2e3ff834-174c-470e-afd1-87079ae672cc -->
- [ ] GitHub repo and AUR `asst-git` <!-- asst:4bc212f1-9b40-4701-9ce0-9ce7fd50cf40 -->

## Planify parity

- [x] Delete completed tasks: all of them from Completed, one list's from its menu, after asking <!-- asst:f10f7772-485a-4c3a-acfa-54b595327ea1 -->
- [x] Notes: Enter continues `- ` and `1.` lists; links show as links and open with Ctrl+click <!-- asst:c62309c0-3947-4158-afa8-71446fdcb68b -->
- [x] Add card and quick add: notes, reminders, `#` suggests lists, Ctrl+D/Ctrl+R/Ctrl+K, remember the last list, a default priority <!-- asst:a621a3f6-00e6-440b-a5b5-230b86e571a1 -->
- [x] Lists: drag to reorder or sort by name, archive, a ring that fills as tasks get done, copy as Markdown <!-- asst:8909ff14-f858-40db-ad41-75dc8f7ae6c9 -->
- [x] Reminders: how long before the due time the default one rings, several snooze lengths, a click on the notification opens the task <!-- asst:22948f8b-c9f8-43cb-a705-554dd31b1d2c -->
- [x] Ctrl+V outside a field adds the clipboard as a task <!-- asst:82030879-2c09-4675-929b-6bf251d42c9d -->
- [x] The other rows dim while a task is open <!-- asst:5c6736a7-4e1c-429d-8982-b47c5f7f0933 -->
- [x] Tab in the title goes to the notes <!-- asst:2ad4e50f-713d-46e9-9d39-68aa0f0a5fcf -->
- [x] Dragging near the top or bottom edge scrolls <!-- asst:c6062d3a-a7d4-4672-a8d0-aed3e58a60f7 -->
- [x] A checked repeating task's new date lights up <!-- asst:9233b4ce-f383-49c2-8a9f-1307ee054d8e -->
- [x] Quick Find: matches in bold, the line of notes that matched, `upcoming`, `p1`, `no date` <!-- asst:e7d34d59-19d8-425f-b3e1-bdea17ca3eb4 -->
- [x] Sort: descending, date modified, numbers in names in order (Task 2 before Task 10) <!-- asst:3d6248fd-f8d0-4492-956a-835b40a133b1 -->
- [x] Quick add: `tonight`, `this morning`, `tomorrow evening`, `midnight`; a switch to leave dates in the title <!-- asst:7fddf1c7-8220-4284-859c-3a9f76eabc26 -->
- [x] "No date" when rescheduling overdue tasks; copy selected tasks; filter a view by due date; Completed by list; save a task as .ics <!-- asst:fc6ff9b0-b647-461b-b6d7-af0c3abae652 -->
- [x] Bug: repeats asst has no words for (last Friday of the month, the 1st and 15th) show as RRULE text, and Custom… opens them as weekly (words added; the rest keeps the rule text and Custom… is greyed out for rules the editor can't show) <!-- asst:83d5ca30-8d1a-432c-9fac-77c351dbb48a -->
