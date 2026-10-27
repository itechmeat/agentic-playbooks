//! The memory file of one playbook: `<root>/.apb/memory/<scope>/<id>.json`,
//! written atomically (0600) under a lock of its directory.
//!
//! Reading never changes anything: a file that cannot be parsed reads as
//! empty with a diagnostic, and only a writer holding the lock moves it
//! aside (`<id>.json.corrupt-<ms>`). A file of a newer format is left
//! untouched and never rewritten.
//!
//! The memory of a global playbook carries a MAC under this installation's
//! key ([`crate::run_origin::tag`]). The file sits in the checkout, while the
//! playbook it seeds is trusted on the whole machine: a file the checkout
//! brought (not written by apb here) is ignored. A project playbook's
//! memory needs no such binding, since its definition comes from the same
//! checkout.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::key::{MemoryError, MemoryKey};
use crate::fsutil::{DirLock, atomic_write_private, lock_dir};
use crate::scope::Scope;

/// The memory file format this build reads and writes.
pub(super) const FORMAT: u32 = 1;

/// The purpose a global memory file's MAC is bound to.
const MAC_DOMAIN: &str = "apb-run-memory-v1";

/// One playbook's remembered run input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct MemoryFile {
    pub format: u32,
    pub scope: String,
    pub id: String,
    /// [`super::params_schema_digest`] of the params the values were saved
    /// against.
    pub params_schema: String,
    pub saved_version: String,
    pub saved_at_ms: u64,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// `Some("")` is an instruction cleared on purpose: it stops the
    /// fallback to the legacy draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// The installation MAC of a global playbook's memory, over the file
    /// without this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

impl MemoryFile {
    pub(super) fn is_empty(&self) -> bool {
        self.params.is_empty() && self.instruction.is_none()
    }

    /// The bytes the MAC covers: the file as written, without the MAC.
    fn signed_bytes(&self) -> io::Result<Vec<u8>> {
        let unsigned = MemoryFile {
            mac: None,
            ..self.clone()
        };
        serde_json::to_vec(&unsigned).map_err(io::Error::other)
    }
}

/// Where memory of `scope` lives under the execution root `root`.
pub(super) fn dir(root: &Path, key: &MemoryKey) -> PathBuf {
    root.join(".apb").join("memory").join(key.scope().as_str())
}

fn file(root: &Path, key: &MemoryKey) -> PathBuf {
    dir(root, key).join(format!("{}.json", key.id()))
}

/// Takes the lock of `key`'s file.
pub(super) fn lock(root: &Path, key: &MemoryKey) -> Result<DirLock, MemoryError> {
    Ok(lock_dir(
        &dir(root, key),
        &format!("{}.json.lock", key.id()),
    )?)
}

/// What is stored for a key, as one read found it.
enum Found {
    None,
    File(MemoryFile),
    /// Not parseable: a locked writer moves it aside.
    Corrupt,
    /// Parseable but not this key's memory as saved here: ignored, and
    /// replaced by the next write.
    Foreign,
}

/// Reads `key`'s file without changing anything. `Ok(None)` when there is
/// none, or when it is corrupt or not bound to this installation
/// (`diagnostics` says so). A newer format is [`MemoryError::NewerFormat`].
pub(super) fn read(
    root: &Path,
    key: &MemoryKey,
    diagnostics: &mut Vec<String>,
) -> Result<Option<MemoryFile>, MemoryError> {
    match find(root, key, diagnostics)? {
        Found::File(f) => Ok(Some(f)),
        Found::None | Found::Corrupt | Found::Foreign => Ok(None),
    }
}

/// [`read`] for a writer holding the lock (`_held`): a corrupt file is moved
/// aside under a name of its own, so the write that follows starts empty
/// and the old bytes stay for inspection.
pub(super) fn read_locked(
    root: &Path,
    key: &MemoryKey,
    diagnostics: &mut Vec<String>,
    _held: &DirLock,
) -> Result<Option<MemoryFile>, MemoryError> {
    match find(root, key, diagnostics)? {
        Found::File(f) => Ok(Some(f)),
        Found::None | Found::Foreign => Ok(None),
        Found::Corrupt => {
            let path = file(root, key);
            let aside = path.with_extension(format!("json.corrupt-{}", crate::clock::now_ms()));
            match std::fs::rename(&path, &aside) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(None),
            }
        }
    }
}

fn find(root: &Path, key: &MemoryKey, diagnostics: &mut Vec<String>) -> Result<Found, MemoryError> {
    let raw = match std::fs::read(file(root, key)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Found::None),
        Err(e) => return Err(e.into()),
    };
    // The format first, so a newer file whose other fields changed shape is
    // left alone rather than taken for a corrupt one.
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        format: u32,
    }
    if let Ok(Probe { format }) = serde_json::from_slice::<Probe>(&raw)
        && format > FORMAT
    {
        return Err(MemoryError::NewerFormat(format));
    }
    let Ok(f) = serde_json::from_slice::<MemoryFile>(&raw) else {
        diagnostics.push(
            "the remembered input could not be read; it is ignored and the next save replaces it"
                .to_string(),
        );
        return Ok(Found::Corrupt);
    };
    if key.scope() == Scope::Global && !bound_here(&f) {
        diagnostics.push(
            "the remembered input of this global playbook was not saved by this apb installation (or by an older build); it is ignored: save it again to keep it"
                .to_string(),
        );
        return Ok(Found::Foreign);
    }
    // The MAC covers the scope and id inside the file: a signed file put
    // under another key's name does not count for that key.
    if f.scope != key.scope().as_str() || f.id != key.id() {
        diagnostics
            .push("the remembered input belongs to another playbook; it is ignored".to_string());
        return Ok(Found::Foreign);
    }
    Ok(Found::File(f))
}

/// Whether `f` carries this installation's MAC.
fn bound_here(f: &MemoryFile) -> bool {
    match (&f.mac, f.signed_bytes()) {
        (Some(mac), Ok(bytes)) => crate::run_origin::verify_tag(MAC_DOMAIN, &bytes, mac),
        _ => false,
    }
}

/// Writes `f` as `key`'s file, or removes the file when `f` holds nothing.
/// A global playbook's memory gets this installation's MAC. The caller
/// holds [`lock`].
pub(super) fn write(root: &Path, key: &MemoryKey, f: &MemoryFile) -> Result<(), MemoryError> {
    let path = file(root, key);
    if f.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    let mut f = MemoryFile {
        mac: None,
        ..f.clone()
    };
    if key.scope() == Scope::Global {
        // Unsigned, the file would be ignored by the next read: refuse to
        // write it rather than report memory that does not stick.
        f.mac = Some(crate::run_origin::tag(MAC_DOMAIN, &f.signed_bytes()?)?);
    }
    let bytes = serde_json::to_vec_pretty(&f).map_err(io::Error::other)?;
    Ok(atomic_write_private(&path, &bytes)?)
}
