# Official Connectors Slice 6: Dashboard Playground - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the connector detail page a manual "call a function" playground: pick a function, get a form generated from its `args_schema`, pick an account (default preselected), toggle dry-run (default ON) vs real, and see the structured result (status, body or error code, `link`, `picked`). Add one server endpoint, `POST /api/connectors/:name/call`, that wraps the same live execution path the healthcheck probe already uses, extended with an arbitrary function name, args, and the dry-run flag, trust-gated identically to the probe for a real call.

**Architecture:** Rust axum server (`crates/apb-server`) delegates to a new `apb_engine::connector_call::play_call` function that generalizes the existing `healthcheck`/`prepare_healthcheck` live-probe pipeline (no run context, no manifest, no grants/budget - a playground call is not a run) to an arbitrary function, args, and dry-run. Trust gating (connector digest + account digest approval) applies only to a real call; a dry-run never resolves secrets and is not gated. The Svelte dashboard (`web/`) gets one new pure-logic module (`web/src/lib/connectorplay.ts`, form generation + result rendering, unit-tested without DOM, mirroring `connectorstats.ts`) and one new thin component (`ConnectorPlaygroundPanel.svelte`) wired into the existing `ConnectorView.svelte` detail page, following the existing `fetchConnector`/`runConnectorHealthcheck` API-client pattern in `web/src/lib/api.ts`.

**Tech Stack:** Rust (axum, serde_json, jsonschema, ureq), Svelte 5 + shadcn-svelte + Tailwind v4 (auto light/dark via `.dark`), bun + vitest for the frontend.

## Global Constraints

