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

The full design is issue #165; every use below ships in the same release.

## Status

| Use | What it asks | Modes | Since | Threshold source |
|---|---|---|---|---|
| `completion_check` | whether a successful agent reply is a finished result rather than a progress note, a plan or a question back | `off`, `shadow`, `advise`, `enforce` | v0.22.0 | `uses.completion_check.thresholds.final_result` (default 0.15) for shadow and advise; enforce needs a stored threshold |
| `judge_node` | the questions a playbook's `judge` node declares | `off`, `shadow`, `advise` (journal only), `enforce` (routes) | v0.22.0 | the node's own `thresholds` in the playbook; no stored threshold |
| `judge_edge` | the yes/no question of a `judge` edge condition | `off`, `shadow`, `advise` (journal only), `enforce` (routes) | v0.22.0 | the edge's own `min_p`; no stored threshold |
| `retry_advice` | whether a same-executor retry after an agent failure is likely to help | `off`, `shadow`, `advise`, `enforce` | v0.22.0 | `uses.retry_advice.thresholds.min_confidence` (default 0.6); enforce needs a stored threshold |
| `supervisor_triage` | what a supervisor should do about a park wake | `off`, `shadow`, `advise`, `enforce` | v0.22.0 | `uses.supervisor_triage.thresholds.looping_max` (default 0.3); enforce needs a stored threshold |
| `review_triage` | which option a reviewer would most likely pick at a `human_review` gate | `off`, `shadow`, `advise`, `enforce` | v0.22.0 | enforce: the higher of the stored threshold and the gate's `auto_decide.min_confidence` |
| `routing` | which of a profile's executor tiers a step needs | `off`, `shadow`, `advise`, `enforce` | v0.22.0 | `uses.routing.thresholds.hysteresis` (default 0.75); enforce needs a stored threshold |
| `catalog_rank` | which catalog playbook fits the task an agent names, whether the task needs a playbook at all, and whether a silenced suggestion covers it (MCP, outside runs) | `off`, `shadow`, `advise` (`enforce` acts as `advise`) | v0.22.0 | `uses.catalog_rank.thresholds.covered` (default 0.8); advisory by design |

Shadow means journal only: the answer is recorded in the run's journal and
nothing acts on it. Advise shows the answer where a person or supervisor
decides (an anomaly wake, a wake's `triage`, a gate's `recommendation`) and
never applies it. Enforce changes engine behaviour, but only under the common
rules in "Enforce" below; without a stored threshold every engine enforce
path behaves as advise. The `judge` node and edge are declared by a playbook
and route only at `enforce`; below it they take their declared fallback (see
their sections). Every default threshold named here is a placeholder until
measured on shadow data (`apb decisions report`).

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

### Kill switch

`APB_DECISIONS=off`, `mode: off` (the ceiling or a use), a lowered ceiling
or use mode, a removed or broken file, and a project opt-out stop every call
to a decision provider, re-checked before each decision so they apply
mid-run: the native providers, the `llm_emulation` endpoints and the MCP
catalog ranking alike. A run's snapshot can only be lowered this way, never
raised.

One thing is not a decision provider and keeps running: a judge node's
profile emulation (`on_unavailable: emulate` through the node's `profile` or
`defaults.profile`). It is the playbook's own declared agent executor, so it
runs whatever the use's mode, the kill switch included, and its answer is
journaled with `calibrated: false` like any emulated decision.

Provider routes for `kind: systemone` (all `POST {base_url}/v1/systemone`):

| Route | `base_url` | Model id |
|---|---|---|
| TypeSafe | `https://api.typesafe.ai` | `jev-1.13.0` |
| OpenRouter | `https://openrouter.ai/api` | `typesafe/jev-1.13` |
| Vercel AI Gateway | `https://ai-gateway.vercel.sh/typesafe` | `typesafe-ai/jev` |
| OpenCode Zen | `https://opencode.ai/zen` | `jev-1.13` |
| Self-hosted | `http://127.0.0.1:<port>` | the server's own id |

### Other provider kinds

Three routes speak a variant of the same format and have a kind of their
own. Each keeps the shared contract: client-side limits (32,000 tokens for
the state plus the longest question, 255 options, 10 levels), strict reply
validation with a locally recomputed confidence where the route sends none,
retries only on 408, 409, 429, 5xx and 529 within the timeout, keys by
reference only. Each answers under its own threshold profile
(`<kind>:<model>`): a threshold measured on one route does not carry over to
another, even for the same model.

