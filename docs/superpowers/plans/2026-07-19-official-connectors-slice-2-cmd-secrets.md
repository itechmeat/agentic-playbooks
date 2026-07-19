# Official Connectors Slice 2: Command-Sourced Secrets - Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a `secret: true` account field source its value from a shell command (`token: "{{cmd:gh auth token}}"`) as an alternative to `{{env.VAR}}`, executed without a shell at call/healthcheck time with a 10 second timeout, resolved value never returned/logged/cached, and pinned by the existing account digest.

**Architecture:** `apb-core` gains the parser and the sandbox-free command executor in `connector/secrets.rs`, plus `cmd_refs` and extended validation in `connector/config.rs`. `apb-engine` carries the reference into the run manifest (`ManifestAccount.cmd`) and resolves it at call time in `connector_call.rs`, folding the resolved value into the interim literal redaction. The `apb-mcp` policy gate and `apb-core::resolve` require zero behavioral change: they only validate reference form and check env-name presence, so commands are provably never executed at gate time.

**Tech Stack:** Rust edition 2024, `shell-words` crate for argv parsing, `std::process::Command` (no shell), `std::thread` + `try_wait` polling for the timeout. No async (core stays sync).

## Global Constraints
- No em-dash U+2014, no exclamation marks in docs/user-facing strings, no CJK.
- Secret values are never returned, logged, or cached.
- Atomic state writes via `apb_core::fsutil`.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- code-ranker `check` must pass before commit (warm cache first with `cargo metadata --format-version 1 >/dev/null`).
- Commits use `git commit --signoff` and end with a `Co-Authored-By` trailer for the acting model.

## What the current code already does (verified, load-bearing for this slice)

- `secrets::parse_env_ref` (crates/apb-core/src/connector/secrets.rs:19) accepts exactly `{{env.VAR}}` with `VAR` matching `[A-Z][A-Z0-9_]*`, whole-value only; any surrounding text, lowercase, or `{{secret.*}}` returns `None`.
- `config::validate_accounts` (crates/apb-core/src/connector/config.rs:162-169) rejects a `secret: true` field whose value is not exactly one `{{env.VAR}}` reference.
- `config::account_digest` (crates/apb-core/src/connector/config.rs:209-219) hashes the domain tag, name, `default`, then **every** field key+value in `BTreeMap` order, including secret fields with their RAW reference string. **Secret field references are therefore already inside the canonical digest.** No digest change is needed; a field edited from `{{env.X}}` to `{{cmd:...}}` drops account trust exactly like any other field edit. Env-only accounts keep trust untouched.
- `config::env_refs` (config.rs:225-235) extracts field -> ENV VAR NAME only for values that parse as env refs; a `{{cmd:...}}` value is skipped, so it contributes nothing to `required_env` and nothing to the adapter scrub list (spec 4.1: command secrets add no env names).
- The policy gate `check_connectors` (crates/apb-mcp/src/policy.rs:294-380) resolves the playbook, checks env-name presence via `missing_vars` over `required_env`, then checks trust. It never resolves secret VALUES and never executes anything.
- `connector_call::resolve_secrets` (crates/apb-engine/src/connector_call.rs:750-767) iterates `ManifestAccount.env` (field -> var), resolves each via `secrets::resolve_var`, and returns `(secrets_by_field, redactions)`; `non_secret_fields` (733-740) excludes fields present in `account.env` so a raw ref never reaches the render `account` map. The interim redaction (`redact`, `redact_error`, 951-978) replaces every literal secret occurrence with `[redacted:<label>]`.
- `ManifestAccount` (crates/apb-engine/src/manifest.rs:59-69) has `fields`, `env: BTreeMap<field,var>`, `digest`. New fields must be added with `#[serde(default)]` (CLAUDE.md convention).

---

### Task 1: Parse `{{cmd:...}}` references in core

**Files:**
- Modify: crates/apb-core/src/connector/secrets.rs

**Interfaces:**
- Produces: `pub fn parse_cmd_ref(value: &str) -> Option<String>` returning the command-line text between `{{cmd:` and `}}`, or `None` for any non-reference (literal, env ref, empty inner, surrounding text).

Steps:

