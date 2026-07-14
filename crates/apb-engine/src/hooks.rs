//! Generation and storage of hook_secret for wait-node webhooks (spec 6.7).
//! For each webhook key, an unpredictable secret (uuid v4) is issued at run
//! start. Secrets are stored in `hooks.json` in the run directory (a
//! key -> secret map). The signal endpoint is bound to the run and the
//! secret, so parallel runs cannot intercept each other's signals.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::fsutil::atomic_write;
use apb_core::schema::{NodeKind, Playbook, WaitFor};

use crate::error::EngineError;

const HOOKS_FILE: &str = "hooks.json";

/// Generates secrets for all of the playbook's webhook keys and saves
/// `hooks.json`. Idempotent: if the file already exists, nothing is
/// overwritten (a run's secrets stay stable for its whole lifetime).
pub fn generate_hooks(run_dir: &Path, playbook: &Playbook) -> Result<(), EngineError> {
    let path = run_dir.join(HOOKS_FILE);
    if path.is_file() {
        return Ok(());
    }
    let mut hooks: BTreeMap<String, String> = BTreeMap::new();
    for node in &playbook.nodes {
        if let NodeKind::Wait {
            wait_for: WaitFor::Webhook { key },
            ..
        } = &node.kind
        {
            hooks
                .entry(key.clone())
                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
        }
    }
    let json = serde_json::to_vec_pretty(&hooks).map_err(|e| EngineError::Yaml(e.to_string()))?;
    atomic_write(&path, &json)?;
    Ok(())
}

/// Reads the run's key -> secret map (empty if the file does not exist).
pub fn read_hooks(run_dir: &Path) -> Result<BTreeMap<String, String>, EngineError> {
    let path = run_dir.join(HOOKS_FILE);
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))
}

/// Relative path of the signal endpoint (the host is prepended by the monitor/client).
pub fn hook_path(run_id: &str, secret: &str) -> String {
    format!("/api/hooks/{run_id}/{secret}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBOOK: &str = r#"
schema: 1
id: h
name: H
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: wait, wait_for: { type: webhook, key: ci-done }, timeout_seconds: 60 }
  - { id: t, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 60 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: t }
  - { from: t, to: done }
"#;

    #[test]
    fn generates_secret_for_webhook_only() {
        let dir = tempfile::tempdir().unwrap();
        let playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        generate_hooks(dir.path(), &playbook).unwrap();
        let hooks = read_hooks(dir.path()).unwrap();
        assert!(
            hooks.contains_key("ci-done"),
            "webhook key must have a secret"
        );
        assert_eq!(hooks.len(), 1, "timer nodes must not get a hook");
        assert!(!hooks["ci-done"].is_empty());
    }

    #[test]
    fn generate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        generate_hooks(dir.path(), &playbook).unwrap();
        let first = read_hooks(dir.path()).unwrap();
        generate_hooks(dir.path(), &playbook).unwrap();
        let second = read_hooks(dir.path()).unwrap();
        assert_eq!(first, second, "secret must be stable across calls");
    }
}
