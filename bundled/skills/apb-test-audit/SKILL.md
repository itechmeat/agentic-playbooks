---
name: apb-test-audit
description: A value gate for tests. Use before writing or changing any test (authoring mode), and on every test a diff adds or changes when reviewing it (audit mode). A test earns its place only when it protects a distinct, meaningful behavior and fails when that behavior breaks.
metadata:
  version: "1.0.0"
---

# Audit tests

A test is code that someone maintains forever. It is worth that cost only
when it guards behavior a person relies on and goes red when that behavior
breaks. This skill has two modes with one bar.

Use the project's own test commands, the ones its instructions, build files
or task runner name. Do not invent a runner, and do not change the project's
configuration to make a test pass.

## Authoring mode

Run this gate before you add or change a test. Each question needs an
answer; without one, do not write the test yet.

1. **Behavior.** Which observable behavior, invariant or public contract does
   it protect? Name it in one sentence a user of the code would recognize.
2. **Failure.** Which realistic change to the code makes it fail? If no
   plausible mistake turns it red, it protects nothing.
3. **Distinctness.** Is that failure already caught by an existing test?
   Each contract has one main test at the boundary that owns it. A second
   test at another layer needs a risk the first cannot reach. Prefer a new
   case in an existing table or fixture over a near copy.
4. **Seams.** Does it need anything production does not: an export, a flag,
   a hook, a wrapper used only by tests? Then test at the real boundary
   instead. Production code never grows a path that exists for tests.

Then hold the test against these rules:

- **Bug fixes.** The regression test must fail on the code before the fix,
  for the reason the bug describes, and pass after it. Run it both ways and
  keep the evidence. A test that was never seen failing proves nothing about
  the fix.
- **Independent expectations.** Expected values come from the requirement, a
  worked example or a known fixture, never from calling the code under test
  or copying its output.
- **No tautologies.** No assertion that compares a value with itself, restates
  a constant, or passes whatever the code does.
- **No self-testing doubles.** A fake or stub may stand in for a dependency.
  It must not implement the behavior the test asserts, or the test checks the
  fake.
- **Behavior, not structure.** A refactor that keeps behavior must keep the
  test green. A test that breaks when a private function is renamed or calls
  are reordered (where order is not observable) is coupled to the
  implementation.
- **Honest names.** The name promises exactly what the assertions check.
- **Negative cases fail for the right reason.** A rejection test asserts the
  specific refusal, not just that something failed.

## Audit mode

Use it when reviewing a change: every test the diff adds or changes, and
every behavior change the diff makes without a test. Read-only: report, do
not edit.

For each test, read the whole test, the production code it exercises, and the
nearby tests that cover the same thing. Then classify what you find:

| Class | Meaning |
|-------|---------|
| `tautological` | Passes regardless of behavior: self-comparison, restated constants, expectations computed by the code under test, a double that implements the asserted behavior |
| `low_value` | Guards nothing a user relies on, or no realistic mistake makes it fail |
| `implementation_coupled` | Breaks on a behavior-preserving refactor |
| `duplicative` | Repeats a contract another test already owns, with no new risk |
| `over_promising_name` | The name or fixture claims more than the assertions check |
| `test_only_seam` | Production code gained an export, flag or path only tests use |
| `missing_test` | The diff changes behavior and no test would fail if that change were reverted |
| `unproven_regression` | A bug-fix test with no evidence it failed before the fix |

Every finding has:

- a location: file, test name, and line or range;
- the class;
- why it matters: the regression it lets through, or the cost it adds;
- evidence: the assertion, the code path, or the run that shows it;
- a recommendation: rewrite at the owning boundary, merge into an existing
  case, delete with the stronger test named, add the missing test with the
  behavior it must pin, or show the pre-fix failure.

Do not recommend deleting a test until you have named what still guards its
contract. Slow or old is not a reason. A test that fails on the base code may
have found a real defect: report it as such.

## Report

List the findings, then one line per test that passed the bar. Say which
tests you ran and their result, or that you ran none and why. A test you did
not read is not covered.
