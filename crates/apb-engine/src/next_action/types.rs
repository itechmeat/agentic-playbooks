//! The shapes of next actions, shared by every module that builds or reads
//! them.

use serde::Serialize;

use crate::state::RunStatus;

/// Why a surface does not offer an action to its caller; every facade's
/// `caller_refusal` is one of these.
pub use crate::decision_channel::CallerRefusal;

/// The one verdict over every open wait of a run and its live children.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopVerdict {
    Finished,
    ProgressesByItself,
    Stopped,
    NeedsInput,
}

/// What an action does. A closed set: a new code is added by a design
/// change, and every surface's exhaustive `match` then fails to compile
/// until it maps it. A reader meeting a code it does not know shows the
/// action's `reason` and treats it as not ready.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionCode {
    AnswerQuestion,
    DecideReview,
    DecidePublish,
    SubmitHostTask,
    SupervisorRetryNode,
    SupervisorContinueFrom,
    SupervisorAbort,
    ResumeRun,
    ResumeWithBudget,
    /// Start a deferred retry now instead of at its due time.
    RetryNow,
    ResolveIntent,
    WaitAgain,
    ReadReport,
}

impl ActionCode {
    /// Every code, in declaration order.
    pub const ALL: [ActionCode; 13] = [
        ActionCode::AnswerQuestion,
        ActionCode::DecideReview,
        ActionCode::DecidePublish,
        ActionCode::SubmitHostTask,
        ActionCode::SupervisorRetryNode,
        ActionCode::SupervisorContinueFrom,
        ActionCode::SupervisorAbort,
        ActionCode::ResumeRun,
        ActionCode::ResumeWithBudget,
        ActionCode::RetryNow,
        ActionCode::ResolveIntent,
        ActionCode::WaitAgain,
        ActionCode::ReadReport,
    ];

    /// The wire name (`answer_question`).
    pub fn as_str(self) -> &'static str {
        match self {
            ActionCode::AnswerQuestion => "answer_question",
            ActionCode::DecideReview => "decide_review",
            ActionCode::DecidePublish => "decide_publish",
            ActionCode::SubmitHostTask => "submit_host_task",
            ActionCode::SupervisorRetryNode => "supervisor_retry_node",
            ActionCode::SupervisorContinueFrom => "supervisor_continue_from",
            ActionCode::SupervisorAbort => "supervisor_abort",
            ActionCode::ResumeRun => "resume_run",
            ActionCode::ResumeWithBudget => "resume_with_budget",
            ActionCode::RetryNow => "retry_now",
            ActionCode::ResolveIntent => "resolve_intent",
            ActionCode::WaitAgain => "wait_again",
            ActionCode::ReadReport => "read_report",
        }
    }
}

/// Whom a run accepts an action from. It describes the run's rule, not the
/// caller: a surface knows its own caller and says separately whether the
/// action is available to it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Principal {
    /// A person deciding for themselves (the dashboard, a terminal).
    Person,
    /// A person operating apb on someone's behalf (scripts, CI).
    Operator,
    /// The host agent session that started or follows the run.
    Host,
    /// The run's supervisor agent.
    Supervisor,
}

/// The kind of request an action answers.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Question,
    Review,
    Publish,
}

/// The request an action answers: the journal seq of the
/// `question_asked` or `review_requested` that opened it, and for a
/// publish gate the nonce of its plan. Executing the action with it lets
/// the engine refuse an answer that arrives after the request changed.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestRef {
    pub kind: RequestKind,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub nonce: Option<String>,
}

/// What an action acts on inside its run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActionTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub request: Option<RequestRef>,
}

/// The value of one argument.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArgValue {
    /// Known: the action carries it.
    Given { value: String },
    /// One of a fixed set; `default` is what the engine itself would pick.
    Choice {
        options: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        default: Option<String>,
    },
    /// The person supplies it (free text, a file); `hint` says what.
    Supplied { hint: String },
}

/// One named argument of an action. A value is never a param value or a
/// token; a nonce appears only on the gate it answers.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionArg {
    pub name: String,
    pub value: ArgValue,
    pub required: bool,
}

/// What happens when a wait runs out by itself.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoOutcome {
    /// The engine posts the question's default answer.
    AnsweredDefault,
    /// A timer `wait` succeeds.
    TimerElapses,
    /// The attempt fails (a question without a default, a host task past
    /// its deadline).
    AttemptFails,
    /// A webhook `wait` (or a timer behind its timeout) fails.
    WaitTimesOut,
    /// The pause waker tries to resume the run.
    ResumeAttempt,
    /// A deferred retry's delay passes and the node's next attempt starts.
    RetryAttempt,
}

