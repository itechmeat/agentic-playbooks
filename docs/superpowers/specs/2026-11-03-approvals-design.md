# Approvals and person-only gates: an exact approval, a channel that does not decide, and the run guard

Date: 2026-11-03. Status: accepted. This is the second of three parts of
the evidence and approvals work; result snapshots came first
(`2026-11-03-result-snapshots-design.md`), citations for node outputs
follow (section 12). Author-facing usage: `docs/HOWTO-authoring.md`
("Approving exact results", "Gates only a person decides"), the channel
and refusal tables in `docs/GUARDRAILS.md` ("Approvals and person-only
gates"), the tools in `docs/MCP.md`, the publish relation in
`docs/PUBLISH.md`.

## 1. Goal

A `human_review` today records an option. Three things are missing:

1. **What was approved.** A person approves "the change", but the bytes
   they read may be rewritten before the next node acts, and a decision
   queued for one visit of a loop answers the next.
2. **Who decided.** Any channel may decide any gate: the host agent over
   MCP, a run step through the CLI, a script with a bearer key. A gate the
   author means for a person has no way to say so.
3. **Older executors.** An older apb drops every new field it does not
   know, so it would continue such a run with plain gates.

A gate now binds its request to exact result snapshots, a later node
re-checks that approval before it acts, a gate can refuse every channel
apb does not count as a person's, and a run that relies on any of this
carries a guard an older apb refuses.

## 2. Declaration

1. **`approves: { results: [<node>...], options?: [<option>...] }`** on a
   `human_review`. `options` are the options that mean approval; the
   default is `approve` when it is an effective option, else the gate's
   first option (`Approves::approving_options`). Every named node is
   snapshotted (`validate::snapshotted_nodes` includes it), with its
   output and fields even without its own `results:`.
2. **`requires_approval: { gate, of?, live: check | ignore }`** on a node
   that does work (`effects::node_acts`, or a `publish`). `of` defaults to
   the gate's `approves.results`; `live` defaults to `check`.
3. **`person_only: bool`** on a `human_review` or a `publish` node.
4. The three fields live on `Node`, are refused when unknown keys appear
   inside them, and are skipped on the wire when absent.

## 3. Binding a request

1. When a gate with `approves` asks, the drive resolves for each named node
   its latest stamped execution (for a list node, the node snapshot and the
   item snapshots of its latest visit) and builds `ReviewBinding { nonce,
   snapshots: [BoundSnapshot { node, execution, item, settlement_seq,
   snapshot_id, incomplete, redacted, omitted }], missing: [{ node, reason:
   not_run | no_snapshot }] }`. A node whose latest finished execution has
   no stamped snapshot of its own is `no_snapshot`; whether the bound
   snapshots verify is checked when the decision is consumed and at every
   re-check point.
2. **Binding nonce** (`approval::binding_nonce`): `short_nonce("approval",
   ["approval-v1", run_id, gate, "visit=<n>", "<node>|<settlement_seq>|
   <snapshot_id>" sorted, "missing:<node>|<reason>" sorted])`, where `n` is
   the gate's request count plus one. It changes with every request, so a
   decision queued for an earlier visit never answers a later one, even
   when the snapshots are the same.
3. `review_requested` gains `binding` (lenient: an unreadable shape is
   dropped) and `person_only` (written only when true). `PendingReview`
   carries both from the latest request; MCP `pending_review`, HTTP
   `progress.pending_reviews`, `apb runs` and the review panel show the
   bound snapshots, the missing nodes and the nonce.

## 4. Deciding

1. A decision on a bound gate carries the binding nonce in the field a
   publish consent uses (MCP `review_decide.nonce`, `apb review --nonce`,
   HTTP body `nonce`); there is no second consent mechanism.
2. Rules in `review::post_decision_via`, under the channel lock: an
   approving option needs the open binding's nonce (missing: invalid,
   naming the binding; stale: conflict, naming the current one); a
   non-approving option without a nonce is stamped with the open one; a
   second decision for the same nonce is refused; a journal or channel
   that cannot be read is a conflict to retry, never an accept.
