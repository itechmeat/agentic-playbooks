//! Where a host task's executor comes from: which step of the profile's
//! executor chain it is (`HintSource`) and, for a step the chain advanced
//! to, what closed the step before it (`FallbackOf`).
//!
//! In host mode the host picks the model itself, so a task's `model_hint`
//! is only the model the profile declares for that chain step. Every
//! surface shows it with this label, so a reader does not take a fallback's
//! declared model for the model that did the work.

use serde::{Deserialize, Serialize};

use crate::execution_identity::CloseReason;
use crate::failure_class::FailureKind;

/// The chain step a host task runs, as the profile declares it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HintSource {
    /// The profile's own executor: no model hint, the host picks the model.
    Primary { profile: String },
    /// Fallback entry `index` (1-based) of the profile's `of` fallbacks.
    Fallback {
        index: u32,
        of: u32,
        profile: String,
    },
    /// A tier that tier routing put in front of the profile's executor.
    Tier { tier: String, profile: String },
    /// The host step a `cli` run appends after the whole agent CLI chain,
    /// taken only when none of its CLIs could start: no model hint.
    HostFallback { profile: String },
}

/// Why a task of a later chain step exists: the attempt that closed the
/// previous step and how it closed (`crate::execution_identity::CloseReason`,
/// its plain journal string on the wire).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackOf {
    pub attempt: u32,
    #[cfg_attr(feature = "ts", ts(as = "String"))]
    pub reason: CloseReason,
}

/// How an attempt ended, as the drive saw it at the attempt's final
/// verdict.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttemptEnd<'a> {
    /// The host task's closing status (`succeeded`, `failed`, `expired`,
    /// `interrupted`, `cancelled`, `question_timeout`), for a host attempt.
    pub host_closed: Option<&'a str>,
    /// The agent or the host replied (the attempt then failed on its
    /// verdict, a `success_check` or the completion check).
    pub replied: bool,
    pub timed_out: bool,
    pub interrupted: bool,
    /// A CLI attempt that could not start (binary missing, logged out).
    pub unstartable: bool,
    /// The close reason of the stop that closed the attempt, as the
    /// drive's stop context read it (`run_paused`, `run_aborted`,
    /// `supervisor_interrupted`).
    pub stop: Option<CloseReason>,
    /// The attempt changed a protected path.
    pub protected: bool,
    /// A `success_check`, the completion check or a required verdict
    /// rejected the attempt's reply.
    pub check_rejected: bool,
    /// The failure classification of a failed process.
    pub failure_kind: Option<FailureKind>,
    /// The agent or the host reported failure.
    pub agent_reported_failure: bool,
}

/// The close reason of an attempt that did not succeed. First match wins:
/// the stop reason; a host closure other than `succeeded` and `failed`;
/// `unstartable`; `protected_paths`; `check_rejected`; `quota`, `auth`,
/// `budget`, `transient` from the failure classification; `timed_out`;
/// `interrupted`; `agent_failed`; `failed`. Nothing the agent wrote is read.
pub(crate) fn close_reason(end: &AttemptEnd<'_>) -> CloseReason {
    if let Some(stop) = &end.stop {
        return stop.clone();
    }
    match end.host_closed {
        Some(s) if s != "succeeded" && s != "failed" => return CloseReason::parse(s),
        _ => {}
    }
    let kind = end.failure_kind.and_then(|k| match k {
        FailureKind::Quota => Some(CloseReason::Quota),
        FailureKind::Auth => Some(CloseReason::Auth),
        FailureKind::Budget => Some(CloseReason::Budget),
        FailureKind::Transient => Some(CloseReason::Transient),
        FailureKind::Agent => None,
    });
    if end.unstartable {
        CloseReason::Unstartable
    } else if end.protected {
        CloseReason::ProtectedPaths
    } else if end.check_rejected {
        CloseReason::CheckRejected
    } else if let Some(reason) = kind {
        reason
    } else if end.timed_out && !end.replied {
        CloseReason::TimedOut
    } else if end.interrupted {
        CloseReason::Interrupted
    } else if end.agent_reported_failure || end.host_closed == Some("failed") {
        CloseReason::AgentFailed
    } else {
        CloseReason::Failed
    }
}

