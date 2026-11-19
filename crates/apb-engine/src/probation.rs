//! The probation ledger's writer and the doctor line. The ledger lives in
//! `apb_core::probation` (the file, its MAC, `add` and `counted`); this
//! module is the one writer: the journal's terminal observer, registered
//! by the process that drives runs, adds one entry per ended run, after
//! the run's terminal event is in the journal and with the outcome read
//! from it. Nothing else writes the ledger: no MCP tool, HTTP route, CLI
//! command or run step can add an entry; only a terminal event does.

use std::path::Path;

use crate::event::{Event, EventPayload};

/// Registers the ledger writer as the journal's second terminal observer,
/// beside the health one ([`crate::terminal_hook::install_ledger`]). The
/// `apb` binary and the engine's test harness call it at start.
pub fn install() {
    crate::terminal_hook::install_ledger(observe_ended);
}

/// The doctor's `probation` line: every ledger file of this project that
/// does not verify, with the remedy. `None` when every file reads (or
/// there is none).
pub fn doctor_line(root: &Path) -> Option<String> {
    use apb_core::scope::Scope;
    use apb_core::signed_store::{self, SignedDoc};
    let mut bad = Vec::new();
    for scope in [Scope::Project, Scope::Global] {
        let dir = signed_store::dir(
            root,
            <apb_core::probation::ProbationFile as SignedDoc>::DIR,
            scope,
        );
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            let readable = apb_core::probation::read(root, scope, id).is_ok_and(|l| l.signed);
            if !readable {
                bad.push(e.path().display().to_string());
            }
        }
    }
    (!bad.is_empty()).then(|| {
        format!(
            "probation ledgers that do not verify (unattended starts of their playbooks are refused until each is moved aside; the count then starts again from zero): {}",
            bad.join(", ")
        )
    })
}

/// The observer the journal's terminal hook calls once a `run_finished` or
/// `run_aborted` line is in the file and the journal lock is released.
/// Best effort: a fault is one stderr line and changes nothing about the
/// run.
fn observe_ended(run_dir: &Path) {
    if let Err(e) = observe_once(run_dir) {
        eprintln!("{e}");
    }
}

/// Writes the run's entry; `Err` is the one stderr line, already worded.
fn observe_once(run_dir: &Path) -> Result<(), String> {
    let fault = |e: String| format!("apb: the probation ledger was not written: {e}");
    // 1. The journal is read strictly; a journal this apb reads only in
    //    part writes nothing.
    let events = crate::event::read_all(run_dir).map_err(|e| fault(e.to_string()))?;
    // 2. The outcome: the last terminal event. None, or a run_resumed
    //    after it (a later append made the run live again before this
    //    observer read): nothing is written; the end that follows calls
    //    the observer again.
    let Some(outcome) = terminal_outcome(&events) else {
        return Ok(());
    };
    // 3. The playbook id and the scope the run came from; none: nothing.
    let Some((id, scope)) = crate::known_problems::started(&events) else {
        return Ok(());
    };
    // 4. A run that does not learn (a scenario, an eval, an external-data
    //    run, a manifest that does not read) writes nothing: it could
    //    never be eligible, and an external-data run keeps nothing in a
    //    learning store.
    if !crate::external_data::learns_from(run_dir) {
        return Ok(());
    }
    // 5. The entry.
    let entry = entry_of(run_dir, &events, &outcome)
        .map_err(|e| fault(format!("the run's records do not read: {e}")))?;
    // 6. One transaction under the signed store's lock; a file that does
    //    not verify is refused and nothing is written. The file names
    //    the project it belongs to.
    let root = crate::run_config::project_root_of(run_dir)
        .ok_or_else(|| fault("the run's project root is unknown".to_string()))?;
    let project = apb_core::probation::project_key(&root);
    apb_core::signed_store::transact::<apb_core::probation::ProbationFile, (), _>(
        &root,
        scope,
        &id,
        apb_core::signed_store::Unsigned::Refuse,
        |f| {
            let moved = f.project != project;
            f.project = project.clone();
            let added = apb_core::probation::add(f, entry);
            Ok::<_, apb_core::signed_store::StoreError>((
                (),
                added || (moved && !f.entries.is_empty()),
            ))
        },
    )
    .map_err(|e| match e {
        apb_core::signed_store::StoreError::Unverified(_) => format!(
            "apb: the probation ledger of `{id}` does not verify; this run is not recorded in it"
        ),
        other => fault(other.to_string()),
    })?;
    // A run whose id has an entry already (a run resumed after it ended,
    // which ends again) keeps its first entry: `add` wrote nothing then.
    Ok(())
}

