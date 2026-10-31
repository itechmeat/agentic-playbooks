//! The Landlock scope the sandbox helper applies before it execs the agent:
//! abstract unix sockets and signals outside the sandbox's own Landlock
//! domain are closed (ABI 6, Linux 6.12 and later). Raw syscalls through
//! `libc`, so no Landlock crate is needed.

/// The kernel's Landlock ABI version, 0 when Landlock is unavailable (an
/// older kernel, Landlock not enabled, another system).
pub fn abi() -> u32 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: landlock_create_ruleset(NULL, 0, VERSION) only returns the
        // ABI version; it reads no memory.
        let v = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<u8>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        u32::try_from(v).unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cfg(target_os = "linux")]
const CREATE_RULESET_VERSION: u32 = 1;

/// `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET | LANDLOCK_SCOPE_SIGNAL`.
#[cfg(target_os = "linux")]
const SCOPED: u64 = 0b11;

/// `struct landlock_ruleset_attr` as of ABI 6.
#[cfg(target_os = "linux")]
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// Restricts this process (and everything it execs) to its own Landlock
/// domain for abstract unix sockets and signals. Sets `no_new_privs` first,
/// which the kernel requires of an unprivileged caller.
pub fn restrict() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Error;
        // SAFETY: plain integer arguments; touches only this process.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(Error::last_os_error());
        }
        let attr = RulesetAttr {
            handled_access_fs: 0,
            handled_access_net: 0,
            scoped: SCOPED,
        };
        // SAFETY: `attr` outlives the call and its size is passed with it.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        let fd = fd as libc::c_int;
        // SAFETY: `fd` is the ruleset just created; it is closed below.
        let r = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0u32) };
        let err = (r != 0).then(Error::last_os_error);
        // SAFETY: closes the descriptor this function opened.
        unsafe { libc::close(fd) };
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::other("Landlock is Linux only"))
    }
}

/// The exit code of the helper when it cannot do what it was asked.
pub const HELPER_REFUSED: i32 = 126;

/// `apb __sandbox-exec [--scope] [--require-scope] -- <program> [args]`:
/// the helper bwrap runs inside the sandbox. With `--scope` it applies the
/// Landlock scope; with `--require-scope` a scope that cannot be applied
/// stops it (exit 126) instead of running the agent unscoped. Then it execs
/// the agent; it only returns on failure, with the exit code to use.
pub fn exec_main(args: &[std::ffi::OsString]) -> i32 {
    let mut scope = false;
    let mut require = false;
    let mut rest = args.iter();
    for a in rest.by_ref() {
        match a.to_str() {
            Some("--scope") => scope = true,
            Some("--require-scope") => require = true,
            Some("--") => break,
            _ => {
                eprintln!("apb __sandbox-exec: unexpected argument");
                return HELPER_REFUSED;
            }
        }
    }
    let argv: Vec<&std::ffi::OsString> = rest.collect();
    let Some((program, args)) = argv.split_first() else {
        eprintln!("apb __sandbox-exec: no program to run");
        return HELPER_REFUSED;
    };
    // A scope asked for but not required is skipped when it fails; a
    // required one stops the helper.
    if (scope || require)
        && let Err(e) = restrict()
        && require
    {
        eprintln!("apb __sandbox-exec: the Landlock scope is required but failed: {e}");
        return HELPER_REFUSED;
    }
    exec(program, args)
}

#[cfg(unix)]
fn exec(program: &std::ffi::OsStr, args: &[&std::ffi::OsString]) -> i32 {
    use std::os::unix::process::CommandExt as _;
    let e = std::process::Command::new(program).args(args).exec();
    eprintln!("apb __sandbox-exec: cannot run the agent: {e}");
    127
}

#[cfg(not(unix))]
fn exec(_program: &std::ffi::OsStr, _args: &[&std::ffi::OsString]) -> i32 {
    eprintln!("apb __sandbox-exec: not supported on this system");
    HELPER_REFUSED
}
