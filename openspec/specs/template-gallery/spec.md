# template-gallery Specification

## Purpose
The example playbooks shipped in the repository are a gallery of templates. `apb templates list | show | install`, the HTTP API and the dashboard Templates page install one as a draft in the project or the global store, with its eval scenarios, under its own id or another one; apb approves nothing for it, and the answer carries the draft's fit report and the next commands. A draft is written only into the store it names: a link beneath the store's resolved root refuses the install, and the window between that check and the writes is a stated limit.

## Requirements

### Requirement: The examples are templates
Every top-level playbook file of `examples/playbooks/` whose id is its file name SHALL be a template with the eval files of the folder of its name, and nothing in a nested folder without such a file SHALL be one.

#### Scenario: A nested research folder
- **WHEN** `examples/playbooks/quality-process/` exists with playbook files inside
- **THEN** no template comes from it

#### Scenario: A template carries no scripts
- **WHEN** the templates are listed
- **THEN** none has a script node

#### Scenario: Every example is a template
- **WHEN** the top-level `*.yaml` files of `examples/playbooks/` are counted
- **THEN** the gallery holds exactly that many templates and each id equals its file stem

### Requirement: A template installs as a draft and approves nothing
Installing a template SHALL create version 1.0.0 of a draft playbook in the chosen scope, with provenance `created_by: template` naming the template and its digest and with its eval files, SHALL never create, revoke or change an approval, and SHALL never lower a launch floor. After the draft is created, a failed eval copy, fit report or provenance write SHALL NOT undo the install and SHALL be named in the answer's warnings.

#### Scenario: A first start after install
- **WHEN** a person installs `dependency-security-fix` and starts it normally, with or without acknowledging untrusted content
- **THEN** the start is refused `draft_requires_trial`

#### Scenario: A digest approved earlier
- **WHEN** the installed content has a digest a person approved earlier
- **THEN** the install writes no approval and the fit report says the playbook is approved

#### Scenario: A stricter launch floor
- **WHEN** the id kept a launch floor stricter than the template's launch policy
- **THEN** the install is refused `launch_policy_loosened` and writes nothing

#### Scenario: Installing under another id
- **WHEN** a person installs `inbox-triage` with `--as my-inbox`
- **THEN** the draft is `my-inbox` and its definition names the id `my-inbox`, even when another playbook has the same trigger

#### Scenario: A second install of the same id
- **WHEN** the id already exists in that scope
- **THEN** the install answers `duplicate_id` and writes nothing

#### Scenario: A reserved or unsafe id
- **WHEN** a person installs a template with `--as apb-mine` or `--as ../x`
- **THEN** the install is refused and writes nothing

#### Scenario: From a run step
- **WHEN** a process that carries a run step's environment installs a template
- **THEN** the install is refused `template_in_run_step`, while listing and showing templates still answer

### Requirement: The install says what is still needed
The install answer SHALL carry the fit report of the new draft for the project it was installed from, and the CLI SHALL print its blocking items and the next commands; a global install that names no project SHALL answer without a fit report and say so.

#### Scenario: A missing account
- **WHEN** a person installs `dependency-security-fix` in a project without a github account
- **THEN** the answer's fit report names the missing `github` setup and the CLI prints `next: apb adopt dependency-security-fix --scope project`

### Requirement: Every surface installs
The CLI (`apb templates list | show | install`), the HTTP API (`GET /api/templates`, `GET /api/templates/{id}`, `POST /api/templates/{id}/install`) and the dashboard Templates page SHALL list and install templates in the project or the global scope through one install entry.

#### Scenario: A global install from the dashboard
- **WHEN** a person installs `document-checklist` with scope global on the Templates page
- **THEN** the draft is written into the global store and the page links to its global playbook page

#### Scenario: A global install with no project
- **WHEN** the dashboard serves no reachable project and a person installs a template with scope global
- **THEN** the install succeeds, the answer has no fit report and a warning says no project was named

#### Scenario: A duplicate over HTTP
- **WHEN** the same template is installed twice into one scope
- **THEN** the second answer is 409 with `rejected: duplicate_id`

### Requirement: A draft is written into the store it names
An install or a capture SHALL be refused `store_path_linked`, writing nothing, when a directory of the chosen store's path is a symbolic link.

#### Scenario: A project store linked to the global one
- **WHEN** a project's `.apb/playbooks` is a link to the global store and a template is installed with scope project
- **THEN** the install is refused and neither store changes
