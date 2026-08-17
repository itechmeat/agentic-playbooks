//! Channel for webhook signals (`signals.jsonl`) for wait nodes. Mirrors
//! review.rs: the HTTP hook handler appends a signal here by key after
//! verifying the secret, while drive only reads it. This does not violate
//! the single-writer rule for events: wait events are only written by drive.
//!
//! `seq` is derived inside a directory lock, not by counting and then
//! appending: the wait node counts arrived signals against consumed ones
//! (`scheduler.rs`), so two posts sharing a number would make a loop
//! re-entering a wait satisfy itself with a signal it already consumed. The
//! connector inbox (`apb_core::connector::inbox`) uses the same discipline.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// Lock file serializing the read-count-then-append critical section over
/// `signals.jsonl`.
const SIGNALS_LOCK: &str = "signals.lock";

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
    // The lock covers the whole critical section: without it two concurrent
    // posts both read the same count and both write that number.
    let _lock = apb_core::fsutil::lock_dir(run_dir, SIGNALS_LOCK)?;

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

    #[test]
    fn concurrent_posters_never_share_a_sequence_number() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let mut handles = Vec::new();
        for worker in 0..3u32 {
            let run_dir = run_dir.clone();
            handles.push(std::thread::spawn(move || {
                let mut seqs = Vec::new();
                for i in 0..10u32 {
                    seqs.push(
                        post_signal(
                            &run_dir,
                            SignalCommand {
                                key: format!("w{worker}-{i}"),
                            },
                        )
                        .unwrap(),
                    );
                }
                seqs
            }));
        }
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort_unstable();
        let mut unique = all.clone();
        unique.dedup();
        assert_eq!(all.len(), 30);
        assert_eq!(
            unique.len(),
            30,
            "seq must be unique across concurrent posters, got {all:?}"
        );
        assert_eq!(
            all,
            (0..30).collect::<Vec<u64>>(),
            "and dense from 0, so a wait node's arrived-vs-consumed count stays exact"
        );
        assert_eq!(read_signals_after(&run_dir, None).unwrap().len(), 30);
    }
}
