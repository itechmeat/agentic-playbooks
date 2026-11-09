//! The cloud model catalogs apb knows, one provider per module (decision
//! record 0014): where a provider is reached, the name of its key variable,
//! the shape of its public model listing and the rule that turns an owner
//! into a privacy class. Nothing outside these modules knows such a fact; an
//! agent that reaches a provider declares it once on its model rules
//! (`crate::agents::ModelPolicy::cloud_routes`).
//!
//! The catalog of a provider is its parsed public listing: an embedded
//! snapshot shipped in the binary, overridden by a cache a refresh writes
//! ([`catalog`]). Only [`catalog::refresh`] touches the network.

pub mod catalog;
pub mod check;
pub mod near_ai;
pub mod privacy;

use serde::{Deserialize, Deserializer, Serialize};

/// One cloud provider apb knows: its catalog and how it is reached.
pub struct CloudProvider {
    /// The provider id (`near-ai-cloud`): config, `/api/models`,
    /// `priced_from`, `decisions.yaml` `catalog:`.
    pub id: &'static str,
    /// The name people read.
    pub display: &'static str,
    /// The OpenAI-compatible base URL.
    pub base_url: &'static str,
    /// The public, unauthenticated model listing.
    pub models_url: &'static str,
    /// The variable that holds the key. Only its name is ever written.
    pub key_env: &'static str,
    /// The listing body embedded in the binary.
    pub embedded: &'static str,
    /// The date the embedded body was fetched (RFC 3339).
    pub embedded_fetched_at: &'static str,
    /// Reads a listing body into rows; the count is of the rows refused.
    pub parse: ParseFn,
}

/// A listing parser: the rows and how many it refused, or why the body is
/// no listing.
pub type ParseFn = fn(&[u8]) -> Result<(Vec<CloudModel>, u32), String>;

static ALL: &[&CloudProvider] = &[&near_ai::NEAR_AI];

/// Every cloud provider apb knows.
pub fn all() -> &'static [&'static CloudProvider] {
    ALL
}

/// The provider `id` names.
pub fn lookup(id: &str) -> Option<&'static CloudProvider> {
    ALL.iter().copied().find(|p| p.id == id)
}

/// What a model row says it can do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudFeatures {
    pub tools: bool,
    pub structured_outputs: bool,
    pub json_mode: bool,
    pub reasoning: bool,
}

/// One model row, normalized; never holds a secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudModel {
    pub id: String,
    pub owned_by: String,
    pub privacy_class: PrivacyClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_in_usd_mtok: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_cached_in_usd_mtok: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_out_usd_mtok: Option<f64>,
    #[serde(default)]
    pub features: CloudFeatures,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
}

// Prices are finite or absent (the parser refuses anything else), so the
// row's equality is total.
impl Eq for CloudModel {}

/// The efforts a reasoning model takes (open question 2 of the change:
/// spelled the same on the wire).
const REASONING_EFFORTS: &[&str] = &["low", "medium", "high"];

impl CloudModel {
    /// Text in and text out.
    pub fn is_chat(&self) -> bool {
        self.input_modalities.iter().any(|m| m == "text")
            && self.output_modalities.iter().any(|m| m == "text")
    }

    /// What an agent step needs: a chat model with tool calling.
    pub fn runs_agent_steps(&self) -> bool {
        self.is_chat() && self.features.tools
    }

    /// The efforts a step may name: `low`, `medium`, `high` for a reasoning
    /// model, none otherwise.
    pub fn efforts(&self) -> &'static [&'static str] {
        if self.features.reasoning {
            REASONING_EFFORTS
        } else {
            &[]
        }
    }
}

/// How private a model's traffic is, from most to least private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    /// The provider's own confidential (TEE) fleet.
    ConfidentialTee,
    /// A third party's attested enclave.
    ThirdPartyTee,
    /// Relayed to a model vendor under the provider's account.
    Incognito,
    /// Nothing known about where it runs.
    Unverified,
}

impl PrivacyClass {
    pub fn as_str(self) -> &'static str {
        match self {
            PrivacyClass::ConfidentialTee => "confidential_tee",
            PrivacyClass::ThirdPartyTee => "third_party_tee",
            PrivacyClass::Incognito => "incognito",
            PrivacyClass::Unverified => "unverified",
        }
    }

    /// Reads a class name; an unknown one reads as `Unverified`.
    pub fn parse(s: &str) -> Self {
        match s {
            "confidential_tee" => PrivacyClass::ConfidentialTee,
            "third_party_tee" => PrivacyClass::ThirdPartyTee,
            "incognito" => PrivacyClass::Incognito,
            _ => PrivacyClass::Unverified,
        }
    }
}

// Lenient, like `CostBasis`: a class a newer apb wrote never fails a
// manifest or a journal.
impl<'de> Deserialize<'de> for PrivacyClass {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(v.as_str().map_or(PrivacyClass::Unverified, Self::parse))
    }
}

/// One catalog an agent reaches, under a model prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudRoute {
    /// The provider id.
    pub provider: &'static str,
    /// The model prefix a profile spells: `<prefix>/<id>[@<effort>]`.
    pub prefix: &'static str,
}

/// The catalog model a profile model names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudModelRef {
    pub provider: &'static str,
    /// The route prefix the model was spelled with.
    pub prefix: &'static str,
    /// The catalog id.
    pub id: String,
    pub effort: Option<String>,
}

