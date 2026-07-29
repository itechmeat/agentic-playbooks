use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_mcp::policy::check_run;
use apb_mcp::tools::{DismissRequest, playbook_capture, playbook_catalog, suggestion_dismiss};
use serde_json::json;

use crate::common::env_lock as lock;

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

fn setup(cfg: &Path) {
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
}

fn good_yaml(id: &str) -> String {
    format!(
        "schema: 1\nid: {id}\nname: {id}\nversion: 1.0.0\ntrigger:\n  when: [\"use when {id}\"]\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n",
    )
}

#[test]
fn capture_creates_draft_with_provenance() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let synopsis =
        json!({ "title": "Nightly cleanup", "trigger": { "when": ["run nightly cleanup"] } });
    let res = playbook_capture(proj.path(), &synopsis, "project", &good_yaml("cleanup")).unwrap();

    assert_eq!(res["lifecycle"], "draft");
    assert_eq!(res["trusted"], false);
    assert_eq!(res["provenance"]["created_by"], "agent-capture");

    // A draft does not pass the run gate.
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "cleanup".into(),
        version: None,
    };
    let refusal = check_run(proj.path(), &wref, false, false).unwrap_err();
    assert_eq!(refusal["policy"], "draft_requires_trial");
}

#[test]
fn capture_rejects_secret_like_values() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let synopsis = json!({ "title": "Deploy", "token": "abcd1234efgh5678 zz" });
    // A secret directly in the synopsis.
    let synopsis_secret = json!({ "note": "api_key: abcd1234efgh5678" });
    let res =
        playbook_capture(proj.path(), &synopsis_secret, "project", &good_yaml("dep")).unwrap();
    assert_eq!(res["rejected"], "secret_like_value");
    let _ = synopsis;
}

#[test]
fn capture_rejects_duplicate_id() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let synopsis = json!({ "title": "First", "trigger": { "when": ["do first thing"] } });
    playbook_capture(proj.path(), &synopsis, "project", &good_yaml("dup")).unwrap();
    // Second capture of the same id (a different trigger so possible_duplicate does not fire).
    let synopsis2 =
        json!({ "title": "Second", "trigger": { "when": ["do a totally different thing"] } });
    let res = playbook_capture(proj.path(), &synopsis2, "project", &good_yaml("dup")).unwrap();
    assert_eq!(res["rejected"], "duplicate_id");
}

#[test]
fn old_style_dismiss_call_is_a_hard_project_dismissal() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    // An old-style call: pattern only, no new fields.
    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "save-cleanup-playbook",
            synopsis: "",
            kind: None,
            scope: None,
            ttl_days: None,
        },
    )
    .unwrap();
    assert_eq!(res["dismissed"], "save-cleanup-playbook");
    assert_eq!(res["kind"], "hard");
    assert_eq!(res["scope"], "project");
    assert_eq!(res["synopsis"], "");
    assert!(
        res["snoozed_until"].as_str().unwrap().ends_with('Z'),
        "the response reports the computed snooze: {res}"
    );

    let cat = playbook_catalog(proj.path(), None, None, None).unwrap();
    let dismissed = cat["dismissed_patterns"].as_array().unwrap();
    assert!(dismissed.iter().any(|p| p == "save-cleanup-playbook"));
}

#[test]
fn soft_dismiss_stores_synopsis_and_reports_the_escalating_snooze() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let req = || DismissRequest {
        pattern: "code-review-run",
        synopsis: "Review a source file for bugs and write findings to a report",
        kind: Some("soft"),
        scope: Some("project"),
        ttl_days: None,
    };
    let first = suggestion_dismiss(proj.path(), req()).unwrap();
    assert_eq!(first["kind"], "soft");
    assert_eq!(first["declines"], 1);
    assert_eq!(
        first["synopsis"],
        "Review a source file for bugs and write findings to a report"
    );
    let second = suggestion_dismiss(proj.path(), req()).unwrap();
    assert_eq!(second["declines"], 2);
    assert!(
        second["snoozed_until_ms"].as_u64().unwrap() > first["snoozed_until_ms"].as_u64().unwrap(),
        "the second soft decline snoozes further out: {second}"
    );
}

