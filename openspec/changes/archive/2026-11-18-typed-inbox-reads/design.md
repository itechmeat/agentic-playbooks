# Design: typed inbox reads

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is today. Built on main 316c8180
(1.0.0-beta.7); every line number below was checked against that tree.
This change has no hard predecessor in the 1.0 sequence (people-in-the-loop
and before-running-alone touch none of its sites); if they merged first
and a line moved, find the site by the name given next to it, never by
the number alone.

One decision, D2. The name D2 is kept from the drafted start-rules design
so that the next change, `start-rules-and-schedules` (D0, D1, D4), can
name it: the typed inbox read (D2) is what a start rule's filter is made
of. The released timer wait (D3 of the drafted design) is a later change
of its own. This change names no type of that change: no rule, no
`TriggeredBy`, no `Initiator::Trigger`, no validator code.

Nothing in this document is left to the implementer. Where a choice was
open, it is decided here and the reason is given; tasks.md only orders
the work.

## What this change reuses (checked on 316c8180)

| Piece | Where |
|---|---|
| the inbox store | `crates/apb-core/src/connector/inbox.rs` (`InboxEvent` 79, `read` 354, `ack` 383, `read_events` 459, `min_cursor` 493, `last_seq` 519, `check_consumer` 803) |
| body paths | `crates/apb-core/src/connector/common.rs` `value_at_path` 117, `scalar_at_path` 135 |
| the list filter's equality | `crates/apb-core/src/list/items.rs` `field_equals` line 240 |
| the ingest listener | `crates/apb-server/src/ingest.rs` (`post_hook_handler` 637, the dedupe id 736, the append 777, `run_ingest_server` 464) |

## D2. Typed, filtered inbox reads

### Schema: the webhook block names the event type

`crates/apb-core/src/connector/def.rs` `WebhookSpec` (line 292) gets two
optional literal fields after `dedupe_path` (line 306):

```rust
/// Dot path into the delivery body naming the event's type
/// (`entry.0.changes.0.field`). At most one of this and
/// `event_type_header`.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub event_type_path: Option<String>,
/// A request header naming the event's type (`X-GitHub-Event`).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub event_type_header: Option<String>,
```

1. `validate_webhook_shape` (line 1168): both set is an error
   `"connector `{c}` webhook declares both event_type_path and event_type_header; keep one"`;
   `event_type_path` follows the `dedupe_path` rule (a dot path with
   non-empty segments, the check at line 1187); `event_type_header` is 1
   to 64 characters of `[A-Za-z0-9-]`, else
   `"connector `{c}` webhook event_type_header must be a header name"`.
2. `validate_webhook_templates` (line 1592): both fields join the
   literal list next to `hook.dedupe_path.as_deref()` (line 1617), so a
   placeholder in either is refused with the existing message.
3. `pub fn names_event_type(&self) -> bool` on `WebhookSpec`: either field set.

### The stored event carries its type

`InboxEvent` (`crates/apb-core/src/connector/inbox.rs` line 79) gets

```rust
/// The event's type as the connector's webhook block names it, cleaned
/// (`webhook::event_type`); absent for an untyped delivery and for every
/// event stored before this field existed.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub event_type: Option<String>,
```

`append_bounded` (line 293) gets a parameter `event_type: Option<&str>`
after `body` and stores it; `append_with` and `append` pass `None`, and a
new `pub fn append_typed(&self, provider_id: &str, event_type: Option<&str>, body: &Value) -> Result<Appended, InboxError>`
passes it with the default retention, dedupe window and clock. Every
other caller of `append_bounded` (tests in this file) passes `None`. The
dedupe identity does not change: a redelivery is a duplicate whatever
its type.

`crates/apb-core/src/connector/webhook.rs` gets

```rust
/// The longest event type kept.
pub const EVENT_TYPE_MAX: usize = 100;

/// The event type of one verified delivery: the header's value when the
/// block names a header, else the scalar at the block's path, cleaned by
/// [`clean_event_type`]; `None` when the block names neither or the value
/// does not clean.
pub fn event_type(body: &Value, header: Option<&str>, hook: &WebhookSpec) -> Option<String>;

/// Trims; keeps a value of 1 to [`EVENT_TYPE_MAX`] characters, each of
/// `[A-Za-z0-9._:/-]`; anything else is `None` (the value is
/// provider-written and is shown on surfaces and in journals).
pub fn clean_event_type(raw: &str) -> Option<String>;
```

