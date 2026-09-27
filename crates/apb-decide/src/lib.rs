//! `apb-decide`: a provider-agnostic, blocking client for decision models.
//!
//! A decision model answers typed questions over a text state and returns
//! probabilities, never prose: `choice` (one of up to 255 named options),
//! `score` (a position on 2 to 10 ordered levels) and `noul` (the
//! probability of "yes"). This crate knows nothing about playbooks or runs:
//! the engine's decision runner decides what is asked, what is sent and what
//! is journaled.
//!
//! - [`DecisionProvider`] is the one trait; [`SystemOne`] speaks the
//!   `/v1/systemone` wire format (route table in its module docs) and
//!   [`FakeProvider`] answers from a script.
//! - [`validate`] holds the client-side limit checks, the strict reply
//!   validation and the confidence recompute every provider shares.
//! - [`ProviderChain`] tries providers in order; [`DecisionCache`] is the
//!   caller-owned per-run cache.
//! - [`ApiKey`] prints as `***`; no error, `Debug` output or log line of this
//!   crate contains a key or a full request or reply body.
//!
//! Dependency direction: this crate depends on no other workspace crate;
//! `apb-engine` depends on it.

mod cache;
mod chain;
pub mod digest;
mod error;
mod fake;
mod key;
mod systemone;
#[cfg(feature = "testing")]
pub mod testing;
mod types;
pub mod validate;

pub use cache::DecisionCache;
pub use chain::ProviderChain;
pub use error::DecideError;
pub use fake::FakeProvider;
pub use key::ApiKey;
pub use systemone::SystemOne;
pub use types::{
    Answer, ChoiceCriteria, DecisionRequest, DecisionResponse, Limits, NoulCriteria, Question,
    Usage, UseSite,
};

/// A decision model behind one configured route. Implementations are
/// blocking and shareable across threads (a run's parallel branches ask
/// through the same provider).
pub trait DecisionProvider: Send + Sync + std::fmt::Debug {
    /// The configured provider id (the journal's `provider`).
    fn id(&self) -> &str;
    /// The configured model id (part of the cache key).
    fn model(&self) -> &str;
    fn limits(&self) -> Limits;
    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError>;
}
