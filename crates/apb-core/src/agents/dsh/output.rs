//! dsh's machine output, checked against dsh 0.2.2-rc.1 on 2026-11-09.
//!
//! `--profile headless --json` prints one JSON object per line:
//!
//! - `session {sessionId, cwd}` first, before any model call;
//! - `status` with `phase` `turn_start`, `step_start`, `step_end` (with
//!   `usage {inputTokens, outputTokens, cacheReadTokens?, cacheWriteTokens?,
//!   reasoningTokens?, totalTokens?}` when every attempt of the step
//!   reported one) and `turn_end` (with `reason`);
//! - `thinking {text}`, `text {text}`, `tool_call {callId, tool, input}`,
//!   `tool_result {callId, status, result, error?}`;
//! - last, `final {sessionId, reason, text}`: the reply and why the turn
//!   ended (`{kind: completed}`, `{kind: error, error: {code, message,
//!   status?}}`, `aborted`, `blocked`, `max-tokens`, `interrupted`, or
//!   `null` without a terminal turn). Builds before 0.2.2 printed `final
//!   {text}` alone, read as completed;
//! - instead, `error {message}` when the one-shot driver itself failed (an
//!   unknown session, a profile that does not compose, a usage error).
//!
//! dsh exits 0 only when the reason is `completed`. Usage counts are
//! disjoint (dsh's `TokenUsage`): `inputTokens` is uncached input, cache
//! reads and writes are separate, and they are summed over every
//! `step_end` as printed. No cost is printed.

use serde_json::Value;

use crate::agents::output::{
    AgentUsage, OutputFormat, Promises, Reply, UsageSource, json_lines, json_string_field,
    last_line_is, str_at, u64_at,
};

/// dsh's output format.
pub struct DshOutput;

/// The words apb's failure classifier reads for one of dsh's stable error
/// codes (`RATE_LIMIT` is a quota, `QUOTA` a spent balance, ...); `None`
/// for a code whose meaning needs no translation.
fn class_words(code: &str) -> Option<&'static str> {
    Some(match code {
        "RATE_LIMIT" => "rate limit",
        "QUOTA" | "ACCOUNT_QUOTA" => "quota exceeded",
        "AUTH" | "MISSING_CREDENTIAL" | "INVALID_CREDENTIAL" => "authentication failed",
        "SERVER" => "server error",
        "TIMEOUT" => "request timed out",
        "TRANSPORT" => "connection error",
        _ => return None,
    })
}

/// A driver message refusing a resume: the session does not exist, or it
/// was recorded in another working directory, so it can never be adopted
/// here and a fresh session is the only way on.
fn refuses_session(message: &str) -> bool {
    message.starts_with("session \"")
        && (message.contains("\" does not exist") || message.contains("\" was recorded in"))
}

/// The line apb shows for a driver `error` event.
fn driver_error(message: &str) -> String {
    if refuses_session(message) {
        format!("dsh error: {message} (session not found)")
    } else {
        format!("dsh error: {message}")
    }
}

/// The line apb shows for a turn that did not complete, `None` for one
/// that did.
fn turn_failure(reason: &Value) -> Option<String> {
    let kind = str_at(reason, "kind")?;
    if kind == "completed" {
        return None;
    }
    let Some(err) = reason.get("error").filter(|_| kind == "error") else {
        return Some(format!("dsh turn ended: {kind}"));
    };
    let code = str_at(err, "code").unwrap_or("UNKNOWN");
    let message = str_at(err, "message").unwrap_or_default();
    let mut head = format!("dsh error {code}");
    let mut notes: Vec<String> = class_words(code).map(str::to_string).into_iter().collect();
    if let Some(status) = err.get("status").and_then(Value::as_u64) {
        notes.push(format!("status {status}"));
    }
    if !notes.is_empty() {
        head.push_str(&format!(" ({})", notes.join(", ")));
    }
    Some(format!("{head}: {message}"))
}

impl OutputFormat for DshOutput {
    fn promises(&self) -> Promises {
        Promises {
            error_flag: true,
            ..Promises::MACHINE
        }
    }

    // The last `final` event's text; a turn that did not complete is the
    // text so far and the failure line, marked failed. A driver `error`
    // without a `final` is the failure line alone.
    fn reply(&self, stdout: &str) -> Option<Reply> {
        let mut last: Option<Value> = None;
        let mut error: Option<String> = None;
        for v in json_lines(stdout) {
            match str_at(&v, "type") {
                Some("final") => last = Some(v),
                Some("error") => error = str_at(&v, "message").map(driver_error),
                _ => {}
            }
        }
        let Some(fin) = last else {
            return error.map(|text| Reply {
                text,
                is_error: true,
            });
        };
        let text = str_at(&fin, "text").unwrap_or_default().to_string();
        match fin.get("reason").and_then(turn_failure) {
            None => Some(Reply::ok(text)),
            Some(why) => Some(Reply {
                text: if text.trim().is_empty() {
                    why
                } else {
                    format!("{text}\n{why}")
                },
                is_error: true,
            }),
        }
    }

    fn usage(&self, stdout: &str) -> Option<AgentUsage> {
        let steps: Vec<Value> = json_lines(stdout)
            .filter(|v| {
                str_at(v, "type") == Some("status") && str_at(v, "phase") == Some("step_end")
            })
            .filter_map(|v| v.get("usage").filter(|u| u.is_object()).cloned())
            .collect();
        if steps.is_empty() {
            return None;
        }
        let sum = |key: &str| {
            steps
                .iter()
                .fold(0u64, |acc, u| acc.saturating_add(u64_at(u, key)))
        };
        Some(AgentUsage {
            input_tokens: sum("inputTokens"),
            output_tokens: sum("outputTokens"),
            cache_read_tokens: sum("cacheReadTokens"),
            cache_write_tokens: sum("cacheWriteTokens"),
            cost_usd: None,
            source: UsageSource::Reported,
            cost_basis: Default::default(),
        })
    }

