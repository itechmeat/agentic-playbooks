//! Scenario files (`evals/scenarios/<name>.yaml`): the loader reads only
//! named regular files below the playbook, the digest follows content and
//! not form, staleness, and each of V146 to V151 fires on its case and stays
//! quiet on a scenario that answers every node kind correctly.

use apb_core::scenario::{self, Scenario};
use apb_core::schema::Playbook;
use apb_core::validate::{Issue, Severity};

const PLAYBOOK: &str = r#"
schema: 2
id: demo
name: demo
version: 1.2.0
params:
  - { name: kind, type: text }
  - { name: token, type: text, secret: true }
goal:
  statement: the work is done
  criteria:
    - { description: tests pass, check: { type: script, path: check.sh } }
    - { description: marker, check: { type: marker, marker: DONE } }
baseline_check:
  checks:
    - id: tests
      project_checks: { only: [test] }
outputs:
  report: { from: { node: work } }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: hi, profile: x, max_retries: 1 }
  - { id: lint, type: script, script: lint.sh, runner: sh }
  - { id: gate, type: human_review, options: [approve, needs_changes] }
  - id: check
    type: judge
    state: { out: "{{nodes.work.output}}" }
    questions:
      verdict: { type: choice, instructions: i, criteria: { clear: null, unclear: null } }
      size: { type: score, instructions: i, levels: [small, big] }
      risky: { type: noul, instructions: i }
    on_unavailable: fail
  - id: files
    type: agent_task
    prompt: per item
    profile: x
    list:
      items: { inline: [ { id: a.md }, { id: b.md } ] }
  - { id: child, type: playbook, playbook: other, output: outputs }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 60 }, timeout_seconds: 120 }
  - { id: ship, type: publish, from: work, remote: origin }
  - { id: done, type: finish, outcome: success, prompt: sum up, profile: x }
  - { id: stop, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { id: quick-exit, from: start, to: stop, condition: { type: judge, question: "nothing to do?", min_p: 0.9, on_unavailable: false } }
  - { id: needs-fix, from: work, to: lint, condition: { type: judge, question: "broken?", min_p: 0.5, on_unavailable: false } }
  - { from: work, to: gate }
  - { from: lint, to: gate }
  - { from: gate, to: check }
  - { from: check, to: files }
  - { from: files, to: child }
  - { from: child, to: pause }
  - { from: pause, to: ship }
  - { from: ship, to: done }
  - { from: done, to: stop, fallback: true }
"#;

const GREEN: &str = r#"schema: 1
id: green
title: every node kind answered
versions: ">=1.0.0, <2.0.0"
instruction: do it
params: { kind: feature }
answers:
  start:
    - { visit: 1, judge_edges: { "id:quick-exit": { p: 0.1 } } }
  work:
    - visit: 1
      attempts:
        - { status: failed, failure_kind: infrastructure, output: "spawn failed" }
        - { status: failed, output: "wrong" }
        - { status: succeeded, output: "ok", check: pass }
      judge_edges: { "id:needs-fix": { p: 0.8 } }
    - visit: default
      attempts:
        - { question: { text: "Which DB?", answer: postgres }, then: { status: succeeded } }
  lint:
    - { visit: 1, script: { exit: 0, stdout: "green" } }
  gate:
    - { visit: 1, review: { decision: needs_changes, note: "again" } }
  check:
    - { visit: 1, judge: { verdict: { value: clear, p: 0.9, confidence: 0.8 }, size: { value: big }, risky: { p: 0.1 } } }
    - { visit: 2, judge: { unavailable: timeout } }
  files:
    - visit: 1
      items:
        "a.md": [ { status: succeeded, output: "ok" } ]
        "*": { status: succeeded, output: "ok" }
  child:
    - { visit: 1, playbook: { status: succeeded, output: { pr: "x" } } }
    - { visit: 2, playbook: { scenario: child-green } }
  pause:
    - { visit: 1, wait: { elapsed: true } }
  ship:
    - { visit: 1, publish: { decision: publish, status: succeeded } }
  done:
    - { visit: 1, attempts: [ { status: succeeded, output: "DONE" } ] }
baseline: { before: { tests: { failed: 0 } }, after: { tests: { failed: 0 } } }
goal: { "0": pass }
expect:
  run: { outcome: [succeeded] }
  finish: done
  route: { path: [start, work, gate], visits: [lint], in_order: true, not_visits: [stop] }
  visits: { work: 1 }
  edges: { taken: { "e:start>work#1": 1 }, not_taken: ["id:needs-fix"] }
  joins: { dead: { gate: [lint] } }
  outputs: [ { node: work, matches: "o." } ]
  run_outputs: { report: { state: value } }
  templates_empty: report
"#;

fn playbook() -> Playbook {
    Playbook::from_yaml(PLAYBOOK).unwrap()
}

/// A playbook directory holding the given scenario files.
fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let scenarios = scenario::scenarios_dir(dir.path());
    std::fs::create_dir_all(&scenarios).unwrap();
    for (name, body) in files {
        std::fs::write(scenarios.join(name), body).unwrap();
    }
    dir
}

