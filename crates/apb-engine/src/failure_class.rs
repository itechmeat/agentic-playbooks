//! Failure classification for an agent attempt, and the bounded retry policy
//! that follows from it (spec 2026-08-05 section 2.3, issue #71 item 2, issue
//! #74 finding 2).
//!
//! Before this module the engine could not tell a spend limit, an expired
//! token, a dropped connection and an ordinary agent error apart: all four
//! arrive as a non-zero exit with some text on stderr, and all four were
//! treated the same way (consume a node retry, then walk the fallback chain).
//! That is wrong in both directions - a network reset burns the node's retry
//! budget that belongs to the agent's own mistakes, and a spend limit walks a
//! fallback chain whose next step is the SAME account and therefore doomed.
//!
//! The classifier is a curated substring table over the failure detail string,
//! in the same spirit as `apb_core::models_table`: a table someone can read and
//! extend, not a regex engine and not a heuristic that learns. It is pure, so
//! the whole table is unit-tested as a matrix.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What kind of thing went wrong in an attempt, decided from the failure detail
/// the adapter produced (stderr then stdout for a process exit, the transport
/// message otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Infrastructure noise: the same executor, run again, has a real chance of
    /// succeeding. Retried on the same executor out of a separate
    /// infrastructure budget, after a backoff.
    Transient,
    /// A credential problem: the same executor will fail identically until a
    /// human re-authenticates. No further retry on this step, and every later
    /// chain step on the same agent is suppressed.
    Auth,
    /// A money or quota problem: same as `Auth`, since it is a property of the
    /// account rather than of the model or the prompt.
    Budget,
    /// Everything else, including the agent's own mistakes: today's behavior,
    /// unchanged (consume a node retry, then advance the fallback chain).
    Agent,
}

impl FailureKind {
    /// Machine-facing label journaled as `attempt_finished.failure_kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Transient => "transient",
            FailureKind::Auth => "auth",
            FailureKind::Budget => "budget",
            FailureKind::Agent => "agent",
        }
    }

    /// True for the kinds that no amount of retrying on this executor can fix.
    pub fn is_non_transient(self) -> bool {
        matches!(self, FailureKind::Auth | FailureKind::Budget)
    }
}

/// Money and quota, in the words the agent CLIs and the APIs behind them
/// actually use. A spend limit is a property of the ACCOUNT: neither a retry nor
/// a different model on the same agent can pay the bill.
///
/// `usage limit` (Claude's periodic cap) belongs here rather than in
/// [`TRANSIENT_PATTERNS`]: it does reset eventually, but not within a backoff
/// any run would sanely wait out, and the useful recovery is the same as for a
/// spend limit - try another agent, or stop and tell a human.
const BUDGET_PATTERNS: &[&str] = &[
    "spend limit",
    "spending limit",
    "usage limit",
    "quota exceeded",
    "exceeded your quota",
    "insufficient_quota",
    "insufficient quota",
    "credit balance",
    "out of credits",
    "insufficient credit",
    "billing",
    "payment required",
    "plan limit",
    "monthly limit",
    "budget exceeded",
];

/// Credentials. Same non-transient handling as [`BUDGET_PATTERNS`], for the same
/// reason: the fix is a human re-authenticating, not another attempt.
///
/// Deliberately NOT here: `permission denied`, which agents produce constantly
/// for ordinary filesystem work and which would then wrongly suppress the whole
/// fallback chain. `api key` and `oauth token` are narrow enough to keep, since
/// an agent that mentions either in a failure detail is talking about its own
/// credential.
const AUTH_PATTERNS: &[&str] = &[
    "unauthorized",
    "authentication",
    "api key",
    "api_key",
    "x-api-key",
    "oauth token",
    "token expired",
    "expired token",
    "token has expired",
    "session expired",
    "invalid token",
    "credentials",
    "forbidden",
    "please log in",
    "please login",
    "not logged in",
    "log in again",
    "/login",
];

