//! Barriers the deadline race tests hold the engine at (test builds only).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Where a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Hook {
    /// In the stop path, before a `due` stop takes the event lock (its
    /// caller already read an expired journal).
    StopBeforeLock,
    /// In the stop path, after a `due` stop was decided under the event
    /// lock and before it is posted.
    StopDecided,
    /// In a resume, before its final check under the event lock.
    BeforeResumeCommit,
    /// In the run's deadline watch, after it fired and before its stop.
    WatchBeforeStop,
    /// In a `playbook` node, after the child was prepared and before
    /// `child_run_started` is appended.
    ChildPrepared,
}

type Action = Arc<dyn Fn(&Path) + Send + Sync>;

static HOOKS: Mutex<BTreeMap<Hook, Vec<(PathBuf, Action)>>> = Mutex::new(BTreeMap::new());

/// Runs `f` at `hook` for every run directory under `scope` (a project
/// root or a run directory) until [`clear`].
pub fn set(hook: Hook, scope: &Path, f: impl Fn(&Path) + Send + Sync + 'static) {
    if let Ok(mut map) = HOOKS.lock() {
        map.entry(hook)
            .or_default()
            .push((scope.to_path_buf(), Arc::new(f)));
    }
}

/// Removes every hook set for `scope`.
pub fn clear(scope: &Path) {
    if let Ok(mut map) = HOOKS.lock() {
        for v in map.values_mut() {
            v.retain(|(s, _)| s != scope);
        }
    }
}

/// Runs the hooks of `hook` whose scope holds `run_dir`.
pub(crate) fn at(hook: Hook, run_dir: &Path) {
    let actions: Vec<Action> = HOOKS
        .lock()
        .map(|map| {
            map.get(&hook)
                .map(|v| {
                    v.iter()
                        .filter(|(s, _)| run_dir.starts_with(s))
                        .map(|(_, a)| Arc::clone(a))
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    for a in actions {
        a(run_dir);
    }
}
