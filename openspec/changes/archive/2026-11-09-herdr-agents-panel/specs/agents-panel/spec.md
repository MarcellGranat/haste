## ADDED Requirements

### Requirement: Agent status model by machine and workspace
The daemon SHALL hold an agent status record for each herdr agent it observes, keyed by machine label, workspace id and pane id, carrying the workspace label and a status of `idle`, `working`, `blocked`, `done`, or `unknown`. Records SHALL NOT contain pane text, pane titles, working directories, or state label values.

#### Scenario: Record keyed by machine and workspace
- **WHEN** a status event or snapshot reports a pane on machine `local` in workspace `w1` labelled `haste`
- **THEN** the record is stored under `local` and `w1` with the label `haste` and the reported status

#### Scenario: No pane text stored
- **WHEN** an event or snapshot carries a title, working directory or state label
- **THEN** the stored record contains none of them

### Requirement: Agents panel in the TUI
The TUI SHALL provide an Agents panel listing agents grouped by herdr machine and then by workspace label, showing each agent's status and how long it has had that status. The panel SHALL update from daemon events without a manual refresh. A machine that is unreachable or in protocol mismatch SHALL be shown with that state rather than with stale agent rows.

#### Scenario: Grouped listing
- **WHEN** agents exist on `local` in two workspaces
- **THEN** the panel shows `local` with two workspace groups, each listing its agents with status

#### Scenario: Unreachable machine
- **WHEN** machine `m1` is unreachable
- **THEN** the panel shows `m1` as unreachable and does not show its last agent rows as current

#### Scenario: All hub machines shown
- **WHEN** the hub's herdr has saved machines `m1`, `m2` and `mbp` all enabled
- **THEN** the panel lists `local`, `m1`, `m2` and `mbp`

### Requirement: Rate-limited agent notices
Agent-side events that raise a haste notice SHALL use the existing `Notice` event with source `herdr:<machine>`. Notices SHALL be limited per machine, agent and notice kind to a burst of 3 and a refill of 1 per 30 seconds. Suppressed notices SHALL be counted and reported in the next notice as a suppressed count. Transitions to `blocked` SHALL be coalesced to at most one notice per machine per 10 seconds. A machine's baseline snapshot SHALL raise no notices.

#### Scenario: Chatty agent limited
- **WHEN** one agent changes status 50 times within a minute
- **THEN** no more than 3 notices are emitted immediately and the remainder are reported as a suppressed count

#### Scenario: Blocked transitions coalesced
- **WHEN** three agents on one machine become `blocked` within 10 seconds
- **THEN** at most one blocked notice is emitted for that machine in that window

### Requirement: Agents panel is read-only
The Agents panel SHALL NOT send keys, prompts, or any other write to a herdr pane, and SHALL NOT display pane output.

#### Scenario: No pane output shown
- **WHEN** the user opens the Agents panel
- **THEN** no pane text is requested from herdr or displayed
