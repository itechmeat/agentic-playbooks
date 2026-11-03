//! The session of a scenario run: the answer book behind every boundary
//! site of the drive, the count of every boundary the run reached, and the
//! first answer it missed under `on_missing: error`.
//!
//! A session is registered under its run directory for as long as the
//! drive runs ([`register`]); a boundary site finds it with [`of`] or asks
//! [`at_run`]. A run directory with no session (every real run) answers
//! [`Resolution::Real`], so the site acts exactly as before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use apb_core::scenario::{OnMissing, Scenario, ScenarioLevel};
use apb_core::schema::Playbook;
use serde::Serialize;

use super::book::{AnswerBook, AnswerUse, MissingAnswer, Taken};
use super::{Answer, BoundaryKey, BoundaryKind, Resolution, Session};
use crate::error::EngineError;

/// The prefix of the error that stops a scenario run on a missing answer
/// under `on_missing: error`.
pub const MISSING_ANSWER: &str = "scenario_missing_answer";

/// The diagnostics record kind of one answer use.
pub const ANSWER_RECORD: &str = "scenario_answer";

/// The diagnostics record kind of a template reference that rendered empty.
pub const TEMPLATE_EMPTY_RECORD: &str = "template_empty";

/// The scripted agent's name, journaled on every scripted attempt.
pub const SCENARIO_AGENT: &str = "scenario";

/// What a declared secret param holds in a scenario run.
pub const SECRET_PLACEHOLDER: &str = "<scenario secret>";

/// The request cap of a smoke run without one asked for, and the most one
/// may ask for.
pub const SMOKE_DEFAULT_REQUESTS: u32 = 20;
pub const SMOKE_MAX_REQUESTS: u32 = 200;

/// The script of a scenario run, handed to its preparation.
#[derive(Debug, Clone)]
pub struct ScenarioPrep {
    pub scenario: Scenario,
    pub level: ScenarioLevel,
    /// The scenario file's digest, kept in the run's origin.
    pub scenario_digest: String,
    /// The decision requests a smoke run may send.
    pub max_requests: u32,
    /// The requests the scenario tree sent so far: a nested child's prep
    /// shares its parent's, so the cap holds for the whole scenario.
    pub sent: SentRequests,
}

/// The decision requests one smoke scenario sent, shared by its run and
/// every nested child run.
#[derive(Debug, Clone, Default)]
pub struct SentRequests(Arc<AtomicU32>);

/// How often a boundary was reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundaryUse {
    pub kind: BoundaryKind,
    pub count: u32,
}

/// The session of one scenario run.
#[derive(Debug)]
pub struct ScenarioSession {
    level: ScenarioLevel,
    max_requests: u32,
    sent: SentRequests,
    run_dir: PathBuf,
    book: Mutex<AnswerBook>,
    boundaries: Mutex<BTreeMap<BoundaryKind, u32>>,
    halted: Mutex<Option<MissingAnswer>>,
}

impl ScenarioSession {
    /// A session of `scenario` over `playbook` at `level`, for the run in
    /// `run_dir`.
    pub fn new(prep: &ScenarioPrep, playbook: &Playbook, run_dir: &Path) -> Self {
        ScenarioSession {
            level: prep.level,
            max_requests: prep.max_requests,
            sent: prep.sent.clone(),
            run_dir: run_dir.to_path_buf(),
            book: Mutex::new(AnswerBook::new(prep.scenario.clone(), playbook)),
            boundaries: Mutex::new(BTreeMap::new()),
            halted: Mutex::new(None),
        }
    }

    pub fn level(&self) -> ScenarioLevel {
        self.level
    }

