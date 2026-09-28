# Decision models

Optional, provider-agnostic support for decision models: small, fast models
that answer typed questions over a text state and return probabilities,
never prose. `choice` picks one of up to 255 named options, `score` places the
state on 2 to 10 ordered levels, and `noul` gives the probability of "yes". A
decision takes a fraction of a second and a fraction of a cent, where an agent
turn takes a minute or more. Decision models cannot write text: every
generative step stays with agents.

**Without a `decisions.yaml`, apb behaves exactly as without this feature:**
no request, no event, and manifests, journals, CLI output and MCP responses
are unchanged.

The full design, including the uses that are not shipped yet, is issue #165.

## Status

| Use | What it asks | Modes available |
|---|---|---|
| `completion_check` | whether a successful agent reply is a finished result rather than a progress note, a plan or a question back | `off`, `shadow`, `advise`, `enforce` |
| `retry_advice` | whether a same-executor retry after an agent failure is likely to help | `off`, `shadow`, `advise`, `enforce` |
| `supervisor_triage` | what a supervisor should do about a park wake | `off`, `shadow`, `advise`, `enforce` |
| `review_triage` | which option a reviewer would most likely pick at a `human_review` gate | `off`, `shadow`, `advise`, `enforce` |
| `routing` | which of a profile's executor tiers a step needs | `off`, `shadow`, `advise`, `enforce` |

Shadow means journal only: the answer is recorded in the run's journal and
nothing acts on it. Advise shows the answer where a person or supervisor
decides (an anomaly wake, a wake's `triage`, a gate's `recommendation`) and
never applies it. Enforce changes engine behaviour, but only under the common
rules below; without a stored threshold every enforce path behaves as advise.
Every threshold named here is a placeholder until measured on shadow data.

## Configuration: `<config_dir>/decisions.yaml`

A file of its own next to `config.yaml` (never a section inside it, which an
older apb would refuse), with its own `version`. `<config_dir>` is
`APB_CONFIG_DIR`, else `$XDG_CONFIG_HOME/apb`, else `~/.config/apb`.

```yaml
version: 1
mode: shadow            # the ceiling for every use: off | shadow | advise | enforce
timeout_ms: 3000        # per decision, retries included
providers:              # tried in order
  - id: main
    kind: systemone
    base_url: https://api.typesafe.ai
    model: jev-1.13.0   # pin a versioned id: aliases move, thresholds do not transfer
    api_key: "{{env.TYPESAFE_API_KEY}}"
  - id: local
    kind: systemone
    base_url: http://127.0.0.1:8080
    model: laya
    data_class: local   # a server on this machine or network
budget: { max_requests_per_run: 200, max_usd_per_run: 0.05 }
uses:                   # absent = off
  completion_check:
    mode: shadow
    thresholds: { final_result: 0.15 }   # the default, see below
privacy:
  send: [prompts, outputs]   # `diffs` is opt-in
  redact: true
  max_state_bytes: 24000
  debug_state: false
```

- `api_key` is a reference only: `{{env.VAR}}` (the process environment, then
  the global `<config_dir>/secrets.env`) or `{{cmd:...}}` (a command whose
  output is the key, run when the run first asks). A literal value is a load
  error that names the field and never repeats the value. A project's
  `.apb/secrets.env` is never consulted for a provider key.
- A key that does not resolve leaves only that provider out.
- An unknown key, an unknown use, an invalid mode or URL makes the file
  invalid. An invalid file leaves the layer off (a warning on stderr and in
  `apb doctor`); it never fails a run.
- `kind: fake` answers from a script (`answers:` by question id, in the wire
  shape) and loads only with `APB_DECISIONS_ALLOW_FAKE=1`.
- The effective mode of a use is the lower of the ceiling and its own mode.
- `APB_DECISIONS=off` switches every use off for the process, checked again
  before every decision.

Provider routes for `kind: systemone` (all `POST {base_url}/v1/systemone`):

| Route | `base_url` | Model id |
|---|---|---|
| TypeSafe | `https://api.typesafe.ai` | `jev-1.13.0` |
| OpenRouter | `https://openrouter.ai/api` | `typesafe/jev-1.13` |
| Vercel AI Gateway | `https://ai-gateway.vercel.sh/typesafe` | `typesafe-ai/jev` |
| OpenCode Zen | `https://opencode.ai/zen` | `jev-1.13` |
| Self-hosted | `http://127.0.0.1:<port>` | the server's own id |

### Project narrowing

