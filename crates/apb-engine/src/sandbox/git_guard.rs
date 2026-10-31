//! The repository of a sandboxed tree: what the view keeps read-only, and
//! what can redirect git or start one inside the tree, checked after every
//! attempt.
//!
//! Git writes its commits into the git directory itself (`index.lock`
//! renamed over `index`, `COMMIT_EDITMSG`, `HEAD.lock`, `packed-refs.lock`),
//! so the directory stays writable and only what can name a program is
//! bound read-only: each git directory's `config`, `hooks/`, `info/` and
//! `config.worktree`, the common one's, each linked worktree's and each
//! submodule's (under `modules/`, or embedded in the tree), and the `.git`
//! files of the tree and of its submodules.
//!
//! A few things can still redirect git, or make the operator's own git run
//! one inside the tree, without a mount point to protect them: a
//! `commondir` hands git another directory (and its config), a `gitdir`
//! another worktree, a `config.worktree` that did not exist yet a config,
//! an `objects/info/alternates` foreign objects, and a submodule that did
//! not exist (a gitlink in the index with its own git directory) a config
//! git applies whenever it checks whether the submodule is dirty.
//! [`Snapshot`] records all of them before the attempt and
//! [`Snapshot::restore`] puts them back after it: a changed file is written
//! back, a new one removed, a new linked worktree that does not lead back to
//! the repository or carries its own `config.worktree` removed whole, a new
//! submodule git directory under `modules/` removed whole, and the `.git` of
//! a new submodule (or a new `.git` of an existing one) removed. Nothing is
//! written through a directory the attempt replaced by a link; that is
//! reported and fails the node.
//!
//! A repository nested in the tree without a gitlink is the agent's own,
//! like the rest of the tree: git never enters it on the operator's behalf.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use apb_core::git::Repo;

/// The files that redirect git, relative to a git directory.
const WATCHED: [&str; 4] = [
    "commondir",
    "gitdir",
    "config.worktree",
    "objects/info/alternates",
];

/// How deep submodules nest before apb stops looking.
const MODULE_DEPTH: usize = 8;

/// The repository the tree belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// The common directory, canonical.
    pub common: PathBuf,
    /// The tree's own git directory: the common one, or a linked worktree's
    /// `<common>/worktrees/<name>`.
    pub git_dir: PathBuf,
    /// The top of the working tree the tree belongs to.
    pub work_tree: PathBuf,
    /// The tree's `.git` when it is a file (a worktree).
    pub git_file: Option<PathBuf>,
    /// What the view binds read-only inside the git directories and the
    /// submodules' `.git` files.
    pub read_only: Vec<PathBuf>,
    /// The submodules (gitlinks in the index), relative to the work tree.
    pub gitlinks: Vec<String>,
}

/// The repository facts of `tree` through apb's pinned git, `None` outside
/// a repository. A redirected repository refuses the sandbox. The common
/// directory's `hooks/` and `info/` are created when missing, so their
/// read-only mounts always apply.
pub fn facts(tree: &Path) -> Result<Option<GitFacts>, String> {
    let repo = match Repo::locate(tree) {
        Ok(r) => r,
        // A `.git` git cannot read (a `commondir` into nothing, a broken
        // gitfile) would leave its config unprotected in the tree: refused.
        Err(apb_core::git::RepoError::NotARepository)
            if tree
                .ancestors()
                .any(|a| std::fs::symlink_metadata(a.join(".git")).is_ok()) =>
        {
            return Err("the tree has a `.git` that git cannot read".to_string());
        }
        Err(apb_core::git::RepoError::NotARepository) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let common = repo.common_dir.clone();
    for d in ["hooks", "info"] {
        std::fs::create_dir_all(common.join(d)).map_err(|e| format!("the git {d} dir: {e}"))?;
    }
    let gitlinks = gitlinks(&repo).ok_or("the repository's index cannot be read")?;
    let mut read_only: Vec<PathBuf> = Vec::new();
    for dir in git_dirs(&common, &repo.work_tree, &gitlinks) {
        // The common directory and each submodule's carry a config, hooks
        // and info; a linked worktree's only its own `config.worktree`.
        let own = !dir.starts_with(common.join("worktrees"));
        for name in ["config", "hooks", "info"] {
            let p = dir.join(name);
            if own && p.exists() {
                read_only.push(p);
            }
        }
        let wt = dir.join("config.worktree");
        if wt.is_file() {
            read_only.push(wt);
        }
    }
    for link in &gitlinks {
        let file = repo.work_tree.join(link).join(".git");
        if real_below(&repo.work_tree, &file)
            && std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file())
        {
            read_only.push(file);
        }
    }
    Ok(Some(GitFacts {
        git_file: Some(tree.join(".git")).filter(|p| p.is_file()),
        git_dir: repo.git_dir,
        work_tree: repo.work_tree,
        common,
        read_only,
        gitlinks,
    }))
}

