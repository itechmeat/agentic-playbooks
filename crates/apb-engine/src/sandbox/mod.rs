//! The isolation sandbox of an agent attempt on Linux: bubblewrap,
//! unprivileged, plus apb's own helper inside it (`apb __sandbox-exec`,
//! [`scope`]). [`probe`] learns what the machine offers, [`view`] decides
//! what the agent sees, [`bwrap`] turns that into a command line, [`home`]
//! lays the sandbox home down. [`build`] puts one attempt's
//! [`SandboxPlan`] together; `adapter::command` wraps the spawn with it and
//! `adapter::spawn_env` applies its environment overlay last.
//!
//! The contract a plan enforces is `apb_core::isolation::contract`; this
//! module only carries it out.

pub mod bwrap;
pub mod git_guard;
pub mod home;
pub mod probe;
pub mod scope;
pub mod view;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::agents::{Access, SandboxNeeds};
use apb_core::isolation::contract::HostFacts;
use apb_core::isolation::{IsolationSpec, Network, paths};

use view::{Mount, ViewFacts};

/// The private runtime directory inside the sandbox (`XDG_RUNTIME_DIR`).
pub const RUNTIME_DIR: &str = "/tmp/apb-runtime";

/// Variables that reach the operator's session; dropped from every
/// sandboxed process.
const SESSION_VARS: [&str; 6] = [
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_AGENT_INFO",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
];

/// How one attempt's process is sandboxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    pub bwrap: PathBuf,
    pub bwrap_version: Option<String>,
    /// bwrap 0.8 or newer: the sandbox cannot make a user namespace.
    pub disable_userns: bool,
    /// The apb executable, run inside as `__sandbox-exec`.
    pub helper: PathBuf,
    /// Apply the Landlock scope, failing closed without it.
    pub scope: bool,
    pub network: Network,
    pub mounts: Vec<Mount>,
    pub cwd: PathBuf,
    pub runtime_dir: PathBuf,
    /// The operator's home (the sandbox home is bound there).
    pub home: PathBuf,
    /// Mount points apb created in the tree, deepest last; removed after
    /// the attempt when empty.
    pub created: Vec<PathBuf>,
    /// The files that redirect git, as they were before the attempt.
    pub git_guard: Option<git_guard::Snapshot>,
}

impl SandboxPlan {
    /// `bwrap ... -- apb __sandbox-exec ... -- <program> <args>`, in the
    /// plan's working directory.
    pub fn command(&self, program: &OsStr, args: &[OsString]) -> Command {
        let line = bwrap::Line {
            network: self.network,
            disable_userns: self.disable_userns,
            mounts: &self.mounts,
            cwd: &self.cwd,
            helper: &self.helper,
            scope: self.scope,
        };
        let mut cmd = Command::new(&self.bwrap);
        cmd.args(bwrap::argv(&line, program, args));
        cmd.current_dir(&self.cwd);
        cmd
    }

    /// The environment overlay every sandboxed process gets last: its own
    /// temp and runtime directories, the session's sockets and display
    /// dropped.
    pub fn overlay(&self, cmd: &mut Command) {
        // The sandbox home is bound at the home's canonical path; `HOME`
        // names that path, also where the operator's `HOME` goes through a
        // link (`/home` -> `/var/home`).
        cmd.env("HOME", &self.home);
        for k in ["TMPDIR", "TMP", "TEMP"] {
            cmd.env(k, "/tmp");
        }
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        for k in SESSION_VARS {
            cmd.env_remove(k);
        }
        let explicit: Vec<OsString> = cmd.get_envs().map(|(k, _)| k.to_os_string()).collect();
        for k in std::env::vars_os().map(|(k, _)| k).chain(explicit) {
            if k.as_encoded_bytes().starts_with(b"DBUS_") {
                cmd.env_remove(k);
            }
        }
    }

    /// Removes the mount points apb created in the tree, when empty, and
    /// puts back any git file the attempt redirected.
    pub fn cleanup(&self) {
        for p in self.created.iter().rev() {
            let _ = std::fs::remove_dir(p);
        }
        let _ = self.check_git();
    }

    /// What the attempt changed that redirects git or starts one in the
    /// tree ([`git_guard`]), put back; the failure that names it, `None`
    /// when nothing changed. `fatal`: something could not be put back.
    pub fn check_git(&self) -> Option<GitViolation> {
        let out = self.git_guard.as_ref()?.restore();
        (!out.changes.is_empty()).then(|| GitViolation {
            reason: format!(
                "isolation: the attempt changed what redirects git: {}",
                out.changes.join("; ")
            ),
            fatal: out.fatal,
        })
    }

    /// The view as an attempt record shows it.
    pub fn shown(&self, cap: usize) -> Vec<(String, &'static str)> {
        view::shown(&self.mounts, &self.home, cap)
    }
}

/// Why an attempt fails after it changed what redirects git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitViolation {
    pub reason: String,
    /// Something could not be put back: the node fails at once.
    pub fatal: bool,
}

