//! The `irreversible` consent gate (0.24.0): a run whose tree declares
//! `irreversible` starts only with a consent, which the manifest records
//! with the sources it covers, a sub-playbook inherits, a resume keeps (or
//! asks for once) and a supervisor patch may not widen.

use apb_core::registry::init_project;
use apb_engine::consent::{Confirmation, RunConsent};

use crate::common;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::path::Path;

fn playbook(id: &str, head: &str, nodes: &str, edges: &str) -> String {
    format!(
        "schema: 1\nid: {id}\nname: {id}\nversion: 1.0.0\n{head}nodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    )
}

/// start -> a (a prompt node, no agent) -> done, with `head` above `nodes:`
/// and `a_extra` inside node `a`.
fn linear(id: &str, head: &str, a_extra: &str) -> String {
    playbook(
        id,
        head,
        &format!("  - {{ id: a, type: prompt, prompt: \"x\"{a_extra} }}\n"),
        "  - { from: start, to: a }\n  - { from: a, to: done }\n",
    )
}

fn seed(root: &Path, id: &str, yaml: &str) {
    let _ = init_project(root);
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn with_consent(by: &str) -> RunOptions {
    RunOptions {
        consent: Some(RunConsent::irreversible(by)),
        ..Default::default()
    }
}

fn manifest_consent(root: &Path, run_id: &str) -> Option<RunConsent> {
    apb_engine::manifest::read(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .and_then(|m| m.consent)
}

fn runs_dir_is_empty(root: &Path) -> bool {
    fs::read_dir(root.join(".apb/runs"))
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

/// Each place a tree declares `irreversible` refuses a start without
/// consent, names that place, and leaves no run behind. A consent that does
/// not cover `irreversible` is no consent.
#[test]
fn an_irreversible_tree_is_refused_without_consent_and_leaves_no_run() {
    let not_irreversible = RunOptions {
        consent: Some(RunConsent {
            irreversible: false,
            ..RunConsent::irreversible("cli")
        }),
        ..Default::default()
    };
    /// (name, playbooks to seed, the start's options, what the refusal names).
    type Case<'a> = (&'a str, Vec<(&'a str, String)>, RunOptions, &'a str);
    let cases: Vec<Case> = vec![
        (
            "playbook",
            vec![("rel", linear("rel", "effects: [irreversible]\n", ""))],
            RunOptions::default(),
            "playbook `rel` has irreversible effects (playbook)",
        ),
        (
            "node",
            vec![("rel", linear("rel", "", ", effects: [irreversible]"))],
            RunOptions::default(),
            "(node a)",
        ),
        (
            "sub-playbook",
            vec![
                ("child", linear("child", "effects: [irreversible]\n", "")),
                ("rel", parent_named("rel", "child")),
            ],
            RunOptions::default(),
            "(sub-playbook node sub)",
        ),
        (
            "irreversible: false",
            vec![("rel", linear("rel", "effects: [irreversible]\n", ""))],
            not_irreversible,
            "(playbook)",
        ),
    ];
    for (case, seeds, opts, names) in cases {
        let dir = tempfile::tempdir().unwrap();
        for (id, yaml) in &seeds {
            seed(dir.path(), id, yaml);
        }
        let err = run(dir.path(), "rel", None, opts)
            .expect_err("no consent, no run")
            .to_string();
        assert!(
            err.contains("irreversible_requires_confirmation"),
            "{case}: {err}"
        );
        assert!(err.contains(names), "{case}: {err}");
        assert!(
            runs_dir_is_empty(dir.path()),
            "{case}: a refusal writes no run"
        );
    }
}

#[test]
fn a_consented_run_records_the_consent_in_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "effects: [irreversible]\n", ""),
    );
    let res = run(dir.path(), "rel", None, with_consent("cli")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        manifest_consent(dir.path(), &res.run_id),
        Some(covering(RunConsent::irreversible("cli"), &["playbook"]))
    );
}

#[test]
fn a_consent_is_not_recorded_for_a_run_that_needs_none() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "plain", &linear("plain", "", ""));
    let res = run(dir.path(), "plain", None, with_consent("cli")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(manifest_consent(dir.path(), &res.run_id), None);
}

#[test]
fn a_resume_keeps_the_consent_and_asks_for_none() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "effects: [irreversible]\n", ""),
    );
    let res = run(dir.path(), "rel", None, with_consent("mcp:host")).unwrap();
    let again = resume(dir.path(), &res.run_id, Some("a")).unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    assert_eq!(
        manifest_consent(dir.path(), &res.run_id),
        Some(covering(
            RunConsent::irreversible("mcp:host"),
            &["playbook"]
        ))
    );
}

fn parent_of(child: &str) -> String {
    parent_named("parent", child)
}

