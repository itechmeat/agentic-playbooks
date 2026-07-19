# Official Connectors Slice 1: Format and Engine Extensions - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the connector format and call pipeline with the primitives four official connectors need: `path` auth with the reserved `{{auth}}` URL placeholder, a per-function `headers` map, a default `User-Agent`, per-function `examples`, response `Link` surfacing, and `response_pick` projection with a `--full` escape.

**Architecture:** Pure schema and validation additions land in `apb-core::connector` (`def.rs`, `template.rs`); rendering, header injection, link extraction, projection, and the `--full` plumb land in `apb-engine::connector_call`; the flag and instruction-block wording land in `apb-cli::connector` and `apb-engine::connector_prompt`. Dependency direction stays core <- engine <- cli; no import cycles.

**Tech Stack:** Rust edition 2024, serde/serde_yaml_ng, serde_json, jsonschema 0.26, percent-encoding, ureq 2. Tests are `#[test]` units in-crate plus integration suites driven against `common::spawn_http` ephemeral servers.

## Global Constraints

Copied verbatim from CLAUDE.md and the shared contract:

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere in code or prose. Machine-facing fields are English; user-facing chat messages are written in the user's chat language.
- New `EventPayload` fields are added only with `#[serde(default)]`.
- State files are written atomically (temp + rename, 0600 on unix) via `apb_core::fsutil`.
- Secret values are never returned, logged, or cached; the recorded pre-auth URL keeps the literal `{{auth}}` unrendered.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- code-ranker check must pass before commit: first warm the cache with `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .`; for a violation read `code-ranker docs base <ID>` before fixing.
- Commits use `git commit --signoff` and end with a `Co-Authored-By` trailer for the acting model.

---

### Task 1: Path auth variant and the `{{auth}}` URL placeholder (core)

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs`
- Modify: `crates/apb-core/src/connector/template.rs`

**Interfaces:**
- Produces: `AuthSpec::Path { value_template: String }` (serde tag `kind: path`).
- Produces: `Namespace::Auth` (the reserved `{{auth}}` marker; renders to itself).
- Consumes: existing `placeholders`, `FieldNames`, `validate_templates`.

Steps:

- [ ] Write a failing test in `crates/apb-core/src/connector/template.rs` `mod tests` asserting `{{auth}}` scans as the reserved marker and renders to itself:
  ```rust
  #[test]
  fn auth_marker_renders_to_itself_and_scans_as_auth_namespace() {
      use crate::connector::template::Namespace;
      let found = placeholders("{{account.base_url}}/{{auth}}/getMe").unwrap();
      assert!(found.iter().any(|(ns, _)| *ns == Namespace::Auth));
      let account = BTreeMap::new();
      let args = serde_json::json!({});
      let secrets = BTreeMap::new();
      let ctx = empty_ctx(&account, &args, &secrets);
      // The pre-auth URL keeps the literal placeholder unrendered (spec 4.3).
      assert_eq!(render_raw("x/{{auth}}/y", &ctx).unwrap(), "x/{{auth}}/y");
  }
  ```
- [ ] Run it and confirm it fails: `cargo test -p apb-core --lib connector::template::tests::auth_marker_renders_to_itself_and_scans_as_auth_namespace` (fails today: `parse_inner("auth")` errors `malformed placeholder`).
- [ ] In `template.rs`, add the `Auth` variant to `Namespace`:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Namespace {
      Account,
      Args,
      Secret,
      /// The reserved `{{auth}}` URL marker (spec 4.3). Not an account field:
      /// it renders to the literal `{{auth}}` so the pre-auth URL keeps the
      /// placeholder unrendered; the executor substitutes the encoded auth
      /// path segment later.
      Auth,
  }
  ```
- [ ] In `parse_inner`, recognize the bare `auth` token immediately after the `args` case:
  ```rust
  if inner == "auth" {
      return Ok((Namespace::Auth, String::new()));
  }
  ```
- [ ] In `resolve`, add the arm that keeps the marker literal:
  ```rust
  Namespace::Auth => Ok("{{auth}}".to_string()),
  ```
- [ ] Run to pass: `cargo test -p apb-core --lib connector::template`.
- [ ] Write failing tests in `crates/apb-core/src/connector/def.rs` `mod tests` for the auth-kind serde and the `{{auth}}` rules:
  ```rust
  #[test]
  fn path_auth_requires_auth_placeholder_exactly_once() {
      let ok = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/getMe\"\n";
      assert!(ConnectorDoc::from_yaml(ok, "t").is_ok());

      let missing = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    method: GET\n    url: \"{{account.base_url}}/getMe\"\n";
      let err = ConnectorDoc::from_yaml(missing, "t").unwrap_err().to_string();
      assert!(err.contains("get_me") && err.contains("auth"), "was: {err}");

      let twice = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/{{auth}}/getMe\"\n";
      assert!(ConnectorDoc::from_yaml(twice, "t").is_err());
  }

  #[test]
  fn auth_placeholder_without_path_auth_is_rejected() {
      let y = "name: t\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/x\"\n";
      let err = ConnectorDoc::from_yaml(y, "t").unwrap_err().to_string();
      assert!(err.contains("auth") && err.contains("path"), "was: {err}");
  }

  #[test]
  fn auth_placeholder_in_query_or_body_is_rejected() {
      let y = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/x\"\n    query: { k: \"{{auth}}\" }\n";
      assert!(ConnectorDoc::from_yaml(y, "t").is_err());
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-core --lib connector::def::tests::path_auth_requires_auth_placeholder_exactly_once` (fails: `Path` variant unknown to serde, and `{{auth}}` currently rejected by scan).
- [ ] In `def.rs`, add the `Path` variant to `AuthSpec`:
  ```rust
  Path {
      value_template: String,
  },
  ```