/// The gitlinks of the index (mode 160000), `None` when it cannot be read.
fn gitlinks(repo: &Repo) -> Option<Vec<String>> {
    let out = repo.read(&repo.work_tree, &["ls-files", "--stage", "-z"])?;
    Some(
        out.split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                meta.starts_with("160000 ").then(|| path.to_string())
            })
            .collect(),
    )
}

/// Every git directory of the repository: the common one, each linked
/// worktree's, each submodule's under `modules/` (nested ones too) and each
/// submodule's own `.git` directory embedded in the tree.
fn git_dirs(common: &Path, work_tree: &Path, gitlinks: &[String]) -> Vec<PathBuf> {
    let mut out = vec![common.to_path_buf()];
    out.extend(subdirs(&common.join("worktrees")));
    modules(common, 0, &mut out);
    for link in gitlinks {
        let embedded = work_tree.join(link).join(".git");
        if real_below(work_tree, &embedded)
            && std::fs::symlink_metadata(&embedded).is_ok_and(|m| m.is_dir())
        {
            out.push(embedded.clone());
            modules(&embedded, 1, &mut out);
        }
    }
    out
}

/// The submodule git directories under `git_dir/modules`.
fn modules(git_dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for m in subdirs(&git_dir.join("modules")) {
        module(&m, depth, out);
    }
}

/// A directory under `modules/`: a submodule's git directory (it holds a
/// `HEAD`), or one level of a submodule name with a slash in it.
fn module(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= MODULE_DEPTH {
        return;
    }
    if dir.join("HEAD").is_file() {
        out.push(dir.to_path_buf());
        modules(dir, depth + 1, out);
    } else {
        for inner in subdirs(dir) {
            module(&inner, depth + 1, out);
        }
    }
}

/// The real directories in `dir` (a symlink is never followed).
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Whether every directory between `root` (excluded) and `path` (excluded)
/// that exists is a real directory, so acting on `path` cannot leave `root`
/// through a link.
fn real_below(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for c in parts.iter().take(parts.len().saturating_sub(1)) {
        let Component::Normal(name) = c else {
            return false;
        };
        at.push(name);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            // Nothing below a missing directory exists to act on.
            Err(_) => return true,
            Ok(_) => return false,
        }
    }
    true
}

/// What one watched path held: its bytes when a regular file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    Absent,
    File(Vec<u8>),
    /// A directory, a symlink or anything else: never written back, only
    /// removed when it appeared.
    Other,
}

fn held(p: &Path) -> Held {
    match std::fs::symlink_metadata(p) {
        Err(_) => Held::Absent,
        Ok(m) if m.is_file() => std::fs::read(p).map_or(Held::Other, Held::File),
        Ok(_) => Held::Other,
    }
}

/// What [`Snapshot::restore`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// One line per change, saying what was done about it.
    pub changes: Vec<String>,
    /// Something could not be put back: a retry would start from a
    /// repository apb could not restore.
    pub fatal: bool,
}

