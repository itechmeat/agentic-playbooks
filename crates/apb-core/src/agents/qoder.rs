//! qoder (Qoder CLI). An aggregator: one subscription serving Qwen, DeepSeek,
//! GLM, Kimi and MiniMax models, so it contributes no curated rows of its
//! own. npm installs both `qoder` and a `qodercli` alias for the same binary;
//! the shorter `qoder` is unambiguous and is the one probed. Model
//! enumeration via `--list-models` requires authentication, so it is
//! deferred like grok's.

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, Capabilities, Descriptor, Forms, FreshForm, HostSetup,
};

/// The agent id.
pub const ID: &str = "qoder";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["qoder"],
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
    session_fields: &["session_id"],
};

/// The qoder agent.
pub struct Qoder;

impl Agent for Qoder {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &QoderSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &OUTPUT
    }

    // qoder's `-p/--print` is a BOOLEAN flag and the prompt is a positional
    // argument, so the prompt slot goes last, after the options, like
    // cursor. Unlike cursor it has a real system-prompt flag;
    // `--append-system-prompt` is used (not `--system-prompt`) so the SOUL
    // rides alongside qoder's own agentic system prompt instead of replacing
    // it. `--permission-mode bypass_permissions` (snake_case, unlike claude's
    // camelCase) is the non-interactive approval mode and `--output-format
    // text` pins plain stdout. Verified against `@qoder-ai/qodercli` 1.1.22
    // `--help`.
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &[
                    "-p",
                    "--output-format",
                    "text",
                    "--model",
                    "{model}",
                    "--",
                    "{prompt}",
                ],
                SoulDelivery::Native,
                Some("--append-system-prompt"),
                &["--permission-mode", "bypass_permissions"],
                Interaction::Resume,
            ),
            // qoder resumes a session via `--resume <id>`; the follow-up
            // prompt stays positional and therefore last, like cursor.
            resume: Some(&[
                "--resume",
                "{session}",
                "-p",
                "--output-format",
                "text",
                "--model",
                "{model}",
                "--",
                "{prompt}",
            ]),
            fresh: FreshForm::Printed,
            profile_mode: None,
        })
    }
}

/// qoder's host needs.
struct QoderSetup;

impl HostSetup for QoderSetup {
    // Checked against the variable names qodercli 1.1.76 reads.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["QODER_*", "QODERCLI_*"],
            when: &[],
        }
    }
}
