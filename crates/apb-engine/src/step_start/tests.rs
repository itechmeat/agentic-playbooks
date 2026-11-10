//! The starter resolution over temp projects: each test runs in its own
//! child process (its config dir and installation key are its own).

use super::*;

/// Runs `f` in a child test process with a fresh config dir and key.
fn child(name: &str, f: impl FnOnce()) {
    apb_core::process_mode::testing::in_plain_child(name, || {
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: a child process runs this one test on one thread.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        apb_core::run_origin::ensure_key().unwrap();
        f();
    });
}

/// A run `id` under `root` with `lines` (JSON events after `run_started`),
/// stamped as created here.
fn seed(root: &Path, id: &str, lines: &[Value]) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut all = vec![
        json!({"seq": 1, "ts": 1, "type": "run_started", "playbook": "p", "version": "1.0.0"}),
    ];
    all.extend_from_slice(lines);
    let text: String = all
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut l = l.clone();
            l["seq"] = json!(i + 1);
            l["ts"] = json!(i + 1);
            format!("{l}\n")
        })
        .collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
    dir
}

fn attempt(node: &str, n: u32, token: &str) -> Value {
    json!({"type": "attempt_started", "node": node, "attempt": n, "agent": "claude", "token": token})
}

fn node_started(node: &str) -> Value {
    json!({"type": "node_started", "node": node, "attempt": 1})
}

fn caller(id: &str, node: &str, token: Option<&str>, task: Option<&str>) -> CallerSnapshot {
    CallerSnapshot {
        run_id: Some(id.into()),
        run_dir: None,
        node: Some(node.into()),
        claim: AttemptClaim {
            attempt: None,
            token: token.map(str::to_string),
            host_task: task.map(str::to_string),
            item: None,
        },
    }
}

fn started(root: &Path, c: &CallerSnapshot) -> StartedBy {
    resolve(root, c).unwrap().expect("a starter")
}

#[test]
fn outside_a_run_step_there_is_no_starter() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(resolve(root.path(), &CallerSnapshot::default()), Ok(None));
}

/// The open attempt's token verifies the step and names its attempt; a
/// wrong token, a finished starter and an `APB_RUN_DIR` that is a link do
/// not, and an unverified start is chain 1.
#[test]
fn a_token_binds_the_open_attempt() {
    child("step_start::tests::a_token_binds_the_open_attempt", || {
        let p = tempfile::tempdir().unwrap();
        let root = p.path();
        seed(
            root,
            "t-1",
            &[node_started("triage"), attempt("triage", 2, "tok-2")],
        );
        let ok = started(root, &caller("t-1", "triage", Some("tok-2"), None));
        assert!(ok.verified);
        assert_eq!((ok.attempt, ok.chain), (Some(2), 1));
        assert_eq!(ok.node.as_deref(), Some("triage"));
        let wrong = started(root, &caller("t-1", "triage", Some("forged"), None));
        assert!(!wrong.verified);
        assert_eq!((wrong.attempt, wrong.chain), (None, 1));

        seed(
            root,
            "t-2",
            &[
                node_started("triage"),
                attempt("triage", 1, "tok-1"),
                json!({"type": "run_finished", "outcome": "succeeded"}),
            ],
        );
        assert!(!started(root, &caller("t-2", "triage", Some("tok-1"), None)).verified);

        let elsewhere = tempfile::tempdir().unwrap();
        let link = elsewhere.path().join("t-1");
        std::os::unix::fs::symlink(root.join(".apb/runs/t-1"), &link).unwrap();
        let mut via_link = caller("t-1", "triage", Some("tok-2"), None);
        via_link.run_dir = Some(link);
        let by = started(root, &via_link);
        assert!(!by.verified);
        assert_eq!(by.chain, 1);

        // A run another installation created is not verified.
        let dir = seed(
            root,
            "t-3",
            &[node_started("triage"), attempt("triage", 1, "tok-3")],
        );
        let other = tempfile::tempdir().unwrap();
        let own = std::env::var_os("APB_CONFIG_DIR").unwrap();
        // SAFETY: a child process runs this one test on one thread.
        unsafe { std::env::set_var("APB_CONFIG_DIR", other.path()) };
        apb_core::run_origin::ensure_key().unwrap();
        apb_core::run_origin::stamp(&dir, "t-3").unwrap();
        unsafe { std::env::set_var("APB_CONFIG_DIR", own) };
        assert!(!started(root, &caller("t-3", "triage", Some("tok-3"), None)).verified);
    });
}

