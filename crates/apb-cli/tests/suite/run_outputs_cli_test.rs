//! Run outputs through the real binary: playbooks with a declared
//! `outputs:` block run by `apb run` (script nodes only, no agent), read
//! back with `apb runs <id> --outputs-json`, `apb runs <id> --json` (the
//! MCP `run_status` object) and the text of `apb runs <id>`; and journals
//! written by hand for the states a real run does not reach cheaply.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

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

    /// Installs playbook `id` as its current version, with its scripts.
    fn playbook(&self, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
        let base = self.root().join(".apb/playbooks").join(id);
        let v = base.join("1.0.0");
        fs::create_dir_all(v.join("scripts")).unwrap();
        fs::write(v.join("playbook.yaml"), yaml).unwrap();
        for (name, body) in scripts {
            fs::write(v.join("scripts").join(name), body).unwrap();
        }
        fs::write(base.join("current"), "1.0.0").unwrap();
    }

    fn apb(&self) -> assert_cmd::Command {
        let mut c = crate::common::apb();
        c.current_dir(self.root())
            .env("APB_CONFIG_DIR", self.cfg.path());
        c
    }

    /// `apb run <id>` in the foreground; returns the run id.
    fn run(&self, id: &str) -> String {
        let out = self.apb().args(["run", id]).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("run "))
            .and_then(|l| l.split_whitespace().next())
            .map(|s| s.trim_end_matches(':').to_string())
            .unwrap_or_else(|| {
                panic!(
                    "no run id: {stdout}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                )
            })
    }

    /// `apb runs <id> --outputs-json`, parsed.
    fn outputs(&self, run: &str) -> Value {
        let out = self
            .apb()
            .args(["runs", run, "--outputs-json"])
            .assert()
            .success();
        serde_json::from_slice(&out.get_output().stdout).unwrap()
    }

    fn text(&self, run: &str) -> String {
        let out = self.apb().args(["runs", run]).assert().success();
        String::from_utf8_lossy(&out.get_output().stdout).into_owned()
    }

    fn status_json(&self, run: &str) -> Value {
        let out = self.apb().args(["runs", run, "--json"]).assert().success();
        serde_json::from_slice(&out.get_output().stdout).unwrap()
    }

    fn run_dir(&self, run: &str) -> std::path::PathBuf {
        self.root().join(".apb/runs").join(run)
    }
}

/// The output named `name` of a projection.
fn named<'a>(outputs: &'a Value, name: &str) -> &'a Value {
    outputs["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == name)
        .unwrap_or_else(|| panic!("no output `{name}` in {outputs:#}"))
}

/// A loop of three executions of `w`, the second of which fails.
const LOOP: &str = r#"schema: 2
id: loop
name: loop
version: 1.0.0
outputs:
  last_verdict: { from: { node: w, field: verdict } }
  first_verdict: { from: { node: w, field: verdict }, pick: first }
  all_verdicts: { from: { node: w, field: verdict }, pick: all }
  text: { from: { node: w } }
  absent: { from: { node: w, field: nope } }
  maybe: { from: { node: w, field: maybe } }
  answer: { from: { node: done } }
  other: { from: { node: other } }
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: "scripts/w.sh", runner: sh, outputs: { fields: [verdict, n, maybe] } }
  - { id: done, type: finish, outcome: success }
  - { id: other, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: w, condition: { type: node_status, node: w, equals: failure }, max_traversals: 3 }
  - { from: w, to: w, condition: { type: output_field, node: w, field: n, equals: "1" }, max_traversals: 3 }
  - { from: w, to: done, condition: { type: output_field, node: w, field: n, equals: "3" } }
  - { from: w, to: other, condition: { type: output_field, node: w, field: n, equals: "99" } }
"#;

/// Counts its runs in `count`; the second run fails.
const LOOP_SCRIPT: &str = r#"n=$(cat count 2>/dev/null || echo 0)
n=$((n+1))
echo $n > count
if [ $n -eq 2 ]; then echo boom; exit 1; fi
printf '{"verdict":"v%s","n":%s,"maybe":null}\n' $n $n
"#;

