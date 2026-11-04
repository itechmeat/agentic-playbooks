# Evidence freshness

A check's result is a fact about the tree it ran on. When a later node of
the same run changes that tree, the result is no longer a fact about the
tree the run left. The run outcome must not show it as current.

## What was reproduced

`crates/apb-engine/tests/suite/evidence_freshness_test.rs` drives a real
run: an agent step writes `src/lib.txt`, its `success_check` script passes
on that content (candidate A), then a script node rewrites the file
(candidate B). Before this change the outcome listed
`[passed] success check: implement` and counted it in the headline
(`1 of 1 check passed`).

What already closes it, and what does not:

| Evidence | When it is taken | Can a later node change its subject |
| --- | --- | --- |
| goal criterion (`script`, `marker`, `list`) | at the finish node, after every node | no |
| baseline comparison | after snapshot at the finish node | no |
| `success_check` script | right after the attempt, mid-run | yes: the reproduced gap |
| list item check (`script`, `connector`, `judge`, `output`) | per item, mid-run | yes, for a script check |
| `success_check` marker, `output` item check | the attempt's own output text | no, the text is journaled |

A goal script criterion sees candidate B and fails, so it closes the gap
only when the author repeats the check as a goal criterion. The stale
success check was still listed next to it as a pass.

## Binding

A script `success_check` that passes journals `evidence_bound` right after
the script exits, inside the attempt, before `attempt_finished`:

- `node`, `attempt`, `check: success_check`;
- `root`: the directory the check ran in (the attempt's working
  directory);
- `scope`: the globs the check declares (empty when it declares none);
- `fingerprint`: the existing workspace fingerprint of that scope, taken
  after the check exited (so a file the check writes itself is part of
  it). A declared scope uses `apb_core::fingerprint::files_fingerprint`
  (the node cache's declared-inputs fingerprint, every regular file the
  globs match, ignored ones included). No scope takes none: the node
  cache's whole-tree `git_fingerprint` sees neither ignored files (a
  rebuilt `dist/`, a `.env`) nor paths above the check's directory (a
  sibling package of a node `workdir`), so equal ones would prove
  nothing. `None` also when it cannot be taken (a bad glob, an unreadable
  file). A list item never binds (V85 refuses a `success_check` on a list
  node, and the outcome reads a list by rule 3).

At the finish node, after the baseline's after snapshot and before the goal
criteria, the drive journals one `evidence_observed` holding the same
fingerprint again for every distinct declared `(root, scope)`: of each
node's binding the outcome reads (rule 1) and of its latest binding when
that differs. A check scope is declared on the script form:

```yaml
success_check: { script: scripts/check.sh, scope: ["src/**", "Cargo.toml"] }
```

The bare string form stays as it is (no scope). V163 refuses an empty
`scope` and a glob that does not compile.

## The reading

One fold (`run_outcome::freshness`) gives each piece of evidence a
`freshness` of `current`, `stale` or `unknown`, with a reason:

1. The binding is the latest `evidence_bound` of the node before its
   latest successful `node_finished`. A later passing check of the same
   node re-binds it: that is the recheck.
2. With an `evidence_observed` entry for its declared `(root, scope)`
   after the binding: equal fingerprints are `current`, different ones
   `stale` (a file the check reads changed).
3. Without one (a run under way, a run that ended before a finish node, a
   fingerprint that could not be taken, a check that declares no scope):
   `unknown` when a node that can act (`apb_core::effects::node_acts`, a
   `read_only` node aside) started after the check or was still running
   when it passed (started before, finished after or never), else
   `current`.
4. A list script item check has no binding. Rule 3 applies from the list
   node's latest settlement.
5. A pass restored from the node cache (`node_cache_hit` in the
   successful execution, no binding) ran its check in another run. Rule 3
   applies from the hit, and its detail names the run the cache came
   from.
6. Evidence from an older journal (a success check with no binding and no
   cache hit) carries no `freshness`; it reads as before.

Only a pass carries a `freshness`: a failed check stays failed, and the
headline counts "not current" among passes only.

`Evidence::passed` holds only for a pass that is not `stale` or
`unknown`. The headline counts the others apart (`0 of 1 check passed,
1 not current`), and every surface that reads the outcome (`run_status`,
`run_report`, `apb runs <id>`, the dashboard run page and the HTML report)
shows the word and the reason.

### Scope rationale

An unrelated edit does not invalidate a check that says what it reads.
Without a declaration apb cannot tell which files a script reads, and a
git fingerprint cannot see every file it might read, so once a node that
can act ran after it an unscoped pass is `unknown`, never `stale` and
never `current`. The node's `inputs.files` and `outputs.files` are not
reused as the scope: they say what the node reads and what it keeps, not
what its check reads, and a scope that is too narrow would show a stale
pass as current, the defect this change removes.

## Executed and self-reported evidence

Every piece of evidence also carries its `basis`:

- `executed`: a command apb ran, decided by its exit or its JSON (a
  script `success_check`, a script goal criterion, a script item check, a
  baseline script or project-checks source);
- `read`: something apb read itself (a connector response, the journal's
  list counts);
- `agent_text`: words the agent wrote (a marker, an `output` item check);
- `model`: a decision model's judgment (a `judge` item check).

The lines say the basis next to the result, so a marker the agent printed
never reads like a test suite that ran.

## Compatibility

`evidence_bound` and `evidence_observed` are new events, safe to skip: a
checkpoint (`node_finished`) follows each. The new `Evidence` fields are
serialized only (`basis` always, `freshness` when known); nothing reads
them back from a journal. The script form with `scope` is a new playbook
shape an older apb refuses to parse, like every new field. Each map form
refuses a key it does not know, so a map that mixes the forms
(`{ marker, scope }`, `{ script, scope, marker }`) is a parse error
rather than a marker check that silently drops the rest. An in-binary
compatibility test and a journal corpus row check that an older reader
reads, routes and stops a run that carries both events.

## Non-goals

- No new identity service and no new gate: the binding reuses the node
  cache's fingerprints, and freshness only changes what the outcome says.
  It never fails, pauses or reroutes a run.
- Edits after the run ended (a person's commit) are not followed. The
  results' live comparison (`docs/RESULTS.md`) and an approval's
  `live: check` cover a file a person wants compared with the tree now.
- Within one list visit, items share the tree; an item's later sibling
  editing its files is not ordered against its check.
- A sibling running in the same parallel batch while a check runs is not
  ordered against it beyond rule 3: with a finish node the observation
  catches its writes; without one, a sibling that can act and was still
  running when the check passed makes the pass `unknown`.
- Goal criteria and baseline comparisons are not bound: nothing in the
  run runs after them.
