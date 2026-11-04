//! The execution mode lines of `apb runs` (0.24.0): who executes a run's
//! agent steps, read from the run manifest's `execution` block, plus the
//! nodes the journal records as having fallen back to the host.

use std::path::Path;

/// The `execution:` line of `apb runs <id>`: `host (argument, client x)`,
/// `cli`, and for a `cli` run with the host fallback the nodes that actually
/// fell back (`execution_fallback` events in the journal).
pub(crate) fn detail_line(run_dir: &Path, events: &[apb_engine::event::Event]) -> String {
    let manifest = apb_engine::manifest::read(run_dir).ok().flatten();
    let mut line = match manifest.as_ref() {
        None => "cli".to_string(),
        Some(m) => {
            let mut attrs = Vec::new();
            if let Some(e) = m.execution.as_ref() {
                if let Some(src) = &e.source {
                    attrs.push(src.clone());
                }
                if let Some(client) = &e.client {
                    attrs.push(format!("client {client}"));
                }
                if e.fallback_to_host {
                    attrs.push("host fallback allowed".to_string());
                }
            }
            // The internal `+guard` suffix an old apb refuses on shows as
            // its own attribute, never inside the mode.
            if m.is_guarded() {
                attrs.push("guarded".to_string());
            }
            if attrs.is_empty() {
                m.base_mode().to_string()
            } else {
                format!("{} ({})", m.base_mode(), attrs.join(", "))
            }
        }
    };
    // The same list `run_status` carries as `execution.fell_back`.
    let fell_back = apb_engine::run_view::fell_back_nodes(events);
    if !fell_back.is_empty() {
        line.push_str(&format!("; host fallback: {}", fell_back.join(", ")));
    }
    line
}

/// The mode column of the `apb runs` table: the manifest's base mode (the
/// guard suffix stays internal), `cli` when the run has no execution
/// block. `None` when the run has no execution block, so the caller adds
/// the column only when some run has one.
pub(crate) fn list_mode(root: &Path, run_id: &str) -> Option<String> {
    let dir = root.join(".apb/runs").join(run_id);
    let m = apb_engine::manifest::read(&dir).ok().flatten()?;
    let exec = m.execution.as_ref()?;
    let mode = m.base_mode();
    Some(if exec.fallback_to_host {
        format!("{mode}+host-fallback")
    } else {
        mode.to_string()
    })
}