- [ ] Write the failing test. Append to the `parse_env_ref` test group in the `#[cfg(test)] mod tests` block of secrets.rs:
  ```rust
  // --- parse_cmd_ref -----------------------------------------------------

  #[test]
  fn parse_cmd_ref_accepts_valid_reference() {
      assert_eq!(
          parse_cmd_ref("{{cmd:gh auth token}}"),
          Some("gh auth token".to_string())
      );
      assert_eq!(
          parse_cmd_ref("{{cmd:op read \"op://vault/item/field\"}}"),
          Some("op read \"op://vault/item/field\"".to_string())
      );
  }

  #[test]
  fn parse_cmd_ref_rejects_malformed_values() {
      assert_eq!(parse_cmd_ref("literal"), None);
      assert_eq!(parse_cmd_ref("{{env.TOKEN}}"), None);
      assert_eq!(parse_cmd_ref("prefix {{cmd:gh}}"), None);
      assert_eq!(parse_cmd_ref("{{cmd:gh}} suffix"), None);
      assert_eq!(parse_cmd_ref("{{cmd:}}"), None);
      assert_eq!(parse_cmd_ref("{{cmd:   }}"), None);
  }

  #[test]
  fn parse_env_ref_and_cmd_ref_are_mutually_exclusive() {
      assert!(parse_env_ref("{{cmd:gh}}").is_none());
      assert!(parse_cmd_ref("{{env.T}}").is_none());
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-core --lib connector::secrets::tests::parse_cmd_ref` fails to compile (`cannot find function parse_cmd_ref`).
- [ ] Implement. Add directly after `parse_env_ref` (after line 32) in secrets.rs:
  ```rust
  /// Parses a `{{cmd:<command line>}}` secret reference: the whole value must
  /// be exactly that placeholder (no surrounding text). Returns the command
  /// line (the text between `{{cmd:` and the trailing `}}`), or `None` when
  /// the value is not a valid reference (a literal, an env reference, empty or
  /// whitespace-only inner text, or extra surrounding text). The command line
  /// is returned verbatim for `shell_words` parsing at resolution time; this
  /// function never executes anything.
  pub fn parse_cmd_ref(value: &str) -> Option<String> {
      let inner = value.strip_prefix("{{cmd:")?.strip_suffix("}}")?;
      if inner.trim().is_empty() {
          return None;
      }
      Some(inner.to_string())
  }
  ```
- [ ] Run and confirm pass: `cargo test -p apb-core --lib connector::secrets::tests::parse_cmd_ref`.
- [ ] Format and lint: `cargo fmt --all -- --check` and `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -am "core: parse {{cmd:...}} secret references"` (with the acting-model Co-Authored-By trailer).

---

### Task 2: Execute command-sourced secrets in core (shell-words, no shell, 10s timeout)

**Files:**
- Modify: Cargo.toml (workspace deps)
- Modify: crates/apb-core/Cargo.toml
- Modify: crates/apb-core/src/connector/secrets.rs
- Test: crates/apb-core/src/connector/secrets.rs (`#[cfg(all(test, unix))]` cases with a stub executable)

**Interfaces:**
- Produces: `pub const CMD_SECRET_TIMEOUT: std::time::Duration` (10s).
- Produces: `pub enum CmdSecretError { Parse(String), Spawn(String), Timeout, NonZero { code: Option<i32>, stderr: String }, Empty { stderr: String } }`.
- Produces: `pub fn resolve_cmd(cmdline: &str, timeout: Duration) -> Result<String, CmdSecretError>` - parses argv with shell-words, spawns the binary (PATH-resolved, no shell), enforces the timeout by killing on deadline, returns stdout with trailing whitespace trimmed; non-zero exit / timeout / empty stdout are errors carrying a trimmed single-line stderr excerpt.

Steps:

- [ ] Add the dependency. In the workspace `Cargo.toml` `[workspace.dependencies]` block add `shell-words = "1"`. In crates/apb-core/Cargo.toml `[dependencies]` add `shell-words.workspace = true`.
- [ ] Run `cargo metadata --format-version 1 >/dev/null` to fetch the crate into the cache (needed for the offline gates and code-ranker).
- [ ] Write the failing tests. Add a stub-executable helper and cases at the end of the secrets.rs test module. These are `#[cfg(unix)]` because the stub is a `/bin/sh` script:
  ```rust
  // --- resolve_cmd (unix stub executable) --------------------------------

  #[cfg(unix)]
  fn write_stub(dir: &Path, name: &str, script: &str) -> PathBuf {
      use std::os::unix::fs::PermissionsExt;
      let path = dir.join(name);
      std::fs::write(&path, script).unwrap();
      let mut perms = std::fs::metadata(&path).unwrap().permissions();
      perms.set_mode(0o755);
      std::fs::set_permissions(&path, &perms).unwrap();
      path
  }

  #[cfg(unix)]
  #[test]
  fn resolve_cmd_returns_trimmed_stdout() {
      let dir = tempfile::tempdir().unwrap();
      let stub = write_stub(dir.path(), "ok-secret", "#!/bin/sh\nprintf 'resolved-token-value\\n\\n'\n");
      let out = resolve_cmd(
          &format!("{} arg1", stub.to_string_lossy()),
          std::time::Duration::from_secs(10),
      )
      .unwrap();
      assert_eq!(out, "resolved-token-value");
  }

  #[cfg(unix)]
  #[test]
  fn resolve_cmd_nonzero_exit_is_error_with_stderr_excerpt() {
      let dir = tempfile::tempdir().unwrap();
      let stub = write_stub(dir.path(), "fail-secret", "#!/bin/sh\necho 'gh: not logged in' >&2\nexit 3\n");
      match resolve_cmd(&stub.to_string_lossy(), std::time::Duration::from_secs(10)) {
          Err(CmdSecretError::NonZero { code, stderr }) => {
              assert_eq!(code, Some(3));
              assert!(stderr.contains("not logged in"), "stderr excerpt: {stderr}");
          }
          other => panic!("expected NonZero, got {other:?}"),
      }
  }

  #[cfg(unix)]
  #[test]
  fn resolve_cmd_empty_output_is_error() {
      let dir = tempfile::tempdir().unwrap();
      let stub = write_stub(dir.path(), "empty-secret", "#!/bin/sh\nexit 0\n");
      assert!(matches!(
          resolve_cmd(&stub.to_string_lossy(), std::time::Duration::from_secs(10)),
          Err(CmdSecretError::Empty { .. })
      ));
  }

  #[cfg(unix)]
  #[test]
  fn resolve_cmd_times_out_and_kills_child() {
      let dir = tempfile::tempdir().unwrap();
      let stub = write_stub(dir.path(), "slow-secret", "#!/bin/sh\nsleep 30\n");
      let started = std::time::Instant::now();
      assert!(matches!(
          resolve_cmd(&stub.to_string_lossy(), std::time::Duration::from_millis(300)),
          Err(CmdSecretError::Timeout)
      ));
      assert!(started.elapsed() < std::time::Duration::from_secs(5), "timeout did not preempt sleep");
  }

  #[cfg(unix)]
  #[test]
  fn resolve_cmd_missing_binary_is_spawn_error() {
      assert!(matches!(
          resolve_cmd("apb-no-such-binary-zzz", std::time::Duration::from_secs(10)),
          Err(CmdSecretError::Spawn(_))
      ));
  }

  #[test]
  fn resolve_cmd_empty_command_is_parse_error() {
      assert!(matches!(
          resolve_cmd("   ", std::time::Duration::from_secs(10)),
          Err(CmdSecretError::Parse(_))
      ));
  }

  #[test]
  fn resolve_cmd_unbalanced_quotes_is_parse_error() {
      assert!(matches!(
          resolve_cmd("gh \"unterminated", std::time::Duration::from_secs(10)),
          Err(CmdSecretError::Parse(_))
      ));
  }
  ```
