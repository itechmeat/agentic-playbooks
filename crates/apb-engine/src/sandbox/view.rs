//! The filesystem an isolated agent sees, as an ordered list of mounts
//! built from facts the caller gathered (pure: no filesystem access here).
//! Later mounts cover earlier ones, so the order is the contract:
//!
//! 1. the system read-only (`/usr`, `/etc`, ... and the resolver file's
//!    real target), a new `/proc` and `/dev`, `/tmp` a tmpfs;
//! 2. the sandbox home at `$HOME`, which hides the operator's home;
//! 3. the extras, parents before children: the read-only paths (PATH
//!    entries, the agent program's directory, the machine's and the node's
//!    extras) and the node's writable ones;
//! 4. the node's tree, read-write, at its own path;
//! 5. git: the common directory read-write (and a linked worktree's own git
//!    directory, so it cannot be swapped), every git directory's `config`,
//!    `hooks/`, `info/` and `config.worktree` and the `.git` files of the
//!    tree and its submodules read-only;
//! 6. the tree's `.apb` covered by a tmpfs, then the attempt's own run
//!    paths bound back;
//! 7. credential locations inside any bound directory (the tree included)
//!    masked, after every mount that could cover them;
//! 8. the agent's own login files;
//! 9. the apb executable read-only, and the skill copies read-only over
//!    the tree's skill directories.
//!
//! The extras come before the tree, so no extra path can cover the tree's
//! git protections or the masked `.apb`; the masks come after the tree, so
//! a tree that holds a credential location never uncovers it. The sandbox
//! home comes before the tree (not after it) so a tree that lives under the
//! home stays visible.

use std::path::{Path, PathBuf};

use apb_core::agents::Access;

/// The system directories every sandbox sees read-only, when present.
pub const SYSTEM_READ_ONLY: [&str; 11] = [
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/nix", "/snap",
];

/// One bwrap mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    /// `--ro-bind-try src dst`
    ReadOnly(PathBuf, PathBuf),
    /// `--bind-try src dst`
    ReadWrite(PathBuf, PathBuf),
    /// `--tmpfs dst`
    Tmpfs(PathBuf),
    /// `--proc dst`
    Proc(PathBuf),
    /// `--dev dst`
    Dev(PathBuf),
    /// `--perms 0700 --dir dst`
    PrivateDir(PathBuf),
    /// `--ro-bind /dev/null dst`: hides a file.
    MaskFile(PathBuf),
}

pub use super::git_guard::GitFacts;

/// Everything the view is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewFacts {
    /// The operator's home, where the sandbox home is bound.
    pub home: PathBuf,
    pub sandbox_home: PathBuf,
    pub tree: PathBuf,
    pub git: Option<GitFacts>,
    /// `<tree>/.apb` when the tree holds the project's `.apb`.
    pub apb_dir: Option<PathBuf>,
    /// The attempt's run paths bound back over the masked `.apb`.
    pub run_paths: Vec<(PathBuf, Access)>,
    /// The resolver file's real target when it is a symlink.
    pub resolv: Option<PathBuf>,
    /// The private runtime directory (`XDG_RUNTIME_DIR`) inside `/tmp`.
    pub runtime_dir: PathBuf,
    pub read_only: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    /// Credential locations to hide, `true` for a directory.
    pub masks: Vec<(PathBuf, bool)>,
    /// The agent's login files: host path, path in the sandbox home, access.
    pub logins: Vec<(PathBuf, PathBuf, Access)>,
    /// The apb executable the sandbox runs as its helper.
    pub helper: Option<PathBuf>,
    /// The attempt's skill copies (a directory of skill directories) and
    /// the tree's skill directories they cover.
    pub skills: Option<PathBuf>,
    pub skill_points: Vec<PathBuf>,
}

/// The read-only paths of the repository.
pub fn git_read_only(git: &GitFacts) -> Vec<PathBuf> {
    let mut out = git.read_only.clone();
    out.extend(git.git_file.iter().cloned());
    out
}

