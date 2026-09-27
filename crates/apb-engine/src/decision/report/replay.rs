//! `apb decisions replay` (issue #165 Part 13): re-asks journaled decisions
//! against another provider, for evaluation only.
//!
//! Only decisions whose run kept its debug state (`privacy.debug_state`,
//! `runs/<id>/decisions/<seq>.json`) can be replayed: the journal itself
//! holds no state text. The state in that file is the one that was sent,
//! already redacted and clipped, so it is sent as is.
//!
//! Replay never writes a journal, a debug file or anything under a run: the
//! results go to `<config_dir>/decisions-replay/`. It is evaluation (how the
//! other provider would have answered, next to the labels), never an export
//! of outputs for training: the provider terms forbid training an imitating
//! model on outputs, and apb has no such feature.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::decisions::EffectiveDecisions;
use apb_decide::{DecisionRequest, Question, UseSite};
use serde::Serialize;
use serde_json::Value;

use super::labels::{DecisionRecord, Label, labeller_for};
use super::stats::rate;
use super::{ReportFilter, RunJournal, run_dirs};
use crate::decision::{DecisionRunner, compact_answers};
use crate::event::DecisionAnswer;

/// The directory under the config dir that replay results go to.
pub const REPLAY_DIR: &str = "decisions-replay";

/// One replayed decision.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayItem {
    pub run_id: String,
    pub seq: u64,
    #[serde(rename = "use")]
    pub use_site: String,
    pub node: Option<String>,
    pub original_provider: Option<String>,
    pub original_model: Option<String>,
    pub original: BTreeMap<String, DecisionAnswer>,
    pub model: Option<String>,
    pub answers: BTreeMap<String, DecisionAnswer>,
    pub latency_ms: u64,
    pub error: Option<String>,
    /// Whether the use would act on the original answer, and on the
    /// replayed one, at its default threshold.
    pub original_acts: Option<bool>,
    pub replay_acts: Option<bool>,
    /// Both lead the use to the same action.
    pub agrees: Option<bool>,
    /// `act`, `keep` or absent (unlabelled).
    pub label: Option<String>,
}

/// What a replay did, as printed and as saved.
#[derive(Debug, Clone, Serialize)]
pub struct ReplaySummary {
    pub provider: String,
    /// Decisions that matched the filter.
    pub matched: usize,
    /// Of those, the ones with a debug state to replay.
    pub with_state: usize,
    pub asked: usize,
    pub errors: usize,
    pub agreement: Option<f64>,
    pub labelled: usize,
    pub original_accuracy: Option<f64>,
    pub replay_accuracy: Option<f64>,
    /// Where the results were written.
    pub results: Option<PathBuf>,
    pub items: Vec<ReplayItem>,
}

/// Why a replay did not start.
#[derive(Debug, PartialEq)]
pub enum ReplayError {
    NoProvider,
    UnknownProvider(String),
    Config(String),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::NoProvider => f.write_str(
                "replay needs --provider <id>: the provider to re-ask, from decisions.yaml",
            ),
            ReplayError::UnknownProvider(id) => {
                write!(f, "no provider `{id}` in decisions.yaml")
            }
            ReplayError::Config(e) => write!(f, "decisions.yaml does not load: {e}"),
        }
    }
}

/// The debug state of one decision: `(state, state_order, questions)`.
fn debug_state(
    run_dir: &Path,
    seq: u64,
) -> Option<(Value, Vec<String>, BTreeMap<String, Question>)> {
    let raw =
        std::fs::read_to_string(run_dir.join("decisions").join(format!("{seq}.json"))).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let questions = serde_json::from_value(v.get("questions")?.clone()).ok()?;
    let order = v
        .get("state_order")
        .and_then(|o| serde_json::from_value(o.clone()).ok())
        .unwrap_or_default();
    Some((v.get("state")?.clone(), order, questions))
}

fn use_site(name: &str) -> Option<UseSite> {
    serde_json::from_value(Value::String(name.to_string())).ok()
}

