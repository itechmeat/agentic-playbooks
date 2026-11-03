//! Smoke scenario runs through `apb eval --level smoke`: the judges ask
//! this machine's decision providers (a local stub here), at most
//! `--max-decision-requests` times, and nothing else leaves the process.
//! The fixture `tests/fixtures/scenario/triage` has three judges in a row,
//! each falling back to `unclear` when no answer comes.

use std::fs;

use serde_json::{Value, json};

use crate::http_service::Service;
use crate::scenario_fixture::{Project, failed_checks, journal, run_dir, snapshot};

/// A `/v1/systemone` reply choosing `value` for the question `verdict`.
fn reply(value: &str) -> &'static str {
    let probs: serde_json::Map<String, Value> = ["yes", "no", "unclear"]
        .iter()
        .map(|o| (o.to_string(), json!(if *o == value { 0.9 } else { 0.05 })))
        .collect();
    let body = json!({
        "model": "jev-1.13.0",
        "answers": { "verdict": { "type": "choice", "choice": value, "probabilities": probs, "confidence": 0.9 } },
        "usage": { "input_tokens": 100, "output_tokens": 2 }
    });
    Box::leak(body.to_string().into_boxed_str())
}

/// A6 and A7 at smoke: the cap is per HTTP request and an ask reserves its
/// worst case (one provider, three requests with its retries), so with a
/// cap of 6 the first two judges ask the stub and route by its answers;
/// the third question is refused `budget` before it leaves and the judge
/// takes its `on_unavailable` default. The report counts the two requests,
/// no program starts, and the project tree is the same as before.
#[test]
fn smoke_asks_the_machines_providers_up_to_the_cap() {
    let p = Project::new(&["triage"]);
    let stub = Service::spawn(
        [(
            "/v1/systemone",
            vec![
                Some((200, reply("yes"))),
                Some((200, reply("yes"))),
                Some((200, reply("yes"))),
            ],
        )]
        .into_iter()
        .collect(),
    );
    fs::write(
        p.cfg.path().join("decisions.yaml"),
        format!(
            "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
            stub.base_url
        ),
    )
    .unwrap();
    let approve = p.apb(&["trust", "approve", "triage"]);
    assert!(
        approve.status.success(),
        "{}",
        String::from_utf8_lossy(&approve.stderr)
    );
    let before = snapshot(p.root());

    let (code, r) = p.play(
        "triage",
        "smoke",
        "cap",
        &["--max-decision-requests", "6", "--yes"],
    );

    assert_eq!(p.started(), "", "a smoke run started a program");
    assert_eq!(before, snapshot(p.root()), "the project tree changed");
    assert_eq!(
        stub.bodies("/v1/systemone").len(),
        2,
        "requests past the cap left the process"
    );
    assert_eq!(
        (code, r["verdict"].as_str()),
        (0, Some("passed")),
        "{:?}\n{r:#}",
        failed_checks(&r)
    );
    assert_eq!(r["decision_requests"], 2, "{r:#}");
    let events = journal(&run_dir(&p, &r));
    let asked: Vec<(Option<&str>, Option<&str>)> = events
        .iter()
        .filter(|e| e["type"] == "decision_made")
        .map(|e| (e["node"].as_str(), e["error"].as_str()))
        .collect();
    assert_eq!(
        asked,
        vec![
            (Some("kind"), None),
            (Some("urgent"), None),
            (Some("owner"), Some("budget"))
        ],
        "{events:#?}"
    );
}
