# machine-scope Specification

## Purpose
Define project tags as herdr workspace labels and how machines and projects scope agents and proposals.

## Requirements

### Requirement: Project tags are workspace labels
A project tag SHALL be the name of a herdr workspace: its label without the leading position marker that herdr adds (a label reads `[3] haste`, where `3` is only the workspace's position on that machine, so the tag is `haste`). A label without such a marker SHALL be used as is. The set of known tags SHALL be the union of the workspace labels in the latest snapshots of all observed machines and the last known label set persisted for each machine. The same label on several machines SHALL be one project; the machine SHALL be recorded separately and SHALL NOT be part of the tag. A tag SHALL be 1 to 64 characters with no control characters and SHALL be compared exactly.

#### Scenario: Position marker is not part of the name
- **WHEN** herdr reports the labels `[4] haste` on one machine and `[2] haste` on another
- **THEN** both are the single project tag `haste`, and reordering workspaces changes no tag

#### Scenario: Same label on two machines is one project
- **WHEN** workspaces labelled `dotfiles` exist on `m1` and on `mbp`
- **THEN** the known tag set contains `dotfiles` once

#### Scenario: Known set survives an outage
- **WHEN** machine `m1` becomes unreachable
- **THEN** the labels last seen on `m1` remain in the known tag set

#### Scenario: Invalid tag refused
- **WHEN** a human attempts to store a tag containing a control character
- **THEN** the tag is refused with a validation error and nothing is stored

### Requirement: Threads carry project tags set by the human
Threads SHALL carry a set of project tags. Only the human principal, through a human-build verb or the TUI, SHALL add or remove a tag, and only a tag in the known set SHALL be accepted. A thread with no tag SHALL be visible only to the human principal. Agent and machine principals, and proposals, SHALL NOT change tags.

#### Scenario: Human tags a thread
- **WHEN** the human adds the known tag `haste` to a thread
- **THEN** the tag is stored on the thread

#### Scenario: Unknown tag refused
- **WHEN** the human adds a tag that is in no machine's label set
- **THEN** the request is refused and nothing is stored

#### Scenario: Agent cannot change tags
- **WHEN** an agent or machine token requests a tag change
- **THEN** the request is denied and audited

#### Scenario: Untagged thread hidden from machines
- **WHEN** a thread has no tag and a machine principal reads threads
- **THEN** the thread is not returned to that machine principal

### Requirement: Machine principals are scoped to their own machine's projects
A machine principal SHALL be identified by its id equalling a herdr machine label. It SHALL see a thread only if one of the thread's tags is in the last known label set of its own machine. A machine principal whose id matches no known machine SHALL see no threads.

#### Scenario: Machine sees its own projects only
- **WHEN** machine `m1` has workspaces `haste` and `notes`, and threads are tagged `haste`, `notes` and `dotfiles`
- **THEN** a read by the `m1` principal returns the `haste` and `notes` threads and not the `dotfiles` thread

#### Scenario: Unknown machine sees nothing
- **WHEN** a machine principal has an id that is not a herdr machine label
- **THEN** its thread reads return an empty result

### Requirement: Scope filter in the daemon read path
Every daemon read that returns threads, reminders, proposals, or agent status records SHALL pass its result through a single scope filter in the daemon before serialisation. A human principal SHALL see all objects. An agent principal SHALL see only its own proposals and SHALL NOT see agent status records of other agents. The filter SHALL NOT be enforced only in the TUI or in client code.

#### Scenario: Empty result rather than error
- **WHEN** a machine principal has no visible objects
- **THEN** the read returns an empty result and not an authorisation error, so the existence of hidden objects is not revealed

#### Scenario: Filter cannot be bypassed by a new read request
- **WHEN** a new read request returning one of the listed kinds is added without passing through the scope filter
- **THEN** the integration test enumerating read requests per principal kind fails

#### Scenario: Reminder reads are covered by the exhaustive test
- **WHEN** the per-principal read test runs
- **THEN** it includes every reminder read request and fails if one skips the filter

### Requirement: Scope narrows reads only
The scope filter SHALL NOT grant any action to a principal. Machine principals SHALL remain read-only by default under the policy gate, and a scope match SHALL NOT change the gate outcome for any write action.

#### Scenario: Tagged item still read-only for machine
- **WHEN** a machine principal with a matching tag requests `draft` on that thread
- **THEN** the policy gate resolves the request as it did before this change

### Requirement: Reminders carry project tags set by the human
Reminders SHALL carry a set of project tags stored by haste, keyed by the reminder's external id, and not on the Reminders record. Only the human principal, through a human-build verb or the TUI, SHALL add or remove a reminder tag, and only a tag in the known set SHALL be accepted. A reminder with no tag SHALL be invisible to machine principals; the human sees all, and agent principals read reminders unscoped, as they read threads (what they may do is decided by the policy gate). Agent and machine principals, and proposals other than an approved create's `project`, SHALL NOT change reminder tags.

#### Scenario: Human tags a reminder
- **WHEN** the human adds the known tag `haste` to a reminder
- **THEN** the tag is stored against the reminder's external id and the reminder becomes visible to the machine whose workspaces include `haste`

#### Scenario: Re-sync keeps reminder tags
- **WHEN** a reminder with tag `haste` is seen again in a later sync, including after an earlier absence
- **THEN** its tag set is unchanged

#### Scenario: Agent cannot tag a reminder
- **WHEN** an agent principal requests a change of a reminder's tags
- **THEN** the request is denied and audited

### Requirement: Reminder read scope follows the thread scope rules
Reminder reads SHALL be filtered by the same rule as threads: a human principal sees all reminders, and a machine principal sees a reminder only when one of its tags is present on that machine's own workspace labels.

#### Scenario: Machine sees tagged reminder
- **WHEN** machine `m1` has workspace `haste` and a reminder is tagged `haste`
- **THEN** a read by a principal on `m1` returns that reminder

#### Scenario: Machine does not see untagged reminder
- **WHEN** a reminder has no tags
- **THEN** a read by a machine principal omits it and the human still sees it
