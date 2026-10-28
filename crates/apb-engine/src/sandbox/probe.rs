//! What this machine offers the sandbox: bubblewrap (where, which version,
//! not setuid, whether one trial sandbox starts) and the Landlock ABI. The
//! expensive part is probed once per process; `APB_SANDBOX=off` is read on
//! every call and can only lower the result.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use apb_core::isolation::contract::{BwrapFacts, HostFacts, Os};

/// The variable that switches the sandbox off: with `off`, `best_effort`
/// runs without a sandbox and `full` refuses.
pub const SANDBOX_ENV: &str = "APB_SANDBOX";

/// How long the trial sandbox may take.
const TRIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// The host's facts, `disabled` read now.
pub fn host_facts() -> HostFacts {
    static PROBED: OnceLock<HostFacts> = OnceLock::new();
    let mut facts = PROBED.get_or_init(probe).clone();
    facts.disabled = disabled();
    facts
}

/// Whether `APB_SANDBOX=off` is set.
pub fn disabled() -> bool {
    std::env::var(SANDBOX_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// The doctor's `sandbox` line: whether `full` can run here, and the
/// detail (bwrap path and version, whether user namespaces work or the
/// trial's error, the Landlock ABI and whether the scope is available).
pub fn doctor_line(h: &HostFacts) -> (bool, String) {
    if h.os != Os::Linux {
        return (
            false,
            "not supported on this system: isolation full refuses, best_effort runs without a sandbox".into(),
        );
    }
    let scope = if h.scope_available() {
        "the scope is available"
    } else {
        "no scope, so network: open under full refuses"
    };
    let landlock = format!("Landlock ABI {} ({scope})", h.landlock_abi);
    let (ok, bwrap) = match &h.bwrap {
        None => match apb_core::config::GlobalConfig::load() {
            Err(e) => (
                false,
                format!("no bwrap: the global config cannot be read ({e})"),
            ),
            Ok(_) => (false, "no bwrap: install bubblewrap".to_string()),
        },
        Some(b) if b.setuid => (
            false,
            format!(
                "{} is setuid, apb uses unprivileged bwrap only",
                b.path.display()
            ),
        ),
        Some(b) => {
            let v = b.version.as_deref().unwrap_or("unknown version");
            match &b.trial {
                Ok(()) => (
                    true,
                    format!("bwrap {v} at {}, user namespaces work", b.path.display()),
                ),
                Err(e) => (
                    false,
                    format!(
                        "bwrap {v} at {}, user namespaces fail: {e}",
                        b.path.display()
                    ),
                ),
            }
        }
    };
    let off = if h.disabled { "; APB_SANDBOX=off" } else { "" };
    (ok && !h.disabled, format!("{bwrap}; {landlock}{off}"))
}

fn probe() -> HostFacts {
    let os = Os::current();
    let bwrap = if os == Os::Linux {
        find_bwrap().map(|path| inspect(&path))
    } else {
        None
    };
    HostFacts {
        os,
        bwrap,
        landlock_abi: super::scope::abi(),
        disabled: false,
    }
}

/// The global config's `isolation.bwrap`, then `/usr/bin/bwrap`, then the
/// first `bwrap` in an absolute PATH entry. A global config that cannot be
/// read finds none: its `isolation.bwrap` may name the only bwrap the
/// operator trusts ([`doctor_line`] says why).
fn find_bwrap() -> Option<PathBuf> {
    let configured = apb_core::config::GlobalConfig::load()
        .ok()?
        .isolation
        .bwrap
        .map(PathBuf::from);
    if let Some(p) = configured {
        return p.is_file().then_some(p);
    }
    let usr = PathBuf::from("/usr/bin/bwrap");
    if usr.is_file() {
        return Some(usr);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|d| d.is_absolute())
        .map(|d| d.join("bwrap"))
        .find(|p| p.is_file())
}

/// The probed bwrap checked again right before a spawn: it still exists
/// and has not become setuid since the probe of this process.
pub fn recheck(path: &Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && !is_setuid(path) => Ok(()),
        Ok(m) if m.is_file() => Err(format!(
            "{} became setuid; apb uses unprivileged bwrap only",
            path.display()
        )),
        Ok(_) => Err(format!("{} is not a file", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn inspect(path: &Path) -> BwrapFacts {
    BwrapFacts {
        path: path.to_path_buf(),
        version: version(path),
        setuid: is_setuid(path),
        trial: trial(path),
    }
}

#[cfg(unix)]
fn is_setuid(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.mode() & 0o4000 != 0)
}

#[cfg(not(unix))]
fn is_setuid(_path: &Path) -> bool {
    false
}

/// `bwrap --version` without the program name (`0.11.1`).
fn version(path: &Path) -> Option<String> {
    let out = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().last()?.to_string();
    (!v.is_empty()).then_some(v)
}

/// Whether `version` is at least `major.minor`.
pub(crate) fn at_least(version: Option<&str>, major: u32, minor: u32) -> bool {
    let Some(v) = version else { return false };
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (a, b) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (a, b) >= (major, minor)
}

/// One trial sandbox: every namespace, the root read-only, `true`. Its
/// stderr is the reason when it fails.
fn trial(path: &Path) -> Result<(), String> {
    use std::process::Stdio;
    let mut child = std::process::Command::new(path)
        .args(["--unshare-all", "--ro-bind", "/", "/", "--", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run bwrap: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(_)) => {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read as _;
                    let _ = s.read_to_string(&mut err);
                }
                let err = err.trim();
                return Err(if err.is_empty() {
                    "the trial sandbox failed".to_string()
                } else {
                    err.chars().take(300).collect()
                });
            }
            Ok(None) if started.elapsed() >= TRIAL_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the trial sandbox did not finish in time".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("cannot wait for bwrap: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doctor_line_names_what_full_needs() {
        let mut h = HostFacts {
            os: Os::Linux,
            bwrap: Some(BwrapFacts {
                path: "/usr/bin/bwrap".into(),
                version: Some("0.11.1".into()),
                setuid: false,
                trial: Ok(()),
            }),
            landlock_abi: 6,
            disabled: false,
        };
        let (ok, line) = doctor_line(&h);
        assert!(ok);
        assert_eq!(
            line,
            "bwrap 0.11.1 at /usr/bin/bwrap, user namespaces work; Landlock ABI 6 (the scope is available)"
        );
        h.bwrap.as_mut().unwrap().trial = Err("setting up uid map: Permission denied".into());
        let (ok, line) = doctor_line(&h);
        assert!(!ok && line.contains("user namespaces fail: setting up uid map"));
        h.bwrap = None;
        assert!(doctor_line(&h).1.starts_with("no bwrap"));
        h.os = Os::MacOs;
        assert!(doctor_line(&h).1.starts_with("not supported"));
    }

    /// The bwrap a spawn uses is checked again: gone or setuid refuses;
    /// the twin is a plain executable.
    #[cfg(unix)]
    #[test]
    fn a_bwrap_that_changed_since_the_probe_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let t = tempfile::tempdir().unwrap();
        let b = t.path().join("bwrap");
        std::fs::write(&b, "").unwrap();
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(recheck(&b), Ok(()));
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o4755)).unwrap();
        assert!(recheck(&b).unwrap_err().contains("setuid"));
        std::fs::remove_file(&b).unwrap();
        assert!(recheck(&b).is_err());
    }

    #[test]
    fn versions_compare_by_major_and_minor() {
        assert!(at_least(Some("0.11.1"), 0, 8));
        assert!(at_least(Some("0.8.0"), 0, 8));
        assert!(!at_least(Some("0.7.9"), 0, 8));
        assert!(!at_least(None, 0, 8));
    }
}
