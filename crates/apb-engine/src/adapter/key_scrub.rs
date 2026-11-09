//! The store-time scrub of a cloud key (`apb_core::cloud_models`): an
//! attempt on a cloud catalog model gets the provider's key value through
//! its spawn environment, and whatever the agent prints of it is replaced
//! with `[redacted:<VARIABLE>]` before apb stores anything: the reply, the
//! failure text, the stream log and the attempt's transcript files.
//!
//! Redaction on read cannot cover it: replies are journaled verbatim, and a
//! reader that runs later without the variable, or after the key was
//! rotated, no longer knows the old value. The value is read once from what
//! the spawn saw, held only as a [`Secret`] and never written.

use std::path::Path;

use apb_core::secret::Secret;

use super::failure::AgentFailure;
use super::spawn_env::SpawnEnv;

/// The shortest value replaced; a shorter one would match ordinary text.
pub(crate) const MIN_CHARS: usize = apb_core::cloud_models::MIN_KEY_CHARS;

/// The key of one attempt and the marker that replaces it.
pub(crate) struct KeyScrub {
    key: Secret,
    marker: String,
}

impl KeyScrub {
    /// The scrub of a spawn with `env`: `None` when it runs no cloud model,
    /// its key does not reach it, or the value is too short to replace.
    pub(crate) fn for_spawn(env: &SpawnEnv) -> Option<Self> {
        let name = env.cloud.as_ref()?.key_env()?;
        let value = env
            .overlay
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| env.passes(name).then(|| std::env::var(name).ok()).flatten())?;
        if value.chars().count() < MIN_CHARS {
            return None;
        }
        Some(KeyScrub {
            key: Secret::new(value),
            marker: format!("[redacted:{name}]"),
        })
    }

    /// `text` with every occurrence of the key replaced.
    pub(crate) fn text(&self, text: &str) -> String {
        text.replace(self.key.expose_secret(), &self.marker)
    }

    /// Replaces the key in `text`, in place.
    pub(crate) fn in_place(&self, text: &mut String) {
        if text.contains(self.key.expose_secret()) {
            *text = self.text(text);
        }
    }

    /// The failure with the key replaced in its text.
    pub(crate) fn failure(&self, f: &mut AgentFailure) {
        self.in_place(&mut f.message);
    }

    /// Rewrites `path` with the key replaced, when it holds it. A file that
    /// does not read is left alone.
    pub(crate) fn file(&self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let needle = self.key.expose_secret().as_bytes();
        if !bytes.windows(needle.len()).any(|w| w == needle) {
            return;
        }
        let text = String::from_utf8_lossy(&bytes);
        let _ = apb_core::fsutil::atomic_write_private(path, self.text(&text).as_bytes());
    }

    /// Every regular file directly under `dir` (an attempt's transcript).
    pub(crate) fn dir(&self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.filter_map(Result::ok) {
            if e.file_type().is_ok_and(|t| t.is_file()) {
                self.file(&e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(value: &str) -> SpawnEnv {
        let cloud = apb_core::agents::cloud_snapshot(
            "dsh",
            "nearai/Qwen/Qwen3.8-27B",
            &apb_core::cloud_models::catalog::load_all(),
        );
        SpawnEnv {
            overlay: vec![("NEARAI_API_KEY".into(), value.into())],
            cloud,
            ..Default::default()
        }
    }

    #[test]
    fn the_key_is_replaced_in_text_and_files() {
        let s = KeyScrub::for_spawn(&env_with("fake-key-1234567")).unwrap();
        assert_eq!(
            s.text("got fake-key-1234567 here"),
            "got [redacted:NEARAI_API_KEY] here"
        );
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("stdout.log");
        std::fs::write(&f, "a fake-key-1234567 b").unwrap();
        s.dir(dir.path());
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "a [redacted:NEARAI_API_KEY] b"
        );
    }

    #[test]
    fn a_short_key_or_no_cloud_model_is_not_scrubbed() {
        assert!(KeyScrub::for_spawn(&env_with("short")).is_none());
        let plain = SpawnEnv {
            overlay: vec![("NEARAI_API_KEY".into(), "fake-key-1234567".into())],
            ..Default::default()
        };
        assert!(KeyScrub::for_spawn(&plain).is_none());
    }
}
