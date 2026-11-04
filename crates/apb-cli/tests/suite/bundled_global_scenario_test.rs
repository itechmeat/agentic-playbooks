//! A shipped default's scenario, played where setup installed it: `apb
//! setup apply` puts `apb-research` with its `evals/scenarios/` in the
//! global store, and `apb eval apb-research --scope global --level quick`
//! plays its scenario `answered` from a project, the run landing in that
//! project's `.apb/scenario-runs/`. A smoke run needs the version trusted
//! by digest: the pristine copy is trusted as shipped, an edited one is
//! not.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

/// A machine of its own: HOME, config dir, and a `PATH` holding only a
/// stub `codex`, so setup has one usable agent and touches no real host.
struct Machine {
    home: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl Machine {
    fn new() -> Machine {
        let m = Machine {
            home: tempfile::tempdir().unwrap(),
            bin: tempfile::tempdir().unwrap(),
        };
        let codex = m.bin.path().join("codex");
        fs::write(
            &codex,
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'codex-cli 0.157.0' ;;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
        m
    }

    fn config(&self) -> PathBuf {
        self.home.path().join("config")
    }

    fn project(&self) -> PathBuf {
        self.home.path().join("project")
    }

    fn apb_in(&self, dir: &Path, args: &[&str]) -> Output {
        crate::common::apb_std()
            .args(args)
            .current_dir(dir)
            .env("HOME", self.home.path())
            .env("PATH", self.bin.path())
            .env("APB_CONFIG_DIR", self.config())
            .env("APB_NO_REGISTRY", "1")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("APB_RUN_ID")
            .env_remove("APB_HOST_TASK")
            .env("APB_PROBE_TIMEOUT_MS", "5000")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    fn json_in(&self, dir: &Path, args: &[&str]) -> (Value, Option<i32>) {
        let out = self.apb_in(dir, args);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (v, out.status.code())
    }

    /// `apb setup plan` and `apply` of exactly that plan.
    fn setup(&self) {
        let (plan, code) = self.json_in(self.home.path(), &["setup", "plan", "--json"]);
        assert_eq!(code, Some(0), "{plan}");
        let digest = plan["digest"].as_str().unwrap();
        let (applied, code) = self.json_in(
            self.home.path(),
            &["setup", "apply", "--plan", digest, "--json"],
        );
        assert_eq!(code, Some(0), "{applied}");
    }

    /// The installed current version's `playbook.yaml` of `apb-research`.
    fn installed(&self) -> PathBuf {
        let pb = self.config().join("playbooks/apb-research");
        let current = fs::read_to_string(pb.join("current")).unwrap();
        pb.join(current.trim()).join("playbook.yaml")
    }
}

#[test]
fn the_shipped_research_scenario_plays_from_the_global_store() {
    let m = Machine::new();
    m.setup();
    let scenario = m
        .config()
        .join("playbooks/apb-research/evals/scenarios/answered.yaml");
    assert!(scenario.is_file(), "setup did not install the scenario");
    let project = m.project();
    apb_core::registry::init_project(&project).unwrap();

    let play = |level: &str, extra: &[&str]| {
        let mut args = vec![
            "eval",
            "apb-research",
            "--scope",
            "global",
            "--level",
            level,
            "--scenario",
            "answered",
            "--json",
        ];
        args.extend_from_slice(extra);
        m.json_in(&project, &args)
    };

    let (out, code) = play("quick", &[]);
    let report = &out["reports"][0];
    assert_eq!(
        (code, report["verdict"].as_str()),
        (Some(0), Some("passed")),
        "{out:#}"
    );
    let run = report["run_id"].as_str().unwrap();
    assert!(
        project
            .join(".apb/scenario-runs")
            .join(run)
            .join("report.json")
            .is_file(),
        "{out:#}"
    );

    // The pristine default carries the shipped digest: smoke passes the
    // trust check. Without a decision provider it stops later, before any
    // request leaves.
    let (out, _) = play("smoke", &["--yes", "--max-decision-requests", "3"]);
    assert_ne!(out["error"], "untrusted", "{out:#}");

    // An edited copy no longer carries the shipped digest: smoke refuses it.
    let yaml = m.installed();
    let text = fs::read_to_string(&yaml).unwrap();
    fs::write(&yaml, format!("{text}# edited locally\n")).unwrap();
    let (out, code) = play("smoke", &["--yes", "--max-decision-requests", "3"]);
    assert_eq!(code, Some(2), "{out:#}");
    assert_eq!(out["error"], "untrusted", "{out:#}");
}
