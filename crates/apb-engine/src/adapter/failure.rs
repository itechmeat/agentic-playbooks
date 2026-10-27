//! How an agent attempt failed: the failure class the drive loop routes on,
//! the message, and what the failed output still reported.

use crate::capture::OutputCapture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Transport,
    ProcessExit,
    StructuredOutputMissing,
    AgentReportedFailure,
    Timeout,
}

/// Why an agent attempt failed, and the token usage its output reported
/// anyway.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentFailure {
    pub class: ErrorClass,
    pub message: String,
    /// The usage the agent's machine output carried although the attempt
    /// failed: claude prints its errored result, usage included, and exits
    /// non-zero. `None` when the failed output reported none. The drive loop
    /// writes it into the failed `AttemptFinished.usage`.
    pub usage: Option<apb_core::agent_output::AgentUsage>,
    /// Whether the failed attempt shows a model turn: it reported token
    /// usage, or printed a reply that is not an error result. The host
    /// fallback treats an auth-looking failure as "cannot start" only
    /// without one, so text such as a `git push` 403 printed after real work
    /// never re-runs the step as a host task.
    pub model_turn: bool,
    /// What the attempt lost of its output (bounded agent output).
    pub capture: OutputCapture,
}

impl AgentFailure {
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            usage: None,
            model_turn: false,
            capture: OutputCapture::default(),
        }
    }

    /// The same failure, with what the attempt lost of its output.
    #[must_use]
    pub fn with_capture(mut self, capture: OutputCapture) -> Self {
        self.capture = capture;
        self
    }

    /// The same failure, marked as having had a model turn.
    #[must_use]
    pub fn after_model_turn(mut self, turn: bool) -> Self {
        self.model_turn = turn;
        self
    }

    /// The same failure, with the usage the output reported.
    #[must_use]
    pub fn with_usage(mut self, usage: Option<apb_core::agent_output::AgentUsage>) -> Self {
        self.usage = usage;
        self
    }
}

impl From<(ErrorClass, String)> for AgentFailure {
    fn from((class, message): (ErrorClass, String)) -> Self {
        Self::new(class, message)
    }
}
