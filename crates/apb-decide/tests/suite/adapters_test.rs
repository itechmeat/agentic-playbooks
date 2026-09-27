//! The Part 15 adapters (`vercel_evaluate`, `openrouter_decisions`,
//! `cloudflare`) against a local stub server: each route's request shape,
//! its reply mapping, the shared error and retry contract, and the key never
//! leaving the header.

use std::collections::BTreeMap;
use std::time::Duration;

use apb_decide::testing::{StubResponse, StubServer};
use apb_decide::{
    Answer, ApiKey, ChoiceCriteria, Cloudflare, DecideError, DecisionProvider, DecisionRequest,
    OpenRouterDecisions, Question, UseSite, VercelEvaluate, is_model_alias,
};
use serde_json::{Value, json};

const SENTINEL_KEY: &str = "sk-sentinel-adapter-key-8d21c0";
const TIMEOUT: Duration = Duration::from_secs(3);

fn request() -> DecisionRequest {
    DecisionRequest {
        use_site: UseSite::CatalogRank,
        state_order: vec!["task".into()],
        state: json!({"task": "t", "extra": "x"}),
        questions: BTreeMap::from([
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
                    criteria: ChoiceCriteria::new().with("b", None).with("a", None),
                },
            ),
            (
                "size".to_string(),
                Question::Score {
                    instructions: json!("How big?"),
                    levels: vec![json!("small"), json!("medium"), json!("large")],
                },
            ),
        ]),
    }
}

/// One adapter under test: how to build it against a stub, the path it
/// posts to, and replies in its own dialect.
struct Adapter {
    name: &'static str,
    path: String,
    build: fn(&StubServer, Option<&str>) -> Box<dyn DecisionProvider>,
    /// A full reply: noul 0.9, choice `a` with confidence 0.6, score without
    /// confidence, cost `cost` when given.
    ok: fn(Option<f64>) -> String,
    /// A reply answering only `done`.
    partial: String,
}

const CF_ACCOUNT: &str = "0123abcdef";

fn vercel(server: &StubServer, key: Option<&str>) -> Box<dyn DecisionProvider> {
    Box::new(VercelEvaluate::new(
        "vercel",
        &server.base_url,
        "typesafe-ai/jev",
        key.map(ApiKey::new),
        TIMEOUT,
    ))
}

fn openrouter(server: &StubServer, key: Option<&str>) -> Box<dyn DecisionProvider> {
    Box::new(OpenRouterDecisions::new(
        "openrouter",
        &server.base_url,
        "typesafe/jev-1.13",
        key.map(ApiKey::new),
        TIMEOUT,
    ))
}

fn cloudflare(server: &StubServer, key: Option<&str>) -> Box<dyn DecisionProvider> {
    Box::new(Cloudflare::new(
        "cf",
        &server.base_url,
        CF_ACCOUNT,
        "typesafe/jev",
        key.map(ApiKey::new),
        TIMEOUT,
    ))
}

const CHOICE_AND_SCORE: &str = r#""kind":{"type":"choice","choice":"a","probabilities":{"a":0.8,"b":0.2},"confidence":0.6},
  "size":{"type":"score","score":1.2,"probabilities":{"0":0.1,"1":0.6,"2":0.3}}"#;

fn vercel_ok(cost: Option<f64>) -> String {
    let meta = cost
        .map(|c| format!(r#","providerMetadata":{{"gateway":{{"cost":"{c}"}}}}"#))
        .unwrap_or_default();
    format!(
        r#"{{"model":"typesafe-ai/jev","answers":{{"done":{{"type":"boolean","probability":0.9}},{CHOICE_AND_SCORE}}},
  "usage":{{"inputTokens":120,"outputTokens":3}}{meta}}}"#
    )
}

fn canonical_ok(model: &str, cost: Option<f64>) -> String {
    let cost = cost.map(|c| format!(r#","cost":{c}"#)).unwrap_or_default();
    format!(
        r#"{{"model":"{model}","answers":{{"done":{{"type":"noul","noul":0.9}},{CHOICE_AND_SCORE}}},
  "usage":{{"input_tokens":120,"output_tokens":3{cost}}}}}"#
    )
}

