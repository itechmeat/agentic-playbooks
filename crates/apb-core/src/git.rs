//! apb's own git commands, hardened against the tree they read
//! ([decision record 0009](../../../docs/adr/0009-apb-git-never-runs-a-program-the-tree-names.md)).
//!
//! A working tree's `.git/config`, `.gitattributes` and hooks are the
//! agent's to edit (and a cloned repository's to ship), so they may name a
//! program: a filesystem monitor, a hooks directory, a signature verifier, a
//! transport, a credential helper, an external diff, a textconv or a
//! clean/smudge filter. The operator's shell may carry `GIT_DIR` and friends
//! (inside a git hook, `git rebase -x`, `git bisect run`) or
//! `GIT_CONFIG_PARAMETERS` that turn one back on. Every git call apb itself
//! makes on a repository goes through [`Repo`]:
//!
//! - every `GIT_*` variable of apb's own environment is removed, and no
//!   pager runs;
//! - [`HARDENED_SETTINGS`] are passed with `-c`, which outranks every config
//!   file (an included one too): no filesystem monitor, hooks from
//!   `/dev/null`, no signature display, no transport at all (so no ssh
//!   command, proxy, askpass or credential helper can run), no credential
//!   helper and no askpass;
//! - the repository is pinned: `GIT_DIR`, `GIT_COMMON_DIR` and
//!   `GIT_WORK_TREE` are set to the canonical paths git discovered, and a
//!   discovery whose common directory is not the git directory itself (or,
//!   for a linked worktree, the directory its `worktrees/` belongs to) is
//!   refused, so a planted `commondir` cannot hand apb another config;
//! - every filter driver the repository's own config (its local or worktree
//!   scope, includes counted) defines has its `clean`, `smudge` and
//!   `process` emptied and `required` turned off, so a `.gitattributes` line
//!   naming it runs nothing; drivers only the operator's global or system
//!   config defines (git-lfs) keep working;
//! - every diff carries [`DIFF_FLAGS`] and every status [`STATUS_FLAGS`]:
//!   no external diff, no textconv, and no git started inside a submodule
//!   (whose own config would apply there); `submodule.recurse` is off, so
//!   no checkout recurses either;
//! - git-lfs leaves pointer files on a checkout (`GIT_LFS_SKIP_SMUDGE`), so
//!   a tree's `.lfsconfig` never makes apb download from a host it names;
//! - git older than 2.31 is refused ([`RepoError::GitTooOld`]): the
//!   discovery needs `rev-parse --path-format` and the driver listing
//!   `config --show-scope`.

pub mod transport;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The `-c` settings every hardened git call carries.
pub const HARDENED_SETTINGS: [&str; 9] = [
    "core.fsmonitor=false",
    "core.hooksPath=/dev/null",
    "log.showSignature=false",
    "protocol.allow=never",
    NO_CREDENTIAL_HELPER,
    "core.askPass=",
    "diff.ignoreSubmodules=dirty",
    "status.submoduleSummary=false",
    "submodule.recurse=false",
];

/// The flags every diff apb takes carries: neither `diff.external`, a
/// driver's `command` nor a driver's `textconv` runs, and no git starts
/// inside a submodule to see whether its work tree is dirty. With `dirty`
/// (not `all`) a change of the commit a submodule points at still shows:
/// git reads the submodule's `HEAD` in its own process.
pub const DIFF_FLAGS: [&str; 3] = [
    "--no-ext-diff",
    "--no-textconv",
    "--ignore-submodules=dirty",
];

/// The flags every status apb takes carries: no git inside a submodule.
pub const STATUS_FLAGS: [&str; 1] = ["--ignore-submodules=dirty"];

/// The oldest git apb runs on a repository.
pub const MIN_GIT: (u32, u32) = (2, 31);

/// The filter keys that name a program, emptied for a tree's own drivers.
const FILTER_PROGRAMS: [&str; 3] = ["clean", "smudge", "process"];