/// Replays the matching decisions of the runs under `roots` against the
/// provider `provider` of `decisions.yaml` in `config_dir`, at most `max`
/// requests, and writes the results under `<config_dir>/decisions-replay/`.
pub fn replay(
    roots: &[PathBuf],
    config_dir: &Path,
    provider: Option<&str>,
    filter: &ReportFilter,
    max: usize,
) -> Result<ReplaySummary, ReplayError> {
    let provider = provider
        .filter(|p| !p.trim().is_empty())
        .ok_or(ReplayError::NoProvider)?;
    let settings = apb_core::decisions::load_file(config_dir)
        .map_err(ReplayError::Config)?
        .ok_or_else(|| ReplayError::Config("no decisions.yaml".into()))?;
    let spec = settings
        .providers
        .iter()
        .find(|p| p.id == provider)
        .cloned()
        .ok_or_else(|| ReplayError::UnknownProvider(provider.to_string()))?;
    let runner = DecisionRunner::for_replay(
        EffectiveDecisions {
            providers: vec![spec],
            ..settings
        },
        config_dir,
    );
    let mut summary = ReplaySummary {
        provider: provider.to_string(),
        matched: 0,
        with_state: 0,
        asked: 0,
        errors: 0,
        agreement: None,
        labelled: 0,
        original_accuracy: None,
        replay_accuracy: None,
        results: None,
        items: Vec::new(),
    };
    for run_dir in run_dirs(roots) {
        let Some(journal) = RunJournal::load(&run_dir) else {
            continue;
        };
        for r in journal.records() {
            if !filter.keeps(&r) || !r.answered() {
                continue;
            }
            summary.matched += 1;
            let (Some(site), Some((state, state_order, questions))) =
                (use_site(&r.use_site), debug_state(&run_dir, r.seq))
            else {
                continue;
            };
            summary.with_state += 1;
            if summary.asked >= max {
                continue;
            }
            summary.asked += 1;
            let request = DecisionRequest {
                use_site: site,
                state,
                state_order,
                questions,
            };
            summary.items.push(ask(&runner, &r, &request, &journal));
        }
    }
    summarize(&mut summary);
    let dir = config_dir.join(REPLAY_DIR);
    let path = dir.join(format!("{}-{provider}.json", apb_core::clock::now_ms_u64()));
    if std::fs::create_dir_all(&dir).is_ok()
        && let Ok(text) = serde_json::to_string_pretty(&summary)
        && apb_core::fsutil::atomic_write(&path, text.as_bytes()).is_ok()
    {
        summary.results = Some(path);
    }
    Ok(summary)
}

fn ask(
    runner: &DecisionRunner,
    r: &DecisionRecord,
    request: &DecisionRequest,
    journal: &RunJournal,
) -> ReplayItem {
    let labeller = labeller_for(&r.use_site);
    let label = labeller.label(r, &journal.events);
    let (model, answers, latency_ms, error) = match runner.ask_unjournaled(request) {
        Ok(resp) => (
            Some(resp.model.clone()),
            compact_answers(&resp.answers),
            resp.latency_ms,
            None,
        ),
        Err(e) => (None, BTreeMap::new(), 0, Some(e.kind().to_string())),
    };
    let t = labeller.default_threshold();
    let replayed = DecisionRecord {
        answers: answers.clone(),
        ..r.clone()
    };
    let original_acts = labeller.acts_at(r, t);
    let replay_acts = labeller.acts_at(&replayed, t).filter(|_| error.is_none());
    let agrees = original_acts.zip(replay_acts).map(|(a, b)| a == b);
    ReplayItem {
        run_id: r.run_id.clone(),
        seq: r.seq,
        use_site: r.use_site.clone(),
        node: r.node.clone(),
        original_provider: r.provider.clone(),
        original_model: r.model.clone(),
        original: r.answers.clone(),
        model,
        answers,
        latency_ms,
        error,
        original_acts,
        replay_acts,
        agrees,
        label: match label {
            Label::Act => Some("act".into()),
            Label::Keep => Some("keep".into()),
            Label::Unlabelled(_) => None,
        },
    }
}

fn summarize(s: &mut ReplaySummary) {
    s.errors = s.items.iter().filter(|i| i.error.is_some()).count();
    let agree: Vec<bool> = s.items.iter().filter_map(|i| i.agrees).collect();
    s.agreement = rate(agree.iter().filter(|a| **a).count(), agree.len());
    let hits = |pick: fn(&ReplayItem) -> Option<bool>| {
        let scored: Vec<bool> = s
            .items
            .iter()
            .filter_map(|i| Some(pick(i)? == (i.label.as_deref()? == "act")))
            .collect();
        (scored.iter().filter(|h| **h).count(), scored.len())
    };
    let original = hits(|i| i.original_acts);
    let replayed = hits(|i| i.replay_acts);
    s.labelled = original.1;
    s.original_accuracy = rate(original.0, original.1);
    s.replay_accuracy = rate(replayed.0, replayed.1);
}
