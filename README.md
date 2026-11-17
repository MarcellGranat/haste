# haste

A personal inbox hub (mail, chats, reminders, notes) driven from a vi-style TUI and
operated by agents through a CLI. **Agents can read, propose and append to a few notes.
They can never send; only you can, from the TUI: press `s`, and the send runs after a 5-second countdown you can cancel with any key.** Everything an agent asks for passes a policy gate in a local daemon.

This repository contains the core (`core-policy-gate`: daemon, store, policy gate, proposals,
approvals TUI, audit log, connector framework) and the first real connector (`gmail-connector`).
The other connectors are separate, later changes (see the roadmap below).

## Layout

| Crate | Role |
|---|---|
| `haste-core` | data model, policy gate, proposal state machine, audit log, tokens, connector trait |
| `haste-proto` | length-prefixed JSON wire protocol (4-byte big-endian length, UTF-8 JSON) |
| `haste-store` | SQLite (WAL), single writer thread, migrations |
| `hasted` | the daemon: Unix socket server, schedulers, result delivery |
| `haste` | agent CLI. Build with `--features human` for the human-shell verbs |
| `haste-tui` | ratatui interface (Catppuccin Mocha): Today, Channels, Calendar, plus approvals, connectors and agents |
| `haste-herdr` | herdr link: local socket feed, the hub's `--machine` CLI feed, safe command builder, pinned protocol schema |
| `haste-connector-icloud` | iCloud Mail connector (read-only IMAP), authorization helper `haste-icloud-auth`, Keychain secret store |
| `haste-connector-reminders` | Apple Reminders connector: typed helper protocol, runner, and the Swift EventKit helper (`helper/`) |
| `haste-google-oauth` | shared Google OAuth (PKCE loopback, token refresh, Keychain store, error mapping); each connector brings its own scopes and Keychain service |
| `haste-connector-gcal` | Google Calendar connector (incremental read, approved create/update, no delete), consent helper `haste-gcal-auth` |
| `haste-connector-gmail` | Gmail connector (read + plain-text drafts), OAuth helper `haste-gmail-auth`, Keychain token store |

```
 haste CLI (agents) ─┐                     ┌─ SQLite store (single writer)
 haste-tui (human)  ─┼─ unix socket 0700 ─▶ hasted ── policy gate ─┼─ audit.jsonl (hash chain)
                     │   token per caller  └─ connectors (trait) ──┴─ notes root (append only)
```

## Install and run (macOS)

```sh
cargo build --release -p hasted -p haste-tui
cargo build --release -p haste                        # agent CLI (no human verbs)
cargo build --release -p haste --features human \
    --target-dir target/human                         # human shell CLI

sed -e "s|__HASTED_BINARY__|$PWD/target/release/hasted|" -e "s|__HOME__|$HOME|g" \
    deploy/com.haste.hasted.plist > ~/Library/LaunchAgents/com.haste.hasted.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.haste.hasted.plist
```

After the first install, one command rebuilds, signs and restarts everything that changed:

```sh
scripts/deploy.sh hasted tui      # or: cli, helpers, reminders (no argument: hasted tui cli helpers)
```

It signs each binary with your Apple Development identity, so the macOS Keychain "Always Allow"
(and the Reminders permission) survives rebuilds instead of asking again. It restarts `hasted`
with `launchctl kickstart -k` only when `hasted` runs as the launchd job above, never one you
started by hand. Set `HASTE_SIGN_IDENTITY` to a certificate hash to use another identity.

Stop and roll back:

```sh
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.haste.hasted.plist
rm ~/Library/LaunchAgents/com.haste.hasted.plist
```

Data lives in `~/Library/Application Support/haste/` (override with `HASTE_HOME`; notes root
with `HASTE_NOTES_ROOT`). Bootstrap and rollback leave the store and audit files in place.

### Token issuance flow

1. First start creates the `human` principal and writes its token to `human.token` (mode `0600`).
2. The human registers an agent, either printing the token once or saving it to a file:
   `haste agent register researcher --allow read --allow draft --allow note.append --write-token`
   (human build). The daemon keeps only a salted SHA-256 hash.
3. The agent runs with `HASTE_AGENT=researcher` (or `HASTE_TOKEN_FILE=...`). That variable only
   chooses which token file the CLI reads. **The daemon identifies callers by token alone.**
4. `haste token revoke researcher` kills the token immediately and permanently.

## Gmail

The connector can **read** mail and, after you approve a draft proposal, **create a plain-text
Gmail draft**. After that, **you** can send it from haste (below). Agents cannot send, and the
connector cannot archive, label or delete. It only asks Google for the `gmail.readonly` and
`gmail.compose` scopes.

### Sending (human only)

Select an approved draft in Approvals and press `s`. haste first shows a **preview**: who it
goes to, the subject, how many sends you have left this hour, and whether the thread changed.
It then counts down 5 seconds (any key cancels, nothing is sent if you do) and sends once. A stale thread, or recipients that changed after you looked, still ask for `y` instead. It refuses (`recipients_changed`) if the recipient it now
resolves differs from the one you were shown. The recipient is the draft's `to`, otherwise the last
person in the thread who is not you; there is exactly one recipient (no Cc, no Bcc, no reply-all).

- Gmail sends the exact text you confirmed through `users.messages.send` (the same
  `gmail.compose` scope; no new scope), threaded into the conversation, then removes the draft
  it made at approval time (best effort).
