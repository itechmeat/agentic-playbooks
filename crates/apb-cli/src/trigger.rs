//! `apb trigger`: the person's command over the start rules of the project
//! (`docs/TRIGGERS.md`, design D1 "Commands" and D0 "A person's command"
//! item 1). Writing, approving, enabling, disabling and removing a rule are
//! a person's: each starts with the run-step refusal, then asks for the
//! rule's name typed at a terminal, and none takes `--yes` or a passkey.
//! `list`, `show`, `log` and `test` only read; `test` is refused from a run
//! step too, because its dry start would run the start checks as the
//! step's process.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::scope::Scope;
use apb_core::signed_store::read as read_signed;
use apb_core::trigger::{Rule, RuleEntry, RulesFile, SetOutcome, TriggerError, TriggerStateFile};
use clap::{Args, Subcommand};

use crate::consent;
use crate::util::print_json;

/// What a CLI channel calls itself in the store (`updated_by`,
/// `approved_by`): the terminal is the person's channel.
const TERMINAL: &str = "terminal";

/// How many journal lines `show` and a bare `log` keep.
const LOG_TAIL: usize = 20;

/// How `apb trigger` was asked.
#[derive(Args)]
pub(crate) struct TriggerArgs {
    #[command(subcommand)]
    action: TriggerAction,
}

