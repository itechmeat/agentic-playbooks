//! The state of a tree a `read_only` node must leave as it found it: what
//! the detected tier compares before and after an attempt.
//!
//! A fingerprint, never a copy: every listed entry is held as its content
//! digest and executable bit (a file) or its target string (a symlink,
//! never followed), plus, for each git repository of the tree, `HEAD`, a
//! digest of every ref, a digest of the staged index and a digest of each
//! file that changes what git does there: `config`, `config.worktree`,
//! `commondir`, `shallow`, the alternates, everything under `info/`
//! (`exclude`, `attributes`, `sparse-checkout`) and under `hooks/`, in the
//! git directory and the common one. Nothing of a file's content is kept,
//! so the state can be compared and reported by name only.
//!
//! The listing is the one protected paths use ([`super::matching_files`]):
//! on a git work tree the files git tracks or would track, so ignored
//! paths are never listed; elsewhere every file. Every `.gitignore` is
//! listed whether or not git ignores it, unless a rule from outside its
//! directory ignores that directory, so a new `.gitignore` that hides
//! itself (and a file next to it) shows as added. An initialized submodule
//! (or a repository nested in the tree) is listed the same way inside, its
//! repository state included, so a change or a commit there is seen too,
//! [`MAX_NESTED`] repositories deep. `.git` is left out of the listing (its
//! state is the repository's), and so is `.apb`, except at the tree's root
//! the definitions the run itself never writes during an attempt
//! ([`APB_DEFINITIONS`]), listed whether or not git ignores them; the run's
//! own files under `.apb` change while the node runs, and an `.apb` below
//! the root is not listed. Anything below a symlinked directory is left out
//! (the link itself is an entry). Limits bound the work: past
//! [`TreeLimits::entries`] entries the rest is not listed, past
//! [`TreeLimits::bytes`] hashed bytes (the git control files count too) a
//! file is held by its size and modification time only, and a repository
//! nested deeper than [`MAX_NESTED`] or a git directory with more than
//! [`MAX_CONTROL_FILES`] control entries is not looked into. A state that
//! hit any of them is `partial` and says which (`entries` for what was not
//! listed, `bytes` for what was not hashed); a limit never turns into "no
//! change".

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{has_symlinked_parent, ignored_paths};

/// The default bounds of one before-state.
pub const DEFAULT_MAX_ENTRIES: u64 = 20_000;
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// How much one state may list and hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLimits {
    pub entries: u64,
    pub bytes: u64,
}

impl Default for TreeLimits {
    fn default() -> Self {
        TreeLimits {
            entries: DEFAULT_MAX_ENTRIES,
            bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// The limit a state hit first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeLimit {
    Entries,
    Bytes,
}

/// One listed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File {
        /// `None` once the byte limit was reached: then `size` and
        /// `mtime_ns` stand in for the content.
        digest: Option<String>,
        size: u64,
        mtime_ns: i128,
        exec: bool,
    },
    Link {
        target: String,
    },
}

/// The `.apb` entries at the tree's root that are listed: the definitions
/// and settings apb reads and never writes while a node runs.
pub const APB_DEFINITIONS: [&str; 9] = [
    "playbooks",
    "profiles",
    "connector-config",
    "config.yaml",
    "policy.yaml",
    "decisions.yaml",
    "secrets.env",
    "workspace.local",
    ".gitignore",
];

/// How deep repositories nested in the tree (submodules) are followed; a
/// deeper one makes the state partial.
pub const MAX_NESTED: usize = 8;

/// At most this many entries (files and directories) are visited under one
/// git directory's `info/` and `hooks/`. Past it none of that directory's
/// control files is held: one marker stands for all of them, the state is
/// partial, and an after-state that crosses the bound either way differs.
pub const MAX_CONTROL_FILES: usize = 2_000;

/// The repository state of a git work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitState {
    /// `rev-parse HEAD`, `None` without a commit.
    head: Option<String>,
    /// The branch `HEAD` names, `None` when detached.
    branch: Option<String>,
    refs: String,
    index: String,
    /// The files that change what git does, by label (tree-relative when
    /// under the tree, else `$GIT_DIR/...` or `$GIT_COMMON_DIR/...`): their
    /// content digest and executable bit, or a symlink's target.
    control: BTreeMap<String, String>,
}

/// One tree's state at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeState {
    entries: BTreeMap<String, Entry>,
    /// The repository state of every git work tree in the tree, by the
    /// tree-relative prefix of its root (`""` for the tree's own, `sub/`
    /// for a submodule).
    repos: BTreeMap<String, GitState>,
    /// What git ignored when the state was taken, so a later listing that
    /// shows such a path (the attempt edited `.gitignore`) never reads it
    /// as added.
    ignored: Vec<String>,
    bytes: u64,
    partial: Option<TreeLimit>,
    /// Paths left out of the listing by the entry limit: `None` unless the
    /// limit was hit; then the first path not listed, so the after-state
    /// compares the same range.
    cut: Option<String>,
}

