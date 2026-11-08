//! The one signed per-playbook store under `.apb/`: the read, MAC, lock and
//! write half the known-problems store ([`crate::known_problems::store`])
//! and the corrections ledger ([`crate::corrections`]) share.
//!
//! Every file carries a MAC under this installation's key
//! ([`crate::run_origin::tag`]): the file sits in the checkout, and what it
//! holds reaches agent prompts, so a file this installation did not write
//! (one a repository brought) is never shown and never merged into; the
//! next write replaces it. Reading never changes anything.
//!
//! The MAC covers the file's JSON as written, every field but `mac` itself
//! (fields this build does not know included), so the check never depends
//! on how this build would serialise the file again. A file whose MAC does
//! not verify is not this installation's whatever schema it claims, so a
//! planted "newer" file never blocks a write.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Serialize, de::DeserializeOwned};

use crate::fsutil::{DirLock, atomic_write_private, lock_dir};
use crate::scope::Scope;

/// A file of a signed store: what its MAC is bound to, its schema, where
/// its files live, and how an unsigned file is counted.
pub trait SignedDoc: Serialize + DeserializeOwned + Clone {
    /// The purpose the file's MAC is bound to (`apb-known-problems-v1`).
    const MAC_DOMAIN: &'static str;
    /// The schema this build reads and writes.
    const SCHEMA: u32;
    /// The directory name under `.apb/` (`problems`, `corrections`).
    const DIR: &'static str;
    /// The key of the array whose length an unsigned file reports.
    const ENTRIES_KEY: &'static str;

    /// The fresh file of `playbook` under `scope`, as a read of a missing
    /// or unsigned file returns it.
    fn fresh(scope: Scope, playbook: &str) -> Self;
    /// `(scope, playbook)` as the file carries them: a signed file put
    /// under another playbook's name does not count for it.
    fn identity(&self) -> (&str, &str);
    /// The file's MAC, when it carries one.
    fn mac(&self) -> Option<&str>;
    /// Sets or clears the file's MAC.
    fn set_mac(&mut self, mac: Option<String>);
    /// Sets the file's schema (a write stamps [`SignedDoc::SCHEMA`]).
    fn set_schema(&mut self, schema: u32);
}

/// The file as one read found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    pub file: T,
    /// Whether this installation wrote it as it is. An unsigned file is
    /// read as a fresh one: its entries are never shown.
    pub signed: bool,
    /// How many entries an unsigned file holds (its texts and ids are not
    /// read).
    pub unsigned_entries: usize,
}

/// Why the file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("playbook id `{0}` is not a safe name")]
    InvalidId(String),
    #[error("the file has schema {0}, newer than this apb understands")]
    NewerSchema(u32),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The directory of `scope`'s files under the execution root `root`.
pub fn dir(root: &Path, dir_name: &str, scope: Scope) -> PathBuf {
    root.join(".apb").join(dir_name).join(scope.as_str())
}

fn checked_id(playbook: &str) -> Result<(), StoreError> {
    if crate::registry::is_safe_segment(playbook) {
        Ok(())
    } else {
        Err(StoreError::InvalidId(playbook.to_string()))
    }
}

fn file<T: SignedDoc>(root: &Path, scope: Scope, playbook: &str) -> PathBuf {
    dir(root, T::DIR, scope).join(format!("{playbook}.json"))
}

