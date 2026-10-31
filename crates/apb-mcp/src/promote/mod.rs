//! "Make it available everywhere": copies a project playbook, with every
//! version, to the global store, so every project on this machine can run
//! it. Digests depend on content only, so the copy keeps the trust of the
//! source. The person chooses what happens to the project copy; the
//! remembered input of this project moves with the playbook (or is copied,
//! when the project copy is kept).
//!
//! Order of the work, under the project playbook's lock: plan again, copy
//! the playbook, check the copy digests like the source (else remove it and
//! refuse), copy the profiles asked for, carry the memory, apply the fate of
//! the project copy, and answer with the global playbook's fit report. A
//! step that fails undoes the earlier ones, so a retry starts from the same
//! state; the fit report is best effort.

mod plan;

use std::path::{Path, PathBuf};

use apb_core::profile::ProfileScope;
use apb_core::registry::{Registry, is_safe_segment};
use apb_core::run_memory::{MemoryError, MemoryKey, What, copy_key, forget, rekey};
use apb_core::scope::{Origin, PlaybookRef, Scope};
use apb_core::trust::{Lifecycle, write_lifecycle};
use serde_json::{Value, json};

pub use plan::{GlobalPlan, Item, plan};

use crate::tools::ToolError;

/// What happens to the project copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// Moved to the project trash (restorable).
    Trash,
    /// Kept but retired: the catalog shadows it by the global one.
    Retire,
    /// Kept as is: the catalog marks the pair ambiguous while both are
    /// active and trusted, and shadows the project copy otherwise.
    Keep,
}

impl Variant {
    pub fn parse(s: &str) -> Option<Variant> {
        match s {
            "trash" => Some(Variant::Trash),
            "retire" => Some(Variant::Retire),
            "keep" => Some(Variant::Keep),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Variant::Trash => "trash",
            Variant::Retire => "retire",
            Variant::Keep => "keep",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Variant::Trash => "move the project copy to the project trash (restorable)",
            Variant::Retire => "retire the project copy, so the global playbook wins here",
            Variant::Keep => {
                "keep the project copy with its remembered input; while both are active and trusted the catalog marks the pair ambiguous, otherwise the global playbook shadows it"
            }
        }
    }
}

/// How to make a playbook global.
#[derive(Debug, Clone, Copy, Default)]
pub struct MakeGlobalOpts {
    /// Required unless `dry_run`.
    pub variant: Option<Variant>,
    /// Copy the project profiles the playbook needs to the global scope.
    pub move_profiles: bool,
    /// Only report the plan.
    pub dry_run: bool,
}

fn engine<E: std::fmt::Display>(e: E) -> ToolError {
    ToolError::Engine(e.to_string())
}

