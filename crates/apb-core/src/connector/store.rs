//! The on-disk connector store: `<config_dir>/connectors/<name>/`, holding
//! `connector.yaml` (the machine manifest, `def::ConnectorDoc`) and an
//! optional `PUBLIC.md` storefront page (spec 2026-07-18-connectors-design,
//! sections 3.1-3.2). Mirrors `skills::resolve_skill`'s canonicalize +
//! containment check and `content::tree_digest` for the whole-folder digest
//! that later tasks snapshot into a run manifest.
//!
//! PUBLIC.md is deliberately best-effort: it is a storefront page for the
//! dashboard, never read by the engine at run time (spec 3.2), so a missing
//! file, a missing frontmatter block, or a frontmatter parse error must never
//! break the machine path (`load`, `list`). Each of those cases falls back to
//! a default `PublicMeta` with `display_name` set to the folder name, so a
//! connector without any storefront authoring still has a usable display
//! name.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::common::ConnectorError;
use super::def::ConnectorDoc;

/// `PUBLIC.md` frontmatter (spec 3.2): a storefront page describing a
/// connector for the dashboard. All fields default to empty so a partially
/// filled or missing frontmatter block never fails to parse; unknown keys in
/// the frontmatter are ignored rather than rejected (a storefront file with
/// extra fields must not break the machine path).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PublicMeta {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub icon: String,
}

/// A lightweight, list-friendly view of one installed connector: identity
/// plus its storefront metadata, without parsing the full manifest body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectorSummary {
    pub name: String,
    pub version: String,
    pub meta: PublicMeta,
}

/// A fully loaded connector: the parsed manifest, its raw source (snapshotted
/// verbatim by the engine into the run manifest), and the whole-folder tree
/// digest used for trust pinning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConnector {
    pub name: String,
    pub dir: PathBuf,
    /// Raw `connector.yaml` content, exactly as read from disk.
    pub yaml: String,
    pub doc: ConnectorDoc,
    /// Tree digest (`content::tree_digest`) of the whole connector folder,
    /// including `PUBLIC.md` and any other files alongside `connector.yaml`.
    pub digest: String,
}

/// The connector store root: `<config_dir>/connectors`. `None` in a
/// config-less environment, mirroring `crate::config::config_dir`.
pub fn connectors_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join("connectors"))
}

/// Lists installed connectors, sorted by name. Each entry in
/// `connectors_dir()` is considered only if its directory name passes
/// `validate_profile_name` and its `connector.yaml` parses; anything else
/// (a non-directory entry, an invalid name, a missing or unparsable
/// manifest) is skipped silently, matching `skills::list_available`. This
/// keeps a single broken connector from breaking the whole listing.
pub fn list() -> Vec<ConnectorSummary> {
    let Some(dir) = connectors_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if crate::profile::validate_profile_name(&name).is_err() {
            continue;
        }
        let path = entry.path();
        let Ok(yaml) = std::fs::read_to_string(path.join("connector.yaml")) else {
            continue;
        };
        let Ok(doc) = ConnectorDoc::from_yaml(&yaml, &name) else {
            continue;
        };
        let meta = public_meta(&path);
        out.push(ConnectorSummary {
            name,
            version: doc.version,
            meta,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Loads one connector by name: validates the name slug, resolves and
/// containment-checks its directory under `connectors_dir()` exactly like
/// `skills::resolve_skill` does, reads and parses `connector.yaml`, and
/// digests the whole folder.
pub fn load(name: &str) -> Result<LoadedConnector, ConnectorError> {
    crate::profile::validate_profile_name(name)
        .map_err(|e| ConnectorError::Invalid(format!("connector name `{name}`: {e}")))?;

    let Some(base) = connectors_dir() else {
        return Err(ConnectorError::NotFound(name.to_string()));
    };
    let cand = base.join(name);
    if !cand.is_dir() {
        return Err(ConnectorError::NotFound(name.to_string()));
    }

    let canonical_path = std::fs::canonicalize(&cand)?;
    // Defense-in-depth: even with a valid name, a symlink inside the
    // connectors root could lead outside it - check containment.
    let canonical_root = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
    if !canonical_path.starts_with(&canonical_root) {
        return Err(ConnectorError::Invalid(format!(
            "connector `{name}` resolves outside its connectors root"
        )));
    }

    let yaml_path = canonical_path.join("connector.yaml");
    let yaml = std::fs::read_to_string(&yaml_path).map_err(|e| {
        ConnectorError::Invalid(format!(
            "connector `{name}` missing or unreadable connector.yaml at {}: {e}",
            yaml_path.display()
        ))
    })?;
    let doc = ConnectorDoc::from_yaml(&yaml, name)?;

    let digest =
        crate::content::tree_digest(&canonical_path, &crate::content::TreeLimits::default())
            .map_err(|e| {
                ConnectorError::Invalid(format!("connector `{name}` digest error: {e}"))
            })?;

    Ok(LoadedConnector {
        name: name.to_string(),
        dir: canonical_path,
        yaml,
        doc,
        digest,
    })
}

/// Splits `PUBLIC.md` content into `(frontmatter, body)` when the file
/// starts with a `---` line and has a matching closing `---` line
/// thereafter. Returns `None` when there is no leading `---` line or no
/// closing one (the file is treated as having no frontmatter at all).
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let first_nl = content.find('\n')?;
    let first_line = content[..first_nl].trim_end_matches('\r');
    if first_line != "---" {
        return None;
    }

    let after_first = &content[first_nl + 1..];
    let mut consumed = 0usize;
    for line in after_first.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            let frontmatter = &after_first[..consumed];
            let body = &after_first[consumed + line.len()..];
            return Some((frontmatter, body));
        }
        consumed += line.len();
    }
    None
}

