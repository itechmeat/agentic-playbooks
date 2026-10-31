//! What an agent CLI's machine-readable output says about one attempt: the
//! reply text and the token usage it reports.
//!
//! Each agent module reads its own format (see `crate::agents`, where every
//! shape is documented with the CLI version it was checked against); these
//! are the agent-neutral entry points. Every reader is lenient: an output
//! that does not have the expected shape yields `None`, and the caller keeps
//! the raw stdout as the reply and records no usage.

pub use crate::agents::{AgentUsage, Reply, UsageSource};

/// The reply in `stdout`, `None` when the output is not the machine format
/// of `agent` (see the module docs).
pub fn reply(agent: &str, stdout: &str) -> Option<Reply> {
    crate::agents::lookup(agent)?.output().reply(stdout)
}

/// The token usage in `stdout`, `None` when the output reports none (see
/// the module docs). Never estimated.
pub fn usage(agent: &str, stdout: &str) -> Option<AgentUsage> {
    crate::agents::lookup(agent)?.output().usage(stdout)
}

/// Whether `stdout` ends with the closing record of `agent`'s machine output,
/// the one carrying the verdict and the usage. `None` for an agent without a
/// machine format, where no record can be checked for.
pub fn has_final_record(agent: &str, stdout: &str) -> Option<bool> {
    crate::agents::lookup(agent)?
        .output()
        .has_final_record(stdout)
}
