//! Host-task decisions (issue #193): the one engine function behind `apb
//! decide` and the MCP tool `decision_ask`.
//!
//! A host task (or a script, or a CLI agent step) asks a bounded question
//! (choose, rank, filter, map, yes/no, score) of the configured decision
//! providers instead of spending a full model turn on it. That is not running
//! the task elsewhere: the answer comes back to the asker, and the engine
//! never acts on it.
//!
//! Inside a run (a run id given), the call goes through the run's
//! [`DecisionRunner`]: the run's snapshot capped by the machine's file now,
//! the run's budget (every `decision_made` the run journaled counts), its
//! redaction, and a `decision_made` with `use_site: host_task` appended to the
//! run's journal. Outside a run it goes through the
//! [`StandaloneDecider`](super::standalone::StandaloneDecider): the project's
//! log `.apb/decisions.jsonl` and a per-day cap
//! (`uses.host_task.max_requests_per_day`).
//!
//! Every refusal names its reason: the kill switch, no provider, the use or
//! the playbook (`defaults.host_decisions: off`) switched off, the budget
//! spent, prompts not allowed to be sent, or a run that has ended.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::decisions::{DecisionMode, Resolution, SendClass};
use apb_decide::{ChoiceCriteria, Question, UseSite};
use serde::Serialize;
use serde_json::{Value, json};

use super::standalone::{StandaloneDecider, StandaloneOutcome};
use super::{
    DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, FieldClass, Judgement,
    StateField, StateParts, redact_value,
};
use crate::error::EngineError;
use crate::event::{DecisionAnswer, EventLog, EventPayload};

/// The longest question or criteria text accepted, in characters.
pub const MAX_TEXT_CHARS: usize = 8_000;
/// The most items one `rank`, `filter` or `map` call may carry: each item is
/// one question of the request.
pub const MAX_ITEMS: usize = 100;
/// The most options of a `choose` or `map` call (the providers' limit).
pub const MAX_OPTIONS: usize = 255;
/// The most levels of a `score` call (the providers' limit).
pub const MAX_LEVELS: usize = 10;

/// What a call asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// One of `options`.
    Choose,
    /// `items` ordered by how well each meets the question, best first.
    Rank,
    /// The `items` that meet the question.
    Filter,
    /// For each of `items`, one of `options`.
    Map,
    /// Yes or no.
    Is,
    /// A position on `options`, the levels lowest first.
    Score,
}

impl AskKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "choose" => AskKind::Choose,
            "rank" => AskKind::Rank,
            "filter" => AskKind::Filter,
            "map" => AskKind::Map,
            "is" => AskKind::Is,
            "score" => AskKind::Score,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AskKind::Choose => "choose",
            AskKind::Rank => "rank",
            AskKind::Filter => "filter",
            AskKind::Map => "map",
            AskKind::Is => "is",
            AskKind::Score => "score",
        }
    }
}

/// One host-task decision as a facade receives it.
#[derive(Debug, Clone)]
pub struct AskRequest {
    pub kind: AskKind,
    pub question: String,
    /// `choose` and `map`: the options; `score`: the levels, lowest first.
    pub options: Vec<String>,
    /// `rank`, `filter` and `map`: the items.
    pub items: Vec<String>,
    /// What a good answer looks like, sent with every question.
    pub criteria: Option<String>,
    /// The run to journal in and whose budget applies.
    pub run_id: Option<String>,
    /// The node that asks (journaled with the decision).
    pub node_id: Option<String>,
}

/// One item's answer of a `rank`, `filter` or `map` call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemAnswer {
    pub item: String,
    /// `map`: the option chosen for the item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// `rank`, `filter`: the probability the item meets the question;
    /// `map`: the chosen option's probability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// The provider refused this item; the rest of the call stands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
}

