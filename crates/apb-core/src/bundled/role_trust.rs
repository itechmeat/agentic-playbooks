//! Keeping the role profiles setup wrote approved when a shipped skill they
//! carry is updated.
//!
//! A profile's bundle digest covers its skills' trees, so a new shipped
//! skill changes it and drops the approval. A role profile keeps its
//! approval across that update only when nothing but shipped content
//! changed: setup wrote it ([`note_role_profile`] recorded what it wrote),
//! its `profile.yaml` and `SOUL.md` are still exactly that, its bundle was
//! approved just before, and every skill it carries is a shipped skill
//! whose installed copy holds exactly the shipped files. The new bundle is
//! then approved with the origin the old one had (never `Bundled`).
//! Anything else keeps the normal trust drop.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::profile::{ProfileScope, QualifiedProfileRef};
use crate::profile_store::{PlaybookOrigin, compute_bundle};
use crate::trust::{Kind, OriginKind, TrustStore};

/// The record of the role profiles setup wrote, in the bundled state dir:
/// profile name -> the profile digest setup wrote.
const RECORD: &str = "role-profiles.json";

fn record_path() -> Option<PathBuf> {
    super::state_dir().map(|d| d.join(RECORD))
}

fn load() -> BTreeMap<String, String> {
    record_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Records that setup wrote the global role profile `name` with
/// `profile_digest` (its `profile.yaml` and `SOUL.md`). The surface that
/// writes the role profiles calls it after each write; a later write of
/// the same name replaces the entry.
pub fn note_role_profile(name: &str, profile_digest: &str) -> std::io::Result<()> {
    let path = record_path().ok_or_else(super::no_state_dir)?;
    let _lock = super::hold()?;
    let mut record = load();
    record.insert(name.to_string(), profile_digest.to_string());
    let json = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
    crate::fsutil::atomic_write_private(&path, &json)
}

/// One role profile as it stood before a skill update.
pub(crate) struct Held {
    name: String,
    profile_digest: String,
    skills: Vec<String>,
    bundle: String,
    origin: OriginKind,
}

/// The global profile `name` with its live bundle.
fn bundle_of(name: &str) -> Option<crate::profile_store::ComputedBundle> {
    let root = crate::config::config_dir()?;
    let r = QualifiedProfileRef {
        name: name.to_string(),
        scope: ProfileScope::Global,
    };
    compute_bundle(&root, PlaybookOrigin::Global, &r).ok()
}

/// The recorded role profiles that are still what setup wrote and whose
/// bundle is approved, taken before the skills change.
pub(crate) fn snapshot() -> Vec<Held> {
    let trust = TrustStore::load();
    load()
        .into_iter()
        .filter_map(|(name, written)| {
            let (profile, _, bundle) = bundle_of(&name)?;
            if profile.profile_digest != written {
                return None;
            }
            let origin = trust.origin(&bundle)?;
            Some(Held {
                skills: profile.doc.skills.iter().map(|s| s.name.clone()).collect(),
                name,
                profile_digest: written,
                bundle,
                origin,
            })
        })
        .collect()
}

/// Whether the installed copy of the shipped skill `name` holds exactly the
/// shipped files.
fn shipped_and_current(name: &str) -> bool {
    super::skill(name).is_some_and(|s| {
        matches!(
            super::skill_state(&s),
            super::InstallState::UpToDate { installed } if installed == s.version
        )
    })
}

/// Approves the new bundle of each `held` profile that carries one of the
/// `updated` skills, when only shipped content changed; returns a line for
/// each approval that could not be written.
pub(crate) fn carry(held: Vec<Held>, updated: &[String]) -> Vec<String> {
    let mut notes = Vec::new();
    for h in held {
        if h.origin == OriginKind::Bundled
            || !h.skills.iter().any(|s| updated.contains(s))
            || !h.skills.iter().all(|s| shipped_and_current(s))
        {
            continue;
        }
        let Some((profile, _, bundle)) = bundle_of(&h.name) else {
            continue;
        };
        if profile.profile_digest != h.profile_digest || bundle == h.bundle {
            continue;
        }
        if let Err(e) =
            TrustStore::load().approve_kind(&bundle, &h.name, Kind::ProfileBundle, h.origin)
        {
            notes.push(format!(
                "role profile `{}` was not approved for the updated skills: {e}",
                h.name
            ));
        }
    }
    notes
}
