//! History discovery's walk and suppression over synthetic sessions: whose
//! sessions a project is (nested and same-named projects), what each cap
//! stops, links and special files under the codex date tree, a rollout read
//! across its header without a lost byte, a decline kept to the projects
//! that saw the repeat, and a save recorded by slug.

use std::path::Path;

use apb_core::discover::{self, DiscoverResult, ProjectRef, SuggestionStatus, store, walk};
use serde_json::json;

use crate::common::config_sandbox;
use crate::history_fixture::{History, code_routine, codex_mcp, codex_meta};

fn root_report<'a>(r: &'a DiscoverResult, name: &str) -> &'a discover::RootReport {
    r.read.roots.iter().find(|x| x.name == name).unwrap()
}

/// Three sessions of the code routine in `p`, on three days.
fn routine(h: &History, p: &ProjectRef, prefix: &str) {
    for (i, age) in [1, 2, 3].iter().enumerate() {
        h.claude(
            p,
            &format!("{prefix}{i}"),
            &code_routine(&p.root, "x"),
            *age,
        );
    }
}

#[test]
fn a_project_registered_inside_a_selected_one_keeps_its_sessions() {
    let _cfg = config_sandbox();
    let h = History::new();
    let outer = h.project("outer");
    let inner_root = outer.root.join("inner");
    std::fs::create_dir_all(&inner_root).unwrap();
    let inner = ProjectRef {
        workspace_id: "ws-inner".into(),
        name: "inner".into(),
        root: inner_root,
    };
    routine(&h, &inner, "s");
    for i in 0..3 {
        h.codex(
            &format!("r{i}"),
            &[codex_meta(&inner.root), codex_mcp("crm", "update")],
            i + 1,
        );
    }
    // Not registered: the outer project reads them as its own.
    let r = discover::discover(&h.request(&[&outer])).unwrap();
    assert_eq!(r.read.sessions, 6);
    // Registered and not selected: they are the inner project's.
    let mut req = h.request(&[&outer]);
    req.registered = vec![outer.root.clone(), inner.root.clone()];
    let r = discover::discover(&req).unwrap();
    assert_eq!(r.read.sessions, 0);
    assert!(r.suggestions.is_empty());
    assert_eq!(
        root_report(&r, "claude_code").skipped.get("other_project"),
        Some(&3)
    );
    assert_eq!(
        root_report(&r, "codex").skipped.get("other_project"),
        Some(&3)
    );
    // Selected too: they are read as the inner project's.
    let mut req = h.request(&[&outer, &inner]);
    req.registered = vec![outer.root.clone(), inner.root.clone()];
    let r = discover::discover(&req).unwrap();
    assert_eq!(r.read.sessions, 6);
    assert!(
        r.suggestions
            .iter()
            .all(|s| s.support.projects == [inner.seen()])
    );
}

#[test]
fn projects_of_one_name_stay_apart_by_workspace_id() {
    let _cfg = config_sandbox();
    let h = History::new();
    let a = h.project("app");
    let b_root = h.dir.path().join("elsewhere/app");
    std::fs::create_dir_all(&b_root).unwrap();
    let b = ProjectRef {
        workspace_id: "ws-app-2".into(),
        name: "app".into(),
        root: std::fs::canonicalize(b_root).unwrap(),
    };
    for (i, age) in [1, 2].iter().enumerate() {
        h.claude(&a, &format!("a{i}"), &code_routine(&a.root, "x"), *age);
        h.claude(&b, &format!("b{i}"), &code_routine(&b.root, "x"), *age);
    }
    let r = discover::discover(&h.request(&[&a, &b])).unwrap();
    let s = &r.suggestions[0];
    assert_eq!(s.support.projects, [a.seen(), b.seen()]);
    let ids: std::collections::BTreeSet<&str> =
        s.sources.iter().map(|x| x.workspace_id.as_str()).collect();
    assert_eq!(ids, ["ws-app", "ws-app-2"].into_iter().collect());
    assert_eq!(r.request.projects, [a.seen(), b.seen()]);
}

