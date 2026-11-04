//! What a newer shipped default changes, for the notice of a copy a person
//! changed ("update available"): the reconcile note, `apb setup plan` and
//! `apb doctor` say what the shipped content changed, not only its version.
//!
//! A playbook is compared from the newest version that still holds apb's
//! own shipped content to the new shipped definition, so the person's own
//! versions and edits are not reported as shipped changes; when no such
//! version is left (every apb-written version was edited in place), the
//! comparison starts at the person's `current`. A skill keeps no copy of
//! the shipped files it was installed from, so its diff is the paths that
//! differ between the installed copy and the new shipped files. Only the
//! first is what the shipped content changed; the other two also hold the
//! person's own edits, reversed, so [`DiffBase`] names the base and its
//! [`DiffBase::label`] words the line differently.
//!
//! Everything here is read-only: nothing is locked, recovered or written.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::{BundledPlaybook, BundledSkill};
use crate::versioning::{VersionDiff, canonical_definition, diff_definitions};

/// What an update diff is counted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffBase {
    /// The newest version that still holds apb's own shipped content: the
    /// diff is what the shipped content changed.
    Shipped,
    /// The person's `current`, when every apb-written version was edited in
    /// place: the diff also holds those edits, reversed.
    Current,
    /// A skill's installed copy: the diff also holds the person's own
    /// edits and files, reversed.
    InstalledCopy,
}

impl DiffBase {
    /// The words before a summary: `shipped changes` only when the diff is
    /// what the shipped content changed.
    pub fn label(self) -> &'static str {
        match self {
            DiffBase::Shipped => "shipped changes",
            DiffBase::Current | DiffBase::InstalledCopy => "differs from the shipped version",
        }
    }

    /// `<label>: <summary>`.
    pub fn line(self, summary: &str) -> String {
        format!("{}: {summary}", self.label())
    }
}

/// The diff of the modified copy in the global store to `shipped`, with
/// what it is counted from, or `None` when the copy is not modified (or
/// nothing reads).
pub fn playbook_update_diff(shipped: &BundledPlaybook) -> Option<(DiffBase, VersionDiff)> {
    playbook_update_diff_in(&super::playbook_store()?, shipped)
}

/// [`playbook_update_diff`] in the store `parent`.
pub(crate) fn playbook_update_diff_in(
    parent: &Path,
    shipped: &BundledPlaybook,
) -> Option<(DiffBase, VersionDiff)> {
    let playbook_dir = parent.join("playbooks").join(&shipped.id);
    let (version, from_shipped) = super::playbooks::update_base(&playbook_dir, shipped)?;
    let from = std::fs::read_to_string(playbook_dir.join(&version).join("playbook.yaml")).ok()?;
    // The shipped text as apb writes it, so a stored version and the shipped
    // one differ only where their content does.
    let to = canonical_definition(&shipped.yaml, &shipped.id, &shipped.version).ok()?;
    let base = if from_shipped {
        DiffBase::Shipped
    } else {
        DiffBase::Current
    };
    Some((base, diff_definitions(&from, &to).ok()?))
}

/// The `diff` a plan warning carries: the definition diff with its `base`.
pub fn playbook_diff_value(base: DiffBase, diff: &VersionDiff) -> serde_json::Value {
    with_base(base, serde_json::to_value(diff))
}

/// The `diff` a plan warning carries for a skill: the paths with their
/// `base`, always the installed copy.
pub fn skill_diff_value(diff: &SkillDiff) -> serde_json::Value {
    with_base(SkillDiff::BASE, serde_json::to_value(diff))
}

fn with_base(
    base: DiffBase,
    value: Result<serde_json::Value, serde_json::Error>,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "base".to_string(),
        serde_json::to_value(base).unwrap_or(serde_json::Value::Null),
    );
    if let Ok(serde_json::Value::Object(fields)) = value {
        out.extend(fields);
    }
    serde_json::Value::Object(out)
}

/// One line for a definition diff: `nodes +1 -0 ~2, edges +1 -0 ~0` (added,
/// removed, changed), with the one-of sources whose edges are checked in
/// another order, `(text only)` when only the text differs, and `no change
/// to the definition` when nothing does.
pub fn diff_summary(d: &VersionDiff) -> String {
    let edges_changed = d.edges.iter().filter(|r| !r.changed.is_empty()).count();
    let mut line = format!(
        "nodes +{} -{} ~{}, edges +{} -{} ~{}",
        d.nodes_added.len(),
        d.nodes_removed.len(),
        d.nodes_changed.len(),
        d.edges_added.len(),
        d.edges_removed.len(),
        edges_changed
    );
    if !d.edge_order_changed.is_empty() {
        line.push_str(&format!(
            ", edge order changed at {}",
            d.edge_order_changed.len()
        ));
    }
    let structural = !(d.nodes_added.is_empty()
        && d.nodes_removed.is_empty()
        && d.nodes_changed.is_empty()
        && d.edges_added.is_empty()
        && d.edges_removed.is_empty()
        && edges_changed == 0
        && d.edge_order_changed.is_empty());
    // The line diff keeps the common lines as context: the definition is
    // the same when no line was added or removed.
    let text_changed = d.yaml_diff.lines().any(|l| !l.starts_with(' '));
    match (structural, text_changed) {
        (true, _) => line,
        (false, true) => format!("{line} (text only)"),
        (false, false) => "no change to the definition".to_string(),
    }
}

