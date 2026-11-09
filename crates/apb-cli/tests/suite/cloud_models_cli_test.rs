//! The cloud catalog on the CLI: `apb models --json` lists the embedded
//! catalog with no network, is refused inside a run step; `apb validate`
//! and `apb profile write` judge a NEAR model by the catalog.

use std::fs;
use std::path::Path;
use std::process::Output;

fn apb(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = crate::common::apb_std();
    cmd.args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .env_remove("APB_RUN_ID")
        .env_remove("APB_HOST_TASK")
        .env_remove("NEARAI_API_KEY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cfg")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    let out = apb(dir.path(), &["init"], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

#[test]
fn models_json_lists_the_embedded_catalog_and_is_refused_in_a_step() {
    let dir = setup();
    let out = apb(
        dir.path(),
        &["models", "--json"],
        &[("NEARAI_API_KEY", "fake-key-1234567")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("fake-key-1234567"));
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let near = &v["cloud"][0];
    assert_eq!(near["source"], "embedded");
    assert_eq!(near["key_present"], true);
    assert!(
        near["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "Qwen/Qwen3.8-27B" && m["privacy_class"] == "confidential_tee")
    );
    let out = apb(dir.path(), &["models", "--json"], &[("APB_RUN_ID", "r1")]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("inside run `r1`"));
}

fn seed_profile(dir: &Path, name: &str, yaml_executor: &str) {
    let p = dir.join(".apb/profiles").join(name);
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("profile.yaml"),
        format!("name: {name}\ndescription: t\nexecutor:\n{yaml_executor}"),
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "").unwrap();
}

#[test]
fn validate_judges_a_near_model_by_the_catalog() {
    let dir = setup();
    seed_profile(
        dir.path(),
        "typo",
        "  agent: dsh\n  model: nearai/Qwen/Qwen3.8-72B\n",
    );
    seed_profile(
        dir.path(),
        "vision",
        "  agent: dsh\n  model: nearai/Qwen/Qwen3-VL-30B-A3B-Instruct\n",
    );
    seed_profile(
        dir.path(),
        "mixed",
        "  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\n  fallbacks:\n    - { agent: dsh, model: nearai/anthropic/claude-sonnet-5-5 }\n",
    );
    for (id, profile) in [("see", "vision"), ("mix", "mixed")] {
        let v = dir.path().join(format!(".apb/playbooks/{id}/1.0.0"));
        fs::create_dir_all(&v).unwrap();
        fs::write(
            v.join("playbook.yaml"),
            format!("schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: x, profile: {profile} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"),
        )
        .unwrap();
        fs::write(
            dir.path().join(format!(".apb/playbooks/{id}/current")),
            "1.0.0",
        )
        .unwrap();
    }
    // An empty PATH: no agent CLI is probed.
    let empty = dir.path().join("home");
    let out = apb(
        dir.path(),
        &["validate"],
        &[("PATH", empty.to_str().unwrap())],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("profile typo: warning cloud_model_unknown"),
        "{stdout}"
    );
    assert!(
        stdout.contains("profile vision: error cloud_model_no_tools"),
        "{stdout}"
    );
    assert!(stdout.contains("no tool calling"), "{stdout}");
    assert!(
        stdout.contains("profile mixed: warning profile_privacy_class_changes"),
        "{stdout}"
    );
    assert!(stdout.contains("see: warning V220"), "{stdout}");
    assert!(stdout.contains("mix: warning V221"), "{stdout}");
    assert!(
        !out.status.success(),
        "a blocking cloud issue fails validate"
    );
}

#[test]
fn profile_write_refuses_a_model_without_tools_and_warns_on_an_unknown_one() {
    let dir = setup();
    let write = |name: &str, model: &str| {
        apb(
            dir.path(),
            &["profile", "write", name, "--agent", "dsh", "--model", model],
            &[],
        )
    };
    let out = write("vision", "nearai/Qwen/Qwen3-VL-30B-A3B-Instruct");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cloud_model_no_tools"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.path().join(".apb/profiles/vision").exists());
    let out = write("effort", "nearai/deepseek/deepseek-v3.2@high");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cloud_effort_unsupported"));
    let out = write("typo", "nearai/Qwen/Qwen3.8-72B");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("cloud_model_unknown"),
        "{v}"
    );
}
