//! The keys of the records a sidecar already holds, so the writer does not
//! write a record twice after a resume. Not visible to execution modules.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use super::{FILE, MAX_BYTES};

/// The `(kind, key)` pairs of every record the sidecar holds.
#[derive(Debug, Default)]
pub(crate) struct Index {
    keys: BTreeSet<(String, String)>,
}

impl Index {
    /// The index of the sidecar at `run_dir` (empty when there is none).
    pub(crate) fn load(run_dir: &Path) -> Self {
        let keys = read_raw(run_dir)
            .into_iter()
            .filter_map(|v| {
                let kind = v.get("kind")?.as_str()?.to_string();
                let key = v.get("key")?.as_str()?.to_string();
                Some((kind, key))
            })
            .collect();
        Index { keys }
    }

    pub(crate) fn contains(&self, kind: &str, key: &str) -> bool {
        self.keys.contains(&(kind.to_string(), key.to_string()))
    }

    /// Adds a record the writer just appended.
    pub(crate) fn insert(&mut self, kind: &str, key: &str) {
        self.keys.insert((kind.to_string(), key.to_string()));
    }
}

/// Every line of the sidecar that parses as a JSON object.
pub(crate) fn read_raw(run_dir: &Path) -> Vec<Value> {
    let Ok(file) = apb_core::fsutil::open_regular_no_follow(&run_dir.join(FILE)) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if file.take(MAX_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}
