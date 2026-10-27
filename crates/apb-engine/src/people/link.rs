//! The link a person follows to a run: its dashboard page, under the
//! machine's public base URL when one is set, else on the loopback
//! dashboard.

use std::path::Path;

/// The dashboard page of run `run_id` of the workspace at `root`; `None`
/// when the workspace has no id yet (it gets one at its first start) or
/// the machine config does not read.
pub fn run_link(root: &Path, run_id: &str) -> Option<String> {
    let cfg = apb_core::config::GlobalConfig::load().ok()?;
    let workspace = apb_core::workspace::read_id(root)?;
    let port = cfg.port.unwrap_or(apb_core::config::DEFAULT_PORT);
    Some(cfg.server.run_url(port, &workspace, run_id))
}
