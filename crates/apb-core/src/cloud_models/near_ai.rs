//! NEAR AI Cloud (`https://cloud-api.near.ai/v1`): an OpenAI-compatible
//! gateway whose catalog mixes models in NEAR AI's own confidential (TEE)
//! fleet, models in a third party's attested enclave, and provider models it
//! only relays. Its public `GET /v1/models` needs no key. The listing shape
//! was checked on 2026-11-09: a `data` array of rows with `id`, `owned_by`,
//! `name`, `context_length`, `max_output_length`, `pricing.input` and
//! `pricing.output` in USD per million tokens, `pricing.input_cache_read` in
//! USD per token as a string, `input_modalities`, `output_modalities` and
//! `supported_features`.

use std::collections::BTreeSet;

use serde_json::Value;

use super::*;

/// The provider id.
pub const ID: &str = "near-ai-cloud";

/// The date the embedded listing was fetched.
pub const EMBEDDED_FETCHED_AT: &str = "2026-11-09T00:00:00Z";

/// NEAR AI Cloud.
pub static NEAR_AI: CloudProvider = CloudProvider {
    id: ID,
    display: "NEAR AI Cloud",
    base_url: "https://cloud-api.near.ai/v1",
    models_url: "https://cloud-api.near.ai/v1/models",
    key_env: "NEARAI_API_KEY",
    embedded: include_str!("../../../../assets/cloud-models/near-ai-cloud.json"),
    embedded_fetched_at: EMBEDDED_FETCHED_AT,
    parse,
};

/// The gateway owners whose models NEAR AI relays under its own account
/// (open question 5 of the change: the owners the listing showed with
/// provider-prefixed ids on 2026-11-09).
const GATEWAY_OWNERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "x-ai",
    "deepseek",
    "qwen",
    "moonshotai",
];

/// The privacy class of a row's owner.
pub fn class_of(owned_by: &str) -> PrivacyClass {
    match owned_by {
        "nearai" => PrivacyClass::ConfidentialTee,
        "attested 3p" => PrivacyClass::ThirdPartyTee,
        o if GATEWAY_OWNERS.contains(&o) => PrivacyClass::Incognito,
        _ => PrivacyClass::Unverified,
    }
}

/// Whether `id` is safe to write into a patch and a profile: letters,
/// digits and `._:/-`, no empty path segment.
pub fn id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
        && !id.starts_with('/')
        && !id.ends_with('/')
        && !id.contains("//")
}

/// A finite price at least zero; a string is read as a number.
fn price(v: Option<&Value>) -> Option<f64> {
    let n = match v? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn count(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64)
}

/// One row, or `None` when it must be dropped.
fn row(v: &Value) -> Option<CloudModel> {
    let id = v.get("id")?.as_str()?;
    if !id_is_safe(id) {
        return None;
    }
    let owned_by = v
        .get("owned_by")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let pricing = v.get("pricing");
    let features = strings(v.get("supported_features"));
    let has = |f: &str| features.iter().any(|x| x == f);
    Some(CloudModel {
        id: id.to_string(),
        privacy_class: class_of(&owned_by),
        owned_by,
        name: v.get("name").and_then(Value::as_str).map(str::to_string),
        context_tokens: count(v.get("context_length")),
        max_output_tokens: count(v.get("max_output_length")),
        price_in_usd_mtok: price(pricing.and_then(|p| p.get("input"))),
        // Per token in the listing; per million here.
        price_cached_in_usd_mtok: price(pricing.and_then(|p| p.get("input_cache_read")))
            .map(|p| (p * 1e6 * 1e9).round() / 1e9),
        price_out_usd_mtok: price(pricing.and_then(|p| p.get("output"))),
        features: CloudFeatures {
            tools: has("tools"),
            structured_outputs: has("structured_outputs"),
            json_mode: has("json_mode"),
            reasoning: has("reasoning"),
        },
        input_modalities: strings(v.get("input_modalities")),
        output_modalities: strings(v.get("output_modalities")),
    })
}