fn issues_of(body: &str) -> Vec<Issue> {
    let dir = dir_with(&[("green.yaml", body)]);
    scenario::validate_all(dir.path(), &playbook())
}

fn show(issues: &[Issue]) -> Vec<String> {
    issues
        .iter()
        .map(|i| format!("{} {:?} {}", i.code, i.severity, i.message))
        .collect()
}

#[test]
fn a_scenario_answering_every_node_kind_validates_clean() {
    let got = issues_of(GREEN);
    assert!(got.is_empty(), "{:#?}", show(&got));
    let dir = dir_with(&[("green.yaml", GREEN)]);
    let loaded = scenario::load(dir.path(), "green").unwrap();
    assert!(loaded.digest.starts_with("sha256:"));
}

/// Each rule on its own case: the edit to the green scenario, the code it
/// must raise with its severity, and a word the message must carry.
#[test]
fn each_rule_fires_on_its_case() {
    let cases: &[(&str, &str, &str, Severity, &str)] = &[
        // V146: the file on its own.
        (
            "schema: 1\n",
            "schema: 2\n",
            "V146",
            Severity::Error,
            "schema 2",
        ),
        (
            "id: green\n",
            "id: red\n",
            "V146",
            Severity::Error,
            "does not match the file name",
        ),
        (
            ">=1.0.0, <2.0.0",
            "around one",
            "V146",
            Severity::Error,
            "versions",
        ),
        (
            "title: every",
            "titel: every",
            "V146",
            Severity::Error,
            "unknown field",
        ),
        (
            "{ visit: 1, script: { exit: 0, stdout: \"green\" } }",
            "{ visit: 1, script: { exit: 0 }, review: { decision: approve } }",
            "V146",
            Severity::Error,
            "a visit takes one kind",
        ),
        (
            "{ visit: 1, wait: { elapsed: true } }",
            "{ visit: 1 }",
            "V146",
            Severity::Error,
            "carries no answer",
        ),
        (
            "wait: { elapsed: true }",
            "wait: { elapsed: true, timed_out: true }",
            "V146",
            Severity::Error,
            "exactly one of `elapsed`",
        ),
        (
            "wait: { elapsed: true }",
            "wait: {}",
            "V146",
            Severity::Error,
            "exactly one of `elapsed`",
        ),
        (
            "{ question: { text: \"Which DB?\", answer: postgres }, then",
            "{ question: { text: \"Which DB?\", answer: postgres, timeout: true }, then",
            "V146",
            Severity::Error,
            "either answered",
        ),
        (
            "{ status: succeeded, output: \"ok\", check: pass }",
            "{ status: succeeded, failure_kind: agent, output: \"ok\" }",
            "V146",
            Severity::Error,
            "no `failure_kind`",
        ),
        (
            "goal: { \"0\": pass }",
            "goal: { first: pass }",
            "V146",
            Severity::Error,
            "not a criterion index",
        ),
        // V147: answer keys.
        (
            "  lint:\n",
            "  lnit:\n",
            "V147",
            Severity::Error,
            "node `lnit`",
        ),
        (
            "{ visit: 1, script: { exit: 0, stdout: \"green\" } }",
            "{ visit: 1, attempts: [ { status: succeeded } ] }",
            "V147",
            Severity::Error,
            "a script node",
        ),
        (
            "{ visit: 1, publish: { decision: publish, status: succeeded } }",
            "{ visit: 1, items: { \"*\": { status: succeeded } } }",
            "V147",
            Severity::Error,
            "has no `list:`",
        ),
        (
            "{ visit: 2, judge: { unavailable: timeout } }",
            "{ visit: 0, judge: { unavailable: timeout } }",
            "V147",
            Severity::Error,
            "visits count from 1",
        ),
        (
            "{ visit: 2, judge: { unavailable: timeout } }",
            "{ visit: 1, judge: { unavailable: timeout } }",
            "V147",
            Severity::Error,
            "answered twice",
        ),
        (
            "  lint:\n    - { visit: 1,",
            "  lint:\n    - { visit: default, script: { exit: 1 } }\n    - { visit: default,",
            "V147",
            Severity::Error,
            "at most one",
        ),
        (
            "goal: { \"0\": pass }",
            "goal: { \"1\": pass }",
            "V147",
            Severity::Error,
            "not a script criterion",
        ),
        (
            "after: { tests:",
            "after: { tsets:",
            "V147",
            Severity::Error,
            "check `tsets`",
        ),
        // V148: answer values.
        (
            "decision: needs_changes",
            "decision: reject",
            "V148",
            Severity::Error,
            "not an option of `gate`",
        ),
        (
            "decision: publish,",
            "decision: approve,",
            "V148",
            Severity::Error,
            "publish decision",
        ),
        (
            "verdict: { value: clear",
            "verdict: { value: vague",
            "V148",
            Severity::Error,
            "not one of its options",
        ),
        (
            "size: { value: big }",
            "size: { value: huge }",
            "V148",
            Severity::Error,
            "not one of its levels",
        ),
        (
            "risky: { p: 0.1 }",
            "riksy: { p: 0.1 }",
            "V148",
            Severity::Error,
            "no question `riksy`",
        ),
        (
            "risky: { p: 0.1 }",
            "risky: { p: 1.5 }",
            "V148",
            Severity::Error,
            "outside 0 to 1",
        ),
        (
            "\"id:needs-fix\": { p: 0.8 }",
            "\"id:nope\": { p: 0.8 }",
            "V148",
            Severity::Error,
            "names no edge",
        ),
        (
            "\"id:needs-fix\": { p: 0.8 }",
            "\"e:start>work#1\": { p: 0.8 }",
            "V148",
            Severity::Error,
            "is not a judge edge of `work`",
        ),
        (
            "scenario: child-green",
            "scenario: ../escape",
            "V148",
            Severity::Error,
            "not a scenario name",
        ),
        (
            "output: { pr: \"x\" }",
            "output: \"plain text\"",
            "V148",
            Severity::Error,
            "must be a JSON object",
        ),
        // V149: a secret param set.
        (
            "params: { kind: feature }",
            "params: { kind: feature, token: abc }",
            "V149",
            Severity::Error,
            "`token` is secret",
        ),
        // V150: expectations.
        (
            "visits: { work: 1 }",
            "visits: { wrok: 1 }",
            "V150",
            Severity::Error,
            "node `wrok`",
        ),
        (
            "finish: done",
            "finish: work",
            "V150",
            Severity::Error,
            "not a finish node",
        ),
        (
            "not_taken: [\"id:needs-fix\"]",
            "not_taken: [\"id:nope\"]",
            "V150",
            Severity::Error,
            "names no edge",
        ),
        (
            "dead: { gate: [lint] }",
            "dead: { gate: [start] }",
            "V150",
            Severity::Error,
            "not an input of `gate`",
        ),
        (
            "report: { state: value }",
            "reprot: { state: value }",
            "V150",
            Severity::Error,
            "no output `reprot`",
        ),
        (
            "report: { state: value }",
            "report: { state: set }",
            "V150",
            Severity::Error,
            "state `set`",
        ),
        (
            "outcome: [succeeded]",
            "outcome: [passed]",
            "V150",
            Severity::Error,
            "`passed`",
        ),
        (
            "matches: \"o.\"",
            "matches: \"o(\"",
            "V150",
            Severity::Error,
            "does not compile",
        ),
        // V151: more attempts than the node makes.
        (
            "- { status: failed, output: \"wrong\" }",
            "- { status: failed, output: \"wrong\" }\n        - { status: failed, output: \"again\" }",
            "V151",
            Severity::Warning,
            "at most 2",
        ),
    ];
    for (from, to, code, severity, needle) in cases {
        assert!(
            GREEN.contains(from),
            "case anchor `{from}` not in the green scenario"
        );
        let body = GREEN.replacen(from, to, 1);
        let got = issues_of(&body);
        assert!(
            got.iter()
                .any(|i| i.code == *code && i.severity == *severity && i.message.contains(needle)),
            "`{from}` -> `{to}`: expected {code} with `{needle}`, got {:#?}",
            show(&got)
        );
    }
}