/// Default metadata for a connector with no usable storefront page: an
/// otherwise-empty `PublicMeta` whose `display_name` is the connector name, so
/// the dashboard always has something to show.
fn fallback_meta(name: &str) -> PublicMeta {
    PublicMeta {
        display_name: name.to_string(),
        ..Default::default()
    }
}

/// Parses `PUBLIC.md` frontmatter from content already in memory, falling back
/// to `fallback_meta(name)` when there is no `---`-delimited block or it fails
/// to parse as YAML. Split out from [`public_meta`] because an embedded
/// official connector carries its `PUBLIC.md` as bytes in the binary and never
/// has a directory to read it from.
pub fn public_meta_from_str(content: &str, name: &str) -> PublicMeta {
    let Some((frontmatter, _body)) = split_frontmatter(content) else {
        return fallback_meta(name);
    };
    let mut meta: PublicMeta =
        serde_yaml_ng::from_str(frontmatter).unwrap_or_else(|_| fallback_meta(name));
    // Frontmatter that parses but omits `display_name` (or sets it to an
    // empty/whitespace-only string) deserializes to `""` via the field's
    // serde default, not the folder name - fall back the same way the
    // no-frontmatter and parse-error cases already do, so a connector with a
    // storefront page that simply forgot `display_name` still has a usable
    // one.
    if meta.display_name.trim().is_empty() {
        meta.display_name = name.to_string();
    }
    meta
}

/// Reads and parses `PUBLIC.md`'s frontmatter (spec 3.2). Falls back to a
/// default `PublicMeta` (see module docs) when the file is missing, has no
/// `---`-delimited frontmatter block, or the frontmatter fails to parse as
/// YAML - the storefront must never break the machine path.
pub fn public_meta(dir: &Path) -> PublicMeta {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match std::fs::read_to_string(dir.join("PUBLIC.md")) {
        Ok(content) => public_meta_from_str(&content, &name),
        Err(_) => fallback_meta(&name),
    }
}

/// The markdown body after `PUBLIC.md`'s frontmatter block, from content
/// already in memory. Empty when there is no `---`-delimited block. Split out
/// from [`public_body`] for the same reason as [`public_meta_from_str`]: an
/// embedded official connector carries `PUBLIC.md` as bytes in the binary and
/// has no directory to read it from.
pub fn public_body_from_str(content: &str) -> String {
    match split_frontmatter(content) {
        Some((_, body)) => body.to_string(),
        None => String::new(),
    }
}