- [ ] In `def.rs` `auth_templates`, add the `Path` arm:
  ```rust
  AuthSpec::Path { value_template } => vec![value_template.as_str()],
  ```
- [ ] In `def.rs` `FieldNames::check`, add the no-op arm so a stray `{{auth}}` in an account context is ignored by field checks (its placement is policed separately):
  ```rust
  Namespace::Args | Namespace::Auth => {}
  ```
- [ ] Add a `reject_auth` helper mirroring `reject_secret`:
  ```rust
  /// Errors if `ns` is `Auth`: the `{{auth}}` marker is confined to a
  /// function `url`, so any occurrence found while walking a query, body, or
  /// auth template is a hard error naming where it was found (spec 4.3).
  fn reject_auth(
      ns: crate::connector::template::Namespace,
      where_: &str,
  ) -> Result<(), ConnectorError> {
      if ns == crate::connector::template::Namespace::Auth {
          return Err(ConnectorError::Invalid(format!(
              "the `{{{{auth}}}}` placeholder is allowed only in a function url ({where_})"
          )));
      }
      Ok(())
  }
  ```
- [ ] Rewrite the URL/query/body/auth loops in `validate_templates` to enforce the `{{auth}}` rules:
  ```rust
  pub fn validate_templates(doc: &ConnectorDoc) -> Result<(), ConnectorError> {
      use crate::connector::template::{Namespace, placeholders};

      let fields = FieldNames::from_fields(&doc.account_fields);
      let is_path_auth = matches!(doc.auth, Some(AuthSpec::Path { .. }));

      for f in &doc.functions {
          if let Some(url) = &f.url {
              let mut auth_markers = 0;
              for (ns, name) in placeholders(url)? {
                  if ns == Namespace::Auth {
                      auth_markers += 1;
                      continue;
                  }
                  reject_secret(ns, &format!("function `{}` url", f.name))?;
                  fields.check(ns, &name)?;
              }
              if is_path_auth && auth_markers != 1 {
                  return Err(ConnectorError::Invalid(format!(
                      "function `{}` url must contain the `{{{{auth}}}}` placeholder exactly once for path auth (found {auth_markers})",
                      f.name
                  )));
              }
              if !is_path_auth && auth_markers > 0 {
                  return Err(ConnectorError::Invalid(format!(
                      "function `{}` url uses `{{{{auth}}}}` but the connector does not use path auth",
                      f.name
                  )));
              }
          }
          for value in f.query.values() {
              for (ns, name) in placeholders(value)? {
                  reject_auth(ns, &format!("function `{}` query", f.name))?;
                  reject_secret(ns, &format!("function `{}` query", f.name))?;
                  fields.check(ns, &name)?;
              }
          }
          if let Some(body) = &f.body {
              validate_body_templates(body, &f.name, &fields)?;
          }
      }

      if let Some(auth) = &doc.auth {
          for template in auth_templates(auth) {
              for (ns, name) in placeholders(template)? {
                  reject_auth(ns, "auth template")?;
                  if ns == Namespace::Args {
                      return Err(ConnectorError::Invalid(
                          "args placeholders are not allowed in auth templates".to_string(),
                      ));
                  }
                  fields.check(ns, &name)?;
              }
          }
      }

      Ok(())
  }
  ```
- [ ] In `validate_body_templates`, add `reject_auth` before `reject_secret` in the `String` arm:
  ```rust
  serde_json::Value::String(s) => {
      for (ns, name) in placeholders(s)? {
          reject_auth(ns, &format!("function `{function_name}` body"))?;
          reject_secret(ns, &format!("function `{function_name}` body"))?;
          fields.check(ns, &name)?;
      }
      Ok(())
  }
  ```
