//! The output contract of typed declared fields (spec node-contracts D1).
//!
//! A typed entry of `outputs.fields` (or a list's `item_outputs`) is checked
//! when the attempt reports success: a missing required field or a value of
//! the wrong type rejects the attempt through the non-fatal rejection path,
//! so `rejected_for` names it and the retry briefing quotes it. A recording
//! stub stands in for the agent.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{invocations, recording_stub, run_with_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// The single run directory under `root`.
fn run_dir(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("one run")
}

/// A one-node playbook whose agent declares `fields` and may retry.
fn playbook(fields: &str, retries: u32) -> String {
    format!(
        r#"schema: 2
id: oc
name: OC
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: w, type: agent_task, prompt: "do", max_retries: {retries}, outputs: {{ fields: {fields} }} }}
  - {{ id: done, type: finish, outcome: success }}
  - {{ id: bad, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: w }}
  - {{ from: w, to: done, condition: {{ type: node_status, node: w, equals: success }} }}
  - {{ from: w, to: bad, condition: {{ type: node_status, node: w, equals: failure }} }}
"#
    )
}

fn seed(root: &Path, fields: &str, retries: u32) {
    init_project(root).unwrap();
    seed_playbook(root, "oc", &playbook(fields, retries));
    common::seed_main(root);
}

/// The stub part that writes a status file with `outputs` and reports success.
fn status_outputs(outputs: &str) -> String {
    format!("printf '%s' '{outputs}' > \"$APB_STATUS_FILE\"; {OK}")
}

/// Every `rejected_for` of `node`'s failed attempts, in order.
fn rejected_for(root: &Path, node: &str) -> Vec<String> {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n,
                status,
                rejected_for,
                ..
            } if n == node && status == "failed" => rejected_for.clone(),
            _ => None,
        })
        .collect()
}

/// A2.3.1: a word where a number was declared is rejected; the retry briefing
/// carries the reason; the corrected second attempt succeeds.
#[test]
fn a_word_where_a_number_was_declared_is_rejected_and_the_retry_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "[{ name: blocking, type: number, required: true }]",
        1,
    );
    let part = format!(
        "if [ \"$N\" = 1 ]; then {}; else {}; fi",
        status_outputs("{\"status\":\"success\",\"outputs\":{\"blocking\":\"two\"}}"),
        status_outputs("{\"status\":\"success\",\"outputs\":{\"blocking\":0}}"),
    );
    let stub = recording_stub(dir.path(), &part);
    assert_eq!(
        run_with_stub(dir.path(), "oc", &stub, None),
        RunStatus::Succeeded
    );
    assert_eq!(
        invocations(dir.path())
            .iter()
            .filter(|i| i.node == "w")
            .count(),
        2
    );
    let rejected = rejected_for(dir.path(), "w");
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0].starts_with("output_contract:"),
        "{}",
        rejected[0]
    );
    assert!(rejected[0].contains("blocking"), "{}", rejected[0]);
    let second = invocations(dir.path())
        .iter()
        .filter(|i| i.node == "w")
        .nth(1)
        .unwrap()
        .prompt()
        .to_string();
    assert!(second.contains("must be a number"), "{second}");
}

/// A2.3.2: the same wrong value on both attempts spends the retries and the
/// node fails with an output that starts with the contract reason.
#[test]
fn the_same_wrong_value_on_both_attempts_fails_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "[{ name: blocking, type: number, required: true }]",
        1,
    );
    let stub = recording_stub(
        dir.path(),
        &status_outputs("{\"status\":\"success\",\"outputs\":{\"blocking\":\"two\"}}"),
    );
    assert_eq!(
        run_with_stub(dir.path(), "oc", &stub, None),
        RunStatus::Failed
    );
    assert_eq!(rejected_for(dir.path(), "w").len(), 2);
    let events = read_all(&run_dir(dir.path())).unwrap();
    let out = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "w" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(out.starts_with("output_contract:"), "{out}");
}

