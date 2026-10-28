//! A git working tree with a local bare remote reached over `file://`, for
//! the publish tests. Every git call the tests make themselves runs with
//! `HOME`, `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_NOSYSTEM` pointing away from
//! the operator's config. The engine's own git calls drop every `GIT_*`
//! variable (so neither of the last two reaches them); [`Fixture::isolate`]
//! points `HOME` and `XDG_CONFIG_HOME` at the fixture's home instead (under
//! the env lock), which replaces the global config. The machine's system
//! config (`/etc/gitconfig`) still applies to them, as it does to the
//! operator's own pushes; a URL rewrite there refuses the plan
//! (`url_rewritten`) rather than sending anything elsewhere. Nothing
//! touches the network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::EnvVar;

pub struct Fixture {
    _tmp: tempfile::TempDir,
    /// The working tree (also the project root).
    pub root: PathBuf,
    /// The bare remote `origin` points at.
    pub bare: PathBuf,
    /// The isolated home every git call reads.
    pub home: PathBuf,
}

impl Fixture {
    /// A tree on `main` with one commit, `origin` a bare remote holding it.
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("tree");
        let bare = base.join("remote.git");
        let home = base.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".gitconfig"), "").unwrap();
        let f = Fixture {
            _tmp: tmp,
            root,
            bare,
            home,
        };
        f.git(&f.root, &["init", "-q", "-b", "main"]);
        fs::write(f.root.join(".gitignore"), ".apb/\n").unwrap();
        f.git(&f.root, &["add", ".gitignore"]);
        f.git(&f.root, &["commit", "-q", "-m", "base"]);
        f.git(&base, &["init", "-q", "--bare", "remote.git"]);
        f.git(&f.root, &["remote", "add", "origin", &f.url()]);
        f.git(&f.root, &["push", "-q", "origin", "main"]);
        f
    }

    /// The `file://` URL of the bare remote.
    pub fn url(&self) -> String {
        url_of(&self.bare)
    }

    /// Points the process's `HOME` and `XDG_CONFIG_HOME` at the isolated
    /// home for the life of the guards; the caller holds the env lock.
    pub fn isolate(&self) -> [EnvVar; 2] {
        [
            EnvVar::set("HOME", &self.home),
            EnvVar::set("XDG_CONFIG_HOME", self.home.join(".config")),
        ]
    }

    /// `git <args>` in `dir`, isolated from the operator's config.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.command(dir).args(args).output().expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn command(&self, dir: &Path) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
        cmd
    }

    /// Commits a new file in the tree; returns the commit.
    pub fn commit(&self, name: &str, subject: &str) -> String {
        fs::write(self.root.join(name), subject).unwrap();
        self.git(&self.root, &["add", name]);
        self.git(&self.root, &["commit", "-q", "-m", subject]);
        self.git(&self.root, &["rev-parse", "HEAD"])
    }

    /// What `refs/heads/<branch>` of the bare remote points at.
    pub fn remote_ref(&self, branch: &str) -> Option<String> {
        remote_ref(&self.bare, branch)
    }

    /// Installs an executable hook in the bare remote.
    pub fn remote_hook(&self, name: &str, body: &str) {
        let dir = self.bare.join("hooks");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        super::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
        make_executable(&path);
    }

    /// A second bare repository next to the first.
    pub fn other_bare(&self, name: &str) -> PathBuf {
        let parent = self.bare.parent().unwrap();
        self.git(parent, &["init", "-q", "--bare", name]);
        parent.join(name)
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

/// The `file://` URL of a local repository.
pub fn url_of(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

/// What `refs/heads/<branch>` of the bare repository `bare` points at;
/// `None` only when the ref is missing (any other git failure panics, so a
/// broken helper never passes an "unchanged" assertion).
pub fn remote_ref(bare: &Path, branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["rev-parse", "--verify", "-q"])
        .arg(format!("refs/heads/{branch}"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git runs");
    match out.status.code() {
        Some(0) => Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        Some(1) => None,
        _ => panic!(
            "rev-parse in {} failed: {}",
            bare.display(),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
