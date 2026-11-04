//! What a check's passing result is bound to, and the same fingerprints
//! again at the finish (spec 2026-11-03-evidence-freshness-design).
//!
//! A script `success_check` that passes journals `evidence_bound` with the
//! fingerprint of its scope taken right after the script exited: the
//! node cache's `files_fingerprint` over a declared scope, its
//! `git_fingerprint` over the whole tree otherwise. The finish node
//! journals `evidence_observed` with the same fingerprint of every current
//! binding, after the baseline's after snapshot and before the goal
//! criteria. `run_outcome::freshness` compares the two; nothing here
//! steers the run.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::fingerprint::files_fingerprint;

use super::journal::Journal;
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, EvidenceObservation};

/// The check name a `success_check` binding carries.
pub(crate) const SUCCESS_CHECK: &str = "success_check";

/// The fingerprint of `scope` beneath `root`, `None` when it cannot be
/// taken (a bad glob, an unreadable file) and for no scope: a git
/// fingerprint of the tree sees neither ignored files nor paths above
/// `root`, so it could not show such a pass current, and the outcome reads
/// an unscoped pass by the nodes that ran after it alone.
pub(crate) fn fingerprint(root: &Path, scope: &[String]) -> Option<String> {
    if scope.is_empty() {
        None
    } else {
        files_fingerprint(root, scope, &[]).ok()
    }
}

/// Journals the binding of `node`'s passing `success_check` in `attempt`,
/// run in `root`.
pub(crate) fn bind(
    journal: &Journal,
    node: &str,
    attempt: u32,
    root: &Path,
    scope: &[String],
) -> Result<(), EngineError> {
    journal.append(EventPayload::EvidenceBound {
        node: node.to_string(),
        attempt,
        check: SUCCESS_CHECK.to_string(),
        root: root.to_string_lossy().into_owned(),
        scope: scope.to_vec(),
        fingerprint: fingerprint(root, scope),
    })
}

/// The declared scopes to observe at the finish, once each: for every node
/// with a binding, the one its latest successful execution made (the one
/// the outcome reads, `run_outcome::freshness::latest_binding`) and its
/// latest one, when they differ. An unscoped binding is not observed.
pub(crate) fn current_keys(events: &[Event]) -> Vec<(String, Vec<String>)> {
    let mut latest: BTreeMap<&str, (String, Vec<String>)> = BTreeMap::new();
    for e in events {
        if let EventPayload::EvidenceBound {
            node, root, scope, ..
        } = &e.payload
        {
            latest.insert(node, (root.clone(), scope.clone()));
        }
    }
    let mut keys: Vec<(String, Vec<String>)> = Vec::new();
    for (node, key) in latest {
        if let Some((_, root, scope, _)) =
            crate::run_outcome::freshness::latest_binding(events, node)
        {
            keys.push((root.clone(), scope.clone()));
        }
        keys.push(key);
    }
    keys.retain(|(_, scope)| !scope.is_empty());
    keys.sort();
    keys.dedup();
    keys
}

/// Journals `evidence_observed` at the finish: the fingerprint of every
/// current binding's scope now. Nothing when the run bound nothing.
pub(crate) fn observe(log: &mut EventLog, events: &[Event]) -> Result<(), EngineError> {
    let keys = current_keys(events);
    if keys.is_empty() {
        return Ok(());
    }
    let observations = keys
        .into_iter()
        .map(|(root, scope)| EvidenceObservation {
            fingerprint: fingerprint(Path::new(&root), &scope),
            root,
            scope,
        })
        .collect();
    log.append(EventPayload::EvidenceObserved { observations })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(lines: &[&str]) -> Vec<Event> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["seq"] = (i as u64 + 1).into();
                v["ts"] = 0.into();
                serde_json::from_value(v).unwrap()
            })
            .collect()
    }

    fn bound(root: &str, scope: &str) -> String {
        format!(
            r#"{{"type":"evidence_bound","node":"w","attempt":1,"check":"success_check","root":"{root}","scope":{scope}}}"#
        )
    }

    const START: &str = r#"{"type":"node_started","node":"w","attempt":1}"#;
    const DONE: &str =
        r#"{"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":""}"#;
    const FAILED: &str =
        r#"{"type":"node_finished","node":"w","status":"failed","attempt":1,"output":""}"#;

    #[test]
    fn the_finish_observes_the_binding_the_outcome_reads() {
        // Visit 1 succeeded in /r1; visit 2 bound /r2 and then failed.
        let events = journal(&[
            START,
            &bound("/r1", r#"["src/**"]"#),
            DONE,
            START,
            &bound("/r2", r#"["src/**"]"#),
            FAILED,
        ]);
        let keys = current_keys(&events);
        let roots: Vec<&str> = keys.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(roots, ["/r1", "/r2"]);
    }

    #[test]
    fn an_unscoped_binding_is_not_observed() {
        let events = journal(&[START, &bound("/r1", "[]"), DONE]);
        assert!(current_keys(&events).is_empty());
    }
}
