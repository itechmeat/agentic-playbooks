//! The `irreversible` consent gate (0.24.0): a run whose effective effects
//! (the playbook's own, its nodes' and every sub-playbook's, recursively)
//! include `irreversible` starts only with an explicit consent from the
//! launch surface, recorded in the run manifest as
//! `consent: { irreversible: true, by: ... }`.
//!
//! Who can consent: a person at `apb run` (the `[y/N]` question at an
//! interactive terminal, or the `--confirm-irreversible` flag a script author
//! writes), a person clicking Run in the dashboard and confirming the dialog,
//! and an MCP host passing `confirm_irreversible` after asking the person. A
//! start with no consent (a trigger, an event bridge posting to the dashboard
//! API without the field, a headless script without the flag) is refused
//! before anything is written.
//!
//! A consent is bound to what the person was shown: every refusal carries a
//! [`consent_nonce`] (a hash of the trust digest and the sorted sources), and
//! a confirmation that echoes a nonce is accepted only while it still matches.
//! A bare `true` is still accepted for one release, with a deprecation note.
//!
//! A sub-playbook inherits its parent's consent. A resume keeps the recorded
//! consent of a run apb created on this machine; a run without one (started
//! by an older apb) asks once, and a supervisor patch may not add sources the
//! consent did not cover.
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
    /// What the consent covered when the run started (or resumed): the
    /// run's own consent sources. A supervisor patch that would add a source
    /// outside this list is rejected. Empty in [`crate::RunOptions`]; the
    /// engine fills it in when it records the consent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

impl RunConsent {
    /// A consent to irreversible effects given by `by`.
    pub fn irreversible(by: impl Into<String>) -> Self {
        Self {
            irreversible: true,
            by: by.into(),
            inherited_from: None,
            sources: Vec::new(),
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
        "playbook `{id}` has irreversible effects ({}); a run needs explicit consent: an MCP host confirms with the person and calls again with confirm_irreversible set to the consent_nonce, `apb run` asks at an interactive terminal or takes --confirm-irreversible=<consent_nonce>, the dashboard asks in its Run dialog. A triggered or headless start cannot consent",
        sources.join(", ")
    )
}

/// The consent nonce for a tree with trust digest `digest` and consent
/// `sources`: `consent-` and 32 hex digits of a sha256 over the digest and
/// the sorted sources. It is not a secret; it binds a confirmation to what
/// the refusal showed, so a playbook that changed in between (another
/// digest, another source) is refused again.
pub fn consent_nonce(digest: &str, sources: &[String]) -> String {
    let mut sorted: Vec<&str> = sources.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut input = String::from(digest);
    for s in sorted {
        input.push('\n');
        input.push_str(s);
    }
    let hex = apb_core::content::sha256_hex(input.as_bytes());
    let hex = hex.strip_prefix("sha256:").unwrap_or(&hex);
    format!("consent-{}", &hex[..32])
}

/// What a launch surface passes as its confirmation of irreversible effects:
/// the `consent_nonce` of the refusal it showed the person, or a bare `true`
/// (accepted for one release, with a deprecation note). `false` is no
/// confirmation. Deserializes from a JSON boolean or string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Confirmation {
    /// `true` or `false`.
    Flag(bool),
    /// The `consent_nonce` of the refusal the person saw.
    Nonce(String),
}

impl Confirmation {
    /// A confirmation from a CLI value: `true` (or an empty value) is the
    /// bare flag, anything else a nonce.
    pub fn from_cli(value: &str) -> Self {
        match value.trim() {
            "" | "true" => Self::Flag(true),
            "false" => Self::Flag(false),
            v => Self::Nonce(v.to_string()),
        }
    }

    /// Whether this is any confirmation at all (a nonce or `true`).
    pub fn is_given(&self) -> bool {
        !matches!(self, Self::Flag(false))
    }
}

/// The deprecation note for a confirmation that is a bare `true` rather than
/// the refusal's nonce.
pub const BARE_CONFIRMATION_NOTE: &str = "a bare `true` confirmation of irreversible effects is deprecated and will be refused in a later release: pass the refusal's consent_nonce, which binds the consent to what the person was shown";

