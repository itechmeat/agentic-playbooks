//! `apb decisions report` (issue #165 Part 13): how well a use's journaled
//! decisions would have acted, measured against labels the run journals
//! already hold, per use and `(provider, model)`.
//!
//! The report is the gate for every enforce mode: confidence measures
//! decisiveness, not correctness, and thresholds do not transfer across
//! question types, model versions or providers. It reads journals only; it
//! never asks a model and never writes anything.
//!
//! Per group: counts and label coverage; accuracy at the use's threshold
//! against the majority class and today's behaviour; the journaled regex
//! baseline on the same items; Brier and a 10-bin ECE; a 0.05-step threshold
//! table with Wilson 95 % intervals on false-action rates; the same core
//! figures for long outputs (1,000 characters or more); `would_change`
//! accuracy; a labelled savings estimate; and whether the group is eligible
//! for enforce. Emulation providers form their own groups.

pub mod labels;
mod render;
pub mod replay;
pub mod stats;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::event::{Event, EventPayload};
use labels::{DecisionRecord, Label, Labeller, labeller_for};
use stats::{brier, ece, median, rate, round4, wilson95};

pub use render::render_text;

/// Outputs at least this long are reported separately: the Phase 0
/// evaluation found the regex baseline weakest on them.
pub const LONG_OUTPUT_CHARS: u64 = 1_000;
/// The eligibility rule's minimum of labelled decisions.
pub const MIN_LABELLED: usize = 50;
/// For a `choice` use, the minimum of labelled decisions per option.
pub const MIN_PER_OPTION: usize = 20;
/// The false-action rate a use must stay under, unless
/// `uses.<name>.thresholds.false_action_target` says otherwise.
pub const DEFAULT_FALSE_ACTION_TARGET: f64 = 0.05;
/// The provider kind of an LLM emulation (reported as its own group).
pub const EMULATION_KIND: &str = "llm_emulation";
/// Printed when nothing matches.
pub const NO_DECISIONS: &str = "no decisions recorded";

/// What to include.
#[derive(Debug, Clone, Default)]
pub struct ReportFilter {
    pub use_site: Option<String>,
    /// Epoch milliseconds: decisions journaled at or after it.
    pub since_ms: Option<u128>,
    pub playbook: Option<String>,
    pub provider: Option<String>,
}

impl ReportFilter {
    fn keeps(&self, r: &DecisionRecord) -> bool {
        self.use_site.as_ref().is_none_or(|u| &r.use_site == u)
            && self.since_ms.is_none_or(|s| r.ts >= s)
            && self.playbook.as_ref().is_none_or(|p| &r.playbook == p)
            && self
                .provider
                .as_ref()
                .is_none_or(|p| r.provider.as_ref() == Some(p))
    }
}

/// The machine-side settings the report reads: the use thresholds and
/// targets from `decisions.yaml` and the stored thresholds.
#[derive(Debug, Clone, Default)]
pub struct ReportSettings {
    /// `uses.<name>.thresholds` from `decisions.yaml`, when it loads.
    pub use_thresholds: BTreeMap<String, BTreeMap<String, f64>>,
    pub stored: Vec<apb_core::decision_thresholds::StoredThreshold>,
}

impl ReportSettings {
    /// Reads both files under `config_dir`; a file that does not load
    /// counts as absent (the report then uses the defaults).
    pub fn load(config_dir: Option<&Path>) -> Self {
        let Some(dir) = config_dir else {
            return Self::default();
        };
        let use_thresholds = apb_core::decisions::load_file(dir)
            .ok()
            .flatten()
            .map(|eff| {
                eff.uses
                    .into_iter()
                    .map(|(k, u)| (k, u.thresholds))
                    .collect()
            })
            .unwrap_or_default();
        let stored = apb_core::decision_thresholds::load_in(dir).unwrap_or_default();
        ReportSettings {
            use_thresholds,
            stored,
        }
    }

    fn stored_for(&self, use_site: &str, provider: &str, model: &str) -> Option<f64> {
        self.stored
            .iter()
            .find(|t| t.use_name == use_site && t.provider == provider && t.model == model)
            .map(|t| t.threshold)
    }

    fn target(&self, use_site: &str) -> f64 {
        self.use_thresholds
            .get(use_site)
            .and_then(|t| t.get("false_action_target"))
            .copied()
            .unwrap_or(DEFAULT_FALSE_ACTION_TARGET)
    }

