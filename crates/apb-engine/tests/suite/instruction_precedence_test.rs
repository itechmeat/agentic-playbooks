use apb_core::registry::{Registry, init_project};
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

fn instruction_of(root: &Path, run_id: &str) -> Option<String> {
    read_run_config(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .instruction
}

#[test]
fn draft_used_when_no_explicit_instruction() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    Registry::open(dir.path())
        .unwrap()
        .write_instruction_draft("p", "from draft")
        .unwrap();

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
    Registry::open(dir.path())
        .unwrap()
        .write_instruction_draft("p", "from draft")
        .unwrap();

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
