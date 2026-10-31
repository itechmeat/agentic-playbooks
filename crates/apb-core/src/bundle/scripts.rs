//! The `scripts` part of a playbook bundle (format 2): the regular files
//! under a version's `scripts/` directory, carried as base64 with their
//! executable bit.
//!
//! Export reads the tree through [`crate::content::read_tree_files`], the
//! same walk and the same [`TreeLimits::default()`] the definition digest
//! uses, so an over-limit tree fails instead of producing a partial bundle,
//! and an imported version digests exactly like its source. Import treats the
//! list as untrusted input and checks it fully before anything is written.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::content::{ContentError, TreeFile, TreeLimits, read_tree_files};

/// Why the scripts of a bundle could not be exported or imported.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScriptsError(pub String);

impl From<io::Error> for ScriptsError {
    fn from(e: io::Error) -> Self {
        ScriptsError(format!("io: {e}"))
    }
}

/// One regular file under a version's `scripts/` directory, as a format-2
/// bundle carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleScript {
    /// Path relative to `scripts/`, `/`-separated, under the bundle's one
    /// script path rule.
    pub path: String,
    /// Raw file bytes, base64 encoded.
    pub content_base64: String,
    /// Whether the file carried an executable bit on export; restored on
    /// import (unix only).
    #[serde(default)]
    pub executable: bool,
}

/// The scripts of the version directory `version_dir`, for export. A
/// version without scripts (no `scripts/` directory, or an empty one)
/// exports an empty list.
pub(super) fn export_scripts(version_dir: &Path) -> Result<Vec<BundleScript>, ScriptsError> {
    let scripts_dir = version_dir.join("scripts");
    match fs::symlink_metadata(&scripts_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(ScriptsError(
                "`scripts` of the version is not a directory".into(),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    }
    let (files, _digest) =
        read_tree_files(&scripts_dir, &TreeLimits::default()).map_err(|e| export_error(&e))?;
    check_script_paths(files.iter().map(|f| f.rel.as_str()))?;
    Ok(files
        .into_iter()
        .map(|f| BundleScript {
            path: f.rel,
            content_base64: STANDARD.encode(&f.bytes),
            executable: f.executable,
        })
        .collect())
}

/// Explains a tree walk failure in bundle terms.
fn export_error(err: &ContentError) -> ScriptsError {
    let detail = match err {
        ContentError::Unsupported(p) | ContentError::Escape(p) => format!(
            "a bundle carries regular files only (no symlinks, special files or directories without files): {}",
            p.display()
        ),
        ContentError::TooLarge(what) => format!("over the scripts limit: {what}"),
        ContentError::Io(e) => format!("read failed: {e}"),
    };
    ScriptsError(format!("cannot export the version's scripts: {detail}"))
}

/// The ONE path-safety rule for bundle scripts, applied by export (over the
/// walked tree) and by import (over untrusted bundle data): a path is
/// relative, `/`-separated, built only of [`is_safe_script_segment`]
/// segments, no deeper than the tree depth limit, and unique.
fn check_script_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), ScriptsError> {
    let max_depth = TreeLimits::default().max_depth;
    let mut seen = HashSet::new();
    for path in paths {
        let segments: Vec<&str> = path.split('/').collect();
        let reason = if path.is_empty() {
            Some("the path is empty".to_string())
        } else if segments.iter().any(|s| !is_safe_script_segment(s)) {
            Some(
                "absolute, or a segment is empty, `.`, `..`, over 255 bytes, or carries a \
                 backslash or a control character"
                    .to_string(),
            )
        } else if segments.len() as u32 > max_depth + 1 {
            Some(format!("deeper than the limit of {max_depth}"))
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(ScriptsError(format!(
                "unsafe script path `{}`: {reason}",
                path.escape_debug()
            )));
        }
        if !seen.insert(path) {
            return Err(ScriptsError(format!("duplicate script path `{path}`")));
        }
    }
    Ok(())
}

/// One segment of a script path: a file or directory name any supported
/// filesystem can hold. Unlike an id segment, a name such as `v1..2.txt` is
/// fine; only a segment that IS `.` or `..` would leave its directory.
fn is_safe_script_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= 255
        && !s.contains('\\')
        && !s.chars().any(char::is_control)
}