#[test]
fn a_loop_projects_last_first_and_all_and_skips_the_failed_execution() {
    let p = Project::new();
    p.playbook("loop", LOOP, &[("w.sh", LOOP_SCRIPT)]);
    let run = p.run("loop");
    let o = p.outputs(&run);
    assert_eq!(o["final"], true, "{o:#}");
    assert_eq!(o["definition"], "stored", "{o:#}");

    let last = named(&o, "last_verdict");
    assert_eq!(last["state"], "value");
    assert_eq!(last["value"], "v3");
    assert_eq!(last["source"]["execution"]["node"], "w");
    assert_eq!(last["source"]["execution"]["ordinal"], 3);
    assert_eq!(last["source"]["part"]["kind"], "fields");
    assert_eq!(
        last["source"]["settlement_seq"],
        last["source"]["execution"]["ended_seq"]
    );

    let first = named(&o, "first_verdict");
    assert_eq!(first["value"], "v1");
    assert_eq!(first["source"]["execution"]["ordinal"], 1);

    // The failed second execution gives no candidate.
    let all = named(&o, "all_verdicts");
    assert_eq!(all["state"], "value");
    assert_eq!(all["value"], json!(["v1", "v3"]));
    let ordinals: Vec<&Value> = all["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| &v["source"]["execution"]["ordinal"])
        .collect();
    assert_eq!(ordinals, [&json!(1), &json!(3)]);

    assert_eq!(
        named(&o, "text")["value"],
        "{\"verdict\":\"v3\",\"n\":3,\"maybe\":null}"
    );
    let absent = named(&o, "absent");
    assert_eq!(absent["state"], "missing");
    assert_eq!(absent["reason"], "field_absent");
    let maybe = named(&o, "maybe");
    assert_eq!(maybe["state"], "null");
    assert!(maybe.get("value").is_none(), "{maybe:#}");
    // The finish node counts: its (empty) answer is a value.
    let answer = named(&o, "answer");
    assert_eq!(answer["state"], "value");
    assert_eq!(answer["source"]["execution"]["node"], "done");
    let other = named(&o, "other");
    assert_eq!(other["state"], "missing");
    assert_eq!(other["reason"], "not_run");

    // The same projection on `run_status` and in the text of `apb runs`.
    let status = p.status_json(&run);
    assert_eq!(
        status["run_outputs"], o,
        "run_status carries the projection"
    );
    let text = p.text(&run);
    assert!(text.contains("  outputs (final):"), "{text}");
    assert!(text.contains("    last_verdict: v3 (from w #3)"), "{text}");
    assert!(text.contains("    absent: missing field_absent"), "{text}");
    assert!(text.contains("    maybe: null (from w #3)"), "{text}");
    assert!(
        text.contains("    all_verdicts: [\"v1\",\"v3\"] (2 candidates)"),
        "{text}"
    );
}

/// Two finish branches, chosen by the file `way` in the project.
const BRANCHES: &str = r#"schema: 2
id: branches
name: branches
version: 1.0.0
outputs:
  url: { from: [ { node: pub_a, field: url }, { node: pub_b, field: url } ] }
  finished_a: { from: { node: done_a } }
  finished_b: { from: { node: done_b } }
nodes:
  - { id: start, type: start }
  - { id: pick, type: script, script: "scripts/pick.sh", runner: sh, outputs: { fields: [way] } }
  - { id: pub_a, type: script, script: "scripts/a.sh", runner: sh, outputs: { fields: [url] } }
  - { id: pub_b, type: script, script: "scripts/b.sh", runner: sh, outputs: { fields: [url] } }
  - { id: done_a, type: finish, outcome: success }
  - { id: done_b, type: finish, outcome: success }
edges:
  - { from: start, to: pick }
  - { from: pick, to: pub_a, condition: { type: output_field, node: pick, field: way, equals: a } }
  - { from: pick, to: pub_b, condition: { type: output_field, node: pick, field: way, equals: b } }
  - { from: pub_a, to: done_a }
  - { from: pub_b, to: done_b }
"#;

#[test]
fn alternative_finish_branches_project_the_branch_that_ran() {
    let p = Project::new();
    p.playbook(
        "branches",
        BRANCHES,
        &[
            ("pick.sh", "printf '{\"way\":\"%s\"}\\n' \"$(cat way)\"\n"),
            ("a.sh", "echo '{\"url\":\"https://example.test/a\"}'\n"),
            ("b.sh", "echo '{\"url\":\"https://example.test/b\"}'\n"),
        ],
    );
    for (way, url, ran, not) in [
        ("a", "https://example.test/a", "finished_a", "finished_b"),
        ("b", "https://example.test/b", "finished_b", "finished_a"),
    ] {
        fs::write(p.root().join("way"), way).unwrap();
        let run = p.run("branches");
        let o = p.outputs(&run);
        let u = named(&o, "url");
        assert_eq!(u["value"], url, "way {way}: {o:#}");
        assert_eq!(u["source"]["execution"]["node"], format!("pub_{way}"));
        assert_eq!(named(&o, ran)["state"], "value", "way {way}: {o:#}");
        assert_eq!(named(&o, not)["reason"], "not_run", "way {way}: {o:#}");
    }
}

const CHILD: &str = r#"schema: 2
id: child
name: child
version: 1.0.0
outputs:
  summary: { from: { node: work, field: summary } }
  verdict: { from: { node: work, field: verdict } }
  tags: { from: { node: work, field: tag }, pick: all }
  gone: { from: { node: work, field: gone } }
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh, outputs: { fields: [summary, verdict, tag] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A parent that reads the child's outputs in a node's working directory
/// template and in an edge condition, next to a default (answer) child.
const PARENT: &str = r#"schema: 2
id: parent
name: parent
version: 1.0.0
outputs:
  child_summary: { from: { node: sub, field: summary } }
  where: { from: { node: read, field: dir } }
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: child, output: outputs }
  - { id: plain, type: playbook, playbook: child }
  - { id: read, type: script, script: "scripts/read.sh", runner: sh, workdir: "out/{{nodes.sub.output.summary}}", outputs: { fields: [dir] } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: sub }
  - { from: sub, to: plain }
  - { from: plain, to: read }
  - { from: read, to: ok, condition: { type: output_field, node: sub, field: verdict, equals: go } }
  - { from: read, to: no, condition: { type: output_field, node: sub, field: verdict, equals: stop } }
"#;

#[test]
fn a_parent_reads_child_outputs_in_its_template_and_edge_condition() {
    let p = Project::new();
    p.playbook(
        "child",
        CHILD,
        &[(
            "work.sh",
            "echo '{\"summary\":\"sum1\",\"verdict\":\"go\",\"tag\":\"t\"}'\n",
        )],
    );
    p.playbook(
        "parent",
        PARENT,
        &[(
            "read.sh",
            "printf '{\"dir\":\"%s\"}\\n' \"$(basename \"$PWD\")\"\n",
        )],
    );
    fs::create_dir_all(p.root().join("out/sum1")).unwrap();
    let run = p.run("parent");
    let journal = fs::read_to_string(p.run_dir(&run).join("events.jsonl")).unwrap();
    let finished = |node: &str| -> Value {
        journal
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|e| e["type"] == "node_finished" && e["node"] == node)
            .unwrap_or_else(|| panic!("no node_finished of {node}: {journal}"))
    };
    // The child's projection: values, `all` as an array, missing left out.
    let sub: Value = serde_json::from_str(finished("sub")["output"].as_str().unwrap()).unwrap();
    assert_eq!(
        sub,
        json!({ "summary": "sum1", "verdict": "go", "tags": ["t"] }),
        "{journal}"
    );
    // The default still hands on the answer text (a promptless finish: "").
    assert_eq!(finished("plain")["output"], "");
    // The working directory template read the child's output, and the edge
    // condition routed on it.
    assert_eq!(finished("read")["status"], "succeeded", "{journal}");
    assert_eq!(finished("ok")["status"], "succeeded", "{journal}");
    let o = p.outputs(&run);
    assert_eq!(named(&o, "child_summary")["value"], "sum1", "{o:#}");
    assert_eq!(named(&o, "where")["value"], "sum1", "{o:#}");
}

