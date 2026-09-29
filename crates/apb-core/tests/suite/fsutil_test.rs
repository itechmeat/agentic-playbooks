use apb_core::fsutil::atomic_write;
use apb_core::registry::init_project;
use std::fs;

#[test]
fn atomic_write_creates_file_with_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("current");
    atomic_write(&path, b"1.0.0").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "1.0.0");
    // a second write overwrites atomically
    atomic_write(&path, b"1.1.0").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "1.1.0");
    // no temp files left behind
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn init_creates_apb_structure_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    for sub in ["playbooks", "profiles", "runs"] {
        assert!(dir.path().join(".apb").join(sub).is_dir(), "missing {sub}");
    }
    assert!(dir.path().join(".apb/config.yaml").is_file());
    // a repeat init doesn't fail and doesn't clobber the config
    fs::write(dir.path().join(".apb/config.yaml"), "port: 9999\n").unwrap();
    init_project(dir.path()).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/config.yaml")).unwrap(),
        "port: 9999\n"
    );
}

/// A freshly written executable reads as busy (ETXTBSY) while any process
/// still holds a write handle to it; the spawn waits that out instead of
/// failing. Linux-only: that is where exec reports a file open for writing
/// as busy (not verified on macOS).
#[cfg(target_os = "linux")]
#[test]
fn a_spawn_waits_out_an_executable_that_is_still_open_for_writing() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("tool");
    let mut writer = std::fs::File::create(&exe).unwrap();
    writer.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
    writer.flush().unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The premise: while the handle is open, a plain spawn is refused.
    let busy = std::process::Command::new(&exe)
        .spawn()
        .map(|mut c| c.wait());
    assert_eq!(
        busy.err().map(|e| e.kind()),
        Some(std::io::ErrorKind::ExecutableFileBusy),
        "the fixture must make the executable busy"
    );
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(writer);
    });
    let mut child =
        apb_core::fsutil::spawn_when_not_busy(&mut std::process::Command::new(&exe)).unwrap();
    release.join().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out after 10s waiting for the spawned tool to exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success());
}
