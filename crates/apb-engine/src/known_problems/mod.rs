//! Known problems in the engine (see [`apb_core::known_problems`] for the
//! block, the types, the store and the recheck rule):
//!
//! - [`observe`]: at the end of every run, what it saw goes into its
//!   playbook's store (a run is counted once however often it is observed);
//! - [`at_start`]: what a starting run shows, rechecked where the source
//!   changed, journaled once as `known_problems_shown`;
//! - [`prompt`]: the attempt prompt section and the supervisor line built
//!   from that event;
//! - [`view`], [`decide`], [`recheck_one`] and [`scan`]: what the surfaces
//!   show and what a person (or a host, or a model on request) does.
//!
//! Nothing here calls a model except a recheck, and only when the
//! project's `uses.problem_recheck` is on.

mod extract;
pub mod prompt;
pub mod recheck;
pub mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::known_problems::recheck::{ProposedVerdict, RecheckProposal, quote_shape};
use apb_core::known_problems::store::{self, PersonDecision, ProblemsError};
use apb_core::known_problems::{
    Decided, HiddenProblem, KnownProblem, KnownProblemsBlock, OBSERVED_RUNS_KEPT, ProblemStatus,
    RecheckBasis, RecheckRecord, RecheckVerdict, ScopeKind, ShownProblem, hidden,
};
use apb_core::schema::{NodeKind, Playbook};
use apb_core::scope::Scope;
use apb_core::scrub::Scrubber;
use serde::Serialize;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::manifest::RunExecutionManifest;
pub use recheck::RecheckBy;

impl From<ProblemsError> for EngineError {
    fn from(e: ProblemsError) -> Self {
        match e {
            ProblemsError::InvalidId(_) | ProblemsError::Refused(_) => {
                EngineError::Invalid(e.to_string())
            }
            ProblemsError::NotFound(_) => EngineError::NotFound(e.to_string()),
            ProblemsError::NewerSchema(_) => EngineError::Conflict(e.to_string()),
            ProblemsError::Io(io) => EngineError::Io(io),
        }
    }
}

/// The playbook a run started, and the scope it came from.
fn started(events: &[Event]) -> Option<(String, Scope)> {
    let id = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
        _ => None,
    })?;
    let scope = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { origin, .. } => origin.as_deref().and_then(Scope::parse),
            _ => None,
        })
        .unwrap_or(Scope::Project);
    Some((id, scope))
}

/// Adds what the run at `run_dir` (under project `root`) saw to its
/// playbook's store, its texts scrubbed and their sources attached. Returns
/// how many sightings it had. Observing a run twice adds nothing; a run
/// without sightings leaves the store untouched.
pub fn observe(root: &Path, run_dir: &Path) -> Result<usize, EngineError> {
    let events = crate::run_view::read_events(run_dir)?;
    let Some((playbook, scope)) = started(&events) else {
        return Ok(0);
    };
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let scrubber = Scrubber::for_workspace(root, Vec::new());
    let mut seen = extract::sightings(&run_id, &events, &scrubber);
    if seen.is_empty() {
        // Nothing to add: the store is left as it is (no file per run).
        return Ok(0);
    }
    let manifest = crate::manifest::read(run_dir).ok().flatten();
    let snapshot = crate::legacy_snapshot::load_run_playbook(run_dir);
    let mut sources = BTreeMap::new();
    for o in &mut seen {
        o.source = sources
            .entry(o.scope.key())
            .or_insert_with(|| {
                source::seen(
                    root,
                    run_dir,
                    &o.scope,
                    manifest.as_ref(),
                    snapshot.as_ref(),
                    &scrubber,
                )
            })
            .clone();
    }
    let count = seen.len();
    store::update(root, scope, &playbook, |f| {
        let changed = f.observe(&run_id, &seen);
        Ok(((), changed))
    })?;
    Ok(count)
}

/// Records what the run at `run_dir` saw as it ended, best effort: a fault
/// is reported on stderr and changes nothing about the run. Called by the
/// finish hooks and by every path that journals an operator abort (which
/// never passes them).
pub(crate) fn observe_ended(run_dir: &Path) {
    let root = root_of(run_dir);
    if let Err(e) = observe(&root, run_dir) {
        eprintln!("apb: could not record the run's known problems: {e}");
    }
}

