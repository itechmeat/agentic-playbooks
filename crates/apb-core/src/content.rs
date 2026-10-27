//! TOCTOU-safe snapshot of a content tree and its digest (spec
//! 2026-07-12-agent-profiles, section 3.5), plus the profile bundle digest
//! (section 3.1).
//!
//! Key invariant: the digest is computed OVER THE COPY. `snapshot_tree` reads
//! the source file, writes it into the snapshot's staging directory, and
//! updates the hasher in a single pass - there is no window between "what we
//! hashed" and "what we put into the run" during which the source could be
//! swapped out.
//!
//! The encoding is collision-resistant: the walk is deterministic (names are
//! sorted within each directory), every field is length-prefixed, the tree
//! and the bundle use different domain tags, and file content is folded in as
//! a fixed-size sub-digest.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Limits on walking a skill/profile tree. Exceeding any of them is a hard
/// failure, not a silent truncation.
#[derive(Debug, Clone, Copy)]
pub struct TreeLimits {
    pub max_total_bytes: u64,
    pub max_files: u32,
    pub max_depth: u32,
    pub max_file_bytes: u64,
}

impl Default for TreeLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 64 * 1024 * 1024,
            max_files: 512,
            max_depth: 16,
            max_file_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("unsupported file type at {0}")]
    Unsupported(PathBuf),
    #[error("symlink escapes content root at {0}")]
    Escape(PathBuf),
    #[error("content exceeds limit: {0}")]
    TooLarge(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Writes the length (u64 LE) followed by the bytes - an unambiguous field
/// separator.
fn lp(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

struct Acc {
    files: u32,
    total: u64,
}

/// One regular file of a tree read by [`read_tree_files`]: its `/`-separated
/// path relative to the tree root, its bytes, and whether it carried an
/// executable bit (always false off unix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    pub rel: String,
    pub bytes: Vec<u8>,
    pub executable: bool,
}

/// Copies the `src` tree into `staging_dst` and returns the digest OF THE
/// COPY. `staging_dst` MUST NOT already exist - otherwise this fails. The
/// same condition rules out source and destination being the same path
/// (source exists, so dst != src), which in turn rules out clobbering the
/// source and removing someone else's directory on cleanup. On an error
/// AFTER staging was created, it is removed entirely (no partial copy is
/// left behind).
pub fn snapshot_tree(
    src: &Path,
    staging_dst: &Path,
    limits: &TreeLimits,
) -> Result<String, ContentError> {
    let canonical_root = fs::canonicalize(src)?;
    // Create only the PARENT of staging, then atomically "claim" staging_dst
    // itself via create_dir (not create_dir_all): an existing directory is a
    // hard failure with no TOCTOU window between the exists-check and the
    // creation. This also rules out source and destination coinciding, and
    // removing someone else's directory on cleanup.
    if let Some(parent) = staging_dst.parent() {
        fs::create_dir_all(parent)?;
        // staging must NOT live inside source: otherwise the walk would
        // recursively snapshot its own growing copy.
        if let Ok(cp) = fs::canonicalize(parent)
            && cp.starts_with(&canonical_root)
        {
            return Err(ContentError::Unsupported(staging_dst.to_path_buf()));
        }
    }
    match fs::create_dir(staging_dst) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ContentError::Unsupported(staging_dst.to_path_buf()));
        }
        Err(e) => return Err(e.into()),
    }
    let mut walker = Walker::new(&canonical_root, limits, None);
    match walker.dir(&canonical_root, Some(staging_dst), "", 0) {
        Ok(()) => Ok(walker.digest()),
        Err(e) => {
            let _ = fs::remove_dir_all(staging_dst);
            Err(e)
        }
    }
}

/// Digest of a tree that already exists (e.g. a published copy inside a
/// run). Same procedure as `snapshot_tree`, but without writing a copy.
pub fn tree_digest(root: &Path, limits: &TreeLimits) -> Result<String, ContentError> {
    let canonical_root = fs::canonicalize(root)?;
    let mut walker = Walker::new(&canonical_root, limits, None);
    walker.dir(&canonical_root, None, "", 0)?;
    Ok(walker.digest())
}

/// Reads every regular file of the tree under `root` in the same
/// deterministic walk, under the same limits, as [`tree_digest`], and returns
/// them together with the tree digest. Meant for carrying a tree as a plain
/// file list (a playbook bundle): what a file list cannot represent is an
/// error instead of a silent loss, so writing the list back yields a tree
/// with the same digest. That rejects symlinks and any directory that holds
/// no regular file (both count in the digest but cannot travel as files).
pub fn read_tree_files(
    root: &Path,
    limits: &TreeLimits,
) -> Result<(Vec<TreeFile>, String), ContentError> {
    let canonical_root = fs::canonicalize(root)?;
    let mut walker = Walker::new(&canonical_root, limits, Some(Vec::new()));
    walker.dir(&canonical_root, None, "", 0)?;
    let files = walker.collected.take().unwrap_or_default();
    Ok((files, walker.digest()))
}

