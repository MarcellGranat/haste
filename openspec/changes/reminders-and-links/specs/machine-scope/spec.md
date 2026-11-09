## ADDED Requirements

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

## MODIFIED Requirements

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
