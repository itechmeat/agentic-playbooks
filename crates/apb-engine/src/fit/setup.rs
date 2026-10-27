//! What this machine and project must provide before a playbook can run
//! here: its profiles and skills, the models of their executor chains, the
//! connector accounts and secrets, and the run input a start must pass.
//! The profile and model checks are the ones the adoption report prints
//! ([`profile_findings`]), with the same codes.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::model_check::{self, ModelContext, ModelIssue};
use apb_core::profile::QualifiedProfileRef;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::run_memory::{Recall, ValueStatus};
use apb_core::trust::TrustStore;
use serde::Serialize;

use super::report::{FitItem, FitSeverity};

/// One finding about a profile a playbook binds, in the adoption report's
/// shape: `code`, the profile (`ref`: its `<scope>/<name>` key once it
/// resolves, else the name asked for), and for a model finding the agent,
/// the model and a detail.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ProfileFinding {
    pub code: &'static str,
    #[serde(rename = "ref")]
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Whether it keeps a run from working (as opposed to a note).
    #[serde(skip)]
    pub blocking: bool,
}

/// The code of a profile bundle nobody approved.
pub const UNTRUSTED: &str = "untrusted";

impl ProfileFinding {
    /// A trust finding (an unapproved bundle) rather than a setup one.
    pub fn is_trust(&self) -> bool {
        self.code == UNTRUSTED
    }

    fn to_item(&self) -> FitItem {
        let mut detail = self.detail.clone().unwrap_or_default();
        if let (Some(agent), Some(model)) = (&self.agent, &self.model) {
            detail = format!("{agent} {model} {detail}").trim_end().to_string();
        } else if let Some(agent) = &self.agent {
            detail = format!("{agent} {detail}").trim_end().to_string();
        }
        FitItem {
            code: self.code.to_string(),
            subject: Some(self.profile.clone()),
            detail,
            severity: if self.blocking {
                FitSeverity::Blocking
            } else {
                FitSeverity::Note
            },
        }
    }
}

/// Checks one profile reference of a playbook of `origin`: that it
/// resolves with its skills, that its bundle is approved, and every
/// `(agent, model)` of its executor chain.
pub fn profile_findings(
    root: &Path,
    origin: PlaybookOrigin,
    r: &QualifiedProfileRef,
    cx: &ModelContext,
    store: &TrustStore,
) -> Vec<ProfileFinding> {
    check_profile(root, origin, r, cx, store).0
}

/// [`profile_findings`], plus the bundle `(<scope>/<name>, digest)` of a
/// profile that resolves: computed once for both the setup and the trust
/// section.
fn check_profile(
    root: &Path,
    origin: PlaybookOrigin,
    r: &QualifiedProfileRef,
    cx: &ModelContext,
    store: &TrustStore,
) -> (Vec<ProfileFinding>, Option<(String, String)>) {
    let mut out = Vec::new();
    match profile_store::compute_bundle(root, origin, r) {
        Ok((loaded, _pairs, bundle)) => {
            let key = format!("{}/{}", profile_store::scope_str(loaded.scope), loaded.name);
            if !store.is_approved(&bundle) {
                out.push(finding(UNTRUSTED, &key, false));
            }
            let ex = &loaded.doc.executor;
            let chain = std::iter::once((ex.agent.as_str(), ex.model.as_str())).chain(
                ex.fallbacks
                    .iter()
                    .map(|f| (f.agent.as_str(), f.model.as_str())),
            );
            for (agent, model) in chain {
                out.extend(model_finding(agent, model, &key, cx));
            }
            (out, Some((key, bundle)))
        }
        Err(e) => {
            let code = match &e {
                profile_store::ProfileError::SkillMissing(_) => "skill_missing",
                profile_store::ProfileError::NotFound(_) => "profile_missing",
                _ => "profile_unresolved",
            };
            out.push(ProfileFinding {
                detail: Some(e.to_string()),
                ..finding(code, &r.name, true)
            });
            (out, None)
        }
    }
}

fn finding(code: &'static str, profile: &str, blocking: bool) -> ProfileFinding {
    ProfileFinding {
        code,
        profile: profile.to_string(),
        agent: None,
        model: None,
        detail: None,
        blocking,
    }
}

/// One `(agent, model)` of a profile's executor chain, judged by the shared
/// [`model_check`]: availability is asserted only when detection authority
/// is Full; zcode's allowlist and the config's `model_policy` block; an id
/// outside apb's closed list for the agent is `model_unknown`.
fn model_finding(
    agent: &str,
    model: &str,
    profile_key: &str,
    cx: &ModelContext,
) -> Option<ProfileFinding> {
    let issue = model_check::check(agent, model, cx)?;
    let blocking = !matches!(issue, ModelIssue::Unverifiable | ModelIssue::NewInFamily);
    let mut f = ProfileFinding {
        agent: Some(agent.to_string()),
        ..finding(issue.code(), profile_key, blocking)
    };
    match &issue {
        ModelIssue::AgentNotInstalled => {}
        ModelIssue::NotAllowed(detail) | ModelIssue::PolicyViolation(detail) => {
            f.model = Some(model.to_string());
            f.detail = Some(detail.clone());
        }
        _ => f.model = Some(model.to_string()),
    }
    Some(f)
}

/// What the profiles of a playbook add to a fit: the setup items (trust
/// findings left out, one item per distinct finding) and the bundle of
/// each profile that resolves, for the trust section.
pub(super) struct Profiles {
    pub setup: Vec<FitItem>,
    pub bundles: BTreeMap<String, String>,
}

/// Checks every profile in `refs` once, against `store`.
pub(super) fn profiles(
    root: &Path,
    origin: PlaybookOrigin,
    refs: &[QualifiedProfileRef],
    store: &TrustStore,
) -> Profiles {
    let mut out = Profiles {
        setup: Vec::new(),
        bundles: BTreeMap::new(),
    };
    // The model context probes the installed agents: only when a profile
    // needs it.
    if refs.is_empty() {
        return out;
    }
    let cx = ModelContext::load();
    let mut seen = std::collections::HashSet::new();
    for r in refs {
        let (findings, bundle) = check_profile(root, origin, r, &cx, store);
        out.bundles.extend(bundle);
        out.setup.extend(
            findings
                .into_iter()
                .filter(|f| !f.is_trust())
                .filter(|f| seen.insert(f.clone()))
                .map(|f| f.to_item()),
        );
    }
    out
}

/// The setup items of remembered input: a remembered value that no longer
/// fits (a note: it is not used), memory that was ignored, and each param
/// a start must pass.
pub(super) fn input_items(recall: &Recall) -> Vec<FitItem> {
    let mut out: Vec<FitItem> = recall
        .values
        .iter()
        .filter_map(|v| match v.status {
            ValueStatus::Incompatible { reason } => Some(FitItem {
                code: "remembered_incompatible".to_string(),
                subject: Some(v.name.clone()),
                detail: format!(
                    "the remembered value no longer fits the param ({}); it is not used",
                    reason.as_str()
                ),
                severity: FitSeverity::Note,
            }),
            ValueStatus::Compatible => None,
        })
        .collect();
    out.extend(recall.diagnostics.iter().map(|d| FitItem {
        code: "remembered_ignored".to_string(),
        subject: None,
        detail: d.clone(),
        severity: FitSeverity::Note,
    }));
    out.extend(recall.missing.iter().map(|name| FitItem {
        code: "param_needs_value".to_string(),
        subject: Some(name.clone()),
        detail: "no default and nothing remembered: a start must pass a value".to_string(),
        severity: FitSeverity::Note,
    }));
    out
}