#[test]
fn global_scope_dismissal_is_recorded_globally() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "never-anywhere",
            synopsis: "Something the user never wants offered again anywhere",
            kind: Some("hard"),
            scope: Some("global"),
            ttl_days: None,
        },
    )
    .unwrap();
    assert_eq!(res["scope"], "global");
    assert!(cfg.path().join("suggestions.json").is_file());
    assert!(!proj.path().join(".apb/suggestions.json").exists());
}

#[test]
fn old_style_call_ttl_days_override_lands_in_snoozed_until() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;
    let before = apb_core::clock::now_ms_u64();
    // Legacy ttl_days argument: a hard dismissal (kind absent) whose TTL is
    // overridden to 5 days instead of the 90-day default.
    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "legacy-ttl-pattern",
            synopsis: "",
            kind: None,
            scope: None,
            ttl_days: Some(5),
        },
    )
    .unwrap();
    assert_eq!(res["kind"], "hard");
    let snoozed_until_ms = res["snoozed_until_ms"].as_u64().unwrap();
    let expected = before + 5 * MS_PER_DAY;
    // Allow a small tolerance for wall-clock time elapsed between `before`
    // and the call itself.
    let diff = snoozed_until_ms.abs_diff(expected);
    assert!(
        diff < 5_000,
        "ttl_days: 5 must override the 90-day default: snoozed_until_ms={snoozed_until_ms}, expected~={expected}"
    );
    assert!(
        snoozed_until_ms < before + 90 * MS_PER_DAY,
        "must not fall back to the 90-day default"
    );
}

/// `apb_core::dismiss::timing` is the only validator of the `suggestions:`
/// config section, and every production caller falls back to the defaults when
/// it is invalid. Unless the dismiss response carries the diagnostics, a typo
/// in that section is silently ignored on every surface at once, and the user
/// is left believing a schedule that is not in effect.
#[test]
fn a_malformed_suggestions_config_surfaces_diagnostics_in_the_response() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    std::fs::write(
        cfg.path().join("config.yaml"),
        "suggestions:\n  soft_backoff_days: []\n",
    )
    .unwrap();

    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "code-review-run",
            synopsis: "Review a source file and write findings to a report",
            kind: Some("soft"),
            scope: Some("project"),
            ttl_days: None,
        },
    )
    .unwrap();

    // The decision is still recorded: a broken config must never block a
    // decline from being honored.
    assert_eq!(res["declines"], 1, "{res}");
    let diagnostics = res["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("the response must carry diagnostics: {res}"));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.as_str().unwrap_or_default().contains("soft_backoff_days")),
        "the invalid key must be named: {res}"
    );
}

/// A clean config leaves the field out entirely: an always-present empty array
/// would train the agent to skip reading it.
#[test]
fn a_clean_config_leaves_the_diagnostics_field_out() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "clean-config-run",
            synopsis: "Nothing wrong with this config",
            kind: Some("soft"),
            scope: Some("project"),
            ttl_days: None,
        },
    )
    .unwrap();
    assert!(res.get("diagnostics").is_none(), "{res}");
}

/// A pattern that the CLI and the dashboard cannot address (their `is_safe_id`
/// rejects it) must never reach the store: such a record could be written but
/// never removed again.
#[test]
fn an_unaddressable_pattern_is_rejected() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    for bad in ["", "..", "a/b", "Mixed-Case", "dots.in.name"] {
        let res = suggestion_dismiss(
            proj.path(),
            DismissRequest {
                pattern: bad,
                synopsis: "Should not be stored",
                kind: Some("soft"),
                scope: Some("project"),
                ttl_days: None,
            },
        );
        assert!(res.is_err(), "`{bad}` must be rejected as a pattern");
    }
    assert!(
        !proj.path().join(".apb/suggestions.json").exists(),
        "a rejected pattern must not create a store"
    );

    let ok = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "perfectly-fine-slug",
            synopsis: "A well formed pattern still works",
            kind: Some("soft"),
            scope: Some("project"),
            ttl_days: None,
        },
    )
    .unwrap();
    assert_eq!(ok["pattern"], "perfectly-fine-slug");
}

#[test]
fn unknown_kind_or_scope_is_rejected() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let err = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "p",
            synopsis: "",
            kind: Some("maybe"),
            scope: None,
            ttl_days: None,
        },
    );
    assert!(err.is_err(), "an unknown kind must not be silently coerced");
    let err = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "p",
            synopsis: "",
            kind: None,
            scope: Some("everywhere"),
            ttl_days: None,
        },
    );
    assert!(err.is_err());
}
