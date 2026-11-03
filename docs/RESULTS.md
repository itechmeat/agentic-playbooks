# Result snapshots

A node that declares `results:` keeps a snapshot of what it produced every
time its work settles: its output text, its declared fields and the files it
names, as text, scrubbed of known secrets and addressed by digest. A snapshot
lives in the run directory, outlives the working tree and the cache, and is
read back exactly or reported corrupt. A loop that rewrites `report.md` three
times keeps three snapshots, one per execution.

Nothing changes for a node without a declaration: no snapshot is taken and
no `results/` directory is created.

The design is
[2026-11-03-result-snapshots-design.md](superpowers/specs/2026-11-03-result-snapshots-design.md).

## Declaring results

```yaml
nodes:
  - id: audit
    type: agent_task
    prompt: "Write the audit to report.md and the findings to findings/"
    outputs:
      fields: [verdict]
    results:
      - path: report.md                 # scope: workspace (the default)
      - path: "findings/*.md"           # a glob in the last segment
      - path: notes/summary.txt
        scope: run                      # relative to the run directory
      - path: records.jsonl
        format: records                 # a record package (JSON Lines)
```

Each entry has:

- `path`: an exact relative path, or a glob whose wildcards sit in the last
  segment only.
- `scope`: `workspace` (default) or `run`. `workspace` is the run's working
  tree (the `worktree` once it is resolved, else the project root), the
  same root `outputs.files` uses; a node's own `workdir` does not move it.
  `run` is the run directory `runs/<id>/`.
- `format`: `text` (default) or `records`. A `records` entry is stored and
  shown like text and is read as a record package by an evidence session
  (`docs/superpowers/specs/2026-11-03-evidence-citations-design.md`): UTF-8
  JSON Lines with a header `{"apb_records": 1, "complete": ..., "source":
  ...}` and one record per line. A text that is no package is kept as plain
  text, and its manifest entry carries `records_invalid` with the first
  problem (lines and fields only, never the text).

`results: []` is a declaration too: the node keeps its output and fields,
and no file.

**Node kinds.** `agent_task`, `script` (a list node is one of them) and a
`finish` with a prompt. Any other kind is V152.

**Path grammar.** A path is relative and `/`-separated. The validator
refuses (V153):

1. an empty path, a leading `/`, or a NUL byte;
2. `**` anywhere;
3. an empty, `.` or `..` segment;
4. `*` or `?` outside the last segment;
5. the other glob syntax `[`, `]`, `{`, `}` and `\` (so a file name that
   contains one cannot be declared);
6. a `workspace` path whose first segment is `.git` or `.apb`;
7. a `run` path whose first segment is apb's own storage: `results`,
   `definitions`, `bindings`, `diagnostics.jsonl` or `evidence`.

Rules 6 and 7 compare in any letter case (`.Git` is `.git` on a file
system that folds case), and the capture checks these names again before
it reads anything.

A glob matches files in one directory level: `*` matches any run of
characters, `?` one character, and a leading dot is not special. A directory
never matches. At most 32 entries per node (V154).

| Code | Severity | What it means |
|------|----------|---------------|
| V152 | error | `results` on a node that settles no work of its own: anything but an `agent_task`, a `script` or a `finish` with a prompt |
| V153 | error | a `results` path breaks the grammar above |
| V154 | error | more than 32 `results` entries on one node |

## When a snapshot is taken

A snapshot is taken at every settlement of a declaring node's work whose
status is `succeeded`, `failed` or `timed_out`:

- a node's own settlement, on the sequential arm and as a batch member;
- a cache hit, after the restore, so the stored files are the restored bytes
  (`cache_hit` in the provenance);
- the finish answer of a `finish` with a prompt;
- every list item that settles right after its step ran (passed, rejected,
  or needing a look after a step): an item snapshot of the item's full
  output and its fields, while `list_item_settled.output` stays clipped as
  before. An item that is interrupted, requeued for a retry, carried over or
  settled for an unknown send gets none.

Write-offs never take one: `cancelled` and `interrupted` settlements, the
losing member of a `join: any`, and the closings of an unattended run or a
deferred retry.

What a snapshot holds:

| Part | When |
|---|---|
| `output` | always, stored first |
| `fields` | when the output is a JSON object: the declared `outputs.fields` (none declared, none stored); for a list item its `item_outputs`, or the whole object when the list declares none. Every key and string is scrubbed, then written as compact JSON |
| files | only on a `succeeded` settlement of the node itself, never for a list item: every declared file as it is at settlement, changed or not. A list node's files are captured once, on the list node's own settlement |

The manifest also names its subject (the node, its execution ordinal, the
list visit and item, and the attempt that produced it) and its provenance:
`cache_hit`, `head` (`HEAD` of the working tree at capture, absent off git,
for an item and in a scenario run) and `commits_seq` (the seq of the
execution's `artifacts_committed`, when it committed).

The settlement event carries a stamp: `node_finished.result` or
`list_item_settled.result`, with `snapshot_id`, `entries`, `bytes`,
`omitted` (written only when not zero) and `incomplete` (written only when
true). Nothing reads a snapshot the journal does not stamp.

**A capture never fails the node.** An I/O error, a full store, a results
lock not taken within 10 seconds or a manifest over its limit settles the
node without a stamp and writes a `result_capture_failed` record (node,
execution, item, reason) to the run's diagnostics file
`runs/<id>/diagnostics.jsonl`, never to the journal. Every read surface
lists such a settlement as `<node> #<n>: snapshot not stored: <why>`
(`not_stored` in the view).

