//! Which models an agent takes: the vendor it is tied to, the closed list apb
//! offers for it, how a model id is spelled and checked, and which billing
//! account a model spends.

/// One agent's model rules. Every hook defaults to "no rule".
pub trait ModelPolicy: Send + Sync {
    /// The vendor a single-vendor agent is tied to (issue #42 finding 9),
    /// narrowing the curated models table to that vendor's rows; `None` for
    /// an aggregator, which keeps the whole table.
    fn vendor(&self) -> Option<&'static str> {
        None
    }

    /// The closed, apb-owned model list of the agent.
    fn closed_list(&self) -> ClosedList {
        ClosedList::Open
    }

    /// The id apb's lists name for `model`, or why the model is refused
    /// outright. `account_family` is the account family the agent's own
    /// settings name, read once per command (zcode); other agents ignore it.
    fn list_id(&self, model: &str, _account_family: &str) -> Result<String, String> {
        Ok(model.to_string())
    }

    /// Whether `id` is on the closed `list`, counting the spellings the CLI
    /// itself accepts.
    fn on_closed_list(&self, id: &str, list: &[String]) -> bool {
        list.iter().any(|m| m == id)
    }

    /// Whether `id`, though not on the closed list, is a well-formed id of a
    /// model family the agent knows: most likely a model newer than apb.
    fn newer_in_family(&self, _id: &str) -> bool {
        false
    }

    /// The spelling a profile stores `model` in, or why the model is refused.
    fn canonical_model(&self, model: &str) -> Result<String, String> {
        Ok(model.to_string())
    }

    /// The billing account `model` spends when it is not the agent itself
    /// (one login, one budget): what a non-transient (auth, budget) failure
    /// blocks for the rest of the fallback chain.
    fn billing_account(&self, _model: &str) -> Option<String> {
        None
    }
}

/// No model rules: any model, the whole curated table.
pub struct OpenModels;

impl ModelPolicy for OpenModels {}

/// The closed list an agent's models come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedList {
    /// No closed list: the options come from the curated table.
    Open,
    /// The models table's `claude_static_models`.
    ClaudeTable,
    /// The models table's `codex_static_models`.
    CodexTable,
    /// A list fixed in the binary because the spawn path enforces it.
    Fixed(&'static [&'static str]),
}
