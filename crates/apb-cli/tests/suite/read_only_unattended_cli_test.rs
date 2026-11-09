//! The read-only node and unattended mode through the real binary.
//!
//! The validator rules of a read-only node (V137 to V140) and of a
//! `human_review` gate's `unattended` outcome (V141 to V143), through
//! `apb validate`: the code, its severity, the words a person reads, and
//! the exit code (1 on an error, 0 on warnings only). Then `apb run`: a
//! read-only script that changes its tree fails at once with evidence that
//! names paths and never holds content, and an unattended run decides a
//! gate that declares it, fails `hitl_required` at one that does not (also
//! through a child), and stays unattended across `apb resume` with its
//! `run.yaml` rewritten.

use std::fs;
use std::path::Path;

struct Project {
    dir: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(dir.path()).unwrap();
        Project { dir, cfg }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Installs playbook `v` as its current version, with the script every
    /// fixture names.
    fn playbook(&self, yaml: &str) {
        let base = self.root().join(".apb/playbooks/v");
        let v = base.join("1.0.0");
        fs::create_dir_all(v.join("scripts")).unwrap();
        fs::write(v.join("playbook.yaml"), yaml).unwrap();
        fs::write(v.join("scripts/w.sh"), "echo '{}'\n").unwrap();
        fs::write(base.join("current"), "1.0.0").unwrap();
    }

