# Design: people in the loop

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is today. Built on main 316c8180
(1.0.0-beta.7, routine-health merged and archived at
`openspec/changes/archive/2026-11-11-routine-health`); every line number
below was checked against that tree. If a line moved, find the site by the
name given next to it, never by the number alone.

This is the first of two changes cut from one draft. It holds the three
decisions that let a person decide informed: the scope at every review
gate (D2), typed fields on a gate (D3) and the derived diagram (D6). The
list pilot and early stop (D1), reminders for open waits (D4) and
readiness and probation (D5) are the change before-running-alone, which
is built on this one after it merges. The decision numbers are kept from
the draft so that both changes name the same decision the same way.

Four sections, built in the order D0, D2, D3, D6 (tasks.md): the scope
(D2) gives the request of every gate that uses a form feature its detail
event (see "Which requests carry a detail"), the form (D3) fills the rest
of that event, and the diagram (D6) is independent and comes last.

Nothing in this document is left to the implementer. Where a choice was
open, it is decided here and the reason is given; tasks.md only orders the
work. Every user-facing string below is final text: copy it.

What before-running-alone takes from this change, so keep these names and
shapes exactly: the `review_detail` event, `GateDrive.workdir`,
`GateRequest.detail`, `GateDetail` (built with `..Default::default()` so a
field added later breaks no construction), the `floor_seq` parameter of
`review::scope::window_start`, the `check_review_target` checks of D2 and
D3, the `PendingReview` fill from the detail in `pending_gate`, and the
two guarded features.

## Size

72 changed files, including the six openspec files (design "Files" lists
them). This is the count the 1.0 plan gives this change. There is no cut
line: a file the compiler names beyond the list (a literal construction of
a changed struct) is fixed where it is and counted in the final report.

## D0. Shared pieces

### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V243 | error | D2 | malformed `human_review.max_scope` |
| V244 | error | D3 | malformed `human_review.fields` |
| V245 | error | D3 | malformed `human_review.show` |
| V246 | error | D3 | a required field without a `default` on a gate the engine may decide |
| V247 | warning | D3 | a field `default` renders untrusted text |

V248 stays reserved. V240 to V242, V249 and V250 belong to
before-running-alone; V251 to V259 stay reserved for this change family.
Checked on 316c8180: `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u`
prints `"V200"`, `"V210"`, `"V211"`, `"V212"`, `"V220"`, `"V221"`,
`"V222"` only.

One new module in `crates/apb-core/src/validate/`, with a module doc
line: `review_form.rs` (V243 to V247). Declared in
`crates/apb-core/src/validate/mod.rs` in alphabetical place, `mod
review_form;` after `mod review;` (line 41). Called from `validate()`
right after the cloud models call (line 461) and before `if
r.is_valid()` (line 462):

```rust
cloud_models::check_cloud_models(playbook, ctx, &mut r); // V220-V222
review_form::check_review_form(playbook, &mut r); // V243-V247
```

The function has the signature `pub(super) fn check_review_form(playbook:
&Playbook, r: &mut ValidationReport)` and reports with `r.error(code,
Some(&node.id), message)` or `r.warn(code, Some(&node.id), message)`, as
`validate/list.rs` does.

### Two new journal events

Added to `EventPayload` in `crates/apb-engine/src/event.rs`, neither a
checkpoint (`is_checkpoint`, line 3605, stays as it is), each written by
the drive only. `ReviewFields` is written between the gate's
`node_started` and its `node_finished`, so an older apb skips it up to
that checkpoint (`event::read_all`). `ReviewDetail` is written only for
a gate that uses a form feature, in the same write as its
`review_requested` (`EventLog::append_pair`, one buffer under the
journal lock), so no reader and no crash ever sees such a request
without it. Every field is `#[serde(default)]` (decision record 0001); a
structured field reads leniently.

### Which requests carry a detail

The project rule is that a new event is safe to skip up to the next
checkpoint, and that a playbook that uses no new feature keeps resuming
on an older apb. A run waiting at a gate has no checkpoint after the
request, so a `review_detail` there would make an older apb refuse to
resume a run that uses nothing of this change. One function over the
gate's definition decides it,
`apb_core::schema::NodeKind::needs_review_detail`: true only for a
`human_review` with `fields`, `show` or `max_scope`.

1. A gate it answers false for (a plain `human_review`, a publish
   consent) writes `review_requested` alone, exactly the journal an apb
   before this change writes. Its request carries no scope: nothing shows
   one for it, and `apb stats` and the run outcome cannot measure its
   decision against a reading floor.
2. A gate it answers true for writes the pair. Every such gate carries
   the run guard (`review_fields` or `review_max_scope`,
   `validate::guarded`), so an older apb refuses to continue the run
   with its "written by a newer apb" message whatever the detail.
3. The writer (`review_gate::await_decision` and
   `journal_missing_detail`) and every reader that checks the form (the
   channel's `check_form` and `formed`, the drive's consume-time `settle`
   and the call before it) ask the same function, so a missing detail of
   a gate that needs one fails closed and is never read as a gate
   without the feature. The display readers (pending review, report,
   stats) show nothing where there is no detail.

Put `ReviewDetail` and `ReviewFields` right after `ReviewWithdrawn` (which
ends at line 869):

```rust
/// What the review request at seq `request` shows beside its options:
/// the scope of what the decision covers, the rendered `show:` entries
/// and field defaults, and the options withheld above `max_scope` with
/// the reason. Written only for a gate that uses a form feature
/// (`apb_core::schema::NodeKind::needs_review_detail`), in the same write as its
/// `review_requested`. Display, the form's defaults and the channel's
/// checks; such a run carries the run guard. Additive variant.
ReviewDetail {
    #[serde(default)]
    node: String,
    #[serde(default)]
    request: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_option"
    )]
    scope: Option<crate::review::scope::ReviewScope>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_default"
    )]
    show: Vec<crate::review::form::ShownEntry>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_default"
    )]
    fields: Vec<crate::review::form::FieldView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    withheld: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    withheld_reason: Option<String>,
},
/// The field values a decision of gate `node` carried, checked against
/// the gate's `fields` and scrubbed of the run's secret values, for the
/// request at seq `request`. Written right before its `review_decided`.
/// Safe to skip up to the gate's `node_finished`; an older apb never
/// continues such a run (run guard `review_fields`). Additive variant.
ReviewFields {
    #[serde(default)]
    node: String,
    #[serde(default)]
    request: u64,
    #[serde(default)]
    fields: BTreeMap<String, serde_json::Value>,
},
```

(before-running-alone adds one more lenient optional field, `pilot`, to
`ReviewDetail`; nothing here may depend on the variant having exactly
these fields.)

`RunState::fold` (`crates/apb-engine/src/state.rs`) matches every
variant: add `| EventPayload::ReviewDetail { .. } |
EventPayload::ReviewFields { .. }` to the arm `EventPayload::ReviewRequested
{ .. } | EventPayload::ReviewWithdrawn { .. } => {}` (line 891). No other
match in the crate is exhaustive over `EventPayload` (checked: only
`state.rs` lists `ListBatchStarted` in a match without `_`).

### Two new guarded features

`crates/apb-core/src/validate/guarded.rs` `GuardedFeature` (line 22)
gets two variants after `Concurrency` (line 50), each with a doc line,
in this order:

```rust
/// A gate takes typed field values and shows rendered outputs
/// (`human_review.fields`, `show`); an older apb drops them and would
/// output the bare decision.
ReviewFields,
/// A gate withholds options above a scope limit (`max_scope`); an older
/// apb drops it and would offer every option.
ReviewMaxScope,
```

`as_str` names them `review_fields`, `review_max_scope`; `ALL` (line 55)
becomes `[GuardedFeature; 11]` with the two last (one more each than the
nine on merged main). `guarded_features` (line 91) inserts `ReviewFields`
when the node is a `human_review` with non-empty `fields` or `show`,
`ReviewMaxScope` when it has `max_scope`. `run_guard_of`
(`crates/apb-engine/src/scheduler/prepare.rs` line 1906) passes every
feature except `deadline` and `concurrency` through, so it needs no
change. Update the two tests that count: `a_deadline_is_a_guarded_feature`
(line 144, its `ALL.len()` assertion at line 150: `9` becomes `11`) and
rename `all_has_nine_entries_in_order` (line 264) to
`all_has_eleven_entries_in_order` with `GuardedFeature::ReviewMaxScope`
last. Add `review_fields_and_max_scope_are_guarded`: two one-node
playbooks each name their one feature, and `parse` round-trips the two
names.

The module doc of `guarded.rs` (lines 1 to 13) gets one sentence before
"A run whose definition": "one that does not know a gate's `fields`,
`show` or `max_scope` would decide the gate as a plain one."

### The review form module (apb-core)

`crates/apb-core/src/review_form.rs` (new, declared `pub mod
review_form;` in `crates/apb-core/src/lib.rs` in alphabetical place)
holds the playbook-side types of D2 and D3 and their pure checks. The
types are listed in D2 ("Schema") and D3 ("Schema").


### Test helpers

Engine integration tests of this change follow the gate helpers pattern
of `crates/apb-engine/tests/suite/review_test.rs` (`run_in_background`
line 101, `latest_run_dir` line 111, `decide` line 122, `wait_result`
line 139): copy those four into the new test file when a test needs a
background run, since they are private there.


## D2. Scope at every review gate

### What a scope is

The scope of a review request is what its decision covers: everything the
run did since the gate's previous decision on the same node (or since the
run started), counted from the journal and, for changed lines, from git.
Counts only: no path, no file content, no subject line is ever stored.

