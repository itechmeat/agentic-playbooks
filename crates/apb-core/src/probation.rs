//! The probation ledger of one playbook in one project:
//! `<root>/.apb/probation/<scope>/<id>.json`, git-ignored, written
//! atomically (0600) under a lock of its directory, signed by this
//! installation like the corrections ledger. Read, MAC, lock and write
//! are the shared signed store ([`crate::signed_store`]).
//!
//! Every run that ends and learns adds one entry for its run id, written
//! once, after its terminal event is in the journal; the start check
//! counts the eligible attended successes whose trust digest and
//! definition digest are those of the definition it loaded.

use std::path::Path;

use crate::scope::Scope;
use crate::signed_store::{Loaded, SignedDoc, StoreError};
use serde::{Deserialize, Serialize};

/// The file format this build reads and writes.
pub const SCHEMA: u32 = 1;
/// The most entries a file keeps; the oldest go first (dropping an entry
/// can only lower a count).
pub const MAX_ENTRIES: usize = 1000;

/// The probation ledger of one playbook in one project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbationFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    /// The project the ledger belongs to: the workspace id registered at
    /// its root, else its canonical root ([`project_key`]). The MAC covers
    /// it, so a signed ledger copied or linked from another project does
    /// not count here.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub entries: Vec<ProbationEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// One ended run, written once after its terminal event and never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbationEntry {
    /// The run id: one entry per run.
    pub run: String,
    pub version: String,
    /// The trust digest the start verified and journaled
    /// (`run_provenance.digest`, `LoadedPlaybook::trust_digest`): the
    /// definition and its scripts. Empty when the journal has none.
    pub trust_digest: String,
    /// The digest of the definition the run started with
    /// (`run_started.definition`). Empty when the journal has none.
    pub definition: String,
    /// Whether the run ran unattended (its manifest's `unattended`).
    pub unattended: bool,
    /// `succeeded`, `failed` or `aborted`.
    pub outcome: String,
    /// The run counts toward probation (the eligibility test in the
    /// engine's writer).
    pub eligible: bool,
    pub ended_ms: u64,
}

/// Adds `entry` unless an entry for its run is there; drops the oldest
/// beyond [`MAX_ENTRIES`]. Returns whether the file changed.
pub fn add(file: &mut ProbationFile, entry: ProbationEntry) -> bool {
    if file.entries.iter().any(|e| e.run == entry.run) {
        return false;
    }
    file.entries.push(entry);
    while file.entries.len() > MAX_ENTRIES {
        file.entries.remove(0);
    }
    true
}

/// The eligible attended successes whose trust digest is `trust` and
/// whose definition is `definition`.
pub fn counted(file: &ProbationFile, trust: &str, definition: &str) -> u32 {
    file.entries
        .iter()
        .filter(|e| {
            !e.unattended
                && e.outcome == "succeeded"
                && e.eligible
                && e.trust_digest == trust
                && e.definition == definition
        })
        .count() as u32
}

/// The project key a ledger at `root` is written with: the workspace id
/// registered at that root, else the canonical root.
pub fn project_key(root: &Path) -> String {
    crate::projects::registered_id(root).unwrap_or_else(|| canonical_root(root))
}

