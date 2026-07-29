## apb playbooks (standing instruction)

This project uses apb playbooks: saved, repeatable processes managed through the agentic-playbooks MCP server. Two duties apply to every task, in addition to whatever skills or tools you use to perform the work itself.

Before acting on a task that describes a doable action, call playbook_catalog once to check whether a saved playbook already fits. Do not call it for chit-chat or clarifying replies. On a confident match to an active, trusted playbook, name it in one line and use it.

After finishing the work: if either (a) the action you just completed was multi-step and likely to be repeated, or (b) the user asked for an action that is recurring by nature, and no playbook matched, you MUST offer once to save it as a playbook with playbook_capture. Ask exactly one short question offering project or global scope with the recommended option first. Before offering, compare the candidate action against the catalog's suppressed_suggestions by the meaning of each record's synopsis, not by slug equality; when a record's synopsis is empty, match by its pattern slug instead. A record that already covers the action means no offer. At most one offer per session.

When the user declines an offer without saying never, record it with suggestion_dismiss using kind soft, project scope, and a one-sentence synopsis of the action; the server computes an escalating silence, so a repeated decline is honored longer. Reserve kind hard for an explicit never-again, and use global scope only when the user's own wording says everywhere. Do not ask an extra question about scope, and never put secret values into a synopsis.
