# Design: inbox waits

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is today, read on main efae5822
(after `typed-inbox-reads` #287, `people-in-the-loop` #288,
`before-running-alone` #289 and `wait-park` #290) and re-checked on
main 3bb424be, where `eval-doubles` (#291) is merged, all archived under
`openspec/changes/archive/`. The lines the fourth pass touches are those
of 3bb424be; elsewhere a line may have moved with eval-doubles (the
journal readers in `event.rs` moved by 13 lines), so find the site by
the name given next to it, never by the number alone.

One decision, D4 (the number is kept from the plan of the 1.0 sequence;
D1 and D5 are the change `eval-doubles`). Nothing here is left to the
implementer: where a choice was open it is decided and the reason is
given; tasks.md only orders the work.

This change uses exactly one thing `eval-doubles` added, marked **on
main since eval-doubles** wherever it appears: the storage redactor
`RunRedactor::for_storing` (`crates/apb-engine/src/run_redact.rs` line
543, with `secret_json` at line 651). Nothing else of eval-doubles is
used. Its two doc comments that name `<node>#inbox` as "a site a later
change adds" (`crates/apb-core/src/eval_doubles.rs` line 42,
`crates/apb-engine/src/eval/doubles.rs` line 66) become true with this
change and are left as they are.

## The shape in one paragraph

An inbox wait is one more binding site, `<node>#inbox`, so the run gate
trusts its connector and its exact account and the manifest records
them like any binding; it never calls a function. A visit reads the
account's stored events with seq above its position (the `through_seq`
of the node's latest `inbox_wait_read` line in the run's journal) and
ends at the first event that matches. Its output is that event's seq,
receive time, type and the values of the fields the playbook declared,
scrubbed; no event body is ever stored. The end is one write of an
`inbox_wait_read` line and the existing `wait_signalled` or
`wait_timeout`. A long wait with nothing matched parks through the
merged wait park; the park's waker resumes the run once an event above
the position is stored. Nothing is acknowledged and nothing outside the
run directory is written.

## D0. Shared pieces

### Validator codes

| Code | Severity | Rule |
|---|---|---|
| V284 | error | a malformed `wait_for: { type: inbox }` block |
| V285 | error | an inbox wait names a connector that is not installed, or one that has no `webhook:` block, so its inbox never receives anything; an MCP connector gets an explicit message that it cannot declare `webhook:` |
| V286 | warning | an inbox wait sets `types` but the connector's webhook block names no event type (`WebhookSpec::names_event_type`, `crates/apb-core/src/connector/def.rs` line 319) |
| V287 | error | two connector binding sites have the same id: a node named like another node's derived site (`a#inbox` beside the inbox wait node `a`, `a#check` beside the list node `a`) |

Checked on efae5822: `grep -rhoE '"V2[0-9][0-9]"' crates | sort -u`
prints V200, V210, V211, V212, V220, V221, V222, V240 to V247, V249 and
V250; on 3bb424be it adds V280 and V281 (eval-doubles, merged). Owners
in flight, read in their worktrees on 2026-11-20: `start-rules` V260 to
V267 (V268 to V279 reserved), `incremental-lists-and-session-fork`
V283, `mcp-tool-connectors` V300 to V303, `one-point-oh-fixes` V320,
`secret-params-in-run-files` V340; V282 stays reserved and unused
(eval-doubles). V284 to V287 are free and this change's; V288 to V299
stay reserved for this family.

The module is `crates/apb-core/src/validate/inbox_wait.rs`, declared in
`validate/mod.rs` as `mod inbox_wait;` between `mod health;` (line 24)
and `mod isolation;` (line 25), and called right after the readiness
call (line 467):

```rust
readiness::check_readiness(playbook, &mut r); // V249, V250
inbox_wait::check_inbox_waits(playbook, ctx, &mut r); // V284-V286
inbox_wait::check_site_ids(playbook, &mut r); // V287
```

### Journal

| Entry | Kind | Checkpoint |
|---|---|---|
| `inbox_wait_read { node, through_seq, examined, end, output }` | new variant of `EventPayload` (`crates/apb-engine/src/event.rs`) | no; skippable once a checkpoint follows it (see "An older apb" for the journals where none does yet) |

Every field is `#[serde(default, ...)]` (decision record 0001). The
variant joins the no-op arm of the run-state fold
(`crates/apb-engine/src/state.rs` lines 714 to 716, `WaitStarted |
WaitSignalled | WaitTimeout`, the one exhaustive `EventPayload` match)
and gets one line in
`crates/apb-engine/tests/fixtures/journal/known_events.jsonl` (loaded by
`tests/suite/journal_compat_test.rs` line 15). It needs no line in
`EventPayload::is_checkpoint` (`event.rs` line 3807, a `matches` macro over
five variants). No existing event gains a field.

### What a run that uses no inbox wait writes

Exactly the journal main writes, in every state a run can reach,
crashes included: the same events, the same appends, the same order.
Every write this change adds happens only inside the inbox arm of the
wait branch, and the positional visit rule (D4 "An ended visit") decides
only inbox waits: a timer or webhook wait keeps main's visit guard
exactly, including what it does after a crash between its end and
`node_finished`. The park's reason form, its request and its guard
lines are unchanged for a timer wait. The park wording on the surfaces
changes (D4 "Surfaces"); no surface text is journaled. The existing
corpus rows W1 and W2 (`crates/apb-cli/tests/suite/journal_corpus_test.rs`
lines 117 to 126) pin the literal event-type sequences of a webhook and
a timer wait and must pass unchanged. A playbook without an inbox wait
has the same binding sites as on main, so its permit, manifest and
consent are unchanged.

### Guarded features and manifest

No `GuardedFeature` and no manifest mark is added. `guarded_features`
(`crates/apb-core/src/validate/guarded.rs` line 156) names the existing
`wait_park` also for a definition with an inbox wait, because the park's
condition requires that guard (wait-park D1 condition 5) and an inbox
wait parks through the same check. `untrusted_input` is named as today
when the untrusted analysis finds a follower, which an inbox wait now
creates (D4 "Untrusted text"); no code in `guarded.rs` changes for that.
The manifest's existing `connectors` and `connector_grants` carry the
wait's connector, account and site (D4 "Authorization").

### An older apb

`WaitFor` (`crates/apb-core/src/schema.rs` line 2259) is an internally
tagged enum with no fallback variant, so `Playbook::from_yaml` of an
apb without this change refuses `type: inbox` with a parse error. Every
path of an older apb that loads the run's definition (drive, resume,
validate, the definition shown on its surfaces) refuses that run with
that parse error; it cannot drive or resume it, with or without the
`wait_park` guard.

Its journal readers: an older binary does not know `inbox_wait_read`.
Its strict reader (`event::read_all`, line 3909) refuses a journal in
which an unknown event follows the last checkpoint
(`check_unknown_settled`, line 3963). That is the state of a journal
while an inbox visit is open: after the start pair (D4 "The visit"
step 2) or a torn start line, after a progress line and before its
`run_paused`, and after the end pair and before its `node_finished`. A
crash can leave a journal in the last three states. An older apb refuses to read such a journal on
its strict paths; its stop still goes through, because a stop only
appends `run_aborted` (`event::read_all_for_stop`, line 3926, with a
warning). Once a checkpoint follows the line (`run_paused`,
`node_finished`), an older reader skips it and counts it (decision record
0001). Nothing outside the run directory refers to the run, so an older
apb has nothing to clean up. A new guard or mark would add nothing: the
definition alone already stops every older driver. Rows I1 to I3 of the
journal corpus pin this (D4 "Tests").

### Files this change adds

1. `crates/apb-core/src/inbox_wait.rs`: the wait's types, its site and
   binding, and the pure matching and value rules; `pub mod inbox_wait;`
   in `crates/apb-core/src/lib.rs`.
2. `crates/apb-core/src/validate/inbox_wait.rs`: V284 to V287.
3. `crates/apb-engine/src/inbox_wait/mod.rs`: the authorization, the
   visit (start, read, gap, match, end, park preparation) and
   `position`; `pub mod inbox_wait;` in `crates/apb-engine/src/lib.rs`.
4. `crates/apb-engine/src/inbox_wait/pending.rs`: the park waker's check
   `delivery_pending`.
5. `crates/apb-engine/tests/suite/inbox_wait_test.rs`, declared in
   `crates/apb-engine/tests/main.rs` in alphabetical place as
   `#[path = "suite/inbox_wait_test.rs"] mod inbox_wait_test;`.

### Unchanged from main

1. The inbox store's write side, `cursors.yaml`, every consumer, the ack
   op and retention (`crates/apb-core/src/connector/inbox.rs`): a wait
   holds no consumer and writes nothing there.
2. The connector call path (`crates/apb-engine/src/connector/call/`), its
   origins, the inbox call builder and every function schema: a wait
   makes no connector call, and its site grants no function.
3. The run gate (`crates/apb-engine/src/gate/mod.rs`,
   `gate/trust_scan.rs`), the run permit, the connector snapshot
   (`crates/apb-engine/src/connector/run.rs`), `scheduler/prepare.rs`,
   the consent sources and the fit report: they walk `connector_sites`
   and cover the wait's site with no change (D4 "Authorization").
4. The ingest listener (`crates/apb-server/src/ingest.rs`): no wake post,
   no start sweep.
5. The journal's terminal observer (`terminal_hook.rs`), `apb resume
   --all` (`pause/resume_all.rs`) and `run_end::record`: nothing to
   retire.
6. The wait park of `wait-park` (#290) as merged: the reason form
   `wait (<node>)` and its parse (`pause/cause.rs` lines 21 and 28),
   `PauseCause::Wait`, `cause::is_park_event` (line 78),
   `PauseRequest::is_park`, the clock exemption (`pause/clock.rs`), the
   notify and interventions folds, the stop watcher, `pending_pause`,
   `ParkRetry` and `PARK_RETRY`, the request's shape (engine, drain,
   `until`, `keep_lock`), the control scan's rules (the park applied only
   as the last word, re-decided under the run's event lock), the waker's
   lease, its pending-control check and its resume through
   `resume_while(.., WakeHold::Pause(seq))`, the numbering lock and the
   crash table of that change.
7. `allocate_run` and every launch surface: a run with an inbox wait
   starts as any run that binds a connector.
8. Eval doubles, the eval capture and `eval::doubles::reserve`: an eval
   case refuses an inbox wait before it starts (D4 "Evals and
   scenarios").
9. The scenario capture (`crates/apb-engine/src/scenario/capture.rs`
   lines 353 to 391): it already reads a timed-out wait as a `timed_out`
   answer and a signalled one as a `wait_signal` gap, which is right for
   an inbox wait too (D4 "Evals and scenarios").
10. The dashboard journal (`web/src/lib/journal.ts`): `eventNote`
    (line 121) gives no note to a kind it does not list ("Every other
    kind has none", line 120), so `inbox_wait_read` shows as its type.

## D4. Inbox waits

### Schema

`crates/apb-core/src/schema.rs` `WaitFor` (line 2259, `#[serde(tag =
"type", rename_all = "snake_case")]`) gains a third variant:

```rust
pub enum WaitFor {
    Timer { seconds: u64 },
    Webhook { key: String },
    /// The first matching event stored in a connector account's inbox
    /// (D4). Boxed: it is much larger than the other two.
    Inbox(Box<crate::inbox_wait::InboxWait>),
}
```

The node's `timeout_seconds` (`NodeKind::Wait`, line 2168, a required
`u64`) bounds it; a wait without it is a parse error, as for every wait.

New `crates/apb-core/src/inbox_wait.rs`:

```rust
/// `wait_for: { type: inbox, ... }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InboxWait {
    pub connector: String,
    pub account: String,
    /// Conditions the event meets (all of them). Empty: every event.
    #[serde(default, rename = "match", skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Stored event types that match (`InboxEvent.event_type`). Empty:
    /// every type, typed or not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// The fields of the matching event the output keeps. Empty: the
    /// output holds `seq`, `received_at` and `type` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub take: Vec<Take>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// A dot path into the body (`pull_request.number`); a numeric
    /// segment indexes an array.
    pub path: String,
    /// A template over every reference V13 accepts (`params.*`,
    /// `nodes.<id>.output[.<field>]`, review decisions, `run.context`,
    /// `run.previous_success`), rendered once per drive of a visit
    /// through the one template render prompts and judge states use
    /// (D4 "The visit" step 2). The body value matches when its text
    /// equals the rendering: a string as it is, anything else as compact
    /// JSON (so `equals: "42"` matches the number 42). This is exact
    /// text equality, not the JSON equality of an inbox read's filter:
    /// a rendered template is always text.
    pub equals: String,
}

/// One declared output field read out of the matching event's body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Take {
    /// The output key, read later as `nodes.<wait>.output.<name>`.
    pub name: String,
    /// A path in the baseline observation grammar
    /// (`crate::baseline::extract::parse_path`, `baseline/extract.rs`
    /// line 27), without a `[]` segment.
    pub path: String,
    /// `field` (the default: one string, bool or number) or `number`.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<crate::baseline::ObservationType>,
}

/// The most `take` entries one wait declares.
pub const MAX_TAKE: usize = 16;
/// Output keys apb sets itself; a `take` name may not be one of them.
pub const RESERVED_NAMES: [&str; 3] = ["seq", "received_at", "type"];
/// The least time between two reads of the account's log by a live
/// visit, and only when the log changed (D4 "The visit" step 3).
pub const MIN_READ_INTERVAL_MS: u64 = 1_000;

/// The binding site of node `node`'s inbox wait: `<node>#inbox`.
pub fn inbox_site(node: &str) -> String;
/// The binding the site carries: the connector, exactly the one
/// account, no function, no call budget, untrusted.
pub fn binding(w: &InboxWait) -> ConnectorBinding;
/// Whether an event matches (`types` through
/// `connector::event_filter::type_passes`, then every condition).
/// Asserts in debug builds that `rendered` has one entry per condition.
pub fn matches(w: &InboxWait, body: &Value, rendered: &[String], event_type: Option<&str>) -> bool;
/// The text of the body value at `path` (`connector::common::value_at_path`,
/// `common.rs` line 117): a string as it is, anything else as compact
/// JSON; None when the path does not resolve.
pub fn value_text(body: &Value, path: &str) -> Option<String>;
/// The output object of a matching event: `seq`, `received_at`, `type`
/// (absent when the event has none) and one key per `take` entry.
pub fn output(w: &InboxWait, seq: u64, received_at: u64, event_type: Option<&str>, body: &Value) -> serde_json::Map<String, Value>;
```

The type test is one function, `type_passes` in
`apb_core::connector::event_filter` (empty passes everything; an
untyped event passes only an empty list), shared by an inbox read's
filter, `matches` and the park waker's check; the inbox wait has no type
test of its own.

`binding(w)` is `ConnectorBinding { name: w.connector, accounts:
Some(Vec::from([w.account])), functions: FunctionsAllow::List(Vec::new()),
max_calls: None, on_unknown: None, untrusted: true }`
(`schema.rs` line 1330).

`output` reads each `take` value with the baseline observation reader:
`crate::baseline::extract::extract` (`baseline/extract.rs` line 123)
over an `Observe { id: name, kind: kind.unwrap_or(Field), path, count:
false, expect: ExpectRule::default() }` (`baseline/mod.rs` line 180). Its
limits apply unchanged: a text longer than `MAX_ITEM_CHARS` (256
characters, `baseline/mod.rs` line 38), a text with a hidden character,
a missing key, a non-scalar or a number that is not finite reads as
unknown. A known value is stored as JSON (`Text` a string, `Bool` a
bool, `Number` a number); an unknown one as `null`. The unknown reason
is not stored. So an output is at most 3 fixed keys plus 16 values of at
most 256 characters each, and no event body leaves the inbox store.

The names are the node's declared output fields:
`validate/node_io.rs` `declared_fields` (line 168) gains an arm for an
inbox wait, right after the list node's and before `outputs.fields`, with
`seq`, `received_at`, `type` and every `take` name: those are the names
the wait outputs, so an `outputs.fields` block on the wait never
replaces them. So V46 checks `nodes.<wait>.output.<name>` references and
`output_field` edges against them, as for any declared field.

The wait ends at its first matching event. There is no window, no count
and no closing condition: a playbook that collects several events loops
around the wait (D4 "Docs").

### Authorization

The wait is one more entry of the one connector binding walk,
`Playbook::connector_sites` (`crates/apb-core/src/connector/sites.rs`
line 44): after the list item checks, one site per inbox wait,
`(inbox_site(node), Vec::from([binding(w)]))`, owned like the `<node>#check`
sites (`list::check_site`, `list/mod.rs` line 60).

