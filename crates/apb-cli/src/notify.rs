//! "Notify me" on the CLI: the notify flags of `apb run` (forwarded to the
//! detached supervised drive like the budget flags), `apb notify` (the
//! machine's settings, a run's notifications and a person's change on a
//! live run, `apb notify test`), and the notify lines of `apb runs <id>`.
//!
//! The arguments map to the same engine request and the same refusals MCP
//! and the HTTP API use (`apb_mcp::tools::notify`).

use std::path::Path;
use std::process::ExitCode;

use apb_core::notify::{NotifyRequest, NotifyTarget, Transition};
use apb_engine::notify::{ChangedBy, RunNotify};
use apb_mcp::tools::notify::{NotifyError, transitions};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, sanitize_for_terminal};

/// The notify flags of `apb run` and of the detached supervised drive.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct NotifyFlags {
    /// Tell me about this run (a review or a question waiting, a pause I
    /// did not make, the end) through the machine's notify target
    #[arg(long, conflicts_with = "no_notify")]
    pub notify: bool,
    /// No notifications for this run, whatever the machine default
    #[arg(long = "no-notify", conflicts_with_all = ["notify_on", "notify_via", "notify_to"])]
    pub no_notify: bool,
    /// Only these transitions, comma separated: review, question, paused,
    /// finished. Implies --notify
    #[arg(long = "notify-on", value_name = "LIST", value_delimiter = ',')]
    pub notify_on: Vec<String>,
    /// Send through this connector account instead of the machine's:
    /// <connector>/<account>[:function]. Implies --notify
    #[arg(long = "notify-via", value_name = "CONNECTOR/ACCOUNT[:FUNCTION]")]
    pub notify_via: Option<String>,
    /// The recipient (a chat id, an address) instead of the machine's.
    /// Implies --notify
    #[arg(long = "notify-to", value_name = "TO", allow_hyphen_values = true)]
    pub notify_to: Option<String>,
}

impl NotifyFlags {
    /// The run's notification request; `None` when no flag is set (the
    /// machine default and the project apply).
    pub(crate) fn request(&self) -> Result<Option<NotifyRequest>, String> {
        if self.no_notify {
            return Ok(Some(NotifyRequest::Off));
        }
        let asked = self.notify
            || !self.notify_on.is_empty()
            || self.notify_via.is_some()
            || self.notify_to.is_some();
        if !asked {
            return Ok(None);
        }
        let machine = machine_base(self.notify_via.as_deref(), self.notify_to.as_deref())?;
        on_request(
            (!self.notify_on.is_empty()).then_some(self.notify_on.as_slice()),
            self.notify_via.as_deref(),
            self.notify_to.as_deref(),
            machine.as_ref(),
        )
        .map(Some)
    }

    /// The flags that pass `req` on to a detached child. Each value is
    /// joined to its flag with `=`, so a recipient that starts with `-` (a
    /// chat group's id) is never read as a flag.
    pub(crate) fn args_of(req: &NotifyRequest) -> Vec<String> {
        let NotifyRequest::On { target, on } = req else {
            return vec!["--no-notify".into()];
        };
        let mut out = vec!["--notify".to_string()];
        if let Some(on) = on {
            let names: Vec<&str> = on.iter().map(|t| t.as_str()).collect();
            out.push(format!("--notify-on={}", names.join(",")));
        }
        if let Some(t) = target {
            out.extend([
                format!("--notify-via={}/{}:{}", t.connector, t.account, t.function),
                format!("--notify-to={}", t.to),
            ]);
        }
        out
    }
}

/// An `On` request from the flag values: the transitions, and a target
/// built over `base` (the run's current target, or the machine's) from
/// `via` and `to`.
fn on_request(
    on: Option<&[String]>,
    via: Option<&str>,
    to: Option<&str>,
    base: Option<&NotifyTarget>,
) -> Result<NotifyRequest, String> {
    let on = on
        .map(|names| {
            let names: Vec<String> = names
                .iter()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            transitions(&names)
        })
        .transpose()
        .map_err(|e| e.detail().to_string())?;
    let req = NotifyRequest::On {
        target: target_of(via, to, base)?,
        on,
    };
    req.validate()?;
    Ok(req)
}

