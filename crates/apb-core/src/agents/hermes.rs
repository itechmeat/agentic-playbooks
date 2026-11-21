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
/// one-shot form ever print one. The reply is the model's own text, so an id
/// found there is used only after the shape check every captured session
/// goes through (`apb_engine::adapter::capture_session`); any other value
/// is no session and the next step starts cold.
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
            // hermes re-enters a session via `--resume <id>`, the prompt
            // after `-z`, still in one-shot mode.
            resume: Some(&["--resume", "{session}", "-z", "{prompt}", "-m", "{model}"]),
            fork: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value option of the launch and the resume form is followed by
    /// its placeholder, so the command line is valid (`-z` takes the prompt,
    /// `-m` the model and, in the resume form, `--resume` the session).
    #[test]
    fn every_value_option_is_followed_by_its_placeholder() {
        let forms = Hermes.forms().expect("hermes has forms");
        let launch: Vec<&str> = forms.launch.argv.iter().map(String::as_str).collect();
        let resume: Vec<&str> = forms.resume.expect("hermes has a resume form").to_vec();
        let pairs: &[(&str, &str)] = &[("-z", "{prompt}"), ("-m", "{model}")];
        let mut resume_pairs = pairs.to_vec();
        resume_pairs.push(("--resume", "{session}"));
        for (name, argv, wanted) in [
            ("launch", &launch[..], pairs),
            ("resume", &resume[..], resume_pairs.as_slice()),
        ] {
            for (option, placeholder) in wanted {
                let i = argv
                    .iter()
                    .position(|a| a == option)
                    .unwrap_or_else(|| panic!("{name} form has no {option}"));
                assert_eq!(
                    argv.get(i + 1).copied(),
                    Some(*placeholder),
                    "{name} form: {option} is not followed by its value"
                );
            }
        }
    }
}
