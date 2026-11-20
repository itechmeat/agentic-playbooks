//! A project with one finished run whose param `message` is declared
//! secret, echoed by its script nodes, and a child run that echoes the
//! parent's value it got in its own `instruction`, rendered from the
//! parent's echoed output (apb never renders a secret param, so the child
//! is handed no param of the parent). Shared by the HTTP and the CLI secret
//! tests.

use std::fs;
use std::path::Path;

/// The value of the secret param. Asserts below never print it.
pub const SECRET: &str = "violet hedgehog";

const PARENT: &str = r#"
schema: 2
id: secretpb
name: Secret parent
version: 1.0.0
params:
  - { name: message, type: text, secret: true }
nodes:
  - { id: start, type: start }
  - { id: echo, type: script, script: "scripts/echo.sh", runner: sh }
  - { id: child, type: playbook, playbook: secretchild, instruction: "{{nodes.echo.output}}" }
  - { id: boom, type: script, script: "scripts/boom.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: echo }
  - { from: echo, to: child }
  - { from: child, to: boom }
  - { from: boom, to: done }
"#;

const CHILD: &str = r#"
schema: 2
id: secretchild
name: Secret child
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: say, type: script, script: "scripts/say.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: say }
  - { from: say, to: done }
"#;

/// Sets `v` to the run's `params.message`, read from its own `run.yaml`
/// under the top-level `params:` key. A line cut, as the authoring guide
/// shows it: it reads a value YAML leaves plain, which the fixture's value
/// is.
macro_rules! read_message {
    () => {
        "v=$(awk '/^params:/ { p = 1; next } /^[^ ]/ { p = 0 } p && sub(/^  message: /, \"\") { print; exit }' \"$APB_RUN_DIR/run.yaml\")\n"
    };
}

/// Prints the run's `message` param to stdout and stderr.
const ECHO: &str = concat!(
    "#!/bin/sh\n",
    read_message!(),
    "echo \"said $v\"\necho \"stderr $v\" >&2\n"
);
/// Fails the node, with the value in what it printed.
const BOOM: &str = concat!(
    "#!/bin/sh\n",
    read_message!(),
    "echo \"failed on $v\"\necho \"failed on $v\" >&2\nexit 3\n"
);
/// The child prints its own `instruction`, the parent's echoed output
/// (`said <value>`), read from its own `run.yaml`: a value a step printed
/// reaches a child through a template that reads that step's output.
const SAY: &str = "#!/bin/sh\nv=$(awk 'sub(/^instruction: /, \"\") { print; exit }' \"$APB_RUN_DIR/run.yaml\")\necho \"child $v\"\n";

fn install(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// A project holding one finished (failed) run of the secret playbook and
/// its child; returns `(parent_run_id, child_run_id)`.
pub fn seed_secret_runs(root: &Path) -> (String, String) {
    apb_core::registry::init_project(root).unwrap();
    install(root, "secretchild", CHILD, &[("say.sh", SAY)]);
    install(
        root,
        "secretpb",
        PARENT,
        &[("echo.sh", ECHO), ("boom.sh", BOOM)],
    );
    let mut opts = apb_engine::RunOptions::default();
    opts.params.insert("message".into(), SECRET.into());
    let res = apb_engine::run(root, "secretpb", None, opts).unwrap();
    let child = apb_engine::list_runs(root)
        .unwrap()
        .into_iter()
        .find(|r| r.parent_run.as_deref() == Some(res.run_id.as_str()))
        .expect("the child run")
        .run_id;
    (res.run_id, child)
}

/// The secret run of [`seed_secret_runs`] as a run started before apb
/// refused to render a secret param: its snapshot renders `message` in a
/// prompt and declares `irreversible`, and no consent is recorded. With
/// `legacy` it is a schema-1 snapshot with executors and no manifest.
/// Returns the run id.
pub fn seed_rendering_run(root: &Path, legacy: bool) -> String {
    let (parent, _) = seed_secret_runs(root);
    let dir = root.join(".apb/runs").join(&parent);
    let head = if legacy {
        fs::remove_file(dir.join("manifest.yaml")).unwrap();
        "schema: 1\nid: secretpb\nname: Secret parent\nversion: 1.0.0\nexecutors:\n  main: { agent: claude, model: haiku }\ndefaults:\n  executor: main\n"
    } else {
        "schema: 2\nid: secretpb\nname: Secret parent\nversion: 1.0.0\n"
    };
    fs::write(
        dir.join("playbook.yaml"),
        format!(
            "{head}effects: [irreversible]\nparams:\n  - {{ name: message, type: text, secret: true }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: say, type: prompt, prompt: \"said {{{{params.message}}}}\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: say }}\n  - {{ from: say, to: done }}\n"
        ),
    )
    .unwrap();
    // The run is still one this machine created: its stamp covers the
    // manifest, which the legacy case removed.
    apb_core::run_origin::stamp(&dir, &parent).unwrap();
    parent
}