fn closed_phrase(reason: &CloseReason) -> String {
    match reason {
        CloseReason::Failed | CloseReason::Expired => reason.as_str().to_string(),
        CloseReason::TimedOut => "timed out".to_string(),
        CloseReason::Interrupted | CloseReason::Cancelled | CloseReason::Superseded => {
            format!("was {}", reason.as_str())
        }
        CloseReason::QuestionTimeout => "got no answer to its question in time".to_string(),
        CloseReason::Unstartable => "could not start".to_string(),
        CloseReason::CheckRejected => "was rejected by its check".to_string(),
        CloseReason::ProtectedPaths => "changed a protected path".to_string(),
        CloseReason::Quota => "hit a provider quota".to_string(),
        CloseReason::Auth => "failed to authenticate".to_string(),
        CloseReason::Budget => "hit a spend limit".to_string(),
        CloseReason::Transient => "failed after its infrastructure retries".to_string(),
        CloseReason::AgentFailed => "reported failure".to_string(),
        CloseReason::Routing => "was routed up a tier".to_string(),
        CloseReason::RunPaused => "was paused".to_string(),
        CloseReason::RunAborted => "was stopped with the run".to_string(),
        CloseReason::SupervisorInterrupted => "was interrupted by the supervisor".to_string(),
        CloseReason::Other(_) => format!("ended ({})", reason.label()),
    }
}

const HOST_PICKS: &str = "the host picks its own model";

/// Every label opens with this, so a declared model never reads as an order
/// to run that model (a host once shelled out to the profile's CLI on it).
pub const ADVISORY: &str = "advisory: ";