/// The definition of a run with the script node `sh` and the agent node
/// `a`.
fn script_definition(dir: &Path) {
    std::fs::write(
        dir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sh, type: script, script: \"scripts/s.sh\", runner: sh }\n  - { id: a, type: agent_task, prompt: do, profile: main }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sh }\n  - { from: sh, to: a }\n  - { from: a, to: done }\n",
    )
    .unwrap();
}

/// A script step carries no token: verified while its visit is open, not
/// after it finished.
#[test]
fn a_script_step_is_verified_while_its_visit_is_open() {
    child(
        "step_start::tests::a_script_step_is_verified_while_its_visit_is_open",
        || {
            let p = tempfile::tempdir().unwrap();
            let root = p.path();
            let dir = seed(root, "s-1", &[node_started("sh"), node_started("a")]);
            script_definition(&dir);
            let open = started(root, &caller("s-1", "sh", None, None));
            assert!(open.verified);
            assert_eq!(open.attempt, None);
            // A claim with attempt credentials is no script step: a token
            // of another attempt does not verify through an open script
            // visit.
            assert!(!started(root, &caller("s-1", "sh", Some("unrelated"), None)).verified);
            // An open visit of a node that is no script (an agent node
            // before its first attempt) verifies nobody.
            assert!(!started(root, &caller("s-1", "a", None, None)).verified);
            let dir = seed(
                root,
                "s-2",
                &[
                    node_started("sh"),
                    json!({"type": "node_finished", "node": "sh", "status": "succeeded", "attempt": 1, "output": ""}),
                ],
            );
            script_definition(&dir);
            assert!(!started(root, &caller("s-2", "sh", None, None)).verified);
            // No token on an agent node's open attempt is no proof.
            seed(root, "s-3", &[node_started("a"), attempt("a", 1, "tok")]);
            assert!(!started(root, &caller("s-3", "a", None, None)).verified);
        },
    );
}

/// A host task's subagent carries the token and the task id; a task a
/// resumed drive adopted (requested again within a later attempt) keeps
/// verifying with the first attempt's token.
#[test]
fn a_host_task_and_an_adopted_one_are_verified() {
    child(
        "step_start::tests::a_host_task_and_an_adopted_one_are_verified",
        || {
            let p = tempfile::tempdir().unwrap();
            let root = p.path();
            let requested = json!({"type": "host_task_requested", "task_id": "task-1", "node": "plan", "attempt": 1, "prompt_ref": "p"});
            seed(
                root,
                "h-1",
                &[
                    node_started("plan"),
                    attempt("plan", 1, "tok-1"),
                    requested.clone(),
                ],
            );
            let by = started(root, &caller("h-1", "plan", Some("tok-1"), Some("task-1")));
            assert!(by.verified);
            assert_eq!(by.attempt, Some(1));
            assert_eq!(by.host_task.as_deref(), Some("task-1"));
            seed(
                root,
                "h-2",
                &[
                    node_started("plan"),
                    attempt("plan", 1, "tok-1"),
                    requested.clone(),
                    json!({"type": "run_paused", "reason": "later", "by": "operator"}),
                    json!({"type": "run_resumed", "from_node": "plan", "by": "operator"}),
                    attempt("plan", 2, "tok-2"),
                    requested,
                ],
            );
            let adopted = started(root, &caller("h-2", "plan", Some("tok-1"), Some("task-1")));
            assert!(adopted.verified, "{adopted:?}");
            assert_eq!(adopted.attempt, Some(2));
        },
    );
}

