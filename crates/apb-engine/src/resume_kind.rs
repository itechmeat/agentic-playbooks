//! What a resume continues and what it expedites, apart from the pause and
//! the scheduler that both read them: the hold an engine resume is bound to
//! ([`WakeHold`]) and the retry-now of a waiting retry ([`RetryNow`], with
//! who asked for it, [`ExpediteBy`], journaled as `retry_expedited.by`).

use serde::{Deserialize, Serialize};

/// What an engine resume (a waker, `apb resume --all`) continues: the pause
/// or the release at a journal sequence. The resumed driver checks it again
/// under the run's event lock and journals nothing once it no longer holds
/// the run, so a person's pause, resume or stop that came after the waker's
/// own look always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeHold {
    /// The `run_paused` at this journal sequence.
    Pause(u64),
    /// The `drive_released` at this journal sequence.
    Release(u64),
}

/// Who asked for a waiting retry to start now (`retry_expedited.by`).
/// Display only; a value this binary does not know reads as absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpediteBy {
    /// A person: `apb resume --retry-now` at a terminal, the dashboard.
    Operator,
    /// The run's supervisor session.
    Supervisor,
    /// The engine itself.
    Engine,
    /// An agent host over MCP (`run_resume { retry_now }`): it may relay a
    /// person or act on its own, so it is not recorded as the operator.
    Host,
}

impl ExpediteBy {
    pub fn as_str(self) -> &'static str {
        match self {
            ExpediteBy::Operator => "operator",
            ExpediteBy::Supervisor => "supervisor",
            ExpediteBy::Engine => "engine",
            ExpediteBy::Host => "host",
        }
    }

    /// The inverse of [`ExpediteBy::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        [
            ExpediteBy::Operator,
            ExpediteBy::Supervisor,
            ExpediteBy::Engine,
            ExpediteBy::Host,
        ]
        .into_iter()
        .find(|b| b.as_str() == s)
    }
}

/// The retry-now part of a resume: the waiting retry of `node` (every one
/// when `None`), expedited by `by`.
#[derive(Debug, Clone, Copy)]
pub struct RetryNow<'a> {
    pub node: Option<&'a str>,
    pub by: ExpediteBy,
}

#[cfg(test)]
mod tests {
    use super::ExpediteBy;

    /// The `by` labels are journal values (`retry_expedited.by`) and the
    /// CLI's `--retry-now-by` words: the literal strings, both ways.
    #[test]
    fn expedite_by_labels_are_the_journaled_words() {
        for (by, label) in [
            (ExpediteBy::Operator, "operator"),
            (ExpediteBy::Supervisor, "supervisor"),
            (ExpediteBy::Engine, "engine"),
            (ExpediteBy::Host, "host"),
        ] {
            assert_eq!(by.as_str(), label);
            assert_eq!(serde_json::to_value(by).unwrap(), label);
            assert_eq!(ExpediteBy::parse(label), Some(by));
        }
        assert_eq!(ExpediteBy::parse("martian"), None);
    }
}
