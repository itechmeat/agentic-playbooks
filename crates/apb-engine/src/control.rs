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
