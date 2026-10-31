//! Remembered run input, per project: the param values and the instruction
//! a person chose to keep for a playbook, used at the next start when the
//! caller passes none.
//!
//! The memory of a playbook lives in the execution root
//! (`.apb/memory/<project|global>/<id>.json`, git-ignored), so a global
//! playbook remembers different values in every project. Values are bound
//! to the playbook's params as they were saved ([`params_schema_digest`]):
//! after the params change, each value is checked again and an incompatible
//! one is reported, never used. Secret params, params named like a
//! credential and credential-shaped values are never stored. A global
//! playbook's memory is bound to this installation (see [`store`]), so a
//! file a checkout brought along does not seed it.
//!
//! Precedence at start ([`apply`]): an explicit value, then a compatible
//! remembered one, then the param's default (filled by the engine). For the
//! instruction: an explicit one, then the remembered one, then the legacy
//! draft (`meta/instruction-draft.md` of the definition), read only.

mod key;
mod schema;
mod store;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use key::{MemoryError, MemoryKey};
pub use schema::{Incompat, check_value, params_schema_digest};

use crate::schema::Param;
use crate::token_shape::{looks_like_credential, name_looks_secret};
use store::MemoryFile;

/// Where the instruction a start would use comes from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionSource {
    Memory,
    LegacyDraft,
    None,
}

/// Whether a remembered value can be used now.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ValueStatus {
    Compatible,
    Incompatible { reason: Incompat },
}

/// One remembered param value.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecalledValue {
    pub name: String,
    /// The value; `None` when the param became a secret (the old value is
    /// never shown).
    pub value: Option<String>,
    pub status: ValueStatus,
}

/// What memory holds for a playbook, checked against its params now.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recall {
    /// The instruction a start without one would use (a blank one counts as
    /// none).
    pub instruction: Option<String>,
    pub instruction_source: InstructionSource,
    pub values: Vec<RecalledValue>,
    /// Params with neither a compatible remembered value nor a default: a
    /// start must pass them.
    pub missing: Vec<String>,
    /// The params changed since the values were saved.
    pub schema_changed: bool,
    pub diagnostics: Vec<String>,
}

impl Recall {
    /// The remembered values a start may use, by name.
    pub fn compatible(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .filter_map(|v| match (&v.status, &v.value) {
                (ValueStatus::Compatible, Some(value)) => Some((v.name.as_str(), value.as_str())),
                _ => None,
            })
    }
}

/// What to remember. `None` fields keep what is stored; given params are
/// merged into the stored ones.
#[derive(Debug, Clone, Default)]
pub struct RememberInput {
    pub params: Option<BTreeMap<String, String>>,
    /// `Some("")` clears the instruction on purpose (no legacy fallback).
    pub instruction: Option<String>,
}

/// A value [`remember`] refused, and why (`unknown_param`, `secret_param`,
/// `param_looks_secret` (a name such as `password` or `api_key`),
/// `value_looks_secret`, `instruction_looks_secret`, or an [`Incompat`]
/// reason).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub name: String,
    pub reason: String,
}

/// What [`remember`] stored (param names, and `instruction`) and refused,
/// and what it found wrong with the memory it replaced.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub stored: Vec<String>,
    pub refused: Vec<Refused>,
    /// The earlier memory could not be used (corrupt, or not saved on this
    /// machine) and was replaced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub diagnostics: Vec<String>,
}

/// What [`forget`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    All,
    Params,
    Instruction,
}

/// The name [`Remembered::stored`] and [`Applied::remembered`] use for the
/// instruction.
pub const INSTRUCTION: &str = "instruction";

/// What memory holds for `key` under the execution root `root`, checked
/// against `params` (the playbook's params now). `legacy_draft` is the
/// definition's old instruction draft, used when memory holds no
/// instruction. Read only: a corrupt file reads as empty with a
/// diagnostic.
pub fn recall(
    root: &Path,
    key: &MemoryKey,
    params: &[Param],
    legacy_draft: Option<String>,
) -> Result<Recall, MemoryError> {
    let mut diagnostics = Vec::new();
    let file = match store::read(root, key, &mut diagnostics) {
        Ok(f) => f,
        Err(MemoryError::NewerFormat(n)) => {
            diagnostics.push(MemoryError::NewerFormat(n).to_string());
            None
        }
        Err(e) => return Err(e),
    };
    let file = file.unwrap_or_default();
    let values: Vec<RecalledValue> = file
        .params
        .iter()
        .map(|(name, value)| {
            let status = match params.iter().find(|p| &p.name == name) {
                None => Err(Incompat::Removed),
                Some(p) => check_value(p, value),
            };
            match status {
                Ok(()) => RecalledValue {
                    name: name.clone(),
                    value: Some(value.clone()),
                    status: ValueStatus::Compatible,
                },
                Err(reason) => RecalledValue {
                    name: name.clone(),
                    value: (reason != Incompat::NowSecret).then(|| value.clone()),
                    status: ValueStatus::Incompatible { reason },
                },
            }
        })
        .collect();
    let missing = params
        .iter()
        .filter(|p| p.default.is_none())
        .filter(|p| {
            !values
                .iter()
                .any(|v| v.name == p.name && v.status == ValueStatus::Compatible)
        })
        .map(|p| p.name.clone())
        .collect();
    let (instruction, instruction_source) = match file.instruction {
        Some(text) => (Some(text), InstructionSource::Memory),
        None => match legacy_draft {
            Some(text) => (Some(text), InstructionSource::LegacyDraft),
            None => (None, InstructionSource::None),
        },
    };
    Ok(Recall {
        instruction: instruction.filter(|s| !s.trim().is_empty()),
        instruction_source,
        values,
        missing,
        schema_changed: !file.params_schema.is_empty()
            && file.params_schema != params_schema_digest(params),
        diagnostics,
    })
}

