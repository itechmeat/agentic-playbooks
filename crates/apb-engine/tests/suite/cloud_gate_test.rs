//! The cloud part of a start (`gate::cloud`): a NEAR model the catalog
//! lacks, one an agent step cannot run on, and a primary executor whose key
//! does not reach it refuse the start before a run directory exists; a
//! fallback's missing key is a warning only; host mode skips the check; a
//! rebind to a profile on a model without tool calling is refused.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::execution::{ExecutionMode, ExecutionRequest};
use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run, run_background};

use crate::common::{self, EnvVar};

const PLAYBOOK: &str = "schema: 2\nid: near\nname: Near\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: \"do\", max_retries: 0 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";

/// A project whose node binds `main` (`agent`, `model`, `fallbacks`), and a
/// stub agent program that exits 0.
fn seed(root: &Path, agent: &str, model: &str, fallbacks: &[(&str, &str)]) -> String {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/near/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/near/current"), "1.0.0").unwrap();
    common::seed_profile(root, "main", agent, model, fallbacks);
    let path = root.join("agent.sh");
    common::write_sync(&path, "#!/bin/sh\necho done\n");
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().into_owned()
}

fn runs(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

fn refused(model: &str, key: Option<&str>) -> String {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let agent = seed(dir.path(), "dsh", model, &[]);
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent),
        match key {
            Some(k) => EnvVar::set("NEARAI_API_KEY", k),
            None => EnvVar::remove("NEARAI_API_KEY"),
        },
    ];
    let Err(err) = run(dir.path(), "near", None, RunOptions::default()) else {
        panic!("the start is refused");
    };
    let err = err.to_string();
    assert_eq!(runs(dir.path()), 0, "no run directory: {err}");
    err
}

#[test]
fn an_unknown_model_is_refused_with_the_refresh_hint() {
    let err = refused("nearai/Qwen/Qwen3.8-72B", Some("fake-key-1234567"));
    assert!(err.contains("cloud_model_unknown"), "{err}");
    assert!(err.contains("apb models --refresh"), "{err}");
}

#[test]
fn a_model_without_tools_is_refused() {
    let err = refused(
        "nearai/Qwen/Qwen3-VL-30B-A3B-Instruct",
        Some("fake-key-1234567"),
    );
    assert!(err.contains("cloud_model_no_tools"), "{err}");
}

#[test]
fn a_primary_without_its_key_is_refused_naming_the_variable() {
    let err = refused("nearai/Qwen/Qwen3.8-27B", None);
    assert!(err.contains("cloud_key_missing"), "{err}");
    assert!(err.contains("NEARAI_API_KEY"), "{err}");
}

#[test]
fn a_fallback_without_its_key_is_a_warning_and_the_start_proceeds() {
    let doc = apb_core::profile::ProfileDoc::from_yaml(
        "name: main\nexecutor:\n  agent: claude\n  model: claude-sonnet-5-5\n  fallbacks:\n    - { agent: dsh, model: nearai/Qwen/Qwen3.8-27B }\n",
    )
    .unwrap();
    let _l = common::env_lock();
    let _env = EnvVar::remove("NEARAI_API_KEY");
    let snaps = apb_core::cloud_models::catalog::load_all();
    let warnings =
        apb_engine::gate::cloud::check_profile(&doc, "project/main", &snaps, &[]).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("NEARAI_API_KEY"), "{warnings:?}");
    drop(_env);
    drop(_l);

    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let agent = seed(
        dir.path(),
        "claude-code",
        "haiku",
        &[("dsh", "nearai/Qwen/Qwen3.8-27B")],
    );
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent),
        EnvVar::remove("NEARAI_API_KEY"),
    ];
    let res = run(dir.path(), "near", None, RunOptions::default()).expect("the start proceeds");
    assert_eq!(runs(dir.path()), 1, "{}", res.run_id);
}

/// A supervised start whose supervisor profile runs on NEAR AI Cloud
/// without its key is refused like a node's, before a run directory exists.
#[test]
fn a_supervisor_without_its_key_is_refused() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let agent = seed(root, "claude-code", "haiku", &[]);
    let vdir = root.join(".apb/playbooks/near/1.0.0");
    fs::write(
        vdir.join("playbook.yaml"),
        PLAYBOOK.replace(
            "defaults:\n  profile: main\n",
            "defaults:\n  profile: main\nsupervisor:\n  profile: sup\n",
        ),
    )
    .unwrap();
    common::seed_profile(root, "sup", "dsh", "nearai/Qwen/Qwen3.8-27B", &[]);
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent),
        EnvVar::remove("NEARAI_API_KEY"),
    ];
    let err = run_background(
        root,
        "near",
        None,
        RunOptions {
            mode: apb_engine::run_config::RunMode::AgentSupervised,
            ..RunOptions::default()
        },
    )
    .expect_err("the start is refused")
    .to_string();
    assert!(err.contains("cloud_key_missing"), "{err}");
    assert!(err.contains("project/sup"), "{err}");
    assert_eq!(runs(root), 0, "no run directory: {err}");
}