    /// The use's configured threshold: the completion check's
    /// `final_result`, else the labeller's default.
    fn configured(&self, l: &dyn Labeller) -> f64 {
        let name = match l.use_site() {
            "completion_check" => "final_result",
            _ => "threshold",
        };
        self.use_thresholds
            .get(l.use_site())
            .and_then(|t| t.get(name))
            .copied()
            .unwrap_or_else(|| l.default_threshold())
    }
}

/// One run's journal, read once.
pub struct RunJournal {
    pub run_id: String,
    pub playbook: String,
    pub events: Vec<Event>,
    /// Provider id to kind, from the run manifest's decisions block.
    pub provider_kinds: BTreeMap<String, String>,
}

impl RunJournal {
    /// Reads `run_dir`; `None` when its journal does not read.
    pub fn load(run_dir: &Path) -> Option<Self> {
        let run_id = run_dir.file_name()?.to_string_lossy().to_string();
        let events = crate::event::read_journal(run_dir).ok()?.events;
        let playbook = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let provider_kinds = crate::manifest::read(run_dir)
            .ok()
            .flatten()
            .and_then(|m| m.decisions)
            .map(|d| {
                d.providers
                    .into_iter()
                    .filter_map(|p| {
                        let kind = serde_json::to_value(p.kind).ok()?.as_str()?.to_string();
                        Some((p.id, kind))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(RunJournal {
            run_id,
            playbook,
            events,
            provider_kinds,
        })
    }

    /// The run's decisions, with the provider kind filled in.
    pub fn records(&self) -> Vec<DecisionRecord> {
        self.events
            .iter()
            .filter_map(|e| DecisionRecord::from_event(&self.run_id, &self.playbook, e))
            .map(|mut r| {
                r.provider_kind = r
                    .provider
                    .as_ref()
                    .and_then(|p| self.provider_kinds.get(p))
                    .cloned();
                r
            })
            .collect()
    }

    /// Wall time of every finished attempt that journaled one.
    fn attempt_durations(&self) -> impl Iterator<Item = u64> + '_ {
        self.events.iter().filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                duration_ms: Some(d),
                ..
            } => Some(*d),
            _ => None,
        })
    }
}

/// Every run directory under the given project roots.
pub fn run_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join(".apb/runs")) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        dirs.sort();
        out.extend(dirs);
    }
    out
}

// --- the report -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionsReport {
    pub runs: usize,
    pub decisions: usize,
    /// The median wall time of an agent attempt over the runs read, the
    /// unit of the savings estimate.
    pub median_attempt_ms: Option<u64>,
    pub groups: Vec<GroupReport>,
    /// [`NO_DECISIONS`] when no decision matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One use with one `(provider, model)`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupReport {
    #[serde(rename = "use")]
    pub use_site: String,
    pub provider: String,
    pub model: String,
    pub provider_kind: Option<String>,
    pub calibrated: bool,
    /// An LLM emulation: its own group, never pooled with a decision model.
    pub emulation: bool,
    pub label_source: String,
    pub decisions: usize,
    pub errors: usize,
    pub answered: usize,
    pub labelled: usize,
    /// `labelled / answered`.
    pub label_coverage: Option<f64>,
    pub act_labels: usize,
    pub keep_labels: usize,
    /// Why the rest stay unlabelled, with counts.
    pub unlabelled: BTreeMap<String, usize>,
    /// The threshold the figures below use.
    pub threshold: f64,
    /// `stored` (decisions-thresholds.yaml) or `configured` (decisions.yaml
    /// or the use default).
    pub threshold_source: String,
    pub all: SliceMetrics,
    /// Outputs of [`LONG_OUTPUT_CHARS`] or more.
    pub long_outputs: SliceMetrics,
    /// Decisions journaled without an output length (older journals).
    pub output_length_unknown: usize,
    pub brier: Option<f64>,
    /// 10 equal-width bins.
    pub ece: Option<f64>,
    pub would_change: Option<Accuracy>,
    pub table: Vec<ThresholdRow>,
    /// The lowest-false-action row with the highest recall whose
    /// false-action rate is under the target; `None` when no row is.
    pub suggested_threshold: Option<f64>,
    pub false_action_target: f64,
    pub savings: Option<Savings>,
    pub cost_usd: f64,
    pub latency_ms_total: u64,
    pub eligible: bool,
    pub eligibility: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Accuracy {
    pub items: usize,
    pub accuracy: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FalseActions {
    pub count: usize,
    /// Decisions labelled `keep`: where acting would have been wrong.
    pub of: usize,
    pub rate: Option<f64>,
    pub ci95: Option<(f64, f64)>,
}

/// The core figures over a slice of the labelled decisions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SliceMetrics {
    pub labelled: usize,
    pub accuracy: Option<f64>,
    pub recall: Option<f64>,
    pub majority_label: String,
    pub majority_accuracy: Option<f64>,
    pub today_behaviour: String,
    pub today_accuracy: Option<f64>,
    /// The journaled regex verdict on the same items.
    pub regex: Accuracy,
    pub false_actions: FalseActions,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThresholdRow {
    pub threshold: f64,
    /// Share of labelled decisions the use would act on.
    pub coverage: Option<f64>,
    pub accuracy: Option<f64>,
    pub recall: Option<f64>,
    pub false_action_rate: Option<f64>,
    pub false_action_ci95: Option<(f64, f64)>,
}

/// A labelled estimate, not a measurement: what acting at the threshold
/// would have saved and cost on these decisions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Savings {
    pub threshold: f64,
    /// Correct actions: each avoids one attempt.
    pub avoidable_attempts: usize,
    pub median_attempt_ms: u64,
    pub saved_ms: u64,
    /// Wrong actions: each costs one needless attempt.
    pub false_actions: usize,
    pub lost_ms: u64,
    pub decision_latency_ms: u64,
    pub decision_cost_usd: f64,
}

