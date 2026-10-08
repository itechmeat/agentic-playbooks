//! Detection tests using stub scripts. The env is global - all tests share one
//! Mutex; PATH/HOME/APB_CONFIG_DIR are set for the duration of the test.
//!
//! Unix-only: the outer `#[cfg(unix)]` now lives on this module's `mod` line
//! in `../main.rs` (this file's former inner `#![cfg(unix)]` attribute is not
//! valid on a file included via `#[path]` as a non-root module).

use std::path::Path;

use apb_core::agent_catalog;
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

    let agents = agent_catalog::agents(true);
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
    let agents = agent_catalog::agents(true);
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

    agent_catalog::agents(true);
    let after_first = count_lines(&e.counter);
    assert!(after_first >= 1, "first detect must spawn");

    // Second call without refresh - from cache, no new spawns.
    agent_catalog::agents(false);
    assert_eq!(
        count_lines(&e.counter),
        after_first,
        "cache hit must not respawn"
    );

    // refresh=true ignores the cache - spawns again.
    agent_catalog::agents(true);
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
    agent_catalog::agents(true);
    let base = count_lines(&e.counter);

    // Overwrite the binary (changing content and mtime) - cache is invalidated.
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_agent(&e.bin, "opencode", &e.counter, "echo 2.0.0");
    agent_catalog::agents(false);
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
    let agents = agent_catalog::agents(true);
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
    let agents = agent_catalog::agents(true);
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

    let agents = agent_catalog::agents(true);
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

    let agents = agent_catalog::agents(true);
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

    agent_catalog::agents(true);
    let base = count_lines(&e.counter);
    // Cache hit: no source changes - no respawn.
    agent_catalog::agents(false);
    assert_eq!(count_lines(&e.counter), base, "cache hit must not respawn");

    // Change config.toml (codex's providers source). Also change the content SIZE,
    // not just the mtime - that way invalidation doesn't depend on the
    // filesystem's mtime granularity (the fingerprint is size:mtime).
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(codex_dir.join("config.toml"), "model = \"bbbbbbbb\"\n").unwrap();
    agent_catalog::agents(false);
    assert!(
        count_lines(&e.counter) > base,
        "config source change must invalidate cache before TTL"
    );
}

/// CI runners (and hermetic tests) often lack a real `codex` on PATH. The
/// `[model_providers.*]` annotation from `~/.codex/config.toml` stays
/// file-based and is never gated on binary presence (regression that failed
/// PR CI on clean Linux runners). The static model list, like claude's, is
/// claimed only once codex is installed: exactly the table's list, default
/// first, and never the `model` line of config.toml.
#[test]
fn codex_static_models_and_config_providers_without_binary() {
    let _l = lock();
    let e = setup();
    // PATH is only the empty temp bin dir from setup: no codex binary.
    let codex_dir = e.home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        "model = \"gpt-custom\"\n[model_providers.openai]\n",
    )
    .unwrap();

    let agents = agent_catalog::agents(true);
    let codex = agents.iter().find(|a| a.agent == "codex").unwrap();
    assert!(
        !codex.installed,
        "no binary on PATH must leave installed=false"
    );
    assert!(codex.models.is_none(), "{codex:?}");
    assert_eq!(
        codex.providers,
        Some(vec!["openai".to_string()]),
        "model_providers sections are file-based"
    );

    write_agent(&e.bin, "codex", &e.counter, "echo 1.0.0");
    let agents = agent_catalog::agents(true);
    let codex = agents.iter().find(|a| a.agent == "codex").unwrap();
    assert!(codex.installed);
    let models = codex.models.as_ref().expect("installed: the static list");
    assert_eq!(models.authority, Authority::Static);
    assert_eq!(
        models.items,
        apb_core::models_table::builtin().codex_static_models,
        "codex's static list, in table order"
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

        let agents = agent_catalog::agents(true);
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
    let agents = agent_catalog::agents(true);
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
    let agents = agent_catalog::agents(true);
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

    let agents = agent_catalog::agents(true);
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

    let agents = agent_catalog::agents(true);
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
    let agents2 = agent_catalog::agents(true);
    let h2 = agents2.iter().find(|a| a.agent == "hermes").unwrap();
    assert!(h2.auth.is_none());
}