/// A2.3.3: the recovery path (a written success verdict on a non-zero exit)
/// rejects a bad field the same way.
#[test]
fn the_recovery_path_rejects_a_bad_field_too() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "[{ name: blocking, type: number, required: true }]",
        0,
    );
    let part = format!(
        "{}\nexit 1",
        status_outputs("{\"status\":\"success\",\"outputs\":{\"blocking\":\"two\"}}")
    );
    let stub = recording_stub(dir.path(), &part);
    assert_eq!(
        run_with_stub(dir.path(), "oc", &stub, None),
        RunStatus::Failed
    );
    assert_eq!(
        invocations(dir.path())
            .iter()
            .filter(|i| i.node == "w")
            .count(),
        1
    );
    let rejected = rejected_for(dir.path(), "w");
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0].starts_with("output_contract:"),
        "{}",
        rejected[0]
    );
}

/// A2.3.4: a script node's stdout is checked against its declared fields; a
/// word where a number was declared fails the node with the reason.
#[test]
fn a_script_printing_the_wrong_type_fails_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(
        dir.path(),
        "oc",
        r#"schema: 2
id: oc
name: OC
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: scripts/count.sh, runner: sh, outputs: { fields: [{ name: count, type: number, required: true }] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#,
    );
    let vdir = dir.path().join(".apb/playbooks/oc/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("count.sh"), "echo '{\"count\": \"many\"}'\n").unwrap();
    common::seed_main(dir.path());
    assert_eq!(
        run_with_stub(dir.path(), "oc", "/bin/true", None),
        RunStatus::Failed
    );
    let events = read_all(&run_dir(dir.path())).unwrap();
    let out = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "s" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(out.starts_with("output_contract:"), "{out}");
    assert!(out.contains("count"), "{out}");
}

/// A2.3.5: a scenario run whose scripted script answer prints the wrong type
/// fails the node the same way (the scripted answer is checked too).
#[test]
fn a_scripted_scenario_answer_is_checked_too() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;

    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(
        dir.path(),
        "oc",
        r#"schema: 2
id: oc
name: OC
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: scripts/count.sh, runner: sh, outputs: { fields: [{ name: count, type: number, required: true }] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#,
    );
    let vdir = dir.path().join(".apb/playbooks/oc/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("count.sh"), "echo '{\"count\": \"many\"}'\n").unwrap();
    common::seed_main(dir.path());
    let scenario_yaml = r#"schema: 1
id: oc
instruction: count
answers:
  s:
    - { visit: 1, script: { exit: 0, stdout: '{"count": "many"}' } }
"#;
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario_yaml).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:oc".into(),
        max_requests: 20,
        sent: Default::default(),
    };
    let quick = run_scenario(dir.path(), "oc", None, &prep).unwrap();
    let events = read_all(&quick.run_dir).unwrap();
    let out = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "s" => Some(output.clone()),
            _ => None,
        })
        .expect("the script node finishes");
    assert!(out.starts_with("output_contract:"), "{out}");
}

/// A2.3.6: a list item's step is checked against `item_outputs`, never
/// against the node's own `outputs.fields`. The node declares nothing; a
/// good number passes, a word needs a look with the contract reason.
#[test]
fn a_list_item_is_checked_against_item_outputs() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(
        dir.path(),
        "oc",
        r#"schema: 2
id: oc
name: OC
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - id: l
    type: agent_task
    prompt: "do the item"
    list:
      items:
        inline:
          - { id: one }
          - { id: two }
      item_outputs: [{ name: n, type: number, required: true }]
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: l }
  - { from: l, to: done }
