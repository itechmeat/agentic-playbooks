---
decided: 2026-10-28
decision-makers: apb maintainers
---

# A built-in form without its machine output means the format changed; a custom form read as plain text is allowed

## Applies to

The output readers of the agent modules (`OutputFormat`), their
`promises()`, and the verdict `agents::conformance`, used by the agent
contract suite and by the paid probe behind `apb doctor --probe-agent`.

- The built-in form of an agent with a machine format (claude, codex,
  opencode, zcode) must print that format. Output without it is `Drifted`:
  the CLI changed its output format.
- A custom form (`agents.<id>.invocation` in the config) may print plain
  text. It is read as the reply with no usage, and is `PlainByDesign`.
- An agent without a machine format is always `PlainByDesign`.

## Why

Every reader is lenient on purpose: plain text never fails an attempt, so a
custom form or a stub agent still works. That leniency would also hide a
real change in a CLI's output, with usage, session ids and the verdict
silently gone. Drawing the line by form keeps both: an operator's own form is
allowed to be plain, while the form apb ships is held to what its module
promises.

## Checked by

- apb-engine `agent_contract::<agent>::custom_form` for every agent with a
  launch form (for example `agent_contract::claude::custom_form`): the form,
  and whether it is the built-in one, come from a config with and without
  `agents.<id>.invocation`; the same plain stdout is `PlainByDesign` through
  the custom form, and through the built-in one `Drifted` for an agent with a
  machine format and `PlainByDesign` for one without.
- apb-engine `agent_probe_test::plain_text_from_the_built_in_form_is_a_drift`,
  `agent_probe_test::plain_text_from_a_custom_form_is_allowed` and
  `agent_probe_test::an_alias_config_is_read_under_the_alias`.
