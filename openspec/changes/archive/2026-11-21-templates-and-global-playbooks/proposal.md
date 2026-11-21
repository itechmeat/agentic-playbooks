# Proposal: templates and global playbooks

## Why

Two things a person expects from apb 1.0 are missing on main (265d1410).

1. The examples cannot be installed. `examples/playbooks/` holds 15
   playbooks (code and non-code), 3 of them with eval scenarios. A person
   copies a file by hand; no command and no page installs one. The bundled
   install (`apb_core::bundled::playbooks`) writes managed, trusted defaults
   that apb upgrades on its own, which is the wrong tool for a starting
   point a person adapts.
2. A global playbook can be started only through MCP. `apb run` builds a
   project reference in every place of `crates/apb-cli/src/run.rs`
   (`gate_run`, `remember_input`, `key_lookup`, the unattended refusal, the
   readiness warning), `crates/apb-cli/src/budget.rs` (`resolve`) and
   `crates/apb-cli/src/notify.rs` (`start_check`), and starts through the
   project-only `run`, `run_background` and `start_detached`. The dashboard
   lists, shows and runs project playbooks only (`list_playbooks`,
   `get_playbook`, `run_playbook_handler`, `after_start` in
   `crates/apb-server/src/routes/playbooks.rs`, `start_budget` in
   `routes/estimate.rs`). The engine already has every resolved start
   (`run_resolved`, `run_background_resolved`, `start_detached_resolved`)
   and MCP `playbook_run` already uses them with `scope: global`.

The two belong together: a template installed in the global scope links to
the global playbook page, and its next command is `apb run <id> --scope
global`.

This change was cut out of the draft "one point oh freeze" so it can be
built while the last feature changes are in review. The freeze itself (the
compatibility fixtures, the platform sentence, the release candidate)
stays in one-point-oh-freeze and is written after this change merges,
because this change adds an HTTP surface and a provenance value the freeze
must see.

| Decision | Code example | Non-code example |
|---|---|---|
| G1 `apb run --scope global` | `apb run dependency-security-fix --scope global` in a service repository | `apb run document-checklist --scope global --detach` in a documents project |
| G2 dashboard | a global `review-panel` shows once in the list, in the group "Global playbooks", and its page gives the command that runs it | a global `inbox-triage` opened from the list shows its fit for the chosen project |
| G3 gallery | `apb templates install dependency-security-fix`, then the answer names the missing `github` account | `document-checklist` installed globally from the Templates page |

## What changes

1. Global playbooks from the CLI (capability `global-playbooks`): `apb
   run <id> --scope project|global` for the synchronous, the detached and
   the host foreground start; the start key, the budget check, the
   estimate, the notify check, remembered input, the unattended refusal
   and the readiness warning all read the same reference. `--supervise`
   with `--scope global` is refused (`supervise_global_unsupported`), as
   MCP refuses `supervise: "self"` for a global playbook.
2. Global playbooks in the dashboard (capability `global-playbooks`): the
   list carries the global store once, the detail takes `scope`, and a
   read-only page shows a global playbook with its fit for a chosen
   project and the commands that run it.
3. Template gallery (capability `template-gallery`): the 15 examples
   embedded as templates; `apb templates list | show | install <id>
   [--scope project|global] [--as <id>]`, HTTP `GET /api/templates`, `GET
   /api/templates/{id}`, `POST /api/templates/{id}/install`, a dashboard
   Templates page. An installed template is a draft with provenance
   `created_by: template` and its eval scenarios; installing approves
   nothing; the install answer carries the fit report.
4. A run of a global definition never touches the candidate line of a
   project playbook of the same id: grading it re-decides nothing, the
   project's quality comparison and grade backfill leave it out, a
   supervisor patch of it is refused `patch_global_unsupported`, and it
   is never marked a candidate trial. These paths find a playbook by id
   alone today, also for MCP's existing global start.

## Out of scope

1. `apb run --supervise --scope global`. The supervised start crosses a
   detached spawn with its launch decision bound to the definition in a
   handoff file, and MCP does not offer the pair either. It is refused with
   a code and can be added in 1.x.
2. A template tool over MCP. A host already authors through
   `playbook_capture`; a template tool can be added in 1.x.
3. `examples/playbooks/quality-process/` as a template. It is untracked on
   main and is local research: it is left as it is, not committed, not
   deleted, and never listed or installed as a template (the loader
   selects top-level files and the `evals/` folder of their name only).
4. Starting a global playbook from the dashboard, and editing, versions,
   frozen, delete, layout, memory, scenarios, problems, statistics and
   health of one there. The Run dialog, the estimate and notify routes
   and the playbook page read the project playbook of an id; making each
   of them scope-aware is a change of its own, after 1.0. The global
   page shows the CLI command.
5. An atomic draft publication (the `lifecycle` file is written after the
   version is published, as `playbook_capture` does today).
6. The compatibility freeze, the platform sentence, a version bump and
   release notes: they are one-point-oh-freeze's.
7. Candidates, patches and quality comparison for global playbooks
   themselves (an origin-aware candidate store): a patch of a global run
   is refused instead. Run estimates and run statistics stay matched by
   playbook id and version without the scope (display and prediction
   only), as today. The resolved start functions are used as they
   are.

## Impact

1. Crates: `apb-core` (new `templates.rs`; `bundled/mod.rs` shares its
   embedding helpers), `apb-mcp` (new `tools/templates.rs`; `capture.rs`
   shares its draft writer and gains `CaptureOrigin::Template`), `apb` CLI
   (new `templates.rs`; `--scope` on `apb run`; `budget.rs` and
   `notify.rs` take the reference), `apb-server` (new
   `routes/templates.rs`; `scope` on the playbook list and detail; ts
   contract), `web` (Templates page, the global playbook page),
   `apb-engine` (`run_origin::is_global` and its guards in the candidate,
   patch and grade paths). `apb-decide`
   and `apb-mcp-client` are not touched.
2. Journal: no new event and no new field. Validator: no new code. MCP:
   no new tool and no changed argument.
3. Bundled content: the examples become embedded templates, read from
   `examples/playbooks/` where they are.
4. Behaviour changes for the release notes: design.md "Release notes".
5. Size: 70 changed files (design.md "Files").
