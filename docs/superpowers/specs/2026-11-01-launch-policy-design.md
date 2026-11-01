# Launch policy for agents

Date: 2026-11-01. Status: accepted, built. Author-facing usage:
`docs/HOWTO-authoring.md` ("Launch policy (agent_launch)"), the guarantee
and its limits: `docs/GUARDRAILS.md` ("Launch policy for agents"), the rule:
decision record 0011.

## 1. Goal

A coding agent connected to apb can start any playbook it can name. For most
playbooks that is the point; for some (a deploy, a release, anything a
person wants to see begin) the person wants to decide, or at least be asked.
Before this change the only brakes were trust (an unapproved definition) and
the irreversible consent, and neither says "an agent may not start this on
its own".

This document records the design as built: three policies and where they
come from (section 2), the floor that keeps an agent from loosening them
(section 3), the one check every launch goes through (section 4), how an
authorization lives as long as its run (section 5), how a decision crosses a
process boundary (section 6), the surfaces (section 7) and how older
versions of apb behave (section 8).

## 2. Policies and their sources

`apb_core::launch::LaunchPolicy` is `allow`, `ask` or `deny`, ordered from
the loosest. `allow` is the default and what apb did before.

Four places set a policy:

1. The playbook's own `agent_launch:` (an optional top-level key of schema
   2). It is part of the definition and of its trust digest, so changing it
   changes what a person approved.
2. The playbook's launch floor (section 3).
3. The machine's `<config dir>/policy.yaml`, section `launch:`, with
   `agent:` (a default for every playbook) and `playbooks:` keyed exactly:
   `<workspace id>/<id>` for a project playbook, `global/<id>` for a global
   one. An exact key reaches one playbook of one project.
4. The project's `.apb/policy.yaml`, the same section, keyed `<id>` or
   `global:<id>`.

The reader of both files (`apb_core::policy_file`) is strict: an unknown
section, key or value is an error that names it. No apb surface writes
either file.