fn adapters() -> Vec<Adapter> {
    vec![
        Adapter {
            name: "vercel_evaluate",
            path: "/v1/evaluate".into(),
            build: vercel,
            ok: vercel_ok,
            partial: r#"{"model":"typesafe-ai/jev","answers":{"done":{"type":"boolean","probability":0.4}},"usage":{"inputTokens":1,"outputTokens":0}}"#.into(),
        },
        Adapter {
            name: "openrouter_decisions",
            path: "/api/alpha/decisions".into(),
            build: openrouter,
            ok: |cost| canonical_ok("typesafe/jev-1.13-20260917", cost),
            partial: r#"{"model":"typesafe/jev-1.13","answers":{"done":{"type":"noul","noul":0.4}},"usage":{"input_tokens":1,"output_tokens":0}}"#.into(),
        },
        Adapter {
            name: "cloudflare",
            path: format!("/accounts/{CF_ACCOUNT}/ai/run"),
            build: cloudflare,
            ok: |cost| {
                format!(
                    r#"{{"result":{},"success":true,"errors":[],"messages":[]}}"#,
                    canonical_ok("jev-1.13.0", cost)
                )
            },
            partial: r#"{"model":"jev-1.13.0","answers":{"done":{"type":"noul","noul":0.4}},"usage":{"input_tokens":1,"output_tokens":0}}"#.into(),
        },
    ]
}

/// The body of a captured raw request.
fn body_of(raw: &str) -> Value {
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(Value::Null)
}

#[test]
fn every_adapter_answers_all_three_types_on_its_own_path() {
    for a in adapters() {
        let server = StubServer::start(vec![StubResponse::json(200, (a.ok)(None))]);
        let r = (a.build)(&server, Some(SENTINEL_KEY))
            .decide(&request())
            .unwrap_or_else(|e| panic!("{}: {e}", a.name));
        assert_eq!(r.answers["done"], Answer::Noul { p: 0.9 }, "{}", a.name);
        assert!(
            matches!(&r.answers["kind"], Answer::Choice { value, confidence, .. } if value == "a" && *confidence == 0.6),
            "{}",
            a.name
        );
        // No confidence in the reply: recomputed from the distribution,
        // (3 * 0.6 - 1) / 2 = 0.4.
        assert!(
            matches!(&r.answers["size"], Answer::Score { confidence, .. } if (confidence - 0.4).abs() < 1e-9),
            "{}",
            a.name
        );
        assert_eq!(r.usage.input_tokens, Some(120), "{}", a.name);
        assert_eq!(r.usage.output_tokens, Some(3), "{}", a.name);
        assert_eq!(r.usage.cost_usd, None, "{}: no cost reported", a.name);
        assert!(r.calibrated, "{}", a.name);
        let raw = &server.requests()[0];
        assert!(
            raw.starts_with(&format!("POST {} ", a.path)),
            "{}: {raw}",
            a.name
        );
        assert!(
            raw.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                SENTINEL_KEY.to_ascii_lowercase()
            )),
            "{}",
            a.name
        );
    }
}

#[test]
fn a_reported_cost_is_kept_by_the_routes_that_report_one() {
    for a in adapters().into_iter().filter(|a| a.name != "cloudflare") {
        let server = StubServer::start(vec![StubResponse::json(200, (a.ok)(Some(0.00005)))]);
        let r = (a.build)(&server, None).decide(&request()).unwrap();
        assert_eq!(r.usage.cost_usd, Some(0.00005), "{}", a.name);
    }
}

#[test]
fn a_partial_batch_keeps_the_good_items() {
    for a in adapters() {
        let server = StubServer::start(vec![StubResponse::json(200, a.partial.clone())]);
        let r = (a.build)(&server, None).decide(&request()).unwrap();
        assert_eq!(r.answers["done"], Answer::Noul { p: 0.4 }, "{}", a.name);
        assert!(
            matches!(r.answers["kind"], Answer::Invalid { .. }),
            "{}",
            a.name
        );
        assert!(
            matches!(r.answers["size"], Answer::Invalid { .. }),
            "{}",
            a.name
        );
    }
}

