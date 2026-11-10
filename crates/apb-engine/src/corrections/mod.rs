//! The corrections ledger's collecting half: what one run contributes
//! ([`extract`]) and the one entry point a run end calls ([`observe`]).
//! The file, the key and the fold are `apb_core::corrections`.

pub mod extract;

use std::path::Path;

use apb_core::corrections::{CorrectionFile, ObservedRun};
use apb_core::scope::Scope;
use apb_core::signed_store::SignedDoc;

use crate::error::EngineError;
use crate::event::{EventPayload, read_all};

/// Adds the run at `run_dir` (under project `root`) to its playbook's
/// ledger. Strict about the journal: a run this apb reads only in part,
/// an unknown event after the last checkpoint, or a broken line, is not
/// counted and stores nothing. Production runs only; a run without
/// sightings whose playbook has no ledger file yet writes nothing.
pub fn observe(root: &Path, run_dir: &Path) -> Result<(), EngineError> {
    // A strict read: a journal this apb reads only in part is not counted.
    let events = read_all(run_dir)?;
    let Some((playbook, scope)) = crate::known_problems::started(&events) else {
        return Ok(());
    };
    // A run whose tree's secret values cannot all be read stores nothing:
    // its sightings would outlive the run and be read without its
    // redactor.
    let redactor = crate::run_redact::RunRedactor::for_run(root, run_dir);
    let Ok(scrubber) = redactor.scrubber() else {
        return Ok(());
    };
    // Only a production run that did not process external data is counted.
    if !crate::external_data::learns_from(run_dir) {
        return Ok(());
    }
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(version) = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { version, .. } => Some(version.clone()),
        _ => None,
    }) else {
        return Ok(());
    };
    let started_ms = events
        .first()
        .map(|e| u64::try_from(e.ts).unwrap_or(0))
        .unwrap_or(0);
    let snapshot = crate::legacy_snapshot::load_run_playbook(run_dir);
    let sightings = extract::sightings(&run_id, &version, &events, scrubber, snapshot.as_ref());
    // A playbook that never had a correction gets no file: a run without
    // sightings counts only toward a ledger that exists.
    if sightings.is_empty() && !ledger_exists(root, scope, &playbook) {
        return Ok(());
    }
    let at_ms = apb_core::clock::now_ms_u64();
    apb_core::signed_store::update::<CorrectionFile, (), apb_core::signed_store::StoreError>(
        root,
        scope,
        &playbook,
        |f| {
            let changed = f.observe(
                ObservedRun {
                    run_id: run_id.clone(),
                    version: version.clone(),
                    started_ms,
                },
                sightings,
                at_ms,
            );
            Ok(((), changed))
        },
    )?;
    Ok(())
}

/// Whether `playbook`'s ledger file exists in `scope` under `root`.
fn ledger_exists(root: &Path, scope: Scope, playbook: &str) -> bool {
    apb_core::signed_store::dir(root, CorrectionFile::DIR, scope)
        .join(format!("{playbook}.json"))
        .exists()
}
