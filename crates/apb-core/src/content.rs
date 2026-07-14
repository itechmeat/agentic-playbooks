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
        // staging must NOT live inside source: otherwise hash_tree would
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
    match hash_tree(&canonical_root, Some(staging_dst), &canonical_root, limits) {
        Ok(d) => Ok(d),
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
    hash_tree(&canonical_root, None, &canonical_root, limits)
}

fn hash_tree(
    src_root: &Path,
    staging_dst: Option<&Path>,
    canonical_root: &Path,
    limits: &TreeLimits,
) -> Result<String, ContentError> {
    let mut h = Sha256::new();
    h.update(b"apb-tree-v1\0");
    let mut acc = Acc { files: 0, total: 0 };
    walk(
        src_root,
        staging_dst,
        "",
        canonical_root,
        0,
        limits,
        &mut h,
        &mut acc,
    )?;
    Ok(format!("sha256:{:x}", h.finalize()))
}

/// Recursive deterministic walk of directory `src_dir`. `rel_prefix` is the
/// path from the tree root down to `src_dir` (`""` at the root). `dst_dir` is
/// the corresponding directory in staging (Some when copying). Every entry is
/// hashed as a type tag (`D`/`F`/`L`) plus a length-prefixed relative path; a
/// file additionally contributes a fixed-size content sub-digest, and a
/// symlink contributes its target.
#[allow(clippy::too_many_arguments)]
fn walk(
    src_dir: &Path,
    dst_dir: Option<&Path>,
    rel_prefix: &str,
    canonical_root: &Path,
    depth: u32,
    limits: &TreeLimits,
    h: &mut Sha256,
    acc: &mut Acc,
) -> Result<(), ContentError> {
    if depth > limits.max_depth {
        return Err(ContentError::TooLarge(format!(
            "depth exceeds {}",
            limits.max_depth
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

    for name in names {
        // The entry-count limit covers ALL types (files, directories,
        // symlinks) - otherwise a tree of millions of empty directories
        // would bypass the limit.
        acc.files += 1;
        if acc.files > limits.max_files {
            return Err(ContentError::TooLarge(format!(
                "entry count exceeds {}",
                limits.max_files
            )));
        }
        // The name must be valid UTF-8: to_string_lossy would replace invalid
        // bytes with U+FFFD, so two DIFFERENT names could produce the same
        // digest (a collision). We reject a non-UTF-8 name instead of
        // encoding it ambiguously.
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
            let target = fs::read_link(&abs)?;
            // Absolute targets are forbidden: copied as-is, such a symlink in
            // the snapshot would keep pointing at the mutable live tree (the
            // snapshot would stop being immutable). Skills use relative
            // links.
            if target.is_absolute() {
                return Err(ContentError::Escape(abs.clone()));
            }
            // The target must also be UTF-8 - the same anti-collision
            // invariant as for names.
            let target_str = target
                .to_str()
                .ok_or_else(|| ContentError::Escape(abs.clone()))?;
            let abs_target = abs.parent().unwrap_or(src_dir).join(&target);
            let canon =
                fs::canonicalize(&abs_target).map_err(|_| ContentError::Escape(abs.clone()))?;
            if !canon.starts_with(canonical_root) {
                return Err(ContentError::Escape(abs.clone()));
            }
            h.update(b"L");
            lp(h, rel.as_bytes());
            lp(h, target_str.as_bytes());
            if let Some(dst) = &dst {
                make_symlink(&target, dst)?;
            }
        } else if ft.is_dir() {
            h.update(b"D");
            lp(h, rel.as_bytes());
            if let Some(dst) = &dst {
                fs::create_dir_all(dst)?;
            }
            walk(
                &abs,
                dst.as_deref(),
                &rel,
                canonical_root,
                depth + 1,
                limits,
                h,
                acc,
            )?;
        } else if ft.is_file() {
            let content_digest = copy_and_hash_file(&abs, dst.as_deref(), limits, acc)?;
            h.update(b"F");
            lp(h, rel.as_bytes());
            h.update(content_digest);
        } else {
            return Err(ContentError::Unsupported(abs.clone()));
        }
    }
    Ok(())
}

/// Reads the source file once: writes to `dst` (if present) and computes the
/// sha256 of its content. The per-file and total limits are checked while
/// reading.
fn copy_and_hash_file(
    src: &Path,
    dst: Option<&Path>,
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
        if let Some(o) = out.as_mut() {
            o.write_all(&buf[..n])?;
        }
    }
    if let Some(o) = out.as_mut() {
        o.sync_all()?;
    }
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn make_symlink(target: &Path, dst: &Path) -> Result<(), ContentError> {
    std::os::unix::fs::symlink(target, dst)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_symlink(_target: &Path, dst: &Path) -> Result<(), ContentError> {
    Err(ContentError::Unsupported(dst.to_path_buf()))
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
    format!("sha256:{:x}", h.finalize())
}

/// Single-pass sha256 of arbitrary bytes in `sha256:<hex>` format. For cases
/// where we need to store/compare not the secret itself but its irreversible
/// fingerprint (e.g. a supervisor session token on disk).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}
