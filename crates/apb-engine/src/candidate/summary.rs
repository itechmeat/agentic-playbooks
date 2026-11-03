//! The candidate summary a person reads before promoting a candidate: three
//! axes kept apart (fitness on this machine, the definition, quality), the
//! items that block a promotion or need it acknowledged, and the id an
//! acknowledgement names.
//!
//! The summary is computed at request time ([`candidate_summary`]). A
//! person's decision goes through [`decide_by_person`]: a promotion
//! ([`super::PersonDecision::Promote`]) names the version and digest the
//! person saw and, when there were attention items, the
//! [`CandidateSummary::summary_id`] the person acknowledged. Under the
//! candidate lock the definition and quality axes are taken again and the
//! promotion is refused with a [`PromoteRefusal`] when either no longer
//! holds. Fitness (`fit::assess`, which may probe agent CLIs) is taken
//! before the lock and kept: its items still count in the id, so a change
//! between view and click asks for a new acknowledgement.
//!
//! Three axes, never merged:
//!
//! - **fitness** is the fit report of the candidate's own version on this
//!   machine; a problem there needs acknowledging and never blocks;
//! - **definition** is the validator on the candidate, the diff from
//!   `current`, and the newest quick and smoke scenario results. A result
//!   is fresh only for the candidate's trust digest and the scenario file
//!   as it is now. A validator error, or a run of the candidate's version
//!   that recorded another digest (the content was edited in place),
//!   blocks; a failed, invalid, incomplete, stale or missing scenario
//!   result needs acknowledging;
//! - **quality** is the candidate comparison as the candidate card shows
//!   it; a verdict other than `promote` needs acknowledging, and a policy
//!   without a quality gate reads `unknown`.
//!
//! Automatic promotion by policy never reads the summary.

use std::path::Path;

use apb_core::quality::QualityRecord;
use apb_core::quality::compare::{QualityDecision, Verdict as QualityVerdict};
use apb_core::registry::Registry;
use apb_core::scenario::ScenarioLevel;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::validate::{IssueView, Severity, ValidationContext};
use apb_core::versioning::VersionDiff;
use serde::{Deserialize, Serialize};

use super::CandidateQualityView;
use super::quality::PersonDecision;
pub use super::quality::{BadVersionDigest, VersionDigest};
use crate::error::EngineError;
use crate::event::EventPayload;
use crate::fit::FitReport;
use crate::next_action::start::StartAction;
use crate::scenario::report::Verdict as ScenarioVerdict;
use crate::scenario::store::IndexLine;

/// The state of one axis.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisState {
    Ok,
    /// A person's promotion needs the summary acknowledged.
    Attention,
    /// A person's promotion is refused.
    Blocking,
    /// Could not be computed.
    Unknown,
}

/// Which axis an item belongs to.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisName {
    /// Whether the candidate can start on this machine.
    Fitness,
    /// The candidate's definition: validation, the diff, scenario results.
    Definition,
    /// How the candidate's graded trials compare with `current`.
    Quality,
}

/// What one summary item is about. Blocking codes refuse a person's
/// promotion; every other code needs the summary acknowledged.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryCode {
    /// The validator reports an error on the candidate.
    ValidatorError,
    /// The candidate's content changed since it became the candidate.
    CandidateEdited,
    /// The latest scenario result for the candidate's digest failed.
    ScenarioFailed,
    /// The latest scenario result for the candidate's digest is invalid.
    ScenarioInvalid,
    /// The latest scenario run for the candidate's digest did not end.
    ScenarioIncomplete,
    /// The latest scenario result is for another digest of the playbook
    /// or of the scenario.
    ScenarioStale,
    /// A scenario has no result for the candidate's digest at any level.
    ScenarioNotRun,
    /// The quality comparison does not say promote.
    QualityNotPromote,
    /// The candidate would not start as it is on this machine.
    FitnessProblem,
}

