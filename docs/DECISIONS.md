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

One use exists, in shadow mode only:

| Use | What it asks | Modes available |
|---|---|---|
| `completion_check` | whether a successful agent reply is a finished result rather than a progress note, a plan or a question back | `off`, `shadow` |

Shadow means journal only: the answer is recorded in the run's journal and
nothing acts on it. Advise mode (showing a flag where a person or supervisor
decides), the `judge` node and edge, and the other uses come later, once shadow
data exists to set their thresholds.

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

Decision outputs are used for evaluation and threshold tuning only
(`apb decisions report` and `apb decisions replay`, below). apb has no
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
`--all-projects`); it asks no model and writes nothing. Per use and
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
| `completion_check` | acting was right when, before the node starts again, a supervisor retried it, the run was moved back to it or to a node that ran before it (`run_continue_from`, a patch or migration `continue_from`), or the next node to start after it failed; wrong when the next node succeeded, or the node was the last and the run succeeded. Otherwise unlabelled |
| other uses | not labelled yet: each gets its labeller once its events journal the join key (attempt, gate visit or wake) |

Unlabelled decisions stay out of every accuracy figure and are listed with the
reason.

**Eligibility** (the rule the enforce modes apply): a threshold stored for
exactly this provider and model, at least 50 labelled decisions (20 per
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
debug file, already redacted and clipped. It refuses without `--provider`,
never writes a journal or anything under a run, and saves its results under
`<config_dir>/decisions-replay/`.

Replay is evaluation only, like threshold tuning: it compares answers against
APB's own labels. It is not an export of outputs for training, and apb has no
such feature (see the provider terms note under "What is sent").
