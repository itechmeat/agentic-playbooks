# Moving `current`, notification filters and wait notices

Date: 2026-11-01. Status: accepted, built. Author-facing usage:
`docs/NOTIFY.md` (filters, the new transitions, older versions),
`docs/HOWTO-authoring.md` ("The current version"), `docs/MCP.md`
(`run_notify`).

## 1. Goal

Two things a person relies on had loose ends:

1. Which version of a playbook runs is the `current` pointer, and several
   code paths wrote it with no lock, no expectation and no record: a
   person in the dashboard, a promoted candidate, a promoted in-run patch,
   a new version, schema migration and the copy into another scope. A
   crash between writing `current` and updating the candidate could leave
   a candidate advertised beside a newer `current`, and nobody could say
   who moved the pointer or when.
2. Notifications covered every playbook and every way a run ends, and
   said nothing while a run waited on a host or on its supervisor.

This document records the design as built: one operation that moves
`current` (section 2), a settings file for notification filters
(section 3), outcome filters and two new transitions (section 4), and
how older versions of apb behave (section 5).

## 2. One operation moves `current`

### 2.1 The operation

`apb_core::versioning::current` owns every write of `current`:

- `move_current(dir, req)` takes the playbook's pointer lock (the
  candidate lock, `.candidate.lock`) and calls `move_current_locked`.
- `move_current_locked(dir, req, held)` runs under a lock the caller
  already holds; candidate settlement holds it and passes it on, so the
  lock is never taken twice.
- `history(dir, limit)` and `recover(dir, held)` read and repair.

A request names the target, an optional expected current value, who
moves it (`person` from the CLI or the dashboard, `candidate_promotion`
with a bounded basis such as `successes:2` or `quality`,
`patch_promotion`, `created`, `migration`, `scope_move`), an optional
reason, and for a candidate's own promotion the trial record to finish.

Under the lock the operation:

1. refuses in a read-only process, before any change;
2. recovers an interrupted earlier move;
3. checks the playbook is not frozen (a `migration` and a `scope_move`
   carry the definition as it is and are exempt), the target has a
   `playbook.yaml`, the expected value still holds (`stale` otherwise; an
   expectation of "unset" holds only while `current` names nothing), a
   person's target is not the candidate (`is_candidate`: a candidate is
   promoted by its trial or by the candidate decision), and every trial
   record the move updates reads (`record_unreadable` otherwise, so a
   refusal leaves the candidate on trial as it was);
4. writes an intent file, `current.intent.json`, naming the change id,
   both values, who moves it, the candidate it ends and the trial record
   it finishes;
5. ends another candidate's trial (`superseded`);
6. writes `current`;
7. when the target is the candidate, clears the candidate pointer and
   writes its trial record `promoted`;
8. appends one line to `current-history.jsonl`;
9. removes the intent.

A failure before step 6 is an error, and the next recovery records the
change as aborted; nothing rolls forward. A supersede made in step 5
before such a failure is kept: the candidate's trial stays ended, and the
aborted line names it. An intent whose promoted candidate is not its own
target was edited by hand; recovery refuses it. A failure after step 6 is a
success with `audit_pending: true`, and recovery finishes steps 7 to 9,
each of which is idempotent. A crash between steps 6 and 7 leaves the
candidate pointer naming the version that is now `current`; the start
gate never offers such a candidate as a trial, and recovers the intent
on its next choice. Recovery runs in every move, in `history`, in
candidate settlement and in the gate's candidate choice when an intent
exists.

Moving grants nothing: a version that waits for script approval may
become `current`, the answer says `awaiting_approval`, and starts stay
refused until a person approves it.

### 2.2 The history

Each line holds the change id, the time, `from`, `to`, who moved it, the
reason, the candidate it ended and `prev`, the sha256 of the line before.
`history` reports `incomplete` when the chain breaks (a line lost or
edited), when a line's `from` is not the previous line's `to`, or when
the `current` file names another value than the last line (a move by an
older binary or by hand). It reports `pending` while an intent waits.
Outside a read-only process it reads under the pointer lock, so a move in
flight never shows as a gap.
The audit compares values, never file times or inodes, so a clone or a
copy of a project reads complete. Its limit: writes outside apb that end
on the recorded value (A to B to A) leave nothing to see, and nothing is
wrong afterwards. A playbook with no recorded move yet (no history file,
or only aborted lines) has no last value to compare, so a move by an
older binary before the first recorded one is not seen.

A reason is free text, so it is scrubbed with the workspace scrubber
(connector secrets, local paths, addresses, token shapes) and cut to 256
characters before the intent is written. A reason with nothing left
after the scrub, or that still looks like a secret once cut, is dropped:
the line is kept without it. Text derived from a run is never stored:
the basis of a promotion is a bounded value and the run is named by its
id.

### 2.3 Every writer

New versions with `make_current` and new playbooks, schema migration,
in-run patch promotion, candidate promotion, the CLI, the HTTP API and
the dashboard all call the operation; the old `promote_version` and
`promote_version_by_person` are gone. Schema migration copies `current`
into its backup and has no rollback that writes it back, so there is
nothing else to route.

