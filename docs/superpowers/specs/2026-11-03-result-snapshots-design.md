# Result snapshots: declared results, the store, reading and cleanup

Date: 2026-11-03. Status: accepted. This is the first of three parts of
the evidence and approvals work; approvals of an exact snapshot set and
citations for node outputs follow (section 11). Author-facing usage:
`docs/RESULTS.md`, the node reference in `docs/HOWTO-authoring.md`
("Result snapshots (results:)"), the tools in `docs/MCP.md`.

## 1. Goal

A run's results today are what its nodes left behind: output text in the
journal, files in a working tree the next node or the next run may change,
and `node_finished.artifacts`, a digest without stored bytes. Three things
are missing:

1. **The result of one execution.** A loop that rewrites `report.md` three
   times leaves one file; the earlier two are gone, and the cache keeps a
   file only when the node is cached and only on clean admission.
2. **Bytes that can be read back exactly.** A person reviewing a run, or a
   later step citing what a node produced, needs the same bytes everywhere,
   and a clear answer when they are not intact.
3. **Safety at capture.** Whatever a node names is written by an agent:
   it can be a link, a FIFO, a huge tree or a file full of secrets. Keeping
   it must not follow the link, block, exhaust the drive or store the
   secret.

A node now declares what it produces, apb keeps a snapshot at each
settlement of its work, and every surface reads it back through one view.

## 2. Declaration

1. **Node field `results:`** (schema 2, additive):
   `Option<Vec<ResultDecl { path, scope, format }>>`, with `scope`
   `workspace | run` (default `workspace`) and `format` `text | records`
   (default `text`). `None` takes no snapshot; `Some([])` keeps the output
   and fields only. `format: records` is stored as metadata only; the
   package reader comes later (section 11).
2. **Kinds.** `agent_task`, `script` (a list node is one of them) and a
   `finish` with a prompt.
