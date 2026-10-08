//! Is a profile's `(agent, model)` pair one this machine can run, and one the
//! user's policy allows? The single judgement `apb validate`, `apb doctor`
//! and the adoption report (`apb adopt`, MCP `playbook_adopt_report`) share;
//! each surface only decides how loudly to report an [`ModelIssue`].
//!
//! What it knows, strongest first:
//! - zcode's allowlist (a hard gate, see [`crate::zcode::check_model_allowed`]);
//! - the global config's `model_policy` ([`ModelRule`]);
//! - apb's closed model lists (claude, codex, zcode; see
//!   [`models_table::static_models_for_agent`]): an id outside the list is
//!   probably a typo or a retired model, except a claude id that follows a
//!   known family's pattern (`claude-sonnet-6-0`), most likely a model newer
//!   than this binary, which passes with an info note;
//! - detection: an installed agent whose list has Full authority
//!   (`opencode models`) names exactly what it can run.

use crate::config::{GlobalConfig, ModelRule};
use crate::detect::{self, AgentInfo, Authority};
use crate::models_table::{self, ModelsTable};

/// Why a model will not run, or should not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelIssue {
    /// Outside zcode's allowlist; `detail` names the allowed models.
    NotAllowed(String),
    /// Refused by a `model_policy` rule of the global config; `detail` names
    /// the rule's allowed models and its reason.
    PolicyViolation(String),
    /// Not on apb's closed list for this agent (`detail` names the list).
    Unknown(String),
    /// Not on apb's closed claude list, but a well-formed id of a known
    /// Claude family (`claude-<family>-<major>-<minor>`, optionally with a
    /// `-YYYYMMDD` date): most likely a model released after this binary.
    /// Informational only, never a warning.
    NewInFamily,
    /// The installed agent lists its models with Full authority and this one
    /// is not among them.
    NotAvailable,
    /// The agent is one detection knows, but it is not installed.
    AgentNotInstalled,
    /// Nothing can confirm or refute the model (a custom agent, a partial or
    /// missing list).
    Unverifiable,
}

impl ModelIssue {
    /// The stable finding code every surface prints.
    pub fn code(&self) -> &'static str {
        match self {
            ModelIssue::NotAllowed(_) => "model_not_allowed",
            ModelIssue::PolicyViolation(_) => "model_policy_violation",
            ModelIssue::Unknown(_) => "model_unknown",
            ModelIssue::NewInFamily => "model_new_in_family",
            ModelIssue::NotAvailable => "model_not_available",
            ModelIssue::AgentNotInstalled => "agent_not_installed",
            ModelIssue::Unverifiable => "model_unverifiable",
        }
    }

    /// Whether the run would be refused or the user's rule broken, as opposed
    /// to a likely-but-unproven problem.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            ModelIssue::NotAllowed(_) | ModelIssue::PolicyViolation(_)
        )
    }

    /// Whether this is only a note: the model is accepted and nothing needs
    /// fixing.
    pub fn is_info(&self) -> bool {
        matches!(self, ModelIssue::NewInFamily)
    }

    /// A one-line explanation naming the agent and model.
    pub fn describe(&self, agent: &str, model: &str) -> String {
        match self {
            ModelIssue::NotAllowed(d) | ModelIssue::PolicyViolation(d) => {
                format!("{agent} model `{model}`: {d}")
            }
            ModelIssue::Unknown(list) => format!(
                "{agent} model `{model}` is not one apb knows for {agent} ({list}); check the id"
            ),
            ModelIssue::NewInFamily => format!(
                "{agent} model `{model}` is not on apb's list yet; it matches a known Claude family's id pattern, so it is accepted as a newer model"
            ),
            ModelIssue::NotAvailable => {
                format!("{agent} does not list model `{model}` on this machine")
            }
            ModelIssue::AgentNotInstalled => format!("agent {agent} is not installed"),
            ModelIssue::Unverifiable => {
                format!("{agent} model `{model}` cannot be verified on this machine")
            }
        }
    }
}

/// Everything a check needs, loaded once per command.
pub struct ModelContext {
    pub agents: Vec<AgentInfo>,
    pub table: ModelsTable,
    pub zcode_family: String,
    pub policy: Vec<ModelRule>,
}

impl ModelContext {
    /// The context for this machine: detection (memoized), the merged models
    /// table (the embedded one when the user overlay is broken) and the global
    /// config's policy (none when the config does not load; `apb doctor`
    /// reports that on its own).
    pub fn load() -> Self {
        Self {
            agents: crate::agent_catalog::agents(false),
            table: models_table::load_merged().unwrap_or_else(|_| models_table::builtin()),
            zcode_family: crate::zcode::home_dir()
                .map(|h| crate::zcode::account_family(&h))
                .unwrap_or_else(|| crate::zcode::DEFAULT_FAMILY.to_string()),
            policy: GlobalConfig::load()
                .map(|g| g.model_policy)
                .unwrap_or_default(),
        }
    }
}