## Limits

Every limit counts real work and stops it, not only storage. Files are taken
in declaration order, then by path bytes.

| Limit | Value | When reached |
|---|---|---|
| Declarations per node | 32 | V154 |
| Directory entries examined per settlement, across all globs | 4096 | enumeration stops; `incomplete: enumeration_limit` |
| Matched files per settlement | 64 | each further match is omitted `match_limit`; `incomplete: match_limit` |
| Bytes read per settlement | 32 MiB | further files omitted `read_limit`; `incomplete: read_limit` |
| One file | 1 MiB | the file is omitted `too_large`, never cut |
| Objects of one snapshot | 16 MiB | further entries omitted `storage_limit`; `incomplete: storage_limit`; an output larger than this is `output_omitted: snapshot_limit` |
| Objects, manifests and staging of one run | 256 MiB | as above; when the run has no room left for the output, `output_omitted: run_limit` |
| Omission records | 64, then one aggregate `{ more, reasons }` | the aggregate counts the rest by reason |
| One manifest | 256 KiB | the capture fails (`result_capture_failed`) |

The output has no per-file cap; the engine's output limits and the snapshot
cap bound it. When several limits stopped a capture, `incomplete` names the
one that leaves the most unknown, in this order: enumeration, match, read,
storage. The stamp's `omitted` counts every omission, the aggregate's
included.

Room is reserved in `results/reservation.json` under the run's results lock
before any byte is written, so two settlements at once never pass the run
limit. The same bytes stored twice in one snapshot cost nothing, and an
object already in the store is kept.

### Omission reasons

| Reason | Meaning |
|---|---|
| `binary_not_supported` | not valid UTF-8, or a NUL byte |
| `hard_link` | a regular file with more than one hard link; not read |
| `link` | a symbolic link at some component of the path; not followed |
| `match_limit` | past the 64 matches of the settlement |
| `non_utf8_path` | a matching name that is not valid UTF-8; listed without a path |
| `not_found` | a declared exact path does not exist |
| `not_regular_file` | a FIFO, device, socket or directory where a file was declared; never opened |
| `read_limit` | past the 32 MiB read budget |
| `scenario_run` | a declared file in a scenario run (see below) |
| `scrub_unavailable` | the run's secret values could not all be read; nothing was read or stored |
| `secret_shaped` | a name matching `.env*`, `*.pem`, `*.key`, `*.p12` or `id_*`, in any letter case; never opened |
| `storage_limit` | past the 16 MiB snapshot or the 256 MiB run limit |
| `too_large` | over 1 MiB |
| `unreadable` | reading failed, or the scope root could not be opened |

### Incomplete reasons

| Reason | Meaning |
|---|---|
| `enumeration_limit` | 4096 directory entries were examined; what was not listed is unknown |
| `match_limit` | more than 64 files matched |
| `read_limit` | 32 MiB were read |
| `storage_limit` | the snapshot or the run ran out of room |

