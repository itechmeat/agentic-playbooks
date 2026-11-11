//! Run retention (D3): `apb runs prune`, a person's command. The plan
//! marks every run keep or remove with its reason; the apply removes
//! whole families under their event locks and leaves a signed summary
//! each, visible before its directories leave, so every reader counts a
//! run once at every moment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use apb_core::policy_file::RetentionSettings;
use apb_core::scope::Scope;
use apb_core::start_key::StartKeysFile;
use serde::Serialize;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};

pub mod archive;

/// One row of the prune plan.
#[derive(Debug, Clone, Serialize)]
pub struct PruneRow {
    pub run_id: String,
    pub playbook: String,
    pub scope: String,
    pub started: String,
    pub outcome: String,
    pub action: PruneAction,
    /// Bytes of the run directory (best effort).
    pub bytes: u64,
}

/// What the plan does with one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", content = "reason", rename_all = "snake_case")]
pub enum PruneAction {
    Remove,
    Keep(KeepReason),
}

/// Why a run is kept; the first rule that holds is the reason shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    Unsafe,
    Unreadable,
    Live,
    Newest,
    LatestSuccess,
    FollowUpPending,
    Graded,
    CandidateTrial,
    UnknownSend,
    StartKey,
    ConcurrencyKey,
    KeptChild,
    Young,
}

impl KeepReason {
    /// The words a person reads in the plan.
    pub fn as_str(self) -> &'static str {
        match self {
            KeepReason::Unsafe => "unsafe",
            KeepReason::Unreadable => "unreadable",
            KeepReason::Live => "live",
            KeepReason::Newest => "newest",
            KeepReason::LatestSuccess => "latest success",
            KeepReason::FollowUpPending => "follow-up pending",
            KeepReason::Graded => "graded",
            KeepReason::CandidateTrial => "candidate trial",
            KeepReason::UnknownSend => "unknown send",
            KeepReason::StartKey => "start key",
            KeepReason::ConcurrencyKey => "concurrency key",
            KeepReason::KeptChild => "kept child",
            KeepReason::Young => "young",
        }
    }
}

/// What an apply did.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub removed: usize,
    pub kept: usize,
    pub bytes: u64,
    pub rows: Vec<PruneRow>,
}

const DAY_MS: u64 = 86_400_000;

/// One run directory as the plan read it.
#[derive(Debug, Clone)]
pub(crate) struct RunEntry {
    pub id: String,
    pub dir: PathBuf,
    /// `None`: the journal does not read strictly (rule 1).
    pub events: Option<Vec<Event>>,
    pub playbook: String,
    pub scope: String,
    pub version: String,
    pub started_ms: u64,
    /// `succeeded`, `failed`, `aborted`, or empty while the run has not
    /// ended.
    pub outcome: String,
    pub terminal: bool,
    /// The run config's `parent_run`.
    pub parent: Option<String>,
    /// The children its journal names, in journal order.
    pub children: Vec<String>,
}

/// Whether `path` is a real directory (not a link).
fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Reads one listed directory. The journal is read with the strict
/// reader; everything else is best effort.
pub(crate) fn read_entry(id: &str, dir: &Path) -> RunEntry {
    let events = crate::event::read_all(dir).ok();
    let head = crate::run_history::started(dir);
    let mut entry = RunEntry {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        events: None,
        playbook: head
            .as_ref()
            .map(|h| h.playbook.clone())
            .unwrap_or_default(),
        scope: "project".into(),
        version: head.as_ref().map(|h| h.version.clone()).unwrap_or_default(),
        started_ms: head
            .as_ref()
            .map(|h| u64::try_from(h.ts).unwrap_or(u64::MAX))
            .unwrap_or(0),
        outcome: String::new(),
        terminal: false,
        parent: crate::run_config::read_run_config(dir)
            .ok()
            .and_then(|c| c.parent_run),
        children: Vec::new(),
    };
    let Some(events) = events else {
        return entry;
    };
    for e in &events {
        match &e.payload {
            EventPayload::RunProvenance {
                origin: Some(origin),
                ..
            } => entry.scope = origin.clone(),
            EventPayload::ChildRunStarted { run_id, .. } if !entry.children.contains(run_id) => {
                entry.children.push(run_id.clone())
            }
            _ => {}
        }
    }
    let status = crate::state::RunState::fold(&events).run_status;
    entry.terminal = status.is_terminal();
    entry.outcome = if entry.terminal {
        match status {
            crate::state::RunStatus::Succeeded => "succeeded",
            crate::state::RunStatus::Failed => "failed",
            _ => "aborted",
        }
        .to_string()
    } else {
        String::new()
    };
    entry.events = Some(events);
    entry
}

/// What a per-playbook store says about the runs it names: every run of
/// the playbook (a file that does not verify), or the named ones.
#[derive(Debug, Clone)]
enum Named {
    All,
    Ids(BTreeSet<String>),
}

impl Named {
    fn names(&self, id: &str) -> bool {
        match self {
            Named::All => true,
            Named::Ids(ids) => ids.contains(id),
        }
    }
}

/// The run ids a live start-key entry of `playbook` names.
fn start_key_names(root: &Path, scope: Scope, playbook: &str, now_ms: u64) -> Named {
    let days = apb_core::policy_file::Policies::load(root)
        .map(|p| p.start_key_days())
        .unwrap_or(365);
    let present = apb_core::signed_store::file::<StartKeysFile>(root, scope, playbook).exists();
    match apb_core::signed_store::read::<StartKeysFile>(root, scope, playbook) {
        Ok(l) if l.signed => Named::Ids(
            l.file
                .entries
                .iter()
                .filter(|e| e.at_ms.saturating_add(u64::from(days) * DAY_MS) >= now_ms)
                .filter_map(|e| e.run_id.clone())
                .collect(),
        ),
        _ if !present => Named::Ids(BTreeSet::new()),
        _ => Named::All,
    }
}

