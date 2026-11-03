# Evidence for node outputs: record packages, reading sessions and claims

Date: 2026-11-03. Status: accepted. The third of three parts of the
evidence and approvals work, after result snapshots
([2026-11-03-result-snapshots-design.md](2026-11-03-result-snapshots-design.md))
and approvals
([2026-11-03-approvals-design.md](2026-11-03-approvals-design.md)).
Author-facing usage: `docs/RESULTS.md` ("Evidence for node outputs"), the
node reference in `docs/HOWTO-authoring.md` ("Evidence for an agent's
output"), the tool in `docs/MCP.md`.

## 1. Goal

An agent that summarizes what earlier steps produced (a week of mail, the
open tickets, a report) states things a person then acts on. Today nothing
ties a statement to a record: the agent may have read the source, half of
it, or none of it. This part lets a node back its output with claims that
cite records it was shown whole, and checks that structure, without ever
claiming more than it checked.

## 2. The boundary

The contract applies only to an `agent_task` that reads its sources
through the engine: a reading session the engine opens per attempt over
stored snapshots, read through the same bounded reader and checked by the
same citation rules as the run grader (`docs/QUALITY.md`). Agent
transcripts are not a read trace: few attempts have one, and none with
bounds. An agent may still read the same files directly; a citation of
something it read that way fails the full-read check, an honest refusal
and not a bypass.

## 3. Record packages

1. A result entry with `format: records` is a package: UTF-8 JSON Lines,
   first line a header `{"apb_records": 1, "complete": true|false,
   "source": "<text>"}`, then one record per line `{ "id", "kind", "text",
   "source"?, "version"?, "at"? }`. An id matches `[A-Za-z0-9._:-]{1,128}`
   and is unique; a kind is a slug; at most 10000 records. Owner:
   `apb_core::records`.
2. A text that does not parse is stored as plain text
   (`format: text`) with `records_invalid` naming the first problem
   (`SnapshotEntry.records_invalid`, serde default). It is not a package.
3. `complete` is the collector's own statement and is shown as such ("the
   collector says the package is complete"). The engine adds its own: a
   package from a snapshot a limit stopped or that lost entries makes its
   views incomplete. Completeness relative to the outside world is never
   promised.
4. A record reference is `r:<snapshot_id>/<entry>@<record id,
   percent-encoded>`, optionally with the byte range `#[a,b)` of the record
   text (`apb_core::result_store::Citation::record`).

## 4. The declaration

`agent_task` gains `evidence: { from: [<node>...], kinds?: [<kind>...],
claims: required | optional (default optional), on_invalid: mark | fail
(default mark), budget_chars?: u64 }` (`apb_core::schema::Evidence`).
V159 refuses it on another kind of node, a `from` list that is empty,
names an unknown node, a node that settles no result or a node that does
not reach the consumer, a `kinds` entry that is not a slug, and `isolation:
full` (the reader needs the run directory). Every `from` node is
snapshotted (`validate::snapshotted_nodes`). A definition with `evidence`
carries a `run_guard` naming `evidence` (`GuardedFeature::Evidence`), so an
older apb refuses to continue a run that would drop the check.

## 5. The session

1. Owner: `apb_engine::evidence` (`session`, `sources`, `check`). Before
   each attempt starts, the engine binds the latest stamped, verified
   snapshot of each `from` node (for a list node, its node snapshot and the
   item snapshots of its latest visit), builds the views and writes
   `runs/<id>/evidence/<session>.json` (private, atomic, under the
   directory's lock): a 128-bit random session id, the run, node, item and
   attempt number, the bound snapshot ids, the views (id, kind, complete,
   the collector's statement for a package, each record's reference, the
   digest of its stored text, whether it is citable), the budget and an
   empty read state. No text. The session is bound to the attempt id (the
   seq of its `attempt_started`) as soon as that is journaled.
2. Views: per snapshot an `output` view (one record), a `fields` view, a
   `file` view per plain text file, and per package a `records` view (one
   record per package record) or, with `kinds`, one view per named kind,
   empty when the package has none. With `kinds`, only views of those kinds
   are built.
3. A record whose stored text the run redactor changes when the session
   opens is served redacted and is never citable, so every citable span is
   a range of stored bytes. A record not citable when the session opened
   stays so.
4. The session id reaches the agent in a `## evidence` prompt section
   (`context::evidence_section`, appended per attempt outside the cache
   key) and in `APB_EVIDENCE_SESSION` (`adapter::spawn_env`); a host task
   carries both.
5. Reading: MCP `run_evidence_read { session, op, ref?, span?, view?,
   from?, count?, phrase? }` and `apb evidence read --session <id> <op>`
   answer the same object through `apb_mcp::tools::evidence`, the five
   operations of the shared reader (`crate::citation::reader`). Every read
   rebuilds the views from the bound snapshots, re-verified, and refuses
   when a record's stored text no longer has the digest the session opened
   with. A caller whose `APB_RUN_ID` names another run is refused (a guard
   against mistakes). The session closes when the attempt ends; a read
   after that, or after the journal records the attempt's end, is refused.
   A read-only MCP server does not route the tool.

## 6. Claims and the check

1. The agent writes `outputs.claims` in its status file: `[{ "id", "text",
   "cites": [{ "ref", "span"? }], "absence": { "kinds": [...] }? }]`. Spans
   are character offsets into the record text as served. Each accepted
   citation is stored in the byte form `r:...#[a,b)`.
2. At the end of a succeeded attempt, before the success decision (the
   same place as `success_check`, so a status-file success recovered after
   a crash is checked too), `evidence::check` gives each claim one verdict
   through `crate::citation::check`: `structurally_valid`, `unsupported`,
   `absence_ok` or `not_enough_data` (`incomplete_view`), with problems.
3. `claims: required` makes a missing or empty list an unsupported output.
   `on_invalid: mark` keeps the node's status; `on_invalid: fail` fails the
   attempt with failure kind `check_rejected` and the problems as its
   rejection note, so the retry path re-runs it and its prompt names the
   problems.
4. The verdicts, the read trace and the flags are written into the session
   file. No source text is stored; the claim text stays in the status file.

## 7. A weak but valid citation

1. The wording, fixed on every surface: "Structurally valid: the cited
   record exists, was returned whole to this attempt, is of an allowed
   kind, and the span lies inside it. Whether it supports the claim was not
   checked." An unsupported claim reads "Not supported by a cited record
   (not shown as a fact)." Never "confirmed" or "verified".
2. Next to each claim the surfaces show the cited fragment (the span, or
   the first 300 characters of the record), so a person compares.
3. Flags, shown as "worth a look": `whole_record`, `short_span` (under 16
   characters), `self_citation`, `shared_record` (one record cited by more
   than five claims), `no_shared_words`.

## 8. Shared code

The view, record and read-state types, the bounded reader and the
per-citation rules live in `apb_engine::citation` (`views`, `rules`,
`reader`), generic over the view kind; the run grader uses them with its
`ReadKind`, an evidence session with kind names. A view set may mark
records as not citable and names its scope in problems ("this run", "this
session"). Every grade test passes unchanged.

## 9. Surfaces

`apb results <run>` prints each session's claims with the wording, flags,
problems and fragments; MCP `run_results` and `GET /api/runs/{id}/results`
carry `evidence` (`EvidenceRow`, in the generated TypeScript); the run
page's Results panel shows them (`EvidenceClaims.svelte`).

## 10. Exact guarantee

Guaranteed for a `structurally_valid` claim: the record exists in a stored
snapshot named by this run's journal; it was returned whole to this
attempt through the engine's reader; it is of a kind the declaration
allows; the span lies inside it; for an absence, every view of the named
kinds was complete and read to its end. Not promised: that the record
supports the claim; that the record is true; that the agent did not read
past the reader or rely on unread text; completeness of a package relative
to the outside world.

## 11. Non-goals

Reading inside `isolation: full`; connector response bodies as packages;
transcripts as read traces; a model's opinion of semantic support; any
change to what grades mean (`rules/1` is unchanged); an MCP sidecar
injected into agent CLIs (the CLI command serves every agent with a shell,
host tasks use MCP).

## 12. Decisions taken where the brief was silent

1. The session file is named by the session id, not the attempt id: the
   id must reach the agent's prompt and environment before the process
   starts, while the attempt id is the seq of `attempt_started`, journaled
   at spawn. The file records the attempt id once it is known.
2. `not_enough_data` is not invalid: `on_invalid: fail` rejects only
   `unsupported` claims and an unsupported output.
3. Without `kinds`, a package is one `records` view; an absence then names
   `records`. With `kinds`, a named record kind a snapshot that lost entries
   shows no view of gets an empty incomplete view, so its absence is
   `not_enough_data`.
4. `apb evidence read` inside a step reads the project `APB_RUN_DIR` names,
   since the step may work in a separate working tree; it exits 2 for a
   session not found and 3 for a refusal.