    /// `apb validate v`: its exit code and stdout.
    fn validate(&self) -> (i32, String) {
        let out = crate::common::apb()
            .current_dir(self.root())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .args(["validate", "v"])
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

/// What one fixture must report: `Some((severity and code, words))` for
/// the one issue of the rules under test, `None` for none of them.
type Expect = Option<(&'static str, &'static str)>;

/// Validates `yaml` and checks that exactly the expected issue among
/// `codes` is reported (on one line, with `words`), and the exit code that
/// follows from its severity.
fn check(case: &str, yaml: &str, codes: &[&str], expect: Expect) {
    let p = Project::new();
    p.playbook(yaml);
    let (code, out) = p.validate();
    let hits: Vec<&str> = out
        .lines()
        .filter(|l| codes.iter().any(|c| l.contains(&format!(" {c} "))))
        .collect();
    match expect {
        None => {
            assert!(hits.is_empty(), "{case}: no issue expected:\n{out}");
            assert_eq!(code, 0, "{case}: exit 0 expected:\n{out}");
        }
        Some((what, words)) => {
            assert_eq!(hits.len(), 1, "{case}: one `{what}` expected:\n{out}");
            let line = hits[0];
            assert!(
                line.starts_with(&format!("v: {what} ")) && line.contains(words),
                "{case}: expected `{what}` with `{words}`:\n{out}"
            );
            let want = if what.starts_with("error") { 1 } else { 0 };
            assert_eq!(code, want, "{case}: exit {want} expected:\n{out}");
        }
    }
}

/// A linear playbook `start -> x -> done` with node `x` given by `node`.
fn linear(node: &str) -> String {
    format!(
        "schema: 2\nid: v\nname: v\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {node}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: x }}\n  - {{ from: x, to: done }}\n"
    )
}

const READ_ONLY_CODES: [&str; 4] = ["V137", "V138", "V139", "V140"];

#[test]
fn read_only_is_checked_on_every_kind_with_list_and_protect() {
    let cases: [(&str, String, Expect); 9] = [
        (
            "script",
            linear(
                "{ id: x, type: script, script: \"scripts/w.sh\", runner: sh, read_only: true, expected_duration: 5s }",
            ),
            None,
        ),
        (
            "agent_task",
            linear("{ id: x, type: agent_task, prompt: hi, profile: p, read_only: true, expected_duration: 5s }"),
            None,
        ),
        (
            "human_review",
            linear("{ id: x, type: human_review, read_only: true }"),
            Some((
                "error V137",
                "`read_only` applies to agent_task and script nodes only; node `x` is a human_review node",
            )),
        ),
        (
            "playbook",
            linear("{ id: x, type: playbook, playbook: other, read_only: true }"),
            Some(("error V137", "is a playbook node")),
        ),
        (
            "judge",
            linear(
                "{ id: x, type: judge, questions: { ok: { type: noul, instructions: i } }, on_unavailable: { route: done }, read_only: true }",
            ),
            Some(("error V137", "is a judge node")),
        ),
        (
            "list script",
            linear(
                "{ id: x, type: script, script: \"scripts/w.sh\", runner: sh, read_only: true, expected_duration: 5s, list: { items: { inline: [a, b] } } }",
            ),
            Some((
                "error V138",
                "`read_only` is not allowed on a list node",
            )),
        ),
        (
            "list agent_task",
            linear(
                "{ id: x, type: agent_task, prompt: hi, profile: p, read_only: true, expected_duration: 5s, list: { items: { inline: [a, b] } } }",
            ),
            Some(("error V138", "list node")),
        ),
        (
            "protect",
            linear(
                "{ id: x, type: agent_task, prompt: hi, profile: p, read_only: true, protect: [\"*.md\"], expected_duration: 5s }",
            ),
            Some((
                "warning V139",
                "`protect` is redundant on a read-only node",
            )),
        ),
        (
            "publish from a read-only node",
            "schema: 2\nid: v\nname: v\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: x, type: script, script: \"scripts/w.sh\", runner: sh, read_only: true, expected_duration: 5s }\n  - { id: pub, type: publish, from: x, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: x }\n  - { from: x, to: pub }\n  - { from: pub, to: done }\n".to_string(),
            Some((
                "warning V140",
                "`from` names `x`, a read-only node",
            )),
        ),
    ];
    for (case, yaml, expect) in cases {
        check(case, &yaml, &READ_ONLY_CODES, expect);
    }
}

const UNATTENDED_CODES: [&str; 3] = ["V141", "V142", "V143"];

/// `start -> work -> gate`; `approve` goes on to `next` and then `done`,
/// `needs_changes` goes back to `work` (or, with `fallback`, by the
/// fallback edge). `gate` carries `unattended`, `next` is given whole, and
/// `top` goes above `nodes:`.
fn gated(top: &str, unattended: &str, next: &str, extra_nodes: &str, extra_edges: &str) -> String {
    format!(
        "schema: 2\nid: v\nname: v\nversion: 1.0.0\n{top}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }}\n  - {{ id: gate, type: human_review, options: [approve, needs_changes], unattended: {unattended} }}\n  - {next}\n{extra_nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: gate }}\n  - {{ from: gate, to: next, condition: {{ type: review_status, equals: approve }} }}\n  - {{ from: gate, to: work, condition: {{ type: review_status, equals: needs_changes }}, max_traversals: 3 }}\n{extra_edges}"
    )
}

/// A plain script step `next` that goes to `done`.
const NEXT: &str =
    "{ id: next, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }";
const NEXT_DONE: &str = "  - { from: next, to: done }\n";
/// A step `next` that declares `irreversible`.
const NEXT_IRREVERSIBLE: &str = "{ id: next, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, effects: [irreversible] }";
/// The `needs_changes` route made safe: an unbounded fallback out of the
/// gate back to `work`.
const FALLBACK: &str = "  - { from: gate, to: work, fallback: true }\n";
/// A failure handler `rescue` that declares `irreversible`, and its edge on.
const RESCUE_IRREVERSIBLE: &str = "  - { id: rescue, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, effects: [irreversible] }\n";
const RESCUE_DONE: &str = "  - { from: rescue, to: done }\n";
/// Two branches `b1` and `b2` out of `next`, joined at `done`.
const BRANCHES: &str = "  - { id: b1, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n  - { id: b2, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n";
const BRANCH_EDGES: &str = "  - { from: next, to: b1 }\n  - { from: next, to: b2 }\n  - { from: b1, to: done }\n  - { from: b2, to: done }\n";

#[test]
fn unattended_is_checked_on_its_kind_its_option_its_reach_and_its_exit() {
    let decide_approve = "{ decide: approve }";
    let decide_fix = "{ decide: needs_changes, note: \"retry without a person\" }";
    let cases: [(&str, String, Expect); 19] = [
        (
            "fail on a gate",
            gated("", "fail", NEXT_IRREVERSIBLE, "", NEXT_DONE),
            None,
        ),
        (
            "decide with a safe reach",
            gated("", decide_approve, NEXT, "", NEXT_DONE),
            None,
        ),
        (
            "unattended on a script",
            linear(
                "{ id: x, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, unattended: fail }",
            ),
            Some((
                "error V141",
                "`unattended` applies to human_review nodes only; node `x` is a script node",
            )),
        ),
        (
            "unattended on a publish step",
            "schema: 2\nid: v\nname: v\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n  - { id: x, type: publish, from: w, remote: origin, branch: \"apb/out\", unattended: { decide: approve } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: x }\n  - { from: x, to: done }\n".to_string(),
            Some((
                "error V141",
                "a publish step pushes only with a person's consent",
            )),
        ),
        (
            "undeclared option",
            gated("", "{ decide: approved }", NEXT, "", NEXT_DONE),
            Some((
                "error V141",
                "`unattended.decide` names `approved`, which is not one of the gate's options (approve, needs_changes)",
            )),
        ),
        (
            "irreversible right after the option",
            gated("", decide_approve, NEXT_IRREVERSIBLE, "", NEXT_DONE),
            Some((
                "error V142",
                "`unattended: { decide: approve }` is refused: node `next` after the gate declares or implies irreversible effects",
            )),
        ),
        (
            "secrets two steps after the option",
            gated(
                "",
                decide_approve,
                NEXT,
                "  - { id: later, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, effects: [secrets] }\n",
                "  - { from: next, to: later }\n  - { from: later, to: done }\n",
            ),
            Some(("error V142", "node `later` after the gate declares or implies secrets effects")),
        ),
        (
            "irreversible after a second gate",
            gated(
                "",
                decide_approve,
                NEXT,
                "  - { id: gate2, type: human_review }\n  - { id: ship, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, effects: [irreversible] }\n",
                "  - { from: next, to: gate2 }\n  - { from: gate2, to: ship }\n  - { from: ship, to: done }\n",
            ),
            None,
        ),
        (
            "irreversible only on another option's route",
            gated("", decide_fix, NEXT_IRREVERSIBLE, "", &format!("{NEXT_DONE}{FALLBACK}")),
            None,
        ),
        (
            "a fallback a spent bounded edge leads to",
            gated(
                "",
                decide_fix,
                NEXT,
                "  - { id: ship, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, effects: [irreversible] }\n",
                "  - { from: next, to: done }\n  - { from: gate, to: ship, fallback: true }\n  - { from: ship, to: done }\n",
            ),
            Some(("error V142", "node `ship` after the gate declares or implies irreversible effects")),
        ),
        (
            "playbook-level secrets",
            gated("effects: [secrets]\n", decide_fix, NEXT, "", &format!("{NEXT_DONE}{FALLBACK}")),
            Some(("error V142", "the playbook declares secrets effects")),
        ),
        (
            "a bounded edge only",
            gated("", decide_fix, NEXT, "", NEXT_DONE),
            Some((
                "error V143",
                "`unattended: { decide: needs_changes }` is refused: only bounded edges route `needs_changes` out of the gate",
            )),
        ),
        (
            "no edge at all",
            "schema: 2\nid: v\nname: v\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: x, type: human_review, options: [approve, needs_changes], unattended: { decide: needs_changes } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: x }\n  - { from: x, to: done, condition: { type: review_status, equals: approve } }\n".to_string(),
            Some(("error V143", "no edge routes `needs_changes` out of the gate")),
        ),
        (
            "the fallback is the way out",
            gated("", decide_fix, NEXT, "", &format!("{NEXT_DONE}{FALLBACK}")),
            None,
        ),
        (
            "an irreversible defaults.on_failure handler",
            gated(
                "defaults: { on_failure: rescue }\n",
                decide_approve,
                NEXT,
                RESCUE_IRREVERSIBLE,
                &format!("{NEXT_DONE}{RESCUE_DONE}"),
            ),
            Some(("error V142", "node `rescue` after the gate declares or implies irreversible effects")),
        ),
        (
            "a harmless defaults.on_failure handler",
            gated(
                "defaults: { on_failure: rescue }\n",
                decide_approve,
                NEXT,
                "  - { id: rescue, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n",
                &format!("{NEXT_DONE}{RESCUE_DONE}"),
            ),
            None,
        ),
        (
            "an irreversible fork.on_failure of a fork in reach",
            gated(
                "",
                decide_approve,
                "{ id: next, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, fork: { on_branch_failure: fail_fast, on_failure: rescue } }",
                &format!("{BRANCHES}{RESCUE_IRREVERSIBLE}"),
                &format!("{BRANCH_EDGES}{RESCUE_DONE}"),
            ),
            Some(("error V142", "node `rescue` after the gate declares or implies irreversible effects")),
        ),
        (
            "an irreversible fork.on_failure of a fork before the gate",
            gated(
                "",
                decide_approve,
                NEXT,
                &format!("  - {{ id: side, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }}\n{RESCUE_IRREVERSIBLE}"),
                &format!("{NEXT_DONE}  - {{ from: start, to: side }}\n  - {{ from: side, to: done }}\n{RESCUE_DONE}"),
            )
            .replace(
                "{ id: start, type: start }",
                "{ id: start, type: start, fork: { on_branch_failure: fail_fast, on_failure: rescue } }",
            ),
            Some(("error V142", "node `rescue` after the gate declares or implies irreversible effects")),
        ),
        (
            "an irreversible fork.on_failure of a fork joined before the gate",
            "schema: 2\nid: v\nname: v\nversion: 1.0.0\nnodes:\n  - { id: start, type: start, fork: { on_branch_failure: fail_fast, on_failure: rescue } }\n  - { id: left, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n  - { id: right, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n  - { id: gate, type: human_review, options: [approve, needs_changes], unattended: { decide: approve } }\n  - { id: next, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s }\n  - { id: rescue, type: script, script: \"scripts/w.sh\", runner: sh, expected_duration: 5s, effects: [irreversible] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: left }\n  - { from: start, to: right }\n  - { from: left, to: gate }\n  - { from: right, to: gate }\n  - { from: gate, to: next, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: done, fallback: true }\n  - { from: next, to: done }\n  - { from: rescue, to: done }\n".to_string(),
            None,
        ),
    ];
    for (case, yaml, expect) in cases {
        check(case, &yaml, &UNATTENDED_CODES, expect);
    }
}

// ------------------------------------------------------------- apb run

impl Project {
    /// Installs playbook `id` as its current version with `scripts`.
    fn install(&self, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
        let base = self.root().join(".apb/playbooks").join(id);
        let v = base.join("1.0.0");
        fs::create_dir_all(v.join("scripts")).unwrap();
        fs::write(v.join("playbook.yaml"), yaml).unwrap();
        for (name, body) in scripts {
            fs::write(v.join("scripts").join(name), body).unwrap();
        }
        fs::write(base.join("current"), "1.0.0").unwrap();
    }

    /// `apb <args>` in the project: exit code and stdout plus stderr.
    fn apb(&self, args: &[&str]) -> (i32, String) {
        self.apb_env(args, &[])
    }

    /// [`Project::apb`] with `env` set, as a run step's process has it.
    fn apb_env(&self, args: &[&str], env: &[(&str, &Path)]) -> (i32, String) {
        let mut cmd = crate::common::apb();
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd
            .current_dir(self.root())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .args(args)
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// The ids of the project's runs, oldest first.
    fn runs(&self) -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(self.root().join(".apb/runs"))
            .map(|d| {
                d.filter_map(Result::ok)
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// The journal of run `id`, one JSON value per line.
    fn journal(&self, id: &str) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root().join(".apb/runs").join(id).join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The run of playbook `id` (its run ids start with it).
    fn run_of(&self, id: &str) -> String {
        let ids: Vec<String> = self
            .runs()
            .into_iter()
            .filter(|r| r.starts_with(&format!("{id}-")))
            .collect();
        assert_eq!(ids.len(), 1, "{ids:?}");
        ids[0].clone()
    }
}

fn of<'a>(journal: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
    journal.iter().filter(|e| e["type"] == kind).collect()
}

/// `apb run` of a read-only script that writes a secret-shaped file: the
/// node fails `read_only_dirty` in its one run, `apb runs <id> --json`
/// lists the change by name, and the secret written appears in no file
/// under the run.
#[test]
fn a_read_only_script_that_writes_fails_with_names_and_no_content() {
    let p = Project::new();
    fs::write(p.root().join("notes.md"), "notes\n").unwrap();
    let marker = "ro-cli-marker-91c2";
    p.install(
        "ro",
        "schema: 2\nid: ro\nname: ro\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: \"scripts/w.sh\", runner: sh, read_only: true }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
        // The script builds the marker, so its own copy under the run does
        // not hold it.
        &[(
            "w.sh",
            "m=ro-cli-; m=\"${m}marker-91c2\"\necho \"TOKEN=$m\" > .env\necho \"$m\" >> notes.md\n",
        )],
    );
    let (code, out) = p.apb(&["run", "ro"]);
    assert_eq!(code, 1, "{out}");
    let run = p.run_of("ro");
    let journal = p.journal(&run);
    let finished: Vec<_> = of(&journal, "node_finished")
        .into_iter()
        .filter(|e| e["node"] == "work")
        .collect();
    assert_eq!(finished.len(), 1, "the node finished once");
    assert_eq!(finished[0]["status"], "failed");
    assert!(
        finished[0]["output"]
            .as_str()
            .unwrap()
            .starts_with("read_only_dirty: "),
        "{}",
        finished[0]
    );
    let (code, json) = p.apb(&["runs", &run, "--json"]);
    assert_eq!(code, 0, "{json}");
    let status: serde_json::Value = serde_json::from_str(&json).unwrap();
    let rows = status["read_only"].as_array().expect("read_only rows");
    assert_eq!(rows.len(), 1, "{status:#}");
    assert_eq!(rows[0]["tier"], "detected");
    let changes = &rows[0]["violated"]["changes"];
    assert_eq!(
        changes,
        &serde_json::json!([
            { "path": ".env", "change": "added" },
            { "path": "notes.md", "change": "modified" }
        ]),
        "{status:#}"
    );
    let evidence = rows[0]["violated"]["evidence"].as_str().unwrap();
    let run_dir = p.root().join(".apb/runs").join(&run);
    assert!(run_dir.join(evidence).is_file(), "{evidence}");
    let mut stack = vec![run_dir];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(marker),
                    "{} holds the content",
                    path.display()
                );
            }
        }
    }
}

