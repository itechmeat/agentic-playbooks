# Proposal: eval doubles

## Why

This is a change of the 1.0 sequence (people-in-the-loop,
before-running-alone, typed-inbox-reads, start-rules-and-schedules,
eval-doubles, inbox-waits, incremental-lists-and-session-fork,
mcp-tool-connectors, mcp-http-and-surfaces, one-point-oh-fixes,
one-point-oh-freeze), with `wait-park` merged before it. It is built on
main efae5822, where `typed-inbox-reads` (#287), `people-in-the-loop`
(#288), `before-running-alone` (#289) and `wait-park` (#290) are merged
(archived under `openspec/changes/archive/`); every name below was read
there. `start-rules-and-schedules` is not merged and nothing here needs
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
2. The `completion_check` labels measure the wrong thing (GitHub issue
   205). `CompletionLabeller::label`
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
   before the call is dispatched; an `--external-data` repetition with doubles is never kept: its capture and directory are removed once its checks ran. `checks.connector_calls`
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
   closes issue 205). `completion_check` labels get a rule version, as
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
3. The lift of the `irreversible` refusal (plan section 7.4, tasks
   section 6 of the combined draft) is not built. A node's declared
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
   downstream-outcome column in the report (issue 205's optional item):
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