`crates/apb-engine/src/review/scope.rs` (new; declare `pub mod scope;`
next to `pub mod channel;` in `crates/apb-engine/src/review.rs` line 12):

```rust
/// What a review decision covers, counted since the gate's previous
/// decision (see [`window_start`]). Counts only, never content.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewScope {
    /// The journal seq the window starts after (0: the run's start).
    #[serde(default)]
    pub since_seq: u64,
    /// Commits the run's nodes made in the window (`artifacts_committed`).
    #[serde(default)]
    pub commits: u32,
    /// Files those commits changed; absent when git could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub files: Option<u32>,
    /// Lines added and removed by those commits; absent when git could not
    /// tell. A binary file counts as a file with no lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub lines_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub lines_removed: Option<u32>,
    /// List items settled `passed` or `needs_look` (carried ones left out).
    #[serde(default)]
    pub items_settled: u32,
    /// Connector calls that wrote (or may have written), apb's own
    /// notifications left out.
    #[serde(default)]
    pub writes: u32,
    /// Of those, writes whose effect nobody can tell.
    #[serde(default)]
    pub writes_unknown: u32,
    /// Send intents recorded, apb's own notifications left out.
    #[serde(default)]
    pub sends: u32,
    /// Of those, sends a person still has to resolve.
    #[serde(default)]
    pub sends_open: u32,
    /// Node executions that took a result snapshot (`results:`).
    #[serde(default)]
    pub results: u32,
}
```

Functions, all `pub`:

1. `window_start(events: &[Event], node: &str, floor_seq: u64) -> u64`:
   the seq of the latest `ReviewDecided` of `node`, or 0 when there is
   none, then the larger of that and `floor_seq`. A `ReviewWithdrawn`
   never moves the window: a withdrawn request decided nothing, so the
   request that reopens the gate still covers the work the withdrawn one
   did (else a `run_continue_from` to the gate would reset the scope
   under `max_scope`).
   Every gate of this change passes 0; the parameter stays so that the
   list pilot gate of the change that follows (before-running-alone)
   can start its window at its visit's `started_seq` without changing
   this signature.
2. `scope_since(workdir: &Path, run_dir: &Path, events: &[Event], since_seq:
   u64, now_ms: u64) -> ReviewScope`, over the events with `seq >
   since_seq`:
   1. `commits`: every commit the window's `ArtifactsCommitted` events
      recorded (`event.rs` line 1481), each once: the distinct shas git
      lists for the recorded ranges, or, when files and lines are not
      read, the distinct listed shas plus every event's `omitted`.
   2. `files`, `lines_added`, `lines_removed`: when there is no
      `ArtifactsCommitted` in the window, all three are `Some(0)`.
      Otherwise the invariant is: every commit the journal recorded in
      the window is counted once, by its own change against its first
      parent (a root commit against the empty tree, a merge commit
      against its first parent, so what a merge brings in or adds in
      itself counts); files are deduplicated by path, lines summed over
      the distinct commits, a binary file is a file with 0 lines, and a
      rename is the two paths git stores. Two reads through the hardened
      git (`crates/apb-core/src/git.rs`, never a program the tree
      names): first, per recorded event, `apb_core::git::read(workdir,
      &["rev-list", "--parents", "<before>..<after>", "--"])`, the commits
      of the range that event recorded with their parents (a range names
      commit objects, so it reads the same after a later node reset or
      rebased the branch, and a commit a reset took off the branch still
      counts; one range from the first `before` to the last `after`
      would read the branch as it is now and miss it); then one
      `apb_core::git::read_input(workdir, &["diff-tree", "--stdin", "-r",
      "-z", "--numstat", "--no-renames", "--root", "--always",
      "--no-color", "--no-ext-diff", "--no-textconv",
      "--ignore-submodules=dirty"], "<sha> <first parent>\n...")` over
      the distinct commits (a root given alone). The flags are pinned: no
      rename detection whatever the repository's config says, no
      textconv, no external diff, NUL-separated paths, and a header for
      every commit, so every commit asked for must answer exactly once.
      Every commit on its own, so a change a later commit takes back
      still counts (a diff of the two endpoints would hide it). The
      paths are held in memory for the count only. A `before` or
      `after` that is not a full commit id (an event of an older apb
      with an empty one), a `read` that fails, a range that no longer
      holds the number of commits the event recorded (`commits.len() +
      omitted`) or one of the shas it listed, a commit git cannot show
      (pruned), output that does not parse or does not answer every
      commit once, make all three `None` (not read), never 0, and an
      unknown count withholds under `max_scope` (fail-closed).
   3. `items_settled`: `ListItemSettled` with `state` `passed` or
      `needs_look` and `carried == false`.
   4. `writes` and `writes_unknown`: over
      `crate::run_outcome::calls::classified_calls(run_dir, events)`
      (`crates/apb-engine/src/run_outcome/calls.rs` line 44) with `seq >
      since_seq` and `node != apb_core::connector::sites::NOTIFY_SITE`:
      `writes` counts `CallClass::WriteOk`, `WriteIncomplete` and
      `WriteUnknown`, `writes_unknown` the last alone.
   5. `sends` and `sends_open`: the `intent` ids of every
      `SendIntentRecorded` (`event.rs` line 1132) with `seq >
      since_seq` and `node != NOTIFY_SITE`; `sends` is their number,
      `sends_open` how many of them
      `crate::run_outcome::intents::intents_from(events, now_ms)`
      (`intents.rs` line 243) reports with `open == true`.
   6. `results`: `NodeFinished` with `result.is_some()`.
3. `fast_floor_ms(s: &ReviewScope) -> u64`: `5_000 + 1_000 * files + 50 *
   (lines_added + lines_removed) + 2_000 * (writes + sends) + 1_000 *
   items_settled`, an unknown count read as 0, clamped to `5_000 ..=
   300_000`. The least active time a person needs to have read the
   scope; below it a person's decision counts as decided without review.
4. `scope_line(s: &ReviewScope) -> String`: the one line every surface
   prints, parts with a zero count left out except commits, in this order:
   `3 commits, 6 files, +41 -38 lines, 12 list items, 2 connector writes
   (1 unknown), 3 sends (1 open), 1 result`; an unread count is named for
   what it is, `files not read` and `lines not read`; a scope with nothing in it prints `nothing changed
   since the last decision`. Singular forms: `1 commit`, `1 file`, `1
   list item`, `1 connector write`, `1 send`, `1 result`.

Why committed work only: the scope is what the decision lets go on. An
uncommitted change in the tree is not part of any node's recorded result,
and reading the working tree would count files no node of this window
touched. A non-code routine has no commits; its scope is the items,
writes and sends.

### When the scope is taken

Once per review request, by the drive, in
`crates/apb-engine/src/scheduler/review_gate.rs` `await_decision` (line
118), right after the request is appended (line 143) and before an
automatic decision is posted (line 150):

1. `GateDrive` (line 104) gets `pub(crate) workdir: &'a Path`, filled at
   its two constructions (`scheduler.rs` line 2411, the human review
   branch, and line 2520, the publish branch) with the drive's `&workdir`.
2. `GateRequest` (line 74) gets `pub(crate) detail:
   crate::review::form::GateDetail` (D3 defines the type). Its three
   constructions (`human_review_request` line 307,
   `unattended_review_request` line 353, and
   `crates/apb-engine/src/scheduler/publish.rs` line 379) fill it; the
   publish one passes `GateDetail::default()`.
