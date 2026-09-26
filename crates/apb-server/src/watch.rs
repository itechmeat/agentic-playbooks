use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use notify::event::{AccessKind, AccessMode, EventKind};
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

/// Whether an event may have changed what the dashboard shows. notify also
/// reports opens and read-closes (`Access`), and the dashboard's own GETs
/// cause those: broadcasting them made every open list or playbook view
/// reload, read again and reload again, twice a second, forever. A write that
/// closes (`Access(Close(Write))`) still counts.
fn is_change(event: &Event) -> bool {
    match event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Project-scoped watcher over a single `<root>/.apb` (test harness / pinned
/// root). Kept for the single-project test server.
pub fn spawn_watcher(
    root: PathBuf,
    tx: broadcast::Sender<String>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res
            && is_change(&event)
        {
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

/// The global config store entries the machine-wide dashboard shows:
/// global profiles, installed connectors and connector accounts (watched
/// recursively), plus the files directly in `config_dir` whose change is
/// visible in the UI (trust decisions, the agent config).
const GLOBAL_SUBDIRS: [&str; 3] = ["profiles", "connectors", "connector-config"];
const GLOBAL_FILES: [&str; 2] = ["trust.json", "config.yaml"];

const CONFIG_CHANGED: &str = r#"{"type":"config_changed"}"#;

/// The message for an event under the global config dir `cfg`, or `None`
/// when it touches nothing the dashboard shows (the server's own lock,
/// `projects.json`, detection caches, temp files of an atomic write).
fn config_message(event: &Event, cfg: &std::path::Path) -> Option<&'static str> {
    let relevant = event.paths.iter().any(|p| {
        let Ok(rel) = p.strip_prefix(cfg) else {
            return false;
        };
        let mut comps = rel.components();
        let Some(first) = comps.next() else {
            return false;
        };
        let first = first.as_os_str();
        let nested = comps.next().is_some();
        if nested {
            GLOBAL_SUBDIRS.iter().any(|d| first == *d)
        } else {
            GLOBAL_SUBDIRS
                .iter()
                .chain(GLOBAL_FILES.iter())
                .any(|f| first == *f)
        }
    });
    relevant.then_some(CONFIG_CHANGED)
}

/// The running global watcher. Dropping it stops the rescan thread (within
/// one rescan interval) and releases every watch.
pub struct GlobalWatcher {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for GlobalWatcher {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Global watcher for the machine-wide dashboard: watches every reachable
/// project's `.apb/{playbooks,profiles,runs}` and the global config store
/// (`profiles/`, `connectors/`, `connector-config/`, `trust.json`,
/// `config.yaml`) and broadcasts a change ping on the shared channel, so run
/// progress and definition, profile, connector and trust edits made from any
/// surface stream to the UI in real time. A background thread owns the
/// watcher and re-scans the registry every few seconds, so projects (and
/// runs) that appear after startup start streaming without a server restart.
/// Real-time updates are best-effort: a watch that cannot be established is
/// skipped, never fatal.
pub fn spawn_global_watcher(tx: broadcast::Sender<String>) -> notify::Result<GlobalWatcher> {
    let cfg = apb_core::config::config_dir();
    let cfg_for_events = cfg.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res
            && is_change(&event)
        {
            let msg = match &cfg_for_events {
                Some(c) if event.paths.iter().any(|p| p.starts_with(c)) => {
                    config_message(&event, c)
                }
                _ => Some(change_message(&event)),
            };
            if let Some(msg) = msg {
                let _ = tx.send(msg.to_string());
            }
        }
    })?;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = stop.clone();
    std::thread::spawn(move || {
        let mut watched: HashSet<(PathBuf, bool)> = HashSet::new();
        while !stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
            // The current desired set of existing watch targets, each with
            // whether it is watched recursively.
            let mut desired: HashSet<(PathBuf, bool)> = HashSet::new();
            for entry in apb_core::projects::list_reachable() {
                let apb = PathBuf::from(&entry.path).join(".apb");
                for sub in WATCHED_SUBDIRS {
                    let p = apb.join(sub);
                    if p.is_dir() {
                        desired.insert((p, true));
                    }
                }
            }
            if let Some(c) = &cfg {
                if c.is_dir() {
                    // Non-recursive: the files directly in config_dir, and a
                    // watched subdir appearing for the first time.
                    desired.insert((c.clone(), false));
                }
                for sub in GLOBAL_SUBDIRS {
                    let p = c.join(sub);
                    if p.is_dir() {
                        desired.insert((p, true));
                    }
                }
            }
            // Drop watches for projects/dirs that disappeared, so removed or
            // unreachable projects do not leak file descriptors over time.
            for t in watched.difference(&desired).cloned().collect::<Vec<_>>() {
                let _ = watcher.unwatch(&t.0);
                watched.remove(&t);
            }
            // Register newly-appeared targets.
            for t in &desired {
                let mode = if t.1 {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };
                if !watched.contains(t) && watcher.watch(&t.0, mode).is_ok() {
                    watched.insert(t.clone());
                }
            }
            std::thread::sleep(RESCAN_INTERVAL);
        }
    });
    Ok(GlobalWatcher { stop })
}
