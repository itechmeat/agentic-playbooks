//! The corrections ledger on the MCP tools: `playbook_corrections` lists
//! the entries and their drafts (read-only, one entry when asked),
//! `correction_decide` takes dismiss and reopen only (the args type refuses
//! `fixed`: marking a fix is the CLI's and the dashboard's), and a run step
//! is refused for both tools.

use super::*;

const KEY: &str = "t:0123456789abcdef";

const PLAYBOOK: &str = "schema: 2\nid: corr\nname: Corr\nversion: 1.0.0\nnodes:\n  \
- { id: start, type: start }\n  \
- { id: work, type: agent_task, prompt: do the work }\n  \
- { id: done, type: finish, outcome: success }\n\
edges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

fn json_of(res: &CallToolResult) -> serde_json::Value {
    serde_json::from_str(&result_text(res)).unwrap()
}

/// A project with `corr` and a hand-built ledger: one proposed entry (two
/// runs of one review note at the gate `review`); returns its id.
fn seed(root: &Path) -> String {
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/corr/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/corr/current"), "1.0.0").unwrap();
    apb_core::signed_store::update(
        root,
        apb_core::scope::Scope::Project,
        "corr",
        |f: &mut apb_core::corrections::CorrectionFile| {
            for i in 0..2u64 {
                let run = format!("corr-{}", i + 1);
                f.observe(
                    apb_core::corrections::ObservedRun {
                        run_id: run.clone(),
                        version: "1.0.0".into(),
                        started_ms: 1000 + i * 1000,
                    },
                    vec![apb_core::corrections::Sighting {
                        kind: apb_core::corrections::CorrectionKind::ReviewNote,
                        node: Some("work".into()),
                        gate: Some("review".into()),
                        key: KEY.into(),
                        text: Some("quote the version".into()),
                        run_id: run,
                        seq: 5,
                        at_ms: 1100 + i * 1000,
                        version: "1.0.0".into(),
                        via_supervisor: false,
                    }],
                    1100 + i * 1000,
                );
            }
            Ok::<_, apb_core::signed_store::StoreError>(((), true))
        },
    )
    .unwrap();
    apb_core::corrections::entry_id(KEY)
}

/// The MCP decide args take dismiss and reopen only: marking a fix is the
/// CLI's and the dashboard's, never MCP, so the args type refuses `fixed`
/// and the served schema offers a client no fixed decision either.
#[test]
fn the_mcp_args_have_no_fixed_decision() {
    for decision in ["dismiss", "reopen"] {
        assert!(
            serde_json::from_value::<crate::server::corrections::CorrectionDecideArgs>(
                serde_json::json!({
                    "id": "corr", "entry": "c-1", "decision": decision,
                })
            )
            .is_ok(),
            "{decision} is an MCP decision"
        );
    }
    assert!(
        serde_json::from_value::<crate::server::corrections::CorrectionDecideArgs>(
            serde_json::json!({
                "id": "corr", "entry": "c-1", "decision": "fixed",
            })
        )
        .is_err(),
        "the MCP decide args accepted `fixed`"
    );
    let tool = WfMcp::new(PathBuf::from("."))
        .tool_router
        .list_all()
        .into_iter()
        .find(|t| t.name == "correction_decide")
        .expect("correction_decide is routed");
    let schema = serde_json::to_value(&tool.input_schema)
        .unwrap()
        .to_string();
    assert!(schema.contains("dismiss"), "{schema}");
    assert!(schema.contains("reopen"), "{schema}");
    assert!(!schema.contains("\"fixed\""), "{schema}");
}

/// Sets `APB_RUN_ID` for one test and puts the earlier value back when
/// dropped, even when the test panics.
struct RunIdGuard(Option<std::ffi::OsString>);

