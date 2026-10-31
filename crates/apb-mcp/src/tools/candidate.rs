//! A playbook's candidate version on the surfaces: how it compares with
//! `current` by quality (MCP `playbook_candidate`, `apb candidate`,
//! `GET /api/playbooks/{id}/candidate`) and a person's decision on it
//! (`apb candidate --promote | --keep-current`, the dashboard's Promote and
//! Keep current). The comparison and the decision are
//! [`apb_engine::candidate`]'s.

use std::path::Path;

use apb_core::quality::QualityRecord;
use apb_core::step_env::StepCaller;
use apb_engine::candidate::{CandidateQualityView, PersonDecision};

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
/// and (a conflict) from a run step (`step`): a run never decides its own
/// playbook's candidate.
pub fn candidate_decide(
    root: &Path,
    id: &str,
    decision: PersonDecision,
    by: &str,
    step: &StepCaller,
) -> Result<QualityRecord, ToolError> {
    outside_run(step, "deciding a candidate")?;
    checked(id)?;
    let decision = match decision {
        PersonDecision::KeepCurrent { reason } => PersonDecision::KeepCurrent {
            reason: clean_reason(reason),
        },
        promote => promote,
    };
    Ok(apb_engine::candidate::decide_by_person(
        root, id, decision, by,
    )?)
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