/// The `-c` setting that empties the credential helper list; the one
/// hardened setting the transport builder leaves out
/// ([`transport::command`]), so the operator's own helper works there.
pub(crate) const NO_CREDENTIAL_HELPER: &str = "credential.helper=";

/// The names of apb's own environment variables that start with `GIT_`.
fn git_vars() -> Vec<OsString> {
    std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| k.as_encoded_bytes().starts_with(b"GIT_"))
        .collect()
}

/// `git` with the hardening applied and no repository: the caller adds
/// `-C` (or `--git-dir`), any further `-c` and the subcommand. A later `-c`
/// of the same key overrides these (the eval runner points
/// `core.hooksPath` at its own empty directory and allows the `file`
/// transport). Use [`Repo`] for anything that reads a working tree.
pub fn base() -> Command {
    let mut cmd = bare_git();
    for s in HARDENED_SETTINGS {
        cmd.arg("-c").arg(s);
    }
    cmd
}

/// `git --no-pager` without any of apb's own `GIT_*` variables and without
/// settings: the start of [`base`] and of [`transport::command`].
fn bare_git() -> Command {
    let mut cmd = Command::new("git");
    for k in git_vars() {
        cmd.env_remove(k);
    }
    cmd.arg("--no-pager");
    cmd
}

/// Why a directory is not a repository apb runs git in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepoError {
    /// Not inside a working tree, or git is unavailable.
    #[error("not a git working tree")]
    NotARepository,
    /// The common directory is neither the git directory nor the owner of
    /// its `worktrees/` entry: a planted `commondir`.
    #[error("the git directory `{git_dir}` names `{common_dir}` as its common directory")]
    Redirected { git_dir: String, common_dir: String },
    /// The repository's config could not be listed.
    #[error("the repository's config cannot be read")]
    Config,
    /// The installed git is older than [`MIN_GIT`].
    #[error("git {0} is too old: apb needs git 2.31 or newer")]
    GitTooOld(String),
}

/// The installed git's version (`2.53.0`), read once per process; `None`
/// without git.
fn git_version() -> Option<&'static str> {
    static VERSION: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            let out = base().arg("--version").stdin(Stdio::null()).output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            text.split_whitespace().nth(2).map(str::to_string)
        })
        .as_deref()
}

/// Whether `version` is at least [`MIN_GIT`].
fn new_enough(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(0)
    });
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= MIN_GIT
}

/// A repository apb runs git in, pinned to the directories git discovered
/// and with the tree's own filter drivers neutralized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub work_tree: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    /// `(key, value)` settings that neutralize the tree's filter drivers,
    /// passed as `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` (a driver name
    /// may hold any character but a newline, which `-c` cannot carry).
    neutral: Vec<(String, String)>,
}

impl Repo {
    /// The repository `dir` belongs to.
    pub fn locate(dir: &Path) -> Result<Repo, RepoError> {
        match git_version() {
            None => return Err(RepoError::NotARepository),
            Some(v) if !new_enough(v) => return Err(RepoError::GitTooOld(v.to_string())),
            Some(_) => {}
        }
        let out = base()
            .arg("-C")
            .arg(dir)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--absolute-git-dir",
                "--git-common-dir",
                "--show-toplevel",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| RepoError::NotARepository)?;
        if !out.status.success() {
            return Err(RepoError::NotARepository);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines().map(|l| Path::new(l).canonicalize());
        let (Some(Ok(git_dir)), Some(Ok(common_dir)), Some(Ok(work_tree))) =
            (lines.next(), lines.next(), lines.next())
        else {
            return Err(RepoError::NotARepository);
        };
        check_common(&git_dir, &common_dir)?;
        let mut repo = Repo {
            work_tree,
            git_dir,
            common_dir,
            neutral: Vec::new(),
        };
        repo.neutral = neutral_settings(&repo.tree_filters()?);
        Ok(repo)
    }

