//! The readiness section of the fit report: whether a playbook is fit to
//! run alone. Done defined by an enforced script goal criterion, every
//! acting agent step checked by a script, every acting node bounded in
//! each effect domain it acts in, a budget, every irreversible step
//! behind a gate along the paths a run can take, and its probation. The
//! pure rules live in `apb_core::readiness`; this module answers the
//! machine's part (what isolation this machine enforces) and builds the
//! report the surfaces show.
//!
//! `ready` never changes `FitReport::ready` (that stays "nothing blocks a
//! run here"): readiness is about running alone, not about running.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

use apb_core::isolation::contract::Backend;
use apb_core::profile_store::PlaybookOrigin;
use apb_core::readiness::{BoundaryDomain, Enforced};
use apb_core::registry::LoadedPlaybook;

use crate::gate::unattended::{ProbationView, probation};

/// Whether a playbook is fit to run alone.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    /// Every item holds.
    pub ready: bool,
    pub items: Vec<ReadinessItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub probation: Option<ProbationView>,
}

/// One readiness item, holding or not.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadinessItem {
    pub code: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subject: Option<String>,
    pub detail: String,
}

/// What this machine enforces of every isolated node of `loaded`, as the
/// readiness rule reads it: `tree` when the node's contract is allowed and
/// its backend is the sandbox, `network_closed` when its holds also close
/// the network.
fn enforced_map(
    root: &Path,
    loaded: &LoadedPlaybook,
    origin: PlaybookOrigin,
) -> BTreeMap<String, Enforced> {
    enforced_of(crate::gate::isolation::node_contracts(
        root,
        &loaded.playbook,
        origin,
        false,
    ))
}

/// [`enforced_map`] over contracts already evaluated.
fn enforced_of(contracts: Vec<crate::gate::isolation::NodeContract>) -> BTreeMap<String, Enforced> {
    use apb_core::isolation::contract::Code;
    let mut out = BTreeMap::new();
    for nc in contracts {
        let allowed = nc.contract.refusals.is_empty() && nc.contract.backend == Backend::Bwrap;
        let network_closed = allowed && nc.contract.holds.contains(&Code::NetworkNone);
        out.insert(
            nc.node,
            Enforced {
                tree: allowed,
                network_closed,
            },
        );
    }
    out
}

