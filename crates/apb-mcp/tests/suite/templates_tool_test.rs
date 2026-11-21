//! The template gallery's one install entry (`tools::templates`): an
//! installed template is a draft that approves nothing, records its
//! provenance, carries its eval scenarios and the fit report, is refused
//! from a run step, and never lowers a launch floor.

use apb_engine::gate::launch::{LaunchContext, LaunchKind};
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_mcp::policy::check_run;
use apb_mcp::tools::templates::{IN_RUN_STEP, InstallRequest, install, list, show};
use serde_json::json;

use crate::common::{ConfigSandbox, config_sandbox};

/// A project inside a config sandbox of its own (the sandbox holds the env
/// lock and restores the previous value on drop). Returns the project root
/// and the sandbox, whose `path()` is the config dir.
fn fixture() -> (tempfile::TempDir, ConfigSandbox) {
    let cfg = config_sandbox();
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    (proj, cfg)
}

/// Every file below `dir`, path and bytes (no link followed).
fn tree(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                walk(base, &p, out);
            } else if ft.is_file() {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// `APB_RUN_ID` set for the test (the caller holds the env lock), restored
/// on drop.
struct RunStepEnv;
impl Drop for RunStepEnv {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_RUN_ID");
        }
    }
}

fn req<'a>(template: &'a str) -> InstallRequest<'a> {
    InstallRequest {
        template,
        scope: "project",
        as_id: None,
        with_fit: false,
    }
}

fn playbook_dir(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(".apb/playbooks").join(id)
}

fn trust_bytes(cfg: &Path) -> Option<Vec<u8>> {
    std::fs::read(cfg.join("trust.json")).ok()
}

#[test]
fn an_installed_template_is_a_draft_and_approves_nothing() {
    let (proj, cfg) = fixture();
    let before = trust_bytes(cfg.path());
    let res = install(proj.path(), req("inbox-triage")).unwrap();
    assert_eq!(res["lifecycle"], "draft");
    assert_eq!(
        trust_bytes(cfg.path()),
        before,
        "the trust store is untouched"
    );
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "inbox-triage".into(),
        version: None,
    };
    for ack in [false, true] {
        let refusal = check_run(
            proj.path(),
            &wref,
            ack,
            false,
            &LaunchContext::dashboard(LaunchKind::Start),
        )
        .unwrap_err();
        assert_eq!(refusal["policy"], "draft_requires_trial", "ack={ack}");
    }
}

#[test]
fn an_installed_template_records_its_provenance() {
    let (proj, _cfg) = fixture();
    let template = apb_core::templates::get("inbox-triage").unwrap();
    install(proj.path(), req("inbox-triage")).unwrap();
    let raw =
        std::fs::read_to_string(playbook_dir(proj.path(), "inbox-triage").join("provenance.json"))
            .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["created_by"], "template");
    assert_eq!(v["template"], "inbox-triage");
    assert_eq!(v["template_digest"], template.summary.digest.as_str());
}

