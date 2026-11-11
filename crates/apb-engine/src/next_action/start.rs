//! Start actions: what a start that was refused before any run existed
//! needs, built from the run gate's refusals and from a fit report.
//!
//! A refused start has no run id and no observation, so it is its own type,
//! [`StartAction`], with its own closed codes:
//! - `confirm_irreversible_start`: a person agrees to the tree's
//!   irreversible effects by echoing the refusal's consent nonce;
//! - `acknowledge_untrusted`: the start acknowledges content nobody
//!   approved (an approval by digest is the lasting fix);
//! - `fix_requirement`: something must change first, said by a
//!   [`Remedy`];
//! - `confirm_launch`: a person confirms an agent's launch of an `ask`
//!   playbook by echoing the refusal's launch nonce (with its fixed version
//!   and choice);
//! - `launch_by_person`: only a person launches it (the dashboard, a
//!   terminal).
//!
//! A fit report's items become actions through one explicit table of
//! actionable codes ([`FIT_CODES`]), independent of the item's severity: a
//! param a start must pass is a note in the report and still an action.

use std::path::Path;

use apb_core::scope::PlaybookRef;
use serde_json::Value;

pub use super::start_action::{Remedy, StartAction, StartActionCode};
use super::types::{ActionArg, ArgValue, Principal};
use crate::fit::{FitItem, FitReport};
use crate::gate::RunPermit;

/// The playbook a start is about.
#[derive(Debug, Clone, Copy)]
pub struct StartOf<'a> {
    pub playbook: &'a str,
    pub version: Option<&'a str>,
}

impl<'a> StartOf<'a> {
    /// The playbook `wref` names.
    pub fn of(wref: &'a PlaybookRef) -> Self {
        StartOf {
            playbook: &wref.id,
            version: wref.version.as_deref(),
        }
    }
}

impl StartOf<'_> {
    fn action(
        &self,
        code: StartActionCode,
        principals: Vec<Principal>,
        remedy: Option<Remedy>,
        reason: String,
    ) -> StartAction {
        StartAction {
            code,
            playbook: self.playbook.to_string(),
            version: self.version.map(str::to_string),
            args: Vec::new(),
            principals,
            remedy,
            reason,
        }
    }

    fn acknowledge(&self, reason: String) -> StartAction {
        let mut a = self.action(
            StartActionCode::AcknowledgeUntrusted,
            vec![Principal::Person, Principal::Operator, Principal::Host],
            None,
            reason,
        );
        a.args = vec![given("acknowledge_untrusted", "true")];
        a
    }

    fn fix(&self, remedy: Remedy, reason: String) -> StartAction {
        let principals = match &remedy {
            Remedy::ApproveDigest { .. } => vec![Principal::Person],
            Remedy::ProvideParam { .. } => {
                vec![Principal::Person, Principal::Operator, Principal::Host]
            }
            _ => vec![Principal::Person, Principal::Operator],
        };
        let mut a = self.action(StartActionCode::FixRequirement, principals, None, reason);
        if let Remedy::ProvideParam { name } = &remedy {
            a.args = vec![ActionArg {
                name: name.clone(),
                value: ArgValue::Supplied {
                    hint: format!("a value for param `{name}`"),
                },
                required: true,
            }];
        }
        a.remedy = Some(remedy);
        a
    }
}

fn given(name: &str, value: &str) -> ActionArg {
    ActionArg {
        name: name.into(),
        value: ArgValue::Given {
            value: value.into(),
        },
        required: true,
    }
}

