# Tasks: secret params in run files

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta
`specs/secret-param-storage/spec.md`. Tick each box (`- [x]`) as soon as
the task is done, so an interrupted run can resume from the first open
box. Partial edits from an earlier attempt may already be in the tree:
review them against `design.md`, keep what is right, finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by the
name `design.md` gives, keep the decision, and note the difference in the
final report. Never invent a different behaviour, and never add a scrub:
`design.md` "Wrong implementations" lists what this change must not do.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first failure
only for a test that names a type or function that does not exist yet
(`apb_core::validate::secret_param_refs`, `SecretParamRef`).

Line numbers are those of main efae5822 and are to be re-verified on the
main the implementer starts from.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-sp-target TMPDIR=$HOME/.cache/apb-sp-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache` fails with a read-only file system, use
`CARGO_TARGET_DIR=/tmp/apb-sp-target` and
`TMPDIR=/tmp/apb-sp-tmp` instead, and before each build check
that `df -h /tmp` shows more than 4G free; if not, stop and report it.
Never use `/dev/shm`, and never put a target dir, cache or temp data
inside the worktree.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Do not commit, push or open a pull request. Do not
start subagents. Search the code with `zg --rg -F <symbol> <path>` first.

Inside a task run only the tests it names:

1. A unit test module: `cargo test -p <crate> --lib <module path>`.
2. An integration test file: `cargo test -p <crate> --test main <file
   module>`. The CLI package is named `apb`: `cargo test -p apb --test
   main run_secrets_cli_test`.
3. `cargo test` takes one name filter per invocation: two modules are two
   commands.

## 0. Preconditions

- [x] 0.1 Record `git log --oneline -1` and `grep -n '^version' Cargo.toml`.
  List `openspec/changes/archive/` and note which of one-point-oh-fixes,
  eval-doubles, start-rules, incremental-lists-and-session-fork,
  inbox-waits, mcp-tool-connectors and one-point-oh-freeze are archived
  (merged). If one-point-oh-freeze is archived, stop and report it: it
  freezes the validator and refusal codes this change adds (design
  "Shared files").
- [x] 0.2 Codes: `git grep -ohE '"V[0-9]+"' crates | tr -d '"V' | sort -n |
  uniq | tail -3` and `git grep -nE '"V34[0-9]"' crates`. V340 must be
  free; if a merged change took it, stop and report it.
- [x] 0.3 Confirm each symbol at its file, by name (`zg --rg -F`), and
  record the line:
  1. `check_params` in `crates/apb-core/src/validate/params.rs` and its
     call in `crates/apb-core/src/validate/mod.rs` (`validate`);
  2. `refs` in `crates/apb-core/src/template.rs`;
  3. `render` and `resolve` in `crates/apb-engine/src/context.rs`, and
     `render_text` in `crates/apb-engine/src/scheduler/list/check.rs`:
     confirm both trim the reference and resolve `params` as design D2
     says (two segments in `resolve`, `split_once('.')` in
     `render_text`);
  4. `NodeKind::Playbook` in `crates/apb-core/src/schema.rs` has no
     params field, and `child_run_options` in
     `crates/apb-engine/src/scheduler/node.rs` leaves `params` at its
     default (design D4);
  5. `validate` in `crates/apb-engine/src/scheduler/prepare.rs` (the
     "playbook `<id>` is invalid" refusal) and that it runs before
     `allocate_run`;
  6. `resume_detached_inner` (the `parse_snapshot_playbook` call after
     the busy-lock comment), `resume_inner_checked` (its
     `parse_snapshot_playbook` call, then `EventLog::open`,
     `run_definitions::on_resume`, the drift `log.append`,
     `concurrency::reserve_resume` and `acquire_tree_for`, in that order)
     and `try_journal_resumed` in `crates/apb-engine/src/scheduler/resume.rs`;
     `try_engine_resume` in `crates/apb-engine/src/pause/waker.rs` and
     that it calls `resume_detached_held` or `resume_detached_req`. Confirm
     that the only non-test `EventPayload::RunResumed` append outside
     `try_journal_resumed` is the scenario-only `engine_pause` in
     `crates/apb-engine/src/scheduler.rs` (the one in
     `scheduler/wait_park.rs` sits in its `mod tests`); another resume
     path that appends `run_resumed` is a stop: report it;
  7. `secret_names` in `crates/apb-engine/src/run_redact.rs` and the two
     `run_params::sensitive(p.secret, ...)` sites in
     `crates/apb-engine/src/scenario/capture.rs`;
  8. `run_env` in `crates/apb-engine/src/script.rs` and the `APB_RUN_DIR`
     line of `crates/apb-engine/src/adapter/spawn_env.rs`;
  9. `apply_patch`, `append_patch_rejected`, the `registry.load` call,
     `keep_secret_declarations`, the `LOOSENED` refusal, the
     existing `super::prepare::run_guard` call (which this change leaves
     as it is), `snapshot_playbook` and the
     `PatchApplied` append in `crates/apb-engine/src/scheduler/patch.rs`,
     in that order; `create`, `refused`, `read_all`, the
     `definition_guard` call, `apb_core::candidate::lock`,
     `check_patch_budget` and `create_forward_patch` in
     `crates/apb-engine/src/forward_patch.rs`, in that order (the
     forward patch's recheck goes between the last two, under the lock
     `create` already holds; `apply_patch` gets no lock, design D11 item
     4);
  10. the candidate branch of the gate in
     `crates/apb-engine/src/gate/mod.rs` (`choose_at_gate`,
     `check_run_loaded`, `skip_reason`, `CANDIDATE_SKIPPED_UNTRUSTED`);
  11. `write_run_config` in `crates/apb-engine/src/run_config.rs` (it
     writes through `yaml_serde::to_string` and
     `fsutil::atomic_write_private`) and `RunConfig.params`; in
     `crates/apb-engine/src/scheduler/prepare.rs` the recall
     (`run_memory::apply`) and the defaults loop before the `RunConfig`
     is built (design D5 "The run.yaml contract for steps" item 2);
  12. the legacy shim in `resume_inner_checked`
     (`legacy_snapshot::has_legacy_executors`,
     `build_ephemeral_manifest`, `manifest::write`) and that it comes
     before `parse_snapshot_playbook` (design D7 item 5.2);
  13. the consent writes before a resume (design D7 item 5.3):
     `resume_consent_need` (its `replace_consent(.., None)`) and
     `record_resume_consent` (the legacy manifest, then the consent) in
     `crates/apb-engine/src/gate/mod.rs`, and their callers: the
     `resume_consent_need` match and `record_resume_consent` call in
     `crates/apb-cli/src/run.rs`, `check_resume_consent` in `run_resume`
     of `crates/apb-mcp/src/server/run.rs` and in the resume route of
     `crates/apb-server/src/routes/runs.rs`. A fourth surface that
     writes into a run before the engine's resume is a stop: report it.
     Also confirm `replace_consent` and `amend` in
     `crates/apb-engine/src/manifest.rs` take no lock (design D7 item
     5.3, the exact guarantee);
  14. the lenient reads in `crates/apb-engine/src/run_redact.rs`:
     `secret_names` (the manifest read through `.ok().flatten()`) and
     `snapshot_declarations` (the snapshot read and parse through
     `.ok()`), which `secret_names_strict` must not copy (design D11
     item 1); and the `is_safe_segment` checks at the top of
     `resume_detached_inner` and `resume_inner_checked`, which the
     preflight repeats (design D7 item 5.3).
- [x] 0.4 `rg -n "serde\(skip\)" crates/apb-core/src/schema.rs`: no field
  that holds a template may be skipped (design D3); a skipped templated
  field is a stop: report it.
- [x] 0.5 Shipped content: `rg -n "secret: true" bundled examples` finds
  no playbook param (design D7). A shipped playbook or example that
  declares a secret param and references it in a template is a stop:
  its fix becomes part of this change, report it first.
- [x] 0.6 Fixtures: run `rg -l "secret: true" crates` and, for each file,
  list every `params.<name>` reference whose name the same fixture
  declares secret. The files and their treatment are decided in design D7
  "In-repo fixtures": `run_secrets_test.rs`, `run_secrets_cli_test.rs`,
  `secret_runs.rs` and `validate_untrusted_test.rs` change as written
  there; `untrusted.rs`, `judge_eval/sandbox.rs`, `judge_eval/example.rs`,
  `validate_quality_test.rs`, `validate_previous_success_test.rs` and
  `validate/concurrency.rs` stay. A file outside that list that renders a
  declared secret through `validate` or preparation is a stop: report it.

## 1. Tests first (design D10)

Write each test, run it, record its first failure.

- [x] 1.1 `crates/apb-core/tests/suite/validate_params_test.rs`: test 1 of
  design D10, one case per site of the design D3 table, each exactly one
  V340 naming the node and the field.
- [x] 1.2 Same file: test 2 (the forms that render and the ones that do
  not).
- [x] 1.3 Same file: test 3 (a literal field reports V340; a plain param
  and an unreferenced secret report none) and the validation half of
  test 4 (V104, V176, V210 alone on their fields; `secret_param_refs`
  itself returns a hit for each). (First fix round: the `when_none` path
  is V340's, design D3 item 5 as amended.)
- [x] 1.4 Same file: the `instruction` case of test 5 (V340 on a
  `playbook` node's `instruction`).
- [x] 1.5 Same file: test 9 (the null, sequence and string keys under
  `supervisor.policy`; the walk error, through a unit test in
  `validate/params.rs` over the walk's inner function, generic over
  `Serialize`, given a type whose `serialize` returns a custom error).
- [x] 1.6 Same file: test 14 (every bundled playbook and every playbook
  under `examples/` gives no hit).
- [x] 1.7 `crates/apb-engine/tests/suite/run_secrets_test.rs`: the child
  case of test 5 (a child's declared secret starts empty; its `run.yaml`
  holds no parent value).
- [x] 1.8 Same file: test 6 (a stored version with the old form refused
  at `run`, naming V340, no run directory left).
- [x] 1.9 Same file: test 7 (a pre-change run: (a) `resume_detached`
  returns `secret_param_rendered` to the caller and spawns no driver;
  (b) the in-process resume returns it; no event appended, slot file and
  busy lock as the pause left them; `stop` still ends it; (c) a snapshot
  without the declaration and a manifest naming it is refused alike;
  (d) a schema-1 run with no manifest whose prompt reads the secret is
  refused in process and no `manifest.yaml` exists afterwards; (f) a
  run whose consent is recorded and whose snapshot is then replaced by
  one that renders the secret: the engine's resume is refused
  `secret_param_rendered` with nothing journaled, and the test asserts
  the refusal, not the absence of the consent; (g)
  `resume_secret_preflight` with an unsafe id and with a missing run
  directory answers `EngineError::NotFound`).
- [x] 1.10 Same file: test 8 (only `run.yaml` holds the value, by name
  and content, mode 0600, `params.message` of the parsed file equals it;
  the start key file, memory file, manifest, `estimate.json` and report
  page hold none; a value YAML must quote reads back unchanged; `params`
  holds the given secret, a recalled plain param and a defaulted plain
  param, and a param with none of the three is absent).
- [x] 1.11 Same file: test 11 (in-run patches: a dropped declaration and
  a removed but referenced param rejected `secret_declaration_dropped`; a
  stored version that renders the secret rejected
  `secret_param_rendered`; `playbook.yaml` and the manifest unchanged;
  (d) a patch that declares a plain param secret and loosens
  `agent_launch` rejected `launch_policy_loosened` with the manifest not
  naming the param and a later resume not refused; (e) the crash
  boundary as on-disk states, no fault seam in the code; (f) an applied
  patch's new name in the manifest).
- [x] 1.12 Same file: test 12 (a forward patch that drops the
  declaration refused `secret_declaration_dropped`, no candidate
  written; the recheck: the forward patch's YAML made from the run as it
  started, an in-run patch that declares `token` secret applied after
  that, and the forward patch then refused `secret_declaration_dropped`
  with no candidate written). No test holds or contends for the
  candidate lock.
- [x] 1.13 Same file: test 13 (a waiting candidate that renders a secret
  skipped `refused: secret_param_rendered`, one that drops a declaration
  skipped `refused: secret_declaration_dropped`; the start runs
  `current`).
- [x] 1.14 `crates/apb-engine/tests/suite/scenario_capture_test.rs`: test
  10, the capture of a run whose manifest names a secret its snapshot no
  longer declares. The judge example half of test 10 is already covered
  by `judge_eval/example.rs` (the `SECRET_PLACEHOLDER` assertions); name
  the existing test in the report instead of adding one.
- [x] 1.15 Test 15 of design D10, the strict secret set: the unit tests
  of `secret_names_strict` in `crates/apb-engine/src/run_redact.rs`
  `mod tests` (the union; no manifest gives the snapshot's; an
  unreadable manifest, a missing `playbook.yaml`, a `playbook.yaml`
  replaced by a directory and one that does not parse each give
  `secret_set_unreadable` naming the file); in `run_secrets_test.rs`
  the crash state between the snapshot replacement and the manifest
  update with the snapshot then unreadable and, in a second case,
  unparsable (the forward patch refused `secret_set_unreadable`, no
  candidate; the preflight and the in-process resume refuse with
  nothing journaled), and an in-run patch on a run whose manifest does
  not read journaled `patch_rejected` with `secret_set_unreadable`,
  `playbook.yaml` byte-equal.
- [x] 1.16 `run_secrets_test.rs`: test 16 (a run of a playbook that
  declares a secret param journals no `run_guard`, the same sequence of
  event types as the same playbook without the declaration, and a
  manifest with no field main's lacks).
- [x] 1.17 `run_secrets_test.rs`: the outside-validation half of test 4,
  for each of `quality.task_type`, `previous_success.when_none.started_at`
  and `concurrency.key`: (a) a waiting candidate skipped `refused:
  secret_param_rendered` and the start runs `current`; (b) a
  `Control::Patch` naming a stored version rejected
  `secret_param_rendered`, `playbook.yaml` byte-equal; (c) a paused run
  whose snapshot reads the secret there refused at resume with nothing
  appended.
- [x] 1.18 Test 7 (e), the consent surfaces: the CLI case in
  `run_secrets_cli_test.rs` (`apb resume <run> --confirm-irreversible
  true` exits 2 with `secret_param_rendered`), the MCP `run_resume` case
  (`confirm_irreversible: true`, a tool conflict) and the dashboard case
  (`confirm_irreversible: true`, HTTP 409) in `run_secrets_api_test.rs`;
  in each, `manifest.yaml` byte-equal to before, and for the schema-1
  run no `manifest.yaml` afterwards.

## 2. V340 (design D2, D3)

- [x] 2.1 `crates/apb-core/src/validate/params.rs`: `SecretParamRef`,
  `SecretWalkError` and `pub fn secret_param_refs(playbook: &Playbook,
  also_secret: &BTreeSet<String>) -> Result<Vec<SecretParamRef>,
  SecretWalkError>` exactly as design D3 items 1 to 5 say (the set first
  and an early `Ok(vec![])` when it is empty; one `yaml_serde::to_value`;
  every scalar string, keys included, no path skipped; the matcher of
  design D2 over `apb_core::template::refs`; an error returned, never
  swallowed). Re-export the three names from
  `crates/apb-core/src/validate/mod.rs`.
- [x] 2.2 `check_params` calls it with an empty set, drops a hit whose
  `node` is `None` and whose `field` is one of the four paths V104, V176
  and V210 own (design D3 item 5; nothing else filters them; first fix
  round: two paths, `quality.task_type` and `concurrency.key`), and
  reports every other hit, and an `Err`, as V340 with the messages of design D3 items 3 and
  6. Update the module comment and the comment on the `check_params` call
  (`// V99, V100, V177, V178` gains V340). Run 1.1 to 1.6 until green.