/// The kind of one change, as the journal names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Mode,
    Link,
    GitHead,
    GitRefs,
    GitIndex,
    GitControl,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Modified => "modified",
            ChangeKind::Added => "added",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Mode => "mode",
            ChangeKind::Link => "link",
            ChangeKind::GitHead => "git_head",
            ChangeKind::GitRefs => "git_refs",
            ChangeKind::GitIndex => "git_index",
            ChangeKind::GitControl => "git_control",
        }
    }
}

/// One change between two states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeChange {
    /// The tree-relative path; `HEAD`, `refs` or `index` for the
    /// repository state (after a submodule's prefix for its own), and a
    /// control file's label for a `git_control` change.
    pub path: String,
    pub kind: ChangeKind,
}

impl TreeState {
    /// The state of the tree at `root` now, within `limits`.
    pub fn take(root: &Path, limits: TreeLimits) -> TreeState {
        let mut state = TreeState {
            entries: BTreeMap::new(),
            repos: BTreeMap::new(),
            ignored: Vec::new(),
            bytes: 0,
            partial: None,
            cut: None,
        };
        state.take_tree(root, root, "", limits, 0);
        state
    }

    /// Lists the tree at `dir`, `prefix` its path from `top` (empty, or
    /// ending in `/`), `depth` repositories deep.
    fn take_tree(
        &mut self,
        top: &Path,
        dir: &Path,
        prefix: &str,
        limits: TreeLimits,
        depth: usize,
    ) {
        // A tree its enclosing repository ignores as a whole (a project
        // under another repository's ignored build directory) is not that
        // repository's: git would call every file in it ignored. It is
        // listed like a tree outside git. A nested directory is a
        // repository of its own only when it is that repository's root
        // (an initialized submodule); else it belongs to the tree around.
        let repo = crate::git::Repo::locate(dir).ok().filter(|r| match depth {
            0 => r.read(dir, &["check-ignore", "-q", "."]).is_none(),
            _ => dir.canonicalize().is_ok_and(|d| d == r.work_tree),
        });
        let listed = match &repo {
            Some(r) => r
                .read(
                    dir,
                    &[
                        "ls-files",
                        "-z",
                        "--cached",
                        "--others",
                        "--exclude-standard",
                    ],
                )
                .map(|s| split_z(&s)),
            None => None,
        };
        let ignored = match &repo {
            Some(_) => ignored_paths(dir),
            None => Vec::new(),
        };
        let mut candidates = match listed {
            Some(paths) => paths,
            None => {
                let mut paths = Vec::new();
                walk_entries(dir, dir, &mut paths);
                paths
            }
        };
        if let Some(r) = &repo {
            let (found, complete) = ignored_gitignores(r, dir, &ignored, limits.entries);
            candidates.extend(found);
            if !complete {
                self.partial.get_or_insert(TreeLimit::Entries);
            }
        }
        candidates.retain(|p| !p.split('/').any(|seg| seg == ".git" || seg == ".apb"));
        if depth == 0 {
            candidates.extend(apb_definitions(dir));
        }
        for c in &mut candidates {
            // An untracked nested repository is listed as `dir/`.
            if c.ends_with('/') {
                c.pop();
            }
        }
        // Path order by segment, the order the recursion below lists in
        // (`sub/x` before `sub-a.txt`), so the entry limit's cut splits the
        // listing where `covers` says.
        candidates.sort_unstable_by(|a, b| segment_cmp(a, b));
        candidates.dedup();
        if let Some(r) = &repo {
            let git = self.git_state(r, dir, top, limits);
            self.repos.insert(prefix.to_string(), git);
            self.ignored
                .extend(ignored.into_iter().map(|p| format!("{prefix}{p}")));
        }
        for rel in candidates {
            let full = format!("{prefix}{rel}");
            if self.entries.len() as u64 >= limits.entries {
                self.partial = Some(TreeLimit::Entries);
                self.cut = Some(full);
                break;
            }
            if has_symlinked_parent(dir, &rel) {
                continue;
            }
            let path = dir.join(&rel);
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let entry = if meta.file_type().is_symlink() {
                let Ok(target) = std::fs::read_link(&path) else {
                    continue;
                };
                Entry::Link {
                    target: target.to_string_lossy().into_owned(),
                }
            } else if meta.is_file() {
                let digest = match self.hash(&path, meta.len(), limits) {
                    Hashed::Done(d, _) => Some(d),
                    Hashed::Over | Hashed::Unreadable => None,
                };
                Entry::File {
                    digest,
                    size: meta.len(),
                    mtime_ns: mtime_ns(&meta),
                    exec: is_exec(&meta),
                }
            } else {
                // A directory git lists is a submodule's work tree (or a
                // repository nested in the tree): listed inside, with its
                // own repository state.
                if meta.is_dir() {
                    if depth >= MAX_NESTED {
                        // Not looked into: what is inside is not covered.
                        self.partial.get_or_insert(TreeLimit::Entries);
                        continue;
                    }
                    self.take_tree(top, &path, &format!("{full}/"), limits, depth + 1);
                    if self.cut.is_some() {
                        break;
                    }
                }
                continue;
            };
            self.entries.insert(full, entry);
        }
    }

