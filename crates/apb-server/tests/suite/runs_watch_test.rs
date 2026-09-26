use apb_server::AppState;
use std::fs;
use std::time::Duration;

#[tokio::test]
async fn watcher_emits_runs_changed_on_run_file() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut rx = state.events.subscribe();
    let _w =
        apb_server::watch::spawn_watcher(dir.path().to_path_buf(), state.events.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    fs::create_dir_all(dir.path().join(".apb/runs/demo-1")).unwrap();
    fs::write(dir.path().join(".apb/runs/demo-1/events.jsonl"), "{}\n").unwrap();

    // wait for a runs event (playbooks events may also slip through - look for the right one)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_runs = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(msg)) if msg.contains("runs_changed") => {
                saw_runs = true;
                break;
            }
            Ok(Ok(_)) => continue,
            // A single iteration's timeout elapsing is not a reason to give
            // up: keep polling until the overall deadline. Only an actually
            // closed channel (Err) breaks the loop.
            Err(_elapsed) => continue,
            Ok(Err(_closed)) => break,
        }
    }
    assert!(saw_runs, "expected a runs_changed event");
}

/// F21: the machine-wide dashboard subscribes to changes of global profiles,
/// installed connectors, connector accounts and trust too, so an edit made by
/// the CLI or MCP there reaches an open dashboard without a navigation.
#[tokio::test]
async fn global_watcher_emits_on_global_config_changes() {
    let sandbox = crate::common::config_sandbox().await;
    let cfg = std::path::PathBuf::from(std::env::var_os("APB_CONFIG_DIR").unwrap());
    fs::create_dir_all(cfg.join("profiles/p")).unwrap();
    let state = AppState::new_global();
    let mut rx = state.events.subscribe();
    let _w = apb_server::watch::spawn_global_watcher(state.events.clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    for (what, path) in [
        ("a global profile", cfg.join("profiles/p/profile.yaml")),
        ("trust", cfg.join("trust.json")),
    ] {
        fs::write(&path, "x: 1\n").unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut saw = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(msg)) if msg.contains("config_changed") => {
                    saw = true;
                    break;
                }
                Ok(Ok(_)) | Err(_) => continue,
                Ok(Err(_closed)) => break,
            }
        }
        assert!(saw, "expected config_changed after writing {what}");
    }
    drop(sandbox);
}