/// A node answered under a judge edge it does not own names that edge, so
/// every surface can point at it.
#[test]
fn an_edge_issue_carries_the_edge_address() {
    let got = issues_of(&GREEN.replacen(
        "\"id:needs-fix\": { p: 0.8 }",
        "\"e:start>work#1\": { p: 0.8 }",
        1,
    ));
    let issue = got.iter().find(|i| i.code == "V148").unwrap();
    assert_eq!(issue.node.as_deref(), Some("work"));
    assert_eq!(issue.edges, vec!["e:start>work#1".to_string()]);
}

/// A node that crosses two boundaries takes both answers in one visit:
/// a list whose item check is a judge (`items` and `judge`), and a judge
/// that may go on to its profile (`judge` and `attempts`). Each part is
/// still held to its own rules, and a node that crosses one of them takes
/// only that one.
#[test]
fn a_node_crossing_two_boundaries_takes_both_answers() {
    let playbook = Playbook::from_yaml(
        r#"
schema: 2
id: pair
name: pair
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: start, type: start }
  - id: files
    type: agent_task
    prompt: per item
    list:
      items: { inline: [ { id: a }, { id: b } ] }
      check:
        judge:
          state: { item: "{{item.id}}" }
          instructions: Is it done?
          criteria: { accept: done, reject: not done }
  - id: plain
    type: agent_task
    prompt: per item
    list:
      items: { inline: [ { id: a } ] }
  - id: triage
    type: judge
    state: { out: "{{nodes.files.output}}" }
    questions:
      verdict: { type: choice, instructions: i, criteria: { done: a, open: b } }
    on_unavailable: emulate
  - id: strict
    type: judge
    state: { out: "{{nodes.files.output}}" }
    questions:
      verdict: { type: choice, instructions: i, criteria: { done: a, open: b } }
    on_unavailable: fail
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: files }
  - { from: files, to: plain }
  - { from: plain, to: triage }
  - { from: triage, to: strict }
  - { from: strict, to: done }
