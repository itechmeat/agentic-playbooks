//! MCP answers and failures to call results (design D3 "Mapping").

use apb_core::secret::Redaction;
use apb_mcp_client::{CallFailure, FailureKind, ToolAnswer};
use serde_json::{Value, json};

use super::redact::{redact_tree, server_text};
use crate::connector::call::encode::project;
use crate::connector::call::wire::BODY_CAP;
use crate::connector::completeness::BodyIncomplete;
use crate::connector::result::{CallError, CallErrorCode, CallOk};

/// The one way this module builds a `CallError` (design D3 "Mapping"):
/// the whole composed message, apb's words and every server-provided part
/// (text, a JSON-RPC code, any number), passes the account's redactions
/// first and is then bounded by `server_text`. A server's number is kept
/// as a number only to classify, never shown past this point.
fn composed(code: CallErrorCode, message: String, redactions: &[Redaction]) -> CallError {
    CallError::new(code, server_text(&message, redactions))
}

/// A connect, list or pin failure, before the call is handed over: it only
/// names the failure, since nothing was recorded to settle.
pub(crate) fn connect_error(f: apb_mcp_client::Failure, redactions: &[Redaction]) -> CallError {
    let message = f.message;
    let (code, message) = match f.kind {
        FailureKind::Spawn | FailureKind::Transport => (
            CallErrorCode::Network,
            format!("the MCP server could not be reached: {message}"),
        ),
        FailureKind::Timeout => (CallErrorCode::Timeout, message),
        FailureKind::Unsupported => (CallErrorCode::Config, message),
        FailureKind::TooLarge | FailureKind::Rejected { .. } => (CallErrorCode::Service, message),
        FailureKind::Refused | FailureKind::ProgramChanged => (CallErrorCode::Permission, message),
    };
    composed(code, message, redactions)
}

/// The answer of a call that was handed over (design D3 table): for a
/// write, every answer but a completed one without `isError` leaves the
/// effect unknown; a read fails on any of them.
pub(crate) fn answer(
    answer: Result<ToolAnswer, apb_mcp_client::CallFailure>,
    read_only: bool,
    response_pick: &[String],
    redactions: &[Redaction],
) -> Result<CallOk, CallError> {
    // Every message below is composed first and then redacted whole and
    // bounded (`composed`).
    let tool_error = |m: String| Err(composed(CallErrorCode::ToolError, m, redactions));
    match answer {
        Ok(ToolAnswer::Complete {
            content,
            structured,
            is_error: false,
        }) => complete(&content, structured, read_only, response_pick, redactions),
        Ok(ToolAnswer::Complete {
            content,
            is_error: true,
            ..
        }) => {
            // The text blocks joined as they are, never parsed: an error
            // text that reads as JSON stays text. Redacted whole, then cut.
            let text = text_blocks(&content).0.join("\n");
            tool_error(format!(
                "the MCP tool reported an error; whether it acted before it failed is not known: {text}"
            ))
        }
        Ok(ToolAnswer::Task) => tool_error(
            "the server started a task apb does not follow; its effect is unknown".to_string(),
        ),
        Ok(ToolAnswer::InputRequired) => tool_error(
            "the server asked for input apb cannot give (multi round-trip); whether the tool acted before it asked is not known".to_string(),
        ),
        Err(CallFailure::Answered(f)) => match f.kind {
            FailureKind::Rejected { code } => {
                let code_of_read = match code {
                    -32700 | -32600 => Some(CallErrorCode::Service),
                    -32601 => Some(CallErrorCode::Config),
                    -32602 => Some(CallErrorCode::InvalidArgs),
                    _ => None,
                };
                // The code is the server's number: in the message only
                // through the whole-message redaction.
                let message = format!(
                    "the server answered the call with JSON-RPC error {code} ({}); whether the tool acted before it failed is not known",
                    f.message
                );
                match code_of_read.filter(|_| read_only) {
                    Some(c) => Err(composed(c, message, redactions)),
                    None => tool_error(message),
                }
            }
            _ => Err(lost(f, redactions)),
        },
        Err(CallFailure::Lost(f)) => Err(lost(f, redactions)),
        Err(CallFailure::NotSent(f)) => Err(not_sent(f, redactions)),
    }
}

/// A call whose request never reached the server (the session closed, or
/// the pipe refused its first byte): a failure before the hand-over, so a
/// recorded intent settles `not_sent`.
fn not_sent(f: apb_mcp_client::Failure, redactions: &[Redaction]) -> CallError {
    composed(
        CallErrorCode::Service,
        format!(
            "the call never reached the MCP server; nothing was sent: {}",
            f.message
        ),
        redactions,
    )
}

