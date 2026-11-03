//! The project's standing playbook instruction: the block `apb init`
//! offers and `apb setup apply --project` writes into `CLAUDE.md` and
//! `AGENTS.md`. One text, one marker and one append rule for both, and a
//! removal that takes out only the exact bytes that were written.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

/// The block's text.
pub const PLAYBOOK_BLOCK: &str = include_str!("../../../apb-cli/assets/playbook-instructions.md");
/// The heading whose presence means the block is there.
pub const PLAYBOOK_MARKER: &str = "## apb playbooks";
/// The instruction files the block goes into.
pub const TARGET_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// Where the block stands in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockState {
    /// In every target file.
    Present,
    /// In some of them.
    Partial,
    Absent,
}

/// Whether `file`'s text holds a block with `marker`.
fn has(dir: &Path, name: &str, marker: &str) -> bool {
    std::fs::read_to_string(dir.join(name)).is_ok_and(|t| t.contains(marker))
}

/// Where the playbook block stands in `dir`.
pub fn block_state(dir: &Path) -> BlockState {
    let n = TARGET_FILES
        .iter()
        .filter(|f| has(dir, f, PLAYBOOK_MARKER))
        .count();
    match n {
        0 => BlockState::Absent,
        n if n == TARGET_FILES.len() => BlockState::Present,
        _ => BlockState::Partial,
    }
}

/// What appending `body` to a file holding `text` inserts.
pub fn separator(text: &str) -> &'static str {
    if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}

/// How one file was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAction {
    Created,
    Appended,
    AlreadyConfigured,
}

/// One target file after [`apply_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The target's name (`CLAUDE.md`, `AGENTS.md`).
    pub name: String,
    /// The file written: the target, or the file inside the project it
    /// links to.
    pub path: PathBuf,
    pub action: BlockAction,
    /// The bytes inserted (empty when already configured).
    pub inserted: String,
}

fn refused(why: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, why)
}

/// The file the block for the target `name` goes into: `dir/name`, or, when
/// that is a symbolic link (`CLAUDE.md -> AGENTS.md`), the file it resolves
/// to, only while that stays inside `dir`. A link out of the project, or
/// one that does not resolve to a file, is refused: nothing outside the
/// project is ever written, and a link is never replaced by a file.
pub fn target(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => {
            let resolved = std::fs::canonicalize(&path).map_err(|e| {
                refused(format!(
                    "{name} is a link that does not resolve ({e}); nothing written"
                ))
            })?;
            let root = std::fs::canonicalize(dir)?;
            if resolved.starts_with(&root) && resolved.is_file() {
                Ok(resolved)
            } else {
                Err(refused(format!(
                    "{name} links to {}, outside the project; nothing written",
                    resolved.display()
                )))
            }
        }
        _ => Ok(path),
    }
}

/// Why the block cannot be written into `dir` at all: a target links out of
/// the project. `None` when every target is writable.
pub fn link_problem(dir: &Path) -> Option<String> {
    TARGET_FILES
        .iter()
        .find_map(|name| target(dir, name).err().map(|e| e.to_string()))
}

