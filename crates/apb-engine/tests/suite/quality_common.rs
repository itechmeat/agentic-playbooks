//! Shared fixtures of the run quality tests: a run directory built from a
//! journal, a playbook snapshot, a manifest and attempt transcripts, the
//! way the engine leaves one.

use std::fs;
use std::path::{Path, PathBuf};

/// The code fixture: a claude session that loads a skill, runs the same
/// failing command three times, edits inside its workdir and holds a
/// planted value in a tool result.
pub const CLAUDE_SESSION: &str = include_str!("../fixtures/quality/claude_skill_loop.jsonl");
/// The non-code fixture: an inbox triage run against a CRM connector.
pub const CRM_JOURNAL: &str = include_str!("../fixtures/quality/crm_run.jsonl");
pub const CRM_CONNECTOR: &str = include_str!("../fixtures/quality/crm_connector.yaml");

/// A token-shaped value, built at run time, planted where the fixture says
/// `PLANTED_VALUE`.
pub fn planted() -> String {
    format!("ghp_{}", "Zq8vR2kM".repeat(5))
}

/// A run directory under `root/.apb/runs/<id>` with `journal` as its
/// events and `snapshot` as its playbook.
pub fn run_dir(root: &Path, id: &str, journal: &str, snapshot: &str) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), journal).unwrap();
    fs::write(dir.join("playbook.yaml"), snapshot).unwrap();
    dir
}

/// Writes `content` as stream `file` of attempt `stem`.
pub fn attempt_file(run_dir: &Path, stem: &str, file: &str, content: &str) {
    let dir = run_dir.join("attempts").join(stem);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(file), content).unwrap();
}

/// The journal lines of a one-node agent run: `agent` ran attempt `fix-1`
/// in `workdir`; `finished_extra` goes into its `attempt_finished`.
pub fn agent_journal(agent: &str, workdir: &Path, finished_extra: &str, outcome: &str) -> String {
    // A JSON string: a Windows path's backslashes are escaped.
    let workdir = serde_json::to_string(&workdir.display().to_string()).unwrap();
    let status = if outcome == "succeeded" {
        "succeeded"
    } else {
        "failed"
    };
    [
        r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"fix-test","version":"1.0.0"}"#.to_string(),
        r#"{"seq":1,"ts":1001,"type":"node_started","node":"fix","attempt":1}"#.to_string(),
        format!(
            r#"{{"seq":2,"ts":1002,"type":"attempt_started","node":"fix","attempt":1,"agent":"{agent}","model":"m","workdir":{workdir},"transcript":"attempts/fix-1"}}"#
        ),
        format!(
            r#"{{"seq":3,"ts":1003,"type":"attempt_finished","node":"fix","attempt":1,"status":"{status}"{finished_extra}}}"#
        ),
        format!(
            r#"{{"seq":4,"ts":1004,"type":"node_finished","node":"fix","status":"{status}","attempt":1,"output":"done","artifacts":[]}}"#
        ),
        format!(r#"{{"seq":5,"ts":1005,"type":"run_finished","outcome":"{outcome}"}}"#),
        String::new(),
    ]
    .join("\n")
}

/// The snapshot of the one-node agent playbook with `quality` appended.
pub fn agent_playbook(quality: &str) -> String {
    format!(
        "schema: 2\nid: fix-test\nname: Fix test\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: fix, type: agent_task, prompt: fix it, profile: coder }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: fix }}\n  - {{ from: fix, to: done }}\n{quality}"
    )
}

/// A manifest binding node `fix` to profile `project/coder` with `skills`.
pub fn manifest_with_skills(run_dir: &Path, skills: &[&str]) {
    let yaml = format!(
        "profiles:\n  - scope: project\n    name: coder\n    profile_digest: sha256:p\n    bundle_digest: sha256:b\n    soul: ''\n    soul_requirement: any\n    skills: [{}]\n    chain: []\nnode_bindings:\n  fix: project/coder\n",
        skills
            .iter()
            .map(|s| format!("{{ name: {s}, scope: project, digest: 'sha256:{s}' }}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let manifest: apb_engine::manifest::RunExecutionManifest = yaml_serde::from_str(&yaml).unwrap();
    apb_engine::manifest::write(run_dir, &manifest).unwrap();
}
