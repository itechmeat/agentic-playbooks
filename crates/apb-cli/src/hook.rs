//! `apb hook <event>`: the commands an agent host runs on its own events.
//! `apb hook stop` is the end-of-turn save check of Claude Code's Stop hook
//! (`apb_core::save_nudge`), registered by `apb setup apply`. A hook never
//! fails its host: it reads its stdin (bounded), prints the host's answer
//! or nothing, and exits 0 whatever happens.

use std::io::Read;
use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum HookEvent {
    /// Claude Code's Stop hook: reminds the agent once per session to check
    /// the playbook catalog and offer to save a repeatable process.
    /// `APB_SAVE_NUDGE=off` silences it; `APB_SAVE_NUDGE_MIN_STEPS` and
    /// `APB_SAVE_NUDGE_MIN_KINDS` set its bounds (default 8 and 3)
    Stop,
    /// An event this apb does not know: nothing, exit 0
    #[command(external_subcommand)]
    Other(#[allow(dead_code)] Vec<String>),
}

/// Runs the hook of `event`; always exit 0.
pub fn run(event: &HookEvent) -> ExitCode {
    // A panic prints nothing to the host and still exits 0.
    std::panic::set_hook(Box::new(|_| {}));
    let out = std::panic::catch_unwind(|| match event {
        HookEvent::Stop => stop(),
        HookEvent::Other(_) => None,
    });
    if let Ok(Some(text)) = out {
        // A host that stopped reading is no failure: the write's error is
        // dropped (`println!` would panic on a closed pipe).
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{text}").and_then(|()| stdout.flush());
    }
    ExitCode::SUCCESS
}

fn stop() -> Option<String> {
    let host = apb_core::agents::lookup(apb_core::agents::claude::ID)?.stop_hook()?;
    if apb_core::save_nudge::off_here().is_some() {
        return None;
    }
    let mut stdin = Vec::new();
    std::io::stdin()
        .take(apb_core::save_nudge::MAX_STDIN_BYTES)
        .read_to_end(&mut stdin)
        .ok()?;
    let state = apb_core::save_nudge::state::dir()?;
    let mut ask = |request: &str, cwd: Option<&std::path::Path>| {
        apb_engine::decision::save_nudge::recurring(cwd?, &state, request)
    };
    apb_core::save_nudge::run_stop_hook(
        host,
        &stdin,
        &state,
        apb_core::save_nudge::Thresholds::from_env(),
        &mut ask,
        &|cwd| {
            cwd.map(|cwd| {
                apb_engine::external_data::flagged_markers(&apb_core::workspace::enclosing_roots(
                    cwd,
                ))
            })
            .unwrap_or_default()
        },
    )
}
