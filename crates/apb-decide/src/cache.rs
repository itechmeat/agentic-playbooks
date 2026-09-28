//! The per-run decision cache.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::digest::{digest, questions_digest};
use crate::{DecisionRequest, DecisionResponse};

/// Answers already given in this run, keyed by `(provider, model, sha256
/// state, sha256 questions)`. Owned by the caller (one per run) and shared
/// across the run's parallel branches.
#[derive(Debug, Default)]
pub struct DecisionCache {
    entries: Mutex<HashMap<(String, String, String, String), DecisionResponse>>,
}

impl DecisionCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(provider: &str, model: &str, req: &DecisionRequest) -> (String, String, String, String) {
        (
            provider.to_string(),
            model.to_string(),
            digest(&req.state),
            questions_digest(&req.questions),
        )
    }

    /// A cached response, marked `cached` with zero latency.
    pub fn get(
        &self,
        provider: &str,
        model: &str,
        req: &DecisionRequest,
    ) -> Option<DecisionResponse> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(&Self::key(provider, model, req)).map(|r| {
            let mut hit = r.clone();
            hit.cached = true;
            hit.latency_ms = 0;
            hit
        })
    }

    pub fn put(
        &self,
        provider: &str,
        model: &str,
        req: &DecisionRequest,
        response: &DecisionResponse,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(Self::key(provider, model, req), response.clone());
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
