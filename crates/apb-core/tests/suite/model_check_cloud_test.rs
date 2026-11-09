//! A model on a cloud route is judged by its catalog, after the person's
//! `model_policy`, and never by the agent's closed list or detection.

use apb_core::cloud_models::{catalog, near_ai::NEAR_AI};
use apb_core::config::ModelRule;
use apb_core::model_check::{ModelContext, ModelIssue, check};

fn cx(policy: Vec<ModelRule>) -> ModelContext {
    ModelContext {
        agents: Vec::new(),
        table: apb_core::models_table::builtin(),
        zcode_family: apb_core::zcode::DEFAULT_FAMILY.to_string(),
        policy,
        cloud: vec![catalog::embedded(&NEAR_AI)],
    }
}

fn code(model: &str, c: &ModelContext) -> Option<(&'static str, bool)> {
    check("dsh", model, c).map(|i| (i.code(), i.is_blocking()))
}

#[test]
fn the_catalog_judges_a_near_model() {
    let c = cx(Vec::new());
    assert_eq!(
        code("nearai/Qwen/Qwen3.8-72B", &c),
        Some(("cloud_model_unknown", false))
    );
    assert_eq!(
        code("nearai/Qwen/Qwen3-VL-30B-A3B-Instruct", &c),
        Some(("cloud_model_no_tools", true))
    );
    assert_eq!(
        code("nearai/typesafe/jev-1.13", &c),
        Some(("cloud_model_not_chat", true))
    );
    assert_eq!(
        code("nearai/deepseek/deepseek-v3.2@high", &c),
        Some(("cloud_effort_unsupported", true))
    );
    // No installed dsh in the context: a good id is still fine.
    assert_eq!(check("dsh", "nearai/Qwen/Qwen3.8-27B@high", &c), None);
    let issue = check("dsh", "nearai/Qwen/Qwen3.8-72B", &c).unwrap();
    assert!(matches!(issue, ModelIssue::Cloud(_)));
    assert!(
        issue
            .describe("dsh", "nearai/Qwen/Qwen3.8-72B")
            .contains("apb models --refresh")
    );
}

#[test]
fn the_persons_policy_keeps_the_last_word() {
    let c = cx(vec![ModelRule {
        agent: "dsh".into(),
        when: None,
        allow: vec!["deepseek-*".into()],
        reason: None,
    }]);
    assert_eq!(
        code("nearai/Qwen/Qwen3.8-27B", &c),
        Some(("model_policy_violation", true))
    );
}
