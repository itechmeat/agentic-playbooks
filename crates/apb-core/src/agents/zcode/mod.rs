//! zcode (Z.ai's ZCode headless CLI). Vendor-tied: every plan serves Zhipu's
//! GLM models. The desktop deploys the headless CLI into the home directory
//! and never puts it on PATH, so a PATH `zcode` (a user-made symlink) wins
//! and the known deploy location is the fallback.
//!
//! ZCode deploys its headless CLI into the user's home at
//! `~/.zcode/server/agents/glm/zcode-agent` (a `sh` wrapper that execs the
//! bundled node on `zcode.cjs`); it is not on PATH. Facts below were verified
//! against zcode-agent 0.16.9 (ZCode desktop 3.14.3):
//!
//! - `-p <prompt>` runs one headless turn; `--mode build|edit|plan|yolo` picks
//!   the permission mode and DEFAULTS TO `yolo` for `-p`, so apb always passes
//!   one explicitly. In `-p` mode every approval request is denied (there is no
//!   interactive gate), so `build` is the safe non-autonomous mode.
//! - `--json` prints ONE pretty-printed JSON object at the end:
//!   `{sessionId, traceId, turnId, response, usage, eventCount, projection}`.
//!   `--resume sess_...` re-enters a session. Failures exit 1 with
//!   `Error: <message> (traceId: ...)` on stderr and nothing on stdout.
//! - There is no `--model` flag. The model comes from the personal provider
//!   config's `defaultModelSelection` (`{providerId, modelId, options:
//!   {reasoningLevel}}`), read from the file named by
//!   `ZCODE_PERSONAL_PROVIDER_CONFIG_FILE`. The standalone CLI also needs
//!   `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`, because the WSL deployment does not
//!   ship the `provider/zcode-builtin.json` it looks for next to `zcode.cjs`;
//!   the desktop materializes that file under `~/.zcode/v2/runtime/provider/`.
//! - The same model is served by several plans ("Z.ai Individual Coding Plan",
//!   "Start Plan", ...), each a separate provider id such as
//!   `account:zai-individual-coding-plan`. apb's canonical zcode model string
//!   is the BARE model id (e.g. `GLM-5.3`), which resolves to the paid
//!   Individual plan; the older plan-qualified spelling
//!   (`zai-individual/GLM-5.3`) is still accepted, see [`parse_model`].
//!   Only the models on apb's allowlist ([`ALLOWED_MODELS`]) are listed and
//!   accepted on ZCode's own plans.
//! - The standalone CLI only sees a plan after `zcode-agent login` stored an
//!   `account-provider:<providerId>:identity` credential next to the plan's
//!   api key in `~/.zcode/v2/credentials.json`. Only the KEY NAMES of that file
//!   are ever looked at here, never a value. The desktop's own login does not
//!   write that identity key, and in 0.16.9 the standalone account source
//!   covers ONLY the `individual-coding-plan` providers: the Start (free),
//!   Team and Idle plans the desktop offers are not reachable headless.
//! - A selection naming a plan the CLI cannot use is NOT an error in ZCode: it
//!   silently falls back to the first usable plan. That would spend the paid
//!   plan for a step meant to run on another one, so apb refuses such a
//!   selection before spawning ([`check_plan_usable`]).

/// Where ZCode deploys the headless CLI wrapper, relative to `$HOME`, as a
/// literal: the one spelling of [`provider::HOME_REL_BIN`] and of every
/// message that names the path.
macro_rules! home_rel_bin {
    () => {
        ".zcode/server/agents/glm/zcode-agent"
    };
}

pub mod output;
pub mod plan;
pub mod provider;

use std::path::{Path, PathBuf};

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
pub use super::types::ZcodeMode;
use super::{
    Agent, AgentCategory, AgentEnvSet, AuthHint, AuthKind, Capabilities, ClosedList, Descriptor,
    Forms, FreshForm, HostNotes, HostSetup, ModelPolicy, ScopedEnvError, Verified,
};
pub use output::{response_text, session_id, trace_id};
pub use plan::*;
pub use provider::*;

/// The agent id.
pub const ID: &str = plan::AGENT_ID;

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &[provider::PATH_BIN],
    version_args: &["--version"],
    help_args: &["--help"],
    home_paths: &[provider::HOME_REL_BIN],
    category: AgentCategory::Vendor,
    capabilities: Capabilities {
        desktop_history: true,
        ..Capabilities::NONE
    },
    verified: Some(Verified {
        version: "0.16.9",
        on: "2026-09-25",
    }),
};

