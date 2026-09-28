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
| `judge_node` | the questions a playbook's `judge` node declares | `off`, `shadow`, `advise` (journal only), `enforce` (routes) |
| `judge_edge` | the yes/no question of a `judge` edge condition | `off`, `shadow`, `advise` (journal only), `enforce` (routes) |

Shadow means journal only: the answer is recorded in the run's journal and
nothing acts on it. The `judge` node and edge are declared by a playbook and
route only at `enforce`; below it they take their declared fallback (see their
sections below). Advise mode for the engine's own uses and the other uses come
later, once shadow data exists to set their thresholds.

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
(`systemone`, `fake`), never to an `llm_emulation` provider; those are used
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
decision.

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

