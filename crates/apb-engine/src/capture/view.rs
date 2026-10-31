//! The run surfaces' view of what attempts lost of their output: one entry
//! per attempt whose `attempt_finished` names a cut stream or an incomplete
//! final record.

use serde::Serialize;

use crate::event::{Event, EventPayload, OutputLoss};

/// One attempt that lost part of its output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptOutputLoss {
    pub node: String,
    pub attempt: u32,
    pub truncated: Vec<OutputLoss>,
    pub transcript_incomplete: bool,
}

impl AttemptOutputLoss {
    /// One line for a terminal: `w attempt 1: stdout kept 5120 bytes, at
    /// least 14980 dropped; final result not read whole`.
    pub fn line(&self) -> String {
        let mut parts: Vec<String> = self
            .truncated
            .iter()
            .map(|l| {
                format!(
                    "{} kept {} bytes, at least {} dropped",
                    l.stream, l.kept_bytes, l.dropped_bytes_at_least
                )
            })
            .collect();
        if self.transcript_incomplete {
            parts.push("final result not read whole".to_string());
        }
        format!(
            "{} attempt {}: {}",
            self.node,
            self.attempt,
            parts.join("; ")
        )
    }
}

/// Every attempt in `events` that lost part of its output, in journal order.
pub fn attempt_losses(events: &[Event]) -> Vec<AttemptOutputLoss> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                attempt,
                output_truncated,
                transcript_incomplete,
                ..
            } if !output_truncated.is_empty() || *transcript_incomplete => {
                Some(AttemptOutputLoss {
                    node: node.clone(),
                    attempt: *attempt,
                    truncated: output_truncated.clone(),
                    transcript_incomplete: *transcript_incomplete,
                })
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_each_cut_stream_and_an_incomplete_result() {
        let loss = AttemptOutputLoss {
            node: "w".into(),
            attempt: 2,
            truncated: vec![OutputLoss {
                stream: "stdout".into(),
                kept_bytes: 5120,
                dropped_bytes_at_least: 14980,
            }],
            transcript_incomplete: true,
        };
        assert_eq!(
            loss.line(),
            "w attempt 2: stdout kept 5120 bytes, at least 14980 dropped; final result not read whole"
        );
    }
}
