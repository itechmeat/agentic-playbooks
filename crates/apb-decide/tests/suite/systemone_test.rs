//! The `/v1/systemone` adapter against a local stub server: wire format,
//! error mapping, retries and the key never leaving the header.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use apb_decide::testing::{StubResponse, StubServer};
use apb_decide::{
    Answer, ApiKey, ChoiceCriteria, DecideError, DecisionProvider, DecisionRequest, Question,
    SystemOne, UseSite,
};
use serde_json::json;

const SENTINEL_KEY: &str = "sk-sentinel-key-4f1c9a7e2b";

fn questions() -> BTreeMap<String, Question> {
    BTreeMap::from([
        (
            "done".to_string(),
            Question::Noul {
                instructions: json!("Is it done?"),
                criteria: None,
            },
        ),
        (
            "kind".to_string(),
            Question::Choice {
                instructions: json!("Which?"),
                criteria: ChoiceCriteria::new()
                    .with("b", None)
                    .with("a", Some(json!("first"))),
            },
        ),
        (
            "size".to_string(),
            Question::Score {
                instructions: json!("How big?"),
                levels: vec![json!("small"), json!("medium"), json!("large")],
            },
        ),
    ])
}

fn request() -> DecisionRequest {
    DecisionRequest {
        use_site: UseSite::CompletionCheck,
        state_order: vec!["task".into(), "result".into()],
        state: json!({"task": "t", "result": "r"}),
        questions: questions(),
    }
}

fn provider(server: &StubServer, key: Option<&str>) -> SystemOne {
    SystemOne::new(
        "stub",
        &server.base_url,
        "jev-test",
        key.map(ApiKey::new),
        Duration::from_secs(3),
    )
}

const OK_REPLY: &str = r#"{"model":"jev-test-answered","answers":{
  "done":{"type":"noul","noul":0.9},
  "kind":{"type":"choice","choice":"a","probabilities":{"a":0.8,"b":0.2},"confidence":0.6},
  "size":{"type":"score","score":1.2,"probabilities":{"0":0.1,"1":0.6,"2":0.3}}},
  "usage":{"input_tokens":120,"output_tokens":3}}"#;

