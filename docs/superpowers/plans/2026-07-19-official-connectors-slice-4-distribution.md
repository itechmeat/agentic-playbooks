# Official Connectors Slice 4: Distribution and Contract Test Runner - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the distribution machinery for official connectors: a top-level `connectors/` folder embedded into `apb-core` via rust-embed, an `apb connector install` command that materializes an embedded folder into the global store and seeds its trust (plus a `--from-dir` sideload path that does not), an "available" section in `apb connector list`, a `tests.yaml` schema parser, an offline contract-test runner that reuses the dry-run render path, and an `apb connector test` command. This is the machinery the slice-5 CI manifest gate calls; the gate itself is slice 5.

**Architecture:** `connectors/<name>/{connector.yaml,PUBLIC.md,tests.yaml}` at the repo root is embedded into `apb-core` behind a small `connector::official` API (`list`/`get`/`OfficialConnector::write_to`). The `tests.yaml` schema is a pure-data module `connector::contract` in `apb-core` (`deny_unknown_fields`). The offline runner lives in `apb-engine` (`connector_test`), reusing an extracted `connector_call::render_http` so a contract test renders exactly what a `--dry-run` call renders, with secrets stubbed to `"test-secret"` and no network. The CLI (`apb-cli/src/connector.rs`) gains `install`, `test`, and the list "available" section.

**Tech Stack:** Rust edition 2024, workspace crates `apb-core` <- `apb-engine` <- (`apb-cli`). rust-embed 8.12 (already used by `apb-server`), serde / serde_yaml_ng / serde_json, sha2 tree digests (`apb_core::content`), the existing `apb_core::trust::TrustStore` and `apb_core::connector::{store,config,template,def}`.

## Global Constraints

Copied verbatim from CLAUDE.md; every task must hold these:

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere in code or prose. Machine-facing fields are English; user-facing chat messages are written in the user's chat language.
- State files are written atomically (temp + rename, 0600 on unix) via `apb_core::fsutil`.
- Secret values (auth files) are never returned, logged, or cached.
- Profile and connector (folder) names: `[a-z0-9][a-z0-9-]*`, at most 64 chars, validated with `apb_core::profile::validate_profile_name`. Function / account-field / placeholder identifiers use `validate_snake_name` (`[a-z0-9][a-z0-9_]*`, max 64).
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- Before code is ready to commit, the code-ranker check must pass. Warm the cargo cache first (`cargo metadata --format-version 1 >/dev/null`), then `code-ranker check .` (exit != 0 on a violation); for a violation read `code-ranker docs base <ID>`, fix, and re-run until clean.
- Commit only after the owner approves. Commits use `git commit --signoff` and end with a `Co-Authored-By` trailer for the acting model. Never add a visible AI-authorship marker to public prose.

Per-task verification block (run at the end of every task before its commit step):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo metadata --format-version 1 >/dev/null && code-ranker check .
```

---

### Task 1: tests.yaml schema in apb-core (`connector::contract`)

Pure-data parser for `tests.yaml`, strict at the `TestsDoc`/`TestCase` level, with an `Expectation` that discriminates HTTP vs smtp vs mock by shape. No engine dependency, lands independently.

**Files:**
- Create: `crates/apb-core/src/connector/contract.rs`
- Modify: `crates/apb-core/src/connector/mod.rs` (declare and re-export the module)

**Interfaces:**
- Produces `apb_core::connector::contract::TestsDoc { cases: Vec<TestCase> }`
- Produces `TestsDoc::from_yaml(&str) -> Result<TestsDoc, ConnectorError>`
- Produces `TestCase { function: String, account: BTreeMap<String,String>, args: serde_json::Value, expect: Expectation }`
- Produces `Expectation` and `Expectation::resolve(&self) -> Result<ExpectKind<'_>, String>`

Steps:

- [ ] Write the failing test module at the bottom of `crates/apb-core/src/connector/contract.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_smtp_and_mock_cases() {
        let yaml = r#"
cases:
  - function: create_issue
    account: { api_base: https://api.github.com }
    args: { owner: acme, repo: site, title: "Broken build" }
    expect:
      method: POST
      url: https://api.github.com/repos/acme/site/issues
      body_contains: { title: "Broken build" }
  - function: send_email
    account: { host: smtp.example.com, port: "587", from_email: a@b.c }
    args: { to: x@y.z, subject: Hi, body_text: Test }
    expect:
      envelope: { from: a@b.c, to: [x@y.z], subject: Hi }
  - function: ping
    account: {}
    args: {}
    expect:
      status: 200
      body: { ok: true }
"#;
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert_eq!(doc.cases.len(), 3);
        assert!(matches!(doc.cases[0].expect.resolve().unwrap(), ExpectKind::Http { .. }));
        assert!(matches!(doc.cases[1].expect.resolve().unwrap(), ExpectKind::Smtp(_)));
        assert!(matches!(doc.cases[2].expect.resolve().unwrap(), ExpectKind::Mock { .. }));
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let yaml = "cases: []\nbogus: 1\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    #[test]
    fn unknown_case_key_is_rejected() {
        let yaml = "cases:\n  - function: f\n    bogus: 1\n    expect: { status: 200, body: {} }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    #[test]
    fn args_default_to_null_and_account_to_empty() {
        let yaml = "cases:\n  - function: ping\n    expect: { status: 200, body: {} }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert!(doc.cases[0].account.is_empty());
        assert!(doc.cases[0].args.is_null());
    }

    #[test]
    fn mock_expectation_missing_status_is_a_resolve_error() {
        let yaml = "cases:\n  - function: ping\n    expect: { body: { ok: true } }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert!(doc.cases[0].expect.resolve().is_err());
    }
}
```

- [ ] Run it and watch it fail to compile (module body not written yet):
  `cargo test -p apb-core --lib connector::contract` -> expected failure: `cannot find type TestsDoc`.

- [ ] Write the module body above the test block:

```rust
//! `tests.yaml` schema (spec 2026-07-19-official-connectors, section 4.6): the
//! declarative offline contract tests shipped inside a connector folder. Parsed
//! here as pure data (`deny_unknown_fields` at the document and case level);
//! executed by the engine's offline runner (`apb_engine::connector_test`),
//! which renders each case through the same path a `--dry-run` call uses and
//! checks the rendered request against the `expect` block.
//!
//! `Expectation` is one struct with all optional fields (not an untagged enum)
//! so `deny_unknown_fields` actually applies (serde ignores it inside untagged
//! variants); `resolve` discriminates by shape - `envelope` -> smtp,
//! `status`/`body` -> mock, otherwise HTTP (`method` + `url`).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::common::ConnectorError;

/// The whole `tests.yaml` document: an ordered list of cases.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestsDoc {
    #[serde(default)]
    pub cases: Vec<TestCase>,
}

/// One contract-test case: the function to render, fake non-secret account
/// field values, the call args, and the expected rendered result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    pub function: String,
    #[serde(default)]
    pub account: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Value,
    pub expect: Expectation,
}

/// The expected rendered result. Shape-discriminated by `resolve`: exactly one
/// of the HTTP (`method` + `url`), smtp (`envelope`), or mock (`status` +
/// `body`) shapes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub body_contains: Option<Value>,
    #[serde(default)]
    pub envelope: Option<Envelope>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub body: Option<Value>,
}

/// The smtp envelope a `send_email`-shaped case asserts.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
}

/// The resolved, shape-typed view of an `Expectation`, borrowed from it.
pub enum ExpectKind<'a> {
    Http {
        method: &'a str,
        url: &'a str,
        headers: Option<&'a BTreeMap<String, String>>,
        body_contains: Option<&'a Value>,
    },
    Smtp(&'a Envelope),
    Mock {
        status: u16,
        body: &'a Value,
    },
}

impl Expectation {
    /// Discriminates the expectation shape. `envelope` -> smtp; `status` or
    /// `body` -> mock; otherwise HTTP (which requires `method` and `url`). An
    /// incomplete shape is an error naming what is missing.
    pub fn resolve(&self) -> Result<ExpectKind<'_>, String> {
        if let Some(env) = &self.envelope {
            return Ok(ExpectKind::Smtp(env));
        }
        if self.status.is_some() || self.body.is_some() {
            let status = self
                .status
                .ok_or_else(|| "mock expectation needs a `status`".to_string())?;
            let body = self
                .body
                .as_ref()
                .ok_or_else(|| "mock expectation needs a `body`".to_string())?;
            return Ok(ExpectKind::Mock { status, body });
        }
        let method = self.method.as_deref().ok_or_else(|| {
            "expectation must be http (`method` + `url`), smtp (`envelope`), or mock (`status` + `body`)".to_string()
        })?;
        let url = self
            .url
            .as_deref()
            .ok_or_else(|| "http expectation needs a `url`".to_string())?;
        Ok(ExpectKind::Http {
            method,
            url,
            headers: self.headers.as_ref(),
            body_contains: self.body_contains.as_ref(),
        })
    }
}

impl TestsDoc {
    /// Parses a `tests.yaml` document.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConnectorError> {
        serde_yaml_ng::from_str(yaml).map_err(|e| ConnectorError::Yaml(e.to_string()))
    }
}
```

- [ ] Add the module to `crates/apb-core/src/connector/mod.rs`: insert `pub mod contract;` after `pub mod config;` and `pub use contract::*;` in the re-export block.

- [ ] Run to pass: `cargo test -p apb-core --lib connector::contract` -> all tests pass.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "connector: add tests.yaml schema (contract module)"` with the acting-model `Co-Authored-By` trailer.

---

### Task 2: Seed connectors/ folder and embed it into apb-core (`connector::official`)

Create the repo-level `connectors/example/` seed (a valid mock + HTTP connector with a `tests.yaml`) and the rust-embed API. The seed folder is required because rust-embed's `#[folder]` path must exist at compile time; slice 5 replaces `example/` with the four real manifests.

**Files:**
- Create: `connectors/example/connector.yaml`
- Create: `connectors/example/PUBLIC.md`
- Create: `connectors/example/tests.yaml`
- Create: `crates/apb-core/src/connector/official.rs`
- Modify: `crates/apb-core/Cargo.toml` (add `rust-embed`)
- Modify: `crates/apb-core/src/connector/mod.rs` (declare and re-export)

**Interfaces:**
- Produces `apb_core::connector::official::list() -> Vec<OfficialConnector>`
- Produces `apb_core::connector::official::get(name: &str) -> Option<OfficialConnector>`
- Produces `OfficialConnector { name: String, version: String, files: BTreeMap<String, Vec<u8>> }`
- Produces `OfficialConnector::write_to(&self, dir: &Path) -> std::io::Result<()>`

Steps:

- [ ] Create `connectors/example/connector.yaml` (valid per `ConnectorDoc::from_yaml`, name matches folder, secret confined to auth):

```yaml
name: example
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - name: api_base
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: ping
    description: Reachability check with no network call
    read_only: true
    mock:
      status: 200
      body: { ok: true }
  - name: get_item
    description: Fetch one item by id
    read_only: true
    method: GET
    url: "{{account.api_base}}/items/{{args.id}}"
    args_schema:
      type: object
      properties:
        id:
          type: string
      required: [id]
  - name: create_item
    description: Create an item
    method: POST
    url: "{{account.api_base}}/items"
    body: "{{args}}"
    args_schema:
      type: object
      properties:
        title:
          type: string
      required: [title]
```

- [ ] Create `connectors/example/PUBLIC.md`:

```markdown
---
display_name: Example
summary: A minimal placeholder connector for distribution smoke tests; replaced by the official connectors in a later slice.
---
# Example

A minimal embedded connector used to exercise install, list, and test before the official connectors land.
```

- [ ] Create `connectors/example/tests.yaml`:

```yaml
cases:
  - function: ping
    account: {}
    args: {}
    expect:
      status: 200
      body: { ok: true }
  - function: get_item
    account: { api_base: https://api.example.com }
    args: { id: "42" }
    expect:
      method: GET
      url: https://api.example.com/items/42
  - function: create_item
    account: { api_base: https://api.example.com }
    args: { title: Hello }
    expect:
      method: POST
      url: https://api.example.com/items
      body_contains: { title: Hello }
```

- [ ] Add rust-embed to `crates/apb-core/Cargo.toml` under `[dependencies]` (matching the version `apb-server` already pins):

```toml
rust-embed = "8.12"
```

- [ ] Write the failing test module at the bottom of `crates/apb-core/src/connector/official.rs`:

```rust
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
```

- [ ] Run and watch it fail: `cargo test -p apb-core --lib connector::official` -> `cannot find function list`.

- [ ] Write the module body above the tests:

```rust
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
```

- [ ] `tempfile` is already an `apb-core` dev-dependency (used by `store.rs` tests); no Cargo change needed for the tests.

- [ ] Add the module to `crates/apb-core/src/connector/mod.rs`: insert `pub mod official;` after `pub mod def;`. Keep call sites fully-qualified (`official::list()`) because `official` exports `list`/`get` that would collide with `store::list` under a glob import.

