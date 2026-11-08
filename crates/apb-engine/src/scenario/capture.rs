//! The capture of a finished run as a scenario: a pure fold of what the
//! run's journal holds plus one read of its run config. It writes nothing;
//! the preview and the save are the tool layer's, over this.
//!
//! The fold is strict: the journal must read whole, the run must be a
//! finished production run that nothing moved, and every answer it holds
//! must be one the scenario format can replay exactly. Anything else is
//! refused with every gap named, so a person never saves a scenario that
//! replays only part of what happened. Every text and value passes the
//! run's scrubber before it is placed, and the YAML is produced only by
//! serializing the scenario type, so captured text cannot add keys to the
//! file.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;

use apb_core::scenario::{
    AttemptAnswer, BaselineAnswer, CheckAnswer, ChildAnswer, Expect, GoalAnswer, JudgeAnswer,
    OnExtra, OnMissing, PublishAnswer, QuestionAnswer, QuestionScript, ReadOnlyAnswer,
    ReviewAnswer, Scenario, ScenarioDecisions, ScriptAnswer, ScriptedFailure, ScriptedStatus,
    SkipWord, TemplatesEmpty, UnavailableKind, Visit, VisitAnswer, WaitAnswer,
};
use apb_core::schema::{Node, NodeKind, Playbook};

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::identity::{self, Execution};
use crate::manifest::RunOrigin;
use crate::run_redact::RunRedactor;

/// What to capture: the scenario name, and the nodes whose `expect.status`
/// and `expect.visits` are asserted (`--node`; empty: every node that
/// finished).
pub struct CaptureRequest {
    pub name: String,
    pub nodes: Vec<String>,
}

/// One built capture: the scenario, the exact YAML a save writes, its
/// digest, and what a person should read before saving.
#[derive(Debug)]
pub struct Capture {
    pub scenario: Scenario,
    /// `yaml_serde::to_string(&scenario)`, the exact text a Write.
    pub yaml: String,
    /// `apb_core::scenario::scenario_digest(&scenario)`.
    pub digest: String,
    /// The playbook id and the version the run executed.
    pub playbook: String,
    pub version: String,
    /// The scope the run's playbook was started from (its `run_provenance`
    /// origin): a save lands in that scope's playbook, never in another
    /// scope's playbook of the same id. `None` for a journal that records
    /// no origin.
    pub scope: Option<apb_core::scope::Scope>,
    /// How many texts the scrubber changed, counted (never listed).
    pub redacted: usize,
    /// One line per fact a person should know before saving.
    pub warnings: Vec<String>,
}

/// One fact the format cannot replay: `code (node): detail`.
#[derive(Clone)]
pub struct Gap {
    pub code: &'static str,
    pub node: Option<String>,
    pub detail: String,
}

pub enum CaptureError {
    /// The run cannot be captured; every gap, sorted by node then code.
    NotCapturable(Vec<Gap>),
    Engine(EngineError),
}

impl From<EngineError> for CaptureError {
    fn from(e: EngineError) -> Self {
        CaptureError::Engine(e)
    }
}

/// One pass over the journal: the scrubber and the counters and the gaps
/// it collects as it goes.
struct Fold<'a> {
    root: &'a Path,
    run_dir: &'a Path,
    req: &'a CaptureRequest,
    scrubber: &'a apb_core::scrub::Scrubber,
    playbook: &'a Playbook,
    events: &'a [Event],
    index: identity::ExecutionIndex,
    redacted: Cell<usize>,
    warnings: std::cell::RefCell<Vec<String>>,
    gaps: std::cell::RefCell<Vec<Gap>>,
}