/// zcode is never on PATH: the desktop deploys its headless CLI under
/// `~/.zcode/server/agents/glm/`. Detection must find it there and report
/// apb's zcode allowlist as bare model ids (the two Individual-plan models -
/// never a Start-plan entry, whatever the built-in provider config enables),
/// annotated with the plan they resolve to, and flag a missing standalone
/// login - reading only credential KEY NAMES.
#[test]
fn zcode_probe_finds_the_home_deployed_cli_and_its_plan_models() {
    let _l = lock();
    let e = setup();
    let glm = e.home.join(".zcode/server/agents/glm");
    std::fs::create_dir_all(&glm).unwrap();
    write_agent(&glm, "zcode-agent", &e.counter, "echo 0.16.9");
    let bundled = e.home.join(".zcode/v2/runtime/provider/bundled");
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(
        bundled.join("zcode-builtin.json"),
        r#"{"config":{"modelConfigRules":{"builtinProviderModelRules":[
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-start-plan"},
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5.3-Flash","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5-Turbo","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:bigmodel-start-plan"}]}}}"#,
    )
    .unwrap();

    // Not logged in: the allowlist, with a note.
    let agents = agent_catalog::agents(true);
    let z = agents.iter().find(|a| a.agent == "zcode").unwrap();
    assert!(z.installed, "{z:?}");
    assert_eq!(z.version.as_deref(), Some("0.16.9"));
    assert_eq!(z.category, AgentCategory::Vendor);
    let m = z.models.as_ref().unwrap();
    assert_eq!(m.authority, Authority::Static);
    assert_eq!(
        m.items,
        vec!["GLM-5.3".to_string(), "GLM-5.3-Flash".to_string()],
        "bare allowlist ids only - no Turbo, no plan prefix"
    );
    assert_eq!(
        z.providers.as_deref(),
        Some(&["zai-individual".to_string()][..])
    );
    assert_eq!(z.auth.as_ref().map(|a| a.kind), Some(AuthKind::None));
    assert!(
        z.notes.iter().any(|n| n.contains("zcode-agent login")),
        "{z:?}"
    );

    // Logged in to both zai plans: the list does not grow (no Start-plan
    // model is ever offered); no secret leaks.
    std::fs::write(
        e.home.join(".zcode/v2/credentials.json"),
        r#"{"account-provider:account:zai-individual-coding-plan:identity":"ident-secret","account-provider:account:zai-start-plan:identity":"ident-secret"}"#,
    )
    .unwrap();
    let agents = agent_catalog::agents(true);
    let z = agents.iter().find(|a| a.agent == "zcode").unwrap();
    assert_eq!(
        z.models.as_ref().unwrap().items,
        vec!["GLM-5.3".to_string(), "GLM-5.3-Flash".to_string()]
    );
    assert_eq!(z.auth.as_ref().map(|a| a.kind), Some(AuthKind::Oauth));
    assert!(
        z.notes.iter().all(|n| !n.contains("zcode-agent login")),
        "logged in: no login note: {z:?}"
    );
    assert!(
        !serde_json::to_string(&agents)
            .unwrap()
            .contains("ident-secret")
    );
}

#[test]
fn hermes_missing_binary_reports_not_installed() {
    let _l = lock();
    let _e = setup();

    let agents = agent_catalog::agents(true);
    let h = agents.iter().find(|a| a.agent == "hermes").unwrap();
    assert!(!h.installed);
    assert!(h.version.is_none());
}