/// A call handed over with no answer: a timeout, or a transport that
/// broke after the request was written.
fn lost(f: apb_mcp_client::Failure, redactions: &[Redaction]) -> CallError {
    let code = match f.kind {
        FailureKind::Timeout => CallErrorCode::Timeout,
        _ => CallErrorCode::Network,
    };
    composed(code, f.message, redactions)
}

/// A completed answer that reports no error: its body by the body rules,
/// redacted, bounded by `BODY_CAP` and projected.
fn complete(
    content: &[Value],
    structured: Option<Value>,
    read_only: bool,
    response_pick: &[String],
    redactions: &[Redaction],
) -> Result<CallOk, CallError> {
    let (body, dropped) = body(content, structured);
    // Strings, member names and numbers alike: the walk the listing check
    // uses.
    let (body, _) = redact_tree(body, redactions);
    let size = serde_json::to_string(&body).map_or(usize::MAX, |s| s.len());
    if size > BODY_CAP {
        let cut = BodyIncomplete::cut_at_cap(BODY_CAP);
        if read_only {
            let mut err = CallError::new(
                CallErrorCode::IncompleteResponse,
                format!(
                    "the MCP answer's body is over the {BODY_CAP} byte cap, so the result would be incomplete; narrow the request and call again"
                ),
            );
            err.incomplete = Some(cut);
            return Err(err);
        }
        return Ok(CallOk::Mcp {
            body: Value::Null,
            incomplete: Some(cut),
            picked: false,
            dropped,
        });
    }
    let picked = !response_pick.is_empty();
    let body = if picked {
        project(&body, response_pick)
    } else {
        body
    };
    Ok(CallOk::Mcp {
        body,
        incomplete: None,
        picked,
        dropped,
    })
}

/// The body of an answer and the count of the content blocks it leaves
/// out: `structuredContent` when present (an explicit `null` included);
/// else the text blocks joined with a newline, parsed as JSON when they
/// parse, else `{"text": <joined>}`. Every block that is not text is
/// dropped and counted.
pub(crate) fn body(content: &[Value], structured: Option<Value>) -> (Value, u32) {
    let (texts, dropped) = text_blocks(content);
    if let Some(v) = structured {
        return (v, dropped);
    }
    let joined = texts.join("\n");
    let value =
        serde_json::from_str::<Value>(&joined).unwrap_or_else(|_| json!({ "text": joined }));
    (value, dropped)
}