A newer apb may write a reason this one does not know; it reads as
`unknown` and the manifest still loads.

## Secrets

- A secret-shaped name is never opened.
- Every stored text (the output, the fields as one JSON text, so a value
  that sits in a number goes too, every file) is scrubbed of the run tree's secret values (the run's, its ancestors' and
  the workspace's connector secrets) and of token shapes. A changed text is
  stored scrubbed with `redacted: true`, and its digest is the digest of the
  stored bytes; nothing of the original is kept, not even a hash.
- A path the scrub changes becomes `<redacted path n>`, numbered in order
  of appearance, in `entries` and `omitted` alike.
- **Fail closed.** When the run's secret values cannot all be read, nothing
  is read or stored: the output is `output_omitted: scrub_unavailable`, the
  fields and each declaration are omitted `scrub_unavailable` without a
  path. The manifest is still published and stamped, and the node settles.

## Unsafe files

Every source is read through a directory descriptor opened once on its scope
root, one path component at a time, never following a link at any component.
A file is looked at before it is opened: a link, a FIFO, a device, a socket
or a directory is refused unread (`link`, `not_regular_file`), so a FIFO at a
declared path never blocks the run, and a regular file with more than one
hard link is refused unread (`hard_link`), so a link to a private file
elsewhere on the same disk cannot pass as a report. A file that grows past
its cap while it is read is omitted, never cut.

At the top of the run directory a `run` glob skips apb's own storage
(`results`, `definitions`, `bindings`, `diagnostics.jsonl`, `evidence`).
From this release on, the run-directory walk of `outputs.files` skips the
same names.

The store is written the same way: directories 0700, files 0600 at
creation, each write renamed into place within its directory's descriptor.

## Scenario runs

A scenario run executes no node, so its tree is not a node's result. A
snapshot there holds the output and fields only: each declared file is
omitted `scenario_run` and `head` is absent. The reading surfaces below look
up runs under `.apb/runs`, so they do not serve a scenario run's snapshots.

## Viewing

### CLI

```
apb results <run> [--node <id>] [--json]
apb results show <run> <ref> [--out <file>]
apb results clean <run> [--dry-run] [--json]
```

- `apb results <run>` lists one line per snapshot (node, `#` execution,
  item, status, snapshot id and flags: `missing`, `corrupt`, `redacted`,
  `incomplete: <reason>`, `output not stored: <reason>`, `omitted n`,
  `cache hit`, `at <head>`), its entries as `r:<id>/<entry>` with size and
  short digest, and under "recorded digests (no stored bytes)" the artifact
  refs of settlements without a stamp, then one line per settlement whose
  snapshot was not stored, with why. `--json` prints the object MCP
  `run_results` answers.
- `apb results show <run> <ref>` prints one entry's whole stored text,
  re-verified on this read and redacted; a range `#[a,b)` in the reference
  prints those bytes only (no display cap), and a range off a character
  boundary is refused. Piped or with `--out`, the bytes are exact; on a terminal,
  control characters other than line breaks are shown as `?`. A read
  redaction is noted on standard error. Exit 2 when the run or entry is not
  found, 1 on any other refusal.
- `apb runs <id>` adds `results: n snapshot(s); apb results <run>` and one
  `result <node> #<n>` line per snapshot, only when the run has a stamp.

### MCP

`run_results { run_id, node?, workspace? }` answers `{ run_id, rows, legacy, not_stored }`.
A row has `execution`, `item` (its list visit and id), `settlement_seq`,
`produced_by_attempt` (the attempt id), `attempt_number`, `status`, `snapshot_id`, `entries` (`entry`, `part`,
`scope`, `path`, `size`, `digest`, `redacted`, `format`), `omitted`,
`incomplete`, `output_omitted`, `provenance` and `verified` (`ok`,
`missing`, `corrupt`). `legacy` lists the artifact refs recorded without
stored bytes (`execution`, `settlement_seq`, `scope`, `path`, `digest`).
`not_stored` lists the settlements whose capture failed (`node`,
`execution`, `item`, `reason`).