/// start -> sub (a sub-playbook node running `child`) -> done.
fn parent_named(id: &str, child: &str) -> String {
    playbook(
        id,
        "",
        &format!("  - {{ id: sub, type: playbook, playbook: {child} }}\n"),
        "  - { from: start, to: sub }\n  - { from: sub, to: done }\n",
    )
}

/// `consent` as the engine records it: with the sources it covered.
fn covering(consent: RunConsent, sources: &[&str]) -> RunConsent {
    RunConsent {
        sources: sources.iter().map(|s| s.to_string()).collect(),
        ..consent
    }
}

/// The run id of the first sub-playbook run the run `run_id` started.
fn child_run_of(root: &Path, run_id: &str) -> String {
    read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .expect("the child ran")
}

#[test]
fn a_sub_playbook_inherits_the_parents_consent() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "child",
        &linear("child", "effects: [irreversible]\n", ""),
    );
    seed(dir.path(), "parent", &parent_of("child"));
    let res = run(dir.path(), "parent", None, with_consent("dashboard")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let child_run = child_run_of(dir.path(), &res.run_id);
    assert_eq!(
        manifest_consent(dir.path(), &child_run),
        Some(RunConsent {
            irreversible: true,
            by: "dashboard".into(),
            inherited_from: Some(res.run_id.clone()),
            sources: vec!["playbook".into()],
        })
    );
}

/// Depth 2: an irreversible grandchild under two ancestors that declare
/// nothing makes the root need consent, and the grandchild inherits the
/// root's consent through its own parent.
#[test]
fn an_irreversible_grandchild_needs_the_roots_consent_and_inherits_it() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "grandchild",
        &linear("grandchild", "effects: [irreversible]\n", ""),
    );
    seed(dir.path(), "child", &parent_named("child", "grandchild"));
    seed(dir.path(), "parent", &parent_of("child"));
    let err = run(dir.path(), "parent", None, RunOptions::default())
        .expect_err("the tree is irreversible")
        .to_string();
    assert!(err.contains("(sub-playbook node sub)"), "{err}");
    assert!(runs_dir_is_empty(dir.path()));

    let res = run(dir.path(), "parent", None, with_consent("cli_flag")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let child = child_run_of(dir.path(), &res.run_id);
    let grandchild = child_run_of(dir.path(), &child);
    let consent = manifest_consent(dir.path(), &grandchild).expect("inherited");
    assert_eq!(consent.by, "cli_flag");
    assert_eq!(consent.inherited_from.as_deref(), Some(child.as_str()));
}

#[test]
fn the_gate_refuses_with_the_sources_and_accepts_a_consent() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "child",
        &linear("child", "effects: [irreversible]\n", ""),
    );
    seed(dir.path(), "parent", &parent_of("child"));
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "parent".into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(dir.path(), &wref, true, false).unwrap();
    let refusal = permit.check_confirmation(None).expect_err("needs consent");
    assert_eq!(refusal["policy"], "irreversible_requires_confirmation");
    assert_eq!(
        refusal["sources"],
        serde_json::json!(["sub-playbook node sub"])
    );
    let nonce = permit.consent_nonce();
    assert_eq!(refusal["consent_nonce"], nonce.as_str());
    assert_eq!(
        permit.check_confirmation(Some(&Confirmation::Nonce(nonce))),
        Ok(None)
    );
    let stale = permit
        .check_confirmation(Some(&Confirmation::Nonce("consent-stale".into())))
        .expect_err("a nonce for another tree");
    assert!(
        stale["reason"]
            .as_str()
            .unwrap()
            .starts_with("consent_nonce_mismatch")
    );
    // A gated run (pins from the permit) is checked along the pins too.
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let err = run(dir.path(), "parent", None, opts).expect_err("pinned tree");
    assert!(err.to_string().contains("(sub-playbook node sub)"), "{err}");
}

/// A trust refusal of an irreversible tree also names its irreversible
/// sources and nonce, so a host asks one question that covers both.
#[test]
fn a_trust_refusal_of_an_irreversible_tree_names_its_sources_too() {
    let _env = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = set_config_dir(cfg.path());
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "", ", effects: [irreversible]"),
    );
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "rel".into(),
        version: None,
    };
    let refusal =
        apb_engine::gate::check_run(dir.path(), &wref, false, false).expect_err("untrusted");
    assert_eq!(refusal["policy"], "untrusted_requires_acknowledge");
    assert_eq!(refusal["irreversible"], serde_json::json!(["node a"]));
    let permit = apb_engine::gate::check_run(dir.path(), &wref, true, false).unwrap();
    assert_eq!(refusal["consent_nonce"], permit.consent_nonce().as_str());
}

// --- supervisor patches ---

