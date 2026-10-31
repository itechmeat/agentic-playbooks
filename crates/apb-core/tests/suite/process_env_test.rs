//! A profile's `process_env`: the allow syntax, the round trip through
//! profile.yaml, and which operator variables a plan keeps.

use std::ffi::OsString;

use apb_core::process_env::{
    AgentEnvSet, EnvPattern, ProcessEnvMode, ProcessEnvPolicy, allowed_patterns, is_truthy, plan,
};
use apb_core::profile::{PROFILE_PROCESS_ENV_INVALID, PROFILE_TIERS_INVALID, ProfileDoc};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn allowlist(list: &[&str]) -> ProcessEnvPolicy {
    ProcessEnvPolicy::from_surface(Some("allowlist"), &names(list))
        .expect("a valid allowlist")
        .expect("an explicit policy")
}

fn pats(list: &[&str]) -> Vec<EnvPattern> {
    allowlist(list).allow
}

fn parent(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn well_formed_entries_are_accepted() {
    let p = allowlist(&["GH_TOKEN", "NPM_CONFIG_*", "_PRIVATE", "SSH_AUTH_SOCK"]);
    assert!(p.problems().is_empty());
}

#[test]
fn malformed_entries_are_refused() {
    let too_many: Vec<String> = (0..65).map(|i| format!("V{i}")).collect();
    let cases: Vec<Vec<String>> = vec![
        names(&["9LIVES"]),
        names(&["HAS-DASH"]),
        names(&["*"]),
        names(&["A*"]),
        names(&["MID*DLE"]),
        names(&["TWO**"]),
        names(&["GH_TOKEN", "GH_TOKEN"]),
        too_many,
    ];
    for allow in cases {
        let refused = ProcessEnvPolicy::from_surface(Some("allowlist"), &allow);
        assert!(refused.is_err(), "{allow:?} must be refused");
    }
    assert!(EnvPattern::try_from("A*".to_string()).is_err());
    assert!(EnvPattern::try_from("AB*".to_string()).is_ok());
}

#[test]
fn allow_under_inherit_is_refused_and_an_unknown_policy_too() {
    assert!(ProcessEnvPolicy::from_surface(Some("inherit"), &names(&["GH_TOKEN"])).is_err());
    assert!(ProcessEnvPolicy::from_surface(Some("strict"), &[]).is_err());
    // Nothing given keeps the stored value; allow alone means allowlist.
    assert_eq!(ProcessEnvPolicy::from_surface(None, &[]).unwrap(), None);
    let implied = ProcessEnvPolicy::from_surface(None, &names(&["GH_TOKEN"]))
        .unwrap()
        .unwrap();
    assert_eq!(implied.policy, ProcessEnvMode::Allowlist);
}

const P: &str = "name: p\nexecutor: { agent: claude, model: sonnet }\n";

#[test]
fn an_allowlist_round_trips_and_inherit_is_stored_as_absence() {
    let doc = ProfileDoc::from_yaml(&format!(
        "{P}process_env:\n  policy: allowlist\n  allow: [GH_TOKEN, NPM_CONFIG_*]\n"
    ))
    .unwrap();
    let policy = doc.process_env.clone().expect("parsed");
    assert!(policy.is_allowlist());
    let yaml = yaml_serde::to_string(&doc).unwrap();
    assert_eq!(ProfileDoc::from_yaml(&yaml).unwrap(), doc);
    assert!(ProfileDoc::from_yaml(P).unwrap().process_env.is_none());
    let inherit = ProcessEnvPolicy::from_surface(Some("inherit"), &[]).unwrap();
    assert_eq!(ProcessEnvPolicy::stored(inherit), None);
    // An unknown key inside the block is refused, like the rest of the profile.
    assert!(
        ProfileDoc::from_yaml(&format!(
            "{P}process_env: {{ policy: allowlist, deny: [X] }}\n"
        ))
        .is_err()
    );
}

#[test]
fn problems_report_both_codes() {
    let doc = ProfileDoc::from_yaml(&format!(
        "{P}tiers:\n  light: {{ for: \"\" }}\nprocess_env: {{ policy: inherit, allow: [\"*\"] }}\n"
    ))
    .unwrap();
    let codes: Vec<&str> = doc.problems().iter().map(|p| p.code).collect();
    assert!(codes.contains(&PROFILE_TIERS_INVALID), "{codes:?}");
    assert!(codes.contains(&PROFILE_PROCESS_ENV_INVALID), "{codes:?}");
    let refusal = doc.check().unwrap_err();
    assert!(refusal.contains("profile_process_env_invalid"), "{refusal}");
    assert!(ProfileDoc::from_yaml(P).unwrap().check().is_ok());
}

#[test]
fn a_plan_keeps_base_agent_and_allow_names_and_withholds_the_rest() {
    let allow = pats(&["GH_TOKEN", "NPM_CONFIG_*", "NPM_TOKEN"]);
    let allowed = allowed_for("claude", "sonnet", &allow, |_| false, &[]);
    let p = plan(
        parent(&[
            "PATH",
            "HOME",
            "LC_ALL",
            "LC_DEPLOY_TOKEN",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            "GH_TOKEN",
            "NPM_CONFIG_REGISTRY",
            "AWS_PROFILE",
            "OPERATOR_SECRET",
            "SLACK_TOKEN",
        ]),
        &allowed,
        &names(&["SLACK_TOKEN", "GH_TOKEN"]),
        &allow,
    );
    assert_eq!(
        p.keep,
        names(&[
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            "HOME",
            "LC_ALL",
            "NPM_CONFIG_REGISTRY",
            "PATH",
        ])
    );
    // The connector scrub wins over `allow`.
    assert_eq!(
        p.withheld,
        names(&[
            "AWS_PROFILE",
            "GH_TOKEN",
            "LC_DEPLOY_TOKEN",
            "OPERATOR_SECRET",
            "SLACK_TOKEN"
        ])
    );
    // Only the profile's own exact names are reported missing.
    assert_eq!(p.absent, names(&["NPM_TOKEN"]));
    assert!(p.keeps("PATH") && !p.keeps("GH_TOKEN"));
}

#[test]
fn claude_cloud_credentials_follow_their_trigger() {
    let env = parent(&["CLAUDE_CODE_USE_BEDROCK", "AWS_PROFILE", "AZURE_TENANT"]);
    let off = allowed_for("claude", "m", &[], |_| false, &[]);
    assert!(!plan(env.clone(), &off, &[], &[]).keeps("AWS_PROFILE"));
    let on = allowed_for("claude", "m", &[], |n| n == "CLAUDE_CODE_USE_BEDROCK", &[]);
    let p = plan(env, &on, &[], &[]);
    assert!(p.keeps("AWS_PROFILE"));
    assert!(!p.keeps("AZURE_TENANT"));
}

#[test]
fn a_trigger_counts_only_with_a_truthy_value() {
    for on in ["1", "true", "TRUE", "yes", "On"] {
        assert!(is_truthy(on), "{on}");
    }
    for off in ["0", "false", "FALSE", "no", "off", ""] {
        assert!(!is_truthy(off), "{off}");
    }
}

/// A malformed `allow` entry that reached the engine without the run-start
/// check (a hand-edited manifest) is dropped: `*` alone must not turn the
/// allowlist into inherit-everything.
#[test]
fn a_malformed_allow_entry_is_dropped_not_trusted() {
    let allow: Vec<EnvPattern> = yaml_serde::from_str("[\"*\", GH_TOKEN]").unwrap();
    let allowed = allowed_for("claude", "m", &allow, |_| false, &[]);
    let p = plan(parent(&["OPERATOR_SECRET", "GH_TOKEN"]), &allowed, &[], &[]);
    assert_eq!(p.keep, names(&["GH_TOKEN"]));
}

#[test]
fn opencode_keeps_the_key_of_its_model_provider_only() {
    let env = parent(&["OPENROUTER_API_KEY", "ZHIPU_API_KEY", "OPENCODE_CONFIG"]);
    let allowed = allowed_for("opencode", "openrouter/some-model", &[], |_| false, &[]);
    let p = plan(env.clone(), &allowed, &[], &[]);
    assert!(p.keeps("OPENROUTER_API_KEY"));
    assert!(p.keeps("OPENCODE_CONFIG"));
    assert!(!p.keeps("ZHIPU_API_KEY"));
    let zai = allowed_for("opencode", "zai-coding-plan/glm", &[], |_| false, &[]);
    assert!(plan(env, &zai, &[], &[]).keeps("ZHIPU_API_KEY"));
    assert!(apb_core::agents::opencode::provider_keys("bare-model").is_empty());
}

#[test]
fn exact_derived_names_are_kept_and_a_custom_agent_gets_only_base_and_allow() {
    let env = parent(&["CUSTOM_PROVIDER_KEY", "OPENAI_API_KEY", "PATH"]);
    let codex = allowed_for(
        "codex",
        "m",
        &[],
        |_| false,
        &names(&["CUSTOM_PROVIDER_KEY", "AWS_*"]),
    );
    let p = plan(env.clone(), &codex, &[], &[]);
    assert!(p.keeps("CUSTOM_PROVIDER_KEY") && p.keeps("OPENAI_API_KEY"));
    // A derived entry is an exact name only: a `PREFIX*` from a config file
    // never widens the set.
    let aws = parent(&["AWS_SECRET_ACCESS_KEY"]);
    assert!(!plan(aws, &codex, &[], &[]).keeps("AWS_SECRET_ACCESS_KEY"));
    let custom = allowed_for("my-agent", "m", &[], |_| false, &[]);
    let p = plan(env, &custom, &[], &[]);
    assert_eq!(p.keep, names(&["PATH"]));
}

/// [`allowed_patterns`] for `agent` running `model`, with the agent's set and
/// model variables from its module, as the engine assembles them.
fn allowed_for(
    agent: &str,
    model: &str,
    allow: &[EnvPattern],
    is_on: impl Fn(&str) -> bool,
    derived: &[String],
) -> Vec<EnvPattern> {
    let setup = apb_core::agents::lookup(agent).map(|a| a.setup());
    allowed_patterns(
        setup.map_or(AgentEnvSet::NONE, |s| s.env_set()),
        setup.map_or(&[][..], |s| s.model_env(model)),
        allow,
        is_on,
        derived,
    )
}
