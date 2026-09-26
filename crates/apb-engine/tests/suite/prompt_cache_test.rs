//! The prompt-cache invariant of the agent invocation (issue #67 item 7).
//!
//! Provider-side prompt caching only hits when the prefix of a request is
//! byte-identical to an earlier one. For an agent CLI the prefix is its
//! system portion: the SOUL, the settings it loads and the directories it is
//! pointed at. So for two nodes bound to the same profile in a run, every
//! argument except the prompt itself and the session id must be identical,
//! and the node's own rendered task must open the prompt, with everything the
//! engine appends per run coming after it.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{
    Invocation, invocations, recording_stub, run_with_stub, seed_playbook,
};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

const PLAYBOOK: &str = r#"schema: 2
id: two
name: Two
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: first, type: agent_task, prompt: "First task for {{params.topic}}." }
  - { id: second, type: agent_task, prompt: "Second task.", outputs: { fields: [verdict] } }
  - { id: done, type: finish, outcome: success }
params:
  - { name: topic, type: text, default: caching }
edges:
  - { from: start, to: first }
  - { from: first, to: second }
  - { from: second, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    seed_playbook(root, "two", PLAYBOOK);
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    let profile = root.join(".apb/profiles/main/profile.yaml");
    let yaml = fs::read_to_string(&profile).unwrap();
    fs::write(&profile, format!("{yaml}skills: [probe]\n")).unwrap();
    fs::write(root.join(".apb/profiles/main/SOUL.md"), "You are careful.").unwrap();
    let skill = root.join(".agents/skills/probe");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "---\nname: probe\n---\nthe skill\n").unwrap();
}

/// The system-portion arguments of one invocation: everything but the
/// prompt (after `--`) and the per-attempt session id.
fn system_portion(inv: &Invocation) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = inv.args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--" => break,
            "--session-id" => {
                args.next();
                out.push("--session-id <id>".to_string());
            }
            // One settings file per node (parallel nodes would race on one
            // path); the CLI reads its content, which is compared below.
            "--settings" => {
                args.next();
                out.push("--settings <file>".to_string());
            }
            _ => out.push(a.clone()),
        }
    }
    out
}

#[test]
fn nodes_of_one_profile_share_a_byte_identical_system_portion() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // The first node scribbles over its skills copy: the second must still
    // get the snapshot content, at the same path.
    let stub = recording_stub(
        dir.path(),
        &format!(
            "prev=''; for a in \"$@\"; do if [ \"$prev\" = --add-dir ]; then [ \"$NODE\" = first ] && echo tampered >> \"$a/.agents/skills/probe/SKILL.md\"; fi; prev=\"$a\"; done; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    assert_eq!(inv.len(), 2);
    assert_eq!(
        system_portion(&inv[0]),
        system_portion(&inv[1]),
        "the system portion must not depend on the node"
    );
    let settings: Vec<Vec<u8>> = inv
        .iter()
        .map(|i| fs::read(i.flag("--settings").expect("minimal settings")).unwrap())
        .collect();
    assert_eq!(settings[0], settings[1], "settings content is fixed");
    let skills = inv[1]
        .flag("--add-dir")
        .expect("minimal claude gets skills");
    let skill =
        fs::read_to_string(Path::new(skills).join(".agents/skills/probe/SKILL.md")).unwrap();
    assert!(
        !skill.contains("tampered"),
        "the copy is laid back from the snapshot: {skill}"
    );
    // The SOUL travels in the system portion, not in the prompt.
    assert!(inv[1].args.iter().any(|a| a == "You are careful."));
    // The node's own task opens the prompt; what the engine adds follows it.
    assert!(
        inv[0].prompt().starts_with("First task for caching."),
        "{}",
        inv[0].prompt()
    );
    assert!(
        inv[1].prompt().starts_with("Second task."),
        "{}",
        inv[1].prompt()
    );
}
