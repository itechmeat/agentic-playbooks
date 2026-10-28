//! agy (the Antigravity CLI). An aggregator; its one-shot form prints plain
//! text and has no resumable session.

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, Authority, Capabilities, Descriptor, Forms, FreshForm,
    HostSetup,
};

/// The agent id.
pub const ID: &str = "agy";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["agy"],
    version_args: &["--version"],
    help_args: &["--help"],
    home_paths: &[],
    category: AgentCategory::Aggregator,
    capabilities: Capabilities::NONE,
    verified: None,
};

/// Plain text; a session id would be read from these JSON fields, should the
/// one-shot form ever print one.
static OUTPUT: super::PlainText = super::PlainText {
    session_fields: &[],
};

/// The agy agent.
pub struct Agy;

impl Agent for Agy {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &AgySetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &OUTPUT
    }

    // Verified against the local `agy --help`: `--dangerously-skip-permissions`
    // auto-approves all tool permission requests, the exact equivalent of
    // claude's flag. agy exposes no resumable session, so its interaction
    // ceiling is `reprompt`.
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &["-p", "{prompt}", "--model", "{model}"],
                SoulDelivery::Prefix,
                None,
                &["--dangerously-skip-permissions"],
                Interaction::Reprompt,
            ),
            resume: None,
            fresh: FreshForm::Printed,
            profile_mode: None,
        })
    }
}

/// agy's host facts and needs.
struct AgySetup;

impl HostSetup for AgySetup {
    // A list for display only: it does not guarantee runnability.
    fn models_command(&self) -> Option<(&'static [&'static str], Authority)> {
        Some((&["models"], Authority::Display))
    }

    // CLOUDSDK_CONFIG is not read by agy itself (unverified there); it
    // locates the gcloud credentials its Google login can rely on.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &[
                "AGY_*",
                "ANTIGRAVITY_*",
                "GEMINI_API_KEY",
                "GOOGLE_API_KEY",
                "GOOGLE_CLOUD_PROJECT",
                "GOOGLE_APPLICATION_CREDENTIALS",
                "CLOUDSDK_CONFIG",
            ],
            when: &[],
        }
    }
}