- [ ] Run to pass: `cargo test -p apb-core --lib connector::def` and `cargo test -p apb-core --lib connector::template`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "core(connector): add path auth kind and the {{auth}} url placeholder"` (with the acting-model Co-Authored-By trailer).

---

### Task 2: Path-auth rendering into a pchar path segment (engine)

**Files:**
- Modify: `crates/apb-engine/src/connector_call.rs`
- Modify: `crates/apb-engine/tests/suite/connector_call.rs`

**Interfaces:**
- Consumes: `AuthSpec::Path { value_template }`, `render_raw`, the pre-auth URL carrying the literal `{{auth}}`.
- Produces: the request URL with `{{auth}}` replaced by a percent-encoded pchar segment (`:` preserved).

Steps:

- [ ] Add a failing integration test to `crates/apb-engine/tests/suite/connector_call.rs`. Add a path-auth connector inline (matching the existing `q-conn` pattern) and assert the rendered path segment keeps `:` and the event URL keeps the literal `{{auth}}`:
  ```rust
  #[test]
  fn path_auth_renders_token_into_path_segment_keeping_colon() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      // A token with a colon and url-reserved-adjacent bytes.
      let path = root.path().join(".apb/secrets.env");
      std::fs::create_dir_all(path.parent().unwrap()).unwrap();
      std::fs::write(&path, format!("{SECRET_VAR}=111:AAA_bbb-CCC\n")).unwrap();

      let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
      const TELE_YAML: &str = r#"
  name: tele
  version: 0.1.0
  auth:
    kind: path
    value_template: "bot{{secret.token}}"
  account_fields:
    - name: base_url
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: get_me
      description: probe
      read_only: true
      method: GET
      url: "{{account.base_url}}/{{auth}}/getMe"
  "#;
      let acct = ManifestAccount {
          name: "acct1".to_string(),
          default: true,
          fields: BTreeMap::from([
              ("base_url".to_string(), server.base_url.clone()),
              ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
          ]),
          env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
          digest: "sha256:acct".to_string(),
      };
      let mut m = RunExecutionManifest::default();
      m.connectors.push(ManifestConnector {
          name: "tele".to_string(),
          digest: "sha256:test".to_string(),
          accounts: vec![acct],
      });
      m.connector_grants.insert(
          NODE.to_string(),
          vec![ManifestConnectorGrant {
              connector: "tele".to_string(),
              accounts: vec!["acct1".to_string()],
              functions: vec!["get_me".to_string()],
              max_calls: None,
          }],
      );
      manifest::write(run.path(), &m).unwrap();
      let cdir = run.path().join("connectors");
      std::fs::create_dir_all(&cdir).unwrap();
      std::fs::write(cdir.join("tele.yaml"), TELE_YAML).unwrap();

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(),
          root: root.path(),
          node_id: NODE,
          connector: "tele",
          function: "get_me",
          account: None,
          args: serde_json::json!({}),
          dry_run: false,
          full: false,
      });
      assert!(ok, "expected ok: {value}");

      let req = server.captured_request().expect("server saw a request");
      assert!(
          req.starts_with("GET /bot111:AAA_bbb-CCC/getMe"),
          "path segment should keep ':' literal: {req}"
      );
      assert!(!req.contains("%3A"), "colon must not be percent-encoded: {req}");

      // The event log keeps the literal {{auth}}, never the token.
      let url = read_all(run.path())
          .unwrap()
          .iter()
          .find_map(|e| match &e.payload {
              EventPayload::ConnectorCall { url, .. } => Some(url.clone()),
              _ => None,
          })
          .unwrap();
      assert!(url.ends_with("/{{auth}}/getMe"), "pre-auth url: {url}");
      assert!(!url.contains("111:AAA"), "token leaked into event url: {url}");
  }
  ```
  Also add a dry-run assertion:
  ```rust
  #[test]
  fn path_auth_dry_run_keeps_auth_placeholder_and_needs_no_secret() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      const TELE_YAML: &str = r#"
  name: tele
  version: 0.1.0
  auth:
    kind: path
    value_template: "bot{{secret.token}}"
  account_fields:
    - name: base_url
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: get_me
      description: probe
      read_only: true
      method: GET
      url: "{{account.base_url}}/{{auth}}/getMe"
  "#;
      let mut m = RunExecutionManifest::default();
      m.connectors.push(ManifestConnector {
          name: "tele".to_string(),
          digest: "sha256:test".to_string(),
          accounts: vec![ManifestAccount {
              name: "acct1".to_string(),
              default: true,
              fields: BTreeMap::from([("base_url".to_string(), "https://api.telegram.org".to_string()), ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}"))]),
              env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
              digest: "sha256:a".to_string(),
          }],
      });
      m.connector_grants.insert(NODE.to_string(), vec![ManifestConnectorGrant {
          connector: "tele".to_string(),
          accounts: vec!["acct1".to_string()],
          functions: vec!["get_me".to_string()],
          max_calls: None,
      }]);
      manifest::write(run.path(), &m).unwrap();
      let cdir = run.path().join("connectors");
      std::fs::create_dir_all(&cdir).unwrap();
      std::fs::write(cdir.join("tele.yaml"), TELE_YAML).unwrap();

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(), root: root.path(), node_id: NODE,
          connector: "tele", function: "get_me", account: None,
          args: serde_json::json!({}), dry_run: true, full: false,
      });
      assert!(ok, "dry-run should succeed without a secret: {value}");
      assert_eq!(value["url"], serde_json::json!("https://api.telegram.org/{{auth}}/getMe"));
  }
  ```
- [ ] Add `full: false` to the `call` helper's `CallRequest` and to the existing inline `CallRequest` literals in this suite (the `q-conn` and `other` tests), so the crate compiles once `CallRequest` gains the field in Task 6. (If Task 6 lands first, this is already done; steps are independent but the `full` field must exist to compile these tests. Sequence Task 6's struct-field step before running this task's tests, or add `full: false` and the field in the same edit here.)
- [ ] Run and confirm failure: `cargo test -p apb-engine --test main connector_call::path_auth_renders_token_into_path_segment_keeping_colon` (fails: `{{auth}}` never substituted, request 404s or path wrong; and `full` field missing).
- [ ] In `connector_call.rs`, add the pchar path-segment encoder:
  ```rust
  /// Percent-encodes a rendered auth value as one URL path segment per RFC 3986
  /// pchar rules, keeping ':' literal (Telegram tokens embed a colon, spec 4.3).
  /// pchar = unreserved / sub-delims / ':' / '@'.
  fn encode_path_segment(s: &str) -> String {
      use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
      const PCHAR: &AsciiSet = &NON_ALPHANUMERIC
          .remove(b'-').remove(b'.').remove(b'_').remove(b'~')
          .remove(b'!').remove(b'$').remove(b'&').remove(b'\'')
          .remove(b'(').remove(b')').remove(b'*').remove(b'+')
          .remove(b',').remove(b';').remove(b'=')
          .remove(b':').remove(b'@');
      utf8_percent_encode(s, PCHAR).to_string()
  }
  ```
- [ ] In `HttpCall::send_raw`, replace the `request_url` computation so path auth substitutes `{{auth}}` while query auth keeps its existing behavior:
  ```rust
  let request_url = match &self.auth {
      Some(AuthSpec::Path { value_template }) => match render_raw(value_template, &ctx) {
          Ok(seg) => self
              .pre_auth_url
              .replacen("{{auth}}", &encode_path_segment(&seg), 1),
          Err(e) => {
              return (
                  Err(CallError::new(
                      CallErrorCode::Config,
                      format!("auth render failed: {e}"),
                  )),
                  None,
              );
          }
      },
      _ => match self.auth_query(&ctx) {
          Ok(Some((param, value))) => {
              assemble_url(&self.pre_auth_url, &format!("{param}={value}"))
          }
          Ok(None) => self.pre_auth_url.clone(),
          Err(e) => return (Err(e), None),
      },
  };
  ```
  `auth_header`'s existing `_ => Ok(None)` arm already covers `Path`, so no header is injected for path auth. No change to the pre-auth URL construction in `build_prepared`: `render_raw` now emits `{{auth}}` verbatim (Task 1), and `validate_url` accepts it (the marker sits in the path, after the authority).
- [ ] Run to pass: `cargo test -p apb-engine --test main connector_call::path_auth`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "engine(connector): render path auth into a pchar url segment"` (with the acting-model Co-Authored-By trailer).

