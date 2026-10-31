//! Bounded capture of an agent attempt's output, and the record of what was
//! cut (`attempt_finished.output_truncated` and `transcript_incomplete`).
//!
//! Each stream the engine reads from an agent is kept twice, both bounded by
//! [`apb_core::output_limits::OutputLimits`]: in memory as a head plus a tail
//! ring ([`Kept`]), which is what the reply, usage and session readers see,
//! and on disk as a capped file (`stdout.log`, `stderr.log`, the streaming
//! transport's log, the session transcript copy). A stream counts as
//! truncated only when neither copy holds all of it.
//!
//! The record every supported CLI prints last carries the verdict and the
//! usage. When a stream was cut and that record is not in the kept copy, the
//! attempt cannot be read and fails with [`INCOMPLETE_RESULT`]; it is never
//! taken for a success.

mod capped_file;
mod head_tail;
mod pipes;
mod view;

pub(crate) use capped_file::CappedFile;
pub use head_tail::Kept;
pub(crate) use pipes::{LineCapture, Pipes, PipesError, StreamCapture, drain_stream};
pub use view::{AttemptOutputLoss, attempt_losses};

use crate::event::OutputLoss;

/// Stream names of [`OutputLoss::stream`].
pub const STREAM_STDOUT: &str = "stdout";
pub const STREAM_STDERR: &str = "stderr";
pub const STREAM_LOG: &str = "stream_log";
pub const STREAM_SESSION_COPY: &str = "session_copy";

/// The failure reason of an attempt whose final record was cut away.
pub const INCOMPLETE_RESULT: &str =
    "the agent's final result could not be read whole: output exceeded the limit";

/// What an attempt lost of its output, for its `attempt_finished`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputCapture {
    /// One entry per stream that was cut.
    pub truncated: Vec<OutputLoss>,
    /// The attempt's final record could not be read whole.
    pub transcript_incomplete: bool,
}

impl OutputCapture {
    /// Adds the loss of one stream, if it lost anything.
    pub fn record(&mut self, loss: Option<OutputLoss>) {
        self.truncated.extend(loss);
    }
}

/// Whether `agent`'s final record is missing from a kept copy that was cut.
/// An uncut copy, or an agent without a machine format, is never reported
/// here: there is nothing to tell a missing record from a plain reply.
pub fn final_record_lost(agent: &str, kept: &Kept, text: &str) -> bool {
    kept.dropped_bytes() > 0 && apb_core::agent_output::has_final_record(agent, text) == Some(false)
}

/// The loss of one stream seen `total` bytes long, of which the best copy
/// kept `kept`: `None` when that copy holds all of it.
fn loss(stream: &str, kept: u64, total: u64) -> Option<OutputLoss> {
    (kept < total).then(|| OutputLoss {
        stream: stream.to_string(),
        kept_bytes: kept,
        dropped_bytes_at_least: total - kept,
    })
}

/// Copies `src` to `dst` up to `cap` bytes, best effort, and returns what
/// the cap kept out as a [`STREAM_SESSION_COPY`] loss.
pub(crate) fn copy_capped(
    src: &std::path::Path,
    dst: &std::path::Path,
    cap: u64,
) -> Option<OutputLoss> {
    use std::io::Read as _;
    let mut input = std::fs::File::open(src).ok()?;
    let mut out = CappedFile::create(dst, cap);
    let mut total = 0u64;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                out.write(&chunk[..n]);
                total += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                // A read that failed partway left the rest uncopied: the
                // source's size says how much there was.
                let size = input.metadata().map_or(0, |m| m.len());
                total = total.max(size);
                break;
            }
        }
    }
    if !out.is_open() {
        return None;
    }
    loss(STREAM_SESSION_COPY, out.written(), total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_copy_over_its_cap_is_cut_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("session.src");
        std::fs::write(&src, "0123456789").unwrap();
        let loss = copy_capped(&src, &dir.path().join("session.jsonl"), 4).unwrap();
        assert_eq!(loss.stream, STREAM_SESSION_COPY);
        assert_eq!((loss.kept_bytes, loss.dropped_bytes_at_least), (4, 6));
        assert_eq!(
            copy_capped(&src, &dir.path().join("whole.jsonl"), 100),
            None
        );
    }

    #[test]
    fn a_lost_final_record_is_not_classified_as_quota_or_transport() {
        assert_eq!(
            crate::failure_class::classify(INCOMPLETE_RESULT),
            crate::failure_class::FailureKind::Agent
        );
    }
}