The site walk says what a site is. `Site` (`connector/sites.rs`) carries
its id, its bindings and a `SiteKind` (`Node`, `ItemCheck { node }`,
`InboxWait { node }`, `Engine`), set by `connector_sites` from the node
that owns the site. V302 and every label read the kind; no code infers
a kind from an id, because `#` is legal in a node id and V287 refuses
only a real collision. `site_label(&Site)` (and `kind_label` for a
caller that holds only the kind) reads an inbox site as ``the inbox wait
of node `<id>` ``; `gate::connector_sources` and
`connector::resolve::expand_grant` take their label from the `Site`, not
from the id, and a caller that holds only an id resolves it against the
playbook's sites. So an ordinary `agent_task` named `fetch#inbox` that
binds an MCP connector, with no wait named `fetch`, is a node: it
validates without V302 and is labelled as a node. Main's list rules stay
as they are: V85 still reserves every id ending in `#check`, and the
spend attribution of `<node>#check` is unchanged.

Site ids are unique (V287). Node ids are not barred from containing `#`,
so a node named `a#inbox` beside the inbox wait node `a` (or `a#check`
beside the list node `a`, possible on main already) would yield a second
site with the same id, and `resolve_playbook` inserts each site's grants
into a `BTreeMap` keyed by site id (`connector/resolve.rs` line 141,
inside the walk over `connector_sites` that starts at line 112): the
later site's grant
replaces the earlier one and the other node's approved call fails at
run time. `inbox_wait::check_site_ids` (`validate/inbox_wait.rs`)
collects the ids `connector_sites` yields, every node's site included
whether it binds or not, plus the derived ids `check_site(n)` of every
list node and `inbox_site(n)` of every inbox wait whether or not their
site binds, and refuses with V287 every id that a node shares with a
derived id ("node id `a#inbox` is the binding site of the inbox wait of
node `a`; rename the node"). Two nodes with one id stay V01 (`validate/
graph.rs` line 13) and are not reported again. It is an error of the
validator, so `apb validate` and the editor report it, and a run's
preparation refuses the definition (`scheduler/prepare.rs` line 862,
`prepare_run_target`, every validator error) before the connector
snapshot writes `manifest.connector_grants`; no run executes over a
grant map in which one site replaced another.

The site is not an engine site in the sense of `is_engine_site` (line
30): that prefix `@` names a run-level site no node may take (V68), and
the inbox wait belongs to one node. Like `<node>#check`, it is a
node-scoped site the engine itself reads under. No flag is needed,
because every gate step reads only the bindings:

1. The run gate's connector check (`gate/mod.rs` `check_connectors`,
   line 1290) runs `trust_scan::scan_connectors` (`gate/trust_scan.rs`
   lines 47 and 50, `binds_connectors` then `resolve_playbook`), which
   walks the sites (`connector/resolve.rs` lines 93 and 112):
   `resolve_connector` (line 163) refuses a connector that is not
   installed or whose accounts do not validate (`connector_unresolved`);
   `expand_grant` (line 198) refuses an account that is not configured
   and expands `List([])` to no function. Then the account's secret
   variables must be set (`connector_env_missing`), and the connector
   digest and every merged account digest must be approved
   (`untrusted_connector_requires_approve`,
   `unapproved_connector_account`; `acknowledge_untrusted` does not
   bypass them).
2. The permit's two maps reach the engine verbatim;
   `scheduler/prepare.rs` (lines 344 to 358) calls
   `connector::run::snapshot_connectors` (`connector/run.rs` line 41),
   which re-resolves, re-verifies both maps exactly, copies
   `connector.yaml` and writes `manifest.connectors` (with the account
   digest) and `manifest.connector_grants["<node>#inbox"]` with the one
   account.
3. The consent sources (`gate/mod.rs` line 523, `connector_sources` line
   481) list only granted irreversible functions: none.
4. `apb_core::preflight` (line 69) and the fit report see a playbook
   that binds a connector, as for any binding.

No function is granted, so no caller can call under the site: the call
gate refuses a function the grant does not list (`connector/call/target.rs`
lines 72 to 80). `effects` and the untrusted analysis read
`NodeKind::connector_bindings` (`schema.rs` line 805), not the sites, so
the wait gains no `network` or `external` effect; its untrusted output is
declared in D4 "Untrusted text".

At read time, `inbox_wait::authorize(root, run_dir, node, w) ->
Result<(), String>` (engine, `inbox_wait/mod.rs`) checks, in order:

1. `manifest.grants_for(&inbox_site(node))` (`manifest.rs` line 744)
   holds a grant of `w.connector` whose accounts are exactly
   `[w.account]`, and `manifest.connector(w.connector)` holds that
   account;
2. the installed connector's digest (`connector::store::load`,
   `store.rs` line 149) equals the manifest connector's `digest`, and
   the live merged account (`connector::config::load_merged`,
   `config.rs` line 100) has the `account_digest` (line 231) the
   manifest account records;
3. both digests are approved in the trust store (`TrustStore::load`,
   `is_approved`, `trust.rs` line 309), the order `LiveTarget::require_trust`
   uses (`connector/call/target.rs` line 183).

A refusal names which check failed and the connector and account, never
a field value. `authorize` guards reads of the inbox, and only reads:
the drive calls it before a visit is declared and at the first poll of
every drive of an open visit whose end is not journaled yet, so a
resume checks again before it reads; the park's waker calls it before
each read. A journaled end needs no authorization, because nothing more
is read: a visit whose end pair is in the journal (`WaitVisit::Ended`,
D4 "An ended visit") and a visit whose end line is in the journal
without its end event (a torn end pair, `poll` step 1) are both finished
from the journal alone, with no `authorize` call and no inbox read, even
when the connector or account changed or lost its approval since the
end was written. The drive and the waker read
only `Inbox::open(w.connector, w.account)` after `authorize` passed, and
`w.account` is the one account the manifest grant names. A refusal fails
the node `inbox_wait_unauthorized: <reason>` with nothing journaled for
the wait; in the waker it reads as "no delivery" (logged once on stderr,
`apb inbox_wait_unauthorized run=<id> node=<id>: <reason>`), so the run
stays parked until its `until`, where the resumed drive fails the node.

### The position

The position of node `n` is the `through_seq` of the latest counted
`inbox_wait_read` of `n` in the run's journal, any visit, start,
progress or end line. `crate::inbox_wait::position(events, node) ->
Option<u64>` (engine, `inbox_wait/mod.rs`) computes it, `None` before
the node's first complete start line; it is the only position state
there is. A visit reads only events with seq above it.

Which lines count, read from the event that follows. `start` writes the
start line and `wait_started` in one `append_pair`, under the journal's
append lock, so nothing can come between them: a complete start line is
directly followed by the node's `wait_started`. `position` walks the
journal in order, keeping whether a visit of `n` is declared (a
`wait_started` of `n` seen with no `node_finished` of `n` after it), and
counts an `inbox_wait_read` of `n` when either:

1. the next event in the journal is a `wait_started` of `n` (a complete
   start line), or
2. a visit of `n` is declared at that line (a progress line or an end
   line of that visit; both are written only inside a declared visit).

Any other line of `n` is a torn start line: the start pair's write was
cut after its first line, so no visit was declared and nothing was
examined against it. `position` ignores it (it is superseded), so it
reads `None` when the torn line was the node's first, and the arm's
next `New` calls `start` again, which reads the account's current head
and writes a new start pair at the end of the journal. A reader tells
the two apart only by the event that follows: `wait_started` of the
node right after the line (a start), or a line inside a declared visit
(a progress or end line), or neither (torn). A torn start line can only
exist where `start` runs, which is only when `position` is `None`; a
later visit of a loop starts after the previous visit's end line and
writes no start line.

1. The first visit of a node in a run starts at the log's head: before
   it declares the visit, the drive reads the account's log and
   journals a start line whose `through_seq` is the seq of the last
   stored event (0 when the log is empty or absent), in one write with
   `wait_started` (D4 "The visit" step 2). Events stored before the
   wait began are below the position, so they are neither examined nor
   reported as lost. Retention never removes the newest stored event
   (`enforce_retention`, `inbox.rs` lines 676 to 686), so the head is a
   real seq and seq never restarts.
2. A later visit (a loop) starts after the previous visit's end line, so
   it never examines an event an earlier visit examined, and it counts
   events stored between the two visits.
3. A resume after any crash recomputes the position from the journal;
   what a lost in-memory scan examined is read again from the position
   (D4 "Crash table").
4. Nothing outside the run holds a position: two runs, two projects or
   two nodes reading one account never interfere, and nothing has to be
   removed when a run ends.

Retention, read on efae5822: `enforce_retention` (`inbox.rs` line 687)
drops an event by age only when it is at or below `min_cursor()` (line
624, the lowest cursor of the account's consumers, 0 when there is
none) and older than 30 days; the size cap (line 720) drops the oldest
events whatever their state. A wait holds no cursor, so it neither
protects events from retention nor lets retention drop them. When the
oldest stored seq is above `position + 1`, retention removed events the
wait never examined: the gap rule of D4 "The visit" step 4 applies,
including at a position of 0 (a log that was empty at the start line).

### The visit

The order of one visit:

1. **Authorize** (D4 "Authorization"): before the visit is declared and
   at the first poll of every drive of an open visit whose end is not
   journaled; a journaled end (a whole or a torn end pair) is finished
   from the journal with no authorization and no read.
2. **Start or position.** A node with no counted `inbox_wait_read` yet
   (none, or only a torn start line, D4 "The position"): read the log's
   head and append, in one write, the start line and `wait_started`.
   Otherwise the position is the latest counted line's `through_seq`.
3. **Read from the position**: the stored events above it, when the log
   changed (or the last read failed) and at most once a second.
4. **Gap line.** When the oldest stored event is above the position plus
   one, the `inbox_gap` line for the missing range must be in the
   journal before anything else of this read happens; when it cannot be
   written, the read stops there.
5. **Match**: examine the events above the position in seq order, stop
   at the first that matches or at the first received after the wait's
   deadline (D4 "The deadline").
6. **End pair, or progress line and park.** A match or the timeout ends
   the visit with one write of the end line and `wait_signalled` or
   `wait_timeout`; the timeout ends a visit only after one final read
   (step 6 of `poll`). With no match and nothing new to read, a progress
   line (when the visit examined past the position) and a park.

`crates/apb-engine/src/inbox_wait/mod.rs`:

```rust
/// What the last read of an account's log left behind. The drive's
/// scan and the park waker's check each keep one.
pub(crate) enum ReadState {
    /// Not read yet, or the last read failed: the next read is due.
    NotRead,
    /// The log was read and does not exist.
    ReadAbsent,
    /// The log was read and carries this stamp.
    Read { len: u64, modified: std::time::SystemTime },
}

/// One visit's in-memory state, owned by the drive loop in a
/// `BTreeMap<String, Scan>` (`inbox_scans`) keyed by node beside
/// `park_retry`, created at the first poll of each drive and dropped
/// when its node finishes or when a patch replaces the definition the
/// drive executes.
pub(crate) struct Scan {
    rendered: Vec<String>,
    /// The last seq examined; starts at the position.
    after: u64,
    examined: u32,
    /// What the last read left behind (`NotRead` before the first read
    /// and after a failed one).
    read_state: ReadState,
    /// When the last ordinary read was attempted, failed ones included.
    last_read_ms: u64,
    /// Whether the last read succeeded, its gap (if any) is journaled,
    /// and it was examined to its end.
    caught_up: bool,
    redactor: Option<crate::run_redact::RunRedactor>,
    /// Whether this drive's first poll ran (its authorize and render).
    first_poll_done: bool,
    /// Whether the visit already logged one failed read on stderr.
    read_error_logged: bool,
}

pub(crate) enum Poll {
    /// Not over; the drive sleeps a tick, or parks when it may.
    Open,
    /// Over; the end pair is journaled; the node succeeds with `output`.
    Ended { output: String },
    /// Over with no match at the timeout; the end pair is journaled; the
    /// node times out.
    TimedOut,
    /// Refused (authorization, unreadable head, unscrubbable output);
    /// nothing was journaled for the wait; the node fails with `reason`.
    Failed { reason: String },
}

/// Step 2 for a node with no position: authorize, read the head, append
/// the start pair. The wait arm calls it instead of appending
/// `wait_started` alone.
pub(crate) fn start(cx: &mut WaitCx<'_>, w: &InboxWait) -> Result<Result<(), String>, EngineError>;

pub(crate) fn poll(
    cx: &mut WaitCx<'_>,  // root, run_dir, node, &mut EventLog, &RunState
    w: &InboxWait,
    scan: &mut Scan,
    elapsed_ms: u128,
    timeout_ms: u128,
) -> Result<Poll, EngineError>;
```

