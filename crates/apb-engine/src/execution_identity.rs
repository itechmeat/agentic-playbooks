//! Where in its executor binding an attempt ran.
//!
//! Before the attempts of an `agent_task` execution the engine writes the
//! binding in force for the node (the manifest entry, or the rebind or tier
//! overlay entry over it) canonically to `runs/<id>/bindings/<sha256>.json`,
//! write-once and private, and every attempt journals an [`ExecStep`] on its
//! `attempt_started`: the digest of that snapshot, the step of its chain and
//! how the step was reached. A later rebind writes another snapshot and
//! never changes what earlier attempts ran on, so a retry that must run on
//! the same binding (a deferred retry) can read it back by its digest.
//!
//! An older apb drops `exec` and folds exactly as before; it never opens
//! `bindings/`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::host_task::HintSource;
use crate::manifest::ManifestProfile;
use crate::run_store::RunStore;

/// The directory of binding snapshots inside a run directory.
pub const BINDINGS_DIR: &str = "bindings";

const STORE: RunStore = RunStore {
    dir: BINDINGS_DIR,
    ext: "json",
};

/// `ExecStep.executed_by` of an attempt an agent CLI ran.
pub const EXECUTED_BY_CLI: &str = "cli";
/// `ExecStep.executed_by` of an attempt the host session ran as a task.
pub const EXECUTED_BY_HOST: &str = "host";

/// The chain position of one attempt, journaled on its `attempt_started`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecStep {
    /// The digest of the binding snapshot the attempt ran on.
    pub binding: String,
    /// The index of the step in that binding's chain; the host fallback
    /// step, which follows the whole chain, is `chain.len()`.
    pub step: u32,
    pub source: StepSource,
    /// `cli` or `host`.
    pub executed_by: String,
}

/// How a chain step was reached.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepSource {
    /// The profile's own executor.
    Primary,
    /// Fallback entry `index` (1-based) of the profile's `of` fallbacks.
    Fallback { index: u32, of: u32 },
    /// A tier that tier routing put in front of the profile's executor.
    Tier { name: String },
    /// The host step a `cli` run takes when none of its CLIs could start.
    HostFallback,
    /// The single executor of a run-local ephemeral override.
    Ephemeral,
}

impl StepSource {
    /// The source of a chain step whose role in the profile is `hint`, in a
    /// binding that is an ephemeral override or not.
    pub(crate) fn of(hint: &HintSource, ephemeral: bool) -> Self {
        match hint {
            HintSource::HostFallback { .. } => StepSource::HostFallback,
            _ if ephemeral => StepSource::Ephemeral,
            HintSource::Primary { .. } => StepSource::Primary,
            HintSource::Fallback { index, of, .. } => StepSource::Fallback {
                index: *index,
                of: *of,
            },
            HintSource::Tier { tier, .. } => StepSource::Tier { name: tier.clone() },
        }
    }
}

/// The canonical bytes of a binding: its JSON with every object's keys in
/// sorted order, so equal bindings always get one digest.
fn canonical(binding: &ManifestProfile) -> std::io::Result<Vec<u8>> {
    let value = serde_json::to_value(binding).map_err(std::io::Error::other)?;
    serde_json::to_vec(&value).map_err(std::io::Error::other)
}

/// Writes `binding` as a snapshot of the run (once: an equal binding reuses
/// its file) and returns its digest.
pub fn store_binding(run_dir: &Path, binding: &ManifestProfile) -> std::io::Result<String> {
    STORE.store(run_dir, &canonical(binding)?)
}

/// The binding stored under `digest`, when it is there, intact and readable.
pub fn read_binding(run_dir: &Path, digest: &str) -> Option<ManifestProfile> {
    serde_json::from_slice(&STORE.read(run_dir, digest)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(soul: &str) -> ManifestProfile {
        yaml_serde::from_str(&format!(
            "scope: project\nname: dev\nprofile_digest: sha256:a\nbundle_digest: sha256:b\nsoul: {soul}\nsoul_requirement: any\nskills: []\nchain: []\n"
        ))
        .unwrap()
    }

    #[test]
    fn a_binding_is_stored_once_and_read_back_by_its_digest() {
        let dir = tempfile::tempdir().unwrap();
        let a = store_binding(dir.path(), &binding("m1")).unwrap();
        assert_eq!(store_binding(dir.path(), &binding("m1")).unwrap(), a);
        let b = store_binding(dir.path(), &binding("m2")).unwrap();
        assert_ne!(a, b);
        assert_eq!(read_binding(dir.path(), &a), Some(binding("m1")));
        assert_eq!(read_binding(dir.path(), &b), Some(binding("m2")));
        assert_eq!(read_binding(dir.path(), &"0".repeat(64)), None);
        let files = std::fs::read_dir(dir.path().join(BINDINGS_DIR))
            .unwrap()
            .count();
        assert_eq!(files, 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(STORE.path(dir.path(), &a))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    #[test]
    fn step_sources_follow_the_chain_role() {
        let p = || "project/dev".to_string();
        let cases = [
            (
                HintSource::Primary { profile: p() },
                false,
                StepSource::Primary,
            ),
            (
                HintSource::Fallback {
                    index: 1,
                    of: 2,
                    profile: p(),
                },
                false,
                StepSource::Fallback { index: 1, of: 2 },
            ),
            (
                HintSource::Tier {
                    tier: "light".into(),
                    profile: p(),
                },
                false,
                StepSource::Tier {
                    name: "light".into(),
                },
            ),
            (
                HintSource::HostFallback { profile: p() },
                true,
                StepSource::HostFallback,
            ),
            (
                HintSource::Primary { profile: p() },
                true,
                StepSource::Ephemeral,
            ),
        ];
        for (hint, ephemeral, want) in cases {
            assert_eq!(StepSource::of(&hint, ephemeral), want);
        }
        let step = ExecStep {
            binding: "ab".into(),
            step: 1,
            source: StepSource::Fallback { index: 1, of: 2 },
            executed_by: EXECUTED_BY_CLI.into(),
        };
        let json = serde_json::to_string(&step).unwrap();
        assert_eq!(
            json,
            r#"{"binding":"ab","step":1,"source":{"kind":"fallback","index":1,"of":2},"executed_by":"cli"}"#
        );
    }
}
