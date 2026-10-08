//! The model each attempt actually ran on (issue #193 part C), for the run
//! surfaces (`run_status`, `apb runs <id>`, the dashboard node list), `apb
//! stats` and `apb doctor --run`.
//!
//! A CLI attempt runs on the model its `attempt_started` names (the model the
//! engine passed to the agent CLI). A host attempt (a host-mode step, or a
//! step that fell back to the host) runs on whatever the host chose: the
//! model is the one its `host_task_submitted` reports, and unknown when the
//! host reported none. The profile's model is only a hint there, so
//! `attempt_started.model` of a host attempt is never read as the actual one.
//!
//! The expected model of a node is the primary model of the profile the run
//! bound it to (the run manifest's `node_bindings`, first chain step). An
//! attempt is a mismatch when both are known and name different models
//! (see [`same_model`]): a fallback step on another model counts too, which
//! is what a reader comparing runs wants to see. The run retro reads the
//! same fold (`AttemptWalk`), so every surface agrees on the model.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::event::{Event, EventPayload};

/// `executed_by` of an attempt the engine ran through an agent CLI.
pub const EXECUTED_BY_CLI: &str = "cli";
/// `executed_by` of an attempt a host executed (host execution mode).
pub const EXECUTED_BY_HOST: &str = "host";
/// The `attempt_started.agent` of a host attempt.
const HOST_AGENT: &str = "host";

/// One attempt and the model it ran on.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptModel {
    pub node: String,
    pub attempt: u32,
    /// [`EXECUTED_BY_CLI`] or [`EXECUTED_BY_HOST`].
    pub executed_by: String,
    /// The agent CLI of a CLI attempt.
    pub agent: Option<String>,
    /// The model the attempt actually ran on; `None` when a host did not
    /// report it.
    pub model: Option<String>,
    /// The primary model of the node's profile, from the run manifest.
    pub expected: Option<String>,
    /// Both models are known and differ.
    pub mismatch: bool,
}