`start`, step by step (the wait arm's `New`, after `node_started`):

1. `authorize`; a refusal returns `Err(reason)` and the arm fails the
   node with nothing journaled for the wait.
2. `Inbox::open(connector, account)?.read_events()` (`inbox.rs` lines
   222 and 590); `head` is the last event's seq, 0 when there is none
   (a missing log reads as empty). A read error is retried within the
   engine's existing bounded infrastructure retries:
   `failure_class::backoff_schedule()` (`failure_class.rs` line 411; by
   default `DEFAULT_BACKOFF_MS`, line 396, a wait of 5 s and then 30 s,
   so at most three reads over about 35 s; `APB_INFRA_BACKOFF_MS`
   overrides it as for attempts), each wait slept through `wait_backoff`
   (line 446) on the drive's cancel flag, so a stop or pause ends it
   within a tick. Only when the last read of the schedule fails does
   `start` return `Err("inbox_wait_unreadable: ...")`, nothing journaled.
   No visit is declared before the head is read, so no timeout bounds
   these reads; the schedule does.
3. `EventLog::append_pair` (`event.rs` line 2949) of the start line
   (`inbox_wait_read { through_seq: head, examined: 0 }`, no `end`, no
   `output`) and `wait_started` (`kind` `"inbox"`), in that order, so a
   write cut after the start line leaves no declared visit (D4 "Crash
   table" row 2).

`poll`, step by step:

1. **A decided end.** When the open visit holds an `inbox_wait_read` of
   this node with `end` set after its latest `wait_started` and no end
   event after that (a write of the end pair cut after its first line,
   D4 "Crash table" row 8), append the end alone (`wait_signalled` for
   `end: matched`, `wait_timeout` for `end: timeout`) and return `Ended`
   with that line's `output`, or `TimedOut`. The repair is made from the
   journal alone: `authorize` is not called and the inbox is not read
   (D4 "Authorization").
2. **First poll of this drive.** `authorize` (a refusal returns
   `Failed`). Render every `equals` through
   `crate::judge_eval::inputs::JudgeInputs::from_state` and its
   `render`, the one template render every prompt and judge state goes
   through, so every reference V13 accepts in a condition (a review
   decision, `run.context`, `run.previous_success` included) renders as
   it would in a prompt; set `after = position(events, node)`. A first
   poll also follows a patch: whenever a patch replaces the definition
   the drive executes, every path that applies one (the control scan,
   a supervisor's directive, a patch applied while the supervisor
   handles a failed batch, `park_for_batch_failures` with
   `BatchWake::Resumed`) reports it through one value, the "definition
   replaced" result of the patch application
   (`scheduler/patch.rs` `Definition::Replaced`), never a comparison of
   version strings, and the drive clears
   `inbox_scans` in the one place that receives it. The next poll of an
   open inbox wait then authorizes the patched wait, renders its
   conditions, reads the position from the journal and builds a new
   redactor and read state, so a patch that changes the account or a
   condition never reads with the old authorization or the old
   rendered values.
3. **Read.** Skip the read (return to step 6 with nothing new) unless
   this is the first poll, or a read is due and at least
   `MIN_READ_INTERVAL_MS` passed since `last_read_ms`. A read is due
   when `read_state` is `NotRead` or the current `Inbox::log_stamp`
   differs from the one held (absent against present included).
   `Inbox::log_stamp(&self) -> Option<(u64, SystemTime)>` (new, core
   `inbox.rs`) is the length and modification time of `events.jsonl`
   from one `metadata` call, `None` when the file does not exist; any
   append changes it. Otherwise take the stamp, then read
   `read_events()`. `last_read_ms` records every ordinary read attempt,
   failed ones included, so a log that stays unreadable is tried once
   per `MIN_READ_INTERVAL_MS`, not on every poll. A successful read
   sets `read_state` from the stamp (`Read`, or `ReadAbsent` for a log
   that does not exist, cached like any other read). A read error is
   never cached: it sets `read_state = NotRead` and `caught_up = false`
   and ends the step (logged once per visit on stderr, `apb
   inbox_wait_read_error node=<id>: <error>`). So the next poll past
   `MIN_READ_INTERVAL_MS` reads again whether or not the file changed,
   and an error that clears without a new write (a transient I/O error,
   a line read while another process was writing it) is retried; the
   wait does not park meanwhile (`caught_up` is false), and the timeout
   still applies.
4. **Gap.** When `apb_core::connector::inbox::lost_prefix(after,
   first_seq)` (line 922, made `pub`) gives a range, journal `inbox_gap`
   for it through `crate::connector::inbox::journal_gap_in(cx.log,
   connector, account, node, &gap, GapScope::Node)`, the existing
   once-per-gap rule of `journal_gap` (`crates/apb-engine/src/connector/inbox.rs`
   line 404) split so it takes the drive's open log (`journal_gap` keeps
   its signature and calls `journal_gap_in` over
   `EventLog::open_foreign` with `GapScope::Account`, so the existing
   connector-read rule is unchanged). The scope decides which earlier
   `inbox_gap` line already records the gap: `GapScope::Account` matches
   on connector, account and range, as main does (line 425);
   `GapScope::Node` also requires the line's `node_id` to be this wait
   node's. So a range another node already recorded (a connector read of
   the same account, another wait) is still journaled for the wait
   before its position advances, and a wait's own gap is written once,
   a resume included. The `node_id` it journals is the wait
   node's own id (`a`), never the site id: every outcome surface reads
   that field as the node that found the gap (`RunInboxGap`,
   `run_outcome.rs` line 59, and `inbox_gap_lines`, line 327), and the
   site id `inbox_site(node)` serves only `authorize`. `journal_gap_in` returns whether the gap
   is in the journal (written now or already there). On `false` the
   read ends at once: step 5 does not run, `after` and `examined` keep
   their values, `caught_up = false` (so the wait does not park), and
   `read_state` returns to `NotRead`, so the next poll past
   `MIN_READ_INTERVAL_MS` reads again whether or not the log changed and
   tries the gap line again. No progress line and no end line is
   written with a `through_seq` above the position the scan held before
   that read: a timeout that falls due in that state ends with
   `through_seq` at that position, so the position never moves past a
   loss the journal does not hold. Nothing later in the same poll sets
   `caught_up` back to true. Chosen over failing the node: a journal
   append that failed once is not a reason to lose the wait, and the
   timeout already bounds the retries.
5. **Match.** Examine every event with `seq > after`, in order. At the
   first event that is not in time (D4 "The deadline") stop: it is not
   examined, `after` stays below it, and the step goes to step 6, where
   the wait is necessarily over (that event was received after the
   deadline and before now). Otherwise, when `matches` is false,
   `examined += 1` and `after = e.seq`; at the first
   that matches, build the output object (`apb_core::inbox_wait::output`),
   pass it through the storage redactor (`scan.redactor`, built once per
   drive with `RunRedactor::for_storing(root, run_dir)`, **on main since
   eval-doubles**, and its `secret_json`), and go to step
   7 with `through_seq = e.seq`. When `secret_json` refuses
   (`Err(SecretsUnresolved)`), return `Failed { reason:
   "inbox_wait_secrets_unresolved: the run's secrets could not be
   resolved, so the event's values could not be stored scrubbed" }`:
   nothing is journaled for the wait. With no match to the end of the
   read (no late event met), `caught_up = true`.
6. **Timeout.** With no match and `elapsed_ms >= timeout_ms` (the
   journal's active wait time, `scheduler::journal::wait_active_ms`,
   line 246, the clock every wait uses), the timeout never ends a visit
   on a stale read: a read is skipped while fewer than
   `MIN_READ_INTERVAL_MS` passed since the last one, so an event stored
   in time inside that interval before the deadline would otherwise
   never be examined.
   1. Unless a read of this same poll already succeeded, the poll makes
      one final read that ignores `MIN_READ_INTERVAL_MS` and the read
      state. It is a settled read (`Inbox::read_events_settled`, at most
      `FINAL_READ_SETTLE`, 5 s): it first waits while an append holds the
      store's lock, looking at the lock only (`fsutil::lock_held`, no lock
      taken, nothing written to the store), so a delivery stamped before
      the deadline whose line is still being written is examined, not
      left for the next visit; past the bound it reads anyway. Steps 4 and
      5 run over it as over any other read, with the same `in_time` rule,
      so a late event still stays above the position, and a match ends
      the visit `matched`.
   2. When that final read fails, it is tried once more at once. When
      it fails again, the visit ends `timeout` with `through_seq` at the
      position held before the read: nothing is lost, the events stay
      above the position for the next visit, and the timeout stays a
      bound. The failure is logged on stderr under the once per visit
      rule.
   3. When the final read finds a gap that cannot be journaled, step 4
      applies: the end line keeps `through_seq` at the position held
      before the read.
   With no match after the final read, go to step 7 with no output and
   `through_seq = after`, which is below the first late event, so that
   event stays above the position for the next visit (D4 "The position"
   item 2). The park waker needs nothing for this: it only resumes the
   run, and the resumed drive's first poll always reads.
7. **End.** Append in one `EventLog::append_pair` the end line
   (`inbox_wait_read { through_seq, examined, end, output }`, `end`
   `matched` with the output's compact JSON, or `timeout` with none) and
   the end event (`wait_signalled` or `wait_timeout`). Return `Ended {
   output }` or `TimedOut`. The drive then appends `node_finished`
   exactly as for a timer or webhook wait.

```rust
    /// An inbox wait's place in its account's log (D4). A start line
    /// (written with `wait_started`) and a progress line (written right
    /// before a park) carry no `end`; an end line (written with
    /// `wait_signalled` or `wait_timeout`) carries `end` and, when it
    /// matched, the output: the event's seq, receive time, type and the
    /// declared values, scrubbed. Never an event body. Not a checkpoint.
    InboxWaitRead {
        node: String,
        #[serde(default)]
        through_seq: u64,
        #[serde(default)]
        examined: u32,
        /// `matched` or `timeout`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
```

The inbox module doc (`inbox.rs` lines 25 to 27, "no caller may put one
in a run's event log") stays true: no body reaches the journal. It gains
one sentence: "an inbox wait journals the values of the fields its
playbook declares, scrubbed, never a body".

What the redactor guarantees, stated plainly: the storage redactor
scrubs the run's known secret values and credential-shaped text; a known
secret shorter than `MIN_SECRET_LEN` (6, `scrub.rs` lines 45 and 133) is
not scrubbed by value, and a credential apb does not know and that has
no token shape is not recognized. A declared field can therefore still
hold a secret apb does not know; the author chooses which fields a wait
takes, and the docs say so.

Why a failed scrub fails at once and is not retried: an unresolved
secret (a command-sourced field, a variable this process cannot see)
does not resolve by waiting, and a retry count would be state the
journal does not hold. The node's own failure policy (a retry, a
failure edge) decides what happens next.

### The deadline

Receipt decides, not polling. An event can match only when it was
stored before the wait's deadline: the wall instant at which the run's
paused-interval clock, measured from the visit's `wait_started`, reaches
the wait's `timeout_seconds`. A drive or a waker that polls late (a slow
tick, a waker that ran after the machine slept, a resume after a
person's pause) therefore neither loses an event that arrived in time
nor accepts one that arrived after.

The rule uses the function that already gives a wait its due time on
that clock, `scheduler::wait_park::due_at_ms` (`wait_park.rs` line 27:
`now` plus the time left over `journal::wait_active_ms`, the paused-
interval clock with the park exemption), evaluated as of the event's
receipt instead of as of the poll:

```rust
/// Whether stored event `e` arrived before the deadline of the current
/// visit of `node` (D4 "The deadline"). Pure over the journal.
pub(crate) fn in_time(events: &[Event], node: &str, timeout_seconds: u64, received_at: u64) -> bool {
    wait_active_ms(events, node, u128::from(received_at))
        .is_some_and(|active| active < u128::from(timeout_seconds) * 1000)
}
```

(`crate::inbox_wait::in_time`, engine, `inbox_wait/mod.rs`.) The
deadline instant lies after `r = received_at` exactly when the wait's
active time at `r` is below the timeout, so the comparison is made
there, in `u128`, and never computes a deadline instant: a timeout whose
deadline would not fit a `u64` instant still takes an event received in
time. A pause before `r` (a person's pause, an
engine pause) is not counted, as for every wait timeout; a park is
counted, as the park exemption of `wait_active_ms` already says. The
millisecond boundary is main's: at the deadline instant itself the
wait's active time equals its timeout, which main's own check
(`elapsed_ms >= timeout_ms`) already reads as over, so an event stored
in that millisecond is late. The wait never reads a time from the event
body.

**Receipt** is the moment the store assigns the event its seq, under
the store's lock, and `received_at` (`InboxEvent.received_at`, epoch
milliseconds, `inbox.rs` line 81) is stamped there. On main it is not:
the production entries `append_with` (line 285) and `append_typed`
(line 306, the one the webhook listener calls, `apb-server/src/ingest.rs`
line 789) read `crate::clock::now_ms_u64()` as an argument, before
`append_bounded` takes the lock (`lock_dir`, line 344), and the seq is
assigned after it (line 357). A delivery stamped before the deadline
could then wait on the lock and be stored after it, and two concurrent
deliveries could take seqs in the opposite order of their stamps. This
change moves the production stamp under the lock (core
`connector/inbox.rs`):

1. A private `append_at(.., clock: impl FnOnce() -> u64)` holds today's
   body of `append_bounded`. It calls `clock()` once, right after
   `lock_dir` returned and after reading the last stored event (the
   read `last_seq` already makes, line 650), and stamps
   `received_at = max(clock(), last.received_at)`; the dedupe check and
   the dedupe entry keep the clock's own value, as on main.
2. `append_with` and `append_typed` call `append_at` with
   `crate::clock::now_ms_u64` (the function, not its value), so the
   clock is read under the lock. `append_bounded` keeps its signature
   and its explicit `now` as the test seam, calling `append_at` with
   `move || now`; its existing tests are unchanged.

So seq and `received_at` are monotonic together: a later seq never has
an earlier stamp, and the first late event in seq order is followed
only by late events, which is what lets `poll` step 5 stop there.

What this means for `start-rules-and-schedules`, which reads the same
store: nothing. It does not use the time to decide anything: its rules
match on seq, type and body, and `received_at` reaches it only as the
template value `{{event.received_at}}` (its design, `trigger/render.rs`
`render`, rendered as RFC 3339), which it renders and never compares
(its coalescing window, the one reader of the time, was cut there). A
start key that renders it stays stable, because each event is stamped
once, when it is stored. The `inbox_read` surfaces show the stamp as before.

Both readers apply it the same way: the drive in `poll` step 5 (the
first late event ends the examination, in seq order, so the decision
never reorders the log), and the waker in `delivery_pending` (D4
"Parking" item 6, a late event never resumes the run for a match; the
waker resumes at `until` in any case). Before the deadline every stored
event is in time (each was stored at or before now), so the rule only
ever decides for a poll that comes after the deadline.

### The wait arm

`crates/apb-engine/src/scheduler.rs`, the wait branch (lines 2647 to
2754):

1. The visit guard (lines 2654 to 2667) keeps main's comparison of the
   two counts for a timer or webhook wait, unchanged, crash behaviour
   included (D4 "An ended visit"); for an inbox wait it reads
   `journal::wait_visit` instead. A timer or webhook `New` appends
   `node_started` and `wait_started` as two appends exactly as on main
   (the `kind` match gains `WaitFor::Inbox(_) => "inbox"`); an inbox
   `New` appends `node_started`, then calls `inbox_wait::start` when
   `position` is `None` (no counted line yet, a torn start line
   included; a refusal fails the node), and appends `wait_started` alone
   otherwise, after `authorize` passed. An inbox `Ended(end)` goes to the
   shared tail with no authorization (D4 "Authorization").
2. The `signalled` match (lines 2677 to 2692) gains `WaitFor::Inbox(_)
   => false`. Before it, an inbox wait that is not scripted calls
   `inbox_wait::poll`: `Ended { output }` yields `(NodeStatus::Succeeded,
   output)`; `TimedOut` yields `(NodeStatus::TimedOut, "wait timeout")`
   without appending `wait_timeout` again (step 7 wrote it); `Failed {
   reason }` yields `(NodeStatus::Failed, reason)`; `Open` falls through
   to the park block, else sleeps `AWAIT_CONTROL_POLL` and continues, as
   the other waits do.
3. A scripted inbox wait (`scripted_wait`, line 703) reads nothing and
   authorizes nothing: `Ended` succeeds with output `"{}"` (and the
   `wait_signalled` main appends), `TimedOut` times out as main does.
   For a timer or webhook wait `scripted_wait` is called where main
   calls it, after the visit's declaration, so a scenario with a missing
   answer under `on_missing: error` journals `node_started` and
   `wait_started` before it stops, exactly as main does. An inbox wait
   needs its scripted answer before its declaration (a scripted inbox
   wait must not run `start`, which authorizes and reads the head): it
   looks the answer up there without the `on_missing` effect, and runs
   `on_missing` after the declaration, so its journal on a missing
   answer has the same order as a timer wait's.