impl Outcome {
    fn note(&mut self, line: String, restored: bool) {
        self.changes.push(line);
        self.fatal |= !restored;
    }
}

/// What can redirect git in the repository before an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    common: PathBuf,
    work_tree: PathBuf,
    /// Each watched path, the directory it must stay inside, what it held.
    held: BTreeMap<PathBuf, (PathBuf, Held)>,
    worktrees: Vec<PathBuf>,
    modules: Vec<PathBuf>,
    gitlinks: BTreeSet<String>,
}

impl Snapshot {
    pub fn take(git: &GitFacts) -> Snapshot {
        let mut map = BTreeMap::new();
        for dir in git_dirs(&git.common, &git.work_tree, &git.gitlinks) {
            let root = if dir.starts_with(&git.common) {
                &git.common
            } else {
                &git.work_tree
            };
            for name in WATCHED {
                let p = dir.join(name);
                let h = held(&p);
                map.insert(p, (root.clone(), h));
            }
        }
        for link in &git.gitlinks {
            let p = git.work_tree.join(link).join(".git");
            let h = held(&p);
            map.insert(p, (git.work_tree.clone(), h));
        }
        let mut modules = Vec::new();
        self::modules(&git.common, 0, &mut modules);
        Snapshot {
            common: git.common.clone(),
            work_tree: git.work_tree.clone(),
            held: map,
            worktrees: subdirs(&git.common.join("worktrees")),
            modules,
            gitlinks: git.gitlinks.iter().cloned().collect(),
        }
    }

    /// Puts back what changed since [`Snapshot::take`] and removes what
    /// appeared; what it found and did.
    pub fn restore(&self) -> Outcome {
        let mut out = Outcome::default();
        for (p, (root, before)) in &self.held {
            if !real_below(root, p) {
                out.note(
                    format!(
                        "a directory above `{}` was replaced by a link (not restored)",
                        self.shown(p)
                    ),
                    false,
                );
                continue;
            }
            let now = held(p);
            if &now == before {
                continue;
            }
            let restored = match before {
                Held::Absent => remove(p),
                Held::File(bytes) => {
                    let _ = remove(p);
                    apb_core::fsutil::atomic_write(p, bytes).is_ok()
                }
                // A directory or link apb never writes back.
                Held::Other => false,
            };
            out.note(self.describe(p, before, &now, restored), restored);
        }
        for dir in subdirs(&self.common.join("worktrees")) {
            if self.worktrees.contains(&dir) {
                continue;
            }
            let leads = leads_to(&dir, &dir.join("commondir"), &self.common);
            if !leads || held(&dir.join("config.worktree")) != Held::Absent {
                let restored = real_below(&self.common, &dir) && remove(&dir);
                out.note(
                    format!(
                        "`{}` is a new worktree that does not lead back to the repository or carries its own config ({})",
                        self.shown(&dir),
                        outcome(restored)
                    ),
                    restored,
                );
            }
        }
        let mut now_modules = Vec::new();
        modules(&self.common, 0, &mut now_modules);
        for dir in now_modules.iter().filter(|d| !self.modules.contains(d)) {
            let restored = real_below(&self.common, dir) && remove(dir);
            out.note(
                format!(
                    "`{}` is a new submodule git directory ({})",
                    self.shown(dir),
                    outcome(restored)
                ),
                restored,
            );
        }
        self.new_gitlinks(&mut out);
        out
    }

    /// The `.git` of each submodule the attempt added to the index, removed
    /// so git never enters it.
    fn new_gitlinks(&self, out: &mut Outcome) {
        let Some(now) = Repo::locate(&self.work_tree)
            .ok()
            .and_then(|r| gitlinks(&r))
        else {
            out.note(
                "the repository's index cannot be read after the attempt".to_string(),
                false,
            );
            return;
        };
        for link in now.iter().filter(|l| !self.gitlinks.contains(*l)) {
            let dotgit = self.work_tree.join(link).join(".git");
            let restored = real_below(&self.work_tree, &dotgit) && remove(&dotgit);
            out.note(
                format!(
                    "`{link}` is a new submodule: its `.git` ({})",
                    outcome(restored)
                ),
                restored,
            );
        }
    }

