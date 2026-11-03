//! A playbook's candidate version on the surfaces: how it compares with
//! `current` by quality (MCP `playbook_candidate`, `apb candidate`,
//! `GET /api/playbooks/{id}/candidate`) and a person's decision on it
//! (`apb candidate --promote | --keep-current`, the dashboard's Promote and
//! Keep current), with the candidate summary a person reads first
//! (`GET /api/playbooks/{id}/candidate/summary`, the `summary` of
//! `playbook_candidate`, `apb candidate`). The comparison, the summary and
//! the decision are [`apb_engine::candidate`]'s; a promotion is re-checked
//! there under the candidate lock and refused with a [`PromoteRefusal`].

use std::path::Path;

use apb_core::quality::QualityRecord;
use apb_core::step_env::StepCaller;
use apb_engine::candidate::summary::{CandidateSummary, PromoteRefusal};
use apb_engine::candidate::{CandidateQualityView, PersonDecision, PromoteError};

use super::{ToolError, outside_run};

fn checked(id: &str) -> Result<(), ToolError> {
    if apb_core::registry::is_safe_segment(id) {
        Ok(())
    } else {
        Err(ToolError::NotFound(format!("playbook `{id}`")))
    }
}

/// The candidate of project playbook `id` and how it compares with
/// `current` now; `None` without a candidate on trial. Read-only.
pub fn candidate_view(root: &Path, id: &str) -> Result<Option<CandidateQualityView>, ToolError> {
    checked(id)?;
    Ok(apb_engine::candidate::quality_view(root, id)?)
}

/// The candidate summary of project playbook `id` (fitness on this
/// machine, the definition, quality; what blocks and what needs
/// acknowledging); `None` without a candidate on trial. Read-only.
pub fn candidate_summary(root: &Path, id: &str) -> Result<Option<CandidateSummary>, ToolError> {
    checked(id)?;
    Ok(apb_engine::candidate::candidate_summary(root, id)?)
}

/// Why a person's decision was not carried out, kept typed for a surface
/// that answers a refusal as data (the summary id to acknowledge, the
/// items that block).
#[derive(Debug)]
pub enum DecideError {
    Refused(PromoteRefusal),
    Tool(ToolError),
}

impl From<ToolError> for DecideError {
    fn from(e: ToolError) -> Self {
        DecideError::Tool(e)
    }
}

/// A refusal as the text surfaces carry it, code first: `stale` and
/// `needs_acknowledgement` are conflicts (look again, then decide);
/// `blocked` and `expected_required` are refused as asked.
impl From<DecideError> for ToolError {
    fn from(e: DecideError) -> Self {
        match e {
            DecideError::Tool(t) => t,
            DecideError::Refused(r) => {
                let text = format!("{}: {}", r.code(), r.describe());
                match r {
                    PromoteRefusal::Stale { .. } | PromoteRefusal::NeedsAcknowledgement { .. } => {
                        ToolError::Conflict(text)
                    }
                    PromoteRefusal::Blocked { .. } | PromoteRefusal::ExpectedRequired => {
                        ToolError::Engine(text)
                    }
                }
            }
        }
    }
}

/// The longest reason a person's decision keeps.
pub const REASON_MAX: usize = 300;

/// A person's reason as the trial record keeps it: one line without hidden
/// characters, at most [`REASON_MAX`] characters; blank reads as none.
fn clean_reason(reason: Option<String>) -> Option<String> {
    let line = apb_core::text::replace_hidden(&reason?, ' ');
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then(|| line.chars().take(REASON_MAX).collect())
}

/// A person decides the candidate of project playbook `id`; `by` names the
/// surface (`cli`, `dashboard`). Refused (not found) without a candidate,
/// (a conflict) from a run step (`step`): a run never decides its own
/// playbook's candidate, and a promotion as the engine refuses it (a
/// candidate other than the one shown, a blocking item, attention items not
/// acknowledged under the summary id they stand under now).
pub fn candidate_decide(
    root: &Path,
    id: &str,
    decision: PersonDecision,
    by: &str,
    step: &StepCaller,
) -> Result<QualityRecord, ToolError> {
    Ok(candidate_decide_typed(root, id, decision, by, step)?)
}

/// [`candidate_decide`] with a promotion's refusal kept as data.
pub fn candidate_decide_typed(
    root: &Path,
    id: &str,
    decision: PersonDecision,
    by: &str,
    step: &StepCaller,
) -> Result<QualityRecord, DecideError> {
    outside_run(step, "deciding a candidate")?;
    checked(id)?;
    let decision = match decision {
        PersonDecision::KeepCurrent { reason } => PersonDecision::KeepCurrent {
            reason: clean_reason(reason),
        },
        promote => promote,
    };
    apb_engine::candidate::decide_by_person(root, id, decision, by).map_err(|e| match e {
        PromoteError::Refused(r) => DecideError::Refused(r),
        PromoteError::Engine(e) => DecideError::Tool(e.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_is_one_short_line_or_none() {
        assert_eq!(clean_reason(None), None);
        assert_eq!(clean_reason(Some(" \n ".into())), None);
        assert_eq!(
            clean_reason(Some("too\nslow\u{202e} now".into())).as_deref(),
            Some("too slow now")
        );
        let long = clean_reason(Some("x".repeat(REASON_MAX + 50))).unwrap();
        assert_eq!(long.chars().count(), REASON_MAX);
    }
}
