//! The irreversible consent of `apb run` and `apb resume` (0.24.0).
//!
//! A tree that declares `irreversible` needs one of:
//! - `--confirm-irreversible=<consent_nonce>` (`by: cli_flag`): the nonce the
//!   refusal printed, which binds the consent to that version of the tree. A
//!   bare `--confirm-irreversible` is accepted for one release with a
//!   deprecation warning.
//! - A `y` to the `[y/N]` question asked on the controlling terminal
//!   (`by: cli`), after the sources are printed. It is asked only when stdin
//!   and stderr are terminals and `/dev/tty` opens, and never inside a run
//!   (`APB_RUN_ID` set): a process that merely inherited a terminal is not a
//!   person.
//!
//! Anything else is refused with the sources and the nonce.

use apb_engine::consent::Confirmation;
use apb_engine::gate::ConsentNeed;

/// How a CLI start obtains the consent.
pub(crate) enum CliConsent {
    /// `apb run` / `apb resume`: the flag's value when given, otherwise the
    /// terminal question.
    Ask { flag: Option<String> },
    /// `__drive-supervised`: the consent the parent `apb run --supervise`
    /// obtained, with the nonce of the tree it was given for.
    Forwarded { by: String, nonce: String },
    /// No consent can be given (a start path with no person behind it).
    None,
}

/// The consent the CLI obtained: who gave it and for which tree.
pub(crate) struct Granted {
    pub by: String,
    pub nonce: String,
}

/// Obtains the consent `need` asks for, or returns the refusal message.
pub(crate) fn obtain(need: &ConsentNeed, how: &CliConsent) -> Result<Granted, String> {
    let granted = |by: &str| Granted {
        by: by.to_string(),
        nonce: need.nonce(),
    };
    match how {
        CliConsent::Ask { flag: Some(value) } => {
            let note = need
                .check(Some(&Confirmation::from_cli(value)))
                .map_err(|r| refusal_message(&r))?;
            if let Some(note) = note {
                eprintln!("warning: {note}");
            }
            Ok(granted("cli_flag"))
        }
        CliConsent::Ask { flag: None } => {
            let refusal = need.check(None).expect_err("no confirmation is a refusal");
            if std::env::var_os("APB_RUN_ID").is_some_and(|v| !v.is_empty()) {
                return Err(format!(
                    "{} (APB_RUN_ID is set: a start from inside a run cannot consent at the terminal; pass --confirm-irreversible=<consent_nonce>)",
                    refusal_message(&refusal)
                ));
            }
            match ask(need) {
                Some(true) => Ok(granted("cli")),
                Some(false) => Err(format!(
                    "run refused ({}): not confirmed",
                    apb_engine::consent::REFUSAL_POLICY
                )),
                None => Err(refusal_message(&refusal)),
            }
        }
        CliConsent::Forwarded { by, nonce } => {
            need.check(Some(&Confirmation::Nonce(nonce.clone())))
                .map_err(|r| refusal_message(&r))?;
            Ok(granted(by))
        }
        CliConsent::None => Err(refusal_message(
            &need.check(None).expect_err("no confirmation is a refusal"),
        )),
    }
}

/// Prints the sources and asks `[y/N]` on the controlling terminal.
/// `None` when there is no person to ask (stdin or stderr is not a terminal,
/// or the terminal does not open).
fn ask(need: &ConsentNeed) -> Option<bool> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }
    let tty = open_tty()?;
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "playbook `{}` has irreversible effects:",
        need.playbook_id
    );
    for s in &need.sources {
        let _ = writeln!(err, "  - {s}");
    }
    let _ = write!(err, "Run it? [y/N] ");
    let _ = err.flush();
    let mut line = String::new();
    std::io::BufReader::new(tty).read_line(&mut line).ok()?;
    let answer = line.trim();
    Some(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

#[cfg(unix)]
fn open_tty() -> Option<std::fs::File> {
    std::fs::File::open("/dev/tty").ok()
}

#[cfg(not(unix))]
fn open_tty() -> Option<std::fs::File> {
    // No controlling-terminal device: the Windows console input handle.
    std::fs::File::open("CONIN$").ok()
}

/// The CLI text of an `irreversible_requires_confirmation` refusal: what is
/// irreversible, the nonce and how to confirm it.
pub(crate) fn refusal_message(refusal: &serde_json::Value) -> String {
    let policy = refusal
        .get("policy")
        .and_then(|v| v.as_str())
        .unwrap_or(apb_engine::consent::REFUSAL_POLICY);
    let sources: Vec<&str> = refusal
        .get("sources")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let nonce = refusal
        .get("consent_nonce")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let mut msg = format!(
        "run refused ({policy}): irreversible effects ({})",
        sources.join(", ")
    );
    if let Some(reason) = refusal.get("reason").and_then(|v| v.as_str()) {
        msg.push_str(&format!("; {reason}"));
    }
    msg.push_str(&format!(
        ". Run it from a terminal to be asked, or confirm with --confirm-irreversible={nonce} (consent_nonce: {nonce}). A triggered or headless start cannot consent"
    ));
    msg
}