- [ ] Add `use std::time::Duration;` and `use std::path::PathBuf;` as needed to the test module imports (the module already imports `std::path::Path`).
- [ ] Run and confirm failure: `cargo test -p apb-core --lib connector::secrets::tests::resolve_cmd` (compile error: `resolve_cmd` / `CmdSecretError` not found).
- [ ] Implement. Add to the top-of-file `use` block: `use std::io::Read as _;`, `use std::process::{Command, Stdio};`, `use std::thread;`, `use std::time::{Duration, Instant};`. Then add the executor after `resolve_var` (below line 97):
  ```rust
  /// The wall-clock budget for a command-sourced secret (spec 4.1): a helper
  /// that hangs must not stall a call indefinitely.
  pub const CMD_SECRET_TIMEOUT: Duration = Duration::from_secs(10);

  /// Why a command-sourced secret could not be resolved. Carries a trimmed
  /// single-line stderr excerpt where the process produced one: credential
  /// helpers put diagnostics (not secrets) on stderr, which is what makes an
  /// error actionable. The engine maps this to a `config` call error naming
  /// the account and field.
  #[derive(Debug)]
  pub enum CmdSecretError {
      /// The command line did not parse into argv, or was empty.
      Parse(String),
      /// The binary could not be started (not found on PATH, not executable).
      Spawn(String),
      /// The command did not finish within the timeout.
      Timeout,
      /// The command exited non-zero. `code` is `None` when killed by a signal.
      NonZero { code: Option<i32>, stderr: String },
      /// The command succeeded but produced no non-whitespace output.
      Empty { stderr: String },
  }

  /// Collapses stderr to a single trimmed line and caps its length, so an
  /// error message stays one line and cannot dump an unbounded helper log.
  fn stderr_excerpt(stderr: &[u8]) -> String {
      const MAX: usize = 200;
      let text = String::from_utf8_lossy(stderr);
      let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
      if one_line.chars().count() > MAX {
          let mut s: String = one_line.chars().take(MAX).collect();
          s.push_str("...");
          s
      } else {
          one_line
      }
  }

  /// Resolves a command-sourced secret (spec 4.1). The command line is parsed
  /// into argv with shell-words rules (quoted arguments supported); NO shell is
  /// involved, so pipes, redirection, and substitution are not interpreted. The
  /// binary is resolved via `PATH`. Stdout with trailing whitespace trimmed is
  /// the secret value. A parse failure, a spawn failure, a non-zero exit, a
  /// timeout, or empty stdout is an error. The resolved value lives only in the
  /// returned `String`; it is never logged here.
  pub fn resolve_cmd(cmdline: &str, timeout: Duration) -> Result<String, CmdSecretError> {
      let argv =
          shell_words::split(cmdline).map_err(|e| CmdSecretError::Parse(e.to_string()))?;
      let (program, args) = argv
          .split_first()
          .ok_or_else(|| CmdSecretError::Parse("empty command".to_string()))?;

      let mut child = Command::new(program)
          .args(args)
          .stdin(Stdio::null())
          .stdout(Stdio::piped())
          .stderr(Stdio::piped())
          .spawn()
          .map_err(|e| CmdSecretError::Spawn(format!("{program}: {e}")))?;

      // Drain both pipes on their own threads so a large output cannot deadlock
      // the child against a full pipe while we wait; keep the child handle so
      // the deadline can kill it.
      let mut out_pipe = child.stdout.take().expect("stdout piped");
      let mut err_pipe = child.stderr.take().expect("stderr piped");
      let out_join = thread::spawn(move || {
          let mut buf = Vec::new();
          let _ = out_pipe.read_to_end(&mut buf);
          buf
      });
      let err_join = thread::spawn(move || {
          let mut buf = Vec::new();
          let _ = err_pipe.read_to_end(&mut buf);
          buf
      });

      let deadline = Instant::now() + timeout;
      let status = loop {
          match child.try_wait() {
              Ok(Some(status)) => break status,
              Ok(None) => {
                  if Instant::now() >= deadline {
                      let _ = child.kill();
                      let _ = child.wait();
                      return Err(CmdSecretError::Timeout);
                  }
                  thread::sleep(Duration::from_millis(20));
              }
              Err(e) => return Err(CmdSecretError::Spawn(e.to_string())),
          }
      };

      let stdout = out_join.join().unwrap_or_default();
      let stderr = err_join.join().unwrap_or_default();
      let excerpt = stderr_excerpt(&stderr);

      if !status.success() {
          return Err(CmdSecretError::NonZero {
              code: status.code(),
              stderr: excerpt,
          });
      }
      let value = String::from_utf8_lossy(&stdout);
      let trimmed = value.trim_end();
      if trimmed.is_empty() {
          return Err(CmdSecretError::Empty { stderr: excerpt });
      }
      Ok(trimmed.to_string())
  }
  ```