/// The one-line label of a task's executor, for `apb tasks`, MCP, the
/// journal and the dashboard: `advisory: model hint M (fallback 1 of 1
/// declared by profile P after attempt 1 failed; the host picks its own
/// model)`, or for the profile's own executor `advisory: primary executor
/// of profile P (no model hint; the host picks its own model)`. How the host
/// executes the task is [`super::EXECUTION_CONTRACT`]. `None` for a task with neither a
/// hint nor a source (an older run's task).
pub fn describe(
    model_hint: Option<&str>,
    source: Option<&HintSource>,
    fallback_of: Option<&FallbackOf>,
) -> Option<String> {
    let after = fallback_of
        .map(|f| format!(" after attempt {} {}", f.attempt, closed_phrase(&f.reason)))
        .unwrap_or_default();
    let declared = match source {
        // Neither carries a hint: the host picks the model.
        Some(HintSource::Primary { profile }) => {
            return Some(format!(
                "{ADVISORY}primary executor of profile {profile}{after} (no model hint; {HOST_PICKS})"
            ));
        }
        Some(HintSource::HostFallback { profile }) => {
            return Some(format!(
                "{ADVISORY}host fallback for profile {profile} after the agent CLI steps{after} (no model hint; {HOST_PICKS})"
            ));
        }
        Some(HintSource::Fallback { index, of, profile }) => {
            format!("fallback {index} of {of} declared by profile {profile}")
        }
        Some(HintSource::Tier { tier, profile }) => {
            format!("routed tier {tier} declared by profile {profile}")
        }
        None => "declared by the profile".to_string(),
    };
    let m = model_hint?;
    Some(format!(
        "{ADVISORY}model hint {m} ({declared}{after}; {HOST_PICKS})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(attempt: u32, reason: &str) -> FallbackOf {
        FallbackOf {
            attempt,
            reason: CloseReason::parse(reason),
        }
    }

    #[test]
    fn the_label_names_the_chain_step_and_what_closed_the_previous_one() {
        let fallback = HintSource::Fallback {
            index: 1,
            of: 1,
            profile: "site-fixer".into(),
        };
        assert_eq!(
            describe(Some("haiku"), Some(&fallback), Some(&after(1, "failed"))).as_deref(),
            Some(
                "advisory: model hint haiku (fallback 1 of 1 declared by profile site-fixer after attempt 1 failed; the host picks its own model)"
            )
        );
        let primary = HintSource::Primary {
            profile: "site-fixer".into(),
        };
        assert_eq!(
            describe(None, Some(&primary), None).as_deref(),
            Some(
                "advisory: primary executor of profile site-fixer (no model hint; the host picks its own model)"
            )
        );
        let host = HintSource::HostFallback {
            profile: "site-fixer".into(),
        };
        assert_eq!(
            describe(None, Some(&host), Some(&after(1, "unstartable"))).as_deref(),
            Some(
                "advisory: host fallback for profile site-fixer after the agent CLI steps after attempt 1 could not start (no model hint; the host picks its own model)"
            )
        );
        let tier = HintSource::Tier {
            tier: "light".into(),
            profile: "p".into(),
        };
        assert_eq!(
            describe(Some("m"), Some(&tier), None).as_deref(),
            Some(
                "advisory: model hint m (routed tier light declared by profile p; the host picks its own model)"
            )
        );
        // An older run's task: a bare hint still says it is a declaration.
        assert_eq!(
            describe(Some("m"), None, None).as_deref(),
            Some("advisory: model hint m (declared by the profile; the host picks its own model)")
        );
        assert_eq!(describe(None, None, None), None);
    }

    #[test]
    fn every_closing_reason_has_its_wording() {
        let source = HintSource::Fallback {
            index: 2,
            of: 2,
            profile: "p".into(),
        };
        for (reason, phrase) in [
            ("failed", "failed"),
            ("expired", "expired"),
            ("timed_out", "timed out"),
            ("interrupted", "was interrupted"),
            ("cancelled", "was cancelled"),
            ("superseded", "was superseded"),
            ("question_timeout", "got no answer to its question in time"),
            ("unstartable", "could not start"),
            ("check_rejected", "was rejected by its check"),
            ("protected_paths", "changed a protected path"),
            ("quota", "hit a provider quota"),
            ("auth", "failed to authenticate"),
            ("budget", "hit a spend limit"),
            ("transient", "failed after its infrastructure retries"),
            ("agent_failed", "reported failure"),
            ("routing", "was routed up a tier"),
            ("run_paused", "was paused"),
            ("run_aborted", "was stopped with the run"),
            (
                "supervisor_interrupted",
                "was interrupted by the supervisor",
            ),
            ("something_new", "ended (something_new)"),
            ("Not A Slug", "ended (other)"),
        ] {
            assert_eq!(
                describe(Some("m"), Some(&source), Some(&after(3, reason))).as_deref(),
                Some(
                    format!(
                        "advisory: model hint m (fallback 2 of 2 declared by profile p after attempt 3 {phrase}; the host picks its own model)"
                    )
                    .as_str()
                ),
                "{reason}"
            );
        }
    }

    #[test]
    fn the_host_closure_wins_over_what_the_drive_saw() {
        let end = |host_closed| AttemptEnd {
            host_closed,
            replied: true,
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(close_reason(&end(Some("expired"))), CloseReason::Expired);
        // A reply the status file or a success_check rejected, without the
        // drive naming which: nothing more is established.
        assert_eq!(close_reason(&end(Some("succeeded"))), CloseReason::Failed);
        assert_eq!(close_reason(&end(None)), CloseReason::Failed);
        let cli = AttemptEnd {
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(close_reason(&cli), CloseReason::TimedOut);
        let unstartable = AttemptEnd {
            unstartable: true,
            ..Default::default()
        };
        assert_eq!(close_reason(&unstartable), CloseReason::Unstartable);
        let interrupted = AttemptEnd {
            interrupted: true,
            replied: true,
            ..Default::default()
        };
        assert_eq!(close_reason(&interrupted), CloseReason::Interrupted);
    }

    /// Design 2.2 item 2: the first fact in the precedence wins, so a
    /// stop is never read as a check, a check never as the agent's own
    /// failure, and a failure the drive cannot name reads `failed`.
    #[test]
    fn each_close_wins_over_every_fact_below_it() {
        let ladder: Vec<(CloseReason, AttemptEnd<'static>)> = vec![
            (
                CloseReason::RunPaused,
                AttemptEnd {
                    stop: Some(CloseReason::RunPaused),
                    ..Default::default()
                },
            ),
            (
                CloseReason::QuestionTimeout,
                AttemptEnd {
                    host_closed: Some("question_timeout"),
                    ..Default::default()
                },
            ),
            (
                CloseReason::Unstartable,
                AttemptEnd {
                    unstartable: true,
                    ..Default::default()
                },
            ),
            (
                CloseReason::ProtectedPaths,
                AttemptEnd {
                    protected: true,
                    ..Default::default()
                },
            ),
            (
                CloseReason::CheckRejected,
                AttemptEnd {
                    check_rejected: true,
                    ..Default::default()
                },
            ),
            (
                CloseReason::Quota,
                AttemptEnd {
                    failure_kind: Some(FailureKind::Quota),
                    ..Default::default()
                },
            ),
            (
                CloseReason::TimedOut,
                AttemptEnd {
                    timed_out: true,
                    ..Default::default()
                },
            ),
            (
                CloseReason::AgentFailed,
                AttemptEnd {
                    agent_reported_failure: true,
                    ..Default::default()
                },
            ),
        ];
        // Each rung, with every fact of the rungs below it added, still
        // reads as itself.
        for (i, (want, _)) in ladder.iter().enumerate() {
            let mut end = ladder[i].1.clone();
            for (_, lower) in &ladder[i + 1..] {
                end.host_closed = end.host_closed.or(lower.host_closed);
                end.unstartable |= lower.unstartable;
                end.protected |= lower.protected;
                end.check_rejected |= lower.check_rejected;
                end.failure_kind = end.failure_kind.or(lower.failure_kind);
                end.timed_out |= lower.timed_out;
                end.agent_reported_failure |= lower.agent_reported_failure;
            }
            assert_eq!(&close_reason(&end), want, "rung {i}");
        }
        assert_eq!(close_reason(&AttemptEnd::default()), CloseReason::Failed);
        for (kind, want) in [
            (FailureKind::Auth, CloseReason::Auth),
            (FailureKind::Budget, CloseReason::Budget),
            (FailureKind::Transient, CloseReason::Transient),
            // The agent's own failure has no class of its own.
            (FailureKind::Agent, CloseReason::Failed),
        ] {
            let end = AttemptEnd {
                failure_kind: Some(kind),
                ..Default::default()
            };
            assert_eq!(close_reason(&end), want);
        }
        // A host task the host closed `failed` is the agent's own report.
        let host_failed = AttemptEnd {
            host_closed: Some("failed"),
            replied: true,
            ..Default::default()
        };
        assert_eq!(close_reason(&host_failed), CloseReason::AgentFailed);
    }

    /// A declared model read as an order once made a host shell out to the
    /// profile's agent CLI on it: every label is framed as advice.
    #[test]
    fn every_label_is_framed_as_advisory() {
        let p = || "p".to_string();
        let sources = [
            HintSource::Primary { profile: p() },
            HintSource::HostFallback { profile: p() },
            HintSource::Fallback {
                index: 1,
                of: 1,
                profile: p(),
            },
            HintSource::Tier {
                tier: "light".into(),
                profile: p(),
            },
        ];
        for source in &sources {
            for hint in [None, Some("m")] {
                if let Some(label) = describe(hint, Some(source), None) {
                    assert!(label.starts_with(ADVISORY), "{label}");
                }
            }
        }
        let bare = describe(Some("m"), None, None).unwrap();
        assert!(bare.starts_with(ADVISORY), "{bare}");
    }
}
