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
