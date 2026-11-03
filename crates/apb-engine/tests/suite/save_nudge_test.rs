//! The model signal of the end-of-turn save check: one yes or no question
//! through the standalone decider, read in any language (the request here
//! is Greek; nothing in apb lists words of any language). The request is
//! the person's own text, so it leaves the machine only when the person
//! turned the use on and allowed the `requests` class.

use apb_engine::decision::save_nudge;
use apb_engine::decision::standalone::StandaloneDecider;

use crate::common;

/// "Every Monday prepare the sales report."
const GREEK: &str = "\u{039A}\u{03AC}\u{03B8}\u{03B5} \u{0394}\u{03B5}\u{03C5}\u{03C4}\u{03AD}\u{03C1}\u{03B1} \u{03B5}\u{03C4}\u{03BF}\u{03AF}\u{03BC}\u{03B1}\u{03C3}\u{03B5} \u{03C4}\u{03B7}\u{03BD} \u{03B1}\u{03BD}\u{03B1}\u{03C6}\u{03BF}\u{03C1}\u{03AC}";

fn decider(cfg: &std::path::Path, root: &std::path::Path, body: &str) -> StandaloneDecider {
    std::fs::write(cfg.join(apb_core::decisions::DECISIONS_FILE), body).unwrap();
    let settings = apb_core::decisions::load_file(cfg).unwrap().unwrap();
    StandaloneDecider::with_settings(settings, root)
}

/// A file with a fake provider answering `p`, and `rest` after it.
fn fake(p: f64, rest: &str) -> String {
    format!(
        "version: 1\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      recurring: {{ type: noul, noul: {p} }}\n{rest}"
    )
}

/// What a person who turned the check on writes.
const ON: &str =
    "mode: advise\nuses: { save_nudge: { mode: advise } }\nprivacy: { send: [requests] }\n";

#[test]
fn the_request_is_sent_only_with_the_persons_consent() {
    let _l = common::env_lock();
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for (rest, why) in [
        ("mode: advise\n", "a provider alone"),
        (
            "mode: advise\nuses: { save_nudge: { mode: advise } }\n",
            "the use without the requests class",
        ),
        (
            "mode: advise\nuses: { save_nudge: { mode: advise } }\nprivacy: { send: [prompts, outputs] }\n",
            "prompts are not requests",
        ),
    ] {
        let d = decider(cfg.path(), root.path(), &fake(0.9, rest));
        assert_eq!(save_nudge::ask(&d, GREEK), None, "{why}");
    }
    let d = decider(cfg.path(), root.path(), &fake(0.9, ON));
    assert_eq!(save_nudge::ask(&d, GREEK), Some(true));
}

#[test]
fn the_model_judges_a_request_in_any_language() {
    let _l = common::env_lock();
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let d = decider(cfg.path(), root.path(), &fake(0.2, ON));
    assert_eq!(save_nudge::ask(&d, GREEK), Some(false));
    // The default cut is 0.7, inclusive; the use's threshold overrides it.
    let d = decider(cfg.path(), root.path(), &fake(0.7, ON));
    assert_eq!(save_nudge::ask(&d, GREEK), Some(true));
    let strict = ON.replace(
        "save_nudge: { mode: advise }",
        "save_nudge: { mode: advise, thresholds: { recurring: 0.95 } }",
    );
    let d = decider(cfg.path(), root.path(), &fake(0.9, &strict));
    assert_eq!(save_nudge::ask(&d, GREEK), Some(false));
    // A provider with no answer for the question: no signal.
    let d = decider(
        cfg.path(),
        root.path(),
        &format!("version: 1\nproviders:\n  - id: fake\n    kind: fake\n{ON}"),
    );
    assert_eq!(save_nudge::ask(&d, GREEK), None);
}

#[test]
fn a_shadow_answer_is_logged_and_never_reminds() {
    let _l = common::env_lock();
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    let shadow = ON.replace("mode: advise\nuses", "mode: shadow\nuses");
    let d = decider(cfg.path(), root.path(), &fake(0.9, &shadow));
    assert_eq!(save_nudge::ask(&d, GREEK), None);
    let log = std::fs::read_to_string(root.path().join(".apb/decisions.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(line["use_site"], "save_nudge");
    assert_eq!(line["mode"], "shadow");
    assert_eq!(line["provider"], "fake", "the question was asked: {line}");
}

/// The session may sit in any folder of a project: the project's opt-out
/// holds there too, and the request is redacted for that project (its root
/// folded out of paths), not for the hook's own folder.
#[test]
fn the_project_of_a_subdirectory_decides_and_redacts() {
    let _l = common::env_lock();
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    std::fs::write(
        cfg.path().join(apb_core::decisions::DECISIONS_FILE),
        fake(0.9, ON),
    )
    .unwrap();
    let project = tempfile::tempdir().unwrap();
    let project = std::fs::canonicalize(project.keep()).unwrap();
    let sub = project.join("crates/x");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(project.join(".apb")).unwrap();
    let hooks = tempfile::tempdir().unwrap();
    let request = format!("{}/notes/plan.md {GREEK}", project.display());

    assert_eq!(
        save_nudge::recurring(&sub, hooks.path(), &request),
        Some(true)
    );
    let log = std::fs::read_to_string(hooks.path().join(".apb/decisions.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    let sent = serde_json::json!({ "request": format!("notes/plan.md {GREEK}") });
    assert_eq!(line["state_digest"], apb_decide::digest::digest(&sent));

    std::fs::write(
        project.join(".apb/config.yaml"),
        "decisions: { enabled: false }\n",
    )
    .unwrap();
    assert_eq!(save_nudge::recurring(&sub, hooks.path(), &request), None);
    assert_eq!(
        save_nudge::recurring(&project, hooks.path(), &request),
        None
    );
    std::fs::remove_dir_all(&project).unwrap();
}

/// A long request in a script of several bytes per character still fits a
/// small state budget: it is clipped to the budget, not skipped.
#[test]
fn a_long_request_is_clipped_to_the_state_budget() {
    let _l = common::env_lock();
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let small = ON.replace(
        "send: [requests]",
        "send: [requests], max_state_bytes: 1024",
    );
    let d = decider(cfg.path(), root.path(), &fake(0.9, &small));
    let long = "\u{0416}\"".repeat(1000);
    assert_eq!(save_nudge::ask(&d, &long), Some(true));
}