// A probe whose agent daemonizes a descendant that inherits the probe's
// stdout. `run_probe` spawns every probe with `process_group(0)` precisely so
// it can SIGKILL that whole group afterwards, on the SUCCESS path as well as
// on timeout - otherwise each `detect --refresh` leaves behind a live process
// and a reader thread blocked on a pipe that will never reach EOF.
//
// What this test can and cannot prove, stated plainly: the defect it guards
// was Linux-only. `detect` used to reap the group by spawning
// `kill -KILL -<pgid>`, which BSD kill (macOS) accepts and procps-ng kill
// (Linux) rejects as a bad option, silently delivering nothing - and the
// discarded ExitStatus hid it. So on macOS this test passed BEFORE the fix
// too, and only on Linux does it distinguish the syscall from the subprocess.
// It is kept because it pins the property on every platform and would catch a
// regression back to any signalling method that does not reach the group; it
// is not evidence that the Linux bug is fixed. That evidence can only come
// from CI.
#[test]
fn probe_reaps_a_daemonized_descendant_of_the_agent() {
    let _l = lock();
    let e = setup();
    let pidfile = e.home.join("probe-descendant.pid");
    // Backgrounds a long sleep (inheriting stdout), records its pid, then
    // answers the version probe and exits.
    write_agent(
        &e.bin,
        "agy",
        &e.counter,
        &format!("sleep 300 &\necho $! > '{}'\necho 1.2.3", pidfile.display()),
    );

    let start = std::time::Instant::now();
    let agents = agent_catalog::agents(true);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(60),
        "the probe blocked on a descendant holding its stdout: {:?}",
        start.elapsed()
    );

    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("the stub agent never recorded its descendant pid")
        .trim()
        .parse()
        .expect("descendant pid");

    // The probe still worked: reaping the group must not cost the answer.
    let agy = agents.iter().find(|a| a.agent == "agy").unwrap();
    assert_eq!(
        agy.version.as_deref(),
        Some("1.2.3"),
        "the version probe must still be read before the group is reaped"
    );

    // ... and the descendant is gone. SIGKILL is not instant, so allow a
    // moment, but bound the wait rather than looping forever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    // SAFETY: signal 0 only performs the existence check.
    while unsafe { libc::kill(pid, 0) } == 0 {
        if std::time::Instant::now() >= deadline {
            // SAFETY: as above. Do not leak a 300-second sleep on failure.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            panic!("the probe left a daemonized descendant (pid {pid}) alive");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// grok, cursor and qoder are built-in probes (spec 2026-07-21, and the
/// 2026-08-15 qoder addition). The binary names are verified against the real
/// CLIs: Grok Build installs `grok`, Cursor installs `cursor-agent`, Qoder
/// installs `qoder` (plus a `qodercli` alias, not `agent`). Grok and Cursor
/// both also ship or overlap with a generic `agent` alias, so `agent` is
/// deliberately NOT probed for either - it cannot identify either agent
/// unambiguously.
#[test]
fn builtin_probes_include_grok_and_cursor() {
    let probes = detect::builtin_probes();
    let by_id = |id: &str| {
        probes
            .iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("built-in probe `{id}` is missing"))
    };

    let grok = by_id("grok");
    assert_eq!(grok.bins, vec!["grok".to_string()]);
    assert_eq!(grok.category, AgentCategory::Vendor);
    assert_eq!(grok.version_args, vec!["--version".to_string()]);

    let cursor = by_id("cursor");
    assert_eq!(cursor.bins, vec!["cursor-agent".to_string()]);
    assert_eq!(cursor.category, AgentCategory::Aggregator);
    assert_eq!(cursor.version_args, vec!["--version".to_string()]);

    let qoder = by_id("qoder");
    assert_eq!(qoder.bins, vec!["qoder".to_string()]);
    assert_eq!(qoder.category, AgentCategory::Aggregator);
    assert_eq!(qoder.version_args, vec!["--version".to_string()]);

    // zcode: a PATH `zcode` first, then the location the ZCode desktop
    // deploys its headless CLI to (never on PATH). Vendor-tied (GLM).
    let zcode = by_id("zcode");
    assert_eq!(zcode.bins, vec!["zcode".to_string()]);
    assert_eq!(
        zcode.home_paths,
        vec![".zcode/server/agents/glm/zcode-agent".to_string()]
    );
    assert_eq!(zcode.category, AgentCategory::Vendor);
    assert_eq!(zcode.version_args, vec!["--version".to_string()]);

    // The ambiguous `agent` alias must not be probed by anyone.
    for p in &probes {
        assert!(
            !p.bins.iter().any(|b| b == "agent"),
            "probe `{}` claims the ambiguous `agent` binary",
            p.id
        );
    }
}

/// The detection memo holds external facts only: the lists apb owns (claude's
/// static list here) are never written to it, and still come back from
/// `detect` because they are added from the running binary on every call.
#[test]
fn memo_never_stores_apb_owned_model_lists() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "claude", &e.counter, "echo 2.0.0");
    let agents = agent_catalog::agents(true);
    let claude = agents.iter().find(|a| a.agent == "claude").unwrap();
    let want = apb_core::models_table::builtin().claude_static_models;
    assert_eq!(claude.models.as_ref().unwrap().items, want);

    let raw = std::fs::read_to_string(e.cfg.join("state/agents-detect.json")).unwrap();
    let memo: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let memo_claude = memo["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "claude")
        .unwrap();
    assert!(
        memo_claude.get("models").is_none(),
        "the memo must not carry claude's list: {memo_claude}"
    );
    assert!(
        want.iter().all(|m| !raw.contains(m.as_str())),
        "no apb-owned model id may appear anywhere in the memo"
    );
    assert_eq!(memo["build_id"], detect::build_id());
}