/// A child whose run output carries its sensitive param (`password` reads
/// as a secret by its name), and a parent that takes the child's outputs.
const SECRET_CHILD: &str = r#"schema: 2
id: schild
name: schild
version: 1.0.0
params:
  - { name: password, type: text, default: orchard-candle }
outputs:
  pw: { from: { node: echo } }
nodes:
  - { id: start, type: start }
  - { id: echo, type: prompt, prompt: "{{params.password}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: echo }
  - { from: echo, to: done }
"#;

const SECRET_PARENT: &str = r#"schema: 2
id: sparent
name: sparent
version: 1.0.0
outputs:
  child: { from: { node: sub } }
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: schild, output: outputs }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sub }
  - { from: sub, to: done }
"#;

/// A run's own declared output that carries its sensitive param: the plain
/// text of `apb runs <id>` masks it like the JSON forms do.
#[test]
fn a_secret_in_a_declared_output_is_masked_in_the_text_of_apb_runs() {
    let p = Project::new();
    p.playbook("schild", SECRET_CHILD, &[]);
    let run = p.run("schild");
    let text = p.text(&run);
    assert!(text.contains("outputs ("), "{text}");
    assert!(!text.contains("orchard-candle"), "{text}");
    assert!(!p.outputs(&run).to_string().contains("orchard-candle"));
}

/// A sensitive param (secret by its name) with a quote in it, and an object output that holds
/// it: serialized, the value reads `pa\"ss-word-1`, a spelling the
/// redactor does not know.
const QUOTED_SECRET: &str = r#"schema: 2
id: quoted
name: quoted
version: 1.0.0
params:
  - { name: password, type: text, default: 'pa"ss-word-1' }
outputs:
  creds: { from: { node: emit, field: data } }