/// Stores `input` for `key`: each param value that is a known, non-secret
/// param, fits it now and does not look like a credential, and the
/// instruction unless it looks like a credential. Everything else is
/// refused with a reason. `version` is the playbook version the values
/// were chosen for. Stored values that no longer pass the same check (a
/// param that is gone, became secret or changed type) are dropped, so the
/// file only ever holds values that fit the params it is bound to.
pub fn remember(
    root: &Path,
    key: &MemoryKey,
    params: &[Param],
    version: &str,
    input: RememberInput,
) -> Result<Remembered, MemoryError> {
    let mut out = Remembered::default();
    let mut accepted = BTreeMap::new();
    for (name, value) in input.params.unwrap_or_default() {
        match refusal(params, &name, &value) {
            Some(reason) => out.refused.push(Refused { name, reason }),
            None => {
                accepted.insert(name, value);
            }
        }
    }
    let instruction = match input.instruction {
        Some(text) if looks_like_credential(&text) => {
            out.refused.push(Refused {
                name: INSTRUCTION.to_string(),
                reason: "instruction_looks_secret".to_string(),
            });
            None
        }
        other => other,
    };
    if accepted.is_empty() && instruction.is_none() {
        return Ok(out);
    }

    let lock = store::lock(root, key)?;
    let mut file = store::read_locked(root, key, &mut out.diagnostics, &lock)?.unwrap_or_default();
    out.stored.extend(accepted.keys().cloned());
    file.params
        .retain(|name, value| refusal(params, name, value).is_none());
    file.params.extend(accepted);
    if let Some(text) = instruction {
        file.instruction = Some(text);
        out.stored.push(INSTRUCTION.to_string());
    }
    file.params_schema = params_schema_digest(params);
    file.saved_version = version.to_string();
    file.saved_at_ms = crate::clock::now_ms_u64();
    write_as(root, key, file)?;
    Ok(out)
}

/// Writes `file` as `key`'s memory in this build's format.
fn write_as(root: &Path, key: &MemoryKey, mut file: MemoryFile) -> Result<(), MemoryError> {
    file.format = store::FORMAT;
    file.scope = key.scope().as_str().to_string();
    file.id = key.id().to_string();
    store::write(root, key, &file)
}

/// Why the value `value` of param `name` is not remembered, if it is not.
fn refusal(params: &[Param], name: &str, value: &str) -> Option<String> {
    let Some(p) = params.iter().find(|p| p.name == name) else {
        return Some("unknown_param".to_string());
    };
    if p.secret {
        return Some("secret_param".to_string());
    }
    if name_looks_secret(name) {
        return Some("param_looks_secret".to_string());
    }
    if let Err(reason) = check_value(p, value) {
        return Some(reason.as_str().to_string());
    }
    looks_like_credential(value).then(|| "value_looks_secret".to_string())
}

/// Removes what `what` names from `key`'s memory (the file goes once it
/// holds nothing). A file this installation would ignore (another key's,
/// or one it did not save) goes as well, and a corrupt one is moved aside;
/// the returned diagnostics say so.
pub fn forget(root: &Path, key: &MemoryKey, what: What) -> Result<Vec<String>, MemoryError> {
    let lock = store::lock(root, key)?;
    let mut diagnostics = Vec::new();
    let Some(mut file) = store::read_locked(root, key, &mut diagnostics, &lock)? else {
        store::write(root, key, &MemoryFile::default())?;
        return Ok(diagnostics);
    };
    if matches!(what, What::All | What::Params) {
        file.params.clear();
    }
    if matches!(what, What::All | What::Instruction) {
        file.instruction = None;
    }
    store::write(root, key, &file)?;
    Ok(diagnostics)
}

/// What [`rekey`] and [`copy_key`] did beyond the copy itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transferred {
    pub diagnostics: Vec<String>,
    /// The legacy draft became the remembered instruction of `to`: undoing
    /// the move must not leave it behind as `from`'s.
    pub seeded_instruction: bool,
}

