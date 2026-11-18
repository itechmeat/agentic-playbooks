# Proposal: typed inbox reads

## Why

This change is the third of the ten 1.0 feature changes. It has no hard
predecessor: nothing it touches is moved by `people-in-the-loop` or
`before-running-alone`. The next change, `start-rules-and-schedules`,
builds on it: a start rule's filter is this change's event filter. Every
file:line below was read on main 316c8180 (1.0.0-beta.7).

Released timer waits (a long timer wait releasing its driver) were
designed together with this change and are split out of it into a later
change of their own; nothing here depends on them.

The gap: an inbox read cannot ask for one kind of event. `inbox_read`
takes only `consumer` and `limit`
(`crates/apb-engine/src/connector/inbox.rs` `build`, line 69), a stored
event has no type (`crates/apb-core/src/connector/inbox.rs` `InboxEvent`,
line 79), and the webhook block knows only the dedupe path
(`crates/apb-core/src/connector/def.rs` `WebhookSpec`, line 292). A
reader that wants new issues gets every comment, label and push too, and
has to ack them itself.

| Decision | Code example | Non-code example |
|---|---|---|
| D2 typed inbox reads | the github connector stores an `issues` delivery typed from `X-GitHub-Event`, and an agent's `inbox_read` with `types: [issues]` and `where: [{ path: action, equals: opened }]` skips pushes and comments for its own consumer only | a CRM inbox read with `where: [{ path: status, equals: new }]` returns only new leads; another consumer of the same account still sees every update |

## What changes

Typed, filtered inbox reads (capability `typed-inbox-reads`): the
webhook block gains `event_type_path` or `event_type_header`; ingest
stores `event_type` on each event; `inbox_read` takes `types` and
`where`, skips non-matching events and acks them for that consumer
only (and only for a caller granted the ack function too), and never
hides a gap. The bundled `github` connector gains a
webhook block (`X-GitHub-Event`) and its three inbox functions; the
bundled `whatsapp` connector gains `event_type_path`.

## Code that differs from the plan

1. The plan's example "the shipped github connector's issues.opened
   delivery" assumes a webhook block the bundled github connector does not
   have (`connectors/github/connector.yaml` has none; only
   `connectors/whatsapp/connector.yaml` line 54 does). GitHub sends the
   event type in a header, not the body, so `event_type_path` alone
   cannot type it. Decided: `event_type_header` next to
   `event_type_path`, and a webhook block for github in this change.
2. The plan's `where` grammar (`crates/apb-core/src/list/items.rs` line 2)
   is a single top-level `field` and `equals`
   (`crates/apb-core/src/list/mod.rs` `ItemFilter`, line 134). A body
   condition needs a dot path: `inbox_read` takes a list of
   `{ path, equals }` read with `connector::common::value_at_path` (line
   117) and compared by the list filter's own `field_equals`
   (`items.rs` line 240), made `pub(crate)`.

## Out of scope

1. Start rules (a stored delivery starts a run) and schedules: the next
   change, `start-rules-and-schedules`. A stored delivery still starts
   nothing here; `docs/CONNECTORS.md` line 413 stays as it is.
2. The inbox helpers only rules need (`forget_consumer`, `head_seq`,
   `event`): `start-rules-and-schedules`.
3. Waits that match inbox events (`wait_for: { type: inbox }`): the change
   `evals-and-inbox-waits`, which reuses this change's event filter and the
   stored event type.
4. Released timer waits: a later change of their own. A timer wait keeps
   its driver here, as it does on main.

## Impact

1. Crates: `apb-core` (new `connector/event_filter.rs` with
   `EventFilter`, `BodyCondition`, `ConditionResult`;
   `connector::inbox` `event_type`, `append_typed`, `read_filtered`;
   `connector::def` webhook fields; `connector::webhook` `event_type`,
   `clean_event_type`; `connector::contract` the seeded event type and
   the expected `skipped`; `list::items::field_equals` made
   `pub(crate)`), `apb-engine` (the `inbox_read` filter in
   `connector/inbox.rs`, who may ack in `connector/call`, the
   contract-test runner in `connector/contract_test.rs`), `apb` CLI
   (`apb connector doctor`'s webhook line), `apb-server` (ingest typing).
2. Bundled content: `connectors/github/connector.yaml` 0.3.0 to 0.4.0
   (webhook block, `webhook_secret` account field, inbox functions) and
   `connectors/whatsapp/connector.yaml` 0.1.0 to 0.2.0
   (`event_type_path`, `types` and `where` on `inbox_read`). These are
   content versions, not an apb version bump.
3. Journal: none. The inbox event gains `event_type` (additive, absent on
   every older line).
4. New state: none. No new dependency, no validator code.
5. Carried with this change: dependency updates (`Cargo.lock`,
   `web/package.json`, `web/bun.lock`, `THIRD-PARTY-NOTICES.txt`
   regenerated), the project's `branch-quality-review` playbook 1.5.1 under
   `.apb/`, and the zg 0.3.0 command syntax in `CLAUDE.md` and
   `AGENTS.md`.
6. Files: 32 (design.md "Code map"), with the 8 more files of item 5,
   40 in all.
7. Behaviour changes for release notes:
   1. `inbox_read` accepts `types` and `where`; a filtered read
      acknowledges the events it skips for its own consumer only, and
      only for a caller granted the ack function too.
   2. A connector's webhook block may name the event type; the github
      connector 0.4.0 receives webhooks and whatsapp 0.2.0 types them.