/// A read-only agent step in a git repository that commits and edits a
/// tracked file, with `max_retries: 3`: one attempt only, the node fails
/// `read_only_dirty`, and the journal and the evidence name the file, the
/// new commit and the moved ref, with the tracked file's diff stat.
#[test]
fn a_read_only_agent_that_commits_fails_once_with_the_repository_named() {
    let p = Project::new();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(p.root())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    fs::write(p.root().join(".gitignore"), ".apb/\n").unwrap();
    fs::write(p.root().join("notes.md"), "notes\n").unwrap();
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    let profile = p.root().join(".apb/profiles/main");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("profile.yaml"),
        "name: main\ndescription: d\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(profile.join("SOUL.md"), "role").unwrap();
    p.install(
        "ro",
        "schema: 2\nid: ro\nname: ro\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: read, read_only: true, max_retries: 3 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
        &[],
    );
    let bin = tempfile::tempdir().unwrap();
    let calls = bin.path().join("calls");
    let stub = bin.path().join("agent.sh");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf x >> '{}'\necho more >> notes.md\ngit -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -qam sneaky\nprintf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\necho done\n",
            calls.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let (code, out) = p.apb_env(&["run", "ro"], &[("APB_AGENT_CMD", &stub)]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "x",
        "the agent ran once"
    );
    let run = p.run_of("ro");
    let journal = p.journal(&run);
    let attempts = of(&journal, "attempt_started")
        .into_iter()
        .filter(|e| e["node"] == "work")
        .count();
    assert_eq!(attempts, 1, "max_retries: 3 never runs it again");
    let finished: Vec<_> = of(&journal, "node_finished")
        .into_iter()
        .filter(|e| e["node"] == "work")
        .collect();
    assert_eq!(finished.len(), 1);
    assert!(
        finished[0]["output"]
            .as_str()
            .unwrap()
            .starts_with("read_only_dirty: "),
        "{}",
        finished[0]
    );
    let violated = of(&journal, "read_only_violated");
    assert_eq!(violated.len(), 1);
    assert_eq!(
        violated[0]["changes"],
        serde_json::json!([
            { "path": "notes.md", "change": "modified" },
            { "path": "HEAD", "change": "git_head" },
            { "path": "refs", "change": "git_refs" },
            { "path": "index", "change": "git_index" }
        ]),
        "{}",
        violated[0]
    );
    let evidence = violated[0]["evidence"].as_str().unwrap();
    let body: serde_json::Value = serde_json::from_slice(
        &fs::read(p.root().join(".apb/runs").join(&run).join(evidence)).unwrap(),
    )
    .unwrap();
    let stat = body["diff_stat"].as_array().unwrap();
    assert!(
        stat.iter()
            .any(|l| l.as_str().unwrap().contains("notes.md")),
        "{body:#}"
    );
}