- [ ] Run to pass: `cargo test -p apb-core --lib connector::official`. Also confirm nothing else broke: `cargo build -p apb-core`.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "connector: embed connectors/ folder via rust-embed with a seed example"` plus the `Co-Authored-By` trailer.

---

### Task 3: Extract `render_http` in apb-engine connector_call

Refactor the URL/query/body render block out of `build_prepared` into a shared `render_http` that the dry-run terminal, the live HTTP send path, and the new offline runner all call. Behavior-preserving.

**Files:**
- Modify: `crates/apb-engine/src/connector_call.rs`

**Interfaces:**
- Produces `pub struct RenderedRequest { pub method: String, pub pre_auth_url: String, pub rendered_body: Option<serde_json::Value> }`
- Produces `pub(crate) fn render_http(function: &FunctionSpec, account_fields: &BTreeMap<String,String>, args: &Value, secrets: &BTreeMap<String,String>) -> Result<RenderedRequest, CallError>`
- Consumes existing private helpers `encode_args_for_url`, `render_query`, `assemble_url`, `validate_url` (unchanged)

Steps:

- [ ] Add a failing unit test to the existing `#[cfg(test)] mod tests` in `connector_call.rs`:

```rust
#[test]
fn render_http_builds_method_url_and_body_offline() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: create\n    description: d\n    method: POST\n    url: \"{{account.api_base}}/items/{{args.id}}\"\n    body: \"{{args}}\"\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("create").unwrap();
    let mut account = BTreeMap::new();
    account.insert("api_base".to_string(), "https://api.example.com".to_string());
    let args = json!({ "id": "4 2", "title": "Hi" });
    let secrets = BTreeMap::new();
    let r = render_http(f, &account, &args, &secrets).unwrap();
    assert_eq!(r.method, "POST");
    // args are percent-encoded into the URL path (space -> %20).
    assert_eq!(r.pre_auth_url, "https://api.example.com/items/4%2042");
    assert_eq!(r.rendered_body, Some(json!({ "id": "4 2", "title": "Hi" })));
}
```

- [ ] Run and watch it fail: `cargo test -p apb-engine --lib connector_call::tests::render_http_builds_method_url_and_body_offline` -> `cannot find function render_http`.

- [ ] Add the new type and function near `build_prepared` in `connector_call.rs`:

```rust
/// The rendered HTTP shape of a function - method, the pre-auth URL (function
/// query included, auth NOT), and the rendered body - produced without touching
/// the network. Shared by the dry-run terminal, the live send path, and the
/// offline `tests.yaml` runner (`connector_test`).
pub struct RenderedRequest {
    pub method: String,
    pub pre_auth_url: String,
    pub rendered_body: Option<Value>,
}

/// Renders a function's method, pre-auth URL, and body against the given
/// non-secret account fields, args, and secrets. `secrets` is unused by the
/// URL/query/body render (the secret-placement policy forbids `{{secret.*}}`
/// outside `auth`), but is threaded through so the render context stays uniform
/// with the live send path and so a later smtp/headers render can share it.
/// Args substituted into the URL are percent-encoded; account prefixes stay
/// raw (spec 6.1), matching the previous inline logic exactly.
pub(crate) fn render_http(
    function: &FunctionSpec,
    account_fields: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, String>,
) -> Result<RenderedRequest, CallError> {
    let ctx = RenderCtx {
        account: account_fields,
        args,
        secrets,
    };
    let method = function.method.clone().unwrap_or_else(|| "GET".to_string());
    let url_args = encode_args_for_url(args);
    let url_ctx = RenderCtx {
        account: account_fields,
        args: &url_args,
        secrets,
    };
    let base = render_raw(function.url.as_deref().unwrap_or(""), &url_ctx)
        .map_err(|e| CallError::new(CallErrorCode::Config, format!("URL render failed: {e}")))?;
    let query = render_query(function, &ctx)?;
    let pre_auth_url = assemble_url(&base, &query);
    validate_url(&pre_auth_url)?;
    let rendered_body = match &function.body {
        Some(b) => Some(render_body(b, &ctx).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("body render failed: {e}"))
        })?),
        None => None,
    };
    Ok(RenderedRequest {
        method,
        pre_auth_url,
        rendered_body,
    })
}
```

- [ ] Replace the inline render block in `build_prepared` with a call to `render_http`, keeping the surrounding secret resolution and the dry-run / HttpCall branching:

```rust
    // 7. Secrets: resolve every env-ref field. Skipped entirely for a dry-run.
    let account_fields = non_secret_fields(maccount);
    let (secrets, redactions) = if dry_run {
        (BTreeMap::new(), Vec::new())
    } else {
        resolve_secrets(root, maccount)?
    };

    // 8. Render URL, query, body (shared with the offline test runner).
    let rendered = render_http(function, &account_fields, args, &secrets)?;

    if dry_run {
        return Ok(Prepared::DryRun(json!({
            "ok": true,
            "dry_run": true,
            "method": rendered.method,
            "url": rendered.pre_auth_url,
            "body": rendered.rendered_body.clone().unwrap_or(Value::Null),
        })));
    }

    Ok(Prepared::Call(Box::new(PreparedCall::Http(HttpCall {
        account: account_name,
        method: rendered.method,
        pre_auth_url: rendered.pre_auth_url,
        rendered_body: rendered.rendered_body,
        auth: auth.cloned(),
        secrets,
        account_fields,
        timeout_sec: function.timeout_sec,
        redactions,
    }))))
```

- [ ] Run the new test and the full connector_call suite to confirm no behavior change: `cargo test -p apb-engine --lib connector_call`.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "connector_call: extract render_http shared by dry-run and the test runner"` plus the `Co-Authored-By` trailer.

---

### Task 4: Offline contract-test runner in apb-engine (`connector_test`)

The runner: for each case, look up the function, render it offline with secrets stubbed to `"test-secret"`, and check the rendered request against the expectation. HTTP (method / url / body_contains subset) and mock (status / body) arms land now; smtp and headers arms are deferred to Tasks 8 and 9.

**Files:**
- Create: `crates/apb-engine/src/connector_test.rs`
- Modify: `crates/apb-engine/src/lib.rs` (declare the module)

**Interfaces:**
- Consumes `apb_core::connector::def::ConnectorDoc`, `apb_core::connector::contract::{TestsDoc, ExpectKind}`, `crate::connector_call::render_http`
- Produces `pub fn run_tests(doc: &ConnectorDoc, tests: &TestsDoc) -> TestReport`
- Produces `pub struct TestReport { pub results: Vec<CaseResult> }`, `TestReport::all_passed(&self) -> bool`
- Produces `pub struct CaseResult { pub function: String, pub passed: bool, pub detail: String }`

Steps:

- [ ] Write the failing test module at the bottom of `crates/apb-engine/src/connector_test.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::connector::contract::TestsDoc;
    use apb_core::connector::def::ConnectorDoc;

    const YAML: &str = r#"
name: example
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - name: api_base
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: ping
    description: d
    read_only: true
    mock: { status: 200, body: { ok: true } }
  - name: get_item
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/items/{{args.id}}"
    args_schema: { type: object, properties: { id: { type: string } }, required: [id] }
  - name: create_item
    description: d
    method: POST
    url: "{{account.api_base}}/items"
    body: "{{args}}"
    args_schema: { type: object }