---

### Task 3: Per-function `headers` map and default `User-Agent` (core + engine)

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs`
- Modify: `crates/apb-engine/src/connector_call.rs`
- Modify: `crates/apb-engine/tests/suite/connector_call.rs`

**Interfaces:**
- Produces: `FunctionSpec.headers: BTreeMap<String, String>` (serde default).
- Produces: `HttpCall.headers` rendered map; default `User-Agent: apb/<crate version>` sent unless overridden.
- Consumes: `render_raw`, the secret-placement policy.

Steps:

- [ ] Write a failing core test in `def.rs` `mod tests` that headers forbid secrets but allow account/args, and reject `{{auth}}`:
  ```rust
  #[test]
  fn headers_forbid_secret_and_auth_allow_account_and_args() {
      let ok = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/x\"\n    headers:\n      X-Api-Version: \"2024-01\"\n      X-From: \"{{account.base_url}}\"\n      X-Q: \"{{args.q}}\"\n";
      assert!(ConnectorDoc::from_yaml(ok, "x").is_ok());

      let sec = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    headers:\n      X-Leak: \"{{secret.token}}\"\n";
      let err = ConnectorDoc::from_yaml(sec, "x").unwrap_err().to_string();
      assert!(err.contains("secret") && err.contains("auth"), "was: {err}");
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-core --lib connector::def::tests::headers_forbid_secret_and_auth_allow_account_and_args` (fails: `headers` is an unknown field under `deny_unknown_fields`).
- [ ] In `def.rs` `FunctionSpec`, add the field (after `query`):
  ```rust
  #[serde(default)]
  pub headers: BTreeMap<String, String>,
  ```
- [ ] In `validate_templates`, add a headers loop inside the per-function block (after the query loop):
  ```rust
  for value in f.headers.values() {
      for (ns, name) in placeholders(value)? {
          reject_auth(ns, &format!("function `{}` headers", f.name))?;
          reject_secret(ns, &format!("function `{}` headers", f.name))?;
          fields.check(ns, &name)?;
      }
  }
  ```
- [ ] Run to pass: `cargo test -p apb-core --lib connector::def`.
- [ ] Write a failing engine test in `crates/apb-engine/tests/suite/connector_call.rs`. Extend the suite's `CONNECTOR_YAML` with two functions:
  ```yaml
    - name: with_headers
      description: sends custom headers
      read_only: true
      method: GET
      url: "{{account.base_url}}/h"
      headers:
        X-Api-Version: "2024-01"
        Accept: application/vnd.test+json
    - name: ua_override
      description: overrides the default user agent
      read_only: true
      method: GET
      url: "{{account.base_url}}/u"
      headers:
        User-Agent: custom-agent/9
  ```
  and add:
  ```rust
  #[test]
  fn sends_function_headers_and_default_user_agent() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      seed_secret(root.path());
      let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
      seed_run(run.path(), vec![account(&server.base_url)], &["acct1"], &["with_headers"], None);
      let (_v, ok) = call(run.path(), root.path(), "with_headers", None, serde_json::json!({}), false);
      assert!(ok);
      let req = server.captured_request().unwrap();
      assert!(req.contains("X-Api-Version: 2024-01"), "custom header missing: {req}");
      assert!(req.contains("Accept: application/vnd.test+json"), "accept missing: {req}");
      assert!(req.contains("User-Agent: apb/"), "default UA missing: {req}");
  }

  #[test]
  fn function_user_agent_overrides_the_default() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      seed_secret(root.path());
      let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
      seed_run(run.path(), vec![account(&server.base_url)], &["acct1"], &["ua_override"], None);
      let (_v, ok) = call(run.path(), root.path(), "ua_override", None, serde_json::json!({}), false);
      assert!(ok);
      let req = server.captured_request().unwrap();
      assert!(req.contains("User-Agent: custom-agent/9"), "override missing: {req}");
      assert!(!req.contains("User-Agent: apb/"), "default UA must not also be sent: {req}");
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-engine --test main connector_call::sends_function_headers_and_default_user_agent`.
- [ ] In `connector_call.rs`, add the constant and the `HttpCall.headers` field:
  ```rust
  /// The default User-Agent (spec 4.4): reqwest/ureq send none, and GitHub
  /// rejects requests without one. Overridden by a function `headers` entry.
  const USER_AGENT: &str = concat!("apb/", env!("CARGO_PKG_VERSION"));
  ```
  Add to `struct HttpCall`:
  ```rust
  /// Rendered per-function headers (spec 4.4). Values use `account.*`/`args.*`
  /// only; `secret.*` is forbidden here by `validate_templates`.
  headers: BTreeMap<String, String>,
  ```
- [ ] In `build_prepared`, render headers with the raw `ctx` (before the `dry_run` early return, so headers are computed but only used on real dispatch):
  ```rust
  let mut headers = BTreeMap::new();
  for (name, template) in &function.headers {
      let value = render_raw(template, &ctx).map_err(|e| {
          CallError::new(CallErrorCode::Config, format!("header `{name}` render failed: {e}"))
      })?;
      headers.insert(name.clone(), value);
  }
  ```
  and add `headers,` to the `HttpCall { ... }` construction. Dry-run output stays method/url/body only (unchanged, per requirement).
- [ ] In `HttpCall::send_raw`, after `let mut request = agent.request(...)` and before auth-header injection, set the default UA then function headers:
  ```rust
  let has_ua = self.headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent"));
  if !has_ua {
      request = request.set("User-Agent", USER_AGENT);
  }
  for (name, value) in &self.headers {
      request = request.set(name, value);
  }
  ```
- [ ] Run to pass: `cargo test -p apb-engine --test main connector_call::sends_function_headers_and_default_user_agent connector_call::function_user_agent_overrides_the_default`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-core -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "connector: per-function headers map and default User-Agent"` (with the acting-model Co-Authored-By trailer).

