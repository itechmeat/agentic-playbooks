//! The quota pause (issue #200, design 6): what an attempt that hit a
//! provider quota or rate limit does next, how long the run then waits, how
//! many such pauses a node gets, and what the wake-up probe journals.
//!
//! The engine-side order is fixed: a profile fallback first; then, when the
//! provider's `Retry-After` outlasts the next infrastructure backoff step,
//! a pause right away, otherwise the ordinary infrastructure retries and a
//! pause once they are spent. The attempt that ends in a pause is journaled
//! `interrupted` (no node retry, no `node_finished`), and the run pauses with
//! `reason: "quota (<agent>/<model>)"`, `by: engine` and an `until`. After
//! [`QUOTA_PAUSE_CAP`] consecutive quota pauses of a node without a
//! successful attempt the node fails instead.

use std::time::Duration;

use crate::event::{Event, EventPayload};

use super::{PauseBy, PauseMode, PauseRequest};

/// Consecutive quota pauses of one node, without a successful attempt in
/// between, after which the node fails with a quota-exhausted reason.
pub const QUOTA_PAUSE_CAP: u32 = 6;

/// Env override for the quota pause schedule, a comma-separated list of
/// milliseconds (`APB_QUOTA_BACKOFF_MS=200,400`); the last step repeats.
/// Tests set it so they do not wait out 15 minutes. Mirrors
/// `failure_class::BACKOFF_ENV`; a malformed or empty value falls back to the
/// default.
pub const QUOTA_BACKOFF_ENV: &str = "APB_QUOTA_BACKOFF_MS";

/// 15 min, 30 min, then 1 h for every later pause.
const DEFAULT_QUOTA_BACKOFF_MS: &[u64] = &[15 * 60_000, 30 * 60_000, 60 * 60_000];

/// `supervisor_action.action` of the wake-up probe's outcome. An existing
/// event type (skip-safe for every binary), the same marker trick as
/// `failure_class::INFRA_RETRY_ACTION`. The detail starts with one of
/// [`PROBE_PASSED`], [`PROBE_QUOTA`] or [`PROBE_ERROR`].
pub const QUOTA_PROBE_ACTION: &str = "quota_probe";

/// The probe answered: the node runs again.
pub const PROBE_PASSED: &str = "passed";
/// The probe hit the quota again: the run pauses at the next schedule step.
pub const PROBE_QUOTA: &str = "quota";
/// The probe failed for another reason: the node runs again and its own
/// failure handling decides.
pub const PROBE_ERROR: &str = "error";

/// The `run_paused.reason` of a quota pause.
pub fn quota_reason(agent: &str, model: &str) -> String {
    format!("quota ({agent}/{model})")
}

/// The `(agent, model)` a quota pause names, read back from its reason. An
/// agent id holds no `/`, so the first one separates it from the model
/// (which may hold more, `zai-start/GLM-5.3`).
pub fn parse_quota_reason(reason: &str) -> Option<(String, String)> {
    let inner = reason.strip_prefix("quota (")?.strip_suffix(')')?;
    let (agent, model) = inner.split_once('/')?;
    (!agent.is_empty()).then(|| (agent.to_string(), model.to_string()))
}

/// The pause schedule in effect, honoring [`QUOTA_BACKOFF_ENV`].
fn schedule() -> Vec<Duration> {
    let parsed: Option<Vec<u64>> = std::env::var(QUOTA_BACKOFF_ENV).ok().and_then(|raw| {
        raw.split(',')
            .map(|part| part.trim().parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()
            .filter(|v| !v.is_empty())
    });
    parsed
        .unwrap_or_else(|| DEFAULT_QUOTA_BACKOFF_MS.to_vec())
        .into_iter()
        .map(Duration::from_millis)
        .collect()
}

/// How long the `n`-th quota pause of a node lasts (`n` counts the pauses
/// before it, so the first pause is `n = 0`); the last step repeats.
pub fn quota_backoff(n: u32) -> Duration {
    let s = schedule();
    s.get(n as usize)
        .or_else(|| s.last())
        .copied()
        .unwrap_or(Duration::ZERO)
}

/// The `until` (epoch ms) of a quota pause: `Retry-After` when the provider
/// gave one, else the schedule step for the `prior`-th pause.
pub fn quota_until(retry_after: Option<Duration>, prior: u32, now_ms: u128) -> u64 {
    let wait = retry_after.unwrap_or_else(|| quota_backoff(prior));
    let now = u64::try_from(now_ms).unwrap_or(u64::MAX);
    now.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX))
}

/// The pause request of a quota pause: by the engine, drain (siblings in a
/// batch finish their work), with the reason naming the executor.
pub fn quota_pause(agent: &str, model: &str, until: u64) -> PauseRequest {
    PauseRequest {
        reason: Some(quota_reason(agent, model)),
        by: Some(PauseBy::Engine),
        mode: PauseMode::Drain,
        until: Some(until),
        keep_lock: false,
    }
}

/// What an attempt that failed on quota does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaStep {
    /// Another chain step can run: take the profile fallback.
    Fallback,
    /// Retry the same executor after this infrastructure backoff.
    InfraRetry(Duration),
    /// Pause the run.
    Pause,
}

/// The order of design 6.3: fallback first; then a `Retry-After` longer than
/// the next infrastructure backoff step pauses right away; else an
/// infrastructure retry while any is left; else a pause.
pub fn next_step(
    fallback_left: bool,
    retry_after: Option<Duration>,
    next_backoff: Option<Duration>,
) -> QuotaStep {
    if fallback_left {
        return QuotaStep::Fallback;
    }
    match next_backoff {
        Some(b) if retry_after.is_none_or(|r| r <= b) => QuotaStep::InfraRetry(b),
        _ => QuotaStep::Pause,
    }
}

