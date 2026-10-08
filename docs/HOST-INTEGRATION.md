# Host integration (tier 0)

APB gives the agent brief behavior rules through the MCP server's `instructions` field (tier 0, spec 4). The host model receives them at session start and learns that it has playbooks, when to offer saving one, and how to apply existing ones. The playbook catalog itself is pulled via the `playbook_catalog` tool rather than baked into the prompt - this keeps free text from the project out of privileged instructions (persistent prompt injection).

## Support for server instructions

MCP only guarantees the presence of the optional `instructions` field; how a host uses it, and whether it survives summarization, depends on the host. Tier-0 delivery therefore has to be confirmed per host.

Confirmed for Claude Code (measured on 2.1.x, July 2026, with controlled headless runs):

- The `instructions` text is injected at session start, but truncated at 2KB per server. The shipped tier-0 text is deliberately kept under that limit; if you extend it, re-check the byte count or the tail silently disappears.
- Tool descriptions are deferred by default (tool search): the model sees only tool names plus the server instructions until it decides to search. Behavioral rules must therefore live in the instructions, not in tool descriptions.
- Wording matters. Imperative rules that name the exact tool ("you MUST offer once to save it with playbook_capture") demonstrably fire; the same duty phrased softly does not.
- Instructions alone are not sufficient for the offer-to-save duty. They reliably trigger the offer only after the model has observed a repetition inside the session. For a task that is recurring by nature but performed once, and whenever a host-level skill takes over the work, the duty fires only when it is also present as a standing instruction in the project's memory files (see below).

Other hosts (opencode, Hermes, Pi) are still unverified; treat tier-0 delivery there as a hypothesis until checked the same way.

The measured delivery contract above is scoped twice over: per host and per MCP spec revision. What is confirmed for Claude Code holds for the pre-2026-07-28 revisions only, because it depends on the `instructions` field of the `initialize` response, and MCP spec revision 2026-07-28 moves the protocol to a stateless core that removes the `initialize` / `initialized` handshake. On that revision the delivery path is now known and already wired: apb runs on rmcp 3.0.1, which derives the new-revision `server/discover` response from the same `get_info` the old-revision `initialize` uses and copies its `instructions` verbatim, so tier-0 text reaches both channels from one source with no separate plumbing. What is still unmeasured is how a host that actually speaks 2026-07-28 consumes that `server/discover` channel: the 2KB truncation limit, the session-start injection, and the offer-to-save trigger were all measured on the old revision only and have to be re-measured for that host on that revision before they can be trusted again. No host ships the new revision yet, so that re-measurement stays open. The standing block that `apb init` writes into CLAUDE.md and AGENTS.md is protocol-independent and does not travel through the handshake, so it is the fallback channel that survives the transition and stays the guaranteed delivery path across revisions. See the protocol compatibility section in `docs/MCP.md` for the migration detail and the open host-gated items.

### Opt-in catalog ranking and tier 0

Decision-model catalog ranking (`playbook_catalog` with a `query`, see `docs/DECISIONS.md` "Catalog ranking" and `docs/MCP.md`) adds no tier-0 bytes and never changes the `instructions` text, whether it is enabled or not: the budget above is nearly spent, and a rule the host follows on every task would cost every user for a feature few enable. It is described in the tool's own description and input schema only. Since tool descriptions are deferred on Claude Code, a host passes `query` once it has loaded the tool's schema; without a `query` the catalog behaves exactly as before, so a host that never passes one loses nothing.

## Standing instruction in CLAUDE.md / AGENTS.md

`apb init` offers (a consent question in the interactive questionnaire) to append a standing playbook section to the project's `CLAUDE.md` and `AGENTS.md`. The write is idempotent (marker `## apb playbooks`), coexists with the feedback-loop section, and a non-TTY `apb init` never writes it. This is the guaranteed delivery path: memory files survive tool-search deferral, skill competition and instruction truncation, and in controlled runs this block is what makes the proactive save offer fire on a first-time task that is recurring by nature.

The canonical text lives in `crates/apb-cli/assets/playbook-instructions.md` and is what init writes. For a host without `apb init`, or for an agent's global config, paste that file's content verbatim; do not fork the wording, so there is a single text to keep current.

The block intentionally duplicates the proactive duties from tier 0 and nothing else: the catalog check before acting, the offer-to-save after, the semantic check against `suppressed_suggestions` before offering, and how a decline is recorded with `suggestion_dismiss` (kind soft by default, kind hard only for an explicit never-again). Run policy, gates and authoring rules stay in the server instructions, so the memory-file section does not go stale when those evolve.

## Node output and host Stop hooks (`outputs.extract`)

