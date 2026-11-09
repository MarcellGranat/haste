## Purpose
Links between mail threads and reminders, and the identifier-only back-reference written into a created reminder.

## Requirements

### Requirement: Create a reminder from a thread through a proposal
A human SHALL be able to create a reminder from an email thread through the TUI or the human CLI, and an agent SHALL be able to propose the same through the proposal path. Both SHALL produce a `ReminderCreate` proposal. The proposed title SHALL be taken from the thread subject, truncated to a fixed maximum length, and the proposal SHALL reference the thread identifier.

#### Scenario: Human creates from thread
- **WHEN** the human runs the email-to-reminder action on a thread
- **THEN** a `ReminderCreate` proposal referencing the thread is created and the human is asked to approve it

#### Scenario: Agent proposes from thread
- **WHEN** an agent proposes a reminder from a thread identifier
- **THEN** a `ReminderCreate` proposal in state `pending` is created with the title taken from the subject

#### Scenario: Unknown thread refused
- **WHEN** a create-from-thread request names a thread that does not exist
- **THEN** the request is refused with a validation error and no proposal is created

### Requirement: Typed link between a reminder and its thread
When a `ReminderCreate` proposal that references a thread is executed successfully, the daemon SHALL store one link of type `about` from the reminder to the thread in the links table, in the same transaction as the `sent` transition. Either side SHALL be able to list the other through the link.

#### Scenario: Link stored on success
- **WHEN** the create executes successfully
- **THEN** exactly one `about` link exists from the new reminder to the thread

#### Scenario: No link on failure
- **WHEN** the create fails
- **THEN** no link is stored

#### Scenario: Either side lists the other
- **WHEN** the human views a thread that has a linked reminder, or a reminder that has a linked thread
- **THEN** the linked object is listed on the other side

### Requirement: Reminder back-reference carries identifiers only
The notes of a reminder created by haste SHALL contain at most a haste identifier for the thread or the proposal. They SHALL NOT contain the thread subject, message body, sender or participant identifiers. The notes SHALL be written once at creation and haste SHALL NOT edit them afterwards.

#### Scenario: Back-reference content
- **WHEN** a reminder is created from a thread whose subject and sender are known
- **THEN** its notes contain only the haste identifier and none of the subject or sender text

### Requirement: Link to a missing object is rejected
A link SHALL NOT be stored when either end references a reminder or thread that does not exist in haste.

#### Scenario: Thread removed before execution
- **WHEN** an approved create references a thread that no longer exists at execution time
- **THEN** the execution is refused, the proposal stays `approved`, and no reminder is created
