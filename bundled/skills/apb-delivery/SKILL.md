---
name: apb-delivery
description: Deliver an authorized feature, bug fix or refactoring from the original request to a reviewed, checked and reported change. Use for product code changes. Inside an apb playbook, run only the stage the node names. Research and design-only tasks stay local and unpublished.
metadata:
  version: "1.2.0"
---

# Deliver a change

Finish the requested outcome and every authorized next step. Do not stop at a
plan or a green unit suite. Do not narrow the task silently.

Inside an apb run, do only the stage your node names. The playbook owns the
order, the checks, the retries and the resume. See
[references/stages.md](references/stages.md).

## 1. Intake: the expected-result card

- Read the original request, the issuer and the reply route, existing work,
  the project instructions, its checks and its CI.
- Write the card in the existing task record, or in the artifacts of the
  project's spec workflow when it has one:
  - observable expected outcomes, taken from the original request;
  - exclusions;
  - behavior that must keep working;
  - assumptions, stated as such;
  - the reference to the original request.
- Never rewrite the card to match the code. A scope change the issuer
  authorized stays explicit and attributed.
- Find code facts yourself before you ask the issuer.

## 2. Risk triggers

Independent review of the risk is required when the change touches:

- persisted formats or stored data;
- a public API or CLI contract;
- security, authentication or secrets;
- concurrency, crash recovery or resume;
- the release pipeline or the check machinery;
- a guarantee other code relies on.

A project may add triggers or size thresholds. File count alone neither sets
nor lowers risk. Lower coverage needs a written reason based on consequences.
Never waive a check the project requires.

## 3. Change kind

- **Feature.** One design pass with the apb-brainstorm skill, or reuse of a
  current advised design.
- **Bug fix.** A failing observation first, then a causal explanation, then
  the fix. Show the original scenario passing and a regression check.
- **Refactoring.** State what must be preserved and the realistic benefit.
  Show preservation with tests, characterization or comparisons.
- A behavior change hidden inside a refactoring or fix is not allowed. Name
  it and give it the feature obligations.
- Details: [references/change-kinds.md](references/change-kinds.md).

Before the first commit, read the repository's commit rules (sign-off,
commit message format, signed commits) and follow them in every commit of
the delivery.

## 4. Decompose a large change

- After design, check the decomposition triggers: more than about five card
  outcomes, a change across several layers, a persisted-format change
  together with behavior, or a result that cannot be reviewed in one pass.
- When one holds, split the change into independently verifiable subtasks,
  each with its part of the card, its own checks and its dependencies in
  order. A subtask is a unit of work and review, not a pull request.
- Each subtask gets its own review with the apb-review skill and one
  self-check repair round. The full review panel then reviews the
  integrated result with at most two repair rounds. The final check runs
  against the whole original card.
- After the second repair round, later checks look only at the delta of
  each fix. A remaining finding outside the agreed boundary goes to the
  issuer as a scope decision, recorded in the card, not as another round.
- When the project uses a work tracker (issues, a graph tracker or
  similar), the subtasks and their status go there. Otherwise they go into
  the task record.