/// The deprecation note for an MCP host that consented through
/// `acknowledge_untrusted` alone.
pub const ACKNOWLEDGE_AS_CONSENT_NOTE: &str = "acknowledge_untrusted: true was taken as consent to irreversible effects for one release only; pass confirm_irreversible (the refusal's consent_nonce) after asking the person, and keep acknowledge_untrusted for trust";

/// A confirmation checked against a tree that needs consent: accepted, with
/// an optional deprecation note, or the refusal (a nonce that no longer
/// matches, or none at all).
pub fn check_confirmation(
    playbook_id: &str,
    digest: &str,
    sources: &[String],
    confirmation: Option<&Confirmation>,
) -> Result<Option<&'static str>, serde_json::Value> {
    let nonce = consent_nonce(digest, sources);
    let refusal = |reason: Option<&str>| {
        let mut v = serde_json::json!({
            "policy": REFUSAL_POLICY,
            "effects": ["irreversible"],
            "sources": sources,
            "consent_nonce": nonce,
            "detail": refusal_detail(playbook_id, sources),
        });
        if let Some(r) = reason {
            v["reason"] = serde_json::json!(r);
        }
        v
    };
    match confirmation {
        Some(Confirmation::Nonce(n)) if n.trim() == nonce => Ok(None),
        Some(Confirmation::Nonce(_)) => Err(refusal(Some(
            "consent_nonce_mismatch: the playbook or its irreversible steps changed since the person was asked; show the sources again and confirm with the new consent_nonce",
        ))),
        Some(Confirmation::Flag(true)) => Ok(Some(BARE_CONFIRMATION_NOTE)),
        Some(Confirmation::Flag(false)) | None => Err(refusal(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nonce_moves_with_the_digest_and_the_sources_but_not_their_order() {
        let a = consent_nonce("d1", &["node b".into(), "node a".into()]);
        assert_eq!(a, consent_nonce("d1", &["node a".into(), "node b".into()]));
        assert_ne!(a, consent_nonce("d2", &["node a".into(), "node b".into()]));
        assert_ne!(a, consent_nonce("d1", &["node a".into()]));
        assert!(a.starts_with("consent-") && a.len() == "consent-".len() + 32);
    }

    #[test]
    fn a_confirmation_is_accepted_only_while_its_nonce_matches() {
        let sources = vec!["node pr".to_string()];
        let nonce = consent_nonce("d", &sources);
        let check = |c: Option<Confirmation>| check_confirmation("p", "d", &sources, c.as_ref());
        assert_eq!(check(Some(Confirmation::Nonce(nonce.clone()))), Ok(None));
        assert_eq!(
            check(Some(Confirmation::Flag(true))),
            Ok(Some(BARE_CONFIRMATION_NOTE))
        );
        for refused in [
            None,
            Some(Confirmation::Flag(false)),
            Some(Confirmation::Nonce("consent-other".into())),
        ] {
            let mismatch = matches!(refused, Some(Confirmation::Nonce(_)));
            let r = check(refused).expect_err("refused");
            assert_eq!(r["policy"], REFUSAL_POLICY);
            assert_eq!(r["consent_nonce"], nonce);
            assert_eq!(r["sources"], serde_json::json!(["node pr"]));
            assert_eq!(r.get("reason").is_some(), mismatch);
        }
    }

    #[test]
    fn a_confirmation_reads_from_json_and_from_the_cli() {
        let t: Confirmation = serde_json::from_str("true").unwrap();
        assert_eq!(t, Confirmation::Flag(true));
        let n: Confirmation = serde_json::from_str("\"consent-x\"").unwrap();
        assert_eq!(n, Confirmation::Nonce("consent-x".into()));
        assert_eq!(Confirmation::from_cli(""), Confirmation::Flag(true));
        assert_eq!(Confirmation::from_cli("true"), Confirmation::Flag(true));
        assert_eq!(
            Confirmation::from_cli("consent-x"),
            Confirmation::Nonce("consent-x".into())
        );
        assert!(!Confirmation::Flag(false).is_given());
    }
}
