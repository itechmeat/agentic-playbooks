//! A project with one finished run whose param `message` is declared
//! secret, echoed by its script nodes and handed to a child run as the
//! child's instruction. Shared by the HTTP and the CLI secret tests.

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
  - { id: child, type: playbook, playbook: secretchild, instruction: "{{params.message}}" }
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

/// Prints the run's `message` param (read from its own `run.yaml`) to
/// stdout and stderr.
const ECHO: &str = "#!/bin/sh\nv=$(sed -n 's/^  message: //p' \"$APB_RUN_DIR/run.yaml\")\necho \"said $v\"\necho \"stderr $v\" >&2\n";
/// Fails the node, with the value in what it printed.
const BOOM: &str = "#!/bin/sh\nv=$(sed -n 's/^  message: //p' \"$APB_RUN_DIR/run.yaml\")\necho \"failed on $v\"\necho \"failed on $v\" >&2\nexit 3\n";
/// The child prints its instruction (the parent's value).
const SAY: &str = "#!/bin/sh\nv=$(sed -n 's/^instruction: //p' \"$APB_RUN_DIR/run.yaml\")\necho \"child said $v\"\n";

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