3. **Path grammar.** Relative, `/`-separated, no empty, `.` or `..`
   segment; `*` and `?` in the last segment only; no `**` and no `[`, `]`,
   `{`, `}`, `\`, so a declaration means the same to every matcher; a
   workspace path stays out of `.git` and `.apb`; a run path names none of
   apb's own storage. The validator refuses what a declaration states
   outright; what a glob matches at run time is the capture's to refuse.
4. **Scope roots.** `workspace` is the run's working tree (the resolved
   worktree, else the execution root), the same root `outputs.files` uses;
   `run` is the run directory.
5. `validate::snapshotted_nodes` returns every node that declares
   `results:` and is the one place later references to a node's results
   extend.

## 3. Storage

Owner of layout, limits, manifest and transaction:
`apb_core::result_store`. Layout under `runs/<id>/results/`: `.lock` (the
run-level results lock), `reservation.json`, `staging/<token>/` with
`txn.json` and `staging/<token>.lock`, `objects/<aa>/<hex>`
(content-addressed, write-once) and `snapshots/<snapshot_id>.json`.

1. **Manifest.** Fixed-order structs, compact JSON, absent optional fields
   omitted; entries sorted by part (`output`, `fields`, `file`), scope and
   path bytes, omissions likewise with the aggregate last. `snapshot_id` is
   the sha256 of those bytes. The manifest names its subject in plain
   values, `SnapshotSubject { node, execution, visit_seq, item, attempt }`,
   since the settlement seq is not known before the settlement event is
   appended; the stamp on that event closes the link, and the engine's view
   maps a subject plus the stamping seq to a result reference. Provenance:
   `cache_hit`, `head` and `commits_seq`.
2. **Internal storage.** `results`, `definitions`, `bindings`,
   `diagnostics.jsonl` and `evidence` are never captured, and the
   run-directory walk of `outputs.files` skips them from now on.
3. **Transaction**, two locks taken through descriptors (`flock`, close on
   exec, released by the kernel when the holder dies):
   1. begin, under the results lock: reserve room for the manifest, create
      the staging directory and its lock, write `txn.json`;
   2. stage each object: reserve its bytes under the results lock, name it
      in `txn.json`, then write it;
   3. publish, under the results lock: move objects into `objects/` (one
      already there is kept), write the manifest, record its id in
      `txn.json`, rename it into `snapshots/`;
   4. the caller journals the stamp on the settlement event;
   5. end, under the results lock: remove staging, settle the reservation,
      release the transaction lock. When journaling fails the transaction
      aborts and its manifest is removed; its objects are left to cleanup,
      since another transaction may have found one in place.
4. **Safe IO** (`fsutil::beneath`). An anchor directory is opened once and
   every path is walked from it with `openat(O_NOFOLLOW | O_DIRECTORY)`. A
   read looks at the last component first and refuses a link, anything
   not a regular file and a file with more than one link before opening
   it with `O_NOFOLLOW | O_NONBLOCK`, checks the opened file is the one it
   looked at, and reads at most its cap, refusing a file that grew past it.
   Writes make directories 0700 with `mkdirat`, temp files 0600 with
   `O_CREAT | O_EXCL | O_NOFOLLOW`, fsync, `renameat` within the
   directory's descriptor and fsync the directory; every write passes the
   process write guard, so a read-only or scenario process writes nothing.
   Off unix each component is checked with `symlink_metadata`, a weaker
   guarantee the module states.

## 4. Capture

Owner: `scheduler/result.rs`.

1. **Which settlements.** A declaring node's settlement with `succeeded`,
   `failed` or `timed_out`, at every writer of node work: the sequential
   arm, a batch member, a batch or sequential cache hit (after the
   restore), the finish answer, and a list item settled right after its
   step ran. Write-offs (`cancelled`, the `join: any` loser, the unattended
   and deferred closings) keep the `node_finished` constructor that
   carries no stamp.
2. **What.** The output first; the declared fields when the output is a
   JSON object (a list item: its `item_outputs`, or the whole object); the
   declared files only on `succeeded` and never for an item. A list item's
   snapshot holds its full output while `list_item_settled.output` stays
   clipped.
3. **Text and scrub.** An entry is text when it is UTF-8 without a NUL.
   Every text, field key and string is scrubbed through
   `RunRedactor::secret_text` (the run tree's values and token shapes); a
   changed text is stored scrubbed with `redacted: true`. A path the scrub
   changes becomes `<redacted path n>`. Secret-shaped names are omitted
   before any open. When the redactor withholds, the capture fails closed:
   nothing read, every part omitted `scrub_unavailable`, the manifest still
   stamped.
4. **Scenario runs** store the output and fields only (`scenario_run` for
   each declaration, no `head`).
5. **Failure.** An I/O error, a full store at begin, a lock not taken in
   10 seconds or a manifest over its limit settles the node without a
   stamp and writes a `result_capture_failed` diagnostics record; nothing
   enters the journal. The view lists such a settlement under
   `not_stored` with its reason.
6. **Per-attempt heads.** The provenance tracker reads `HEAD` at each agent
   `attempt_started`; an attempt's head is the one the next attempt started
   from, the last one's the final `after`. `artifacts_committed` lists them
   in `attempts` and names the last attempt in `attempt`; the commit rows
   walk from the newest commit and attribute each to an attempt ("committed
   by attempt N").

## 5. Limits

All in `result_store`, each stopping work, not only storage: 32
declarations per node; 4096 directory entries examined per settlement
across all globs; 64 matched files; 32 MiB read; 1 MiB per file (larger is
omitted, never cut); 16 MiB of objects per snapshot; 256 MiB of objects,
manifests and staging per run; 64 omission records, then one aggregate
`{ more, reasons }`; a manifest of at most 256 KiB. The reservation is
updated under the results lock before any byte, so concurrent settlements
cannot pass the run limit. `incomplete` names the limit that leaves the
most unknown: enumeration, match, read, storage. An output larger than a
snapshot may hold is `output_omitted: snapshot_limit`; one the run has no
room left for, `output_omitted: run_limit`. Reason names read
leniently: a name a newer apb wrote reads `unknown`.

## 6. Threat model

**Assets.** Secrets in the tree and the run directory; the run directory's
integrity; disk; the drive process, which must not block or exhaust memory;
the project tree, never written. **Actors.** The node's agent controls file
contents, names, links, FIFOs and tree size and can race the capture; the
playbook author writes the declarations; other local processes; readers on
MCP, HTTP and the CLI.

| Threat | Control |
|---|---|
| a link swapped in at any component, before or during the walk | the descriptor walk with `O_NOFOLLOW`; tests swap a parent between listing and reading and a directory between creation and write |
| a hard link to a private file named like a report | a regular file with more than one link is omitted `hard_link` unread |
| a FIFO, device or socket at a declared path | looked at before opening, opened `O_NONBLOCK`; omitted `not_regular_file` |
| enumeration and size bombs | the limits of section 5 |
| secrets reaching storage | secret-shaped names never opened, the scrub with the run tree's values, per-string scrub of metadata, fail closed |
| concurrent settlements overrunning the run | the reservation under the results lock |
| a crash or kill at any stage | the transaction and its lock; nothing readable that the journal does not name |
| capturing apb's own storage | internal names excluded by the validator and by the capture, and by the `outputs.files` walk |
| a tampered store | every read re-hashes manifest and object; a mismatch is `corrupt` |
| stored text rendered in a browser | the raw route is `text/plain`, `nosniff`, `attachment`; the dashboard shows text in a `<pre>`, never as HTML |
| a secret known only after capture | every answer passes the run redactor (`redacted_on_read`) |

**Out of the boundary.** That the bytes are what the node wrote (another
process may have written the file); a secret the scrub does not recognize
(a copied credential under an innocent name, without token shape); binary
content; meaning; and a dashboard without keys, which serves result reads
to any host it allows, as it serves run detail.

## 7. Reading

Owner: `result_view.rs`; every surface answers through it.

1. `ResultsView::load` builds `ResultRow`s from every `node_finished` and
   `list_item_settled` that carries a stamp (execution from the identity
   index, the item's list visit, the stamping seq, the producing attempt
   from the subject), and `LegacyRefRow`s from `node_finished.artifacts`
   without one. `verified` is `missing` without a manifest, `corrupt` when
   the manifest does not hash to its id, does not decode, or is not the
   stamping settlement's (node, item, run), or when an object is absent,
   changed, of another size or not UTF-8. A run without stamps opens
   nothing under `results/`.
2. `read_entry` and `read_window` serve one entry of a stamped snapshot,
   re-verified, through the run redactor (`Withheld` when it cannot
   resolve). Ranges and windows count in the served text, so a range never
   cuts a redacted value; digest and size are the served text's. The
   display cap is 64 KiB; the raw route serves the whole entry.
3. `compare_live` answers `same`, `differs`, `missing`, `unreadable` or
   `not_comparable`, never live content, re-hashing the live file through
   the same walk in the tree capture read it from: the run's worktree,
   else the project root. Only an unredacted workspace file
   is comparable.
4. **Citations.** `r:<snapshot_id>/<entry>` with an optional byte range
   `#[a,b)`; an entry is `output`, `fields` or `file:<scope>:<path>`, the
   path percent-encoded per segment. A shared fixture
   (`crates/apb-core/tests/fixtures/result_manifest.json`) checks the
   canonical bytes, the id, the citations and the UTF-16 ranges in Rust and
   in vitest.
