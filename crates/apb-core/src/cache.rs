//! Content-addressed cache store for node execution results.
//!
//! Two content-addressed halves live under `<project_root>/.apb/cache`:
//! `records/<hex[0..2]>/<hex>.json` (small JSON metadata, one per cache key)
//! and `objects/<hex[0..2]>/<hex>` (the output/artifact bytes, keyed by their
//! own digest). A `format` file at the store root pins the on-disk schema
//! version. `<hex>` always strips the `sha256:` scheme prefix.
//!
//! The store never fails its caller on a read: every miss path in [`load`]
//! (missing record, corrupt record JSON, an unrecognized `format_version`,
//! an expired ttl, a missing or tampered object) degrades to `None` rather
//! than an error. A corrupt record or one whose object is gone is
//! opportunistically deleted so the next lookup is a single cheap miss
//! instead of repeating the same dead read. `store`/`prune`/`clear` return
//! `std::io::Result` for a caller that wants to downgrade failures itself
//! (a cache write is best-effort; it must never abort the node it caches).
//!
//! [`load`]: CacheStore::load

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::content::sha256_hex;
use crate::fsutil::atomic_write;

/// On-disk schema version for cache records. `load` treats any other value
/// as a miss (never an error): a newer or unrelated writer's record is
/// simply ignored, not deleted, since a different build of `apb` may still
/// depend on it.
pub const CACHE_FORMAT: u32 = 1;

/// Where a cached artifact lives relative to its root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactScope {
    Run,
    Workspace,
}

/// A single artifact produced alongside a cached node's primary output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    /// `"sha256:<hex>"`.
    pub digest: String,
    pub scope: ArtifactScope,
    /// Relative to the scope root.
    pub path: String,
}

/// The parts folded into a cache key by [`cache_key`]. Any field change
/// changes the key. Field order is part of the wire contract: it determines
/// the canonical JSON serialization, so it must not be reordered.
#[derive(Serialize, Clone)]
pub struct KeyParts<'a> {
    pub format: u32,
    /// Canonical JSON of the Node.
    pub node_def: &'a str,
    pub script_digest: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub rendered_prompt: Option<&'a str>,
    pub bundle_digest: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
    /// Sorted by the caller; the store folds them in as given.
    pub connector_digests: Vec<String>,
    pub workspace_fingerprint: &'a str,
}

/// `"sha256:<hex of canonical JSON of parts>"`.
pub fn cache_key(parts: &KeyParts) -> String {
    let json = serde_json::to_string(parts).expect("key parts serialize");
    sha256_hex(json.as_bytes())
}

/// Where a cached result came from: which run, playbook, and node produced
/// it, for `apb cache inspect` and audit trails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub run_id: String,
    pub playbook_id: String,
    pub playbook_version: String,
    pub node_id: String,
}

/// What was checked before trusting this cached result as still valid for
/// its node type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub workspace_unchanged: bool,
    /// `"read_only"` | `"none"`.
    pub connector_calls: String,
}

/// The metadata record stored at `records/<hex[0..2]>/<hex>.json`. The
/// output bytes and artifact bytes themselves live in `objects/` under their
/// own digests; the record only references them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRecord {
    pub format_version: u32,
    pub key: String,
    pub created_at_unix: u64,
    pub node_type: String,
    pub provenance: Provenance,
    #[serde(default)]
    pub profile_bundle_digest: Option<String>,
    pub workspace_fingerprint: String,
    pub verification: Verification,
    /// `"sha256:<hex>"` of the output object.
    pub output_digest: String,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

/// A cache hit: the record plus its decoded output text.
pub struct CachedEntry {
    pub record: CacheRecord,
    pub output: String,
}

/// Aggregate counts for `apb cache status`.
pub struct StoreStatus {
    pub records: usize,
    pub objects: usize,
    pub total_bytes: u64,
}

/// What a [`CacheStore::prune`] call did.
pub struct PruneReport {
    pub removed_records: usize,
    pub removed_objects: usize,
}

/// A content-addressed cache store rooted at `<project_root>/.apb/cache`.
pub struct CacheStore {
    root: PathBuf,
}

impl CacheStore {
    /// Opens the store rooted at `<project_root>/.apb/cache`. Does not touch
    /// the filesystem; directories are created lazily on first write.
    pub fn open(project_root: &Path) -> CacheStore {
        CacheStore {
            root: project_root.join(".apb/cache"),
        }
    }