`run_result_read { run_id, ref, offset?, limit?, compare_live?, workspace? }`
answers `{ run_id, ref, content, live? }`, where `content` is `text`,
`digest`, `size`, `range`, `redacted_at_capture` and `redacted_on_read`. A
range in `ref` wins and may span at most 64 KiB; without one, `offset`
(default 0) and `limit` (at most 64 KiB, the default) select a window. The
next window starts at the end of `content.range`; an empty range means the
text ended.

Both tools are read-only and served in full by `apb mcp --read-only`.

### HTTP

| Route | Answer |
|---|---|
| `GET /api/runs/{id}/results` (`node`) | the `run_results` object |
| `GET /api/runs/{id}/results/{snapshot}?entry=&offset=&limit=` | the `run_result_read` object for one entry, a window of at most 64 KiB |
| `GET /api/runs/{id}/results/{snapshot}/raw?entry=` | the whole entry as `text/plain; charset=utf-8`, `X-Content-Type-Options: nosniff`, `Content-Disposition: attachment`, `Cache-Control: no-store` |
| `GET /api/runs/{id}/results/{snapshot}/live?entry=` | `{ "live": <word> }` |

Every route takes `workspace`. The snapshot is an id and the entry an entry
id (URL-encoded, since it holds `:` and `%`); no route takes a path. Not
found answers 404, `corrupt` and `secrets_unresolved` 409, a bad range or
entry 422.

### Dashboard

The run page has a Results panel after the outcome panels. Rows are grouped
by node, then execution (`#2`), then list item, each with its status, the
producing attempt, the commit it was taken at and badges (`corrupt`,
`missing`, `cache hit`, `redacted`, `incomplete: <reason>`, `omitted n`,
`output not stored: <reason>`). Every entry shows its name, size and short
digest, and every omission its reason; "Recorded digests (no stored bytes)"
holds the legacy refs, and "Not stored" the settlements whose capture
failed, with why. Each entry of a verified row has:

- **View**: the text in a dialog, as literal text (never rendered as HTML),
  64 KiB at a time with Show more, marked "redacted at capture" or
  "redacted on read" when so;
- **Download**: the raw route;
- **Compare with live** (workspace files only): the comparison word.

A row that is not `ok` offers none of them.

## Citations and entry ids

An entry id is `output`, `fields` or `file:<scope>:<path>`, the path
percent-encoded segment by segment (every byte except `A-Z a-z 0-9 - . _ ~`;
`/` stays literal). A citation names one stored entry:

```
r:<snapshot_id>/<entry>
r:<snapshot_id>/<entry>#[a,b)
```

`snapshot_id` is 64 lowercase hex characters, the sha256 of the manifest's
canonical bytes. `#[a,b)` is a byte range with `a <= b`, on character
boundaries of the text. The dashboard converts it to UTF-16 offsets for
display only.

A record of a package entry is `r:<snapshot_id>/<entry>@<record id>`, the
id percent-encoded, a range counting in the record's text. It is read
through an evidence session (`run_evidence_read`, `apb evidence read`);
`run_result_read` and `apb results show` refuse it and read whole entries
only.

## Verification

Every read re-verifies: the manifest must hash to its id and name the
stamping settlement (node, item and run), and every object must hash to its
entry's digest, have its size and be UTF-8. The listing re-hashes every
object of every row.

- `missing`: no manifest under that id.
- `corrupt`: a manifest or object that is absent, changed, or not the
  stamping settlement's. A corrupt entry is never served as content: a read
  answers `corrupt` on every surface alike. An entry whose object is absent
  or changed is listed without `digest` and `size` (`unverified`), since
  nothing verifies the stored ones.

## Redaction on read

Every answer also passes the run redactor, so a secret known only after
capture (a connector secret configured later) stays hidden, and
`redacted_on_read` says so. Such an entry is shown by what is served, never
by what is stored:

- a path the redactor changes reads `<redacted path n>`, numbered after the
  ones capture gave, and the entry id is built from it, so no spelling of
  the value leaves, the percent-encoded one included; the stored id no
  longer names the entry;
- `digest` and `size`, in the listing and in a read, are the served
  text's, so the stored digest cannot confirm a guess of the value, and a
  reader pages against the served length;