/// Writes a manifest whose launch record names a starter at `chain`, and
/// stamps the run again.
fn record_chain(dir: &Path, chain: u32) {
    let manifest = json!({
        "profiles": [],
        "node_bindings": {},
        "launch": {
            "kind": "start", "initiator": "run_step:cli", "policy": "allow", "sources": [],
            "started_by": { "run_id": "x", "verified": true, "chain": chain }
        }
    });
    // JSON is YAML: the manifest reader takes it as it is.
    std::fs::write(dir.join("manifest.yaml"), manifest.to_string()).unwrap();
    // The origin stamp covers the manifest: stamp the run again.
    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
    apb_core::run_origin::stamp(dir, &id).unwrap();
}

/// `chain_of` reads a run's record, a child's through its parent, and 0
/// without either; a starter at chain 5 refuses a sixth start.
#[test]
fn the_chain_follows_the_record_and_is_bounded() {
    child(
        "step_start::tests::the_chain_follows_the_record_and_is_bounded",
        || {
            let p = tempfile::tempdir().unwrap();
            let root = p.path();
            let parent = seed(root, "c-1", &[node_started("n"), attempt("n", 1, "tok")]);
            assert_eq!(chain_of(&parent), 0);
            record_chain(&parent, 4);
            assert_eq!(chain_of(&parent), 4);
            let kid = seed(root, "c-2", &[node_started("k"), attempt("k", 1, "tok-k")]);
            let cfg = crate::run_config::RunConfig {
                parent_run: Some("c-1".into()),
                ..Default::default()
            };
            crate::run_config::write_run_config(&kid, &cfg).unwrap();
            assert_eq!(chain_of(&kid), 4);
            assert_eq!(
                started(root, &caller("c-2", "k", Some("tok-k"), None)).chain,
                5
            );
            record_chain(&parent, 5);
            let refused = resolve(root, &caller("c-1", "n", Some("tok"), None)).unwrap_err();
            assert_eq!(refused["policy"], CHAIN_TOO_DEEP);
            assert_eq!(refused["chain"], 6);
            let detail = refused["detail"].as_str().unwrap();
            assert!(
                detail.contains("is the 5th") && detail.contains("a 6th is refused"),
                "{detail}"
            );
        },
    );
}

/// A starter in another registered project keeps its chain and names its
/// workspace; one in a project nobody registered is not verified.
#[test]
fn a_starter_in_another_registered_project_keeps_its_chain() {
    child(
        "step_start::tests::a_starter_in_another_registered_project_keeps_its_chain",
        || {
            let a = tempfile::tempdir().unwrap();
            let b = tempfile::tempdir().unwrap();
            apb_core::registry::init_project(a.path()).unwrap();
            apb_core::registry::init_project(b.path()).unwrap();
            let dir = seed(
                a.path(),
                "x-1",
                &[node_started("n"), attempt("n", 1, "tok")],
            );
            record_chain(&dir, 2);
            let mut c = caller("x-1", "n", Some("tok"), None);
            c.run_dir = Some(dir.clone());
            // Not registered: not verified, chain 1.
            assert!(!started(b.path(), &c).verified);
            // `bind_id`, not `touch`: `touch` registers nothing under `CI`.
            let ws = apb_core::projects::bind_id(a.path()).expect("registered");
            assert_eq!(
                apb_core::projects::registered_id(a.path()).as_deref(),
                Some(ws.as_str())
            );
            let by = started(b.path(), &c);
            assert!(by.verified, "{by:?}");
            assert_eq!(by.chain, 3);
            assert_eq!(by.workspace_id.as_deref(), Some(ws.as_str()));
        },
    );
}

#[test]
fn ordinals_read_as_english() {
    let got: Vec<String> = [1, 2, 3, 4, 5, 6, 11, 12, 13, 21, 22].map(ordinal).to_vec();
    assert_eq!(
        got,
        [
            "1st", "2nd", "3rd", "4th", "5th", "6th", "11th", "12th", "13th", "21st", "22nd"
        ]
    );
}
