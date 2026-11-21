//! The list skip (`list.skip`, incremental lists D2): the one earlier run
//! a list node's set is read from, the set every visit of the run reads
//! from its `list_skip_read` line instead of the history, the skip digest
//! an item passes with and the patch refusal. The record types live in the
//! leaf [`super::skip_record`], which the journal and the views read.

use std::path::Path;

use apb_core::list::verdict::ItemState;
use apb_core::list::{LIST_SKIP_MAX_BYTES, LIST_SKIP_MAX_ENTRIES};
use apb_core::schema::Playbook;

use super::ledger::ListFold;
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, PreviousSuccess};
use crate::run_config::RunConfig;

/// The refusal of a run whose sets are partly recorded.
pub const INCOMPLETE: &str = "list_skip_incomplete";
/// The refusal of a set read whose history does not read.
pub const UNREADABLE: &str = "list_skip_unreadable";
/// The refusal of a set write whose journal lock cannot be taken.
pub const LOCK_UNAVAILABLE: &str = "list_skip_lock_unavailable";
/// The set-time refusal of a run whose type cannot be told after a crash.
pub const RUN_TYPE_UNKNOWN: &str = "skip_run_type_unknown";
/// A visit's refusal while a supervisor note is in force.
pub const SUPERVISOR_NOTE: &str = super::ledger::SUPERVISOR_NOTE;
/// A visit's refusal when the live scripts differ from the pin.
pub const SCRIPTS_CHANGED: &str = "skip_scripts_changed";
/// The failure of a visit whose scripts changed before its skip
/// settlements were written.
pub const SCRIPTS_CHANGED_FAIL: &str = "list_skip_scripts_changed";
/// The patch and rebind refusal once a run settled items by a skip.
pub const WORK_CHANGED: &str = "list_skip_work_changed";
/// A visit's refusal when a field `unless_changed` names is in no item.
pub const UNLESS_CHANGED_ABSENT: &str = "skip_unless_changed_absent";
/// Why a visit stopped stamping: its scripts copy differed from the pin at
/// an item's start or settlement.
pub const SCRIPTS_DIVERGED: &str = "skip_scripts_diverged";
/// The reason a skipped row of a continued or reopened visit goes back to
/// pending: its skip digest no longer matches the work.
pub const SKIP_CONFIG_CHANGED: &str = super::ledger::SKIP_CONFIG_CHANGED;
/// Why a visit stopped stamping: the machine's MCP tool approvals for a
/// granted account changed while it ran.
pub const MCP_PINS_CHANGED: &str = "skip_mcp_pins_changed";
/// Why a visit skips nothing or stops stamping: a runtime its work names
/// is no longer the file the run resolved (replaced at the same path).
pub const RUNTIME_CHANGED: &str = "skip_runtime_changed";
/// The set-time refusal of a source run that, on the read the sets come
/// from, is no longer an earlier success the skip may read.
pub const SOURCE_INELIGIBLE: &str = "skip_source_ineligible";
/// The refusal of a set write on a full disk.
pub const DISK_FULL: &str = "list_skip_disk_full";
/// How long the set write waits for the journal lock before it refuses.
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// The rebind refusal when the work cannot be compared.
pub const WORK_UNKNOWN: &str = "list_skip_work_unknown";

pub use super::skip_record::{SkipSet, SkipView};

/// Whether a run journaled the sets of its definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// Every list node with `skip` has its line (or the definition has
    /// none).
    All,
    /// None has.
    None,
    /// Some have, some have not: a reader refuses it.
    Partial {
        recorded: Vec<String>,
        missing: Vec<String>,
    },
}

/// The list nodes of `playbook` that declare `skip`, in definition order.
pub(crate) fn skipping_nodes(playbook: &Playbook) -> Vec<&str> {
    playbook
        .nodes
        .iter()
        .filter(|n| n.list().is_some_and(|l| l.skip.is_some()))
        .map(|n| n.id.as_str())
        .collect()
}

/// Whether any list node of `playbook` declares `skip`.
pub(crate) fn declares_skip(playbook: &Playbook) -> bool {
    !skipping_nodes(playbook).is_empty()
}

/// Every list node with `skip` -> an empty set.
pub(crate) fn empty_sets(playbook: &Playbook) -> Vec<(String, SkipSet)> {
    skipping_nodes(playbook)
        .into_iter()
        .map(|n| (n.to_string(), SkipSet::default()))
        .collect()
}

/// Every list node with `skip` -> a set refused for `reason`.
fn refused_sets(playbook: &Playbook, reason: &str) -> Vec<(String, SkipSet)> {
    skipping_nodes(playbook)
        .into_iter()
        .map(|n| {
            (
                n.to_string(),
                SkipSet {
                    refused: Some(reason.to_string()),
                    ..SkipSet::default()
                },
            )
        })
        .collect()
}

