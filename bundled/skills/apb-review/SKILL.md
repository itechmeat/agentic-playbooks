---
name: apb-review
description: Independent, role-based, read-only review of a concrete change against its original request, with verified findings and at most two repair rounds. Use for substantive changes, review findings or an explicit review request. It grants no authority to merge or publish.
metadata:
  version: "1.1.0"
---

# Review a change

Review the actual candidate against the original request and its
expected-result card. A review is evidence that a review ran. It is not
acceptance.

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

Two roles always run their audit skill, on every review:

- The tests role runs the apb-test-audit skill in audit mode on every test
  the change adds or changes, and reports behavior the change makes without
  a test.
- The security role runs the apb-security-audit skill on the whole diff.

A general reviewer does not replace them. When the skill is not available to
the executor, the role reports a capability gap; it never reports clean.

The project may add roles. A risk trigger (see apb-delivery) needs the role
that covers it. A routine prose or mechanical change may use the native
checks and a diff reading only.

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

## 3. Run the reviewers

- Each reviewer starts in a clean context with only the packet: the card, the
  design, the diff and the source. No implementer reasoning, no other
  reviewer's conclusions.
- Reviewers are read-only. They never edit files or run checks that change
  the tree.
- Missing review capability is a failure, never a clean result.

## 3a. Run the installed helpers

A review or check helper installed on this machine is a required step of
every review cycle, not an option. Installed helpers are the ones the setup
plan lists with `required: true` in its `suggestions`, and the ones the
project declares among its checks.

- Run each one on the candidate and fold its output into the findings, each
  verified like any other finding.
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

## 5. Converge

- One initial review, then at most two repair rounds.
- A repair round fixes the confirmed findings and reruns the affected checks.
  Then the affected roles review again with fresh context.
- Recheck what a change affected. Do not rerun everything, and do not keep
  old evidence without a reason it is still valid.
- After the second repair round, or after repeated regressions, stop and
  reconsider the approach with the task issuer. Do not loop again.

## 6. Report

Use [references/findings-template.md](references/findings-template.md).

Keep these apart: implemented, reviewed, ready, integrated. Publishing follows
the parent process and the host's policy. This skill grants no authority.