impl Fold<'_> {
    /// Scrubs one text, counting every change.
    fn text(&self, s: &str) -> String {
        let out = self.scrubber.scrub(s);
        if out != s {
            self.redacted.set(self.redacted.get() + 1);
        }
        out
    }

    /// Scrubs one JSON value, counting every change.
    fn value(&self, v: serde_json::Value) -> serde_json::Value {
        let before = serde_json::to_string(&v).unwrap_or_default();
        let out = self.scrubber.redact_value(v);
        if serde_json::to_string(&out).unwrap_or_default() != before {
            self.redacted.set(self.redacted.get() + 1);
        }
        out
    }

    /// Records one gap.
    fn gap(&self, code: &'static str, node: Option<&str>, detail: String) {
        self.gaps.borrow_mut().push(Gap {
            code,
            node: node.map(str::to_string),
            detail,
        });
    }

    /// The events of one execution: every event of `node` the index puts
    /// inside it.
    fn execution_events(&self, node: &str, exec: &Execution) -> Vec<&Event> {
        self.events
            .iter()
            .filter(|e| {
                self.index
                    .containing(node, e.seq)
                    .is_some_and(|x| x == exec)
            })
            .collect()
    }

    /// Every node's visit answers, or the gaps that stop the capture.
    fn answers(&self) -> BTreeMap<String, Vec<VisitAnswer>> {
        let mut answers: BTreeMap<String, Vec<VisitAnswer>> = BTreeMap::new();
        for (node, execs) in self.index.nodes() {
            let Some(schema_node) = self.playbook.node(node) else {
                self.gap(
                    "unknown_node",
                    Some(node),
                    "the run's playbook names no node".into(),
                );
                continue;
            };
            if schema_node.list.is_some() {
                self.gap(
                    "list_node",
                    Some(node),
                    "a list node's per-item answers cannot be replayed".into(),
                );
                continue;
            }
            for (i, exec) in execs.iter().enumerate() {
                if let Some(a) = self.visit_answer(schema_node, node, exec, i as u32 + 1) {
                    answers.entry(node.to_string()).or_default().push(a);
                }
            }
        }
        answers
    }

    /// The one visit answer of one execution, or none for a node that
    /// takes no answer (a start, a condition, a promptless finish).
    fn visit_answer(
        &self,
        schema_node: &Node,
        node: &str,
        exec: &Execution,
        ordinal: u32,
    ) -> Option<VisitAnswer> {
        let mine = self.execution_events(node, exec);
        let visit = Visit::Nth(ordinal);
        let has_attempts = mine
            .iter()
            .any(|e| matches!(e.payload, EventPayload::AttemptFinished { .. }));
        if mine
            .iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheHit { .. }))
            && !has_attempts
        {
            self.gap(
                "cache_hit",
                Some(node),
                "this execution was served from the node cache; a scenario run bypasses the cache"
                    .into(),
            );
            return None;
        }
        let finished = mine.iter().find_map(|e| match &e.payload {
            EventPayload::NodeFinished { status, output, .. } => {
                Some((status.as_str(), output.as_str()))
            }
            _ => None,
        });
        match &schema_node.kind {
            NodeKind::AgentTask { .. }
            | NodeKind::Prompt { .. }
            | NodeKind::Finish {
                prompt: Some(_), ..
            } => {
                let attempts = self.attempt_answers(node, &mine);
                (!attempts.is_empty()).then(|| {
                    let mut v = visit_answer_of(visit);
                    v.attempts = attempts;
                    v
                })
            }
            NodeKind::Script { .. } => {
                let (status, output) = finished?;
                let exit = match status {
                    "succeeded" => 0,
                    "failed" => 1,
                    other => {
                        self.gap(
                            "attempt_status",
                            Some(node),
                            format!("a script execution ended `{other}`"),
                        );
                        return None;
                    }
                };
                {
                    let mut v = visit_answer_of(visit);
                    v.script = Some(ScriptAnswer {
                        exit,
                        stdout: self.text(output),
                        stderr: String::new(),
                    });
                    Some(v)
                }
            }
            NodeKind::HumanReview { .. } => {
                if mine
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::ReviewWithdrawn { .. }))
                {
                    self.gap(
                        "directive",
                        Some(node),
                        "the review was withdrawn; a scenario cannot replay that".into(),
                    );
                    return None;
                }
                let (decision, note) = mine.iter().find_map(|e| match &e.payload {
                    EventPayload::ReviewDecided { decision, note, .. } => {
                        Some((decision.clone(), note.clone()))
                    }
                    _ => None,
                })?;
                {
                    let mut v = visit_answer_of(visit);
                    v.review = Some(ReviewAnswer {
                        decision,
                        note: (!note.is_empty()).then(|| self.text(&note)),
                    });
                    Some(v)
                }
            }
            NodeKind::Judge { .. } => {
                let made = mine.iter().find_map(|e| match &e.payload {
                    EventPayload::DecisionMade {
                        use_site,
                        answers,
                        error,
                        ..
                    } if use_site.as_str() == "judge_node" => {
                        Some((answers.clone(), error.clone()))
                    }
                    _ => None,
                });
                let (made_answers, error) = made?;
                let judge = match error.as_deref() {
                    None | Some("") => {
                        let mut map = BTreeMap::new();
                        for (qid, a) in made_answers {
                            map.insert(
                                qid,
                                QuestionAnswer {
                                    value: a.value.map(|v| self.value(v)),
                                    p: a.p,
                                    confidence: a.confidence,
                                },
                            );
                        }
                        JudgeAnswer::Answers(map)
                    }
                    Some(kind) => {
                        let unavailable = match kind {
                            "timeout" => UnavailableKind::Timeout,
                            "rate_limited" => UnavailableKind::RateLimited,
                            "auth" => UnavailableKind::Auth,
                            "budget" => UnavailableKind::Budget,
                            "invalid" => UnavailableKind::Invalid,
                            _ => UnavailableKind::Unavailable,
                        };
                        if kind == "cancelled" {
                            self.gap(
                                "judge_cancelled",
                                Some(node),
                                "the judge ask was cancelled; a scenario cannot replay that".into(),
                            );
                            return None;
                        }
                        JudgeAnswer::Unavailable { unavailable }
                    }
                };
                {
                    let mut v = visit_answer_of(visit);
                    v.judge = Some(judge);
                    Some(v)
                }
            }
            NodeKind::Playbook { .. } => {
                let (status, output) = finished?;
                let scripted = match status {
                    "succeeded" => ScriptedStatus::Succeeded,
                    "failed" => ScriptedStatus::Failed,
                    other => {
                        self.gap(
                            "attempt_status",
                            Some(node),
                            format!("a child execution ended `{other}`"),
                        );
                        return None;
                    }
                };
                let output = match serde_json::from_str::<serde_json::Value>(output) {
                    Ok(v) if v.is_object() => Some(self.value(v)),
                    _ => {
                        let text = self.text(output);
                        (!text.is_empty()).then_some(serde_json::Value::String(text))
                    }
                };
                {
                    let mut v = visit_answer_of(visit);
                    v.playbook = Some(ChildAnswer::Result {
                        status: scripted,
                        output,
                    });
                    Some(v)
                }
            }
            NodeKind::Wait { wait_for, .. } => {
                if mine
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::WaitTimeout { .. }))
                {
                    let mut v = visit_answer_of(visit);
                    v.wait = Some(WaitAnswer {
                        timed_out: true,
                        ..Default::default()
                    });
                    return Some(v);
                }
                if mine
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::WaitSignalled { .. }))
                {
                    self.gap(
                        "wait_signal",
                        Some(node),
                        "the wait was ended by a signal whose payload the journal does not hold"
                            .into(),
                    );
                    return None;
                }
                if matches!(wait_for, apb_core::schema::WaitFor::Timer { .. }) {
                    let mut v = visit_answer_of(visit);
                    v.wait = Some(WaitAnswer {
                        elapsed: true,
                        ..Default::default()
                    });
                    return Some(v);
                }
                self.gap(
                    "wait_signal",
                    Some(node),
                    "the wait was ended by a signal whose payload the journal does not hold".into(),
                );
                None
            }
            NodeKind::Publish { .. } => {
                let decided = mine.iter().find_map(|e| match &e.payload {
                    EventPayload::ReviewDecided {
                        decision,
                        plan_digest: Some(_),
                        ..
                    } => Some(decision.clone()),
                    _ => None,
                });
                let (status, _) = finished?;
                let scripted = match status {
                    "succeeded" => Some(ScriptedStatus::Succeeded),
                    "failed" => Some(ScriptedStatus::Failed),
                    _ => None,
                };
                let decision = decided?;
                {
                    let mut v = visit_answer_of(visit);
                    v.publish = Some(PublishAnswer {
                        decision,
                        status: scripted,
                    });
                    Some(v)
                }
            }
            NodeKind::Start
            | NodeKind::Condition { .. }
            | NodeKind::Finish { prompt: None, .. } => None,
        }
    }
}