/// What a skipped item carries of `output`, as a source settlement recorded
/// it: `Some(None)` for a source item that produced no output object (it
/// carries none, as it had none), `Some(Some(o))` for an output that was
/// not clipped at settlement, holds no redaction marker (the scrubbed text
/// is not what the item produced) and is within the per-item cap; `None`
/// when it cannot be carried, so the item is worked.
fn carried_output(output: Option<&String>) -> Option<Option<&String>> {
    let Some(o) = output else {
        return Some(None);
    };
    let withheld = o == r#"{"truncated":true}"#
        || o.contains("[redacted")
        || o.contains(crate::run_redact::WITHHELD);
    (!withheld && o.len() <= apb_core::list::LIST_SKIP_OUTPUT_MAX_BYTES).then_some(Some(o))
}

/// The set of `node` in the source journal `events`: every passed item of
/// its latest visit that carries a skip digest and an output it can carry
/// ([`carried_output`]), with that output, in id order, up to both caps
/// (`LIST_SKIP_MAX_ENTRIES` entries, `LIST_SKIP_MAX_BYTES` for ids, digests
/// and outputs together). A pass whose output cannot be carried is counted
/// in `not_inherited` and left out, so it is worked.
pub(crate) fn set_of(events: &[Event], node: &str, source: &str) -> SkipSet {
    let mut set = SkipSet {
        source: Some(source.to_string()),
        ..SkipSet::default()
    };
    let Some(fold) = ListFold::of(events, node) else {
        return set;
    };
    let mut bytes = 0usize;
    for (id, r) in &fold.items {
        if r.state != Some(ItemState::Passed) {
            continue;
        }
        let Some(digest) = &r.skip_digest else {
            continue;
        };
        let Some(output) = carried_output(r.output.as_ref()) else {
            set.not_inherited += 1;
            continue;
        };
        // One entry as the line holds it: `"<id>":"<digest>",` in the items
        // and `"<id>":"<output>",` in the outputs.
        let id_len = serde_json::to_string(id).map_or(0, |s| s.len());
        let entry = id_len
            + serde_json::to_string(digest).map_or(0, |s| s.len())
            + 2
            + output.map_or(0, |o| {
                id_len + serde_json::to_string(o).map_or(0, |s| s.len()) + 2
            });
        if set.items.len() >= LIST_SKIP_MAX_ENTRIES || bytes + entry > LIST_SKIP_MAX_BYTES {
            set.truncated = true;
            break;
        }
        bytes += entry;
        set.items.insert(id.clone(), digest.clone());
        if let Some(o) = output {
            set.outputs.insert(id.clone(), o.clone());
        }
    }
    set
}

/// Every list node with `skip` -> its set, read from the one earlier run:
/// the run `window` names when the run recorded its `run.previous_success`
/// window, else the run `previous_success::find` returns. `Err`
/// (`list_skip_unreadable`) when that lookup or the source's journal does
/// not read.
pub(crate) fn read_sets(
    root: &Path,
    playbook: &Playbook,
    playbook_id: &str,
    scope_label: &str,
    window: Option<&PreviousSuccess>,
) -> Result<Vec<(String, SkipSet)>, EngineError> {
    let unreadable = |run: &str, what: &str| {
        EngineError::Invalid(format!(
            "{UNREADABLE}: run {run}: its {what} does not read; repair or remove it, then start again"
        ))
    };
    let source = match window {
        Some(w) => w.run_id.as_ref().map(|id| root.join(".apb/runs").join(id)),
        None => crate::previous_success::find(root, playbook_id, scope_label)
            .map_err(|h| unreadable(&h.run, h.what))?,
    };
    let Some(dir) = source else {
        return Ok(empty_sets(playbook));
    };
    sets_from(&dir, playbook)
}

/// Every skipping node's set from the run in `dir`: its journal read once,
/// judged on that read and consumed from it. The one step both the window
/// path and the lookup path of [`read_sets`] end in.
fn sets_from(dir: &Path, playbook: &Playbook) -> Result<Vec<(String, SkipSet)>, EngineError> {
    let unreadable = |run: &str, what: &str| {
        EngineError::Invalid(format!(
            "{UNREADABLE}: run {run}: its {what} does not read; repair or remove it, then start again"
        ))
    };
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let events = crate::event::read_all(dir).map_err(|_| unreadable(&id, "journal"))?;
    // The one read the sets come from is judged on itself, on both paths:
    // the run a window names and the run `find` approved on its own read
    // are each checked again on this journal (made here, a production run
    // others may learn from, top level, still settled succeeded). A source
    // that no longer is gives no skip: every skipping node records the
    // refusal and works every item.
    if let Err(why) = eligible_source(dir, &events) {
        let refused = format!(
            "{SOURCE_INELIGIBLE}: run {id} is no longer an earlier success the skip may read ({why}), so every item works"
        );
        return Ok(skipping_nodes(playbook)
            .into_iter()
            .map(|n| {
                (
                    n.to_string(),
                    SkipSet {
                        refused: Some(refused.clone()),
                        ..SkipSet::default()
                    },
                )
            })
            .collect());
    }
    Ok(skipping_nodes(playbook)
        .into_iter()
        .map(|n| (n.to_string(), set_of(&events, n, &id)))
        .collect())
}