nodes:
  - { id: start, type: start }
  - { id: emit, type: script, script: "scripts/emit.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: emit }
  - { from: emit, to: done }
"#;

/// An object output whose secret holds a quote: the text of `apb runs`
/// masks it like the JSON forms do.
#[test]
fn a_secret_with_a_quote_in_an_object_output_is_masked_in_the_text_of_apb_runs() {
    let p = Project::new();
    p.playbook(
        "quoted",
        QUOTED_SECRET,
        &[(
            "emit.sh",
            "printf '%s\\n' '{\"data\":{\"token\":\"pa\\\"ss-word-1\"}}'\n",
        )],
    );
    let run = p.run("quoted");
    let o = p.outputs(&run);
    assert!(named(&o, "creds")["value"].is_object(), "{o:#}");
    assert!(!o.to_string().contains("ss-word-1"), "{o:#}");
    let text = p.text(&run);
    assert!(text.contains("creds: {"), "{text}");
    assert!(!text.contains("ss-word-1"), "{text}");
}

#[test]
fn a_childs_secret_in_its_outputs_stays_hidden_on_the_parents_surfaces() {
    let p = Project::new();
    p.playbook("schild", SECRET_CHILD, &[]);
    p.playbook("sparent", SECRET_PARENT, &[]);
    let run = p.run("sparent");
    // The parent's journal keeps what the child produced: routing reads it.
    let journal = fs::read_to_string(p.run_dir(&run).join("events.jsonl")).unwrap();
    assert!(journal.contains("orchard-candle"), "{journal}");
    let o = p.outputs(&run);
    assert!(named(&o, "child")["value"].is_string(), "{o:#}");
    for (surface, shown) in [
        ("--outputs-json", o.to_string()),
        ("--json", p.status_json(&run).to_string()),
        ("text", p.text(&run)),
    ] {
        assert!(
            !shown.contains("orchard-candle"),
            "{surface} shows the child's secret: {shown}"
        );
    }
}

/// The surfaces of one run, as text, keyed by surface.
fn surfaces(p: &Project, run: &str) -> Vec<(&'static str, String)> {
    vec![
        ("--outputs-json", p.outputs(run).to_string()),
        ("--json", p.status_json(run).to_string()),
        ("text", p.text(run)),
    ]
}

/// The run id of the child the run `run` started for `node`.
fn child_of(p: &Project, run: &str, node: &str) -> String {
    let journal = fs::read_to_string(p.run_dir(run).join("events.jsonl")).unwrap();
    journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "child_run_started" && e["node_id"] == node)
        .and_then(|e| e["run_id"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("no child for {node}: {journal}"))
}

/// A parent journal whose last append was torn inside a multibyte
/// character: the surfaces still read the complete events before it, so
/// the child's secret in them stays hidden.
#[test]
fn a_childs_secret_stays_hidden_when_the_parents_journal_tail_is_torn() {
    let p = Project::new();
    p.playbook("schild", SECRET_CHILD, &[]);
    p.playbook("sparent", SECRET_PARENT, &[]);
    let run = p.run("sparent");
    let path = p.run_dir(&run).join("events.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    // The first byte of a two-byte character, and no newline after it.
    bytes.extend_from_slice(b"{\"seq\":999,\"ts\":1,\"type\":\"note\",\"text\":\"caf\xC3");
    fs::write(&path, bytes).unwrap();
    // The surfaces read the complete events: the child's output is there.
    let o = p.outputs(&run);
    assert!(named(&o, "child")["value"].is_string(), "{o:#}");
    for (surface, shown) in surfaces(&p, &run) {
        assert!(
            !shown.contains("orchard-candle"),
            "{surface} shows the child's secret: {shown}"
        );
    }
}

/// A child whose secret-discovery input cannot be read (its `run.yaml`
/// does not parse): the parent's surfaces withhold the run's text rather
/// than read the child as holding no secret.
#[test]
fn a_child_whose_params_cannot_be_read_withholds_the_parents_text() {
    let p = Project::new();
    p.playbook("schild", SECRET_CHILD, &[]);
    p.playbook("sparent", SECRET_PARENT, &[]);
    let run = p.run("sparent");
    let child = child_of(&p, &run, "sub");
    fs::write(p.run_dir(&child).join("run.yaml"), "params: [unclosed\n").unwrap();
    for (surface, shown) in surfaces(&p, &run) {
        assert!(
            !shown.contains("orchard-candle"),
            "{surface} shows the child's secret: {shown}"
        );
    }
    let status = p.status_json(&run);
    assert_eq!(status["secrets_unresolved"], true, "{status:#}");
    assert_eq!(status["run_id"], run.as_str(), "{status:#}");
}

/// A parent that hands child `a`'s output to child `b` as its instruction:
/// `b` has no sensitive param of its own, yet the value it got from its
/// sibling stays hidden on `b`'s surfaces.
const SIBLING_B: &str = r#"schema: 2
id: sib-b
name: sib-b
version: 1.0.0
outputs:
  said: { from: { node: echo } }
nodes:
  - { id: start, type: start }
  - { id: echo, type: prompt, prompt: "{{run.instruction}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: echo }
  - { from: echo, to: done }
"#;

const SIBLING_PARENT: &str = r#"schema: 2
id: sib-parent
name: sib-parent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: playbook, playbook: schild, output: outputs }
  - { id: b, type: playbook, playbook: sib-b, instruction: "{{nodes.a.output.pw}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: b, to: done }
"#;

#[test]
fn a_secret_one_child_hands_a_sibling_stays_hidden_on_the_siblings_surfaces() {
    let p = Project::new();
    p.playbook("schild", SECRET_CHILD, &[]);
    p.playbook("sib-b", SIBLING_B, &[]);
    p.playbook("sib-parent", SIBLING_PARENT, &[]);
    let run = p.run("sib-parent");
    let b = child_of(&p, &run, "b");
    // The sibling's journal holds the value it was handed.
    let journal = fs::read_to_string(p.run_dir(&b).join("events.jsonl")).unwrap();
    assert!(journal.contains("orchard-candle"), "{journal}");
    let o = p.outputs(&b);
    assert!(named(&o, "said")["value"].is_string(), "{o:#}");
    for (surface, shown) in surfaces(&p, &b) {
        assert!(
            !shown.contains("orchard-candle"),
            "{surface} of the sibling shows the secret: {shown}"
        );
    }
}

/// A real list node whose items print a value, a null, no field, more than
/// the list keeps, and a failure: the run output names each item's value
/// or why it is missing.
const LIST_OUTPUTS: &str = r#"schema: 2
id: lst
name: lst
version: 1.0.0
outputs:
  status: { from: { node: items, item_field: status } }
nodes:
  - { id: start, type: start }
  - { id: items, type: script, script: "scripts/i.sh", runner: sh, list: { items: { inline: [{ id: ok }, { id: nul }, { id: none }, { id: big }, { id: bad }] } } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: items }
  - { from: items, to: done }
"#;

const LIST_ITEM_SCRIPT: &str = r#"case "$APB_ITEM_ID" in
  ok) echo '{"status":"green"}' ;;
  nul) echo '{"status":null}' ;;
  none) echo '{"other":1}' ;;
  big) printf '{"status":"%s"}\n' "$(head -c 3000 /dev/zero | tr '\0' x)" ;;
  *) echo 'broken' 1>&2; exit 1 ;;