- [ ] Run and confirm pass: `cargo test -p apb-core --lib connector::secrets`.
- [ ] Format and lint: `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -am "core: resolve command-sourced secrets with a 10s no-shell timeout"` (with the acting-model Co-Authored-By trailer).

---

### Task 3: Accept cmd refs in account validation; add `cmd_refs`; assert digest coverage

**Files:**
- Modify: crates/apb-core/src/connector/config.rs
- Test: crates/apb-core/src/connector/config.rs

**Interfaces:**
- Consumes: `secrets::parse_cmd_ref`.
- Produces: `pub fn cmd_refs(doc: &ConnectorDoc, account: &Account) -> BTreeMap<String, String>` (field name -> command line, secret fields with a valid cmd ref only).
- Modifies: `validate_accounts` secret-field branch to accept either an env ref or a cmd ref.

Steps:

- [ ] Write the failing tests. Add to config.rs test module:
  ```rust
  #[test]
  fn validate_accepts_cmd_ref_secret() {
      let doc = jira_doc();
      let accounts = vec![acct(
          "prod",
          true,
          &[("base_url", "https://a"), ("token", "{{cmd:gh auth token}}")],
      )];
      assert!(validate_accounts(&doc, &accounts).is_empty());
  }

  #[test]
  fn validate_still_rejects_literal_secret_message_names_both_forms() {
      let doc = jira_doc();
      let accounts = vec![acct(
          "prod",
          false,
          &[("base_url", "https://a"), ("token", "sk-literal")],
      )];
      let errs = validate_accounts(&doc, &accounts);
      assert!(errs.iter().any(|e| e.contains("token") && e.contains("cmd")));
  }

  #[test]
  fn cmd_refs_extracts_command_from_cmd_secret_only() {
      let doc = jira_doc();
      let account = acct(
          "prod",
          false,
          &[("base_url", "https://a"), ("token", "{{cmd:gh auth token}}")],
      );
      let refs = cmd_refs(&doc, &account);
      assert_eq!(refs.len(), 1);
      assert_eq!(refs["token"], "gh auth token");
      // An env-ref field is not a cmd ref, and vice versa.
      assert!(env_refs(&doc, &account).is_empty());
  }

  #[test]
  fn env_and_cmd_refs_are_disjoint_for_the_same_account() {
      let doc = jira_doc();
      let env_account = acct("e", false, &[("base_url", "https://a"), ("token", "{{env.T}}")]);
      assert_eq!(env_refs(&doc, &env_account).len(), 1);
      assert!(cmd_refs(&doc, &env_account).is_empty());
  }

  #[test]
  fn digest_changes_when_secret_ref_switches_env_to_cmd() {
      // Regression pin: the account digest already covers secret field
      // reference strings, so swapping the source drops account trust.
      let a = acct("x", false, &[("token", "{{env.T}}")]);
      let b = acct("x", false, &[("token", "{{cmd:gh auth token}}")]);
      assert_ne!(account_digest(&a), account_digest(&b));
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-core --lib connector::config::tests` (compile error: `cmd_refs` not found; the message-content assertion fails).
- [ ] Implement `cmd_refs`. Add after `env_refs` (after line 235) in config.rs:
  ```rust
  /// The shell command line each secret field sources its value from, keyed by
  /// field name. Only fields declared `secret: true` in `doc` whose value is a
  /// valid `{{cmd:...}}` reference are included; env-ref and invalid fields are
  /// skipped here (rejecting an invalid value is `validate_accounts`'s job).
  /// This never executes the command; it only extracts the reference text.
  pub fn cmd_refs(doc: &ConnectorDoc, account: &Account) -> BTreeMap<String, String> {
      let mut out = BTreeMap::new();
      for name in doc.secret_fields() {
          if let Some(value) = account.fields.get(&name)
              && let Some(cmd) = parse_cmd_ref(value)
          {
              out.insert(name, cmd);
          }
      }
      out
  }
  ```
- [ ] Update the import at config.rs line 18 from `use super::secrets::parse_env_ref;` to `use super::secrets::{parse_cmd_ref, parse_env_ref};`.
- [ ] Update the secret-field validation branch (config.rs 162-169) to accept either form:
  ```rust
  Some(true) => {
      if parse_env_ref(value).is_none() && parse_cmd_ref(value).is_none() {
          errors.push(format!(
              "account `{}` field `{key}` must be exactly one `{{{{env.VAR}}}}` or `{{{{cmd:...}}}}` reference, not a literal value",
              account.name
          ));
      }
  }
  ```