impl CloudModelRef {
    /// Reads `<prefix>/<id>[@<effort>]` for each route; `None` when no route
    /// matches or the id part is empty.
    pub fn read(model: &str, routes: &[CloudRoute]) -> Option<Self> {
        let s = model.trim();
        routes.iter().find_map(|r| {
            let rest = s.strip_prefix(r.prefix)?.strip_prefix('/')?;
            let (id, effort) = match rest.rsplit_once('@') {
                Some((i, e)) => (i, Some(e.to_string())),
                None => (rest, None),
            };
            (!id.is_empty()).then(|| CloudModelRef {
                provider: r.provider,
                prefix: r.prefix,
                id: id.to_string(),
                effort,
            })
        })
    }
}

/// The catalog row of a cloud model an executor runs, as the run started:
/// every later read (the patch, the price, the class) uses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudSnapshotRef {
    pub provider: String,
    /// The route prefix the profile spelled (`nearai`).
    pub route: String,
    pub model: CloudModel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub catalog_digest: String,
    pub fetched_at: String,
}

impl Eq for CloudSnapshotRef {}

impl CloudSnapshotRef {
    /// The snapshot of `r`'s row in `snaps`, when the catalog lists it.
    pub fn of(r: &CloudModelRef, snaps: &[catalog::CatalogSnapshot]) -> Option<Self> {
        let snap = snaps.iter().find(|s| s.provider == r.provider)?;
        let model = catalog::find(&r.id, snap)?.clone();
        Some(CloudSnapshotRef {
            provider: r.provider.to_string(),
            route: r.prefix.to_string(),
            model,
            effort: r.effort.clone(),
            catalog_digest: snap.digest.clone(),
            fetched_at: snap.fetched_at.clone(),
        })
    }

    /// The provider's key variable name, when the provider is known.
    pub fn key_env(&self) -> Option<&'static str> {
        lookup(&self.provider).map(|p| p.key_env)
    }
}

/// The date part of an RFC 3339 time.
pub fn date(at: &str) -> &str {
    at.split_once('T').map_or(at, |(d, _)| d)
}

/// The shortest key value apb replaces in what an agent prints: a shorter
/// one would match ordinary text.
pub const MIN_KEY_CHARS: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn row(
        id: &str,
        inp: &[&str],
        out: &[&str],
        tools: bool,
        reasoning: bool,
    ) -> CloudModel {
        CloudModel {
            id: id.into(),
            owned_by: "nearai".into(),
            privacy_class: PrivacyClass::ConfidentialTee,
            name: None,
            context_tokens: None,
            max_output_tokens: None,
            price_in_usd_mtok: None,
            price_cached_in_usd_mtok: None,
            price_out_usd_mtok: None,
            features: CloudFeatures {
                tools,
                reasoning,
                ..Default::default()
            },
            input_modalities: inp.iter().map(|s| s.to_string()).collect(),
            output_modalities: out.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn privacy_classes_round_trip_and_unknown_reads_unverified() {
        for c in [
            PrivacyClass::ConfidentialTee,
            PrivacyClass::ThirdPartyTee,
            PrivacyClass::Incognito,
            PrivacyClass::Unverified,
        ] {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
            assert_eq!(serde_json::from_str::<PrivacyClass>(&json).unwrap(), c);
        }
        assert_eq!(
            serde_json::from_str::<PrivacyClass>("\"sealed_vault\"").unwrap(),
            PrivacyClass::Unverified
        );
        assert_eq!(
            serde_json::from_str::<PrivacyClass>("{\"a\":1}").unwrap(),
            PrivacyClass::Unverified
        );
        let mut all = [
            PrivacyClass::Unverified,
            PrivacyClass::Incognito,
            PrivacyClass::ConfidentialTee,
            PrivacyClass::ThirdPartyTee,
        ];
        all.sort();
        assert_eq!(all[0], PrivacyClass::ConfidentialTee);
        assert_eq!(all[3], PrivacyClass::Unverified);
    }

    #[test]
    fn chat_agent_steps_and_efforts_follow_the_row() {
        let agent = row("a", &["text", "image"], &["text"], true, true);
        assert!(agent.is_chat() && agent.runs_agent_steps());
        assert_eq!(agent.efforts(), ["low", "medium", "high"]);
        let vl = row("vl", &["text", "image"], &["text"], false, false);
        assert!(vl.is_chat() && !vl.runs_agent_steps());
        assert!(vl.efforts().is_empty());
        let embed = row("e", &["text"], &["embedding"], false, false);
        assert!(!embed.is_chat() && !embed.runs_agent_steps());
        let decide = row("d", &["text"], &["decisions"], true, false);
        assert!(!decide.is_chat() && !decide.runs_agent_steps());
    }

    #[test]
    fn a_model_ref_reads_prefix_id_and_effort() {
        let routes = [CloudRoute {
            provider: "near-ai-cloud",
            prefix: "nearai",
        }];
        let r = CloudModelRef::read("nearai/Qwen/Qwen3.8-27B@high", &routes).unwrap();
        assert_eq!(r.provider, "near-ai-cloud");
        assert_eq!(r.id, "Qwen/Qwen3.8-27B");
        assert_eq!(r.effort.as_deref(), Some("high"));
        assert!(CloudModelRef::read("nearai/", &routes).is_none());
        assert!(CloudModelRef::read("nearaix/a", &routes).is_none());
        assert!(CloudModelRef::read("deepseek-flash", &routes).is_none());
        assert!(CloudModelRef::read("x", &[]).is_none());
    }
}