/// One labelled decision.
struct Item<'a> {
    r: &'a DecisionRecord,
    act: bool,
}

fn act_of(label: &Label) -> Option<bool> {
    match label {
        Label::Act => Some(true),
        Label::Keep => Some(false),
        Label::Unlabelled(_) => None,
    }
}

fn slice(l: &dyn Labeller, items: &[Item], threshold: f64) -> SliceMetrics {
    let n = items.len();
    let acts = items.iter().filter(|i| i.act).count();
    let keeps = n - acts;
    let decided: Vec<(bool, bool)> = items
        .iter()
        .filter_map(|i| l.acts_at(i.r, threshold).map(|a| (a, i.act)))
        .collect();
    let correct = decided.iter().filter(|(a, y)| a == y).count();
    let tp = decided.iter().filter(|(a, y)| *a && *y).count();
    let fp = decided.iter().filter(|(a, y)| *a && !*y).count();
    let negatives = decided.iter().filter(|(_, y)| !*y).count();
    let positives = decided.len() - negatives;
    let (today_label, today_name) = l.today();
    let today_act = today_label == Label::Act;
    let today_hits = items.iter().filter(|i| i.act == today_act).count();
    let regex: Vec<bool> = items
        .iter()
        .filter_map(|i| i.r.baseline.as_ref().map(|b| b.regex_flag == i.act))
        .collect();
    SliceMetrics {
        labelled: n,
        accuracy: rate(correct, decided.len()),
        recall: rate(tp, positives),
        majority_label: if acts > keeps { "act" } else { "keep" }.to_string(),
        majority_accuracy: rate(acts.max(keeps), n),
        today_behaviour: today_name.to_string(),
        today_accuracy: rate(today_hits, n),
        regex: Accuracy {
            items: regex.len(),
            accuracy: rate(regex.iter().filter(|h| **h).count(), regex.len()),
        },
        false_actions: FalseActions {
            count: fp,
            of: negatives,
            rate: rate(fp, negatives),
            ci95: wilson95(fp, negatives),
        },
    }
}

fn table(l: &dyn Labeller, items: &[Item]) -> Vec<ThresholdRow> {
    (1..20)
        .map(|k| {
            let t = round4(k as f64 * 0.05);
            let decided: Vec<(bool, bool)> = items
                .iter()
                .filter_map(|i| l.acts_at(i.r, t).map(|a| (a, i.act)))
                .collect();
            let acted = decided.iter().filter(|(a, _)| *a).count();
            let correct = decided.iter().filter(|(a, y)| a == y).count();
            let tp = decided.iter().filter(|(a, y)| *a && *y).count();
            let fp = decided.iter().filter(|(a, y)| *a && !*y).count();
            let negatives = decided.iter().filter(|(_, y)| !*y).count();
            ThresholdRow {
                threshold: t,
                coverage: rate(acted, decided.len()),
                accuracy: rate(correct, decided.len()),
                recall: rate(tp, decided.len() - negatives),
                false_action_rate: rate(fp, negatives),
                false_action_ci95: wilson95(fp, negatives),
            }
        })
        .collect()
}

