//! The model signal of the end-of-turn save check
//! (`apb_core::save_nudge`): one bounded yes or no question, asked through
//! the standalone decider of the session's project, whether the request a
//! host session just finished is recurring or repeatable by nature. The
//! model reads the request in whatever language it was written; there is
//! no word list anywhere. Every failure is no signal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::decisions::{DecisionMode, EffectiveDecisions};
use apb_decide::{Answer, Question, UseSite};
use serde_json::json;

use super::standalone::{StandaloneDecider, StandaloneOutcome};

/// The question's id.
const QUESTION: &str = "recurring";
/// The `p` at or above which the request counts as recurring
/// (`uses.save_nudge.thresholds.recurring` overrides it).
pub const DEFAULT_CUT: f64 = 0.7;
/// The longest the question may wait for its providers together, whatever
/// `timeout_ms` says: the host waits for the hook.
const MAX_TIMEOUT_MS: u64 = 4000;
/// The most characters of the request sent (fewer when the state's byte
/// budget, `privacy.max_state_bytes`, is smaller).
const MAX_REQUEST_CHARS: usize = 2000;

/// Whether the decision model judges `request` recurring or repeatable;
/// `None` when no model is configured for the use or the question failed.
/// `cwd` is the session's folder: the settings are the machine's as every
/// project it lies in narrows them (a project that opted out is never
/// asked about, from any of its subfolders), and the request is redacted
/// for the nearest of them. The questions are logged and capped per day in
/// `log_root/.apb/decisions.jsonl`, the hook's own folder, so a folder that
/// is not an apb project gets no `.apb` folder from the check.
pub fn recurring(cwd: &Path, log_root: &Path, request: &str) -> Option<bool> {
    let roots = apb_core::workspace::enclosing_roots(cwd);
    let resolution = if roots.is_empty() {
        apb_core::decisions::resolve(cwd)
    } else {
        let refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        apb_core::decisions::resolve_for_all(&refs)
    };
    let settings = bounded(resolution.active()?);
    let project = roots.first().map_or(cwd, PathBuf::as_path);
    ask(
        &StandaloneDecider::with_settings_logged_in(settings, project, log_root),
        request,
    )
}

/// `settings` with the provider chain's whole wait bounded by
/// [`MAX_TIMEOUT_MS`]: each provider gets its share, since the chain tries
/// them one after another.
fn bounded(mut settings: EffectiveDecisions) -> EffectiveDecisions {
    let providers = settings.providers.len().max(1) as u64;
    settings.timeout_ms = settings.timeout_ms.min(MAX_TIMEOUT_MS / providers);
    settings
}

/// [`recurring`] over a given decider.
pub fn ask(decider: &StandaloneDecider, request: &str) -> Option<bool> {
    if decider.mode_for(UseSite::SaveNudge) == DecisionMode::Off || !decider.available() {
        return None;
    }
    // Redacted first, then clipped, so a cut never splits a secret: to
    // the characters sent, then to the state's byte budget (a character
    // may take several bytes, and escaping adds more).
    let mut text: String = decider
        .redact(request)
        .chars()
        .take(MAX_REQUEST_CHARS)
        .collect();
    let budget = decider.max_state_bytes();
    loop {
        let size = serde_json::to_string(&json!({ "request": text })).map_or(0, |s| s.len());
        if size <= budget || text.is_empty() {
            break;
        }
        let mut cut = text.len().saturating_sub(size - budget);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    let questions = BTreeMap::from([(
        QUESTION.to_string(),
        Question::Noul {
            instructions: json!(
                "Is `request` a process a person is likely to repeat, or recurring by nature (a routine, a scheduled or regular job), rather than a one-off question or change? It may be written in any language."
            ),
            criteria: None,
        },
    )]);
    let state = json!({ "request": text });
    match decider.decide(UseSite::SaveNudge, state, vec!["request".into()], questions) {
        // Shadow only logs the answer; a reminder needs `advise`.
        StandaloneOutcome::Answered(a) if a.mode < DecisionMode::Advise => None,
        StandaloneOutcome::Answered(a) => match a.answers.get(QUESTION) {
            Some(Answer::Noul { p }) => {
                let cut = decider
                    .threshold(UseSite::SaveNudge, QUESTION)
                    .unwrap_or(DEFAULT_CUT);
                Some(*p >= cut)
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chain tries its providers in turn: together they wait at most
    /// [`MAX_TIMEOUT_MS`].
    #[test]
    fn the_providers_share_the_wait() {
        let one =
            "providers: [{ id: a, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }]\n";
        let two = "providers: [{ id: a, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }, { id: b, kind: systemone, base_url: 'http://127.0.0.1:2', model: m }]\ntimeout_ms: 9000\n";
        for (body, want) in [(one, 3000), (two, MAX_TIMEOUT_MS / 2)] {
            let cfg = tempfile::tempdir().unwrap();
            std::fs::write(cfg.path().join(apb_core::decisions::DECISIONS_FILE), body).unwrap();
            let settings = apb_core::decisions::load_file(cfg.path()).unwrap().unwrap();
            assert_eq!(bounded(settings).timeout_ms, want, "{body}");
        }
    }
}
