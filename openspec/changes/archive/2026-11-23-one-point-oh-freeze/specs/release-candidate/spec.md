## ADDED Requirements

### Requirement: Version 1.0.0-rc.1
This change SHALL set the version to 1.0.0-rc.1 and SHALL ship `docs/release-notes/v1.0.0-rc.1.md` with the lines of every unreleased change of the 1.0 plan and the soak checklist. The automated tests SHALL promise only what `apb self-update --check` reports; the real update of an installer install SHALL be a soak step a person performs and records, and the soak SHALL not be complete without that record.

#### Scenario: A beta install sees the candidate
- **WHEN** a 1.0.0-beta.7 install runs `apb self-update --check` while stable 0.25.0 and pre-release v1.0.0-rc.1 are served
- **THEN** it reports an update available with exit code 10

#### Scenario: A stable install does not see the candidate
- **WHEN** a 0.25.0 install runs `apb self-update --check` while the same releases are served
- **THEN** it reports that apb is up to date with exit code 0

#### Scenario: The soak records the real update
- **WHEN** a person reads the soak checklist of the release notes
- **THEN** it holds the step: on a 1.0.0-beta.7 installer install run `apb self-update`, check that `apb --version` prints 1.0.0-rc.1 and `apb doctor` runs, and record the date, the system and the versions
