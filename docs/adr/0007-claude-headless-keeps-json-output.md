---
decided: 2026-10-28
decision-makers: apb maintainers
---

# Claude's headless form keeps `--output-format json`; tool exposure is checked on the stream transport only

## Applies to

Claude's built-in launch and resume forms (`agents/claude`), the stream
transport (`agents.claude.transport: acp`, `--output-format stream-json
--verbose`) and the tool exposure check (`agents::exposure`,
`scheduler/node/exposure`, the `tool_exposure_mismatch` event).

- The headless default prints one JSON result object. It carries no tool
  list, so no tool exposure is checked there.
- On the stream transport the `system/init` record lists the tools and MCP
  servers the session sees. apb compares them with what it handed the
  attempt (the shell on a node with connector grants, its ask-server
  connected with `ask_user` on a live node, no other server under a strict
  MCP config) and journals a difference before `attempt_finished`.
- A server still starting (`pending`) is not judged, and neither are its
  tools; any other status but `connected` (`failed`, `needs-auth`, one claude
  adds later) is missing. A server of the
  user's claude.ai account (`claude.ai <name>`) is not unexpected under a
  strict config. Both rules are read from claude's documented statuses and
  naming, not from a captured init record: whether `--strict-mcp-config`
  drops account servers, and how often apb's server is still `pending` when
  the record is written, is unverified until a real capture shows it.
- `apb doctor` says, per installed claude, which of the two applies.

## Why

Switching the default to stream-json would change three things at once:
`outputs.extract` would scan every assistant turn instead of only the reply,
much more stdout would reach the output limits (more `transcript_incomplete`
failures), and the grading views that read stdout as one result object would
break. The exposure check is worth having, but not at that price, so it runs
where stream-json already runs and the gap on the default is stated rather
than hidden.

## Checked by

- apb-engine `tool_exposure_test::the_headless_form_keeps_json_output`
- apb-engine `tool_exposure_test::a_server_beyond_a_strict_config_is_journaled_before_the_finish`
- apb-engine `tool_exposure_test::a_failed_attempt_journals_its_mismatch_too`
- apb-core `agents::exposure::tests::a_pending_server_and_its_tools_are_not_missing`
- apb-engine `scheduler::node::exposure::tests::a_pending_ask_server_and_an_account_connector_are_no_mismatch`
