//! Channel for human_review decisions (`reviews.jsonl`). Mirrors control.rs:
//! decision makers (`apb review`, MCP review_decide, HTTP) append their
//! decision here, while drive only reads it and, based on it, writes a
//! ReviewDecided event. This does not violate the single-writer rule for
//! events: events.jsonl is still written only by drive.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommand {
    pub node: String,
    pub decision: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEntry {
    pub seq: u64,
    #[serde(flatten)]
    pub cmd: ReviewCommand,
}

pub fn post_review(run_dir: &Path, cmd: ReviewCommand) -> Result<u64, EngineError> {
    std::fs::create_dir_all(run_dir)?;

    let seq = read_reviews_after(run_dir, None)?.len() as u64;

    let entry = ReviewEntry { seq, cmd };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("reviews.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.flush()?;

    Ok(seq)
}

pub fn read_reviews_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<ReviewEntry>, EngineError> {
    let path = run_dir.join("reviews.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ReviewEntry =
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
    fn post_and_read_reviews_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s0 = post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        let s1 = post_review(
            dir.path(),
            ReviewCommand {
                node: "gate2".into(),
                decision: "rejected".into(),
                note: "b".into(),
            },
        )
        .unwrap();
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);

        let all = read_reviews_after(dir.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].cmd.node, "gate");

        let after = read_reviews_after(dir.path(), Some(s0)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].cmd.decision, "rejected");
    }
}
