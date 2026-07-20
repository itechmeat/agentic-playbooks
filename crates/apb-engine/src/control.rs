use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Control {
    Retry {
        node: String,
        prompt_override: Option<String>,
    },
    ContinueFrom {
        node: String,
    },
    Pause,
    Abort {
        reason: String,
    },
    ContextAppend {
        note: String,
    },
    Progress {
        done: u64,
        total: u64,
        #[serde(default)]
        label: Option<String>,
    },
    Patch {
        version: String,
        classification: String,
        continue_from: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEntry {
    pub seq: u64,
    #[serde(flatten)]
    pub cmd: Control,
}

pub fn post_control(run_dir: &Path, cmd: Control) -> Result<u64, EngineError> {
    std::fs::create_dir_all(run_dir)?;

    let seq = read_control_after(run_dir, None)?.len() as u64;

    let entry = ControlEntry { seq, cmd };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("control.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.flush()?;

    Ok(seq)
}

/// Reads the persisted control cursor for this run (`runs/<id>/control.cursor`):
/// the seq of the last control.jsonl entry any drive loop of this run has
/// already applied. A missing file means no control entry has ever been
/// applied yet - `None`, the exact state a fresh drive starts in. A corrupt
/// file is a hard error rather than silently degrading to `None`: silently
/// replaying from the beginning is exactly the duplicate-application bug this
/// cursor exists to prevent.
pub fn read_control_cursor(run_dir: &Path) -> Result<Option<u64>, EngineError> {
    let path = run_dir.join("control.cursor");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let seq = trimmed
        .parse::<u64>()
        .map_err(|e| EngineError::Yaml(format!("invalid control cursor `{trimmed}`: {e}")))?;
    Ok(Some(seq))
}

/// Persists the control cursor right after a control.jsonl entry is applied,
/// atomically (temp + rename, per `apb_core::fsutil::atomic_write`) so a crash
/// mid-write never leaves a torn cursor file for a later drive to misread.
/// Every site that advances the in-memory cursor (the drive-loop top-of-loop
/// scan, `await_control`, `drain_progress_after_execute`) calls this so a
/// resumed drive - or a fresh wake within the same drive - never re-applies an
/// entry a prior pass already consumed.
pub fn write_control_cursor(run_dir: &Path, seq: u64) -> Result<(), EngineError> {
    Ok(apb_core::fsutil::atomic_write(
        &run_dir.join("control.cursor"),
        seq.to_string().as_bytes(),
    )?)
}

pub fn read_control_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<ControlEntry>, EngineError> {
    let path = run_dir.join("control.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ControlEntry =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;

        if let Some(threshold) = after_seq {
            if entry.seq > threshold {
                out.push(entry);
            }
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}
