# mail-drafts-attachments Specification

## Purpose
TBD - created by archiving change google-connectors. Update Purpose after archive.

## Requirements

### Requirement: IMAP drafts
The IMAP `append_draft` op SHALL compose a message with the shared composer and APPEND it with the `\Draft` and `\Seen` flags to the given folder or, without one, to the folder the server marks `\Drafts`. Without such a folder the call SHALL fail asking for `folder`.

#### Scenario: Autodetected drafts folder
- **WHEN** the server lists a folder with the `\Drafts` attribute and no folder is given
- **THEN** the message is appended there and the result names the folder and the Message-ID

### Requirement: Saving attachments into the run
The IMAP `save_attachments` op SHALL write a message's attachments under `runs/<id>/attachments/<node>/<uid>/` (with the item between node and uid for a list item), read the mailbox without changing it, and return the absolute paths. Outside a run it SHALL be refused.

#### Scenario: Outside a run
- **WHEN** `save_attachments` is called from the dashboard playground
- **THEN** it fails `permission`

### Requirement: Safe file names and no overwrite
Saved names SHALL drop path components, control characters and `:*?"<>|`, never be `.` or `..`, and an existing file SHALL never be overwritten: a numbered suffix is used instead.

#### Scenario: A hostile name
- **WHEN** an attachment is named `../../evil.sh` and `a.txt` already exists for another one
- **THEN** the files land as `evil.sh` and `a-2.txt` inside the uid directory and the existing file keeps its bytes

### Requirement: Writes stay inside the run
Attachment directories and files SHALL be created descriptor-relatively beneath the run directory, refusing a symbolic link at every component, and a file SHALL be created exclusively, never replacing one.

#### Scenario: A linked attachments directory
- **WHEN** `runs/<id>/attachments` is a link to a directory outside the run
- **THEN** the call fails and nothing is written outside the run

### Requirement: Caps
At most 20 files, 10 MiB per file and 25 MiB per call SHALL be written, each checked before the write; a message whose received bytes exceed 50 MiB SHALL be refused whatever size the server advertised. Files left out SHALL be listed with a reason.

#### Scenario: A large attachment
- **WHEN** one attachment is 11 MiB
- **THEN** it is listed under `skipped` with `too_large` and the others are saved

#### Scenario: The call cap is checked before writing
- **WHEN** a message has attachments of 9, 9, 9 and 1 MiB
- **THEN** three files totalling 19 MiB are saved and the third 9 MiB one is skipped with `call_cap`
