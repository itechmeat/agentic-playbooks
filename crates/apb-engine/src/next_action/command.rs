//! The command line of a next action: one `apb` invocation built from the
//! action's data, never from text.
//!
//! The rules: every named value is one word, `--name=value`; positional
//! values come last, after `--` whenever any of them starts with `-`; each
//! word is quoted for a POSIX shell by [`shell_quote`]. An argument the
//! person still has to supply (a free answer, a reply file) renders as
//! `<hint>` and makes the line not runnable. A line is runnable only when
//! the real parser reads it back to the same values (the CLI's tests hold
//! every code to that). The engine owns it so that a text the engine writes
//! (a run outcome's gap) prints the same command the action does.

use std::collections::BTreeMap;

use super::types::{ActionCode, ArgValue, NextAction, RequestKind};

/// `--request <kind>:<seq>` of `apb answer` and `apb review`: the request
/// a next action names (`question:12`, `review:7`, `publish:9`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestArg {
    pub kind: RequestKind,
    pub seq: u64,
}

impl RequestArg {
    fn kind_name(kind: RequestKind) -> &'static str {
        match kind {
            RequestKind::Question => "question",
            RequestKind::Review => "review",
            RequestKind::Publish => "publish",
        }
    }

    /// The word a command line carries.
    pub fn word(kind: RequestKind, seq: u64) -> String {
        format!("{}:{seq}", Self::kind_name(kind))
    }

    /// The seq, when the request is of one of `kinds` (the command's own).
    pub fn seq_for(self, kinds: &[RequestKind]) -> Result<u64, String> {
        if kinds.contains(&self.kind) {
            Ok(self.seq)
        } else {
            Err(format!(
                "--request names a {} request; this command answers {}",
                Self::kind_name(self.kind),
                kinds
                    .iter()
                    .map(|k| Self::kind_name(*k))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ))
        }
    }
}

impl std::str::FromStr for RequestArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, seq) = s
            .split_once(':')
            .ok_or_else(|| format!("`{s}` is not <kind>:<seq> (question:12, review:7)"))?;
        let kind = match kind {
            "question" => RequestKind::Question,
            "review" => RequestKind::Review,
            "publish" => RequestKind::Publish,
            other => return Err(format!("unknown request kind `{other}`")),
        };
        let seq = seq
            .parse()
            .map_err(|_| format!("`{seq}` is not a journal seq"))?;
        Ok(Self { kind, seq })
    }
}

/// One rendered command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCommand {
    pub argv: Vec<String>,
    /// No argument is left to supply: the line runs as it is.
    pub runnable: bool,
}

impl CliCommand {
    /// The line to print or copy, every word shell-quoted. A placeholder
    /// (`'<your answer>'`, `'--sent|--not-sent'`) is quoted too: its text
    /// can come from a playbook or an agent (a question's options), and a
    /// line pasted before it is filled in must run nothing but `apb`. The
    /// dashboard shows this same line.
    pub fn line(&self) -> String {
        self.argv
            .iter()
            .map(|w| shell_quote(w))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One word of a command: a value, or a placeholder still to fill in.
struct Word {
    text: String,
    placeholder: bool,
}

impl From<String> for Word {
    fn from(text: String) -> Self {
        Word {
            text,
            placeholder: false,
        }
    }
}

impl Word {
    fn placeholder(text: String) -> Self {
        Word {
            text,
            placeholder: true,
        }
    }
}

/// The placeholder the renderer prints for an argument nobody filled in
/// (`<your answer>`, `<approved|rejected>`); `None` for a given value.
pub fn placeholder_of(value: &ArgValue) -> Option<String> {
    match value {
        ArgValue::Given { .. } => None,
        ArgValue::Choice { options, .. } => Some(format!("<{}>", options.join("|"))),
        ArgValue::Supplied { hint } => Some(format!("<{hint}>")),
    }
}

/// Whether `value` is exactly a placeholder the renderer printed for one of
/// `actions`' arguments: a line pasted before it was filled in. A command
/// that takes free text refuses it, so the paste spends no request; any
/// other text, `<ok>` included, is an answer like any other.
pub fn is_unfilled(actions: &[NextAction], value: &str) -> bool {
    actions
        .iter()
        .flat_map(|a| &a.args)
        .filter_map(|arg| placeholder_of(&arg.value))
        .any(|p| p == value)
}

/// Quotes `word` for a POSIX shell: a word made only of
/// `[A-Za-z0-9_./:=@%+,-]` that does not start with `=` or `%` (zsh expands
/// a leading `=word` to a command path) stays bare, anything else (an empty
/// word included) is single-quoted, with `'` written as `'\''`.
pub fn shell_quote(word: &str) -> String {
    let bare = !word.is_empty()
        && !word.starts_with(['=', '%'])
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c));
    if bare {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Builds one command: options first, then the positionals.
struct Builder {
    words: Vec<Word>,
    positionals: Vec<Word>,
    runnable: bool,
}

impl Builder {
    fn new(subcommand: &[&str]) -> Self {
        let mut words = vec![Word::from("apb".to_string())];
        words.extend(subcommand.iter().map(|s| Word::from(s.to_string())));
        Self {
            words,
            positionals: Vec::new(),
            runnable: true,
        }
    }

    fn option<W: Into<Word>>(&mut self, flag: &str, value: Option<W>) {
        if let Some(v) = value {
            let v = v.into();
            self.words.push(Word {
                text: format!("--{flag}={}", v.text),
                placeholder: v.placeholder,
            });
        }
    }

    fn flag(&mut self, flag: &str) {
        self.words.push(Word::from(format!("--{flag}")));
    }

    fn positional<W: Into<Word>>(&mut self, value: Option<W>) {
        if let Some(v) = value {
            self.positionals.push(v.into());
        }
    }

    /// The words of `apb intent resolve`: the decision's flag (either flag,
    /// and a line that does not run, while nobody chose one), then the run
    /// and the intent.
    fn resolve_intent(
        &mut self,
        run: Option<String>,
        decision: Option<&str>,
        intent: Option<Word>,
    ) {
        match decision {
            Some("sent") => self.flag("sent"),
            Some("not_sent") => self.flag("not-sent"),
            _ => {
                self.runnable = false;
                self.words
                    .push(Word::placeholder("--sent|--not-sent".to_string()));
            }
        }
        self.positional(run);
        self.positional(intent);
    }

    fn finish(mut self) -> CliCommand {
        if self
            .positionals
            .iter()
            .any(|p| !p.placeholder && p.text.starts_with('-'))
        {
            self.words.push(Word::from("--".to_string()));
        }
        self.words.append(&mut self.positionals);
        CliCommand {
            argv: self.words.into_iter().map(|w| w.text).collect(),
            runnable: self.runnable,
        }
    }
}

/// The values of an action's arguments: those the action carries, those
/// the caller `chose` (an option of a choice, a supplied text), and a
/// placeholder for the rest of the required ones.
struct Values<'a> {
    action: &'a NextAction,
    chosen: &'a BTreeMap<String, String>,
}

impl Values<'_> {
    /// The value of argument `name`: `None` for an optional argument
    /// nobody gave; a `<placeholder>` (and a line that does not run) for a
    /// required one.
    fn get(&self, name: &str, b: &mut Builder) -> Option<Word> {
        if let Some(v) = self.chosen.get(name) {
            return Some(Word::from(v.clone()));
        }
        let arg = self.action.args.iter().find(|a| a.name == name)?;
        match &arg.value {
            ArgValue::Given { value } => Some(Word::from(value.clone())),
            _ if !arg.required => None,
            other => {
                b.runnable = false;
                placeholder_of(other).map(Word::placeholder)
            }
        }
    }
}