/// Why the run in `dir`, whose journal is `events`, is not a source the
/// skip may read: the predicates of `previous_success::find`.
fn eligible_source(dir: &Path, events: &[Event]) -> Result<(), &'static str> {
    if !crate::decision::report::created_here(dir) {
        return Err("it was not made on this machine");
    }
    if crate::manifest::read(dir).is_err() || !crate::external_data::learns_from(dir) {
        return Err("it is not a production run others learn from");
    }
    match crate::run_config::read_run_config(dir) {
        Ok(cfg) if cfg.parent_run.is_none() => {}
        _ => return Err("it is not a top-level run"),
    }
    if !crate::previous_success::settled_succeeded(events) {
        return Err("it is not settled succeeded");
    }
    Ok(())
}

/// The set this run journaled for `node` (its `list_skip_read` naming
/// it); `None` when it journaled none.
pub(crate) fn recorded(events: &[Event], node: &str) -> Option<SkipSet> {
    super::ledger::recorded_skip_set(events, node)
}

/// Whether this run journaled the sets of its definition.
pub(crate) fn coverage(events: &[Event], playbook: &Playbook) -> Coverage {
    let (recorded, missing): (Vec<&str>, Vec<&str>) = skipping_nodes(playbook)
        .into_iter()
        .partition(|n| recorded(events, n).is_some());
    match (recorded.is_empty(), missing.is_empty()) {
        (_, true) => Coverage::All,
        (true, false) => Coverage::None,
        (false, false) => Coverage::Partial {
            recorded: recorded.into_iter().map(str::to_string).collect(),
            missing: missing.into_iter().map(str::to_string).collect(),
        },
    }
}

/// The refusal of a partial coverage, naming whose run when it is not this
/// one.
fn incomplete(of: Option<&str>, recorded: &[String], missing: &[String]) -> EngineError {
    let whose = of.map_or_else(|| "the run".to_string(), |r| format!("run {r}"));
    EngineError::Invalid(format!(
        "{INCOMPLETE}: {whose} recorded the skip sets of {} but not of {}; start a new run",
        recorded.join(", "),
        missing.join(", ")
    ))
}

/// The sets of a run that continues `pred`, whose journal is
/// `pred_events` and definition `pred_playbook` (`None` when its
/// snapshot does not read): a node the predecessor recorded keeps its set,
/// any other node an empty one. A predecessor whose sets are partly
/// recorded refuses the continuation.
pub(crate) fn continued_sets(
    pred: &str,
    pred_events: &[Event],
    pred_playbook: Option<&Playbook>,
    playbook: &Playbook,
) -> Result<Vec<(String, SkipSet)>, EngineError> {
    // Without the predecessor's definition its coverage cannot be told:
    // refused, never copied in part.
    let Some(pp) = pred_playbook else {
        return Err(EngineError::Invalid(format!(
            "{INCOMPLETE}: run {pred}: its definition snapshot does not read, so the skip sets it recorded cannot be checked; start a new run"
        )));
    };
    if let Coverage::Partial { recorded, missing } = coverage(pred_events, pp) {
        return Err(incomplete(Some(pred), &recorded, &missing));
    }
    Ok(skipping_nodes(playbook)
        .into_iter()
        .map(|n| (n.to_string(), recorded(pred_events, n).unwrap_or_default()))
        .collect())
}

/// The predecessor's journal and definition for [`continued_sets`].
pub(crate) fn continued_from(
    root: &Path,
    pred: &str,
    playbook: &Playbook,
) -> Result<Vec<(String, SkipSet)>, EngineError> {
    let dir = root.join(".apb/runs").join(pred);
    let events = crate::event::read_all(&dir)?;
    continued_sets(
        pred,
        &events,
        predecessor_definition(&dir).as_ref(),
        playbook,
    )
}

/// The definition snapshot of the run in `dir`, `None` when it does not
/// read.
pub(crate) fn predecessor_definition(dir: &Path) -> Option<Playbook> {
    std::fs::read_to_string(dir.join("playbook.yaml"))
        .ok()
        .and_then(|y| Playbook::from_yaml(&y).ok())
}