esac
"#;

#[test]
fn a_real_list_projects_each_items_value_null_or_missing_reason() {
    let p = Project::new();
    p.playbook("lst", LIST_OUTPUTS, &[("i.sh", LIST_ITEM_SCRIPT)]);
    let run = p.run("lst");
    let o = p.outputs(&run);
    let status = named(&o, "status");
    assert_eq!(status["state"], "value", "{o:#}");
    assert_eq!(
        status["value"],
        json!({ "ok": "green", "nul": null }),
        "{o:#}"
    );
    let items: Vec<(String, String, Value, Value)> = status["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_string(),
                i["state"].as_str().unwrap().to_string(),
                i.get("value").cloned().unwrap_or(Value::Null),
                i.get("reason").cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    let row = |id: &str, state: &str, value: Value, reason: Value| {
        (id.to_string(), state.to_string(), value, reason)
    };
    assert_eq!(
        items,
        [
            row("ok", "value", json!("green"), Value::Null),
            row("nul", "null", Value::Null, Value::Null),
            row("none", "missing", Value::Null, json!("field_absent")),
            row("big", "missing", Value::Null, json!("truncated")),
            row("bad", "missing", Value::Null, json!("item_not_passed")),
        ],
        "{o:#}"
    );
}

/// A child that projects an output, then fails: the parent's `output:
/// outputs` node fails with the child's ending, never with the outputs of
/// a run that did not succeed, and the parent's own output that reads the
/// node is missing as `not_succeeded`.
const FAILING_CHILD: &str = r#"schema: 2
id: fchild
name: fchild
version: 1.0.0
outputs:
  verdict: { from: { node: first, field: verdict } }
nodes:
  - { id: start, type: start }
  - { id: first, type: script, script: "scripts/first.sh", runner: sh, outputs: { fields: [verdict] } }
  - { id: boom, type: script, script: "scripts/boom.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: first }
  - { from: first, to: boom }
  - { from: boom, to: done }
"#;

const FAILING_PARENT: &str = r#"schema: 2
id: fparent
name: fparent
version: 1.0.0
outputs:
  child: { from: { node: sub } }
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: fchild, output: outputs }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sub }
  - { from: sub, to: done }
"#;

