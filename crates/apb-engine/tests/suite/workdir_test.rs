use apb_engine::error::EngineError;
use apb_engine::run_config::{RunConfig, read_run_config, write_run_config};
use apb_engine::workdir::acquire;
use std::collections::BTreeMap;

#[test]
fn run_config_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut params = BTreeMap::new();
    params.insert("task".to_string(), "do it".to_string());
    let cfg = RunConfig {
        params,
        instruction: Some("careful".into()),
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        parent_run: None,
        depth: 0,
        expected_children: None,
    };
    write_run_config(dir.path(), &cfg).unwrap();
    let back = read_run_config(dir.path()).unwrap();
    assert_eq!(back.params.get("task").map(String::as_str), Some("do it"));
    assert_eq!(back.instruction.as_deref(), Some("careful"));
}

#[test]
fn second_writer_is_refused_but_shared_allowed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let guard = acquire(root.path(), false).unwrap();
    assert!(guard.is_some());
    // second acquire without allow_shared - rejected
    match acquire(root.path(), false) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("expected WorkdirBusy, got {other:?}"),
    }
    // with allow_shared - allowed (no guard returned)
    assert!(acquire(root.path(), true).unwrap().is_none());
    // after releasing the first lock, acquire is possible again
    drop(guard);
    assert!(acquire(root.path(), false).unwrap().is_some());
}