/// The readiness of `loaded` (scope label `scope`) in the project at
/// `root`, with the machine's isolation answers and the probation of the
/// playbook itself.
pub(crate) fn assess(
    root: &Path,
    scope: &str,
    loaded: &LoadedPlaybook,
    origin: PlaybookOrigin,
) -> Readiness {
    let playbook = &loaded.playbook;
    let mut items: Vec<ReadinessItem> = Vec::new();
    // 1. done_defined: the goal has enforced script criteria.
    let enforced_scripts = playbook
        .goal
        .as_ref()
        .filter(|g| g.enforce)
        .map(|g| {
            g.criteria
                .iter()
                .filter(|c| matches!(c.check, apb_core::schema::GoalCheck::Script { .. }))
                .count()
        })
        .unwrap_or(0);
    items.push(ReadinessItem {
        code: "done_defined".into(),
        ok: enforced_scripts > 0,
        subject: None,
        detail: if enforced_scripts > 0 {
            format!("the goal has {enforced_scripts} enforced script criteria")
        } else {
            "nothing checks when the run is done: the goal has no script criterion under goal.enforce: true".into()
        },
    });
    // 2. Every acting agent_task has a deterministic check.
    let unchecked = apb_core::readiness::unchecked_actors(playbook);
    if unchecked.is_empty() {
        items.push(ReadinessItem {
            code: "checked_actors".into(),
            ok: true,
            subject: None,
            detail: "every acting agent_task has a deterministic check".into(),
        });
    } else {
        for id in unchecked {
            items.push(ReadinessItem {
                code: "unchecked_actor".into(),
                ok: false,
                subject: Some(id.to_string()),
                detail: format!("acting agent_task `{id}` has no deterministic check"),
            });
        }
    }
    // 3. Every acting node has a boundary in each effect domain it acts in.
    let enforced = enforced_map(root, loaded, origin);
    let isolation_answer = |id: &str| enforced.get(id).copied().unwrap_or_default();
    let unbounded = apb_core::readiness::unbounded_actors(playbook, &isolation_answer);
    if unbounded.is_empty() {
        items.push(ReadinessItem {
            code: "bounded_actors".into(),
            ok: true,
            subject: None,
            detail: "every acting node has a boundary in the working tree, at every connector it binds and on the network".into(),
        });
    } else {
        for u in unbounded {
            let subject = match &u.domain {
                BoundaryDomain::Connector(c) => Some(format!("{}/{c}", u.node)),
                _ => Some(u.node.to_string()),
            };
            let detail = match u.domain {
                BoundaryDomain::Tree => format!(
                    "acting node `{}` can change the working tree without a boundary: set read_only or writes, or an isolation this machine enforces",
                    u.node
                ),
                BoundaryDomain::Connector(_) => format!(
                    "acting node `{}` calls connector `{}` without a limit: allow only read_only functions, or name its functions and set max_calls",
                    u.node,
                    subject
                        .as_deref()
                        .unwrap_or_default()
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                ),
                BoundaryDomain::Network => format!(
                    "acting node `{}` is not bounded externally: it can reach the network and external services outside its connectors, and only an isolation with network: none that this machine enforces limits that",
                    u.node
                ),
                BoundaryDomain::FinishAgent => format!(
                    "finish node `{}` runs an agent and cannot declare a local boundary (read_only, writes and isolation apply to agent_task and script nodes only): move the work into a bounded agent_task, or leave it unbounded",
                    u.node
                ),
            };
            items.push(ReadinessItem {
                code: "unbounded_actor".into(),
                ok: false,
                subject,
                detail,
            });
        }
    }
    // 4. A budget limits a run.
    items.push(ReadinessItem {
        code: "budget".into(),
        ok: playbook.budget.is_some(),
        subject: None,
        detail: if playbook.budget.is_some() {
            "a budget limits a run".into()
        } else {
            "no budget: a run alone spends without a limit".into()
        },
    });
    // 5. Every irreversible step behind a gate on every path a run can take.
    let ungated = apb_core::readiness::ungated_irreversible(playbook);
    if ungated.is_empty() {
        items.push(ReadinessItem {
            code: "irreversible_gated".into(),
            ok: true,
            subject: None,
            detail: "every irreversible step is behind a gate".into(),
        });
    } else {
        for id in ungated {
            items.push(ReadinessItem {
                code: "ungated_irreversible".into(),
                ok: false,
                subject: Some(id.to_string()),
                detail: format!(
                    "irreversible node `{id}` is reached without a human_review or publish gate, from the start or a failure route"
                ),
            });
        }
    }
    // 6. The playbook's own probation. Only the playbook itself: a pinned
    // child's probation is refused at the start and is not a readiness
    // item.
    let view = match probation(root, scope, loaded) {
        Ok(view) => view,
        Err(e) => {
            items.push(ReadinessItem {
                code: "probation".into(),
                ok: false,
                subject: None,
                detail: format!(
                    "{e}; the probation it may ask for cannot be known, so an unattended start is refused until it is fixed"
                ),
            });
            None
        }
    };
    if let Some(p) = &view {
        // The machine's waiver passes an unverifiable ledger too, as the
        // start check does.
        let ok = p.waived || (p.ledger_ok && p.counted >= p.required);
        let waived = if p.waived {
            ", waived on this machine"
        } else {
            ""
        };
        let detail = if !p.ledger_ok {
            format!(
                "the probation ledger of this playbook in this project does not verify; move it aside to count again from zero{waived}"
            )
        } else {
            format!(
                "{} of {} attended runs of this definition in this project{waived}",
                p.counted, p.required
            )
        };
        items.push(ReadinessItem {
            code: "probation".into(),
            ok,
            subject: None,
            detail,
        });
    }
    let ready = items.iter().all(|i| i.ok);
    Readiness {
        ready,
        items,
        probation: view,
    }
}

