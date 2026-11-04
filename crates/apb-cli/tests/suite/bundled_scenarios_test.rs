//! Every shipped default playbook carries scenarios, and every one of them
//! passes at `--level quick` through the binary: each playbook is laid out
//! in a temp project as a version with its `evals/`, the role profiles are
//! seeded, and every connector it binds is installed from the repository's
//! `connectors/` and approved with a fake `default` account, so the version
//! validates exactly as it does once a person set the connector up. A new
//! default without a suite, or a change to a default that breaks one of its
//! routes, fails here.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The fewest scenarios a shipped playbook may carry.
const MIN_SCENARIOS: usize = 2;

/// Runs `apb <args>` in `dir` with its own config dir and no inherited run
/// context.
fn apb(dir: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .env_remove("APB_RUN_DIR")
        .env_remove("APB_NODE_ID")
        .env_remove("APB_RUN_ID")
        .output()
        .unwrap()
}

fn apb_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = apb(dir, args);
    assert!(
        out.status.success(),
        "`apb {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The shipped playbook folders, by id.
fn shipped() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(repo_root().join("bundled/playbooks"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("playbook.yaml").is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort();
    out
}

/// A fake `default` account per connector a shipped playbook binds. A new
/// connector in a default needs its line here.
fn fake_account(name: &str) -> &'static str {
    match name {
        "github" => {
            "accounts:\n  - name: default\n    api_base: https://api.github.com\n    token: \"{{env.APB_TEST_GITHUB_TOKEN}}\"\n"
        }
        "imap" => {
            "accounts:\n  - name: default\n    host: imap.example.com\n    port: \"993\"\n    use_tls: \"true\"\n    auth_method: password\n    username: mailbox@example.com\n    password: \"{{env.APB_TEST_IMAP_PASSWORD}}\"\n"
        }
        other => panic!("a shipped playbook binds connector `{other}`: add a fake account for it"),
    }
}

/// Every connector a playbook binds: node grants and item checks.
fn bound_connectors(yaml: &str) -> BTreeSet<String> {
    let doc: serde_json::Value = yaml_serde::from_str(yaml).unwrap();
    let mut out = BTreeSet::new();
    for node in doc["nodes"].as_array().into_iter().flatten() {
        for c in node["connectors"].as_array().into_iter().flatten() {
            if let Some(n) = c["name"].as_str() {
                out.insert(n.to_string());
            }
        }
        if let Some(n) = node["list"]["check"]["connector"]["name"].as_str() {
            out.insert(n.to_string());
        }
    }
    out
}

fn install_and_approve(root: &Path, name: &str) {
    let src = repo_root().join("connectors").join(name);
    apb_ok(
        root,
        &["connector", "install", "--from-dir", src.to_str().unwrap()],
    );
    let cfg = root
        .join(".apb/connector-config")
        .join(format!("{name}.yaml"));
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(&cfg, fake_account(name)).unwrap();
    apb_ok(root, &["connector", "approve", name]);
    apb_ok(
        root,
        &["connector", "approve", name, "--account", "default"],
    );
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&to).unwrap();
            copy_tree(&entry.path(), &to);
        } else {
            fs::create_dir_all(dst).unwrap();
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A project with the role profiles the defaults name.
fn project(dir: &Path) -> PathBuf {
    let root = dir.to_path_buf();
    fs::create_dir_all(root.join("cfg")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    apb_core::registry::init_project(&root).unwrap();
    for name in ["apb-main", "apb-reviewer", "apb-researcher"] {
        let p = root.join(".apb/profiles").join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("profile.yaml"),
            format!(
                "name: {name}\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n"
            ),
        )
        .unwrap();
        fs::write(p.join("SOUL.md"), "test soul").unwrap();
    }
    root
}

/// Lays the shipped playbook `id` out as its version, made current, with
/// its `evals/` beside it.
fn lay_out(root: &Path, id: &str, src: &Path) {
    let yaml = fs::read_to_string(src.join("playbook.yaml")).unwrap();
    let doc: serde_json::Value = yaml_serde::from_str(&yaml).unwrap();
    let version = doc["version"].as_str().unwrap();
    let pb = root.join(".apb/playbooks").join(id);
    fs::create_dir_all(pb.join(version)).unwrap();
    fs::write(pb.join(version).join("playbook.yaml"), &yaml).unwrap();
    fs::write(pb.join("current"), version).unwrap();
    if src.join("evals").is_dir() {
        copy_tree(&src.join("evals"), &pb.join("evals"));
    }
}

fn scenario_names(src: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(src.join("evals/scenarios"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().and_then(|x| x.to_str()) == Some("yaml"))
                        .then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Every shipped playbook has at least two scenarios, validates with its
/// connectors set up, and every scenario passes at quick without reaching
/// a scenario guard.
#[test]
fn every_shipped_scenario_passes_at_quick() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(dir.path());
    let defaults = shipped();
    assert!(!defaults.is_empty(), "no shipped playbook found");
    let mut installed = BTreeSet::new();
    for (id, src) in &defaults {
        let names = scenario_names(src);
        assert!(
            names.len() >= MIN_SCENARIOS,
            "{id} ships {} scenario(s), at least {MIN_SCENARIOS} are required",
            names.len()
        );
        let yaml = fs::read_to_string(src.join("playbook.yaml")).unwrap();
        for c in bound_connectors(&yaml) {
            if installed.insert(c.clone()) {
                install_and_approve(&root, &c);
            }
        }
        lay_out(&root, id, src);
        let out = apb_ok(&root, &["validate", id]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(&format!("{id}: OK")), "{id}: {stdout}");

        let out = apb(&root, &["eval", id, "--level", "quick", "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{id}: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let reports = v["reports"].as_array().unwrap();
        let played: Vec<String> = reports
            .iter()
            .map(|r| r["scenario"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(played, names, "{id}: every scenario plays");
        for r in reports {
            let name = r["scenario"].as_str().unwrap_or_default();
            assert_eq!(r["verdict"], "passed", "{id}/{name}: {r:#}");
            assert!(
                !r["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("scenario_guard"),
                "{id}/{name} reached a scenario guard: {r:#}"
            );
        }
        assert!(out.status.success(), "{id}: exit {:?}", out.status.code());
    }
}