/// Gate `gate1` declares `unattended: { decide: approved }`, gate `gate`
/// declares nothing; a failure handler `rescue` must never run.
const TWO_GATES: &str = "schema: 2\nid: ua\nname: ua\nversion: 1.0.0\ndefaults: { on_failure: rescue }\nnodes:\n  - { id: start, type: start }\n  - { id: gate1, type: human_review, options: [approved, rejected], unattended: { decide: approved, note: ci } }\n  - { id: work, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: gate, type: human_review, options: [approved, rejected] }\n  - { id: rescue, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: ok, type: finish, outcome: success }\n  - { id: no, type: finish, outcome: failure }\nedges:\n  - { from: start, to: gate1 }\n  - { from: gate1, to: work, condition: { type: review_status, equals: approved } }\n  - { from: gate1, to: no, condition: { type: review_status, equals: rejected } }\n  - { from: work, to: gate }\n  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }\n  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }\n  - { from: rescue, to: no }\n";

/// The run error of a journal: its node and reason.
fn run_error(journal: &[serde_json::Value]) -> (String, String) {
    let e = of(journal, "run_error").pop().expect("a run_error");
    (
        e["node"].as_str().unwrap_or_default().to_string(),
        e["reason"].as_str().unwrap_or_default().to_string(),
    )
}

