## Context

haste-tui is a thin client. It talks only to the `hasted` daemon over the length-prefixed JSON protocol in `crates/haste-proto`, and it depends on `haste-core`, `haste-proto`, `ratatui`, `crossterm` and `serde_json`. Its current state is a single `App` in `crates/haste-tui/src/app.rs` with one `View` enum (approvals, connectors, agents and similar) drawn from `ui.rs`.

The store already holds triage state (`priority`, `snoozed_until`, `set_aside`, `archived`) separately from thread and message content, and connector re-syncs do not overwrite it. Reminders and calendar events are already modelled (`Reminder`, `Event` in `haste-core`) and exposed through daemon requests. Mail archive, reminder creation and calendar writes already flow through approved proposals and connector paths; the human-only actions in this change reuse those paths.

Constraints from the request: haste-tui must not depend on connector crates or `reqwest`; every effect goes through the daemon protocol; no message text in logs; Catppuccin colours live in one module.

## Goals / Non-Goals

**Goals:**
- Three-tab shell (Today, Channels, Calendar) with a status bar showing key hints.
- Today tab that merges mail and chat threads under the active preset filter and adds Apple Reminders TODOs due today or undated.
- Triage keys `e r g p s t` on threads, all executed through the daemon.
- Preset filters in `filters.json` with strict key validation and key-driven cycling.
- Catppuccin Mocha theme in a single module, rounded borders everywhere.
- Approvals, Connectors and Agents reachable from a secondary overlay.

**Non-Goals:**
- No light theme and no user-configurable palette in this change.
- No new send path. Replies open a human draft only; sending uses the existing human send flow.
- No agent-side archive. Agents still cannot archive mail.
- No calendar write (create/update/delete event) from the TUI. Agenda is read-only.
- No change to connector sync, OAuth, or the triage data model.
- No mouse support.

## Decisions

### D1. Tabs are a shell over the existing `App`, not new processes
`App` gains a `Tab` enum (`Today`, `Channels`, `Calendar`) and an `Overlay` option (`Approvals`, `Connectors`, `Agents`, or none). The existing approval, connector and agent rendering moves behind the overlay unchanged. Alternative considered: separate binaries per tab. Rejected: it would duplicate the daemon subscription and break the single status bar.

### D2. One inbox query, three projections
Today, Channels and the per-channel counts are projections of the same unified inbox listing. The daemon exposes one request that returns non-archived, non-snoozed threads with `source`, `unread`, `priority`, `set_aside`, and the sender and label fields needed by filters. The TUI groups by `source` client-side for Channels. Alternative: one daemon request per tab. Rejected: counts and lists would drift and the subscription would fire three times per change.

Message bodies are not part of the listing; they are fetched only for the selected thread when needed for the reply draft.

### D3. Preset filters are validated by the daemon, stored in the data folder
`filters.json` lives in the haste data folder. The daemon owns parsing and validation so that the TUI and any future client see the same rules. Schema: a list of named rules over `sender`, `label`, `source`, `unread`. Unknown keys are rejected with an error that names the key and the filter; the file is not partially applied. A missing file means a single built-in default filter that shows all non-archived threads. The TUI reads the list via a request and cycles the active filter index locally; cycling does not write the file.

Alternative: parse the file in the TUI. Rejected: it would make the TUI read a data file directly and duplicate validation.

### D4. Triage keys map to existing triage writes
- `g` sets `set_aside`, `p` toggles `priority`, `s` sets `snoozed_until` from a typed value in hours or days, `t` creates a reminder link for the thread through the existing reminder-link path.
- `e` archives. For mail the daemon requests the archive through the Gmail or iCloud connector path as a human-only action and only then sets `archived`. For chats it sets `archived` locally. The request is refused when the caller is an agent principal.
- `r` opens a human draft. If the thread has more than one distinct recipient the TUI asks reply or reply-all first. The daemon creates a draft only; no send request is issued from this key.

Triage writes never touch connector-owned content, so re-syncs remain safe.

### D5. Theme as constants in one module
A `theme` module contains every Catppuccin Mocha colour as a `const` (for example `BASE`, `SURFACE0`, `TEXT`, `MAUVE`, `GREEN`, `RED`, `YELLOW`, `BLUE`, `OVERLAY0`) and a small set of semantic aliases (`BORDER`, `SELECTED`, `HINT_KEY`, `HINT_TEXT`, `UNREAD`, `PRIORITY`). Widgets reference only the aliases or the palette constants; no inline `Color::Rgb` literal appears in `ui.rs` or `app.rs`. Borders use `BorderType::Rounded`.

### D6. Logging
The TUI logs only event kinds, counts and identifiers. Subject, sender, body and snippet fields are never passed to a logger. Daemon-side handlers added here follow the same rule.

## Risks / Trade-offs

- [Unified inbox size grows with history] → Listing is limited to non-archived, non-snoozed threads and paged by the daemon; Channels counts come from the same page metadata where possible.
- [Archive for mail is irreversible from the TUI if the connector call succeeds but the local write fails] → Order the operations: connector archive first, then local `archived`; on local failure, the next sync reconciles the state and the TUI shows an error line. No message text is logged in either case.
- [Key collisions: `r` reply versus the earlier request's reminder key] → The user explicitly moved reminder to `t`; `r` is reply everywhere in the inbox and Today views.
- [Calendar agenda needs a time window] → Agenda shows the next 7 days by default; the window is a daemon parameter, not a TUI constant.
- [Overlay hides Approvals from the default view, so pending approvals may be missed] → The status bar shows a pending-approvals count on every tab; pressing the overlay key opens Approvals directly.
- [Filter file typos silently hide threads] → Unknown keys are rejected and reported in the status bar; the previous valid filter set stays active.

## Migration Plan

1. Land the theme module and the tab shell behind the existing views; no behaviour change for existing views.
2. Add the daemon requests (inbox listing, channel counts, calendar agenda, reminder TODOs, filter list, triage writes) as additive protocol variants. Older clients ignore unknown responses only if the protocol version is bumped; bump the protocol version in `haste-proto` and refuse mismatches as the existing protocol already does.
3. Switch the default landing tab to Today once the inbox listing is live.
4. Rollback: set the default landing tab back to the overlay view. Protocol additions are inert if unused. `filters.json` is optional, so deleting it restores the default filter.

## Decisions from the user (formerly open questions)

- `t` (reminder for a thread) always opens a due-date prompt, prefilled with a default derived from context (a date found in the subject, otherwise tomorrow 09:00). Enter accepts, an empty value clears it to undated.
- Snooze accepts whole hours or whole days only (`3h`, `2d`); fractions are rejected.
- The Calendar agenda window is 7 days.
- Reply-all offers To and Cc only; Bcc is never offered.