3. For a gate `needs_review_detail` answers true for, the drive appends the
   request and one `EventPayload::ReviewDetail` in one write:
   `log.append_pair(req.event, detail)`, where `detail` builds,
   from the request's seq, `node` the gate, `request` = that seq, `scope =
   Some(scope_since(workdir, run_dir, &events, window_start(&events,
   current, req.detail.floor_seq), now_ms))` where `events` is the
   journal `await_decision` read before the request, `show` and
   `fields` from `req.detail`, and `withheld`, `withheld_reason` from
   `withheld` below (`review_gate::detail_event`). Only then the
   automatic decision is posted, so the channel's checks (D3 "Checked
   when posted") read the detail. Any other gate appends its request
   alone (`log.append(req.event)`), as before this change.
4. A request of such a gate already open without its detail (a write cut
   by a crash right after the request's line):
   `review_gate::journal_missing_detail` journals the detail for the
   open request first, before an unattended run posts its declared
   decision and before `await_decision` consumes or awaits one, from a
   fresh `render_form` (the closure the drive hands it). A gate that
   needs no detail is never given one. Until it exists the checks fail
   closed (D2 "Withheld options are refused, not hidden").
   A crash that cuts the write inside the detail's line leaves a torn
   last line: the strict read of every resumer refuses that journal,
   exactly as it refuses a torn single append, and the pair adds no
   recovery of its own.
5. A request whose work changed while it waited: a deferred retry that
   falls due takes the drive while the gate waits
   (`scheduler::deferred::give_way`), so work can land after the scope
   was measured. Before `await_decision` consumes a decision on a gate
   whose detail carries a scope, `review_gate::withdraw_if_rescoped`
   measures the scope again with `scope_since` over the window the
   detail recorded (`since_seq`). Equal to the detail: the decision is
   consumed and nothing is journaled. Different (a count moved, or one
   that read before no longer reads): the decision is not consumed, the
   request is withdrawn with the existing `review_withdrawn` and the
   reason `scope_changed: work landed while the request waited, so the
   gate asks again on what is there now`, and in the same look the gate
   asks again with a fresh request and detail, so `max_scope` and the
   channel's checks run on what is really there. No new event and no new
   field. The count-based rule passes the entry it set aside
   (`event::review_consumed_count`: one per decision and one per scope
   re-ask, the count the channel's post check reads too); a stamped
   entry names the withdrawn request and answers no other. A queued or
   unattended decision follows the same path: an unattended gate posts
   its declared decision again on the new request, and a withheld one
   fails the run there. A re-ask happens only when the measured scope
   differs from the open request's detail, and each one is one journal
   pair, so the gate never loops without progress. The new request's
   instruction ends with `progress::ASKED_AGAIN_LINE`. `apb stats` and
   the run outcome count the withdrawn request and the one asked after it
   as one gate visit, not a withdrawn one (`run_stats::interventions`),
   and the fast-decision check is measured from the request decided.

### max_scope (schema)

In `crates/apb-core/src/review_form.rs`:

```rust
/// Options a gate does not offer above a scope (`human_review.max_scope`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MaxScope {
    /// Changed files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u32>,
    /// Lines added plus lines removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    /// Connector writes plus send intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<u32>,
    /// The options withheld above a limit, as the author wrote them;
    /// `None` withholds `approve` when the gate offers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withhold: Option<Vec<String>>,
}
/// The option a `max_scope` without `withhold` withholds.
pub const DEFAULT_WITHHOLD: &str = "approve";
impl MaxScope {
    /// The authored list, else `approve` when the gate offers it, else
    /// nothing; only offered options.
    pub fn withhold_for(&self, offered: &[String]) -> Vec<String>;
}
```

The default is resolved against the gate's options, never written into
the definition, so a gate that offers no `approve` is not refused for an
option its author never named (V243 item 5).

`NodeKind::HumanReview` (`crates/apb-core/src/schema.rs` line 2118) gets,
after `prompt` (line 2129):

```rust
/// Options the gate withholds when the decision's scope is larger
/// (people-in-the-loop D2). Absent: every option is offered.
#[serde(default, skip_serializing_if = "Option::is_none")]
max_scope: Option<crate::review_form::MaxScope>,
```

(D3 adds `fields` and `show` in the same place.) The four patterns that
name every field of the variant get `..`: `scheduler.rs` line 2371,
`progress.rs` line 979, `review_gate.rs` line 431 and
`crates/apb-core/tests/suite/schema_test.rs` line 160.

`pub fn withheld(max: &MaxScope, s: &ReviewScope, options: &[String]) ->
(Vec<String>, Option<String>)` in `scope.rs`: the limits that a count
passes (`files` against `s.files`, `lines` against `lines_added +
lines_removed`, `writes` against `writes + sends`); a limit whose count
is unknown (`None`) counts as passed (fail-closed). When any limit is
passed, the result is `max.withhold_for(options)`, and the reason `the scope (6 files, 900 lines) is above
max_scope (files 5); split the work or choose another option`: the
counts that passed in parentheses after "the scope", the limits they
passed in parentheses after "max_scope", an unknown count named for what
it is (`files not read`, `lines not read`). Else `(vec![], None)`.

### V243

`validate/review_form.rs`, error `V243` on the gate, one issue per
problem:

1. no limit set: `max_scope sets no limit; give files, lines or writes`;
2. a limit of 0: `max_scope.<key> is 0; a limit is at least 1`;
3. an authored withheld option not among
   `effective_review_options(options)`: `max_scope.withhold entry <n>
   names an option the gate does not offer` (1-based; the option is
   definition text and is never repeated);
4. every offered option withheld: `max_scope withholds every option of the
   gate; keep at least one to decide with`;
5. no `withhold` on a gate that offers no `approve`: `max_scope withholds
   nothing: the gate does not offer \`approve\`; name the options to
   withhold in max_scope.withhold`.

### Withheld options are refused, not hidden

