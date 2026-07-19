# Node result cache (incremental execution)

Date: 2026-07-19
Status: approved design, pending implementation plan

## Summary

`apb` gets a content-addressed cache of node results so that a repeated run of
a playbook over a weakly changed workspace skips nodes whose inputs did not
change. The user-facing framing is incremental execution: apb executes only
the steps whose results could have changed. The cache is the internal
mechanism.

Explicitly out of scope for this design: KV-cache / LMCache style model-level
caching (apb does not control the model or its inference server), semantic
similarity hits (unsafe as an automatic default for coding tasks), agent
session reuse (context reuse, a separate concern), and patch-mode caching of
workspace-writing nodes (a possible later stage).

## Driving scenario

The primary scenario is a repeated run of the same playbook against a
workspace that changed little or not at all (lint, review, analysis
playbooks). Cache reuse across runs saves whole agent executions. Cheap retry
of a failed run and author iteration benefit automatically but do not drive
the design.

Cache reuse is strictly distinct from the existing resume (continue the same
run) and retry (re-execute a node inside the same run) mechanisms. Cache
reuse takes a result from another run and is visible as such in events and
UI.

## Trust model

apb's existing effects philosophy is "declared cannot narrow inferred": an
author's declaration is not taken on faith. The cache follows it with a
hybrid model:

- The node-level `cache: auto` declaration expresses intent only. It makes a
  node cache-eligible; it proves nothing.
- Admission to the cache requires a verified fact, checked after the node
  ran: the workspace fingerprint before and after the node is identical (the
  node effectively did not write to the workspace), and every connector call
  the node made was from the connector's `read_only` function set (the engine
  mediates connector calls and sees this directly).
- A node that fails this verification is executed normally and simply not
  admitted; the run records `node_cache_rejected` with the reason. Failed,
  cancelled, interrupted, and timed-out nodes are never admitted.

Known blind spot: direct network activity by the agent that bypasses
connectors is not observable and therefore not verifiable. Mitigations: the
cache is an explicit per-node opt-in by the author, and a node may declare a
TTL after which the entry expires.

## Cache key

The key is a digest over:

- cache format/schema version (allows changing the key recipe without
  poisoning old entries);
- the canonicalized node definition; for `script` nodes additionally the
  script file content digest and the runner;
- the fully rendered prompt (template rendering already folds in upstream
  node outputs and params, so changed upstream results invalidate downstream
  entries with no extra machinery);
- for `agent_task` nodes: the profile `bundle_digest` (already covers
  profile.yaml, SOUL, and actual skill contents), the agent, and the model;
- digests of the connectors bound to the node;
- the workspace fingerprint (see below).

The playbook id and version are deliberately not part of the key. The key is
purely content-addressed, which gives free reuse across playbooks of the same
project. Provenance (which run and playbook produced the entry) lives in the
cache record, not in the key.

## Workspace fingerprint

Two modes, chosen per node:

- Default, git-aware: digest of `HEAD` plus the dirty state (staged diff,
  unstaged diff, untracked files). Works with zero declarations but is
  coarse: any repository change invalidates every entry keyed this way.
- Refined, input-based: when the node declares `inputs.files` globs, the
  fingerprint is the hash of exactly those files. Precise invalidation; a
  forgotten input can cause a false hit, which is the standard build-system
  trade-off and is why `cache: auto` stays an explicit author decision.

If the project is not a git repository or git is unavailable, nodes without
declared `inputs` are not cacheable (recorded as a miss with that reason);
nodes with declared `inputs` keep working through file-set hashes.

The run directory is not part of the workspace fingerprint. A node that
writes only its declared outputs into the run directory stays read-only from
the verifier's point of view.

## Node results and artifacts

Today a node result is a plain string (`NodeFinished.output`). The design
adds first-class artifacts:

- A node may declare `outputs.files`: paths (in the run directory or the
  workspace) that the engine captures into the content-addressed store after
  execution, as artifacts with digests.
- On a cache hit the artifacts are restored alongside the output.
- Paths declared in `outputs.files` are excluded from the before/after
  workspace fingerprint comparison: they are the node's declared products,
  not an undeclared side effect, and on a hit they are restored into place.
  Verification stays meaningful for everything the node did not declare.
- `NodeFinished` gains an `artifacts: Vec<ArtifactRef>` field, added with
  `#[serde(default)]` per the project rule for new `EventPayload` fields.

Downstream nodes and the UI get verifiable content-addressed artifacts
instead of only a text blob.

## Storage

Project-local, file-based, no SQLite:

```text
.apb/cache/
  format            # single line with the store format version
  records/ab/<key>.json
  objects/ab/<digest>
```

- Records and objects are written through `apb_core::fsutil` (atomic temp +
  rename, 0600 on unix).
- Objects are content-addressed; the filename is the digest of the content.
- Concurrent writers are safe by construction: the same key produces the same
  content, and atomic rename makes the last write idempotent.
- If an index is ever needed for scale, it can be added on top without
  changing the record format.

Record shape:

```json
{
  "format_version": 1,
  "key": "sha256:...",
  "created_at": "...",
  "node_type": "agent_task",
  "provenance": {
    "run_id": "...",
    "playbook_id": "...",
    "playbook_version": "...",
    "node_id": "..."
  },
  "profile_bundle_digest": "sha256:...",
  "workspace_fingerprint": "sha256:...",
  "verification": {
    "workspace_unchanged": true,
    "connector_calls": "read_only"
  },
  "output_digest": "sha256:...",
  "artifacts": [
    { "name": "findings.json", "digest": "sha256:...", "path": "..." }
  ],
  "ttl_seconds": null
}
```