- A pull request stays within the size an external reviewer the project
  uses can read (for example a review bot's file limit). A delivery larger
  than that goes out as several pull requests in dependency order, each
  green on its own. Count the files after the first shared subtask, not at
  the end. The size limit never justifies dropping a test: split instead.
- A protective feature (disclosure, authorization, isolation) states its
  threat model and protected boundary in the card before it is built.
  Reviewers judge findings against that boundary and mark the ones outside
  it as such.
- Details and where the breakdown goes:
  [references/decomposition.md](references/decomposition.md).

## 5. Acceptance before code

- Define how each outcome will be observed before the code that depends on it.
- Keep an evidence map: every card outcome names the check that proves it.
  The check drives the shipped entry point (the real binary, process,
  route or tool the person uses), not an in-process stand-in, and it fails
  when that outcome is broken (shown failing first, or by a named mutation).
  An outcome without such a check is not delivered.
- When the change alters a shared mechanism (scheduling, state, retries,
  locks, a stored format), list in the preserved behavior how it meets
  every existing kind and state that uses the mechanism. Each pair gets a
  check or a written reason why it cannot interact.
- For a substantial criterion, ask an independent role which wrong
  implementation could still pass. Combine this with design or review.
- Test-first is a choice, not a rule. Implement end-to-end slices through the
  real entry points.
- A UI claim needs real interaction with the affected UI.

## 6. Check and review

- Run the project's own checks (build, tests, lint, type checks) through the
  real interfaces, on the actual candidate.
- Gate every test you add or change with the authoring mode of the
  apb-test-audit skill before it lands.
- Review with the apb-review skill. Its tests role always runs the
  apb-test-audit skill on every added or changed test, its security role
  always runs the apb-security-audit skill on the diff, and every installed
  review helper runs as a required step. A general reviewer does not replace
  these. Failed, unknown, skipped or stale evidence is not a pass.
- Choose the executors with the apb-model-selection skill: a review round by
  another family is required before integration when one is available.
- A usage limit routes the review to the next rung of the fallback chain. It
  never stops the task.

## 7. Integrate

- **Publishing** (push, pull request) happens only within the authority the
  user or the project grants: the project instructions and the apb
  settings. When it is granted, publish after fresh required checks and a
  converged review. When it is not, stop at a local, reviewed candidate and
  report it.
- **Merge** follows the project's policy. Without a stated policy, ask.
- **Tags and releases** follow the project's policy.
- Local commits on a feature branch are allowed during the work. Never commit
  on the shared main branch.
- **Research or design-only work** stays local: no commit, push, pull request
  or release.
- apb's own publish consent still applies. Never bypass it with a manual push
  or an invented approval.
- Other external effects keep their own authority.

## 7a. Clean up

Before you report the delivery done, and again after a merge:

- After the merge, remove the task's worktrees, delete its merged branches
  and prune the stale worktree records (in git: `git worktree remove`,
  `git branch -d`, `git worktree prune`), with the build output directories
  those worktrees used. A squash merge leaves the branch unreachable from
  the base, so `git branch -d` refuses it: delete such a branch with
  `git branch -D` only after you checked that the base holds its work
  (a commit on the base with the branch's tree, or every one of its
  patches on the base).
- Delete the temporary and scratch files the task created: temporary home
  directories, logs, downloaded archives, built binaries, browser profiles.
  What must survive goes into a named cache directory, not a temporary one.
- Create temporary data under one parent directory named for the task, never
  in an anonymous temporary directory, and remove it when the work ends,
  also on failure (for example a shell `trap` on exit). A test that creates
  temporary data removes it in its teardown.
- Delete only what this task created. Never delete by a wide pattern: other
  sessions may work on the same machine at the same time.
- Stop the background processes, servers and browser sessions the task
  started, and only those.
- For a build cache, prune old artifacts instead of a full clean (a build
  cache pruner such as cargo sweep for Rust projects).
- A worktree or branch a pull request still needs stays until the merge.

## 8. Close and report

- Compare every expected outcome with the original request: delivered,
  partial or not delivered, with evidence.
- Say what the cleanup removed, what it kept and where, and what is left for
  after the merge.
- Put assumptions first. Add stage conformance (done, not applicable, or
  skipped with a reason), findings and their dispositions, the real
  integration state and the next action.
- Use [references/result-template.md](references/result-template.md) inside
  the existing task record or the project's spec workflow artifacts. It is
  not a new mandatory file.
- Give one cost line per role that ran: the implementer, every reviewer and
  every audit. Each line names the role, the figure, its basis (billed,
  subscription equivalent and not billed, or basis unknown) and the class of
  login the executor loaded (an api key, a subscription login, a cloud
  account, or unknown). Take the basis from the run's usage when apb ran the
  role (`cost_basis` of `attempt_finished.usage`, the split of `apb runs
  <id>` or `run_status`), never from a guess. Never write a key, a token or
  any other secret value; name the class only. A role with no reported
  figure says "no figure" and its login class.
- Never start a role on billed work, or on work whose basis is unknown,
  without the person's explicit command for this delivery. When apb
  refuses or pauses for billed use, ask; do not switch a role to another
  login to get around it.

## Measurements per change

Record: change kind, risk triggers hit, findings per review role (accepted
and rejected), repair rounds, findings that escaped to later checks, wall
time, tokens or cost when measured, each cost with its basis. Unknown stays
unknown.

## Questions and authority

- Ask the task issuer. For delegated work that is the assigning agent.
- If the issuer is unavailable, settle reversible engineering choices with an
  advisor, record them and continue. Only work that depends on an owner-only
  fact or a missing permission for an irreversible effect waits.
- Keep existing grants. Do not ask again for what was already authorized.
- If no permitted independent context exists, disclose it. Never present
  self-review as independent review.
