//! Why an attempt closed its chain step: one vocabulary for
//! `FallbackTriggered.reason`, `ExecStep.entered.reason` and the host
//! task's `fallback_of.reason`.
//!
//! On the wire a reason is a plain snake_case string, the shape every
//! older apb reads as opaque text. Any string reads: one this apb does not
//! know is kept as [`CloseReason::Other`] and written back unchanged, so a
//! journal a newer apb wrote never fails to decode on it. A surface shows
//! an `Other` reason only when it is a slug (`[a-z0-9_]{1,40}`), else as
//! `other`, so journal text never reaches a surface through it.
//!
//! The three stop reasons come from the drive's stop context, never from
//! the attempt status, which reads `interrupted` for all three:
//! `run_paused` and `run_aborted` end the chain walk with no transition,
//! `supervisor_interrupted` keeps retry and fallback.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How an attempt closed its chain step.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CloseReason {
    /// The attempt failed and nothing more specific was established.
    Failed,
    /// A host task expired before a submission.
    Expired,
    TimedOut,
    Interrupted,
    Cancelled,
    /// A host task's question got no answer in time.
    QuestionTimeout,
    /// A CLI attempt that could not start (binary missing, logged out).
    Unstartable,
    /// A host task replaced by a newer one.
    Superseded,
    /// A rejected `success_check`, completion check or verdict-required
    /// output.
    CheckRejected,
    /// The attempt changed a protected path.
    ProtectedPaths,
    Quota,
    Auth,
    Budget,
    /// A close after the infrastructure retries ran out.
    Transient,
    /// The agent or the host reported failure.
    AgentFailed,
    /// A tier-routing cascade went up a tier.
    Routing,
    /// A `--now` pause stopped the attempt: the chain ends.
    RunPaused,
    /// A run abort stopped the attempt: the chain ends.
    RunAborted,
    /// A supervisor interrupted the attempt: retry and fallback go on.
    SupervisorInterrupted,
    /// A reason this apb does not know, as written.
    Other(String),
}

impl CloseReason {
    /// Every reason this apb names, in display order.
    pub const KNOWN: [CloseReason; 19] = [
        CloseReason::Failed,
        CloseReason::Expired,
        CloseReason::TimedOut,
        CloseReason::Interrupted,
        CloseReason::Cancelled,
        CloseReason::QuestionTimeout,
        CloseReason::Unstartable,
        CloseReason::Superseded,
        CloseReason::CheckRejected,
        CloseReason::ProtectedPaths,
        CloseReason::Quota,
        CloseReason::Auth,
        CloseReason::Budget,
        CloseReason::Transient,
        CloseReason::AgentFailed,
        CloseReason::Routing,
        CloseReason::RunPaused,
        CloseReason::RunAborted,
        CloseReason::SupervisorInterrupted,
    ];

    /// The journal string, an `Other` reason as written.
    pub fn as_str(&self) -> &str {
        match self {
            CloseReason::Failed => "failed",
            CloseReason::Expired => "expired",
            CloseReason::TimedOut => "timed_out",
            CloseReason::Interrupted => "interrupted",
            CloseReason::Cancelled => "cancelled",
            CloseReason::QuestionTimeout => "question_timeout",
            CloseReason::Unstartable => "unstartable",
            CloseReason::Superseded => "superseded",
            CloseReason::CheckRejected => "check_rejected",
            CloseReason::ProtectedPaths => "protected_paths",
            CloseReason::Quota => "quota",
            CloseReason::Auth => "auth",
            CloseReason::Budget => "budget",
            CloseReason::Transient => "transient",
            CloseReason::AgentFailed => "agent_failed",
            CloseReason::Routing => "routing",
            CloseReason::RunPaused => "run_paused",
            CloseReason::RunAborted => "run_aborted",
            CloseReason::SupervisorInterrupted => "supervisor_interrupted",
            CloseReason::Other(s) => s,
        }
    }

    /// The reason a journal string names; never fails.
    pub fn parse(s: &str) -> Self {
        Self::KNOWN
            .iter()
            .find(|r| r.as_str() == s)
            .cloned()
            .unwrap_or_else(|| CloseReason::Other(s.to_string()))
    }

    /// What a surface may show: the journal string, or `other` for an
    /// `Other` reason that is not a slug.
    pub fn label(&self) -> &str {
        match self {
            CloseReason::Other(s) if !is_slug(s) => "other",
            r => r.as_str(),
        }
    }
}

/// `[a-z0-9_]{1,40}`.
fn is_slug(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl Serialize for CloseReason {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CloseReason {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(CloseReason::parse(&String::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are a journal contract: each known reason reads from
    /// and writes back to its literal string.
    #[test]
    fn every_reason_reads_and_writes_its_journal_string() {
        let cases = [
            ("failed", CloseReason::Failed),
            ("expired", CloseReason::Expired),
            ("timed_out", CloseReason::TimedOut),
            ("interrupted", CloseReason::Interrupted),
            ("cancelled", CloseReason::Cancelled),
            ("question_timeout", CloseReason::QuestionTimeout),
            ("unstartable", CloseReason::Unstartable),
            ("superseded", CloseReason::Superseded),
            ("check_rejected", CloseReason::CheckRejected),
            ("protected_paths", CloseReason::ProtectedPaths),
            ("quota", CloseReason::Quota),
            ("auth", CloseReason::Auth),
            ("budget", CloseReason::Budget),
            ("transient", CloseReason::Transient),
            ("agent_failed", CloseReason::AgentFailed),
            ("routing", CloseReason::Routing),
            ("run_paused", CloseReason::RunPaused),
            ("run_aborted", CloseReason::RunAborted),
            ("supervisor_interrupted", CloseReason::SupervisorInterrupted),
        ];
        assert_eq!(cases.len(), CloseReason::KNOWN.len());
        for (wire, reason) in cases {
            let json = format!("\"{wire}\"");
            assert_eq!(serde_json::from_str::<CloseReason>(&json).unwrap(), reason);
            assert_eq!(serde_json::to_string(&reason).unwrap(), json);
            assert!(CloseReason::KNOWN.contains(&reason), "{wire} not listed");
        }
    }

    /// A reason from a newer apb is kept as written, but only a slug ever
    /// reaches a surface: journal text cannot pass through it.
    #[test]
    fn an_unknown_reason_is_kept_and_shown_only_as_a_slug() {
        for (wire, shown) in [
            ("model_refused", "model_refused"),
            ("Model Refused", "other"),
            ("", "other"),
            ("a\nb", "other"),
            (&"x".repeat(41), "other"),
            (&"x".repeat(40), &"x".repeat(40)),
        ] {
            let json = serde_json::to_string(wire).unwrap();
            let r: CloseReason = serde_json::from_str(&json).unwrap();
            assert_eq!(r, CloseReason::Other(wire.to_string()));
            assert_eq!(serde_json::to_string(&r).unwrap(), json);
            assert_eq!(r.to_string(), shown);
        }
        assert_eq!(CloseReason::CheckRejected.to_string(), "check_rejected");
    }
}