/// The target `--notify-via` and `--notify-to` name, filled from `base`
/// (the run's current target, or the machine's) where they leave a part
/// out; `None` when neither is given (the base applies as it is).
fn target_of(
    via: Option<&str>,
    to: Option<&str>,
    base: Option<&NotifyTarget>,
) -> Result<Option<NotifyTarget>, String> {
    if via.is_none() && to.is_none() {
        return Ok(None);
    }
    let (connector, account, function) = match via {
        Some(v) => parse_via(v)?,
        None => {
            let b = base.ok_or(
                "--notify-to needs --notify-via: no notify target is configured on this machine",
            )?;
            (
                b.connector.clone(),
                b.account.clone(),
                Some(b.function.clone()),
            )
        }
    };
    // The base's function and recipient only fit its own connector account.
    let same = base.filter(|b| b.connector == connector && b.account == account);
    let function = match (function, same) {
        (Some(f), _) => f,
        (None, Some(b)) => b.function.clone(),
        (None, None) => {
            return Err(format!(
                "name the function: --notify-via {connector}/{account}:<function>"
            ));
        }
    };
    let to = match (to, same) {
        (Some(t), _) => t.to_string(),
        (None, Some(b)) => b.to.clone(),
        (None, None) => return Err("name the recipient with --notify-to".into()),
    };
    Ok(Some(NotifyTarget {
        connector,
        account,
        function,
        to,
    }))
}

/// The machine's target, read only when `via` and `to` name part of a
/// target and leave another part out: a settings file that does not read
/// stops nothing that does not need it.
fn machine_base(via: Option<&str>, to: Option<&str>) -> Result<Option<NotifyTarget>, String> {
    let named = via.is_some() || to.is_some();
    let complete = to.is_some() && via.is_some_and(|v| v.contains(':'));
    if !named || complete {
        return Ok(None);
    }
    Ok(apb_core::config::GlobalConfig::load()?.notify.target)
}

/// `<connector>/<account>[:function]`.
fn parse_via(v: &str) -> Result<(String, String, Option<String>), String> {
    let (pair, function) = match v.split_once(':') {
        Some((p, f)) => (p, Some(f.trim().to_string())),
        None => (v, None),
    };
    let (connector, account) = pair.split_once('/').ok_or_else(|| {
        format!("bad --notify-via `{v}` (expected <connector>/<account>[:function])")
    })?;
    Ok((
        connector.trim().to_string(),
        account.trim().to_string(),
        function,
    ))
}

/// The lines of a run's `notify` block, for a terminal.
pub(crate) fn block_lines(block: &RunNotify) -> Vec<String> {
    let clean = |s: &str| sanitize_for_terminal(s, QUESTION_TEXT_MAX);
    let mut lines = vec![match (&block.target, block.active) {
        (Some(t), true) => format!(
            "notify: on, {}/{} {} to {}, about {}{}",
            clean(&t.connector),
            clean(&t.account),
            clean(&t.function),
            clean(&t.to),
            on_text(&block.on),
            block
                .source
                .as_deref()
                .map(|s| format!(" (from {})", clean(s)))
                .unwrap_or_default()
        ),
        _ => "notify: off".to_string(),
    }];
    for s in &block.sent {
        lines.push(format!("  notified {}: {}", clean(&s.transition), s.state));
    }
    for p in &block.problems {
        lines.push(format!("  notify problem: {}", clean(p)));
    }
    lines
}

fn on_text(on: &[Transition]) -> String {
    on.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ")
}

/// Prints the warnings of a start's notify check, or refuses the start.
pub(crate) fn start_check(root: &Path, req: Option<&NotifyRequest>) -> Result<(), String> {
    match apb_mcp::tools::notify::start_check(root, req) {
        Ok(warnings) => {
            for w in warnings {
                eprintln!("warning: {}", sanitize_for_terminal(&w, QUESTION_TEXT_MAX));
            }
            Ok(())
        }
        Err(e) => Err(format!(
            "{}: {}",
            apb_engine::notify::REFUSED_START,
            e.detail()
        )),
    }
}

/// What `apb notify` was asked to do with a run.
pub(crate) struct NotifyChange<'a> {
    /// `Some("")` turns notifications on with the default transitions;
    /// `Some("review,finished")` narrows them.
    pub on: Option<&'a str>,
    pub off: bool,
    pub via: Option<&'a str>,
    pub to: Option<&'a str>,
}

impl NotifyChange<'_> {
    fn is_empty(&self) -> bool {
        self.on.is_none() && !self.off && self.via.is_none() && self.to.is_none()
    }

    /// The engine request, a target left out filled from `base`.
    fn request(&self, base: Option<&NotifyTarget>) -> Result<NotifyRequest, String> {
        if self.off {
            return Ok(NotifyRequest::Off);
        }
        let on: Option<Vec<String>> = self
            .on
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.split(',').map(str::to_string).collect());
        on_request(on.as_deref(), self.via, self.to, base)
    }
}

