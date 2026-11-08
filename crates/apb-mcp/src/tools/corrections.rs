//! The corrections ledger on the surfaces: a playbook's corrections (MCP
//! `playbook_corrections`, `apb corrections`, `GET
//! /api/playbooks/{id}/corrections`) and a person's decision on one entry
//! (`correction_decide`, `apb corrections dismiss|reopen|fixed`, `POST
//! /api/playbooks/{id}/corrections/{entry}/{action}`). The logic is
//! [`apb_core::corrections`]; this layer reads the signed store
//! ([`apb_core::signed_store`]) and rebuilds each entry's view.
//!
//! The ledger holds people's words and engine fingerprints and is shown
//! only to people: every entry point, reads included, is refused from a
//! run step here, once ([`super::outside_run`]). Marking a fix names a
//! version of the playbook, a statement only a person makes, so the MCP
//! server never constructs [`Decision::MarkFixed`]; the CLI and the
//! dashboard offer it.

use std::path::{Path, PathBuf};

use apb_core::corrections::{Correction, CorrectionFile, CorrectionStatus, Draft};
use apb_core::scope::Scope;
use apb_core::signed_store::{self};
use apb_core::step_env::StepCaller;
use serde::Serialize;

pub use super::problems::Target;
use super::{ToolError, outside_run};

/// What a refusal from a run step names.
const READING: &str = "reading or deciding corrections";
const DECIDING: &str = "deciding corrections";

/// A person's decision on one entry. `MarkFixed` names the version of the
/// playbook's store that carries the fix; only the CLI and the dashboard
/// construct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Dismiss,
    Reopen,
    MarkFixed { version: String },
}

/// The per-run rates a `fixed` or `reopened` entry is judged by: hits per
/// run on the base version against hits per run on the fix version (see
/// [`apb_core::corrections::REOPEN_AFTER_RUNS`]).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixRate {
    pub base_runs: u32,
    pub base_hits: u32,
    pub fix_runs: u32,
    pub fix_hits: u32,
}

/// One entry as every surface shows it: the stored correction, the draft a
/// person may edit and apply (a `proposed` or `reopened` entry with a node
/// that has a prompt), and both rates once a fix is marked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorrectionView {
    pub correction: Correction,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub draft: Option<Draft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub rate: Option<FixRate>,
}

/// A playbook's corrections as every surface shows them. An unsigned file
/// (one this installation did not write) is never shown: its entries are
/// counted, never read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorrectionsView {
    pub playbook: String,
    pub scope: String,
    pub signed: bool,
    pub unsigned_entries: usize,
    pub entries: Vec<CorrectionView>,
}

/// The view of one entry. The draft is offered by a `proposed` or
/// `reopened` entry only (see the design), computed against the playbook's
/// current version; a playbook that does not load gives none.
fn view_of(
    playbook: Option<&apb_core::schema::Playbook>,
    file: &CorrectionFile,
    e: &Correction,
) -> CorrectionView {
    let draft = playbook.and_then(|pb| {
        if matches!(
            e.status,
            CorrectionStatus::Proposed | CorrectionStatus::Reopened
        ) {
            apb_core::corrections::draft(e, pb)
        } else {
            None
        }
    });
    CorrectionView {
        correction: e.clone(),
        draft,
        rate: rate_of(file, e),
    }
}

/// Both rates of a fixed or reopened entry, from its fix record and the
/// file's counted runs: `None` while no fix is marked (a person's reopen
/// clears the record, so a plain `reopened`-by-fold entry keeps it).
fn rate_of(file: &CorrectionFile, e: &Correction) -> Option<FixRate> {
    let fix = e.fix.as_ref()?;
    Some(FixRate {
        base_runs: fix.base_runs,
        base_hits: fix.base_hits,
        fix_runs: file.runs_by_version.get(&fix.version).copied().unwrap_or(0),
        fix_hits: e.runs_by_version.get(&fix.version).copied().unwrap_or(0),
    })
}

