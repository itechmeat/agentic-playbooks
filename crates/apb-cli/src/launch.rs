//! The launch policy at the CLI: the `--confirm-launch` flags of `apb run`,
//! `apb resume` and `apb eval`, the terminal question, and `apb launch
//! floor`.
//!
//! The CLI's initiator is a run step when `APB_RUN_ID` or `APB_HOST_TASK`
//! is set (then nothing confirms a launch), else the CLI at a terminal or
//! without one. Only a terminal question or the refusal's nonce counts as a
//! confirmation: a missing variable never makes the caller a person.
//! `apb eval --yes` approves a suite and never a launch.

use std::path::Path;
use std::process::ExitCode;

use apb_core::launch::LaunchPolicy;
use apb_core::launch::floor::{self, FloorKey};
use apb_engine::gate::launch::{
    ASK_POLICY, DENY_POLICY, Initiator, LaunchConfirmation, LaunchContext, LaunchKind,
};
use serde_json::Value;

/// The launch confirmation flags.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct LaunchFlags {
    /// Confirm an agent launch of a playbook that asks first (`agent_launch:
    /// ask`): the launch_nonce the refusal printed, with its
    /// --launch-version and --launch-choice. At a terminal, apb asks
    /// instead. Never accepted inside a run step
    #[arg(
        long = "confirm-launch",
        value_name = "LAUNCH_NONCE",
        require_equals = true
    )]
    pub confirm_launch: Option<String>,
    /// The version the launch refusal named
    #[arg(long = "launch-version", value_name = "VERSION", require_equals = true)]
    pub launch_version: Option<String>,
    /// How the launch refusal chose that version
    #[arg(
        long = "launch-choice",
        value_name = "CHOICE",
        value_parser = ["candidate", "current", "explicit"],
        require_equals = true
    )]
    pub launch_choice: Option<String>,
}

/// Whether a person can be asked here: stdin and stderr are terminals.
pub(crate) fn at_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

impl LaunchFlags {
    /// The CLI's launch context for `kind`.
    pub(crate) fn context(&self, kind: LaunchKind) -> LaunchContext {
        LaunchContext::new(Initiator::cli(at_terminal()), kind)
            .confirmed(
                self.confirm_launch
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .map(|nonce| LaunchConfirmation::CliFlag { nonce }),
            )
            .fixed(self.launch_version.clone(), self.launch_choice.as_deref())
    }
}

/// Runs `check` with `ctx`; when it refuses a launch a person at this
/// terminal may confirm (`ask` or `deny`, the CLI at a terminal), asks them
/// and runs it once more with the terminal's confirmation and the refusal's
/// fixed version and choice.
pub(crate) fn with_prompt<T>(
    ctx: &LaunchContext,
    check: impl Fn(&LaunchContext) -> Result<T, Value>,
) -> Result<T, Value> {
    match check(ctx) {
        Ok(v) => Ok(v),
        Err(refusal) => match prompt_retry(ctx, &refusal) {
            Some(retry) => check(&retry),
            None => Err(refusal),
        },
    }
}

/// The context to retry a refused launch with once a person at this
/// terminal confirmed it; `None` when nobody here may confirm it (not a
/// terminal, a run step, another refusal) or the person said no.
pub(crate) fn prompt_retry(ctx: &LaunchContext, refusal: &Value) -> Option<LaunchContext> {
    let askable = matches!(ctx.initiator, Initiator::CliUnattested { terminal: true })
        && matches!(refusal["policy"].as_str(), Some(ASK_POLICY | DENY_POLICY));
    if !askable || ask(refusal) != Some(true) {
        return None;
    }
    Some(
        ctx.unconfirmed()
            .confirmed(Some(LaunchConfirmation::TerminalPrompt))
            .fixed(
                refusal["launch_version"].as_str().map(str::to_string),
                refusal["launch_choice"].as_str(),
            ),
    )
}