/// The text of every text block, in order, and the count of the blocks
/// that are not text.
fn text_blocks(content: &[Value]) -> (Vec<&str>, u32) {
    let mut texts = Vec::new();
    let mut dropped = 0u32;
    for block in content {
        match (
            block.get("type").and_then(Value::as_str),
            block.get("text").and_then(Value::as_str),
        ) {
            (Some("text"), Some(text)) => texts.push(text),
            _ => dropped = dropped.saturating_add(1),
        }
    }
    (texts, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::effect::{CallClass, CallFacts, classify};
    use apb_core::secret::Secret;
    use apb_mcp_client::{CallFailure, Ended, Failure, FailureKind};

    const SECRET: &str = "map-secret-9911";

    fn failure(kind: FailureKind, message: &str) -> Failure {
        Failure {
            kind,
            message: message.to_string(),
            ended: Ended::NotStarted,
        }
    }

    fn rejected(code: i64) -> Result<ToolAnswer, CallFailure> {
        Err(CallFailure::Answered(failure(
            FailureKind::Rejected { code },
            "bad",
        )))
    }

    fn complete(is_error: bool) -> Result<ToolAnswer, CallFailure> {
        Ok(ToolAnswer::Complete {
            content: vec![json!({ "type": "text", "text": "done" })],
            structured: None,
            is_error,
        })
    }

    /// The outcome string and the write class of one answer.
    fn row(answer: Result<ToolAnswer, CallFailure>, read_only: bool) -> (String, CallClass) {
        let outcome = match super::answer(answer, read_only, &[], &[]) {
            Ok(_) => "ok".to_string(),
            Err(e) => e.code.as_str().to_string(),
        };
        let class = classify(
            CallFacts {
                outcome: &outcome,
                http_status: None,
                response_complete: (outcome == "ok").then_some(true),
            },
            read_only,
        );
        (outcome, class)
    }

    #[test]
    fn every_answer_maps_as_the_table_says() {
        // Writes.
        assert_eq!(
            row(complete(false), false),
            ("ok".into(), CallClass::WriteOk)
        );
        assert_eq!(
            row(complete(true), false),
            ("tool_error".into(), CallClass::WriteUnknown)
        );
        assert_eq!(
            row(Ok(ToolAnswer::Task), false),
            ("tool_error".into(), CallClass::WriteUnknown)
        );
        assert_eq!(
            row(Ok(ToolAnswer::InputRequired), false),
            ("tool_error".into(), CallClass::WriteUnknown)
        );
        for code in [-32700, -32600, -32601, -32602, -32603, -32000, -32099] {
            assert_eq!(
                row(rejected(code), false),
                ("tool_error".into(), CallClass::WriteUnknown),
                "write, code {code}"
            );
        }
        let lost = |kind| Err(CallFailure::Lost(failure(kind, "gone")));
        assert_eq!(
            row(lost(FailureKind::Timeout), false),
            ("timeout".into(), CallClass::WriteUnknown)
        );
        assert_eq!(
            row(lost(FailureKind::Transport), false),
            ("network".into(), CallClass::WriteUnknown)
        );
        assert_eq!(
            row(lost(FailureKind::TooLarge), false),
            ("network".into(), CallClass::WriteUnknown)
        );
        // Reads.
        assert_eq!(row(complete(false), true), ("ok".into(), CallClass::ReadOk));
        assert_eq!(
            row(complete(true), true),
            ("tool_error".into(), CallClass::Failed)
        );
        assert_eq!(
            row(Ok(ToolAnswer::Task), true),
            ("tool_error".into(), CallClass::Failed)
        );
        assert_eq!(
            row(Ok(ToolAnswer::InputRequired), true),
            ("tool_error".into(), CallClass::Failed)
        );
        for (code, outcome) in [
            (-32700, "service"),
            (-32600, "service"),
            (-32601, "config"),
            (-32602, "invalid_args"),
            (-32603, "tool_error"),
            (-32000, "tool_error"),
            (-32099, "tool_error"),
        ] {
            assert_eq!(
                row(rejected(code), true),
                (outcome.to_string(), CallClass::Failed),
                "read, code {code}"
            );
        }
        assert_eq!(
            row(lost(FailureKind::Timeout), true),
            ("timeout".into(), CallClass::Failed)
        );
        assert_eq!(
            row(lost(FailureKind::Transport), true),
            ("network".into(), CallClass::Failed)
        );
        // The messages say what is not known.
        let task = super::answer(Ok(ToolAnswer::Task), false, &[], &[]).unwrap_err();
        assert!(
            task.message.contains("task apb does not follow"),
            "{}",
            task.message
        );
        let input = super::answer(Ok(ToolAnswer::InputRequired), false, &[], &[]).unwrap_err();
        assert!(
            input.message.contains("multi round-trip"),
            "{}",
            input.message
        );
        let rpc = super::answer(rejected(-32602), false, &[], &[]).unwrap_err();
        assert!(
            rpc.message.contains("JSON-RPC error -32602"),
            "{}",
            rpc.message
        );
        assert!(rpc.message.contains("is not known"), "{}", rpc.message);
    }

    #[test]
    fn a_call_that_never_left_is_a_failure_before_the_hand_over() {
        let not_sent = |read_only| {
            row(
                Err(CallFailure::NotSent(failure(
                    FailureKind::Transport,
                    "the MCP session is closed",
                ))),
                read_only,
            )
        };
        assert_eq!(not_sent(false), ("service".into(), CallClass::Failed));
        assert_eq!(not_sent(true), ("service".into(), CallClass::Failed));
    }

    #[test]
    fn a_secret_across_the_cut_is_redacted_whole_before_the_cut() {
        let redactions = vec![Redaction::new(Secret::new(SECRET), "token")];
        // The secret starts at character 495 of the composed message (the
        // fixed words in front of the server's text count), so a cut at
        // 500 before the redaction would leave its first five characters.
        let prefix =
            "the MCP tool reported an error; whether it acted before it failed is not known: ";
        let filler = super::super::redact::SERVER_TEXT_CHARS - 5 - prefix.chars().count();
        let text = format!("{}{SECRET} and more", "x".repeat(filler));
        let e = super::answer(
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": text })],
                structured: None,
                is_error: true,
            }),
            false,
            &[],
            &redactions,
        )
        .unwrap_err();
        assert!(!e.message.contains(&SECRET[..5]), "{}", e.message);
        assert!(e.message.contains("[cut: "), "{}", e.message);
    }

    /// The review's case: the account's secret is `123456` and the server
    /// answers with it as the JSON-RPC error code. The code still
    /// classifies the answer, and no message carries it: every message is
    /// redacted whole after it is composed.
    #[test]
    fn a_secret_reflected_as_a_number_is_redacted_in_every_message() {
        const NUMERIC: &str = "123456";
        let redactions = vec![Redaction::new(Secret::new(NUMERIC), "token")];
        let with = |answer: Result<ToolAnswer, CallFailure>, read_only: bool| {
            super::answer(answer, read_only, &[], &redactions).unwrap_err()
        };
        let rejected_by = |code: i64, message: &str| {
            Err(CallFailure::Answered(failure(
                FailureKind::Rejected { code },
                message,
            )))
        };
        let e = with(rejected_by(123456, "failed"), false);
        assert_eq!(e.code, CallErrorCode::ToolError);
        assert!(!e.message.contains(NUMERIC), "{}", e.message);
        assert!(e.message.contains("[redacted:token]"), "{}", e.message);
        // A read: the code still classifies (-32602 is invalid_args).
        let e = with(rejected_by(-32602, "bad 123456"), true);
        assert_eq!(e.code, CallErrorCode::InvalidArgs);
        assert!(!e.message.contains(NUMERIC), "{}", e.message);
        // Every other composed message: the tool's error text, a lost
        // answer, a call not sent, a connect failure.
        let text = |t: &str| {
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": t })],
                structured: None,
                is_error: true,
            })
        };
        for e in [
            with(text("code 123456"), false),
            with(
                Err(CallFailure::Lost(failure(
                    FailureKind::Timeout,
                    "id 123456",
                ))),
                false,
            ),
            with(
                Err(CallFailure::NotSent(failure(
                    FailureKind::Transport,
                    "id 123456",
                ))),
                false,
            ),
            connect_error(
                failure(
                    FailureKind::Rejected { code: 123456 },
                    "Mcp error: 123456: x",
                ),
                &redactions,
            ),
        ] {
            assert!(!e.message.contains(NUMERIC), "{}", e.message);
        }
    }

    #[test]
    fn server_text_has_a_bound_everywhere() {
        let long = "y".repeat(100_000);
        let rpc = super::answer(
            Err(CallFailure::Answered(failure(
                FailureKind::Rejected { code: -32000 },
                &long,
            ))),
            false,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(rpc.message.chars().count() < 1_000, "{}", rpc.message.len());
        let lost = super::answer(
            Err(CallFailure::Lost(failure(FailureKind::Transport, &long))),
            false,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(lost.message.chars().count() < 1_000);
        let connect = connect_error(failure(FailureKind::Transport, &long), &[]);
        assert!(connect.message.chars().count() < 1_000);
        let tool = super::answer(
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": long })],
                structured: None,
                is_error: true,
            }),
            false,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(tool.message.chars().count() < 1_000);
    }

    #[test]
    fn an_error_text_that_reads_as_json_is_kept_as_text() {
        let e = super::answer(
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": "{\"code\":7,\"why\":\"quota\"}" })],
                structured: None,
                is_error: true,
            }),
            false,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(
            e.message.ends_with("{\"code\":7,\"why\":\"quota\"}"),
            "{}",
            e.message
        );
        let number = super::answer(
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": "42" })],
                structured: None,
                is_error: true,
            }),
            false,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(number.message.ends_with(": 42"), "{}", number.message);
    }

    #[test]
    fn a_secret_as_an_answer_key_or_number_is_redacted() {
        const PIN: &str = "4815162342";
        let redactions = vec![
            Redaction::new(Secret::new(SECRET), "token"),
            Redaction::new(Secret::new(PIN), "pin"),
        ];
        let ok = super::answer(
            Ok(ToolAnswer::Complete {
                content: Vec::new(),
                structured: Some(json!({
                    SECRET: "a value under a secret key",
                    "account": 4815162342u64,
                    "nested": [{ "pin": 4815162342u64, "fine": 7 }]
                })),
                is_error: false,
            }),
            true,
            &[],
            &redactions,
        )
        .unwrap();
        let text = serde_json::to_string(&ok.to_success_json()).unwrap();
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains(PIN), "{text}");
        assert!(
            text.contains("\"fine\":7"),
            "numbers without a secret stay: {text}"
        );
        // A text answer that parses as a number holding the secret.
        let ok = super::answer(
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": PIN })],
                structured: None,
                is_error: false,
            }),
            true,
            &[],
            &redactions,
        )
        .unwrap();
        let text = serde_json::to_string(&ok.to_success_json()).unwrap();
        assert!(!text.contains(PIN), "{text}");
    }

    #[test]
    fn a_connect_failure_is_pre_dispatch() {
        let code = |kind| connect_error(failure(kind, "why"), &[]).code.as_str();
        assert_eq!(code(FailureKind::Spawn), "network");
        assert_eq!(code(FailureKind::Transport), "network");
        assert_eq!(code(FailureKind::Timeout), "timeout");
        assert_eq!(code(FailureKind::Unsupported), "config");
        assert_eq!(code(FailureKind::TooLarge), "service");
        assert_eq!(code(FailureKind::Rejected { code: -32601 }), "service");
        let reach = connect_error(failure(FailureKind::Spawn, "no such file"), &[]);
        assert!(
            reach
                .message
                .starts_with("the MCP server could not be reached: "),
            "{}",
            reach.message
        );
        let big = connect_error(
            failure(
                FailureKind::TooLarge,
                "mcp_message_too_large: the tool list is over its limit",
            ),
            &[],
        );
        assert!(
            big.message.starts_with("mcp_message_too_large:"),
            "{}",
            big.message
        );
    }

    #[test]
    fn a_refused_spawn_is_permission() {
        let e = connect_error(
            failure(
                FailureKind::Refused,
                "read_only: starting a program is refused",
            ),
            &[],
        );
        assert_eq!(e.code.as_str(), "permission");
        assert_eq!(e.message, "read_only: starting a program is refused");
    }

    #[test]
    fn messages_are_redacted() {
        let redactions = vec![Redaction::new(Secret::new(SECRET), "token")];
        let e = connect_error(
            failure(FailureKind::Transport, &format!("broke near {SECRET}")),
            &redactions,
        );
        assert!(!e.message.contains(SECRET), "{}", e.message);
        let e = super::answer(
            Err(CallFailure::Answered(failure(
                FailureKind::Rejected { code: -32603 },
                &format!("internal {SECRET}"),
            ))),
            false,
            &[],
            &redactions,
        )
        .unwrap_err();
        assert!(!e.message.contains(SECRET), "{}", e.message);
        let e = super::answer(
            Ok(ToolAnswer::Complete {
                content: vec![json!({ "type": "text", "text": format!("failed with {SECRET}") })],
                structured: None,
                is_error: true,
            }),
            true,
            &[],
            &redactions,
        )
        .unwrap_err();
        assert!(!e.message.contains(SECRET), "{}", e.message);
        let e = super::answer(
            Err(CallFailure::Lost(failure(
                FailureKind::Transport,
                &format!("lost {SECRET}"),
            ))),
            false,
            &[],
            &redactions,
        )
        .unwrap_err();
        assert!(!e.message.contains(SECRET), "{}", e.message);
    }

    #[test]
    fn the_body_rules_hold() {
        // 1. structuredContent, an explicit null included.
        let (b, d) = body(
            &[json!({ "type": "text", "text": "x" })],
            Some(json!({ "a": 1 })),
        );
        assert_eq!((b, d), (json!({ "a": 1 }), 0));
        assert_eq!(body(&[], Some(Value::Null)).0, Value::Null);
        // 2. Text blocks joined, parsed as JSON when they parse.
        let parsed = body(
            &[
                json!({ "type": "text", "text": "{\"a\":" }),
                json!({ "type": "text", "text": "2}" }),
            ],
            None,
        );
        assert_eq!(parsed.0, json!({ "a": 2 }));
        let plain = body(
            &[
                json!({ "type": "text", "text": "one" }),
                json!({ "type": "text", "text": "two" }),
            ],
            None,
        );
        assert_eq!(plain.0, json!({ "text": "one\ntwo" }));
        assert_eq!(body(&[], None).0, json!({ "text": "" }));
        // 3. Other blocks are counted, not returned.
        let mixed = body(
            &[
                json!({ "type": "image", "data": "AAAA", "mimeType": "image/png" }),
                json!({ "type": "text", "text": "kept" }),
                json!({ "type": "resource_link", "uri": "file:///x" }),
            ],
            None,
        );
        assert_eq!(mixed, (json!({ "text": "kept" }), 2));
        // 4. Redactions, then the projection.
        let redactions = vec![Redaction::new(Secret::new(SECRET), "token")];
        let ok = super::answer(
            Ok(ToolAnswer::Complete {
                content: Vec::new(),
                structured: Some(json!({ "items": [{ "id": 1, "note": SECRET }] })),
                is_error: false,
            }),
            true,
            &["items.id".to_string()],
            &redactions,
        )
        .unwrap();
        let shown = ok.to_success_json().to_string();
        assert!(!shown.contains(SECRET), "{shown}");
        assert_eq!(ok.to_success_json()["picked"], json!(true), "{shown}");
    }
}