#[test]
fn each_cap_stops_reading_and_says_so() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    routine(&h, &p, "s");
    // The byte cap: the first file takes it all.
    let mut req = h.request(&[&p]);
    req.limits.max_total_bytes = 200;
    let r = discover::discover(&req).unwrap();
    let c = root_report(&r, "claude_code");
    assert_eq!(c.skipped.get("byte_cap"), Some(&2));
    assert_eq!(c.files_read, 1);
    assert!(c.bytes_read <= 200);
    // The step cap: what the miner holds in memory, across sessions.
    let mut req = h.request(&[&p]);
    req.limits.max_total_steps = 6;
    let r = discover::discover(&req).unwrap();
    let c = root_report(&r, "claude_code");
    assert_eq!(c.skipped.get("step_cap"), Some(&1));
    assert_eq!(c.files_read, 2);
}

#[test]
fn another_projects_rollouts_do_not_use_up_the_file_cap() {
    let _cfg = config_sandbox();
    let h = History::new();
    let mine = h.project("mine");
    let other = h.project("other");
    // The other project's rollouts are the newest.
    for i in 0..3 {
        h.codex(
            &format!("o{i}"),
            &[codex_meta(&other.root), codex_mcp("crm", "update")],
            1,
        );
    }
    h.codex(
        "m",
        &[codex_meta(&mine.root), codex_mcp("crm", "update")],
        2,
    );
    let mut req = h.request(&[&mine]);
    req.limits.max_files = 1;
    let r = discover::discover(&req).unwrap();
    let x = root_report(&r, "codex");
    assert_eq!(x.skipped.get("other_project"), Some(&3));
    assert_eq!(x.files_read, 1);
    assert_eq!(x.skipped.get("file_cap"), None);
    assert_eq!(r.read.sessions, 1);
}

#[test]
fn a_rollout_past_its_header_loses_no_byte() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    // Alternating calls: every line is a step of its own.
    let mut lines = vec![codex_meta(&p.root)];
    for i in 0..1500 {
        let tool = if i % 2 == 0 { "update" } else { "read" };
        lines.push(codex_mcp("crm", tool));
    }
    let path = h.codex("big", &lines, 1);
    assert!(std::fs::metadata(&path).unwrap().len() > 2 * walk::CODEX_HEADER_BYTES);
    let mut req = h.request(&[&p]);
    req.limits.max_steps_per_session = 5000;
    let (reports, sessions) = walk::read_roots(&req, apb_core::clock::now_ms_u64());
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].steps.len(), 1500);
    let codex = reports.iter().find(|r| r.name == "codex").unwrap();
    assert_eq!(codex.skipped.get("line_too_long"), None);
    assert_eq!(codex.cut_files, 0);
}

#[cfg(unix)]
#[test]
fn links_and_special_files_in_the_trees_are_never_followed() {
    use std::os::unix::fs::symlink;
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    let real = h.codex(
        "real",
        &[codex_meta(&p.root), codex_mcp("crm", "update")],
        1,
    );
    let day = real.parent().unwrap().to_path_buf();
    let month = day.parent().unwrap().to_path_buf();
    let year = month.parent().unwrap().to_path_buf();
    // A linked year, month and day directory, each beside the real one.
    symlink(&year, h.codex_root().join("1999")).unwrap();
    symlink(&month, year.join("13")).unwrap();
    symlink(&day, month.join("32")).unwrap();
    // A fifo named like a rollout: never opened (it could block).
    let fifo = day.join("rollout-fifo.jsonl");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    // A linked project directory under the claude root.
    let elsewhere = h.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    symlink(
        &elsewhere,
        h.claude_root()
            .join(apb_core::agent_actions::claude::encode_cwd(
                &p.root.to_string_lossy(),
            )),
    )
    .unwrap();
    let r = discover::discover(&h.request(&[&p])).unwrap();
    let x = root_report(&r, "codex");
    assert_eq!(x.skipped.get("symlink"), Some(&3));
    assert_eq!(x.skipped.get("not_regular"), Some(&1));
    assert_eq!(x.files_read, 1);
    assert_eq!(
        root_report(&r, "claude_code").skipped.get("symlink"),
        Some(&1)
    );
    assert_eq!(r.read.sessions, 1);
}