The copy into another scope (`playbook_make_global`) takes the source's
pointer lock first, recovers the source, copies it while holding the lock
(so `current`, the candidate, the trial records and the history come from
one moment), leaves out the lock files and the intent, and appends one
`scope_move` line to the staged copy before it is renamed into place. The
order is always the source's lock, then the destination's.

### 2.4 Surfaces

- CLI: `apb version list|use|history <playbook>` with `--scope
  project|global`; `use` takes `--expect` and `--reason`, and is refused
  inside a run step.
- HTTP: `POST /api/playbooks/{id}/current` with `version`,
  `expected_current` and an optional `reason` (409 `stale`, `frozen`,
  `is_candidate`; 200 with the move, `audit_pending` included) and `GET
  /api/playbooks/{id}/current-history`. The old promote route moves
  through the same operation without an expectation and is deprecated.
- Dashboard: "Make current" opens a dialog that shows the move from the
  current version to the chosen one and takes a reason; a stale answer
  reloads the versions and says so. A history list shows each move, and
  marks an incomplete or pending history.
- MCP: none.

## 3. The settings file `policy.yaml`

Filters that older binaries must never misread live in a file of their
own, beside `config.yaml`: `<config dir>/policy.yaml` for the machine and
`.apb/policy.yaml` for a project. An older apb never opens it, so its
`config.yaml` loads exactly as before.

The only section is `notify:`, with `finished` (the outcomes an end-of-run
notice covers) and `playbooks` (the playbooks a default subscription
covers, `<id>` or `global:<id>`). The machine file sets them; the project
file only narrows them. The reader is strict: an unknown section, key or
value is an error that names it. A present file that does not read
creates no default subscription and the run journals the problem; a run
that asked for notifications is refused with it. `apb doctor` prints a
`policy` line for both files. No apb command writes the file.

## 4. Notification filters and new transitions

### 4.1 Outcomes

A run that ended is `succeeded`, `needs_look` (a success whose outcome
calls for a look), `failed` or `aborted` (stopped by anything but a
person). A run asks for its own filter with `--notify-finished`, `apb
notify <run> --finished=`, the `finished` field of the MCP and HTTP
`notify` argument, or the outcome checkboxes on the run page. An explicit
request wins for every field it names and is not filtered by playbook; a
default subscription exists only when the machine default is on, the
project did not turn it off and the playbook passes both `playbooks`
lists.

A host (MCP `run_notify`) may keep or narrow the filter, never widen it
past the run's current one, or its last one when notifications are off. A
change that leaves the filter or the transitions out keeps the run's
current ones, or the last ones it had, so off and on again through a host
restores them.

The journal names a filtered `finished` as one `finished:<outcome>` name
per outcome in `notify_set.on`, and an unfiltered one as `finished`. The
sweep reads the filter from the journal only, so a resume after a
settings change keeps it.

### 4.2 `host_task` and `supervisor`

Two transitions are asked for by name: `host_task` (a host task nobody
submitted, open until it is submitted or the run ends) and `supervisor`
(a failed or timed-out step parked for the supervisor, open until the
supervisor retries or continues, or the run pauses, is patched or ends).
Their notices name who is expected and never carry a task's prompt or a
failure's detail.

The drive loop does not sweep while an attempt waits for its host or
while the run is parked for its supervisor, and the answer closes the
transition before the loop sweeps again. Both waits therefore run the
same throttled sweep the question wait already ran (at most once a
second, a few seconds at most), through the same send-once ledger.

### 4.3 Commands in notices

A notice names a command only through the next-action renderer, for the
notified run and the very request it is about: the review or publish
request of that seq (with a publish plan's nonce), the question of that
seq, the host task it requested, the pause. When the run no longer has
that request open, the notice names no command, so a late notice for one
review never prints a command that answers the next one. Supervisor
actions and the end of a run name none. The final redaction of the
subject and the body with the run's secret values stays.

## 5. Older versions of apb

| Change | 1.0.0-beta.1 | 0.25.0 |
| --- | --- | --- |
| `finished:<outcome>`, `host_task`, `supervisor` in `notify_set.on` | names it does not know are ignored: a filtered subscription sends no end-of-run notice, a missed notice rather than an unwanted one | `notify_set` is an unknown record, skipped before a checkpoint; no notifications |
| `policy.yaml` | never read: defaults notify every playbook and outcome | no notifications |
| `current-history.jsonl`, `current.intent.json` | never read; an older move shows as `incomplete` once a move is recorded | same |
| `host_task`, `supervisor` in `notify.on` of `config.yaml` or `.apb/config.yaml` | the file does not load: name these transitions per run, or only once every binary on the machine and the team is this version | same |

The journal corpus checks the first row with real binaries: the new
binary starts a run with a `finished:failed` and `host_task`
subscription to a local sink; beta.1's resume to a success sends
nothing, and with a plain `finished` the same resume sends one notice;
0.25.0 reads the run with one skipped record and sends nothing.

No event type is added and no event field changes.

## 6. Non-goals

An MCP tool for `current`; rewriting history; digests or new channels for
notifications; changing a finished run's subscription; launch policies
(a later change extends `policy.yaml` with its own section).