- **At most once.** One request, never retried. If Gmail answers with a clear refusal the
  proposal returns to *approved*. If the outcome is unknown (a timeout, a 5xx) it stays
  *executing* and shows **NEEDS CHECK**: look in Gmail, then press `m` (it is there) or `b`
  (it is not). A restart never re-sends anything.
- Limits: 32 KiB body, 10 sends per hour (counted in the database, so a restart cannot reset
  it), plain text only, no attachments. An address or subject containing a line break is refused,
  never cleaned up. The audit holds ids and counts, never an address, subject or body.

### One-time Google Cloud setup

A project in "Testing" status makes refresh tokens expire after 7 days, so use a personal
project set to production:

1. In the [Google Cloud console](https://console.cloud.google.com/) create a project and enable the **Gmail API**.
2. **OAuth consent screen**: user type External, add the scopes `gmail.readonly` and `gmail.compose`, and add yourself as a user.
3. **Publishing status**: click *Publish app* so it reads **In production**. Leave it unverified: for one user (the cap is 100) no verification is needed.
4. **Credentials**: create an OAuth client ID of type *Desktop app* and note the client id and secret.
5. Build the daemon with Gmail, then authorize once:

```sh
cargo build --release -p hasted --features gmail -p haste-connector-gmail
cargo build --release -p haste --features human --target-dir target/human
export HASTE_GMAIL_CLIENT_ID=...  HASTE_GMAIL_CLIENT_SECRET=...
target/human/release/haste connector gmail auth     # opens the browser
launchctl kickstart -k gui/$(id -u)/com.haste.hasted
```

Google shows "hasn't verified this app" once: choose *Advanced*, then continue. The refresh
token goes to the macOS Keychain (service `com.haste.gmail`); `gmail.json` in the data directory
holds only the client id, secret and your address (mode `0600`). If the token is ever revoked
(`invalid_grant`), the connector engages its kill switch and notifies you: run the auth command
again, then release the connector.

### Re-reading a mail inbox (human only)

`haste connector resync gmail` (or `icloud`) forgets haste's local copy of that source (its
threads and messages, with their triage marks) and reads the inbox again from the start. Nothing
changes at the provider. Use it after changing what is loaded, for example to drop the
Promotions that were read before only the primary inbox was kept.

### Attachments: preview and save (human only)

In the reader, `a` opens the attachments of the message under the cursor. `p` previews the
selected file as text in a popup (PDF with `pdftotext`, Word with macOS `textutil`; other types
are not previewed), `Enter` or `d` saves it, `D` saves all of them. Saving opens a fuzzy folder
picker (type to narrow, Enter chooses); a file never overwrites another one (`name (1).ext`).
The file is fetched from the provider only now: Gmail `attachments.get`, an IMAP
`BODY.PEEK` of that one part for iCloud, and Beeper's local download endpoint for chats. Files
are limited to 25 MB. A preview is written to a private folder in the data directory and deleted
right after the text is read. Messages stored before this feature have no fetch reference:
run `haste connector resync gmail` (or `icloud`); chats are re-read once by themselves.

### Archiving (human only)

`e` archives a thread for real. Gmail: the thread leaves the inbox (`threads.modify` removes the
`INBOX` label and changes nothing else; it stays in All Mail). iCloud: the thread's inbox
messages are moved to the `Archive` folder with `UID MOVE` (change the folder with
`archive_folder` in `icloud.json`; nothing is deleted). It runs at the provider first and is
recorded here only when that worked. In the TUI it runs in the background, one at a time: the
thread leaves the list at once so you can keep moving, and if the mailbox refuses it comes back
with the reason in the status line. Approving an agent's archive proposal does the same. Chats are
archived here only. Gmail needs the extra `gmail.modify` permission, so after upgrading run
`haste connector gmail auth` once more. Only the primary inbox is read: Promotions, Social,
Updates, Forums, spam and other folders are not loaded.

### Sending attachments (human only)

In the TUI, `r` opens the reply editor under the thread; `Ctrl-F` attaches files (up to 5, 10 MB
in all; chats take one). The files are copied to `attachments/` in the data directory, the draft
names them, and the send confirmation lists them. Gmail and iCloud send a `multipart/mixed`
message; Beeper chats upload the file to the local Beeper service first, then send it with the
text. Only the human can attach: an agent's draft with attachments is refused (`Denied`), and a
send reads only plain, non-link files from `attachments/`. The audit records the count of files,
never a name.

### Connector administration (human only)

```sh
haste connector status            # every connector and whether its kill switch is engaged
haste connector release gmail     # resume; sync continues from the saved cursor
haste connector engage gmail      # stop talking to Gmail at once (cursor is kept)
```

The same actions live in the TUI's Connectors tab (`R` release, asking for `y`; `E` engage).
Agents cannot use any of this. The circuit breaker engages a switch by itself on a Google
restriction or challenge, with no retry, and never releases it.

### Manual smoke check (live account)

1. `haste connector gmail auth` finishes and prints your address; `haste connector status` shows `gmail` running.
2. After five minutes (or a daemon restart) `haste thread list --source gmail` shows recent threads.
3. As an agent: `haste draft --thread <id> --body "Thanks, noon works."`
4. In `haste-tui` approve the proposal: a plain-text draft appears in that Gmail thread, and the proposal's result reads `draft:<id>`.
5. `haste connector engage gmail`, wait one cycle: no new Gmail requests; `release` resumes it.