    /// `p` as `.git/...` (the common directory's own name), or relative to
    /// the work tree.
    fn shown(&self, p: &Path) -> String {
        let name = self
            .common
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if let Ok(rest) = p.strip_prefix(&self.common) {
            return format!("{name}/{}", rest.display());
        }
        match p.strip_prefix(&self.work_tree) {
            Ok(rest) => rest.display().to_string(),
            Err(_) => p.display().to_string(),
        }
    }

    fn describe(&self, p: &Path, before: &Held, now: &Held, restored: bool) -> String {
        let what = match (before, now) {
            (Held::Absent, _) => "appeared",
            (_, Held::Absent) => "was removed",
            _ => "changed",
        };
        let outcome = match (restored, before) {
            (true, Held::Absent) => "removed",
            (true, _) => "restored",
            (false, _) => "not restored",
        };
        format!("`{}` {what} ({outcome})", self.shown(p))
    }
}

fn outcome(restored: bool) -> &'static str {
    if restored { "removed" } else { "not removed" }
}

/// Whether the `commondir` file of `dir` names `common`.
fn leads_to(dir: &Path, commondir: &Path, common: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(commondir) else {
        return false;
    };
    let target = dir.join(text.trim_end_matches(['\n', '\r']));
    target.canonicalize().is_ok_and(|t| t == common)
}

