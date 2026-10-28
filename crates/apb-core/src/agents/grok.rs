//! grok (Grok Build, xAI). The CLI installs BOTH `grok` and a generic `agent`
//! alias pointing at the same binary; Cursor installs a generic `agent` alias
//! too, so `agent` cannot identify either agent and only the unambiguous
//! `grok` is probed. Model enumeration via the `models` subcommand is
//! deferred: it requires authentication, so an unauthenticated machine would
//! cache an empty list.

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, Capabilities, Descriptor, Forms, FreshForm, HostSetup,
    ModelPolicy,
};

/// The agent id.
pub const ID: &str = "grok";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["grok"],
    version_args: &["--version"],
    help_args: &["--help"],
    home_paths: &[],
    category: AgentCategory::Vendor,
    capabilities: Capabilities::NONE,
    verified: None,
};

/// Plain text; a session id would be read from these JSON fields, should the
/// one-shot form ever print one.
static OUTPUT: super::PlainText = super::PlainText {
    session_fields: &["session_id", "sessionId"],
};

/// The grok agent.
pub struct Grok;

impl Agent for Grok {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn models(&self) -> &'static dyn super::ModelPolicy {
        &GrokModels
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &GrokSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &OUTPUT
    }

    // grok runs headless one-shot via `-p/--single` and has a native
    // system-prompt flag (`--system-prompt-override`), so its SOUL does not
    // have to travel as a prompt prefix. Like claude, an authorized effectful
    // run needs an explicit non-interactive permission mode or every tool
    // call blocks on an approval that never arrives. Both verified against
    // Grok Build 0.2.x `--help`.
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &["-p", "{prompt}", "-m", "{model}"],
                SoulDelivery::Native,
                Some("--system-prompt-override"),
                &["--permission-mode", "bypassPermissions"],
                Interaction::Resume,
            ),
            // grok resumes by id via `-r <id>` and takes the follow-up as a
            // fresh `-p` single-turn prompt.
            resume: Some(&["-r", "{session}", "-p", "{prompt}", "-m", "{model}"]),
            fresh: FreshForm::Printed,
            profile_mode: None,
        })
    }
}

/// grok's host needs.
struct GrokSetup;

impl HostSetup for GrokSetup {
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["GROK_*", "XAI_API_KEY"],
            when: &[],
        }
    }
}

/// grok's model rules.
struct GrokModels;

impl ModelPolicy for GrokModels {
    fn vendor(&self) -> Option<&'static str> {
        Some("xai")
    }
}
