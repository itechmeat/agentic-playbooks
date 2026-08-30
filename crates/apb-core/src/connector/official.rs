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

    /// This connector's parsed manifest, from the embedded `connector.yaml`.
    /// `None` when the file is absent, is not UTF-8, or fails to parse -
    /// [`list`] already skips such a folder, so a connector obtained from
    /// [`list`] or [`get`] always parses; the `Option` exists so a caller
    /// never has to unwrap on that invariant.
    pub fn doc(&self) -> Option<ConnectorDoc> {
        let raw = self.files.get("connector.yaml")?;
        let text = std::str::from_utf8(raw).ok()?;
        ConnectorDoc::from_yaml(text, &self.name).ok()
    }

    /// The markdown body of this connector's embedded `PUBLIC.md`, after its
    /// frontmatter block. Empty when the file is absent, is not UTF-8, or has
    /// no frontmatter block, matching [`super::store::public_body`].
    pub fn public_body(&self) -> String {
        self.files
            .get("PUBLIC.md")
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(super::store::public_body_from_str)
            .unwrap_or_default()
    }

    /// The storefront metadata from this connector's embedded `PUBLIC.md`, so
    /// an "available to install" listing can show the same `display_name`,
    /// `summary` and `tags` the installed listing shows. Falls back to a
    /// `PublicMeta` carrying just the connector name when `PUBLIC.md` is
    /// absent, is not UTF-8, or has no usable frontmatter block.
    pub fn meta(&self) -> super::store::PublicMeta {
        self.files
            .get("PUBLIC.md")
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(|content| super::store::public_meta_from_str(content, &self.name))
            .unwrap_or_else(|| super::store::PublicMeta {
                display_name: self.name.clone(),
                ..Default::default()
            })
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
    fn list_includes_the_official_github_connector() {
        let all = list();
        let github = all
            .iter()
            .find(|c| c.name == "github")
            .expect("embedded `github` connector must be present");
        assert_eq!(github.version, "0.1.0");
        assert!(github.files.contains_key("connector.yaml"));
        assert!(github.files.contains_key("tests.yaml"));
        assert!(github.files.contains_key("PUBLIC.md"));
    }

    /// The four-file convention from docs/CONNECTORS.md: the machine manifest,
    /// its offline contract cases, the dashboard storefront, and the two setup
    /// documents. A connector added without `README.md`/`INSTALL.md` still
    /// installs and runs, which is exactly why the omission would go unnoticed
    /// without this test.
    ///
    /// The name set is pinned exactly (sorted, since `list()` sorts by name)
    /// rather than just counted: a bare `len() >= 11` would not notice one
    /// official connector silently dropping out while another was added,
    /// since the count could stay the same or even grow.
    #[test]
    fn every_official_connector_carries_the_full_file_set() {
        let all = list();
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "amocrm", "asana", "atrip", "discord", "github", "gitlab", "imap", "sentry",
                "slack", "smtp", "telegram", "twenty", "whatsapp", "youtrack", "zulip",
            ],
            "the embedded official connector set changed; update this list deliberately"
        );
        for c in &all {
            for required in [
                "connector.yaml",
                "tests.yaml",
                "PUBLIC.md",
                "README.md",
                "INSTALL.md",
            ] {
                assert!(
                    c.files.contains_key(required),
                    "connector `{}` is missing {required}",
                    c.name
                );
            }
        }
    }

    /// Each setup document must be about the connector it ships with: a copied
    /// file whose heading and agent-install prompt still name the connector it
    /// was copied from is worse than no file, since it sends the reader to
    /// another connector's runbook.
    ///
    /// The heading is matched as the whole `# <name>:` prefix rather than by
    /// substring, because connector names nest: a document headed
    /// `# github: ...` contains the name of a hypothetical `git` connector, and
    /// a substring check would pass exactly the copy-paste this test exists to
    /// catch.
    #[test]
    fn setup_documents_name_their_own_connector() {
        for c in &list() {
            for doc in ["README.md", "INSTALL.md"] {
                let body = std::str::from_utf8(c.files.get(doc).expect(doc)).expect("utf-8");
                let first = body.lines().next().unwrap_or_default();
                let expected = format!("# {}:", c.name);
                assert!(
                    first.starts_with(&expected),
                    "`{}`/{doc} heading must start with `{expected}`, got: {first}",
                    c.name
                );
            }
            let readme = std::str::from_utf8(c.files.get("README.md").unwrap()).unwrap();
            assert!(
                readme.contains(&format!("connectors/{}/INSTALL.md", c.name)),
                "`{}`/README.md must point an agent at its own INSTALL.md",
                c.name
            );
        }
    }

    #[test]
    fn get_returns_a_connector_whose_manifest_parses() {
        let c = get("github").expect("github present");
        let yaml = std::str::from_utf8(c.files.get("connector.yaml").unwrap()).unwrap();
        let doc = crate::connector::def::ConnectorDoc::from_yaml(yaml, "github").unwrap();
        assert_eq!(doc.version, "0.1.0");
    }

    #[test]
    fn write_to_materializes_every_file_under_the_target() {
        let c = get("github").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("github");
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
