//! A function's `people:` block: which of its string args people read (the
//! message text, a subject) and which one names the recipient.
//!
//! ```yaml
//!   - name: send_message
//!     people: { to: chat_id, text: text }
//!     idempotency: { mode: none, result_ref: result.message_id }
//! ```
//!
//! A node granted such a function gets apb's writing brief for the args
//! people read. A function that also names `to` and declares `idempotency`
//! can carry a run notification (`crate::notify`): the recipient goes into
//! `to`, and the send-once ledger keeps a notice from going out twice.

use serde::{Deserialize, Serialize};

use super::common::{ConnectorError, is_string_arg};

/// The args of a function that people read, by name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeopleArgs {
    /// The message body.
    pub text: String,
    /// The recipient: a chat id, an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// A subject or a topic line people read before the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl PeopleArgs {
    /// The args people read, subject first.
    pub fn read_args(&self) -> impl Iterator<Item = &str> {
        self.subject
            .as_deref()
            .into_iter()
            .chain([self.text.as_str()])
    }

    fn named(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            Some(("text", self.text.as_str())),
            self.to.as_deref().map(|a| ("to", a)),
            self.subject.as_deref().map(|a| ("subject", a)),
        ]
        .into_iter()
        .flatten()
    }
}

/// Checks the `people:` block of function `function`: it sits only on a
/// function that `sends` something (not `read_only`, not an inbox or imap
/// read), every arg it names is a string property of `args_schema`, and no
/// arg fills two roles (the recipient would overwrite the text).
pub fn validate(
    function: &str,
    people: &PeopleArgs,
    sends: bool,
    args_schema: Option<&serde_json::Value>,
) -> Result<(), ConnectorError> {
    if !sends {
        return Err(ConnectorError::Invalid(format!(
            "function `{function}` sets `people`, but it sends nothing: the block belongs on a function that writes to people"
        )));
    }
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (role, arg) in people.named() {
        if let Some((other, _)) = seen.iter().find(|(_, a)| *a == arg) {
            return Err(ConnectorError::Invalid(format!(
                "function `{function}` names `{arg}` as both `people.{other}` and `people.{role}`; each role needs its own arg"
            )));
        }
        seen.push((role, arg));
        if !is_string_arg(args_schema, arg) {
            return Err(ConnectorError::Invalid(format!(
                "function `{function}` sets `people.{role}: {arg}`, but `{arg}` is not a string property of its args_schema"
            )));
        }
    }
    Ok(())
}

/// Whether a function with this `people:` block can carry a run
/// notification: the block names the recipient, the function declares
/// `idempotency` (`idempotent`), so a notice goes out at most once, and
/// every arg its `args_schema` requires is one the block names, since a
/// notification fills nothing else.
pub fn notice_capable(
    people: Option<&PeopleArgs>,
    idempotent: bool,
    args_schema: Option<&serde_json::Value>,
) -> bool {
    let Some(people) = people.filter(|p| p.to.is_some()) else {
        return false;
    };
    let required = args_schema
        .and_then(|s| s.get("required"))
        .and_then(serde_json::Value::as_array);
    let filled = |name: &str| people.named().any(|(_, arg)| arg == name);
    idempotent && required.is_none_or(|req| req.iter().all(|r| r.as_str().is_some_and(filled)))
}
