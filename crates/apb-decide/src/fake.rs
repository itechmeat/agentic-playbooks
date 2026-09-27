//! A scripted provider for tests and dry runs. No network.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use crate::digest::digest;
use crate::validate::{check_limits, validate_answers};
use crate::{DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits, Usage};

/// Answers from a script instead of a model. Reply items use the
/// `/v1/systemone` wire shape (`{"type": "noul", "noul": 0.9}`) and go through
/// the same validation as a real reply. A state-digest script wins over a
/// question-id script; a question with neither is answered `Invalid`.
#[derive(Debug)]
pub struct FakeProvider {
    id: String,
    model: String,
    by_question: BTreeMap<String, Value>,
    by_state: BTreeMap<String, BTreeMap<String, Value>>,
    failures: Mutex<VecDeque<DecideError>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<DecisionRequest>>,
    limits: Limits,
}

impl FakeProvider {
    pub fn new(id: impl Into<String>) -> Self {
        FakeProvider {
            id: id.into(),
            model: "fake-1".into(),
            by_question: BTreeMap::new(),
            by_state: BTreeMap::new(),
            failures: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            limits: Limits::default(),
        }
    }

    /// Answers question `id` with the wire item `item`.
    pub fn answer(mut self, id: impl Into<String>, item: Value) -> Self {
        self.by_question.insert(id.into(), item);
        self
    }

    /// Answers question `id` with `item` when the state's digest is `state_digest`.
    pub fn answer_for_state(
        mut self,
        state_digest: impl Into<String>,
        id: impl Into<String>,
        item: Value,
    ) -> Self {
        self.by_state
            .entry(state_digest.into())
            .or_default()
            .insert(id.into(), item);
        self
    }

    /// The next call fails with `error` (queued; each is used once).
    pub fn fail_next(self, error: DecideError) -> Self {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(error);
        self
    }

    /// Calls that reached the provider (limit-check failures excluded).
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Every request that reached the provider, in order.
    pub fn requests(&self) -> Vec<DecisionRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl DecisionProvider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn limits(&self) -> Limits {
        self.limits
    }

    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        check_limits(req, &self.limits)?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.clone());
        if let Some(e) = self
            .failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            return Err(e);
        }
        let state_script = self.by_state.get(&digest(&req.state));
        let mut items = serde_json::Map::new();
        for id in req.questions.keys() {
            if let Some(item) = state_script
                .and_then(|s| s.get(id))
                .or_else(|| self.by_question.get(id))
            {
                items.insert(id.clone(), item.clone());
            }
        }
        let (answers, ignored_items) = validate_answers(&req.questions, &items);
        Ok(DecisionResponse {
            provider: self.id.clone(),
            model: self.model.clone(),
            calibrated: false,
            answers,
            usage: Usage::default(),
            latency_ms: 0,
            cached: false,
            ignored_items,
        })
    }
}
