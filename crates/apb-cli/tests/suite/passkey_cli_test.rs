//! `apb passkey`: enrollment and removal are a person's, at a terminal.
//! The one-time enrollment code is only issued (and only becomes live)
//! after the person typed it back at the terminal, and it is refused from a
//! run step; every removal, last or not, asks for a typed code, the same
//! rule the dashboard enforces with an assertion. The config dir is the
//! suite's shared sandbox; these tests enroll nothing that survives them.

use crate::common::apb;

/// A private config dir for this suite: the shared sandbox holds state
/// other tests race on.
fn own_config_dir() -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap().keep();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A non-interactive shell is never a person: `apb passkey enroll` with
/// piped stdin writes no code file and exits nonzero, so a coding agent
/// that runs the command cannot get a usable code.
#[test]
fn an_enroll_without_a_terminal_writes_no_code() {
    let cfg = own_config_dir();
    let out = apb()
        .args(["passkey", "enroll", "agent-key"])
        .env("APB_CONFIG_DIR", &cfg)
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not confirmed") || err.contains("terminal"),
        "{err}"
    );
    let code_file = cfg.join("passkey-enroll-code.yaml");
    assert!(!code_file.exists(), "no code was written: {code_file:?}");
}

/// Inside a run step the command is refused before anything else, like the
/// other person-only commands.
#[test]
fn an_enroll_from_a_run_step_is_refused() {
    let cfg = own_config_dir();
    let out = apb()
        .args(["passkey", "enroll", "agent-key"])
        .env("APB_CONFIG_DIR", &cfg)
        .env("APB_RUN_ID", "some-run")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("refused"), "{err}");
    let code_file = cfg.join("passkey-enroll-code.yaml");
    assert!(!code_file.exists(), "no code was written: {code_file:?}");
}

/// Removing a passkey needs the typed code at a terminal, for the last one
/// and for every other one alike (the dashboard's assertion rule, F10):
/// a piped invocation removes nothing.
#[test]
fn a_remove_without_a_terminal_removes_nothing() {
    // Seed two passkeys straight into this suite's store.
    let cfg = own_config_dir();
    let file = cfg.join("passkeys.yaml");
    if file.exists() {
        std::fs::remove_file(&file).unwrap();
    }
    apb_core::passkey::add_into(&file, "one", "aWQ=", "cEE=", -7, 0).unwrap();
    apb_core::passkey::add_into(&file, "two", "aWQy", "cEE=", -7, 0).unwrap();
    let out = apb()
        .args(["passkey", "remove", "one"])
        .env("APB_CONFIG_DIR", &cfg)
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not confirmed"), "{err}");
    let listed = apb_core::passkey::list_from(&file).unwrap();
    assert_eq!(listed.len(), 2, "nothing was removed");
    // Cleanup so other suites start from an empty store.
    let _ = std::fs::remove_file(&file);
}

/// `apb passkey list` shows a credential by its eight-character prefix, the
/// dashboard's convention (a credential id can track a person), and never
/// writes a label's control characters to the terminal raw.
#[test]
fn a_list_shows_a_credential_prefix_and_a_clean_label() {
    let cfg = own_config_dir();
    let file = cfg.join("passkeys.yaml");
    let id = "Q3JlZGVudGlhbElkVGhhdElzTG9uZw";
    apb_core::passkey::add_into(&file, "desk\u{1b}[31mred", id, "cEE=", -7, 0).unwrap();
    let out = apb()
        .args(["passkey", "list"])
        .env("APB_CONFIG_DIR", &cfg)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("credential Q3JlZGVu..."), "{text}");
    assert!(
        !text.contains(id),
        "the full credential id is not shown: {text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "no raw escape reaches the terminal: {text}"
    );
    assert!(text.contains("desk"), "{text}");
}