1. When posted: `check_review_target` (`review.rs` line 114) runs
   `check_form` for every `human_review` node, a `person_only` or
   `approves` gate included (after `check_bound_decision`, never skipped
   by its early return). For a gate with `max_scope` it reads the latest
   `ReviewDetail` of the node whose `request` is the open request
   (`crate::review::open_request`) and refuses a decision in its
   `withheld` with `EngineError::Invalid(format!("option \`{d}\` of
   gate \`{node}\` is withheld: {reason}"))`. Without that detail it
   fails closed: an option `max_scope.withhold_for` names is refused with
   `EngineError::Conflict` ("the scope of gate \`<node>\` is not recorded
   yet, so option \`<d>\` may be withheld; decide again"), and a journal
   that does not read is a `Conflict` to retry for a gate with
   `max_scope` or `fields`, never an accepted decision. This runs for the engine's
   own decisions too (`post_engine_decision`, line 775, goes through the
   same `append_decision`), so an enforced recommendation of a withheld
   option is refused and the gate waits for a person, and an unattended
   run's declared decision of a withheld option is refused and the run
   fails through the existing path (`scheduler.rs` lines 2394 to 2401:
   `post_unattended` returns the error, `unattended_stop` ends the run).
2. When consumed: the drive refuses it again (D3 "Checked when consumed");
   a request without its detail counts every option
   `max_scope.withhold_for` names as withheld.
3. Every surface shows the withheld options as not offered, with the
   reason (D2 "Surfaces"). No surface offers a withheld option as a
   command: the `decide_review` next action (and so the CLI hint, the
   MCP action and the dashboard, which all render it) lists only the
   options not withheld in its `decision` choice and names the withheld
   ones with the reason in its `reason`; the dashboard's terminal
   commands for a person-only gate leave them out too.

### Decided without review

1. `ReviewCounts` (`crates/apb-engine/src/run_stats/interventions.rs`
   line 87) gets `pub fast: u64` with the doc "Channel decisions taken
   faster than their scope takes to read (`review::scope::fast_floor_ms`
   over active run time); report only." The fold (`interventions`) keeps
   per open visit the request's `ts` and, when the `ReviewDetail` of that
   request follows, its `fast_floor_ms`. At a `ReviewDecided` counted
   in `channel` (line 361), it computes `crate::pause::active_run_ms(events,
   requested_ts, decided_ts, now_ms)` (`crates/apb-engine/src/pause.rs`
   line 231) and adds 1 to `fast` when that is below the floor. A visit
   without a `ReviewDetail` (an older journal) is never fast. Who decided
   is the consumed decision's own `decided_by` and `via`: an applied
   `review_triage` of the visit counts it automatic only when the
   decision records neither (an older writer's enforced recommendation),
   so a person who decides after a recommendation `max_scope` withheld is
   counted in `channel` and can be fast
   (`interventions::tests::an_applied_recommendation_leaves_a_persons_decision_with_the_person`).
   Engine decisions are never fast, and neither is a scenario's scripted
   decision (`via: scenario`): a script reads nothing. The review channel
   stamps the poster's channel on the entry of every gate, an ordinary one
   included (`check_review_target`), so the drive journals `via` for it
   and a scripted decision is never read as a person's. The fold keeps the
   seqs of the fast decisions (`ReviewCounts.fast_seqs`, not
   serialized), the one rule the HTML report marks a decision by.
2. `apb stats` gets a `fast` split of the review category
   (`version_stats` in `interventions.rs`), printed in the review line
   as `N decided without review` (`interventions::lines`, which
   `run_stats/render.rs` prints); the run's own lines
   (`interventions::run_lines`) say `, N decided without review`.
3. The run outcome: `OutcomeSummary`
   (`crates/apb-engine/src/run_outcome/summary.rs` line 217) gets
   `#[serde(skip_serializing_if = "is_zero")] pub fast_decisions: u32`
   (TypeScript optional), filled from the same count, and the outcome's
   text lines (the function that prints the `needs a look` line in
   `summary.rs`) add `N gate decision(s) taken faster than their scope
   takes to read` when above 0. It is not a `NeedsLookReason`: a fast
   decision never changes a run's `needs_look` outcome or a `finished`
   notification filter.

### Surfaces

1. `PendingReview` (`progress.rs` line 84) gets, each with
   `#[serde(skip_serializing_if = ...)]` and TypeScript optional:
   `scope: Option<ReviewScope>`, `scope_line: Option<String>`,
   `withheld: Vec<String>`, `withheld_reason: Option<String>`,
   `requested_at: Option<u64>` (the request's `ts`, for "open since"),
   and D3's `show` and `fields`. `pending_review` (line
   235) and `pending_publish` (line 131) set them empty; `pending_gate`
   (line 194) and `pending_publish` then fill them from the latest
   `ReviewDetail` of the node whose `request` equals the open request's
   seq. MCP `run_status` and `run_wait` carry `pending_review` as they
   do today; the CLI, the dashboard and the report read it.
2. CLI: one function, `gate_detail_lines` in `crates/apb-cli/src/run.rs`,
   prints `  scope: <scope_line>` and `  withheld: <options> (<reason>)`
   when any (and D3's lines) under every pending gate the CLI shows: a
   run waiting at a gate (`apb wait`, `apb run`), a paused run's open
   gates, and `apb runs <id>` (`waiting on human review (node ...)`).
   Every value goes through `sanitize_for_terminal` with a cap.
3. Dashboard: `web/src/lib/ReviewPanel.svelte` shows the scope line above
   the options with the CLI's label (`scope: <scope_line>`) and renders a
   withheld option as a disabled button with the
   reason as its `title` and as a line under the buttons (on a dashboard
   without keys a person-only gate shows no terminal command for it,
   only that line). `web/src/lib/PublishPanel.svelte` shows the same
   `scope:` line above an open publish plan. Plain CSS over
   the existing CSS variables.
4. HTML report: the review section of `crates/apb-engine/src/report_html/`
   (the function that prints decided gates; find it with `rg -n
   "review_decided\|ReviewDecided" crates/apb-engine/src/report_html`)
   adds the scope line of each decision's request and `decided without
   review` next to a fast one, by `ReviewCounts.fast_seqs`.
5. `ReviewScope` is registered in `crates/apb-server/src/ts_contract.rs`
   next to `PendingReview` (line 116).

### Docs

`docs/HOWTO-authoring.md` `## Human review and conditional edges` (line
2259) gets `### What a decision covers (scope, max_scope)` after the
person-only section (before `## Publishing`, line 2515): the scope line,
the window rule, counts only, committed work only, `max_scope` with an
example, V243, and the fast floor as a report-only signal.
`docs/STATS.md` names `reviews.fast`.

### Wrong implementations

1. Counting lines from the working tree (`git diff` without a commit
   range, or `git status`). Caught by
   `review_scope_test::an_uncommitted_change_is_not_in_the_scope`: a
   node commits one file of 3 lines and leaves another file modified
   uncommitted; the scope is `1 commit, 1 file, +3 -0 lines`.
2. Storing paths or diff text in the event. Caught by
   `review_scope_test::the_detail_holds_counts_only`: the committed file
   is named `secret-name.txt`; no line of `events.jsonl` contains
   `secret-name`.
3. Reading 0 for a range git cannot read. Caught by
   `scope::tests::an_unreadable_range_is_not_read` (a `before` that is
   not a commit gives `files: None`),
   `scope::tests::a_recorded_commit_git_cannot_show_is_not_read` and
   `scope::tests::an_unknown_count_passes_a_limit`. Reading one range
   from the first `before` to the last `after` is caught by
   `scope::tests::a_reset_between_nodes_still_counts_every_recorded_commit`
   (a 3-file commit, a reset to the base, a 1-file commit: 2 commits, 4
   files) and `scope::tests::a_commit_recorded_twice_counts_once`.
   The counting rule as a whole is caught by the table test
   `scope::tests::every_recorded_commit_counts_by_its_own_change` over
   real repositories: linear commits, a revert pair, a reset between
   nodes, a merge with changes on the side branch, a merge with changes
   only in the merge commit, an octopus merge, a root commit merged in,
   a binary file, a rename under `diff.renames: copies`, and a commit
   git can no longer show (pruned). A `git log --numstat` read gives a
   merge commit no files and no lines, so a change put into a merge
   commit passed `max_scope`.
4. Counting since the run's start on every visit. Caught by
   `review_scope_test::a_second_visit_counts_since_the_first_decision`:
   a loop back to the gate after one more commit shows `1 commit`.
5. Counting apb's own notifications as writes or sends. Caught by
   `scope::tests::notify_sends_are_left_out`.
6. Hiding a withheld option only in the dashboard. Caught by
   `review_scope_test::a_withheld_option_is_refused_through_the_channel`
   (`post_decision_via` with `approve` is `Invalid`, `reject` passes)
   and `review_scope_test::an_unattended_withheld_decision_fails_the_run`.
7. Taking wall time for the fast flag. Caught by
   `interventions::tests::a_pause_inside_a_visit_does_not_count_as_reading`:
   a visit open 10 min with 9 min 58 s paused and a floor of 5 s is fast.
8. A request journaled apart from its detail, or a check that trusts a
   missing detail. Caught by `event` `a_request_and_its_detail_are_one_write`
   and `review_scope_test::an_open_request_without_its_detail_fails_closed_until_the_resume_writes_it`.
9. Skipping the form's checks on a stamped gate. Caught by
   `review_scope_test::a_person_only_gate_refuses_a_withheld_option_and_a_bad_value_when_posted`.


## D3. Typed fields on a review gate

### Schema

In `crates/apb-core/src/review_form.rs`:

```rust
/// The most fields one gate takes.
pub const MAX_FIELDS: usize = 20;
/// The most `show:` entries one gate has.
pub const MAX_SHOW: usize = 10;
/// The longest field name.
pub const FIELD_NAME_MAX: usize = 32;
/// The longest `show:` label, in characters.
pub const SHOW_LABEL_MAX: usize = 60;
/// The most bytes of one rendered `show:` text or default; more is cut.
pub const RENDERED_MAX_BYTES: usize = 2000;
/// A text field's `max_len` when none is given, and its upper bound.
pub const TEXT_DEFAULT_MAX_LEN: u32 = 2000;
pub const TEXT_MAX_LEN: u32 = 10_000;
/// The key of the decision in a gate's JSON output; no field may use it.
pub const DECISION_KEY: &str = "decision";

/// One value a person fills or corrects at a gate.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewField {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    /// `true`: every decision needs a value; a list: the decisions that do.
    #[serde(default, skip_serializing_if = "Required::is_off")]
    pub required: Required,
    /// A template over earlier outputs, rendered once per request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The allowed values of an `enum` field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The longest `text` value, in characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind { Text, Number, Bool, Enum }

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Required { Flag(bool), Options(Vec<String>) }
impl Default for Required { fn default() -> Self { Required::Flag(false) } }
impl Required {
    pub fn is_off(&self) -> bool { matches!(self, Required::Flag(false)) }
    /// Whether a decision of `option` needs a value.
    pub fn applies(&self, option: &str) -> bool {
        match self {
            Required::Flag(b) => *b,
            Required::Options(o) => o.iter().any(|x| x == option),
        }
    }
}

/// One rendered earlier output a gate shows above its options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShowEntry {
    pub label: String,
    /// A template over earlier outputs, rendered once per request.
    pub text: String,
}
```

`NodeKind::HumanReview` gets, after `prompt` and before D2's
`max_scope`:

```rust
/// Values the reviewer fills or corrects (people-in-the-loop D3). With
/// any, the gate's output is one JSON object: `decision` and every field.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
fields: Vec<crate::review_form::ReviewField>,
/// Earlier outputs shown above the options, rendered once per request.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
show: Vec<crate::review_form::ShowEntry>,
```

The doc of `prompt` (lines 2124 to 2127) stays true: the prompt is still
literal; `show:` is where templates render.

Example (the invoice gate of the proposal):

```yaml
- id: confirm
  type: human_review
  options: [approve, reject]
  prompt: Check the extracted invoice before it is booked.
  show:
    - { label: Supplier, text: "{{nodes.extract.output.supplier}}" }
  fields:
    - { name: amount, type: number, required: [approve], default: "{{nodes.extract.output.total}}" }
    - { name: currency, type: enum, options: [EUR, USD], default: EUR }
    - { name: remark, type: text, max_len: 200 }
```

### V244, V245, V246, V247

`validate/review_form.rs`, on every `human_review` node:

1. V244 (error), one issue per problem: more than `MAX_FIELDS` fields; a
   name not matching `[a-z][a-z0-9_]*` or longer than `FIELD_NAME_MAX`;
   a name used twice; the name `decision`; an `enum` without
   `options`, with a duplicate option, or `options` on a non-enum
   field; `max_len` on a non-text field, or `max_len` 0 or above
   `TEXT_MAX_LEN`; `required` as a list naming an option the gate does
   not offer; a literal `default` (no `{{`) that does not coerce to the
   field's type (`coerce` below). Messages: `<field>: <what>`,
   for example `field \`amount\`: the literal default is not a number`,
   `field 2: name must match [a-z][a-z0-9_]*`, `field \`currency\`:
   option 2 repeats an earlier option`, `field \`amount\`: required entry
   1 names an option the gate does not offer`.
2. V245 (error): more than `MAX_SHOW` entries; a label empty or longer
   than `SHOW_LABEL_MAX` characters; an empty `text`. Message:
   `show entry <n>: <what>` (1-based).
3. V246 (error): a gate the engine may decide (it has `auto_decide`, or
   `unattended: { decide }`) with a field that is required for an option
   the engine may choose and has no `default` (or a blank literal one,
   which the channel reads as no value): `field \`<name>\` is
   required and has no default, but the gate may be decided without a
   person (auto_decide or unattended); give it a default or make it
   optional`. For `unattended: { decide: x }` the option is `x`; for
   `auto_decide` every option.
4. V247 (warning): a field whose `default` reads untrusted text
   (`crate::untrusted::text_reads_untrusted`, below): `field \`<name>\`
   defaults to untrusted text from <sources>; a person who keeps the
   default passes it on as their own value`.

No message of V243 to V247 repeats a literal default, an option or a
field name outside the grammar: a definition may hold a token in any of
them, and validation output reaches the terminal and every validation
API. The issue names the node; a field is `field \`<name>\`` only when
its name is in the V244 grammar and not shaped like a credential
(`apb_core::review_form::nameable`), else `field <n>` by its 1-based
position (`review_form::field_label`); an option, a `required` entry or
a `withhold` entry is named by its 1-based position; a literal default
by the kind it fails (`is not a number`, `is not true or false`, `is
not one of the field's options`, `does not fit the field's length`).
Caught by `validate_review_test::review_form_messages_repeat_no_definition_text`.

Templates: `crate::schema::template_texts` (`schema.rs` line 2616) adds,
for a `human_review`, each field `default` that contains `{{` as
`(gate id, text)`, so V13 resolves them and the untrusted analysis reads
them. A new `pub fn review_show_texts(playbook: &Playbook) -> Vec<(&str,
&str)>` next to it returns the `show` texts; `check_templates`
(`crates/apb-core/src/validate/templates.rs` line 73) chains it into its
loop, so V13 resolves them too. `show` texts are not in
`template_texts`: they are shown to a person, not passed on.

`declared_fields` (`crates/apb-core/src/validate/node_io.rs` line 168)
inserts, for a `human_review` with fields, `decision` and every field
name, so V46 checks `{{nodes.<gate>.output.<field>}}` and
`output_field` edges against them.

### Untrusted text (apb-core)

`crates/apb-core/src/untrusted.rs`:

1. `carries_text` (line 82) also returns true for `NodeKind::HumanReview
   { fields, .. } if !fields.is_empty()`: rule 3 (line 153) already reads
   the gate's `template_texts`, which now hold its field defaults, so a
   default over tainted output taints the gate's output for its readers.
2. `Analysis` (line 71) gets `pub outputs: BTreeMap<String,
   BTreeSet<Source>>` (the `output` map the loop builds, line 131),
   set in the returned value (line 298).
3. `pub fn text_reads_untrusted(playbook: &Playbook, analysis: &Analysis,
   text: &str) -> BTreeSet<Source>`: the sources of the references of
   `text` by rule 3's own match (params marked untrusted, `nodes.<x>.*`
   reads of a tainted output, `run.previous_success.nodes.*`,
   `run.context` and `run.retro`). Factor rule 3's match (lines 159 to
   207) into one private function both use, so the rule exists once.

