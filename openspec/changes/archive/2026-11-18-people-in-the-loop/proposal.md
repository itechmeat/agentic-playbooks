# Proposal: people in the loop

## Why

A person is apb's main control at gates, and a gate today gives that
person too little to decide with. On main 316c8180 (1.0.0-beta.7) three
gaps stand out:

1. A reviewer decides blind and fast. The pending gate carries the
   options, the literal prompt and a recommendation
   (`crates/apb-engine/src/progress.rs` `PendingReview`, line 84), never
   what the decision covers (commits, changed lines, connector writes,
   sends, list items). Nothing measures how long a gate was open before an
   approval: `ReviewCounts`
   (`crates/apb-engine/src/run_stats/interventions.rs` line 87) counts
   decisions only. A gate cannot refuse to offer `approve` when the change
   is too large to read.
2. A gate takes one word and a note. The prompt is literal ("Template
   placeholders inside are NOT rendered",
   `crates/apb-core/src/schema.rs` line 2126) and a decision is
   `ReviewCommand { node, decision, note }`
   (`crates/apb-engine/src/review.rs` line 21). A value a person corrects
   (an invoice amount, a release version) reaches the next node only as
   free text in a note.
3. A person reads a playbook as YAML or in the editor only. The run report
   draws an SVG of one run (`crates/apb-engine/src/report_html/graph.rs`
   line 1); there is no text diagram beside the definition that a person can
   paste into a ticket or a document.

| Decision | Code example | Non-code example |
|---|---|---|
| D2 scope at the gate | the gate before publish shows 3 commits, 6 files, +41 -38 lines; `max_scope: { files: 20 }` withholds `approve` on a 90-file change | the gate before sending shows 3 drafts, 0 sends, 0 connector writes; an approval in 2 seconds is counted as decided without review |
| D3 typed fields | `fields: [{ name: version, type: text, required: [approve] }]`: the release version a person types reaches the publish node as `{{nodes.confirm.output.version}}` | an invoice gate shows the extracted total and takes a corrected `amount` (number) before booking |
| D6 diagram | `apb diagram fix-tests` prints the flowchart text for a pull request description | the diagram of a mail routine pasted into a process document |

Examples of the whole change:

1. Code: a release gate shows `3 commits, 6 files, +41 -38 lines`,
   withholds `approve` above its `max_scope` and takes the typed field
   `release_note`, which the next node reads as
   `{{nodes.confirm.output.release_note}}`; `apb diagram issue-triage`
   prints the playbook's flowchart text for a pull request description.
2. Non-code: the invoice gate's scope shows 12 drafted items and no
   sends, and the gate takes a
   typed `amount` the reviewer corrects before booking; the dashboard
   playbook page shows the diagram text of the mail routine to copy into
   a process document.

This change is the first of two cut from one draft. The second,
before-running-alone, adds the list pilot and early stop, reminders for
open waits, and readiness and probation before a routine runs alone; it
builds on the review detail this change adds.

## What changes

1. Scope at a form gate (capability `review-scope`): the request of a
   `human_review` that uses `max_scope`, `fields` or `show` journals what
   the decision covers (a plain gate and a publish consent journal their
   request as before, so an older apb still resumes a run waiting there) since the gate's last decision: commits, changed files
   and lines (counts only, from `apb_core::git`), list items settled,
   connector writes, send intents and result snapshots. A channel decision
   faster than a floor that grows with the scope is counted as decided
   without review in `apb stats` and the run outcome (report only).
   `human_review.max_scope` withholds named options above a limit.
   Validator V243. One new journal event, `review_detail`.
2. Typed fields on a review gate (capability `review-fields`):
   `human_review.fields` (text, number, bool, enum, with `required` and
   a templated `default`) and a templated `show:` block. Values come
   through `review_decide`, `apb review --field`, the HTTP route and the
   dashboard panel; they are checked when posted and again when consumed,
   scrubbed of the run's secret values, journaled, and the gate's output
   becomes one JSON object read as `{{nodes.<gate>.output.<field>}}` and
   by `output_field` edges. A passkey assertion covers the field values.
   An older apb refuses to continue such a run (run guard). Validator V244
   to V247. One new journal event, `review_fields`.