/// `apb run --unattended`: the gate that declares a decision passes as
/// `decided_by: unattended`, the plain gate ends the run `failed` with exit
/// 1 and `hitl_required`, nobody is asked there and the failure handler
/// never runs.
#[test]
fn an_unattended_run_decides_the_declared_gate_and_fails_at_a_plain_one() {
    let p = Project::new();
    p.install("ua", TWO_GATES, &[("w.sh", "echo w\n")]);
    let (code, out) = p.apb(&["run", "ua", "--unattended"]);
    assert_eq!(code, 1, "{out}");
    let run = p.run_of("ua");
    let journal = p.journal(&run);
    let decided = of(&journal, "review_decided");
    assert_eq!(decided.len(), 1, "{journal:?}");
    assert_eq!(decided[0]["node"], "gate1");
    assert_eq!(decided[0]["decided_by"], "unattended");
    assert_eq!(decided[0]["note"], "ci");
    let (node, reason) = run_error(&journal);
    assert_eq!(node, "gate");
    assert_eq!(
        reason,
        "hitl_required: human_review `gate` has no unattended outcome"
    );
    assert!(
        !of(&journal, "review_requested")
            .iter()
            .any(|e| e["node"] == "gate"),
        "nobody is asked at the plain gate"
    );
    assert!(
        !of(&journal, "node_started")
            .iter()
            .any(|e| e["node"] == "rescue"),
        "no failure route"
    );
    let (code, json) = p.apb(&["runs", &run, "--json"]);
    assert_eq!(code, 0, "{json}");
    let status: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(status["unattended"], true, "{status:#}");
}

