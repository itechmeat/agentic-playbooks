# Citations for node outputs and record packages

Status: implemented (PR 20c). Builds on result snapshots
(`2026-11-03-result-snapshots-design.md`) and approvals
(`2026-11-03-approvals-design.md`). Validator code: V159.

## 1. The boundary

The contract applies only to an `agent_task` that reads its sources through
the engine: a reading session the engine opens per attempt over stored
result snapshots, with the same bounded reader and the same citation rules
the run grader uses (`crate::citation`). Agent transcripts are not a source
of read traces: few attempts have one, and none with bounds. An agent may
still read the same files directly; its citations of them then fail the
full-read check, which is an honest refusal, not a bypass.

## 2. Record packages

1. A result entry declared `format: records` is a package: UTF-8 JSON
   Lines, the first line a header
   `{"apb_records": 1, "complete": true|false, "source": "<text>"}`, then
   one record per line `{ "id", "kind", "text", "source"?, "version"?,
   "at"? }`. `id` matches `[A-Za-z0-9._:-]{1,128}` and is unique; `kind`
   is a slug (`[a-z0-9][a-z0-9_-]*`, at most 64); at most 10000 records.
   Unknown fields are refused. The owner of the format is
   `apb_core::records`.
2. A text that does not parse is no package: capture stores it as plain
   text and records `records_invalid` on the manifest entry with the first
   problem. A problem names lines and fields only, never the text.
3. `complete` is the collector's own statement about its source and is
   shown as such ("the collector says the package is complete"). The
   engine adds its own: a package view of a snapshot that is `incomplete`
   or omitted anything is incomplete. Completeness relative to the outside
   world (all mail, all customers) is never promised.

## 3. The consumer declaration

```yaml
- id: answer
  type: agent_task
  prompt: "Answer from the collected mail"
  evidence:
    from: [collect]            # nodes whose stored snapshots are read
    kinds: [email]             # optional: the view kinds a claim may cite
    claims: required           # required | optional (default)
    on_invalid: fail           # mark (default) | fail
    budget_chars: 50000        # optional: characters one attempt may read
```

V159 (error): `evidence` on a kind other than `agent_task`; an empty
`from`; a `from` node that is not declared or from which no edge path
leads to the consumer (the consumer itself only around a loop); a `kinds`
entry that is not a slug; `evidence` with `isolation: full` (the reader
needs the run directory). Every `from` node takes a snapshot
(`snapshotted_nodes`), so it needs no `results:` of its own.

## 4. The session

1. Owner: `apb_engine::evidence` (`sources`, `session`, `check`). Before an
   attempt's prompt is built the engine resolves the latest stamped
   snapshot of each `from` node (for a list node: its node snapshot and the
   item snapshots of its latest visit) and builds the views; right after
   the attempt's `attempt_started` is journaled it writes
   `runs/<id>/evidence/<attempt_id>.json` (0600, atomic): the session id
   (128 random bits), the run id, the attempt, the bound snapshots, the
   views (id, kind, complete, the collector's statement, and per record its
   reference, the digest of its text as served and whether it is
   citable), the budget and the read state. No text.
2. Views, per bound snapshot: an `output` view and a `fields` view (one
   record each), a `file` view per plain text file (one record), and per
   package one view of kind `records`, or one view per record kind when the
   declaration names `kinds`. References: `r:<snapshot_id>/<entry>` and,
   for a package record, `r:<snapshot_id>/<entry>@<record id>` with the id
   percent-encoded.
3. Every text is served through the run redactor. A record the redactor
   changes (a secret known after capture) is served redacted and is not
   citable, so every citable span is a range of stored bytes; a record
   whose served text changes later stops being citable.
4. The handle `<run_id>:<session>` reaches the agent in an `## evidence`
   prompt section (`context::evidence_section`) and in
   `APB_EVIDENCE_SESSION`; a host task carries it in its prompt.