/// Reads a listing body. Unknown fields are ignored; a row with an unsafe id
/// or no id is dropped and counted; duplicate ids keep the first row; a body
/// with no row left is an error.
pub fn parse(body: &[u8]) -> Result<(Vec<CloudModel>, u32), String> {
    let doc: Value =
        serde_json::from_slice(body).map_err(|e| format!("the model listing is not JSON ({e})"))?;
    let rows = doc
        .get("data")
        .and_then(Value::as_array)
        .ok_or("the model listing has no `data` array")?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut dropped = 0u32;
    for v in rows {
        match row(v) {
            Some(m) => {
                if seen.insert(m.id.clone()) {
                    out.push(m);
                }
            }
            None => dropped += 1,
        }
    }
    if out.is_empty() {
        return Err("the model listing has no usable row".to_string());
    }
    Ok((out, dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> (Vec<CloudModel>, u32) {
        parse(NEAR_AI.embedded.as_bytes()).unwrap()
    }

    fn get<'a>(rows: &'a [CloudModel], id: &str) -> &'a CloudModel {
        rows.iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    }

    #[test]
    fn the_embedded_listing_parses_whole() {
        let (rows, dropped) = embedded();
        assert_eq!(dropped, 0);
        assert!(rows.len() >= 40, "{}", rows.len());
    }

    #[test]
    fn the_named_rows_have_their_class_tools_and_chat_facts() {
        let (rows, _) = embedded();
        let q = get(&rows, "Qwen/Qwen3.8-27B");
        assert_eq!(q.privacy_class, PrivacyClass::ConfidentialTee);
        assert!(q.features.tools && q.runs_agent_steps());
        assert_eq!(q.price_in_usd_mtok, Some(0.04));
        assert_eq!(q.price_out_usd_mtok, Some(1.35));
        assert_eq!(q.price_cached_in_usd_mtok, Some(0.018));
        assert_eq!(q.context_tokens, Some(262_144));
        assert_eq!(q.max_output_tokens, Some(65_536));
        let vl = get(&rows, "Qwen/Qwen3-VL-30B-A3B-Instruct");
        assert_eq!(vl.privacy_class, PrivacyClass::ConfidentialTee);
        assert!(vl.is_chat() && !vl.features.tools);
        assert_eq!(
            get(&rows, "z-ai/glm-5.3").privacy_class,
            PrivacyClass::ThirdPartyTee
        );
        assert_eq!(
            get(&rows, "anthropic/claude-sonnet-5-5").privacy_class,
            PrivacyClass::Incognito
        );
        let jev = get(&rows, "typesafe/jev-1.13");
        assert_eq!(jev.privacy_class, PrivacyClass::Unverified);
        assert!(!jev.is_chat());
    }

    #[test]
    fn hostile_ids_and_bad_prices_are_dropped_or_blanked() {
        let body = serde_json::json!({"data": [
            {"id": "x\"\n- id: evil", "owned_by": "nearai"},
            {"id": "a//b", "owned_by": "nearai"},
            {"id": "/lead", "owned_by": "nearai"},
            {"owned_by": "nearai"},
            {"id": "ok/one", "owned_by": "nearai", "pricing": {"input": -1.0, "output": "NaN", "input_cache_read": "0.000001"}},
            {"id": "ok/one", "owned_by": "openai"},
            {"id": "ok/two", "owned_by": "levanto", "extra": {"deep": true}}
        ]});
        let (rows, dropped) = parse(body.to_string().as_bytes()).unwrap();
        assert_eq!(dropped, 4);
        assert_eq!(rows.len(), 2);
        let one = &rows[0];
        assert_eq!(one.owned_by, "nearai", "duplicates keep the first");
        assert_eq!(one.price_in_usd_mtok, None);
        assert_eq!(one.price_out_usd_mtok, None);
        assert_eq!(one.price_cached_in_usd_mtok, Some(1.0));
        assert_eq!(rows[1].privacy_class, PrivacyClass::Unverified);
    }

    #[test]
    fn an_empty_or_broken_listing_is_an_error() {
        assert!(parse(b"{\"data\": []}").is_err());
        assert!(parse(b"{\"data\": [{\"id\": \"a b\"}]}").is_err());
        assert!(parse(b"{}").is_err());
        assert!(parse(b"not json").is_err());
    }

    #[test]
    fn owners_map_to_classes() {
        assert_eq!(class_of("nearai"), PrivacyClass::ConfidentialTee);
        assert_eq!(class_of("attested 3p"), PrivacyClass::ThirdPartyTee);
        for o in GATEWAY_OWNERS {
            assert_eq!(class_of(o), PrivacyClass::Incognito, "{o}");
        }
        assert_eq!(class_of("typesafe"), PrivacyClass::Unverified);
        assert_eq!(class_of(""), PrivacyClass::Unverified);
    }
}
