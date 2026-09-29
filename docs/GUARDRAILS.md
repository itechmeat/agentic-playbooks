# Guardrails: what advises and what enforces

A playbook steers agents with two kinds of controls, and it helps to keep them
apart when deciding where a rule belongs:

- **Advisory controls** shape what the agent tries to do: the node prompt, the
  profile's SOUL, skills, project memory files (`CLAUDE.md`, `AGENTS.md`). They
  make a violation rare. They cannot make it impossible, because the agent may
  misread, forget or ignore them.
- **Deterministic controls** check or constrain what actually happened: a script
  exit code, a literal marker, a file fingerprint, a trust digest, a gate that
  waits for a person. They do not depend on the agent following instructions.

The rule of thumb: state the rule in a prompt or skill, and put a deterministic
check behind every rule that must hold. A rule that exists only in a prompt is a
wish. The feedback-loop reports behind several APB features (a run that reported
success without the outcome, a deliverable a later node destroyed, placeholder
output recorded as a result) were all cases where prompt discipline alone did
not hold.

## Control layers

Delivery work with agents is usually described in these layers, from the
softest to the hardest:

1. **Guidance**: prompts, role prompts, skills and memory files. Advisory.
2. **Inner-loop checks**: the tests, linters and checks the agent runs on its
   own work, and a check that decides whether a step's output is accepted.
3. **Tool and environment policy**: what the agent may touch while it works
   (permissions, per-tool-call hooks in the host agent, sandboxing, protected
   paths).
4. **Gates between stages**: a deterministic check or a person that decides
   whether the output of one stage may start the next.
5. **Release and irreversible actions**: merge, deploy, publish and anything
   that cannot be taken back, behind an explicit human confirmation.
6. **Provenance and measurement**: the record of who asked for what, what the
   agent produced, who approved it, and how the process performs over time.

APB is an engine that drives agents, not an agent host. It enforces at the
boundaries it owns (an attempt, a node, a run, a launch) and leaves per-tool-call
policy to the host agent and its administrators (see "Host agent settings" below).

## APB constructs by layer