/// start -> a -> b -> done, prompt nodes; `b_extra` goes inside node `b`.
fn two_step(b_extra: &str) -> String {
    playbook(
        "p",
        "",
        &format!(
            "  - {{ id: a, type: prompt, prompt: \"x\" }}\n  - {{ id: b, type: prompt, prompt: \"y\"{b_extra} }}\n"
        ),
        "  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n",
    )
}

/// Starts `p` 1.0.0 supervised with `consent`, posts a patch to a version
/// whose node `b` carries `b_extra`, drives the run to its end and returns
/// the run's events.
fn patched_run(
    v1_b_extra: &str,
    patch_b_extra: &str,
    consent: Option<RunConsent>,
) -> Vec<EventPayload> {
    use apb_engine::control::Control;
    use apb_engine::scheduler::{
        RunMode, drive_prepared, post_supervisor_command, prepare_supervised_background,
    };
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "p", &two_step(v1_b_extra));
    let prepared = prepare_supervised_background(
        dir.path(),
        "p",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            consent,
            ..Default::default()
        },
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    let version = apb_core::versioning::create_patch_version(
        dir.path(),
        "p",
        "1.0.0",
        &two_step(patch_b_extra),
        &run_id,
        "workaround",
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version,
            classification: "workaround".into(),
            continue_from: "a".into(),
        },
    )
    .unwrap();
    drive_prepared(dir.path(), prepared).unwrap();
    read_all(&dir.path().join(".apb/runs").join(&run_id))
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

fn rejection(events: &[EventPayload]) -> Option<&str> {
    events.iter().find_map(|e| match e {
        EventPayload::PatchRejected { reason } => Some(reason.as_str()),
        _ => None,
    })
}

/// A supervisor patch may not bring irreversible steps into a run started
/// without consent, nor add sources beyond the ones a consented run covers;
/// a patch that keeps the consented sources applies.
#[test]
fn a_patch_may_not_add_irreversible_steps_the_consent_does_not_cover() {
    let irreversible = ", effects: [irreversible]";
    // No consent, the patch adds `node b`.
    let events = patched_run("", irreversible, None);
    let reason = rejection(&events).expect("rejected");
    assert!(
        reason.starts_with("irreversible_requires_confirmation"),
        "{reason}"
    );
    assert!(reason.contains("node b"), "{reason}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EventPayload::PatchApplied { .. }))
    );

    // Consented to `node b`: a patch that keeps it applies.
    let events = patched_run(
        irreversible,
        irreversible,
        Some(RunConsent::irreversible("cli")),
    );
    assert_eq!(rejection(&events), None, "{events:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, EventPayload::PatchApplied { .. }))
    );
}

/// A consented run may not gain a source it was not consented for.
#[test]
fn a_patch_may_not_widen_a_consent() {
    let irreversible = ", effects: [irreversible]";
    let dir_events = {
        use apb_engine::control::Control;
        use apb_engine::scheduler::{
            RunMode, drive_prepared, post_supervisor_command, prepare_supervised_background,
        };
        let dir = tempfile::tempdir().unwrap();
        // 1.0.0: only `node b` is irreversible; the patch adds `node a` too.
        seed(dir.path(), "p", &two_step(irreversible));
        let prepared = prepare_supervised_background(
            dir.path(),
            "p",
            None,
            RunOptions {
                mode: RunMode::Supervised,
                consent: Some(RunConsent::irreversible("cli")),
                ..Default::default()
            },
        )
        .unwrap();
        let run_id = prepared.run_id().to_string();
        let widened = two_step(irreversible).replace(
            "prompt: \"x\" }",
            "prompt: \"x\", effects: [irreversible] }",
        );
        let version = apb_core::versioning::create_patch_version(
            dir.path(),
            "p",
            "1.0.0",
            &widened,
            &run_id,
            "workaround",
        )
        .unwrap();
        post_supervisor_command(
            dir.path(),
            &run_id,
            Control::Patch {
                version,
                classification: "workaround".into(),
                continue_from: "a".into(),
            },
        )
        .unwrap();
        drive_prepared(dir.path(), prepared).unwrap();
        read_all(&dir.path().join(".apb/runs").join(&run_id))
            .unwrap()
            .into_iter()
            .map(|e| e.payload)
            .collect::<Vec<_>>()
    };
    let reason = rejection(&dir_events).expect("rejected");
    assert!(
        reason.contains("node a") && !reason.contains("node b"),
        "{reason}"
    );
    assert!(reason.contains("does not cover"), "{reason}");
}

// --- resume ---

/// Restores `APB_CONFIG_DIR` on drop (held under `common::env_lock`).
struct ConfigDirGuard(Option<std::ffi::OsString>);
impl Drop for ConfigDirGuard {
    fn drop(&mut self) {
        // SAFETY: under `common::env_lock`, like every env mutation here.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
    }
}