/// Infrastructure noise. The cost of a false positive here is one bounded
/// backoff and one extra attempt on the same executor, which is why this list
/// can afford to be the broadest of the three.
///
/// Deliberately NOT here: a bare `timed out`. The engine's own deadline kill
/// reads `agent timed out after {n}s`, and treating that as infrastructure would
/// hand every timed-out node two extra same-executor attempts - a behavior
/// change for every existing playbook. Only transport-shaped timeout wording is
/// listed.
const TRANSIENT_PATTERNS: &[&str] = &[
    "connection reset",
    "connection refused",
    "connection closed",
    "connection aborted",
    "connection error",
    "connection timed out",
    "econnreset",
    "econnrefused",
    "econnaborted",
    "etimedout",
    "enotfound",
    "eai_again",
    "epipe",
    "broken pipe",
    "socket hang up",
    "getaddrinfo",
    "dns",
    "name or service not known",
    "temporary failure in name resolution",
    "network is unreachable",
    "network unreachable",
    "network error",
    "handshake",
    "request timed out",
    "read timeout",
    "gateway timeout",
    "rate limit",
    "rate_limit",
    "ratelimit",
    "too many requests",
    "overloaded",
    "server error",
    "bad gateway",
    "service unavailable",
    "temporarily unavailable",
    "upstream connect",
    "upstream request",
    "try again",
    "stream aborted",
    "stream error",
    "stream closed",
    "premature close",
    "unexpected eof",
    "fetch failed",
    "api_error",
];

/// HTTP statuses that mean "the other side is having a bad time". Matched as
/// standalone numbers (see [`has_code`]), because agent CLIs print them bare
/// (`API Error: 503 ...`) far more often than they print `status: 503`.
///
/// Only the transient family gets numeric matching: a stray `401` in some
/// unrelated number would suppress a whole fallback chain, whereas a stray `503`
/// costs one retry. Auth and budget rely on their words alone.
const TRANSIENT_CODES: &[&str] = &[
    "408", "429", "500", "502", "503", "504", "522", "524", "529",
];

/// Classifies a failure detail string. Case-insensitive; matching is plain
/// substring containment over the lowercased detail.
///
/// Precedence is budget, then auth, then transient: a non-transient cause wins
/// over a transient-looking one in the same message, because a spend limit and
/// an expired token are both very often DELIVERED as a 429, and retrying either
/// on the same executor is pure waste.
pub fn classify(detail: &str) -> FailureKind {
    let hay = detail.to_lowercase();
    if contains_any(&hay, BUDGET_PATTERNS) {
        return FailureKind::Budget;
    }
    if contains_any(&hay, AUTH_PATTERNS) {
        return FailureKind::Auth;
    }
    if contains_any(&hay, TRANSIENT_PATTERNS) || has_code(&hay, TRANSIENT_CODES) {
        return FailureKind::Transient;
    }
    FailureKind::Agent
}

fn contains_any(hay: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| hay.contains(p))
}