/// Reads `playbook`'s file without changing anything: a fresh signed file
/// when there is none, an unsigned fresh one when this installation did
/// not write it (or it does not parse). Only a signed file's entries are
/// ever read: an unsigned file's texts and ids reach no surface.
pub fn read<T: SignedDoc>(
    root: &Path,
    scope: Scope,
    playbook: &str,
) -> Result<Loaded<T>, StoreError> {
    checked_id(playbook)?;
    let raw = match std::fs::read(file::<T>(root, scope, playbook)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded {
                file: T::fresh(scope, playbook),
                signed: true,
                unsigned_entries: 0,
            });
        }
        Err(e) => return Err(e.into()),
    };
    let unsigned = |count: usize| {
        Ok(Loaded {
            file: T::fresh(scope, playbook),
            signed: false,
            unsigned_entries: count,
        })
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return unsigned(0);
    };
    let count = value
        .get(T::ENTRIES_KEY)
        .and_then(|p| p.as_array())
        .map_or(0, Vec::len);
    let mac = value
        .as_object_mut()
        .and_then(|o| o.remove("mac"))
        .and_then(|m| m.as_str().map(str::to_string));
    let signed_here = match (mac, bytes_of(&value)) {
        (Some(mac), Ok(bytes)) => crate::run_origin::verify_tag(T::MAC_DOMAIN, &bytes, &mac),
        _ => false,
    };
    if !signed_here {
        return unsigned(count);
    }
    let schema = value.get("schema").and_then(|v| v.as_u64()).unwrap_or(0);
    if schema > u64::from(T::SCHEMA) {
        return Err(StoreError::NewerSchema(
            u32::try_from(schema).unwrap_or(u32::MAX),
        ));
    }
    let Ok(f) = serde_json::from_value::<T>(value) else {
        return unsigned(count);
    };
    // The MAC covers the scope and id inside the file: a signed file put
    // under another playbook's name does not count for it.
    if f.identity() != (scope.as_str(), playbook) {
        return unsigned(count);
    }
    Ok(Loaded {
        file: f,
        signed: true,
        unsigned_entries: 0,
    })
}

/// The bytes a MAC covers: the JSON value without `mac`, every object's
/// keys sorted explicitly (so the bytes never depend on whether a crate in
/// the build turns on serde_json's `preserve_order`).
pub fn bytes_of(value: &serde_json::Value) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&canonical(value)).map_err(io::Error::other)
}