impl SummaryCode {
    /// Every code, in declaration order.
    pub const ALL: [SummaryCode; 9] = [
        SummaryCode::ValidatorError,
        SummaryCode::CandidateEdited,
        SummaryCode::ScenarioFailed,
        SummaryCode::ScenarioInvalid,
        SummaryCode::ScenarioIncomplete,
        SummaryCode::ScenarioStale,
        SummaryCode::ScenarioNotRun,
        SummaryCode::QualityNotPromote,
        SummaryCode::FitnessProblem,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SummaryCode::ValidatorError => "validator_error",
            SummaryCode::CandidateEdited => "candidate_edited",
            SummaryCode::ScenarioFailed => "scenario_failed",
            SummaryCode::ScenarioInvalid => "scenario_invalid",
            SummaryCode::ScenarioIncomplete => "scenario_incomplete",
            SummaryCode::ScenarioStale => "scenario_stale",
            SummaryCode::ScenarioNotRun => "scenario_not_run",
            SummaryCode::QualityNotPromote => "quality_not_promote",
            SummaryCode::FitnessProblem => "fitness_problem",
        }
    }

    /// Whether an item with this code refuses a person's promotion (else
    /// it needs the summary acknowledged). Fitness never blocks.
    pub fn blocks(self) -> bool {
        matches!(
            self,
            SummaryCode::ValidatorError | SummaryCode::CandidateEdited
        )
    }
}

/// One thing a person should know before promoting.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryItem {
    pub axis: AxisName,
    pub code: SummaryCode,
    /// One line for people.
    pub detail: String,
}

/// One axis: its state and what it rests on.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Axis<T> {
    pub state: AxisState,
    pub detail: T,
}

/// Fitness on this machine: the fit report of the candidate's own version
/// and the start actions its refusals render as.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FitnessAxis {
    pub report: FitReport,
    #[serde(default)]
    pub start_actions: Vec<StartAction>,
}

/// The candidate's definition: the validator's issues, the diff from
/// `current`, the scenario results and the scenarios with no fresh result.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefinitionAxis {
    pub issues: Vec<IssueView>,
    /// Absent when either definition could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub diff: Option<VersionDiff>,
    pub scenarios: Vec<ScenarioResultRef>,
    /// Scenario names with no fresh result at any level.
    pub not_run: Vec<String>,
}

/// Quality: the candidate comparison as the playbook surfaces show it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QualityAxis {
    pub view: CandidateQualityView,
}

/// The latest result of one scenario at one level, and whether it is
/// about the version and scenario as they are now.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioResultRef {
    pub scenario: String,
    pub level: ScenarioLevel,
    pub verdict: ScenarioVerdict,
    pub version: String,
    pub playbook_digest: String,
    pub scenario_digest: String,
    pub at_ms: u64,
    /// The result's playbook digest is the version's digest and its
    /// scenario digest the scenario file's now.
    pub fresh: bool,
    pub run_id: String,
}

/// What a person reads before promoting the candidate of a playbook.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateSummary {
    /// sha256 over the basis (the two version digests, the scenario
    /// results' digests) and the item codes, never over times: the id an
    /// acknowledgement names.
    pub summary_id: String,
    pub playbook: String,
    pub candidate: VersionDigest,
    pub current: VersionDigest,
    pub computed_at_ms: u64,
    /// On this machine.
    pub fitness: Axis<FitnessAxis>,
    pub definition: Axis<DefinitionAxis>,
    pub quality: Axis<QualityAxis>,
    pub blocking: Vec<SummaryItem>,
    pub attention: Vec<SummaryItem>,
}

/// The refusal code of a promotion without `expected` (an older dashboard
/// or CLI calling the body-less route).
pub const EXPECTED_REQUIRED: &str = "expected_required";

/// Why a person's promotion was refused.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PromoteRefusal {
    /// No `expected` version and digest came with the promotion.
    ExpectedRequired,
    /// The candidate is no longer the version and digest the person saw.
    Stale {
        expected: VersionDigest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        found: Option<VersionDigest>,
    },
    /// Items that cannot be acknowledged.
    Blocked { items: Vec<SummaryItem> },
    /// Attention items the person has not acknowledged as they stand now:
    /// acknowledging means naming this `summary_id`.
    NeedsAcknowledgement {
        summary_id: String,
        items: Vec<SummaryItem>,
    },
}

impl PromoteRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            PromoteRefusal::ExpectedRequired => EXPECTED_REQUIRED,
            PromoteRefusal::Stale { .. } => "stale",
            PromoteRefusal::Blocked { .. } => "blocked",
            PromoteRefusal::NeedsAcknowledgement { .. } => "needs_acknowledgement",
        }
    }
}

/// Why a person's decision was not carried out: a refusal the person can
/// act on, or a fault.
#[derive(Debug)]
pub enum PromoteError {
    Refused(PromoteRefusal),
    Engine(EngineError),
}

