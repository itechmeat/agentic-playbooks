---
decided: 2026-10-28
decision-makers: apb maintainers
---

# An isolation level is a contract apb checks before every spawn: `full` refuses what this machine cannot enforce, `best_effort` journals what it could not

## Applies to

Every place that decides or reports how an isolated `agent_task` runs
(`crates/apb-core/src/isolation/contract.rs`, the one pure `evaluate` all of
them ask):

- the run gate every launch surface calls (`gate/isolation.rs`), which
  returns `isolation_unavailable` before anything of the run is written;
- run preparation, which checks again with the run's real execution mode;
- every attempt, a retry and a resume included (`scheduler/node/sandbox.rs`),
  so a sandbox that went away mid-run fails the node before a spawn;
- the sandboxed `success_check`, which runs inside the same sandbox;
- the "will it fit here?" report, through the gate's per-node contracts;
- an attempt that cannot have its sandbox after all (a host fallback step,
  a sandbox that could not be set up) asks `without_sandbox`, the same
  answer `evaluate` gives for a machine without one: `full` refuses,
  `best_effort` runs and records why;
- every run surface (`apb runs <id>`, MCP `run_status` and `run_report`, the
  dashboard), which shows the `attempt_started.isolation` record: holds,
  limits and why no sandbox was used, worded from the one code table that is
  also exported to the dashboard.

`apb doctor` reports the host probe the contract reads, and `apb doctor
--run` the journaled records; neither decides anything.

A new spawn site for an isolated node, a new surface or a new backend goes
through `evaluate` and records what it decided; it never decides on its own.

## Why

An isolation level that is only declared reads as a promise nobody checks: a
playbook written on a machine with a sandbox would run unprotected on one
without, and nothing would say so. Asking one contract at every step keeps
the gate, preparation, the attempt and the surfaces from disagreeing, and
listing the limits on every attempt keeps the guarantee honest about what a
sandbox does not cover (the host uid, the open network, kernel escapes).

## Checked by

Refusal tests, on every machine:

- apb-core `isolation_contract_test::full_refuses_what_the_machine_cannot_enforce`
- apb-core `isolation_contract_test::best_effort_without_a_sandbox_lists_its_limits`
- apb-engine `sandbox_run_test::full_refuses_where_the_sandbox_is_unavailable`
  (the gate and preparation, nothing written or spawned)
- apb-engine `sandbox_run_test::a_host_mode_run_refuses_a_full_node_at_prepare`
- apb-engine `sandbox_run_test::best_effort_without_the_sandbox_runs_in_a_scratch_directory`
- apb-engine `fit_test::an_isolated_node_this_machine_cannot_isolate_blocks_the_fit`
- apb-engine `host_mode_test::a_full_node_never_falls_back_to_a_host_task`

Enforcement tests, on Linux with bubblewrap (with `APB_SANDBOX_TESTS=require`
a missing capability fails them instead of skipping):

- apb-engine `sandbox_run_test::full_with_no_network_enforces_the_contract`
- apb-engine `sandbox_run_test::a_retry_refuses_when_the_sandbox_went_away`
  (it needs a working sandbox for the first attempt, then turns it off)
- apb-engine `sandbox_run_test::an_extra_path_through_a_link_into_a_credential_location_is_refused`
- apb-engine `sandbox_run_test::a_writable_extra_over_the_tree_keeps_its_protections`
- apb-engine `sandbox_run_test::ungranted_skills_of_the_tree_stay_out_of_the_sandbox`
- apb-engine `host_mode_test::a_best_effort_host_fallback_is_recorded_without_a_sandbox`
- apb-cli `sandbox_exec_test::the_scope_blocks_signals_out_of_the_sandbox_or_the_helper_refuses`