/// The row with the highest recall among those under the target (the
/// lowest threshold of equals).
fn suggest(rows: &[ThresholdRow], target: f64) -> Option<f64> {
    rows.iter()
        .filter(|r| r.false_action_rate.is_some_and(|f| f < target))
        .filter(|r| r.recall.is_some_and(|x| x > 0.0))
        .fold(None::<&ThresholdRow>, |best, r| match best {
            Some(b) if b.recall >= r.recall => Some(b),
            _ => Some(r),
        })
        .map(|r| r.threshold)
}

struct GroupInput<'a> {
    labeller: &'a dyn Labeller,
    records: Vec<(&'a DecisionRecord, Label)>,
}

fn group_report(
    key: &GroupKey,
    input: GroupInput,
    settings: &ReportSettings,
    median_attempt_ms: Option<u64>,
) -> GroupReport {
    let l = input.labeller;
    let decisions = input.records.len();
    let errors = input
        .records
        .iter()
        .filter(|(r, _)| r.error.is_some())
        .count();
    let answered: Vec<&(&DecisionRecord, Label)> =
        input.records.iter().filter(|(r, _)| r.answered()).collect();
    let mut unlabelled = BTreeMap::new();
    let mut items = Vec::new();
    for (r, label) in &answered {
        match act_of(label) {
            Some(act) => items.push(Item { r, act }),
            None => {
                if let Label::Unlabelled(why) = label {
                    *unlabelled.entry((*why).to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    let stored = settings.stored_for(&key.use_site, &key.provider, &key.model);
    let (threshold, threshold_source) = match stored {
        Some(t) => (t, "stored"),
        None => (settings.configured(l), "configured"),
    };
    let target = settings.target(&key.use_site);
    let all = slice(l, &items, threshold);
    let long: Vec<Item> = items
        .iter()
        .filter(|i| i.r.output_chars.is_some_and(|c| c >= LONG_OUTPUT_CHARS))
        .map(|i| Item { r: i.r, act: i.act })
        .collect();
    let probs: Vec<(f64, bool)> = items
        .iter()
        .filter_map(|i| l.act_probability(i.r).map(|p| (p, i.act)))
        .collect();
    let would: Vec<bool> = items
        .iter()
        .filter_map(|i| i.r.would_change.map(|w| w == i.act))
        .collect();
    let rows = table(l, &items);
    let cost_usd = round_usd(input.records.iter().filter_map(|(r, _)| r.cost_usd).sum());
    let latency_ms_total: u64 = input.records.iter().map(|(r, _)| r.latency_ms).sum();
    let savings = median_attempt_ms.filter(|_| !items.is_empty()).map(|m| {
        let tp = items
            .iter()
            .filter(|i| i.act && l.acts_at(i.r, threshold) == Some(true))
            .count();
        let fp = items
            .iter()
            .filter(|i| !i.act && l.acts_at(i.r, threshold) == Some(true))
            .count();
        Savings {
            threshold,
            avoidable_attempts: tp,
            median_attempt_ms: m,
            saved_ms: tp as u64 * m,
            false_actions: fp,
            lost_ms: fp as u64 * m,
            decision_latency_ms: latency_ms_total,
            decision_cost_usd: cost_usd,
        }
    });
    let options: BTreeMap<String, usize> = items
        .iter()
        .filter_map(|i| {
            let label = if i.act { Label::Act } else { Label::Keep };
            l.label_option(i.r, &label)
        })
        .fold(BTreeMap::new(), |mut m, o| {
            *m.entry(o).or_insert(0) += 1;
            m
        });
    let mut notes = Vec::new();
    if stored.is_none()
        && let Some(other) = settings
            .stored
            .iter()
            .find(|t| t.use_name == key.use_site && t.provider == key.provider)
    {
        notes.push(format!(
            "the threshold stored for {}/{} does not carry over to model {}: a new shadow period is needed",
            other.provider, other.model, key.model
        ));
    }
    if !key.calibrated {
        notes
            .push("uncalibrated provider: enforce also needs allow_uncalibrated: true".to_string());
    }
    let (eligible, eligibility) = eligibility(&EligibilityInput {
        stored,
        key,
        all: &all,
        options: &options,
        target,
        labeller_pending: l.pending(),
    });
    GroupReport {
        use_site: key.use_site.clone(),
        provider: key.provider.clone(),
        model: key.model.clone(),
        provider_kind: key.provider_kind.clone(),
        calibrated: key.calibrated,
        emulation: key.emulation,
        label_source: l.label_source().to_string(),
        decisions,
        errors,
        answered: answered.len(),
        labelled: items.len(),
        label_coverage: rate(items.len(), answered.len()),
        act_labels: items.iter().filter(|i| i.act).count(),
        keep_labels: items.iter().filter(|i| !i.act).count(),
        unlabelled,
        threshold,
        threshold_source: threshold_source.to_string(),
        long_outputs: slice(l, &long, threshold),
        output_length_unknown: input
            .records
            .iter()
            .filter(|(r, _)| r.output_chars.is_none())
            .count(),
        all,
        brier: brier(&probs),
        ece: ece(&probs, 10),
        would_change: (!would.is_empty()).then(|| Accuracy {
            items: would.len(),
            accuracy: rate(would.iter().filter(|h| **h).count(), would.len()),
        }),
        suggested_threshold: suggest(&rows, target),
        table: rows,
        false_action_target: target,
        savings,
        cost_usd,
        latency_ms_total,
        eligible,
        eligibility,
        notes,
    }
}

/// Dollars to eight decimals: a decision costs fractions of a cent.
fn round_usd(x: f64) -> f64 {
    (x * 1e8).round() / 1e8
}

struct EligibilityInput<'a> {
    stored: Option<f64>,
    key: &'a GroupKey,
    all: &'a SliceMetrics,
    options: &'a BTreeMap<String, usize>,
    target: f64,
    labeller_pending: bool,
}

/// The promotion rule (Part 14 enforces it): a stored threshold, at least
/// [`MIN_LABELLED`] labelled decisions ([`MIN_PER_OPTION`] per option for a
/// choice use), accuracy above both the majority class and today's
/// behaviour, and a false-action rate under the use's target. Returns
/// whether all hold, and one line per rule that does not (or the one line
/// that all do).
fn eligibility(i: &EligibilityInput) -> (bool, Vec<String>) {
    let mut why = Vec::new();
    if i.labeller_pending {
        why.push("no labeller for this use yet".to_string());
    }
    if i.stored.is_none() {
        why.push(format!(
            "no stored threshold for {}/{} (apb decisions thresholds set)",
            i.key.provider, i.key.model
        ));
    }
    if i.all.labelled < MIN_LABELLED {
        why.push(format!("{} labelled, needs {MIN_LABELLED}", i.all.labelled));
    }
    for (option, n) in i.options {
        if *n < MIN_PER_OPTION {
            why.push(format!("{n} labelled `{option}`, needs {MIN_PER_OPTION}"));
        }
    }
    let beats = |baseline: Option<f64>| match (i.all.accuracy, baseline) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    };
    if !beats(i.all.majority_accuracy) {
        why.push("accuracy not above the majority class".to_string());
    }
    if !beats(i.all.today_accuracy) {
        why.push(format!(
            "accuracy not above today's behaviour ({})",
            i.all.today_behaviour
        ));
    }
    match i.all.false_actions.rate {
        Some(r) if r < i.target => {}
        Some(r) => why.push(format!(
            "false-action rate {:.1}% not under the {:.1}% target",
            r * 100.0,
            i.target * 100.0
        )),
        None => why.push("no decision labelled keep to measure false actions".to_string()),
    }
    if why.is_empty() {
        (true, vec!["all promotion rules hold".to_string()])
    } else {
        (false, why)
    }
}

/// What groups decisions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    /// Emulation groups sort after decision models.
    emulation: bool,
    use_site: String,
    provider: String,
    model: String,
    calibrated: bool,
    provider_kind: Option<String>,
}

impl GroupKey {
    fn of(r: &DecisionRecord) -> Self {
        let emulation = match r.provider_kind.as_deref() {
            Some(kind) => kind == EMULATION_KIND,
            None => !r.calibrated && r.provider.is_some(),
        };
        GroupKey {
            emulation,
            use_site: r.use_site.clone(),
            provider: r.provider.clone().unwrap_or_else(|| "none".to_string()),
            model: r.model.clone().unwrap_or_else(|| "unknown".to_string()),
            calibrated: r.calibrated,
            provider_kind: r.provider_kind.clone(),
        }
    }
}

/// Builds the report over already-read journals.
pub fn build(
    journals: &[RunJournal],
    filter: &ReportFilter,
    settings: &ReportSettings,
) -> DecisionsReport {
    let mut durations: Vec<u64> = journals
        .iter()
        .flat_map(RunJournal::attempt_durations)
        .collect();
    let median_attempt_ms = median(&mut durations);
    let mut labellers: BTreeMap<String, Box<dyn Labeller>> = BTreeMap::new();
    let mut labelled: Vec<(DecisionRecord, Label)> = Vec::new();
    for j in journals {
        for r in j.records().into_iter().filter(|r| filter.keeps(r)) {
            let l = labellers
                .entry(r.use_site.clone())
                .or_insert_with(|| labeller_for(&r.use_site));
            let label = if r.answered() {
                l.label(&r, &j.events)
            } else {
                Label::Unlabelled("not answered")
            };
            labelled.push((r, label));
        }
    }
    // Errors carry no model: they join the group of an answered decision of
    // the same provider, in the same run when there is one.
    let mut groups: BTreeMap<GroupKey, Vec<(&DecisionRecord, Label)>> = BTreeMap::new();
    let mut like_in_run: BTreeMap<(&str, &str, &str), GroupKey> = BTreeMap::new();
    let mut like_any: BTreeMap<(&str, &str), GroupKey> = BTreeMap::new();
    for (r, _) in labelled.iter().filter(|(r, _)| r.answered()) {
        let provider = r.provider.as_deref().unwrap_or_default();
        like_in_run
            .entry((r.run_id.as_str(), r.use_site.as_str(), provider))
            .or_insert_with(|| GroupKey::of(r));
        like_any
            .entry((r.use_site.as_str(), provider))
            .or_insert_with(|| GroupKey::of(r));
    }
    for (r, label) in &labelled {
        let provider = r.provider.as_deref().unwrap_or_default();
        let key = if r.model.is_some() {
            GroupKey::of(r)
        } else {
            like_in_run
                .get(&(r.run_id.as_str(), r.use_site.as_str(), provider))
                .or_else(|| like_any.get(&(r.use_site.as_str(), provider)))
                .cloned()
                .unwrap_or_else(|| GroupKey::of(r))
        };
        groups.entry(key).or_default().push((r, label.clone()));
    }
    let decisions = labelled.len();
    let groups: Vec<GroupReport> = groups
        .into_iter()
        .map(|(key, records)| {
            let labeller = labellers
                .get(&key.use_site)
                .map(|b| b.as_ref())
                .expect("a labeller per use seen");
            group_report(
                &key,
                GroupInput { labeller, records },
                settings,
                median_attempt_ms,
            )
        })
        .collect();
    DecisionsReport {
        runs: journals.len(),
        decisions,
        median_attempt_ms,
        note: (decisions == 0).then(|| NO_DECISIONS.to_string()),
        groups,
    }
}

/// Reads every run under `roots` and builds the report.
pub fn report(
    roots: &[PathBuf],
    filter: &ReportFilter,
    settings: &ReportSettings,
) -> DecisionsReport {
    let journals: Vec<RunJournal> = run_dirs(roots)
        .iter()
        .filter_map(|d| RunJournal::load(d))
        .collect();
    build(&journals, filter, settings)
}

/// Parses `--since`: a duration back from now (`7d`, `24h`, `90m`) or a
/// UTC date (`2026-09-20`). Epoch milliseconds.
pub fn parse_since(s: &str, now_ms: u128) -> Option<u128> {
    let s = s.trim();
    if let Some((y, rest)) = s.split_once('-')
        && let Some((m, d)) = rest.split_once('-')
    {
        let (y, m, d): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let days = days_from_civil(y, m, d);
        return u128::try_from(days).ok().map(|d| d * 86_400_000);
    }
    let secs = apb_core::duration::parse_duration_str(s)?;
    // A bare number is ambiguous here: require a unit.
    if s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(now_ms.saturating_sub(u128::from(secs) * 1000))
}

/// Days since 1970-01-01 of a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`, public domain).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests;
