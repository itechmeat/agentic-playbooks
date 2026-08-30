# paperclip-apb-adapter

A Paperclip **external server adapter** (`type: "apb"`) that dispatches Paperclip
agent work to a locally running **apb** engine ([agentic-playbooks](https://github.com/itechmeat/agentic-playbooks)).

When a Paperclip agent configured with `adapterType: "apb"` wakes, this adapter
picks a playbook, starts an apb run over apb's HTTP API, streams the apb run
journal back into the Paperclip run log, and maps the terminal apb run state onto
a Paperclip execution result.

Verified against **apb 0.20.2** and **paperclipai 2026.824.1** on Node 24.

The integration overview, prerequisites and scope live in the recipe:
[docs/integrations/paperclip.md](../../../docs/integrations/paperclip.md).

---

## Architecture

```
Paperclip heartbeat
      │  execute(ctx)
      ▼
┌──────────────────────────┐
│ paperclip-apb-adapter    │
│  resolve.js  → playbook  │        apb engine (127.0.0.1:7321)
│  apb-client.js           │  POST /api/playbooks/{id}/run?workspace=…
│                          │ ─────────────────────────────────────────▶ run_id
│  poll loop               │  GET  /api/runs/{run_id}?workspace=…
│    ├─ onLog(...)         │ ◀───────────────────────────────────────── run detail
│    └─ onEvent(...)       │        (run_status, events[], nodes, outputs, answer)
└──────────────────────────┘
      │  AdapterExecutionResult { exitCode, summary, sessionParams, resultJson }
      ▼
Paperclip run record
```

Plain ESM, Node ≥ 24, **no build step and no runtime dependencies** - only Node
built-ins and global `fetch`. (The bundled `@paperclipai/hermes-paperclip-adapter`
ships compiled JS from TypeScript; the loader only ever imports the resolved
entry file, so shipping hand-written ESM is equally valid.)

### Why polling, not the WebSocket

apb exposes `GET /api/ws`, but it is a one-way broadcast carrying only two
contentless change pings - `{"type":"runs_changed"}` and
`{"type":"playbooks_changed"}`. There is no run id, no payload, and no subscribe
protocol, so a client must refetch anyway. apb also has **no incremental log or
SSE endpoint**: the run journal is the `events` array embedded in the run detail
response. Polling that endpoint and emitting only newly-seen `seq` values is
therefore both simpler and strictly more reliable than adding a socket that
would only tell us to do the same fetch.

---

## apb API surface used

| Purpose | Call |
|---|---|
| Reachability | `GET /api/health` → `{"status":"ok"}` |
| Project → workspace id | `GET /api/projects` → `[{name, path, workspace_id, playbook_count}]` |
| Playbook existence | `GET /api/playbooks` → `[{id, current, versions, project, workspace_id, …}]` |
| Profile trust advisory | `GET /api/profiles?workspace=<ws>` → `{profiles:[{name, trusted, …}]}` |
| **Start a run** | `POST /api/playbooks/{id}/run?workspace=<ws>` body `{instruction?, params?, continued_from?}` → `{"run_id"}` |
| **Poll a run** | `GET /api/runs/{run_id}?workspace=<ws>` → `{run_status, events[], nodes, outputs, answer, failure_reason, progress, …}` |

Two apb quirks the client defends against:

1. **`params` is typed `BTreeMap<String,String>` server-side** - every value must
   be a string. Non-string values are JSON-stringified before being sent.
2. **Unknown `/api/*` paths return HTTP 200 with the dashboard SPA HTML**, because
   the router ends in `.fallback(static_handler)`. apb 0.20.2 serves that body as
   `application/octet-stream` - *not* `text/html` - so a content-type test alone
   never fires; the client sniffs the `<!doctype html>` prefix. It deliberately
   does not reject on `application/octet-stream` alone, so a valid JSON body with
   a sloppy content-type still parses.

`GET /api/playbooks` ignores a `project` query parameter and always returns every
reachable workspace's playbooks, so filtering happens client-side on `workspace_id`.

---

## Configuration reference

Exposed through `getConfigSchema()`, so the Paperclip agent form renders these
fields automatically. Note that Paperclip does **not** validate `adapterConfig`
against this schema - it is form metadata and secret-field discovery only - so
every value is defensively coerced at run time. Textarea fields accept either a
real object (API) or a JSON string (UI); malformed JSON degrades to unset rather
than throwing.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `apbBaseUrl` | text | `http://127.0.0.1:7321` | apb API base URL. |
| `apbApiKey` | text (secret) | – | Only needed when apb server mode has issued keys. Sent as `Authorization: Bearer`. |
| `project` | text (**required**) | *none* | apb project name from `apb projects list`; resolved to a `workspace_id` per run. There is deliberately **no default** - see below. |
| `playbook` | text | – | Default playbook id. |
| `playbookMap` | textarea (JSON) | – | `taskKey` / issue identifier / issue id / `wakeReason` → playbook id. Supports `PREFIX-*` globs and a `default` key. |
| `params` | textarea (JSON) | – | Default apb run params. |
| `instruction` | textarea | – | Default free-text instruction for the run. |
| `timeoutMs` | number | `900000` | How long to wait for a terminal apb state. |
| `pollIntervalMs` | number | `2000` | Run-detail poll cadence (floor 250 ms). |
| `pollGiveUpMs` | number | `60000` | How long apb may stay continuously unreachable mid-run before the adapter stops waiting. |
| `onPause` | select | `return` | `return` = hand control back on a human gate; `wait` = keep polling until timeout. |
| `streamNodeOutput` | toggle | `true` | Include apb node output in logs, event payloads and the summary. |
| `allowTextDirectives` | toggle | `false` | **Security-sensitive.** Honour `apb:` directives found in issue text. |
| `logParamValues` | toggle | `false` | Log param values as well as keys (secretish keys stay masked). |

**`project` has no default.** It previously defaulted to the name of a live
business project, so a blank or mistyped `project` silently dispatched real
playbooks. A missing project is now a hard configuration error (exit `78`).

`apbApiKey` is declared `meta: { secret: true }`, which is the only way an
external adapter gets first-class Paperclip secret handling - a plain text field
would be stored in cleartext on the agent row. Credentials embedded in
`apbBaseUrl` userinfo (`http://user:pass@host`) are stripped at construction and
resent as a `Basic` header: Node's `fetch` refuses a credentialed URL outright,
and its error text quotes the password into any log that records the failure.

---

## The Paperclip context contract

An adapter only sees what the wake puts on `ctx`. Verified against the installed
`@paperclipai/server` bundle (`dist/services/heartbeat.js`), the free text a wake
carries lives at exactly these paths:

| Path | What it is |
|---|---|
| `context.paperclipWake.issue.{title,description,identifier,status,workMode}` | the issue behind the wake |
| `context.paperclipWake.agentMessage.text` | a message addressed to the agent |
| `context.paperclipIssue.{title,description,identifier,workMode}` | same issue, flatter shape |
| `context.paperclipTaskMarkdown` / `…Compact` | the rendered task brief |
| `context.paperclipWakeComment` | the comment that triggered the wake |
| `context.taskId`, `context.issueId`, `context.wakeReason` | scalars |
| **`ctx.runtime.taskKey`** | the task key - **not** on the context bag |

There is no `context.taskTitle` / `taskBody` / `context.task`, no
`context.issueIds` array, and no `context.wakeSource`. An earlier version of this
adapter scanned those invented names, which meant that on every ordinary wake the
`apb:playbook` directive was inert, the provenance params were empty, and the
instruction collapsed to a bare `wakeReason`.

---

## Playbook-resolution convention

First match wins:

1. `runtime.sessionParams.apbPlaybook` - per-session pin carried by a previous run.
2. `context.apb.playbook` / `context.apbPlaybook` - structured hint from a
   programmatic caller. Paperclip itself never sets these.
3. **`apb:playbook=<id>` directive** in the wake text listed above - only when
   `allowTextDirectives: true`. See the security note below.
4. `adapterConfig.playbookMap` - looked up against `runtime.taskKey`, then the
   issue identifier, then the issue id, then `wakeReason`; exact keys first, then
   `PREFIX-*` globs, then `default`. A non-string map value is skipped **with a
   warning** rather than silently.
5. `adapterConfig.playbook`.
6. Otherwise the run fails fast with exit code `78` / `APB_NO_PLAYBOOK`.

### Security: text directives are opt-in and default OFF

The text those directives are read from is verbatim issue content - any user who
can file or comment on an issue writes it. Honouring `apb:playbook=` and
`apb:param.<k>=` from it lets an issue author choose **which playbook runs and
with which parameters**: a direct injection channel into the automation engine.
So it is gated behind `allowTextDirectives` (default `false`), and even when
enabled the adapter enforces three limits:

- **The separator is mandatory.** An earlier optional-separator pattern read
  prose like "we love apb:playbooks" as playbook `s`.
- **A directive can never overwrite an operator-configured param.**
- **A directive can never write a `paperclip_*` provenance key**, so run/agent/
  issue provenance cannot be spoofed.

Each refusal is logged rather than silently dropped, and `testEnvironment` raises
a warning whenever the flag is on.

### Parameters

Merged in ascending priority: `apb:param` directives (when enabled, and only for
keys nobody else claimed) < `context.apb.params` < `adapterConfig.params`.
Directive values may be quoted: `apb:param.note="two words"`.

The adapter then injects a provenance block - `paperclip_run_id`,
`paperclip_agent_id`, `paperclip_company_id`, `paperclip_task_id`,
`paperclip_task_key`, `paperclip_wake_reason`, `paperclip_issue_id`,
`paperclip_issue_key` - so a playbook can correlate its run back to the Paperclip
work item. Injected keys never overwrite an operator-supplied key of the same
name. apb accepts undeclared params, so a playbook need not declare these.

Param **values are not logged** by default (`logParamValues`), because they
routinely carry customer data lifted from the issue; keys are always logged, and
secretish key names stay masked even when values are enabled.

### Instruction

`context.apb.instruction` > `adapterConfig.instruction` > the real wake text
above > a generated one-liner naming the task key and Paperclip run id.
`wakeReason` is deliberately excluded - it is an internal state token such as
`finish_successful_run_handoff`, meaningless to a playbook, and it travels as the
`paperclip_wake_reason` param instead.

---

## Re-attaching to a live run

`sessionParams` carries `apbRunId`, `apbPlaybook`, `apbProject` and
`apbWorkspaceId`. On each wake the adapter **probes the prior run first**: if it
is still live, it adopts it and streams it to completion instead of starting a
new one.

This matters because every path that can return while an apb run is still going -
timeout, pause-return, poll give-up - leaves that run alive. Without re-attach,
the next wake started a second run on top of it, which is how one issue
assignment turned into three concurrent apb runs.

---

## Result mapping

| apb `run_status` | exit code | notes |
|---|---|---|
| `succeeded` | `0` | |
| `failed` | `1` | `failure_reason` becomes the summary |
| `aborted` | `130` | operator/engine stopped the run |
| `interrupted` | `137` | the run driver process is provably gone |
| `paused` (with `onPause: "return"`) | `75` | resumable; the next wake re-attaches |

Pre-flight failures: `69` apb unreachable, `77` connector-trust refusal,
`78` missing project / unknown playbook / unresolvable playbook, `75` workdir busy.

> **These codes are diagnostic, not behavioural.** Paperclip's only success test
> is `(exitCode ?? 0) === 0 && !errorMessage` - no numeric code is special-cased.
> Every non-zero code above is recorded as a **failed** run, exit `75` included;
> the specific number and `errorCode` exist for whoever reads `resultJson` and
> the logs. The adapter always sets `errorMessage` on a non-zero exit, because
> Paperclip renders a generic "Adapter failed" without one.

`summary` is apb's `answer` when the playbook has a finish prompt; otherwise the
output of the last node to finish **in event order** (`detail.outputs` is a
serialized `BTreeMap` in alphabetical key order, so its "last" entry is an
arbitrary node); otherwise a status line.

`resultJson` always carries `apbRunId`, `apbPlaybook`, `apbProject` and
`apbWorkspaceId` - on **every** path that got as far as starting or adopting a
run, including timeout, pause and give-up, where it also sets `stillLive: true`.
A terminal run adds `apbRunStatus`, `apbPlaybookVersion`, `failureReason`,
`nodes` and `answer`. Paths that fail before a run exists (bad config,
unreachable engine) carry no `resultJson`.

`sessionDisplayId` is the apb run id, so it shows up in Paperclip's session
column. The session codec passes the server's own `__paperclip*` metadata keys
through untouched: Paperclip deserializes **before** stripping that metadata and
re-attaches it after `serialize`, so an allowlist codec would destroy
model-change reset and config-freshness detection.

`usageBasis` is set to `"per_run"` deliberately. Leaving it unset makes Paperclip
apply a legacy session-delta heuristic that subtracts the previous run's totals
whenever a session id is persisted - which would silently corrupt accounting.
apb reports no token usage to this adapter, so no `usage` object is emitted.

---

## Install / registration

The adapter is a **local-path** plugin; nothing is published to npm.

### Option A - hand-register the plugin record (used here)

`POST /api/adapters/install` requires instance-admin authentication. On an
instance whose board has not yet been claimed there is no way to obtain that
token without consuming the owner's one-time claim URL, so this install uses the
documented file-based path instead.

Write `~/.paperclip/adapter-plugins.json` (a top-level array, keyed by `type`;
this file lives at `$PAPERCLIP_HOME`, not under the instance directory):

```json
[
  {
    "packageName": "paperclip-apb-adapter",
    "localPath": "/absolute/path/to/paperclip-apb-adapter",
    "version": "0.1.0",
    "type": "apb",
    "installedAt": "2026-08-30T05:30:00.000Z"
  }
]
```

then restart Paperclip:

```sh
sudo systemctl restart paperclipai.service
journalctl -u paperclipai.service -n 50 | grep -i 'external adapter'
# INFO: Loading external adapter package {"packageName":"paperclip-apb-adapter",…}
# INFO: Loaded external adapters from plugin store {"count":1,"adapters":["apb"]}
```

### Option B - CLI / API (needs a board-authenticated instance admin)

```sh
paperclipai adapter install --payload-json \
  '{"packageName":"/absolute/path/to/paperclip-apb-adapter","isLocalPath":true}'
paperclipai adapter list
paperclipai adapter config-schema apb

# Always pass the config you actually want tested - see the pitfall below.
paperclipai adapter test-environment apb -C <companyId> --payload-json \
  '{"adapterConfig":{"project":"test-fixture","playbook":"apb-noop"}}'
```

> **Pitfall - `test-environment` without a payload tests the adapter's
> *defaults*, not your agent.** `--payload-json` defaults to `{}`. Since
> `project` now has no default, that returns a `fail` naming the missing project
> rather than quietly probing some other project - but it still tells you nothing
> about whether a particular agent's config works. Always pass
> `--payload-json '{"adapterConfig":{…}}'` carrying the same config the agent has.

### Dev loop

`paperclipai adapter reload apb` cache-busts **only the entry file**
(`src/index.js`). Changes to `apb-client.js` / `resolve.js` / `config-schema.js`
stay cached - restart the service for those. Local-path adapters cannot be
`reinstall`ed (400); use reload or restart.

---

## Issue assignment semantics

> **Drive apb agents with explicit wakeups or routines - not by assigning an
> issue.** One issue assignment currently produces **three** real apb runs and
> leaves the issue **blocked**.

Observed empirically on issues `SWA-2` and `SWA-3`. The three wakes arrive with
these `wakeReason` values, in order:
`issue_status_changed` → `finish_successful_run_handoff` → `source_scoped_recovery_action`.

1. Assigning the issue to an apb agent fires a wake; the issue moves to
   `in_progress`. The adapter dispatches the playbook and the apb run succeeds
   (exit 0).
2. Because this adapter **never sets an issue disposition**, Paperclip's recovery
   machinery does not see the issue resolved. It fires a corrective run
   (`successful_run_handoff_required`) - a second apb run.
3. That run also sets no disposition, so Paperclip escalates - a third apb run -
   and then parks the issue at **`blocked`**.

Net: **3× run amplification and a blocked issue** from a single assignment. The
apb work itself succeeds every time; what is missing is the Paperclip-side
acknowledgement that closes the loop.

### What re-attach does and does not fix

[Re-attaching to a live run](#re-attaching-to-a-live-run) removes one source of
amplification: a wake that returns while its apb run is still going (timeout,
pause, poll give-up) no longer leaves an orphan for the next wake to run
*alongside*. That was the case where two apb runs executed concurrently.

It does **not** fix the cascade above. There the first run has already finished
before the corrective wake arrives, so there is nothing live to adopt and a new
run legitimately starts. The corrective runs are driven by the missing issue
disposition, not by orphaned runs, so the fix has to be a disposition - the
recommendation below stands unchanged.

Two ways to make issue assignment safe, neither implemented yet:

- **The playbook reports back.** Have the playbook call Paperclip's issue API
  (`PATCH /api/issues/{id}` with a `status` and `comment`) so the issue reaches a
  terminal disposition on its own.
- **The adapter grows an issue-disposition feature.** Have `execute()` set a
  disposition from the apb run outcome before returning. This needs a Paperclip
  credential in the adapter - deliberately out of scope for now, since the
  adapter currently sets `supportsLocalAgentJwt: false` and never calls back into
  Paperclip.

Until one of those lands, use `POST /api/agents/:id/wakeup` or a routine.

**Cosmetic:** `external_run_id` stays `NULL` on the Paperclip run row. The apb run
id is still recorded - in `resultJson.apbRunId` and `sessionParams.apbRunId`, and
as `sessionDisplayId` - so correlation works; the dedicated column just is not
populated by this path.

---

## Limitations and gotchas

**apb has no HTTP stop endpoint.** Stopping a run is CLI/MCP only. On timeout the
adapter returns `timedOut: true` and says so explicitly - **the apb run keeps
going** and must be stopped with `apb stop <run-id>` if unwanted. The adapter
never leaves a silent orphan, but it cannot cancel one either.

**Connector trust (`trusted: false`) cannot be acknowledged over HTTP.** A
playbook that binds connectors through an untrusted profile is refused at start
with HTTP 409 and a `untrusted_connector_requires_approve` /
`unapproved_connector_account` body. apb's `acknowledge` argument exists **only on
the MCP tool**, not on `POST /api/playbooks/{id}/run`, so this adapter surfaces
the refusal as exit code `77` with the payload rather than pretending it can
consent. Approve out of band (`apb connector`, or `POST /api/connectors/approve`)
or mark the profile trusted. `testEnvironment` raises a warning listing any
untrusted profiles so this is caught before a wake, not during one.

**No token usage or cost.** apb does not report per-run token counts through the
run API, so `usage`/`costUsd` are omitted rather than guessed. `model` is set to
the playbook id so the Paperclip UI shows what actually ran.

**No UI parser.** The package ships no `./ui-parser` export, so the Paperclip run
view renders the streamed log as plain text. Adding one would require a
dependency-free, non-ESM CJS file exporting `parseStdoutLine(line, ts)`.

**Unknown `adapterType` fails open.** Paperclip's `getServerAdapter()` falls back
to the generic `process` adapter for an unregistered type - a typo in an agent's
`adapterType` silently runs the wrong adapter instead of erroring. Confirm with
`paperclipai adapter list` after registering.

**Pause handling is a policy choice.** `onPause: "return"` ends the Paperclip run
at exit code `75` rather than holding a wake open for a human gate that may take
hours. The apb run stays parked and resumable, and the next wake **re-attaches**
to it via `sessionParams` rather than starting a second one. Set
`onPause: "wait"` to hold the wake open until the gate is decided or the timeout
expires.

**`streamNodeOutput: false` withholds node output, not the run answer.** With it
off, apb node outputs are stripped from the log stream, from `onEvent` payloads,
and from the summary fallback. apb's own `answer` - the value a finish-prompt
playbook composes as its deliberate result - is still returned in `summary` and
`resultJson.answer`, because suppressing that would leave the adapter with
nothing to report. If a playbook's *answer* is sensitive, do not surface it at
all: keep the finish node prompt-less so apb returns no answer.

### Board claim - resolved

> **Status: done.** The board has been claimed, the adapter is live, and the full
> Paperclip → apb wake path has been verified end to end in production
> (agent *APB Runner (test)*, Paperclip run → apb run `apb-noop-…`, exit 0, with
> the `paperclip_*` provenance params confirmed on the apb side).
> `paperclipai adapter list` reports:
> ```
> type=apb label=apb source=external loaded=true version=0.1.0
>   packageName=paperclip-apb-adapter isLocalPath=true
> ```

The steps below are kept as a reference for re-authenticating a CLI, or for
bringing up a **fresh** instance where the synthetic `local-board` is still the
only admin (`BOARD CLAIM REQUIRED` in the startup banner). Until such an instance
is claimed, everything behind `assertBoardOrgAccess` / `assertInstanceAdmin`
returns `403 Board access required`:

- `GET /api/adapters` and `paperclipai adapter list` - listing the registered adapter
- `GET /api/adapters/apb/config-schema`, `.../ui-parser.js`
- `POST /api/adapters/install`, `.../reload`, `.../reinstall`, `PATCH`, `DELETE`
- creating a company and an agent with `adapterType: "apb"`
- `POST /api/companies/:companyId/adapters/apb/test-environment`
- triggering a real Paperclip heartbeat wake that calls `execute()`

Registration and loading can be verified without any of these - the server log
plus a direct load through Paperclip's own `buildExternalAdapters()` - and
`execute()` / `testEnvironment()` can be exercised directly against live apb.

**Owner steps**, in order:

1. Sign in to the Paperclip UI (by default `http://127.0.0.1:3100`) as a real user.
2. Open the one-time board-claim URL printed in the `paperclipai.service`
   startup banner (`journalctl -u paperclipai.service | grep board-claim`).
   Treat it as a credential: it grants instance ownership exactly once.
3. Authenticate the CLI for board access:
   ```sh
   PAPERCLIP_INSTANCE_ID=default paperclipai auth login --instance-admin --no-browser
   ```
   Approve the printed URL in the browser. The board key is stored at
   `~/.paperclip/auth.json`.
4. `paperclipai adapter list` - confirm `apb` appears.
5. Create a company and an agent with `adapterType: "apb"` and an `adapterConfig`
   naming a `project` and a `playbook` (or `playbookMap`).
6. Test the agent's real config (not the defaults - see the pitfall above):
   ```sh
   paperclipai adapter test-environment apb -C <companyId> --payload-json \
     '{"adapterConfig":{"project":"test-fixture","playbook":"apb-noop"}}'
   ```
   Expect `pass`.
7. Trigger a wake - **via explicit wakeup or a routine, not by assigning an
   issue** (see *Issue assignment semantics* above) - and confirm the apb run
   appears in `apb runs`.

> The agent-API-key trap (`403 RESPONSIBLE_USER_UNAVAILABLE` for keys minted while
> `local-board` is the only admin) is unrelated to this adapter: `execute()` never
> calls back into the Paperclip API and sets `supportsLocalAgentJwt: false`, so it
> needs no agent key or run JWT. It resolves once step 2 is done.

---

## Testing

```sh
npm test          # offline only - no apb engine needed (57 tests)
npm run test:live # real apb runs against the throwaway fixture (6 tests)
npm run test:all  # both
```

Requires **Node ≥ 24** (matching the Paperclip host); the `node --test` glob
form used by these scripts needs Node 21+.

**Offline suites** (`test/resolve.test.js`, `test/adapter.test.js`) need no
engine. `ApbClient` takes an injectable `fetchImpl`, which lets the tests drive
the paths that are hard or destructive to reproduce live: the SPA fallback in
both content-types, a mid-body connection reset, a stalled body read, poll
give-up and transient-blip recovery, pause return-vs-wait, re-attach, event
dedup edge cases, output suppression, and the 404/409/429 error arms. Every
context fixture mirrors the real Paperclip contract documented above.

**Live suite** (`test/live-apb.test.js`) fires real apb runs, but only ever
against the throwaway `test-fixture/` project in this repo. Its single playbook
`apb-noop` is one deterministic `script` node (`sh scripts/noop.sh`) - no agent,
no LLM, no connectors, no network - so it can never reach a business playbook.
Tests skip themselves when apb is unreachable.

The fixture registers itself with apb implicitly: running any `apb` command
inside `test-fixture/` adds it to `~/.config/apb/projects.json`, and the running
engine picks it up live without a restart.

> The timeout test deliberately abandons a run it started. That run is the noop
> fixture, which finishes by itself in about a second, so it leaves nothing
> behind that needs stopping. The follow-on test then re-attaches to it, which is
> what proves a second wake does not double-fire.
