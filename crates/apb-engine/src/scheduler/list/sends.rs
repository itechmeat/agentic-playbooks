//! The sends of a list node's items, folded once from one journal read.

use std::collections::HashMap;

use apb_core::connector::idempotency::IdempotencyMode;

use crate::connector::intent::{
    IntentState, Ledger, chain_verdict, item_scope_floor, passed_floors,
};
use crate::event::Event;
use crate::exec_key::ExecKey;

/// The sends of one list node's items, folded once from one journal read:
/// what a batch's settlements and a visit's start ask of every item,
/// without a fold per item.
pub(super) struct ItemSends {
    ledger: Ledger,
    /// Per item, the first seq after its last passed settlement.
    passed: HashMap<String, u64>,
    now_ms: u64,
}

impl ItemSends {
    pub(super) fn of(events: &[Event], node: &str) -> Self {
        ItemSends {
            ledger: Ledger::fold(events),
            passed: passed_floors(events, node),
            now_ms: apb_core::clock::now_ms_u64(),
        }
    }

    /// The mode of a send of the item `key` whose outcome is unknown, if
    /// any, within the item's effect scope.
    pub(super) fn unknown(&self, key: ExecKey<'_>) -> Option<IdempotencyMode> {
        let passed = key
            .item
            .and_then(|i| self.passed.get(i))
            .copied()
            .unwrap_or(0);
        let alive = &crate::liveness::pid_alive;
        let floor = item_scope_floor(&self.ledger, key, passed, self.now_ms, alive);
        self.ledger
            .records()
            .iter()
            .filter(|r| r.seq >= floor && r.key.exec_key() == key && r.superseded_by.is_none())
            .find_map(|tip| {
                let v = chain_verdict(&self.ledger, tip, self.now_ms);
                matches!(
                    v.state,
                    IntentState::Unknown { .. } | IntentState::Pending { .. }
                )
                .then_some(v.member.mode)
            })
    }
}
