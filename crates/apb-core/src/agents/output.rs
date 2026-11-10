//! What an agent CLI's output says about one attempt: the reply, the token
//! usage it reports, its closing record and its session id.
//!
//! The built-in forms ask claude, codex, opencode, zcode and dsh for their
//! machine output; each of those modules documents its shape and the CLI
//! version it was checked against. Every reader is lenient in the same way: an output
//! that does not have the expected shape (plain text from a custom invocation
//! form, a stub agent, an older CLI) yields `None`, and the caller keeps the
//! raw stdout as the reply and records no usage. Usage is only ever what the
//! CLI printed; nothing here estimates tokens or prices them.
//!
//! [`AgentUsage`] takes the numbers as the CLI reports them. The one
//! adjustment is the cache reads a CLI is documented to count inside its
//! input (codex, zcode), which are moved out of `input_tokens`; opencode's
//! separate reasoning count is added to `output_tokens`. Nothing else is
//! added or subtracted: whether codex's `cache_write_input_tokens` sit
//! inside its `input_tokens`, or zcode's `reasoningTokens` inside its
//! `outputTokens`, is not something apb can verify, so both are kept as
//! printed. The numbers may therefore not be comparable across agents.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::exposure::ExposedTools;

/// How one agent's output reads.
pub trait OutputFormat: Send + Sync {
    /// What the built-in form's output carries.
    fn promises(&self) -> Promises;

    /// The reply in `stdout`, `None` when it is not this agent's machine
    /// format.
    fn reply(&self, _stdout: &str) -> Option<Reply> {
        None
    }

    /// The token usage in `stdout`, `None` when the output reports none.
    fn usage(&self, _stdout: &str) -> Option<AgentUsage> {
        None
    }

    /// Whether `stdout` ends with the closing record of the machine output,
    /// the one carrying the verdict and the usage. Every CLI prints it last,
    /// so a copy of the output that kept only the beginning and the end still
    /// has it unless it was cut. `None` for an agent without a machine format,
    /// where no record can be checked for.
    fn has_final_record(&self, _stdout: &str) -> Option<bool> {
        None
    }

    /// The session id in `stdout`, unfiltered.
    fn session_id(&self, stdout: &str) -> Option<String>;

    /// The session record the built-in form prints before any work, read
    /// from the start of `stdout` alone; `None` for an agent that prints
    /// none (see `Capabilities::printed_session`), unfiltered.
    fn opening_session(&self, _stdout: &str) -> Option<String> {
        None
    }

    /// A session id printed in a human-readable header on stderr rather than
    /// in the output.
    fn stderr_session(&self, _stderr: &str) -> Option<String> {
        None
    }

    /// The tools and MCP servers the agent reports at start on its streaming
    /// form; `None` when the output names none.
    fn exposed_tools(&self, _stdout: &str) -> Option<ExposedTools> {
        None
    }

    /// The name of the tool that runs shell commands, through which an
    /// agent reaches apb's connectors (`apb connector call`); `None` when
    /// the output names no tools.
    fn shell_tool(&self) -> Option<&'static str> {
        None
    }

    /// The name tool `tool` of MCP server `server` goes by in the tool list
    /// the output reports; `None` when the output names no tools.
    fn mcp_tool(&self, _server: &str, _tool: &str) -> Option<String> {
        None
    }

    /// Whether MCP server `name` is one the agent lists whatever MCP config
    /// it was given (a connector of the agent's own account), so a strict
    /// MCP config cannot remove it and the exposure check does not count it
    /// as unexpected.
    fn managed_server(&self, _name: &str) -> bool {
        false
    }

    /// Whether `stdout` is this agent's machine output, a drifted one, or
    /// plain text that is allowed (see [`Conformance`]). `form_is_builtin`
    /// says whether the attempt ran the agent's built-in form.
    fn conformance(&self, form_is_builtin: bool, stdout: &str) -> Conformance {
        if !self.promises().reply {
            Conformance::PlainByDesign
        } else if self.reply(stdout).is_some() {
            Conformance::Machine
        } else if form_is_builtin {
            Conformance::Drifted
        } else {
            Conformance::PlainByDesign
        }
    }
}

