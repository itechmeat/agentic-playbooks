//! The OpenAI-compatible LLM-emulation backend against a local stub server:
//! the request it sends, the uncalibrated response it builds, error mapping,
//! and the key never leaving the header.

use std::collections::BTreeMap;
use std::time::Duration;

use apb_decide::testing::{StubResponse, StubServer};
use apb_decide::{
    Answer, ApiKey, ChoiceCriteria, DecisionProvider, DecisionRequest, LlmEmulation, Question,
    StructuredOutput, UseSite,
};
use serde_json::json;

const SENTINEL_KEY: &str = "sk-sentinel-emulation-key-8a2d";

fn request() -> DecisionRequest {
    DecisionRequest {
        use_site: UseSite::JudgeNode,
        state_order: vec!["review".into()],
        state: json!({"review": "One failing test in parser.rs."}),
        questions: BTreeMap::from([
            (
                "risky".to_string(),
                Question::Noul {
                    instructions: json!("Does `review` mention deleted tests?"),
                    criteria: None,
                },
            ),
            (
                "verdict".to_string(),
                Question::Choice {
                    instructions: json!("Which outcome does `review` report?"),
                    criteria: ChoiceCriteria::new()
                        .with("clean", None)
                        .with("needs_fix", None)
                        .with("unclear", None),
                },
            ),
        ]),
    }
}

fn provider(server: &StubServer, mode: StructuredOutput) -> LlmEmulation {
    LlmEmulation::new(
        "emulated",
        format!("{}/v1", server.base_url),
        "small-model",
        Some(ApiKey::new(SENTINEL_KEY)),
        Duration::from_secs(3),
        mode,
    )
}

fn completion(content: &str) -> String {
    json!({
        "model": "small-model-2026",
        "choices": [{"message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 310, "completion_tokens": 22},
    })
    .to_string()
}

#[test]
fn an_answer_comes_back_uncalibrated_and_normalised() {
    let server = StubServer::start(vec![StubResponse::json(
        200,
        completion(r#"{"q1": 0.2, "q2": {"clean": 0.1, "needs_fix": 0.8, "unclear": 0.3}}"#),
    )]);
    let p = provider(&server, StructuredOutput::JsonSchema);
    let resp = p.decide(&request()).unwrap();
    assert!(!resp.calibrated);
    assert_eq!(resp.provider, "emulated");
    assert_eq!(resp.model, "small-model-2026");
    assert_eq!(resp.usage.input_tokens, Some(310));
    assert_eq!(resp.answers["risky"], Answer::Noul { p: 0.2 });
    let Answer::Choice {
        value,
        probabilities,
        ..
    } = &resp.answers["verdict"]
    else {
        panic!("{:?}", resp.answers)
    };
    assert_eq!(value, "needs_fix");
    assert!((probabilities.values().sum::<f64>() - 1.0).abs() < 1e-9);

    let raw = &server.requests()[0];
    assert!(raw.starts_with("POST /v1/chat/completions"));
    assert!(raw.contains(&format!("authorization: Bearer {SENTINEL_KEY}")));
    assert!(raw.contains(r#""response_format":{"json_schema""#));
    assert!(
        raw.contains(r#""strict":true"#),
        "structured output is strict"
    );
    assert!(raw.contains("<document>"));
}

#[test]
fn prompt_only_parses_the_first_object_of_a_chatty_reply() {
    let server = StubServer::start(vec![StubResponse::json(
        200,
        completion(
            "Sure. {\"q1\": 0.9, \"q2\": {\"clean\": 1, \"needs_fix\": 0, \"unclear\": 0}} Hope that helps {\"q1\": 0}",
        ),
    )]);
    let resp = provider(&server, StructuredOutput::PromptOnly)
        .decide(&request())
        .unwrap();
    assert_eq!(resp.answers["risky"], Answer::Noul { p: 0.9 });
    assert!(!server.requests()[0].contains("response_format"));
}

#[test]
fn malformed_replies_and_refusals_map_to_decision_errors() {
    for (status, body, want) in [
        (200, "not json".to_string(), "unavailable"),
        (200, completion("I cannot answer that."), "unavailable"),
        (401, "{}".to_string(), "auth"),
        (402, "{}".to_string(), "budget"),
        (422, format!("bad request for {SENTINEL_KEY}"), "invalid"),
    ] {
        let server = StubServer::start(vec![StubResponse::json(status, body)]);
        let err = provider(&server, StructuredOutput::JsonSchema)
            .decide(&request())
            .unwrap_err();
        assert_eq!(err.kind(), want, "{status}");
        assert!(!format!("{err} {err:?}").contains(SENTINEL_KEY));
    }
    // An item out of range is an invalid answer, the rest of the batch stands.
    let server = StubServer::start(vec![StubResponse::json(
        200,
        completion(r#"{"q1": 7, "q2": {"clean": 1, "needs_fix": 0, "unclear": 0}}"#),
    )]);
    let resp = provider(&server, StructuredOutput::JsonSchema)
        .decide(&request())
        .unwrap();
    assert!(matches!(resp.answers["risky"], Answer::Invalid { .. }));
    assert!(matches!(resp.answers["verdict"], Answer::Choice { .. }));
}

#[test]
fn the_key_is_never_in_debug_output() {
    let server = StubServer::start(vec![]);
    let p = provider(&server, StructuredOutput::JsonSchema);
    assert!(!format!("{p:?}").contains(SENTINEL_KEY));
}
