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
    /// In the concurrency key path, right after `admit` returned `Ok`
    /// (preparation step 4).
    KeyAdmitted,
    /// In `wait_turn`, before each look at the slot.
    KeyBeforeLook,
    /// In health phase 2, after the claim of phase 1 and before the
    /// dispatch check (D2). The argument is `<root>/.apb/health`.
    HealthBeforeDispatch,
    /// In health phase 2, after the call returned and before phase 3 (D2).
    /// The argument is `<root>/.apb/health`.
    HealthBeforeSettle,
    /// In retention `apply`, after the event locks of a family are taken
    /// and before its recheck (D3). The argument is the family root's run
    /// directory.
    PruneFamilyLocked,
    /// In retention `apply`, after the members of a family were moved into
    /// staging and before the commit rename (D3). The argument is the
    /// family root's run directory.
    PruneAfterStage,
    /// In retention `apply`, after the recheck of a family under its event
    /// locks found nothing that keeps it and before its summary and staged
    /// move (D3). The argument is the family root's run directory.
    PruneAfterRecheck,
    /// In a health evaluation, after the facts were read and before the
    /// state transaction (D2). The argument is `<root>/.apb/health`.
    HealthAfterFacts,
    /// In the health facts, after a listed run checked valid and before
    /// its run config and start are read (D2). The argument is the run
    /// directory.
    HealthFactRead,
    /// In the health facts, after a listed run was found gone and before
    /// the fresh view is taken (D2). The argument is the run directory.
    HealthFactGone,
    /// In a health evaluation, before the evaluation lock is taken (D2).
    /// The argument is `<root>/.apb/health`.
    HealthBeforeLock,
    /// In a grade write, after the grades directory was made and before
    /// the grade file is written. The argument is the run directory.
    GradeBeforeWrite,
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
