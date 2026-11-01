//! "Notify me": the settings that let a person hear about a run's
//! transitions through a connector they already use. The machine's
//! `config.yaml` holds the target (`notify:`), a project's
//! `.apb/config.yaml` may only turn notifications off or narrow the
//! transitions, and a run may ask for its own subscription
//! ([`NotifyRequest`]). [`precedence::effective`] combines the three.
//!
//! A message carries the run's status, what the person is asked to do and a
//! link. The engine sends it (`apb_engine::notify`); this module only holds
//! the shapes and their checks.

pub mod precedence;

use serde::{Deserialize, Deserializer, Serialize};

pub use precedence::{Effective, NotifySource, Settings, effective};

/// The longest recipient a target may name.
pub const MAX_TO_LEN: usize = 256;

/// A run transition a person can be told about.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    /// A human-review gate waits for a decision.
    Review,
    /// An interactive node asked a question nobody answered yet.
    Question,
    /// The engine or the supervisor paused the run.
    Paused,
    /// The run finished, or something other than a person stopped it.
    Finished,
    /// A host task waits for the host to submit it (opt-in).
    HostTask,
    /// A failed or timed-out node waits for the run's supervisor (opt-in).
    Supervisor,
}

impl Transition {
    /// Every transition, in the order they are reported.
    pub const ALL: [Transition; 6] = [
        Transition::Review,
        Transition::Question,
        Transition::Paused,
        Transition::Finished,
        Transition::HostTask,
        Transition::Supervisor,
    ];

    /// The transitions a subscription covers when nothing narrows it: the
    /// waits on the person and the run's end. `host_task` and `supervisor`
    /// wait on a host or an agent and are asked for by name.
    pub const DEFAULT: [Transition; 4] = [
        Transition::Review,
        Transition::Question,
        Transition::Paused,
        Transition::Finished,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Transition::Review => "review",
            Transition::Question => "question",
            Transition::Paused => "paused",
            Transition::Finished => "finished",
            Transition::HostTask => "host_task",
            Transition::Supervisor => "supervisor",
        }
    }

    /// The inverse of [`Transition::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// `on` in report order, each transition once.
    pub fn normalized(on: &[Transition]) -> Vec<Transition> {
        Self::ALL.into_iter().filter(|t| on.contains(t)).collect()
    }

    /// The names every surface accepts, for an error that refuses another.
    pub fn names() -> String {
        Self::ALL.map(Transition::as_str).join(", ")
    }
}

/// How a run that ended came out, for a `finished` filter.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishOutcome {
    /// The run succeeded and nothing calls for a look.
    Succeeded,
    /// The run succeeded, but its outcome says something needs a look.
    NeedsLook,
    /// The run failed.
    Failed,
    /// Something other than a person stopped the run.
    Aborted,
}

impl FinishOutcome {
    /// Every outcome, in report order.
    pub const ALL: [FinishOutcome; 4] = [
        FinishOutcome::Succeeded,
        FinishOutcome::NeedsLook,
        FinishOutcome::Failed,
        FinishOutcome::Aborted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FinishOutcome::Succeeded => "succeeded",
            FinishOutcome::NeedsLook => "needs_look",
            FinishOutcome::Failed => "failed",
            FinishOutcome::Aborted => "aborted",
        }
    }

    /// The inverse of [`FinishOutcome::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }

    /// `outcomes` in report order, each once.
    pub fn normalized(outcomes: &[FinishOutcome]) -> Vec<FinishOutcome> {
        Self::ALL
            .into_iter()
            .filter(|o| outcomes.contains(o))
            .collect()
    }

    /// The names every surface accepts, for an error that refuses another.
    pub fn names() -> String {
        Self::ALL.map(FinishOutcome::as_str).join(", ")
    }

    /// Parses outcome names, refusing an unknown one by name.
    pub fn parse_all<S: AsRef<str>>(names: &[S]) -> Result<Vec<FinishOutcome>, String> {
        names
            .iter()
            .map(|n| {
                let n = n.as_ref().trim();
                Self::parse(n).ok_or_else(|| {
                    format!("unknown outcome `{n}`: expected {}", FinishOutcome::names())
                })
            })
            .collect()
    }
}

/// What a subscription covers, as `notify_set` journals it: the
/// transitions, and for `finished` the outcomes it is told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub on: Vec<Transition>,
    /// The outcomes a `finished` notice is sent for; every outcome when
    /// nothing narrows it. Meaningless without `finished` in `on`.
    pub finished: Vec<FinishOutcome>,
}

