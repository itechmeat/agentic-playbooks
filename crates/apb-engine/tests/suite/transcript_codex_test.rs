//! codex's `exec --json` stdout as a transcript view: one record per line,
//! its text the line as written, with the tool facts its finished items
//! carry. The rules grader (`rules/1`) still reads facts from claude's
//! session only: a codex run grades the same as before codex had facts.

use apb_engine::event::read_journal;
use apb_engine::grade::grade_rules;
use apb_engine::transcript::{self, Stream};

use crate::quality_common::{
    agent_journal, agent_playbook, attempt_file, manifest_with_skills, run_dir,
};

const CODEX_EXEC: &str = include_str!("../fixtures/codex_exec.jsonl");

/// A codex run whose attempt printed `stdout`, and the run directory.
fn codex_run(tmp: &std::path::Path, stdout: &str) -> std::path::PathBuf {
    let dir = run_dir(
        tmp,
        "r1",
        &agent_journal("codex", &tmp.join("work"), "", "succeeded"),
        &agent_playbook(""),
    );
    attempt_file(&dir, "fix-1", "stdout.log", stdout);
    attempt_file(&dir, "fix-1", "stderr.log", "");
    dir
}

fn stdout_view(stdout: &str) -> transcript::View {
    let tmp = tempfile::tempdir().unwrap();
    let dir = codex_run(tmp.path(), stdout);
    let events = read_journal(&dir).unwrap().events;
    transcript::sources(&dir, &events)
        .iter()
        .find(|s| s.stream == Stream::Stdout)
        .map(transcript::read)
        .unwrap()
}

#[test]
fn a_codex_exec_stream_keeps_its_lines_and_carries_tool_facts() {
    let view = stdout_view(CODEX_EXEC);
    assert!(view.complete);
    assert!(!view.source.structured());
    let lines: Vec<&str> = CODEX_EXEC.lines().collect();
    let texts: Vec<&str> = view.records.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, lines);
    let with_tools: Vec<(u64, &str)> = view
        .records
        .iter()
        .flat_map(|r| r.tools.iter().map(move |t| (r.index, t.name.as_str())))
        .collect();
    assert_eq!(
        with_tools,
        [
            (3, "command_execution"),
            (4, "file_change"),
            (5, "command_execution"),
            (6, "command_execution"),
            (7, "mcp_tool_call"),
            (8, "command_execution"),
        ]
    );
    // The same command three times reads as one repeated call, whatever it
    // printed.
    let digest = |i: usize| view.records[i].tools[0].input_digest.clone();
    assert_eq!(digest(3), digest(5));
    assert_eq!(digest(3), digest(6));
    let change = &view.records[4].tools[0];
    assert!(change.writes);
    assert_eq!(change.paths, ["/etc/apb-codex-test"]);
    assert!(view.records[8].tools[0].destructive);
    assert!(!view.records[3].tools[0].destructive);
    assert_eq!(view.records[6].tool_errors, 1);
    assert_eq!(view.record_ref(8), "t:fix-1/stdout#8");
    // An item that only started is not a call.
    assert!(view.records[2].tools.is_empty());
}

#[test]
fn a_torn_line_makes_the_codex_view_incomplete() {
    let view = stdout_view(&format!("{CODEX_EXEC}{{\"type\":\"item.comp"));
    assert!(!view.complete);
    assert_eq!(view.records.last().unwrap().text, "{\"type\":\"item.comp");
}

#[test]
fn a_plain_codex_output_reads_as_plain_lines() {
    let view = stdout_view("I reviewed the change.\nNothing else to do.\n");
    assert!(view.complete);
    assert_eq!(view.records.len(), 2);
    assert!(view.records.iter().all(|r| r.tools.is_empty()));
    // A line that is JSON but not an event does not make it the machine
    // form: the plain view stays whole.
    let view = stdout_view("Counted the failures:\n42\n{}\nnull\nDone.\n");
    assert!(view.complete);
    assert_eq!(view.records.len(), 5);
}

#[test]
fn a_json_line_without_a_type_in_the_machine_form_leaves_the_view_incomplete() {
    let text = format!("{CODEX_EXEC}{{\"usage\": 1}}\n");
    let view = stdout_view(&text);
    assert!(!view.complete);
    assert_eq!(view.records.len(), CODEX_EXEC.lines().count() + 1);
}

/// `rules/1` on a codex run: what it concluded per criterion is pinned, so
/// the facts the codex stream now carries never change a `rules/1` grade.
#[test]
fn rules_1_grades_a_codex_run_as_before_it_had_tool_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = codex_run(tmp.path(), CODEX_EXEC);
    manifest_with_skills(&dir, &["rust-skills"]);
    let g = grade_rules(&dir).unwrap();
    assert_eq!(g.grader.to_string(), "rules/1");
    let got: Vec<serde_json::Value> = g
        .criteria
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "level": c.level.as_str(),
                "reason": c.reason_code,
                "cites": c.cites.iter().map(|x| x.r#ref.as_str()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let unseen = |id: &str| serde_json::json!({"id": id, "level": "not_enough_data", "reason": "unstructured_transcript", "cites": []});
    assert_eq!(
        got,
        [
            serde_json::json!({"id": "outcome", "level": "good", "reason": "succeeded", "cites": ["j:5"]}),
            unseen("loops"),
            unseen("scope"),
            unseen("consent"),
            serde_json::json!({"id": "skill_use", "level": "not_enough_data", "reason": "skill_read_unknown", "cites": []}),
        ]
    );
    let views: Vec<(&str, bool)> = g
        .reading
        .views
        .iter()
        .map(|v| (v.view.as_str(), v.complete))
        .collect();
    assert_eq!(
        views,
        [
            ("journal", true),
            ("outputs", true),
            ("connector_calls", true),
            ("findings", true),
            ("t:fix-1/stdout", true),
            ("t:fix-1/stderr", true),
        ]
    );
}