    /// The digest of the file at `path` (`len` bytes when stat'ed) within
    /// the byte limit, charged to it. Past the limit the state turns
    /// partial and the file is not hashed (`Hashed::Over`).
    fn hash(&mut self, path: &Path, len: u64, limits: TreeLimits) -> Hashed {
        let left = limits.bytes.saturating_sub(self.bytes);
        let hashed = match len > left {
            true => Hashed::Over,
            false => file_digest(path, left),
        };
        match &hashed {
            Hashed::Done(_, n) => self.bytes += n,
            Hashed::Over => {
                self.partial.get_or_insert(TreeLimit::Bytes);
            }
            Hashed::Unreadable => {}
        }
        hashed
    }

    fn git_state(
        &mut self,
        repo: &crate::git::Repo,
        root: &Path,
        top: &Path,
        limits: TreeLimits,
    ) -> GitState {
        let read = |args: &[&str]| {
            repo.read(root, args)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let head = read(&["rev-parse", "--verify", "-q", "HEAD"]);
        let branch = read(&["symbolic-ref", "-q", "HEAD"]);
        let digest = |args: &[&str]| {
            let text = repo.read(root, args).unwrap_or_default();
            crate::content::sha256_hex(text.as_bytes())
        };
        let mut control = BTreeMap::new();
        self.control_files(&repo.git_dir, "$GIT_DIR", top, limits, &mut control);
        if repo.common_dir != repo.git_dir {
            self.control_files(
                &repo.common_dir,
                "$GIT_COMMON_DIR",
                top,
                limits,
                &mut control,
            );
        }
        GitState {
            head,
            branch,
            refs: digest(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            index: digest(&["ls-files", "--stage", "-z", "--full-name"]),
            control,
        }
    }

    /// The files of git directory `dir` that change what git does, into
    /// `out` by label: the path from `top` when `dir` lies under it, else
    /// from `name` (`$GIT_DIR` or `$GIT_COMMON_DIR`). Each is held by a
    /// digest of its content and its executable bit (by its size and
    /// modification time past the byte limit, which it is charged to), a
    /// symlink by its target, never followed. A missing file is simply
    /// absent, so creating one shows. Past [`MAX_CONTROL_FILES`] visited
    /// entries under `info/` and `hooks/` one marker stands for all of
    /// them and the state is partial.
    fn control_files(
        &mut self,
        dir: &Path,
        name: &str,
        top: &Path,
        limits: TreeLimits,
        out: &mut BTreeMap<String, String>,
    ) {
        let top = top.canonicalize().ok();
        let label = |path: &Path| match top.as_deref().and_then(|t| path.strip_prefix(t).ok()) {
            Some(rel) => rel.to_string_lossy().replace('\\', "/"),
            None => format!(
                "{name}/{}",
                path.strip_prefix(dir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/")
            ),
        };
        let mut paths: Vec<std::path::PathBuf> = [
            "config",
            "config.worktree",
            "commondir",
            "shallow",
            "objects/info/alternates",
            "objects/info/http-alternates",
        ]
        .iter()
        .map(|f| dir.join(f))
        .collect();
        let fixed = paths.len();
        let mut visits = MAX_CONTROL_FILES;
        for sub in ["info", "hooks"] {
            if !walk_control(&dir.join(sub), &mut paths, &mut visits) {
                // Which files the walk reached is not stable: only the
                // fixed files are held, the marker stands for the rest.
                paths.truncate(fixed);
                self.partial.get_or_insert(TreeLimit::Entries);
                out.insert(format!("{name}/..."), "over the bound".into());
                break;
            }
        }
        paths.sort();
        for path in paths {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let value = if meta.file_type().is_symlink() {
                match std::fs::read_link(&path) {
                    Ok(t) => format!("link {}", t.to_string_lossy()),
                    Err(_) => continue,
                }
            } else if meta.is_file() {
                let held = match self.hash(&path, meta.len(), limits) {
                    Hashed::Done(d, _) => d,
                    Hashed::Over => format!("size {} mtime {}", meta.len(), mtime_ns(&meta)),
                    Hashed::Unreadable => "unreadable".to_string(),
                };
                format!("{held} {}", is_exec(&meta))
            } else {
                continue;
            };
            out.insert(label(&path), value);
        }
    }

    /// How many entries the state lists.
    pub fn entries(&self) -> u64 {
        self.entries.len() as u64
    }

    /// How many bytes it hashed.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The commit `HEAD` named, on a git work tree with a commit.
    pub fn head_commit(&self) -> Option<&str> {
        self.repos.get("")?.head.as_deref()
    }

    /// The limit it hit, if any.
    pub fn partial(&self) -> Option<TreeLimit> {
        self.partial
    }

    fn was_ignored(&self, path: &str) -> bool {
        // The `.apb` definitions and every `.gitignore` are listed whether
        // git ignores them or not: both states list them by the same rule.
        !path.starts_with(".apb/")
            && !is_gitignore(path)
            && self
                .ignored
                .iter()
                .any(|i| i == path || (i.ends_with('/') && path.starts_with(i.as_str())))
    }

    /// Whether `path` lies in the range this (before-)state covers.
    fn covers(&self, path: &str) -> bool {
        self.cut
            .as_deref()
            .is_none_or(|cut| segment_cmp(path, cut).is_lt())
    }

    /// What changed from `self` (before) to `after`, in path order, the
    /// repository changes last. Only the range the before-state listed is
    /// compared.
    pub fn changes(&self, after: &TreeState) -> Vec<TreeChange> {
        let mut out = Vec::new();
        let push = |out: &mut Vec<TreeChange>, path: &str, kind| {
            out.push(TreeChange {
                path: path.to_string(),
                kind,
            });
        };
        for (path, before) in &self.entries {
            match after.entries.get(path) {
                None if after.covers(path) => push(&mut out, path, ChangeKind::Deleted),
                None => {}
                Some(now) => {
                    if let Some(kind) = entry_change(before, now) {
                        push(&mut out, path, kind);
                    }
                }
            }
        }
        for path in after.entries.keys() {
            if !self.entries.contains_key(path) && self.covers(path) && !self.was_ignored(path) {
                push(&mut out, path, ChangeKind::Added);
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        let prefixes: std::collections::BTreeSet<&String> =
            self.repos.keys().chain(after.repos.keys()).collect();
        for prefix in prefixes {
            let at = |what: &str| format!("{prefix}{what}");
            match (self.repos.get(prefix), after.repos.get(prefix)) {
                (Some(b), Some(a)) => {
                    if b.head != a.head || b.branch != a.branch {
                        push(&mut out, &at("HEAD"), ChangeKind::GitHead);
                    }
                    if b.refs != a.refs {
                        push(&mut out, &at("refs"), ChangeKind::GitRefs);
                    }
                    if b.index != a.index {
                        push(&mut out, &at("index"), ChangeKind::GitIndex);
                    }
                    let labels: std::collections::BTreeSet<&String> =
                        b.control.keys().chain(a.control.keys()).collect();
                    for label in labels {
                        if b.control.get(label) != a.control.get(label) {
                            push(&mut out, label, ChangeKind::GitControl);
                        }
                    }
                }
                // The repository appeared or went away.
                _ => push(&mut out, &at("HEAD"), ChangeKind::GitHead),
            }
        }
        out
    }
}

fn entry_change(before: &Entry, now: &Entry) -> Option<ChangeKind> {
    match (before, now) {
        (Entry::Link { target: a }, Entry::Link { target: b }) => {
            (a != b).then_some(ChangeKind::Link)
        }
        (Entry::Link { .. }, _) | (_, Entry::Link { .. }) => Some(ChangeKind::Link),
        (
            Entry::File {
                digest: d1,
                size: s1,
                mtime_ns: m1,
                exec: x1,
            },
            Entry::File {
                digest: d2,
                size: s2,
                mtime_ns: m2,
                exec: x2,
            },
        ) => {
            let content_changed = match (d1, d2) {
                (Some(a), Some(b)) => a != b,
                // Unhashed on either side: size and modification time.
                _ => s1 != s2 || m1 != m2,
            };
            if content_changed {
                Some(ChangeKind::Modified)
            } else if x1 != x2 {
                Some(ChangeKind::Mode)
            } else {
                None
            }
        }
    }
}

fn split_z(s: &str) -> Vec<String> {
    s.split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every file and symlink under `dir`, recursively, never following a
/// link, visiting at most `visits` more entries; `false` once that budget
/// ran out (the walk stops there).
fn walk_control(dir: &Path, out: &mut Vec<std::path::PathBuf>, visits: &mut usize) -> bool {
    let Ok(read) = std::fs::read_dir(dir) else {
        return true;
    };
    for entry in read.flatten() {
        if *visits == 0 {
            return false;
        }
        *visits -= 1;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !walk_control(&entry.path(), out, visits) {
                return false;
            }
        } else {
            out.push(entry.path());
        }
    }
    true
}

/// Whether `path` names a `.gitignore` file.
fn is_gitignore(path: &str) -> bool {
    path.rsplit('/').next() == Some(".gitignore")
}

/// Orders two tree-relative paths segment by segment, so a directory's
/// entries (`sub/x`) come right after it and before a sibling that only
/// shares its name as a prefix (`sub-a.txt`).
fn segment_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.split('/').cmp(b.split('/'))
}

/// The `.gitignore` files git ignores in the work tree at `dir`, relative
/// to it, from `ignored` (its ignored paths, a directory with a trailing
/// `/`): an ignored `.gitignore` itself, and every one inside an ignored
/// directory that no rule from outside that directory ignores (git ignores
/// it only for what its own files say, the shape a self-hiding `.gitignore`
/// leaves). A directory a rule outside it ignores is not looked into, at
/// every level of the walk: what is under it was hidden before the attempt
/// too. At most `budget` entries are visited inside; `false` with what was
/// found once that ran out.
fn ignored_gitignores(
    repo: &crate::git::Repo,
    dir: &Path,
    ignored: &[String],
    budget: u64,
) -> (Vec<String>, bool) {
    let mut found: Vec<String> = ignored
        .iter()
        .filter(|p| !p.ends_with('/') && is_gitignore(p))
        .cloned()
        .collect();
    let mut level: Vec<String> = ignored
        .iter()
        .filter_map(|p| p.strip_suffix('/'))
        .filter(|p| !p.split('/').any(|seg| seg == ".git" || seg == ".apb"))
        .map(str::to_string)
        .collect();
    let mut visits = budget;
    // One level of directories at a time, one `check-ignore` per level.
    while !level.is_empty() {
        let by_rule = ignored_by_rule(repo, dir, &level);
        let mut next = Vec::new();
        for d in level.iter().filter(|d| !by_rule.contains(*d)) {
            if !list_gitignores(dir, d, &mut found, &mut next, &mut visits) {
                return (found, false);
            }
        }
        level = next;
    }
    (found, true)
}

/// Which of `dirs` (relative to the work tree at `dir`) a rule ignores.
/// `check-ignore` names a directory a rule ignores; a directory's own
/// `.gitignore` never applies to the directory itself. It exits 1 when it
/// names none, so an empty set means every directory is looked into.
fn ignored_by_rule(
    repo: &crate::git::Repo,
    dir: &Path,
    dirs: &[String],
) -> std::collections::BTreeSet<String> {
    let mut input = String::new();
    for d in dirs {
        input.push_str(d);
        input.push('\0');
    }
    repo.read_input(dir, &["check-ignore", "-z", "--stdin"], &input)
        .map(|s| split_z(&s).into_iter().collect())
        .unwrap_or_default()
}

/// The entries of directory `rel` under `root`, never following a link:
/// its `.gitignore` into `found`, its directories but `.git` and `.apb`
/// into `next`, each entry charged to `visits`; `false` once that ran out.
fn list_gitignores(
    root: &Path,
    rel: &str,
    found: &mut Vec<String>,
    next: &mut Vec<String>,
    visits: &mut u64,
) -> bool {
    let Ok(read) = std::fs::read_dir(root.join(rel)) else {
        return true;
    };
    for entry in read.flatten() {
        if *visits == 0 {
            return false;
        }
        *visits -= 1;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let path = format!("{rel}/{}", name.to_string_lossy());
        if file_type.is_dir() {
            if name != ".git" && name != ".apb" {
                next.push(path);
            }
        } else if name == ".gitignore" {
            found.push(path);
        }
    }
    true
}

/// The `.apb` definitions at the tree's root ([`APB_DEFINITIONS`]),
/// tree-relative, never following a link.
fn apb_definitions(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for name in APB_DEFINITIONS {
        let path = root.join(".apb").join(name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk_definitions(root, &path, &mut out);
        } else {
            out.push(format!(".apb/{name}"));
        }
    }
    out
}

/// [`walk_entries`] for a directory under `.apb`, whose own name is not
/// skipped.
fn walk_definitions(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_definitions(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// What hashing one file gave.
enum Hashed {
    /// The sha256 and how many bytes were read.
    Done(String, u64),
    /// The file held more than the bytes left (it grew since it was
    /// stat'ed); reading stopped there.
    Over,
    Unreadable,
}

/// The sha256 of the file at `path`, streamed, reading at most `max`
/// bytes.
fn file_digest(path: &Path, max: u64) -> Hashed {
    let Ok(mut f) = crate::fsutil::open_regular_no_follow(path) else {
        return Hashed::Unreadable;
    };
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0u64;
    loop {
        let Ok(read) = f.read(&mut buf) else {
            return Hashed::Unreadable;
        };
        if read == 0 {
            break;
        }
        n += read as u64;
        if n > max {
            return Hashed::Over;
        }
        h.update(&buf[..read]);
    }
    Hashed::Done(crate::content::hex_lower(&h.finalize()), n)
}

#[cfg(unix)]
fn is_exec(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_exec(_meta: &std::fs::Metadata) -> bool {
    false
}

fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128)
}

/// Every file and symlink under `dir` as paths relative to `root`, never
/// following a link and skipping `.git` and `.apb`.
fn walk_entries(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            if name == ".git" || name == ".apb" {
                continue;
            }
            walk_entries(root, &path, out);
        } else if (file_type.is_file() || file_type.is_symlink())
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let ok = crate::git::base()
            .args(["-c", "commit.gpgsign=false", "-c", "user.name=t"])
            .args(["-c", "user.email=t@t", "-C"])
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("a.txt"), "one").unwrap();
        std::fs::write(root.join("b.txt"), "two").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "c1"]);
        dir
    }