/// A child of an unattended run is unattended and stops at its plain gate;
/// the parent fails with the child's code and `via child <run>`, and its
/// own failure handler never runs.
#[test]
fn an_unattended_parent_stops_with_its_childs_hitl_required() {
    let p = Project::new();
    p.install(
        "child",
        "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
        &[],
    );
    p.install(
        "parent",
        "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\ndefaults: { on_failure: rescue }\nnodes:\n  - { id: start, type: start }\n  - { id: c, type: playbook, playbook: child }\n  - { id: rescue, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\n  - { id: no, type: finish, outcome: failure }\nedges:\n  - { from: start, to: c }\n  - { from: c, to: done }\n  - { from: rescue, to: no }\n",
        &[("w.sh", "echo w\n")],
    );
    let (code, out) = p.apb(&["run", "parent", "--unattended"]);
    assert_eq!(code, 1, "{out}");
    let parent = p.run_of("parent");
    let child = p.run_of("child");
    let (node, reason) = run_error(&p.journal(&parent));
    assert_eq!(node, "c");
    assert!(
        reason.starts_with(&format!("hitl_required: via child {child}: ")),
        "{reason}"
    );
    assert!(
        !of(&p.journal(&parent), "node_started")
            .iter()
            .any(|e| e["node"] == "rescue"),
        "no failure route in the parent"
    );
    let (_, child_reason) = run_error(&p.journal(&child));
    assert!(
        child_reason.starts_with("hitl_required: "),
        "{child_reason}"
    );
}

