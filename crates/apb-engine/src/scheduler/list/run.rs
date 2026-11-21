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
const SETTLED_OUTPUT_MAX_BYTES: usize = apb_core::list::LIST_SKIP_OUTPUT_MAX_BYTES;

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
    /// The visit may skip and mint skip digests: the node declares `skip`
    /// and the visit's start recorded no refusal.
    pub skip_allowed: std::sync::atomic::AtomicBool,
    /// The work fingerprint of the visit ([`ListRun::work`]), computed once.
    pub work: std::sync::OnceLock<Result<String, String>>,
    /// The scripts digest the run pinned at start, read once per visit.
    pub pin: std::sync::OnceLock<Option<String>>,
    /// Why this visit's work mints no skip digest, decided once per visit
    /// ([`ListRun::stamp_block`]).
    pub stamp_block: std::sync::OnceLock<Option<String>>,
    /// Set once the run's scripts copy was seen to differ from the pin at an
    /// item's start or at a settlement: from then on the visit stamps
    /// nothing (fail closed), whatever the copy holds later.
    pub scripts_diverged: std::sync::atomic::AtomicBool,
    /// The latch was journaled on a settlement of this execution.
    pub latch_journaled: std::sync::atomic::AtomicBool,
    /// Set once a settlement found the machine's MCP pins for the granted
    /// accounts differ from those the visit's work names.
    pub pins_diverged: std::sync::atomic::AtomicBool,
    /// Set once a settlement found a runtime the visit's work names no
    /// longer the file the run resolved.
    pub runtimes_diverged: std::sync::atomic::AtomicBool,
    /// The visit's resolved execution ([`ListRun::execution`]), once.
    pub execution:
        std::sync::OnceLock<Result<std::sync::Arc<super::super::cache::VisitExecution>, String>>,
}

