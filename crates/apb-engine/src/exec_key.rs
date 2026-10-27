//! What one execution of a step is keyed by: its node and, for an item of a
//! list node, the item. Items of one node run at the same time, so
//! everything that pairs attempts (the open set, liveness, the attempt a
//! connector call binds to, the send ledger's scope, attempt memory) and
//! every per-attempt file name keys by both. Without an item every key and
//! file name is exactly what it was before list mode.

use crate::event::{Event, EventPayload};

/// One execution's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecKey<'a> {
    pub node: &'a str,
    pub item: Option<&'a str>,
}

impl<'a> ExecKey<'a> {
    /// The key of a plain node execution.
    pub fn node(node: &'a str) -> Self {
        ExecKey { node, item: None }
    }

    /// The key of `item` of list node `node`.
    pub fn new(node: &'a str, item: Option<&'a str>) -> Self {
        ExecKey { node, item }
    }

    /// The key of an `attempt_started`, `None` for any other event.
    pub fn of_started(p: &'a EventPayload) -> Option<Self> {
        match p {
            EventPayload::AttemptStarted { node, item, .. } => {
                Some(ExecKey::new(node, item.as_deref()))
            }
            _ => None,
        }
    }

    /// The key of an `attempt_finished`, `None` for any other event.
    pub fn of_finished(p: &'a EventPayload) -> Option<Self> {
        match p {
            EventPayload::AttemptFinished { node, item, .. } => {
                Some(ExecKey::new(node, item.as_deref()))
            }
            _ => None,
        }
    }

    /// The key of an `attempt_exited`, `None` for any other event.
    pub fn of_exited(p: &'a EventPayload) -> Option<Self> {
        match p {
            EventPayload::AttemptExited { node, item, .. } => {
                Some(ExecKey::new(node, item.as_deref()))
            }
            _ => None,
        }
    }

    /// Whether `e` is an `attempt_started` of this key.
    pub fn started(&self, e: &Event) -> bool {
        ExecKey::of_started(&e.payload) == Some(*self)
    }

    /// Whether `e` is an `attempt_finished` of this key.
    pub fn finished(&self, e: &Event) -> bool {
        ExecKey::of_finished(&e.payload) == Some(*self)
    }

    /// The per-attempt file stem: `<node>-<attempt>` without an item
    /// (unchanged), `<node>.i-<32 hex of the item's digest>-<attempt>` with
    /// one, so an item id never reaches a path.
    pub fn stem(&self, attempt: u32) -> String {
        match self.item {
            None => format!("{}-{attempt}", self.node),
            Some(item) => format!("{}.i-{}-{attempt}", self.node, item_hash(item)),
        }
    }

    /// The per-attempt directory segment of an isolated node:
    /// `<attempt>` without an item, `i-<hash>-<attempt>` with one.
    pub fn work_segment(&self, attempt: u32) -> String {
        match self.item {
            None => attempt.to_string(),
            Some(item) => format!("i-{}-{attempt}", item_hash(item)),
        }
    }

    /// The title a fresh agent session is given, findable later.
    pub fn session_title(&self, run_id: &str, attempt: u32) -> String {
        match self.item {
            None => format!("apb {run_id} {} {attempt}", self.node),
            Some(_) => format!("apb {run_id} {}", self.stem(attempt)),
        }
    }

    /// The owned item id.
    pub fn item_owned(&self) -> Option<String> {
        self.item.map(str::to_string)
    }
}

pub use apb_core::list::items::item_hash;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_key_keeps_every_name_it_had() {
        let k = ExecKey::node("work");
        assert_eq!(k.stem(3), "work-3");
        assert_eq!(k.work_segment(3), "3");
        assert_eq!(k.session_title("r1", 3), "apb r1 work 3");
    }

    #[test]
    fn an_item_key_names_files_by_a_digest_of_the_item() {
        let k = ExecKey::new("triage", Some("../../etc"));
        let stem = k.stem(2);
        assert!(
            stem.starts_with("triage.i-") && stem.ends_with("-2"),
            "{stem}"
        );
        assert!(!stem.contains('/'), "an item id never reaches a path");
        assert_ne!(stem, ExecKey::new("triage", Some("other")).stem(2));
        assert_eq!(k.work_segment(2), format!("i-{}-2", item_hash("../../etc")));
    }
}
