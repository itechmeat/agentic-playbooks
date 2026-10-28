//! The paths an isolated node may name (`read_only`, `writable`, the
//! machine's `isolation.read_only`), the credential locations no path may
//! reach, the places a writable path must not make writable, and the `~/`
//! form every surface shows a path in.
//!
//! The validator checks the paths as written ([`check_extra`],
//! [`check_writable`]); the engine checks them again on the host after
//! resolving symlinks, with the real home ([`check_resolved`]), so an
//! absolute path into the home or a link into a credential location is
//! refused too.

use std::path::{Component, Path, PathBuf};

/// Locations that hold credentials or the operator's live session. An extra
/// path equal to one of these, or inside one, is refused (V114); one inside
/// a bound home subtree is masked in the sandbox view.
pub const CREDENTIAL_LOCATIONS: &[&str] = &[
    "~/.ssh",
    "~/.gnupg",
    "~/.aws",
    "~/.azure",
    "~/.config/gcloud",
    "~/.config/gh",
    "~/.config/hub",
    "~/.config/apb",
    "~/.config/git/credentials",
    "~/.docker",
    "~/.kube",
    "~/.netrc",
    "~/.git-credentials",
    "~/.npmrc",
    "~/.pypirc",
    "~/.cargo/credentials",
    "~/.cargo/credentials.toml",
    "~/.password-store",
    "~/.local/share/keyrings",
    "~/.pki",
    "~/.claude",
    "~/.claude.json",
    "~/.codex",
    "~/.local/share/opencode",
    "/run",
    "/var/run",
    "/tmp",
    "/var/tmp",
    "/proc",
    "/dev",
    "/root",
];

/// Places a program the operator later runs outside the sandbox is read
/// from: shell startup files, autostart and user units, the user's own
/// program directories and the operator's git config. A `writable` path
/// equal to one of these, inside one or above one is refused (V114): one
/// that does not exist yet could be created through a writable parent.
pub const PERSISTENCE_LOCATIONS: &[&str] = &[
    "~/.bashrc",
    "~/.bash_profile",
    "~/.bash_login",
    "~/.bash_logout",
    "~/.profile",
    "~/.zshrc",
    "~/.zshenv",
    "~/.zprofile",
    "~/.zlogin",
    "~/.pam_environment",
    "~/.xprofile",
    "~/.xsessionrc",
    "~/.config/fish",
    "~/.config/autostart",
    "~/.config/systemd",
    "~/.config/environment.d",
    "~/.local/bin",
    "~/.local/share/applications",
    "~/.cargo/bin",
    "~/.gitconfig",
    "~/.config/git",
];

/// Why an extra path is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    /// Neither `~/...` nor absolute.
    Relative,
    /// A `..` component.
    Parent,
    /// `/` or `~` itself.
    Root,
    /// Equal to or inside the named credential location.
    Credential(&'static str),
    /// A writable path equal to or inside the named persistence location.
    Persistence(&'static str),
    /// The resolved path is the home or one of its parents.
    Home,
}

impl std::fmt::Display for PathProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathProblem::Relative => f.write_str("must start with `~/` or `/`"),
            PathProblem::Parent => f.write_str("must not contain `..`"),
            PathProblem::Root => f.write_str("must name a directory below `/` or `~`"),
            PathProblem::Credential(loc) => write!(f, "reaches the credential location `{loc}`"),
            PathProblem::Persistence(loc) => write!(
                f,
                "would make `{loc}` writable, which the operator runs outside the sandbox"
            ),
            PathProblem::Home => f.write_str("resolves to the home or a directory above it"),
        }
    }
}

/// The path split into its home-relative or absolute normal components:
/// `(true, ["a", "b"])` for `~/a/b`, `(false, ["a"])` for `/a`.
fn split(p: &str) -> Option<(bool, Vec<&str>)> {
    let (home, rest) = if p == "~" {
        (true, "")
    } else if let Some(r) = p.strip_prefix("~/") {
        (true, r)
    } else if p.starts_with('/') {
        (false, p)
    } else {
        return None;
    };
    Some((
        home,
        rest.split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect(),
    ))
}