/// The last terminal event's outcome: `run_finished { outcome }` as
/// journaled, `run_aborted` as `aborted`. `None` when a `run_resumed`
/// follows the last terminal event (the run went live again) or when
/// there is none.
fn terminal_outcome(events: &[Event]) -> Option<String> {
    let last = events.iter().rev().find(|e| {
        matches!(
            &e.payload,
            EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
        )
    })?;
    let resumed_after = events
        .iter()
        .any(|e| e.seq > last.seq && matches!(&e.payload, EventPayload::RunResumed { .. }));
    (!resumed_after).then(|| match &last.payload {
        EventPayload::RunFinished { outcome } => outcome.clone(),
        _ => "aborted".to_string(),
    })
}

/// The ledger entry of the ended run: the run id (the directory name),
/// `run_started.version`, `run_provenance.digest`, `run_started.definition`,
/// the run mode (the manifest, else `run.yaml`), the outcome, `eligible` (the exact test)
/// and the time.
fn entry_of(
    run_dir: &Path,
    events: &[Event],
    outcome: &str,
) -> Result<apb_core::probation::ProbationEntry, String> {
    use apb_core::probation::ProbationEntry;
    let run = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let started = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted {
            version,
            definition,
            ..
        } => Some((version.clone(), definition.clone())),
        _ => None,
    });
    let Some((version, definition)) = started else {
        return Err(format!("run `{run}` journaled no run_started"));
    };
    let trust_digest = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { digest, .. } => digest.clone(),
            _ => None,
        })
        .unwrap_or_default();
    let manifest = crate::manifest::read(run_dir).ok().flatten();
    // The mode comes from a record that exists (the manifest, else the
    // persisted `run.yaml`; `manifest::recorded_mode`). When neither
    // establishes it, the entry is recorded unattended and not eligible,
    // never attended by default.
    let mode = match crate::manifest::recorded_mode(run_dir) {
        Ok(crate::manifest::RecordedMode::Attended) => Some(false),
        Ok(crate::manifest::RecordedMode::Unattended(_)) => Some(true),
        Err(_) => None,
    };
    let unattended = mode.unwrap_or(true);
    // `read_run_config` answers the default for a missing file, which
    // would read as no overrides: only a file that exists is a record.
    let config = run_dir
        .join("run.yaml")
        .is_file()
        .then(|| crate::run_config::read_run_config(run_dir).ok())
        .flatten();
    // A run a run step started is never eligible: a run step is never a
    // person, and attended means started without --unattended by one.
    let from_step = manifest
        .as_ref()
        .and_then(|m| m.launch.as_ref())
        .is_some_and(|l| l.started_by.is_some());
    let unoverridden = config
        .as_ref()
        .is_some_and(|cfg| cfg.overrides.as_ref().is_none_or(|o| o.nodes.is_empty()));
    let eligible = outcome == "succeeded"
        && mode.is_some()
        && !from_step
        && eligible_definition(events, definition.as_deref())
        && unoverridden;
    Ok(ProbationEntry {
        run,
        version,
        trust_digest,
        definition: definition.unwrap_or_default(),
        unattended,
        outcome: outcome.to_string(),
        eligible,
        ended_ms: apb_core::clock::now_ms_u64(),
    })
}