"#,
    );
    common::seed_main(dir.path());
    let part = format!(
        "if [ \"$APB_ITEM_ID\" = two ]; then {}; else {}; fi",
        status_outputs("{\"status\":\"success\",\"outputs\":{\"n\":\"many\"}}"),
        status_outputs("{\"status\":\"success\",\"outputs\":{\"n\":1}}"),
    );
    let stub = recording_stub(dir.path(), &part);
    assert_eq!(
        run_with_stub(dir.path(), "oc", &stub, None),
        RunStatus::Succeeded
    );
    let events = read_all(&run_dir(dir.path())).unwrap();
    let rejected: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                item: Some(_),
                rejected_for,
                ..
            } => rejected_for.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert!(
        rejected[0].starts_with("output_contract:"),
        "{}",
        rejected[0]
    );
    let settled: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled { item, state, .. } => {
                Some((item.clone(), state.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(settled.contains(&("one".into(), "passed".into())));
    assert!(settled.contains(&("two".into(), "needs_look".into())));
}

/// A list `script` item is checked against the list's `item_outputs`,
/// not the node's fields: the item that prints a word where a number was
/// declared needs a look, the other passes.
#[test]
fn a_list_script_item_is_checked_against_item_outputs() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(
        dir.path(),
        "oc",
        r#"schema: 2
id: oc
name: OC
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - id: l
    type: script
    script: scripts/item.sh
    runner: sh
    list:
      items:
        inline:
          - { id: one }
          - { id: two }
      item_outputs: [{ name: n, type: number, required: true }]
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: l }
  - { from: l, to: done }
"#,
    );
    let vdir = dir.path().join(".apb/playbooks/oc/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("item.sh"),
        "if [ \"$APB_ITEM_ID\" = two ]; then echo '{\"n\": \"many\"}'; else echo '{\"n\": 1}'; fi\n",
    )
    .unwrap();
    common::seed_main(dir.path());
    run_with_stub(dir.path(), "oc", "/bin/true", None);
    let settled: Vec<(String, String)> = read_all(&run_dir(dir.path()))
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled { item, state, .. } => {
                Some((item.clone(), state.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(
        settled.contains(&("one".into(), "passed".into())),
        "{settled:?}"
    );
    assert!(
        settled.contains(&("two".into(), "needs_look".into())),
        "{settled:?}"
    );
}

/// A2.3.7: an optional typed field left out succeeds and journals the
/// `output_fields_missing` warning as before.
#[test]
fn an_optional_typed_field_left_out_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "[{ name: blocking, type: number, required: true }, { name: notes, type: string }]",
        0,
    );
    let stub = recording_stub(
        dir.path(),
        &status_outputs("{\"status\":\"success\",\"outputs\":{\"blocking\":1}}"),
    );
    assert_eq!(
        run_with_stub(dir.path(), "oc", &stub, None),
        RunStatus::Succeeded
    );
    let events = read_all(&run_dir(dir.path())).unwrap();
    let missing: Vec<Vec<String>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::OutputFieldsMissing { node, fields } if node == "w" => {
                Some(fields.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(missing, vec![vec!["notes".to_string()]]);
    assert!(rejected_for(dir.path(), "w").is_empty());
}

/// A2.3.8: a configured connector secret inside a 200-character value of a
/// `number` field never appears in the journaled rejection reason, the
/// journaled rejected output or the retry prompt, in any 8-character run
/// (D1 wrong implementation 9).
#[test]
fn a_secret_inside_the_offending_value_never_leaks() {
    use apb_core::connector::{config, store};
    use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};

    const CONNECTOR: &str = "mock-tracker";
    const TOKEN_VAR: &str = "APB_OC_TOKEN";
    const SECRET: &str = "Qv7Zx9Kp2WtR4mNb8sDf0cJu3e";

    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    seed(
        dir.path(),
        "[{ name: blocking, type: number, required: true }]",
        1,
    );
    // Bind the connector on the node, install it into the config dir, resolve
    // its token from the project secret, and approve both as installed.
    let vdir = dir.path().join(".apb/playbooks/oc/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&vdir)
        .unwrap()
        .replace(
            "prompt: \"do\", max_retries: 1,",
            "prompt: \"do\", max_retries: 1, connectors: [{ name: mock-tracker, functions: read_only }],",
        );
    fs::write(&vdir, yaml).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connectors")
        .join(CONNECTOR);
    let dst = cfg.path().join("connectors").join(CONNECTOR);
    fs::create_dir_all(&dst).unwrap();
    fs::copy(src.join("connector.yaml"), dst.join("connector.yaml")).unwrap();
    let accounts = config::project_config_path(dir.path(), CONNECTOR);
    fs::create_dir_all(accounts.parent().unwrap()).unwrap();
    fs::write(
        &accounts,
        format!(
            "accounts:\n  - name: me\n    default: true\n    base_url: http://127.0.0.1:9\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".apb")).unwrap();
    fs::write(
        dir.path().join(".apb/secrets.env"),
        format!("{TOKEN_VAR}={SECRET}\n"),
    )
    .unwrap();
    let mut trust = TrustStore::load();
    let loaded = store::load(CONNECTOR).unwrap();
    trust
        .approve_kind(
            &loaded.digest,
            CONNECTOR,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let merged = config::load_merged(dir.path(), CONNECTOR).unwrap();
    let digest = config::account_digest(&merged[0]);
    trust
        .approve_kind(
            &digest,
            &account_trust_id(CONNECTOR, "me"),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();

    // The offending value: 200 characters holding the secret in the middle.
    let value = format!(
        "{}{}{}",
        "x".repeat(90),
        SECRET,
        "y".repeat(200 - 90 - SECRET.len())
    );
    let part = format!(
        "if [ \"$N\" = 1 ]; then {}; else {}; fi",
        status_outputs(&format!(
            "{{\"status\":\"success\",\"outputs\":{{\"blocking\":\"{value}\"}}}}"
        )),
        status_outputs("{\"status\":\"success\",\"outputs\":{\"blocking\":0}}"),
    );
    let stub = recording_stub(dir.path(), &part);
    // run_with_stub would take the env lock this test already holds: run
    // inline under the lock instead.
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &stub);
        std::env::set_var(apb_engine::failure_class::BACKOFF_ENV, "1,1");
    }
    // The connector permit maps the gate would hand the engine verbatim.
    let pb_yaml =
        fs::read_to_string(dir.path().join(".apb/playbooks/oc/1.0.0/playbook.yaml")).unwrap();
    let pb = apb_core::schema::Playbook::from_yaml(&pb_yaml).unwrap();
    let resolved = apb_core::connector::resolve::resolve_playbook(dir.path(), &pb).unwrap();
    let mut expected_connectors = std::collections::BTreeMap::new();
    let mut expected_accounts = std::collections::BTreeMap::new();
    for (name, r) in &resolved.connectors {
        expected_connectors.insert(name.clone(), r.loaded.digest.clone());
        for a in &r.accounts {
            expected_accounts.insert(
                format!("{name}/{}", a.name),
                apb_core::connector::config::account_digest(a),
            );
        }
    }
    let opts = apb_engine::scheduler::RunOptions {
        expected_connectors,
        expected_connector_accounts: expected_accounts,
        ..apb_engine::scheduler::RunOptions::default()
    };
    let res = apb_engine::scheduler::run(dir.path(), "oc", None, opts).unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
        std::env::remove_var(apb_engine::failure_class::BACKOFF_ENV);
    }
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let leak = |text: &str| (0..=SECRET.len() - 8).any(|i| text.contains(&SECRET[i..i + 8]));
    for reason in rejected_for(dir.path(), "w") {
        assert!(!leak(&reason), "the reason leaks the secret: {reason}");
    }
    // The rejected attempt's discarded output is journaled too
    // (`rejected_output`): the journal file holds no fragment of it.
    let journal = fs::read_to_string(run_dir(dir.path()).join("events.jsonl")).unwrap();
    assert!(
        journal.contains("rejected_output"),
        "the rejection kept its discarded output"
    );
    assert!(!leak(&journal), "the journal leaks the secret");
    for i in invocations(dir.path()).iter().filter(|i| i.node == "w") {
        if leak(i.prompt()) {
            let pr = i.prompt();
            let pos = (0..=SECRET.len() - 8)
                .find(|i| pr.contains(&SECRET[*i..*i + 8]))
                .unwrap_or(0);
            let from = pos.saturating_sub(250);
            let to = (pos + 350).min(pr.len());
            panic!(
                "a prompt leaks the secret at {pos}: ...{}...",
                &pr[from..to]
            );
        }
    }
}