#[derive(Subcommand)]
enum TriggerAction {
    /// The rules of this project: enabled, source, target, the last verdict
    List {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// One rule: its YAML, its incarnation and digest, its approval, the
    /// position and the last 20 journal lines
    Show {
        rule: String,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Write (create or replace) a rule from a YAML file; the file's `name`
    /// must equal the rule named here; a person's
    Set {
        rule: String,
        /// The rule document as YAML (`--file <path>`)
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
    },
    /// Approve a rule for its current binding, so it starts its playbook;
    /// a person's
    Approve { rule: String },
    /// Enable a rule (drops its state, so the next pass positions it at the
    /// head of its source); a person's
    Enable { rule: String },
    /// Disable a rule (it evaluates nothing until enabled again); a
    /// person's
    Disable { rule: String },
    /// Remove a rule with its state and its journal; a person's
    Remove { rule: String },
    /// Take one stored event through the rule and the checks a start would
    /// run before preparation; writes nothing. Exit 0 when the event
    /// matches and passes them, 3 when it does not, 2 on an error
    Test {
        rule: String,
        /// The event to test (default: the newest stored one)
        #[arg(long, value_name = "SEQ")]
        event: Option<u64>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// A rule's trigger journal (newest last), codes only
    Log {
        rule: String,
        /// At most this many lines (default: the last 20)
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn trigger_cmd(root: &Path, args: TriggerArgs) -> ExitCode {
    match args.action {
        TriggerAction::List { json } => list_cmd(root, json),
        TriggerAction::Show { rule, json } => show_cmd(root, &rule, json),
        TriggerAction::Set { rule, file } => set_cmd(root, &rule, &file),
        TriggerAction::Approve { rule } => approve_cmd(root, &rule),
        TriggerAction::Enable { rule } => enabled_cmd(root, &rule, true),
        TriggerAction::Disable { rule } => enabled_cmd(root, &rule, false),
        TriggerAction::Remove { rule } => remove_cmd(root, &rule),
        TriggerAction::Test { rule, event, json } => test_cmd(root, &rule, event, json),
        TriggerAction::Log { rule, limit, json } => log_cmd(root, &rule, limit, json),
    }
}

fn ok() -> ExitCode {
    ExitCode::SUCCESS
}

fn refused() -> ExitCode {
    ExitCode::from(2)
}

fn invalid() -> ExitCode {
    ExitCode::from(1)
}

/// The run-step refusal of every mutation (D0 "A person's command" item 1):
/// a step never decides what a person decides. Prints and returns true when
/// this process is a step of a run.
fn step_refused(what: &str) -> bool {
    if let Some(why) = apb_core::step_env::StepCaller::from_env().refusal(what) {
        eprintln!("{why}");
        return true;
    }
    false
}

/// The terminal is missing: the sentence every mutation answers with.
const NO_TERMINAL: &str =
    "a person changes a rule at an interactive terminal or in a signed-in dashboard";

/// How the typed confirmation went.
enum Ask {
    Confirmed,
    NoTerminal,
    WrongName,
}

/// Asks for the rule's name typed at the terminal (D0 item 1). No passkey,
/// no `--yes`: the name is the confirmation.
fn ask_for_name(rule: &str, verb: &str) -> Ask {
    match consent::ask_typed(&[], &format!("Type the rule's name to {verb} it: ")) {
        None => Ask::NoTerminal,
        Some(typed) if typed == rule => Ask::Confirmed,
        Some(_) => Ask::WrongName,
    }
}

/// Answers the confirmation with the sentence D0 item 1 gives, on stderr,
/// and returns the exit code.
fn confirm_refused(ask: Ask, rule: &str) -> ExitCode {
    match ask {
        Ask::Confirmed => ok(),
        Ask::NoTerminal => {
            eprintln!("{NO_TERMINAL}");
            refused()
        }
        Ask::WrongName => {
            eprintln!("not confirmed: the name typed is not `{rule}`; nothing was changed");
            refused()
        }
    }
}

/// The entry of `rule` in the signed rules file; the error paths of every
/// read-only command.
fn entry_of(root: &Path, rule: &str) -> Result<RuleEntry, String> {
    let loaded = read_signed::<RulesFile>(root, Scope::Project, "rules")
        .map_err(|e| format!("the project's rules file does not read: {e}"))?;
    if !loaded.signed {
        return Err(apb_core::trigger::rules_unverified_text(
            &apb_core::signed_store::file::<RulesFile>(root, Scope::Project, "rules"),
        ));
    }
    loaded
        .file
        .rules
        .into_iter()
        .find(|e| e.rule.name == rule)
        .ok_or_else(|| {
            format!("no rule `{rule}` in this project (apb trigger list shows the rules)")
        })
}

fn set_cmd(root: &Path, rule: &str, file: &Path) -> ExitCode {
    if step_refused("changing a trigger rule") {
        return refused();
    }
    let doc = match std::fs::read_to_string(file) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("cannot read {}: {e}", file.display());
            return refused();
        }
    };
    // The document as the store parses it, so every refusal of `set`
    // happens before the terminal is asked.
    let parsed: Rule = match apb_core::trigger::parse_rule_document(&doc) {
        Ok(parsed) => parsed,
        Err(apb_core::trigger::TriggerError::EnabledNotSettable) => {
            eprintln!(
                "trigger_enabled_not_settable: the rule document names `enabled`; only `apb trigger enable` and `apb trigger disable` change it"
            );
            return refused();
        }
        Err(apb_core::trigger::TriggerError::Invalid(issues)) => {
            for issue in &issues {
                println!("rule {rule}: error {} {}", issue.code, issue.message);
            }
            return invalid();
        }
        Err(other) => {
            println!("rule {rule}: error V260 the rule document does not parse: {other}");
            return invalid();
        }
    };
    if parsed.name != rule {
        eprintln!(
            "the rule document names `{}`; apb trigger set writes the rule `{rule}`; nothing was changed",
            parsed.name
        );
        return refused();
    }
    if apb_core::projects::registered_id(root).is_none() {
        eprintln!(
            "trigger_project_unregistered: this project has no registered workspace id, so no listener would sweep a rule saved here; run any apb command in the project to register it"
        );
        return refused();
    }
    // A rules file that does not verify refuses every write: said before
    // the ask, with the one remedy that works.
    if let Ok(loaded) = read_signed::<RulesFile>(root, Scope::Project, "rules")
        && !loaded.signed
    {
        eprintln!(
            "{}",
            apb_core::trigger::rules_unverified_text(&apb_core::signed_store::file::<RulesFile>(
                root,
                Scope::Project,
                "rules"
            ))
        );
        return refused();
    }
    // Every issue, printed before the ask; an error stops here.
    let issues = apb_core::trigger::check_rule(
        &parsed,
        &apb_core::trigger::RuleContext::load(root, &parsed),
    );
    for issue in &issues {
        let sev = match issue.severity {
            apb_core::validate::Severity::Error => "error",
            apb_core::validate::Severity::Warning => "warning",
        };
        println!("rule {rule}: {sev} {} {}", issue.code, issue.message);
    }
    if issues
        .iter()
        .any(|i| i.severity == apb_core::validate::Severity::Error)
    {
        return invalid();
    }
    match confirm_refused(ask_for_name(rule, "set"), rule) {
        code if code == ok() => {}
        code => return code,
    }
    match apb_core::trigger::set_rule(root, &doc, TERMINAL, apb_core::clock::Millis::now()) {
        Ok((outcome, warnings)) => {
            for issue in &warnings {
                let sev = match issue.severity {
                    apb_core::validate::Severity::Error => "error",
                    apb_core::validate::Severity::Warning => "warning",
                };
                println!("rule {rule}: {sev} {} {}", issue.code, issue.message);
            }
            match outcome {
                SetOutcome::Created => println!("rule `{rule}` created"),
                SetOutcome::Updated { approval_dropped } => {
                    if approval_dropped {
                        println!("approval dropped");
                    }
                }
                SetOutcome::Unchanged => println!("rule `{rule}` is unchanged"),
            }
            if matches!(outcome, SetOutcome::Created | SetOutcome::Updated { .. }) {
                println!("needs approval: apb trigger approve {rule}");
            }
            ok()
        }
        Err(TriggerError::Invalid(issues)) => {
            for issue in &issues {
                let sev = match issue.severity {
                    apb_core::validate::Severity::Error => "error",
                    apb_core::validate::Severity::Warning => "warning",
                };
                println!("rule {rule}: {sev} {} {}", issue.code, issue.message);
            }
            invalid()
        }
        Err(e) => {
            eprintln!("{e}");
            refused()
        }
    }
}

fn approve_cmd(root: &Path, rule: &str) -> ExitCode {
    if step_refused("approving a trigger rule") {
        return refused();
    }
    let entry = match entry_of(root, rule) {
        Ok(entry) => entry,
        Err(e) => {
            eprintln!("{e}");
            return refused();
        }
    };
    let need = match apb_mcp::tools::trigger::approval_need(root, &entry) {
        Ok(need) => need,
        Err(v) => {
            eprintln!("{}", refusal_text(&v));
            return refused();
        }
    };
    // What a person approves, as YAML, then the one sentence about what it
    // means (design D1 "Commands" item 4).
    match yaml_serde::to_string(&need) {
        Ok(yaml) => print!("{yaml}"),
        Err(e) => eprintln!("cannot print the approval need: {e}"),
    }
    println!(
        "Approving rule `{rule}` lets it start {} {} with nobody at the keyboard.",
        need.playbook, need.version
    );
    match confirm_refused(ask_for_name(rule, "approve"), rule) {
        code if code == ok() => {}
        code => return code,
    }
    match apb_mcp::tools::trigger::approve(root, rule, &need, TERMINAL) {
        Ok(_) => {
            println!(
                "rule `{rule}` approved; its next matching event starts {} {}",
                need.playbook, need.version
            );
            ok()
        }
        Err(v) => {
            eprintln!("{}", refusal_text(&v));
            refused()
        }
    }
}

/// A refusal value as one line (`<policy>: <detail>`).
fn refusal_text(v: &serde_json::Value) -> String {
    format!(
        "{}: {}",
        v["policy"].as_str().unwrap_or("refused"),
        v["detail"].as_str().unwrap_or_default()
    )
}

fn enabled_cmd(root: &Path, rule: &str, enabled: bool) -> ExitCode {
    let (verb, what) = if enabled {
        ("enable", "enabling a trigger rule")
    } else {
        ("disable", "disabling a trigger rule")
    };
    if step_refused(what) {
        return refused();
    }
    match confirm_refused(ask_for_name(rule, verb), rule) {
        code if code == ok() => {}
        code => return code,
    }
    match apb_core::trigger::set_enabled(
        root,
        rule,
        enabled,
        TERMINAL,
        apb_core::clock::Millis::now(),
    ) {
        Ok(()) => {
            if enabled {
                println!(
                    "rule `{rule}` enabled; the next pass positions it at the head of its source, so nothing that arrived while it was disabled starts"
                );
            } else {
                println!("rule `{rule}` disabled");
            }
            ok()
        }
        Err(e) => {
            eprintln!("{e}");
            refused()
        }
    }
}

fn remove_cmd(root: &Path, rule: &str) -> ExitCode {
    if step_refused("removing a trigger rule") {
        return refused();
    }
    match confirm_refused(ask_for_name(rule, "remove"), rule) {
        code if code == ok() => {}
        code => return code,
    }
    match apb_core::trigger::remove_rule(root, rule) {
        Ok(outcome) if outcome.removed => {
            // A state entry or a journal the removal could not delete
            // beneath the anchor: the rule is gone either way, and the
            // warnings name what stays behind (W5).
            let mut left = Vec::new();
            if let Some(state) = &outcome.state_left {
                left.push(format!("state_left: {state}"));
            }
            if let Some(journal) = &outcome.journal_left {
                left.push(format!("journal_left: {journal}"));
            }
            if left.is_empty() {
                println!("rule `{rule}` removed, with its state and journal");
            } else {
                println!("rule `{rule}` removed");
                for warning in left {
                    eprintln!("warning: {warning}");
                }
            }
            ok()
        }
        Ok(_) => {
            eprintln!("no rule `{rule}`");
            refused()
        }
        Err(e) => {
            eprintln!("{e}");
            refused()
        }
    }
}

fn list_cmd(root: &Path, json: bool) -> ExitCode {
    let views = match apb_mcp::tools::trigger::list(root) {
        Ok(views) => views,
        Err(e) => {
            eprintln!("{e}");
            return refused();
        }
    };
    if json {
        print_json(&serde_json::json!({ "rules": views }));
        return ok();
    }
    if views.is_empty() {
        println!("no rules in this project (apb trigger set <rule> --file <rule.yaml> writes one)");
        return ok();
    }
    for v in &views {
        let mut line = format!(
            "{}: {}, {}, {} {}, {} start(s) in the last hour",
            v.name,
            if v.enabled { "enabled" } else { "disabled" },
            v.source,
            v.playbook,
            v.version,
            v.starts_last_hour,
        );
        if let Some(last) = &v.last {
            line.push_str(&format!(
                ", {}",
                apb_engine::trigger::view::verdict_words(last)
            ));
        } else {
            line.push_str(", no event judged yet");
        }
        if v.needs_approval {
            line.push_str(&format!(", needs approval: apb trigger approve {}", v.name));
        } else if v.approved {
            line.push_str(", approved");
        } else {
            line.push_str(", not approved: starts nothing");
        }
        println!("{line}");
    }
    ok()
}

fn show_cmd(root: &Path, rule: &str, json: bool) -> ExitCode {
    let entry = match entry_of(root, rule) {
        Ok(entry) => entry,
        Err(e) => {
            eprintln!("{e}");
            return refused();
        }
    };
    let digest = apb_core::trigger::rule_digest(&entry);
    // An unsigned state's entries are never read, as the list reads them:
    // every position shows 0.
    let seen_through = read_signed::<TriggerStateFile>(root, Scope::Project, "state")
        .ok()
        .filter(|loaded| loaded.signed)
        .map(|loaded| {
            loaded
                .file
                .rules
                .iter()
                .find(|s| s.rule == rule && s.incarnation == entry.incarnation)
                .map_or(0, |s| s.seen_through)
        })
        .unwrap_or(0);
    // The journal tail and the refusal that stopped it, from one read.
    let (journal, refusal) =
        apb_core::trigger::read_with_refusal(root, &entry.incarnation, LOG_TAIL);
    // A journal the reader refuses is reported, not substituted: a pass
    // answers the rule with the same refusal, and nothing starts for it.
    let journal_error = refusal
        .map(|e| apb_core::trigger::journal_refused_text(&entry.rule.name, &entry.incarnation, &e));
    if json {
        print_json(&serde_json::json!({
            "rule": entry.rule,
            "enabled": entry.enabled,
            "incarnation": entry.incarnation,
            "rule_digest": digest,
            "approval": entry.approval,
            "seen_through": seen_through,
            "journal": journal,
            "journal_error": journal_error,
        }));
        return ok();
    }
    match yaml_serde::to_string(&entry.rule) {
        Ok(yaml) => print!("{yaml}"),
        Err(e) => eprintln!("cannot print the rule: {e}"),
    }
    println!("enabled: {}", entry.enabled);
    println!("incarnation: {}", entry.incarnation);
    println!("rule_digest: {digest}");
    match &entry.approval {
        Some(a) => println!(
            "approved: yes, for binding {} at {}",
            a.binding,
            apb_core::dismiss::iso_utc(a.approved_at)
        ),
        None => println!("approved: no (starts nothing until a person approves it)"),
    }
    println!("seen_through: {seen_through}");
    if let Some(e) = &journal_error {
        println!("journal: refused: {e}");
    }
    println!("journal (newest last):");
    for line in &journal {
        println!("  {}", apb_engine::trigger::view::journal_line_text(line));
    }
    ok()
}

fn test_cmd(root: &Path, rule: &str, event: Option<u64>, json: bool) -> ExitCode {
    // Refused from a run step by name (D0 "A person's command" item 3):
    // its dry start would run the checks before preparation as the step's
    // process, which refuses or flags what the listener would not.
    if step_refused("testing a trigger rule") {
        eprintln!(
            "{}: see the line above",
            apb_mcp::tools::trigger::TRIGGER_TEST_REFUSED
        );
        return refused();
    }
    // Values appear only for a person at a terminal (D1 "`apb trigger
    // test`"): a pipe, an agent or a dashboard gets counts, never text.
    let show_values = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let report = match apb_mcp::tools::trigger::test_rule(root, rule, event, show_values) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("{e}");
            return refused();
        }
    };
    if json {
        match serde_json::to_value(&report) {
            Ok(v) => print_json(&v),
            Err(e) => {
                eprintln!("cannot print the report: {e}");
                return refused();
            }
        }
    } else {
        print_report(&report);
    }
    let passed = matches!(
        report.filter,
        apb_mcp::tools::trigger::FilterVerdict::Matched
    ) && !report.checks.iter().any(|line| {
        matches!(
            line.state,
            apb_mcp::tools::trigger::CheckState::Refused { .. }
        )
    });
    if passed { ok() } else { ExitCode::from(3) }
}

/// The report as lines: the verdict, the conditions, the params and the key
/// (counts only unless `show_values` was true), then the twelve checks in
/// the order the fire decides them.
fn print_report(report: &apb_mcp::tools::trigger::TestReport) {
    let type_text = report
        .event_type
        .as_deref()
        .map(|t| format!(" ({t})"))
        .unwrap_or_default();
    println!("rule `{}`, event {}{type_text}", report.rule, report.seq);
    match &report.filter {
        apb_mcp::tools::trigger::FilterVerdict::Matched => println!("filter: matched"),
        apb_mcp::tools::trigger::FilterVerdict::Skipped { code, name } => match name {
            Some(name) => println!("filter: skipped ({code}, {name})"),
            None => println!("filter: skipped ({code})"),
        },
    }
    let passed = report.conditions.iter().filter(|c| c.passed).count();
    println!("conditions: {passed} of {} passed", report.conditions.len());
    for param in &report.params {
        match (&param.failure, param.chars) {
            (Some(f), _) => println!("param {}: did not render ({f:?})", param.name),
            (None, Some(chars)) => {
                let pattern = match param.pattern_ok {
                    Some(true) => ", pattern ok",
                    Some(false) => ", pattern failed",
                    None => "",
                };
                println!("param {}: rendered ({chars} chars{})", param.name, pattern);
            }
            (None, None) => println!("param {}: rendered", param.name),
        }
    }
    match (&report.key.failure, &report.key.tag) {
        (Some(f), _) => println!("start key: did not render ({f:?})"),
        (None, Some(tag)) => println!("start key: rendered (tag {tag})"),
        (None, None) => println!("start key: rendered"),
    }
    if let Some(use_of) = &report.key_use {
        let words = match use_of {
            apb_mcp::tools::trigger::KeyUse::Free => "free".to_string(),
            apb_mcp::tools::trigger::KeyUse::Used { run_id, conflict } => {
                if *conflict {
                    format!("used by run `{run_id}` with other params")
                } else {
                    format!("used by run `{run_id}`")
                }
            }
            apb_mcp::tools::trigger::KeyUse::Busy => "busy".to_string(),
        };
        println!("start key use: {words}");
    }
    println!("the checks a start runs before preparation, in order:");
    for line in &report.checks {
        let state = match &line.state {
            apb_mcp::tools::trigger::CheckState::Passed => "passed".to_string(),
            apb_mcp::tools::trigger::CheckState::Refused { code } => format!("refused ({code})"),
            apb_mcp::tools::trigger::CheckState::NotRun => "not run".to_string(),
        };
        println!("  {:?}: {state}", line.id);
    }
    let refused = report.checks.iter().any(|line| {
        matches!(
            line.state,
            apb_mcp::tools::trigger::CheckState::Refused { .. }
        )
    });
    let checked = report.checked_at_start.join(", ");
    if matches!(
        report.filter,
        apb_mcp::tools::trigger::FilterVerdict::Matched
    ) && !refused
    {
        println!("passes the checks before start; checked at start: {checked}");
    } else {
        println!("does not pass the checks before start; checked at start: {checked}");
    }
}

fn log_cmd(root: &Path, rule: &str, limit: Option<usize>, json: bool) -> ExitCode {
    let entry = match entry_of(root, rule) {
        Ok(entry) => entry,
        Err(e) => {
            eprintln!("{e}");
            return refused();
        }
    };
    let lines = apb_core::trigger::read(root, &entry.incarnation, limit.unwrap_or(LOG_TAIL));
    if json {
        print_json(&serde_json::json!({ "journal": lines }));
        return ok();
    }
    if lines.is_empty() {
        println!("no journal lines yet");
        return ok();
    }
    for line in &lines {
        println!("  {}", apb_engine::trigger::view::journal_line_text(line));
    }
    ok()
}
