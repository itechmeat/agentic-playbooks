# Design: incremental lists and session fork

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is today, all read on main
efae5822 (1.0.0-beta.7), where `typed-inbox-reads` (#287),
`people-in-the-loop` (#288), `before-running-alone` (#289) and
`wait-park` (#290) are merged. If a line moved, find the site by the
name given next to it, never by the number alone.

This change is implemented after `eval-doubles` and `inbox-waits` are
merged. Neither is needed by anything here; both touch some of the same
files (D0 "Files shared with eval-doubles and inbox-waits"). Where one of
them adds an entry to the same list (the `GuardedFeature::ALL` array,
the known-events fixture, the exhaustive arm of the run-state fold),
this change adds its own entry after theirs.

Two decisions, built in the order D0, D3, D2 (tasks.md): the fork (D3)
is independent and small; the list skip (D2) reads one earlier run and
journals one bounded set per list node.

One principle runs through both: an existing engine path is reused with
other arguments instead of a protocol of its own, and whatever this
change does not name is unchanged from main.

Nothing here is left to the implementer. Every choice is decided below
with its reason (Review record: the closure pass, items 1 to 24, the
second pass, items 25 to 38, its follow-up decisions, items 39 to 41,
the third pass, items 42 to 51, the fourth pass, items 52 and 53, the fifth pass, items 54 to 57, the sixth
pass, items 58 and 59, the seventh pass, items 60 to 63, the eighth
pass, items 64 to 75, the ninth pass, items 76 to 81, the tenth
pass, items 82 to 86, a follow-up decision, item 87, and the eleventh
pass, items 88 to 91; "Cut in the second pass" lists what
the second pass removed); tasks.md only orders the work.

## D0. Shared pieces

### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V283 | error | D2 | a malformed `list.skip` block |
| V288 | error | D2 | `list.skip` on a node with an isolation other than `none` |
| V289 | error | D2 | `list.skip` on an agent list whose prompt reads a run value other than a param or `run.instruction` |

The block: one code, V283, inside the V280 to V299 family. Checked on
efae5822: `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u` prints V200,
V210, V211, V212, V220, V221, V222, V240 to V247, V249 and V250. The
two changes merged before this one use V280 and V281 (`eval-doubles`)
and V284 to V286 (`inbox-waits`); `eval-doubles` keeps V282 reserved and
unused, both name V283 as this change's, and both keep V287 to V299
reserved for the family. V248 and V251 to V259 stay reserved for the
people-in-the-loop family, V260 to V279 for `start-rules-and-schedules`.
The fork needs no code: what it can check statically is V44, V45 and
V85, which read the source through `.from()` (D3).

V283 joins the list rules in `crates/apb-core/src/validate/list.rs`
(`check_lists`, line 17, called at `validate/mod.rs` line 436 with the
comment `// V85-V89, V93-V95`, which becomes `// V85-V89, V93-V95,
V283`). The module doc line 1 names V283 too.

V288 (error, Fix round 7) joins them in `check_lists`
(`check_skip_isolation`): `list.skip` on a node whose `isolation` is
`full` or `best_effort` ("skip on isolated nodes is not supported
yet"). An isolated item may run in a sandbox's or a fresh scratch
directory the work does not represent, so the narrowing replaces
representing it. It is the first code of the V280 to V299 family past
the ones other changes hold or keep reserved: V280 and V281
`eval-doubles`, V282 reserved unused, V283 this change, V284 to V286
`inbox-waits`, V287 reserved; V289 (Fix round 10, below) is this
change's too, and V290 to V299 stay reserved; main 3248c77f takes V300 to V302, V320
and V340, `mcp-http-and-surfaces` V303, `start-rules` V260 to V279,
`one-point-oh-freeze` V321 to V339. Since validation refuses it at
`apb validate`, at every save and at run start (`scheduler/prepare.rs`),
the run side adds only one line where the stamp is minted
(`ListRun::minted_digest`: an isolated execution never stamps).

V289 (error, Fix round 10) joins them in `check_lists`
(`check_skip_reads`): `list.skip` on an `agent_task` list whose prompt
reads a template value other than `params.*` and `run.instruction` (an
earlier node's output, `run.context`, `run.id`, `run.retro`,
`run.active_elapsed_seconds`, `run.hooks.*`, `run.previous_success.*`).
The skip digest compares what apb configures, not what a run produces
(below, "Outside it" item 2), so such a read would leave an item skipped
that its prompt now asks something else of; the narrowing refuses it
instead of representing rendered values. A value an item depends on is
passed as an item field, which the skip compares per item.

### Journal entries

| Entry | Kind | Decision | Checkpoint |
|---|---|---|---|
| `list_skip_read { node, set }` | new variant, one per list node with `skip` | D2 | no; safe to skip up to the next checkpoint (below) |
| `list_item_settled.skipped_from`, `list_item_settled.skip_digest` | additive fields of a known event | D2 | |
| `list_started.skip_refused` | additive field of a known event, written only for a list with `skip` whose visit skips nothing | D2 | |
| `session_handoff.mode` | additive field of a known event | D3 | |

Every new field is `#[serde(default, skip_serializing_if = ...)]`
(decision record 0001), and every new value is written only for a list
that declares `skip` or a node that forks, so a journal of a run that
uses neither feature is byte for byte what main writes. The list
output's `skipped` count is one of those values: `compose` writes it
only for a list that declares `skip` (D2 "Output"), because the output
is stored in `node_finished` (`scheduler/list/mod.rs` line 388, the
`output` of `NodeFinished` in `event.rs`). The test
`a_plain_list_run_journals_what_main_journals` (D2 "Tests") compares
a plain list run's journal with a fixture recorded on main before any
code of this change.

`list_skip_read` is safe to skip: a run's lines are written in one
journal write (`EventLog::append_group_decided`, D2) before the first node of
the run starts (at start after the `run_guard` line, the set of a start
that took a concurrency slot at once read right after the slot was
admitted; for a queued run, after its turn; for a run that journaled none
before a drive, at that drive's entry), so a checkpoint (the first
`node_finished`, or a `run_paused` or `run_aborted`) follows it before
any driver continues; a run that has it always carries the `list_skip`
guard (below), so an apb that knows run guards but not this name refuses
to continue it. An older apb's strict reader refuses the journal before that
checkpoint (the unknown `list_skip_read`) and after it (the run guard
that follows every checkpoint names `list_skip`), so it never pauses
such a run; it reads and stops it (D0 "What an older apb does").

`EventPayload::is_checkpoint` (`crates/apb-engine/src/event.rs` line
3820) is a `matches!` over the five checkpoint variants: it does not
change. The run-state fold (`crates/apb-engine/src/state.rs` lines 835
to 839, the one exhaustive `EventPayload` match) gets `ListSkipRead {
.. }` in the list no-op arm (`ListStarted | ListBatchStarted |
ListItemSettled | ListPilot | ListStopped`, after `ListStopped`), since
`list::skip::recorded` reads the set, not the run state.

`crates/apb-engine/tests/fixtures/journal/known_events.jsonl` (the
byte-exact round trip of `tests/suite/journal_compat_test.rs`, line 15;
29 lines on efae5822, seq 0 to 28) gets four lines appended after its
last line on merged main, with the next seq numbers (`inbox-waits`
appends its own line first): one `list_skip_read`, one
`list_item_settled` with `carried`, `skipped_from` and `skip_digest`,
one `session_handoff` with `mode`, and one `list_started` with
`skip_refused`. Each is written exactly as
`serde_json::to_string` writes it (field order of the struct).

### Run guard

`crates/apb-core/src/validate/guarded.rs`: `GuardedFeature` (line 31)
gets `ListSkip`, the last entry of `ALL` (line 81), `as_str`
`"list_skip"`, doc line "A list node skips items that passed in earlier
runs (`list.skip`); apb 0.25.0, which predates list mode, drops the whole
list block and runs the step once, skipping nothing and repeating its
outside actions."
On efae5822 `ALL` has 13 entries (`wait_park` last); `eval-doubles`
makes it 14 (`eval_doubles` last); `inbox-waits` adds none. This change
makes it 15, `list_skip` last. The two count assertions
(`a_deadline_is_a_guarded_feature`, line 213, and
`all_has_fourteen_entries_in_order`, line 383, as `eval-doubles`
leaves them) move to 15; the latter's index assertions gain
`ALL[14] == ListSkip`, and its name and doc line say fifteen.

`guarded_features` (line 127) names `ListSkip` for a playbook with any
node whose `list()` has `skip`, next to the `ListPilot` rule (line 159).
`run_guard_of` (`crates/apb-engine/src/scheduler/prepare.rs` line 1995)
does not change: it keeps every definition feature but `Deadline` and
`Concurrency`. A supervisor patch recomputes the guard through
`run_guard` (line 1959) as today.

The fork carries no guard name: an apb that does not know the object
form of `continue_session` cannot parse the definition at all
(`Option<String>` refuses a map, in every released binary), so it never
starts the playbook and never continues the run with the fork dropped
(D0 "What an older apb does").

### Run guard names after this change

`approval_binding`, `person_only`, `required_approval`, `evidence`,
`write_scope`, `previous_success`, `untrusted_input`, `deadline`,
`concurrency`, `review_fields`, `review_max_scope`, `list_pilot`,
`wait_park` (main), `eval_doubles` (`eval-doubles`), `list_skip` (this
change), in this order.

### What an older apb does

| Binary | A definition with `list.skip` | A definition with a fork | A run of either, before its first checkpoint after `list_skip_read` | A run of either, after it |
|---|---|---|---|---|
| this change and later | runs it | runs it | drives it | drives it |
| 1.0.0-beta.1 up to this change | refuses at parse (the list block denies unknown fields): starts nothing | refuses at parse | reads and stops a skip run; refuses to pause or resume it (the unknown event no checkpoint follows) | reads and stops a skip run; refuses to pause or resume it (the guard that follows every checkpoint), and a fork run's definition does not parse |
| 0.25.0 | accepts unknown list fields; it predates list mode, so it drops the whole list block and runs the step once, skipping nothing | refuses at parse | reads and stops a skip run; refuses to pause or resume it (the same rule) | reads and stops a skip run; refuses to pause or resume it (the guard) |
| older than 0.23 | as 0.25.0 | refuses at parse | not covered by the corpus | does not know run guards |

The pause columns are the existing unknown-event rule, not a promise of
this change: `pause::request_pause` reads the journal with the strict
`read_all` under the run's event lock (`crates/apb-engine/src/pause/request.rs`
lines 73 and 74), and `read_all` refuses an unknown event that no
checkpoint follows (`event.rs` `check_unknown_settled`, line 3974).
Before the first checkpoint that event is `list_skip_read`; after it,
every checkpoint is followed in the same write by the run guard
(`event.rs` line 3090), whose `list_skip` name an older apb reads as an
unknown event, so an older apb refuses to pause a skip run at any time,
and the guard ordering is not changed.
Stopping reads through `read_all_for_stop` (line 3939), which only
warns, so an older apb stops such a run at any time. A fork run has no
new event type, so only its definition stops an older apb.

The 0.25.0 row is the one the guard exists for, as for `list_pilot`
(`docs/GUARDRAILS.md` line 276): such a binary would start the playbook
and repeat every item's outside actions. The journal corpus proves the
rows for the two released readers (`crates/apb-cli/tests/suite/journal_corpus_test.rs`,
rows L1 and F1, D2 and D3 "Tests"), the immediate window included, and
runs in the gate with the old binaries required (`APB_CORPUS_REQUIRED=1`
with `APB_OLD_BINARIES`, the way CI's `journal-corpus` job runs it,
`.github/workflows/ci.yml` lines 182 to 258): a missing binary fails the
gate, never passes it. The playbook format still has no way to demand a
minimum apb; the machine holds the guarantee, as the existing paragraph
of `docs/GUARDRAILS.md` says.

### What stays unchanged from main

This change reuses the engine's paths with other arguments and adds no
hand-over, control, waker or recovery protocol of its own:

1. The history lookup: the skip reads exactly the one run
   `run.previous_success` resolves, through `previous_success::find`
   (`crates/apb-engine/src/previous_success.rs` line 33) with its
   eligibility (created here, same playbook and scope, production, a
   top-level run, settled succeeded), its external-data rule
   (`external_data::learns_from`, line 106) and its refusals, or the run
   the window already recorded (D2 "Reading the set"). `previous_success.rs`
   does not change.
2. The read-once points: the set is read in preparation next to the
   window (`scheduler/prepare.rs`, after the key resolution of lines 1129
   to 1142, before `allocate_run`) for a start without a concurrency
   slot, and by the same `after_the_turn` (`scheduler/entry.rs` line
   334) under the same "already journaled" rule for a queued run; the
   same function records them at the entry of a drive that finds none
   (D2 "Where the set is read and journaled").
3. The grouped write: `append_pair` (`event.rs` line 2955) is generalized
   into `append_group_decided` over the same `append_lines`, and the form gate's
   call keeps its behaviour.
4. The patch refusal: a patch that adds a skip the start never read is
   refused like a patch that adds an unrecorded `run.previous_success`
   read (`previous_success::patch_refusal`, line 363, called at
   `scheduler/patch.rs` line 116); the new refusal sits next to that call.
5. The visit's opening and its crash recovery: the skip is one more
   opening step inside `opening_steps` (`scheduler/list/mod.rs` line
   171); `complete_opening` (line 199) recovers it with no change, as
   it recovers `carry_passed` and `park_unknown_sends`.
6. The settlement: a skipped item is settled through `ListRun::settle`
   (`scheduler/list/run.rs` line 115) with `carried: true` and no step
   output, like a carried item.
7. The node cache key: `scheduler/cache.rs` `prepare` and every cache
   record are unchanged; the skip's work fingerprint lives beside it and
   shares no code path with it (D2 "The skip digest"). It reuses the
   journaled definition, `run_scripts::digest`, the manifest's recorded
   profiles, execution block and account digests, the decision runner's
   effective settings and `run_origin::tag_if_keyed` as they are.
8. The pilot slice (`scheduler/list/pilot.rs` `step`, line 32) and the
   batch queue (`batch.rs` line 125) take `fold.remaining`, so a
   skipped item is never in a slice or a batch: no change.
9. The early stop: `stop_when_holds` (`batch.rs` line 204) and
   `counts_excluding` keep their rule; only the excluded ids they are
   handed grow (D2 "Counts").
10. The goal criterion `{ type: list }` (`list_criterion`, `ledger.rs`
   line 470), readiness, the pilot gate, the budget gate, the review
   channel, the wait park, the deadline, the concurrency slot,
   retention and the run guard machinery: no change.
11. The fork resumes through the existing session map of the attempt
    loop (`KnownSession`, `scheduler/node.rs` line 86), the existing
    capture of an attempt's session (lines 2792 to 2868), the existing
    handoff decision (`scheduler/handoff.rs` `decide`, line 85) and the
    existing retry briefing (`scheduler/attempt_memory`); it adds an
    argument, not a path, and persists nothing (D3).
12. The doctor's free check (`crates/apb-core/src/agents/check.rs`) and
    its probe (`apb_core::detect::probe_output`, called by
    `crates/apb-core/src/doctor/agents.rs` line 159) serve the fork's
    help check as they are (D3 "Fork forms").
13. MCP: `run_status` carries `RunList` as it is
    (`crates/apb-mcp/src/tools/run.rs` unchanged). The HTTP routes carry
    it as they are (`crates/apb-server/src/routes/runs.rs` unchanged).

### Files this change adds

1. `crates/apb-engine/src/list/skip.rs` (D2): the set read, the
   journaled sets, the skip digest, the patch refusal and the view;
   `pub mod skip;` in `crates/apb-engine/src/list/mod.rs`.
2. Integration tests, each declared in `crates/apb-engine/tests/main.rs`
   in alphabetical place as `#[path = "suite/<name>.rs"] mod <name>;`
   (`list_skip_test` after `list_run_test`, line 430; `session_fork_test`
   before `session_handoff_test`, line 604):
   `crates/apb-engine/tests/suite/list_skip_test.rs`,
   `crates/apb-engine/tests/suite/session_fork_test.rs`.
3. `crates/apb-engine/tests/fixtures/journal/plain_list_run.jsonl`: the
   normalized journal of a plain list run, recorded on the unmodified
   tree (task 0.6) and compared by
   `a_plain_list_run_journals_what_main_journals`.

### Files shared with eval-doubles and inbox-waits

Both are merged before this change. Shared with both:
`crates/apb-core/src/validate/guarded.rs`,
`crates/apb-engine/src/event.rs`,
`crates/apb-engine/src/scheduler/prepare.rs`,
`crates/apb-engine/tests/main.rs`,
`crates/apb-cli/tests/suite/journal_corpus_test.rs`,
`docs/GUARDRAILS.md`, `docs/HOWTO-authoring.md`, `CLAUDE.md`,
`AGENTS.md`. Shared with `eval-doubles` only:
`crates/apb-engine/src/scheduler/patch.rs`. Shared with `inbox-waits`
only: `crates/apb-core/src/schema.rs`, `crates/apb-core/src/untrusted.rs`,
`crates/apb-core/src/validate/mod.rs`, `crates/apb-engine/src/state.rs`,
`crates/apb-engine/src/scheduler/entry.rs`, `crates/apb-engine/src/scheduler.rs`,
`crates/apb-engine/tests/fixtures/journal/known_events.jsonl`,
`crates/apb-server/src/ts_contract.rs`, `web/src/lib/api.gen.ts`,
`web/src/lib/journal.ts`. Each of these gets an entry next to theirs;
none of this change's decisions depends on theirs.

## D3. Session fork

### Schema

`crates/apb-core/src/schema.rs` line 2112, field `continue_session` of
`NodeKind::AgentTask`, changes type from `Option<String>` to
`Option<ContinueSession>`:

```rust
/// How a node with `continue_session` uses its source's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// Continue the source's conversation: this node's prompt is the next
    /// message of the same session (the behaviour before modes).
    #[default]
    Continue,
    /// Start from a copy of the source's conversation; the source session
    /// is never written to.
    Fork,
}

/// `continue_session`: a bare node id (mode `continue`) or
/// `{ from, mode }`. A bare id serializes back as a bare id, so the
/// digest of every existing definition is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueSession {
    pub from: String,
    pub mode: SessionMode,
}

impl ContinueSession {
    pub fn from(&self) -> &str { &self.from }
    pub fn forks(&self) -> bool { self.mode == SessionMode::Fork }
}
```

Serde by hand, like `FunctionsAllow` (line 1288, its `Deserialize` at
line 1304): an untagged intermediate `enum ContinueSessionForm {
Id(String), Full(ContinueSessionFull) }` where `ContinueSessionFull {
from: String, #[serde(default)] mode: SessionMode }` carries
`deny_unknown_fields`; `Serialize` writes the bare string when `mode ==
Continue` and the object otherwise. `mode: continue` written explicitly
reads the same as the bare id and serializes as the bare id.

Every reader of the field moves to `.from()` (the compiler lists them
once the type changes). The Rust sites on efae5822:
`crates/apb-core/src/preflight.rs` line 158,
`crates/apb-core/src/untrusted.rs` line 177,
`crates/apb-core/src/validate/node_io.rs` lines 29 and 68,
`crates/apb-core/src/validate/decisions.rs` line 318,
`crates/apb-core/src/validate/list.rs` lines 107 and 142,
`crates/apb-engine/src/scheduler/node.rs` lines 493 and 1472 (and the
destructured binding at line 959),
`crates/apb-engine/src/scheduler/handoff.rs` line 136. The test suites
write the field only in YAML, so no test source changes for the type.
The tier routing exclusion (`node.rs` line 493, "never routed") and V85's
refusal of a node continuing a list node's session (`validate/list.rs`
line 139) apply to a fork as they are: both match `Some(_)` or read the
source.

### Fork forms

`crates/apb-core/src/agents/forms.rs` `Forms` (line 10) gets, after
`resume`:

```rust
    /// The fork argv: the resume form that starts from a copy of
    /// `{session}` and writes nothing back to it. Same placeholders as
    /// `resume`. `None` for an agent with no such form: a fork starts
    /// cold.
    pub fork: Option<&'static [&'static str]>,
```

and `pub fn fork_argv(&self) -> Option<Vec<String>>` next to
`resume_argv` (line 28). Every agent's `forms()` sets the field. The two
forms are fixed here, read from the help output of the CLIs on this
machine on 2026-11-20 (`claude --help` and `opencode run --help`, no
other command):

1. claude 2.1.327 lists `--fork-session` ("When resuming, create a new
   session ID ..."). Its form
   (`crates/apb-core/src/agents/claude/mod.rs`, the `Forms` at line 96)
   is the resume argv of lines 115 to 125 with `"--fork-session"`
   inserted after `"{session}"`.
2. opencode 2.2.5 lists `--fork` under `opencode run` ("Fork the session
   before continuing", next to `--session, -s`). Its form
   (`crates/apb-core/src/agents/opencode/mod.rs`, the `Forms` at line 58)
   is the resume argv of lines 67 to 77 with `"--fork"` inserted after
   `"{session}"`.

The comment next to each `Some` says "Verified against <cli> --help
(<version>)", as the existing forms do (claude line 126). The build
machine's CLIs play no part: the forms are source, the same on every
build.

Every other agent's `Forms` literal sets `fork: None`: codex
(`codex/mod.rs` line 72; `codex fork` is an interactive picker and
`codex exec resume` has no fork flag), zcode (`zcode/mod.rs` line 123),
dsh (`dsh/mod.rs` line 116), agy (`agy.rs` line 54), cursor (`cursor.rs`
line 58), grok (`grok.rs` line 64), hermes (`hermes.rs` line 61), qoder
(`qoder.rs` line 62). `pi` has no `forms()` of its own and takes the
trait's `None` (`agents/agent.rs` line 20): no change.

An installed CLI whose help lacks the flag (an older or newer release
than the one read here) is handled by the existing pieces, not by a
build-time switch:

1. `apb doctor --agents` reports it: `crates/apb-core/src/agents/check.rs`
   `form_flags` (line 151) adds the fork argv's flags after the resume
   argv's, so the existing help-line check (`help_line`, line 129, over
   `missing_flags`, line 183) warns "help does not list --fork-session:
   the built-in form may have changed".
2. At run time the fork's handoff checks the same thing before it
   decides (D3 "The handoff" item 1): the installed CLI's help, read
   through `apb_core::detect::probe_output` with the agent's
   `help_args` (the probe `apb doctor --agents` runs,
   `crates/apb-core/src/doctor/agents.rs` line 159), must list every
   flag of the fork form (`check::missing_flags`, over the argv flags
   `check::argv_flags` lists, the one filter `form_flags` uses too). The
   probe runs last, after every cheap check of `decide` passed, so a node
   that will not fork (another agent, another profile, no session, an
   isolated node, another directory) never probes. It runs at most once
   per process and agent binary while its answer holds, never per
   execution: its answer is cached in process memory by
   `crates/apb-engine/src/scheduler/handoff.rs` `fork_help(agent,
   program)`, a map keyed by the agent id and the executable fingerprint
   the detector already uses, `path:size:mtime_ms` of the canonicalized
   program (`apb_core::detect` `fingerprint`,
   `crates/apb-core/src/detect.rs` lines 178 to 193, the value its cache
   keeps per agent at line 165; made public as it is), each key holding
   its own entry lock, so the global map's lock is held only to find the
   entry and never across a probe. A help that reads and lacks the flag
   is kept for the process; a help that does not read is kept for
   `FORK_HELP_RETRY` (60 seconds) and probed again after it, so a passing
   failure (a busy machine, a timeout) does not cold-start every fork of
   the process. A binary replaced at the same path has a new fingerprint
   and is probed again. Nothing is written to disk. When the help does not list the
   flag, or the probe fails (the help does not read), the node starts
   cold, journaling `session_handoff { warm: false, mode: fork, reason }`
   with the reason "the installed <agent> CLI does not list <flag> in its
   help" or "the help of <agent> could not be read: <why>".

`crates/apb-engine/src/invocation.rs`: `resume_spec` (line 87) keeps its
signature and its callers (five test suites call it); its body moves to
a private `fn session_spec(base, argv: Vec<String>, session) ->
InvocationDef`, and `pub fn fork_spec(base, agent, session) ->
Option<InvocationDef>` calls the same `session_spec` over
`pub fn fork_argv(agent_id) -> Option<Vec<String>>` (next to
`resume_argv`, line 79). One substitution, two argvs.

### The handoff

`crates/apb-engine/src/scheduler/handoff.rs`:

1. `decide` (line 85) takes `mode: SessionMode` and `fork_help:
   Result<(), String>` as its last arguments; it stays a function of its
   arguments and the journal. For `Fork` the first check (line 98) is
   `crate::invocation::fork_argv(agent).is_none()` with the cold reason
   "agent `<agent>` cannot fork a session"; the executor check, then for a
   fork the whole resolved profile (`ForkChecks.profile`, from
   `handoff::same_profile`: on the source's side the binding snapshot its
   succeeded attempt recorded when it ran (`attempt_started.exec.binding`,
   read through `execution_identity::read_binding`), never its binding
   now, so a source rebound after it ran is not forked under its new
   profile; on the node's side the binding the execution entry will
   actually run on (`entry`: a deferred retry's pinned binding, else the
   manifest with the rebind overlay applied); compared through
   `cache::profile_work`, so the role prompt, skills, fallbacks and tiers
   count and the routing fields do not; cold reason "`<src>` ran under
   profile X, this node runs under Y"), then the session id, isolation
   and directory checks as for `Continue` (a fork reads the source's
   session file from the same place), and the help last
   (`ForkChecks.help`, a closure that probes only when called; its `Err`
   is the cold reason). `Handoff::Warm` gains
   `fork: bool`. `node.rs` takes `fork_help` from the cached
   `handoff::fork_help` only for a fork on the CLI transport (D3 "Fork
   forms" item 2); a scenario run's
   scripted adapter spawns nothing and passes `Ok(())`, and a host-mode
   step never reaches `decide` (line 1476).
2. `is_source` (line 134) reads `.from()`: a fork's source must leave a
   session id behind like a continuation's.

`crates/apb-engine/src/scheduler/node.rs`:

1. `KnownSession::Id` (line 89) gains `fork: bool`: `false` everywhere it
   is built today (lines 116, 1460, 1525 and 2844).
2. `resolve_session` (line 100) returns `(id, workdir, fork)`; a titled
   entry resolves with `fork: false`. `continued` (line 1833) carries
   the flag.
3. The warm handoff (lines 1471 to 1507) runs for a fork on every entry
   of the node's execution: its condition `(Some(source), None, None)`
   over `(continue_session, &resume, &continuation)` becomes "no answer
   round, and either no continuation or a source that forks". So a
   deferred retry (`continuation` is `Some`, line 1472) forks again,
   as does a re-execution after a pause, a driver death or a resume
   (`continuation` is `None` there already). The step it decides for is
   the step the attempt will run on: `steps[0]` on a fresh entry, as
   today (line 1478), and `steps[continuation.step]` on a deferred entry
   (the step `entry_step` resumes, lines 1549 and 1642), so a deferral on
   a fallback step forks with that step's agent and model, probes that
   step's binary (`fork_help` over its program) and seeds that step's
   binding (`(agent, model)` of that step), never the primary's. It
   inserts `KnownSession::Id { id, workdir: Some(workdir), fork }` from
   `Handoff::Warm { fork, .. }` under that binding, passes `source.mode`
   and `fork_help` to `decide`, and journals `SessionHandoff { mode: Some("fork".into()),
   .. }` for a fork, cold or warm, once per entry (the field stays
   `None` for a continuation, and a continuation's block runs as today,
   so existing journals are unchanged). A host-mode step stays cold with
   the host reason (line 1476): no change.
4. The `--now` pause seed (lines 1512 to 1531) is skipped for a node
   that forks: the interrupted attempt's session is the fork's, and a
   re-entry forks the source again (item 3). For every other node it is
   unchanged.
5. The spec choice (lines 2066 to 2070, `match &continued`): a continued
   session whose entry has `fork == true` builds
   `crate::invocation::fork_spec(base, &step.agent, sid)` (`expect` text
   "a forked session implies a fork form"), else `resume_spec` as today.
6. The prompt (lines 2169 to 2209): an attempt that forks gets the
   handoff preamble and the whole node prompt, as the first handoff
   attempt does today (line 2173), followed by
   `attempt_memory::retry_briefing_now(run_dir, key)` (as the fresh arm
   at line 2207 appends it), which tells a re-entered attempt what the
   earlier attempts did and is empty for the first one. A retry that
   continues the fork's own session takes the continuation arm (line
   2177) as today.
7. The session an attempt leaves (lines 2759 to 2865): for an attempt
   that ran from a fork entry, `session_now` is the id the attempt's
   output reports (`report.session`, read by the agent's own reader:
   claude's `session_id`, `claude/output.rs` line 60; opencode's
   `sessionID`, `opencode/output.rs` line 83), and never the fallback
   to the continued id (the `.or_else(|| continued...)` at line 2808
   is skipped for a fork, and so is the same fallback of the transcript
   copy at line 2767, which would file the source's transcript as this
   attempt's). An id equal to the source's own id is checked first,
   before the transcript copy, the session file of a `--now` pause and
   the binding are written: the attempt did not fork, so the binding's
   entry is removed, the report's session is cleared, and the run
   journals a cold `session_handoff` with `mode: fork` and the reason
   `FORK_REPORTED_SOURCE`; the attempt's verdict stands as it is, and a
   retry in the same loop starts a fresh agent. When the attempt reports
   any other id, the binding's
   entry becomes `KnownSession::Id { id, workdir, fork: false }`: a
   retry in the same attempt loop continues the fork, never the source.
   When it reports none (a crash before any output), the entry stays the
   source with `fork: true`: the retry forks the source again, which
   never writes into it.

### Re-entry

The session map lives for one execution of the node (it is built at
line 1456 each time the node executes), and nothing about a
fork is persisted. The rule follows from that:

1. Within one attempt loop, a retry continues the session id the fork
   reported (item 7).
2. On every re-entry of the node (a deferred retry of `retry_delay`,
   which returns from the loop at line 1816 and comes back with a
   continuation; a pause and its resume; a driver death and the resume
   that reaps its attempt; any `apb resume`) the next attempt forks
   again from the source session (item 3). The source is never written
   into by a fork, so it is the same copy every time; the fork's earlier
   session is left behind, and the retry briefing tells the new attempt
   what failed (item 6).

The journal keeps the attempts' session ids as today
(`attempt_finished`); nothing reads them back for a fork.

`crates/apb-engine/src/event.rs` `SessionHandoff` (line 1400) gains:

```rust
        /// `fork` when the node forked its source's session (warm or
        /// cold); absent for a continuation and in older journals.
        /// Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
```

Its one construction outside tests is `node.rs` line 1501; the one test
pattern that names every field (`session_handoff_test.rs` line 70)
gets `..`.

### Validator and preflight

1. V44 (`node_io.rs` line 17) is unchanged in meaning; it reads
   `.from()`.
2. V45's race reason (lines 64 to 76, `racing` at line 66): two nodes on one source are
   reported only when at least one of the two continues (not forks).
   Two forks never race: neither writes into the source. A fork racing
   a continuation still warns: the continuation writes into the session
   the fork copies.
3. `crates/apb-core/src/preflight.rs` `cold_handoffs` (line 150): for a
   fork, the agent must have a fork form
   (`crate::agents::lookup(agent)` then `forms()` then `fork.is_some()`),
   else the line reads "node `<n>` forks the session of `<source>`, but
   its agent `<agent>` cannot fork a session, so it starts a fresh
   agent"; for a continuation the line stays as today
   (`detect::continues_sessions`, line 176). `apb fit` and the
   dashboard's fit report show it through the existing
   `session_handoff_cold` rule (`crates/apb-engine/src/fit/mod.rs` line
   212).
4. `crates/apb-core/src/untrusted.rs` line 177: a fork carries taint from
   its source like a continuation (it reads the same conversation).

### Surfaces

1. The dashboard's journal line for `session_handoff`
   (`web/src/lib/journal.ts` line 138): for `mode == "fork"` it reads
   "warm (fork): starts from a copy of the session of <from>" or "cold
   (fork)<reason>". No CLI command prints `session_handoff` (`rg -n
   session_handoff crates/apb-cli/src` finds nothing on efae5822).
2. Web editor: `web/src/lib/NodePanel.svelte` reads the field at line 197
   (`str(n.continue_session)`) and edits it at line 617; it gets a "fork
   instead of continue" checkbox next to the node field, and reads the
   object form's `from`. `web/src/lib/nodefields.ts` line 62 describes
   both shapes; `web/src/lib/NodeSummary.svelte` line 114 shows "<from>
   (fork)". The editor's model of the field is `string | { from: string;
   mode: 'continue' | 'fork' }`; it writes the bare string unless the box
   is ticked. Plain CSS over the existing CSS variables.

### Docs

`docs/HOWTO-authoring.md` "Continuing another node's session
(continue_session)" (line 3664): the object form, what a fork copies,
that two forks of one source may run at once, which agents fork (claude,
opencode), that the others and a CLI whose help lacks the flag start
cold with the reason, that a retry continues the fork, and that a
deferred retry, a pause, a driver death or a resume forks the source
again with the retry briefing.

### Wrong implementations

1. Passing the source's session id to two plain resumes, so both write
   into the source. Caught by
   `two_forks_run_in_parallel_and_leave_the_source_unchanged`.
2. Forking every retry from the source again after the fork reported its
   session. Caught by `a_retry_after_a_fork_continues_the_fork`.
3. Falling back to the source id for the retry of a forked attempt that
   reported no session, so the retry resumes the source plainly. Caught
   by `a_fork_that_reports_no_session_never_resumes_the_source`.
4. Serializing a bare id as an object, changing every playbook digest.
   Caught by `a_bare_continue_session_round_trips_as_a_string`.
5. Falling back to a plain continue when the agent has no fork form.
   Caught by `a_fork_on_codex_starts_cold_with_the_reason`.
6. Dropping V45's race warning for a fork racing a continuation. Caught
   by `v45_still_warns_when_a_fork_races_a_continuation`.
7. Persisting the fork id and continuing it after a re-entry, starting
   cold after one, or deciding a deferred entry for the primary step
   instead of the step it resumes (also caught by
   `a_delayed_retry_on_a_fallback_step_forks_on_that_step`). Caught by
   `a_delayed_retry_forks_the_source_again`,
   `a_pause_and_resume_forks_the_source_again` and
   `a_driver_death_forks_the_source_again`.
8. Passing `--fork-session` to a CLI whose help does not list it.
   Caught by `a_fork_on_a_cli_without_the_flag_starts_cold`.
9. Probing the help on every execution, or keeping an answer for a
   binary replaced at the same path. Caught by
   `the_help_probe_runs_once_per_process_and_binary` and
   `a_replaced_cli_at_the_same_path_is_probed_again`.
10. Comparing the source's binding now (the manifest through the rebind
    overlay) instead of the binding its attempt recorded when it ran, so a
    source rebound after it ran is forked under the node's profile.
    Caught by `a_source_rebound_after_it_ran_is_not_forked`.
11. Comparing the target's binding now instead of the binding its
    execution entry runs on, so a deferred retry rebound during its delay
    is forked into its pinned profile. Caught by
    `a_deferred_retry_rebound_during_its_delay_is_not_forked`.

### Tests

1. `crates/apb-core/src/schema.rs` `mod tests` (line 2735):
   `a_bare_continue_session_round_trips_as_a_string`,
   `the_object_form_reads_fork_and_continue`,
   `an_unknown_mode_is_a_parse_error`.
2. `crates/apb-core/tests/suite/validate_node_io_test.rs` (where V45 and
   the preflight are tested today, lines 167 and 200):
   `two_forks_of_one_source_do_not_race`,
   `v45_still_warns_when_a_fork_races_a_continuation`,
   `a_fork_on_an_agent_without_a_fork_form_is_cold`.
3. `crates/apb-core/tests/suite/agent_check_test.rs`:
   `form_flags_include_the_fork_flags` (claude's include
   `--fork-session`, opencode's `--fork`) and
   `a_help_without_the_fork_flag_warns` (the help line of
   `check::check` over a claude help text without `--fork-session`).
4. `crates/apb-engine/src/invocation.rs` `mod tests` (line 322):
   `fork_spec_inserts_the_fork_flag_after_the_session`.
5. `crates/apb-engine/tests/suite/session_fork_test.rs`, on the recording
   stub that `session_handoff_test.rs` imports from
   `token_economy_test` (`recording_stub`, `invocations`,
   `run_with_stub`, `seed_playbook`, lines 15 to 17; read that file first
   and reuse the helpers): the stub prints a help text listing the fork
   flag (or not, per test), appends to the session file it resumes and,
   given the fork flag, copies it to a new id first and prints that id;
   `two_forks_run_in_parallel_and_leave_the_source_unchanged` (the
   source file's bytes before and after),
   `a_retry_after_a_fork_continues_the_fork` (the second attempt's argv
   names the fork's id and has no fork flag),
   `a_fork_that_reports_no_session_never_resumes_the_source`,
   `a_delayed_retry_forks_the_source_again` (a `retry_delay` node: the
   attempt after the deferral has the fork flag and the source id, and
   its prompt carries the retry briefing; the deferral helpers of
   `deferred_wake_test.rs`),
   `a_delayed_retry_on_a_fallback_step_forks_on_that_step` (a profile
   with a fallback whose executor matches the source's: the deferral
   falls on the fallback step, and the attempt after it forks with the
   fallback's binary and binding, warm, while the primary's is untouched),
   `a_pause_and_resume_forks_the_source_again` (a `--now` pause
   mid-attempt, then a resume; `request_pause` as
   `operator_pause_test.rs` uses it),
   `a_driver_death_forks_the_source_again` (the drive killed
   mid-attempt, then `apb resume`, as `operator_pause_test.rs` line
   737 sets a dead driver up),
   `a_fork_on_codex_starts_cold_with_the_reason`,
   `a_fork_on_a_cli_without_the_flag_starts_cold`,
   `the_help_probe_runs_once_per_process_and_binary` (two forks and a
   re-entry in one test process: the stub records one help call),
   `a_replaced_cli_at_the_same_path_is_probed_again` (the stub binary is
   replaced at the same path with one whose help lacks the flag: the next
   fork probes again and starts cold),
   `the_handoff_journals_mode_fork`. In each re-entry test the source
   file's bytes are unchanged at the end.
6. `crates/apb-cli/tests/suite/journal_corpus_test.rs` row F1:
   `f1_fork_old_binaries_refuse_the_definition` (both released readers
   refuse to validate and to start a playbook with the object form, and
   append nothing).
7. `web/src/lib/journal.test.ts`: `a forked handoff says fork`;
   `web/src/lib/nodefields.test.ts`: `continue_session reads both shapes`.

## D2. Incremental lists

### Schema

`crates/apb-core/src/list/mod.rs` `ListSpec` (line 72) gets, after
`stop_when` (line 92):

```rust
    /// Items that passed in the previous successful run and are not
    /// worked again (V283). Absent: every item runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<ListSkip>,
```

```rust
/// `list.skip`: which earlier run counts, and what makes an item new
/// again.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListSkip {
    /// The one earlier run the set is read from.
    pub passed_in: PassedIn,
    /// Top-level item fields whose values must be the same as when the
    /// item passed. Absent: the whole item must be the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless_changed: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassedIn {
    /// The run `run.previous_success` names: the latest earlier
    /// top-level production run of the playbook in this scope that
    /// ended succeeded. The one value; a later source is an additive
    /// value.
    PreviousSuccess,
}

/// How many item entries one node's skip set keeps.
pub const LIST_SKIP_MAX_ENTRIES: usize = 1000;
/// How many bytes of entries one node's skip set keeps, so its
/// `list_skip_read` line stays under 200 KiB.
pub const LIST_SKIP_MAX_BYTES: usize = 192 * 1024;
```

`LIST_OUTPUT_FIELDS` (line 36) gains `"skipped"` after `"filtered_out"`
(9 entries), so a later node may read `nodes.<list>.output.skipped`
(V46 reads the array, `validate/node_io.rs` line 175). A list without
`skip` writes no such field (D2 "Output"), so the reference renders
empty there, as any absent output field does. It is not mirrored into
`web/src/lib/consts.gen.ts`.

`crates/apb-core/src/list/items.rs` gets, next to `items_digest` (line
207), over the existing private `canonical` (line 313) and the
`sha256_hex` the module uses:

```rust
/// The digest of an item's `unless_changed` fields: SHA-256 hex over
/// the canonical JSON of `{"fields":[..],"values":[..]}`, the field
/// names in the order written and each value encoding its presence:
/// `{"present":false}` when the item has no such field,
/// `{"present":true,"value":<v>}` otherwise, so an absent field and an
/// explicit `null` differ, as the list's own field comparison tells them
/// apart (`list/items.rs` line 239). The names are part of the digest, so
/// a run with another field list never matches.
pub fn fields_digest(value: &Value, fields: &[String]) -> String
```

### Validator: V283

`crates/apb-core/src/validate/list.rs`, in `check_lists`, for every node
with `list.skip`: V283 (error) when `unless_changed` is present and
empty ("unless_changed lists no field; leave it out to compare the whole
item"), names a field twice ("names `<f>` twice"), or names a field that
is empty or contains a `.` or whitespace ("skip compares top-level item
fields only"). Message prefix: `list.skip of node <id>:`. A `skip` on a
node without `list:` cannot occur (the field lives in `ListSpec`).

### The skip digest

**What the fingerprint claims.** The work fingerprint covers what apb
configures for an item: the definition, the scripts copy, the item, the
params, the profiles and their bundles, the connector definitions and the
account entries as written, the grants and the MCP pins, the runtimes,
the directories apb runs the step and the check in, `PATH` as the
children receive it. A node with an isolation other than `none` cannot
declare `skip` (V288). It does not cover the
machine around apb: inherited environment variables other than `PATH`,
the values secrets resolve to, files outside the scripts copy, and the
state of outside services. An author turns `skip` on for a list whose
items depend only on what the fingerprint covers; after changing anything
outside it (new credentials for another tenant, a tool on the machine) a
person makes the next run skip nothing by publishing a new version of the
playbook (the definition is part of the work). No sentence of this design
says more: two equal fingerprints mean apb configured the item the same
way, not that the world it acts on is the same.

A pass is reusable only while its skip digest matches. The skip digest
of an item is SHA-256 hex over the canonical JSON of
`{"id": <item id>, "item": <item part>, "work": <work fingerprint>}`:

1. The item part: the item's content digest (`item.digest`, the digest
   `rerun: not_passed` compares, `list/items.rs`) without
   `unless_changed`, else `fields_digest(&item.value, fields)`.
2. The work fingerprint: everything the run was configured with,
   taken whole; only the contents of the world are outside it. One
   function computes it, `pub(crate) fn work_fingerprint(inputs:
   &WorkInputs) -> String` in `crates/apb-engine/src/scheduler/cache.rs`,
   over the inputs `pub(crate) fn work_inputs(run: &ListRun) ->
   Result<WorkInputs, String>` gathers for one list node of one visit;
   it is the only comparison of work anywhere: the skip digest, the patch
   refusal and the rebind refusal all call it (D2 "The patch
   and rebind refusals"). It is SHA-256 hex over the canonical JSON of
   the parts below, under the fixed label `list-skip/1`.

#### What `WorkInputs` covers, and what it leaves out

The principle replaces a list of node-level inputs, which three reviews
in a row found one entry short: what apb configures for the item is
inside, taken whole; the machine around apb is outside (see "What the
fingerprint claims" above). `WorkInputs` covers exactly:

1. The definition: the whole definition the run executes, the same
   definition the journaled digest names (`run_definitions::digest`,
   `crates/apb-engine/src/run_definitions.rs` line 43, journaled as
   `run_started.definition` at `scheduler/prepare.rs` line 1600, and as
   `patch_applied.definition` after `store_best_effort`,
   `scheduler/patch.rs` line 268), whole: the latest journaled digest of
   the definition in force (`run_started`, `run_resumed` or
   `patch_applied`), over the YAML bytes, inline items included. It covers by construction
   the node, every other node, the `defaults` block, the top-level
   `worktree` declaration, `version` and every other field. Publishing
   changed content takes a new version (`apb_core::versioning`,
   `crates/apb-core/src/versioning/mod.rs` line 209), so every new
   version reruns every item, an inline list's items included: incremental
   skipping is for lists whose items come from an earlier node or a
   source, not for inline lists edited between versions.
2. The scripts: the digest of the run's whole `scripts/` tree as the
   run pinned it at start. The scripts a run executes are its own copy
   under `run_dir/scripts`, taken at start (`run_scripts.rs` lines 1 to
   3), and `run_provenance.scripts_digest` pins its digest
   (`run_scripts::digest`, line 13; `scheduler/prepare.rs` lines 1381 to
   1387, whose condition gains "the definition has a list `skip`"). No
   patch replaces the copy: a patch writes `playbook.yaml` only
   (`snapshot_playbook`, `scheduler/patch.rs` line 269). The code does
   allow a mid-run change: any node can rewrite the copy, since its path
   is in `APB_RUN_DIR` (`run_scripts.rs` lines 33 to 40, the reason
   `tampered` exists). So a visit whose live digest at its start differs
   from the pin skips nothing (`skip_scripts_changed`, D2 "Applying the
   set"), and a settlement after a mid-visit rewrite carries no skip
   digest. The pin is read from the journal once per visit and kept in
   the visit's state (`ListRun.pin`). The live digest of the scripts tree
   is taken at both ends of every item of a skipping list: as the item's
   work starts (`batch.rs` `run_item`) and at its fresh settlement,
   after its work and its check (`ListRun::minted_digest`), each through
   `ListRun::scripts_held`. Any difference at either end of any item
   latches `ListRun.scripts_diverged` and the visit stamps nothing from
   then on (fail closed), whatever the copy holds later; no answer is kept
   from one item for another. The first settlement after the latch
   closes journals why, once per list execution
   (`list_item_settled.skip_latched`, `skip_scripts_diverged`), and every
   surface shows it on the skip line ("stamping stopped: ..."). In a run
   whose definition declares a `skip` and that may skip (not a child, an
   eval or a scenario run: `list::skip::may_skip`, the choice
   `ensure_recorded` makes) the copy is also read-only exactly while
   items can run: every drive takes it at its entry through a guard value
   (`run_scripts::Protected`, held by `scheduler::drive`), which records
   the mode of every entry it walks and whose `Drop` sets exactly those
   modes again (a `0500` script is `0500` after the drive, never `0700`)
   on every exit of the drive, whatever
   ends it (a finish, an abort, a pause drain or `--now`, a park, an
   engine pause, an error return, a panic that unwinds); a run that
   waits at a gate or sits paused or parked, with no drive, has a
   writable copy, as does a prepared run before its driver starts. Only a
   killed process leaves it read-only: the run's end gives owner write
   back to a copy left read-only (`run_end::record`, only when the copy's
   directory lacks it), and the removers of production run directories,
   `retention` (`apply` and `archive::recover`) and the scenario store's
   cleanup, make its directories writable first
   (`run_scripts::writable_tree`). Eval, scenario and child runs never
   take the protection, so the eval removers (the scratch drop, the stale
   scratch sweep, the case directories) never meet a read-only copy. The
   modes guard against accidents: a script or an agent that writes under
   the copy fails on its own write. They are not a boundary: the copy's
   parent stays writable, so a same-user process can move the copy aside,
   put another in its place and restore it, and root is not stopped by the
   mode at all. What apb detects is only a change still present when an
   item starts or settles; with `batch.parallel` above 1 the items of one
   visit can affect each other through the shared copy inside each other's
   windows (see Fix round 4, finding 4). Runs without `skip` keep the
   copy's modes as on main.
3. The run's inputs: the canonical start request
   (`apb_core::start_key::StartRequestFields`,
   `crates/apb-core/src/start_key.rs` line 98), whole, with the fields
   named below removed. The start key captures that struct only at a
   launch surface, from the arguments as typed, and keeps only its MAC
   (`StartRequest::capture`, line 150; the CLI fills
   `context_max_bytes` and `context_compact_model` with `None`,
   `crates/apb-cli/src/run.rs` lines 757 and 758), so `work_inputs`
   fills the same struct for every run from the run's persisted
   `RunConfig` (`run.yaml`, the effective values) through one private
   function beside `work_inputs`, `start_request_of(&RunConfig)`, which
   builds the struct literal (so a field added to the struct later fails
   to compile until it is filled, and is then inside by construction). Kept: `params`,
   `instruction` (`RunConfig.instruction`, `run_config.rs` line 158,
   which a list item's prompt carries after the rendered template,
   `scheduler/node.rs` lines 1022 and 1043), `overrides`,
   `context_max_bytes`, `context_compact_model`, and every field added
   later. Added, since the struct lacks it: `RunConfig.output_limits`
   (`run_config.rs` line 258), which caps what an attempt's output
   capture keeps. Removed by name, each because it cannot change how an
   item is worked or checked: `v` (the form's version); `version` (the
   version asked for; the executed definition is part 1); `worktree` (the
   tree asked for; the resolved identity is part 6); `supervision` (who
   watches; what a supervisor applies is handled at the visit, D2
   "Applying the set"); `cache` (a list never takes the node cache,
   `scheduler/cache.rs` line 85); `max_parallel` (scheduling);
   `max_patches_per_run` (a patch after skips is refused anyway);
   `deadline_ms` and `budget` (they stop work, never change it);
   `unattended` (who decides gates, not how an item is worked);
   `allow_shared_workdir` (locking); `continued_from` (lineage);
   `external_data` (an external run's passes never reach a later set);
   `execution` (the mode asked for: `RunConfig` has no execution field,
   `run_config.rs` line 153, so it cannot be filled from the run, and the
   effective mode is the manifest's execution block, part 4).
   The struct holds no notify setting and not the start key itself, so
   neither needs removing. Every value for which
   `apb_core::run_params::sensitive(declared, name, value)`
   (`crates/apb-core/src/run_params.rs` line 41: declared secret,
   secret-shaped name, credential-shaped value) is `Some`, the
   instruction included when it looks like a credential, enters only as
   the installation-keyed MAC
   `apb_core::run_origin::tag_if_keyed("apb-list-skip-param-v1", <name
   and value>)` (`crates/apb-core/src/run_origin.rs` line 136, the
   non-creating form of the keyed tag `tag`, line 126, that the start key,
   `start_key.rs` lines 88 and 153, and remembered input,
   `run_memory/store.rs` line 203, use), never bare and never as a bare
   hash; when it returns `None` (the installation has no key yet),
   `work_inputs` fails with `skip_no_key` and the visit skips nothing.
   The creating `tag` is never called for this: a list skip does not
   create the installation key.
4. What the manifest resolved: its execution block whole, as recorded
   (`RunExecutionManifest.execution`, `ManifestExecution`,
   `crates/apb-engine/src/manifest.rs` lines 349 and 511: the effective
   mode `cli` or `host` with the `+guard` and `+external` suffixes it
   records, its `source`, the host `client` and `fallback_to_host`), or
   the marker `none` when it has none; and for the node the effective profile as the manifest recorded it
   resolved, whole (`ManifestProfile`, `manifest.rs`, through
   `effective_for_node` so a rebind is seen): its bundle digest, and every
   resolved invocation of its chain and of its tiers as recorded
   (`ResolvedInvocation`, `crates/apb-engine/src/invocation.rs` line 18:
   agent, model, invocation specification, soul delivery, the canonical
   executable path and the cloud snapshot, with `executable_fingerprint`
   removed by name, see exclusion 10), and `ephemeral`; not `executor_digest` alone, which leaves the
   executable out (`manifest.rs` lines 109 to 117), so a changed
   `agents.<id>.program` (`apb_core::config`, `config.rs` line 414,
   resolved by `program_for`, `invocation.rs` line 182) reruns every item, while the
   same binary updated in place at the same path does not; and at the node site and at
   `<node>#check` the connector definition digests and the resolved
   grants (`ManifestConnectorGrant`, `manifest.rs` line 290) with each
   granted account's identity and snapshotted digest (`account_digest`,
   `connector/run.rs` line 181). A node with neither a profile nor a
   connector reads no manifest, and a run without one is fine for it
   (main omits an empty manifest, `scheduler/prepare.rs` line 1580); a
   node that needs one and has none fails `work_inputs` with
   `skip_needs_manifest`.
5. The decision configuration that governs the node's judge checks: the
   non-secret effective settings the run's decision runner uses, the
   manifest's snapshot capped by the machine's live file
   (`DecisionRunner::for_run`, `crates/apb-engine/src/decision/mod.rs`
   lines 393 to 402, `EffectiveDecisions`, `crates/apb-core/src/decisions.rs`
   line 398: the global mode, each use and whether it is off, advising or
   enforcing, its thresholds, the providers with their models and
   routes, budget and privacy), serialized without any key value, or the
   marker `none` when the run has no decisions block. The item judge
   check has no model of its own (`JudgeCheck`, `list/mod.rs` line 406);
   it asks this runner (`scheduler/list/check.rs` line 328).
6. The execution target: the canonical directory the node's step
   executes in (`cache::execution_dir`): the run's tree as the drive
   takes it (`scheduler::tree_dir`: the resolved worktree, a scenario
   run's directory, else the project root it falls back to), under the
   node's own `workdir` template rendered by the same resolution and
   renderer the execution uses (`node_workdir::resolve` over
   `render_node_prompt`); never the marker `none`, so a project copied
   to another directory works every item there. Its identity only, never
   its contents; a template that resolves to no directory refuses
   (`skip_workdir_unresolved`). It is read when the visit starts, since a
   node can resolve the tree mid-run.
7. The script runtimes: for every runner the node's step and its checks
   run (a script node's `runner`, `sh` for a script `success_check` or a
   script item check), the runner as the run resolved it
   (`script::ResolvedRunner`: the key, whether the machine config's
   `runners` names it, the candidate list whole, the candidate chosen, the
   canonical absolute path of the program that is exec'd, the name it is
   started under and its identity, `path:size:mtime_ms` of that file). A
   configured candidate given as a relative path is taken against the
   run's project root, once; a program that does not canonicalize fails
   the resolution. It is resolved once per run directory and process
   (`script::resolved_runner`), held in the visit's resolved execution
   (`cache::VisitExecution`) and every item of a skip visit runs its step
   under that value (`script::run_script_with_runtime`); a runner that does
   not resolve refuses (`skip_runtime_unresolved`). Its identity is read
   again as the visit's work is computed and at every settlement
   (`cache::runtime_changed`): a runtime replaced at the same path after
   the run resolved it refuses the visit's skip and latches the visit, so
   no later settlement stamps (`skip_runtime_changed`). A sandboxed
   `success_check` starts the `sh` runner under its configured name, not
   its canonical target, as before this change: a multicall binary behind
   a link picks its applet from that name, and no skip covers an isolated
   node (V288).
8. Removed in Fix round 7: a node with an isolation other than `none`
   cannot declare `skip` (V288), so no isolation is part of the work.
   The number stays free so the other parts keep theirs.
9. The MCP tool approvals of the granted accounts (`pin_digest` from the
   machine's pin store), reread at each settlement (Fix round 4).
10. The canonical directory the item checks run in, the run's tree
    (Fix round 5).
11. `PATH` as the steps and checks of a skip visit receive it (Fix round
    5). No other inherited variable is part of the work. An absent `PATH`
    is captured as absent; one that is not valid Unicode cannot be named,
    so the visit's work does not resolve and the skip is refused
    (`skip_path_unrepresentable`).
12. The item checks' policy flags the visit derives from the run's
    options and the definition: `guard_tree` (one worker only) and
    `holds_tree` (Fix round 6); the checks run from the captured flags.

A tree that comes from a node output (`scheduler/worktree.rs` lines 1 to
7: resolved when the node the template reads succeeds) or that is created
fresh for each run has a new path every run, so such a playbook never
skips; the docs say so.

Outside it, each for one reason:

1. File contents of the workspace: they are the world the list works
   on, and they change every day; only the target's identity counts.
2. Outputs of earlier nodes: they are results of this run, not its
   configuration; they reach a list as its items (compared per item). A
   skipping agent list's prompt may not read them, nor any other run
   value but params and the instruction (V289).
3. Time: the clock is not configuration; a routine that must rerun by
   date names the date in its items.
4. The previous run's own state: the source run is only where the set
   comes from; what it did is in its settlements, not in this run's
   configuration.
5. The item's own data: it is in the item part of the skip digest,
   compared per item, so one added or changed item of a list fed by an
   earlier node or a source does not rerun the others. Inline items are
   part of the definition (part 1), so they get no such exception.
6. Secret values in clear: a sensitive value enters only through the
   keyed MAC of item 3, an account's secret through its snapshotted
   digest, never a bare value or hash (CLAUDE.md conventions).
7. The `## known problems` section of an item prompt
   (`known_problems::prompt::section`, `context.rs` line 308, from the
   `known_problems_shown` event at start): it is advice about tools from
   a store that earlier runs fill, not configuration of the task.
8. Supervisor notes: they are not inside; instead a visit that would
   carry one skips nothing (D2 "Applying the set"), because a note can
   change the task while the run is under way.
9. Tier routing: the fingerprint names the configured executor chain
   (part 4 holds every resolved invocation of the chain and the tiers), so an attempt
   the engine routes to a configured tier neither changes the
   fingerprint nor needs a comparison; routing follows main.
10. The agent CLI's executable fingerprint (`executable_fingerprint`,
   size and modification time, `invocation.rs` line 25): an agent CLI
   updated in place at the same path is the world changing, like a model
   behind the same model id, not the run's configuration; the canonical
   executable path and the invocation form identify the configured
   executor. It is removed from part 4 by name.

So any new playbook version, any changed param or instruction, any
changed profile, connector account or decision setting, and any changed
script or working-tree target makes every item run again once; the
author narrows what counts as an item change with `unless_changed`, and
nothing else.

The ordinary node cache key (`scheduler/cache.rs` `prepare`) is
unchanged; `work_fingerprint` shares no code path with it.

The digest is computed from the definition in force when the item
settles: `ListRun` (`scheduler/list/run.rs`) computes the work
fingerprint once per visit, and a patched run's later visit would
compute it from the patched definition; since a patch always changes
the definition, a run that already has skip-derived settlements refuses
every in-run patch (D2 "The patch and rebind refusals"), and a run
without them is patched as on main and its later skips simply do not
match. Inline item values are part of the definition, so an edited
inline list reruns every item under its new version; only the items of
a list fed by an earlier node or a source are compared one by one.

`work_inputs` returns `Err(<reason>)` when a part does not read (the
scripts tree, a needed manifest, a key for a secret param); the skip is
then refused for the node by that name (`SkipSet.refused`, D2 "Applying
the set"): its items work, and their settlements carry no skip digest.

### Reading the set: once, from one earlier run

New `crates/apb-engine/src/list/skip.rs`:

```rust
/// One list node's skip set: every item id that passed in the source run
/// with the skip digest it passed with.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SkipSet {
    /// The run the set was read from; absent when there is none (no
    /// earlier success, a child, scenario or eval run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Item id -> the skip digest it passed with.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, String>,
    /// More entries passed than `LIST_SKIP_MAX_ENTRIES` or
    /// `LIST_SKIP_MAX_BYTES` keep: the first in id order were kept, the
    /// rest run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Item id -> the output its source settlement recorded (the compact
    /// JSON of its declared outputs, at most `LIST_SKIP_OUTPUT_MAX_BYTES`,
    /// scrubbed when it was journaled): a skipped item carries it, so later
    /// nodes read what the item produced. An entry of `items` absent here
    /// passed with no output object, and a skipped item carries none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// Items that passed in the source with a stamp but are not in the set
    /// because their output cannot be carried: withheld (clipped at
    /// settlement or a redaction marker in it) or over the per-item cap.
    /// They are worked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub not_inherited: u32,
    /// Set-time refusal, for two reasons: the run's type cannot be told
    /// after a crash (`skip_run_type_unknown`, written by
    /// [`ensure_recorded`](super::skip::ensure_recorded)), or the source run
    /// is no longer an earlier success on the read the sets come from
    /// (`skip_source_ineligible`); the node then skips nothing in this run.
    /// Every reason that needs the work is decided at the visit's start and
    /// recorded on its `list_started` (`skip_refused`), never here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// Every list node with `skip` -> its set, read from the one earlier run:
/// the run `window` names when the run recorded its
/// `run.previous_success` window, else the run `previous_success::find`
/// returns. `Err` (`list_skip_unreadable`) when that lookup or the
/// source's journal does not read.
pub(crate) fn read_sets(
    root: &Path,
    playbook: &Playbook,
    playbook_id: &str,
    scope_label: &str,
    window: Option<&PreviousSuccess>,
) -> Result<Vec<(String, SkipSet)>, EngineError>;

/// Every list node with `skip` -> an empty set.
pub(crate) fn empty_sets(playbook: &Playbook) -> Vec<(String, SkipSet)>;

/// The set this run journaled for `node` (its `list_skip_read` naming
/// it); `None` when it journaled none.
pub(crate) fn recorded(events: &[Event], node: &str) -> Option<SkipSet>;

/// Whether this run journaled the sets of its definition: `All`, `None`
/// or `Some` (partial, a reader refuses it: `list_skip_incomplete`).
pub(crate) fn coverage(events: &[Event], playbook: &Playbook) -> Coverage;

/// Records the sets of a run that declares `skip` and journaled none:
/// the same choice as preparation (D2 "Where the set is read and
/// journaled"), written as one group. A no-op when all are recorded;
/// `Err(list_skip_incomplete)` when only some are.
pub(crate) fn ensure_recorded(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
    cfg: &RunConfig,
    log: &mut EventLog,
) -> Result<(), EngineError>;

/// The skip digest of one item (D2 "The skip digest").
pub(crate) fn skip_digest(id: &str, item_part: &str, work: &str) -> String;

/// Why a patch from `old` to `new` must be refused, if it must.
pub(crate) fn patch_refusal(old: &Playbook, new: &Playbook, events: &[Event]) -> Option<String>;

/// The skip view of one node for the run surfaces (D2 "Surfaces").
pub fn view(fold: &ListFold, set: Option<&SkipSet>) -> Option<SkipView>;
```

`read_sets`:

1. The source: `window.run_id` when the run recorded a window (the
   definition reads `run.previous_success`; its lookup already ran,
   `scheduler/prepare.rs` lines 1078 to 1115, or after the turn), so
   the window and the skip always name the same run; else
   `crate::previous_success::find(root, playbook_id, scope_label)`
   (line 33), the same checked lookup, its eligibility and its
   external-data rule. No source: every node gets an empty set.
2. The source's journal through the strict `event::read_all` (`find`
   just read it the same way). For each list node with `skip`,
   `ListFold::of(&events, node)` (`list/ledger.rs` line 150, the source's
   latest visit of that node): every item record whose state is
   `Passed` and that carries a `skip_digest` enters the set as `id ->
   skip_digest`, in id order, until the set holds
   `LIST_SKIP_MAX_ENTRIES` entries or its serialized entries would pass
   `LIST_SKIP_MAX_BYTES`; then `truncated` is set. A skipped item of the
   source is a passed record with its skip digest (D2 "Applying the
   set"), so the chain continues through each next successful run: an
   item worked once is skipped by every later success while its digest
   holds.
3. An error refuses with `list_skip_unreadable: run <id>: its <what>
   does not read; repair or remove it, then start again` (the
   `HistoryUnreadable` fields, as `look_up_previous_success` words its
   own, `scheduler/prepare.rs` line 2093).

A run started with `external_data` reads the history like any run; its
own passes never reach a later set, because `find` skips every run
`learns_from` refuses (the rule `docs/EXTERNAL-DATA.md` states for
every reader of earlier runs): no change there.

### Where the set is read and journaled

`crates/apb-engine/src/scheduler/prepare.rs`, after the concurrency key
is resolved (`let key = crate::concurrency::key_for_start(..)`, lines
1129 to 1142, which follows the `run.previous_success` window of lines
1074 to 1115 and the worktree resolution) and before `allocate_run`
(line 1227), where params are final and nothing is written yet:

```rust
let has_skip = playbook.nodes.iter().any(|n| n.list().is_some_and(|l| l.skip.is_some()));
let mut list_skip = if !has_skip {
    None
} else if scenario.is_some() || opts.eval.is_some() || opts.parent_run.is_some() {
    // Repeatable runs and children read no history: every item runs.
    Some(crate::list::skip::empty_sets(&playbook))
} else if let Some(pred) = opts.continued_from.as_deref() {
    // A continued run keeps its predecessor's sets; a node the
    // predecessor recorded none for gets an empty set.
    Some(continued_sets(&pred_events(pred)?, &playbook))
} else if key.is_some() {
    // A start that takes a concurrency slot reads after admission (below).
    None
} else {
    Some(crate::list::skip::read_sets(root, &playbook, id, t.origin_label, previous_success.as_ref())?)
};
```

`pred_events` stands for the predecessor journal read the window
branch does at lines 1088 to 1093: lift that read into one local
(`Option<Vec<Event>>`, read once when `continued_from` is set and either
reader needs it) so the journal is not read twice; its refusal text
stays the window's. `continued_sets` first checks the predecessor's own
coverage: `coverage(pred_events, <the predecessor's definition>)`, the
definition read from the predecessor's run snapshot
(`runs/<pred>/playbook.yaml`). Partial coverage (some of the
predecessor's skipping list nodes recorded, others not: a write cut
between complete set lines) refuses the continuation with
`list_skip_incomplete: run <pred> recorded the skip sets of <recorded>
but not of <missing>; start a new run`, before anything is copied and
before the run directory exists. With complete coverage it copies
`recorded(events, node)` per list node of the new definition with
`skip`, and gives an empty set to a node the predecessor's definition
did not declare as skipping. A predecessor with no set at all (its
definition declared no skip, legitimately unrecorded) keeps the
documented fallback: every node gets an empty set.

Three outcomes, by whether the start can queue:

1. A start that takes no concurrency slot (`key` is `None`) never
   queues: its set is read here, before `allocate_run` (line 1227),
   so an unreadable history refuses the start before the run directory
   exists and leaves no run.
2. A start that takes a slot does not read here: whether it queues is
   decided by `concurrency::admit` (line 1252), which needs the
   allocated run. Admitted at once (`Admission::Active`, line 1269), it
   reads right after the admission, before `key_queued` is used (line
   1289) and before anything is journaled; an unreadable history refuses
   it through the cleanup the refused admission uses (lines 1253 to
   1262, lifted into one local closure both call): the directory holds
   only the empty journal and `driver.pid` and goes away whole, the
   start key's `Claim` frees its entry, and the provisional slot claim
   is released with its owner as on any preparation error before
   `confirm` (line 1650). It leaves no run.
3. A start that queues (`Admission::Queued`) reads nothing in
   preparation (no early read): its set is read once, after its turn
   (below). A read that fails there ends the run failed with the
   named reason, as a failed `run.previous_success` lookup does.

After the `run_guard` append (line 1644), when `list_skip` is
`Some(sets)`, every node's line in one journal write, in definition
order, as one decision under the journal's append lock:

```rust
log.append_group_decided(|events| match coverage(events, &playbook) {
    Coverage::None => sets.into_iter().map(|(node, set)| EventPayload::ListSkipRead { node, set }).collect(),
    _ => Vec::new(), // another writer recorded them first: append nothing
})?;
```

What is recorded at record time is only what is known then: the source
run, the items with their skip digests, `truncated`, and `refused`
only for a run whose type cannot be told (`ensure_recorded`, below).
Every reason that needs `work_inputs` (a needed manifest, the scripts
pin, the installation key for a sensitive value, a supervisor note) is
decided at the visit's opening step, where a `ListRun` exists, and
recorded on the visit's `list_started` (D2 "Applying the set").

`EventLog::append_group_decided` (`crates/apb-engine/src/event.rs`)
reuses the decided path every check-then-append in the journal uses:
`Build::Decided` (`event.rs` line 3241) re-reads the journal under the
append lock (lines 3018 to 3021) before it writes. It always takes the
required lock: the durable path of `append_lines`, which calls
`JournalLock::required` (lines 3015 and 3016, `required` at line 3268)
as `append_decided_synced` (line 2930) does, never the best-effort
`JournalLock::on` of the plain path (line 3018), which goes on when the
lock cannot be taken. The required lock waits at most `LOCK_WAIT` (2
seconds, `JournalLock::required_within`) and then fails with
`journal_locked`. A lock that cannot be taken refuses the write
(`list_skip_lock_unavailable: the journal lock could not be taken, so
the skip sets were not recorded`); a full disk is its own reason
(`list_skip_disk_full`), and any other I/O error passes through as it
is, never named a lock failure. The start or drive returns that
error before any node runs, closed out the way its caller closes any
error (preparation's `prep_try`, `after_the_turn`'s callers, `drive`'s
error path). With the lock held, so two
entries that race (a turn's entry and a resume's drive) record one
group and the second appends nothing; `recorded(events, node)` is the
one `list_skip_read` naming the node. It is the general form of
`append_pair` (line 2955, the one multi-event append,
which the form gate uses for a review request and its `review_detail`,
`scheduler/review_gate.rs` line 206): the private `append_lines` (line
2997) takes its `follow` builders as a list instead of one, and
`append_pair` becomes the two-event call of it. All lines go out in one
write under one lock, none a checkpoint, so a crash leaves all or none,
up to a torn last line, which fails every strict read as today. A write
torn after a complete line can still leave some lines: `coverage`
reports that as partial and every reader refuses it (below).

`crates/apb-engine/src/scheduler/entry.rs` `after_the_turn` (line 334)
reads a queued run's sets after the window: its early return (line
342) becomes "nothing to read" only when the definition neither reads
`run.previous_success` nor has a list `skip`, or the run continues
another. The function then holds two blocks, each guarded by its own
"already journaled" check (`PreviousSuccessSet` for the window, as
today; `ListSkipRead` for the sets): the window block first, unchanged,
then the sets block, which calls `read_sets(root, playbook, <playbook
id>, &scope, <the window>)` with the scope the window block reads from
`run_provenance.origin` (lift that read above both blocks) and the
window this run recorded (the one the block above appended, else
`previous_success::recorded(&events)`), and appends one `ListSkipRead`
per node through the same group write: the sets block is
`list::skip::ensure_recorded`. When `read_sets` fails, the block returns its
`list_skip_unreadable: ...` error exactly as the window block returns
its lookup error; both callers (lines 245 and 738) then run
`let_go_unclaimed` and `close_out_unclaimed` as today, which journals
`run_error` with the reason and `run_finished failed` (lines 401 to
407). One outcome for both reads: a queued run whose window or skip
read fails after its turn ends failed with the named reason.

Every drive records what is missing before it drives:
`crates/apb-engine/src/scheduler.rs` `drive_inner` (called by `drive`,
line 463, through which every start, resume and continued run passes)
first calls `list::skip::ensure_recorded`, the function
`after_the_turn` uses. So a run that declares `skip` and journaled no
set when a drive begins (a queued run paused before its turn and then
resumed, which reserves its slot in `resume.rs` line 1225 and goes
straight to `drive` at line 1305; a crash between the `run_guard` line
and the group write; a resume from a named node, `resume.rs` line 112)
reads and records its sets at that point, before any node runs, from the
history as it stands then, with the window the run recorded. A failure
goes through `drive`'s own error path (`run_error` with the reason and
`run_finished failed`, the doc at line 455). A run with some sets but not
all cannot be written by construction (one group write); a reader that
meets one anyway (`coverage` is partial) refuses to drive with
`list_skip_incomplete: the run recorded the skip sets of <recorded> but not
of <missing>; start a new run`, the same refusal `opening_steps` gives,
and never substitutes an empty set. The choice of `ensure_recorded`
follows preparation and fails closed on the run type: empty sets for a
child or eval run (`RunConfig.parent_run`, `RunConfig.eval`) and for a
scenario run (`scenario::session::is_scenario(run_dir)`,
`scenario/session.rs` line 444), as `prepare` decides; then the
manifest's `RunOrigin` must read and name a production run; with no
manifest (a script-only run writes none, `prepare.rs` line 1580) or an
origin that does not read, the skip reads no history and every set is
recorded `refused: skip_run_type_unknown: the run's origin cannot be
told after a crash, so its skip reads no history`; the predecessor's
sets for a continued run; else `read_sets`.

`crates/apb-engine/src/event.rs`, a new variant after
`PreviousSuccessSet` (line 241):

```rust
    /// The skip set of one list node (`list.skip`), read once at start
    /// (or once a queued run got its concurrency key, or at the first
    /// drive that found none). Every visit of the run reads this record,
    /// never the history. One line per list node with `skip`, all of a
    /// run's lines in one write, bounded by `LIST_SKIP_MAX_ENTRIES` and
    /// `LIST_SKIP_MAX_BYTES`. Not a checkpoint: safe to skip up to the
    /// first `node_finished`; a run that has it carries the `list_skip`
    /// guard.
    ListSkipRead {
        node: String,
        #[serde(default)]
        set: crate::list::skip::SkipSet,
    },
```

A line holds the node id, the source run id and at most
`LIST_SKIP_MAX_BYTES` (192 KiB) of entries, so it stays under 200 KiB
whatever the number of list nodes.

`ListItemSettled` (line 2135) gains, after `digest`:

```rust
        /// The earlier run this item passed in, for an item a `list.skip`
        /// settled without running it in this visit. A skipped row is
        /// never carried into a later visit. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skipped_from: Option<String>,
        /// The skip digest of the item (D2 "The skip digest"): minted by
        /// a fresh settlement whose step executed in this visit, kept by a
        /// carried row as it had it, the source's stamp on a skipped row.
        /// Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skip_digest: Option<String>,
```

Every construction that names all fields of `ListItemSettled` gets the
two as `None` (on efae5822: `scheduler/result.rs`,
`scheduler/attempt_memory.rs`, `scheduler/approval.rs`, `result_view.rs`,
`connector/call/attempt.rs`, the test helper in `list/ledger.rs`, and
`crates/apb-server/tests/suite/runs_api_test.rs`; the compiler lists
them), except `carry_passed`, which copies the carried record's
`skip_digest` as it had it (D2 "Applying the set").

`ItemRecord` (`list/ledger.rs` line 32) gains `skipped_from:
Option<String>` and `skip_digest: Option<String>`, folded in `fold`
(line 162) from those fields like `carried` and `digest`.

### The patch and rebind refusals

1. `list::skip::patch_refusal` refuses a patch whose `new` definition
   has a list node with `skip` for which the run journaled no
   `list_skip_read`: `list_skip_unrecorded: node <n> has no skip set
   recorded at start; start a new run instead`. It mirrors
   `previous_success::patch_refusal` (`previous_success.rs` line 363,
   "previous_success_unrecorded") and is called right after it in
   `scheduler/patch.rs` (line 116).
2. The same function refuses every in-run patch of a run that already
   has skip-derived settlements (a `list_item_settled` with
   `skipped_from`, of any list node): `list_skip_work_changed: the run
   settled items by a list skip; a patch changes its definition and so
   the work those items were skipped under; start a new run instead`.
   It needs no comparison: part 1 of `WorkInputs` is the whole
   definition, which every patch changes, so `work_fingerprint` would
   always differ, and the refusal is unconditional for such a run (no
   other comparison of work exists; `nodes_differ`,
   `scheduler/patch.rs` line 350, is not used for this). A run without
   such settlements is patched as on main; its later visits compute the
   fingerprint from the patched definition, so its skips simply do not
   match and every item works.
3. A rebind (`scheduler/rebind.rs` `apply_rebind`, line 127) of such a
   node is compared the same way: `work_inputs` of the node as bound now
   and with the candidate, whose part 4 is computed from the whole
   candidate `ManifestProfile` as the overlay would hold it
   (`rebind.rs` lines 134 to 138: its bundle digest and every resolved
   invocation of its chain and tiers whole, with the connector grants
   the run keeps), through
   `work_fingerprint`;
   when they differ it is journaled `RebindRejected` with
   `list_skip_work_changed: node <n> settled items by its skip in this
   run; a rebind may not change its work`, as the existing rejections are
   (line 147); when either fingerprint cannot be computed it is rejected
   with `list_skip_work_unknown: node <n> settled items by its skip in
   this run, and its work cannot be compared: <reason>`. Tier routing is
   not refused and follows main: the fingerprint names the configured
   executor chain, and part 4 holds every resolved invocation of the
   chain and the tiers, so an attempt routed to a configured tier (`set_routed`,
   line 246, called by the engine's `route_execution`, `node.rs` line
   541) neither changes the fingerprint nor needs a comparison.

The refused change matters because an open visit keeps its terminal
rows (`scheduler/list/mod.rs` line 137): without the refusal an item
skipped under the old work would stay passed in the visit under the
new one without ever being worked.

### Applying the set

`crates/apb-engine/src/scheduler/list/run.rs`: `ListRun` computes the
work fingerprint once (`work()`, a lazily filled
`Result<String, String>`, unused without `skip`). `Settlement` (line 164)
gains `skipped_from: Option<String>` and `skip_digest: Option<String>`;
`settle_stamped` (line 135) writes both. A skip digest is minted only by
a fresh settlement whose item's work ran under exactly what apb configured
the work fingerprint covers and whose check actually ran and passed. The
rule is stated once, here, and enforced in one place, `ListRun::minted_digest`:
a settlement with `ran: Some` (the step's result, line 117) in state
`passed` of a node that declares `skip`, whose visit's recorded decision
allows skipping, whose item check (when it has one) ran to a verdict
(`Settlement.check_decided`: not the `on_unavailable` fallback of an
unavailable judge, so neither `accept_with_note` nor any "not run" or
"unknown" outcome), with `work()` `Ok`, the scripts equal to the pin, and
nothing in force that the fingerprint does not cover (`ListRun::stamp_block`:
a supervisor note applied to the run before the settlement, whether at
the visit's start, at a reopen, at a resume or with a supervisor retry,
whatever the visit decided at its start; or a revised pilot's guidance
applied to the visit). A carried row keeps the stamp it had, or stays
without one; a skipped row keeps the source's stamp (the entry it
matched); every other settlement (an oversized item, an interruption, a
reconcile) carries none. Only a passed row with a stamp enters a later
set.

`crates/apb-engine/src/scheduler/list/mod.rs` `opening_steps` (line 171),
in the new-visit branch, after `carry_passed` and before
`park_unknown_sends`:

```rust
if run.spec.skip.is_some() {
    skip_passed(run, skip_set.as_ref(), sends, done, &carried)?;
}
```

`opening_steps` takes the node's recorded set as one more argument
(`Option<&SkipSet>`, `crate::list::skip::recorded(events, node)` over
the journal `open_visit` and `complete_opening` already hold), so a
drive that died after `list_started` and before its first batch skips
the same items when `complete_opening` (line 199) reruns the opening
steps: no recovery code of its own. Every new visit applies it, a later
visit of a loop included; a resumed or reopened visit never does (it
already holds its settlements).

`carry_passed` (line 308) carries only rows that were worked and
passed, exactly as on main: it leaves every row with `skipped_from` out,
so a row the skip settled in an earlier visit is pending for this visit.
It returns the ids it carried and copies the carried record's
`skip_digest` as it had it (line 325), never minting one. The skip step
of each visit then decides the earlier skipped rows again from the run's
recorded set under that visit's recorded decision: skipped again with
`skipped_from` when the decision allows (after the pin check before the
skip settlements), worked when it is refused (a supervisor note, changed
scripts, any other reason).

`skip_passed`: a node that declares `skip` and has no recorded set
refuses the visit with `list_skip_incomplete` (it cannot happen after
`ensure_recorded`; an empty set is never substituted). The visit's skip
decision is computed in `open_visit` (`scheduler/list/mod.rs` line 132)
before the `ListStarted` append (line 150), on the state at the visit's
start, and stored in it: `ListStarted` gains `skip_refused:
Option<String>` (`#[serde(default, skip_serializing_if =
"Option::is_none")]`, written only for a node with `skip`; absent means
the visit may skip). `opening_steps`, `skip_passed` and
`complete_opening` only read it from the visit's `list_started` and never
recompute it, so a drive that died between `list_started` and the
opening steps replays the same decision. The scripts pin is checked again right before the skip settlements are
written, both in the opening steps and in their recovery
(`complete_opening`): when the live scripts digest differs from the pin
then, the visit fails with `list_skip_scripts_changed: the run's scripts
copy changed before the skip settlements were written` and no item is
settled as skipped (the node finishes failed with that reason, as an
opening error does); a recorded decision that allowed skipping is never
acted on against changed scripts. Items settled by work after a
mid-visit rewrite still carry no skip digest. The visit skips nothing, and
its settlements carry no skip digest, when any of these holds at its
start, in this order: the set
is recorded `refused`; `work()` is `Err` (its reason:
`skip_needs_manifest`, `skip_scripts_unreadable`, `skip_no_key`); the
live scripts digest differs from the pin (`skip_scripts_changed`); an
applied supervisor note is in force for the node, that is
`context::applied_supervisor_notes(events)` (`context.rs` line 170,
public; the notes `supervisor_notes_section`, lines 200 to 210, puts into
every item prompt,
`with_supervisor_notes` at line 213) is not empty when the visit starts
(`skip_supervisor_note: a supervisor note is in force, so every item
works`).
Otherwise, for each item of `run.items` not in `done`, not carried in
this pass, whose id is in the set, whose send in this run is not unknown
(`sends.unknown(run.key(&item.id))` is `None`: such an item is parked by
`park_unknown_sends` as today), and whose skip digest now equals the
entry's: settle it through `run.settle(Settlement { id, state:
settled::PASSED, carried: true, skipped_from: set.source.clone(),
skip_digest: Some(<that digest>), ..Default })` (`ran: None`, so no
snapshot is taken; `round: 0`, no verdict or note), with the output the
set carries for the item (`SkipSet.outputs`, D2 "Output"; none when the
source item had no output object).

### Counts

Within a visit an item is worked (settled with `carried: false`),
carried by `rerun: not_passed` (`carried: true`, no `skipped_from`; only
a row worked and passed in the previous visit) or skipped (`carried:
true` with `skipped_from`, settled by the skip step of this visit).

1. `ListCounts` (`list/ledger.rs` line 66) gains `skipped: u32`: passed
   records with `skipped_from.is_some()`. They also count in `passed`
   and `processed`, so the goal criterion `{ type: list }` treats a
   skipped item as done (no change to `list_criterion`).
2. `ListFold` gains `pub fn skipped_ids(&self) -> Vec<String>`: every
   record with `skipped_from`.
3. The early stop: `batch.rs` line 115 builds `outside` from the pilot
   slice; it becomes the slice plus `fold.skipped_ids()`, so `stop_when`
   never counts an item carrying skip provenance (a share over 290
   skipped and 10 worked items would never stop). Items carried by
   `rerun: not_passed` count in the share as on main, so a plain list
   behaves exactly as on main. `stop_when_holds` and `counts_excluding`
   are unchanged.
4. `progress_label` (line 455) appends `, <n> skipped` when `skipped >
   0`.

### Probation

A list node that declares `skip` earns its run attended probation
credit only with at least one fresh passed settlement whose step actually
ran: a `list_item_settled` of the node with state `passed`, `carried:
false` and admitted in a batch (`ItemRecord.batch` above 0, folded from
`list_batch_started`, `list/ledger.rs`). A skipped or carried item does
not count, and neither does one settled without execution, such as an
oversized item `settle_too_large` settles before any batch
(`scheduler/list/mod.rs` line 343, with `carried: false` by default,
`list/run.rs` line 163), which with `on_needs_look: continue` would
otherwise let a list that worked nothing succeed (line 405). An item
restored from the node cache does not count either (a cache hit runs
nothing): a passed settlement that follows a `node_cache_hit` of the
node in the same visit (`EventPayload::NodeCacheHit`, `event.rs` line
1354) is not fresh. On main a list node never takes the node cache
(`cache_eligible` refuses it, `scheduler/cache.rs` line 85, "A list
node's output is its items' settlements, never a cache entry", and every
list settlement passes `cache_hit: false` to the snapshot capture,
`scheduler/list/run.rs` line 124), so the rule guards a later change
and costs one match over the journal. The rule applies only to a list node that skipped at least one
item in the run (some `list_item_settled` of the node with
`skipped_from`): a skipping list never reached (a branch not taken) or
with zero items costs the run no credit, as the same run without `skip`
earns it. A run in which a node that skipped items has no fresh worked
pass succeeded without running the version's work. The eligibility is
read in the terminal observer's entry,
`crates/apb-engine/src/probation.rs` `entry_of` (line 148): the
`eligible` conjunction (line 204) gains `&& skip_lists_worked(events)`,
a new private function over the journal. `ListFold` offers only `of`
(the latest visit) and `previous` (`list/ledger.rs` lines 150 and 156);
`visit_starts` and `fold` are private, so this change adds one public
`ListFold::visits(events, node) -> Vec<ListFold>`, every visit of the
node in order over the same `visit_starts` and `fold`, with its unit
test `visits_lists_every_visit_in_order`.
The entry is still written (unattended, outcome and digest as today);
only `eligible` is false. Readiness (`apb_core::readiness`,
`fit::readiness`) is unchanged.

### Output

`compose` (`scheduler/list/mod.rs` line 365) inserts `"skipped":
c.skipped` after `"filtered_out"` only when `run.spec.skip` is `Some`; an
item row gains `"skipped_from"` only when set. A plain list's output is
what main writes. A skipped item carries the output its source settlement
recorded: `skip::set_of` takes the latest visit's `ItemRecord.output` for
each entry from the same journal read the set comes from (rule B, one
read) into `SkipSet.outputs`, and `skip_passed` settles the row with it,
so `compose` writes it into `nodes.<list>.output.items[]` and a later list
or template reads what the item produced, its `skipped_from` naming run R.
Bounds: an output is carried only up to `LIST_SKIP_OUTPUT_MAX_BYTES` (2
KiB, the cap a list settlement already clips an item's output at), and the
set's ids, digests and outputs together stay within `LIST_SKIP_MAX_BYTES`
(192 KiB, past which the remaining entries are worked and the set says
`truncated`). An item whose source output was clipped
(`{"truncated":true}`) or holds a redaction marker (`[redacted` or
`[secrets_unresolved]`) is not in the set: it is worked, counted in
`SkipSet.not_inherited` and shown on the skip line ("N not skipped"). A
source item that produced no output object carries none, as it had none
in its own run, so a list whose steps print no JSON still skips.
Declared output fields: the carried output passed the output contract in
its own run, under the same work fingerprint, so it is not checked again.
Secrets: the stored output was scrubbed with the run's scrubber when it
was journaled, and nothing else is read for it.

### Surfaces

1. The view, shaped like the pilot's: `SkipView { skipped: u32,
   read_items: u32, source: Option<String>, truncated: bool, refused:
   Option<String> }` (ts,
   mirrored in `crates/apb-server/src/ts_contract.rs` next to
   `PilotView`), built by `list::skip::view`: `None` when the node
   skipped nothing and the run journaled no set for it. `RunList`
   (`list/view.rs` line 20) gains `skip: Option<SkipView>`
   (`skip_serializing_if` none, ts optional), filled in `run_lists`
   (line 273) from `recorded(events, node)`; `RunList::of` (line 102)
   fills it from the fold alone. `ListProgress` (line 82) gains
   `skipped: u32` (`skip_serializing_if` zero, ts optional).
   `RunListItem` (line 50) gains `skipped_from: Option<String>`
   (`skip_serializing_if` none, ts optional).
2. `ListOutcome` (`run_outcome/lists.rs` line 20) gains `skip:
   Option<SkipView>` copied in `list_outcomes` (line 137) like `pilot`,
   and `line` (line 54) appends `, <n> skipped (passed in earlier runs)`
   after the processed count when non-zero. `run_outcome/summary.rs`
   reads the lines as they are: `apb runs <id>`, MCP `run_status`, the
   dashboard and the HTML report show it with no further code.
3. `apb runs <id> --items` (`crates/apb-cli/src/run_items.rs`):
   `item_line` (line 109) prints a skipped row as `skipped (passed in
   <run>)`; `item_lines` (line 69) prints `skip: <read_items> items from
   run <source>` (`, more not kept` when truncated; `skip: no earlier
   success` without a source; `skip refused: <reason>` when refused)
   above the node's rows.
4. Dashboard: `web/src/lib/ListProgressPanel.svelte` shows "<n> skipped"
   next to the counts and the skip line, and marks a skipped row with
   the earlier run's id as a link to that run;
   `web/src/lib/listprogress.ts` builds both labels (`skipLabel`). The
   dashboard journal (`web/src/lib/journal.ts`) gets a `list_skip_read`
   line: "skip set of <node>: <n> items from run <source>" (or "no earlier
   success"). Plain CSS over the existing CSS variables.

### Docs

1. `docs/HOWTO-authoring.md` "List processing (list:)" (line 1252): a
   `skip` subsection: `passed_in: previous_success` (the run
   `run.previous_success` names, and how a skipped item carries its
   stamp to the next success), `unless_changed`, the skip digest in
   plain words, saying plainly: any new playbook version, any changed
   param or instruction, any changed profile, connector account or
   decision setting, any changed script or working-tree target makes
   every item run again once; the author narrows what counts as an item
   change with `unless_changed`, and nothing else; incremental skipping
   is for lists whose items come from an earlier node or a source, not
   for inline lists edited between versions (a new version reruns every
   item); a playbook whose working tree comes from a node output
   or is created fresh for each run never skips; an agent CLI updated in
   place keeps the skips, while a changed program path reruns them; a visit that carries a
   supervisor note works every item; the `## known problems` section of
   a prompt does not count, that the set is read once at start (after
   its turn for a queued run) and a resume keeps it, that a patch that
   adds a `skip` is refused, that once a run settled items by a skip
   every in-run patch is refused (a patch changes the definition) and a
   rebind that changes the work too, while a run without such
   settlements is patched as on main and its later skips do not match, that children, scenarios and evals skip nothing, that skipped
   items are never in a pilot slice and `stop_when` leaves skipped
   items out of its share (carried ones count as before), that a run in which a list that skipped items
   passed no item it worked fresh earns no probation credit, the V283 row, and the example:
   `dependency-security-fix` working only new or changed advisories.
2. `docs/GUARDRAILS.md` line 276: the guard name `list_skip` in the
   list of names, one sentence on what 0.25.0 does with it (it predates
   list mode: it drops the whole list block and runs the step once,
   skipping nothing), one on an older apb (it reads and stops such a run at any
   time and refuses to pause it at any time, since the guard follows
   every checkpoint), and one on trust: the skip
   picks its earlier run by the same origin stamp and checks
   `run.previous_success` uses, and the journals of runs under the
   operator's own account are inside the trust boundary, as the
   paragraph on the passkey store says for every other apb guard (line
   406: a same-user writer stays out of scope), so an edited journal of
   an earlier run is not defended against.

### Skipped rows across a continued visit

A continued visit (a resume, a drive after a crash) and a reopened one (a
supervisor retry) first decide whether the visit may hold skips at all,
with the rule a new visit is decided on (`list::visit_refusal`: a field
`unless_changed` names that no item has, the work that does not read,
scripts that differ from the pin, a supervisor note in force). If it may
not, every skipped row goes back to pending, with `skip_supervisor_note`
for a note and `skip_config_changed` otherwise; if it may, a row whose
skip digest no longer matches the work goes back with
`skip_config_changed`. The ledger fold reads either reason as taking the
row back (`ledger::invalidates_skip`): it is pending, counted in
`SkipView.reworked`, with no skip provenance and no output, so a later
settlement that prints none leaves it with none. Rows the visit worked
are left as on main.

Every row state against every event that continues or changes a visit,
with the test that covers the cell (engine tests in `list_skip_test.rs`
unless named otherwise; "main" cells run main's code and are covered by
main's list tests):

| Row | Pause | Resume | Reopen (supervisor retry) | Patch | Rebind | Deferred retry |
|---|---|---|---|---|---|---|
| pending | stays pending (main: `list_resume_test::a_now_pause_interrupts_items_and_the_resume_files_each_message_once`) | worked (`a_resume_with_nothing_changed_keeps_the_skips`: c) | worked (main: `list_resume_test::a_supervisor_retry_reruns_only_the_items_that_need_a_look`) | applied when no row was skipped (`a_patch_before_any_skip_is_applied_and_skips_nothing`), refused otherwise (`a_patch_after_skips_is_refused`) | refused once a row was skipped (`a_rebind_of_a_node_with_skipped_items_is_refused_in_a_run`), else main | not reachable |
| passed, worked | kept (`a_resume_with_nothing_changed_keeps_the_skips`: b) | kept (`a_resumed_visit_works_a_skipped_item_whose_runtime_changed`, CLI: b) | kept with its stamp (`a_reopen_under_a_supervisor_note_stamps_nothing`: a) | as pending | as pending | not reachable |
| skipped | kept (`a_resume_with_nothing_changed_keeps_the_skips`: a) | kept when the work holds (same test), back to pending when it changed (`a_resumed_visit_works_a_skipped_item_whose_runtime_changed`) | back to pending under a note (`a_note_in_force_takes_back_the_retained_skips_of_a_reopen`), kept otherwise (`a_reopen_with_nothing_changed_keeps_the_retained_skips`) | refused (`a_patch_after_skips_is_refused`) | refused (`a_rebind_of_a_node_with_skipped_items_is_refused_in_a_run`) | not reachable |
| skipped, then taken back | pending, no output, no provenance on the fold and `run_lists` (`a_skip_taken_back_keeps_no_inherited_output`) | worked; settles with the output it prints, none here (same test) | reached as passed, worked: the visit works it before the node can finish | refused: the run settled a row by a skip (`a_patch_after_skips_is_refused`) | refused, likewise (`a_rebind_of_a_node_with_skipped_items_is_refused_in_a_run`) | not reachable |
| needs a look | kept (main: `list_resume_test::a_drain_pause_finishes_the_running_item_and_the_resume_runs_the_rest`) | kept (main) | worked again (`a_reopen_under_a_supervisor_note_stamps_nothing`: b; `a_note_in_force_takes_back_the_retained_skips_of_a_reopen`: b) | as pending | as pending | not reachable |
| skipped in an earlier visit, worked in this one (the retry briefing) | as pending | its `## item` section names no earlier attempt and no `previous_outputs`: a skipped row never ran in this run (`ListRun::previous` ignores it in this visit's fold and the earlier visit's, `a_skipped_row_is_never_the_items_earlier_attempt`) | the same rule, both branches of `ListRun::previous` | refused (`a_patch_after_skips_is_refused`) | refused (`a_rebind_of_a_node_with_skipped_items_is_refused_in_a_run`) | not reachable |
| carried (`rerun: not_passed`) | kept (main: `list_run_test::a_loop_visit_under_rerun_not_passed_carries_the_passed_items`) | kept: a carried row is a worked row, never a skipped one (the carry leaves skipped rows out, `scheduler/list/mod.rs` `carry_passed`; they are skipped again from the set, `the_second_loop_visit_decides_skipped_rows_again`) | kept (main) | as pending | as pending | not reachable |

A deferred retry never reaches a list visit: `retry_delay` is refused on
a list node (V126, `validate/retry.rs`), and a run releases for another
node's delay only with nothing running, so its resume is the resume
column. The patch and rebind columns hold for every row because the
refusal reads the run, not the row: once any row of the run was settled
by a skip, a patch or a rebind of the node is refused.

### Wrong implementations

1. Reading the history at each visit or on resume, so a run that passed
   in between changes the set. Caught by
   `a_resume_keeps_the_journaled_set`.
2. Skipping by id alone, ignoring a changed item. Caught by
   `a_changed_item_runs_again` and
   `a_changed_field_outside_the_list_is_still_skipped`.
3. Keeping a hand-picked list of inputs, so one configured input is
   missing from the fingerprint (an item check, a default, a helper
   script, a param, the instruction, an invocation, a profile skill, an
   account, a decision use, the working tree). Caught by
   `every_configured_input_reruns_every_item`,
   `a_judge_turned_from_off_to_enforce_reruns_every_item`,
   `an_instruction_only_change_reruns_every_item` and
   `a_worktree_only_change_reruns_every_item`.
4. Exempting inline item values from the definition, so an edited
   inline list skips under a new version. Caught by
   `every_configured_input_reruns_every_item` (its definition row adds an
   inline item).
5. Hashing a secret param value, bare or in clear. Caught by
   `a_sensitive_param_enters_only_as_a_keyed_tag`.
6. Taking the version a run started with instead of the definition in
   force at settlement. Caught by
   `a_patched_run_stamps_the_patched_definition`.
7. Letting any in-run patch through once the run settled items by a
   skip, or refusing a patch of a run without such settlements. Caught
   by `a_patch_after_skips_is_refused` and
   `a_patch_before_any_skip_is_applied_and_skips_nothing`.
8. Letting a rebind change the work of a node that settled
   items by its skip, or through when its work cannot be compared.
   Caught by `a_rebind_of_a_node_with_skipped_items_is_refused`.
9. Accepting a patch that adds a skip the start never read. Caught by
   `a_patch_adding_a_skip_is_refused`.
10. Counting a skipped item as pending, so a `{ type: list }` goal fails.
    Caught by `a_goal_over_a_list_counts_skipped_items_as_done`.
11. Reading any run but the one `run.previous_success` names. Caught by
    `items_that_passed_in_the_previous_success_are_skipped` (a newer
    failed run is not read) and
    `the_skip_reads_the_run_the_window_recorded`.
12. Dropping a skipped item from the next run's set, so the chain breaks
    after one skip. Caught by
    `a_skipped_item_carries_its_stamp_to_the_next_run`.
13. Silently skipping nothing when the source does not read. Caught by
    `an_unreadable_earlier_run_refuses_the_start`.
14. Reading early for a start that queues, or leaving a run behind for an
    unqueued one. Caught by `a_queued_run_reads_its_set_after_the_turn`,
    `a_queued_run_whose_read_fails_ends_failed` and
    `a_keyed_start_whose_read_fails_leaves_no_run`.
15. Reading history in a child, an eval or a scenario run. Caught by
    `a_child_run_skips_nothing`.
16. Comparing digests of different field lists. Caught by
    `fields_digest_names_the_fields`.
17. Putting a skipped item into the pilot slice, counting skipped items
    in the early-stop share, or dropping the items `rerun: not_passed`
    carried from it. Caught by
    `a_skipped_item_is_never_in_the_pilot_slice` and
    `stop_when_judges_only_the_items_worked`.
18. Carrying a skipped row into a later visit, so a refused visit keeps
    it passed, or minting a stamp on a carried row. Caught by
    `a_note_between_visits_makes_the_skipped_item_work`,
    `a_scripts_change_after_the_second_list_started_fails_before_any_skipped_row`
    and `a_carried_item_after_restored_scripts_gets_no_stamp`.
19. A recovery of its own, or none, for a drive that died between
    `list_started` and the skip settlements. Caught by
    `a_drive_that_died_after_list_started_skips_the_same_items`.
20. An unbounded journal line, or one line for every node. Caught by
    `the_set_keeps_entries_up_to_both_caps` and
    `each_list_node_journals_its_own_set`.
21. Writing the sets one append at a time, or substituting an empty set
    for a missing one. Caught by `each_list_node_journals_its_own_set`
    (one write), `a_crash_before_the_set_write_records_them_at_the_next_drive`
    and `a_partly_recorded_run_refuses_to_drive`.
22. Copying a predecessor's partial sets into a continued run. Caught
    by `a_continuation_after_a_cut_between_set_lines_is_refused`.
23. Requiring a manifest a script list does not need. Caught by
    `a_script_list_without_a_manifest_skips`.
24. Writing `skipped` into a plain list's output. Caught by
    `a_plain_list_run_journals_what_main_journals`.
25. Counting a run that worked nothing toward probation, an oversized
    item settled without execution or an item restored from the node
    cache included. Caught by
    `a_list_that_worked_no_item_is_not_eligible`.
26. Skipping a visit that carries a supervisor note. Caught by
    `a_supervisor_note_before_the_visit_skips_nothing`.
27. Taking a hand-picked subset of the run's configuration, or hashing
    a sensitive plain param bare. Caught by
    `every_configured_input_reruns_every_item`,
    `work_inputs_takes_each_part` and
    `a_sensitive_param_enters_only_as_a_keyed_tag`.
28. Comparing a rebind without the candidate's resolved invocations, or letting it
    through when the work cannot be compared. Caught by
    `a_rebind_changing_only_the_executor_is_refused` and
    `a_rebind_whose_work_cannot_be_compared_is_refused`.
29. Deciding a refusal at record time that needs the visit, or
    stamping digests after the scripts copy changed. Caught by
    `a_visit_after_a_scripts_rewrite_skips_nothing` and
    `a_scripts_rewrite_mid_visit_stamps_no_digest`.
30. Two set groups from racing entries. Caught by
    `two_concurrent_entries_record_one_group`.
31. Reading history for a run whose type cannot be told. Caught by
    `a_crashed_script_only_eval_reads_no_history`.
32. Denying probation credit to a run whose skipping list skipped
    nothing. Caught by the unreached case of
    `a_list_that_worked_no_item_is_not_eligible`.
33. Leaving the effective execution mode out, so a host run skips what
    a cli run passed. Caught by `a_cli_to_host_change_changes_the_work_fingerprint`.
34. Writing the sets on the best-effort lock, so two writers append two
    groups, or going on without the lock. Caught by
    `two_concurrent_entries_record_one_group` and
    `a_journal_lock_failure_refuses_the_set_write`.
35. Recomputing the visit's skip decision in the opening steps or in
    recovery. Caught by
    `a_crash_after_list_started_replays_the_recorded_skip_decision`.
36. Taking `executor_digest` alone, so a changed program skips. Caught by
    `a_program_only_change_reruns_every_item`.
37. Acting on a recorded skip decision against changed scripts in
    recovery. Caught by `a_crash_then_scripts_rewrite_fails_the_visit`.
38. Creating the installation key for a sensitive value, or classifying
    a crashed eval run by its manifest. Caught by
    `a_sensitive_param_enters_only_as_a_keyed_tag`,
    `a_crashed_script_only_eval_reads_no_history` and
    `a_crashed_run_with_an_unknown_origin_reads_no_history`.
39. Treating an absent field and an explicit `null` alike. Caught by
    `fields_digest_tells_absent_from_null`.
40. Comparing a rebind by `executor_digest`. Caught by
    `a_rebind_changing_only_the_program_path_is_refused`.
41. A cached comparison across items: keeping one item's scripts answer
    (per batch or per visit) for another item's stamp, so an item that ran
    under scripts another item rewrote is stamped for the pinned ones.
    Caught by `a_scripts_rewrite_after_the_first_settlement_stamps_no_later_item`.
42. A fingerprint field that differs from what executes: the runner
    resolved afresh at each script while the work names none, or the
    marker `none` while execution falls back to the project root. Caught
    by `a_changed_runner_runtime_reruns_every_item`,
    `a_run_keeps_the_runtime_it_resolved`,
    `a_copied_project_works_every_item_in_its_new_directory` and
    `the_execution_dir_is_the_tree_or_the_nodes_own_workdir`.
43. A second read judged apart from the first: the source run approved on
    the lookup's read and its sets consumed from another read that is
    never judged (the lookup path), so a source reopened in between hands
    over a new visit's stamps. Caught by
    `a_source_reopened_after_the_lookup_gives_no_skip` and
    `a_window_source_no_longer_settled_gives_no_skip`.
44. A skipped row counted as executed: inherited passes in the item check
    evidence or its freshness, so an all-skipped list reports a current
    executed check. Caught by `an_all_skipped_list_claims_no_executed_check`
    and `skipped_items_are_never_executed_check_evidence`.
45. A skipped row with no output feeding later work: a downstream list or
    template reads `null` for every skipped item, works on it and stamps
    that. Caught by `a_skipped_item_carries_its_output_down_the_chain`
    and `an_output_over_the_cap_means_the_item_is_worked`.
46. A tree the checks cannot see between their two ends: a writable
    scripts copy a script or an agent writes under by accident inside
    another item's work. Caught by
    `a_script_writing_next_to_itself_fails_on_its_own_write`; a latch that
    says nothing is caught by `a_latch_is_journaled_once_and_shown`. A
    deliberate replacement of the tree is a stated limit, not caught.
47. A fingerprint input execution resolves again: the work names one
    directory or runtime and the item's execution resolves its own.
    Caught by `an_item_runs_in_the_directory_its_visit_resolved` and
    `a_relative_runner_runs_the_program_its_work_names`.
48. A skip kept across a resume under another configuration: a continued
    or reopened visit keeps its skipped rows while its execution resolves
    to other work. Caught by
    `a_resumed_visit_works_a_skipped_item_whose_runtime_changed` (and
    `a_resume_with_nothing_changed_keeps_the_skips` for the other side).
49. A switch derived from run options left out of the work: the worker
    count decides whether a check guards the tree while the fingerprint
    drops `max_parallel`. Caught by `the_check_policy_is_part_of_the_work`.
50. A skip on an isolated node: the stamp names the tree directory while
    an item without a sandbox, or with one that degraded, runs in a fresh
    scratch directory. Refused by V288; caught by
    `skip_on_an_isolated_node_is_refused` and
    `skip_holds_under_isolation_none_and_is_refused_when_isolated`.
51. Two rules for "may this visit hold skips": a new visit refuses under a
    supervisor note while a continued or reopened visit checks only the
    digest, so a reopen after a note keeps its skipped rows. One function,
    `list::visit_refusal`, decides both. Caught by
    `a_note_in_force_takes_back_the_retained_skips_of_a_reopen` (and
    `a_reopen_with_nothing_changed_keeps_the_retained_skips` for the other
    side).
52. A skipped row taken back keeps its inherited output, so a new
    execution that prints none settles with the old JSON and the next run
    inherits it. Caught by `a_skip_taken_back_keeps_no_inherited_output`.
53. A skipped row read as the item's earlier attempt: a later visit's
    retry briefing hands the item the output it inherited, labelled as its
    own. Caught by `a_skipped_row_is_never_the_items_earlier_attempt`.
54. A protection taken by a run that skips nothing, so the eval removers
    meet a read-only copy after a killed driver. Caught by
    `an_eval_run_leaves_its_scripts_modes_untouched` and
    `a_scenario_run_leaves_its_scripts_modes_untouched`.
55. Giving the copy back by adding owner write, so a script shipped
    without it gains it. Caught by
    `a_drive_gives_the_scripts_copy_its_exact_modes_back`.

### Tests

1. `crates/apb-core/src/list/items.rs` `mod tests` (line 333):
   `fields_digest_names_the_fields`, `fields_digest_tells_absent_from_null`
   (an item without `amount`, then with `"amount": null`: the digests
   differ).
2. `crates/apb-core/tests/suite/list_schema_test.rs` (where V85 to V95
   are tested today): `v283_refuses_an_empty_field_list`,
   `v283_refuses_a_dotted_field`, `v283_refuses_a_repeated_field`,
   `a_valid_skip_is_clean`.
3. `crates/apb-core/src/validate/guarded.rs` `mod tests` (line 192):
   `list_skip_is_a_guarded_feature`, and the count test renamed
   `all_has_fifteen_entries_in_order`.
4. `crates/apb-engine/src/list/skip.rs` `mod tests`:
   `the_set_keeps_entries_up_to_both_caps`,
   `recorded_is_none_without_the_event`,
   `the_skip_digest_covers_the_id_the_item_and_the_work`,
   `coverage_tells_all_none_and_partial`.
5. `crates/apb-engine/src/list/ledger.rs` `mod tests` (line 482):
   `skipped_items_count_as_passed_and_skipped`,
   `visits_lists_every_visit_in_order`.
6. `crates/apb-engine/src/scheduler/cache.rs` `mod tests` (line 1043):
   `work_inputs_takes_each_part` (one assertion per part of `WorkInputs`
   over its serialized form, the removed start-request fields absent and
   a sensitive value present only as its tag).
7. `crates/apb-engine/src/run_outcome/lists.rs` `mod tests` (line 222):
   `the_line_names_skipped_items`.
8. `crates/apb-engine/src/probation.rs` `mod tests` (line 278, its
   `journal()` helper): `a_list_that_worked_no_item_is_not_eligible`
   (every item skipped; and, as a second case, every item skipped but
   one oversized item settled `needs_look` before any batch with
   `on_needs_look: continue`: not eligible either; as a third case, the
   one passed item admitted in a batch follows a `node_cache_hit` of the
   node in the visit: not eligible; an otherwise equal journal with one
   passed item admitted in a batch and no cache hit stays eligible; and
   a run whose skipping list was never reached, or loaded zero items,
   stays eligible).
9. `crates/apb-engine/tests/suite/list_skip_test.rs` (runs of one
   playbook version with a script step; a test whose items stay the same
   between runs may use an inline list, and a test that changes items
   between runs feeds the list from an earlier script node that prints
   them from a file outside the run, since an edited inline list is a new
   version; the fixtures of `list_run_test.rs`, `playbook`,
   `inline_items`, `seed`, `run_list` and `events`, lines 40 to 219, are
   `pub(crate)`: reuse them):
   `items_that_passed_in_the_previous_success_are_skipped`,
   `every_configured_input_reruns_every_item` (one table test, one row per
   input of D2 "What `WorkInputs` covers": a row changes only its input
   between two runs and asserts that every item runs again; rows: a new
   version with a description-only change, a `defaults` change, an item
   check change and an added inline item (part 1), a helper script under `scripts/` (part
   2), a param value, a secret param value, the run instruction,
   `context_max_bytes`, `context_compact_model`, the node overrides and
   the output limits (part 3), the machine's `agents.<id>.invocation.argv`, a profile
   skill's content, a granted account's field, the execution mode
   from `cli` to `host` and the configured program of the agent (part
   4); and a negative row: the agent binary updated in place at the same
   path (size and modification time change, path and form do not) and
   every item is still skipped, a decision
   use of the judge check (part 5) and the resolved worktree path (part
   6); a control row changes nothing and asserts every item is skipped),
   `a_judge_turned_from_off_to_enforce_reruns_every_item` (the review's
   reproducer: items accepted with a note while the judge use was off;
   the machine's file then enforces it; the next run works every item),
   `an_instruction_only_change_reruns_every_item` ("produce summaries",
   then "translate each summary into French", nothing else changed),
   `a_worktree_only_change_reruns_every_item` (a script list in
   `worktree: a`, then a version whose only change is `worktree: b`),
   `a_sensitive_param_enters_only_as_a_keyed_tag` (for a declared secret, a
   plain param with a secret-shaped name and a plain param holding a
   credential-shaped value, the serialized `WorkInputs` holds the MAC,
   never the value or its bare hash; a run without an installation key
   skips nothing with `skip_no_key` recorded on `list_started`, and no key
   is created),
   `a_supervisor_note_before_the_visit_skips_nothing` (same version and
   params as the previous success; a supervisor note is applied before
   the list's first visit, and before the second visit of a loop in a
   second case: the visit's `list_started` records
   `skip_supervisor_note`, every item works and carries no skip digest),
   `a_worktree_from_a_node_output_never_skips` (the tree comes from an
   earlier node's output with a new path each run: every item works in
   both runs),
   `a_visit_after_a_scripts_rewrite_skips_nothing` and
   `a_scripts_rewrite_mid_visit_stamps_no_digest` (an earlier node, or a
   parallel one during the visit, rewrites `run_dir/scripts`),
   `two_concurrent_entries_record_one_group` (a turn's entry and a
   resume's drive race `ensure_recorded` on one run, each on the
   required lock: one group of `list_skip_read` lines, the second writer
   appends nothing),
   `a_journal_lock_failure_refuses_the_set_write` (a unit test in
   `list/skip.rs`: the test holds the journal's lock on a second open
   file and calls the public `write_sets`; the write waits its bounded
   time, is refused `list_skip_lock_unavailable`, and leaves the journal
   unchanged; after the lock is released the same write succeeds) and
   `a_full_disk_is_not_a_lock_failure`,
   `a_program_only_change_reruns_every_item` (only the machine's
   `agents.<id>.program` changes, to another binary with the same
   version and form: the recorded canonical executable differs, every
   item runs again),
   `a_cli_to_host_change_changes_the_work_fingerprint` (the previous success ran in
   `cli` mode, the next run in `host` mode with every other input equal:
   every item runs again),
   `a_crash_after_list_started_replays_the_recorded_skip_decision` (the
   drive dies right after `list_started`, before the opening steps; on
   resume `complete_opening` skips exactly what the recorded decision
   allows),
   `a_crash_then_scripts_rewrite_fails_the_visit` (the drive dies right
   after `list_started` with skipping allowed; another node rewrites
   `run_dir/scripts`; on resume the pin check before the skip
   settlements fails the visit `list_skip_scripts_changed` and no item is
   settled as skipped),
   `a_crashed_script_only_eval_reads_no_history` (a script-only eval run
   without a manifest that crashed before its set write: `RunConfig.eval`
   decides before any manifest classification, so the drive entry
   records empty sets, no history read, no refusal),
   `a_crashed_run_with_an_unknown_origin_reads_no_history` (a top-level
   run, neither eval, child nor scenario, whose manifest is absent or
   whose origin does not read, crashed before its set write: the drive
   entry records every set `refused: skip_run_type_unknown`),
   `the_skip_reads_the_run_the_window_recorded`,
   `a_skipped_item_carries_its_stamp_to_the_next_run`,
   `a_changed_item_runs_again`,
   `a_changed_field_outside_the_list_is_still_skipped`,
   `a_continuation_after_a_cut_between_set_lines_is_refused` (a
   predecessor with two skipping lists whose journal is cut after the
   first complete `list_skip_read` line: `continued_from` it is refused
   `list_skip_incomplete` and leaves no run; a predecessor whose
   definition declared no skip still continues with empty sets),
   `a_script_list_without_a_manifest_skips` (a script list node with no
   connector started without a manifest: its items carry skip digests
   and the next run skips them),
   `a_node_that_needs_a_manifest_refuses_the_skip_without_one` (the
   visit's `list_started` records `skip_refused: skip_needs_manifest`,
   the set itself carries no refusal, every item works),
   `a_patched_run_stamps_the_patched_definition`,
   `a_patch_adding_a_skip_is_refused`,
   `a_patch_after_skips_is_refused` (after skip settlements any patch,
   a description-only one included, is refused
   `list_skip_work_changed`; the definition snapshot is unchanged),
   `a_patch_before_any_skip_is_applied_and_skips_nothing` (a patch before
   any skip settlement is applied as on main, and the next visit skips
   nothing),
   `a_rebind_of_a_node_with_skipped_items_is_refused`,
   `a_rebind_changing_only_the_executor_is_refused` (the candidate profile
   differs from the bound one only in its invocation specification),
   `a_rebind_changing_only_the_program_path_is_refused` (the candidate
   differs only in the canonical executable of one invocation),
   `a_rebind_whose_work_cannot_be_compared_is_refused` (the scripts copy
   cannot be digested: `list_skip_work_unknown`),
   `a_resume_keeps_the_journaled_set`,
   `a_queued_run_reads_its_set_after_the_turn`,
   `a_queued_run_whose_read_fails_ends_failed`,
   `a_keyed_start_whose_read_fails_leaves_no_run`,
   `a_goal_over_a_list_counts_skipped_items_as_done`,
   `an_unreadable_earlier_run_refuses_the_start`,
   `a_child_run_skips_nothing`,
   `a_skipped_item_is_never_in_the_pilot_slice`,
   `stop_when_judges_only_the_items_worked`,
   `the_second_loop_visit_decides_skipped_rows_again` (a loop over a list
   with `rerun: not_passed`: the second visit carries only the rows worked
   and passed in the first, skips the earlier skipped rows again from the
   recorded set with `skipped_from`, and its early-stop share counts no
   skipped row while a carried row counts as on main),
   `a_note_between_visits_makes_the_skipped_item_work` (a supervisor note
   applied between the two visits: the second visit records
   `skip_supervisor_note` and works the item the first visit skipped),
   `a_scripts_change_after_the_second_list_started_fails_before_any_skipped_row`
   (another node rewrites `run_dir/scripts` after the second visit's
   `list_started`: the pin check fails the visit
   `list_skip_scripts_changed` before any skipped row is written),
   `a_carried_item_after_restored_scripts_gets_no_stamp` (the
   interleaving: the first visit runs with changed scripts and refuses
   skipping, an item runs and passes without a stamp; the pinned scripts
   are restored before the second visit, which carries that item as on
   main with no stamp; the next run works it),
   `a_drive_that_died_after_list_started_skips_the_same_items`,
   `a_queued_run_paused_before_its_turn_records_its_sets_when_resumed`
   (paused while queued, resumed: the drive entry records every set in
   one write before the first node),
   `a_crash_before_the_set_write_records_them_at_the_next_drive` (the
   journal cut right after `run_guard`, then `apb resume`, and the same
   from a named node with `--from-node`),
   `a_partly_recorded_run_refuses_to_drive` (one node's line removed by
   hand: the resume refuses `list_skip_incomplete` and appends nothing
   but its failure),
   `each_list_node_journals_its_own_set`,
   `the_output_and_the_view_carry_the_skipped_count`,
   `a_plain_list_run_journals_what_main_journals` (a list without
   `skip`, normalized as `external_data_test.rs` `normalized`, line
   329, normalizes a journal, with the temporary root replaced too,
   equals `tests/fixtures/journal/plain_list_run.jsonl`; with
   `APB_WRITE_FIXTURES=1` it writes the fixture instead, which task 0.6
   does once on the unmodified tree).
10. `crates/apb-cli/src/run_items.rs` `mod tests` (line 137):
   `a_skipped_item_names_its_run`.
11. `crates/apb-cli/tests/suite/journal_corpus_test.rs` row L1:
    `l1_list_skip_old_binaries_refuse_start_and_resume` (beta.1 refuses
    the definition at parse; 0.25.0, which predates list mode, drops the
    whole list block and runs the step once, skipping nothing; neither resumes a run of the new binary, and
    the run's guard names `list_skip`) and
    `l1_list_skip_old_binaries_stop_but_never_pause`
    (the window right after the set write, on a prepared run nothing
    drives: the case prepares the run in process through
    `apb_engine::scheduler::prepare_supervised_background`, as
    `baseline_test.rs` line 554 does, under the case's project and config
    dir; a start node finishes at once once driven, `entry.rs` line 765
    and `node.rs` line 915, so a gate cannot hold that window. The order
    is fixed, because the corpus restore deletes and recreates the runs
    directory (`journal_corpus_test.rs` line 556) while a prepared run's
    `EventLog` keeps its journal file open (`event.rs` line 2826): (1)
    capture the pre-checkpoint snapshot with the `PreparedRun` held, so
    the journal ends with the group of `list_skip_read` lines and no
    checkpoint after them; (2) consume the `PreparedRun` with
    `drive_prepared` until a `human_review` gate after the list waits,
    so a `node_finished` and its run guard follow the sets, and capture
    that gate snapshot; (3) only then restore either snapshot, on a
    fresh copy for each operation, for the released readers. On both
    snapshots both readers read the run, refuse to pause it (before the
    checkpoint the unknown `list_skip_read`, after it the guard's unknown
    `list_skip` name) and append nothing, and stop it with
    `run_aborted`).
12. `web/src/lib/listprogress.test.ts`: `the label names skipped items`;
    `web/src/lib/ListProgressPanel.test.ts`: `a skipped row links its
    earlier run`; `web/src/lib/journal.test.ts`: `a skip read line
    names the node, the items and the run`.

## Code map

One row per changed file.

| # | File | Decision | Change |
|---|---|---|---|
| 1 | `crates/apb-core/src/schema.rs` | D3 | `SessionMode`, `ContinueSession`, its serde, the field type |
| 2 | `crates/apb-core/src/list/mod.rs` | D2 | `ListSkip`, `PassedIn`, the two limits, `LIST_OUTPUT_FIELDS` |
| 3 | `crates/apb-core/src/list/items.rs` | D2 | `fields_digest` |
| 4 | `crates/apb-core/src/validate/list.rs` | D2, D3 | V283; `.from()` |
| 5 | `crates/apb-core/src/validate/mod.rs` | D2 | the V283 comment on the `check_lists` call |
| 6 | `crates/apb-core/src/validate/guarded.rs` | D2 | `ListSkip`, the count tests |
| 7 | `crates/apb-core/src/validate/node_io.rs` | D3 | the V45 race rule; `.from()` |
| 8 | `crates/apb-core/src/validate/decisions.rs` | D3 | `.from()` |
| 9 | `crates/apb-core/src/preflight.rs` | D3 | the fork cold line |
| 10 | `crates/apb-core/src/untrusted.rs` | D3 | `.from()` |
| 11 | `crates/apb-core/src/agents/forms.rs` | D3 | `Forms.fork`, `fork_argv` |
| 12 | `crates/apb-core/src/agents/check.rs` | D3 | fork flags in `form_flags` |
| 13 | `crates/apb-core/src/agents/claude/mod.rs` | D3 | the fork form |
| 14 | `crates/apb-core/src/agents/opencode/mod.rs` | D3 | the fork form |
| 15-22 | `crates/apb-core/src/agents/codex/mod.rs`, `zcode/mod.rs`, `dsh/mod.rs`, `agy.rs`, `cursor.rs`, `grok.rs`, `hermes.rs`, `qoder.rs` | D3 | `fork: None` (8 files) |
| 23 | `crates/apb-core/tests/suite/validate_node_io_test.rs` | D3 | V45 and preflight fork tests |
| 24 | `crates/apb-core/tests/suite/list_schema_test.rs` | D2 | V283 tests |
| 25 | `crates/apb-core/tests/suite/agent_check_test.rs` | D3 | the fork flags and the help warning |
| 26 | `crates/apb-core/src/detect.rs` | D3 | `fingerprint` made public for the fork help cache |
| 27 | `crates/apb-engine/src/list/skip.rs` (new) | D2 | `SkipSet`, `SkipView`, `read_sets`, `empty_sets`, `recorded`, `coverage`, `ensure_recorded`, `skip_digest`, `patch_refusal`, `view` |
| 28 | `crates/apb-engine/src/list/mod.rs` | D2 | `pub mod skip;`, re-exports |
| 29 | `crates/apb-engine/src/list/ledger.rs` | D2 | `ItemRecord` fields, the fold, `ListCounts.skipped`, `skipped_ids`, `progress_label`, `ListFold::visits` |
| 30 | `crates/apb-engine/src/list/view.rs` | D2 | `RunList.skip`, `ListProgress.skipped`, `RunListItem.skipped_from` |
| 31 | `crates/apb-engine/src/scheduler/list/mod.rs` | D2 | `skip_passed` and the visit's refusal in `opening_steps`, `carry_passed` leaves skipped rows out, `compose` |
| 32 | `crates/apb-engine/src/scheduler/list/run.rs` | D2 | `ListRun::work`, `Settlement` fields, `skip_digest` in `settle_stamped` |
| 33 | `crates/apb-engine/src/scheduler/list/batch.rs` | D2 | skipped ids in the stop rule's `outside` |
| 34 | `crates/apb-engine/src/scheduler/cache.rs` | D2 | `work_inputs`, `start_request_of` and `work_fingerprint` over the six parts of D2 "What `WorkInputs` covers" |
| 35 | `crates/apb-engine/src/scheduler/prepare.rs` | D2 | read (unkeyed, or after an at-once admission), the decided group write, the scripts pin for a skip definition |
| 36 | `crates/apb-engine/src/scheduler/entry.rs` | D2 | `after_the_turn` reads a queued run's sets, failing like the window |
| 37 | `crates/apb-engine/src/scheduler/patch.rs` | D2 | the `list_skip_unrecorded` refusal and the unconditional `list_skip_work_changed` refusal after skips |
| 38 | `crates/apb-engine/src/scheduler/rebind.rs` | D2 | `list_skip_work_changed` and `list_skip_work_unknown` for a rebind, the candidate's part 4 with its resolved invocations |
| 39 | `crates/apb-engine/src/scheduler.rs` | D2 | `ensure_recorded` at drive entry |
| 40 | `crates/apb-engine/src/probation.rs` | D2 | `skip_lists_worked` in `entry_of` |
| 41 | `crates/apb-engine/src/run_outcome/lists.rs` | D2 | `ListOutcome.skip`, the line |
| 42 | `crates/apb-engine/src/event.rs` | D2, D3 | `ListSkipRead`, `ListItemSettled` fields, `ListStarted.skip_refused`, `SessionHandoff.mode`, `append_group_decided` |
| 43 | `crates/apb-engine/src/state.rs` | D2 | `ListSkipRead` in the list no-op arm |
| 44 | `crates/apb-engine/src/invocation.rs` | D3 | `fork_argv`, `fork_spec`, `session_spec` |
| 45 | `crates/apb-engine/src/scheduler/handoff.rs` | D3 | `decide` with the mode and the help check, `Handoff::Warm.fork`, the cached `fork_help` |
| 46 | `crates/apb-engine/src/scheduler/node.rs` | D3 | `KnownSession::Id.fork`, the re-entry fork, the help probe, the spec, the prompt, the session left, the journaled mode |
| 47-51 | `crates/apb-engine/src/scheduler/result.rs`, `scheduler/attempt_memory.rs`, `scheduler/approval.rs`, `result_view.rs`, `connector/call/attempt.rs` | D2 | the two new `ListItemSettled` fields as `None` (5 files) |
| 52 | `crates/apb-engine/tests/main.rs` | D2, D3 | two test modules |
| 53 | `crates/apb-engine/tests/suite/list_skip_test.rs` (new) | D2 | the skip end to end |
| 54 | `crates/apb-engine/tests/suite/session_fork_test.rs` (new) | D3 | the fork end to end, the re-entries |
| 55 | `crates/apb-engine/tests/suite/session_handoff_test.rs` | D3 | `..` in the one exhaustive `SessionHandoff` pattern |
| 56 | `crates/apb-engine/tests/fixtures/journal/known_events.jsonl` | D2, D3 | three lines |
| 57 | `crates/apb-engine/tests/fixtures/journal/plain_list_run.jsonl` (new) | D2 | the plain list journal recorded on main |
| 58 | `crates/apb-cli/src/run_items.rs` | D2 | the skipped row, the skip line |
| 59 | `crates/apb-cli/tests/suite/journal_corpus_test.rs` | D2, D3 | rows L1 (two cases) and F1 |
| 60 | `crates/apb-server/src/ts_contract.rs` | D2 | `SkipView` |
| 61 | `crates/apb-server/tests/suite/runs_api_test.rs` | D2 | the two new `ListItemSettled` fields as `None` |
| 62 | `web/src/lib/api.gen.ts` | D2 | generated |
| 63 | `web/src/lib/ListProgressPanel.svelte` | D2 | skipped count, skip line, the earlier run link |
| 64 | `web/src/lib/ListProgressPanel.test.ts` | D2 | `a skipped row links its earlier run` |
| 65 | `web/src/lib/listprogress.ts` | D2 | the label, `skipLabel` |
| 66 | `web/src/lib/listprogress.test.ts` | D2 | `the label names skipped items` |
| 67 | `web/src/lib/NodePanel.svelte` | D3 | both shapes, the "fork instead of continue" checkbox |
| 68 | `web/src/lib/nodefields.ts` | D3 | both shapes of `continue_session` |
| 69 | `web/src/lib/nodefields.test.ts` | D3 | `continue_session reads both shapes` |
| 70 | `web/src/lib/NodeSummary.svelte` | D3 | "<from> (fork)" |
| 71 | `web/src/lib/journal.ts` | D2, D3 | the forked handoff line, the skip read line |
| 72 | `web/src/lib/journal.test.ts` | D2, D3 | two tests |
| 73 | `docs/HOWTO-authoring.md` | D2, D3 | the `skip` subsection, the fork |
| 74 | `docs/GUARDRAILS.md` | D2 | the guard name `list_skip`, the pause window, the trust sentence |
| 75 | `docs/superpowers/specs/<date>-incremental-lists-and-session-fork-design.md` (new) | records | public record |
| 76-77 | `CLAUDE.md`, `AGENTS.md` | records | module map |
| 78-82 | `openspec/changes/incremental-lists-and-session-fork/proposal.md`, `design.md`, `tasks.md`, `specs/incremental-lists/spec.md`, `specs/session-fork/spec.md` | records | this change (5 files) |

Count: 82 changed files (26 in `apb-core`, 25 engine sources, 6
engine tests and fixtures, 2 in `apb-cli`, 2 in `apb-server`, 11 in
`web`, 2 docs and 8 records), about the target of 80. Against the
closure pass (76): `previous_success.rs` left the map (no shared
iterator), and `scheduler/cache.rs`, `scheduler/patch.rs`,
`probation.rs` and the plain-list fixture joined it (79); the third
pass added `scheduler.rs` (the drive-entry recording) and
`scheduler/rebind.rs` (the rebind refusal) (81); the ninth pass added
`crates/apb-core/src/detect.rs` (its `fingerprint` made public for the
fork help cache) (82). Not touched:
`crates/apb-engine/src/previous_success.rs` (`find` is called as it
is), `crates/apb-core/src/agents/pi.rs` (the trait's `None`),
`crates/apb-core/src/doctor/agents.rs` (the doctor's check is called as
it is),
`crates/apb-core/src/cache.rs` (the cache key as it is), `crates/apb-engine/src/run_definitions.rs`, `crates/apb-core/src/run_origin.rs` and `crates/apb-engine/src/decision/mod.rs` (called as they are), `crates/apb-engine/src/run_scripts.rs` (`digest` as it is), `crates/apb-engine/src/scheduler/resume.rs` (it reaches `drive`),
`crates/apb-engine/src/run_outcome/summary.rs` (it reads
`ListOutcome::line`), `crates/apb-engine/src/scheduler/list/pilot.rs`
and `batch.rs`'s queue (`fold.remaining` already leaves skipped items
out), `crates/apb-engine/src/pause/request.rs` (the existing rule),
`crates/apb-mcp/src/tools/run.rs`, `crates/apb-server/src/routes/runs.rs`,
`crates/apb-cli/src/run.rs` and `web/src/lib/consts.gen.ts`.

## Release notes

1. A list node can skip items that passed in the previous successful
   run (`list.skip`). Any new playbook version, changed param or
   instruction, changed profile, connector account or decision setting,
   changed script or working tree makes every item run again once; the
   fields you name in `unless_changed` decide when an item itself counts
   as changed. Skipped items count as passed and are shown as skipped.
2. `continue_session: { from, mode: fork }` starts from a copy of another
   node's session; forks of one session may run at once (claude and
   opencode; other agents start cold with the reason).

## Implementation notes

1. Run `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract` after
   every change to a mirrored type (`RunList`, `RunListItem`,
   `ListProgress`, `ListOutcome`, `SkipView`), then the same command
   without the variable.
2. Change the type of `continue_session` first and let the compiler list
   every site (`cargo build --workspace`), then fix each with `.from()`.
   Do the same for the two `ListItemSettled` fields.
3. Never run a real agent or reach a real service in a test: the stub
   agents of the existing suites are the only executors.

## Review record

### Closure pass

Closure pass of 2026-11-20, on main efae5822, with the designs of
`eval-doubles` and `inbox-waits` read for overlap. Each item is a claim
of the draft checked against the code, or a question the draft left
open, with what this pass decided and why.

1. Base moved from 316c8180 to efae5822: every path, line and symbol
   re-read there; the "(to be re-verified after ... merges)" markers are
   gone, because `before-running-alone` (#289) and `wait-park` (#290)
   are merged and read, and the draft's `evals-and-inbox-waits` was split
   into `eval-doubles` and `inbox-waits`, whose designs are read.
2. `is_checkpoint` was described as an exhaustive match needing a
   `false` arm: it is a `matches!` over five variants (event.rs line
   3820); no change there.
3. The known-events fixture holds no `session_handoff` line; the draft's
   "second line" became three appended lines after the merged main's
   last one.
4. `previous_success::find` is `pub(crate)`, not `pub`; the split now
   keeps classification lazy, because a run older than the success must
   never be reached (an existing test pins it). Superseded by item 27: the split was cut; `find` is called as it is.
5. `any_success` counted runs still active; decided: only runs that
   ended (`settled_outcome`), since an active run may still be revived
   and its journal is being written. Superseded by item 27: `any_success` was cut.
6. The draft applied the skip inside `open_visit` beside
   `carry_passed`; on main the opening steps moved into
   `opening_steps` with `complete_opening` as their crash recovery
   (#289). Decided: the skip is one more opening step, so the existing
   recovery covers it with no code of its own.
7. The skip settlement was a hand-built `ListItemSettled`; decided:
   `ListRun::settle` with `ran: None`, the path every settlement takes.
8. `stop_when` was not considered: decided that skipped ids join the
   excluded ids the stop rule already takes for the pilot slice, so the
   share is over items worked.
9. Pilot: confirmed that the slice is taken from `fold.remaining`, so a
   skipped item is never in it; no change.
10. A supervisor patch that adds or changes `skip` was not considered:
    decided that the set records its rule and applies only while the
    node's rule is equal; a patch makes the visit skip nothing, which
    works more, never less. Superseded by item 29: the rule is no longer recorded; the skip digest covers the node definition, so a patch that changes the node skips nothing, and a patch that adds a skip is refused.
11. The set had no size bound: decided `LIST_SKIP_MAX_ENTRIES` (1000 per
    node, newest decided kept, `truncated` shown) and run indexes
    instead of repeated run ids, so the one journal line stays under 256
    KiB. Superseded by item 36: one bounded line per node.
12. A skipped item's output: decided `null` with `skipped_from`, since
    carrying 2 KiB outputs for up to 1000 items would make the line
    unbounded. Superseded by Fix round 3 M1: the output is carried within
    the set's existing byte cap.
13. `ListItemSettled` is built with every field in eight more files than
    the draft listed; added to the code map.
14. `ListCounts`, `RunList` and `ListOutcome` lines and names re-read;
    the separate `skipped` and `skip_read` fields became one `skip`
    view shaped like `pilot`; `run_outcome/summary.rs` needs no change
    (it reads `ListOutcome::line`). The view is `{ skipped, read_items, source, truncated }` after item 27.
15. Validator: V283 confirmed free on efae5822 and named as this
    change's by both in-flight designs; no other code needed.
16. Guard count: 13 on efae5822, 14 after `eval-doubles`, 15 here; the
    count tests are named.
17. Guard reason: the draft said an older apb "drops the block"; on
    main the list block denies unknown fields since 1.0.0-beta.1, so the
    guard is for 0.25.0 (which drops it), as `docs/GUARDRAILS.md` says
    for `list_pilot`; corpus row L1 proves it.
18. The fork carries no guard: an older apb cannot parse the object
    form; corpus row F1 proves it.
19. The fork's session: on main a failed resumed attempt falls back to
    the continued id (node.rs lines 2767 and 2808), which for a fork is the
    source; decided that a fork never takes that fallback, so a retry
    either continues the fork's reported id or forks again. Extended by item 25: within one attempt loop only; every re-entry forks the source again.
20. `untrusted.rs` is at line 177, not 218; `node.rs` sites re-read
    (493, 959, 1472, 1501, 2066); V45 tests and the preflight test live
    in `tests/suite/validate_node_io_test.rs`, not in the source module,
    which has no tests; the same for V283 (`list_schema_test.rs`) and
    `form_flags` (`agent_check_test.rs`).
21. `resume_spec` has five test callers; decided to keep its signature
    and share a private `session_spec`.
22. Tests in `src/list/view.rs` do not exist; the view is covered by the
    integration test and the ledger and outcome unit tests.
23. File count: 63 in the draft; 76 after adding the omitted
    constructions, the corpus rows, `GUARDRAILS.md`, `batch.rs`,
    `validate/mod.rs` and the test suites; within the target of about 80,
    so no cut. Superseded by item 38 below: 79.
24. Dashboard: the draft gave the skip line to the list panel only;
    decided that the journal view also prints `list_skip_read`, since it
    prints every journaled read.

### Second pass

Second pass of 2026-11-20, over the design review of the closure pass
(two blockers, eight major, three minor findings and one pre-existing
note), each finding verified against the code on main efae5822 before
its disposition. Line numbers are those of main.

25. Blocker, fork retries lose the fork session after a deferred retry.
    Verified: the session map is built per execution (`scheduler/node.rs`
    line 1456); a deferred retry returns from the attempt loop
    (`AttemptOutcome::Deferred`, lines 1802 to 1816) and comes back with a
    continuation, which skips the handoff block (line 1472), so
    `resolve_session` (line 1842) finds nothing. Accepted with another
    remedy than the review's: no fork id is persisted. Within one attempt
    loop a retry continues the fork's reported id; on every re-entry
    (a deferred retry, a pause, a driver death, a resume) the next attempt
    forks the source again, which a fork never writes into, and the retry
    briefing tells it what failed (D3 "Re-entry"). Tests: delayed retry,
    pause and resume, driver death.
26. Blocker, ordinary list runs change their journal. Verified: `compose`
    builds the output (`scheduler/list/mod.rs` lines 388 to 395) that
    `node_finished` stores. Accepted: `skipped` is written only for a list
    that declares `skip`, and `a_plain_list_run_journals_what_main_journals`
    compares a plain list run with a fixture recorded on main.
27. Major, `any_success` can choose an older settlement. Verified: the
    candidates are sorted by `run_started` time (`previous_success.rs`
    lines 83 to 85). Resolved by a cut: `any_success` is gone; the skip
    reads exactly the one run `run.previous_success` resolves, through
    `find` as it is or the window the run recorded, and a skipped item
    carries its stamp so the chain continues through each next success.
    The shared iterator, `settled_outcome`, the ordering rules and their
    tests are cut; nothing else needed them.
28. Major, `on_version_change: reset` uses the wrong version for a patched
    run. Verified: `RunHead.version` comes from `run_started`
    (`run_history.rs` lines 39 to 48) and a patch replaces the definition
    later (`scheduler/patch.rs` lines 259 to 275). Resolved by a cut:
    `on_version_change` is gone; the skip digest is computed from the
    definition in force when the item settles, so a patched run needs no
    rule.
29. Major, a pass is reused after the action or check changes. Accepted:
    the skip digest covers the item and a work fingerprint built from the
    node cache key's parts (`scheduler/cache.rs` lines 254 to 312 and 334)
    plus what the key lacks (the item check's and `success_check`'s
    script contents, the check site's connectors); any change makes every
    item run again (D2 "The skip digest"). Revised by items 43 to 45.
30. Major, history journal edits can forge skip evidence. Verified: the
    candidate check is the origin stamp (`previous_success.rs` line 60,
    `created_here`), a MAC over the run id and the manifest only
    (`crates/apb-core/src/run_origin.rs` lines 63 to 75), not over the
    journal. Rejected: the source run is chosen by the same stamp and
    checks `run.previous_success` already uses on main to steer a run from
    an earlier journal, and files under the operator's own account are
    inside the trust boundary, as `docs/GUARDRAILS.md` says for every
    other apb guard (line 406, a same-user writer stays out of scope). One
    sentence in the docs task says so.
31. Major, a skipped item loses its origin on the next loop visit.
    Verified: `opening_steps` runs `carry_passed` first (`scheduler/list/mod.rs`
    lines 183 to 188), whose settlement names its fields (lines 325 to
    338), and `stop_when` excludes only the pilot slice (`batch.rs` lines
    115 to 119). Accepted: `carry_passed` keeps `skipped_from` and
    `skip_digest`; the early-stop share excludes every carried or skipped
    item; the second loop visit is tested. Revised by items 40 and 45. Superseded by item 88: a skipped row is never carried; each visit decides it again.
32. Major, no-work runs can satisfy probation. Verified: `entry_of`
    (`probation.rs` lines 195 to 208) judges outcome, mode, starter,
    definition and overrides only. Accepted: a run in which a list that
    declares `skip` worked no item is not eligible; readiness is
    unchanged. Revised by item 46.
33. Major, an older binary cannot always pause a new skip run. Verified:
    `request_pause` reads with the strict `read_all` under the event lock
    (`pause/request.rs` lines 73 and 74), which refuses an unknown event no
    checkpoint follows (`event.rs` lines 3922 to 3925 and 3974 to 3988);
    stopping reads through `read_all_for_stop` (line 3939). Accepted: the
    table now says an older apb reads and stops such a run at any time and
    pauses or resumes it only by the existing unknown-event rule, never
    resuming it because of the guard; the immediate window is a corpus
    case. Revised by item 56: an older apb refuses to pause such a run
    after a checkpoint too.
34. Major, fork support is left to the implementer's CLI. Accepted: the
    forms are fixed in D3 from the help of claude 2.1.327 and opencode
    2.2.5; an installed CLI without the flag is reported by the doctor's
    existing check and starts cold with a journaled reason; the build-time
    fallback of task 0.6 is removed. Refined by item 41.
35. Minor, the queued history failure contract contradicts itself.
    Verified: the window is read before `allocate_run` (`prepare.rs` lines
    1078 to 1115 against line 1227), queueing is decided by
    `concurrency::admit` after it (line 1252), the window is cleared for a
    queued run (line 1289) and read after the turn (`entry.rs` lines 245
    and 738). Accepted: no read in preparation for a start that queues;
    an unkeyed start is refused before the run directory exists; a keyed
    start admitted at once reads right after its admission and is refused
    through the refused-admission cleanup, leaving no run (whether a keyed
    start queues is known only once the run directory exists); a queued
    run whose read fails after its turn ends aborted with
    `list_skip_unreadable`. Revised by item 39: it ends failed; the read
    placement is fixed by item 49.
36. Minor, the journal-line bound is per node but the event held every
    node. Accepted: one `list_skip_read { node, set }` per list node, each
    capped by entries and bytes, with `truncated`. All lines of a run in
    one write since item 42.
37. Minor, the compatibility gate can pass without testing. Verified: the
    corpus passes a missing binary unless `APB_CORPUS_REQUIRED=1`
    (`journal_corpus_test.rs` lines 270 to 282), which CI's job sets
    (`.github/workflows/ci.yml` lines 256 to 258). Accepted: tasks 3.9 and
    5.10 run it that way, so a missing binary fails.
38. Pre-existing on main: two runs started without a shared concurrency
    key can work the same item at once; an ended-history lookup cannot
    serialize them. Recorded, no change: the `concurrency:` key is the
    existing tool for it, and the docs say a skip reads ended runs only.
    The file count is 79 after this pass (Code map).

### Follow-up decisions of the second pass

Three follow-up decisions of 2026-11-20 on the second pass.

39. A queued run whose skip read fails after its turn ends failed,
    exactly as a failed `run.previous_success` lookup does on main
    (`close_out_unclaimed`, `scheduler/entry.rs` lines 401 to 407:
    `run_error` with the reason, then `run_finished failed`), with the
    named reason `list_skip_unreadable`: one outcome for both reads. The
    sets block no longer appends `run_aborted`; the test is
    `a_queued_run_whose_read_fails_ends_failed`.
40. `stop_when` keeps main's behaviour for plain lists: the share
    excludes only the pilot slice and items carrying `skipped_from`
    (skipped in this visit or carried with skip provenance, through
    `ListFold::skipped_ids`); items carried by `rerun: not_passed` count
    as on main. The plain-list behaviour-change note and its release-note
    line are removed, and `carried_ids` is not added.
41. The run-time help probe of a fork form runs at most once per process
    and agent binary: `handoff::fork_help` caches the answer in process
    memory (`static FORK_HELP` in `scheduler/handoff.rs`, keyed by agent
    id and canonical program path, a failure cached too), never per
    execution and never on disk. A probe failure means a cold start with
    the journaled reason. Test
    `the_help_probe_runs_once_per_process_and_binary`. No file is added.

### Cut in the second pass

1. `passed_in: any_success`, `LIST_SKIP_MAX_RUNS`, the shared
   `previous_success::candidates` iterator with `Candidate` and
   `settled_outcome`, the ordering rules, and the tests
   `candidates_lists_newest_first_and_find_is_unchanged`,
   `settled_outcome_reads_an_abort_and_a_reopen`,
   `any_success_reads_failed_runs_too` and
   `a_newer_needs_look_hides_an_older_pass`, with the spec scenario "A
   newer needs look". `previous_success.rs` is no longer changed.
2. `on_version_change` (`OnVersionChange`, `keep`, `reset`), the
   candidate's version, `reset_on_version_change_ignores_older_versions`
   and the spec sentence on it.
3. `SkipSet.rule`, `SkipSet.runs` and `SkipEntry` (run index and
   digest): one source run, and the skip digest covers the rule.
4. The one `list_skip_read { sets }` for every node: one line per node.
5. The early read of a keyed start and its clear at `prepare.rs` line
   1291 (`list_skip = None`).
6. A continued run's fresh history read when its predecessor recorded no
   set: such a node gets an empty set.
7. Task 0.6's run of the agent CLIs and its "compile as `None`" fallback.
8. `SkipView.read_runs` (replaced by `source`).
9. The `skipped` output field on every list (now only with `skip`).

### Third pass

Third pass of 2026-11-20, over the second design review (no blocker, six
major, two minor findings, two pre-existing notes), each finding verified
against the code on main efae5822.

42. Major, missing skip-set records silently disable skipping after
    recovery. Verified: a resume with an explicit target returns at once
    (`scheduler/resume.rs` lines 112 to 117) and reaches `drive` (line
    1305) without `after_the_turn`; a queued run paused before its turn
    reserves its slot on resume (line 1225) and also goes straight to
    `drive`; the second pass appended one line per node. Accepted: every
    line of a run goes out in one write through `EventLog::append_group`,
    the general form of `append_pair` (`event.rs` line 2955, used by the
    form gate, `scheduler/review_gate.rs` line 206); a run that declares
    `skip` and has none recorded when a drive begins reads and records
    them in `drive_inner` (`scheduler.rs`, `drive` at line 463) through
    `list::skip::ensure_recorded`, the function `after_the_turn` uses; a
    partial record (a write torn after a complete line) is refused
    `list_skip_incomplete`, never filled with an empty set. Tests in
    tasks 3.5 and 3.6. Revised by item 70: the group write is one decision under the append lock.
43. Major, valid script-only runs never get a skip digest. Verified: main
    writes no manifest when it is empty (`scheduler/prepare.rs` line 1580,
    `manifest.rs` line 706), and an unchecked direct start is allowed for a
    playbook agents may launch (`prepare.rs` lines 1809 to 1816).
    Accepted: the fingerprint reads the manifest only for an agent_task
    or a node that binds a connector; without a needed manifest the set
    records `refused: skip_needs_manifest`, the items work.
44. Major, changed script dependencies are missed. Verified: scripts get
    the run directory (`script.rs` lines 82 and 131) and can source any
    file under `scripts/`. Accepted: the fingerprint uses the whole
    scripts-tree digest `run_scripts::digest` (`run_scripts.rs` line 13)
    in place of single script hashes; test with a helper-only change.
45. Major, carried skip provenance bypasses a changed skip rule. Verified:
    `carry_passed` checks only the item's content (`scheduler/list/mod.rs`
    line 320) and an open visit keeps its terminal rows (line 137).
    Accepted: a patch or rebind that changes the skip rule or the work
    fingerprint of a list node with skip-derived settlements in the run is
    refused `list_skip_work_changed` (`scheduler/patch.rs` next to the
    `list_skip_unrecorded` refusal; `scheduler/rebind.rs` `apply_rebind`
    and `set_routed`). `patch.rs` has no list refusals on main to sit
    beside; the refusal sits next to `previous_success::patch_refusal`
    (line 116). A patch never replaces `scripts/` (`snapshot_playbook`,
    line 269), so `nodes_differ` (line 350) covers the fingerprint.
    Revised by item 52: the refusal compares `work_fingerprint`, which
    covers the defaults, not `nodes_differ`. Superseded by item 61.
46. Major, probation credit without an executed item. Verified:
    `settle_too_large` settles before any batch with `carried: false`
    (`scheduler/list/mod.rs` line 343, `list/run.rs` line 163), and
    `on_needs_look: continue` lets the list succeed (line 405). Accepted:
    credit needs at least one fresh passed settlement admitted in a batch
    per skipping list; the oversized-item case is tested. Script items
    journal no `attempt_started` (only agent steps do, `node.rs` line
    2329), so the batch admission, not an attempt event, is the evidence
    that the step ran. Revised by item 71: only a list that skipped items needs a fresh pass.
47. Major, the deferred fork handoff checks and seeds the wrong executor.
    Verified: the block decides for `steps[0]` (`node.rs` line 1478) while
    a deferred retry resumes `continuation.step` (lines 1549 and 1642).
    Accepted: a deferred entry decides, probes and seeds for the step it
    resumes; test a deferral on a fallback step.
48. Minor, the compatibility-window test cannot reach its state. Verified:
    driving selects the start node (`entry.rs` line 765), which finishes
    at once (`node.rs` line 915) with a `node_finished` checkpoint
    (`event.rs` line 3820). Accepted: the case holds a prepared, undriven
    run (`prepare_supervised_background`) right after the set write, then
    releases it with `drive_prepared` for the post-checkpoint case.
49. Minor, the preparation snippet uses `key` before it exists.
    Verified: `key` is resolved at `prepare.rs` line 1132, after the
    window. Accepted: the read sits after the key resolution and before
    `allocate_run` (line 1227); task 3.6 says so.
50. Pre-existing on main: concurrent runs without a shared key (as item
    38), and a queued run paused before its turn and then resumed never
    fills its `run.previous_success` window (`resume.rs` lines 1225 and
    1305). Recorded, no change to the window; the skip set of such a run
    is recorded at the drive entry (item 42), from `find` when no window
    was recorded. The file count is 81 after this pass (Code map).
51. Follow-up decision: an item restored from the node cache does not
    count as worked for probation credit. Read from the existing
    `node_cache_hit` event (`event.rs` line 1354): a passed settlement
    after a cache hit of the node in the same visit is not fresh. Checked:
    on main a list node never uses the node cache (`scheduler/cache.rs`
    line 85) and list settlements pass `cache_hit: false`
    (`scheduler/list/run.rs` line 124), so the case cannot arise today;
    the rule guards a later change. Test: the third case of
    `a_list_that_worked_no_item_is_not_eligible`. This also closes the
    open point of item 46.

### Fourth pass

Fourth pass of 2026-11-20, over the third design review (two major
findings, two pre-existing notes already recorded as items 38 and 50),
each verified against the code on main efae5822.

52. Major, tightening an effective check does not invalidate passes.
    Verified: the engine turns a node's verdict requirement on from
    `defaults.require_verdict` (`scheduler/node.rs` line 1083) and rejects
    a missing verdict (line 2986), while the fingerprint serialized the
    node alone and the patch refusal compared `nodes_differ`
    (`scheduler/patch.rs` line 350), which sees the node only. Accepted:
    one function, `work_fingerprint` over `work_inputs`, is the only
    comparison of work; the skip digest, the patch refusal and the rebind
    and route refusal all call it, with no `nodes_differ` shortcut. Its
    input adds the playbook's whole `defaults` block as serialized
    (`Defaults`, `crates/apb-core/src/schema.rs` line 953), chosen over a
    hand-kept list so a default added later cannot be forgotten. Tests:
    `a_tightened_default_runs_every_item_again` (across versions) and
    `a_patch_tightening_a_default_after_skips_is_refused`. Superseded by item 60: the definition is hashed whole.
53. Major, inline item changes invalidate every item. Verified: inline
    items live in the list block (`ItemSource.inline`,
    `crates/apb-core/src/list/mod.rs` line 190), and the cache key
    serializes the whole node (`scheduler/cache.rs` line 300). Accepted:
    the fingerprint's node representation empties `list.items.inline`
    and keeps the source configuration, the skip rule and every work and
    check declaration; each item's own data stays in its item part. The
    "any change of the node" wording is narrowed accordingly. Tests:
    `an_added_inline_item_leaves_the_others_skipped` and
    `an_inline_field_outside_unless_changed_is_still_skipped`.

### Fifth pass

Fifth pass of 2026-11-20, over the fourth design review (three major,
one minor finding, three pre-existing notes), each verified against the
code on main efae5822.
 Superseded by item 76: inline items are part of the definition.
54. Major, the work fingerprint omits connector accounts. Verified: the
    run snapshots each account's fields with its own digest
    (`ManifestAccount`, `connector/run.rs` lines 175 to 182, `account_digest`
    at line 181), separate from the connector definition digest the
    cache key collects (`scheduler/cache.rs` lines 356 to 360); a grant
    names its accounts (`ManifestConnectorGrant`, `manifest.rs` line
    290). Accepted: `WorkInputs` takes the resolved grants at the node
    site and at `<node>#check` with the identity and snapshotted digest
    of every granted account. Test:
    `a_changed_account_field_runs_every_item_again`. Kept by item 60 as part 4.
55. Major, a continuation bypasses the partial-record refusal. Verified:
    `continued_sets` gave an empty set to every missing predecessor
    record, and the continuation check covers identity and supersession
    only (`run_lineage.rs` line 106). Accepted: before copying, the
    predecessor's coverage against its own definition is checked; a
    partial record refuses `list_skip_incomplete` before the run
    directory exists; a predecessor with no set at all keeps the
    empty-set fallback. Test:
    `a_continuation_after_a_cut_between_set_lines_is_refused`.
56. Major, the post-checkpoint pause expectation is impossible.
    Verified: every checkpoint is followed in the same write by the run
    guard (`event.rs` line 3090), whose `list_skip` name an older apb
    reads as an unknown event after its latest checkpoint, and its pause
    reads with the strict `read_all` (`pause/request.rs` line 74,
    `event.rs` line 3986). Accepted: the table, the docs text and the
    corpus case now say an older apb refuses to pause a skip run at any
    time and still reads and stops it; the corpus case is
    `l1_list_skip_old_binaries_stop_but_never_pause`. Guard ordering is
    unchanged.
57. Minor, task 3.4 names a type that does not exist yet. Verified:
    `ListSkipRead.set` needs `list::skip::SkipSet`, which task 3.5
    created. Accepted: the new task 3.3a creates the module declaration
    and the state-type skeleton with compiling stubs before 3.4; the
    behaviour stays in 3.6.

Pre-existing on main, recorded with no change: concurrent runs without a
shared key (item 38), a queued run paused before its turn and resumed
leaving its window unset (item 50), and the pilot review and stop text
counting already-carried items among the work remaining
(`scheduler/list/pilot.rs` line 168), which skipped rows inherit. The
file count stays 81.

### Sixth pass

Sixth pass of 2026-11-20, over the fifth design review (one major, one
minor finding, three pre-existing notes already recorded as items 38, 50
and the Fifth pass), each verified against the code on main efae5822.

58. Major, the fingerprint misses changes to the resolved invocation.
    Verified: `agent_key_parts` takes bundle, agent, model and grants only
    (`scheduler/cache.rs` lines 334 to 364), while the machine's
    `agents.<id>.invocation` replaces the form independently of the
    profile (`invocation.rs` `spec_for`, line 150); lists never take the
    cache (`scheduler/cache.rs` line 85), so this is new exposure.
    Accepted: `WorkInputs` folds in the effective profile's existing
    `executor_digest()` (`manifest.rs` line 115, documented at 109: chain,
    invocation specifications, tiers, `ephemeral`); ordinary cache keys
    are unchanged. The design now states in one closed list what
    `WorkInputs` covers and, one reason each, what it leaves out (D2 "What
    `WorkInputs` covers, and what it leaves out"). Test:
    `an_invocation_only_change_runs_every_item_again`. Kept by item 60 as part 4; its closed list is replaced by the one of item 60. Revised by item 68: tier routing is not refused. Revised by item 82: part 4 takes every resolved invocation whole.
59. Minor, the patch-refusal contract lacked its input and its error
    rule. Verified: the declared `patch_refusal(old, new, events)` had no
    run directory, and the caller has one (`scheduler/patch.rs` line
    102). Accepted: it takes `run_dir`; a failure to compute either
    fingerprint refuses the patch before any mutation with
    `list_skip_work_unknown` and the reason (and a rebind or route the
    same way). Test: `a_patch_whose_work_cannot_be_compared_is_refused`,
    with skip-derived settlements in place. The file count stays 81. Superseded by item 61: a patch after skips is refused unconditionally, so the patch refusal computes no fingerprint and takes no run directory; the comparison and its failure refusal remain for a rebind or route.

### Seventh pass

Seventh pass of 2026-11-20, over the sixth design review (three major,
one minor finding, three pre-existing notes already recorded as items
38 and 50 and the Fifth pass). Three passes in a row found one more
input missing from the work fingerprint, so this pass changes the
principle instead of adding inputs: everything the run was configured
with is inside the fingerprint, taken whole; only the contents of the
world are outside (D2 "What `WorkInputs` covers, and what it leaves
out").

60. Major, the decision-model exclusion relied on a false fact.
    Verified: `JudgeCheck` has no model field (`list/mod.rs` line 406);
    the item judge asks the run's decision runner
    (`scheduler/list/check.rs` line 328), whose settings are the
    manifest's snapshot capped by the machine's live file
    (`decision/mod.rs` lines 393 to 402). Accepted under the new
    principle: part 5 of `WorkInputs` is the non-secret effective
    decision configuration (`EffectiveDecisions`, `decisions.rs` line
    398), with every use's off, advising or enforcing mode. Reproducer
    test `a_judge_turned_from_off_to_enforce_reruns_every_item`.
61. Major, explicit run instructions were missing. Verified:
    `RunConfig.instruction` (`run_config.rs` line 158) is appended to a
    list item's prompt after the rendered template (`scheduler/node.rs`
    lines 1022 and 1043). Accepted: part 3 of `WorkInputs` is every param
    value and the instruction; a secret param enters only as
    `run_origin::tag` (`run_origin.rs` line 126), the keyed MAC the start
    key (`start_key.rs` lines 88 and 153) and remembered input
    (`run_memory/store.rs` line 203) use. Reproducer test
    `an_instruction_only_change_reruns_every_item`. With the whole
    definition in the fingerprint, every patch changes it, so a run that
    already has skip-derived settlements refuses every in-run patch
    (`list_skip_work_changed`, now unconditional for such a run), and a
    run without them is patched as on main. Revised by items 65 and 66: part 3 is the start request whole, and every sensitive value is keyed. Revised by item 84: the tag is `tag_if_keyed`.
62. Major, a changed execution target kept passes from the old one.
    Verified: the run's resolved working tree is the working directory of
    script and agent nodes (`scheduler.rs` line 1146), and scripts run
    there (`script.rs` line 133). Accepted: part 1 hashes the whole
    definition, the top-level `worktree` declaration included, and part 6
    is the identity of the resolved tree (its path or `none`), never its
    contents. Reproducer test `a_worktree_only_change_reruns_every_item`.
    The definition is the one the journal names (`run_definitions::digest`,
    `run_definitions.rs` line 43, journaled at `scheduler/prepare.rs`
    line 1597), hashed from the parsed definition with inline items
    emptied, since the journaled digest is over the YAML bytes. Revised by item 76: the definition is the journaled digest itself, inline items included.
63. Minor, the corpus sequence invalidated the held journal handle.
    Verified: the restore deletes and recreates the runs directory
    (`journal_corpus_test.rs` line 556) while the prepared run's
    `EventLog` keeps its file open (`event.rs` line 2826). Accepted: the
    case captures the pre-checkpoint snapshot, consumes `drive_prepared`
    and captures the gate snapshot, and only then restores either
    snapshot for the old readers; task 3.9 states the order.

The file count is 82: `crates/apb-core/src/detect.rs` joins the map for
item 80.

### Cut in the seventh pass

1. The hand-kept node-level inputs of the work fingerprint: the node
   serialized alone with `list.items.inline` emptied, the `defaults`
   block as a separate part, the reuse of the node cache's `KeyParts`,
   `cache_key` and `agent_key_parts` with the fixed `list-skip` basis,
   the separate `check_connectors` fold, and the closed lists of items
   52, 53 and 58; replaced by the six parts of item 60's principle.
2. The per-input regression tests
   `a_changed_script_runs_every_item_again`,
   `a_changed_item_check_runs_every_item_again`,
   `a_tightened_default_runs_every_item_again`,
   `a_changed_helper_script_runs_every_item_again`,
   `an_invocation_only_change_runs_every_item_again` and
   `a_changed_account_field_runs_every_item_again`; replaced by the
   table test `every_configured_input_reruns_every_item` and the three
   reproducers of items 60 to 62. The inline-item tests stay. Superseded by item 76: the cross-version inline tests were removed in the ninth pass.
3. The conditional patch refusal (comparing fingerprints before and
   after a patch), its `run_dir` argument and its
   `list_skip_work_unknown` refusal for patches, and the tests
   `a_patch_changing_a_node_with_skipped_items_is_refused`,
   `a_patch_tightening_a_default_after_skips_is_refused`,
   `a_patch_whose_work_cannot_be_compared_is_refused` and
   `a_changed_skip_rule_skips_nothing`; replaced by the unconditional
   refusal after skips (`a_patch_after_skips_is_refused`) and
   `a_patch_before_any_skip_is_applied_and_skips_nothing`. A rebind or
   route keeps the comparison and `list_skip_work_unknown`.

### Eighth pass

Eighth pass of 2026-11-20, over a fresh reviewer's findings (one major,
eleven minor, four pre-existing notes), each verified against the code
on main efae5822.

64. Major, supervisor notes are configured inputs of every item prompt.
    Verified: a list item's text goes through
    `context::assemble_agent_prompt` (`scheduler/node.rs` lines 1025 and
    1059 to 1065), which adds the instruction, the known-problems section
    (`context.rs` line 308) and `with_supervisor_notes` (line 309, body at
    213 to 220, over `applied_supervisor_notes`, line 170). Accepted with
    the owner's rule: a visit of a list node with an applied supervisor
    note in force skips nothing and records `skip_supervisor_note` on its
    `list_started`; every item works and carries no skip digest. The
    known-problems section is outside the fingerprint with its reason
    (advice about tools from a store, not configuration of the task).
    Test `a_supervisor_note_before_the_visit_skips_nothing`.
65. Minor, `RunConfig` was not taken whole. Verified: `StartRequestFields`
    (`start_key.rs` line 98) holds `context_max_bytes` and
    `context_compact_model`; `RunConfig.output_limits` (`run_config.rs`
    line 258) caps output capture. Accepted: part 3 is the canonical start
    request whole, filled for every run from the persisted `RunConfig`
    (the struct itself is built only by launch surfaces from typed
    arguments and kept only as a MAC, `StartRequest::capture`, line 150),
    plus `output_limits`, which it lacks, with every removed field named
    and its reason. Revised by item 77: `execution` comes from the manifest, not from the start request.
66. Minor, a credential in a plain param entered bare. Verified:
    `run_params::sensitive` (`run_params.rs` line 41) covers declared,
    secret-shaped names and credential-shaped values. Accepted: every
    value it marks enters only through the keyed tag.
67. Minor, the rebind comparison lacked the executor digest. Verified:
    `executor_digest` (`manifest.rs` lines 109 to 117). Accepted: the
    candidate's part 4 comes from the whole candidate `ManifestProfile`.
    Test `a_rebind_changing_only_the_executor_is_refused`.
68. Minor, the route refusal was undefined. Verified: `set_routed` is
    called by the engine's `route_execution` (`node.rs` line 541), not by
    a control. Accepted as the owner decided: the route refusal is
    dropped; tier routing follows main, and the fingerprint names the
    configured executor chain, which `executor_digest` covers with tiers
    and fallbacks, so a routed attempt neither changes it nor needs a
    comparison. Its test and task lines are removed.
69. Minor, the record-time `refused` had no defined computation.
    Accepted: record time records the source, the items, `truncated` and
    `refused` only for a run whose type cannot be told (item 74); every
    reason that needs `work_inputs` is decided at the visit's opening
    step and recorded on its `list_started` (`skip_refused`). Refined by item 79: the decision is computed before `list_started` is appended.
70. Minor, the check and the append were not one decision. Verified:
    `Build::Decided` (`event.rs` line 3241) re-reads under the append lock
    (lines 3018 to 3021). Accepted: `append_group_decided` writes the group
    only while coverage is still none; a second writer appends nothing;
    `recorded` names the one line per node. Test
    `two_concurrent_entries_record_one_group`. Refined by item 78: the write takes the required lock.
71. Minor, probation refused credit where no skip happened. Accepted:
    only a list node that settled at least one item with `skipped_from`
    needs a fresh worked pass. `ListFold` offers only `of` and `previous`
    (`list/ledger.rs` lines 150 and 156), so `ListFold::visits` is added
    with `visits_lists_every_visit_in_order`; the unreached and
    zero-item cases are eligible in the probation test.
72. Minor, a node-resolved working tree. Verified:
    `scheduler/worktree.rs` lines 1 to 7. Accepted: part 6 and the docs
    say a playbook whose tree comes from a node output or is fresh each
    run never skips. Test `a_worktree_from_a_node_output_never_skips`.
73. Minor, scripts rewritten during the visit. Verified: the run's
    scripts are its own copy taken at start (`run_scripts.rs` lines 1 to
    3), no patch replaces it (`snapshot_playbook`, `scheduler/patch.rs`
    line 269), but any node can rewrite it (`run_scripts.rs` lines 33 to
    40). Accepted: part 2 is the digest pinned at start
    (`prepare.rs` lines 1381 to 1387, the pin now also taken for a
    definition with a list `skip`); a visit whose live digest differs
    skips nothing (`skip_scripts_changed`), and a settlement after a
    mid-visit rewrite carries no skip digest. Tests
    `a_visit_after_a_scripts_rewrite_skips_nothing` and
    `a_scripts_rewrite_mid_visit_stamps_no_digest`.
74. Minor, `ensure_recorded` classified repeatable runs by the manifest.
    Verified: an absent manifest reads as production and a script-only
    run writes none (`prepare.rs` line 1580). Accepted: it decides by
    `RunConfig.eval`, `RunConfig.parent_run` and
    `scenario::session::is_scenario` (`scenario/session.rs` line 444)
    first, and with no manifest or an origin that does not read it reads
    no history and records every set `refused: skip_run_type_unknown`.
    Test `a_crashed_script_only_eval_reads_no_history`.
75. Minor, tasks.md. Accepted: task 3.3 starts with the failing unit test
    `work_inputs_takes_each_part`; the missing tests are named
    (`a_rebind_whose_work_cannot_be_compared_is_refused` for
    `list_skip_work_unknown`, and the tests of items 64 to 74); the fold
    over every visit names `ListFold::visits`; the typo "the other other
    tests" is fixed.

Pre-existing on main, recorded with no change: `after_the_turn` checks and
appends `previous_success_set` without a decided append (`entry.rs`
lines 334 to 374); concurrent runs without a shared key (item 38); a
queued run paused before its turn leaving its window unset (item 50);
the pilot review text counting carried items (`pilot.rs` line 168).
The file count is 82: `crates/apb-core/src/detect.rs` joins the map for
item 80.

### Ninth pass

Ninth pass of 2026-11-20, over the eighth Codex review (one blocker,
three major, two minor findings, four pre-existing notes already
recorded as items 38, 50, the Fifth pass and the Eighth pass), each
verified against the code on main efae5822.

76. Blocker, the inline-item scenarios could not pass. Verified: the
    fingerprint includes `Playbook.version` (`schema.rs` line 146), and
    publishing changed content takes a new version
    (`versioning/mod.rs` line 209), so adding an inline item changes every
    item's fingerprint. Accepted: the inline exception is removed; part 1
    is the definition whole, the latest journaled digest of the
    definition in force; a new version reruns every item, inline lists
    included; the two cross-version inline scenarios and their tests are
    removed (no same-version test means anything, since an inline list
    cannot change within a version); the docs say incremental skipping is
    for lists fed by an earlier node or a source.
77. Major, the execution mode was absent. Verified: `RunConfig` has no
    execution field (`run_config.rs` line 153), and the manifest holds the
    effective mode and the host fallback (`ManifestExecution`,
    `manifest.rs` line 511, `RunExecutionManifest.execution` at line
    349). Accepted: part 4 takes the manifest's execution block whole, as
    recorded; `execution` leaves the start request by name. Test
    `a_cli_to_host_change_changes_the_work_fingerprint`.
78. Major, the group write was not guaranteed a lock. Verified: the
    plain path takes `JournalLock::on`, which goes on without the lock
    (`event.rs` line 3018); the durable path takes `JournalLock::required`
    (lines 3015 and 3016). Accepted: the group goes through the
    required-lock path; a lock failure refuses the write
    (`list_skip_lock_unavailable`) and no node runs. Tests
    `a_journal_lock_failure_refuses_the_set_write` and
    `two_concurrent_entries_record_one_group`.
79. Major, the visit refusal had contradictory ordering. Verified:
    `open_visit` appends `list_started` (`scheduler/list/mod.rs` line 150)
    before `opening_steps` (line 159). Accepted: the decision is computed
    in `open_visit` before that append and stored in
    `list_started.skip_refused`; `opening_steps` and `complete_opening`
    only read it; a scripts difference found at recovery leaves the
    recorded decision standing, and items settled after it carry no skip
    digest. Test
    `a_crash_after_list_started_replays_the_recorded_skip_decision`. Revised by item 83: the pin is checked again before the skip settlements, and a change fails the visit.
80. Minor, the fork help cache identified a path, not a binary.
    Verified: the detector keeps `path:size:mtime_ms` per agent
    (`detect.rs` line 165, built by the private `fingerprint` at lines
    178 to 193; the struct at line 157 the review names is its cache).
    Accepted: the cache is keyed by that fingerprint, made public as it
    is. Test `a_replaced_cli_at_the_same_path_is_probed_again`.
81. Minor, the `SkipSet.refused` comment pointed at record time.
    Accepted: the comment reserves set-time refusal for an unknown run
    type; every `work_inputs` reason is decided at the visit's start.

The file count is 82: `crates/apb-core/src/detect.rs` joins the map for
item 80.

### Tenth pass

Tenth pass of 2026-11-20, over the ninth Codex review (two major, three
minor findings, four pre-existing notes already recorded), each verified
against the code on main efae5822.

82. Major, a configured executable could change without invalidating a
    pass. Verified: `executor_digest` leaves out `canonical_executable`
    and `executable_fingerprint` (`manifest.rs` lines 109 to 117), while
    `agents.<id>.program` (`config.rs` line 414, resolved by
    `program_for`, `invocation.rs` line 182) selects the binary. Accepted under the
    "whole" principle: part 4 takes the effective profile as the manifest
    recorded it resolved, every `ResolvedInvocation` of the chain and the
    tiers whole (`invocation.rs` line 18), the canonical executable
    included. Test `a_program_only_change_reruns_every_item`. Refined by item 87: `executable_fingerprint` is removed by name. Task 3.6 aligned by item 90.
83. Major, recovery could skip under changed scripts. Verified:
    `open_visit` appends `list_started` (`scheduler/list/mod.rs` line 150)
    and a drive can die before the opening steps while another node
    rewrites `run_dir/scripts`. Accepted: the pin is checked again right
    before the skip settlements are written, in the opening steps and in
    `complete_opening`; a difference fails the visit
    `list_skip_scripts_changed` and no item is settled as skipped. Test
    `a_crash_then_scripts_rewrite_fails_the_visit`.
84. Minor, `skip_no_key` relied on a creating tag. Verified: `tag`
    creates a missing key (`run_origin.rs` lines 121 to 132), `tag_if_keyed`
    does not (line 136). Accepted: part 3 uses `tag_if_keyed`; `None` means
    `skip_no_key`.
85. Minor, two tests contradicted the rules. Accepted: the crashed eval
    run gets empty sets (`RunConfig.eval` decides before classification);
    the missing-manifest case records `skip_needs_manifest` on
    `list_started`; a separate case
    `a_crashed_run_with_an_unknown_origin_reads_no_history` covers
    `skip_run_type_unknown`.
86. Minor, the cut named removed work. Accepted: the seventh pass's
    sentence on inline-item tests is marked superseded by item 76; task
    0.1 no longer asks for `agent_key_parts` or `JUDGE_FINGERPRINT`,
    which nothing in the design uses any more.

The file count stays 82.

### Follow-up decision of the tenth pass

87. The owner's decision on item 82's open point: `executable_fingerprint`
    (size and modification time of the agent CLI, `invocation.rs` line 25)
    is removed from part 4 by name, with its reason in the closed
    exclusions (exclusion 10): an agent CLI updated in place at the same
    path is the world changing, like a model behind the same model id,
    not the run's configuration; the canonical path and the invocation
    form identify the configured executor. The table test gains a
    negative row (an in-place update keeps every item skipped), and the
    docs say so. The file count stays 82.

### Eleventh pass

Eleventh pass of 2026-11-20, over the tenth Codex review (three major,
one minor finding, two pre-existing notes), each verified against the
code on main efae5822. Two majors were in the carry of skipped rows, so
the mechanism is replaced by a simpler rule instead of being gated.

88. Major, a refused skip could still carry a skipped item. Verified:
    `opening_steps` runs `carry_passed` before the skip step
    (`scheduler/list/mod.rs` lines 182 to 188), and the design copied
    `skipped_from` into the carried row. Replaced: a skipped row is never
    carried; `carry_passed` carries only rows that were worked and passed,
    as on main, and leaves rows with `skipped_from` pending; the skip step
    of each visit decides them again from the recorded set under that
    visit's recorded decision. The pin check before the skip settlements
    covers the scripts interleaving. Tests
    `a_note_between_visits_makes_the_skipped_item_work` and
    `a_scripts_change_after_the_second_list_started_fails_before_any_skipped_row`.
89. Major, a carried item could acquire skip evidence. Verified:
    `carry_passed` carries by content digest only (line 308), and the
    design filled an empty `skip_digest` on every settlement. Accepted: a
    skip digest is minted only by a fresh settlement whose step executed in
    this visit under the visit's work (`ran: Some`, `scheduler/list/run.rs`
    line 117); a carried row keeps the stamp it had or stays without one;
    a skipped row keeps the source's stamp. Test
    `a_carried_item_after_restored_scripts_gets_no_stamp`.
90. Major, task 3.6 compared a rebind by the executor digest. Verified:
    `executor_digest` leaves out `canonical_executable` (`manifest.rs`
    lines 109 to 117). Accepted: task 3.6 compares the whole candidate
    resolved profile, the same part (d) input, canonical executable
    included. Test `a_rebind_changing_only_the_program_path_is_refused`.
91. Minor, an absent field and an explicit `null` compared equal.
    Verified: the list's own field comparison tells them apart
    (`list/items.rs` line 239). Accepted: `fields_digest` encodes presence.
    Test `fields_digest_tells_absent_from_null`.

Pre-existing on main, recorded with no change: `rerun: not_passed` carries
fresh passed items by content digest (`scheduler/list/mod.rs` line 305);
runs without a shared concurrency key are not serialized (item 38). The
file count stays 82.

### Cut in the eleventh pass

1. Carrying skip provenance: `carry_passed` copying `skipped_from` into a
   carried row, counting a carried row with a stamp as skipped, the
   `ListItemSettled.skipped_from` note that it is kept on a carry, and the
   test `the_second_loop_visit_keeps_the_skip_stamp` with its spec
   scenario; replaced by "a skipped row is never carried; each visit
   decides it again" and the tests of items 88 and 89.
2. The rule that filled an empty `skip_digest` on every settlement of a
   skipping node; replaced by minting on a fresh executed settlement only.

### Reconciled with main 3bb424be

After `eval-doubles` (#291) merged as 3bb424be, every line number this
design cites in a file that change touched was re-read on the new main
and moved to where the same site is now: `event.rs`
(`is_checkpoint`, `check_unknown_settled`, the guard after a checkpoint,
`append_pair`, `SessionHandoff`, `NodeCacheHit`, `Build`, the lock
paths, `read_all`, `read_all_for_stop`, the open journal file),
`validate/guarded.rs` (`GuardedFeature`, `ALL`, `guarded_features`, the
`ListPilot` rule, `mod tests`, the two count tests, the latter now named
`all_has_fourteen_entries_in_order`), `scheduler/prepare.rs` (the
window, the key resolution, `allocate_run`, the scripts pin, the
manifest write, `run_started.definition`, `run_guard`, `run_guard_of`,
the unchecked start, the unreadable history refusal, the keyed clear),
`manifest.rs` (`RunExecutionManifest.execution`, `ManifestExecution`,
`ManifestConnectorGrant`, `is_empty`), `connector/run.rs`
(`ManifestAccount`, `account_digest`), `validate/decisions.rs`,
`journal_corpus_test.rs` (`restore`, the required-binaries check) and
`crates/apb-engine/tests/main.rs`; tasks 3.6 moves with them. Counts
checked: `GuardedFeature::ALL` is 14 with `eval_doubles` last, the
known-events fixture is still 29 lines, and the validator codes in use
in the family are V280 and V281, so V283 is free. `previous_success.rs`,
`scheduler/patch.rs`, `run_config.rs`, `decision/mod.rs`,
`docs/GUARDRAILS.md` line 276, `docs/HOWTO-authoring.md` line 1252 and
`.github/workflows/ci.yml` kept their cited lines. `inbox-waits` is not
merged yet; where this design places an entry after its, the entry goes
after the last one on main. No decision changed.

### Fix round of the implementation reviews

Two reviews of the implementation (spec conformance: three major, fourteen
minor; audit: four major, seventeen minor), each finding verified against
the code before its disposition. Nothing the design cut came back.

1. The stamp rule (audit M1 to M3). One rule, stated in D2 "Applying the
   set" and enforced in `ListRun::minted_digest` only: a fresh settlement
   mints a skip digest only when the item's work ran under exactly the
   configuration the fingerprint covers and its check ran to a verdict
   and passed. `ListRun::stamp_block` (read once per `ListRun`) names a
   supervisor note in force for the run, whatever the visit decided at
   its start, and a revised pilot's guidance; `Settlement.check_decided`
   is false for the `on_unavailable` fallback (`Checked.fallback`), so
   `accept_with_note` from an unavailable judge never stamps. The test
   that asserted a stamp for `accept_with_note` now asserts none. Tests
   `a_reopen_under_a_supervisor_note_stamps_nothing` (a supervised run,
   the real `ContextAppend` and `Retry` controls, a reopened visit),
   `a_revised_pilot_stamps_nothing` (the real pilot gate answered
   `revise` with guidance).
2. The table (audit M4, m9). Every row runs a control run first (the
   same input again skips both items), and the changed run asserts that
   its set came from the control run with both items and that the visit
   recorded no refusal. The defaults, item-check and inline-item rows edit
   the definition in place instead of publishing a version, so each
   changes only its own input.
3. `skipped` joins `LIST_OUTPUT_FIELDS` (spec M1); V46 no longer warns
   (`a_node_reading_the_skipped_count_gets_no_v46`). The secret tests
   cover the three cases of `run_params::sensitive` and a
   credential-shaped instruction: a unit test over the serialized work
   (`sensitive_inputs_enter_only_as_keyed_tags`) and
   `a_sensitive_param_enters_only_as_a_keyed_tag` over the journaled
   digests, the set's source and the journal text (spec M2). The lock
   (spec M3, audit m7, m13): `JournalLock::required_within` waits at most
   `LOCK_WAIT` (2 seconds) and fails `journal_locked`; `lock_refusal`
   maps only that to `list_skip_lock_unavailable`, a full disk to
   `list_skip_disk_full`, and passes every other error through; the test
   holds the lock on a second open file and calls the public `write_sets`.
4. Fork safety (audit m1 to m4, spec minor 10). A fork that reports the
   source's session id is a failed fork (`FORK_REPORTED_SOURCE`): the
   binding is dropped and the node starts cold with the journaled reason.
   `decide` compares the whole resolved profile through
   `handoff::same_profile` and runs the help probe last, through a
   closure, so a node that will not fork never probes. The probe cache
   holds one entry lock per key; a help that does not read expires after
   `FORK_HELP_RETRY` (60 seconds). A fork attempt that errors but printed
   its new id is continued (`adapter::reported_session`). Tests
   `a_fork_that_reports_the_source_session_is_never_continued`,
   `a_fork_under_another_profile_starts_cold_without_a_probe`,
   `an_opencode_fork_starts_from_a_copy_and_a_retry_continues_it`,
   `a_failed_fork_that_printed_its_session_is_continued` and the unit
   test `a_failed_probe_is_probed_again_after_its_lifetime`.
5. An `unless_changed` field that no item of the visit has makes the
   visit skip nothing (`skip_unless_changed_absent`, audit m5). Test
   `an_unless_changed_field_no_item_has_skips_nothing`.
6. Fail closed (spec minors 6 and 7, audit m6): the rebind refusal
   refuses when the journal does not read; a continuation refuses
   `list_skip_incomplete` when the predecessor's definition does not
   read; the window path of the set read rechecks its source run
   (`eligible_source`: created here, a readable manifest a reader may
   learn from, top level, settled succeeded; `previous_success::settled_succeeded`
   became `pub(crate)` for it, the one change to that file). Tests
   `a_journal_that_does_not_read_refuses_the_rebind`,
   `a_continuation_without_the_predecessors_definition_is_refused`,
   `a_window_source_no_longer_settled_is_refused` (renamed
   `a_window_source_no_longer_settled_gives_no_skip` by Fix round 2 rule
   B, which records the refusal on the set instead of refusing the run).
7. Cost (spec minor 5, audit m8): the scripts pin is read once per visit
   (`ListRun.pin`). Deviation from "the tree digest once per visit": the
   live digest of the scripts tree is taken once per batch, at its first
   stamp, so a rewrite between batches is still seen before the next
   stamp (D2 "Work fingerprint" part 2); per settlement it is never taken.
   Superseded by Fix round 2 item 1: the tree is digested at both ends of
   every item, and a difference latches the visit.
8. Records and duplicates (spec minors 1, 2, 4, 9, 11, 12): the 0.25.0
   sentence says it drops the whole list block and runs the step once;
   the public record lost its leftovers; the intra-doc link resolves;
   `#[cfg(unix)]` is back on `session_handoff_test`; `check::argv_flags`
   is the one flag filter, `cache::pinned_scripts` the one pin lookup,
   `skip::run_identity` the one scope lookup, `skip::predecessor_definition`
   the one snapshot read, `no_set` the one incomplete message, and the
   panel computes the item text once. `profile_work` serializes the
   resolved profile whole minus `routed_tier` and `cascade`
   (`PROFILE_NOT_WORK`). `work_inputs_takes_each_part` gives every part a
   value (a manifest with a chain, a tier and a grant, decisions, a tree)
   and asserts each. The corpus window snapshots are made in process
   (`l1_prepared_windows`): prepared and copied with the `PreparedRun`
   held, driven to the gate and copied again, then stopped.
9. Tests (audit m10, m12, m14 to m17): the cli-to-host and account tests
   are on `work_fingerprint(work_inputs(..))` and named for it
   (`a_cli_to_host_change_changes_the_work_fingerprint`,
   `a_granted_account_field_changes_the_work_fingerprint`); the
   concurrent-write test starts its writers behind a `Barrier`; the
   queued-run negative is checked right after the queued record, with no
   sleep; the fork tests assert the argv contract and that the source
   session file's bytes are unchanged; the supervisor-note test posts the
   note through the control channel while the run holds before the list;
   `external_data_eval_and_scenario_successes_are_never_a_source` runs
   all three end to end.

Not changed, with the reason: audit m11 asked to leave field coverage to
the table, the coordinator's decision asks for an assertion per part, so
the test asserts every part over the serialized form; the cli-to-host
case stays at the fingerprint, because host mode refuses list nodes at
start. The same 0.25.0 wording of `pilot` and `stop_when` in
`docs/GUARDRAILS.md` and the HOWTO belongs to `before-running-alone`
and is left to it.

### Rebased on main 0336dada

`secret-params-in-run-files` (#292) merged as 0336dada; the branch is
rebased onto it. The work's run request (part 3) classifies a param as
declared secret by the run's secret set, read strictly
(`run_redact::secret_names_strict`: the manifest's `secret_params` and the
snapshot's declarations) plus the definition in force, the set every
redaction of the run uses; a set that does not read refuses the work
(`skip_secret_set_unreadable`), so the visit skips nothing. A
`--supervise` start now hands its params to the detached driver in the
launch handoff, never on argv; the driver's run config holds the same
params, so the work is unchanged (test
`handed_params_give_the_skip_the_same_work`). `scheduler/patch.rs` keeps
both the skip's patch refusal and main's secret refusal; the resume
entries keep main's secret check before the drive entry records missing
sets. Unit test `the_runs_secret_set_classifies_the_params`. No fixture of
this change renders a secret param, so V340 moved none.

### Fix round 2

A code pass over the implementation (six major findings, one minor),
each verified against the code with its trigger reproduced as a test that
failed with the fix reverted. Three rules close them where they share a
cause.

Rule A (findings 1 to 3): the fingerprint is taken from what is actually
executed, resolved once, and the same resolved value is what runs.

1. Scripts: the comparison kept for a whole batch let an item rewrite the
   scripts copy after another item's settlement and get a stamp for the
   original. Now both ends of every item are checked (its start and its
   fresh settlement, after its work and its check) and any difference
   latches the visit so it stamps nothing after it (D2 "What
   `WorkInputs` covers" part 2). Test
   `a_scripts_rewrite_after_the_first_settlement_stamps_no_later_item`.
2. Runtimes: `GlobalConfig.runners` and the program each runner
   resolves to were not in the work while every script resolved them
   afresh. Part 7 fingerprints the resolved runner, and `script::resolve`
   runs that same resolution, pinned per run directory and process. The
   audit of what script execution and script checks read afresh from the
   machine: the `runners` entry and `PATH` (part 7), the working
   directory (part 6), the machine's isolation settings and the host
   facts for an isolated node (part 8, removed in Fix round 7 when V288
   narrowed skip to nodes without isolation); the script file is the run's
   scripts copy (part 2) and every other input of `run_script` and
   `run_check` (the script path, the timeout, the run context variables,
   the item's own variables) comes from the definition, the run or the
   item. Tests `a_changed_runner_runtime_reruns_every_item` and
   `a_run_keeps_the_runtime_it_resolved`.
3. Directory: no explicit worktree fingerprinted `none` while execution
   fell back to the project root, so a copied project skipped for another
   directory. Part 6 is the canonical effective directory, the fallback
   and the node's own `workdir` included. Tests
   `a_copied_project_works_every_item_in_its_new_directory` and
   `the_execution_dir_is_the_tree_or_the_nodes_own_workdir`.

Rule B (finding 5): the journal a skip consumes is judged on the same read
that consumes it, on both paths (`skip::sets_from`); a source that is no
longer an eligible earlier success at that read gives no skip: every
skipping node records `skip_source_ineligible` on its set and works every
item (the window path refused the run before; it now records the same
refusal). The race of the lookup path cannot be timed without a seam, so
`a_source_reopened_after_the_lookup_gives_no_skip` drives `sets_from`,
the step both paths end in, on the journal of a source reopened after it
succeeded; `a_window_source_no_longer_settled_gives_no_skip` drives the
window path end to end.

Rule C (finding 6): a skipped item is never executed evidence. The item
check evidence counts only the items checked in this run (its detail says
so when any item was skipped), an all-skipped list gives none, and the
freshness of a script item check reads the latest settlement of an item
checked in this run. Inherited passes are told apart by the list line
with their source run ("2 skipped (passed in run R, not checked
again)"). Every surface that counts list items reads these through the
run outcome (`apb runs`, `run_status`, the run page, the report page);
`apb stats` and the estimate count attempts, which a skipped item has
none of. Tests `an_all_skipped_list_claims_no_executed_check` and
`skipped_items_are_never_executed_check_evidence`.

Finding 4 (session fork): fork eligibility compares the binding the
source's attempt recorded when it ran with the binding the fork will
execute (D3 "The handoff"). Test
`a_source_rebound_after_it_ran_is_not_forked`.

Finding 7 (minor): the journal corpus window case prepares the run in a
child process of the test binary with an explicit environment
(`l1_window_child`); no process-wide environment change remains in that
test.

### Fix round 3

An independent review of the implementation (one major, four minor
findings), each verified against the code, its trigger reproduced as a
test that failed with the fix reverted.

M1. A skipped item reached later nodes with `output: null`, so a
downstream list worked on null and stamped that. A skipped item now
carries the output its source settlement recorded, taken from the same
read the set comes from, within the per-item and per-set caps; an item
whose output cannot be carried is worked and counted (D2 "Output"). Tests
`a_skipped_item_carries_its_output_down_the_chain` (the review's chain
over three runs: B's item digests for skipped A items equal run 1's, B
skips them, nothing is worked on a missing value, the agent after the
lists sees the values) and `an_output_over_the_cap_means_the_item_is_worked`.

m1. A concurrent sibling could rewrite and restore the scripts copy inside
another item's work. Narrowed, not closed by a new copy mechanism: in a
run whose definition declares `skip` the copy is read-only while the run
can execute (D2 part 2). The writers of a run's scripts tree, found by a
search for every write and removal under a run directory: preparation's
`copy_scripts` (the only writer; it protects right after the copy is
verified and pinned), the drive entry (protects again), the run's end
(`run_end::record` gives the write bits back), and the removers of run
directories, `retention` (`apply` and `archive::recover`) and the scenario
store's cleanup (`writable_tree` first). A supervisor patch writes only
`playbook.yaml` and a resume never copies the scripts again. Tests
`a_script_writing_next_to_itself_fails_on_its_own_write` (not asserted as
root) and `a_run_without_skip_keeps_its_scripts_writable`; the plain-run
journal fixture is unchanged.

m2. The latch was silent. It is journaled once per list execution on the
first settlement after it (`list_item_settled.skip_latched`, a
`serde(default)` field, no new event) and shown on the skip line of every
surface. Test `a_latch_is_journaled_once_and_shown`.

m3. `ensure_recorded` gave an external-data run empty sets where
preparation reads the history; it now reads the history for it too. Tests
`an_external_data_run_that_crashed_before_its_set_write_reads_the_history`
and `a_queued_external_data_run_resumed_reads_the_history`.

m4. `freshness::item_check` changed for plain lists. Its reference is again
the later of the list's `node_finished` of a visit that checked an item and
its latest settlement of an item checked here, so a plain list reads as
on main. Test `a_plain_list_item_check_is_read_as_on_main`.

### Fix round 3 follow-up

The read-only scripts copy outlived the drive: only the run's end gave the
write bits back, so a paused, parked or gate-waiting run, and every test
that left a run paused, kept a tree a plain removal could not delete. The
copy is now held by a guard value for exactly the life of each drive
(D2 part 2); preparation no longer protects it. A run directory left
read-only by a killed apb is removed with `apb runs prune`, or after
`chmod -R u+w`. Tests
`a_paused_run_leaves_its_scripts_writable_and_a_resume_protects_them`
(also: nothing under the test's root is left read-only after a paused
run), `a_now_pause_leaves_its_scripts_writable` and
`a_parked_run_leaves_its_scripts_writable`.

### Fix round 4

Codex code pass 2 (five major findings, one minor), each verified against
the code, its trigger reproduced as a test that failed with the fix
reverted.

Findings 1 to 3 share one cause: a value fingerprinted at one moment and
resolved again when it executes. Closed by construction: one value,
`cache::VisitExecution`, holds everything an item's execution reads from
outside the definition; it is resolved once per list execution
(`ListRun::execution`), the work is computed from it, and every item of a
skip visit takes it as its input (`ItemExec.exec`). The audit, per field:

| Field | Resolved once in | Consumed at execution by |
|---|---|---|
| directory (the tree under the node's `workdir`, canonical) | `cache::execution_dir`, from `ListRun::execution` | `node.rs` agent arm (`node_dir`) and script arm (`workdir`): `ItemExec.exec.dir` |
| step runtime (canonical program, start name, identity) | `script::resolved_runner` (per run directory and process), into `VisitExecution.runtimes` | `node.rs` script arm: `script::run_script_with_runtime` over `exec.runtimes[runner]` |
| item check runtime (`sh`) | the same pin, into `VisitExecution.runtimes["sh"]` | `baseline::read::run_script_json` through `resolved_runner`, the same pinned value (the pin is never re-resolved within a process; a new process resolves anew and recomputes the work from that same pin) |
| the scripts copy | the pin in `run_provenance` | the copy itself, checked at both ends of every item (part 2) |
| MCP tool approvals of granted accounts (`pin_digest`) | `cache::mcp_pins` from the machine's pin store, into `VisitExecution.mcp_pins` | every MCP call checks the live server against the pin store (`connector::mcp::pin`); a settlement rereads the store and a difference latches the visit (`ListRun::pins_held`, `skip_mcp_pins_changed`) |
| the tree (`ctx.workdir`) | the drive, once per node execution (`cache::tree_dir`) | every item of the execution through `ctx` |
| item check directory (the tree, canonical) | `cache::visit_execution` into `VisitExecution.check_dir` (Fix round 5) | `list/check.rs` `read_ctx` (`workdir`) |
| item check policy (`guard_tree`, `holds_tree`) | `cache::visit_execution` into `VisitExecution.check_policy` (Fix round 6), from the worker count and the definition | `list/check.rs` `read_ctx` |
| `PATH` | `cache::visit_execution` into `VisitExecution.path_env` (Fix round 5) | the script step's env and the agent's spawn overlay (`node.rs`), the item check's env (`list/check.rs`) |

1. Workdir link switched between two sequential items. Test
   `an_item_runs_in_the_directory_its_visit_resolved`.
2. A relative runner: taken against the project root once and exec'd by
   its canonical path (`script.rs` `resolve_runner_now`,
   `command_for_runtime`). Test
   `a_relative_runner_runs_the_program_its_work_names`.
3. Isolation mounts changed between items. Superseded in Fix round 7:
   skip on an isolated node is refused (V288), so the captured settings,
   the sandbox plumbing that read them and their test were removed.
4. Rejected, with the reason: a sibling item that renames the scripts copy
   aside, substitutes another and restores it inside another item's
   window. This is the third pass on the scripts tree; the protection
   stays what it is (read-only modes against accidents, digests at both
   ends of every item, the latch). A same-user process that deliberately
   replaces the tree and restores it inside a sibling's window is outside
   what apb defends against anywhere: the same trust as an agent that
   fakes the output its own check reads, and as the unsigned pin store a
   same-user process can write. Stated as a limit in D2 part 2 and the
   HOWTO.
5. A deferred retry's fork compared the target's current overlay:
   `same_profile` now takes the binding the execution entry runs on.
   Test `a_deferred_retry_rebound_during_its_delay_is_not_forked`.
6. The dashboard's skip label dropped the stamping explanation when the
   set has no source: it now follows every branch. Label test with no
   source.

#### Stated limits of list skip

1. The scripts copy is protected against accidents only; a same-user
   process that replaces it and restores it inside an item's window is
   not detected (finding 4 above).
2. Changes to the copy are detected only when still present as an item
   starts or settles.
3. Root is not stopped by the copy's modes.
4. The journals of earlier runs under the operator's own account are inside
   the trust boundary (D0 "What an older apb does").
5. Skip is not available on a node with an isolation other than `none`
   (V288, Fix round 7).

### Rebased on main 3248c77f

`MCP tool connectors` (#293, f0340685) and `One point oh fixes` (#294,
3248c77f) merged; the branch is rebased onto 3248c77f.

1. AGENTS.md and CLAUDE.md: main's crate map kept whole (the `mcp`
   validator family V300 to V302, the isolation note) with this change's
   `list_skip` guard name added; both files mirrored.
2. Validator codes: V283 is this change's; main takes V300 to V303 and
   V320, so nothing collides.
3. `project_checks` (#294, JUnit reports): read by a baseline check only;
   a list's item check runs scripts, connectors or a judge, never project
   checks, so the work fingerprint reads nothing `project_checks` changed.
   The one edit #294 made to `scheduler/list/check.rs` (`holds_tree` for a
   JUnit scan) is kept as main wrote it.
4. MCP connector grants: the manifest's connector and account digests were
   already part of the resolved grants (part 4). The MCP tool approval a
   granted account holds in the machine's pin store is added as part 9,
   held in the visit's resolved execution, and reread at each settlement:
   a re-approval during a visit stops stamping (`skip_mcp_pins_changed`).
   Test `a_granted_accounts_mcp_pin_is_part_of_the_work`.
5. `hermes`'s agent forms: main's resume form kept, with this change's
   `fork: None`.
6. The journal corpus test: main's row M1 and this change's rows L1 and F1
   both kept.

### Fix round 5

Codex code pass 3 (three major findings, one minor). This is the third
pass to find something the fingerprint leaves out; that class cannot be
closed by adding fields, so this round narrows the claim ("What the
fingerprint claims", at the top of D2 "The skip digest") and fixes only
what apb itself configures.

1. The item check's working directory (Codex finding 3): apb configures
   it, so it is covered. `VisitExecution.check_dir` holds it as resolved
   (the run's worktree, else the project root), the work names it (part
   10) and the check runs there. Test
   `the_check_directory_is_part_of_the_work` (a fixed absolute node
   `workdir`, a check reading `./approved`, `--worktree A` then `B`). The
   audit table now lists every directory and program apb chooses for an
   item; none is outside `VisitExecution` or the drive's `ctx`.
2. `PATH` (Codex finding 1): covered. `VisitExecution.path_env` holds the
   value as apb's process had it when the visit started, the work names it
   (part 11), and the script step, the agent spawn and the item check of a
   skip visit receive that value. Test `path_is_part_of_the_work`. Other
   inherited variables are not covered, a stated limit: fingerprinting the
   whole environment would make every run differ (a terminal's own
   variables change between sessions), and a profile's `process_env` is
   the tool that bounds what an agent inherits.
3. The value a connector secret resolves to (Codex finding 2): rejected,
   a stated limit. A secret is resolved only by the call that needs it;
   resolving every account secret, helper commands included, to
   fingerprint a visit would start programs and read secrets no call asked
   for.
4. The escape the claim names: no start surface had a way to start a run
   that skips nothing. A `no_skip` start option on every surface (the CLI,
   MCP, HTTP, the Run dialog and the launch handoff, with its recording in
   preparation and at the drive entry) needs about eight files beyond its
   tests, over the bound this round set, so it is not added here and is
   reported instead. Until then the documented way is to publish a new
   version of the playbook: the definition is part of the work.
5. The deferred-fork test (Codex finding 4) no longer depends on a
   scheduling window: the run is paused during the delay, the rebind is
   queued while nothing drives it, the resume applies it, and the test
   asserts that `profile_rebound` comes before the deferred entry's handoff.

The stated limits of list skip (Fix round 4) gain:

5. Inherited environment variables other than `PATH` are not part of the
   work.
6. The values connector secrets resolve to are not part of the work.

Sentences corrected to the claim: D2 "What `WorkInputs` covers" ("whatever
the run was configured with is inside" now "what apb configures for the
item is inside"), D2 "Applying the set" ("exactly the configuration" now
"exactly what apb configured"), the spec delta's stamp rule, and the
HOWTO skip section (the skip digest paragraph and "while nothing about
it changed" now "while nothing the skip covers about it changed").

### Fix round 6

Codex code pass 4 (two major findings, one minor), all accepted, each
trigger reproduced as a test that failed with the fix reverted.

1. The item checks' policy: the worker count (`RunOptions.max_parallel`
   with `batch.parallel`) decides whether a script or connector check must
   leave the tree as it found it, while the work drops `max_parallel`. The
   policy flags are now captured in `VisitExecution.check_policy`,
   fingerprinted (part 12) and the checks run from them. The audit of
   every switch the item and check paths derive from run options or the
   machine (`scheduler/list/check.rs`, `batch.rs`, `run.rs`):
   - `guard_tree` (`workers == 1`): captured, part 12.
   - `holds_tree` (the tree lock predicate and `workers == 1`): captured,
     part 12; it decides only whether a report scan may trust the tree.
   - `workers` itself (concurrency): not work; it decides only how many
     items run at once, its effect on checks is the two flags above.
   - `track_provenance` (`workers == 1`): display only (which attempt
     made which commit).
   - `cap_secs` (the run deadline left): a time limit, not configuration;
     a check that runs out of time gives no verdict, and a settlement
     without a decided check stamps nothing.
   - `spawn_env` (`RunConfig::spawn_env`): an eval run's overlay only, and
     an eval run skips nothing.
   - `CHECK_TIMEOUT_SECONDS`, `scripts_label`, `site`: constants.
   Test `the_check_policy_is_part_of_the_work`.
2. A continued or reopened visit (a resume in a new process, a supervisor
   retry after a patch or a rebind) resolves its execution again while its
   skipped rows stayed skipped under the old one. Rule: a skipped row is
   valid only under the work it was skipped with. `list::recheck_skipped`
   compares every skipped row of such a visit with the work as this
   execution resolves it; a row whose digest no longer matches, or every
   skipped row when the work does not compute, goes back to pending
   (`list_item_settled` `interrupted`, reason `skip_config_changed`) and
   is worked; the skip line counts them ("N skipped items worked after the
   configuration changed", `SkipView.reworked`). Rows the visit actually
   worked stay as they are, as on main; a run that never skips writes what
   it wrote before. Tests
   `a_resumed_visit_works_a_skipped_item_whose_runtime_changed` (a CLI
   test: run 2 skips a and is paused, `runners.sh` moves to runtime B, the
   resume in a new process works a under B) and
   `a_resume_with_nothing_changed_keeps_the_skips`.
3. The deferred-fork test: a ten-minute retry delay releases the run, the
   run is paused, the rebind is queued while nothing drives it, and the
   resume releases the deferred retry through the retry-now path; the test
   asserts the event order, and no sleep or wall-clock window remains in it.

Future work: a start option that skips nothing, on every start surface
(Fix round 5 item 4). Until then, publishing a new version is the way.

### Fix round 7

Codex code pass 5 (three major findings), all accepted, each trigger
reproduced as a test that failed with the fix reverted.

1. An isolated item without a sandbox (`best_effort` where the machine
   cannot isolate), or with a sandbox that degraded to scratch, runs in a
   fresh directory while the stamp names the tree directory. Narrowed
   instead of represented: V288 refuses `list.skip` on a node whose
   isolation is `full` or `best_effort` at validate, every save and run
   start ("skip on isolated nodes is not supported yet"). What that made
   unreachable is removed: `VisitExecution.isolation`, part 8 of the work
   (`isolation_work`), the round-4 sandbox plumbing
   (`NodeIsolation::with_machine`, `Request.machine_read_only`, the
   captured argument of `declared_extras`; `sandbox/mod.rs` and
   `scheduler/node/sandbox.rs` are main's again) and the test
   `an_item_is_sandboxed_with_the_isolation_its_visit_captured`. One run
   side line remains, in `ListRun::minted_digest`: an isolated execution
   never stamps. Tests `skip_on_an_isolated_node_is_refused` (core) and
   `skip_holds_under_isolation_none_and_is_refused_when_isolated`.
2. A supervisor note in force did not take back the retained skips of a
   reopened visit: the recheck compared only the digest, which leaves
   notes out on purpose. One function, `list::visit_refusal`, now decides
   whether a visit may hold skips, for a new visit (`skip_decision`) and a
   continued or reopened one (`recheck_skipped`); a refusal sends every
   skipped row back to pending, a note's with the reason
   `skip_supervisor_note` the new-visit path already records. Tests
   `a_note_in_force_takes_back_the_retained_skips_of_a_reopen` and
   `a_reopen_with_nothing_changed_keeps_the_retained_skips`.
3. A skipped row taken back kept its inherited output, so a new execution
   that printed none settled with the old JSON and the next run inherited
   it. The fold now clears the output of a row taken back
   (`ledger::invalidates_skip`, both reasons), so a later settlement
   without output leaves it with none, and a row taken back that the run
   paused before working is pending with no output and no provenance on
   the fold and `run_lists`. The skip line now reads "N skipped items
   worked after their skip no longer held", since a note counts too. Test
   `a_skip_taken_back_keeps_no_inherited_output`.

Self-review of the continued-visit path against the ledger fold: the
table in D2 "Skipped rows across a continued visit". Every reachable cell
names its test; the one cell without a test before this round (a skipped
row kept by a reopen with nothing changed) has one now; the deferred
retry column is not reachable (V126).

Future work: skip on isolated nodes, once a list execution's actual
directory under a sandbox or a scratch fallback is part of its value.

### Fix round 8

An independent review of c17302ba: no blocker, no major, five minors,
all fixed, each new test failing with its fix reverted.

1. A skipped row reached a later visit's retry briefing as the item's own
   earlier attempt (`previous_round`, `previous_outputs` from the run it
   was skipped from). `ListRun::previous` ignores a record with
   `skipped_from` in both branches, this visit's fold and the earlier
   visit's: a skipped row never ran in this run. Test
   `a_skipped_row_is_never_the_items_earlier_attempt` (a loop under
   `rerun: not_passed`, a note between visits); the state table has the
   row.
2. The take-back count said work that may not have happened. It now says
   what is counted: "N skipped items taken back because their skip no
   longer held", "1 skipped item taken back because its skip no longer
   held". One wording source: `list::skip_taken_back` with
   `SKIP_TAKEN_BACK_ONE` and `SKIP_TAKEN_BACK_MANY`, used by the run
   outcome and the CLI, exported to `consts.gen.ts` for the web label.
   Tests: the CLI test's line and the web label test, singular and
   plural.
3. Stale comments after the isolation capture went: `ListRun::execution`,
   `ItemExec.exec`, the `WorkInputs` part count ("parts 1 to 12, 8
   retired") and the dashboard's `SKIP_HELP`, which named isolation as
   covered and now says skip is not available on isolated nodes.
4. The scripts protection was taken by every drive of a definition with a
   skipping node, eval, scenario and child runs included, although they
   skip nothing, so a killed eval driver left a read-only copy its
   removers could not delete. It is now taken only by a run that may
   skip (`list::skip::may_skip`, the one choice `ensure_recorded` makes);
   the removers' `writable_tree` stays. Tests
   `an_eval_run_leaves_its_scripts_modes_untouched` (asserted inside each
   item) and `a_scenario_run_leaves_its_scripts_modes_untouched` (no
   program runs in a scenario, so the copy is looked at beside the drive;
   Fix round 9 made that look deterministic).
   The removers sentence in D2 part 2 now names who meets the copy.
5. Giving the copy back added owner write to every entry. `Protected`
   records the mode of every entry it walks and its `Drop` sets exactly
   those again; `run_end`'s `unprotect` acts only on a copy left
   read-only (its directory lacking owner write), and the removers'
   `writable_tree` stays the blunt one. Test
   `a_drive_gives_the_scripts_copy_its_exact_modes_back`, with the modes
   `0500` and `0400`: a run directory is private and a resume narrows
   group and other bits away (`run_perms`), so the odd modes are owner
   bits only.

### Fix round 9

The last code pass left one minor, fixed.

1. `a_scenario_run_leaves_its_scripts_modes_untouched` watched the copy
   from a polling thread beside the drive, so it depended on scheduling:
   a watcher first scheduled after the drive ended failed correct code,
   and a watcher that looked before the protection was taken and was not
   scheduled again passed with the fix reverted. The test now asserts
   synchronously through the journal's terminal observer
   (`terminal_hook`), which runs inside the drive once `run_finished` is
   in the file, before a protection the drive held is dropped (a
   scenario's finish gives no write bits back first). The observer slots
   are process wide and the test binary's init fills them, so the test
   runs the scenario in a child test process of its own, for which the
   init leaves them free; no production seam was added. With the
   scenario exclusion reverted in the drive the observer counts one
   read-only directory and the test fails; restored, it passes.

### Fix round 10

The pull request's bot review left seven findings: five fixed (two of
them, on the runtime's identity, by one change) and two answered with the
reason they are not adopted; one CI failure on macOS was fixed besides.

1. A skipping agent list's prompt could read an earlier node's output, a
   value the skip digest does not compare, so a changed value left an
   item skipped. Narrowed instead of represented: V289 refuses `skip` on
   an agent list whose prompt reads a template value other than
   `params.*` and `run.instruction`. Test
   `skip_with_a_prompt_that_reads_run_values_is_refused`.
2. A runtime replaced at the same path while the run held its
   resolution kept its old identity for the rest of the run. Its
   identity is read again as the visit's work is computed and at every
   settlement (`cache::runtime_changed`); a difference refuses the
   visit's skip and latches stamping (`skip_runtime_changed`). Test
   `a_runtime_replaced_mid_visit_stamps_no_digest`.
3. The sandboxed `success_check` started the `sh` runner by its
   canonical path, so a multicall binary behind a link lost its applet
   name. It starts the configured name again, as main did
   (`script::sandboxed_check_program`). Test
   `a_sandboxed_check_starts_sh_under_its_configured_name`.
4. Tests created the installation key outside the env lock while another
   test could hold a redirected `APB_CONFIG_DIR`; every such call goes
   through `key()`, which takes the lock.
5. Not adopted: a content digest of the runtime. The identity is the
   agent detection's `path:size:mtime_ms` by design (part 7); a file
   replaced with the same size and modification time is a deliberate act
   inside the operator's trust boundary (Out of scope item 6), and item 2
   above covers a replacement during a run.
6. Not adopted: a deadline on the failure records written after the set
   write's lock refusal. Every engine append waits for the journal lock;
   the bounded wait exists so the sets are never decided without it, and
   a holder that keeps the lock stalls every writer of the run alike.
7. CI: the macOS test leg failed on two test helpers used only by a
   Linux test; they carry the same `cfg`.

The same review's body carried 27 further findings (two outside the
diff range, 23 minor, two trivial), all fixed:

1. Session fork. A fork whose status file says success after an adapter
   error journals its checked session, so a later node continues it
   (`a_fork_success_from_the_status_file_is_continued_by_a_later_node`);
   a failed fork's session is read from its output up to the run's
   `log_file_bytes`, not 64 KiB (`a_failed_fork_with_a_large_result_is_continued`);
   the "reported the source's id" check runs before the transcript copy,
   the pause session file and the binding are written
   (`a_fork_that_reports_the_source_session_copies_no_source_transcript`);
   three fork tests assert more (the rebind before the handoff, the codex
   help calls recorded, the opencode fork file's contents); the editor's
   fork hint names the binding a fork needs, and clearing the source
   resets fork mode (`NodePanel.test.ts`); the design and the authoring
   guide say what the source-id case does to the verdict and a retry.
2. List skip. A partial set record seen under the lock refuses the set
   write (`list_skip_incomplete`,
   `a_partial_record_under_the_lock_refuses_the_set_write`); the drive
   gives the scripts copy back before it releases its lock; a `PATH`
   that is not Unicode refuses the skip (`skip_path_unrepresentable`,
   `a_path_that_is_not_unicode_refuses_the_capture`); `program_location`
   returns the `PATHEXT` variant that matched; the scenario child lookup
   in the test binary is unix-only; the sets' one write is proven where
   it happens (`the_sets_go_out_in_one_write`); five skip tests assert
   the verdicts and fresh work they claim; the wait test creates its key
   under the env lock; `stop_when` leaves excluded rows out by key.
3. Records. The validator allocation names V283, V288 and V289; the set's
   bounded output inheritance, the 0.25.0 row (it predates list mode and
   drops the whole block), when `apb runs prune` removes a killed run and
   the probation rule in terms of actual skips now match the code.