5. Reading: MCP `run_evidence_read { session, op, ref?, span?, view?,
   from?, count?, phrase? }` and `apb evidence read --session <handle>
   <op> ...` (JSON), the five operations of the shared reader
   (`outline`, `children`, `read`, `page`, `search`), both through one
   request builder (`apb_mcp::tools::evidence`), so their answers and
   budgets are equal. Every returned text counts against the budget. A
   caller whose `APB_RUN_ID` names another run is refused (a guard against
   mistakes, not a boundary). The session closes when the attempt's
   process is gone; later reads are refused.

## 5. Claims and the check

1. With `evidence`, the agent writes `outputs.claims` in its status file:
   `[{ "id": "c1", "text": "...", "cites": [{ "ref": "r:...", "span": [a, b] }], "absence": { "kinds": [...] }? }]`.
   Spans are character offsets into the record text as served; each
   accepted citation is kept in byte form (`...#[a,b)` over the record's
   stored UTF-8 text).
2. At the end of a succeeded attempt, before its success check, the claims
   are checked with the shared citation rules: the reference names a
   record of this session; the record is of an allowed kind; it was
   returned whole to this attempt, not only listed or found; it is
   citable; the span is non-empty and inside the record. A claim of
   absence needs every view of its kinds complete and read to its end, and
   is `not_enough_data` (`incomplete_view`) when a view is incomplete. One
   verdict per claim: `structurally_valid`, `unsupported`, `absence_ok` or
   `not_enough_data`, with its problems.
3. `claims: required` makes a missing or empty `claims` a problem.
   `on_invalid: mark` keeps the node's status and marks the claims;
   `on_invalid: fail` fails the attempt with failure kind
   `check_rejected` and the problems as its note, and the retry's prompt
   names them.
4. The outcome (verdicts, problems, flags, the citations in byte form)
   and the read trace (operations, budget, how far each view was read) are
   written into the session file. No source text and no claim text is
   stored; the claim text stays in the node output.

## 6. A weak but valid citation

1. The wording is fixed on every surface: "Structurally valid: the cited
   record exists, was returned whole to this attempt, is of an allowed
   kind, and the span lies inside it. Whether it supports the claim was
   not checked." Never "confirmed" or "verified".
2. Next to each claim the surfaces show the cited fragment (the span, or
   the first 300 characters of the record), so a person compares.
3. Deterministic flags, shown as "worth a look", never as errors:
   `whole_record` (no span), `short_span` (under 16 characters),
   `self_citation` (a record of the consumer node's own earlier
   execution), `shared_record` (one record cited by more than 5 claims),
   `no_shared_words` (no word of 4 or more letters shared between the claim
   text and the fragment after case folding).
4. A claim that does not stand reads "not supported by a cited record (not
   shown as a fact)".

## 7. Shared code

`grade/reader.rs`, `grade/sources.rs` and `grade/evidence.rs` keep their
behaviour: the view and record types, the per-citation rules and the
reader live in `crate::citation` (`views`, `rules`, `reader`), generic over
the kind of a view, and both the grader and `evidence::check` call them.
The grader's references and spans keep their meaning; snapshot references
are a second reference family in the same rules.

## 8. Surfaces

- `apb results <run>` prints each session's claims with the wording, the
  fragments and the flags; MCP `run_results` and the HTTP results route
  carry them as `evidence`; the run page shows them in the Results panel.
- `run_result_read` refuses a package record reference: a record is read
  through its session.
- The read-only MCP server removes `run_evidence_read`.
- A run whose definition uses `evidence` carries the run guard `evidence`,
  so an older apb refuses to continue it.

## 9. Exact guarantee and non-promises

Guaranteed for a `structurally_valid` claim: the record exists in a stored
snapshot named by this run's journal; it was returned whole to this
attempt through the engine's reader; it is of a kind the declaration
allows; the span lies inside it; for an absence, every view of the named
kinds was complete and read to its end. Not promised: that the record
supports the claim; that the record is true (a node output is what an
agent wrote); that the agent did not read sources past the reader or rely
on unread text; completeness of a package relative to the outside world.

## 10. Non-goals

Reading inside `isolation: full`; connector response bodies as packages;
transcripts as read traces; model-judged support; citations in grades
changing meaning (`rules/1` unchanged); an injected MCP sidecar for agent
CLIs (the CLI command serves every agent with a shell; host tasks use
MCP).