In `post_hook_handler` (`crates/apb-server/src/ingest.rs` line 637),
right after the dedupe id (line 736):

```rust
let type_header = hook
    .event_type_header
    .as_deref()
    .and_then(|h| headers.get(h))
    .and_then(|v| v.to_str().ok());
let event_type = webhook::event_type(&parsed, type_header, &hook);
```

and the append (line 777) becomes
`store.append_typed(&id, event_type.as_deref(), &parsed)`. The type is
computed only after the signature verified, so an unsigned caller never
reaches it. Nothing else in the handler changes: a stored delivery still
starts nothing (start rules are the next change, start-rules-and-schedules).

### The filter

New file `crates/apb-core/src/connector/event_filter.rs`, declared in
`connector/mod.rs` next to `pub mod inbox;`:

```rust
/// At most this many types and this many conditions in one filter.
pub const MAX_TYPES: usize = 20;
pub const MAX_CONDITIONS: usize = 10;

/// Which inbox events a reader wants: an event passes when its type is in
/// `types` (when `types` is not empty) and every condition holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    #[serde(default, rename = "where", skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<BodyCondition>,
}

/// One condition over the event body: the value at `path` equals
/// `equals` (a string, number, bool or null; never an object or array).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyCondition {
    pub path: String,
    pub equals: Value,
}

/// How one condition came out on one event. Never carries body text:
/// `got` is the event type for the type condition, `None` otherwise.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionResult {
    /// `type in [issues, pull_request]` or `action = "opened"`.
    pub condition: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub got: Option<String>,
}

impl EventFilter {
    pub fn is_empty(&self) -> bool;
    /// The filter's own shape: the caps above, every type cleans to itself
    /// (`webhook::clean_event_type`), every path is a dot path with
    /// non-empty segments, every `equals` is a scalar or null. The error
    /// names the first problem.
    pub fn check(&self) -> Result<(), String>;
    /// One result per condition, the type condition first (when `types` is
    /// not empty), in declaration order.
    pub fn explain(&self, event: &InboxEvent) -> Vec<ConditionResult>;
    /// Every result of `explain` passed.
    pub fn matches(&self, event: &InboxEvent) -> bool;
}
```

Semantics, in one place:

1. `types` not empty: the event's `event_type` is `Some(t)` and `t` is in
   `types` (exact, case-sensitive). An untyped event never passes a type
   condition.
2. A condition: `field_equals(value_at_path(&event.body, &c.path), &c.equals)`.
   `field_equals` (`crates/apb-core/src/list/items.rs` line 240) becomes
   `pub(crate)`, so a body condition and a list's `where` compare the
   same way (`equals: null` needs the path present and null; a missing
   path never equals anything; an object or array never matches).
   `value_at_path` is `connector::common::value_at_path` (line 117).