/// The run ids a slot of `playbook` names: active, waiting, or owed a stop.
fn slot_names(root: &Path, scope: Scope, playbook: &str) -> Named {
    use crate::concurrency::ConcurrencyFile;
    let present = apb_core::signed_store::file::<ConcurrencyFile>(root, scope, playbook).exists();
    match apb_core::signed_store::read::<ConcurrencyFile>(root, scope, playbook) {
        Ok(l) if l.signed => Named::Ids(
            l.file
                .slots
                .iter()
                .flat_map(|s| {
                    s.active
                        .iter()
                        .chain(s.waiting.iter())
                        .map(|h| h.run_id.clone())
                        .chain(s.stops.iter().map(|p| p.run_id.clone()))
                })
                .collect(),
        ),
        _ if !present => Named::Ids(BTreeSet::new()),
        _ => Named::All,
    }
}

/// The facts the rules read that are shared by the runs of one playbook.
struct PlaybookFacts {
    /// The newest `keep_last` runs.
    newest: BTreeSet<String>,
    /// The latest production success; `None` with `all_successes` when it
    /// cannot be told.
    latest_success: Option<String>,
    all_successes: bool,
}

/// Rules 1, 2 and 5 to 10 of one run: the rules `apply` checks again
/// under the family's locks.
pub(crate) fn recheck_reason(root: &Path, run: &RunEntry, now_ms: u64) -> Option<KeepReason> {
    let Some(events) = &run.events else {
        return Some(KeepReason::Unreadable);
    };
    if !run.terminal || crate::liveness::driver_alive(&run.dir, &run.id) == Some(true) {
        return Some(KeepReason::Live);
    }
    let manifest = crate::manifest::read(&run.dir);
    let origin = crate::manifest::RunOrigin::of_read(&manifest);
    if matches!(
        crate::follow_up::state(
            crate::legacy_snapshot::load_run_playbook(&run.dir).as_ref(),
            events,
            origin.runs_follow_up(),
        ),
        crate::follow_up::FollowUpState::Pending { .. }
    ) {
        return Some(KeepReason::FollowUpPending);
    }
    if std::fs::read_dir(run.dir.join(crate::grade::store::GRADES_DIR))
        .is_ok_and(|mut rd| rd.next().is_some())
    {
        return Some(KeepReason::Graded);
    }
    if let Ok(Some(m)) = &manifest
        && m.candidate_trial
        && apb_core::candidate_pointer::read_candidate(&apb_core::candidate::playbook_dir(
            root,
            &run.playbook,
        ))
        .as_deref()
            == Some(run.version.as_str())
    {
        return Some(KeepReason::CandidateTrial);
    }
    if crate::run_outcome::intents::intents_from(events, now_ms)
        .iter()
        .any(|i| i.is_open_unknown())
    {
        return Some(KeepReason::UnknownSend);
    }
    let scope = Scope::parse(&run.scope).unwrap_or(Scope::Project);
    if apb_core::registry::is_safe_segment(&run.playbook) {
        if start_key_names(root, scope, &run.playbook, now_ms).names(&run.id) {
            return Some(KeepReason::StartKey);
        }
        if slot_names(root, scope, &run.playbook).names(&run.id) {
            return Some(KeepReason::ConcurrencyKey);
        }
    }
    None
}

/// Rules 1 to 10 of one run, the shared facts read once per playbook.
fn own_reason(
    root: &Path,
    run: &RunEntry,
    facts: &PlaybookFacts,
    now_ms: u64,
) -> Option<KeepReason> {
    if run.events.is_none() {
        return Some(KeepReason::Unreadable);
    }
    if !run.terminal || crate::liveness::driver_alive(&run.dir, &run.id) == Some(true) {
        return Some(KeepReason::Live);
    }
    if facts.newest.contains(&run.id) {
        return Some(KeepReason::Newest);
    }
    if facts.latest_success.as_deref() == Some(run.id.as_str())
        || (facts.all_successes && run.outcome == "succeeded")
    {
        return Some(KeepReason::LatestSuccess);
    }
    recheck_reason(root, run, now_ms)
}

/// Rule 12.
fn young(run: &RunEntry, settings: &RetentionSettings, now_ms: u64) -> bool {
    let days = if run.outcome == "succeeded" {
        settings.keep_days
    } else {
        settings.keep_failed_days
    };
    run.started_ms.saturating_add(u64::from(days) * DAY_MS) > now_ms
}

/// The bytes under `dir`, links not followed (best effort).
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                total += m.len();
            }
        }
    }
    total
}

/// One family: the top-level run first, then its descendants depth first.
#[derive(Debug, Clone)]
pub(crate) struct Family {
    pub members: Vec<String>,
    /// A member names a child that is not a safe, listed run directory of
    /// its own whose `parent_run` names it: the whole family is kept.
    pub unsafe_parent: Option<String>,
}

/// The listed entries: the readable run directories by id, and the names
/// that are not (links, files, unsafe names).
pub(crate) fn list_runs(root: &Path) -> (BTreeMap<String, RunEntry>, Vec<String>) {
    let dir = archive::runs_dir(root);
    let mut runs = BTreeMap::new();
    let mut unsafe_names = Vec::new();
    if !real_dir(&dir) {
        return (runs, unsafe_names);
    }
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return (runs, unsafe_names);
    };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = dir.join(&name);
        let contained = apb_core::fsutil::ensure_no_symlink_below(&dir, &path).is_ok();
        if !apb_core::registry::is_safe_segment(&name) || !real_dir(&path) || !contained {
            unsafe_names.push(name);
            continue;
        }
        runs.insert(name.clone(), read_entry(&name, &path));
    }
    unsafe_names.sort();
    (runs, unsafe_names)
}

/// The families of the listed runs, and the runs no family reaches (a
/// child whose parent is gone or does not name it).
pub(crate) fn families(runs: &BTreeMap<String, RunEntry>) -> (Vec<Family>, Vec<String>) {
    let mut by_parent: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for r in runs.values() {
        if let Some(p) = &r.parent {
            by_parent.entry(p.as_str()).or_default().push(r.id.as_str());
        }
    }
    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for top in runs.values().filter(|r| r.parent.is_none()) {
        let mut fam = Family {
            members: Vec::new(),
            unsafe_parent: None,
        };
        let mut stack = vec![top.id.clone()];
        while let Some(id) = stack.pop() {
            if reached.contains(&id) {
                continue;
            }
            reached.insert(id.clone());
            fam.members.push(id.clone());
            let run = &runs[&id];
            let mut kids: Vec<String> = Vec::new();
            for c in &run.children {
                let ok = apb_core::registry::is_safe_segment(c)
                    && runs
                        .get(c)
                        .is_some_and(|k| k.parent.as_deref() == Some(id.as_str()));
                if !ok {
                    fam.unsafe_parent.get_or_insert_with(|| id.clone());
                    continue;
                }
                kids.push(c.clone());
            }
            for k in by_parent.get(id.as_str()).into_iter().flatten() {
                if !kids.iter().any(|c| c == k) {
                    kids.push((*k).to_string());
                }
            }
            // Depth first in journal order: pushed reversed.
            for k in kids.into_iter().rev() {
                stack.push(k);
            }
        }
        out.push(fam);
    }
    let orphans = runs
        .keys()
        .filter(|id| !reached.contains(*id))
        .cloned()
        .collect();
    (out, orphans)
}