/// `apb notify [<run> | test] [--on [LIST] | --off] [--via ...] [--to ...]`.
pub(crate) fn notify_cmd(root: &Path, run: Option<&str>, change: &NotifyChange<'_>) -> ExitCode {
    match run {
        None if change.is_empty() => machine_cmd(root),
        None => {
            eprintln!("notify: name the run to change (`apb notify <run-id> ...`)");
            ExitCode::from(2)
        }
        Some(run_id) if change.is_empty() && run_id != "test" => show_cmd(root, run_id),
        Some(run_id) => {
            // Who hears about a run, and through which account, is the
            // person's to decide: a step of a run cannot redirect, silence
            // or send through their notifications.
            if let Some(step) = StepCaller::from_env().describe() {
                eprintln!(
                    "notify: refused: this comes from {step}, and only a person changes or tests notifications; ask the person to run it from a terminal or use the dashboard"
                );
                return ExitCode::from(2);
            }
            if run_id == "test" {
                test_cmd(root, change)
            } else {
                set_cmd(root, run_id, change)
            }
        }
    }
}

/// `apb notify`: the machine's notify settings and this project's default.
fn machine_cmd(root: &Path) -> ExitCode {
    let cfg = match apb_core::config::GlobalConfig::load() {
        Ok(c) => c.notify,
        Err(e) => {
            eprintln!("notify: {e}");
            return ExitCode::from(2);
        }
    };
    let clean = |s: &str| sanitize_for_terminal(s, QUESTION_TEXT_MAX);
    match &cfg.target {
        Some(t) => println!(
            "target: {}/{} {} to {}",
            clean(&t.connector),
            clean(&t.account),
            clean(&t.function),
            clean(&t.to)
        ),
        None => println!("target: none (set notify.target in the machine config.yaml)"),
    }
    println!("about: {}", on_text(&Transition::normalized(&cfg.on)));
    let project = match apb_core::config::project_notify(root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "notify: {}",
                sanitize_for_terminal(&e, QUESTION_TEXT_MAX * 2)
            );
            return ExitCode::from(2);
        }
    };
    let by_default = apb_core::notify::effective(&cfg, &project, None).is_some();
    println!(
        "runs here notify {}",
        if by_default {
            "by default"
        } else {
            "only when asked (--notify)"
        }
    );
    ExitCode::SUCCESS
}

/// `apb notify test`: one live message through the target.
fn test_cmd(root: &Path, change: &NotifyChange<'_>) -> ExitCode {
    if change.on.is_some() || change.off {
        eprintln!("notify test takes only --via and --to");
        return ExitCode::from(2);
    }
    let base = machine_base(change.via, change.to);
    let target = match base.and_then(|m| target_of(change.via, change.to, m.as_ref())) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("notify test failed: {e}");
            return ExitCode::from(2);
        }
    };
    match apb_engine::notify::send_trial(root, target) {
        Ok(t) => {
            let clean = |s: &str| sanitize_for_terminal(s, QUESTION_TEXT_MAX);
            println!(
                "test notification sent through {}/{} {} to {}",
                clean(&t.connector),
                clean(&t.account),
                clean(&t.function),
                clean(&t.to)
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "notify test failed: {}",
                sanitize_for_terminal(&e, QUESTION_TEXT_MAX * 2)
            );
            ExitCode::from(1)
        }
    }
}

/// `apb notify <run>`: the run's notifications.
fn show_cmd(root: &Path, run_id: &str) -> ExitCode {
    let Some((dir, events)) = run_events(root, run_id) else {
        return ExitCode::from(2);
    };
    match apb_mcp::tools::notify::run_block(&dir, &events) {
        Some(block) => block_lines(&block).iter().for_each(|l| println!("{l}")),
        None => println!("notify: never set for this run"),
    }
    ExitCode::SUCCESS
}

/// The directory and the journal of run `run_id`; `None` (said on stderr)
/// when there is no such run or it cannot be read.
fn run_events(
    root: &Path,
    run_id: &str,
) -> Option<(std::path::PathBuf, Vec<apb_engine::event::Event>)> {
    let dir = root.join(".apb/runs").join(run_id);
    if !apb_core::registry::is_safe_segment(run_id) || !dir.is_dir() {
        eprintln!(
            "notify: run `{}` not found",
            sanitize_for_terminal(run_id, QUESTION_TEXT_MAX)
        );
        return None;
    }
    match apb_engine::event::read_all_lossy_tail(&dir) {
        Ok(events) => Some((dir, events)),
        Err(e) => {
            eprintln!(
                "notify: cannot read run `{run_id}`: {}",
                sanitize_for_terminal(&e.to_string(), QUESTION_TEXT_MAX * 2)
            );
            None
        }
    }
}