    /// Takes `n` decision requests from the scenario's cap, shared with its
    /// parent and nested children, all or none; `false` when fewer are
    /// left. A quick run sends no request and is never capped here.
    pub fn take_requests(&self, n: u32) -> bool {
        if self.level != ScenarioLevel::Smoke {
            return true;
        }
        let cap = self.max_requests.min(SMOKE_MAX_REQUESTS);
        self.sent
            .0
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |sent| {
                sent.checked_add(n).filter(|t| *t <= cap)
            })
            .is_ok()
    }

    /// Gives back `n` requests [`Self::take_requests`] took that never
    /// went out (a cache hit, an empty chain, a refused ask).
    pub fn give_back_requests(&self, n: u32) {
        if self.level == ScenarioLevel::Smoke {
            let _ = self
                .sent
                .0
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |sent| {
                    Some(sent.saturating_sub(n))
                });
        }
    }

    fn book(&self) -> std::sync::MutexGuard<'_, AnswerBook> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn on_missing(&self) -> OnMissing {
        self.book().scenario().on_missing
    }

    /// Counts one crossing of `kind`.
    pub fn count(&self, kind: BoundaryKind) {
        *self
            .boundaries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(kind)
            .or_default() += 1;
    }

    /// Every boundary reached, with its count, in the order of the drive.
    pub fn boundaries(&self) -> Vec<BoundaryUse> {
        let counts = self.boundaries.lock().unwrap_or_else(|e| e.into_inner());
        BoundaryKind::ALL
            .iter()
            .filter_map(|k| {
                counts.get(k).map(|c| BoundaryUse {
                    kind: *k,
                    count: *c,
                })
            })
            .collect()
    }

    /// How the scenario's answers were used.
    pub fn usage(&self) -> AnswerUse {
        self.book().usage()
    }

    /// The missing answer that stopped the run, under `on_missing: error`.
    pub fn halted(&self) -> Option<MissingAnswer> {
        self.halted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Records a boundary with no answer. Under `on_missing: error` the
    /// first one stops the run: the error the site returns.
    pub fn missing(&self, kind: BoundaryKind, key: &BoundaryKey) -> Option<EngineError> {
        let key = &self.keyed(kind, key);
        let m = MissingAnswer {
            boundary: kind.as_str().to_string(),
            node: key.node.clone(),
            visit: key.visit,
            attempt: key.attempt,
            item: key.item.clone(),
            part: key.part.clone(),
        };
        self.book().record_missing(m.clone());
        self.record_use(kind, key, "missing", None);
        match self.on_missing() {
            OnMissing::Error => {
                let mut halted = self.halted.lock().unwrap_or_else(|e| e.into_inner());
                if halted.is_none() {
                    *halted = Some(m.clone());
                }
                Some(EngineError::Invalid(format!(
                    "{MISSING_ANSWER}: {}",
                    m.describe()
                )))
            }
            OnMissing::Fail => None,
        }
    }

    /// The visit a node boundary of `node` is at: the open execution (its
    /// finish count plus one), or for a judge edge the execution that just
    /// finished.
    fn visit_of(&self, kind: BoundaryKind, node: &str) -> u32 {
        let events = crate::event::read_all(&self.run_dir).unwrap_or_default();
        let finished = finished_count(&events, node);
        match kind {
            BoundaryKind::JudgeEdges => finished,
            _ => finished + 1,
        }
    }

    /// `key` with its visit filled from the journal when the site left it 0.
    fn keyed(&self, kind: BoundaryKind, key: &BoundaryKey) -> BoundaryKey {
        let mut key = key.clone();
        if key.visit == 0
            && let Some(node) = &key.node
        {
            key.visit = self.visit_of(kind, node);
        }
        key
    }

    /// The next scripted attempt of the node execution and item `key` (no
    /// attempt number: the book counts them), with the key completed.
    pub fn attempt(
        &self,
        key: &BoundaryKey,
    ) -> (
        BoundaryKey,
        Option<Taken<apb_core::scenario::AttemptAnswer>>,
    ) {
        self.count(BoundaryKind::AgentAttempt);
        let mut key = self.keyed(BoundaryKind::AgentAttempt, key);
        let node = key.node.clone().unwrap_or_default();
        let (k, taken) = self.book().attempt(&node, key.visit, key.item.as_deref());
        key.attempt = Some(k);
        if let Some(t) = &taken {
            self.record_use(BoundaryKind::AgentAttempt, &key, "used", Some(&t.from));
        }
        (key, taken)
    }

    /// The paths the read-only check of the current attempt of `key` finds
    /// changed.
    pub fn read_only_dirty(&self, key: &BoundaryKey) -> Vec<String> {
        let key = self.keyed(BoundaryKind::ReadOnlyCheck, key);
        self.book().read_only_dirty(
            key.node.as_deref().unwrap_or_default(),
            key.visit,
            key.item.as_deref(),
        )
    }

    fn record_use(
        &self,
        kind: BoundaryKind,
        key: &BoundaryKey,
        how: &str,
        from: Option<&super::book::AnswerRef>,
    ) {
        let after = crate::event::read_all_lossy_tail(&self.run_dir)
            .ok()
            .and_then(|e| e.last().map(|e| e.seq))
            .unwrap_or(0);
        let fields = serde_json::json!({
            "boundary": kind.as_str(),
            "key": key,
            "use": how,
            "from": from,
        });
        let serde_json::Value::Object(fields) = fields else {
            return;
        };
        let key_text = format!(
            "{}:{}:{}",
            kind.as_str(),
            serde_json::to_string(key).unwrap_or_default(),
            how
        );
        crate::diagnostics::writer::write_with(&self.run_dir, |_| {
            Some(crate::diagnostics::writer::Record {
                kind: ANSWER_RECORD,
                after_seq: after,
                key: key_text,
                fields,
            })
        });
    }
}