    fn kinds(before: &TreeState, root: &Path) -> Vec<(String, &'static str)> {
        before
            .changes(&TreeState::take(root, TreeLimits::default()))
            .into_iter()
            .map(|c| (c.path, c.kind.as_str()))
            .collect()
    }

    fn pair(p: &str, k: &'static str) -> (String, &'static str) {
        (p.to_string(), k)
    }

    /// Each kind of change a read-only attempt can make is named, and an
    /// untouched tree (and an ignored file, and `.apb`) compares clean.
    #[cfg(unix)]
    #[test]
    fn every_change_kind_is_named_and_ignored_paths_are_not() {
        use std::os::unix::fs::PermissionsExt;
        let dir = repo();
        let root = dir.path();
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
        let before = TreeState::take(root, TreeLimits::default());
        assert!(before.partial().is_none());
        assert_eq!(before.entries(), 4);
        assert!(kinds(&before, root).is_empty(), "a tree compares clean");

        std::fs::write(root.join("ignored.txt"), "x").unwrap();
        std::fs::create_dir_all(root.join(".apb/runs")).unwrap();
        std::fs::write(root.join(".apb/runs/x"), "x").unwrap();
        assert!(kinds(&before, root).is_empty(), "ignored and .apb stay out");

        std::fs::write(root.join("a.txt"), "changed").unwrap();
        assert_eq!(kinds(&before, root), [pair("a.txt", "modified")]);
        std::fs::write(root.join("a.txt"), "one").unwrap();

        std::fs::set_permissions(root.join("b.txt"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(kinds(&before, root), [pair("b.txt", "mode")]);
        std::fs::set_permissions(root.join("b.txt"), std::fs::Permissions::from_mode(0o644))
            .unwrap();

        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("b.txt", root.join("link")).unwrap();
        assert_eq!(kinds(&before, root), [pair("link", "link")]);
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();

        std::fs::write(root.join("new.txt"), "n").unwrap();
        std::fs::remove_file(root.join("b.txt")).unwrap();
        assert_eq!(
            kinds(&before, root),
            [pair("b.txt", "deleted"), pair("new.txt", "added")]
        );
        std::fs::write(root.join("b.txt"), "two").unwrap();

        git(root, &["add", "new.txt"]);
        assert_eq!(
            kinds(&before, root),
            [pair("new.txt", "added"), pair("index", "git_index")]
        );
        git(root, &["commit", "-qm", "c2"]);
        assert_eq!(
            kinds(&before, root),
            [
                pair("new.txt", "added"),
                pair("HEAD", "git_head"),
                pair("refs", "git_refs"),
                pair("index", "git_index"),
            ]
        );
    }

    /// The files that change what git does are part of the state: a file
    /// the attempt hides by adding it to `info/exclude`, a planted hook and
    /// a config change each show as `git_control`. The `.apb` definitions
    /// at the root are listed though git ignores `.apb`; the run's own
    /// files there are not.
    #[test]
    fn the_files_that_steer_git_and_the_apb_definitions_are_covered() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n.apb/\n").unwrap();
        std::fs::create_dir_all(root.join(".apb/playbooks/p")).unwrap();
        std::fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
        let before = TreeState::take(root, TreeLimits::default());
        assert!(kinds(&before, root).is_empty(), "a tree compares clean");

        let exclude = root.join(".git/info/exclude");
        let kept = std::fs::read_to_string(&exclude).unwrap_or_default();
        std::fs::write(&exclude, format!("{kept}hidden.txt\n")).unwrap();
        std::fs::write(root.join("hidden.txt"), "x").unwrap();
        assert_eq!(
            kinds(&before, root),
            [pair(".git/info/exclude", "git_control")]
        );
        std::fs::write(&exclude, kept).unwrap();
        std::fs::remove_file(root.join("hidden.txt")).unwrap();

        std::fs::write(root.join(".git/hooks/pre-commit"), "#!/bin/sh\n").unwrap();
        assert_eq!(
            kinds(&before, root),
            [pair(".git/hooks/pre-commit", "git_control")]
        );
        std::fs::remove_file(root.join(".git/hooks/pre-commit")).unwrap();

        git(root, &["config", "core.hooksPath", "elsewhere"]);
        assert_eq!(kinds(&before, root), [pair(".git/config", "git_control")]);
        git(root, &["config", "--unset", "core.hooksPath"]);
        assert!(kinds(&before, root).is_empty(), "the config is back");

        std::fs::write(root.join(".apb/playbooks/p/current"), "2.0.0").unwrap();
        std::fs::write(root.join(".apb/policy.yaml"), "launch: {}\n").unwrap();
        std::fs::create_dir_all(root.join(".apb/runs/r")).unwrap();
        std::fs::write(root.join(".apb/runs/r/events.jsonl"), "{}").unwrap();
        assert_eq!(
            kinds(&before, root),
            [
                pair(".apb/playbooks/p/current", "modified"),
                pair(".apb/policy.yaml", "added"),
            ]
        );
    }

    /// An initialized submodule is listed inside with its own repository
    /// state: a change to one of its files and a commit in it show, though
    /// the parent's own state (its staged gitlink) does not move.
    #[test]
    fn a_submodule_is_covered_inside() {
        let inner = repo();
        let dir = repo();
        let root = dir.path();
        git(
            root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                inner.path().to_str().unwrap(),
                "sub",
            ],
        );
        git(root, &["commit", "-qm", "with sub"]);
        let before = TreeState::take(root, TreeLimits::default());
        assert!(kinds(&before, root).is_empty(), "a tree compares clean");
        assert!(
            before.entries.contains_key("sub/a.txt"),
            "{:?}",
            before.entries.keys()
        );

        std::fs::write(root.join("sub/a.txt"), "changed").unwrap();
        assert_eq!(kinds(&before, root), [pair("sub/a.txt", "modified")]);

        git(&root.join("sub"), &["commit", "-qam", "inside"]);
        let changed = kinds(&before, root);
        assert!(
            changed.contains(&pair("sub/HEAD", "git_head")),
            "{changed:?}"
        );
        assert!(
            changed.contains(&pair("sub/refs", "git_refs")),
            "{changed:?}"
        );
        assert!(!changed.iter().any(|(p, _)| p == "HEAD"), "{changed:?}");
    }