fn strings(v: &Value, field: &str) -> Vec<String> {
    v[field]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The start actions of one structured refusal of the run gate (or of the
/// irreversible-consent check). A trust refusal of a tree with irreversible
/// effects carries the consent the retry needs too (`irreversible` and
/// `consent_nonce`), so it gives both actions and one retry with both
/// answers goes through. A refusal no action can answer (a missing
/// playbook, a draft, a cycle) gives none.
pub fn from_refusal(of: StartOf<'_>, refusal: &Value) -> Vec<StartAction> {
    let mut out = policy_actions(of, refusal);
    let consent = match refusal["policy"].as_str() {
        Some(crate::consent::REFUSAL_POLICY) => "sources",
        _ => "irreversible",
    };
    if let Some(nonce) = refusal["consent_nonce"].as_str() {
        out.push(confirm(of, &strings(refusal, consent), nonce));
    }
    out.extend(launch_action(of, refusal));
    out
}

/// The launch action a refusal carries, on its own or beside a trust or
/// consent refusal: `confirm_launch` with the nonce and the fixed version
/// and choice, or `launch_by_person`.
fn launch_action(of: StartOf<'_>, refusal: &Value) -> Option<StartAction> {
    let policy = refusal["launch_policy"].as_str()?;
    let sources = strings(refusal, "launch_sources").join(", ");
    let version = refusal["launch_version"].as_str().map(str::to_string);
    let billed = refusal["policy"]
        .as_str()
        .is_some_and(crate::gate::billing::is_billing_refusal);
    let held = strings(&refusal["billing"], "held").join(", ");
    if let Some(nonce) = refusal["launch_nonce"].as_str() {
        let why = if billed {
            format!(
                "playbook `{}` would start agent work that is billed or of unknown basis ({held}); a person confirms this launch",
                of.playbook
            )
        } else {
            format!(
                "playbook `{}` is `{policy}` for agents ({sources}); a person confirms this launch",
                of.playbook
            )
        };
        let mut a = of.action(
            StartActionCode::ConfirmLaunch,
            vec![Principal::Person, Principal::Operator, Principal::Host],
            None,
            why,
        );
        a.version = version.or(a.version);
        a.args = vec![given("confirm_launch", nonce)];
        if let Some(v) = refusal["launch_version"].as_str() {
            a.args.push(given("launch_version", v));
        }
        if let Some(c) = refusal["launch_choice"].as_str() {
            a.args.push(given("launch_choice", c));
        }
        return Some(a);
    }
    refusal["launch_by_person"].as_bool()?.then(|| {
        let why = if billed {
            format!(
                "playbook `{}` would start agent work that is billed or of unknown basis ({held}); a person launches it from the dashboard or a terminal",
                of.playbook
            )
        } else {
            format!(
                "playbook `{}` is `{policy}` for agents ({sources}); a person launches it from the dashboard or a terminal",
                of.playbook
            )
        };
        let mut a = of.action(StartActionCode::LaunchByPerson, vec![Principal::Person], None, why);
        a.version = version.or(a.version);
        a
    })
}

/// `confirm_irreversible_start`: a person agrees to `sources` by echoing
/// `nonce`.
fn confirm(of: StartOf<'_>, sources: &[String], nonce: &str) -> StartAction {
    let mut a = of.action(
        StartActionCode::ConfirmIrreversibleStart,
        vec![Principal::Person],
        None,
        format!(
            "the run has irreversible effects ({}); a person confirms them",
            sources.join(", ")
        ),
    );
    a.args = vec![given("confirm_irreversible", nonce)];
    a
}

/// The actions of the refusal's own policy.
fn policy_actions(of: StartOf<'_>, refusal: &Value) -> Vec<StartAction> {
    let policy = refusal["policy"].as_str().unwrap_or_default();
    let id = refusal["id"].as_str().unwrap_or(of.playbook);
    match policy {
        "untrusted_requires_acknowledge" => {
            let mut out = vec![of.acknowledge(format!(
                "nobody approved playbook `{id}` at this digest; acknowledge it for this start"
            ))];
            if let Some(digest) = refusal["digest"].as_str() {
                out.push(of.fix(
                    Remedy::ApproveDigest {
                        kind: "playbook".into(),
                        digest: digest.into(),
                    },
                    format!("approve playbook `{id}` at this digest so later starts need no acknowledge"),
                ));
            }
            out
        }
        "untrusted_profile_requires_acknowledge" => vec![of.acknowledge(format!(
            "nobody approved the profile bundles {}; acknowledge them for this start",
            strings(refusal, "profiles").join(", ")
        ))],
        "imported_scripts_require_approval" => refusal["digest"]
            .as_str()
            .map(|digest| {
                of.fix(
                    Remedy::ApproveDigest {
                        kind: "imported_scripts".into(),
                        digest: digest.into(),
                    },
                    format!(
                        "playbook `{id}` was imported with scripts; review them and approve the version by its digest"
                    ),
                )
            })
            .into_iter()
            .collect(),
        "untrusted_connector_requires_approve" => strings(refusal, "connectors")
            .into_iter()
            .map(|c| {
                of.fix(
                    Remedy::Manual {
                        detail: format!("approve connector `{c}`"),
                    },
                    format!("nobody approved connector `{c}`; an acknowledge does not cover it"),
                )
            })
            .collect(),
        "unapproved_connector_account" => strings(refusal, "accounts")
            .into_iter()
            .map(|a| {
                of.fix(
                    Remedy::Manual {
                        detail: format!("approve connector account `{a}`"),
                    },
                    format!("nobody approved connector account `{a}`; an acknowledge does not cover it"),
                )
            })
            .collect(),
        "connector_env_missing" => strings(refusal, "missing")
            .into_iter()
            .map(|name| {
                let reason = format!("secret env var `{name}` is not set");
                of.fix(Remedy::SetEnv { name }, reason)
            })
            .collect(),
        crate::gate::PARAM_NEEDS_VALUE => strings(refusal, "missing")
            .into_iter()
            .map(|name| {
                let reason =
                    format!("param `{name}` has no default and nothing remembered: pass a value");
                of.fix(Remedy::ProvideParam { name }, reason)
            })
            .collect(),
        crate::gate::PARAM_PATTERN_MISMATCH => strings(refusal, "invalid")
            .into_iter()
            .map(|name| {
                let pattern = refusal["patterns"][&name].as_str().unwrap_or_default();
                let reason = format!(
                    "param `{name}` does not match its pattern `{pattern}`: pass a value that does"
                );
                of.fix(Remedy::ProvideParam { name }, reason)
            })
            .collect(),
        "requires_unmet" => strings(refusal, "missing")
            .into_iter()
            .map(|m| {
                of.fix(
                    Remedy::Manual {
                        detail: format!("provide {m}"),
                    },
                    format!("playbook `{id}` requires {m}, which this project or machine lacks"),
                )
            })
            .collect(),
        crate::run_config::UNATTENDED_UNSUPPORTED => {
            let detail = refusal["detail"].as_str().unwrap_or_default();
            vec![of.fix(
                Remedy::Manual {
                    detail: match refusal["requirement"].as_str() {
                        Some("cli_execution") => "start the unattended run with execution: cli",
                        Some("unattended_caller") => {
                            "start it attended, or start the outer run unattended"
                        }
                        _ => "start the unattended run without supervision",
                    }
                    .into(),
                },
                format!("playbook `{id}` cannot start unattended this way: {detail}"),
            )]
        }
        "playbook_on_hold" => vec![of.fix(
            Remedy::Manual {
                detail: format!("apb health clear {id}"),
            },
            refusal["detail"]
                .as_str()
                .unwrap_or("the playbook is on hold")
                .to_string(),
        )],
        "requires_unsafe_path" | "requires_unsafe_command" => {
            let what = refusal["path"]
                .as_str()
                .or(refusal["command"].as_str())
                .unwrap_or_default();
            vec![of.fix(
                Remedy::Manual {
                    detail: format!("fix the `requires` entry `{what}` of playbook `{id}`"),
                },
                format!("playbook `{id}` names `{what}` in `requires` in a form apb refuses to check"),
            )]
        }
        _ => Vec::new(),
    }
}

/// A refused start: the gate's structured refusal and every action it
/// needs.
#[derive(Debug, Clone)]
pub struct StartRefusal {
    pub refusal: Value,
    pub actions: Vec<StartAction>,
}

/// The run gate for a start, with the start actions of a refusal
/// (`check_run` itself is unchanged; this only reads its answers). On
/// success the permit comes back with the deprecation note a bare `true`
/// confirmation earns.
pub fn check_start(
    root: &Path,
    wref: &PlaybookRef,
    acknowledge_untrusted: bool,
    supervised: bool,
    confirmation: Option<&crate::consent::Confirmation>,
    launch: &crate::gate::launch::LaunchContext,
) -> Result<(RunPermit, Option<&'static str>), StartRefusal> {
    let of = StartOf::of(wref);
    let refused = |refusal: Value| {
        let mut actions = from_refusal(of, &refusal);
        actions.sort_by_key(|a| a.code);
        StartRefusal { refusal, actions }
    };
    let permit = crate::gate::check_run(root, wref, acknowledge_untrusted, supervised, launch)
        .map_err(refused)?;
    let note = permit.check_confirmation(confirmation).map_err(refused)?;
    Ok((permit, note))
}

/// What a fit item's code asks for, whatever its severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitRule {
    ProvideParam,
    InstallAgent,
    SetEnv,
    RunDoctor,
    Manual,
    /// Nothing a start action can do.
    NoAction,
}

/// Every code the fit report emits, with what it asks for. A code the
/// table does not know gives no action; a test keeps the table complete.
pub const FIT_CODES: &[(&str, FitRule)] = &[
    ("param_needs_value", FitRule::ProvideParam),
    ("agent_not_installed", FitRule::InstallAgent),
    ("connector_env_missing", FitRule::SetEnv),
    ("model_not_available", FitRule::RunDoctor),
    ("requires_unmet", FitRule::Manual),
    ("requires_unsafe", FitRule::Manual),
    ("requires_unsafe_path", FitRule::Manual),
    ("requires_unsafe_command", FitRule::Manual),
    ("connector_no_account", FitRule::Manual),
    ("skill_missing", FitRule::Manual),
    ("profile_missing", FitRule::Manual),
    ("profile_unresolved", FitRule::Manual),
    ("model_not_allowed", FitRule::Manual),
    ("model_policy_violation", FitRule::Manual),
    ("model_unknown", FitRule::Manual),
    ("draft_requires_trial", FitRule::NoAction),
    ("retired_not_runnable", FitRule::NoAction),
    ("not_found", FitRule::NoAction),
    ("sub_playbook_cycle", FitRule::NoAction),
    ("tree_not_assessed", FitRule::NoAction),
    ("connector_unresolved", FitRule::NoAction),
    ("session_handoff_cold", FitRule::NoAction),
    ("isolation", FitRule::NoAction),
    ("publish_target", FitRule::NoAction),
    ("remembered_incompatible", FitRule::NoAction),
    ("remembered_ignored", FitRule::NoAction),
    ("remembered_unreadable", FitRule::NoAction),
    ("model_new_in_family", FitRule::NoAction),
    ("model_unverifiable", FitRule::NoAction),
    ("V76", FitRule::NoAction),
    ("V91", FitRule::NoAction),
    ("V92", FitRule::NoAction),
];

/// The rule of a fit code; an unknown code gives no action.
pub fn fit_rule(code: &str) -> FitRule {
    FIT_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(FitRule::NoAction, |(_, r)| *r)
}

/// The start actions of a fit report: one per actionable compatibility or
/// setup item, an acknowledge when an unapproved part is one an acknowledge
/// covers, and an approval by digest per unapproved part. Consent needs the
/// gate's nonce, so it is not here.
pub fn from_fit(report: &FitReport) -> Vec<StartAction> {
    let of = StartOf {
        playbook: &report.id,
        version: Some(&report.version),
    };
    let mut out: Vec<StartAction> = report
        .compatibility
        .iter()
        .chain(&report.setup)
        .filter_map(|item| fit_item_action(of, item))
        .collect();
    let unapproved: Vec<_> = report.trust.iter().filter(|t| !t.approved).collect();
    if unapproved.iter().any(|t| t.acknowledge_covers) {
        out.push(of.acknowledge(
            "parts of this playbook are not approved; acknowledge them for this start".into(),
        ));
    }
    for t in unapproved {
        let reason = format!(
            "nobody approved {} `{}`",
            t.kind.replace('_', " "),
            t.subject
        );
        out.push(match &t.digest {
            Some(digest) => of.fix(
                Remedy::ApproveDigest {
                    kind: t.kind.clone(),
                    digest: digest.clone(),
                },
                reason,
            ),
            None => of.fix(
                Remedy::Manual {
                    detail: format!("approve {} `{}`", t.kind.replace('_', " "), t.subject),
                },
                reason,
            ),
        });
    }
    out
}

fn fit_item_action(of: StartOf<'_>, item: &FitItem) -> Option<StartAction> {
    let subject = item.subject.clone();
    let manual = || Remedy::Manual {
        detail: item.detail.clone(),
    };
    let remedy = match fit_rule(&item.code) {
        FitRule::NoAction => return None,
        FitRule::ProvideParam => subject.map_or_else(manual, |name| Remedy::ProvideParam { name }),
        FitRule::SetEnv => subject.map_or_else(manual, |name| Remedy::SetEnv { name }),
        // An agent finding reads `<agent> <detail>` (see the fit's profile
        // items); its subject is the profile.
        FitRule::InstallAgent => {
            item.detail
                .split_whitespace()
                .next()
                .map_or_else(manual, |agent| Remedy::InstallAgent {
                    agent: agent.to_string(),
                })
        }
        FitRule::RunDoctor => Remedy::RunDoctor,
        FitRule::Manual => manual(),
    };
    let about = item
        .subject
        .as_deref()
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    Some(of.fix(remedy, format!("{}{about}: {}", item.code, item.detail)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(rel: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
    }

    /// Every code the fit report can emit is in [`FIT_CODES`]: the literal
    /// codes of the fit module, the model checks' codes, the missing-script
    /// codes, and the refusals of the sub-playbook walk the report turns
    /// into compatibility items. A new code without a row fails here.
    /// A held playbook's refusal offers a person the clear command.
    #[test]
    fn a_hold_refusal_offers_the_clear_command() {
        let refusal = serde_json::json!({
            "policy": "playbook_on_hold",
            "id": "digest",
            "detail": "playbook `digest` is on hold since 2026-11-10T06:00:00Z: consecutive_failures (5 failed in a row); a person clears it with `apb health clear digest`",
            "rule": "consecutive_failures",
            "since": "2026-11-10T06:00:00Z",
        });
        let actions = from_refusal(
            StartOf {
                playbook: "digest",
                version: None,
            },
            &refusal,
        );
        assert_eq!(actions.len(), 1, "{actions:?}");
        let a = &actions[0];
        assert_eq!(a.code, StartActionCode::FixRequirement);
        assert_eq!(
            a.remedy,
            Some(Remedy::Manual {
                detail: "apb health clear digest".into()
            })
        );
        assert!(a.reason.contains("on hold"), "{}", a.reason);
    }

    #[test]
    fn the_fit_table_covers_every_code_the_fit_report_emits() {
        let literal = regex::Regex::new(
            r#"(?:\bitem\(\s*|\bcode:\s*|\bfinding\(\s*|=>\s*|reference_issue\(\s*)"([A-Za-z][A-Za-z0-9_]*)""#,
        )
        .unwrap();
        let mut emitted = std::collections::BTreeSet::new();
        for file in [
            "src/fit/mod.rs",
            "src/fit/setup.rs",
            "../apb-core/src/model_check.rs",
            "../apb-core/src/preflight.rs",
        ] {
            let text = source(file);
            // Only the code function of the model checks maps to codes.
            let text = match file.ends_with("model_check.rs") {
                true => {
                    let at = text.find("pub fn code(&self)").unwrap();
                    text[at..at + text[at..].find("\n    }\n").unwrap()].to_string()
                }
                false => text,
            };
            emitted.extend(literal.captures_iter(&text).map(|c| c[1].to_string()));
        }
        // What `assess` reports from the walk of the sub-playbook tree
        // (`gate::resolve_tree`) when it is not a trust refusal.
        emitted.extend(
            [
                "not_found",
                "sub_playbook_cycle",
                "draft_requires_trial",
                "retired_not_runnable",
                "requires_unmet",
                "requires_unsafe_path",
                "requires_unsafe_command",
                "connector_unresolved",
                "connector_env_missing",
                "profile_unresolved",
            ]
            .map(str::to_string),
        );
        // The profile finding that goes to the trust section, not to setup.
        emitted.remove(crate::fit::UNTRUSTED);
        let known: std::collections::BTreeSet<_> =
            FIT_CODES.iter().map(|(c, _)| c.to_string()).collect();
        let missing: Vec<_> = emitted.difference(&known).collect();
        assert!(missing.is_empty(), "fit codes without a row: {missing:?}");
        assert!(emitted.contains("param_needs_value") && emitted.contains("V91"));
        assert_eq!(fit_rule("param_needs_value"), FitRule::ProvideParam);
        assert_eq!(fit_rule("no_such_code"), FitRule::NoAction);
    }

    #[test]
    fn the_codes_are_closed_lists_with_stable_names() {
        let names: Vec<_> = StartActionCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "confirm_irreversible_start",
                "fix_requirement",
                "acknowledge_untrusted",
                "confirm_launch",
                "launch_by_person"
            ]
        );
        for c in StartActionCode::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
        for c in crate::next_action::ActionCode::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }
}