#[test]
fn a_failed_child_fails_the_parents_node_without_its_outputs() {
    let p = Project::new();
    p.playbook(
        "fchild",
        FAILING_CHILD,
        &[
            ("first.sh", "echo '{\"verdict\":\"go\"}'\n"),
            ("boom.sh", "echo broken 1>&2\nexit 1\n"),
        ],
    );
    p.playbook("fparent", FAILING_PARENT, &[]);
    let run = p.run("fparent");
    let child = child_of(&p, &run, "sub");
    // The child projected its output before it failed.
    let co = p.outputs(&child);
    assert_eq!(named(&co, "verdict")["value"], "go", "{co:#}");
    let journal = fs::read_to_string(p.run_dir(&run).join("events.jsonl")).unwrap();
    let sub = journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "node_finished" && e["node"] == "sub")
        .unwrap_or_else(|| panic!("{journal}"));
    assert_eq!(sub["status"], "failed", "{sub}");
    assert_eq!(
        sub["output"],
        format!("sub-playbook child run `{child}` ended failed"),
        "{sub}"
    );
    let o = p.outputs(&run);
    assert_eq!(named(&o, "child")["state"], "missing", "{o:#}");
    assert_eq!(named(&o, "child")["reason"], "not_succeeded", "{o:#}");
}

#[test]
fn a_playbook_without_outputs_reads_as_before() {
    let p = Project::new();
    p.playbook(
        "plain",
        "schema: 2\nid: plain\nname: plain\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        &[],
    );
    let run = p.run("plain");
    assert_eq!(
        p.outputs(&run),
        json!({ "final": true, "definition": "stored", "outputs": [] })
    );
    assert!(p.status_json(&run).get("run_outputs").is_none());
    assert!(!p.text(&run).contains("outputs ("));
}

// --- journals written by hand ---------------------------------------------

/// A run directory with `playbook.yaml` and a journal of `lines` (each a
/// JSON object without `seq` and `ts`, which are filled in).
fn hand_run(p: &Project, run: &str, playbook: &str, lines: &[Value]) {
    let dir = p.run_dir(run);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook).unwrap();
    // A prepared run always has its config: one without it withholds.
    fs::write(dir.join("run.yaml"), "params: {}\n").unwrap();
    write_journal(&dir, lines);
}

