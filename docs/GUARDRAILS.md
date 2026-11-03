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
| Profile `process_env: allowlist` | 3 Tool and environment | Enforced (configuration) | An agent process inherits only a base set, the variables its agent CLI reads and the profile's `allow` names, not the operator's whole environment. Each attempt journals `agent_env_applied` with the withheld names; `apb doctor --run` names them when an attempt fails. Context compaction in such a run gets an empty allowlist for claude. On Linux apb also hides its own environment from the agent (non-dumpable, so `/proc/$PPID/environ` is unreadable); macOS has no equivalent. Not isolation from a hostile agent of the same user: the operator's other processes stay readable. Scripts, checks and host-mode steps keep the operator environment (PROFILES.md "Process environment"). |
| Connector grants, `functions: read_only`, `max_calls` | 3 Tool and environment | Enforced | A node can call only the connector functions it is granted, read-only when so declared, within a per-attempt call budget. Secrets never reach the agent. |
| `model_policy` (global config) | 3 Tool and environment | Enforced | Models outside the policy are refused at validation and run start. Lives only in the global config, out of a project's reach. |
| `isolation` | 3 Tool and environment | Enforced (Linux) | An unprivileged bubblewrap sandbox per attempt: own process namespaces, only the node's tree and declared paths visible, the repository's git config and hooks read-only, `.apb` masked, the environment forced to the allowlist, `network: none` or the host network. `full` refuses to run where the sandbox cannot be built (`isolation_unavailable`: no bubblewrap, macOS, an agent without declared sandbox needs, host mode); `best_effort` runs without it and records why. Every attempt lists its limits (the host uid, the open network, the agent's own login, scripts outside the sandbox). See ISOLATION.md. |
| `read_only: true` on `agent_task` and `script` | 3 Tool and environment | Enforced (sandbox), else detected (post-hoc) | The node must not change its tree. In the sandbox (`isolation: full`, or `best_effort` where it works) the tree and its git directory are mounted read-only, so a write fails as it happens (`enforced`). Without a sandbox apb compares the tree (initialized submodules included), each repository's `HEAD`, refs, staged index and the files that change what git does (config, hooks, `info/`), every `.gitignore` (so a new one that hides itself shows) and the `.apb` definitions at the root before and after each attempt (`detected`, or `detected_partial` above 20000 entries, 512 MiB, 8 nested repositories or 2000 git control entries): a change journals `read_only_violated` and fails the node with `read_only_dirty`, with no retry, no fallback, no reprompt and no restore; ignored files, the run's own `.apb` files, `.apb` and `.git` directories below the root and changes outside the tree are not checked. A detected-tier node never runs in a concurrent batch. Evidence names the changed paths, never their content. See ISOLATION.md "Read-only nodes". |
| `agent_launch`, launch floor, `launch:` in `policy.yaml` | 4 Gates | Enforced (run gate) | Whether an agent may launch a playbook: `allow` (the default), `ask` (only with the nonce of the refusal, after the host asked the person) or `deny` (only a person, from the dashboard or an interactive terminal). The run's policy is the strictest over its tree of the playbook's declaration held up by its launch floor, the machine settings and the project settings. Every start, trial, plan execution, eval and resume of a run never authorized goes through the one run gate; a run authorized once stays authorized for its lifetime. See "Launch policy for agents" below. |
| `human_review` | 4 Gates | Enforced | The run waits for a person's decision; nothing is preselected. |
| Unattended mode (`apb run --unattended`), `human_review.unattended` | 4 Gates | Enforced | A run nobody attends fails at once (`hitl_required`) at every review, publish gate and question that declares no unattended outcome, without taking a failure route, and fails `budget_exhausted` where an attended run would pause for its budget. A review may declare `unattended: { decide: <option> }`, journaled with `decided_by: unattended` and never counted as a person's consent (nor is a `decided_by` this apb does not know); the validator refuses an option that reaches an irreversible or secret step before the next gate, over edges and failure handlers (V142), and the run checks it again. An unattended review never waits: a decision lost to a crash is made on resume, one that cannot be recorded fails the run. A publish gate has no unattended outcome. Host execution and supervised modes are refused at start, and so is an unattended start from a run step of an attended run (a person, CI, an agent session or a step of an unattended run may choose the mode; a run step is recognized by its environment, as for every run-step refusal, so this keeps a cooperating step from choosing it by mistake). The mode grants no consent: irreversible starts and the launch policy are unchanged. See CI.md "Unattended runs". |
| `judge` nodes and edges | 4 Gates | Model-decided | A decision model answers typed questions and routes; thresholds are declared, failures take `on_unavailable`. |
| Trust digests (playbook, profile bundle, connector, account) | 4 Gates | Enforced | A changed playbook, profile, skill or connector drops its approval; MCP starts are refused until a person approves or acknowledges. |
| Run gate (`lifecycle`, `requires`) | 4 Gates | Enforced | Drafts are refused, requirements must be met, sub-playbook pins must match. |
| `goal.criteria` | 4 Gates | Reported, enforce opt-in | At the finish node, `script` criteria (under the version's `scripts/`, run in the run's working tree, exit 0 passes; a run-directory copy changed after the start is not run and reads as `error`) and `marker` criteria (a literal string in the finish answer or any node's latest output) are checked and journaled as `goal_checked`; `manual` criteria are listed as a checklist for a person. With `goal.enforce: true` a failed script or marker criterion fails the run. |
| `effects: [irreversible]` (playbook, node, any sub-playbook, or a granted connector function flagged `irreversible: true`) | 5 Release | Enforced (engine, at run start; patch and resume guarded) | A run whose tree declares `irreversible` (the playbook's own effects, its nodes', every sub-playbook's recursively, and every connector function flagged `irreversible: true` that a node is granted) starts only with an explicit consent, recorded in the run manifest as `consent: { irreversible: true, by: ..., sources: [...] }`. Who consents: an MCP host that passes `confirm_irreversible` on `playbook_run`, `playbook_execute_plan` or `run_resume` after asking the person (`by: mcp` or `mcp:<client>`); a person answering `y` to the `[y/N]` question `apb run` asks at an interactive terminal (`cli`) or a script author writing `--confirm-irreversible=<consent_nonce>` (`cli_flag`); a person confirming the dashboard's Run dialog (`dashboard`). A CLI start from inside a run (`APB_RUN_ID` set, as in an agent or script step) cannot consent at all: the terminal question is not asked and the flag is refused, since a step that read the refusal could echo its nonce; the refusal names the parent run and points at the host (MCP `confirm_irreversible` after asking the person). No apb path forwards a consent to a nested start through the environment; `apb run --supervise` hands its own consent to its drive process in a private, single-use handoff file (never on its command line), outside any run. Without it the start is refused before anything is written (`irreversible_requires_confirmation`, with the `sources` and a `consent_nonce`). The consent is bound to what was shown: the confirmation echoes the nonce (a hash of the trust digests of the playbook and every pinned sub-playbook, and the sorted sources), and a tree that changed since, a sub-playbook included, is refused again; a bare `true` (and, on MCP, `acknowledge_untrusted: true` alone) is accepted for one release with a deprecation note. `acknowledge_untrusted` answers trust only; a trust refusal of an irreversible tree names its irreversible sources and nonce too, so the host asks one question. Every start path checks it (the engine again in run preparation, so no start skips it); a supervisor patch may not add a source the run's consent does not cover or point a consented sub-playbook node at another playbook (`patch_rejected`, `irreversible_requires_confirmation`); the engine refuses a resume of an irreversible run whose manifest records no covering consent, whatever starts it; a resume asks once for a run with no valid consent (one an older apb started, or a directory apb did not create here), and the consent is then written into its manifest. A sub-playbook inherits its parent's consent (`inherited_from`) only for a node that consent covered. The dashboard API trusts its authenticated caller: any holder of the dashboard token can send `confirm_irreversible`, so a trigger or an event bridge is refused only because it does not. A shipping-looking step that declares nothing is V90 (a warning), not a consent source. `apb eval` never runs such a playbook at all (V81, docs/EVALS.md), and `playbook_trial` refuses it (`trial_forbidden_irreversible`). |
| `publish` node | 5 Release | Enforced | The only push apb makes: the remote is pinned before agents run, a read-only plan (URL, branch, commits, the remote's state) is shown, a person consents to exactly that plan by its nonce (`apb publish approve`, MCP `review_decide` with `nonce`, the dashboard's "Publish exactly this"), the push is recorded in the send ledger before it leaves and runs under an exact lease, and an unknown result is never repeated on its own. A dashboard without keys refuses the consent (anything on the machine reaches it) and shows the CLI command. It guarantees that nothing else pushed only when every agent node is `isolation: full` (`publish_only` makes that an error to break and holds sub-playbook runs to it); otherwise V119 warns and the plan flags commits already on the remote. See PUBLISH.md. |
| `auto_decide` refusal (V73) | 5 Release | Enforced | An automatic review decision is refused where the playbook declares `irreversible` or `secrets` effects, a downstream node declares `effects: [irreversible]`, a downstream node binds a connector function flagged `irreversible: true`, or a downstream step looks like a merge, push, deploy or publish by name. Downstream write connector functions give a warning; a downstream connector whose manifest does not load warns at validation and refuses at run time (effects unknown). `auto_decide_ok: true` is the author's explicit override. |
| Journal (`events.jsonl`), run manifest | 6 Provenance | Recorded | Every attempt, gate decision, retry, supervisor action and decision is journaled; the manifest snapshots what the run was started with. |
| `APB_RUN_ID`, `{{run.id}}`, `Apb-Run:` trailer, `artifacts_committed` | 6 Provenance | Recorded | Links commits to runs in both directions (see HOWTO-authoring.md "Linking runs, commits and tracker records"). |
| `apb stats`, `apb decisions report` | 6 Measurement | Read-only | Cross-run metrics from journals only, and the accuracy of each decision-model use against labels the journals hold. |

## Launch policy for agents

A playbook can say how far an agent may launch it on its own, with
`agent_launch: allow | ask | deny` (HOWTO-authoring.md "Launch policy"). The
run gate decides it for every launch surface, on the exact permit the run
will use:

| Effective policy | Agent over MCP | Run step (MCP or CLI) | CLI at a terminal | CLI without a terminal | Dashboard with keys | Dashboard without keys | Engine (waker, quota probe, `resume --all`) |
|---|---|---|---|---|---|---|---|
| `allow` | runs | runs | runs | runs | runs | runs | runs |
| `ask` | runs with the refusal's nonce (`confirm_launch`), else refused | refused | asks the person, or `--confirm-launch` | `--confirm-launch` only | runs | runs after the dashboard confirms the refusal's nonce | refused |
| `deny` | refused | refused | asks the person | refused | runs | refused | refused |

A dashboard without keys counts as the CLI without a terminal, since
anything on the machine can reach its port: issue a key with `apb server key
issue` and sign in to start a `deny` playbook from the dashboard. A run step
or a CLI without a terminal whose config dir was redirected (`APB_CONFIG_DIR`,
`XDG_CONFIG_HOME`, `HOME`) away from the default one, `~/.config/apb` of the
real user, is refused with `policy_file_invalid` when the default one holds
stricter launch settings or floors; `apb eval` hands its cases the config dir
it read, and that one is accepted. A project's floors and the machine's exact
`<workspace id>/<id>` entries use only a workspace id registered at that
project's root in `<config dir>/projects.json`, never what
`.apb/workspace.local` alone says.

A refusal carries a start action: `confirm_launch` with the nonce, the version
and how it was chosen, or `launch_by_person`. The nonce covers the execution
workspace, the playbook and the chosen version, a digest of the whole permit
(the definition, verified profile bundles with their skills, connectors,
accounts and every pinned sub-playbook with its own maps), the effective
policy with its sources and the launch kind; anything that changes after the
refusal needs a fresh one. The engine column applies only to a run that holds
no authorization.

**The guarantee.** An agent that uses apb's documented agent channels (MCP,
or the CLI inside a run step or without a terminal) cannot start, trial,
evaluate or resume a playbook whose effective policy is `deny`, and cannot do
so for `ask` without passing the nonce of the refusal; the CLI at an
interactive terminal asks the person. An agent channel cannot loosen a
playbook's declared policy. Outside this guarantee: proof that a person
answered (a process that controls a terminal, signs in to a dashboard with a
key, edits the machine files or forges apb's internal handoff file is not
stopped); the project file, which anything that writes the project tree can
change and which only narrows; a new playbook an agent writes under a new id,
which has no floor of its own and falls under the machine and project
defaults (set the machine default to `ask` to cover it); and older apb
binaries on the same machine, which ignore every launch setting.

**The launch floor.** Every saved version of a playbook raises its floor, the
strictest `agent_launch` any version of it declared, kept outside the
definition in `<config dir>/launch-floors.json`. A save from an agent channel
that would declare less is refused (`launch_policy_loosened`); deleting the
key, deleting and recreating the playbook, restoring it from the trash or
copying it to global scope keeps the floor (a playbook deleted before its
floor was recorded is read from its trash entries). A person lowers it by
confirming the lowering in the editor of a dashboard with keys, or with `apb
launch floor <id> [--global] --lower-to <policy>` at an interactive
terminal; the editor of a dashboard without keys never lowers it. A floor is
written, durably, before the version it covers becomes visible, so a crash
leaves the floor stricter than the store, never looser.

**Lifetime.** A run records its authorization once (`launch` in its
manifest, or on the `run_resumed` that authorized it): who launched it, how
an `ask` or `deny` launch was confirmed, the policy and its sources, never a
nonce. Pauses, the waker, the quota probe and every later resume continue it
without another question, even after the policy was tightened. A run an
older apb started holds no authorization: its first resume is checked on its
own snapshot and pinned children, and a run without pins gets the child tree
that check verified persisted as its pins.

**Fail closed.** A `policy.yaml` that does not read (an unknown section, key
or value, named in the error) refuses agent launches with
`policy_file_invalid`, and a damaged `launch-floors.json` refuses agent
launches and agent saves with `launch_floor_invalid`; the dashboard and a
terminal still launch, with a warning, and `apb doctor` names the file. A
later release that adds a section to `policy.yaml` makes this release refuse
agent launches on a machine whose file uses it, until apb is upgraded.

**Older binaries.** An apb older than this release ignores `agent_launch`,
the floors and `policy.yaml`: it reads and resumes the new records, and runs
every playbook as `allow`. The policy holds only while every apb on the
machine is this release or newer; `apb doctor` warns when another apb is on
`PATH`.

## Billed use

Agent work can be paid for in two ways: on a subscription login, or billed
(an api key, a cloud account, metered usage). Which one a person accepts
differs from person to person, so apb starts billed work only on a
person's explicit command, unless the settings say otherwise. The
`billing:` section of `policy.yaml` decides it, the machine's file
(`<config dir>/policy.yaml`) first and a project's (`.apb/policy.yaml`) only
narrowing it, like the `launch:` section:

```yaml
billing:
  billed: ask                       # allow | ask | deny (default ask)
  unknown_basis: ask                # allow | ask | deny (default ask)
  budget_counts_subscription: true  # true | false (default true)
```

- `billed` decides agent work whose cost is billed, `unknown_basis` work
  whose basis apb cannot tell (HOWTO-authoring.md "What counts as money"
  says how each agent's basis is read). Work on a subscription login is
  never held.
- `allow` starts it. `ask` starts it only after a person confirmed the
  exact set of agents and bases the run would use. `deny` never starts it,
  for anyone; the remedy is a profile whose agent runs on a subscription
  login, or a changed machine setting.
- `budget_counts_subscription` says whether a run budget's dollar limit
  counts the list price of work a subscription covers (HOWTO-authoring.md
  "budget"). The default `true` counts every reported cost, since a
  subscription can bill extra usage under the same login; `false` is for a
  person who knows their plan never bills extra.
- A project's file may only make a setting stricter (`allow` to `ask` or
  `deny`, `ask` to `deny`, `false` to `true`); a looser value there is
  ignored. A file that does not read, the machine's or the project's,
  fails closed: billed and unknown-basis work is denied (and a budget
  counts every cost) until it is fixed, since the file may have said so;
  a damaged project file never drops a machine's restriction. `apb
  doctor` shows the effective section and the settings that produced it,
  and flags a file that does not read.

**Before a run starts.** The run gate (every launch surface: `apb run`, MCP
`playbook_run`, `playbook_execute_plan` and `playbook_trial`, the
dashboard, `apb eval`, and the resume of a run never authorized) predicts
the basis of every agent the run's profiles bind, primary, fallbacks and
tier executors, over the whole tree, and the background supervisor of a
supervised run, from the facts a spawn would see (a node attempt under
apb's minimal agent settings unless its profile asks for the full
environment, the supervisor on the agent's own settings). Under `ask`, a
billed or unknown prediction is refused with
`billing_requires_confirmation`, a `billing` block naming the held agents
and bases, and a nonce that covers that exact set:

| Who launches | `ask` | `deny` |
|---|---|---|
| Agent over MCP | the refusal's nonce (`confirm_launch`) after the host asked the person | refused |
| Run step (MCP or CLI) | refused (`billing_refused_in_run_step`) | refused |
| CLI at a terminal | asks the person, or `--confirm-launch` | refused |
| CLI without a terminal | `--confirm-launch` only | refused |
| Dashboard (with or without keys) | the refusal's nonce, after the dashboard asked the person | refused |
| Engine (waker, `resume --all`) | refused (`billing_refused_for_engine`) | refused |

A run step or a CLI without a terminal whose config dir was redirected
(`APB_CONFIG_DIR`, `XDG_CONFIG_HOME`, `HOME`) is also held to the settings
of the default config dir, the stricter of each, since a variable is not
how a setting is changed; an eval case keeps the config dir its `apb eval`
handed over. When the launch policy also asks, one question and one nonce
cover both.
A confirmation covers one start. The nonce of a billing refusal is a fresh
challenge recorded in the workspace (`.apb/launch-challenges`) for that
exact launch and predicted set, and it lapses after an hour. The start that
passes it back on its own channel consumes it once the start is otherwise
committed: right before the run's launch record is written, before a
detached `apb run --supervise` hands its grant over, once for a whole `apb
eval`. A start refused for another reason after the billing check (the
irreversible consent, a missing param, the budget, an untrusted playbook)
leaves it open, so the corrected start needs no second question. Starting
again with a consumed nonce is refused with a new one, so a person is asked
for every run. Without a working random source no challenge is recorded,
and the nonce handed out confirms nothing.
The run keeps what passed in its launch record (`launch.billing`: the bases
the settings allowed, the agents and bases a person confirmed, how), never
a nonce, for its whole lifetime.

**At every spawn.** Right before an agent attempt starts, its basis is read
again from the facts that spawn sees. Work the run's record does not cover
(a login that changed to an api key mid-run, a fallback on another login)
is not started: the engine pauses the run, `billing (<agent>: <basis>)`,
like a budget pause; an unattended run, which has nobody to confirm it,
fails there instead (``hitl_required: billing hold `<node>` ...``, no failure
route, no pause). A plain resume, a supervisor and the engine are
refused (`billing_paused`); a person resumes it at a terminal, with
`--confirm-launch`, through MCP `run_resume` with the refusal's nonce after
asking, or from the dashboard, which adds that agent and basis to the
run's record. A setting changed to `allow` in the meantime lets a resume
continue without a question. A terminal answer or a judge whose work is not
covered fails instead of spending, and context compaction is skipped. A
run started by an older apb holds no billing record and is checked against
the settings in force at each spawn. The background supervisor and the
wake-up probe of a quota pause are agent work too: each is checked the
same way right before it starts, on the settings its process loads. A
supervisor the record does not cover is not started, and nothing records a
spawn for it: the run journals it (`supervisor_action`
`supervisor_billing_hold`) and takes the billing pause, before its first
node when it is the start's supervisor, so a supervised run never waits for
a supervisor that is not coming. A person's resume confirms its agent and
basis, and the resumed run starts the supervisor. A probe the record does
not cover is not sent, and the node's own check then pauses the run for a
person. `apb doctor --probe-agent` is started by a person, which confirms it
under `ask`; `deny` refuses it before the agent is spawned. Without a
terminal it is held to the default config dir's settings as a launch is,
so a redirected config dir does not hide a `deny` there.

**Limits.** The prediction reads local facts only (variable names, the
keys of an agent's own login and settings files), never a secret value and
never the network, so a provider that bills a subscription login for extra
usage is not seen; that is what `budget_counts_subscription` and a run
budget are for. A host task (host execution mode) runs in the host's own
session and is not checked. An older apb never checks billed use: one
without launch policies ignores `policy.yaml`, and one with them refuses
agent launches on a machine whose file has a `billing:` section (an unknown
section, see "Fail closed" above) until it is upgraded.

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
  person in front of it. A push of the run's commits belongs in a `publish`
  node, not in an agent's prompt. A trigger or a model is not a person's confirmation:
  the engine refuses such a run without a consent (see the table).

## Host agent settings

APB does not reimplement per-tool-call hooks or permission lists: those
belong to the host agent and its administrators, and a second, weaker copy
inside APB would only add confusion. The one OS boundary APB owns is a node's
`isolation`, the sandbox around the agent process it starts (ISOLATION.md);
the host agent's own sandbox settings still apply inside it. What matters is how an APB run
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