impl Session for ScenarioSession {
    fn resolve(&self, kind: BoundaryKind, key: &BoundaryKey) -> Resolution {
        use BoundaryKind as K;
        let smoke = self.level == ScenarioLevel::Smoke;
        match kind {
            // Nothing outside the run directory is touched, nothing waits.
            K::Protect
            | K::Worktree
            | K::Provenance
            | K::Notify
            | K::NodeCache
            | K::FinishHooks
            | K::RetryDelay
            | K::InfraBackoff
            | K::EngineAid => {
                self.count(kind);
                Resolution::Skip
            }
            // A quick run models no `llm_emulation` provider: its judge
            // goes on to the profile, a scripted attempt.
            K::JudgeEmulation if !smoke => {
                self.count(kind);
                Resolution::Skip
            }
            // Smoke asks this machine's decision providers.
            K::JudgeEmulation | K::JudgeNative | K::JudgeEdges | K::ListJudgeCheck if smoke => {
                self.count(kind);
                Resolution::Real
            }
            K::AgentAttempt => match self.attempt(key) {
                (_, Some(t)) => Resolution::Answer(Answer::Attempt(t.answer)),
                (_, None) => Resolution::Missing,
            },
            K::ReadOnlyCheck => {
                self.count(kind);
                Resolution::Skip
            }
            _ => {
                self.count(kind);
                let key = self.keyed(kind, key);
                let node = key.node.clone().unwrap_or_default();
                let visit = key.visit;
                let mut book = self.book();
                let found: Option<(Answer, super::book::AnswerRef)> = match kind {
                    K::Question => book.question(&node, visit, key.item.as_deref()).map(|q| {
                        let from = super::book::AnswerRef {
                            node: Some(node.clone()),
                            visit: None,
                            part: "question".into(),
                        };
                        (Answer::Question(q), from)
                    }),
                    K::SuccessCheck | K::CompletionCheck => {
                        let c = book.check(&node, visit, key.item.as_deref());
                        Some((
                            Answer::Check(c),
                            super::book::AnswerRef {
                                node: Some(node.clone()),
                                visit: None,
                                part: "check".into(),
                            },
                        ))
                    }
                    K::Script => book
                        .script(&node, visit)
                        .map(|t| (Answer::Script(t.answer), t.from)),
                    K::Review => book
                        .review(&node, visit)
                        .map(|t| (Answer::Review(t.answer), t.from)),
                    K::Publish => book
                        .publish(&node, visit)
                        .map(|t| (Answer::Publish(t.answer), t.from)),
                    K::Wait => book
                        .wait(&node, visit)
                        .map(|t| (Answer::Wait(t.answer), t.from)),
                    K::ChildPlaybook => book
                        .child(&node, visit)
                        .map(|t| (Answer::Child(t.answer), t.from)),
                    K::ListSource => book
                        .source(&node, visit)
                        .map(|t| (Answer::Source(t.answer), t.from)),
                    K::JudgeNative | K::ListJudgeCheck => book
                        .judge(&node, visit)
                        .map(|t| (Answer::Judge(t.answer), t.from)),
                    K::JudgeEdges => book
                        .judge_edges(&node, visit)
                        .map(|t| (Answer::JudgeEdges(t.answer), t.from)),
                    K::Baseline => book
                        .baseline()
                        .map(|t| (Answer::Baseline(t.answer), t.from)),
                    K::GoalScript => key
                        .part
                        .as_deref()
                        .and_then(|p| p.parse::<usize>().ok())
                        .and_then(|i| book.goal(i))
                        .map(|t| (Answer::Goal(t.answer), t.from)),
                    _ => None,
                };
                drop(book);
                match found {
                    Some((answer, from)) => {
                        self.record_use(kind, &key, "used", Some(&from));
                        Resolution::Answer(answer)
                    }
                    None => Resolution::Missing,
                }
            }
        }
    }
}

