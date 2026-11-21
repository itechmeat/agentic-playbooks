## ADDED Requirements

### Requirement: One binary serves both revisions
`apb mcp` SHALL serve an old-revision host through `initialize` and a 2026-07-28 host through `server/discover` with tier 0 and per-request metadata, from one binary, on the rmcp version it already pins. Wire tests SHALL pin the envelopes an old host relies on, the sorted tool names and two named tool definitions, not apb's whole tool list.

#### Scenario: A 2025-06-18 host
- **WHEN** a client opens with `initialize` at `2025-06-18`, lists tools and calls `playbook_catalog`
- **THEN** the envelopes, the sorted tool names and the definitions of `playbook_catalog` and `run_status` equal the recorded fixture

#### Scenario: A 2026-07-28 host
- **WHEN** a client sends `server/discover` with per-request `_meta`
- **THEN** the answer lists `2026-07-28` and carries the tier-0 instructions

### Requirement: Cache hints only on the current revision
`tools/list` SHALL carry `ttlMs` and `cacheScope` for a 2026-07-28 request and SHALL carry neither, nor `resultType`, for an older one.

#### Scenario: An older host
- **WHEN** a `2025-11-25` client lists tools
- **THEN** the result has no `ttlMs`, `cacheScope` or `resultType`

### Requirement: The docs say what the wire tests prove
`docs/HOST-INTEGRATION.md` and `docs/MCP.md` SHALL state the rmcp version in use, the contract per revision that the wire tests prove, and that the per-host matrix and the tier-0 size limit on 2026-07-28 are not measured yet.

#### Scenario: The doc after the change
- **WHEN** a reader opens `docs/HOST-INTEGRATION.md`
- **THEN** it no longer says that no host ships the new revision and it names the host matrix as open
