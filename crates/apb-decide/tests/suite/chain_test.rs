//! The provider chain and the per-run cache, over fake providers.

use std::collections::BTreeMap;
use std::sync::Arc;

use apb_decide::{
    Answer, DecideError, DecisionCache, DecisionProvider, DecisionRequest, DecisionResponse,
    FakeProvider, Limits, ProviderChain, Question, UseSite,
};
use serde_json::json;

fn request(state: &str) -> DecisionRequest {
    DecisionRequest {
        use_site: UseSite::CompletionCheck,
        state_order: Vec::new(),
        state: json!(state),
        questions: BTreeMap::from([(
            "done".to_string(),
            Question::Noul {
                instructions: json!("Is it done?"),
                criteria: None,
            },
        )]),
    }
}

/// A shared handle, so a test can read a provider's counters after the
/// chain took ownership of its box.
#[derive(Debug)]
struct Shared(Arc<FakeProvider>);

impl DecisionProvider for Shared {
    fn id(&self) -> &str {
        self.0.id()
    }
    fn model(&self) -> &str {
        self.0.model()
    }
    fn limits(&self) -> Limits {
        self.0.limits()
    }
    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        self.0.decide(req)
    }
}

fn fake(id: &str, p: f64) -> Arc<FakeProvider> {
    Arc::new(FakeProvider::new(id).answer("done", json!({"type": "noul", "noul": p})))
}

fn chain(providers: &[&Arc<FakeProvider>]) -> ProviderChain {
    ProviderChain::new(
        providers
            .iter()
            .map(|p| Box::new(Shared(Arc::clone(p))) as Box<dyn DecisionProvider>)
            .collect(),
    )
}

#[test]
fn failures_that_move_on_reach_the_next_provider() {
    for error in [
        DecideError::Auth,
        DecideError::Budget,
        DecideError::Timeout,
        DecideError::Unavailable("down".into()),
        DecideError::RateLimited { retry_after: None },
    ] {
        let first = Arc::new(
            FakeProvider::new("first")
                .answer("done", json!({"noul": 0.1}))
                .fail_next(error.clone()),
        );
        let second = fake("second", 0.7);
        let r = chain(&[&first, &second])
            .decide(&request("s"), None)
            .unwrap();
        assert_eq!(r.provider, "second", "{error:?} must move on");
        assert_eq!(r.answers["done"], Answer::Noul { p: 0.7 });
    }
}

#[test]
fn an_invalid_request_stops_the_chain() {
    let first = Arc::new(FakeProvider::new("first").fail_next(DecideError::Invalid("bad".into())));
    let second = fake("second", 0.7);
    let err = chain(&[&first, &second])
        .decide(&request("s"), None)
        .unwrap_err();
    assert_eq!(err, DecideError::Invalid("bad".into()));
    assert_eq!(second.calls(), 0);
}

#[test]
fn an_empty_chain_is_unavailable_at_once() {
    let err = ProviderChain::default()
        .decide(&request("s"), None)
        .unwrap_err();
    assert!(matches!(err, DecideError::Unavailable(_)));
}

#[test]
fn the_cache_answers_a_repeated_request_without_asking() {
    let p = fake("only", 0.9);
    let c = chain(&[&p]);
    let cache = DecisionCache::new();
    let first = c.decide(&request("same"), Some(&cache)).unwrap();
    let again = c.decide(&request("same"), Some(&cache)).unwrap();
    assert_eq!(p.calls(), 1);
    assert!(!first.cached);
    assert!(again.cached);
    assert_eq!(again.latency_ms, 0);
    assert_eq!(again.answers, first.answers);
    c.decide(&request("other"), Some(&cache)).unwrap();
    assert_eq!(p.calls(), 2, "another state is another key");
}

#[test]
fn the_fake_scripts_by_state_digest_and_captures_requests() {
    let req = request("special");
    let digest = apb_decide::digest::digest(&req.state);
    let p = FakeProvider::new("f")
        .answer("done", json!({"noul": 0.2}))
        .answer_for_state(digest, "done", json!({"noul": 0.95}));
    assert_eq!(
        p.decide(&req).unwrap().answers["done"],
        Answer::Noul { p: 0.95 }
    );
    assert_eq!(
        p.decide(&request("plain")).unwrap().answers["done"],
        Answer::Noul { p: 0.2 }
    );
    assert_eq!(p.requests().len(), 2);
    assert_eq!(p.requests()[0].state, json!("special"));
}
