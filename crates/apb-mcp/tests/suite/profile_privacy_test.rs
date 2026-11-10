//! The resolved privacy chain of a profile (`profile_get`'s `resolved`
//! block) and the profile write's cloud findings.

use apb_core::cloud_models::{catalog, near_ai::NEAR_AI};
use apb_core::profile::ProfileDoc;
use apb_mcp::profile_tools::resolved_chain;

fn doc(yaml: &str) -> ProfileDoc {
    ProfileDoc::from_yaml(yaml).unwrap()
}

fn snaps() -> Vec<catalog::CatalogSnapshot> {
    vec![catalog::embedded(&NEAR_AI)]
}

#[test]
fn an_all_nearai_chain_is_uniform_confidential() {
    let r = resolved_chain(
        &doc(
            "name: p\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\n  fallbacks:\n    - { agent: dsh, model: nearai/z-ai/glm-5.3-flash }\n    - { agent: dsh, model: nearai/Qwen/Qwen3.6-35B-A3B-FP8 }\n",
        ),
        &snaps(),
    );
    assert_eq!(r["privacy"]["uniform"], true);
    assert_eq!(r["privacy"]["least_private"], "confidential_tee");
    assert!(r["privacy"]["warnings"].as_array().unwrap().is_empty());
    assert_eq!(r["chain"].as_array().unwrap().len(), 3);
    assert_eq!(r["chain"][0]["cloud"]["price_out_usd_mtok"], 1.35);
}

#[test]
fn an_incognito_fallback_is_not_uniform_and_warns() {
    let r = resolved_chain(
        &doc(
            "name: p\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\n  fallbacks:\n    - { agent: dsh, model: nearai/anthropic/claude-sonnet-5-5 }\n",
        ),
        &snaps(),
    );
    assert_eq!(r["privacy"]["uniform"], false);
    assert_eq!(r["privacy"]["least_private"], "incognito");
    assert_eq!(r["privacy"]["warnings"][0], "profile_privacy_class_changes");
    assert_eq!(r["chain"][1]["role"], "fallback");
    assert_eq!(r["chain"][1]["index"], 0);
}

#[test]
fn a_claude_tier_is_unclassified_and_named() {
    let r = resolved_chain(
        &doc(
            "name: p\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\ntiers:\n  quick: { agent: claude, model: claude-haiku-5-5, for: \"Small lookups.\" }\n",
        ),
        &snaps(),
    );
    assert_eq!(r["chain"][1]["role"], "tier");
    assert_eq!(r["chain"][1]["name"], "quick");
    assert!(r["chain"][1]["cloud"].is_null());
    assert_eq!(r["privacy"]["classes"][1], serde_json::Value::Null);
    assert!(r["privacy"]["least_private"].is_null());
    assert_eq!(r["privacy"]["warnings"][0], "profile_privacy_class_changes");
}