4. The park block (lines 2706 to 2752): D4 "Parking".

### An ended visit

On main the wait arm declares a visit when `wait_started_count <=
wait_ended_count` (`scheduler.rs` line 2655, `scheduler/journal.rs`
lines 195 to 210). A crash after the end event and before the shared
tail's `node_finished` leaves the counts equal: the resume declares a
new visit and waits again. For an inbox wait that would lose the
journaled event (the new visit starts after its `through_seq`). A visit
that ends with no end event (an inbox `Failed`) would leave the started
count above the ended count for ever. One read-side rule fixes both, for
inbox waits only, in `crates/apb-engine/src/scheduler/journal.rs`.

Timer and webhook waits keep main's count comparison exactly: after a
crash between their end and `node_finished` the resume appends a second
`node_started` and `wait_started` and waits again, as main does. That
gap is pre-existing on main and stays outside this change (Review record
"Fourth pass" item 1): changing it would change the journal of runs without an
inbox wait, which this change promises to leave as main writes it (D0
"What a run that uses no inbox wait writes"). The arm asks `wait_visit`
only for a node whose `wait_for` is `Inbox`:

```rust
pub(crate) enum WaitVisit<'a> {
    /// No `wait_started`, or the latest is followed by a `node_finished`
    /// of the node: declare a new visit.
    New,
    /// The latest `wait_started` has no end and no `node_finished` after it.
    Open,
    /// It has an end (`wait_signalled`, `wait_timeout`) after it and no
    /// `node_finished`: finish the node from that end.
    Ended(&'a Event),
}
pub(crate) fn wait_visit<'a>(events: &'a [Event], node: &str) -> WaitVisit<'a>;
```

`Ended(end)` declares nothing, journals nothing for the wait, calls no
`authorize` and reads no inbox, and yields straight to the shared tail:
a `wait_signalled` gives `(Succeeded, output)` with `output` the latest
`inbox_wait_read.output` of the node after that `wait_started` (the pair
wrote it in the same write), or `{}` when no read line carries one
(`inbox_wait::signalled_output`): only a scripted wait of a scenario run
journals no read line, and `{}` is what its uninterrupted run outputs; a
`wait_timeout` gives `(TimedOut, "wait timeout")`. The counters stay for their other readers and for the timer
and webhook guard; `wait_signalled_count` keeps deciding the webhook
signal. The rule reads positions every apb already writes; nothing a
timer or webhook wait writes changes, in any state.

### Parking

A long inbox wait parks through the merged wait park with the timer
park's own reason form. What changes is listed here; everything else is
"Unchanged from main" item 6.

1. **The check.** `ParkCheck.timer: bool` (`scheduler/wait_park.rs`
   line 75) becomes `parks: bool`, "the wait's kind may park now": true
   for a timer wait, true for an inbox wait whose scan is `caught_up`
   (its last read succeeded, its gap is journaled, no event matched) and
   whose read state still matches the current `Inbox::log_stamp`
   (nothing stored since its last read), false otherwise (a webhook wait, an
   inbox wait whose last read failed or whose gap line is not written).
   `park_request` reads `check.parks` where it read `check.timer`;
   nothing else in it changes. With no match the only end left is the
   timeout, so the wake time is a pure function of the journal.
2. **The wake time.** `pub(crate) fn park_seconds(wait_for: &WaitFor,
   timeout_seconds: u64) -> u64` (new, `wait_park.rs`): the timer's
   seconds, the inbox wait's `timeout_seconds`, 0 for a webhook wait.
   The wait arm calls it where it matches `timer_seconds` today (line
   2713), so `due_at_ms(&events, &current, park_seconds(..),
   timeout_seconds, now)` is the inbox wait's timeout due time; the
   control scan's re-decision calls it in `control_apply.rs`, whose
   `wait_timer_of` (line 38) becomes `park_wait_of(playbook, node) ->
   Option<(u64, u64)>`: `Some((park_seconds(..), timeout))` for a timer
   or inbox wait, `None` for a webhook wait. So the scan recomputes an
   inbox park's `until` with the same function over the same journal.
3. **The progress line.** Right before it posts a park (after
   `park_request` returned a request), an inbox wait whose `scan.after`
   is above `position(events, node)` appends one `inbox_wait_read` with
   no `end` and no `output`, `through_seq = scan.after` and the visit's
   `examined`. Then the arm posts the request with
   `control::post_control_unless_busy` and records
   `park_retry.posted(events.len() + appended, seq)` (`appended` 1 when
   the progress line was written, else 0), so `ParkRetry` compares the
   journal it will read next. The line is written only after the read's
   gap line is in the journal (item 1) and only over events that did
   not match, so it never moves the position past a match or an
   unreported loss. It makes the position equal to what the scan
   examined, so the waker and a resumed drive start after those events.
4. **The reason.** `wait (<node>)` (`cause::wait_reason`), the timer
   park's own form. Every reader keyed on `PauseCause::Wait`,
   `is_park_event` or `is_park` treats the inbox park as a park with no
   change. The surfaces word the park without naming the kind (D4
   "Surfaces").
5. **The guard.** `guarded_features` names `wait_park` for a `wait` node
   whose `wait_for` is `Timer` or `Inbox` (`guarded.rs` line 156); the
   park keeps requiring it.
6. **The wake.** The waker of a park (`pause/waker.rs` `run_waker`, the
   block under `pause_at_is_wait`, line 160) also asks, at each of its
   polls, `crate::inbox_wait::pending::delivery_pending(&root, &run_dir,
   &events, seq, &mut inbox_state)`; true resumes the run once through
   the same `resume_while(root, run_id, WakeHold::Pause(seq))` the
   pending-control branch uses. `delivery_pending`:
   1. takes the node from the pause's reason (`cause::parse_wait_reason`
      of the `run_paused` at `seq`) and the node's `WaitFor` from
      `legacy_snapshot::load_run_playbook(run_dir)` (line 55); anything
      but an inbox wait gives false (a timer park is untouched);
   2. calls `authorize` (a refusal gives false, logged once);
   3. reads `position(events, node)`;
   4. at its first call, after a failed read, and otherwise only when
      `Inbox::log_stamp` changed since its previous successful read
      (`inbox_state` is a `ReadState`, set from the stamp only after a
      read succeeded and returned to `NotRead` by a failed one, so an
      error is never cached and the next poll reads again whether or not
      the file changed), reads
      `read_events()` and returns true when an event has `seq` above the
      position, `event_filter::type_passes(&w.types, e.event_type)` and `in_time` (D4
      "The deadline", over the same journal), or when the
      oldest stored seq is above the position plus one (a gap the
      resumed drive must journal); a read error gives false for this
      poll, logged once, as the control check does.
   The first call always reads, so an event stored between the drive's
   last read and the waker's start is found. The poll is the waker's
   (`pause::waker::poll()`, line 50, 5 s by default,
   `APB_WAKER_POLL_MS`), so a delivery resumes a parked inbox wait
   within one poll. The waker interprets nothing beyond a seq and a
   type: the resumed drive authorizes, reads, decides, and parks again
   when nothing matches. `inbox_wait::pending` depends on
   `apb_core::connector::inbox`, the event types, `legacy_snapshot` and
   `inbox_wait::authorize`; its one read of `pause` is
   `pause::cause::parse_wait_reason`, a leaf that depends on nothing of
   the waker, so the module graph keeps no cycle.
7. **Churn, a stated limit.** Every stored event of a listed type
   resumes a parked visit once; when its conditions do not match, the
   visit writes a progress line and parks again (a `run_paused` and a
   `run_resumed` each time). The docs say to set `types` on a busy
   account.

### Evals and scenarios

1. `apb_core::eval::refusal` (`crates/apb-core/src/eval.rs` line 466)
   gains, in its node loop beside the sub-playbook rule, "node `<id>`
   waits for inbox events, which evals do not run". On main the new
   site alone already makes `refusal` refuse the playbook (its
   connector-site loop, line 471); the node rule is still needed
   because eval-doubles moves that loop into `case_refusal`, where a
   case with doubles is judged by `coverage_gaps` and a site that grants
   no function would leave nothing to double. So an eval case never runs
   an inbox wait: it is refused before anything is created
   (`eval_refused_effects`, exit 2) and `apb validate` reports it as
   V81, as for every reason `refusal` gives (`docs/EVALS.md` lines 29 to
   39).
2. A scenario run scripts the wait as today: `signal` or `elapsed` ends
   it with `{}`, `timed_out` times it out (the wait arm item 3); no
   inbox is read.
3. `apb scenario capture` of a run with an inbox wait: unchanged code.
   A timed-out visit is captured as a `timed_out` answer, a visit that
   ended with an event is the existing `wait_signal` gap (its event is
   not in the scenario format). `docs/EVALS.md` says so in the
   `wait_signal` row (line 937).

### Untrusted text

`crates/apb-core/src/untrusted.rs`: an inbox wait is an entry point of
untrusted text whatever else its definition says (inbox content is
written by whoever can reach the callback URL, `docs/CONNECTORS.md`
lines 458 and 459): inside `analyze`'s fixed-point loop, beside rule 2
(a connector binding that returns outside text, line 150), a `Wait`
node with `WaitFor::Inbox` inserts `Source::Connector { node, connector
}` into its own `sources`, and `carries_text` (line 88) is true for such
a node, so a node that renders `nodes.<wait>.output` is a follower and
V179 to V182 apply. The source is inserted in the loop, not seeded into
`tainted` before it: the loop takes another pass only when a node's
source count grows (`changed`), so a seed present from the start would
never grow (`changed` is set at line 213, the loop ends at line 224),
and a follower placed before the wait in node order, which
reads an empty output map on the first pass, would be missed. Inserted
in the loop, the wait's count grows on the first pass, `output` gains
the wait, and the next pass reaches every reader whatever its place in
node order. The analysis reads `NodeKind::connector_bindings`, not the
sites, so the site's `untrusted: true` alone would not reach it.

### Validator: V284 to V287

`check_inbox_waits(playbook, ctx, r)`, for each `Wait` node with
`WaitFor::Inbox`, message prefix `inbox wait of node <id>:`:

1. V284 (error): `timeout_seconds` is 0; a condition's `path` is empty
   or has an empty segment; `types` or the conditions break the shape
   rules of an inbox read's filter, checked by the same code
   (`apb_core::connector::event_filter`: at most `MAX_TYPES` types, at
   most `MAX_CONDITIONS` conditions, and every type cleans to itself,
   `webhook::clean_event_type`, so an empty type is refused too; exactly
   the cap is accepted, one above it refused); `take` has more
   than `MAX_TAKE` entries; a `take` name is not a snake name
   (`crate::connector::validate_snake_name`), repeats another, or is one
   of `RESERVED_NAMES`; a `take` path that
   `crate::baseline::extract::parse_path` refuses or that has a `[]`
   segment; a `take` type other than `field` or `number`. An absent
   `timeout_seconds` never reaches the validator: the field is a
   required `u64` (`schema.rs` line 2168), so `Playbook::from_yaml`
   refuses it (a parser test pins it). The connector and account names
   are checked at the site by the existing V23 and V25
   (`validate/connectors.rs` lines 46 to 70, `validate_profile_name`,
   so `echo-hooks` passes).
2. V285 (error), when `ctx.connectors` is not empty (the caller has a
   connector store): the connector is not in it ("is not installed"),
   or its `ConnectorFacts.has_webhook` (`connector/resolve.rs` line 418)
   is false. With an empty map (no store) V285 stays silent, as V42
   does (`validate/mod.rs` lines 286 to 292), and the run gate refuses
   the start (`connector_unresolved`, D4 "Authorization" item 1).
   V43 (`validate/connectors.rs` `check_inbox_binding`) also checks the
   account an inbox wait reads, though the site grants no function: an
   account that does not define a field the webhook block verifies
   deliveries with is refused ("reads the inbox of connector ..."), as
   for a binding that grants an inbox function, since no delivery to it
   could be verified. A connector without a webhook block stays V285's
   to report there, not V42's.
3. V286 (warning): `types` is set and the connector's new
   `ConnectorFacts.names_event_type` is false. The field (`bool`; the
   struct derives `Default`, line 414, so no literal elsewhere changes)
   is filled in `of_manifest` beside `has_webhook` (line 474) from
   `doc.webhook.as_ref().is_some_and(|w| w.names_event_type())`.

4. V287 (error), `check_site_ids(playbook, r)` over the whole definition
   (not only inbox waits): two binding sites with one id, as D4
   "Authorization" sets out ("Site ids are unique"). The sites are
   collected where every binding site is, `Playbook::connector_sites`
   (`connector/sites.rs` line 44: the nodes, then `check_site` of every
   list node with a connector check, then the engine sites), plus the
   derived ids of every list node and every inbox wait whether or not
   their site binds. The message names the node and the derived site's
   owner. A new code rather than V01, because V01 means two nodes with
   one id, and a site collision is a node colliding with a site.

The conditions' `equals` texts join `crate::schema::template_texts`
(`schema.rs` line 2652) so V13 resolves their references.

### Surfaces

1. The node's output is the matching event's object; every surface that
   shows a node's output shows it, and a gap shows as every `inbox_gap`
   does. No new view type, field or generated TypeScript. The journal
   page shows `inbox_wait_read` as its type with no note ("Unchanged
   from main" item 10).
2. The park wording, one text for both kinds, since the reason does not
   tell them apart:
   1. `pause/display.rs` `PauseView::line` (line 50): ``waiting at
      `<node>` until <T> at the latest; a waker resumes the run``
      (without `until`: ``waiting at `<node>`; a waker resumes the
      run``), with `, lock kept` as today.
   2. `next_action/verdict.rs` (lines 647 and 650): ``waiting at
      `<node>` until <T> at the latest; a waker resumes it (a resume
      before then parks it again unless the wait can end)`` and ``the wait at `<node>` was
      due at <T> and no waker resumed it; resume it``. The action text
      (line 566) becomes a match: an inbox wait reads ``node `<node>`
      waits for an inbox event of `<connector>` ``, the other two as
      today.
   3. `apb-cli/src/pause.rs` (line 85): ``the wait at `<node>` is due at
      <T> at the latest; unless the wait can end now, the run waits
      again until then``. An early resume of an inbox wait whose matching
      event is already stored ends the wait, so neither line says that a
      resume only parks again.
   4. MCP `run_wait` (`apb-mcp/src/tools/run.rs` line 829): "the run is
      parked at a wait (see pause); no driver runs, its waker resumes it
      at pause.until at the latest, an inbox wait as soon as an event
      arrives. Call run_wait again; run_stop ends the run".
   5. `web/src/lib/pause.ts`: the cause line for `wait` reads "on a
      wait" (line 31) and `parkLine` uses the wording of item 2.1 (lines
      52 and 53).
   "At the latest" is true for a timer park too: its waker also resumes
   it at once on a pending control entry (wait-park D7).
3. Next actions: `next_action/deadlines.rs` `wait_node` (lines 85 to 90,
   exhaustive): `WaitFor::Inbox(_)` joins the `Timer { .. } | Webhook {
   .. }` arm (`AutoOutcome::WaitTimesOut`).
   `next_action/unattended.rs` line 113 already reads every non-timer
   wait as `WaitTimesOut`; unchanged.

### Docs

