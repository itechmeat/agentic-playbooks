//! The corrections file of one playbook:
//! `<root>/.apb/corrections/<scope>/<id>.json`, git-ignored, written
//! atomically (0600) under a lock of its directory. Read, MAC, lock and
//! write are the shared signed store ([`crate::signed_store`]).
//!
//! The ledger holds people's words and engine fingerprints, so like the
//! known-problems store it is only ever this installation's file: a file
//! the installation did not sign (one a repository brought) reads as a
//! fresh empty one and is replaced on the next write.

use crate::scope::Scope;
use crate::signed_store::SignedDoc;

use super::CorrectionFile;

/// The file format this build reads and writes.
pub const SCHEMA: u32 = 1;

impl SignedDoc for CorrectionFile {
    const MAC_DOMAIN: &'static str = "apb-corrections-v1";
    const SCHEMA: u32 = SCHEMA;
    const DIR: &'static str = "corrections";
    const ENTRIES_KEY: &'static str = "entries";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        CorrectionFile {
            schema: SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            ..CorrectionFile::default()
        }
    }

    fn identity(&self) -> (&str, &str) {
        (&self.scope, &self.playbook)
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
    use crate::corrections::entry_id;
    use crate::corrections::normalize::key_of;
    use crate::corrections::{CorrectionKind, ObservedRun, Sighting};
    use crate::signed_store::{self, StoreError};

    /// A key of the shape apb mints (at least 64 characters), seeded into
    /// the test's own config dir, so the MACs verify whatever machine and
    /// whatever parallel env race runs this.
    const TEST_KEY: &str =
        "corrections-store-test-key-0123456789abcdef0123456789abcdef0123456789abcdef";

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

    #[test]
    fn the_file_is_written_signed_and_read_back() {
        with_config_dir(|| {
            let tmp = tempfile::tempdir().unwrap();
            let key = key_of(CorrectionKind::ReviewNote, Some("work"), "word one two");
            signed_store::update(tmp.path(), Scope::Project, "p", |f: &mut CorrectionFile| {
                f.observe(
                    ObservedRun {
                        run_id: "r1".into(),
                        version: "1.0.0".into(),
                        started_ms: 1000,
                    },
                    vec![Sighting {
                        kind: CorrectionKind::ReviewNote,
                        node: Some("work".into()),
                        gate: None,
                        key: key.clone(),
                        text: Some("word one two".into()),
                        run_id: "r1".into(),
                        seq: 3,
                        at_ms: 1100,
                        version: "1.0.0".into(),
                        via_supervisor: false,
                    }],
                    1100,
                );
                Ok::<_, StoreError>(((), true))
            })
            .unwrap();
            let loaded =
                signed_store::read::<CorrectionFile>(tmp.path(), Scope::Project, "p").unwrap();
            assert!(loaded.signed, "the file this build wrote did not verify");
            assert_eq!(loaded.unsigned_entries, 0);
            assert_eq!(loaded.file.entries.len(), 1);
            assert_eq!(loaded.file.entries[0].id, entry_id(&key));
            assert_eq!(loaded.file.entries[0].runs, 1);
            assert_eq!(loaded.file.entries[0].evidence[0].seq, 3);
        });
    }

    #[test]
    fn a_file_signed_under_another_domain_reads_unsigned() {
        with_config_dir(|| {
            let tmp = tempfile::tempdir().unwrap();
            let dir = signed_store::dir(tmp.path(), "corrections", Scope::Project);
            std::fs::create_dir_all(&dir).unwrap();
            let mut value =
                serde_json::to_value(CorrectionFile::fresh(Scope::Project, "p")).unwrap();
            value.as_object_mut().unwrap().remove("mac");
            let covered = signed_store::bytes_of(&value).unwrap();
            let tagged =
                crate::run_origin::tag("apb-known-problems-v1", &covered).expect("a test key");
            value
                .as_object_mut()
                .unwrap()
                .insert("mac".into(), serde_json::Value::String(tagged));
            std::fs::write(
                dir.join("p.json"),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap();
            let loaded =
                signed_store::read::<CorrectionFile>(tmp.path(), Scope::Project, "p").unwrap();
            assert!(!loaded.signed, "a foreign domain's MAC verified");
            assert!(
                loaded.file.entries.is_empty(),
                "an unsigned file is never shown"
            );
        });
    }
}