/// A run of playbook `id` of `scope` under `root` that waits on a host
/// task now: its host session is doing the run's work, so a decision that
/// session relays is not a person's word. `None` when none waits. Every
/// run with a `tasks/` directory (only a host-mode run has one) is looked
/// at, however old; a task past its deadline does not count (its drive gave
/// up on it, or is gone).
pub fn run_waiting_on_host(root: &Path, scope: Scope, id: &str) -> Option<String> {
    let now = apb_core::clock::now_ms_u64();
    std::fs::read_dir(root.join(".apb/runs"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|d| d.join("tasks").is_dir())
        .find(|d| {
            crate::run_view::read_events(d).is_ok_and(|events| {
                started(&events).is_some_and(|(p, s)| p == id && s == scope)
                    && crate::host_task::pending_tasks(d, &events)
                        .iter()
                        .any(|t| t.deadline.is_none_or(|at| at > now))
            })
        })
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// What [`scan`] did.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ScanReport {
    /// Ended runs looked at.
    pub runs: u32,
    /// Sightings they had (a run observed before adds nothing again).
    pub sightings: u32,
}

/// Observes up to `last` of the latest ended runs of playbook `id` of
/// `scope` under `root` (backfill for runs that ended before this apb
/// collected). `last` is capped at [`OBSERVED_RUNS_KEPT`], the runs the
/// store remembers as observed, so a scan repeated never counts a run
/// twice.
pub fn scan(root: &Path, scope: Scope, id: &str, last: usize) -> Result<ScanReport, EngineError> {
    let last = last.min(OBSERVED_RUNS_KEPT);
    let mut report = ScanReport::default();
    let dirs =
        crate::run_history::runs_of(&[root.to_path_buf()], id, None, crate::grade::BACKFILL_SCAN);
    for d in dirs {
        if report.runs as usize >= last {
            break;
        }
        let ended = crate::run_view::read_events(&d).is_ok_and(|events| {
            started(&events).is_some_and(|(_, s)| s == scope)
                && events.iter().any(|e| {
                    matches!(
                        e.payload,
                        EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
                    )
                })
        });
        if !ended {
            continue;
        }
        report.runs += 1;
        report.sightings += observe(root, &d)? as u32;
    }
    Ok(report)
}

/// The agent nodes a problem of `p`'s scope is shown to: `None` for every
/// agent node (a path, a program, an MCP tool, anything else); else the
/// nodes that bind its connector, carry its skill or are its node, which
/// may be none.
fn nodes_for(
    p: &KnownProblem,
    playbook: &Playbook,
    manifest: &RunExecutionManifest,
) -> Option<Vec<String>> {
    let agents = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::AgentTask { .. }))
        .map(|n| n.id.as_str());
    let scoped: Vec<String> = match p.scope.kind {
        ScopeKind::Connector => {
            let connector = p.scope.connector_parts().map_or("", |(c, _)| c);
            agents
                .filter(|n| {
                    manifest
                        .grants_for(n)
                        .iter()
                        .any(|g| g.connector == connector)
                })
                .map(str::to_string)
                .collect()
        }
        ScopeKind::Skill => agents
            .filter(|n| {
                manifest
                    .for_node(n)
                    .is_some_and(|e| e.skills.iter().any(|s| s.name == p.scope.name))
            })
            .map(str::to_string)
            .collect(),
        ScopeKind::Node => agents
            .filter(|n| *n == p.scope.name)
            .map(str::to_string)
            .collect(),
        ScopeKind::Path | ScopeKind::Cli | ScopeKind::McpTool | ScopeKind::Other => return None,
    };
    Some(scoped)
}

/// The reason a significant problem is hidden by its latest recheck, if it
/// is.
fn hidden_by(check: Option<&RecheckRecord>) -> Option<&'static str> {
    match check.map(|c| c.verdict) {
        Some(RecheckVerdict::Undetermined) => Some(hidden::UNDETERMINED),
        Some(RecheckVerdict::ClosedProposed) => Some(hidden::CLOSED_PROPOSED),
        _ => None,
    }
}