/// Asks the person to confirm the launch a refusal describes.
fn ask(refusal: &Value) -> Option<bool> {
    let id = refusal["id"].as_str().unwrap_or_default();
    let policy = refusal["launch_policy"].as_str().unwrap_or_default();
    let mut lines = vec![format!(
        "playbook `{id}` is `{policy}` for agents; you are launching it from this terminal:"
    )];
    if let Some(sources) = refusal["launch_sources"].as_array() {
        lines.extend(
            sources
                .iter()
                .filter_map(Value::as_str)
                .map(|s| format!("  - {s}")),
        );
    }
    crate::consent::ask_terminal(&lines, "Launch it?")
}

/// The CLI text of a launch refusal: the policy, its sources and the way a
/// person confirms it.
pub(crate) fn refusal_message(refusal: &Value) -> String {
    let policy = refusal["policy"].as_str().unwrap_or("launch_refused");
    let mut msg = format!(
        "refused ({policy}): {}",
        refusal["detail"].as_str().unwrap_or_default()
    );
    if let Some(nonce) = refusal["launch_nonce"].as_str() {
        msg.push_str(&format!(
            ". Confirm with --confirm-launch={nonce} --launch-version={} --launch-choice={} after asking the person, or run it from a terminal to be asked",
            refusal["launch_version"].as_str().unwrap_or_default(),
            refusal["launch_choice"].as_str().unwrap_or_default()
        ));
    }
    msg
}

/// `apb launch floor <id> [--global] [--lower-to <policy>]`: prints a
/// playbook's launch floor, or a person lowers it at a terminal.
pub(crate) fn floor_cmd(root: &Path, id: &str, global: bool, lower_to: Option<&str>) -> ExitCode {
    if !apb_core::registry::is_safe_segment(id) {
        eprintln!("`{id}` is not a playbook id");
        return ExitCode::from(2);
    }
    let (key, dir) = if global {
        (
            Some(FloorKey::global(id)),
            apb_core::store::global_playbooks_parent().map(|p| p.join("playbooks").join(id)),
        )
    } else {
        (
            FloorKey::read_project(root, id),
            Some(apb_core::versioning::playbooks_dir(root).join(id)),
        )
    };
    let Some(lower_to) = lower_to else {
        return match floor::floor_of(key.as_ref(), dir.as_deref()) {
            Ok(Some(p)) => {
                println!("launch floor of `{id}`: {p}");
                ExitCode::SUCCESS
            }
            Ok(None) => {
                println!("launch floor of `{id}`: none (no version declared agent_launch)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(2)
            }
        };
    };
    let Some(to) = LaunchPolicy::parse(lower_to) else {
        eprintln!("--lower-to takes allow, ask or deny");
        return ExitCode::from(2);
    };
    if let Some(why) = crate::util::StepCaller::from_env().refusal("lowering a launch floor") {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    if !at_terminal() {
        eprintln!(
            "lowering a launch floor is a person's decision: run `apb launch floor {id} --lower-to {to}` at an interactive terminal, or save the looser version in the dashboard editor"
        );
        return ExitCode::from(2);
    }
    let key = if global {
        FloorKey::global(id)
    } else {
        match FloorKey::project_of(root, id) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("cannot name the project: {e}");
                return ExitCode::from(2);
            }
        }
    };
    let now = floor::floor_of(Some(&key), dir.as_deref()).ok().flatten();
    let lines = vec![format!(
        "the launch floor of `{id}` is {}; lowering it to `{to}` lets agents launch it as `{to}` unless a version or a setting declares stricter",
        now.map(|p| p.as_str()).unwrap_or("none")
    )];
    if crate::consent::ask_terminal(&lines, "Lower it?") != Some(true) {
        eprintln!("not lowered");
        return ExitCode::from(2);
    }
    match floor::set_by_person(&key, to) {
        Ok(()) => {
            println!("launch floor of `{id}` set to {to}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot set the floor: {e}");
            ExitCode::from(2)
        }
    }
}