"#,
    )
    .unwrap();
    let check = |answers: &str| -> Vec<Issue> {
        let body = format!("schema: 1\nid: pair\nanswers:\n{answers}");
        let dir = dir_with(&[("pair.yaml", &body)]);
        scenario::validate_all(dir.path(), &playbook)
    };
    let both = "  files:\n    - visit: 1\n      items: { \"*\": { status: succeeded } }\n      judge: { verdict: { value: accept, p: 0.9 } }\n  triage:\n    - visit: 1\n      judge: { unavailable: timeout }\n      attempts: [ { status: succeeded, output: done } ]\n";
    let got = check(both);
    assert!(got.is_empty(), "{:#?}", show(&got));
    // The list check's answer is held to the list's own question.
    let got = check(&both.replace("value: accept", "value: maybe"));
    assert!(
        got.iter()
            .any(|i| i.code == "V148" && i.message.contains("maybe")),
        "{:#?}",
        show(&got)
    );
    // A node that crosses only one of the two takes only that one.
    for (answers, node) in [
        (
            "  plain:\n    - visit: 1\n      items: { \"*\": { status: succeeded } }\n      judge: { verdict: { value: accept } }\n",
            "plain",
        ),
        (
            "  strict:\n    - visit: 1\n      judge: { unavailable: timeout }\n      attempts: [ { status: succeeded } ]\n",
            "strict",
        ),
    ] {
        let got = check(answers);
        assert!(
            got.iter()
                .any(|i| i.code == "V147" && i.node.as_deref() == Some(node)),
            "{node}: {:#?}",
            show(&got)
        );
    }
    // No node takes any other pair, nor three kinds.
    let got = check(
        "  files:\n    - visit: 1\n      items: { \"*\": { status: succeeded } }\n      judge: { verdict: { value: accept } }\n      attempts: [ { status: succeeded } ]\n",
    );
    assert!(
        got.iter()
            .any(|i| i.code == "V146" && i.message.contains("a visit takes one kind")),
        "{:#?}",
        show(&got)
    );
}