#[test]
fn statuses_map_onto_the_shared_errors() {
    for a in adapters() {
        for (status, expected) in [
            (401, DecideError::Auth),
            (403, DecideError::Auth),
            (402, DecideError::Budget),
            (404, DecideError::Unavailable("http 404".into())),
        ] {
            let server = StubServer::start(vec![StubResponse::json(status, "{}")]);
            assert_eq!(
                (a.build)(&server, None).decide(&request()).unwrap_err(),
                expected,
                "{} {status}",
                a.name
            );
            assert_eq!(server.count(), 1, "{} {status} is final", a.name);
        }
        for status in [400, 422] {
            let server = StubServer::start(vec![StubResponse::json(
                status,
                r#"{"message":"questions.done.type: bad","error_type":"invalid_request"}"#,
            )]);
            let err = (a.build)(&server, None).decide(&request()).unwrap_err();
            assert_eq!(
                err,
                DecideError::Invalid("questions.done.type: bad".into()),
                "{} {status}",
                a.name
            );
            assert!(!err.moves_on(), "an invalid request is invalid everywhere");
        }
        for body in ["not json", "{}", r#"{"answers": 3}"#] {
            let server = StubServer::start(vec![StubResponse::json(200, body)]);
            assert!(
                matches!(
                    (a.build)(&server, None).decide(&request()),
                    Err(DecideError::Unavailable(_))
                ),
                "{}: malformed reply {body}",
                a.name
            );
        }
    }
}

#[test]
fn retryable_statuses_are_retried_and_then_given_up() {
    for a in adapters() {
        for status in [429, 500, 502, 503, 529] {
            let server = StubServer::start(vec![
                StubResponse::json(status, "{}"),
                StubResponse::json(200, (a.ok)(None)),
            ]);
            assert!(
                (a.build)(&server, None).decide(&request()).is_ok(),
                "{} {status}",
                a.name
            );
            assert_eq!(server.count(), 2);
        }
        let server = StubServer::start_with_fallback(vec![], StubResponse::json(529, "{}"));
        assert_eq!(
            (a.build)(&server, None).decide(&request()).unwrap_err(),
            DecideError::Unavailable("http 529".into()),
            "{}",
            a.name
        );
        assert_eq!(server.count(), 3, "{}: one attempt and two retries", a.name);
        // A retry-after longer than the timeout is not waited for.
        let server = StubServer::start(vec![
            StubResponse::json(429, "{}").header("retry-after", "30"),
        ]);
        assert_eq!(
            (a.build)(&server, None).decide(&request()).unwrap_err(),
            DecideError::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            },
            "{}",
            a.name
        );
        assert_eq!(server.count(), 1);
    }
}

#[test]
fn the_key_never_leaks_into_errors_or_debug_output() {
    for a in adapters() {
        let echo = format!(
            r#"{{"message":"bad key {SENTINEL_KEY} {}"}}"#,
            "x".repeat(400)
        );
        let server = StubServer::start(vec![
            StubResponse::json(422, echo),
            StubResponse::json(500, format!("{{\"error\":\"{SENTINEL_KEY}\"}}")),
        ]);
        let p = (a.build)(&server, Some(SENTINEL_KEY));
        let err = p.decide(&request()).unwrap_err();
        let DecideError::Invalid(detail) = &err else {
            panic!("{}: {err:?}", a.name)
        };
        assert!(detail.chars().count() <= 200);
        let err2 = p.decide(&request()).unwrap_err();
        for text in [
            format!("{p:?}"),
            format!("{err:?} {err}"),
            format!("{err2:?} {err2}"),
        ] {
            assert!(!text.contains(SENTINEL_KEY), "{}: {text}", a.name);
        }
    }
}

#[test]
fn a_limit_violation_sends_nothing() {
    for a in adapters() {
        let server = StubServer::start(vec![]);
        let mut req = request();
        req.state = json!("x".repeat(200_000));
        assert!(matches!(
            (a.build)(&server, None).decide(&req),
            Err(DecideError::Invalid(_))
        ));
        assert_eq!(server.count(), 0, "{}", a.name);
    }
}

#[test]
fn each_adapter_has_its_own_threshold_profile() {
    let server = StubServer::start(vec![]);
    let profiles: Vec<String> = adapters()
        .iter()
        .map(|a| (a.build)(&server, None).threshold_profile())
        .collect();
    assert_eq!(
        profiles,
        [
            "vercel_evaluate:typesafe-ai/jev",
            "openrouter_decisions:typesafe/jev-1.13",
            "cloudflare:typesafe/jev",
        ]
    );
}

// --- per-route mappings -----------------------------------------------------

