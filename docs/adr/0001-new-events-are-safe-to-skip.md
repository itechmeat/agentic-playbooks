---
decided: 2026-10-28
decision-makers: apb maintainers
---

# A new event type is safe to skip up to the next checkpoint; new fields are `#[serde(default)]`

## Applies to

Every change to `EventPayload` in `crates/apb-engine/src/event.rs`, the
run journal (`runs/<id>/events.jsonl`) and every reader of it: the drive
loop, resume, the folds, the run surfaces, the dashboard.

- A new field of a known event is added only with `#[serde(default)]`; a
  structured optional field decodes leniently, so a shape a newer apb wrote
  reads as absent.
- A new event type is written only where a checkpoint
  (`EventPayload::is_checkpoint`: `node_finished`, `run_paused`,
  `run_finished`, `run_aborted`) always follows it, and the engine's state
  does not depend on it.
- A known event is never replaced by a new type; the new one is added
  alongside it.

## Why

An older apb binary may read a journal a newer one wrote: an operator
downgrades, or two installs share a project. `event::read_all` skips and
counts an event type it does not know, and continues a run only while a
checkpoint follows every such event, because whatever the event meant was
settled by then. An event the engine needs to continue, or a field without a
default, would make the older binary act on a journal it read only in part,
or refuse it outright.

Stopping a run is the one exception: `event::read_all_for_stop` lets it
through an unknown event newer than the last checkpoint, since the stop only
appends `run_aborted`, itself a checkpoint.

## Checked by

- apb-engine `journal_compat_test::unknown_event_types_are_skipped_and_counted`
- apb-engine `journal_compat_test::an_unknown_event_after_the_last_checkpoint_stops_the_engine`
- One round-trip and skip test per additive event in the same file, such as
  `journal_compat_test::tool_exposure_mismatch_round_trips_and_is_skippable_before_a_checkpoint`.
