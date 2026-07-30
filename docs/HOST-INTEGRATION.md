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

The measured delivery contract above is scoped twice over: per host and per MCP spec revision. What is confirmed for Claude Code holds for the pre-2026-07-28 revisions only, because it depends on the `instructions` field of the `initialize` response, and MCP spec revision 2026-07-28 moves the protocol to a stateless core that removes the `initialize` / `initialized` handshake. The day Claude Code (or any host) adopts the stateless core, the delivery path for server instructions changes (to `server/discover` or elsewhere, not yet known), so the 2KB truncation limit, the session-start injection, and the offer-to-save trigger all have to be re-measured for that host on that revision before they can be trusted again. The standing block that `apb init` writes into CLAUDE.md and AGENTS.md is protocol-independent and does not travel through the handshake, so it is the fallback channel that survives the transition and stays the guaranteed delivery path across revisions. See the protocol compatibility section in `docs/MCP.md` for the adoption plan and the Phase 1 trigger conditions.

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