/// Reads the store and builds the view of every entry.
fn read_view(root: &Path, t: &Target) -> Result<CorrectionsView, ToolError> {
    let loaded = signed_store::read::<CorrectionFile>(root, t.scope, &t.id)?;
    let playbook = apb_engine::known_problems::source::current_playbook(root, t.scope, &t.id);
    let entries = loaded
        .file
        .entries
        .iter()
        .map(|e| view_of(playbook.as_ref(), &loaded.file, e))
        .collect();
    Ok(CorrectionsView {
        playbook: t.id.clone(),
        scope: t.scope.as_str().to_string(),
        signed: loaded.signed,
        unsigned_entries: loaded.unsigned_entries,
        entries,
    })
}

/// A playbook's corrections as every surface shows them. Read-only.
/// Refused from a run step.
pub fn corrections_view(
    root: &Path,
    t: &Target,
    step: &StepCaller,
) -> Result<CorrectionsView, ToolError> {
    outside_run(step, READING)?;
    read_view(root, t)
}

/// The playbook directory of `t` under its scope: the project's, or the
/// global store's for a global playbook.
fn playbook_dir(root: &Path, t: &Target) -> Result<PathBuf, ToolError> {
    let dir = match t.scope {
        Scope::Project => root.join(".apb/playbooks").join(&t.id),
        Scope::Global => {
            let parent = apb_core::store::global_playbooks_parent().ok_or_else(|| {
                ToolError::Engine("no global config dir for a global playbook".into())
            })?;
            parent.join("playbooks").join(&t.id)
        }
    };
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(ToolError::NotFound(format!("playbook `{}`", t.id)))
    }
}