3. A derived diagram (capability `playbook-diagram`): one pure function
   renders a flowchart text (Mermaid syntax) from a definition, written as
   `playbook.mmd` in every version directory, outside every digest,
   checked by `apb doctor`, rebuilt by `apb diagram --rebuild`, printed
   by `apb diagram`, shown as text on the dashboard playbook page, in a
   collapsed block of the HTML report and in MCP `playbook_get`.

## Out of scope

1. Per-branch worktrees, a live course check during an attempt and a
   review-capacity limit: owner decisions.
2. Rendering the diagram in the dashboard: the editor already draws the
   graph; a second renderer is a large dependency and a CSP change for no
   new information.
3. Importing a diagram as a draft: after 1.0.
4. The list pilot and early stop, reminders, readiness and probation, the
   `policy.yaml` keys `notify.remind` and `unattended:`, and validator
   codes V240 to V242, V249 and V250: the change before-running-alone,
   which follows this one.

## Where the code differs from the plan

1. "Read as `{{nodes.<review>.fields.<x>}}`": the template engine already
   reads one field of a JSON object output
   (`crates/apb-engine/src/context.rs` line 763) and `output_field`
   edges read node outputs (`crates/apb-engine/src/parallel.rs` line 87).
   A gate with fields therefore outputs one JSON object (`decision` plus
   the fields) and no new namespace is added.
2. "`pending_review` gains scope" through fields on `review_requested`
   and `review_decided`: those variants are built literally in more than
   twenty test files (`rg -n "ReviewRequested \{" crates`), so the new
   facts go into two new events (`review_detail`, `review_fields`) that
   are safe to skip, with the same reading surfaces.

## Impact

1. Crates: `apb-core` (new `review_form.rs` and `diagram.rs`; schema
   fields `HumanReview.fields`, `show` and `max_scope`; validator module
   `validate/review_form.rs`; two new guarded features; `untrusted` rules
   for field defaults; the diagram write in `versioning` and
   `schema_migrate`), `apb-engine` (new `review/scope.rs` and
   `review/form.rs`; drive loop, review gate, publish gate, review
   channel, progress, interventions, run outcome, HTML report), `apb-mcp`
   (`review_decide.fields`, `playbook_get.diagram`), `apb` CLI package
   (`apb review --field`, `apb diagram`, `apb doctor`), `apb-server`
   (review route fields, passkey challenge, playbook detail, ts
   contract), `web` (review panel, playbook page).
2. Bundled content: none.
3. Journal: two new event types, neither a checkpoint, each safe to skip
   up to the node's `node_finished` or a `run_paused`: `review_detail`
   and `review_fields`. Two new guarded feature names after
   `concurrency`: `review_fields`, `review_max_scope` (`GuardedFeature::ALL`
   goes from 9 to 11 entries).
4. New files beside definitions: `playbook.mmd` in every version
   directory written from now on (derived, never read back, not in any
   digest or bundle).
5. Behaviour changes for release notes:
   1. The request of a gate that uses a form feature shows its scope;
      `apb stats` reports such decisions made faster than their scope
      takes to read.
   2. A gate with `fields:` outputs a JSON object instead of the bare
      decision word; routing on `review_status` is unchanged.
   3. Every new version directory carries `playbook.mmd`; `apb doctor`
      names missing, stale and edited diagrams.
6. Built on main 316c8180, after the open pull requests 284, 285 and 286
   are merged or closed. Validator codes V243 to V247 of the reserved
   V240 to V259 (checked free: `git grep -ohE '"V2[0-9][0-9]"' crates |
   sort -u` prints V200, V210, V211, V212, V220, V221, V222 only); V248
   stays reserved, V240 to V242, V249 and V250 go to before-running-alone,
   and V251 to V259 stay reserved for this change family.
7. Size: 72 changed files (design "Files").