fn canonical_root(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

/// Every key a ledger of the project at `root` may carry: its registered
/// workspace id and its canonical root (a project registered after its
/// ledger was written still reads it).
fn project_keys(root: &Path) -> Vec<String> {
    let mut keys = vec![canonical_root(root)];
    keys.extend(crate::projects::registered_id(root));
    keys
}

/// Reads the probation ledger of `playbook` under `scope` in the project
/// at `root` (the shared signed store's read, with no link followed and
/// the project compared).
pub fn read(
    root: &Path,
    scope: Scope,
    playbook: &str,
) -> Result<Loaded<ProbationFile>, StoreError> {
    crate::signed_store::read::<ProbationFile>(root, scope, playbook)
}

impl SignedDoc for ProbationFile {
    const MAC_DOMAIN: &'static str = "apb-probation-v1";
    const SCHEMA: u32 = SCHEMA;
    const DIR: &'static str = "probation";
    const ENTRIES_KEY: &'static str = "entries";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        ProbationFile {
            schema: SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            ..ProbationFile::default()
        }
    }

    const NO_LINKS: bool = true;

    fn identity(&self) -> (&str, &str) {
        (&self.scope, &self.playbook)
    }

    fn belongs_to(&self, root: &Path) -> bool {
        project_keys(root).contains(&self.project)
    }

    fn mac(&self) -> Option<&str> {
        self.mac.as_deref()
    }

    fn set_mac(&mut self, mac: Option<String>) {
        self.mac = mac;
    }

    fn set_schema(&mut self, schema: u32) {
        self.schema = schema;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signed_store::{self, StoreError};

    /// A key of the shape apb mints (at least 64 characters), seeded into
    /// the test's own config dir, so the MACs verify whatever machine and
    /// whatever parallel env race runs this.
    const TEST_KEY: &str =
        "probation-store-test-key-0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Runs `f` with `APB_CONFIG_DIR` on a fresh directory holding the test
    /// key, under the env lock, restored after.
    fn with_config_dir<R>(f: impl FnOnce() -> R) -> R {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        std::fs::write(cfg.path().join("run-origin.key"), TEST_KEY).unwrap();
        let prev = std::env::var_os("APB_CONFIG_DIR");
        // SAFETY: under env_test_lock, the one env change at a time.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        let out = f();
        // SAFETY: still under the lock (the guard drops after this body).
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        out
    }

    fn entry(run: &str, unattended: bool, outcome: &str, eligible: bool) -> ProbationEntry {
        ProbationEntry {
            run: run.to_string(),
            version: "1.0.0".into(),
            trust_digest: "sha256:trust".into(),
            definition: "sha256:def".into(),
            unattended,
            outcome: outcome.into(),
            eligible,
            ended_ms: 1000,
        }
    }

    #[test]
    fn add_writes_a_run_once() {
        let mut file = ProbationFile::default();
        assert!(add(&mut file, entry("r1", false, "succeeded", true)));
        assert!(
            !add(&mut file, entry("r1", false, "failed", false)),
            "written once"
        );
        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].outcome, "succeeded");
        assert!(add(&mut file, entry("r2", false, "succeeded", true)));
        assert_eq!(file.entries.len(), 2);
    }

    #[test]
    fn add_drops_the_oldest_beyond_the_cap() {
        let mut file = ProbationFile::default();
        for i in 0..MAX_ENTRIES + 10 {
            add(&mut file, entry(&format!("r{i}"), false, "succeeded", true));
        }
        assert_eq!(file.entries.len(), MAX_ENTRIES);
        assert_eq!(
            file.entries[0].run,
            "r10".to_string(),
            "the oldest went first"
        );
        assert_eq!(
            file.entries.last().unwrap().run,
            format!("r{}", MAX_ENTRIES + 9)
        );
    }

    /// `counted` takes the eligible attended successes of one digest
    /// pair: an unattended, a failed, an ineligible entry and one of
    /// another trust digest or definition count 0.
    #[test]
    fn counted_takes_eligible_attended_successes_of_one_digest_pair() {
        let mut file = ProbationFile::default();
        add(&mut file, entry("r1", false, "succeeded", true));
        add(&mut file, entry("r2", true, "succeeded", true));
        add(&mut file, entry("r3", false, "failed", true));
        add(&mut file, entry("r4", false, "succeeded", false));
        let other_trust = ProbationEntry {
            trust_digest: "sha256:other".into(),
            ..entry("r5", false, "succeeded", true)
        };
        add(&mut file, other_trust);
        let other_def = ProbationEntry {
            definition: "sha256:other".into(),
            ..entry("r6", false, "succeeded", true)
        };
        add(&mut file, other_def);
        // Only the one entry of the exact pair counts; the unattended, the
        // failed, the ineligible and the entries of another trust digest
        // or definition do not inflate it.
        assert_eq!(counted(&file, "sha256:trust", "sha256:def"), 1);
    }

    /// The `SignedDoc` impl: the file this build wrote verifies; one a
    /// repository brought reads unsigned, counted by nothing.
    #[test]
    fn the_store_verifies_its_own_file_and_counts_nothing_unsigned() {
        with_config_dir(|| {
            let tmp = tempfile::tempdir().unwrap();
            let project = project_key(tmp.path());
            signed_store::update(tmp.path(), Scope::Project, "p", |f: &mut ProbationFile| {
                f.project = project.clone();
                Ok::<_, StoreError>(((), add(f, entry("r1", false, "succeeded", true))))
            })
            .unwrap();
            let loaded = read(tmp.path(), Scope::Project, "p").unwrap();
            assert!(loaded.signed, "the file this build wrote did not verify");
            assert_eq!(loaded.file.entries.len(), 1);
            // The same signed file in another project does not verify
            // there: the MAC covers the project it names.
            let other = tempfile::tempdir().unwrap();
            let dir = other.path().join(".apb/probation/project");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(
                tmp.path().join(".apb/probation/project/p.json"),
                dir.join("p.json"),
            )
            .unwrap();
            assert!(!read(other.path(), Scope::Project, "p").unwrap().signed);
            // A file a repository brought: unsigned, counted by nothing.
            let repo = tempfile::tempdir().unwrap();
            std::fs::write(
                repo.path().join("probation.json"),
                serde_json::to_string(&ProbationFile {
                    entries: vec![entry("r9", false, "succeeded", true)],
                    ..ProbationFile::default()
                })
                .unwrap(),
            )
            .unwrap();
            std::fs::create_dir_all(tmp.path().join(".apb/probation/project")).unwrap();
            std::fs::copy(
                repo.path().join("probation.json"),
                tmp.path().join(".apb/probation/project/foreign.json"),
            )
            .unwrap();
            let loaded =
                signed_store::read::<ProbationFile>(tmp.path(), Scope::Project, "foreign").unwrap();
            assert!(!loaded.signed, "a foreign file must read unsigned");
            assert_eq!(
                counted(&loaded.file, "sha256:trust", "sha256:def"),
                0,
                "an unsigned file counts nothing"
            );
        });
    }
}
