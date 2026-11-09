---
decided: 2026-11-09
decision-makers: apb maintainers
---

# Run-scoped files an agent takes only as arguments lead its argv

## Applies to

An agent whose run-scoped file (a file apb prepares for one spawn in the
node's agent home, `agent-home/<agent>/<node>`) can only be named on its
command line, not through a variable. Today that is dsh: it has no model
flag and reads its model from the composed profile, which a `--patch <file>`
overlay replaces (`crates/apb-core/src/agents/dsh/model.rs`).

- The agent module declares the arguments in
  `HostSetup::scoped_args(model, scoped_dir)`, the argv twin of
  `HostSetup::scoped_env`, and writes the file there.
- The one spawn path (`adapter::command::spawn_agent`, through
  `adapter::spawn_env::with_scoped_args`) puts them in front of the form's
  own arguments, for the start and the resume form of every transport, the
  doctor's paid probe included, and inside the sandbox command too. The
  supervisor spawn, which starts its detached process on its own, applies
  the same function, so a supervisor runs the model its profile names.
- A file is named after its content and never rewritten with other bytes:
  items of one list node share the node's agent home and may run at once
  on different fallback models, and each keeps reading its own.
- They lead the argv because a launcher reads its own flags first: dsh's
  launcher parses `--patch` and `--profile` up to the first flag it does not
  know and hands the rest to the app.
- A custom `agents.<id>.invocation` form gets them as well: it runs the same
  binary, which needs the same file to run the model the profile names.
- The supervisor spawn gets these arguments but not the run-scoped
  environment (`scoped_env`): codex's run-scoped home holds no MCP servers,
  and a supervisor works through apb's MCP tools.

## Why

A model the profile names must be the model that runs. Without the patch a
dsh step would silently run the headless profile's own default, possibly on
another provider's account. Putting the file into the built-in form would
need a new placeholder for every such agent and could not reach a resume or
a custom form; deriving it at the one place every agent process starts keeps
every form on the same rule, as decision record 0004 does for the
environment.

## Checked by

- apb-engine `agent_contract::the_run_scoped_arguments_lead_the_form`
- apb-engine `agent_env_spawn_test::the_supervisor_spawn_gets_the_run_scoped_leading_arguments`
- apb-core `agents::dsh::model::tests::overlapping_launches_on_different_routes_keep_their_own_patch`
- apb-core `agents::dsh::model::tests::the_scoped_patch_is_written_once_and_named_in_front`
- apb-core `agents::dsh::model::tests::strings_that_could_reshape_the_patch_are_refused`
