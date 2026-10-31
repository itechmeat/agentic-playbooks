---
decided: 2026-10-28
decision-makers: apb maintainers
---

# Post-start reads come from the write-once run manifest

## Applies to

The run manifest `runs/<id>/manifest.yaml` (`crates/apb-engine/src/manifest.rs`)
and everything that runs after a start: retries, profile fallbacks, resume,
supervisor rebinds, sub-playbooks, connector grants and a profile's
`process_env` policy (the policy is recorded; the plan of kept names is
rebuilt from it at every spawn).

- A start snapshots the resolved profiles, SOUL, skills, invocations and
  connector grants into the manifest, written once.
- Every later read takes them from the manifest, never from the live
  profile, skill or config files.
- A resume compares the recorded agent binaries and invocations with the
  live ones and stops on drift unless the operator allows it.

## Why

A run can last hours and be resumed days later. If a retry read the live
profile, an edit made after the start (a new model, a changed SOUL, a skill
that lost a line) would change a run the gate already approved, and two
attempts of one node could run under different instructions with nothing in
the journal saying so. Trust is granted to exact digests at the start; only a
snapshot keeps the run on what was trusted.

## Checked by

- apb-engine `profile_run_test::manifest_is_write_once`
- apb-engine `profile_run_test::live_profile_edit_mid_run_does_not_affect_a_fallback`:
  the live profile and SOUL edited during the run never reach the fallback
- apb-engine `profile_run_test::live_profile_edit_after_start_does_not_affect_resume`
- apb-engine `agent_env_run_test::a_resume_keeps_the_snapshotted_policy_after_a_live_profile_edit`
- apb-engine `profile_run_test::env_drift_stops_resume_unless_allowed`