fn row_of(run: &RunEntry, action: PruneAction) -> PruneRow {
    PruneRow {
        run_id: run.id.clone(),
        playbook: run.playbook.clone(),
        scope: run.scope.clone(),
        started: if run.started_ms > 0 {
            apb_core::clock::iso_utc(run.started_ms)
        } else {
            String::new()
        },
        outcome: run.outcome.clone(),
        action,
        bytes: dir_bytes(&run.dir),
    }
}

fn unsafe_row(name: &str) -> PruneRow {
    PruneRow {
        run_id: name.to_string(),
        playbook: String::new(),
        scope: String::new(),
        started: String::new(),
        outcome: String::new(),
        action: PruneAction::Keep(KeepReason::Unsafe),
        bytes: 0,
    }
}

/// The shared facts of every playbook the listed runs belong to.
fn playbook_facts(
    root: &Path,
    runs: &BTreeMap<String, RunEntry>,
    settings: &RetentionSettings,
) -> BTreeMap<(String, String), PlaybookFacts> {
    let mut groups: BTreeMap<(String, String), Vec<&RunEntry>> = BTreeMap::new();
    for r in runs
        .values()
        .filter(|r| r.events.is_some() && !r.playbook.is_empty())
    {
        groups
            .entry((r.playbook.clone(), r.scope.clone()))
            .or_default()
            .push(r);
    }
    groups
        .into_iter()
        .map(|((playbook, scope), mut members)| {
            members.sort_by(|a, b| (b.started_ms, &b.id).cmp(&(a.started_ms, &a.id)));
            let newest = members
                .iter()
                .take(settings.keep_last as usize)
                .map(|r| r.id.clone())
                .collect();
            let (latest_success, all_successes) =
                match crate::previous_success::find(root, &playbook, &scope) {
                    Ok(dir) => (
                        dir.and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned())),
                        false,
                    ),
                    Err(_) => (None, true),
                };
            (
                (playbook, scope),
                PlaybookFacts {
                    newest,
                    latest_success,
                    all_successes,
                },
            )
        })
        .collect()
}

/// The plan over the listed runs: one action per run, families whole.
pub(crate) fn plan_listed(
    root: &Path,
    runs: &BTreeMap<String, RunEntry>,
    settings: &RetentionSettings,
    now_ms: u64,
) -> (Vec<Family>, BTreeMap<String, PruneAction>, Vec<String>) {
    let facts = playbook_facts(root, runs, settings);
    let empty = PlaybookFacts {
        newest: BTreeSet::new(),
        latest_success: None,
        all_successes: false,
    };
    let (families, orphans) = families(runs);
    let mut actions: BTreeMap<String, PruneAction> = BTreeMap::new();
    for fam in &families {
        let mut reasons: BTreeMap<&str, Option<KeepReason>> = BTreeMap::new();
        // Deepest first: a member is kept by its own rules 1 to 10, by a
        // kept descendant (11), or by its age (12).
        for id in fam.members.iter().rev() {
            let run = &runs[id];
            let f = facts
                .get(&(run.playbook.clone(), run.scope.clone()))
                .unwrap_or(&empty);
            let own = if fam.unsafe_parent.as_deref() == Some(id.as_str()) {
                Some(KeepReason::Unsafe)
            } else {
                own_reason(root, run, f, now_ms)
            };
            let child_kept = fam
                .members
                .iter()
                .filter(|m| runs[*m].parent.as_deref() == Some(id.as_str()))
                .any(|m| reasons.get(m.as_str()).is_some_and(Option::is_some));
            let reason = own
                .or(child_kept.then_some(KeepReason::KeptChild))
                .or(young(run, settings, now_ms).then_some(KeepReason::Young));
            reasons.insert(id.as_str(), reason);
        }
        let kept = reasons.values().any(Option::is_some);
        for id in &fam.members {
            let action = match reasons[id.as_str()] {
                Some(r) => PruneAction::Keep(r),
                None if kept => PruneAction::Keep(KeepReason::KeptChild),
                None => PruneAction::Remove,
            };
            actions.insert(id.clone(), action);
        }
    }
    for id in &orphans {
        actions.insert(id.clone(), PruneAction::Keep(KeepReason::Unsafe));
    }
    (families, actions, orphans)
}

/// The abort code of an ended run, from a fixed list, never the reason's
/// text: a reason a secret or a param value reached is `other`.
pub fn abort_code(reason: &str, by: Option<crate::pause::kind::PauseBy>) -> &'static str {
    if by == Some(crate::pause::kind::PauseBy::Operator) {
        return "operator_stop";
    }
    if reason.starts_with(crate::run_deadline::DEADLINE_EXCEEDED) {
        return "deadline_exceeded";
    }
    if reason
        .strip_prefix(apb_core::concurrency::SUPERSEDED)
        .is_some_and(|rest| rest.starts_with(':'))
    {
        return "superseded";
    }
    if reason == crate::stop::PARENT_ABORTED_REASON {
        return "parent_aborted";
    }
    "other"
}