impl ListRun<'_, '_, '_> {
    /// The visit's resolved execution ([`super::super::cache::VisitExecution`]),
    /// resolved once from the run as it stands: the directory (the drive's
    /// tree `ctx.workdir` under the node's own `workdir`), the runtimes, the
    /// check directory, `PATH`, the check policy and the MCP pins. The work
    /// is computed from it, and every item of a skip visit executes with it.
    pub(super) fn execution(
        &self,
    ) -> Result<std::sync::Arc<super::super::cache::VisitExecution>, String> {
        self.execution
            .get_or_init(|| {
                super::super::cache::visit_execution(
                    self.ctx.playbook,
                    self.node,
                    self.ctx.workdir,
                    self.ctx.run_dir,
                    self.ctx.run_id,
                    self.ctx.state,
                    self.ctx.cfg,
                    self.workers,
                )
                .map(std::sync::Arc::new)
            })
            .clone()
    }

    /// The work fingerprint of this visit (incremental lists D2), computed
    /// once from the run as it stands; `Err` with the skip refusal's
    /// reason when a part does not read.
    pub(super) fn work(&self) -> Result<String, String> {
        self.work
            .get_or_init(|| {
                let events = crate::event::read_all(self.ctx.run_dir).map_err(|e| e.to_string())?;
                // Computed from the visit's resolved execution, the value
                // every item of the visit then executes with.
                let exec = self.execution().map(|e| (*e).clone());
                let src = super::super::cache::WorkSource {
                    run_dir: self.ctx.run_dir,
                    events: &events,
                    playbook: self.ctx.playbook,
                    node: self.node,
                    cfg: self.ctx.cfg,
                    decisions: self.ctx.decisions.map(|d| d.settings()),
                    exec: &exec,
                };
                super::super::cache::work_inputs(&src)
                    .map(|w| super::super::cache::work_fingerprint(&w))
            })
            .clone()
    }

    /// The part of an item its skip digest covers: its content digest, or
    /// the digest of its `unless_changed` fields.
    pub(super) fn item_part(&self, item: &Item) -> String {
        match self
            .spec
            .skip
            .as_ref()
            .and_then(|s| s.unless_changed.as_ref())
        {
            Some(fields) => apb_core::list::items::fields_digest(&item.value, fields),
            None => item.digest.clone(),
        }
    }

    /// Whether the run's live `scripts/` copy matches the digest the run
    /// pinned at start (the pin read once per visit, the tree digested now).
    pub(super) fn scripts_pinned(&self) -> bool {
        let pin = self.pin.get_or_init(|| {
            crate::event::read_all(self.ctx.run_dir)
                .ok()
                .and_then(|events| super::super::cache::pinned_scripts(&events).ok())
        });
        match (pin, crate::run_scripts::digest(self.ctx.run_dir)) {
            (Some(p), Ok(now)) => *p == now,
            _ => false,
        }
    }

    /// The scripts check of one item end, its start or its settlement: the
    /// live copy digested now against the pin. A mismatch latches
    /// [`ListRun::scripts_diverged`], so no later settlement of the visit
    /// stamps either. True only while no end of any item saw a difference.
    /// Only a list that skips pays for the digest.
    pub(super) fn scripts_held(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.spec.skip.is_none() {
            return true;
        }
        if !self.scripts_pinned() {
            self.scripts_diverged.store(true, Ordering::SeqCst);
        }
        !self.scripts_diverged.load(Ordering::SeqCst)
    }

    /// Why this visit's work is not the work the fingerprint covers, so no
    /// settlement of it mints a skip digest: a supervisor note in force or
    /// a revised pilot's guidance, both of which reach every item prompt.
    /// Decided once per visit from the journal as the visit (new, reopened
    /// or resumed) starts: either changes only through a drive that ends
    /// this visit's execution first.
    fn stamp_block(&self) -> Option<String> {
        self.stamp_block
            .get_or_init(|| {
                let events = crate::event::read_all(self.ctx.run_dir).ok()?;
                if !crate::context::applied_supervisor_notes(&events).is_empty() {
                    return Some("a supervisor note is in force".to_string());
                }
                ListFold::of(&events, &self.node.id)
                    .and_then(|f| f.pilot)
                    .and_then(|p| p.guidance)
                    .map(|_| "a revised pilot's guidance is applied".to_string())
            })
            .clone()
    }

    /// Whether the machine's MCP pins for the granted accounts still are the
    /// ones the visit's work names, read at this settlement; a difference
    /// (or a store that does not read) latches the visit. True at once for
    /// a node with no granted account.
    fn pins_held(&self) -> bool {
        use std::sync::atomic::Ordering;
        let Ok(exec) = self.execution() else {
            return false;
        };
        if exec.mcp_pins.is_empty() {
            return !self.pins_diverged.load(Ordering::SeqCst);
        }
        match super::super::cache::mcp_pins(self.ctx.run_dir, self.node) {
            Ok(now) if now == exec.mcp_pins => {}
            _ => self.pins_diverged.store(true, Ordering::SeqCst),
        }
        !self.pins_diverged.load(Ordering::SeqCst)
    }

    /// Whether every runtime the visit's work names is still the file the
    /// run resolved, read at this settlement; a difference (or an execution
    /// that did not resolve) latches the visit.
    fn runtimes_held(&self) -> bool {
        use std::sync::atomic::Ordering;
        let Ok(exec) = self.execution() else {
            return false;
        };
        if super::super::cache::runtime_changed(&exec.runtimes).is_some() {
            self.runtimes_diverged.store(true, Ordering::SeqCst);
        }
        !self.runtimes_diverged.load(Ordering::SeqCst)
    }

    /// The reason the visit stopped stamping, for the first settlement
    /// journaled after the latch closed; `None` before it and after it was
    /// journaled once.
    fn latch_note(&self) -> Option<String> {
        use std::sync::atomic::Ordering;
        let scripts = self.scripts_diverged.load(Ordering::SeqCst);
        let pins = self.pins_diverged.load(Ordering::SeqCst);
        let runtimes = self.runtimes_diverged.load(Ordering::SeqCst);
        ((scripts || pins || runtimes) && !self.latch_journaled.swap(true, Ordering::SeqCst)).then(|| {
            if scripts {
                format!(
                    "{}: the run's scripts copy differed from the one pinned at start at an item's start or settlement, so no item settled after it is stamped",
                    crate::list::skip::SCRIPTS_DIVERGED
                )
            } else if runtimes {
                format!(
                    "{}: a runtime the visit's work names was replaced after the run resolved it, so no item settled after it is stamped",
                    crate::list::skip::RUNTIME_CHANGED
                )
            } else {
                format!(
                    "{}: the machine's MCP tool approvals for a granted account changed during the visit, so no item settled after it is stamped",
                    crate::list::skip::MCP_PINS_CHANGED
                )
            }
        })
    }

    /// The one rule for stamps: a fresh settlement mints a skip digest only
    /// when the item's work ran under exactly the configuration the work
    /// fingerprint covers and its item check ran to a verdict and passed
    /// (or the list declares no check). None for a visit that may not skip,
    /// under a supervisor note or a revised pilot's guidance, after a check
    /// that did not decide (an unavailable judge's fallback, a check that
    /// could not tell), or once the scripts differ from the pin.
    fn minted_digest(&self, s: &Settlement<'_>) -> Option<String> {
        s.ran?;
        // Refused at validate and start (V288); an isolated execution never stamps.
        if self.node.kind.is_isolated()
            || s.state != crate::list::ledger::settled::PASSED
            || !self.skip_allowed.load(std::sync::atomic::Ordering::SeqCst)
            || (self.spec.check.is_some() && !s.check_decided)
            || self.stamp_block().is_some()
        {
            return None;
        }
        let item = self.item(s.id)?;
        let work = self.work().ok()?;
        // The scripts right at this settlement, after the item's work and
        // its check, against the pin; an earlier difference at any item's
        // start or settlement has already latched the visit.
        if !self.scripts_held() || !self.pins_held() || !self.runtimes_held() {
            return None;
        }
        Some(crate::list::skip::skip_digest(
            s.id,
            &self.item_part(item),
            &work,
        ))
    }

    pub(super) fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// What `id` did before this run of it: its record in this visit once
    /// it ran, else its record of the visit before when that settled the
    /// same content (an item that changed is another item to its step). A
    /// skipped row never ran in this run, so it is never what the item did
    /// before: its inherited output is not the item's own earlier attempt.
    pub(super) fn previous<'f>(&'f self, fold: &'f ListFold, id: &str) -> Option<&'f ItemRecord> {
        let digest = self.item(id).map(|i| i.digest.as_str());
        fold.item(id)
            .filter(|r| r.skipped_from.is_none() && ran_before(r))
            .or_else(|| {
                self.earlier
                    .as_ref()
                    .and_then(|f| f.item(id))
                    .filter(|r| r.skipped_from.is_none())
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
        apb_core::fsutil::atomic_write_private_under(run_dir, &path, text.as_bytes())?;
        let output = match output {
            Some(o) => {
                let p = dir.join(format!("{stem}.output.json"));
                apb_core::fsutil::atomic_write_private_under(run_dir, &p, o.as_bytes())?;
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

    /// Journals one item's settlement. A settlement right after the item's
    /// step ran takes an item snapshot of its full output and fields when
    /// the node declares `results:` (the stored `output` stays clipped).
    pub(super) fn settle(&self, s: Settlement<'_>) -> Result<(), EngineError> {
        use super::super::result;
        let taken = s.ran.and_then(|(status, output)| {
            result::capture(result::Request {
                run_dir: self.ctx.run_dir,
                workdir: self.ctx.workdir,
                node: self.node,
                status,
                output,
                cache_hit: false,
                item: Some(result::ItemSubject {
                    id: s.id,
                    fields: &self.spec.item_outputs,
                }),
                snapshotted: result::snapshotted(self.ctx.playbook, &self.node.id),
            })
        });
        let minted = self.minted_digest(&s);
        result::journal(taken, |stamp| self.settle_stamped(s, stamp, minted))
    }

    fn settle_stamped(
        &self,
        s: Settlement<'_>,
        result: Option<apb_core::result_store::ResultStamp>,
        minted: Option<String>,
    ) -> Result<(), EngineError> {
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
            skipped_from: s.skipped_from,
            skip_digest: s.skip_digest.or(minted),
            skip_latched: self.latch_note(),
            result,
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
    /// The step's status and full output, for a settlement right after the
    /// item's step ran; `None` for a settlement of no new work.
    pub ran: Option<(crate::state::NodeStatus, &'s str)>,
    /// The earlier run a skip settled the item from.
    pub skipped_from: Option<String>,
    /// The skip digest a skipped item carries (the source's entry).
    pub skip_digest: Option<String>,
    /// The item's check ran to a verdict of its own (not a fallback of an
    /// unavailable judge, not a check that could not tell).
    pub check_decided: bool,
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
            .filter(|(k, _)| spec.item_outputs.iter().any(|f| f.name() == k))
            .collect()
    };
    let text = serde_json::to_string(&kept).ok()?;
    if text.len() <= SETTLED_OUTPUT_MAX_BYTES {
        Some(text)
    } else {
        Some(r#"{"truncated":true}"#.to_string())
    }
}