/// A memo written by another apb build is never reused, even within the TTL
/// and with every probe input unchanged.
#[test]
fn memo_from_another_build_is_ignored() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "opencode", &e.counter, "echo 1.0.0");
    agent_catalog::agents(true);
    let base = count_lines(&e.counter);
    agent_catalog::agents(false);
    assert_eq!(count_lines(&e.counter), base, "same build: memo hit");

    let path = e.cfg.join("state/agents-detect.json");
    let mut memo: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    memo["build_id"] = serde_json::json!("0.0.0|old-binary|0000");
    std::fs::write(&path, serde_json::to_vec(&memo).unwrap()).unwrap();
    agent_catalog::agents(false);
    assert!(count_lines(&e.counter) > base, "another build: re-probe");
}

/// `opencode models` depends on opencode's own config and model catalog, so
/// an edit to either invalidates the memo.
#[test]
fn opencode_config_change_invalidates_the_memo() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "opencode", &e.counter, "echo a/b");
    let cfg = e.home.join(".config/opencode");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("opencode.json"), "{}").unwrap();
    agent_catalog::agents(true);
    let base = count_lines(&e.counter);
    agent_catalog::agents(false);
    assert_eq!(count_lines(&e.counter), base);

    std::fs::write(cfg.join("opencode.json"), "{\"provider\":{}}").unwrap();
    agent_catalog::agents(false);
    assert!(
        count_lines(&e.counter) > base,
        "opencode.json edit: re-probe"
    );
}

/// Current opencode keeps its credentials under XDG data; provider names are
/// read from there (values never).
#[test]
fn opencode_providers_come_from_the_xdg_auth_file() {
    let _l = lock();
    let e = setup();
    write_agent(&e.bin, "opencode", &e.counter, "echo 1.0.0");
    let data = e.home.join(".local/share/opencode");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("auth.json"),
        r#"{"zhipuai-coding-plan":{"type":"api","key":"SECRET"}}"#,
    )
    .unwrap();
    let agents = agent_catalog::agents(true);
    let oc = agents.iter().find(|a| a.agent == "opencode").unwrap();
    assert_eq!(
        oc.providers.as_deref(),
        Some(&["zhipuai-coding-plan".to_string()][..])
    );
    let raw = std::fs::read_to_string(e.cfg.join("state/agents-detect.json")).unwrap();
    assert!(!raw.contains("SECRET"));
}