By default the engine persists an `agent_task` node's output as the agent's final message with the trailing yaml report block stripped. On the acp / stream-json transport (claude-code) the final message is the terminal `result` event, and that is the LAST thing the agent said. A host that runs a Stop hook or guardrail (for example an Open Second Brain Stop hook) can inject extra assistant turns AFTER the work is finished, so the final message becomes hook bookkeeping like "Nothing to log." instead of the work product. With last-message-wins output that bookkeeping becomes the node output, and everything downstream that reads it - `{{nodes.X.output}}` templating, `output_match` edge conditions, and run reports - gets the wrong text.

The `outputs.extract` contract makes the persisted output the work product regardless of trailing host turns. Set `outputs.extract: <marker>` on the node and have the node prompt instruct the agent to wrap its final work product in `<marker>...</marker>` (the marker value is a tag name, for example `node_output`, so the agent emits `<node_output>...</node_output>`). The engine then takes the content of the LAST `<marker>...</marker>` block the agent emitted anywhere in its turns as the node output. On the stream transport it scans the assistant prose across all turns first and falls back to the terminal result text; on the headless transport it scans the whole process stdout. When a marker match is used it overrides only the `output` field; node status, the one-line summary, session capture, and interactive question handling still come from the report block exactly as before.

The contract is opt-in: a node without `outputs.extract` keeps the existing last-message-with-report-block-stripped behavior byte-for-byte, and if the marker is set but no `<marker>...</marker>` block is found the engine falls back to that same default. The field is honored only on the `outputs` of `agent_task` nodes.

Example node:

```yaml
- id: summarize
  type: agent_task
  prompt: |
    Summarize the changes. Wrap ONLY the final summary you want persisted in
    <node_output> and </node_output> tags, and put nothing else inside them.
  outputs:
    extract: node_output
```

## Host execution mode (running agent steps with the host's own subagents)

By default apb executes every agent step by spawning the agent CLI the node's profile names (`claude`, `codex`, `zcode`, ...). Host execution mode (informally "mono-agent" mode) turns that around: apb spawns no agent CLI at all, and the agent that started the run executes every agent step with its own subagents. Nothing in apb detects or special-cases a host by name; the mode works the same in any MCP host that can run subagents (Claude Code, ZCode, opencode, and so on).

### When an agent picks it

`cli` is always the default, and there is no machine or dashboard switch that turns host mode on. The host agent passes `execution: "host"` on `playbook_run`, per run, only when the person explicitly asks for it: "mono", "host" or "single-agent" mode, or "run it with your own subagents". The agent never picks host mode on its own. A person may ask because apb spawning a second copy of an agent's CLI does not suit their setup, for example a subscription-bound desktop agent whose second CLI process would run outside the desktop session and lose the subscription context the person is paying for.

Besides that explicit request, a step becomes a host task only through the automatic fallback below, when no CLI of the step can start at all.

The rule is stated in the `playbook_run` tool description and in `playbook_howto`, which is where a host agent looks when it runs a playbook; the tier-0 instructions do not grow (their byte budget is spent, see above).

A second, automatic path exists for runs a host session starts in the background (`background: true` or `supervise: "self"`): when none of a step's CLIs can start at all (the binary is missing, or it is not logged in), that single step is handed to the host as a host task and the journal records `execution_fallback`. Ordinary agent failures (a model turn that fails, a timeout, a bad report, or an auth error printed after a model turn) never fall back; they follow the normal retry policy. A step's output that came from the fallback is never stored in the node cache, which is keyed by the CLI agent. `execution: { fallback_to_host: false }` in the global `config.yaml` or the project `.apb/config.yaml` turns the fallback off; a project can never turn host mode or the fallback on. `APB_EXECUTION=cli` in a process's environment turns off both host mode and the fallback for that process. It is never silent: a run that asked for `execution: host` under it starts as a `cli` run (still in the background on MCP), and every start response carries the `execution` block with `execution_notes` saying "execution: host was requested but APB_EXECUTION=cli forces cli"; `apb run --execution host` prints the same note to stderr.

### The execution contract

Every host task comes with one fixed contract, rendered by the engine (`apb_engine::host_task::EXECUTION_CONTRACT`) and repeated verbatim in `run_wait`'s and `supervisor_wait_event`'s `next` instruction, in the `run_wait` and `run_task_submit` tool descriptions, in each pending task's `execution_note`, at the top of `apb tasks`, and in the dashboard's host task panel:

> Execute this task in the current session with your own subagent tool, or do it yourself. Do not launch another agent CLI or process to run it (such as claude -p, codex exec, opencode run, gemini or a ZCode CLI), and never with permission-bypass flags. The profile's executor agent and model do not apply in host mode, and model_hint is advisory only: choose among the models your own subagent tool offers, use your session's model when none matches, and report the model actually used as run_task_submit model (apb tasks submit --model). Pass the prompt to your subagent by path (tell it to read prompt_path, and role_path as its role) instead of copying it. Bounded decisions inside the task (choose, rank, filter, map, yes/no) may be asked of the configured decision providers with `apb decide` or the MCP tool `decision_ask`; that is not running the task elsewhere.

The point of host mode is that the work stays inside the host session: its permission system, its accounting, its subscription. A host that shells out to an agent CLI for a task recreates the external executor host mode removes, outside those controls. `hint_note` opens with `advisory:` for the same reason: a declared model is never an order.

### The protocol

1. `playbook_run { id, execution: "host", ... }` starts the run in the background (a blocking call could not serve the tasks) and answers `run_id`, `execution: { mode: "host" }` and a `next` hint.
2. `run_wait { run_id }` returns `reason: needs_input`, `needs: host_task` and `pending_tasks`: one entry per agent step waiting for the host, each with `task_id`, `node`, `attempt`, the full `prompt` (the rendered node prompt with apb's report contract appended), `role_prompt` (the profile's SOUL), `prompt_path` and `role_path` (the absolute paths of the same two texts on disk, `runs/<id>/tasks/<task_id>/prompt.md` and `role.md`; `role_path` is null without a role), `skills` (paths of the skill copies materialized from the run snapshot), `workdir`, `env` (`APB_RUN_DIR`, `APB_RUN_ID`, `APB_NODE_ID`, `APB_STATUS_FILE`: set them for the subagent so `apb connector call`, the status file and commit provenance work), `outputs` (the node's declared outputs contract), `deadline` (from the node's `timeout_seconds`), `model_hint` (the model the profile declares for this chain step: a fallback entry or a routed tier; the profile's own executor gives none; optional to honor, and never a record of what ran), `hint_source` (which chain step the task is: `{ kind: "primary", profile }`, `{ kind: "fallback", index, of, profile }`, `{ kind: "tier", tier, profile }`, or `{ kind: "host_fallback", profile }` for the host step of a `cli` run whose CLIs could not start), `fallback_of` (on the first task of a later step: `{ attempt, reason }`, the attempt that closed the previous step and how, such as `failed`, `expired` or `question_timeout`) `hint_note` (the same as one English line, for example `advisory: model hint M (fallback 1 of 1 declared by profile P after attempt 1 failed; the host picks its own model)`) and `execution_note` (the execution contract above, verbatim).
3. For each task the host runs a subagent through its own subagent tool, in the current session (see [the execution contract](#the-execution-contract)), working in `workdir`: the host tells the subagent to read `prompt_path` as its task and `role_path` as its system context instead of copying the texts into the call (a ~4k-token prompt re-typed by the host model costs about a minute per node). A host whose subagents cannot read files passes `role_prompt` and `prompt` inline. `run_wait` and `supervisor_wait_event` take `inline_prompt` (default `true`, for compatibility); a host that reads files passes `inline_prompt: false` and gets each task without the inline `prompt` and `role_prompt`. Independent tasks may run concurrently (parallel branches expose several tasks at once, bounded by `max_parallel`).
4. `run_task_submit { run_id, task_id, status, output, usage?, note?, model? }` hands the subagent's final reply back verbatim. `status` is `succeeded`, `failed` (the run applies its retry policy; a retry or a fallback is a new task) or `blocked` (the subagent needs the person: `output` is the question; the run parks on it like on an interactive question and `run_answer` continues it with a follow-up task that carries the question and the answer). `model` is the model the subagent actually ran on, when the host knows it; it is journaled on `host_task_submitted` as reported (`apb tasks submit --model` does the same).
5. `run_wait` again, until `reason: finished`.

The engine treats a submission exactly like a finished CLI attempt: the report block is parsed (a missing block reads as success), the status file (if the subagent wrote it) and `success_check` decide, `require_verdict` takes the submission as the verdict, the completion check and judge nodes run in the engine, and retries, fallbacks (the profile's chain engages in host mode too: an expired deadline or an unanswered question advances to the next fallback entry or tier at once, while a `failed` submission first spends the node's retries on the same step; each step is a new task whose `host_task_requested` event carries `hint_source`, `fallback_of` and `hint_note`, so the journal says why it exists while it waits), loops, gates, sub-playbooks (they inherit the mode, and their tasks show up on the parent run; each task carries its own `run_id`, and a submission through the parent's id is refused as ambiguous when the same task id is pending on more than one of those runs, so pass the task's `run_id`) and resume work unchanged. A task that is not submitted by its deadline fails its attempt (`host_task_timeout`). A resume after a driver died re-exposes the same task (the node's latest open task under its own id, even when the resumed prompt differs, for example by an interruption note), and a submission that landed meanwhile is consumed without a new task. Any other task of that node still open is closed by the engine with status `superseded`.

A supervising session (`supervise: "self"`) also gets each request from `supervisor_wait_event` (`reason: host_task`, with `pending_tasks` and the same `next` instruction).

### Decisions inside a host task

A host task that needs a small bounded decision (pick one of N, rank, filter, map items to options, yes or no, a score) can ask the decision providers configured in `decisions.yaml` instead of spending a full model turn on it. This is sanctioned by the contract: a decision call is not an agent run. Ask through apb's decision layer only, never through an arbitrary agent CLI:

- `apb decide <kind> "<question>" [--option X]... [--item Y]... [--criteria "..."]`, with `kind` one of `choose`, `rank`, `filter`, `map`, `is`, `score`. Inside a task the run comes from `APB_RUN_ID` (and the node from `APB_NODE_ID`), both in the task's `env`. Prints one JSON line; exits 0 when answered, 1 when refused or failed, 2 on a malformed request.
- The MCP tool `decision_ask { kind, question, options?, items?, criteria?, run_id?, node_id? }`, for a host that drives the run itself.

The answer carries `answered: true`, the answer (`answer` and `p` or `confidence`; `items`, `kept` for the item kinds), the provider, model, latency and cost. With a run id the decision is journaled in that run as `decision_made` with `use_site: host_task`, the node and attempt, the provider, cost and latency, and the run's decision budget and redaction apply; the same question asked twice in one attempt is answered from the journal. A refusal is `answered: false` with `refused` and a `reason`: `off` (`APB_DECISIONS=off`), `no_provider`, `use_off` (`uses.host_task: { mode: off }` in `decisions.yaml`, or the project's narrowing), `playbook_off` (the playbook sets `defaults.host_decisions: off`), `budget`, `privacy` (`privacy.send` excludes prompts) or `run_ended`. An answer whose run ended while the provider answered carries `not_journaled` and is not in the journal. The task then decides on its own. See `docs/DECISIONS.md`.

### The model actually used

In host mode the profile's model is advisory, so the model a task ran on is the one the host reports (`run_task_submit model`). `run_status` carries `attempt_models` (per attempt: `node`, `attempt`, `executed_by` `cli` or `host`, the actual `model`, the profile's primary `expected` model and `mismatch`) and `model_mismatch`; `apb runs <run>` prints a `model` line per attempt, the dashboard's run page lists them in a Models panel, `apb stats` counts the models per node and version, and `apb doctor --run <run>` warns on each mismatch.

### Trust and attribution

Host mode changes who executes, not what is allowed: the run gate (trusted playbook and profiles, effects consent, `irreversible` confirmation) is unchanged. A host-mode start of a playbook whose tree declares `irreversible` needs the same consent as a `cli` one: the refusal names the sources and a `consent_nonce`, the host asks the person and passes `confirm_irreversible: "<consent_nonce>"` (`acknowledge_untrusted` is for trust only; a trust refusal of such a playbook names both, so one question covers them), the manifest records `consent: { irreversible: true, by: "mcp:<client>" }`, and the sub-playbooks whose steps come back as host tasks inherit it. The host executing a task never grants a consent: a host task cannot start an irreversible sub-playbook the run was not consented for, and a supervisor patch cannot add irreversible steps. The host fallback does not change it either: a step that falls back to the host runs under the consent the run started with. The prompt is rendered from the trusted snapshot exactly as for a CLI attempt, and the host never receives a connector secret (connector calls still go through `apb connector call`). Every submission is journaled as `host_task_submitted` with `submitted_by: host` and the MCP client name (`clientInfo.name`); the engine's own closures of a task (expired, cancelled, interrupted) say `submitted_by: engine`.

### Other surfaces

- CLI: `apb run <id> --execution host` (the scripted hand-off: `apb tasks [run]` lists the tasks, `apb tasks submit <run> <task_id> --status succeeded --output-file <file>` answers one).
- `apb doctor` and `playbook_adopt_report` state the default and whether the fallback is on; `run_status` carries the run's `execution`, and `apb runs <run>` prints it as an `execution:` line (`--json` gives the `run_status` object).
- The dashboard's run page shows a read-only host mode badge, the pending host tasks with their prompts, and the request and submission rows in its timeline.
- `docs/PROFILES.md` lists what host mode ignores in a profile.

