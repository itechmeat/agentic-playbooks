# apb documentation

Everything about `apb`, grouped by what you are trying to do. Agents have a
machine-oriented entry point in [llms.txt](../llms.txt).

## Get started

- [Install](INSTALL.md): platforms, the installer, Homebrew, cargo, manual download with checksum and attestation checks, source build, updating, uninstalling
- [Your first run](FIRST-RUN.md): the keyless demo, a shipped playbook, a playbook of your own
- [Setup by an agent](AGENT-SETUP.md): the procedure a coding agent follows to install and set up apb for you
- [Command line reference](CLI.md): every command in one map, the workspace registry

## Build a routine

- [Playbook authoring](HOWTO-authoring.md): the playbook format, node types, edges, conditions, lists, waits, templating
- [Playbook interview](HOWTO-interview.md): capturing a process as a playbook by interviewing the person who does it
- [Profiles](PROFILES.md): agent, model, fallbacks, role prompt and skills
- [Connectors](CONNECTORS.md): calling mail and other services from playbooks, inbox events, sending once
- [Cloud models](CLOUD-MODELS.md): models served by a cloud catalog
- [Decision models](DECISIONS.md): optional small models for checks, judges and ranking
- [Interactive agents](INTERACTIVE-AGENTS.md): nodes that ask the person a question mid-run
- [Notifications](NOTIFY.md): being told when a run needs you or ends

## Use from a coding agent

- [MCP tools](MCP.md): connecting an agent, the tool surface, supervision, the read-only server
- [Host integration](HOST-INTEGRATION.md): runs where the calling agent executes the tasks

## Safety

- [Guardrails](GUARDRAILS.md): which controls advise and which enforce, the launch policy, billed use, unattended runs
- [Isolation](ISOLATION.md): running an agent in a sandbox on Linux, and what each level guarantees
- [Publishing](PUBLISH.md): how a push leaves a run, and what a person approves
- [Result snapshots](RESULTS.md): what a run keeps of its results, approvals of an exact snapshot
- [External data runs](EXTERNAL-DATA.md): runs whose data must not reach any learning store
- [Deployment](DEPLOYMENT.md): the dashboard on a server behind a proxy, keys, hardening
- [Security policy](../SECURITY.md): reporting a vulnerability

## Measure and improve

- [Evals and scenarios](EVALS.md): checking a playbook in scratch repositories, scenarios that play in seconds
- [Run stats](STATS.md): metrics across runs
- [Run quality](QUALITY.md): grading runs on quality criteria and promoting a candidate by quality
- [Known problems](KNOWN-PROBLEMS.md): what earlier runs met with a playbook's tools
- [Corrections](CORRECTIONS.md): what people keep correcting in a playbook's runs
- [Repeats in session history](DISCOVER.md): step sequences past agent sessions repeat, offered as drafts
- [Reference scenarios](scenarios/README.md): realistic code and routine situations features are checked against

## Operate and contribute

- [CI](CI.md): running apb in pipelines
- [Compatibility](COMPATIBILITY.md): what the 1.x promise covers
- [Release notes](release-notes/): one file per tagged release
- [Feedback loop](FEEDBACK-LOOP.md): how an agent reports apb problems after a run, and the block to add to your project
- [Contributing](../CONTRIBUTING.md), [maintainers](../MAINTAINERS.md), [testing guidelines](TESTING-GUIDELINES.md), [build optimization](BUILD-OPTIMIZATION.md)
- [Decision records](adr/) and [design specs](superpowers/specs/)