/// Writes `sets` as one group under the required journal lock, unless
/// another writer recorded them first. A lock that cannot be taken refuses
/// the write, and so does a partial record found under the lock
/// (`list_skip_incomplete`): nothing is appended to it.
pub(crate) fn write_sets(
    log: &mut EventLog,
    playbook: &Playbook,
    sets: Vec<(String, SkipSet)>,
) -> Result<(), EngineError> {
    let mut partial = None;
    log.append_group_decided(LOCK_WAIT, |events| match coverage(events, playbook) {
        Coverage::None => sets
            .into_iter()
            .map(|(node, set)| EventPayload::ListSkipRead { node, set })
            .collect(),
        // Another writer recorded them first: append nothing.
        Coverage::All => Vec::new(),
        // A partial record is refused, never completed.
        Coverage::Partial { recorded, missing } => {
            partial = Some(incomplete(None, &recorded, &missing));
            Vec::new()
        }
    })
    .map_err(lock_refusal)?;
    partial.map_or(Ok(()), Err)
}

/// The refusal of a set write by what failed: the journal lock that could
/// not be taken within [`LOCK_WAIT`] (`list_skip_lock_unavailable`), a full
/// disk (`list_skip_disk_full`); any other error as it is.
fn lock_refusal(e: EngineError) -> EngineError {
    match e {
        EngineError::Conflict(why) if why.starts_with(crate::event::JOURNAL_LOCKED) => {
            EngineError::Invalid(format!(
                "{LOCK_UNAVAILABLE}: the journal lock could not be taken, so the skip sets were not recorded ({why})"
            ))
        }
        EngineError::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => {
            EngineError::Invalid(format!(
                "{DISK_FULL}: the disk is full, so the skip sets were not recorded ({io})"
            ))
        }
        other => other,
    }
}

/// Whether the run in `run_dir` may skip at all: not a child run, not an
/// eval and not a scenario run (those read no history and get empty sets).
/// The one choice [`ensure_recorded`] and the drive's scripts protection
/// make.
pub(crate) fn may_skip(cfg: &RunConfig, run_dir: &Path) -> bool {
    cfg.parent_run.is_none()
        && cfg.eval.is_none()
        && !crate::scenario::session::is_scenario(run_dir)
}

/// Records the sets of a run that declares `skip` and journaled none, at
/// the entry of a drive: the same choice as preparation, failing closed on
/// a run type that cannot be told after a crash, written as one group. A
/// no-op when all are recorded; `Err(list_skip_incomplete)` when only some
/// are.
pub(crate) fn ensure_recorded(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
    cfg: &RunConfig,
    log: &mut EventLog,
) -> Result<(), EngineError> {
    record_missing(run_dir, playbook, log, |events| {
        if !may_skip(cfg, run_dir) {
            // Repeatable runs and children read no history.
            return Ok(empty_sets(playbook));
        }
        // Fail closed on the run type: only a manifest that reads and
        // names a production run reads the history.
        let origin = match crate::manifest::read(run_dir) {
            Ok(Some(m)) => Some(m.origin()),
            Ok(None) | Err(_) => None,
        };
        match origin {
            None => Ok(refused_sets(
                playbook,
                &format!(
                    "{RUN_TYPE_UNKNOWN}: the run's origin cannot be told after a crash, so its skip reads no history"
                ),
            )),
            // An external-data run reads the history like any run, as
            // preparation does; it only never becomes a source itself.
            Some(o) if !o.is_production() && o != crate::manifest::RunOrigin::ExternalData => {
                Ok(empty_sets(playbook))
            }
            Some(_) => history_sets(root, playbook, cfg, events),
        }
    })
}

/// Records the sets of a run that waited for its concurrency key, once it
/// got it: such a run is a top-level production run by construction (a
/// child, a scenario and an eval take no key), so its sets come from the
/// history as it stands now, with the window this run recorded.
pub(crate) fn record_after_turn(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
    cfg: &RunConfig,
    log: &mut EventLog,
) -> Result<(), EngineError> {
    record_missing(run_dir, playbook, log, |events| {
        history_sets(root, playbook, cfg, events)
    })
}

/// The sets a top-level production run reads: its predecessor's for a
/// continued run, else the one earlier run's.
fn history_sets(
    root: &Path,
    playbook: &Playbook,
    cfg: &RunConfig,
    events: &[Event],
) -> Result<Vec<(String, SkipSet)>, EngineError> {
    match cfg.continued_from.as_deref() {
        Some(pred) => continued_from(root, pred, playbook),
        None => {
            let (playbook_id, scope) = run_identity(events);
            read_sets(
                root,
                playbook,
                &playbook_id,
                &scope,
                crate::previous_success::recorded(events).as_ref(),
            )
        }
    }
}

