//! One list visit as the batches and the checks see it: the node, its
//! spec and items, the shared template, the stop flags, and how an item's
//! files and settlements are written.

use std::path::PathBuf;

use apb_core::list::ListSpec;
use apb_core::list::items::Item;
use apb_core::schema::Node;
use serde_json::{Map, Value};

use super::super::outcome::NodeCtx;
use crate::error::EngineError;
use crate::event::EventPayload;
use crate::exec_key::{ExecKey, item_hash};
use crate::list::ledger::{ItemRecord, ListFold};
use crate::list::prompt::ran_before;

/// The item's declared outputs as a JSON object, `None` when the output is
/// not one.
pub(super) fn output_object(output: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(output).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// The most bytes of an item's declared outputs a settlement keeps.
const SETTLED_OUTPUT_MAX_BYTES: usize = 2048;

/// An item's files under the run directory.
pub(super) struct ItemFiles {
    pub item: PathBuf,
    pub output: Option<PathBuf>,
}

/// One visit of one list node.
pub(super) struct ListRun<'r, 'a, 'j> {
    pub ctx: &'r NodeCtx<'a, 'j>,
    pub node: &'a Node,
    pub spec: &'a ListSpec,
    /// The visit's items, in order.
    pub items: Vec<Item>,
    /// The node template rendered once (empty for a script step).
    pub template: String,
    pub stop: Option<&'r crate::stop::StopFlags>,
    /// The drive's control cursor (see `ExecOptions::control_after`).
    pub control_after: Option<u64>,
    /// The run's budget gate, weighed before each batch.
    pub budget: Option<&'r crate::scheduler::budget_gate::DriveGate>,
    pub workers: usize,
    /// The execution root (connector secrets resolve against it).
    pub root: PathBuf,
    pub spawn_env: Vec<(String, String)>,
    /// The node's visit before this one, read under `rerun: not_passed`.
    pub earlier: Option<ListFold>,
}

impl ListRun<'_, '_, '_> {
    pub(super) fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// What `id` did before this run of it: its record in this visit once
    /// it ran, else its record of the visit before when that settled the
    /// same content (an item that changed is another item to its step).
    pub(super) fn previous<'f>(&'f self, fold: &'f ListFold, id: &str) -> Option<&'f ItemRecord> {
        let digest = self.item(id).map(|i| i.digest.as_str());
        fold.item(id).filter(|r| ran_before(r)).or_else(|| {
            self.earlier
                .as_ref()
                .and_then(|f| f.item(id))
                .filter(|r| r.digest.is_some() && r.digest.as_deref() == digest)
        })
    }

    pub(super) fn key<'k>(&'k self, id: &'k str) -> ExecKey<'k> {
        ExecKey::new(&self.node.id, Some(id))
    }

    /// Writes the item's JSON (and, when given, its step's output) under the
    /// run directory, named by a digest of its id.
    pub(super) fn files(&self, item: &Item, output: Option<&str>) -> std::io::Result<ItemFiles> {
        let run_dir = self.ctx.run_dir;
        let dir = run_dir.join("list-items").join(&self.node.id);
        apb_core::fsutil::create_dir_under(run_dir, &run_dir.join("list-items"))?;
        apb_core::fsutil::create_dir_under(run_dir, &dir)?;
        let stem = item_hash(&item.id);
        let path = dir.join(format!("{stem}.json"));
        let text = serde_json::to_string(&item.value).unwrap_or_default();
        apb_core::fsutil::atomic_write_under(run_dir, &path, text.as_bytes())?;
        let output = match output {
            Some(o) => {
                let p = dir.join(format!("{stem}.output.json"));
                apb_core::fsutil::atomic_write_under(run_dir, &p, o.as_bytes())?;
                Some(p)
            }
            None => None,
        };
        Ok(ItemFiles { item: path, output })
    }

    /// The item env every process of an item check gets.
    pub(super) fn item_env(
        &self,
        item: &Item,
        files: &ItemFiles,
    ) -> Result<Vec<(&'static str, String)>, String> {
        crate::adapter::item_env(&item.id, &files.item)
    }

    /// Journals one item's settlement.
    pub(super) fn settle(&self, s: Settlement<'_>) -> Result<(), EngineError> {
        self.ctx.journal.append(EventPayload::ListItemSettled {
            node: self.node.id.clone(),
            item: s.id.to_string(),
            state: s.state.to_string(),
            verdict: s.verdict.map(str::to_string),
            note: s.note,
            reason: s.reason.map(str::to_string),
            output: s.output,
            round: s.round,
            carried: s.carried,
            check: s.check.map(str::to_string),
            // Every settlement names the content it settled, so a later
            // visit tells the same item from a changed one.
            digest: s
                .digest
                .or_else(|| self.item(s.id).map(|i| i.digest.as_str()))
                .map(str::to_string),
        })
    }
}

/// One `list_item_settled`.
#[derive(Default)]
pub(super) struct Settlement<'s> {
    pub id: &'s str,
    pub state: &'s str,
    pub verdict: Option<&'s str>,
    pub note: Option<String>,
    pub reason: Option<&'s str>,
    pub output: Option<String>,
    pub round: u32,
    pub carried: bool,
    pub check: Option<&'s str>,
    pub digest: Option<&'s str>,
}

/// The compact JSON of an item output's declared fields. Over
/// [`SETTLED_OUTPUT_MAX_BYTES`] it is `{"truncated": true}`, so a reader
/// sees the outputs were too large rather than absent.
pub(super) fn settled_output(spec: &ListSpec, output: &str) -> Option<String> {
    let obj = output_object(output)?;
    let kept: Map<String, Value> = if spec.item_outputs.is_empty() {
        obj
    } else {
        obj.into_iter()
            .filter(|(k, _)| spec.item_outputs.contains(k))
            .collect()
    };
    let text = serde_json::to_string(&kept).ok()?;
    if text.len() <= SETTLED_OUTPUT_MAX_BYTES {
        Some(text)
    } else {
        Some(r#"{"truncated":true}"#.to_string())
    }
}