    /// Path for a content-addressed entry of `kind` (`"records"` or
    /// `"objects"`) keyed by `digest` (`"sha256:<hex>"`, prefix optional).
    /// Two-level sharding by the first two hex chars keeps any one directory
    /// from growing unbounded.
    fn shard(&self, kind: &str, digest: &str) -> PathBuf {
        let hex = digest.trim_start_matches("sha256:");
        self.root
            .join(kind)
            .join(&hex[..2.min(hex.len())])
            .join(hex)
    }

    /// Writes the output object, every artifact object, and finally the
    /// record (in that order, so a record is never visible before the bytes
    /// it references). All writes go through [`atomic_write`].
    pub fn store(
        &self,
        record: &CacheRecord,
        output: &str,
        artifacts: &[(ArtifactRef, Vec<u8>)],
    ) -> io::Result<()> {
        atomic_write(
            &self.root.join("format"),
            format!("{CACHE_FORMAT}\n").as_bytes(),
        )?;
        atomic_write(
            &self.shard("objects", &record.output_digest),
            output.as_bytes(),
        )?;
        for (artifact, bytes) in artifacts {
            atomic_write(&self.shard("objects", &artifact.digest), bytes)?;
        }
        let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
        let mut rec_path = self.shard("records", &record.key);
        rec_path.set_extension("json");
        atomic_write(&rec_path, &json)
    }

    /// Looks up `key`. Every failure mode described at the module level
    /// degrades to `None`; see there for the full list.
    pub fn load(&self, key: &str, now_unix: u64) -> Option<CachedEntry> {
        let mut rec_path = self.shard("records", key);
        rec_path.set_extension("json");
        let bytes = fs::read(&rec_path).ok()?;
        let Ok(record) = serde_json::from_slice::<CacheRecord>(&bytes) else {
            let _ = fs::remove_file(&rec_path); // corrupt record
            return None;
        };
        if record.format_version != CACHE_FORMAT {
            return None;
        }
        if let Some(ttl) = record.ttl_seconds
            && now_unix.saturating_sub(record.created_at_unix) > ttl
        {
            return None;
        }
        let Some(output) = self.read_object(&record.output_digest) else {
            let _ = fs::remove_file(&rec_path); // object missing or tampered
            return None;
        };
        Some(CachedEntry {
            record,
            output: String::from_utf8_lossy(&output).into_owned(),
        })
    }

    /// Reads the object for `digest`, digest-verified: content is re-hashed
    /// and compared before returning, so a missing OR tampered object is
    /// always a `None`, never a corrupted `Some`.
    pub fn read_object(&self, digest: &str) -> Option<Vec<u8>> {
        let bytes = fs::read(self.shard("objects", digest)).ok()?;
        let actual = sha256_hex(&bytes);
        (actual == digest).then_some(bytes)
    }