/// What showing one problem came to.
enum Showing {
    Shown(&'static str),
    Hidden(&'static str),
    Closed,
}

/// What a starting run of `playbook` (of `scope`, under `root`) shows: the
/// significant problems in store order, each rechecked first when its
/// source changed since it was last checked (by the project's decision
/// model, at most [`recheck::MAX_MODEL_RECHECKS_PER_START`] per start),
/// at most `max_shown`. `None` (nothing journaled) for a playbook whose
/// block shows nothing. Recheck results go to the store, never to the
/// journal.
pub fn at_start(
    root: &Path,
    scope: Scope,
    playbook: &Playbook,
    manifest: &RunExecutionManifest,
) -> Option<EventPayload> {
    let block = playbook.known_problems.as_ref().filter(|b| b.shows())?;
    let loaded = match store::read(root, scope, &playbook.id) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("apb: known problems not read: {e}");
            return Some(EventPayload::KnownProblemsShown {
                shown: Vec::new(),
                hidden: Vec::new(),
                unsigned: 0,
            });
        }
    };
    if !loaded.signed {
        // Only a count: a file this installation did not write puts none of
        // its texts or ids in the journal.
        return Some(EventPayload::KnownProblemsShown {
            shown: Vec::new(),
            hidden: Vec::new(),
            unsigned: u32::try_from(loaded.unsigned_problems).unwrap_or(u32::MAX),
        });
    }
    let significant: Vec<&KnownProblem> = loaded
        .file
        .problems
        .iter()
        .filter(|p| p.is_significant(block))
        .collect();
    let scrubber = Scrubber::for_workspace(root, Vec::new());
    let mut model = None::<Option<crate::decision::standalone::StandaloneDecider>>;
    let mut asked = 0usize;
    let mut records: Vec<Rechecked> = Vec::new();
    let (mut shown, mut hide) = (Vec::new(), Vec::new());
    let full = |shown: &Vec<ShownProblem>| shown.len() >= block.max_shown as usize;
    for p in significant {
        let nodes = nodes_for(p, playbook, manifest);
        let showing = if nodes.as_ref().is_some_and(Vec::is_empty) {
            Showing::Hidden(hidden::NO_MATCHING_NODE)
        } else {
            match &p.source {
                None => carried(p),
                Some(_) => match source::current(root, &p.scope, scope, &playbook.id, &scrubber) {
                    None => Showing::Hidden(hidden::SOURCE_UNREADABLE),
                    Some(now) if Some(now.digest.as_str()) == p.checked_digest() => carried(p),
                    // A full prompt spends no model request on a problem
                    // it could not show anyway.
                    Some(_) if full(&shown) => Showing::Hidden(hidden::OVER_LIMIT),
                    Some(now) => {
                        let decider = model.get_or_insert_with(|| recheck::decider(root));
                        let record = match decider {
                            Some(d) if asked < recheck::MAX_MODEL_RECHECKS_PER_START => {
                                asked += 1;
                                recheck::by_model(d, p, &now).ok()
                            }
                            _ => None,
                        };
                        match record {
                            None => Showing::Hidden(hidden::SOURCE_CHANGED_UNCHECKED),
                            Some(r) => {
                                let showing = match r.verdict {
                                    RecheckVerdict::Holds => Showing::Shown("model"),
                                    RecheckVerdict::Closed => Showing::Closed,
                                    RecheckVerdict::ClosedProposed => {
                                        Showing::Hidden(hidden::CLOSED_PROPOSED)
                                    }
                                    RecheckVerdict::Undetermined => {
                                        Showing::Hidden(hidden::UNDETERMINED)
                                    }
                                };
                                records.push(Rechecked {
                                    id: p.id.clone(),
                                    checked: p.checked_digest().map(str::to_string),
                                    record: r,
                                });
                                showing
                            }
                        }
                    }
                },
            }
        };
        match showing {
            Showing::Shown(_) if full(&shown) => hide.push(HiddenProblem {
                id: p.id.clone(),
                reason: hidden::OVER_LIMIT.into(),
            }),
            Showing::Shown(basis) => {
                let (symptom, workaround) = p.shown_texts(block);
                shown.push(ShownProblem {
                    id: p.id.clone(),
                    scope: p.scope.key(),
                    symptom,
                    workaround,
                    nodes: nodes.unwrap_or_default(),
                    basis: basis.into(),
                })
            }
            Showing::Hidden(reason) => hide.push(HiddenProblem {
                id: p.id.clone(),
                reason: reason.into(),
            }),
            Showing::Closed => {}
        }
    }
    store_rechecks(root, scope, &playbook.id, records);
    Some(EventPayload::KnownProblemsShown {
        shown,
        hidden: hide,
        unsigned: 0,
    })
}

