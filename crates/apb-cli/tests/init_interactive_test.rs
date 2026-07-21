use std::process::Command;

#[test]
fn init_with_piped_stdio_stays_noninteractive() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("init")
        .current_dir(dir.path())
        .env("APB_NO_REGISTRY", "1")
        .stdin(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("initialized"));
    // questionnaire must not have run: no consent files written
    assert!(!dir.path().join("CLAUDE.md").exists());
    assert!(!dir.path().join("AGENTS.md").exists());
    assert!(dir.path().join(".apb/config.yaml").exists());
}

#[test]
fn init_rerun_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let out = Command::new(env!("CARGO_BIN_EXE_apb"))
            .arg("init")
            .current_dir(dir.path())
            .env("APB_NO_REGISTRY", "1")
            .stdin(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert!(out.status.success());
    }
}
