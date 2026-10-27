//! Generation and storage of hook_secret for wait-node webhooks (spec 6.7).
//! For each webhook key, an unpredictable secret (uuid v4) is issued at run
//! start. Secrets are stored in `hooks.json` in the run directory (a
//! key -> secret map). The signal endpoint is bound to the run and the
//! secret, so parallel runs cannot intercept each other's signals.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::fsutil::atomic_write;
use apb_core::schema::{NodeKind, Playbook, WaitFor};
use apb_core::secret::{Secret, ct_eq_str};
use serde::{Deserialize, Serialize};

use crate::error::EngineError;

pub(crate) const HOOKS_FILE: &str = "hooks.json";

/// The on-disk shape of `hooks.json`: key -> secret. Private, so the plain
/// values exist only between the file and [`RunHooks`].
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
struct HooksFile(BTreeMap<String, String>);

/// A run's webhook secrets, key -> secret, sealed.
#[derive(Debug, Default, Clone)]
pub struct RunHooks(BTreeMap<String, Secret>);

impl RunHooks {
    /// Key -> relative path of the signal endpoint, `/api/hooks/<run>/<secret>`.
    /// The intended exposure: the path is the capability a wait node hands
    /// to whoever must signal it.
    pub fn paths(&self, run_id: &str) -> BTreeMap<String, String> {
        self.0
            .iter()
            .map(|(k, secret)| {
                let path = format!("/api/hooks/{run_id}/{}", secret.expose_secret());
                (k.clone(), path)
            })
            .collect()
    }

    /// The key whose secret `presented` is, or `None`. Every candidate is
    /// compared in constant time; the first match in key order wins, so a run
    /// whose hooks somehow share a secret always signals the same key.
    pub fn key_for(&self, presented: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, secret)| ct_eq_str(secret.expose_secret(), presented))
            .map(|(k, _)| k.as_str())
    }

    /// Every secret, for a redactor.
    pub fn secrets(&self) -> Vec<Secret> {
        self.0.values().cloned().collect()
    }
}

/// Generates secrets for all of the playbook's webhook keys and saves
/// `hooks.json`. Idempotent: if the file already exists, nothing is
/// overwritten (a run's secrets stay stable for its whole lifetime).
pub fn generate_hooks(run_dir: &Path, playbook: &Playbook) -> Result<(), EngineError> {
    let path = run_dir.join(HOOKS_FILE);
    if path.is_file() {
        return Ok(());
    }
    let mut hooks = HooksFile::default();
    for node in &playbook.nodes {
        if let NodeKind::Wait {
            wait_for: WaitFor::Webhook { key },
            ..
        } = &node.kind
        {
            hooks
                .0
                .entry(key.clone())
                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
        }
    }
    let json = serde_json::to_vec_pretty(&hooks).map_err(|e| EngineError::Yaml(e.to_string()))?;
    atomic_write(&path, &json)?;
    Ok(())
}

/// Reads the run's hooks (empty if the file does not exist).
pub fn read_hooks(run_dir: &Path) -> Result<RunHooks, EngineError> {
    let path = run_dir.join(HOOKS_FILE);
    if !path.is_file() {
        return Ok(RunHooks::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    let file: HooksFile =
        serde_json::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))?;
    Ok(RunHooks(
        file.0
            .into_iter()
            .map(|(k, v)| (k, Secret::new(v)))
            .collect(),
    ))
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
        let paths = read_hooks(dir.path()).unwrap().paths("run-1");
        assert_eq!(
            paths.keys().collect::<Vec<_>>(),
            ["ci-done"],
            "only the webhook key gets a hook"
        );
        let secret = paths["ci-done"].strip_prefix("/api/hooks/run-1/");
        assert!(
            secret.is_some_and(|s| !s.is_empty()),
            "path carries a secret"
        );
    }

    #[test]
    fn generate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        generate_hooks(dir.path(), &playbook).unwrap();
        let first = read_hooks(dir.path()).unwrap().paths("r");
        generate_hooks(dir.path(), &playbook).unwrap();
        let second = read_hooks(dir.path()).unwrap().paths("r");
        assert!(first == second, "secret must be stable across calls");
    }

    fn hooks(pairs: &[(&str, &str)]) -> RunHooks {
        RunHooks(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Secret::new(*v)))
                .collect(),
        )
    }

    #[test]
    fn key_for_finds_the_first_key_whose_secret_matches() {
        let h = hooks(&[("b-key", "shared"), ("a-key", "shared"), ("c-key", "other")]);
        assert_eq!(h.key_for("shared"), Some("a-key"), "first in key order");
        assert_eq!(h.key_for("other"), Some("c-key"));
        assert_eq!(h.key_for("share"), None, "a prefix is not a match");
        assert_eq!(h.key_for(""), None);
        assert_eq!(RunHooks::default().key_for("shared"), None);
    }

    #[test]
    fn debug_never_prints_a_secret() {
        let h = hooks(&[("k", "visible-if-leaked")]);
        assert!(!format!("{h:?}").contains("visible-if-leaked"));
        assert_eq!(h.secrets().len(), 1);
    }
}