/// The summary row of one run, read while its journal is still there:
/// the class comes from [`apb_core::health::class_of`] over the terminal
/// event, never from the abort code; no reason text is kept.
pub(crate) fn archived_run(run: &RunEntry) -> archive::ArchivedRun {
    let events = run.events.as_deref().unwrap_or_default();
    let end = crate::health::facts::ended(events);
    let (ended_ms, by, reason, by_word) = (end.ts, end.by, end.reason, end.by_word());
    let manifest = crate::manifest::read(&run.dir);
    let origin = match crate::manifest::RunOrigin::of_read(&manifest) {
        crate::manifest::RunOrigin::Production => "production",
        crate::manifest::RunOrigin::ExternalData => "external_data",
        crate::manifest::RunOrigin::Eval => "eval",
        crate::manifest::RunOrigin::Scenario { .. } => "scenario",
        crate::manifest::RunOrigin::Unknown => "unknown",
    };
    let execution = match manifest.ok().flatten() {
        None => Some(
            apb_core::execution::ExecutionMode::default()
                .as_str()
                .to_string(),
        ),
        Some(m) => m.execution_mode().ok().map(|x| x.as_str().to_string()),
    };
    let usage = crate::run_view::RunUsage::totals(events);
    let active_ms = ended_ms.map(|end| {
        let end = u128::from(end);
        crate::run_stats::spans::active_ms(
            &crate::pause::clock::paused_intervals(events, end),
            u128::from(run.started_ms),
            end,
        )
    });
    let aborted = run.outcome == "aborted";
    // The class the live health facts give the run: a run they never
    // count (`concurrency::checked_run` finds no origin stamp of this
    // installation or no `run_provenance`) is `ignored`, so its summary
    // never adds a fact the directory did not.
    let attested = apb_core::run_origin::verify(&run.dir, &run.id)
        && events.iter().any(|e| {
            matches!(
                &e.payload,
                EventPayload::RunProvenance {
                    origin: Some(_),
                    ..
                }
            )
        });
    let class = if attested {
        end.class()
    } else {
        apb_core::health::RunClass::Ignored
    };
    archive::ArchivedRun {
        run_id: run.id.clone(),
        playbook: run.playbook.clone(),
        scope: run.scope.clone(),
        version: run.version.clone(),
        origin: origin.to_string(),
        parent: run.parent.clone(),
        started_ms: run.started_ms,
        ended_ms,
        outcome: run.outcome.clone(),
        class: class.as_str().to_string(),
        aborted_by: aborted.then(|| by_word.map(str::to_string)).flatten(),
        abort_code: aborted.then(|| abort_code(reason.unwrap_or(""), by).to_string()),
        active_ms,
        tokens: (usage.attempts > 0).then(|| usage.billable_tokens()),
        cost_usd: usage.cost_usd,
        execution,
    }
}

/// The plan with what `apply` needs to act on it.
struct FullPlan {
    runs: BTreeMap<String, RunEntry>,
    families: Vec<Family>,
    rows: Vec<PruneRow>,
}

fn full_plan(
    root: &Path,
    settings: &RetentionSettings,
    only: Option<&str>,
    now_ms: u64,
) -> FullPlan {
    let archive_ok = archive::archive_state(root).is_ok();
    let (runs, unsafe_names) = list_runs(root);
    let (families, actions, orphans) = plan_listed(root, &runs, settings, now_ms);
    let mut rows = Vec::new();
    let mut push = |run: &RunEntry| {
        let action = if archive_ok {
            actions[&run.id].clone()
        } else {
            PruneAction::Keep(KeepReason::Unsafe)
        };
        rows.push(row_of(run, action));
    };
    let families: Vec<Family> = families
        .into_iter()
        .filter(|f| only.is_none_or(|p| runs[&f.members[0]].playbook == p))
        .collect();
    for fam in &families {
        for id in &fam.members {
            push(&runs[id]);
        }
    }
    for id in &orphans {
        if only.is_none_or(|p| runs[id].playbook == p) {
            push(&runs[id]);
        }
    }
    if only.is_none() {
        rows.extend(unsafe_names.iter().map(|n| unsafe_row(n)));
    }
    FullPlan {
        runs,
        families,
        rows,
    }
}

/// The plan: every run of the project (or of the families whose top-level
/// run is of `only`) with its action. Writes nothing.
pub fn plan(
    root: &Path,
    settings: &RetentionSettings,
    only: Option<&str>,
    now_ms: u64,
) -> Result<Vec<PruneRow>, EngineError> {
    Ok(full_plan(root, settings, only, now_ms).rows)
}

