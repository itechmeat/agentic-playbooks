//! Planning the role profiles the shipped playbooks name: `apb-main` does
//! the work, `apb-reviewer` reviews it, `apb-researcher` gathers sources.
//!
//! Pure: the plan comes from the detected agents ([`crate::detect`]) and the
//! models table with the user's subscriptions ([`crate::models_table`]); no
//! model or provider is named here. Writing the profiles is the normal
//! profile write of the user's own profiles (normal profile trust, never
//! `Bundled`), done by the surface that applies the plan.

use serde::Serialize;

use crate::detect::AgentInfo;
use crate::models_table::{ModelsTable, model_options_for_agent, static_models_for_agent};

/// The profile that does the work.
pub const ROLE_MAIN: &str = "apb-main";
/// The profile that reviews the work.
pub const ROLE_REVIEWER: &str = "apb-reviewer";
/// The profile that gathers and weighs sources.
pub const ROLE_RESEARCHER: &str = "apb-researcher";

/// The shipped skills each role profile carries, as the shipped playbooks
/// use them: the main profile designs, delivers, diagnoses bugs before a
/// fix, gates its own tests, self-reviews subtasks, chooses executors and
/// runs apb itself in `apb-setup`; the reviewer reviews, with the test and
/// security audits its roles always run; the researcher gathers sources and
/// needs none of them.
const ROLE_SKILLS: &[(&str, &[&str])] = &[
    (
        ROLE_MAIN,
        &[
            "apb-brainstorm",
            "apb-delivery",
            "apb-diagnosis",
            "apb-review",
            "apb-test-audit",
            "apb-security-audit",
            "apb-model-selection",
            "apb-usage",
        ],
    ),
    (
        ROLE_REVIEWER,
        &["apb-review", "apb-test-audit", "apb-security-audit"],
    ),
    (ROLE_RESEARCHER, &[]),
];

/// The shipped skills the role profile `name` carries (empty for a name
/// that is not a role). A surface writes only the ones installed.
pub fn role_skills(name: &str) -> &'static [&'static str] {
    ROLE_SKILLS
        .iter()
        .find(|(role, _)| *role == name)
        .map_or(&[], |(_, skills)| skills)
}

/// Whether the reviewer runs on another agent than the main profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Independence {
    /// The reviewer is another agent CLI.
    SeparateAgent,
    /// Only one agent is usable: the reviewer is the same agent in a fresh
    /// session, a limit the surfaces must say out loud.
    SameAgent,
}

/// One fallback executor of a planned profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleFallback {
    pub agent: String,
    pub model: String,
}

/// One planned role profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleProfile {
    pub name: String,
    pub description: String,
    pub agent: String,
    pub model: String,
    pub fallbacks: Vec<RoleFallback>,
    /// The role prompt (`SOUL.md`).
    pub soul_md: String,
    /// Skill names the profile carries; empty in the plan, a surface adds
    /// the installed ones of [`role_skills`] before writing.
    pub skills: Vec<String>,
}

/// A detected agent the plan does not use, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedAgent {
    pub agent: String,
    pub reason: String,
}

/// The role profiles to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RolePlan {
    /// `apb-main`, `apb-reviewer`, `apb-researcher`, in that order.
    pub profiles: Vec<RoleProfile>,
    pub independence: Independence,
    pub skipped: Vec<SkippedAgent>,
}

/// Why no plan could be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error(
        "no_usable_agent: no installed agent CLI with a model in the models table; install one, or run playbooks in host mode"
    )]
    NoUsableAgent,
}

impl PlanError {
    /// The machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            PlanError::NoUsableAgent => "no_usable_agent",
        }
    }
}

/// The default model of `agent` from the models table: the first entry of
/// its closed list; else the first table row of its vendor; else (an
/// aggregator, which needs its own spelling) the first model the agent
/// itself reported that the table offers. `None` when there is none.
pub fn default_model(agent: &AgentInfo, table: &ModelsTable) -> Option<String> {
    default_choice(agent, table).map(|(id, _)| id)
}

/// [`default_model`] with its family: the vendor of that model, `None`
/// when nothing says which it is.
fn default_choice(agent: &AgentInfo, table: &ModelsTable) -> Option<(String, Option<String>)> {
    let own = crate::models_table::agent_vendor(&agent.agent);
    if let Some(list) = static_models_for_agent(&agent.agent, table) {
        return list
            .into_iter()
            .next()
            .map(|id| (id, own.map(str::to_string)));
    }
    let detected = agent
        .models
        .as_ref()
        .map(|m| m.items.clone())
        .unwrap_or_default();
    let mut options = model_options_for_agent(&agent.agent, &detected, table).into_iter();
    let chosen = match own {
        Some(_) => options.next(),
        None => options.find(|o| o.detected),
    }?;
    let family = Some(chosen.vendor).filter(|v| !v.is_empty());
    Some((chosen.id, family))
}

/// A usable agent with its default model and that model's family.
struct Usable {
    agent: String,
    model: String,
    family: Option<String>,
}