/// A person's decision on entry `entry` of the playbook `t` names: dismiss,
/// reopen, or mark a version of the playbook's store as carrying the fix.
/// `by` names the surface (`cli`, `dashboard`). Refused from a run step.
pub fn correction_decide(
    root: &Path,
    t: &Target,
    entry: &str,
    decision: Decision,
    by: &str,
    reason: Option<&str>,
    step: &StepCaller,
) -> Result<CorrectionView, ToolError> {
    outside_run(step, DECIDING)?;
    // A fix names a version the playbook's store has: a statement about a
    // definition that does not exist says nothing.
    if let Decision::MarkFixed { version } = &decision {
        let dir = playbook_dir(root, t)?;
        let have = apb_core::registry::list_versions(&dir)
            .map_err(|e| ToolError::Engine(e.to_string()))?;
        if !have.iter().any(|v| v == version) {
            return Err(ToolError::Conflict(format!(
                "unknown_version: version `{version}` is not a version of playbook `{}` (the store has {})",
                t.id,
                if have.is_empty() {
                    "none".to_string()
                } else {
                    have.join(", ")
                }
            )));
        }
    }
    let at_ms = apb_core::clock::now_ms_u64();
    signed_store::update::<CorrectionFile, _, ToolError>(root, t.scope, &t.id, |f| {
        let result = match &decision {
            Decision::Dismiss => f.dismiss(entry, by, at_ms, reason),
            Decision::Reopen => f.reopen(entry, by, at_ms, reason),
            Decision::MarkFixed { version } => f.mark_fixed(entry, version, by, at_ms, reason),
        };
        // A refused decision changes nothing; an accepted one always does.
        result.map_err(ToolError::Conflict)?;
        Ok(((), true))
    })?;
    // The one entry, rebuilt like the list's.
    let loaded = signed_store::read::<CorrectionFile>(root, t.scope, &t.id)?;
    let e = loaded
        .file
        .get(entry)
        .ok_or_else(|| ToolError::NotFound(format!("correction `{entry}`")))?;
    let playbook = apb_engine::known_problems::source::current_playbook(root, t.scope, &t.id);
    Ok(view_of(playbook.as_ref(), &loaded.file, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::corrections::{CorrectionKind, ObservedRun, Sighting, entry_id};
    use apb_core::signed_store::StoreError;
    use std::fs;

    /// One env change at a time across this binary's tests (the same lock
    /// the server tests hold).
    use crate::server::tests::CROSS_ENV_LOCK as ENV_LOCK;

    const KEY: &str = "t:0123456789abcdef";

    const PLAYBOOK: &str = "schema: 2\nid: corr\nname: Corr\nversion: 1.0.0\nnodes:\n  \
         - { id: start, type: start }\n  \
         - { id: work, type: agent_task, prompt: do the work }\n  \
         - { id: done, type: finish, outcome: success }\n\
       edges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

    fn target() -> Target {
        Target {
            id: "corr".into(),
            scope: Scope::Project,
        }
    }

    fn sighting(text: Option<&str>, run_id: &str, seq: u64, at_ms: u64) -> Sighting {
        Sighting {
            kind: CorrectionKind::ReviewNote,
            node: Some("work".into()),
            gate: Some("review".into()),
            key: KEY.into(),
            text: text.map(str::to_string),
            run_id: run_id.into(),
            seq,
            at_ms,
            version: "1.0.0".into(),
            via_supervisor: false,
        }
    }

    fn seed(root: &Path) {
        apb_core::registry::init_project(root).unwrap();
        let vdir = root.join(".apb/playbooks/corr/1.0.0");
        fs::create_dir_all(&vdir).unwrap();
        fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
        fs::write(root.join(".apb/playbooks/corr/current"), "1.0.0").unwrap();
    }

    /// Runs `f` with a keyed config dir and a seeded project `corr`, under
    /// the env lock (the MACs must verify, so the config dir is ours).
    fn project<R>(f: impl FnOnce(&Path) -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("APB_CONFIG_DIR");
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: under ENV_LOCK, the one env change at a time.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        apb_core::run_origin::ensure_key().unwrap();
        let root = tempfile::tempdir().unwrap();
        seed(root.path());
        let out = f(root.path());
        // SAFETY: still under the lock; restored before it drops.
        unsafe {
            match &old {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        out
    }

    /// A ledger with a proposed entry (two runs of one sighting); returns
    /// the entry's id.
    fn ledger(root: &Path) -> String {
        signed_store::update(root, Scope::Project, "corr", |f: &mut CorrectionFile| {
            f.observe(
                ObservedRun {
                    run_id: "corr-1".into(),
                    version: "1.0.0".into(),
                    started_ms: 1000,
                },
                vec![sighting(Some("quote the version"), "corr-1", 5, 1100)],
                1100,
            );
            f.observe(
                ObservedRun {
                    run_id: "corr-2".into(),
                    version: "1.0.0".into(),
                    started_ms: 2000,
                },
                vec![sighting(Some("quote the version"), "corr-2", 6, 2100)],
                2100,
            );
            Ok::<_, StoreError>(((), true))
        })
        .unwrap();
        entry_id(KEY)
    }

    #[test]
    fn every_entry_point_is_refused_from_a_run_step() {
        project(|root| {
            let id = ledger(root);
            let old = std::env::var_os("APB_RUN_ID");
            // SAFETY: under ENV_LOCK (project holds it), one change at a time.
            unsafe { std::env::set_var("APB_RUN_ID", "corr-3") };
            let step = StepCaller::from_env();
            let view = corrections_view(root, &target(), &step);
            let decide =
                correction_decide(root, &target(), &id, Decision::Dismiss, "cli", None, &step);
            // SAFETY: under ENV_LOCK.
            unsafe {
                match &old {
                    Some(v) => std::env::set_var("APB_RUN_ID", v),
                    None => std::env::remove_var("APB_RUN_ID"),
                }
            }
            for (what, refused, needle) in [
                (
                    "view",
                    view.err().map(|e| e.to_string()),
                    "reading or deciding corrections",
                ),
                (
                    "decide",
                    decide.err().map(|e| e.to_string()),
                    "deciding corrections",
                ),
            ] {
                let m = refused.unwrap_or_else(|| panic!("{what}: expected a refusal"));
                assert!(m.contains("run step"), "{what}: {m}");
                assert!(m.contains(needle), "{what}: {m}");
            }
            // The decision never landed.
            let view = corrections_view(root, &target(), &StepCaller::from_env()).unwrap();
            assert_eq!(
                view.entries[0].correction.status,
                CorrectionStatus::Proposed
            );
        });
    }

    #[test]
    fn the_view_lists_entries_with_their_draft() {
        project(|root| {
            let id = ledger(root);
            let view = corrections_view(root, &target(), &StepCaller::from_env()).unwrap();
            assert_eq!(view.playbook, "corr");
            assert_eq!(view.scope, "project");
            assert!(view.signed);
            assert_eq!(view.unsigned_entries, 0);
            assert_eq!(view.entries.len(), 1);
            let e = &view.entries[0];
            assert_eq!(e.correction.id, id);
            assert_eq!(e.correction.status, CorrectionStatus::Proposed);
            assert_eq!(e.correction.runs, 2);
            let d = e
                .draft
                .as_ref()
                .expect("a proposed entry with a node and text");
            assert_eq!(d.node, "work");
            assert_eq!(
                d.line,
                "A person corrected this step in earlier runs: quote the version"
            );
            assert!(e.rate.is_none(), "no fix is marked, so no rate");
        });
    }

    #[test]
    fn a_playbook_that_does_not_load_gives_no_draft() {
        project(|root| {
            fs::remove_file(root.join(".apb/playbooks/corr/current")).unwrap();
            ledger(root);
            let view = corrections_view(root, &target(), &StepCaller::from_env()).unwrap();
            assert!(view.entries[0].draft.is_none());
        });
    }

    #[test]
    fn a_fixed_entry_shows_both_rates_and_no_draft() {
        project(|root| {
            let id = ledger(root);
            let vdir = root.join(".apb/playbooks/corr/1.1.0");
            fs::create_dir_all(&vdir).unwrap();
            fs::write(
                vdir.join("playbook.yaml"),
                PLAYBOOK.replace("version: 1.0.0", "version: 1.1.0"),
            )
            .unwrap();
            let e = correction_decide(
                root,
                &target(),
                &id,
                Decision::MarkFixed {
                    version: "1.1.0".into(),
                },
                "cli",
                Some("new prompt"),
                &StepCaller::from_env(),
            )
            .unwrap();
            assert_eq!(e.correction.status, CorrectionStatus::Fixed);
            assert!(e.draft.is_none(), "a fixed entry offers no draft");
            let fix = e.correction.fix.as_ref().unwrap();
            assert_eq!(fix.version, "1.1.0");
            assert_eq!(
                fix.base_version, "1.0.0",
                "the entry's latest sighting then"
            );
            assert_eq!(fix.base_runs, 2);
            assert_eq!(fix.base_hits, 2);
            assert_eq!(fix.marked.by, "cli");
            assert_eq!(fix.marked.reason, "new prompt");
            let rate = e.rate.as_ref().expect("a fixed entry shows its rate");
            assert_eq!(
                (rate.base_runs, rate.base_hits, rate.fix_runs, rate.fix_hits),
                (2, 2, 0, 0),
                "no run of the fix version yet"
            );
            // The view of the list agrees with the one entry's view.
            let view = corrections_view(root, &target(), &StepCaller::from_env()).unwrap();
            assert_eq!(view.entries[0], e);
        });
    }

    #[test]
    fn dismiss_and_reopen_keep_the_decision() {
        project(|root| {
            let id = ledger(root);
            let e = correction_decide(
                root,
                &target(),
                &id,
                Decision::Dismiss,
                "cli",
                Some("a\nb\u{202e} c"),
                &StepCaller::from_env(),
            )
            .unwrap();
            assert_eq!(e.correction.status, CorrectionStatus::Dismissed);
            assert!(e.draft.is_none(), "a dismissed entry offers no draft");
            let decided = e.correction.decided.as_ref().unwrap();
            assert_eq!(decided.by, "cli");
            assert_eq!(decided.reason, "a b c", "one line, hidden characters gone");

            match correction_decide(
                root,
                &target(),
                &id,
                Decision::Dismiss,
                "cli",
                None,
                &StepCaller::from_env(),
            ) {
                Err(ToolError::Conflict(m)) => assert!(m.contains("already dismissed"), "{m}"),
                other => panic!("expected a conflict, got {:?}", other.is_ok()),
            }

            let e = correction_decide(
                root,
                &target(),
                &id,
                Decision::Reopen,
                "dashboard",
                Some("still happens"),
                &StepCaller::from_env(),
            )
            .unwrap();
            assert_eq!(e.correction.status, CorrectionStatus::Proposed, "two runs");
            assert!(e.correction.fix.is_none());
            assert_eq!(
                e.correction.decided.as_ref().unwrap().reason,
                "still happens"
            );
            assert!(e.draft.is_some(), "a reopened entry offers its draft again");

            match correction_decide(
                root,
                &target(),
                &id,
                Decision::Reopen,
                "dashboard",
                None,
                &StepCaller::from_env(),
            ) {
                Err(ToolError::Conflict(m)) => {
                    assert!(m.contains("not dismissed or fixed"), "{m}")
                }
                other => panic!("expected a conflict, got {:?}", other.is_ok()),
            }
        });
    }

    #[test]
    fn mark_fixed_refuses_a_version_the_store_does_not_have() {
        project(|root| {
            let id = ledger(root);
            match correction_decide(
                root,
                &target(),
                &id,
                Decision::MarkFixed {
                    version: "9.9.9".into(),
                },
                "cli",
                None,
                &StepCaller::from_env(),
            ) {
                Err(ToolError::Conflict(m)) => assert!(m.starts_with("unknown_version"), "{m}"),
                other => panic!("expected a conflict, got {:?}", other.is_ok()),
            }
            // The refused decision changed nothing.
            let view = corrections_view(root, &target(), &StepCaller::from_env()).unwrap();
            assert_eq!(
                view.entries[0].correction.status,
                CorrectionStatus::Proposed
            );
            assert!(view.entries[0].correction.fix.is_none());
            // An unknown entry is refused with the store's words.
            match correction_decide(
                root,
                &target(),
                "c-nope",
                Decision::Dismiss,
                "cli",
                None,
                &StepCaller::from_env(),
            ) {
                Err(ToolError::Conflict(m)) => assert!(m.contains("no correction `c-nope`"), "{m}"),
                other => panic!("expected a conflict, got {:?}", other.is_ok()),
            }
        });
    }

    #[test]
    fn an_unsigned_file_reads_as_empty_with_its_entry_count() {
        project(|root| {
            let dir = signed_store::dir(root, "corrections", Scope::Project);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("corr.json"),
                r#"{"schema":1,"scope":"project","playbook":"corr","entries":[{"id":"c-1"},{"id":"c-2"},{"id":"c-3"}]}"#,
            )
            .unwrap();
            let view = corrections_view(root, &target(), &StepCaller::from_env()).unwrap();
            assert!(!view.signed);
            assert_eq!(view.unsigned_entries, 3);
            assert!(
                view.entries.is_empty(),
                "an unsigned file's entries are never shown"
            );
            // A decision reads the same fresh file, so it finds no entry.
            match correction_decide(
                root,
                &target(),
                &entry_id(KEY),
                Decision::Dismiss,
                "cli",
                None,
                &StepCaller::from_env(),
            ) {
                Err(ToolError::Conflict(m)) => assert!(m.contains("no correction"), "{m}"),
                other => panic!("expected a conflict, got {:?}", other.is_ok()),
            }
        });
    }
}