/// The quota pauses node `node` went through since its last successful
/// attempt: every attempt a quota pause ended (`interrupted`, failure kind
/// `quota`) and every wake-up probe that hit the quota again.
pub fn quota_pauses(events: &[Event], node: &str) -> u32 {
    let mut n = 0;
    for e in events {
        match &e.payload {
            EventPayload::AttemptFinished {
                node: nd,
                status,
                failure_kind,
                ..
            } if nd == node => {
                if status == "succeeded" {
                    n = 0;
                } else if status == "interrupted" && failure_kind.as_deref() == Some("quota") {
                    n += 1;
                }
            }
            EventPayload::SupervisorAction {
                action,
                node: Some(nd),
                detail,
            } if nd == node && action == QUOTA_PROBE_ACTION && detail.starts_with(PROBE_QUOTA) => {
                n += 1;
            }
            _ => {}
        }
    }
    n
}

/// The node the latest quota pause of the run belongs to: the node of the
/// latest attempt a quota pause ended, or of the latest probe that hit the
/// quota again.
pub fn quota_node(events: &[Event]) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::AttemptFinished {
            node,
            status,
            failure_kind,
            ..
        } if status == "interrupted" && failure_kind.as_deref() == Some("quota") => {
            Some(node.clone())
        }
        EventPayload::SupervisorAction {
            action,
            node: Some(node),
            detail,
        } if action == QUOTA_PROBE_ACTION && detail.starts_with(PROBE_QUOTA) => Some(node.clone()),
        _ => None,
    })
}

/// The output of a node that ran out of quota pauses.
pub fn exhausted_output(agent: &str, model: &str, pauses: u32, detail: &str) -> String {
    format!(
        "quota exhausted: `{agent}/{model}` was still out of quota after {pauses} quota pauses without a successful attempt; last error: {}",
        detail.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: u128::from(seq),
            payload,
        }
    }

    fn finished(node: &str, status: &str, kind: Option<&str>) -> EventPayload {
        EventPayload::AttemptFinished {
            item: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
            node: node.into(),
            attempt: 1,
            status: status.into(),
            duration_ms: None,
            session: None,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: kind.map(str::to_string),
            usage: None,
        }
    }

    fn probe(node: &str, detail: &str) -> EventPayload {
        EventPayload::SupervisorAction {
            action: QUOTA_PROBE_ACTION.into(),
            node: Some(node.into()),
            detail: detail.into(),
        }
    }

    #[test]
    fn the_reason_round_trips_with_a_slash_in_the_model() {
        let r = quota_reason("zcode", "zai-start/GLM-5.3");
        assert_eq!(r, "quota (zcode/zai-start/GLM-5.3)");
        assert_eq!(
            parse_quota_reason(&r),
            Some(("zcode".into(), "zai-start/GLM-5.3".into()))
        );
        assert_eq!(
            parse_quota_reason("quota (claude/)"),
            Some(("claude".into(), String::new()))
        );
        assert_eq!(parse_quota_reason("operator pause"), None);
        assert_eq!(parse_quota_reason("quota (/x)"), None);
    }

    #[test]
    fn the_default_schedule_is_15m_30m_then_hourly() {
        assert_eq!(
            DEFAULT_QUOTA_BACKOFF_MS,
            &[900_000, 1_800_000, 3_600_000],
            "the documented policy"
        );
    }

    #[test]
    fn until_prefers_retry_after() {
        assert_eq!(quota_until(Some(Duration::from_secs(2)), 3, 10_000), 12_000);
    }

    #[test]
    fn the_order_is_fallback_then_retry_after_then_infra_retries() {
        let b = Some(Duration::from_secs(5));
        assert_eq!(next_step(true, None, b), QuotaStep::Fallback);
        assert_eq!(
            next_step(true, Some(Duration::from_secs(3600)), b),
            QuotaStep::Fallback,
            "a fallback comes before any pause"
        );
        assert_eq!(
            next_step(false, Some(Duration::from_secs(60)), b),
            QuotaStep::Pause,
            "a Retry-After past the next backoff pauses right away"
        );
        assert_eq!(
            next_step(false, Some(Duration::from_secs(2)), b),
            QuotaStep::InfraRetry(Duration::from_secs(5))
        );
        assert_eq!(
            next_step(false, None, b),
            QuotaStep::InfraRetry(Duration::from_secs(5))
        );
        assert_eq!(next_step(false, None, None), QuotaStep::Pause);
    }

    #[test]
    fn pauses_count_since_the_last_success() {
        let events = vec![
            ev(1, finished("a", "interrupted", Some("quota"))),
            ev(2, probe("a", "quota: still limited")),
            ev(3, finished("b", "interrupted", Some("quota"))),
            ev(4, finished("a", "failed", Some("quota"))),
            ev(5, probe("a", "passed: answered")),
        ];
        assert_eq!(quota_pauses(&events, "a"), 2);
        assert_eq!(quota_pauses(&events, "b"), 1);
        assert_eq!(quota_node(&events).as_deref(), Some("b"));
        let mut more = events.clone();
        more.push(ev(6, finished("a", "succeeded", None)));
        more.push(ev(7, finished("a", "interrupted", Some("quota"))));
        assert_eq!(quota_pauses(&more, "a"), 1, "a success resets the count");
        assert_eq!(quota_node(&more).as_deref(), Some("a"));
        // An interrupted attempt of another cause is no quota pause.
        let other = vec![ev(1, finished("a", "interrupted", None))];
        assert_eq!(quota_pauses(&other, "a"), 0);
        assert_eq!(quota_node(&other), None);
    }
}