type Registry = Mutex<BTreeMap<PathBuf, Arc<ScenarioSession>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Registers `session` for the run in `run_dir` (at prepare, once the run
/// directory exists), until [`unregister`].
pub fn register(run_dir: &Path, session: Arc<ScenarioSession>) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(run_dir.to_path_buf(), session);
}

/// Ends the session of the run in `run_dir` and returns it.
pub fn unregister(run_dir: &Path) -> Option<Arc<ScenarioSession>> {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(run_dir)
}

/// The session of the run in `run_dir`; `None` for a real run.
pub fn of(run_dir: &Path) -> Option<Arc<ScenarioSession>> {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if reg.is_empty() {
        return None;
    }
    reg.get(run_dir).cloned()
}

/// Whether the run in `run_dir` is a scenario run.
pub fn is_scenario(run_dir: &Path) -> bool {
    of(run_dir).is_some()
}

/// The one question a boundary site with a run directory asks before it
/// acts ([`super::at`] over the run's session).
pub fn at_run(run_dir: &Path, kind: BoundaryKind, key: &BoundaryKey) -> Resolution {
    let session = of(run_dir);
    super::at(session.as_deref().map(|s| s as &dyn Session), kind, key)
}

/// Whether a boundary of a run in `run_dir` is skipped (a scenario run
/// crosses it without acting).
pub fn skips(run_dir: &Path, kind: BoundaryKind) -> bool {
    matches!(
        at_run(run_dir, kind, &BoundaryKey::default()),
        Resolution::Skip
    )
}

/// The key of a node boundary of `node`, its visit filled by the session.
pub fn node_key(node: &str) -> BoundaryKey {
    BoundaryKey {
        node: Some(node.to_string()),
        ..Default::default()
    }
}