On first use macOS may ask whether `hasted` may read the Keychain item created by the auth
helper: choose *Always Allow*.

## iCloud Mail

The connector reads iCloud Mail over IMAP (`imap.mail.me.com:993`, implicit TLS, certificate
verification on). Reading sends only LOGIN, EXAMINE, UID SEARCH, UID FETCH (always `BODY.PEEK`,
so mail is never marked read) and LOGOUT. The only things it ever writes are a **human-approved
send** over SMTP and the one copy of that message it files in your Sent folder (below). Attachment
bodies are never downloaded; attachments appear as a metadata line (name, type, size). One text
part per message is fetched: `text/plain` if there is one, otherwise `text/html`, which is
reduced to text on your machine (tags, `<style>`, `<script>` and comments removed). HTML is never
rendered and nothing it links to (images, tracking pixels) is requested.

### Setup

1. At [appleid.apple.com](https://appleid.apple.com) open *Sign-In and Security*, then *App-Specific Passwords*, and create one for haste. (Your main Apple ID password is refused by iCloud.)
2. Build and authorize:

```sh
cargo build --release -p hasted --features icloud -p haste-connector-icloud
cargo build --release -p haste --features human --target-dir target/human
target/human/release/haste connector icloud auth \
    --address you@icloud.com --folder INBOX --folder "Sent Messages" --backfill 500
launchctl kickstart -k gui/$(id -u)/com.haste.hasted
```

The helper asks for the app-specific password without echo, checks it with a real login and
`EXAMINE INBOX`, and only then stores it in the macOS Keychain (service `com.haste.icloud`).
Nothing is stored if the login fails.

### Your settings

`icloud.json` in the data directory (mode `0600`, never contains the password) holds what you
chose; edit it any time and restart `hasted`:

```json
{
  "address": "you@icloud.com",
  "folders": [
    { "name": "INBOX" },
    { "name": "Sent Messages", "backfill": 1000 }
  ],
  "backfill_count": 200
}
```

* `folders`: which folders to sync (INBOX only if empty). A folder that does not exist is skipped and noted in the audit log; the others still sync.
* `backfill_count`: how many of the newest messages the first sync fetches per folder (default 200). A folder's own `backfill` overrides it. The same size is used if a folder's `UIDVALIDITY` changes on the server.

### If the password stops working

A rejected login is terminal: the connector engages its kill switch, notifies you, and does
**not** retry. Create a new app-specific password, run `haste connector icloud auth` again, then
`haste connector release icloud`. Network trouble is different: it is retried a few times within
one cycle, then waits for the next one, and never trips the breaker.

### Manual smoke check (live account)

1. `haste connector icloud auth ...` prints `Authorized you@icloud.com`; `haste connector status` lists `icloud` running.
2. After a restart and up to five minutes, `haste thread list --source icloud` shows recent threads, and they are still unread in the Mail app.
3. Send yourself a mail: it appears after the next cycle and only that one is fetched (the audit log line shows `INBOX:new=1`).
4. `haste connector engage icloud`, wait a cycle: no IMAP traffic; `release` resumes.

### Sending from iCloud (human only)

The same flow as Gmail (preview, 5-second cancellable countdown, one recipient, at most once, NEEDS CHECK when the
outcome is unknown), over SMTP: `smtp.mail.me.com:587` with STARTTLS and certificate verification,
logging in with the same app-specific password from the Keychain. The message is plain text with
`From` set to your iCloud address. After the server accepts it, haste files one copy in your Sent
folder over IMAP. If that copy fails the mail is still sent (it is never submitted twice) and the
proposal's result says `smtp;copy_failed`.

Two optional settings in `icloud.json`: `"sent_folder"` (default `"Sent Messages"`) and
`"append_sent"` (default `true`; set it to `false` if iCloud already files your sent mail itself, or
you would get duplicates; the first real send shows which). iCloud replies carry no
`In-Reply-To`/`References` yet (the store keeps no raw headers), so a reply is matched by subject.

## Herdr: agents, projects and machines

Haste watches [herdr](https://herdr.dev) so you can see what your agents are doing and so a
proposal knows which project it belongs to. It works from your **hub** computer, the one whose
herdr has the saved SSH machines:

* the **local** machine over herdr's unix socket (live status events);
* each **enabled saved machine** (`herdr machine list`) through the hub's own
  `herdr --machine <label> api snapshot`, polled every 15 seconds. Haste never runs `ssh`, never
  needs forwarded sockets, and never changes your saved machines. Every herdr call it makes has
  no stdin and a 20 second kill timeout, so an SSH prompt can never hang the daemon.

Turn it off with `HASTE_HERDR=0`. `HASTE_HERDR_SOCKET` and `HASTE_HERDR_BIN` override the socket
path and the `herdr` executable.

### The Agents tab and blocked agents

`haste-tui` has an **Agents** tab: every machine with its state (connected, unreachable, or
protocol mismatch) and, while it is connected, its workspaces and agents with their status and how
long they have had it. An unreachable machine shows no rows rather than stale ones. Agents that
herdr reports as `blocked` (waiting on an approval or question) also appear in the **Approvals**
tab, in a read-only *Blocked agents* group, oldest first. Haste never sends keystrokes or reads
pane output; the tab only shows who and where.

You get a notice when an agent becomes blocked or done (rate limited: a burst of 3, then one per
30 seconds per agent, blocked notices at most one per 10 seconds per machine). Starting haste
announces nothing for agents that were already blocked.

### Projects: workspace names are tags

A herdr **workspace name** is a project tag. Herdr shows labels like `[4] haste`; the number is only the workspace's position on that machine, so the tag is `haste`. The same name on two machines is one project, and reordering workspaces changes no tag.

```sh
haste tag list                       # the known projects (labels from all machines)
haste tag add <thread-id> haste      # tag a thread; only a known label is accepted
haste tag remove <thread-id> haste
haste machine status                 # machines, their state and their agents
```

(Human build only.) Tags are set by you, never by agents. A thread with no tag is visible only to
you and to agents, as before; a **machine** principal (an id equal to a herdr machine label) sees
only threads tagged with a project that has a workspace on its own machine.

### Telling haste where an agent runs

When an agent proposes something from a herdr pane, the `haste` CLI sends two hints:
`HERDR_PANE_ID` (herdr sets this) and `HASTE_MACHINE` (the herdr machine label; `local` if unset).
The daemon looks the pane up and stores the pane's **workspace label as the proposal's project**,
and the machine as its origin. In Approvals, press `P` to group by project instead of by agent;
`/` also filters by project. Set `HASTE_MACHINE=m1` in the shell profile on machine `m1`.

Hints are routing only. Who the caller is still comes from the token alone, and a hint can never
grant anything. How the CLI on a *remote* machine reaches the hub's daemon is out of scope here.

### Results go back to the agent

When a proposal is sent, rejected or expired, the agent is prompted with `agent.prompt`: over the
socket for a local agent, through `herdr --machine <label> agent prompt` for an agent on a remote
machine. If the local socket is down the plain CLI is used instead. Three attempts, then it stops
and `haste proposal status <id>` still has the result. An agent whose id is not a valid herdr
agent name (`[a-z][a-z0-9_-]{0,31}`) is never prompted.

### Protocol pinning

Haste speaks herdr **protocol 22** and checks it on the socket and in every snapshot. If herdr
reports another number, that machine is not observed and you get one notice naming both numbers.
The committed copy of herdr's schema (`crates/haste-herdr/schema/herdr-api-22.json`) is checked by
a test. After upgrading herdr: `herdr api schema --json > crates/haste-herdr/schema/herdr-api-NN.json`,
review the diff, bump `HERDR_PROTOCOL`.

### Security boundary

Haste never reads pane output and never sends keys. The only write to a pane is the result message
above. Titles, working directories and workspace contents are dropped on arrival and never stored,
audited or announced; the audit log records machine labels, ids and counts only.

### Manual smoke check (real hub)

1. Start `hasted`; `haste machine status` lists `local` and each enabled saved machine as connected.
2. Make an agent wait on an approval in a herdr pane: it appears under *Blocked agents* in Approvals.
3. From an agent in workspace `haste`, run `haste draft --thread <id> --body hi`: the proposal row shows `[haste]`.
4. `haste tag add <thread-id> haste` works; `haste tag add <thread-id> nonsense` is refused.

## Apple Reminders

Reminders are mirrored read-only into haste, and an agent can *propose* creating a reminder
(for example from a mail thread) or marking one done. Nothing is written until you approve the
proposal **and** run it. haste never deletes or edits a reminder.

```
 agent ── propose ─▶ pending ─ approve ─▶ approved ─ run (s) ─▶ executing ─▶ sent
                                                        └─ failed before any effect ─▶ approved
```

### Setup (macOS only)

1. Build the helper (a small EventKit program, ad-hoc signed, with a usage description):
   `crates/haste-connector-reminders/helper/build.sh` (writes `target/helper/haste-reminders-helper`).
   Put it next to `hasted` or point `HASTE_REMINDERS_HELPER` at it.
2. Deploy with `scripts/deploy.sh hasted reminders`: `hasted` then runs from
   `~/Applications/Haste.app`, which gives it its own macOS privacy identity. Run
   `haste connector reminders auth` (from anywhere, including SSH): the daemon asks macOS and
   the dialog appears **on the Mac's screen** (use Screen Sharing if you are away). Allow it
   there. A grant given to Terminal does not reach a launchd job, which is why the daemon asks.
3. Start `hasted`. The Reminders connector starts **engaged** (nothing runs) the first time.
   Release it with `haste connector release reminders`; `haste connector reminders status` then
   shows permission, kill switch and counts (never titles).

### Using it

- Leave a list out (groceries, say): `haste connector reminders exclude "Groceries"` (the name as Reminders shows it, any case). Its reminders are no longer read or shown, and what was stored from it disappears within a minute; no restart. `include` brings it back, `excluded` lists them. The setting is `reminders.json` in the data folder.
- Email to reminder: `haste email to-reminder <thread-id> [--title ..] [--project ..] [--due ..]`
  (or `T` on a thread in the TUI). It creates a proposal; approve it (`a`), then run it (`s`, or
  `haste proposal execute <id>`).
- Agents: `haste reminder create --title ".." --from-thread <id>`, `haste reminder complete <id>`,
  `haste reminder list`. Their `project` defaults to the workspace of their herdr pane and must be
  a known workspace label.
- Tags: `haste tag add-reminder <reminder-id> <project>` (human only). A machine sees a reminder
  only when it is tagged with a workspace that exists on that machine.
- The created reminder's notes contain only `haste:thread/<id>`, never the mail subject, sender or body.

### Known limits and the crash rule

- macOS only. No delete, no edit. Notes are mirrored truncated.
- If the helper times out or the daemon dies while a write is in flight, the proposal stays
  `executing` (shown as NEEDS CHECK) and is **never re-run automatically**. Look in Reminders, then
  `haste proposal resolve <id> --done` (it is there) or `haste proposal resolve <id>` (it is not).
- Missing permission is reported as `permission needed`; it never trips the circuit breaker.
- TUI reminder tagging and linked reminders on a thread view are not built yet.

### Manual smoke check (real Reminders)

1. `haste connector reminders auth` prints `granted` (after you allow the dialog on the Mac).
2. Release the kill switch; `haste connector reminders status` shows counts only.
3. `haste tag add-reminder <id> <project>` on one reminder; a machine principal sees it only on that machine.
4. `haste email to-reminder <thread>`, approve, run: the reminder appears with only the identifier in its notes.
5. Propose and run a completion for it, then delete it by hand in Reminders.

## Google Calendar

haste mirrors events from the calendars **you choose** (read-only), and an agent can *propose*
creating or changing an event. Nothing is written until you approve the proposal **and** run it.
There is no delete, no cancel, no attendee change, and no invitation is ever emailed
(`sendUpdates=none` on every write).

### Setup

1. In your Google Cloud project enable the **Google Calendar API** (the same Desktop OAuth
   client as Gmail works; the consent screen may stay in *Testing*, but then refresh tokens
   expire after 7 days and you re-run the auth verb).
2. Build the daemon with the calendar feature and the human CLI:
   `cargo build --release -p hasted --features gcal -p haste-connector-gcal` and
   `cargo build --release -p haste --features human --target-dir target/human`.
3. Authorize once (opens your browser; the app is unverified, choose *Advanced*, then continue):

```sh
export HASTE_GMAIL_CLIENT_ID=...  HASTE_GMAIL_CLIENT_SECRET=...
target/human/release/haste connector gcal auth
```

   It asks for `calendar.events` and `calendar.calendarlist.readonly`, stores the refresh token
   in the Keychain under `com.haste.gcal` (Gmail's `com.haste.gmail` item is never touched), and
   prints the calendars it found.
4. Put the ids of the calendars to sync in `gcal.json` in the data directory (written with mode
   `0600`, no secrets): `{"account": "you@example.org", "calendars": ["primary"]}`. An empty list
   reads nothing.
5. Restart `hasted`. The connector starts **engaged**: release it with
   `haste connector release gcal`.

### Using it

- Agents: `haste event list`, `haste event create --calendar primary --title ".." --start <unix> --end <unix>`,
  `haste event update <event-id> --title ".."`. Their `project` defaults to the workspace of their herdr pane.
- You: approve (`a`), then run (`s` in the TUI, or `haste proposal execute <id>`). Tag an event with
  `haste tag add-event <event-id> <project>`; a machine sees an event only if it is tagged with a
  workspace that exists on that machine.
- An update proposed against an event that changed afterwards is marked STALE and needs your
  acknowledgement (`--ack-stale`).

### Calendar colours

In the TUI's Calendar tab every calendar has a colour of its own: one picked from a built-in
palette by the calendar's id (the same colour every time), or the one you choose in
`calendar_colors.json` in the data folder (`~/Library/Application Support/haste`, or `$HASTE_HOME`).
The file is one JSON object from a calendar to a colour:

```json
{ "Work": "#f38ba8", "me@example.org": "teal", "local": "sky" }
```

- A calendar is named by its name or its id; capitals do not matter. `local` is the events that
  live only in haste (`c` on the Calendar tab, "local only").
- A colour is `#rrggbb`, `#rgb` or one of the palette names: rosewater, rose (a vivid one), flamingo, pink, mauve, red,
  maroon, peach, yellow, green, teal, sky, sapphire, blue, lavender.
- Mauve (the cursor) and red (the line for the time now) are not in the automatic palette, so they
  keep meaning one thing. The file is read when the TUI starts; an entry that is not understood is
  left out and the status line says how many.

### Known limits

- The first sync lists the whole calendar (Google only gives a sync token for an unwindowed
  listing) but stores only events within 30 days back and 180 days ahead; recurring series are
  kept as one master event, not expanded.
- Changing the times of an all-day event is written as a timed event; use a timed event for that.
- If a write times out the proposal stays `executing` (NEEDS CHECK) and is never re-run
  automatically: look in Google Calendar, then `haste proposal resolve <id> [--done]`.

### Manual smoke check (real calendar)

1. Fresh install: `haste connector status` lists `gcal` engaged and no event is read.
2. `connector gcal auth`, choose one test calendar, release, and see its events appear.
3. Propose a throwaway event, approve and run it: it exists in Google Calendar and no invitation email went out.
4. Propose a title change; an attendee-change proposal is refused. Engage `gcal` again: reads and approved writes are refused.
   Delete the throwaway event by hand (haste never deletes).

## Notes folder

Agents can keep notes about people, tasks and mail, but only inside **one folder you choose**.
haste never writes, reads or lists anything outside it, so it can sit inside an Obsidian vault
(or anywhere) without touching the rest of the vault. Agents only ever *append*: no edit, no
delete, no overwrite.

### Setup

1. Make the folder yourself (haste never creates it): `mkdir -p ~/Documents/haste-notes`.
2. Tell the daemon, in `notes.json` in the data directory (launchd does not pass your shell's
   environment, so this file is the normal way; `HASTE_NOTES_ROOT` only overrides it for development):

```json
{ "root": "/Users/you/Documents/haste-notes" }
```

   Optional front-matter settings (both default to off; unknown keys are an error):
   `{"person_notes": {"aliases": false, "include_addresses": false}}`.
3. Restart `hasted`. With the folder missing, note actions are refused with `notes_root_missing`.

### What agents can write

- `haste note append people/ana.md --text ".."`: plain text at a path inside the folder. Allowed
  top folders: `people/`, `tasks/` and `projects/<workspace>/` (the workspace must be a known herdr
  workspace). Paths are relative to the folder; `..`, absolute paths, hidden names (anything
  starting with `.`), symlinks and non-`.md` files are refused.
- `haste note person <person-id> --text ".."`: a dated entry in the Person's note
  `people/<name>.md`. The daemon writes front matter once (`haste_person`, `created`) and never
  changes it; no email address is written unless you opt in. Two people with the same name get
  `ana.md`, `ana-2.md`, and so on; a hand-written file is never modified.
- `haste note ref thread|reminder|event <id> --topic "Billing follow-up"`: one line
  `- haste:thread/<id>` under a dated heading in `tasks/billing-follow-up.md`. There is no text
  field: a subject, title or attendee can never end up in a reference note.

Limits: 64 KiB per append, 256 KiB per file, 200 appends per agent per day. Bodies cannot contain
a `---` line or a `<!-- haste:` marker. Machine principals never write notes.

### Reviewing

- Notes land directly by default. To review an agent's notes first, run
  `haste note mode <agent> approval`: its notes then wait in Approvals (`a` writes it, `r` drops it)
  and nothing is written until you approve. `haste note mode <agent> auto` switches back.
- `haste note show <path>` reads a note; `haste note log [--since <unix>]` lists writes (path,
  outcome, bytes or reason, never text).
- Every write is audited with the path and byte count only.

### Sync caveat

Appends are safe for several agents at once on a local disk. If the folder is synced (iCloud Drive,
Obsidian Sync) a conflict copy can appear when the sync service touches the same file. On this setup
the folder is not synced; if you move it into a synced location, test it first.

## The TUI

`haste-tui` uses the Catppuccin Mocha palette (all colours live in `crates/haste-tui/src/theme.rs`)
and has three main tabs, plus three secondary screens:

| Key | Screen |
|---|---|
| `1` Today | Two panes. **Incoming** (top): every channel under the active filter, priority first, then newest; 25 rows at a time, `j`/`k` scrolls through the rest. **TODO** (below): open Apple Reminders due today (overdue included) or with no date. `Tab` switches the cursor between the two panes |
| `2` Channels | The same inbox by channel (gmail, icloud, whatsapp, messenger, imessage) with counts; `Enter` opens a channel, `Esc` goes back |
| `3` Calendar | The next 7 days of your calendar, grouped by day (read-only, your local time) |
| `4` `5` `6` | Approvals, Connectors, Agents (`Esc` returns to Today); the header shows the pending-approvals count |

On a thread (Today, or inside a channel), only you can use these keys:

| Key | Does |
|---|---|
| `Enter` | open the thread and read it: every message in a chat-style frame (yours on the right, links underlined, attachment names shown), oldest first, starting at the newest. the newest 6 messages are shown and `o` shows 6 more (older ones are already stored, nothing is read again), `j`/`k` move between messages, `g`/`G` first and last, opening a thread also reads its earlier history once (iCloud: the same subject in your folders and Archive; chats: up to 150 older messages; Gmail already gives whole threads), chat messages show their reactions under the text (`↳ 👍×2 ❤️`; they refresh when you open the chat, at most every 10 minutes), `+` reacts to the selected chat message (a human-only action: pick 1-8, sent once through your chat network, never retried, audited with counts only), links are clickable (Cmd-click in a terminal with hyperlink support; a long address is shown as `https://host/…` and the full one is opened), and HTML mail keeps each link's address next to its text, `r` reply, `n` note, `l` label, `e` archive, `Esc` closes. Message text is cleaned of terminal control and bidirectional characters before it is drawn, and is never written to the logs |
| `e` | archive, **in haste only**: the thread leaves the inbox, nothing changes in Gmail, iCloud or the chat app |
| `r` | write a reply in `$VISUAL`/`$EDITOR`; the text becomes a draft proposal (approve it in Approvals, then send as usual). A mail thread with several people asks first; **reply-all is not available yet** because sending addresses one recipient |
| `g` | put aside / bring back (aside threads leave Today and show with `◌` in Channels) |
| `p` | toggle priority (marked `★`, sorted first) |
| `s` | snooze, whole hours or days only: `3h`, `2d` |
| `t` | turn the thread into a TODO. One key picks when: `Enter` the suggestion (a `YYYY-MM-DD` date in the subject at 09:00, otherwise tomorrow morning), `m` tomorrow morning (09:00), `w` next week (Monday 09:00), `l` later (three hours from now), `n` no date, `c` type a date (`2026-11-20 14:30`, `tomorrow`, `+3d`). Times are your local time. The reminder is created at once: only agents' actions wait for approval |
| `x` | mute: the sender goes into `.haste_ignore` and the thread leaves Today and Channels (see below) |
| `f` | next preset filter |
| `j` `k` `G` `0` | move, bottom, top |

In the TODO pane: `Space` selects a task (several if you like), `e` marks the selected tasks, or the
one under the cursor, as done. They stay listed, struck through, for **30 seconds** (`e` on one again
takes it back); then haste completes them in Reminders and removes them. Closing the TUI finishes
the ones still waiting instead of dropping them. Completing runs through the same audited steps as
an approval (propose, approve, run).

**Muting** is local: `.haste_ignore` in the data folder holds one regular expression per line
(`#` starts a comment), matched case-insensitively against a thread's senders (for a chat, also its
name). `x` appends an exact pattern for the sender of the latest message that is not you (for a chat,
its name), and never your own address. Edit the file to undo or to write wider patterns such as
`@newsletter\.example$`; a bad line is skipped and reported by line number. Nothing changes in your
mailbox or chat apps, and agents are not affected. The audit log records that a mute happened, not who.

**Preset filters** are `filters.json` in the data folder (the daemon validates it; a typo is shown in
the footer and the previous filters stay active; with no file there is one filter, `all`):

```json
{"filters": [
  {"name": "VIP", "sender": ["boss@corp.com"], "min_priority": 1},
  {"name": "Chats", "source": ["whatsapp", "messenger", "imessage"]},
  {"name": "Invoices", "subject": "invoice"}
]}
```

A thread matches when every key present matches; within one key any value may match. Keys:
`sender` (part of a participant address), `subject`, `source`, `min_priority`. Gmail labels and an
unread flag are not stored yet, so they are not filter keys.

## Logging and diagnosing problems

Two logs, with different jobs:

- **Audit log** (`audit.jsonl`): the hash-chained record of decisions (who asked for what, allowed
  or denied). Unchanged.
- **Operational log** (`logs/hasted.log`, `logs/haste-tui.log` in the data directory): one JSON line
  per event, for working out *why* something happened. Directory 0700, files 0600, rotated at 4 MB
  with 4 older files kept.

The operational log can never hold message text, chat or contact names, addresses or tokens: its
fields are numbers, fixed labels and identifiers restricted to `[A-Za-z0-9_.:-]`, so free text
cannot be passed to it (see `crates/haste-core/src/log.rs`). Chat ids are not logged (routes show
`v1/chats/:id`), nor are note paths or error messages; errors are logged as a class
(`authorization`, `transient`, `ambiguous`, ...).

What is recorded: daemon start and stop (version, whether `HASTE_HOME` is custom, which connectors
started), every audit record, every request that fails (all requests at `HASTE_LOG=debug`), every
connector fetch and send with duration and error class, Beeper calls (route, HTTP status,
duration), the Keychain read result, why a chat read came back empty (messages returned, new,
skipped), why a send was refused or why its outcome is unknown (connect, timeout, HTTP status,
answer without an id), circuit-breaker trips, panics (file and line only) and the TUI losing the
daemon.

```
haste logs                                   # last 100 lines at info and above
haste logs --level warn --since 2h           # what went wrong in the last two hours
haste logs --component beeper --level debug  # every Beeper call
haste logs --process haste-tui               # the TUI's log
haste logs --json                            # raw lines
```

`HASTE_LOG=debug` (set before starting `hasted`) records successful requests too. Times are UTC.

## Beeper (WhatsApp, Messenger, iMessage)

WhatsApp, Messenger and iMessage come in through **Beeper Desktop's local API** (a loopback server
on this Mac, `http://127.0.0.1:23373`). haste starts the three connectors **engaged**: nothing is
requested until you release a source (`haste connector release whatsapp`, and so on).

Read this before you turn it on:

- **Terms of service.** These networks do not offer an official API for personal accounts, and
  Beeper itself warns that accounts can be suspended and that unofficial integrations can be
  blocked. This is for personal use, a few chats, human-paced sending. You carry that risk.
- **Cloud accounts need your say-so, per account.** Beeper reports where each account runs
  (`bridge.provider`). An on-device (`local`) account just works. A **cloud** account (its messages
  pass through Beeper's servers) is refused unless you write `"allow_cloud": true` for that one
  account in `beeper.json`. If Beeper does not say (`platform-sdk`, `self-hosted`, older versions),
  haste refuses it unless you attest it (`"on_device_attested": true`). Attesting never overrides a
  reported cloud connection; `allow_cloud` is the only way to accept one.
- **iMessage self-chat does not send.** A message to your own iMessage chat came back unconfirmed (NEEDS CHECK, nothing delivered); sends to other contacts work. WhatsApp and Messenger self-chats work.
- **Agents never send.** They propose a reply; approving it creates nothing at Beeper. Only you can
  send, from the TUI: the preview shows the chat name, the network and the remaining sends, and the send runs after a 5-second countdown you can cancel.

### Setup

1. In Beeper Desktop, create an API token (Settings, Developers).
2. Write `beeper.json` in the haste data directory. It is the only thing that selects what is read:

   ```json
   {
     "accounts": [{ "id": "<account id>", "on_device_attested": false, "allow_cloud": false }],
     "chats": { "allow": ["<chat id>"], "deny": [] },
     "history_days": 30
   }
   ```

   Unknown keys are rejected; the file must be mode 0600. Nothing outside `allow` is ever
   requested, and `deny` wins over `allow`.

   To read **all** chats of the selected accounts instead of naming them, write
   `"chats": { "allow_all": true, "deny": ["<chat id>"] }`. Each cycle then reads the 50 most recent
   chats of those accounts (the per-cycle chat limit; quieter chats are counted as skipped), plus any
   chat named in `allow`. `deny` still wins. Every chat read this way is also a chat you can reply
   to from the TUI, and cloud accounts still need `allow_cloud`.
3. `haste connector beeper auth` (the human build) asks for the token without echoing it, checks it
   against the local API, and stores it in the Keychain (`com.haste.beeper`) only if the check
   passes. It then lists your accounts and chats so you can fill in `beeper.json`.
4. Restart `hasted`, then release the sources you want.

### Sending (human only)

The same flow as email (preview, 5-second cancellable countdown, at most once, NEEDS CHECK when the outcome is
unknown) with chat limits: one chat per message, at most 4 KiB of text, **10 per hour for each
network's account and 20 per day across all three**. The confirmation is bound to a digest of the chat id, the
chat name and the text, so a changed chat is refused. A refused or failed send returns the
proposal to approved; a send whose outcome is unknown stays `executing` and is never repeated.
`haste` records only counts and fixed labels in the audit log, never a chat name or text.

### Known limits

The API field names are provisional until the live spike (`openspec/changes/beeper-messaging`,
tasks 1.x) confirms them; unknown shapes fail closed. Attachments are shown as `[attachment]`
and are never fetched or sent. Beeper Desktop must be running; when it is not, haste reports
"not running" and does not retry in a loop.

### Manual smoke check (real Beeper Desktop, throwaway chat)

Tasks 11.x of the change walk through it: one throwaway chat, one account, one send.

## Policy

| Action | Agent outcome |
|---|---|
| read | auto |
| draft, archive, snooze, reminder.create | pending: a proposal is created, nothing happens until a human approves |
| note.append | auto inside `people/**` and `tasks/**` only; daemon-performed, append-only, attributed |
| send, send preview, approve, reject, manage | denied: human only; sending exists only in the TUI (the CLI has no `send` verb); a Beeper send is one chat, 10/hour per account and 20/day across networks |

Proposals return a pending id immediately. They expire after **7 days**, are superseded by a
newer proposal for the same thread and action, and carry a stale flag when the thread changed.
Approve and send are separate human actions. The agent hears the outcome through
`herdr agent prompt` (three attempts, backoff) and can always run `haste proposal status <id>`.

## Threat model

Protects against: a misbehaving or prompt-injected agent sending messages, editing or deleting
notes, writing outside the allow-list (including `..` and symlink tricks), approving its own
proposals, or reading other agents' proposals.

Does **not** protect against: another process running as the same macOS user. It can read
`human.token`, tamper with the files and rewrite the whole audit chain (the hash chain is
tamper-evident, not tamper-proof). The herdr environment variable is a hint, never identity.
There is no network listener and no encryption at rest beyond file permissions.

Connectors for risky services (WhatsApp, Messenger) will sit behind a per-connector kill
switch and circuit breaker: a challenge or checkpoint error stops the connector at once, with
no retry, and notifies the human.

## Spec coverage notes

Every spec scenario is covered by a unit or end-to-end test except these, which hold by
construction and have a manual check:

| Scenario | Why it holds | Manual check |
|---|---|---|
| No network listener | `hasted` only creates a `UnixListener` | `lsof -a -p $(pgrep hasted) -i` prints nothing |
| Client cannot bypass daemon | `haste` and `haste-tui` do not depend on `rusqlite` or `haste-store` | `cargo tree -p haste -p haste-tui \| grep -E 'rusqlite\|haste-store'` prints nothing |
| Agent requests note edit or delete | no such action exists in the protocol or CLI | `haste note --help` has no edit or delete verb |
| Token not retrievable | only a salted hash is stored | `sqlite3 haste.db 'select * from tokens'` shows hashes only |
| iCloud's only mailbox write is the Sent copy of a human send | the session wrapper has no method for STORE/COPY/MOVE/EXPUNGE, and APPEND is reachable only from the send path | the integration tests record every command the fake servers receive, and the sync test double panics on APPEND |
| Clients link no HTTP or Keychain code | the OAuth flow is a separate helper binary | `cargo tree -p haste -p haste-tui \| grep -E 'reqwest\|security-framework'` prints nothing |
| Agents cannot send; Gmail cannot change labels | `Action::Send` is denied to every non-human principal and the send requests have no agent path; no label method or extra scope exists | the consent URL lists only `gmail.readonly` and `gmail.compose`; the exhaustive per-principal test |
| A send happens at most once | `approved -> executing -> sent`, one request, no retry at any layer; an unknown outcome stays `executing` | the daemon and connector tests count requests per failure phase |

## Develop

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p haste --features human
```

## Roadmap (separate OpenSpec changes)

1. ~~`gmail-connector` (OAuth, sync, drafts)~~ done
2. ~~`icloud-imap-connector` (IMAP with an app-specific password in the keychain)~~ done
3. ~~`herdr-agents-panel` (status feeds, blocked agents, workspace tags)~~ done
4. ~~`reminders-and-links` (EventKit helper, email to reminder)~~ implemented, awaiting live smoke check
5. ~~`google-calendar`~~ implemented, awaiting live smoke check; `imessage-connector` is parked until Messages is set up on the hub Mac
6. ~~`obsidian-notes` (Person notes, one notes folder)~~ implemented, awaiting live smoke check
7. ~~`beeper-messaging` (WhatsApp, Messenger, iMessage through Beeper Desktop; replaces `whatsapp-messenger-spike`)~~ implemented and smoke-tested live (WhatsApp, Messenger, iMessage sends); remaining smoke checks 11.1-11.3, 11.5, 11.6, 11.8-11.10 open