/// The prefix of a narrowed `finished` in the journal: `finished:failed`.
const FINISHED_PREFIX: &str = "finished:";

impl Coverage {
    pub fn new(on: &[Transition], finished: &[FinishOutcome]) -> Self {
        Coverage {
            on: Transition::normalized(on),
            finished: FinishOutcome::normalized(finished),
        }
    }

    /// Whether a `finished` notice for a run that came out `outcome` is due
    /// under this coverage.
    pub fn covers_finish(&self, outcome: FinishOutcome) -> bool {
        self.on.contains(&Transition::Finished) && self.finished.contains(&outcome)
    }

    /// Whether a `finished` notice is narrowed to some outcomes.
    pub fn filters_finish(&self) -> bool {
        self.finished.len() < FinishOutcome::ALL.len()
    }

    /// The names `notify_set.on` carries: each transition, with `finished`
    /// as `finished` when every outcome is covered and as one
    /// `finished:<outcome>` per outcome otherwise. An older apb ignores the
    /// names it does not know, so a filtered subscription misses a notice
    /// there instead of sending an unwanted one.
    pub fn journal_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.on {
            if *t == Transition::Finished && self.filters_finish() {
                out.extend(
                    self.finished
                        .iter()
                        .map(|o| format!("{FINISHED_PREFIX}{}", o.as_str())),
                );
            } else {
                out.push(t.as_str().to_string());
            }
        }
        out
    }

    /// The inverse of [`Coverage::journal_names`]; a name this apb does not
    /// know is ignored.
    pub fn from_journal_names<S: AsRef<str>>(names: &[S]) -> Self {
        let mut on = Vec::new();
        let mut finished = Vec::new();
        let mut plain_finished = false;
        for name in names {
            let name = name.as_ref();
            if let Some(o) = name
                .strip_prefix(FINISHED_PREFIX)
                .and_then(FinishOutcome::parse)
            {
                on.push(Transition::Finished);
                finished.push(o);
            } else if let Some(t) = Transition::parse(name) {
                plain_finished |= t == Transition::Finished;
                on.push(t);
            }
        }
        if plain_finished || finished.is_empty() {
            finished = FinishOutcome::ALL.to_vec();
        }
        Coverage::new(&on, &finished)
    }
}

/// Whether every run of the machine notifies (`on`) or only the runs that
/// ask (`off`). Read from `on` / `off` or `true` / `false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    On,
    #[default]
    Off,
}

impl<'de> Deserialize<'de> for Toggle {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Flag(bool),
            Word(String),
        }
        match Raw::deserialize(d)? {
            Raw::Flag(true) => Ok(Toggle::On),
            Raw::Flag(false) => Ok(Toggle::Off),
            Raw::Word(w) if w == "on" => Ok(Toggle::On),
            Raw::Word(w) if w == "off" => Ok(Toggle::Off),
            Raw::Word(w) => Err(serde::de::Error::custom(format!(
                "`{w}` is not `on` or `off`"
            ))),
        }
    }
}

/// Where a notification goes: one function of one configured connector
/// account, and the recipient it names (a chat id, an address).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyTarget {
    pub connector: String,
    pub account: String,
    pub function: String,
    pub to: String,
}

impl NotifyTarget {
    /// The names are slugs (the function a snake name), and the recipient
    /// is short and never credential-shaped: it is written into the run's
    /// journal, so a token pasted into it by mistake is refused.
    pub fn validate(&self) -> Result<(), String> {
        crate::profile::validate_slug("notify connector", &self.connector)?;
        crate::profile::validate_slug("notify account", &self.account)?;
        crate::connector::validate_snake_name(&self.function)
            .map_err(|e| format!("notify function: {e}"))?;
        let to = self.to.as_str();
        if to.trim() != to {
            return Err("notify recipient `to` starts or ends with whitespace".into());
        }
        if to.is_empty() {
            return Err("notify recipient `to` is empty".into());
        }
        if to.chars().count() > MAX_TO_LEN {
            return Err(format!(
                "notify recipient `to` is longer than {MAX_TO_LEN} characters"
            ));
        }
        if crate::token_shape::looks_like_credential(to) {
            return Err(
                "notify recipient `to` looks like a credential; name a chat id or an address"
                    .into(),
            );
        }
        Ok(())
    }
}

