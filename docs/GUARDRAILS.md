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
| `untrusted` / `accept_untrusted` on params, connector bindings and nodes | 3 Validation | Enforced at save | A node that renders text from outside the author's control and can send outward is refused (V179) unless the author states why the risk is accepted; the run surfaces list each such node with its reason, and every attempt ends its prompt with a fixed section telling the agent the text is data, never instructions. See HOWTO-authoring.md "Untrusted input". |
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
| `read_only: true` on `agent_task` and `script` | 3 Tool and environment | Enforced (sandbox), else detected (post-hoc) | The node must not change its tree. In the sandbox (`isolation: full`, or `best_effort` where it works) the tree and its git directory are mounted read-only, so a write fails as it happens (`enforced`). Without a sandbox apb compares the tree (initialized submodules included), each repository's `HEAD`, refs, staged index and the files that change what git does (config, hooks, `info/`), every `.gitignore` (so a new one that hides itself shows) and the `.apb` definitions at the root before and after each attempt (`detected`, or `detected_partial` above 20000 entries, 512 MiB, 8 nested repositories or 2000 git control entries): a change journals `read_only_violated` and fails the node with `read_only_dirty`, with no retry, no fallback, no reprompt and no restore; ignored files, the run's own `.apb` files, `.apb` and `.git` directories below the root and changes outside the tree are not checked. A detected-tier node may share a concurrent batch, but only beside members that cannot write the tree: a writer is admitted to a batch only after every detected read-only member has ended, and a detected read-only node is admitted only into a batch with no writer, finished members included (a read-only node under `isolation: full` is sandbox-enforced and may sit beside any batchable node; `protect` nodes and list nodes stay out of every batch). When a change is seen, every member that saw it fails, and the `read_only_violated` event lists in `concurrent_with` the batch members whose attempts overlapped, so the change cannot be attributed to one of them. Evidence names the changed paths, never their content. See ISOLATION.md "Read-only nodes". |
| `writes`, `max_changed_files` on `agent_task` and `script` | 3 Tool and environment | Detected (post-hoc) | The attempt may change only files matching the declared globs (or the rendered template), at most `max_changed_files`. After every attempt apb compares the tree (the read-only check's comparison, including each repository's state but counting file changes only): a change outside the scope, or more files than the cap, fails the node with `write_scope` and no retry, no restore. A scoped node never shares a concurrent batch with another writer and is never served from the node cache; a comparison cut short by a limit fails the node as unknown. Git-ignored files, the run's own `.apb` files and paths outside the node's tree are not checked. An older apb drops the keys and runs the node unchecked (a run this version created carries the `write_scope` run guard). |
| `agent_launch`, launch floor, `launch:` in `policy.yaml` | 4 Gates | Enforced (run gate) | Whether an agent may launch a playbook: `allow` (the default), `ask` (only with the nonce of the refusal, after the host asked the person) or `deny` (only a person, from the dashboard or an interactive terminal). The run's policy is the strictest over its tree of the playbook's declaration held up by its launch floor, the machine settings and the project settings. Every start, trial, plan execution, eval and resume of a run never authorized goes through the one run gate; a run authorized once stays authorized for its lifetime. See "Launch policy for agents" below. |
| `human_review` | 4 Gates | Enforced | The run waits for a person's decision; nothing is preselected. |
| `approves` on `human_review`, `requires_approval` on a working node | 4 Gates | Enforced (engine, at admission) | A gate's request is bound to the latest stored result snapshots of the nodes it `approves`; an approving decision must carry that binding's nonce, and the drive re-verifies every bound snapshot before it consumes the decision (a snapshot that no longer verifies ends the run `approval_snapshot_unreadable`). A node with `requires_approval` starts only when the gate's latest decision approved a binding that covers its `of` nodes, none of them ran again, the snapshots verify and are complete and, with `live: check` (the default), every bound file still hashes to its approved digest; otherwise it finishes failed with the refusal code and no attempt runs. An approval is a fact about stored bytes, never a consent: a publish still needs its own. See "Approvals and person-only gates" below. |
| `person_only` on `human_review` and `publish` | 4 Gates | Enforced (channel) | Only a person's channel decides the gate: `apb review` (or `apb publish approve`) at an interactive terminal outside a run step after a typed confirmation, and the dashboard with a signed-in session. MCP, a run step, the CLI without a terminal, a bearer key and a dashboard without keys are refused before anything is queued; no automatic decision may be declared (V156). It does not prove that a person decided. With a passkey enrolled on the machine the bar is raised further: the decision then needs a fresh WebAuthn assertion through the dashboard, and every other caller is refused `passkey_required`. See "Passkeys on person-only gates and approvals" below. |
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