"#;

    fn doc() -> ConnectorDoc {
        ConnectorDoc::from_yaml(YAML, "example").unwrap()
    }

    #[test]
    fn all_cases_pass_for_matching_expectations() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: ping\n    expect: { status: 200, body: { ok: true } }\n  - function: get_item\n    account: { api_base: https://api.example.com }\n    args: { id: \"42\" }\n    expect: { method: GET, url: https://api.example.com/items/42 }\n  - function: create_item\n    account: { api_base: https://api.example.com }\n    args: { title: Hi }\n    expect: { method: POST, url: https://api.example.com/items, body_contains: { title: Hi } }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.all_passed(), "{:?}", failing(&report));
        assert_eq!(report.results.len(), 3);
    }

    #[test]
    fn url_mismatch_fails_that_case() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: get_item\n    account: { api_base: https://api.example.com }\n    args: { id: \"42\" }\n    expect: { method: GET, url: https://api.example.com/items/99 }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(!report.all_passed());
        assert!(report.results[0].detail.contains("url mismatch"));
    }

    #[test]
    fn body_contains_is_a_subset_match() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: create_item\n    account: { api_base: https://api.example.com }\n    args: { title: Hi, extra: 1 }\n    expect: { method: POST, url: https://api.example.com/items, body_contains: { title: Hi } }\n",
        )
        .unwrap();
        assert!(run_tests(&doc(), &tests).all_passed());
    }

    #[test]
    fn unknown_function_fails() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: nope\n    expect: { status: 200, body: {} }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.results[0].detail.contains("not defined"));
    }

    #[test]
    fn kind_mismatch_between_function_and_expectation_fails() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: ping\n    expect: { method: GET, url: https://x }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.results[0].detail.contains("mock"));
    }

    fn failing(report: &TestReport) -> Vec<String> {
        report
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| format!("{}: {}", r.function, r.detail))
            .collect()
    }
}
```

- [ ] Run and watch it fail: `cargo test -p apb-engine --lib connector_test` -> `cannot find function run_tests`.

- [ ] Write the module body above the tests:

```rust
//! Offline connector contract-test runner (spec 2026-07-19-official-connectors,
//! section 4.6). Runs every `tests.yaml` case of a connector through the same
//! render path a `--dry-run` call uses (`connector_call::render_http`), with
//! secrets stubbed to a fixed value and no network, then checks the rendered
//! request against the case's `expect` block. Exit-code semantics live in the
//! CLI (`apb connector test`); this module returns a structured report.

use std::collections::BTreeMap;

use apb_core::connector::contract::{Envelope, ExpectKind, TestCase, TestsDoc};
use apb_core::connector::def::{ConnectorDoc, FunctionSpec};
use serde_json::Value;

use crate::connector_call::render_http;

/// The fixed value every secret account field resolves to in the offline
/// runner. A real secret is never read (spec 4.6).
const SECRET_STUB: &str = "test-secret";

/// The result of one contract-test case.
pub struct CaseResult {
    pub function: String,
    pub passed: bool,
    /// Empty when the case passed; the failure reason otherwise.
    pub detail: String,
}

/// The result of running a connector's whole `tests.yaml`.
pub struct TestReport {
    pub results: Vec<CaseResult>,
}

impl TestReport {
    /// True when every case passed (an empty case list passes vacuously; the
    /// per-function coverage requirement is the slice-5 CI gate's job).
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }
}

/// Runs every case in `tests` against `doc`.
pub fn run_tests(doc: &ConnectorDoc, tests: &TestsDoc) -> TestReport {
    let results = tests.cases.iter().map(|case| run_case(doc, case)).collect();
    TestReport { results }
}

fn run_case(doc: &ConnectorDoc, case: &TestCase) -> CaseResult {
    let detail = evaluate(doc, case).err().unwrap_or_default();
    CaseResult {
        function: case.function.clone(),
        passed: detail.is_empty(),
        detail,
    }
}

fn evaluate(doc: &ConnectorDoc, case: &TestCase) -> Result<(), String> {
    let function = doc.function(&case.function).ok_or_else(|| {
        format!(
            "function `{}` is not defined by the connector",
            case.function
        )
    })?;
    let kind = case.expect.resolve()?;
    let args = if case.args.is_null() {
        Value::Object(Default::default())
    } else {
        case.args.clone()
    };
    match kind {
        ExpectKind::Mock { status, body } => eval_mock(function, status, body),
        ExpectKind::Http {
            method,
            url,
            headers,
            body_contains,
        } => eval_http(doc, function, &case.account, &args, method, url, headers, body_contains),
        ExpectKind::Smtp(envelope) => eval_smtp(envelope),
    }
}

