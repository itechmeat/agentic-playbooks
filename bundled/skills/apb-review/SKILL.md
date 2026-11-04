---
name: apb-review
description: Independent, role-based, read-only review of a concrete change against its original request, with verified findings and at most two repair rounds. Use for substantive changes, review findings or an explicit review request. It grants no authority to merge or publish.
metadata:
  version: "1.3.0"
---

# Review a change

Review the actual candidate against the original request and its
expected-result card. A review is evidence that a review ran. It is not
acceptance.

## 0. Depth by risk

Pick the depth before the roles, from what the change can break:

- **Lightweight:** prose or a mechanical change with no risk trigger that
  adds or changes no test. The project's native checks, the installed
  review helpers (section 3a) and one diff reading by a reviewer in a
  context that did not write the change. Report it as a lightweight
  review, not as a full one.
- **Standard:** a substantive change with no risk trigger. The roles of
  section 1.
- **High risk:** the change hits a risk trigger (persisted data or formats,
  a public API or CLI contract, security or secrets, concurrency or
  recovery, the release or check machinery, a guarantee other code relies
  on). The roles of section 1, a role that covers each trigger hit, and the
  protected boundary listed before the review starts.

File count alone neither sets nor lowers the depth. A lower depth needs a
written reason based on consequences, and never waives a check the project
requires.

## 1. Pick the roles

For a substantive change, use these roles:

- **Correctness:** request fidelity, edge cases, API and state contracts,
  error handling.
- **Security:** permissions, secrets, injection, unsafe input, recovery
  boundaries.
- **Tests and maintainability:** test value, behavior coverage, regression
  evidence, readability, coupling.
- **Native checks:** the project's own build, tests, lint and type checks.
- **Review helpers:** every review or check helper installed on this machine
  (see section 3a).

Every role reads the project's agent instructions (the instruction files
at the repository root and the decision records they point to) and checks
the diff against the invariants and conventions they state. A new piece of
code that duplicates a hardened helper the project already has (file
opening, locking, process spawning, redaction) is a finding.

A fix for a disclosure or authorization defect is checked at every boundary
where the protected data leaves or is kept: every surface, cache, stored
copy and derived text, not only the consumer the finding named. The
reviewer lists those boundaries and says for each whether the fix holds.
Prefer a fix that makes the unsafe use impossible (a type or a refusing
API) over one that patches call sites. Agree the protected boundary before
the fix; widening it later turns one finding into a long chain.

Two roles always run their audit skill, on every standard or high-risk
review:

- The tests role runs the apb-test-audit skill in audit mode on every test
  the change adds or changes, and reports behavior the change makes without
  a test.
- The security role runs the apb-security-audit skill on the whole diff.

A general reviewer does not replace them. When the skill is not available to
the executor, the role reports a capability gap; it never reports clean.

The project may add roles. A risk trigger (see apb-delivery) needs the role
that covers it.

Partial role coverage is reported as partial. It never certifies the roles
that did not run.

## 2. Resolve each role to an executor

- Follow the user's model-selection guidance, the apb-model-selection skill,
  or the apb profiles of the playbook (`apb-reviewer` for reviewers).
- Use an executor that differs from the implementer. When a model of another
  family than the implementer's is available, a review round by that family
  is required (apb-model-selection, section 5).
- Fallback chain: another available family, then another agent of the same
  family, then a fresh clean-context session or subagent that gets only the
  review packet.
- A usage limit is a routing signal, not a stop. Do not wait without a bound.
- Record the actual executor and rung. A configured model is a request, not
  proof of what ran.
- Record each reviewer's and each audit's cost line: the figure or "no
  figure", its basis (billed, subscription equivalent and not billed, or
  basis unknown) and the class of login the executor loaded (an api key, a
  subscription login, a cloud account, or unknown). Take the basis from the
  run's usage when apb ran the role, never from a guess, and never write a
  secret value.

## 3. Run the reviewers

- Each reviewer starts in a clean context with only the packet: the card, the
  design, the diff and the source. No implementer reasoning, no other
  reviewer's conclusions.
- Hand a reviewer the code, not your conclusion. A reviewer told what you
  expect it to find tends to confirm it.
- Reviewers are read-only. They never edit files or run checks that change
  the tree.
- Missing review capability is a failure, never a clean result.

## 3a. Run the installed helpers

A review or check helper installed on this machine is a required step of
every review cycle, not an option. Installed helpers are the ones the setup
plan lists with `required: true` in its `suggestions`, and the ones the
project declares among its checks.

- Run each one on the candidate in every review round and fold its output
  into the findings, each verified like any other finding. The round's
  report lists each helper as run or failed; a round without them is not
  complete.
- Checks and helpers run code and configuration that come from the
  candidate (build scripts, tests, a helper's settings in the repository).
  For a change by an author the person does not trust, run them only inside
  a sandbox (an isolated apb node); otherwise report them as not run, with
  that reason. Never run them with the person's credentials just because
  this section asks for them.
- A helper that fails to run (crashes, refuses its configuration, needs a
  login, times out) is a blocking failure: report it with its error. The
  reviewer does not repair it; the implementer or the person fixes it
  before the review counts as done. Do not fall back silently to a review
  without it, and do not mark its role as covered.
- A helper that is not installed stays optional. Mention it; never install
  one without the person.

## 4. Classify and verify findings

Every finding gets one class:

1. **Confirmed defect:** wrong behavior, shown against the code.
2. **Design concern:** a concrete consequence, not taste.
3. **Verification gap:** a claim with no evidence, or a check that did not
   run.
4. **Optional:** a suggestion with no consequence.

- Verify each finding against the actual source before acting on it.
- Each finding gets a stable id, a location, why it matters, and evidence.
- Dispose each one: fixed with evidence, rejected with a reason, or open.
- Never relabel a material risk to claim readiness.

## 4a. Evidence before claims

- A claim of "passes", "clean" or "fixed" names the check, the candidate it
  ran on (commit, tree or file set) and its result. A claim with no evidence
  is a verification gap, never a pass.
- Evidence holds only for the candidate it ran on. A fix, a rebase or a
  changed base makes the affected evidence stale: rerun it or say it is
  stale. An unchanged patch on a changed base is a new candidate.
- A clean review names what it read and what it did not read. Silence on a
  part of the diff is not a pass for that part.
- A reviewer's output proves that a review ran. Whether it was adequate
  needs its scope, findings and evidence read by the integrator.

## 5. Converge

- One initial review, then at most two repair rounds.
- A repair round fixes the confirmed findings and reruns the affected checks.
  Then the affected roles review again with fresh context.
- Recheck what a change affected. Do not rerun everything, and do not keep
  old evidence without a reason it is still valid.
- A repair round judges the fixes and what they touched. It does not reopen
  findings already rejected with a reason, unless new evidence appears.
- A regression a fix introduces counts toward the bound.
- After the second repair round, or after repeated regressions, stop and
  reconsider the approach with the task issuer. Do not loop again.

## 5a. Clean up

Checks and helpers leave scratch output behind: temporary directories, logs,
build output outside the project's usual build directory, servers or browser
sessions started for a check. Before you report, and also when the review
stops early because a check or helper failed, delete what this review
created under one parent directory named for it and stop the processes it
started. Delete only your own, never by a wide pattern: other sessions may
share the machine. Findings keep their evidence as text, not as files left
behind.

## 6. Report

Use [references/findings-template.md](references/findings-template.md).

Keep these apart: implemented, reviewed, ready, integrated. Publishing follows
the parent process and the host's policy. This skill grants no authority.