fn write_journal(dir: &Path, lines: &[Value]) {
    let mut text = String::new();
    for (seq, l) in lines.iter().enumerate() {
        let mut l = l.clone();
        l["seq"] = json!(seq);
        l["ts"] = json!(1_790_000_000_000u64 + seq as u64);
        text.push_str(&l.to_string());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn node_finished(node: &str, status: &str, output: &str) -> Value {
    json!({ "type": "node_finished", "node": node, "status": status, "attempt": 1, "output": output, "artifacts": [] })
}

const HAND: &str = r#"schema: 2
id: hand
name: hand
version: 1.0.0
outputs:
  failed_only: { from: { node: f, field: x } }
  not_object: { from: { node: t, field: x } }
  checked: { from: { node: items, item_field: status } }
nodes:
  - { id: start, type: start }
  - { id: f, type: script, script: f.sh, runner: sh }
  - { id: t, type: script, script: t.sh, runner: sh }
  - { id: items, type: script, script: i.sh, runner: sh, list: { items: { inline: [a, b, c] } } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: f }
  - { from: f, to: t }
  - { from: t, to: items }
  - { from: items, to: done }
"#;

#[test]
fn hand_journals_give_every_missing_reason_and_list_items() {
    let p = Project::new();
    let aggregate = json!({
        "total": 3, "processed": 3, "passed": 2, "passed_with_note": 0, "needs_look": 1, "filtered_out": 0,
        "items": [
            { "id": "a", "state": "passed", "note": null, "item": "a", "output": { "status": "ok" } },
            { "id": "b", "state": "passed", "note": null, "item": "b", "output": { "truncated": true } },
            { "id": "c", "state": "needs_look", "note": "x", "item": "c", "output": { "status": "bad" } },
        ]
    })
    .to_string();
    hand_run(
        &p,
        "hand-1",
        HAND,
        &[
            json!({ "type": "run_started", "playbook": "hand", "version": "1.0.0" }),
            json!({ "type": "node_started", "node": "f", "attempt": 1 }),
            node_finished("f", "failed", "boom"),
            json!({ "type": "node_started", "node": "t", "attempt": 1 }),
            node_finished("t", "succeeded", "plain text"),
            json!({ "type": "node_started", "node": "items", "attempt": 1 }),
            json!({ "type": "list_started", "node": "items", "total": 3 }),
            json!({ "type": "list_item_settled", "node": "items", "item": "a", "state": "passed" }),
            json!({ "type": "list_item_settled", "node": "items", "item": "b", "state": "passed" }),
            json!({ "type": "list_item_settled", "node": "items", "item": "c", "state": "needs_look" }),
            node_finished("items", "succeeded", &aggregate),
        ],
    );
    let o = p.outputs("hand-1");
    // No run_finished: the run is not final, and nothing is stored, so the
    // current playbook.yaml is the declaration read.
    assert_eq!(o["final"], false, "{o:#}");
    assert_eq!(o["definition"], "current", "{o:#}");
    assert_eq!(named(&o, "failed_only")["reason"], "not_succeeded", "{o:#}");
    assert_eq!(
        named(&o, "not_object")["reason"],
        "not_json_object",
        "{o:#}"
    );
    let checked = named(&o, "checked");
    assert_eq!(checked["state"], "value", "{o:#}");
    assert_eq!(checked["value"], json!({ "a": "ok" }), "{o:#}");
    let items = checked["items"].as_array().unwrap();
    let reasons: Vec<(&Value, &Value)> = items.iter().map(|i| (&i["id"], &i["state"])).collect();
    assert_eq!(
        reasons,
        [
            (&json!("a"), &json!("value")),
            (&json!("b"), &json!("missing")),
            (&json!("c"), &json!("missing"))
        ]
    );
    assert_eq!(items[1]["reason"], "truncated");
    assert_eq!(items[2]["reason"], "item_not_passed");
    // Each item cites its own settlement in the visit.
    assert_eq!(items[0]["source"]["settlement_seq"], 7, "{o:#}");
    assert_eq!(
        items[0]["source"]["item"]["visit"]["started_seq"], 6,
        "{o:#}"
    );
    assert_eq!(items[0]["source"]["item"]["id"], "a");
}

/// A stored start definition, then a resume whose definition is not stored:
/// an unchanged `playbook.yaml` projects the same values, a changed
/// declaration reads `definition_unknown`; a revival after the finish
/// flips `final` back.
#[test]
fn an_unknown_last_definition_projects_with_the_current_file_unless_it_changed() {
    let p = Project::new();
    let pb = "schema: 2\nid: d\nname: d\nversion: 1.0.0\noutputs:\n  out: { from: { node: w } }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: w.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";
    fs::create_dir_all(p.run_dir("d-1")).unwrap();
    let digest = apb_engine::run_definitions::store(&p.run_dir("d-1"), pb.as_bytes()).unwrap();
    let base = vec![
        json!({ "type": "run_started", "playbook": "d", "version": "1.0.0", "definition": digest }),
        json!({ "type": "node_started", "node": "w", "attempt": 1 }),
        node_finished("w", "succeeded", "hello"),
        node_finished("done", "succeeded", ""),
        json!({ "type": "run_finished", "outcome": "succeeded" }),
    ];
    hand_run(&p, "d-1", pb, &base);
    let stored = p.outputs("d-1");
    assert_eq!(stored["definition"], "stored");
    assert_eq!(stored["final"], true);
    assert_eq!(named(&stored, "out")["value"], "hello");

    // A resume an older apb wrote (no digest): the same file, the same values.
    let mut resumed = base.clone();
    resumed.push(json!({ "type": "run_resumed", "from_node": "w" }));
    write_journal(&p.run_dir("d-1"), &resumed);
    let current = p.outputs("d-1");
    assert_eq!(current["definition"], "current", "{current:#}");
    assert_eq!(current["final"], false, "a revived run is not final");
    assert_eq!(current["outputs"], stored["outputs"]);

    // The file now declares outputs differently from the start definition.
    fs::write(
        p.run_dir("d-1").join("playbook.yaml"),
        pb.replace("{ node: w }", "{ node: done }"),
    )
    .unwrap();
    let changed = p.outputs("d-1");
    assert_eq!(named(&changed, "out")["state"], "missing", "{changed:#}");
    assert_eq!(named(&changed, "out")["reason"], "definition_unknown");
}

// --- the validator, through `apb validate` -------------------------------

/// A playbook with `outputs` (YAML lines under `outputs:`) over a script
/// `w` declaring `fields: [a]`, a list node `l` keeping `item_outputs: [s]`,
/// and a node `island` nothing reaches.
fn with_outputs(outputs: &str) -> String {
    format!(
        "schema: 2\nid: v\nname: v\nversion: 1.0.0\noutputs:\n{outputs}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: script, script: scripts/w.sh, runner: sh, outputs: {{ fields: [a] }} }}\n  - {{ id: l, type: script, script: scripts/w.sh, runner: sh, list: {{ items: {{ inline: [{{ id: x }}] }}, item_outputs: [s] }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: l }}\n  - {{ from: l, to: done }}\n"
    )
}

fn validate(p: &Project, id: &str) -> String {
    let out = p.apb().args(["validate", id]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_validator_checks_every_run_outputs_rule() {
    let cases: [(&str, &str, &str); 8] = [
        (
            "  Bad-Name: { from: { node: w } }\n",
            "error V131",
            "must match",
        ),
        ("  x: { from: [] }\n", "error V132", "has no source"),
        (
            "  x: { from: { node: ghost } }\n",
            "error V132",
            "does not exist",
        ),
        (
            "  x: { from: { node: l, field: total, item_field: s } }\n",
            "error V132",
            "not both",
        ),
        (
            "  x: { from: { node: w, field: b } }\n",
            "warning V133",
            "publishes only [a]",
        ),
        (
            "  x: { from: { node: w, item_field: s } }\n",
            "error V134",
            "not a list node",
        ),
        (
            "  x: { from: { node: l, item_field: t } }\n",
            "warning V135",
            "item_outputs [s]",
        ),
        (
            "  a1: { from: { node: w, field: a } }\n  b2: { from: { node: l, item_field: s }, pick: all }\n",
            "OK",
            "",
        ),
    ];
    for (decl, code, words) in cases {
        let p = Project::new();
        p.playbook("v", &with_outputs(decl), &[("w.sh", "echo '{}'\n")]);
        let out = validate(&p, "v");
        let hit = out
            .lines()
            .any(|l| l.starts_with(&format!("v: {code}")) && l.contains(words));
        assert!(hit, "{decl} -> expected `{code}` with `{words}`:\n{out}");
        if code == "OK" {
            assert!(!out.contains("V13"), "{decl}:\n{out}");
        }
    }
    let p = Project::new();
    let many: String = (0..65)
        .map(|i| format!("  o{i}: {{ from: {{ node: w }} }}\n"))
        .collect();
    p.playbook("v", &with_outputs(&many), &[("w.sh", "echo '{}'\n")]);
    assert!(validate(&p, "v").contains("v: error V131 outputs declares 65 outputs"));
}

#[test]
fn a_read_of_a_child_output_the_child_does_not_declare_is_reported() {
    let p = Project::new();
    p.playbook("child", CHILD, &[("work.sh", "echo '{}'\n")]);
    let parent = |field: &str| {
        format!(
            "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: sub, type: playbook, playbook: child, output: outputs }}\n  - {{ id: ok, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: sub }}\n  - {{ from: sub, to: ok, condition: {{ type: output_field, node: sub, field: {field}, equals: go }} }}\n  - {{ from: sub, to: no, fallback: true }}\n"
        )
    };
    p.playbook("parent", &parent("verdikt"), &[]);
    let out = validate(&p, "parent");
    assert!(
        out.lines().any(|l| l.contains("warning V46")
            && l.contains("reads field `verdikt` of `sub`")
            && l.contains("summary, verdict, tags, gone")),
        "{out}"
    );
    p.playbook("parent", &parent("verdict"), &[]);
    assert!(!validate(&p, "parent").contains("V46"));
}

/// A child named with `scope: global` is checked against the global
/// child's outputs, the one a run executes, even when the project holds a
/// playbook of the same id with other outputs.
#[test]
fn a_child_named_in_the_global_scope_is_checked_against_the_global_child() {
    let p = Project::new();
    let child = |output: &str| {
        format!(
            "schema: 2\nid: twin\nname: twin\nversion: 1.0.0\noutputs:\n  {output}: {{ from: {{ node: work, field: {output} }} }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh, outputs: {{ fields: [{output}] }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
        )
    };
    p.playbook("twin", &child("other"), &[("work.sh", "echo '{}'\n")]);
    let global = p.cfg.path().join("playbooks/twin");
    fs::create_dir_all(global.join("1.0.0/scripts")).unwrap();
    fs::write(global.join("1.0.0/playbook.yaml"), child("result")).unwrap();
    fs::write(global.join("1.0.0/scripts/work.sh"), "echo '{}'\n").unwrap();
    fs::write(global.join("current"), "1.0.0").unwrap();
    let parent = |field: &str| {
        format!(
            "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: sub, type: playbook, playbook: {{ id: twin, scope: global }}, output: outputs }}\n  - {{ id: ok, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: sub }}\n  - {{ from: sub, to: ok, condition: {{ type: output_field, node: sub, field: {field}, equals: go }} }}\n  - {{ from: sub, to: no, fallback: true }}\n"
        )
    };
    p.playbook("parent", &parent("result"), &[]);
    let out = validate(&p, "parent");
    assert!(!out.contains("V46"), "{out}");
    p.playbook("parent", &parent("other"), &[]);
    let out = validate(&p, "parent");
    assert!(
        out.lines()
            .any(|l| l.contains("warning V46") && l.contains("reads field `other` of `sub`")),
        "{out}"
    );
}

/// A misspelt key in a run output or its source is a load error, never a
/// silent default (a source without `field` would project the whole text).
#[test]
fn a_misspelt_key_in_a_run_output_is_refused() {
    let p = Project::new();
    for outputs in [
        "  url: { from: { node: w, feild: url } }\n",
        "  url: { from: { node: w, field: url }, pikc: first }\n",
    ] {
        p.playbook("v", &with_outputs(outputs), &[("w.sh", "echo '{}'\n")]);
        let out = p.apb().args(["validate", "v"]).output().unwrap();
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "{outputs}: {shown}");
        assert!(shown.contains("unknown field"), "{outputs}: {shown}");
    }
}