fn eval_mock(function: &FunctionSpec, status: u16, body: &Value) -> Result<(), String> {
    let mock = function.mock.as_ref().ok_or_else(|| {
        format!(
            "function `{}` is not a mock but the case expects a mock response",
            function.name
        )
    })?;
    if mock.status != status {
        return Err(format!(
            "status mismatch: expected {status}, rendered {}",
            mock.status
        ));
    }
    if &mock.body != body {
        return Err(format!(
            "body mismatch: expected {body}, rendered {}",
            mock.body
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn eval_http(
    doc: &ConnectorDoc,
    function: &FunctionSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    method: &str,
    url: &str,
    headers: Option<&BTreeMap<String, String>>,
    body_contains: Option<&Value>,
) -> Result<(), String> {
    if function.mock.is_some() {
        return Err(format!(
            "function `{}` is a mock but the case expects an HTTP request",
            function.name
        ));
    }
    // Header expectations need the per-function `headers` map and default
    // User-Agent from connectors slice 1; until that lands, asserting on them
    // is an explicit, honest failure rather than a silent pass (Task 8).
    if headers.is_some() {
        return Err(
            "header expectations require the per-function `headers` support from connectors slice 1"
                .to_string(),
        );
    }
    let secrets: BTreeMap<String, String> = doc
        .secret_fields()
        .into_iter()
        .map(|f| (f, SECRET_STUB.to_string()))
        .collect();
    let rendered = render_http(function, account, args, &secrets)
        .map_err(|e| format!("render failed: {}", e.message))?;
    if !rendered.method.eq_ignore_ascii_case(method) {
        return Err(format!(
            "method mismatch: expected {method}, rendered {}",
            rendered.method
        ));
    }
    if rendered.pre_auth_url != url {
        return Err(format!(
            "url mismatch: expected `{url}`, rendered `{}`",
            rendered.pre_auth_url
        ));
    }
    if let Some(subset) = body_contains {
        let body = rendered.rendered_body.unwrap_or(Value::Null);
        if !json_subset(subset, &body) {
            return Err(format!(
                "body_contains mismatch: `{subset}` is not a subset of `{body}`"
            ));
        }
    }
    Ok(())
}

/// The smtp expectation arm is delivered by connectors slice 3 (the `smtp`
/// function kind and its offline envelope render). Until that lands, an smtp
/// case is an explicit failure. See Task 9.
fn eval_smtp(_envelope: &Envelope) -> Result<(), String> {
    Err("smtp expectations require the smtp function kind from connectors slice 3".to_string())
}

/// Subset match: every key of `expected` (recursively for nested objects) must
/// be present in `actual` with a subset-matching value; non-object values must
/// be exactly equal. Keeps cases robust to services (and manifests) adding
/// fields (spec 4.6).
fn json_subset(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|av| json_subset(v, av))),
        _ => expected == actual,
    }
}
```

- [ ] Add `pub mod connector_test;` to `crates/apb-engine/src/lib.rs` (after `pub mod connector_run;`).

- [ ] Run to pass: `cargo test -p apb-engine --lib connector_test`.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "connector_test: offline tests.yaml runner (http + mock arms)"` plus the `Co-Authored-By` trailer.

---

### Task 5: `apb connector install` (embedded + --from-dir + trust seeding)

CLI command that materializes an embedded connector into `<config_dir>/connectors/<name>/` and records connector trust in the same action; refuses on a differing target unless `--force`; same-digest reinstall is a no-op. `--from-dir <path>` installs any folder (validated) without recording trust.

**Files:**
- Modify: `crates/apb-cli/src/connector.rs`
- Modify: `crates/apb-cli/tests/suite/connector_cli.rs`

**Interfaces:**
- Consumes `apb_core::connector::official::{get}`, `OfficialConnector::write_to`, `apb_core::content::{tree_digest, TreeLimits}`, `apb_core::trust::{TrustStore, Kind, OriginKind}`, `apb_core::connector::store::connectors_dir`, `apb_core::connector::def::ConnectorDoc`
- Produces `ConnectorAction::Install { name: Option<String>, from_dir: Option<PathBuf>, force: bool }` and its handler

Steps:

- [ ] Add the failing integration tests to `connector_cli.rs`:

```rust
// --- install --------------------------------------------------------------

#[test]
fn install_embedded_example_records_trust_and_lists_approved() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    let out = apb_ok(dir.path(), &["connector", "install", "example"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("installed connector `example`"),
        "install should confirm: {stdout}"
    );
    let cfg = dir.path().join("cfg/connectors/example");
    assert!(cfg.join("connector.yaml").is_file());
    assert!(cfg.join("tests.yaml").is_file());

    // Embedded install seeds trust: the connector lists as approved.
    let list = apb_ok(dir.path(), &["connector", "list"]);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("example") && list_out.contains("approved"),
        "installed connector should list as approved: {list_out}"
    );
}

#[test]
fn install_same_digest_is_a_noop_and_differing_refuses_without_force() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "install", "example"]);

    // Re-install: same digest, a no-op that still succeeds.
    let again = apb_ok(dir.path(), &["connector", "install", "example"]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("already installed"));

    // Mutate the installed folder so it differs from the embedded version.
    let manifest = dir.path().join("cfg/connectors/example/connector.yaml");
    let mut body = fs::read_to_string(&manifest).unwrap();
    body.push_str("# local edit\n");
    fs::write(&manifest, body).unwrap();

    let refused = playbook(dir.path(), &["connector", "install", "example"]);
    assert!(!refused.status.success(), "differing target must refuse");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--force"),
        "refusal should point at --force"
    );

    // --force overwrites and restores the embedded content.
    apb_ok(dir.path(), &["connector", "install", "example", "--force"]);
    let restored = fs::read_to_string(&manifest).unwrap();
    assert!(!restored.contains("# local edit"), "force should overwrite");
}

#[test]
fn install_from_dir_installs_without_recording_trust() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    // A local connector folder named `widget` (basename is the connector name).
    let src = dir.path().join("src/widget");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("connector.yaml"),
        "name: widget\nversion: 0.1.0\nfunctions:\n  - name: ping\n    description: d\n    mock: { status: 200, body: { ok: true } }\n",
    )
    .unwrap();

    let out = apb_ok(
        dir.path(),
        &["connector", "install", "--from-dir", src.to_str().unwrap()],
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("installed connector `widget`"));
    assert!(dir.path().join("cfg/connectors/widget/connector.yaml").is_file());

    // No trust recorded: the connector lists as unapproved.
    let list = apb_ok(dir.path(), &["connector", "list"]);
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("unapproved"),
        "--from-dir must not seed trust"
    );
}
```

- [ ] Run and watch it fail: `cargo test -p apb-cli --test suite connector_cli::install_embedded_example_records_trust_and_lists_approved` -> the binary rejects the unknown `install` subcommand.

- [ ] Extend imports at the top of `crates/apb-cli/src/connector.rs`:

```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};
```

(replace the existing `use std::path::Path;`).

- [ ] Add the `Install` variant to `ConnectorAction`:

```rust
    /// Install an embedded official connector into the global store and record
    /// its trust in the same action; or install any folder on disk with
    /// --from-dir (validated, but no trust recorded - the normal approve flow
    /// applies). Refuses a differing existing target unless --force; a
    /// same-digest reinstall is a no-op.
    Install {
        /// Name of the embedded connector to install; omit only with --from-dir
        name: Option<String>,
        /// Install from this folder instead of the embedded set (no trust)
        #[arg(long)]
        from_dir: Option<PathBuf>,
        /// Overwrite an existing, differing target
        #[arg(long)]
        force: bool,
    },
```

- [ ] Add the dispatch arm in `connector_cmd`:

```rust
        ConnectorAction::Install {
            name,
            from_dir,
            force,
        } => install_cmd(name, from_dir, force),
```

- [ ] Add the handler functions:

```rust
// --- install --------------------------------------------------------------

fn install_cmd(name: Option<String>, from_dir: Option<PathBuf>, force: bool) -> ExitCode {
    match from_dir {
        Some(dir) => install_from_dir(&dir, force),
        None => match name {
            Some(n) => install_embedded(&n, force),
            None => {
                eprintln!("connector error: provide a connector name or --from-dir <path>");
                ExitCode::from(2)
            }
        },
    }
}

/// Records connector trust for a freshly installed embedded connector. Origin
/// is `Bundled`: the bytes came from the trusted binary the user already runs.
fn record_connector_trust(name: &str, digest: &str) {
    let mut trust = TrustStore::load();
    if let Err(e) = trust.approve_kind(digest, name, Kind::Connector, OriginKind::Bundled) {
        eprintln!("connector warning: installed `{name}` but could not record trust: {e}");
    }
}

fn install_embedded(name: &str, force: bool) -> ExitCode {
    if let Err(e) = apb_core::profile::validate_profile_name(name) {
        eprintln!("connector error: invalid connector name `{name}`: {e}");
        return ExitCode::from(2);
    }
    let Some(official) = apb_core::connector::official::get(name) else {
        eprintln!("connector error: `{name}` is not an embedded official connector");
        return ExitCode::from(2);
    };
    let Some(base) = store::connectors_dir() else {
        eprintln!("connector error: no config directory available");
        return ExitCode::from(2);
    };
    let target = base.join(name);
    let staging = base.join(format!(".{name}.install-tmp"));
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = official.write_to(&staging) {
        eprintln!("connector error: cannot stage `{name}`: {e}");
        let _ = std::fs::remove_dir_all(&staging);
        return ExitCode::from(2);
    }
    let limits = apb_core::content::TreeLimits::default();
    let new_digest = match apb_core::content::tree_digest(&staging, &limits) {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            eprintln!("connector error: cannot digest `{name}`: {e}");
            return ExitCode::from(2);
        }
    };

    if target.exists() {
        let current = apb_core::content::tree_digest(&target, &limits).ok();
        if current.as_deref() == Some(new_digest.as_str()) {
            let _ = std::fs::remove_dir_all(&staging);
            record_connector_trust(name, &new_digest);
            println!(
                "connector `{name}` already installed at version {} (no changes)",
                official.version
            );
            return ExitCode::SUCCESS;
        }
        if !force {
            let _ = std::fs::remove_dir_all(&staging);
            eprintln!(
                "connector error: `{}` already exists and differs from the embedded version; pass --force to overwrite",
                target.display()
            );
            return ExitCode::from(2);
        }
        if let Err(e) = std::fs::remove_dir_all(&target) {
            let _ = std::fs::remove_dir_all(&staging);
            eprintln!("connector error: cannot replace {}: {e}", target.display());
            return ExitCode::from(2);
        }
    }
    if let Err(e) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("connector error: cannot install into {}: {e}", target.display());
        return ExitCode::from(2);
    }
    record_connector_trust(name, &new_digest);
    println!(
        "installed connector `{name}` version {} and recorded its trust",
        official.version
    );
    ExitCode::SUCCESS
}

fn install_from_dir(src: &Path, _force: bool) -> ExitCode {
    let Some(name) = src.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
        eprintln!("connector error: cannot derive a connector name from {}", src.display());
        return ExitCode::from(2);
    };
    if let Err(e) = apb_core::profile::validate_profile_name(&name) {
        eprintln!("connector error: folder name `{name}` is not a valid connector name: {e}");
        return ExitCode::from(2);
    }
    // Validate: the manifest must parse and match the folder name.
    let yaml = match std::fs::read_to_string(src.join("connector.yaml")) {
        Ok(y) => y,
        Err(e) => {
            eprintln!("connector error: cannot read {}/connector.yaml: {e}", src.display());
            return ExitCode::from(2);
        }
    };
    if let Err(e) = ConnectorDoc::from_yaml(&yaml, &name) {
        eprintln!("connector error: {} does not validate: {e}", src.display());
        return ExitCode::from(2);
    }
    let Some(base) = store::connectors_dir() else {
        eprintln!("connector error: no config directory available");
        return ExitCode::from(2);
    };
    let target = base.join(&name);
    let staging = base.join(format!(".{name}.install-tmp"));
    let _ = std::fs::remove_dir_all(&staging);
    // TOCTOU-safe copy + digest of the source folder into staging.
    let limits = apb_core::content::TreeLimits::default();
    if let Err(e) = apb_core::content::snapshot_tree(src, &staging, &limits) {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("connector error: cannot copy {}: {e}", src.display());
        return ExitCode::from(2);
    }
    // --from-dir is an explicit local dev action and records no trust, so it
    // overwrites freely (the run/probe gate still catches an unapproved digest).
    if target.exists()
        && let Err(e) = std::fs::remove_dir_all(&target)
    {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("connector error: cannot replace {}: {e}", target.display());
        return ExitCode::from(2);
    }
    if let Err(e) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("connector error: cannot install into {}: {e}", target.display());
        return ExitCode::from(2);
    }
    println!("installed connector `{name}` from {} (trust not recorded; approve it before use)", src.display());
    ExitCode::SUCCESS
}
```

- [ ] Run to pass: `cargo test -p apb-cli --test suite connector_cli::install`.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "cli: apb connector install (embedded trust seeding + --from-dir)"` plus the `Co-Authored-By` trailer.

---

### Task 6: `apb connector list` available section and version-drift note

Add an "available" section listing embedded-but-not-installed connectors with versions, and a note when an installed connector's version differs from the embedded one.

**Files:**
- Modify: `crates/apb-cli/src/connector.rs` (`list_cmd`)
- Modify: `crates/apb-cli/tests/suite/connector_cli.rs`

**Interfaces:**
- Consumes `apb_core::connector::official::list`

Steps:

- [ ] Add the failing integration test:

```rust
#[test]
fn list_shows_embedded_available_section_before_install_and_hides_after() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    // Before install: `example` appears under the available section.
    let before = apb_ok(dir.path(), &["connector", "list"]);
    let before_out = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_out.contains("AVAILABLE") && before_out.contains("example"),
        "available section should list the embedded example: {before_out}"
    );

    apb_ok(dir.path(), &["connector", "install", "example"]);

    // After install: `example` is an installed row, no longer under available.
    let after = apb_ok(dir.path(), &["connector", "list"]);
    let after_out = String::from_utf8_lossy(&after.stdout);
    let available_block = after_out.split("AVAILABLE").nth(1).unwrap_or("");
    assert!(
        !available_block.contains("example"),
        "installed connector must not appear under available: {after_out}"
    );
}
```

- [ ] Run and watch it fail: no `AVAILABLE` text yet.

- [ ] Rewrite `list_cmd` to always compute the available section, keeping the installed table intact:

```rust
fn list_cmd(root: &Path) -> ExitCode {
    let summaries = store::list();
    let official = apb_core::connector::official::list();

    if summaries.is_empty() {
        println!("no connectors installed (see `apb connector install <name>` or `apb connector init <name>`)");
    } else {
        let trust = TrustStore::load();
        let approved_connector_ids = trust.approved_record_ids(Kind::Connector);
        let mut rows: Vec<[String; 4]> = vec![[
            "NAME".to_string(),
            "VERSION".to_string(),
            "TRUST".to_string(),
            "ACCOUNTS".to_string(),
        ]];
        for s in &summaries {
            let trust_state = match store::load(&s.name) {
                Ok(loaded) => {
                    if trust.is_approved(&loaded.digest) {
                        "approved"
                    } else if approved_connector_ids.iter().any(|id| id == &s.name) {
                        "changed"
                    } else {
                        "unapproved"
                    }
                }
                Err(_) => "invalid",
            };
            let accounts_count = config::load_merged(root, &s.name)
                .map(|a| a.len())
                .unwrap_or(0);
            rows.push([
                s.name.clone(),
                s.version.clone(),
                trust_state.to_string(),
                accounts_count.to_string(),
            ]);
        }
        print_table(&rows);

        // Version-drift notes: an installed connector whose embedded version
        // differs (a binary upgrade shipped a newer manifest).
        for s in &summaries {
            if let Some(o) = official.iter().find(|o| o.name == s.name)
                && o.version != s.version
            {
                println!(
                    "note: `{}` installed {}, embedded {} (reinstall with --force to upgrade)",
                    s.name, s.version, o.version
                );
            }
        }
    }

    let installed: HashSet<&str> = summaries.iter().map(|s| s.name.as_str()).collect();
    let available: Vec<&apb_core::connector::official::OfficialConnector> = official
        .iter()
        .filter(|o| !installed.contains(o.name.as_str()))
        .collect();
    if !available.is_empty() {
        println!();
        println!("AVAILABLE (embedded, not installed):");
        for o in &available {
            println!("  {}  {}", o.name, o.version);
        }
        println!("install one with `apb connector install <name>`");
    }

    ExitCode::SUCCESS
}
```

- [ ] Confirm the pre-existing `list_shows_scaffold_as_unapproved_with_zero_accounts` still passes (the installed table format is unchanged). Run: `cargo test -p apb-cli --test suite connector_cli::list`.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "cli: apb connector list shows the embedded available section and version drift"` plus the `Co-Authored-By` trailer.