/// The one entry point: builds the capture of the finished production run
/// at `run_dir`, or names every gap that stops it.
pub fn capture(root: &Path, run_dir: &Path, req: &CaptureRequest) -> Result<Capture, CaptureError> {
    let mut gaps: Vec<Gap> = Vec::new();

    // The origin: only a production run is captured.
    let origin = RunOrigin::of_read(&crate::manifest::read(run_dir));
    if !origin.is_production() {
        gaps.push(Gap {
            code: "not_production",
            node: None,
            detail: "the run is not a production run".into(),
        });
    }

    // The journal: strictly whole, and nothing unknown skipped.
    let events = match crate::event::read_all(run_dir) {
        Ok(events) => events,
        Err(e) => {
            gaps.push(Gap {
                code: "journal_unreadable",
                node: None,
                detail: e.to_string(),
            });
            return Err(CaptureError::NotCapturable(gaps));
        }
    };
    let read = crate::event::read_journal(run_dir)?;
    if !read.unknown.is_empty() {
        let words: Vec<String> = read.unknown.iter().map(|u| u.kind.clone()).collect();
        gaps.push(Gap {
            code: "unknown_events",
            node: None,
            detail: format!(
                "the journal holds {} event type(s) this apb does not know: {}",
                words.len(),
                words.join(", ")
            ),
        });
    }

    // The scrubber: fail closed when the run's secrets cannot be read.
    let redactor = RunRedactor::for_run(root, run_dir);
    let scrubber = match redactor.scrubber() {
        Ok(s) => s,
        Err(e) => {
            gaps.push(Gap {
                code: "secrets_unresolved",
                node: None,
                detail: format!("{e:?}"),
            });
            return Err(CaptureError::NotCapturable(gaps));
        }
    };

    // The playbook the run executed.
    let Some(playbook) = crate::legacy_snapshot::load_run_playbook(run_dir) else {
        return Err(CaptureError::Engine(EngineError::Invalid(
            "the run holds no playbook snapshot to capture against".into(),
        )));
    };
    if playbook.baseline_check.as_ref().is_some_and(|b| b.enforce) {
        gaps.push(Gap {
            code: "baseline_enforced",
            node: None,
            detail: "the run's playbook enforces its baseline check".into(),
        });
    }

    // The run config: instruction, params, unattended.
    let config = crate::run_config::read_run_config(run_dir)?;

    // Params: a sensitive value with no default to re-derive it from can
    // never be replayed; one with a default is left out of the scenario
    // params, warned.
    for p in &playbook.params {
        let Some(value) = config.params.get(&p.name) else {
            continue;
        };
        if apb_core::run_params::sensitive(p.secret, &p.name, value).is_some()
            && p.default.is_none()
        {
            gaps.push(Gap {
                code: "param_withheld",
                node: None,
                detail: format!(
                    "param `{}` holds a sensitive value and has no default, so a replay cannot take it",
                    p.name
                ),
            });
        }
    }

    // What the journal says happened.
    let state = crate::state::RunState::fold(&events);
    if !matches!(
        state.run_status,
        crate::state::RunStatus::Succeeded | crate::state::RunStatus::Failed
    ) {
        gaps.push(Gap {
            code: "not_finished",
            node: None,
            detail: format!("the run's state is {}", state.run_status.as_str()),
        });
    }
    if events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
    {
        gaps.push(Gap {
            code: "aborted",
            node: None,
            detail: "the run was stopped; a scenario has no answer that stops a run".into(),
        });
    }
    if events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::HostTaskRequested { .. }))
    {
        gaps.push(Gap {
            code: "host_mode",
            node: None,
            detail: "the run had a host task".into(),
        });
    }
    for e in &events {
        let moved = match &e.payload {
            EventPayload::SupervisorAction { action, .. } => {
                action == "node_retry" || action == "run_continue_from"
            }
            EventPayload::PatchApplied { .. } => true,
            EventPayload::ProfileRebound { .. } => true,
            EventPayload::NodesRevived { .. } => true,
            _ => false,
        };
        if moved {
            gaps.push(Gap {
                code: "directive",
                node: None,
                detail: "a supervisor directive moved the run".into(),
            });
            break;
        }
    }
    if !gaps.is_empty() {
        return Err(CaptureError::NotCapturable(gaps));
    }

    // The run identity from `run_started`.
    let Some((playbook_id, run_version)) = run_identity(&events) else {
        return Err(CaptureError::Engine(EngineError::Invalid(
            "the journal holds no run_started".into(),
        )));
    };

    let fold = Fold {
        root,
        run_dir,
        req,
        scrubber,
        playbook: &playbook,
        events: &events,
        index: identity::index(&events),
        redacted: Cell::new(0),
        warnings: std::cell::RefCell::new(Vec::new()),
        gaps: std::cell::RefCell::new(Vec::new()),
    };
    let answers = fold.answers();
    let mut answers = answers;
    fold.fold_judge_edges(&mut answers);
    let scenario = fold.build(answers);
    let value = serde_json::to_value(&scenario).map_err(|e| {
        CaptureError::Engine(EngineError::Invalid(format!(
            "the capture does not serialize: {e}"
        )))
    })?;
    if let Some(flag) = scrubber.refuses_value(&value) {
        fold.gap(
            "credential_shaped",
            None,
            format!("the capture still holds a {flag:?} after its scrub"),
        );
    }
    let gaps: Vec<Gap> = fold.gaps.borrow().clone();
    if !gaps.is_empty() {
        let mut sorted = gaps;
        sorted.sort_by(|a, b| (a.node.as_deref(), a.code).cmp(&(b.node.as_deref(), b.code)));
        return Err(CaptureError::NotCapturable(sorted));
    }

    // Serialize only through the type.
    let yaml = yaml_serde::to_string(&scenario).map_err(|e| {
        CaptureError::Engine(EngineError::Invalid(format!(
            "the capture does not serialize: {e}"
        )))
    })?;
    // The capture must parse and validate against the run's own playbook:
    // a failure here is a bug, never a person-visible refusal.
    let parsed = apb_core::scenario::parse(&yaml, &req.name).map_err(|e| {
        CaptureError::Engine(EngineError::Invalid(format!(
            "the capture does not parse: {e}"
        )))
    })?;
    let problems = apb_core::scenario::validate_scenario(&req.name, &parsed, &playbook);
    let errors: Vec<String> = problems
        .iter()
        .filter(|i| i.severity == apb_core::validate::Severity::Error)
        .map(|i| format!("{} {}", i.code, i.message))
        .collect();
    if !errors.is_empty() {
        return Err(CaptureError::Engine(EngineError::Invalid(format!(
            "the captured scenario does not validate: {}",
            errors.join("; ")
        ))));
    }

    let warnings = fold.warnings.borrow().clone();
    let redacted = fold.redacted.get();
    let digest = apb_core::scenario::scenario_digest(&scenario);
    Ok(Capture {
        scenario,
        yaml,
        digest,
        playbook: playbook_id,
        version: run_version,
        scope: events.iter().find_map(|e| match &e.payload {
            EventPayload::RunProvenance { origin, .. } => {
                origin.as_deref().and_then(apb_core::scope::Scope::parse)
            }
            _ => None,
        }),
        redacted,
        warnings,
    })
}

