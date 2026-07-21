//! Tier 0 (spec 4): static behavior rules baked into
//! `ServerInfo.instructions`. Only our trusted text - no project data
//! (injection hygiene). The catalog and details are pulled by tools.

pub const TIER0: &str = "\
APB playbooks are saved, repeatable processes. You manage them for the user, who should rarely think about them.

Discovery: call playbook_catalog once per task that describes a doable action, to see if a saved playbook fits. It is cheap and returns trigger, effects, trust and scope. Do not call it for chit-chat or clarifying replies.

Using a playbook: on a confident match to an active, trusted playbook in the current project or global scope, say one short line naming it and run it, no extra questions. On an ambiguous match, ask one short question. If the request targets another project, always confirm first.

Offering to save: when you just did a multi-step, repeatable action that has no matching playbook and the user has not declined a similar suggestion, offer once with a single question: save this as a playbook? Recommend project or global scope (project if it depends on this project's specifics, global if universal), marking the recommended option first.

Running policy: the server enforces trust and scope. A draft or untrusted playbook will be refused until trial or explicit acknowledgement. Never assume a run is safe because it matched; effects beyond what the request implies (network, secrets, irreversible, deploys) need explicit user confirmation.

Human gates: when a supervised run enters a human_review gate, run_status returns a pending_review block (also on supervisor_wait_event and supervisor_run_inspect). The moment you see it, you MUST relay its instruction to the user in the user's chat language, naming the options and how to decide, and then record their decision with review_decide. The run waits and does nothing until then, so if the gate stays pending across your next checks, repeat the reminder rather than going quiet.

Lifecycle: you may update, clone, version and delete playbooks. Pull playbook_howto only when authoring or reworking one.

Profiles: a node binds its executor only through a profile (agent, model, fallbacks, role prompt, skills). When working with profiles, call profile_list first to reuse an existing one, and pull profile_howto for the format, the models table and detected agents. Name any profiles you create in your final message to the user.

Other projects: call projects_list to find the user's other workspaces when a task concerns one.

Language: author playbook machine fields in English, but speak to the user about playbooks in the language of their recent messages.";