/// One deterministic walk of a tree: hashes every entry and, depending on
/// the caller, copies it into a staging directory or collects its files.
struct Walker<'a> {
    canonical_root: &'a Path,
    limits: &'a TreeLimits,
    h: Sha256,
    acc: Acc,
    /// `Some` when the walk collects regular files ([`read_tree_files`]).
    collected: Option<Vec<TreeFile>>,
}

impl<'a> Walker<'a> {
    fn new(
        canonical_root: &'a Path,
        limits: &'a TreeLimits,
        collected: Option<Vec<TreeFile>>,
    ) -> Self {
        let mut h = Sha256::new();
        h.update(b"apb-tree-v1\0");
        Walker {
            canonical_root,
            limits,
            h,
            acc: Acc { files: 0, total: 0 },
            collected,
        }
    }

    fn digest(self) -> String {
        format!("sha256:{}", hex_lower(&self.h.finalize()))
    }

    /// Recursive deterministic walk of directory `src_dir`. `rel_prefix` is
    /// the path from the tree root down to `src_dir` (`""` at the root).
    /// `dst_dir` is the corresponding directory in staging (Some when
    /// copying). Every entry is hashed as a type tag (`D`/`F`/`L`) plus a
    /// length-prefixed relative path; a file additionally contributes a
    /// fixed-size content sub-digest, and a symlink contributes its target.
    fn dir(
        &mut self,
        src_dir: &Path,
        dst_dir: Option<&Path>,
        rel_prefix: &str,
        depth: u32,
    ) -> Result<(), ContentError> {
        if depth > self.limits.max_depth {
            return Err(ContentError::TooLarge(format!(
                "depth exceeds {}",
                self.limits.max_depth
            )));
        }
        // We do NOT swallow errors for individual entries: otherwise the digest
        // would describe an incomplete tree, and the snapshot would silently
        // lose files.
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        for entry in fs::read_dir(src_dir)? {
            names.push(entry?.file_name());
        }
        names.sort();
        let collected_before = self.collected.as_ref().map(Vec::len);

        for name in names {
            // The entry-count limit covers ALL types (files, directories,
            // symlinks) - otherwise a tree of millions of empty directories
            // would bypass the limit.
            self.acc.files += 1;
            if self.acc.files > self.limits.max_files {
                return Err(ContentError::TooLarge(format!(
                    "entry count exceeds {}",
                    self.limits.max_files
                )));
            }
            // The name must be valid UTF-8: to_string_lossy would replace
            // invalid bytes with U+FFFD, so two DIFFERENT names could produce
            // the same digest (a collision). We reject a non-UTF-8 name
            // instead of encoding it ambiguously.
            let name_str = name
                .to_str()
                .ok_or_else(|| ContentError::Unsupported(src_dir.join(&name)))?;
            let rel = if rel_prefix.is_empty() {
                name_str.to_string()
            } else {
                format!("{rel_prefix}/{name_str}")
            };
            let abs = src_dir.join(&name);
            let dst = dst_dir.map(|d| d.join(&name));
            let meta = fs::symlink_metadata(&abs)?;
            let ft = meta.file_type();

            if ft.is_symlink() {
                self.symlink(&abs, src_dir, &rel, dst.as_deref())?;
            } else if ft.is_dir() {
                self.h.update(b"D");
                lp(&mut self.h, rel.as_bytes());
                if let Some(dst) = &dst {
                    fs::create_dir_all(dst)?;
                }
                self.dir(&abs, dst.as_deref(), &rel, depth + 1)?;
            } else if ft.is_file() {
                let mut kept = self.collected.as_ref().map(|_| Vec::new());
                let content_digest = copy_and_hash_file(
                    &abs,
                    dst.as_deref(),
                    kept.as_mut(),
                    self.limits,
                    &mut self.acc,
                )?;
                self.h.update(b"F");
                lp(&mut self.h, rel.as_bytes());
                self.h.update(content_digest);
                if let (Some(out), Some(bytes)) = (self.collected.as_mut(), kept) {
                    out.push(TreeFile {
                        rel,
                        bytes,
                        executable: is_executable(&meta),
                    });
                }
            } else {
                return Err(ContentError::Unsupported(abs.clone()));
            }
        }
        // A file list cannot carry a directory that holds no regular file
        // (it still counts in the digest), so collecting refuses it.
        if depth > 0
            && let (Some(before), Some(out)) = (collected_before, self.collected.as_ref())
            && out.len() == before
        {
            return Err(ContentError::Unsupported(src_dir.to_path_buf()));
        }
        Ok(())
    }

