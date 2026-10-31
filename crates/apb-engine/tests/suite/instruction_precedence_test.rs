use apb_core::registry::init_project;
use apb_core::run_memory::{MemoryKey, RememberInput, remember};
use apb_core::scope::Scope;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::run_config::read_run_config;
use apb_engine::scheduler::{RunOptions, run};
use std::fs;
use std::path::Path;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.instruction}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}

// A `prompt` node's output IS its rendered prompt text (`node.rs::execute_node`,
// `NodeKind::Prompt` arm) - the same `render_node_prompt`/`build_context_for_render`
// path every real agent prompt goes through. Using `{{run.context}}` here (instead
// of `{{run.instruction}}` as in `seed` above) exercises the ACTUAL live render
// path a stub/real node sees, not just the context.md materialized file.
fn seed_context_prompt(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/ctxprompt/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: ctxprompt\nname: ctxprompt\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.context}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/ctxprompt/current"), "1.0.0").unwrap();
}

/// Writes the legacy draft a project may have committed under `meta/`.
fn write_legacy_draft(root: &Path, id: &str, text: &str) {
    let meta = root.join(".apb/playbooks").join(id).join("meta");
    fs::create_dir_all(&meta).unwrap();
    fs::write(meta.join("instruction-draft.md"), text).unwrap();
}

fn instruction_of(root: &Path, run_id: &str) -> Option<String> {
    read_run_config(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .instruction
}

#[test]
fn draft_used_when_no_explicit_instruction() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    write_legacy_draft(dir.path(), "p", "from draft");

    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();
    assert_eq!(
        instruction_of(dir.path(), &res.run_id).as_deref(),
        Some("from draft")
    );
}

#[test]
fn explicit_instruction_beats_draft() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    write_legacy_draft(dir.path(), "p", "from draft");

    let opts = RunOptions {
        instruction: Some("explicit".into()),
        ..Default::default()
    };
    let res = run(dir.path(), "p", None, opts).unwrap();
    assert_eq!(
        instruction_of(dir.path(), &res.run_id).as_deref(),
        Some("explicit")
    );
}

#[test]
fn none_when_no_draft_and_no_explicit() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();
    assert_eq!(instruction_of(dir.path(), &res.run_id), None);
}

// Task 4 completion-plan defect 3: the run instruction used to be rendered
// only where a playbook author referenced `{{run.instruction}}` explicitly -
// a summarizing first node silently dropped it downstream for everyone else.
// `rebuild_context_md` now leads context.md with a `## run instruction`
// section whenever the run carries a non-empty instruction.
#[test]
fn context_md_leads_with_run_instruction_section_when_present() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let opts = RunOptions {
        instruction: Some("stay within budget".into()),
        ..Default::default()
    };
    let res = run(dir.path(), "p", None, opts).unwrap();

    let context_md = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&res.run_id)
            .join("context.md"),
    )
    .unwrap();
    assert!(
        context_md.starts_with("## run instruction\n\nstay within budget\n\n"),
        "expected context.md to lead with the run instruction section, got:\n{context_md}"
    );
}

#[test]
fn context_md_has_no_instruction_section_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();

    let context_md = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&res.run_id)
            .join("context.md"),
    )
    .unwrap();
    assert!(
        !context_md.contains("## run instruction"),
        "context.md must have no instruction section when the run has none, got:\n{context_md}"
    );
}

// Fix-review Important item: the context.md-only tests above do not prove
// the fix reaches an actual node prompt - `{{run.context}}` in a real render
// resolves through `build_context_for_render` (called from
// `render_node_prompt`), which never reads context.md back. This end-to-end
// run proves the live path: the `n` node's own output IS its rendered
// `{{run.context}}`, so its NodeFinished output must lead with the section.
#[test]
fn rendered_node_prompt_leads_with_run_instruction_when_present() {
    let dir = tempfile::tempdir().unwrap();
    seed_context_prompt(dir.path());

    let opts = RunOptions {
        instruction: Some("stay within budget".into()),
        ..Default::default()
    };
    let res = run(dir.path(), "ctxprompt", None, opts).unwrap();

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let rendered = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "n" => Some(output.clone()),
            _ => None,
        })
        .expect("expected a NodeFinished event for node `n`");
    assert!(
        rendered.starts_with("## run instruction\n\nstay within budget\n\n"),
        "expected the rendered node prompt ({{{{run.context}}}}) to lead with the run \
         instruction section, got:\n{rendered}"
    );
}

