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

1. Load the list from the CSV, row by row, unchanged. (agent node, read-only)
2. Check every row against the mailbox for a payment confirmation naming its
   invoice; verdict paid or unpaid per row, with evidence. (agent node, a
   check per item: imap search_messages)
3. Send one personal reminder per unpaid row; address the debtor by name,
   name the invoice, the amount and the due date; nothing to paid rows.
   (agent node, connector write: smtp send_email, one message per item)
4. The goal criteria check the three count markers (loaded, checked, sent);
   no-double-send and nothing-to-paid are a reader's criteria. (goal
   criteria)

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

examples/playbooks/bulk-reminders.yaml expresses the three steps and the
markers. The gaps:

1. The whole list goes to one agent call per step; there is no per-item node
   and no per-item verdict the engine holds.
2. max_calls budgets are hand-tuned to the expected list length; a longer
   list clips silently mid-run.
3. A retry after a timeout may send twice: whether a row was sent lives only
   in the session memory of the attempt.
4. A resume after a pause replays the send step from the start; there is no
   per-item sent ledger to skip against.
5. The counts (loaded, checked, sent) are agent-written text; the engine
   cannot assert sent equals unpaid minus failed, so an arithmetic lie in
   the report would pass the markers.

## 7. Target

The list is a first-class value; each row flows through per-item steps with
per-item verdicts (sent, skipped-paid, failed) recorded by the engine; every
send is keyed by the invoice number as an idempotency key, and a retry or a
resume reads the ledger and sends only the remainder. The run report's
counts are engine-checked (sent plus failed equals unpaid). A feature closing
these gaps carries an engine test that interrupts after 9 sends and asserts
the resume sends exactly the remaining rows and nothing twice.