(Copied from `CLAUDE.md`; applies to every task below.)

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere in code or prose. Machine-facing fields are English; user-facing chat messages are written in the user's chat language.
- Secret values are never returned, logged, or cached. This slice's endpoint must never surface a resolved secret in a response body.
- Format and lint gates (must be clean): `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
- Frontend (`web/`): `bun run test` and `bun run check` must be clean; `bun run build` before anything considered release-ready.
- Before code is ready to commit: run code-ranker and fix any violation. First warm the cargo cache: `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .`; for a violation read `code-ranker docs base <ID>` before fixing, fix, and re-run until clean.
- Commit only after the owner approves; commits use `git commit --signoff` and end with a `Co-Authored-By` trailer for the acting model; never add a visible AI-authorship marker to public prose.
- Navigate the code through codegraph rather than ad-hoc grep where possible.

## Shared interface contract (from the wave-1 master plan)

This slice depends on slices 1-5 having landed. In particular:

- The call result carries `link` (raw `Link` response header, when present) and `picked` (true when `response_pick` was applied). NOTE: slice 3 refactors `CallOk` into a two-variant enum (`Http`/`Smtp`); the code below written against `ok.status`/`ok.body`/`ok.link`/`ok.picked` field access must be adapted to the enum shape (match, or `to_success_json`) at execution time.
- The call result JSON shapes (base spec section 8, extended by slice 1) are:
  - success: `{ "ok": true, "status": <u16>, "body": <json>, "truncated": <bool>, "link": <string|null>, "picked": <bool> }` (HTTP) or `{ "ok": true, "body": <json> }` (smtp)
  - error: `{ "ok": false, "error": { "code": <string>, "message": <string>, "http_status"?: <u16>, "retry_after_sec"?: <u64> } }`
  - dry-run: `{ "ok": true, "dry_run": true, "method": <string>, "url": <string>, "body": <json|null> }` (HTTP) or the smtp envelope form.

## Resolved ambiguities

- **Does a dry-run go through trust gating?** Resolution: **no**. `build_prepared`'s secret-resolution step already skips entirely for a dry-run; the trust gate exists solely to guard secret egress, so gating a call that touches no secret would refuse a safe render for no security benefit. `play_call` applies the trust gate only when `dry_run` is `false`.
- **Account defaults when the request omits `account`.** Mirror the CLI's `select_account` defaulting logic (single configured account wins outright; else the one account flagged `default`; else a `config` error listing the choices) - implemented as `select_live_account` in the engine.
- **`args_schema` is not currently exposed by `GET /api/connectors/:name`.** This slice extends that endpoint's function objects with `args_schema` - a small, backward-compatible addition, not a new endpoint.
- **Raw-JSON-fallback trigger.** The fallback is both automatic (any schema with a nested object/array property, `oneOf`/`anyOf`, or no `properties` forces raw mode) and always available as a manual override switch when the schema is simple, via `isSimpleObjectSchema`.

---

### Task 1: Engine - generalize the live-probe pipeline into `play_call`

**Files:**
- Modify: `crates/apb-engine/src/connector_call.rs`
- Create: `crates/apb-engine/tests/suite/connector_play_call.rs`
- Modify: `crates/apb-engine/tests/main.rs`

**Interfaces:**
- Consumes: `apb_core::connector::store::load`, `apb_core::connector::config::{load_merged, account_digest, env_refs, validate_accounts, Account}`, `apb_core::trust::{TrustStore, account_trust_id}`, the existing `build_prepared` gate+render pipeline.
- Produces (new public engine API):
  ```rust
  pub fn play_call(
      root: &Path,
      name: &str,
      account: Option<&str>,
      function_name: &str,
      args: &Value,
      dry_run: bool,
  ) -> (Value, bool)
  ```
  Same `(Value, bool)` shape as `execute`/`healthcheck`: the JSON document to return verbatim, and an ok hint.

**Steps:**

- [ ] Confirm the slice-1/slice-3 `CallOk` shape before writing code that assumes it:
  ```bash
  grep -n "enum CallOk\|struct CallOk" -A8 crates/apb-engine/src/connector_call.rs
  grep -n '"link"\|"picked"' crates/apb-engine/src/connector_call.rs
  ```
  Adapt the success-JSON construction below to whatever landed (field access for a struct, match or `to_success_json` for the enum).

- [ ] Write the failing engine test file first:

  `crates/apb-engine/tests/suite/connector_play_call.rs`:
  ```rust
  //! Slice 6: the dashboard playground's live call
  //! (`connector_call::play_call`, spec 2026-07-19-official-connectors-design
  //! section 7). Generalizes the `healthcheck` probe pipeline (live connector
  //! definition, live merged account config, no run context) to an arbitrary
  //! function, args, and an optional dry-run. Mirrors
  //! `connector_healthcheck.rs`'s structure and fixtures.
  //!
  //! Trust gating: a real call is gated exactly like the healthcheck probe. A
  //! dry-run resolves no secrets and is therefore NOT gated.
  //!
  //! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` and the fixture's
  //! secret env var are process-wide state shared with every other module in
  //! this consolidated test binary.

  use std::path::Path;

  use apb_core::connector::config;
  use apb_core::connector::store;
  use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
  use apb_engine::connector_call::play_call;
  use serde_json::json;

  use crate::common;

  struct EnvGuard {
      var: String,
      prior: Option<std::ffi::OsString>,
  }
  impl Drop for EnvGuard {
      fn drop(&mut self) {
          unsafe {
              match &self.prior {
                  Some(v) => std::env::set_var(&self.var, v),
                  None => std::env::remove_var(&self.var),
              }
          }
      }
  }
  fn set_var(var: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
      let prior = std::env::var_os(var);
      unsafe {
          std::env::set_var(var, value);
      }
      EnvGuard {
          var: var.to_string(),
          prior,
      }
  }

  const CONNECTOR: &str = "play-conn";
  const TOKEN_VAR: &str = "APB_PLAY_TEST_TOKEN";

  fn write_connector(cfg: &Path, yaml: &str) {
      let dir = cfg.join("connectors").join(CONNECTOR);
      std::fs::create_dir_all(&dir).unwrap();
      std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
  }

  fn write_account(root: &Path, yaml: &str) {
      let path = root
          .join(".apb/connector-config")
          .join(format!("{CONNECTOR}.yaml"));
      std::fs::create_dir_all(path.parent().unwrap()).unwrap();
      std::fs::write(path, yaml).unwrap();
  }

  fn approve_connector() {
      let loaded = store::load(CONNECTOR).unwrap();
      let mut trust = TrustStore::load();
      trust
          .approve_kind(
              &loaded.digest,
              CONNECTOR,
              Kind::Connector,
              OriginKind::LocallyApproved,
          )
          .unwrap();
  }

  fn approve_account(root: &Path, account: &str) {
      let accounts = config::load_merged(root, CONNECTOR).unwrap();
      let acct = accounts.iter().find(|a| a.name == account).unwrap();
      let digest = config::account_digest(acct);
      let mut trust = TrustStore::load();
      trust
          .approve_kind(
              &digest,
              &account_trust_id(CONNECTOR, account),
              Kind::ConnectorAccount,
              OriginKind::LocallyApproved,
          )
          .unwrap();
  }

  const HTTP_YAML: &str = r#"
  name: play-conn
  version: 0.1.0
  auth:
    kind: header
    header: Authorization
    value_template: "Bearer {{secret.token}}"
  account_fields:
    - name: base_url
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: list_items
      description: List items
      read_only: true
      method: GET
      url: "{{account.base_url}}/items"
      args_schema: { type: object, properties: { q: { type: string } } }
    - name: ping
      description: Reachability check
      mock: { status: 200, body: { ok: true } }
  "#;

  #[test]
  fn dry_run_renders_without_secrets_or_trust_approval() {
      let _lock = common::env_lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = set_var("APB_CONFIG_DIR", cfg.path());
      write_connector(cfg.path(), HTTP_YAML);
      write_account(
          root.path(),
          &format!(
              "accounts:\n  - name: acct1\n    default: true\n    base_url: https://unused.example\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
          ),
      );
      // Neither the connector nor the account is approved; TOKEN_VAR is unset.

      let (value, ok) = play_call(
          root.path(),
          CONNECTOR,
          Some("acct1"),
          "list_items",
          &json!({}),
          true,
      );
      assert!(ok, "a dry-run must succeed with no approval and no secret: {value}");
      assert_eq!(value["ok"], json!(true));
      assert_eq!(value["dry_run"], json!(true));
      assert_eq!(value["method"], json!("GET"));
      assert_eq!(value["url"], json!("https://unused.example/items"));
  }

  #[test]
  fn real_call_on_unapproved_connector_is_permission_denied() {
      let _lock = common::env_lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = set_var("APB_CONFIG_DIR", cfg.path());
      write_connector(cfg.path(), HTTP_YAML);
      write_account(
          root.path(),
          &format!(
              "accounts:\n  - name: acct1\n    default: true\n    base_url: https://unused.example\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
          ),
      );
      let _g_tok = set_var(TOKEN_VAR, "secret-value");

      let (value, ok) = play_call(
          root.path(),
          CONNECTOR,
          Some("acct1"),
          "list_items",
          &json!({}),
          false,
      );
      assert!(!ok, "a real call on an unapproved connector must refuse: {value}");
      assert_eq!(value["error"]["code"], json!("permission"));
  }

  #[test]
  fn approved_real_call_reaches_the_url_with_auth() {
      let _lock = common::env_lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g_cfg = set_var("APB_CONFIG_DIR", cfg.path());
      let _g_tok = set_var(TOKEN_VAR, "play-secret-value");
      write_connector(cfg.path(), HTTP_YAML);

      let server = common::spawn_http(200, "OK", &[], r#"{"items":[]}"#.to_string());
      write_account(
          root.path(),
          &format!(
              "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
              server.base_url
          ),
      );
      approve_connector();
      approve_account(root.path(), "acct1");

      let (value, ok) = play_call(
          root.path(),
          CONNECTOR,
          Some("acct1"),
          "list_items",
          &json!({}),
          false,
      );
      assert!(ok, "approved real call should succeed: {value}");
      assert_eq!(value["status"], json!(200));
      assert_eq!(value["body"], json!({"items": []}));

      let req = server.captured_request().expect("server saw a request");
      assert!(
          req.contains("Authorization: Bearer play-secret-value"),
          "auth header missing/wrong in request:\n{req}"
      );
  }

  #[test]
  fn unknown_function_name_is_config_error() {
      let _lock = common::env_lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = set_var("APB_CONFIG_DIR", cfg.path());
      write_connector(cfg.path(), HTTP_YAML);
      write_account(
          root.path(),
          "accounts:\n  - name: acct1\n    default: true\n    base_url: https://unused.example\n    token: \"{{env.NOPE}}\"\n",
      );

      let (value, ok) = play_call(
          root.path(),
          CONNECTOR,
          Some("acct1"),
          "no_such_function",
          &json!({}),
          true,
      );
      assert!(!ok);
      assert_eq!(value["error"]["code"], json!("config"));
      let msg = value["error"]["message"].as_str().unwrap();
      assert!(msg.contains("no_such_function"), "message: {msg}");
  }

  #[test]
  fn single_configured_account_is_auto_selected() {
      let _lock = common::env_lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = set_var("APB_CONFIG_DIR", cfg.path());
      write_connector(cfg.path(), HTTP_YAML);
      // Only one account, not flagged default: still auto-selected.
      write_account(
          root.path(),
          "accounts:\n  - name: only-one\n    base_url: https://solo.example\n    token: \"{{env.NOPE}}\"\n",
      );

      let (value, ok) = play_call(root.path(), CONNECTOR, None, "list_items", &json!({}), true);
      assert!(ok, "the single account should be auto-selected: {value}");
      assert_eq!(value["url"], json!("https://solo.example/items"));
  }

  #[test]
  fn ambiguous_accounts_without_default_is_config_error() {
      let _lock = common::env_lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = set_var("APB_CONFIG_DIR", cfg.path());
      write_connector(cfg.path(), HTTP_YAML);
      write_account(
          root.path(),
          "accounts:\n  - name: a\n    base_url: https://a.example\n    token: \"{{env.NOPE}}\"\n  - name: b\n    base_url: https://b.example\n    token: \"{{env.NOPE}}\"\n",
      );

      let (value, ok) = play_call(root.path(), CONNECTOR, None, "list_items", &json!({}), true);
      assert!(!ok);
      assert_eq!(value["error"]["code"], json!("config"));
      let msg = value["error"]["message"].as_str().unwrap();
      assert!(msg.contains('a') && msg.contains('b'), "message should list choices: {msg}");
  }
  ```

- [ ] Register the new module in `crates/apb-engine/tests/main.rs`, right after `connector_healthcheck`:
  ```rust
  #[path = "suite/connector_play_call.rs"]
  mod connector_play_call;
  ```

- [ ] Run and confirm the expected compile failure (unresolved `play_call`):
  ```bash
  cargo test -p apb-engine --test main connector_play_call:: 2>&1 | tail -20
  ```

- [ ] Implement the engine changes in `crates/apb-engine/src/connector_call.rs`. Insert `play_call` immediately after the existing `healthcheck` function, replace `prepare_healthcheck`'s body to delegate to a new shared `prepare_play_call`, and add `prepare_play_call` plus its two small helpers right after it:

  ```rust
  /// Live playground call (dashboard slice 6, spec
  /// 2026-07-19-official-connectors-design section 7): the connector detail
  /// page's manual "call a function" panel. Runs an arbitrary function
  /// against the LIVE connector definition and LIVE merged account config
  /// through the exact same gate + render + dispatch pipeline `healthcheck`
  /// uses (`prepare_play_call` and `build_prepared`), so a dry-run renders
  /// the request without touching secrets and a real call gets the same
  /// trust gating, URL hardening, auth injection, and interim redaction as
  /// any other call. `account: None` selects the single or default
  /// configured account, exactly like the CLI's `select_account`. Returns
  /// the same `{ "ok": bool, ... }` shape `execute`/`healthcheck` do.
  pub fn play_call(
      root: &Path,
      name: &str,
      account: Option<&str>,
      function_name: &str,
      args: &Value,
      dry_run: bool,
  ) -> (Value, bool) {
      match prepare_play_call(root, name, account, function_name, args, dry_run) {
          Ok(Prepared::DryRun(v)) => (v, true),
          Ok(Prepared::Call(prepared)) => {
              let (result, _status) = prepared.dispatch();
              match result {
                  Ok(ok) => (ok.to_success_json(), true),
                  Err(err) => (err.to_json(), false),
              }
          }
          Err(e) => (e.to_json(), false),
      }
  }
  ```
  (Adapt the success arm to the landed `CallOk` shape; with slice 3's enum, `to_success_json` already covers link/picked once slice 1 folded them into the Http variant.)

  Replace `prepare_healthcheck` with the thin delegating version:
  ```rust
  fn prepare_healthcheck(root: &Path, name: &str, account: &str) -> Result<Prepared, CallError> {
      let loaded = store::load(name)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("connector `{name}`: {e}")))?;
      let hc_name = loaded.doc.healthcheck.clone().ok_or_else(|| {
          CallError::new(
              CallErrorCode::Config,
              format!("connector `{name}` declares no healthcheck"),
          )
      })?;
      // A healthcheck is always live (never dry-run) and takes no arguments,
      // per the healthcheck contract - it is a reachability probe, not a data
      // call. Delegates account resolution, trust gating, and rendering to
      // the shared playground preparation path so the two live-call callers
      // never duplicate that logic.
      prepare_play_call(root, name, Some(account), &hc_name, &json!({}), false)
  }
  ```

  Add, after `prepare_healthcheck`:
  ```rust
  /// Shared live-call preparation for both `healthcheck` (fixed function, no
  /// args, always live) and `play_call` (dashboard playground, spec section
  /// 7: arbitrary function, args, optional dry-run). Loads the LIVE
  /// connector definition and LIVE merged account config - no run context,
  /// no manifest, no event log, no grant/budget checks (those are run-scoped
  /// concepts a standalone live call does not have).
  ///
  /// Trust gating applies ONLY when `dry_run` is false: a dry-run never
  /// resolves secrets (`build_prepared` skips secret resolution entirely for
  /// `dry_run: true`), so gating it would refuse a safe, secret-free render
  /// for no security benefit - the gate exists to guard secret egress, and a
  /// dry-run has none to guard.
  fn prepare_play_call(
      root: &Path,
      name: &str,
      account: Option<&str>,
      function_name: &str,
      args: &Value,
      dry_run: bool,
  ) -> Result<Prepared, CallError> {
      let loaded = store::load(name)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("connector `{name}`: {e}")))?;
      let function = loaded.doc.function(function_name).cloned().ok_or_else(|| {
          CallError::new(
              CallErrorCode::Config,
              format!("connector `{name}` declares no function `{function_name}`"),
          )
      })?;

      let accounts = config::load_merged(root, name).map_err(|e| {
          CallError::new(
              CallErrorCode::Config,
              format!("connector `{name}` account config: {e}"),
          )
      })?;
      let acct = select_live_account(&accounts, account)
          .ok_or_else(|| account_selection_error(name, account, &accounts))?
          .clone();

      if !dry_run {
          let trust = TrustStore::load();
          if !trust.is_approved(&loaded.digest) {
              return Err(CallError::new(
                  CallErrorCode::Permission,
                  format!(
                      "connector `{name}` is not approved; approve it before calling (see the connector approve flow)"
                  ),
              ));
          }
          let account_digest = config::account_digest(&acct);
          if !trust.is_approved(&account_digest) {
              return Err(CallError::new(
                  CallErrorCode::Permission,
                  format!(
                      "account `{}` is not approved; approve it before calling (see the connector approve flow)",
                      account_trust_id(name, &acct.name)
                  ),
              ));
          }
      }

      let errors = config::validate_accounts(&loaded.doc, std::slice::from_ref(&acct));
      if !errors.is_empty() {
          return Err(CallError::new(
              CallErrorCode::Config,
              format!(
                  "connector `{name}` account `{}` is invalid: {}",
                  acct.name,
                  errors.join("; ")
              ),
          ));
      }

      let env = config::env_refs(&loaded.doc, &acct);
      let cmd = config::cmd_refs(&loaded.doc, &acct);
      let secret_keys: std::collections::HashSet<&str> =
          env.keys().chain(cmd.keys()).map(String::as_str).collect();
      let fields: BTreeMap<String, String> = acct
          .fields
          .iter()
          .filter(|(k, _)| !secret_keys.contains(k.as_str()))
          .map(|(k, v)| (k.clone(), v.clone()))
          .collect();
      let digest = config::account_digest(&acct);
      let maccount = ManifestAccount {
          name: acct.name.clone(),
          default: acct.default,
          fields,
          env,
          cmd,
          digest,
      };

      build_prepared(
          &function,
          loaded.doc.auth.as_ref(),
          acct.name.clone(),
          &maccount,
          args,
          root,
          dry_run,
          true,
      )
  }
  ```
  (The trailing `true` is slice 1's `full` parameter: the playground returns the raw body like the healthcheck probe; adjust if the landed signature differs. `cmd` comes from slice 2.)

  ```rust
  /// Picks the account for a live probe/playground call: an explicit name
  /// must match one of the LIVE configured accounts; with none given, the
  /// single configured account is used, else the one flagged `default`, else
  /// no selection (ambiguous, reported by the caller via
  /// `account_selection_error`). Mirrors the CLI pipeline's `select_account`
  /// defaulting rule, minus the grant list (a live call has no grants).
  fn select_live_account<'a>(
      accounts: &'a [config::Account],
      account: Option<&str>,
  ) -> Option<&'a config::Account> {
      if let Some(explicit) = account {
          return accounts.iter().find(|a| a.name == explicit);
      }
      if let [only] = accounts {
          return Some(only);
      }
      let defaults: Vec<&config::Account> = accounts.iter().filter(|a| a.default).collect();
      if let [only] = defaults.as_slice() {
          return Some(only);
      }
      None
  }

  fn account_selection_error(
      name: &str,
      account: Option<&str>,
      accounts: &[config::Account],
  ) -> CallError {
      if let Some(explicit) = account {
          return CallError::new(
              CallErrorCode::Config,
              format!("connector `{name}` has no account `{explicit}`"),
          );
      }
      let choices: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
      CallError::new(
          CallErrorCode::Config,
          format!(
              "connector `{name}` has several accounts and no single default; specify an account (choices: {})",
              choices.join(", ")
          ),
      )
  }
  ```

- [ ] Run the engine tests and confirm they pass; the existing `connector_healthcheck` tests must still pass unchanged (the refactor must not change `healthcheck`'s observable behavior):
  ```bash
  cargo test -p apb-engine --test main connector_play_call:: 2>&1 | tail -30
  cargo test -p apb-engine --test main connector_healthcheck:: 2>&1 | tail -20
  ```

- [ ] Format, lint, and code-rank gates:
  ```bash
  cargo fmt --all -- --check
  cargo clippy -p apb-engine --all-targets -- -D warnings
  cargo metadata --format-version 1 >/dev/null
  code-ranker check .
  ```

- [ ] Commit:
  ```bash
  git add crates/apb-engine/src/connector_call.rs crates/apb-engine/tests/suite/connector_play_call.rs crates/apb-engine/tests/main.rs
  git commit --signoff -m "engine: add connector_call::play_call for the dashboard playground"
  ```
  (with the acting-model Co-Authored-By trailer in the message body).

---

### Task 2: Server - `POST /api/connectors/:name/call` endpoint, `args_schema` exposure, integration tests

**Files:**
- Modify: `crates/apb-server/src/lib.rs`
- Modify: `crates/apb-server/tests/suite/common.rs`
- Modify: `crates/apb-server/tests/suite/connectors_api_test.rs`

**Interfaces:**
- Produces (new route): `POST /api/connectors/{name}/call?workspace=<id>`
  - Request body: `{ "function": string, "account": string | null, "args": object, "dry_run": boolean }`
  - Response body (always HTTP 200, outcome carried in the body like the healthcheck endpoint): the `play_call` JSON verbatim.
- Modifies existing route: `GET /api/connectors/{name}` - each entry in `functions[]` gains `"args_schema": <json schema object> | null`.
- Consumes: `apb_engine::connector_call::play_call` (Task 1).

**Steps:**

- [ ] Write the failing server integration tests first, extending `crates/apb-server/tests/suite/connectors_api_test.rs`. First extend the shared fixture's `list_items` function with an `args_schema` in `write_connector`'s YAML:
  ```rust
  fn write_connector(cfg: &Path) {
      let dir = cfg.join("connectors").join(CONNECTOR);
      std::fs::create_dir_all(&dir).unwrap();
      std::fs::write(
          dir.join("connector.yaml"),
          r#"
  name: mock-tracker
  version: 0.1.0
  healthcheck: ping
  account_fields:
    - name: base_url
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: list_items
      description: List items
      read_only: true
      method: GET
      url: "{{account.base_url}}/items"
      args_schema: { type: object, properties: { q: { type: string } } }
    - name: ping
      description: Reachability check
      mock: { status: 200, body: { ok: true } }
  "#,
      )
      .unwrap();
      std::fs::write(
          dir.join("PUBLIC.md"),
          "---\ndisplay_name: Mock Tracker\nsummary: A fixture connector\ntags: [test]\n---\nBody.\n",
      )
      .unwrap();
  }
  ```

  Then append these tests at the end of the file:
  ```rust
  // --- args_schema exposure (slice 6, spec section 7) ------------------------

  #[tokio::test]
  async fn detail_endpoint_exposes_function_args_schema() {
      let _guard = crate::common::env_lock().await;
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = setup(cfg.path(), root.path());

      let app = build_router(AppState::new(root.path().to_path_buf()));
      let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}")).await;
      assert_eq!(status, StatusCode::OK);
      let functions = json["functions"].as_array().unwrap();
      let list_items = functions
          .iter()
          .find(|f| f["name"] == "list_items")
          .expect("list_items present");
      assert_eq!(
          list_items["args_schema"]["properties"]["q"]["type"],
          serde_json::json!("string"),
          "args_schema must be surfaced verbatim: {list_items}"
      );
      let ping = functions.iter().find(|f| f["name"] == "ping").unwrap();
      assert_eq!(
          ping["args_schema"],
          serde_json::json!(null),
          "a function with no args_schema serializes null, not omitted: {ping}"
      );
  }

  // --- POST /api/connectors/{name}/call (slice 6, spec section 7) -----------

  #[tokio::test]
  async fn call_endpoint_refuses_unapproved_connector_for_real_call() {
      let _guard = crate::common::env_lock().await;
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = setup(cfg.path(), root.path());
      // Neither the connector nor the account is approved.

      let app = build_router(AppState::new(root.path().to_path_buf()));
      let (status, json) = post_json(
          app,
          &format!("/api/connectors/{CONNECTOR}/call"),
          serde_json::json!({ "function": "list_items", "account": "acct1", "args": {}, "dry_run": false }),
      )
      .await;
      assert_eq!(status, StatusCode::OK);
      assert_eq!(json["ok"], serde_json::json!(false), "call: {json}");
      assert_eq!(json["error"]["code"], serde_json::json!("permission"));
  }

  #[tokio::test]
  async fn call_endpoint_dry_run_works_without_approval_or_secrets() {
      let _guard = crate::common::env_lock().await;
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = setup(cfg.path(), root.path());
      // Neither the connector nor the account is approved; TOKEN_VAR is unset.

      let app = build_router(AppState::new(root.path().to_path_buf()));
      let (status, json) = post_json(
          app,
          &format!("/api/connectors/{CONNECTOR}/call"),
          serde_json::json!({ "function": "list_items", "account": "acct1", "args": {}, "dry_run": true }),
      )
      .await;
      assert_eq!(status, StatusCode::OK);
      assert_eq!(json["ok"], serde_json::json!(true), "dry-run call: {json}");
      assert_eq!(json["dry_run"], serde_json::json!(true));
      assert_eq!(json["method"], serde_json::json!("GET"));
      assert_eq!(json["url"], serde_json::json!("https://first.example.com/items"));
  }

  #[tokio::test]
  async fn call_endpoint_real_call_reaches_a_live_mock_http_server() {
      let _guard = crate::common::env_lock().await;
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g_cfg = setup(cfg.path(), root.path());
      let _g_tok = set_var(TOKEN_VAR, "secret-value");

      // Point acct1's base_url at a spawned one-shot mock server instead of
      // the fixture's default unreachable https://first.example.com.
      let server = crate::common::spawn_http(200, "OK", &[], r#"{"items":["a","b"]}"#.to_string());
      let path = config::project_config_path(root.path(), CONNECTOR);
      std::fs::write(
          &path,
          format!(
              "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
              server.base_url
          ),
      )
      .unwrap();
      approve_connector_and_account(root.path(), "acct1");

      let app = build_router(AppState::new(root.path().to_path_buf()));
      let (status, json) = post_json(
          app,
          &format!("/api/connectors/{CONNECTOR}/call"),
          serde_json::json!({ "function": "list_items", "account": "acct1", "args": {}, "dry_run": false }),
      )
      .await;
      assert_eq!(status, StatusCode::OK);
      assert_eq!(json["ok"], serde_json::json!(true), "real call: {json}");
      assert_eq!(json["status"], serde_json::json!(200));
      assert_eq!(json["body"], serde_json::json!({"items": ["a", "b"]}));
      // The fixture declares no response_pick, so `picked` must never read true.
      assert_ne!(json["picked"], serde_json::json!(true), "unexpected pick: {json}");

      let req = server.captured_request().expect("server saw a request");
      assert!(
          req.contains("Authorization: Bearer secret-value"),
          "auth header missing/wrong:\n{req}"
      );
  }

  #[tokio::test]
  async fn call_endpoint_unknown_function_is_config_error() {
      let _guard = crate::common::env_lock().await;
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let _g = setup(cfg.path(), root.path());

      let app = build_router(AppState::new(root.path().to_path_buf()));
      let (status, json) = post_json(
          app,
          &format!("/api/connectors/{CONNECTOR}/call"),
          serde_json::json!({ "function": "no_such_fn", "account": "acct1", "args": {}, "dry_run": true }),
      )
      .await;
      assert_eq!(status, StatusCode::OK);
      assert_eq!(json["ok"], serde_json::json!(false));
      assert_eq!(json["error"]["code"], serde_json::json!("config"));
  }
  ```

- [ ] Add the shared one-shot mock HTTP server helper to `crates/apb-server/tests/suite/common.rs` (adapted from `apb-engine`'s `tests/suite/common/mod.rs`; duplicated because it is test-only code not exported by either crate's library target):
  ```rust
  // --- Ephemeral one-shot HTTP server (mirrors apb-engine's tests/suite/common/mod.rs) ---

  use std::io::{BufRead, BufReader, Read as _, Write as _};
  use std::net::{TcpListener, TcpStream};
  use std::sync::{Arc, Mutex as StdMutex};
  use std::thread::JoinHandle;

  /// A canned one-shot HTTP server on `127.0.0.1:0`: serves a single request
  /// with a fixed response and captures the raw request text for assertions
  /// (e.g. "was the auth header injected"). `base_url` is the
  /// `http://127.0.0.1:<port>` origin to point a connector account's
  /// `base_url` at. The serving thread joins on drop.
  pub struct TestHttpServer {
      pub base_url: String,
      addr: std::net::SocketAddr,
      request: Arc<StdMutex<Option<String>>>,
      handle: Option<JoinHandle<()>>,
  }

  impl TestHttpServer {
      pub fn captured_request(&self) -> Option<String> {
          self.request.lock().unwrap().clone()
      }
  }

  impl Drop for TestHttpServer {
      fn drop(&mut self) {
          if let Some(h) = self.handle.take() {
              let _ = std::net::TcpStream::connect(self.addr);
              let _ = h.join();
          }
      }
  }

  pub fn spawn_http(
      status: u16,
      reason: &str,
      headers: &[(&str, &str)],
      body: String,
  ) -> TestHttpServer {
      let listener = TcpListener::bind("127.0.0.1:0").unwrap();
      let addr = listener.local_addr().unwrap();
      let base_url = format!("http://{addr}");

      let mut head = format!(
          "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n",
          body.len()
      );
      let mut has_ctype = false;
      for (k, v) in headers {
          if k.eq_ignore_ascii_case("content-type") {
              has_ctype = true;
          }
          head.push_str(&format!("{k}: {v}\r\n"));
      }
      if !has_ctype {
          head.push_str("Content-Type: application/json\r\n");
      }
      head.push_str("Connection: close\r\n\r\n");
      let mut response = head.into_bytes();
      response.extend_from_slice(body.as_bytes());

      let request = Arc::new(StdMutex::new(None));
      let req_slot = request.clone();
      let handle = std::thread::spawn(move || {
          if let Ok((mut stream, _)) = listener.accept() {
              let captured = read_http_request(&mut stream);
              *req_slot.lock().unwrap() = Some(captured);
              let _ = stream.write_all(&response);
              let _ = stream.flush();
          }
      });

      TestHttpServer {
          base_url,
          addr,
          request,
          handle: Some(handle),
      }
  }

  fn read_http_request(stream: &mut TcpStream) -> String {
      let mut reader = BufReader::new(stream);
      let mut head = String::new();
      let mut content_length = 0usize;
      loop {
          let mut line = String::new();
          if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
              break;
          }
          if let Some(rest) = line
              .to_ascii_lowercase()
              .strip_prefix("content-length:")
          {
              content_length = rest.trim().parse().unwrap_or(0);
          }
          head.push_str(&line);
      }
      head.push_str("\r\n");
      if content_length > 0 {
          let mut body = vec![0u8; content_length];
          let _ = reader.read_exact(&mut body);
          head.push_str(&String::from_utf8_lossy(&body));
      }
      head
  }
  ```

- [ ] Run and confirm the expected failures (route does not exist yet; `args_schema` assertion fails):
  ```bash
  cargo test -p apb-server --test main connectors_api_test:: 2>&1 | tail -60
  ```

- [ ] Implement the server changes in `crates/apb-server/src/lib.rs`. Add the route (in `build_router`, right after the healthcheck route):
  ```rust
          .route(
              "/api/connectors/{name}/healthcheck/{account}",
              post(healthcheck_connector_handler),
          )
          .route("/api/connectors/{name}/call", post(call_connector_handler))
  ```

  Extend `get_connector_handler`'s `functions` mapping to include `args_schema`:
  ```rust
      let functions: Vec<serde_json::Value> = loaded
          .doc
          .functions
          .iter()
          .map(|f| {
              serde_json::json!({
                  "name": f.name,
                  "description": f.description,
                  "read_only": f.read_only,
                  "deprecated": f.deprecated,
                  "args_schema": f.args_schema,
              })
          })
          .collect();
  ```

  Add the new handler and its request body type, right after `healthcheck_connector_handler`:
  ```rust
  #[derive(Deserialize)]
  struct ConnectorCallBody {
      function: String,
      #[serde(default)]
      account: Option<String>,
      #[serde(default)]
      args: serde_json::Value,
      #[serde(default)]
      dry_run: bool,
  }

  /// POST /api/connectors/{name}/call: the dashboard playground's manual call
  /// (spec 2026-07-19-official-connectors-design section 7). Wraps the same
  /// live execution path the healthcheck probe uses
  /// (`apb_engine::connector_call::play_call`), extended with an arbitrary
  /// function name, args, and a dry-run flag. Like the healthcheck probe, the
  /// server answers HTTP 200 even for a refused or failed call - the outcome
  /// is carried in the body's `ok`/`error`, never as an HTTP error status.
  /// Account defaulting (an omitted or null `account`) is resolved inside
  /// `play_call`, mirroring the CLI's single-or-default selection rule.
  async fn call_connector_handler(
      State(state): State<AppState>,
      AxPath(name): AxPath<String>,
      Query(q): Query<WsQuery>,
      Json(body): Json<ConnectorCallBody>,
  ) -> impl IntoResponse {
      let root = match resolve_root(&state, q.workspace.as_deref()) {
          Ok(r) => r,
          Err(e) => return e,
      };
      // An absent/null `args` in the request body deserializes to
      // `Value::Null`; the executor's schema validation and template
      // rendering both expect an object, so normalize here rather than push
      // that concern into the engine.
      let args = if body.args.is_null() {
          serde_json::json!({})
      } else {
          body.args
      };
      let (value, _ok) = apb_engine::connector_call::play_call(
          &root,
          &name,
          body.account.as_deref(),
          &body.function,
          &args,
          body.dry_run,
      );
      Json(value).into_response()
  }
  ```

- [ ] Run and confirm the tests now pass, then the full crate:
  ```bash
  cargo test -p apb-server --test main connectors_api_test:: 2>&1 | tail -60
  cargo test -p apb-server --test main 2>&1 | tail -20
  ```

- [ ] Format, lint, and code-rank gates (as in Task 1).

- [ ] Commit:
  ```bash
  git add crates/apb-server/src/lib.rs crates/apb-server/tests/suite/common.rs crates/apb-server/tests/suite/connectors_api_test.rs
  git commit --signoff -m "server: add POST /api/connectors/:name/call and expose args_schema"
  ```
  (with the acting-model Co-Authored-By trailer in the message body).

---

### Task 3: Web - API client types and wire mapping

**Files:**
- Modify: `web/src/lib/connectors.ts`
- Modify: `web/src/lib/api.ts`
- Modify: `web/src/lib/api.test.ts`
- Create: `web/src/lib/connectorplay.ts` (type-only stub; Task 4 fills in the logic)

**Interfaces:**
- Produces in `connectors.ts`:
  ```ts
  export interface JsonSchemaProperty {
    type?: string
    enum?: (string | number)[]
    description?: string
    default?: unknown
  }
  export interface JsonSchema {
    type?: string
    properties?: Record<string, JsonSchemaProperty>
    required?: string[]
  }
  export interface ConnectorFunction {
    name: string
    description: string
    readOnly: boolean
    deprecated: boolean
    argsSchema: JsonSchema | null
  }
  ```
- Produces in `api.ts`:
  ```ts
  export interface PlayCallRequest {
    function: string
    account: string | null
    args: Record<string, unknown>
    dryRun: boolean
  }
  export const callConnector: (
    name: string,
    req: PlayCallRequest,
    workspace?: string,
  ) => Promise<PlayCallResult>
  ```

**Steps:**

- [ ] Create the minimal type-only stub `web/src/lib/connectorplay.ts`:
  ```ts
  // Form generation and result rendering for the connector playground panel
  // (spec 2026-07-19-official-connectors-design section 7). Pure functions
  // only, no DOM - the panel component (ConnectorPlaygroundPanel.svelte)
  // stays thin and calls into this module, mirroring connectorstats.ts.

  export interface PlayCallError {
    code: string
    message: string
    http_status?: number
    retry_after_sec?: number
  }

  // The executor's structured outcome, returned verbatim by POST
  // /api/connectors/{name}/call (like HealthcheckResult, no camelCase
  // mapping - this is a passthrough JSON blob, not a fetch-boundary DTO).
  export interface PlayCallResult {
    ok: boolean
    status?: number
    body?: unknown
    truncated?: boolean
    link?: string | null
    picked?: boolean
    dry_run?: boolean
    method?: string
    url?: string
    error?: PlayCallError
  }
  ```

- [ ] Write the failing test first in `web/src/lib/api.test.ts` (add `callConnector` to the existing import list and append a new `describe` block at the end):
  ```ts
  describe('callConnector', () => {
    it('POSTs to /api/connectors/{name}/call with snake_case dry_run', async () => {
      fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, dry_run: true, method: 'GET', url: 'https://x/items', body: null }))
      await callConnector('mock-tracker', {
        function: 'list_items',
        account: 'acct1',
        args: { q: 'hi' },
        dryRun: true,
      })
      expect(fetchMock).toHaveBeenCalledWith('/api/connectors/mock-tracker/call', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ function: 'list_items', account: 'acct1', args: { q: 'hi' }, dry_run: true }),
      })
    })

    it('passes a null account through unchanged (server resolves the default)', async () => {
      fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, dry_run: true, method: 'GET', url: 'https://x/items', body: null }))
      await callConnector('mock-tracker', { function: 'ping', account: null, args: {}, dryRun: true })
      const [, init] = fetchMock.mock.calls[0]
      const sent = JSON.parse((init as RequestInit).body as string)
      expect(sent.account).toBeNull()
    })

    it('adds ?workspace= to select a project on the global dashboard', async () => {
      fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, status: 200, body: {}, truncated: false, link: null, picked: false }))
      await callConnector('mock-tracker', { function: 'ping', account: 'acct1', args: {}, dryRun: false }, 'ws-abc')
      expect(fetchMock).toHaveBeenCalledWith(
        '/api/connectors/mock-tracker/call?workspace=ws-abc',
        expect.objectContaining({ method: 'POST' }),
      )
    })
  })
  ```

- [ ] Run and confirm the expected failure (`callConnector` not exported):
  ```bash
  cd web && bun run test -- api.test.ts 2>&1 | tail -30
  ```

- [ ] Implement. In `web/src/lib/connectors.ts`, add the schema types above `ConnectorFunction` and extend it (shapes above). In `web/src/lib/api.ts`, update imports, DTO, and mapper:
  ```ts
  interface ConnectorFunctionDto {
    name: string
    description: string
    read_only: boolean
    deprecated: boolean
    args_schema?: JsonSchema | null
  }

  const toConnectorFunction = (d: ConnectorFunctionDto): ConnectorFunction => ({
    name: d.name,
    description: d.description,
    readOnly: d.read_only,
    deprecated: d.deprecated,
    argsSchema: d.args_schema ?? null,
  })
  ```

  Append at the end of `web/src/lib/api.ts` (after `fetchConnectorStats`):
  ```ts
  export interface PlayCallRequest {
    function: string
    account: string | null
    args: Record<string, unknown>
    dryRun: boolean
  }

  interface PlayCallRequestDto {
    function: string
    account: string | null
    args: Record<string, unknown>
    dry_run: boolean
  }

  // POST /api/connectors/{name}/call: the dashboard playground's manual call
  // (design doc 2026-07-19-official-connectors-design section 7). Wraps the
  // same live execution path the healthcheck probe uses, extended with an
  // arbitrary function, args, and a dry-run flag. Like the healthcheck probe,
  // the server answers HTTP 200 even for a refused or failed call - the
  // outcome is carried in the body's `ok`/`error`, never as an HTTP error.
  export const callConnector = (name: string, req: PlayCallRequest, workspace = '') =>
    requestJson<PlayCallResult>(`${conn(name)}/call${qs({ workspace })}`, {
      method: 'POST',
      headers: jsonHeaders,
      body: JSON.stringify({
        function: req.function,
        account: req.account,
        args: req.args,
        dry_run: req.dryRun,
      } satisfies PlayCallRequestDto),
    })
  ```
  with `import type { PlayCallResult } from './connectorplay'` added to the imports.

- [ ] Run and confirm all tests pass, plus the type checker:
  ```bash
  cd web && bun run test -- api.test.ts 2>&1 | tail -30
  cd web && bun run check 2>&1 | tail -40
  ```

- [ ] Commit:
  ```bash
  git add web/src/lib/connectors.ts web/src/lib/api.ts web/src/lib/api.test.ts web/src/lib/connectorplay.ts
  git commit --signoff -m "web: add callConnector API client and expose function args_schema"
  ```
  (with the acting-model Co-Authored-By trailer in the message body).

---

### Task 4: Web - playground pure logic (form generation, result rendering)

**Files:**
- Modify: `web/src/lib/connectorplay.ts` (fills in the stub from Task 3)
- Create: `web/src/lib/connectorplay.test.ts`

**Interfaces:**
- Produces:
  ```ts
  export type PlayFieldKind = 'string' | 'number' | 'boolean' | 'enum' | 'unsupported'
  export interface PlayField {
    name: string
    kind: PlayFieldKind
    required: boolean
    description?: string
    enumValues?: (string | number)[]
  }
  export function isSimpleObjectSchema(schema: JsonSchema | null | undefined): boolean
  export function buildPlayFields(schema: JsonSchema | null | undefined): PlayField[]
  export function coerceFormValues(fields: PlayField[], values: Record<string, string | boolean>): Record<string, unknown>
  export function parseRawArgs(text: string): Record<string, unknown>
  export function resultSummary(r: PlayCallResult): string
  export function formatResultBody(r: PlayCallResult): string
  ```

**Steps:**

- [ ] Write the failing test file `web/src/lib/connectorplay.test.ts` covering: `isSimpleObjectSchema` (simple leaves accepted; nested object rejected; non-object top level rejected; null/undefined rejected), `buildPlayFields` (one field per property with required marking; boolean kind; enum kind with values; empty for no properties), `coerceFormValues` (strings pass through; number parsing; empty optional omitted; boolean always included; invalid number dropped), `parseRawArgs` (empty -> {}; valid object; descriptive error on invalid JSON; error on non-object top level), `resultSummary` (dry run; real success; error with and without http_status), `formatResultBody` (success body; error object; dry-run body) - the full test code as authored by the planning agent, asserting exact strings like `'dry run: GET https://x/items'`, `'200 ok'`, `'auth (HTTP 401)'`, `'permission'`.

- [ ] Run and confirm the expected failures:
  ```bash
  cd web && bun run test -- connectorplay.test.ts 2>&1 | tail -40
  ```

- [ ] Implement the full `web/src/lib/connectorplay.ts` (replacing the Task 3 stub, keeping `PlayCallError`/`PlayCallResult`):
  ```ts
  import type { JsonSchema, JsonSchemaProperty } from './connectors'

  export type PlayFieldKind = 'string' | 'number' | 'boolean' | 'enum' | 'unsupported'

  export interface PlayField {
    name: string
    kind: PlayFieldKind
    required: boolean
    description?: string
    enumValues?: (string | number)[]
  }

  function fieldKind(prop: JsonSchemaProperty): PlayFieldKind {
    if (Array.isArray(prop.enum) && prop.enum.length > 0) return 'enum'
    switch (prop.type) {
      case 'string':
        return 'string'
      case 'number':
      case 'integer':
        return 'number'
      case 'boolean':
        return 'boolean'
      default:
        return 'unsupported'
    }
  }

  // Whether a schema is simple enough for the generated form: object type,
  // with properties, all of which are simple leaves (string/number/boolean/
  // enum). Anything else falls back to the raw JSON textarea; the form
  // generator does not attempt partial coverage of a complex schema.
  export function isSimpleObjectSchema(schema: JsonSchema | null | undefined): boolean {
    if (!schema || schema.type !== 'object' || !schema.properties) return false
    return Object.values(schema.properties).every((p) => fieldKind(p) !== 'unsupported')
  }

  // Builds the ordered field list for the generated form from an args_schema.
  export function buildPlayFields(schema: JsonSchema | null | undefined): PlayField[] {
    if (!schema?.properties) return []
    const required = new Set(schema.required ?? [])
    return Object.entries(schema.properties).map(([name, prop]) => ({
      name,
      kind: fieldKind(prop),
      required: required.has(name),
      description: prop.description,
      enumValues: prop.enum,
    }))
  }

  // Coerces the raw values the form widgets produce into the JSON-typed args
  // object a call expects. An empty string on a non-required field is
  // omitted; an unparsable number is omitted rather than sent as NaN.
  export function coerceFormValues(
    fields: PlayField[],
    values: Record<string, string | boolean>,
  ): Record<string, unknown> {
    const out: Record<string, unknown> = {}
    for (const field of fields) {
      const raw = values[field.name]
      if (field.kind === 'boolean') {
        out[field.name] = raw === true
        continue
      }
      if (raw === undefined || raw === '') continue
      if (field.kind === 'number') {
        const n = Number(raw)
        if (!Number.isNaN(n)) out[field.name] = n
        continue
      }
      out[field.name] = raw
    }
    return out
  }

  // Parses the raw-JSON textarea fallback. Empty text is an empty args
  // object. Throws a descriptive error on invalid JSON or a non-object top
  // level - connector args are always an object.
  export function parseRawArgs(text: string): Record<string, unknown> {
    const trimmed = text.trim()
    if (trimmed === '') return {}
    let parsed: unknown
    try {
      parsed = JSON.parse(trimmed)
    } catch (e) {
      throw new Error(`invalid JSON: ${String(e instanceof Error ? e.message : e)}`)
    }
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
      throw new Error('args must be a JSON object')
    }
    return parsed as Record<string, unknown>
  }

  // --- result rendering ------------------------------------------------------

  export interface PlayCallError {
    code: string
    message: string
    http_status?: number
    retry_after_sec?: number
  }

  export interface PlayCallResult {
    ok: boolean
    status?: number
    body?: unknown
    truncated?: boolean
    link?: string | null
    picked?: boolean
    dry_run?: boolean
    method?: string
    url?: string
    error?: PlayCallError
  }

  // A one-line status summary for the result panel header.
  export function resultSummary(r: PlayCallResult): string {
    if (r.dry_run) return `dry run: ${r.method ?? ''} ${r.url ?? ''}`.trim()
    if (r.ok) return `${r.status ?? ''} ok`.trim()
    const status = r.error?.http_status ? ` (HTTP ${r.error.http_status})` : ''
    return `${r.error?.code ?? 'error'}${status}`
  }

  // Pretty-prints the body (success or dry run) or the error object
  // (failure) for the result panel, 2-space indented JSON.
  export function formatResultBody(r: PlayCallResult): string {
    if (!r.ok) return JSON.stringify(r.error ?? {}, null, 2)
    return JSON.stringify(r.body ?? null, null, 2)
  }
  ```

- [ ] Run and confirm all tests pass:
  ```bash
  cd web && bun run test -- connectorplay.test.ts 2>&1 | tail -40
  cd web && bun run test 2>&1 | tail -30
  cd web && bun run check 2>&1 | tail -40
  ```

- [ ] Commit:
  ```bash
  git add web/src/lib/connectorplay.ts web/src/lib/connectorplay.test.ts
  git commit --signoff -m "web: add connectorplay pure logic for the playground panel"
  ```
  (with the acting-model Co-Authored-By trailer in the message body).

---

### Task 5: Web - `ConnectorPlaygroundPanel.svelte` component and `ConnectorView.svelte` wiring

**Files:**
- Create: `web/src/lib/components/ConnectorPlaygroundPanel.svelte`
- Modify: `web/src/pages/ConnectorView.svelte`

**Interfaces:**
- Consumes: `connectorplay.ts` (Task 4), `api.ts`'s `callConnector` (Task 3), `connectors.ts`'s `ConnectorFunction`/`ConnectorAccount`.
- Component props:
  ```ts
  let {
    name,
    workspace = '',
    functions,
    accounts,
  }: { name: string; workspace?: string; functions: ConnectorFunction[]; accounts: ConnectorAccount[] } = $props()
  ```

**Steps:**

- [ ] Create `web/src/lib/components/ConnectorPlaygroundPanel.svelte` per the planning agent's full component code: shadcn-svelte Card with a FlaskConical-headed "Playground" panel; function Select (monospace, description below); account Select (default marked, preselected via `$effect`); a dry-run Switch defaulting ON and a raw-JSON Switch shown only when the schema supports a form; the generated form (Input for string/number, Switch for boolean, Select for enum, required markers, descriptions) or a monospace Textarea in raw mode; an args error line; a Call/Render Button with Spinner while calling; and a result block showing a tone-colored summary Badge (`resultSummary`), `picked`/`truncated` badges, the `link` value, and a `<pre>` with `formatResultBody`. State via Svelte 5 runes (`$state`/`$derived`/`$effect`), three effects: keep function selection valid, preselect default account, reset form/result on function change. Errors surface via `svelte-sonner` toasts. Exact code as authored in the agent's plan output (session transcript); adjust shadcn-svelte component prop names if `bun run check` reports mismatches against the installed version.

- [ ] Wire the panel into `web/src/pages/ConnectorView.svelte`: add the import
  ```svelte
    import ConnectorPlaygroundPanel from '$lib/components/ConnectorPlaygroundPanel.svelte'
  ```
  and insert between the Accounts card and the Usage card:
  ```svelte
      <ConnectorPlaygroundPanel {name} {workspace} functions={detail.functions} accounts={detail.accounts} />
  ```

- [ ] Run the full web test suite, type check, and build:
  ```bash
  cd web && bun run test 2>&1 | tail -40
  cd web && bun run check 2>&1 | tail -60
  cd web && bun run build 2>&1 | tail -40
  ```

- [ ] Manual smoke check with the `run` skill or a local `apb serve` against a project with an installed test connector: open the connector detail page, confirm the Playground card renders, pick a function with args, fill the form, toggle dry-run off/on, click Call/Render, and confirm the result panel shows status/body or error/picked/link as expected.

- [ ] Commit:
  ```bash
  git add web/src/lib/components/ConnectorPlaygroundPanel.svelte web/src/pages/ConnectorView.svelte
  git commit --signoff -m "web: add the connector detail page playground panel"
  ```
  (with the acting-model Co-Authored-By trailer in the message body).

---

## Definition of done for this slice

- `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `code-ranker check .` are all clean.
- `cd web && bun run test`, `bun run check`, and `bun run build` are all clean.
- `POST /api/connectors/:name/call` exists, is trust-gated identically to the healthcheck probe for a real call, and dry-run works with no connector/account approval and no secrets.
- The connector detail page's playground panel calls a function end to end against a real (test) connector, dry-run and live, and renders status/body/error/link/picked correctly.
