//! The `irreversible` consent gate (0.24.0): a run whose effective effects
//! (the playbook's own, its nodes' and every sub-playbook's, recursively)
//! include `irreversible` starts only with an explicit consent from the
//! launch surface, recorded in the run manifest as
//! `consent: { irreversible: true, by: ... }`.
//!
//! Who can consent: a person at `apb run` (an interactive terminal, or the
//! `--confirm-irreversible` flag a script author writes), a person clicking
//! Run in the dashboard and confirming the dialog, and an MCP host passing
//! `acknowledge_untrusted: true` after asking the person. A start with no
//! consent (a trigger, an event bridge posting to the dashboard API, a
//! headless script without the flag) is refused before anything is written.
//! A sub-playbook inherits its parent's consent; a resume keeps it because it
//! is read from the write-once manifest, never asked again.
//!
//! This module holds only the types and texts, so it depends on nothing in
//! the engine. The engine enforces the consent in preparation
//! (`scheduler::prepare`), so no launch surface can skip it; the run gate ([`crate::gate::RunPermit::consent_refusal`]) answers the same
//! question earlier with a structured refusal the surfaces pass on.

use apb_core::schema::{Effect, Playbook};
use serde::{Deserialize, Serialize};

/// Who consented to a run's irreversible effects. In [`crate::RunOptions`]
/// it is what the launch surface grants; in the manifest it is recorded only
/// when the run needs it, so a manifest of a run without irreversible
/// effects is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConsent {
    /// The consent covers `irreversible` effects.
    pub irreversible: bool,
    /// The surface that consented: `cli` (an interactive terminal),
    /// `cli_flag` (`--confirm-irreversible`), `dashboard`, `mcp` or
    /// `mcp:<client>` (the host after asking the person).
    pub by: String,
    /// Set on a sub-playbook run: the parent run whose consent it inherited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_from: Option<String>,
}

impl RunConsent {
    /// A consent to irreversible effects given by `by`.
    pub fn irreversible(by: impl Into<String>) -> Self {
        Self {
            irreversible: true,
            by: by.into(),
            inherited_from: None,
        }
    }

    /// The MCP consent for a host session named `client`.
    pub fn mcp(client: Option<&str>) -> Self {
        Self::irreversible(match client {
            Some(c) if !c.trim().is_empty() => format!("mcp:{}", c.trim()),
            _ => "mcp".to_string(),
        })
    }
}

/// The policy code of a refusal, shared by the gate and the engine.
pub const REFUSAL_POLICY: &str = "irreversible_requires_confirmation";

/// What in `playbook` declares `irreversible`: `playbook` for the
/// playbook-level declaration, `node <id>` for a node's own. Sub-playbooks
/// are not named here (see [`crate::gate::consent_sources`]).
pub fn own_sources(playbook: &Playbook) -> Vec<String> {
    let mut out = Vec::new();
    if playbook.effects.contains(&Effect::Irreversible) {
        out.push("playbook".to_string());
    }
    for n in &playbook.nodes {
        if n.effects.contains(&Effect::Irreversible) {
            out.push(format!("node {}", n.id));
        }
    }
    out
}

/// The refusal text for a start that needs consent and has none.
pub fn refusal_detail(id: &str, sources: &[String]) -> String {
    format!(
        "playbook `{id}` has irreversible effects ({}); a run needs explicit consent: an MCP host confirms with the person and calls again with acknowledge_untrusted: true, `apb run` needs an interactive terminal or --confirm-irreversible, the dashboard asks in its Run dialog. A triggered or headless start cannot consent",
        sources.join(", ")
    )
}