/// The playbook id and version the run executed, from `run_started`.
fn run_identity(events: &[Event]) -> Option<(String, String)> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted {
            playbook, version, ..
        } => Some((playbook.clone(), version.clone())),
        _ => None,
    })
}

/// The finish node the run ended at, when one finished.
fn finish_node(playbook: &Playbook, events: &[Event]) -> Option<String> {
    events
        .iter()
        .rev()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, .. } => Some(node.clone()),
            _ => None,
        })
        .find(|node| {
            matches!(
                playbook.node(node).map(|n| &n.kind),
                Some(NodeKind::Finish { .. })
            )
        })
}

/// An empty visit answer of one visit.
fn visit_answer_of(visit: Visit) -> VisitAnswer {
    VisitAnswer {
        visit,
        attempts: Vec::new(),
        script: None,
        review: None,
        judge: None,
        judge_edges: BTreeMap::new(),
        items: BTreeMap::new(),
        source: None,
        playbook: None,
        wait: None,
        publish: None,
    }
}

/// One journal attempt of an agent execution.
struct RawAttempt {
    status: String,
    rejected_for: Option<String>,
    rejected_output: Option<String>,
    partial_output: Option<String>,
    failure_kind: Option<String>,
    /// The question asked when this attempt ended (its suspension), with
    /// its answer text; `None` answer text is a timeout answer.
    question: Option<(String, Option<String>)>,
    /// The paths a read-only violation before this attempt's end found.
    read_only_dirty: Option<Vec<String>>,
    /// The journal's attempt number, which a read-only violation names.
    attempt: u32,
}

