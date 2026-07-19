//! Detection tests using stub scripts. The env is global - all tests share one
//! Mutex; PATH/HOME/APB_CONFIG_DIR are set for the duration of the test.
//!
//! Unix-only: the outer `#[cfg(unix)]` now lives on this module's `mod` line
//! in `../main.rs` (this file's former inner `#![cfg(unix)]` attribute is not
//! valid on a file included via `#[path]` as a non-root module).

use std::path::Path;

use apb_core::detect::{self, AgentCategory, AuthKind, Authority};

use crate::common::env_lock as lock;

/// Writes an executable sh script `name` into `dir`; each invocation appends
/// its arguments to `counter` (to count the number of spawns).
fn write_agent(dir: &Path, name: &str, counter: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n",
        counter.display()
    );
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn count_lines(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

struct Env {
    _bin: tempfile::TempDir,
    _home: tempfile::TempDir,
    _cfg: tempfile::TempDir,
    bin: std::path::PathBuf,
    home: std::path::PathBuf,
    cfg: std::path::PathBuf,
    counter: std::path::PathBuf,
    orig_path: Option<std::ffi::OsString>,
    orig_home: Option<std::ffi::OsString>,
    orig_cfg: Option<std::ffi::OsString>,
    orig_cwd: std::path::PathBuf,
}

// Restores PATH/HOME/APB_CONFIG_DIR/CWD to their pre-test values. Formerly
// this crate's detect tests ran in their own process (one file = one
// binary), so leaving these process-global settings mutated at test end was
// harmless - the process exited right after. Now that this module shares a
// process with every other module in the consolidated integration binary,
// an unrestored PATH/HOME/APB_CONFIG_DIR/CWD leaks into whichever test runs
// next and can make unrelated checks fail nondeterministically (e.g. a
// doctor_test check that expects the real PATH to still contain `sh`).
impl Drop for Env {
    fn drop(&mut self) {
        unsafe {
            match &self.orig_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
            match &self.orig_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.orig_cfg {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        let _ = std::env::set_current_dir(&self.orig_cwd);
    }
}

/// Prepares a hermetic environment: an empty bin in PATH, fresh HOME and APB_CONFIG_DIR.
fn setup() -> Env {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let counter = bin.path().join("_calls");
    let orig_path = std::env::var_os("PATH");
    let orig_home = std::env::var_os("HOME");
    let orig_cfg = std::env::var_os("APB_CONFIG_DIR");
    let orig_cwd = std::env::current_dir().unwrap();
    unsafe {
        std::env::set_var("PATH", bin.path());
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    Env {
        bin: bin.path().to_path_buf(),
        home: home.path().to_path_buf(),
        cfg: cfg.path().to_path_buf(),
        counter,
        orig_path,
        orig_home,
        orig_cfg,
        orig_cwd,
        _bin: bin,
        _home: home,
        _cfg: cfg,
    }
}

#[test]
fn detects_version_and_full_models_for_installed_aggregator() {
    let _l = lock();
    let e = setup();
    write_agent(
        &e.bin,
        "opencode",
        &e.counter,
        "case \"$1\" in\n  --version) echo 9.9.9 ;;\n  models) printf 'prov/a\\nprov/b\\n' ;;\nesac",
    );

    let agents = detect::detect(true);
    let oc = agents.iter().find(|a| a.agent == "opencode").unwrap();
    assert!(oc.installed);
    assert_eq!(oc.category, AgentCategory::Aggregator);
    assert_eq!(oc.version.as_deref(), Some("9.9.9"));
    let models = oc.models.as_ref().unwrap();
    assert_eq!(models.authority, Authority::Full);
    assert_eq!(
        models.items,
        vec!["prov/a".to_string(), "prov/b".to_string()]
    );

    // pi is not installed - installed=false, no panic.
    let pi = agents.iter().find(|a| a.agent == "pi").unwrap();
    assert!(!pi.installed);
    assert!(pi.version.is_none());
}

#[test]
fn claude_static_models_when_installed() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "claude", &e.counter, "echo 'claude 1.0.0'");
    let agents = detect::detect(true);
    let c = agents.iter().find(|a| a.agent == "claude").unwrap();
    assert!(c.installed);
    assert_eq!(c.category, AgentCategory::Vendor);
    let m = c.models.as_ref().unwrap();
    assert_eq!(m.authority, Authority::Static);
    assert!(m.items.iter().any(|s| s.starts_with("claude-")));
}

#[test]
fn cache_hit_avoids_respawn_but_refresh_forces_it() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "opencode", &e.counter, "echo 1.0.0");

    detect::detect(true);
    let after_first = count_lines(&e.counter);
    assert!(after_first >= 1, "first detect must spawn");

    // Second call without refresh - from cache, no new spawns.
    detect::detect(false);
    assert_eq!(
        count_lines(&e.counter),
        after_first,
        "cache hit must not respawn"
    );

    // refresh=true ignores the cache - spawns again.
    detect::detect(true);
    assert!(
        count_lines(&e.counter) > after_first,
        "refresh must respawn"
    );
}