## 3. Fixtures (design D7 "In-repo fixtures")

- [x] 3.1 `run_secrets_test.rs`: `MONO`'s `p2` becomes `prompt:
  "second"`; split `a_patch_that_drops_a_secret_declaration_never_drops_the_redaction`
  into the three tests of design D7 fixtures item 1 (the refused patch
  is test 1.11's first case); `seed_definition` gains an optional list of
  script files; the routing test's `p` becomes the `print.sh` script node
  of design D7 fixtures item 1.
- [x] 3.2 `run_secrets_cli_test.rs`: `HOST_PLAYBOOK`'s prompt and the
  renamed, reversed host test exactly as design D7 fixtures item 2 says.
- [x] 3.3 `secret_runs.rs`: the literal `instruction`, the `SAY` script
  that reads the parent's `run.yaml` through `parent_run`, and the module
  comments of `secret_runs.rs` and `run_secrets_api_test.rs`, as design
  D7 fixtures item 3 says. (First fix round: the child now echoes its own
  instruction, `{{nodes.echo.output}}`, read from its own `run.yaml`.)
- [x] 3.4 `validate_untrusted_test.rs`: the V340 assertion next to V180.
  Done differently (design D7 fixtures item 4 as amended): V180 can never
  appear next to V340, since the untrusted rules run only on a definition
  with no error, so the test is removed (the V340 table pins the `writes`
  row) together with the dead secret-param entry of `secret_entries`.
- [x] 3.5 Run `cargo test -p apb-engine --test main run_secrets_test`,
  `cargo test -p apb --test main run_secrets_cli_test`, `cargo test -p apb
  --test main mcp_read_only_cli_test`, `cargo test -p apb-server --test
  main run_secrets_api_test` and `cargo test -p apb-core --test main
  validate_untrusted_test`.

## 4. The run's secret set, patches, the gate, resume and capture (design D6, D7, D11)

- [x] 4.1 `crates/apb-engine/src/run_redact.rs`: `secret_names_strict`
  and `secret_render_refusal` exactly as design D11 items 1 and 2 say
  (the declaration check first, then `secret_param_refs` with the run's
  set; an `Err` of the walk is `secret_param_rendered`).
  `secret_names_strict` is strict on both reads: a manifest that exists
  and does not read, and a `playbook.yaml` that is missing, does not
  read or does not parse as its `params` list, each return
  `EngineError::Conflict("secret_set_unreadable: run `<id>`: <file> does
  not read (<cause>); ...")` with the sentence of design D11 item 1;
  never `.ok()`, `unwrap_or_default` or an empty set. The mapping is
  decided, one refusal per caller: the in-run patch journals
  `append_patch_rejected(log, "secret_set_unreadable: <sentence>")`
  (task 4.2); the forward patch answers `refused("secret_set_unreadable",
  <sentence>)` and writes no candidate (task 4.3); both resume entries
  return the `Conflict` before anything is journaled, reserved or locked
  (task 4.5); the consent preflight returns it before any consent step
  (tasks 4.5, 4.8). The gate's candidate branch does not call it (task
  4.4 judges by `current`'s parsed declarations). Run the unit tests of
  1.15.
- [x] 4.2 `crates/apb-engine/src/scheduler/patch.rs` `apply_patch`: the
  check of design D11 item 3 beside the existing refusals, journaled
  through `append_patch_rejected`; split the manifest writes (design D11
  item 3: the running definition's names where `keep_secret_declarations`
  is called today, the patched definition's names right after
  `snapshot_playbook` returned and before `PatchApplied`); rewrite the
  doc comment of `keep_secret_declarations`. No lock is taken. Run 1.11.
- [x] 4.3 `crates/apb-engine/src/forward_patch.rs` `create`: the recheck
  of design D11 item 4.1 under the candidate lock `create` already
  holds, after `check_patch_budget` and immediately before
  `create_forward_patch`, with the source run's set
  (`secret_names_strict`) read there, as `refused(code, sentence)`. Run
  1.12.
- [x] 4.4 `crates/apb-engine/src/gate/mod.rs`: the candidate check of
  design D7 item 4 before `check_run_loaded` in the candidate branch.
  Run 1.13.
- [x] 4.5 `crates/apb-engine/src/scheduler/resume.rs`: the check of
  design D7 item 5 in `resume_detached_inner` right after its parse and
  in `resume_inner_checked` on its parsed `playbook`, the parse moved
  above the legacy shim and the check before the shim's manifest write,
  as `EngineError::Conflict("secret_param_rendered: ...")`, both through
  one inner function over the parsed playbook that the public read-only
  `resume_secret_preflight(root, run_id)` (design D7 item 5.3) also
  calls. The preflight first rejects an unsafe run id with
  `apb_core::registry::is_safe_segment`, as the two resume entries do,
  answering `EngineError::NotFound("run `<id>`")` before it joins the id
  to `.apb/runs`, and answers the same for a missing run directory
  (design D7 item 5.3). `try_journal_resumed` is not touched. Run 1.8,
  1.9, 1.15 and 1.17, then `cargo test -p apb-engine --test main
  journal_compat_test` and `cargo test -p apb-engine --test main
  resume_test`.
- [x] 4.6 `crates/apb-engine/src/scenario/capture.rs`: at both sites, a
  param counts as declared when `run_redact::secret_names(run_dir)` names
  it or the current definition declares it. Run 1.14 and the rest of
  `scenario_capture_test`.
- [x] 4.7 Run 1.7 and 1.10: they must pass without further code (the
  child and the copies of run config need no change by design). A failure
  is a design gap: stop and report it, do not scrub.
- [x] 4.8 The consent surfaces (design D7 item 5.3): call
  `apb_engine::scheduler::resume_secret_preflight(root, run_id)` first,
  before the consent step, in `crates/apb-cli/src/run.rs` (before the
  `resume_consent_need` match; an `Err` goes through `resume_failed`,
  exit 2), `crates/apb-mcp/src/server/run.rs` `run_resume` (before
  `check_resume_consent`; the error through `ToolError::from`) and
  `crates/apb-server/src/routes/runs.rs` (before `check_resume_consent`;
  the error mapped as the route maps the engine resume's, 409 for a
  conflict). The guarantee is the exact one of design D7 item 5.3:
  nothing written when the snapshot the preflight reads refuses; no
  lock is added to make the consent step and the engine's check atomic.
  Run 1.18 and 1.16.

## 5. Docs and records

- [x] 5.1 `docs/HOWTO-authoring.md`, "Secret params": V340 joins the
  table; "What is delivered" says apb never renders a secret param, a
  step reads it from `$APB_RUN_DIR/run.yaml` (scripts, plain cli agents,
  host-mode subagents; not a sandboxed agent; a `success_check` is not
  handed `APB_RUN_DIR`), states the contract of design D5 "The run.yaml
  contract for steps" (path, the top-level `params` mapping, parse it as
  YAML because values are quoted when YAML needs it, mode 0600, when the
  file exists) with a short script example that parses it, says a
  connector that needs a credential uses its account secrets, a
  sub-playbook gets no params, and a patch may not remove `secret: true`
  from a param a run holds secret; `params` holds the run's resolved
  values (given, recalled or defaulted; a param with none is absent); "What is stored" says only `run.yaml`
  (0600) holds it and what a step prints after reading it is its output,
  redacted on the way out as before; the short value warning stays. The
  V180 bullet (line 476) drops "renders a secret param" or says it is now
  V340's. Add the migration sentence of design D7. State the limit
  toward older binaries once, in "Secret params", with the wording of
  design D7 "Older binaries": the rule is enforced by this apb and
  later; an older apb never had it, so an older binary that resumes or
  patches a run can render a secret param exactly as it could before
  this change; mixing binaries on one run is outside the guarantee;
  every apb that drives a run with a secret param should be this
  release or newer.
