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

// The standing-instruction consent steps of the questionnaire, driven on a real
// pty because they only run when stdin AND stdout are terminals. Prompts are
// matched on a distinctive fragment of their question and answered with a
// single key ('y'/'n' submit immediately in cliclack), so a prompt that never
// appears fails the test on the deadline instead of hanging forever.
#[cfg(unix)]
const PLAYBOOK_MARKER: &str = "## apb playbooks (standing instruction)";
#[cfg(unix)]
const FEEDBACK_MARKER: &str = "## apb feedback loop (standing instruction)";
#[cfg(unix)]
const PLAYBOOK_PROMPT: &str = "discover saved playbooks";
#[cfg(unix)]
const FEEDBACK_PROMPT: &str = "report apb errors";

/// Runs `apb init` in `project` on a pseudo-terminal, answering `answers` in
/// order: each entry is the prompt fragment to wait for and the key to send.
/// The subscriptions survey is pre-declined in a throwaway config dir so the
/// run ends right after the consent steps. Returns everything the child wrote
/// to the terminal.
#[cfg(unix)]
fn init_on_tty(project: &std::path::Path, answers: &[(&str, u8)]) -> String {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path().join("state")).unwrap();
    std::fs::write(
        cfg.path().join("state/onboarding.json"),
        b"{\"state\":\"declined\"}",
    )
    .unwrap();

    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // A concrete window size: a 0x0 terminal is not what any real session looks
    // like, and prompt rendering is width-driven.
    let mut winsize = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut winsize,
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };

    let mut child = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("init")
        .current_dir(project)
        .env("APB_NO_REGISTRY", "1")
        .env("APB_CONFIG_DIR", cfg.path())
        .stdin(std::process::Stdio::from(slave.try_clone().unwrap()))
        .stdout(std::process::Stdio::from(slave.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    // The parent must not hold the slave open, otherwise reads on the master
    // never end once the child is gone.
    drop(slave);

    // Drain the master continuously: a full pty buffer would block the child
    // mid-prompt, which would look exactly like a missing prompt.
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let mut reader = std::fs::File::from(master.try_clone().unwrap());
    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });

    let mut writer = std::fs::File::from(master.try_clone().unwrap());
    let mut consumed = 0usize;
    for (fragment, key) in answers {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = seen.lock().unwrap().clone();
            if let Some(at) = text[consumed..].find(fragment) {
                consumed += at + fragment.len();
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("prompt `{fragment}` never appeared; terminal so far:\n{text}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        writer.write_all(&[*key]).unwrap();
        writer.flush().unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let text = seen.lock().unwrap().clone();
            panic!("`apb init` did not finish the questionnaire; terminal so far:\n{text}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(master);
    let _ = pump.join();
    let text = seen.lock().unwrap().clone();
    assert!(status.success(), "exit was {status:?}; terminal:\n{text}");
    text
}

#[cfg(unix)]
#[test]
fn playbook_consent_writes_the_block_to_both_files() {
    let dir = tempfile::tempdir().unwrap();
    init_on_tty(
        dir.path(),
        &[(PLAYBOOK_PROMPT, b'y'), (FEEDBACK_PROMPT, b'n')],
    );
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert!(
            text.contains(PLAYBOOK_MARKER),
            "{name} has no playbook block"
        );
        assert!(
            text.contains("playbook_catalog"),
            "{name} block is truncated"
        );
        // The declined feedback step must not have written anything.
        assert!(
            !text.contains(FEEDBACK_MARKER),
            "{name} got the feedback block after a decline"
        );
    }
}

#[cfg(unix)]
#[test]
fn declining_both_standing_blocks_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    init_on_tty(
        dir.path(),
        &[(PLAYBOOK_PROMPT, b'n'), (FEEDBACK_PROMPT, b'n')],
    );
    assert!(!dir.path().join("CLAUDE.md").exists());
    assert!(!dir.path().join("AGENTS.md").exists());
    assert!(dir.path().join(".apb/config.yaml").exists());
}

#[cfg(unix)]
#[test]
fn rerun_does_not_duplicate_the_playbook_block() {
    let dir = tempfile::tempdir().unwrap();
    init_on_tty(
        dir.path(),
        &[(PLAYBOOK_PROMPT, b'y'), (FEEDBACK_PROMPT, b'n')],
    );
    // Second run: the block is already there, so only the feedback step asks.
    let text = init_on_tty(dir.path(), &[(FEEDBACK_PROMPT, b'n')]);
    assert!(
        text.contains("Playbook instructions already configured"),
        "re-run did not report the block as configured; terminal:\n{text}"
    );
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let body = std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(
            body.matches(PLAYBOOK_MARKER).count(),
            1,
            "{name} has a duplicated playbook block"
        );
    }
}

#[cfg(unix)]
#[test]
fn playbook_and_feedback_blocks_coexist_in_one_file() {
    let dir = tempfile::tempdir().unwrap();
    init_on_tty(
        dir.path(),
        &[(PLAYBOOK_PROMPT, b'y'), (FEEDBACK_PROMPT, b'y')],
    );
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
        let playbooks = text.find(PLAYBOOK_MARKER).expect("playbook block");
        let feedback = text.find(FEEDBACK_MARKER).expect("feedback block");
        assert_eq!(text.matches(PLAYBOOK_MARKER).count(), 1, "{name}");
        assert_eq!(text.matches(FEEDBACK_MARKER).count(), 1, "{name}");
        assert!(
            playbooks < feedback,
            "{name} lost the ask order of the two blocks"
        );
        assert!(
            text.contains("\n\n## apb feedback loop"),
            "{name} lost the blank-line separator between blocks"
        );
    }
}
