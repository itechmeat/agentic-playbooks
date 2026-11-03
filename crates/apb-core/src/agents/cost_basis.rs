//! What a cost an agent CLI reported is: money billed, the list price of
//! work a subscription covers, or not known ([`CostBasis`]).
//!
//! The basis is decided in this order and never guessed:
//!
//! 1. the agent's own module ([`super::HostSetup::cost_basis`]) from facts
//!    it reads locally, with no network and no secret value: which login
//!    variables reach the spawned process (by name), the agent's config
//!    files (by key name or by presence); a module that knows its agent's
//!    logins well enough decides every case itself, and reads a
//!    subscription equivalent only when the facts show a subscription
//!    login (claude's does);
//! 2. else the subscription the person declared for the agent, or for the
//!    provider of the model (`provider/model`), in the models overlay
//!    (`crate::models_table::Subscription::covers_all`: one declared with
//!    `coverage: partial` covers only part of the work, so it decides
//!    nothing);
//! 3. else unknown.
//!
//! A custom agent has no module, so only the declared subscription decides.

use super::output::CostBasis;
pub use super::setup::{CostFacts, CostReading};

/// The basis of a cost `agent` reported on `model`, given what the spawned
/// process sees and `declared`: whether the person declared a subscription
/// that covers all the work of an agent or provider name.
pub fn resolve(
    agent: &str,
    model: &str,
    facts: &CostFacts,
    declared: &dyn Fn(&str) -> bool,
) -> CostReading {
    let module = super::lookup(agent);
    if let Some(reading) = module.and_then(|a| a.setup().cost_basis(facts)) {
        return reading;
    }
    let canonical = module.map(|a| a.descriptor().id);
    let provider = model.split_once('/').map(|(p, _)| p);
    let names = [Some(agent), canonical, provider];
    let basis = if names.into_iter().flatten().any(declared) {
        CostBasis::SubscriptionEquivalent
    } else {
        CostBasis::Unknown
    };
    CostReading::new(basis, "declared".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A custom agent is matched by the declared subscription alone, under
    /// its own name or the provider of its model.
    #[test]
    fn a_declared_subscription_matches_the_agent_or_the_model_provider() {
        let nothing = CostFacts {
            present: &|_| false,
            setting: &|_| None,
            home: None,
            workdir: None,
            hermetic: false,
            system_root: std::path::Path::new("/nonexistent"),
        };
        let cases = [
            (
                "my-agent",
                "m",
                "my-agent",
                CostBasis::SubscriptionEquivalent,
            ),
            (
                "my-agent",
                "acme/m",
                "acme",
                CostBasis::SubscriptionEquivalent,
            ),
            ("my-agent", "acme/m", "other", CostBasis::Unknown),
            ("my-agent", "m", "", CostBasis::Unknown),
        ];
        for (agent, model, name, expected) in cases {
            let declared = |n: &str| n == name;
            assert_eq!(
                resolve(agent, model, &nothing, &declared).basis,
                expected,
                "{agent} {model} {name}"
            );
        }
    }
}