#[test]
fn param_default_is_filled_into_persisted_run_config() {
    // Review I6/R1-I2: a declared param the caller omits falls back to its
    // schema `default` in the SINGLE normalization point (prepare), for every
    // run - here a plain top-level run. The persisted RunConfig carries it.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/pd/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: pd\nname: pd\nversion: 1.0.0\nparams:\n  - { name: mode, type: text, default: \"fast\" }\n  - { name: given, type: text, default: \"unused\" }\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{params.mode}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/pd/current"), "1.0.0").unwrap();

    // Caller supplies `given` explicitly; `mode` is omitted and must default.
    let mut params = std::collections::BTreeMap::new();
    params.insert("given".to_string(), "explicit".to_string());
    let opts = RunOptions {
        params,
        ..Default::default()
    };
    let res = run(dir.path(), "pd", None, opts).unwrap();

    let cfg = read_run_config(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(
        cfg.params.get("mode").map(|s| s.as_str()),
        Some("fast"),
        "the omitted param takes its schema default"
    );
    assert_eq!(
        cfg.params.get("given").map(|s| s.as_str()),
        Some("explicit"),
        "an explicitly passed param is not overwritten by its default"
    );
}

const REMEMBERING: &str = "schema: 2\nid: m\nname: m\nversion: 1.0.0\nparams:\n  - { name: channel, type: enum, options: [beta, stable], default: stable }\n  - { name: tag, type: text, default: \"none\" }\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{params.channel}} {{run.instruction}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n";

fn seed_remembering(root: &Path) -> Vec<apb_core::schema::Param> {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/m/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), REMEMBERING).unwrap();
    fs::write(root.join(".apb/playbooks/m/current"), "1.0.0").unwrap();
    apb_core::schema::Playbook::from_yaml(REMEMBERING)
        .unwrap()
        .params
}

fn remember_in(root: &Path, params: &[apb_core::schema::Param], channel: &str, instruction: &str) {
    let key = MemoryKey::new(Scope::Project, "m").unwrap();
    let input = RememberInput {
        params: Some([("channel".to_string(), channel.to_string())].into()),
        instruction: Some(instruction.to_string()),
    };
    let done = remember(root, &key, params, "1.0.0", input).unwrap();
    assert!(done.refused.is_empty());
}

fn remembered_of(root: &Path, run_id: &str) -> Vec<String> {
    read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::RunProvenance { remembered, .. } => Some(remembered),
            _ => None,
        })
        .unwrap()
}