impl From<EngineError> for PromoteError {
    fn from(e: EngineError) -> Self {
        PromoteError::Engine(e)
    }
}

impl std::fmt::Display for PromoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromoteError::Engine(e) => write!(f, "{e}"),
            PromoteError::Refused(r) => write!(f, "{}: {}", r.code(), r.describe()),
        }
    }
}

impl std::error::Error for PromoteError {}

impl PromoteRefusal {
    /// One line for people: what was refused and what to do.
    pub fn describe(&self) -> String {
        let codes = |items: &[SummaryItem]| {
            items
                .iter()
                .map(|i| i.code.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            PromoteRefusal::ExpectedRequired => {
                "a promotion names the candidate's version and digest as shown".to_string()
            }
            PromoteRefusal::Stale { expected, found } => match found {
                Some(f) => {
                    format!("the candidate is {f} now, not {expected} as shown: look at it again")
                }
                None => format!("the candidate is no longer {expected}: look at it again"),
            },
            PromoteRefusal::Blocked { items } => format!(
                "the candidate cannot be promoted as it is ({})",
                codes(items)
            ),
            PromoteRefusal::NeedsAcknowledgement { summary_id, items } => format!(
                "the summary has items to acknowledge ({}): promote again acknowledging {summary_id}",
                codes(items)
            ),
        }
    }
}

/// The fitness axis of `report`: every blocking compatibility or setup item
/// needs acknowledging; `None` (no report could be made) is unknown and
/// needs acknowledging too.
fn fitness_axis(id: &str, version: &str, report: Option<FitReport>) -> Axis<FitnessAxis> {
    let Some(report) = report else {
        return Axis {
            state: AxisState::Unknown,
            detail: FitnessAxis {
                report: crate::fit::unavailable(id, version),
                start_actions: Vec::new(),
            },
        };
    };
    let state = if crate::fit::problems(&report).next().is_some() {
        AxisState::Attention
    } else {
        AxisState::Ok
    };
    Axis {
        state,
        detail: FitnessAxis {
            start_actions: report.start_actions.clone(),
            report,
        },
    }
}

fn fitness_items(axis: &Axis<FitnessAxis>) -> Vec<SummaryItem> {
    let item = |detail: String| SummaryItem {
        axis: AxisName::Fitness,
        code: SummaryCode::FitnessProblem,
        detail,
    };
    if axis.state == AxisState::Unknown {
        return vec![item(
            "whether the candidate would start on this machine could not be checked".into(),
        )];
    }
    crate::fit::problems(&axis.detail.report)
        .map(|i| {
            item(match &i.subject {
                Some(s) => format!("{} ({s}): {}", i.code, i.detail),
                None => format!("{}: {}", i.code, i.detail),
            })
        })
        .collect()
}

/// The fit report of `version` of project playbook `id` on this machine;
/// `None` when none could be made.
fn assess(root: &Path, id: &str, version: &str) -> Option<FitReport> {
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: id.to_string(),
        version: Some(version.to_string()),
    };
    crate::fit::assess(root, &wref).ok()
}

/// The definition axis and its items, read from the files as they are.
struct Definition {
    candidate: VersionDigest,
    current: VersionDigest,
    axis: Axis<DefinitionAxis>,
    items: Vec<SummaryItem>,
}

fn def_item(code: SummaryCode, detail: String) -> SummaryItem {
    SummaryItem {
        axis: AxisName::Definition,
        code,
        detail,
    }
}

/// The trust digest of `version`, or the reason it has none.
fn digest_of(reg: &Registry, id: &str, version: &str) -> Result<String, String> {
    reg.load(id, Some(version))
        .map_err(|e| e.to_string())?
        .trust_digest()
        .map_err(|e| e.to_string())
}

/// The first run of `version` (runs apb created here only) whose start
/// recorded a trust digest other than `digest`: the version's content was
/// edited in place since that run.
fn edited_since(root: &Path, id: &str, version: &str, digest: &str) -> Option<(String, String)> {
    let dirs = crate::run_history::all_runs_of(&[root.to_path_buf()], id, Some(version));
    dirs.iter().find_map(|d| {
        let events = crate::run_view::read_events(d).ok()?;
        let ran = events.iter().find_map(|e| match &e.payload {
            EventPayload::RunProvenance {
                digest: Some(ran), ..
            } => Some(ran.clone()),
            _ => None,
        })?;
        let run = d.file_name()?.to_str()?.to_string();
        (ran != digest).then_some((run, ran))
    })
}