/// The markdown body of `PUBLIC.md`, after its frontmatter block. Empty
/// when the file is missing or has no frontmatter block.
pub fn public_body(dir: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(dir.join("PUBLIC.md")) else {
        return String::new();
    };
    public_body_from_str(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ConfigDirGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                    None => std::env::remove_var("APB_CONFIG_DIR"),
                }
            }
        }
    }

    /// Points `APB_CONFIG_DIR` at a fresh tempdir for the duration of the
    /// guard, restoring the prior value on drop. Must be created under
    /// `crate::env_test_lock()`.
    fn set_config_dir() -> (tempfile::TempDir, ConfigDirGuard) {
        let cfg = tempfile::tempdir().unwrap();
        let guard = ConfigDirGuard {
            prior: std::env::var_os("APB_CONFIG_DIR"),
        };
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        (cfg, guard)
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const MINIMAL_YAML: &str = "name: foo\nversion: 0.1.0\n";

    // --- load / digest ---------------------------------------------------

    #[test]
    fn load_valid_folder_gives_a_stable_digest() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();
        let dir = connectors_dir().unwrap().join("foo");
        write(&dir.join("connector.yaml"), MINIMAL_YAML);

        let first = load("foo").unwrap();
        let second = load("foo").unwrap();

        assert_eq!(first.name, "foo");
        assert_eq!(first.yaml, MINIMAL_YAML);
        assert_eq!(first.doc.version, "0.1.0");
        assert_eq!(first.digest, second.digest);
        assert!(first.digest.starts_with("sha256:"));
        drop(cfg);
    }

    #[test]
    fn digest_changes_when_public_md_changes() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();
        let dir = connectors_dir().unwrap().join("foo");
        write(&dir.join("connector.yaml"), MINIMAL_YAML);

        let before = load("foo").unwrap().digest;

        write(
            &dir.join("PUBLIC.md"),
            "---\ndisplay_name: Foo\nsummary: does stuff\n---\nBody text.\n",
        );
        let after = load("foo").unwrap().digest;

        assert_ne!(before, after);
        drop(cfg);
    }

    #[test]
    fn bad_name_is_rejected() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        let err = load("Bad_Name").unwrap_err();
        assert!(matches!(err, ConnectorError::Invalid(_)));
        drop(cfg);
    }

    #[test]
    fn path_traversal_name_is_rejected() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        let err = load("../etc").unwrap_err();
        assert!(matches!(err, ConnectorError::Invalid(_)));
        drop(cfg);
    }

    #[test]
    fn missing_connector_is_not_found() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        let err = load("nope").unwrap_err();
        assert!(matches!(err, ConnectorError::NotFound(_)));
        drop(cfg);
    }

    // --- list --------------------------------------------------------------

    #[test]
    fn list_skips_broken_yaml_and_parses_frontmatter() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();
        let base = connectors_dir().unwrap();

        write(
            &base.join("good").join("connector.yaml"),
            "name: good\nversion: 0.1.0\n",
        );
        write(
            &base.join("good").join("PUBLIC.md"),
            "---\ndisplay_name: Good Co\nsummary: A good connector\ntags: [alpha, beta]\n---\nBody.\n",
        );
        write(
            &base.join("broken").join("connector.yaml"),
            "name: broken\nversion: \"\"\n", // empty version -> from_yaml rejects
        );
        // A non-directory entry alongside the connector folders must not
        // trip up the listing.
        write(&base.join("stray.txt"), "not a connector");

        let out = list();
        let names: Vec<&str> = out.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["good"]);

        let good = &out[0];
        assert_eq!(good.version, "0.1.0");
        assert_eq!(good.meta.display_name, "Good Co");
        assert_eq!(good.meta.summary, "A good connector");
        assert_eq!(
            good.meta.tags,
            vec!["alpha".to_string(), "beta".to_string()]
        );
        drop(cfg);
    }

    #[test]
    fn list_sorts_by_name() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();
        let base = connectors_dir().unwrap();

        for name in ["zeta", "alpha", "mid"] {
            let yaml = format!("name: {name}\nversion: 0.1.0\n");
            write(&base.join(name).join("connector.yaml"), &yaml);
        }

        let names: Vec<String> = list().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
        drop(cfg);
    }

    #[test]
    fn list_empty_without_config_dir() {
        let _lock = crate::env_test_lock();
        // Save and restore every env var that steers `config_dir()`, so the
        // test is hermetic. Leaving HOME set (as this test once did) makes
        // `config_dir()` resolve to the machine's real `~/.config/apb`, which
        // may hold installed connectors - so the assertion would flip based on
        // developer state. Clear all three to genuinely exercise the
        // config-less branch (`connectors_dir()` -> None).
        struct EnvGuard([(&'static str, Option<std::ffi::OsString>); 3]);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                for (key, prior) in &self.0 {
                    unsafe {
                        match prior {
                            Some(v) => std::env::set_var(key, v),
                            None => std::env::remove_var(key),
                        }
                    }
                }
            }
        }
        let keys = ["APB_CONFIG_DIR", "XDG_CONFIG_HOME", "HOME"];
        let _g = EnvGuard(keys.map(|k| (k, std::env::var_os(k))));
        unsafe {
            for k in keys {
                std::env::remove_var(k);
            }
        }
        assert!(list().is_empty());
    }

    // --- public_meta / public_body -----------------------------------------

    #[test]
    fn public_meta_falls_back_to_folder_name_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        std::fs::create_dir_all(&sub).unwrap();

        let meta = public_meta(&sub);
        assert_eq!(meta.display_name, "my-connector");
        assert_eq!(meta.summary, "");
    }

    #[test]
    fn public_meta_falls_back_when_no_frontmatter_block() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(&sub.join("PUBLIC.md"), "Just a plain markdown file.\n");

        let meta = public_meta(&sub);
        assert_eq!(meta.display_name, "my-connector");
    }

    #[test]
    fn public_meta_falls_back_when_frontmatter_fails_to_parse() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(
            &sub.join("PUBLIC.md"),
            "---\n[not: valid, yaml\n---\nBody\n",
        );

        let meta = public_meta(&sub);
        assert_eq!(meta.display_name, "my-connector");
    }

    #[test]
    fn public_meta_falls_back_when_frontmatter_omits_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(
            &sub.join("PUBLIC.md"),
            "---\nsummary: does stuff\n---\nBody\n",
        );

        let meta = public_meta(&sub);
        assert_eq!(meta.display_name, "my-connector");
        assert_eq!(meta.summary, "does stuff");
    }

    #[test]
    fn public_meta_ignores_unknown_frontmatter_keys() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(
            &sub.join("PUBLIC.md"),
            "---\ndisplay_name: Foo\nbogus_future_field: 1\n---\nBody\n",
        );

        let meta = public_meta(&sub);
        assert_eq!(meta.display_name, "Foo");
    }

    #[test]
    fn public_meta_parses_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(
            &sub.join("PUBLIC.md"),
            "---\ndisplay_name: Foo\nsummary: does stuff\ntags: [a, b]\npublisher: Acme\nhomepage: https://example.com\nicon: rocket\n---\nBody\n",
        );

        let meta = public_meta(&sub);
        assert_eq!(meta.display_name, "Foo");
        assert_eq!(meta.summary, "does stuff");
        assert_eq!(meta.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(meta.publisher, "Acme");
        assert_eq!(meta.homepage, "https://example.com");
        assert_eq!(meta.icon, "rocket");
    }

    #[test]
    fn public_body_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        std::fs::create_dir_all(&sub).unwrap();

        assert_eq!(public_body(&sub), "");
    }

    #[test]
    fn public_body_empty_when_no_frontmatter_block() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(&sub.join("PUBLIC.md"), "Just markdown.\n");

        assert_eq!(public_body(&sub), "");
    }

    #[test]
    fn public_body_returns_text_after_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("my-connector");
        write(
            &sub.join("PUBLIC.md"),
            "---\ndisplay_name: Foo\n---\nHello there.\nSecond line.\n",
        );

        assert_eq!(public_body(&sub), "Hello there.\nSecond line.\n");
    }
}