/// Issue #139 F19: runs launch `agents.<id>.program` from the config, so that
/// is the binary detection must report for a built-in agent, not whatever the
/// agent's default name finds on PATH. Repointing it must also invalidate the
/// detection memo.
#[test]
fn a_builtin_agent_is_detected_at_its_configured_program() {
    let _l = lock();
    let e = setup();
    // A PATH `codex` that runs never use.
    write_agent(&e.bin, "codex", &e.counter, "echo path-codex 1.0.0");
    let tools = e.home.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let configure = |name: &str, version: &str| {
        write_agent(&tools, name, &e.counter, &format!("echo {name} {version}"));
        let program = tools.join(name);
        std::fs::write(
            e.cfg.join("config.yaml"),
            format!("agents:\n  codex:\n    program: {}\n", program.display()),
        )
        .unwrap();
        std::fs::canonicalize(program).unwrap()
    };
    let codex = |refresh: bool| {
        agent_catalog::agents(refresh)
            .into_iter()
            .find(|a| a.agent == "codex")
            .unwrap()
    };

    let first = configure("my-codex", "2.0.0");
    let found = codex(true);
    assert_eq!(found.version.as_deref(), Some("my-codex 2.0.0"));
    assert_eq!(found.canonical_path.as_deref(), Some(first.as_path()));

    configure("other-codex", "3.0.0");
    assert_eq!(
        codex(false).version.as_deref(),
        Some("other-codex 3.0.0"),
        "a memo written for the old program was reused"
    );
}

#[test]
fn probe_keeps_a_nested_apb_out_of_the_project_registry() {
    let _l = lock();
    let e = setup();
    let seen = e.bin.join("_env");
    write_agent(
        &e.bin,
        "opencode",
        &e.counter,
        &format!(
            "case \"$1\" in\n  --version) printf '%s|%s|%s' \"$APB_NO_REGISTRY\" \"$APB_CONFIG_DIR\" \"$(pwd)\" > {}; echo 1.0.0 ;;\nesac",
            seen.display()
        ),
    );

    agent_catalog::agents(true);
    let env = std::fs::read_to_string(&seen).unwrap();
    // Outside the workspace (no project config, no `.apb` to register).
    assert_eq!(env, format!("1|{}|/", e.cfg.display()));
}

#[test]
fn zcode_desktop_app_on_path_is_skipped_for_the_home_cli() {
    let _l = lock();
    let e = setup();
    // The Linux desktop package: PATH `zcode` is the Electron app.
    // Not `ZCode`: on a case-insensitive file system (macOS) it would be the
    // same entry as the `zcode` link below.
    let desktop = e.bin.join("desktop-app");
    std::fs::create_dir_all(desktop.join("resources")).unwrap();
    std::fs::write(desktop.join("resources/app.asar"), "").unwrap();
    let ran = e.bin.join("_desktop_ran");
    write_agent(&desktop, "zcode", &ran, "echo 9.9.9-desktop");
    std::os::unix::fs::symlink(desktop.join("zcode"), e.bin.join("zcode")).unwrap();
    let glm = e.home.join(".zcode/server/agents/glm");
    std::fs::create_dir_all(&glm).unwrap();
    write_agent(&glm, "zcode-agent", &e.counter, "echo 0.16.9");

    let agents = agent_catalog::agents(true);
    let z = agents.iter().find(|a| a.agent == "zcode").unwrap();
    assert_eq!(z.version.as_deref(), Some("0.16.9"), "{z:?}");
    assert!(!ran.exists(), "the desktop app must never be run");
    assert!(apb_core::zcode::is_desktop_app(&e.bin.join("zcode")));
    assert_eq!(
        apb_core::zcode::default_program(),
        glm.join("zcode-agent").to_string_lossy()
    );
}