/// The paths that differ between an installed skill and the shipped files,
/// relative to the skill folder, each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SkillDiff {
    /// In the shipped files only.
    pub added: Vec<String>,
    /// In the installed copy only.
    pub removed: Vec<String>,
    /// In both, with other content (or not a plain file in the copy).
    pub changed: Vec<String>,
}

impl SkillDiff {
    /// A skill's diff is always counted from its installed copy.
    pub const BASE: DiffBase = DiffBase::InstalledCopy;

    /// One line: `files +1 -0 ~2` (added, removed, changed).
    pub fn summary(&self) -> String {
        format!(
            "files +{} -{} ~{}",
            self.added.len(),
            self.removed.len(),
            self.changed.len()
        )
    }
}

/// The paths that differ between the modified copy of `skill` in the
/// global skills directory and its shipped files, or `None` when the copy
/// is not modified (or cannot be read within the limits).
pub fn skill_update_diff(skill: &BundledSkill) -> Option<SkillDiff> {
    skill_update_diff_in(&super::SkillDirs::global()?.skills, skill)
}

/// [`skill_update_diff`] in the skills directory `skills_dir`.
pub(crate) fn skill_update_diff_in(skills_dir: &Path, skill: &BundledSkill) -> Option<SkillDiff> {
    if !super::skills::is_modified(skills_dir, skill) {
        return None;
    }
    let installed = read_tree(&skills_dir.join(&skill.name))?;
    let shipped: BTreeMap<&str, String> = skill
        .files
        .iter()
        .map(|(rel, bytes)| (rel.as_str(), crate::content::sha256_hex(bytes)))
        .collect();
    let mut diff = SkillDiff::default();
    for (rel, digest) in &shipped {
        match installed.get(*rel) {
            None => diff.added.push((*rel).to_string()),
            Some(Entry::File(d)) if d == digest => {}
            Some(_) => diff.changed.push((*rel).to_string()),
        }
    }
    diff.removed = installed
        .keys()
        .filter(|rel| !shipped.contains_key(rel.as_str()))
        .cloned()
        .collect();
    Some(diff)
}

/// The most entries [`read_tree`] visits, folders included, before it
/// gives up.
const MAX_ENTRIES: usize = 10_000;
/// The deepest folder [`read_tree`] enters.
const MAX_DEPTH: usize = 32;
/// The largest file [`read_tree`] hashes; a larger one counts as changed.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// One entry of an installed copy.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// A plain file and the SHA-256 of its bytes.
    File(String),
    /// Anything else (a link, a special file, a file over the limit, an
    /// entry or a folder that does not read): never the shipped content.
    Other,
}

/// The entries below `root` by relative path (`/` separated), without
/// following a link; `None` when `root` itself does not read or the walk
/// passes the limits. A folder or entry below it that does not read is one
/// [`Entry::Other`], so the rest of the copy is still compared.
fn read_tree(root: &Path) -> Option<BTreeMap<String, Entry>> {
    struct Walk<'a> {
        root: &'a Path,
        visited: usize,
        out: BTreeMap<String, Entry>,
    }
    impl Walk<'_> {
        fn rel(&self, path: &Path) -> Option<String> {
            Some(
                path.strip_prefix(self.root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        }
        fn dir(&mut self, dir: &Path, depth: usize) -> Option<()> {
            if depth > MAX_DEPTH {
                return None;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(_) if depth > 0 => {
                    let rel = self.rel(dir)?;
                    self.out.insert(rel, Entry::Other);
                    return Some(());
                }
                Err(_) => return None,
            };
            for entry in entries {
                self.visited += 1;
                if self.visited > MAX_ENTRIES {
                    return None;
                }
                // An entry the folder listing could not name: nothing to
                // compare it by, so the folder itself differs.
                let Ok(entry) = entry else {
                    if depth > 0 {
                        let rel = self.rel(dir)?;
                        self.out.insert(rel, Entry::Other);
                    }
                    continue;
                };
                let path = entry.path();
                let rel = self.rel(&path)?;
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    self.out.insert(rel, Entry::Other);
                    continue;
                };
                if meta.is_dir() {
                    self.dir(&path, depth + 1)?;
                    continue;
                }
                let entry = if meta.is_file() && meta.len() <= MAX_FILE_BYTES {
                    std::fs::read(&path)
                        .map(|bytes| Entry::File(crate::content::sha256_hex(&bytes)))
                        .unwrap_or(Entry::Other)
                } else {
                    Entry::Other
                };
                self.out.insert(rel, entry);
            }
            Some(())
        }
    }
    let mut walk = Walk {
        root,
        visited: 0,
        out: BTreeMap::new(),
    };
    walk.dir(root, 0)?;
    Some(walk.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_count_toward_the_entry_limit() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..MAX_ENTRIES {
            std::fs::create_dir(root.path().join(format!("d{i}"))).unwrap();
        }
        assert!(read_tree(root.path()).is_some(), "at the limit");
        std::fs::create_dir(root.path().join("one-more")).unwrap();
        assert!(read_tree(root.path()).is_none(), "past the limit");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_does_not_read_is_one_changed_entry() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("SKILL.md"), "x").unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("a.md"), "a").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok();
        let tree = read_tree(root.path());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if readable {
            // Running as root: permissions do not stop the read.
            return;
        }
        let tree = tree.expect("the rest of the copy still reads");
        assert_eq!(tree.get("locked"), Some(&Entry::Other));
        assert!(matches!(tree.get("SKILL.md"), Some(Entry::File(_))));
    }
}