- [x] 5.2 `CLAUDE.md` and `AGENTS.md`, in the same change: the `params`
  phrase of the `apb-core` validate module ("`params` for the secret
  param rules V99 and V100 and the search param rules V160-V162") names
  V340 and `secret_param_refs`, and the `run_redact.rs` phrase names
  `secret_render_refusal`, the one check a patch, a resume and the gate's
  candidate branch share. The two files stay in sync.
- [x] 5.3 Write `docs/superpowers/specs/<implementation date>-secret-params-in-run-files-design.md`
  (`2026-11-XX` with the real date): the content of `proposal.md` and
  `design.md` as one public record, with no mention of `openspec/` and no
  reference to any outside article, author or project.
- [x] 5.4 Release notes: when the implementing pull request carries a
  version bump, add design "Release notes" to its notes file; otherwise
  put the lines in the report for the next release.

## 6. Style

- [x] 6.1 Over every changed doc: no U+2013 or U+2014 (`rg -nP
  '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation mark in
  prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=` and Rust macros such as
  `writeln!(` do not count), no CJK (`rg -nP
  '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds nothing).

## 7. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 7.1 `cargo fmt --all -- --check`
- [x] 7.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 7.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 7.4 `cargo test --workspace --doc`
- [x] 7.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`: no mirrored type changes, the generated files must be
  current as they are)
- [x] 7.6 `scripts/third-party-notices.sh --check` (no dependency is
  added; it must pass unchanged)
- [x] 7.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 7.8 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 7.9 `openspec validate secret-params-in-run-files --strict`

## 8. Close

- [x] 8.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started.
- [x] 8.2 Final report: the answers to 0.1 to 0.6, each test of section 1
  with its first failure, every fixture changed in 3.1 to 3.4 and how, every place
  where the code differed from `design.md` and what you did (every moved
  line), each gate of section 7 with its numbers, the release notes
  lines, the changed file count, and anything left open.

## File budget

26 changed files, listed one by one in `design.md` "Files": records 5;
core 4 (`validate/params.rs`, `validate/mod.rs`,
`validate_params_test.rs`, `validate_untrusted_test.rs`); engine 6
(`run_redact.rs`, `scheduler/resume.rs`, `scheduler/patch.rs`,
`forward_patch.rs`, `gate/mod.rs`, `scenario/capture.rs`); engine tests
2 (`run_secrets_test.rs`, `scenario_capture_test.rs`); CLI and server
tests 3 (`run_secrets_cli_test.rs`, `secret_runs.rs`,
`run_secrets_api_test.rs`); docs 3 (`docs/HOWTO-authoring.md`,
`CLAUDE.md`, `AGENTS.md`); surfaces 3
(`crates/apb-cli/src/run.rs`, `crates/apb-mcp/src/server/run.rs`,
`crates/apb-server/src/routes/runs.rs`). The release notes file
adds at most 1; name it in the report. A file outside this list is a
stop: report it.

## Wrong implementations (design "Wrong implementations")

1. Any scrub of a rendered text, at any site.
2. Rendering a secret reference as a marker, placeholder, hash or empty
   string at run time instead of refusing the definition.
3. A V340 matcher of its own instead of `apb_core::template::refs` plus
   the first-dot split.
4. V340 over a hand list of fields instead of every string.
5. The whole validator at resume instead of the check of design D11
   item 2 alone.
6. A new `playbook` node param binding, or a parent value copied into a
   child.
7. A new delivery channel (an environment variable, a file) for agents.
8. Touching the value in `run.yaml` or its mode.
9. Turning the short value warning into a refusal.
10. A new event, event field, run guard name or manifest mark, for a run
    with or without a secret param.
11. The capture's declared set from the current definition only, or from
    `RunRedactor::for_run`.
12. A walk that skips on error, or a JSON conversion of the definition.
13. The resume check only in the detached child, under the event lock, or
    on a second read of `playbook.yaml`.
14. A patch, resume or candidate check over the checked definition's own
    declarations alone.
15. Recording a patch's new secret names before its snapshot is written.
16. The forward patch's check outside the candidate lock or on a set read
    before it, or a lock added to `apply_patch`.
17. The legacy ephemeral manifest written before the resume check.
18. The three owned paths skipped inside `secret_param_refs` or
    `secret_render_refusal` instead of only in `check_params`.
19. A consent recorded or cleared, or a legacy manifest written, before
    the secret preflight on a resume surface, or a lock added to make the
    consent step and the engine's check atomic.
20. A lenient read of the run's secret set (an unreadable manifest or
    snapshot taken as an empty set), or `secret_set_unreadable` answered
    with anything but the caller's own refusal.