    /// A new `.gitignore` that ignores itself, and the file it hides, shows
    /// as added; so does one added to an existing directory. A
    /// `.gitignore` under a directory an outer rule ignores (a cache a
    /// test run writes) is not looked at.
    #[test]
    fn a_self_hiding_gitignore_shows_as_added() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "ignored.txt\ncache/\n").unwrap();
        git(root, &["commit", "-qam", "ignore cache"]);
        std::fs::create_dir_all(root.join("cache/sub")).unwrap();
        std::fs::write(root.join("cache/sub/.gitignore"), "*\n").unwrap();
        let before = TreeState::take(root, TreeLimits::default());
        assert!(kinds(&before, root).is_empty(), "a tree compares clean");

        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(root.join("tools/.gitignore"), "*\n").unwrap();
        std::fs::write(root.join("tools/conftest.py"), "x").unwrap();
        assert_eq!(kinds(&before, root), [pair("tools/.gitignore", "added")]);
        std::fs::remove_dir_all(root.join("tools")).unwrap();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/.gitignore"), ".gitignore\nplanted.py\n").unwrap();
        std::fs::write(root.join("src/planted.py"), "x").unwrap();
        assert_eq!(kinds(&before, root), [pair("src/.gitignore", "added")]);
        std::fs::remove_dir_all(root.join("src")).unwrap();

        std::fs::create_dir_all(root.join("cache/new")).unwrap();
        std::fs::write(root.join("cache/new/.gitignore"), "*\n").unwrap();
        assert!(
            kinds(&before, root).is_empty(),
            "under an ignored directory"
        );
        std::fs::remove_dir_all(root.join("cache/new")).unwrap();

        // Inside a directory only its own `.gitignore` hides, a directory
        // that rule ignores is not looked into either.
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(root.join("tools/.gitignore"), "*\n").unwrap();
        let before = TreeState::take(root, TreeLimits::default());
        std::fs::create_dir_all(root.join("tools/cache")).unwrap();
        std::fs::write(root.join("tools/cache/.gitignore"), "*\n").unwrap();
        assert!(
            kinds(&before, root).is_empty(),
            "under a directory a parent's own rule ignores"
        );
    }

    /// Past the bound of a git directory's control entries the state is
    /// partial, and leaving the bound reads as a change.
    #[test]
    fn the_control_bound_makes_the_state_partial() {
        let dir = repo();
        let root = dir.path();
        let hooks = root.join(".git/hooks/many");
        std::fs::create_dir_all(&hooks).unwrap();
        for i in 0..MAX_CONTROL_FILES {
            std::fs::write(hooks.join(format!("h{i}")), "x").unwrap();
        }
        let before = TreeState::take(root, TreeLimits::default());
        assert_eq!(before.partial(), Some(TreeLimit::Entries));
        assert!(
            kinds(&before, root).is_empty(),
            "the same tree compares clean"
        );
        std::fs::remove_dir_all(&hooks).unwrap();
        assert!(
            kinds(&before, root).contains(&pair("$GIT_DIR/...", "git_control")),
            "{:?}",
            kinds(&before, root)
        );
    }

    /// A repository nested deeper than the bound is not looked into, so the
    /// state is partial; at the bound everything is listed.
    #[test]
    fn the_nesting_bound_makes_the_state_partial() {
        let dir = repo();
        let mut at = dir.path().to_path_buf();
        for i in 1..=MAX_NESTED {
            at = at.join(format!("n{i}"));
            std::fs::create_dir_all(&at).unwrap();
            git(&at, &["init", "-q"]);
            std::fs::write(at.join("f"), "x").unwrap();
        }
        let deep = TreeState::take(dir.path(), TreeLimits::default());
        assert_eq!(deep.partial(), None, "{MAX_NESTED} deep is covered");
        let last = at.join("f").strip_prefix(dir.path()).unwrap().to_owned();
        assert!(deep.entries.contains_key(last.to_str().unwrap()));
        at = at.join("deeper");
        std::fs::create_dir_all(&at).unwrap();
        git(&at, &["init", "-q"]);
        std::fs::write(at.join("f"), "x").unwrap();
        let deeper = TreeState::take(dir.path(), TreeLimits::default());
        assert_eq!(deeper.partial(), Some(TreeLimit::Entries));
    }

    /// The control files are charged to the byte limit: one past it is
    /// held by its size and time, and the state is partial.
    #[test]
    fn control_files_count_toward_the_byte_limit() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join(".git/hooks/big"), vec![b'x'; 4096]).unwrap();
        let small = TreeLimits {
            entries: DEFAULT_MAX_ENTRIES,
            bytes: 1024,
        };
        let before = TreeState::take(root, small);
        assert_eq!(before.partial(), Some(TreeLimit::Bytes));
        assert!(before.bytes() <= 1024, "{}", before.bytes());
        std::fs::write(root.join(".git/hooks/big"), vec![b'y'; 4097]).unwrap();
        let changed: Vec<_> = before
            .changes(&TreeState::take(root, small))
            .into_iter()
            .map(|c| (c.path, c.kind.as_str()))
            .collect();
        assert!(
            changed.contains(&pair(".git/hooks/big", "git_control")),
            "{changed:?}"
        );
    }

    /// With a submodule the entry limit cuts where the listing stopped: a
    /// file that sorts after the submodule's entries (`sub-a.txt`) is not
    /// in the range once the cut falls inside the submodule.
    #[test]
    fn a_cut_inside_a_submodule_covers_only_what_was_listed() {
        let inner = repo();
        let dir = repo();
        let root = dir.path();
        git(
            root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                inner.path().to_str().unwrap(),
                "sub",
            ],
        );
        std::fs::write(root.join("sub-a.txt"), "x").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "with sub"]);
        // `.gitignore`, `.gitmodules`, `a.txt`, `b.txt`, then `sub/.gitignore`.
        let tight = TreeLimits {
            entries: 5,
            bytes: DEFAULT_MAX_BYTES,
        };
        let before = TreeState::take(root, tight);
        assert_eq!(before.partial(), Some(TreeLimit::Entries));
        assert!(
            before.entries.contains_key("sub/.gitignore"),
            "{:?}",
            before.entries.keys()
        );
        assert!(!before.entries.contains_key("sub-a.txt"));
        assert!(!before.covers("sub-a.txt"), "cut {:?}", before.cut);
        assert!(before.covers("b.txt"));
    }

    /// In a linked worktree the hooks live in the common git directory and
    /// the worktree's own config in its git directory: both are covered.
    #[test]
    fn a_linked_worktree_covers_its_common_hooks() {
        let dir = repo();
        let main = dir.path();
        let linked = tempfile::tempdir().unwrap();
        let wt = linked.path().join("wt");
        git(main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        let before = TreeState::take(&wt, TreeLimits::default());
        assert!(kinds(&before, &wt).is_empty(), "a tree compares clean");
        std::fs::write(main.join(".git/hooks/post-checkout"), "#!/bin/sh\n").unwrap();
        assert_eq!(
            kinds(&before, &wt),
            [pair("$GIT_COMMON_DIR/hooks/post-checkout", "git_control")]
        );
    }

    /// The entry limit lists a prefix and compares only that range; the
    /// byte limit falls back to size and time for the rest. Both mark the
    /// state partial.
    #[test]
    fn limits_make_the_state_partial_and_never_hide_a_change_in_range() {
        let dir = repo();
        let root = dir.path();
        let tight = TreeLimits {
            entries: 2,
            bytes: DEFAULT_MAX_BYTES,
        };
        let before = TreeState::take(root, tight);
        assert_eq!(before.partial(), Some(TreeLimit::Entries));
        assert_eq!(before.entries(), 2);
        // `.gitignore` and `a.txt` are listed, `b.txt` is not.
        std::fs::write(root.join("b.txt"), "unseen").unwrap();
        assert!(before.changes(&TreeState::take(root, tight)).is_empty());
        std::fs::write(root.join("a.txt"), "seen").unwrap();
        assert_eq!(
            before.changes(&TreeState::take(root, tight)),
            [TreeChange {
                path: "a.txt".into(),
                kind: ChangeKind::Modified
            }]
        );

        let few_bytes = TreeLimits {
            entries: DEFAULT_MAX_ENTRIES,
            bytes: 12,
        };
        let before = TreeState::take(root, few_bytes);
        assert_eq!(before.partial(), Some(TreeLimit::Bytes));
        std::fs::write(root.join("b.txt"), "a longer content").unwrap();
        let after = TreeState::take(root, few_bytes);
        assert_eq!(
            before
                .changes(&after)
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            ["b.txt"]
        );
    }

    /// A tree an enclosing repository ignores as a whole (a project under
    /// another repository's ignored build directory) is listed like one
    /// outside git: git would call every file in it ignored, and the check
    /// would compare nothing.
    #[test]
    fn a_tree_its_enclosing_repository_ignores_is_listed_like_one_outside_git() {
        let outer = repo();
        std::fs::write(outer.path().join(".gitignore"), "build/\n").unwrap();
        let root = outer.path().join("build/project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("x"), "x").unwrap();
        let before = TreeState::take(&root, TreeLimits::default());
        assert_eq!(before.entries(), 1);
        std::fs::write(root.join("y"), "y").unwrap();
        assert_eq!(kinds(&before, &root), [pair("y", "added")]);
    }

    /// Outside git every file and link is listed, with no repository state.
    #[test]
    fn outside_git_every_file_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/x"), "x").unwrap();
        let before = TreeState::take(root, TreeLimits::default());
        assert_eq!(before.entries(), 1);
        std::fs::write(root.join("sub/y"), "y").unwrap();
        assert_eq!(kinds(&before, root), [pair("sub/y", "added")]);
    }
}