#[test]
fn a_decline_holds_in_the_projects_that_saw_the_repeat() {
    use apb_core::dismiss::{DecisionInput, DecisionKind, DecisionScope, record_decision};
    let _cfg = config_sandbox();
    let h = History::new();
    let seen = h.project("seen");
    let quiet = h.project("quiet");
    routine(&h, &seen, "s");
    let slug = discover::discover(&h.request(&[&seen]))
        .unwrap()
        .suggestions[0]
        .slug
        .clone();
    let decline = |root: &Path| {
        record_decision(
            root,
            DecisionInput {
                pattern: slug.clone(),
                synopsis: "repeated code routine".into(),
                kind: DecisionKind::Soft,
                scope: DecisionScope::Project,
                hard_ttl_days_override: None,
            },
        )
        .unwrap();
    };
    // Declined in a project that never saw it: still offered.
    decline(&quiet.root);
    let r = discover::discover(&h.request(&[&seen, &quiet])).unwrap();
    assert_eq!(r.suggestions.len(), 1);
    assert!(r.suppressed.is_empty());
    // Declined where it was seen: left out.
    decline(&seen.root);
    let r = discover::discover(&h.request(&[&seen, &quiet])).unwrap();
    assert!(r.suggestions.is_empty());
    assert_eq!(r.suppressed[0].kind, "soft");
}

#[test]
fn a_save_is_resolved_once_and_suppresses_by_slug_whatever_its_id() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    routine(&h, &p, "s");
    let mut r = discover::discover(&h.request(&[&p])).unwrap();
    store::save(&mut r).unwrap();
    let slug = r.suggestions[0].slug.clone();
    let saved = SuggestionStatus::Saved {
        playbook: "ship-flow".into(),
        version: "0.1.0".into(),
        scope: "project".into(),
    };
    let mut runs = 0;
    let (out, unrecorded) = store::resolve(&r.id, &slug, |s| {
        runs += 1;
        assert_eq!(s.status, SuggestionStatus::New);
        Ok::<_, store::StoreError>(store::Outcome::Resolved(
            json!("done"),
            saved.clone(),
            Some(store::SavedIn::Project("ws-app".into())),
        ))
    })
    .unwrap();
    assert_eq!((out, unrecorded, runs), (json!("done"), None, 1));
    // A second action finds it resolved and does not run.
    let again = store::resolve(
        &r.id,
        &slug,
        |_| -> Result<store::Outcome<()>, store::StoreError> {
            unreachable!("a resolved suggestion is not acted on twice")
        },
    );
    assert_eq!(
        again.unwrap_err(),
        store::StoreError::Resolved(slug.clone(), saved.clone())
    );
    assert_eq!(
        store::load(&r.id).unwrap().result.suggestions[0].status,
        saved
    );
    // No playbook is named after the slug, yet the next discovery leaves
    // the suggestion out.
    assert!(!p.root.join(".apb/playbooks").join(&slug).exists());
    assert_eq!(
        store::saved(),
        [store::SavedSlug {
            slug: slug.clone(),
            saved_in: store::SavedIn::Project("ws-app".into()),
        }]
    );
    let next = discover::discover(&h.request(&[&p])).unwrap();
    assert!(next.suggestions.is_empty());
    assert_eq!(
        (
            next.suppressed[0].kind.as_str(),
            next.suppressed[0].scope.as_str()
        ),
        ("saved", "project")
    );
}