#[test]
fn answers_all_three_types_and_sends_the_wire_shape() {
    let server = StubServer::start(vec![StubResponse::json(200, OK_REPLY)]);
    let r = provider(&server, Some(SENTINEL_KEY))
        .decide(&request())
        .unwrap();
    assert_eq!(r.provider, "stub");
    assert_eq!(r.model, "jev-test-answered");
    assert_eq!(r.answers["done"], Answer::Noul { p: 0.9 });
    assert!(
        matches!(&r.answers["kind"], Answer::Choice { value, confidence, .. } if value == "a" && *confidence == 0.6)
    );
    // No confidence in the reply: recomputed from the peak, (3 * 0.6 - 1) / 2.
    assert!(
        matches!(&r.answers["size"], Answer::Score { value, confidence, .. } if *value == 1.2 && (*confidence - 0.4).abs() < 1e-9)
    );
    assert_eq!(r.usage.input_tokens, Some(120));
    assert_eq!(r.usage.cost_usd, None);

    let raw = &server.requests()[0];
    assert!(raw.starts_with("POST /v1/systemone "), "{raw}");
    assert!(
        raw.to_ascii_lowercase()
            .contains(&format!("authorization: bearer {SENTINEL_KEY}"))
    );
    let body: serde_json::Value =
        serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], "jev-test");
    assert_eq!(
        body["questions"]["size"]["criteria"],
        json!(["small", "medium", "large"])
    );
    assert_eq!(
        body["questions"]["kind"]["criteria"],
        json!({"a": "first", "b": null})
    );
    // Option order and the state's key order are kept as given, not sorted.
    assert!(
        raw.contains(r#""state":{"task":"t","result":"r"}"#),
        "{raw}"
    );
    let raw_body = raw.split("\r\n\r\n").nth(1).unwrap();
    assert!(raw_body.find(r#""b":null"#).unwrap() < raw_body.find(r#""a":"first""#).unwrap());
    assert_eq!(
        body["questions"]["done"],
        json!({"type": "noul", "instructions": "Is it done?"})
    );
}

#[test]
fn no_key_means_no_authorization_header() {
    let server = StubServer::start(vec![StubResponse::json(200, OK_REPLY)]);
    provider(&server, None).decide(&request()).unwrap();
    assert!(
        !server.requests()[0]
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}

#[test]
fn a_reported_cost_is_kept() {
    let reply = OK_REPLY.replace(
        r#""output_tokens":3"#,
        r#""output_tokens":3,"cost":0.00005"#,
    );
    let server = StubServer::start(vec![StubResponse::json(200, reply)]);
    let r = provider(&server, None).decide(&request()).unwrap();
    assert_eq!(r.usage.cost_usd, Some(0.00005));
}

#[test]
fn a_partial_batch_keeps_the_good_items() {
    let reply = r#"{"model":"m","answers":{"done":{"type":"noul","noul":0.4},"kind":{"type":"choice","choice":"b","probabilities":{"a":0.8,"b":0.2}}}}"#;
    let server = StubServer::start(vec![StubResponse::json(200, reply)]);
    let r = provider(&server, None).decide(&request()).unwrap();
    assert_eq!(r.answers["done"], Answer::Noul { p: 0.4 });
    assert!(matches!(r.answers["kind"], Answer::Invalid { .. }));
    assert!(matches!(r.answers["size"], Answer::Invalid { .. }));
}

#[test]
fn status_codes_map_to_errors_without_retrying_the_final_ones() {
    let cases = [
        (401, DecideError::Auth),
        (403, DecideError::Auth),
        (402, DecideError::Budget),
        (404, DecideError::Unavailable("http 404".into())),
    ];
    for (status, expected) in cases {
        let server = StubServer::start(vec![StubResponse::json(status, "{}")]);
        assert_eq!(
            provider(&server, None).decide(&request()).unwrap_err(),
            expected
        );
        assert_eq!(server.count(), 1, "status {status} must not be retried");
    }
    let server = StubServer::start(vec![StubResponse::json(200, "not json")]);
    assert!(matches!(
        provider(&server, None).decide(&request()),
        Err(DecideError::Unavailable(_))
    ));
}

#[test]
fn a_422_detail_is_truncated_and_never_echoes_the_key() {
    let echo = format!(
        r#"{{"detail":"bad request with Bearer {SENTINEL_KEY} {}"}}"#,
        "x".repeat(500)
    );
    let server = StubServer::start(vec![StubResponse::json(422, echo)]);
    let err = provider(&server, Some(SENTINEL_KEY))
        .decide(&request())
        .unwrap_err();
    let DecideError::Invalid(detail) = &err else {
        panic!("{err:?}")
    };
    assert!(detail.chars().count() <= 200);
    for text in [detail.clone(), err.to_string(), format!("{err:?}")] {
        assert!(!text.contains(SENTINEL_KEY), "{text}");
    }
}

#[test]
fn retryable_statuses_are_retried_at_most_twice() {
    for status in [408, 409, 429, 500, 503, 529] {
        let server = StubServer::start(vec![
            StubResponse::json(status, "{}"),
            StubResponse::json(status, "{}"),
            StubResponse::json(200, OK_REPLY),
        ]);
        assert!(
            provider(&server, None).decide(&request()).is_ok(),
            "status {status}"
        );
        assert_eq!(server.count(), 3);
    }
    let server = StubServer::start_with_fallback(vec![], StubResponse::json(503, "{}"));
    assert_eq!(
        provider(&server, None).decide(&request()).unwrap_err(),
        DecideError::Unavailable("http 503".into())
    );
    assert_eq!(server.count(), 3, "one attempt and two retries");
}

#[test]
fn retry_after_is_honoured_only_within_the_timeout() {
    let server = StubServer::start(vec![
        StubResponse::json(429, "{}").header("retry-after", "1"),
        StubResponse::json(200, OK_REPLY),
    ]);
    let started = Instant::now();
    assert!(provider(&server, None).decide(&request()).is_ok());
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the retry waited for retry-after"
    );

    // A wait longer than the remaining timeout is not taken.
    let server = StubServer::start(vec![
        StubResponse::json(429, "{}").header("retry-after", "30"),
    ]);
    let err = provider(&server, None).decide(&request()).unwrap_err();
    assert_eq!(
        err,
        DecideError::RateLimited {
            retry_after: Some(Duration::from_secs(30))
        }
    );
    assert_eq!(server.count(), 1);

    // A value too large for the clock is an ordinary rate limit, bounded,
    // never a panic.
    let server = StubServer::start(vec![
        StubResponse::json(429, "{}").header("retry-after", "18446744073709551615"),
    ]);
    let err = provider(&server, None).decide(&request()).unwrap_err();
    assert_eq!(
        err,
        DecideError::RateLimited {
            retry_after: Some(Duration::from_secs(3600))
        }
    );
}

#[test]
fn a_slow_server_times_out_once_and_is_not_asked_again() {
    let server = StubServer::start(vec![
        StubResponse::json(200, OK_REPLY).delayed(Duration::from_millis(1500)),
    ]);
    let p = SystemOne::new(
        "stub",
        &server.base_url,
        "m",
        None,
        Duration::from_millis(300),
    );
    assert_eq!(p.decide(&request()).unwrap_err(), DecideError::Timeout);
    assert_eq!(
        server.count(),
        1,
        "a read timeout after the body was sent is never retried"
    );
}

#[test]
fn a_limit_violation_sends_nothing() {
    let server = StubServer::start(vec![StubResponse::json(200, OK_REPLY)]);
    let mut req = request();
    req.state = json!("x".repeat(200_000));
    assert!(matches!(
        provider(&server, None).decide(&req),
        Err(DecideError::Invalid(_))
    ));
    req.state = json!("s");
    req.questions.clear();
    assert!(matches!(
        provider(&server, None).decide(&req),
        Err(DecideError::Invalid(_))
    ));
    assert_eq!(server.count(), 0);
}

#[test]
fn the_key_never_appears_in_debug_output() {
    let server = StubServer::start(vec![]);
    let p = provider(&server, Some(SENTINEL_KEY));
    let key = ApiKey::new(SENTINEL_KEY);
    for text in [
        format!("{p:?}"),
        format!("{key:?}"),
        format!("{key}"),
        format!("{:?}", request()),
    ] {
        assert!(!text.contains(SENTINEL_KEY), "{text}");
    }
    let err = p.decide(&request()).unwrap_err();
    assert!(!format!("{err:?} {err}").contains(SENTINEL_KEY));
}