impl RunIdGuard {
    fn set(value: &str) -> Self {
        let old = std::env::var_os("APB_RUN_ID");
        // SAFETY: under CROSS_ENV_LOCK, like every env change in these tests.
        unsafe { std::env::set_var("APB_RUN_ID", value) };
        RunIdGuard(old)
    }
}

impl Drop for RunIdGuard {
    fn drop(&mut self) {
        // SAFETY: under CROSS_ENV_LOCK.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("APB_RUN_ID", v),
                None => std::env::remove_var("APB_RUN_ID"),
            }
        }
    }
}

/// The list answers the entries with their drafts, `entry` filters to one,
/// and a decision is recorded with `by: mcp`.
#[tokio::test]
async fn the_tools_list_and_decide() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let id = seed(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());

    let list = json_of(
        &server
            .playbook_corrections(Parameters(crate::server::corrections::CorrectionsArgs {
                id: "corr".into(),
                scope: None,
                entry: None,
                workspace: None,
            }))
            .await,
    );
    assert_eq!(list["playbook"], "corr", "{list}");
    assert_eq!(list["scope"], "project");
    assert!(list["signed"].as_bool().unwrap());
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
    let e = &list["entries"][0];
    assert_eq!(e["correction"]["id"], id.as_str());
    assert_eq!(e["correction"]["status"], "proposed");
    assert_eq!(e["draft"]["node"], "work");
    assert!(e.get("rate").is_none(), "no fix is marked, so no rate");

    let one = json_of(
        &server
            .playbook_corrections(Parameters(crate::server::corrections::CorrectionsArgs {
                id: "corr".into(),
                scope: None,
                entry: Some(id.clone()),
                workspace: None,
            }))
            .await,
    );
    assert_eq!(one["entries"].as_array().unwrap().len(), 1);

    let decided = json_of(
        &server
            .correction_decide(Parameters(
                crate::server::corrections::CorrectionDecideArgs {
                    id: "corr".into(),
                    scope: None,
                    entry: id.clone(),
                    decision: crate::server::corrections::CorrectionDecision::Dismiss,
                    reason: Some("not a problem".into()),
                    workspace: None,
                },
            ))
            .await,
    );
    assert_eq!(decided["correction"]["status"], "dismissed", "{decided}");
    assert_eq!(decided["correction"]["decided"]["by"], "mcp");
    assert_eq!(decided["correction"]["decided"]["reason"], "not a problem");
    assert!(
        decided.get("draft").is_none(),
        "a dismissed entry offers no draft"
    );

    // Reopen records the person's word again (by mcp; the fix is marked
    // elsewhere).
    let reopened = json_of(
        &server
            .correction_decide(Parameters(
                crate::server::corrections::CorrectionDecideArgs {
                    id: "corr".into(),
                    scope: None,
                    entry: id,
                    decision: crate::server::corrections::CorrectionDecision::Reopen,
                    reason: None,
                    workspace: None,
                },
            ))
            .await,
    );
    assert_eq!(reopened["correction"]["status"], "proposed", "{reopened}");
}

/// A run step reads and decides nothing: every entry point is refused.
#[tokio::test]
async fn a_run_step_is_refused_for_both_tools() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let _g = RunIdGuard::set("corr-3");
    let dir = tempfile::tempdir().unwrap();
    let id = seed(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());

    let list = result_text(
        &server
            .playbook_corrections(Parameters(crate::server::corrections::CorrectionsArgs {
                id: "corr".into(),
                scope: None,
                entry: None,
                workspace: None,
            }))
            .await,
    );
    assert!(list.contains("run step"), "{list}");
    let decided = result_text(
        &server
            .correction_decide(Parameters(
                crate::server::corrections::CorrectionDecideArgs {
                    id: "corr".into(),
                    scope: None,
                    entry: id,
                    decision: crate::server::corrections::CorrectionDecision::Dismiss,
                    reason: None,
                    workspace: None,
                },
            ))
            .await,
    );
    assert!(decided.contains("run step"), "{decided}");
}
