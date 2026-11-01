---
decided: 2026-11-01
decision-makers: apb maintainers
---

# An agent launches a playbook only as its launch policy allows, decided once in the run gate, and no agent channel loosens that policy

## Applies to

- The launch check of the one run gate (`apb_engine::gate::launch`, called
  from `gate::check_run` and `gate::check_trial_launch`, and through
  `apb_engine::resume_launch::check_resume_launch` for a resume): every
  start, trial, plan execution, eval, resume, `apb resume --all`, the pause
  waker and the quota probe go through it with an explicit `Initiator` (an agent over MCP, a run step, the
  CLI with or without a terminal, the dashboard with keys, the dashboard
  without keys, the engine) and an explicit `LaunchConfirmation` (a terminal
  question, a CLI flag, a host-relayed nonce, a keyless dashboard's nonce, a
  forwarded grant). No initiator is a person because an environment
  variable is absent, and a dashboard without keys, which anything on the
  machine reaches, is decided like the CLI without a terminal.
- The inputs it reads are the person's own: the config dir a run step or a
  CLI without a terminal names in the environment is compared with the
  default one from the real home, and a redirect that would hide stricter
  launch settings refuses (`policy_file_invalid`), except the config dir an
  `apb eval` hands its cases; and a workspace id keys a floor or an exact
  machine entry only when the projects registry has it registered at that
  root, never because `.apb/workspace.local` names it.
- The policy itself (`apb_core::launch`): a playbook's `agent_launch`, the
  launch floor (`apb_core::launch::floor`, `<config dir>/launch-floors.json`),
  and the `launch:` sections of the machine's and the project's
  `policy.yaml` (`apb_core::policy_file`), combined by
  `apb_core::launch::resolve` into the strictest policy over the run's tree.
- Every publication of a playbook version (`versioning::commit_version_dir`,
  `create_draft_in`, the scope copy, the trash restore): the floor of the
  explicit key is raised under the floor lock before the rename makes the
  version visible, and a save from an agent channel that would loosen it is
  refused there (`launch_policy_loosened`). Only a lowering a person
  confirmed in the editor of a dashboard with keys and `apb launch floor
  --lower-to` at a terminal lower a floor; a dashboard without keys never
  does.
- The grant a passing check returns (`LaunchGrant`): the engine rechecks its
  permit digest and policy state before it writes the record, in
  `scheduler/prepare` for a start (`manifest.launch`, its final check made
  under the floor lock until the manifest is written) and, through
  `resume_launch::commit_resume_launch`, in
  `scheduler/resume::try_journal_resumed` under the run's event lock for a
  resume (`RunResumed.launch`, with the child pins of a run that had none).
  Across a detached spawn the grant travels in a private handoff file
  (`apb_engine::launch_handoff`), never on argv.

What is guaranteed: an agent that uses apb's documented agent channels (MCP,
or the CLI inside a run step or without a terminal) cannot start, trial,
evaluate or resume a playbook whose effective policy is `deny`, and cannot
do so for `ask` without passing the nonce of the refusal; the CLI at an
interactive terminal asks the person. An agent channel cannot loosen a
playbook's declared policy. A run authorized once stays authorized for its
lifetime, pauses and resumes included.

Honest scope: apb does not prove that a person answered. A process that
controls a terminal, signs in to a dashboard with a key, edits the machine
files or forges the handoff file is not stopped. The project file only narrows and
anything that writes the tree can change it. A playbook an agent writes under
a new id has no floor of its own and falls under the machine and project
defaults. Older apb binaries ignore every launch setting.

## Why

Without one check, each launch surface would grow its own reading of who
may launch, and the weakest one (a resume path, a detached child, a waker
that fires later) would become the way around the others. Without the
floor, an agent that may not launch a playbook could save a looser copy of
it and launch that. Without the recheck at the writer, a policy tightened
between the check and the start, or a skill changed after a person
confirmed, would run under a confirmation given for something else.

## Checked by

