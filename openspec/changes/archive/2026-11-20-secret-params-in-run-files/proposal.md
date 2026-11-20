# Proposal: secret params in run files

## Why

A param declared `secret: true` is the operator's private value. apb
keeps it for the life of a run in `runs/<id>/run.yaml`
(`RunConfig.params`, `crates/apb-engine/src/run_config.rs` line 156,
written with mode 0600 by `write_run_config`, line 333), because every
resume reads it there. On main efae5822 (Wait park, #290; apb
1.0.0-beta.7) a playbook may also reference the value in any template:
`{{params.<secret>}}` renders it into a prompt, a host task's
`prompt.md`, a `prompt` node's journaled output, a path, a cache key, a
publish branch or a connector argument, and from there into logs,
session stores and the journal. V99 and V100
(`crates/apb-core/src/validate/params.rs` lines 13 to 31) refuse only a
secret with a default and one that is not text.

The first design of this change scrubbed the value out of every file
after it was rendered. Its review found one blocker and ten major
findings: every pass found another write site, another encoding, another
crash window, and the scrub changed runs that have no secret at all. That
approach does not converge (design "Why the first design was replaced").

This was found in the design review of one-point-oh-fixes (its D4.12).

| Example | Code | Non-code |
|---|---|---|
| a prompt reads a secret | V340 refuses `prompt: "deploy with {{params.token}}"`; the deploy script reads the token from `run.yaml` | V340 refuses a CRM routine's prompt that reads its API token; the connector's account secret carries it |
| a path reads a secret | V340 refuses `workdir: "build/{{params.token}}"` | V340 refuses a `writes` entry that names a mailbox password |
| a stored version | starting a stored review playbook whose prompt reads a secret is refused before any run directory exists | the same for a stored mail triage playbook |

## What changes

1. apb never renders a secret param's value into text. V340 refuses
   every reference to a param declared `secret: true`, in every form the
   renderers resolve, in every string of the definition, keys included;
   a definition that cannot be walked is refused, never passed (design
   D2, D3).
2. A step that needs the value reads it from `run.yaml` through
   `APB_RUN_DIR`, as on main: scripts, plain cli agents and host-mode
   subagents can; a sandboxed agent cannot, and a `success_check` is not
   told where it is (design D5). Every unsandboxed step runs as the
   operator and can read every unpruned run's `run.yaml` in the project;
   `isolation: full` keeps a step away from other runs' files. The file's path, its `params` key (the
   run's resolved values: given, recalled or defaulted), its quoting and
   its mode become a documented contract. No delivery channel is added.
3. A param once declared secret in a run stays secret for that run and
   for every version a patch creates from it: an in-run or forward patch
   that drops the declaration is refused (`secret_declaration_dropped`),
   and every check judges by the run's full secret set (design D11). An
   in-run patch records new secret names only after its snapshot is
   written, so a refused or interrupted patch never makes a valid run
   unresumable; a forward patch rechecks the source run's set under the
   candidate lock it already holds, right before it writes its
   candidate (no new lock). Every check outside validation judges every
   string of the definition, the fields V104 and V210 own at validation
   and `previous_success.when_none` included.
4. A run whose definition renders a secret is refused at resume
   (`secret_param_rendered`), in the synchronous part of every resume
   entry, so the person who asked sees it, before anything is written
   (the legacy manifest of a schema-1 run included, and on the CLI, MCP
   and the dashboard no consent either: each runs the read-only check
   before it records a consent, so nothing is written when the snapshot
   it reads renders a secret; when another process changes the snapshot
   between that check and the engine's own, the engine still refuses and
   a consent may have been recorded); a stored version
   that does is
   refused at start by the validation preparation already runs, and a
   waiting candidate that does is skipped for `current` (design D7).
5. The scenario capture takes the run's declared secret set from the
   manifest and the snapshot, so a declaration dropped before this change
   still keeps the value out (design D6).
6. The run's secret set is read strictly: a manifest or snapshot that
   does not read is a refusal (`secret_set_unreadable`) on every caller,
   never an empty set (design D11).
7. The rule is enforced by this apb and later ones. An older apb never
   had it, so an older binary that resumes or patches a run can render a
   secret param exactly as it could before; mixing binaries on one run is
   outside the guarantee (design D7 "Older binaries").

Capability `secret-param-storage`.

## Out of scope

1. Removing the value from `run.yaml`: every resume and every script
   reads it there.
2. What a step prints or writes after it read the value itself: that is
   the step's output, stored as on main and redacted on the way out as on
   main (design D8).
3. A store-time scrub of any kind; the short value warning stays a
   warning (design D9).
4. A sub-playbook param binding (none exists on main) and an environment
   hand-over to agents (design "Future work").
5. A version bump. The release notes lines go into the next release.

## Impact

1. Crates: `apb-core` (V340 and `secret_param_refs` in
   `validate/params.rs`), `apb-engine` (the run's secret set and the one
   check in `run_redact.rs`, the resume checks in `scheduler/resume.rs`,
   the patch refusals in `scheduler/patch.rs` and `forward_patch.rs`, the
   candidate skip in `gate/mod.rs`, the declared set in
   `scenario/capture.rs`). In the `apb` CLI, `apb-mcp` and `apb-server`,
   one call each: the resume surface runs the engine's read-only secret
   preflight before it records a consent; the refusals reach every
   surface through the mappings it already has.
2. Journal: no new event, event field, run guard name or manifest mark.
   A refused patch is the existing `patch_rejected`. A run with or
   without a secret param writes exactly the journal it wrote before, by
   construction (design D8).
3. Breaking for authors: a playbook that renders a secret param is
   refused at validate, save and start, and its in-flight runs at resume;
   a patch may no longer declassify a param a run holds secret. No
   shipped playbook or example does either (design D7).
4. Validator: V340 (error); V341 to V349 reserved (design D0).
5. Size: 26 changed files, 27 with the release notes (design "Files";
   the first design listed 42).
