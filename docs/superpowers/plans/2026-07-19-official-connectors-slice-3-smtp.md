# Official Connectors Slice 3: SMTP Function Kind - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a third connector function kind, `smtp`, executed natively by the engine over the `lettre` crate, so a declarative connector manifest can send email and probe an SMTP server without HTTP and without a code-execution escape hatch. Covers the `SmtpSpec` schema, the extended secret-placement policy, native send/verify execution with STARTTLS, the call-result and error taxonomy for SMTP, dry-run envelope rendering, and body/credential-free event logging.

**Architecture:** `apb-core` gains the `SmtpSpec`/`SmtpConnection`/`SmtpMessage` schema types on `FunctionSpec` plus the three-way (HTTP xor mock xor smtp) validation and the template-namespace/secret-placement extension. `apb-engine` gains a new `connector_smtp` module holding all SMTP rendering, address validation, dry-run shaping, lettre execution and error mapping; `connector_call.rs` grows one branch in `build_prepared`, one `PreparedCall::Smtp` variant, and refactors `CallOk` into a two-shape enum so SMTP can return `{ ok, body }` while HTTP keeps `{ ok, status, body, truncated }`. `event.rs` gains two `#[serde(default)]` fields for the SMTP subject and recipient count; host and port ride in the existing `url` field as an `smtp://host:port` endpoint.

**Tech Stack:** Rust edition 2024, blocking execution (the `apb connector call` process is synchronous, like the existing ureq path), `lettre` 0.11 with the blocking low-level `SmtpConnection` client and `rustls-tls`, `serde`/`serde_yaml_ng`/`serde_json`, `jsonschema`, existing `apb_core::connector::template` renderer.

## Global Constraints

Copied verbatim from CLAUDE.md and the shared contract; every task must hold to these:

- No em-dash (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere in code or prose. Machine-facing fields are English.
- New `EventPayload` fields are added only with `#[serde(default)]`.
- Secret values (auth files, SMTP passwords) are never returned, logged, or cached. The event log records host, port, subject, and recipient count only, never message bodies and never credentials.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- Before commit, the code-ranker check must pass. Warm the cargo cache first: `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .`; for any violation, read `code-ranker docs base <ID>`, fix, and re-run until clean.
- Commit only after the owner approves. Commits use `git commit --signoff` and end with a `Co-Authored-By` trailer for the acting model.

---

### Task 1: SMTP schema types and the three-way function-kind validation

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs`

**Interfaces:**
- Produces `pub struct SmtpSpec { connection: SmtpConnection, message: Option<SmtpMessage>, verify: bool }`, `pub struct SmtpConnection { host, port, use_tls, username: Option<String>, password: Option<String> }`, `pub struct SmtpMessage { from_email, from_name: Option<String>, to, cc: Option<String>, bcc: Option<String>, subject, body_text: Option<String>, body_html: Option<String> }`, all `#[serde(deny_unknown_fields)]`, all inner values `String` (template strings).
- Produces `FunctionSpec { ..., #[serde(default)] pub smtp: Option<SmtpSpec> }` and `FunctionSpec::is_smtp(&self) -> bool`.
- Consumes the existing `is_http`/`is_mock` xor logic in `ConnectorDoc::from_yaml`, extended to three-way.

Steps:

- [ ] Write a failing test in the `def.rs` `tests` module for a valid `smtp` send function and a valid `verify` function:
  ```rust
  const SMTP_YAML: &str = r#"
  name: smtp
  version: 0.1.0
  healthcheck: verify
  account_fields:
    - name: host
      required: true
    - name: port
      required: true
    - name: use_tls
    - name: username
    - name: from_email
      required: true
    - name: from_name
    - name: password
      required: true
      secret: true
  functions:
    - name: send_email
      description: Send an email over SMTP
      smtp:
        connection:
          host: "{{account.host}}"
          port: "{{account.port}}"
          use_tls: "{{account.use_tls}}"
          username: "{{account.username}}"
          password: "{{secret.password}}"
        message:
          from_email: "{{account.from_email}}"
          from_name: "{{account.from_name}}"
          to: "{{args.to}}"
          cc: "{{args.cc}}"
          bcc: "{{args.bcc}}"
          subject: "{{args.subject}}"
          body_text: "{{args.body_text}}"
          body_html: "{{args.body_html}}"
      args_schema: { type: object, properties: { to: { type: string }, subject: { type: string } }, required: [to, subject] }
    - name: verify
      description: Probe the SMTP connection without sending
      read_only: true
      smtp:
        connection:
          host: "{{account.host}}"
          port: "{{account.port}}"
          use_tls: "{{account.use_tls}}"
          username: "{{account.username}}"
          password: "{{secret.password}}"
        verify: true
  "#;

  #[test]
  fn parses_smtp_send_and_verify() {
      let doc = ConnectorDoc::from_yaml(SMTP_YAML, "smtp").unwrap();
      let send = doc.function("send_email").unwrap();
      assert!(send.is_smtp());
      assert!(!send.is_mock());
      let smtp = send.smtp.as_ref().unwrap();
      assert!(!smtp.verify);
      assert!(smtp.message.is_some());
      let verify = doc.function("verify").unwrap();
      assert!(verify.is_smtp());
      assert!(verify.read_only);
      assert!(verify.smtp.as_ref().unwrap().verify);
      assert!(verify.smtp.as_ref().unwrap().message.is_none());
  }

  #[test]
  fn rejects_function_that_is_both_smtp_and_http() {
      let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n";
      assert!(ConnectorDoc::from_yaml(y, "x").is_err());
  }

  #[test]
  fn rejects_verify_with_message_and_send_without_message() {
      let with_msg = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n      message: { from_email: a@b.c, to: \"{{args.to}}\", subject: \"{{args.subject}}\" }\n";
      assert!(ConnectorDoc::from_yaml(with_msg, "x").is_err());
      let no_msg = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n";
      assert!(ConnectorDoc::from_yaml(no_msg, "x").is_err());
  }
  ```
- [ ] Run it and watch it fail to compile: `cargo test -p apb-core connector::def::tests::parses_smtp_send_and_verify` (expected: `no field 'smtp' on type FunctionSpec`, `no method 'is_smtp'`).
- [ ] Add the schema types above the `FunctionSpec` definition:
  ```rust
  /// The SMTP connection block: host/port/TLS plus optional credentials. Every
  /// value is a template string. `password` is the one non-`auth` location the
  /// secret-placement policy allows `{{secret.*}}` (spec 4.2).
  #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct SmtpConnection {
      pub host: String,
      pub port: String,
      pub use_tls: String,
      #[serde(default)]
      pub username: Option<String>,
      #[serde(default)]
      pub password: Option<String>,
  }

  /// The SMTP message block. `to`/`cc`/`bcc` are comma-separated address lists.
  /// Message templates follow function-body rules: only `account.*` and
  /// `args.*`, never `secret.*`.
  #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct SmtpMessage {
      pub from_email: String,
      #[serde(default)]
      pub from_name: Option<String>,
      pub to: String,
      #[serde(default)]
      pub cc: Option<String>,
      #[serde(default)]
      pub bcc: Option<String>,
      pub subject: String,
      #[serde(default)]
      pub body_text: Option<String>,
      #[serde(default)]
      pub body_html: Option<String>,
  }

  /// The `smtp` function kind (spec 4.2). Exactly one of a connector's function
  /// kinds. `verify: true` probes the connection and carries no `message`;
  /// `verify: false` (the default) sends the `message`.
  #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct SmtpSpec {
      pub connection: SmtpConnection,
      #[serde(default)]
      pub message: Option<SmtpMessage>,
      #[serde(default)]
      pub verify: bool,
  }
  ```
- [ ] Add `#[serde(default)] pub smtp: Option<SmtpSpec>` to `FunctionSpec` (after `mock`), and add `pub fn is_smtp(&self) -> bool { self.smtp.is_some() }` alongside `is_mock`.
- [ ] Replace the two-way `match (is_http, is_mock)` in `from_yaml` with a three-way exclusivity check plus SMTP-internal rules:
  ```rust
  let is_http = f.method.is_some() || f.url.is_some();
  let is_mock = f.mock.is_some();
  let is_smtp = f.smtp.is_some();
  match (is_http, is_mock, is_smtp) {
      (false, false, false) => {
          return Err(ConnectorError::Invalid(format!(
              "function `{}` is not an HTTP call (method + url), a mock, or an smtp block",
              f.name
          )));
      }
      (true, false, false) => {
          if f.method.is_none() || f.url.is_none() {
              return Err(ConnectorError::Invalid(format!(
                  "function `{}` must set both `method` and `url`",
                  f.name
              )));
          }
      }
      (false, true, false) => {
          if !f.query.is_empty() || f.body.is_some() {
              return Err(ConnectorError::Invalid(format!(
                  "mock function `{}` must not set `query` or `body`",
                  f.name
              )));
          }
      }
      (false, false, true) => validate_smtp_shape(f)?,
      _ => {
          return Err(ConnectorError::Invalid(format!(
              "function `{}` must be exactly one of: an HTTP call, a mock, or an smtp block",
              f.name
          )));
      }
  }
  ```
- [ ] Add the SMTP-shape helper below `from_yaml` (verify xor message, and no HTTP-only fields):
  ```rust
  /// SMTP-internal shape rules (spec 4.2): a `verify: true` function carries no
  /// `message`; a `verify: false` function must carry one. An smtp function must
  /// not also carry `query` or `body` (those are HTTP-only).
  fn validate_smtp_shape(f: &FunctionSpec) -> Result<(), ConnectorError> {
      let smtp = f.smtp.as_ref().expect("caller checked is_smtp");
      if !f.query.is_empty() || f.body.is_some() {
          return Err(ConnectorError::Invalid(format!(
              "smtp function `{}` must not set `query` or `body`",
              f.name
          )));
      }
      match (smtp.verify, smtp.message.is_some()) {
          (true, true) => Err(ConnectorError::Invalid(format!(
              "smtp function `{}` sets `verify: true` and must not carry a `message`",
              f.name
          ))),
          (false, false) => Err(ConnectorError::Invalid(format!(
              "smtp function `{}` must carry a `message` (or set `verify: true`)",
              f.name
          ))),
          _ => Ok(()),
      }
  }
  ```
- [ ] Run the tests to green: `cargo test -p apb-core connector::def`.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, warm cache then `code-ranker check .`.
- [ ] Commit: `git commit --signoff -m "core: add smtp function kind schema and three-way kind validation"` (with the Co-Authored-By trailer).

---

### Task 2: SMTP secret-placement and template-namespace validation

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs` (`validate_templates` and helpers)

**Interfaces:**
- Consumes `crate::connector::template::{Namespace, placeholders}` and the existing `FieldNames`/`reject_secret` helpers.
- Produces: `{{secret.*}}` allowed only in `auth` and in `smtp.connection.password`; `smtp.connection` allows `account.*`/`secret.*` (no `args.*`); `smtp.message` allows only `account.*`/`args.*` (no `secret.*`).

Steps:

- [ ] Add failing tests to the `def.rs` `tests` module:
  ```rust
  #[test]
  fn smtp_password_allows_secret_placeholder() {
      assert!(ConnectorDoc::from_yaml(SMTP_YAML, "smtp").is_ok());
  }

  #[test]
  fn secret_in_smtp_message_is_rejected() {
      let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: password\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      message: { from_email: a@b.c, to: \"{{secret.password}}\", subject: s }\n";
      let err = ConnectorDoc::from_yaml(y, "x").unwrap_err();
      assert!(err.to_string().contains("secret"), "message was: {err}");
  }

  #[test]
  fn secret_in_smtp_host_is_rejected() {
      let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: \"{{secret.token}}\", port: \"25\", use_tls: \"false\" }\n      verify: true\n";
      assert!(ConnectorDoc::from_yaml(y, "x").is_err());
  }

  #[test]
  fn args_in_smtp_connection_is_rejected() {
      let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: \"{{args.host}}\", port: \"25\", use_tls: \"false\" }\n      verify: true\n";
      assert!(ConnectorDoc::from_yaml(y, "x").is_err());
  }

  #[test]
  fn unknown_account_field_in_smtp_message_is_rejected() {
      let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      message: { from_email: \"{{account.nope}}\", to: \"{{args.to}}\", subject: s }\n";
      assert!(ConnectorDoc::from_yaml(y, "x").is_err());
  }
  ```
- [ ] Run and watch fail: `cargo test -p apb-core connector::def::tests::secret_in_smtp_message_is_rejected` (currently `validate_templates` ignores `f.smtp`, so `{{secret.*}}` in the message is silently accepted).
- [ ] In `validate_templates`, inside the `for f in &doc.functions` loop, after the existing `url`/`query`/`body` handling add:
  ```rust
  if let Some(smtp) = &f.smtp {
      validate_smtp_templates(smtp, &f.name, &fields)?;
  }
  ```
- [ ] Add the SMTP template walker near `validate_body_templates`:
  ```rust
  /// Validates the placeholders in an `smtp` function. The `connection` block
  /// follows auth-adjacent rules: `account.*` and `secret.password` are allowed
  /// (`secret.*` only via the `password` field), `args.*` is rejected. The
  /// `message` block follows function-body rules: `account.*` and `args.*` only,
  /// `secret.*` rejected.
  fn validate_smtp_templates(
      smtp: &SmtpSpec,
      function_name: &str,
      fields: &FieldNames,
  ) -> Result<(), ConnectorError> {
      use crate::connector::template::{Namespace, placeholders};

      // Connection: account/secret allowed, args rejected. Only `password` may
      // carry a secret; a secret placeholder anywhere else in `connection` is a
      // hard error.
      let conn = &smtp.connection;
      let non_password: [&str; 3] = [conn.host.as_str(), conn.port.as_str(), conn.use_tls.as_str()];
      for template in non_password
          .iter()
          .copied()
          .chain(conn.username.as_deref())
      {
          for (ns, name) in placeholders(template)? {
              if ns == Namespace::Args {
                  return Err(ConnectorError::Invalid(format!(
                      "args placeholders are not allowed in smtp connection of function `{function_name}`"
                  )));
              }
              reject_secret(ns, &format!("function `{function_name}` smtp connection"))?;
              fields.check(ns, &name)?;
          }
      }
      if let Some(password) = &conn.password {
          for (ns, name) in placeholders(password)? {
              if ns == Namespace::Args {
                  return Err(ConnectorError::Invalid(format!(
                      "args placeholders are not allowed in smtp connection password of function `{function_name}`"
                  )));
              }
              fields.check(ns, &name)?;
          }
      }

      // Message: account/args only, secret rejected.
      if let Some(msg) = &smtp.message {
          let strings: [Option<&str>; 8] = [
              Some(msg.from_email.as_str()),
              msg.from_name.as_deref(),
              Some(msg.to.as_str()),
              msg.cc.as_deref(),
              msg.bcc.as_deref(),
              Some(msg.subject.as_str()),
              msg.body_text.as_deref(),
              msg.body_html.as_deref(),
          ];
          for template in strings.into_iter().flatten() {
              for (ns, name) in placeholders(template)? {
                  reject_secret(ns, &format!("function `{function_name}` smtp message"))?;
                  fields.check(ns, &name)?;
              }
          }
      }
      Ok(())
  }
  ```
  NOTE (cross-slice): slice 1 adds `Namespace::Auth`; when both slices are in, `{{auth}}` must also be rejected here (add a `reject_auth` call mirroring the query/body loops). The slice that lands second owns that arm.
- [ ] Run to green: `cargo test -p apb-core connector::def`.
- [ ] fmt, clippy, code-ranker as above.
- [ ] Commit: `git commit --signoff -m "core: extend secret-placement policy to smtp connection password and message"`.

---

### Task 3: Reject response_pick on smtp functions (guarded on slice 1)

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs`

**Interfaces:**
- Consumes `FunctionSpec::response_pick` (added by slice 1 "Format and engine"). This slice and slice 1 are independent; whichever lands second owns this cross-check.

Steps:

- [ ] Check whether `response_pick` exists yet: `grep -n "response_pick" crates/apb-core/src/connector/def.rs`.
- [ ] If it is absent (slice 1 has not landed): do nothing in code; the slice that lands second must reject response_pick on any `is_smtp()`/`is_mock()` function in `ConnectorDoc::from_yaml`. Leave the code steps below unchecked with a note "blocked on slice 1".
- [ ] If `response_pick` exists (slice 1 landed first): write a failing test:
  ```rust
  #[test]
  fn response_pick_on_smtp_is_rejected() {
      let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    response_pick: [a, b]\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n";
      let err = ConnectorDoc::from_yaml(y, "x").unwrap_err();
      assert!(err.to_string().contains("response_pick"), "message was: {err}");
  }
  ```
- [ ] Run and watch fail: `cargo test -p apb-core connector::def::tests::response_pick_on_smtp_is_rejected`.
- [ ] In `from_yaml`, in the per-function loop, after the kind match add (guarding both non-HTTP kinds, matching the spec 4.5 wording that response_pick is a validation error on mock and smtp):
  ```rust
  if !f.response_pick.is_empty() && (f.is_mock() || f.is_smtp()) {
      return Err(ConnectorError::Invalid(format!(
          "function `{}` sets `response_pick` but is not an HTTP function; \
           response_pick projects HTTP response bodies only",
          f.name
      )));
  }
  ```
- [ ] Run to green, fmt, clippy, code-ranker.
- [ ] Commit: `git commit --signoff -m "core: reject response_pick on smtp and mock functions"`.

---

### Task 4: SMTP metadata fields on the ConnectorCall event

**Files:**
- Modify: `crates/apb-engine/src/event.rs`

**Interfaces:**
- Produces two new `#[serde(default)]` fields on `EventPayload::ConnectorCall`: `smtp_subject: Option<String>`, `smtp_recipients: Option<u32>`. Host and port ride in the existing `url` field as an `smtp://host:port` endpoint (no credentials).

Steps:

- [ ] Add a failing round-trip test in `crates/apb-engine/tests/suite/event_test.rs` (a module that already exercises event serde):
  ```rust
  #[test]
  fn connector_call_smtp_fields_round_trip() {
      use apb_engine::event::EventPayload;
      let p = EventPayload::ConnectorCall {
          node_id: "n".into(),
          connector: "smtp".into(),
          function: "send_email".into(),
          account: "acct1".into(),
          url: "smtp://smtp.example.com:587".into(),
          outcome: "ok".into(),
          http_status: None,
          duration_ms: 12,
          smtp_subject: Some("Hi".into()),
          smtp_recipients: Some(2),
      };
      let s = serde_json::to_string(&p).unwrap();
      assert!(!s.contains("password"));
      let back: EventPayload = serde_json::from_str(&s).unwrap();
      assert_eq!(format!("{back:?}"), format!("{p:?}"));
  }

  #[test]
  fn old_connector_call_without_smtp_fields_still_parses() {
      // A log line written before this slice: the new fields default to None.
      let json = r#"{"kind":"connector_call","node_id":"n","connector":"c","function":"f","account":"a","url":"","outcome":"ok","http_status":null,"duration_ms":1}"#;
      let _p: apb_engine::event::EventPayload = serde_json::from_str(json).unwrap();
  }
  ```
  (Adjust the `kind` tag literal in the second test to match the crate's `EventPayload` serde tag; confirm with `grep -n "tag =" crates/apb-engine/src/event.rs`.)
- [ ] Run and watch fail to compile: `cargo test -p apb-engine --test main event_test::connector_call_smtp_fields_round_trip` (missing struct fields).
- [ ] Add the two fields to `EventPayload::ConnectorCall`, after `duration_ms`:
  ```rust
  /// SMTP-only: the message subject and total recipient count. `None` for HTTP
  /// and mock calls and for smtp `verify`. Bodies and credentials are never
  /// recorded (spec 4.2).
  #[serde(default)]
  smtp_subject: Option<String>,
  #[serde(default)]
  smtp_recipients: Option<u32>,
  ```
- [ ] Run to green.
- [ ] fmt, clippy, code-ranker.
- [ ] Commit: `git commit --signoff -m "engine: add smtp subject and recipient-count fields to the ConnectorCall event"`.

---

### Task 5: SMTP rendering, address validation, dry-run, and the CallOk shape split

**Files:**
- Modify: root `Cargo.toml` (`[workspace.dependencies]`), `crates/apb-engine/Cargo.toml`
- Create: `crates/apb-engine/src/connector_smtp.rs`
- Modify: `crates/apb-engine/src/lib.rs`, `crates/apb-engine/src/connector_call.rs`

**Interfaces:**
- Consumes `apb_core::connector::def::{SmtpSpec, SmtpConnection, SmtpMessage}`, `apb_core::connector::template::{RenderCtx, render_raw}`, `lettre::message::Mailbox`, `lettre::Address`.
- Produces in `connector_smtp`: `pub(crate) enum SmtpBuild { DryRun(serde_json::Value), Call(Box<SmtpCall>) }`, `pub(crate) fn build(spec: &SmtpSpec, account: &BTreeMap<String,String>, args: &Value, secrets: &BTreeMap<String,String>, redactions: Vec<(String,String)>, dry_run: bool, timeout_sec: u64) -> Result<SmtpBuild, CallError>`, `pub(crate) struct SmtpCall`.
- Produces in `connector_call`: `pub enum CallOk { Http { status: u16, body: Value, truncated: bool }, Smtp { body: Value } }` replacing the struct, plus `PreparedCall::Smtp(Box<SmtpCall>)`.
- CROSS-SLICE NOTE: slice 1 adds `link`/`picked` to the HTTP success shape. If slice 1 lands first, the `Http` variant carries `link: Option<String>` and `picked: bool` too; if this slice lands first, slice 1 adds them to the enum variant instead of a struct. The master plan tracks this.

Steps:

- [ ] Add the dependency. In root `Cargo.toml` `[workspace.dependencies]`:
  ```toml
  lettre = { version = "0.11", default-features = false, features = ["builder", "smtp-transport", "rustls-tls", "hostname"] }
  ```
  In `crates/apb-engine/Cargo.toml` `[dependencies]`:
  ```toml
  # Native SMTP execution for the `smtp` connector function kind (spec 4.2):
  # blocking low-level SmtpConnection client, STARTTLS via rustls (TLS 1.2+ by
  # default), MIME/multipart built by the library.
  lettre.workspace = true
  ```
- [ ] Register the module: add `pub mod connector_smtp;` to `crates/apb-engine/src/lib.rs` (keep the list alphabetical: after `connector_run`).
- [ ] Write failing unit tests at the bottom of the new `connector_smtp.rs` (address validation and dry-run render, both network-free):
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use std::collections::BTreeMap;
      use serde_json::json;

      fn spec_send() -> apb_core::connector::def::SmtpSpec {
          serde_yaml_ng::from_str(
              "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nmessage:\n  from_email: \"{{account.from_email}}\"\n  from_name: \"{{account.from_name}}\"\n  to: \"{{args.to}}\"\n  cc: \"{{args.cc}}\"\n  subject: \"{{args.subject}}\"\n  body_text: \"{{args.body_text}}\"\n  body_html: \"{{args.body_html}}\"\n",
          ).unwrap()
      }

      fn account() -> BTreeMap<String, String> {
          BTreeMap::from([
              ("host".into(), "smtp.example.com".into()),
              ("port".into(), "587".into()),
              ("use_tls".into(), "true".into()),
              ("username".into(), "u".into()),
              ("from_email".into(), "a@b.c".into()),
              ("from_name".into(), "Alice".into()),
          ])
      }

      #[test]
      fn dry_run_renders_envelope_without_secrets() {
          let spec = spec_send();
          let args = json!({"to": "x@y.z, w@y.z", "cc": "c@y.z", "subject": "Hi", "body_text": "T"});
          let out = match build(&spec, &account(), &args, &BTreeMap::new(), Vec::new(), true, 30).unwrap() {
              SmtpBuild::DryRun(v) => v,
              _ => panic!("expected dry-run"),
          };
          assert_eq!(out["dry_run"], json!(true));
          assert_eq!(out["envelope"]["from"], json!("a@b.c"));
          assert_eq!(out["envelope"]["to"], json!(["x@y.z", "w@y.z"]));
          assert_eq!(out["envelope"]["cc"], json!(["c@y.z"]));
          assert_eq!(out["envelope"]["subject"], json!("Hi"));
      }

      #[test]
      fn bad_address_is_invalid_args() {
          let spec = spec_send();
          let args = json!({"to": "not-an-email", "subject": "Hi", "body_text": "T"});
          let err = build(&spec, &account(), &args, &BTreeMap::new(), Vec::new(), true, 30).unwrap_err();
          assert_eq!(err.code, CallErrorCode::InvalidArgs);
      }

      #[test]
      fn missing_body_is_invalid_args() {
          let spec = spec_send();
          let args = json!({"to": "x@y.z", "subject": "Hi"});
          let err = build(&spec, &account(), &args, &BTreeMap::new(), Vec::new(), true, 30).unwrap_err();
          assert_eq!(err.code, CallErrorCode::InvalidArgs);
      }
  }
  ```
- [ ] Refactor `CallOk` in `connector_call.rs` from a struct into an enum and centralize the success JSON. Replace the struct and its two `execute`/`healthcheck` construction sites:
  ```rust
  /// A successful call result. HTTP and mock carry `status`/`truncated`; smtp
  /// carries only a body (spec 4.2: `{ ok: true, body: { accepted, from, subject } }`).
  #[derive(Debug)]
  pub enum CallOk {
      Http { status: u16, body: Value, truncated: bool },
      Smtp { body: Value },
  }

  impl CallOk {
      /// The `{ "ok": true, ... }` success JSON, shaped per kind.
      fn to_success_json(&self) -> Value {
          match self {
              CallOk::Http { status, body, truncated } => json!({
                  "ok": true, "status": status, "body": body, "truncated": truncated,
              }),
              CallOk::Smtp { body } => json!({ "ok": true, "body": body }),
          }
      }

      /// Test/inspection accessor for the response body regardless of shape.
      pub fn body(&self) -> &Value {
          match self { CallOk::Http { body, .. } | CallOk::Smtp { body } => body }
      }
  }
  ```
  In `execute`, change the `Outcome::Ok(ok, meta)` arm to `let value = ok.to_success_json();`. In `healthcheck`, change `Ok(ok) => (ok.to_success_json(), true)`. In `map_status`, return `Ok(CallOk::Http { status, body, truncated })`.
- [ ] Add the SMTP branch to `build_prepared`, after the secret-resolution step (7) and before the HTTP render (8):
  ```rust
  // 7b. SMTP: render the message/connection off the same resolved secrets and
  // account fields, or produce a dry-run envelope without connecting.
  if let Some(smtp) = &function.smtp {
      return match crate::connector_smtp::build(
          smtp,
          &account_fields,
          args,
          &secrets,
          redactions,
          dry_run,
          function.timeout_sec,
      )? {
          crate::connector_smtp::SmtpBuild::DryRun(v) => Ok(Prepared::DryRun(v)),
          crate::connector_smtp::SmtpBuild::Call(call) => Ok(Prepared::Call(Box::new(
              PreparedCall::Smtp { account: account_name, call },
          ))),
      };
  }
  ```
- [ ] Add the `PreparedCall::Smtp` variant and wire the three `PreparedCall` methods:
  ```rust
  Smtp { account: String, call: Box<crate::connector_smtp::SmtpCall> },
  ```
  - `account`: `PreparedCall::Smtp { account, .. } => account`.
  - `pre_auth_url`: `PreparedCall::Smtp { call, .. } => call.endpoint()` (the `smtp://host:port` string).
  - `dispatch`: `PreparedCall::Smtp { call, .. } => (call.send(), None)`.
  - Add `fn event_extra(&self) -> (Option<String>, Option<u32>)` returning `call.event_extra()` for the Smtp arm and `(None, None)` otherwise; call it in `run()` and store into the extended `EventMeta`.
- [ ] Write `connector_smtp.rs` rendering + dry-run (execution lands in Task 6; here `SmtpCall::send` is a stub that returns a `service` error so the crate compiles and the dry-run/validation tests pass):
  ```rust
  //! Native SMTP execution for the `smtp` connector function kind (spec 4.2).
  //! Rendering, address validation, and dry-run shaping live here; the blocking
  //! lettre transport (Task 6) does connect/EHLO/STARTTLS/AUTH/QUIT. This module
  //! is the SMTP twin of the HTTP path in `connector_call`, kept separate so
  //! that file stays cohesive.

  use std::collections::BTreeMap;

  use apb_core::connector::def::{SmtpMessage, SmtpSpec};
  use apb_core::connector::template::{RenderCtx, render_raw};
  use lettre::message::Mailbox;
  use serde_json::{Value, json};

  use crate::connector_call::{CallError, CallErrorCode, CallOk};

  /// The outcome of preparing an smtp call: a terminal dry-run render, or a
  /// ready-to-send call.
  pub(crate) enum SmtpBuild {
      DryRun(Value),
      Call(Box<SmtpCall>),
  }

  /// A rendered, ready-to-send smtp call. Holds resolved connection parameters
  /// (including the secret password, never logged), the built envelope, and the
  /// formatted message bytes. `verify` calls carry no message.
  pub(crate) struct SmtpCall {
      pub(crate) host: String,
      pub(crate) port: u16,
      pub(crate) use_tls: bool,
      pub(crate) username: Option<String>,
      pub(crate) password: Option<String>,
      pub(crate) timeout_sec: u64,
      /// `Some` for send, `None` for verify.
      pub(crate) message: Option<BuiltMessage>,
      pub(crate) redactions: Vec<(String, String)>,
  }

  /// The built email: the from address string, the subject, the per-list
  /// recipient email strings, the lettre envelope, and the RFC 5322 bytes.
  pub(crate) struct BuiltMessage {
      pub(crate) from: String,
      pub(crate) subject: String,
      pub(crate) to: Vec<String>,
      pub(crate) cc: Vec<String>,
      pub(crate) bcc: Vec<String>,
      pub(crate) recipients: Vec<String>,
      pub(crate) envelope: lettre::address::Envelope,
      pub(crate) formatted: Vec<u8>,
  }

  impl SmtpCall {
      /// The event-log endpoint: `smtp://host:port`, no credentials.
      pub(crate) fn endpoint(&self) -> String {
          format!("smtp://{}:{}", self.host, self.port)
      }

      /// Event metadata: subject and recipient count (both `None` for verify).
      pub(crate) fn event_extra(&self) -> (Option<String>, Option<u32>) {
          match &self.message {
              Some(m) => (Some(m.subject.clone()), Some(m.recipients.len() as u32)),
              None => (None, None),
          }
      }

      // Real implementation lands in Task 6.
      pub(crate) fn send(self) -> Result<CallOk, CallError> {
          Err(CallError::new(CallErrorCode::Service, "smtp send not implemented"))
      }
  }

  /// Renders and validates an smtp function. Dry-run shapes the envelope without
  /// connecting or touching secrets; a real build resolves the connection and
  /// formats the message.
  pub(crate) fn build(
      spec: &SmtpSpec,
      account: &BTreeMap<String, String>,
      args: &Value,
      secrets: &BTreeMap<String, String>,
      redactions: Vec<(String, String)>,
      dry_run: bool,
      timeout_sec: u64,
  ) -> Result<SmtpBuild, CallError> {
      let ctx = RenderCtx { account, args, secrets };

      // Message rendering + address validation happens for both dry-run and real
      // sends (verify has no message). Bad addresses are invalid_args and must be
      // caught before any connection.
      let built = match &spec.message {
          Some(m) => Some(render_message(m, &ctx)?),
          None => None,
      };

      if dry_run {
          return Ok(SmtpBuild::DryRun(dry_run_json(spec, account, &built)?));
      }

      let (host, port, use_tls, username, password) = render_connection(spec, &ctx)?;
      Ok(SmtpBuild::Call(Box::new(SmtpCall {
          host,
          port,
          use_tls,
          username,
          password,
          timeout_sec,
          message: built,
          redactions,
      })))
  }
  ```
  Add the helpers in the same module:
  ```rust
  /// Renders and parses a comma-separated address list into lettre mailboxes.
  /// Empty tokens are skipped; a token that is not a valid address is
  /// invalid_args naming the field.
  fn render_addresses(
      field: &str,
      template: &str,
      ctx: &RenderCtx,
  ) -> Result<Vec<Mailbox>, CallError> {
      let rendered = render_raw(template, ctx)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp {field} render failed: {e}")))?;
      let mut out = Vec::new();
      for token in rendered.split(',') {
          let token = token.trim();
          if token.is_empty() {
              continue;
          }
          let mb = token.parse::<Mailbox>().map_err(|_| {
              CallError::new(
                  CallErrorCode::InvalidArgs,
                  format!("smtp {field} address is not valid: `{token}`"),
              )
          })?;
          out.push(mb);
      }
      Ok(out)
  }

  /// Renders one optional address list (cc/bcc); absent template yields an empty
  /// list.
  fn render_optional_addresses(
      field: &str,
      template: Option<&str>,
      ctx: &RenderCtx,
  ) -> Result<Vec<Mailbox>, CallError> {
      match template {
          Some(t) => render_addresses(field, t, ctx),
          None => Ok(Vec::new()),
      }
  }

  /// Renders and builds the message: from mailbox, recipient mailboxes, subject,
  /// and the multipart/alternative-or-single body. At least one of body_text or
  /// body_html is required (invalid_args otherwise).
  fn render_message(msg: &SmtpMessage, ctx: &RenderCtx) -> Result<BuiltMessage, CallError> {
      use lettre::message::{Message, MultiPart, SinglePart};

      let from_email = render_raw(&msg.from_email, ctx)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp from_email render failed: {e}")))?;
      let from_name = match &msg.from_name {
          Some(t) => Some(render_raw(t, ctx).map_err(|e| {
              CallError::new(CallErrorCode::Config, format!("smtp from_name render failed: {e}"))
          })?),
          None => None,
      };
      let from_addr = from_email.parse::<lettre::Address>().map_err(|_| {
          CallError::new(CallErrorCode::InvalidArgs, format!("smtp from_email is not valid: `{from_email}`"))
      })?;
      let from_mb = Mailbox::new(from_name, from_addr);

      let subject = render_raw(&msg.subject, ctx)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp subject render failed: {e}")))?;
      let to = render_addresses("to", &msg.to, ctx)?;
      let cc = render_optional_addresses("cc", msg.cc.as_deref(), ctx)?;
      let bcc = render_optional_addresses("bcc", msg.bcc.as_deref(), ctx)?;
      if to.is_empty() {
          return Err(CallError::new(CallErrorCode::InvalidArgs, "smtp `to` has no recipients"));
      }

      let body_text = render_body_opt("body_text", msg.body_text.as_deref(), ctx)?;
      let body_html = render_body_opt("body_html", msg.body_html.as_deref(), ctx)?;

      let mut b = Message::builder().from(from_mb.clone()).subject(&subject);
      for mb in &to { b = b.to(mb.clone()); }
      for mb in &cc { b = b.cc(mb.clone()); }
      for mb in &bcc { b = b.bcc(mb.clone()); }

      let email = match (body_text, body_html) {
          (Some(t), Some(h)) => b.multipart(MultiPart::alternative_plain_html(t, h)),
          (Some(t), None) => b.singlepart(SinglePart::plain(t)),
          (None, Some(h)) => b.singlepart(SinglePart::html(h)),
          (None, None) => {
              return Err(CallError::new(
                  CallErrorCode::InvalidArgs,
                  "smtp message needs body_text or body_html",
              ));
          }
      }
      .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp message build failed: {e}")))?;

      let to_strings: Vec<String> = to.iter().map(|mb| mb.email.to_string()).collect();
      let cc_strings: Vec<String> = cc.iter().map(|mb| mb.email.to_string()).collect();
      let bcc_strings: Vec<String> = bcc.iter().map(|mb| mb.email.to_string()).collect();
      let recipients: Vec<String> = to_strings
          .iter()
          .chain(cc_strings.iter())
          .chain(bcc_strings.iter())
          .cloned()
          .collect();
      let envelope = email.envelope().clone();
      Ok(BuiltMessage {
          from: from_mb.email.to_string(),
          subject,
          to: to_strings,
          cc: cc_strings,
          bcc: bcc_strings,
          recipients,
          envelope,
          formatted: email.formatted(),
      })
  }

  fn render_body_opt(field: &str, template: Option<&str>, ctx: &RenderCtx) -> Result<Option<String>, CallError> {
      match template {
          Some(t) => Ok(Some(render_raw(t, ctx).map_err(|e| {
              CallError::new(CallErrorCode::Config, format!("smtp {field} render failed: {e}"))
          })?)),
          None => Ok(None),
      }
  }

  /// Renders the connection block into concrete parameters. `port` must parse to
  /// a u16 and `use_tls` to a bool (config errors otherwise). The password is
  /// resolved from secrets and never logged.
  fn render_connection(
      spec: &SmtpSpec,
      ctx: &RenderCtx,
  ) -> Result<(String, u16, bool, Option<String>, Option<String>), CallError> {
      let c = &spec.connection;
      let host = render_raw(&c.host, ctx)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp host render failed: {e}")))?;
      let port_s = render_raw(&c.port, ctx)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp port render failed: {e}")))?;
      let port: u16 = port_s
          .trim()
          .parse()
          .map_err(|_| CallError::new(CallErrorCode::Config, format!("smtp port is not a valid port: `{port_s}`")))?;
      let use_tls_s = render_raw(&c.use_tls, ctx)
          .map_err(|e| CallError::new(CallErrorCode::Config, format!("smtp use_tls render failed: {e}")))?;
      let use_tls: bool = use_tls_s
          .trim()
          .parse()
          .map_err(|_| CallError::new(CallErrorCode::Config, format!("smtp use_tls must be true or false: `{use_tls_s}`")))?;
      let username = render_opt(c.username.as_deref(), ctx, "username")?;
      let password = render_opt(c.password.as_deref(), ctx, "password")?;
      Ok((host, port, use_tls, username, password))
  }

  fn render_opt(template: Option<&str>, ctx: &RenderCtx, field: &str) -> Result<Option<String>, CallError> {
      match template {
          Some(t) => {
              let v = render_raw(t, ctx).map_err(|e| {
                  CallError::new(CallErrorCode::Config, format!("smtp {field} render failed: {e}"))
              })?;
              Ok(if v.is_empty() { None } else { Some(v) })
          }
          None => Ok(None),
      }
  }

  /// The dry-run JSON: the rendered envelope (from/to/cc/bcc/subject), no
  /// connection, no secrets. For a verify function (no message) it reports the
  /// endpoint only.
  fn dry_run_json(
      spec: &SmtpSpec,
      account: &BTreeMap<String, String>,
      built: &Option<BuiltMessage>,
  ) -> Result<Value, CallError> {
      match built {
          Some(m) => Ok(json!({
              "ok": true,
              "dry_run": true,
              "envelope": { "from": m.from, "to": m.to, "cc": m.cc, "bcc": m.bcc, "subject": m.subject },
          })),
          None => {
              // verify dry-run: host/port are non-secret account fields, safe to show.
              let host = account.get("host").cloned().unwrap_or_default();
              let port = account.get("port").cloned().unwrap_or_default();
              let _ = spec;
              Ok(json!({ "ok": true, "dry_run": true, "verify": true, "endpoint": format!("smtp://{host}:{port}") }))
          }
      }
  }
  ```
- [ ] Fix the `connector_call` compile: the `EventMeta` struct gains `smtp_subject: Option<String>` and `smtp_recipients: Option<u32>`; populate them in `run()` from `prepared.event_extra()` and pass them through `append_event` into the new `EventPayload::ConnectorCall` fields (Task 4). For HTTP/mock they are `(None, None)`.
- [ ] Run the unit tests: `cargo test -p apb-engine --lib connector_smtp` (dry-run and address-validation tests pass; execution tests come in Task 6).
- [ ] Run the existing call suite to confirm the `CallOk` refactor did not regress HTTP/mock: `cargo test -p apb-engine --test main connector_call connector_e2e connector_healthcheck`.
- [ ] fmt, clippy, code-ranker (watch cohesion/complexity on `connector_smtp.rs`; the split into `render_message`/`render_connection`/`dry_run_json` keeps each function small).
- [ ] Commit: `git commit --signoff -m "engine: smtp rendering, address validation, dry-run, and CallOk shape split"`.

---

### Task 6: SMTP execution (send and verify) via lettre, with error mapping

**Files:**
- Create: `crates/apb-engine/tests/suite/connector_smtp.rs` and register it in `crates/apb-engine/tests/main.rs`
- Modify: `crates/apb-engine/src/connector_smtp.rs` (`SmtpCall::send`, verify, error mapping)

**Interfaces:**
- Consumes `lettre::transport::smtp::client::{SmtpConnection, TlsParameters}`, `lettre::transport::smtp::extension::ClientId`, `lettre::transport::smtp::authentication::{Credentials, Mechanism}`, `lettre::transport::smtp::Error`.
- Produces `SmtpCall::send(self) -> Result<CallOk, CallError>` returning `CallOk::Smtp { body: { accepted, from, subject } }` for send and `{ verified: true }` for verify.

Steps:

- [ ] Add the test-support SMTP listener and the first failing execution test. Create `crates/apb-engine/tests/suite/connector_smtp.rs`. The listener is a plain `std::thread` + `std::net::TcpListener` server (mirroring the existing `common::spawn_http`), because the executor is blocking and the crate's test infra is sync; it speaks enough SMTP to record everything. See the "Testing choices" note below for why no tokio and no real STARTTLS handshake.
  ```rust
  //! Slice-3 smtp execution tests. A blocking std-thread SMTP listener records
  //! the whole conversation (EHLO, AUTH, MAIL/RCPT/DATA, QUIT) so we can assert
  //! on the rendered envelope and MIME structure without a network or a real TLS
  //! stack. STARTTLS itself is not exercised here (a real handshake needs a
  //! self-signed cert); live smoke tests cover real TLS. The non-TLS send and
  //! verify paths, plus a use_tls-refuses-plaintext unit and error mapping, are
  //! covered here.

  use std::collections::BTreeMap;
  use std::io::{BufRead, BufReader, Write};
  use std::net::{TcpListener, TcpStream};
  use std::sync::{Arc, Mutex};
  use std::thread::JoinHandle;

  use apb_core::connector::def::SmtpSpec;
  use apb_engine::connector_smtp::{build, SmtpBuild};
  use serde_json::json;

  /// What the listener recorded, for assertions.
  #[derive(Default, Clone)]
  struct Recorded {
      ehlo: bool,
      auth_plain: Option<String>,
      mail_from: Option<String>,
      rcpt_to: Vec<String>,
      data: String,
      quit: bool,
  }

  struct SmtpTestServer {
      host: String,
      port: u16,
      rec: Arc<Mutex<Recorded>>,
      handle: Option<JoinHandle<()>>,
  }

  impl SmtpTestServer {
      fn recorded(&self) -> Recorded {
          self.rec.lock().unwrap().clone()
      }
  }
  impl Drop for SmtpTestServer {
      fn drop(&mut self) {
          if let Some(h) = self.handle.take() {
              let _ = TcpStream::connect((self.host.as_str(), self.port));
              let _ = h.join();
          }
      }
  }

  /// Spawns a one-connection SMTP listener. `advertise_starttls` controls whether
  /// EHLO advertises STARTTLS; `advertise_auth` whether it advertises AUTH.
  fn spawn_smtp(advertise_starttls: bool, advertise_auth: bool) -> SmtpTestServer {
      let listener = TcpListener::bind("127.0.0.1:0").unwrap();
      let addr = listener.local_addr().unwrap();
      let rec = Arc::new(Mutex::new(Recorded::default()));
      let slot = rec.clone();
      let handle = std::thread::spawn(move || {
          if let Ok((mut stream, _)) = listener.accept() {
              serve(&mut stream, &slot, advertise_starttls, advertise_auth);
          }
      });
      SmtpTestServer { host: addr.ip().to_string(), port: addr.port(), rec, handle: Some(handle) }
  }

  fn line(reader: &mut BufReader<&TcpStream>) -> String {
      let mut s = String::new();
      let _ = reader.read_line(&mut s);
      s.trim_end().to_string()
  }

  fn serve(stream: &mut TcpStream, rec: &Arc<Mutex<Recorded>>, starttls: bool, auth: bool) {
      let mut w = stream.try_clone().unwrap();
      let mut reader = BufReader::new(&*stream);
      let _ = w.write_all(b"220 test ESMTP\r\n");
      loop {
          let cmd = line(&mut reader);
          let upper = cmd.to_ascii_uppercase();
          if upper.starts_with("EHLO") || upper.starts_with("HELO") {
              rec.lock().unwrap().ehlo = true;
              let mut resp = String::from("250-test\r\n");
              if starttls { resp.push_str("250-STARTTLS\r\n"); }
              if auth { resp.push_str("250-AUTH PLAIN LOGIN\r\n"); }
              resp.push_str("250 SMTPUTF8\r\n");
              let _ = w.write_all(resp.as_bytes());
          } else if upper.starts_with("AUTH PLAIN") {
              rec.lock().unwrap().auth_plain = Some(cmd.clone());
              let _ = w.write_all(b"235 2.7.0 Authentication successful\r\n");
          } else if upper.starts_with("MAIL FROM") {
              rec.lock().unwrap().mail_from = Some(cmd.clone());
              let _ = w.write_all(b"250 OK\r\n");
          } else if upper.starts_with("RCPT TO") {
              rec.lock().unwrap().rcpt_to.push(cmd.clone());
              let _ = w.write_all(b"250 OK\r\n");
          } else if upper.starts_with("DATA") {
              let _ = w.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n");
              let mut body = String::new();
              loop {
                  let l = line(&mut reader);
                  if l == "." { break; }
                  body.push_str(&l);
                  body.push('\n');
              }
              rec.lock().unwrap().data = body;
              let _ = w.write_all(b"250 2.0.0 Ok: queued\r\n");
          } else if upper.starts_with("QUIT") {
              rec.lock().unwrap().quit = true;
              let _ = w.write_all(b"221 Bye\r\n");
              break;
          } else if upper.starts_with("STARTTLS") {
              // Not negotiated in tests; refuse so a plaintext-refusing client
              // never proceeds. (use_tls path is asserted via the unit below.)
              let _ = w.write_all(b"454 TLS not available\r\n");
          } else if cmd.is_empty() {
              break;
          } else {
              let _ = w.write_all(b"250 OK\r\n");
          }
      }
  }

  fn send_spec() -> SmtpSpec {
      serde_yaml_ng::from_str(
          "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nmessage:\n  from_email: \"{{account.from_email}}\"\n  to: \"{{args.to}}\"\n  subject: \"{{args.subject}}\"\n  body_text: \"{{args.body_text}}\"\n  body_html: \"{{args.body_html}}\"\n",
      ).unwrap()
  }

  fn account(host: &str, port: u16, use_tls: bool) -> BTreeMap<String, String> {
      BTreeMap::from([
          ("host".into(), host.into()),
          ("port".into(), port.to_string()),
          ("use_tls".into(), use_tls.to_string()),
          ("username".into(), "u".into()),
          ("from_email".into(), "a@b.c".into()),
      ])
  }

  fn secrets() -> BTreeMap<String, String> {
      BTreeMap::from([("password".into(), "pw".into())])
  }

  #[test]
  fn send_over_plaintext_delivers_multipart() {
      let srv = spawn_smtp(false, true);
      let spec = send_spec();
      let args = json!({"to": "x@y.z, w@y.z", "subject": "Hi", "body_text": "T", "body_html": "<p>T</p>"});
      let call = match build(&spec, &account(&srv.host, srv.port, false), &args, &secrets(), Vec::new(), false, 15).unwrap() {
          SmtpBuild::Call(c) => c,
          _ => panic!("expected a call"),
      };
      let ok = call.send().expect("send should succeed");
      let body = ok.body();
      assert_eq!(body["from"], json!("a@b.c"));
      assert_eq!(body["subject"], json!("Hi"));
      assert_eq!(body["accepted"], json!(["x@y.z", "w@y.z"]));

      let r = srv.recorded();
      assert!(r.ehlo && r.quit);
      assert!(r.auth_plain.is_some(), "AUTH PLAIN expected");
      assert_eq!(r.rcpt_to.len(), 2);
      assert!(r.data.contains("Subject: Hi"));
      assert!(r.data.to_lowercase().contains("multipart/alternative"));
      // No credential ever appears in the recorded envelope metadata beyond the
      // AUTH line (which the event log never records).
      assert!(!r.data.contains("pw"));
  }
  ```
- [ ] Register the new suite module in `crates/apb-engine/tests/main.rs`:
  ```rust
  #[path = "suite/connector_smtp.rs"]
  mod connector_smtp;
  ```
- [ ] Run and watch fail: `cargo test -p apb-engine --test main connector_smtp::send_over_plaintext_delivers_multipart` (the stub `send` returns a `service` error).
- [ ] Implement `SmtpCall::send` and the verify/error-mapping helpers in `connector_smtp.rs`:
  ```rust
  use lettre::transport::smtp::authentication::{Credentials, Mechanism};
  use lettre::transport::smtp::client::{SmtpConnection, TlsParameters};
  use lettre::transport::smtp::extension::ClientId;

  /// Which handshake stage an error came from, for taxonomy mapping.
  #[derive(Clone, Copy)]
  enum Stage { Connect, Starttls, Auth, Send, Quit }

  impl SmtpCall {
      pub(crate) fn send(self) -> Result<CallOk, CallError> {
          let timeout = std::time::Duration::from_secs(self.timeout_sec);
          let hello = ClientId::default();

          // Connect (TCP + greeting + EHLO). No implicit TLS; STARTTLS is applied
          // below when use_tls is set.
          let mut conn = SmtpConnection::connect(
              (self.host.as_str(), self.port),
              Some(timeout),
              &hello,
              None,
              None,
          )
          .map_err(|e| self.map_err(Stage::Connect, e))?;

          if self.use_tls {
              if !conn.can_starttls() {
                  let _ = conn.quit();
                  return Err(CallError::new(
                      CallErrorCode::Service,
                      "smtp server does not advertise STARTTLS but use_tls is set; refusing to send in plaintext",
                  ));
              }
              let tls = TlsParameters::new(self.host.clone()).map_err(|e| {
                  CallError::new(CallErrorCode::Config, format!("smtp TLS setup failed: {e}"))
              })?;
              conn.starttls(tls, &hello).map_err(|e| self.map_err(Stage::Starttls, e))?;
          }

          if let (Some(user), Some(pass)) = (self.username.as_ref(), self.password.as_ref()) {
              let creds = Credentials::new(user.clone(), pass.clone());
              conn.auth(&[Mechanism::Plain, Mechanism::Login], &creds)
                  .map_err(|e| self.map_err(Stage::Auth, e))?;
          }

          let result = match &self.message {
              Some(m) => {
                  conn.send(&m.envelope, &m.formatted)
                      .map_err(|e| self.map_err(Stage::Send, e))?;
                  CallOk::Smtp {
                      body: json!({
                          "accepted": m.recipients,
                          "from": m.from,
                          "subject": m.subject,
                      }),
                  }
              }
              None => CallOk::Smtp { body: json!({ "verified": true }) },
          };
          let _ = conn.quit();
          Ok(result)
      }

      /// Maps a lettre smtp error into the call taxonomy (spec 4.2): a timeout is
      /// `timeout`; a connect/DNS failure is `network`; an AUTH rejection is
      /// `auth`; any other protocol rejection is `service` with the SMTP reply
      /// code carried in the message (lettre's Display includes it). Every
      /// message is scrubbed through the interim literal redaction so a resolved
      /// password can never leak.
      fn map_err(&self, stage: Stage, err: lettre::transport::smtp::Error) -> CallError {
          if err.is_timeout() {
              return self.redact(CallError::new(CallErrorCode::Timeout, "smtp operation timed out"));
          }
          let (code, prefix) = match stage {
              Stage::Connect => (CallErrorCode::Network, "smtp connection failed"),
              Stage::Auth => (CallErrorCode::Auth, "smtp authentication rejected"),
              Stage::Starttls => (CallErrorCode::Service, "smtp STARTTLS failed"),
              Stage::Send => (CallErrorCode::Service, "smtp send rejected"),
              Stage::Quit => (CallErrorCode::Service, "smtp quit failed"),
          };
          self.redact(CallError::new(code, format!("{prefix}: {err}")))
      }

      fn redact(&self, mut err: CallError) -> CallError {
          err.message = crate::connector_call::redact_message(err.message, &self.redactions);
          err
      }
  }
  ```
  Because `redact_message` in `connector_call.rs` is private, mark it `pub(crate)` and call it directly from `connector_smtp`.
  - Note in a code comment: confirm the exact `SmtpConnection::connect` / `starttls` / `auth` / `send` signatures against the pinned lettre 0.11 version when wiring; lettre exposes these on the blocking client under the `smtp-transport` feature. Adjust by-ref-vs-by-value on `starttls` if the compiler requires it. `Error::is_timeout()` is the timeout predicate.
- [ ] Add the remaining execution tests to `connector_smtp.rs`:
  ```rust
  #[test]
  fn verify_authenticates_and_quits() {
      let srv = spawn_smtp(false, true);
      let spec: SmtpSpec = serde_yaml_ng::from_str(
          "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nverify: true\n",
      ).unwrap();
      let call = match build(&spec, &account(&srv.host, srv.port, false), &json!({}), &secrets(), Vec::new(), false, 15).unwrap() {
          SmtpBuild::Call(c) => c,
          _ => panic!("expected call"),
      };
      let ok = call.send().unwrap();
      assert_eq!(ok.body()["verified"], json!(true));
      let r = srv.recorded();
      assert!(r.ehlo && r.auth_plain.is_some() && r.quit);
      assert!(r.mail_from.is_none(), "verify must not send mail");
  }

  #[test]
  fn use_tls_refuses_plaintext_when_starttls_absent() {
      // Server advertises no STARTTLS; a use_tls call must refuse before AUTH/DATA.
      let srv = spawn_smtp(false, true);
      let spec = send_spec();
      let args = json!({"to": "x@y.z", "subject": "Hi", "body_text": "T"});
      let call = match build(&spec, &account(&srv.host, srv.port, true), &args, &secrets(), Vec::new(), false, 15).unwrap() {
          SmtpBuild::Call(c) => c,
          _ => panic!("expected call"),
      };
      let err = call.send().unwrap_err();
      assert_eq!(err.code, CallErrorCode::Service);
      let r = srv.recorded();
      assert!(r.mail_from.is_none() && r.auth_plain.is_none(), "must not proceed in plaintext");
  }
  ```
  (Import `apb_engine::connector_call::CallErrorCode` in the test module for these asserts.)
- [ ] Run all three to green: `cargo test -p apb-engine --test main connector_smtp`.
- [ ] Run the full engine suite to confirm no regression: `cargo test -p apb-engine`.
- [ ] fmt, clippy, warm cache then code-ranker.
- [ ] Commit: `git commit --signoff -m "engine: execute smtp send and verify over lettre with taxonomy error mapping"`.

---

### Task 7: End-to-end dry-run and invalid-args coverage through `execute`

**Files:**
- Modify: `crates/apb-engine/tests/suite/connector_call.rs` (reuse its manifest/snapshot seeding harness)

**Interfaces:**
- Consumes the full `connector_call::execute` pipeline (grant, snapshot, render) with an smtp connector snapshot, exercising dry-run (no secrets, no network) and the event fields.

Steps:

- [ ] Add a failing dry-run test that goes through the real gate + snapshot pipeline (proving `build_prepared`'s smtp branch and the dry-run terminal are wired, and that no event is recorded):
  ```rust
  const SMTP_YAML: &str = r#"
  name: mock-tracker
  version: 0.1.0
  account_fields:
    - name: host
      required: true
    - name: port
      required: true
    - name: use_tls
    - name: from_email
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: send_email
      description: Send an email
      smtp:
        connection:
          host: "{{account.host}}"
          port: "{{account.port}}"
          use_tls: "{{account.use_tls}}"
          password: "{{secret.token}}"
        message:
          from_email: "{{account.from_email}}"
          to: "{{args.to}}"
          subject: "{{args.subject}}"
          body_text: "{{args.body_text}}"
      args_schema: { type: object, properties: { to: { type: string }, subject: { type: string } }, required: [to, subject] }
  "#;

  fn seed_smtp_run(run_dir: &Path) {
      let mut m = RunExecutionManifest::default();
      m.connectors.push(ManifestConnector {
          name: CONNECTOR.to_string(),
          digest: "sha256:test".to_string(),
          accounts: vec![ManifestAccount {
              name: "acct1".to_string(),
              default: true,
              fields: BTreeMap::from([
                  ("host".to_string(), "smtp.example.com".to_string()),
                  ("port".to_string(), "587".to_string()),
                  ("use_tls".to_string(), "true".to_string()),
                  ("from_email".to_string(), "a@b.c".to_string()),
                  ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
              ]),
              env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
              digest: "sha256:acct".to_string(),
          }],
      });
      m.connector_grants.insert(
          NODE.to_string(),
          vec![ManifestConnectorGrant {
              connector: CONNECTOR.to_string(),
              accounts: vec!["acct1".to_string()],
              functions: vec!["send_email".to_string()],
              max_calls: None,
          }],
      );
      manifest::write(run_dir, &m).unwrap();
      let cdir = run_dir.join("connectors");
      std::fs::create_dir_all(&cdir).unwrap();
      std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), SMTP_YAML).unwrap();
  }

  #[test]
  fn smtp_dry_run_renders_envelope_and_records_no_event() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      // No secret seeded: dry-run must not need it.
      seed_smtp_run(run.path());
      let (value, ok) = call(
          run.path(),
          root.path(),
          "send_email",
          None,
          serde_json::json!({"to": "x@y.z", "subject": "Hi", "body_text": "T"}),
          true,
      );
      assert!(ok, "dry-run should succeed: {value}");
      assert_eq!(value["dry_run"], serde_json::json!(true));
      assert_eq!(value["envelope"]["from"], serde_json::json!("a@b.c"));
      assert_eq!(value["envelope"]["to"], serde_json::json!(["x@y.z"]));
      assert_eq!(value["envelope"]["subject"], serde_json::json!("Hi"));
      assert!(!value.to_string().contains("port"), "dry-run must not leak the connection block");
      let events = read_all(run.path()).unwrap();
      assert!(!events.iter().any(|e| matches!(e.payload, EventPayload::ConnectorCall { .. })));
  }

  #[test]
  fn smtp_bad_address_is_invalid_args() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      seed_smtp_run(run.path());
      let (value, ok) = call(
          run.path(),
          root.path(),
          "send_email",
          None,
          serde_json::json!({"to": "not-an-email", "subject": "Hi", "body_text": "T"}),
          true,
      );
      assert!(!ok);
      assert_eq!(value["error"]["code"], serde_json::json!("invalid_args"));
  }
  ```
- [ ] Run and watch fail (if any wiring gap remains) then pass after confirming: `cargo test -p apb-engine --test main connector_call::smtp_dry_run_renders_envelope_and_records_no_event connector_call::smtp_bad_address_is_invalid_args`.
- [ ] Run the whole workspace test suite: `cargo test --workspace`.
- [ ] fmt, clippy, warm cache then code-ranker.
- [ ] Commit: `git commit --signoff -m "engine: end-to-end smtp dry-run and invalid-args coverage through execute"`.

---

## Testing choices (documented per plan requirements)

- **Test transport is a plain blocking std-thread listener, not tokio.** The `apb connector call` executor is fully synchronous (the existing HTTP path uses blocking `ureq`), and lettre's blocking `SmtpConnection` blocks the calling thread. The crate's existing test infra (`common::spawn_http`) is a std-thread one-shot server. A tokio `TcpListener` would add an async runtime and a dev-dependency for no benefit and would still have to run on a separate thread from the blocking client. The listener therefore mirrors `spawn_http`: one `std::thread`, one `std::net::TcpListener`, recording the whole SMTP conversation.
- **Real STARTTLS is not exercised in unit/contract tests.** A genuine STARTTLS handshake needs a self-signed cert and a rustls server config in the listener, which is disproportionate. Instead: the non-TLS send and verify paths are exercised fully (EHLO, AUTH PLAIN, MAIL/RCPT/DATA, QUIT), and a dedicated unit (`use_tls_refuses_plaintext_when_starttls_absent`) proves that `use_tls: true` against a server that does not advertise STARTTLS refuses before any AUTH or DATA, so the client never leaks credentials or a body in plaintext. Real TLS 1.2+ negotiation is covered by the `#[ignore]` live smoke tests against a real server (spec section 8, slice 5), not in CI.
- **TLS 1.2 minimum is satisfied by the backend.** With the `rustls-tls` feature, rustls supports only TLS 1.2 and 1.3 (it dropped 1.0/1.1), so `TlsParameters::new(host)` already enforces the "TLS 1.2 minimum" requirement without extra configuration; no OpenSSL system dependency is pulled, which is friendlier for CI.

---

## Slice notes for parallel slices

- lettre pin: `lettre = "0.11"`, `default-features = false`, `features = ["builder", "smtp-transport", "rustls-tls", "hostname"]`.
- `CallOk` is refactored from a struct into a two-variant enum (`Http` / `Smtp`). Slice 1 adds `link`/`picked` to the HTTP success shape; whichever slice lands second reconciles those fields into the `Http` variant.
- The event log carries host+port inside the existing `url` field as an `smtp://host:port` endpoint string; only `smtp_subject` and `smtp_recipients` are added as new `#[serde(default)]` fields.
- Verify success body is `{ ok: true, body: { verified: true } }`; verify dry-run is `{ ok: true, dry_run: true, verify: true, endpoint: "smtp://host:port" }`.
- Neither-body (`body_text`/`body_html` both absent) is `invalid_args`; an empty rendered `to` list is `invalid_args`.
- The `response_pick`-on-smtp validation is owned by whichever of slices 1 and 3 lands second (Task 3 here is guarded).
- Slice 4's offline contract-test runner needs an envelope render entry point; `connector_smtp::build` with `dry_run: true` provides it (the `SmtpBuild::DryRun` JSON carries the envelope).