/// What the built-in form's output must carry. The contract suite and the
/// doctor's probe both check an output against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Promises {
    /// The reply is wrapped in the machine output.
    pub reply: bool,
    /// The output names the session.
    pub session: bool,
    /// The output reports token usage.
    pub usage: bool,
    /// The output ends with a closing record.
    pub final_record: bool,
    /// The CLI marks a failed attempt in its output.
    pub error_flag: bool,
}

impl Promises {
    /// Everything a machine format carries, without an error flag.
    pub const MACHINE: Promises = Promises {
        reply: true,
        session: true,
        usage: true,
        final_record: true,
        error_flag: false,
    };
}

/// How an attempt's stdout relates to the agent's machine format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conformance {
    /// The machine format, read as such.
    Machine,
    /// The built-in form printed something that is not its machine format:
    /// the CLI's output format changed.
    Drifted,
    /// Plain text where plain text is allowed: an agent without a machine
    /// format, or a custom `agents.<id>.invocation` form.
    PlainByDesign,
}

/// The output of an agent without a machine format: plain text, whose only
/// readable fact is a session id in a JSON line, should the CLI print one.
pub struct PlainText {
    /// The JSON fields a session id would be printed under.
    pub session_fields: &'static [&'static str],
}

impl OutputFormat for PlainText {
    fn promises(&self) -> Promises {
        Promises::default()
    }

    fn session_id(&self, stdout: &str) -> Option<String> {
        json_string_field(stdout, self.session_fields)
    }
}

/// Where the numbers of an [`AgentUsage`] come from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// The agent CLI printed them, whatever it says about their origin.
    Reported,
    /// apb estimated them itself. Reserved: nothing records it yet, and a
    /// count the agent CLI printed is always [`UsageSource::Reported`].
    Estimated,
}

/// Token usage of one agent attempt, as the agent CLI reported it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUsage {
    /// Input tokens neither read from nor written to the prompt cache.
    pub input_tokens: u64,
    /// Output tokens, reasoning included.
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// The attempt's cost in US dollars as the CLI printed it, or as apb
    /// priced it from a catalog (`priced_from`), whatever its basis
    /// ([`Self::cost_basis`] says whether anybody is billed for it).
    /// A printed `0` is no figure and is left out: some CLIs print it on a
    /// login they do not price, and a non-zero figure is no proof of money
    /// spent either, since a CLI may price work a subscription covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<f64>,
    pub source: UsageSource,
    /// What `cost_usd` is: money billed, the list price of work a
    /// subscription covers, or not known. Absent in a journal an older apb
    /// wrote, and a value a newer apb wrote reads as unknown.
    #[serde(default)]
    pub cost_basis: CostBasis,
    /// Where `cost_usd` came from when apb priced it from a cloud catalog
    /// (`crate::cloud_models`), not the CLI. Absent for a printed cost and
    /// in older journals; a shape a newer apb wrote reads as absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_priced_from"
    )]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub priced_from: Option<PricedFrom>,
}

/// The catalog row a cost was priced from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricedFrom {
    /// The provider id (`near-ai-cloud`).
    pub catalog: String,
    pub model: String,
    /// The digest of the catalog the run snapshotted.
    pub digest: String,
    pub fetched_at: String,
}

fn lenient_priced_from<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PricedFrom>, D::Error> {
    let raw = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(raw).ok())
}

/// What a reported cost is. apb never guesses it: the agent's own module
/// tells it from facts it reads locally, else the person's declared
/// subscription for the agent or provider, else it stays unknown
/// (`crate::agents::cost_basis::resolve`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CostBasis {
    /// Money somebody is billed: an api key or a cloud account.
    Billed,
    /// What the work would cost at list price on a subscription that covers
    /// it: nobody is billed this figure.
    SubscriptionEquivalent,
    /// Not known. A run budget counts it as money (`docs/HOWTO-authoring.md`,
    /// "budget (run spend limit)").
    #[default]
    Unknown,
}

impl Serialize for CostBasis {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

/// A basis this binary does not know (a newer apb wrote it, in any shape:
/// another name, an object, `null`) reads as unknown, so the usage block
/// around it still decodes.
impl<'de> Deserialize<'de> for CostBasis {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        Ok(CostBasis::ALL
            .into_iter()
            .find(|b| raw.as_str() == Some(b.key()))
            .unwrap_or(CostBasis::Unknown))
    }
}