/// `apb notify <run> --on/--off/--via/--to`: a person's change. A part of
/// the target left out comes from the run's current target, else the
/// machine's.
fn set_cmd(root: &Path, run_id: &str, change: &NotifyChange<'_>) -> ExitCode {
    let Some((dir, events)) = run_events(root, run_id) else {
        return ExitCode::from(2);
    };
    let current = apb_mcp::tools::notify::run_block(&dir, &events).and_then(|b| b.target);
    let base = match current {
        Some(t) => Ok(Some(t)),
        None => machine_base(change.via, change.to),
    };
    let base = match base {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "notify: {}",
                sanitize_for_terminal(&e, QUESTION_TEXT_MAX * 2)
            );
            return ExitCode::from(2);
        }
    };
    let req = match change.request(base.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("notify: {e}");
            return ExitCode::from(2);
        }
    };
    match apb_mcp::tools::notify::set(root, run_id, &req, ChangedBy::Person) {
        Ok(out) => {
            let events = apb_engine::event::read_all_lossy_tail(&dir).unwrap_or_default();
            match apb_mcp::tools::notify::run_block(&dir, &events) {
                Some(b) => block_lines(&b).iter().for_each(|l| println!("{l}")),
                None => println!("notify: off"),
            }
            for w in out["warnings"].as_array().into_iter().flatten() {
                eprintln!(
                    "warning: {}",
                    sanitize_for_terminal(w.as_str().unwrap_or(""), QUESTION_TEXT_MAX)
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let what = match &e {
                NotifyError::Unavailable(_) => "notifications cannot be sent",
                NotifyError::Ended(_) => "the run has ended",
                _ => "refused",
            };
            eprintln!(
                "notify: {what}: {}",
                sanitize_for_terminal(e.detail(), QUESTION_TEXT_MAX * 2)
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flag_asks_for_nothing_and_no_notify_turns_it_off() {
        assert_eq!(NotifyFlags::default().request().unwrap(), None);
        let off = NotifyFlags {
            no_notify: true,
            ..NotifyFlags::default()
        };
        assert_eq!(off.request().unwrap(), Some(NotifyRequest::Off));
    }

    #[test]
    fn a_request_survives_the_round_trip_to_a_detached_child() {
        #[derive(clap::Parser)]
        struct Probe {
            #[command(flatten)]
            flags: NotifyFlags,
        }
        // A chat group's id starts with `-`.
        for to in ["123", "-1001234567890"] {
            let req = NotifyRequest::On {
                target: Some(NotifyTarget {
                    connector: "telegram".into(),
                    account: "me".into(),
                    function: "send_message".into(),
                    to: to.into(),
                }),
                on: Some(vec![Transition::Review, Transition::Finished]),
            };
            let args = NotifyFlags::args_of(&req);
            let parsed = <Probe as clap::Parser>::try_parse_from(
                std::iter::once("probe".to_string()).chain(args),
            )
            .unwrap();
            assert_eq!(parsed.flags.request().unwrap(), Some(req));
        }
        // Typed by a person with a space, too.
        let typed = <Probe as clap::Parser>::try_parse_from([
            "probe",
            "--notify-via",
            "telegram/me:send_message",
            "--notify-to",
            "-100123",
        ])
        .unwrap();
        assert_eq!(typed.flags.notify_to.as_deref(), Some("-100123"));
        assert_eq!(
            NotifyFlags::args_of(&NotifyRequest::Off),
            vec!["--no-notify".to_string()]
        );
    }

    #[test]
    fn a_left_out_part_comes_from_the_base_only_for_the_same_account() {
        let base = NotifyTarget {
            connector: "telegram".into(),
            account: "me".into(),
            function: "send_message".into(),
            to: "42".into(),
        };
        let same = target_of(Some("telegram/me"), None, Some(&base)).unwrap();
        assert_eq!(same, Some(base.clone()));
        let only_to = target_of(None, Some("7"), Some(&base)).unwrap().unwrap();
        assert_eq!(
            (only_to.connector.as_str(), only_to.to.as_str()),
            ("telegram", "7")
        );
        // Another account of the same connector needs its own recipient.
        let err = target_of(Some("telegram/work:send_message"), None, Some(&base)).unwrap_err();
        assert!(err.contains("--notify-to"), "{err}");
    }

    #[test]
    fn via_names_connector_account_and_an_optional_function() {
        assert_eq!(
            parse_via("telegram/me:send_message").unwrap(),
            ("telegram".into(), "me".into(), Some("send_message".into()))
        );
        assert_eq!(
            parse_via("slack/team").unwrap(),
            ("slack".into(), "team".into(), None)
        );
        assert!(parse_via("telegram").is_err());
    }
}
