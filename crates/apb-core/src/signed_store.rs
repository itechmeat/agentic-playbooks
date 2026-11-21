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

    /// Whether the file is read beneath the execution root without
    /// following a link at any component, and only as a regular file with
    /// one link: a file that is not is read as unsigned. Off by default.
    const NO_LINKS: bool = false;

    /// Whether a signed file belongs to the project at `root`: a file the
    /// MAC covers but another project's store wrote does not count here.
    /// Every file by default.
    fn belongs_to(&self, _root: &Path) -> bool {
        true
    }
}

/// The most bytes a [`SignedDoc::NO_LINKS`] file is read up to, and the one
/// limit a write of such a file is refused past: the reader's constant,
/// named once, so a save can never make a store its reader refuses.
pub const NO_LINKS_CAP: u64 = 8 * 1024 * 1024;

/// The bytes of `playbook`'s file read beneath `root` with no link
/// followed: `Ok(None)` when there is none, `Err(None)` when an entry is
/// there that is not a plain regular file (read as unsigned).
fn read_no_links<T: SignedDoc>(
    root: &Path,
    scope: Scope,
    playbook: &str,
) -> Result<Option<Vec<u8>>, Option<io::Error>> {
    use crate::fsutil::beneath::{Beneath, CappedRead, ReadRefusal};
    let anchor = match Beneath::open(root) {
        Ok(a) => a,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Some(e)),
    };
    let rel = format!(".apb/{}/{}/{playbook}.json", T::DIR, scope.as_str());
    match anchor.read_regular_capped(&rel, NO_LINKS_CAP) {
        Ok(CappedRead::Read { bytes, .. }) => Ok(Some(bytes)),
        Ok(CappedRead::Refused(ReadRefusal::Missing)) => Ok(None),
        Ok(CappedRead::Refused(_)) => Err(None),
        Err(e) => Err(Some(e)),
    }
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
    /// A present file that does not verify or parse, under a transaction
    /// that refuses to replace it ([`Unsigned::Refuse`]).
    #[error("{} does not verify", .0.display())]
    Unverified(PathBuf),
    /// A write the reader's own cap refuses: the signed bytes of the
    /// replacement would be more than the read takes, so the stored file
    /// would stop reading. Nothing was written.
    #[error(
        "{} would be {} bytes, over the {} bytes its reader reads; nothing was written",
        .path.display(),
        .wrote,
        .limit
    )]
    TooLarge {
        path: PathBuf,
        wrote: u64,
        limit: u64,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What a transaction does with a present file that does not verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsigned {
    /// Read it as a fresh file and replace it on the next write (the known
    /// problems and the corrections ledger).
    Replace,
    /// Refuse it: [`StoreError::Unverified`], nothing written.
    Refuse,
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

/// The path of `playbook`'s file under `scope`.
pub fn file<T: SignedDoc>(root: &Path, scope: Scope, playbook: &str) -> PathBuf {
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
    let fresh = || {
        Ok(Loaded {
            file: T::fresh(scope, playbook),
            signed: true,
            unsigned_entries: 0,
        })
    };
    let unsigned = |count: usize| {
        Ok(Loaded {
            file: T::fresh(scope, playbook),
            signed: false,
            unsigned_entries: count,
        })
    };
    let raw = if T::NO_LINKS {
        match read_no_links::<T>(root, scope, playbook) {
            Ok(Some(raw)) => raw,
            Ok(None) => return fresh(),
            Err(None) => return unsigned(0),
            Err(Some(e)) => return Err(e.into()),
        }
    } else {
        match std::fs::read(file::<T>(root, scope, playbook)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return fresh(),
            Err(e) => return Err(e.into()),
        }
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
    if f.identity() != (scope.as_str(), playbook) || !f.belongs_to(root) {
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
    transact(root, scope, playbook, Unsigned::Replace, change)
}

/// [`update`] with the choice of what a present file that does not verify
/// means: under the file's lock, `change` gets the signed file;
/// [`Unsigned::Refuse`] returns [`StoreError::Unverified`] for a present
/// file that does not verify or parse, writing nothing. The lock is held
/// only for the transaction.
pub fn transact<T, R, E>(
    root: &Path,
    scope: Scope,
    playbook: &str,
    unsigned: Unsigned,
    change: impl FnOnce(&mut T) -> Result<(R, bool), E>,
) -> Result<R, E>
where
    T: SignedDoc,
    E: From<StoreError>,
{
    checked_id(playbook)?;
    let _held: DirLock = lock_dir(&dir(root, T::DIR, scope), &format!("{playbook}.json.lock"))
        .map_err(StoreError::Io)?;
    let loaded = read::<T>(root, scope, playbook)?;
    if !loaded.signed && unsigned == Unsigned::Refuse {
        return Err(StoreError::Unverified(file::<T>(root, scope, playbook)).into());
    }
    // Under `Replace` an unsigned file reads as a fresh one: it is replaced.
    let mut f = loaded.file;
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
    // A file the reader caps is never written over its own read limit: a
    // save that would make the store unreadable is refused here, before
    // the replacement, and the stored file stays as it was.
    if T::NO_LINKS {
        let wrote = bytes.len() as u64;
        if wrote > NO_LINKS_CAP {
            return Err(StoreError::TooLarge {
                path: file::<T>(root, scope, playbook),
                wrote,
                limit: NO_LINKS_CAP,
            });
        }
    }
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

    fn add(root: &Path, unsigned: Unsigned, entry: &str) -> Result<(), StoreError> {
        transact(root, Scope::Project, "p", unsigned, |f: &mut Doc| {
            f.entries.push(entry.to_string());
            Ok::<_, StoreError>(((), true))
        })
    }

    /// `Refuse` on garbage and on a valid shape without a MAC is
    /// `Unverified` and leaves the bytes as they were.
    #[test]
    fn refuse_leaves_an_unverified_file_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let path = file::<Doc>(root, Scope::Project, "p");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for bytes in [
            &b"not json at all"[..],
            br#"{"schema":1,"scope":"project","playbook":"p","entries":["a"]}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            let e = add(root, Unsigned::Refuse, "x").unwrap_err();
            assert!(
                matches!(e, StoreError::Unverified(ref p) if *p == path),
                "{e:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        // A missing file is fresh, not unverified.
        std::fs::remove_file(&path).unwrap();
        add(root, Unsigned::Refuse, "x").unwrap();
        add(root, Unsigned::Refuse, "y").unwrap();
        let loaded = read::<Doc>(root, Scope::Project, "p").unwrap();
        assert!(loaded.signed);
        assert_eq!(loaded.file.entries, ["x", "y"]);
    }

    /// `Replace` replaces an unsigned file, as `update` always did.
    #[test]
    fn replace_replaces_an_unverified_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let path = file::<Doc>(root, Scope::Project, "p");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"garbage").unwrap();
        add(root, Unsigned::Replace, "x").unwrap();
        let loaded = read::<Doc>(root, Scope::Project, "p").unwrap();
        assert!(loaded.signed);
        assert_eq!(loaded.file.entries, ["x"]);
    }

    /// Two writers interleaving through `transact` lose nothing.
    #[test]
    fn transact_serializes_writers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let threads: Vec<_> = (0..2)
            .map(|t| {
                let root = root.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        transact(
                            &root,
                            Scope::Project,
                            "p",
                            Unsigned::Refuse,
                            |f: &mut Doc| {
                                let n = f.entries.len();
                                // Widen the window between the read and the write.
                                std::thread::yield_now();
                                f.entries.push(format!("{t}-{i}-{n}"));
                                Ok::<_, StoreError>(((), true))
                            },
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let loaded = read::<Doc>(&root, Scope::Project, "p").unwrap();
        assert!(loaded.signed, "the MAC does not verify");
        assert_eq!(loaded.file.entries.len(), 200);
    }

    /// A doc the reader caps: a save whose signed bytes would pass the
    /// read limit is refused and leaves the stored file as it was, and
    /// every earlier save still reads as it was written.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
    struct CappedDoc {
        schema: u32,
        scope: String,
        playbook: String,
        #[serde(default)]
        entries: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mac: Option<String>,
    }

    impl SignedDoc for CappedDoc {
        const MAC_DOMAIN: &'static str = "apb-test-capped-doc-v1";
        const SCHEMA: u32 = 1;
        const DIR: &'static str = "test-capped-docs";
        const ENTRIES_KEY: &'static str = "entries";
        const NO_LINKS: bool = true;

        fn fresh(scope: Scope, playbook: &str) -> Self {
            CappedDoc {
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

    /// A signed document of exactly the read cap is taken; one byte more
    /// is refused and the stored file stays at the cap.
    #[test]
    fn a_write_of_exactly_the_read_cap_is_taken_and_one_byte_more_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let scope = Scope::Project;
        let path = file::<CappedDoc>(root, scope, "p");
        // One small entry, then one entry grown so the signed bytes land
        // exactly on the cap: an entry's characters are the file's bytes
        // one for one, and the mac's length does not move.
        transact(root, scope, "p", Unsigned::Refuse, |f: &mut CappedDoc| {
            f.entries.push("x".to_string());
            Ok::<_, StoreError>(((), true))
        })
        .unwrap();
        let small = std::fs::read(&path).unwrap().len() as u64;
        let delta = (NO_LINKS_CAP - small) as usize;
        transact(root, scope, "p", Unsigned::Refuse, |f: &mut CappedDoc| {
            f.entries = vec!["x".repeat(1 + delta)];
            Ok::<_, StoreError>(((), true))
        })
        .unwrap();
        let at_cap = std::fs::read(&path).unwrap();
        assert_eq!(
            at_cap.len() as u64,
            NO_LINKS_CAP,
            "the boundary write is taken"
        );
        // One byte more: refused, the file stays exactly at the cap.
        let refused = transact(root, scope, "p", Unsigned::Refuse, |f: &mut CappedDoc| {
            f.entries = vec!["x".repeat(2 + delta)];
            Ok::<_, StoreError>(((), true))
        });
        match refused {
            Err(StoreError::TooLarge { wrote, limit, .. }) => {
                assert_eq!(wrote, NO_LINKS_CAP + 1, "{wrote}");
                assert_eq!(limit, NO_LINKS_CAP);
            }
            other => panic!("one byte over the cap is TooLarge: {other:?}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), at_cap, "nothing written");
    }

    #[test]
    fn a_write_past_the_read_cap_is_refused_and_the_file_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let scope = Scope::Project;
        // Entries of about a megabyte each, until one save would cross the
        // reader's limit.
        let big = "x".repeat(1024 * 1024);
        let path = file::<CappedDoc>(root, scope, "p");
        for i in 0..100 {
            let before = std::fs::read(&path).unwrap_or_default();
            let saved = i;
            let refused = transact(root, scope, "p", Unsigned::Refuse, |f: &mut CappedDoc| {
                f.entries.push(format!("{big}{saved}"));
                Ok::<_, StoreError>(((), true))
            })
            .is_err();
            if refused {
                // The stored file is exactly what the last good save wrote.
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    before,
                    "the refused write left the file as it was"
                );
                // Everything saved before still reads as this build wrote it.
                let loaded = read::<CappedDoc>(root, scope, "p").unwrap();
                assert!(loaded.signed, "the store still reads");
                assert_eq!(loaded.file.entries.len(), i, "every earlier entry");
                // And a read of the bytes is inside the cap.
                assert!((before.len() as u64) <= NO_LINKS_CAP, "saved under the cap");
                return;
            }
        }
        panic!("no save was refused past the read cap");
    }
}
