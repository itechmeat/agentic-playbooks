//! The one error type of the crate.

use std::time::Duration;

/// Why a decision could not be made. Every variant carries only short,
/// provider-independent text: never a key, never a request or response body
/// (a 422 detail is truncated and scrubbed of the key first).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecideError {
    /// The provider could not be reached or answered with an unusable reply.
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    /// Rate limited, and no retry fit into the timeout.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    /// The request is invalid: a client-side limit check failed (nothing was
    /// sent) or the provider refused it (422).
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The key was refused (401, 403).
    #[error("authentication failed")]
    Auth,
    /// Out of credits (402), or a caller budget is spent.
    #[error("budget exhausted")]
    Budget,
    /// The request did not complete within the timeout.
    #[error("timed out")]
    Timeout,
    /// The caller stopped the request (a stopped run, a branch that lost a
    /// `join: any`). Not an answer to replay: a resumed run asks again.
    #[error("cancelled")]
    Cancelled,
}

impl DecideError {
    /// The journal's `error` value for this failure.
    pub fn kind(&self) -> &'static str {
        match self {
            DecideError::Unavailable(_) => "unavailable",
            DecideError::RateLimited { .. } => "rate_limited",
            DecideError::Invalid(_) => "invalid",
            DecideError::Auth => "auth",
            DecideError::Budget => "budget",
            DecideError::Timeout => "timeout",
            DecideError::Cancelled => "cancelled",
        }
    }

    /// Whether a provider chain moves on to its next provider after this
    /// error. An invalid request stays invalid for every provider, and a
    /// cancelled one is not asked again.
    pub fn moves_on(&self) -> bool {
        !matches!(self, DecideError::Invalid(_) | DecideError::Cancelled)
    }
}
