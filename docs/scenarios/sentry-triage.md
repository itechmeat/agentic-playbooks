# Sentry triage

Kind: code work. Runnable example: examples/playbooks/sentry-triage.yaml.

## 1. Purpose

A stream of external reports (Sentry issues) becomes tracked work (GitHub
issues) plus one chat summary. The scenario stands for list-to-list triage
with side effects: read a list, judge each item, write one downstream record
per selected item, then notify once. It is the smallest scenario where a
retry can duplicate an irreversible write.

## 2. Initial state of the world

- A Sentry project with 12 unresolved issues: 5 of them report 4 distinct
  real bugs (one bug is reported twice, as 2 separate issues), and 7 are
  noise (known flaky errors).
- The GitHub repository has none of the 4 filed yet.
- A Telegram chat exists for the summary.

## 3. Steps

1. Fetch the unresolved issues from Sentry. (agent node, connector read:
   sentry list_issues)
2. Decide which are real bugs worth filing; skip noise and duplicates; write
   a proposed title and body per selected issue. (agent node, judgment only)
3. Create one GitHub issue per selected item, each body carrying the Sentry
   permalink. (agent node, connector write: github create_issue, bounded by
   max_calls)
4. Post one summary to Telegram. (agent node, connector write: telegram
   send_message)

## 4. Expected outcome

Exactly 4 GitHub issues exist afterwards, one per real bug, no issue filed
twice for the duplicate pair, every issue body carrying its Sentry permalink,
and one Telegram message summarizing the triage.

## 5. Failure case

The filing node's attempt dies with a network error after filing 2 of the 4
issues. The attempt is retried; the retry sees the same input list and files
all 4 again, and the repository ends with the first 2 duplicated.

- What apb must show: which issues were already filed before the retry, so
  the operator can see the duplication and the run report can name it.
- What must NOT happen: a retry after a network error filing the same Sentry
  issue twice; the duplicate pair in the input must not become a duplicate
  pair in GitHub.

## 6. Today

examples/playbooks/sentry-triage.yaml covers the happy path as written. The
gaps:

1. The whole list goes to one agent call: fetch, triage and filing each
   handle all issues in a single attempt, so there is no per-item record the
   engine holds.
2. No check before filing: that an issue does not exist already is prompt
   discipline, not a read-only look at GitHub, beyond apb's lookup of its own
   earlier sends in the same run.
3. max_calls budgets are hand-tuned to the expected list length and clip a
   longer list silently.

Closed within a run: each issue is filed with `--intent` named after its
Sentry shortId, recorded in the run's send ledger and stamped with a hidden
marker. A retry after the network error answers the 2 filed issues
`already_sent`, and for a filing whose answer was lost it reads the token's
account and the recent issues, finds the marker and returns the existing
issue instead of filing it again (engine test
`a_github_comment_with_a_lost_answer_is_found_by_its_marker_and_not_posted_again`
in crates/apb-engine/tests/suite/send_intent_test.rs, and the `create_issue`
reconcile cases in connectors/github/tests.yaml). A re-run as a new run does
not see them.

## 7. Target

The list is a first-class value and the filing runs per item, keyed by the
Sentry shortId, also across runs. Before each file a read-only check asks
GitHub whether the issue exists; after an interruption, or in a re-run, the
ledger of filed keys is read back and what is done is skipped. The run report
shows filed, skipped and failed counts per item. A feature closing this gap
carries an engine test that interrupts the filing after two items and asserts
no duplicate issue on a re-run.