/// An unattended run whose driver was lost before its plain gate is
/// resumed by `apb resume` after its `run.yaml` lost the mode (an older
/// apb rewrites it without the field): the write-once manifest keeps it
/// unattended, so the gate fails the run.
#[test]
fn a_resume_keeps_the_run_unattended_when_run_yaml_lost_the_mode() {
    // `apb resume` drives the run in this process; `apb resume --all`
    // resumes a run whose driver died in a detached driver of its own, the
    // way the pause and release wakers do.
    for all in [false, true] {
        let p = Project::new();
        p.install(
            "ua",
            &TWO_GATES.replace(
                "{ id: work, type: script, script: \"scripts/w.sh\", runner: sh }",
                "{ id: work, type: script, script: \"scripts/hold.sh\", runner: sh }",
            ),
            &[
                ("w.sh", "echo w\n"),
                (
                    "hold.sh",
                    ": > held\nwhile [ -f hold ]; do sleep 0.05; done\necho held\n",
                ),
            ],
        );
        fs::write(p.root().join("hold"), "").unwrap();
        let mut driver = crate::common::apb_std()
            .current_dir(p.root())
            .env("APB_CONFIG_DIR", p.cfg.path())
            .args(["run", "ua", "--unattended"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !p.root().join("held").exists() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        driver.kill().unwrap();
        driver.wait().unwrap();
        let run = p.run_of("ua");
        let run_dir = p.root().join(".apb/runs").join(&run);
        let cfg = fs::read_to_string(run_dir.join("run.yaml")).unwrap();
        assert!(cfg.contains("unattended: true"), "{cfg}");
        let lost = cfg.replace("unattended: true\n", "");
        assert!(!lost.contains("unattended"), "{lost}");
        fs::write(run_dir.join("run.yaml"), lost).unwrap();
        fs::remove_file(p.root().join("hold")).unwrap();
        if all {
            let (code, out) = p.apb(&["resume", "--all"]);
            assert_eq!(code, 0, "{out}");
            // The detached driver ends the run on its own.
            let start = std::time::Instant::now();
            while !of(&p.journal(&run), "run_finished").iter().any(|_| true) {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(60),
                    "resume --all: the run did not end: {out}"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        } else {
            let (code, out) = p.apb(&["resume", &run]);
            assert_eq!(code, 1, "{out}");
        }
        let (node, reason) = run_error(&p.journal(&run));
        assert_eq!(node, "gate", "all: {all}");
        assert!(
            reason.starts_with("hitl_required: "),
            "all: {all}: {reason}"
        );
    }
}

/// Who may choose unattended mode: a run step of an attended run may not
/// (it would take the person that run waits on out of the gates of the run
/// it starts), and is refused before any run file; a step of an unattended
/// run may.
#[test]
fn only_a_step_of_an_unattended_run_may_start_one_unattended() {
    let p = Project::new();
    p.install(
        "plain",
        "schema: 2\nid: plain\nname: plain\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        &[],
    );
    let (code, out) = p.apb(&["run", "plain"]);
    assert_eq!(code, 0, "{out}");
    let attended = p.runs()[0].clone();
    let step_of = |id: &str| {
        let dir = p.root().join(".apb/runs").join(id);
        (std::path::PathBuf::from(id), dir)
    };

    let (id, dir) = step_of(&attended);
    let (code, out) = p.apb_env(
        &["run", "plain", "--unattended"],
        &[("APB_RUN_ID", &id), ("APB_RUN_DIR", &dir)],
    );
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("run refused (unattended_unsupported)")
            && out.contains(&format!(
                "an unattended start is refused inside run `{attended}`"
            )),
        "{out}"
    );
    assert_eq!(p.runs().len(), 1, "a refused start writes no run");

    let (code, out) = p.apb(&["run", "plain", "--unattended"]);
    assert_eq!(code, 0, "{out}");
    let unattended = p.runs().into_iter().find(|r| r != &attended).unwrap();
    let (id, dir) = step_of(&unattended);
    let (code, out) = p.apb_env(
        &["run", "plain", "--unattended"],
        &[("APB_RUN_ID", &id), ("APB_RUN_DIR", &dir)],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(p.runs().len(), 3);
}

// ---------------------------------------------------------- external data

/// The run directories of `p`, sorted.
fn run_ids(p: &Project) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(p.root().join(".apb/runs"))
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// `apb run --external-data`: the run's manifest carries the flag, `apb
/// runs` and `apb runs <id> --json` show it, `--remember` with it is
/// refused before a run directory exists with the memory unchanged, and
/// `apb resume --external-data false` is refused with the journal
/// unchanged.
#[test]
fn apb_run_external_data_is_flagged_shown_and_never_remembers() {
    let p = Project::new();
    p.install(
        "ext",
        "schema: 2\nid: ext\nname: ext\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
        &[("w.sh", "echo w\n")],
    );
    let (code, out) = p.apb(&["run", "ext", "--external-data", "--remember"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("external_data_remember_refused"), "{out}");
    assert!(run_ids(&p).is_empty());
    assert!(!p.root().join(".apb/memory").exists());

    let (code, out) = p.apb(&["run", "ext", "--external-data"]);
    assert_eq!(code, 0, "{out}");
    let ids = run_ids(&p);
    assert_eq!(ids.len(), 1);
    let run = &ids[0];
    let dir = p.root().join(".apb/runs").join(run);
    assert!(
        apb_engine::manifest::read(&dir)
            .unwrap()
            .unwrap()
            .external_data
    );
    let (code, json) = p.apb(&["runs", run, "--json"]);
    assert_eq!(code, 0, "{json}");
    let status: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(status["external_data"], true, "{status:#}");
    let (_, text) = p.apb(&["runs", run]);
    assert!(text.contains("external data"), "{text}");
    assert!(!text.contains("+external"), "{text}");
    let (_, table) = p.apb(&["runs"]);
    assert!(table.contains("external data"), "{table}");

    let journal = fs::read(dir.join("events.jsonl")).unwrap();
    let (code, out) = p.apb(&["resume", run, "--external-data", "false"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("external_data_immutable"), "{out}");
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), journal);
}
