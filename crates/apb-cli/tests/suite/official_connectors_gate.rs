//! CI manifest gate (spec 2026-07-19-official-connectors-design, section 8,
//! tier 1): iterates every connector folder under the repository's
//! top-level `connectors/`, asserting each one is complete and correct,
//! fully offline. This is the gate that keeps the official connectors
//! always installable and correct; it discovers folders dynamically so it
//! grows as slice 5's tasks add github, telegram, smtp, and sentry one at
//! a time.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
        check_connector_folder(&root.join(&name), &name);
    }
}

/// The gate over one connector folder: panics on the first rule it breaks.
fn check_connector_folder(dir: &Path, name: &str) {
    // 1. Manifest parses and the name matches the folder.
    let yaml_path = dir.join("connector.yaml");
    let yaml = read(&yaml_path);
    let doc = ConnectorDoc::from_yaml(&yaml, name)
        .unwrap_or_else(|e| panic!("connectors/{name}/connector.yaml: {e}"));
    assert_eq!(doc.name, name);

    // 2. Tree digest computes.
    let digest = tree_digest(dir, &TreeLimits::default())
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
    let fm = frontmatter(&public, name);
    let meta: PublicMeta = yaml_serde::from_str(&fm)
        .unwrap_or_else(|e| panic!("connectors/{name}/PUBLIC.md frontmatter: {e}"));
    assert!(
        !meta.display_name.is_empty(),
        "{name} PUBLIC.md needs display_name"
    );
    assert!(!meta.summary.is_empty(), "{name} PUBLIC.md needs summary");

    // 4. args_schema is a JSON Schema object; every url-embedded args
    //    placeholder is in `required` (a missing routing arg breaks
    //    rendering, not just schema validation); response_pick is
    //    required on read_only HTTP functions, and is forbidden by the
    //    core validator on smtp and imap functions, so those are
    //    excluded here; every example validates against args_schema.
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
        let smtp_or_imap = f.smtp.is_some() || f.imap.is_some();
        if smtp_or_imap {
            assert!(
                f.response_pick.is_empty(),
                "{name}/{}: smtp/imap functions must not set response_pick",
                f.name
            );
        }
        assert!(
            read_only_projection_rule(
                f.read_only,
                &f.response_pick,
                &f.content_types,
                smtp_or_imap,
            ),
            "{name}/{} is read_only but declares neither response_pick nor content_types",
            f.name
        );
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
    let tests: serde_json::Value = yaml_serde::from_str(&tests_yaml)
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
    let out = crate::common::apb_std()
        .args(["connector", "test", "--dir"])
        .arg(dir)
        .output()
        .expect("failed to spawn apb");
    assert!(
        out.status.success(),
        "apb connector test --dir connectors/{name} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The gate itself, not only its rule: a folder complete in every other
/// way whose read-only HTTP function declares neither `response_pick` nor
/// `content_types` is refused by the read-only projection rule.
#[test]
fn the_gate_refuses_a_read_only_function_without_a_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("fixture");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        "name: fixture\nversion: 0.1.0\nhealthcheck: get_thing\n\
         account_fields:\n  - { name: api_base, required: true }\n\
         functions:\n  - name: get_thing\n    description: d\n    read_only: true\n\
         \x20   method: GET\n    url: \"{{account.api_base}}/thing\"\n\
         \x20   args_schema: { type: object, properties: {}, required: [] }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("PUBLIC.md"),
        "---\ndisplay_name: Fixture\nsummary: A fixture.\n---\n\nBody.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests.yaml"),
        "cases:\n  - function: get_thing\n    account: { api_base: https://example.test }\n\
         \x20   args: {}\n    expect:\n      method: GET\n      url: \"https://example.test/thing\"\n",
    )
    .unwrap();
    let refused = std::panic::catch_unwind(|| check_connector_folder(&dir, "fixture"))
        .expect_err("the gate passed a read-only function without a projection");
    let message = refused
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| refused.downcast_ref::<&str>().map(|m| m.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains(
            "fixture/get_thing is read_only but declares neither response_pick nor content_types"
        ),
        "the gate failed on another rule: {message}"
    );
}

/// The read-only projection rule of the official gate (D5): a read-only
/// HTTP function declares a non-empty `response_pick` unless it declares a
/// non-empty `content_types` guard (it returns a whole document, which a
/// projection would only cut; JSON documents included). smtp/imap/inbox
/// functions are decided elsewhere.
fn read_only_projection_rule(
    read_only: bool,
    response_pick: &[String],
    content_types: &[String],
    is_smtp_or_imap: bool,
) -> bool {
    if is_smtp_or_imap {
        return true;
    }
    !read_only || !response_pick.is_empty() || !content_types.is_empty()
}

#[test]
fn the_read_only_projection_rule_exempts_content_types_only() {
    let pick = ["a".to_string()];
    let text = ["text/".to_string()];
    // `read_text_file` (JSON included) passes without response_pick.
    assert!(read_only_projection_rule(true, &[], &text, false));
    // A projection alone passes, and so does a function that writes.
    assert!(read_only_projection_rule(true, &pick, &[], false));
    assert!(read_only_projection_rule(false, &[], &[], false));
    // A read-only function with neither fails.
    assert!(!read_only_projection_rule(true, &[], &[], false));
    // smtp/imap are out of this rule's scope.
    assert!(read_only_projection_rule(true, &[], &[], true));
}
