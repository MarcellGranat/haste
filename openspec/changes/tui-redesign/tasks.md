## 1. Theme and shell

- [ ] 1.1 Add a theme module in `crates/haste-tui` with every Catppuccin Mocha colour as a `const` and the semantic aliases from the tui-theme spec
- [ ] 1.2 Replace inline colour literals in `ui.rs` and `app.rs` with theme aliases and switch all borders to rounded
- [ ] 1.3 Add a `Tab` enum (Today, Channels, Calendar) and tab-switch handling with per-tab selection preservation
- [ ] 1.4 Render the status bar with active tab, focus-dependent key hints, and the pending-approvals count
- [ ] 1.5 Move Approvals, Connectors and Agents behind a secondary overlay opened from any tab, keeping their current rendering
- [ ] 1.6 Verify `j`/`k`/`g`/`G` navigation works in every list and that the TUI crate has no connector or `reqwest` dependency

## 2. Daemon protocol and store reads

- [ ] 2.1 Add additive protocol requests and responses in `crates/haste-proto` for the unified inbox listing with source, unread, priority, set-aside and sender/label fields
- [ ] 2.2 Add protocol requests for the Today TODO list (Apple Reminders due today or undated) and the calendar agenda with a daemon-supplied window
- [ ] 2.3 Add protocol requests for triage writes: archive, set-aside toggle, priority toggle, snooze with a duration, and reminder-for-thread
- [ ] 2.4 Add a protocol request that returns the parsed filter list, and bump the protocol version for the additions
- [ ] 2.5 Implement the daemon handlers for 2.1 to 2.4 in `crates/hasted`, reusing the existing triage fields in `crates/haste-store`
- [ ] 2.6 Add daemon tests that a re-sync does not reset triage fields and that an agent principal is refused on archive

## 3. Preset filters

- [ ] 3.1 Define the `filters.json` schema and loader in the daemon with the allowed keys `sender`, `label`, `source`, `unread`
- [ ] 3.2 Reject unknown keys with an error naming the key and filter, leaving the previous filter set active
- [ ] 3.3 Implement the default filter used when the file is absent or empty
- [ ] 3.4 Add the filter cycle key in the TUI, cycling in file order then back to the default, without writing the file
- [ ] 3.5 Add tests for valid load, unknown-key rejection, cycle order, and default-only behaviour

## 4. Today tab

- [ ] 4.1 Render the unified inbox list on the Today tab under the active filter, ordered by most recent activity
- [ ] 4.2 Render the Apple Reminders TODO section for items due today or undated
- [ ] 4.3 Subscribe the Today list and TODO section to daemon change events
- [ ] 4.4 Verify that logs from the Today path contain only event kinds and identifiers

## 5. Triage keys

- [ ] 5.1 Wire `e` archive: mail archives upstream through the human-only path before setting `archived`; chats archive locally; agent principals refused
- [ ] 5.2 Wire `r` reply: prompt reply or reply-all when there are several distinct recipients, open a human draft, and never send
- [ ] 5.3 Wire `g` set-aside toggle and `p` priority toggle through daemon requests
- [ ] 5.4 Wire `s` snooze with a whole-hours-or-days prompt (`3h`, `2d`; fractions rejected) and set `snoozed_until` through the daemon
- [ ] 5.5 Wire `t` to prompt for a due date (prefilled from context, empty means undated) and create a reminder linked to the thread through the existing reminder-link path
- [ ] 5.6 Add tests that triage writes leave thread and message content untouched

## 6. Channels tab

- [ ] 6.1 Render the channel list (gmail, icloud, whatsapp, messenger, imessage) with non-archived counts under the active filter
- [ ] 6.2 Render the per-channel list using the Today row rendering and triage keys

## 7. Calendar tab

- [ ] 7.1 Render the upcoming-events agenda grouped by day, soonest first, using the daemon window (default 7 days)
- [ ] 7.2 Subscribe the agenda to daemon change events and confirm the Calendar tab issues no write requests
- [ ] 7.3 Verify that agenda loading logs only counts

## 8. Checks and cleanup

- [ ] 8.1 Run `cargo fmt`, `cargo clippy` and `cargo test` for the workspace and fix any warnings introduced by this change
- [ ] 8.2 Grep the TUI and daemon changes for message text or event titles passed to log macros and remove any found
- [ ] 8.3 Update the README section for haste-tui with the three tabs, key hints and `filters.json` format
