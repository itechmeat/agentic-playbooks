//! cursor (Cursor CLI). An aggregator: it runs gpt, claude, gemini and other
//! vendor models, so it contributes no curated rows of its own. It is
//! installed as `cursor-agent`; the bare `cursor` binary is the GUI editor
//! CLI, not the headless agent.

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, Capabilities, Descriptor, Forms, FreshForm, HostSetup,
};

/// The agent id.
pub const ID: &str = "cursor";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["cursor-agent"],
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
    session_fields: &["chatId", "chat_id", "session_id"],
};

/// The cursor agent.
pub struct Cursor;

impl Agent for Cursor {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &CursorSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &OUTPUT
    }

    // cursor's `-p/--print` is a BOOLEAN flag and the prompt is a positional
    // argument, so the prompt slot goes last, after the options.
    // `--output-format text` pins plain stdout (it only takes effect together
    // with `--print`); it is part of every form, since the output shape has
    // nothing to do with permissions. `--force` alone is the non-interactive
    // approval mode. No system-prompt flag exists, so the SOUL travels as a
    // prefix like the other aggregators.
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
                SoulDelivery::Prefix,
                None,
                &["--force"],
                Interaction::Resume,
            ),
            // cursor resumes a chat via `--resume <chatId>`; the follow-up
            // prompt stays positional and therefore last.
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

/// cursor's host needs.
struct CursorSetup;

impl HostSetup for CursorSetup {
    // unverified: no cursor-agent was installed where the sets were checked;
    // the prefix follows the vendor's documentation.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["CURSOR_*"],
            when: &[],
        }
    }
}
