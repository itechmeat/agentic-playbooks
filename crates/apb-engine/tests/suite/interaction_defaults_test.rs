//! Task 7 (a): the per-agent `interaction` transport ceiling.
//!
//! The built-in agents carry the defaults from the spec (claude/claude-code
//! `live`, codex/opencode/hermes `resume`, agy `reprompt`), and a config agent
//! may override the ceiling under `agents.<id>.invocation.interaction`. These
//! are pure resolution assertions - no drive loop, no waits.

use apb_core::config::{GlobalConfig, Interaction};
use apb_engine::invocation::{builtin, resume_argv, spec_for};

#[test]
fn builtin_interaction_defaults_match_the_spec_table() {
    assert_eq!(builtin("claude").unwrap().interaction, Interaction::Live);
    assert_eq!(
        builtin("claude-code").unwrap().interaction,
        Interaction::Live
    );
    assert_eq!(builtin("codex").unwrap().interaction, Interaction::Resume);
    assert_eq!(
        builtin("opencode").unwrap().interaction,
        Interaction::Resume
    );
    assert_eq!(builtin("hermes").unwrap().interaction, Interaction::Resume);
    assert_eq!(builtin("agy").unwrap().interaction, Interaction::Reprompt);
}

#[test]
fn config_agent_overrides_the_interaction_ceiling() {
    // A config-defined invocation for `claude` sets `interaction: reprompt`,
    // which must win over the built-in `live` default.
    let yaml = "agents:\n  claude:\n    invocation:\n      argv: [-p, \"{prompt}\", --model, \"{model}\"]\n      interaction: reprompt\n";
    let global: GlobalConfig = serde_yaml_ng::from_str(yaml).unwrap();
    let spec = spec_for("claude", &global).unwrap();
    assert_eq!(
        spec.interaction,
        Interaction::Reprompt,
        "config `agents.claude.invocation.interaction` must override the built-in default"
    );
}

#[test]
fn interaction_defaults_to_reprompt_when_unspecified() {
    // A config invocation that omits `interaction` gets the serde default
    // (`reprompt`), the safe floor.
    let yaml =
        "agents:\n  custom:\n    invocation:\n      argv: [run, \"{prompt}\", -m, \"{model}\"]\n";
    let global: GlobalConfig = serde_yaml_ng::from_str(yaml).unwrap();
    let spec = spec_for("custom", &global).unwrap();
    assert_eq!(spec.interaction, Interaction::Reprompt);
}

#[test]
fn resume_forms_present_for_resumable_agents_absent_otherwise() {
    // Every built-in agent that can resume exposes a declarative resume form
    // with a `{session}` placeholder; agy and unknown agents do not.
    for id in ["claude", "claude-code", "codex", "opencode", "hermes"] {
        let form = resume_argv(id).unwrap_or_else(|| panic!("resume form for {id}"));
        assert!(
            form.iter().any(|a| a == "{session}"),
            "resume form for {id} must carry a {{session}} slot: {form:?}"
        );
        assert!(
            form.iter().any(|a| a == "{prompt}"),
            "resume form for {id} must carry a {{prompt}} slot: {form:?}"
        );
    }
    assert!(resume_argv("agy").is_none(), "agy has no resume form");
    assert!(resume_argv("unknown").is_none());
}