/// Checks the scripts of an incoming bundle and decodes them, BEFORE
/// anything is written: the shared path rule, a file that is also used as a
/// directory, the base64 payload, and the same limits export enforces (the
/// entry count covers the implied directories, as the tree walk counts them).
pub(super) fn import_scripts(list: &[BundleScript]) -> Result<Vec<TreeFile>, ScriptsError> {
    check_script_paths(list.iter().map(|s| s.path.as_str()))?;
    let limits = TreeLimits::default();
    // Paths are compared case-folded too: on a case-insensitive filesystem
    // (macOS, Windows) `run.sh` and `RUN.sh` are one file, and the import
    // would silently keep only one of them.
    let mut files: HashSet<String> = HashSet::new();
    for script in list {
        if !files.insert(script.path.to_lowercase()) {
            return Err(ScriptsError(format!(
                "script path `{}` collides with another one on a case-insensitive filesystem",
                script.path
            )));
        }
    }
    let mut dirs: HashSet<String> = HashSet::new();
    for script in list {
        let mut end = 0;
        for segment in script.path.split('/') {
            if end > 0 {
                let dir = &script.path[..end - 1];
                let folded = dir.to_lowercase();
                if files.contains(&folded) {
                    return Err(ScriptsError(format!(
                        "script path `{dir}` is both a file and a directory"
                    )));
                }
                dirs.insert(folded);
            }
            end += segment.len() + 1;
        }
    }
    let entries = list.len() + dirs.len();
    if entries as u64 > u64::from(limits.max_files) {
        return Err(ScriptsError(format!(
            "the bundle scripts hold {entries} entries; the limit is {}",
            limits.max_files
        )));
    }
    // The sizes the base64 payloads declare are checked for every file and
    // in total BEFORE anything is decoded, so an over-limit bundle costs no
    // decoding memory at all.
    let mut declared_total = 0u64;
    for script in list {
        let declared = declared_len(&script.content_base64);
        if declared > limits.max_file_bytes {
            return Err(over_file_limit(&script.path, limits.max_file_bytes));
        }
        declared_total += declared;
    }
    if declared_total > limits.max_total_bytes {
        return Err(over_total_limit(limits.max_total_bytes));
    }
    let mut out = Vec::with_capacity(list.len());
    for script in list {
        let bytes = STANDARD
            .decode(script.content_base64.as_bytes())
            .map_err(|e| {
                ScriptsError(format!("script `{}` is not valid base64: {e}", script.path))
            })?;
        out.push(TreeFile {
            rel: script.path.clone(),
            bytes,
            executable: script.executable,
        });
    }
    Ok(out)
}

/// The decoded size a padded base64 payload declares: three bytes per four
/// characters, minus the padding. For the canonical padded encoding the
/// decoder accepts this is exactly the decoded size; a malformed payload
/// (its length not a multiple of four) is rounded up, and decoding rejects
/// it anyway.
fn declared_len(base64: &str) -> u64 {
    let len = base64.len() as u64;
    let padding = base64.bytes().rev().take(2).filter(|b| *b == b'=').count() as u64;
    (len.div_ceil(4) * 3).saturating_sub(padding)
}

fn over_total_limit(cap: u64) -> ScriptsError {
    ScriptsError(format!(
        "the bundle scripts exceed the total limit of {cap} bytes"
    ))
}

fn over_file_limit(path: &str, cap: u64) -> ScriptsError {
    ScriptsError(format!(
        "script `{path}` exceeds the per-file limit of {cap} bytes"
    ))
}