The committed `.apb/config.yaml` is repository content, so it can only
narrow what the machine allows:

```yaml
decisions:
  enabled: false                # or narrow instead:
  mode: shadow                  # lower the ceiling
  send: []                      # a subset of the machine's `send`
  data_class: local             # keep requests on local providers
  uses: { completion_check: { mode: off } }
```

Anything else there (a provider, a URL, a key, a budget, an unknown key)
opts the project out of the layer altogether; `apb doctor` says why.

A node can switch the completion check off for itself with
`completion_check: off` (see HOWTO-authoring.md).

### `apb doctor`

One line: `not configured`, or the ceiling, the effective use modes and each
provider's id, kind, host, whether its key resolves and whether a TCP
connection to its host opens. No key and no request are ever sent by doctor.

## What is sent

A run snapshots the effective settings into its manifest at start (a
`decisions` block with provider ids, kinds, URLs, models, data class and the
key *reference*, never a key), so editing `decisions.yaml` does not change a
started run.

For the completion check, after an `agent_task` attempt reported success and
passed its `success_check`, and before the node finishes, one request carries:

- `task`: the attempt's rendered prompt, first 4 kB;
- `result`: the attempt's raw output, first 1 kB and last 8 kB (never a
  summary);
- `meta.missing_fields`: the declared `outputs.fields` the output leaves out;

and two questions in one call:

- `final_result` (noul): "Is `result` a finished result for `task`, rather
  than a progress note, a plan, or a question back to the user?"
- `completion` (choice): `complete`, `partial`, `not_started`,
  `blocked_on_input`, `unclear`, with criteria that do not count follow-ups
  or later-phase steps against a finished result.

It is skipped for a node with a script `success_check` (a checked fact
outranks a judgment), for interrupted, failed or empty-output attempts, and for
`completion_check: off`. The decision's latency is part of the attempt's
duration. Any failure (timeout, provider down, spent budget) is journaled and
otherwise ignored: the check fails open.

Before anything leaves the machine, every text field is:

1. emptied unless its class (`prompts`, `outputs`) is in `privacy.send`;
2. redacted (unless `privacy.redact: false`): the values of every variable an
   installed connector references and of the provider keys become
   `[redacted]`; token-shaped strings (JWTs, well-known key prefixes, bearer
   values, long mixed-case runs with digits) become `[redacted-token]`;
   absolute paths under the project become repo-relative and other home
   paths `~/...`; e-mail addresses become `[email]`;
3. clipped to its own budget and then to its share of
   `privacy.max_state_bytes`, keeping head and tail with the cut marked.

Hosted providers keep what they receive under their own terms (TypeSafe
offers zero retention only to enterprise customers). Use `data_class: local`
providers and `send` to keep material on the machine.