impl Fold<'_> {
    /// The attempt answers of one agent execution. A question round and
    /// the attempt that continued it are one scripted answer (`question`
    /// with `then`), because that is one answer of the book.
    fn attempt_answers(&self, node: &str, mine: &[&Event]) -> Vec<AttemptAnswer> {
        let mut raws: Vec<RawAttempt> = Vec::new();
        let mut questions: Vec<(String, Option<String>)> = Vec::new();
        let mut answered = 0usize;
        // The engine journals a read-only violation before its attempt's
        // `attempt_finished`, naming the attempt: kept by number and
        // attached once the attempts are known.
        let mut dirty: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        // A shape without an attempt number goes to the next attempt to end.
        let mut unnumbered: Option<Vec<String>> = None;
        // An attempt is open between its `attempt_started` and its
        // `attempt_finished`. A question asked while one is open is a live
        // round inside that attempt; the format scripts only a question that
        // ends its attempt, so a live round is a gap.
        let mut open = false;
        for e in mine {
            match &e.payload {
                EventPayload::AttemptStarted { .. } => open = true,
                EventPayload::AttemptFinished {
                    attempt,
                    status,
                    rejected_for,
                    rejected_output,
                    partial_output,
                    failure_kind,
                    ..
                } => {
                    open = false;
                    raws.push(RawAttempt {
                        attempt: *attempt,
                        status: status.clone(),
                        rejected_for: rejected_for.clone(),
                        rejected_output: rejected_output.clone(),
                        partial_output: partial_output.clone(),
                        failure_kind: failure_kind.clone(),
                        question: None,
                        read_only_dirty: unnumbered.take(),
                    })
                }
                EventPayload::QuestionAsked { question, .. } => {
                    if open {
                        self.gap(
                            "live_question",
                            Some(node),
                            "a question was asked and answered inside a running attempt; the format scripts only a question that ends its attempt".into(),
                        );
                    }
                    questions.push((question.clone(), None))
                }
                EventPayload::QuestionAnswered {
                    answer,
                    answered_by,
                    ..
                } => {
                    if let Some(slot) = questions.get_mut(answered) {
                        slot.1 = (answered_by != "timeout").then(|| answer.clone());
                    }
                    answered += 1;
                }
                EventPayload::ReadOnlyViolated {
                    attempt, changes, ..
                } => {
                    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
                    match attempt {
                        0 => unnumbered = Some(paths),
                        n => {
                            dirty.insert(*n, paths);
                        }
                    }
                }
                _ => {}
            }
        }
        // The question asked after attempt i belongs to it: the drive
        // journals the question when the suspension lands, right after
        // that attempt's `attempt_finished`.
        for raw in raws.iter_mut() {
            if let Some(paths) = dirty.remove(&raw.attempt) {
                raw.read_only_dirty = Some(paths);
            }
        }
        let mut qi = 0usize;
        for raw in raws.iter_mut() {
            if qi < questions.len() {
                raw.question = Some(questions[qi].clone());
                qi += 1;
            }
        }
        let node_output = mine
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::NodeFinished { output, .. } => Some(output.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < raws.len() {
            let (a, used) = self.build_attempt(node, &raws[i..], &node_output);
            out.push(a);
            i += used;
        }
        out
    }

    /// Builds one scripted answer from `raws[i..]`; returns it and how
    /// many raw attempts it consumed (a question's `then` consumes the
    /// continuation attempt).
    fn build_attempt(
        &self,
        node: &str,
        raws: &[RawAttempt],
        node_output: &str,
    ) -> (AttemptAnswer, usize) {
        let raw = &raws[0];
        let mut a = AttemptAnswer::default();
        let mut consumed = 1usize;
        if raw.rejected_output.is_some() && raw.rejected_for.is_none() && raw.status == "failed" {
            self.gap("fatal_rejection", Some(node), "a success check rejected this attempt fatally; the format has no fatal check answer".into());
        }
        match raw.status.as_str() {
            "succeeded" => {
                if raw.rejected_for.is_some() {
                    a.status = Some(ScriptedStatus::Succeeded);
                    a.check = Some(CheckAnswer::Reject);
                    a.output = raw.rejected_output.clone().map(|o| self.text(&o));
                } else if let Some(q) = &raw.question {
                    a.question = Some(QuestionScript {
                        text: self.text(&q.0),
                        answer: q.1.as_deref().map(|x| self.text(x)),
                        timeout: q.1.is_none(),
                    });
                    if raws.len() > 1 {
                        let (then, used) = self.build_attempt(node, &raws[1..], node_output);
                        a.then = Some(Box::new(then));
                        consumed += used;
                    }
                } else {
                    a.status = Some(ScriptedStatus::Succeeded);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(node_output) {
                        if v.is_object() {
                            a.outputs = match self.value(v) {
                                serde_json::Value::Object(map) => map.into_iter().collect(),
                                _ => BTreeMap::new(),
                            };
                        } else {
                            let t = self.text(node_output);
                            a.output = (!t.is_empty()).then_some(t);
                        }
                    } else {
                        let t = self.text(node_output);
                        a.output = (!t.is_empty()).then_some(t);
                    }
                }
            }
            "timed_out" => {
                a.timed_out = true;
                if let Some(p) = &raw.partial_output {
                    let t = self.text(p);
                    a.output = (!t.is_empty()).then_some(t);
                }
            }
            "failed" => {
                if raw.rejected_output.is_some() && raw.rejected_for.is_some() {
                    a.status = Some(ScriptedStatus::Succeeded);
                    a.check = Some(CheckAnswer::Reject);
                    a.output = raw.rejected_output.clone().map(|o| self.text(&o));
                } else {
                    a.status = Some(ScriptedStatus::Failed);
                    match raw.failure_kind.as_deref() {
                        Some("transient" | "infrastructure") => {
                            a.failure_kind = Some(ScriptedFailure::Transient)
                        }
                        Some("quota") => a.failure_kind = Some(ScriptedFailure::Quota),
                        Some("auth") => a.failure_kind = Some(ScriptedFailure::Auth),
                        Some("budget") => a.failure_kind = Some(ScriptedFailure::Budget),
                        Some("agent") => a.failure_kind = Some(ScriptedFailure::Agent),
                        _ => {}
                    }
                }
            }
            other => {
                self.gap(
                    "attempt_status",
                    Some(node),
                    format!("the attempt ended `{other}`; the format has no answer for that"),
                );
            }
        }
        if let Some(dirty) = &raw.read_only_dirty
            && !dirty.is_empty()
        {
            a.read_only = Some(ReadOnlyAnswer {
                dirty: dirty.clone(),
            });
        }
        (a, consumed)
    }

    /// The judge edge answers of the run: one `judge_edges` map per source
    /// node visit, keyed by the edge address each question answered.
    fn fold_judge_edges(&self, answers: &mut BTreeMap<String, Vec<VisitAnswer>>) {
        for e in self.events {
            let EventPayload::DecisionMade {
                use_site,
                node: Some(source),
                attempt: Some(attempt),
                answers: made,
                edge_addresses,
                ..
            } = &e.payload
            else {
                continue;
            };
            if use_site.as_str() != "judge_edge" {
                continue;
            }
            let Some(exec) = self.index.judge_edge_execution(source, e.seq, *attempt) else {
                continue;
            };
            let _ = &exec;
            let mut map = BTreeMap::new();
            for (qid, a) in made {
                let Some(address) = edge_addresses.get(qid) else {
                    self.gap(
                        "judge_edge_address",
                        Some(source),
                        format!("the judge edge answer `{qid}` names no edge address"),
                    );
                    continue;
                };
                map.insert(
                    address.clone(),
                    QuestionAnswer {
                        value: a.value.clone().map(|v| self.value(v)),
                        p: a.p,
                        confidence: a.confidence,
                    },
                );
            }
            if map.is_empty() {
                continue;
            }
            let visits = answers.entry(source.clone()).or_default();
            let visit = Visit::Nth(exec.ordinal);
            if let Some(existing) = visits.iter_mut().find(|v| v.visit == visit) {
                existing.judge_edges.extend(map);
            } else {
                let mut v = visit_answer_of(visit);
                v.judge_edges = map;
                visits.push(v);
            }
        }
    }
}

