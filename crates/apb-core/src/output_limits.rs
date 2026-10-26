//! Size limits on what apb keeps of an agent attempt's output.
//!
//! An agent CLI can print far more than anyone needs to keep (a runaway tool
//! loop, a dumped file, a verbose stream). apb keeps a bounded copy of every
//! stream instead of all of it, and says what it dropped. The record that
//! carries an attempt's verdict and usage is printed LAST by every supported
//! CLI, so the in-memory copy of a stream keeps its beginning (the head, for
//! session ids) and its end (the tail, for the final record), and drops the
//! middle.
//!
//! The limits live in the `output_limits:` section of the global
//! `config.yaml`. A project `.apb/config.yaml` may carry the same section to
//! LOWER any of them for its own runs; a project value larger than the
//! machine's is ignored, so a checked-out repository can never make apb keep
//! more than its owner allowed (`crate::config::GlobalConfig::output_limits_for`
//! resolves both).
//!
//! ```yaml
//! output_limits:
//!   head_bytes: 2097152          # in memory, per stream: the beginning kept
//!   tail_bytes: 6291456          # in memory, per stream: the end kept
//!   log_file_bytes: 67108864     # stdout.log and stderr.log, each
//!   stream_log_bytes: 67108864   # the streaming transport's event log
//!   session_copy_bytes: 67108864 # the copied agent session transcript
//!   stream_line_queue: 1024      # lines buffered between the reader and the engine
//! ```

use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;

/// Default bytes kept from the beginning of each stream in memory.
pub const DEFAULT_HEAD_BYTES: u64 = 2 * MIB;
/// Default bytes kept from the end of each stream in memory. Also bounds
/// the streaming transport: a line kept whole is shorter than this (its
/// newline must fit too), and the lines waiting between its reader and the
/// engine never hold more bytes than this.
pub const DEFAULT_TAIL_BYTES: u64 = 6 * MIB;
/// Default size cap of `stdout.log` and of `stderr.log`, each.
pub const DEFAULT_LOG_FILE_BYTES: u64 = 64 * MIB;
/// Default size cap of the streaming transport's event log.
pub const DEFAULT_STREAM_LOG_BYTES: u64 = 64 * MIB;
/// Default size cap of the copied agent session transcript.
pub const DEFAULT_SESSION_COPY_BYTES: u64 = 64 * MIB;
/// Default number of lines buffered between the streaming transport's
/// reader and the engine (within the `tail_bytes` byte bound).
pub const DEFAULT_STREAM_LINE_QUEUE: u64 = 1024;

/// The resolved limits one run applies to every agent attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLimits {
    pub head_bytes: u64,
    pub tail_bytes: u64,
    pub log_file_bytes: u64,
    pub stream_log_bytes: u64,
    pub session_copy_bytes: u64,
    pub stream_line_queue: u64,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            head_bytes: DEFAULT_HEAD_BYTES,
            tail_bytes: DEFAULT_TAIL_BYTES,
            log_file_bytes: DEFAULT_LOG_FILE_BYTES,
            stream_log_bytes: DEFAULT_STREAM_LOG_BYTES,
            session_copy_bytes: DEFAULT_SESSION_COPY_BYTES,
            stream_line_queue: DEFAULT_STREAM_LINE_QUEUE,
        }
    }
}

/// The `output_limits:` section as written, in the global or the project
/// config. Every key is optional: an absent key keeps the value below it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputLimitSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_log_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_copy_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_line_queue: Option<u64>,
}

impl OutputLimitSettings {
    /// Each key with its value, in declaration order.
    fn entries(&self) -> [(&'static str, Option<u64>); 6] {
        [
            ("head_bytes", self.head_bytes),
            ("tail_bytes", self.tail_bytes),
            ("log_file_bytes", self.log_file_bytes),
            ("stream_log_bytes", self.stream_log_bytes),
            ("session_copy_bytes", self.session_copy_bytes),
            ("stream_line_queue", self.stream_line_queue),
        ]
    }

    /// Every limit must be positive: a zero would keep nothing at all, which
    /// is never what a smaller number was meant to say.
    pub fn validate(&self) -> Result<(), String> {
        match self.entries().iter().find(|(_, v)| *v == Some(0)) {
            Some((key, _)) => Err(format!("output_limits.{key} must be positive")),
            None => Ok(()),
        }
    }
}

impl OutputLimits {
    /// The machine's limits: the defaults with the global section applied.
    pub fn from_global(global: &OutputLimitSettings) -> Self {
        let d = Self::default();
        Self {
            head_bytes: global.head_bytes.unwrap_or(d.head_bytes),
            tail_bytes: global.tail_bytes.unwrap_or(d.tail_bytes),
            log_file_bytes: global.log_file_bytes.unwrap_or(d.log_file_bytes),
            stream_log_bytes: global.stream_log_bytes.unwrap_or(d.stream_log_bytes),
            session_copy_bytes: global.session_copy_bytes.unwrap_or(d.session_copy_bytes),
            stream_line_queue: global.stream_line_queue.unwrap_or(d.stream_line_queue),
        }
    }

    /// These limits lowered by a project section. A project value only ever
    /// lowers a limit; a larger one is ignored.
    #[must_use]
    pub fn lowered_by(self, project: &OutputLimitSettings) -> Self {
        let low = |cur: u64, v: Option<u64>| v.map_or(cur, |v| v.min(cur));
        Self {
            head_bytes: low(self.head_bytes, project.head_bytes),
            tail_bytes: low(self.tail_bytes, project.tail_bytes),
            log_file_bytes: low(self.log_file_bytes, project.log_file_bytes),
            stream_log_bytes: low(self.stream_log_bytes, project.stream_log_bytes),
            session_copy_bytes: low(self.session_copy_bytes, project.session_copy_bytes),
            stream_line_queue: low(self.stream_line_queue, project.stream_line_queue),
        }
    }
}

/// `n` as a `usize`, saturating on a narrower platform.
pub fn as_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_lowers_a_limit_but_never_raises_one() {
        let machine = OutputLimits::from_global(&OutputLimitSettings {
            tail_bytes: Some(100),
            ..Default::default()
        });
        let project = OutputLimitSettings {
            head_bytes: Some(10),
            tail_bytes: Some(1_000),
            ..Default::default()
        };
        let got = machine.lowered_by(&project);
        assert_eq!(got.head_bytes, 10);
        assert_eq!(got.tail_bytes, 100);
        assert_eq!(got.log_file_bytes, DEFAULT_LOG_FILE_BYTES);
    }

    #[test]
    fn a_zero_limit_is_a_configuration_error() {
        let s = OutputLimitSettings {
            log_file_bytes: Some(0),
            ..Default::default()
        };
        assert_eq!(
            s.validate().unwrap_err(),
            "output_limits.log_file_bytes must be positive"
        );
    }
}