---

### Task 7: `apb connector test` command

Run the offline contract tests of an installed connector, an embedded one, or a folder (`--dir`). Print per-case pass/fail; exit non-zero on any failure.

**Files:**
- Modify: `crates/apb-cli/src/connector.rs`
- Modify: `crates/apb-cli/tests/suite/connector_cli.rs`

**Interfaces:**
- Consumes `apb_engine::connector_test::run_tests`, `apb_core::connector::contract::TestsDoc`, `apb_core::connector::official::get`, `apb_core::connector::store::load`, `apb_core::connector::def::ConnectorDoc`
- Produces `ConnectorAction::Test { name: Option<String>, dir: Option<PathBuf> }` and its handler

Steps:

- [ ] Add the failing integration tests:

```rust
// --- test -----------------------------------------------------------------

#[test]
fn test_runs_embedded_example_cases_and_passes() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    // Embedded connector, not installed: `test` resolves it from the binary.
    let out = apb_ok(dir.path(), &["connector", "test", "example"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[pass] ping"), "ping case should pass: {stdout}");
    assert!(stdout.contains("[pass] get_item"), "get_item case should pass: {stdout}");
    assert!(stdout.contains("[pass] create_item"), "create_item should pass: {stdout}");
}

#[test]
fn test_dir_with_a_failing_case_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    let src = dir.path().join("src/widget");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("connector.yaml"),
        "name: widget\nversion: 0.1.0\nfunctions:\n  - name: get_item\n    description: d\n    read_only: true\n    method: GET\n    url: \"{{account.api_base}}/items/{{args.id}}\"\n    args_schema: { type: object, properties: { id: { type: string } }, required: [id] }\n",
    )
    .unwrap();
    fs::write(
        src.join("tests.yaml"),
        "cases:\n  - function: get_item\n    account: { api_base: https://api.example.com }\n    args: { id: \"42\" }\n    expect: { method: GET, url: https://api.example.com/items/WRONG }\n",
    )
    .unwrap();

    let out = playbook(dir.path(), &["connector", "test", "--dir", src.to_str().unwrap()]);
    assert!(!out.status.success(), "a failing case must exit non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[fail] get_item"), "should report the failing case: {stdout}");
}
```