/// What one attempt's sandbox is built from.
pub struct Request<'a> {
    pub host: &'a HostFacts,
    pub run_dir: &'a Path,
    /// The node's tree: the working directory.
    pub tree: &'a Path,
    /// The canonical agent id.
    pub agent: &'a str,
    pub node: &'a str,
    /// The attempt's own directory under the run (status file, skills).
    pub attempt_box: &'a Path,
    pub program: &'a str,
    pub spec: &'a IsolationSpec,
    pub scope: bool,
    pub needs: &'a SandboxNeeds,
    /// One of the agent's login variables is passed to it.
    pub login_from_env: bool,
    /// Files the agent reads that live in the run (claude's settings).
    pub run_files: Vec<PathBuf>,
    /// The attempt's skill copies (an empty directory when the profile
    /// grants none, which still covers the tree's own skills).
    pub skills: Option<PathBuf>,
}

/// Builds one attempt's plan: gathers the facts, lays the sandbox home
/// down, creates the mount points apb owns, and plans the view.
pub fn build(req: &Request) -> Result<SandboxPlan, String> {
    let bw = req
        .host
        .bwrap
        .as_ref()
        .ok_or_else(|| "bwrap is not available".to_string())?;
    probe::recheck(&bw.path)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| paths::is_plain_absolute(h) && h != Path::new("/"))
        .ok_or_else(|| "HOME is not an absolute path".to_string())?
        .canonicalize()
        .map_err(|e| format!("HOME: {e}"))?;
    let tree = req
        .tree
        .canonicalize()
        .map_err(|e| format!("the tree: {e}"))?;
    paths::check_resolved(&tree, &home, paths::Use::Tree)
        .map_err(|why| format!("the tree {why}"))?;
    let helper = apb_core::fsutil::reexec_exe().map_err(|e| format!("the apb executable: {e}"))?;
    let sandbox_home = home::dir(req.run_dir, req.agent, req.node);
    let logins = home::prepare(
        &sandbox_home,
        &home,
        req.needs,
        req.login_from_env,
        &home::operator_identity(),
    )
    .map_err(|e| format!("the sandbox home: {e}"))?;
    let git = git_guard::facts(&tree)?;
    let agent_home = crate::agent_home::scoped_dir(req.run_dir, req.agent, req.node);
    std::fs::create_dir_all(&agent_home).map_err(|e| format!("the agent home: {e}"))?;
    let mut run_paths = vec![
        (agent_home, Access::ReadWrite),
        (req.attempt_box.to_path_buf(), Access::ReadWrite),
    ];
    // The skill copies live in the box: read-only through its path too.
    run_paths.extend(req.skills.iter().map(|s| (s.clone(), Access::ReadOnly)));
    run_paths.extend(
        req.run_files
            .iter()
            .filter(|p| p.exists())
            .map(|p| (p.clone(), Access::ReadOnly)),
    );
    let mut read_only = Vec::new();
    for p in path_entries(&home) {
        push_unique(&mut read_only, p);
    }
    if let Some(dir) = program_dir(req.program).filter(|d| !home.starts_with(d)) {
        push_unique(&mut read_only, dir);
    }
    let (declared_ro, writable) = declared_extras(req.spec, &home)?;
    for p in declared_ro {
        push_unique(&mut read_only, p);
    }
    // A read-only path inside the tree is already visible; binding it again
    // would make that part of the tree read-only.
    read_only.retain(|p| !p.starts_with(&tree));
    let masks = masks(&home, read_only.iter().chain(&writable).chain([&tree]));
    let mut created = Vec::new();
    let mut skill_points = Vec::new();
    if let Some(skills) = &req.skills {
        let granted = std::fs::read_dir(skills).is_ok_and(|mut d| d.next().is_some());
        for point in view::skill_mount_points(&tree) {
            // Granted skills get their mount points; with none granted, only
            // the tree's own skill directories are covered (by an empty one).
            if link_on_the_way(&tree, &point) {
                return Err(format!("{} is or lies below a symlink", point.display()));
            }
            if granted {
                create_owned(&tree, &point, &mut created)?;
            }
            if point.is_dir() {
                skill_points.push(point);
            }
        }
    }
    let git_guard = git.as_ref().map(git_guard::Snapshot::take);
    let facts = ViewFacts {
        apb_dir: Some(tree.join(".apb")).filter(|p| p.is_dir()),
        home: home.clone(),
        sandbox_home,
        tree: tree.clone(),
        git,
        run_paths,
        resolv: resolv_target(),
        runtime_dir: PathBuf::from(RUNTIME_DIR),
        read_only,
        writable,
        masks,
        logins,
        helper: Some(helper.clone()),
        skills: req.skills.clone(),
        skill_points,
    };
    Ok(SandboxPlan {
        bwrap: bw.path.clone(),
        bwrap_version: bw.version.clone(),
        disable_userns: probe::at_least(bw.version.as_deref(), 0, 8),
        helper,
        scope: req.scope,
        network: req.spec.network(),
        mounts: view::plan(&facts),
        cwd: tree,
        runtime_dir: PathBuf::from(RUNTIME_DIR),
        home,
        created,
        git_guard,
    })
}