/// Claude Code's own model aliases: it accepts them for `--model` and resolves
/// them itself, so they are known models, not typos.
const CLAUDE_ALIASES: &[&str] = &["default", "opus", "sonnet", "haiku", "fable", "opusplan"];

/// The Claude model families apb knows. An unlisted id of one of them in
/// Anthropic's id pattern is accepted with a note ([`ModelIssue::NewInFamily`]).
const CLAUDE_FAMILIES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// The first issue with running `model` on `agent`, or `None` when nothing is
/// wrong as far as this machine can tell.
pub fn check(agent: &str, model: &str, cx: &ModelContext) -> Option<ModelIssue> {
    let probe = detect::canonical_agent_id(agent);
    // The id the lists name: zcode's canonical bare model without its effort
    // suffix (`zai-individual/glm-5.3-flash@high` -> `GLM-5.3-Flash`).
    let bare: String = if probe == crate::zcode::AGENT_ID {
        if let Err(e) = crate::zcode::check_model_allowed(model, &cx.zcode_family) {
            return Some(ModelIssue::NotAllowed(e));
        }
        let canonical = crate::zcode::canonical_model(model, &cx.zcode_family);
        strip_effort(&canonical).to_string()
    } else {
        model.to_string()
    };
    if let Some(issue) = policy_issue(probe, model, &bare, &cx.policy) {
        return Some(issue);
    }
    let closed = models_table::static_models_for_agent(probe, &cx.table);
    if let Some(list) = &closed
        && !on_closed_list(probe, &bare, list)
    {
        if probe == "claude" && is_claude_family_id(bare.strip_suffix("[1m]").unwrap_or(&bare)) {
            return Some(ModelIssue::NewInFamily);
        }
        return Some(ModelIssue::Unknown(list.join(", ")));
    }
    let Some(info) = cx.agents.iter().find(|a| a.agent == probe) else {
        return closed.is_none().then_some(ModelIssue::Unverifiable);
    };
    if !info.installed {
        return Some(ModelIssue::AgentNotInstalled);
    }
    let listed = |m: &detect::ModelsInventory| m.items.iter().any(|x| x == &bare);
    match &info.models {
        Some(m) if m.authority == Authority::Full => {
            (!listed(m)).then_some(ModelIssue::NotAvailable)
        }
        _ if closed.is_some() => None,
        Some(m) if listed(m) => None,
        _ => Some(ModelIssue::Unverifiable),
    }
}

fn strip_effort(model: &str) -> &str {
    model.split_once('@').map_or(model, |(m, _)| m)
}

fn on_closed_list(probe: &str, bare: &str, list: &[String]) -> bool {
    if list.iter().any(|m| m == bare) {
        return true;
    }
    // claude: an alias, or any listed id with Claude Code's `[1m]` context
    // suffix.
    probe == "claude" && {
        let base = bare.strip_suffix("[1m]").unwrap_or(bare);
        CLAUDE_ALIASES.contains(&base) || list.iter().any(|m| m == base)
    }
}