fn default_transitions() -> Vec<Transition> {
    Transition::DEFAULT.to_vec()
}

fn is_default(on: &[Transition]) -> bool {
    Transition::normalized(on) == Transition::DEFAULT
}

/// The machine's `notify:` section of `config.yaml`.
///
/// ```yaml
/// notify:
///   default: off        # on: every run notifies; off: only runs that ask
///   target: { connector: telegram, account: me, function: send_message, to: "123456789" }
///   on: [review, question, paused, finished]   # the default
/// ```
///
/// Which playbooks and which run outcomes a default subscription covers is
/// set in `policy.yaml` ([`crate::policy_file`]), a file older binaries
/// never read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifySettings {
    pub default: Toggle,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<NotifyTarget>,
    #[serde(skip_serializing_if = "is_default")]
    pub on: Vec<Transition>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifySettings {
            default: Toggle::Off,
            target: None,
            on: default_transitions(),
        }
    }
}

impl NotifySettings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(t) = &self.target {
            t.validate()?;
        }
        if self.default == Toggle::On && self.target.is_none() {
            return Err("notify.default is on but notify.target is not set".into());
        }
        if self.on.is_empty() {
            return Err("notify.on names no transition".into());
        }
        Ok(())
    }
}

/// A project's `notify:` section of `.apb/config.yaml`. The file may be
/// committed, so it names no target and no recipient: it can only turn the
/// machine default off for this project's runs or narrow the transitions.
///
/// ```yaml
/// notify:
///   enabled: false
///   on: [review, finished]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectNotify {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<Transition>>,
}

impl ProjectNotify {
    /// An `on` that names no transition is refused: to turn notifications
    /// off for the project, `enabled: false` says so.
    pub fn validate(&self) -> Result<(), String> {
        if self.on.as_ref().is_some_and(Vec::is_empty) {
            return Err(
                "notify.on names no transition; set `enabled: false` to turn notifications off"
                    .into(),
            );
        }
        Ok(())
    }
}

/// What a run asks for, from the person who starts it or changes it later.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NotifyRequest {
    /// No notifications for this run, whatever the machine default says.
    Off,
    /// Notifications for this run: to `target` (the machine's when absent),
    /// about `on` (the narrowed default when absent).
    On {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        target: Option<NotifyTarget>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        on: Option<Vec<Transition>>,
        /// The outcomes a `finished` notice is sent for (every outcome the
        /// settings allow when absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        finished: Option<Vec<FinishOutcome>>,
    },
}

/// Why an outcome filter is refused: without `finished` among the
/// transitions in force it would filter nothing.
pub const OUTCOMES_WITHOUT_FINISHED: &str = "notify `finished` filters the end-of-run notice, but `finished` is not among the transitions; add `finished` to `on`";

impl NotifyRequest {
    /// Notifications through the machine's target, about what the settings
    /// cover.
    pub fn plain_on() -> Self {
        NotifyRequest::On {
            target: None,
            on: None,
            finished: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            NotifyRequest::Off => Ok(()),
            NotifyRequest::On {
                target,
                on,
                finished,
            } => {
                if let Some(t) = target {
                    t.validate()?;
                }
                if on.as_ref().is_some_and(Vec::is_empty) {
                    return Err("notify `on` names no transition".into());
                }
                if finished.as_ref().is_some_and(Vec::is_empty) {
                    return Err(
                        "notify `finished` names no outcome; leave `finished` out of `on` instead"
                            .into(),
                    );
                }
                if finished.is_some()
                    && on
                        .as_ref()
                        .is_some_and(|on| !on.contains(&Transition::Finished))
                {
                    return Err(OUTCOMES_WITHOUT_FINISHED.into());
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_outcome_class_passes_only_its_filter() {
        for filter in FinishOutcome::ALL {
            let cov = Coverage::new(&[Transition::Finished], &[filter]);
            for outcome in FinishOutcome::ALL {
                assert_eq!(cov.covers_finish(outcome), outcome == filter);
            }
        }
        let all = Coverage::new(&[Transition::Finished], &FinishOutcome::ALL);
        assert!(FinishOutcome::ALL.iter().all(|o| all.covers_finish(*o)));
        let none = Coverage::new(&[Transition::Review], &FinishOutcome::ALL);
        assert!(!none.covers_finish(FinishOutcome::Failed));
    }
}