/// The event locks of the latest successes of the members' playbooks,
/// taken while each is still the latest success, so no resume reopens one
/// while the family leaves. `Err` names the members that must stay: one
/// is the latest success itself, or the latest success cannot be told or
/// held (then every succeeded member stays, as the plan keeps them).
fn latest_success_held(
    root: &Path,
    entries: &[RunEntry],
) -> Result<Vec<apb_core::fsutil::DirLock>, Vec<String>> {
    let members: BTreeSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    let groups: BTreeSet<(&str, &str)> = entries
        .iter()
        .filter(|e| !e.playbook.is_empty())
        .map(|e| (e.playbook.as_str(), e.scope.as_str()))
        .collect();
    let succeeded = |playbook: &str, scope: &str| -> Vec<String> {
        entries
            .iter()
            .filter(|e| e.playbook == playbook && e.scope == scope && e.outcome == "succeeded")
            .map(|e| e.id.clone())
            .collect()
    };
    let name_of = |d: &Path| {
        d.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut locks = Vec::new();
    let mut keep: Vec<String> = Vec::new();
    for (playbook, scope) in groups {
        let latest = match crate::previous_success::find(root, playbook, scope) {
            Ok(Some(dir)) => dir,
            Ok(None) => continue,
            Err(_) => {
                keep.extend(succeeded(playbook, scope));
                continue;
            }
        };
        let name = name_of(&latest);
        if members.contains(name.as_str()) {
            keep.push(name);
            continue;
        }
        // Held, then asked again: the answer under its lock stands until
        // the commit, because reopening it takes that lock.
        match apb_core::fsutil::lock_dir(&latest, crate::stop::EVENT_LOCK) {
            Ok(lock) => match crate::previous_success::find(root, playbook, scope) {
                Ok(Some(again)) if again == latest => locks.push(lock),
                Ok(Some(again)) if members.contains(name_of(&again).as_str()) => {
                    keep.push(name_of(&again));
                }
                // No success left among the runs: none to protect.
                Ok(None) => {}
                // Another run became the latest meanwhile: kept this time,
                // the next prune plans the family again.
                Ok(Some(_)) | Err(_) => keep.extend(succeeded(playbook, scope)),
            },
            Err(_) => keep.extend(succeeded(playbook, scope)),
        }
    }
    if keep.is_empty() {
        Ok(locks)
    } else {
        Err(keep)
    }
}

/// Removes one family the plan removes: its event locks, the recheck of
/// rules 1, 2 and 5 to 10 of every member under them, then the summary
/// and the staged move under the archive lock, the commit rename, and the
/// deletion outside every lock. `Ok(Some(reasons))` when the recheck
/// keeps the family.
fn remove_family(
    root: &Path,
    fam: &Family,
    now_ms: u64,
) -> Result<Option<BTreeMap<String, KeepReason>>, EngineError> {
    let runs_dir = archive::runs_dir(root);
    let adir = archive::archive_dir(root);
    let mut event_locks = Vec::new();
    for id in &fam.members {
        let dir = runs_dir.join(id);
        apb_core::fsutil::ensure_no_symlink_below(&runs_dir, &dir)?;
        event_locks.push(apb_core::fsutil::lock_dir(&dir, crate::stop::EVENT_LOCK)?);
    }
    #[cfg(any(test, feature = "testing"))]
    let top_dir = runs_dir.join(&fam.members[0]);
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(crate::deadline_hooks::Hook::PruneFamilyLocked, &top_dir);
    let entries: Vec<RunEntry> = fam
        .members
        .iter()
        .map(|id| read_entry(id, &runs_dir.join(id)))
        .collect();
    let mut kept: BTreeMap<String, KeepReason> = entries
        .iter()
        .filter_map(|e| recheck_reason(root, e, now_ms).map(|r| (e.id.clone(), r)))
        .collect();
    // The membership again: a member resumed and ended since the plan may
    // have started a child the plan never saw, which would be left behind
    // with no parent. The family is kept; the next prune plans it whole.
    for e in &entries {
        if e.children.iter().any(|c| !fam.members.contains(c)) {
            kept.entry(e.id.clone()).or_insert(KeepReason::KeptChild);
        }
    }
    // The latest success again, held: a newer success resumed since the
    // plan makes an older member the latest one.
    let success_locks = match latest_success_held(root, &entries) {
        Ok(locks) => locks,
        Err(ids) => {
            for id in ids {
                kept.entry(id).or_insert(KeepReason::LatestSuccess);
            }
            Vec::new()
        }
    };
    if !kept.is_empty() {
        return Ok(Some(kept));
    }
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(crate::deadline_hooks::Hook::PruneAfterRecheck, &top_dir);
    let family = archive::ArchivedFamily {
        format: 1,
        root: fam.members[0].clone(),
        runs: entries.iter().map(archived_run).collect(),
        mac: String::new(),
    };
    let removed = adir.join(format!(".removed-{}", family.root));
    {
        let _archive = apb_core::fsutil::lock_dir(&adir, archive::ARCHIVE_LOCK)?;
        archive::write_summary(root, &family)?;
        let staging = adir.join(format!(".removing-{}", family.root));
        apb_core::fsutil::create_dir_under(&adir, &staging)?;
        for id in fam.members.iter().rev() {
            std::fs::rename(runs_dir.join(id), staging.join(id))?;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::deadline_hooks::at(crate::deadline_hooks::Hook::PruneAfterStage, &top_dir);
        std::fs::rename(&staging, &removed)?;
    }
    drop(success_locks);
    drop(event_locks);
    std::fs::remove_dir_all(&removed)?;
    Ok(None)
}

/// Removes every family the plan removes, each rechecked under its locks;
/// one prune at a time, after the recovery of an interrupted one.
pub fn apply(
    root: &Path,
    settings: &RetentionSettings,
    only: Option<&str>,
    now_ms: u64,
) -> Result<PruneReport, EngineError> {
    let adir = archive::archive_dir(root);
    archive::archive_state(root)?;
    if !real_dir(&root.join(".apb")) {
        return Ok(PruneReport::default());
    }
    apb_core::fsutil::create_dir_under(root, &adir)?;
    archive::archive_state(root)?;
    let _prune = match apb_core::fsutil::try_hold_lock(&adir, archive::PRUNE_LOCK) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(EngineError::Conflict(
                "prune_in_progress: another apb runs prune is running in this project".into(),
            ));
        }
        Err(e) => return Err(e.into()),
    };
    archive::recover(root)?;
    let plan = full_plan(root, settings, only, now_ms);
    let mut rows = plan.rows;
    let mut report = PruneReport::default();
    for fam in &plan.families {
        let all_remove = fam.members.iter().all(|id| {
            rows.iter()
                .any(|r| &r.run_id == id && r.action == PruneAction::Remove)
        });
        if !all_remove {
            continue;
        }
        let bytes: u64 = fam
            .members
            .iter()
            .filter_map(|id| plan.runs.get(id).map(|r| dir_bytes(&r.dir)))
            .sum();
        match remove_family(root, fam, now_ms)? {
            None => {
                report.removed += fam.members.len();
                report.bytes += bytes;
            }
            Some(kept) => {
                for r in rows.iter_mut().filter(|r| fam.members.contains(&r.run_id)) {
                    r.action = PruneAction::Keep(
                        kept.get(&r.run_id)
                            .copied()
                            .unwrap_or(KeepReason::KeptChild),
                    );
                }
            }
        }
    }
    report.kept = rows.len() - report.removed;
    report.rows = rows;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const DAY: u64 = 86_400_000;
    /// The time every seeded run is judged at: day 100.
    const NOW: u64 = 100 * DAY;

    fn settings() -> RetentionSettings {
        RetentionSettings {
            keep_days: 30,
            keep_last: 1,
            keep_failed_days: 90,
        }
    }

    struct Seed<'a> {
        id: &'a str,
        playbook: &'a str,
        day: u64,
        end: Option<&'a str>,
        parent: Option<&'a str>,
        children: &'a [&'a str],
    }

    fn seed(root: &Path, s: Seed<'_>) -> PathBuf {
        let dir = root.join(".apb/runs").join(s.id);
        fs::create_dir_all(&dir).unwrap();
        apb_core::run_origin::ensure_key().unwrap();
        apb_core::run_origin::stamp(&dir, s.id).unwrap();
        let ts = s.day * DAY;
        let mut lines = vec![
            serde_json::json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": s.playbook, "version": "1.0.0"}),
            serde_json::json!({"seq": 2, "ts": ts, "type": "run_provenance", "origin": "project"}),
        ];
        for (i, c) in s.children.iter().enumerate() {
            lines.push(serde_json::json!({"seq": 3 + i as u64, "ts": ts, "type": "child_run_started", "node_id": "sub", "run_id": c}));
        }
        let next = 3 + s.children.len() as u64;
        match s.end {
            Some("aborted") => lines.push(serde_json::json!({"seq": next, "ts": ts + 1, "type": "run_aborted", "reason": "stop requested", "by": "operator"})),
            Some(outcome) => lines.push(serde_json::json!({"seq": next, "ts": ts + 1, "type": "run_finished", "outcome": outcome})),
            None => {}
        }
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(dir.join("events.jsonl"), text).unwrap();
        crate::run_config::write_run_config(
            &dir,
            &crate::run_config::RunConfig {
                parent_run: s.parent.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap();
        dir
    }

    fn run(root: &Path, id: &str, day: u64, end: &str) -> PathBuf {
        seed(
            root,
            Seed {
                id,
                playbook: "p",
                day,
                end: Some(end),
                parent: None,
                children: &[],
            },
        )
    }

    fn plan_at(root: &Path) -> Vec<PruneRow> {
        plan(root, &settings(), None, NOW).unwrap()
    }

    fn action(rows: &[PruneRow], id: &str) -> PruneAction {
        rows.iter()
            .find(|r| r.run_id == id)
            .unwrap_or_else(|| panic!("no row {id}: {rows:?}"))
            .action
            .clone()
    }

    fn keep(r: KeepReason) -> PruneAction {
        PruneAction::Keep(r)
    }

    /// Two old runs and a new success: the old success can go.
    fn base(root: &Path) {
        run(root, "p-1", 10, "succeeded");
        run(root, "p-9", 99, "succeeded");
    }

    #[test]
    fn abort_code_maps_each_known_reason() {
        assert_eq!(
            abort_code(
                &format!("{}: 2h passed", crate::run_deadline::DEADLINE_EXCEEDED),
                Some(crate::pause::kind::PauseBy::Engine)
            ),
            "deadline_exceeded"
        );
        assert_eq!(
            abort_code(
                "superseded: run p-2 started for key k",
                Some(crate::pause::kind::PauseBy::Engine)
            ),
            "superseded"
        );
        assert_eq!(
            abort_code(
                "stop requested",
                Some(crate::pause::kind::PauseBy::Operator)
            ),
            "operator_stop"
        );
        assert_eq!(
            abort_code(
                crate::stop::PARENT_ABORTED_REASON,
                Some(crate::pause::kind::PauseBy::Engine)
            ),
            "parent_aborted"
        );
        assert_eq!(
            abort_code("superseded", Some(crate::pause::kind::PauseBy::Engine)),
            "other"
        );
    }

    #[test]
    fn a_secret_bearing_reason_is_other() {
        assert_eq!(
            abort_code(
                "token sk-live-QQQ999 rejected",
                Some(crate::pause::kind::PauseBy::Engine)
            ),
            "other"
        );
        assert_eq!(abort_code("acct-7781 failed", None), "other");
    }

    #[test]
    fn keeps_an_unreadable_run() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let d = run(t.path(), "p-2", 11, "succeeded");
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(d.join("events.jsonl"))
            .unwrap();
        std::io::Write::write_all(&mut f, b"{\"seq\": 9, \"ty").unwrap();
        assert_eq!(
            action(&plan_at(t.path()), "p-2"),
            keep(KeepReason::Unreadable)
        );
    }

    #[test]
    fn keeps_a_live_run() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        seed(
            t.path(),
            Seed {
                id: "p-2",
                playbook: "p",
                day: 11,
                end: None,
                parent: None,
                children: &[],
            },
        );
        assert_eq!(action(&plan_at(t.path()), "p-2"), keep(KeepReason::Live));
    }

    #[test]
    fn keeps_the_newest_runs() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        run(t.path(), "p-99", 99, "failed");
        // p-99 started on the same day as p-9 but later in the listing:
        // the newest of p is the one with the latest start.
        let rows = plan_at(t.path());
        assert!(matches!(action(&rows, "p-9"), PruneAction::Keep(_)));
        assert_eq!(action(&rows, "p-1"), PruneAction::Remove);
    }

    #[test]
    fn keeps_the_latest_success() {
        let t = tempfile::tempdir().unwrap();
        run(t.path(), "p-1", 10, "succeeded");
        run(t.path(), "p-2", 11, "succeeded");
        run(t.path(), "p-3", 12, "failed");
        let rows = plan_at(t.path());
        assert_eq!(action(&rows, "p-3"), keep(KeepReason::Newest));
        assert_eq!(action(&rows, "p-2"), keep(KeepReason::LatestSuccess));
        assert_eq!(action(&rows, "p-1"), PruneAction::Remove);
    }

    #[test]
    fn keeps_a_pending_follow_up() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let d = run(t.path(), "p-2", 11, "succeeded");
        crate::run_config::snapshot_playbook(
            &d,
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nfollow_up:\n  after: 1m\n  check:\n    id: c\n    description: d\n    script: scripts/c.sh\n    observe:\n      - { id: s, type: field, path: s, expect: { equals: won } }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        assert_eq!(
            action(&plan_at(t.path()), "p-2"),
            keep(KeepReason::FollowUpPending)
        );
    }

    #[test]
    fn keeps_a_graded_run() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let d = run(t.path(), "p-2", 11, "succeeded");
        fs::create_dir_all(d.join(crate::grade::store::GRADES_DIR)).unwrap();
        fs::write(d.join(crate::grade::store::GRADES_DIR).join("g.json"), "{}").unwrap();
        assert_eq!(action(&plan_at(t.path()), "p-2"), keep(KeepReason::Graded));
    }

    #[test]
    fn keeps_an_open_candidate_trial() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let d = run(t.path(), "p-2", 11, "succeeded");
        let m = crate::manifest::RunExecutionManifest {
            candidate_trial: true,
            ..Default::default()
        };
        crate::manifest::write(&d, &m).unwrap();
        let pdir = apb_core::candidate::playbook_dir(t.path(), "p");
        fs::create_dir_all(&pdir).unwrap();
        fs::write(
            pdir.join(apb_core::candidate_pointer::CANDIDATE_FILE),
            "1.0.0",
        )
        .unwrap();
        assert_eq!(
            action(&plan_at(t.path()), "p-2"),
            keep(KeepReason::CandidateTrial)
        );
        fs::write(
            pdir.join(apb_core::candidate_pointer::CANDIDATE_FILE),
            "1.1.0",
        )
        .unwrap();
        assert_eq!(action(&plan_at(t.path()), "p-2"), PruneAction::Remove);
    }

    #[test]
    fn keeps_an_unknown_send() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let d = run(t.path(), "p-2", 11, "succeeded");
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(d.join("events.jsonl"))
            .unwrap();
        let rec = serde_json::json!({"seq": 10, "ts": 11 * DAY + 5, "type": "send_intent_recorded", "intent": "si-1", "nonce": "n", "node": "work", "connector": "c", "function": "f", "account": "a", "mode": "key", "label": "l"});
        let set = serde_json::json!({"seq": 11, "ts": 11 * DAY + 6, "type": "send_intent_settled", "intent": "si-1", "outcome": "unknown", "source": "response"});
        std::io::Write::write_all(&mut f, format!("{rec}\n{set}\n").as_bytes()).unwrap();
        assert_eq!(
            action(&plan_at(t.path()), "p-2"),
            keep(KeepReason::UnknownSend)
        );
    }

    #[test]
    fn keeps_a_start_key_run() {
        use apb_core::start_key::{EntryState, StartKeyEntry, StartKeysFile};
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        run(t.path(), "p-2", 11, "failed");
        apb_core::signed_store::transact(
            t.path(),
            apb_core::scope::Scope::Project,
            "p",
            apb_core::signed_store::Unsigned::Refuse,
            |f: &mut StartKeysFile| {
                f.entries.push(StartKeyEntry {
                    key: "k".into(),
                    request: "r".into(),
                    state: EntryState::Bound,
                    reservation: None,
                    run_id: Some("p-2".into()),
                    at_ms: NOW - DAY,
                    reused: 0,
                });
                Ok::<_, apb_core::signed_store::StoreError>(((), true))
            },
        )
        .unwrap();
        assert_eq!(
            action(&plan_at(t.path()), "p-2"),
            keep(KeepReason::StartKey)
        );
    }

    #[test]
    fn keeps_a_concurrency_run() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        run(t.path(), "p-2", 11, "succeeded");
        apb_core::signed_store::transact(
            t.path(),
            apb_core::scope::Scope::Project,
            "p",
            apb_core::signed_store::Unsigned::Refuse,
            |f: &mut crate::concurrency::ConcurrencyFile| {
                f.slots.push(crate::concurrency::Slot {
                    key_digest: "d".into(),
                    display: "k".into(),
                    active: None,
                    waiting: None,
                    stops: vec![crate::concurrency::PendingStop {
                        run_id: "p-2".into(),
                        reason: "superseded: x".into(),
                    }],
                });
                Ok::<_, apb_core::signed_store::StoreError>(((), true))
            },
        )
        .unwrap();
        assert_eq!(
            action(&plan_at(t.path()), "p-2"),
            keep(KeepReason::ConcurrencyKey)
        );
    }

    fn family(root: &Path) {
        seed(
            root,
            Seed {
                id: "p-2",
                playbook: "p",
                day: 11,
                end: Some("succeeded"),
                parent: None,
                children: &["k-1"],
            },
        );
        seed(
            root,
            Seed {
                id: "k-1",
                playbook: "k",
                day: 11,
                end: Some("succeeded"),
                parent: Some("p-2"),
                children: &["g-1"],
            },
        );
        seed(
            root,
            Seed {
                id: "g-1",
                playbook: "g",
                day: 11,
                end: Some("succeeded"),
                parent: Some("k-1"),
                children: &[],
            },
        );
        // Newer runs of k and g, so the family's own are not the newest.
        run(root, "k-9", 99, "succeeded");
        run(root, "g-9", 99, "succeeded");
        fs::write(
            root.join(".apb/runs/k-9/events.jsonl"),
            format!("{}\n{}\n{}\n",
                serde_json::json!({"seq": 1, "ts": 99 * DAY, "type": "run_started", "playbook": "k", "version": "1.0.0"}),
                serde_json::json!({"seq": 2, "ts": 99 * DAY, "type": "run_provenance", "origin": "project"}),
                serde_json::json!({"seq": 3, "ts": 99 * DAY, "type": "run_finished", "outcome": "succeeded"})),
        ).unwrap();
        fs::write(
            root.join(".apb/runs/g-9/events.jsonl"),
            format!("{}\n{}\n{}\n",
                serde_json::json!({"seq": 1, "ts": 99 * DAY, "type": "run_started", "playbook": "g", "version": "1.0.0"}),
                serde_json::json!({"seq": 2, "ts": 99 * DAY, "type": "run_provenance", "origin": "project"}),
                serde_json::json!({"seq": 3, "ts": 99 * DAY, "type": "run_finished", "outcome": "succeeded"})),
        ).unwrap();
    }

    #[test]
    fn keeps_every_ancestor_of_a_kept_grandchild() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        family(t.path());
        let g = t
            .path()
            .join(".apb/runs/g-1")
            .join(crate::grade::store::GRADES_DIR);
        fs::create_dir_all(&g).unwrap();
        fs::write(g.join("x.json"), "{}").unwrap();
        let rows = plan_at(t.path());
        assert_eq!(action(&rows, "g-1"), keep(KeepReason::Graded));
        assert_eq!(action(&rows, "k-1"), keep(KeepReason::KeptChild));
        assert_eq!(action(&rows, "p-2"), keep(KeepReason::KeptChild));
    }

    #[test]
    fn keeps_a_young_run() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        run(t.path(), "p-2", 80, "succeeded");
        run(t.path(), "p-3", 20, "failed");
        let rows = plan_at(t.path());
        assert_eq!(action(&rows, "p-2"), keep(KeepReason::Young));
        assert_eq!(action(&rows, "p-3"), keep(KeepReason::Young));
    }

    #[test]
    fn removes_an_old_success() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let rows = plan_at(t.path());
        assert_eq!(action(&rows, "p-1"), PruneAction::Remove);
        let row = rows.iter().find(|r| r.run_id == "p-1").unwrap();
        assert_eq!(
            (row.playbook.as_str(), row.scope.as_str()),
            ("p", "project")
        );
        assert_eq!(row.outcome, "succeeded");
        assert!(row.started.starts_with("1970-01-11"), "{}", row.started);
    }

    #[test]
    fn removes_a_parent_with_its_children() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        family(t.path());
        let rows = plan_at(t.path());
        for id in ["p-2", "k-1", "g-1"] {
            assert_eq!(action(&rows, id), PruneAction::Remove, "{id}");
        }
    }

    #[test]
    fn marks_a_linked_entry_unsafe() {
        let t = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        base(t.path());
        std::os::unix::fs::symlink(outside.path(), t.path().join(".apb/runs/p-5")).unwrap();
        assert_eq!(action(&plan_at(t.path()), "p-5"), keep(KeepReason::Unsafe));
    }

    #[test]
    fn marks_a_file_entry_unsafe() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        fs::write(t.path().join(".apb/runs/p-5"), "x").unwrap();
        assert_eq!(action(&plan_at(t.path()), "p-5"), keep(KeepReason::Unsafe));
    }

    #[test]
    fn marks_a_parent_with_an_unsafe_child_id_unsafe() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        seed(
            t.path(),
            Seed {
                id: "p-2",
                playbook: "p",
                day: 11,
                end: Some("succeeded"),
                parent: None,
                children: &["../../etc"],
            },
        );
        assert_eq!(action(&plan_at(t.path()), "p-2"), keep(KeepReason::Unsafe));
    }

    #[test]
    fn marks_a_parent_whose_child_names_another_parent_unsafe() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        seed(
            t.path(),
            Seed {
                id: "p-2",
                playbook: "p",
                day: 11,
                end: Some("succeeded"),
                parent: None,
                children: &["k-1"],
            },
        );
        seed(
            t.path(),
            Seed {
                id: "p-3",
                playbook: "p",
                day: 12,
                end: Some("succeeded"),
                parent: None,
                children: &[],
            },
        );
        seed(
            t.path(),
            Seed {
                id: "k-1",
                playbook: "k",
                day: 11,
                end: Some("succeeded"),
                parent: Some("p-3"),
                children: &[],
            },
        );
        assert_eq!(action(&plan_at(t.path()), "p-2"), keep(KeepReason::Unsafe));
    }

    fn archived_ids(root: &Path) -> Vec<String> {
        archive::runs_view(root)
            .unwrap()
            .archived
            .into_iter()
            .map(|a| a.run_id)
            .collect()
    }

    #[test]
    fn apply_removes_a_family_and_leaves_one_summary() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        family(t.path());
        let report = apply(t.path(), &settings(), None, NOW).unwrap();
        assert_eq!(report.removed, 4, "{:?}", report.rows);
        for id in ["p-1", "p-2", "k-1", "g-1"] {
            assert!(!t.path().join(".apb/runs").join(id).exists(), "{id}");
        }
        let fam = archive::read_verified_locked(t.path());
        let roots: Vec<&str> = fam.iter().map(|f| f.root.as_str()).collect();
        assert_eq!(roots, ["p-1", "p-2"]);
        let p2 = fam.iter().find(|f| f.root == "p-2").unwrap();
        let ids: Vec<&str> = p2.runs.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["p-2", "k-1", "g-1"]);
        assert_eq!(p2.runs[1].parent.as_deref(), Some("p-2"));
        assert_eq!(p2.runs[0].class, "succeeded");
        let staging: Vec<String> = fs::read_dir(t.path().join(".apb/runs-archive"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".remov"))
            .collect();
        assert!(staging.is_empty(), "{staging:?}");
    }

    #[test]
    fn recover_finishes_a_committed_removal() {
        let t = tempfile::tempdir().unwrap();
        let done = t.path().join(".apb/runs-archive/.removed-p-1/p-1");
        fs::create_dir_all(&done).unwrap();
        fs::write(done.join("events.jsonl"), "x").unwrap();
        archive::recover(t.path()).unwrap();
        assert!(!t.path().join(".apb/runs-archive/.removed-p-1").exists());
    }

    #[test]
    fn recover_undoes_a_staged_family() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        family(t.path());
        let fam = archive::ArchivedFamily {
            format: 1,
            root: "p-2".into(),
            runs: Vec::new(),
            mac: String::new(),
        };
        archive::write_summary(t.path(), &fam).unwrap();
        let staging = t.path().join(".apb/runs-archive/.removing-p-2");
        fs::create_dir_all(&staging).unwrap();
        for id in ["g-1", "k-1"] {
            fs::rename(t.path().join(".apb/runs").join(id), staging.join(id)).unwrap();
        }
        archive::recover(t.path()).unwrap();
        assert!(!staging.exists());
        for id in ["p-2", "k-1", "g-1"] {
            assert!(t.path().join(".apb/runs").join(id).is_dir(), "{id}");
        }
        assert!(!t.path().join(".apb/runs-archive/p-2.json").exists());
    }

    #[test]
    fn recover_deletes_a_summary_whose_runs_are_still_there() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        family(t.path());
        let mut fam = archive::ArchivedFamily {
            format: 1,
            root: "p-2".into(),
            runs: Vec::new(),
            mac: String::new(),
        };
        for id in ["p-2", "k-1", "g-1", "gone-1"] {
            let mut r = super::archived_run(&read_entry(id, &t.path().join(".apb/runs").join(id)));
            r.run_id = id.into();
            fam.runs.push(r);
        }
        archive::write_summary(t.path(), &fam).unwrap();
        assert!(archived_ids(t.path()).contains(&"gone-1".to_string()));
        archive::recover(t.path()).unwrap();
        assert!(!t.path().join(".apb/runs-archive/p-2.json").exists());
        assert!(archived_ids(t.path()).is_empty());
    }

    #[test]
    fn a_held_prune_lock_refuses_apply() {
        let t = tempfile::tempdir().unwrap();
        base(t.path());
        let dir = t.path().join(".apb/runs-archive");
        fs::create_dir_all(&dir).unwrap();
        let _held = apb_core::fsutil::try_hold_lock(&dir, archive::PRUNE_LOCK).unwrap();
        let e = apply(t.path(), &settings(), None, NOW).unwrap_err();
        assert!(e.to_string().contains("prune_in_progress"), "{e}");
        assert!(t.path().join(".apb/runs/p-1").is_dir());
    }
}