impl Fold<'_> {
    /// Assembles the scenario: run fields, decisions, expectations, the
    /// baseline and the goal, and the warnings a person reads.
    fn build(&self, mut answers: BTreeMap<String, Vec<VisitAnswer>>) -> Scenario {
        let config = crate::run_config::read_run_config(self.run_dir).ok();
        let mut params = BTreeMap::new();
        for (name, value) in config
            .as_ref()
            .map(|c| c.params.clone())
            .unwrap_or_default()
        {
            let declared = self.playbook.params.iter().find(|p| p.name == name);
            let sensitive = declared
                .map(|p| apb_core::run_params::sensitive(p.secret, &p.name, &value).is_some())
                .unwrap_or_else(|| apb_core::run_params::sensitive(false, &name, &value).is_some());
            if sensitive {
                self.warnings.borrow_mut().push(format!(
                    "param `{name}` was withheld: its value is sensitive; a replay takes its default or asks again"
                ));
                continue;
            }
            params.insert(name.clone(), self.text(&value));
        }
        let instruction = config
            .as_ref()
            .and_then(|c| c.instruction.clone())
            .map(|i| self.text(&i));
        let unattended = config.as_ref().map(|c| c.unattended).unwrap_or(false);

        // The modes the run's judges decided under, of the first use each.
        let mut decisions = ScenarioDecisions::default();
        for e in self.events {
            if let EventPayload::DecisionMade { use_site, mode, .. } = &e.payload {
                let Ok(mode) = mode_of(mode) else { continue };
                match use_site.as_str() {
                    "judge_node" if decisions.judge_node.is_none() => {
                        decisions.judge_node = Some(mode)
                    }
                    "judge_edge" if decisions.judge_edge.is_none() => {
                        decisions.judge_edge = Some(mode)
                    }
                    _ => {}
                }
            }
        }

        // The expectations.
        let state = crate::state::RunState::fold(self.events);
        let outcome = match state.run_status {
            crate::state::RunStatus::Succeeded => "succeeded",
            _ => "failed",
        };
        let mut in_scope: Vec<String> = if self.req.nodes.is_empty() {
            self.index
                .nodes()
                .filter(|(n, _)| self.playbook.node(n).is_some())
                .map(|(n, _)| n.to_string())
                .collect()
        } else {
            for n in &self.req.nodes {
                if self.playbook.node(n).is_none() {
                    self.gap(
                        "unknown_node",
                        Some(n),
                        "the run's playbook names no node".into(),
                    );
                }
            }
            self.req.nodes.clone()
        };
        in_scope.sort();
        in_scope.dedup();
        let mut expect = Expect {
            run: Some(apb_core::eval::RunCheck {
                outcome: vec![outcome.to_string()],
            }),
            finish: finish_node(self.playbook, self.events),
            templates_empty: TemplatesEmpty::Report,
            ..Default::default()
        };
        for node in &in_scope {
            let execs = self.index.executions(node);
            let Some(last) = execs.last() else { continue };
            if let Some(status) = &last.status {
                expect.status.insert(node.clone(), status.clone());
            }
            expect.visits.insert(node.clone(), execs.len() as u32);
        }

        let baseline = self
            .playbook
            .baseline_check
            .is_some()
            .then_some(BaselineAnswer::Skip(SkipWord::Skip));
        let mut goal = BTreeMap::new();
        for e in self.events {
            if let EventPayload::GoalChecked {
                index,
                check,
                status,
                ..
            } = &e.payload
                && check == "script"
            {
                goal.insert(
                    index.to_string(),
                    if status == "passed" {
                        GoalAnswer::Pass
                    } else {
                        GoalAnswer::Fail
                    },
                );
            }
        }

        // What a person should know before saving.
        let (playbook_id, run_version) = run_identity(self.events).unwrap_or_default();
        let playbook_dir = self.root.join(".apb/playbooks").join(&playbook_id);
        if let Some(current) = apb_core::versioning::read_current_pointer(&playbook_dir)
            && current != run_version
        {
            self.warnings.borrow_mut().push(format!(
                "the run executed version {run_version}, which is not current ({current}); the capture asserts what that version did"
            ));
        }

        answers.retain(|_, visits| !visits.is_empty());
        Scenario {
            schema: apb_core::scenario::SCENARIO_SCHEMA,
            id: self.req.name.clone(),
            title: Some(format!("Captured from run {}", run_id_of(self.run_dir))),
            versions: None,
            playbook_digest: None,
            instruction,
            params,
            unattended,
            on_missing: OnMissing::Error,
            on_extra: OnExtra::Error,
            decisions,
            answers,
            baseline,
            goal,
            expect,
        }
    }
}

/// The decision mode a journal word names, or none.
fn mode_of(word: &str) -> Result<apb_core::decisions::DecisionMode, ()> {
    match word {
        "off" => Ok(apb_core::decisions::DecisionMode::Off),
        "shadow" => Ok(apb_core::decisions::DecisionMode::Shadow),
        "advise" => Ok(apb_core::decisions::DecisionMode::Advise),
        "enforce" => Ok(apb_core::decisions::DecisionMode::Enforce),
        _ => Err(()),
    }
}

/// The run directory's id: its file name.
fn run_id_of(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// One line per gap: `code (node): detail`.
pub fn describe_gap(gap: &Gap) -> String {
    match &gap.node {
        Some(node) => format!("{} ({node}): {}", gap.code, gap.detail),
        None => format!("{}: {}", gap.code, gap.detail),
    }
}
