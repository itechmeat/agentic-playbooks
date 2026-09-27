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
