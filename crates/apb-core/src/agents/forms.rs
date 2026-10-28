//! How an agent is launched: its built-in invocation form, its resume form
//! and how a fresh attempt makes its session findable.

use crate::config::{Interaction, InvocationDef, PromptVia, SoulDelivery, Transport};

use super::types::ZcodeMode;

/// An agent's built-in forms.
#[derive(Debug, Clone)]
pub struct Forms {
    /// The launch form.
    pub launch: InvocationDef,
    /// The resume argv (spec 2026-07-20, Task 7): `{session}` (the captured
    /// session id), `{prompt}` (the follow-up) and `{model}` are whole argv
    /// elements, filled like the launch argv. `None` for an agent with no
    /// resume form, which forces the runtime downgrade to `reprompt`.
    pub resume: Option<&'static [&'static str]>,
    /// How a fresh attempt makes its session findable.
    pub fresh: FreshForm,
    /// The autonomy flags a profile's `zcode_mode` asks for, for an agent
    /// whose permission mode a profile can pick; they replace the launch
    /// form's own.
    pub profile_mode: Option<fn(ZcodeMode) -> Vec<String>>,
}

impl Forms {
    /// The resume argv as owned strings.
    pub fn resume_argv(&self) -> Option<Vec<String>> {
        self.resume
            .map(|argv| argv.iter().map(|s| s.to_string()).collect())
    }

    /// The [`FreshSession`] of a fresh attempt; `title` must be unique to the
    /// attempt (run, node and attempt number).
    pub fn fresh_session(&self, title: &str) -> FreshSession {
        match self.fresh {
            FreshForm::Assigned { flag } => {
                let id = uuid::Uuid::new_v4().to_string();
                FreshSession::Assigned {
                    args: vec![flag.to_string(), id.clone()],
                    id,
                }
            }
            FreshForm::Titled { flag, .. } => FreshSession::Titled {
                args: vec![flag.to_string(), title.to_string()],
                title: title.to_string(),
            },
            FreshForm::Printed => FreshSession::Printed,
        }
    }
}

/// How an agent's fresh session becomes known, as data.
#[derive(Debug, Clone, Copy)]
pub enum FreshForm {
    /// apb picks the id and passes it with `flag`.
    Assigned { flag: &'static str },
    /// apb names the session with `flag` and finds its id in `listing`.
    Titled {
        flag: &'static str,
        listing: SessionListing,
    },
    /// Only an id the agent prints itself is known.
    Printed,
}

/// The command that lists an agent's sessions, run with the agent's program
/// in the attempt's working directory, and the reader that finds the id of
/// the session with a given title in its stdout.
#[derive(Debug, Clone, Copy)]
pub struct SessionListing {
    pub args: &'static [&'static str],
    /// `(stdout, title) -> id`.
    pub find: fn(&str, &str) -> Option<String>,
}

/// How a fresh attempt makes its agent session findable, so a retry, a
/// fallback back onto the same binding or a deadline continuation can resume
/// it instead of re-sending the whole prompt (issue #136 items 2 and 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshSession {
    /// apb picks the id and hands it over at launch (claude `--session-id`),
    /// so the session is known even when the attempt is killed at its
    /// deadline, before it printed anything.
    Assigned { id: String, args: Vec<String> },
    /// The session gets a unique title at launch and is looked up by it
    /// afterwards (opencode `--title`, then `session list --format json`), so
    /// it is found even when the attempt printed no id.
    Titled { title: String, args: Vec<String> },
    /// Only an id the agent prints itself (on a normal exit) is known.
    Printed,
}

/// A headless launch form with the prompt in argv.
pub(super) fn headless(
    argv: &[&str],
    soul: SoulDelivery,
    soul_flag: Option<&str>,
    autonomous_args: &[&str],
    interaction: Interaction,
) -> InvocationDef {
    InvocationDef {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        prompt_via: PromptVia::Argv,
        soul,
        soul_flag: soul_flag.map(|s| s.to_string()),
        transport: Transport::Headless,
        autonomous_args: autonomous_args.iter().map(|s| s.to_string()).collect(),
        interaction,
    }
}