/// The extras in bind order: parents before children, so a writable path
/// inside a read-only one stays writable and the reverse stays read-only.
fn extras(f: &ViewFacts) -> Vec<(&Path, Access)> {
    let mut all: Vec<(&Path, Access)> = f
        .read_only
        .iter()
        .map(|p| (p.as_path(), Access::ReadOnly))
        .chain(f.writable.iter().map(|p| (p.as_path(), Access::ReadWrite)))
        .collect();
    all.sort_by_key(|(p, _)| p.components().count());
    all
}

/// Where the skill copies are mounted in `tree`.
pub fn skill_mount_points(tree: &Path) -> [PathBuf; 2] {
    [tree.join(".agents/skills"), tree.join(".claude/skills")]
}

fn bind(access: Access, src: &Path, dst: &Path) -> Mount {
    match access {
        Access::ReadOnly => Mount::ReadOnly(src.to_path_buf(), dst.to_path_buf()),
        Access::ReadWrite => Mount::ReadWrite(src.to_path_buf(), dst.to_path_buf()),
    }
}

fn same(p: &Path) -> (PathBuf, PathBuf) {
    (p.to_path_buf(), p.to_path_buf())
}

/// The mounts of `f`, in order.
pub fn plan(f: &ViewFacts) -> Vec<Mount> {
    let mut m: Vec<Mount> = SYSTEM_READ_ONLY
        .iter()
        .map(|p| {
            let (s, d) = same(Path::new(p));
            Mount::ReadOnly(s, d)
        })
        .collect();
    m.push(Mount::ReadOnly("/sys".into(), "/sys".into()));
    if let Some(r) = &f.resolv {
        let (s, d) = same(r);
        m.push(Mount::ReadOnly(s, d));
    }
    m.push(Mount::Proc("/proc".into()));
    m.push(Mount::Dev("/dev".into()));
    m.push(Mount::Tmpfs("/tmp".into()));
    m.push(Mount::PrivateDir(f.runtime_dir.clone()));
    // 2. the sandbox home over the operator's
    m.push(bind(Access::ReadWrite, &f.sandbox_home, &f.home));
    // 3. extras
    for (p, access) in extras(f) {
        m.push(bind(access, p, p));
    }
    // 4. the tree
    m.push(bind(Access::ReadWrite, &f.tree, &f.tree));
    // 5. git
    if let Some(g) = &f.git {
        m.push(bind(Access::ReadWrite, &g.common, &g.common));
        if g.git_dir != g.common {
            m.push(bind(Access::ReadWrite, &g.git_dir, &g.git_dir));
        }
        for p in git_read_only(g) {
            m.push(bind(Access::ReadOnly, &p, &p));
        }
    }
    // 6. `.apb` masked, the attempt's run paths bound back
    if let Some(apb) = &f.apb_dir {
        m.push(Mount::Tmpfs(apb.clone()));
    }
    for (p, access) in &f.run_paths {
        m.push(bind(*access, p, p));
    }
    // 7. credential masks inside any bound directory, the tree included:
    // after every tree and extra mount, so none of them covers a mask
    for (p, dir) in &f.masks {
        m.push(if *dir {
            Mount::Tmpfs(p.clone())
        } else {
            Mount::MaskFile(p.clone())
        });
    }
    // 8. the agent's login
    for (src, dst, access) in &f.logins {
        m.push(bind(*access, src, dst));
    }
    // 9. the helper, the skills
    if let Some(h) = &f.helper {
        m.push(bind(Access::ReadOnly, h, h));
    }
    if let Some(s) = &f.skills {
        for dst in &f.skill_points {
            m.push(bind(Access::ReadOnly, s, dst));
        }
    }
    m
}