### The values a decision carries

1. `ReviewEntry` (`crates/apb-engine/src/review.rs` line 29) gets
   `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")] pub
   fields: BTreeMap<String, serde_json::Value>`, set in `append_decision`
   (the construction at line 898) from a new parameter, and
   `fields: BTreeMap::new()` in the two test constructions (lines 1433
   and 1460). `ReviewCommand` (line 21) does not change: it is built
   literally in more than twenty test files.
2. One new entry point every surface that takes fields calls:

```rust
/// A decision with field values, through `channel`, optionally with a
/// verified passkey assertion: the one entry the CLI, MCP and the HTTP
/// route use when a gate takes fields. The other `post_decision_*`
/// functions call it with no fields.
pub struct DecisionPost {
    pub cmd: ReviewCommand,
    pub nonce: Option<String>,
    pub request: Option<u64>,
    pub channel: DecisionChannel,
    pub fields: BTreeMap<String, serde_json::Value>,
    /// The credential id prefix and the assertion, from the dashboard
    /// route only.
    pub passkey: Option<(String, passkey_proof::PasskeyProof)>,
}
pub fn post(run_dir: &Path, p: DecisionPost) -> Result<u64, EngineError>;
```

   `post` does exactly what `post_decision_via` (line 657) and
   `post_decision_passkey` (line 693) do today, by `p.passkey`, with the
   fields handed to `append_decision`; those two become one-line wrappers
   over `post` with `fields: BTreeMap::new()`.
3. Surfaces:
   1. MCP: `ReviewDecideArgs` (`crates/apb-mcp/src/server/args.rs` line
      596) gets `#[serde(default)] pub fields: BTreeMap<String,
      serde_json::Value>` with the doc "A gate with `fields` only: the
      values the person gave, by field name; leave a field out to keep its
      default (`pending_review.fields[].default`)." `review_decide_via`
      (`crates/apb-mcp/src/tools/run.rs` line 1373) takes a new last
      parameter `fields: &BTreeMap<String, Value>` and calls
      `apb_engine::review::post`; its two callers
      (`tools/run.rs` line 1357 and `crates/apb-cli/src/run.rs` line
      3153) pass it.
   2. CLI: `Command::Review` (`crates/apb-cli/src/main.rs`, the arguments
      of the arm at line 1308) gets `#[arg(long = "field", value_name =
      "NAME=VALUE")] field: Vec<String>` with the help "A value for one of
      the gate's fields; repeat for more (the value is read as the field's
      type)". `review_cmd` (`run.rs` line 3117) splits each at the first
      `=` (no `=`: exit 2 with `--field needs NAME=VALUE`) into a map of
      strings and passes it on. It keeps the run id check it had: a run id
      that is not one path segment of an existing run is refused with exit
      2 (`review failed: run \`<id>\` not found`) before anything is posted.
   3. HTTP: `ReviewBody` (`crates/apb-server/src/routes/runs.rs` line
      677) gets `#[serde(default)] fields: BTreeMap<String,
      serde_json::Value>`; `post_review_handler` (line 728) calls
      `apb_engine::review::post` instead of the two calls at lines 823 and
      830.
   4. Dashboard: `postReview` (`web/src/lib/api/core.ts` line 498) takes
      an optional `fields` and sends it; `web/src/lib/runposts.ts` passes
      the panel's values through (line 107 builds the body).

### Checked when posted

First, before the reviews lock and before anything of the post is
written, `append_decision` runs `refuse_secret_fields` (`review.rs`):
each posted value goes through
`RunRedactor::for_run_dir(run_dir).secret_json`, the scrub the drive
applies when it consumes. A value the scrub would change (it holds a
secret value of the run's tree or the workspace, or a token-shaped
string) is refused `Invalid("gate \`<node>\`: field \`<name>\` holds a
value this run keeps secret; it is not taken")`, through
`review::form::refusal_text`; while the run's secrets do not resolve,
a non-empty text value is refused `Invalid("field \`<name>\`: this
run's secret values cannot be read right now, so a text value is not
taken (it may hold a value this run keeps secret); decide again
later")`, the way the run redactor withholds text. The field is named
only when the gate declares it and `review_form::nameable` holds, else
`a posted value`; the value is never repeated. So the channel line,
which is the post as typed, never holds such a value, on any channel
(`review::post` and the engine's own posts share `append_decision`).
The note is outside this check. The CLI's malformed `--field` refusal
does not repeat the argument either.

`check_review_target` (`review.rs` line 114), for a `human_review` with
fields, after the pending checks and under the reviews lock: read the
latest `ReviewDetail` of the node whose `request` is the open request
(`open_request`); `apb_core::review_form::check_values(&fields_spec,
&decision, &given, &defaults)` where `defaults` maps each
`FieldView.name` to its `default`. An error is
`EngineError::Invalid(format!("gate \`{node}\`: {e}"))`. Fields sent to a
gate without fields: `Invalid("gate \`{node}\` takes no fields")`. A
detail that does not read (none journaled yet for the open request) checks
against the definition with no defaults. These checks run for every gate,
stamped ones included (`check_form`, D2).

`check_values(spec: &[ReviewField], decision: &str, given: &BTreeMap<String,
Value>, defaults: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>,
String>`, pure, in `review_form.rs`:

1. A key of `given` that is not a field: `a posted value names no field
   of this gate` (the key is a posted value and is never repeated).
2. For each field in order: the given value through `coerce`, else the
   default when it coerces (a default that does not fit the field is no
   value, so it never fails a decision the field is not required for),
   else, when `required.applies(decision)`, the error `field
   \`<name>\` is required for this decision` (the decision word is posted
   text and is never repeated), else `Value::Null`. A
   blank text (empty or only whitespace) is no value for a field the
   decision requires: given, it is refused with that error (a default
   does not stand in for a value a person cleared), and a blank default
   does not fill it. For a decision the field is not required for, a
   given blank stays as typed on a `text` field (a person may clear a
   default) and reads as no value on any other type. This is the one
   rule: the channel (MCP, CLI, HTTP) and the drive call it, and the
   dashboard posts what was typed (a field the person cleared is sent
   blank) and shows the server's refusal; it never decides on its own
   whether a required value is missing. A number string is read trimmed,
   as the dashboard reads it.
3. Returns every field name with its value (nulls included).

`coerce(field: &ReviewField, v: &Value) -> Result<Value, String>`:
`text`: a string without NUL of at most `max_len.unwrap_or(2000)`
characters, else `field \`<n>\`: longer than <max> characters`; `number`: a
finite JSON number, or a string that parses as `f64` and is finite (the
result is the JSON number, an integral value within 2^53 kept an integer:
`42`, never `42.0`); `bool`: `true`, `false`, or the strings
`true` and `false`; `enum`: a string among `options`, else `field
\`<n>\`: is not one of the field's options` (the options are definition
text and are never listed). `null` given
for a field means "no value" (default, then required). An error names
the field and the kind of value it takes, never the value, an option, a
posted key or the decision word (`field \`<n>\`: is not a number`, `field
\`<n>\`: is not true or false`): a refusal may be journaled or returned
over HTTP, and any of them may be a secret. The field is named only when
its name is in the V244 grammar and not shaped like a credential, else
`a field`. On top of that, every refusal the channel's form check
(`review::check_form`: the withheld option, the unrecorded scope, a gate
that takes no fields, the values) and the drive's consume-time check
return or journal passes the run's redactor and is cut at 300 bytes
(`review::form::refusal_text`): gate ids and option words are
definition text a hand-written definition may fill with anything.

### Checked when consumed, scrubbed, journaled

In the human review branch of `scheduler.rs`, `GateStep::Decided(entry)`
(line 2447), after the binding check (lines 2450 to 2458) and before
`NodeStarted` (line 2460):

1. When the gate has `fields` or `max_scope`:
   `crate::review::form::settle(root, run_dir, &playbook, &current, &entry,
   &read_all(run_dir)?)` returns `Ok(Option<BTreeMap<String, Value>>)`
   (the checked, scrubbed values; `None` for a gate without fields) or
   `Err(reason)`. It reruns the withheld check of D2 and `check_values`
   against the request's `ReviewDetail` (without one, every option
   `max_scope.withhold_for` names counts as withheld), then scrubs the values with
   `crate::run_redact::RunRedactor::for_run(root, run_dir).secret_json`
   (`crates/apb-engine/src/run_redact.rs`, the in-place JSON scrub next
   to `secret_text`). `Err` (a hand-written channel line, a scrub
   refused) ends the run: `stop_with_reason(log, &current, format!("{}:
   {reason}", crate::review::form::FIELDS_REFUSED), &mut frontier)` with
   `pub const FIELDS_REFUSED: &str = "review_fields_refused"`. The
   reason is scrubbed with the same redactor's `secret_text`
   (`[secrets_unresolved]` when the secrets do not resolve) and cut at
   300 bytes before it is returned: a hand-written line is free text.