1. `docs/HOWTO-authoring.md` `## Connectors (external services)` (line
   512): a subsection `### Waiting for an inbox event (wait_for: inbox)`:
   the block (`connector`, `account`, `match`, `types`, `take`, the
   node's `timeout_seconds`); that the connector and account go through
   the same trust as any binding (installed, approved, the account's
   secrets set) and that the wait calls no function; that the wait ends
   at its first matching event or at the timeout; the output (`seq`,
   `received_at`, `type` when the event has one, and the `take` values,
   read as `nodes.<wait>.output.<name>`; a taken value is `null` when
   the path does not resolve, the text is over 256 characters or holds
   a hidden character, the value is not a scalar, or a number is not
   finite; `take.type` may be `number`); that `equals` is exact text
   equality with the rendered template (`"42"` matches the number 42),
   not the JSON equality of an inbox read's filter; that no
   event body is stored, and the stated limit that a declared field can
   still hold a secret apb does not know (a known secret shorter than 6
   characters included), so the author chooses fields; the position
   rule (events stored after the wait first started, then after what
   the previous visit examined; nothing is acknowledged, no consumer);
   retention (a wait does not hold events; what it lost shows as an
   inbox gap); that the output is untrusted; that collecting several
   events in one wait is future work and a loop around the wait already
   expresses it, with the example of answering each review comment
   until the pull request closes (the wait takes `action` and the
   comment fields, an agent answers, an `output_field` edge on an exact
   value of `action` leaves the loop, never a substring test); the
   customer reply example; the park (a long wait
   with nothing matched parks; a stored event of a listed type resumes
   it within the waker's poll; set `types` on a busy account); that a
   visit fails `inbox_wait_unauthorized` when the connector or account
   changed or lost its approval, and `inbox_wait_secrets_unresolved`
   when a secret the workspace's connectors reference cannot be
   resolved; that eval cases refuse an inbox wait; that an event counts
   only when it was stored before the wait's timeout on the run's clock,
   however late the run looks; and V284 to V287.
2. `docs/HOWTO-authoring.md` `## wait (a timer or a signal)` (line 4320)
   becomes "a timer, a signal or an inbox event"; the park paragraph
   (lines 4337 to 4343) names the inbox wait and the neutral park line.
3. `docs/CONNECTORS.md` lines 454 to 456: "A playbook consumes the inbox
   by polling it" gains the inbox wait as the way that spends no agent
   turn, keeps its position in its run's journal and acknowledges
   nothing; the binding site `<node>#inbox` in the list of sites; the
   module-doc sentence on declared values.
4. `docs/GUARDRAILS.md` line 278: `wait_park` is named for "a timer or
   inbox wait".
5. `docs/EVALS.md`: the refusal list (lines 29 to 35) names "a wait
   node that waits for an inbox event"; the `wait_signal` row (line
   937) adds "or an inbox wait that ended with an event".

### Crash table

Journal of one inbox visit: `node_started`; on the node's first visit
the start pair (`inbox_wait_read` start line, `wait_started`), else
`wait_started`; then per read possibly `inbox_gap`, possibly a progress
`inbox_wait_read` and a park (`run_paused`, later `run_resumed`); then
the end pair (`inbox_wait_read` with `end` and its `wait_signalled` or
`wait_timeout`) and `node_finished`. A wait changes nothing outside its
run directory, so every recovery is a re-read from the journal.

| # | Crash after | Recovery |
|---|---|---|
| 1 | `node_started`, before the start pair | `wait_visit` gives `New`; a second `node_started`, as main does for every wait; the head is read again (the wait never started, so nothing was missed) |
| 2 | the start pair cut after its first line (a start line, no `wait_started`) | `New`; the torn line is superseded: no `wait_started` of the node follows it and no visit is declared at it, so `position` does not count it and reads `None`; the next `start` reads the current head and writes a new start pair at the end of the journal, which becomes the position; no visit was declared, so nothing was missed |
| 3 | `wait_started` (reads, gaps journaled or not) | `Open`; a fresh scan from `position`, the same events examined again; a gap already journaled is not journaled twice (the once-per-gap rule); a gap retention opened meanwhile is journaled before anything is examined |
| 4 | a gap append that failed | nothing examined, nothing else written; the next poll tries again; a timeout ends the visit with `through_seq` at the position |
| 5 | a progress line, before the park was posted | `Open` on resume; the position covers only events that did not match; the drive may park again |
| 6 | the park posted or applied | the wait-park crash table, unchanged; the waker's first poll authorizes and reads the log |
| 7 | a store read failed | nothing journaled; the next poll reads again; the timeout bounds it |
| 8 | the end pair cut after its first line (`inbox_wait_read` with `end`, no end event) | step 1 appends the end alone from the journal (no `authorize`, no read) and finishes with the line's output |
| 9 | the end pair cut inside its second line | a torn last line, which every strict read refuses exactly as for any torn append on main |
| 10 | the end pair, before `node_finished` | `Ended(end)`: `node_finished` from the pair, no new visit, no `authorize`, no read (test barrier `Hook::WaitBeforeNodeFinished`) |
| 11 | a `Failed` poll, before its `node_finished` | `Open`; the next drive refuses again at the same check, nothing journaled for the wait |
| 12 | the waker resumed the run and the resume is refused | wait-park D7: the run stays parked until its `until`, a person or `apb resume --all` |
| 13 | the machine rebooted while parked | `apb resume --all` resumes the park as any park; the new drive authorizes and reads from the position |

Retention between a crash and the resume needs no row of its own: the
resumed drive reads from the journaled position, and whatever retention
removed above it is a gap, journaled before anything is examined (row
3).

Stated limits: a crash repeats the reads of the lost scan (cost only);
every stored event of a listed type wakes a parked visit once. A timer
or webhook wait that crashes between its end and `node_finished` waits
again on resume, as on main (D4 "An ended visit"); this table covers
inbox waits only.

### Wrong implementations

1. Keeping a consumer, acknowledging, or writing anything under the
   inbox directory. Caught by `a_wait_leaves_the_inbox_directory_unchanged`
   (the account's `cursors.yaml` and directory listing are byte-equal
   before and after a run whose wait ended with an event).
2. Reading an account the manifest does not name, or reading after the
   connector or account changed or lost its approval. Caught by
   `an_unapproved_account_refuses_the_start`,
   `a_changed_account_fails_the_wait_on_resume` and
   `the_waker_does_not_resume_for_an_unauthorized_account`.
3. Taking the position from anything but the run's journal (a cursor, a
   file under the config dir). Caught by
   `two_runs_wait_on_one_account_independently` and
   `a_second_visit_counts_only_newer_events`.
4. Counting events stored before the wait started. Caught by
   `events_before_the_wait_are_below_its_start_line`.
5. A `through_seq` past the event that ended the visit. Caught by
   `events_after_the_match_stay_for_the_next_visit`.
6. Advancing the position past a loss the journal does not hold. Caught
   by `a_failed_gap_append_does_not_advance_the_position` and
   `a_gap_after_an_empty_start_is_journaled`.
7. Storing an event body, or a field the playbook did not declare.
   Caught by `an_undeclared_field_never_reaches_the_journal`.