/// The view as an attempt record shows it: each bound path (`~/...` under
/// the home) and its access, system paths left out, at most `cap` entries.
pub fn shown(mounts: &[Mount], home: &Path, cap: usize) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for mount in mounts {
        let (path, access) = match mount {
            Mount::ReadOnly(_, d) => (d, "read_only"),
            Mount::ReadWrite(_, d) => (d, "read_write"),
            Mount::Tmpfs(d) | Mount::MaskFile(d) => (d, "hidden"),
            Mount::Proc(_) | Mount::Dev(_) | Mount::PrivateDir(_) => continue,
        };
        if SYSTEM_READ_ONLY.iter().any(|s| path == Path::new(s)) || path == Path::new("/sys") {
            continue;
        }
        if out.len() >= cap {
            break;
        }
        out.push((apb_core::isolation::paths::display(path, home), access));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ViewFacts {
        ViewFacts {
            home: "/h".into(),
            sandbox_home: "/p/.apb/runs/r/sandbox/home/claude".into(),
            tree: "/p".into(),
            apb_dir: Some("/p/.apb".into()),
            runtime_dir: "/tmp/apb-runtime".into(),
            ..ViewFacts::default()
        }
    }

    fn index(m: &[Mount], want: &Mount) -> usize {
        m.iter()
            .position(|x| x == want)
            .unwrap_or_else(|| panic!("{want:?} in {m:?}"))
    }

    #[test]
    fn a_plain_directory_hides_the_home_and_masks_apb() {
        let mut f = facts();
        f.run_paths = vec![(
            "/p/.apb/runs/r/sandbox/attempts/a".into(),
            Access::ReadWrite,
        )];
        let m = plan(&f);
        let home = index(&m, &Mount::ReadWrite(f.sandbox_home.clone(), "/h".into()));
        let tree = index(&m, &Mount::ReadWrite("/p".into(), "/p".into()));
        let mask = index(&m, &Mount::Tmpfs("/p/.apb".into()));
        let back = index(
            &m,
            &Mount::ReadWrite(
                "/p/.apb/runs/r/sandbox/attempts/a".into(),
                "/p/.apb/runs/r/sandbox/attempts/a".into(),
            ),
        );
        assert!(home < tree && tree < mask && mask < back);
        assert!(m.contains(&Mount::Tmpfs("/tmp".into())));
        assert!(
            !m.iter()
                .any(|x| matches!(x, Mount::ReadOnly(s, _) if s.starts_with("/p/.git")))
        );
    }

    #[test]
    fn a_git_tree_keeps_its_control_files_read_only() {
        let mut f = facts();
        f.git = Some(GitFacts {
            common: "/p/.git".into(),
            git_dir: "/p/.git".into(),
            read_only: ["config", "hooks", "info"]
                .iter()
                .map(|n| PathBuf::from("/p/.git").join(n))
                .collect(),
            ..GitFacts::default()
        });
        let m = plan(&f);
        let common = index(&m, &Mount::ReadWrite("/p/.git".into(), "/p/.git".into()));
        for p in ["/p/.git/config", "/p/.git/hooks", "/p/.git/info"] {
            assert!(
                index(&m, &Mount::ReadOnly(p.into(), p.into())) > common,
                "{p}"
            );
        }
    }

    #[test]
    fn a_worktree_keeps_its_git_file_and_worktree_config_read_only() {
        let mut f = facts();
        f.tree = "/w".into();
        f.apb_dir = None;
        f.git = Some(GitFacts {
            common: "/p/.git".into(),
            git_dir: "/p/.git/worktrees/w".into(),
            git_file: Some("/w/.git".into()),
            read_only: vec![
                "/p/.git/config".into(),
                "/p/.git/worktrees/w/config.worktree".into(),
            ],
            ..GitFacts::default()
        });
        let m = plan(&f);
        // The worktree's own git directory is a mount point: it cannot be
        // renamed away and replaced.
        let own = index(
            &m,
            &Mount::ReadWrite("/p/.git/worktrees/w".into(), "/p/.git/worktrees/w".into()),
        );
        let conf = index(
            &m,
            &Mount::ReadOnly(
                "/p/.git/worktrees/w/config.worktree".into(),
                "/p/.git/worktrees/w/config.worktree".into(),
            ),
        );
        assert!(own < conf);
        for p in [
            "/w/.git",
            "/p/.git/worktrees/w/config.worktree",
            "/p/.git/config",
        ] {
            assert!(m.contains(&Mount::ReadOnly(p.into(), p.into())), "{p}");
        }
        assert!(
            !m.iter()
                .any(|x| matches!(x, Mount::Tmpfs(p) if p.ends_with(".apb")))
        );
    }

    #[test]
    fn masks_follow_the_binds_and_skills_come_last() {
        let mut f = facts();
        f.read_only = vec!["/h/.cargo/bin".into()];
        f.writable = vec!["/h/.cargo".into()];
        f.masks = vec![
            ("/h/.cargo/credentials.toml".into(), false),
            ("/h/.config/gh".into(), true),
        ];
        f.skills = Some("/box/skills".into());
        f.skill_points = skill_mount_points(&f.tree).to_vec();
        let m = plan(&f);
        // A read-only path inside a writable one stays read-only.
        let ro = index(
            &m,
            &Mount::ReadOnly("/h/.cargo/bin".into(), "/h/.cargo/bin".into()),
        );
        let w = index(
            &m,
            &Mount::ReadWrite("/h/.cargo".into(), "/h/.cargo".into()),
        );
        let mask = index(&m, &Mount::MaskFile("/h/.cargo/credentials.toml".into()));
        assert!(w < mask && w < ro);
        assert!(m.contains(&Mount::Tmpfs("/h/.config/gh".into())));
        let last = &m[m.len() - 2..];
        assert_eq!(
            last,
            [
                Mount::ReadOnly("/box/skills".into(), "/p/.agents/skills".into()),
                Mount::ReadOnly("/box/skills".into(), "/p/.claude/skills".into()),
            ]
        );
        let shown = shown(&m, Path::new("/h"), 64);
        assert!(shown.contains(&("~/.cargo".to_string(), "read_write")));
        assert!(shown.contains(&("~/.config/gh".to_string(), "hidden")));
        assert!(!shown.iter().any(|(p, _)| p == "/usr"));
        assert_eq!(super::shown(&m, Path::new("/h"), 2).len(), 2);
        assert!(super::shown(&m, Path::new("/h"), 0).is_empty());
    }

    /// A tree that holds a credential location (a project at `~/.config`,
    /// also writable as an extra) never covers its mask: masks come after
    /// every tree and extra mount.
    #[test]
    fn a_tree_that_holds_a_credential_location_keeps_it_masked() {
        let mut f = facts();
        f.tree = "/h/.config".into();
        f.apb_dir = None;
        f.writable = vec!["/h/.config".into()];
        f.masks = vec![("/h/.config/gh".into(), true)];
        let m = plan(&f);
        let mask = index(&m, &Mount::Tmpfs("/h/.config/gh".into()));
        let tree = m
            .iter()
            .rposition(|x| x == &Mount::ReadWrite("/h/.config".into(), "/h/.config".into()))
            .unwrap();
        assert!(tree < mask, "the tree mount comes before the mask");
    }

    /// An extra that contains the tree is bound before it, so the tree's
    /// git protections and the masked `.apb` still cover it.
    #[test]
    fn an_extra_around_the_tree_never_covers_its_protections() {
        let mut f = facts();
        f.writable = vec!["/p".into()];
        f.read_only = vec!["/x".into()];
        f.git = Some(GitFacts {
            common: "/p/.git".into(),
            git_dir: "/p/.git".into(),
            read_only: vec!["/p/.git/config".into()],
            ..GitFacts::default()
        });
        let m = plan(&f);
        let around = index(&m, &Mount::ReadWrite("/p".into(), "/p".into()));
        let config = index(
            &m,
            &Mount::ReadOnly("/p/.git/config".into(), "/p/.git/config".into()),
        );
        let apb = index(&m, &Mount::Tmpfs("/p/.apb".into()));
        assert!(around < config && around < apb);
        let last_extra = m
            .iter()
            .rposition(|x| x == &Mount::ReadWrite("/p".into(), "/p".into()))
            .unwrap();
        assert!(
            last_extra > around,
            "the tree itself is bound after the extras"
        );
    }
}