/// The index line `line` as a result reference, fresh when both its
/// digests are the ones given (the version's trust digest and the scenario
/// file's digest now).
pub fn result_ref(
    line: &IndexLine,
    playbook_digest: &str,
    scenario_digest: &str,
) -> ScenarioResultRef {
    ScenarioResultRef {
        scenario: line.scenario.clone(),
        level: line.level,
        verdict: line.verdict,
        version: line.version.clone(),
        playbook_digest: line.playbook_digest.clone(),
        scenario_digest: line.scenario_digest.clone(),
        at_ms: line.at_ms,
        fresh: line.playbook_digest == playbook_digest && line.scenario_digest == scenario_digest,
        run_id: line.run_id.clone(),
    }
}

/// The item a fresh result raises, if any.
fn fresh_item(r: &ScenarioResultRef) -> Option<SummaryItem> {
    let (code, what) = match r.verdict {
        ScenarioVerdict::Passed => return None,
        ScenarioVerdict::Failed => (SummaryCode::ScenarioFailed, "failed"),
        ScenarioVerdict::Invalid => (
            SummaryCode::ScenarioInvalid,
            "could not be played as written",
        ),
        ScenarioVerdict::Incomplete => (SummaryCode::ScenarioIncomplete, "did not end"),
        ScenarioVerdict::Stale => (SummaryCode::ScenarioStale, "did not fit the version"),
    };
    Some(def_item(
        code,
        format!(
            "scenario `{}` at {} {what} (run {})",
            r.scenario,
            r.level.as_str(),
            r.run_id
        ),
    ))
}

/// The scenario results of the candidate and the items they raise.
fn scenario_part(
    root: &Path,
    id: &str,
    version: &str,
    digest: &str,
    playbook: Option<&apb_core::schema::Playbook>,
) -> (
    Vec<IssueView>,
    Vec<ScenarioResultRef>,
    Vec<String>,
    Vec<SummaryItem>,
) {
    let playbook_dir = root.join(".apb/playbooks").join(id);
    let mut issues = Vec::new();
    let mut items = Vec::new();
    if let Some(pb) = playbook {
        for issue in apb_core::scenario::validate_all(&playbook_dir, pb) {
            if issue.severity == Severity::Error {
                items.push(def_item(
                    SummaryCode::ScenarioInvalid,
                    format!("{} {}", issue.code, issue.message),
                ));
            }
            issues.push(IssueView::from(&issue));
        }
    }
    let latest = crate::scenario::store::latest(root, id);
    let (mut results, mut not_run) = (Vec::new(), Vec::new());
    for s in apb_core::scenario::load_all(&playbook_dir).scenarios {
        if !apb_core::scenario::applies_to(&s.scenario, version) {
            continue;
        }
        let refs: Vec<ScenarioResultRef> = [ScenarioLevel::Quick, ScenarioLevel::Smoke]
            .into_iter()
            .filter_map(|level| latest.get(&(s.name.clone(), level)))
            .map(|line| result_ref(line, digest, &s.digest))
            .collect();
        if let Some(why) = apb_core::scenario::staleness(&s.scenario, version, digest) {
            items.push(def_item(
                SummaryCode::ScenarioStale,
                format!("scenario `{}`: {why}", s.name),
            ));
        }
        for r in &refs {
            if r.fresh {
                items.extend(fresh_item(r));
            } else {
                items.push(def_item(
                    SummaryCode::ScenarioStale,
                    format!(
                        "scenario `{}` at {} last ran on another version or scenario file (run {})",
                        s.name,
                        r.level.as_str(),
                        r.run_id
                    ),
                ));
            }
        }
        if !refs.iter().any(|r| r.fresh) {
            if refs.is_empty() {
                items.push(def_item(
                    SummaryCode::ScenarioNotRun,
                    format!("scenario `{}` has never run", s.name),
                ));
            }
            not_run.push(s.name.clone());
        }
        results.extend(refs);
    }
    (issues, results, not_run, items)
}

