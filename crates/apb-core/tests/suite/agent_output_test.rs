//! Reply and token usage from each agent CLI's machine output, over
//! recorded outputs (see `apb_core::agent_output` for the shapes).

use apb_core::agent_output::{AgentUsage, UsageSource, has_final_record, reply, usage};

const CLAUDE: &str = include_str!("../fixtures/agent_output/claude_result.json");
const CODEX: &str = include_str!("../fixtures/agent_output/codex_exec.jsonl");
const CODEX_FAILED: &str = include_str!("../fixtures/agent_output/codex_exec_failed.jsonl");
const OPENCODE: &str = include_str!("../fixtures/agent_output/opencode_run.jsonl");
const DSH: &str = include_str!("../fixtures/agent_output/dsh_run.jsonl");
const DSH_RESUME: &str = include_str!("../fixtures/agent_output/dsh_resume.jsonl");
const DSH_BAD_MODEL: &str = include_str!("../fixtures/agent_output/dsh_bad_model.jsonl");
const DSH_NO_SESSION: &str = include_str!("../fixtures/agent_output/dsh_no_session.jsonl");
const DSH_TOOLS: &str = include_str!("../fixtures/agent_output/dsh_tools.jsonl");
const ZCODE: &str = include_str!("../../../apb-engine/tests/fixtures/zcode/json_edit.json");

const REPORT: &str = "hello\n\n```yaml\nstatus: success\nsummary: read the file\n```";

#[test]
fn claude_stream_json_reads_the_final_result_line() {
    let stream = format!(
        "{}\n{}\n{}\n",
        r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
        CLAUDE.trim()
    );
    assert_eq!(reply("claude-code", &stream).unwrap().text, REPORT);
    assert_eq!(usage("claude-code", &stream).unwrap().output_tokens, 443);
}

#[test]
fn claude_is_error_and_usage_without_a_model_breakdown() {
    let out = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"","total_cost_usd":0,"usage":{"input_tokens":5,"output_tokens":7,"cache_read_input_tokens":11,"cache_creation_input_tokens":13}}"#;
    assert!(reply("claude", out).unwrap().is_error);
    assert_eq!(
        usage("claude", out).unwrap(),
        AgentUsage {
            input_tokens: 5,
            output_tokens: 7,
            cache_read_tokens: 11,
            cache_write_tokens: 13,
            // A printed zero is no figure.
            cost_usd: None,
            source: UsageSource::Reported,
            cost_basis: Default::default(),
            priced_from: None,
        }
    );
}

#[test]
fn a_failed_codex_turn_reports_no_reply_and_no_usage() {
    assert_eq!(reply("codex", CODEX_FAILED), None);
    assert_eq!(usage("codex", CODEX_FAILED), None);
}

#[test]
fn zcode_json_usage_excludes_cache_reads_from_input() {
    let u = usage("zcode", ZCODE).unwrap();
    assert_eq!(u.input_tokens, 41775 - 36224);
    assert_eq!(u.cache_read_tokens, 36224);
    assert_eq!(u.output_tokens, 318);
    assert_eq!(u.source, UsageSource::Reported);
}