/// What a site does with a missing answer: the run's stop (`Err`) under
/// `on_missing: error`, else `Ok` and the site fails the boundary.
pub fn on_missing(
    run_dir: &Path,
    kind: BoundaryKind,
    key: &BoundaryKey,
) -> Result<(), EngineError> {
    match of(run_dir).and_then(|s| s.missing(kind, key)) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Writes a `template_empty` record for every reference of template `what`
/// of `node` that rendered as nothing in the scenario run of `run_dir`,
/// with why (`unfilled`, from [`crate::judge_eval::JudgeInputs::unfilled`],
/// computed only when [`is_scenario`]). A real run writes none.
pub fn note_empty_references(
    run_dir: &Path,
    node: &str,
    what: &str,
    unfilled: impl FnOnce() -> Vec<(String, &'static str)>,
) {
    if !is_scenario(run_dir) {
        return;
    }
    let visit = {
        let events = crate::event::read_all(run_dir).unwrap_or_default();
        finished_count(&events, node) + 1
    };
    let after = crate::event::read_all_lossy_tail(run_dir)
        .ok()
        .and_then(|e| e.last().map(|e| e.seq))
        .unwrap_or(0);
    for (reference, why) in unfilled() {
        let fields = serde_json::json!({
            "node": node,
            "visit": visit,
            "template": what,
            "reference": reference,
            "why": why,
        });
        let serde_json::Value::Object(fields) = fields else {
            continue;
        };
        let key = format!("{node}:{visit}:{what}:{reference}");
        crate::diagnostics::writer::write_with(run_dir, |_| {
            Some(crate::diagnostics::writer::Record {
                kind: TEMPLATE_EMPTY_RECORD,
                after_seq: after,
                key,
                fields,
            })
        });
    }
}

/// The executions of `node` that ended.
fn finished_count(events: &[crate::event::Event], node: &str) -> u32 {
    crate::identity::index(events)
        .executions(node)
        .iter()
        .filter(|x| !x.is_open())
        .count() as u32
}

/// The stop of a scenario run that missed an answer under
/// `on_missing: error`, once it did.
pub fn halt_error(run_dir: &Path) -> Option<EngineError> {
    let missing = of(run_dir)?.halted()?;
    Some(EngineError::Invalid(format!(
        "{MISSING_ANSWER}: {}",
        missing.describe()
    )))
}

/// The working directory of a scenario run: the empty `work/` inside its
/// run directory. `None` for a real run.
pub fn workdir(run_dir: &Path) -> Option<PathBuf> {
    is_scenario(run_dir).then(|| run_dir.join(WORK_DIR))
}

/// The name of a scenario run's working directory inside its run
/// directory.
pub const WORK_DIR: &str = "work";

/// The script of a nested scenario child: scenario `name` of child
/// playbook `id` (next to its versions, under `definition_parent`), at the
/// level and request cap of the parent's scenario run in `run_dir`.
pub fn child_prep(
    run_dir: &Path,
    definition_parent: &Path,
    id: &str,
    name: &str,
) -> Result<ScenarioPrep, String> {
    let parent = of(run_dir).ok_or("a nested scenario runs only inside a scenario run")?;
    let playbook_dir = definition_parent.join("playbooks").join(id);
    let loaded = apb_core::scenario::load(&playbook_dir, name)
        .map_err(|e| format!("child playbook `{id}`: {e}"))?;
    Ok(ScenarioPrep {
        scenario: loaded.scenario,
        level: parent.level,
        scenario_digest: loaded.digest,
        max_requests: parent.max_requests,
        sent: parent.sent.clone(),
    })
}

/// A scenario run that ended: where it is, how it ended, and how the
/// scenario's answers and the boundaries were used.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioRun {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub outcome: crate::state::RunStatus,
    pub answers: AnswerUse,
    pub boundaries: Vec<BoundaryUse>,
    /// The missing answer that stopped the run under `on_missing: error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halted: Option<MissingAnswer>,
}

impl ScenarioRun {
    pub(crate) fn of(
        run_id: String,
        run_dir: PathBuf,
        outcome: crate::state::RunStatus,
        session: Option<&ScenarioSession>,
    ) -> Self {
        ScenarioRun {
            run_id,
            run_dir,
            outcome,
            answers: session.map(ScenarioSession::usage).unwrap_or_default(),
            boundaries: session.map(ScenarioSession::boundaries).unwrap_or_default(),
            halted: session.and_then(ScenarioSession::halted),
        }
    }
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod drive_tests;