#[test]
fn install_as_another_id_renames_the_playbook() {
    let (proj, _cfg) = fixture();
    // A person's own playbook whose trigger is the template's: no trigger
    // dedup stands between the template and its draft (a capture of the
    // same text would answer `possible_duplicate`).
    let template = apb_core::templates::get("ci-failure-triage").unwrap();
    let when = "a CI job failed and its log needs a short triage";
    assert!(
        template.yaml.contains(when),
        "the template carries the trigger"
    );
    let own = format!(
        "schema: 2\nid: own-ci\nname: Own CI\nversion: 1.0.0\ntrigger:\n  when: [\"{when}\"]\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    );
    apb_core::versioning::save_definition(proj.path(), "own-ci", &own, None, true).unwrap();
    let res = install(
        proj.path(),
        InstallRequest {
            template: "ci-failure-triage",
            scope: "project",
            as_id: Some("my-inbox"),
            with_fit: false,
        },
    )
    .unwrap_or_else(|e| panic!("install: {e}"));
    assert_eq!(res["id"], "my-inbox", "{res}");
    assert!(res.get("rejected").is_none(), "{res}");
    let yaml =
        std::fs::read_to_string(playbook_dir(proj.path(), "my-inbox").join("1.0.0/playbook.yaml"))
            .unwrap();
    assert!(
        yaml.contains("id: my-inbox"),
        "the stored definition names the new id"
    );
    assert!(!playbook_dir(proj.path(), "ci-failure-triage").exists());
}

#[test]
fn a_second_install_is_a_duplicate() {
    let (proj, _cfg) = fixture();
    install(proj.path(), req("inbox-triage")).unwrap();
    let before = tree(&playbook_dir(proj.path(), "inbox-triage"));
    let res = install(proj.path(), req("inbox-triage")).unwrap();
    assert_eq!(res["rejected"], "duplicate_id");
    assert_eq!(
        tree(&playbook_dir(proj.path(), "inbox-triage")),
        before,
        "the first draft is byte for byte as it was"
    );
}

#[test]
fn a_reserved_or_unsafe_id_is_refused() {
    let (proj, _cfg) = fixture();
    for (as_id, says) in [
        (
            "apb-mine",
            apb_core::bundled::reserved_id_message("apb-mine"),
        ),
        ("../x", "invalid id `../x`".to_string()),
    ] {
        let e = install(
            proj.path(),
            InstallRequest {
                template: "inbox-triage",
                scope: "project",
                as_id: Some(as_id),
                with_fit: false,
            },
        )
        .unwrap_err();
        assert_eq!(e.to_string(), format!("engine error: {says}"), "{as_id}");
        assert!(
            !playbook_dir(proj.path(), as_id).exists(),
            "nothing written for {as_id}"
        );
    }
    assert!(!playbook_dir(proj.path(), "inbox-triage").exists());
}

#[test]
fn a_template_install_in_a_run_step_is_refused() {
    let (proj, _cfg) = fixture();
    unsafe {
        std::env::set_var("APB_RUN_ID", "run-1");
    }
    let _run = RunStepEnv;
    let e = install(proj.path(), req("inbox-triage")).unwrap_err();
    assert!(e.to_string().contains(IN_RUN_STEP), "{e}");
    // Listing and showing still answer.
    assert!(list()["templates"].as_array().unwrap().len() >= 15);
    assert!(show("inbox-triage").is_ok());
}

#[test]
fn the_install_answer_carries_the_fit_report() {
    let (proj, _cfg) = fixture();
    let res = install(
        proj.path(),
        InstallRequest {
            template: "dependency-security-fix",
            scope: "project",
            as_id: None,
            with_fit: true,
        },
    )
    .unwrap();
    let fit = &res["fit"];
    assert!(fit.is_object(), "fit is the whole report");
    let text = serde_json::to_string(fit).unwrap();
    assert!(
        text.contains("github"),
        "the fit names the missing setup: {text}"
    );
    let needs = res["needs"].as_array().unwrap();
    assert!(
        needs
            .iter()
            .any(|n| n.as_str().is_some_and(|s| s.contains("github"))),
        "needs names github: {needs:?}"
    );
    // Without a project named: no fit report, a warning says so.
    let res = install(
        proj.path(),
        InstallRequest {
            with_fit: false,
            ..req("inbox-triage")
        },
    )
    .unwrap();
    assert!(res["fit"].is_null());
    assert_eq!(res["needs"], json!([]));
    let warnings = res["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|s| s.contains("no project was named"))),
        "{warnings:?}"
    );
}

#[test]
fn scenarios_are_installed_with_the_draft() {
    let (proj, _cfg) = fixture();
    let res = install(proj.path(), req("task-check-first")).unwrap();
    assert_eq!(res["scenarios"], 3);
    let scenarios = playbook_dir(proj.path(), "task-check-first").join("evals/scenarios");
    let files: Vec<_> = std::fs::read_dir(&scenarios)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(files.len(), 3);
}

#[test]
fn an_unknown_template_is_not_found() {
    let (proj, _cfg) = fixture();
    let e = install(proj.path(), req("no-such-template")).unwrap_err();
    assert!(matches!(e, apb_mcp::tools::ToolError::NotFound(_)), "{e:?}");
    assert!(show("no-such-template").is_err());
}

#[test]
fn a_global_install_writes_the_global_store() {
    let (proj, cfg) = fixture();
    let res = install(
        proj.path(),
        InstallRequest {
            template: "inbox-triage",
            scope: "global",
            as_id: None,
            with_fit: false,
        },
    )
    .unwrap();
    assert_eq!(res["scope"], "global");
    assert!(
        cfg.path()
            .join("playbooks/inbox-triage/1.0.0/playbook.yaml")
            .is_file()
    );
    assert!(!playbook_dir(proj.path(), "inbox-triage").exists());
}

#[test]
fn an_install_never_lowers_a_launch_floor() {
    let (proj, _cfg) = fixture();
    // The id kept a floor stricter than the template's launch policy: a
    // saved playbook of this id with `agent_launch: deny`, then deleted
    // (the floor outlives its playbook).
    let deny = "schema: 2\nid: inbox-triage\nname: Deny\nversion: 1.0.0\nagent_launch: deny\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
    apb_core::versioning::save_definition(proj.path(), "inbox-triage", deny, None, true).unwrap();
    apb_core::versioning::delete_playbook(proj.path(), "inbox-triage", 1).unwrap();
    let e = install(proj.path(), req("inbox-triage")).unwrap_err();
    assert!(e.to_string().contains("launch_policy_loosened"), "{e}");
    assert!(!playbook_dir(proj.path(), "inbox-triage").exists());
}

#[test]
fn a_previously_approved_digest_is_reported_as_it_is() {
    let (proj, cfg) = fixture();
    install(proj.path(), req("inbox-triage")).unwrap();
    let loaded = apb_core::registry::Registry::open_dir(&proj.path().join(".apb"))
        .unwrap()
        .load("inbox-triage", Some("1.0.0"))
        .unwrap();
    let digest = loaded.trust_digest().unwrap();
    std::fs::remove_dir_all(playbook_dir(proj.path(), "inbox-triage")).unwrap();
    apb_core::trust::TrustStore::load()
        .approve(
            &digest,
            "inbox-triage",
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .unwrap();
    let after_manual = trust_bytes(cfg.path());
    let res = install(
        proj.path(),
        InstallRequest {
            template: "inbox-triage",
            scope: "project",
            as_id: None,
            with_fit: true,
        },
    )
    .unwrap();
    let trust = res["fit"]["trust"].as_array().unwrap();
    assert!(
        trust
            .iter()
            .any(|t| t["kind"] == "playbook" && t["approved"] == true),
        "the fit says the playbook is approved: {trust:?}"
    );
    assert_eq!(
        trust_bytes(cfg.path()),
        after_manual,
        "the install approved nothing"
    );
}

#[test]
fn a_failed_eval_copy_is_a_warning() {
    let (proj, _cfg) = fixture();
    // Another install holds the eval tree's lock (a fresh lock file in the
    // playbook directory, the only entry there, which a draft creation
    // accepts as a crash leftover): the eval copy fails, the draft stays.
    let dir = playbook_dir(proj.path(), "task-check-first");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".evals.install.lock"), b"held").unwrap();
    let res = install(proj.path(), req("task-check-first")).unwrap();
    assert!(
        dir.join("1.0.0/playbook.yaml").is_file(),
        "the draft exists"
    );
    assert_eq!(res["lifecycle"], "draft");
    assert_eq!(res["scenarios"], 0);
    assert!(!dir.join("evals").exists());
    let warnings = res["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|s| s.starts_with("eval scenarios not installed: "))),
        "{warnings:?}"
    );
}