- [ ] Run and watch it fail: unknown `test` subcommand.

- [ ] Add the `Test` variant to `ConnectorAction`:

```rust
    /// Run a connector's offline contract tests (tests.yaml). Resolves an
    /// installed connector, an embedded one, or a folder on disk (--dir).
    /// Fully offline; exits non-zero on any failing case.
    Test {
        /// Installed or embedded connector name; omit only with --dir
        name: Option<String>,
        /// Test the connector folder at this path instead
        #[arg(long)]
        dir: Option<PathBuf>,
    },
```

- [ ] Add the dispatch arm in `connector_cmd`:

```rust
        ConnectorAction::Test { name, dir } => test_cmd(name, dir),
```

- [ ] Add the handler and resolver:

```rust
// --- test -----------------------------------------------------------------

fn test_cmd(name: Option<String>, dir: Option<PathBuf>) -> ExitCode {
    let (doc, tests) = match load_test_target(name.as_deref(), dir.as_deref()) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("connector error: {msg}");
            return ExitCode::from(2);
        }
    };
    let report = apb_engine::connector_test::run_tests(&doc, &tests);
    for r in &report.results {
        if r.passed {
            println!("[pass] {}", r.function);
        } else {
            println!("[fail] {}: {}", r.function, r.detail);
        }
    }
    if report.all_passed() {
        println!("{} case(s) passed", report.results.len());
        ExitCode::SUCCESS
    } else {
        let failed = report.results.iter().filter(|r| !r.passed).count();
        eprintln!("connector test: {failed} case(s) failed");
        ExitCode::from(1)
    }
}

/// Resolves the `(ConnectorDoc, TestsDoc)` pair for `apb connector test` from,
/// in precedence order: a folder (--dir), an installed connector, then an
/// embedded one.
fn load_test_target(
    name: Option<&str>,
    dir: Option<&Path>,
) -> Result<(ConnectorDoc, apb_core::connector::contract::TestsDoc), String> {
    use apb_core::connector::contract::TestsDoc;

    if let Some(dir) = dir {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("cannot derive a connector name from {}", dir.display()))?;
        let yaml = std::fs::read_to_string(dir.join("connector.yaml"))
            .map_err(|e| format!("cannot read {}/connector.yaml: {e}", dir.display()))?;
        let doc = ConnectorDoc::from_yaml(&yaml, dir_name).map_err(|e| e.to_string())?;
        let tests_raw = std::fs::read_to_string(dir.join("tests.yaml"))
            .map_err(|e| format!("cannot read {}/tests.yaml: {e}", dir.display()))?;
        let tests = TestsDoc::from_yaml(&tests_raw).map_err(|e| e.to_string())?;
        return Ok((doc, tests));
    }

    let name = name.ok_or_else(|| "provide a connector name or --dir <path>".to_string())?;

    // Installed connector first.
    if let Ok(loaded) = store::load(name) {
        let tests_raw = std::fs::read_to_string(loaded.dir.join("tests.yaml"))
            .map_err(|e| format!("connector `{name}` has no readable tests.yaml: {e}"))?;
        let tests = TestsDoc::from_yaml(&tests_raw).map_err(|e| e.to_string())?;
        return Ok((loaded.doc, tests));
    }

    // Otherwise an embedded connector.
    let official = apb_core::connector::official::get(name)
        .ok_or_else(|| format!("`{name}` is neither installed nor an embedded connector"))?;
    let yaml = official
        .files
        .get("connector.yaml")
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| format!("embedded `{name}` has no readable connector.yaml"))?;
    let doc = ConnectorDoc::from_yaml(yaml, name).map_err(|e| e.to_string())?;
    let tests_raw = official
        .files
        .get("tests.yaml")
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| format!("embedded `{name}` has no tests.yaml"))?;
    let tests = TestsDoc::from_yaml(tests_raw).map_err(|e| e.to_string())?;
    Ok((doc, tests))
}
```

