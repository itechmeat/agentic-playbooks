//! What this machine and project must provide before a playbook can run
//! here: its profiles and skills, the models of their executor chains, the
//! connector accounts and secrets, the run input a start must pass, the
//! sandbox its isolated nodes need, and the remote its publish nodes push to.
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

/// One item per isolated node: what its isolation gets on this machine. A
/// `full` node this machine cannot isolate blocks (the gate refuses it); a
/// `best_effort` node without a sandbox is a note naming why.
pub(super) fn isolation_items(
    root: &Path,
    playbook: &apb_core::schema::Playbook,
    origin: PlaybookOrigin,
) -> Vec<FitItem> {
    use apb_core::isolation::contract::Backend;
    crate::gate::isolation::node_contracts(root, playbook, origin, false)
        .into_iter()
        .map(|n| {
            let c = &n.contract;
            let reasons = |r: &[apb_core::isolation::contract::Reason]| {
                r.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let (detail, severity) = if !c.allowed() {
                (
                    format!(
                        "isolation full cannot be enforced here: {}",
                        reasons(&c.refusals)
                    ),
                    FitSeverity::Blocking,
                )
            } else if c.backend == Backend::None {
                (
                    format!(
                        "isolation {} runs without a sandbox here: {}",
                        n.level.as_str(),
                        reasons(&c.fallbacks)
                    ),
                    FitSeverity::Note,
                )
            } else {
                let limits: Vec<&str> = c.limits.iter().map(|l| l.id()).collect();
                (
                    format!(
                        "isolation {} is enforced by bwrap; limits: {}",
                        n.level.as_str(),
                        limits.join(", ")
                    ),
                    FitSeverity::Note,
                )
            };
            FitItem {
                code: "isolation".to_string(),
                subject: Some(n.node),
                detail,
                severity,
            }
        })
        .collect()
}

/// One item per publish node: the remote it pins on this project, and
/// whether its push URL is one the publish step pushes to. A missing remote
/// or a refused URL blocks: the step would refuse at its plan.
pub(super) fn publish_items(root: &Path, playbook: &apb_core::schema::Playbook) -> Vec<FitItem> {
    use apb_core::publish::url::PushUrl;
    use apb_core::schema::NodeKind;
    let repo = apb_core::git::Repo::locate(root).ok();
    playbook
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Publish { remote, .. } => Some((n.id.clone(), remote.as_str())),
            _ => None,
        })
        .map(|(node, remote)| {
            let url = repo
                .as_ref()
                .and_then(|r| crate::publish::pin::push_url(r, root, remote));
            let (detail, severity) = match (&repo, url) {
                (None, _) => (
                    "the project is not a git working tree apb runs git in".to_string(),
                    FitSeverity::Blocking,
                ),
                (Some(_), None) => (
                    format!("the repository has no remote `{remote}`"),
                    FitSeverity::Blocking,
                ),
                (Some(_), Some(text)) => match PushUrl::parse(&text) {
                    Ok(url) => (
                        format!(
                            "pushes to remote `{remote}` at {} over {}",
                            url.as_str(),
                            url.scheme().as_str()
                        ),
                        FitSeverity::Note,
                    ),
                    // The refused URL is not shown: it may carry a credential.
                    Err(why) => (
                        format!("the push URL of remote `{remote}` is refused: {why}"),
                        FitSeverity::Blocking,
                    ),
                },
            };
            FitItem {
                code: "publish_target".to_string(),
                subject: Some(node),
                detail,
                severity,
            }
        })
        .collect()
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
