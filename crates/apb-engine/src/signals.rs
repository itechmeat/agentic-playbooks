//! Channel for webhook signals (`signals.jsonl`) for wait nodes. Mirrors
//! review.rs: the HTTP hook handler appends a signal here by key after
//! verifying the secret, while drive only reads it. This does not violate
//! the single-writer rule for events: wait events are only written by drive.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalCommand {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEntry {
    pub seq: u64,
    #[serde(flatten)]
    pub cmd: SignalCommand,
}

pub fn post_signal(run_dir: &Path, cmd: SignalCommand) -> Result<u64, EngineError> {
    std::fs::create_dir_all(run_dir)?;

    let seq = read_signals_after(run_dir, None)?.len() as u64;

    let entry = SignalEntry { seq, cmd };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("signals.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.flush()?;

    Ok(seq)
}

pub fn read_signals_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<SignalEntry>, EngineError> {
    let path = run_dir.join("signals.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SignalEntry =
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_read_signals_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s0 = post_signal(dir.path(), SignalCommand { key: "ci".into() }).unwrap();
        let s1 = post_signal(
            dir.path(),
            SignalCommand {
                key: "deploy".into(),
            },
        )
        .unwrap();
        assert_eq!((s0, s1), (0, 1));
        assert_eq!(read_signals_after(dir.path(), None).unwrap().len(), 2);
        let after = read_signals_after(dir.path(), Some(s0)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].cmd.key, "deploy");
    }
}