- [ ] Run to pass: both new CLI tests green.

- [ ] Run the per-task verification block. Commit:
  `git commit --signoff -am "cli: apb connector test runs offline tests.yaml for installed, embedded, or --dir"` plus the `Co-Authored-By` trailer.

At this point slice 4 is independently complete and landable: install (with trust seeding), from-dir sideload, list available section, tests.yaml schema, offline runner (http + mock), and `apb connector test`. Tasks 8 and 9 are sequenced last because they depend on parallel slices.

---

### Task 8 (DEFERRED - depends on slice 1): headers expectation matching

Slice 1 adds `FunctionSpec.headers` (a template map) and the default `User-Agent`, and renders them in the executor. Only then can a `tests.yaml` `headers` expectation be checked. This task must land after slice 1 has merged; until then the runner intentionally fails a `headers` expectation (Task 4's `eval_http` guard).

**Files:**
- Modify: `crates/apb-engine/src/connector_call.rs` (extend `RenderedRequest` with `headers` and populate it in `render_http`)
- Modify: `crates/apb-engine/src/connector_test.rs` (`eval_http` headers subset-match, remove the guard)

**Interfaces:**
- Extends `RenderedRequest` with `pub headers: BTreeMap<String, String>` populated from `function.headers` (slice 1) plus the default `User-Agent`
- Consumes `ExpectKind::Http { headers, .. }`

Steps (author only after slice 1 lands, so `FunctionSpec::headers` exists):

- [ ] Add a failing test to `connector_test.rs` tests using a connector whose function declares `headers` (slice-1 schema), asserting a `headers` subset match passes and a mismatch fails.
- [ ] Extend `RenderedRequest` with `headers: BTreeMap<String,String>` and populate it in `render_http` by rendering `function.headers` (account + args namespaces only) plus the default `User-Agent: apb/<version>` unless overridden, matching the slice-1 executor.
- [ ] In `eval_http`, replace the `if headers.is_some()` guard with a subset match: every expected header key must be present in `rendered.headers` with an exactly-equal value; report `header mismatch` naming the key on failure.
- [ ] Run to pass, run the verification block, commit `connector_test: match tests.yaml headers expectations (post slice 1)`.

Explicitly sequenced last: do not start until slice 1 is merged, because `FunctionSpec` has no `headers` field before then and the code will not compile.

---

### Task 9 (DEFERRED - depends on slice 3): smtp envelope expectation matching

Slice 3 adds the `smtp` function kind (`SmtpSpec { connection, message, verify }`) and its offline envelope render. Only then can a `tests.yaml` `envelope` expectation be checked. This task replaces Task 4's `eval_smtp` stub with a real match against a rendered envelope. It must land after slice 3 has merged.

**Files:**
- Modify: `crates/apb-engine/src/connector_test.rs` (`eval_smtp`)
- Consumes: the slice-3 offline smtp render entry point: `connector_smtp::build` with `dry_run: true` returns `SmtpBuild::DryRun(json)` whose `envelope` object carries from/to/subject; secrets stub to `SECRET_STUB`.

**Interfaces:**
- Consumes `apb_core::connector::def::SmtpSpec` (slice 3) and `ExpectKind::Smtp(&Envelope)`

Steps (author only after slice 3 lands):

- [ ] Add a failing test to `connector_test.rs` using an smtp connector (slice-3 schema) with a `send_email` function and an `envelope` expectation (`from`, `to`, `subject`), asserting a match passes and a differing subject fails.
- [ ] Rewrite `eval_smtp` to: confirm the function is an smtp kind; build the stub secret map; render the envelope via `connector_smtp::build(..., dry_run: true)`; compare `from`, `to` (order-sensitive list equality), and `subject`; report the first mismatch.
- [ ] Run to pass, run the verification block, commit `connector_test: match tests.yaml smtp envelope expectations (post slice 3)`.

Explicitly sequenced last: do not start until slice 3 is merged, because `SmtpSpec` and the smtp render path do not exist before then, so this arm cannot compile.

---

## Notes on sequencing and dependencies

- Tasks 1-7 are the independently landable core of slice 4 and depend on nothing from slices 1-3 (matching spec section 11: "4 needs nothing from 1-3").
- Tasks 8 and 9 are gated on slices 1 and 3 respectively and are sequenced last. Their runner arms are stubbed to fail loudly in Task 4 so no expectation silently passes before its machinery exists.
- The seed `connectors/example/` folder is replaced by slice 5's four real manifests. When slice 5 removes `example/`, the CLI integration tests that reference `example` (Tasks 5-7) must be re-pointed at one of the real connectors (e.g. `github`) or at a `--dir` fixture. This handoff is noted for the slice-5 executor.

## Design decisions recorded

- **rust-embed setup**: crate `apb-core`, `rust-embed = "8.12"` (same as apb-server), derive `#[derive(rust_embed::Embed)] #[folder = "../../connectors"]`, default features (debug builds read the folder from disk, release bakes bytes in), so CLI integration tests spawning the debug binary see the seed.
- **Bootstrap**: `connectors/example/` is a real minimal official-format folder (mock ping + two HTTP functions + tests.yaml), not a hidden fixture, because install/list/test operate on exactly this. `example` (not `_smoke`) because folder names must pass `validate_profile_name`.
- **Expectation as a struct, not an untagged enum**: `deny_unknown_fields` is ignored by serde inside untagged variants; the single-struct + `resolve()` form keeps strictness real. Externally the schema matches the contract's three shapes.
- **Trust origin for embedded installs**: `OriginKind::Bundled`.
- **`--from-dir` overwrites freely** (no refuse-on-diff): it records no trust and is the explicit local dev loop; the run/probe trust gate still catches unapproved digests. `--force` is accepted there as a no-op for CLI uniformity.