impl AutoOutcome {
    /// Whether the run goes on successfully by itself.
    pub fn is_success(self) -> bool {
        matches!(
            self,
            AutoOutcome::AnsweredDefault | AutoOutcome::TimerElapses
        )
    }
}

/// Progress a wait makes without anyone.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutoProgress {
    /// At `at_ms` (epoch milliseconds). `assured`: a live driver runs the
    /// clock and nothing outside the engine is needed.
    At {
        at_ms: u64,
        outcome: AutoOutcome,
        assured: bool,
    },
    /// The run is paused: the timer does not run, `remaining_ms` is left.
    Frozen {
        remaining_ms: u64,
        outcome: AutoOutcome,
    },
}

impl AutoProgress {
    pub fn outcome(&self) -> AutoOutcome {
        match self {
            AutoProgress::At { outcome, .. } | AutoProgress::Frozen { outcome, .. } => *outcome,
        }
    }

    /// The run goes on successfully by itself, for sure.
    pub fn assured_success(&self) -> bool {
        matches!(self, AutoProgress::At { assured: true, outcome, .. } if outcome.is_success())
    }
}

/// Why an action cannot be taken now.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotReady {
    pub code: String,
    pub detail: String,
}

/// One thing that can be done for a run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NextAction {
    pub code: ActionCode,
    /// The run it acts on: the observed run or one of its live children.
    pub run_id: String,
    pub target: ActionTarget,
    pub args: Vec<ActionArg>,
    /// Whom the run accepts it from.
    pub principals: Vec<Principal>,
    /// The run's state allows it now.
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub not_ready: Option<NotReady>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub auto: Option<AutoProgress>,
    /// One English line.
    pub reason: String,
    /// The `apb` command line that takes the action, from the one renderer
    /// every surface prints ([`super::command::render`]); set on the actions
    /// a surface shows ([`super::for_surface`]), absent for an action the
    /// CLI has no command for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub command: Option<String>,
    /// What an unattended run does at this wait, shown on every run,
    /// attended or not, so a CI author sees what the mode would do. Absent
    /// where it does not apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub unattended: Option<UnattendedOutcome>,
    /// Only a person's channel takes it: the CLI at an interactive terminal
    /// outside a run step, or the dashboard with a signed-in session (a
    /// gate marked `person_only`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub person_only: bool,
    /// Why the surface that shows it does not take it from its caller
    /// ([`super::mark_for_caller`]); absent when the caller may take it, or
    /// on a surface that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub caller_refusal: Option<CallerRefusal>,
}

/// What an unattended run does at a wait ([`NextAction::unattended`]).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnattendedOutcome {
    /// The wait ends by itself as in an attended run.
    Proceeds { auto: AutoOutcome },
    /// The gate is decided at once with its declared option.
    Decides { option: String },
    /// The question is answered at once with its default.
    AnswersDefault,
    /// The run fails at once with this run error code
    /// (`hitl_required`, `budget_exhausted`).
    FailsRun { code: String },
    /// An unattended start is refused before the run exists
    /// (`unattended_unsupported`).
    RefusedAtStart { code: String },
}

/// Everything a run is ready for at one observation.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunActions {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub observed_at_ms: u64,
    pub status: RunStatus,
    pub stop: StopVerdict,
    /// In primary order: the first is the one to lead with.
    pub actions: Vec<NextAction>,
    /// The index of the primary action in `actions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub primary: Option<usize>,
    /// A live child sat too deep to be observed; its waits are not here.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub children_truncated: bool,
}

impl NextAction {
    /// Every text of the action passed through `scrub`: the reason, the
    /// detail of `not_ready`, the item it targets and every option and
    /// hint. Codes, run and node ids, seqs, principals and the given
    /// argument values (an intent id, a plan nonce, a node) are apb's own
    /// and stay: the path that executes the action compares them exactly.
    pub fn scrub_texts(&mut self, scrub: impl Fn(&str) -> String) {
        self.reason = scrub(&self.reason);
        if let Some(n) = &mut self.not_ready {
            n.detail = scrub(&n.detail);
        }
        if let Some(item) = &mut self.target.item {
            *item = scrub(item);
        }
        for arg in &mut self.args {
            match &mut arg.value {
                ArgValue::Given { .. } => {}
                ArgValue::Choice { options, default } => {
                    options.iter_mut().for_each(|o| *o = scrub(o));
                    if let Some(d) = default {
                        *d = scrub(d);
                    }
                }
                ArgValue::Supplied { hint } => *hint = scrub(hint),
            }
        }
    }
}

impl RunActions {
    /// The primary action, if any.
    pub fn primary(&self) -> Option<&NextAction> {
        self.primary.and_then(|i| self.actions.get(i))
    }
}