#[test]
fn binary_mtime_change_invalidates_cache() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "opencode", &e.counter, "echo 1.0.0");
    detect::detect(true);
    let base = count_lines(&e.counter);

    // Overwrite the binary (changing content and mtime) - cache is invalidated.
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_agent(&e.bin, "opencode", &e.counter, "echo 2.0.0");
    detect::detect(false);
    assert!(count_lines(&e.counter) > base, "mtime change must reprobe");
}

#[test]
fn hung_agent_times_out_without_hanging() {
    let _l = lock();
    let e = setup();
    unsafe {
        std::env::set_var("APB_PROBE_TIMEOUT_MS", "200");
    }
    write_agent(&e.bin, "agy", &e.counter, "sleep 30");
    let start = std::time::Instant::now();
    let agents = detect::detect(true);
    unsafe {
        std::env::remove_var("APB_PROBE_TIMEOUT_MS");
    }
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "must not hang"
    );
    let agy = agents.iter().find(|a| a.agent == "agy").unwrap();
    assert!(agy.installed, "binary present -> installed");
    assert!(
        agy.version.is_none(),
        "timed-out version probe yields no version"
    );
    assert!(agy.notes.iter().any(|n| n.contains("version probe failed")));
}

#[test]
fn large_output_does_not_deadlock() {
    let _l = lock();
    let e = setup();
    // ~1 MiB for models - gets drained, the process doesn't block on the write.
    write_agent(
        &e.bin,
        "opencode",
        &e.counter,
        "case \"$1\" in\n  --version) echo 1.0.0 ;;\n  models) yes prov/x | head -c 1000000 ;;\nesac",
    );
    let start = std::time::Instant::now();
    let agents = detect::detect(true);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "must not deadlock"
    );
    let oc = agents.iter().find(|a| a.agent == "opencode").unwrap();
    assert!(oc.installed);
    assert!(oc.models.is_some());
}

#[test]
fn configured_custom_agent_gets_presence_result() {
    let _l = lock();
    let e = setup();
    // A custom agent mycli with probe: true in the global config.
    std::fs::write(
        e.cfg.join("config.yaml"),
        "agents:\n  mycli:\n    probe: true\n",
    )
    .unwrap();
    write_agent(&e.bin, "mycli", &e.counter, "echo 3.2.1");

    let agents = detect::detect(true);
    let m = agents
        .iter()
        .find(|a| a.agent == "mycli")
        .expect("custom agent detected");
    assert!(m.installed);
    assert_eq!(m.version.as_deref(), Some("3.2.1"));
}

