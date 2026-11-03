---
decided: 2026-11-02
decision-makers: apb maintainers
---

# Billed agent work starts only as the billing settings allow, decided once in the run gate and checked again at every spawn

## Applies to

- The billing half of the launch check (`apb_engine::gate::billing`, called
  through `apb_engine::gate::launch::decide` for every start, trial, plan
  execution, eval and resume of a run never authorized): the basis of every
  executor the run's profiles bind (primary, fallbacks, tier executors,
  over the whole tree) is predicted from the facts a spawn would see
  (`apb_engine::adapter::cost_reading`, each agent's own module, decision
  record 0005), and the `billing:` section of `policy.yaml`
  (`apb_core::billing`, the machine's file narrowed by the project's)
  decides it: `allow` runs, `ask` refuses with the launch nonce, which then
  covers the predicted set, and only a person confirms (a terminal
  question, the nonce on the CLI, the host relaying the person's answer
  over MCP, the dashboard's confirmation), `deny` refuses everyone.
- The record a run keeps (`LaunchRecord.billing`, in the manifest or on the
  `run_resumed` that added to it): the bases the settings allowed and the
  agents and bases a person confirmed, never a nonce.
- Every agent spawn of a run (`apb_engine::pause::billing::spawn_hold`): the
  basis read from the facts that spawn sees must be covered by the run's
  record, else a node attempt pauses the run (`billing (<agent>: <basis>)`)
  before the agent starts, a terminal answer or a judge fails, and context
  compaction is skipped. Only a person resumes a billing pause
  (`apb_engine::resume_launch`), which adds the entry to the record.
- A run budget's dollar limit counts every reported cost unless
  `billing.budget_counts_subscription` is `false` on the machine; the
  setting in force is recorded on each `budget_set`.

## Why

People differ: some never accept billed work, some accept it every time,
and an enterprise may run on api keys only. A login can change while a run
works (a key set mid-run, a fallback on another login), so a check at the
start alone would let billed work through later. A subscription can bill
extra usage under the same login, so a budget that left subscription
equivalents out by default could let a run spend money nobody counted.

## Checked by

- apb-core `billing::tests` (unit tests: the defaults, a project narrows and
  never loosens, a record covers allowed bases and the confirmed agent only)
- apb-core `agents::codex::billing::tests` (unit tests: an account login is
  a subscription, an api key login and a key variable are billed, an
  endpoint or a named provider is unknown)
- apb-core `policy_file_test::an_unknown_section_key_or_value_is_an_error_that_names_it`
- apb-engine `pause::billing::tests` (unit test: the reason round-trips and
  only an engine pause holds)
- apb-engine `budget_gate_test::the_money_budget_counts_a_subscription_unless_the_machine_says_it_never_bills`
- apb `billing_cli_test::billed_work_starts_without_a_terminal_only_with_the_refusals_nonce`
- apb `billing_cli_test::an_unknown_basis_is_held_and_a_subscription_runs_without_a_question`
- apb `billing_cli_test::at_a_terminal_allow_runs_deny_refuses_and_a_project_only_narrows`
- apb `billing_cli_test::a_redirected_config_dir_does_not_loosen_billing_for_an_agent_channel`
- apb `billing_cli_test::at_a_terminal_the_person_is_asked_about_the_billed_agents`
- apb `billing_cli_test::a_mid_run_switch_to_billed_pauses_until_a_person_confirms`
- apb-mcp `server::tests::billing::billed_work_starts_only_with_the_nonce_the_host_relays_and_never_from_a_step`
- apb-server `launch_api_test::billed_work_starts_from_the_dashboard_only_with_the_refusals_nonce`
