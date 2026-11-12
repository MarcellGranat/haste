## Why

The haste TUI is organised around individual views (approvals, connectors, agents) rather than around the question the user opens it with: "what needs me today?". Mail and chat threads, reminders and calendar events live in separate places, and triage actions (archive, aside, priority, snooze) are not reachable from one list. The look is also unstyled, which makes the status of each item hard to scan.

This change reshapes haste-tui into three tabs (Today, Channels, Calendar) with a Catppuccin Mocha look, a status bar with key hints, and a Today inbox that triages across all channels. It reuses the triage state that already exists in the store and keeps every existing view reachable.

## What Changes

- Replace the current single-view layout with a three-tab shell: **Today**, **Channels**, **Calendar**. A status bar at the bottom shows the active tab and the key hints for the focused view.
- Restyle all TUI surfaces with Catppuccin Mocha colours, defined as named constants in exactly one module. Rounded borders on all panels.
- **Today** tab: a unified inbox of all channels (mail and chats) restricted by the active preset filter, followed by TODOs from Apple Reminders that are due today or have no due date.
- Preset filters are saved queries stored in `filters.json` in the haste data folder. Each filter is a named rule over `sender`, `label`, `source` and `unread`. Unknown keys are rejected at load time. A key cycles through the filters; the default filter shows all non-archived threads.
- Inbox keys on a selected thread:
  - `e` archive. Mail is archived in Gmail or iCloud immediately; this is a human-only action and agents remain unable to archive. Chats are archived locally in haste only.
  - `r` start editing a reply. When the thread has several recipients the user is asked to choose reply or reply-all. This opens a human draft and never sends.
  - `g` put aside.
  - `p` set priority.
  - `s` snooze; asks for hours or days.
  - `t` set a reminder for the thread (the request originally named `r`; the user changed it to `t` because `r` is reply).
- Send continues to go only through the existing human send flow.
- **Channels** tab: the inbox grouped by channel/source (gmail, icloud, whatsapp, messenger, imessage), with a count per channel and a list per channel.
- **Calendar** tab: an agenda view of upcoming events grouped by day.
- Approvals, Connectors and Agents remain reachable as a secondary overlay opened from the shell. No existing view is removed.
- Vi-style navigation (`j`/`k` to move, `g`/`G` to jump) is kept in every list.
- All effects triggered from the TUI go through the daemon protocol. haste-tui gains no dependency on connector crates or on `reqwest`.
- Message text is never written to logs by the TUI.

No **BREAKING** changes to the daemon protocol are intended beyond additive requests needed to list Today, Channels and Calendar data and to read/write filters through the daemon.

## Capabilities

### New Capabilities
- `tui-shell`: three-tab layout, status bar with key hints, secondary overlay for Approvals, Connectors and Agents, vi navigation.
- `tui-theme`: Catppuccin Mocha palette as constants in a single module; rounded borders; light/dark is not in scope.
- `today-inbox`: unified Today list across channels, filtered by the active preset, plus due-today and undated Apple Reminders TODOs.
- `preset-filters`: `filters.json` schema, validation (unknown keys rejected), key-driven cycling, default filter.
- `inbox-triage-actions`: the `e`, `r`, `g`, `p`, `s`, `t` key actions and their confirmation/choice prompts, routed through the daemon.
- `channels-view`: inbox grouped by source with counts and per-channel lists.
- `calendar-agenda-view`: agenda of upcoming calendar events grouped by day.

### Modified Capabilities
- None at the requirement level. Triage state, archive rules and the send flow keep their existing requirements; the TUI only exposes them. The Approvals and Agents panels keep their requirements and gain only a new entry point through the overlay.

## Impact

- **Code**: `crates/haste-tui` (`app.rs`, `ui.rs`, `main.rs`, `client.rs`) is restructured around tabs; a new theme module holds the Catppuccin constants. Existing approval and agent rendering is reused inside the overlay.
- **Daemon protocol** (`crates/haste-proto`): additive requests for the Today list, channel grouping counts, calendar agenda, reminder TODOs, triage writes (archive, aside, priority, snooze, reminder-for-thread) and filter reads. Existing requests are unchanged.
- **Daemon** (`crates/hasted`): handlers for the new requests; archive for mail goes through the existing connector path and stays human-only.
- **Store** (`crates/haste-store`): reuses `priority`, `snoozed_until`, `set_aside` and `archived` triage fields; no schema change expected for triage. Reads of inbox and calendar use existing tables.
- **Data folder**: new optional `filters.json`; absence means the built-in default filter.
- **Dependencies**: no new dependency in `haste-tui`; it must not depend on connector crates or `reqwest`.
- **Logging**: no message text in logs from the TUI or the daemon paths it exercises.
