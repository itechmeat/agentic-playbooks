//! Remembered run input on the tool surfaces: `playbook_memory` (read,
//! remember, forget) and the `remember` option of a run start, shared by the
//! MCP server and the dashboard API. The store itself is
//! [`apb_core::run_memory`]; this layer resolves the playbook whose params
//! the values are checked against.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::registry::Registry;
use apb_core::run_memory::{
    self, MemoryKey, RememberInput, Remembered, What, forget, recall, remember,
};
use apb_core::store::ResolvedPlaybook;
use serde_json::{Value, json};

use super::ToolError;

/// What to do with a playbook's memory.
#[derive(Debug, Clone)]
pub enum MemoryAction {
    /// The memory checked against the playbook's params now.
    Get,
    /// Remember these values (merged into what is stored).
    Set(RememberInput),
    /// Forget part or all of it.
    Clear(What),
}

/// Parses the `what` of a clear: `all` (also when absent), `params` or
/// `instruction`.
pub fn parse_what(what: Option<&str>) -> Option<What> {
    match what.unwrap_or("all") {
        "all" => Some(What::All),
        "params" => Some(What::Params),
        "instruction" => Some(What::Instruction),
        _ => None,
    }
}

/// Which playbook's memory: `id` in definition scope `scope` (`project` or
/// `global`), checked against `version` (the current one when `None`).
#[derive(Debug, Clone, Copy)]
pub struct MemoryTarget<'a> {
    pub id: &'a str,
    pub scope: &'a str,
    pub version: Option<&'a str>,
}

fn memory_error(e: run_memory::MemoryError) -> ToolError {
    match e {
        run_memory::MemoryError::InvalidId(id) => ToolError::NotFound(format!("playbook `{id}`")),
        other => ToolError::Engine(other.to_string()),
    }
}

/// Runs `action` on the memory of `target` in the project at `root`.
///
/// - get: `{id, scope, version, instruction, instruction_source, values,
///   missing, schema_changed, diagnostics}`, checked against `version` (the
///   current one when `None`).
/// - set: `{id, scope, stored, refused, diagnostics?}`, always checked
///   against the current version: the one a later start and read use.
/// - clear: `{id, scope, cleared, diagnostics?}`. It needs no definition,
///   so memory of a playbook that was deleted or no longer loads can still
///   be cleared.
pub fn playbook_memory(
    root: &Path,
    target: &MemoryTarget<'_>,
    action: MemoryAction,
) -> Result<Value, ToolError> {
    let (scope, parent) = super::scope_parent(root, target.scope)?;
    let key = MemoryKey::new(scope, target.id).map_err(memory_error)?;
    let head = json!({ "id": target.id, "scope": scope.as_str() });
    if let MemoryAction::Clear(what) = action {
        let diagnostics = forget(root, &key, what).map_err(memory_error)?;
        let mut body = json!({ "cleared": what_str(what) });
        if !diagnostics.is_empty() {
            body["diagnostics"] = json!(diagnostics);
        }
        return Ok(merge(head, body));
    }
    let reg = Registry::open_dir(&parent)?;
    let body = if let MemoryAction::Set(input) = action {
        let loaded = reg.load(target.id, None)?;
        json!(
            remember(root, &key, &loaded.playbook.params, &loaded.version, input)
                .map_err(memory_error)?
        )
    } else {
        let loaded = reg.load(target.id, target.version)?;
        let legacy = reg.read_instruction_draft(target.id)?;
        let recalled = recall(root, &key, &loaded.playbook.params, legacy).map_err(memory_error)?;
        let mut v = json!(recalled);
        v["version"] = json!(loaded.version);
        v
    };
    Ok(merge(head, body))
}

fn what_str(what: What) -> &'static str {
    match what {
        What::All => "all",
        What::Params => "params",
        What::Instruction => "instruction",
    }
}

fn merge(mut head: Value, body: Value) -> Value {
    if let (Value::Object(h), Value::Object(b)) = (&mut head, body) {
        h.extend(b);
    }
    head
}

/// Remembers the input of a start that succeeded (`remember` on
/// `playbook_run`, the dashboard's Run and `apb run --remember`): the
/// params given and the instruction when one was given, for `resolved` in
/// its execution root, checked against the playbook's current version (a
/// trial of a candidate still remembers for the definition every later
/// start and read use). Returns `{remembered, not_remembered}` (plus
/// `memory_diagnostics` when earlier memory was replaced); a value refused
/// (a secret param, a credential-shaped value) is listed with its reason,
/// never echoed.
pub fn remember_at_start(
    resolved: &ResolvedPlaybook,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
) -> Result<Value, ToolError> {
    let key = MemoryKey::for_label(resolved.origin_label, &resolved.id).map_err(memory_error)?;
    let reg = Registry::open_dir(&resolved.definition_parent)?;
    let loaded = reg.load(&resolved.id, None)?;
    let input = RememberInput {
        params: (!params.is_empty()).then(|| params.clone()),
        instruction: instruction.map(str::to_string),
    };
    let Remembered {
        stored,
        refused,
        diagnostics,
    } = remember(
        &resolved.execution_root,
        &key,
        &loaded.playbook.params,
        &loaded.version,
        input,
    )
    .map_err(memory_error)?;
    let mut out = json!({ "remembered": stored, "not_remembered": refused });
    if !diagnostics.is_empty() {
        out["memory_diagnostics"] = json!(diagnostics);
    }
    Ok(out)
}

/// [`remember_at_start`] for a start that has a ref, not yet a resolved
/// playbook (the dashboard's Run, `apb run --remember`): `wref` resolved
/// for a run in `root`.
pub fn remember_for_ref(
    root: &Path,
    wref: &apb_core::scope::PlaybookRef,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
) -> Result<Value, ToolError> {
    let resolved = apb_core::store::resolve(root, wref)?;
    remember_at_start(&resolved, params, instruction)
}