3. `ReviewEntry` gains `request` and `channel`, stamped for every bound or
   person-only gate. Older readers ignore both.
4. The drive consumes a bound or person-only gate's decision only from an
   entry whose `request` is the open request (and, for a person-only gate,
   whose `channel` decides it, section 6). An unstamped entry, one for
   another request or binding, or one from a non-person channel stays in
   the file unconsumed, and `apb doctor --run` lists it (`reviews` row).
5. **Re-check at consumption.** Before journaling `review_decided`, the
   drive re-verifies every bound snapshot (manifest and every object). A
   failure ends the run failed with `approval_snapshot_unreadable`; nothing
   is decided on content apb can no longer show.
6. `review_decided` gains `binding` (the nonce) and `via` (the channel of
   a decision through a surface). An engine decision (an enforced
   recommendation, an unattended `decide`) is stamped with the open nonce
   and keeps `decided_by` without `via`: one fact, one field.
7. An unattended or enforced approving decision on a bound gate satisfies
   `requires_approval` downstream, labelled by `decided_by`; `person_only`
   is the declaration for a gate only a person may approve.

## 5. Re-check points

`scheduler::approval::check_before` runs when the drive admits a node with
`requires_approval`, before `node_started` and before any attempt, in the
sequential and the batch path, and again as each execution of the node
begins (`node::execute_node_kind`): the resume of an open visit after a
pause or a quota pause, a deferred retry and each list item. Immediate
retries inside one execution are not checked again, since the tree then
holds the node's own work:

| Check | Refusal |
|---|---|
| the gate's latest decision is an approving option with a binding, and every node in `of` is bound | `approval_missing` |
| no node in `of` has a newer stamped execution than the bound one | `approval_stale: <node> ran again after approval` |
| the bound snapshots verify | `approval_snapshot_unreadable` |
| no bound snapshot is incomplete | `approval_incomplete` |
| `live: check`: every bound file entry, re-hashed live through `Beneath`, equals its digest; a redacted entry cannot be compared | `approval_stale: <path> changed`, `approval_unverifiable: <path> was stored redacted` |

A failure at admission journals `node_started` and `node_finished {
failed, output: "<code>: <detail>" }` with no attempt; a failure as an
execution begins settles the open visit failed with the same output and
starts no attempt, so no side effect runs and routing
follows the failure edges. For a list item the refusal settles that item,
not the visit: a rejection that counts against `batch.retries`, then
`needs_look`. The check reads the tree as it is when the execution begins,
so it also sees the node's own work: an attempt that edited a bound file
and was then paused refuses its resume (`approval_stale`), and an item of a
sequential list node refuses on the previous item's write. Both fail
closed; `live: ignore` is the author's way out. `live: ignore` keeps the first four checks. An
approval stays a fact about the snapshot when content changes; every later
re-check refuses, and a fresh approval needs a new request (a loop back,
or a supervisor retry of the gate).

## 6. Person-only channels

1. **The promise.** A gate marked `person_only` refuses decisions from
   channels apb does not count as a person's: MCP, the CLI inside a run
   step or without a terminal, the HTTP API with a bearer key or without
   keys, and the engine. It accepts the CLI at an interactive terminal
   outside a run step after a typed confirmation, and the dashboard with a
   signed-in session. It does not prove that a person decided.
2. **Channels** (`decision_channel::DecisionChannel`): `terminal`,
   `cli_unattended`, `dashboard_session`, `dashboard_keyless`,
   `http_bearer`, `mcp`, `run_step`, `scenario`, `engine`, `unattended`, and
   `unknown` for a name a newer apb wrote. `is_person_channel` is true for
   `terminal` and `dashboard_session` only; a scenario run's scripted
   decision counts inside a scenario run only.
3. **Facades.** MCP states `mcp`, or `run_step` inside a step. The CLI
   states `run_step` inside a step; for a person-only gate `terminal` only
   after the person types the option at the terminal question
   (`consent::ask_typed`), else `cli_unattended`. The HTTP auth layer
   inserts a `RequestChannel` (keyless, bearer, session) beside the client
   context, which the review route maps to its channel.
