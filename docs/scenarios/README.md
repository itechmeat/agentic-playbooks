# Reference scenarios

A fixed set of eleven realistic scenarios, half code work and half non-code
routine, that apb features are checked against. Reliability features (honest
run results, list processing, retry safety) are designed against these
situations instead of against abstract ones.

Every place where a scenario is awkward or impossible to express today is a
gap in apb, and the scenario names it plainly. A gap is not a defect report;
it is the reason a feature exists, written down where the feature's test can
point back to it.

A **check** in these documents is any read-only look at the world: a connector
function call, a script, a decision-model question. A step that writes
somewhere is an effect, not a check.

## Section format

Each scenario file has the same seven numbered sections:

1. **Purpose** - what the scenario stands for, one paragraph, plain words.
2. **Initial state of the world** - concrete and checkable, so a test can
   build it: "37 open deals have no owner", "the audit lists 2 high findings
   and the suite has 2 failing tests that predate the run".
3. **Steps** - numbered, and each step names the apb capability it needs
   (agent node, connector function, script node, judge node, human review
   gate, loop edge, goal criteria, and so on).
4. **Expected outcome** - what a reader or a check observes afterwards.
5. **Failure case** - at least one: what goes wrong, what apb must show the
   operator, and what must NOT happen ("a retry after a network error must
   not post the reminder twice").
6. **Today** - what the example playbook does now, or why the scenario cannot
   be expressed today, plus the gaps: each gap one numbered line ("the whole
   list goes to one agent call", "no before/after snapshot").
7. **Target** - the expected behaviour once planned capabilities exist,
   described as intent. A sketch may show YAML only in a fenced block marked
   `not valid today`, and never names a future field as if it already
   existed.

## The set

| Scenario | Kind | Runnable example | Main gaps |
|---|---|---|---|
| [ci-failure-triage](ci-failure-triage.md) | code | examples/playbooks/ci-failure-triage.yaml | the root-cause criterion stays manual; a wrong diagnosis still reports success |
| [review-triage](review-triage.md) | code | examples/playbooks/review-triage.yaml | one review and one fix, no second review round |
| [sentry-triage](sentry-triage.md) | code | examples/playbooks/sentry-triage.yaml | the whole list goes to one agent call; a retry may file an issue twice |
| [dependency-security-fix](dependency-security-fix.md) | code | examples/playbooks/dependency-security-fix.yaml | the audit findings are not engine-held (the tests are); no per-finding loop |
| [document-review-panel](document-review-panel.md) | code | docs only | no per-item state across loop rounds; a bounded loop re-runs the whole body |
| [inbox-triage](inbox-triage.md) | routine | examples/playbooks/inbox-triage.yaml | first candidate for list processing; the search limit clips silently; an inbox gap is invisible |
| [whatsapp-inbox](whatsapp-inbox.md) | routine | examples/playbooks/whatsapp-inbox.yaml | a retry may reply twice; the batch ack is all-or-nothing |
| [release-announce](release-announce.md) | routine | examples/playbooks/release-announce.yaml | no idempotency: a re-run may create the release and re-send the email again |
| [release-heartbeat](release-heartbeat.md) | routine | examples/playbooks/release-heartbeat.yaml | "exactly one issue" is prompt discipline; overlapping runs cannot see each other |
| [bulk-reminders](bulk-reminders.md) | routine | examples/playbooks/bulk-reminders.yaml | a retry after a timeout may send twice; no per-item sent ledger |
| [crm-cleanup](crm-cleanup.md) | routine | examples/playbooks/crm-cleanup.yaml | no per-deal ledger: the engine-held count shows how many are left, not which |

Every runnable example lives in examples/playbooks/ and is validated on the
current schema by `the_demo_playbooks_validate` in
crates/apb-cli/tests/suite/demo_playbooks_test.rs. That test names the
examples explicitly, so a new example is covered once its id is added there.

## The rule

A feature that closes a gap adds an engine test for its scenario in that
feature's PR. The scenario document names the gap; the test pins the behaviour
that closes it, on the initial state the document describes. A PR that claims
a gap is closed without the scenario's test is not done. A docs-only scenario
(document-review-panel today) gets its test in the PR that first makes it
expressible.
