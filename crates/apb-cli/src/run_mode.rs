//! The execution mode lines of `apb runs` (0.24.0): who executes a run's
//! agent steps, read from the run manifest's `execution` block, plus the
//! nodes the journal records as having fallen back to the host.

use std::path::Path;

use apb_engine::event::EventPayload;

/// The `execution:` line of `apb runs <id>`: `host (argument, client x)`,
/// `cli`, and for a `cli` run with the host fallback the nodes that actually
/// fell back (`execution_fallback` events in the journal).
pub(crate) fn detail_line(run_dir: &Path, events: &[apb_engine::event::Event]) -> String {
    let manifest = apb_engine::manifest::read(run_dir).ok().flatten();
    let exec = manifest.as_ref().and_then(|m| m.execution.as_ref());
    let mut line = match exec {
        None => "cli".to_string(),
        Some(e) => {
            let mut attrs = Vec::new();
            if let Some(src) = &e.source {
                attrs.push(src.clone());
            }
            if let Some(client) = &e.client {
                attrs.push(format!("client {client}"));
            }
            if e.fallback_to_host {
                attrs.push("host fallback allowed".to_string());
            }
            if attrs.is_empty() {
                e.mode.clone()
            } else {
                format!("{} ({})", e.mode, attrs.join(", "))
            }
        }
    };
    let mut fell_back: Vec<&str> = Vec::new();
    for e in events {
        if let EventPayload::ExecutionFallback { node, .. } = &e.payload
            && !fell_back.contains(&node.as_str())
        {
            fell_back.push(node);
        }
    }
    if !fell_back.is_empty() {
        line.push_str(&format!("; host fallback: {}", fell_back.join(", ")));
    }
    line
}

/// The mode column of the `apb runs` table: the manifest's mode, `cli` when
/// the run has no execution block. `None` when the run has no execution
/// block, so the caller adds the column only when some run has one.
pub(crate) fn list_mode(root: &Path, run_id: &str) -> Option<String> {
    let dir = root.join(".apb/runs").join(run_id);
    let exec = apb_engine::manifest::read(&dir).ok().flatten()?.execution?;
    Some(if exec.fallback_to_host {
        format!("{}+host-fallback", exec.mode)
    } else {
        exec.mode
    })
}
