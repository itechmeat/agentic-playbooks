# Templates and global playbooks

Design record of the change, from its proposal and design. Started on
main at 1.0.0-beta.7, after "MCP over HTTP and listing surfaces" #295,
and closed on top of "Inbox waits" #297.

---

# Design: templates and global playbooks

Every decision below names its files and functions as they are at the
change's close. A site is named by its function or constant, never by a
line number.

Four decision groups, G0 to G3: the gallery (G3) links an installed
global template to the global playbook page (G2) and prints `apb run
<id> --scope global` (G1) as the next command; G0 keeps a run of a global
definition away from the candidate line of a project playbook of the
same id.

No validator code, no journal event, no journal field, no MCP tool and no
MCP argument is new in this change; the new surfaces are CLI commands and
HTTP routes. `apb-engine` changes for G0 (the one reading of a run's
scope and the guards over it) and, for G1's tests only, a barrier in the
start key compiled with the `testing` feature; the resolved start
functions it already exports are used as they are.

## What this change reuses

| Need | What exists (file, name) |
|---|---|
| A reference to a project or global playbook | `apb_core::scope::{PlaybookRef, Origin, Scope}`, `Scope::parse`, `origin_scope_label` |
| Resolving a reference to a definition and its execution root | `apb_core::store::resolve` (a global playbook executes in the project root it is given), `ResolvedPlaybook` |
| Starting a resolved playbook | `apb_engine::run_resolved`, `run_background_resolved`, `start_detached_resolved` (each ties the expected digest to what the resolver read) |
| The reference MCP uses for a global start | `crates/apb-mcp/src/server/run.rs`, the body of `playbook_run`: the scope match, `keyed_lookup` with the scope label, `PlaybookKey::of`, `tools::budget::preflight(&resolved, ..)`, `start_detached_resolved` |
| The global store | `apb_core::store::global_playbooks_parent()` and `Registry::open_dir(&parent)` |
| A playbook key for the notify check | `apb_core::policy_file::PlaybookKey::of(&PlaybookRef)` |
| Remembered input by reference | `apb_mcp::tools::memory::remember_for_ref` |
| The fit report | `apb_engine::fit::assess(root, &PlaybookRef)`, `apb_engine::fit::problems` |
| Embedding a folder | `rust_embed::Embed` as `BundledAssets` in `crates/apb-core/src/bundled/mod.rs`, with `embedded`, `sub_tree`, and `crate::tree_install::{group_by_folder, FileMap, install_tree}` |
| Writing a draft | the second half of `capture_draft` in `crates/apb-mcp/src/tools/capture.rs`: `apb_core::versioning::create_draft_in_by` with `SaveChannel::Agent` (it sets the draft's id and version itself and refuses an unsafe or reserved id), `apb_core::trust::write_lifecycle`, `provenance.json` |
| A scope argument in the tool layer | `scope_parent` in `crates/apb-mcp/src/tools/mod.rs` |
| A run step caller | `apb_core::step_env::StepCaller::from_env().refusal(what)` |
| The HTTP root of a request | `resolve_root` and `resolve_root_for_scope` in `crates/apb-server/src/state.rs` |
| The fit route's scope parse | `get_fit_handler` in `crates/apb-server/src/routes/fit.rs` |

## G0. A global run never touches a project's candidate line

A candidate (a supervisor's forward patch waiting for its trial), its
trial record, its quality comparison and a run patch version all live in a
project: every writer and reader goes through
`apb_core::candidate::playbook_dir(root, id)` or `Registry::open(root)`,
which are `<root>/.apb/playbooks/<id>`. Those paths find a playbook by id
alone. Before this change a run of a global definition whose id a
project playbook shares reached them (MCP's global start existed on main;
`apb run --scope global` adds a second entry):

1. grading the run re-decided the project candidate
   (`after_stored(root, &grade.playbook)` in
   `crates/apb-mcp/src/tools/grade.rs`, called from the rules and model
   grading, the host grading submit and the backfill);
2. a supervisor patch of the run, in-run or forward, loaded and wrote the
   project playbook (`playbook_patch` in
   `crates/apb-mcp/src/tools/supervisor.rs`, `create` in
   `crates/apb-engine/src/forward_patch.rs`, and the drive's application
   of a queued patch control in `crates/apb-engine/src/scheduler/patch.rs`,
   which loads `Registry::open(root)` by the run's playbook id);
3. the quality comparison and the candidate summary counted the run as a
   run of the project playbook (`crate::run_history::all_runs_of`, called
   only from `candidate/quality.rs` `side` and `candidate/summary.rs`),
   and so did the grade backfill of a project playbook.

Decision: one reading of a run's scope and four guards. Making the
candidate store origin-aware (candidates for global playbooks) is not in
this change.

A run's definition scope has three answers, read in one place
(`crates/apb-engine/src/run_origin.rs`):

```rust
pub enum RunScope { Project, Global, Unreadable }
/// The first `run_provenance` origin label; `Project` without one. The
/// one parse of that label.
pub fn origin_of_events(events: &[Event]) -> RunScope;
/// The first `run_provenance` origin label of the events, if any.
pub fn first_label(events: &[Event]) -> Option<&str>;
/// The scope one label names: `global`, else project.
pub fn scope_of_label(label: &str) -> Scope;
/// `origin_of_events` as a definition scope (`Unreadable` reads as
/// project), for readers that key a store or a slot by scope.
pub fn scope_of_events(events: &[Event]) -> Scope;
/// `origin_of_events` over a journal read whole
/// (`event::read_journal_whole`), `Unreadable` when the run path holds
/// no regular `events.jsonl` (the path itself missing or not a directory
/// included), or a line does not parse (a torn or corrupt line).
pub fn run_scope(run_dir: &Path) -> RunScope;
/// The parent run's origin for a child's `scope: auto` and the gate:
/// `origin_of_events` over the strict reader, `Unreadable` as project.
pub fn parent_run_origin(run_dir: &Path) -> Origin;
/// The refusal of a patch of a run of `scope`: `None` for `Project`.
pub fn patch_refusal(scope: RunScope) -> Option<(&'static str, &'static str)>;
```

`Project` is a run labelled `project` or a legacy journal that reads and
carries no label; `Global` is a run labelled `global`; `Unreadable` is a
run whose scope is unknown, which is evidence for neither side. A run
directory without a journal is `Unreadable`, not an empty project run.
A global run that fails in preparation, before its full `run_provenance`
is known, journals a `run_provenance` that carries only its origin before
its failure, so it never reads as a project run of the same id; a
project run's failure journal stays as it was.
No other code parses the label: `parent_run_origin` (a child's `scope:
auto`, the gate's tree walk, which keeps reading `Unreadable` as
project), the rebind's origin, the drive's patch guard, the known
problems store, the concurrency slot and its run check, the start key's
run check, the run listing's scope, the run views' concurrency block,
routine health, `previous_success` and the scenario capture go through
`origin_of_events`, `scope_of_events` or `first_label` with
`scope_of_label` (the run history head keeps the first label for
`previous_success` and health). A raw label is read only where the text
itself is stored: the skip digest of a list's `skip`.

1. Grading: `after_stored(root, run_dir, id)` in
   `crates/apb-mcp/src/tools/grade.rs` re-decides nothing for a run whose
   `run_scope` is not `Project` (`Global` or `Unreadable`) and returns the
   empty answer. Its per-run callers pass the run's directory. The
   backfill re-decides once for the project playbook through its own
   entry, `after_backfill(root, id)`, so the per-run function always has
   a run. Grading itself is allowed and stored; only the candidate
   re-decision is skipped.
2. The runs a project candidate is judged from: one function,
   `candidate::runs::project_runs_of(root, id, version)` in
   `crates/apb-engine/src/candidate/runs.rs`, the production runs of the
   project playbook created on this machine, newest first:
   `run_history::all_runs_of` without every run that is not `Project`
   (`candidate::runs::is_project_run`). The quality comparison
   (`candidate/quality.rs`) and the candidate summary
   (`candidate/summary.rs`) use it. The grade backfill (`backfill` in
   `crates/apb-engine/src/grade/mod.rs`) keeps its own bounded
   enumeration, `runs_of(.., BACKFILL_SCAN)`, and leaves out the same
   runs before it grades.
3. Patches: a patch of a run that is not `Project` is refused before any
   project definition is looked up or written, by the one refusal the
   three guards share, `run_origin::patch_refusal`:
   `patch_global_unsupported: a run of a global playbook cannot be
   patched` for `Global`, and `patch_origin_unreadable: the run's journal
   does not read, so its definition scope is unknown` for `Unreadable`.
   1. `playbook_patch` (in-run, `crates/apb-mcp/src/tools/supervisor.rs`)
      checks the run id first through `scheduler::run_dir_of` (one safe
      path segment naming an existing run, before any journal is read),
      then `patch_refusal(run_scope(&run_dir))`, then reads the run's
      playbook reference;
   2. `forward_patch::create` right after the same run id check, before
      the external data check and the journal read;
   3. the drive (`scheduler/patch.rs`) before it loads the project
      definition for a queued patch, over the events it already holds
      (`origin_of_events`), which it read whole, so its answer is never
      `Unreadable`; the refusal is journaled as the patch rejection.
4. The trial: the `manifest.candidate_trial` expression of
   `scheduler/prepare.rs` requires `t.origin_label == "project"` (a
   candidate lives in a project, `apb_core::candidate::playbook_dir`: a
   global definition has none), so a stray or hand-made candidate pointer
   inside the global store marks no trial. The bit of a manifest
   authorizes nothing by itself: the settlement at a run's end
   (`candidate::settle`) and the trial view (`candidate::trial_of`) check
   `is_project_run` first and, for a run that is not `Project`, return
   without opening the project's candidate store. A manifest written
   before the start checked the scope can carry the bit on a global run;
   it settles nothing.

Tests (every test with `dup` in both stores at the same version unless it
says otherwise; the project `dup` has `current` 1.0.0 and a candidate
1.1.0, seeded as the candidate tests of the engine suite seed one; a
"torn" journal ends in a half-written line after a complete
provenance):

1. `crates/apb-engine/tests/suite/global_scope_run_test.rs`
   1. `a_global_run_leaves_a_project_candidate_alone`: a global `dup`
      1.1.0 runs to its end through `run_resolved`, once succeeding and
      once failing; the project's candidate pointer, trial record and
      `current` are unchanged and the journal has no candidate event.
      This one passes before the change too (settlement needs the trial
      mark): it is the isolation regression.
   2. `a_stray_global_candidate_pointer_marks_no_trial`: the global store
      itself has `current` 1.0.0 and a candidate pointer 1.1.0 written by
      hand; a global run of 1.1.0 has `candidate_trial: false` in its
      manifest.
   3. `a_global_run_with_the_trial_bit_settles_no_project_candidate`: a
      global run directory whose manifest carries the bit, with a project
      candidate of the same id and version, finishes and leaves the
      project's candidate pointer, trial record and `current` byte for
      byte; `trial_of` answers no trial for it.
   4. `a_forward_patch_of_a_global_run_is_refused`: `forward_patch::create`
      on a finished global run answers `patch_global_unsupported`; the
      project playbook directory is byte for byte unchanged.
   5. `a_forward_patch_of_an_unreadable_run_is_refused` (a torn journal)
      and `a_forward_patch_of_a_run_without_a_journal_is_refused`: each
      answers `patch_origin_unreadable` and the project playbook is
      unchanged.
   6. `a_queued_patch_of_a_global_run_is_rejected`: the patch control is
      posted as the patch tests of the suite post it, for a version the
      project holds, on a global run held at a wait or review; the run
      journals the patch rejection with the code and the project playbook
      is unchanged.
2. Unit tests of `run_origin.rs`:
   `a_run_directory_without_a_journal_is_unreadable` (no `events.jsonl`,
   or one that is a directory) and `a_legacy_journal_without_a_label_is_project`.
3. Unit tests in `candidate/runs.rs`, beside `project_runs_of` (one shared
   `seeded_run` helper under `#[cfg(test)]`):
   `a_global_run_is_not_a_project_run_for_the_comparison` (one project
   run and one global run of `dup` 1.0.0; the function returns only the
   project run, `all_runs_of` returns both),
   `a_global_run_with_a_torn_journal_is_no_project_run` and
   `a_run_without_a_journal_is_no_project_run`.
4. The two consumers, each failing while its consumer still calls
   `all_runs_of`:
   1. `crates/apb-engine/tests/suite/candidate_quality_test.rs`:
      `a_global_grade_does_not_move_the_project_comparison`: a graded
      global run of the project's `current` id and version whose grade
      would change the verdict; the project verdict is the one without
      it;
   2. unit tests in `candidate/summary.rs`:
      `a_global_run_does_not_mark_the_candidate_edited` (a global run of
      the candidate's id and version with another definition digest) and
      `a_global_run_with_a_torn_journal_does_not_mark_the_candidate_edited`;
      the summary does not report the project candidate as edited.
5. `crates/apb-mcp/tests/suite/grade_tool_test.rs`
   1. `grading_a_global_run_leaves_the_project_candidate`: grade the
      global run with the rules grader under the project policy
      `promote_supervisor_patches: { when: quality }`; a grade is stored,
      the answer's candidate block is empty and the project candidate
      pointer and trial record are unchanged.
   2. `a_backfill_of_a_project_playbook_skips_global_runs`: the backfill
      of project `dup` grades the project run and not the global one.
6. `crates/apb-mcp/tests/suite/patch_tool_test.rs`:
   `an_in_run_patch_of_a_global_run_is_refused` (`playbook_patch` answers
   the code; no patch version appears under the project playbook),
   `an_in_run_patch_of_an_unreadable_run_is_refused`,
   `an_in_run_patch_of_a_run_without_a_journal_is_refused` (both
   `patch_origin_unreadable`), and
   `an_in_run_patch_checks_the_run_id_before_reading_a_journal`.
7. `every_scope_reader_answers_the_one_parser`, a unit test in
   `scheduler/listing.rs`: journals whose first label is unknown (or
   absent) and whose later label is `global`; the known problems store,
   the concurrency slot and its run check, the run listing, the start
   key's run check and the scenario capture all answer what
   `origin_of_events` answers.

No test waits with a fixed sleep.

Stated limit, not changed here: the run estimate
(`run_estimate::history`) and the run statistics (`run_stats`, the
dashboard's per-playbook history) match runs by playbook id and version
without the definition scope, so a project playbook and a global one of
the same id and version share estimate history and statistics. That is
display and prediction only (a budget is a predictive stop, never a
ceiling); no candidate, trust or launch decision reads it. Separating
them is a change after 1.0.

## G1. apb run --scope global

### Before this change

`run_cmd` in `crates/apb-cli/src/run.rs` worked on a bare `name`. Each of
these built its own project reference or project key from it:

1. `gate_run` (the `wref` at its top, read by `check_run`, the start
   actions of a refusal and `check_params_with`);
2. `remember_input`;
3. `key_lookup` (`apb_engine::start_key::lookup(root, "project", name,
   req)`);
4. the refusal of `check_unattended` in `run_cmd` (the `wref` its start
   actions are built from);
5. the readiness warning of an unattended start in `run_cmd`
   (`apb_engine::fit::assess`);
6. `resolve` in `crates/apb-cli/src/budget.rs`, behind `prestart` and
   `estimate_cmd`;
7. `start_check` in `crates/apb-cli/src/notify.rs` (`PlaybookKey { id,
   global: false }`).

The three starts were project-only: `run(root, name, version, opts)`,
`apb_engine::start_detached(root, name, version, opts)` for `--detach` and
`apb_engine::run_background(root, name, version, opts)` in
`run_host_foreground`.

### The flag and the one reference

1. `RunArgs` gets

```rust
/// Definition scope: project (default) or global; a global playbook
/// runs in this project
#[arg(long, value_name = "SCOPE", default_value = "project")]
pub scope: String,
```

2. One helper in `run.rs`:

```rust
/// The playbook `apb run` was asked for: `name` in `scope`.
fn playbook_ref(name: &str, version: Option<&str>, scope: &str) -> Result<PlaybookRef, String> {
    let origin = match apb_core::scope::Scope::parse(scope) {
        Some(Scope::Project) => Origin::Project { workspace_id: None },
        Some(Scope::Global) => Origin::Global,
        None => return Err("--scope must be project or global".into()),
    };
    Ok(PlaybookRef { origin, id: name.to_string(), version: version.map(str::to_string) })
}
```

   `run_cmd` calls it once, right after the `--require-ready` and
   `--waive-probation` flag checks and before the deadline parse; an `Err`
   is printed and the command exits 2. The result is `wref`, the reference
   with the version as the person asked for it.
3. Two refusals follow at once, before any definition is read and before
   any run or remembered input is written (the CLI's own start-up, which
   registers the workspace and ensures the installation key, has already
   happened):
   1. `--supervise` with a global `wref`: print `run refused
      (supervise_global_unsupported): a supervised run of a global
      playbook is not supported; start it without --supervise`, exit 2.
      The constant `const SUPERVISE_GLOBAL: &str =
      "supervise_global_unsupported";` lives in `run.rs`.
   2. `--eval-settings` with a global `wref`: print `--eval-settings is
      only for a project playbook`, exit 2 (an eval case's definition
      under test is always a project playbook of its scratch project).
4. Every site of "Before this change" items 1 to 7 takes the reference instead of the
   name. No function of `run.rs`, `budget.rs` or `notify.rs` that serves
   `apb run` builds a `PlaybookRef` or a `PlaybookKey` from a bare name
   afterwards:
   1. `gate_run(root, wref: &PlaybookRef, supervised, opts, how, launch)`:
      the `name` and `version` parameters go; the function uses `wref` as
      its own reference (the `run_ref` it derives for the param check
      keeps the origin).
   2. `remember_input(root, wref: &PlaybookRef, params, instruction)`: it
      passes `PlaybookRef { version: None, ..wref.clone() }`.
   3. `key_lookup(root, wref, req, detached)`: the scope label is
      `apb_core::scope::origin_scope_label(&wref.origin)`.
   4. The unattended refusal and the readiness warning use `wref` (the
      warning at the version the gate selected, as before).
   5. `budget.rs`: `resolve(root, wref: &PlaybookRef)`, and `prestart` and
      `estimate_cmd` take `wref: &PlaybookRef` in place of `name` and
      `version`. `run_cmd` hands them `wref` with the version the gate
      selected where it did so before (the non-supervised `prestart`), and
      the requested one for `--estimate`.
   6. `notify.rs` `start_check(root, wref: &PlaybookRef, req)`: the key is
      `PlaybookKey::of(wref)`. Every other caller of `start_check` in the
      CLI builds a project reference with a small local `PlaybookRef` literal
      and behaves as before.
   The supervised branch of `run_cmd` and `drive_supervised_child` keep
   starting a project playbook: they build their reference with
   `playbook_ref(name, version, "project")` (it cannot fail) and are
   otherwise unchanged. `spawn_detached_supervised`, the `DriveSupervised`
   command and the launch handoff are not touched.

### The three starts

One private type in `run.rs`:

```rust
/// What a start of `apb run` runs: a project playbook by its id, or a
/// global one resolved once at the version the gate selected.
enum StartTarget {
    Project,
    Global(Box<apb_core::store::ResolvedPlaybook>),
}

/// `wref` at `version`, the version the run gate selected: the one
/// reference every check after the gate reads.
fn at_version(wref: &PlaybookRef, version: Option<&str>) -> PlaybookRef;

/// The target of `selected` (a reference at the version the run gate
/// selected). A global playbook is resolved here, once; a resolver error
/// is the start's error.
fn start_target(root: &Path, selected: &PlaybookRef) -> Result<StartTarget, String>;
```

`run_cmd` builds `selected = at_version(&wref, version)` once after the
gate and hands it to every later check, `remember_input` and
`start_target`. `start_target` returns `StartTarget::Project` for a
project origin without reading anything, and for `Origin::Global` calls
`apb_core::store::resolve(root, selected)`, mapping an error to its
`to_string()`. `run_cmd` calls it once, after `remember_input` and
before the `detach` branch; an `Err(e)` prints `run failed: {e}` and
exits 2.

The three call sites match on it:

| Start | Project (as before) | Global |
|---|---|---|
| synchronous | `run(root, name, version, opts)` | `apb_engine::run_resolved(&resolved, opts)` |
| `--detach` | `apb_engine::start_detached(root, name, version, opts)` | `apb_engine::start_detached_resolved(&resolved, opts)` |
| host foreground | `apb_engine::run_background(root, name, version, opts)` | `apb_engine::run_background_resolved(&resolved, opts)` |

`run_host_foreground` takes `target: &StartTarget` (with `name` and
`version` for the project arm). One tail after a start: each start form
matches on `StartTarget` only to get the engine's result, and what
follows (the started or finished line, `already_started`,
`print_recalled`, the host hint, `released_exit`, the exit code) exists
once per form and is the same for both scopes: `detached_started` for
`--detach`, `foreground_finished` for the synchronous start, and the
second half of `run_host_foreground`.

`Registry::open(root)` still must succeed ("no project here"): a global
playbook runs in a project. `apb run --scope global` inside a run step is
a normal start; the gate decides as for a project playbook.

Docs: `docs/HOWTO-authoring.md` "## Making a playbook global" gets one
paragraph after its command block: a global playbook is started in any
project with `apb run <id> --scope global` (also `--detach`), `--supervise`
is refused for it, and `apb memory show <id> --scope global` shows its
remembered input. `llms.txt` and `README.md`: the `apb run` line of each
command list names `--scope project|global` if the line lists flags; else
nothing.

### Tests

New `crates/apb-cli/tests/suite/global_run_cli_test.rs`, declared in
`crates/apb-cli/tests/main.rs`. Each test has a project (`apb init`
through the suite's helpers in `suite/common.rs`) and its own config dir
passed as `APB_CONFIG_DIR` to every `apb` command of the test, holding a
global playbook written the way `seed_global` of
`crates/apb-engine/tests/suite/global_scope_run_test.rs` writes one (a
schema 2 definition with a `start`, one `prompt` node and a `finish`;
`current`), as a schema 2 definition approved for the test's config
dir with `apb trust approve` (the approval names the stored trust
digest). The playbook has no agent node, so no agent CLI runs. No test
waits with a fixed sleep: completion is read with `apb wait`.

1. `apb_run_scope_global_runs_the_global_playbook_here`: exit 0; the
   run directory is under the project's `.apb/runs`; the run's
   `run_provenance` event carries origin `global` (read `events.jsonl`).
2. `apb_run_scope_global_detach_starts_and_finishes`: `--scope global
   --detach` prints `run started: <id>`; `apb wait <id>` exits 0.
3. `a_project_playbook_of_the_same_id_is_not_taken`: both scopes hold
   `dup` whose prompt texts differ; `--scope global` runs the global one
   (the journaled definition digest, `run_started.definition`, equals the
   global file's) and plain `apb run dup` runs the project one.
4. `an_unknown_scope_exits_2`: stderr holds `--scope must be project or
   global`; `.apb/runs` holds no run.
5. `supervise_with_scope_global_is_refused`: exit 2, stderr holds
   `supervise_global_unsupported`, no run directory.
6. `remember_with_scope_global_keys_the_global_playbook`: the global
   playbook declares one text param; `apb run <id> --scope global
   --remember --param k=v`, then `apb memory show <id> --scope global`
   lists `k` and `apb memory show <id>` (project) does not; `dup` is
   seeded in the project too, and values are matched as whole cells.
7. `a_start_key_with_scope_global_answers_the_same_run`: two identical
   `apb run <id> --scope global --detach --start-key e1` calls; the second
   prints the first run's id (`already_started`); one run directory.
   `a_start_key_is_held_per_scope`: with `dup` in both scopes, a project
   start with key `e1` leaves a global start with `e1` free to start its
   own run, and the reverse.
   `an_already_started_answer_is_the_same_for_both_scopes`: a keyed start
   and its repeat print the same lines and exit with the same codes for
   both scopes, on the detached and the synchronous form. The repeat
   reaches the engine's `AlreadyStarted` arm of the shared tail, not only
   the CLI's own lookup: its lookup answers the key free and stops at a
   barrier in `apb_engine::start_key` (compiled only with the `testing`
   feature, so never in a release build), another start claims the key,
   and the repeat then goes on to its own start.
8. `estimate_with_scope_global_reads_the_global_playbook`: only the
   global store holds the id; `apb run <id> --scope global --estimate`
   exits 0 and starts nothing.
9. `eval_settings_with_scope_global_is_refused`: exit 2, the sentence of
   "The flag and the one reference" item 3.2, no run directory.
10. `a_global_host_start_runs_in_the_foreground`: `--scope global
    --execution host` on the agent-free playbook exits 0 and the run's
    provenance origin is `global`.
11. `recall_with_scope_global_reads_the_global_memory_only`: `dup` in
    both scopes with one required text param; a value remembered for the
    project playbook is not recalled by a `--scope global` start (it is
    refused `param_needs_value`); a value remembered with `--scope global
    --remember` is recalled by the next `--scope global` start and not by
    a project start.
12. `a_budget_check_reads_the_global_playbook`: only the global store
    holds the id; `apb run <id> --scope global --budget-tokens 100000`
    starts the run, and its output holds no "budget check skipped"
    warning (the check resolved the global definition).
13. `the_notify_policy_of_a_global_start_is_the_global_key`: the machine
    `policy.yaml` of the test's config dir subscribes only the key
    `global:dup` (the notify section's form of
    `crates/apb-core/src/policy_file.rs`) with a target
    that cannot be used; `apb run dup --scope global` prints the notify
    warning of that default, and `apb run dup` (project) prints none.
14. `an_unattended_global_start_reads_the_global_readiness`: the global
    `dup` is not ready to run alone (no budget) and the project `dup` is
    really ready (what `fit::readiness::assess` requires); `apb run dup
    --scope global --unattended --require-ready` is refused with the
    readiness refusal, `apb run dup --scope global --unattended` prints
    "warning: not ready to run alone", and a plain project start prints
    no such warning.

## G2. Global playbooks in the dashboard

Scope of this decision: the list shows global playbooks, and a read-only
page shows one with its fit for a chosen project and the commands that
run it. Running, editing, versions, frozen, delete, layout, memory,
scenarios, problems, corrections, statistics and health of a global
playbook are not in the dashboard in this change: their routes and
components read the project playbook of that id, so none of them is
reachable from the global page. `run_playbook_handler`, `after_start`,
`routes/estimate.rs`, `routes/notify.rs` and `RunStartDialog.svelte` are
not changed.

### Server

In `crates/apb-server/src/routes/playbooks.rs` unless named.

1. A shared helper:

```rust
/// The definition origin a `scope` query value names: `project` when
/// absent. Any other value is 400 "scope must be project or global".
#[allow(clippy::result_large_err)]
pub(crate) fn origin_of(scope: Option<&str>) -> Result<apb_core::scope::Origin, Response>;
```

   `get_fit_handler` in `routes/fit.rs` uses it instead of its own match
   (same answers, one place).
2. `list_playbooks` aggregates both scopes and takes no scope selector:
   every project entry gets `"scope": "project"` (`tag_summary` gains a
   `scope: &str` parameter and inserts it; `list_playbooks` is its only
   caller). After the workspace loop the global store is listed once:
   `apb_core::store::global_playbooks_parent()`, `Registry::open_dir`,
   `list()`; each summary is tagged `tag_summary(summary, "", "",
   "global")`. A global store that is missing, fails to open or fails to
   list adds nothing. A global entry carries no `health`.
3. `get_playbook`: `DetailQuery` gets `#[serde(default)] scope:
   Option<String>`. For a project origin nothing changes. For a global
   origin the root is `resolve_root_for_scope(&state, q.workspace.as_deref(),
   "global")` (a global detail needs no workspace), the registry is
   `Registry::open_dir(&parent)` of the global store (a missing store is
   404 "not found") instead of `Registry::open(&root)`. `validation_issues`
   gets a parameter `origin: apb_core::profile_store::PlaybookOrigin` used
   in both places that said `PlaybookOrigin::Project`; its other
   caller, `crates/apb-server/src/routes/graph.rs`, passes `Project`. The
   answer has the same shape plus `"scope": "project" | "global"`.

### Web

Plain CSS over the existing CSS variables in every new or changed
component, in a `<style>` block like the neighbouring pages; no utility
class strings (a class a changed line needs is a class of the
component's own style block). Text uses the foreground variables of
`web/src/app.css` (`--muted-foreground` for muted text), one variable per
role: no `var(--a, var(--b))` fallback chains.

1. `web/src/lib/types.ts`: `PlaybookSummary` gains `scope?: 'project' |
   'global'`.
2. `web/src/lib/api/core.ts`: `fetchPlaybook(id, workspace, version,
   scope?)`; `scope` is sent as a query value only when it is `global`.
   `fetchFit` already takes a scope.
3. `web/src/lib/playbooklist.ts`, three pure functions:
   `groupKey(entry: Pick<PlaybookSummary, 'scope' | 'workspace_id'>):
   string` returns `'global'` for a global entry, else the workspace id
   (or `'_'` when it is empty, as the page did before);
   `groupLabel(key: string, project: string): string` returns `'Global
   playbooks'` for `'global'`, else `project`; `sortGroups(groups)` puts
   the global group first and the rest by project name as before.
4. `web/src/pages/PlaybookList.svelte`:
   1. the list filter keeps global entries when a project is selected:
      use the scope-aware filter that already exists for profiles,
      `filterProfilesByProject` in `web/src/lib/projectfilter.ts`, renamed
      to `filterScopedByProject` (the profile list keeps calling it under
      the new name), with entries whose `scope` is absent counted as
      project entries; the project picker's options are built from
      `projectScopeItems(items)` as `ProfileList.svelte` does (the
      helper's type takes an absent `scope` as project), so a global row
      adds no project option, and the "N projects" count and the
      project label of the heading and the top bar are derived from the
      groups without the global group;
   2. the `groups` derivation uses `groupKey`, `groupLabel`, `sortGroups`;
   3. a global row links to `#/global-playbook/<id>` and renders no
      Duplicate, no Delete and no run statistics (those read a project).
5. `web/src/App.svelte`: the route `#/global-playbook/<id>` (page
   `global-playbook`) next to `#/playbook/`.
6. New page `web/src/pages/GlobalPlaybookView.svelte`, props `{ id }`:
   1. starts each id from a clean state (no earlier detail or failure
      shown when the route names another global playbook), and loads
      `fetchProjects()` and `fetchPlaybook(id, '', undefined, 'global')`
      (a global detail needs no workspace; the first reachable project is
      the Project select's start);
   2. shows the name, the id, the version, the description, the effective
      effects, the validation issues through `IssuesPanel` with
      `fix={null}`, and the YAML in a read-only `<pre>`;
   3. a `<select>` labelled "Project" over the reachable projects; for
      the chosen one, the fit report through `FitCard` with
      `fetchFit(id, workspace, 'global')`. With no reachable project the
      select and the fit are replaced by the line "Open a project to see
      whether this playbook fits it.";
   4. a block "Run it" with the two commands as text, each on its own
      line in a `<code>` element: `apb run <id> --scope global` and `apb
      adopt <id> --scope global`, and the line "Start and edit a global
      playbook with the CLI; the dashboard shows it.";
   5. nothing else: no run button, no editor link, no versions, no
      memory, no scenarios, no problems, no statistics.

### Tests

1. `crates/apb-server/tests/suite/api_test.rs` (the private config dir
   helper of `suite/common.rs`; the global playbook written as G1's tests
   write it):
   1. `the_playbook_list_carries_global_playbooks`: two workspaces and
      one global playbook; the list holds it exactly once, with `scope:
      global` and empty `workspace_id`; every project entry has `scope:
      project`.
   2. `a_global_playbook_detail_validates_as_global`: a global playbook
      whose agent node binds `profile: { name: x, scope: project }`
      answers V14 in `validation` with `scope=global`.
   3. `a_global_detail_needs_no_workspace`: on a server without a pinned
      root, `GET /api/playbooks/<id>?scope=global` answers 200.
   4. `a_global_detail_is_not_the_project_playbook`: both scopes hold
      `dup` with different names; `scope=global` answers the global name
      and no `scope` the project one.
   5. `an_unknown_scope_is_400`: the detail and the fit each answer 400
      to `scope=team`.
   6. `a_missing_global_playbook_is_404`: the detail with `scope=global`
      for an id only the project holds.
2. `web/src/lib/playbooklist.test.ts`: `groupKey puts global entries in
   one group`, `groupLabel names the global group`, `sortGroups puts the
   global group first`.
3. `web/src/lib/projectfilter.test.ts`, one `describe` block for
   `filterScopedByProject`: `a selected project keeps global playbooks`,
   `an entry without a scope counts as a project entry`, `a list of
   global playbooks only offers no project option`.
4. `web/src/pages/GlobalPlaybookView.test.ts`, mounted like
   `web/src/pages/RunList.svelte.test.ts` with the fetch functions
   mocked: `it shows the commands and no run or edit control` (the text
   `apb run <id> --scope global` is there; no link whose href starts with
   `#/edit/` and no button named Run), `it asks the detail with the
   global scope`, `it asks the fit of the global playbook for the picked
   project` (`fetchFit` called with `(id, workspace, 'global')`),
   `without a project it shows the note and no fit`.
5. `web/src/pages/PlaybookList.test.ts` (mounted the same way, the
   `fetchRuns` mock answering an array): `a global row has no Duplicate
   and no Delete and links to the global page`, `the picker offers the
   projects and no entry for the global group`, `counts projects without
   the global group and keeps the project label` and `keeps the project
   label with a project selected`.
6. Acceptance: the list, the global page and the Templates page (G3) are
   opened in a browser, in the light and the dark theme, with one
   project and one global playbook, and checked from screenshots.

## G3. Template gallery

### The source

The templates are the top-level files of `examples/playbooks/`: every
`<stem>.yaml` whose `id` equals `<stem>`, with the files under
`<stem>/evals/` when that folder exists. At the change's close that is 15 playbooks
(code: `ci-failure-triage`, `dependency-security-fix`, `design-doc-check`,
`review-panel`, `review-panel-list`, `review-triage`, `sentry-triage`,
`task-check-first`, `release-announce`, `release-heartbeat`; non-code:
`bulk-reminders`, `crm-cleanup`, `document-checklist`, `inbox-triage`,
`whatsapp-inbox`). Three carry eval scenarios: `design-doc-check` 4,
`document-checklist` 4, `task-check-first` 3 (files under
`evals/scenarios/`). None has a script node. A nested folder without a
top-level file of its name (`quality-process/`, untracked on main) is
never listed or installed as a template.

Why the examples stay where they are: many tests and docs name
`examples/playbooks/<file>`, and the
examples are the gallery: one place to keep them.

The embed takes the folder as it is (no include filter: rust-embed's
filter feature is not enabled in this workspace and this change enables
no feature of it), so the selection happens at read time: the
unfiltered embed may carry the bytes of a nested folder in a release
binary built locally with that folder present, and enumeration may read
them before they are filtered out; a CI build never has the folder. As
with the bundled content, a build needs the repository layout (the folder
is outside the crate's own directory), a debug binary reads the folder at
run time and a release binary embeds it.

### Core

A new module `crates/apb-core/src/templates.rs` (`pub mod templates;` in
`crates/apb-core/src/lib.rs`, in alphabetical place):

```rust
//! The template gallery: the example playbooks apb ships, embedded from
//! `examples/playbooks/` the way the bundled content is embedded
//! (`crate::bundled`). A template is a starting point a person installs
//! as a draft and adapts; it is never approved, never upgraded and never
//! managed by apb. Only the top-level `<id>.yaml` files and their `<id>/evals/`
//! are read.

#[derive(rust_embed::Embed)]
#[folder = "../../examples/playbooks"]
struct TemplateAssets;

/// The `created_by` of a draft installed from a template.
pub const CREATED_BY: &str = "template";

/// One template, as every surface lists it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TemplateSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// The effective effects (`crate::effects::effective`), as that
    /// function's values serialize.
    pub effects: Vec<String>,
    /// How many eval scenario files ship with it (the files under
    /// `evals/scenarios/`).
    pub scenarios: usize,
    /// `sha256:<hex>` of its yaml bytes.
    pub digest: String,
}

/// One template with its content.
#[derive(Debug, Clone)]
pub struct Template {
    pub summary: TemplateSummary,
    pub yaml: String,
    /// Its `evals/`, paths relative to that folder.
    pub evals: crate::tree_install::FileMap,
}

/// Every template, sorted by id. A file that does not parse, or whose id
/// is not its file name, is skipped (the test `every_example_is_a_template`
/// keeps that from happening in a release).
pub fn all() -> Vec<Template>;

/// The templates of an inventory of `(path, bytes)` pairs: `all` is this
/// over the embedded folder. Public for the selection test only in the
/// sense that the test reaches it through `pub(crate)` from a unit test
/// module of this file.
pub(crate) fn select(files: Vec<(String, Vec<u8>)>) -> Vec<Template>;

/// The template `id`.
pub fn get(id: &str) -> Option<Template>;

/// Writes the template's `evals/` into the new draft's `playbook_dir`
/// (`<playbook_dir>/evals/`). Nothing to write, or an identical tree
/// already there, is `Ok(0)`; the count of files written otherwise. An
/// existing different `evals/` is never replaced: that is an `Err`.
pub fn install_evals(playbook_dir: &Path, template: &Template) -> Result<usize, String>;
```

How:

1. `TemplateSummary` derives its TypeScript type exactly as the exported
   types of `crates/apb-core/src/corrections/mod.rs` do
   (`#[cfg_attr(feature = "ts", derive(ts_rs::TS))]`, the crate's `ts`
   feature).
2. `all` reads `TemplateAssets::iter()` once into `(path, bytes)` pairs
   and returns `select` of them; the rest of this item is `select`.
   A path with no `/` that ends in `.yaml` is a candidate: its text must
   be UTF-8, parse with `Playbook::from_yaml`, and name the id of its
   stem. The eval files of a candidate are the embedded paths under
   `<stem>/evals/`, taken through the helpers `bundled/mod.rs` already
   has: make `sub_tree` `pub(crate)` and call
   `crate::tree_install::group_by_folder` on the pairs, then
   `crate::bundled::sub_tree(files_of_stem, "evals")`; neither helper is
   copied.
3. `effects`: `crate::effects::effective(&playbook)`, each value
   serialized with `serde_json::to_value` and read as a string, in that
   function's order.
4. `scenarios`: the number of keys of `evals` that start with
   `scenarios/`.
5. `digest`: `crate::content::sha256_hex(yaml.as_bytes())` (it returns
   the `sha256:<hex>` form).
6. `install_evals`: `Ok(0)` for an empty map; else
   `crate::tree_install::install_tree(playbook_dir, crate::eval::EVALS_DIR,
   &template.evals, |_| false)`: `Ok(0)` when the result's `no_op` is
   true, the map's length otherwise, and an error rendered with
   `format!("{e:?}")` (`TreeInstallError` derives `Debug` only).

Tests, new `crates/apb-core/tests/suite/templates_test.rs` (declared in
`crates/apb-core/tests/main.rs`):

1. `every_example_is_a_template`: the top-level `*.yaml` files of
   `examples/playbooks` read from disk
   (`Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/playbooks")`)
   are exactly the ids of `all()`, and each summary id equals its stem.
2. `no_template_carries_a_script_node`: no node of any template has the
   type `script`.
3. `templates_with_scenarios_carry_them`: `task-check-first` has
   `scenarios == 3`, `design-doc-check` 4, `document-checklist` 4, and
   `inbox-triage` 0 with an empty `evals`.
4. `a_nested_folder_is_not_a_template`, a unit test in `templates.rs`
   over `select` with an in-memory inventory: a valid top-level
   `a.yaml` (id `a`) with `a/evals/scenarios/s.yaml`, a valid playbook
   at `nested/b.yaml` with no top-level file of that name, and a
   top-level `c.yaml` whose id is `other`. Only `a` is selected, with its
   one eval file.
5. `every_template_validates_without_errors`: each parses and
   `apb_core::validate::validate(&pb, &ValidationContext::default())` has
   no issue of severity error.
6. `the_digest_is_of_the_yaml_bytes`: for one template, `digest ==
   sha256_hex(yaml.as_bytes())` and it starts with `sha256:`.
7. `install_evals_writes_the_scenarios_once`: into a temp dir,
   `task-check-first` writes 3 files under `evals/scenarios/` and answers
   3, each file equal (`assert_eq!`) to the bytes of the template's map;
   an identical second call answers 0; with one file's content changed
   on disk a third call is an `Err` and that file keeps the changed
   bytes.

### Install

The one install entry lives in the tool layer, shared by the CLI and the
HTTP API: a new `crates/apb-mcp/src/tools/templates.rs` (`pub mod
templates;` in `tools/mod.rs`, in alphabetical place).

```rust
/// The refusal code of a template install inside a run step.
pub const IN_RUN_STEP: &str = "template_in_run_step";

/// One template install.
pub struct InstallRequest<'a> {
    pub template: &'a str,
    /// `project` or `global` (`super::scope_parent`).
    pub scope: &'a str,
    /// The playbook id to install as; the template's own id when `None`.
    pub as_id: Option<&'a str>,
    /// Whether `root` is a project the person named: the fit report is
    /// made only then.
    pub with_fit: bool,
}

/// Installs a template as a draft: the playbook (version 1.0.0, lifecycle
/// `draft`, provenance `created_by: template`), its eval files, and the
/// answer with the fit report of the new draft. It never creates, revokes
/// or changes an approval.
pub fn install(root: &Path, req: InstallRequest<'_>) -> Result<Value, ToolError>;

/// Every template's summary: `{"templates": [TemplateSummary, ..]}`.
pub fn list() -> Value;

/// One template: `{"template": TemplateSummary, "yaml": "<text>"}`.
pub fn show(id: &str) -> Result<Value, ToolError>;
```

`install` does, in order:

1. Refuse from a run step through the tool layer's one guard,
   `super::outside_run(&apb_core::step_env::StepCaller::from_env(),
   "installing a template")` (`tools/mod.rs`, the guard discovery uses),
   with the code put in front of its sentence: a
   `ToolError::Conflict(why)` becomes
   `ToolError::Conflict(format!("{IN_RUN_STEP}: {why}"))`. HTTP answers a
   `Conflict` with 409. `list` and `show` are allowed anywhere. What this
   guard is: the refusal applies when the installing process carries
   `APB_RUN_ID` or `APB_HOST_TASK`, so a step of a run does not leave
   definitions behind. It is not a person-only boundary: an install
   outside a marked run is allowed, an agent session included, and the
   HTTP routes authorize by the dashboard's own auth without attesting a
   person. That is enough because an install only ever makes a draft (see
   "What an install can never do").
2. `apb_core::templates::get(template)` or
   `Err(ToolError::NotFound(format!("template `{template}`")))`.
3. The id: `as_id` or the template's id. It must pass
   `apb_core::registry::is_safe_segment`, and
   `apb_core::bundled::is_reserved_id` must be false; else
   `Err(ToolError::Engine(..))` with `invalid id `<id>`` or
   `apb_core::bundled::reserved_id_message(id)`.
4. `super::scope_parent(root, scope)`.
5. The draft write, shared with `capture_draft`. Extract from
   `capture_draft` in `capture.rs` everything from the `store_origin`
   computation to the provenance write into

```rust
/// Writes `yaml` as draft `id` 1.0.0 under `parent` through the agent
/// channel (a draft never lowers a launch floor), marks it a draft and
/// writes its provenance. `Ok(Err(value))` is the `duplicate_id`
/// rejection the capture answers; `Ok(Ok((version, provenance, written)))`
/// otherwise, `written` saying whether the provenance file was stored.
pub(crate) fn write_draft(
    parent: &Path,
    id: &str,
    yaml: &str,
    global: bool,
    origin: CaptureOrigin<'_>,
    title: &str,
) -> Result<Result<(String, Value, bool), Value>, ToolError>;
```

   `capture_draft` calls it with the id it read from the yaml and builds
   its answer from the returned pair; its answers stay byte for byte what
   they were (the capture and discover tests prove it, their assertions
   unchanged). `CaptureOrigin` gets a third variant

```rust
    /// A person installed a template of the gallery: its id and the
    /// digest of its yaml.
    Template { id: &'a str, digest: &'a str },
```

   whose provenance is `{"created_by": "template", "title": <title>,
   "template": <id>, "template_digest": <digest>}` (the constant
   `apb_core::templates::CREATED_BY` for the first value). `write_draft`
   also returns whether the provenance file was written (the third value
   of its `Ok(Ok(..))`): the capture ignores it as before, the template
   install turns `false` into the warning `"provenance not written"`. The template
   install passes the template's `name` as the title, and skips the
   capture's secret scan and trigger dedup: the content is apb's own, and
   a template's trigger matching a person's playbook is no reason to
   refuse (the id decides).
   The store check (`refuse_linked_store` in `capture.rs`, called by
   `write_draft` before any write): the store's root is resolved first
   (`canonicalize` of the global store directory or of the project's
   `.apb`), so a root a person links on purpose (a linked config dir or
   `.apb`), or a link above it, is followed as before. Only what lies
   beneath that root is checked, without following links: the
   `playbooks` directory and the playbook's own directory, each existing
   component read with `symlink_metadata`. A link there is refused
   `store_path_linked: <path> is a link; a draft is written only into the
   store itself`, for the install and the capture alike. The same check
   runs again right before the eval tree is written into the playbook's
   directory (the eval copy's parent).
   Limit: this is a check before the writes, not writes anchored to a
   directory descriptor. A link swapped in beneath the root between the
   check and the writes (the version, its lifecycle, its provenance, the
   eval tree) is not covered. Closing that needs every version write of
   `versioning` anchored to a descriptor of the store's root, a change of
   its own, recorded as future work.
6. The eval files:
   `apb_core::templates::install_evals(&parent.join("playbooks").join(id),
   &template)`; a failure is one entry of `warnings` in the answer
   (`"eval scenarios not installed: <error>"`), never a failed install.
7. The fit report, only with `with_fit`: `apb_engine::fit::assess(root, &PlaybookRef { origin
   (Project { workspace_id: None } or Global), id, version: None })`,
   serialized whole under `"fit"`, and under `"needs"` one string per
   item of `apb_engine::fit::problems(&report)`, in its order: `<code>
   <subject>: <detail>`, or `<code>: <detail>` for an item without a
   subject (the fields of `FitItem`). A fit error is `"fit": null`,
   `"needs": []` and the warning `"fit report not made: <error>"`; without
   `with_fit` the same two values and the warning `"fit report not made:
   no project was named"`.
8. The warnings come from one pure function of `tools/templates.rs`,
   `warnings_of(eval: &Result<usize, String>, fit: Option<&Result<_,
   String>>, provenance_written: bool)`, over the raw results, called by
   `install`; the private `fn answer(..) -> Value` builds the answer from
   the id, the scope, the template id, the scenario count, the fit report
   and those warnings:

```json
{ "id": "dependency-security-fix", "version": "1.0.0", "scope": "project",
  "template": "dependency-security-fix", "lifecycle": "draft",
  "scenarios": 0, "fit": { },
  "needs": ["connector_account_missing github: no account is configured"],
  "warnings": [] }
```

   (the `needs` text above only shows the shape; the real text is the
   fit item's). `scenarios` is the count `install_evals` returned (0 on
   its failure).
   A duplicate id answers `{"rejected": "duplicate_id", "id": <id>}` (the
   capture's word), nothing written. The answer has no `trusted` field:
   whether the stored digest is approved is the fit report's trust
   section, which reads the real trust store.

#### What an install can never do

1. Approve anything: no code path of the install touches the trust
   store. Trust is machine-wide and keyed by digest, so identical content
   a person approved earlier is still approved; the fit report says what
   is true. In a clean trust store the draft is untrusted.
2. Skip the draft lifecycle: a normal start of a draft is refused
   `draft_requires_trial` by the run gate, with or without
   `acknowledge_untrusted`. A person runs a trial or approves the draft.
3. Lower a launch floor: the draft is written through
   `SaveChannel::Agent`, which refuses a definition looser than a floor
   the id kept (`launch_policy_loosened`); the install fails and writes
   nothing.
4. Known limit, shared with `playbook_capture` and not changed here: the
   draft's `lifecycle` file is written after `create_draft_in_by`
   published the version, so a crash between the two leaves an active
   definition of that id (the install creates no approval, but an earlier
   approval may already cover its digest). After draft creation, a
   failed eval copy, fit report or provenance write does not undo the
   install: each is a warning in the answer.

Tests, new `crates/apb-mcp/tests/suite/templates_tool_test.rs` (declared
in `crates/apb-mcp/tests/main.rs`; project and config dir fixtures as
`capture_tools_test.rs` has them):

1. `an_installed_template_is_a_draft_and_approves_nothing`: lifecycle
   `draft`; the trust store file of the test's config dir is byte for
   byte what it was before the install (or still absent);
   `apb_engine::gate::check_run` refuses the draft with the policy
   `draft_requires_trial` both without and with `acknowledge_untrusted`
   (launch context as `policy_test.rs` builds it).
2. `an_installed_template_records_its_provenance`: `provenance.json` has
   `created_by: template`, the template id and its digest.
3. `install_as_another_id_renames_the_playbook`: the project already
   holds a person's playbook whose trigger `when` equals that of
   `ci-failure-triage` (a template that has a trigger);
   `ci-failure-triage` installed `as my-inbox` succeeds, and the stored
   definition's `id` is `my-inbox`.
4. `a_second_install_is_a_duplicate`: `rejected: duplicate_id`; the
   first draft's files are byte for byte unchanged.
5. `a_reserved_or_unsafe_id_is_refused`: `as apb-mine` and `as ../x`
   are errors, each with its message, and write nothing.
6. `a_template_install_in_a_run_step_is_refused`: with `APB_RUN_ID` set
   (under the suite's env lock) the error names `template_in_run_step`;
   `list()` and `show(..)` still answer.
7. `the_install_answer_carries_the_fit_report`:
   `dependency-security-fix` in a project without a github account: the
   `fit` value has a blocking item whose text names `github` and `needs`
   holds a line naming it; with `with_fit: false` the answer has `fit:
   null`, empty `needs` and the "no project was named" warning.
8. `scenarios_are_installed_with_the_draft`: `task-check-first` answers
   `scenarios: 3` and three files exist under
   `<playbook_dir>/evals/scenarios/`.
9. `an_unknown_template_is_not_found`.
10. `a_global_install_writes_the_global_store`: with a private config
    dir, scope `global` writes `<config>/playbooks/<id>/` and nothing
    under the project.
11. `an_install_never_lowers_a_launch_floor`: the id has a retained
    floor stricter than the template's `agent_launch` (seeded as the
    launch floor tests seed one); the install fails naming
    `launch_policy_loosened` and no playbook directory of that id is
    left.
12. `a_previously_approved_digest_is_reported_as_it_is`: install, read
    the stored trust digest, remove the draft's directory, approve that
    digest in the test's trust store, install again: the fit report's
    trust section says the playbook is approved, and the install wrote
    no approval (the trust store bytes equal those after the manual
    approval).
13. `a_failed_eval_copy_is_a_warning`: `install` of
    `task-check-first` while another install holds the eval tree's lock
    (a fresh lock file in the playbook directory, which a draft creation
    accepts as a crash leftover): the draft exists, `scenarios` is 0, no
    `evals/` was written, and the answer's warnings hold `eval scenarios
    not installed: ..`.
14. `a_failed_fit_and_provenance_are_warnings`: with a failed fit report
    and a failed provenance write injected (`tools::install_faults`, a
    thread-local fault switch compiled only with the `testing` feature),
    the draft stays and both warnings reach the answer.
15. `each_failed_result_is_its_warning`, a unit test of `warnings_of`:
    failing inputs, each sentence checked.
16. `a_linked_store_path_is_refused_for_install_and_capture`: a project
    whose `.apb/playbooks` links to the global store's `playbooks`; a
    project install and a project capture are both refused
    `store_path_linked` before any write and neither store changes.
17. `a_linked_store_root_is_followed_for_install_and_capture`: with the
    config dir itself a link, a global install and a global capture land
    in the directory it names; with the project's `.apb` a link, a
    project install and a project capture do.
18. `a_link_beneath_the_store_root_is_refused_in_both_scopes`: a link at
    `playbooks` and a link at the playbook's own directory are each
    refused `store_path_linked` for an install and a capture, in both
    scopes, and the directory the link names stays empty.

### Surfaces

1. CLI, new `crates/apb-cli/src/templates.rs`:

```rust
#[derive(clap::Args)]
pub(crate) struct TemplatesArgs {
    #[command(subcommand)]
    cmd: TemplatesCmd,
}

#[derive(clap::Subcommand)]
enum TemplatesCmd {
    /// List the templates
    List { #[arg(long)] json: bool },
    /// Print a template's definition
    Show { id: String },
    /// Install a template as a draft
    Install {
        id: String,
        /// Where the draft goes: project (default) or global
        #[arg(long, default_value = "project")]
        scope: String,
        /// The playbook id to install as
        #[arg(long = "as", value_name = "ID")]
        as_id: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn templates_cmd(root: &Path, args: TemplatesArgs) -> ExitCode;
```

   `with_fit` and the project: `list` and `show` need no project. For
   `install`, `with_fit` is `Registry::open(root).is_ok()`; a
   project-scope install without an initialized project prints `no
   project here (run `apb init`)` and exits 2; a global install proceeds
   with `with_fit: false`.
   `Command::Templates(TemplatesArgs)` in `main.rs` next to `Suggestions`,
   doc line "List, show and install the example playbooks apb ships, as
   drafts", dispatched like its neighbours.
   1. `list`: one line per template,
      `<id>\t<version>\t<effects joined by ,>\t<name>`; `--json` prints
      `list()`.
   2. `show`: the yaml, exit 0; an unknown id prints `template `<id>` not
      found` and exits 2.
   3. `install`: on success prints `installed template <t> as draft <id>
      1.0.0 (<scope>)`, then one line `  needs: <text>` per entry of the
      answer's `needs`, then every warning as `  warning: <text>`, then `next: apb
      adopt <id> --scope <scope>` and `next: apb run <id> --scope <scope>
      (a normal start needs the draft approved; a trial runs it without
      activating it)`; these two lines carry the exact id, quoted for a
      POSIX shell when it is not one plain word
      (`next_action::command::shell_quote`), and go through the command
      sanitizer, which keeps spaces. `--json`
      prints the answer. Every string that came from a file goes through
      `sanitize_for_terminal`. Exit 0; 1 on `duplicate_id` (stderr:
      `playbook `<id>` already exists in the <scope> scope; pass --as
      <other id>`); 2 on any other refusal or error.
2. HTTP, new `crates/apb-server/src/routes/templates.rs` (`pub(crate)
   mod templates;` in `routes/mod.rs`), routes in `lib.rs` next to
   `/api/projects`:
   1. `GET /api/templates`: `list()`.
   2. `GET /api/templates/{id}`: `show`; 404 for an unknown or unsafe id.
   3. `POST /api/templates/{id}/install`, body `{ "scope":
      "project"|"global" (default "project"), "as": string?, "workspace":
      string? }`. The root and `with_fit`: `resolve_root(&state,
      workspace)` and `true` when that succeeds; for the global scope
      when it fails because no workspace was named,
      `resolve_root_for_scope(&state, None, "global")` and `false` (a
      global install needs no project, and then no fit report is made);
      a project install without a resolvable project answers the
      resolver's error. The install runs in `tokio::task::spawn_blocking`.
      Answers: 200 with the install answer; 409 with the body when it
      holds `rejected`; every `ToolError` through the mapping `state.rs`
      already has for tool-layer errors, `tool_refusal` (404 for
      `NotFound`, 409 for `Conflict`), as `routes/discover.rs` answers.
   The routes sit behind the same auth layer as every other route of
   their method (no new rule), and the dashboard's own server is never a
   run step.
3. Dashboard:
   1. `web/src/lib/components/Topbar.svelte`: a "Templates" link (`key:
      'templates'`, `href: '#/templates'`, an icon from the icon set the
      file already imports from) after "Playbooks".
   2. `web/src/App.svelte`: the route `#/templates` (page `templates`).
   3. New page `web/src/pages/TemplateList.svelte`: one card per template
      (name, description, effects as small labels, "N scenarios" when
      not zero), a project `<select>` (reachable projects), a scope
      `<select>` (project, global), an optional "Install as" text input
      and an Install button; after an install the card shows the fit
      report through the existing fit component the playbook page uses
      (the component `PlaybookView.svelte` renders the result of its
      `fetchFit` call with), the `needs` lines, the warnings, and a link
      to the new playbook (`installTarget`). The selected project is
      sent as `workspace` for both scopes whenever a project is
      reachable, and the Project select stays enabled for a global
      install: it names the project the fit report is made for. Each
      answer keeps the workspace it was installed with, and its link is
      built from that. With no reachable project, or when the project
      list cannot be loaded, the scope starts at global, no workspace is
      sent and the project scope is disabled; a global install then
      shows the two next commands as text in place of the fit report,
      and the link still works (the global page needs no project). A
      project install waits until the project list has answered; each
      template's button is disabled only while its own install is in
      flight, so different templates install side by side. A 409
      shows "A playbook with this id already exists in that scope; pick
      another id in Install as". Plain CSS over the existing CSS
      variables, the foreground variables for text as in G2, no utility
      class strings.
   4. A pure helper `web/src/lib/templates.ts`: `installTarget(scope,
      workspace, id)` (`#/playbook/<ws>/<id>` or
      `#/global-playbook/<id>`, each part through
      `encodeURIComponent`) and `effectLabels(effects)` (`irreversible`
      first, the rest in their order).
   5. `fetchTemplates`, `fetchTemplate` and `installTemplate` in
      `web/src/lib/api/core.ts`, in the style of `fetchPlaybooks`.
4. `TemplateSummary` is registered in
   `crates/apb-server/src/ts_contract.rs` next to the other `apb-core`
   types and `web/src/lib/api.gen.ts` is regenerated with
   `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`; never
   edited by hand.
5. Docs: `docs/HOWTO-authoring.md` gets "## Starting from a template"
   right before "## Sharing a playbook": the three commands, the
   dashboard page, "a template installs as a draft apb approves nothing for: run a
   trial, or approve it before a normal start", `--as`, the global scope and the
   next commands. `llms.txt` lists `apb templates` in its command list
   next to `apb adopt`; `README.md` lists it next to `apb adopt` in its
   command list.

Tests:

1. `crates/apb-cli/tests/suite/templates_cli_test.rs` (new, declared in
   `tests/main.rs`): `templates_list_names_every_template` (15 lines,
   one starting with `dependency-security-fix\t`),
   `templates_show_prints_the_yaml`,
   `templates_install_prints_the_needs_and_the_next_commands`
   (`dependency-security-fix`: a `needs:` line naming `github`, both
   `next:` lines), `templates_install_global_writes_the_global_store`
   (the test's `APB_CONFIG_DIR`), `a_duplicate_install_exits_1`,
   `an_unknown_template_exits_2`,
   `a_global_install_outside_a_project_has_no_fit_report` (an empty
   directory: exit 0, the "no project was named" warning),
   `a_project_install_outside_a_project_exits_2`,
   `an_installed_global_template_meets_the_global_start` (install any
   template with `--scope global`, then `apb run <id> --scope global`:
   exit 2 naming `draft_requires_trial`, before any agent starts, which
   proves the start read the global draft).
2. `crates/apb-server/tests/suite/templates_api_test.rs` (new, declared
   in `tests/main.rs`): `get_templates_lists_them`,
   `get_template_shows_the_yaml`, `post_install_creates_a_draft`,
   `post_install_twice_is_409`, `an_unknown_template_is_404`,
   `a_global_install_with_no_project_has_no_fit_report` (a server
   without a pinned root and without registered projects),
   `a_global_install_lands_in_the_global_store`.
3. `web/src/lib/templates.test.ts`: `installTarget links a global
   install to the global route`, `installTarget encodes its parts`,
   `effectLabels puts irreversible first`.
4. `web/src/pages/TemplateList.test.ts`, mounted with the fetch
   functions mocked: `a global install names the selected project for
   its fit report`, `without a reachable project the scope starts at
   global`, `when the project list fails the scope is global and no
   workspace is sent`, `the Open link keeps the project the install
   used`.

## Records

1. `CLAUDE.md` and `AGENTS.md` carry the same shared text (mirror rule).
   In the crate map: `apb-core` names the template gallery
   (`templates.rs`: embedded from `examples/playbooks/`,
   `TemplateSummary`, `install_evals`); `apb-engine` names the rule that
   a run of a global definition never settles, re-decides or patches a
   project playbook's candidate line, with `run_origin::origin_of_events`
   as the one parse of the origin label, `scope_of_events` and
   `scope_of_label` over it for every reader that decides by scope
   (known problems, concurrency, start keys, the run listing, health,
   `previous_success`, scenario capture), `run_scope` with its three
   answers `RunScope::Project`, `Global` and `Unreadable` (a run whose
   journal is missing or does not read counting for neither side),
   `candidate::runs::project_runs_of`, `patch_global_unsupported` and
   `patch_origin_unreadable`; `apb-mcp` `tools/` names `templates` (the
   one install entry shared with the HTTP API and the CLI, `write_draft`
   shared with `capture`, refused from a run step); `apb-cli` names the
   global start of `apb run --scope global` through the resolved engine
   starts (`run_resolved`, `start_detached_resolved`,
   `run_background_resolved`), `--supervise` refused for it
   (`supervise_global_unsupported`), and `templates.rs` (`apb templates
   list|show|install`, one install entry through
   `apb_mcp::tools::templates`); `apb-server` `routes/` names `templates`
   (the template gallery and its install) and the `scope` of the
   playbook list and detail in `playbooks` (`origin_of`, shared with
   `fit`).
2. This record: the design, decisions only.
3. No release notes file and no version bump: one-point-oh-freeze
   writes the notes of the release candidate from "Release notes" below.

## Release notes

1. Templates: `apb templates list | show | install <id> [--scope
   project|global] [--as <id>]` and the dashboard Templates page install
   an example playbook as a draft (apb approves nothing for it) with its eval scenarios and
   say what it still needs.
2. Global playbooks: `apb run <id> --scope global` starts a global
   playbook in the current project (also `--detach` and host execution;
   `--supervise` is refused for it); the dashboard lists global playbooks
   and shows one with its fit for a chosen project and the commands that
   run it (starting and editing stay on the CLI).
3. HTTP: the playbook list entries and the playbook detail carry `scope`;
   the detail takes `scope=project|global`.
4. A run of a global playbook no longer touches a project playbook of
   the same id: grading it does not re-decide the project's candidate,
   the project's quality comparison does not count it, and a supervisor
   patch of it is refused `patch_global_unsupported`.

## Review record

Two implementation review passes ran on 2026-11-21; every finding is
folded into the decisions above.
