//! The sandbox home: one directory per run, agent and node
//! (`sandbox/home/<agent>/<node>`), bound at `$HOME` so the operator's home
//! stays hidden. It holds a generated `.gitconfig` with only the operator's
//! `user.name` and `user.email`, the filtered copies the agent's module
//! asks for, and the mount points of its login files.
//!
//! The directory is the agent's to change between attempts, so everything
//! apb writes there refuses to follow a link: a link the agent left where
//! apb writes is removed (the link, never its target) and the path written
//! afresh.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use apb_core::agents::{Access, SandboxNeeds};

/// The sandbox home of `agent` (a canonical id) for `node` in `run_dir`.
/// Parallel nodes of one agent never share it.
pub fn dir(run_dir: &Path, agent: &str, node: &str) -> PathBuf {
    run_dir.join("sandbox").join("home").join(agent).join(node)
}

/// Lays the sandbox home down and returns the agent's login binds: host
/// path, path inside the sandbox, access. `login_from_env`: a login
/// variable is passed, so no login file is bound.
pub fn prepare(
    sandbox_home: &Path,
    home: &Path,
    needs: &SandboxNeeds,
    login_from_env: bool,
    identity: &[(&str, String)],
) -> std::io::Result<Vec<(PathBuf, PathBuf, Access)>> {
    create_private_dir(sandbox_home)?;
    write_gitconfig(sandbox_home, identity)?;
    for copy in &needs.copies {
        let (src, dst) = (home.join(copy.home_rel), sandbox_home.join(copy.home_rel));
        if !src.is_file() || std::fs::symlink_metadata(&dst).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let bytes = std::fs::read(&src)?;
        private_parents(sandbox_home, &dst)?;
        create_file(&dst, &(copy.sanitize)(&bytes))?;
    }
    let mut logins = Vec::new();
    if login_from_env {
        return Ok(logins);
    }
    for b in &needs.binds {
        let src = home.join(b.home_rel);
        let Ok(meta) = std::fs::metadata(&src) else {
            continue;
        };
        // The mount point inside the sandbox home, so bwrap never has to
        // create it.
        let point = sandbox_home.join(b.home_rel);
        private_parents(sandbox_home, &point)?;
        if meta.is_dir() {
            private_dir_in(sandbox_home, &point)?;
        } else if !std::fs::symlink_metadata(&point).is_ok_and(|m| m.is_file()) {
            create_file(&point, b"")?;
        }
        logins.push((src, home.join(b.home_rel), b.access));
    }
    Ok(logins)
}

/// Creates `dir` (owned by apb, outside the agent's reach) private.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    set_private(dir)
}

fn set_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Makes every directory between `root` and `path` (both excluded) a real,
/// private directory inside `root`.
fn private_parents(root: &Path, path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(parent) if parent != root => private_dir_in(root, parent),
        _ => Ok(()),
    }
}

/// Makes `dir` and every directory between `root` and it real, private
/// directories: a link (or a file) on the way is removed first.
fn private_dir_in(root: &Path, dir: &Path) -> std::io::Result<()> {
    let rel = dir
        .strip_prefix(root)
        .map_err(|_| std::io::Error::other("a path outside the sandbox home"))?;
    let mut at = root.to_path_buf();
    for c in rel.components() {
        let Component::Normal(name) = c else {
            return Err(std::io::Error::other(
                "a path with `..` in the sandbox home",
            ));
        };
        at.push(name);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                std::fs::remove_file(&at)?;
                std::fs::create_dir(&at)?;
            }
            Err(_) => std::fs::create_dir(&at)?,
        }
        set_private(&at)?;
    }
    Ok(())
}

/// Writes a new file at `path`: whatever is there (a link the agent left
/// included) is removed first, and the create never follows a link.
fn create_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(_) => {}
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)
}

/// The operator's `user.name` and `user.email` from their global git
/// config, read with the hardened git.
pub fn operator_identity() -> Vec<(&'static str, String)> {
    ["user.name", "user.email"]
        .into_iter()
        .filter_map(|key| {
            let out = apb_core::git::base()
                .args(["config", "--global", "--get", key])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())?;
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!v.is_empty()).then_some((key, v))
        })
        .collect()
}