## Engine integration

Both integration points wrap the existing `execute_node` call in the
scheduler drive loop:

1. Lookup, before the node runs. If the node declares `cache: auto` and the
   run was not started with `--no-cache` or `--refresh-cache`: build the key,
   look up the record, check TTL. On a hit, append `node_cache_hit` and a
   `NodeFinished` with status `succeeded` and the restored output, then
   restore artifacts. `NodeStatus` is not extended: for the DAG a cached node
   is simply succeeded; the UI derives a distinct `cached` badge from the
   event. On a miss, append `node_cache_miss` and execute normally.
2. Admission, after the node ran. If the node succeeded and post-hoc
   verification passed (workspace fingerprint unchanged, connector calls all
   read-only), write the object(s) and the record and append
   `node_cache_stored`. Otherwise append `node_cache_rejected` with the
   reason.

New `EventPayload` variants: `NodeCacheHit { node, key, source_run }`,
`NodeCacheMiss { node, key }`, `NodeCacheStored { node, key }`,
`NodeCacheRejected { node, reason }`. No separate lookup event: a lookup
always ends in a hit or a miss.

## Policy and anti-TOCTOU

The `RunPermit` model does not need to be extended. The profile
`bundle_digest` is part of the cache key, so a hit mathematically guarantees
the cached result was produced by exactly the bundle the permit pinned for
the current run; otherwise the key would not match.

One addition on the restore path: the engine verifies the digest of the
object content against `output_digest` from the record before using it. This
protects against cache file tampering between lookup and read. A corrupt or
mismatching object is treated as a miss and the record is deleted; it is
never a run error.

## YAML surface

```yaml
- id: analyze
  type: agent_task
  profile: architect
  prompt: |
    Analyze {{params.module}} and propose improvements.
  inputs:
    files: ["src/auth/**", "Cargo.toml"]
  outputs:
    files: ["findings.json"]
  cache: auto              # shorthand
  # or the full form:
  cache:
    mode: auto             # auto | off (default: off)
    ttl: 7d                # optional; parsed by the existing duration parser
```

Validator additions (next free codes in the V01+ range):

- `cache: auto` on a node type that cannot cache (condition, human review,
  nested playbook) is an error;
- `ttl` without `mode: auto` is a warning;
- invalid globs in `inputs.files` / `outputs.files` are an error.

## CLI surface

```bash
apb run <id> --no-cache        # ignore the cache, write nothing
apb run <id> --refresh-cache   # skip lookup, overwrite via admission
apb cache status               # size, record count, per-playbook breakdown
apb cache inspect <key>        # record and provenance
apb cache prune [--older-than 30d] [--max-size 1g]
apb cache clear
```

## Web UI

Minimal in this iteration: a `cached` badge on the node in RunView, derived
from `node_cache_hit` in the events the UI already loads, and the cache
events shown in the shared event list. No dedicated cache page.

## Error handling

The cache never fails a run; every failure degrades to a miss.

- Corrupt record JSON, digest mismatch, missing object: miss, and the broken
  record is deleted.
- Admission failure (object write error, disk full): recorded as
  `node_cache_rejected` with a store-error reason; the node stays succeeded
  and the run continues.
- `prune` / `clear` racing an active run: after restore the run no longer
  needs the cache files; between lookup and restore the digest check catches
  the race and degrades to a miss.
- Failed, cancelled, interrupted, timed-out nodes are never admitted.

## Testing

- Unit, `apb-core`: cache key stability (golden test: identical input gives
  an identical key; changing any single component changes the key);
  fingerprints on a fixture git repository (clean, staged, unstaged,
  untracked); parsing of the `cache:` shorthand and full form; TTL; validator
  codes.
- Integration, `apb-engine` (mock-agent style, like the existing
  profile_run tests): a second run hits and does not launch the agent; a node
  that mutated the workspace is rejected and not admitted;
  `--refresh-cache` overwrites; a changed profile bundle invalidates;
  artifacts restore with correct digests; a non-read-only connector call
  blocks admission.
- E2e: a full playbook twice, asserting events and statuses; the `cached`
  badge derivation covered by vitest on pure logic.

## Implementation order

A sequence, not a scope cut. Every step passes the project gates
(`cargo fmt --check`, `clippy -D warnings`, code-ranker).

1. `apb-core`: `cache` module (types, store, key builder), `fingerprint`
   module, schema additions (`cache:`, `inputs:`, `outputs:` on nodes),
   validator codes.
2. `apb-engine`: lookup and admission in the scheduler for `script` nodes.
3. `apb-engine`: `agent_task` with post-hoc verification and the connector
   read-only check.
4. Artifacts: capture of `outputs.files` into the store, restore on hit, the
   new `NodeFinished` field.
5. CLI: `apb cache *` subcommands and the `apb run` flags.
6. Web UI: the `cached` badge and cache events in RunView.

## Known limitations

- Nodes executed on the concurrent branch path (parallel fan-out) bypass the
  cache entirely: no lookup, no admission, no cache events. Only the
  sequential drive-loop path is cached. Routing the parallel path through
  the same lookup/admission wrapper is a follow-up.
- The git-aware fingerprint requires a git work tree with at least one
  commit; without one, only nodes with declared `inputs` are cacheable.

## Future stages (recorded, not designed here)

- TTL-driven revalidation and negative caching for cheap checks.
- Shared or remote content-addressed store; export and import of entries.
- Patch-mode caching for workspace-writing nodes (base fingerprint, cached
  patch, expected resulting fingerprint, optional cheap validation).
- Semantic candidate retrieval, surfaced only as a suggestion or as reference
  context for the agent, never as an automatic hit.