    fn has_final_record(&self, stdout: &str) -> Option<bool> {
        Some(last_line_is(stdout, &["final", "error"]))
    }

    fn session_id(&self, stdout: &str) -> Option<String> {
        json_string_field(stdout, &["sessionId"])
    }

    // The `session` event is the first line, written before any model call.
    fn opening_session(&self, stdout: &str) -> Option<String> {
        let first = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
        let v: Value = serde_json::from_str(first).ok()?;
        (str_at(&v, "type") == Some("session"))
            .then(|| str_at(&v, "sessionId").map(str::to_string))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUCCESS: &str = concat!(
        r#"{"type":"session","sessionId":"session-1","cwd":"/work"}"#,
        "\n",
        r#"{"type":"status","phase":"turn_start","turn":1}"#,
        "\n",
        r#"{"type":"status","phase":"step_start","turn":1,"step":1}"#,
        "\n",
        r#"{"type":"tool_call","callId":"c1","tool":"bash","input":{"command":"ls"}}"#,
        "\n",
        r#"{"type":"status","phase":"step_end","turn":1,"step":1,"usage":{"inputTokens":100,"outputTokens":7,"cacheReadTokens":900,"totalTokens":1007}}"#,
        "\n",
        r#"{"type":"status","phase":"step_end","turn":1,"step":2,"usage":{"inputTokens":10,"outputTokens":3,"cacheReadTokens":20,"cacheWriteTokens":5,"totalTokens":38}}"#,
        "\n",
        r#"{"type":"status","phase":"turn_end","turn":1,"reason":{"kind":"completed"}}"#,
        "\n",
        r#"{"type":"final","sessionId":"session-1","reason":{"kind":"completed"},"text":"MANGO"}"#,
        "\n",
    );

    #[test]
    fn the_final_event_is_the_reply_and_the_steps_are_the_usage() {
        let o = DshOutput;
        assert_eq!(o.reply(SUCCESS), Some(Reply::ok("MANGO".into())));
        assert_eq!(o.session_id(SUCCESS).as_deref(), Some("session-1"));
        assert_eq!(o.has_final_record(SUCCESS), Some(true));
        let u = o.usage(SUCCESS).unwrap();
        assert_eq!(
            (
                u.input_tokens,
                u.output_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens
            ),
            (110, 10, 920, 5)
        );
        assert_eq!(u.cost_usd, None);
    }

    /// The final record of an older build carries the text alone.
    #[test]
    fn a_final_without_a_reason_reads_as_completed() {
        let out = "{\"type\":\"session\",\"sessionId\":\"session-2\",\"cwd\":\"/w\"}\n{\"type\":\"final\",\"text\":\"done\"}\n";
        assert_eq!(DshOutput.reply(out), Some(Reply::ok("done".into())));
        assert_eq!(DshOutput.session_id(out).as_deref(), Some("session-2"));
    }

    #[test]
    fn an_error_reason_is_a_failed_reply_in_the_classifiers_words() {
        let out = r#"{"type":"final","sessionId":"s","reason":{"kind":"error","error":{"code":"RATE_LIMIT","message":"slow down","status":429}},"text":""}"#;
        let r = DshOutput.reply(out).unwrap();
        assert!(r.is_error);
        assert_eq!(
            r.text,
            "dsh error RATE_LIMIT (rate limit, status 429): slow down"
        );
        let partial = r#"{"type":"final","reason":{"kind":"max-tokens"},"text":"half"}"#;
        let r = DshOutput.reply(partial).unwrap();
        assert!(r.is_error);
        assert_eq!(r.text, "half\ndsh turn ended: max-tokens");
    }

    #[test]
    fn a_refused_resume_reads_as_a_missing_session() {
        for msg in [
            "session \\\"session-x\\\" does not exist; omit --session-id to start a new Session",
            "session \\\"session-x\\\" was recorded in \\\"/a\\\", not \\\"/b\\\"",
        ] {
            let out = format!("{{\"type\":\"error\",\"message\":\"{msg}\"}}\n");
            let r = DshOutput.reply(&out).unwrap();
            assert!(r.is_error);
            assert!(r.text.ends_with("(session not found)"), "{}", r.text);
            assert_eq!(DshOutput.has_final_record(&out), Some(true));
        }
        let other = "{\"type\":\"error\",\"message\":\"a task is required\"}\n";
        assert_eq!(
            DshOutput.reply(other).unwrap().text,
            "dsh error: a task is required"
        );
    }

    /// The session record dsh prints before any work is its first line, and
    /// only that line: a later line naming another id does not count.
    #[test]
    fn the_opening_session_record_is_the_first_line_only() {
        assert_eq!(
            DshOutput.opening_session(SUCCESS).as_deref(),
            Some("session-1")
        );
        let late = "{\"type\":\"status\",\"phase\":\"turn_start\"}\n{\"type\":\"session\",\"sessionId\":\"session-2\"}\n";
        assert_eq!(DshOutput.opening_session(late), None);
        assert_eq!(DshOutput.opening_session("MANGO\n"), None);
    }

    #[test]
    fn plain_text_is_not_the_machine_format() {
        assert_eq!(DshOutput.reply("MANGO\n"), None);
        assert_eq!(DshOutput.usage("MANGO\n"), None);
        assert_eq!(DshOutput.has_final_record("MANGO\n"), Some(false));
        let cut = SUCCESS.lines().take(4).collect::<Vec<_>>().join("\n");
        assert_eq!(DshOutput.has_final_record(&cut), Some(false));
    }
}