```yaml
providers:
  - id: vercel
    kind: vercel_evaluate          # POST {base_url}/v1/evaluate
    model: typesafe-ai/jev         # base_url defaults to https://ai-gateway.vercel.sh
    api_key: "{{env.AI_GATEWAY_API_KEY}}"
    zero_data_retention: true      # providerOptions.gateway.zeroDataRetention
  - id: openrouter-decisions
    kind: openrouter_decisions     # POST {base_url}/api/alpha/decisions (alpha)
    model: typesafe/jev-1.13       # base_url defaults to https://openrouter.ai
    api_key: "{{env.OPENROUTER_API_KEY}}"
  - id: cloudflare
    kind: cloudflare               # POST {base_url}/accounts/{account_id}/ai/run
    model: typesafe/jev            # base_url defaults to https://api.cloudflare.com/client/v4
    account_id: "{{env.CLOUDFLARE_ACCOUNT_ID}}"   # or the id itself (letters and digits)
    api_key: "{{env.CLOUDFLARE_API_TOKEN}}"       # a token with the Workers AI permission
```

| Kind | Differences from `systemone` | Calibrated | Cost reported | Retention (as documented) |
|---|---|---|---|---|
| `vercel_evaluate` | `noul` is sent as `boolean` and answered as `probability`; usage in camelCase; cost from `providerMetadata.gateway.cost`; choice and score may come without confidence (recomputed) | only when the answering model is `typesafe-ai/jev...`; a language-model fallback is not | yes | the gateway's terms; `zero_data_retention: true` restricts routing to zero-retention providers, and a request none can serve fails |
| `openrouter_decisions` | the same shape on OpenRouter's alpha Decisions route; 400 is a validation refusal | yes | yes (`usage.cost`) | OpenRouter's terms plus TypeSafe's |
| `cloudflare` | the body is `{"model", "input": {"state", "questions"}}`; the reply is accepted bare or in Cloudflare's `result` envelope | yes | no (list price applies) | the model page lists zero data retention |

- `openrouter_decisions` is an alpha API that may change without notice; the
  `systemone` kind against `https://openrouter.ai/api` reaches the same model
  on a stable path. Pin a versioned model id: one community report had the
  `~typesafe/jev-latest` alias fail on this route. `apb doctor` flags any
  alias model id (a leading `~`, `-latest`, `-preview`).
- `cloudflare` follows the REST form on Cloudflare's model page; it is covered
  by fixture tests but was not exercised against the live service.
- `account_id` belongs to `cloudflare` only and `zero_data_retention` to
  `vercel_evaluate` only; anywhere else they make the file invalid.

Sources (read 2026-09-27):
https://vercel.com/docs/ai-gateway/modalities/evaluation,
https://vercel.com/docs/ai-gateway/sdks-and-apis/typesafe,
https://openrouter.ai/docs/guides/community/jev.md,
https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-questions-and-answers-request.md,
https://developers.cloudflare.com/ai/models/typesafe/jev/.

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
provider's id, kind, host, whether its key resolves, and the outcome of one
request that costs nothing, sent with the provider's key to its own base URL
within `timeout_ms`: the HTTP status and latency (`GET /v1/models: HTTP 200
in 230 ms`), or `no answer within <timeout_ms> ms`, or why it failed. The
free request per kind is `GET /v1/models` (`systemone`, `vercel_evaluate`),
`GET /models` (`llm_emulation`), `GET /api/v1/key` (`openrouter_decisions`)
and `GET /user/tokens/verify` (`cloudflare`). A provider whose key comes from
a command (`{{cmd:...}}`, which only a run executes) or whose variable does
not resolve gets a TCP connect instead, reported as `reachable (connect
only)` or `unreachable (connect only)`. The line is a warning when any
provider's check fails. Doctor never sends a decision request and never
prints a key.

## What is sent

A run snapshots the effective settings into its manifest at start (a
`decisions` block with provider ids, kinds, URLs, models, data class and the
key *reference*, never a key). The manifest lives in the project tree, so a
running or resumed run treats the snapshot only as an upper bound: it keeps the
providers the machine's `decisions.yaml` still lists unchanged (kind, URL,
model, key reference), and takes the stricter of the snapshot and the live
file for the modes, the budget, the timeout, privacy and the enforce settings.
Loosening `decisions.yaml` does not widen a started run; tightening it, or
removing a provider, applies at once.

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