4. **Refusal.** `review::channel::refuse_for_person_only` answers
   `EngineError::ChannelRefused { refusal, message }` before anything is
   queued: an MCP tool error, HTTP 403, a CLI error. The message names the
   exact `apb review` command and the dashboard.
5. **Own run.** `review::channel::refuse_own_run`: a decision or answer
   from a run step to a run of its own tree (the run, its ancestors through
   `parent_run` to depth 8, and every descendant of them through child
   runs, so siblings too, whose outcome steers the shared parent) is
   refused `run_step_own_run`; a tree that cannot be read whole refuses
   the step (fail closed). The refusal holds on
   every gate, question and unknown send (MCP `review_decide`, `run_answer`
   for a person's question, `run_intent_resolve`; CLI `apb review`, `apb
   answer`, `apb publish approve`, `apb intent resolve`). Deciding another
   run's ordinary gate from a step stays allowed.
6. **Limits.** The HTTP server cannot see a caller's environment, so it
   cannot tell a run step; with keys on, a step holds no browser session,
   and with keys off person-only decisions are refused outright. Over MCP
   the server cannot tell a host agent from a person relaying, so MCP never
   decides a person-only gate.
7. **A key is a person's credential.** The same key a bearer request
   carries signs in to the dashboard (`POST /api/auth/login`), and a
   signed-in session is `dashboard_session`. A key holder can therefore
   decide a person-only gate; the `bearer_key` refusal guards against
   mistakes only. `apb server key issue` and `apb doctor` say not to hand
   a key to an agent, a script or CI that must not decide such gates.
8. **Supervisor patches.** A supervisor's patch of the current run or for
   the next runs (`scheduler::approval::patch_refusal`, the forward patch
   `definition_guard`) is refused `approval_weakened` when it drops
   `person_only`, drops or narrows `approves` (fewer results, more
   approving options), drops or loosens `requires_approval` (another gate,
   fewer results, `live: ignore`), removes a node declaring one of them,
   lets a node a person-only or approving gate stood before on every path
   be reached without it, or (in-run) changes where any option of such a
   gate routes (the option-to-target map is frozen).
9. **Moving past a gate.** One check, `scheduler::approval::skips_gate`,
   holds every move to a named node: an in-run patch's `continue_from`,
   `run_continue_from` and `node_retry` (at a parked gate and at a failed
   node, `supervisor::refuse_gate_skip`), and a resume's `from_node`
   (`refuse_resume_skip`, called by the CLI, MCP and the HTTP API). A
   person-only or approving gate that dominates the target counts as passed
   only by its latest decision when that decision answers the latest
   request, is an approving option on its binding (a gate that `approves`)
   or any option but `reject` (a gate that is only `person_only`), routes
   by an edge that leads to the target without the gate, and no node
   started since it leads back into the gate and not on to the target. A
   refused directive is journaled `supervisor_action { action:
   directive_refused }` and consumed; the run stays, an open request stays
   open. A resume past such a gate goes through only a person's channel
   (`terminal` after typing the node, `dashboard_session`).

## 7. Next actions and surfaces

1. `decide_review` of a bound gate carries `RequestRef.nonce` and a given
   `nonce` argument, with the bound snapshot ids in its `reason`.
   `principals` are `[person, operator]`, or `[person]` for a person-only
   gate.
2. `CallerRefusal` is a closed list: `read_only_server`, `supervisor_only`,
   `relay_to_person`, `not_this_caller`, `agent_channel`, `run_step`,
   `no_terminal`, `keyless_dashboard`, `bearer_key`, `run_step_own_run`.
   `next_action::mark_for_caller` marks each action with what the surface's
   channel may not take; MCP, HTTP and the CLI render the same wire names
   (a parity test runs every code through the three).
3. MCP `run_wait` at a person-only gate tells the host to ask the person to
   decide at a terminal or on the dashboard and to wait, never to call
   `review_decide`.
4. The dashboard's review panel lists the bound snapshots with links to
   their rows in the results panel; at a person-only gate it shows the
   buttons only with a session, and on a dashboard without keys the quoted
   `apb review ... --nonce ... --request review:<seq>` line per option and
   how to issue a key. A node refused by its approval check shows the code
   on its card.

| Capability | MCP | HTTP | CLI | Dashboard |
|---|---|---|---|---|
| decide a person-only gate | refused (`agent_channel`) | session: yes; bearer and keyless: 403 | terminal with typed confirmation: yes; no terminal or a run step: refused | session: buttons; keyless: CLI line |
| decide its own run from a run step | refused | not detectable (6.6) | refused | not applicable |
| bound gate decision | `nonce` | `nonce` | `--nonce` | buttons post the nonce |
| see binding and `person_only` | `pending_review` | `pending_reviews` | `apb runs` | review panel |

## 8. Publish

1. A publish node with `requires_approval` runs the section 5 checks before
   every plan, and one more: when its `from` node is bound, `HEAD` must be
   the commit its bound snapshot recorded in `provenance.head`, else the
   plan refuses `approval_mismatch`; a bound snapshot that recorded no
   commit refuses it too, since nothing approved can be compared.
2. The plan digest gains the line `approval=<binding nonce>` only when the
   node requires an approval, so the consent names what was approved; any
   other plan keeps exactly the digest it had.
3. An approval never replaces the publish consent. `person_only` on a
   publish node makes the consent a person's (section 6).

## 9. The run guard

1. **Why fields are not enough.** An older apb drops `binding`,
   `person_only`, `requires_approval` and the stamps, and would consume a
   queued decision by count and run an acting node without its check. A
   field it drops cannot refuse it.
2. **Event.** `run_guard { requires: [<feature>...] }`, not a checkpoint.
   Features: `approval_binding`, `person_only`, `required_approval`
   (`validate::guarded::GuardedFeature`, computed by `guarded_features`).
   The guard is active while the latest `run_guard` names any.
3. **Where.** After `run_provenance` at start when the definition uses a
   feature; before `patch_applied` with the new definition's features (a
   supervisor patch may not drop one, section 6 item 8; an empty
   `run_guard` after `patch_applied` ends a guard whose definition names
   none); and after
   every checkpoint while active, in the same buffer and write as the
   checkpoint, after the deferral guard (`event::append_inner`), so no
   call site can forget it.
4. **Older binaries** meet an unknown type after the last checkpoint and
   refuse to resume, answer through their engine or pause a driverless run
   ("written by a newer apb"); their stop goes through. Their review poster
   appends an unstamped entry the new drive never consumes.
5. **Newer binaries.** A `run_guard` naming a feature this binary does not
   know reads as an unknown event, so resume refuses and stop goes through;
   later releases add feature names, not guard types.
6. **Crash.** One buffer, one write: a crash leaves neither line, both, or
   a torn tail every binary's strict read refuses. Not covered: a crash
   between `run_provenance` and the first guard, before any checkpoint.
7. **Not claimed.** An older apb that *starts* such a playbook drops the
   fields and runs plain gates; nothing it reads can refuse that. An older
   apb that *stops* such a run writes its own `run_aborted` as the last
   checkpoint with no guard after it, and beta.1 and 0.25.0 resume an
   aborted run: after its stop, a second resume of that same older binary
   skips the guards before the checkpoint, takes an unstamped decision and
   runs the gated nodes (corpus row E5 pins this). No write of this
   binary is part of that sequence. Documented, and `apb doctor` warns
   when another `apb` on `PATH` exists.

## 10. Journal and older binaries

| Change | beta.1 and 0.25.0 | Rule |
|---|---|---|
| `review_requested.binding`, `review_requested.person_only` | dropped; the run is refused by the guard | section 9 |
| `review_decided.binding`, `review_decided.via` | dropped (display only) | a new field is `serde(default)` and lenient |
| `run_guard` | unknown after the last checkpoint: resume and writers refused, stop works | section 9 |
| `ReviewEntry.request`, `ReviewEntry.channel` | ignored | channel file |
| publish refusal `approval_mismatch` | read as `unrecognized` | publish |
| node fields `approves`, `requires_approval`, `person_only` | dropped on load: a start runs plain gates | section 9 item 7 |

The journal corpus pins these against the real old binaries (rows E2 to
E8): a run waiting at a bound person-only gate, paused at it, with an
unreached `requires_approval` node behind sibling checkpoints, stopped by
an older apb (which that older apb can then continue, the stated limit of
section 9 item 7), torn inside the guard's write, a guard ended by an
empty `run_guard` after a patch (written by hand: no supervisor patch may
drop a feature), and a guard naming an unknown feature.

## 11. Validator

| Code | Rule |
|---|---|
| V155 | `person_only` on a node other than `human_review` or `publish` |
| V156 | `person_only` with `auto_decide` or `unattended: { decide }` |
| V157 | `approves` not on a `human_review`; empty `results`; a named node unknown, settling no result or not reaching the gate; `options` empty or not offered |
| V158 | `requires_approval` on a node that does no work; a gate unknown, not a `human_review`, without `approves` or not reaching the node; `of` empty or not a subset |

All are errors. The bundled playbooks use none of the fields.

## 12. Threat model

**Assets.** A person's decision on a gate; that an approving decision
answers exactly the bytes shown; that later work does not act on anything
else; that an older executor never continues such a run. **Actors.** The
host agent over MCP; a run step over CLI, MCP or HTTP; local processes
holding a bearer key; a browser session; the engine and an unattended run;
a supervisor agent patching the run or its playbook; an older apb.

| Threat | Control |
|---|---|
| a decision for an older visit or other snapshots | the nonce over run, gate, visit and every bound snapshot |
| tampering between post and consumption | re-verify at consumption; `approval_snapshot_unreadable` ends the run |
| the producer re-ran | `approval_stale` at admission |
| the live file was edited, also while the run was paused | `live: check` re-hash through `Beneath`, at admission and as each execution begins |
| a redacted or incomplete snapshot | `approval_unverifiable`, `approval_incomplete` |
| a push of another commit | `approval_mismatch`, also when the approved result recorded no commit |
| a supervisor patch that strips or skips a gate or an approval, or re-points its options | `approval_weakened` (section 6 item 8) |
| a directive or an agent's resume that moves the run past a gate, on a rejection or an earlier loop's decision | `skips_gate`, `directive_refused`, `approval_weakened` (section 6 item 9) |
| an agent deciding a person-only gate | the channel refusal before anything is queued, and consumption only of person-stamped entries |
| a run step deciding its own gate, or a sibling's | `run_step_own_run` over the whole run tree, failing closed |
| an entry appended by hand or by an older apb | never consumed; listed by the doctor |
| an older apb continuing the run | the run guard |

**Out of the boundary.** A process that controls a terminal, holds a
session cookie or a server key (a key signs in, section 6 item 7) or
writes `reviews.jsonl` with forged stamps; files not
declared; meaning; `live: ignore` by the author's choice; an older apb
that starts such a playbook, or that stops such a run and then resumes it;
a hand-edited journal; a supervisor's in-run patch that rewrites the
definition of a node behind a person-only gate (its script or prompt) or
adds nodes before the gate that do the protected work, which are the
supervisor's general powers, not a weakening of the gate's fields (run
without a supervisor, or without `patch_playbook` in its `capabilities`,
to rule them out). Availability: a run tree with a deleted or pruned child run
does not read whole, so every run-step decision on it is refused.

Later part: record packages (`format: records`) and citations for node
outputs, with `evidence` added to the run guard's features.

## 13. Not covered

A consent time-to-live (a snapshot is immutable and every use of live
content is re-checked); proving a person; person-only questions
(`answer_by: human` gains only the own-run refusal); the run view's
"approved snapshot; live file differs" line for `live: ignore`; approval
state on result rows; new kinds of push.