---

### Task 4: `examples` field and example-args validation (core)

**Files:**
- Modify: `crates/apb-core/Cargo.toml`
- Modify: `crates/apb-core/src/connector/def.rs`

**Interfaces:**
- Produces: `FunctionSpec.examples: Vec<ExampleSpec>` (serde default), `ExampleSpec { args: serde_json::Value, note: String }`.
- Consumes: `jsonschema::validator_for` over `args_schema`.

Steps:

- [ ] Write a failing test in `def.rs` `mod tests`:
  ```rust
  #[test]
  fn examples_validate_against_args_schema() {
      let ok = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }\n    examples:\n      - args: { title: hi }\n        note: minimal create\n";
      assert!(ConnectorDoc::from_yaml(ok, "x").is_ok());

      let bad = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }\n    examples:\n      - args: { nope: 1 }\n        note: missing required title\n";
      let err = ConnectorDoc::from_yaml(bad, "x").unwrap_err().to_string();
      assert!(err.contains("f") && err.contains("example"), "was: {err}");
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-core --lib connector::def::tests::examples_validate_against_args_schema` (fails: `examples` unknown field; jsonschema not a dep).
- [ ] Add the dependency to `crates/apb-core/Cargo.toml` under `[dependencies]`:
  ```toml
  jsonschema.workspace = true
  ```
- [ ] In `def.rs`, add the `ExampleSpec` struct near `MockSpec`:
  ```rust
  /// One authored example call for a function: `args` renders into the agent
  /// instruction block after the description, and the validator checks it
  /// against the function's `args_schema` (spec 4.4).
  #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct ExampleSpec {
      pub args: serde_json::Value,
      pub note: String,
  }
  ```
- [ ] Add the field to `FunctionSpec` (after `args_schema`):
  ```rust
  #[serde(default)]
  pub examples: Vec<ExampleSpec>,
  ```
- [ ] Add the validator and call it at the end of `from_yaml`, after `validate_templates(&doc)?;`:
  ```rust
  validate_examples(&doc)?;
  ```
  ```rust
  /// Validates that every function example's `args` conform to that function's
  /// `args_schema` (spec 4.4), so an example cannot drift from the schema
  /// silently. Functions without an `args_schema` are skipped (nothing to
  /// check against).
  fn validate_examples(doc: &ConnectorDoc) -> Result<(), ConnectorError> {
      for f in &doc.functions {
          if f.examples.is_empty() {
              continue;
          }
          let Some(schema) = &f.args_schema else {
              continue;
          };
          let validator = jsonschema::validator_for(schema).map_err(|e| {
              ConnectorError::Invalid(format!(
                  "function `{}` args_schema is not a valid JSON schema: {e}",
                  f.name
              ))
          })?;
          for (i, ex) in f.examples.iter().enumerate() {
              if let Some(err) = validator.iter_errors(&ex.args).next() {
                  return Err(ConnectorError::Invalid(format!(
                      "function `{}` example {} args fail its args_schema: {err}",
                      f.name,
                      i + 1
                  )));
              }
          }
      }
      Ok(())
  }
  ```