/// A scenario kept for other versions is not held to the loaded one, like
/// an eval case; its file problems still are.
#[test]
fn a_scenario_for_other_versions_is_not_held_to_this_one() {
    let other =
        GREEN
            .replacen(">=1.0.0, <2.0.0", ">=2.0.0", 1)
            .replacen("  lint:\n", "  lnit:\n", 1);
    assert!(issues_of(&other).is_empty());
    let broken = other.replacen("schema: 1\n", "schema: 2\n", 1);
    assert!(issues_of(&broken).iter().any(|i| i.code == "V146"));
}

#[test]
fn the_loader_reads_only_named_regular_files_below_the_playbook() {
    let dir = dir_with(&[("green.yaml", GREEN)]);
    // A name outside the grammar never becomes a path.
    for bad in ["../green", "Green", "-x", "", &"a".repeat(65)] {
        let err = scenario::load(dir.path(), bad).unwrap_err();
        assert!(err.contains("must match"), "{bad}: {err}");
    }
    assert!(scenario::is_valid_name(&"a".repeat(64)));
    // A link to a scenario elsewhere is not read.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("x.yaml"),
        GREEN.replace("id: green", "id: linked"),
    )
    .unwrap();
    let scenarios = scenario::scenarios_dir(dir.path());
    std::os::unix::fs::symlink(outside.path().join("x.yaml"), scenarios.join("linked.yaml"))
        .unwrap();
    assert!(scenario::load(dir.path(), "linked").is_err());
    // An oversized file is refused, not read whole.
    let big = format!(
        "{GREEN}#{}\n",
        "x".repeat(scenario::MAX_FILE_BYTES as usize)
    );
    std::fs::write(
        scenarios.join("big.yaml"),
        big.replace("id: green", "id: big"),
    )
    .unwrap();
    assert!(
        scenario::load(dir.path(), "big")
            .unwrap_err()
            .contains("larger than")
    );
    // load_all reports both as V146 and still loads the good one.
    let all = scenario::load_all(dir.path());
    assert_eq!(
        all.scenarios
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["green"]
    );
    assert_eq!(all.issues.len(), 2, "{:#?}", show(&all.issues));
    assert!(all.issues.iter().all(|i| i.code == "V146"));
    // No directory: no scenario, no issue.
    let empty = tempfile::tempdir().unwrap();
    let none = scenario::load_all(empty.path());
    assert!(none.scenarios.is_empty() && none.issues.is_empty());
}

fn parsed(body: &str) -> Scenario {
    scenario::parse(body, "green").unwrap()
}

#[test]
fn the_digest_follows_content_not_form() {
    let base = scenario::scenario_digest(&parsed(GREEN));
    // Comments, key order and a default spelt out do not change it.
    let same = GREEN
        .replacen(
            "schema: 1\nid: green\n",
            "# a comment\nid: green\nschema: 1\non_extra: error\n",
            1,
        )
        .replacen(
            "{ exit: 0, stdout: \"green\" }",
            "{ stdout: green, exit: 0 }",
            1,
        );
    assert_eq!(scenario::scenario_digest(&parsed(&same)), base);
    // An answer, an expectation or a mode does.
    for (from, to) in [
        ("exit: 0, stdout: \"green\"", "exit: 1, stdout: \"green\""),
        ("visits: { work: 1 }", "visits: { work: 2 }"),
        ("params: { kind: feature }", "params: { kind: fix }"),
        ("schema: 1\n", "schema: 1\non_extra: warn\n"),
    ] {
        let changed = GREEN.replacen(from, to, 1);
        assert_ne!(
            scenario::scenario_digest(&parsed(&changed)),
            base,
            "{from} -> {to}"
        );
    }
}

#[test]
fn a_scenario_is_stale_outside_its_versions_or_its_digest_pin() {
    let s = parsed(GREEN);
    assert_eq!(scenario::staleness(&s, "1.2.0", "sha256:a"), None);
    assert!(
        scenario::staleness(&s, "2.0.0", "sha256:a")
            .unwrap()
            .contains("outside")
    );
    let pinned =
        parsed(&GREEN.replacen("instruction:", "playbook_digest: sha256:a\ninstruction:", 1));
    assert_eq!(scenario::staleness(&pinned, "1.2.0", "sha256:a"), None);
    assert!(
        scenario::staleness(&pinned, "1.2.0", "sha256:b")
            .unwrap()
            .contains("pinned")
    );
}