/// The definition axis of `candidate` against `current`, from the files as
/// they are now.
fn definition(
    root: &Path,
    id: &str,
    candidate: &str,
    current: &str,
) -> Result<Definition, EngineError> {
    let reg = Registry::open(root)?;
    let mut items = Vec::new();
    let mut issues = Vec::new();
    let loaded = reg.load(id, Some(candidate));
    let digest = match &loaded {
        Ok(l) => match l.trust_digest() {
            Ok(d) => d,
            Err(e) => {
                items.push(def_item(SummaryCode::ValidatorError, e.to_string()));
                String::new()
            }
        },
        Err(e) => {
            items.push(def_item(
                SummaryCode::ValidatorError,
                format!("the candidate does not load: {e}"),
            ));
            String::new()
        }
    };
    let playbook = loaded.as_ref().ok().map(|l| &l.playbook);
    if let Some(pb) = playbook {
        let ctx =
            ValidationContext::for_registry(&reg, apb_core::profile_store::PlaybookOrigin::Project)
                .with_child_outputs(pb, &reg);
        for issue in apb_core::validate::validate(pb, &ctx).issues {
            if issue.severity == Severity::Error {
                items.push(def_item(
                    SummaryCode::ValidatorError,
                    format!("{} {}", issue.code, issue.message),
                ));
            }
            issues.push(IssueView::from(&issue));
        }
    }
    if !digest.is_empty()
        && let Some((run, ran)) = edited_since(root, id, candidate, &digest)
    {
        items.push(def_item(
            SummaryCode::CandidateEdited,
            format!(
                "run {run} ran {candidate} as {ran}; its content reads as {digest} now (edited in place)"
            ),
        ));
    }
    let (scenario_issues, scenarios, not_run, scenario_items) =
        scenario_part(root, id, candidate, &digest, playbook);
    issues.extend(scenario_issues);
    items.extend(scenario_items);
    let diff = apb_core::versioning::version_diff(root, id, current, candidate).ok();
    let current_digest = digest_of(&reg, id, current).unwrap_or_default();
    Ok(Definition {
        candidate: VersionDigest {
            version: candidate.to_string(),
            digest,
        },
        current: VersionDigest {
            version: current.to_string(),
            digest: current_digest,
        },
        axis: Axis {
            state: state_of(&items),
            detail: DefinitionAxis {
                issues,
                diff,
                scenarios,
                not_run,
            },
        },
        items,
    })
}

/// The quality axis state and items of `decision` (`None` without a
/// quality gate: nothing compares, so unknown, with no item).
fn quality_items(decision: Option<&QualityDecision>) -> (AxisState, Vec<SummaryItem>) {
    match decision {
        None => (AxisState::Unknown, Vec::new()),
        Some(d) if d.verdict == QualityVerdict::Promote => (AxisState::Ok, Vec::new()),
        Some(d) => (
            AxisState::Attention,
            vec![SummaryItem {
                axis: AxisName::Quality,
                code: SummaryCode::QualityNotPromote,
                detail: format!("the comparison says {}: {}", d.verdict.as_str(), d.reason),
            }],
        ),
    }
}

/// Blocking when any item blocks, attention when there is any, else ok.
fn state_of(items: &[SummaryItem]) -> AxisState {
    if items.iter().any(|i| i.code.blocks()) {
        AxisState::Blocking
    } else if items.is_empty() {
        AxisState::Ok
    } else {
        AxisState::Attention
    }
}

/// The id an acknowledgement names: sha256 over the two versions and
/// digests, every scenario result's identity, the quality comparison the
/// summary shows (verdict, reason and every criterion's counts; the gate is
/// `current`'s, under its digest) and every item, never over a time. Any
/// change in what the person saw changes it.
fn summary_id(
    def: &Definition,
    items: &[&SummaryItem],
    quality: Option<&QualityDecision>,
) -> String {
    let results: Vec<_> = def
        .axis
        .detail
        .scenarios
        .iter()
        .map(|r| {
            (
                &r.scenario,
                r.level,
                r.verdict,
                &r.playbook_digest,
                &r.scenario_digest,
                &r.run_id,
                r.fresh,
            )
        })
        .collect();
    let basis = serde_json::json!({
        "candidate": def.candidate,
        "current": def.current,
        "scenarios": results,
        "quality": quality,
        "items": items,
    });
    apb_core::content::sha256_hex(basis.to_string().as_bytes())
}