/// Whether the journal executed one definition from start to end:
/// exactly one `run_started` with a definition digest, a `run_provenance`
/// with one, no `patch_applied`, no `profile_rebound`, no `run_migrated`,
/// and every `run_resumed` on the same definition.
fn eligible_definition(events: &[Event], definition: Option<&str>) -> bool {
    let Some(definition) = definition else {
        return false;
    };
    let started = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::RunStarted { .. }))
        .count();
    if started != 1 {
        return false;
    }
    let provenance = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::RunProvenance {
                digest: Some(_),
                ..
            }
        )
    });
    if !provenance {
        return false;
    }
    // An in-run patch put another definition in force; a rebind changed a
    // node's executor after the start; a migration rewrote the
    // definition. None of them may stand.
    let touched = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::PatchApplied { .. }
                | EventPayload::ProfileRebound { .. }
                | EventPayload::RunMigrated { .. }
        )
    });
    if touched {
        return false;
    }
    // Every resume on the same definition: one that executed other bytes,
    // or one an older apb journaled without a digest, is not the same
    // definition.
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::RunResumed { .. }))
        .all(|e| match &e.payload {
            EventPayload::RunResumed { definition: d, .. } => d.as_deref() == Some(definition),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> Vec<Event> {
        [
            serde_json::json!({"seq": 1, "ts": 0, "type": "run_started",
                "playbook": "pb", "version": "1.0.0", "definition": "sha256:def"}),
            serde_json::json!({"seq": 2, "ts": 0, "type": "run_provenance",
                "digest": "sha256:trust"}),
            serde_json::json!({"seq": 3, "ts": 0, "type": "run_finished",
                "outcome": "succeeded"}),
        ]
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect()
    }

    /// An unattended run whose manifest is gone takes its mode from the
    /// persisted `run.yaml`: it is recorded unattended, so it never counts
    /// toward a later unattended start.
    #[test]
    fn a_missing_manifest_takes_the_mode_from_run_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::run_config::RunConfig {
            unattended: true,
            ..Default::default()
        };
        crate::run_config::write_run_config(dir.path(), &cfg).unwrap();
        let entry = entry_of(dir.path(), &journal(), "succeeded").unwrap();
        assert!(entry.unattended, "{entry:?}");
        assert_eq!(
            apb_core::probation::counted(
                &apb_core::probation::ProbationFile {
                    entries: vec![entry],
                    ..Default::default()
                },
                "sha256:trust",
                "sha256:def"
            ),
            0
        );
    }

    /// With neither the manifest nor `run.yaml` reading, the mode is not
    /// established: the entry is not eligible and not recorded attended.
    #[test]
    fn no_record_of_the_mode_is_not_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let entry = entry_of(dir.path(), &journal(), "succeeded").unwrap();
        assert!(!entry.eligible && entry.unattended, "{entry:?}");
    }

    /// An attended run with no manifest (nothing to record) and a plain
    /// `run.yaml` is still an eligible attended success.
    #[test]
    fn an_attended_run_without_a_manifest_stays_eligible() {
        let dir = tempfile::tempdir().unwrap();
        crate::run_config::write_run_config(dir.path(), &Default::default()).unwrap();
        let entry = entry_of(dir.path(), &journal(), "succeeded").unwrap();
        assert!(entry.eligible && !entry.unattended, "{entry:?}");
    }

    /// The doctor line names an unverifiable ledger.
    #[test]
    fn the_doctor_line_names_an_unverifiable_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let scope_dir = dir.path().join(".apb/probation/project");
        std::fs::create_dir_all(&scope_dir).unwrap();
        std::fs::write(scope_dir.join("pb.json"), "{not json").unwrap();
        let line = doctor_line(dir.path()).expect("a line");
        assert!(
            line.contains("probation ledgers that do not verify") && line.contains("pb.json"),
            "{line}"
        );
    }
}