3. The `condition` text of a body condition is
   `format!("{} = {}", c.path, c.equals)` (the condition as written, never
   the body's value).

### A filtered read

`Inbox` gets (inbox.rs, after `read` at line 354):

```rust
/// What a filtered read found.
#[derive(Debug, Clone, PartialEq)]
pub struct FilteredRead {
    pub events: Vec<InboxEvent>,
    /// The consumer's cursor after the read (moved past a leading run of
    /// skipped events when `ack_skipped`).
    pub cursor: u64,
    /// The gap, computed from the cursor before anything moved.
    pub gap: Option<InboxGap>,
    /// Pending events the read did not return because they did not match.
    pub skipped: u64,
}

/// [`Inbox::read`] through `filter`: the pending events of `consumer`
/// that match, oldest first, at most `limit`.
///
/// The events before the first match that do not match are skipped and,
/// when `ack_skipped`, acknowledged for this consumer only: its cursor
/// moves to the last of them. A non-matching event after a match is left
/// unacknowledged (the reader has not acked the match yet); it is skipped
/// again by the next read, once the match is acked. The gap is computed
/// exactly as `read` does, before the cursor moves, so a filter never
/// hides one. A move past a gap first hands the gap to `record_gap`
/// under the lock, before the cursor file is written; on `false` the
/// cursor does not move.
pub fn read_filtered(
    &self,
    consumer: &str,
    limit: usize,
    filter: &EventFilter,
    ack_skipped: bool,
    record_gap: impl FnOnce(&InboxGap) -> bool,
) -> Result<FilteredRead, InboxError>;
```

`read_filtered` with `ack_skipped` takes the directory lock
(`lock_dir(&self.dir, INBOX_LOCK)`, as `ack` does at line 383) for the
whole read-and-move, so a concurrent append or ack never interleaves;
without `ack_skipped` it reads without the lock, as `read` does. The
cursor moves forward only (the rule of `ack`).

A gap is durable in the run journal before any cursor move that would
hide it, the one ordering rule of every read. An unfiltered read moves no
cursor: the call path journals its gap (`journal_gap`, synced) before
the result goes back, and the reader's `inbox_ack` comes later. A
filtered read that acks writes the gap from inside `record_gap`, the
same `journal_gap` on the run the call belongs to
(`InboxCall::journal_gaps_to`), and the cursor moves only when that
write succeeded or found the gap already journaled. A journal failure
keeps the cursor where it was and the read still answers, with the gap
in its envelope, as a journal failure leaves an unfiltered read's result
unchanged; the next read finds the same gap and tries again. A crash
between the two leaves the gap journaled and the cursor unmoved, never
the reverse. A person's live call outside a run has no journal: its
cursor moves and the gap is only in the returned envelope.

### `inbox_read` takes the filter

`crates/apb-engine/src/connector/inbox.rs`:

1. `InboxCall` (line 47) gets `filter: EventFilter`.
2. `build` (line 69) reads `args.types` (an array of strings) and
   `args.where` (an array of `{ path, equals }`) with
   `serde_json::from_value` into `EventFilter`; a value that does not
   deserialize or fails `check` is `invalid_args` with the check's
   message. Either key on an `ack` or `peek_depth` call is `invalid_args`
   `"only op `read` takes types or where"`. The dry run's `inbox` object
   gets `"types"` and `"where"` when given.
3. `send` (line 170), `InboxOp::Read`: an empty filter reads as today;
   otherwise
   `store.read_filtered(&self.consumer, self.limit, &|e| self.filter.matches(e), ack_skipped)`
   with `ack_skipped = self.may_ack && !apb_core::process_mode::is_read_only()`
   (see "Who may move a cursor" below), the gap travels as today and is
   journaled before a cursor moves past it (see "A filtered read"),
   and the body is `filtered_envelope(&r.events, r.cursor, r.skipped)`.
4. New `pub fn filtered_envelope(events: &[InboxEvent], cursor: u64, skipped: u64) -> Value`:
   `read_envelope`'s object plus `"skipped": skipped`. `read_envelope`
   (line 230) keeps its exact shape, so the contract tests of every
   unfiltered read stay byte-equal. Each event row of both envelopes gets
   `"type"` when the event has one (`event_row`), and the dashboard's
   events view (`GET /api/connectors/<name>/inbox/<account>/events`)
   lists the same rows, so an operator sees the value a `types` filter
   matches.
5. A function accepts `types` and `where` only when its `args_schema`
   declares them, as for any argument.

### Who may move a cursor

`inbox_read` is declared `read_only: true`, so a grant of read-only
functions only (`functions: read_only`) and every engine site that calls
read-only functions only (an item check, a baseline or follow-up read,
the reconcile of a send) may call it. A filtered read that acked its
skips would let such a caller move any consumer's cursor, an effect that
grant refuses through `inbox_ack`. So a filtered read acks its skips only
when its caller may move the cursor anyway:

1. `build` takes `may_ack: bool` and `InboxCall` keeps it.
2. The call path (`connector/call/mod.rs`) decides it once, in
   `inbox_may_ack(origin, doc, grant)`: `true` only for a node's own call
   (`CallOrigin::Node`) whose grant names a function of the connector
   with `inbox: { op: ack }`; `false` for every engine origin
   (`Check`, `Reconcile`, `Notify`, `NotifyReconcile`) and for a call
   with no grant. A person's live call outside a run (`apb connector
   call`, the dashboard's playground) passes `true`: it may call the ack
   function itself. A notice and the healthcheck pass `false`.
3. A read with `may_ack` false (or in a read-only process) reads without
   the lock and moves nothing, and still reports `skipped` and the gap:
   the next read sees the skipped events again.

The function stays `read_only: true` whatever its arguments; the
alternative, classifying a filtered read as not read-only, would have
taken the read away from every read-only grant and check.

### Bundled connectors

1. `connectors/github/connector.yaml`: `version: 0.4.0`; account field
   `{ name: webhook_secret, secret: true, required: false }` after
   `token`; a webhook block after `identity`:
   ```yaml
   webhook:
     signature:
       scheme: hmac_sha256_hex
       header: X-Hub-Signature-256
       prefix: "sha256="
       secret: "{{secret.webhook_secret}}"
     event_type_header: X-GitHub-Event
   ```
   (GitHub resends a delivery with the same body, so the raw-body digest
   is its dedupe id; no `dedupe_path`), and the three inbox functions
   `inbox_read`, `inbox_ack`, `inbox_peek_depth` copied from
   `connectors/whatsapp/connector.yaml` lines 450 to 512 with their
   `examples` adapted (consumer `triage`), `inbox_read`'s `args_schema`
   adding
   ```yaml
   types: { type: array, items: { type: string }, description: "event types to keep (X-GitHub-Event), such as issues or pull_request" }
   where: { type: array, items: { type: object }, description: "conditions over the body: [{ path, equals }]" }
   ```
   and three cases in `connectors/github/tests.yaml` mirroring the
   whatsapp inbox cases (line 163 on). `README.md` of the connector gets a
   "Receiving events" paragraph (webhook URL, the secret, the
   `X-GitHub-Event` type).
2. `connectors/whatsapp/connector.yaml`: `version: 0.2.0`;
   `event_type_path: entry.0.changes.0.field` in the webhook block (line
   54); `types` and `where` in `inbox_read`'s `args_schema`.

### Surfaces and docs

1. `docs/CONNECTORS.md` "Receiving events (webhooks and the inbox)"
   (line 339): the two fields with an example each; `types` and `where` on
   `inbox_read` with the skip rule (leading skips acked for this consumer
   only, a later one left, the gap never hidden, `skipped` in the
   envelope); the `github` section (line 946) and the `whatsapp` section
   (line 1266) name their event types.
2. `apb connector doctor` (`crates/apb-cli/src/connector.rs` line 795)
   adds `"; event type from header X-GitHub-Event"` or `"from body path ..."`
   to the webhook line.
3. The dashboard's inbox panel shows a typed event's `type` next to its
   seq and time, as text (sender influenced, cleaned at ingest).

### Tests

1. Unit, `crates/apb-core/src/connector/event_filter.rs`:
   `an_untyped_event_never_passes_a_type_condition`,
   `a_condition_reads_a_dot_path`, `equals_null_needs_a_present_null`,
   `an_object_never_equals`, `check_refuses_an_object_equals`,
   `explain_never_carries_body_text` (a body string `"SECRET-BODY"` appears
   in no `ConditionResult`).
2. Unit, `inbox.rs`: `a_filtered_read_acks_only_the_leading_skips` (events
   1 skip, 2 match, 3 skip: the cursor moves to 1, the read returns 2,
   `skipped == 2`; after `ack(2)` the next read moves the cursor to 3),
   `a_filtered_read_leaves_other_consumers_alone` (consumer `b`'s cursor
   unchanged), `a_filter_never_hides_a_gap` (retention dropped 1 to 3,
   every remaining event skipped: the gap is still reported),
   `a_gap_is_recorded_before_the_cursor_moves_past_it` (the crash point:
   `record_gap` sees the stored cursor unmoved, and a refusal moves
   nothing),
   `an_old_event_reads_untyped` (a line
   without `event_type` parses).
3. Unit, `webhook.rs`: `event_type_prefers_the_header`,
   `clean_event_type_refuses_spaces_and_long_values`.
4. Unit, `def.rs`: `a_webhook_with_both_event_type_fields_is_refused`,
   `event_type_fields_refuse_placeholders`.
5. Integration, `crates/apb-engine/tests/suite/inbox_filter_test.rs` (new):
   `inbox_read_with_types_skips_and_acks_for_its_consumer` (two consumers
   over one account: the filtered one skips, the default one still reads
   every event), `inbox_read_with_where_returns_skipped`,
   `a_filter_on_ack_is_invalid_args`,
   `a_read_only_grant_moves_no_cursor` (a node granted `inbox_read` and
   `inbox_depth` only reports its skips and moves nothing),
   `a_filtered_read_reports_the_gap` (the gap at the top of the call
   result when every remaining event is skipped),
   `a_gap_is_journaled_before_the_cursor_moves_past_it` (a journal that
   cannot be written keeps the cursor and the gap in the envelope; once
   it writes, the gap is journaled once and the cursor moves),
   `a_read_only_process_moves_no_cursor` (`process_mode::enter_read_only`
   in a child process, as the read-only tests do: the call is refused and
   the cursor stays). Unit, `connector/call/tests.rs`:
   `only_a_node_granted_the_ack_function_may_ack_skipped_events` (every
   engine origin and a read-only grant pass `false`).
6. Server, `crates/apb-server/tests/suite/ingest_test.rs` (the existing
   ingest test file):
   `a_delivery_stores_its_header_type` and `a_delivery_stores_its_body_type`,
   and over the embedded bundled connectors
   `the_bundled_github_connector_stores_the_event_header_type` and
   `the_bundled_whatsapp_connector_stores_the_change_field_type` (a signed
   delivery to `/hooks/github/main` and `/hooks/whatsapp/main`);
   `crates/apb-server/tests/suite/inbox_api_test.rs`
   `the_events_endpoint_names_the_stored_type`. Web,
   `ConnectorInboxPanel.test.ts`: the type renders as escaped text and
   only for a typed event.
7. The bundled connector gates pass with the new github cases:
   `cargo test -p apb --test main official_connectors_gate` and
   `cargo test -p apb-core --test main bundled_content_lint_test`; the
   github engine suite, if one exists (`rg -l github crates/apb-engine/tests/suite`), too.

### Wrong implementations

1. One shared cursor for skips (moving `default` or the minimum cursor).
   Caught by `inbox_read_with_types_skips_and_acks_for_its_consumer`.
2. Acking every non-matching pending event, including those after an
   unacked match: a crash after the read then loses nothing visible, but a
   reader that never acks its match would have events past it acked.
   Caught by `a_filtered_read_acks_only_the_leading_skips`.
3. Computing the gap after the skip moved the cursor, so a skipped prefix
   that covers the gap hides it. Caught by `a_filter_never_hides_a_gap`.
4. Moving the cursor before the gap is in the run journal, so a crash in
   between loses the gap on resume. Caught by
   `a_gap_is_journaled_before_the_cursor_moves_past_it`.
5. Typing the event from the body for GitHub (there is no type field in
   its body). Caught by `a_delivery_stores_its_header_type`.
6. Storing the raw header value (spaces, 1 KB of text). Caught by
   `clean_event_type_refuses_spaces_and_long_values`.

## Code map

New files (none exists on 316c8180; checked):

| File | Decision |
|---|---|
| `crates/apb-core/src/connector/event_filter.rs` | D2 |
| `crates/apb-engine/tests/suite/inbox_filter_test.rs` | D2 |

The integration test file is declared in `crates/apb-engine/tests/main.rs`
with `#[path = "suite/<name>.rs"] mod <name>;` in alphabetical place (as
`crates/apb-engine/tests/main.rs` line 263 declares
`connector_contract_inbox`).

