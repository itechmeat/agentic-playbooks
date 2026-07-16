use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

/// The three `.apb` subdirectories whose changes the dashboard reacts to.
const WATCHED_SUBDIRS: [&str; 3] = ["playbooks", "profiles", "runs"];

/// How often the global watcher re-scans the project registry to pick up
/// projects (and their `.apb/runs`) that appeared after startup.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Classifies a filesystem event into the change message the frontend listens
/// for. Changes under `.apb/runs` are run updates; everything else is a
/// definition change. We match the adjacent (`.apb`, `runs`) component pair
/// rather than any `runs` component, so a playbook/profile named `runs` or an
/// ancestor directory of that name does not falsely register as a run change.
fn change_message(event: &Event) -> &'static str {
    let is_run = event.paths.iter().any(|p| {
        let comps: Vec<_> = p
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        comps.windows(2).any(|w| w[0] == ".apb" && w[1] == "runs")
    });
    if is_run {
        r#"{"type":"runs_changed"}"#
    } else {
        r#"{"type":"playbooks_changed"}"#
    }
}

/// Project-scoped watcher over a single `<root>/.apb` (test harness / pinned
/// root). Kept for the single-project test server.
pub fn spawn_watcher(
    root: PathBuf,
    tx: broadcast::Sender<String>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // Ignore the send error: no subscribers means nothing to send.
            let _ = tx.send(change_message(&event).to_string());
        }
    })?;
    for sub in WATCHED_SUBDIRS {
        let p = root.join(".apb").join(sub);
        if p.is_dir() {
            watcher.watch(&p, RecursiveMode::Recursive)?;
        }
    }
    Ok(watcher)
}

/// Global watcher for the machine-wide dashboard: watches every reachable
/// project's `.apb/{playbooks,profiles,runs}` and broadcasts a change ping on
/// the shared channel, so run progress and definition edits stream to the UI in
/// real time across all projects. A background thread owns the watcher and
/// re-scans the registry every few seconds, so projects (and runs) that appear
/// after startup start streaming without a server restart. Real-time updates
/// are best-effort: a watch that cannot be established is skipped, never fatal.
pub fn spawn_global_watcher(
    tx: broadcast::Sender<String>,
) -> notify::Result<std::thread::JoinHandle<()>> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let _ = tx.send(change_message(&event).to_string());
        }
    })?;
    let handle = std::thread::spawn(move || {
        let mut watched: HashSet<PathBuf> = HashSet::new();
        loop {
            // The current desired set of existing watch targets.
            let mut desired: HashSet<PathBuf> = HashSet::new();
            for entry in apb_core::projects::list_reachable() {
                let apb = PathBuf::from(&entry.path).join(".apb");
                for sub in WATCHED_SUBDIRS {
                    let p = apb.join(sub);
                    if p.is_dir() {
                        desired.insert(p);
                    }
                }
            }
            // Drop watches for projects/dirs that disappeared, so removed or
            // unreachable projects do not leak file descriptors over time.
            for p in watched.difference(&desired).cloned().collect::<Vec<_>>() {
                let _ = watcher.unwatch(&p);
                watched.remove(&p);
            }
            // Register newly-appeared targets (recursive, as before).
            for p in &desired {
                if !watched.contains(p) && watcher.watch(p, RecursiveMode::Recursive).is_ok() {
                    watched.insert(p.clone());
                }
            }
            std::thread::sleep(RESCAN_INTERVAL);
        }
    });
    Ok(handle)
}