/// Appends the block with `marker` and `body` to every target file in
/// `dir`, creating a missing file; a file that has it already is left
/// alone. A target that links to another file inside the project is
/// written there (so two names linked to one file get one block); a link
/// out of the project refuses the whole write before anything changes.
/// Every target is read before the first write, so a file that does not
/// read changes nothing; a write that fails puts back the files written
/// before it, so a failed call leaves no block behind that nothing records.
pub fn apply_block(dir: &Path, marker: &str, body: &str) -> std::io::Result<Vec<Written>> {
    let targets = TARGET_FILES
        .iter()
        .map(|name| target(dir, name).map(|path| (name.to_string(), path)))
        .collect::<std::io::Result<Vec<_>>>()?;
    // Read everything first: (written, the text before, the text after).
    let mut planned: Vec<(Written, Option<String>, String)> = Vec::new();
    for (name, path) in targets {
        let seen = planned.iter().any(|(w, _, _)| same_file(&w.path, &path));
        let before = if path.exists() {
            Some(std::fs::read_to_string(&path)?)
        } else {
            None
        };
        let (action, inserted, after) = match &before {
            _ if seen => (BlockAction::AlreadyConfigured, String::new(), String::new()),
            Some(text) if text.contains(marker) => {
                (BlockAction::AlreadyConfigured, String::new(), String::new())
            }
            Some(text) => {
                let inserted = format!("{}{body}", separator(text));
                let after = format!("{text}{inserted}");
                (BlockAction::Appended, inserted, after)
            }
            None => (BlockAction::Created, body.to_string(), body.to_string()),
        };
        let written = Written {
            name,
            path,
            action,
            inserted,
        };
        planned.push((written, before, after));
    }
    for i in 0..planned.len() {
        let (w, _, after) = &planned[i];
        if w.action == BlockAction::AlreadyConfigured {
            continue;
        }
        if let Err(e) = apb_core::fsutil::atomic_rewrite_in_place(&w.path, after.as_bytes()) {
            for (done, before, _) in &planned[..i] {
                if done.action == BlockAction::AlreadyConfigured {
                    continue;
                }
                let _ = match before {
                    Some(text) => {
                        apb_core::fsutil::atomic_rewrite_in_place(&done.path, text.as_bytes())
                            .map(|_| ())
                    }
                    None => std::fs::remove_file(&done.path),
                };
            }
            return Err(e);
        }
    }
    Ok(planned.into_iter().map(|(w, _, _)| w).collect())
}

/// Whether `a` and `b` name one file (two names linked to it).
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// Writes the playbook block into `dir`; what the setup record keeps.
pub fn write(dir: &Path) -> std::io::Result<Value> {
    let files: Vec<Value> = apply_block(dir, PLAYBOOK_MARKER, PLAYBOOK_BLOCK)?
        .into_iter()
        .filter(|w| w.action != BlockAction::AlreadyConfigured)
        .map(|w| {
            json!({
                "path": w.path,
                "created_file": w.action == BlockAction::Created,
                "inserted": w.inserted,
            })
        })
        .collect();
    Ok(json!({ "dir": dir, "files": files }))
}

/// Takes the block back out of every file [`write`] changed, only where its
/// bytes are still exactly as written. The files left as they are, with
/// the reason.
pub fn remove(written: &Value) -> Vec<(String, String)> {
    let mut kept = Vec::new();
    let dir = written.get("dir").and_then(Value::as_str).map(Path::new);
    for f in written
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(path), Some(inserted)) = (
            f.get("path").and_then(Value::as_str),
            f.get("inserted").and_then(Value::as_str),
        ) else {
            continue;
        };
        let created = f.get("created_file").and_then(Value::as_bool) == Some(true);
        if let Err(why) = remove_one(dir, Path::new(path), inserted, created) {
            kept.push((path.to_string(), why));
        }
    }
    kept
}

/// The file to take the block out of: `path`, or the file inside the
/// project `dir` it links to now. The link itself is never replaced.
fn removal_target(dir: Option<&Path>, path: &Path) -> Result<PathBuf, String> {
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        return Ok(path.to_path_buf());
    }
    let (Some(dir), Some(name)) = (dir, path.file_name()) else {
        return Err("a link with no project recorded; left as is".to_string());
    };
    if path.parent() != Some(dir) {
        return Err("a link outside the recorded project; left as is".to_string());
    }
    target(dir, &name.to_string_lossy()).map_err(|e| format!("left as is: {e}"))
}

fn remove_one(
    dir: Option<&Path>,
    path: &Path,
    inserted: &str,
    created: bool,
) -> Result<(), String> {
    let path = &removal_target(dir, path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    if created && text == inserted {
        return std::fs::remove_file(path).map_err(|e| e.to_string());
    }
    let needle = if text.contains(inserted) {
        inserted
    } else if text.contains(PLAYBOOK_BLOCK) {
        PLAYBOOK_BLOCK
    } else if !text.contains(PLAYBOOK_MARKER) {
        return Ok(());
    } else {
        return Err("the block was edited after setup wrote it".to_string());
    };
    let out = text.replacen(needle, "", 1);
    apb_core::fsutil::atomic_rewrite_in_place(path, out.as_bytes())
        .map(|_| ())
        .map_err(|e| e.to_string())
}