- [ ] Run to pass: `cargo test -p apb-core --lib connector::def`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "core(connector): per-function examples validated against args_schema"` (with the acting-model Co-Authored-By trailer).

---

### Task 5: Surface the response `Link` header (engine)

**Files:**
- Modify: `crates/apb-engine/src/connector_call.rs`
- Modify: `crates/apb-engine/tests/suite/connector_call.rs`

**Interfaces:**
- Produces: `CallOk.link: Option<String>`; result JSON gains `link` only when present.
- Consumes: `ureq::Response::header("Link")`.

Steps:

- [ ] Write a failing test in the engine suite:
  ```rust
  #[test]
  fn response_link_header_surfaces_in_result() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      seed_secret(root.path());
      let link = r#"<https://api.example/next>; rel="next""#;
      let server = common::spawn_http(200, "OK", &[("Link", link)], r#"{"items":[]}"#.to_string());
      seed_run(run.path(), vec![account(&server.base_url)], &["acct1"], &["list_items"], None);
      let (value, ok) = call(run.path(), root.path(), "list_items", None, serde_json::json!({}), false);
      assert!(ok, "{value}");
      assert_eq!(value["link"], serde_json::json!(link));
  }

  #[test]
  fn absent_link_header_omits_the_field() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      seed_secret(root.path());
      let server = common::spawn_http(200, "OK", &[], r#"{"items":[]}"#.to_string());
      seed_run(run.path(), vec![account(&server.base_url)], &["acct1"], &["list_items"], None);
      let (value, ok) = call(run.path(), root.path(), "list_items", None, serde_json::json!({}), false);
      assert!(ok);
      assert!(value.get("link").is_none(), "link must be absent: {value}");
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-engine --test main connector_call::response_link_header_surfaces_in_result`.
- [ ] In `connector_call.rs`, add the field to `CallOk`:
  ```rust
  /// The raw `Link` response header, when the service sent one (spec 4.4).
  pub link: Option<String>,
  ```
- [ ] In `map_status`, set `link: None` in the success `CallOk`:
  ```rust
  return Ok(CallOk {
      status,
      body,
      truncated,
      link: None,
      picked: false, // set in Task 6
  });
  ```
  (If Task 6 has not yet added `picked`, omit that line; the two tasks touch the same struct literal, so land them together or keep the field set consistent.)
- [ ] In `HttpCall::send_raw`, capture the header before consuming the body and attach it to the mapped Ok:
  ```rust
  let link = response.header("Link").map(|s| s.to_string());
  let (body, truncated) = read_body(response);
  let body = self.redact(body);
  let mut mapped = map_status(status, body, truncated, retry_after);
  if let Ok(ok) = &mut mapped {
      ok.link = link;
  }
  (mapped, Some(status))
  ```
- [ ] In `execute`, extend the `Outcome::Ok` arm to add `link` when present:
  ```rust
  Outcome::Ok(ok, meta) => {
      append_event(&req, &meta);
      let mut value = json!({
          "ok": true,
          "status": ok.status,
          "body": ok.body,
          "truncated": ok.truncated,
      });
      if let Some(link) = &ok.link {
          value["link"] = json!(link);
      }
      (value, true)
  }
  ```
- [ ] Run to pass: `cargo test -p apb-engine --test main connector_call::response_link connector_call::absent_link`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "engine(connector): surface the response Link header in the call result"` (with the acting-model Co-Authored-By trailer).

---

### Task 6: `response_pick` projection, `picked` marker, and `--full` plumb (core + engine)

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs`
- Modify: `crates/apb-engine/src/connector_call.rs`
- Modify: `crates/apb-engine/tests/suite/connector_call.rs`

**Interfaces:**
- Produces: `FunctionSpec.response_pick: Vec<String>` (serde default); `CallRequest.full: bool`; `CallOk.picked: bool`; result JSON gains `picked: true` only when applied.
- Consumes: response body Value; the projection engine `project`.

Steps:

- [ ] Write a failing core test in `def.rs` `mod tests`:
  ```rust
  #[test]
  fn response_pick_on_mock_is_rejected() {
      let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    response_pick: [a, b]\n    mock: { status: 200, body: { a: 1 } }\n";
      let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
      assert!(err.contains("f") && err.contains("response_pick"), "was: {err}");
  }

  #[test]
  fn response_pick_on_http_is_accepted() {
      let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    response_pick: [number, user.login]\n";
      assert!(ConnectorDoc::from_yaml(y, "x").is_ok());
  }
  ```
- [ ] Run and confirm failure (fails: `response_pick` unknown field).
- [ ] In `def.rs` `FunctionSpec`, add:
  ```rust
  #[serde(default)]
  pub response_pick: Vec<String>,
  ```
- [ ] In `from_yaml`, inside the per-function loop after the `match (is_http, is_mock)` block, reject `response_pick` on non-HTTP functions:
  ```rust
  // response_pick projects an HTTP response body (spec 4.5); a mock returns
  // an authored payload, so a projection there is meaningless.
  // NOTE(smtp-slice): extend this guard with `|| is_smtp` when the smtp
  // function kind lands.
  if !f.response_pick.is_empty() && is_mock {
      return Err(ConnectorError::Invalid(format!(
          "function `{}` sets response_pick but is a mock; response_pick is only valid on HTTP functions",
          f.name
      )));
  }
  ```
- [ ] Run to pass: `cargo test -p apb-core --lib connector::def`.
- [ ] Add failing projection unit tests to `connector_call.rs` `mod tests`:
  ```rust
  #[test]
  fn project_keeps_named_object_fields_and_nested_paths() {
      let body = json!({
          "number": 7, "title": "t", "extra": "drop",
          "user": { "login": "octo", "id": 1 }
      });
      let picked = project(&body, &["number".into(), "title".into(), "user.login".into()]);
      assert_eq!(picked, json!({ "number": 7, "title": "t", "user": { "login": "octo" } }));
  }

  #[test]
  fn project_maps_over_arrays_at_top_level_and_midway() {
      let body = json!([
          { "number": 1, "labels": [ { "name": "bug", "color": "red" }, { "name": "p1" } ], "x": 9 },
          { "number": 2, "labels": [] }
      ]);
      let picked = project(&body, &["number".into(), "labels.name".into()]);
      assert_eq!(picked, json!([
          { "number": 1, "labels": [ { "name": "bug" }, { "name": "p1" } ] },
          { "number": 2, "labels": [] }
      ]));
  }

  #[test]
  fn project_drops_missing_paths_silently() {
      let body = json!({ "a": 1 });
      let picked = project(&body, &["a".into(), "b.c".into()]);
      assert_eq!(picked, json!({ "a": 1 }));
  }
  ```
- [ ] Run and confirm failure (fails: `project` undefined).
- [ ] In `connector_call.rs`, add the projection engine:
  ```rust
  /// Projects `body` down to the dot-separated field chains in `paths`
  /// (spec 4.5). Objects keep the named field; arrays (top-level or midway
  /// through a chain) map the projection over their elements; a path absent
  /// from the body is silently dropped.
  fn project(body: &Value, paths: &[String]) -> Value {
      let split: Vec<Vec<&str>> = paths.iter().map(|p| p.split('.').collect()).collect();
      let refs: Vec<&[&str]> = split.iter().map(Vec::as_slice).collect();
      project_value(body, &refs)
  }

  fn project_value(source: &Value, paths: &[&[&str]]) -> Value {
      match source {
          Value::Array(items) => {
              Value::Array(items.iter().map(|it| project_value(it, paths)).collect())
          }
          Value::Object(map) => {
              let mut out = serde_json::Map::new();
              for (key, val) in map {
                  let mut sub: Vec<&[&str]> = Vec::new();
                  let mut terminal = false;
                  for p in paths {
                      if p.first() == Some(&key.as_str()) {
                          if p.len() == 1 {
                              terminal = true;
                          } else {
                              sub.push(&p[1..]);
                          }
                      }
                  }
                  if terminal {
                      out.insert(key.clone(), val.clone());
                  } else if !sub.is_empty() {
                      out.insert(key.clone(), project_value(val, &sub));
                  }
              }
              Value::Object(out)
          }
          other => other.clone(),
      }
  }
  ```
- [ ] Run the projection units to pass: `cargo test -p apb-engine --lib connector_call::tests::project`.
- [ ] Add `pub full: bool` to `CallRequest` (documented as the `--full` escape) and `pub picked: bool` to `CallOk`. Set `picked: false` in `map_status`'s success construction.
- [ ] Add `response_pick: Vec<String>` to `struct HttpCall`.
- [ ] Change `build_prepared`'s signature to take a `full: bool` parameter (add after `dry_run`), compute the effective pick list, and store it:
  ```rust
  let response_pick = if full { Vec::new() } else { function.response_pick.clone() };
  ```
  and add `response_pick,` to the `HttpCall { ... }` construction.
- [ ] Update the two `build_prepared` call sites:
  - In `prepare`, pass `req.dry_run, req.full`.
  - In `prepare_healthcheck`, pass `false, true` (a reachability probe returns the raw body; note this in a one-line comment).
- [ ] In `HttpCall::send_raw`, after attaching `link`, apply projection:
  ```rust
  if let Ok(ok) = &mut mapped {
      ok.link = link;
      if !self.response_pick.is_empty() {
          ok.body = project(&ok.body, &self.response_pick);
          ok.picked = true;
      }
  }
  ```
- [ ] In `execute`, add `picked` to the ok JSON when true (alongside the Task 5 `link` insert):
  ```rust
  if ok.picked {
      value["picked"] = json!(true);
  }
  ```
- [ ] Add `full: false` to the `call` helper's `CallRequest` and to every inline `CallRequest` literal in this suite and in `connector_e2e.rs` (see Task 8), and add a `call_full` helper:
  ```rust
  fn call_full<'a>(
      run_dir: &'a Path, root: &'a Path, function: &'a str, args: serde_json::Value,
  ) -> (serde_json::Value, bool) {
      execute(CallRequest {
          run_dir, root, node_id: NODE, connector: CONNECTOR, function,
          account: None, args, dry_run: false, full: true,
      })
  }
  ```
- [ ] Add a failing e2e test using an inline projecting function. Extend the suite's `CONNECTOR_YAML` with:
  ```yaml
    - name: list_pick
      description: list with a projection
      read_only: true
      method: GET
      url: "{{account.base_url}}/pick"
      response_pick: [number, user.login, labels.name]
  ```
  and:
  ```rust
  #[test]
  fn response_pick_projects_by_default_and_full_bypasses_it() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      seed_secret(root.path());
      let raw = r#"[{"number":1,"title":"x","user":{"login":"octo","id":9},"labels":[{"name":"bug","color":"red"}]}]"#;
      let server = common::spawn_http(200, "OK", &[], raw.to_string());
      seed_run(run.path(), vec![account(&server.base_url)], &["acct1"], &["list_pick"], None);

      // Default: projected body, picked: true.
      let (value, ok) = call(run.path(), root.path(), "list_pick", None, serde_json::json!({}), false);
      assert!(ok, "{value}");
      assert_eq!(value["picked"], serde_json::json!(true));
      assert_eq!(value["body"], serde_json::json!([
          { "number": 1, "user": { "login": "octo" }, "labels": [ { "name": "bug" } ] }
      ]));

      // --full: raw body, no picked marker. Needs a fresh one-shot server.
      let server2 = common::spawn_http(200, "OK", &[], raw.to_string());
      seed_run(run.path(), vec![account(&server2.base_url)], &["acct1"], &["list_pick"], None);
      let (full, ok2) = call_full(run.path(), root.path(), "list_pick", serde_json::json!({}));
      assert!(ok2, "{full}");
      assert!(full.get("picked").is_none(), "full must not mark picked: {full}");
      assert_eq!(full["body"][0]["title"], serde_json::json!("x"));
      assert_eq!(full["body"][0]["user"]["id"], serde_json::json!(9));
  }
  ```
- [ ] Run to pass: `cargo test -p apb-engine --test main connector_call::response_pick_projects_by_default_and_full_bypasses_it`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-core -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "connector: response_pick projection with picked marker and --full escape"` (with the acting-model Co-Authored-By trailer).

