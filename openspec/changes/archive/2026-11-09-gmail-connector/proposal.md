## Why

Haste's core (`core-policy-gate`) can store threads, gate agent actions and keep a human in the loop, but it has no real source of mail. Without a Gmail connector the inbox is empty and the draft and approval flow has nothing to act on. Gmail is the first real provider, so this change also proves the connector framework (`Connector` trait, guard, kill switch, circuit breaker) against a live API.

## What Changes

- Add a `haste-connector-gmail` crate that implements the existing `Connector` trait for `Source::Gmail`.
- Add OAuth 2.0 (installed-app flow with a loopback redirect) for a single Gmail account, using a personal Google Cloud project set to the "In production" publishing status (unverified, single user). This avoids the 7-day refresh-token expiry of testing mode; the consent screen shows an unverified-app warning that the human clicks through once. Refresh tokens are stored in the macOS Keychain, never in the SQLite store or the audit log.
- Add incremental sync: an initial bounded backfill, then history-based delta sync using the Gmail `historyId` cursor. The cursor is stored as the connector `Cursor`.
- Map Gmail threads and messages into the existing `Thread{source=gmail}` and `Message` data model, including participants and the triage fields already defined by the store.
- Add draft creation: an approved `draft` proposal creates a plain-text Gmail draft (`users.drafts.create`, `text/plain` only, no HTML). Drafts are never sent by this change. Creating a draft is a human-approved side effect, consistent with the policy matrix (`draft` = pending).
- Declare capabilities `read = true`, `draft = true`, `send = false`, `archive = false`. Sending remains unavailable in this change even for humans; it is a later decision.
- Wire the connector into the daemon scheduler behind the existing per-connector kill switch and circuit breaker. A `challenge` or `checkpoint` response from Google stops the connector at once and notifies the human; transient errors use the per-cycle attempt budget and never loop.
- Add a `haste connector gmail auth` human-only verb (human build) to run the OAuth consent flow.
- Add human-only connector administration that applies to every connector, not just Gmail: `haste connector status`, `haste connector release <source>` and `haste connector engage <source>` (human build only), a matching TUI action, and new human-only, audited protocol requests in `haste-proto` and `hasted`. This closes the gap left by `core-policy-gate`, where the circuit breaker could engage a kill switch but nothing could release it.
- Treat an `invalid_grant` refresh failure as terminal: engage the kill switch and notify the human that re-authorization is needed, with no retry.
- Add an audit entry for every sync cycle and every draft creation.

### Non-goals

- Sending mail, in any form (agents or humans).
- Archiving, labelling or deleting messages in Gmail.
- Multiple Gmail accounts, Google Workspace admin features, or push notifications (Pub/Sub watch).
- Attachment bodies: attachment metadata is stored, the content is not downloaded.

## Capabilities

### New Capabilities
- `connector-gmail`: OAuth consent and Keychain token storage, history-based sync into the data model, plain-text draft creation from approved proposals, the capability declaration (`read` and `draft` only), and human-only kill-switch status/release/engage administration for all connectors (this requirement is general; it is kept in this spec because the change has no `connector-framework` delta file yet, and can be moved there later).

### Modified Capabilities
<!-- The kill-switch administration requirement extends connector-framework in substance; it lives in the connector-gmail spec for now (see above). The Connector trait itself is used as it is. If implementation shows the trait must change (for example, an OAuth-aware constructor), that becomes a delta spec for connector-framework. -->

## Impact

- **New code**: `crates/haste-connector-gmail` (Gmail HTTP client, OAuth loopback, sync mapper); wiring in `crates/hasted` (scheduler, dispatch, connector admin requests) and `crates/haste` (human-only `connector gmail auth|status|release|engage` verbs); new request types in `crates/haste-proto`; a kill-switch listing query in `crates/haste-store`; a connectors action in `crates/haste-tui`.
- **Dependencies**: an HTTP client with TLS (for example `reqwest` with rustls), `serde` and `serde_json` for the Gmail JSON, a loopback listener for the OAuth redirect, and a Keychain binding for token storage (for example `security-framework`). Exact crates are chosen in `design.md`.
- **External systems**: Google OAuth 2.0 and the Gmail REST API (`gmail.readonly` and `gmail.compose` scopes only). This is the first change that contacts an external service.
- **Filesystem**: no new files in the notes root. Token material lives only in the Keychain.
- **Security surface**: the connector holds a long-lived refresh token, so the Keychain item is the only copy and the daemon is the only process that reads it. Agents still cannot send, and drafts only appear after human approval.
- **Operations**: the `gmail` kill switch is administered with the new `haste connector` human verbs and the TUI action (status, release, engage). A one-time Google Cloud project setup (production publishing status, OAuth client ID, scopes) is documented in `README.md`. Roadmap item 1 in `README.md` is satisfied by this change.
