//! Reading an attempt's transcript as records: claude's session yields
//! tool calls, skill loads and sub-agent messages; the completeness of each
//! stream follows what the attempt's `attempt_finished` says was cut.

use apb_engine::event::read_journal;
use apb_engine::transcript::{self, Stream};

use crate::quality_common::{CLAUDE_SESSION, agent_journal, agent_playbook, attempt_file, run_dir};

fn views(journal_extra: &str, agent: &str) -> Vec<transcript::View> {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let dir = run_dir(
        tmp.path(),
        "r1",
        &agent_journal(agent, &work, journal_extra, "succeeded"),
        &agent_playbook(""),
    );
    attempt_file(&dir, "fix-1", "session.jsonl", CLAUDE_SESSION);
    attempt_file(&dir, "fix-1", "stdout.log", "line one\nline two\n");
    attempt_file(&dir, "fix-1", "stderr.log", "");
    let events = read_journal(&dir).unwrap().events;
    transcript::sources(&dir, &events)
        .iter()
        .map(transcript::read)
        .collect()
}

#[test]
fn a_claude_session_yields_skill_loads_sidechains_and_repeated_calls() {
    let views = views("", "claude");
    let streams: Vec<Stream> = views.iter().map(|v| v.source.stream).collect();
    assert_eq!(streams, [Stream::Session, Stream::Stdout, Stream::Stderr]);
    let session = &views[0];
    assert!(session.complete);
    assert_eq!(session.id(), "t:fix-1/session");
    let skills: Vec<(u64, &str)> = session
        .records
        .iter()
        .flat_map(|r| {
            r.tools
                .iter()
                .filter_map(move |t| t.skill.as_deref().map(|s| (r.index, s)))
        })
        .collect();
    assert_eq!(skills, [(1, "rust-skills")]);
    let sidechain: Vec<u64> = session
        .records
        .iter()
        .filter(|r| r.sidechain)
        .map(|r| r.index)
        .collect();
    assert_eq!(sidechain, [9]);
    let tests: Vec<&str> = session
        .records
        .iter()
        .flat_map(|r| r.tools.iter())
        .filter(|t| t.name == "Bash")
        .map(|t| t.input_digest.as_str())
        .collect();
    assert_eq!(tests.len(), 3);
    assert!(tests.iter().all(|d| *d == tests[0]));
    assert_eq!(
        session.records.iter().map(|r| r.tool_errors).sum::<u32>(),
        3
    );
    let edit = session.records[10].tools[0].clone();
    assert!(edit.writes);
    assert_eq!(edit.paths, ["src/parser.rs"]);
    // The plain streams are one record per line.
    assert_eq!(views[1].records.len(), 2);
    assert_eq!(views[1].record_ref(1), "t:fix-1/stdout#1");
}

#[test]
fn a_cut_or_unfinished_stream_is_incomplete() {
    let cut = views(
        r#","output_truncated":[{"stream":"session_copy","kept_bytes":10,"dropped_bytes_at_least":90}]"#,
        "claude",
    );
    assert!(!cut[0].complete, "a session cut at the copy cap");
    assert!(cut[1].complete, "stdout was not cut");
    let lost = views(r#","transcript_incomplete":true"#, "claude");
    assert!(!lost[0].complete && !lost[1].complete);
    assert!(lost[2].complete, "stderr does not carry the final record");
}

#[test]
fn a_torn_last_line_makes_the_session_incomplete() {
    let (records, whole) =
        transcript::claude::parse(&format!("{CLAUDE_SESSION}{{\"type\":\"assistant\",\"mess"));
    assert!(!whole);
    assert_eq!(records.len(), 14);
}

#[test]
fn another_agent_has_no_session_view() {
    let views = views("", "codex");
    let streams: Vec<Stream> = views.iter().map(|v| v.source.stream).collect();
    assert_eq!(streams, [Stream::Stdout, Stream::Stderr]);
    assert!(views.iter().all(|v| !v.source.structured()));
}

/// The views of a run whose journal is `journal`, with the code fixture's
/// files under `attempts/fix-1`.
fn views_of_journal(journal: &str) -> Vec<transcript::View> {
    let tmp = tempfile::tempdir().unwrap();
    let dir = run_dir(tmp.path(), "r1", journal, &agent_playbook(""));
    attempt_file(&dir, "fix-1", "session.jsonl", CLAUDE_SESSION);
    attempt_file(&dir, "fix-1", "stdout.log", "line one\n");
    let events = read_journal(&dir).unwrap().events;
    transcript::sources(&dir, &events)
        .iter()
        .map(transcript::read)
        .collect()
}

#[test]
fn an_attempt_without_a_readable_transcript_is_an_empty_incomplete_view() {
    let work = std::path::Path::new("/work");
    for (from, to) in [
        (r#","transcript":"attempts/fix-1""#, ""),
        (r#""attempts/fix-1""#, r#""../outside""#),
    ] {
        let journal = agent_journal("claude", work, "", "succeeded").replace(from, to);
        let views = views_of_journal(&journal);
        assert_eq!(views.len(), 1, "{to}");
        let v = &views[0];
        assert!(!v.complete && v.records.is_empty() && !v.source.tracked);
        assert_eq!(v.id(), "t:@untracked-2/stdout");
    }
}

#[test]
fn a_rerun_that_reuses_a_stem_keeps_one_view_of_it() {
    // A resumed run restarted the attempt counter: attempt 1 of `fix`
    // started twice and its directory holds the second one's output.
    let journal = agent_journal("claude", std::path::Path::new("/work"), "", "succeeded");
    let started = journal.lines().nth(2).unwrap().to_string();
    let again = started.replace(r#""seq":2,"ts":1002"#, r#""seq":3,"ts":1003"#);
    let mut lines: Vec<String> = journal.lines().map(str::to_string).collect();
    lines.insert(3, again);
    let renumbered: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let seq = l.split(',').next().unwrap_or_default().to_string();
            l.replacen(&seq, &format!("{{\"seq\":{i}"), 1)
        })
        .collect();
    let views = views_of_journal(&(renumbered.join("\n") + "\n"));
    let sessions: Vec<&transcript::View> = views
        .iter()
        .filter(|v| v.source.stream == Stream::Session)
        .collect();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].source.attempt_seq, 3);
    assert!(sessions[0].complete);
}
