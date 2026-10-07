//! Where a host task's executor comes from: which step of the profile's
//! executor chain it is (`HintSource`) and, for a step the chain advanced
//! to, what closed the step before it (`FallbackOf`).
//!
//! In host mode the host picks the model itself, so a task's `model_hint`
//! is only the model the profile declares for that chain step. Every
//! surface shows it with this label, so a reader does not take a fallback's
//! declared model for the model that did the work.

use serde::{Deserialize, Serialize};

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
/// previous step and how it closed (`failed`, `expired`, `timed_out`,
/// `interrupted`, `cancelled`, `question_timeout`, `unstartable`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackOf {
    pub attempt: u32,
    pub reason: String,
}

/// How an attempt ended, as the drive saw it.
#[derive(Debug, Clone, Copy, Default)]
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
}

/// The `fallback_of.reason` of an attempt that did not succeed: the host
/// task's own closure first (a `succeeded` reply that was then rejected
/// counts as `failed`), then what the drive observed.
pub(crate) fn close_reason(end: AttemptEnd<'_>) -> &str {
    match end.host_closed {
        Some(s) if s != "succeeded" => s,
        _ if end.unstartable => "unstartable",
        _ if end.interrupted => "interrupted",
        _ if end.replied => "failed",
        _ if end.timed_out => "timed_out",
        _ => "failed",
    }
}

fn closed_phrase(reason: &str) -> String {
    match reason {
        "failed" | "expired" => reason.to_string(),
        "timed_out" => "timed out".to_string(),
        "interrupted" | "cancelled" | "superseded" => format!("was {reason}"),
        "question_timeout" => "got no answer to its question in time".to_string(),
        "unstartable" => "could not start".to_string(),
        other => format!("ended ({other})"),
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
            reason: reason.into(),
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
            ("something_new", "ended (something_new)"),
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
        assert_eq!(close_reason(end(Some("expired"))), "expired");
        // A reply the status file or a success_check rejected.
        assert_eq!(close_reason(end(Some("succeeded"))), "failed");
        assert_eq!(close_reason(end(None)), "failed");
        let cli = AttemptEnd {
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(close_reason(cli), "timed_out");
        let unstartable = AttemptEnd {
            unstartable: true,
            ..Default::default()
        };
        assert_eq!(close_reason(unstartable), "unstartable");
        let interrupted = AttemptEnd {
            interrupted: true,
            replied: true,
            ..Default::default()
        };
        assert_eq!(close_reason(interrupted), "interrupted");
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
