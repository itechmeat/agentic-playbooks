//! Write-once, content-addressed files inside a run directory:
//! `runs/<id>/<dir>/<sha256>.<ext>`. Stored definitions and binding
//! snapshots both live in one. A file is named by the sha256 of its exact
//! bytes, written privately and atomically once and never rewritten, and a
//! read returns the bytes only when they still hash to their name.

use std::path::{Path, PathBuf};

/// One store: a directory of the run and the extension of its files.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunStore {
    pub dir: &'static str,
    pub ext: &'static str,
}

/// The digest a file is stored under: sha256 of its exact bytes,
/// lower-case hex.
pub fn digest(bytes: &[u8]) -> String {
    let prefixed = apb_core::content::sha256_hex(bytes);
    prefixed
        .strip_prefix("sha256:")
        .unwrap_or(&prefixed)
        .to_string()
}

fn is_hex_digest(d: &str) -> bool {
    d.len() == 64
        && d.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl RunStore {
    pub(crate) fn path(self, run_dir: &Path, digest: &str) -> PathBuf {
        run_dir
            .join(self.dir)
            .join(format!("{digest}.{}", self.ext))
    }

    /// Stores `bytes` and returns their digest. Write-once: a file already
    /// stored under the digest is kept. Refuses a store entry that is not a
    /// real directory (a link included) and a stored entry that is not a
    /// regular file.
    pub(crate) fn store(self, run_dir: &Path, bytes: &[u8]) -> std::io::Result<String> {
        let digest = digest(bytes);
        let dir = run_dir.join(self.dir);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(std::io::Error::other(format!(
                    "the run's {} entry is not a directory",
                    self.dir
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                apb_core::fsutil::create_dir_under(run_dir, &dir)?;
            }
            Err(e) => return Err(e),
        }
        let path = self.path(run_dir, &digest);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() => return Ok(digest),
            Ok(_) => {
                return Err(std::io::Error::other(format!(
                    "a stored {} entry is not a regular file",
                    self.dir
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        apb_core::fsutil::atomic_write_private_under(run_dir, &path, bytes)?;
        Ok(digest)
    }

    /// The bytes stored under `digest`, when they are there, regular, and
    /// hash to `digest`.
    pub(crate) fn read(self, run_dir: &Path, digest: &str) -> Option<Vec<u8>> {
        use std::io::Read;
        if !is_hex_digest(digest) {
            return None;
        }
        let dir = run_dir.join(self.dir);
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_dir()) {
            return None;
        }
        let mut file =
            apb_core::fsutil::open_regular_no_follow(&self.path(run_dir, digest)).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        (self::digest(&bytes) == digest).then_some(bytes)
    }
}