- apb-core `launch::tests` (unit tests: declaration and floor, an exact
  machine entry, another workspace's entry, the project file only narrows,
  a child's `deny`)
- apb-core `launch_floor_test::an_agent_save_that_loosens_is_refused_and_the_floor_stays`
- apb-core `launch_floor_test::delete_and_recreate_or_restore_keeps_the_floor`
- apb-core `launch_floor_test::a_copy_to_global_carries_the_floor_to_the_machine_wide_key`
- apb-core `launch_floor_test::a_failed_publication_after_the_floor_write_leaves_the_floor`
- apb-core `launch_floor_test::a_duplicate_draft_leaves_the_floor_untouched`
- apb-core `launch_floor_test::a_recreate_scans_the_trash_of_a_key_without_a_record`
- apb-core `launch_floor_test::a_workspace_id_not_registered_here_keys_nothing`
- apb-core `launch_floor_test::a_person_save_raises_and_never_lowers_but_the_editor_lowers_once_confirmed`
- apb-core `launch_floor_test::a_keyless_editor_never_lowers_and_points_to_the_terminal`
- apb-core `launch_floor_test::an_import_raises_the_floor_and_never_lowers_it`
- apb-core `launch_floor_test::a_migration_publishes_through_the_floor`
- apb-core `launch_floor_test::a_damaged_floor_file_refuses_saves`
- apb-core `launch_floor_test::a_lowering_and_a_save_are_serialized`
- apb-engine `launch_policy_test::every_cell_of_the_decision_table_for_every_start_kind`
- apb-engine `launch_policy_test::every_cell_of_the_decision_table_for_resumes`
- apb-engine `launch_policy_test::a_profile_bundle_change_between_refusal_and_retry_needs_a_fresh_nonce`
- apb-engine `launch_policy_test::a_global_floor_holds_in_every_workspace_for_an_explicit_older_version`
- apb-engine `launch_policy_test::an_authorization_covers_its_runs_lifetime`
- apb-engine `launch_policy_test::a_child_turned_deny_after_the_confirmation_does_not_run`
- apb-engine `launch_policy_test::a_policy_change_before_the_commit_refuses_and_leaves_nothing`
- apb-engine `launch_policy_test::the_engine_never_resumes_an_unauthorized_deny_run`
- apb-engine `launch_policy_test::the_records_never_hold_a_nonce`
- apb-engine `launch_policy_test::a_reattached_child_must_match_its_pin`
- apb-engine `launch_policy_test::a_reattached_legacy_child_runs_its_parents_pinned_descendants`
- apb-engine `launch_policy_test::a_driver_started_without_a_resume_checks_the_authorization`
- apb-engine `launch_policy_test::a_damaged_project_file_never_drops_the_machine_section_in_a_backstop`
- apb-engine `launch_policy_test::a_floor_raised_during_preparation_refuses_the_start`
- apb-engine `launch_policy_test::a_supervisor_patch_version_cannot_loosen_a_held_floor`
- apb-engine `quota_pause_test::resume_all_never_probes_a_run_whose_launch_check_was_set_aside`
- apb-engine `candidate_trial_test::a_forward_patch_may_not_loosen_the_launch_policy`
- apb-engine `launch_handoff::tests` (unit tests: a path outside the handoff
  directory, a link, a FIFO, a directory, another user's file)
- apb-mcp `server::tests::launch` (an `ask` refusal and its confirmed retry, a
  `deny` playbook, a run step, an agent's loosening save, through the
  handlers of `playbook_run`, `playbook_run` with `continued_from`,
  `playbook_execute_plan`, `run_resume` and `playbook_trial`, and a trial
  nonce after a profile change)
- apb-mcp `promote_tools_test::an_agents_make_global_never_loosens_the_global_floor`
- apb-mcp `capture_tools_test::capture_discover_save_and_trash_restore_never_loosen_a_floor`
- apb `launch_cli_test::the_cli_without_a_terminal_confirms_ask_only_with_the_nonce_and_never_deny`
- apb `launch_cli_test::a_run_step_never_confirms_a_launch_at_the_cli`
- apb `launch_cli_test::a_detached_driver_resumes_only_with_its_parents_handoff`
- apb `launch_cli_test::an_interactive_supervised_start_of_a_deny_playbook_asks_once`
- apb `launch_cli_test::a_supervised_driver_without_a_valid_handoff_is_the_cli_without_a_terminal`
- apb `launch_cli_test::resume_all_leaves_a_run_the_launch_policy_refuses`
- apb `launch_cli_test::a_person_at_a_terminal_launches_past_a_damaged_settings_file_with_a_warning`
- apb `launch_cli_test::a_damaged_floor_file_warns_a_person_and_refuses_an_agent_channel`
- apb `launch_cli_test::a_forged_workspace_id_does_not_match_another_projects_machine_entry`
- apb `eval_cli_test::an_ask_playbook_is_evaluated_only_with_its_launch_nonce`
- apb `eval_cli_test::yes_never_confirms_a_deny_launch`
- apb-engine `launch_policy_test::a_trial_runs_the_pinned_child_when_its_current_moves_to_deny`
- apb-engine `launch_policy_test::a_supervisor_patch_never_loosens_the_runs_launch_policy`
- apb-engine `forward_patch::tests::a_forward_patch_never_changes_the_launch_policy` (unit test)
- apb-engine `candidate_trial_test::a_launch_retry_with_a_fixed_choice_reaches_the_same_candidate_every_time`
- apb-server `launch_api_test::a_keyless_dashboard_starts_ask_only_with_its_nonce_and_never_deny`
- apb-server `launch_api_test::the_editor_lowers_a_floor_only_with_keys_and_a_confirmation`
- apb-server `launch_api_test::a_never_authorized_run_resumes_by_the_dashboards_rules`
- apb `journal_corpus_test` rows 3, 4, 6 and 9 (older binaries read and resume
  the launch records; they do not enforce them)