2. After `NodeStarted` and before `review_decided` (line 2483), when the
   values are `Some`: append `EventPayload::ReviewFields { node, request:
   entry.request.unwrap_or_else(|| <the open request seq>), fields }`.
3. The node's output (line 2484) becomes
   `apb_core::review_form::gate_output(&decision, &values)` when the gate
   has fields: compact JSON of one object, `decision` and every field
   (`serde_json::Map`, keys sorted). Without fields it stays the decision
   word.
4. `dead_end_reason` (`scheduler.rs` line 215) words a review's end
   `decision \`{output}\``; for a gate with fields it reads the decision
   from `state.reviews` instead, so the message names the word, not the
   JSON, and matches the word against the `review_status` edges it names
   as exhausted.

Downstream: `{{nodes.confirm.output.amount}}` (`context.rs` line 763)
and `{ type: output_field, node: confirm, field: amount, gt: 1000 }`
(`parallel.rs` line 87) read the values; `{{nodes.confirm.review_decision}}`
and `review_status` edges read the decision as before.

### Rendering show and defaults

In `review_gate.rs`, a new `fn render_form(ctx: &FormCtx<'_>, gate:
&Node) -> GateDetail` where `FormCtx { playbook, run_dir, run_id, state,
cfg, root }` carries what `super::node::render_node_prompt`
(`crates/apb-engine/src/scheduler/node.rs` line 25) takes:

1. Each `show` entry: `render_node_prompt(..., &entry.text,
   &ContextBudget::UNLIMITED)`, rendered whole (a context budget cuts each
   output it inserts, and a secret cut in part no longer matches the
   scrubber), scrubbed first with
   `RunRedactor::for_run(root, run_dir).secret_text` (the run's secret
   values and token-shaped strings; `[secrets_unresolved]` when they do
   not resolve), then cut to `RENDERED_MAX_BYTES` at a character boundary
   (`clipped: true`), so a secret across the cut is never kept in part,
   and
   `untrusted` set when `apb_core::untrusted::text_reads_untrusted` over
   `untrusted::analyze(playbook)` is not empty.
2. Each field: its `default` rendered the same way, a literal one too (it
   renders to itself), so a literal default is scrubbed and cut like a
   template, then `coerce`d from a string; a default that does not
   coerce gives `default: None` and `default_problem: Some(<the coerce
   error>)`, itself scrubbed and cut.
   Every other authored text of the detail goes through the same
   `scrub_then_clip`: the `show` labels and the enum `options`. Field
   names need none (V244 holds them to `[a-z][a-z0-9_]*`). There is no
   second path into `review_detail`.
3. `human_review_request` and `unattended_review_request` (lines 277 and
   335) take `form: &FormCtx<'_>` as a new last parameter (the
   `root`/`run_dir` parameters stay), build `detail = render_form(form,
   gate)` with `max_scope` and `floor_seq: 0` (written as
   `GateDetail { show, fields, max_scope, options, ..Default::default() }`,
   so a field added later needs no change here), and the two callers in
   `scheduler.rs` (lines 2423 and 2433) build the `FormCtx` from the
   drive's `root`, `run_dir`, `run_id`, `state` and `cfg`.

The engine-side view types, in `crates/apb-engine/src/review/form.rs`
(new, `pub mod form;` in `review.rs`), all `Serialize, Deserialize,
ts`:

```rust
pub struct ShownEntry { pub label: String, pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub untrusted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub clipped: bool }
pub struct FieldView { pub name: String, pub kind: apb_core::review_form::FieldKind,
    /// The options a value is required for (every option for `required: true`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub required: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub default_problem: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_len: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub untrusted: bool }
/// What a request shows beside its options, before the scope is taken.
#[derive(Default)]
pub struct GateDetail { pub show: Vec<ShownEntry>, pub fields: Vec<FieldView>,
    pub max_scope: Option<apb_core::review_form::MaxScope>,
    /// The options the request offers, for the withheld check.
    pub options: Vec<String>,
    /// The seq the scope window may not start before (0 for every gate of
    /// this change; see D2 `window_start`).
    pub floor_seq: u64 }
```

(`GateDetail` derives `Debug, Clone, Default` only; it is never
serialized. `FieldKind` derives `ts` in core under the `ts` feature like
other mirrored core types.)

### Passkeys cover the values

The passkey challenge covers the decision word today
(`passkey_proof::decision_challenge`,
`crates/apb-engine/src/review/passkey_proof.rs` line 50). With fields it
covers `apb_core::review_form::decision_material(decision, fields)`:
`decision` itself when `fields` is empty (so every existing challenge is
unchanged), else `format!("{decision}\0fields:{}", digest)` where
`digest` is the hex SHA-256 of the compact JSON of `fields` (a
`BTreeMap`, so key order is fixed). Sites:

1. `passkey_stamp_ok` (`review.rs` line 407) builds the `Stamp` with
   `decision: &decision_material(&e.cmd.decision, &e.fields)` (an owned
   `String` bound before the `Stamp`).
2. The dashboard's assertion start: the decision purpose in
   `crates/apb-server/src/routes/passkeys.rs` (the body that carries
   `decision`, read at line 155, and `challenge_for`, line 170) takes an
   optional `fields` map and passes `decision_material(&d.decision,
   &d.fields)` where it passes `&d.decision` today.
3. The dashboard's decide flow (`web/src/lib/runposts.ts`, the decision
   post at lines 102 to 124 and the passkey purpose it hands
   `assertChallenge` at line 93) sends the same `fields` in the review
   body and in the decision purpose, so the challenge it is issued and the
   one the drive recomputes agree.

### Surfaces

1. `PendingReview.show: Vec<ShownEntry>` and `fields: Vec<FieldView>`
   (optional when empty), filled by `pending_gate` from the detail.
2. CLI pending print (`run.rs` line 3036): each shown entry as `  <label>:
   <text>` (`(untrusted)` appended when marked), each field as `  field
   <name> (<type>[, required for <options>]): default <default or
   none>` (`(untrusted)` appended when the default renders untrusted
   text), and the hint `  decide with: apb review <run> <gate>
   --decision <option> --field <name>=<value>`.
3. Dashboard `ReviewPanel.svelte`: the show entries as a definition list
   (an untrusted one with the label suffix `(untrusted text)`), one input
   per field (text: `<textarea maxlength>`, number: `<input type="text"
   inputmode="decimal">`, since a browser's number input drops letters
   it cannot take and would post "12abc" as 12; bool: a checkbox when the
   field has a default, else a `<select>` of "not chosen", `true` and
   `false` that starts at "not chosen", marked, and posts no value
   there, so an untouched bool is absent as on the CLI and MCP and a
   required one needs an explicit choice; enum: `<select>`), each name with the suffix `(untrusted text)` when its
   default renders untrusted text, prefilled with the
   default, a `(required for <options>)` marker, and the values sent with
   the decision. The values are held per open request (`refusalKey`: run,
   node, request seq) over the request's defaults, so a looping gate's next
   request starts from its own defaults; an enum without a default starts
   at an empty placeholder option. A number that does not read (anything
   but a whole decimal number: "12abc", "0x10" and "1_000" are refused,
   never read as the number they start with) posts nothing and names the
   field. Whether a required field has a value is never decided in the
   browser: the panel posts what was typed (a field the person edited
   and left blank is sent blank) and the server's one `check_values`
   decides, its refusal shown as the request's refusal, so the dashboard
   and the other three surfaces agree by construction. The pure half
   lives in a new `web/src/lib/reviewform.ts` (`initialValues`,
   `invalidNumbers`, `toPayload`; an edited text field is sent as typed,
   empty included, a number is read trimmed) with
   `web/src/lib/reviewform.test.ts`. Plain CSS over the existing CSS
   variables.
4. The run's HTML report lists a decision's field values under the
   decision (they are already scrubbed).

### Docs

`docs/HOWTO-authoring.md` `### Values a reviewer fills in (fields, show)`
after the scope section of D2: the example above, the types and their
coercion, `required` forms, defaults as templates, the JSON output and
how to read it, V244 to V247, untrusted defaults, and that an older apb
refuses such a run. `docs/GUARDRAILS.md` `## Passkeys on person-only
gates and approvals` (line 262) gets one paragraph: an assertion covers
the field values.

### Wrong implementations

1. Rendering `show:` or a default through the template engine and
   passing it on unmarked. Caught by
   `review_form_test::an_untrusted_default_is_marked_and_taints_readers`:
   a gate after a node bound to a connector with `untrusted: true` whose
   field defaults to that node's output gets V247, its `FieldView` is
   `untrusted`, and an agent that reads `{{nodes.confirm.output.x}}` is a
   follower in `untrusted::analyze`.
2. Checking the values only in the CLI. Caught by
   `review_form_test::a_value_of_the_wrong_type_is_refused_by_the_channel`
   (`apb_engine::review::post` with `amount: "ten"` is `Invalid`) and
   `review_form_test::a_hand_written_entry_with_bad_fields_ends_the_run`
   (a line appended to `reviews.jsonl` by hand ends the run
   `review_fields_refused`).