#[test]
fn interpreter_beside_agent_is_reachable_via_child_path() {
    let _l = lock();
    let e = setup();
    // Interpreter beside the agent; the agent is a shebang pointing to it via env.
    write_agent(&e.bin, "fake-runtime", &e.counter, "echo 7.7.7");
    use std::os::unix::fs::PermissionsExt;
    let agy = e.bin.join("agy");
    std::fs::write(&agy, "#!/usr/bin/env fake-runtime\n# ignored by runtime\n").unwrap();
    std::fs::set_permissions(&agy, std::fs::Permissions::from_mode(0o755)).unwrap();

    let agents = detect::detect(true);
    let a = agents.iter().find(|a| a.agent == "agy").unwrap();
    assert!(a.installed);
    // Version was captured - meaning env found fake-runtime on the child PATH
    // (the binary's parent dir was added). Without that, exec would have failed
    // and there would be no version.
    assert_eq!(
        a.version.as_deref(),
        Some("7.7.7"),
        "interpreter beside the agent must be reachable: {:?}",
        a.notes
    );
}

#[test]
fn config_source_change_invalidates_cache_before_ttl() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "codex", &e.counter, "echo 1.0.0");
    let codex_dir = e.home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(codex_dir.join("config.toml"), "model = \"a\"\n").unwrap();

    detect::detect(true);
    let base = count_lines(&e.counter);
    // Cache hit: no source changes - no respawn.
    detect::detect(false);
    assert_eq!(count_lines(&e.counter), base, "cache hit must not respawn");

    // Change config.toml (codex's models source). Also change the content SIZE,
    // not just the mtime - that way invalidation doesn't depend on the
    // filesystem's mtime granularity (the fingerprint is size:mtime).
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(codex_dir.join("config.toml"), "model = \"bbbbbbbb\"\n").unwrap();
    detect::detect(false);
    assert!(
        count_lines(&e.counter) > base,
        "config source change must invalidate cache before TTL"
    );
}

