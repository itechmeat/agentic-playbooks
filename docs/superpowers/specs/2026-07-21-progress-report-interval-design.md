# Playbook progress report interval

A per-playbook setting telling the controlling agent (the chat agent that
launched the run through MCP and supervises it) how often to post a short
progress update into the chat while the run is going, the way a human
supervisor would.

## Field

- Name: `progress_report_minutes`, top-level optional playbook field next
  to `defaults`.
- Type: integer, whole minutes. Valid range 0 to 60 inclusive. 0 (and the
  field absent) means no timer-based reporting.
- Serialization: `#[serde(default)]`; absent in YAML means 0. Existing
  playbooks stay valid and unchanged.
- Validation: a new validator code (next free V-number) rejects values
  outside 0..=60 and non-integer values with a clear message. Playbook
  digest naturally covers the field like any other definition content.

## Semantics

The field is advisory metadata for the run supervisor, not an engine
timer:

- The engine and scheduler do not act on it.
- The MCP layer exposes it so a controlling agent can honor it:
  `playbook_get` returns it as part of the definition, and the run-start
  responses of `playbook_run` (both the `supervise: "self"` and
  `background: true` shapes) carry it back as `progress_report_minutes`
  so the supervisor learns the cadence without a second call.
- The apb MCP server instructions gain one line: when a run was started
  with a nonzero `progress_report_minutes`, the controlling agent should
  post a brief progress note to the user roughly every N minutes while
  the run is active (what changed: nodes finished, questions pending,
  failures), and must stay silent on the timer when the value is 0.

## Web UI

On the playbook page the setting is shown and editable:

- Placement: the existing playbook settings area of the editor (where
  name and description live), labeled "Progress report interval (min)".
- Control: a numeric input that accepts ONLY a whole number from 0 to
  60 - no letters, no decimals, no negatives; invalid input is refused at
  the field level (clamped or rejected, matching how other numeric inputs
  in the editor behave). An empty value or 0 shows as 0 = off.
- Display: when the playbook is viewed (not edited), a nonzero value is
  visible next to the other playbook metadata; 0 renders as "off".
- Saving goes through the ordinary playbook update path (a new minor
  version, digest change, trust drop), like any other definition edit.

## Out of scope

- No engine-side timers, events, or scheduler changes.
- No change to the two board playbooks in `.apb/` (they can adopt the
  field later by an ordinary version bump).
- No notification transport of its own: the chat message is written by
  the controlling agent, not by apb.

## Acceptance criteria

- A playbook with `progress_report_minutes: 15` validates, round-trips
  through save/load, and its value is visible in `playbook_get` output
  and in the run-start response of `playbook_run`.
- Values 61, -1, and 2.5 are rejected by the validator with the new V
  code; 0 and absent behave identically.
- The web editor shows the input on the playbook page, refuses `abc`,
  `-3`, `61`, and `1.5`, saves 0..60, and the view mode renders the value
  (0 as off).
- `cargo test --workspace` and the web `bun run check` / `bun run test`
  pass; no release build is required.
