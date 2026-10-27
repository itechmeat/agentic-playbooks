//! Where an imported version came from: the bundle file, the exporter's
//! own view of it, and the scripts it brought. Stored in the version's
//! provenance sidecar so the import can be shown and its scripts approved
//! by their exact final digest.

use serde::{Deserialize, Serialize};

use crate::content::{TreeFile, sha256_hex};

/// The longest file name kept in the provenance (in bytes).
pub const MAX_FILE_NAME: usize = 128;

/// What the exporter wrote about itself into a bundle: the trust digest of
/// the version it exported, the scope it lived in, the apb version that
/// wrote the file and when.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleOrigin {
    /// The exporter's trust digest of the version (YAML plus scripts).
    pub digest: String,
    /// `project` or `global`.
    pub scope: String,
    pub apb_version: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub exported_at_ms: u64,
}

/// One script an import brought, by its content fingerprint.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEntry {
    /// Path relative to `scripts/`.
    pub path: String,
    /// `sha256:<hex>` of the file bytes.
    pub sha256: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes: u64,
    #[serde(default)]
    pub executable: bool,
}

impl ScriptEntry {
    pub fn of(file: &TreeFile) -> Self {
        ScriptEntry {
            path: file.rel.clone(),
            sha256: sha256_hex(&file.bytes),
            bytes: file.bytes.len() as u64,
            executable: file.executable,
        }
    }
}

/// The provenance of a version written by an import.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProvenance {
    /// `sha256:<hex>` of the bundle file as read.
    pub bundle_digest: String,
    /// The base name of the bundle file, sanitized, when it was a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub file_name: Option<String>,
    /// The id and version the bundle declared.
    pub source_id: String,
    pub source_version: String,
    /// The exporter's own view, when the bundle carried one. Informational:
    /// nothing local vouches for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<BundleOrigin>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub imported_at_ms: u64,
    /// The scripts the import wrote (empty when it carried none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<ScriptEntry>>", optional))]
    pub scripts: Vec<ScriptEntry>,
    /// The trust digest of the version the import wrote: what an approval
    /// of the import must name.
    #[serde(default)]
    pub digest: String,
}

/// The base name of `name` (the last `/` or `\` segment), with hidden
/// characters dropped (controls, bidi overrides, zero-width and other
/// invisible format characters, see [`crate::text::is_hidden_char`]) and
/// cut to [`MAX_FILE_NAME`] bytes on a char boundary. `None` when nothing
/// printable is left.
pub fn sanitize_file_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut out = String::new();
    for c in base.chars().filter(|c| !crate::text::is_hidden_char(*c)) {
        if out.len() + c.len_utf8() > MAX_FILE_NAME {
            break;
        }
        out.push(c);
    }
    let out = out.trim().to_string();
    (!out.is_empty() && out != "." && out != "..").then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_keeps_only_a_printable_base_name() {
        assert_eq!(
            sanitize_file_name("/tmp/x/release\u{1b}[1m.apb.json").as_deref(),
            Some("release[1m.apb.json")
        );
        assert_eq!(
            sanitize_file_name("C:\\a\\b.json").as_deref(),
            Some("b.json")
        );
        assert_eq!(
            sanitize_file_name("release\u{202E}nosj.hs").as_deref(),
            Some("releasenosj.hs")
        );
        assert_eq!(
            sanitize_file_name("a\u{200B}b\u{FEFF}.json").as_deref(),
            Some("ab.json")
        );
        assert_eq!(sanitize_file_name("dir/"), None);
        assert_eq!(
            sanitize_file_name(&"é".repeat(100)).map(|s| s.len()),
            Some(128)
        );
    }
}