#[test]
fn codex_auth_classified_by_key_names_without_leaking_values() {
    let _l = lock();
    // (tokens) -> oauth; (OPENAI_API_KEY) -> api-key; unknown shape -> api-key.
    // Expected value is the lowercased Debug form of AuthKind (Oauth->oauth, ApiKey->apikey).
    for (body, want) in [
        (r#"{"tokens":{"access":"SECRET-OAUTH-TOKEN"}}"#, "oauth"),
        (r#"{"account_id":"acct_123"}"#, "oauth"),
        (r#"{"OPENAI_API_KEY":"SECRET-API-KEY"}"#, "apikey"),
        (r#"{"something_else":"SECRET-XYZ"}"#, "apikey"),
    ] {
        let e = setup();
        write_agent(&e.bin, "codex", &e.counter, "echo 1.0.0");
        let codex_dir = e.home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("auth.json"), body).unwrap();

        let agents = detect::detect(true);
        let codex = agents.iter().find(|a| a.agent == "codex").unwrap();
        let kind = codex
            .auth
            .as_ref()
            .map(|a| format!("{:?}", a.kind).to_lowercase())
            .unwrap_or_default();
        assert_eq!(kind, want, "auth kind for `{body}`");

        // The secret value must NOT leak into either the detect result or the cache.
        let serialized = serde_json::to_string(&agents).unwrap();
        for secret in [
            "SECRET-OAUTH-TOKEN",
            "SECRET-API-KEY",
            "SECRET-XYZ",
            "acct_123",
        ] {
            assert!(
                !serialized.contains(secret),
                "secret `{secret}` leaked into detect output"
            );
        }
        let cache =
            std::fs::read_to_string(e.cfg.join("state/agents-detect.json")).unwrap_or_default();
        for secret in [
            "SECRET-OAUTH-TOKEN",
            "SECRET-API-KEY",
            "SECRET-XYZ",
            "acct_123",
        ] {
            assert!(
                !cache.contains(secret),
                "secret `{secret}` leaked into the detect cache"
            );
        }
    }
}

#[test]
fn truncated_models_output_adds_note() {
    let _l = lock();
    let e = setup();
    // opencode models emits > MAX_OUTPUT_BYTES (256 KiB) -> a truncation note.
    write_agent(
        &e.bin,
        "opencode",
        &e.counter,
        "case \"$1\" in\n  --version) echo 1.0.0 ;;\n  models) yes prov/x | head -c 300000 ;;\nesac",
    );
    let agents = detect::detect(true);
    let oc = agents.iter().find(|a| a.agent == "opencode").unwrap();
    assert!(
        oc.notes.iter().any(|n| n.contains("truncated")),
        "truncation must add a note: {:?}",
        oc.notes
    );
}

#[test]
fn project_local_path_entry_is_ignored() {
    let _l = lock();
    let e = setup();
    // Place a fake agent in a subdirectory of CWD and add it to PATH: detect
    // must ignore it (project-local protection).
    let proj = tempfile::tempdir().unwrap();
    std::env::set_current_dir(proj.path()).unwrap();
    let local_bin = proj.path().join("node_modules/.bin");
    std::fs::create_dir_all(&local_bin).unwrap();
    write_agent(&local_bin, "opencode", &e.counter, "echo 6.6.6");
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", local_bin.display(), e.bin.display()),
        );
    }
    let agents = detect::detect(true);
    let oc = agents.iter().find(|a| a.agent == "opencode").unwrap();
    assert!(!oc.installed, "project-local agent must be ignored");
}

#[test]
fn hermes_probe_detects_stub_binary() {
    let _l = lock();
    let e = setup();
    write_agent(
        &e.bin,
        "hermes",
        &e.counter,
        "echo 'Hermes Agent v0.18.2 (2026.7.7.2) · upstream e361c5e2'",
    );

    let agents = detect::detect(true);
    let h = agents.iter().find(|a| a.agent == "hermes").unwrap();
    assert!(h.installed);
    assert_eq!(h.category, AgentCategory::Aggregator);
    assert_eq!(
        serde_json::to_string(&h.category).unwrap(),
        "\"aggregator\""
    );
    let version = h.version.as_deref().unwrap_or_default();
    assert!(
        version.contains("0.18.2"),
        "version must contain 0.18.2: {version:?}"
    );
    assert!(h.models.is_none());
}

#[test]
fn hermes_auth_hint_from_env_file() {
    let _l = lock();

    // With `~/.hermes/.env` present - api-key hint.
    let e = setup();
    write_agent(
        &e.bin,
        "hermes",
        &e.counter,
        "echo 'Hermes Agent v0.18.2 (2026.7.7.2) · upstream e361c5e2'",
    );
    let hermes_dir = e.home.join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(hermes_dir.join(".env"), "SOME_KEY=secret\n").unwrap();

    let agents = detect::detect(true);
    let h = agents.iter().find(|a| a.agent == "hermes").unwrap();
    let kind = h.auth.as_ref().map(|a| a.kind);
    assert_eq!(kind, Some(AuthKind::ApiKey));

    // The secret value must never leak into the detect output.
    let serialized = serde_json::to_string(&agents).unwrap();
    assert!(!serialized.contains("secret"));

    // Without the file - no auth hint.
    let e2 = setup();
    write_agent(
        &e2.bin,
        "hermes",
        &e2.counter,
        "echo 'Hermes Agent v0.18.2 (2026.7.7.2) · upstream e361c5e2'",
    );
    let agents2 = detect::detect(true);
    let h2 = agents2.iter().find(|a| a.agent == "hermes").unwrap();
    assert!(h2.auth.is_none());
}

#[test]
fn hermes_missing_binary_reports_not_installed() {
    let _l = lock();
    let _e = setup();

    let agents = detect::detect(true);
    let h = agents.iter().find(|a| a.agent == "hermes").unwrap();
    assert!(!h.installed);
    assert!(h.version.is_none());
}
