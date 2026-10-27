//! Known problems on the MCP tools, with arguments as a client sends them:
//! `playbook_known_problems` lists a collected problem and, for one, its
//! changed passages; `known_problem_decide` records a person's confirm and
//! refuses a close without a quote; `known_problem_recheck` takes a host's
//! verdict; `run_status` carries what the run was shown.

use super::*;

const EVENTS: &str =
    include_str!("../../../../apb-engine/tests/fixtures/known_problems/events.jsonl");

fn json_of(res: &CallToolResult) -> serde_json::Value {
    serde_json::from_str(&result_text(res)).unwrap()
}

fn args<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).unwrap())
}

/// A project whose one ended run of `fix-build` reported its build script
/// and was collected, plus a `known_problems_shown` in its journal.
fn seeded(root: &Path) -> String {
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::registry::init_project(root).unwrap();
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::write(
        root.join("scripts/build.sh"),
        "cc -Werror=deprecated -c z.c\n",
    )
    .unwrap();
    let run = root.join(".apb/runs/fix-build-1");
    fs::create_dir_all(&run).unwrap();
    let shown = r#"{"seq":7,"ts":1007,"type":"known_problems_shown","shown":[{"id":"kp-1","scope":"path:scripts/build.sh","symptom":"the build fails","basis":"unchanged"}],"hidden":[{"id":"kp-2","reason":"undetermined"}]}"#;
    fs::write(run.join("events.jsonl"), format!("{EVENTS}{shown}\n")).unwrap();
    apb_core::run_origin::stamp(&run, "fix-build-1").unwrap();
    apb_engine::known_problems::observe(root, &run).unwrap();
    let store = apb_engine::known_problems::view_current(
        root,
        apb_core::scope::Scope::Project,
        "fix-build",
    )
    .unwrap();
    store
        .problems
        .iter()
        .find(|p| p.problem.scope.key() == "path:scripts/build.sh")
        .expect("the build-script problem")
        .problem
        .id
        .clone()
}

#[tokio::test]
async fn known_problems_round_trip_through_the_tools() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let id = seeded(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());

    let list = json_of(
        &server
            .playbook_known_problems(args(json!({ "id": "fix-build" })))
            .await,
    );
    assert_eq!(list["scope"], "project", "{list}");
    assert!(
        list["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == id.as_str() && p["source_now"] == "unchanged"),
        "{list}"
    );
    let one = json_of(
        &server
            .playbook_known_problems(args(json!({ "id": "fix-build", "problem": id })))
            .await,
    );
    assert_eq!(one["problem"]["id"], id.as_str());
    assert_eq!(one["changed_passages"], json!([]));

    let no_quote = json_of(
        &server
            .known_problem_decide(args(
                json!({ "id": "fix-build", "problem": id, "action": "close" }),
            ))
            .await,
    );
    assert_eq!(no_quote["error"], "invalid_arguments");
    let confirmed = json_of(
        &server
            .known_problem_decide(args(json!({
                "id": "fix-build", "problem": id, "action": "confirm", "reason": "seen it",
            })))
            .await,
    );
    assert_eq!(confirmed["confirmed"]["by"], "mcp", "{confirmed}");

    let bad_by = json_of(
        &server
            .known_problem_recheck(args(
                json!({ "id": "fix-build", "problem": id, "by": "me" }),
            ))
            .await,
    );
    assert_eq!(bad_by["error"], "invalid_arguments");
    // A host's recheck needs its verdict; the model's takes none of the
    // host's fields (they would be dropped without a word).
    for bad in [
        json!({ "id": "fix-build", "problem": id, "by": "host" }),
        json!({ "id": "fix-build", "problem": id, "by": "model", "verdict": "closed" }),
        json!({ "id": "fix-build", "problem": id, "by": "model", "quote": "a fix line here" }),
    ] {
        let refused = json_of(&server.known_problem_recheck(args(bad.clone())).await);
        assert_eq!(refused["error"], "invalid_arguments", "{bad}: {refused}");
    }
    // The source did not change, so a host's holds is recorded against it.
    let holds = json_of(
        &server
            .known_problem_recheck(args(json!({
                "id": "fix-build", "problem": id, "by": "host", "verdict": "holds",
            })))
            .await,
    );
    assert_eq!(
        (holds["verdict"].clone(), holds["basis"].clone()),
        (json!("holds"), json!("host"))
    );

    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: "fix-build-1".into(),
                workspace: None,
            }))
            .await,
    );
    assert_eq!(
        status["known_problems"]["shown"][0]["id"], "kp-1",
        "{status}"
    );
    assert_eq!(
        status["known_problems"]["hidden"][0]["reason"],
        "undetermined"
    );
}