Decision outputs are used for evaluation and threshold tuning only. apb has no
feature that exports decisions for training a model; the provider terms
(TypeSafe: https://typesafe.ai/legal/mca) forbid training an imitating model
on outputs.

## The `decision_made` event

Written to the run journal through the attempt journal *before* anything reads
the answer, so a resumed run replays a journaled decision (same use, node,
attempt, state and questions) with no request.

```json
{"type":"decision_made","use_site":"completion_check","node":"fix","attempt":2,
 "provider":"main","model":"jev-1.13.0","calibrated":true,"mode":"shadow",
 "questions_digest":"sha256:...","state_digest":"sha256:...","state_bytes":5120,
 "answers":{"final_result":{"p":0.91},"completion":{"value":"complete","p":0.83,"confidence":0.77}},
 "applied":false,"would_change":false,"baseline":{"regex_flag":false,"pattern":null},
 "latency_ms":212,"input_tokens":1840,"cost_usd":0.0000773,"cost_estimated":true,
 "cached":false,"error":null}
```

- `answers` is compact: the value, its probability and the confidence.
- `applied` is true only when behaviour changed, which never happens in shadow.
- `would_change`: `final_result` below `uses.completion_check.thresholds.final_result`
  (default 0.15). The `completion` choice never flags on its own. The cut is a
  measured starting point (an offline evaluation of about 490 labelled
  replies) and is to be re-fitted on shadow data; no enforce cut exists.
- `baseline`: a code-only verdict on the same reply, for comparison: a
  declared field missing, a reply ending in `?`, a reply under 80 characters,
  or a generic "not done yet" phrase in its last 600 characters ("still
  running", "I'll pause", "waiting for", "let me", ...). `pattern` names what
  flagged.
- `cost_usd` is the provider's figure, or with `cost_estimated` the list price
  (USD 0.042 per million input tokens for the Jev 1.13 ids).
- `error`: `unavailable`, `timeout`, `rate_limited`, `auth`, `budget`,
  `invalid`, or null. Never a key or a body.
- Nothing in the event holds state text; `state_digest` identifies it.

An older apb that does not know `decision_made` skips it: the attempt it
belongs to still ends with its own `attempt_finished` and `node_finished`,
which a shadow decision never changes.

With `privacy.debug_state: true`, the redacted state, the questions and the
full answer distributions of each decision are kept in
`runs/<id>/decisions/<seq>.json`.

### Budget

`budget.max_requests_per_run` and `budget.max_usd_per_run` count every
request of the run, resumes included. Past either, a decision is journaled
with `error: budget` and nothing is sent.

## Engine uses (issue #165 Parts 8 to 12)

Each use lives in its own module under `crates/apb-engine/src/decision/` and
asks through the one runner. Every decision records join keys under
`decision_made.join` for the report's labellers.

### Completion check (Part 8)

Asked after an agent attempt reported success and passed its
`success_check`; never for a script `success_check`, an empty output or
`completion_check: off`. State: `task` (prompt head 4 kB), `result` (reply
head 1 kB and tail 8 kB), `meta.missing_fields`. A generic regex verdict is
recorded alongside. A decision is flagged when `final_result` is below
`uses.completion_check.thresholds.final_result` (default 0.15); the
`completion` choice never flags on its own.

- shadow: `would_change` only.
- advise: one `WakeRaised` `anomaly` per flagged attempt, detail
  ``agent_task node `fix` attempt 2 reported success, but the completion
  check rates it partial (p=0.81) and final_result p=0.12``. The attempt's
  status never changes.

### Retry advice (Part 9)

Asked only for an attempt that failed with the `agent` failure kind while a
same-executor retry is still pending; never for transient, auth or budget
failures, deadline continuations, interrupts or cancellations. State: `step`
(title and prompt head 2 kB), `failure` (last 6 kB), `previous_failure` (the
execution's previous agent failure, when there is one) and `meta.attempt`,
`retries_left`, `fallbacks_left`. Questions: `next`
(`retry_same_likely_helps`, `switch_executor`, `stop_and_route_failure`,
`unclear`) and, with a previous failure, `repeat`. `would_change` when `next`
is not `retry_same_likely_helps` at `thresholds.min_confidence` (0.6) or
more; the retry runs as today below enforce. Join keys: `retries_left`,
`fallbacks_left`.

### Supervisor wake pre-triage (Part 10)

Asked once per park wake (a supervised run parked on a failed or timed-out
node), before the wake is raised; anomaly and question wakes are never
triaged. State: `step` (id, title, prompt 2 kB), `output_tail` (6 kB), and
`meta.trigger`, `failure_kind`, `attempt`, `retries_left`,
`alternative_executor`, `recent_actions` (the last five supervisor actions).
The `action` choice holds only what is legal now: `retry_same`,
`retry_with_note`, `switch_executor` (only with another executor or
profile), `continue_from_next` (only with a successor), `pause_for_human`,
`needs_supervisor`; plus the `looping` noul. Join key: `wake_seq`.

- advise: the wake gets `triage: {action, p, confidence, looping_p,
  provider, model}` (mirrored to a parent run with the wake), its detail ends
  with `Triage (advisory): retry_with_note p=0.78.`, and the supervisor's
  brief gets one sentence: follow the triage unless the detail contradicts
  it, and use `supervisor_run_inspect` only when neither is enough. With the
  use off or the provider down, the wake and the brief are byte-identical to
  before.

### Review gate recommendation (Part 11)

Asked once per gate visit, before `review_requested`. State: `gate` (title
and the literal `prompt`), `inputs` (the outputs of the gate's direct
predecessors that ran, 6 kB tail each, by node id). One `decision` choice
over the gate's effective options (the defaults included), with the gate's
`option_descriptions` as criteria. Join key: `gate_visit`.

- advise: `review_requested.recommendation: {option, p, confidence,
  provider, model, calibrated}`, and the instruction ends with `Advisory
  recommendation: approve (p=0.86).` `run_status`, `run_wait` and
  `supervisor_wait_event` carry it in `pending_review`. Nothing is
  preselected and the gate still waits for a person.

### Executor tier routing (Part 12)

For an `agent_task` with `route: auto` whose profile declares `tiers` (see
PROFILES.md), before the first attempt of each execution. State: `step`
(title and prompt head 3 kB) and `meta.declared_outputs`. Questions: `tier`
(a choice over the declared tiers with their `for` texts, plus `unclear`)
and `difficulty` (a five-level score, in words). Hysteresis: after a node of
the same profile ran on a tier, a different tier needs
`thresholds.hysteresis` (0.75) confidence. Join keys: `profile`,
`executor_tier`, `recommended`, `tier` (the tier after hysteresis).

- shadow and advise: the would-be tier is journaled, the profile's own
  executor runs, `would_change` when the tiers differ.
- Never routed, with `join.excluded` naming why and no request: a node with
  `continue_session`, a handoff source (a later node continues its session),
  a node a supervisor rebound. Retries and fallback steps are never routed:
  routing happens once per execution.

## Enforce (Part 14)

Common rules, applied by the runner for every use:

1. The use is in `enforce` for the run (the snapshot, capped by the machine
   and the project at the moment of the decision: `APB_DECISIONS=off`, a
   lowered ceiling or use mode, a removed file or a project opt-out stop
   every path mid-run), and the playbook opts in (below).
2. The answer is calibrated, or `uses.<name>.allow_uncalibrated: true`.
3. A threshold is stored for the use and the answering provider and model in
   `<config_dir>/decisions-thresholds.yaml` (`thresholds: [{use, provider,
   model, threshold}]`, written by the report tooling). The model must match
   exactly: a new model id never inherits.
4. At most `uses.<name>.max_actions` (default 3) automatic actions per use
   and run.

When a rule fails, the decision is journaled with `enforce_refused`
(`not_opted_in`, `uncalibrated`, `no_threshold`, `cap`, or `effects` at a
gate) and the use behaves as advise. An acting decision is journaled with
`applied: true` before anything acts on it, and a resumed run replays it
without a request, so the same path repeats. Every path is fail-open except
the review auto-decision, which is fail-closed.

| Path | Opt-in | Acts when | Action |
|---|---|---|---|
| Completion | node `completion_check: enforce` | `final_result` below the stored threshold and `completion` is not `blocked_on_input` | the attempt fails with reason `completion check: partial (p=0.84)`, the reply kept as `rejected_output`, consuming a normal retry; a `blocked_on_input` answer only raises the advise anomaly |
| Retry advice | `defaults.retry_advice: enforce` | `next` is `switch_executor` (with a fallback left) or `stop_and_route_failure` at the stored threshold's confidence | `switch_executor` skips the remaining same-executor retries to the next fallback; `stop_and_route_failure` fails the node now; each is journaled as a `supervisor_action` `retry_advice` marker |
| Supervisor auto-retry | `supervisor: { pre_triage: enforce }` | `action` is `retry_same` or `retry_with_note` at the stored threshold and `looping` below `thresholds.looping_max` (0.3) | the engine posts the same `node_retry` command a supervisor sends, after a `supervisor_action` `triage_retry` marker; `retry_with_note` first appends the code-template note `Previous attempt failed with: <failure_kind>; <first error line of the output tail>` to the run context; the wake is still raised, with `triage.applied: true` |
| Review auto-decision | gate `auto_decide: { allow: [needs_changes], min_confidence: 0.9 }` | the recommendation is an allowed option at the higher of the stored threshold and `min_confidence` | the option is posted through the review channel with note `auto: <provider>/<model> p=<p>` and journaled as an ordinary `review_decided`; revertible with `continue_from`; a decision a person posted first wins |
| Routing | node `route: auto` | the tier after hysteresis differs from the profile's executor tier at the stored threshold's confidence | the first attempt runs on that tier through the rebind overlay (`profile_rebound` with reason `routing: tier ...`); an agent failure on a tier below the executor goes up a tier at once (`fallback_triggered.reason: routing`) before the normal chain; a later execution routed back to the executor clears the overlay the same way |

The review auto-decision is refused by the validator (V73) on a playbook that
declares `irreversible` or `secrets` effects, or whose nodes after the gate
include a merge, push, deploy or publish step (by id, title or script path),
unless the gate sets `auto_decide_ok: true`. The inferred `external` effect
does not count (every playbook with an agent has it). The run re-checks the
same rule with the declared effects of the sub-playbooks it runs and refuses
with `enforce_refused: effects`. `allow` may contain only `needs_changes`,
never `approve` (V72).