/// zcode's own `source` label does not make a count apb's estimate: every
/// number the CLI printed is `reported`, so `estimated` stays free for counts
/// apb itself works out.
#[test]
fn zcode_usage_is_reported_whatever_its_own_source_says() {
    assert!(ZCODE.contains(r#""source": "provider""#));
    for label in [r#""source": "local""#, r#""source": "estimate""#, ""] {
        let out = ZCODE.replace(
            r#""source": "provider","#,
            &format!("{label}{}", if label.is_empty() { "" } else { "," }),
        );
        assert_ne!(out, ZCODE, "fixture edit for {label:?}");
        assert_eq!(
            usage("zcode", &out).unwrap().source,
            UsageSource::Reported,
            "{label:?}"
        );
    }
}

#[test]
fn plain_text_output_has_no_reply_and_no_usage() {
    let text = "the answer\n\ntokens used\n12,345";
    for agent in ["claude", "codex", "opencode", "zcode", "hermes", "custom"] {
        assert_eq!(reply(agent, text), None, "{agent}");
        assert_eq!(usage(agent, text), None, "{agent}");
    }
    // Another agent's machine output is not read as this agent's.
    assert_eq!(usage("hermes", CLAUDE), None);
}

#[test]
fn absurd_token_counts_saturate_instead_of_overflowing() {
    let max = u64::MAX;
    let step = format!(
        r#"{{"type":"step_finish","part":{{"tokens":{{"input":{max},"output":{max},"reasoning":{max},"cache":{{"read":0,"write":0}}}}}}}}"#
    );
    let u = usage("opencode", &format!("{step}\n{step}\n")).unwrap();
    assert_eq!((u.input_tokens, u.output_tokens), (max, max));
}

#[test]
fn a_finished_turn_without_a_message_is_an_empty_reply() {
    let codex = CODEX
        .lines()
        .filter(|l| !l.contains("agent_message"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(reply("codex", &codex).unwrap().text, "");
    let opencode = OPENCODE
        .lines()
        .filter(|l| !l.contains(r#""type": "text""#))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(reply("opencode", &opencode).unwrap().text, "");
}

/// Counts whose overlap apb cannot verify are kept as the CLI printed them:
/// codex's cache writes are not taken out of its input (only the cache reads
/// it documents as inclusive are), and zcode's reasoning tokens are not added
/// to its output.
#[test]
fn unverifiable_overlaps_are_kept_as_the_cli_reports_them() {
    let codex = r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":600,"cache_write_input_tokens":250,"output_tokens":80,"reasoning_output_tokens":30}}"#;
    let u = usage("codex", codex).unwrap();
    assert_eq!(u.input_tokens, 1000 - 600);
    assert_eq!(u.cache_read_tokens, 600);
    assert_eq!(u.cache_write_tokens, 250);
    assert_eq!(u.output_tokens, 80);

    let zcode = ZCODE.replace(r#""reasoningTokens": 0"#, r#""reasoningTokens": 40"#);
    assert_ne!(zcode, ZCODE);
    assert_eq!(usage("zcode", &zcode).unwrap().output_tokens, 318);
}

#[test]
fn the_final_record_is_found_in_each_machine_format_and_missing_when_cut() {
    for (agent, out) in [
        ("claude", CLAUDE),
        ("codex", CODEX),
        ("opencode", OPENCODE),
        ("zcode", ZCODE),
        ("dsh", DSH),
        ("dsh", DSH_NO_SESSION),
    ] {
        assert_eq!(has_final_record(agent, out), Some(true), "{agent}");
        // The closing record torn in half is not a record.
        let cut = &out[..out.trim_end().len() - 5];
        assert_eq!(has_final_record(agent, cut), Some(false), "{agent} cut");
    }
    assert_eq!(has_final_record("hermes", "plain text reply"), None);
}

/// A failed run ends with its own terminal record instead of the success
/// one; that is a whole ending, not a cut one.
#[test]
fn a_failed_run_ends_with_a_final_record_too() {
    for (agent, last) in [
        (
            "codex",
            r#"{"type":"turn.failed","error":{"message":"rate limited"}}"#,
        ),
        ("codex", r#"{"type":"error","message":"stream error"}"#),
        (
            "opencode",
            r#"{"type":"error","error":{"name":"APIError"}}"#,
        ),
    ] {
        let out = format!("{{\"type\":\"item.started\"}}\n{last}\n");
        assert_eq!(has_final_record(agent, &out), Some(true), "{agent}: {last}");
    }
}

/// dsh 0.2.2-rc.1 captures: a run, its resume, a refused model and a
/// refused session; and an older build's run with tool calls, whose final
/// record carries the text alone.
#[test]
fn dsh_json_events_read_reply_session_and_usage() {
    assert_eq!(reply("dsh", DSH).unwrap().text, "MANGO");
    assert_eq!(
        usage("dsh", DSH).unwrap(),
        AgentUsage {
            input_tokens: 8491,
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
            source: UsageSource::Reported,
            cost_basis: Default::default(),
            priced_from: None,
        }
    );
    let resumed = usage("dsh", DSH_RESUME).unwrap();
    assert_eq!(
        (resumed.input_tokens, resumed.cache_read_tokens),
        (188, 8320)
    );
    let failed = reply("dsh", DSH_BAD_MODEL).unwrap();
    assert!(failed.is_error);
    assert!(
        failed
            .text
            .starts_with("dsh error INVALID_REQUEST (status 400): The supported API model names"),
        "{}",
        failed.text
    );
    // A failed step reports no usage sample.
    assert_eq!(usage("dsh", DSH_BAD_MODEL), None);
    let refused = reply("dsh", DSH_NO_SESSION).unwrap();
    assert!(refused.is_error && refused.text.ends_with("(session not found)"));
    let tools = usage("dsh", DSH_TOOLS).unwrap();
    assert_eq!(tools.input_tokens, 8609 + 673 + 988);
    assert_eq!(tools.cache_read_tokens, 8576 + 16896);
    assert_eq!(reply("dsh", DSH_TOOLS).unwrap().text, "Report written.");
}