- the live comparison answers `not_comparable`.

When the run's secret values cannot all be read, no text is served
(`secrets_unresolved`).

Ranges and windows count in the text as served. When the read redaction
changed the text, the offsets therefore do not map onto the stored bytes;
this keeps a range from cutting a secret value in half. A window's `offset`
must sit on a character boundary, and its end moves back to one.

## Comparing with the live file

`compare_live` (MCP `run_result_read` with `compare_live: true`, the `live`
route, the dashboard's Compare) re-hashes the stored object first, like
every read (`corrupt` otherwise), then the live file through the same
no-follow walk, and answers one word, never live content:

| Word | Meaning |
|---|---|
| `same` | the live file has the stored digest |
| `differs` | it has another digest, or is over 1 MiB |
| `missing` | no live file, or no live tree |
| `unreadable` | a link, a FIFO or another non-regular file, a hard link, or a read error |
| `not_comparable` | the entry is the output or fields, a `run` file, redacted at capture or on read, or has a redacted path |

The live tree is the one capture read the file from: the run's resolved
working tree, else the project root. A node's own `workdir` is not used,
by capture or by the comparison.

## Cleanup

```
apb results clean <run> [--dry-run]
```

A capture is one transaction under the run's results lock: it reserves room,
stages its objects in `results/staging/<token>/` (named in `txn.json` before
their bytes exist), moves them to `results/objects/<aa>/<hex>`, renames the
manifest to `results/snapshots/<id>.json`, journals the stamp, and only then
removes its staging. A transaction's lock is held for its whole life, so a
free lock means its writer ended.

Cleanup, under the same lock:

1. resolves every transaction whose writer ended: completed when the journal
   names its manifest, rolled back otherwise;
2. removes a manifest or object only when no journal stamp and no open
   transaction names it.

The journal is read textually: any run of 64 lowercase hex characters in
`events.jsonl` protects that snapshot, so an event this binary cannot decode
or a torn last line still protects what it names. When a kept manifest does
not verify, no object is removed. `--dry-run` takes no lock and writes
nothing. A crash at any stage leaves nothing readable the journal does not
name; cleanup completes or rolls back the rest.

`apb doctor --run <id>` adds a `results` row when the run has a result
store: OK with the snapshot count, or WARN with the unreferenced snapshots,
objects and bytes, the transactions whose writer ended and the cleanup
command. The doctor writes nothing.

Snapshots live inside the run directory and leave with it; cache prune and
clear never touch them.

## Which attempt made a commit

When a node commits, `artifacts_committed` now lists `HEAD` as each attempt
left it (`attempts`) and the attempt that ended the execution (`attempt`).
`apb runs <id>` and the run page then say "committed by attempt N" on each
commit row, N being the attempt's number; the `commits` block of
`run_status` carries the same as `by_attempt` (per listed commit, the
attempt's `id` and `number`), absent when no head was recorded. It is
display only, and a merge or a history rewrite inside the range can make it
inexact. Script nodes
journal no attempts, and older runs carry no heads; their commit rows read
as before.

## What is not promised

For an execution apb snapshotted, the stored entries are the bytes of the
declared files and the output as apb read them at settlement, after the
secret scrub, addressed by digest; a read returns exactly those bytes or
reports them corrupt. Not promised:

- that the bytes are what the node wrote: another local process may have
  written the file;
- a secret the scrub does not recognize, such as a copied credential file
  under an innocent name and without a token shape;
- binary content, and files beyond the limits (they are listed as omitted);
- the content of an entry the scrub changed;
- anything about meaning;
- privacy from the dashboard's hosts: a dashboard without keys serves result
  reads to any host it allows, as it serves run detail.

## Older apb

Every new field is display and storage only; no routing decision reads it.

| Change | Older apb |
|---|---|
| `node_finished.result` | dropped; same routing |
| `list_item_settled.result` | dropped; where the event itself is unknown it is skipped up to the next checkpoint, as every list event already is |
| `artifacts_committed.attempt`, `artifacts_committed.attempts` | dropped |
| the node field `results:` | dropped on load: the run takes no snapshot |
| `runs/<id>/results/` | never opened |