/// The zcode agent.
pub struct Zcode;

impl Agent for Zcode {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn models(&self) -> &'static dyn super::ModelPolicy {
        &ZcodeModels
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &ZcodeSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &output::ZcodeOutput
    }

    // Verified against zcode-agent 0.16.9. `--mode` DEFAULTS TO `yolo` for
    // `-p`, so the base form pins `build` (every approval request is denied
    // in `-p` mode, there is no interactive gate) and only an authorized
    // effectful run gets `yolo`: the autonomy flags come later in argv and
    // zcode's option parser keeps the last value. `--json` prints one JSON
    // object with `sessionId` and `response`. There is no `--model` flag and
    // no system-prompt flag: the model travels through a run-scoped provider
    // config and the SOUL as a prompt prefix.
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &["-p", "{prompt}", "--json", "--mode", "build"],
                SoulDelivery::Prefix,
                None,
                &["--mode", "yolo"],
                Interaction::Resume,
            ),
            // zcode re-enters a persisted session via `--resume sess_...`;
            // the mode is pinned again (a resumed `-p` turn would otherwise
            // run as yolo).
            resume: Some(&[
                "--resume",
                "{session}",
                "-p",
                "{prompt}",
                "--json",
                "--mode",
                "build",
            ]),
            fresh: FreshForm::Printed,
            profile_mode: Some(mode_args),
        })
    }
}

/// The autonomy flags for a profile's `zcode_mode`: the `--mode` that follows
/// the base form's `build` (zcode keeps the last value). The launch form's
/// `yolo` is [`ZcodeMode::Yolo`].
fn mode_args(mode: ZcodeMode) -> Vec<String> {
    vec!["--mode".to_string(), mode.as_str().to_string()]
}

/// zcode's host facts and needs.
struct ZcodeSetup;

impl HostSetup for ZcodeSetup {
    // Key names only: a plan identity key means `zcode-agent login` ran; the
    // api key alone (synced by the desktop) is not enough for the standalone
    // CLI.
    fn auth_hint(&self, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        let kind = if provider::logged_in_providers(home).is_empty() {
            AuthKind::None
        } else {
            AuthKind::Oauth
        };
        (Some(AuthHint { kind }), None)
    }

    // The plan annotation and the login note are external facts; the
    // allowlist itself comes from `crate::agent_catalog`.
    fn host_notes(&self, home: Option<&Path>, installed: bool) -> HostNotes {
        if !installed {
            return HostNotes::default();
        }
        let family = home
            .map(provider::account_family)
            .unwrap_or_else(|| plan::DEFAULT_FAMILY.to_string());
        let mut notes = Vec::new();
        if let Some(home) = home
            && provider::logged_in_providers(home).is_empty()
        {
            notes.push(
                "standalone CLI not logged in: run `zcode-agent login` (the desktop's \
                 login alone does not cover headless runs)"
                    .to_string(),
            );
        }
        HostNotes {
            providers: Some(vec![format!("{family}-individual")]),
            notes,
        }
    }

    // The credentials hold the login; the settings name the account family
    // of the plan annotation.
    fn memo_files(&self, home: &Path) -> Vec<PathBuf> {
        vec![
            home.join(provider::HOME_REL_CREDENTIALS),
            home.join(provider::HOME_REL_SETTINGS),
        ]
    }

    // The headless CLI needs its own `zcode-agent login`: the desktop app's
    // login does not give it a plan identity, and every run then fails with
    // "Select a model before continuing".
    fn login_warning(&self, auth: Option<&AuthHint>) -> Option<&'static str> {
        auth.is_some_and(|a| a.kind == AuthKind::None)
            .then_some(concat!(
                "standalone CLI not logged in to any Z.ai plan: run `~/",
                home_rel_bin!(),
                " login` once"
            ))
    }

    fn default_program(&self) -> Option<String> {
        Some(provider::default_program())
    }

    // The zcode desktop app on PATH is not the headless CLI, and probing it
    // would boot the desktop: detection falls through to the CLI's home
    // location.
    fn rejects_binary(&self, path: &Path) -> bool {
        provider::is_desktop_app(path)
    }

    // apb sets the two provider-config variables itself after the plan.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["ZCODE_*"],
            when: &[],
        }
    }

    // zcode has no `--model` flag: the run-scoped provider config carrying
    // the model selection IS its model passing.
    fn scoped_env(
        &self,
        model: &str,
        scoped_dir: Option<&Path>,
    ) -> Result<Vec<(String, PathBuf)>, ScopedEnvError> {
        provider::spawn_env(model, scoped_dir).map_err(|source| ScopedEnvError {
            what: format!("{ID} provider config"),
            source,
        })
    }
}