    /// The hardened `git` pinned to this repository, without the filter
    /// neutralization.
    fn pinned(&self) -> Command {
        let mut cmd = base();
        cmd.env("GIT_DIR", &self.git_dir)
            .env("GIT_COMMON_DIR", &self.common_dir)
            .env("GIT_WORK_TREE", &self.work_tree);
        cmd
    }

    /// The filter driver names the repository's own config defines.
    fn tree_filters(&self) -> Result<Vec<String>, RepoError> {
        let out = self
            .pinned()
            .arg("-C")
            .arg(&self.work_tree)
            .args([
                "config",
                "--show-scope",
                "--name-only",
                "-z",
                "--get-regexp",
                r"^filter\.",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| RepoError::Config)?;
        match out.status.code() {
            Some(0) => Ok(tree_drivers(&out.stdout, "filter")),
            // No key matched.
            Some(1) if out.stdout.is_empty() => Ok(Vec::new()),
            _ => Err(RepoError::Config),
        }
    }

    /// The hardened, pinned `git -C dir`; the caller adds the subcommand.
    /// For a diff use [`Repo::diff`].
    pub fn command(&self, dir: &Path) -> Command {
        let mut cmd = self.pinned();
        cmd.env("GIT_CONFIG_COUNT", self.neutral.len().to_string());
        for (i, (k, v)) in self.neutral.iter().enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), k)
                .env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        cmd.env("GIT_LFS_SKIP_SMUDGE", "1");
        cmd.arg("-C").arg(dir);
        cmd
    }

    /// `git -C dir status` with [`STATUS_FLAGS`]; the caller adds the rest.
    pub fn status(&self, dir: &Path) -> Command {
        let mut cmd = self.command(dir);
        cmd.arg("status").args(STATUS_FLAGS);
        cmd
    }

    /// Whether `git -C dir <args>` exits 0.
    pub fn succeeds(&self, dir: &Path, args: &[&str]) -> bool {
        self.command(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// `git -C dir diff` with [`DIFF_FLAGS`]; the caller adds the rest.
    pub fn diff(&self, dir: &Path) -> Command {
        let mut cmd = self.command(dir);
        cmd.arg("diff").args(DIFF_FLAGS);
        cmd
    }

    /// The stdout of `git -C dir <args>`, `None` on any failure.
    pub fn read(&self, dir: &Path, args: &[&str]) -> Option<String> {
        stdout_of(self.command(dir).args(args))
    }

    /// The stdout of `git -C dir <args>` with `input` on its stdin (a
    /// `--stdin` revision list, a `cat-file --batch-check` query), `None` on
    /// any failure. One process however many lines `input` holds.
    pub fn read_input(&self, dir: &Path, args: &[&str], input: &str) -> Option<String> {
        use std::io::Write;
        let mut child = self
            .command(dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // A writer thread: git may fill its stdout before it read all of
        // its stdin.
        let mut stdin = child.stdin.take()?;
        let input = input.to_string();
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let out = child.wait_with_output().ok()?;
        writer.join().ok()?.ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// A git directory may name another common directory only as a linked
/// worktree's admin directory `<common>/worktrees/<name>`.
fn check_common(git_dir: &Path, common_dir: &Path) -> Result<(), RepoError> {
    if git_dir == common_dir || git_dir.parent() == Some(common_dir.join("worktrees").as_path()) {
        return Ok(());
    }
    Err(RepoError::Redirected {
        git_dir: git_dir.display().to_string(),
        common_dir: common_dir.display().to_string(),
    })
}

/// The driver names of `section` that a `--show-scope --name-only -z`
/// listing shows outside the operator's own (global and system) config.
fn tree_drivers(listing: &[u8], section: &str) -> Vec<String> {
    let text = String::from_utf8_lossy(listing);
    let fields: Vec<&str> = text.split('\0').collect();
    let prefix = format!("{section}.");
    let mut names: Vec<String> = Vec::new();
    for pair in fields.chunks(2) {
        let [scope, key] = pair else { continue };
        if matches!(*scope, "global" | "system") {
            continue;
        }
        let Some((name, _var)) = key
            .strip_prefix(prefix.as_str())
            .and_then(|r| r.rsplit_once('.'))
        else {
            continue;
        };
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// The settings that make each filter driver of `names` run nothing.
fn neutral_settings(names: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in names {
        for var in FILTER_PROGRAMS {
            out.push((format!("filter.{name}.{var}"), String::new()));
        }
        out.push((format!("filter.{name}.required"), "false".to_string()));
    }
    out
}

/// The stdout of a prepared git command, `None` on any failure.
pub(crate) fn stdout_of(cmd: &mut Command) -> Option<String> {
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The hardened, pinned `git -C dir`, `None` when `dir` is not a working
/// tree apb runs git in ([`Repo::locate`]).
pub fn command(dir: &Path) -> Option<Command> {
    Repo::locate(dir).ok().map(|r| r.command(dir))
}

/// The stdout of the hardened `git -C dir <args>`, `None` on any failure
/// (no git, not a repository apb runs git in, a non-zero exit).
pub fn read(dir: &Path, args: &[&str]) -> Option<String> {
    Repo::locate(dir).ok()?.read(dir, args)
}

/// [`Repo::read_input`] for the repository `dir` belongs to.
pub fn read_input(dir: &Path, args: &[&str], input: &str) -> Option<String> {
    Repo::locate(dir).ok()?.read_input(dir, args, input)
}

/// Whether the hardened `git -C dir <args>` exits 0: `Ok(false)` outside a
/// repository (or without git), an error for a repository apb refuses to
/// run git in (a redirected one, an unreadable config, a git too old), so a
/// caller for whom `false` is the permissive answer can fail closed.
pub fn succeeds(dir: &Path, args: &[&str]) -> Result<bool, RepoError> {
    match Repo::locate(dir) {
        Ok(repo) => Ok(repo.succeeds(dir, args)),
        Err(RepoError::NotARepository) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_precede_the_directory_and_the_subcommand() {
        let mut cmd = base();
        cmd.arg("-C").arg("/x");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(args[0], "--no-pager");
        assert!(args.contains(&"core.hooksPath=/dev/null".into()));
        assert!(args.contains(&"protocol.allow=never".into()));
        let c = args.iter().position(|a| a == "-C").unwrap();
        assert_eq!(args[c + 1], "/x");
        assert_eq!(c, 1 + HARDENED_SETTINGS.len() * 2);
    }

    #[test]
    fn git_older_than_2_31_is_refused() {
        assert!(new_enough("2.31.0") && new_enough("2.53.0") && new_enough("3.0"));
        assert!(new_enough("2.39.5 (Apple Git-154)"));
        assert!(!new_enough("2.30.2") && !new_enough("2.25.1"));
    }

    #[test]
    fn only_drivers_outside_the_operators_config_are_neutralized() {
        let listing = b"global\0filter.lfs.clean\0local\0filter.x.clean\0local\0filter.a.b.smudge\0worktree\0filter.x.process\0system\0filter.sys.clean\0";
        assert_eq!(tree_drivers(listing, "filter"), ["x", "a.b"]);
        let s = neutral_settings(&["x".into()]);
        assert!(s.contains(&("filter.x.clean".into(), String::new())));
        assert!(s.contains(&("filter.x.process".into(), String::new())));
        assert!(s.contains(&("filter.x.required".into(), "false".into())));
    }

    #[test]
    fn a_common_dir_is_the_git_dir_or_owns_its_worktree_entry() {
        let c = Path::new("/r/.git");
        assert!(check_common(c, c).is_ok());
        assert!(check_common(Path::new("/r/.git/worktrees/w"), c).is_ok());
        assert!(check_common(c, Path::new("/evil")).is_err());
        assert!(check_common(Path::new("/r/.git/worktrees/w"), Path::new("/evil")).is_err());
        assert!(check_common(Path::new("/r/.git/modules/m"), c).is_err());
    }
}