`apb_core::launch::resolve` combines them. For each playbook of the run's
tree (the playbook and every pinned sub-playbook, recursively): the stricter
of its declaration and its floor, unless the machine file has an entry for
that exact key, which replaces both (the machine file is the person's own).
Then the strictest of that, the machine default, the project default and the
project entry. The run's policy is the strictest over the tree. The result,
`EffectivePolicy`, names every setting that holds the run at its policy, so
a refusal can say why.

The project file can only narrow: it never loosens a declaration, a floor or
a machine setting.

## 3. The launch floor

A policy in the definition alone would be easy to undo: an agent that may
not launch `deploy` could save a version of `deploy` without the key and
launch that. The floor closes this.

### 3.1 Identity

`apb_core::launch::floor::FloorKey` is either `Project { workspace_id, id }`
or `Global { id }`. A project key names the workspace that owns the store
holding the definition, never the workspace a run executes in; a global key
is one machine-wide key for the one global store. Every writer passes the
key explicitly (`FloorKey::project_of(owning_root, id)`, `FloorKey::global(id)`),
and every reader builds it from the resolved definition's origin. A global
playbook run from any project therefore meets the same floor.

The workspace id of a project key, and of the machine file's exact
`<workspace id>/<id>` entries, is never taken from `.apb/workspace.local`
alone, a file anything that writes the project tree can change. It counts
only when it has the shape apb mints (`ws-` and lowercase hex) and the
projects registry under the config dir (`<config dir>/projects.json`) has
it registered at this very root (`apb_core::projects::registered_id`). A
writer registers the workspace first when it is not yet, whatever the
listing opt-out says (`projects::bind_id`), and refuses an id another live
directory holds or one apb did not mint. A reader with no registered id
keys nothing and falls back to the store's scan.

### 3.2 The store

`<config dir>/launch-floors.json` maps keys to policies. It is written
atomically and privately under its own directory lock, and its directory is
synced after the rename, so a power cut never keeps a newer stricter
version while losing the record that held it. A key with no record has the
floor of the strictest declaration among the versions its store holds now
and among the trash entries of the same id beside it (`floor::store_scan`),
so a playbook deleted before its floor was ever recorded is not recreated
looser; that scan is never written by a read, so the fit report and the
read-only surfaces write nothing. The first write of a record starts from
the scan.

### 3.3 Who may move it

- An agent channel never lowers it. A save from an agent channel whose
  declaration is looser than the floor is refused with
  `launch_policy_loosened` and a next action for a person. The agent
  channels are the MCP authoring tools (`playbook_update`,
  `playbook_create` over an existing or trashed id, `playbook_capture`, the
  discover save, `playbook_make_global`, `playbook_trash_restore`), the
  supervisor patch, the forward patch (whose protected fields now include
  `agent_launch`) and the current-run patch.
- A person channel other than the editor (`apb import`, `apb migrate`, the
  CLI's trash restore and make-global) raises the floor to the new
  declaration and never lowers it.
- The dashboard editor with keys raises the floor like a person channel and
  lowers it only to the declaration the person confirmed in the editor: a
  save that would lower it (a looser declaration, a version without the
  key, a recreate of a deleted id) is refused with
  `launch_floor_lowering_unconfirmed`, the editor shows the floor and the
  new level, and the confirmed retry carries `confirm_lower_floor`. The
  editor of a dashboard without keys never lowers a floor
  (`launch_floor_lowering_needs_terminal`, pointing to `apb launch floor`),
  since anything on the machine reaches its port.
- `apb launch floor <id> [--global] --lower-to <policy>` lowers it after
  asking at an interactive terminal; it is refused inside a run step and
  without a terminal.

Deleting the key, deleting and recreating the id, restoring from the trash,
copying to global scope and editing the project file all leave the floor in
force. Deleting a playbook writes the scanned record before the directory
moves to the trash, so a later recreate meets it. A copy to global scope
raises the global key to the stricter of the source's floor and every copied
version's declaration before the copy becomes visible.

### 3.4 Publication

Every path that makes a version visible in a store (`commit_version_dir`
for a save, an import or a patch; a migration's new version;
`create_draft_in`; the scope copy's final rename; the trash restore's
rename) raises the explicit key's floor first, durably, and only then
renames. `create_draft_in` creates the playbook's directory before it
touches the floor, so a duplicate id is refused without raising anything. The loosening check, the raise
and the rename happen while the floor lock is held, so a lowering cannot
fall between a check and its publication. Lock order: the playbook's
pointer locks first, then the floor lock; `move_current` runs after the
floor lock is released and needs no floor write, because every version it
can name is already covered.

A crash between the floor write and the rename leaves the floor stricter
than the store (fail closed; only a person lowers it). A crash after the
rename leaves both. A crash during the floor write leaves the old file and
no new version. No recovery step reads the floor file.

A damaged `launch-floors.json` refuses every agent-channel launch and save
with `launch_floor_invalid`; the dashboard and a terminal still start, with a
warning, and `apb doctor` names the file.

## 4. The launch check

### 4.1 Who launches and how it is confirmed

The engine's `gate::launch` names the caller with explicit types:

- `Initiator`: `AgentMcp` (MCP outside a run step), `RunStep` (MCP or CLI
  with `APB_RUN_ID` or `APB_HOST_TASK` set, whatever the channel),
  `CliUnattested { terminal }` (the CLI outside a run step), `Dashboard`
  (a dashboard signed in with a key), `DashboardKeyless` (a dashboard
  without keys, which anything on the machine reaches: decided like the CLI
  without a terminal), and `Engine` (the pause waker, the quota probe,
  `apb resume --all`). The CLI is never a person because a variable is
  absent; only a terminal question or a nonce counts.
- `LaunchConfirmation`: `TerminalPrompt`, `CliFlag { nonce }`,
  `HostRelayed { nonce }` (MCP `confirm_launch`), `DashboardNonce { nonce }`
  (a keyless dashboard's `confirm_launch`), and `Forwarded` (a decision
  another apb process made, section 6).
- `LaunchKind`: start, trial, execute plan, eval, resume, resume all,
  continue from.

`acknowledge_untrusted` stays a trust answer and confirms no launch.

### 4.2 The decision table

| Effective | Agent over MCP | Run step | CLI, terminal | CLI, no terminal | Dashboard, keys | Dashboard, no keys | Engine |
|---|---|---|---|---|---|---|---|
| allow | runs | runs | runs | runs | runs | runs | runs |
| ask | the refusal's nonce, else refused with `confirm_launch` | refused | asks, or `--confirm-launch` | `--confirm-launch` | runs | the refusal's nonce (`confirm_launch`) | refused |
| deny | refused with `launch_by_person` | refused | asks | refused | runs | refused | refused |

The refusal codes are `launch_requires_confirmation`,
`launch_requires_person`, `launch_refused_in_run_step` and
`launch_refused_for_engine`; a damaged settings file is
`policy_file_invalid`.

**A redirected config dir.** The machine `policy.yaml`, the floor records,
the projects registry and the global store all live in the config dir,
which `APB_CONFIG_DIR`, `XDG_CONFIG_HOME` and `HOME` choose. For a run step
and for the CLI without a terminal (`gate::launch::effective_for`), when the
config dir differs from the default one resolved from the real home (the
password database entry of the real user id, `<home>/.config/apb`, ignoring
those variables), the launch inputs of the default one are read as well, and
when they hold the tree stricter (or do not read) the launch is refused with
`policy_file_invalid`, naming both directories. A variable is not how a
launch setting is changed. The one carve-out is `apb eval`: it hands each
case the config dir it resolved through the case's handoff
(`HandoffGrant.config_dir`), and the case accepts exactly that one; a
detached supervised driver gets the same from `apb run --supervise`.

### 4.3 Where it runs

`gate::check_run` takes a `LaunchContext` and runs the launch check on the
finished permit, after the version is chosen and outside the inner check
that settles a candidate, so a launch refusal never reads as a skipped
candidate. Trial and eval use `gate::check_trial_launch`, which resolves and
verifies the tree and the root's profile bundles, decides on it and hands
the verified child pins and bundles to the engine, so a child whose
`current` moves, or a profile edited, after the check is not what runs.

### 4.4 The fixed choice

The gate chooses the version first: the explicit one, the candidate by its
random draw, or `current`. A refusal names that choice in its start action
(`launch_version`, `launch_choice: candidate | current | explicit`). A
retry with a confirmation must pass both back, and the gate then makes no
draw: a `candidate` retry runs the candidate when the pointer still names it
and its lineage and inherited trust still hold, a `current` retry runs
`current` when it still names that version; otherwise the retry is refused
(`launch_choice_changed`) with a fresh nonce. The same nonce always reaches
the same permit or a refusal.

### 4.5 The nonce

The nonce is a short digest over the execution workspace id (read without
creating it), the playbook id and scope, the chosen version and its choice,
the canonical permit digest, the effective policy with its sorted sources,
the launch kind, and the run for a resume or a continue. The canonical
permit digest covers every authorization-relevant field of the final
permit: the definition's trust digest, the verified profile-bundle map, the
connector and connector-account maps and the whole pinned child tree with
each pin's scope, version, digest and its own maps, in a canonical
encoding. A changed skill, connector, account or child scope after a
refusal changes the nonce. A trial's and an eval's nonce cover the root's
profile bundles, connectors and accounts the same way.

When trust acknowledgement or the irreversible consent is needed as well,
one refusal carries every hint and every start action, so the host asks the
person once.

## 5. An authorization covers its run's lifetime

A run started by this release records its authorization once, in the
write-once run manifest as `launch: LaunchRecord`: the launch kind, the
initiator, the confirmation kind (never a nonce), the policy and its
sources. That record covers the run for its whole lifetime: pauses, the
waker, the quota probe, `apb resume`, `apb resume --all`, MCP `run_resume`
and the dashboard's resume. A policy tightened after the start does not stop
an authorized run.

A run without an authorization (started by an older apb) needs a launch
check on resume, from its own snapshot and its pinned children, each loaded
by version and verified by digest. A run that has no pins gets its child
tree resolved and verified from its own origin; the grant carries that tree,
and the engine persists it as the run's pins in the same step that records
the authorization, so the drive spawns exactly the checked children. A tree
that does not resolve or verify refuses the resume.

A resume that passes records its authorization on `run_resumed.launch`;
from then on the run is authorized (the latest record wins). The engine's
own resumes are `Engine` in the table: an unauthorized `ask` or `deny` run
stays paused, so a pause with `until` set by an agent on such a run never
resumes it. `apb resume --all` checks each run before its quota probe and
lists a run refused by the policy as `left: launch policy`, without marking
it driverless. The resume's launch check is separate from the origin and
trust checks of `check_resume`; `apb resume` still does not check the
installation stamp.

A child run spawned under its parent's pin records `launch` with kind
`child` and the parent's initiator. A child run without its own record is
reattached by its parent's resume only when its snapshot digest equals the
parent's pin for that node; otherwise the node fails with
`launch_child_not_covered`. A parent's record alone never covers an
unverified child. A reattached child that has no pins of its own gets the
parent's pin for its children persisted as its `expected_children` before
it continues, so its descendants are the verified ones from then on.

A run whose driver never started (`__drive-run` without `--resume`, which
takes no handoff) and that holds no authorization is driven only when its
playbook is `allow` for agents now, read strictly. The engine's backstops
for a start or a resume without a grant read the settings strictly too: a
damaged project `policy.yaml` never drops the machine section there.

## 6. The grant and the handoff

A passing check returns a `LaunchGrant`: the record to write, the canonical
permit digest it was checked for and a digest of the policy state that
decided it. The grant is a value, never a file the run keeps.

- A start hands it to the engine in `RunOptions.launch`. Run preparation
  recomputes the policy state of the snapshot it pinned and refuses the
  start with `launch_policy_changed` when it differs, once before anything
  is written and once more right before the manifest, holding the floor lock
  from that final check until `manifest.launch` is written, so a publication
  that raises a floor during preparation lands before the check (and
  refuses the start) or after the record.
- A resume passes it in `ResumeRequest.launch` through every shared engine
  path. The engine commits it in `try_journal_resumed`, under the run's
  event lock, after rechecking that the run is still unauthorized, that the
  snapshot and pins still give the grant's permit digest and that the policy
  state is unchanged; only then are a legacy run's pins persisted and
  `run_resumed` appended. A refused, failed or superseded resume journals
  nothing.

A decision crosses a process boundary only through a handoff file
(`apb_engine::launch_handoff`): `.apb/handoff/<128-bit random>.json`, created
exclusively with mode 0600, its path the only thing on argv. The receiver
touches only a file named like a handoff in its own project's
`.apb/handoff` (compared canonically; any other path is refused and left as
it is), opens it without following a link and without blocking, checks the
open file, then deletes it, and accepts it only when it is younger than 60
seconds, a regular file of the user with mode 0600, and written by its own
parent. The writer waits until it is taken and deletes it when the spawn
fails. Three kinds exist:

- `Supervise`: `apb run --supervise` asks at the terminal (and obtains the
  irreversible consent) in the parent; `__drive-supervised` recomputes the
  permit with the fixed choice and accepts the decision only when its own
  nonce matches. The irreversible consent travels in the same file and
  keeps its own nonce check; it no longer appears on argv.
- `Eval`: `apb eval` checks the launch once, before any case starts; each
  case's `apb run` accepts the decision only for the same playbook id,
  version, trust digest and digest of the verified child pins (its scratch
  tree has another workspace, so a nonce would not match), and only while
  the tree's policy is not stricter than the one decided, and records the
  parent's initiator and confirmation.
- `Resume`: a detached resume (`__drive-run --resume --launch-handoff`)
  receives the grant it commits.

Without a valid handoff the receiver is the CLI without a terminal.

`apb eval --yes` approves the suite and its cost only; `apb eval` has its
own `--confirm-launch`, `--launch-version` and `--launch-choice`, and the
terminal question.

## 7. Surfaces

- MCP: `playbook_run`, `playbook_trial`, `playbook_execute_plan` and
  `run_resume` take `confirm_launch`, `launch_version` and `launch_choice`;
  refusals carry `start_actions` with the codes `confirm_launch` and
  `launch_by_person`. The server reads the run-step markers like the CLI.
  No new tool joins the read-only set, and every floor or grant write
  checks the process-wide read-only guard.
- CLI: `apb run`, `apb resume` (one run and `--all`) and `apb eval` take
  `--confirm-launch`, `--launch-version` and `--launch-choice`; at a terminal
  they ask instead. `apb launch floor <id> [--global] [--lower-to
  <policy>]` prints or lowers a floor.
- HTTP and dashboard: a dashboard with keys passes the dashboard initiator,
  one without keys the keyless one, whose start and resume take
  `confirm_launch` (with `launch_version` and `launch_choice` for a start);
  a start with `continued_from` is a continue; the start dialog and the fit
  card show the effective policy and the floor; the editor's save lowers a
  floor only as section 3.3 says, and asks the person first.
- Fit: `FitReport.launch` (`LaunchView`: policy, sources, floor, and the
  problem when the policy could not be read).
- Run surfaces: `run_status`, `run_report`, `apb runs <id>`, the HTTP run
  detail and the run page show the authorization, added before each
  surface's final redaction. Execution reads the record only from the
  manifest and the journal.
- Doctor: `apb doctor` prints both settings files, the floors of the
  project's playbooks, and warns when another apb is on `PATH`.

Generated TypeScript gains `LaunchPolicy`, `LaunchView`, `LaunchRecord`,
`RunDetail.launch` and the two start action codes.

## 8. Journal and older versions

No event type is added. The new records are optional fields:
`Playbook.agent_launch`, the manifest's `launch` and `RunResumed.launch`
(`#[serde(default)]`, omitted when absent, decoded leniently so a shape a
newer apb wrote reads as absent). A legacy run's persisted pins use the
existing `expected_children` field of its run config.

An older apb drops `agent_launch` when it loads a playbook, ignores the
manifest's `launch` and `run_resumed.launch`, never reads `policy.yaml`,
`launch-floors.json` or a handoff file, and runs every playbook as `allow`.
It reads, resumes and stops runs this release wrote; it does not enforce
their policy. The journal corpus checks this against 1.0.0-beta.1 and
0.25.0. The policy therefore holds only while every apb on the machine is
this release or newer, which `apb doctor` warns about and
`docs/GUARDRAILS.md` states.

A later release that adds a section to `policy.yaml` makes this release
refuse agent launches on a machine whose file uses it, until apb is
upgraded; that is the strict reader failing closed.

## 9. The exact guarantee

An agent that uses apb's documented agent channels (MCP, or the CLI inside
a run step or without a terminal) cannot start, trial, evaluate or resume a
playbook whose effective policy is `deny`, and cannot do so for `ask`
without passing the nonce of the refusal; the CLI at an interactive terminal
asks the person. An agent channel cannot loosen a playbook's declared
policy.

The guarantee holds whatever an agent writes into the project tree
(`.apb/workspace.local` keys nothing it was not registered for, a damaged
`.apb/policy.yaml` refuses an agent channel and the engine's backstops),
whatever config dir it names in the environment (section 4.2), and through
a dashboard without keys (section 4.2).

Outside it: proof that a person answered (a process that controls a
terminal, signs in to a dashboard with a key, edits the machine files or
forges the handoff file is not stopped); the project file, which anything
that writes the project tree can change and which only narrows; a new playbook
an agent writes under a new id, which has no floor of its own and falls
under the machine and project defaults (a machine default of `ask` covers
it); and older apb binaries on the same machine.

## 10. Not in scope

Proving that a person answered; policies for script steps; per-user
policies; recognizing a copy of a playbook saved under a new id.