---

### Task 7: `--full` CLI flag and instruction-block updates (cli + engine prompt)

**Files:**
- Modify: `crates/apb-cli/src/connector.rs`
- Modify: `crates/apb-engine/src/connector_prompt.rs`
- Modify: `crates/apb-cli/tests/suite/connector_cli.rs`

**Interfaces:**
- Consumes: `CallRequest.full`, `FunctionSpec.examples`.
- Produces: `apb connector call --full`; instruction block documents `--full` and renders examples.

Steps:

- [ ] Add a failing prompt test in `connector_prompt.rs` `mod tests`. First extend `sample_docs()`'s `list_items` function with:
  ```yaml
      examples:
        - args: { q: bug }
          note: filter items by text
  ```
  then:
  ```rust
  #[test]
  fn documents_full_flag_and_renders_examples() {
      let grants = vec![grant(&["list_items"])];
      let out = instruction_block(&grants, &[sample_connector()], &sample_docs());
      assert!(out.contains("--full"), "the --full escape should be documented: {out}");
      assert!(out.contains("filter items by text"), "example note missing: {out}");
      assert!(out.contains(r#"{"q":"bug"}"#), "example args missing: {out}");
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-engine --lib connector_prompt::tests::documents_full_flag_and_renders_examples`.
- [ ] In `connector_prompt.rs` `instruction_block`, extend the help text to mention `--full` (rewrite the existing dry-run note paragraph):
  ```rust
  out.push_str(
      "`--args -` reads the JSON arguments from stdin (use it for large payloads). \
       `--dry-run` previews the request (method, URL, body) without executing it. \
       Calls return a trimmed subset of the response by default; add `--full` to \
       get the complete body when debugging.\n",
  );
  ```