/// zcode's model rules: apb's allowlist on the paid Individual plan, and one
/// billing account per Z.ai plan.
struct ZcodeModels;

impl ModelPolicy for ZcodeModels {
    fn vendor(&self) -> Option<&'static str> {
        Some("zhipu")
    }

    // A Rust constant because the spawn path enforces it.
    fn closed_list(&self) -> ClosedList {
        ClosedList::Fixed(plan::ALLOWED_MODELS)
    }

    // The canonical bare model without its effort suffix
    // (`zai-individual/glm-5.3-flash@high` is `GLM-5.3-Flash`); a model off
    // the allowlist is refused.
    fn list_id(&self, model: &str, account_family: &str) -> Result<String, String> {
        plan::check_model_allowed(model, account_family)?;
        let canonical = plan::canonical_model(model, account_family);
        Ok(canonical
            .split_once('@')
            .map_or(canonical.as_str(), |(m, _)| m)
            .to_string())
    }

    // Stored in the canonical bare spelling (the legacy `zai-individual/`
    // prefix is accepted and dropped), refused outside apb's allowlist.
    fn canonical_model(&self, model: &str) -> Result<String, String> {
        let family = account_family_here();
        plan::check_model_allowed(model, &family)?;
        Ok(plan::canonical_model(model, &family))
    }

    // Each Z.ai plan (paid Individual, free Start, ...) is its own account
    // with its own quota, even for the same model, so the key carries the
    // plan and a paid `GLM-5.3` that ran out can still fall back to the free
    // plan's `GLM-5.3`.
    fn billing_account(&self, model: &str) -> Option<String> {
        plan::parse_model(model, &account_family_here(), &[]).map(|sel| sel.account_key())
    }
}

