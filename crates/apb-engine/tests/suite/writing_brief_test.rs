//! The writing brief reaches exactly the prompts whose output people read:
//! an `agent_task` marked `audience: people`, the finish composer unless it
//! says `audience: agents`, and a node granted a function with a `people:`
//! block. Every other prompt carries none of it.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use apb_core::connector::ConnectorDoc;
use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_engine::manifest::ManifestConnectorGrant;
use apb_engine::people::brief::{WRITING_BRIEF, applies};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

const PLAYBOOK: &str = r#"
schema: 2
id: brief
name: Brief
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: pr, type: agent_task, prompt: "MARK_PR write the description", audience: people }
  - { id: code, type: agent_task, prompt: "MARK_CODE change the code" }
  - { id: done, type: finish, outcome: success, prompt: "MARK_DONE sum up" }
edges:
  - { from: start, to: pr }
  - { from: pr, to: code }
  - { from: code, to: done }
"#;

/// Runs the playbook with an agent that appends every prompt it gets to a
/// file, and returns each prompt by its marker.
fn prompts(playbook: &str) -> BTreeMap<&'static str, String> {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/brief/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), playbook).unwrap();
    fs::write(root.join(".apb/playbooks/brief/current"), "1.0.0").unwrap();
    common::seed_main(root);
    let log = root.join("prompts.txt");
    let agent = root.join("agent.sh");
    common::write_sync(
        &agent,
        &format!(
            "#!/bin/sh\nprintf '%s\\n=====\\n' \"$*\" >> '{}'\necho done\n",
            log.display()
        ),
    );
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _agent = EnvVar::set("APB_AGENT_CMD", &agent);
    let res = run(root, "brief", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let all = fs::read_to_string(log).unwrap();
    let mut out = BTreeMap::new();
    for marker in ["MARK_PR", "MARK_CODE", "MARK_DONE"] {
        let prompt = all
            .split("=====\n")
            .find(|p| p.contains(marker))
            .unwrap_or_else(|| panic!("no prompt carries {marker}"));
        out.insert(marker, prompt.to_string());
    }
    out
}

#[test]
fn the_brief_reaches_marked_nodes_and_the_closing_answer_only() {
    let got = prompts(PLAYBOOK);
    assert!(got["MARK_PR"].contains(WRITING_BRIEF));
    assert!(got["MARK_DONE"].contains(WRITING_BRIEF));
    assert!(!got["MARK_CODE"].contains("## writing for people"));
}

#[test]
fn a_closing_answer_for_agents_carries_no_brief() {
    let got = prompts(&PLAYBOOK.replace("sum up\" }", "sum up\", audience: agents }"));
    assert!(!got["MARK_DONE"].contains("## writing for people"));
}

const PEOPLE_CONNECTOR: &str = r#"
name: chat
version: 0.1.0
functions:
  - name: post
    description: Post a message
    method: POST
    url: https://chat.example.com/post
    body: "{{args}}"
    args_schema: { type: object, properties: { channel: { type: string }, title: { type: string }, body: { type: string } } }
    people: { to: channel, subject: title, text: body }
  - name: list
    description: List messages
    read_only: true
    method: GET
    url: https://chat.example.com/list
"#;

fn grant(functions: &[&str]) -> Vec<ManifestConnectorGrant> {
    vec![ManifestConnectorGrant {
        connector: "chat".into(),
        accounts: vec!["me".into()],
        functions: functions.iter().map(|f| f.to_string()).collect(),
        max_calls: None,
        on_unknown: None,
    }]
}

#[test]
fn a_node_granted_a_function_people_read_gets_the_brief_and_the_args_named() {
    let docs = BTreeMap::from([(
        "chat".to_string(),
        ConnectorDoc::from_yaml(PEOPLE_CONNECTOR, "chat").unwrap(),
    )]);
    let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
    let code = &pb.node("code").unwrap().kind;
    assert!(applies(code, &grant(&["post"]), &docs));
    assert!(!applies(code, &grant(&["list"]), &docs));
    assert!(!applies(code, &[], &docs));

    let block =
        apb_engine::connector::prompt::instruction_block(&grant(&["post", "list"]), &[], &docs);
    assert!(block.contains("people read: title, body"), "{block}");
    let read_only = apb_engine::connector::prompt::instruction_block(&grant(&["list"]), &[], &docs);
    assert!(!read_only.contains("people read"), "{read_only}");
}