/// Explicit input beats remembered input, which beats the legacy draft and
/// the param default; the start records what it took from memory.
#[test]
fn remembered_input_sits_between_explicit_input_and_the_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let params = seed_remembering(dir.path());
    write_legacy_draft(dir.path(), "m", "from draft");
    remember_in(dir.path(), &params, "beta", "from memory");

    let res = run(dir.path(), "m", None, RunOptions::default()).unwrap();
    let cfg = read_run_config(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(cfg.instruction.as_deref(), Some("from memory"));
    assert_eq!(cfg.params.get("channel").map(String::as_str), Some("beta"));
    assert_eq!(cfg.params.get("tag").map(String::as_str), Some("none"));
    assert_eq!(
        remembered_of(dir.path(), &res.run_id),
        vec!["channel", "instruction"]
    );

    let opts = RunOptions {
        instruction: Some("explicit".into()),
        params: [("channel".to_string(), "stable".to_string())].into(),
        ..Default::default()
    };
    let res = run(dir.path(), "m", None, opts).unwrap();
    let cfg = read_run_config(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(cfg.instruction.as_deref(), Some("explicit"));
    assert_eq!(
        cfg.params.get("channel").map(String::as_str),
        Some("stable")
    );
    assert!(remembered_of(dir.path(), &res.run_id).is_empty());
}

/// An instruction cleared on purpose in memory stops the legacy draft.
#[test]
fn a_cleared_remembered_instruction_suppresses_the_legacy_draft() {
    let dir = tempfile::tempdir().unwrap();
    let params = seed_remembering(dir.path());
    write_legacy_draft(dir.path(), "m", "from draft");
    remember_in(dir.path(), &params, "beta", "");

    let res = run(dir.path(), "m", None, RunOptions::default()).unwrap();
    assert_eq!(instruction_of(dir.path(), &res.run_id), None);
}

/// A parent playbook running `m` as a sub-playbook.
fn seed_parent(root: &Path, id: &str, child: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!(
            "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\n  - {{ id: sub, type: playbook, playbook: {child} }}\n  - {{ id: f, type: finish, outcome: success }}\nedges:\n  - {{ from: s, to: sub }}\n  - {{ from: sub, to: f }}\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// The run config of the run of playbook `id` under `root`.
fn config_of_playbook(root: &Path, id: &str) -> apb_engine::run_config::RunConfig {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|dir| {
            read_all(dir).unwrap_or_default().iter().any(|e| {
                matches!(&e.payload, EventPayload::RunStarted { playbook, .. } if playbook == id)
            })
        })
        .map(|dir| read_run_config(&dir).unwrap())
        .expect("a run of the playbook")
}

/// A sub-playbook child takes no param values from memory: the parent's
/// consent and pins cover the child's declared defaults, not what someone
/// last typed for it.
#[test]
fn a_child_run_uses_its_defaults_not_its_remembered_values() {
    let dir = tempfile::tempdir().unwrap();
    let params = seed_remembering(dir.path());
    remember_in(dir.path(), &params, "beta", "from memory");
    seed_parent(dir.path(), "par", "m");

    run(dir.path(), "par", None, RunOptions::default()).unwrap();
    let child = config_of_playbook(dir.path(), "m");
    assert_eq!(
        child.params.get("channel").map(String::as_str),
        Some("stable")
    );
}

/// The legacy draft is read only when a start passes no instruction: one
/// that cannot be read fails such a start, never one with an instruction.
#[cfg(unix)]
#[test]
fn an_explicit_instruction_never_reads_the_legacy_draft() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    write_legacy_draft(dir.path(), "p", "from draft");
    let draft = dir
        .path()
        .join(".apb/playbooks/p/meta/instruction-draft.md");
    fs::set_permissions(&draft, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&draft).is_ok() {
        // Permissions do not bind this user (root): nothing to show here.
        return;
    }

    let opts = RunOptions {
        instruction: Some("explicit".into()),
        ..Default::default()
    };
    let res = run(dir.path(), "p", None, opts).unwrap();
    assert_eq!(
        instruction_of(dir.path(), &res.run_id).as_deref(),
        Some("explicit")
    );
    assert!(run(dir.path(), "p", None, RunOptions::default()).is_err());
}

/// A version an import wrote with scripts runs only once approved by
/// digest, however it is started: directly, or as a sub-playbook of a
/// parent no gate checked.
#[test]
fn an_unapproved_imported_version_never_runs() {
    let _env = crate::common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let bundle = apb_core::bundle::PlaybookBundle {
        apb_bundle: 2,
        id: "imp".to_string(),
        version: "1.0.0".to_string(),
        playbook: fs::read_to_string(dir.path().join(".apb/playbooks/p/1.0.0/playbook.yaml"))
            .unwrap()
            .replace("id: p\nname: p", "id: imp\nname: imp"),
        layout: None,
        scripts: vec![apb_core::bundle::BundleScript {
            path: "x.sh".to_string(),
            content_base64: "ZWNobwo=".to_string(),
            executable: false,
        }],
        origin: None,
    };
    apb_core::bundle::import_bundle(dir.path(), &bundle, true, None).unwrap();

    let err = run(dir.path(), "imp", None, RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("imported with scripts"), "{err}");

    seed_parent(dir.path(), "par", "imp");
    let res = run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_ne!(res.outcome.as_str(), "succeeded");
    // The parent failed for this reason, not another one.
    let journal = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(journal.contains("imported with scripts"), "{journal}");
}

/// A child also takes no instruction from memory or the old draft: only
/// what its parent passes.
#[test]
fn a_child_run_takes_no_remembered_instruction() {
    let dir = tempfile::tempdir().unwrap();
    let params = seed_remembering(dir.path());
    remember_in(dir.path(), &params, "beta", "from memory");
    write_legacy_draft(dir.path(), "m", "from draft");
    seed_parent(dir.path(), "par", "m");

    run(dir.path(), "par", None, RunOptions::default()).unwrap();
    assert_eq!(config_of_playbook(dir.path(), "m").instruction, None);
}
