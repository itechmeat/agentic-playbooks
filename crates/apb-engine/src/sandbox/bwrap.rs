//! The bwrap command line of one sandboxed process (pure). bwrap runs the
//! helper `apb __sandbox-exec`, which applies the Landlock scope and then
//! execs the program.
//!
//! Namespaces: user, PID, IPC, UTS, cgroup (when the kernel has one), and
//! the network under `network: none`. No capabilities, the sandbox dies
//! with apb, and with bwrap 0.8 or newer it cannot create a user namespace
//! of its own. No `--new-session`: apb calls `setsid()` before exec, so the
//! sandbox has no controlling terminal while its process group still equals
//! its pid (the group kill and the graceful stop keep working).

use std::ffi::{OsStr, OsString};
use std::path::Path;

use apb_core::isolation::Network;

use super::view::Mount;

/// What the command line is built from.
pub struct Line<'a> {
    pub network: Network,
    /// bwrap 0.8 or newer: `--disable-userns`.
    pub disable_userns: bool,
    pub mounts: &'a [Mount],
    pub cwd: &'a Path,
    /// The apb executable, run inside as `__sandbox-exec`.
    pub helper: &'a Path,
    /// Ask the helper for the Landlock scope, failing closed without it.
    pub scope: bool,
}

/// The argv after the bwrap program itself.
pub fn argv(plan: &Line, program: &OsStr, args: &[OsString]) -> Vec<OsString> {
    let mut a: Vec<OsString> = [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if plan.network == Network::None {
        a.push("--unshare-net".into());
    }
    a.extend(["--die-with-parent", "--cap-drop", "ALL"].map(OsString::from));
    if plan.disable_userns {
        a.push("--disable-userns".into());
    }
    for m in plan.mounts {
        push_mount(&mut a, m);
    }
    a.push("--chdir".into());
    a.push(plan.cwd.into());
    a.push("--".into());
    a.push(plan.helper.into());
    a.push("__sandbox-exec".into());
    if plan.scope {
        a.push("--scope".into());
        a.push("--require-scope".into());
    }
    a.push("--".into());
    a.push(program.to_os_string());
    a.extend(args.iter().cloned());
    a
}

fn push_mount(a: &mut Vec<OsString>, m: &Mount) {
    let mut two = |flag: &str, s: &std::path::Path, d: &std::path::Path| {
        a.push(flag.into());
        a.push(s.into());
        a.push(d.into());
    };
    match m {
        Mount::ReadOnly(s, d) => two("--ro-bind-try", s, d),
        Mount::ReadWrite(s, d) => two("--bind-try", s, d),
        Mount::MaskFile(d) => two("--ro-bind", std::path::Path::new("/dev/null"), d),
        Mount::Tmpfs(d) => {
            a.push("--tmpfs".into());
            a.push(d.into());
        }
        Mount::Proc(d) => {
            a.push("--proc".into());
            a.push(d.into());
        }
        Mount::Dev(d) => {
            a.push("--dev".into());
            a.push(d.into());
        }
        Mount::PrivateDir(d) => {
            a.extend(["--perms", "0700", "--dir"].map(OsString::from));
            a.push(d.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &[Mount] = &[];

    fn plan(network: Network, scope: bool) -> Line<'static> {
        Line {
            network,
            disable_userns: true,
            mounts: MOUNTS,
            cwd: Path::new("/p"),
            helper: Path::new("/opt/apb"),
            scope,
        }
    }

    fn strings(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_network_flag_follows_the_spec() {
        let none = strings(&argv(&plan(Network::None, true), OsStr::new("x"), &[]));
        assert!(none.contains(&"--unshare-net".to_string()));
        let open = strings(&argv(&plan(Network::Open, true), OsStr::new("x"), &[]));
        assert!(!open.contains(&"--unshare-net".to_string()));
        for flag in [
            "--unshare-user",
            "--unshare-pid",
            "--die-with-parent",
            "--disable-userns",
        ] {
            assert!(open.contains(&flag.to_string()), "{flag}");
        }
        assert!(!open.contains(&"--new-session".to_string()));
    }

    #[test]
    fn the_scope_is_asked_only_when_the_kernel_offers_it() {
        let a = strings(&argv(
            &plan(Network::Open, true),
            OsStr::new("agent"),
            &["-p".into()],
        ));
        let tail: Vec<&str> = a[a.iter().position(|s| s == "/opt/apb").unwrap()..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "/opt/apb",
                "__sandbox-exec",
                "--scope",
                "--require-scope",
                "--",
                "agent",
                "-p"
            ]
        );
        let b = strings(&argv(&plan(Network::None, false), OsStr::new("agent"), &[]));
        assert!(!b.contains(&"--scope".to_string()));
    }
}
