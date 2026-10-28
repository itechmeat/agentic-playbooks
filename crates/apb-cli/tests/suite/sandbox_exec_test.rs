//! `apb __sandbox-exec`: the helper bwrap runs inside an isolation sandbox.
//! With `--scope` it applies the Landlock scope before it execs the
//! program, so the program cannot signal a process outside the sandbox (its
//! own parent here). On a kernel without the scope, `--require-scope`
//! refuses to run the program at all: either the claim holds or nothing
//! runs. `APB_SANDBOX_TESTS=require` turns a kernel without the scope into a
//! failure instead.

use crate::common::apb_std;

/// `sh -c 'kill -0 $PPID'` through the helper: whether the program could
/// signal its parent (this test process), and the exit code.
fn signal_parent(flags: &[&str]) -> (bool, Option<i32>) {
    let marker = tempfile::tempdir().unwrap();
    let ok = marker.path().join("signalled");
    let script = format!("kill -0 $PPID 2>/dev/null && touch '{}'", ok.display());
    let st = apb_std()
        .arg("__sandbox-exec")
        .args(flags)
        .args(["--", "sh", "-c", &script])
        .status()
        .unwrap();
    (ok.exists(), st.code())
}

#[cfg(target_os = "linux")]
fn scope_available() -> bool {
    apb_engine::sandbox::scope::abi() >= apb_core::isolation::contract::SCOPE_ABI
}

#[cfg(target_os = "linux")]
#[test]
fn the_scope_blocks_signals_out_of_the_sandbox_or_the_helper_refuses() {
    let (signalled, code) = signal_parent(&["--scope", "--require-scope"]);
    if scope_available() {
        assert!(!signalled, "a scoped program signalled its parent");
        assert_eq!(code, Some(1), "kill -0 fails under the scope");
    } else {
        assert!(!signalled);
        assert_eq!(code, Some(126), "--require-scope refuses without the scope");
        assert!(
            std::env::var("APB_SANDBOX_TESTS").as_deref() != Ok("require"),
            "APB_SANDBOX_TESTS=require: this kernel has no Landlock scope (ABI 6)"
        );
        eprintln!("skip: no Landlock scope on this kernel, the refusal was checked");
    }
}

/// Without `--scope` the helper is a plain exec: the control for the test
/// above.
#[cfg(unix)]
#[test]
fn without_the_scope_the_helper_just_execs() {
    let (signalled, code) = signal_parent(&[]);
    assert!(signalled);
    assert_eq!(code, Some(0));
}

#[test]
fn a_malformed_helper_call_refuses() {
    let st = apb_std()
        .args(["__sandbox-exec", "--bogus", "--", "true"])
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(126));
    let st = apb_std().args(["__sandbox-exec", "--"]).status().unwrap();
    assert_eq!(st.code(), Some(126));
}
