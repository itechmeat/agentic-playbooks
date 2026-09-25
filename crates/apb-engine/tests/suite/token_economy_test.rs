//! Token economy (issue #136): what a run hands its agents, measured at the
//! one boundary that decides the bill - the argv of every agent process a run
//! spawns. A recording stub stands in for the agent: each invocation writes
//! its node id and full argv to a log directory, then plays a scripted part
//! (big outputs, a reported failure, a deadline kill, a verdict), so the
//! tests read back exactly the prompts, resume flags and environment flags a
//! real agent would have received. No model is ever called.
//!
//! `token_budget_of_a_representative_run` is the bench: it runs one playbook
//! that exercises every waste path the issue names (growing context, a
//! retry, a require_verdict deadline kill, a loop) and prints the prompt bytes
//! sent per run (run with `--nocapture` to see the numbers).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

/// One recorded agent process: which node spawned it and its argv.
#[derive(Debug)]
pub struct Invocation {
    #[allow(dead_code)]
    pub node: String,
    pub args: Vec<String>,
}

impl Invocation {
    /// The value following `flag`, when present.
    pub fn flag(&self, flag: &str) -> Option<&str> {
        self.args
            .windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].as_str())
    }

    /// The prompt text handed over with `-p` (claude's and the stub's form).
    pub fn prompt(&self) -> &str {
        self.flag("-p").unwrap_or("")
    }
}

/// Writes the recording stub. `part` is shell run after recording, with
/// `$NODE` (the node id) and `$N` (this node's 1-based invocation count) set.
pub fn recording_stub(dir: &Path, part: &str) -> String {
    let log = dir.join("inv");
    fs::create_dir_all(&log).unwrap();
    let body = format!(
        r#"#!/bin/sh
LOG='{log}'
NODE="${{APB_NODE_ID:-none}}"
all=$(cat "$LOG/count" 2>/dev/null || echo 0); all=$((all+1)); echo $all > "$LOG/count"
N=$(cat "$LOG/count-$NODE" 2>/dev/null || echo 0); N=$((N+1)); echo $N > "$LOG/count-$NODE"
{{ printf '%s\0' "$NODE"; for a in "$@"; do printf '%s\0' "$a"; done; }} > "$LOG/$(printf %04d $all)"
{part}
"#,
        log = log.display()
    );
    let path = dir.join("stub.sh");
    common::write_sync(&path, &body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

/// Every recorded invocation, in spawn order.
pub fn invocations(dir: &Path) -> Vec<Invocation> {
    let log = dir.join("inv");
    let mut names: Vec<PathBuf> = fs::read_dir(&log)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|p| {
            let raw = fs::read(&p).unwrap();
            let mut parts: Vec<String> = raw
                .split(|b| *b == 0)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
            parts.pop(); // the trailing NUL leaves an empty last element
            let node = parts.remove(0);
            Invocation { node, args: parts }
        })
        .collect()
}

pub fn seed_playbook(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Runs `id` under the stub with the infrastructure backoff shortened, so a
/// retry path costs milliseconds instead of the production 5 s / 30 s.
pub fn run_with_stub(root: &Path, id: &str, stub: &str, instruction: Option<&str>) -> RunStatus {
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var(apb_engine::failure_class::BACKOFF_ENV, "1,1");
    }
    let opts = RunOptions {
        instruction: instruction.map(str::to_string),
        ..RunOptions::default()
    };
    let res = run(root, id, None, opts);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
        std::env::remove_var(apb_engine::failure_class::BACKOFF_ENV);
    }
    res.unwrap().outcome
}

/// A report block the adapter parses as the agent's self-assessed status.
const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";
/// A one-agent playbook whose template is `body`, for prompt-shape checks.
fn single(body: &str) -> String {
    format!(
        "schema: 2\nid: one\nname: One\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: {body:?} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

/// Item 5 of #136: a template that places the run context already carries the
/// run instruction (the context leads with it), so the trailing instruction
/// section must not repeat it. A template that places neither still receives
/// it exactly once through that trailing section.
#[test]
fn run_instruction_reaches_the_prompt_exactly_once() {
    const INSTRUCTION: &str = "Keep the public API unchanged and write every artifact in English.";
    for (template, why) in [
        (
            "Do the work.\n\n{{run.context}}",
            "template reads run.context",
        ),
        ("Do the work.", "template reads nothing"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        init_project(dir.path()).unwrap();
        seed_playbook(dir.path(), "one", &single(template));
        common::seed_main(dir.path());
        let stub = recording_stub(dir.path(), &format!("echo done; {OK}"));
        assert_eq!(
            run_with_stub(dir.path(), "one", &stub, Some(INSTRUCTION)),
            RunStatus::Succeeded
        );
        let inv = invocations(dir.path());
        let prompt = inv[0].prompt();
        assert_eq!(
            prompt.matches(INSTRUCTION).count(),
            1,
            "{why}: the instruction must appear once, got:\n{prompt}"
        );
    }
}
