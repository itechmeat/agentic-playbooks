//! hermes (Hermes Agent). An aggregator; its one-shot `-z` form prints only
//! the final response text and reads its keys from `~/.hermes/.env`.

use std::path::{Path, PathBuf};

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, AuthHint, AuthKind, Capabilities, Descriptor, Forms,
    FreshForm, HostSetup,
};

/// The agent id.
pub const ID: &str = "hermes";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["hermes"],
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
    session_fields: &["session", "session_id"],
};

/// The hermes agent.
pub struct Hermes;

impl Agent for Hermes {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &HermesSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &OUTPUT
    }

    // hermes one-shot mode prints only the final response text to stdout and
    // auto-bypasses approvals by design (script mode); the SOUL travels as a
    // prompt prefix like the other aggregators.
    //
    // No autonomous flag: hermes documents `--yolo` at the same top level as
    // `-z`, but it could not be verified in combination with the one-shot
    // `-z` form against a local binary, and shipping an unverified flag into
    // every hermes invocation is worse than the documented gap. `apb doctor
    // --run` warns on a node bound to hermes instead (#85 finding 1).
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &["-z", "{prompt}", "-m", "{model}"],
                SoulDelivery::Prefix,
                None,
                &[],
                Interaction::Resume,
            ),
            // hermes re-enters a session via `--resume <id>`, still in script
            // mode.
            resume: Some(&["-z", "--resume", "{session}", "{prompt}", "-m", "{model}"]),
            fresh: FreshForm::Printed,
            profile_mode: None,
        })
    }
}

/// hermes's host facts and needs.
struct HermesSetup;

impl HostSetup for HermesSetup {
    // Presence of `~/.hermes/.env` only: values are never read.
    fn auth_hint(&self, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        let hint = home.join(".hermes/.env").is_file().then_some(AuthHint {
            kind: AuthKind::ApiKey,
        });
        (hint, None)
    }

    fn memo_files(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".hermes/.env")]
    }

    // hermes reads its keys from `~/.hermes/.env`, found through HOME.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["HERMES_*"],
            when: &[],
        }
    }
}