8. Writing the `inbox_wait_read` and the end event in two appends.
   Caught by `the_end_is_one_write` (the end pair's two lines share one
   write: a journal cut after the first line still finishes with that
   line's output, step 1).
9. Declaring a new visit after an inbox wait that ended before its
   `node_finished`. Caught by
   `an_inbox_wait_ended_before_node_finished_keeps_its_output`. The
   opposite mistake, applying the positional rule to a timer or webhook
   wait, changes the journal of a run without an inbox wait; caught by
   `a_timer_wait_ended_before_node_finished_waits_again_as_on_main` and
   `a_webhook_wait_ended_before_node_finished_waits_again_as_on_main`.
10. Parking without the progress line, so the waker resumes at once on
    events the visit already examined. Caught by
    `a_parked_wait_over_unmatched_events_stays_parked`.
11. A waker that compares the log only from its own start, so an event
    stored between the drive's last read and the waker's start is lost.
    Caught by `an_event_stored_before_the_waker_started_resumes_the_run`
    (barrier `Hook::InboxWaitBeforePark` stores an event after the
    progress line).
12. A new reason form or a new pause cause for the inbox park. Caught by
    review and `a_long_inbox_wait_parks_with_the_timer_reason`.
13. Storing values through `redact_json`, or storing anything when the
    storage redactor refuses. Caught by
    `an_unresolved_secret_stores_nothing_and_fails`.
14. Reading the log at every drive tick, caching a failed read, or
    retrying a failed read on every poll. Caught by
    `an_idle_wait_reads_the_absent_log_once` and
    `a_failed_read_waits_the_read_interval`.
15. Treating the output as trusted text. Caught by
    `an_inbox_wait_output_taints_its_reader`.
16. Letting an eval case run an inbox wait. Caught by
    `an_eval_case_refuses_an_inbox_wait`.
17. Changing the journal of a run without an inbox wait. Caught by the
    corpus rows W1 and W2, unchanged, and the two scenario wait tests
    that compare the whole journal with main's.
18. Giving the wait node itself a grant (so node-keyed readers such as
    `scheduler/node.rs` line 2925 see a wait that calls connectors).
    Caught by `the_inbox_site_is_its_own_site`.
19. Deciding the deadline by the poll instead of the receipt, so a late
    poll loses an event that arrived in time or takes one that arrived
    after. Caught by `an_event_received_before_the_deadline_matches_when_polled_late`,
    `an_event_received_after_the_deadline_is_left_for_the_next_visit` and
    `a_pause_moves_the_deadline_by_its_length`, and in the waker by
    `delivery_pending_ignores_a_late_event`.
20. Counting a torn start line as the position, so a later `start` is
    skipped and the visit reads from an old head. Caught by
    `a_start_line_without_wait_started_is_superseded` and
    `position_ignores_a_torn_start_line`.
21. Letting a node take a derived site's id, so one grant replaces
    another. Caught by `v287_refuses_a_node_named_like_an_inbox_site` and
    `v287_refuses_a_node_named_like_a_check_site`.
22. Journaling the site id as the gap's node. Caught by
    `a_retention_gap_is_journaled_once` (it asserts `node_id` is the
    wait node's id).
23. Authorizing, or reading the inbox, to finish a journaled end. Caught
    by `a_journaled_end_is_finished_without_authorization`.
24. Seeding the wait's taint before the fixed-point loop. Caught by
    `an_inbox_wait_output_taints_a_reader_declared_before_it`.
25. Stamping `received_at` before the store's lock. Caught by
    `concurrent_deliveries_are_stamped_in_seq_order` and
    `a_delivery_waiting_on_the_lock_is_stamped_when_it_gets_it`.
26. Treating another node's gap line as the wait's. Caught by
    `a_gap_another_node_recorded_is_journaled_for_the_wait`.
27. Caching a failed read's stamp. Caught by
    `a_failed_read_is_retried_without_a_new_write` and
    `delivery_pending_retries_a_failed_read`.

### Tests

Unit tests (inline `mod tests`):

1. `apb-core` `inbox_wait.rs`: `a_plain_wait_matches_every_event`,
   `conditions_compare_rendered_text`, `type_passes_needs_a_type_when_listed`,
   `output_keeps_only_declared_values`,
   `an_unreadable_value_is_null` (a missing key, a text over 256
   characters, a hidden character), `inbox_site_round_trips`.
2. `apb-core` `schema.rs`: `an_inbox_wait_parses`,
   `an_inbox_wait_with_an_unknown_key_is_refused`,
   `an_inbox_wait_without_a_timeout_is_a_parse_error`.
3. `apb-core` `validate/inbox_wait.rs`: one test per V284 rule,
   `the_site_accepts_hyphenated_connector_and_account` (V23 and V25
   silent for `echo-hooks` and `main`),
   `v285_refuses_a_connector_that_is_not_installed`,
   `v285_needs_a_webhook_connector`,
   `v285_is_silent_without_a_connector_store`,
   `v286_warns_on_types_without_a_type_path`,
   `v287_refuses_a_node_named_like_an_inbox_site` (wait node `a` beside
   a node `a#inbox`),
   `v287_refuses_a_node_named_like_a_check_site` (list node `a` beside a
   node `a#check`), `v287_is_silent_for_distinct_sites`; the cap
   tests `v284_refuses_types_over_the_cap`,
   `v284_refuses_conditions_over_the_cap` and
   `v284_refuses_take_over_the_cap` accept exactly the cap and refuse one
   above it.
4. `apb-core` `validate/node_io.rs`:
   `an_inbox_wait_declares_its_taken_fields` (V46 accepts
   `nodes.w.output.action` and refuses `nodes.w.output.body`).
5. `apb-core` `connector/sites.rs`: `the_inbox_site_is_its_own_site`
   (one `<node>#inbox` site with the binding of `binding(w)`, the wait
   node's own site empty, `site_label` wording),
   `a_node_named_like_a_derived_site_is_its_own_site` (kind `Node`).
   `validate/mcp.rs`: `v302_is_silent_for_a_node_named_like_a_derived_site`
   (an `agent_task` named `fetch#inbox` with an MCP binding and no wait
   `fetch`) and `v302_is_silent_for_a_node_named_x_check_and_v85_refuses_it`
   (V85 keeps refusing `x#check` as on main).
6. `apb-core` `connector/inbox.rs`: `log_stamp_changes_on_append`,
   `log_stamp_is_none_without_a_log`,
   `concurrent_deliveries_are_stamped_in_seq_order` (two threads call
   `append_typed` at once, many times: every later seq has a stamp at
   or above the earlier one),
   `a_delivery_waiting_on_the_lock_is_stamped_when_it_gets_it` (the test
   holds the store's lock for 200 ms; a concurrent `append_typed` is
   stamped at or after the release),
   `received_at_never_goes_back` (a clock that steps back stamps the
   previous event's time).
7. `apb-core` `untrusted.rs`: `an_inbox_wait_output_taints_its_reader`,
   `an_inbox_wait_output_taints_a_reader_declared_before_it` (the
   sending agent precedes the wait in node order; V179 still names it).
8. `apb-core` `eval.rs`: `an_eval_case_refuses_an_inbox_wait`.
9. `apb-core` `validate/guarded.rs`: `an_inbox_wait_names_wait_park`.
10. `apb-engine` `scheduler/wait_park.rs`: `park_seconds_by_kind`,
    `an_inbox_wait_parks_only_caught_up`.
11. `apb-engine` `scheduler/control_apply.rs`:
    `an_inbox_park_is_re_decided_with_its_timeout`.
12. `apb-engine` `scheduler/journal.rs`: `wait_visit_reads_positions`
    (new, open, ended, a duplicate `node_started`, a failed visit).
13. `apb-engine` `inbox_wait/mod.rs`: `position_is_the_latest_read_line`,
    `position_ignores_a_torn_start_line` (a start line with no
    `wait_started` after it reads `None`; one followed by `wait_started`
    counts; a progress and an end line inside a declared visit count),
    `in_time_reads_the_receipt_on_the_paused_interval_clock` (an event
    1 ms before the deadline is in time, one at it and one after are not;
    a person's pause between `wait_started` and the receipt moves the
    deadline by the pause's length; a park does not),
    `a_condition_renders_over_the_run_state` (a condition over a review
    decision, over `run.context` and over `run.previous_success`; each
    fails against a render over an empty state).
14. `apb-engine` `inbox_wait/pending.rs`:
    `delivery_pending_compares_with_the_position`,
    `delivery_pending_reads_only_when_the_log_changed`,
    `delivery_pending_reads_an_absent_log_once`,
    `delivery_pending_applies_the_types`,
    `delivery_pending_wakes_for_a_gap`,
    `delivery_pending_ignores_a_timer_park`,
    `delivery_pending_ignores_a_late_event`,
    `delivery_pending_retries_a_failed_read` (the first read fails, the
    second call receives the state the failed call left, the log's bytes
    and stamp do not change between them, and the next poll reads and
    finds the event).
15. `apb-engine` `pause/waker.rs`:
    `an_inbox_park_waker_resumes_on_a_new_event`.
16. `apb-engine` `pause/display.rs`, `next_action/tests.rs`,
    `apb-cli` `pause.rs`, `apb-mcp` `tools/run.rs`: the existing park
    wording tests with the new text.
17. `web/src/lib/pause.test.ts`: the park line, the cause line.
18. Also: `apb-core` `connector/inbox.rs`
    `a_settled_read_waits_for_the_append_in_flight`; `validate/inbox_wait.rs`
    `v43_checks_the_account_an_inbox_wait_reads`; `validate/node_io.rs`
    `an_inbox_wait_keeps_its_names_over_an_outputs_block`; `apb-engine`
    `inbox_wait/mod.rs`
    `in_time_takes_an_event_under_a_timeout_past_the_u64_instant` and
    `a_signalled_end_outputs_the_empty_object_without_a_read_line`.

Integration tests, `crates/apb-engine/tests/suite/inbox_wait_test.rs`
(the `echo-hooks` fixture connector, `tests/fixtures/connectors/echo-hooks`,
installed and approved by the test helper; events stored with
`Inbox::append_with`, as `tests/suite/connector_inbox.rs` does):

1. Authorization: `an_unapproved_account_refuses_the_start` (the gate's
   `unapproved_connector_account`, no run directory),
   `a_connector_that_is_not_installed_refuses_the_start`
   (`connector_unresolved`),
   `the_manifest_records_the_inbox_site` (`connector_grants` holds
   `<node>#inbox` with the one account and no function),
   `a_changed_account_fails_the_wait_on_resume` (the account edited
   while parked: the resumed drive fails `inbox_wait_unauthorized`,
   nothing journaled for the wait),
   `the_waker_does_not_resume_for_an_unauthorized_account`.
2. The visit: `a_wait_ends_at_its_first_matching_event`,
   `the_output_is_the_declared_values`, `the_output_keeps_the_event_type`,
   `a_timeout_without_a_match_times_out`,
   `an_event_received_before_the_deadline_matches_when_polled_late` (an
   event stored with a receipt before the deadline while nothing drives,
   first read after the deadline with no pause moving it: the final read
   ends the visit with it; fails against "on timeout, end without
   reading"),
   `a_second_event_stored_inside_the_read_interval_is_not_lost` (the
   second event stored inside the read interval before the deadline),
   `an_event_received_after_the_deadline_is_left_for_the_next_visit` (an
   event stored while the first visit is open, with a receipt half a
   second after its deadline: that visit reads it, times out with
   `through_seq` below it, and the loop's next visit ends with it), `a_delivery_that_waited_on_the_lock_across_the_deadline_is_late`
   (the event is stored with a stamp 1 ms after the deadline, as a
   delivery that held the lock past it is, while its body would have
   arrived before: the visit times out),
   `a_failed_read_is_retried_without_a_new_write` (one ordinary read
   fails through the read failure seam, armed before the matching event
   is stored so the read that would find it is the one that fails; the
   log keeps its bytes and modification time between the failure and the
   retry, and the visit ends with the event),
   `a_failed_read_waits_the_read_interval` (three seconds of an
   unreadable log: the ordinary read counter is at least three and at
   most four, so a wait that stops retrying fails it as well as one that
   reads every poll),
   `an_idle_wait_reads_the_absent_log_once` (a successful read of an
   absent log is cached; fails against "read every poll" and against
   "cache a failed read"), `a_transient_head_read_error_is_retried` (the
   first head read fails, the line is repaired only once a counter of
   head reads keyed by run directory shows that failure, and the second
   succeeds: the start pair is written; with every read of the schedule
   failing the node fails `inbox_wait_unreadable`), `a_pause_moves_the_deadline_by_its_length` (a person's pause
   between `wait_started` and the deadline moves the deadline by the
   pause's length: an event stored after the wall time of the unpaused
   deadline but before the paused-interval deadline matches; one after
   that does not),
   `events_before_the_wait_are_below_its_start_line`,
   `events_after_the_match_stay_for_the_next_visit`,
   `a_second_visit_counts_only_newer_events`,
   `two_runs_wait_on_one_account_independently`,
   `a_wait_leaves_the_inbox_directory_unchanged`,
   `a_scripted_inbox_wait_ends_with_an_empty_object`,
   `a_scripted_inbox_wait_that_times_out_ends_with_a_timeout`,
   `the_inbox_next_action_names_the_wait_and_its_connector`,
   `a_scenario_timer_wait_without_an_answer_declares_the_visit_first`
   and `a_scenario_webhook_wait_without_an_answer_declares_the_visit_first`
   (under `on_missing: error` the whole journal sequence, order and
   count, is the one main writes).
3. Bodies and secrets: `an_undeclared_field_never_reaches_the_journal`
   (a marker in an undeclared body field and in a field a condition
   reads: the journal file and every node output hold no marker),
   `a_short_known_secret_in_a_declared_field_is_the_stated_limit` (a
   known secret of 8 characters in a declared field is stored redacted;
   one of 4 characters, below `MIN_SECRET_LEN`, is stored as it is, the
   documented limit), `an_unresolved_secret_stores_nothing_and_fails`
   and `a_command_sourced_secret_stores_no_value` (both use the storage
   redactor, **on main since eval-doubles**).
4. Gaps: `a_retention_gap_is_journaled_once` (its `node_id` is the wait
   node's id, not `<node>#inbox`),
   `a_gap_another_node_recorded_is_journaled_for_the_wait` (a connector
   read of the same account journals the range first under its node;
   the wait journals it again under its own id before its position
   advances), `a_connector_read_gap_rule_is_unchanged` (two connector
   reads of one range still journal it once),
   `a_gap_after_an_empty_start_is_journaled` (start line at 0, the log
   then holds 5 to 9 with 1 to 4 dropped: one `inbox_gap` 1 to 4),
   `a_failed_gap_append_does_not_advance_the_position` (a seam compiled
   out of a release build, keyed by run directory and armed through a
   guard that disarms on drop, makes `journal_gap_in` answer `false` a
   counted number of times: while it fails no `inbox_wait_read` of the
   node carries a `through_seq` at or above the first stored seq, and
   after it clears the gap line is written before any line that moves
   the position), `a_failed_gap_append_does_not_park` (a timeout above
   the release threshold: the wait does not park while the append
   fails), `a_failed_gap_append_held_through_the_deadline_times_out_at_the_held_position`,
   `a_gap_is_journaled_before_the_end_pair`.
   The final read (read failures injected only at the final read,
   through a seam keyed by run directory that takes which read it fails,
   so a delayed drive cannot spend them on an ordinary read; each test
   asserts two final reads): `a_final_read_that_fails_once_still_finds_the_event`
   and `a_final_read_that_fails_twice_times_out_at_the_held_position`
   (the next visit examines the event).
5. Crashes: `the_end_is_one_write`,
   `an_inbox_wait_ended_before_node_finished_keeps_its_output`,
   `a_crash_before_the_end_reads_the_same_events_again`,
   `a_start_line_without_wait_started_is_superseded` (a journal cut
   after the start line, events stored before the resume: the resumed
   drive writes a new start pair at the end of the journal with the
   current head, and the visit never examines an event at or below it),
   `a_journaled_end_is_finished_without_authorization` (a whole end pair
   and a torn one, each with the account edited before the resume: the
   node finishes from the journal and the inbox is not read),
   `a_failed_inbox_wait_lets_the_next_visit_start`.
6. The park: `a_long_inbox_wait_parks_with_the_timer_reason` (the run
   parks with `until` at the timeout, reason `wait (<node>)`),
   `a_delivery_resumes_a_parked_wait` (an event appended after the park:
   the waker resumes the run within one poll and the visit ends with
   it), `an_event_stored_before_the_waker_started_resumes_the_run`,
   `a_parked_wait_over_unmatched_events_stays_parked`,
   `the_park_barrier_fires_on_an_inbox_park_without_a_progress_line`,
   `an_inbox_park_is_no_intervention_and_not_paused_time`.
7. Patches: `a_patch_that_changes_the_account_fails_the_wait` (posted
   only after the read counter of account A is at least one; the node
   fails `inbox_wait_unauthorized` and the read counter of account B
   stays at zero), `a_patch_that_changes_the_condition_matches_by_the_new_condition`,
   `a_batch_supervisor_patch_drops_the_wait_scan` (a supervised fork with
   two deferred agent retries and an open inbox wait on account A; the
   retries fail as a batch, the supervisor's patch moves the wait to an
   account the manifest does not grant and continues from the wait: the
   node fails `inbox_wait_unauthorized` with no read after the patch).

`crates/apb-engine/tests/suite/wait_test.rs` pins that timer and webhook
waits keep main's journal in every state, a crash included:
`a_timer_wait_ended_before_node_finished_waits_again_as_on_main` and
`a_webhook_wait_ended_before_node_finished_waits_again_as_on_main`
(the barrier `Hook::WaitBeforeNodeFinished` stops the drive after the
end event; the resume appends a second `node_started` and
`wait_started`, the sequence main writes). The plain timer journal is
pinned by corpus row W2, so no separate test repeats it.

`crates/apb-engine/tests/suite/report_html_test.rs` line 461: the park
line's new text.

`crates/apb-cli/tests/suite/journal_corpus_test.rs`:

1. Row I1: the new binary runs an inbox wait over an empty `echo-hooks`
   inbox with a 1 s timeout; its journal is main's timer-wait sequence
   with the start line before `wait_started` (`kind` `inbox`) and the
   end line before `wait_timeout`; both old binaries read the finished
   journal (the new lines skipped and counted) and refuse to resume the
   run with a definition parse error.
2. Row I2: the same journal cut after a progress line, before
   `run_paused`: both old binaries refuse a strict read and still stop
   the run (`run_aborted` appended).
3. Row I3: the same journal cut after the end pair, before
   `node_finished`: the same two results.

New test barriers in `crates/apb-engine/src/deadline_hooks.rs` (`Hook`,
line 9): `WaitBeforeNodeFinished` (between a wait's end and the shared
tail's `node_finished`, every kind, so the timer and webhook tests can
stop there too) and `InboxWaitBeforePark` (after the
progress line, before the park is posted).

## Code map

One row per changed file, as merged. (E) marks a file `eval-doubles`
also touches; find the sites by name.

| # | File | Change |
|---|---|---|
| 1 | `crates/apb-core/src/lib.rs` (E) | `pub mod inbox_wait;` |
| 2 | `crates/apb-core/src/schema.rs` | `WaitFor::Inbox`, the conditions in `template_texts`, three tests |
| 3 | `crates/apb-core/src/inbox_wait.rs` (new) | types, constants, site and binding, matching (`debug_assert_eq!` on the rendered length), the output values |
| 4 | `crates/apb-core/src/validate/inbox_wait.rs` (new) | V284 to V287, the shape rules shared with the filter |
| 5 | `crates/apb-core/src/validate/mod.rs` | `mod inbox_wait;` and its call |
| 6 | `crates/apb-core/src/validate/guarded.rs` (E) | `wait_park` also for an inbox wait |
| 7 | `crates/apb-core/src/validate/node_io.rs` | the taken fields in `declared_fields` |
| 8 | `crates/apb-core/src/validate/mcp.rs` | V302 reads the site's kind, two tests |
| 9 | `crates/apb-core/src/validate/connectors.rs` | the site walk's `Site`, its label, V43 for an inbox wait's account |
| 10 | `crates/apb-core/src/validate/params.rs` | a test: every inbox wait string is checked for secret params |
| 11 | `crates/apb-core/src/connector/sites.rs` | `Site` with `SiteKind`, the `<node>#inbox` site, `site_label(&Site)` and `kind_label` |
| 12 | `crates/apb-core/src/connector/event_filter.rs` | `type_passes`, `check_types` and the condition cap shared with the wait |
| 13 | `crates/apb-core/src/connector/inbox.rs` | `log_stamp`, `read_events_settled`, `lost_prefix` made `pub`, the module-doc sentence, the receipt stamp under the lock (`append_at`) |
| 14 | `crates/apb-core/src/connector/resolve.rs` (E) | `ConnectorFacts.names_event_type`, `expand_grant` labels from the `Site` |
| 15 | `crates/apb-core/src/untrusted.rs` | an inbox wait seeds taint, `carries_text` |
| 16 | `crates/apb-core/src/eval.rs` (E) | an eval case refuses an inbox wait |
| 17 | `crates/apb-core/src/eval_doubles.rs` (E) | reads the site walk's `Site` |
| 18 | `crates/apb-core/tests/suite/list_schema_test.rs` | reads the site walk's `Site` |
| 19 | `crates/apb-engine/src/lib.rs` | `pub mod inbox_wait;` |
| 20 | `crates/apb-engine/src/inbox_wait/mod.rs` (new) | `ReadState`, `authorize`, `Scan`, `start`, `poll` with the final read, the end pair, the progress line, `position`, `in_time`, `signalled_output`, the test seams (read and head read counters, the gap and read failures, keyed by run directory) |
| 21 | `crates/apb-engine/src/inbox_wait/pending.rs` (new) | `delivery_pending` |
| 22 | `crates/apb-engine/src/connector/inbox.rs` | `journal_gap_in` split out of `journal_gap`, with `GapScope` |
| 23 | `crates/apb-engine/src/event.rs` (E) | `InboxWaitRead` |
| 24 | `crates/apb-engine/src/state.rs` | `InboxWaitRead` in the no-op arm |
| 25 | `crates/apb-engine/src/gate/mod.rs` | `connector_sources` labels from the `Site` |
| 26 | `crates/apb-engine/src/scheduler.rs` | the visit guard over `wait_visit`, the inbox arm, the scripted inbox wait, the park block, `inbox_scans` cleared on a replaced definition |
| 27 | `crates/apb-engine/src/scheduler/patch.rs` | `Definition` (`Current`, `Replaced`), the result of a patch application |
| 28 | `crates/apb-engine/src/scheduler/supervisor.rs` | the supervisor's `Resumed` wakes carry the `Definition` |
| 29 | `crates/apb-engine/src/scheduler/journal.rs` | `WaitVisit`, `wait_visit` |
| 30 | `crates/apb-engine/src/scheduler/wait_park.rs` | `ParkCheck.parks`, `park_seconds` |
| 31 | `crates/apb-engine/src/scheduler/control_apply.rs` | `park_wait_of` |
| 32 | `crates/apb-engine/src/pause/waker.rs` | the inbox check beside the control check |
| 33 | `crates/apb-engine/src/pause/display.rs` | the neutral park line |
| 34 | `crates/apb-engine/src/next_action/deadlines.rs` | the `Inbox` arm |
| 35 | `crates/apb-engine/src/next_action/verdict.rs` | the action text, the neutral park lines |
| 36 | `crates/apb-engine/src/next_action/tests.rs` | the park wording |
| 37 | `crates/apb-engine/src/deadline_hooks.rs` (E) | two barriers |
| 38 | `crates/apb-engine/tests/main.rs` (E) | one test module |
| 39 | `crates/apb-engine/tests/suite/inbox_wait_test.rs` (new) | the wait end to end, authorization, the final read, gaps, patches, the park |
| 40 | `crates/apb-engine/tests/suite/wait_test.rs` | timer and webhook waits keep main's journal, a crash included |
| 41 | `crates/apb-engine/tests/suite/report_html_test.rs` | the park wording |
| 42 | `crates/apb-engine/tests/fixtures/journal/known_events.jsonl` | one `inbox_wait_read` line |
| 43 | `crates/apb-mcp/src/tools/run.rs` | the park text of `run_wait` and its test |
| 44 | `crates/apb-cli/src/pause.rs` | the early resume note and its test |
| 45 | `crates/apb-cli/tests/suite/journal_corpus_test.rs` (E) | rows I1 to I3 |
| 46 | `web/src/lib/pause.ts` | the park and cause lines |
| 47 | `web/src/lib/pause.test.ts` | its test |
| 48 | `docs/HOWTO-authoring.md` (E) | the inbox wait subsection, the wait section |
| 49 | `docs/CONNECTORS.md` | the inbox wait as the way to consume, its site, the module-doc sentence |
| 50 | `docs/GUARDRAILS.md` (E) | `wait_park` for an inbox wait |
| 51 | `docs/EVALS.md` (E) | the refusal, the `wait_signal` row |
| 52 | `docs/superpowers/specs/2026-11-20-inbox-waits-design.md` (new) | public record |
| 53 | `openspec/specs/inbox-waits/spec.md` (new) | the capability's spec |
| 54 | `openspec/changes/archive/2026-11-21-inbox-waits/` | the archived change |
| 55 | `CLAUDE.md` (E) | module map |
| 56 | `AGENTS.md` (E) | module map |

The site walk's `Site` replaced the `(id, bindings)` pair every caller
of `connector_sites` destructured, which is why rows 9, 14, 17, 18 and
25 change with no behaviour of their own.

Shared with changes in flight (each site differs; resolve by name on
rebase):

| File | This change | Others |
|---|---|---|
| `connector/inbox.rs` (core) | `log_stamp`, `lost_prefix` `pub`, module doc | start-rules adds `head_seq` after `read_filtered`; this change neither adds nor uses `head_seq`, `read(.., after)`, `retire`, `tail_seq` or a public `cursor` |
| `connector/sites.rs` | the `<node>#inbox` site | eval-doubles names `<node>#inbox` in two doc comments as a later change's site and reads every site through `connector_sites` |
| `apb-server/src/ingest.rs` | not touched | start-rules: `trigger_sweep::nudge()` on a stored delivery, `spawn()` at listener start |
| `schema.rs`, `untrusted.rs`, `validate/mod.rs`, `validate/guarded.rs` | `WaitFor::Inbox`, the inbox taint, one module, the `wait_park` condition | lists (`list_skip`, `.from()` taint at `untrusted.rs` line 177), start-rules (`rule_start`), secret-params (`secret_params`), eval-doubles (`eval_doubles`): other variants, other lines |
| `event.rs`, `state.rs`, `known_events.jsonl` | `InboxWaitRead` | lists, mcp-tool-connectors, eval-doubles: their own variants |
| `scheduler.rs` | the wait branch | lists: `ensure_recorded` at drive entry |
| `eval.rs` | one rule in `refusal` | eval-doubles moved connector sites to `case_refusal`; mcp-tool-connectors names its functions there |
| `pause/waker.rs`, `scheduler/wait_park.rs` | the inbox check, `parks`, `park_seconds` | secret-params only cites lines there (no edit) |
| `deadline_hooks.rs`, `tests/main.rs` | two barriers, one module | each change its own |
| docs, `CLAUDE.md`, `AGENTS.md` | own sections and lines | each change its own |

Symbol ownership: `Inbox::retire`, `Inbox::tail_seq`, `Inbox::read(..,
after)`, a public `Inbox::cursor`, `CallOrigin::Wait`, `waiters/` and
`inbox_wait::is_wait_consumer` are no longer added by anyone (start-rules
withdrew its note on `retire`, and this change cut them). `Inbox::event`
does not exist: start-rules cut it in its third pass (its design, "Cut
in the third pass" item 1). `Inbox::head_seq` and the ingest nudge are
start-rules'; this change uses neither (the start line takes the head
from `read_events`).

Files not touched, checked: `crates/apb-server/src/ingest.rs`,
`crates/apb-engine/src/terminal_hook.rs`, `pause/resume_all.rs`,
`run_end.rs`, `scheduler/prepare.rs`, `gate/trust_scan.rs`,
`connector/run.rs`, `connector/call/*`, `manifest.rs`, `effects.rs`,
`pause/cause.rs`, `pause.rs`, `pause/clock.rs`,
`run_stats/interventions.rs`, `scenario/capture.rs`, `apb-cli/src/main.rs`,
`apb-server/src/routes/runs.rs`, `apb-server/src/ts_contract.rs`,
`web/src/lib/api.gen.ts`, `web/src/lib/consts.gen.ts`,
`web/src/lib/runoutcome.ts`, `web/src/lib/journal.ts` and
`crates/apb-engine/src/eval/doubles.rs` (eval-doubles).

## Release notes

1. `wait_for: { type: inbox }` waits for the first matching event in a
   connector account's inbox and hands the next node the values of the
   fields the playbook declares (with the event's seq, receive time and
   type). The connector and account are trusted like any binding. It
   keeps its place in its own run's journal, stores no event body and
   acknowledges nothing. A long inbox wait parks like a long timer
   wait, and a stored event resumes it within the waker's poll.
2. A playbook with an inbox wait carries the run guard `wait_park`; an
   older apb cannot load such a definition at all.
3. An inbox wait whose process died after the wait ended and before the
   node was finished is finished on resume from the journal instead of
   waiting again. Timer and webhook waits are unchanged, in every state.
4. An inbox event counts only when it was stored before the wait's
   timeout on the run's paused-interval clock, however late the run
   reads it.
5. A playbook in which a node is named like another node's derived
   binding site (`a#inbox`, `a#check`) is refused (V287).
6. A parked wait reads "waiting at `<node>` until <T> at the latest" on
   every surface (was "waiting on timer").
7. Eval cases refuse a playbook with an inbox wait.

## Implementation notes

1. `eval-doubles` is on main (3bb424be); the marked site
   (`RunRedactor::for_storing`) is found by name.
2. `known_events.jsonl` needs one line for `inbox_wait_read` with the
   fields above, or the journal round trip test fails.
3. Never run a real agent or reach a real service in a test.
4. The release clippy gate (tasks 4.11) passes when `cargo clippy
   --release` prints no error and no warning in a file this change
   touches that the same command on the base main does not also print.
5. The module map in `CLAUDE.md` and `AGENTS.md` (the same edit in
   both): `inbox_wait/` is its own apb-engine entry (`mod` the
   authorization, the visit, `position` and `in_time`; `pending` the
   park waker's check), outside the sentence that lists the phases under
   `scheduler/`; one apb-core entry for `inbox_wait.rs` (the site, the
   matching, the declared values); the waker sentence in the `pause/`
   entry.
6. Every gate of tasks section 4 runs, `scripts/third-party-notices.sh
   --check` included. Temp data of the local gates goes to a directory
   outside the worktree and outside `/tmp`.

## Cut

The closure pass of 2026-11-20 applied the lessons of the last three
changes to the previous draft (69 files, a run-scoped consumer,
acknowledgements, a settle protocol across visits, a waiter registry
woken by the listener, terminal cleanup). Each mechanism below was owned
by this change; each is gone, with the reason and what carries its job
now.

1. The run-scoped inbox consumer (`run_<32 hex>`, the project key, its
   computation per visit, `wait_started.inbox`). Why: its only job was
   to hold a position, and a position in a shared machine store needed
   a project identity, a uniqueness argument and a cleanup. The run's
   journal already holds positions uniquely per run; start-rules made
   the same decision for its rules (a reader keeps its position in its
   own state and registers no consumer). Now: `position(events, node)`.
2. Acknowledgements and everything that guarded them: the ack call, the
   settle-before-ack protocol, the reconcile of an earlier visit's ack,
   `Scan.acked`, `MAX_ACK_FAILURES`, `inbox_wait_ack_failed`, the
   dropped-only settle's ack, the reserved consumer names and their
   refusal in `inbox::build`, `CallOrigin::Wait`, the start-after call
   option `after_seq`, the `read` and `ack` fields, V285's argument
   samples, `ConnectorFacts.inbox_ops` and `inbox_args_schemas`, the
   start-time consumer check in `allocate_run` and
   `inbox_wait_consumer_refused`. Why: with no consumer there is nothing
   to acknowledge, and every one of these existed to make an ack safe.
   Now: the wait reads `Inbox::read_events` directly and moves nothing.
3. The waiter registry (`waiters/<consumer>.json`, its lock, the 256
   capacity, `inbox_waiter_full` in the diagnostics, the view's `note`),
   the listener's wake post (`Control::Interrupt` per waiter) and its
   start sweep, `wake_all`, the `tail_seq` bounded read and the mark.
   Why: it was a second wake channel beside the waker the park already
   has, with its own capacity, crash windows and cleanup. Now: the
   park's own waker reads the account's log at its poll against the
   journaled position (D4 "Parking" item 6); latency is one waker poll
   (5 s), the cost of a delivery-driven wake.
4. Retirement after the terminal event (the third terminal observer
   slot, `retire_ended`, the `apb resume --all` sweep, `Inbox::retire`,
   `InboxWaitBeforeRetire`). Why: nothing outside the run exists to
   retire.
5. The `<node>#inbox` site as a call grant for `read` and `ack`: cut,
   and brought back in the third pass as a site that grants no function
   and exists only so the gate authorizes the connector and the account
   (D4 "Authorization"; "Cut in the third pass" item 6).
6. Doubled waits (the doubled read and ack, the idempotent ack in
   `eval::doubles::reserve`, `Reservation.repeat`, the never-park rule
   for a doubled run, `a_doubled_wait_ack_after_a_crash_uses_no_second_answer`).
   Why: an eval case never needs a live inbox; refusing it in
   `eval::refusal`, as a sub-playbook is refused, removes a whole class
   of crash windows. Future work: scripted inbox events in a case.
7. A second reason form `inbox wait (<node>)` with `park_node`,
   `parse_inbox_wait_reason`, `is_inbox_park_reason` and the changes to
   `cause_of` and `PauseRequest::is_park`. Why: the park classification
   already keys on `wait (<node>)`; the waker and the scan read the
   node's kind from the definition they already load. Now: one form, and
   neutral park wording on the five surfaces.
8. The surface view (`InboxWaitView`, `RunDetail.inbox_waits`, MCP
   `run_status.inbox_waits`, the outcome summary lines, the ts contract,
   `api.gen.ts`, `runoutcome.ts`). Why: its last unique content was the
   capacity note (item 3); the position is in the journal line, the
   event in the node's output, gaps in the existing gap surfaces.
9. The paired `node_started` and `wait_started` write (it journaled the
   consumer before any read; there is none).
10. `poll_seconds` and its V284 range: a live visit reads when the log
    changed, at most once per second (`MIN_READ_INTERVAL_MS`).
11. `from: stored` (every stored event): future work; the first visit
    starts at the log's head when the wait starts.
12. The three-polls bound on unresolved secrets: the first refusal fails
    the node (an unresolved secret does not resolve by waiting).
13. The paging rule ("drained, defined", `MAX_PAGES_PER_POLL`): the wait
    reads the whole log, as `Inbox::read` already does on main.
14. The `scenario/capture.rs` change: the existing code is already right
    for an inbox wait (D4 "Evals and scenarios" item 3).

## Cut in the third pass

1. Batching: the `batch` block (`quiet_seconds`, `max_events`,
   `max_wait_seconds`), the type `Batch`, the window rules
   `closed_before` and `decide`, `Counted`, the end reasons `quiet`,
   `max_wait`, `max_events`, `first` and `size` (`ClosedBy` with them;
   `end` is now `matched` or `timeout`), `MAX_EVENTS`, the in-memory
   list of counted entries, the wall-time judgement of an open window,
   their V284 range rules and their tests. Why: counted events held only
   in memory across reads could be lost to retention after a crash
   (third-pass review, the first major). A wait now ends at its first
   matching event, so nothing is held across reads. Now: one event per
   visit; several events are a loop around the wait.
2. `until` (the `Until` block, `closes`, `until.types` in V286, the
   closing-event scenario). Why: with one event per visit every match
   ends the visit; a loop leaves on an `output_field` edge over a taken
   field.
3. Event bodies in the journal and the output: the body in each output
   entry (the `event_row` shape), `MAX_EVENT_BYTES` and the withheld
   marker, `MAX_OUTPUT_BYTES`, `too_large`, the `seen`, `matched`,
   `dropped` and `closed_by` fields of `inbox_wait_read`. Why: the
   storage redactor cannot promise that a body holds no secret (third
   pass, the third major). Now: `take`, the declared values only.
4. The run-start filter of the first visit (`run_started_ms`, events
   received before `run_started` dropped by time) and "a first visit
   reports no gap". Why: a position of 0 silently suppressed a gap
   (third pass, the second major). Now: the start line at the log's head,
   journaled with `wait_started`, and the gap rule at every position.
5. V284's slug rule for `connector` and `account`, and V285's skip of a
   connector that is not installed. Why: the site's binding is checked
   by V23 and V25 like any binding, and the gate refuses a missing
   connector; V285 now names it whenever a connector store is known.
6. The second Cut item 5 reason ("a wait makes no call, so it needs no
   site"). Why: reading an inbox is still reading a connector account's
   data, which needs the same trust as a call (third pass, the
   blocker). Now: the `<node>#inbox` site, no function granted.
7. The dashboard journal note for `inbox_wait_read` (`web/src/lib/journal.ts`
   and its test) and Code map rows 50 and 51 (the two eval-doubles doc
   comments, now true as written). Why: the line holds a position and
   counts that the node output and the gap lines already show, and the
   doc comments name exactly the site this pass adds.

## Review record

Four design review rounds ran on the earlier drafts (rounds 1 to 3 on
the combined draft `evals-and-inbox-waits`, round 4 on this change
alone; their findings were applied in the previous pass without a
re-review). Their rows are kept by number with where each stands after
this pass.

Round 1: R1.2 (settlement past what the output holds): moot, one event
per visit. R1.3 (waiter type filter): moot, no registry; the waker
applies `types` (`delivery_pending_applies_the_types`). R1.5 (redaction
that does not fail closed): kept, `for_storing`, failing at the first
refusal. R1.6 (retirement boundary): moot, nothing to retire. R1.7 (V285
ops and the projected read): moot, no connector call. R1.10 (waiters
beyond the listener's read limit): moot. R1.11 (full-file head check):
the waker reads the log only when its stamp changed; the whole-file read
is a stated limit. R1.13 (retention drops an unread prefix):
pre-existing; a wait journals the gap before it advances.

Round 2: R2.14 (crash between the end and `node_finished`): kept, the
positional `wait_visit`, also covering the end pair's torn first line;
narrowed to inbox waits in the fourth pass.
R2.15 (storage scrubbing): kept, now over declared values only. R2.18
(children missed by `apb resume --all`): moot. R2.19 (park recheck
mark): replaced by the progress line and the waker's first-call read.
R2.20 (lost wake): the waker's own check is the only wake, so a failed
post cannot exist. R2.21 (`after_seq` against the schema): moot, no
call. R2.24 (an ack error after the settle): moot.

Round 3: R3.1 (two projects share a consumer): moot, the position is
per run journal. R3.3 (backlog closes at the wrong event): moot, no
windows. R3.5 (other callers advance a wait's cursor): moot, no cursor.
R3.6 (doubled ack after a crash): moot, evals refuse inbox waits. R3.9
(V285 accepts uncallable functions): moot. R3.10 (event type in the
output): kept. R3.11 (journal of runs without the feature): kept, no
paired write for any other wait. R3.12 (three-failure bound): moot.
R3.14 (capacity note per visit): moot. R3.15 (release clippy
criterion): kept.

Round 4: D4.B1 (terminal cleanup of guarded runs): moot. D4.B2 (V284
slugs): kept in substance, now V23 and V25 at the site. D4.B3 (a
precondition that could not pass): task 0 names the exact files again.
D4.M4, D4.M5 (ack of a later or earlier visit): moot. D4.M6 (the sample
consumer): moot. D4.M7 (drain across pages): moot, one read returns
every stored event. D4.M8 (drops scanned again after a timeout): kept,
every inbox end writes its `through_seq`, a timeout included. D4.m9
(error journal claim): moot.

Closure pass (2026-11-20), checked against main efae5822 and the
eval-doubles branch (PR #291):

1. Applied the lessons: no own consumer, queue, wake channel or
   retirement beside the engine's (Cut 1 to 4); the position is one
   function over the journal; the end is one write of an existing end
   event plus one new line, before the checkpoint that explains it; no
   new guard or manifest mark; a run without an inbox wait writes main's
   journal.
2. Drift fixed against the code: eval-doubles' redactor is
   `RunRedactor::for_storing` in `run_redact.rs` (line 543 on its
   branch), its pure doubles are in `crates/apb-core/src/eval_doubles.rs`
   (not only `eval/doubles.rs`), and its `eval::refusal` keeps the node
   loop while connector sites moved to `case_refusal`;
   `readiness::check_readiness` is at `validate/mod.rs` line 467 (was
   468); `pause_at_is_wait` guards the waker's control check at
   `waker.rs` line 160; start-rules adds `Inbox::head_seq` only
   (`Inbox::event` was cut there), so neither symbol is this change's.
3. Validator codes re-checked free against every change in flight.
4. Every requirement of the spec delta is under 500 characters.

### Third pass

A design review of the closure draft (2026-11-20) found 1 blocker, 3
majors and 2 minors. Each was checked against main efae5822 (this
worktree's base) and, where named, the eval-doubles branch.

1. Blocker, inbox reads bypass connector and account authorization.
   Confirmed: `resolve_playbook` (`connector/resolve.rs` lines 93 and
   112), the gate's trust scan (`gate/trust_scan.rs` line 50) and the
   snapshot (`connector/run.rs` line 41) walk only `connector_sites`
   (`connector/sites.rs` line 44), and `Inbox::open` (`inbox.rs` line
   222) addresses the machine store by connector and account alone; the
   draft's V285 skipped a connector that is not installed. Fixed through
   the existing mechanism: the wait is the site `<node>#inbox`, a
   binding of the connector and exactly the account with no function;
   the gate's trust check, the permit and the manifest's connector
   snapshot cover it with no gate code change; `authorize` checks the
   manifest grant, the live digests and their approval before the first
   read, at every drive (so at resume) and in the waker (narrowed in the
   fourth pass: a journaled end needs none); V285 refuses a
   connector that is not installed whenever a connector store is known
   (D4 "Authorization", "Validator").
2. Major, counted events lost in a crash. Confirmed: the draft held
   counted entries only in `Scan` until the end pair, and the size cap
   (`inbox.rs` line 720) can drop them before a resume. Fixed by cutting
   batching: a visit ends at its first matching event, its output is
   that event, and a crash before the end pair reads again from the
   journaled position; what retention removed above it meanwhile is the
   gap of item 3 ("Cut in the third pass" items 1 and 2).
3. Major, a failed gap append followed by a position advance. Confirmed:
   `journal_gap` returns `false` on a journal failure (`connector/inbox.rs`
   lines 392 to 436) and the draft went on examining. Fixed: the gap
   line must be in the journal before anything else of the read; on
   `false` nothing is examined, the wait does not park, the next poll
   tries again, and a timeout ends the visit with `through_seq` at the
   position (retry chosen over failing the node; D4 "The visit" step 4).
   The draft's position 0 suppressed a gap; now the first visit starts
   at the log's head, journaled with `wait_started`, so events before the
   wait began are below the position and the gap rule applies at every
   position, 0 included (D4 "The position" item 1).
4. Major, the storage redactor cannot guarantee a body holds no secret.
   Confirmed: `Scrubber::new` drops known values shorter than
   `MIN_SECRET_LEN` (`scrub.rs` lines 45 and 133), and an unknown
   credential without a token shape is not recognized. Narrowed as
   decided: no body is stored; the playbook declares `take` entries
   (names joined to the declared output fields, `validate/node_io.rs`
   line 168; paths and limits of the baseline observation reader,
   `baseline/extract.rs` lines 27 and 123, `baseline/mod.rs` line 38),
   and only those values, the type, the seq and the receive time are
   stored, scrubbed and untrusted. The limit is stated in the design and
   the docs; tests pin that an undeclared field never reaches the journal
   and what happens to a short known secret.
5. Minor, older-binary readability overstated. Confirmed: `read_all`
   (`event.rs` line 3909) refuses an unknown event after the last
   checkpoint (`check_unknown_settled`, line 3963), and
   `read_all_for_stop` (line 3926) lets a stop through. Fixed: D0 "An
   older apb" states the refusal for the open-visit journals; corpus
   rows I2 and I3 test both intermediate journals.
6. Minor, two tasks not verifiable. Confirmed: `timeout_seconds` is a
   required `u64` (`schema.rs` line 2168), so an absent one fails the
   parse before validation; the case is now
   `an_inbox_wait_without_a_timeout_is_a_parse_error` in `schema.rs`,
   and V284 keeps only `timeout_seconds: 0`. Task 2.13 is two commands.

Pre-existing on main, named by the same review: a timer or webhook wait
that ended before `node_finished` starts a new visit after a crash
(`scheduler.rs` line 2655): left as it is (the fourth pass withdrew the
fix "for every kind"; `wait_visit` decides inbox waits only).
A torn final journal line makes strict reads fail (`event.rs` line
3996): unchanged, outside this change (crash table row 9).

Recount: 49 changed files, down from 51 (Code map).

Pre-existing on main, not changed here: retention can drop an
unacknowledged prefix under its size cap (`inbox.rs` line 720); every
inbox read reads the whole log.

### Fourth pass

A confirmation review of the third-pass draft (2026-11-20) found 1
blocker, 3 majors and 3 minors. The branch was rebased onto main
3bb424be, where `eval-doubles` (#291) is merged, so every "requires
eval-doubles, merged first" now reads "on main since eval-doubles"; the
lines this pass cites are those of 3bb424be. Each finding was checked
against that tree.

1. Blocker, a run without an inbox wait could write another journal.
   Confirmed: the draft's `wait_visit` decided timer and webhook waits
   too, so after a crash between their end and `node_finished` it
   finished the node where main appends a second `node_started` and
   `wait_started` (`scheduler.rs` line 2655). Fixed: the positional
   rule decides inbox waits only; timer and webhook waits keep main's
   count guard exactly, crash behaviour included, so a run without an
   inbox wait writes main's journal in every state (D0 "What a run that
   uses no inbox wait writes", D4 "An ended visit", "The wait arm" item
   1). The "fixed for every kind" claim is withdrawn; that timer and
   webhook gap stays a pre-existing note (the third pass's closing
   paragraph). `wait_test.rs` now pins main's sequence for both kinds.
2. Major, a torn start pair used the old head. Confirmed: a start line
   with no `wait_started` made `position` `Some`, so the arm appended
   `wait_started` alone. Fixed: a line is counted only when the event
   that follows it is the node's `wait_started` (a complete start line)
   or when it lies inside a declared visit (a progress or end line);
   anything else is a torn start line, superseded, so `position` reads
   `None` and the next drive writes a new start pair at the current log
   head (D4 "The position", crash table row 2).
3. Major, an event received after the timeout could succeed. Confirmed:
   `poll` matched in step 5 before it checked the timeout in step 6, and
   the waker looked only at seq and type. Fixed: receipt decides. An
   event is in time when the wait's due time on the paused-interval
   clock, `wait_park::due_at_ms` (`wait_park.rs` line 27) evaluated as
   of its `received_at`, lies after that receipt; the drive stops its
   examination at the first late event, which stays for the next visit,
   and the waker ignores a late event (D4 "The deadline"). Tests: a
   delivery just before and just after the deadline, each polled late,
   and a person's pause in between.
4. Major, `<node>#inbox` could collide with a node id. Confirmed: node
   ids may contain `#`, sites come from `connector_sites`
   (`connector/sites.rs` line 44) and `resolve_playbook` inserts grants
   by site id (`connector/resolve.rs` line 141), so a later site
   replaces an earlier one; `a#check` beside a list node `a` collides
   on main already. Fixed: new code V287 refuses a node named like a
   derived site, an error, so preparation refuses the definition before
   grant resolution (D4 "Authorization", "Validator" item 4). Tests for
   both collisions.
5. Minor, recovery skipped the stated authorization recheck. Confirmed.
   Decided: a journaled end needs no authorization, because nothing more
   is read; a complete end pair (`WaitVisit::Ended`) and a torn one
   (`poll` step 1) are finished from the journal alone, with no
   `authorize` and no inbox read. The "every drive" wording now covers
   only an open visit whose end is not journaled (D4 "Authorization",
   "The visit" step 1). Test:
   `a_journaled_end_is_finished_without_authorization`.
6. Minor, the gap named the site as its node. Confirmed against
   `RunInboxGap` (`run_outcome.rs` line 59) and `inbox_gap_lines`
   (line 327). Fixed: `inbox_gap.node_id` is the wait node's id; the site
   id is used only for authorization (D4 "The visit", `poll` step 4).
7. Minor, task 2.10 could not meet its asserted result. Follows from
   item 2: with the torn start line superseded,
   `a_start_line_without_wait_started_is_superseded` is achievable as
   written; task 2.10 names `position`'s counting rule as its code.

Recount: 49 changed files, unchanged (V287 lives in the new validator
module; the other fixes change files already listed).

### Fifth pass

A last review of the fourth-pass draft (2026-11-20) found 1 blocker, 3
majors and 1 minor, all accepted with the review's fix, each checked
against main 3bb424be.

1. Blocker, untrusted output could bypass V179. Confirmed: `analyze`
   takes another pass only when a node's source count grows
   (`untrusted.rs` lines 213 and 224), so a wait source seeded into
   `tainted` before the loop never grows and a follower placed before
   the wait in node order is missed. Fixed: the wait's source is
   inserted inside the loop beside rule 2 (line 150), so the first pass
   grows it and the next reaches every reader (D4 "Untrusted text").
   Test: `an_inbox_wait_output_taints_a_reader_declared_before_it`,
   V179 with the consuming agent declared before the wait.
2. Major, receipt stamped before the store lock. Confirmed:
   `append_with` and `append_typed` (`inbox.rs` lines 285 and 306)
   read the clock as an argument before `append_bounded` takes the lock
   (line 344); the seq is assigned at line 356. Fixed: receipt is the
   moment the seq is assigned under the lock, and the production stamp
   is taken there through a private `append_at` with a clock function,
   never below the previous event's stamp, so stamps are monotonic with
   seq; `append_bounded` keeps its explicit `now` as the test seam (D4
   "The deadline"). For start-rules, which reads the same store,
   nothing changes: it does not use the time (it renders
   `{{event.received_at}}` and never compares it). Tests: two concurrent
   deliveries, a delivery that waits on the lock across the deadline.
3. Major, a gap another node recorded let the wait advance. Confirmed:
   the once-per-gap rule matches connector, account and range only
   (`connector/inbox.rs` line 425). Fixed: `journal_gap_in` takes a
   `GapScope`; a wait uses `GapScope::Node`, which also requires the
   line's `node_id`, so the range is journaled for the wait before its
   position advances; connector reads keep `GapScope::Account`, the
   rule of main, unchanged (`poll` step 4). Tests:
   `a_gap_another_node_recorded_is_journaled_for_the_wait`,
   `a_connector_read_gap_rule_is_unchanged`.
4. Major, a transient read error could leave a parked wait asleep.
   Confirmed: `read_events` can fail with an I/O or parse error
   (`inbox.rs` line 590) and both readers read again only on a changed
   stamp. Fixed: a stamp is remembered only after a successful read and
   a failed read clears it, so the drive (`poll` step 3) and the waker
   (`delivery_pending` step 4) read again at their next poll; the
   initial head read in `start` is retried within the engine's existing
   bounded infrastructure retries, `failure_class::backoff_schedule()`
   (default 5 s then 30 s, at most three reads over about 35 s,
   `APB_INFRA_BACKOFF_MS`), before it fails the node
   `inbox_wait_unreadable`. Tests:
   `a_failed_read_is_retried_without_a_new_write`,
   `delivery_pending_retries_a_failed_read`,
   `a_transient_head_read_error_is_retried`.
5. Minor, task 0.2 omitted V287. Fixed: its check is `"V28[4-7]"`.

Recount: 49 changed files, unchanged (the stamp lives in core
`connector/inbox.rs` and the scope in engine `connector/inbox.rs`, both
already listed; `apb-server/src/ingest.rs` keeps calling
`append_typed` unchanged).

### Implementation review

Three review rounds of the implementation (2026-11-21), each by
independent reviewers, changed the text above where they found a rule
missing or a test that could not fail; their rules are now part of the
body and no section overrides another. Round 1: a gap that cannot be
journaled stops the read (`poll` step 4); the timeout ends a visit only
after one final read (`poll` step 6); conditions render through the one
template render (`poll` step 2); three read states (`ReadState`); a
patch drops the scans (`poll` step 2); a binding site is classified by
what owns it (`SiteKind`, D4 "Authorization"); V284 shares the filter's
shape rules; the scripted wait is read where main reads it (the wait
arm item 3); the docs on taken values and exact text equality; the
module map. Round 2: one "definition replaced" value reports every
patch, the batch path included; V85 and the `#check` spend attribution
stay main's; a failed read also waits the read interval; the gap
failure seam is keyed by run directory and disarms on drop; tests that
reach their boundary. Round 3 found no production defect: the read
interval test gained its lower bound and the final read tests inject
their failure only at the final read. The last independent pass found
no production defect.

The pull request's automated review then led to these changes, folded
into the body above: V43 also checks the account an inbox wait reads; the
wait's output names take precedence over an `outputs.fields` block;
`in_time` compares active time with the timeout instead of computing a
deadline instant that could overflow; a resumed scripted wait finishes
with `{}`; the final read waits, read-only, for an append in flight; the
early-resume line no longer claims a resume only parks again; and several
tests were made independent of timing (the crash fixtures wait for the
panicked drive to unwind, the failed-read test arms the read failure seam
before the store, the head-read test waits for the counted failure, the
late-delivery test reaches the first visit, the gap tests compare with
the first line that moves the position).
