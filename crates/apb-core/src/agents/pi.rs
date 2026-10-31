//! pi. An aggregator that detection probes; apb has no launch form for it
//! yet.

use super::{Agent, AgentCategory, Capabilities, Descriptor};

/// The agent id.
pub const ID: &str = "pi";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["pi"],
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

/// The pi agent.
pub struct Pi;

impl Agent for Pi {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &OUTPUT
    }
}
