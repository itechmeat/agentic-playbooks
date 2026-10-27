//! "Notify me" on the run surfaces: the `notify` argument a start and a
//! live-run change take, the check before a start (the `notify_unavailable`
//! refusal), a person's change on a live run, and the `notify` block of a
//! run answer. The HTTP API maps the same arguments and refusals, so MCP
//! and the dashboard answer alike.
//!
//! A notification is outside the run gate's permit: its target is the
//! person's own subscription, checked here when it is set and by the engine
//! at every send against the digests pinned then (see `apb_engine::notify`).

use std::path::Path;

use apb_core::notify::{NotifyRequest, NotifyTarget, Transition};
use apb_engine::event::Event;
use apb_engine::notify::{ChangedBy, RunNotify, SetError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// The refusal of a start or a change whose notifications cannot be sent.
pub const NOTIFY_UNAVAILABLE: &str = "notify_unavailable";

/// The refusal of a malformed `notify` argument.
pub const INVALID_NOTIFY: &str = "invalid_notify";

/// The refusal of a change on a run that has ended.
pub const RUN_ENDED: &str = "run_ended";

/// The `notify` argument: `true` (notify through the machine's target),
/// `false` (no notifications for this run, whatever the machine default),
/// or the settings to use.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NotifyArg {
    Flag(bool),
    Settings(NotifySettingsArg),
}

/// Notifications with their own settings; each field left out takes the
/// machine's.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifySettingsArg {
    /// The transitions to notify about: review, question, paused, finished.
    #[serde(default)]
    pub on: Option<Vec<String>>,
    /// Where the messages go: an approved connector account and a function
    /// that can carry notifications, and the recipient.
    #[serde(default)]
    pub target: Option<NotifyTargetArg>,
}

/// A notification target.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifyTargetArg {
    pub connector: String,
    pub account: String,
    pub function: String,
    /// The recipient: a chat id or an address, never a credential.
    pub to: String,
}

impl From<NotifyTargetArg> for NotifyTarget {
    fn from(a: NotifyTargetArg) -> Self {
        NotifyTarget {
            connector: a.connector,
            account: a.account,
            function: a.function,
            to: a.to,
        }
    }
}

/// The `notify` argument of `run_notify`, a host's change on a live run:
/// `true` (on, through the run's current target or the machine's),
/// `false` (off), or the transitions to keep. It names no target: a new
/// recipient is set by the person, from a terminal outside any run
/// (`apb notify <run> --via/--to`) or in the dashboard.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NotifyChangeArg {
    Flag(bool),
    Narrow(NotifyNarrowArg),
}

/// The transitions a live run keeps notifying about: only ones its
/// subscription already covers.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifyNarrowArg {
    /// review, question, paused, finished.
    pub on: Vec<String>,
}

impl NotifyChangeArg {
    /// The engine request: never a target.
    pub fn request(&self) -> Result<NotifyRequest, NotifyError> {
        let req = match self {
            NotifyChangeArg::Flag(false) => NotifyRequest::Off,
            NotifyChangeArg::Flag(true) => NotifyRequest::On {
                target: None,
                on: None,
            },
            NotifyChangeArg::Narrow(n) => NotifyRequest::On {
                target: None,
                on: Some(transitions(&n.on)?),
            },
        };
        req.validate().map_err(NotifyError::Invalid)?;
        Ok(req)
    }
}

/// Why a notify request was not honoured. Each surface maps it to its own
/// answer: MCP returns [`NotifyError::body`], HTTP adds a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyError {
    /// The argument does not parse or does not validate.
    Invalid(String),
    /// The target cannot carry notifications or is not approved.
    Unavailable(String),
    /// No such run.
    NotFound(String),
    /// The run has ended; its subscription can no longer change.
    Ended(String),
    /// The journal could not take the change.
    Failed(String),
}