/// Moves `from`'s memory to `to` (a playbook that changed scope), checked
/// against `params` (the playbook's params now) the way [`remember`] checks
/// a save: a value that would be refused is left behind, so a file a
/// checkout brought cannot carry a credential into memory bound to this
/// installation. When the memory holds no instruction, `legacy_draft` (the
/// old draft of the definition at `from`) becomes the remembered
/// instruction, so the input survives the move; a draft that looks like a
/// credential is not carried (the diagnostics say so). Refuses
/// [`MemoryError::DestinationExists`] when `to` already has memory; nothing
/// changes then. When `from` cannot be cleared after `to` was written, `to`
/// is removed again, so a retry finds the state it started from; the error
/// says when that removal failed too.
pub fn rekey(
    root: &Path,
    from: &MemoryKey,
    to: &MemoryKey,
    params: &[Param],
    legacy_draft: Option<String>,
) -> Result<Transferred, MemoryError> {
    transfer(root, from, to, params, legacy_draft, true)
}

/// [`rekey`] that keeps `from`'s memory: a playbook copied to another scope
/// while the original stays in use.
pub fn copy_key(
    root: &Path,
    from: &MemoryKey,
    to: &MemoryKey,
    params: &[Param],
    legacy_draft: Option<String>,
) -> Result<Transferred, MemoryError> {
    transfer(root, from, to, params, legacy_draft, false)
}

fn transfer(
    root: &Path,
    from: &MemoryKey,
    to: &MemoryKey,
    params: &[Param],
    legacy_draft: Option<String>,
    clear_from: bool,
) -> Result<Transferred, MemoryError> {
    let mut out = Transferred::default();
    if from == to {
        return Ok(out);
    }
    // Both locks, in a fixed order, so two opposite moves cannot deadlock.
    let order = |k: &MemoryKey| (k.scope().as_str(), k.id().to_string());
    let (first, second) = if order(from) <= order(to) {
        (from, to)
    } else {
        (to, from)
    };
    let lock_a = store::lock(root, first)?;
    let lock_b = store::lock(root, second)?;
    let (from_lock, to_lock) = if first == from {
        (&lock_a, &lock_b)
    } else {
        (&lock_b, &lock_a)
    };
    let diagnostics = &mut out.diagnostics;
    if store::read_locked(root, to, diagnostics, to_lock)?.is_some() {
        return Err(MemoryError::DestinationExists);
    }
    let mut file = store::read_locked(root, from, diagnostics, from_lock)?.unwrap_or_default();
    let before = file.params.len();
    file.params
        .retain(|name, value| refusal(params, name, value).is_none());
    if file.params.len() < before {
        diagnostics.push(format!(
            "{} remembered value(s) no longer fit or look like a credential; they were not carried",
            before - file.params.len()
        ));
    }
    if file.instruction.is_none() {
        file.instruction = legacy_draft.filter(|s| !s.trim().is_empty()).filter(|s| {
            let secret = looks_like_credential(s);
            if secret {
                diagnostics.push(
                    "the old instruction draft looks like a credential; it was not remembered"
                        .to_string(),
                );
            }
            !secret
        });
        out.seeded_instruction = file.instruction.is_some();
    }
    if file.is_empty() {
        return Ok(out);
    }
    file.params_schema = params_schema_digest(params);
    write_as(root, to, file)?;
    if clear_from && let Err(e) = store::write(root, from, &MemoryFile::default()) {
        return Err(match store::write(root, to, &MemoryFile::default()) {
            Ok(()) => e,
            Err(undo) => MemoryError::Io(std::io::Error::other(format!(
                "{e}; the copy at the destination could not be removed either: {undo}"
            ))),
        });
    }
    Ok(out)
}

/// The run input a start uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub params: BTreeMap<String, String>,
    pub instruction: Option<String>,
    /// The param names (and [`INSTRUCTION`]) taken from memory.
    pub remembered: Vec<String>,
}

/// The precedence rule, pure: explicit values first, then the compatible
/// remembered ones; the instruction explicit, then recalled. Defaults are
/// the engine's (after this).
pub fn apply(
    explicit_params: &BTreeMap<String, String>,
    explicit_instruction: Option<String>,
    recall: &Recall,
) -> Applied {
    let mut params = explicit_params.clone();
    let mut remembered = Vec::new();
    for (name, value) in recall.compatible() {
        if !params.contains_key(name) {
            params.insert(name.to_string(), value.to_string());
            remembered.push(name.to_string());
        }
    }
    let instruction = match explicit_instruction {
        Some(text) => Some(text),
        None => {
            if recall.instruction.is_some()
                && recall.instruction_source == InstructionSource::Memory
            {
                remembered.push(INSTRUCTION.to_string());
            }
            recall.instruction.clone()
        }
    };
    Applied {
        params,
        instruction,
        remembered,
    }
}
