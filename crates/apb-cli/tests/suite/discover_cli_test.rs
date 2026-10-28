//! `apb discover`: a discovery over a synthetic history root named in the
//! global config, its stored result listed and shown (absolute source
//! paths only with `--paths`), a suggestion saved and another declined, a
//! raw directory read through `--sessions`, and every subcommand refused
//! inside a run step. Every session file is made up here.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use predicates::prelude::*;

use crate::common::apb;

const SESSION: &str = include_str!("../../../apb-core/tests/fixtures/history/claude_session.jsonl");

/// A history under `base`: a project, six synthetic claude sessions of it
/// on two days (three with `bun test` for `cargo test`), and a config dir
/// naming their directory as the root `synthetic`. Returns the project
/// root, the claude directory and the config dir.
fn history(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let base = std::fs::canonicalize(base).unwrap();
    let project = base.join("work/proj");
    std::fs::create_dir_all(&project).unwrap();
    apb_core::registry::init_project(&project).unwrap();
    let claude = base.join("claude/projects");
    let dir = claude.join(apb_core::agent_actions::claude::encode_cwd(
        &project.to_string_lossy(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, age) in [1u32, 1, 2, 1, 1, 2].iter().enumerate() {
        let path = dir.join(format!("s{i}.jsonl"));
        let mut text = SESSION.replace(
            "__CWD__",
            serde_json::to_string(&project).unwrap().trim_matches('"'),
        );
        if i >= 3 {
            text = text.replace("cargo test", "bun test");
        }
        std::fs::write(&path, text).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(86_400) * *age)
            .unwrap();
    }
    let config = base.join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.yaml"),
        format!(
            "discover:\n  roots:\n    - {{ name: synthetic, agent: claude_code, path: {} }}\n",
            claude.display()
        ),
    )
    .unwrap();
    (project, claude, config)
}

fn discover(project: &Path, config: &Path) -> assert_cmd::Command {
    let mut cmd = apb();
    cmd.env("APB_CONFIG_DIR", config).current_dir(project);
    cmd.arg("discover");
    cmd
}

#[test]
fn a_discovery_is_shown_stored_saved_and_declined() {
    let base = tempfile::tempdir().unwrap();
    let (project, claude, config) = history(base.path());
    let base_text = std::fs::canonicalize(base.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let out = discover(&project, &config)
        .args(["--root", "synthetic"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "root synthetic (claude_code): 6 files read of 6",
        ))
        .stdout(predicate::str::contains("source: synthetic:proj/s"))
        .stdout(predicate::str::contains("effects: fs_write"))
        .stdout(predicate::str::contains(&base_text).not())
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(out).unwrap().contains("6 sessions"));

    let out = discover(&project, &config)
        .args(["--json", "--since", "30d"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["request"]["since_days"], 30);
    assert!(v.get("mac").is_none());
    let id = v["id"].as_str().unwrap().to_string();
    let slugs: Vec<String> = v["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(slugs.len() >= 2, "{v}");

    discover(&project, &config)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(&id));
    discover(&project, &config)
        .args(["show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains(&base_text).not());
    discover(&project, &config)
        .args(["show", &id, "--paths"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            claude.to_string_lossy().into_owned(),
        ));

    discover(&project, &config)
        .args(["save", &id, &slugs[0]])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "saved as draft playbook {}",
            slugs[0]
        )));
    discover(&project, &config)
        .args(["save", &id, &slugs[0]])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already saved"));
    discover(&project, &config)
        .args(["dismiss", &id, &slugs[1], "--hard"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "declined {} (hard)",
            slugs[1]
        )));
    discover(&project, &config)
        .args(["show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("[dismissed]"))
        .stdout(predicate::str::contains(format!("[saved as {}", slugs[0])));
}

#[test]
fn a_refused_save_exits_2_in_json_too() {
    let base = tempfile::tempdir().unwrap();
    let (project, _, config) = history(base.path());
    let out = discover(&project, &config).arg("--json").output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v["id"].as_str().unwrap();
    let s = &v["suggestions"][0];
    // A playbook with the suggestion's trigger already exists.
    apb_core::versioning::create_draft_in(
        &project.join(".apb"),
        "by-hand",
        s["yaml"].as_str().unwrap(),
        apb_core::profile_store::PlaybookOrigin::Project,
    )
    .unwrap();
    let out = discover(&project, &config)
        .args(["save", "--json", id, s["slug"].as_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["rejected"], "possible_duplicate");
}

#[test]
fn a_typed_sessions_directory_is_read_without_configured_roots() {
    let base = tempfile::tempdir().unwrap();
    let (project, claude, _) = history(base.path());
    let empty = tempfile::tempdir().unwrap();
    discover(&project, empty.path())
        .arg("--sessions")
        .arg(&claude)
        .args(["--agent", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "root @sessions (claude_code): 6 files read of 6",
        ));
    // Its path is not kept: `show --paths` says so instead of looking the
    // file up under a configured root.
    let out = discover(&project, empty.path())
        .arg("--sessions")
        .arg(&claude)
        .args(["--agent", "claude-code", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    discover(&project, empty.path())
        .args(["show", v["id"].as_str().unwrap(), "--paths"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "read from a --sessions directory: its path is not kept",
        ));
    // It is read instead of the configured roots, not beside them.
    discover(&project, empty.path())
        .arg("--sessions")
        .arg(&claude)
        .args(["--agent", "claude-code", "--root", "synthetic"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
    discover(&project, empty.path())
        .arg("--sessions")
        .arg(&claude)
        .args(["--agent", "cursor"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--agent takes claude-code or codex",
        ));
}

#[test]
fn every_discover_subcommand_is_refused_inside_a_run_step() {
    let base = tempfile::tempdir().unwrap();
    let (project, _, config) = history(base.path());
    for args in [
        vec![],
        vec!["list"],
        vec!["show", "disc-0123456789"],
        vec!["save", "disc-0123456789", "seq-0123456789"],
        vec!["dismiss", "disc-0123456789", "seq-0123456789"],
    ] {
        discover(&project, &config)
            .env("APB_RUN_ID", "r1")
            .args(&args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is refused inside run `r1`"));
    }
}

#[test]
fn show_paths_says_when_the_roots_do_not_load() {
    let base = tempfile::tempdir().unwrap();
    let (project, _, config) = history(base.path());
    let out = discover(&project, &config).arg("--json").output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    std::fs::write(config.join("config.yaml"), "discover:\n  since_days: 0\n").unwrap();
    discover(&project, &config)
        .args(["show", v["id"].as_str().unwrap(), "--paths"])
        .assert()
        .success()
        .stderr(predicate::str::contains("the history roots do not load"))
        .stdout(predicate::str::contains("(the history roots did not load)"))
        .stdout(predicate::str::contains("not found under a configured root").not());
}