## Approvals and person-only gates

Both declarations live on nodes (docs/HOWTO-authoring.md "Approving exact
results"); the design is
`docs/superpowers/specs/2026-11-03-approvals-design.md`.

### Which channel decides a person-only gate

Every surface states the channel a decision arrives through. The channel
entry (`reviews.jsonl`) carries it with the request it answers, and a
decision through a surface journals it as `review_decided.via`.

| Channel | Surface | Decides a person-only gate | Refusal code |
|---|---|---|---|
| `terminal` | `apb review` or `apb publish approve` at an interactive terminal outside a run step, after typing the option to confirm | yes | none |
| `dashboard_session` | the run page of a dashboard with keys, signed in (cookie with the CSRF marker) | yes | none |
| `cli_unattended` | the CLI without a terminal (a script, CI) | no | `no_terminal` |
| `dashboard_keyless` | a dashboard without keys: anything on the machine reaches it | no (HTTP 403; the page shows the CLI line instead of buttons) | `keyless_dashboard` |
| `http_bearer` | the HTTP API with a bearer key | no (HTTP 403) | `bearer_key` |
| `mcp` | MCP `review_decide`, whoever the host says is deciding | no (tool error) | `agent_channel` |
| `run_step` | any surface called from inside a run step (`APB_RUN_ID` or `APB_HOST_TASK` set) | no | `run_step` |
| `scenario` | a scenario run's scripted decision | only inside a scenario run | `not_this_caller` |
| `engine`, `unattended` | an enforced review recommendation, an unattended run's declared `decide` | never: V156 refuses both on a person-only gate | `not_this_caller` |

The refusal names both ways a person decides, with the exact `apb review`
command. The drive consumes a person-only gate's decision only from an
entry stamped with a person's channel and the open request; a line
appended by hand, by an older apb or through another channel stays in the
file, is never consumed, and `apb doctor --run` lists it in its `reviews`
row.

**What this does not prove.** A process that controls a terminal, holds a
signed-in browser session cookie, or writes apb's channel files is not
stopped. Over MCP the server cannot tell a host agent from a person
relaying, and over HTTP it cannot see a caller's environment, so neither
ever counts as a person's channel.

**A server key is a person's credential.** Whoever holds a key from `apb
server key issue` can sign in to the dashboard with it (`POST
/api/auth/login`) and decide a person-only gate through the signed-in
session. The `bearer_key` refusal therefore guards against mistakes (a
script posting with its key by habit), not against a key holder. Do not
hand a key to an agent, a script or CI that must not decide person-only
gates; `apb server key issue` and `apb doctor` (its `server keys` line)
say so.

**Host-mode tasks.** A host agent's own MCP server runs no step, so it has
no `APB_RUN_ID`: the own-run refusal below does not reach a host task's
subagents through that server, and MCP is never a person's channel in any
case.

**Supervisor patches.** A supervisor's patch, for the current run or for
the next runs, never weakens these constraints: it is refused
(`approval_weakened`) when it drops `person_only`, drops or narrows
`approves` (fewer results, more approving options), drops or loosens
`requires_approval` (another gate, fewer results, `live: ignore`), removes
a node that declares one of them, or opens a way to a node that a
person-only or approving gate stood before on every path. An in-run patch
is refused as well when it changes where any option of such a gate routes
(the gate's option-to-target map is frozen while the run is under way, so
a person's `reject` never comes to lead where `approve` led).

**Moving a run past a gate.** Every way of moving a run to a named node
shares one check (`apb_engine::approval::skips_gate`): an in-run patch's
`continue_from`, `supervisor_run_continue_from` and `supervisor_node_retry`
(at a parked gate or at a failed node), and a resume's `from_node`. A
person-only or approving gate that stands before the target on every path
counts as passed only by its latest decision, and only when that decision
answers the gate's latest request; is an approving option on that
request's binding for a gate that `approves` results, and any option but
`reject` for a gate that is only `person_only`; takes an edge that leads to
the target without passing the gate again; and belongs to the current
visit (no node started since it that leads back into the gate and not on to
the target, as a loop back toward the gate does). A rejection, or an
approval from an earlier loop, never lets a move past the gate. A refused
directive is journaled as a `supervisor_action` with action
`directive_refused` and the `approval_weakened` reason, and the run stays
where it was: an open request stays open. A resume from such a node is
refused (`approval_weakened`, with the channel's refusal code) through MCP
`run_resume`, inside a run step, at the CLI without a terminal and on a
dashboard without keys; only a person goes past the gate: `apb resume <run>
--from-node <node>` at an interactive terminal outside a run step, after
typing the node to confirm, or the run page of a dashboard where the person
is signed in.

**Re-checks see the node's own edits.** `requires_approval` with `live:
check` is checked again as each execution of the node begins, against the
tree as it is then. A node whose attempt edited a bound file and was then
paused (an operator pause, a quota or budget pause) is refused
`approval_stale` when it resumes, and a sequential list node whose items
edit a bound file refuses the next item because of the previous item's
write. Both fail closed; declare `live: ignore` for a node that edits what
it was approved on, or bind the files it does not touch. A refusal settles
an admitted visit failed; for a list item it settles that item as a
rejection, which counts against `batch.retries` and then marks the item
`needs_look`, while the visit goes on.

**What these checks do not cover.** A supervisor's in-run patch may still
rewrite the definition of a node behind a person-only gate (its script or
its prompt), or add new nodes before the gate that do the protected work:
these are the supervisor's general powers over the run's definition, not a
weakening of the gate's fields, and a playbook that must not allow them
runs without a supervisor or with `supervisor.capabilities` that leave out
`patch_playbook`. The own-run refusal below reads the whole run tree: a
parent whose journal names a child run that was deleted or pruned cannot
be read whole, so every run-step decision on that tree is refused, unrelated
steps included (fail closed).

### A run step never decides for its own run

Independently of `person_only`, a decision or an answer posted from inside
a run step (`APB_RUN_ID` set) to a run of its own tree is refused with
`run_step_own_run`. The tree is the run, its ancestors (followed through
`parent_run`, up to 8 levels) and every descendant of each of them through
the child runs, so a sibling run (a child of the same parent, whose outcome
steers that parent) is in it. A tree that cannot be read whole (a config
or a journal that does not read, or links beyond the depth) refuses the
step: the check fails closed. It covers MCP `review_decide`,
`run_answer` for a question answered by a person and `run_intent_resolve`,
and the CLI `apb review`, `apb answer`, `apb publish approve` and `apb
intent resolve`. A step deciding another run's ordinary gate is allowed (an
orchestrating playbook may drive other runs).

### Caller refusal codes

Every next action a surface shows carries `caller_refusal` when this
caller may not take it. The list is closed and the same on MCP, the HTTP
API and the CLI (`CALLER_REFUSALS` in the generated constants):

| Code | Meaning |
|---|---|
| `read_only_server` | a read-only MCP server takes no action that writes |
| `supervisor_only` | only the run's supervisor session takes this |
| `relay_to_person` | the run does not take this from its supervisor; relay it to the person |
| `not_this_caller` | the run does not take this from this caller |
| `agent_channel` | a person-only gate refuses MCP |
| `run_step` | a person-only gate refuses a run step |
| `no_terminal` | a person-only gate refuses the CLI without a terminal |
| `keyless_dashboard` | a person-only gate refuses a dashboard without keys, and a dashboard without keys gives no publish consent (its `skip` still goes through) |
| `bearer_key` | a person-only gate refuses the HTTP API with a bearer key |
| `run_step_own_run` | a run step never decides a gate or answers a question of its own run tree |
| `passkey_required` | a passkey is enrolled on the machine, so a person-only gate or an approval decides only with a fresh assertion through the dashboard |

### Approval refusals

A node whose approval check fails finishes failed with the output
`<code>: <detail>`; a failure route handles it like any failed node (an
author loops back to the producer and the gate for a fresh approval).

| Code | When |
|---|---|
| `approval_missing` | the gate has no approving decision whose binding covers every node in `of` |
| `approval_stale` | a required node ran again after the approval, or with `live: check` an approved file changed (checked when the visit is admitted and again as each execution of the node begins: a resume, a deferred retry, a list item) |
| `approval_snapshot_unreadable` | a bound snapshot no longer verifies (also ends the run when found while consuming the decision) |
| `approval_incomplete` | a bound snapshot is incomplete: what was not stored was not approved |
| `approval_unverifiable` | with `live: check`, an approved file was stored redacted, so the live file cannot be compared |
| `approval_mismatch` | publish only: the commit to push is not the commit the approved result was taken at, or the approved result recorded no commit to compare with |

### Older apb binaries

A run whose definition uses `approves`, `requires_approval` or
`person_only` journals a `run_guard` record after every checkpoint and
records `execution.mode: cli+guard` (or `host+guard` for a host run) in
its write-once manifest. Every apb from 0.23 on refuses to resume such a
run, naming the execution mode with its "written by a newer apb" message,
writes nothing, and still reads the run and stops it; a pause of a run
whose journal carries the guard is refused by that guard as it was
before this change, so an older apb keeps the pause refusal; this apb
reads, resumes, pauses and stops the run as before. The stop limit is
closed for binaries from 0.23 on: an older apb that *stops* such a run
leaves its own stop as the last checkpoint with no guard after it, but
the manifest guard still stands, so its next resume still refuses. The
manifest guard follows the start definition only: a supervisor patch
that adds `person_only`, `approves` or `requires_approval` to a running
run journals the guard, but the write-once manifest cannot be rewritten,
so for that run the old stop-then-resume limit stays open. What
older ones still do: an apb older than 0.23 does not know the execution
block and ignores it, so it can still resume such a run past its gates;
and an older apb that *starts* such a playbook drops the three fields
and runs plain gates. Every apb on a machine that runs these playbooks
must be this release or newer; `apb doctor` warns about other `apb`
binaries on `PATH`.

## Passkeys on person-only gates and approvals

A channel refusal proves a caller type, not a person: whoever holds a
server key can sign in to the dashboard and decide. When at least one
passkey is enforced on the machine, apb raises the bar: a person-only
gate's decision and an approval (a gate with `approves` or
`requires_approval`, and a publish consent) then need a fresh WebAuthn
assertion, made through the dashboard and bound to the run, the gate, the
request and the chosen option. Without a passkey enrolled, behavior is
exactly as before; `apb doctor` shows a warning row recommending
`apb passkey enroll`. When the store exists but does not read, nothing
falls back: the requirement stands and every person-only decision refuses
until the file is fixed (fail closed), and `apb doctor` reports it as a
failure. Ordinary review gates are unchanged.

**What apb stores.** Passkeys live per machine in
`<config_dir>/passkeys.yaml` (0600, atomic write through
`apb_core::fsutil`): the credential id, the COSE public key, the sign
count, a label and the created time. Nothing secret is stored and nothing
secret is logged; the authenticator's private key never leaves it.

**The ceremonies.** apb-server verifies registration and assertion itself
(ES256 and EdDSA, attestation `none`, `userVerification` required). Its
challenges are single use, live in memory for two minutes, and the origin
and RP ID are checked against the dashboard's own origin
(`server.public_base_url`, else the request host on http). A dashboard at
an IP literal cannot use passkeys: passkeys cannot bind to an IP address,
and apb refuses it with a clear message. Verification uses `p256`
and `ciborium` (both MIT or Apache-2.0) and `ed25519-dalek`
(BSD-3-Clause); no
`webauthn-rs` (its license is not compatible).

**Enrollment and management.** `apb passkey enroll` (a person only,
refused inside a run step like the other person-only commands, and only
at an interactive terminal) prints a one-time code and asks the person to
type it back; only then is the code written, so a piped stdout leaks
nothing usable. The code is valid five minutes, and the dashboard's
Passkeys page asks for it. Every later passkey is added through the
dashboard with an assertion from an existing one. `apb passkey list`
shows them; `apb passkey remove <label>` removes one, and every removal,
last or not, needs a one-time code typed back at the terminal (the same
rule the dashboard enforces with an assertion), under a store lock, so
two removals cannot race the count down to zero. Enrollment and removal
journal nothing into runs, but each sends a notice to the machine's
notify target when one is set ("a passkey was added on `<machine>`").

**Deciding.** With a passkey enforced, `apb review` (and `apb publish
approve`) prints the dashboard link and refuses with `passkey_required`
(a new code in the closed `CallerRefusal` list); the person decides on
the dashboard, which runs `navigator.credentials.get` and posts the
assertion. MCP is refused as before, and a bearer-key request is refused
even when it carries a valid token: a key-made client is never a person.
The engine stamps the credential id prefix (`passkey`) and the full
assertion (`passkey_proof`) on the consumed channel entry, and
re-verifies the proof against the store when it consumes the decision:
the challenge is derived from the run, the gate, the decision, the
request and the nonce, so a hand-written line with a made-up prefix, or
a proof replayed onto another request, is refused, and `apb doctor
--run` names it. What the proof does not re-check is the browser's
origin (the engine cannot see the request's Host); that check happens
where the assertion is first accepted, in the server. An unattended
run's declared `decide` and a scenario run's script are exempt: they are
not a person's consent anyway, and V156 already refuses them on a
person-only gate.

**What the gate buys.** An agent that reads the config dir and the
dashboard key file can sign in to the dashboard but still cannot decide:
it cannot sign an assertion with the authenticator's private key, which
never leaves it, and a `passkey` stamp it writes into the channel file
by hand carries no assertion, so the drive refuses it.

**What the gate buys.** An agent that acts through apb's surfaces cannot
decide a passkey-required gate: the CLI, MCP and an HTTP caller with a
key are refused, and a bearer-key request is refused even with a valid
assertion token. A hand-written channel line, whatever fields it forges
(`passkey`, `channel`, even `by`), carries no assertion, and the drive
re-verifies the proof against the store before it consumes anything: on a person-only gate nothing at
all is exempt for the live drive (its own definition decides, never a
re-read of the run directory; a resume executes the run's snapshot, and
one that no journaled digest accounts for is recorded as
`definition_mismatch`, a detached driver refuses it), and a
`by: model` line is bound to the exact request it answers, which must be
the visit whose recommendation the engine enforced. Honest about the
residual: on a bound gate that is not person-only, the `by` exemption
for the engine's own decision is corroborated from the run directory
itself (the run's manifest, the definition snapshot and the journal),
which a writer of the run directory can alter, so such a writer can
still decide a bound, non-person-only gate; the passkey gate does not
claim otherwise. A run step whose isolation sandbox keeps
`<config_dir>` out of reach cannot enroll or assert at all.

**Limits.** The server cannot tell a browser from a plain HTTP client,
so whoever holds a valid credential can assert with a script instead of
a browser; that is no loss, the assertion is the proof either way. Out
of scope, and documented as such: a same-user agent that drives the
person's own browser through remote debugging, a virtual authenticator
plugged in during the person's own enrollment, or a process that
captures the enroll code from the terminal pane (piped stdout, a herdr
or tmux pane) and races the person to `enroll/start` once the code is
live; the code is inert without the typed confirmation, so the race wins
only during the person's own enrollment window. Enrollment is the
protected step (a person at a terminal, typing the code back). The store
itself has no tamper proof, and a same-user writer of `<config_dir>`
stays out of scope, like for every other apb guard: appending a record
with its own key into `passkeys.yaml` directly enrolls it with no
notice (the notify notice covers only enrollments and removals through
apb's own surfaces, and no run journal records either; `apb passkey
list` is the one view of what is enrolled), and deleting the file reads as "no passkey enrolled", which
switches the whole requirement off silently; the only trace is the
`apb doctor` warning row that recommends enrolling again. Nothing inside
the same uid can be made tamper-proof against such a writer: it can
replace the `apb` binary, the dashboard assets and the installation key
too, and a MAC on the store keyed with the installation key would be
re-signed by an apb-aware writer, since the key sits beside the store.
What the feature honestly claims is the paragraph above: apb's surfaces,
the run files and the sandbox boundary, never the config dir against its
own user. The design is
`docs/superpowers/specs/2026-11-04-passkeys-design.md`.

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

**Runs started from a run step.** A start a run step makes (an agent, a
script or a host task's subagent calling `apb run` or MCP `playbook_run`,
`playbook_trial` or `playbook_execute_plan`) records its starter in the
new run's launch record as `started_by`: the starting run, its workspace,
the node, the attempt number, whether it was verified and the chain
depth. It is verified only for a run this installation created, not ended,
whose open attempt (or script visit) the step's token binds to; the token
itself is never stored. A chain of such starts is bounded at 5 runs, across
projects: a sixth is refused `start_chain_too_deep` on every surface. A
resume is never refused for its chain, and a sub-playbook child is not
marked. A step of a run started with `external_data: true` starts a run
with the flag too; an explicit `false` there is refused
`external_data_immutable`. Nothing is written into the starting run: its
surfaces list the runs its steps started in the same project from their
records.

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

## External data runs

A run started with `external_data: true` (`apb run --external-data`, MCP
and HTTP `external_data`, the Run dialog's "External data" box) processes
data from outside the operator and keeps nothing for learning; see
[EXTERNAL-DATA.md](EXTERNAL-DATA.md). It adds no check to a start beyond
these three refusals, each answered before anything is written:

| Code | When |
|---|---|
| `external_data_remember_refused` | the start asks to remember its input (`remember: true`, `--remember`), also when it inherits the flag from a `continued_from` predecessor (HTTP 409) |
| `external_data_immutable` | a resume asks for another value than the run's own, or a `continued_from` successor of a flagged run passes `external_data: false` (HTTP 409) |
| `external_data_run` | a tool that learns from one run is pointed at a flagged run: a scenario capture, a judge example or the judge's run values, a forward patch, run memory from a step of the run; or `current`, `apb version use` or an unflagged start names a version a flagged run's patch made |

The flag is a record of where the data came from, not a sandbox: it never
weakens trust, consent, the launch policy or billing, and it does not reach
what an agent CLI keeps by itself.

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
