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

pub use precedence::{Effective, NotifySource, effective};

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
}

impl Transition {
    /// Every transition, in the order they are reported.
    pub const ALL: [Transition; 4] = [
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

fn all_transitions() -> Vec<Transition> {
    Transition::ALL.to_vec()
}

fn is_all(on: &[Transition]) -> bool {
    Transition::normalized(on) == Transition::ALL
}

/// The machine's `notify:` section of `config.yaml`.
///
/// ```yaml
/// notify:
///   default: off        # on: every run notifies; off: only runs that ask
///   target: { connector: telegram, account: me, function: send_message, to: "123456789" }
///   on: [review, question, paused, finished]   # the default
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifySettings {
    pub default: Toggle,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<NotifyTarget>,
    #[serde(skip_serializing_if = "is_all")]
    pub on: Vec<Transition>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifySettings {
            default: Toggle::Off,
            target: None,
            on: all_transitions(),
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
    },
}

impl NotifyRequest {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            NotifyRequest::Off => Ok(()),
            NotifyRequest::On { target, on } => {
                if let Some(t) = target {
                    t.validate()?;
                }
                if on.as_ref().is_some_and(Vec::is_empty) {
                    return Err("notify `on` names no transition".into());
                }
                Ok(())
            }
        }
    }
}
