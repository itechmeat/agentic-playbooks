//! The top-level `unattended:` block of a playbook: a leaf the schema
//! reads, so the schema does not depend on the readiness rules.

use serde::{Deserialize, Serialize};

/// The most attended successes a probation may ask for (V249).
pub const MAX_PROBATION: u32 = 100;

/// The top-level `unattended:` block: how a playbook may run alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnattendedPolicy {
    /// Attended successful runs of the exact definition, in this project
    /// on this machine, before an unattended start is admitted.
    pub after_attended_successes: u32,
}