/// Checks one extra path an author or the machine names.
pub fn check_extra(p: &str) -> Result<(), PathProblem> {
    let (home, parts) = split(p.trim()).ok_or(PathProblem::Relative)?;
    if parts.contains(&"..") {
        return Err(PathProblem::Parent);
    }
    if parts.is_empty() {
        return Err(PathProblem::Root);
    }
    within(home, &parts, CREDENTIAL_LOCATIONS)
        .map_or(Ok(()), |loc| Err(PathProblem::Credential(loc)))
}

/// Checks one `writable` path: [`check_extra`], and no persistence location
/// equal to it, around it or inside it.
pub fn check_writable(p: &str) -> Result<(), PathProblem> {
    check_extra(p)?;
    let (home, parts) = split(p.trim()).ok_or(PathProblem::Relative)?;
    let found = PERSISTENCE_LOCATIONS.iter().copied().find(|loc| {
        let (lhome, lparts) = split(loc).expect("a listed location parses");
        lhome == home && (parts.starts_with(&lparts) || lparts.starts_with(&parts))
    });
    found.map_or(Ok(()), |loc| Err(PathProblem::Persistence(loc)))
}

/// The location of `list` the split path equals or lies inside.
fn within(home: bool, parts: &[&str], list: &[&'static str]) -> Option<&'static str> {
    list.iter().copied().find(|loc| {
        let (lhome, lparts) = split(loc).expect("a listed location parses");
        lhome == home && parts.starts_with(&lparts)
    })
}

/// What a resolved host path is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// A `read_only` extra.
    ReadOnly,
    /// A `writable` extra.
    Writable,
    /// The node's tree: read-write by nature, and masked where it holds a
    /// credential location.
    Tree,
}

/// Checks a host path the engine is about to bind, already resolved
/// (`canonicalize`), against the real home (canonical too), each location
/// also taken resolved (for a symlinked `~/.config`):
///
/// - never the home nor a parent of it;
/// - an extra never equal to or inside a credential location, the tree
///   never inside one (it may hold one: the view masks it); an absolute
///   location (`/run`, `/tmp`) counts only for a path outside the home, and
///   the temporary directories never for the tree;
/// - a writable extra and the tree never equal to, inside or above a
///   persistence location.
pub fn check_resolved(path: &Path, home: &Path, purpose: Use) -> Result<(), PathProblem> {
    if home.starts_with(path) {
        return Err(PathProblem::Home);
    }
    let resolved = |loc: &str| {
        let l = expand(loc, home);
        let c = resolve_loose(&l);
        std::iter::once(l).chain(c).collect::<Vec<_>>()
    };
    let in_home = path.starts_with(home);
    let credential = CREDENTIAL_LOCATIONS.iter().copied().find(|loc| {
        let absolute = !loc.starts_with('~');
        if absolute && in_home {
            return false;
        }
        // A tree may live below a temporary directory, never be one: the
        // whole host `/tmp` holds other sessions' sockets.
        if absolute && purpose == Use::Tree && TMP.contains(loc) {
            return path == Path::new(loc);
        }
        resolved(loc).iter().any(|l| path.starts_with(l))
    });
    if let Some(loc) = credential {
        return Err(PathProblem::Credential(loc));
    }
    if purpose != Use::ReadOnly {
        let persistence = PERSISTENCE_LOCATIONS.iter().copied().find(|loc| {
            resolved(loc)
                .iter()
                .any(|l| path.starts_with(l) || l.starts_with(path))
        });
        if let Some(loc) = persistence {
            return Err(PathProblem::Persistence(loc));
        }
    }
    Ok(())
}

/// The temporary directories among the credential locations (for their
/// sockets): a tree may live there.
const TMP: [&str; 2] = ["/tmp", "/var/tmp"];

/// `path` with its longest existing ancestor resolved (`canonicalize`) and
/// the rest appended: where a location that does not exist yet would be
/// created, also behind a symlinked parent.
pub fn resolve_loose(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut at = path.to_path_buf();
    loop {
        if let Ok(c) = at.canonicalize() {
            return Some(
                rest.iter()
                    .rev()
                    .fold(c, |a, s: &std::ffi::OsString| a.join(s)),
            );
        }
        rest.push(at.file_name()?.to_os_string());
        if !at.pop() {
            return None;
        }
    }
}

/// The host path of a `~/` or absolute path, `home` standing for `~`.
pub fn expand(p: &str, home: &Path) -> PathBuf {
    match split(p.trim()) {
        Some((true, parts)) => parts.iter().fold(home.to_path_buf(), |a, s| a.join(s)),
        _ => PathBuf::from(p.trim()),
    }
}

/// The host paths of `list` strictly inside `dir`, each also taken resolved
/// (a location behind a symlinked parent is found at its real place): what
/// a bind of `dir` must cover again.
pub fn inside(dir: &Path, home: &Path, list: &[&str]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for loc in list {
        let l = expand(loc, home);
        let resolved = resolve_loose(&l);
        for p in std::iter::once(l).chain(resolved) {
            if p != dir && p.starts_with(dir) && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The credential locations inside `dir`: what a bind of `dir` must mask.
pub fn credentials_inside(dir: &Path, home: &Path) -> Vec<PathBuf> {
    inside(dir, home, CREDENTIAL_LOCATIONS)
}

/// `path` as every surface shows it: `~/...` under `home`, else as is.
pub fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Whether `path` has no `..` and is absolute (a path the engine composed
/// itself and is about to bind).
pub fn is_plain_absolute(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_paths_are_checked() {
        assert_eq!(check_extra("~/.rustup"), Ok(()));
        assert_eq!(check_extra("/opt/sdk"), Ok(()));
        assert_eq!(check_extra("rel/x"), Err(PathProblem::Relative));
        assert_eq!(check_extra("~/a/../.ssh"), Err(PathProblem::Parent));
        assert_eq!(check_extra("~"), Err(PathProblem::Root));
        assert_eq!(check_extra("/"), Err(PathProblem::Root));
        assert_eq!(
            check_extra("~/.ssh/id"),
            Err(PathProblem::Credential("~/.ssh"))
        );
        assert_eq!(
            check_extra("/run/user/1000"),
            Err(PathProblem::Credential("/run"))
        );
        // A parent of a location is allowed; the view masks the location.
        assert_eq!(check_extra("~/.config"), Ok(()));
        assert_eq!(
            check_extra("/tmp/.X11-unix"),
            Err(PathProblem::Credential("/tmp"))
        );
        assert_eq!(
            check_extra("~/.local/share/opencode/auth.json"),
            Err(PathProblem::Credential("~/.local/share/opencode"))
        );
    }

    #[test]
    fn a_writable_path_never_reaches_what_the_operator_runs() {
        assert_eq!(check_writable("~/.cargo/registry"), Ok(()));
        assert_eq!(check_extra("~/.cargo/bin"), Ok(()));
        assert_eq!(
            check_writable("~/.cargo/bin"),
            Err(PathProblem::Persistence("~/.cargo/bin"))
        );
        assert_eq!(
            check_writable("~/.config/systemd/user"),
            Err(PathProblem::Persistence("~/.config/systemd"))
        );
        assert_eq!(
            check_writable("~/.ssh"),
            Err(PathProblem::Credential("~/.ssh"))
        );
        // A parent of a persistence location, which the agent could create.
        assert_eq!(
            check_writable("~/.local"),
            Err(PathProblem::Persistence("~/.local/bin"))
        );
        assert_eq!(
            check_writable("~/.cargo"),
            Err(PathProblem::Persistence("~/.cargo/bin"))
        );
        assert_eq!(check_writable("~/.cache/x"), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn a_resolved_path_is_checked_against_the_real_home() {
        let home = Path::new("/h/u");
        let (ro, rw, tree) = (Use::ReadOnly, Use::Writable, Use::Tree);
        assert_eq!(check_resolved(&home.join("tools"), home, rw), Ok(()));
        assert_eq!(
            check_resolved(&home.join(".ssh"), home, ro),
            Err(PathProblem::Credential("~/.ssh"))
        );
        assert_eq!(
            check_resolved(&home.join(".aws/config"), home, ro),
            Err(PathProblem::Credential("~/.aws"))
        );
        assert_eq!(check_resolved(home, home, ro), Err(PathProblem::Home));
        assert_eq!(
            check_resolved(Path::new("/h"), home, tree),
            Err(PathProblem::Home)
        );
        assert_eq!(
            check_resolved(Path::new("/tmp/.X11-unix"), home, ro),
            Err(PathProblem::Credential("/tmp"))
        );
        assert_eq!(
            check_resolved(&home.join(".local/bin"), home, rw),
            Err(PathProblem::Persistence("~/.local/bin"))
        );
        // A parent of a persistence location that does not exist yet: the
        // agent could create it through the writable parent.
        assert_eq!(
            check_resolved(&home.join(".config"), home, rw),
            Err(PathProblem::Persistence("~/.config/fish"))
        );
        assert_eq!(check_resolved(&home.join(".local/bin"), home, ro), Ok(()));
        // The tree may hold a credential location (masked) but not sit in
        // one, and may live in a temporary directory.
        assert_eq!(
            check_resolved(&home.join(".aws/proj"), home, tree),
            Err(PathProblem::Credential("~/.aws"))
        );
        assert_eq!(check_resolved(Path::new("/tmp/proj"), home, tree), Ok(()));
        for t in ["/tmp", "/var/tmp"] {
            assert!(
                matches!(
                    check_resolved(Path::new(t), home, tree),
                    Err(PathProblem::Credential(_))
                ),
                "{t}"
            );
        }
        assert_eq!(
            check_resolved(&home.join(".config"), home, tree),
            Err(PathProblem::Persistence("~/.config/fish"))
        );
        // A home under `/tmp` (CI, a test) does not make every path under it
        // a credential location.
        let tmp_home = Path::new("/tmp/h");
        assert_eq!(
            check_resolved(&tmp_home.join("tools"), tmp_home, ro),
            Ok(())
        );
        assert_eq!(
            check_resolved(&tmp_home.join(".ssh"), tmp_home, ro),
            Err(PathProblem::Credential("~/.ssh"))
        );
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonicalize().unwrap();
        // A location behind a symlinked parent is found at its real place.
        std::fs::create_dir_all(home.join("real/gh")).unwrap();
        std::os::unix::fs::symlink(home.join("real"), home.join(".config")).unwrap();
        assert_eq!(
            check_resolved(&home.join("real/gh"), &home, ro),
            Err(PathProblem::Credential("~/.config/gh"))
        );
        assert!(credentials_inside(&home.join("real"), &home).contains(&home.join("real/gh")));
    }

    #[test]
    fn paths_expand_and_display_under_home() {
        let home = Path::new("/h/u");
        assert_eq!(
            expand("~/.cargo/registry", home),
            home.join(".cargo/registry")
        );
        assert_eq!(expand("/opt/x", home), PathBuf::from("/opt/x"));
        assert_eq!(display(&home.join(".rustup"), home), "~/.rustup");
        assert_eq!(display(Path::new("/opt"), home), "/opt");
        let inside = credentials_inside(&home.join(".cargo"), home);
        assert!(inside.contains(&home.join(".cargo/credentials.toml")));
        assert!(!inside.contains(&home.join(".ssh")));
    }
}