/// The detail a surface names when readiness does not pass: the `detail`
/// of every item that is not ok, joined by `"; "`. `None` when ready.
pub fn not_ready_detail(r: &Readiness) -> Option<String> {
    (!r.ready).then(|| {
        r.items
            .iter()
            .filter(|i| !i.ok)
            .map(|i| i.detail.clone())
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// The preparation check of `--require-ready`: `assess` on `loaded` and on
/// every pinned sub-playbook (each child with its own scope label and the
/// origin of its pin), its items without the one whose code is
/// `probation` (probation is decided by [`crate::gate::unattended::
/// check_probation`], so a waived start is not refused on it here), and
/// the first refusal text. `Err` carries the `not_ready: ` detail.
pub(crate) fn start_check(
    root: &Path,
    scope: &str,
    loaded: &LoadedPlaybook,
    origin: PlaybookOrigin,
    pins: Option<&BTreeMap<String, crate::run_config::ChildExpectation>>,
) -> Result<(), String> {
    let one =
        |scope: &str, child: &LoadedPlaybook, child_origin: PlaybookOrigin| -> Option<String> {
            let mut r = assess(root, scope, child, child_origin);
            r.items.retain(|i| i.code != "probation");
            r.ready = r.items.iter().all(|i| i.ok);
            not_ready_detail(&r)
        };
    if let Some(detail) = one(scope, loaded, origin) {
        return Err(format!(
            "{}: playbook `{}` {} is not ready to run alone: {detail}",
            crate::run_config::NOT_READY,
            loaded.playbook.id,
            loaded.version
        ));
    }
    let Some(pins) = pins else {
        return Ok(());
    };
    for (node, pin) in pins {
        let child_scope = match pin.scope {
            apb_core::profile::ProfileScope::Global => "global",
            _ => "project",
        };
        let child_origin = match pin.scope {
            apb_core::profile::ProfileScope::Global => PlaybookOrigin::Global,
            _ => PlaybookOrigin::Project,
        };
        let Some(child) = pin.load_definition(root) else {
            return Err(format!(
                "{}: sub-playbook `{}` (node `{node}`) cannot be read to assess its readiness; start the run again once it reads",
                crate::run_config::NOT_READY,
                pin.id
            ));
        };
        if let Some(detail) = one(child_scope, &child, child_origin) {
            return Err(format!(
                "{}: sub-playbook `{}` {} (node `{node}`) is not ready to run alone: {detail}",
                crate::run_config::NOT_READY,
                child.playbook.id,
                child.version
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests live in `crates/apb-engine/tests/suite/probation_test.rs`
    /// (the start checks against real preparation) and in
    /// `crates/apb-engine/tests/suite/fit_test.rs` (the report shapes),
    /// where the fit helpers stand; this module's own unit tests hold the
    /// wording of the items.
    #[test]
    fn not_ready_detail_joins_the_missing_items() {
        let r = Readiness {
            ready: false,
            items: vec![
                ReadinessItem {
                    code: "budget".into(),
                    ok: false,
                    subject: None,
                    detail: "no budget: a run alone spends without a limit".into(),
                },
                ReadinessItem {
                    code: "checked_actors".into(),
                    ok: true,
                    subject: None,
                    detail: "every acting agent_task has a deterministic check".into(),
                },
            ],
            probation: None,
        };
        assert_eq!(
            not_ready_detail(&r).as_deref(),
            Some("no budget: a run alone spends without a limit")
        );
        let ready = Readiness {
            ready: true,
            items: vec![ReadinessItem {
                code: "budget".into(),
                ok: true,
                subject: None,
                detail: "a budget limits a run".into(),
            }],
            probation: None,
        };
        assert_eq!(not_ready_detail(&ready), None);
    }

    /// A `best_effort` node on a host without bubblewrap falls back to no
    /// sandbox: it is not bounded in its tree, and the readiness rule
    /// lists it there.
    #[test]
    fn a_best_effort_node_without_a_sandbox_is_not_bounded() {
        use apb_core::isolation::contract::{Backend, HostFacts, NodeFacts, Os, evaluate};
        use apb_core::isolation::{Level, Network};
        let host = HostFacts {
            os: Os::Linux,
            bwrap: None,
            landlock_abi: 0,
            disabled: false,
        };
        let contract = evaluate(
            &host,
            &NodeFacts {
                level: Level::BestEffort,
                network: Network::None,
                agents_without_needs: Vec::new(),
                host_mode: false,
                publish_only: false,
            },
        );
        assert_eq!(contract.backend, Backend::None, "{contract:?}");
        assert!(!contract.fallbacks.is_empty(), "it says why");
        let enforced = enforced_of(vec![crate::gate::isolation::NodeContract {
            node: "work".into(),
            level: Level::BestEffort,
            contract,
        }]);
        assert_eq!(enforced.get("work"), Some(&Enforced::default()));
        let pb = apb_core::schema::Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: P, profile: x, isolation: { level: best_effort, network: none } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
        )
        .unwrap();
        let answer = |id: &str| enforced.get(id).copied().unwrap_or_default();
        let unbounded = apb_core::readiness::unbounded_actors(&pb, &answer);
        assert!(
            unbounded
                .iter()
                .any(|u| u.node == "work" && u.domain == BoundaryDomain::Tree),
            "{unbounded:?}"
        );
    }
}