Changed files:

| File | What |
|---|---|
| `crates/apb-core/src/connector/mod.rs` | `pub mod event_filter;` |
| `crates/apb-core/src/connector/def.rs` | webhook event type fields and checks |
| `crates/apb-core/src/connector/inbox.rs` | `event_type`, `append_typed`, `FilteredRead`, `read_filtered` |
| `crates/apb-core/src/connector/webhook.rs` | `event_type`, `clean_event_type` |
| `crates/apb-core/src/connector/contract.rs` | the seeded event's `type`, the expected `skipped` |
| `crates/apb-core/src/connector/official.rs` | the github version the tests expect |
| `crates/apb-core/src/list/items.rs` | `field_equals` made `pub(crate)` |
| `crates/apb-core/tests/suite/inbox_store_test.rs` | the `append_bounded` callers that pass `None` |
| `crates/apb-engine/src/connector/inbox.rs` | filter args, `filtered_envelope`, the event's `type` |
| `crates/apb-engine/src/connector/call/mod.rs`, `call/tests.rs` | `inbox_may_ack`, who may move a cursor, the gap journal of an inbox call |
| `crates/apb-engine/src/connector/contract_test.rs` | a filtered read of a contract case |
| `crates/apb-engine/tests/suite/connector_inbox.rs` | the stored event's `event_type` |
| `crates/apb-engine/tests/main.rs` | the test module |
| `crates/apb-cli/src/connector.rs` | the event type on the webhook line |
| `crates/apb-server/src/ingest.rs` | event type, `append_typed` |
| `crates/apb-server/tests/suite/ingest_test.rs` | the typed delivery tests |
| `crates/apb-server/src/routes/connectors/inbox.rs`, `tests/suite/inbox_api_test.rs` | the events view's `type` |
| `web/src/lib/api/connectors.ts`, `web/src/lib/connectorinbox.ts` | the event row's `type` |
| `web/src/lib/components/ConnectorInboxPanel.svelte`, `.test.ts` | the type shown as text |
| `connectors/github/connector.yaml`, `tests.yaml`, `README.md` | D2 |
| `connectors/whatsapp/connector.yaml`, `tests.yaml` | D2 |
| `docs/CONNECTORS.md` | docs |
| `CLAUDE.md`, `AGENTS.md` | the code map line |
| `docs/superpowers/specs/2026-11-18-typed-inbox-reads-design.md` | the public record |