/// Words a model id may carry beside the model itself: the vendor or the
/// release channel. `claude-opus-4-1` and `opus` name one model; a word
/// outside this list (`flash`, `mini`, `haiku`) names another one.
const NEUTRAL_WORDS: &[&str] = &[
    "claude",
    "anthropic",
    "openai",
    "google",
    "gemini",
    "latest",
    "preview",
    "exp",
    "experimental",
    "stable",
];

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether two model names name the same model. Both are split into
/// lowercase tokens on every non-alphanumeric character. They match when
/// the token lists are equal, or when the shorter list is a subsequence of
/// the longer one and every extra token of the longer one is a version or
/// date number (all digits) or a vendor or channel word ([`NEUTRAL_WORDS`]).
/// So `opus` matches `claude-opus-4-1` and `sonnet` matches
/// `claude-sonnet-4-5-20250929`, while `glm-5.3` does not match
/// `glm-5.3-flash` and `gpt-5` does not match `gpt-5-mini`: a variant word
/// is a different model, which is what the mismatch flag is for.
pub fn same_model(a: &str, b: &str) -> bool {
    let (a, b) = (tokens(a), tokens(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let mut want = short.iter().peekable();
    for t in long {
        if want.peek() == Some(&t) {
            want.next();
        } else if !(t.chars().all(|c| c.is_ascii_digit()) || NEUTRAL_WORDS.contains(&t.as_str())) {
            return false;
        }
    }
    want.peek().is_none()
}

/// The primary model of each node's profile, from the run manifest
/// (`node_bindings` and the profile's first chain step). Empty when the run
/// has no manifest.
pub fn primary_models(run_dir: &Path) -> BTreeMap<String, String> {
    let Ok(Some(m)) = crate::manifest::read(run_dir) else {
        return BTreeMap::new();
    };
    m.node_bindings
        .iter()
        .filter_map(|(node, key)| {
            let p = m.profiles.iter().find(|p| &p.key() == key)?;
            Some((node.clone(), p.chain.first()?.model.clone()))
        })
        .filter(|(_, model)| !model.trim().is_empty())
        .collect()
}

/// The journal fold behind [`attempt_models`], shared with the run retro
/// (`run_retro`), so both read an attempt's actual model one way.
///
/// One slot per attempt execution: a loop back into a node numbers its
/// attempts from 1 again, so a `(node, attempt)` whose latest slot already
/// started (or finished) opens a new slot. A host attempt's first
/// `host_task_requested` comes before its `attempt_started`, a blocked
/// task's follow-up after it; both land on the same slot.
#[derive(Debug, Default)]
pub(crate) struct AttemptWalk {
    slots: Vec<Slot>,
    /// Host task id -> slot index.
    tasks: BTreeMap<String, usize>,
}

#[derive(Debug)]
struct Slot {
    model: AttemptModel,
    started: bool,
    finished: bool,
}

impl AttemptWalk {
    /// The open slot of `(node, attempt)`, or a new one. `starting`: the
    /// event is the attempt's `attempt_started`, so a slot that already
    /// started belongs to an earlier execution.
    fn slot(&mut self, node: &str, attempt: u32, starting: bool) -> usize {
        let open = self
            .slots
            .iter()
            .rposition(|s| s.model.node == node && s.model.attempt == attempt)
            .filter(|&i| !self.slots[i].finished && !(starting && self.slots[i].started));
        if let Some(i) = open {
            return i;
        }
        self.slots.push(Slot {
            model: AttemptModel {
                node: node.to_string(),
                attempt,
                executed_by: EXECUTED_BY_CLI.to_string(),
                agent: None,
                model: None,
                expected: None,
                mismatch: false,
            },
            started: false,
            finished: false,
        });
        self.slots.len() - 1
    }

    /// Folds one event; returns the index of the slot it belongs to, when
    /// it is an attempt event (started, finished, host task requested or
    /// submitted).
    pub(crate) fn step(&mut self, e: &Event) -> Option<usize> {
        match &e.payload {
            EventPayload::HostTaskRequested {
                task_id,
                node,
                attempt,
                ..
            } => {
                let i = self.slot(node, *attempt, false);
                self.tasks.insert(task_id.clone(), i);
                let s = &mut self.slots[i].model;
                if s.executed_by != EXECUTED_BY_HOST {
                    s.executed_by = EXECUTED_BY_HOST.to_string();
                    s.agent = None;
                    s.model = None;
                }
                Some(i)
            }
            EventPayload::AttemptStarted {
                node,
                attempt,
                agent,
                model,
                ..
            } => {
                let i = self.slot(node, *attempt, true);
                self.slots[i].started = true;
                let s = &mut self.slots[i].model;
                if agent == HOST_AGENT {
                    s.executed_by = EXECUTED_BY_HOST.to_string();
                } else if s.executed_by == EXECUTED_BY_CLI {
                    s.agent = Some(agent.clone());
                    s.model = model.clone().filter(|m| !m.trim().is_empty());
                }
                Some(i)
            }
            EventPayload::AttemptFinished { node, attempt, .. } => {
                let i = self.slot(node, *attempt, false);
                self.slots[i].finished = true;
                Some(i)
            }
            EventPayload::HostTaskSubmitted {
                task_id,
                model,
                submitted_by,
                ..
            } => {
                let i = *self.tasks.get(task_id)?;
                // An engine closure (expired, cancelled, ...) is no host reply.
                if let Some(m) = model
                    && submitted_by != "engine"
                    && !m.trim().is_empty()
                {
                    self.slots[i].model.model = Some(m.clone());
                }
                Some(i)
            }
            _ => None,
        }
    }

    /// The attempt at `i`, as folded so far.
    pub(crate) fn get(&self, i: usize) -> &AttemptModel {
        &self.slots[i].model
    }

    /// Every attempt in the order it started, checked against `expected`.
    pub(crate) fn finish(self, expected: &BTreeMap<String, String>) -> Vec<AttemptModel> {
        self.slots
            .into_iter()
            .map(|s| {
                let mut a = s.model;
                a.expected = expected.get(&a.node).cloned();
                a.mismatch =
                    matches!((&a.model, &a.expected), (Some(m), Some(x)) if !same_model(m, x));
                a
            })
            .collect()
    }
}

/// Every attempt of `events` in the order it started, with the model it ran
/// on, checked against `expected` (node to model, see [`primary_models`]).
pub fn attempt_models(events: &[Event], expected: &BTreeMap<String, String>) -> Vec<AttemptModel> {
    let mut walk = AttemptWalk::default();
    for e in events {
        walk.step(e);
    }
    walk.finish(expected)
}

/// [`attempt_models`] of the run at `run_dir`, its expected models read
/// from the run manifest.
pub fn run_attempt_models(run_dir: &Path, events: &[Event]) -> Vec<AttemptModel> {
    attempt_models(events, &primary_models(run_dir))
}

/// One line per mismatched attempt, for a terminal: `node attempt 1 ran on
/// M (host), the profile names P`.
pub fn mismatch_lines(models: &[AttemptModel]) -> Vec<String> {
    models
        .iter()
        .filter(|a| a.mismatch)
        .map(|a| {
            format!(
                "{} attempt {} ran on {} ({}), the profile names {}",
                a.node,
                a.attempt,
                a.model.as_deref().unwrap_or("?"),
                a.executed_by,
                a.expected.as_deref().unwrap_or("?")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, v: serde_json::Value) -> Event {
        let mut v = v;
        v["seq"] = seq.into();
        v["ts"] = seq.into();
        serde_json::from_value(v).expect("event")
    }

    #[test]
    fn a_reentered_node_and_a_host_attempt_each_get_their_own_slot() {
        let events = [
            ev(
                1,
                serde_json::json!({"type": "attempt_started", "node": "a", "attempt": 1, "agent": "claude", "model": "sonnet"}),
            ),
            ev(
                2,
                serde_json::json!({"type": "attempt_finished", "node": "a", "attempt": 1, "status": "succeeded"}),
            ),
            // The loop back into `a` numbers its attempts from 1 again; its
            // host task is requested before the attempt starts.
            ev(
                3,
                serde_json::json!({"type": "host_task_requested", "task_id": "t1", "node": "a", "attempt": 1}),
            ),
            ev(
                4,
                serde_json::json!({"type": "attempt_started", "node": "a", "attempt": 1, "agent": "host", "model": "sonnet"}),
            ),
            ev(
                5,
                serde_json::json!({"type": "host_task_submitted", "task_id": "t1", "status": "succeeded", "submitted_by": "host", "model": "glm-5.3-flash"}),
            ),
            ev(
                6,
                serde_json::json!({"type": "attempt_finished", "node": "a", "attempt": 1, "status": "succeeded"}),
            ),
        ];
        let expected = BTreeMap::from([("a".to_string(), "claude-sonnet-4-5".to_string())]);
        let models = attempt_models(&events, &expected);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].executed_by, EXECUTED_BY_CLI);
        assert_eq!(models[0].model.as_deref(), Some("sonnet"));
        assert!(!models[0].mismatch);
        assert_eq!(models[1].executed_by, EXECUTED_BY_HOST);
        assert_eq!(models[1].model.as_deref(), Some("glm-5.3-flash"));
        assert!(models[1].mismatch);
    }

    #[test]
    fn aliases_and_dated_ids_are_the_same_model() {
        assert!(same_model("opus", "claude-opus-4-1"));
        assert!(same_model("sonnet", "claude-sonnet-4-5-20250929"));
        assert!(same_model(
            "claude-sonnet-4-5",
            "anthropic/claude-sonnet-4-5-latest"
        ));
        assert!(same_model("GLM-5.3-Flash", "glm_5.3_flash"));
        assert!(same_model("gpt-5", "openai/gpt-5"));
    }

    #[test]
    fn a_variant_word_or_another_family_is_another_model() {
        assert!(!same_model("glm-5.3", "glm-5.3-flash"));
        assert!(!same_model("gpt-5", "gpt-5-mini"));
        assert!(!same_model("sonnet", "haiku"));
        assert!(!same_model("sonnet", "claude-haiku-4-5"));
        assert!(!same_model("GLM-5.3-Flash", "opus"));
        assert!(!same_model("", "opus"));
        assert!(!same_model("-", "opus"));
    }
}