/// The sandbox's git config: only the identity. The operator's own config
/// (credential helpers, url rewrites, aliases) never enters.
fn write_gitconfig(sandbox_home: &Path, identity: &[(&str, String)]) -> std::io::Result<()> {
    let file = sandbox_home.join(".gitconfig");
    create_file(&file, b"")?;
    for (key, v) in identity {
        let ok = apb_core::git::base()
            .arg("config")
            .arg("--file")
            .arg(&file)
            .args([key, v.as_str()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err(std::io::Error::other(format!(
                "cannot write {key} into the sandbox git config"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::agents::HomeBind;

    const TOKEN: &str = "fixture-token-0123456789";

    fn claude_needs() -> SandboxNeeds {
        apb_core::agents::lookup("claude")
            .and_then(|a| a.setup().sandbox_needs())
            .expect("claude declares its needs")
    }

    #[test]
    fn logins_are_bound_unless_the_env_carries_one_and_copies_are_made_once() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
        std::fs::write(home.join(".claude.json"), "{\"userID\":\"u\"}").unwrap();
        let mut needs = claude_needs();
        needs.binds.push(HomeBind {
            home_rel: ".missing",
            access: Access::ReadOnly,
        });
        let sb = t.path().join("sb");
        let who = [("user.name", "T".to_string())];
        let logins = prepare(&sb, &home, &needs, false, &who).unwrap();
        assert_eq!(logins.len(), 1);
        assert_eq!(logins[0].1, home.join(".claude/.credentials.json"));
        assert!(sb.join(".claude/.credentials.json").is_file());
        assert!(
            std::fs::read_to_string(sb.join(".claude.json"))
                .unwrap()
                .contains("\"userID\": \"u\"")
        );
        // The agent's edit of its copy survives the next attempt.
        std::fs::write(sb.join(".claude.json"), "edited").unwrap();
        assert!(prepare(&sb, &home, &needs, true, &who).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(sb.join(".claude.json")).unwrap(),
            "edited"
        );
        let cfg = std::fs::read_to_string(sb.join(".gitconfig")).unwrap();
        assert!(cfg.contains("name = T") && !cfg.contains("email"));
    }

    /// Another service's token in the operator's `~/.claude.json` (an MCP
    /// server's `env` or `headers`) never reaches the sandbox home.
    #[test]
    fn a_token_of_another_service_never_reaches_the_sandbox_home() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            format!(
                r#"{{"hasCompletedOnboarding":true,"mcpServers":{{"gh":{{"env":{{"GITHUB_TOKEN":"{TOKEN}"}}}},"web":{{"headers":{{"Authorization":"Bearer {TOKEN}"}}}}}}}}"#
            ),
        )
        .unwrap();
        let sb = t.path().join("sb");
        prepare(&sb, &home, &claude_needs(), true, &[]).unwrap();
        let copy = std::fs::read_to_string(sb.join(".claude.json")).unwrap();
        assert!(copy.contains("hasCompletedOnboarding"));
        assert!(
            !copy.contains(TOKEN),
            "a fixture token reached the sandbox home"
        );
        assert!(!copy.contains("mcpServers"));
    }

    /// A link the agent left in its home never makes apb write or chmod
    /// outside it; the twin is the home as apb lays it down.
    #[cfg(unix)]
    #[test]
    fn links_the_agent_left_are_never_followed() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        let outside = t.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sb = t.path().join("sb");
        std::fs::create_dir_all(&sb).unwrap();
        symlink(outside.join("gitconfig"), sb.join(".gitconfig")).unwrap();
        symlink(outside.join("state"), sb.join(".claude.json")).unwrap();
        symlink(&outside, sb.join(".claude")).unwrap();
        prepare(&sb, &home, &claude_needs(), false, &[]).unwrap();
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "nothing written outside"
        );
        let mode = std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the outside directory kept its mode");
        for p in [".gitconfig", ".claude.json", ".claude/.credentials.json"] {
            let m = std::fs::symlink_metadata(sb.join(p)).unwrap();
            assert!(m.is_file(), "{p} is a real file");
        }
        assert!(
            std::fs::symlink_metadata(sb.join(".claude"))
                .unwrap()
                .is_dir()
        );
    }
}
