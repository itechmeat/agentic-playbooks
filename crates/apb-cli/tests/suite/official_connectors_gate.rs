//! CI manifest gate (spec 2026-07-19-official-connectors-design, section 8,
//! tier 1): iterates every connector folder under the repository's
//! top-level `connectors/`, asserting each one is complete and correct,
//! fully offline. This is the gate that keeps the official connectors
//! always installable and correct; it discovers folders dynamically so it
//! grows as slice 5's tasks add github, telegram, smtp, and sentry one at
//! a time.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::connector::template::{Namespace, placeholders};
use apb_core::connector::{ConnectorDoc, PublicMeta};
use apb_core::content::{TreeLimits, tree_digest};

fn repo_connectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors")
        .canonicalize()
        .expect("repository connectors/ directory must exist")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn connector_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Splits `PUBLIC.md` into its frontmatter block, requiring one to exist
/// (unlike the runtime path's best-effort parse; this gate wants a hard
/// failure on a missing or unparsable block, not a silent fallback).
fn frontmatter(public_md: &str, name: &str) -> String {
    let rest = public_md.strip_prefix("---\n").unwrap_or_else(|| {
        panic!("connectors/{name}/PUBLIC.md must start with a `---` frontmatter block")
    });
    let (fm, _body) = rest
        .split_once("\n---\n")
        .unwrap_or_else(|| panic!("connectors/{name}/PUBLIC.md frontmatter block is not closed"));
    fm.to_string()
}

#[test]
fn every_official_connector_folder_is_complete() {
    let root = repo_connectors_dir();
    for name in connector_names(&root) {
        let dir = root.join(&name);

        // 1. Manifest parses and the name matches the folder.
        let yaml_path = dir.join("connector.yaml");
        let yaml = read(&yaml_path);
        let doc = ConnectorDoc::from_yaml(&yaml, &name)
            .unwrap_or_else(|e| panic!("connectors/{name}/connector.yaml: {e}"));
        assert_eq!(doc.name, name);

        // 2. Tree digest computes.
        let digest = tree_digest(&dir, &TreeLimits::default())
            .unwrap_or_else(|e| panic!("connectors/{name} digest: {e}"));
        assert!(
            digest.starts_with("sha256:"),
            "{name} digest malformed: {digest}"
        );

        // 3. PUBLIC.md frontmatter parses and carries a display_name/summary.
        let public_path = dir.join("PUBLIC.md");
        assert!(
            public_path.is_file(),
            "connectors/{name}/PUBLIC.md is missing"
        );
        let public = read(&public_path);
        let fm = frontmatter(&public, &name);
        let meta: PublicMeta = serde_yaml_ng::from_str(&fm)
            .unwrap_or_else(|e| panic!("connectors/{name}/PUBLIC.md frontmatter: {e}"));
        assert!(
            !meta.display_name.is_empty(),
            "{name} PUBLIC.md needs display_name"
        );
        assert!(!meta.summary.is_empty(), "{name} PUBLIC.md needs summary");

        // 4. args_schema is a JSON Schema object; every url-embedded args
        //    placeholder is in `required` (a missing routing arg breaks
        //    rendering, not just schema validation); response_pick is
        //    declared iff the function is read_only and never on smtp
        //    functions; every example validates against args_schema.
        for f in &doc.functions {
            let schema = f
                .args_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{name}/{} has no args_schema", f.name));
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "{name}/{}: args_schema must be a JSON Schema object",
                f.name
            );
            let required: Vec<String> = schema
                .get("required")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(url) = &f.url {
                for (ns, arg_name) in placeholders(url).unwrap() {
                    if ns == Namespace::Args {
                        assert!(
                            required.contains(&arg_name),
                            "{name}/{}: url arg `{arg_name}` must be in args_schema.required",
                            f.name
                        );
                    }
                }
            }
            if f.smtp.is_some() || f.imap.is_some() {
                assert!(
                    f.response_pick.is_empty(),
                    "{name}/{}: smtp/imap functions must not set response_pick",
                    f.name
                );
            } else if f.read_only {
                assert!(
                    !f.response_pick.is_empty(),
                    "{name}/{} is read_only but declares no response_pick",
                    f.name
                );
            }
            for ex in &f.examples {
                let validator = jsonschema::validator_for(schema)
                    .unwrap_or_else(|e| panic!("{name}/{}: invalid args_schema: {e}", f.name));
                assert!(
                    validator.is_valid(&ex.args),
                    "{name}/{}: example args do not validate against args_schema: {:?}",
                    f.name,
                    ex.args
                );
            }
        }

        // 5. healthcheck exists and is read_only or mock.
        let hc_name = doc
            .healthcheck
            .as_ref()
            .unwrap_or_else(|| panic!("{name} declares no healthcheck"));
        let hc = doc
            .function(hc_name)
            .unwrap_or_else(|| panic!("{name} healthcheck `{hc_name}` names no function"));
        assert!(
            hc.read_only || hc.is_mock(),
            "{name} healthcheck `{hc_name}` must be read_only or mock"
        );

        // 6. Every function has at least one tests.yaml case.
        let tests_path = dir.join("tests.yaml");
        let tests_yaml = read(&tests_path);
        let tests: serde_json::Value = serde_yaml_ng::from_str(&tests_yaml)
            .unwrap_or_else(|e| panic!("connectors/{name}/tests.yaml: {e}"));
        let cases = tests["cases"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}/tests.yaml has no `cases` list"));
        let covered: HashSet<String> = cases
            .iter()
            .map(|c| c["function"].as_str().unwrap().to_string())
            .collect();
        for f in &doc.functions {
            assert!(
                covered.contains(&f.name),
                "{name}/tests.yaml has no case for function `{}`",
                f.name
            );
        }

        // 7. Every declared case passes, fully offline.
        let out = Command::new(env!("CARGO_BIN_EXE_apb"))
            .args(["connector", "test", "--dir"])
            .arg(&dir)
            .output()
            .expect("failed to spawn apb");
        assert!(
            out.status.success(),
            "apb connector test --dir connectors/{name} failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
