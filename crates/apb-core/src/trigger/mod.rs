//! Start rules (`docs/TRIGGERS.md`): a rule a person writes, approves and
//! enables that starts a top-level run of a pinned playbook from an inbox
//! delivery. The rule (`rule.rs`), its templates (`render.rs`), its checks
//! (`validate.rs`, codes V260 to V267), its signed state and trigger
//! journal (`state.rs`) and the evaluation of one event (`evaluate.rs`)
//! live here; the sweep that walks the events lives in `apb-engine` and the
//! start itself in `apb-mcp`. Only a person changes a rule; the store and
//! the state are signed, bound to the registered project and read with no
//! link followed.

mod evaluate;
mod model;
mod render;
mod rule;
mod state;
mod validate;

pub use evaluate::{EventVerdict, evaluate_event};
pub use render::{Part, RenderError, parse, reads_event_text, render};
pub use rule::{
    Approval, InboxSource, MAX_PARAMS, NAME_MAX, RENDERED_MAX_CHARS, RULES_UNVERIFIED, Rule,
    RuleEntry, RuleLimits, RuleLock, RulePlaybook, RuleScope, RuleSource, RulesFile, SetOutcome,
    TriggerError, checked_rule_name, hold_rule_lock, parse_rule_document, remove_rule,
    remove_rule_locked, revoke_approval_locked, rule_digest, rules_unverified_detail,
    rules_unverified_text, set_enabled, set_enabled_locked, set_rule, set_rule_locked,
    store_approval, store_approval_locked,
};
pub use state::{
    ConditionMark, JournalRefusal, LOG_KEEP, LOG_REWRITE_AT, LogEntry, MAX_EVENTS_PER_PASS,
    MAX_STARTS_PER_PASS, RuleState, SourceId, TriggerStateFile, Verdict, append, delete_entry,
    is_code_text, journal_ids, journal_reads, journal_refused_text, needs_position,
    position_at_head, read, read_with_refusal,
};
#[cfg(any(test, feature = "testing"))]
pub use state::{journal_reads_count, reset_journal_reads};
pub use validate::{RuleContext, V260, V261, V262, V263, V264, V265, V266, V267, check_rule};