/// A model recheck made at a run's start, and the digest its problem was
/// last checked against when it was asked.
struct Rechecked {
    id: String,
    checked: Option<String>,
    record: RecheckRecord,
}

/// Stores the rechecks a run start made. One whose problem changed
/// meanwhile (a person closed or dismissed it, or another check landed)
/// is dropped: the later word stands.
fn store_rechecks(root: &Path, scope: Scope, id: &str, records: Vec<Rechecked>) {
    if records.is_empty() {
        return;
    }
    let saved = store::update(root, scope, id, |f| {
        let mut changed = false;
        for r in records {
            let current = f.get(&r.id).is_some_and(|p| {
                p.status == ProblemStatus::Open && p.checked_digest() == r.checked.as_deref()
            });
            if !current {
                continue;
            }
            match f.record_check(&r.id, r.record) {
                Ok(()) => changed = true,
                Err(e) => eprintln!("apb: known-problem recheck not stored: {e}"),
            }
        }
        Ok(((), changed))
    });
    if let Err(e) = saved {
        eprintln!("apb: known-problem rechecks not stored: {e}");
    }
}

/// How a problem shows when nothing needs rechecking: as its latest
/// recheck left it.
fn carried(p: &KnownProblem) -> Showing {
    match hidden_by(p.check.as_ref()) {
        Some(why) => Showing::Hidden(why),
        None => Showing::Shown(basis_of(p.check.as_ref())),
    }
}

/// The basis a problem is shown on: the source unchanged since the last
/// recheck that let it stand (or since it was seen).
fn basis_of(check: Option<&RecheckRecord>) -> &'static str {
    match check {
        Some(c) if c.verdict == RecheckVerdict::Holds => c.basis.as_str(),
        _ => RecheckBasis::Unchanged.as_str(),
    }
}

/// How a problem's source compares now with the text it was last checked
/// against.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceNow {
    /// The same text: the last verdict carries forward.
    Unchanged,
    /// Changed since the last check: the next run start rechecks it (by
    /// model when the project allows), or a person does.
    Changed,
    /// It cannot be read now (gone, moved): it never closes on its own.
    Unreadable,
}

/// One problem as the surfaces show it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProblemView {
    #[serde(flatten)]
    pub problem: KnownProblem,
    /// Shown to a run that starts now (before any recheck), under the
    /// playbook's block or the default one.
    pub significant: bool,
    /// A person should look at it: it is open and its latest recheck could
    /// not settle it or proposes a close, or it is significant and its
    /// source changed since it was checked or cannot be read now.
    pub needs_look: bool,
    /// Its source now, for a problem that has one (`cli:`, `mcp:` and
    /// `other` problems have none and are never rechecked).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source_now: Option<SourceNow>,
}

/// What a run showed its agents and what it hid, from its
/// `known_problems_shown` event; `None` for a run whose playbook shows
/// nothing. The one reading every run surface uses (`run_status`,
/// `supervisor_run_inspect`, the dashboard's run view).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunKnownProblems {
    pub shown: Vec<ShownProblem>,
    pub hidden: Vec<HiddenProblem>,
    /// Problems of a file this installation did not write: never shown,
    /// and only counted.
    pub unsigned: u32,
}

