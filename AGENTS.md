# AGENTS.md

This file provides guidance to coding agents working in this repository.

> Mirror rule: AGENTS.md and CLAUDE.md must stay in sync. Any change to the
> shared guidance in one file must be mirrored in the other in the same change.
> CLAUDE.md additionally carries a small "Claude-specific" section with no twin
> here.

## What this is

`apb` - a local runner for agentic playbooks: YAML playbook definitions, a
svelte-flow web visualizer, semver versioning, and an MCP server so coding
agents can drive playbooks. Rust workspace, edition 2024. Full design spec:
`docs/superpowers/specs/2026-07-08-workflows-cli-design.md` and the agent-profiles
spec `docs/superpowers/specs/2026-07-12-agent-profiles-design.md`.

## Crates (big picture)

Six crates under `crates/`; the dependency direction is core <- engine <- mcp,
with cli and server on top, and `apb-decide` (no workspace dependency) <-
engine, mcp. Do not introduce import cycles (enforced by code-ranker, see below).

- `apb-core` - domain layer, no async. Playbook schema (`schema.rs`,
  `Playbook::from_yaml`), validator (`validate/`, codes V01+, one module per
  rule family: `graph`, `nodes`, `connectors`, `templates`, `node_io` for
  `continue_session`, declared output fields, node `workdir`, the run
  `worktree` and `cache.key`, `baseline` for V62-V67, `list` for the list
  rules V85-V89 and V93-V95, `budget` for V96-V98, `params` for the secret
  param rules V99 and V100, `quality` for the `quality:` block and the
  supervisor policy V102-V107, `known_problems` for the `known_problems:`
  block and `quality.criteria_grader` V108-V111; `nodes` also holds V68, ids that start with `@`
  are the engine's, and V101, `audience` on a finish without a prompt), the
  "Notify me" settings (`notify/`: the machine target, the project's
  narrowing, a run's request, and `precedence` combining them), the run
  budget's schema,
  the precedence of a run argument over the playbook's block and the amount
  parsers (`budget.rs`), list mode (`list/`: the `list:` block, item
  loading with ids and the `where` filter in `items`, the per-item state
  machine in `verdict`), the baseline check
  (`baseline/`: the `baseline_check` block, the observation path reader and
  the rule comparison), the project's own test and lint commands
  (`project_checks.rs`), the findings and warnings an attempt reports and
  their sanitizer (`run_notes.rs`), one walk over every connector binding
  site, nodes, a list node's item check `<node>#check` and the engine sites
  `@baseline` and `@notify` (`connector/sites.rs`, `is_engine_site`), a
  function's `people:` block, the args people read and whether it can carry
  a notification (`connector/people.rs`), a
  function's send-once declaration and the connector's `identity`
  (`connector/idempotency.rs`) and the hidden send marker
  (`connector/marker.rs`), profiles
  (`profile.rs` types incl. `ProfileError` and `ProfileDoc::problems`, the one
  structural check of a run start, `apb validate` and every profile write,
  `profile_store.rs` scope resolution and bundle trust, `skills.rs`), a
  profile's `process_env` (`process_env.rs`: the policy, the `allow` syntax and
  the name-only plan; `process_env/sets.rs`: the base set, each agent's own
  set living in its module), registry and versioning (`versioning/`: `diff` the definition
  diff, `imported` the import provenance, `approval` the approval of a
  version by its exact digest), bundle export, import and the import preview
  (`bundle/`), remembered run input per project (`run_memory/`: `key`,
  `schema` the params digest and the value check, `store` the file, bound
  to the installation by a MAC for a global playbook), the credential-shape
  detector of values and param names (`token_shape.rs`), run quality
  (`quality/`: `criteria` the `quality:` block, the built-in criteria and
  their digests, `grade` the grade a grader stores, `compare` the comparable
  key and the deterministic comparison of a candidate with `current`), known
  problems (`known_problems/`: `block` the `known_problems:` block, `mod` the
  stored types, ids, significance and the location grammar `scope_of`,
  `store` the signed per-playbook file under a lock, `recheck` the pure
  rule that settles a recheck and the changed passages), the one scrubber
  every stored or sent text goes through (`scrub.rs`: `Scrubber` and its
  `flags` check), the
  one supervisor policy reader (`candidate/policy.rs`), the pure statistics
  (`stats.rs`: rates, medians, the Wilson interval), copying a playbook into
  another scope's store (`scope_move.rs`), schema 1->2 migrator
  (`schema_migrate.rs`), the built-in agents, one module each (`agents/`: the
  registry `all` and `lookup`, `agent.rs` the `Agent` trait, its descriptor,
  capabilities and verified CLI version, `forms.rs` the launch, resume and
  fresh-session forms, `output.rs` the output readers with `promises` and
  `conformance`, `setup.rs` what detection reads and the run-scoped
  environment, `policy.rs` the model rules, `check.rs` the doctor's free
  local check of an installed CLI (version against the verified one, the
  built-in form's flags in its help), `exposure.rs` the tools an agent
  reports against the ones an attempt was handed; `claude/`, `codex/`, `opencode/`
  and `zcode/` hold their readers and helpers, the other agents one file each;
  `apb_core::zcode` re-exports `agents/zcode`), free agent detection over the
  agent registry (`detect.rs`), curated models table
  (`models_table.rs` + `assets/models.yaml`), graph SCC and reachability helpers
  shared by the validator and the engine (`graphutil.rs`), content/bundle digests
  (`content.rs`), the sealed type of every resolved secret with its
  constant-time comparison and labelled redaction (`secret.rs`), trust store
  (`trust.rs`), atomic state IO, symlinks and dir
  locks (`fsutil.rs`), the single wall-clock source (`clock.rs`), the agent-neutral
  entry to each agent's output reader: reply and reported token usage
  (`agent_output.rs`), the execution mode resolution (`execution.rs`: the
  `execution` config section, `cli` or `host` per run, the fallback rule),
  the eval case and suite format with its V80-V83 checks, digests and the
  machine's suite approvals (`eval.rs`), the decision-model config (`decisions.rs`: the
  machine's `decisions.yaml`, project narrowing, the doctor line), the
  doctor's free per-provider request (`decision_probe.rs`), the
  measured-threshold store (`decision_thresholds.rs`), one decoder per agent
  log format (`agent_actions/`: `claude` and `codex` lines as tool actions,
  typed requests and the working directory, kept in their agent modules and
  re-exported here, `command` the shell command
  normalizer with the destructive list, `known` the subcommand and option
  lists a step token may name) and history discovery
  (`discover/`: `walk` the bounded no-follow walk of a root, `session` a
  session file as steps and requests, `token` an action as an
  argument-free step token, `mine` the repeated n-grams, `draft` a
  suggestion with its draft, scrubbed examples and sources, `store` the
  signed result store, the one-time resolve of a save or a decline and
  the saved slugs).
- `apb-decide` - a blocking, provider-agnostic decision-model client: the
  `DecisionProvider` trait, question and answer types, the `systemone`,
  `fake` and `llm_emulation` (uncalibrated) providers and the route
  adapters (`vercel_evaluate`, `openrouter_decisions`, `cloudflare`) over
  one shared HTTP exchange, limit checks and strict reply validation,
  retries, the provider chain, the per-run cache and the `ApiKey` wrapper; the `testing` feature adds a
  multi-response HTTP stub. Knows nothing about runs.
- `apb-engine` - execution. The drive loop (`scheduler.rs`) with its phases in
  `scheduler/` (`entry` start, handoff and dead-attempt reaping, `control_apply`
  the control scan, `supervisor` heartbeat and wake park, `node` execution,
  `journal` event-log folds, `status_file` the attempt verdict contract and its
  findings and warnings, `attempt_notes` their journal entry,
  `attempt_memory` the retry briefing on what earlier attempts did, `baseline` the
  before and after snapshots and comparisons, `resume`,
  `patch`, `rebind`, `cache`, `prepare`, `live`, `listing`, `handoff` the warm
  session handoff between nodes, `node_workdir` a node's templated working
  directory, `skills_copy` the shared per-profile skills copy, `transcript`
  per-attempt output, `worktree` the run's working tree and its lock move,
  `protect` the protected-path snapshot and restore, `goal` the goal criteria
  checked at run end, `provenance` the run id in the agent env and the
  commits a node made, `host` the host-mode adapter that turns an agent step
  into a host task, `dispatch` the one entry that sends a list node to its
  runner, `list/` the list runner: `source` the item load, `batch` the
  batches on the list's worker pool and each item's settlement, `check` the
  item checks, `run` one visit's shared state), list mode's pure half
  (`list/`: `ledger` the journal fold of a visit, `prompt` the `## item`
  prompt section, `stamp` the item stamp on item-scoped events, `view` the
  surface view), the execution key that pairs attempts, files and sends by
  node and list item (`exec_key.rs`), the host task store behind `run_task_submit` and
  `apb tasks` (`host_task.rs`), the baseline check's journal fold and its
  source reads (`baseline.rs`, `baseline/`: `read` a connector or script,
  `project` the project's commands), the read-only goal and commits blocks of the
  run surfaces (`run_outcome.rs`; `run_outcome/`: `baseline` and `notes` the
  baseline and findings views, `calls` connector calls by what they say about
  the outcome, `claims` the evidence and gap types, `commits` the commits and their `Apb-Run` trailers, `intents`
  the send intents and the unknown ones a person resolves, `lists` the list
  nodes' counts, item-check evidence and items that need a look, `summary` the
  human outcome every surface shows and the "needs a look" marker), the
  cross-run metrics behind `apb stats`
  (`run_stats.rs`, `run_stats/`, `findings` for findings and baseline
  results, `quality` for the levels of signed grades per comparable key and
  their `--compare` deltas), the irreversible-effects consent a run
  start records (`consent.rs`), the eval checks and result store behind
  `apb eval` (`eval/`),
  the operator pause: who paused a run and how, the pause every run surface
  reports, and the paused-interval clock every deadline, timeout and
  duration goes through (`pause.rs`; `pause/`: `kind` the journal-facing
  types, `request` the one pause entry point and what a paused drive leaves
  behind, `interrupt` the graceful kill registry of a `--now` pause,
  `waker` the detached resume at `until`, `resume_all` behind
  `apb resume --all`, `operator` the request built from surface arguments,
  `display` the one text line of a pause, `quota` the quota pause policy and
  schedule, `probe` the wake-up probe of a quota pause, run by
  `scheduler/quota_probe`; `budget` the budget pause's reason and request,
  `cause` the cause a quota or budget pause's reason names),
  the run budget (`budget.rs`; `budget/`: `kind` the journal-facing types,
  `fold` the limit in force and the gate's pauses and waivers, `spent` what
  the run spent per dimension with unknown attempts imputed, `gate` the pure
  admission decision, `resume` what a resume may change, `start` the check
  every launch surface makes before a start, `view` the status every surface
  reports; the drive side is `scheduler/budget_gate`), the run estimate
  (`run_estimate.rs`; `run_estimate/`: `history` the comparable runs,
  `sample` the per-run and per-node samples `apb stats` shares, `compose` the
  figures, `snapshot` the `estimate.json` a run starts with), the run lookups
  by id (`scheduler/run_ref`), "Notify me" (`notify/`: `start` the check
  every launch surface makes and a person's change on a live run,
  `transitions` the pure list of transitions due, `notice` the message,
  `send` the drive's sweep through the send ledger under `@notify`, `view`
  the block every surface reports, `trial` the test message of `apb notify
  test`), the text apb writes for people (`people/`: `brief` the writing
  brief and where it applies, `instruction` the review and supervisor
  instructions, `link` a run's dashboard link), the run report as one
  self-contained HTML page (`report_html/`), run quality (`grade/`:
  `sources` every view of a run a grader reads, `reader` the bounded,
  redacting, traced reader, `evidence` the one validator every grader's
  output goes through, `rules/` the rules grader of the built-ins, `model`
  the decision-model grader of the playbook's own criteria, `session`
  a host's grading session, `store` the write-once grade files;
  `transcript/` the attempt transcript adapter, claude's session and
  codex's `exec --json` stdout parsed with tool facts, the other agents'
  streams as plain text; `candidate/`: the gate's choice and
  the trial view in `mod`, `settle` by successes, `quality` by quality with
  `resettle` after a stored grade and a person's decision; `run_history.rs`
  the earlier runs of a playbook the estimate and the comparison share),
  the redactor every text that
  leaves the machine goes through (`redact.rs`, over
  `apb_core::scrub::Scrubber`), known problems (`known_problems/`: `observe`
  at a run's end, `at_start` what a starting run shows and journals once,
  `view`, `decide`, `recheck_one` and `scan` for the surfaces; `extract` the
  sightings of a journal, `source` a problem's current and old source text,
  `recheck` the model, host and person rechecks, `prompt` the attempt
  section and the supervisor line),
  the tree-scoped busy lock (`workdir.rs`), attempt failure
  classification and the bounded infrastructure backoff (`failure_class.rs`),
  the immutable write-once run manifest (`manifest.rs`), invocation resolution
  (`invocation.rs`), agent adapters (`adapter.rs`, with `adapter/spawn_env`
  the one environment every agent spawn gets: the process-env plan, the
  connector scrub, the run context, the agent's run-scoped environment in
  the directory `agent_home.rs` names), the process-env plan of
  one agent step and its `agent_env_applied` event (`agent_env.rs`; the
  per-step wiring in `scheduler/node/step_env`), the per-run doctor
  (`run_doctor.rs`, `run_doctor/env` the process env rows, `run_doctor/exposure`
  the tool exposure rows), the paid agent probe behind `apb doctor
  --probe-agent` (`agent_probe.rs`), the expected tools of an attempt and its
  `tool_exposure_mismatch` (`scheduler/node/exposure`), the append-only event log
  (`event.rs`), connector execution (`connector/`, with `call/` split into
  account selection, auth, encoding and response mapping, `attempt` the
  call's attempt binding, `target` the snapshot grant or the live
  trust-gated target a call resolves first, and `send_once` the send-once
  flow; `intent/` the
  send ledger: `identity` what makes two sends the same, `ledger` the fold
  and the admission table, `stamp` the key and the marker, `reconcile` the
  lookup of a marker send; `effect` what a call says about its effect), background
  supervisor spawn, legacy run-resume shim (`legacy_snapshot.rs`), the run
  policy gate every launch surface calls (`gate/`, with `trust_scan` the
  connector and profile-bundle trust facts collected before any refusal),
  the "will it fit here?" report (`fit/`: compatibility, `setup`, `trust` and
  consent kept apart, `report` the shapes), and the decision runner
  (`decision/`: the one entry point for decision-model uses, redaction,
  budget, replay, the `decision_made` event, the enforce gate, and one
  module per use: completion check, retry advice, supervisor wake triage,
  review recommendation, tier routing, `bounded` the shared ask over whole
  views the model grader and the recheck use, the judge node and edge answers
  (`scheduler/judge` runs them), and `report/`: the labellers, statistics
  and replay behind `apb decisions`; see `docs/DECISIONS.md`).
- `apb-mcp` - rmcp stdio MCP server (`server/`) and the tool layer in `tools/`
  (one module per domain: `playbook`, `run`, `supervisor`, `trial`, `capture`,
  `meta`, `intent` for `run_intent_resolve` and `apb intent`, `budget` for the
  budget arguments, the pre-start decision and the estimate and budget blocks
  the HTTP API reuses, `memory` for `playbook_memory` and the `remember` of a
  start, shared with the HTTP API, `notify` for the notify argument, the
  `notify_unavailable` start check, a live change and the run's `notify`
  block, shared with the HTTP API and the CLI, `grade` for grading a run,
  its grades and the `grades` block of `run_status`, where every stored
  grade re-decides the candidate (`after_stored`), `candidate` for the
  candidate comparison and a person's decision, `problems` for a
  playbook's known problems, a person's decision, a recheck and a scan,
  and `discover` for history discovery, its stored results, a save
  through `capture::capture_draft` and a decline, all shared with the HTTP
  API and the CLI); `promote/` is `playbook_make_global` (the
  plan and its blockers in `plan`, the apply in `mod`); `catalog_rank` is the
  opt-in decision-model ranking behind `playbook_catalog(query)`, asked through the engine's standalone decider
  (`decision/standalone.rs`).
- `apb-cli` - package name `apb` (bin `apb`, `main.rs`); thin dispatch over
  core/engine/mcp. `apb init` runs a short interactive questionnaire in a
  terminal (feedback-loop consent into CLAUDE.md/AGENTS.md, agent
  subscriptions survey); non-TTY runs skip it. `apb self-update` updates
  installer-based installs in place. `doctor.rs` is `apb doctor` (the
  environment, `--agents`, `--run <id>`, `--probe-agent <id>`). `apb intent list|resolve` shows a run's
  send intents and records a person's decision on an unknown one. `budget.rs`
  holds the budget flags of `apb run` and `apb resume`, the estimate printing
  and the terminal question of `on_no_estimate: ask`. `memory.rs` is `apb
  memory`, `make_global.rs` is `apb make-global`, `import_preview.rs` prints
  the preview `apb import` shows before it asks for acceptance. `notify.rs`
  is `apb notify` and the notify flags of `apb run` (forwarded to the
  supervised drive like the budget flags), `report.rs` is `apb runs <id>
  --html`, `grade.rs` is `apb grade` and the grade lines of `apb runs
  <id>`, `candidate.rs` is `apb candidate`, `problems.rs` is `apb
  problems`, `discover.rs` is `apb discover`.
- `apb-server` - axum web API: a thin router in `lib.rs`, one module per
  resource in `routes/` (`estimate` for the estimate route and the run and
  resume budgets, `fit` for the fit report, `memory` for remembered run
  input, `notify` for the machine's notify target and a live run's change,
  `quality` for a run's grades and a playbook's candidate, `problems` for
  a playbook's known problems, `discover` for history discovery, loopback
  only; `state.rs` also holds the tool-layer
  answer and refusal mapping they share;
  `runs` also serves the report page), shared request state in `state.rs`, and the svelte
  frontend from `web/` embedded via rust-embed (`assets.rs`, `web/dist`).

### Concepts that span files

- **Run quality** (`docs/QUALITY.md`): a playbook's `quality:` block names
  the built-in criteria and at most three of its own. The rules grader
  grades the built-ins as a run ends (`run_graded` before `run_finished`); a
  host grades the playbook's own in a bounded MCP reading session. Every
  grade goes through `grade::evidence` (cite only records read whole, in
  the views the criterion reads; an absence needs complete views read to
  their end) and keeps references, counts and one short redacted `why`,
  never the source text it read; it is signed with the installation key.
  A run takes one host grade until a person reopens it. A candidate under
  `promote_supervisor_patches: { when: quality }` is compared with
  `current` per comparable key (criterion digest, grader, task type; runs
  created on this machine and grades signed here only) and promoted on its
  own only when shown not worse (a non-inferiority margin; no evidence
  never promotes); a grade stored after a run ended writes no journal event
  and re-decides the candidate through
  `apb_mcp::tools::grade::after_stored`, which every grading surface calls.
  Grading and deciding a candidate are refused from a run step
  (`StepCaller`). The model grader (`model/1`) grades the playbook's own
  criteria only on request, through the standalone decider's daily-capped
  `grade` use; `quality.criteria_grader` says whose grades a comparison
  expects.
- **Known problems** (`docs/KNOWN-PROBLEMS.md`): every run that ends adds
  the tool problems it saw (findings with `kind: tool_problem` or a
  location in the grammar, and a connector function failing alike at least
  twice in one run) to its playbook's signed, scrubbed store
  (`.apb/problems/<scope>/<id>.json`). A playbook with a `known_problems:`
  block gets one `known_problems_shown` event at start, after
  `run_provenance`; attempt prompts rebuild their `## known problems`
  section from that event alone, so a resume shows the same. A problem
  whose source digest changed is rechecked first (by model only with
  `uses.problem_recheck` on, at most two per start); a close stands only on
  a quote of new source text (`known_problems::recheck::settle`), a host's
  or an advising model's close becomes `closed_proposed`, and so does an
  enforcing model's unless its answer is calibrated (or
  `allow_uncalibrated`) at `min_confidence`. A wording of an agent's text
  is shown only once it recurs in `min_runs` runs; an unsigned store is
  only counted. Every write (decide, recheck, scan) goes through
  `apb_mcp::tools::problems`, which refuses a run step (and, on MCP, a
  decision or recheck while a run of the playbook waits on a host task);
  reading is allowed anywhere.
- **Repeats in session history** (`docs/DISCOVER.md`): `apb discover`,
  MCP `playbook_discover` and the dashboard's Discover page read the Claude
  Code and Codex history of named projects under named roots
  (`discover.roots` in the global config; only the CLI takes a typed
  directory: `--sessions` for a history root, `--project` for a
  project), within fixed bounds and without following a
  link, as argument-free step tokens, and offer the repeated n-grams as
  drafts with scrubbed examples and `<project>/<file>` sources. No model
  and no network call; results are signed and stored under the config
  dir; a result names projects by workspace id, and a session is its
  deepest registered project's. Saving goes through `capture_draft`
  (`created_by: discover`), declining through the `suggestion_dismiss`
  store, each once under the discovery store's lock
  (`discover::store::resolve`). Every entry, reads included, is refused
  from a run step (`tools::discover::guard`); HTTP answers only a client
  straight from this machine (a route layer: loopback peer, loopback
  `Host`, no forwarding header) and never takes or returns a path.
  `rules/1` still reads tool facts from claude sessions only (codex facts
  are deferred to a `rules/2`, see `docs/QUALITY.md`).

- A **profile** is the single executor binding for an `agent_task` node: agent +
  model + ordered fallbacks + SOUL (role prompt) + skills, with a scope
  (Project | Global | Auto). Nodes reference it by name or `{ name, scope }`.
- A profile's `process_env` (`inherit`, the default, or `allowlist`) decides
  which operator variables an agent process inherits. Every agent spawn gets
  its env through `adapter::spawn_env`
  ([decision record 0004](docs/adr/0004-every-agent-spawn-gets-its-env-through-spawn-env.md)).
- `profile_digest` = hash of profile.yaml + SOUL.md; `bundle_digest` = profile
  digest plus the actual content digests of its skills. Editing a skill changes
  the bundle and drops trust.
- A run snapshots its resolved profiles, skills, and invocations into an
  immutable, write-once `runs/<id>/manifest.yaml`
  ([decision record 0002](docs/adr/0002-post-start-reads-come-from-the-manifest.md)).
- One run gate for every launch surface: `apb_engine::gate::check_run`
  (lifecycle, `requires`, playbook/profile/connector trust, sub-playbook pins).
  MCP, the dashboard and the CLI all call it; they differ only in
  `acknowledge_untrusted` (a person clicking Run or typing `apb run` is the
  confirmation, MCP asks the user first). `apb_mcp::policy` re-exports it.
- Import approval: a version `apb import` wrote with scripts records its
  import (`VersionProvenance.imported`) and runs only after an approval that
  names its exact trust digest (`versioning::approval::approve_version`,
  behind `apb trust approve --digest` and `playbook_approve.digest`); the gate
  refuses it with `imported_scripts_require_approval` for the playbook and
  every sub-playbook, and `acknowledge_untrusted` does not bypass it.
- Remembered run input lives in the project (`.apb/memory/<scope>/<id>.json`,
  git-ignored), bound to a digest of the params; a start applies explicit
  input, then compatible remembered values, then defaults. Secret params and
  credential-shaped values are never stored. The legacy
  `meta/instruction-draft.md` is a read-only seed.
- Anti-TOCTOU: the gate returns a `RunPermit`
  (playbook digest + the exact verified profile-bundle map) in one pass; the
  engine is handed that map verbatim as `expected_*` and rejects any drift
  ([decision record 0003](docs/adr/0003-the-run-permit-reaches-the-engine-verbatim.md)).
- Knowledge about a built-in agent lives only in its module under
  `apb_core::agents`
  ([decision record 0005](docs/adr/0005-agent-knowledge-lives-in-its-module.md)).
  A built-in form without its machine output means the CLI's format changed,
  while a custom form read as plain text is allowed
  ([decision record 0006](docs/adr/0006-a-built-in-form-without-machine-output-has-drifted.md)).
  Claude's headless form keeps `--output-format json`, so the tools an agent
  sees are checked on the stream transport only
  ([decision record 0007](docs/adr/0007-claude-headless-keeps-json-output.md)).
- schema 2 removed the old `executors` block; playbooks bind executors only
  through profiles. `apb migrate` converts legacy schema 1 playbooks.
- The supervisor is an optional agent that watches a run and intervenes via the
  `supervisor_*` MCP tools; its binding exists in the manifest only when the run
  is actually externally supervised. `supervisor_interrupt_attempt` takes an
  optional `node`: targeted at one attempt, or a whole-run broadcast when omitted.
- An **operator pause** (issue #200) goes through
  `apb_engine::pause::request_pause`: a live driver gets `Control::Pause`
  (drain: no new node; now: the stop watcher fires a separate pause fanout,
  never the Abort-only `cancel`, so in-flight attempts get SIGTERM, a grace
  period, then SIGKILL, and journal `attempt_finished { interrupted }` with
  no `node_finished`); a run nothing drives gets `run_paused` journaled
  directly. A paused run holds no driver: `--keep-lock` parks the busy lock
  as `paused:<run_id>`, and a pause with `until` leaves a detached
  `apb __pause-waker` that resumes only if the same `run_paused` (journal
  seq) still holds the run.
- A **quota pause** is the engine's own pause on `FailureKind::Quota`
  (`failure_class.rs`, design section 6): the node attempt loop (`node.rs`)
  tries a profile fallback first, then infrastructure retries unless
  `Retry-After` outlasts them, then returns `AttemptOutcome::EnginePaused`
  (the attempt journaled `interrupted`, no `node_finished`); the drive posts
  it as an engine `Control::Pause` with `until`, so the drain path applies
  it. At `until` the waker (and `apb resume --all`) runs
  `pause::probe_before_wake` before resuming; a probe that hits the quota
  re-pauses. Probe outcomes are `supervisor_action quota_probe` markers.
  Six quota pauses of a node without a success fail it.
- A **run budget** (`budget:` block, or a run argument over it per field)
  is a predictive stop, not a ceiling: `scheduler/budget_gate` weighs what is
  left (spent from the journal, unknown attempts imputed, parallel members'
  medians reserved) against a gated node's median per visit (per batch for a
  list node) from the run's `estimate.json`, journals any decision other
  than a plain admission as `budget_gate`, and pauses through the same
  engine pause as a quota (`pause::budget`, reason `budget (...)`, no
  `until`). The limit in force is the latest `budget_set` (at start, and at a
  resume that raises or sets it). A plain resume of a budget pause is
  refused (`budget_paused`) until a person changes the budget or lets the
  node run once; a supervisor or the engine never resumes one. Children are
  not gated and never get a `budget_set`; their spend counts toward the
  parent's.
- **Notifications** ("Notify me") sit outside the run permit: the target
  is the person's subscription, checked when it is set
  (`notify::start::check`, every launch surface calls it first and refuses
  `notify_unavailable`) and pinned in `notify_set` with the connector and
  account digests; every send checks the live target against those pins.
  A send is apb's own call under `@notify` through the send ledger,
  labelled `notify:<kind>.<seq>`, so a restart never sends twice; the run
  outcome leaves those sends out. A failed notification never fails the run.
  Only a person sets a target: `apb notify` refuses a change or a test
  inside a run step (`APB_RUN_ID`, `APB_HOST_TASK`, read through
  `apb_core::step_env`), a start from a step may not name its own target,
  and MCP `run_notify` only turns notifications on or off and narrows them
  (`notify::set` with `ChangedBy::Host`).
- A **baseline check** (`baseline_check`) reads its sources once before the
  first node that can act (`apb_core::effects::node_acts`) and once at the
  finish, journaled as `baseline_observed` and `baseline_compared`; the
  before snapshot lives only in the journal, so a resume reuses it. Anything
  that cannot be read honestly is unknown, never zero or met. It reports the
  change; whether the goal was reached comes from the goal criteria only.
  Findings and warnings (`attempt_notes`) are reported, never acted on. The
  run outcome (`run_outcome::OutcomeSummary`) keeps what the engine checked
  apart from what nobody checked.
- **Sending once**: a connector function that declares `idempotency`
  (`key`, `marker` or `none`) is recorded in the run journal before its
  request leaves (`send_intent_recorded`, synced under the journal lock) and
  settled afterwards (`send_intent_settled`: `confirmed`, `not_sent` or
  `unknown`). A repeat of the same intent (node, function, account and the
  `--intent` label, else an args hash) within the node's effect scope (since
  its last success) is answered `already_sent`, replayed with the same key,
  looked up by its marker, or refused (`unknown_effect`, `in_flight`,
  `attempt_closed`); the binding's `on_unknown: resend` allows a resend.
  A repeat is decided on the whole chain of a send (the first intent, its
  replays and resends) through one verdict, `ledger::chain_verdict`: any
  member confirmed means sent, and a member that did not go out never hides
  an earlier unknown one. The run outcome and a person's decision read each
  send on its own (`ledger::send_verdict`): replays share one nonce and one
  effect, while every resend is a separate send that stays open while
  unknown. A send binds to its attempt by the token
  `attempt_started` journals (`APB_ATTEMPT_TOKEN`). An unknown send is an
  outcome gap only a person resolves (`apb intent resolve` refuses inside
  a run; MCP `run_intent_resolve` is an instruction-level rule like
  `review_decide`). The retry briefing
  (`scheduler/attempt_memory`) tells the next attempt what the ledger holds,
  outside the node cache key.
- **List mode**: `list:` on an `agent_task` or `script` node runs its step
  once per item (from an earlier node's declared field, a list node's
  `items`, or inline), in batches on the list's own pool capped by
  `min(batch.parallel, max_parallel)`, while the node itself runs alone on
  the sequential arm. Each item ends `passed` or `needs_look` through the
  pure transition in `apb_core::list::verdict` (a rejection re-runs it while
  `batch.retries` lasts; a pause, quota pause or crash returns it to
  pending without spending a round; an unknown `key`/`marker` send gets one
  free reconcile run, a `none` one needs a look). The journal holds the
  visit (`list_started`, `list_batch_started`, `list_item_settled`, and
  `item` on attempt, connector call, send intent and decision events);
  `ListFold` is the one reader for resume, supervisor retry, `rerun:
  not_passed`, progress and every surface. Everything keyed by node is
  keyed by `ExecKey` (node + item) so items of one node can run at once.
  The prompt is rendered once per visit; the item goes in a trailing
  `## item` section, so templates may not read `item.*` (V86). Host
  execution mode refuses list nodes at run start.
- Fan-in is a **join**: an explicit `join: all|any` on an incoming edge, or an
  implicit wait-for-all when an acyclic fan-in carries no `join` at all (a fan-in
  inside its own cycle stays first-arrival, else loops deadlock). An unreachable
  source counts as satisfied. Inside a loop a join counts only deliveries since
  it last started, and routing reads each node's latest execution
  (`RunState::seqs`; liveness in `parallel/liveness.rs`). Concurrency is capped by `max_parallel`
  (`defaults.max_parallel`, then the run config, then the engine default 4).

## Commands

Build (debug): `cargo build`. Release (bakes the frontend first):
`cd web && bun install && bun run build` then `cargo build --release`.

Test everything: `cargo test --workspace`. A single test:
`cargo test -p <crate> --test main <module>::<test_name>` (each crate has
one integration binary, `main`; e.g.
`cargo test -p apb-engine --test main profile_run_test::legacy_run_resume_via_ephemeral_snapshot`).

Frontend (`web/`, bun + vite + vitest): `bun run test`, `bun run build`,
`bun run check`.

Generated TypeScript: `web/src/lib/api.gen.ts` (run payload types) and
`web/src/lib/consts.gen.ts` (core constants) are rendered from the Rust side by
`apb-server`'s `ts_contract` test, which fails when they drift. After changing
a mirrored Rust type or constant, regenerate with
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract` and commit the
result; never edit the generated files by hand.

Format and lint gates (must be clean):
`cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets -- -D warnings`.

### Release pipeline (dist)

Releases are built by [cargo-dist](https://opensource.axo.dev/cargo-dist)
0.32.0, configured in `dist-workspace.toml`. `dist generate` writes
`.github/workflows/release.yml` from that config; the file is GENERATED and
must never be hand-edited directly. To change release CI, edit
`dist-workspace.toml` (and the referenced `.github/build-setup.yml`,
`.github/workflows/test-gate.yml`, `.github/workflows/release-notes.yml`),
then re-run `dist generate` and commit the regenerated workflow alongside the
config change.

Pushing a tag `vX.Y.Z` triggers the release workflow: it builds the shell
installer, the Homebrew formula (tap `itechmeat/homebrew-agentic-playbooks`),
and archives for the configured targets. A test gate (fmt, clippy, nextest,
doctests, and a check that `docs/release-notes/<tag>.md` exists) runs as a
local-artifacts job next to the builds, one matrix leg per shipped OS (Linux,
macOS arm, macOS Intel), while the build legs only build; the host job that
creates the release requires the gate job to succeed, so a failing gate leg
blocks publishing. The host job attests every artifact
(`github-attestations`, keyless build provenance) before creating the release.
Every action is pinned to a commit SHA (`[dist.github-action-commits]` for the
generated file); `release_workflow_test` in apb-cli checks all three. `dist plan` also runs
on every PR (`pr-run-mode = "plan"`) to catch config drift before a tag is
pushed. `docs/release-notes/<tag>.md` is required per tag and becomes the
published release body via a post-announce job.

Every archive also ships `THIRD-PARTY-NOTICES.txt` (`include` in
`dist-workspace.toml`), generated from `Cargo.lock` and `web/bun.lock` by
`scripts/third-party-notices.sh` (cargo-about with `about.toml`/`about.hbs`,
and `web/scripts/third-party-notices.ts` for what Vite bundles). After a
dependency change, regenerate and commit it; CI and the release test gate run
`scripts/third-party-notices.sh --check`.

## Required gates

- **Before code is ready to commit:** run code-ranker and fix any violation.
  First warm the cargo cache (the rust plugin needs it, else an offline error):
  `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .`
  (exit != 0 on a violation). For a violation, read `code-ranker docs base <ID>`
  before fixing, fix, and re-run until clean. It guards dependency cycles (ADP),
  cohesion, complexity, and SOLID/DRY/KISS.
- **Before code is considered ready for release:** run `cargo clippy --release`.
  Fix every error in the logs; also clean up warnings as far as practical.
- Commit only after the owner approves. Do not commit or push on your own
  initiative.
- **Never upload, publish, or send any file to any server or external service
  without the owner's explicit per-action approval** (git push, artifacts,
  deploys, package registries, file sharing - any network destination). Local
  writes are fine; anything leaving the machine needs an approval first.

## Conventions

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings.
  No CJK anywhere in code or prose. Machine-facing fields are English;
  user-facing chat messages are written in the user's chat language.
- New `EventPayload` fields are added only with `#[serde(default)]`
  ([decision record 0001](docs/adr/0001-new-events-are-safe-to-skip.md)).
- A new `EventPayload` variant must be safe to skip up to the next checkpoint
  (`EventPayload::is_checkpoint`): an older binary skips an event type it does
  not know and counts it, and the engine continues a run only when a
  checkpoint follows every such event (`event::read_all`). Stopping a run is
  the one exception: it only appends `run_aborted`, itself a checkpoint, so it
  goes through with a warning (`event::read_all_for_stop`). Never replace a
  known event with a new type; add alongside it. A structured optional field
  of a known event (such as `AttemptFinished.usage`) decodes leniently, so a
  shape a newer apb wrote reads as absent instead of failing the journal.
- State files are written atomically (temp + rename, 0600 on unix) via
  `apb_core::fsutil`.
- Secret values (auth files) are never returned, logged, or cached; skill
  content is never embedded into a prompt (skills are delivered by name, or as
  materialized snapshot copies for isolated nodes).
- Resolved secrets are `apb_core::secret::Secret`: `Debug` and `Display` print
  `***`, it is neither `Serialize` nor `PartialEq`, and it is compared only
  through `ct_eq` (`apb_core::secret::ct_eq_str` for anything else
  secret-derived). Read the value only through `expose_secret`, at the point
  it must leave the process (a request header, a login, a one-time print).
- Profile and skill names: `[a-z0-9][a-z0-9-]*`, at most 64 chars; path segments
  are validated with `is_safe_segment` / `validate_profile_name`.
- Search the code with zg (zvec-grep) over the live files:
  `zg query --rg -F <symbol> <path>` or `zg query --fts <name>` annotate each
  ripgrep hit with its enclosing symbol and line range; the semantic
  `zg query "<question>"` is for orientation in unfamiliar code only. The index
  lives in `.zvec-grep/` (git-ignored); rebuild it with `zg index` after a
  merge. Verify an edit (no `oldName` left) with plain `rg`/`git grep`, never
  through an index.

## apb feedback loop (standing instruction)

apb (agentic-playbooks) is under active development. On every playbook run you
supervise from this workspace, collect flaws, bugs, and improvement ideas that
belong to apb itself (the MCP server, the playbook engine, the validator, the
supervisor tools, the run lifecycle). Keep those separate from problems that
belong to this project's own playbooks, profiles, or prompts.

When the run is finished and the list is not empty, follow the reporting
instruction in
https://github.com/itechmeat/agentic-playbooks/blob/main/docs/FEEDBACK-LOOP.md
to check existing issues and file one consolidated issue for the run. Never
include secrets, tokens, or private prompt content in an issue.