    /// Reads a record as stored, with no validity checks (no ttl, no
    /// format_version, no object verification) and no side effects. For
    /// introspection (`apb cache inspect`), not for deciding a cache hit -
    /// use [`load`](Self::load) for that.
    pub fn inspect(&self, key: &str) -> Option<CacheRecord> {
        let mut rec_path = self.shard("records", key);
        rec_path.set_extension("json");
        let bytes = fs::read(&rec_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Aggregate counts and total on-disk size of `records/` and `objects/`.
    pub fn status(&self) -> StoreStatus {
        let record_paths = self.record_paths();
        let object_paths = self.object_paths();
        let total_bytes = record_paths
            .iter()
            .chain(object_paths.iter())
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        StoreStatus {
            records: record_paths.len(),
            objects: object_paths.len(),
            total_bytes,
        }
    }

    /// Reclaims space in two stages, then a cleanup pass:
    ///
    /// 1. If `older_than_secs` is set, every record whose age (`now_unix -
    ///    created_at_unix`) exceeds it is removed.
    /// 2. If `max_bytes` is set, while the total size of the objects still
    ///    referenced by surviving records exceeds it, the oldest surviving
    ///    record (by `created_at_unix`) is removed.
    /// 3. Any object no surviving record references is deleted.
    ///
    /// A record file that fails to read or parse is left alone (not
    /// counted as removed): pruning degrades corrupt entries silently
    /// rather than erroring, consistent with the rest of this store, but it
    /// does not go out of its way to clean them up either - that stays
    /// [`load`](Self::load)'s job, triggered by an actual lookup.
    pub fn prune(
        &self,
        older_than_secs: Option<u64>,
        max_bytes: Option<u64>,
        now_unix: u64,
    ) -> PruneReport {
        let mut survivors: Vec<(PathBuf, CacheRecord)> = self
            .record_paths()
            .into_iter()
            .filter_map(|path| {
                let bytes = fs::read(&path).ok()?;
                let record: CacheRecord = serde_json::from_slice(&bytes).ok()?;
                Some((path, record))
            })
            .collect();

        let mut removed_records = 0usize;

        if let Some(older_than_secs) = older_than_secs {
            let mut kept = Vec::with_capacity(survivors.len());
            for (path, record) in survivors {
                if now_unix.saturating_sub(record.created_at_unix) > older_than_secs {
                    if fs::remove_file(&path).is_ok() {
                        removed_records += 1;
                    }
                } else {
                    kept.push((path, record));
                }
            }
            survivors = kept;
        }

        if let Some(max_bytes) = max_bytes {
            survivors.sort_by_key(|(_, r)| r.created_at_unix);
            while !survivors.is_empty() && self.referenced_bytes(&survivors) > max_bytes {
                let (path, _) = survivors.remove(0);
                if fs::remove_file(&path).is_ok() {
                    removed_records += 1;
                }
            }
        }

        let referenced = referenced_digests(&survivors);
        let mut removed_objects = 0usize;
        for obj_path in self.object_paths() {
            let hex = obj_path.file_name().and_then(|n| n.to_str());
            let is_referenced = hex.is_some_and(|hex| referenced.contains(hex));
            if !is_referenced && fs::remove_file(&obj_path).is_ok() {
                removed_objects += 1;
            }
        }

        PruneReport {
            removed_records,
            removed_objects,
        }
    }

    /// Total bytes of the distinct objects `records` reference, read from
    /// disk. Shared objects are counted once, matching what would remain
    /// after the final orphan-object cleanup if `records` were the final
    /// surviving set.
    fn referenced_bytes(&self, records: &[(PathBuf, CacheRecord)]) -> u64 {
        referenced_digests(records)
            .iter()
            .filter_map(|hex| {
                fs::metadata(
                    self.root
                        .join("objects")
                        .join(&hex[..2.min(hex.len())])
                        .join(hex),
                )
                .ok()
            })
            .map(|m| m.len())
            .sum()
    }

    /// Removes `records/` and `objects/` entirely (an absent directory is
    /// not an error). Leaves the `format` marker in place.
    pub fn clear(&self) -> io::Result<()> {
        for dir in ["records", "objects"] {
            match fs::remove_dir_all(self.root.join(dir)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// All `records/<shard>/<hex>.json` paths currently on disk. Empty
    /// (never an error) if `records/` does not exist yet.
    fn record_paths(&self) -> Vec<PathBuf> {
        list_shard_dir(&self.root.join("records"), Some("json"))
    }

    /// All `objects/<shard>/<hex>` paths currently on disk. Empty (never an
    /// error) if `objects/` does not exist yet.
    fn object_paths(&self) -> Vec<PathBuf> {
        list_shard_dir(&self.root.join("objects"), None)
    }
}

/// Lists files two levels down from `root` (the `<shard>/<entry>` layout
/// shared by `records/` and `objects/`), optionally filtered by extension.
/// A missing `root` or an unreadable shard yields no entries for that
/// portion rather than an error: directory listing here is best-effort
/// bookkeeping (status/prune/inspect), never the sole path to correctness.
fn list_shard_dir(root: &Path, extension: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(shards) = fs::read_dir(root) else {
        return out;
    };
    for shard in shards.flatten() {
        let Ok(entries) = fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let matches = extension.is_none_or(|ext| path.extension().is_some_and(|e| e == ext));
            if matches {
                out.push(path);
            }
        }
    }
    out
}

/// Hex digests (scheme prefix stripped) of every object `records` reference:
/// each record's output plus its artifacts.
fn referenced_digests(records: &[(PathBuf, CacheRecord)]) -> HashSet<String> {
    records
        .iter()
        .flat_map(|(_, r)| {
            std::iter::once(r.output_digest.as_str())
                .chain(r.artifacts.iter().map(|a| a.digest.as_str()))
        })
        .map(|d| d.trim_start_matches("sha256:").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: &str, output: &str, ttl: Option<u64>) -> CacheRecord {
        CacheRecord {
            format_version: CACHE_FORMAT,
            key: key.into(),
            created_at_unix: 1000,
            node_type: "script".into(),
            provenance: Provenance {
                run_id: "r1".into(),
                playbook_id: "p".into(),
                playbook_version: "1.0.0".into(),
                node_id: "n".into(),
            },
            profile_bundle_digest: None,
            workspace_fingerprint: "sha256:ws".into(),
            verification: Verification {
                workspace_unchanged: true,
                connector_calls: "none".into(),
            },
            output_digest: crate::content::sha256_hex(output.as_bytes()),
            artifacts: vec![],
            ttl_seconds: ttl,
        }
    }

    #[test]
    fn store_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path());
        let key = "sha256:aaaa";
        store
            .store(&record(key, "hello", None), "hello", &[])
            .unwrap();
        let entry = store.load(key, 2000).unwrap();
        assert_eq!(entry.output, "hello");
        assert_eq!(entry.record.provenance.run_id, "r1");
    }

    #[test]
    fn ttl_expiry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path());
        store
            .store(&record("sha256:bbbb", "x", Some(10)), "x", &[])
            .unwrap();
        assert!(store.load("sha256:bbbb", 1005).is_some()); // within ttl
        assert!(store.load("sha256:bbbb", 2000).is_none()); // expired
    }

