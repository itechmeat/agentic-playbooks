---
decided: 2026-10-28
decision-makers: apb maintainers
---

# The run permit reaches the engine verbatim

## Applies to

The one run gate, `apb_engine::gate::check_run`, every launch surface that
calls it (MCP, the dashboard, the CLI) and the engine start
(`RunOptions::expected_*`).

- The gate verifies lifecycle, `requires`, playbook, profile, script and
  connector trust and sub-playbook pins in one pass and returns a
  `RunPermit`: the playbook digest (which also covers the playbook's
  `scripts/`) and the exact verified profile-bundle, connector,
  connector-account and sub-playbook maps.
- The surface hands those to the engine verbatim, and the engine rejects any
  difference from what it snapshots.
- Nothing recomputes them between the gate and the run.

## Why

Recomputing the map after the gate opens a time-of-check to time-of-use gap:
a skill, a profile or a script edited between the check and the start would
run under a trust decision that never saw it. Handing the permit through
unchanged makes the engine refuse that run instead of running unreviewed
content.

## Checked by

- apb-mcp `profile_e2e_test::permit_bundle_map_is_enforced_against_skill_drift`
- apb-engine `script_test::a_run_refuses_scripts_changed_since_its_permit`
