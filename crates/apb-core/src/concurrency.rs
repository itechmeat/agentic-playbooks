//! The `concurrency:` block: at most one active and one waiting run per
//! rendered key, what the playbook says about a busy key. Its schema and
//! the reason words of a replaced run; the store and the admission live in
//! the engine ([`apb_engine::concurrency`]).

use serde::{Deserialize, Serialize};

/// One run per rendered key: what the playbook's `concurrency:` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyBlock {
    /// A template over `params.*`; absent: one key for the whole playbook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default)]
    pub on_busy: OnBusy,
}

/// What a start does when the key is busy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnBusy {
    /// The newest start waits; a newer one replaces the waiting one.
    #[default]
    QueueLatest,
    /// The newest start also stops the active one gracefully.
    Supersede,
    /// A busy key refuses the start `concurrency_busy`.
    Refuse,
}

impl OnBusy {
    /// The machine-facing word (`queue_latest`, `supersede`, `refuse`).
    pub fn as_str(self) -> &'static str {
        match self {
            OnBusy::QueueLatest => "queue_latest",
            OnBusy::Supersede => "supersede",
            OnBusy::Refuse => "refuse",
        }
    }
}

/// The longest key template, in characters (V210).
pub const KEY_MAX_CHARS: usize = 500;
/// The longest key text shown, in characters.
pub const DISPLAY_MAX_CHARS: usize = 120;
/// The MAC domain of a rendered key.
pub const KEY_DOMAIN: &str = "apb-concurrency-key-v1";
/// The prefix of the reason of every run a newer start replaced.
pub const SUPERSEDED: &str = "superseded";
/// The rendered key of an absent `key`.
pub const WHOLE_PLAYBOOK_KEY: &str = "*";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_round_trips() {
        let yaml = "key: \"{{params.account}}\"\non_busy: supersede\n";
        let block: ConcurrencyBlock = yaml_serde::from_str(yaml).unwrap();
        assert_eq!(block.key.as_deref(), Some("{{params.account}}"));
        assert_eq!(block.on_busy, OnBusy::Supersede);
        let back: ConcurrencyBlock =
            yaml_serde::from_str(&yaml_serde::to_string(&block).unwrap()).unwrap();
        assert_eq!(block, back);
    }

    #[test]
    fn on_busy_defaults_to_queue_latest() {
        let block: ConcurrencyBlock = yaml_serde::from_str("key: k\n").unwrap();
        assert_eq!(block.on_busy, OnBusy::QueueLatest);
        let bare: ConcurrencyBlock = yaml_serde::from_str("{}\n").unwrap();
        assert_eq!(bare.on_busy, OnBusy::QueueLatest);
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let err = yaml_serde::from_str::<ConcurrencyBlock>("key: k\nextra: 1\n").unwrap_err();
        assert!(
            err.to_string().contains("unknown"),
            "an unknown field must be refused: {err}"
        );
    }
}