/// `value` with every object's keys in sorted order.
fn canonical(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// Changes `playbook`'s file under its lock: `change` gets the signed file
/// (a fresh one in place of an unsigned file) and the result is written
/// signed. Nothing is written when `change` reports no change.
pub fn update<T, R, E>(
    root: &Path,
    scope: Scope,
    playbook: &str,
    change: impl FnOnce(&mut T) -> Result<(R, bool), E>,
) -> Result<R, E>
where
    T: SignedDoc,
    E: From<StoreError>,
{
    checked_id(playbook)?;
    let _held: DirLock = lock_dir(&dir(root, T::DIR, scope), &format!("{playbook}.json.lock"))
        .map_err(StoreError::Io)?;
    // An unsigned file reads as a fresh one: it is replaced.
    let mut f = read::<T>(root, scope, playbook)?.file;
    let (out, changed) = change(&mut f)?;
    if changed {
        write(root, scope, playbook, f)?;
    }
    Ok(out)
}

fn write<T: SignedDoc>(
    root: &Path,
    scope: Scope,
    playbook: &str,
    mut f: T,
) -> Result<(), StoreError> {
    f.set_mac(None);
    f.set_schema(T::SCHEMA);
    // The MAC covers the canonical JSON without `mac`, as the read checks
    // it: the value's own bytes, keys sorted explicitly.
    let value = serde_json::to_value(&f).map_err(io::Error::other)?;
    let covered = bytes_of(&value)?;
    f.set_mac(Some(crate::run_origin::tag(T::MAC_DOMAIN, &covered)?));
    let bytes = serde_json::to_vec_pretty(&f).map_err(io::Error::other)?;
    Ok(atomic_write_private(
        &file::<T>(root, scope, playbook),
        &bytes,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// A small test doc: two entries keyed `entries`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
    struct Doc {
        schema: u32,
        scope: String,
        playbook: String,
        #[serde(default)]
        entries: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mac: Option<String>,
    }

    impl SignedDoc for Doc {
        const MAC_DOMAIN: &'static str = "apb-test-doc-v1";
        const SCHEMA: u32 = 1;
        const DIR: &'static str = "test-docs";
        const ENTRIES_KEY: &'static str = "entries";

        fn fresh(scope: Scope, playbook: &str) -> Self {
            Doc {
                schema: Self::SCHEMA,
                scope: scope.as_str().to_string(),
                playbook: playbook.to_string(),
                entries: Vec::new(),
                mac: None,
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

    /// An unsigned file reads as a fresh one and reports its entry count;
    /// a file signed under another domain reads unsigned; a misplaced file
    /// reads unsigned; a newer schema refuses an update; an unsafe id is
    /// `InvalidId`.
    #[test]
    fn unsigned_misplaced_newer_and_unsafe_are_handled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let scope = Scope::Project;

        // No file: a fresh signed read.
        let loaded = read::<Doc>(root, scope, "p").unwrap();
        assert!(loaded.signed && loaded.file.entries.is_empty());

        // An unsigned file: counted, never shown, replaced on the next
        // write.
        let path = dir(root, Doc::DIR, scope).join("p.json");
        std::fs::create_dir_all(dir(root, Doc::DIR, scope)).unwrap();
        std::fs::write(
            &path,
            r#"{"schema":1,"scope":"project","playbook":"p","entries":["a","b"]}"#,
        )
        .unwrap();
        let loaded = read::<Doc>(root, scope, "p").unwrap();
        assert!(!loaded.signed);
        assert_eq!(loaded.unsigned_entries, 2);
        update(root, scope, "p", |f: &mut Doc| {
            f.entries.push("c".into());
            Ok::<_, StoreError>(((), true))
        })
        .unwrap();
        let loaded = read::<Doc>(root, scope, "p").unwrap();
        assert!(loaded.signed);
        assert_eq!(loaded.file.entries, ["c"]);

        // A file signed under another domain reads unsigned.
        {
            let mut value = serde_json::to_value(&loaded.file).unwrap();
            value.as_object_mut().unwrap().remove("mac");
            let bytes = bytes_of(&value).unwrap();
            let tagged = crate::run_origin::tag("apb-other-domain-v9", &bytes).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("mac".into(), serde_json::Value::String(tagged));
            std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        }
        let loaded = read::<Doc>(root, scope, "p").unwrap();
        assert!(!loaded.signed, "a foreign domain's MAC verified");

        // A signed file put under another playbook's name reads unsigned.
        let loaded = read::<Doc>(root, scope, "p").unwrap();
        let mut f = loaded.file.clone();
        f.playbook = "q".into();
        write(root, scope, "p", f).unwrap_or(());
        std::fs::write(&path, {
            // A signed file of q, renamed to p on disk.
            let renamed = tempfile::tempdir().unwrap();
            update(renamed.path(), scope, "q", |f: &mut Doc| {
                f.entries.push("x".into());
                Ok::<_, StoreError>(((), true))
            })
            .unwrap();
            std::fs::read(dir(renamed.path(), Doc::DIR, scope).join("q.json")).unwrap()
        })
        .unwrap();
        let loaded = read::<Doc>(root, scope, "p").unwrap();
        assert!(!loaded.signed, "a renamed file verified");

        // A newer schema refuses an update: the file must be genuinely
        // signed (an unsigned "newer" file reads as unsigned).
        std::fs::write(&path, {
            let mut value = serde_json::to_value(Doc::fresh(scope, "p")).unwrap();
            value["schema"] = serde_json::Value::from(2);
            let covered = bytes_of(&value).unwrap();
            let tagged = crate::run_origin::tag(Doc::MAC_DOMAIN, &covered).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("mac".into(), serde_json::Value::String(tagged));
            serde_json::to_vec_pretty(&value).unwrap()
        })
        .unwrap();
        let e = update(root, scope, "p", |_: &mut Doc| {
            Ok::<_, StoreError>(((), false))
        })
        .unwrap_err();
        assert!(matches!(e, StoreError::NewerSchema(2)), "{e:?}");

        // An unsafe id is InvalidId before any path is built.
        assert!(matches!(
            read::<Doc>(root, scope, "../evil"),
            Err(StoreError::InvalidId(_))
        ));
    }
}