    #[test]
    fn corrupt_object_is_a_miss_and_record_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path());
        let rec = record("sha256:cccc", "good", None);
        store.store(&rec, "good", &[]).unwrap();
        // overwrite the object with different content, digest now mismatches
        let hex = rec.output_digest.trim_start_matches("sha256:");
        let obj = dir
            .path()
            .join(".apb/cache/objects")
            .join(&hex[..2])
            .join(hex);
        std::fs::write(&obj, "tampered").unwrap();
        assert!(store.load("sha256:cccc", 2000).is_none());
        assert!(store.inspect("sha256:cccc").is_none()); // record removed
    }

    #[test]
    fn key_changes_when_any_part_changes() {
        let base = KeyParts {
            format: CACHE_FORMAT,
            node_def: "{}",
            script_digest: None,
            runner: None,
            rendered_prompt: Some("p"),
            bundle_digest: Some("b"),
            agent: Some("claude"),
            model: Some("m"),
            connector_digests: vec![],
            workspace_fingerprint: "w",
        };
        let k1 = cache_key(&base);
        let k2 = cache_key(&KeyParts {
            rendered_prompt: Some("p2"),
            ..base
        });
        assert_ne!(k1, k2);
        assert!(k1.starts_with("sha256:"));
    }

    #[test]
    fn prune_by_age_removes_old_records_and_orphan_objects() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path());
        let mut old = record("sha256:old1", "old-output", None);
        old.created_at_unix = 1000;
        store.store(&old, "old-output", &[]).unwrap();
        let mut fresh = record("sha256:new1", "new-output", None);
        fresh.created_at_unix = 1900;
        store.store(&fresh, "new-output", &[]).unwrap();

        // now=2000, older_than_secs=500: old1 (age 1000) is pruned, new1 (age 100) survives.
        let report = store.prune(Some(500), None, 2000);
        assert_eq!(report.removed_records, 1);
        assert_eq!(report.removed_objects, 1);
        assert!(store.inspect("sha256:old1").is_none());
        assert!(store.inspect("sha256:new1").is_some());
    }

    #[test]
    fn prune_by_max_bytes_removes_oldest_first_until_budget_fits() {
        let dir = tempfile::tempdir().unwrap();
        let store = CacheStore::open(dir.path());
        let mut r1 = record("sha256:k1", "aaaaaaaaaa", None); // 10 bytes, distinct object
        r1.created_at_unix = 1000;
        store.store(&r1, "aaaaaaaaaa", &[]).unwrap();
        let mut r2 = record("sha256:k2", "bbbbbbbbbb", None); // 10 bytes, distinct object
        r2.created_at_unix = 2000;
        store.store(&r2, "bbbbbbbbbb", &[]).unwrap();
        let mut r3 = record("sha256:k3", "cccccccccc", None); // 10 bytes, distinct object
        r3.created_at_unix = 3000;
        store.store(&r3, "cccccccccc", &[]).unwrap();

        // budget only fits one 10-byte object; the two oldest records (and
        // their now-unreferenced objects) are pruned.
        let report = store.prune(None, Some(10), 4000);
        assert_eq!(report.removed_records, 2);
        assert_eq!(report.removed_objects, 2);
        assert!(store.inspect("sha256:k1").is_none());
        assert!(store.inspect("sha256:k2").is_none());
        assert!(store.inspect("sha256:k3").is_some());
        let status = store.status();
        assert_eq!(status.records, 1);
        assert_eq!(status.objects, 1);
    }
}