    fn symlink(
        &mut self,
        abs: &Path,
        src_dir: &Path,
        rel: &str,
        dst: Option<&Path>,
    ) -> Result<(), ContentError> {
        // A file list carries regular files only.
        if self.collected.is_some() {
            return Err(ContentError::Unsupported(abs.to_path_buf()));
        }
        let target = fs::read_link(abs)?;
        // Absolute targets are forbidden: copied as-is, such a symlink in
        // the snapshot would keep pointing at the mutable live tree (the
        // snapshot would stop being immutable). Skills use relative
        // links.
        if target.is_absolute() {
            return Err(ContentError::Escape(abs.to_path_buf()));
        }
        // The target must also be UTF-8 - the same anti-collision
        // invariant as for names.
        let target_str = target
            .to_str()
            .ok_or_else(|| ContentError::Escape(abs.to_path_buf()))?;
        let abs_target = abs.parent().unwrap_or(src_dir).join(&target);
        let canon =
            fs::canonicalize(&abs_target).map_err(|_| ContentError::Escape(abs.to_path_buf()))?;
        if !canon.starts_with(self.canonical_root) {
            return Err(ContentError::Escape(abs.to_path_buf()));
        }
        self.h.update(b"L");
        lp(&mut self.h, rel.as_bytes());
        lp(&mut self.h, target_str.as_bytes());
        if let Some(dst) = dst {
            crate::fsutil::symlink(&target, dst).map_err(|e| {
                if e.kind() == std::io::ErrorKind::Unsupported {
                    ContentError::Unsupported(dst.to_path_buf())
                } else {
                    ContentError::Io(e)
                }
            })?;
        }
        Ok(())
    }
}

/// Whether file metadata carries any executable bit (always false off unix).
pub fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Reads the source file once: writes to `dst` (if present), keeps the bytes
/// in `keep` (if present) and computes the sha256 of its content. The
/// per-file and total limits are checked while reading.
fn copy_and_hash_file(
    src: &Path,
    dst: Option<&Path>,
    mut keep: Option<&mut Vec<u8>>,
    limits: &TreeLimits,
    acc: &mut Acc,
) -> Result<[u8; 32], ContentError> {
    let mut f = File::open(src)?;
    let mut out = match dst {
        Some(p) => Some(File::create(p)?),
        None => None,
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut file_bytes: u64 = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file_bytes += n as u64;
        if file_bytes > limits.max_file_bytes {
            return Err(ContentError::TooLarge(format!(
                "file {} exceeds {} bytes",
                src.display(),
                limits.max_file_bytes
            )));
        }
        acc.total += n as u64;
        if acc.total > limits.max_total_bytes {
            return Err(ContentError::TooLarge(format!(
                "total exceeds {} bytes",
                limits.max_total_bytes
            )));
        }
        hasher.update(&buf[..n]);
        if let Some(k) = keep.as_deref_mut() {
            k.extend_from_slice(&buf[..n]);
        }
        if let Some(o) = out.as_mut() {
            o.write_all(&buf[..n])?;
        }
    }
    if let Some(o) = out.as_mut() {
        o.sync_all()?;
    }
    Ok(hasher.finalize().into())
}

/// Profile bundle digest (spec 3.1): sha256 of the domain tag, profile_digest,
/// and sorted pairs of (qualified skill ref, skill_digest). Independent of
/// skill order.
pub fn bundle_digest(profile_digest: &str, skills: &[(String, String)]) -> String {
    let mut items: Vec<&(String, String)> = skills.iter().collect();
    items.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut h = Sha256::new();
    h.update(b"apb-bundle-v1\0");
    lp(&mut h, profile_digest.as_bytes());
    for (r, d) in items {
        lp(&mut h, r.as_bytes());
        lp(&mut h, d.as_bytes());
    }
    format!("sha256:{}", hex_lower(&h.finalize()))
}

/// Single-pass sha256 of arbitrary bytes in `sha256:<hex>` format. For cases
/// where we need to store/compare not the secret itself but its irreversible
/// fingerprint (e.g. a supervisor session token on disk).
pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Single-pass sha256 of arbitrary bytes in `sha256:<hex>` format. For cases
/// where we need to store/compare not the secret itself but its irreversible
/// fingerprint (e.g. a supervisor session token on disk).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{}", hex_lower(&h.finalize()))
}

/// The SHA-256 of `bytes` in standard base64, the form a Content Security
/// Policy hash source (`'sha256-...'`) takes.
pub fn sha256_base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(bytes))
}