| Construct | Layer | Advisory or enforced | What it does |
|---|---|---|---|
| Node `prompt`, profile SOUL, `skills` | 1 Guidance | Advisory | Tells the agent what to do and how. Skills are delivered by name or as snapshot copies, never pasted into the prompt. |
| `success_check` script (`scripts/...`) | 2 Inner loop | Enforced | A non-zero exit rejects the attempt's success report; the attempt fails, consumes a retry and keeps its text in `rejected_output`. |
| `success_check: { marker: ... }` | 2 Inner loop | Enforced | The literal marker must appear in the output, else the success report is rejected. Defends against interim text recorded as a result. |
| `require_verdict` | 2 Inner loop | Enforced | The attempt must write its verdict to `APB_STATUS_FILE`; a process that ends without one is `interrupted`, not `succeeded`. |
| `outputs.files` | 2 Inner loop | Reported | Declared deliverables are captured; a missing one journals `deliverable_missing` (a warning, the node keeps its status). |
| `outputs.fields` | 2 Inner loop | Reported | Declared JSON fields of the output; missing ones journal `output_fields_missing`. |
| `completion_check` (decision model) | 2 Inner loop | Advisory, enforce opt-in | A decision model rates whether the reply is a finished result. Shadow and advise change nothing; enforce needs a stored, measured threshold (see DECISIONS.md). |
| `protect` on `agent_task` | 3 Tool and environment | Enforced (post-hoc) | Globs of paths the attempt must not change, relative to the node's working directory. A change fails the attempt with `protected path modified: <path>`, consumes a retry, keeps `rejected_output`, journals `protected_paths_modified`, and the engine restores the protected files from its pre-attempt snapshot before the retry. `.gitignore`d paths (as ignored when the attempt starts) are ignored on git trees; the restore never writes through a symlinked directory or into a hardlinked file, refuses a stored copy that no longer matches its digest (the node then fails at once), and keeps the copies when a path cannot be restored. A node with `protect` never runs in a concurrent batch. V75 rejects an invalid, absolute or escaping glob; V76 warns when a glob matches no file. |
| Profile `environment: minimal` | 3 Tool and environment | Enforced (configuration) | A claude executor starts without the user's settings, plugins, user skills and MCP servers. Project hooks still run (measured below). |
| Connector grants, `functions: read_only`, `max_calls` | 3 Tool and environment | Enforced | A node can call only the connector functions it is granted, read-only when so declared, within a per-attempt call budget. Secrets never reach the agent. |
| `model_policy` (global config) | 3 Tool and environment | Enforced | Models outside the policy are refused at validation and run start. Lives only in the global config, out of a project's reach. |
| `isolation` | 3 Tool and environment | Declared | A per-node working directory with skill copies. Not an OS sandbox (V16 says so). |
| `human_review` | 4 Gates | Enforced | The run waits for a person's decision; nothing is preselected. |
| `judge` nodes and edges | 4 Gates | Model-decided | A decision model answers typed questions and routes; thresholds are declared, failures take `on_unavailable`. |
| Trust digests (playbook, profile bundle, connector, account) | 4 Gates | Enforced | A changed playbook, profile, skill or connector drops its approval; MCP starts are refused until a person approves or acknowledges. |
| Run gate (`lifecycle`, `requires`) | 4 Gates | Enforced | Drafts are refused, requirements must be met, sub-playbook pins must match. |
| `goal.criteria` | 4 Gates | Reported, enforce opt-in | At the finish node, `script` criteria (under the version's `scripts/`, run in the run's working tree, exit 0 passes; a run-directory copy changed after the start is not run and reads as `error`) and `marker` criteria (a literal string in the finish answer or any node's latest output) are checked and journaled as `goal_checked`; `manual` criteria are listed as a checklist for a person. With `goal.enforce: true` a failed script or marker criterion fails the run. |
| `effects: [irreversible]` (playbook, node, any sub-playbook, or a granted connector function flagged `irreversible: true`) | 5 Release | Enforced (engine, at run start; patch and resume guarded) | A run whose tree declares `irreversible` (the playbook's own effects, its nodes', every sub-playbook's recursively, and every connector function flagged `irreversible: true` that a node is granted) starts only with an explicit consent, recorded in the run manifest as `consent: { irreversible: true, by: ..., sources: [...] }`. Who consents: an MCP host that passes `confirm_irreversible` on `playbook_run`, `playbook_execute_plan` or `run_resume` after asking the person (`by: mcp` or `mcp:<client>`); a person answering `y` to the `[y/N]` question `apb run` asks at an interactive terminal (`cli`; never inside a run, `APB_RUN_ID` set) or a script author writing `--confirm-irreversible=<consent_nonce>` (`cli_flag`); a person confirming the dashboard's Run dialog (`dashboard`). Without it the start is refused before anything is written (`irreversible_requires_confirmation`, with the `sources` and a `consent_nonce`). The consent is bound to what was shown: the confirmation echoes the nonce (a hash of the trust digests of the playbook and every pinned sub-playbook, and the sorted sources), and a tree that changed since, a sub-playbook included, is refused again; a bare `true` (and, on MCP, `acknowledge_untrusted: true` alone) is accepted for one release with a deprecation note. `acknowledge_untrusted` answers trust only; a trust refusal of an irreversible tree names its irreversible sources and nonce too, so the host asks one question. Every start path checks it (the engine again in run preparation, so no start skips it); a supervisor patch may not add a source the run's consent does not cover or point a consented sub-playbook node at another playbook (`patch_rejected`, `irreversible_requires_confirmation`); the engine refuses a resume of an irreversible run whose manifest records no covering consent, whatever starts it; a resume asks once for a run with no valid consent (one an older apb started, or a directory apb did not create here), and the consent is then written into its manifest. A sub-playbook inherits its parent's consent (`inherited_from`) only for a node that consent covered. The dashboard API trusts its authenticated caller: any holder of the dashboard token can send `confirm_irreversible`, so a trigger or an event bridge is refused only because it does not. A shipping-looking step that declares nothing is V90 (a warning), not a consent source. `apb eval` never runs such a playbook at all (V81, docs/EVALS.md), and `playbook_trial` refuses it (`trial_forbidden_irreversible`). |
| `auto_decide` refusal (V73) | 5 Release | Enforced | An automatic review decision is refused where the playbook declares `irreversible` or `secrets` effects, a downstream node declares `effects: [irreversible]`, a downstream node binds a connector function flagged `irreversible: true`, or a downstream step looks like a merge, push, deploy or publish by name. Downstream write connector functions give a warning; a downstream connector whose manifest does not load warns at validation and refuses at run time (effects unknown). `auto_decide_ok: true` is the author's explicit override. |
| Journal (`events.jsonl`), run manifest | 6 Provenance | Recorded | Every attempt, gate decision, retry, supervisor action and decision is journaled; the manifest snapshots what the run was started with. |
| `APB_RUN_ID`, `{{run.id}}`, `Apb-Run:` trailer, `artifacts_committed` | 6 Provenance | Recorded | Links commits to runs in both directions (see HOWTO-authoring.md "Linking runs, commits and tracker records"). |
| `apb stats`, `apb decisions report` | 6 Measurement | Read-only | Cross-run metrics from journals only, and the accuracy of each decision-model use against labels the journals hold. |

## Choosing where a rule goes

- The rule is about the result of a step ("the tests pass", "the report has a
  verdict line"): a `success_check` script or marker on that node.
- The rule is about the run as a whole ("the PR exists", "the changelog names
  the release"): a `goal` criterion, with `goal.enforce: true` when a miss must
  fail the run.
- The rule is "do not touch X while doing Y" (tests during a fix, a spec during
  implementation): `protect` on the node. It is post-hoc, so it works with every
  agent, and it cannot prevent the write, only reject the attempt and restore
  the files.
- The rule needs judgment: a `human_review` gate, or a judge with a measured
  threshold, never a prompt alone.
- The action cannot be taken back: declare `effects: [irreversible]` and keep a
  person in front of it. A trigger or a model is not a person's confirmation:
  the engine refuses such a run without a consent (see the table).

## Host agent settings

APB does not reimplement per-tool-call hooks, permission lists or OS sandboxing:
those belong to the host agent and its administrators, and a second, weaker
copy inside APB would only add confusion. What matters is how an APB run
interacts with them.

### Do a project's `.claude/settings.json` hooks run under `environment: minimal`?

Yes. Measured with Claude Code 2.1.284 on 2026-09-29: a scratch git repository
had a `.claude/settings.json` with `SessionStart`, `UserPromptSubmit` and
`PreToolUse` (matcher `Bash`) hooks, each appending a line to a marker file. The
agent was asked to run `echo hi` with its Bash tool.

The claude command line of a minimal-environment node, as apb builds it:

```sh
claude -p --output-format json --model <model> \
  --permission-mode bypassPermissions \
  --settings <run>/agent-settings/<node>.json \
  --setting-sources project,local --strict-mcp-config \
  -- "<prompt>"
```

where `<run>/agent-settings/<node>.json` is apb's own file
(`{"hooks": {}, "enabledPlugins": {}, "enableAllProjectMcpServers": false}`).

Results:

| Launch | Marker file |
|---|---|
| The command above, by hand | `sessionstart`, `userpromptsubmit`, `pretooluse` |
| The same without the `--settings`/`--setting-sources`/`--strict-mcp-config` flags (`environment: full`) | `sessionstart`, `userpromptsubmit`, `pretooluse` |
| A real `apb run` of a one-node playbook whose claude profile has no `environment` key (so `minimal`; the manifest records `hermetic: true`) | `sessionstart`, `userpromptsubmit`, `pretooluse` |

So the project's own hooks (`.claude/settings.json`, and
`.claude/settings.local.json` through the `local` source) keep running under the
minimal environment: the empty `hooks` object in apb's settings file does not
remove hooks that the project and local sources define. What the minimal
environment leaves out are the user-scope settings (`~/.claude/settings.json`
and its hooks), user plugins, user skills and user MCP servers. A team that puts
its guardrail hooks in the repository therefore keeps them in APB runs; a hook
that lives only in a person's user settings does not reach a minimal node (use
`environment: full` on that profile, or move the hook into the repository).
Managed settings that an administrator deploys are outside both sources apb
selects and follow the host's own precedence rules.

Other agents (codex, opencode, zcode and others) run as they are configured;
their own hook or policy mechanisms apply as they would in a manual session.
