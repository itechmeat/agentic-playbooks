//! One text rendering of a [`PauseInfo`] for the surfaces that print it
//! (`apb runs`, `apb wait`, `apb pause`), so the CLI and the docs agree on
//! the shape: `paused (reason, by operator, since <time>, until <time>)`.

use super::PauseInfo;

fn iso(ms: u128) -> String {
    apb_core::dismiss::iso_utc(u64::try_from(ms).unwrap_or(u64::MAX))
}

impl PauseInfo {
    /// The word a surface shows in place of a status: `paused`, or `pausing`
    /// while a live driver still drains toward a queued pause.
    pub fn word(&self) -> &'static str {
        if self.pausing { "pausing" } else { "paused" }
    }

    /// The parts after the reason, in a fixed order: who, the engine's
    /// cause, how (only for a `pausing` run), since, until, a kept lock.
    pub fn details(&self) -> Vec<String> {
        let mut parts = vec![format!("by {}", self.by.as_str())];
        if let Some(cause) = self.cause {
            parts.push(format!("cause {}", cause.as_str()));
        }
        if self.pausing {
            parts.push(format!("mode {}", self.mode.as_str()));
        }
        if let Some(since) = self.since {
            parts.push(format!("since {}", iso(since)));
        }
        if let Some(until) = self.until {
            parts.push(format!("until {}", iso(u128::from(until))));
        }
        if self.keep_lock {
            parts.push("lock kept".to_string());
        }
        parts
    }

    /// `paused (reason, by operator, since 2026-10-15T20:00:00Z, until ...)`.
    /// The reason is returned as journaled; a terminal caller sanitizes it.
    pub fn line(&self) -> String {
        let mut parts = vec![self.reason.clone()];
        parts.extend(self.details());
        format!("{} ({})", self.word(), parts.join(", "))
    }
}