/// The command line of `action`, with the values the caller `chose`; `None`
/// for an action the CLI has no command for (the supervisor's).
pub fn render(action: &NextAction, chosen: &BTreeMap<String, String>) -> Option<CliCommand> {
    let v = Values { action, chosen };
    let run = Some(action.run_id.clone());
    let node = action.target.node.clone();
    let request = action
        .target
        .request
        .as_ref()
        .map(|r| RequestArg::word(r.kind, r.seq));
    let mut b;
    match action.code {
        ActionCode::AnswerQuestion => {
            b = Builder::new(&["answer"]);
            b.option("node", node);
            b.option("request", request);
            b.positional(run);
            let answer = v.get("answer", &mut b);
            b.positional(answer);
        }
        ActionCode::DecideReview | ActionCode::DecidePublish => {
            b = Builder::new(&["review"]);
            let decision = v.get("decision", &mut b);
            b.option("decision", decision);
            let note = v.get("note", &mut b);
            b.option("note", note);
            let nonce = v.get("nonce", &mut b);
            b.option("nonce", nonce);
            b.option("request", request);
            b.positional(run);
            b.positional(node);
        }
        ActionCode::SubmitHostTask => {
            b = Builder::new(&["tasks", "submit"]);
            let status = v.get("status", &mut b);
            b.option("status", status);
            let output = v.get("output", &mut b);
            b.option("output-file", output);
            b.positional(run);
            b.positional(action.target.task_id.clone());
        }
        ActionCode::ResumeRun => {
            b = Builder::new(&["resume"]);
            b.positional(run);
        }
        ActionCode::ResumeWithBudget => {
            b = Builder::new(&["resume"]);
            let tokens = v.get("raise_budget_tokens", &mut b);
            b.option("raise-budget-tokens", tokens);
            b.positional(run);
        }
        ActionCode::RetryNow => {
            b = Builder::new(&["resume"]);
            b.flag("retry-now");
            let node = v.get("node", &mut b);
            b.option("node", node);
            b.positional(run);
        }
        ActionCode::ResolveIntent => {
            b = Builder::new(&["intent", "resolve"]);
            let decision = v.get("decision", &mut b);
            let intent = v.get("intent", &mut b);
            b.resolve_intent(run, decision.as_ref().map(|w| w.text.as_str()), intent);
        }
        ActionCode::WaitAgain => {
            b = Builder::new(&["wait"]);
            b.positional(run);
        }
        ActionCode::ReadReport => {
            b = Builder::new(&["runs"]);
            b.positional(run);
        }
        ActionCode::SupervisorRetryNode
        | ActionCode::SupervisorContinueFrom
        | ActionCode::SupervisorAbort => return None,
    }
    Some(b.finish())
}

/// `apb intent resolve` of intent `intent` (an `<intent>` placeholder
/// without one) of run `run_id`, with `decision` (`sent`, `not_sent`) or,
/// without one, either flag: the one command every text that tells a
/// person to resolve a send prints, the same one the `resolve_intent` next
/// action renders.
pub fn resolve_intent(run_id: &str, intent: Option<&str>, decision: Option<&str>) -> CliCommand {
    let mut b = Builder::new(&["intent", "resolve"]);
    let intent = match intent {
        Some(i) => Word::from(i.to_string()),
        None => Word::placeholder("<intent>".to_string()),
    };
    b.resolve_intent(Some(run_id.to_string()), decision, Some(intent));
    b.finish()
}