/// The machine's and the node's read-only extras and the node's writable
/// ones, resolved on this host and checked again
/// ([`paths::check_resolved`]): a symlink or an absolute path into a
/// credential location, the home or a parent of it, and a writable path
/// onto a persistence location refuse the attempt. A read-only path that
/// does not exist is left out; a writable one that does not refuses, since
/// the agent's writes would land in the throwaway sandbox home.
fn declared_extras(
    spec: &IsolationSpec,
    home: &Path,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let machine = apb_core::config::GlobalConfig::load()
        .map_err(|e| format!("the global config: {e}"))?
        .isolation
        .read_only;
    let mut read_only = Vec::new();
    for p in machine.iter().chain(&spec.read_only) {
        if let Ok(c) = paths::expand(p, home).canonicalize() {
            paths::check_resolved(&c, home, paths::Use::ReadOnly)
                .map_err(|why| format!("read_only path `{p}` {why}"))?;
            read_only.push(c);
        }
    }
    let mut writable = Vec::new();
    for p in &spec.writable {
        let c = paths::expand(p, home)
            .canonicalize()
            .map_err(|e| format!("writable path `{p}`: {e}"))?;
        paths::check_resolved(&c, home, paths::Use::Writable)
            .map_err(|why| format!("writable path `{p}` {why}"))?;
        writable.push(c);
    }
    Ok((read_only, writable))
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.contains(&p) {
        v.push(p);
    }
}

/// The canonical PATH entries that are not already part of the system
/// view. A relative entry and the home or a parent of it are never bound.
fn path_entries(home: &Path) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|p| p.is_absolute())
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| !home.starts_with(p))
        .filter(|p| !in_system(p))
        .collect()
}

fn in_system(p: &Path) -> bool {
    view::SYSTEM_READ_ONLY
        .iter()
        .any(|s| p.starts_with(s) && Path::new(s).exists() && !is_link(Path::new(s)))
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// The canonical directory of the program (a PATH lookup for a bare name).
fn program_dir(program: &str) -> Option<PathBuf> {
    let path = if program.contains('/') {
        PathBuf::from(program)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(program))
            .find(|p| p.is_file())?
    };
    path.canonicalize().ok()?.parent().map(Path::to_path_buf)
}

/// The credential locations inside the bound directories, with whether
/// each is a directory.
fn masks<'a>(home: &Path, bound: impl Iterator<Item = &'a PathBuf>) -> Vec<(PathBuf, bool)> {
    let mut out: Vec<(PathBuf, bool)> = Vec::new();
    for dir in bound {
        for loc in paths::credentials_inside(dir, home) {
            if let Ok(m) = std::fs::metadata(&loc)
                && !out.iter().any(|(p, _)| p == &loc)
            {
                out.push((loc, m.is_dir()));
            }
        }
    }
    out
}

/// The resolver file's real target when `/etc/resolv.conf` points outside
/// the system view (the systemd-resolved stub under `/run`).
fn resolv_target() -> Option<PathBuf> {
    let target = Path::new("/etc/resolv.conf").canonicalize().ok()?;
    (!in_system(&target)).then_some(target)
}

/// Whether a component between `tree` (excluded) and `point` (included)
/// is a symlink.
fn link_on_the_way(tree: &Path, point: &Path) -> bool {
    let mut at = point.to_path_buf();
    while at != tree {
        if is_link(&at) {
            return true;
        }
        if !at.pop() {
            break;
        }
    }
    false
}

/// Creates `point` (and its missing parents up to `tree`) as directories,
/// recording each one created so the cleanup can remove it.
fn create_owned(tree: &Path, point: &Path, created: &mut Vec<PathBuf>) -> Result<(), String> {
    // A planted link on the way would move the mount, and every directory
    // created below it, outside the tree: checked before anything is made.
    if link_on_the_way(tree, point) {
        return Err(format!("{} is or lies below a symlink", point.display()));
    }
    let mut missing = Vec::new();
    let mut at = point.to_path_buf();
    while at != tree && !at.exists() {
        missing.push(at.clone());
        if !at.pop() {
            break;
        }
    }
    for dir in missing.into_iter().rev() {
        if is_link(&dir) {
            return Err(format!("{} is a symlink", dir.display()));
        }
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        created.push(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link the tree holds on the way to a mount point never makes apb
    /// create a directory outside the tree; the twin is a plain tree, where
    /// the missing directories are created and recorded.
    #[cfg(unix)]
    #[test]
    fn a_mount_point_is_never_created_through_a_link() {
        let t = tempfile::tempdir().unwrap();
        let tree = t.path().join("tree");
        let outside = t.path().join("outside");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join(".claude")).unwrap();
        let mut created = Vec::new();
        assert!(create_owned(&tree, &tree.join(".claude/skills"), &mut created).is_err());
        assert!(created.is_empty());
        assert!(
            !outside.join("skills").exists(),
            "a directory was made outside the tree"
        );

        let mut created = Vec::new();
        create_owned(&tree, &tree.join(".agents/skills"), &mut created).unwrap();
        assert_eq!(created, [tree.join(".agents"), tree.join(".agents/skills")]);
    }
}