- [ ] In the function loop, after the `args:` line, render examples:
  ```rust
  for ex in &f.examples {
      out.push_str(&format!("  example: {} - {}\n", compact_json(&ex.args), ex.note));
  }
  ```
- [ ] Run to pass: `cargo test -p apb-engine --lib connector_prompt`.
- [ ] In `connector.rs`, add the flag to `ConnectorAction::Call`:
  ```rust
  /// Return the complete response body, skipping the function's
  /// response_pick projection (spec 4.5 debugging escape)
  #[arg(long)]
  full: bool,
  ```
- [ ] Thread it through `connector_cmd`'s `Call` arm and `call_cmd`'s signature, and set `full` on the `CallRequest`:
  ```rust
  ConnectorAction::Call { name, function, account, args, dry_run, full } =>
      call_cmd(root, &name, &function, account, args, dry_run, full),
  ```
  ```rust
  fn call_cmd(
      root: &Path, name: &str, function: &str,
      account: Option<String>, args: Option<String>, dry_run: bool, full: bool,
  ) -> ExitCode {
      // ... unchanged body ...
      let req = apb_engine::connector_call::CallRequest {
          run_dir: Path::new(&run_dir), root, node_id: &node_id,
          connector: name, function, account: account.as_deref(),
          args: parsed_args, dry_run, full,
      };
      // ...
  }
  ```
- [ ] Add a CLI smoke test to `connector_cli.rs` asserting the flag parses (no run context needed; the flag must not be rejected by clap):
  ```rust
  #[test]
  fn call_accepts_the_full_flag() {
      let dir = tempfile::tempdir().unwrap();
      setup(dir.path());
      apb_ok(dir.path(), &["connector", "init", "widget"]);
      // No run context, so this still exits config-error, but --full must parse.
      let out = playbook(dir.path(), &["connector", "call", "widget", "ping", "--full"]);
      let stdout = String::from_utf8_lossy(&out.stdout);
      let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
      assert_eq!(v["error"]["code"], serde_json::json!("config"));
  }
  ```
- [ ] Run to pass: `cargo test -p apb-cli --test main connector_cli::call_accepts_the_full_flag` and `cargo test -p apb-engine --lib connector_prompt`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p apb-cli -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -m "cli+prompt(connector): --full flag and instruction-block examples"` (with the acting-model Co-Authored-By trailer).

---

### Task 8: Fixture refresh and cross-suite `CallRequest` field fix

**Files:**
- Modify: `crates/apb-engine/tests/fixtures/connectors/mock-tracker/connector.yaml`
- Modify: `crates/apb-engine/tests/suite/connector_e2e.rs`

**Interfaces:**
- Consumes: the new `FunctionSpec` fields end to end through the resolve + snapshot + call path.

Steps:

- [ ] Add `full: false` to the `CallRequest` literal in `connector_e2e.rs`'s `call` helper (the field is now mandatory; without this the suite will not compile). Run `cargo test -p apb-engine --test main --no-run` and confirm it now compiles.
- [ ] Extend the `mock-tracker` fixture `list_items` function with `headers` and `examples` (both inert to the existing `e2e_snapshot_and_call_flow` body/redaction assertions; the function keeps its `q` arg and returns the server body verbatim):
  ```yaml
    - name: list_items
      description: List items over HTTP
      read_only: true
      method: GET
      url: "{{account.base_url}}/items"
      query: { q: "{{args.q}}" }
      headers:
        X-Api-Version: "2024-01"
      args_schema: { type: object, properties: { q: { type: string } }, required: [q] }
      examples:
        - args: { q: bug }
          note: filter items by text
  ```
  Do NOT add `response_pick` to `list_items`: that function's body is asserted verbatim (including the echoed secret for the redaction test), and projecting it would drop `echo`. response_pick behavior is covered by the inline `list_pick` function in Task 6.
- [ ] Run the full connector suites to confirm no regression: `cargo test -p apb-engine --test main connector_e2e connector_call connector_healthcheck`.
- [ ] Run the whole workspace once: `cargo test --workspace`.
- [ ] `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Warm the code-ranker cache and run it: `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .`; fix any violation per its docs.
- [ ] Commit: `git commit --signoff -m "test(connector): refresh mock-tracker fixture with headers and examples"` (with the acting-model Co-Authored-By trailer).

---

## Slice notes for parallel slices

- `Namespace::Auth` is an additive variant on `apb_core::connector::template::Namespace`; any slice that exhaustively matches `Namespace` (the smtp slice touches template validation) must add an arm for it.
- `ExampleSpec.note` is a required `String` (no serde default); manifest authors must always provide a note.
- The `ConnectorCall` event payload records no body field, so `response_pick` does not touch the event log at all (projection applies only to the printed CLI result); the spec's "raw body in the log" phrasing resolves to "the event keeps recording outcome, status, and pre-auth url from the raw call".
- The healthcheck probe passes `full = true` internally: a reachability probe always returns the untrimmed body.