/// An answered call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AskAnswer {
    pub kind: AskKind,
    /// `choose`: the option; `score`: the nearest level; `is`: `yes` or
    /// `no`. Absent for the item kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// `choose`: the option's probability; `is`: the probability of yes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    /// `score`: the expected level index (0 = the first level).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// `rank` (best first), `filter` (every item, `kept` says which pass)
    /// and `map` (in the order given).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemAnswer>,
    /// `filter`: the items that pass (p at least 0.5), in the order given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<Vec<String>>,
    pub provider: String,
    pub model: String,
    pub calibrated: bool,
    /// The use's effective mode when it was asked.
    pub mode: String,
    /// Answered from the run's journal: the same question was already asked
    /// in this attempt, and no request was made.
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The run the decision was journaled in, with the event's seq.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// What a call came to. Every variant serializes with `answered`.
#[derive(Debug, Clone, PartialEq)]
pub enum AskOutcome {
    Answered(Box<AskAnswer>),
    /// Nothing was asked: `code` is machine-readable, `reason` one English
    /// sentence for the person.
    Refused {
        code: &'static str,
        reason: String,
    },
    /// Asked and failed (`unavailable`, `timeout`, `rate_limited`, `auth`,
    /// `invalid`, `journal`).
    Failed {
        error: String,
    },
}

impl AskOutcome {
    /// The JSON every facade returns.
    pub fn to_json(&self) -> Value {
        match self {
            AskOutcome::Answered(a) => {
                let mut v = json!(a);
                v["answered"] = json!(true);
                v
            }
            AskOutcome::Refused { code, reason } => {
                json!({ "answered": false, "refused": code, "reason": reason })
            }
            AskOutcome::Failed { error } => json!({
                "answered": false,
                "error": error,
                "reason": format!("the decision providers did not answer ({error}); decide without them"),
            }),
        }
    }

    pub fn answered(&self) -> bool {
        matches!(self, AskOutcome::Answered(_))
    }
}

fn refused(code: &'static str, reason: impl Into<String>) -> AskOutcome {
    AskOutcome::Refused {
        code,
        reason: reason.into(),
    }
}

