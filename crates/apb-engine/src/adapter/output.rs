//! What an attempt's bounded output capture means for its outcome: the
//! losses it records, and the failures it causes (see `crate::capture`).

use std::time::Duration;

use super::failure::{AgentFailure, ErrorClass};
use crate::capture::{
    INCOMPLETE_RESULT, OutputCapture, PipesError, STREAM_STDERR, STREAM_STDOUT, StreamCapture,
    final_record_lost,
};

/// The capture record of an attempt: the loss of its stdout (named
/// `stdout_name`, the stream log on the streaming transport) and of its
/// stderr, and whether the final record of `agent` was cut from stdout.
pub(super) fn stream_capture(
    agent: &str,
    stdout: &StreamCapture,
    stdout_name: &str,
    stderr: Option<&StreamCapture>,
) -> OutputCapture {
    let mut capture = OutputCapture::default();
    capture.record(stdout.loss(stdout_name));
    capture.record(stderr.and_then(|s| s.loss(STREAM_STDERR)));
    capture.transcript_incomplete = final_record_lost(agent, &stdout.kept, &stdout.text());
    capture
}

/// [`stream_capture`] of the headless transport.
pub(super) fn headless_capture(
    agent: &str,
    stdout: &StreamCapture,
    stderr: &StreamCapture,
) -> OutputCapture {
    stream_capture(agent, stdout, STREAM_STDOUT, Some(stderr))
}

/// The failure of an attempt whose final record was cut away. Classified
/// as missing structured output: neither a quota nor a transport problem,
/// and nothing a retry on the same output would read differently.
pub(super) fn incomplete_failure(capture: OutputCapture) -> AgentFailure {
    AgentFailure::new(ErrorClass::StructuredOutputMissing, INCOMPLETE_RESULT).with_capture(capture)
}

/// The failure of an attempt whose exited agent's streams could not be
/// collected.
pub(super) fn pipes_failure(program: &str, budget: Duration, err: PipesError) -> AgentFailure {
    match err {
        PipesError::HeldOpen => AgentFailure::new(
            ErrorClass::Timeout,
            format!(
                "`{program}` exited but its stdout/stderr were still held open {budget:?} later, \
                 so its output could not be collected: a descendant that outlived it inherited \
                 the pipes"
            ),
        ),
        PipesError::Wait(e) => AgentFailure::new(
            ErrorClass::ProcessExit,
            format!("collect `{program}` output failed: {e}"),
        ),
    }
}
