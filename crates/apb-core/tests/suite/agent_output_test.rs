//! Reply and token usage from each agent CLI's machine output, over
//! recorded outputs (see `apb_core::agent_output` for the shapes).

use apb_core::agent_output::{AgentUsage, UsageSource, reply, usage};

const CLAUDE: &str = include_str!("../fixtures/agent_output/claude_result.json");
const CODEX: &str = include_str!("../fixtures/agent_output/codex_exec.jsonl");
const CODEX_FAILED: &str = include_str!("../fixtures/agent_output/codex_exec_failed.jsonl");
const OPENCODE: &str = include_str!("../fixtures/agent_output/opencode_run.jsonl");
const ZCODE: &str = include_str!("../../../apb-engine/tests/fixtures/zcode/json_edit.json");

const REPORT: &str = "hello\n\n```yaml\nstatus: success\nsummary: read the file\n```";

#[test]
fn claude_json_result_gives_reply_usage_and_reported_cost() {
    let r = reply("claude", CLAUDE).unwrap();
    assert_eq!(r.text, REPORT);
    assert!(!r.is_error);
    assert_eq!(
        usage("claude", CLAUDE).unwrap(),
        AgentUsage {
            input_tokens: 18,
            output_tokens: 443,
            cache_read_tokens: 52079,
            cache_write_tokens: 14967,
            cost_usd: Some(0.0373749),
            source: UsageSource::Reported,
        }
    );
}

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
            // A zero cost is what a subscription prints, not a price.
            cost_usd: None,
            source: UsageSource::Reported,
        }
    );
}

#[test]
fn codex_exec_json_gives_the_last_agent_message_and_turn_usage() {
    assert_eq!(reply("codex", CODEX).unwrap().text, REPORT);
    assert_eq!(
        usage("codex", CODEX).unwrap(),
        AgentUsage {
            input_tokens: 24763 - 24448,
            output_tokens: 122,
            cache_read_tokens: 24448,
            cache_write_tokens: 0,
            cost_usd: None,
            source: UsageSource::Reported,
        }
    );
}

#[test]
fn a_failed_codex_turn_reports_no_reply_and_no_usage() {
    assert_eq!(reply("codex", CODEX_FAILED), None);
    assert_eq!(usage("codex", CODEX_FAILED), None);
}

#[test]
fn opencode_json_sums_every_step() {
    assert_eq!(reply("opencode", OPENCODE).unwrap().text, REPORT);
    assert_eq!(
        usage("opencode", OPENCODE).unwrap(),
        AgentUsage {
            input_tokens: 63799 + 217,
            output_tokens: 56 + 24 + 3 + 17,
            cache_read_tokens: 63744,
            cache_write_tokens: 0,
            cost_usd: None,
            source: UsageSource::Reported,
        }
    );
}

#[test]
fn zcode_json_usage_excludes_cache_reads_from_input() {
    let u = usage("zcode", ZCODE).unwrap();
    assert_eq!(u.input_tokens, 41775 - 36224);
    assert_eq!(u.cache_read_tokens, 36224);
    assert_eq!(u.output_tokens, 318);
    assert_eq!(u.source, UsageSource::Reported);
    let own_count = ZCODE.replace(r#""source": "provider""#, r#""source": "local""#);
    assert_eq!(
        usage("zcode", &own_count).unwrap().source,
        UsageSource::Estimated
    );
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