/// Writes the sets `choose` makes of the journal when the run declares
/// `skip` and journaled none; refuses a partial record.
fn record_missing(
    run_dir: &Path,
    playbook: &Playbook,
    log: &mut EventLog,
    choose: impl FnOnce(&[Event]) -> Result<Vec<(String, SkipSet)>, EngineError>,
) -> Result<(), EngineError> {
    if !declares_skip(playbook) {
        return Ok(());
    }
    let events = crate::event::read_all(run_dir)?;
    match coverage(&events, playbook) {
        Coverage::All => Ok(()),
        Coverage::Partial { recorded, missing } => Err(incomplete(None, &recorded, &missing)),
        Coverage::None => {
            let sets = choose(&events)?;
            write_sets(log, playbook, sets)
        }
    }
}

/// The playbook id and scope label a run was started under.
pub(crate) fn run_identity(events: &[Event]) -> (String, String) {
    let id = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let scope = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { origin, .. } => origin.clone(),
            _ => None,
        })
        .unwrap_or_else(|| "project".to_string());
    (id, scope)
}

/// The skip digest of one item: SHA-256 hex over the canonical JSON of
/// `{"id", "item", "work"}`.
pub(crate) fn skip_digest(id: &str, item_part: &str, work: &str) -> String {
    // `serde_json` keeps object keys sorted, so the compact form is
    // canonical.
    let doc = serde_json::json!({ "id": id, "item": item_part, "work": work });
    apb_core::content::sha256_hex(doc.to_string().as_bytes())
}

/// Whether the run settled any item by a skip.
pub(crate) fn has_skip_settlements(events: &[Event]) -> bool {
    events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::ListItemSettled {
                skipped_from: Some(_),
                ..
            }
        )
    })
}