/// The blocking and the attention items of the three axes, in axis order.
fn split(
    fitness: Vec<SummaryItem>,
    definition: &[SummaryItem],
    quality: Vec<SummaryItem>,
) -> (Vec<SummaryItem>, Vec<SummaryItem>) {
    fitness
        .into_iter()
        .chain(definition.iter().cloned())
        .chain(quality)
        .partition(|i| i.code.blocks())
}

/// The summary of the candidate of project playbook `id` under `root`;
/// `None` without a candidate on trial. Read-only.
pub fn candidate_summary(root: &Path, id: &str) -> Result<Option<CandidateSummary>, EngineError> {
    let Some(view) = super::quality_view(root, id)? else {
        return Ok(None);
    };
    let fitness = fitness_axis(id, &view.version, assess(root, id, &view.version));
    let def = definition(root, id, &view.version, &view.current)?;
    let (q_state, q_items) = quality_items(view.decision.as_ref());
    let (blocking, attention) = split(fitness_items(&fitness), &def.items, q_items);
    let all: Vec<&SummaryItem> = blocking.iter().chain(&attention).collect();
    Ok(Some(CandidateSummary {
        summary_id: summary_id(&def, &all, view.decision.as_ref()),
        playbook: id.to_string(),
        candidate: def.candidate,
        current: def.current,
        computed_at_ms: apb_core::clock::now_ms_u64(),
        fitness,
        definition: def.axis,
        quality: Axis {
            state: q_state,
            detail: QualityAxis { view },
        },
        blocking,
        attention,
    }))
}

/// A person decides the candidate of playbook `id`: promote it, or keep
/// `current` and drop it. `by` names the surface (`cli`, `dashboard`).
/// Refused without a candidate. A promotion is carried out only when, under
/// the candidate lock, the candidate is still `expected`, nothing blocks,
/// and an acknowledgement names the summary id as it stands then (when it
/// has attention items); the record keeps that id.
pub fn decide_by_person(
    root: &Path,
    id: &str,
    decision: PersonDecision,
    by: &str,
) -> Result<QualityRecord, PromoteError> {
    let PersonDecision::Promote {
        expected,
        acknowledged,
    } = &decision
    else {
        return super::quality::decide_checked(root, id, decision, by, |_, _, _| Ok(None));
    };
    // Fitness may probe agent CLIs: taken before the lock and kept.
    let fitness_items = fitness_items(&fitness_axis(
        id,
        &expected.version,
        assess(root, id, &expected.version),
    ));
    let check = |candidate: &str, current: &str, quality: Option<&QualityDecision>| {
        let def = definition(root, id, candidate, current)?;
        if def.candidate != *expected {
            return Err(PromoteError::Refused(PromoteRefusal::Stale {
                expected: expected.clone(),
                found: Some(def.candidate),
            }));
        }
        let (_, q_items) = quality_items(quality);
        let (blocking, attention) = split(fitness_items.clone(), &def.items, q_items);
        let all: Vec<&SummaryItem> = blocking.iter().chain(&attention).collect();
        let sid = summary_id(&def, &all, quality);
        if !blocking.is_empty() {
            return Err(PromoteError::Refused(PromoteRefusal::Blocked {
                items: blocking,
            }));
        }
        if !attention.is_empty() && acknowledged.as_deref() != Some(sid.as_str()) {
            return Err(PromoteError::Refused(
                PromoteRefusal::NeedsAcknowledgement {
                    summary_id: sid,
                    items: attention,
                },
            ));
        }
        Ok(Some(sid))
    };
    super::quality::decide_checked(root, id, decision.clone(), by, check)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--expect` takes exactly what `apb candidate` prints: a version, an
    /// `@`, an algorithm-named digest. Anything less is refused rather than
    /// promoting against a guess.
    #[test]
    fn an_expected_version_reads_only_as_version_at_digest() {
        let v: VersionDigest = "1.3.0@sha256:ab12".parse().unwrap();
        assert_eq!(
            v,
            VersionDigest {
                version: "1.3.0".into(),
                digest: "sha256:ab12".into()
            }
        );
        assert_eq!(v.to_string(), "1.3.0@sha256:ab12");
        for bad in [
            "1.3.0",
            "@sha256:ab12",
            "1.3.0@",
            "1.3.0@ab12",
            "1.3.0@sha256:",
            "1.3.0@:ab12",
            "1.3.0 @sha256:ab12",
        ] {
            assert!(bad.parse::<VersionDigest>().is_err(), "accepted `{bad}`");
        }
    }
}
