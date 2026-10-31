---
decided: 2026-10-28
decision-makers: apb maintainers
---

# A push leaves a run only through apb's publish step, after a person's consent to the exact plan, and a push whose result is unknown is never repeated on its own

## Applies to

- The drive's arm for a `publish` node (`crates/apb-engine/src/scheduler/publish.rs`)
  and the engine's `publish/` modules: the pin taken before the first acting
  node, the read-only plan, the ledger record before the push, the settle
  and the read-only reconcile.
- The transport builder `apb_core::git::transport`
  (`crates/apb-core/src/git/transport.rs`): the one git call apb runs with a
  transport and the operator's credentials, from a shadow git directory
  made fresh for each plan, push and reconcile, so neither the tree's own
  config nor anything planted ahead of time applies, and a URL rewrite of
  the operator's config refuses the plan. It is the one exception decision
  record 0009 names.
- The review channel's nonce check (`apb_engine::review::post_decision`):
  a `publish` decision must carry the nonce of the open plan, from every
  surface (CLI, MCP, HTTP, dashboard), and is refused inside a run step. A
  dashboard without keys refuses a `publish` decision outright
  (`apb_server::routes::runs`), since anything on the machine reaches it.
- `publish_only` (`apb_core::validate::publish_only_violations`): V120 at
  validation, and a sub-playbook's run held to it at its start.
- The send ledger's `ref` mode (`IdempotencyMode::Ref`): an unknown push
  is reconciled by reading the remote ref back, never pushed again; a
  person resolves it with `apb intent resolve`.

What is guaranteed for apb's own push: it is exactly the plan the person
consented to (digest and nonce), to the URL pinned before agents ran, under
an exact lease on the remote state the person saw, recorded before it
leaves, and never repeated while its result is unknown. URLs carrying
credentials are refused and never journaled.

Honest scope: this holds for apb's own pushes. Only `isolation: full` on
every agent node (and `publish_only`, which makes that an error to break,
holds sub-playbooks to it and refuses a credential the allowlist would
pass) makes the publish step the only way a push leaves the run, apart
from an endpoint that needs no credentials under `network: open`; a
dashboard without keys then refuses a publish consent, since such an agent
reaches it over the loopback. An agent without a sandbox, a script
node or a host-mode step has the operator's credentials and can push by
itself; apb then only flags commits of the run already on the remote
(`outside_push_suspected`), and V119 says so at validation. See
`docs/PUBLISH.md`.

## Why

A push is irreversible and leaves the machine with the operator's
credentials. Without one place it goes through, an agent's push, a stale
consent applied to a newer plan, or a retry after a timeout that actually
reached the remote would publish something no person saw, or publish it
twice. Reusing the review channel and the send ledger keeps one consent
path and one record of what left, on every surface.

## Checked by

- apb-engine `publish_test::approving_the_current_plan_pushes_exactly_it`
- apb-engine `publish_test::a_remote_that_moved_after_the_plan_is_asked_about_again`
- apb-engine `publish_test::a_node_that_changes_the_remote_url_is_refused`
- apb-engine `publish_test::a_later_commit_is_refused_as_head_moved`
- apb-engine `publish_test::a_url_the_operators_config_rewrites_further_is_refused`
- apb-engine `publish_test::an_operators_push_rewrite_is_refused`
- apb-engine `publish_test::a_rewrite_the_tree_adds_mid_run_never_moves_the_push`
- apb-engine `publish_test::a_shadow_planted_ahead_of_the_push_is_never_used`
- apb-engine `publish_test::a_file_remote_inside_the_tree_is_refused_and_its_hooks_never_run`
- apb-engine `publish_test::a_file_url_with_a_host_part_is_refused_at_the_pin`
- apb-engine `publish_test::a_push_under_a_stale_lease_is_not_sent`
- apb-engine `publish_test::a_push_stopped_before_the_remote_updated_is_unknown_and_never_repeated`
- apb-engine `publish_test::a_push_stopped_after_the_remote_updated_is_confirmed_by_the_reconcile`
- apb-engine `publish_test::an_unspent_consent_pushes_an_identical_plan_once_after_a_crash`
- apb-engine `publish_test::a_push_its_recorder_left_running_is_reconciled_only_after_its_deadline`
- apb-engine `publish_test::a_child_of_a_publish_only_run_is_held_to_it`
- apb-engine `publish_test::a_child_started_after_its_parent_acted_pins_nothing`
- apb-engine `publish_test::a_run_step_cannot_consent_even_without_the_runs_playbook`
- apb-engine `sandbox_run_test::publish_only_refuses_an_attempt_that_would_pass_a_credential`
- apb-server `runs_api_test::a_keyless_dashboard_refuses_a_publish_consent`
- apb-engine `publish_test::a_url_with_credentials_is_refused_and_never_journaled`
- apb-engine `publish_test::a_supervisor_retry_withdraws_the_publish_request_and_its_nonce`
- apb-engine `publish_test::an_enforced_review_triage_never_decides_a_publish_gate`
- apb `publish_cli_test::an_unknown_push_is_never_repeated_and_a_resolved_retry_pushes_once`
- apb `publish_cli_test::a_push_that_reached_the_remote_is_confirmed_without_a_second_push`
- apb-core `git::transport::tests` (unit tests: one transport allowed, the
  operator's helper kept, the exact lease, the hardened base unchanged, a
  fresh private shadow per call, a rewrite rule detected)
