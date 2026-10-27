//! "Will it fit here?": one read-only report on whether a playbook can run
//! in this project, kept in four separate sections so none hides another.
//!
//! - **Compatibility**: what the project cannot change by configuration (the
//!   lifecycle, `requires`, the sub-playbook tree, scripts the version
//!   lacks, connectors that do not resolve), plus notes (cold session
//!   handoffs, protect globs that match nothing).
//! - **Setup**: what the person can provide (profiles, skills, models,
//!   connector accounts and secrets, param values).
//! - **Trust**: who vouched for each part, and whether an acknowledge at
//!   start covers it. An untrusted playbook can be ready.
//! - **Consent**: what a start must be agreed to (irreversible effects) and
//!   the effects of the whole tree.
//!
//! `ready` means nothing in compatibility or setup blocks a run; it does not
//! mean a start goes through without a word: `trusted` and `needs_consent`
//! say what a start must still be approved or agreed to.
//!
//! The same report backs every surface; nothing here writes.

mod report;
mod setup;
mod trust;

use std::path::Path;

use apb_core::preflight::{
    RequiresRefusal, cold_handoffs, missing_scripts, requires_unmet, unmatched_protect_globs,
};
use apb_core::profile_store::PlaybookOrigin;
use apb_core::registry::{Registry, RegistryError};
use apb_core::run_memory::{MemoryKey, recall};
use apb_core::scope::{Origin, PlaybookRef, origin_scope_label};
use apb_core::trust::{Lifecycle, TrustStore, read_lifecycle};

pub use report::{ConsentView, FitItem, FitReport, FitSeverity, FitSummary, TrustItem};
pub use setup::{ProfileFinding, UNTRUSTED, profile_findings};

