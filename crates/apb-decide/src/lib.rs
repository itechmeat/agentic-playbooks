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
//!   [`FakeProvider`] answers from a script. [`VercelEvaluate`] (Vercel AI
//!   Gateway `/v1/evaluate`), [`OpenRouterDecisions`] (OpenRouter's alpha
//!   Decisions API) and [`Cloudflare`] (Workers AI REST) map their route's
//!   variant of the format onto the same types (issue #165 Part 15). [`LlmEmulation`] imitates the
//!   interface with a chat model (uncalibrated, see [`llm_emulation`]).
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
mod cloudflare;
pub mod digest;
mod error;
mod fake;
mod http;
mod key;
pub mod llm_emulation;
mod openrouter_decisions;
mod systemone;
#[cfg(feature = "testing")]
pub mod testing;
mod types;
pub mod validate;
mod vercel_evaluate;

pub use cache::DecisionCache;
pub use chain::ProviderChain;
pub use cloudflare::{CLOUDFLARE_BASE_URL, Cloudflare};
pub use error::DecideError;
pub use fake::FakeProvider;
pub use key::ApiKey;
pub use llm_emulation::{LlmEmulation, StructuredOutput};
pub use openrouter_decisions::OpenRouterDecisions;
pub use systemone::SystemOne;
pub use types::{
    Answer, ChoiceCriteria, DecisionRequest, DecisionResponse, Limits, NoulCriteria, Question,
    Usage, UseSite,
};
pub use vercel_evaluate::VercelEvaluate;

/// A decision model behind one configured route. Implementations are
/// blocking and shareable across threads (a run's parallel branches ask
/// through the same provider).
pub trait DecisionProvider: Send + Sync + std::fmt::Debug {
    /// The configured provider id (the journal's `provider`).
    fn id(&self) -> &str;
    /// The configured model id (part of the cache key).
    fn model(&self) -> &str;
    fn limits(&self) -> Limits;
    /// The threshold profile this provider's answers belong to: the wire
    /// route and the model, `<kind>:<model>`. Thresholds tuned on one
    /// profile do not transfer to another (another route can post-process,
    /// fall back or answer with another model), so a threshold store keys on
    /// it alongside the provider id. The default is `<id>:<model>`.
    fn threshold_profile(&self) -> String {
        format!("{}:{}", self.id(), self.model())
    }
    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError>;
}
