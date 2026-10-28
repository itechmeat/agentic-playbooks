---
decided: 2026-10-28
decision-makers: apb maintainers
---

# Every agent spawn gets its environment through `adapter::spawn_env`

## Applies to

Every place the engine starts an agent process: node attempts in their
start and resume forms on every transport, the finish answer, the judge, the
supervisor, the quota probe, context compaction and the doctor's paid agent
probe (`crates/apb-engine/src/adapter/spawn_env.rs`).

- One function prepares the environment: the profile's `process_env` plan
  (under `allowlist` it clears the environment and copies back only the
  planned names), the connector scrub, the run context and the agent's
  run-scoped environment.
- The plan is per chain step, so a fallback onto another agent gets that
  agent's set from the same profile policy.
- Connector-referenced variables are withheld under either policy.
- The paid agent probe (`apb doctor --probe-agent`) runs outside a run, under
  the default `inherit` policy: connector variables withheld, and the
  agent's scoped files and claude's minimal settings in the probe's own
  temporary directory instead of a run directory.
- Scripts, checks and host-mode steps are not agent spawns and keep the
  operator environment.

## Why

The environment is where credentials live. A spawn site that built its own
environment would skip the allowlist or the connector scrub, and an agent
would see tokens its profile never granted. One preparation point keeps
every form and transport on the same rules, and a new spawn site gets them by
calling it.

## Checked by

- apb-engine `agent_env_spawn_test::every_agent_gets_only_its_allowlist_in_start_and_resume_forms`
  (node start and resume, connector scrub included)
- apb-engine `agent_env_spawn_test::the_supervisor_spawn_gets_the_allowlist_and_its_own_variables`
- apb-engine `agent_env_run_test::a_finish_answer_runs_under_the_allowlist`,
  `a_judge_emulation_runs_under_the_allowlist`,
  `a_quota_probe_runs_under_the_allowlist` and
  `context_compaction_follows_the_runs_allowlist`
- apb-engine `agent_probe_test::the_probe_spawns_like_a_run_step_without_autonomy`
  and `agent_probe_test::codex_gets_an_isolated_home_in_the_probe_directory`