- [ ] Update the `account_digest` doc comment (config.rs 202-208) to note that a secret field's reference string (env or cmd) participates verbatim, so switching source drops account trust. No code change to the digest body.
- [ ] Run and confirm pass: `cargo test -p apb-core --lib connector::config::tests`.
- [ ] Format and lint: `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -am "core: accept {{cmd:...}} secret refs in account validation and expose cmd_refs"` (with the acting-model Co-Authored-By trailer).

---

### Task 4: Carry cmd refs into the run manifest (`ManifestAccount.cmd`)

**Files:**
- Modify: crates/apb-engine/src/manifest.rs (struct field)
- Modify: crates/apb-engine/src/connector_run.rs (populate)
- Modify: crates/apb-engine/src/connector_prompt.rs (existing literal, line ~139)
- Test: crates/apb-engine/src/manifest.rs (roundtrip default)

**Interfaces:**
- Produces: `ManifestAccount.cmd: BTreeMap<String, String>` (secret field name -> command line), `#[serde(default)]` for backward compatibility with existing manifests.
- Consumes: `config::cmd_refs`.

Steps:

- [ ] Write the failing test. Add to the manifest.rs test module (create one if absent, following the crate's existing test style):
  ```rust
  #[test]
  fn manifest_account_cmd_defaults_to_empty_and_roundtrips() {
      let acct = ManifestAccount {
          name: "a".to_string(),
          default: false,
          fields: BTreeMap::from([("base_url".to_string(), "https://x".to_string())]),
          env: BTreeMap::new(),
          cmd: BTreeMap::from([("token".to_string(), "gh auth token".to_string())]),
          digest: "sha256:x".to_string(),
      };
      let yaml = serde_yaml_ng::to_string(&acct).unwrap();
      let back: ManifestAccount = serde_yaml_ng::from_str(&yaml).unwrap();
      assert_eq!(back, acct);
      // An older manifest without `cmd` still parses (serde default).
      let legacy = "name: a\ndefault: false\nfields: {}\nenv: {}\ndigest: sha256:x\n";
      let parsed: ManifestAccount = serde_yaml_ng::from_str(legacy).unwrap();
      assert!(parsed.cmd.is_empty());
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-engine --lib manifest` (compile error: missing field `cmd`).
- [ ] Implement the struct field. In manifest.rs `ManifestAccount` (after line 66, the `env` field) add:
  ```rust
      /// Secret field name -> the shell command line that produces the secret
      /// at call time (spec 4.1), never the secret value itself. Empty for an
      /// env-sourced or non-secret account; disjoint from `env` (a secret field
      /// is exactly one of the two forms).
      #[serde(default)]
      pub cmd: BTreeMap<String, String>,
  ```
- [ ] Populate it at the real construction site. In connector_run.rs `ManifestAccount { ... }` (line 151-157) add `cmd: config::cmd_refs(&resolved.loaded.doc, account),` after the `env:` line, and update the step-6 comment to mention the `cmd` map alongside `env`.
- [ ] Fix the other in-tree literal. In connector_prompt.rs (line ~139-146) add `cmd: BTreeMap::new(),` to the `ManifestAccount { ... }` literal (this is the prompt-preview fixture; env stays as-is).
- [ ] Run and confirm pass: `cargo test -p apb-engine --lib manifest` and `cargo build -p apb-engine` (all literals compile).
- [ ] Format and lint: `cargo fmt --all -- --check`; `cargo clippy -p apb-engine --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -am "engine: record command-sourced secret refs in the run manifest"` (with the acting-model Co-Authored-By trailer).

---

### Task 5: Resolve cmd secrets at call time with redaction; extend healthcheck

**Files:**
- Modify: crates/apb-engine/src/connector_call.rs (`resolve_secrets`, `non_secret_fields`, `prepare_healthcheck`, new `cmd_secret_error`)
- Test: crates/apb-engine/tests/suite/connector_call.rs (stub-executable integration cases)

**Interfaces:**
- Consumes: `secrets::resolve_cmd`, `secrets::CMD_SECRET_TIMEOUT`, `secrets::CmdSecretError`, `config::cmd_refs`.
- Produces: cmd secrets resolved into the render `secrets` map (keyed by field), added to `redactions` with the label `cmd:<field>` so the interim literal redaction scrubs them from body and error messages. A parse/spawn/timeout/non-zero/empty failure is a `config` CallError naming the account and field with a trimmed stderr excerpt.

Steps:

- [ ] Write the failing integration tests. Add a stub helper and cases to crates/apb-engine/tests/suite/connector_call.rs. These use a connector whose secret is a cmd ref and a stub binary the test writes into a temp dir (referenced by absolute path in the connector config, so no PATH mutation is needed; a PATH-resolution case is covered by the core unit tests). Add a helper to the top of the file:
  ```rust
  #[cfg(unix)]
  fn write_cmd_connector(run_dir: &Path, stub_path: &str) {
      // A connector whose `token` secret is sourced from a command, echoed back
      // by the `echo` function so redaction can be asserted end to end.
      let yaml = format!(
          r#"
  name: cmd-conn
  version: 0.1.0
  auth:
    kind: header
    header: Authorization
    value_template: "Bearer {{{{secret.token}}}}"
  account_fields:
    - name: base_url
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: echo
      description: Echo whatever the service returns
      method: GET
      url: "{{{{account.base_url}}}}/echo"
  "#
      );
      let cdir = run_dir.join("connectors");
      std::fs::create_dir_all(&cdir).unwrap();
      std::fs::write(cdir.join("cmd-conn.yaml"), yaml).unwrap();
      let _ = stub_path; // referenced by the account below
  }

  #[cfg(unix)]
  fn cmd_account(base_url: &str, stub_path: &str) -> ManifestAccount {
      ManifestAccount {
          name: "acct1".to_string(),
          default: true,
          fields: BTreeMap::from([
              ("base_url".to_string(), base_url.to_string()),
              ("token".to_string(), format!("{{{{cmd:{stub_path}}}}}")),
          ]),
          env: BTreeMap::new(),
          cmd: BTreeMap::from([("token".to_string(), stub_path.to_string())]),
          digest: "sha256:acct".to_string(),
      }
  }

  #[cfg(unix)]
  fn write_stub(dir: &Path, name: &str, script: &str) -> String {
      use std::os::unix::fs::PermissionsExt;
      let path = dir.join(name);
      std::fs::write(&path, script).unwrap();
      let mut perms = std::fs::metadata(&path).unwrap().permissions();
      perms.set_mode(0o755);
      std::fs::set_permissions(&path, &perms).unwrap();
      path.to_string_lossy().into_owned()
  }

  #[cfg(unix)]
  fn seed_cmd_run(run_dir: &Path, account: ManifestAccount) {
      let mut m = RunExecutionManifest::default();
      m.connectors.push(ManifestConnector {
          name: "cmd-conn".to_string(),
          digest: "sha256:test".to_string(),
          accounts: vec![account],
      });
      m.connector_grants.insert(
          NODE.to_string(),
          vec![ManifestConnectorGrant {
              connector: "cmd-conn".to_string(),
              accounts: vec!["acct1".to_string()],
              functions: vec!["echo".to_string()],
              max_calls: None,
          }],
      );
      manifest::write(run_dir, &m).unwrap();
  }
  ```
  Then the cases:
  ```rust
  #[cfg(unix)]
  #[test]
  fn cmd_secret_is_resolved_and_injected_as_auth_header() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let stubs = tempfile::tempdir().unwrap();
      let stub = write_stub(stubs.path(), "ok-token", "#!/bin/sh\nprintf 'cmd-secret-42\\n'\n");

      let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
      write_cmd_connector(run.path(), &stub);
      seed_cmd_run(run.path(), cmd_account(&server.base_url, &stub));

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(),
          root: root.path(),
          node_id: NODE,
          connector: "cmd-conn",
          function: "echo",
          account: None,
          args: serde_json::json!({}),
          dry_run: false,
      });
      assert!(ok, "expected ok: {value}");
      let req = server.captured_request().expect("server saw a request");
      assert!(req.contains("Authorization: Bearer cmd-secret-42"), "auth header wrong:\n{req}");
  }

  #[cfg(unix)]
  #[test]
  fn cmd_secret_is_redacted_in_result_body() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let stubs = tempfile::tempdir().unwrap();
      let stub = write_stub(stubs.path(), "ok-token", "#!/bin/sh\nprintf 'cmd-secret-42\\n'\n");

      // The service echoes the token back in its body.
      let server = common::spawn_http(200, "OK", &[], r#"{"echo":"cmd-secret-42"}"#.to_string());
      write_cmd_connector(run.path(), &stub);
      seed_cmd_run(run.path(), cmd_account(&server.base_url, &stub));

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(), root: root.path(), node_id: NODE, connector: "cmd-conn",
          function: "echo", account: None, args: serde_json::json!({}), dry_run: false,
      });
      assert!(ok, "expected ok: {value}");
      assert_eq!(value["body"]["echo"], serde_json::json!("[redacted:cmd:token]"));
      assert!(!value.to_string().contains("cmd-secret-42"), "secret leaked: {value}");
  }

  #[cfg(unix)]
  #[test]
  fn cmd_secret_nonzero_exit_is_config_error_naming_account_and_field() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let stubs = tempfile::tempdir().unwrap();
      let stub = write_stub(stubs.path(), "fail-token", "#!/bin/sh\necho 'gh: not logged in' >&2\nexit 4\n");

      write_cmd_connector(run.path(), &stub);
      seed_cmd_run(run.path(), cmd_account("https://unused.example", &stub));

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(), root: root.path(), node_id: NODE, connector: "cmd-conn",
          function: "echo", account: None, args: serde_json::json!({}), dry_run: false,
      });
      assert!(!ok);
      assert_eq!(value["error"]["code"], serde_json::json!("config"));
      let msg = value["error"]["message"].as_str().unwrap();
      assert!(msg.contains("acct1") && msg.contains("token"), "names account and field: {msg}");
      assert!(msg.contains("not logged in"), "includes stderr excerpt: {msg}");
  }

  #[cfg(unix)]
  #[test]
  fn cmd_secret_empty_output_is_config_error() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let stubs = tempfile::tempdir().unwrap();
      let stub = write_stub(stubs.path(), "empty-token", "#!/bin/sh\nexit 0\n");

      write_cmd_connector(run.path(), &stub);
      seed_cmd_run(run.path(), cmd_account("https://unused.example", &stub));

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(), root: root.path(), node_id: NODE, connector: "cmd-conn",
          function: "echo", account: None, args: serde_json::json!({}), dry_run: false,
      });
      assert!(!ok);
      assert_eq!(value["error"]["code"], serde_json::json!("config"));
  }

  #[cfg(unix)]
  #[test]
  fn dry_run_does_not_execute_cmd_secret() {
      let _lock = common::env_lock();
      let run = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let stubs = tempfile::tempdir().unwrap();
      // A stub that touches a sentinel iff it ever runs.
      let sentinel = stubs.path().join("ran.marker");
      let stub = write_stub(
          stubs.path(),
          "sentinel-token",
          &format!("#!/bin/sh\ntouch '{}'\nprintf 'x\\n'\n", sentinel.display()),
      );
      write_cmd_connector(run.path(), &stub);
      seed_cmd_run(run.path(), cmd_account("https://api.example", &stub));

      let (value, ok) = execute(CallRequest {
          run_dir: run.path(), root: root.path(), node_id: NODE, connector: "cmd-conn",
          function: "echo", account: None, args: serde_json::json!({}), dry_run: true,
      });
      assert!(ok, "dry-run should render without secrets: {value}");
      assert!(!sentinel.exists(), "dry-run must not execute the secret command");
  }
  ```
- [ ] Run and confirm failure: `cargo test -p apb-engine --test <suite> cmd_secret` (the redaction label `cmd:token` and the config-error path do not exist yet).
- [ ] Implement `non_secret_fields`. In connector_call.rs (733-740) change the filter to exclude both maps:
  ```rust
  fn non_secret_fields(account: &ManifestAccount) -> BTreeMap<String, String> {
      account
          .fields
          .iter()
          .filter(|(k, _)| {
              !account.env.contains_key(k.as_str()) && !account.cmd.contains_key(k.as_str())
          })
          .map(|(k, v)| (k.clone(), v.clone()))
          .collect()
  }
  ```
- [ ] Implement cmd resolution in `resolve_secrets` (750-767). After the existing `for (field, var) in &account.env { ... }` loop and before `Ok((secrets, redactions))`, add:
  ```rust
      for (field, cmdline) in &account.cmd {
          let value = secrets::resolve_cmd(cmdline, secrets::CMD_SECRET_TIMEOUT)
              .map_err(|e| cmd_secret_error(&account.name, field, e))?;
          // resolve_cmd rejects empty output, so the value is always non-empty
          // and safe to register for redaction. The label carries no secret.
          redactions.push((value.clone(), format!("cmd:{field}")));
          secrets.insert(field.clone(), value);
      }
  ```
- [ ] Add the error mapper near `resolve_secrets`:
  ```rust
  /// Maps a `CmdSecretError` to a `config` call error naming the account and
  /// field and, where the helper produced one, a trimmed stderr excerpt. The
  /// resolved secret is never part of any variant, so nothing sensitive can
  /// reach this message.
  fn cmd_secret_error(account: &str, field: &str, err: secrets::CmdSecretError) -> CallError {
      use secrets::CmdSecretError as E;
      let detail = match err {
          E::Parse(m) => format!("command reference is not valid: {m}"),
          E::Spawn(m) => format!("command could not start: {m}"),
          E::Timeout => "command timed out after 10s".to_string(),
          E::NonZero { code, stderr } => {
              let code = code
                  .map(|c| c.to_string())
                  .unwrap_or_else(|| "signal".to_string());
              if stderr.is_empty() {
                  format!("command exited with status {code}")
              } else {
                  format!("command exited with status {code}: {stderr}")
              }
          }
          E::Empty { stderr } => {
              if stderr.is_empty() {
                  "command produced no output".to_string()
              } else {
                  format!("command produced no output: {stderr}")
              }
          }
      };
      CallError::new(
          CallErrorCode::Config,
          format!("secret for account `{account}` field `{field}`: {detail}"),
      )
  }
  ```
- [ ] Extend the healthcheck account build in `prepare_healthcheck` (593-608). Replace the env-only secret-key computation with one covering both maps, and add `cmd` to the `ManifestAccount` literal:
  ```rust
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
  ```
- [ ] Fix the in-suite `ManifestAccount` literals so the crate's own tests still compile: in crates/apb-engine/tests/suite/connector_call.rs add `cmd: BTreeMap::new(),` to `account()` (line ~65-76), the `q-conn` `acct` literal (~604-615), and the `other` connector's account literal in `max_calls_budget_is_per_connector` (~689-695).
- [ ] Run and confirm pass: `cargo test -p apb-engine --test <suite> cmd_secret` and the full suite `cargo test -p apb-engine`.
- [ ] Format and lint: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -am "engine: resolve command-sourced secrets at call time with interim redaction"` (with the acting-model Co-Authored-By trailer).

---

### Task 6: Prove the policy gate and validation never execute commands

**Files:**
- Modify: crates/apb-mcp/tests/suite/connector_policy.rs (new cmd-secret gate cases)
- Test: crates/apb-core/src/connector/config.rs (validate does not spawn)

**Interfaces:**
- Consumes: `apb_mcp::policy::check_run`, `config::validate_accounts`.
- Asserts: a cmd-only secret field passes the env-presence gate (no env var to miss), the gate reaches the trust check, and no command runs during resolve/gate/validate.

Steps:

- [ ] Write the failing core test. Add to config.rs test module a case proving `validate_accounts` never spawns (a cmd ref that would fail if executed still validates clean):
  ```rust
  #[test]
  fn validate_does_not_execute_the_cmd_ref() {
      let doc = jira_doc();
      // A command that would exit non-zero if run; validation must accept the
      // reference form without executing anything.
      let accounts = vec![acct(
          "prod",
          false,
          &[("base_url", "https://a"), ("token", "{{cmd:false}}")],
      )];
      assert!(validate_accounts(&doc, &accounts).is_empty());
  }
  ```
- [ ] Write the failing MCP gate test. In crates/apb-mcp/tests/suite/connector_policy.rs add a variant that writes a stub touching a sentinel, configures `acct1.token` as `{{cmd:<stub>}}` (drop the env var), approves connector + account, and asserts the gate does not miss env and does not execute the command:
  ```rust
  #[cfg(unix)]
  #[test]
  fn cmd_secret_passes_env_gate_without_executing_command() {
      let _l = lock();
      let cfg = tempfile::tempdir().unwrap();
      let root = tempfile::tempdir().unwrap();
      let stubs = tempfile::tempdir().unwrap();
      let _g = setup(cfg.path(), root.path(), "https://first.example.com");

      // A stub that touches a sentinel iff it ever runs.
      use std::os::unix::fs::PermissionsExt;
      let sentinel = stubs.path().join("ran.marker");
      let stub = stubs.path().join("cmd-token");
      std::fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\nprintf 'x\\n'\n", sentinel.display())).unwrap();
      let mut p = std::fs::metadata(&stub).unwrap().permissions();
      p.set_mode(0o755);
      std::fs::set_permissions(&stub, &p).unwrap();

      // Rewrite acct1 to source its token from the command (no env var).
      let path = config::project_config_path(root.path(), CONNECTOR_NAME);
      std::fs::write(
          &path,
          format!(
              "accounts:\n  - name: acct1\n    base_url: https://first.example.com\n    token: \"{{{{cmd:{}}}}}\"\n  - name: acct2\n    base_url: https://second.example.com\n    token: \"{{{{env.{TOKEN_B}}}}}\"\n",
              stub.to_string_lossy()
          ),
      )
      .unwrap();

      approve_connector();
      approve_accounts(root.path(), &parsed_playbook());

      // The gate must not report a missing env var for the cmd-sourced acct1,
      // and must not execute the command. It permits (acct2's env var is set by
      // `setup`, and both accounts are approved after the rewrite).
      let result = check_run(root.path(), &wref(), false, false);
      assert!(result.is_ok(), "gate should permit: {result:?}");
      assert!(!sentinel.exists(), "the policy gate must never execute a secret command");
  }
  ```
  Note: `approve_accounts` is called AFTER the rewrite so the approved digest matches the cmd-sourced acct1 (the digest already covers the `{{cmd:...}}` string, Task 3).
- [ ] Run and confirm failure first (before Task 3-5 land, these fail; when running this task after them they should compile). Run `cargo test -p apb-mcp --test <suite> cmd_secret_passes_env_gate` and `cargo test -p apb-core --lib validate_does_not_execute`.
- [ ] No production code change is expected here: the gate already only calls `validate_accounts`, `env_refs`-derived `required_env`, and `missing_vars`, none of which execute commands. If any test fails, treat it as a regression signal and use superpowers:systematic-debugging rather than adding gate-time execution.
- [ ] Run and confirm pass: both tests green.
- [ ] Format and lint: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Commit: `git commit --signoff -am "test: pin that policy gate and account validation never execute cmd secrets"` (with the acting-model Co-Authored-By trailer).

---

### Task 7: Final gates

**Files:** none (verification only).

Steps:

- [ ] Full workspace test: `cargo test --workspace`.
- [ ] Format: `cargo fmt --all -- --check`.
- [ ] Clippy: `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Warm the cargo cache then run code-ranker: `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .`; for any violation, `code-ranker docs base <ID>`, fix, and re-run until clean.
- [ ] Confirm no em-dash U+2014, no exclamation marks, and no CJK were introduced in any new doc or user-facing string (grep the diff).
- [ ] Do not commit further or push without the owner's approval.

---

## Deferred / explicitly out of scope for this slice

- The CLI `apb connector` status/doctor path (crates/apb-cli/src/connector.rs:476-487, 619) reports only env-var presence via `env_refs`/`missing_vars`. A cmd-sourced field simply is not listed there and is not flagged missing (safe: it cannot report a false-missing). Spec 4.1 lists "doctor" as a resolution point, but executing helper commands during a passive status check is out of scope for this slice per the shared contract's gate-time no-execution rule; a follow-up may add an explicit opt-in probe. The live healthcheck (Task 5) does resolve cmd secrets, because it is an active reachability probe that already sends real secrets.
- The manifest format for connectors is unchanged except the additive `ManifestAccount.cmd` field (serde-default, backward compatible with existing run manifests).
