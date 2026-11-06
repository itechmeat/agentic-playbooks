# Project: apb (agentic-playbooks)

A local runner for agentic playbooks: a Rust workspace (edition 2024) with a
Svelte dashboard. `CLAUDE.md` at the repository root is the map of crates,
invariants and gates; read it before any change.

This `openspec/` directory is committed with the code. Each change maps to
one PR; the public design record of a change is also written to
`docs/superpowers/specs/` by its last task. Before the PR of a change is
pushed, the change is archived (`openspec archive <change> --yes`) and the
archive and the updated `openspec/specs/` ship in that PR, so `openspec/specs/`
always describes main. Nothing here names an outside article, author or
project.

Conventions that apply to every change here:

1. English only in code, docs and comments. No em or en dashes, no
   exclamation marks, no CJK.
2. New `EventPayload` fields only with `#[serde(default)]`; a new event type
   only when the change design says so, and then safe to skip up to the next
   checkpoint.
3. State files go through `apb_core::fsutil`; every stored or sent text goes
   through the scrubber.
4. Test first: write the failing test, see it fail, then the code.