/// Makes project playbook `id` of the project at `root` global (see the
/// module docs). Blockers, and a dry run, change nothing and return the
/// plan (`applied: false`).
pub fn make_global(root: &Path, id: &str, opts: MakeGlobalOpts) -> Result<Value, ToolError> {
    // An unsafe id is only planned (`not_found`): it never reaches a path,
    // not even the lock's.
    if opts.dry_run || !is_safe_segment(id) {
        let plan = plan(root, id, &opts);
        return Ok(json!({ "id": id, "dry_run": opts.dry_run, "applied": false, "plan": plan }));
    }
    let project_playbooks = apb_core::versioning::playbooks_dir(root);
    let _lock = if project_playbooks.is_dir() {
        Some(
            apb_core::fsutil::lock_dir(&project_playbooks, &format!("{id}.lock"))
                .map_err(engine)?,
        )
    } else {
        None
    };
    let plan = plan(root, id, &opts);
    if !plan.blockers.is_empty() {
        return Ok(json!({ "id": id, "dry_run": false, "applied": false, "plan": plan }));
    }
    let (Some(variant), Some(global_parent)) =
        (opts.variant, apb_core::store::global_playbooks_parent())
    else {
        // The plan blocks both cases; kept total for the compiler.
        return Err(ToolError::Engine("make-global: plan incomplete".into()));
    };
    let mut warnings: Vec<String> = Vec::new();
    let mut undo = Undo::default();

    let project_dir = project_playbooks.join(id);
    let global_dir =
        apb_core::scope_move::copy_playbook_to(&project_dir, &global_parent, id).map_err(engine)?;
    undo.global_dir = Some(global_dir);
    let copy = Registry::open_dir(&global_parent).and_then(|reg| reg.load(id, None));
    let refusal = match copy
        .as_ref()
        .map_err(|e| e.to_string())
        .and_then(|l| l.trust_digest().map_err(|e| e.to_string()))
    {
        Ok(d) if Some(&d) == plan.digest.as_ref() => None,
        Ok(_) => Some((
            "digest_changed",
            "the global copy does not digest like the project playbook".to_string(),
        )),
        Err(e) => Some((
            "copy_unreadable",
            format!("the global copy could not be loaded: {e}"),
        )),
    };
    if let Some((code, detail)) = refusal {
        let leftovers = undo.run(root);
        return Ok(json!({
            "id": id,
            "applied": false,
            "error": code,
            "detail": format!("{detail}; {}", nothing_changed(&leftovers)),
        }));
    }
    let params = copy.map(|l| l.playbook.params).unwrap_or_default();

    for name in &plan.profiles_to_copy {
        // Recorded before the copy: a copy that fails part way removes
        // what it wrote, and the undo covers a profile it finished.
        let dst = apb_core::profile_store::global_dir().map(|d| d.join(name));
        if let Some(dst) = dst.filter(|d| !d.exists()) {
            undo.profiles.push(dst);
        }
        if let Err(e) = crate::profile_tools::copy_profile_locked(
            root,
            name,
            ProfileScope::Project,
            ProfileScope::Global,
        ) {
            return Err(undone(engine(e), undo.run(root)));
        }
    }

    let legacy = match Registry::open(root).and_then(|reg| reg.read_instruction_draft(id)) {
        Ok(draft) => draft,
        Err(e) => {
            warnings.push(format!("the old instruction draft could not be read: {e}"));
            None
        }
    };
    let from = MemoryKey::new(Scope::Project, id).map_err(engine)?;
    let to = MemoryKey::new(Scope::Global, id).map_err(engine)?;
    // A kept project copy keeps running here, so it keeps its memory.
    let copied = variant == Variant::Keep;
    let carried = if copied {
        copy_key(root, &from, &to, &params, legacy)
    } else {
        rekey(root, &from, &to, &params, legacy)
    };
    match carried {
        Ok(done) => {
            warnings.extend(done.diagnostics);
            undo.memory = Some(MemoryUndo {
                from,
                to,
                copied,
                seeded: done.seeded_instruction,
                params,
            });
        }
        Err(MemoryError::DestinationExists) => warnings.push(
            "this project already remembers input for the global playbook; the project playbook's memory was left in place".into(),
        ),
        Err(e) => warnings.push(format!("the remembered input was not carried: {e}")),
    }

    let fate = match variant {
        Variant::Trash => {
            apb_core::versioning::delete_playbook(root, id, apb_core::clock::now_ms())
                .map(|_| ())
                .map_err(ToolError::from)
        }
        Variant::Retire => write_lifecycle(&project_dir, Lifecycle::Retired).map_err(engine),
        Variant::Keep => {
            warnings.push(KEEP_NOTE.into());
            Ok(())
        }
    };
    if let Err(e) = fate {
        return Err(undone(e, undo.run(root)));
    }

    let wref = PlaybookRef {
        origin: Origin::Global,
        id: id.to_string(),
        version: None,
    };
    let fit = match apb_engine::fit::assess(root, &wref)
        .map_err(|e| e.to_string())
        .and_then(|f| serde_json::to_value(f).map_err(|e| e.to_string()))
    {
        Ok(f) => f,
        Err(e) => {
            warnings.push(format!("the fit report could not be built: {e}"));
            Value::Null
        }
    };
    Ok(json!({
        "id": id,
        "dry_run": false,
        "applied": true,
        "variant": variant.as_str(),
        "plan": plan,
        "warnings": warnings,
        "fit": fit,
    }))
}

/// What the catalog shows for a kept project copy beside the global one.
const KEEP_NOTE: &str = "the project copy stays: while both are active and trusted the catalog marks the pair ambiguous until one is retired; otherwise the global playbook shadows it";

/// What a make-global call changed so far, undone in reverse order when a
/// later step fails.
#[derive(Default)]
struct Undo {
    global_dir: Option<PathBuf>,
    profiles: Vec<PathBuf>,
    memory: Option<MemoryUndo>,
}

/// The memory carried `from` -> `to`.
struct MemoryUndo {
    from: MemoryKey,
    to: MemoryKey,
    /// Copied (the source kept) rather than moved.
    copied: bool,
    /// The legacy draft became `to`'s instruction: it was never `from`'s.
    seeded: bool,
    params: Vec<apb_core::schema::Param>,
}

impl Undo {
    /// Undoes every step; returns what could not be undone.
    fn run(self, root: &Path) -> Vec<String> {
        let mut failed = Vec::new();
        if let Some(m) = self.memory {
            let back = if m.copied {
                forget(root, &m.to, What::All).map(|_| ())
            } else {
                rekey(root, &m.to, &m.from, &m.params, None).and_then(|_| {
                    if m.seeded {
                        forget(root, &m.from, What::Instruction).map(|_| ())
                    } else {
                        Ok(())
                    }
                })
            };
            if let Err(e) = back {
                failed.push(format!("the remembered input: {e}"));
            }
        }
        for dir in self.profiles.iter().rev().chain(self.global_dir.as_ref()) {
            if let Err(e) = std::fs::remove_dir_all(dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                failed.push(format!("{}: {e}", dir.display()));
            }
        }
        failed
    }
}

/// "nothing was changed", or what the undo left behind.
fn nothing_changed(leftovers: &[String]) -> String {
    if leftovers.is_empty() {
        "nothing was changed".to_string()
    } else {
        format!("the undo left behind: {}", leftovers.join("; "))
    }
}

/// `e`, with what the undo left behind when it could not undo everything.
fn undone(e: ToolError, leftovers: Vec<String>) -> ToolError {
    if leftovers.is_empty() {
        return e;
    }
    ToolError::Engine(format!("{e}; {}", nothing_changed(&leftovers)))
}
