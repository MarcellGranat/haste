## MODIFIED Requirements

### Requirement: Connector declares sync and send capabilities
The iCloud connector SHALL declare the capabilities `read = true`, `draft = false`, `send = true` and `archive = false` for `Source::Icloud`. The `send` flag is consulted only by the human send path, which submits mail over SMTP as defined in `connector-icloud-smtp`. Sync SHALL NOT issue any IMAP command that changes mailbox state (APPEND, STORE, COPY, MOVE, EXPUNGE, CREATE, DELETE, RENAME) except the single Sent-copy APPEND issued by the human send path after SMTP acceptance.

#### Scenario: Capability declaration
- **WHEN** the daemon registers the iCloud connector
- **THEN** the declared capabilities are `read` and `send`, and no others

#### Scenario: No mailbox writes during sync
- **WHEN** a sync cycle runs against a mailbox with new and unread messages
- **THEN** no IMAP command other than LOGIN, LOGOUT, EXAMINE/SELECT, UID SEARCH and UID FETCH is sent, and no message flag changes

#### Scenario: Sent-copy APPEND only on the send path
- **WHEN** a sync cycle runs, or a send is refused before SMTP acceptance
- **THEN** no APPEND command is sent

### Requirement: Authentication with an app-specific password in the Keychain
The connector SHALL connect to `imap.mail.me.com` on port 993 over implicit TLS with certificate verification enabled for sync, and to `smtp.mail.me.com` on port 587 over STARTTLS with certificate verification enabled for sending. In both cases it SHALL authenticate with the Apple ID address and an app-specific password. The password SHALL be stored only in the macOS Keychain under service `com.haste.icloud`. The non-secret settings (address, host, port, the user's folder list, backfill sizes and the Sent folder name) SHALL be stored in `icloud.json` in the data directory with mode `0600`, and that file SHALL NOT contain the password.

#### Scenario: Password never written to disk outside the Keychain
- **WHEN** `haste connector icloud auth` completes successfully
- **THEN** `icloud.json` contains no password or secret field, and the Keychain item `com.haste.icloud` holds the app-specific password

#### Scenario: Certificate verification
- **WHEN** the server presents a certificate that does not validate
- **THEN** the connection is refused and the cycle is treated as a transient error

#### Scenario: Sent folder setting
- **WHEN** `icloud.json` names no `sent_folder`
- **THEN** the Sent copy goes to the folder `Sent Messages`