21. A preflight that joins a run id to `.apb/runs` before the
    safe-segment check.

## Review record

Design review d1 of the first design (1 blocker, 10 major, 10 minor, 5
pre-existing notes): every finding and its disposition is the table in
`design.md` "Review record". In short: B1, M2, M9, M10 and the
pre-existing digest, prompt, argv and session-store notes are fixed by
the principle or by V340 and the resume check; m7 is kept with a reason
(`cache.key` stays in V340); M1, M3 to M8, m1 to m6 and m8 to m10 are
removed with the mechanism; the note that an agent can read `run.yaml`
is kept as the documented delivery.

Third pass, design review d2 (1 blocker, 4 major, 5 minor): every
finding was verified against the code and is recorded in `design.md`
"Review record", "Third pass". In short: B1 fixed by design D11 (no
declassifying patch, the run's full secret set at every check); M1 fixed
by the YAML value walk that fails closed; M2 fixed by the synchronous
resume check in both resume entries; M3 fixed by the candidate skip; M4
accepted, the golden journal test is cut; m1 to m4 fixed; m5 settled by
M2.

Fourth pass, design review d3 (1 blocker, 2 major, 2 minor, 2
pre-existing notes): every finding was verified against the code on
efae5822 and is recorded in `design.md` "Review record", "Fourth pass";
each was accepted as the review proposed. In short: B1 fixed by a run
guard name (cut in the sixth pass, with its tasks); M1 fixed by splitting the manifest writes around the snapshot
(design D11 item 3, tasks 1.11, 4.2); M2 fixed by making the candidate
lock serialize the forward patch's check and the in-run patch's writes
(design D11 item 4, tasks 1.12, 4.2, 4.3); m1 fixed by parsing and
checking before the legacy manifest write (design D7 item 5.2, tasks
1.9, 4.5); m2 fixed by defining `params` as the resolved run values
(design D5, tasks 1.10, 5.1); the two pre-existing notes are kept out of
scope with their reasons.

Fifth pass, design review d4 (1 blocker, 2 major, 3 minor): every
finding was verified against the code on efae5822 and is recorded in
`design.md` "Review record", "Fifth pass", with the removals under "Cut
in the fifth pass". In short: B1 fixed in the run guard (cut in the
sixth pass, with its tasks); M1 fixed by walking the three owned fields in
every check outside validation, the duplicate dropped only by
`check_params` (design D3 item 5, tasks 1.3, 1.17, 2.1, 2.2); M2
settled by cutting the lock change: `apply_patch` takes no lock, the
forward patch rechecks under the lock it already holds, the remaining
interleaving accepted with its reason (design D11 item 4, tasks 1.12,
4.2, 4.3; the lock order item of task 0.3 removed); m1 fixed by the
read-only preflight before every consent surface (design D7 item 5.3,
tasks 0.3 item 13, 1.18, 4.8); m2 settled by M2 (test 12 now tests the
recheck); m3 fixed in the old-binary row (cut in the sixth pass with
the guard).

Sixth pass, design review d5 (1 blocker, 2 major, 1 minor, 1
pre-existing note): every finding was verified against the code on
efae5822 and is recorded in `design.md` "Review record", "Sixth pass",
with the removals under "Cut in the sixth pass". In short: B1 (a crash
between `run.yaml` and the guard append leaves a run an older binary
resumes) settled by cutting the run guard completely and stating the
limit toward older binaries once (design D7 "Older binaries", task
5.1); M1 fixed by reading the run's secret set strictly on both the
manifest and the snapshot, with `secret_set_unreadable` as each
caller's own refusal (design D11 item 1, tasks 0.3 item 14, 1.15, 4.1);
M2 settled by stating the exact no-write guarantee of the preflight and
testing it (design D7 item 5.3, tasks 0.3 item 13, 1.9 (f), 4.8); m1
fixed by the safe-segment check at the preflight's entry (design D7
item 5.3, tasks 1.9 (g), 4.5); the pre-existing note is design D8 item
1.