/// The run's latest `known_problems_shown`, as [`RunKnownProblems`].
pub fn of_run(events: &[Event]) -> Option<RunKnownProblems> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::KnownProblemsShown {
            shown,
            hidden,
            unsigned,
        } => Some(RunKnownProblems {
            shown: shown.clone(),
            hidden: hidden.clone(),
            unsigned: *unsigned,
        }),
        _ => None,
    })
}

/// The scope a surface reads playbook `id` under when the caller names
/// none: the project when it has the playbook or a problem file for it,
/// else global when the global store has the playbook, else the project.
pub fn scope_for(root: &Path, id: &str) -> Scope {
    let in_project = root.join(".apb/playbooks").join(id).is_dir()
        || store::dir(root, Scope::Project)
            .join(format!("{id}.json"))
            .is_file();
    if !in_project && source::current_playbook(root, Scope::Global, id).is_some() {
        Scope::Global
    } else {
        Scope::Project
    }
}

/// The known problems of playbook `id` of `scope` under `root`, judged
/// under the block of the playbook's current version (the default block
/// when it has none, or is gone).
pub fn view_current(root: &Path, scope: Scope, id: &str) -> Result<ProblemsView, EngineError> {
    let block = source::current_playbook(root, scope, id).and_then(|p| p.known_problems);
    view(root, scope, id, block.as_ref())
}

/// A passage of a problem's source that its old text does not have.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedPassage {
    pub index: u32,
    /// 1-based, inclusive, in the current text.
    pub lines: [u32; 2],
    pub text: String,
}

/// The passages of problem `problem`'s source (scrubbed) that its old text
/// does not have: what a host or a person quotes from to close it. Empty
/// when the source has not changed, has no old text, cannot be read, or
/// still looks like it holds a credential after scrubbing.
pub fn changed_passages(
    root: &Path,
    scope: Scope,
    id: &str,
    problem: &str,
) -> Result<Vec<ChangedPassage>, EngineError> {
    let p = stored(root, scope, id, problem)?;
    let scrubber = Scrubber::for_workspace(root, Vec::new());
    let Some(now) = source::current(root, &p.scope, scope, id, &scrubber).filter(|c| !c.flagged)
    else {
        return Ok(Vec::new());
    };
    let old = p.source.as_ref().and_then(|s| s.excerpt.as_deref());
    Ok(
        apb_core::known_problems::recheck::changed_passages(old, &now.text)
            .into_iter()
            .map(|q| ChangedPassage {
                index: q.index as u32,
                lines: q.lines,
                text: q.text,
            })
            .collect(),
    )
}

/// A playbook's known problems as the surfaces show them.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProblemsView {
    pub playbook: String,
    pub scope: String,
    /// Whether this installation wrote the file. An unsigned file's
    /// problems are never shown nor listed, only counted.
    pub signed: bool,
    /// How many problems an unsigned file holds.
    pub unsigned_problems: u32,
    pub problems: Vec<ProblemView>,
}

/// The known problems of playbook `id` of `scope` under `root`, judged
/// under `block` (the default block when the playbook has none).
pub fn view(
    root: &Path,
    scope: Scope,
    id: &str,
    block: Option<&KnownProblemsBlock>,
) -> Result<ProblemsView, EngineError> {
    let loaded = store::read(root, scope, id)?;
    let default = KnownProblemsBlock::default();
    let block = block.unwrap_or(&default);
    let scrubber = Scrubber::for_workspace(root, Vec::new());
    let problems = loaded
        .file
        .problems
        .into_iter()
        .map(|p| {
            let significant = loaded.signed && p.is_significant(block);
            let source_now = source_now(root, scope, id, &p, &scrubber);
            // Hidden from the next start until someone settles it: its
            // latest recheck did not, or its source changed or is gone.
            let unsettled = hidden_by(p.check.as_ref()).is_some()
                || (significant
                    && matches!(source_now, Some(SourceNow::Changed | SourceNow::Unreadable)));
            ProblemView {
                significant,
                needs_look: p.status == ProblemStatus::Open && unsettled,
                source_now,
                problem: p,
            }
        })
        .collect();
    Ok(ProblemsView {
        playbook: id.to_string(),
        scope: scope.as_str().to_string(),
        signed: loaded.signed,
        unsigned_problems: u32::try_from(loaded.unsigned_problems).unwrap_or(u32::MAX),
        problems,
    })
}

