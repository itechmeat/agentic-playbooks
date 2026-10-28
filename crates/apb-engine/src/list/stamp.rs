//! Marks the events an item's execution writes with the item they belong
//! to, so the drive never builds item-scoped events by hand: the journal of
//! an item (`Journal::for_item`) stamps every event it appends.

use crate::event::EventPayload;

impl EventPayload {
    /// Sets `item` on the variants that carry one (the attempt lifecycle, a
    /// connector call, a send intent, a decision, the agent env, the tool exposure); every other event is
    /// left as it is. An item already set is kept.
    pub fn stamp_item(&mut self, id: &str) {
        match self {
            EventPayload::AttemptStarted { item, .. }
            | EventPayload::AttemptExited { item, .. }
            | EventPayload::AttemptFinished { item, .. }
            | EventPayload::ConnectorCall { item, .. }
            | EventPayload::SendIntentRecorded { item, .. }
            | EventPayload::DecisionMade { item, .. }
            | EventPayload::AgentEnvApplied { item, .. }
            | EventPayload::ToolExposureMismatch { item, .. }
                if item.is_none() =>
            {
                *item = Some(id.to_string());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_item_scoped_events_are_stamped() {
        let mut exited = EventPayload::AttemptExited {
            node: "n".into(),
            attempt: 1,
            item: None,
        };
        exited.stamp_item("a");
        assert!(
            matches!(exited, EventPayload::AttemptExited { item: Some(ref i), .. } if i == "a")
        );
        let mut retry = EventPayload::RetryStarted {
            node: "n".into(),
            attempt: 2,
        };
        retry.stamp_item("a");
        assert!(matches!(retry, EventPayload::RetryStarted { .. }));
        let mut env = crate::agent_env::applied_event(&Default::default(), "n", 1, "claude");
        env.stamp_item("a");
        assert!(matches!(env, EventPayload::AgentEnvApplied { item: Some(ref i), .. } if i == "a"));
    }
}
