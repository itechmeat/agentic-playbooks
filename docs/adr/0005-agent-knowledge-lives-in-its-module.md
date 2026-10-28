---
decided: 2026-10-28
decision-makers: apb maintainers
---

# Knowledge about a built-in agent lives only in its module

## Applies to

Everything apb knows about one agent CLI: how it is found and launched, its
resume and fresh-session forms, how its output reads, what it needs from the
host and which models it takes. Each built-in agent has a module under
`crates/apb-core/src/agents/` implementing the `Agent` trait, registered in
`agents::all()`.

- The rest of apb asks the registry (`agents::lookup`, `agents::all`) and a
  module's descriptor, capabilities, forms, output reader, setup and model
  policy.
- Non-test Rust code outside `agents/` never names a built-in agent id or
  alias as a string literal. Code that needs one agent for a reason of its
  own refers to that module's `ID` constant instead, so the dependency stays
  searchable: claude is the historical form of a configured agent without
  a form of its own (`invocation::spec_for`), the program and form behind
  `APB_AGENT_CMD` (`adapter::adapter_for`, `ClaudeAdapter::from_env`) and
  the summarizer of context compaction (`scheduler/node`).
- Comparing agent ids in general (finding a catalog entry by id, matching a
  chain step) is not agent knowledge. Choosing behaviour by one agent's id
  is, and belongs in a capability or the module; the test cannot tell the
  two apart, so review checks it. The web dashboard's defaults are outside
  the test too, and review covers them as well.

## Why

Before the modules, one agent's facts were spread over detection, invocation,
output reading, the adapter, failure classification, the agent home and the
models table, and nine copies of a `match` on the agent name had to change
together for each new agent or each CLI change. A fact kept in one place is
changed in one place, and adding an agent is one module, one registry entry
and one contract fixture.

## Checked by

- apb-core `agents_test::agent_knowledge_stays_in_the_agent_modules`, with an
  allowlist that gives a reason for each remaining literal (string literals
  in Rust `src`, `#[cfg(test)]` items left out;
  `agents_test::the_test_item_scan_skips_braces_in_literals_and_comments`
  keeps that skip honest).
- Code review, for id comparisons and the web dashboard.