/// Why no report could be made.
#[derive(Debug, thiserror::Error)]
pub enum FitError {
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

fn item(code: &str, subject: Option<&str>, detail: String, severity: FitSeverity) -> FitItem {
    FitItem {
        code: code.to_string(),
        subject: subject.map(str::to_string),
        detail,
        severity,
    }
}

/// Assesses playbook `wref` for a run in the project at `root` (the
/// execution root; a global playbook is read from the global store).
pub fn assess(root: &Path, wref: &PlaybookRef) -> Result<FitReport, FitError> {
    let resolved = apb_core::store::resolve(root, wref)?;
    let reg = Registry::open_dir(&resolved.definition_parent)?;
    let loaded = reg.load(&wref.id, Some(&resolved.version))?;
    let playbook = &loaded.playbook;
    let playbook_dir = resolved.definition_parent.join("playbooks").join(&wref.id);
    let origin = match wref.origin {
        Origin::Global => PlaybookOrigin::Global,
        Origin::Project { .. } => PlaybookOrigin::Project,
    };
    let store = TrustStore::load();
    let refs = trust::refs_of(playbook);

    let mut compatibility = Vec::new();
    let mut setup = Vec::new();
    let mut trust_items = trust::playbook_items(
        &playbook_dir,
        &wref.id,
        &resolved.version,
        &resolved.digest,
        &store,
    );

    match read_lifecycle(&playbook_dir) {
        Lifecycle::Active => {}
        Lifecycle::Draft => compatibility.push(item(
            "draft_requires_trial",
            None,
            "the playbook is a draft: it runs only as a trial until it is approved".into(),
            FitSeverity::Blocking,
        )),
        Lifecycle::Retired => compatibility.push(item(
            "retired_not_runnable",
            None,
            "the playbook is retired".into(),
            FitSeverity::Blocking,
        )),
    }
    if let Some(req) = &playbook.requires {
        compatibility.extend(requires_items(root, req));
    }

    // The sub-playbook tree, read-only about trust; a trust refusal of a
    // child belongs to the trust section.
    let tree = crate::gate::resolve_tree(root, playbook, &wref.origin, &wref.id, true);
    let (children, effects) = match tree {
        Ok(t) => (Some(t.children), t.effects),
        Err(refusal) => {
            match trust::refusal_items(&refusal) {
                Some(items) => trust_items.extend(items),
                None => compatibility.push(item(
                    refusal["policy"].as_str().unwrap_or("sub_playbook_refused"),
                    refusal["id"].as_str(),
                    refusal["detail"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| refusal.to_string()),
                    FitSeverity::Blocking,
                )),
            }
            // The effects and the trust items below then cover this
            // playbook alone; say so rather than show a partial tree as
            // the whole one.
            compatibility.push(item(
                "tree_not_assessed",
                None,
                "the sub-playbook tree could not be resolved: the effects and trust items cover this playbook only".into(),
                FitSeverity::Note,
            ));
            (None, apb_core::effects::effective(playbook))
        }
    };

    // Profile problems are the setup items of `profiles` below; only the
    // scripts the version lacks are checked here.
    let version_dir = playbook_dir.join(&resolved.version);
    for issue in missing_scripts(&version_dir, playbook) {
        compatibility.push(item(
            issue.code,
            issue.node.as_deref(),
            issue.message,
            FitSeverity::Blocking,
        ));
    }

    match crate::gate::trust_scan::scan_connectors(root, playbook) {
        Err(errors) => compatibility.extend(
            errors
                .into_iter()
                .map(|e| item("connector_unresolved", None, e, FitSeverity::Blocking)),
        ),
        Ok(scan) => {
            for name in &scan.no_account {
                setup.push(item(
                    "connector_no_account",
                    Some(name),
                    "no configured account: every call would fail until one is added".into(),
                    FitSeverity::Blocking,
                ));
            }
            if !scan.missing_env.is_empty() {
                setup.push(item(
                    "connector_env_missing",
                    None,
                    format!("unset secret env vars: {}", scan.missing_env.join(", ")),
                    FitSeverity::Blocking,
                ));
            }
            trust_items.extend(trust::connector_items(&scan));
        }
    }
    for problem in cold_handoffs(root, playbook) {
        compatibility.push(item(
            "session_handoff_cold",
            None,
            problem,
            FitSeverity::Note,
        ));
    }
    for problem in unmatched_protect_globs(root, playbook) {
        compatibility.push(item("V76", None, problem, FitSeverity::Note));
    }

    let profiles = setup::profiles(root, origin, &refs, &store);
    setup.extend(profiles.setup);
    // Remembered input only adds notes: memory that does not read is one
    // more note, never a failed report.
    let remembered = MemoryKey::for_label(resolved.origin_label, &wref.id)
        .map_err(|e| e.to_string())
        .and_then(|key| {
            let legacy = reg
                .read_instruction_draft(&wref.id)
                .map_err(|e| e.to_string())?;
            recall(root, &key, &playbook.params, legacy).map_err(|e| e.to_string())
        });
    match remembered {
        Ok(remembered) => setup.extend(setup::input_items(&remembered)),
        Err(e) => setup.push(item(
            "remembered_unreadable",
            None,
            format!("the remembered input could not be read: {e}"),
            FitSeverity::Note,
        )),
    }

    trust_items.extend(trust::bundle_items(&profiles.bundles, &store));
    if let Some(children) = &children {
        trust::child_items(children, "", &store, &mut trust_items);
    }
    trust::dedup(&mut trust_items);

    let irreversible =
        crate::gate::consent_sources(root, playbook, &wref.origin, children.as_ref());
    let consent = ConsentView {
        irreversible,
        effects: effects
            .iter()
            .map(|e| crate::gate::effect_str(e).to_string())
            .collect(),
    };
    Ok(FitReport {
        id: wref.id.clone(),
        scope: origin_scope_label(&wref.origin).to_string(),
        version: resolved.version,
        ready: !report::blocks(&compatibility) && !report::blocks(&setup),
        trusted: trust_items.iter().all(|t| t.approved),
        needs_consent: !consent.irreversible.is_empty(),
        compatibility,
        setup,
        trust: trust_items,
        consent,
    })
}

fn requires_items(root: &Path, req: &apb_core::schema::Requires) -> Vec<FitItem> {
    match requires_unmet(root, req) {
        Ok(missing) => missing
            .into_iter()
            .map(|m| {
                item(
                    "requires_unmet",
                    Some(&m),
                    format!("this project or machine lacks {m}"),
                    FitSeverity::Blocking,
                )
            })
            .collect(),
        Err(RequiresRefusal::UnsafePath(p)) => vec![item(
            "requires_unsafe",
            Some(&p),
            "requires.files entries must be relative paths inside the project".into(),
            FitSeverity::Blocking,
        )],
        Err(RequiresRefusal::UnsafeCommand(c)) => vec![item(
            "requires_unsafe",
            Some(&c),
            "requires.commands entries must be program names, not paths".into(),
            FitSeverity::Blocking,
        )],
    }
}
