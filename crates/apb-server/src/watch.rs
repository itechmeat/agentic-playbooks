use std::path::PathBuf;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

pub fn spawn_watcher(
    root: PathBuf,
    tx: broadcast::Sender<String>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // We pick the message type from the paths: changes under
            // .apb/runs are runs. Important: we do not look for any "runs"
            // component (that would falsely trigger on a playbook/profile
            // named "runs" or on a project root that lives inside an
            // ancestor directory named "runs"), but specifically an adjacent
            // pair of components (".apb", "runs") - this unambiguously
            // points at the .apb/runs subdirectory and is robust to the
            // absolute prefix and path canonicalization (e.g. /var vs
            // /private/var on macOS).
            let is_run = event.paths.iter().any(|p| {
                let comps: Vec<_> = p
                    .components()
                    .map(|c| c.as_os_str().to_os_string())
                    .collect();
                comps.windows(2).any(|w| w[0] == ".apb" && w[1] == "runs")
            });
            let msg = if is_run {
                r#"{"type":"runs_changed"}"#
            } else {
                r#"{"type":"playbooks_changed"}"#
            };
            // Ignore the send error: no subscribers means nothing to send.
            let _ = tx.send(msg.to_string());
        }
    })?;
    for sub in ["playbooks", "profiles", "runs"] {
        let p = root.join(".apb").join(sub);
        if p.is_dir() {
            watcher.watch(&p, RecursiveMode::Recursive)?;
        }
    }
    Ok(watcher)
}