/// Why a patch from `old` to `new` must be refused, if it must: `new`
/// has a skipping list the run recorded no set for, or the run already
/// settled items by a skip (a patch changes the definition, so the work
/// those items were skipped under).
pub(crate) fn patch_refusal(_old: &Playbook, new: &Playbook, events: &[Event]) -> Option<String> {
    if let Some(n) = skipping_nodes(new)
        .into_iter()
        .find(|n| recorded(events, n).is_none())
    {
        return Some(format!(
            "list_skip_unrecorded: node {n} has no skip set recorded at start; start a new run instead"
        ));
    }
    has_skip_settlements(events).then(|| {
        format!(
            "{WORK_CHANGED}: the run settled items by a list skip; a patch changes its definition and so the work those items were skipped under; start a new run instead"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source the lookup approved and then reopened: its run was made
    /// here, is a production top-level run, finished succeeded, and was then
    /// resumed into another visit of the list that stamped an item. The read
    /// the sets come from judges that very journal: no longer settled
    /// succeeded, it gives no skip and nothing of its new visit.
    #[test]
    fn a_source_reopened_after_the_lookup_gives_no_skip() {
        apb_core::process_mode::testing::in_plain_child(
            "list::skip::tests::a_source_reopened_after_the_lookup_gives_no_skip",
            || {
                let cfg = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
                apb_core::run_origin::ensure_key().unwrap();
                let runs = tempfile::tempdir().unwrap();
                let dir = runs.path().join("r-src");
                std::fs::create_dir_all(&dir).unwrap();
                let line = |seq: u64, v: serde_json::Value| {
                    let mut v = v;
                    v["seq"] = seq.into();
                    v["ts"] = 0.into();
                    format!("{v}\n")
                };
                let settled = |seq: u64, item: &str, d: &str| {
                    line(
                        seq,
                        serde_json::json!({"type": "list_item_settled", "node": "a",
                        "item": item, "state": "passed", "round": 0, "skip_digest": d,
                        "output": "{}"}),
                    )
                };
                let mut j = String::new();
                j += &line(
                    1,
                    serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
                );
                j += &line(
                    2,
                    serde_json::json!({"type": "run_provenance", "origin": "project"}),
                );
                j += &line(
                    3,
                    serde_json::json!({"type": "list_started", "node": "a", "total": 1}),
                );
                j += &settled(4, "x", "d-first");
                j += &line(
                    5,
                    serde_json::json!({"type": "run_finished", "outcome": "succeeded"}),
                );
                let finished = j.clone();
                // Reopened after the lookup approved it, a new visit stamped.
                j += &line(
                    6,
                    serde_json::json!({"type": "run_resumed", "from_node": "a"}),
                );
                j += &line(
                    7,
                    serde_json::json!({"type": "list_started", "node": "a", "total": 1}),
                );
                j += &settled(8, "x", "d-reopened");
                let m = crate::manifest::RunExecutionManifest {
                    origin: Some(crate::manifest::RunOrigin::Production),
                    ..Default::default()
                };
                crate::manifest::write(&dir, &m).unwrap();
                apb_core::run_origin::stamp(&dir, "r-src").unwrap();
                // The control: settled succeeded, the set is read.
                std::fs::write(dir.join("events.jsonl"), &finished).unwrap();
                let sets = sets_from(&dir, &two_lists()).unwrap();
                assert_eq!(
                    sets[0].1.items.get("x").map(String::as_str),
                    Some("d-first")
                );
                assert!(sets[0].1.refused.is_none());
                std::fs::write(dir.join("events.jsonl"), &j).unwrap();
                let sets = sets_from(&dir, &two_lists()).unwrap();
                assert!(sets.iter().all(|(_, s)| s.items.is_empty()), "{sets:?}");
                assert!(
                    sets.iter().all(|(_, s)| s
                        .refused
                        .as_deref()
                        .is_some_and(|r| r.starts_with(SOURCE_INELIGIBLE))),
                    "{sets:?}"
                );
            },
        );
    }

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload,
        }
    }

    fn passed(seq: u64, item: &str, digest: Option<&str>) -> Event {
        ev(
            seq,
            EventPayload::ListItemSettled {
                node: "l".into(),
                item: item.into(),
                state: "passed".into(),
                verdict: None,
                note: None,
                reason: None,
                output: Some("{}".into()),
                round: 0,
                carried: false,
                check: None,
                digest: None,
                skipped_from: None,
                skip_digest: digest.map(str::to_string),
                skip_latched: None,
                result: None,
            },
        )
    }

    fn started(seq: u64) -> Event {
        ev(
            seq,
            EventPayload::ListStarted {
                node: "l".into(),
                total: 0,
                filtered_out: 0,
                items_digest: String::new(),
                batch_size: 1,
                parallel: 1,
                reopened: false,
                skip_refused: None,
            },
        )
    }

    fn two_lists() -> Playbook {
        Playbook::from_yaml(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s.sh, runner: sh, list: { items: { inline: [x] }, skip: { passed_in: previous_success } } }\n  - { id: b, type: script, script: s.sh, runner: sh, list: { items: { inline: [x] }, skip: { passed_in: previous_success } } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n",
        )
        .unwrap()
    }

    #[test]
    fn the_set_keeps_entries_up_to_both_caps() {
        // The entry cap.
        let mut events = vec![started(0)];
        for i in 0..(LIST_SKIP_MAX_ENTRIES + 5) {
            events.push(passed(i as u64 + 1, &format!("i{i:05}"), Some("d")));
        }
        let set = set_of(&events, "l", "r0");
        assert_eq!(set.items.len(), LIST_SKIP_MAX_ENTRIES);
        assert!(set.truncated);
        assert_eq!(set.items.keys().next().map(String::as_str), Some("i00000"));
        // The byte cap.
        let long = "x".repeat(1000);
        let mut events = vec![started(0)];
        for i in 0..400 {
            events.push(passed(i + 1, &format!("i{i:03}"), Some(&long)));
        }
        let set = set_of(&events, "l", "r0");
        assert!(set.truncated);
        let bytes: usize = set.items.iter().map(|(k, v)| k.len() + v.len() + 6).sum();
        assert!(bytes <= LIST_SKIP_MAX_BYTES, "{bytes}");
        assert!(serde_json::to_string(&set).unwrap().len() < 200 * 1024);
        // A pass without a stamp never enters a set.
        let set = set_of(&[started(0), passed(1, "a", None)], "l", "r0");
        assert!(set.items.is_empty() && !set.truncated);
    }

    #[test]
    fn recorded_is_none_without_the_event() {
        assert_eq!(recorded(&[started(0)], "l"), None);
        let set = SkipSet {
            source: Some("r0".into()),
            ..SkipSet::default()
        };
        let events = vec![ev(
            1,
            EventPayload::ListSkipRead {
                node: "l".into(),
                set: set.clone(),
            },
        )];
        assert_eq!(recorded(&events, "l"), Some(set));
        assert_eq!(recorded(&events, "other"), None);
    }

    #[test]
    fn the_skip_digest_covers_the_id_the_item_and_the_work() {
        let base = skip_digest("a", "item", "work");
        assert_eq!(base, skip_digest("a", "item", "work"));
        assert_ne!(base, skip_digest("b", "item", "work"));
        assert_ne!(base, skip_digest("a", "other", "work"));
        assert_ne!(base, skip_digest("a", "item", "other"));
    }

    #[test]
    fn coverage_tells_all_none_and_partial() {
        let pb = two_lists();
        let line = |seq, node: &str| {
            ev(
                seq,
                EventPayload::ListSkipRead {
                    node: node.into(),
                    set: SkipSet::default(),
                },
            )
        };
        assert_eq!(coverage(&[], &pb), Coverage::None);
        assert_eq!(coverage(&[line(1, "a"), line(2, "b")], &pb), Coverage::All);
        assert_eq!(
            coverage(&[line(1, "a")], &pb),
            Coverage::Partial {
                recorded: vec!["a".into()],
                missing: vec!["b".into()],
            }
        );
        let plain = Playbook::from_yaml(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        assert_eq!(coverage(&[], &plain), Coverage::All, "nothing to record");
    }

    /// A run directory whose journal holds its start and provenance.
    fn run_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":0,\"type\":\"run_provenance\",\"origin\":\"project\"}\n",
        )
        .unwrap();
        dir
    }

    fn eval_cfg() -> RunConfig {
        RunConfig {
            eval: Some(crate::run_config::EvalRunSettings::default()),
            ..RunConfig::default()
        }
    }

    fn reads(dir: &Path) -> Vec<(String, SkipSet)> {
        crate::event::read_all(dir)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::ListSkipRead { node, set } => Some((node, set)),
                _ => None,
            })
            .collect()
    }

    /// A script-only eval run without a manifest that crashed before its
    /// set write: the run type decides before any manifest, so the entry
    /// records empty sets and reads no history.
    #[test]
    fn a_crashed_script_only_eval_reads_no_history() {
        let dir = run_dir();
        let mut log = EventLog::open(dir.path()).unwrap();
        ensure_recorded(dir.path(), dir.path(), &two_lists(), &eval_cfg(), &mut log).unwrap();
        assert_eq!(
            reads(dir.path()),
            vec![
                ("a".to_string(), SkipSet::default()),
                ("b".to_string(), SkipSet::default())
            ]
        );
    }

    /// A top-level run without a manifest crashed before its set write:
    /// its type cannot be told, so every set is refused.
    #[test]
    fn a_crashed_run_with_an_unknown_origin_reads_no_history() {
        let dir = run_dir();
        let mut log = EventLog::open(dir.path()).unwrap();
        ensure_recorded(
            dir.path(),
            dir.path(),
            &two_lists(),
            &RunConfig::default(),
            &mut log,
        )
        .unwrap();
        let r = reads(dir.path());
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|(_, s)| {
            s.source.is_none()
                && s.refused
                    .as_deref()
                    .is_some_and(|w| w.starts_with(RUN_TYPE_UNKNOWN))
        }));
        // A manifest that does not read is no better.
        let dir = run_dir();
        std::fs::write(dir.path().join("manifest.yaml"), "not: [a manifest").unwrap();
        let mut log = EventLog::open(dir.path()).unwrap();
        ensure_recorded(
            dir.path(),
            dir.path(),
            &two_lists(),
            &RunConfig::default(),
            &mut log,
        )
        .unwrap();
        assert!(reads(dir.path()).iter().all(|(_, s)| s.refused.is_some()));
    }

    /// Writers that race on one run record one group: every writer has
    /// decided outside the lock that nothing is recorded (its sets computed
    /// before the barrier), and only the decision under the journal lock
    /// keeps the others from writing again.
    #[test]
    fn two_concurrent_entries_record_one_group() {
        let dir = run_dir();
        let pb = two_lists();
        let writers = 4;
        let barrier = std::sync::Barrier::new(writers);
        std::thread::scope(|s| {
            for _ in 0..writers {
                s.spawn(|| {
                    let mut log = EventLog::open(dir.path()).unwrap();
                    let events = crate::event::read_all(dir.path()).unwrap();
                    assert_eq!(coverage(&events, &pb), Coverage::None);
                    let sets = empty_sets(&pb);
                    barrier.wait();
                    write_sets(&mut log, &pb, sets).unwrap();
                });
            }
        });
        assert_eq!(reads(dir.path()).len(), 2, "one group of two lines");
    }

    /// A run with one node's line removed refuses to drive and appends
    /// nothing.
    #[test]
    fn a_partly_recorded_run_refuses_to_drive() {
        let dir = run_dir();
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append(EventPayload::ListSkipRead {
            node: "a".into(),
            set: SkipSet::default(),
        })
        .unwrap();
        let before = crate::event::read_all(dir.path()).unwrap().len();
        let err = ensure_recorded(dir.path(), dir.path(), &two_lists(), &eval_cfg(), &mut log)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(INCOMPLETE) && err.contains("but not of b"),
            "{err}"
        );
        assert_eq!(crate::event::read_all(dir.path()).unwrap().len(), before);
    }

    /// Every node's set goes out in one write: a write cut a few bytes
    /// short leaves the first node's line whole and the last one's torn, where
    /// one append per node would have torn the first line and never
    /// written the second.
    #[test]
    fn the_sets_go_out_in_one_write() {
        let dir = run_dir();
        let mut log = EventLog::open(dir.path()).unwrap();
        log.fault_next_write(5);
        assert!(write_sets(&mut log, &two_lists(), empty_sets(&two_lists())).is_err());
        let lossy = crate::event::read_all_lossy_tail(dir.path()).unwrap();
        let whole: Vec<&str> = lossy
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ListSkipRead { node, .. } => Some(node.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(whole, ["a"], "the first line whole, the last torn");
        let text = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let torn = text.rsplit('\n').next().unwrap();
        assert!(
            torn.contains("\"list_skip_read\"") && torn.contains("\"node\":\"b\""),
            "{torn}"
        );
    }

    /// A partial record found under the lock refuses the write and
    /// appends nothing to it.
    #[test]
    fn a_partial_record_under_the_lock_refuses_the_set_write() {
        let dir = run_dir();
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append(EventPayload::ListSkipRead {
            node: "a".into(),
            set: SkipSet::default(),
        })
        .unwrap();
        let before = std::fs::read(dir.path().join("events.jsonl")).unwrap();
        let err = write_sets(&mut log, &two_lists(), empty_sets(&two_lists()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(INCOMPLETE) && err.contains("but not of b"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("events.jsonl")).unwrap(),
            before
        );
    }

    /// The set write takes the required journal lock: while another writer
    /// holds it (the same OS lock, taken here on a second open of the
    /// journal), the write waits [`LOCK_WAIT`] and is refused
    /// `list_skip_lock_unavailable`, and the journal is unchanged. A
    /// best-effort lock would have written.
    #[test]
    fn a_journal_lock_failure_refuses_the_set_write() {
        let dir = run_dir();
        let mut log = EventLog::open(dir.path()).unwrap();
        let before = std::fs::read(dir.path().join("events.jsonl")).unwrap();
        let held = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("events.jsonl"))
            .unwrap();
        held.lock().unwrap();
        let started = std::time::Instant::now();
        let err = write_sets(&mut log, &two_lists(), empty_sets(&two_lists()))
            .unwrap_err()
            .to_string();
        assert!(err.contains(LOCK_UNAVAILABLE), "{err}");
        assert!(
            started.elapsed() >= LOCK_WAIT,
            "the write waited for the lock"
        );
        assert_eq!(
            std::fs::read(dir.path().join("events.jsonl")).unwrap(),
            before
        );
        held.unlock().unwrap();
        // Once the lock is free the same write goes through.
        write_sets(&mut log, &two_lists(), empty_sets(&two_lists())).unwrap();
        assert_eq!(reads(dir.path()).len(), 2);
    }

    /// Only a lock that could not be taken is named a lock failure: a full
    /// disk has its own reason, and any other error passes as it is.
    #[test]
    fn a_full_disk_is_not_a_lock_failure() {
        let full = lock_refusal(EngineError::Io(std::io::Error::from(
            std::io::ErrorKind::StorageFull,
        )))
        .to_string();
        assert!(full.contains(DISK_FULL), "{full}");
        let other = lock_refusal(EngineError::Io(std::io::Error::other("eio"))).to_string();
        assert!(
            !other.contains(LOCK_UNAVAILABLE) && !other.contains(DISK_FULL),
            "{other}"
        );
    }

    #[test]
    fn a_patch_adding_a_skip_is_refused() {
        let plain = Playbook::from_yaml(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        let why = patch_refusal(&plain, &two_lists(), &[]).unwrap();
        assert!(why.starts_with("list_skip_unrecorded: node a"), "{why}");
        assert_eq!(patch_refusal(&plain, &plain, &[]), None);
    }

    #[test]
    fn a_patch_after_skips_is_refused() {
        let pb = two_lists();
        let line = |seq, node: &str| {
            ev(
                seq,
                EventPayload::ListSkipRead {
                    node: node.into(),
                    set: SkipSet::default(),
                },
            )
        };
        let mut events = vec![line(1, "a"), line(2, "b"), started(3)];
        assert_eq!(
            patch_refusal(&pb, &pb, &events),
            None,
            "no skip settlement yet"
        );
        let mut skipped = passed(4, "x", Some("d"));
        if let EventPayload::ListItemSettled { skipped_from, .. } = &mut skipped.payload {
            *skipped_from = Some("r0".into());
        }
        events.push(skipped);
        let why = patch_refusal(&pb, &pb, &events).unwrap();
        assert!(why.starts_with(WORK_CHANGED), "{why}");
    }
}