1. emptied unless its class (`prompts`, `outputs`) is in `privacy.send`. A
   node's rendered prompt (`task`, `step`) is `outputs` material when its
   template reads `{{nodes.*}}` or `{{run.context}}`, since it then carries
   agent output; the `detail` of earlier supervisor actions in
   `supervisor_triage`'s `meta` is sent only with `outputs`;
2. redacted (unless `privacy.redact: false`): the values of every variable an
   installed connector references and of the provider keys become
   `[redacted]`; token-shaped strings (JWTs, well-known key prefixes, bearer
   values, long mixed-case runs with digits) become `[redacted-token]`;
   absolute paths under the project become repo-relative and other home
   paths `~/...`; e-mail addresses become `[email]`;
3. clipped to its own budget and then to its share of
   `privacy.max_state_bytes`, keeping head and tail with the cut marked; the
   shares shrink until the state as serialized (JSON escapes included) fits
   `max_state_bytes`.

Hosted providers keep what they receive under their own terms (TypeSafe
offers zero retention only to enterprise customers). Use `data_class: local`
providers and `send` to keep material on the machine.

Decision outputs are used for evaluation and threshold tuning only
(`apb decisions report` and `apb decisions replay`, below). apb has no
feature that exports decisions for training a model; the provider terms
(TypeSafe: https://typesafe.ai/legal/mca) forbid training an imitating model
on outputs.

## The `decision_made` event

Written to the run journal through the attempt journal *before* anything reads
the answer, so a resumed run replays a journaled decision (same use, node,
attempt, state and questions) with no request. Each journaled decision is
replayed once, the latest first; a failed one replays as the same failure.
A replayed action is repeated only while the use is still in enforce and the
rest of the enforce gate still holds (opt-in, no refusal, calibration, a
stored threshold for the journaled provider and model).
Within one drive nothing is replayed: a node executed again (a loop, a
`node_retry`) is asked anew, journaled and counted, and an identical state
is then answered from the run's cache.

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
  `invalid`, `cancelled` (the run stopped the ask, for example a judge
  node's profile emulation on a stopped run; a resume asks again), or null.
  Never a key or a body.
- `output_chars`: characters of the output the use judged (the completion
  check: the raw reply), before redaction and clipping. The report uses it to
  set long outputs apart; absent in journals written before it existed.
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

## Cost and latency on run surfaces

Every read-only run surface reports the run's decisions as one compact object
when, and only when, the run journaled at least one `decision_made`:

- `apb runs <id>` and `apb wait` print one line, for example
  `decisions: 14 (2 replayed, 1 error), $0.0004, p50 190 ms; shadow would change: 3`
  (the parenthesis lists only what is not zero; `estimated` follows the cost
  when a price-table estimate is in it);
- MCP `run_status` and `run_report` carry `decisions`: `decisions`,
  `requests`, `replayed`, `errors`, `cost_usd`, `cost_estimated`,
  `p50_latency_ms`, `p95_latency_ms` and `by_use` (`requests`, `errors`,
  `applied`, `shadow_would_change` per use). Each decision's detail stays in
  `run_events`;
- the dashboard's run page shows a "Decisions" card (totals, a line per use,
  and per decision: use site, node, answers with their p, provider and model,
  latency, applied or shadow) and a note on each `decision_made` in the event
  list.

`requests` counts requests actually sent; `replayed` counts decisions answered
without a request (the run's cache). A resume that replays a journaled
decision journals nothing new, so it is in neither count. Latency percentiles
cover the requests actually sent. `apb runs` has no list column for it.

## Measuring a use: `apb decisions report`

```text
apb decisions report [--use USE] [--since 7d|2026-09-20] [--playbook ID]
                     [--provider ID] [--all-projects] [--json]
```

Reads run journals only (this project's, or every registered one with
`--all-projects`); it asks no model and writes nothing. Only run directories
apb created on this machine count (their `origin.stamp` verifies against the
installation key, as for an MCP resume): a `.apb/runs/<id>` that came with a
repository is skipped by the report and by `apb decisions replay`, and the
command names how many it skipped on stderr. Per use and
`(provider, model)`:

- counts, errors, and label coverage (labelled of answered);
- accuracy at the use's threshold, next to the majority class, today's
  behaviour ("always complete" for the completion check) and the journaled
  regex baseline on the same items;
- Brier score and a 10-bin expected calibration error of the probability that
  acting is right;
- a threshold table in 0.05 steps: coverage (share acted on), accuracy,
  recall and false-action rate with a Wilson 95% interval;
- the same core figures for long outputs (1,000 characters or more,
  `output_chars`);
- `would_change` accuracy;
- a labelled savings estimate at the threshold: correct actions times the
  median agent attempt wall time, against wrong actions at the same cost and
  the decisions' own cost and latency. It is an estimate from labels, not a
  measurement;
- "eligible for enforce: yes/no" with every reason.

An emulation provider (uncalibrated) is reported as its own group and is never
pooled with a decision model. With no matching decision the report prints
`no decisions recorded`.

**Labels** come from what the run did later, never from a model:

| Use | Label |
|---|---|
| `completion_check` | acting was right when, before the node starts again, a supervisor retried it, the run was moved back to it or to a node that ran before it (`run_continue_from`, a patch or migration `continue_from`), or the next node to start after it failed; wrong when the next node succeeded, or the node was the last and the run succeeded. Unlabelled when another attempt of the same visit followed (its outcome is not this attempt's), when above shadow the check's own anomaly wake came first (the outcome may be the decision's doing), and otherwise until the run shows an outcome |
| `review_triage` | acting (deciding on the recommendation) was right when the person chose the recommended option at the same gate visit (`gate_visit`, the decision's `attempt`), wrong when they chose another. Unlabelled while the visit is undecided or was withdrawn, and when the model decided the gate itself (an `auto:` note). A recommendation the reviewer was shown (advise or enforce) is labelled, but reported on its own line, "shown to the reviewer", and kept out of every figure: agreement with advice one has seen is a biased label. Only shadow decisions measure the model |
| `retry_advice` | acting (switching executor or stopping) was right when the next attempt of the node on the same agent and model failed too, wrong when it succeeded. Unlabelled when no further attempt followed, when the next attempt ran on another executor (a fallback), and when an enforced advice changed what ran next |
| other uses | not labelled yet: their decisions journal the join keys (`decision_made.join`: `wake_seq`, tier), and each use gets its labeller in a later release |

Unlabelled decisions stay out of every accuracy figure and are listed with the
reason.

**Eligibility** (the rule the enforce modes apply): a threshold stored for
exactly this provider and model (for the Part 14 paths: completion, retry
advice, supervisor triage, review triage and routing; a judge node or edge
enforces with the thresholds its playbook declares and reads none, and
`apb decisions thresholds set` refuses `judge_node`, `judge_edge` and
`catalog_rank`), at least 50 labelled decisions (20 per
option for a `choice` use), accuracy above both the majority class and
today's behaviour, and a false-action rate under the use's target (default
5%, `uses.<name>.thresholds.false_action_target`).

## Stored thresholds: `apb decisions thresholds`

```text
apb decisions thresholds set --use completion_check --provider main --model jev-1.13.0 --threshold 0.12
apb decisions thresholds list [--json]
```

`set` writes `<config_dir>/decisions-thresholds.yaml` (its own file with its
own `version`; numbers and names only). A threshold applies to exactly one
`(use, provider, model)`: a new model id, another provider or another use
never inherits it, and the report says a new shadow period is needed. Code
reads it through one lookup,
`apb_core::decision_thresholds::stored_threshold(use, provider, model) -> Option<f64>`;
`None` means an enforce path refuses and journals `enforce_refused:
no_threshold`. The value is on the use's own scale (for the completion check,
the `final_result` cut: flag below it). Without a stored threshold, use sites
keep their `uses.<name>.thresholds` defaults.

## Replay against another provider: `apb decisions replay`

```text
apb decisions replay --provider ID [--use USE] [--since 7d] [--max 200] [--json]
```

Re-asks decisions whose run kept its debug state (`privacy.debug_state`)
against another provider from `decisions.yaml` (another hosted model, a
self-hosted server, an emulation) and prints agreement with the original
answers and labelled accuracy side by side. The state sent is the one in the
debug file, redacted again before it is sent. Each run is replayed under its
project's settings as a run would see them now: `APB_DECISIONS=off` refuses
the replay, and a run whose project turned the layer or the use off, left
the provider out (`data_class: local`) or narrowed `send` is skipped and
counted. For a use without a labeller, agreement compares the answers
themselves (the same option or level, a noul on the same side of 0.5). It
refuses without `--provider`,
never writes a journal or anything under a run, and saves its results under
`<config_dir>/decisions-replay/`.

Replay is evaluation only, like threshold tuning: it compares answers against
APB's own labels. It is not an export of outputs for training, and apb has no
such feature (see the provider terms note under "What is sent").

## Catalog ranking (MCP, opt-in)

Off unless the machine's `decisions.yaml` enables it:

```yaml
uses:
  catalog_rank:
    mode: advise                   # shadow: ask and log, answer unchanged
    thresholds: { covered: 0.8 }   # the default; a starting point to be measured
    max_requests_per_day: 200      # per project and UTC day; the default
```

`playbook_catalog` then accepts an optional `query`, the task in one
sentence. One request asks a `choice` over the catalog's playbooks (their
`when`, `avoid_when` and `examples` in the state, clipped to
`privacy.max_state_bytes`, plus `none_of_these`), a `needs_playbook` noul
("is the task a doable action a saved procedure could perform?") and one
`covered` noul for each of the first 16 active silenced suggestions ("does
the suggestion `suggestions.s<i>` describe the same procedure?"; the synopses
travel in the state, never in the question text). The state is sent as `prompts` class and redacted like
a run's. Above 254 playbooks the catalog is ranked in chunks and the chunk
leaders again.

In advise the response is the full catalog plus `ranked` (the top five
`{ref, p, trusted, lifecycle, ambiguous}`, the last three copied from the
entry so the facts stand next to the advice), `confidence`, `needs_playbook_p`, `covered_by` (`{pattern,
scope, p}` when a record reaches the `covered` cut) and `ranking: {provider,
model, calibrated}`. Entries are never filtered or reordered, and nothing is
applied: the host still decides what to run and whether to offer a capture.
`enforce` acts as `advise`, since catalog ranking is advisory by design.
`revision` is bypassed when a query is ranked, and answers are cached for
the server's lifetime per project, catalog revision and query. A failure
keeps the full catalog with `ranking: {error}`.

A query about another workspace's catalog (`workspace` set) is narrowed by
both projects: the calling session's project and the target project each
apply their `decisions:` section, so the stricter setting of the two wins
(either one turning `catalog_rank` off keeps the plain catalog). The request
is logged in the target project.

The response is byte-identical to today without `decisions.yaml`, with the
use off, with `APB_DECISIONS=off`, when no provider key resolves, in shadow,
and whenever `query` is absent or blank. The server's instructions (TIER0)
never change.

Each decision is logged as one line of `<root>/.apb/decisions.jsonl`
(git-ignored): the `decision_made` fields without node and attempt, plus
`ts_ms`. Past `max_requests_per_day` a line with `error: budget` is logged and
nothing is sent. The count, the request and its line are taken under a lock
on the log, so concurrent MCP servers never pass the cap; a call that finds
the lock still held after about two seconds is not ranked (`ranking.error:
busy`, the plain catalog). A holder may keep the lock for a whole provider
chain, so the lock counts as abandoned only once it is older than
`timeout_ms` times the number of providers plus ten seconds; an abandoned
lock (its holder died) is broken and a `{"note": "stale_lock_broken"}` line
is logged. The log is opened only as a regular file, never
through a link: a log that is a link, a directory or anything else counts as
a spent cap (nothing is sent and nothing written), and only its last 8 MiB
are read to count the day's requests. The task, the triggers and a suggestion's synopsis
are redacted like a run's state (including the variables installed
connectors reference) before anything is clipped or sent.

## Judge node (`judge_node`)

A playbook's `judge` node (authoring: HOWTO-authoring.md, "Judge nodes and
judge edges") asks its own typed questions over its rendered state. The use
decides what happens with the answer:

- `enforce`: the answer is thresholded in code and becomes the node output;
  the `decision_made` has `applied: true` and is journaled before the output.
- `shadow`, `advise`: the question is asked and journaled (`applied: false`,
  `would_change` when the answer would have routed differently from the
  fallback), and the node applies its `on_unavailable` with reason `mode`.
- `off`, no `decisions.yaml`, `APB_DECISIONS=off`: nothing is asked and
  nothing is journaled; the node applies `on_unavailable` (reason
  `not_configured` or `off`).

A judge node's own question goes to the decision models of the chain only
(every kind but `llm_emulation`), never to an `llm_emulation` provider; those are used
by `on_unavailable: emulate`. The judge node needs no stored threshold: its
thresholds are declared by the author in the playbook, for the provider and
model the machine pins. Answers from an uncalibrated provider are used, since
the node declared them (the uncalibrated refusal applies to the engine's own
enforce uses).

State: every `state` field is rendered like a prompt; a field reading a node
output (or `run.context`) is `outputs` material, any other `prompts`, so
`privacy.send` applies. Each field is clipped (head and tail, 12 kB each)
before its share of `privacy.max_state_bytes`. `decision_made` carries
`node` and no `attempt`, so an unchanged state replays across a resume and
across loop passes.

Cache: with `cache: auto`, a judge node answered at enforce is stored in the
node cache under the node definition (questions, thresholds), the digest of
the rendered state and the first native provider and its pinned model; the
working tree is not part of the key. Fallback and emulated outputs are never
stored. A hit makes no request.

## LLM emulation (uncalibrated)

A chat model can imitate the decision interface through structured output,
which keeps judge playbooks portable to machines without a decision model and
makes A/B comparisons possible. Its probabilities are self-reported, not
calibrated: every emulated answer is journaled with `calibrated: false`,
reports group it apart, and thresholds tuned on a native model do not apply to
it. It costs roughly 10 to 25 times more per decision and is slower, so it is
never a silent substitute: it runs only where configured or declared.

The request (both backends): one JSON schema per request (a `noul` a number,
a `choice` an object with one number per option, a `score` one number per
level, all keyed by neutral ids `q1`, `q2`, ... rather than the playbook's
question ids), a system prompt asking for probabilities only and no reasoning,
and the state wrapped in `<document>...</document>` as untrusted data whose
instructions are to be ignored (a closing tag inside the state is defused).
The reply is normalised in code: each distribution is scaled to sum to 1, a
`choice` takes its argmax, a `score` its expected level index, the confidence
is recomputed with the native formula; a missing option or a value outside
[0, 1] makes that item invalid.

Two backends:

- **An OpenAI-compatible endpoint**, a provider in `decisions.yaml`:

  ```yaml
  providers:
    - { id: emulated, kind: llm_emulation, via: openai_compatible, base_url: https://api.example-llm-provider.com/v1, model: some-small-model, api_key: "{{env.EMULATION_API_KEY}}", structured_output: json_schema }
  ```

  `POST {base_url}/chat/completions`; `structured_output: json_schema`
  (default) sends `response_format: {type: json_schema, strict: true}`,
  `prompt_only` embeds the schema and parses the first JSON object of the
  reply. Keys by reference, retries and error mapping as for `systemone`. Fast,
  but a second credential.
- **An APB agent profile**, declared on the judge node: `on_unavailable:
  emulate` with `profile` (or `defaults.profile`). One attempt on the
  profile's primary executor (no fallback chain, at most one retry), a fresh
  session, no handoff, no autonomy grant, no status-verdict protocol; the
  reply's first JSON object is the answer. Journaled as a normal attempt of
  the node plus a `decision_made` with `provider: profile:<name>` and
  `calibrated: false`. No new credential, but an agent's cold start. The
  profile is snapshotted and trust-checked like any node profile. A
  `via: profile` entry in `decisions.yaml` is refused: declare it on the node.

`emulate` tries the configured `llm_emulation` providers first (when the
`judge_node` use is above off), then the profile. The profile backend runs
whatever the use's mode, `APB_DECISIONS=off` included, because it is the
playbook's own declared executor; its answer is replayed on resume like any
decision. A host-mode run (`execution: host`) never spawns an agent CLI, so
there the profile backend is unavailable: after the `llm_emulation` providers
give no answer, the node fails with the reason "host execution mode spawns no
agent CLI" and follows its failure edges, like an emulation that gave no
answer. It is not turned into a host task: a judge answer is a decision, not
an agent step.

## Judge edge (`judge_edge`)

A `judge` edge condition asks one yes/no question about its source node's
output. When a node succeeds, all its judge edges are asked in one request
(question ids `edge_<index>`, the edge's position among the node's outgoing
edges; state `step`, the node title, and `output`), journaled with the node's
execution count as `attempt` before any routing, so each loop pass gets its
own answer. Edge selection stays a pure fold over the journal: an edge
matches at `p >= min_p` when that execution's decision was applied (the use
at enforce), and otherwise exactly when its mandatory `on_unavailable` is
true. A drive that stopped between the node's finish and the decision asks on
resume; an execution already decided is never asked again. A failed source
asks nothing. Judge edges go to the decision models only, never to emulation.

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
   `<config_dir>/decisions-thresholds.yaml` (see "Stored thresholds" above;
   written by `apb decisions thresholds set`). The model must match exactly:
   a new model id never inherits.
4. At most `uses.<name>.max_actions` (default 3) automatic actions per use
   and run.

When a rule fails, the decision is journaled with `enforce_refused`
(`not_opted_in`, `uncalibrated`, `no_threshold`, `cap`, `effects` at a
gate, or `no_retry` for a completion check whose attempt has no
same-executor retry left to consume) and the use behaves as advise. An acting decision is journaled with
`applied: true` before anything acts on it, and a resumed run replays it
without a request, so the same path repeats, as long as the use is still in
enforce (after the kill switch or a lowered ceiling the replayed answer is
not acted on). Every path is fail-open except
the review auto-decision, which is fail-closed.

| Path | Opt-in | Acts when | Action |
|---|---|---|---|
| Completion | node `completion_check: enforce` | `final_result` below the stored threshold and `completion` is not `blocked_on_input` | the attempt fails with reason `completion check: partial (p=0.84)`, the reply kept as `rejected_output`, consuming a normal retry; a `blocked_on_input` answer only raises the advise anomaly |
| Retry advice | `defaults.retry_advice: enforce` | `next` is `switch_executor` (with a fallback left) or `stop_and_route_failure` at the stored threshold's confidence | `switch_executor` skips the remaining same-executor retries to the next fallback; `stop_and_route_failure` fails the node now; each is journaled as a `supervisor_action` `retry_advice` marker |
| Supervisor auto-retry | `supervisor: { pre_triage: enforce }` | `action` is `retry_same` or `retry_with_note` at the stored threshold and `looping` below `thresholds.looping_max` (0.3) | the engine posts the same `node_retry` command a supervisor sends, after a `supervisor_action` `triage_retry` marker; `retry_with_note` first appends the code-template note `Previous attempt failed with: <failure_kind>. ...` to the run context (never a quote of the output: a supervisor note outranks the node template for every later prompt); the wake is still raised, with `triage.applied: true` |
| Review auto-decision | gate `auto_decide: { allow: [needs_changes], min_confidence: 0.9 }` | the recommendation is an allowed option at the higher of the stored threshold and `min_confidence` | the option is posted through the review channel with note `auto: <provider>/<model> p=<p>` and journaled as an ordinary `review_decided`; revertible with `continue_from`; a decision a person posted first wins |
| Routing | node `route: auto` | the tier after hysteresis differs from the profile's executor tier at the stored threshold's confidence | the first attempt runs on that tier through the rebind overlay (`profile_rebound` with reason `routing: tier ...`); an agent failure on a tier below the executor goes up a tier at once (`fallback_triggered.reason: routing`) before the normal chain; a later execution routed back to the executor clears the overlay the same way |

The review auto-decision is refused by the validator (V73) on a playbook that
declares `irreversible` or `secrets` effects, or whose nodes after the gate
include a step that ships something out of reach of a later correction,
unless the gate sets `auto_decide_ok: true`. A node after the gate counts as
such a step when it declares node-level `effects: [irreversible]` (or
`secrets`), when it is granted a connector function its manifest flags
`irreversible: true`, or, as the fallback, when its id, title or script path
names a merge, push, deploy or publish. A node granted connector functions
that are merely not `read_only` gets a V73 warning naming them, not a
refusal. The inferred `external` effect
does not count (every playbook with an agent has it). The run re-checks the
same rule with the declared effects of every sub-playbook it runs, at any
depth and in any scope (resolved as the run gate resolves them), and refuses
with `enforce_refused: effects`; a sub-playbook tree that does not resolve is
refused the same way, and so is a gate inside a sub-playbook run, which cannot
see what its parent does after it returns. `allow` may contain only `needs_changes`,
never `approve` (V72).
