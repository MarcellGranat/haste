## ADDED Requirements

### Requirement: One note per Person record
An agent SHALL be able to append a dated entry to the note for a `Person` record. The note path SHALL be `people/<slug>.md`, where the slug is derived from the person's display name by ASCII folding, lowercasing, replacing runs of non-alphanumeric characters with `-`, trimming `-` from both ends, and truncating to 60 characters. The link between note and record SHALL be the person id, not the slug.

#### Scenario: First entry for a person
- **WHEN** an agent appends an entry for person `p-123` with display name "Ana Lopez" and no note exists
- **THEN** `people/ana-lopez.md` is created and begins with front matter containing `haste_person: p-123`

#### Scenario: Unknown person
- **WHEN** an agent requests a person entry for an id that does not exist in the store
- **THEN** the request fails with `NotFound` and nothing is written

### Requirement: Front matter is written once, at creation
The daemon SHALL write front matter only when it creates a note file. For an existing file, the daemon SHALL NOT add, change, or remove front matter. An existing file whose front matter names a different person, or has none, SHALL refuse the append with reason `front_matter_mismatch`.

#### Scenario: Existing note with matching id
- **WHEN** `people/ana-lopez.md` exists with `haste_person: p-123` and an entry is appended for `p-123`
- **THEN** the entry is appended after the existing bytes and the front matter is unchanged

#### Scenario: Existing note with another id
- **WHEN** `people/ana-lopez.md` names `p-999` and an entry is requested for `p-123`
- **THEN** the append is refused with `front_matter_mismatch` and the file is unchanged

### Requirement: Front matter contains no addresses by default
Front matter SHALL contain the person id and the creation date. It SHALL contain `aliases` only when the human has enabled `person_notes.aliases`. Email addresses SHALL NOT appear in any person note unless the human has enabled `person_notes.include_addresses`. Both settings default to off.

#### Scenario: Defaults
- **WHEN** a person note is created with default settings and the person has an email identifier
- **THEN** the front matter contains `haste_person` and `created` only, and no address appears in the file

#### Scenario: Aliases opted in
- **WHEN** `person_notes.aliases` is enabled and the person has display name "Ana"
- **THEN** the front matter lists `aliases` with the display name and no email address

### Requirement: Dated entries carry attribution
Each entry SHALL be preceded by a dated heading `### <YYYY-MM-DD HH:MM>` in the daemon's local time and the existing attribution comment naming the agent and timestamp. The entry body SHALL be agent text that passes the body check in `obsidian-vault`.

#### Scenario: Entry format
- **WHEN** agent `agent-a` appends "Prefers morning calls." for person `p-123`
- **THEN** the file gains a `###` heading with the date, the line `<!-- haste: appended by agent-a at <timestamp> -->`, and the text, and no earlier bytes change

### Requirement: Person entries never include message content
A person entry SHALL be composed only from the agent's text and the person id. The daemon SHALL NOT copy subjects, message bodies, or identifiers of other people into a person note.

#### Scenario: No copied subject
- **WHEN** an agent appends an entry for a person while referring to a thread
- **THEN** the note contains only the agent text and the person id, and no thread subject
