//! Embedded official connectors (spec 2026-07-19-official-connectors, section
//! 3): the repo-level `connectors/<name>/` folders baked into `apb-core` with
//! rust-embed, so every crate above core can enumerate and materialize the
//! official set from the binary. The folder format is exactly the installed
//! format, so a future marketplace is a second source without a format change.
//!
//! Mirrors `store::list`'s resilience: a folder whose name is not a valid slug
//! or whose `connector.yaml` fails to parse is skipped rather than breaking the
//! whole listing.

use std::collections::BTreeMap;
use std::path::Path;

use super::def::ConnectorDoc;

/// The embedded `connectors/` tree, baked in at build time (release) or read
/// from the repo folder at run time (debug), exactly like `apb-server`'s
/// `web/dist` embed.
#[derive(rust_embed::Embed)]
#[folder = "../../connectors"]
struct OfficialAssets;

/// One embedded official connector: its folder name, the version parsed from
/// the embedded manifest, and the full file map (path relative to the
/// connector folder -> bytes) needed to materialize it on disk.
pub struct OfficialConnector {
    pub name: String,
    pub version: String,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl OfficialConnector {
    /// Writes every embedded file of this connector into `dir` (creating
    /// parents), atomically per file. `dir` is the connector folder itself
    /// (e.g. `<config_dir>/connectors/<name>`); the caller decides placement.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        for (rel, bytes) in &self.files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::fsutil::atomic_write(&path, bytes)?;
        }
        Ok(())
    }
}

/// Every embedded official connector, sorted by name. A folder whose name is
/// not a valid slug, that has no `connector.yaml`, or whose manifest does not
/// parse (or whose `name` does not match the folder) is skipped.
pub fn list() -> Vec<OfficialConnector> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    for path in OfficialAssets::iter() {
        let path = path.as_ref();
        // Only files nested under a connector folder count; a stray top-level
        // file has no `<name>/` prefix and is ignored.
        let Some((name, rel)) = path.split_once('/') else {
            continue;
        };
        if crate::profile::validate_profile_name(name).is_err() {
            continue;
        }
        if let Some(file) = OfficialAssets::get(path) {
            grouped
                .entry(name.to_string())
                .or_default()
                .insert(rel.to_string(), file.data.into_owned());
        }
    }

    let mut out = Vec::new();
    for (name, files) in grouped {
        let Some(raw) = files.get("connector.yaml") else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(raw) else {
            continue;
        };
        let Ok(doc) = ConnectorDoc::from_yaml(text, &name) else {
            continue;
        };
        out.push(OfficialConnector {
            name,
            version: doc.version,
            files,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// One embedded connector by name, or `None` when it is not embedded.
pub fn get(name: &str) -> Option<OfficialConnector> {
    list().into_iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_includes_the_seed_example_connector() {
        let all = list();
        let example = all
            .iter()
            .find(|c| c.name == "example")
            .expect("embedded `example` connector must be present");
        assert_eq!(example.version, "0.1.0");
        assert!(example.files.contains_key("connector.yaml"));
        assert!(example.files.contains_key("tests.yaml"));
        assert!(example.files.contains_key("PUBLIC.md"));
    }

    #[test]
    fn get_returns_a_connector_whose_manifest_parses() {
        let c = get("example").expect("example present");
        let yaml = std::str::from_utf8(c.files.get("connector.yaml").unwrap()).unwrap();
        let doc = crate::connector::def::ConnectorDoc::from_yaml(yaml, "example").unwrap();
        assert_eq!(doc.version, "0.1.0");
    }

    #[test]
    fn write_to_materializes_every_file_under_the_target() {
        let c = get("example").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("example");
        c.write_to(&dir).unwrap();
        assert!(dir.join("connector.yaml").is_file());
        assert!(dir.join("tests.yaml").is_file());
        assert!(dir.join("PUBLIC.md").is_file());
        // The materialized folder digests and loads like any installed one.
        let digest =
            crate::content::tree_digest(&dir, &crate::content::TreeLimits::default()).unwrap();
        assert!(digest.starts_with("sha256:"));
    }

    #[test]
    fn get_unknown_name_is_none() {
        assert!(get("definitely-not-a-connector").is_none());
    }
}