impl Usable {
    /// How `self` ranks as a step away from the families in `away`: another
    /// known family first, an unknown one next, one of `away` last.
    fn rank_away(&self, away: &[Option<&str>]) -> u8 {
        match self.family.as_deref() {
            Some(f) if away.contains(&Some(f)) => 2,
            Some(_) => 0,
            None => 1,
        }
    }
}

/// Plans the three role profiles from the detected `agents` and `table`
/// (with the user's subscriptions in it).
///
/// An agent is usable when it is installed, apb knows how to launch it, and
/// it has a default model ([`default_model`]). Order of preference: the
/// agents the subscriptions name, in their order, then the rest in
/// detection order. `apb-main` and `apb-researcher` take the first usable
/// agent; `apb-reviewer` takes the first usable agent whose model is of
/// another known family, else of an unknown one, else another agent, else
/// the same agent with [`Independence::SameAgent`]. Each profile falls back
/// to the other usable agents (the reviewer only to agents other than the
/// main one), another known family first, an unknown one next and its own
/// family (and, for the reviewer, the builder's) last, in order within each.
pub fn plan_role_profiles(
    agents: &[AgentInfo],
    table: &ModelsTable,
) -> Result<RolePlan, PlanError> {
    let mut skipped = Vec::new();
    let mut usable: Vec<Usable> = Vec::new();
    for a in agents {
        let reason = if !a.installed {
            Some("not installed")
        } else if crate::agents::lookup(&a.agent).is_none() {
            Some("not a built-in agent")
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push(SkippedAgent {
                agent: a.agent.clone(),
                reason: reason.to_string(),
            });
            continue;
        }
        match default_choice(a, table) {
            Some((model, family)) => usable.push(Usable {
                agent: a.agent.clone(),
                model,
                family,
            }),
            None => skipped.push(SkippedAgent {
                agent: a.agent.clone(),
                reason: "no default model in the models table".to_string(),
            }),
        }
    }
    // Subscribed agents first, in subscription order; the sort is stable.
    let rank = |agent: &str| {
        table
            .subscriptions
            .iter()
            .position(|s| s.agent == agent)
            .unwrap_or(usize::MAX)
    };
    usable.sort_by_key(|u| rank(&u.agent));
    let main = usable.first().ok_or(PlanError::NoUsableAgent)?;
    let builder = [main.family.as_deref()];
    let reviewer = usable
        .iter()
        .filter(|u| u.agent != main.agent)
        .min_by_key(|u| u.rank_away(&builder));
    let independence = if reviewer.is_some() {
        Independence::SeparateAgent
    } else {
        Independence::SameAgent
    };
    let reviewer = reviewer.unwrap_or(main);
    let fallbacks = |primary: &Usable, exclude: Option<&str>| -> Vec<RoleFallback> {
        let away = [primary.family.as_deref(), main.family.as_deref()];
        let mut rest: Vec<&Usable> = usable
            .iter()
            .filter(|u| u.agent != primary.agent && Some(u.agent.as_str()) != exclude)
            .collect();
        rest.sort_by_key(|u| u.rank_away(&away));
        rest.into_iter()
            .map(|u| RoleFallback {
                agent: u.agent.clone(),
                model: u.model.clone(),
            })
            .collect()
    };
    let reviewer_exclude =
        (independence == Independence::SeparateAgent).then_some(main.agent.as_str());
    let profile = |name: &str, who: &Usable, fallbacks: Vec<RoleFallback>| RoleProfile {
        name: name.to_string(),
        description: role_description(name).to_string(),
        agent: who.agent.clone(),
        model: who.model.clone(),
        fallbacks,
        soul_md: role_soul(name).to_string(),
        skills: Vec::new(),
    };
    Ok(RolePlan {
        profiles: vec![
            profile(ROLE_MAIN, main, fallbacks(main, None)),
            profile(
                ROLE_REVIEWER,
                reviewer,
                fallbacks(reviewer, reviewer_exclude),
            ),
            profile(ROLE_RESEARCHER, main, fallbacks(main, None)),
        ],
        independence,
        skipped,
    })
}

fn role_description(name: &str) -> &'static str {
    match name {
        ROLE_MAIN => "Does the work of the shipped playbooks: designs, changes and verifies.",
        ROLE_REVIEWER => "Reviews the work of the shipped playbooks independently, read only.",
        _ => "Gathers and weighs sources for the shipped playbooks.",
    }
}

fn role_soul(name: &str) -> &'static str {
    match name {
        ROLE_MAIN => {
            "You do the work a playbook step asks for. Keep to the request and its expected result, change only what the step needs, check your work with the project's own commands, and report what you did and what you could not do.\n"
        }
        ROLE_REVIEWER => {
            "You review a change you did not make. Read only: never edit the work under review. Compare it with the original request, verify each finding before you report it, and separate defects from preferences.\n"
        }
        _ => {
            "You gather sources for a question. Prefer primary sources, cite each one, keep findings apart from guesses, and say what you could not check.\n"
        }
    }
}
