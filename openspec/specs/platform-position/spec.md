# platform-position Specification

## Purpose

apb 1.x enforces node isolation on Linux only: on any other system a `full` isolation refuses with `os_unsupported` and one sentence every surface shows, and `best_effort` runs without a sandbox.

## Requirements

### Requirement: The 1.x platform position
On a system other than Linux, isolation `full` SHALL refuse with the reason code `os_unsupported` and a sentence that states apb 1.x has no sandbox there, and `best_effort` SHALL run without a sandbox. The fit report and the doctor SHALL show the same sentence.

#### Scenario: A full node on a Mac
- **WHEN** a playbook with an `isolation: full` node is started on macOS
- **THEN** it is refused with `os_unsupported` and the 1.x sentence

#### Scenario: A best effort node on a Mac
- **WHEN** the same node declares `best_effort`
- **THEN** it runs without a sandbox and the attempt records `no_sandbox`

#### Scenario: The fit report and the doctor off Linux
- **WHEN** the fit report or `apb doctor` describes isolation on a system other than Linux
- **THEN** it shows the 1.x sentence, the fit item with the code `os_unsupported` in front
