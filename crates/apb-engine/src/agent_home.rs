//! Where an agent's run-scoped files live (spec 2026-07-21 run-reliability):
//! one directory per agent and node under the run, `agent-home/<agent>/<node>`,
//! which the agent module's `scoped_env` fills (codex's isolated config home,
//! zcode's provider config).

use std::path::{Path, PathBuf};

/// The run-scoped directory of `agent` (a canonical id) for `node` under
/// `run_dir`.
pub(crate) fn scoped_dir(run_dir: &Path, agent: &str, node: &str) -> PathBuf {
    run_dir.join("agent-home").join(agent).join(node)
}
