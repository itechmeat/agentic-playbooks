# Proposal: one point oh fixes

## Why

apb 1.0 needs a set of small defects and unverified paths from earlier
changes closed before its schema, journal and MCP tools are frozen. These
are the 1.0 plan's "before 1.0" verdicts for this change (plan section 4,
items 2, 4, 13, 16, 19, 41, 44, 52, 54, 67, 70 and 75), and two
connector defects another review found. Each was checked
again on main efae5822 (Wait park, #290; apb 1.0.0-beta.7), which holds
typed-inbox-reads (#287), people-in-the-loop (#288), before-running-alone
(#289) and wait-park (#290). None was done there; every one below is
still true. The changes in flight (eval-doubles, start-rules,
incremental-lists-and-session-fork, inbox-waits, and the mcp drafts) are
not preconditions; `design.md` names the files they share with this one.

1. GitLab `create_release` is not sent once
   (`connectors/gitlab/connector.yaml` line 252: no `idempotency`, and
   the connector has no `identity` block). GitLab keys a release by its
   tag and answers a second creation for the same tag with `409
   Conflict`, so a retry after a lost answer fails the node although the
   release was created.
2. Only `cargo test` failing tests are read
   (`crates/apb-engine/src/baseline/project.rs` lines 15 to 17 and 270):
   every other runner reads unknown while it keeps failing.
3. The dashboard profile editor has no `process_env` field
   (`web/src/pages/ProfileEdit.svelte`, no match for `process_env`)
   although the save body takes it (`web/src/lib/api/core.ts` line 108,
   `crates/apb-server/src/routes/profiles.rs` line 166).
4. A copy to another scope, or a draft creation, killed between its claim
   and its write leaves an empty `playbooks/<id>` that refuses the id
   (`crates/apb-core/src/scope_move.rs` lines 68 to 75); nothing reports
   it.
5. The hermes resume form is invalid: `["-z", "--resume", "{session}",
   "{prompt}", ...]` (`crates/apb-core/src/agents/hermes.rs` line 71)
   gives `-z` no value. Checked against the installed hermes 0.23.2:
   `hermes -z --resume s hello -m m` exits with `argument -z/--oneshot:
   expected one argument`.
6. The adapter type is still named `ClaudeAdapter`
   (`crates/apb-engine/src/adapter.rs` line 836) although it serves every
   agent; 18 files name it.
7. Three behaviours were never tested: a CLI run (no host session) whose
   list item cannot start its agent (only the host-session case is,
   `list_run_test.rs` line 799); commits of list items under one worker
   and none under several (`scheduler/list/batch.rs` line 402,
   `track_provenance: run.workers == 1`, and the tracker keyed by node,
   `scheduler/provenance.rs` lines 46 and 236); a join collector whose
   branch of the earlier round finished while the other branch of the new
   round has not started (the case `join_loop_test.rs` line 123 covers in
   part).
8. A secret param's value reaches files under the run directory other
    than `run.yaml`: split out into its own change,
    secret-params-in-run-files.
9. Two connector defects: `apb connector approve` is not refused
    inside a run step (`crates/apb-cli/src/connector.rs` line 345), and
    `POST /api/connectors/approve` takes an approval from any caller that
    reaches the dashboard, a keyless request from a run step's script
    included (`crates/apb-server/src/routes/connectors/mod.rs` line 520);
    an inbox grant suppresses the untrusted-output paragraph for
    the node's other untrusted functions
    (`crates/apb-engine/src/connector/prompt.rs` lines 88 and 89).

The freeze of the 1.x promise, the template gallery, global playbooks
(`apb run --scope global` and the dashboard) and the release candidate
follow in the next change, one-point-oh-freeze. This change adds
`project_checks.junit` and V320, which that change freezes, so it is
merged first.

| Item | Code example | Non-code example |
|---|---|---|
| D4.1 GitLab release | `create_release` carries a marker in its description: a retry after a lost answer finds the release and settles as already sent instead of failing with 409 | the same marker rule keeps a release announcement of a documents project from being created twice |
| D4.2 JUnit | `project_checks.junit: "reports/*.xml"` reads the failing tests of a jest suite, so the baseline names a newly failing case | a document build whose checker writes JUnit XML reports a new failing check the same way |
| D4.3 profile editor | an implementer profile set to `allowlist` with `GITHUB_TOKEN` in the dashboard | the profile editor sets a `process_env` allowlist for a mail-drafting profile, so it sees only its mail account variable |
| D4.6 doctor | a copy to the global store killed after its claim | `apb doctor` names the empty playbook directory a stopped copy of an invoice routine left, with a quoted `rmdir` command, and asks to inspect any directory that is not empty |
| D4.9 hermes | a hermes code review node that resumes gets a valid command line | a hermes mail triage node gets a valid resume command line |
| D4.13 connectors | an implementer step cannot approve the GitHub connector for itself, on the CLI or through a dashboard without keys | a mail triage step that reads an inbox and searches the web is warned about both kinds of outside text |
| D4.10, D4.11 | `CliAdapter` names what serves every agent; three tests pin list and join behaviour | a mail triage list run records each item's own result under one worker |

## What changes

1. GitLab `create_release` sent once by marker (with `identity` and
   `list_releases`); the connector goes to 0.3.0 (design D4.1).
2. `project_checks.junit` read with a bounded JUnit XML reader, validator
   V320 (design D4.2).
3. `process_env` in the dashboard profile editor, read from the profile's
   YAML; no server change (design D4.3).
4. `apb doctor` names a stale claimed playbook directory, with a removal
   command only for an empty one (design D4.6).
5. The hermes resume form fixed (design D4.9).
6. `ClaudeAdapter` renamed `CliAdapter` (design D4.10).
7. The three missing tests (design D4.11).
8. Connector approval refused inside a run step on the CLI and taken
   over HTTP only from a signed-in dashboard session, and the
   untrusted-output paragraph beside an inbox grant (design D4.13).

All of it is capability `one-point-oh-leftovers`.

## Out of scope

1. Text mode on GitLab release text. The plan asked for "text mode and
   marker"; GitLab runs no quick action from release text
   (`connectors/gitlab/PUBLIC.md` lines 101 and 102, `docs/CONNECTORS.md`
   lines 1156 to 1158), so `markdown_no_commands` would only add visible
   backslashes. Only the marker is added.
2. Secret param values under the run directory (plan item 16): the
   change secret-params-in-run-files.
3. A flip of the `process_env` default (plan item 10, verdict NO): listed
   under "Decisions pending the owner".
4. Script env policy (plan item 11): re-verified, a script check in a
   sandbox gets only the base set (`crates/apb-engine/src/script.rs`
   lines 200 to 203); nothing to do.
5. A `process_env` key in MCP `profile_get`: the answer already carries
   the stored value inside `profile_yaml` (`profile_tools.rs` line 128),
   which the editor parses; a second copy is not added.
6. A hermes session that one-shot mode never prints: hermes `-z` prints no
   session id, so apb rarely learns one; only the form's command line is
   fixed.
7. `apb run --scope global` and global playbooks in the dashboard (plan
   items 17 and 18), `GCM_INTERACTIVE` on macOS (item 63), the
   compatibility freeze, the template gallery, the platform position and
   the release candidate: one-point-oh-freeze.
8. A version bump: only one-point-oh-freeze (1.0.0-rc.1) and the 1.0.0
   release bump apb.
9. Agent homes under `isolation: full` (a moved claude configuration
   directory, zcode, hermes, opencode): a later change of its own. Agents
   under isolation stay exactly as on main: claude and codex declared,
   every other agent refused `agent_needs_unknown`, `CLAUDE_CONFIG_DIR`
   not followed. Three reviews of this change found why it needs its own
   design: a moved directory reached through a symlink ancestor in a
   writable place can be swapped between the check and the spawn; agents
   read config from paths their variables name (zcode's
   `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE` and
   `ZCODE_PERSONAL_PROVIDER_CONFIG_FILE`, opencode's `XDG_DATA_HOME`,
   `XDG_CONFIG_HOME` and `OPENCODE_CONFIG_CONTENT`), which home binds do
   not follow; and an older apb could resume such a run under the old
   credential rules unless a run guard keeps it out.
10. A `{{cmd:...}}` secret helper named by a bare program name is still
    found through the caller's `PATH`
    (`crates/apb-core/src/connector/secrets.rs` line 250). A fixed search
    path would not bind the approval to the program that runs: a writable
    prefix or an absolute path can be replaced after approval. The right
    fix pins the helper's path and content at approval, the mechanism
    mcp-tool-connectors introduces for its server program; it is future
    work beside that mechanism.
11. An attempt lifecycle for script executions (commits per attempt for
    script list items). A script execution journals no `attempt_started`
    or `attempt_finished` (the `NodeKind::Script` arm of
    `scheduler::node::execute_node_kind`), and the provenance tracker is
    fed only from the agent arms (`scheduler::provenance::attempt_started`),
    so `artifacts_committed.attempts` of a script node is always empty
    (`run_outcome::commits` documents it). Journaling one changes the
    journal of every script run and every fold that reads attempts
    (liveness, execution identity, exec keys, the run surfaces), with its
    own numbering and close reasons: a change of its own. This change pins
    what holds today: one commit record per item under one worker, tied to
    its item by journal order and subject.
12. A connector approval bound to a terminal or a passkey. The CLI refusal
    of this change reads the caller's environment, a mistake guard a
    process can clear; a terminal-only or passkey-bound approval is a
    change of its own, and MCP connectors get the terminal rule in their
    own change.
13. A profile save that loosens `process_env` (from `allowlist` to
    `inherit`, or more allowed names) should need a person, as a
    loosening of the launch policy does. That holds on main for every
    channel a profile is saved through; it is a change of its own.

## Impact

1. Crates: `apb-core` (new `project_checks/junit.rs`;
   `ProjectChecksSpec.junit`; V320 in `validate/baseline.rs`; `scope_move::stale_claims` and
   `fsutil::lock_held`; `versioning::is_draft_debris`; `Beneath::file_stamp`; the hermes resume
   form), `apb-engine` (`CliAdapter`; the JUnit read in
   `baseline/project.rs`, charged to the command's deadline and read only
   in a run that holds its tree (`ReadCtx.holds_tree`); the
   untrusted-output paragraph in `connector/prompt.rs`), `apb` CLI
   (doctor line; the rename in `run.rs`; the approve refusal in
   `connector.rs`), `apb-server` (the session-only approve in
   `routes/connectors/mod.rs`), `web` (`process_env` fields).
   `apb-mcp` does not change; it reaches the new refusals through the
   shared gate and preparation.
2. Bundled content: the GitLab connector goes to 0.3.0 (a content
   version, not an apb bump).
3. Journal, manifest, run guards, signed stores: nothing new. A run that
   does not set `junit` journals exactly what main journals, digest
   included.
4. Validator: V320 (error); V321 to V339 reserved for this family. Free on
   efae5822 (highest code V250) and against every other change's block
   (`design.md` D0).
5. Older apb: refuses a definition that sets `project_checks.junit` with
   a parse error; reads everything else this change writes as before.
6. Behaviour changes for release notes:
   1. GitLab `create_release` carries a marker in its description: a
      retry after a lost answer, which failed with 409, is now
      recognized as already sent (looked up with `list_releases`); an
      unknown outcome is a send a person resolves; the connector reads
      its own account (`identity`).
   2. `baseline_check` `project_checks.junit` reads failing tests from
      JUnit XML reports, in a run that holds its working tree; a run
      that shares it reads them unknown. An older apb refuses a
      definition that sets it.
   3. hermes's resume form is a valid command line.
   4. `apb doctor` names a playbook directory a stopped copy or draft
      creation left: an empty one with a quoted `rmdir` command, any
      other with its path and a request to inspect it.
   5. The dashboard profile editor sets `process_env`.
   6. Newly refused by name: `apb connector approve` inside a run step;
      `POST /api/connectors/approve` from a dashboard without keys or a
      bearer key (403, the CLI command named).
   7. A node granted an inbox function beside other untrusted functions
      gets both warnings.
7. Size: 62 changed files (`design.md` "Files").