/// A fit report that could not be made and a provenance that was not
/// written reach the install answer as warnings; the draft stays.
#[test]
fn a_failed_fit_and_provenance_are_warnings() {
    use apb_mcp::tools::install_faults::{Fault, inject};
    let (proj, _cfg) = fixture();
    let _faults = inject(&[Fault::Fit, Fault::Provenance]);
    let mut r = req("task-check-first");
    r.with_fit = true;
    let res = install(proj.path(), r).unwrap();
    let dir = playbook_dir(proj.path(), "task-check-first");
    assert!(
        dir.join("1.0.0/playbook.yaml").is_file(),
        "the draft exists"
    );
    assert!(!dir.join("provenance.json").exists());
    assert_eq!(res["fit"], serde_json::Value::Null);
    let warnings: Vec<&str> = res["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("fit report not made: ") && w.contains("injected")),
        "{warnings:?}"
    );
    assert!(warnings.contains(&"provenance not written"), "{warnings:?}");
}

/// A project whose `.apb/playbooks` is a link to the global store's
/// `playbooks`: a project install and a project capture are both refused
/// `store_path_linked` before any write, and neither store changes.
#[cfg(unix)]
#[test]
fn a_linked_store_path_is_refused_for_install_and_capture() {
    let (proj, cfg) = fixture();
    let global = cfg.path().join("playbooks");
    std::fs::create_dir_all(&global).unwrap();
    let project_playbooks = proj.path().join(".apb/playbooks");
    let _ = std::fs::remove_dir_all(&project_playbooks);
    std::os::unix::fs::symlink(&global, &project_playbooks).unwrap();
    let before_global = tree(cfg.path());
    let before_project = tree(&proj.path().join(".apb"));

    let e = install(proj.path(), req("inbox-triage")).unwrap_err();
    assert!(e.to_string().contains("store_path_linked"), "{e}");
    let yaml = apb_core::templates::get("inbox-triage")
        .unwrap()
        .yaml
        .replace("id: inbox-triage", "id: captured");
    let e = apb_mcp::tools::playbook_capture(
        proj.path(),
        &json!({ "summary": "triage" }),
        "project",
        &yaml,
    )
    .unwrap_err();
    assert!(e.to_string().contains("store_path_linked"), "{e}");

    assert_eq!(
        tree(cfg.path()),
        before_global,
        "the global store is unchanged"
    );
    assert_eq!(
        tree(&proj.path().join(".apb")),
        before_project,
        "the project store is unchanged"
    );
    assert!(!global.join("inbox-triage").exists());
    assert!(!global.join("captured").exists());
}