`crates/apb-server/src/ts_contract.rs` does not change: `ConditionResult`
keeps its `ts` derive for the next change but is not registered here (no
surface of this change returns it), so no generated TypeScript changes.

The count is 38 files: 2 new code and test files, 29 changed code,
content, test and doc files (the connector files counted one by one), the
spec folder (`typed-inbox-reads`) and 6 records (`proposal.md`,
`design.md`, `tasks.md`, the public record, `CLAUDE.md`, `AGENTS.md`).
The dependency updates carried with this change (proposal "Impact" item 5)
are counted apart.

## Release notes

The change is not tagged here. List in the final report:
1. `inbox_read` accepts `types` and `where`; a filtered read
   acknowledges the events it skips for its own consumer only, and only
   for a caller granted the ack function too (a read-only grant, an engine
   check and a read-only process move no cursor), reports `skipped` and
   never hides a gap; every stored event keeps its type.
2. A connector's `webhook:` block may name the event type
   (`event_type_header` or `event_type_path`).
3. The bundled github connector 0.4.0 receives signed webhooks typed by
   `X-GitHub-Event` and offers `inbox_read`, `inbox_ack` and
   `inbox_peek_depth`; whatsapp 0.2.0 types its deliveries.

## Implementation notes

Where building the design met the code, and what was done:

1. `Inbox::read_filtered` takes the matcher as
   `&impl Fn(&InboxEvent) -> bool`, not `&EventFilter`: the filter reads
   `InboxEvent`, so a typed parameter makes the two files import each
   other, which the dependency rule refuses. The engine passes
   `&|e| filter.matches(e)`.
2. The `append_bounded` callers that pass `None` live in
   `crates/apb-core/tests/suite/inbox_store_test.rs`.
3. The doctor's ingest line puts the header or the path in backticks, as
   the line already does for the signature header.
4. A read in a read-only process is refused at the call gate before it
   reads; the test goes through `execute` and asserts the refusal and an
   unchanged cursor.
5. `read_filtered` stops acking at the first match it sees, returned or
   not, so a limit that keeps a match out of the page never acks past it.
6. `EventFilter::matches` decides without building the `explain`
   results; a test pins that the two agree.
7. The decision "Who may move a cursor" above was added after review: a
   filtered read under a read-only grant or from an engine site moves no
   cursor.
8. The contract-test runner reads a seeded event's `type` and checks
   `skipped` on a filtered read (with no acknowledgement), so the
   bundled github and whatsapp cases pin typed reads through each
   function's own `response_pick`. The whatsapp `inbox_read` keeps
   `skipped` in its pick.
9. A type read from a header is not authenticated when the provider
   signs the body only (GitHub does); the docs tell a reader to check
   the body fields it relies on with `where`.
10. After review: a filtered read that acks journals its gap (synced)
   before the cursor moves and keeps the cursor when the journal cannot
   take it; the dashboard's events view and panel show the stored type.