fn set_config_dir(dir: &Path) -> ConfigDirGuard {
    let prior = std::env::var_os("APB_CONFIG_DIR");
    // SAFETY: the caller holds `common::env_lock`.
    unsafe { std::env::set_var("APB_CONFIG_DIR", dir) };
    ConfigDirGuard(prior)
}

/// A run apb created here (stamped) whose snapshot needs consent and whose
/// manifest has none, as a run an older apb started: `resume_consent_need`
/// asks, a consent is recorded once and then honoured; a stale nonce is
/// refused. A directory without a valid stamp never has its manifest
/// consent honoured.
#[test]
fn a_resume_asks_once_for_a_run_without_consent_and_honours_only_local_consent() {
    use apb_engine::gate::{check_resume_consent, resume_consent_need};
    let _env = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = set_config_dir(cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Started without irreversible (as by apb 0.23.0), then the snapshot is
    // the irreversible version the older apb ignored.
    seed(dir.path(), "rel", &linear("rel", "", ""));
    let res = run(dir.path(), "rel", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    fs::write(
        run_dir.join("playbook.yaml"),
        linear("rel", "effects: [irreversible]\n", ""),
    )
    .unwrap();
    assert!(apb_core::run_origin::verify(&run_dir, &res.run_id));

    let need = resume_consent_need(dir.path(), &res.run_id)
        .unwrap()
        .expect("no consent recorded");
    assert_eq!(need.sources, vec!["playbook".to_string()]);
    let refusal = check_resume_consent(dir.path(), &res.run_id, None, "mcp").expect_err("asks");
    assert_eq!(refusal["policy"], "irreversible_requires_confirmation");
    assert_eq!(refusal["consent_nonce"], need.nonce().as_str());
    check_resume_consent(
        dir.path(),
        &res.run_id,
        Some(&Confirmation::Nonce("consent-stale".into())),
        "mcp",
    )
    .expect_err("a stale nonce");

    assert_eq!(
        check_resume_consent(
            dir.path(),
            &res.run_id,
            Some(&Confirmation::Nonce(need.nonce())),
            "mcp:host",
        ),
        Ok(None)
    );
    assert_eq!(
        manifest_consent(dir.path(), &res.run_id),
        Some(covering(
            RunConsent::irreversible("mcp:host"),
            &["playbook"]
        ))
    );
    // Recorded and still stamped: the next resume asks nothing.
    assert!(apb_core::run_origin::verify(&run_dir, &res.run_id));
    assert_eq!(resume_consent_need(dir.path(), &res.run_id).unwrap(), None);

    // Without a valid stamp (a directory that came with a repository) the
    // manifest's consent is not honoured.
    fs::remove_file(run_dir.join("origin.stamp")).unwrap();
    assert!(
        resume_consent_need(dir.path(), &res.run_id)
            .unwrap()
            .is_some()
    );
}

// --- connector functions ---

/// A node granted a connector function flagged `irreversible: true` is a
/// consent source; a `read_only` grant is not.
#[test]
fn a_granted_irreversible_connector_function_needs_consent() {
    let _env = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = set_config_dir(cfg.path());
    let cdir = cfg.path().join("connectors/tracker");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(
        cdir.join("connector.yaml"),
        "name: tracker\nversion: 0.1.0\nfunctions:\n  - name: merge\n    description: d\n    method: PUT\n    url: http://a\n    irreversible: true\n  - name: list\n    description: d\n    method: GET\n    url: http://a\n    read_only: true\n",
    )
    .unwrap();
    let node = |binding: &str| {
        apb_core::schema::Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\n  - id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{binding}]\nedges: []\n"
        ))
        .unwrap()
    };
    let origin = apb_core::scope::Origin::Project { workspace_id: None };
    let root = tempfile::tempdir().unwrap();
    let sources = |binding: &str| {
        apb_engine::gate::consent_sources(root.path(), &node(binding), &origin, None)
    };
    let merge = vec!["node a (connector tracker: merge)".to_string()];
    assert_eq!(sources("{ name: tracker, functions: [merge] }"), merge);
    // The bare name grants every function.
    assert_eq!(sources("tracker"), merge);
    assert!(sources("{ name: tracker, functions: read_only }").is_empty());
    assert!(sources("{ name: tracker, functions: [list] }").is_empty());
}

/// A sub-playbook that does not resolve counts as needing consent at start,
/// rather than failing halfway through.
#[test]
fn an_unresolvable_sub_playbook_needs_consent_at_start() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "parent", &parent_of("missing"));
    let err = run(dir.path(), "parent", None, RunOptions::default())
        .expect_err("fail closed")
        .to_string();
    assert!(err.contains("(sub-playbook node sub)"), "{err}");
    assert!(runs_dir_is_empty(dir.path()));
}