/// How `p`'s source compares now with the digest it was last checked
/// against; `None` for a problem without a source (the branch a run start
/// takes for it too).
fn source_now(
    root: &Path,
    scope: Scope,
    id: &str,
    p: &KnownProblem,
    scrubber: &Scrubber,
) -> Option<SourceNow> {
    p.source.as_ref()?;
    let checked = p.checked_digest()?;
    Some(match source::current(root, &p.scope, scope, id, scrubber) {
        None => SourceNow::Unreadable,
        Some(now) if now.digest == checked => SourceNow::Unchanged,
        Some(_) => SourceNow::Changed,
    })
}

/// A person's action on a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemAction {
    Confirm,
    Dismiss,
    Reopen,
    /// Close with a quote of the source's current text that the old text
    /// does not have. A problem without a readable source (none, or one
    /// gone now) closes on the person's word, with a quote of 12 to 400
    /// characters kept as given.
    Close {
        quote: String,
    },
}

/// Applies a person's `action` to problem `problem` of playbook `id`.
/// `by` names the surface (`cli`, `mcp`, `dashboard`). Returns the problem
/// as stored. A close whose quote does not settle is refused.
pub fn decide(
    root: &Path,
    scope: Scope,
    id: &str,
    problem: &str,
    action: ProblemAction,
    by: &str,
    reason: &str,
) -> Result<KnownProblem, EngineError> {
    let scrubber = Scrubber::for_workspace(root, Vec::new());
    let who = Decided {
        by: by.to_string(),
        at_ms: apb_core::clock::now_ms_u64(),
        reason: apb_core::known_problems::capped(&scrubber.scrub(reason), 300),
    };
    let decision = match action {
        ProblemAction::Confirm => PersonDecision::Confirm,
        ProblemAction::Dismiss => PersonDecision::Dismiss,
        ProblemAction::Reopen => PersonDecision::Reopen,
        ProblemAction::Close { quote } => {
            let proposal = RecheckProposal {
                verdict: ProposedVerdict::Closed,
                quote: Some(quote),
                why: String::new(),
            };
            // Settled first and stored only when it closes: a refused quote
            // leaves the problem exactly as it was.
            let (r, seen) = settle_now(root, scope, id, problem, &RecheckBy::Person(proposal), by)?;
            if r.verdict != RecheckVerdict::Closed {
                return Err(EngineError::Invalid(format!(
                    "the quote does not close the problem ({}): quote new text of its source exactly",
                    r.reason_code
                )));
            }
            store_check(root, scope, id, problem, r, &seen)?;
            return stored(root, scope, id, problem);
        }
    };
    store::update(root, scope, id, |f| {
        f.decide(problem, decision, who)?;
        Ok(((), true))
    })?;
    stored(root, scope, id, problem)
}

fn stored(root: &Path, scope: Scope, id: &str, problem: &str) -> Result<KnownProblem, EngineError> {
    store::read(root, scope, id)?
        .file
        .get(problem)
        .cloned()
        .ok_or_else(|| EngineError::NotFound(format!("no known problem `{problem}`")))
}

/// Rechecks problem `problem` of playbook `id` now, by `by`, and stores the
/// record. A model recheck asks only when the project's `problem_recheck`
/// use is on. A person's close of a problem without a readable source is
/// taken on the person's word.
pub fn recheck_one(
    root: &Path,
    scope: Scope,
    id: &str,
    problem: &str,
    by: RecheckBy,
    surface: &str,
) -> Result<RecheckRecord, EngineError> {
    let (record, seen) = settle_now(root, scope, id, problem, &by, surface)?;
    store_check(root, scope, id, problem, record.clone(), &seen)?;
    Ok(record)
}

/// What a recheck was settled against: the problem's status and the digest
/// it was last checked against when it was read.
struct Seen {
    status: ProblemStatus,
    checked: Option<String>,
}

