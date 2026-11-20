# Eval doubles: connector doubles in evals and honest completion labels

Design record of the change, from its proposal and design. Built on main
efae5822 (after "Typed inbox reads" #287, "People in the loop" #288,
"Before running alone" #289 and "Wait park" #290).

---

# Proposal: eval doubles

## Why

This is a change of the 1.0 sequence (people-in-the-loop,
before-running-alone, typed-inbox-reads, start-rules-and-schedules,
eval-doubles, inbox-waits, incremental-lists-and-session-fork,
mcp-tool-connectors, mcp-http-and-surfaces, one-point-oh-fixes,
one-point-oh-freeze), with `wait-park` merged before it. It is built on
main efae5822, where `typed-inbox-reads` (#287), `people-in-the-loop`
(#288), `before-running-alone` (#289) and `wait-park` (#290) are merged;
every name below was read there. `start-rules-and-schedules` is not merged and nothing here needs
it. The change `inbox-waits` is implemented after this one is merged and
uses parts of it (design D0 "What the change `inbox-waits` takes from
this one").

History: this change and `inbox-waits` were drafted as one change,
`evals-and-inbox-waits`, and split after its third design review round.

Two gaps:

1. A routine that acts through connectors cannot be evaluated. `apb
   eval` refuses every playbook that binds a connector
   (`crates/apb-core/src/eval.rs` `refusal`, line 466, the connector
   leg at lines 471 to 479), and a case that names doubles is refused as
   "a later release" (`LATER_FIELDS` and `LATER_CHECKS`, lines 49 and
   50). `docs/EVALS.md` lines 15 to 20 still say "Not built yet: mock
   connectors". The connector function kind `mock` already exists
   (`crates/apb-core/src/connector/def.rs` `MockSpec`, line 92, answered
   by `PreparedCall::Mock` in `crates/apb-engine/src/connector/call/mod.rs`
   line 307) but only as a property of the installed connector, never
   per case.
2. The `completion_check` labels measure the wrong thing (issue
   #205). `CompletionLabeller::label`
   (`crates/apb-engine/src/decision/report/labels.rs` lines 269 to 348)
   labels a decision "act" when the next node failed, a supervisor
   retried or the run moved back. On one installation's data every one
   of the 52 "act" labels was a finished report. `apb decisions report`
   scores a correct model as wrong, and a threshold fitted on these
   labels would teach enforce to flag finished replies.

| Decision | Code example | Non-code example |
|---|---|---|
| D1 connector doubles in evals | an eval of a playbook that reads `sentry.list_issues` and files `github.create_issue` (with no `irreversible` effect declared) asserts one `create_issue` call whose args contain the permalink | `crm-cleanup` evaluated with recorded `twenty.list_opportunities` answers; `checks.connector_calls` asserts three `update_opportunity` sends with the default owner id |
| D5 honest completion labels | a finished implementation report followed by a failing `pr_prepare` is no longer labelled "act" | a finished customer reply followed by a failing CRM update is no longer labelled "act" |

## What changes

1. Connector doubles in evals (capability `eval-connector-doubles`). An
   eval case gets `connectors: { <connector>: { <function>: [answer,
   ...] } }`: per function an ordered list of recorded answers, each an
   optional `match` (a subset of the call's args, a text the args
   contain, a binding site), an `answer` in the `MockSpec` shape
   (`status`, `body`) and an optional `times`. `apb eval` hands the
   doubles to the run through the existing eval run settings; every
   connector call of that run (a node's, an item check's, the baseline's,
   the follow-up's, a send's reconcile) is checked as a real call is and
   then answered by a double or refused fail closed with
   `double_unmatched`, without resolving a secret or opening a
   connection. A doubled run's manifest records account names only, so
   an `apb` that does not know doubles (an older binary run by an
   absolute path, or one that resumes the run) fails every call of the
   run instead of reaching a service. Every dispatched call (the calls
   the doubles answer or refuse unmatched; a repeat the send ledger
   answers `already_sent` is not one) is captured, its input arguments
   scrubbed, in `runs/<id>/eval-calls.jsonl`, its reservation synced
   before the call is dispatched; an `--external-data` repetition with
   doubles is never kept: its capture and directory are removed once its
   checks ran. `checks.connector_calls`
   asserts count, bounds, and the input arguments and contained text
   (before the connector's text modes and send markers); an assertion over args the capture
   clipped or withheld fails as indeterminate, a capture that does not
   read fails the repetition, and so does a reservation never delivered.
   V81 no longer refuses a connector binding for a case whose doubles
   cover every function of every binding site (V280 names what is not
   covered, V281 a malformed assertion). A declared `irreversible`
   effect stays refused exactly as on main, doubles or not. A doubled
   run never sends a notification, puts this `apb` first on its steps'
   `PATH` (then the inherited one), and carries the run guard `eval_doubles`, which an older apb
   that knows run guards refuses to resume once the journal holds it;
   the doubled start (only it) writes that guard in the same journal
   write as its provenance (a narrowing, not an atomic boundary: what
   keeps a doubled run from a service through a connector is the
   account-free snapshot). A doubled call resolves no secret for itself;
   its captured args are stored only through a redactor that refuses
   whenever a referenced secret is unresolved, and are withheld
   otherwise. A call needs this installation's key for its identity and
   is refused without one. A call whose process died between its
   reservation and its delivery is replayed to its retry and never
   counted twice; an answer is handed out only after its delivery is
   recorded.
2. Honest completion labels (capability `honest-decision-labels`,
   closes issue #205). `completion_check` labels get a rule version, as
   `retry_advice` has: the new rule `reply/1` labels "keep" only when a
   person approved the reply at the review gate that follows it, and
   leaves everything else unlabelled; the gate must approve the reply
   (`approves.results` names the node) and the person's final decision,
   through any scope re-ask that keeps the reply's binding, must be one
   of its approving options. Nothing is labelled "act": nothing in the
   journal says a reply was unfinished (a reply that asks a question back
   stops before the check runs). A downstream failure, a supervisor
   retry and a move back no longer label it. A threshold stored under
   the old labels never enforces, and the report suggests none for this
   use. `review_triage` and `retry_advice` were re-checked and keep their
   rules.

A run that uses no new feature writes exactly the journal main writes.

## Plan items changed against the code

1. Seeding doubles from a past run's journaled responses (plan 3.3 D1
   item 2) is dropped: the journal holds no response body. `connector_call`
   records the URL, outcome, status and timing only
   (`crates/apb-engine/src/event.rs` lines 1112 to 1168), and putting
   third-party bodies into every production journal to make seeding
   possible would store outside data where external-data runs promise
   not to keep it. A person writes the answers; the captured calls of a
   first eval repetition show what the playbook asked for.
2. "Pair with isolation full and network none" (plan 3.3 D1 item 4)
   applies only to the nodes that bind no connector: V113 refuses
   isolation on a node with `connectors`
   (`crates/apb-core/src/validate/isolation.rs` lines 38 to 47), and a
   doubled node still calls `apb connector call`, which writes the
   journal the sandbox withholds. `docs/EVALS.md` says so.
3. The lift of the `irreversible` refusal (plan section 7.4, the task
   list's section 6 of the combined draft) is not built. A node's declared
   `irreversible` is an effect the engine cannot derive from the node
   type (`crates/apb-core/src/effects.rs` line 121), and an agent with a
   doubled connector still has the network and the person's logged-in
   CLIs, so doubles do not make that work safe. A declared
   `irreversible` stays refused for eval runs exactly as on main. Most
   shipped connector examples declare it (`sentry-triage.yaml` line 15,
   `inbox-triage.yaml`, `release-announce.yaml`, `whatsapp-inbox.yaml`)
   and stay refused; `crm-cleanup.yaml` (`effects: [network, external]`)
   is the worked case. An explicit consent for the actions doubles
   cannot cover would be its own change.
4. Plan 3.3 D5 item 3 ("relabelling without invalidating stored
   thresholds") is met by reuse: labelling rules with versions and the
   v2 thresholds file already exist for `retry_advice`
   (`crates/apb-core/src/decision_thresholds.rs` `versioned_use`, line
   80). `completion_check` joins it.
5. The re-check of `review_triage` and `retry_advice` found no mismatch:
   the triage question asks which option the reviewer will choose and is
   labelled by the reviewer's choice at the same visit
   (`labels.rs` line 452); retry advice asks for the most useful next
   step and is labelled by the next same-executor attempt of the same
   visit (line 530). Both read their own outcome, not a later node's.
6. Plan 3.3 D5 item 1 ("uses only signals that match the question")
   leaves only one signal: a person's approval of the reply at the gate
   that follows it. The draft's "act when a person answered a question
   the reply asked" cannot happen on main: a reply that asks a question
   suspends its attempt before the completion check runs
   (`scheduler/node.rs` lines 2946 to 2968, the check at 3149 and 3410).
   So `reply/1` labels "keep" only (design D5).
7. Size: 44 changed files (design "Code map"), inside the 70 to 90 of
   one pull request.

## Out of scope

1. Scripted gate and question answers, `stop_before` and push checks in
   evals (`answers`, `stop_before`, `checks.pushed` stay refused by
   V80), the `after_eval` promotion policy, an eval dashboard card and
   an MCP eval tool: the rest of `docs/EVALS.md` lines 15 to 19, after
   1.0 (plan 3.3).
2. A built-in scheduler in evals and scripted host tasks in evals: after
   1.0 (plan 3.3).
3. Running a playbook with a declared `irreversible` effect in an eval
   (proposal "Plan items changed" item 3).
4. The inbox wait (`wait_for: { type: inbox }`) and its doubled read and
   acknowledgement: the change `inbox-waits`, implemented after this one
   is merged.
5. A person's own label for a decision (`apb decisions label`) and a
   downstream-outcome column in the report (issue #205's optional item):
   the report already counts coverage; a second question needs its own
   use. A person's label is also the only way `completion_check` could
   get "act" labels (design D5); after 1.0.

## Impact

1. Crates: `apb-core` (`eval.rs` case fields, checks and suite rules; new
   `eval_doubles.rs`; `connector/resolve.rs` `ConnectorFacts.reconcile_functions`;
   `decision_thresholds.rs`; `validate/guarded.rs`; `lib.rs`),
   `apb-engine` (new `eval/doubles.rs`; `eval/checks.rs` and `store.rs`;
   `connector/call/mod.rs`, `target.rs` and `tests.rs`;
   `connector/result.rs`; `connector/run.rs` and `manifest.rs` (the
   doubled snapshot); `run_config.rs`; `run_redact.rs` (the storage
   redactor); `event.rs`; `scheduler/prepare.rs`; `deadline_hooks.rs`;
   `decision/report/labels.rs`, `mod.rs` and `replay.rs`), `apb` CLI
   (`apb eval`, the `--rule` help). No server, MCP or web change.
2. Bundled content: none. `docs/EVALS.md` gets a worked doubles case for
   `examples/playbooks/crm-cleanup.yaml`, which is unchanged.
3. Journal: no new event type. `connector_call.outcome` may read
   `double_unmatched`; `run_provenance.eval_doubles` (`serde(default)`);
   the manifest's `doubled_accounts`. One new guarded feature,
   `eval_doubles` (last in `GuardedFeature::ALL`, 14 entries), for a
   doubled eval run.
4. New files under a run directory: `eval-calls.jsonl`, its lock and
   the reservations' owner locks under `eval-calls.owners/` (doubled eval
   runs only).
5. Behaviour changes for release notes: design "Release notes".
6. Validator codes V280 and V281 of the reserved V280 to V299, checked
   free on efae5822 (design D0). V282 stays reserved and unused; V283
   belongs to `incremental-lists-and-session-fork`, V284 to V286 to
   `inbox-waits`. Names checked free: `double_unmatched`,
   `eval_doubles`, `eval-calls.jsonl`, `eval_call_key_unavailable`,
   `eval_call_delivery_unrecorded`.
7. Size: 44 changed files (design "Code map").

## Decisions taken in the closure pass

No question is left for the owner. The former owner question (the lift
of the irreversible refusal) is decided no: a declared `irreversible`
stays refused for eval runs (proposal "Plan items changed" item 3).

---

# Design: eval doubles

Read the proposal above first. Every decision below names its files, its
functions and the line where the code is today, all read on main
efae5822 (after `typed-inbox-reads` #287, `people-in-the-loop` #288,
`before-running-alone` #289 and `wait-park` #290). If a line moved, find the site by the name
given next to it, never by the number alone. `people-in-the-loop` and
`before-running-alone` change what D5 reads (a review decision names its
request; a person's decision is the one with no `decided_by`; a form
gate re-asks after a scope change), said where it matters.
`start-rules-and-schedules` is not merged and this change needs nothing
from it; if it merges first it only moves lines.

Two decisions, built in the order D0, D5, D1 (the task list): the label fix
(D5) is independent and small; the doubles (D1) are the larger part.
The decision numbers are kept from the plan of the 1.0 sequence, so
D2 to D4 are not in this change (D4 is the change `inbox-waits`, built
after this one is merged).

Nothing here is left to the implementer. Where a choice was open it is
decided and the reason is given; the task list only orders the work.

## D0. Shared pieces

### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V280 | error | D1 | an eval case's `connectors:` doubles are malformed, name a connector or function no binding site of the version binds, or leave a bound function without a double |
| V281 | error | D1 | a case's `checks.connector_calls` entry is malformed or names a function the case does not double |

Checked on efae5822: `rg -ohE '"V2[0-9][0-9]"' crates | sort -u`
prints V200, V210, V211, V212, V220, V221, V222, V240 to V247, V249 and
V250; `rg -ohE '\bV2[0-9]{2}\b'` over `docs`, `crates` and the capability specs adds only
reserved ranges (V201, V209, V213, V219, V223, V229, V248, V251 to
V259). Taken: V240 to V242 (`validate/pilot`, #289), V243 to V247
(`validate/review_form`, #288), V249 and V250 (`validate/readiness`,
#289); reserved: V248 and V251 to V259 for that family, V260 to V279 for
`start-rules-and-schedules`. V280 and V281 are free. V282 is not used:
an earlier draft gave it to a lift of the `irreversible` refusal that
this change does not build (D1 "What stays refused"); it stays reserved
and unused. V283 belongs to `incremental-lists-and-session-fork`, V284
to V286 to `inbox-waits`, and V287 to V299 stay reserved for this
family.

V280 and V281 are suite rules: they live in `crates/apb-core/src/eval.rs`
next to V80 to V83 (`validate_suite_with`) and are reported by
`apb validate` through `apb_engine::eval::checks::validate_suite`
(`crates/apb-engine/src/eval/checks.rs`).

### New and changed journal entries

| Entry | Kind | Decision | Checkpoint |
|---|---|---|---|
| `run_provenance.eval_doubles` | new optional field of an existing event | D1 | written in one write with the start's `run_guard`, for a doubled start only (D1 "The run guard of a doubled run") |
| `connector_call.outcome = "double_unmatched"` | new value of an existing string field | D1 | |

Every new field is `#[serde(default, ...)]` (decision record 0001). No
new event variant.

### What a run that uses no new feature writes

A run whose eval settings carry no doubles writes exactly the journal
main writes: the same events, the same appends, the same order. The one
paired write of this change (`run_provenance` with `run_guard` in one
`EventLog::append_pair`) is used only for a doubled start (D1 "The run
guard of a doubled run" item 3); every other start keeps main's three
separate appends of `run_started`, `run_provenance` and `run_guard`.
The `completion_check` labels (D5) are computed by the report from
journals as they are and write nothing into a run.

### Guarded features

This change adds one `GuardedFeature`
(`crates/apb-core/src/validate/guarded.rs` line 24): `eval_doubles`, a
run feature, not a definition feature, named for a doubled eval run (D1
"The run guard of a doubled run"). `GuardedFeature::ALL` grows
from 13 to 14 entries, the new one last (`approval_binding`,
`person_only`, `required_approval`, `evidence`, `write_scope`,
`previous_success`, `untrusted_input`, `deadline`, `concurrency`,
`review_fields`, `review_max_scope`, `list_pilot`, `wait_park`,
`eval_doubles`). The next changes add theirs after it.

### Files this change adds

1. `crates/apb-core/src/eval_doubles.rs` (D1): the doubles types, matching,
   coverage; declared `pub mod eval_doubles;` in
   `crates/apb-core/src/lib.rs` next to `pub mod eval;`.
2. `crates/apb-engine/src/eval/doubles.rs` (D1): the run side (answer a
   call, capture, read the captures); `pub mod doubles;` in
   `crates/apb-engine/src/eval/mod.rs`.
3. Integration tests (each declared in its crate's `tests/main.rs` in
   alphabetical place as `#[path = "suite/<name>.rs"] mod <name>;`):
   `crates/apb-engine/tests/suite/eval_doubles_test.rs`,
   `completion_labels_test.rs`;
   `crates/apb-cli/tests/suite/eval_doubles_cli_test.rs`.

### What the change `inbox-waits` takes from this one

`inbox-waits` is implemented after this change is merged. It uses, and
this change therefore keeps stable: `RunRedactor::for_storing` (D1
"Answering a call", the storage redactor), `eval::doubles::of_run` and
the doubled answer of an inbox function (D1 "Answering a call" item 3),
the `<node>#inbox` value of `DoubleMatch.site` and `CapturedCall.site`
(documented here as a site a later change adds; `bound_functions` reads
every binding site through `connector_sites`, so the new site is covered
with no change here), and `eval::doubles::reserve`, which `inbox-waits`
extends with the idempotent answer of a wait's acknowledgement.

## D5. Honest completion labels (issue #205)

### The defect

`CompletionLabeller::label` (`crates/apb-engine/src/decision/report/labels.rs`
lines 269 to 348) answers "acting was right" (`Label::Act`) when, in the
window up to the node's next start, a supervisor retried it or moved the
run back (`moved_back_to`, line 227), or the next node failed (lines 325
to 333). The question the model is asked is about the reply itself:
"Is `result` a finished result for `task`, rather than a progress note, a
plan, or a question back to the user?"
(`crates/apb-engine/src/decision/completion.rs` line 79). A later failure
answers a different question.

### What the journal can say about the reply itself

Read on efae5822, so the rule below labels only what the journal proves:

1. Nothing proves a reply unfinished. A reply that asks a question back
   ends its attempt `Suspended` (`scheduler/node.rs` lines 2946 to 2968)
   before the completion check runs (lines 3149 and 3410), and on the live
   transport `question_asked` is journaled during the attempt, before the
   decision. So a `question_asked` never follows a completion decision of
   the same attempt, and the draft's "act when a person answered a
   question the reply asked" can never fire. A rejection at a later gate,
   a failing next node, a supervisor retry and a move back all say the
   work was wrong or failed later, which a finished reply can be.
2. A person's approval at the review gate that is the next visit after
   the reply proves that a person took the reply as the result only when
   that gate approves the reply: its `approves.results` names the checked
   node, so its request is bound to the reply's stored result
   (`review_requested.binding`) and the person decided over it, and the
   decision is one of the gate's approving options
   (`apb_core::schema::Approves::approving_options`, `schema.rs` line
   1698: the declared `approves.options`, else `approve` when it is an
   effective option, else the gate's first option). A gate without
   `approves`, or one that approves other nodes, decides about something
   that need not be the reply, and its decision labels nothing. The
   decision is read as the helper `review_triage` already uses,
   `gate_visit_decision` (`labels.rs` lines 379 to 419), reads it, with
   one difference: a scope re-ask that keeps the binding of this reply
   continues the visit (D5 "The labeller", the chain of re-asks). It
   refuses every other withdrawal and every decision with `decided_by: Some(..)`
   (`unattended`, `model`, `unknown`, `event.rs` line 2537), so one
   definition of "a person decided" serves both uses. It also requires `review_decided.via` (#288) to name a person's channel
   (`terminal` or `dashboard_session`, `DecisionChannel::is_person_channel`):
   a decision with no recorded channel, or one through MCP, a run step or
   any other surface, labels nothing, so an older journal's decisions,
   which carry no channel, stay unlabelled (Review record, round 5).

### The decision: rule `reply/1`, keep labels only

`reply/1` labels a `completion_check` decision `keep` (flagging the reply
was wrong) when a person approved the reply at the gate that follows it
and that gate approves the reply (`approves.results` names the checked
node), with one of that gate's approving options, and leaves every other
decision unlabelled. It labels nothing `act`.

Consequence, stated in `docs/DECISIONS.md` and in the report: with keep
labels only, `apb decisions report` measures the use's false-action rate
(how often the model flags a reply a person approved), which is what an
enforcing check costs; it cannot measure recall, so the promotion rule
(`report/mod.rs` `eligibility`, lines 746 to 800: accuracy above the
majority class) never holds and the report never suggests a threshold for
`completion_check`. A person can still store one with `apb decisions
thresholds set --rule reply/1`. Act labels need a signal about the reply
that the journal does not hold; a person's own label for a decision
(proposal "Out of scope" item 5) is the way after 1.0. This is the honest
figure issue #205 asks for ("leave the rest unlabelled and report
coverage").

### Rule versions

Reuse the rule versions `retry_advice` already has
(`crates/apb-core/src/decision_thresholds.rs`: `RETRY_ADVICE_LEGACY_RULE`
line 72, `RETRY_ADVICE_RULE` line 75, `versioned_use` line 80, the v2 file
`THRESHOLDS_V2_FILE` line 68 with a `rule` per entry, line 94).

```rust
/// The current rule of `completion_check` labels: only a person's approval
/// of the reply at the gate that follows it, labelled `keep`.
pub const COMPLETION_RULE: &str = "reply/1";

/// The current labelling rule of a use whose labels have rule versions.
pub fn versioned_use(use_name: &str) -> Option<&'static str> {
    match use_name {
        "retry_advice" => Some(RETRY_ADVICE_RULE),
        "completion_check" => Some(COMPLETION_RULE),
        _ => None,
    }
}
```

What follows from it on main, with no other change:

1. `stored_threshold_in` (line 149 to 159) looks a versioned use up in the
   v2 file only, under its current rule, so every `completion_check`
   threshold stored before this change (legacy file) stops enforcing at
   once. That is the intended invalidation: it was fitted on wrong labels.
2. `set_threshold_with_rule_in` (lines 236 to 240) already refuses a rule
   other than the current one for a versioned use.
3. `apb decisions thresholds list` marks a legacy `completion_check`
   threshold `legacy, not enforced` through `versioned_use`
   (`apb-cli/src/decisions.rs` lines 258 to 296), and the report lists it
   in its missing-threshold notes (`report/mod.rs` lines 995 to 999).
4. `report/mod.rs` lines 115 and 993 send every row whose rule is not
   `RETRY_ADVICE_LEGACY_RULE` to the v2 file; a `reply/1` row goes there.
   They do not change. The draft's `legacy_rule` helper and
   `COMPLETION_LEGACY_RULE` are dropped: under `reply/1`, chosen in code,
   no row ever carries an older completion rule.

### The labeller

`CompletionLabeller` implements `label_with_rule` (the trait method at
`labels.rs` line 178) returning `(label, COMPLETION_RULE)`, and `label`
returns its first element, as `RetryAdviceLabeller` does (line 526). The
rule is chosen by the code, not by the journal, because the old rule is
wrong for every journal (unlike retry advice, whose old rule is only less
precise).

The approving options come from the gate's definition, which the journal
does not hold: `review_requested` carries the gate's `options` and its
`binding` (the bound nodes), but not `approves.options`, and a
non-approving decision on a bound gate carries the binding nonce as an
approving one does (`review.rs` lines 719 to 745). So the labeller reads
the definition the run executed when the gate asked:

1. The `Labeller` trait gains `fn label_in(&self, r: &DecisionRecord,
   events: &[Event], run_dir: Option<&Path>) -> (Label, Option<&'static str>)`,
   defaulting to `self.label_with_rule(r, events)` (the journal's events
   and run directory, not the `RunJournal`, so `labels.rs` does not import
   the report module: that import is a cycle code-ranker refuses); `RunJournal`
   (`report/mod.rs` line 164) gains `pub run_dir: Option<PathBuf>`, set
   by `RunJournal::load` (line 174) and `None` in fixtures built in
   memory. The report's labelling loop (`report/mod.rs` line 871) and
   the replay (`report/replay.rs` line 266) call `label_in`, so both
   label alike. Every other labeller keeps the default.
2. `CompletionLabeller::label_in` finds the definition of the segment
   holding the final request of the gate's chain of re-asks (below; the
   first request when there is no re-ask): the last boundary at or before
   that request's seq (`run_definitions::boundaries`, `run_definitions.rs` line
   111) whose `definition` digest is set and whose bytes
   `run_definitions::read_stored(run_dir, digest)` (line 88) returns,
   parsed with `Playbook::from_yaml`. A segment that is not `Known` this
   way, a definition that does not parse, or `run_dir: None` leaves the
   decision unlabelled ("the gate's definition is not stored"); `label`
   alone (events only) cannot read a definition and so never labels
   `keep`.

The window and the early unlabelled cases stay as today (lines 270 to
311 and the `restarted` case at 340 to 342: no node, decision not in its
journal, a later attempt of the node followed, the decision's own anomaly
wake above shadow). Then:

1. **Keep** when all of these hold:
   1. the node's next `node_finished` after the decision (seq `F`) has
      status `succeeded`;
   2. the first event after it that names another node and is a
      `node_started` or a `review_requested` is a `review_requested {
      node: gate, plan_digest: None }` (a publish consent carries a
      `plan_digest` and is not a review of the reply; a list's pilot
      request follows its own node's `node_started`, so it is never
      first). The gate's own `node_started` follows its
      `review_requested` (`scheduler.rs` line 2552), so the order holds;
   3. that request's `binding` holds a `BoundSnapshot` of the checked
      node whose `settlement_seq` is `F`: the person was shown this
      reply's stored result, not an earlier one;
   4. the gate in the definition of the final request's segment (above)
      is a `human_review` whose `approves.results` names the checked
      node;
   5. the final person decision of that request's chain of re-asks
      (below) is `Ok((decision, _, _))` with `decision` in
      `approves.approving_options(options)` of that gate.
2. Otherwise **unlabelled**, with the reason the chain walk gave,
   "the gate does not approve the reply" (no `approves`, another node's
   results, no bound snapshot of this execution), "the gate's definition
   is not stored", "the gate's definition changed while it waited", or "no person approved the reply at the gate that
   follows it" (a downstream failure, a supervisor retry, a move back, a
   rejection, a non-approving option and a question included).

The chain of re-asks. A form gate whose scope changed while it waited
writes `review_withdrawn` with the reason `SCOPE_CHANGED_REASON`
(`event.rs` line 3466) and asks
again at once with a fresh `review_requested`
(`scheduler/review_gate.rs` `withdraw_if_rescoped`, line 297, #288). On
main `gate_visit_decision` (`labels.rs` line 379) counts requests as
visits and returns "the gate visit was withdrawn" at the first
withdrawal, so a person's approval on the re-ask would be lost. The
labeller therefore walks the chain, by request seq instead of a visit
count, in a new private helper beside it:

```rust
/// The person's decision on the request at `first` of gate `gate` and on
/// every re-ask that keeps its binding of `node`'s snapshot settled at
/// `settlement` with no `patch_applied` since `first`: `Ok((decision,
/// note, final_request_seq))` of the last request of the chain, or why
/// there is none.
fn chain_decision<'a>(
    events: &'a [Event],
    gate: &str,
    first: u64,
    node: &str,
    settlement: u64,
) -> Result<(&'a str, &'a str, u64), &'static str>;
```

1. Start at the request with seq `first`. Scan forward. A
   `review_decided` of the gate while the request is open is read as
   `gate_visit_decision` reads it today (a `decided_by: Some(..)` gives its refusal), and then only when its `via` is a person's channel (`terminal` or `dashboard_session`; another channel ends the walk "the gate visit was decided through no person's channel", none "the gate visit's decision records no channel"); else `Ok`.
2. A `review_withdrawn` of the gate for the open request continues the
   chain only when all hold: its reason is a scope re-ask (`event::is_scope_changed`, `event.rs`
   line 3470: `SCOPE_CHANGED_REASON` or its `scope_changed:` form); the
   next event of the gate is a `review_requested` (the re-ask), with no
   `node_started` of any other node between the withdrawal and it; and
   that request's `binding` holds a `BoundSnapshot` of `node` whose
   `settlement_seq` is `settlement`; and no `patch_applied` lies between
   the chain's first request and the re-ask. The re-ask becomes the open
   request and the scan goes on. Any other withdrawal (another reason, no re-ask,
   a re-ask whose binding no longer names this snapshot) ends the walk unlabelled "the gate visit was withdrawn". Any `patch_applied` after the first request ends the walk "the gate's definition changed while it waited", re-asked or not: no decision across a patch is read as an approval (Review record, round 5).
3. A `review_requested` of the gate that is not a re-ask (the next
   visit) while a request is open ends the walk "the gate visit was never
   decided", as today.
4. The chain ends at a definition boundary: a re-ask after a
   `patch_applied` that follows the chain's first request ends the walk
   unlabelled "the gate's definition changed while it waited", whatever
   the person decided. A patch may replace the gate while it waits
   (`scheduler/patch.rs` line 264 stores the new definition and journals
   `patch_applied.definition`, a boundary of `run_definitions::boundaries`,
   line 111), and an option that approved under the first definition may
   not approve under the second, so no decision across a patch is read
   as an approval. A resume boundary does not end the chain; the
   definition read is always the final request's segment (item 2 of the
   list above), so a resume that executes other bytes is read with those.
5. The end of the journal ends it "the gate visit is not decided yet".

`gate_visit_decision` itself is unchanged and still serves
`review_triage`, whose question is about one visit's request.

`label_source` returns
`"the reply itself: a person approved it at the review gate that follows it and approves its result (keep); nothing in the journal says a reply was unfinished, so none is labelled act"`.

`moved_back_to` (line 227) is then unused by the completion labeller;
delete it if no other caller uses it (clippy `dead_code` says).

### Coverage

`apb decisions report` already prints label coverage (labelled of
answered, `docs/DECISIONS.md` line 377). Nothing new is computed: the
lower coverage is the honest figure. The `completion_check` row carries
`rule: reply/1` like the retry-advice rows, and its eligibility lines
name "accuracy not above the majority class".

### Docs

`docs/DECISIONS.md` line 402 (the `completion_check` row of the Labels
table) becomes:

> wrong to act (flag the reply) when a person approved the reply at the
> review gate that follows it, and that gate approves the reply's result
> (`approves.results` names the node; one of its approving options).
> Nothing labels acting right: a reply that
> asks a question back stops before the check runs, and a later node
> failing, a supervisor retry or a move back say something went wrong
> later, not that the reply was unfinished. Labelled under rule
> `reply/1`; thresholds stored under the old labels never enforce, and
> the report suggests none for this use (it measures only the
> false-action rate).

The "rule versions" paragraph (line 438) names both uses. The
`review_triage` and `retry_advice` rows stay; add one sentence after the
table: "Each use's label answers that use's own question: the review
triage is labelled by the same gate visit's decision and retry advice by
the next same-executor attempt of the same visit, never by a later node."

### Wrong implementations

1. Keeping the downstream rule and only dropping the "next node failed"
   branch: a supervisor retry still labels "act". Caught by
   `a_supervisor_retry_leaves_the_reply_unlabelled`.
2. Changing the labeller without a rule version, so stored legacy
   thresholds keep enforcing. Caught by
   `a_threshold_stored_under_the_old_rule_no_longer_enforces`.
3. Labelling "keep" whenever the run succeeded with no question asked.
   Caught by `a_finished_run_without_a_gate_stays_unlabelled`.
4. Treating a `reject` at the following gate as "act". Caught by
   `a_rejected_reply_stays_unlabelled`.
5. Counting an unattended or model decision at the gate as a person's.
   Caught by `an_unattended_approval_leaves_it_unlabelled`.
6. Counting a publish consent after the node as a review of the reply.
   Caught by `a_publish_consent_is_not_a_review_of_the_reply`.
7. Reading the literal option `approve` as approval: a gate whose
   `approves.options` is `[ship]` misses its approvals, and a plain gate
   without `approves` labels a decision about something else. Caught by
   `a_declared_approving_option_labels_keep` and
   `a_gate_without_approves_leaves_it_unlabelled`.
8. Accepting a gate whose `approves.results` names another node, or a
   binding of an earlier execution. Caught by
   `a_gate_approving_another_node_leaves_it_unlabelled` and
   `a_binding_of_an_earlier_execution_leaves_it_unlabelled`.
9. Stopping at the first withdrawal, so a person's approval on a
   scope re-ask that still binds this reply is lost; or following a
   re-ask whose binding names another snapshot. Caught by
   `an_approval_on_a_scope_reask_labels_keep` and
   `a_reask_binding_another_snapshot_leaves_it_unlabelled`.
10. Following a re-ask across a patch, or reading the approving options
    from the first request's definition: a patch that makes the chosen
    option non-approving while the gate waits yields a false keep.
    Caught by `a_reask_after_a_patch_leaves_it_unlabelled` and
    `the_final_requests_definition_is_read`.

### Tests

1. `crates/apb-engine/src/decision/report/labels.rs` `mod tests`: the
   label tests named above plus `an_approval_at_the_next_gate_labels_keep`
   (a run directory with the stored definition of a gate that `approves`
   the node), `label_alone_never_labels_keep` (the same journal through
   `label`, no definition), `an_unstored_definition_leaves_it_unlabelled`,
   `nothing_is_labelled_act_under_reply_1`,
   `an_approval_on_a_scope_reask_labels_keep` (the request is withdrawn
   `scope_changed`, the gate asks again at once with a binding of the
   same settlement, a person approves the re-ask),
   `a_reask_binding_another_snapshot_leaves_it_unlabelled`,
   `a_reask_after_a_patch_leaves_it_unlabelled` (a `patch_applied` with a
   stored definition in which the gate's chosen option is not approving
   lies between the first request and the re-ask; the person approves
   the re-ask with that option; unlabelled "the gate's definition changed
   while it waited", and with the patch removed the same journal labels
   `keep`),
   `the_final_requests_definition_is_read` (a resume boundary with
   other stored bytes between the first request and the re-ask: the
   approving options come from the re-ask's segment),
   `a_withdrawal_without_a_reask_leaves_it_unlabelled` and
   `a_finished_report_followed_by_a_failing_node_is_not_act` (the issue's
   acceptance case); they replace `the_next_node_decides_the_completion_label`
   (line 934) and `a_retry_or_a_move_back_labels_act` (line 961).
2. `crates/apb-core/src/decision_thresholds.rs` `mod tests` (line 365):
   `completion_check_is_a_versioned_use` and
   `a_threshold_stored_under_the_old_rule_no_longer_enforces`.
3. `crates/apb-engine/tests/suite/completion_labels_test.rs`:
   `a_report_on_a_finished_reply_and_a_failing_next_node` (a fixture run
   whose completion check answered `final_result` high and whose next node
   failed: the report computes the row, the decision is unlabelled, the
   row's rule is `reply/1` and no threshold is suggested).

## D1. Connector doubles in evals

### The case format

`crates/apb-core/src/eval.rs`: `LATER_FIELDS` (line 49) becomes
`["answers", "stop_before"]` and `LATER_CHECKS` (line 50) becomes
`["pushed"]`; the refusal message at line 351 ("it needs mock
connectors, a later release") becomes "`<field>` is not supported by
this apb (scripted answers, stop_before and push checks are a later
release)", since mock connectors are what this change builds. `EvalCase`
(line 55) gains, after `env`:

```rust
    /// Recorded answers per connector function (`connectors:` in the
    /// file). A case with doubles runs every connector call against them
    /// and none against the service.
    #[serde(default, rename = "connectors", skip_serializing_if = "BTreeMap::is_empty")]
    pub doubles: crate::eval_doubles::Doubles,
```

and `CaseChecks` (line 125) gains:

```rust
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connector_calls: Vec<crate::eval_doubles::CallCheck>,
```

New `crates/apb-core/src/eval_doubles.rs`:

```rust
//! Connector doubles of an eval case: recorded answers per connector
//! function, the rule that picks the answer for one call, and which
//! functions of a version's binding sites a case covers. Shared by the
//! suite validator (V280 and V281), `apb eval` and the engine.

/// connector -> function -> answers, in the order written.
pub type Doubles = BTreeMap<String, BTreeMap<String, Vec<DoubleAnswer>>>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleAnswer {
    /// When this answer applies; absent: every call.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub matcher: Option<DoubleMatch>,
    /// The recorded answer, in the shape of a `mock` function.
    pub answer: crate::connector::def::MockSpec,
    /// How many calls this answer serves; absent: every matching call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleMatch {
    /// A JSON object every key of which the call's args hold with a
    /// value that matches (objects recursively, everything else equal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Text some string value of the args contains (case-sensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Only calls made at this binding site (a node id, `<node>#check`,
    /// `@baseline`, `@follow_up`, or a site a later change adds to
    /// `connector_sites`, such as `<node>#inbox` of `inbox-waits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

/// One `checks.connector_calls` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CallCheck {
    /// `<connector>.<function>`.
    pub function: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// A JSON object every key of which the call's input arguments hold:
    /// the arguments the call was made with, before the connector's text
    /// modes, the neutralizing of forged markers and an appended send
    /// marker; never the body a real request would carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Text some string value of the call's input arguments contains
    /// (case-sensitive), read as `args` is: before text modes and markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Exactly this many selected calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

/// Whether `pattern` (an object) is held by `args`.
pub fn args_hold(pattern: &Value, args: &Value) -> bool;
/// Whether some string leaf of `args` contains `text`.
pub fn args_contain(args: &Value, text: &str) -> bool;
/// Whether `m` holds for a call at `site` with `args`.
pub fn matches(m: &DoubleMatch, site: &str, args: &Value) -> bool;

/// The answer for one call: the index of the first answer of the
/// function whose `match` holds and whose `times` is not used up
/// (`used[i]` is how many calls answer `i` already served), or `None`.
pub fn pick(answers: &[DoubleAnswer], site: &str, args: &Value, used: &[u32]) -> Option<usize>;

/// The functions every binding site of `playbook` may call, from the
/// installed connectors' facts: `all` is every function, `read_only`
/// every function not in `write_functions`, a list is that list, over
/// every site `Playbook::connector_sites` walks (so a site a later change
/// adds there is covered with no change here). Each function
/// that declares `idempotency` adds its `reconcile.function` (the
/// send ledger's read-back runs through the same call path with
/// `CallOrigin::Reconcile` and is doubled too; read from the new
/// `ConnectorFacts.reconcile_functions`, a map of function to its
/// reconcile function filled in `resolve.rs` beside `idempotency`), and
/// with it the connector's `identity.function`
/// (`ConnectorFacts.identity_function`, set only for a connector with a
/// `marker` send): a marker read-back reads the own identity first,
/// through the same call path (Review record, round 5). A
/// connector missing from
/// `facts` is reported, never guessed.
pub fn bound_functions(
    playbook: &Playbook,
    facts: &BTreeMap<String, ConnectorFacts>,
) -> (BTreeSet<(String, String)>, Vec<String>);

/// Why `case` cannot run `playbook` with its doubles: every bound
/// function without an answer, every connector the facts lack, and
/// every double of a function no site binds. Empty when the doubles
/// cover the version exactly enough to run.
pub fn coverage_gaps(
    playbook: &Playbook,
    doubles: &Doubles,
    facts: &BTreeMap<String, ConnectorFacts>,
) -> Vec<String>;
```

Matching rules, exactly:

1. `args_hold(p, a)`: when `p` is an object, `a` is an object and every
   key `k` of `p` is in `a` with `args_hold(p[k], a[k])`; otherwise
   `p == a` (arrays must be equal, numbers compared as JSON values).
2. `args_contain` walks objects and arrays; numbers, booleans and null
   never contain text.
3. A `DoubleMatch` holds when every field it sets holds; an empty
   `match: {}` holds always.
4. `pick` walks the answers in order; the first holding one with
   `times.is_none_or(|t| used[i] < t)` answers. Answers without `times`
   after ones with it make a sequence: `[{answer: A, times: 1}, {answer:
   B}]` answers A once, then B for ever.

### Validator: V280 and V281

`case_problems` (line 572) is unchanged; `validate_suite_with` (line 735)
gains a parameter `facts: &BTreeMap<String, ConnectorFacts>` (the engine
wrapper `apb_engine::eval::checks::validate_suite`, line 117, passes
`apb_core::connector::resolve::validation_facts()`; the core
`validate_suite`, line 728, passes an empty map and so reports the
connector rules as "cannot be checked without the installed
connectors"). Both suite callers, `crates/apb-cli/src/eval.rs` line 1779
(`snapshot_suite`) and `crates/apb-cli/src/run.rs` line 287, call the
engine wrapper and do not change. For each loaded case that applies to
the version:

1. V280 (error), only for a case that declares doubles (a non-empty
   `connectors:`): for each string of `coverage_gaps`, prefixed `eval
   case <id>: `; for an answer whose `status` is outside 100 to 599 or whose
   `times` is 0; for a `match.args` that is not an object; for a
   `match.site` that is not a binding site of the version.
2. V281 (error) for a `connector_calls` entry whose `function` is not
   `<connector>.<function>` or names a function the case does not double;
   that sets `count` together with `min` or `max`; whose `min > max`;
   whose `args` is not an object; whose `site` is not a binding site.

`refusal` (line 466) loses only its connector leg (lines 471 to 479),
which moves to `case_refusal` (below): every other leg (the playbook's
`irreversible` effects at line 468, a node's `irreversible` or `secrets`
declaration and a shipping step through `node_shipping_reason`, a
sub-playbook) stays in `refusal` exactly as on main, for every case,
doubled or not (D1 "What stays refused"). V81 reports, per case,
`case_refusal` without the coverage gaps (those are V280), so a case
without doubles of a connector playbook gets exactly today's reasons
under V81, and a fully doubled case gets the reasons of `refusal` only.

New in `eval.rs`:

```rust
/// Every refusal for running `case`: [`refusal`], then for a case without
/// doubles today's connector sites (one reason per site that binds a
/// connector, the text of main), for a case with doubles its coverage
/// gaps ([`crate::eval_doubles::coverage_gaps`]).
pub fn case_refusal(
    playbook: &Playbook,
    case: &EvalCase,
    facts: &BTreeMap<String, ConnectorFacts>,
) -> Vec<String>;
```

The connector reason's text changes from "mocks are not supported yet"
to "eval runs never call real connectors; give the case `connectors:`
doubles for every function it binds", since doubles are what this
change builds.

Where a refusal reaches the person in `apb eval` (`crates/apb-cli/src/eval.rs`):
`eval_cmd` (line 1634) calls `core_eval::refusal(&playbook)` at line 1651
(case-independent, `eval_refused_effects`, exit 2), asks the launch
policy (line 1705), then `snapshot_suite` validates the suite (line 1779)
and refuses `invalid_suite` naming the codes. So a case without doubles,
or with partial doubles, of a connector playbook is refused `invalid_suite`
with V81 or V280 before anything runs, where 0.24 refused it
`eval_refused_effects`. Both exit 2 before anything runs; the release
notes name the changed code. `run_eval` rechecks `case_refusal` per
selected case (D1 "Running a doubled case" item 2) with the facts read
then, because the installed connectors may have changed between the
validation and the run.

### What stays refused

A declared `irreversible` effect stays refused for eval runs exactly as
on main: the playbook's own `effects`, the effective effects (a node's
declaration widens them, `effects.rs` line 121: a declaration names an
effect the engine cannot derive from the node type) and every node's
declaration, whatever the case's doubles cover. Reason: doubles answer
the calls a node makes through `apb connector call`; they do not reach
what an agent can still do with the network and the person's logged-in
CLIs, so a node that says it does something irreversible can still do
it in a doubled run. An earlier draft lifted the refusal for a fully
doubled case; it is not built, and no case-level escape replaces it.

What this leaves evaluable: a playbook that binds connectors and
declares no `irreversible` effect, such as the shipped
`examples/playbooks/crm-cleanup.yaml` (`effects: [network, external]`,
the Twenty connector at the baseline and three nodes; the worked case in
D1 "Docs"). Most other shipped connector examples declare `irreversible`
(`sentry-triage.yaml` line 15, `inbox-triage.yaml`,
`release-announce.yaml`, `whatsapp-inbox.yaml` and others) and stay
refused `eval_refused_effects`, as on main. A separate, explicit consent
for the actions doubles cannot cover would be its own change.

### Running a doubled case

`crates/apb-cli/src/eval.rs`:

1. `eval_cmd` (defined at line 1634): `core_eval::refusal(&playbook)` at
   line 1651 stays (the case-independent part), then the suite validation
   of `snapshot_suite` (line 1779) as above.
2. `run_eval` (line 1829), after the cases are selected (line 1844) and
   before any repetition is planned: for each selected case compute
   `core_eval::case_refusal(playbook, &lc.case, &validation_facts())`; any
   non-empty result refuses the invocation with `eval_refused_effects`
   (the same JSON shape as line 1655, each reason prefixed with the case
   id) and exit 2, before anything is materialized. This is the recheck
   of the validation with the facts read now.
3. `run_repetition` (line 1326): the `EvalRunSettings` built at line 1354
   gets `doubles: (!lc.case.doubles.is_empty()).then(|| lc.case.doubles.clone())`,
   and, for a doubled case, a `PATH` in its `spawn_env` whose first entry
   is the directory of this binary (`apb_core::fsutil::reexec_exe()`'s
   parent), followed by the inherited `PATH` of the `apb eval` process
   and nothing else (a case cannot set `PATH`: the suite validation
   refuses it, `apb-core` `eval.rs` line 499). Reason: an agent step answers its connector calls by
   running `apb connector call` from its `PATH`; an older `apb` there
   does not know doubles. It cannot reach a service either (the doubled
   snapshot, below, names no account any binary can resolve), but every
   call it makes fails `config`, so the case would fail for a reason
   that is not the playbook's. Putting this binary first reuses the
   existing case env overlay (`spawn_env`, applied to every agent and
   script the run spawns, `run_config.rs` line 311). It is for the case
   to work, not for safety.
4. After the run ended, before `CheckInput` is built (line 1402), read the
   captured calls `apb_engine::eval::doubles::read_calls(&run_dir)` and
   hand the result to `CheckInput` (new field `calls: Result<&'a
   [CapturedCall], &'a CaptureError>`), so the checks, not the runner,
   fail a capture that does not read (D1 "The checks").
5. An `--external-data` repetition with doubles is never kept (Cut,
   below). Scrubbing removes secret values, not outside data, and
   `--external-data` promises that such data is kept nowhere. So, in
   `run_repetition`, right after `checks::evaluate` returned, a doubled
   repetition run with `--external-data` removes its capture (`CAPTURE_ENTRIES`) and then its whole repetition directory, both in `remove_external_repetition` over `fsutil::beneath`, whatever the
   outcome and whether or not a driver or helper still runs; it is neither
   stored nor kept. A directory a step made read-only gets its private
   mode back through its descriptor and the removal is retried. When it
   still cannot be removed, the repetition fails with the check
   `external_data_removal` naming the path, and nothing says it was
   removed. The checks have read the capture by then, so
   `Repetition.connector_calls` (a summary without args) and the check
   results are reported. A repetition without `--external-data` keeps its capture and, when it cannot be stored, its tree as before. The rule is: apb removes the repetition directory it created, following no link and touching nothing outside it (`fsutil::beneath`: `remove_tree`, which unlinks a link, and the new `make_dirs_private`, which gives each directory mode 0700 through its own descriptor, run before the check). Detecting a moved run tree is best effort: a component that is a link, missing or unreadable is reported with its path and fails the repetition; a directory put in the place of the original is not detected (no device and inode tracking). A capture or repetition removal that fails is that error too. A filesystem a step mounted inside the repetition directory is removed with it like any other content. What this covers: what apb wrote and what lies in the repetition directory when it is removed. It does not contain the evaluated agent. A process the agent left running, or the agent itself during the run, can write the data it was given anywhere its user can write, a path it recreates included; containing that is the node's isolation (docs/ISOLATION.md), not the eval runner, and docs/EXTERNAL-DATA.md limits the external-data promise to what apb itself keeps. For an external-data eval of an agent you do not trust, give its agent nodes `isolation: full`.
6. The dry-run plan (`--dry-run`) lists, per case, the doubled functions.

`crates/apb-engine/src/run_config.rs` `EvalRunSettings` (line 290,
deriving `PartialEq, Eq`, which is why the doubles types derive `Eq`)
gains:

```rust
    /// The case's connector doubles: every connector call of the run is
    /// answered from them or refused; none reaches a service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doubles: Option<apb_core::eval_doubles::Doubles>,
```

and `RunConfig` gets `pub fn doubles(&self) -> Option<&Doubles>` next to
`spawn_env` (line 311).

Secrets at the start: unchanged. The run gate's connector check refuses
`connector_env_missing` when an account's variable is not set
(`gate/mod.rs` lines 1304 and 1305, from `gate/trust_scan.rs` line 57),
and the snapshot's step 4 (`connector/run.rs` lines 114 to 130) refuses
the same; both test that the variable is present
(`secrets::missing_vars`), never read its value and never run a
`{{cmd:...}}` helper. A doubled case therefore needs each bound
connector installed and an account configured and trusted, as a real
run does, and each referenced variable present with any value (a CI
exports placeholders); no call of the run reads it. Decided against a
gate exception for doubled runs: it would be a second gate path keyed on
a settings file the run's own argument names, for a check that leaks
nothing. `docs/EVALS.md` says so.

The doubled snapshot. A doubled run's manifest names no account any
binary can resolve, so no apb, of this version or an older one, can make
a real connector call through the run's snapshot, whoever invokes it.
This is the one property that keeps a doubled run from reaching a
service through a connector; the run guard below narrows what an older
apb does with the run, it does not carry that guarantee:

1. `crates/apb-engine/src/connector/run.rs` `snapshot_connectors` (line
   41) gains a parameter `doubled: bool` (`scheduler/prepare.rs` line 350
   passes `opts.eval` carrying doubles). Steps 1 to 5 are unchanged (the
   permit, the account digests, the presence of the accounts' variables,
   the connector copy). Step 6, for a doubled run, writes each
   `ManifestConnector` with `accounts: []` and the new
   `#[serde(default, skip_serializing_if = "Vec::is_empty")]
   doubled_accounts: Vec<DoubledAccount>` (`crates/apb-engine/src/manifest.rs`
   `ManifestConnector`, line 264; `DoubledAccount { name: String,
   default: bool }`, the account's name and default flag, no field, no
   variable name, no command). Grants keep their account names.
2. Why it holds for every binary: a snapshot call resolves its target
   first (`connector/call/target.rs` `snapshot`, line 46): the manifest,
   the grant, `select_account`, then the granted account's record in
   `mconn.accounts`, failing `config` "granted account `<a>` is not in
   the connector snapshot" when it is absent. That lookup runs before
   the connector definition is loaded, before any secret is resolved and
   before any request is built, and every apb with connectors has it
   (it shipped with connectors, #19). An apb that does not know doubles
   ignores `doubled_accounts` (`ManifestConnector` reads unknown fields
   leniently) and finds no account: every call fails `config`, nothing
   is sent, no secret is read and no `{{cmd:...}}` helper runs. This
   covers an older `apb connector call` an agent runs by an absolute
   path, a run resumed by an older apb, and a start torn before its
   guard (below).
3. This binary: `target::snapshot` gains `doubles: Option<&Doubles>`
   (`call/mod.rs` `prepare` computes `eval::doubles::of_run` once, as its
   first statement, and passes it). For a doubled run it selects the
   account with `select_account` over a view of the connector whose
   accounts are the `doubled_accounts` (so a default account is picked
   as a real run picks it), and builds the target with an empty
   `ManifestAccount` (no fields, no `env`, no `cmd`, digest empty).
   A doubled run whose manifest holds an account in `accounts`, or an
   undoubled run whose manifest holds `doubled_accounts`, fails `config`
   ("the run's snapshot and its eval settings disagree"), nothing sent.
4. The prompt's connector block (`connector/prompt.rs`
   `instruction_block`) already lists a granted account it finds no
   record of by its name alone (the `None` arm); a doubled run's prompt
   shows the account names without fields, and needs no change.

Notifications are live calls (the installed connector and account, not
the snapshot): this binary refuses them in a doubled run (below). An
older apb sends a run's notification only to a target the journal pins
in `notify_set`, which the start appends after its provenance and guard
(`prepare.rs` line 1686); a journal that holds `notify_set` therefore
holds the guard, and an older apb that knows run guards refuses to drive
it. A start torn before `notify_set` has no pinned target. No claim is
made beyond that.

### The run guard of a doubled run

An older apb reads `run.yaml` leniently and drops the unknown `doubles`
field, so it would drive a doubled run as an ordinary one. What keeps
such a drive from reaching a service through a connector is the doubled
snapshot above, and nothing else: every connector call of the run fails
`config` before a definition, a secret or a request, in every binary.
The run guard narrows the cases in which an older apb drives the run at
all; it is the standing contract of every guard on main, not a new
promise:

1. `RunProvenance` (`event.rs`) gains `#[serde(default, skip_serializing_if
   = "std::ops::Not::not")] eval_doubles: bool`, written true at start by
   `scheduler/prepare.rs` when `opts.eval` carries doubles.
2. `apb_core::validate::GuardedFeature::EvalDoubles` (`as_str`
   `"eval_doubles"`, last in `ALL`, which grows from 13 to 14; the module
   doc gains: an apb that does not know doubles would drive a doubled
   eval run as an ordinary one). `guarded_features` never names it (it is
   not a definition feature); `scheduler/prepare.rs` `run_guard` names it
   when the journal's `run_provenance` has `eval_doubles: true`, as it
   names `concurrency` from `run_provenance.concurrency`, and
   `run_guard_of` gains the matching `eval_doubles: bool` argument. A
   patch recomputes the guard through `run_guard` (`scheduler/patch.rs`
   line 259) and keeps it.
3. For a doubled start only, the guard goes into the journal with
   `run_provenance` through `EventLog::append_pair` (`event.rs` line
   2949), which writes a non-checkpoint event and a second one in one
   write under one lock and updates the run guard tracker for both lines
   (`append_lines`, the loop at line 3106). `run_provenance` is not a
   checkpoint, so a doubled start becomes `log.append_pair(provenance,
   |_, _| {}, |_| guard)`. Every other start keeps main's separate
   appends of `run_started`, `run_provenance` and `run_guard`
   (`prepare.rs` lines 1594, 1610 and 1644), so a run that uses no new
   feature writes exactly the journal main writes (D0 "What a run that
   uses no new feature writes"). Nothing is appended between the
   provenance and the guard on main (`prepare.rs` lines 1610 to 1645), so
   the pair keeps the same lines in the same order.

   Crash points of a doubled start, in journal order (the manifest with
   the doubled snapshot is written before `run_started`, line 1579):
   1. Before `run_started`: no run is visible to a resume; the run
      directory holds a manifest and a `run.yaml` only, and nothing can
      call a service through it (the doubled snapshot).
   2. After `run_started`, before the pair: the window main has for every
      guarded feature (Review record, pre-existing). An older apb may
      drive the run; every connector call fails `config` (the doubled
      snapshot); its agents and scripts can run, as for any guarded run
      torn the same way. No new start marker is added: eval runs are not
      resumed by people in practice.
   3. A write of the pair cut at the end of its first line: the
      provenance reads alone (as `append_pair` documents), the same as
      crash point 2.
   4. A write cut inside the second line: the torn last line fails every
      strict read, as a torn single append does; a stop still goes
      through (`read_all_for_stop`).
   5. After the pair: the guard is in force (item 4).

   The pair is a narrowing, not an atomic boundary: what keeps a doubled
   run from a service through a connector is the account-free snapshot
   alone.
4. After the start, every checkpoint carries the guard (the event log),
   so an apb that knows run guards but not `eval_doubles` refuses to
   resume or pause a run whose journal holds it, and still stops it (the
   run guard's rule since #288).

### Answering a call

New `crates/apb-engine/src/eval/doubles.rs`:

```rust
/// The capture file of a doubled run, under the run directory.
pub const CALLS_FILE: &str = "eval-calls.jsonl";
const CALLS_LOCK: &str = "eval-calls.lock";
/// The directory of the reservations' owner locks, under the run directory.
const OWNERS_DIR: &str = "eval-calls.owners";
/// The most bytes of one call's scrubbed args kept in a capture.
pub const CAPTURED_ARGS_MAX_BYTES: usize = 16 * 1024;
/// The MAC domain of a call's identity (`apb_core::run_origin::tag`).
const CALL_KEY_DOMAIN: &str = "eval-call/1";

/// One line of [`CALLS_FILE`]: a reservation, written and synced before
/// the call is dispatched, or the delivery of an earlier reservation,
/// written and synced before the call returns its answer to its caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureLine {
    Reserved(CapturedCall),
    Delivered { reservation: u64 },
}

/// One reserved call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedCall {
    /// The line number of the reservation in [`CALLS_FILE`].
    pub seq: u64,
    /// The binding site: the node id, `<node>#check`, `@baseline`,
    /// `@follow_up`, or a site a later change adds (`<node>#inbox`).
    pub site: String,
    pub connector: String,
    pub function: String,
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The call's identity: a MAC under this installation's key of the
    /// canonical JSON of site, connector, function, account, item and the
    /// raw args, so two calls compare equal without the raw args being
    /// stored. Never empty: a call whose key cannot be computed is
    /// refused before a reservation is written (below).
    pub call_key: String,
    /// The call's input arguments (as the call was made, before the
    /// connector's text modes and send markers), through the run's
    /// storage redactor; `{"clipped":
    /// true}` when they are longer than [`CAPTURED_ARGS_MAX_BYTES`],
    /// `{"withheld": "secrets_unresolved"}` when the redactor refuses.
    pub args: Value,
    /// Whether `args` holds the whole scrubbed args. An assertion that
    /// reads args never decides over a capture that does not.
    #[serde(default)]
    pub args_kept: ArgsKept,
    /// The index of the answer that served it; `None`: unmatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<usize>,
    /// Set by [`read_calls`]: a `delivered` line names this reservation.
    #[serde(skip)]
    pub delivered: bool,
}

/// The doubles of the run in `run_dir` (its `run.yaml` eval settings),
/// `Ok(None)` for a run that has none. Fails closed: a `run.yaml` that
/// does not read, or a journal whose `run_provenance` says
/// `eval_doubles: true` while the settings name none, is an error
/// (`CallErrorCode::Config`), never "no doubles".
pub(crate) fn of_run(run_dir: &Path, events: &[Event]) -> Result<Option<Doubles>, CallError>;

/// Reserves the answer of one call of a doubled run (below) and returns
/// it with the reservation it holds, or `double_unmatched`.
pub(crate) fn reserve(
    run_dir: &Path,
    root: &Path,
    doubles: &Doubles,
    call: &DoubledCall<'_>,
) -> Result<Reservation, CallError>;

/// The reservation a call holds until its answer is delivered: the
/// line's seq, the answer, and the owner lock (`HeldLock`) of the
/// reservation, released on drop.
pub(crate) struct Reservation { pub seq: u64, pub answer: Result<MockSpec, CallError>, owner: HeldLock }

/// Appends the `delivered` line of `r` under [`CALLS_LOCK`], syncs the
/// file (`sync_data`) and then releases its owner lock. An error when the
/// line cannot be written or synced: the caller then returns that error
/// instead of the answer (below), and the reservation stays undelivered.
pub(crate) fn deliver(run_dir: &Path, r: Reservation) -> Result<(), CallError>;

/// Every reservation of the run, in order, each with `delivered` set
/// from the `delivered` lines; `Ok(empty)` without the file. A line that
/// does not parse (torn or corrupt) is an error naming its line number,
/// never skipped.
pub fn read_calls(run_dir: &Path) -> Result<Vec<CapturedCall>, CaptureError>;

/// `whole`, `clipped` or `withheld`; reads as `withheld` when absent, so
/// a capture written without it never counts as whole.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsKept { Whole, Clipped, #[default] Withheld }

/// A capture file that does not read: its line and why.
pub struct CaptureError { pub line: usize, pub reason: String }

/// Every entry of the capture under the run directory: [`CALLS_FILE`],
/// [`CALLS_LOCK`] and [`OWNERS_DIR`]; `remove_external_repetition` removes
/// them (D1 "Running a doubled case" item 5).
pub const CAPTURE_ENTRIES: [&str; 3];
```

`DoubledCall` carries `site`, `connector`, `function`, `account`,
`item` and `args`. Every write goes under `apb_core::fsutil::lock_dir(run_dir,
CALLS_LOCK)` (`fsutil.rs` line 901), and each line is one `write` of one
line opened with `O_APPEND` at mode 0600.

`reserve`, under [`CALLS_LOCK`]:

1. Read the capture file with the strict reader of `read_calls`; a line
   that does not parse fails the call `config` ("the call capture does
   not read"), nothing answered and nothing sent, so a corrupt capture
   can never hand out a `times` answer twice.
2. Compute the call's `call_key` in memory (`apb_core::run_origin::tag(
   CALL_KEY_DOMAIN, ..)`, `run_origin.rs` line 126); the raw args are
   never written. `tag` creates the installation key when there is none
   and returns an error when it cannot (no config directory, a key that
   cannot be written or read, an unusable key); it never returns an empty
   tag. That error refuses the call, fail closed:
   `CallError::new(CallErrorCode::Config, "eval_call_key_unavailable:
   this installation has no usable key, so a doubled call cannot be
   identified; nothing was answered")`, before any line is written and
   before any answer is picked. A doubled call is never answered without
   an identity: a reservation without one could not be replayed to its
   retry, and its `times` answer would be spent by a call that never
   received it.
3. Replay: when an undelivered reservation has the same non-empty
   `call_key` and its owner lock `OWNERS_DIR/<seq>.lock` can be taken
   (`apb_core::fsutil::try_hold_lock`, the one-attempt probe of a
   `HeldLock`: free means the process that reserved it is gone), return
   that reservation with the same answer, now owned by this call. No
   line is written and no `times` is used: the caller that crashed
   between its reservation and its delivery never received the answer,
   and the retry gets it. A reservation whose owner lock is held belongs
   to a live call (two identical calls at once): it is not replayed, and
   this call reserves its own.
4. Otherwise pick the answer with `pick`, counting `used[i]` as the
   reservations (delivered or not) with answer `i`, take the owner lock
   of the next seq (held before the line exists, so no reservation is
   ever without a live owner until its process ends), append the
   `reserved` line (`seq` is the line count before the append), and sync
   the file (`sync_data`) before `CALLS_LOCK` is released and `reserve`
   returns. A reservation line that cannot be written or synced fails the
   call `config` ("the call capture could not record this reservation"),
   nothing answered and nothing sent. The call is dispatched only after
   its reservation is durable, so a power loss at any later point leaves
   the reservation on disk and the checks can never read "no call" for a
   call that was dispatched.

The call then dispatches (`PreparedCall::Double`, below), the
`connector_call` event is journaled as for any call, a tracked send is
settled through the send ledger (`settle_send`), and the last step of
`execute`, right before it returns the result to its caller, is
`deliver`. An answer is returned only after its delivery line is
durable: when `deliver` fails, `execute` returns
`CallError::new(CallErrorCode::Config, "eval_call_delivery_unrecorded:
the call capture could not record this answer's delivery; the answer was
not handed out")` instead of the result, and the reservation stays
undelivered with its owner lock released when the call returns. Crash
points of one doubled call, in order:

1. Before the reservation: nothing written; a retry reserves afresh.
2. After the reservation, before `deliver` returned (in dispatch, before
   or after the journal's `connector_call`, before or after a send's
   settlement), or `deliver` failed: the owner lock is released by the
   process's end or the call's return. A retry of the same call that
   reaches the answer step replays the reservation (item 3) and gets the
   same answer with no `times` used; a second `connector_call` may be
   journaled for it. If nobody retries, the reservation stays
   undelivered: its answer slot is used, and the checks fail the
   repetition over it (D1 "The checks": an undelivered reservation is
   never read as "no call").
3. The same window for a tracked send whose ledger already settled
   (`send_intent_settled` journaled `confirmed`, `not_sent` or `unknown`
   before `deliver` failed or the process died): a repeat of that intent
   is answered by the send gate (`already_sent`, `unknown_effect`, or a
   resend under `on_unknown: resend`) before it reaches the answer step,
   so the reservation is never replayed. Recovery is the checks': the
   reservation stays undelivered and the repetition fails
   `connector_doubles` naming it, and every `connector_calls` check that
   selects its function is indeterminate. The repetition is reported as
   such; the person runs it again. No retry can mistake that send for
   one that did not happen, and none is counted twice.
4. After `deliver` returned, before the caller read the result (an
   agent's `apb connector call` killed while printing): the call counts
   as delivered; a retry is a new call. This window is the process's own
   output and is not closed.

When the capture and the journal disagree. The capture and the run
journal are separate files written in this order for one call: the
reservation (synced), the `connector_call` event and, for a tracked
send, `send_intent_settled` (both under the journal lock), then the
delivery (synced). A power loss can therefore leave either one ahead of
the other: a synced reservation with no `connector_call` in the journal
(the loss came before the journal append reached the disk), or a
`connector_call` and a settled send with no delivery line. Neither file
is repaired from the other, and each has one reader:

1. The capture wins for the checks. The checks read the capture only
   (`read_calls`), never the journal's `connector_call` records: a
   reservation with no delivery fails the repetition (`connector_doubles`)
   and makes its function's `connector_calls` checks indeterminate,
   whether or not the journal holds its `connector_call`; a delivered
   capture counts as a call even when its `connector_call` is missing.
2. The journal wins for the run. A resumed drive, the send ledger, the
   run outcome and every run surface read the journal only, never the
   capture: a send the journal settled is sent for the ledger whatever
   the capture holds, and a call the journal does not record is not a
   call of the run for any of them.

So a disagreement can make a repetition fail or a check indeterminate,
never pass a check over a call it did not see.

A doubled send goes through the send ledger first (D1 item 3 below): a
crash after `send_intent_recorded` leaves the intent `unknown`, and the
ledger decides the repeat (`unknown_effect` unless `on_unknown: resend`)
before a reservation is looked at; the replay applies only to what the
ledger lets through.

Scrubbing the capture. The args are scrubbed before they are written
with the run's storage redactor, `RunRedactor::for_storing(root,
run_dir)` and its `secret_json` (a constructor added to
`crates/apb-engine/src/run_redact.rs`, below); when it refuses
(`Err(SecretsUnresolved)`) the capture holds `{"withheld":
"secrets_unresolved"}` instead of the args and `args_kept: withheld`;
args longer than 16 KiB once scrubbed are kept as `{"clipped": true}`
with `args_kept: clipped`. The match of an answer and the `call_key`
read the call's own args in memory, never the capture, so clipping or
withholding never changes which answer serves a call.

The storage redactor. `RunRedactor::secret_text` and `secret_json`
(`run_redact.rs` lines 627 and 638), the API result snapshots store
through (`scheduler/result.rs` line 350, `scrub_unavailable`), refuse
only when the redactor withholds, and `WorkspaceRedaction::new` (line
428) counts a referenced secret it could not resolve (a command-sourced
field, which a redactor never runs; a variable missing here; a connector
that does not load; `resolve.rs` `resolve_referenced_checked`, line 397)
as a reason to withhold only in a read-only process (line 434). So in a
driving process a command-sourced secret is neither scrubbed nor
refused. This change adds no second scrubber: it adds one constructor,

```rust
/// [`RunRedactor::for_run`] for text this process stores: it withholds,
/// and so `secret_text` and `secret_json` refuse, whenever any secret the
/// workspace references is unresolved, in every process mode.
pub fn for_storing(root: &Path, run_dir: &Path) -> Self;
```

which builds `WorkspaceRedaction` with `withhold: referenced.unresolved >
0` (no process-mode condition) and the same `Scrubber`, tree secrets and
`storing_refused` as `for_run`. Every text this change stores goes
through it: a doubled call's captured args (the change `inbox-waits`
stores its wait bodies through it too). Result snapshots keep
`for_run_dir` as on main (not changed here; the same gap is listed in
the Review record as pre-existing). The consequence, stated in
`docs/EVALS.md`: a workspace whose installed connectors reference a
command-sourced secret, or a variable this process cannot see, stores no
captured args (args assertions are indeterminate).

What a doubled call reads. A doubled call resolves no secret for itself:
no account field, no variable for its request, no command helper
(`build_prepared` answers before `resolve_secrets`, item 3 below, and the
doubled snapshot holds no account record to resolve). The storage
redactor, built for the capture, reads the workspace's referenced
variables to scrub them, as every redactor of a run does, and never runs
a command helper; when it cannot account for every referenced secret
that way, the args are withheld (above) and every check that needs them
fails as indeterminate (D1 "The checks"). The two requirements, no secret
read for the call and no unscrubbed args stored, therefore never
conflict: the second gives way to withholding, never to a read.

`crates/apb-engine/src/connector/result.rs` `CallErrorCode` (line 17)
gains `DoubleUnmatched` (`as_str` `"double_unmatched"`), documented "an
eval run's call no double answers: nothing was sent". `effect::classify`
(`connector/effect.rs` line 36) already reads an outcome other than
`ok` with no HTTP status as `Failed`, so a recorded send intent settles
`not_sent`.

`crates/apb-engine/src/connector/call/mod.rs`:

1. `PreparedCall` (line 307) gains a variant
   `Double { account: String, reservation: eval::doubles::Reservation }`.
   `cap_timeout`, `account`, `pre_auth_url` (`"eval-double"`) and
   `event_extra` (`(None, None)`) get their arms; `dispatch` (line 386)
   maps the reservation's `Ok(spec)` through `map_status(spec.status, spec.body, None,
   None, false)` with `http_status: Some(spec.status)`, exactly as the
   `Mock` arm (lines 388 to 398), and `Err(e)` to `Dispatched { result:
   Err(e), http_status: None, .. }`. A double therefore goes through the
   same status table, journal record (`append_event`, line 1209) and send
   settlement (`settle_send`, line 186) as a mock. The reservation
   travels with the result to the end of `execute`, which calls
   `eval::doubles::deliver` right before it returns and returns its
   error instead of the result when it fails (D1 "Answering a call").
2. `prepare` (line 434), first statement: compute
   `eval::doubles::of_run` once (it is handed to `target::resolve`, D1
   "The doubled snapshot" item 3, and to `build_prepared`); when
   `ctx.origin.is_live()` (a notification, `request.rs` line 189) and it
   gives `Some` (or fails), return `Err(CallError::new(
   CallErrorCode::Permission, "an eval run never sends notifications"))`.
3. `build_prepared` (called at line 546, after the send-once gate at
   lines 510 to 518, so a doubled send is recorded and settled through
   the send ledger as a real one), the one place that resolves secrets
   (`resolve_secrets`, line 783): when `of_run` gives `Some(doubles)`,
   answer the call from the doubles right after the args were checked as
   a real call checks them, so a call a real run refuses `invalid_args`
   is refused the same way: for an ordinary function after
   `args_schema` validation (lines 735 to 737) and before the function's
   own `mock` (step 6, lines 739 to 745, which returns at once; a double
   placed after it would let a function `mock` answer a doubled call
   with no `times` used and no capture written), so before
   `resolve_secrets` too; for an inbox function after
   `connector::inbox::build` parsed its arguments (line 752), in place of
   the local `PreparedCall::Inbox` (a doubled run's inbox is the double;
   its answer is the JSON a real read or ack returns). Build
   `PreparedCall::Double { account: target.account.clone(), reservation:
   crate::eval::doubles::reserve(...)? }` with the site of the call
   (`req.node_id`, which already is the site id for checks and engine
   sites) and the typed args (`typed_rendered_args`, lines 521 to 531,
   for a check; the call's args otherwise). These are the call's input
   arguments: the text modes, the neutralizing of forged markers and the
   appended send marker (`connector/intent/stamp.rs`
   `prepare_body_args`, line 39) apply only to the body of a real
   request, which a doubled call never builds, so the capture and every
   `connector_calls` assertion over it read the arguments as the caller
   gave them. When `of_run` fails, the
   call fails with its error: nothing is sent. No secret is resolved for
   a doubled call (D1 "Answering a call", "What a doubled call reads").
4. A `--dry-run` call is unchanged: it stays `Prepared::DryRun`
   (`send_once.rs` line 116), journals nothing, reserves nothing and uses
   no `times`.

The `healthcheck` and `play_call` paths (`answer_live`, line 1009) have
no run directory and are untouched.

Which calls are captured: the dispatched calls only, the calls that
reach the answer step in `build_prepared` (matched or `double_unmatched`).
A call refused before that step leaves no capture line, exactly as it
sends nothing: a grant or process-mode refusal, `max_calls`, the run
budget, `invalid_args`, and every repeat the send gate answers itself
(`already_sent`, `unknown_effect`, `in_flight`, `attempt_closed`;
`send_once::gate`, `call/mod.rs` line 510, runs before
`build_prepared`). The journal keeps its `connector_call` record of such
a repeat as on main. So `checks.connector_calls` counts what the doubles
answered, and a repeated tracked send answered `already_sent` is one
captured call, not two (D1 "The checks" says so, and `docs/EVALS.md`).

### The checks

`crates/apb-engine/src/eval/checks.rs` `evaluate` (line 558) adds, after
the `events` checks:

The checks read the dispatched calls only (D1 "Answering a call",
"Which calls are captured"): a repeat the send gate answered
`already_sent` is not a captured call, so an assertion about a call
count counts the calls the doubles answered. The checks run after the
run ended, when no call of it is live. A delivered reservation is a
call. A reservation with no `delivered` line (its caller died before the
answer reached it, or its delivery could not be recorded, D1 "Answering
a call" crash points 2 and 3) is never read as "no call": it fails the
repetition (item 1) and makes every check that selects its function
indeterminate (item 2), because nobody can tell whether its caller acted
on the answer.

1. `connector_doubles` (always, for a case with doubles): failed when any
   delivered capture has `answer: None`, detail `<n> call(s) no double answered:
   <connector.function at site>, ...` (first five); failed when any
   reservation is undelivered, detail `<n> call(s) reserved and never
   delivered: <connector.function at site>, ...` (first five); failed when
   `read_calls` returns an error, detail `the call capture does not read
   (line <n>): <reason>`. A capture that does not read fails the
   repetition: this check fails, and so does every `connector_calls`
   check (detail `indeterminate: the call capture does not read`), never
   passed over the lines that did read.
2. One `connector_calls[<function>]` check per entry: select the
   delivered captures of that `connector.function`, narrowed by `site`;
   when an undelivered reservation of that function (and site, when set)
   exists, the check fails as indeterminate, detail `indeterminate: <n>
   call(s) of <function> reserved and never delivered`, whatever its
   rule; `args` and `contains` read the capture's input arguments
   (before text modes and markers, D1 "Answering a call"); when the
   entry sets `args` or `contains` and any selected capture has
   `args_kept` other than `whole`, the check fails as indeterminate,
   detail `indeterminate: <n> call(s) of <function> kept no whole args
   (clipped or withheld)`, whatever its rule (a `count: 0` over clipped
   text is never passed). Otherwise narrow by `args` (`args_hold`) and
   `contains` (`args_contain`); then `count` must equal the selection's
   size, `min` and `max` bound it, and with none of the three the
   selection must not be empty. Detail on failure: `<k> matching
   call(s), expected <rule>`. An entry with neither `args` nor
   `contains` counts every selected capture, whole or not.

`crates/apb-engine/src/eval/store.rs` `Repetition` (line 148) gains
`#[serde(default, skip_serializing_if = "Vec::is_empty")] pub
connector_calls: Vec<CallSummary>` where `CallSummary { function: String,
site: String, answered: bool, delivered: bool }` (no args: the result file is a summary;
the captures stay in the stored run directory, which moves with the run to
`<config-dir>/evals/runs/<playbook>/<run-id>/`, except for an
`--external-data` repetition with doubles, which is never kept: its capture and its directory are removed once the checks ran, D1 "Running a doubled case" item 5). `apb eval`'s text
report prints, per repetition with doubles, `calls: <n> (<u> unmatched)`,
with `, <r> reserved and never delivered` when there are such.

### Docs

`docs/EVALS.md`:

1. Lines 15 to 20: mock connectors are built; the list of what is not
   built yet loses them.
2. "Never with irreversible effects" (line 22) stays as it is for
   `irreversible`, and gains: a node that binds a connector is allowed
   when the case doubles every function of every binding site; a
   declared `irreversible` effect is refused whatever the doubles cover,
   because doubles do not reach what an agent can do with the network and
   the person's logged-in CLIs (D1 "What stays refused").
3. A new section "Connector doubles" after "Checks": the `connectors:`
   format, `match`, `times` and sequences, what a doubled run does (every
   call answered or `double_unmatched`, no secret resolved, no
   notification), `eval-calls.jsonl` (reservations and deliveries; a call whose
   process died before its answer reached it is replayed to its retry
   and never counted twice; an answer is handed out only after its
   delivery is recorded; a reservation never delivered fails the
   repetition; the reservation is synced before the call is dispatched;
   when a power loss leaves the capture and the journal apart, the
   capture is what the checks read and the journal what the run reads;
   an `--external-data` repetition with doubles is never kept: its capture and directory are removed once its checks ran),
   `checks.connector_calls` (dispatched calls only: a repeat the send
   ledger answers `already_sent` is not a captured call; `args` and
   `contains` read the input arguments of the call, before the
   connector's text modes and send markers, never the request body),
   that a
   doubled call needs this installation's key and is refused
   `eval_call_key_unavailable` without one, and the
   guidance: run the agent nodes that bind no connector with `isolation:
   { level: full, network: none }`, and cut the operator's credentials
   with the case `env`, because a node that binds a connector cannot be
   isolated (V113); that the accounts' variables must be present (any value, a placeholder in CI) and are never sent, read only to scrub the captured arguments; that a send function's reconcile function needs a double too, and for a `marker` send the connector's identity function as well, and that a recorded answer never holds a marker nonce, so a doubled marker reconcile finds no matching item and the send stays unknown; that `apb eval` puts its own binary first on the steps'
   `PATH`, followed by its own inherited `PATH`; that a doubled run's manifest records account names only, so
   an `apb` that does not know doubles fails every call of the run
   instead of reaching a service; that an args assertion over clipped
   (above 16 KiB) or withheld args fails as indeterminate, and that args
   are withheld whenever a connector installed in the workspace
   references a command-sourced secret or a variable the run cannot see
   (the storage redactor, D1 "Answering a call"); and the shape
   of a doubled inbox read (the JSON a real `read` returns). A worked
   case for `examples/playbooks/crm-cleanup.yaml` (Twenty connector,
   `list_opportunities` at the baseline and at the nodes `count_unowned`
   and `recount`, `update_opportunity` at `assign_owners`; no
   `irreversible` effect):

```yaml
schema: 1
id: assigns-three-owners
fixture: { dir: fixtures/empty }
params: { default_owner_id: u-default, open_stages: "NEW,SCREENING" }
connectors:
  twenty:
    list_opportunities:
      - match: { site: "@baseline" }
        answer: { status: 200, body: { data: { opportunities: [] }, totalCount: 3 } }
        times: 1
      - match: { site: "@baseline" }
        answer: { status: 200, body: { data: { opportunities: [] }, totalCount: 0 } }
      - match: { site: count_unowned }
        answer: { status: 200, body: { data: { opportunities: [ { id: o-1 }, { id: o-2 }, { id: o-3 } ] }, totalCount: 3, pageInfo: { hasNextPage: false } } }
      - match: { site: recount }
        answer: { status: 200, body: { data: { opportunities: [] }, totalCount: 0, pageInfo: { hasNextPage: false } } }
    update_opportunity:
      - answer: { status: 200, body: { data: { updateOpportunity: { id: o-1 } } } }
checks:
  connector_calls:
    - { function: twenty.update_opportunity, contains: u-default, count: 3 }
    - { function: twenty.list_opportunities, site: recount, min: 1 }
```

4. "Validation codes": V280, V281.

`docs/HOWTO-authoring.md` "Eval cases (evals/)" (line 5364): one
paragraph pointing at the doubles section.

### Wrong implementations

1. Falling through to the real connector when no answer matches. Caught
   by `an_unmatched_call_fails_closed_without_a_socket` (the connector's
   base URL points at a `common::ScriptedHttpServer` that records every
   request; after a doubled run with one unmatched call the server
   received nothing and the journal's `connector_call.outcome` is
   `double_unmatched`).
2. Resolving the account's secrets before answering (a `{{cmd:...}}`
   secret helper runs). Caught by `a_doubled_run_resolves_no_secret` (the
   account's secret is a `{{cmd:...}}` that writes a marker file; no
   marker after the run).
3. Counting `times` per process, so two `apb connector call` processes
   both get the `times: 1` answer. Caught by
   `times_is_counted_across_call_processes` (two calls through the CLI
   binary; the second gets the next answer).
4. Writing raw args into the capture. Caught by
   `captured_args_go_through_the_run_redactor` (a secret param value in
   the args is not in `eval-calls.jsonl`).
5. Lifting V81 for a case that doubles only some functions. Caught by
   `v280_names_an_uncovered_function` and the CLI test
   `a_partly_doubled_case_is_refused_before_anything_runs` (refused
   `invalid_suite` naming V280, nothing materialized).
6. Sending a notification from a doubled run. Caught by
   `a_doubled_run_never_notifies`.
7. Bypassing the send ledger for a doubled send, so a repeat sends twice.
   Caught by `a_repeated_doubled_send_is_answered_already_sent`.
8. Lifting the `irreversible` refusal for a fully doubled case. Caught
   by `full_doubles_keep_the_irreversible_refusal` (a fully doubled case
   of a playbook declaring `irreversible`, and one whose node declares
   it: both refused with main's reasons, V81 in `apb validate`).
9. Answering before the args are checked, so a call a real run refuses
   `invalid_args` gets a recorded answer. Caught by
   `a_doubled_call_with_invalid_args_is_refused_as_a_real_one`.
10. Doubling a dry run, so it journals a call and uses up a `times`
    answer. Caught by `a_dry_run_of_a_doubled_run_stays_a_dry_run`.
11. Treating a `run.yaml` that does not read as "no doubles". Caught by
    `an_unreadable_run_config_refuses_the_call` (the run's provenance says
    `eval_doubles`, `run.yaml` is truncated, the call fails `config` and
    the scripted server received nothing).
12. Leaving the `eval_doubles` guard out of a doubled run's start or
    dropping it in a patch. Caught by
    `a_doubled_run_carries_the_eval_doubles_guard` (prepare test: the
    guard names `eval_doubles`; `patch.rs` keeps it).
13. Leaving a send's reconcile function out of the coverage. Caught by
    `bound_functions_adds_reconcile_functions`.
14. Snapshotting a doubled run's accounts as a real run does, so an apb
    that does not know doubles (an older binary run by an absolute path,
    or one that resumes the run) calls the service. Caught by
    `an_apb_blind_to_doubles_cannot_call_from_a_doubled_snapshot` (the
    doubled run directory with its `run.yaml` doubles, its
    `eval_doubles` provenance and the manifest's `doubled_accounts` taken
    away, which is what such an apb sees, since it ignores all three:
    the call fails `config` naming the account, the scripted
    server received nothing, the `{{cmd:...}}` marker file is absent) and
    by the journal corpus row V1 (v0.25.0 and v1.0.0-beta.1 run `apb
    connector call` on a doubled run directory: refused, the server
    received nothing).
15. Answering a function's own `mock` before the double. Caught by
    `a_function_mock_does_not_bypass_the_double` (a fixture function
    with a `mock`: the doubled call gets the double's answer, uses its
    `times` and is captured).
16. Deciding an args assertion over clipped or withheld args. Caught by
    `a_clipped_capture_makes_an_args_check_indeterminate` (a `count: 0`
    with `contains` over a clipped capture fails as indeterminate).
17. Skipping a capture line that does not parse. Caught by
    `a_corrupt_capture_line_fails_the_repetition`.
18. Writing a doubled start's guard in its own append after the
    provenance, or pairing the provenance and guard of a start that is
    not doubled. Caught by `the_guard_is_written_with_the_provenance`
    (prepare test: for a doubled start the `run_guard` line follows
    `run_provenance` at the next seq with the same `ts`, from one write)
    and `an_undoubled_guarded_start_writes_as_main` (a start with a
    deadline, no doubles: three appends as on main).
19. Counting `times` at the capture with no reservation, so a call whose
    process died before its answer was handed out consumes the answer and
    is counted by the checks. Caught by
    `a_crash_between_reservation_and_delivery_replays_the_answer` (the
    barrier `Hook::DoubleBeforeDeliver` stops the call process after its
    reservation; the retried call gets the same answer, the next call the
    next one, and the checks count one call) and
    `an_identical_live_call_is_not_replayed` (two identical calls at
    once: each its own answer).
20. Storing captured args through a redactor that does not refuse a
    command-sourced secret in a driving process. Caught by
    `a_command_sourced_secret_withholds_captured_args` (a workspace
    connector whose account holds a `{{cmd:...}}` secret: the capture's
    args read `withheld`, the helper's marker file is absent, and an
    args check over the call fails as indeterminate).
21. Answering a doubled call without an identity when the installation
    has no usable key. Caught by `a_doubled_call_without_a_key_is_refused`
    (a config dir whose key file cannot be read: the call fails
    `eval_call_key_unavailable`, no capture line, no `times` used).
22. Returning an answer whose delivery was not recorded. Caught by
    `an_unrecorded_delivery_returns_no_answer` (the capture file is made
    unwritable after the reservation through the barrier
    `Hook::DoubleBeforeDeliver`: the call fails
    `eval_call_delivery_unrecorded`, a retry of the same call gets the
    same answer with no `times` used) and
    `an_undelivered_send_fails_the_repetition` (a tracked send settled
    `confirmed`, its delivery not recorded: the repeat is answered
    `already_sent`, the reservation stays undelivered, `connector_doubles`
    fails naming it and a `connector_calls` check of that function is
    indeterminate).
23. Counting a repeat the send ledger answered as a captured call.
    Caught by `a_repeated_doubled_send_is_answered_already_sent` (the
    second call is answered `already_sent`, the capture holds one call,
    and `{ function: <send>, count: 1 }` passes).
24. Keeping anything of an `--external-data` repetition with doubles. Caught by the CLI tests `a_passing_external_data_repetition_leaves_nothing`, `a_failing_external_data_repetition_leaves_nothing` and `an_external_data_repetition_with_a_live_driver_leaves_nothing` (no repetition directory, no kept tree and no `eval-calls.*` entry under the config directory afterwards), and `an_external_data_repetition_that_cannot_be_removed_fails_and_says_where`.
25. Dispatching a call before its reservation is durable, so a power
    loss leaves a dispatched call with no capture line and a `count: 0`
    passes. Caught by `a_crash_after_dispatch_leaves_a_reservation_the_checks_fail_on`, `the_capture_directory_is_synced_before_the_first_dispatch`
    (the barrier `Hook::DoubleBeforeDispatch`, added beside
    `DoubleBeforeDeliver`, stops the call process right after `reserve`
    returned, before dispatch, which is what a power loss after the sync
    leaves; the reserved line reads through a fresh handle, the journal
    holds no `connector_call` for it, and the checks fail
    `connector_doubles` naming the reservation and make `{ function:
    <f>, count: 0 }` indeterminate) and
    `the_capture_and_the_journal_are_read_by_their_own_readers` (a run
    directory whose capture holds a delivered call whose `connector_call`
    the journal lacks, and a reservation whose tracked send the journal
    settled `confirmed`: the checks count the delivered call and fail on
    the undelivered one; the send ledger answers a repeat of the send
    `already_sent`, and the run outcome lists no call the journal does
    not hold).
26. Asserting the request body instead of the input arguments, so an
    assertion passes or fails on text the connector's text modes, the
    neutralizing of forged markers or the appended send marker produced.
    Caught by `connector_calls_args_are_the_input_args` (a `marker`
    send function with a text mode: `contains` the input argument text
    matches, `contains` the marker text does not, and an input argument
    holding a forged marker is matched as the caller wrote it).
27. Building the step `PATH` from anything but this binary's directory
    and the inherited `PATH`. Caught by
    `a_doubled_case_puts_this_binary_first_on_the_step_path` (exact
    value).

### Tests

1. `crates/apb-core/src/eval_doubles.rs` tests: `args_hold_is_a_subset_match`,
   `args_contain_reads_string_leaves_only`, `pick_walks_answers_in_order_with_times`,
   `bound_functions_reads_all_read_only_and_lists`,
   `bound_functions_adds_reconcile_functions`,
   `coverage_gaps_names_missing_connectors_and_functions`.
2. `crates/apb-core/src/eval.rs` tests: `a_doubled_case_parses`,
   `answers_and_stop_before_stay_refused`, `v280_names_an_uncovered_function`,
   `v281_refuses_count_with_min`, `a_case_without_doubles_keeps_every_reason`,
   `full_doubles_keep_the_irreversible_refusal`.
3. `crates/apb-core/src/validate/guarded.rs` tests: `ALL` has 14 entries
   in order with `eval_doubles` last; `crates/apb-engine/src/scheduler/prepare.rs`
   `mod run_guard_tests`: `a_doubled_run_carries_the_eval_doubles_guard`,
   `the_guard_is_written_with_the_provenance`,
   `an_undoubled_guarded_start_writes_as_main`;
   `crates/apb-engine/src/connector/run.rs` test
   `a_doubled_snapshot_records_no_account` (`accounts` empty,
   `doubled_accounts` with names and default flags, no variable name and
   no command anywhere in the connector's manifest entry).
4. `crates/apb-engine/tests/suite/eval_doubles_test.rs`, built like
   `crates/apb-engine/tests/suite/connector_e2e.rs` (the `mock-tracker`
   fixture installed into a temp config dir, a snapshot into a run
   directory, then `apb_engine::connector::call::execute`), with the run's
   `run.yaml` carrying `eval: { doubles }`:
   `a_doubled_call_is_answered_and_captured`,
   `an_unmatched_call_fails_closed_without_a_socket`,
   `a_doubled_run_resolves_no_secret` (the helper never runs, and the
   capture's args read `withheld` because the redactor cannot account
   for the command-sourced secret),
   `captured_args_go_through_the_run_redactor`,
   `a_command_sourced_secret_withholds_captured_args`,
   `a_crash_between_reservation_and_delivery_replays_the_answer`,
   `an_identical_live_call_is_not_replayed`,
   `a_doubled_call_without_a_key_is_refused`,
   `an_unrecorded_delivery_returns_no_answer`,
   `an_undelivered_send_fails_the_repetition`,
   `a_crash_after_dispatch_leaves_a_reservation_the_checks_fail_on`, `the_capture_directory_is_synced_before_the_first_dispatch`,
   `the_capture_and_the_journal_are_read_by_their_own_readers`,
   `connector_calls_args_are_the_input_args` (through `execute`, then
   the checks over the capture),
   `a_repeated_doubled_send_is_answered_already_sent`,
   `a_double_answering_500_on_a_send_leaves_it_unknown`,
   `a_doubled_call_with_invalid_args_is_refused_as_a_real_one`,
   `a_dry_run_of_a_doubled_run_stays_a_dry_run`,
   `an_unreadable_run_config_refuses_the_call`,
   `a_doubled_inbox_read_returns_the_recorded_envelope`,
   `an_apb_blind_to_doubles_cannot_call_from_a_doubled_snapshot`,
   `a_function_mock_does_not_bypass_the_double` (a fixture function with
   a `mock` block added to the `mock-tracker` copy the test installs).
5. `crates/apb-engine/src/eval/checks.rs` tests:
   `connector_calls_count_min_max_args_contains`,
   `an_unmatched_capture_fails_connector_doubles`,
   `a_clipped_capture_makes_an_args_check_indeterminate`,
   `a_corrupt_capture_line_fails_the_repetition`,
   `an_undelivered_reservation_fails_the_repetition` (an undelivered
   reservation fails `connector_doubles` and makes a `connector_calls`
   check of its function indeterminate, never "no call");
   `eval/doubles.rs` tests
   `read_calls_reports_a_torn_line`,
   `a_replay_needs_a_free_owner_lock`; `run_redact.rs` test
   `for_storing_refuses_an_unresolved_secret_in_a_driving_process` (a
   command-sourced reference, read-only mode off: `secret_json` refuses,
   `for_run`'s does not).
6. `crates/apb-cli/tests/suite/eval_doubles_cli_test.rs` (read how
   `crates/apb-cli/tests/suite/eval_cli_test.rs` binds a stub executor;
   here the stub agent's script runs `apb connector call`):
   `a_doubled_case_runs_and_passes` (one `agent_task` that binds the
   `mock-tracker` fixture, through `apb eval --yes --json`),
   `times_is_counted_across_call_processes` (two `apb connector call`
   processes of one run; the second gets the next answer),
   `a_partly_doubled_case_is_refused_before_anything_runs`,
   `the_dry_run_lists_doubled_functions`,
   `a_doubled_case_puts_this_binary_first_on_the_step_path` (the stub
   step prints `command -v apb` and `$PATH`: the first is this binary's
   path, and the second equals exactly `<this binary's directory>:<the
   PATH the test gave apb eval>`, nothing added or dropped),
   `a_passing_external_data_repetition_leaves_nothing`, `a_failing_external_data_repetition_leaves_nothing`, `an_external_data_repetition_with_a_live_driver_leaves_nothing`, `an_external_data_repetition_that_cannot_be_removed_fails_and_says_where`, `a_doubled_repetition_without_external_data_keeps_its_tree_and_capture`. The CLI test
   reaches the `mock-tracker` fixture by a relative path, as
   `problems_cli_test.rs` line 12 does, and exports placeholder values
   for the fixture account's variables.
7. `crates/apb-engine/src/connector/call/tests.rs` (it reaches the
   crate-private `CallOrigin`): `a_doubled_run_never_notifies` (a
   notification call against a run directory with doubles is refused and
   nothing is captured), `snapshot_and_settings_that_disagree_fail_closed`
   (a doubled manifest without doubles in `run.yaml`, and doubles with
   a manifest that holds accounts: both `config`, nothing sent).
8. `crates/apb-cli/tests/suite/journal_corpus_test.rs` row V1 (above).


## Code map

One row per changed file. Read on efae5822; every row that is not new
names a file that exists.

| # | File | Decision | Change |
|---|---|---|---|
| 1 | `crates/apb-core/src/lib.rs` | D1 | `pub mod eval_doubles;` |
| 2 | `crates/apb-core/src/eval_doubles.rs` (new) | D1 | types, matching, coverage with reconcile functions |
| 3 | `crates/apb-core/src/eval.rs` | D1 | case fields, `LATER_*` and their message, `refusal` without its connector leg, `case_refusal`, V280, V281, V81 per case |
| 4 | `crates/apb-core/src/validate/guarded.rs` | D1 | `GuardedFeature::EvalDoubles` (ALL 14) |
| 5 | `crates/apb-core/src/connector/resolve.rs` | D1 | `ConnectorFacts.reconcile_functions` (coverage) |
| 6 | `crates/apb-core/src/decision_thresholds.rs` | D5 | `COMPLETION_RULE`, `versioned_use` |
| 7 | `crates/apb-engine/src/eval/mod.rs` | D1 | `pub mod doubles;` |
| 8 | `crates/apb-engine/src/eval/doubles.rs` (new) | D1 | `of_run`, `reserve` with the call key (refused without a key), the owner lock, the replay and the reservation synced before dispatch, `deliver` durable before an answer, `read_calls` as a `Result`, `args_kept`, `CAPTURE_ENTRIES` |
| 9 | `crates/apb-engine/src/eval/checks.rs` | D1 | `validate_suite` facts, `connector_doubles` and `connector_calls[...]` over delivered captures, an undelivered reservation fails or makes indeterminate, indeterminate over kept-incomplete args, a capture that does not read |
| 10 | `crates/apb-engine/src/eval/store.rs` | D1 | `CallSummary` (with `delivered`) on `Repetition` |
| 11 | `crates/apb-engine/src/run_config.rs` | D1 | `EvalRunSettings.doubles`, `RunConfig::doubles` |
| 12 | `crates/apb-engine/src/run_redact.rs` | D1 | `RunRedactor::for_storing` |
| 13 | `crates/apb-engine/src/connector/result.rs` | D1 | `CallErrorCode::DoubleUnmatched` |
| 14 | `crates/apb-engine/src/connector/call/mod.rs` | D1 | `PreparedCall::Double`, `deliver` before the result leaves `execute`, `of_run` once in `prepare`, the notification refusal, the double in `build_prepared` before the function `mock` |
| 15 | `crates/apb-engine/src/connector/call/tests.rs` | D1 | `a_doubled_run_never_notifies`, `snapshot_and_settings_that_disagree_fail_closed` |
| 16 | `crates/apb-engine/src/connector/call/target.rs` | D1 | `snapshot` of a doubled run over `doubled_accounts`; snapshot and settings that disagree fail closed |
| 17 | `crates/apb-engine/src/connector/run.rs` | D1 | `snapshot_connectors(.., doubled)`: no account recorded for a doubled run |
| 18 | `crates/apb-engine/src/manifest.rs` | D1 | `ManifestConnector.doubled_accounts`, `DoubledAccount` |
| 19 | `crates/apb-engine/src/scheduler/prepare.rs` | D1 | `run_provenance.eval_doubles`, `run_guard` names `eval_doubles`, the paired write for a doubled start only, `doubled` to the connector snapshot |
| 20 | `crates/apb-engine/src/event.rs` | D1 | `RunProvenance.eval_doubles` |
| 21 | `crates/apb-engine/src/deadline_hooks.rs` | D1 | `DoubleBeforeDispatch`, `DoubleBeforeDeliver` |
| 22 | `crates/apb-engine/src/decision/report/labels.rs` | D5 | rule `reply/1` over a gate that approves the reply, `chain_decision` over scope re-asks ending at a patch, `label_in` with the stored definition of the final request |
| 23 | `crates/apb-engine/src/decision/report/mod.rs` | D5 | `RunJournal.run_dir`, the labelling loop calls `label_in` |
| 24 | `crates/apb-engine/src/decision/report/replay.rs` | D5 | the replay calls `label_in` |
| 25 | `crates/apb-engine/tests/main.rs` | D1, D5 | two test modules |
| 26 | `crates/apb-engine/tests/suite/eval_doubles_test.rs` (new) | D1 | doubled calls |
| 27 | `crates/apb-engine/tests/suite/completion_labels_test.rs` (new) | D5 | the report on a fixture run |
| 28 | `crates/apb-cli/src/eval.rs` | D1 | case refusal, doubles and `PATH` (this binary, then the inherited one) to the run, captures to the checks, an `--external-data` repetition with doubles removed whole, never kept, dry run, report line |
| 29 | `crates/apb-cli/src/decisions.rs` | D5 | the `--rule` help names `completion_check: reply/1` |
| 30 | `crates/apb-cli/tests/main.rs` | D1 | one test module |
| 31 | `crates/apb-cli/tests/suite/eval_doubles_cli_test.rs` (new) | D1 | `apb eval` with doubles |
| 32 | `crates/apb-cli/tests/suite/journal_corpus_test.rs` | D1 | row V1: an older `apb connector call` on a doubled run directory sends nothing |
| 33 | `docs/EVALS.md` | D1 | doubles section, what stays refused, V280 and V281 |
| 34 | `docs/HOWTO-authoring.md` | D1 | the eval paragraph |
| 35 | `docs/DECISIONS.md` | D5 | the `completion_check` label row, rule versions |
| 36 | `docs/GUARDRAILS.md` | D1 | the guard name `eval_doubles` |
| 37 | the change's proposal | records | this change |
| 38 | the change's design | records | this change |
| 39 | the change's task list | records | this change |
| 40 | the spec delta of the capability `eval-connector-doubles` | records | spec delta |
| 41 | the spec delta of the capability `honest-decision-labels` | records | spec delta |
| 42 | `docs/superpowers/specs/2026-11-20-eval-doubles-design.md` (new) | records | public record (this file) |
| 43 | `CLAUDE.md` | records | module map |
| 44 | `AGENTS.md` | records | module map |

Count: 44 changed files (6 in `apb-core`, 18 in `apb-engine` sources,
3 engine test files, 5 in `apb-cli`, 4 docs, 8 records), well inside
the 70 to 90 of one pull request. The lift of the earlier draft is gone
(`validate/decisions.rs` is not touched), and the inbox wait is the
change `inbox-waits`.

Files not touched, checked: `crates/apb-core/src/validate/decisions.rs` (`node_shipping_reason` keeps refusing a declared `irreversible`, D1 "What stays refused"; only its comment on the connector leg is corrected), `crates/apb-engine/src/connector/prompt.rs` (a granted account with no record is listed by name already, D1 "The doubled snapshot" item 4; its tests gain the `doubled_accounts` field only), `crates/apb-engine/src/scheduler/result.rs`
(result snapshots keep `for_run_dir`), `crates/apb-core/src/run_origin.rs`
(the call key uses the existing `tag`), `crates/apb-core/src/fsutil.rs`
(`try_hold_lock` and `lock_dir` as they are),
`crates/apb-engine/src/scheduler/patch.rs` (it recomputes the guard through `run_guard`; its tests gain the `eval_doubles` field only), `crates/apb-cli/src/run.rs` (the suite caller
already goes through the engine wrapper), `web/` (no surface changes).

## Release notes

1. `apb eval` evaluates playbooks that act through connectors: an eval
   case's `connectors:` answers every call from recorded answers, never
   the service, and `checks.connector_calls` asserts the input arguments
   of the dispatched calls, before the connector's text modes and send
   markers (a repeat the send ledger answers `already_sent` is not
   counted again). The accounts' variables must still be present; no value is sent, and one is read only to scrub the captured arguments.
2. A case of a connector playbook without full doubles is now refused
   `invalid_suite` (V81 or V280) where 0.24 refused it
   `eval_refused_effects`; both before anything runs. A playbook that declares `irreversible` effects, or a node that declares them or is named like a shipping step, stays refused, doubles or not; a connector function's own `irreversible` flag is not consulted, because every doubled call is answered locally.
3. `completion_check` labels in `apb decisions report` read only a person's approval of the reply, recorded through a person's channel (`terminal` or `dashboard_session`); nothing is labelled "act" any more, so
   the report suggests no `completion_check` threshold, and earlier stored
   `completion_check` thresholds no longer enforce.
4. Run guards: a doubled eval run carries `eval_doubles`; an older apb
   that knows run guards refuses to resume or pause a run whose journal
   holds it.
5. A doubled eval run records account names only in its manifest, so an
   older `apb` that runs a call of such a run fails it instead of
   reaching the service.
6. Captured eval call arguments are stored only when every secret the
   workspace's connectors reference can be scrubbed; otherwise they are
   withheld and an assertion over them is indeterminate.

## Implementation notes

1. Never run a real agent or reach a real service in a test: the stub
   agents and `common::ScriptedHttpServer` of the existing suites are the
   only executors and servers.
2. The release clippy gate (task 6.10) passes when `cargo clippy
   --release` prints no error, and no warning in a file this change
   touches that `cargo clippy --release` on main efae5822 does not also
   print.

3. `label_in` takes the journal's events and its run directory, not the
   `RunJournal`: `labels.rs` importing the report module is a cycle
   code-ranker refuses. Behaviour is the same.
4. The journal corpus row of this change is V1: E1 is the result
   snapshots' row.
5. The kept-path recipe of D1 "Tests" (a regular file at `<config>/evals/runs/<playbook>`) now tests a doubled repetition without `--external-data`, which is kept with its capture; an external-data repetition with doubles is never kept (Cut).
6. The capture's first append syncs the run directory; the test makes
   that sync fail through the hook `DoubleBeforeCaptureDirSync` of the
   existing `deadline_hooks` testing seam and asserts the call is
   refused before it is dispatched.
7. `Repetition.doubled` (`serde(default)`, left out when false) lets the
   text report print `calls: 0 (0 unmatched)` for a doubled repetition
   whose run made no call.
8. `PreparedCall::dispatch` hands a doubled call's reservation back to
   its caller, so one place answers and delivers a double.

## Cut

1. The process stop of an `--external-data` repetition (fix rounds 2 to 5:
   a group kill, then a pidfd scan of `/proc`). Proving that every
   process of a repetition stopped needs a containment the engine does
   not have: a helper can clear its environment, leave the driver's group,
   change its directory, make itself not dumpable or outlive a driver
   that exited before `apb eval` read its pid, and no platform but Linux
   has `/proc`. The need for the proof is removed instead: an
   `--external-data` repetition with doubles is never kept, so nothing has
   to be stopped before its capture goes. A process that still runs holds
   handles only into an unlinked directory: what it writes through an open
   descriptor goes to an unlinked file, and what it writes afterwards by path lands outside any stored or kept run and goes with the scratch directory, so nothing it writes is stored or read by apb.
   `crates/apb-cli/src/eval/stop.rs` and `liveness::is_detached_driver_of`
   are deleted. Main's `kill_leftover_group` and its callers are untouched.

## Review record

Four design review rounds: rounds 1 to 3 on the combined draft this change was
split from, round 4 on this change alone (its history line is in the proposal); each finding was
checked against the code on efae5822 before it was applied. The rows
that concern this change are carried over with their original numbers
(R1 to R3; round 3 numbered in the review's own order: 1 and 2 the blockers, 3 to 11 the majors, 12 to 15 the minors), one line per finding, with where it landed; the inbox wait
rows went to `inbox-waits`.

Round 1:

1. R1.1 Blocker, guard after a start crash: confirmed (three separate appends, `prepare.rs` lines 1594, 1610 and 1644, for every guarded feature, so pre-existing on main); the guard of a doubled start is written with `run_provenance` in one `append_pair` (D1 "The run guard of a doubled run" item 3, narrowed to doubled starts in round 3, R3.11); the `run_started` to provenance window stays pre-existing for every guard.
2. R1.4 Blocker, no-service guarantee bypass: confirmed (`call/mod.rs` checks the process mode, then `target.rs` resolves the snapshot; no guard is read); the doubled snapshot, a manifest that records no account (`accounts: []`, names in `doubled_accounts`), because the granted-account lookup every apb with connectors runs fails `config` before the definition, any secret or any request (D1 "The doubled snapshot"; corpus row V1).
3. R1.8 Major, built-in mock before the double: confirmed (`build_prepared` returns for a `mock` right after the args check); the double comes before the function `mock` (D1 "Answering a call" item 3; `a_function_mock_does_not_bypass_the_double`).
4. R1.9 Major, assertions over incomplete captures: confirmed; captures carry `args_kept`, an args or contains assertion over a capture not kept whole fails as indeterminate, `read_calls` returns a `Result` and a line that does not parse fails the repetition and refuses further answers (D1 "Answering a call" and "The checks").
5. R1.12 Minor, a lift task's test name: superseded; the lift is not built (R3.2).

Round 2:

6. R2.15 Blocker, storage scrubbing that does not fail closed: confirmed (`WorkspaceRedaction::new` withholds for an unresolved reference only when `read_only::active()`, `run_redact.rs` line 434); one constructor, `RunRedactor::for_storing`, withholds whenever a referenced secret is unresolved, in every process mode; captured args go through it (D1 "Answering a call"; `for_storing_refuses_an_unresolved_secret_in_a_driving_process`, `a_command_sourced_secret_withholds_captured_args`). Result snapshots are not changed here.
7. R2.16 Blocker, doubled calls and secrets: confirmed (`for_run` reads referenced variables); a doubled call resolves no secret for itself; the capture's storage redactor never runs a command helper and withholds the args when it cannot account for every reference (D1 "What a doubled call reads").
8. R2.17 Blocker, the older-binary guard at start: confirmed (`append_pair` may leave its first line alone; `run_started` precedes the pair); recorded as the standing contract of every guard on main; what keeps a doubled run from a service through a connector is the account-free snapshot alone (D1 "The doubled snapshot", "The run guard of a doubled run").
9. R2.22 Major, the literal `approve`: confirmed (`Approves::approving_options`, `schema.rs` line 1698; a non-approving decision on a bound gate carries the nonce too); keep only for a gate whose `approves.results` names the node, whose request binds this execution's snapshot, with one of its approving options read from the stored definition through `label_in` (D5 "The labeller").
10. R2.23 Major, a doubled answer consumed by a crash: confirmed; a reservation and a delivery are two capture lines, an undelivered reservation whose owner lock is free is replayed to the same call (D1 "Answering a call"; `a_crash_between_reservation_and_delivery_replays_the_answer`, `an_identical_live_call_is_not_replayed`).
11. Sweep after round 2: crash points of a doubled call listed (D1 "Answering a call"); the doubled call's replay decision is made under `CALLS_LOCK` with the owner lock probed there; claims narrowed to calls through the run's snapshot and to what the journal order proves about notifications (D1).

Round 3:

12. R3.2 Blocker, full doubles do not make declared irreversible work safe: confirmed (`effects.rs` line 121: a declaration names an effect the engine cannot derive; an agent keeps the network and logged-in CLIs). Decided: the lift is removed. A declared `irreversible` stays refused for eval runs exactly as on main; V282, `liftable`, `node_shipping_reason_beyond_doubles`, the lift's tasks, tests and docs text are deleted; the worked case moved to `crm-cleanup` (D1 "What stays refused", "Docs").
13. R3.4 Major, a scope re-ask hides an approval: confirmed (`withdraw_if_rescoped`, `review_gate.rs` line 297; `gate_visit_decision` returns at the withdrawal, `labels.rs` line 396); the labeller follows re-asks that keep the binding of this reply's settlement to the final person decision (D5 "The labeller", `chain_decision`; three tests).
14. R3.7 Major, a failed delivery still hands out an answer: confirmed (`deliver` was best effort); the delivery line is written and synced before `execute` returns the answer, a failure returns `eval_call_delivery_unrecorded` instead, and the recovery for a send whose ledger already settled is stated: no replay (the ledger answers the repeat), the repetition fails `connector_doubles` and its function's checks are indeterminate (D1 "Answering a call" crash points 2 and 3, "The checks").
15. R3.8 Major, `connector_calls` and a repeat answered `already_sent`: confirmed (`send_once::gate` answers before the reservation, `call/mod.rs` line 510); the requirement and the checks cover dispatched calls only, said in the spec, the design and `docs/EVALS.md` (D1 "Which calls are captured", "The checks").
16. R3.11 Major, runs using no new feature change their journal writes: confirmed (`prepare.rs` lines 1610 and 1644 are separate appends); the paired write is used for doubled starts only, every other start writes main's journal, and the doubled start lists its crash points (D0 "What a run that uses no new feature writes", D1 "The run guard of a doubled run" item 3; `an_undoubled_guarded_start_writes_as_main`).
17. R3.13 Minor, the absent-key call identity: confirmed (`run_origin::tag` creates a key or errors, never empty, `run_origin.rs` line 126); a doubled call without a usable key is refused `eval_call_key_unavailable`, fail closed, before any line is written (D1 "Answering a call" `reserve` step 2; `a_doubled_call_without_a_key_is_refused`).
18. R3.15 Minor, the release clippy criterion: the gate passes with no error and no warning in a file this change touches that main does not also print (task 6.10, Implementation notes 2).

Round 4 (one review of this change alone, after the split; each
finding checked against the code on efae5822):

19. D1.M1 Major, an `--external-data` eval keeps an argument capture:
    confirmed (`--external-data` is passed to the run, `apb-cli`
    `eval.rs` line 607; only the result file is suppressed, line 2083;
    the clean path moves the run directory to the store, line 1454, and
    the keep path keeps the tree); the capture is removed after the
    checks ran and before the repetition is stored or kept, on both
    paths, and a capture that cannot be removed discards the repetition
    (D1 "Running a doubled case" item 5, `remove_capture`;
    `an_external_data_repetition_stores_no_capture`,
    `a_kept_external_data_repetition_keeps_no_capture`).
20. D1.M2 Major, a power loss can erase a reservation the checks rely
    on: confirmed (only `deliver` synced). The reservation is synced
    before `reserve` returns and so before dispatch; a reservation
    without a delivery fails the repetition (as decided in R3.7); when
    the capture and the connector journal or the send settlement
    disagree, the capture wins for the checks and the journal for the
    run, neither repaired from the other (D1 "Answering a call", "When
    the capture and the journal disagree";
    `a_crash_after_dispatch_leaves_a_reservation_the_checks_fail_on`, `the_capture_directory_is_synced_before_the_first_dispatch`,
    `the_capture_and_the_journal_are_read_by_their_own_readers`).
21. D1.M3 Major, a scope re-ask labelled with the wrong gate
    definition: confirmed (`scheduler/patch.rs` line 264 stores a new
    definition and journals `patch_applied.definition`, a boundary of
    `run_definitions::boundaries`, line 111, while the gate waits). The
    chain ends at a `patch_applied` after its first request (unlabelled
    "the gate's definition changed while it waited"), and the definition
    read is the final request's segment (D5 "The labeller";
    `a_reask_after_a_patch_leaves_it_unlabelled`,
    `the_final_requests_definition_is_read`).
22. D1.M4 Major, `connector_calls.args` named sent arguments but
    captures input arguments: confirmed (`connector/intent/stamp.rs`
    `prepare_body_args`, line 39, applies text modes, neutralizes forged
    markers and appends the marker for a real request's body only). The
    assertion is defined as the call's input arguments, before text
    modes and markers, in the spec, the design (`CallCheck` field docs,
    `CapturedCall.args`, "Answering a call", "The checks", release notes)
    and the docs (`docs/EVALS.md`) (`connector_calls_args_are_the_input_args`).
23. D1.m1 Minor, the `PATH` fallback relied on a case `PATH`: confirmed
    (`apb-core` `eval.rs` line 499 refuses `PATH` in a case or suite
    env). The step `PATH` is this binary's directory followed by the
    inherited `PATH` only; the CLI test `a_doubled_case_puts_this_binary_first_on_the_step_path` asserts the exact value.

Pre-existing on main, not changed here:

24. Result snapshots store through `for_run_dir`, which does not refuse a command-sourced secret in a driving process (R2.15; R3 pre-existing, `run_redact.rs` line 428).
25. The window between `run_started` and the start's provenance and guard, for every guarded feature (R2.17; R3 pre-existing, `prepare.rs` line 1594).
26. Eval already moves a completed `--external-data` run directory, its journal included, to the eval store (`apb-cli` `eval.rs` line 1454); this change removes only the capture it adds (round 4).

Round 5 (the implementation review, fix round 1; each finding checked
against the branch):

27. Identity function of a marker reconcile: confirmed
    (`send_once::reconcile_marker` reads `identity.function` first through
    the call path, which in a doubled run reached the answer step
    unmatched, while coverage neither asked for it nor accepted its
    double). `ConnectorFacts.identity_function` (set for a connector with a
    `marker` send) joins `bound_functions` beside the reconcile function,
    so V280 asks for its double and accepts it (D1 `bound_functions`;
    `bound_functions_adds_reconcile_functions`,
    `a_doubled_marker_reconcile_reads_the_doubled_identity_and_listing`).
28. Capture durability: confirmed (only the file was synced). The append
    that creates the capture syncs the run directory before the first
    dispatch (`the_capture_directory_is_synced_before_the_first_dispatch`).
29. A kept external-data repetition with a live driver: confirmed. The
    repetition's processes are stopped before the first capture removal,
    and a capture removal that fails puts the run directory's mode back
    once before the repetition is discarded
    (`a_kept_repetition_with_a_live_driver_is_stopped_before_its_capture_goes`,
    `an_external_data_repetition_whose_capture_stays_is_discarded`).
30. The capture reader followed links and could block on a FIFO:
    confirmed; it reads through `fsutil::beneath` (no link, regular file
    with one link, 64 MiB cap), and a list item id passes the storage
    redactor like the args (`the_reader_opens_only_a_plain_capped_file`,
    `a_captured_item_id_goes_through_the_run_redactor`).
31. Labels credited any decision without `decided_by` to a person:
    confirmed; a keep needs `via` of a person's channel (`terminal` or
    `dashboard_session`), and any patch while the gate waits leaves the
    reply unlabelled (D5 "The labeller"; `only_a_persons_channel_labels_keep`,
    `a_patch_while_the_gate_waits_leaves_it_unlabelled`).
32. Report coverage: confirmed. A real run approved at a bound gate
    through the terminal labels keep through the report and the replay
    (`the_report_labels_a_real_run_and_replay_leaves_its_journal_untouched`
    fails when the run directory is not handed to `label_in`), and the
    hand-computed figures moved to a retry advice fixture, which labels
    both ways.
33. The engine tests now run the checks over the real capture and read
    the run outcome (D1 "Tests" item 4).
34. A production run whose `run.yaml` does not read refused a notification
    as an eval run: confirmed; it is refused with the settings' own reason.
35. Minors: the refusal printing is one helper; a `PATH` that cannot be
    built refuses the repetition; a doubled repetition always prints its
    `calls:` line; the deadlock-prone test drops its sender on every exit;
    the misnamed power-loss test is renamed; the CLI keep test has no
    timing loop without a bound; a legacy `completion_check` threshold's
    list line is tested; the dead `Double` arm of `dispatch` is gone; the
    owner lock field is `_owner`.

Round 6 (the recheck of fix round 1):

36. R1 The stop was Linux only (`kill_leftover_group` reads `/proc`): confirmed. `stop_repetition` signals each live driver's process group (the pid read at start and the one `driver.pid` names, each only while `liveness::driver_pid_is_live` holds) with `killpg`-style `kill(-pgid)`, reaps its own children, and waits until `kill(.., 0)` fails, at most 5 s, on every unix; a group not shown gone discards the repetition (`discard_unstopped`). The live-driver test passes with the `/proc` scan disabled.
37. R2 A replayed reservation was dispatched with no directory sync: confirmed. The directory sync runs before every dispatch, new reservation or replay, once per process (`sync_capture_dir`); the test asserts the second sync, one reservation (seq 0) and its delivery.
38. R3 The mode reset went through a path: confirmed; `reset_private_mode` opens the directory `O_DIRECTORY | O_NOFOLLOW` and changes the mode with `fchmod` on the descriptor.
39. R4 The live-driver test polls each pid, bounded, until it is gone or a zombie.

Round 7 (an independent code review of the branch):

40. A missing or short capture read as no call: confirmed (`read_calls` reads no file as no call, `eval/doubles.rs`; the checks then passed `count: 0`). Every call a doubled run dispatches is journaled `connector_call` (`connector/call/mod.rs` `append_event`, called in `execute_bounded` before `deliver_double`), so no new journal entry is needed and an older binary sees nothing new. The checks compare the journaled calls with the capture per function: `capture_missing` or `capture_incomplete` fails every connector check (`checks.rs` `capture_gap`; `a_deleted_capture_fails_every_connector_check`, `a_truncated_capture_fails_every_connector_check`).
41. The stop depended on driver liveness and found nothing on macOS: confirmed. The driver leads its own process group (`driver::spawn_detached_driver`, `process_group(0)`); `apb eval` records that group at start, kept only when the pid's argv names this run's `__drive-run` (`liveness::is_detached_driver_of`), and the stop signals it and waits until `kill(-pgid, 0)` fails with ESRCH, bounded, whether or not the driver lives. On Linux the `/proc` scan is repeated until none of the run's processes is left; without `/proc` nothing proves a helper that left the group stopped, so the repetition is discarded with the reason (`stop_proven`, unit-tested with the platform as an argument; `a_helper_left_in_the_drivers_group_is_stopped_before_the_capture_goes`, which checks the helper joined the group and passes with the `/proc` scan disabled).
42. An item's completion decision was credited with the list's outcome: confirmed (`DecisionRecord` dropped `decision_made.item`). The record keeps the item and the labeller leaves such a decision unlabelled; report and replay read the same record (`an_item_decision_is_never_credited_with_the_lists_approval`).

Round 8 (the second independent code review of the branch):

43. A group signalled by a number another group may own: confirmed (`apb eval` recorded the driver pid as the group id, while the driver is spawned by the `apb run --detach` it runs, `start_run` with `Command::output`, in `apb_engine::driver::spawn_detached_driver`, and reaped there by `reap_in_background` or by init: the number can be reused). `apb eval` holds no driver handle, so no group is signalled by number (`stop_rule`, unit-tested with the handle and the platform as arguments). On Linux the `/proc` scan stops each process of the run by its own pid until none is left; elsewhere the repetition is discarded. Taking the driver's handle into `apb eval` would mean spawning the driver itself instead of `apb run --detach`, which this change does not do.
44. Both records of a call could disappear: confirmed (`append_event` dropped a journal error; a missing capture read as no call). The capture is created empty and synced with its directory when a doubled run is prepared (`prepare.rs`, after `journal_provenance`; `create_capture`); a missing file is `capture_missing` whatever the journal says, and a doubled call refuses without it; a doubled call the journal cannot record is refused `eval_call_unjournaled` before delivery, while a real call keeps main's best-effort journaling (`journal_call`). No new event or field: an older binary sees an empty `eval-calls.jsonl` it never reads. Tests: `a_removed_capture_fails_the_checks_with_an_empty_journal`, `a_doubled_call_the_journal_cannot_record_is_refused`, `a_doubled_case_without_a_call_passes_count_zero`.

Round 9 (the third independent code review of the branch, all in the stop):

45. The stop signalled by pid and group number (`kill_pid`, `kill_group` through `kill_leftover_group`), and read an unreadable `/proc` as empty: confirmed. The stop is rewritten from one invariant, stated at the top of `crates/apb-cli/src/eval/stop.rs`: a signal goes only through a handle that pins the process (a pidfd opened before the process is inspected), never by pid or group number, and anything not proven stopped is not kept. Each candidate is pinned (`pidfd_open`), read through `/proc`, confirmed still the pinned process (`pidfd_send_signal(fd, 0)` after the read), matched by the run's marks, then signalled and waited for (`pidfd_send_signal`, `poll`). The process list, a pin, or a read of one of this user's processes failing makes the stop unproven, and a process of this user that cannot be read counts as the run's unless it started before the repetition (`stat` start time). The verdict is injected through `ProcView` and `Pinned`, so the pid reuse races are tested deterministically (`a_pid_reused_before_the_pin_is_never_signalled`, `a_pid_reused_after_the_pin_is_never_signalled`, `an_unreadable_proc_is_an_unproven_stop`, `an_opaque_process_is_unproven_unless_it_is_older_than_the_run`). An unproven stop discards the repetition (`an_unproven_stop_discards_the_repetition`). `kill_leftover_group`, `kill_pid` and `kill_group` are main's, unchanged, and keep their callers on main (the interrupt guard and the clean path before a tree is removed); the code this change adds calls none of them. The `stop_rule` of round 8 is gone with the numeric signals it guarded. No dependency was added: `libc` gives the two syscalls.

Round 10 (the fourth independent code review, again in the stop proof):

46. The scan could miss a helper that made itself not dumpable (its `/proc` entry then reads as root's), one that cleared its environment and left the repetition and the driver's group, or one whose driver exited before its pid was read; and an unproven stop on macOS could leave a capture behind a read-only directory while reporting the repetition discarded: confirmed. The stop is cut (Cut 1): an `--external-data` repetition with doubles is never kept on any platform, its capture and its directory are removed once the checks ran (`remove_external_repetition`, the private mode put back through a descriptor and the removal retried), and a removal that fails names the path and fails the repetition (`external_data_removal`) without saying it was removed. Tests: `a_passing_external_data_repetition_leaves_nothing`, `a_failing_external_data_repetition_leaves_nothing`, `an_external_data_repetition_with_a_live_driver_leaves_nothing`, `an_external_data_repetition_that_cannot_be_removed_fails_and_says_where`, `a_doubled_repetition_without_external_data_keeps_its_tree_and_capture`, and the unit tests `an_external_repetition_is_removed_whole` and `a_repetition_that_cannot_be_removed_names_what_stayed`. Items 29, 36, 41, 43 and 45 are superseded by this one.

Round 11 (the fifth independent code review):

47. Accepted, blocker: the capture was removed by path through `run_dir` (`eval::doubles::remove_capture`), so a `tree/.apb` moved out of the repetition and replaced by a link sent the removal outside and reported success while the moved run stayed. (The repetition itself went through `std::fs::remove_dir_all`, which on unix opens each level with `O_NOFOLLOW` and unlinks a link without following it; the path-based capture removal and the mode reset were the gaps.) `remove_external_repetition` now opens the scratch directory and reaches every entry through `fsutil::beneath`: it checks the run tree component by component (a link or a missing component means the run tree was moved: `external_data_removal` with the path, nothing outside touched), removes the capture and then the repetition with `Beneath::remove_tree`, and restores modes with the new `Beneath::make_dirs_private` (its own test `make_dirs_private_never_follows_a_link`). Tests: `a_moved_run_tree_is_reported_and_nothing_outside_is_touched`, `a_link_inside_the_tree_is_unlinked_not_followed`, and the CLI test `an_external_data_repetition_whose_run_tree_was_moved_fails_and_touches_nothing_outside`.
48. Rejected, major (a helper that recreates `$APB_RUN_DIR` after the removal and writes there): the never-kept rule covers what apb wrote and what lies in the repetition directory when it is removed. It does not contain the evaluated agent: a process the agent left running, or the agent itself during the run, can write the data it was given anywhere its user can write, a recreated path included. Containing that is the node's isolation (docs/ISOLATION.md), not the eval runner, and docs/EXTERNAL-DATA.md already limits the external-data promise to what apb itself keeps. The boundary is written in D1 "Running a doubled case" item 5, the spec delta and docs/EVALS.md, which recommends `isolation: full` for external-data evals of untrusted agents.
49. Own review M1, a replayed doubled call failed the checks as `capture_incomplete`: confirmed (a replay wrote no capture line while the retry journaled its own `connector_call`). The replay appends a synced `replayed` line naming its reservation before it is dispatched, and `capture_gap` counts reservations plus replays per function (`CapturedCall.replays`). `a_crash_between_reservation_and_delivery_replays_the_answer` and `an_unrecorded_delivery_returns_no_answer` now run the checks and pass; before the retry the missing delivery still fails them.
50. Own review m2, the scratch guard kept an external-data repetition with doubles: confirmed (`ScratchGuard::drop` moved a live run's repetition to `kept/`). `LiveRun.never_kept` makes the guard remove it through `remove_external_repetition` instead (`guard_action`, unit-tested as `the_scratch_guard_never_keeps_an_external_data_repetition`; the unwinding path itself has no seam-free test).
51. Own review m1 and m3 are items 47 and 48; the "cannot create an entry" wording is narrowed to what holds (what a process writes afterwards by path lands outside any stored or kept run and goes with the scratch directory).

Round 12 (the recheck of round 7, the last):

52. Recheck M1 (a run tree moved and replaced by a real directory is not detected): rejected under the boundary of item 48: a step that moves its run directory elsewhere and leaves a fresh directory behind is the agent keeping its own copy, which only the node's isolation bounds. The code no longer claims more: the success text says only "the repetition directory was removed", and the rule is written as "apb removes the repetition directory it created, following no link and touching nothing outside it", with the moved-tree check stated as best effort. No device and inode tracking.
53. m1: the modes are put back before the run tree is checked, and the check names what it found: a link (the run tree was moved), a missing component, one that cannot be read; the message says the repetition directory was removed only when it was (`run_tree_problem`; `an_unreadable_component_is_reset_before_the_check`, `a_missing_component_is_reported_as_missing`).
54. m2: a capture removal that fails is the `external_data_removal` error with its path (`a_capture_that_cannot_be_removed_is_reported`), and the tree is checked again after the capture removal, so a link swapped in meanwhile is reported; descriptors never follow it.
55. m3: the comment in `run_repetition` carries the narrowed wording. m4: `eval::doubles::remove_capture` and its test lines are removed, and the design names `CAPTURE_ENTRIES` and `remove_external_repetition` instead. m5: the docs say "follows no link", and that a filesystem a step mounted inside the repetition directory is removed with it like any other content.