impl NotifyError {
    pub fn code(&self) -> &'static str {
        match self {
            NotifyError::Invalid(_) => INVALID_NOTIFY,
            NotifyError::Unavailable(_) => NOTIFY_UNAVAILABLE,
            NotifyError::NotFound(_) => "not_found",
            NotifyError::Ended(_) => RUN_ENDED,
            NotifyError::Failed(_) => "notify_failed",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            NotifyError::Invalid(d)
            | NotifyError::Unavailable(d)
            | NotifyError::NotFound(d)
            | NotifyError::Ended(d)
            | NotifyError::Failed(d) => d,
        }
    }

    /// The JSON answer of the refusal.
    pub fn body(&self) -> Value {
        json!({ "error": self.code(), "detail": self.detail() })
    }
}

impl NotifyArg {
    /// The engine request.
    pub fn request(&self) -> Result<NotifyRequest, NotifyError> {
        let req = match self {
            NotifyArg::Flag(false) => NotifyRequest::Off,
            NotifyArg::Flag(true) => NotifyRequest::On {
                target: None,
                on: None,
            },
            NotifyArg::Settings(s) => NotifyRequest::On {
                target: s.target.clone().map(NotifyTarget::from),
                on: s.on.as_deref().map(transitions).transpose()?,
            },
        };
        req.validate().map_err(NotifyError::Invalid)?;
        Ok(req)
    }

    /// The engine request of an optional argument; `None` asks for nothing.
    pub fn request_of(arg: Option<&Self>) -> Result<Option<NotifyRequest>, NotifyError> {
        arg.map(Self::request).transpose()
    }
}

/// Parses transition names, refusing an unknown one.
pub fn transitions(names: &[String]) -> Result<Vec<Transition>, NotifyError> {
    names
        .iter()
        .map(|n| {
            Transition::parse(n.trim()).ok_or_else(|| {
                NotifyError::Invalid(format!(
                    "unknown transition `{n}`: expected review, question, paused or finished"
                ))
            })
        })
        .collect()
}

/// The check before a start in the workspace at `root`: the warnings to
/// show with the start's answer (a machine default that will not notify),
/// or the refusal of a request the person made that cannot be honoured.
pub fn start_check(
    root: &Path,
    request: Option<&NotifyRequest>,
) -> Result<Vec<String>, NotifyError> {
    // Only a person sets a target: a start from inside a run step may ask
    // for notifications through the machine's target, not name its own.
    if let Some(NotifyRequest::On {
        target: Some(_), ..
    }) = request
        && let Some(step) = apb_core::step_env::StepCaller::from_env().describe()
    {
        return Err(NotifyError::Invalid(format!(
            "this start comes from {step}, and a run step does not choose where notifications go; leave out the target to use the machine's, or ask the person to start it"
        )));
    }
    let check = apb_engine::notify::check(root, request);
    match check.refusal {
        Some(why) => Err(NotifyError::Unavailable(why)),
        None => Ok(check.warnings),
    }
}

/// A change of the subscription of live run `run_id` made `by` the person
/// or a host: the run's `notify` block after it, with the warnings of the
/// check.
pub fn set(
    root: &Path,
    run_id: &str,
    request: &NotifyRequest,
    by: ChangedBy,
) -> Result<Value, NotifyError> {
    let check = apb_engine::notify::set(root, run_id, request, by).map_err(|e| match e {
        SetError::NotFound(m) => NotifyError::NotFound(m),
        SetError::Ended(m) => NotifyError::Ended(m),
        SetError::NotAllowed(m) => NotifyError::Invalid(m),
        SetError::Journal(e) => NotifyError::Failed(e.to_string()),
    })?;
    if let Some(why) = check.refusal {
        return Err(NotifyError::Unavailable(why));
    }
    let dir = root.join(".apb/runs").join(run_id);
    let events = apb_engine::event::read_all_lossy_tail(&dir).unwrap_or_default();
    let mut out = json!({ "run_id": run_id });
    add_run_block(&mut out, &dir, &events);
    if !check.warnings.is_empty() {
        out["warnings"] = json!(check.warnings);
    }
    Ok(out)
}

/// A host's change of live run `run_id` (`run_notify`): on, off, or
/// fewer transitions. The engine bounds it against the journal it changes:
/// a host never widens what the person chose, nor redirects it.
pub fn set_by_host(
    root: &Path,
    run_id: &str,
    change: &NotifyChangeArg,
) -> Result<Value, NotifyError> {
    set(root, run_id, &change.request()?, ChangedBy::Host)
}