/// Whether `id` is `claude-<family>-<major>-<minor>` with a known family and
/// one- or two-digit version parts, optionally followed by a `-YYYYMMDD` date
/// (`claude-sonnet-6-0`, `claude-haiku-5-5-20261007`).
fn is_claude_family_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("claude-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let digits = |s: &str, min: usize, max: usize| {
        (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    match parts.as_slice() {
        [family, major, minor, date @ ..] if date.len() <= 1 => {
            CLAUDE_FAMILIES.contains(family)
                && digits(major, 1, 2)
                && digits(minor, 1, 2)
                && date.iter().all(|d| digits(d, 8, 8))
        }
        _ => false,
    }
}

/// The first `model_policy` rule that covers this model and does not allow it.
/// A rule covers an agent (claude-code counts as claude) and, with `when`,
/// only the models matching that glob; `allow` lists the globs a covered model
/// must match. Globs match case-insensitively, against the model as written
/// and, for zcode, its canonical bare id; an `@effort` suffix is ignored.
fn policy_issue(probe: &str, model: &str, bare: &str, rules: &[ModelRule]) -> Option<ModelIssue> {
    let candidates = [model, strip_effort(model), bare];
    let matches_any = |globs: &[String]| {
        globs.iter().any(|g| {
            globset::GlobBuilder::new(g)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
                .is_ok_and(|m| candidates.iter().any(|c| m.is_match(c)))
        })
    };
    rules
        .iter()
        .filter(|r| detect::canonical_agent_id(&r.agent) == probe)
        .filter(|r| {
            r.when
                .as_ref()
                .is_none_or(|w| matches_any(std::slice::from_ref(w)))
        })
        .find(|r| !matches_any(&r.allow))
        .map(|r| {
            let reason = r
                .reason
                .as_deref()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default();
            ModelIssue::PolicyViolation(format!(
                "the model policy for {} allows only {}{reason}",
                r.agent,
                r.allow.join(", ")
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(policy: Vec<ModelRule>) -> ModelContext {
        ModelContext {
            agents: Vec::new(),
            table: models_table::builtin(),
            zcode_family: crate::zcode::DEFAULT_FAMILY.to_string(),
            policy,
        }
    }

    fn rule(agent: &str, when: Option<&str>, allow: &[&str]) -> ModelRule {
        ModelRule {
            agent: agent.into(),
            when: when.map(str::to_string),
            allow: allow.iter().map(|s| s.to_string()).collect(),
            reason: None,
        }
    }

    /// The closed lists accept exactly their ids, and for claude the CLI's own
    /// aliases and `[1m]` variants; anything else is `model_unknown`. With no
    /// detection (nothing installed) that is the only verdict.
    #[test]
    fn closed_lists_know_their_ids_and_claude_aliases() {
        let c = cx(Vec::new());
        for ok in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-haiku-5-5[1m]",
            "haiku",
            "opus[1m]",
            "claude-opus-5-5[1m]",
        ] {
            assert_eq!(check("claude", ok, &c), None, "{ok}");
            assert_eq!(check("claude-code", ok, &c), None, "{ok}");
        }
        assert!(matches!(
            check("claude", "claude-made-up-9", &c),
            Some(ModelIssue::Unknown(_))
        ));
        assert!(matches!(
            check("codex", "gpt-4-imaginary", &c),
            Some(ModelIssue::Unknown(_))
        ));
        assert_eq!(check("customx", "m1", &c), Some(ModelIssue::Unverifiable));
    }

    /// An unlisted claude id of a known family in Anthropic's id pattern is a
    /// newer model, accepted with a note; an unknown family or a malformed id
    /// stays `model_unknown`, and other agents get no family pass.
    #[test]
    fn unlisted_ids_of_a_known_claude_family_pass_with_a_note() {
        let c = cx(Vec::new());
        for newer in [
            "claude-sonnet-6-0",
            "claude-opus-6-1[1m]",
            "claude-haiku-5-5-20261007",
            "claude-fable-10-12",
        ] {
            let issue = check("claude", newer, &c);
            assert_eq!(issue, Some(ModelIssue::NewInFamily), "{newer}");
            assert!(issue.unwrap().is_info());
        }
        for wrong in [
            "claude-foo-5-5",
            "claude-sonnet-6",
            "claude-sonnet-6-0-1",
            "claude-sonnet-six-0",
            "claude-sonnet-6-0-2026107",
            "claude-sonnet-6-0-preview",
            "sonnet-6-0",
        ] {
            let issue = check("claude", wrong, &c);
            assert!(matches!(issue, Some(ModelIssue::Unknown(_))), "{wrong}");
            assert!(!issue.unwrap().is_info());
        }
        assert!(matches!(
            check("codex", "claude-sonnet-6-0", &c),
            Some(ModelIssue::Unknown(_))
        ));
    }

    /// A policy covers its agent, optionally only the models `when` matches,
    /// and refuses what `allow` does not match; effort suffixes and zcode's
    /// legacy plan prefix do not dodge it.
    #[test]
    fn policy_refuses_what_its_allow_list_does_not_match() {
        let c = cx(vec![
            rule("zcode", None, &["GLM-5.3"]),
            rule("opencode", Some("anthropic/*"), &["*haiku*"]),
        ]);
        assert!(matches!(
            check("zcode", "GLM-5.3-Flash@max", &c),
            Some(ModelIssue::PolicyViolation(_))
        ));
        assert!(matches!(
            check("zcode", "zai-individual/GLM-5.3-Flash", &c),
            Some(ModelIssue::PolicyViolation(_))
        ));
        assert!(!matches!(
            check("zcode", "zai-individual/glm-5.3@low", &c),
            Some(ModelIssue::PolicyViolation(_))
        ));
        assert!(matches!(
            check("opencode", "anthropic/claude-opus-5-5", &c),
            Some(ModelIssue::PolicyViolation(_))
        ));
        assert!(!matches!(
            check("opencode", "anthropic/claude-haiku-5", &c),
            Some(ModelIssue::PolicyViolation(_))
        ));
        // Outside `when`, and other agents, the rules do not apply.
        assert!(!matches!(
            check("opencode", "opencode/big-pickle", &c),
            Some(ModelIssue::PolicyViolation(_))
        ));
        assert_eq!(check("claude", "claude-sonnet-5", &c), None);
    }
}