fn ephemeral(agent: &str, model: &str) -> apb_core::overrides::RunOverrides {
    apb_core::overrides::RunOverrides::from_yaml(&format!(
        "nodes:\n  w:\n    ephemeral_executor: {{ agent: {agent}, model: {model} }}\n"
    ))
    .unwrap()
}

/// A node's ephemeral executor is judged in place of its profile's chain:
/// an unknown NEAR model there refuses a start whose profile is local, and
/// a local override starts a node whose profile alone would be refused.
#[test]
fn an_ephemeral_override_is_judged_in_place_of_the_profile() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let agent = seed(dir.path(), "claude-code", "haiku", &[]);
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent),
        EnvVar::set("NEARAI_API_KEY", "fake-key-1234567"),
    ];
    let err = run(
        dir.path(),
        "near",
        None,
        RunOptions {
            overrides: Some(ephemeral("dsh", "nearai/does-not-exist")),
            ..RunOptions::default()
        },
    )
    .expect_err("the start is refused")
    .to_string();
    assert!(err.contains("cloud_model_unknown"), "{err}");
    assert!(err.contains("ephemeral/w"), "{err}");
    assert_eq!(runs(dir.path()), 0, "no run directory: {err}");
    drop(_env);
    drop(_l);

    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let agent = seed(dir.path(), "dsh", "nearai/Qwen/Qwen3.8-27B", &[]);
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent),
        EnvVar::remove("NEARAI_API_KEY"),
    ];
    run(
        dir.path(),
        "near",
        None,
        RunOptions {
            overrides: Some(ephemeral("claude-code", "haiku")),
            ..RunOptions::default()
        },
    )
    .expect("the start proceeds");
    assert_eq!(runs(dir.path()), 1);
}

/// A key variable an installed connector references is dropped from every
/// agent spawn, so it does not count as reaching the primary executor.
#[test]
fn a_key_the_connector_scrub_drops_is_refused() {
    let _l = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let agent = seed(root, "dsh", "nearai/Qwen/Qwen3.8-27B", &[]);
    let cdir = cfg.path().join("connectors/near-tool");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(
        cdir.join("connector.yaml"),
        "name: near-tool\nversion: 0.1.0\naccount_fields:\n  - { name: token, required: true, secret: true }\nfunctions:\n  - { name: ping, description: Ping, read_only: true, method: GET, url: \"https://example.com\" }\n",
    )
    .unwrap();
    let account = apb_core::connector::config::project_config_path(root, "near-tool");
    fs::create_dir_all(account.parent().unwrap()).unwrap();
    fs::write(
        &account,
        "accounts:\n  - name: a\n    token: \"{{env.NEARAI_API_KEY}}\"\n",
    )
    .unwrap();
    let _env = [
        EnvVar::set("APB_CONFIG_DIR", cfg.path()),
        EnvVar::set("APB_AGENT_CMD", agent),
        EnvVar::set("NEARAI_API_KEY", "fake-key-1234567"),
    ];
    assert_eq!(
        apb_core::connector::resolve::all_referenced_env_names(root),
        ["NEARAI_API_KEY"]
    );
    let Err(err) = run(root, "near", None, RunOptions::default()) else {
        panic!("the start is refused");
    };
    let err = err.to_string();
    assert!(err.contains("cloud_key_missing"), "{err}");
    assert_eq!(runs(root), 0, "no run directory: {err}");
}

#[test]
fn host_mode_skips_the_cloud_check() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let agent = seed(
        dir.path(),
        "dsh",
        "nearai/Qwen/Qwen3-VL-30B-A3B-Instruct",
        &[],
    );
    let _env = [
        EnvVar::set("APB_AGENT_CMD", agent),
        EnvVar::remove("NEARAI_API_KEY"),
    ];
    let run_id = run_background(
        dir.path(),
        "near",
        None,
        RunOptions {
            execution: ExecutionRequest {
                mode: Some(ExecutionMode::Host),
                host_session: true,
                client: Some("test-host".into()),
                inherited: false,
            },
            ..Default::default()
        },
    )
    .expect("host mode starts");
    let _ = apb_engine::stop::stop_run(dir.path(), &run_id);
}

#[test]
fn a_rebind_to_a_profile_on_a_model_without_tools_is_refused() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let _env = [EnvVar::set("NEARAI_API_KEY", "fake-key-1234567")];
    seed(dir.path(), "claude-code", "haiku", &[]);
    common::seed_profile(
        dir.path(),
        "vision",
        "dsh",
        "nearai/Qwen/Qwen3-VL-30B-A3B-Instruct",
        &[],
    );
    let refusal = apb_engine::gate::check_rebind(
        dir.path(),
        apb_core::profile_store::PlaybookOrigin::Project,
        "vision",
        apb_core::profile::ProfileScope::Project,
        true,
    )
    .unwrap_err();
    assert_eq!(refusal["policy"], "cloud_model_no_tools", "{refusal}");
    // The same profile on a model with tool calling rebinds.
    common::seed_profile(dir.path(), "good", "dsh", "nearai/Qwen/Qwen3.8-27B", &[]);
    apb_engine::gate::check_rebind(
        dir.path(),
        apb_core::profile_store::PlaybookOrigin::Project,
        "good",
        apb_core::profile::ProfileScope::Project,
        true,
    )
    .unwrap();
}