impl CostBasis {
    /// Every basis, billed first: the order a split lists them in.
    pub const ALL: [CostBasis; 3] = [
        CostBasis::Billed,
        CostBasis::SubscriptionEquivalent,
        CostBasis::Unknown,
    ];

    /// The journal's name of the basis.
    pub fn key(self) -> &'static str {
        match self {
            CostBasis::Billed => "billed",
            CostBasis::SubscriptionEquivalent => "subscription_equivalent",
            CostBasis::Unknown => "unknown",
        }
    }

    /// The plain words every surface shows next to a figure.
    pub fn label(self) -> &'static str {
        match self {
            CostBasis::Billed => "billed",
            CostBasis::SubscriptionEquivalent => "subscription equivalent, not billed",
            CostBasis::Unknown => "basis unknown",
        }
    }

    /// Whether a run budget's money limit counts a cost on this basis:
    /// everything but a subscription equivalent, an unknown basis being
    /// counted as the money it may be.
    pub fn counts_as_money(self) -> bool {
        self != CostBasis::SubscriptionEquivalent
    }
}

/// The reply an agent CLI's machine output carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// The CLI itself marked the attempt as failed (claude's `is_error`).
    pub is_error: bool,
}

impl Reply {
    /// A reply the CLI did not mark failed.
    pub fn ok(text: String) -> Self {
        Reply {
            text,
            is_error: false,
        }
    }
}

/// The string field `key` of a JSON object.
pub(super) fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The number field `key` of a JSON object, `0` when absent.
pub(super) fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// A reported cost, dropped when it is zero (no figure, see
/// [`AgentUsage::cost_usd`]).
pub(super) fn cost(v: Option<f64>) -> Option<f64> {
    v.filter(|c| c.is_finite() && *c > 0.0)
}

/// Every line of `stdout` that is a JSON object.
pub(super) fn json_lines(stdout: &str) -> impl Iterator<Item = Value> + '_ {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
}

/// Whether the last non-empty line of `stdout` is a JSON object whose `type`
/// is one of `kinds`.
pub(super) fn last_line_is(stdout: &str, kinds: &[&str]) -> bool {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .is_some_and(|v| str_at(&v, "type").is_some_and(|t| kinds.contains(&t)))
}

/// Scans each line of `raw` as a top-level JSON object and returns the LAST
/// non-empty string value found under any name in `fields` (the terminal event
/// wins, matching how a stream's final `result` event carries the id). `None`
/// when no line parses to such an object with such a field: the plain-text
/// one-shot case.
pub fn json_string_field(raw: &str, fields: &[&str]) -> Option<String> {
    let mut found: Option<String> = None;
    for val in json_lines(raw) {
        for f in fields {
            if let Some(s) = str_at(&val, f).filter(|s| !s.is_empty()) {
                found = Some(s.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod priced_from_tests {
    use super::*;

    /// `priced_from` is optional and lenient: absent decodes, a shape a
    /// newer apb wrote reads as absent, a priced usage round-trips.
    #[test]
    fn priced_from_decodes_leniently_and_round_trips() {
        let base = r#"{"input_tokens":1,"output_tokens":2,"source":"reported"}"#;
        let u: AgentUsage = serde_json::from_str(base).unwrap();
        assert_eq!(u.priced_from, None);
        let odd = r#"{"input_tokens":1,"output_tokens":2,"source":"reported","priced_from":[1,2]}"#;
        let u: AgentUsage = serde_json::from_str(odd).unwrap();
        assert_eq!(u.priced_from, None);
        let priced = AgentUsage {
            cost_usd: Some(0.175),
            priced_from: Some(PricedFrom {
                catalog: "near-ai-cloud".into(),
                model: "Qwen/Qwen3.8-27B".into(),
                digest: "sha256:ab".into(),
                fetched_at: "2026-11-09T00:00:00Z".into(),
            }),
            ..u
        };
        let back: AgentUsage =
            serde_json::from_str(&serde_json::to_string(&priced).unwrap()).unwrap();
        assert_eq!(back, priced);
    }
}
