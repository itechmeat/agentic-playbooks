# Host integration (tier 0)

APB gives the agent brief behavior rules through the MCP server's
`instructions` field (tier 0, spec 4). The host model gets them in its system
prompt and learns that it has playbooks, when to offer saving one, and how to
apply existing ones. The playbook catalog itself is pulled via the
`playbook_catalog` tool rather than baked into the prompt - this keeps free
text from the project out of privileged instructions (persistent prompt
injection).

## Support for server instructions

MCP only guarantees the presence of the optional `instructions` field; how a
host uses it, and whether it survives summarization, depends on the host.
So tier-0 delivery is a hypothesis that must be confirmed on each target host.

The compatibility matrix has NOT been captured yet: it's a separate
compatibility spike (Task 0 of the plan), run manually by an operator
(connect the binary as an MCP server to Claude Code / opencode / Hermes / Pi
and check: is `instructions` read, where does it end up, does it survive
compaction, how are destructive-tool confirmations shown). Until then, don't
rely on tier-0 delivery via `instructions` - use the fallback below as the
guaranteed path.

## Fallback for clients without instructions

If the host ignores `instructions`, proactive behavior is not lost - the
catalog is still available via the tool. To give the agent starting rules
manually, paste the tier-0 text into the project's `CLAUDE.md` / `AGENTS.md`
or the agent's global config. APB does not write to these files itself: the
section would go stale, and modifying someone else's files automatically is
not the right thing to do.

Minimal fallback text:

> You have APB playbooks: saved, repeatable processes you manage for the user.
> Call `playbook_catalog` once per task that describes a doable action to see
> if a saved playbook fits. On a confident match, name the playbook in one line,
> but do not assume the run is allowed: the server reports gates and you must
> follow every one before running - applicability preflight (requires), trust
> and scope validation, effects confirmation for anything beyond what the
> request implies (network, secrets, irreversible), and any required
> preparation or approval (trial for drafts, the two-phase plan for another
> workspace). Only run once those gates pass. When you just did a multi-step
> repeatable action with no matching playbook, offer once to save it (project or
> global scope). Pull `playbook_howto` only when authoring. Speak to the user
> about playbooks in the language of their recent messages.
