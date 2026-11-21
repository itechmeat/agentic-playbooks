//! The run gate of a playbook granting MCP tools (design D2 "The run
//! gate"): every granted tool needs a pin for its connector tree and its
//! account before a start is admitted. The gate reads the pin store only:
//! it starts no server and hashes no program file.

use std::path::Path;

use apb_core::connector::config;
use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::gate::launch::{LaunchContext, LaunchKind};
use apb_engine::next_action::start::{Remedy, check_start};
use serde_json::Value;

use crate::common;
use crate::connector_mcp::{ACCOUNT, CONNECTOR, Fx, TOKEN_VAR, connector_yaml};

const PLAYBOOK: &str = r#"
schema: 2
id: mcpgate
name: MCP gate
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: p, connectors: [{ name: tracker-mcp, functions: [search_issues] }] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

/// A project at `root` holding the playbook and its profile.
fn seed_project(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/mcpgate/1.0.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    std::fs::write(root.join(".apb/playbooks/mcpgate/current"), "1.0.0").unwrap();
    common::seed_profile(root, "p", "claude", "haiku", &[]);
}

fn wref() -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "mcpgate".into(),
        version: None,
    }
}

/// The gate's answer for the playbook in `root`; an acknowledge is given,
/// which never covers a tool approval.
fn gate(root: &Path) -> Result<(), Value> {
    apb_engine::gate::check_run(
        root,
        &wref(),
        true,
        false,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .map(|_| ())
}

fn unpinned(refusal: &Value) -> Vec<String> {
    assert_eq!(refusal["policy"], "mcp_tools_unpinned", "{refusal}");
    refusal["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

/// The installed connector with its project account approved, no pin.
fn installed() -> Fx {
    let fx = Fx::new();
    fx.install(&format!("{{{{env.{TOKEN_VAR}}}}}"));
    seed_project(&fx.root);
    fx
}

#[test]
fn a_start_with_an_unpinned_tool_is_refused() {
    let fx = installed();
    let refusal = gate(&fx.root).expect_err("refused");
    assert_eq!(
        unpinned(&refusal),
        vec![format!("{CONNECTOR}/{ACCOUNT}/search_issues")]
    );
    assert!(
        refusal["detail"]
            .as_str()
            .unwrap()
            .contains("acknowledge_untrusted does not bypass"),
        "{refusal}"
    );
}

#[test]
fn a_start_with_every_tool_pinned_is_admitted() {
    let fx = Fx::new();
    fx.install_pinned();
    seed_project(&fx.root);
    gate(&fx.root).expect("admitted");
}

#[test]
fn the_gate_starts_no_server() {
    let fx = installed();
    gate(&fx.root).expect_err("refused");
    fx.assert_not_started();
    fx.install_pinned();
    gate(&fx.root).expect("admitted");
    fx.assert_not_started();
    assert!(!fx.stub.dir.join("args.log").exists());
}

#[test]
fn another_project_account_of_the_same_name_is_unpinned() {
    let fx = Fx::new();
    fx.install_pinned();
    seed_project(&fx.root);
    gate(&fx.root).expect("admitted in the first project");
    // A second project with its own `work` account of other fields.
    let second = fx.base().join("second");
    seed_project(&second);
    let file = second
        .join(".apb/connector-config")
        .join(format!("{CONNECTOR}.yaml"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    workspace: other\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(second.join(".apb/secrets.env"), format!("{TOKEN_VAR}=x\n")).unwrap();
    let account = config::load_merged(&second, CONNECTOR)
        .unwrap()
        .into_iter()
        .find(|a| a.name == ACCOUNT)
        .unwrap();
    TrustStore::load()
        .approve_kind(
            &config::account_digest(&account),
            &account_trust_id(CONNECTOR, ACCOUNT),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let refusal = gate(&second).expect_err("refused in the second project");
    assert_eq!(
        unpinned(&refusal),
        vec![format!("{CONNECTOR}/{ACCOUNT}/search_issues")]
    );
}

#[test]
fn an_old_pin_under_a_reapproved_tree_is_unpinned() {
    let fx = Fx::new();
    fx.install_pinned();
    seed_project(&fx.root);
    gate(&fx.root).expect("admitted");
    // The definition changes and a person approves the new tree; the pin
    // record still names the old one.
    let yaml = connector_yaml(&fx.program.to_string_lossy()).replace(
        "args: [\"--workspace\"",
        "args: [\"--verbose\", \"--workspace\"",
    );
    let dir = fx.cfg.join("connectors").join(CONNECTOR);
    std::fs::write(dir.join("connector.yaml"), &yaml).unwrap();
    let loaded = apb_core::connector::store::load(CONNECTOR).unwrap();
    TrustStore::load()
        .approve_kind(
            &loaded.digest,
            CONNECTOR,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let refusal = gate(&fx.root).expect_err("refused");
    assert_eq!(
        unpinned(&refusal),
        vec![format!("{CONNECTOR}/{ACCOUNT}/search_issues")]
    );
}

#[test]
fn the_refusal_offers_a_manual_remedy() {
    let fx = installed();
    let refused = check_start(
        &fx.root,
        &wref(),
        true,
        false,
        None,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .expect_err("refused");
    let manual: Vec<&String> = refused
        .actions
        .iter()
        .filter_map(|a| match &a.remedy {
            Some(Remedy::Manual { detail }) => Some(detail),
            _ => None,
        })
        .collect();
    assert_eq!(
        manual,
        vec![&format!(
            "a person approves the MCP tools of connector `{CONNECTOR}` for account `{ACCOUNT}` at a terminal"
        )],
        "{:?}",
        refused.actions
    );
    let text = serde_json::to_string(&refused.actions).unwrap();
    assert!(!text.contains("apb connector approve"), "{text}");
}

#[test]
fn fit_lists_the_unpinned_tool() {
    let fx = installed();
    let fit = apb_engine::fit::assess(&fx.root, &wref()).unwrap();
    let row = fit
        .trust
        .iter()
        .find(|t| t.kind == "mcp_tools")
        .expect("an mcp_tools row");
    assert_eq!(row.subject, format!("{CONNECTOR}/{ACCOUNT}/search_issues"));
    assert!(!row.approved && !row.acknowledge_covers);
    fx.assert_not_started();
}
