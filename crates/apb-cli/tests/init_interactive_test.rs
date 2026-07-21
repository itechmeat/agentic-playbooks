use std::process::Command;

// Regression guard: with stdin a terminal but stdout piped (e.g.
// `apb subscriptions | tool`), the interactive survey must NOT launch a
// blocking prompt. It has to fall through to the plain hint and exit
// promptly without reading stdin. We give the child a real pty on stdin (so
// `is_terminal()` is true there) while piping stdout, then assert it finishes
// well within a deadline and printed the non-interactive hint rather than a
// survey prompt. Before the both-TTY fix this hung on the multiselect.
#[cfg(unix)]
#[test]
fn survey_offer_does_not_block_when_stdout_is_piped() {
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::time::{Duration, Instant};

    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // Allocate a pseudo-terminal; the slave becomes the child's stdin.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // Own both fds so they close deterministically. The master stays open for
    // the whole wait so the slave never sees EOF: a broken build that entered
    // the prompt would truly block on an empty terminal.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };

    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap(); // fresh, so onboarding is Uninitialized
    let mut child = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("subscriptions")
        .current_dir(dir.path())
        .env("APB_NO_REGISTRY", "1")
        .env("APB_CONFIG_DIR", cfg.path())
        .stdin(std::process::Stdio::from(slave)) // a terminal
        .stdout(std::process::Stdio::piped()) // NOT a terminal
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`apb subscriptions` blocked on a prompt when stdout was piped");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(master);

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .ok();
    assert!(status.success(), "exit was {status:?}; stdout: {stdout}");
    // Fell through to the plain, non-interactive hint...
    assert!(
        stdout.contains("to declare"),
        "expected the non-interactive hint; got: {stdout}"
    );
    // ...and never rendered the interactive survey prompt.
    assert!(
        !stdout.contains("space to toggle"),
        "survey prompt leaked to piped stdout: {stdout}"
    );
}

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