/// Checks the request's shape. `Err` names what is wrong.
fn validate(req: &AskRequest) -> Result<(), EngineError> {
    let bad = |m: String| Err(EngineError::Invalid(m));
    if req.question.trim().is_empty() {
        return bad("`question` must not be empty".into());
    }
    for (name, text) in [
        ("question", Some(&req.question)),
        ("criteria", req.criteria.as_ref()),
    ] {
        if text.is_some_and(|t| t.chars().count() > MAX_TEXT_CHARS) {
            return bad(format!(
                "`{name}` is longer than {MAX_TEXT_CHARS} characters"
            ));
        }
    }
    let options = |min: usize, max: usize, what: &str| -> Result<(), EngineError> {
        let n = req.options.len();
        if n < min || n > max {
            return Err(EngineError::Invalid(format!(
                "`{}` needs {min} to {max} {what} in `options`, got {n}",
                req.kind.as_str()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some(dup) = req.options.iter().find(|o| !seen.insert(o.trim())) {
            return Err(EngineError::Invalid(format!(
                "option `{dup}` is given twice"
            )));
        }
        if req.options.iter().any(|o| o.trim().is_empty()) {
            return Err(EngineError::Invalid("an option must not be empty".into()));
        }
        Ok(())
    };
    let items = || -> Result<(), EngineError> {
        let n = req.items.len();
        if n == 0 || n > MAX_ITEMS {
            return Err(EngineError::Invalid(format!(
                "`{}` needs 1 to {MAX_ITEMS} entries in `items`, got {n}",
                req.kind.as_str()
            )));
        }
        Ok(())
    };
    match req.kind {
        AskKind::Choose => options(2, MAX_OPTIONS, "options"),
        AskKind::Score => options(2, MAX_LEVELS, "levels"),
        AskKind::Is => Ok(()),
        AskKind::Rank | AskKind::Filter => items(),
        AskKind::Map => {
            items()?;
            options(2, MAX_OPTIONS, "options")
        }
    }
}

/// The question id of item `i`.
fn item_id(i: usize) -> String {
    format!("item_{i}")
}

fn instructions(req: &AskRequest, item: Option<&str>) -> Value {
    let mut v = json!({ "question": req.question });
    if let Some(item) = item {
        v["item"] = json!(item);
    }
    if let Some(c) = req.criteria.as_deref().filter(|c| !c.trim().is_empty()) {
        v["criteria"] = json!(c);
    }
    v
}

fn choice(req: &AskRequest, item: Option<&str>) -> Question {
    Question::Choice {
        instructions: instructions(req, item),
        criteria: req
            .options
            .iter()
            .map(|o| (o.trim().to_string(), None))
            .collect::<ChoiceCriteria>(),
    }
}

fn noul(req: &AskRequest, item: Option<&str>) -> Question {
    Question::Noul {
        instructions: instructions(req, item),
        criteria: None,
    }
}

/// The questions of a request, by id.
fn questions(req: &AskRequest) -> BTreeMap<String, Question> {
    let mut out = BTreeMap::new();
    match req.kind {
        AskKind::Choose => {
            out.insert("answer".to_string(), choice(req, None));
        }
        AskKind::Is => {
            out.insert("answer".to_string(), noul(req, None));
        }
        AskKind::Score => {
            out.insert(
                "answer".to_string(),
                Question::Score {
                    instructions: instructions(req, None),
                    levels: req.options.iter().map(|o| json!(o)).collect(),
                },
            );
        }
        AskKind::Rank | AskKind::Filter => {
            for (i, item) in req.items.iter().enumerate() {
                out.insert(item_id(i), noul(req, Some(item)));
            }
        }
        AskKind::Map => {
            for (i, item) in req.items.iter().enumerate() {
                out.insert(item_id(i), choice(req, Some(item)));
            }
        }
    }
    out
}

/// The state every request carries: the question and the criteria (both of
/// the prompt class: the asker's own text).
fn state_parts(req: &AskRequest) -> StateParts {
    let mut fields = vec![StateField {
        name: "question",
        class: FieldClass::Prompt,
        text: req.question.clone(),
        head: 6_000,
        tail: 2_000,
    }];
    if let Some(c) = req.criteria.as_deref().filter(|c| !c.trim().is_empty()) {
        fields.push(StateField {
            name: "criteria",
            class: FieldClass::Prompt,
            text: c.to_string(),
            head: 6_000,
            tail: 2_000,
        });
    }
    let mut meta = serde_json::Map::new();
    meta.insert("kind".into(), json!(req.kind.as_str()));
    StateParts { fields, meta }
}

/// Reads the compact answers into the shape the kind returns.
fn shape(req: &AskRequest, answers: &BTreeMap<String, DecisionAnswer>) -> AskAnswer {
    let mut out = AskAnswer {
        kind: req.kind,
        answer: None,
        p: None,
        score: None,
        confidence: None,
        items: Vec::new(),
        kept: None,
        provider: String::new(),
        model: String::new(),
        calibrated: false,
        mode: String::new(),
        cached: false,
        latency_ms: None,
        cost_usd: None,
        run_id: None,
        seq: None,
    };
    let one = answers.get("answer");
    match req.kind {
        AskKind::Choose => {
            if let Some(a) = one {
                out.answer = a.value.as_ref().and_then(Value::as_str).map(str::to_string);
                out.p = a.p;
                out.confidence = a.confidence;
            }
        }
        AskKind::Is => {
            if let Some(p) = one.and_then(|a| a.p) {
                out.answer = Some(if p >= 0.5 { "yes" } else { "no" }.to_string());
                out.p = Some(p);
            }
        }
        AskKind::Score => {
            if let Some(a) = one {
                let value = a.value.as_ref().and_then(Value::as_f64);
                out.score = value;
                out.confidence = a.confidence;
                out.answer = value.and_then(|v| {
                    let i = v.round().max(0.0) as usize;
                    req.options
                        .get(i.min(req.options.len().saturating_sub(1)))
                        .cloned()
                });
            }
        }
        AskKind::Rank | AskKind::Filter | AskKind::Map => {
            for (i, item) in req.items.iter().enumerate() {
                let a = answers.get(&item_id(i));
                out.items.push(ItemAnswer {
                    item: item.clone(),
                    value: a
                        .and_then(|a| a.value.as_ref())
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    p: a.and_then(|a| a.p),
                    confidence: a.and_then(|a| a.confidence),
                    invalid: match a {
                        None => Some("no answer".into()),
                        Some(a) => a.invalid.clone(),
                    },
                });
            }
            if req.kind == AskKind::Filter {
                out.kept = Some(
                    out.items
                        .iter()
                        .filter(|i| i.p.is_some_and(|p| p >= 0.5))
                        .map(|i| i.item.clone())
                        .collect(),
                );
            }
            if req.kind == AskKind::Rank {
                // Best first; an item without an answer goes last, and equal
                // probabilities keep the given order (a stable sort).
                out.items.sort_by(|a, b| {
                    b.p.unwrap_or(-1.0)
                        .partial_cmp(&a.p.unwrap_or(-1.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
    }
    out
}

/// Asks one host-task decision for the project at `root`. `Err` only for a
/// malformed request or an unknown run; every other refusal is an
/// [`AskOutcome::Refused`] with its reason.
pub fn ask(root: &Path, req: &AskRequest) -> Result<AskOutcome, EngineError> {
    validate(req)?;
    if apb_core::decisions::killed_by_switch() {
        return Ok(refused(
            "off",
            format!(
                "decision models are switched off for this process ({}=off)",
                apb_core::decisions::KILL_SWITCH_ENV
            ),
        ));
    }
    match &req.run_id {
        Some(run_id) => ask_in_run(root, run_id, req),
        None => Ok(ask_standalone(root, req)),
    }
}

/// Why the machine's configuration gives no decision models for `root`, or
/// `None` when it does.
fn unconfigured(resolution: &Resolution) -> Option<AskOutcome> {
    let file = apb_core::decisions::DECISIONS_FILE;
    match resolution {
        Resolution::Active(eff)
            if eff.mode_for(apb_core::decisions::HOST_TASK_USE) == DecisionMode::Off =>
        {
            Some(refused(
                "use_off",
                format!("host-task decisions are switched off (`uses.host_task` in {file})"),
            ))
        }
        Resolution::Active(_) => None,
        Resolution::NotConfigured => Some(refused(
            "no_provider",
            format!("no decision provider is configured on this machine (no {file})"),
        )),
        Resolution::Invalid(e) => Some(refused(
            "no_provider",
            format!("{file} does not load, so no decision provider is available: {e}"),
        )),
        Resolution::KilledBySwitch => Some(refused(
            "off",
            format!(
                "decision models are switched off for this process ({}=off)",
                apb_core::decisions::KILL_SWITCH_ENV
            ),
        )),
        Resolution::OptedOut(why) => Some(refused(
            "use_off",
            format!("decision models are off for this project: {why}"),
        )),
        Resolution::AllOff => Some(refused(
            "use_off",
            format!("every decision use is off in {file}, host_task included"),
        )),
    }
}

fn ask_standalone(root: &Path, req: &AskRequest) -> AskOutcome {
    let resolution = apb_core::decisions::resolve(root);
    if let Some(r) = unconfigured(&resolution) {
        return r;
    }
    let Some(settings) = resolution.active() else {
        return refused("no_provider", "no decision provider is configured");
    };
    let decider = StandaloneDecider::with_settings(settings, root);
    if !decider.available() {
        return refused(
            "no_provider",
            "no configured decision provider resolved (a missing key or account id)",
        );
    }
    let parts = state_parts(req);
    let state = Value::Object(
        parts
            .fields
            .iter()
            .map(|f| (f.name.to_string(), json!(decider.redact(&f.text))))
            .chain([("meta".to_string(), Value::Object(parts.meta.clone()))])
            .collect(),
    );
    let order = parts
        .fields
        .iter()
        .map(|f| f.name.to_string())
        .chain(["meta".to_string()])
        .collect();
    match decider.decide(UseSite::HostTask, state, order, questions(req)) {
        StandaloneOutcome::Answered(a) => {
            let compact = super::compact_answers(&a.answers);
            let mut out = shape(req, &compact);
            out.provider = a.provider;
            out.model = a.model;
            out.calibrated = a.calibrated;
            out.mode = a.mode.as_str().to_string();
            out.latency_ms = Some(a.latency_ms);
            AskOutcome::Answered(Box::new(out))
        }
        StandaloneOutcome::Skipped { reason } => standalone_refusal(reason),
        StandaloneOutcome::Failed { error_kind } => AskOutcome::Failed { error: error_kind },
    }
}

fn standalone_refusal(reason: &'static str) -> AskOutcome {
    match reason {
        "budget" => refused(
            "budget",
            "the daily cap of host-task decisions outside a run is spent (`uses.host_task.max_requests_per_day`)",
        ),
        "send" => refused(
            "privacy",
            "privacy.send does not allow prompts, so the question may not be sent",
        ),
        "state_too_large" => refused(
            "too_large",
            "the question is over privacy.max_state_bytes once redacted; shorten it",
        ),
        "busy" => refused(
            "busy",
            "another decision of this project holds the log lock; ask again shortly",
        ),
        _ => refused("use_off", "host-task decisions are switched off"),
    }
}

/// Appends a decision to a run's journal from outside its drive: the same
/// path `apb connector call` takes. The drive re-reads the high-water mark
/// before its next append ([`EventLog::append`]), so no seq is reused.
struct RunJournalAppender {
    run_dir: PathBuf,
}

impl DecisionJournal for RunJournalAppender {
    fn append_decision(&self, payload: EventPayload) -> Result<u64, EngineError> {
        Ok(EventLog::open(&self.run_dir)?.append(payload)?.seq)
    }
}

fn ask_in_run(root: &Path, run_id: &str, req: &AskRequest) -> Result<AskOutcome, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let events = crate::event::read_all(&run_dir)?;
    let status = crate::state::RunState::fold(&events).run_status;
    if matches!(
        status,
        crate::state::RunStatus::Succeeded
            | crate::state::RunStatus::Failed
            | crate::state::RunStatus::Aborted
    ) {
        return Ok(refused(
            "run_ended",
            format!(
                "run `{run_id}` has ended ({}); a decision is journaled only in a live run",
                status.as_str()
            ),
        ));
    }
    if let Some(pb) = crate::legacy_snapshot::load_run_playbook(&run_dir)
        && pb.defaults.host_decisions == Some(apb_core::schema::HostDecisions::Off)
    {
        return Ok(refused(
            "playbook_off",
            format!(
                "playbook `{}` switches host-task decisions off (`defaults.host_decisions: off`)",
                pb.id
            ),
        ));
    }
    let live_resolution = apb_core::decisions::resolve(root);
    if let Some(r) = unconfigured(&live_resolution) {
        return Ok(r);
    }
    let live = live_resolution.active();
    // The run's snapshot, capped by what the machine allows now. A run
    // started before host-task decisions existed (or before a provider was
    // configured) takes the use from the live file: the cap keeps every
    // other setting of the run.
    let settings = match crate::manifest::read(&run_dir)
        .ok()
        .flatten()
        .and_then(|m| m.decisions)
    {
        Some(mut snapshot) => {
            if let Some(l) = live.as_ref()
                && let Some(u) = l.uses.get(apb_core::decisions::HOST_TASK_USE)
            {
                snapshot
                    .uses
                    .entry(apb_core::decisions::HOST_TASK_USE.to_string())
                    .or_insert_with(|| u.clone());
            }
            snapshot.capped_by(live.as_ref())
        }
        None => match live {
            Some(l) => l,
            None => return Ok(refused("no_provider", "no decision provider is configured")),
        },
    };
    let scrub = apb_core::connector::resolve::all_referenced_env_names(root);
    let runner = DecisionRunner::with_settings(settings, root, &run_dir, &events, &scrub);
    if runner.mode_for(UseSite::HostTask) == DecisionMode::Off {
        return Ok(refused(
            "use_off",
            "host-task decisions are switched off for this run",
        ));
    }
    if runner.chains().all.is_empty() {
        return Ok(refused(
            "no_provider",
            "no configured decision provider resolved for this run (a missing key or account id)",
        ));
    }
    if !runner.settings().sends(SendClass::Prompts) {
        return Ok(refused(
            "privacy",
            "privacy.send does not allow prompts, so the question may not be sent",
        ));
    }
    let node = req.node_id.as_deref().filter(|n| !n.trim().is_empty());
    let attempt = node.and_then(|n| {
        events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::AttemptStarted { node, attempt, .. } if node == n => Some(*attempt),
            _ => None,
        })
    });
    // The questions carry the asker's text too (the items and options).
    let mut qs = questions(req);
    if runner.settings().privacy.redact {
        qs = serde_json::to_value(&qs)
            .ok()
            .map(|v| redact_value(runner.redactor(), v))
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(qs);
    }
    let judge = |_: &BTreeMap<String, DecisionAnswer>| Judgement::default();
    let journal = RunJournalAppender {
        run_dir: run_dir.clone(),
    };
    let call = DecisionCall {
        site: UseSite::HostTask,
        node,
        attempt,
        state: state_parts(req),
        questions: qs,
        baseline: None,
        judge: &judge,
        join: BTreeMap::from([("kind".to_string(), json!(req.kind.as_str()))]),
        join_from: None,
        enforce: None,
    };
    Ok(match runner.decide(&journal, call) {
        DecisionOutcome::Answered {
            answers,
            mode,
            replayed,
            meta,
        } => {
            let mut out = shape(req, &answers);
            out.provider = meta.provider.unwrap_or_default();
            out.model = meta.model.unwrap_or_default();
            out.calibrated = meta.calibrated;
            out.mode = mode.as_str().to_string();
            out.cached = replayed;
            out.run_id = Some(run_id.to_string());
            out.seq = meta.seq;
            if let Some(seq) = meta.seq
                && let Some((latency, cost)) = journaled_cost(&run_dir, seq)
            {
                out.latency_ms = Some(latency);
                out.cost_usd = cost;
            }
            AskOutcome::Answered(Box::new(out))
        }
        DecisionOutcome::Skipped { reason: "budget" } => refused(
            "budget",
            "the run's decision budget is spent (`budget` in decisions.yaml); decide without it",
        ),
        DecisionOutcome::Skipped { .. } => refused(
            "use_off",
            "host-task decisions are switched off for this run",
        ),
        DecisionOutcome::Failed { error_kind } => AskOutcome::Failed { error: error_kind },
    })
}

/// The latency and cost a journaled decision recorded.
fn journaled_cost(run_dir: &Path, seq: u64) -> Option<(u64, Option<f64>)> {
    crate::event::read_all_lossy_tail(run_dir)
        .ok()?
        .into_iter()
        .rev()
        .find(|e| e.seq == seq)
        .and_then(|e| match e.payload {
            EventPayload::DecisionMade {
                latency_ms,
                cost_usd,
                ..
            } => Some((latency_ms, cost_usd)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: AskKind) -> AskRequest {
        AskRequest {
            kind,
            question: "which?".into(),
            options: vec![],
            items: vec![],
            criteria: None,
            run_id: None,
            node_id: None,
        }
    }

    #[test]
    fn malformed_requests_say_what_is_wrong() {
        let mut r = req(AskKind::Choose);
        r.options = vec!["a".into()];
        assert!(validate(&r).unwrap_err().to_string().contains("2 to 255"));
        r.options = vec!["a".into(), "a".into()];
        assert!(validate(&r).unwrap_err().to_string().contains("twice"));
        let mut r = req(AskKind::Filter);
        assert!(validate(&r).unwrap_err().to_string().contains("items"));
        r.items = vec!["x".into()];
        assert!(validate(&r).is_ok());
        let mut r = req(AskKind::Is);
        r.question = "  ".into();
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rank_orders_best_first_and_filter_keeps_the_passing_items() {
        let mut r = req(AskKind::Rank);
        r.items = vec!["a".into(), "b".into(), "c".into()];
        let answers: BTreeMap<String, DecisionAnswer> = [(0, 0.2), (1, 0.9), (2, 0.6)]
            .into_iter()
            .map(|(i, p)| {
                (
                    item_id(i),
                    DecisionAnswer {
                        p: Some(p),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let ranked: Vec<_> = shape(&r, &answers)
            .items
            .into_iter()
            .map(|i| i.item)
            .collect();
        assert_eq!(ranked, ["b", "c", "a"]);
        r.kind = AskKind::Filter;
        assert_eq!(
            shape(&r, &answers).kept.unwrap(),
            ["b".to_string(), "c".to_string()]
        );
    }
}
