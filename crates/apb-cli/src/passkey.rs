//! `apb passkey`: enroll, list and remove the passkeys that gate
//! person-only decisions and approvals on this machine.
//!
//! Enrollment is the protected step: the first passkey is enrolled only
//! here, by a person at a terminal outside a run step, which prints a
//! one-time code the dashboard's Passkeys page asks for. Every later
//! passkey is added through the dashboard with an assertion from one that
//! exists. Removing the last passkey needs a one-time code typed back at
//! the terminal, so a process that can run commands cannot quietly take
//! the gate off the machine. Every change sends a notice through the
//! machine's notify target when one is set, and journals nothing: no run
//! owns it.

use std::path::Path;
use std::process::ExitCode;

use apb_core::passkey;
use apb_core::step_env::StepCaller;

use crate::util::sanitize_for_terminal;

/// What `apb passkey` was asked to do.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum PasskeyAction {
    /// Enroll the first passkey: prints a one-time code the dashboard's
    /// Passkeys page asks for, valid for five minutes
    Enroll {
        /// A name for the new passkey the page suggests
        #[arg(default_value = "laptop", value_name = "LABEL")]
        label: String,
    },
    /// List the passkeys enrolled on this machine
    List,
    /// Remove the passkey with this label; the last one needs a one-time
    /// code typed back at the terminal
    Remove { label: String },
}

/// The machine's host name, for the notices.
fn machine_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this machine".to_string())
}

/// Sends the passkey notice through the machine's notify target,
/// best-effort: quiet when notifications are off, never a fault of the
/// change itself.
fn send_notice(root: &Path, text: &str) {
    if let Err(why) = apb_engine::notify::trial::send_notice(root, "apb passkeys", text) {
        eprintln!("apb: notice not sent: {why}");
    }
}

pub(crate) fn passkey_cmd(root: &Path, action: PasskeyAction) -> ExitCode {
    // Enrollment and removal are what an agent must not be able to do
    // quietly: a run step is never a person for them, as for the other
    // person-only commands.
    if let Some(from) = StepCaller::from_env().describe() {
        eprintln!(
            "passkey enrollment and removal are refused {from}: only a person at a terminal manages the passkeys that gate person-only decisions; do it outside a run"
        );
        return ExitCode::from(2);
    }
    match action {
        PasskeyAction::List => list(),
        PasskeyAction::Enroll { label } => enroll(label),
        PasskeyAction::Remove { label } => remove(root, &label),
    }
}

fn list() -> ExitCode {
    let all = match passkey::try_load() {
        Ok(f) => f.passkeys,
        Err(e) => {
            eprintln!("passkey list failed: {e}");
            return ExitCode::from(2);
        }
    };
    if all.is_empty() {
        println!("no passkeys enrolled on this machine.");
        println!("run `apb passkey enroll` to require one for person-only gates and approvals.");
        return ExitCode::SUCCESS;
    }
    println!("passkeys enrolled on this machine:");
    for p in &all {
        println!(
            "  {}  alg {}  added {}  credential {}...",
            p.label, p.alg, p.created_at, p.id
        );
    }
    ExitCode::SUCCESS
}

fn enroll(label: String) -> ExitCode {
    let dir = match apb_core::config::config_dir() {
        Some(d) => d,
        None => {
            eprintln!("passkey enroll failed: no config directory; set HOME or APB_CONFIG_DIR");
            return ExitCode::from(2);
        }
    };
    if passkey::count_strict().unwrap_or(1) > 0 {
        eprintln!(
            "a passkey is already enrolled on this machine: every later passkey needs an assertion from one that exists. Use the Add button on the dashboard's Passkeys page."
        );
        return ExitCode::from(2);
    }
    // The code is minted here but only becomes live after a person at this
    // terminal typed it back: an agent that runs this command with a piped
    // stdout and a captured code cannot activate it, because it cannot
    // answer the typed confirmation (the same gate as --feedback-loop yes).
    let code = passkey::new_enroll_code();
    println!("Enrolling the first passkey for person-only gates and approvals.");
    println!(
        "Open the dashboard's Passkeys page and enter this one-time code within five minutes:"
    );
    println!();
    println!("    {code}");
    println!();
    println!(
        "The page then asks your authenticator to register a passkey with the label `{}`.",
        sanitize_for_terminal(&label, 100)
    );
    let typed = crate::consent::ask_typed(
        &["Only a person at this terminal may enroll the first passkey.".to_string()],
        "Type the one-time code shown above to activate it (anything else cancels):",
    );
    if typed.as_deref() != Some(code.as_str()) {
        eprintln!("not confirmed: the code was not written, so nothing can enroll with it");
        return ExitCode::from(2);
    }
    if let Err(e) = passkey::store_enroll_code(&dir, &code) {
        eprintln!("passkey enroll failed: {e}");
        return ExitCode::from(2);
    }
    println!("The code is live. Finish the registration on the dashboard within five minutes.");
    println!("Until a passkey is enrolled, person-only gates and approvals decide as before.");
    ExitCode::SUCCESS
}

fn remove(root: &Path, label: &str) -> ExitCode {
    let path = match passkey::passkey_file_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("passkey remove failed: {e}");
            return ExitCode::from(2);
        }
    };
    let all = passkey::list_from(&path).unwrap_or_default();
    if !all.iter().any(|p| p.label == label) {
        eprintln!(
            "passkey remove failed: no passkey with label `{}` on this machine",
            sanitize_for_terminal(label, 100)
        );
        return ExitCode::from(2);
    }
    // Every removal, last or not, needs a code typed back at the terminal:
    // the same rule the dashboard enforces with an assertion, so a process
    // that can run commands cannot quietly take a passkey off the machine.
    {
        let mut code_bytes = [0u8; 6];
        if getrandom::fill(&mut code_bytes).is_err() {
            eprintln!("passkey remove failed: no randomness from the operating system");
            return ExitCode::from(2);
        }
        let alphabet = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let code: String = code_bytes
            .iter()
            .map(|b| alphabet[(*b as usize) % alphabet.len()] as char)
            .collect();
        let question = format!(
            "Remove the passkey `{}` from this machine? Type this code to confirm: {code}",
            sanitize_for_terminal(label, 100)
        );
        match crate::consent::ask_typed(
            &["Only a person at this terminal removes a passkey.".to_string()],
            &question,
        ) {
            Some(typed) if typed == code => {}
            _ => {
                eprintln!("not confirmed: nothing was removed");
                return ExitCode::from(2);
            }
        }
    }
    match passkey::remove_by_label_into(&path, label, true) {
        Ok(r) => {
            println!("removed passkey `{}` from this machine", r.label);
            send_notice(
                root,
                &format!(
                    "a passkey was removed on {}: label `{}`",
                    machine_name(),
                    r.label
                ),
            );
            if passkey::count_strict().unwrap_or(1) == 0 {
                println!(
                    "no passkey is enrolled now: person-only gates and approvals decide as before (a signed-in dashboard session alone)."
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("passkey remove failed: {e}");
            ExitCode::from(2)
        }
    }
}