#[test]
fn vercel_sends_boolean_questions_and_the_zero_retention_flag() {
    let server = StubServer::start(vec![
        StubResponse::json(200, vercel_ok(None)),
        StubResponse::json(200, vercel_ok(None)),
    ]);
    let plain = VercelEvaluate::new("v", &server.base_url, "typesafe-ai/jev", None, TIMEOUT);
    plain.decide(&request()).unwrap();
    plain
        .clone()
        .with_zero_data_retention(true)
        .decide(&request())
        .unwrap();
    let requests = server.requests();
    let first = body_of(&requests[0]);
    assert_eq!(
        first,
        json!({
            "model": "typesafe-ai/jev",
            "state": {"task": "t", "extra": "x"},
            "questions": {
                "done": {"type": "boolean", "instructions": "Is it done?"},
                "kind": {"type": "choice", "instructions": "Which?", "criteria": {"b": null, "a": null}},
                "size": {"type": "score", "instructions": "How big?", "criteria": ["small", "medium", "large"]}
            }
        })
    );
    // Choice options and state keys keep their order on the wire.
    let raw = requests[0].split("\r\n\r\n").nth(1).unwrap();
    assert!(raw.find(r#""b":null"#) < raw.find(r#""a":null"#));
    assert!(raw.find(r#""task""#) < raw.find(r#""extra""#));
    assert!(first.get("providerOptions").is_none());
    assert_eq!(
        body_of(&requests[1])["providerOptions"],
        json!({"gateway": {"zeroDataRetention": true}})
    );
}

#[test]
fn vercel_marks_a_language_model_fallback_uncalibrated() {
    let reply =
        vercel_ok(None).replace(r#""model":"typesafe-ai/jev""#, r#""model":"openai/gpt-x""#);
    let server = StubServer::start(vec![StubResponse::json(200, reply)]);
    let r = vercel(&server, None).decide(&request()).unwrap();
    assert_eq!(r.model, "openai/gpt-x");
    assert!(!r.calibrated);
}

#[test]
fn vercel_boolean_answers_are_validated_like_nouls() {
    let reply = vercel_ok(None).replace(r#""probability":0.9"#, r#""probability":1.7"#);
    let server = StubServer::start(vec![StubResponse::json(200, reply)]);
    let r = vercel(&server, None).decide(&request()).unwrap();
    assert!(matches!(r.answers["done"], Answer::Invalid { .. }));
    assert!(matches!(r.answers["kind"], Answer::Choice { .. }));
}

#[test]
fn openrouter_and_cloudflare_send_their_documented_bodies() {
    let server = StubServer::start(vec![StubResponse::json(
        200,
        canonical_ok("typesafe/jev-1.13", None),
    )]);
    openrouter(&server, None).decide(&request()).unwrap();
    let or = body_of(&server.requests()[0]);
    assert_eq!(or["model"], "typesafe/jev-1.13");
    assert_eq!(or["questions"]["done"]["type"], "noul");

    let server = StubServer::start(vec![StubResponse::json(
        200,
        canonical_ok("jev-1.13.0", None),
    )]);
    let r = cloudflare(&server, None).decide(&request()).unwrap();
    assert_eq!(r.model, "jev-1.13.0", "a bare reply is accepted too");
    let cf = body_of(&server.requests()[0]);
    assert_eq!(cf["model"], "typesafe/jev");
    assert_eq!(cf["input"]["state"], json!({"task": "t", "extra": "x"}));
    assert_eq!(cf["input"]["questions"]["done"]["type"], "noul");
    assert!(cf.get("state").is_none());
}

#[test]
fn cloudflare_refuses_an_unsafe_account_id_and_a_failed_envelope() {
    let server = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"result":null,"success":false,"errors":[{"code":5006,"message":"bad"}]}"#,
    )]);
    let bad = Cloudflare::new(
        "cf",
        &server.base_url,
        "../x",
        "typesafe/jev",
        None,
        TIMEOUT,
    );
    assert!(matches!(
        bad.decide(&request()),
        Err(DecideError::Invalid(_))
    ));
    assert_eq!(server.count(), 0);
    assert!(matches!(
        cloudflare(&server, None).decide(&request()),
        Err(DecideError::Unavailable(_))
    ));
}

#[test]
fn aliases_are_told_apart_from_pinned_ids() {
    for alias in ["~typesafe/jev-latest", "jev-latest", "jev-preview"] {
        assert!(is_model_alias(alias), "{alias}");
    }
    for pinned in [
        "typesafe/jev-1.13",
        "jev-1.13.0",
        "typesafe-ai/jev",
        "typesafe/jev",
    ] {
        assert!(!is_model_alias(pinned), "{pinned}");
    }
}