/// True when `hay` contains one of `codes` as a standalone number, i.e. with no
/// digit on either side. Without the boundary check a duration like `14293 ms`
/// would read as HTTP 429.
fn has_code(hay: &str, codes: &[&str]) -> bool {
    codes.iter().any(|code| {
        hay.match_indices(code).any(|(at, found)| {
            let before = hay[..at].chars().next_back();
            let after = hay[at + found.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_digit())
                && !after.is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// `supervisor_action.action` marker written before each infrastructure backoff,
/// so the wait and its reason are visible in the attempt timeline without a new
/// event type (the same trick `stall::STALL_ACTION` uses).
pub const INFRA_RETRY_ACTION: &str = "infra_retry";

/// Backoff before each infrastructure retry. The LENGTH of the schedule is the
/// infrastructure retry budget: two extra attempts on the same executor by
/// default, waiting 5 s and then 30 s.
const DEFAULT_BACKOFF_MS: &[u64] = &[5_000, 30_000];

/// Env override for [`DEFAULT_BACKOFF_MS`], a comma-separated list of
/// milliseconds (`APB_INFRA_BACKOFF_MS=20,20`). Tests set a tiny value so they
/// do not wait out a real 35 seconds; it mirrors the
/// `APB_SUPERVISOR_HEARTBEAT_MS` precedent. A malformed or empty value falls
/// back to the default rather than silently disabling the retries.
pub const BACKOFF_ENV: &str = "APB_INFRA_BACKOFF_MS";

/// The backoff schedule in effect, honoring [`BACKOFF_ENV`].
pub fn backoff_schedule() -> Vec<Duration> {
    let parsed: Option<Vec<u64>> = std::env::var(BACKOFF_ENV).ok().and_then(|raw| {
        raw.split(',')
            .map(|part| part.trim().parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()
            .filter(|v| !v.is_empty())
    });
    parsed
        .unwrap_or_else(|| DEFAULT_BACKOFF_MS.to_vec())
        .into_iter()
        .map(Duration::from_millis)
        .collect()
}

/// The backoff wait is slept in slices this size, so a run that is aborted
/// during a 30 s backoff ends within a tick rather than 30 s later. Mirrors
/// `stop::WATCH_SLICE`, which is the granularity at which an `Abort` in
/// `control.jsonl` reaches the drive's cancel flag in the first place.
const BACKOFF_TICK: Duration = Duration::from_millis(25);

/// Waits out one backoff, polling `cancel` every [`BACKOFF_TICK`]. Returns
/// `false` when the wait was cut short because the run was cancelled: `cancel`
/// is the Abort-only flag `stop::StopWatcher` latches via its
/// `stop::CancelFanout` on a pending `Control::Abort`, so the caller can hand
/// the decision back to its own cancellation check.
///
/// A supervisor `Interrupt` is deliberately NOT polled here: it targets a
/// RUNNING attempt, and during a backoff there is no agent process to tear down.
/// Nor does the next attempt pick it up: every attempt takes its control baseline
/// from the latest seq already posted when it begins (`node::execute_node`), so an
/// entry queued during the backoff sits at or below that baseline and is never
/// re-acked - it is spent, and the next node boundary consumes it as the no-op
/// that `control_apply`'s `Interrupt` arm documents. An operator who wants work to
/// stop during a backoff has to post an `Abort`, which is the signal this wait
/// does poll.
pub fn wait_backoff(total: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + total;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(BACKOFF_TICK));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The curated table, asserted as a matrix: one row per pattern family with
    /// a realistic detail string from an agent CLI. Every row states WHY it is
    /// classified the way it is in the module docs of the table itself.
    #[test]
    fn transient_details_are_classified_transient() {
        for detail in [
            "API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"}}",
            "API Error: 500 Internal Server Error",
            "API Error: 502 Bad Gateway",
            "503 Service Unavailable",
            "504 Gateway Timeout",
            "Overloaded",
            "{\"type\":\"overloaded_error\"}",
            "too many requests, please try again later",
            "read ECONNRESET",
            "connect ECONNREFUSED 127.0.0.1:443",
            "Error: connection reset by peer",
            "write EPIPE: broken pipe",
            "socket hang up",
            "getaddrinfo ENOTFOUND api.anthropic.com",
            "dns error: temporary failure in name resolution",
            "network is unreachable",
            "TLS handshake failed",
            "connection timed out",
            "request timed out",
            "upstream connect error",
            "stream aborted unexpectedly",
            "fetch failed",
            "the service is temporarily unavailable",
        ] {
            assert_eq!(
                classify(detail),
                FailureKind::Transient,
                "expected transient for {detail:?}"
            );
        }
    }

    #[test]
    fn auth_details_are_classified_auth() {
        for detail in [
            "API Error: 401 {\"type\":\"authentication_error\"}",
            "Unauthorized",
            "invalid api key",
            "invalid x-api-key provided",
            "your oauth token has expired",
            "token expired, please log in again",
            "credentials are no longer valid",
            "Forbidden: the token lacks the required scope",
            "please run /login to re-authenticate",
            "not logged in",
        ] {
            assert_eq!(
                classify(detail),
                FailureKind::Auth,
                "expected auth for {detail:?}"
            );
        }
    }

    #[test]
    fn budget_details_are_classified_budget() {
        for detail in [
            "Claude usage limit reached, resets at 3pm",
            "your organization has reached its spend limit",
            "spending limit exceeded for this workspace",
            "quota exceeded for this project",
            "insufficient_quota",
            "Your credit balance is too low to access the API",
            "out of credits",
            "billing is not configured for this account",
            "402 Payment Required",
            "monthly limit reached",
        ] {
            assert_eq!(
                classify(detail),
                FailureKind::Budget,
                "expected budget for {detail:?}"
            );
        }
    }

    #[test]
    fn ordinary_agent_failures_stay_agent() {
        for detail in [
            "",
            "boom",
            "agent exited with Some(1): task failed: the tests do not pass",
            "cargo build failed with 3 errors",
            "permission denied while writing /tmp/out.txt",
            "attempt interrupted by supervisor",
            "cancelled",
        ] {
            assert_eq!(
                classify(detail),
                FailureKind::Agent,
                "expected agent for {detail:?}"
            );
        }
    }

    /// The engine's OWN deadline kill must not be read as a transport timeout:
    /// classifying it transient would give every timed-out node two extra
    /// same-executor attempts with backoff, which is a behavior change nobody
    /// asked for (an agent that hangs once tends to hang again, and the
    /// fallback chain is the documented recovery for it).
    #[test]
    fn the_engines_own_deadline_kill_is_not_transient() {
        assert_eq!(classify("agent timed out after 30s"), FailureKind::Agent);
    }

    /// A digit run that merely CONTAINS an HTTP-looking code is not one.
    #[test]
    fn a_number_containing_a_status_code_is_not_a_status_code() {
        assert_eq!(
            classify("wrote 14293 bytes, then failed"),
            FailureKind::Agent
        );
        assert_eq!(classify("exited after 5000 ms"), FailureKind::Agent);
    }

    /// Classification is case-insensitive over the whole table.
    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(classify("CONNECTION RESET BY PEER"), FailureKind::Transient);
        assert_eq!(classify("UNAUTHORIZED"), FailureKind::Auth);
        assert_eq!(classify("SPEND LIMIT REACHED"), FailureKind::Budget);
    }

    /// Precedence: a non-transient cause wins over a transient-looking one in
    /// the same message. A spend limit is very often delivered AS a 429, and
    /// retrying it on the same account is pure waste.
    #[test]
    fn a_budget_message_delivered_as_a_429_is_budget() {
        assert_eq!(
            classify("API Error: 429 usage limit reached"),
            FailureKind::Budget
        );
        assert_eq!(
            classify("429 too many requests: quota exceeded"),
            FailureKind::Budget
        );
    }

    #[test]
    fn an_auth_message_delivered_as_a_429_is_auth() {
        assert_eq!(
            classify("429 too many requests (unauthorized client)"),
            FailureKind::Auth
        );
    }

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(FailureKind::Transient.as_str(), "transient");
        assert_eq!(FailureKind::Auth.as_str(), "auth");
        assert_eq!(FailureKind::Budget.as_str(), "budget");
        assert_eq!(FailureKind::Agent.as_str(), "agent");
        assert!(FailureKind::Auth.is_non_transient());
        assert!(FailureKind::Budget.is_non_transient());
        assert!(!FailureKind::Transient.is_non_transient());
        assert!(!FailureKind::Agent.is_non_transient());
    }

    #[test]
    fn the_default_backoff_schedule_is_two_retries_of_5s_then_30s() {
        // No env override in effect for this assertion: the default is the
        // documented policy, and the schedule length IS the budget.
        assert_eq!(DEFAULT_BACKOFF_MS, &[5_000, 30_000]);
    }

    #[test]
    fn a_backoff_wait_returns_false_as_soon_as_the_run_is_cancelled() {
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!wait_backoff(Duration::from_secs(30), &cancel));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an already-cancelled run must not wait out the backoff"
        );
    }

    #[test]
    fn a_backoff_wait_of_zero_returns_immediately() {
        let cancel = AtomicBool::new(false);
        assert!(wait_backoff(Duration::ZERO, &cancel));
    }
}