/// The account family ZCode's settings name on this machine.
pub fn account_family_here() -> String {
    provider::home_dir()
        .map(|h| provider::account_family(&h))
        .unwrap_or_else(|| plan::DEFAULT_FAMILY.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn known() -> Vec<String> {
        ["GLM-5.3", "GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn plan_aliases_round_trip() {
        for (pid, alias) in [
            ("account:zai-individual-coding-plan", "zai-individual"),
            ("account:zai-team-coding-plan", "zai-team"),
            ("account:zai-start-plan", "zai-start"),
            ("account:zai-offpeak-idle-plan", "zai-idle"),
            ("account:bigmodel-start-plan", "bigmodel-start"),
        ] {
            assert_eq!(plan_alias(pid).as_deref(), Some(alias));
            assert_eq!(resolve_plan(alias, "zai"), pid);
        }
        assert_eq!(plan_alias("my-openai"), None);
    }

    #[test]
    fn resolve_plan_accepts_every_spelling() {
        let want = "account:zai-start-plan";
        for s in [
            "zai-start",
            "zai-start-plan",
            "account:zai-start-plan",
            "start",
            "start-plan",
            "ZAI-Start",
        ] {
            assert_eq!(resolve_plan(s, "zai"), want, "{s}");
        }
        // A bare kind takes the account family.
        assert_eq!(
            resolve_plan("individual", "bigmodel"),
            "account:bigmodel-individual-coding-plan"
        );
        // Unknown providers pass through verbatim.
        assert_eq!(resolve_plan("my-provider", "zai"), "my-provider");
    }

    #[test]
    fn qualified_models_keep_their_plan() {
        let paid = parse_model("zai-individual/glm-5.3", "zai", &known()).unwrap();
        assert_eq!(paid.provider_id, "account:zai-individual-coding-plan");
        assert_eq!(paid.model_id, "GLM-5.3");
        assert_eq!(paid.effort, None);
        let free = parse_model("zai-start/GLM-5.3@Max", "zai", &known()).unwrap();
        assert_eq!(free.provider_id, "account:zai-start-plan");
        assert_eq!(free.model_id, "GLM-5.3");
        assert_eq!(free.effort.as_deref(), Some("max"));
        assert_ne!(paid.account_key(), free.account_key());
    }

    #[test]
    fn unqualified_model_resolves_to_the_paid_individual_plan() {
        let s = parse_model("GLM-5.3-Flash", "zai", &known()).unwrap();
        assert_eq!(s.provider_id, "account:zai-individual-coding-plan");
        assert_eq!(s.model_id, "GLM-5.3-Flash");
        let b = parse_model("glm-5.2@high", "bigmodel", &known()).unwrap();
        assert_eq!(b.provider_id, "account:bigmodel-individual-coding-plan");
        assert_eq!(b.model_id, "GLM-5.2");
        assert_eq!(b.effort.as_deref(), Some("high"));
    }

    #[test]
    fn empty_or_planless_models_are_none() {
        assert_eq!(parse_model("", "zai", &known()), None);
        assert_eq!(parse_model("  ", "zai", &known()), None);
        assert_eq!(parse_model("zai-start/", "zai", &known()), None);
    }

    #[test]
    fn unknown_model_ids_pass_through() {
        let s = parse_model("my-provider/some-model", "zai", &known()).unwrap();
        assert_eq!(s.provider_id, "my-provider");
        assert_eq!(s.model_id, "some-model");
    }

    /// Allowlisted ids canonicalize to the allowlist spelling even when the
    /// built-in config is unknown to the caller, and case is rewritten.
    #[test]
    fn allowlisted_models_canonicalize_without_a_config() {
        for raw in ["GLM-5.3", "glm-5.3", "GLM-5.3-flash"] {
            let s = parse_model(raw, "zai", &[]).unwrap();
            assert_eq!(s.provider_id, "account:zai-individual-coding-plan");
            assert!(
                is_allowed_model(&s.model_id),
                "{raw} must canonicalize onto the allowlist, got {}",
                s.model_id
            );
        }
        let with_effort = parse_model("glm-5.3-flash@LOW", "zai", &[]).unwrap();
        assert_eq!(with_effort.model_id, "GLM-5.3-Flash");
        assert_eq!(with_effort.effort.as_deref(), Some("low"));
    }

    /// The legacy `plan/model` spelling still resolves to the same selection
    /// as the bare name - it is accepted but never emitted.
    #[test]
    fn legacy_plan_qualified_spelling_parses_like_the_bare_name() {
        for raw in ["zai-individual/GLM-5.3", "GLM-5.3", "glm-5.3"] {
            let s = parse_model(raw, "zai", &known()).unwrap();
            assert_eq!(s.provider_id, "account:zai-individual-coding-plan", "{raw}");
            assert_eq!(s.model_id, "GLM-5.3", "{raw}");
        }
    }

    /// The allowlist governs model strings on ZCode's own plans; a custom
    /// provider and an empty string pass.
    #[test]
    fn check_model_allowed_enforces_the_allowlist() {
        assert!(check_model_allowed("GLM-5.3", "zai").is_ok());
        assert!(check_model_allowed("GLM-5.3-Flash@low", "zai").is_ok());
        assert!(check_model_allowed("zai-individual/glm-5.3-flash@high", "zai").is_ok());
        assert!(check_model_allowed("", "zai").is_ok());
        assert!(check_model_allowed("my-provider/anything", "zai").is_ok());
        let err = check_model_allowed("GLM-5.2", "zai").unwrap_err();
        assert!(err.contains("GLM-5.2"), "{err}");
        assert!(err.contains("GLM-5.3-Flash"), "{err}");
        assert!(err.contains("Individual plan"), "{err}");
        assert!(check_model_allowed("zai-start/GLM-5-Turbo", "zai").is_err());
        // An allowed model on a non-Individual plan is refused too.
        let start = check_model_allowed("zai-start/GLM-5.3", "zai").unwrap_err();
        assert!(start.contains("zai-start"), "{start}");
        assert!(start.contains("Individual plan"), "{start}");
    }

    /// apb emits only the bare spelling: the legacy prefix and case are
    /// normalized, anything that is not the individual plan is left alone.
    #[test]
    fn canonical_model_drops_the_individual_plan_prefix() {
        assert_eq!(
            canonical_model("zai-individual/glm-5.3-flash@HIGH", "zai"),
            "GLM-5.3-Flash@high"
        );
        assert_eq!(canonical_model("zai-individual/GLM-5.3", "zai"), "GLM-5.3");
        assert_eq!(
            canonical_model(" GLM-5.3-Flash@low ", "zai"),
            "GLM-5.3-Flash@low"
        );
        assert_eq!(
            canonical_model("zai-start/GLM-5.3", "zai"),
            "zai-start/GLM-5.3"
        );
        assert_eq!(canonical_model("my-provider/x", "zai"), "my-provider/x");
        assert_eq!(canonical_model("", "zai"), "");
    }

    #[test]
    fn selection_json_matches_zcode_schema() {
        let s = parse_model("zai-start/GLM-5.3@low", "zai", &known()).unwrap();
        assert_eq!(
            s.to_json(),
            serde_json::json!({
                "providerId": "account:zai-start-plan",
                "modelId": "GLM-5.3",
                "options": {"reasoningLevel": "low"}
            })
        );
        let d = parse_model("GLM-5.3", "zai", &known()).unwrap();
        assert!(d.to_json().get("options").is_none());
    }

    #[test]
    fn plans_the_cli_is_not_logged_in_to_are_refused() {
        let sel = parse_model("zai-start/GLM-5.3", "zai", &known()).unwrap();
        let paid: BTreeSet<String> = ["account:zai-individual-coding-plan".to_string()].into();
        let err = check_plan_usable(&sel, &paid).unwrap_err();
        assert!(err.contains("not logged in"), "{err}");
        assert!(err.contains("zai-start"), "{err}");
        let ok = parse_model("zai-individual/GLM-5.3", "zai", &known()).unwrap();
        assert!(check_plan_usable(&ok, &paid).is_ok());
        let custom = parse_model("my-provider/model", "zai", &known()).unwrap();
        assert!(check_plan_usable(&custom, &BTreeSet::new()).is_ok());
    }

    /// The offered list is exactly the allowlist, as bare ids - no plan
    /// prefix, no Start-plan entries, independent of login state.
    #[test]
    fn model_list_is_exactly_the_allowlist_as_bare_ids() {
        assert_eq!(model_list(), vec!["GLM-5.3", "GLM-5.3-Flash"]);
    }

    #[test]
    fn logged_in_providers_reads_key_names_only() {
        let home = tempfile::tempdir().unwrap();
        let v2 = home.path().join(".zcode/v2");
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::write(
            v2.join("credentials.json"),
            r#"{"oauth:zai:access_token":"x","account-provider:account:zai-start-plan:identity":"id","account-provider:coding-plan:account:zai-start-plan:account:id:api-key":"k"}"#,
        )
        .unwrap();
        let got = logged_in_providers(home.path());
        assert_eq!(got, BTreeSet::from(["account:zai-start-plan".to_string()]));
        assert!(logged_in_providers(tempfile::tempdir().unwrap().path()).is_empty());
    }

    /// A trimmed copy of ZCode 0.16.9's built-in provider config: the plan
    /// model list and the reasoning-level rules that matter here.
    const FAKE_BUILTIN: &str = r#"{"schemaVersion":1,"revision":30,"config":{"modelConfigRules":{
        "modelRules":[
            {"modelMatch":".*","config":{"optionSpecs":{"reasoningLevel":{"values":["disabled","enabled"]}}}},
            {"modelMatch":".*GLM-5\\.2(?:[.\\-:/\\[].*)?","config":{"optionSpecs":{"reasoningLevel":{"values":["disabled","high","max"]}}}},
            {"modelMatch":".*glm-5\\.3(?:-flash)?(?:[.\\-:/\\[].*)?","config":{"optionSpecs":{"reasoningLevel":{"values":["low","high","max"]}}}}
        ],
        "builtinProviderModelRules":[
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5.3-Flash","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5-Turbo","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-start-plan"}]}}}"#;

    #[test]
    fn reasoning_levels_follow_the_last_matching_rule() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.json");
        std::fs::write(&f, FAKE_BUILTIN).unwrap();
        assert_eq!(
            reasoning_levels(&f, "GLM-5.3-Flash").unwrap(),
            vec!["low", "high", "max"]
        );
        assert_eq!(
            reasoning_levels(&f, "GLM-5.2").unwrap(),
            vec!["disabled", "high", "max"]
        );
        assert_eq!(
            reasoning_levels(&f, "GLM-5-Turbo").unwrap(),
            vec!["disabled", "enabled"]
        );
    }

    /// Every selection apb hands ZCode must be one it honors: ZCode replaces
    /// an invalid one with GLM-5.3 at max effort without saying so. The
    /// allowlist gate comes first: GLM-5-Turbo is enabled on the individual
    /// plan in the fixture config, but apb refuses it anyway.
    #[test]
    fn complete_selection_fills_effort_and_refuses_what_zcode_would_replace() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.json");
        std::fs::write(&f, FAKE_BUILTIN).unwrap();
        // An omitted effort gets apb's default, not ZCode's own (the model's
        // highest level, `max`, which made medium tasks take many minutes).
        let mut flash = parse_model("zai-individual/glm-5.3-flash", "zai", &known()).unwrap();
        complete_selection(&mut flash, &f).unwrap();
        assert_eq!(flash.effort.as_deref(), Some(DEFAULT_EFFORT));
        assert_ne!(flash.effort.as_deref(), Some("max"));
        // An explicit `@max` stays the per-profile override.
        let mut max = parse_model("GLM-5.3@max", "zai", &known()).unwrap();
        complete_selection(&mut max, &f).unwrap();
        assert_eq!(max.effort.as_deref(), Some("max"));
        let mut high = parse_model("zai-individual/GLM-5.3-Flash@high", "zai", &known()).unwrap();
        complete_selection(&mut high, &f).unwrap();
        assert_eq!(high.effort.as_deref(), Some("high"));

        let mut turbo = parse_model("zai-individual/GLM-5-Turbo", "zai", &known()).unwrap();
        let e = complete_selection(&mut turbo, &f).unwrap_err();
        assert!(e.contains("not allowed"), "{e}");
        assert!(e.contains("GLM-5.3-Flash"), "{e}");

        let mut bad_effort =
            parse_model("zai-individual/GLM-5.3-Flash@xhigh", "zai", &known()).unwrap();
        let e = complete_selection(&mut bad_effort, &f).unwrap_err();
        assert!(e.contains("low, high, max"), "{e}");
        let mut off_plan = parse_model("zai-start/GLM-5.3-Flash", "zai", &known()).unwrap();
        let e = complete_selection(&mut off_plan, &f).unwrap_err();
        assert!(e.contains("not offered on plan `zai-start`"), "{e}");
        let mut typo = parse_model("zai-individual/GLM-5.3-Flsh", "zai", &known()).unwrap();
        assert!(complete_selection(&mut typo, &f).is_err());
        let mut custom = parse_model("my-provider/anything", "zai", &known()).unwrap();
        complete_selection(&mut custom, &f).unwrap();
        assert_eq!(custom.effort, None);
    }

    /// Lays out a fake ZCode home: built-in config with two plans, a personal
    /// config, and credentials logged in to the individual plan only.
    fn fake_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let v2 = home.path().join(".zcode/v2");
        let bundled = home.path().join(HOME_REL_BUILTIN_CONFIG);
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(&bundled, FAKE_BUILTIN).unwrap();
        std::fs::write(
            v2.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
        )
        .unwrap();
        std::fs::write(v2.join("setting.json"), r#"{"providerFamilyDomain":"zai"}"#).unwrap();
        std::fs::write(
            v2.join("credentials.json"),
            r#"{"account-provider:account:zai-individual-coding-plan:identity":"id"}"#,
        )
        .unwrap();
        home
    }

    #[test]
    fn spawn_env_points_zcode_at_a_scoped_model_selection() {
        if std::env::var_os(BUILTIN_CONFIG_ENV).is_some()
            || std::env::var_os(PERSONAL_CONFIG_ENV).is_some()
        {
            return; // the caller's own override wins; nothing to assert here
        }
        let home = fake_home();
        let scoped = home.path().join("run/agent-home/zcode/n1");
        let env = spawn_env_in(
            home.path(),
            "zai-individual/glm-5.3-flash@low",
            Some(&scoped),
        )
        .unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(
            get(BUILTIN_CONFIG_ENV).unwrap(),
            home.path().join(HOME_REL_BUILTIN_CONFIG)
        );
        let personal = get(PERSONAL_CONFIG_ENV).unwrap();
        assert_eq!(personal, scoped.join("provider_config.json"));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&personal).unwrap()).unwrap();
        assert_eq!(
            doc["config"]["defaultModelSelection"],
            serde_json::json!({
                "providerId": "account:zai-individual-coding-plan",
                "modelId": "GLM-5.3-Flash",
                "options": {"reasoningLevel": "low"}
            })
        );
        // The user's own personal config is untouched.
        let own = std::fs::read_to_string(home.path().join(HOME_REL_PERSONAL_CONFIG)).unwrap();
        assert!(!own.contains("defaultModelSelection"));

        // No model: the user's own personal config is named as-is.
        let env = spawn_env_in(home.path(), "", None).unwrap();
        assert_eq!(
            env.iter()
                .find(|(n, _)| n == PERSONAL_CONFIG_ENV)
                .unwrap()
                .1,
            home.path().join(HOME_REL_PERSONAL_CONFIG)
        );

        // A plan the CLI is not logged in to is refused before any spawn.
        let err = spawn_env_in(home.path(), "zai-start/GLM-5.3", Some(&scoped)).unwrap_err();
        assert!(err.to_string().contains("not logged in"), "{err}");
    }

    /// `complete_selection` is public and may get a selection that did not
    /// come through `parse_model`: its model id is matched against the plan's
    /// models case-insensitively, like every other ZCode id here, and
    /// rewritten to the config's spelling (ZCode itself compares exactly and
    /// would silently replace a differently cased id).
    #[test]
    fn complete_selection_matches_model_ids_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.json");
        std::fs::write(&f, FAKE_BUILTIN).unwrap();
        let mut sel = ModelSelection {
            provider_id: "account:zai-individual-coding-plan".into(),
            model_id: "glm-5.3-flash".into(),
            effort: None,
        };
        complete_selection(&mut sel, &f).unwrap();
        assert_eq!(sel.model_id, "GLM-5.3-Flash");
        assert_eq!(sel.effort.as_deref(), Some(DEFAULT_EFFORT));
    }

    /// A model that does not offer apb's default level gets ZCode's own
    /// default (its highest level) rather than a level ZCode would refuse.
    #[test]
    fn a_model_without_the_default_level_gets_its_highest() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.json");
        std::fs::write(
            &f,
            r#"{"config":{"modelConfigRules":{
                "modelRules":[{"modelMatch":"GLM-5\\.3-Flash","config":{"optionSpecs":{"reasoningLevel":{"values":["high","max"]}}}}],
                "builtinProviderModelRules":[{"modelId":"GLM-5.3-Flash","providerId":"account:zai-individual-coding-plan"}]}}}"#,
        )
        .unwrap();
        let mut sel = parse_model("GLM-5.3-Flash", "zai", &known()).unwrap();
        complete_selection(&mut sel, &f).unwrap();
        assert_eq!(sel.effort.as_deref(), Some("max"));
    }

    #[test]
    fn trace_id_is_read_from_the_json_result() {
        assert_eq!(
            trace_id("{\n  \"sessionId\": \"sess_1\",\n  \"traceId\": \"t-1\"\n}").as_deref(),
            Some("t-1")
        );
        assert_eq!(trace_id("plain text"), None);
        assert_eq!(trace_id("{\"traceId\": \"\"}"), None);
    }

    /// The selection reported for a finished step is the one it ran with:
    /// canonical ids, the plan, and the effort apb filled in.
    #[test]
    fn effective_selection_reports_the_completed_selection() {
        if std::env::var_os(BUILTIN_CONFIG_ENV).is_some() {
            return; // the caller's own built-in config would be read instead
        }
        let home = fake_home();
        let sel = effective_selection(home.path(), "glm-5.3-flash").unwrap();
        assert_eq!(sel.provider_id, "account:zai-individual-coding-plan");
        assert_eq!(sel.model_id, "GLM-5.3-Flash");
        assert_eq!(sel.effort.as_deref(), Some(DEFAULT_EFFORT));
        let high = effective_selection(home.path(), "GLM-5.3@high").unwrap();
        assert_eq!(high.effort.as_deref(), Some("high"));
        assert_eq!(effective_selection(home.path(), ""), None);
    }

    /// A step whose selection the built-in config refuses now (it changed
    /// since the step ran) is still reported with its model.
    #[test]
    fn effective_selection_keeps_the_parsed_selection_when_completion_refuses() {
        if std::env::var_os(BUILTIN_CONFIG_ENV).is_some() {
            return;
        }
        let home = fake_home();
        let sel = effective_selection(home.path(), "GLM-5.2").expect("still reported");
        assert_eq!(sel.model_id, "GLM-5.2");
        assert_eq!(sel.effort, None);
    }

    /// An `account:` prefix in any case is the same plan, so the allowlist
    /// and the login check see it.
    #[test]
    fn the_account_prefix_is_matched_in_any_case() {
        let sel = parse_model("ACCOUNT:zai-start-plan/GLM-5.3", "zai", &known()).unwrap();
        assert_eq!(sel.provider_id, "account:zai-start-plan");
        assert!(check_model_allowed("Account:zai-start-plan/GLM-5.3", "zai").is_err());
    }

    /// Without a level list to pick from (no built-in config), a plan
    /// selection still carries apb's default effort: no effort at all is the
    /// selection ZCode replaces with its most expensive one.
    #[test]
    fn the_default_effort_is_filled_without_a_level_list() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let mut sel = parse_model("GLM-5.3-Flash", "zai", &known()).unwrap();
        complete_selection(&mut sel, &missing).unwrap();
        assert_eq!(sel.effort.as_deref(), Some(DEFAULT_EFFORT));
        let mut custom = parse_model("my-provider/anything", "zai", &known()).unwrap();
        complete_selection(&mut custom, &missing).unwrap();
        assert_eq!(custom.effort, None);
    }

    #[test]
    fn stream_json_usage_is_read_from_the_result_line() {
        let stream = "{\"type\":\"event\"}\n{\"type\":\"result\",\"sessionId\":\"sess_1\",\"traceId\":\"t-2\",\"response\":\"done\",\"usage\":{\"inputTokens\":10,\"outputTokens\":3,\"cacheReadTokens\":4}}\n";
        use crate::agents::OutputFormat as _;
        let usage = output::ZcodeOutput.usage(stream).expect("usage");
        assert_eq!((usage.input_tokens, usage.output_tokens), (6, 3));
        assert_eq!(session_id(stream).as_deref(), Some("sess_1"));
        assert_eq!(trace_id(stream).as_deref(), Some("t-2"));
        assert_eq!(response_text(stream).as_deref(), Some("done"));
        // A later result line without a session id does not hide the
        // earlier one.
        let two = format!("{stream}{{\"type\":\"result\",\"sessionId\":\"\"}}\n");
        assert_eq!(session_id(&two).as_deref(), Some("sess_1"));
        assert_eq!(
            output::ZcodeOutput.usage(&two).map(|u| u.output_tokens),
            Some(3)
        );
    }

    /// A known plan with its `account:` prefix resolves to the same id in
    /// any case; an unknown one keeps its spelling.
    #[test]
    fn a_prefixed_plan_resolves_whatever_its_case() {
        assert_eq!(
            resolve_plan("ACCOUNT:ZAI-INDIVIDUAL-CODING-PLAN", "zai"),
            "account:zai-individual-coding-plan"
        );
        assert!(check_model_allowed("ACCOUNT:ZAI-INDIVIDUAL-CODING-PLAN/GLM-5.3", "zai").is_ok());
        assert_eq!(resolve_plan("Account:My-Plan", "zai"), "account:My-Plan");
    }

    #[test]
    fn personal_config_copy_overrides_only_the_default_selection() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.json");
        std::fs::write(
            &src,
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[{"x":1}]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]},"defaultModelSelection":{"providerId":"a","modelId":"b"}}}"#,
        )
        .unwrap();
        let before = std::fs::read(&src).unwrap();
        let sel = parse_model("zai-start/GLM-5.3", "zai", &known()).unwrap();
        let dest = dir.path().join("out/provider_config.json");
        write_personal_config(&src, &sel, &dest).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), before, "source untouched");
        let out: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(out["config"]["defaultModelSelection"], sel.to_json());
        assert_eq!(
            out["config"]["providerConfigRules"]["providerRules"][0]["x"],
            1
        );

        // No source: a minimal valid config.
        let dest2 = dir.path().join("fresh.json");
        write_personal_config(&dir.path().join("missing.json"), &sel, &dest2).unwrap();
        let out2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dest2).unwrap()).unwrap();
        assert_eq!(out2["schemaVersion"], 1);
        assert_eq!(out2["config"]["defaultModelSelection"], sel.to_json());
    }
}