3. Storing a secret a person typed. Caught by
   `review_form_test::a_secret_value_in_a_field_is_refused_before_the_channel_holds_it`:
   a secret param's value posted in a text or number field, alone or
   inside other text, is refused naming the field and never the value,
   and neither `reviews.jsonl`, `events.jsonl` nor the gate's output
   holds it; and `review_form_test::a_text_value_is_refused_while_the_runs_secrets_do_not_read`.
4. Keeping the bare decision as the output of a gate with fields. Caught
   by `review_form_test::fields_reach_the_next_prompt_and_an_output_field_edge`.
5. A passkey that covers only the word. Caught by
   `review_form::tests::the_material_changes_with_the_fields` (core) and
   `review_form_test::a_passkey_minted_without_the_fields_is_not_consumed`.
6. Requiring a value for every option. Caught by
   `review_form::tests::required_applies_to_the_named_options_only`
   (`required: [approve]` lets `reject` through without a value).


## D6. The derived diagram next to the YAML

### The function (apb-core)

`crates/apb-core/src/diagram.rs` (new, `pub mod diagram;` in `lib.rs`):

```rust
/// The file a version directory holds beside `playbook.yaml`.
pub const DIAGRAM_FILE: &str = "playbook.mmd";
/// The first line's prefix; the YAML's digest follows it.
pub const DIGEST_PREFIX: &str = "%% apb-diagram ";
/// The longest label, in characters, after escaping.
pub const LABEL_MAX: usize = 60;

/// The flowchart text (Mermaid syntax) of `playbook`, whose YAML has
/// `digest` (`scope::digest_str`). Pure: the same definition gives the
/// same bytes.
pub fn mermaid(playbook: &Playbook, digest: &str) -> String;

/// Writes `playbook.mmd` into `version_dir` from `yaml` (already
/// validated), atomically.
pub fn write_beside(version_dir: &Path, yaml: &str) -> std::io::Result<()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramState { Current, Missing, Stale, Edited, Unreadable }

/// How the diagram of one version directory stands against its YAML.
pub fn state_of(version_dir: &Path) -> DiagramState;

/// Counts over every version directory under `playbooks_dir`
/// (`<dir>/<id>/<version>/playbook.yaml`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagramScan { pub current: u32, pub missing: u32, pub stale: u32, pub edited: u32 }
pub fn scan(playbooks_dir: &Path) -> DiagramScan;
```

The text, line by line:

1. `%% apb-diagram <digest> derived from playbook.yaml; apb diagram
   --rebuild writes it again`.
2. `flowchart TD`.
3. One line per node in definition order: `  n<i><open>"<label>"<close>`
   with `i` the node's index (never author text in an id). Shapes:
   `start` and `finish` `(["...`/`"])`, `human_review`, `publish`,
   `condition` and `judge` `{"..."}`, every other kind `["..."]`. The
   label is `<title or id> (<kind>)` (`kind` the YAML type name;
   `finish` adds its outcome: `(finish success)`), a list node adds
   ` list`, a node with `person_only` adds ` person only`.
4. One line per edge in declaration order (the edge-address order of
   `apb_core::edge_address::edge_addresses`,
   `crates/apb-core/src/edge_address.rs` line 206): `  n<a> --> n<b>`,
   or `  n<a> -->|"<label>"| n<b>` when the edge has a condition, a
   `join` or a `max_traversals`. Label parts, joined by `, `:
   `review_status` the option; `output_field` `<field> <op> <value>`
   (`=`, `<`, `<=`, `>`, `>=`); `output_match` `matches <pattern>`;
   `node_status` `<node> <status>`; `judge` `judge: <question>`;
   `no_new_items` `no new <field>`; `join all` or `join any`; `max
   <n>`. An edge naming a node the definition lacks is left out.
5. Escaping, for every label, in this order: every control character and
   line break becomes a space; `#` becomes `#35;`, `"` `#quot;`, `&`
   `#amp;`, `<` `#lt;`, `>` `#gt;`, `|` `#124;`, `\`` `#96;`; then
   cut to `LABEL_MAX` characters at an entity boundary (never inside a
   `#...;`). Author text therefore never starts a line, never closes a
   quote and never forms a directive: no `click`, link, `%%{init}` or
   style line can come from a title, an id or a condition.

`write_beside` parses `yaml` with `Playbook::from_yaml` and writes
`mermaid(&pb, &crate::scope::digest_str(yaml))` (`text_of`) with
`crate::fsutil::atomic_write`, into a version directory its caller just
made. `state_of` reads both files beneath the version directory through
`crate::fsutil::beneath` (no link followed, no FIFO or device opened): no
`playbook.yaml`, one that is not a regular file or one that does not
parse: `Unreadable`; no `playbook.mmd`: `Missing`; one that is not a
regular file: `Edited`; the first line's digest is not `digest_str` of
the YAML: `Stale`; the whole file differs from `mermaid` of the YAML:
`Edited`; else `Current`.

The file is derived: neither a person nor an agent maintains it. An edit
of `playbook.mmd` is overwritten: whenever apb writes or repairs the
file (every version write, every move of `current`, `apb diagram
--rebuild`), the result is the diagram of exactly the YAML bytes,
whatever was there. `Edited` exists only for the doctor line between
writes. `replace_beside(dir, version_rel, text)` is the one write of a
stored version's diagram: an entry at `playbook.mmd` that is not a
regular file (a link, a FIFO, a device) is replaced by a fresh regular
file written under a temporary name in the same directory, beneath the
anchored directory, and renamed over the entry: the rename replaces a
link itself, never what it points at, so no moment of the repair leaves
the version without a `playbook.mmd`. A directory at that name, empty
or not, is never removed: it is an error that names the file and says to
remove it.

### Written at every version write, read back nowhere

The invariant: at no crash point does a version directory that anything
can read or that `current` can name hold a `playbook.yaml` without the
`playbook.mmd` derived from exactly those bytes. Every path that creates
a version builds it in a staging directory (`.tmp-<version>-<nanos>`,
never listed) through `versioning::stage_definition` (the yaml, then
`crate::diagram::write_beside` from the same string) and publishes it by
one rename inside `floor::publish`, and every move of `current` gives
its target the diagram first:

1. `commit_version_dir` and `stage_version`
   (`crates/apb-core/src/versioning/mod.rs`): every write through it
   (`create_version_impl`, `create_patch_version_with`,
   `write_version_in` for bundled content, the import path) stages and
   renames.
2. `create_draft_in_by`: the draft stages in the playbook folder it just
   made and renames to `1.0.0`. A retry after a crash finishes the draft
   (`interrupted_draft`): a folder with no `current` and nothing but
   hidden entries (staging leftovers, the pointer lock), the pointer's
   intent and history files and a `1.0.0` holding exactly the bytes this
   creation stores is this draft interrupted, and the retry publishes it
   (or, when `1.0.0` is there, only moves `current`); anything else stays
   a `Conflict`.
3. `schema_migrate::apply`: the migrated version is staged
   (`stage_migrated`: the source version copied, the rewritten yaml and
   its diagram) and renamed to the new version; a repeated apply finds a
   complete version or none.
4. `scope_move::copy_playbook_to`: the copy is staged and renamed already;
   before the rename every copied version gets its diagram when it is
   missing, stale or edited (`crate::diagram::ensure_beside`).
5. `current::move_current` (every writer of `current`): before `current`
   is written (and, for a move that names an existing version, before its
   intent), `current::require_diagram` runs
   `crate::diagram::ensure_beside`, which writes the target's diagram
   from exactly its `playbook.yaml` bytes, read and written beneath the
   playbook folder (no link followed), when it is missing, stale or
   edited, through `replace_beside`. Anything but `Current` afterwards
   refuses the move and `current` stays where it was: an entry that
   cannot be replaced (the error names the file), or a version whose YAML
   does not read as a playbook, so no diagram can be derived. So a
   version an interrupted write, an older apb or a hand edit left without
   its diagram is completed before anything points at it.

A run's `runs/<id>/playbook.yaml` (`run_config.rs`) is a snapshot, not a
version directory, and carries no diagram. Moving a playbook or a version
out of the trash renames a whole directory and writes no yaml. `apb
doctor` naming missing and stale diagrams is the safety net, never the
mechanism.

Outside every digest and trust by construction: `definition_digest`
(`crates/apb-core/src/scope.rs` line 130) hashes the YAML and `scripts/`
only, and `PlaybookBundle` (`crates/apb-core/src/bundle/mod.rs` line 49)
carries the YAML, the layout and the scripts only, so an export never
carries it and an import writes it fresh. No code reads `playbook.mmd`
except `state_of` and `scan`: every surface renders `mermaid` from the
YAML it already loaded.

### Surfaces

1. `apb doctor` (`doctor_env`, `crates/apb-cli/src/doctor.rs` line 136,
   after the concurrency line at line 180): `scan` of
   `<root>/.apb/playbooks` and of
   `apb_core::store::global_playbooks_parent()` joined with `playbooks`
   (`crates/apb-core/src/store.rs` line 43); nothing found: no line; all current: `OK diagrams: {n}
   current`; else `WARN diagrams: {missing} missing, {stale} stale,
   {edited} edited of {total}; apb diagram --rebuild --all writes them
   again`. Doctor stays read-only.
