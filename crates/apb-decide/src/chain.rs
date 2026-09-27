//! An ordered list of providers tried in turn.

use crate::{DecideError, DecisionCache, DecisionProvider, DecisionRequest, DecisionResponse};

/// Tries each provider in order. An authentication failure, a spent budget,
/// an unreachable or timed-out provider and an exhausted rate limit move on
/// to the next one; an invalid request does not (it is invalid for every
/// provider). The response names the provider that answered. An empty chain
/// is unavailable at once.
#[derive(Default)]
pub struct ProviderChain {
    providers: Vec<Box<dyn DecisionProvider>>,
}

impl std::fmt::Debug for ProviderChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.providers.iter().map(|p| p.id().to_string()))
            .finish()
    }
}

impl ProviderChain {
    pub fn new(providers: Vec<Box<dyn DecisionProvider>>) -> Self {
        ProviderChain { providers }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The configured provider ids, in order.
    pub fn ids(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// Asks the chain, consulting and filling `cache` when one is given: a
    /// cached answer for a provider is used before that provider is asked,
    /// with `cached: true` and zero latency.
    pub fn decide(
        &self,
        req: &DecisionRequest,
        cache: Option<&DecisionCache>,
    ) -> Result<DecisionResponse, DecideError> {
        let mut last = DecideError::Unavailable("no provider configured".into());
        for provider in &self.providers {
            if let Some(hit) = cache.and_then(|c| c.get(provider.id(), provider.model(), req)) {
                return Ok(hit);
            }
            match provider.decide(req) {
                Ok(response) => {
                    if let Some(c) = cache {
                        c.put(provider.id(), provider.model(), req, &response);
                    }
                    return Ok(response);
                }
                Err(e) if e.moves_on() => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}
