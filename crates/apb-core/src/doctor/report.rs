//! The environment doctor's report: one check per line, and whether any
//! of them blocks.

/// Result of a single environment check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Default, Clone)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub(super) fn push(
        &mut self,
        status: CheckStatus,
        name: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.checks.push(Check {
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }

    /// Whether at least one check failed (for the CLI exit code).
    pub fn has_failure(&self) -> bool {
        self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }
}