2. `apb diagram` (new `crates/apb-cli/src/diagram.rs` with `DiagramArgs`
   and `diagram_cmd`; `Command::Diagram(DiagramArgs)` in `main.rs` next
   to `Command::Health` at line 1363):
   1. `apb diagram <id> [--version V] [--scope project|global] [--out
      PATH]` prints `mermaid` of the loaded definition (or writes it to
      `PATH` with `apb_core::fsutil::atomic_write`, printing `wrote
      PATH`). A version that is not one path segment (`is_safe_segment`)
      is refused with exit 2 before anything is read.
   2. `apb diagram --rebuild <id>` rewrites `playbook.mmd` (through
      `replace_beside`, so an edit is overwritten, an entry that is not
      a regular file is replaced by a rename over it without being
      followed, and a directory at that name skips the version) of every
      version of `id` in its scope; `apb diagram --rebuild --all` of every
      playbook of the project and the global store. Both read and write
      through `apb_core::fsutil::beneath` anchored at the store's
      `playbooks` directory: a playbook directory that is a link is
      refused (`<id>` exit 2) or passed by (`--all`), a version
      directory that is a link is not listed, and a link swapped in
      later is refused by the walk, so nothing is written outside the
      store. `apb diagram <id>` reads its YAML the same way. Prints `rebuilt {n}
      diagrams` (and each version that does not read, `skipped <id>
      <version>: <why>`, and each playbook whose versions do not list,
      `skipped <id>: its versions do not list: <why>`). Exit 0, or 1 when
      one was skipped.
3. MCP `playbook_get` (`crates/apb-mcp/src/tools/playbook.rs` line 136)
   adds `"diagram": mermaid(&loaded.playbook, &digest_str(&loaded.yaml))`
   in both detail modes.
4. HTTP `GET /api/playbooks/{id}` (`get_playbook`,
   `crates/apb-server/src/routes/playbooks.rs` line 832) adds
   `"diagram"` the same way.
5. Dashboard `web/src/pages/PlaybookView.svelte`: a collapsed "Diagram
   text" block with the text in a `<pre>` and a copy button
   (`navigator.clipboard.writeText`); no diagram library, no rendering.
   The block is `web/src/lib/DiagramText.svelte`, placed in the layers
   panel's column below its bar (the panel bounds its width and the
   `<pre>` scrolls), so it never covers the layer toggles or the zoom
   controls. Plain CSS over the existing CSS variables.
6. HTML report: `run_graph` (`crates/apb-engine/src/report_html/graph.rs`
   line 59) appends, after the SVG of the section at line 77, a
   `<details><summary>Diagram text</summary><pre>...</pre></details>`
   whose text is `mermaid` of the executed definition passed through
   `w.text` (`sections.rs` line 23, the writer's redacted, escaped text).
   Above `GRAPH_MAX_NODES` (line 24) the text block is still written: a
   text has no layout limit.

### Docs

`docs/HOWTO-authoring.md` `## Visual editor and graph check` (line 254)
gets `### The diagram text (playbook.mmd)`: what it is, that it is
derived and never read back, that an edit of `playbook.mmd` is
overwritten, `apb diagram`, the doctor line and `--rebuild`.

### Wrong implementations

1. Accepting diagram text from a save request or reading the file back.
   Caught by `diagram_cli_test::an_edited_diagram_is_never_used`: edit
   `playbook.mmd` of a version, then `apb diagram <id>`, MCP
   `playbook_get` and the HTTP detail all return the text of the YAML,
   and `apb doctor` names one edited diagram.
2. A write site without the diagram. Caught by
   `versioning_test::every_version_write_leaves_a_diagram`
   (`crates/apb-core/tests/suite/versioning_test.rs`): `create_version`,
   `create_version_with_override`, `save_definition`, `save_definition_by`,
   `create_with_scripts`, `save_imported`, `create_patch_version`,
   `create_run_patch_version`, `create_draft_in` and `create_draft_in_by`
   each leave a `Current` diagram. A crash between the two writes is
   caught by `versioning_test::a_retried_draft_after_a_crash_is_completed_with_its_diagram`
   (an empty folder, a staging leftover, the yaml without its diagram,
   the version without `current`: each retry ends `Current` at 1.0.0),
   `schema_migrate_test::a_retried_migration_completes_the_diagram`,
   `current::tests::a_move_completes_the_diagram_of_its_target` (missing,
   stale and edited are written again),
   `current::tests::a_move_replaces_what_is_not_a_regular_diagram_file`
   (a link, a FIFO and an empty directory replaced, a link's target left
   alone, a full directory refusing the move),
   `current::tests::a_move_to_a_version_whose_diagram_cannot_be_derived_is_refused`,
   `diagram_cli_test::a_rebuild_overwrites_an_edit_and_replaces_what_is_not_a_file` and
   `scope_move::tests::every_copied_version_carries_its_diagram`; `write_version_in` is crate-private, so
   `versioning::tests::shipped_content_leaves_a_diagram` (the unit tests at
   `versioning/mod.rs` line 1772) covers it.
3. The diagram inside a digest or a bundle. Caught by
   `diagram::tests::the_diagram_changes_no_digest` (`definition_digest` of
   a version is the same with and without `playbook.mmd`) and
   `bundle_test::an_export_carries_no_diagram`
   (`crates/apb-core/tests/suite/bundle_test.rs`).
4. Author text that escapes its label. Caught by
   `diagram::tests::author_text_never_leaves_its_label`: a title
   `x"] click n0 "https://e" \n%%{init: {}}%%` and an option `a|b` give
   lines that each start with two spaces and `n`, `flowchart` or `%%
   apb-diagram`, with no `click` outside quotes.
5. Ordering by a map. Caught by `diagram::tests::the_text_is_stable` (two
   renders of one definition are equal and follow definition order).


## Release notes

The user-visible changes, for the release notes (the final report copies
them):

1. A review gate that uses `max_scope`, `fields` or `show` shows what
   the decision covers: commits, changed files and lines, list items
   settled, connector writes, sends and result snapshots, as counts only.
   `apb stats` and the run outcome report such decisions made faster than
   their scope takes to read. A plain gate and a publish consent journal
   their request as before, so an older apb still resumes a run waiting
   there.
2. A gate can withhold options above a size limit (`max_scope`); a
   withheld option is refused on every channel.
3. A gate can show earlier outputs (`show:`) and take typed values
   (`fields:`); a gate with fields outputs one JSON object instead of the
   bare decision word, and routing on `review_status` is unchanged. A
   passkey assertion covers the values. An older apb refuses to continue
   such a run.
4. Every new version directory carries `playbook.mmd`, a flowchart text
   derived from the definition; `apb diagram` prints it, MCP
   `playbook_get`, the dashboard playbook page and the HTML report show
   it, and `apb doctor` names missing, stale and edited diagrams. The
   file is derived: an edit of it is overwritten at the next version
   write, move of `current` or `apb diagram --rebuild`.

## Files

1. apb-core (17): `schema.rs`, `review_form.rs` (new), `lib.rs`,
   `untrusted.rs`, `diagram.rs` (new), `versioning/mod.rs`,
   `schema_migrate.rs`, `validate/mod.rs`, `validate/guarded.rs`,
   `validate/review_form.rs` (new), `validate/templates.rs`,
   `validate/node_io.rs`, `tests/suite/schema_test.rs`,
   `tests/suite/validate_review_test.rs`, `tests/suite/versioning_test.rs`,
   `tests/suite/bundle_test.rs`, `tests/suite/schema_migrate_test.rs`.
2. apb-engine (17): `event.rs`, `state.rs`, `scheduler.rs`,
   `scheduler/review_gate.rs`, `scheduler/publish.rs`, `review.rs`,
   `review/scope.rs` (new), `review/form.rs` (new), `progress.rs`,
   `run_stats/interventions.rs`, `run_stats/render.rs`,
   `run_outcome/summary.rs`, `report_html/sections.rs`,
   `report_html/graph.rs`, `tests/main.rs`,
   `tests/suite/review_scope_test.rs` (new),
   `tests/suite/review_form_test.rs` (new).
3. apb-mcp (5): `server/args.rs`, `tools/run.rs`, `tools/playbook.rs`,
   `tests/suite/review_tool_test.rs`, `tests/suite/read_tools_test.rs`.
4. apb CLI (7): `main.rs`, `run.rs`, `doctor.rs`, `diagram.rs` (new),
   `tests/main.rs`, `tests/suite/people_cli_test.rs` (new),
   `tests/suite/diagram_cli_test.rs` (new).
5. apb-server (6): `routes/runs.rs`, `routes/passkeys.rs`,
   `routes/playbooks.rs`, `ts_contract.rs`, `tests/suite/runs_api_test.rs`,
   `tests/suite/api_test.rs`.
6. web (8): `src/lib/api.gen.ts` (generated), `src/lib/api/core.ts`,
   `src/lib/runposts.ts`, `src/lib/ReviewPanel.svelte`,
   `src/lib/ReviewPanel.test.ts`, `src/lib/reviewform.ts` (new),
   `src/lib/reviewform.test.ts` (new), `src/pages/PlaybookView.svelte`.
7. Docs (3): `docs/HOWTO-authoring.md`, `docs/GUARDRAILS.md`,
   `docs/STATS.md`.
8. Records (3): `docs/superpowers/specs/2026-11-XX-people-in-the-loop-design.md`
   (new), `CLAUDE.md`, `AGENTS.md`.
9. This change (6): `openspec/changes/people-in-the-loop/` (proposal,
   design, tasks, three spec files).

That is 72 files. `crates/apb-engine/src/scheduler/prepare.rs` is not in
the list: `run_guard_of` needs no change (D0 "Two new guarded features").
A file the compiler names beyond these (a literal construction of a
changed struct) is fixed where it is and counted in the report.
