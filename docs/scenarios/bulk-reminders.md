# Bulk reminders

Kind: routine. Runnable example: examples/playbooks/bulk-reminders.yaml.

## 1. Purpose

A list of overdue invoices, one personal reminder per item: check each item
against the mailbox for a payment that arrived after the export, then send
the ones still unpaid. The scenario is the canonical retry-safety case for
routines: every reminder is an irreversible external write, the list is
long enough that a mid-list failure is likely, and a resume after a pause
must not send anything twice.

## 2. Initial state of the world

- A CSV file with 24 rows: email, name, invoice number, amount, due date.
- 20 of the 24 are still unpaid; 4 were paid after the list was exported; 2
  of the unpaid addresses will bounce.
- The mailbox folder holds payment confirmations for the 4 paid ones, none
  for the rest.

## 3. Steps

1. Load the list from the CSV, row by row, unchanged, keyed by invoice
   number. (agent node, read-only, declared output `rows`)
2. Check every row against the mailbox for a payment confirmation naming its
   invoice; verdict paid or unpaid per row, with evidence. (list node over
   `rows`, connector read: imap search_messages, an `output` item check
   that a row whose search did not run needs a look instead of reading as
   unpaid)
3. Send one personal reminder per unpaid row; address the debtor by name,
   name the invoice, the amount and the due date; nothing to paid rows.
   (list node over the checked rows filtered by `paid`, connector write:
   smtp send_email, one message per item, a judge item check on each
   message)
4. The goal criteria count both lists through the engine's item states;
   that the load step read every row of the file stays a reader's
   criterion. (goal criteria `type: list` and `type: manual`)

## 4. Expected outcome

18 reminder emails were delivered, one per unpaid invoice with a working
address, each personal to its recipient; the 4 paid invoices received
nothing; the 2 bounces are named in the run report with their errors; the
counts add up: 24 loaded, 24 checked, 4 paid, 20 unpaid, 18 sent, 2 failed.

## 5. Failure case

The send step times out after 9 sends. The infrastructure retry resumes the
same session; the agent continues and may or may not remember exactly which
9 went out. Worse, the operator pauses the run and resumes it an hour later:
the send step re-runs from its input list, and the first 9 recipients get a
second reminder.

- What apb must show: which rows were already sent before the retry or the
  resume, so the continuation skips them, and the operator can see the
  ledger.
- What must NOT happen: a retry after a network error posting the reminder
  twice; a resume sending to a row the earlier pass already sent to; a paid
  invoice receiving a reminder at any point.

## 6. Today

examples/playbooks/bulk-reminders.yaml loads the file once and checks and
sends once per row, as list nodes:

```yaml
- id: send_reminders
  type: agent_task
  connectors:
    - { name: smtp, accounts: [default], functions: [send_email], max_calls: 2 }
  list:
    items:
      from: { node: check_payments, field: items }
      where: { state: passed, field: paid, equals: "false" }
      max_items: 500
    batch: { size: 10, parallel: 2, retries: 0 }
    item_outputs: [subject, body_excerpt]
```

Where that leaves the gaps the first version had:

1. Closed: each row is one item with its own verdict the engine holds. The
   payment check and the reminder run per row; the send step reads the
   check step's own items, filtered to the rows whose check passed and
   that read unpaid, so a paid row, or one whose search could not run,
   never reaches it.
2. Closed: each row's step has its own small `max_calls`, so no budget is
   tuned to the list length. The list's size is bounded by `max_items`, and
   a file with more rows fails the step before anything is sent instead of
   clipping mid-run.
3. Still a person's call, now per row: a reminder whose answer was lost is
   recorded as an unknown send of that row. The row needs a look
   (`unknown_send`) and is never sent again on its own, by a retry, a
   resume or a supervisor retry, until a person resolves the send with
   `apb intent resolve`; the other rows carry on.
4. Closed: the counts are the engine's. The run report, `run_status` and
   `apb runs <id> --items` show checked, passed and needing-a-look rows per
   step, and the goal's `type: list` criteria fail while a row is
   unprocessed or needs a look, so a report cannot claim more than was
   checked.

Within a run, each row's sends are keyed by the node and the invoice, so an
infrastructure retry, a resume after a pause and a supervisor retry repeat
only the rows that did not pass, and a row that passed is never sent again
(engine tests
`a_drain_pause_finishes_the_running_item_and_the_resume_runs_the_rest` and
`an_unknown_none_send_is_never_resent_until_a_person_resolves_it` in
crates/apb-engine/tests/suite/list_resume_test.rs, and
`a_retry_after_a_failed_visit_is_answered_and_a_loop_after_success_sends_again`
in crates/apb-engine/tests/suite/send_intent_test.rs). A fresh run of the
same list does not see the earlier run's sends.

## 7. Target

The list is a first-class value; each row flows through per-item steps with
per-item verdicts (sent, skipped-paid, failed) recorded by the engine; every
send is keyed by the invoice number, and a re-run as well as a retry or a
resume reads the ledger and sends only the remainder. The run report's
counts are engine-checked (sent plus failed equals unpaid). A feature closing
these gaps carries an engine test that interrupts after 9 sends and asserts
the resume sends exactly the remaining rows and nothing twice.