/// Removes whatever is at `p` (a link itself, never its target).
fn remove(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Err(_) => true,
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).is_ok(),
        Ok(_) => std::fs::remove_file(p).is_ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = apb_core::git::base()
            .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "protocol.file.allow=always",
                "-C",
            ])
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?}");
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let root = root.canonicalize().unwrap();
        (t, root)
    }

    #[test]
    fn a_planted_commondir_and_a_changed_gitdir_are_put_back() {
        let (t, root) = repo();
        let wt = t.path().join("wt");
        git(
            &root,
            &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
        );
        let f = facts(&wt).unwrap().unwrap();
        assert_eq!(f.common, root.join(".git"));
        assert_eq!(f.git_dir, root.join(".git/worktrees/wt"));
        assert!(f.git_file.is_some());
        assert!(f.read_only.contains(&root.join(".git/config")));
        let snap = Snapshot::take(&f);
        assert_eq!(snap.restore(), Outcome::default(), "nothing changed yet");

        std::fs::write(root.join(".git/commondir"), "/evil").unwrap();
        let gitdir = root.join(".git/worktrees/wt/gitdir");
        let original = std::fs::read(&gitdir).unwrap();
        std::fs::write(&gitdir, "/elsewhere/.git\n").unwrap();
        std::fs::write(root.join(".git/config.worktree"), "[core]\n").unwrap();
        let out = snap.restore();
        assert_eq!(out.changes.len(), 3, "{out:?}");
        assert!(!out.fatal);
        assert!(
            out.changes
                .iter()
                .any(|c| c.contains(".git/commondir` appeared (removed)"))
        );
        assert!(!root.join(".git/commondir").exists());
        assert!(!root.join(".git/config.worktree").exists());
        assert_eq!(std::fs::read(&gitdir).unwrap(), original);
        assert_eq!(
            snap.restore(),
            Outcome::default(),
            "restored to the snapshot"
        );
    }

    #[test]
    fn a_new_worktree_must_lead_back_to_the_repository() {
        let (t, root) = repo();
        let f = facts(&root).unwrap().unwrap();
        let snap = Snapshot::take(&f);
        // A worktree git itself makes is accepted.
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                t.path().join("ok").to_str().unwrap(),
            ],
        );
        assert_eq!(snap.restore(), Outcome::default());
        // One whose commondir points elsewhere is removed whole, with the
        // config and hooks planted next to it.
        let bad = root.join(".git/worktrees/bad");
        std::fs::create_dir_all(bad.join("hooks")).unwrap();
        std::fs::write(bad.join("commondir"), t.path().display().to_string()).unwrap();
        std::fs::write(bad.join("config"), "[core]\n\tfsmonitor = /bin/false\n").unwrap();
        let out = snap.restore();
        assert_eq!(out.changes.len(), 1, "{out:?}");
        assert!(!bad.exists());
    }

    #[test]
    fn a_new_submodule_git_directory_is_removed() {
        let (_t, root) = repo();
        let snap = Snapshot::take(&facts(&root).unwrap().unwrap());
        let m = root.join(".git/modules/evil");
        std::fs::create_dir_all(m.join("hooks")).unwrap();
        std::fs::write(m.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(m.join("config"), "[core]\n\tfsmonitor = /bin/false\n").unwrap();
        let out = snap.restore();
        assert_eq!(out.changes.len(), 1, "{out:?}");
        assert!(!m.exists());
    }

    /// A submodule the attempt adds to the index loses its `.git`, so no
    /// git ever enters it; one that existed keeps it.
    #[test]
    fn a_new_submodule_loses_its_git_directory() {
        let (t, root) = repo();
        let up = t.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "-q"]);
        git(&up, &["commit", "-q", "--allow-empty", "-m", "up"]);
        git(
            &root,
            &["submodule", "add", "-q", up.to_str().unwrap(), "old"],
        );
        git(&root, &["commit", "-q", "-m", "old"]);
        let f = facts(&root).unwrap().unwrap();
        assert_eq!(f.gitlinks, ["old"]);
        assert!(
            f.read_only.contains(&root.join("old/.git")),
            "its gitfile is read-only"
        );
        let snap = Snapshot::take(&f);
        let new = root.join("new");
        std::fs::create_dir_all(&new).unwrap();
        git(&new, &["init", "-q"]);
        git(&new, &["commit", "-q", "--allow-empty", "-m", "n"]);
        git(&root, &["add", "new"]);
        let out = snap.restore();
        assert_eq!(out.changes.len(), 1, "{out:?}");
        assert!(out.changes[0].contains("`new` is a new submodule"));
        assert!(!new.join(".git").exists());
        assert!(root.join("old/.git").exists());
    }

    /// Nothing is written through a git directory the attempt replaced by a
    /// link: reported, and the node fails.
    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_a_git_directory_is_never_followed() {
        let (t, root) = repo();
        let wt = t.path().join("wt");
        git(
            &root,
            &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
        );
        let snap = Snapshot::take(&facts(&root).unwrap().unwrap());
        let outside = t.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let admin = root.join(".git/worktrees/wt");
        std::fs::rename(&admin, t.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &admin).unwrap();
        std::fs::create_dir_all(outside.join("objects/info")).unwrap();
        let out = snap.restore();
        assert!(out.fatal, "{out:?}");
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "only what was there"
        );
        assert!(!outside.join("gitdir").exists(), "nothing written outside");
    }

    #[test]
    fn a_redirected_repository_refuses_the_sandbox() {
        let (t, root) = repo();
        // Into another repository's git directory, and into nothing.
        let other = t.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "-q"]);
        std::fs::write(
            root.join(".git/commondir"),
            other.join(".git").to_str().unwrap(),
        )
        .unwrap();
        assert!(facts(&root).is_err());
        std::fs::write(
            root.join(".git/commondir"),
            t.path().join("none").to_str().unwrap(),
        )
        .unwrap();
        assert!(facts(&root).is_err());
        // Twin: a directory outside any repository has no git facts.
        let plain = t.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(facts(&plain).unwrap(), None);
    }
}