5. **Surfaces.** MCP `run_results` and `run_result_read` (read-only, in the
   `Full` table of `apb mcp --read-only`); `apb results`, `apb results
   show`, and the results lines of `apb runs <id>`; `GET
   /api/runs/{id}/results`, `.../results/{snapshot}?entry=`, `.../raw` and
   `.../live`; the run page's Results panel with View, Download and Compare
   with live. Runs resolve only under `.apb/runs`; no surface takes a path.

## 8. Cleanup

`result_clean::clean(run_dir, dry_run)` behind `apb results clean <run>
[--dry-run]`, under the results lock, reading the journal after taking it:

1. a transaction whose lock is free is completed when the journal names its
   manifest, rolled back otherwise;
2. a manifest or object is removed only when no stamp and no open
   transaction names it. Stamps are read textually (every run of 64
   lowercase hex characters protects that id), so an undecodable or newer
   line still protects; an unverified kept manifest keeps every object;
3. the reservation is recomputed from what is kept.

`survey` answers the same without the lock and writes nothing; a dry run
and the `results` row of `apb doctor --run` use it. The cleanup command is
`apb results clean`, since `apb runs` takes a run id as its first argument.
apb has no command that deletes a run; one added later calls cleanup first.

## 9. Journal and older binaries

No new event type. Every field is display or storage only.

| Change | Older binaries | Rule |
|---|---|---|
| `node_finished.result: Option<ResultStamp>` | dropped; same routing | a new field is `serde(default)` and lenient |
| `list_item_settled.result` | dropped; where the event is unknown it is skipped up to the next checkpoint, as every list event already is | same |
| `artifacts_committed.attempt: Option<u64>`, `attempts: Vec<AttemptHead { attempt, head }>` | dropped | same |
| node field `results:` | dropped on load; the run takes no snapshot | stated per feature |
| `runs/<id>/results/` | never opened | none |

`ResultStamp` is `{ snapshot_id, entries, bytes, omitted, incomplete }`,
`omitted` written only when not zero and `incomplete` only when true. A run
without a declaration writes no `result`, `attempt` or `attempts` key.

## 10. Validator

| Code | Severity | Rule |
|---|---|---|
| V152 | error | `results` on a node kind that does not settle work of its own (allowed: `agent_task`, `script`, `finish` with a prompt) |
| V153 | error | a `results` path breaks the grammar of section 2 item 3 |
| V154 | error | more than 32 `results` entries on one node |

## 11. Later parts

The second part lets a `human_review` approve an exact set of snapshots and
a later node require that approval, with person-only gates and a run guard
that refuses an older executor which would silently drop the requirement.
The third part adds record packages (`format: records`) and citations for
node outputs: an agent reads stored snapshots through the engine's reader,
and its claims cite records it read, checked structurally. Both extend
`snapshotted_nodes` and add their own storage under the run directory.

## 12. Not covered

Binary results; attempt-level snapshots; connector response bodies;
undeclared files; cross-run deduplication; showing live content; a
run-deletion command; any promise about meaning.