/// A store root a person links on purpose is followed: with the config
/// dir itself a link, a global install and a global capture land in the
/// directory it names; with the project's `.apb` a link, a project install
/// and a project capture land in the directory it names.
#[cfg(unix)]
#[test]
fn a_linked_store_root_is_followed_for_install_and_capture() {
    let (proj, cfg) = fixture();
    let aliases = tempfile::tempdir().unwrap();
    let cfg_link = aliases.path().join("config");
    std::os::unix::fs::symlink(cfg.path(), &cfg_link).unwrap();
    // SAFETY: the sandbox holds the env lock and restores the variable.
    unsafe { std::env::set_var("APB_CONFIG_DIR", &cfg_link) };

    let mut global = req("inbox-triage");
    global.scope = "global";
    let res = install(proj.path(), global).unwrap();
    assert_eq!(res["scope"], "global", "{res}");
    assert!(
        cfg.path()
            .join("playbooks/inbox-triage/1.0.0/playbook.yaml")
            .is_file()
    );
    let yaml = |id: &str| {
        apb_core::templates::get("inbox-triage")
            .unwrap()
            .yaml
            .replace("id: inbox-triage", &format!("id: {id}"))
    };
    let res = apb_mcp::tools::playbook_capture(
        proj.path(),
        &json!({ "summary": "triage" }),
        "global",
        &yaml("captured-global"),
    )
    .unwrap();
    assert!(
        res["scope"] == "global",
        "the capture is not in the global scope"
    );
    assert!(
        cfg.path()
            .join("playbooks/captured-global/1.0.0/playbook.yaml")
            .is_file()
    );

    // The project's `.apb` moved elsewhere and linked back.
    let real_apb = aliases.path().join("real-apb");
    std::fs::rename(proj.path().join(".apb"), &real_apb).unwrap();
    std::os::unix::fs::symlink(&real_apb, proj.path().join(".apb")).unwrap();
    let res = install(proj.path(), req("task-check-first")).unwrap();
    assert_eq!(res["scope"], "project", "{res}");
    assert!(
        real_apb
            .join("playbooks/task-check-first/1.0.0/playbook.yaml")
            .is_file()
    );
    let res = apb_mcp::tools::playbook_capture(
        proj.path(),
        &json!({ "summary": "triage" }),
        "project",
        &yaml("captured-project"),
    )
    .unwrap();
    assert!(
        res["scope"] == "project",
        "the capture is not in the project scope"
    );
    assert!(
        real_apb
            .join("playbooks/captured-project/1.0.0/playbook.yaml")
            .is_file()
    );
}

/// Beneath a store's root nothing is followed: a link at `playbooks` and
/// a link at the playbook's own directory are each refused
/// `store_path_linked` for an install and a capture, in both scopes, and
/// the directory the link names stays empty.
#[cfg(unix)]
#[test]
fn a_link_beneath_the_store_root_is_refused_in_both_scopes() {
    for scope in ["project", "global"] {
        for at_playbook_dir in [false, true] {
            let (proj, cfg) = fixture();
            let parent = if scope == "global" {
                cfg.path().to_path_buf()
            } else {
                proj.path().join(".apb")
            };
            let elsewhere = tempfile::tempdir().unwrap();
            let playbooks = parent.join("playbooks");
            std::fs::create_dir_all(&playbooks).unwrap();
            let ids = ["inbox-triage", "captured"];
            if at_playbook_dir {
                for id in ids {
                    let target = elsewhere.path().join(id);
                    std::fs::create_dir_all(&target).unwrap();
                    std::os::unix::fs::symlink(&target, playbooks.join(id)).unwrap();
                }
            } else {
                std::fs::remove_dir_all(&playbooks).unwrap();
                std::os::unix::fs::symlink(elsewhere.path(), &playbooks).unwrap();
            }
            let what = format!("{scope}, link at the playbook dir: {at_playbook_dir}");

            let mut r = req("inbox-triage");
            r.scope = scope;
            let e = install(proj.path(), r).unwrap_err();
            assert!(e.to_string().contains("store_path_linked"), "{what}: {e}");
            let yaml = apb_core::templates::get("inbox-triage")
                .unwrap()
                .yaml
                .replace("id: inbox-triage", "id: captured");
            let e = apb_mcp::tools::playbook_capture(
                proj.path(),
                &json!({ "summary": "triage" }),
                scope,
                &yaml,
            )
            .unwrap_err();
            assert!(e.to_string().contains("store_path_linked"), "{what}: {e}");
            assert!(
                tree(elsewhere.path()).is_empty(),
                "{what}: nothing written where the link points"
            );
        }
    }
}
