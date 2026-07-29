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

## Standing instruction in CLAUDE.md / AGENTS.md

`apb init` offers (a consent question in the interactive questionnaire) to append a standing playbook section to the project's `CLAUDE.md` and `AGENTS.md`. The write is idempotent (marker `## apb playbooks`), coexists with the feedback-loop section, and a non-TTY `apb init` never writes it. This is the guaranteed delivery path: memory files survive tool-search deferral, skill competition and instruction truncation, and in controlled runs this block is what makes the proactive save offer fire on a first-time task that is recurring by nature.

The canonical text lives in `crates/apb-cli/assets/playbook-instructions.md` and is what init writes. For a host without `apb init`, or for an agent's global config, paste that file's content verbatim; do not fork the wording, so there is a single text to keep current.

The block intentionally duplicates the proactive duties from tier 0 and nothing else: the catalog check before acting, the offer-to-save after, the semantic check against `suppressed_suggestions` before offering, and how a decline is recorded with `suggestion_dismiss` (kind soft by default, kind hard only for an explicit never-again). Run policy, gates and authoring rules stay in the server instructions, so the memory-file section does not go stale when those evolve.
