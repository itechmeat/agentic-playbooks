//! History discovery through the tool layer every surface shares: a
//! discovery over a synthetic history root named in the global config, a
//! save through capture (`created_by: discover`), a decline that later
//! discoveries leave out, no absolute path in what is returned, and every
//! entry refused from a run step.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use apb_core::step_env::StepCaller;
use apb_mcp::tools::ToolError;
use apb_mcp::tools::discover::{
    DiscoverArgs, ProjectPick, SaveRequest, discover_dismiss, discover_save, result_get,
    results_list, run_discover,
};

use crate::common::{config_sandbox, registry_sandbox};

const SESSION: &str = include_str!("../../../apb-core/tests/fixtures/history/claude_session.jsonl");

/// A project under `base` with synthetic claude sessions of it on two
/// days (three of the fixture, three with `bun test` for `cargo test`, so
/// more than one sequence repeats), and the global config naming their directory as the root
/// `synthetic`. Returns the project root.
pub(crate) fn seed_history(base: &Path) -> PathBuf {
    let base = std::fs::canonicalize(base).unwrap();
    let project = base.join("work/proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude = base.join("claude/projects");
    let dir = claude.join(apb_core::agent_actions::claude::encode_cwd(
        &project.to_string_lossy(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, age) in [1u32, 1, 2, 1, 1, 2].iter().enumerate() {
        let path = dir.join(format!("s{i}.jsonl"));
        let mut text = SESSION.replace(
            "__CWD__",
            serde_json::to_string(&project).unwrap().trim_matches('"'),
        );
        if i >= 3 {
            text = text.replace("cargo test", "bun test");
        }
        std::fs::write(&path, text).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(86_400) * *age)
            .unwrap();
    }
    let config = apb_core::config::config_dir().unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.yaml"),
        format!(
            "discover:\n  roots:\n    - {{ name: synthetic, agent: claude_code, path: {} }}\n",
            claude.display()
        ),
    )
    .unwrap();
    project
}

fn args(project: &Path) -> DiscoverArgs {
    DiscoverArgs {
        projects: vec![ProjectPick::Root(project.to_path_buf())],
        ..DiscoverArgs::default()
    }
}

fn person() -> StepCaller {
    StepCaller::default()
}

#[test]
fn a_discovery_is_saved_through_capture_and_a_decline_is_left_out_later() {
    let _cfg = config_sandbox();
    let base = tempfile::tempdir().unwrap();
    let project = seed_history(base.path());
    apb_core::registry::init_project(&project).unwrap();

    let view = run_discover(&args(&project), &person()).unwrap();
    assert!(view.signed && view.stored);
    assert_eq!(view.result.read.sessions, 6);
    assert_eq!(view.result.request.roots, vec!["synthetic".to_string()]);
    assert!(view.result.suggestions.len() >= 2, "{:?}", view.result);
    let json = serde_json::to_string(&view).unwrap();
    assert!(
        !json.contains(&*base.path().to_string_lossy()),
        "no absolute path is returned"
    );
    assert!(!json.contains("\"mac\""));
    let id = view.result.id.clone();
    let first = view.result.suggestions[0].slug.clone();
    let second = view.result.suggestions[1].slug.clone();

    let saved = discover_save(
        &project,
        SaveRequest {
            id: &id,
            slug: &first,
            scope: "project",
            yaml: None,
        },
        &person(),
    )
    .unwrap();
    assert_eq!(saved["lifecycle"], "draft", "{saved}");
    assert_eq!(saved["provenance"]["created_by"], "discover");
    assert_eq!(saved["provenance"]["pattern"], first.as_str());
    assert_eq!(saved["provenance"]["result"], id.as_str());
    let again = SaveRequest {
        id: &id,
        slug: &first,
        scope: "project",
        yaml: None,
    };
    assert!(matches!(
        discover_save(&project, again, &person()),
        Err(ToolError::Conflict(_))
    ));

    let declined = discover_dismiss(&project, &id, &second, "soft", None, &person()).unwrap();
    assert_eq!(declined["pattern"], second.as_str());
    assert_eq!(declined["kind"], "soft");

    let stored = result_get(&id, &person()).unwrap();
    let state = |slug: &str| {
        let s = stored
            .result
            .suggestions
            .iter()
            .find(|s| s.slug == slug)
            .unwrap();
        serde_json::to_value(&s.status).unwrap()["state"].clone()
    };
    assert_eq!(state(&first), "saved");
    assert_eq!(state(&second), "dismissed");
    let list = results_list(&person()).unwrap();
    assert_eq!((list[0].saved, list[0].dismissed), (1, 1));

    let later = run_discover(&args(&project), &person()).unwrap();
    assert!(later.result.suppressed.iter().any(|s| s.slug == second));
    assert!(later.result.suggestions.iter().all(|s| s.slug != second));
    // The saved one is not offered again either.
    let saved_again = later.result.suppressed.iter().find(|s| s.slug == first);
    assert_eq!(
        saved_again.map(|s| (s.kind.as_str(), s.scope.as_str())),
        Some(("saved", "project"))
    );
    assert!(
        later
            .suppressed_suggestions
            .iter()
            .any(|r| r["pattern"] == second.as_str())
    );
}

#[test]
fn every_discovery_entry_is_refused_from_a_run_step() {
    let _cfg = config_sandbox();
    let base = tempfile::tempdir().unwrap();
    let project = seed_history(base.path());
    let id = run_discover(&args(&project), &person()).unwrap().result.id;
    let step = StepCaller {
        run_id: Some("r1".into()),
        host_task: None,
    };
    let refused = |r: Result<(), ToolError>| match r {
        Err(ToolError::Conflict(why)) => assert!(why.contains("inside run `r1`"), "{why}"),
        other => panic!("not refused: {other:?}"),
    };
    refused(run_discover(&args(&project), &step).map(|_| ()));
    refused(results_list(&step).map(|_| ()));
    refused(result_get(&id, &step).map(|_| ()));
    let save = SaveRequest {
        id: &id,
        slug: "seq-0000000000",
        scope: "project",
        yaml: None,
    };
    refused(discover_save(&project, save, &step).map(|_| ()));
    refused(discover_dismiss(&project, &id, "seq-0000000000", "soft", None, &step).map(|_| ()));
}

#[test]
fn an_unknown_root_or_a_missing_result_is_refused() {
    let _cfg = config_sandbox();
    let base = tempfile::tempdir().unwrap();
    let project = seed_history(base.path());
    let mut a = args(&project);
    a.roots = vec!["elsewhere".into()];
    match run_discover(&a, &person()) {
        Err(ToolError::Engine(why)) => assert!(why.contains("unknown history root"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        result_get("disc-0123456789", &person()),
        Err(ToolError::NotFound(_))
    ));
}

/// The yaml saved for playbook `id` under `parent`.
fn saved_yaml(parent: &Path, id: &str) -> String {
    std::fs::read_to_string(
        parent
            .join("playbooks")
            .join(id)
            .join("1.0.0/playbook.yaml"),
    )
    .unwrap()
}

/// The default profile the playbook `id` saved under `parent` names.
fn saved_profile(parent: &Path, id: &str) -> String {
    let doc: serde_json::Value = yaml_serde::from_str(&saved_yaml(parent, id)).unwrap();
    doc["defaults"]["profile"]["name"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_rewritten_draft_is_marked_edited_and_its_suggestion_left_out_by_slug() {
    let _cfg = config_sandbox();
    let base = tempfile::tempdir().unwrap();
    let project = seed_history(base.path());
    apb_core::registry::init_project(&project).unwrap();
    let view = run_discover(&args(&project), &person()).unwrap();
    let s = &view.result.suggestions[0];
    let mut doc: serde_json::Value = yaml_serde::from_str(&s.yaml).unwrap();
    doc["id"] = "ship-flow".into();
    doc["name"] = "Ship the fix".into();
    let rewritten = yaml_serde::to_string(&doc).unwrap();
    let saved = discover_save(
        &project,
        SaveRequest {
            id: &view.result.id,
            slug: &s.slug,
            scope: "project",
            yaml: Some(&rewritten),
        },
        &person(),
    )
    .unwrap();
    assert_eq!(saved["id"], "ship-flow", "{saved}");
    assert_eq!(saved["provenance"]["created_by"], "discover");
    assert_eq!(saved["provenance"]["pattern"], s.slug.as_str());
    assert_eq!(saved["provenance"]["edited"], true);
    assert!(saved_yaml(&project.join(".apb"), "ship-flow").contains("Ship the fix"));
    // No playbook is named after the slug, and still the next discovery
    // leaves it out as saved.
    assert!(!project.join(".apb/playbooks").join(&s.slug).exists());
    let later = run_discover(&args(&project), &person()).unwrap();
    assert!(later.result.suggestions.iter().all(|x| x.slug != s.slug));
    assert!(
        later
            .result
            .suppressed
            .iter()
            .any(|x| x.slug == s.slug && x.kind == "saved")
    );
}

#[test]
fn the_own_draft_names_the_profile_of_the_place_it_is_saved_in() {
    let _cfg = config_sandbox();
    let base = tempfile::tempdir().unwrap();
    let project = seed_history(base.path());
    apb_core::registry::init_project(&project).unwrap();
    // Discovered while the project had no profile: the draft names `main`.
    let view = run_discover(&args(&project), &person()).unwrap();
    let first = view.result.suggestions[0].clone();
    let second = view.result.suggestions[1].clone();
    assert!(first.yaml.contains("profile: main"), "{}", first.yaml);
    std::fs::create_dir_all(project.join(".apb/profiles/coder")).unwrap();
    let global = apb_core::config::config_dir().unwrap();
    std::fs::create_dir_all(global.join("profiles/reviewer")).unwrap();
    let save = |s: &apb_core::discover::Suggestion, scope: &str| {
        discover_save(
            &project,
            SaveRequest {
                id: &view.result.id,
                slug: &s.slug,
                scope,
                yaml: None,
            },
            &person(),
        )
        .unwrap()
    };
    let saved = save(&first, "project");
    assert_eq!(saved["provenance"]["edited"], false, "{saved}");
    assert_eq!(saved_profile(&project.join(".apb"), &first.slug), "coder");
    // A global playbook resolves only global profiles.
    let saved = save(&second, "global");
    assert_eq!(saved["scope"], "global", "{saved}");
    assert_eq!(saved_profile(&global, &second.slug), "reviewer");
}

#[test]
fn a_save_without_a_project_lands_in_the_one_the_id_names_not_its_namesake() {
    let _cfg = registry_sandbox();
    let base = tempfile::tempdir().unwrap();
    let project = seed_history(base.path());
    // A registered project of the same name, registered first.
    let twin = std::fs::canonicalize(base.path())
        .unwrap()
        .join("other/proj");
    std::fs::create_dir_all(&twin).unwrap();
    apb_core::registry::init_project(&twin).unwrap();
    apb_core::projects::touch(&twin);
    apb_core::registry::init_project(&project).unwrap();
    apb_core::projects::touch(&project);
    let view = run_discover(&args(&project), &person()).unwrap();
    let s = &view.result.suggestions[0];
    let own_id = apb_core::workspace::read_id(&project).unwrap();
    assert_eq!(s.support.projects[0].workspace_id, own_id);
    assert_eq!(s.support.projects[0].name, "proj");
    let home = apb_mcp::tools::discover::home_project(&view.result.id, &s.slug).unwrap();
    assert_eq!(home, project);
    assert!(matches!(
        apb_mcp::tools::discover::home_project("disc-0123456789", &s.slug),
        Err(ToolError::NotFound(_))
    ));
    // A stored result lists the declines that hold in its projects now.
    discover_dismiss(&project, &view.result.id, &s.slug, "soft", None, &person()).unwrap();
    let stored = result_get(&view.result.id, &person()).unwrap();
    assert!(
        stored
            .suppressed_suggestions
            .iter()
            .any(|r| r["pattern"] == s.slug.as_str())
    );
}
