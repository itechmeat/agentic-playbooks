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
  `Playbook::from_yaml`; a node's `results:` entries are `ResultDecl`), validator (`validate/`, codes V01+, one module per
  rule family: `graph`, `nodes`, `connectors`, `templates`, `node_io` for
  `continue_session`, declared output fields, node `workdir`, the run
  `worktree` and `cache.key`, `baseline` for V62-V67, `list` for the list
  rules V85-V89 and V93-V95, `budget` for V96-V98, `params` for the secret
  param rules V99 and V100, `quality` for the `quality:` block and the
  supervisor policy V102-V107, `known_problems` for the `known_problems:`
  block and `quality.criteria_grader` V108-V111, `isolation` for a node's
  `isolation` V112-V115, `publish` for a `publish` node and `publish_only`
  V116-V120 and a publish from a read-only node V140, `edges` for edge ids, pairs and judge values V121-V125, `review` for the routes out of a `human_review` (`review_routes`, `review_takes`) with V136 a decision only bounded edges route, V144 a `review_status` edge no decision matches and V145 an option no edge carries, `retry` for `retry_delay` V126-V130, `outputs` for the run `outputs:` block V131-V135, `read_only` for a node's `read_only` V137-V139, `unattended` for a gate's `unattended` outcome V141-V143 with the walk the run rechecks (`unattended_decide_refusal`), `results` for a node's `results:` declaration V152-V154 (kinds, the path grammar, the cap) with `snapshotted_nodes`, the nodes whose executions take a snapshot (also every node an `approves`, a `requires_approval` or an `evidence.from` names), `approval` for a gate's `approves`, a node's `requires_approval` and `person_only` V155-V158, `evidence` for an `agent_task`'s `evidence:` block V159, `guarded` for the features a run guard names (`GuardedFeature`, `guarded_features` of a definition, a name this binary does not know refused by `parse`); `nodes` also holds V68, ids that start with `@`
  are the engine's, and V101, `audience` on a finish without a prompt), the
  "Notify me" settings (`notify/`: the machine target, the project's
  narrowing, a run's request, the outcome classes and the journal names of
  a filtered `finished` (`Coverage`), and `precedence` combining them),
  the settings file `policy.yaml` of the machine and the project
  (`policy_file.rs`: the strict reader of its `notify:` filters and its
  `launch:` section, and the doctor line; nothing in apb writes it), the
  billed use (`billing.rs`: the `billing:` section of `policy.yaml`,
  `EffectiveBilling` with a project only narrowing, the `BillingRecord` a
  run keeps and `covers`), the
  launch policy of agents (`launch.rs`: `resolve` and `EffectivePolicy`
  over a run's tree, the `LaunchRecord` a run keeps; `launch/policy.rs` the
  three policies, a leaf the schema and the settings reader share;
  `launch/floor.rs`: the `FloorKey` of the owning workspace (its id
  registered at that root in the projects registry, `projects::bind_id`
  and `registered_id`) or the global store, the `launch-floors.json` store
  under its lock with its directory synced, the scan of a key with no
  record (its store and the id's trash entries), `hold` the lock a start's
  final recheck keeps until its manifest is written, and `publish`, the
  raise, loosening and editor-lowering check every version publication runs
  before its rename), the run
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
  version by its exact digest, `error` and `sidecar` the error and the
  provenance types, `current` the one locked operation that moves
  `current` with its intent file, recovery and value-audited history:
  `current/model` the history line, `history` the audit and the newest
  move, `settled` the pointers as recovery leaves them, `intent` the
  intent file, `fault` the test-only crash points), bundle export, import and the import preview
  (`bundle/`), the content apb ships from the repository folder `bundled/`
  (`bundled/`: the embedded playbooks and skills and the reserved `apb-` id
  prefix every create path refuses (`reserved_id`); every write of shipped
  content holds one lock under the config dir (`hold`, `try_hold` for the
  reconcile a start runs); `playbooks` the install
  into the global store as ordinary versions trusted `Bundled` for their
  exact digest; every write takes the playbook's pointer lock, runs
  `current::recover` first and decides through one `classify` over
  `current::settled` (absent, pristine, modified, or newer installed,
  which every path refuses), and writes the shipped version inside the move itself
  (`current::move_current_writing_locked`), so an interrupted install or
  upgrade is an aborted or rolled-forward move; the read-only state projects
  a pending intent without writing,
  `replace_modified_playbook` the shipped content over a changed default on
  a person's request and `restore_replaced_playbook` its undo,
  `skills` the copy into the global skills dir with its sidecar digest and
  claude bridge, `SkillUpdates` several skill installs judged once for the
  role profiles, `replace_modified_skill` with the changed copy moved to a
  backup (put back when the write fails) and `restore_replaced_skill` its
  undo, `role_trust` the approval of a setup-made role profile carried to
  its new bundle digest after a shipped skill update, only while the
  profile is what setup wrote and every skill it carries holds the shipped
  files (the origin it had, never `Bundled`), `roles` the plan of the
  `apb-main`, `apb-reviewer` and
  `apb-researcher` profiles from detection and the models table; the
  profiles are written by `apb_mcp::profile_tools::write_role_profiles`;
  `InstallState`, `playbook_state` and `skill_state` the read-only state
  setup and the doctor report (no lock, nothing written), `report` the one wording of what an
  upgrade reconcile did (`describe`, `reconcile_defaults`), `remove_playbook` to the global trash
  (`versioning::delete_global_playbook`) and `remove_skill` of an unchanged
  copy),
  the staged install of an embedded file tree shared with the official
  connectors (`tree_install.rs`, one install of a target at a time under
  its lock, leftovers judged by the time in their name), remembered run input per project
  (`run_memory/`: `key`,
  `schema` the params digest and the value check, `store` the file, bound
  to the installation by a MAC for a global playbook), the credential-shape
  detector of values and param names (`token_shape.rs`), a run's sensitive
  params (`run_params.rs`: `sensitive`, the one rule remembered input and
  run redaction share, and `redacted`), the end-of-turn save check of
  `apb hook stop` (`save_nudge/`: the language-independent rule over the
  last task in `mod`, `task` the session tail read by the discovery
  decoder, `state` once per session and the cached model answers under
  the config dir), the process-wide read-only mode of
  `apb mcp --read-only` and the guard every write and program start checks
  (`process_mode.rs`; its scenario mode, entered once by a scenario run
  process, refuses a program start and a connector call at every level, a
  decision request at quick, and a write outside its write root, each as
  `scenario_guard: <operation>`, and keeps what it refused in
  `scenario_refusals`), run quality
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
  another scope's store (`scope_move.rs`, under the source's pointer lock,
  with a `scope_move` history line on the staged copy), schema 1->2 migrator
  (`schema_migrate.rs`), a node's isolation (`isolation.rs`: the level, the
  network, the extra paths and the machine's settings; `isolation/contract`
  the pure contract of what this machine enforces, with its code and reason
  sentences; `isolation/paths` the extra paths an author may name and the
  credential locations nothing binds), the hardened git command every git
  call apb makes goes through (`git.rs`) and its one exception, the
  transport-enabled builder of the publish step's push and remote listings
  run from a shadow git directory (`git/transport.rs`), the publish step's
  pure rules (`publish.rs`: options, node outputs, refusal codes and their
  sentences, the plan digest and nonce; `publish/`: `url` the accepted push
  URLs, `refname` the ref and remote name rules, `porcelain` a push's
  outcome), the built-in agents, one module each (`agents/`: the
  registry `all` and `lookup`, `agent.rs` the `Agent` trait, its descriptor,
  capabilities and verified CLI version, `forms.rs` the launch, resume and
  fresh-session forms, `output.rs` the output readers with `promises` and
  `conformance`, `setup.rs` what detection reads and the run-scoped
  environment, `policy.rs` the model rules, `check.rs` the doctor's free
  local check of an installed CLI (version against the verified one, the
  built-in form's flags in its help), `exposure.rs` the tools an agent
  reports against the ones an attempt was handed, `sandbox.rs` what an agent
  needs inside the sandbox (its login, files to bind or copy), `mcp_host.rs`
  the `McpHost` an agent CLI that is an MCP host gives (`Agent::mcp_host`:
  its config path, whether a server is registered, its CLI's register and
  remove commands or a byte-exact insert and removal, each agent's own in
  its `mcp` module), `hook_host.rs` the `StopHookHost` an agent CLI with an
  end-of-turn hook gives (`Agent::stop_hook`: its settings path, the
  byte-exact insert and removal of apb's hook, the event it hands the hook
  and the answer that keeps the turn going; claude's in `claude/hooks.rs`);
  `claude/`, `codex/`, `opencode/`
  and `zcode/` hold their readers and helpers, the other agents one file each;
  `apb_core::zcode` re-exports `agents/zcode`), free agent detection over the
  agent registry (`detect.rs`), curated models table
  (`models_table.rs` + `assets/models.yaml`), graph SCC and reachability helpers
  shared by the validator and the engine (`graphutil.rs`, with
  `reachable_with_policies` the one reachability from the start and every
  failure policy handler that V07 and the run graph's "why it did not run"
  share), the address of one
  edge in one definition with its rank and check order (`edge_address.rs`,
  shared with the dashboard's port through
  `tests/fixtures/edge_addresses.json`), content/bundle digests
  (`content.rs`), record packages (`records.rs`: the JSON Lines header
  and records of a `format: records` entry, `parse` with its first
  problem, the record id grammar and its percent-encoding), result
  snapshots (`result_store.rs`: the layout of
  `runs/<id>/results/` and apb's own run storage names, the limits, the
  canonical manifest with its `SnapshotSubject` and id, the `ResultStamp` a
  settlement event carries, the omission and incomplete reasons, the
  citation grammar `r:<id>/<entry>[@<record>][#[a,b)]` with `EntryId`,
  `records_invalid` on an entry declared a package that does not parse, the
  secret-shaped names, and `ResultTxn`, the staging transaction under the
  run's results lock with its run reservation), the sealed type of every resolved secret with its
  constant-time comparison and labelled redaction (`secret.rs`), trust store
  (`trust.rs`), atomic state IO, symlinks and dir
  locks (`fsutil.rs`; `fsutil/beneath` the descriptor-relative reads and
  writes beneath one anchor: no link followed at any component, a regular
  file with one link only, capped reads that never cut, 0700 and 0600 at
  creation, `flock` locks closed on exec), the single wall-clock source (`clock.rs`), the agent-neutral
  entry to each agent's output reader: reply and reported token usage
  (`agent_output.rs`), the execution mode resolution (`execution.rs`: the
  `execution` config section, `cli` or `host` per run, the fallback rule),
  the eval case and suite format with its V80-V83 checks, digests and the
  machine's suite approvals (`eval.rs`; `has_suite` is `suite.yaml` or a
  case file, so `evals/scenarios/` alone is no suite), scenario files
  (`scenario/`: `format` the types of
  `evals/scenarios/<name>.yaml`, `mod` the loader of named regular files
  below the playbook, the digest and `staleness`, `validate` V146-V151 and
  `validate_all` behind `apb validate`), the judge node digest
  (`judge::node_digest`, `j1:` over state, questions, thresholds and
  `on_unavailable`), `judge::expected_names` and `labelled_value` (what a
  label names in a judge output) and `validate::asks_decision_models`,
  labelled judge examples on disk (`judge_example.rs`: `evals/judge/<node>/`,
  safe example ids, `next_id`, reads of plain files only, no link followed),
  the keyless demo (`demo.rs` and `assets/demo/`: `demo-report` installed
  into `<config dir>/demo` through `versioning::create_with_scripts`, an
  ordinary person-channel version neither imported nor approved, and
  removed again; nothing in the gate knows it), the
  decision-model config (`decisions.rs`: the
  machine's `decisions.yaml`, project narrowing, the doctor line), the
  doctor's free per-provider request (`decision_probe.rs`), the
  measured-threshold store (`decision_thresholds.rs`, with the v2 file of a
  use whose labels have rule versions: `retry_advice` under `exec/1`), one decoder per agent
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
  `stop_context` the stop a chain step reads after a failed attempt (a pause
  or abort ends the chain, a supervisor interrupt names itself),
  `journal` event-log folds, `status_file` the attempt verdict contract and its
  findings and warnings, `attempt_notes` their journal entry,
  `attempt_memory` the retry briefing on what earlier attempts did, `baseline` the
  before and after snapshots and comparisons, `resume`,
  `patch`, `rebind`, `cache`, `prepare`, `live`, `listing`, `deferred` the
  deferred retries of `retry_delay` (the delay, the due time on the
  paused-interval clock, the continuation and the hold of a waiting node),
  `handoff` the warm
  session handoff between nodes, `node_workdir` a node's templated working
  directory, `skills_copy` the shared per-profile skills copy, `transcript`
  per-attempt output, `worktree` the run's working tree and its lock move,
  `protect` the protected-path snapshot and restore, `node/read_only` the
  before and after tree states of a `read_only` node, its
  `read_only_checked` record and its `read_only_violated` exit,
  `unattended` the drive's outcome of each wait in an unattended run (the
  mode from the manifest, a gate's decision, a child's stop), `goal` the
  goal criteria
  checked at run end, `provenance` the run id in the agent env and the
  commits a node made with the head each attempt left, `result` the
  snapshot capture at every settlement of a declaring node's work (scrub,
  limits, fail closed, the stamp journaled before the transaction ends),
  `host` the host-mode adapter that turns an agent step
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
  `project` the project's commands), the publish step (`publish/`: `pin`
  the remote fixed before the first acting node, `plan` the read-only plan
  and its refusals, `intent` the push in the send ledger with its `ref` mode
  and reconcile, `push` the one networked git call, `view` the fold the run
  surfaces show; the drive's arm is `scheduler/publish.rs`, its consent
  waits in `scheduler/review_gate.rs`, shared with `human_review`), the
  read-only goal and commits blocks of the
  run surfaces (`run_outcome.rs`; `run_outcome/`: `baseline` and `notes` the
  baseline and findings views, `calls` connector calls by what they say about
  the outcome, `claims` the evidence and gap types, `commits` the commits, their `Apb-Run` trailers and the attempt that made each, `intents`
  the send intents and the unknown ones a person resolves, `publish` the
  publish nodes' lines and done lines, `lists` the list
  nodes' counts, item-check evidence and items that need a look, `summary` the
  human outcome every surface shows and the "needs a look" marker,
  `route_evidence` what the journal proves about routing: raw traversal
  records, deliveries normalized by source execution, segment and edge, and
  routing episodes only a complete one of which supports a negative claim,
  `graph` the run graph over it with taken, not taken and not established
  edges and `RunGraph::present` that redacts before it clips,
  `judge_answers` each judge node execution and judge edge answer with the
  comparison its question kind routes on, `route_explain` "why it did not
  run" per entry with its class, `overlay` a run laid over a playbook
  version with its alignment and per-segment mapping), the
  cross-run metrics behind `apb stats`
  (`run_stats.rs`, `run_stats/`, `executors` the executor report: per node
  and executor group the chain steps, the transitions by close reason and
  the host's hint follow, the one hint rule the run surfaces share,
  `stats_run` one run as the report reads it, `route` for membership over established
  definitions and the reasons a node did not run, `findings` for findings and baseline
  results, `quality` for the levels of signed grades per comparable key and
  their `--compare` deltas), the irreversible-effects consent a run
  start records (`consent.rs`), the eval checks and result store behind
  `apb eval` (`eval/`), the reading view of a run's result snapshots every
  surface answers through (`result_view.rs`: rows by node, execution and
  item, every manifest and object re-hashed on every read, text through the
  run redactor, `compare_live` a word only, `verified_manifest` and
  `live_state` for an approval, `stored_entries` for an evidence session)
  and their cleanup
  (`result_clean.rs`: dead transactions resolved by the journal, only what
  no stamp and no open transaction names removed, `survey` writing nothing),
  evidence for node outputs (`evidence/`: `sources` the latest stamped
  snapshots of the `from` nodes bound and built into views of records,
  `session` the per-attempt session file under `runs/<id>/evidence/`, its
  open before the attempt, its bind to `attempt_started`, its reads
  through `citation::reader` and its close, `check` the claims of a
  succeeded attempt with their verdicts and flags, `rows` in `mod` what
  every surface shows with the fixed wording; the attempt loop opens and
  closes it in `scheduler/node.rs`, and `on_invalid: fail` rejects as
  `check_rejected` in `success_check_rejection`),
  the approval of an exact snapshot set (`approval.rs`: `ReviewBinding`
  with its `BoundSnapshot`s and `BindingMissing` nodes, the binding nonce
  over run, gate, visit and every bound snapshot, `ApprovalRefusal` with
  its sentences, `ApprovalFailure`; `scheduler/review_gate.rs` binds a
  request and re-verifies its snapshots when it consumes the decision,
  `approval_snapshot_unreadable` ending the run; `scheduler/approval.rs`
  `check_before`, the re-check of a `requires_approval` node at admission
  before `node_started`, sequential and batch, and `approved_binding` and
  `approved_head`, which the publish arm reads for its own check before
  every plan), the decision channels (`decision_channel.rs`:
  `DecisionChannel`, a person's only `terminal` and `dashboard_session`,
  and `CallerRefusal`, the closed list every facade renders;
  `review/channel.rs`: `decides_person_only`, `refuse_for_person_only` and
  `refuse_own_run`, a run step never deciding or answering for its own run,
  its ancestors or descendants at depth 8, checked by
  `question::post_answer_to` too; `review::post_decision_via`, the one
  entry that states a channel and stamps `request` and `channel` on the
  entry),
  the operator pause: who paused a run and how, the pause every run surface
  reports, and the paused-interval clock every deadline, timeout and
  duration goes through (`pause.rs`; `pause/`: `kind` the journal-facing
  types, `request` the one pause entry point and what a paused drive leaves
  behind, `interrupt` the graceful kill registry of a `--now` pause,
  `waker` the body of the pause waker (`--seq`, the resume at `until`) and
  of the release waker (`--release`, the resume of a released run when its
  deferred retry is due), each resuming only while its pause or release
  (`WakeHold`) still holds the run, `resume_all` behind
  `apb resume --all`, `operator` the request built from surface arguments,
  `display` the one text line of a pause, `quota` the quota pause policy and
  schedule, `probe` the wake-up probe of a quota pause, run by
  `scheduler/quota_probe`; `budget` the budget pause's reason and request,
  `cause` the cause a quota, budget or billing pause's reason names,
  `billing` the billing pause a spawn takes),
  the run budget (`budget.rs`; `budget/`: `kind` the journal-facing types,
  `fold` the limit in force and the gate's pauses and waivers, `spent` what
  the run spent per dimension with unknown attempts imputed, `gate` the pure
  admission decision, `resume` what a resume may change, `start` the check
  every launch surface makes before a start, `view` the status every surface
  reports; the drive side is `scheduler/budget_gate`), the run estimate
  (`run_estimate.rs`; `run_estimate/`: `history` the comparable runs,
  `sample` the per-run and per-node samples `apb stats` shares, `compose` the
  figures, `snapshot` the `estimate.json` a run starts with, `types` the key
  a node's history matches: profile, bundle and executor, `ESTIMATE_KEY_VERSION`), the run lookups
  by id (`scheduler/run_ref`), "Notify me" (`notify/`: `start` the check
  every launch surface makes and a person's change on a live run,
  `transitions` the pure list of transitions due, `notice` the message,
  `send` the drive's sweep through the send ledger under `@notify`, `view`
  the block every surface reports, `trial` the test message of `apb notify
  test`, `retry` the notify waker's body (`--notify`) and the one sweep
  `apb resume --all` gives an ended run), the detached `apb __pause-waker`
  processes (`waker.rs`: spawning the pause, release and notify wakers and
  the OS-locked lease that keeps one waker per wait under
  `runs/<id>/wakers/`), what every surface shows of a deferred retry, a
  released run's line and the one retry-now check (`retry_wait.rs`; the
  retry-now, wake-hold and expedite-actor types sit in the leaf module
  `resume_kind.rs`), the text apb writes for people (`people/`: `brief` the writing
  brief and where it applies, `instruction` the review and supervisor
  instructions with `gate_how_to_decide` for a person-only or bound gate,
  `link` a run's dashboard link), the run report as one
  self-contained HTML page (`report_html/`), run quality (`grade/`:
  `sources` every view of a run a grader reads, `reader` the bounded,
  redacting, traced reader, `evidence` the one validator every grader's
  output goes through, over `citation/`, generic over the view kind and
  shared with evidence sessions: `views` the views a citation points into
  (a `ViewSet` may mark records not citable and names its scope in a
  problem) and the `ReadState` that proves a record was read whole,
  `rules` the structural rules of one citation, `reader` the bounded
  reader both read through, `check` in `mod` the one entry with the
  absence rule; `rules/` the rules grader of the built-ins, `model`
  the decision-model grader of the playbook's own criteria, `session`
  a host's grading session, `store` the write-once grade files;
  `transcript/` the attempt transcript adapter, claude's session and
  codex's `exec --json` stdout parsed with tool facts, the other agents'
  streams as plain text; `candidate/`: the gate's choice and
  the trial view in `mod`, `settle` by successes, `quality` by quality with
  `resettle` after a stored grade and `decide_checked`, a person's decision
  that cannot promote without its check, `summary` the candidate summary
  (fitness, definition and quality kept apart, blocking and attention
  items, `summary_id`, `result_ref`) and `decide_by_person`, the one public
  promotion entry that re-checks `expected` and the acknowledgement under
  the candidate lock (`PromoteError`, `PromoteRefusal`); `run_history.rs`
  the earlier runs of a playbook the estimate and the comparison share,
  with `counts_as_history` the one filter: only production runs count),
  the redactor every text that
  leaves the machine goes through (`redact.rs`, over
  `apb_core::scrub::Scrubber`), the run-scoped redactor of a run's secret
  values and those of every run of its tree, ancestors, children and
  siblings (`run_redact.rs`: `RunRedactor`, `redact_text`
  for text that leaves the machine, `redact_json` for structured answers;
  a read-only process missing a referenced connector secret, or any
  process that cannot read a run, config or journal of the tree, withholds
  the run text as `[secrets_unresolved]`),
  the run directory's private modes and their narrowing (`run_perms.rs`),
  entering read-only mode with the decision client's request ban
  (`read_only.rs`), next actions (`next_action/`: `observation` one read of
  a run and its live children, `verdict` the waits and the one stop
  verdict, `deadlines` on the paused-interval clock, `types` the action
  shapes, `command` the one command-line renderer every surface prints,
  `unattended` what an unattended run would do at each wait (the
  `unattended` field of every next action), `caller_refusal` and
  `mark_for_caller` what a surface's channel may not take (a person-only
  gate, a step's own run),
  `start` and `start_action` the start actions of a refused start and of
  the fit report with their remedies), known problems (`known_problems/`: `observe`
  at a run's end, `at_start` what a starting run shows and journals once,
  `view`, `decide`, `recheck_one` and `scan` for the surfaces; `extract` the
  sightings of a journal, `source` a problem's current and old source text,
  `recheck` the model, host and person rechecks, `prompt` the attempt
  section and the supervisor line),
  the tree-scoped busy lock (`workdir.rs`), attempt failure
  classification and the bounded infrastructure backoff (`failure_class.rs`),
  the immutable write-once run manifest (`manifest.rs`, with `RunOrigin`:
  production, scenario, eval or unknown, absent read as production, and
  `RunOrigin::of_read`, where a manifest that does not read is unknown and
  so no history), scenario runs (`scenario/`: `BoundaryKind` the closed
  list of boundaries a scenario answers, `BoundaryKey`, `Session`, `at` and
  `RunsDir`, `guard::enter` the scenario mode plus the decision client's
  request ban at quick, `book` the answer book: attempts counted per visit
  and item, `usage` with the used, default, missing and extra answers,
  `session` the session registry by run directory, the resolution of every
  boundary kind and `child_prep` a nested child's scenario through the
  core loader, `decide` the scripted decision provider of a quick run,
  `adapter` the scripted agent behind the real attempt loop, `bind` the
  drive adapter (`drive`, `DriveRequest`, `Drive`, the book's use as the
  report's boundaries and keys), `report` the report built from the run
  with every expectation a named check and the trace, `store` `enter` over
  `guard::enter`, `DRIVE`, `execute`, `report.json`, the results index
  `index/<playbook>.jsonl` and its retention; the entry is
  `scheduler::run_scenario` over `prepare::Placement`, a child run lives
  next to its parent through `run_config::sibling_run`, and
  `DecisionRunner::for_scenario` is a quick run's decision runner), one
  judge evaluation (`judge_eval/`: `mod` the evaluation the judge node
  runs through, `evaluate_given` over given answers, `as_configured` what
  the node applies, `node_digest` over `apb_core::judge::node_digest`;
  `inputs` the inputs from a run's state, the one template render every
  prompt and judge state goes through, and `unfilled`, the references that
  rendered empty; `sandbox` one judge node evaluated outside any run, given
  answers at this machine's mode (`Answers::GivenAt`) or a live ask through
  `DecisionRunner::outside_run` under the fixed cap of 100 requests per
  project and UTC day counted on `use: judge_sandbox` lines (`SandboxLog`),
  the profile emulation never started; `example` "fill from a run"
  (`run_values`), `save_from_run`, `list` and `check` of labelled
  examples), the
  command runner of scripts, checks and project commands (`proc.rs`,
  `run_capture_in` the program-start guard site), the diagnostics sidecar (`diagnostics/`: `writer` the one
  writer that never fails a run, scrubs before it clips and drops a record
  rather than wait on its lock, `reader` the
  tolerant reader, `index` the dedup keys, `route` the `route_evaluated`
  records, `mismatch` the `definition_mismatch` records, and the
  `result_capture_failed` record of a snapshot not stored), execution, list
  visit and attempt identity from event order (`identity.rs`), the
  definitions a run executed and the segments they cover
  (`run_definitions.rs`, over the write-once content-addressed run files of
  `run_store.rs`), the execution identity of an attempt
  (`execution_identity/`: `mod` the `ExecStep` on `attempt_started` with
  `entered`, the reason its step was entered, and the binding snapshots
  under `runs/<id>/bindings/`; `close` the close reasons; `fold` every
  attempt's identity with the model each host task reported, credited to
  a model only when all name one, and `ExecutionIdentityView`, the row
  every run surface shows; `digest` the `x1:` executor digest and the
  estimate key), invocation resolution
  (`invocation.rs`), agent adapters (`adapter.rs`, with `adapter/command`
  the one `spawn_agent` both transports use, the sandbox wrap and `setsid`
  included, and `adapter/spawn_env`
  the one environment every agent spawn gets: the process-env plan, the
  connector scrub, the run context, the agent's run-scoped environment in
  the directory `agent_home.rs` names, the sandbox overlay last), the
  isolation sandbox (`sandbox/`: `probe` the cached host facts and the
  doctor line, `view` the pure filesystem view, `bwrap` its argv, `home`
  the sandbox home, `git_guard` the repository's read-only paths and the
  files that redirect git, put back after every attempt, `scope` the
  Landlock scope the hidden `apb
  __sandbox-exec` helper applies; the per-attempt place in
  `scheduler/node/sandbox`), the process-env plan of
  one agent step and its `agent_env_applied` event (`agent_env.rs`; the
  per-step wiring in `scheduler/node/step_env`), the per-run doctor
  (`run_doctor.rs` with the model and step rows, `run_doctor/env` the process env rows, `run_doctor/exposure`
  the tool exposure rows, `run_doctor/isolation` the isolation rows, its inline `results` module the result store's leftovers, `run_doctor/review` the channel entries no gate consumes (unstamped, another request or binding, not a person's on a person-only gate), `run_doctor/drift` the journal
  format drift: the binaries that drove the run, a downgrade, unknown,
  undecodable and torn lines, open deferrals and their guard; the binaries
  themselves in `run_binaries.rs`), the paid agent probe behind `apb doctor
  --probe-agent` (`agent_probe.rs`), the expected tools of an attempt and its
  `tool_exposure_mismatch` (`scheduler/node/exposure`), the append-only event log
  (`event.rs`, with `event/survey` the reader that classifies every line
  without failing and the guard state of its deferrals; `event/deferral_guard`
  the open deferred retries, the one closing rule the run state shares, and
  the guard written with every checkpoint while one is open, added on open
  when a crash left it out; `event/run_guard` the run guard: the latest
  `run_guard` names the features a run relies on and is written in the same
  write as every checkpoint while it names any, after the deferral guard;
  `scheduler/prepare.rs` writes it after `run_provenance`, `scheduler/patch.rs`
  before and after `patch_applied`, and a feature this binary does not know
  reads as an unknown event, so resume refuses and stop goes through),
  connector execution (`connector/`, with `call/` split into
  account selection, auth, encoding and response mapping, `attempt` the
  call's attempt binding, `target` the snapshot grant or the live
  trust-gated target a call resolves first, and `send_once` the send-once
  flow; `intent/` the
  send ledger: `identity` what makes two sends the same, `ledger` the fold
  and the admission table, `stamp` the key and the marker, `reconcile` the
  lookup of a marker send; `effect` what a call says about its effect), background
  supervisor spawn, legacy run-resume shim (`legacy_snapshot.rs`), the
  definition origin a run was started from (`run_origin.rs`), the launch
  check of a resume, one rule for every resume path, with its commit under
  the run's event lock and the pins of a run that had none
  (`resume_launch.rs`), the
  handoff file a checked launch crosses a detached spawn in
  (`launch_handoff.rs`: `Supervise`, `Eval` and `Resume`, private,
  single-use, written by the receiver's parent), the run
  policy gate every launch surface calls (`gate/`, with `billing` the
  billing half of the launch check: the predicted basis of every executor
  of the tree and the `billing_*` refusals, `challenge` the single-use
  nonce of a billing confirmation, `trust_scan` the
  connector and profile-bundle trust facts collected before any refusal,
  `isolation` the `isolation_unavailable` refusal, `unattended` the
  `unattended_unsupported` refusal of an unattended start in host or
  supervised mode and the run-time recheck of a gate's `decide`, `launch`
  the launch
  check: the explicit `Initiator`, `LaunchConfirmation` and `LaunchKind`,
  the decision table, `effective_for` and its refusal of a redirected config
  dir that hides stricter settings, the canonical permit digest and the
  nonce, the fixed choice of a retry, the `LaunchGrant` the engine rechecks, a run's
  authorization and `child_covered` for a reattached child, and `params` the
  `param_needs_value` refusal every launch surface checks after the gate,
  with the start warning about a declared secret value too short to scrub),
  the "will it fit here?" report (`fit/`: compatibility, `setup`, `trust` and
  consent kept apart, `report` the shapes, `problems` its blocking items and
  `unavailable` the report that could not be made), and the decision runner
  (`decision/`: the one entry point for decision-model uses, redaction,
  budget, replay, the `decision_made` event, the enforce gate, and one
  module per use: completion check, retry advice, supervisor wake triage,
  review recommendation, tier routing, `bounded` the shared ask over whole
  views the model grader and the recheck use, the judge node and edge answers
  (`scheduler/judge` runs them), `save_nudge` the save check's one yes or
  no question whether a request is recurring, `standalone` the decider and
  `DecisionLog`, the `decisions.jsonl` reader, lock and counter every
  daily-capped use shares, and `report/`: the labellers, statistics
  and replay behind `apb decisions`; see `docs/DECISIONS.md`).
- `apb-mcp` - rmcp stdio MCP server (`server/`, with `read_only` the
  allowlist of `apb mcp --read-only` and the tables every tool is classified
  in) and the tool layer in `tools/`
  (one module per domain: `playbook`, `run`, `supervisor`, `trial`, `capture`,
  `actions` the next actions and start actions as tool calls with
  `available_to_caller`, `caller_refusal` and `mcp_channel` (the channel of
  a decision through MCP, a run step's inside one),
  `meta`, `intent` for `run_intent_resolve` and `apb intent`, `budget` for the
  budget arguments, the pre-start decision and the estimate and budget blocks
  the HTTP API reuses, `memory` for `playbook_memory` and the `remember` of a
  start, shared with the HTTP API, `notify` for the notify argument, the
  `notify_unavailable` start check, a live change and the run's `notify`
  block, shared with the HTTP API and the CLI, `grade` for grading a run,
  its grades and the `grades` block of `run_status`, where every stored
  grade re-decides the candidate (`after_stored`), `candidate` for the
  candidate comparison, its summary and a person's decision (a refusal
  typed by its code), `judge` for the judge sandbox and labelled examples
  (`JudgeCaller`: given answers anywhere, a live ask, a live check and a
  save never from a run step or a dashboard without keys), `scenario` for
  scenario runs on every surface (`scenarios`, `run_scenario`,
  `scenario_run_view`, and `play`, the bounded `apb __scenario-run` child
  `apb eval --level` uses too; a smoke run needs a cap and an approved
  version and is refused from a run step and a dashboard without keys),
  `problems` for a
  playbook's known problems, a person's decision, a recheck and a scan,
  `results` for a run's result snapshots (`run_results`, `run_result_read`,
  the raw and live reads and `apb results clean`), `evidence` for
  `run_evidence_read` and `apb evidence read` (a session found by id, a
  step reading only its own run's),
  and `discover` for history discovery, its stored results, a save
  through `capture::capture_draft` and a decline, all shared with the HTTP
  API and the CLI); `promote/` is `playbook_make_global` (the
  plan and its blockers in `plan`, the apply in `mod`); `catalog_rank` is the
  opt-in decision-model ranking behind `playbook_catalog(query)`, asked through the engine's standalone decider
  (`decision/standalone.rs`). `setup/` is agent-first setup (`plan` the
  read-only plan with its digest over the actions and their observed
  state, `apply` the run of exactly that plan under the setup lock,
  `remove` the undo (a replaced default given back to the person's copy),
  `record` the setup record, host config backups under `<config_dir>/setup`
  and the setup lock with its lifetime, `hosts` the MCP registration of
  every agent that is an MCP host (a symlinked config edited in place with
  its mode, written only while it still holds what was read, a host CLI
  stopped after its time limit, a shape apb cannot edit safely a `manual`
  step), `hook` the end-of-turn save check registered the same way in
  every agent with a Stop hook (`register_hook`), `standing` the
  project's standing playbook block shared with `apb init` (every target
  read before the first write, earlier writes put back on a failure; a
  link inside the project written through, a link out of it refused), `observe` the
  work trackers and optional review helpers the plan reports for
  information only, `doctor` the defaults and reviewer lines of `apb
  doctor`, `upgrade` the reconcile of the installed defaults once setup
  was applied, run at `apb mcp` start and by `apb self-update`); apply and
  remove are refused inside a run step (`step_refusal`), the read-only
  plan is not, and a changed default
  is replaced only through the plan's `--replace-modified` actions; the MCP tools `setup_plan` and `setup_howto`
  (`docs/AGENT-SETUP.md`) sit over it, applying is CLI only.
- `apb-cli` - package name `apb` (bin `apb`, `main.rs`); thin dispatch over
  core/engine/mcp. `apb init` runs a short interactive questionnaire in a
  terminal (feedback-loop consent into CLAUDE.md/AGENTS.md, agent
  subscriptions survey); non-TTY runs skip it, and its flags
  (`--standing-block`, `--feedback-loop`, `--subscriptions`, `--json`)
  answer without a terminal, except `--feedback-loop yes`, which only a
  person gives at a terminal (refused with 3 otherwise and inside a run
  step). `setup.rs` is `apb setup plan|apply|remove`
  over `apb_mcp::setup`. `hook.rs` is `apb hook stop`, Claude Code's Stop
  hook over `apb_core::save_nudge`, which prints the host's answer or
  nothing and always exits 0. `apb self-update` updates
  installer-based installs in place, then runs the hidden `apb
  __reconcile-defaults` in the updated binary. `doctor.rs` is `apb doctor` (the
  environment with the defaults and reviewer lines, `--agents`, `--run
  <id>`, `--probe-agent <id>`, `--json`). `apb intent list|resolve` shows a run's
  send intents and records a person's decision on an unknown one. `budget.rs`
  holds the budget flags of `apb run` and `apb resume`, the estimate printing
  and the terminal question of `on_no_estimate: ask`. `memory.rs` is `apb
  memory`, `make_global.rs` is `apb make-global`, `version.rs` is `apb
  version list|use|history`, `publish.rs` is `apb
  publish show|approve|skip` (thin wrappers over the review channel; a
  person-only gate takes them only at a terminal after the typed
  confirmation of `consent::ask_typed`, else as `cli_unattended` or
  `run_step`), `import_preview.rs` prints
  the preview `apb import` shows before it asks for acceptance. `notify.rs`
  is `apb notify` and the notify flags of `apb run` (forwarded to the
  supervised drive like the budget flags), `report.rs` is `apb runs <id>
  --html`, `grade.rs` is `apb grade` and the grade lines of `apb runs
  <id>`, `candidate.rs` is `apb candidate` (the summary and the exact
  `--promote --expect <v>@<digest> [--ack <id>]` line), `judge.rs` is `apb
  judge eval|examples|save-example|values`, `demo.rs` is `apb demo`
  (`--run` starts the real `apb run demo-report --detach`), `problems.rs` is `apb
  problems`, `discover.rs` is `apb discover`, `results.rs` is `apb results`
  (`show`, `clean`, the evidence claims) and the results lines of `apb runs <id>`,
  `evidence.rs` is `apb evidence read`, `actions.rs` prints next
  actions and start actions (`apb runs <id>`, `apb wait`, `--follow-auto`),
  `launch.rs` the `--confirm-launch` flags of `apb run`, `apb resume` and
  `apb eval`, the terminal question and `apb launch floor`,
  `pipe.rs` the panic hook that ends the binary quietly on a closed stdout.
  `eval.rs` is `apb eval` and holds `EvalLevel` (`deep`, `quick`, `smoke`);
  `scenario.rs` is `apb eval --level quick|smoke` (the version's validator
  errors refused first, the smoke checks, one bounded `apb __scenario-run`
  child per scenario through `apb_mcp::tools::scenario::play`) and `apb
  eval --scenarios`.
- `apb-server` - axum web API: a thin router in `lib.rs`, one module per
  resource in `routes/` (`estimate` for the estimate route and the run and
  resume budgets, `fit` for the fit report (`?version=` for one version),
  `graph` for the editor's
  draft validation and the overlay of a run on a version, `memory` for remembered run
  input, `notify` for the machine's notify target and a live run's change,
  `current` for moving a playbook's `current` and its history,
  `quality` for a run's grades, a playbook's candidate, its summary and a
  promotion that names what it saw, `judge` for the judge sandbox and
  labelled examples, `scenario` for scenario runs, `demo` for the keyless
  demo, `problems` for
  a playbook's known problems, `results` for a run's result snapshots
  (the listing, one entry, the raw download and the live comparison),
  `discover` for history discovery, loopback
  only; `auth.rs` inserts the `RequestChannel` (keyless, bearer, session)
  a decision route maps to its channel; `state.rs` also holds the tool-layer
  answer and refusal mapping they share (`ToolError::Refused` is 403);
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
  (`StepCaller`). A person's promotion names the version and digest the
  person saw and, while the candidate summary holds attention items, its
  `summary_id`; both are checked again under the candidate lock
  (`candidate::summary::decide_by_person`), and a validator error or an
  edited candidate blocks it. Automatic promotion never reads the summary. The model grader (`model/1`) grades the playbook's own
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
- **Result snapshots** (`docs/RESULTS.md`): a node that declares
  `results:` keeps a snapshot at every settlement of its work (`succeeded`,
  `failed`, `timed_out`; a cache hit after its restore, the finish answer,
  a list item's full output; write-offs never): the output, the declared
  fields and, on success only, the declared files, as UTF-8 text scrubbed
  by `RunRedactor::secret_text` and stored content-addressed under
  `runs/<id>/results/`. Sources are read through `fsutil::beneath` (no
  link, FIFO or hard-linked file opened; secret-shaped names never opened),
  every limit stops work and marks the snapshot `incomplete`, and a
  redactor that cannot resolve fails closed (`scrub_unavailable`). The
  stamp (`node_finished.result`, `list_item_settled.result`) is journaled
  before the transaction ends; nothing reads a snapshot no stamp names,
  every read re-hashes manifest and object (`corrupt` otherwise) and passes
  the run redactor, ranges counting in the served text. A failed capture
  never fails the node (`result_capture_failed` in the diagnostics, listed
  by every view as `not_stored`); a live comparison reads the tree capture
  read, never an attempt's own `workdir`. `apb results clean` resolves dead transactions by the journal and
  removes only what nothing names; `apb doctor --run` only reports.
  `artifacts_committed.attempts` records each attempt's head, so commit
  rows say which attempt made a commit. All new fields are
  `serde(default)` and display or storage only.
- **Evidence for node outputs** (spec
  `docs/superpowers/specs/2026-11-03-evidence-citations-design.md`): an
  `agent_task` with `evidence: { from, kinds?, claims?, on_invalid?,
  budget_chars? }` gets one reading session per attempt, opened before
  the attempt over the latest stamped snapshots of its `from` nodes, named
  in a `## evidence` prompt section and `APB_EVIDENCE_SESSION`, read with
  `apb evidence read` or MCP `run_evidence_read` (removed in read-only
  mode) and closed when the attempt ends. A `format: records` entry is a
  package served record by record (`r:<snapshot>/<entry>@<id>`). The
  claims of a succeeded attempt (`outputs.claims`) are checked by the
  grader's citation rules: `structurally_valid`, `unsupported`,
  `absence_ok`, `not_enough_data`, with flags worth a look; `on_invalid:
  fail` fails the attempt as `check_rejected` and the retry is told the
  problems. The session file keeps references and counts only; a record
  the run redactor changes is served redacted and never citable. Surfaces
  say "Structurally valid ... Whether it supports the claim was not
  checked.", never "confirmed". A run using it carries a run guard naming
  `evidence`.
- **Approvals and person-only gates** (spec
  `docs/superpowers/specs/2026-11-03-approvals-design.md`): a
  `human_review` with `approves: { results }` binds its request to the
  latest stamped snapshots of those nodes (`review_requested.binding`), and
  an approving decision carries the binding nonce in the field a publish
  consent uses; the drive consumes only an entry stamped with the open
  request and re-verifies every bound snapshot first. A node with
  `requires_approval: { gate, of?, live }` is re-checked at admission and
  again as each execution begins (`node::execute_node_kind`: a resume, a
  deferred retry, a list item) (`approval_missing`, `approval_stale`,
  `approval_snapshot_unreadable`, `approval_incomplete`,
  `approval_unverifiable`) and finishes failed with no attempt when
  refused; a publish node adds `approval_mismatch` (also when the approved
  snapshot recorded no commit) and the plan digest line
  `approval=<nonce>`. An approval is a fact about stored
  bytes and never a consent. A gate marked `person_only` takes a decision
  only through `terminal` or `dashboard_session` (a scenario run's script
  in a scenario run); every surface states its channel, MCP and run steps
  are refused (`ChannelRefused`, MCP tool error, HTTP 403), and a run step
  never decides or answers for its own run tree (siblings included; a tree
  that does not read refuses) on any gate. A server key signs in, so it is
  a person's credential and the bearer refusal guards against mistakes
  only. A supervisor patch, in-run or forward, never weakens these fields,
  and an in-run one never re-points such a gate's options
  (`approval_weakened`, `scheduler::approval::patch_refusal`). Every move
  to a named node (a patch's `continue_from`, `node_retry`,
  `run_continue_from`, a resume's `from_node`) goes through one check,
  `scheduler::approval::skips_gate`: a gate is passed only by its latest decision,
  passing (approving on the current binding, or not `reject`), routed
  toward the target and of the current visit; a refused directive is
  journaled `directive_refused`, and only a person's resume (terminal,
  `dashboard_session`) goes past (`refuse_resume_skip`). An unattended
  or enforced decision on a bound gate is stamped with the nonce and
  satisfies `requires_approval`, labelled by `decided_by`. A run that uses
  any of these carries the run guard, so an older apb refuses to continue
  it; an older apb that starts such a playbook drops the fields, and one
  that stopped such a run can resume it past its gates (its stop leaves no
  guard), which is documented, not prevented.

- **Edge addresses and route facts** (spec
  `docs/superpowers/specs/2026-11-01-edge-identity-design.md`): every edge
  has an address in its definition (`id:<id>` or `e:<from>><to>#<n>`,
  `apb_core::edge_address`), carried by validation issues (`Issue.edges`),
  the version diff's correspondence, `edge_traversed.edges` and the judge
  edges' `decision_made.edge_addresses` (display only; routing never reads
  them) and the editor. Diagnostics live in `runs/<id>/diagnostics.jsonl`,
  never in the journal, and never steer a run (an equal-journals test
  guards it); execution code only hands the writer a finished value.
  Every drive journals the digest of the `playbook.yaml` it executes
  (`run_started.definition`, `run_resumed.definition`,
  `patch_applied.definition`) and stores those bytes under
  `runs/<id>/definitions/<sha256>.yaml`; a segment is `Known` only when its
  own journaled digest matches stored bytes, never on the sidecar. A resume
  that finds a `playbook.yaml` no journaled digest accounts for records
  `definition_mismatch` as detail and executes it as before; a detached
  driver refuses one. A patch that would move a judge-edge answer routing
  has not used yet is refused unless `continue_from` re-runs its source.

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
- The **launch policy** (`docs/GUARDRAILS.md` "Launch policy for agents"):
  a playbook's `agent_launch: allow | ask | deny`, held up by its launch
  floor and narrowed by the `launch:` sections of `policy.yaml`, is decided
  in that one gate for every start, trial, plan execution, eval and resume
  of a run never authorized, with an explicit initiator (a run step is
  never a person; the CLI is a person only through a terminal question or a
  nonce) ([decision record 0011](docs/adr/0011-an-agent-launches-only-what-the-launch-policy-allows.md)).
  An `ask` refusal carries a nonce over the canonical permit and the fixed
  version choice; the grant is rechecked by the engine before it writes
  `manifest.launch` or `RunResumed.launch`, and that record covers the run
  for its lifetime. Every version publication raises the floor first under
  the floor lock; an agent channel's loosening save is refused
  (`launch_policy_loosened`); only a lowering a person confirmed in the
  editor of a dashboard with keys and `apb launch floor --lower-to` at a
  terminal lower it. A dashboard without keys is decided like the CLI
  without a terminal and never lowers a floor. A run step or a CLI without a
  terminal whose config dir is redirected away from the default one is
  refused (`policy_file_invalid`) when that hides stricter settings (an
  eval case keeps its handed config dir), and a workspace id keys a floor
  or an exact machine entry only when registered at that root. A decision
  crosses a detached spawn only in a handoff file in the project's
  `.apb/handoff`, never on argv.
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
- apb's own git commands go through `apb_core::git` (hooks, the filesystem
  monitor and signature programs off, `GIT_*` removed), so they never run a
  program the tree names
  ([decision record 0009](docs/adr/0009-apb-git-never-runs-a-program-the-tree-names.md)).
- The **publish step** (`docs/PUBLISH.md`): a push leaves a run only
  through a `publish` node, after a person consented to the exact plan, and
  a push whose result is unknown is never repeated on its own
  ([decision record 0010](docs/adr/0010-a-push-leaves-only-through-the-publish-step.md)).
  The remote is pinned before the first acting node; the plan is read-only;
  the consent reuses the review channel, bound to the plan by a nonce
  (`apb_engine::review::post_decision`, one check for every surface); the
  push is recorded in the send ledger (`@git` intent, mode `ref`) before it
  leaves, runs through `apb_core::git::transport` from a fresh shadow git
  directory per call, and an unknown result is reconciled read-only or
  resolved by a person (`apb intent resolve`). A dashboard without keys
  refuses a publish consent; `publish_only` holds sub-playbook runs too. A publish node implies the effects
  `network`, `external` and `irreversible` (`NodeKind::implied_effects`).
- **Isolation** (`docs/ISOLATION.md`): a node's `isolation` is a contract
  apb checks before every spawn
  ([decision record 0008](docs/adr/0008-an-isolation-level-is-a-contract-checked-before-every-spawn.md)).
  `apb_core::isolation::contract::evaluate` is the one answer the gate,
  prepare, every attempt, fit and the doctor share: `full` refuses
  (`isolation_unavailable`) what this machine cannot enforce, `best_effort`
  runs without a sandbox and journals why, `none` is untouched. On Linux
  the sandbox is unprivileged bubblewrap plus the `apb __sandbox-exec`
  helper; macOS is not supported. Every isolated attempt journals
  `attempt_started.isolation` (holds, limits, view), shown by `apb runs
  <id>`, `apb doctor --run`, `run_status` and the run page. An isolated
  node always gets the env allowlist, and V113 refuses it with connectors.
- schema 2 removed the old `executors` block; playbooks bind executors only
  through profiles. `apb migrate` converts legacy schema 1 playbooks.
- The supervisor is an optional agent that watches a run and intervenes via the
  `supervisor_*` MCP tools; its binding exists in the manifest only when the run
  is actually externally supervised. `supervisor_interrupt_attempt` takes an
  optional `node`: targeted at one attempt, or a whole-run broadcast when omitted.
- **Billed use** (GUARDRAILS.md "Billed use",
  [decision record 0012](docs/adr/0012-billed-agent-work-starts-only-on-a-persons-command.md)):
  the `billing:` section of `policy.yaml` (`billed`, `unknown_basis`:
  `allow | ask | deny`, default `ask`; `budget_counts_subscription`,
  default `true`) is decided in the launch check on the basis predicted
  for every executor the run binds (the supervisor on the agent's own
  settings); an `ask` refusal carries a single-use challenge
  (`gate::challenge`, one start per confirmation) over the launch and the
  predicted set, looked at by the check and consumed only where the start
  commits (`LaunchGrant::consume_challenge`), and the run keeps
  `launch.billing`. A settings file that does not read denies
  (`EffectiveBilling::load`). Every spawn, the supervisor and the quota
  probe included, reads the basis again (`pause::billing::spawn_hold`);
  uncovered work pauses the run and only a person's resume adds it (a held
  supervisor is journaled and pauses the run, `hold_for_supervisor`). Tests set `APB_TEST_BILLING_DEFAULT=allow`
  (a `testing` seam) so stand-in agents of unknown basis are not held;
  the `testing` features of apb-core, apb-engine and apb-decide are a
  compile error without debug assertions, so no release binary has a seam.
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
  (`notify::set` with `ChangedBy::Host`), outcome filter included. Which
  playbooks and outcomes a default subscription covers comes from
  `policy.yaml` (`apb_core::policy_file`); a filtered `finished` is
  journaled as `finished:<outcome>` names, which older readers ignore.
  `host_task` and `supervisor` are opt-in transitions; their waits
  (`scheduler/host.rs`, `scheduler/supervisor.rs` `await_control`) run the
  same throttled sweep, and a notice's command comes only from
  `next_action::command::render` for the notified request.
- **Moving `current`** goes through `apb_core::versioning::current`
  (`move_current`, or `move_current_locked` under the candidate lock a
  settlement already holds): an intent file, ordered writes, recovery,
  and a history line chained by hash; a candidate's own promotion writes
  its trial record inside the operation. No other code writes `current`.
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
`dist-workspace.toml` (and the referenced `.github/build-setup.yml` and
`.github/workflows/test-gate.yml`), then re-run `dist generate` and commit the regenerated workflow alongside the
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
published release body, on releases and pre-releases alike, through
`.github/workflows/release-notes.yml`. That workflow is not part of the
generated file (dist's post-announce jobs follow the Homebrew publish, which
is skipped on a pre-release, so `dist-workspace.toml` has none): it runs on
`workflow_run` of the Release workflow and, before any checkout or edit,
requires a successful `push` run of this repository whose ref is a tag
`vX.Y.Z` or `vX.Y.Z-pre` that dereferences to the run's head commit. It then
checks out that commit without credentials and runs `gh release edit` with
the notes file, failing when the file is missing. As a `workflow_run`
workflow it runs from the default branch, so a change to it takes effect
once merged. `release_workflow_test` covers its trigger, permissions and
checks, with a pre-release run that passes and a pull request run that does
not.

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
- **Authorized product development** means a task the owner assigned for
  delivery. For such a task, commits on a local feature branch are permitted;
  never commit on the shared main branch.
- Once the change passes every gate in this section and its review has
  converged, push the branch and create or update its pull request without
  asking. The review is the cycle the review skill in use defines, or else
  the project's review process. It has converged when the cycle ended with no
  open blocking finding and every finding has a recorded disposition (fixed,
  or rejected with a reason).
- Merging follows the policy the owner sets per project. For this repository:
  ask before merging, unless the owner pre-approved the series the change
  belongs to.
- Research or design-only tasks stay local and uncommitted unless the owner
  promotes them to delivery.
- Unrelated uploads, deployments, package publication and file sharing still
  require explicit authorization. A product-delivery push/PR grant is not a
  blanket permission to send other files or change infrastructure.

## Daily cleanup

On the first agent session of the day in this repository, check the stamp
`${XDG_CACHE_HOME:-$HOME/.cache}/apb-cleanup/last-run`. When it does not hold
today's date (`date +%F`), run `scripts/daily-cleanup.sh` before other work
(`--dry-run` shows what it would remove, `--force` runs it regardless of the
stamp). It removes worktrees whose branch is merged into the default branch
and that hold no uncommitted change, sweeps cargo artifacts older than 7 days
(`cargo sweep`, never a full clean), removes empty leaked test config dirs and
this project's logs older than 7 days, then writes the stamp. Besides that,
remove a task's worktrees and their build output as soon as its pull request
merges, delete only paths you created, and stop only processes you started.

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