/// Stores `record`, unless the problem changed since it was settled (a
/// person decided, a run start or another surface rechecked it): then the
/// later word stands and the caller is told to look again.
fn store_check(
    root: &Path,
    scope: Scope,
    id: &str,
    problem: &str,
    record: RecheckRecord,
    seen: &Seen,
) -> Result<(), EngineError> {
    store::update(root, scope, id, |f| {
        let p = f
            .get(problem)
            .ok_or_else(|| ProblemsError::NotFound(problem.to_string()))?;
        if p.status != seen.status || p.checked_digest() != seen.checked.as_deref() {
            return Err(ProblemsError::Refused(format!(
                "problem `{problem}` changed while it was rechecked: look at it again"
            )));
        }
        f.record_check(problem, record)?;
        Ok(((), true))
    })?;
    Ok(())
}

/// The record a recheck of `problem` by `by` comes to now, not stored, and
/// what it was settled against.
fn settle_now(
    root: &Path,
    scope: Scope,
    id: &str,
    problem: &str,
    by: &RecheckBy,
    surface: &str,
) -> Result<(RecheckRecord, Seen), EngineError> {
    let loaded = store::read(root, scope, id)?;
    if !loaded.signed {
        return Err(EngineError::Conflict(
            "the known-problem file was not written by this apb installation".into(),
        ));
    }
    let p = loaded
        .file
        .get(problem)
        .cloned()
        .ok_or_else(|| EngineError::NotFound(format!("no known problem `{problem}`")))?;
    let scrubber = Scrubber::for_workspace(root, Vec::new());
    let current = source::current(root, &p.scope, scope, id, &scrubber);
    let seen = Seen {
        status: p.status,
        checked: p.checked_digest().map(str::to_string),
    };
    let record = match (by, current) {
        (RecheckBy::Person(proposal), None) if proposal.verdict == ProposedVerdict::Closed => {
            person_word_close(&p, proposal, &scrubber, surface)?
        }
        (_, None) => {
            return Err(EngineError::Conflict(format!(
                "the source of `{problem}` cannot be read now: nothing to recheck against"
            )));
        }
        (RecheckBy::Model, Some(now)) => {
            let decider = recheck::decider(root)
                .ok_or_else(|| EngineError::Conflict(recheck::MODEL_OFF.into()))?;
            recheck::by_model(&decider, &p, &now)
                .map_err(|why| EngineError::Conflict(why.message().into()))?
        }
        (RecheckBy::Host(proposal), Some(now)) => {
            recheck::settle_by(&p, &now, proposal, RecheckBasis::Host, surface)
        }
        (RecheckBy::Person(proposal), Some(now)) => {
            recheck::settle_by(&p, &now, proposal, RecheckBasis::Person, surface)
        }
    };
    Ok((record, seen))
}

/// A person's close of a problem whose source cannot be read (it has none,
/// or it is gone): taken on the person's word, with a quote of the usual
/// length and no hidden character, kept as given (scrubbed).
fn person_word_close(
    p: &KnownProblem,
    proposal: &RecheckProposal,
    scrubber: &Scrubber,
    surface: &str,
) -> Result<RecheckRecord, EngineError> {
    let quote = scrubber.scrub(proposal.quote.as_deref().unwrap_or_default().trim());
    quote_shape(&quote).map_err(|code| {
        EngineError::Invalid(format!(
            "a close on your word needs a quote of 12 to 400 characters without hidden characters ({code})"
        ))
    })?;
    Ok(RecheckRecord {
        digest: String::new(),
        verdict: RecheckVerdict::Closed,
        basis: RecheckBasis::Person,
        quote: Some(apb_core::known_problems::Quote {
            text: quote,
            lines: [0, 0],
        }),
        reason_code: if p.source.is_none() {
            "no_source"
        } else {
            "source_unreadable"
        }
        .into(),
        mode: None,
        at_ms: apb_core::clock::now_ms_u64(),
        by: surface.to_string(),
    })
}

/// The root a run directory belongs to, for the hooks that only know the
/// directory.
pub(crate) fn root_of(run_dir: &Path) -> PathBuf {
    crate::run_retro::project_root_of(run_dir).unwrap_or_else(|| run_dir.to_path_buf())
}
