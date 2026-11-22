## ADDED Requirements

### Requirement: Automatic draft only for approved senders
When the user opens a thread whose sender matches the `allowed_senders` file, the daemon SHALL enqueue a background draft job for that thread. For any other thread it SHALL NOT enqueue one. The draft context SHALL be the latest inbound message and at most 4 earlier loaded messages of the thread, oldest first, plus matching memories.

#### Scenario: Approved sender opened
- **WHEN** the user opens a thread from an approved sender
- **THEN** a draft job is enqueued and runs when the resource gate passes

#### Scenario: Other sender opened
- **WHEN** the user opens a thread from a sender not in `allowed_senders`
- **THEN** no draft job is enqueued

#### Scenario: Long history
- **WHEN** the thread has 12 loaded messages
- **THEN** the prompt contains the latest inbound message and the 4 earlier messages before it, and no others

### Requirement: Ghost text in the reply form
When a draft suggestion is available for the thread, the reply form SHALL show it as dim text after the cursor in the body. The suggestion SHALL NOT be part of the body until the user accepts it.

#### Scenario: Suggestion ready while the form is open
- **WHEN** the reply form is open and the draft becomes available
- **THEN** the suggestion is shown dimmed and the body text is unchanged

#### Scenario: No suggestion
- **WHEN** no suggestion exists or the gate has not passed
- **THEN** the form shows no ghost text and no message

### Requirement: Accepting or dismissing a suggestion
`Right` at the end of the body, or `Tab` at any cursor position, SHALL insert the suggestion into the body (after a newline when the body is not empty). While a suggestion is shown `Tab` SHALL NOT move focus; with no suggestion it SHALL keep moving focus. Typing any character SHALL dismiss the suggestion. A suggestion SHALL never be sent without the existing send confirmation.

#### Scenario: Accept with Right
- **WHEN** the cursor is at the end of an empty body and the user presses Right
- **THEN** the suggestion text becomes the body and the suggestion is no longer shown as ghost text

#### Scenario: Typing dismisses
- **WHEN** the user types a character while ghost text is shown
- **THEN** the ghost text disappears and the typed character is inserted

#### Scenario: Never auto-sent
- **WHEN** a suggestion is shown and the user does nothing
- **THEN** no message is sent

### Requirement: Rewrite popup
`Ctrl-r` in the reply form SHALL open a rewrite popup with the options: custom command (one-line instruction typed by the user), check grammar, more formal, more fluent, more friendly. The daemon SHALL receive the mode, the current draft text, at most 2 earlier messages of the thread and matching memories. The result SHALL be previewed and SHALL replace the body only after the user confirms with `Enter`, and `Ctrl-z` in the reply SHALL restore the previous body.

#### Scenario: Rewrite as more formal
- **WHEN** the user selects "more formal" with a non-empty draft
- **THEN** the daemon returns a rewritten text, the form shows it for confirmation, and confirming replaces the body

#### Scenario: Undo
- **WHEN** the user presses `Ctrl-z` after a confirmed rewrite
- **THEN** the body returns to the text it had before the rewrite

#### Scenario: Empty draft
- **WHEN** the body is empty and the user chooses a rewrite mode
- **THEN** the popup shows a one-line message and no model call is made

#### Scenario: Rewrite fails
- **WHEN** the rewrite outcome is `refused`, `timeout` or `unavailable`
- **THEN** the body is unchanged and a one-line message names the outcome

### Requirement: Rewrite is user-triggered and uncached
Every rewrite SHALL be started by a user key press and SHALL NOT be enqueued in the background. Rewrite results SHALL NOT be stored in `ai_results`.

#### Scenario: No background rewrite
- **WHEN** a thread is opened
- **THEN** no rewrite call is made