#[test]
fn a_refused_action_leaves_the_suggestion_new() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    routine(&h, &p, "s");
    let mut r = discover::discover(&h.request(&[&p])).unwrap();
    store::save(&mut r).unwrap();
    let slug = r.suggestions[0].slug.clone();
    let (out, _) = store::resolve(&r.id, &slug, |_| {
        Ok::<_, store::StoreError>(store::Outcome::Refused("refused"))
    })
    .unwrap();
    assert_eq!(out, "refused");
    assert_eq!(
        store::load(&r.id).unwrap().result.suggestions[0].status,
        SuggestionStatus::New
    );
    assert!(store::saved().is_empty());
}

/// Records a save of `slug` of the stored result `id` in `place`.
fn record_save(id: &str, slug: &str, place: store::SavedIn) {
    let status = SuggestionStatus::Saved {
        playbook: "ship-flow".into(),
        version: "1.0.0".into(),
        scope: place.scope().into(),
    };
    let (_, unrecorded) = store::resolve(id, slug, |_| {
        Ok::<_, store::StoreError>(store::Outcome::Resolved((), status, Some(place)))
    })
    .unwrap();
    assert_eq!(unrecorded, None);
}

#[test]
fn a_project_save_hides_a_repeat_only_there_and_a_global_one_everywhere() {
    let _cfg = config_sandbox();
    let h = History::new();
    let a = h.project("a");
    let b = h.project("b");
    routine(&h, &a, "a");
    routine(&h, &b, "b");
    let mut r = discover::discover(&h.request(&[&a])).unwrap();
    store::save(&mut r).unwrap();
    let slug = r.suggestions[0].slug.clone();
    record_save(&r.id, &slug, store::SavedIn::Project("ws-a".into()));
    let offered = |p: &ProjectRef| {
        discover::discover(&h.request(&[p]))
            .unwrap()
            .suggestions
            .iter()
            .any(|s| s.slug == slug)
    };
    assert!(!offered(&a));
    // Project b has no access to a's playbook: still offered there.
    assert!(offered(&b));
    let mut r = discover::discover(&h.request(&[&b])).unwrap();
    store::save(&mut r).unwrap();
    record_save(&r.id, &slug, store::SavedIn::Global);
    assert!(!offered(&b));
    let next = discover::discover(&h.request(&[&b])).unwrap();
    assert_eq!(next.suppressed[0].scope, "global");
}

#[test]
fn a_saved_record_that_does_not_read_is_never_replaced() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    routine(&h, &p, "s");
    let mut r = discover::discover(&h.request(&[&p])).unwrap();
    store::save(&mut r).unwrap();
    let dir =
        std::path::PathBuf::from(std::env::var_os("APB_CONFIG_DIR").unwrap()).join(store::DIR);
    std::fs::write(dir.join(store::SAVED), "not json").unwrap();
    let slug = r.suggestions[0].slug.clone();
    let status = SuggestionStatus::Saved {
        playbook: "x".into(),
        version: "1.0.0".into(),
        scope: "global".into(),
    };
    let (_, unrecorded) = store::resolve(&r.id, &slug, |_| {
        Ok::<_, store::StoreError>(store::Outcome::Resolved(
            (),
            status,
            Some(store::SavedIn::Global),
        ))
    })
    .unwrap();
    assert!(matches!(unrecorded, Some(store::StoreError::Io(_))));
    assert_eq!(
        std::fs::read_to_string(dir.join(store::SAVED)).unwrap(),
        "not json"
    );
}

#[test]
fn a_session_the_step_cap_stops_part_way_is_counted_as_cut() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    routine(&h, &p, "s");
    let mut req = h.request(&[&p]);
    // The newest session has 5 steps; the cap leaves 3 for the second.
    req.limits.max_total_steps = 8;
    let r = discover::discover(&req).unwrap();
    let c = root_report(&r, "claude_code");
    assert_eq!(c.cut_files, 1);
    assert_eq!(c.files_read, 2);
    assert_eq!(c.skipped.get("step_cap"), Some(&1));
}