/// The `notify` block of the run at `run_dir` as of now, with the
/// subscription its sends follow; `None` for a run that never had one.
pub fn run_block(run_dir: &Path, events: &[Event]) -> Option<RunNotify> {
    RunNotify::for_run(run_dir, events, apb_core::clock::now_ms_u64())
}

/// Adds the run's `notify` block to a run answer; absent when the run never
/// had a subscription.
pub fn add_run_block(out: &mut Value, run_dir: &Path, events: &[Event]) {
    if let Some(block) = run_block(run_dir, events) {
        out["notify"] = json!(block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(v: Value) -> NotifyArg {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_flag_or_settings_map_to_the_engine_request() {
        assert_eq!(arg(json!(false)).request().unwrap(), NotifyRequest::Off);
        assert_eq!(
            arg(json!(true)).request().unwrap(),
            NotifyRequest::On {
                target: None,
                on: None
            }
        );
        let req = arg(json!({ "on": ["finished", "review"] }))
            .request()
            .unwrap();
        assert_eq!(
            req,
            NotifyRequest::On {
                target: None,
                on: Some(vec![Transition::Finished, Transition::Review])
            }
        );
    }

    #[test]
    fn unknown_transitions_empty_lists_and_credential_recipients_are_invalid() {
        let bad = arg(json!({ "on": ["sometimes"] })).request().unwrap_err();
        assert_eq!(bad.code(), INVALID_NOTIFY);
        assert!(bad.detail().contains("sometimes"));
        assert_eq!(
            arg(json!({ "on": [] })).request().unwrap_err().code(),
            INVALID_NOTIFY
        );
        let target = json!({
            "target": {
                "connector": "telegram",
                "account": "me",
                "function": "send_message",
                "to": format!("ghp_{}", "a".repeat(36)),
            }
        });
        let refused = arg(target).request().unwrap_err();
        assert_eq!(refused.code(), INVALID_NOTIFY);
        assert!(!refused.detail().contains("ghp_"));
    }

    #[test]
    fn unknown_fields_do_not_parse() {
        assert!(serde_json::from_value::<NotifyArg>(json!({ "via": "x" })).is_err());
    }

    #[test]
    fn a_hosts_change_names_no_target() {
        let target = json!({
            "target": {
                "connector": "telegram",
                "account": "me",
                "function": "send_message",
                "to": "1",
            }
        });
        assert!(serde_json::from_value::<NotifyChangeArg>(target).is_err());
        let narrow: NotifyChangeArg = serde_json::from_value(json!({ "on": ["review"] })).unwrap();
        assert_eq!(
            narrow.request().unwrap(),
            NotifyRequest::On {
                target: None,
                on: Some(vec![Transition::Review])
            }
        );
    }

    #[test]
    fn a_host_narrows_the_transitions_of_a_run_but_never_widens_them() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("events.jsonl"),
            concat!(
                "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
                "{\"seq\":1,\"ts\":0,\"type\":\"notify_set\",\"target\":{\"connector\":\"telegram\",\"account\":\"me\",\"function\":\"send_message\",\"to\":\"1\",\"connector_digest\":\"c\",\"account_digest\":\"a\"},\"on\":[\"review\"],\"source\":\"run\"}\n",
            ),
        )
        .unwrap();
        let wider: NotifyChangeArg =
            serde_json::from_value(json!({ "on": ["review", "finished"] })).unwrap();
        let refused = set_by_host(root.path(), "r1", &wider).unwrap_err();
        assert_eq!(refused.code(), INVALID_NOTIFY);
        assert!(
            refused.detail().contains("`finished`"),
            "{}",
            refused.detail()
        );

        // Turned off, the run keeps the bound the person last chose.
        set(root.path(), "r1", &NotifyRequest::Off, ChangedBy::Person).unwrap();
        let refused = set_by_host(root.path(), "r1", &wider).unwrap_err();
        assert_eq!(refused.code(), INVALID_NOTIFY);
        // A path outside the runs is no run.
        let outside = set_by_host(root.path(), "../r1", &wider).unwrap_err();
        assert_eq!(outside.code(), "not_found");
    }
}
