# Official Connectors Slice 5: The Four Manifests, Demos, and Gates - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the four wave-1 official connectors (github, telegram, smtp, sentry) as complete, tested `connectors/<name>/` folders at the repository root, two demo playbooks that exercise them end to end, the tier-1 CI manifest gate, the tier-3 live smoke tests, and the docs/CONNECTORS.md updates that document them, per spec `docs/superpowers/specs/2026-07-19-official-connectors-design.md` sections 5, 6, 8.

**Architecture:** Pure content authoring on top of the engine surface slices 1-4 ship (path auth, `headers`, `examples`, `response_pick`, the `smtp` function kind, command-sourced secrets, embedded distribution, `tests.yaml`/`apb connector test`, `apb connector install --from-dir`). No engine code changes in this slice: every task here is a `connector.yaml` + `PUBLIC.md` + `tests.yaml` triple, a demo playbook, or a Rust integration test that shells out to the real `apb` binary (mirroring `crates/apb-cli/tests/suite/connector_cli.rs`'s pattern).

**Tech Stack:** YAML manifests validated by `apb_core::connector::ConnectorDoc`; Rust integration tests in `crates/apb-cli/tests/suite/` (registered in `crates/apb-cli/tests/main.rs`, one binary for the whole crate); `apb connector test --dir <path>` as the offline contract-test runner.

## Global Constraints

- No em-dash (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere in code or prose. Machine-facing fields are English; user-facing chat messages are written in the user's chat language.
- Function and account field names: `[a-z0-9][a-z0-9_]*`, at most 64 chars, unique within the connector (`validate_snake_name`).
- Connector folder names: `[a-z0-9][a-z0-9-]*`, at most 64 chars (`validate_profile_name`, `is_safe_segment`).
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean before a task is done.
- **Before code is ready to commit:** run code-ranker and fix any violation. First warm the cargo cache (`cargo metadata --format-version 1 >/dev/null`), then `code-ranker check .` (exit != 0 on a violation). For a violation, read `code-ranker docs base <ID>` before fixing, fix, and re-run until clean.
- Commits: `git commit --signoff`, ending with a `Co-Authored-By:` trailer for the acting model; never a visible AI-authorship marker in public prose.
- Commit only after the owner approves; never push or upload anything without explicit per-action approval.

## Load-bearing renderer findings (read before authoring any manifest)

1. **The connector template renderer has no "optional" mechanism for `query` map entries or for individually-templated body fields** (`apb_core::connector::template::resolve` errors on any `{{args.x}}` whose `x` is absent from the call args; only the whole-body `body: "{{args}}"` shorthand skips this by passing `ctx.args` through untouched). Consequence applied throughout every manifest: every non-GET function's body is the whole-args shorthand (so genuinely optional fields like `body`, `draft`, `labels`, `cc`, `bcc` work without forcing the caller to pass them, at the cost of harmless extra routing fields such as `owner`/`repo`/`number` riding along in the JSON body, which GitHub/Telegram/Sentry's REST layers ignore); every `query`-templated filter (`state`, `labels`, `page`, `base`, `query`, `project`, `cursor`) is instead marked `required` in `args_schema`, with descriptions telling the agent what value means "no filter" (empty string, or an explicit enum choice).
2. **Implicit dependency on slice 3 (smtp):** the `send_email` manifest relies on the `smtp.message` block tolerating individually-templated optional fields (`cc`, `bcc`, `body_html`) being absent from the call args without erroring. If slice 3's implementation reuses the strict generic resolver for `smtp.message` fields, `send_email` calls that omit `cc`/`bcc`/`body_html` will fail to render; slice 3 must render optional message fields leniently (missing optional arg means field absent). This is recorded in the master plan's cross-slice obligations.
3. **`tests.yaml`'s `envelope` expectation arm** is defined only for a message-carrying function; a `verify: true` smtp function has no message. Resolved: `expect: { envelope: {} }` for `verify` asserts only that the connection block renders without error. (Requires the slice-4 `Envelope` struct's fields to be optional or an empty-envelope form to be accepted by the runner; reconcile with slice 4's `Envelope { from, to, subject }` at execution time - either make those fields `Option`/default in `contract.rs` or give `verify` a dedicated expectation form.)

---

### Task 1: GitHub connector manifest, PUBLIC.md, tests.yaml, and the CI manifest gate scaffold

**Files:**
- Create: `connectors/github/connector.yaml`
- Create: `connectors/github/PUBLIC.md`
- Create: `connectors/github/tests.yaml`
- Create: `crates/apb-cli/tests/suite/official_connectors_gate.rs`
- Modify: `crates/apb-cli/tests/main.rs` (register the new module)
- Modify: `crates/apb-cli/Cargo.toml` (add `serde_yaml_ng` and `jsonschema` dev-dependencies)

**Interfaces:** `apb_core::connector::{ConnectorDoc, PublicMeta}`, `apb_core::connector::template::{Namespace, placeholders}`, `apb_core::content::{TreeLimits, tree_digest}`, CLI `apb connector test --dir <path>`.

Steps:

- [ ] Add dev-dependencies to `crates/apb-cli/Cargo.toml`:
  ```toml
  [dev-dependencies]
  assert_cmd = "2.2.2"
  predicates = "3.1.4"
  tempfile = "3.27.0"
  serde_yaml_ng.workspace = true
  jsonschema.workspace = true
  ```
- [ ] Create the CI manifest gate test file first (TDD: it will fail until `connectors/github/` exists). Write `crates/apb-cli/tests/suite/official_connectors_gate.rs`:
  ```rust
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
      std::fs::read_to_string(path)
          .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
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
      let rest = public_md
          .strip_prefix("---\n")
          .unwrap_or_else(|| panic!("connectors/{name}/PUBLIC.md must start with a `---` frontmatter block"));
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
          assert!(digest.starts_with("sha256:"), "{name} digest malformed: {digest}");

          // 3. PUBLIC.md frontmatter parses and carries a display_name/summary.
          let public_path = dir.join("PUBLIC.md");
          assert!(public_path.is_file(), "connectors/{name}/PUBLIC.md is missing");
          let public = read(&public_path);
          let fm = frontmatter(&public, &name);
          let meta: PublicMeta = serde_yaml_ng::from_str(&fm)
              .unwrap_or_else(|e| panic!("connectors/{name}/PUBLIC.md frontmatter: {e}"));
          assert!(!meta.display_name.is_empty(), "{name} PUBLIC.md needs display_name");
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
                  .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
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
              if f.smtp.is_some() {
                  assert!(
                      f.response_pick.is_empty(),
                      "{name}/{}: smtp functions must not set response_pick",
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
  ```
- [ ] Register the module in `crates/apb-cli/tests/main.rs`:
  ```rust
  #[path = "suite/official_connectors_gate.rs"]
  mod official_connectors_gate;
  ```
- [ ] Run `cargo test -p apb-cli --test main official_connectors_gate` and confirm it fails (nothing under `connectors/` yet beyond the slice-4 `example` seed, which the gate rules may fail on `response_pick`; remove `connectors/example/` in this task, re-pointing the slice-4 CLI tests that referenced it at `github` - the handoff slice 4's plan recorded).
- [ ] Write `connectors/github/tests.yaml` (one case per function, all 21 functions). Full content:
  ```yaml
  cases:
    - function: get_rate_limit
      account: { api_base: https://api.github.com }
      args: {}
      expect:
        method: GET
        url: https://api.github.com/rate_limit
        headers:
          Accept: application/vnd.github+json
          X-GitHub-Api-Version: "2022-11-28"

    - function: list_issues
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, state: open, labels: "", page: 1 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/issues?labels=&page=1&state=open

    - function: get_issue
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 42 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/issues/42

    - function: create_issue
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, title: "Broken build" }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/issues
        body_contains: { title: "Broken build" }

    - function: update_issue
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 42, state: closed }
      expect:
        method: PATCH
        url: https://api.github.com/repos/acme/site/issues/42
        body_contains: { state: closed }

    - function: comment_issue
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 42, body: "Looking into this." }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/issues/42/comments
        body_contains: { body: "Looking into this." }

    - function: add_labels
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 42, labels: [bug] }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/issues/42/labels
        body_contains: { labels: [bug] }

    - function: remove_label
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 42, name: bug }
      expect:
        method: DELETE
        url: https://api.github.com/repos/acme/site/issues/42/labels/bug

    - function: add_assignees
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 42, assignees: [octocat] }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/issues/42/assignees
        body_contains: { assignees: [octocat] }

    - function: list_pulls
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, state: open, base: "", page: 1 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/pulls?base=&page=1&state=open

    - function: get_pull
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 7 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/pulls/7

    - function: create_pull
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, title: "Add feature", head: feature-branch, base: main }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/pulls
        body_contains: { title: "Add feature", head: feature-branch, base: main }

    - function: merge_pull
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 7 }
      expect:
        method: PUT
        url: https://api.github.com/repos/acme/site/pulls/7/merge
        body_contains: { number: 7 }

    - function: request_reviewers
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 7, reviewers: [octocat] }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/pulls/7/requested_reviewers
        body_contains: { reviewers: [octocat] }

    - function: create_review
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, number: 7, event: APPROVE }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/pulls/7/reviews
        body_contains: { event: APPROVE }

    - function: create_release
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, tag_name: v1.4.0 }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/releases
        body_contains: { tag_name: v1.4.0 }

    - function: get_release_by_tag
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, tag: v1.4.0 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/releases/tags/v1.4.0

    - function: dispatch_workflow
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, workflow_file: release.yml, ref: main }
      expect:
        method: POST
        url: https://api.github.com/repos/acme/site/actions/workflows/release.yml/dispatches
        body_contains: { ref: main }

    - function: list_workflow_runs
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, workflow_file: release.yml, page: 1 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/actions/workflows/release.yml/runs?page=1

    - function: list_check_runs
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, ref: abc123 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/commits/abc123/check-runs

    - function: get_combined_status
      account: { api_base: https://api.github.com }
      args: { owner: acme, repo: site, ref: abc123 }
      expect:
        method: GET
        url: https://api.github.com/repos/acme/site/commits/abc123/status
  ```
- [ ] Run `apb connector test --dir connectors/github` and confirm it fails: no `connector.yaml` yet.
- [ ] Write `connectors/github/connector.yaml` with all 21 functions (auth: header Bearer; account fields api_base + token secret; every function carries `Accept: application/vnd.github+json` and `X-GitHub-Api-Version: "2022-11-28"` headers; every non-GET body is the `body: "{{args}}"` shorthand; every read_only function declares `response_pick`). Full content:
  ```yaml
  name: github
  version: 0.1.0
  healthcheck: get_rate_limit
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
    - name: get_rate_limit
      description: Check the current GitHub API rate limit status
      read_only: true
      method: GET
      url: "{{account.api_base}}/rate_limit"
      args_schema: { type: object, properties: {}, required: [] }
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [resources.core.limit, resources.core.remaining, resources.core.reset]

    - name: list_issues
      description: List issues in a repository, filtered by state, labels, and page
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues"
      query:
        state: "{{args.state}}"
        labels: "{{args.labels}}"
        page: "{{args.page}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          state: { type: string, enum: [open, closed, all] }
          labels: { type: string, description: "comma-separated label names, or empty for no label filter" }
          page: { type: integer, minimum: 1 }
        required: [owner, repo, state, labels, page]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [number, title, state, html_url, user.login, labels.name]

    - name: get_issue
      description: Get one issue by number
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues/{{args.number}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
        required: [owner, repo, number]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [number, title, state, html_url, user.login, labels.name, body, comments]

    - name: create_issue
      description: Create an issue in a repository
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          title: { type: string }
          body: { type: string }
          labels: { type: array, items: { type: string } }
          assignees: { type: array, items: { type: string } }
        required: [owner, repo, title]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      examples:
        - args: { owner: acme, repo: site, title: "Broken build on main", labels: [bug, priority-high] }
          note: "labels is a list of existing label names; a label that does not exist yet must be created first with add_labels or in the repository settings."

    - name: update_issue
      description: Update an issue's title, body, state, labels, or assignees
      method: PATCH
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues/{{args.number}}"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          title: { type: string }
          body: { type: string }
          state: { type: string, enum: [open, closed] }
          labels: { type: array, items: { type: string } }
          assignees: { type: array, items: { type: string } }
        required: [owner, repo, number]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: comment_issue
      description: Post a comment on an issue or pull request
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues/{{args.number}}/comments"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          body: { type: string }
        required: [owner, repo, number, body]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: add_labels
      description: Add one or more labels to an issue or pull request
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues/{{args.number}}/labels"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          labels: { type: array, items: { type: string } }
        required: [owner, repo, number, labels]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      examples:
        - args: { owner: acme, repo: site, number: 42, labels: [needs-triage] }
          note: "a label that does not already exist on the repository is created automatically."

    - name: remove_label
      description: Remove one label from an issue or pull request
      method: DELETE
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues/{{args.number}}/labels/{{args.name}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          name: { type: string }
        required: [owner, repo, number, name]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: add_assignees
      description: Add one or more assignees to an issue or pull request
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/issues/{{args.number}}/assignees"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          assignees: { type: array, items: { type: string } }
        required: [owner, repo, number, assignees]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: list_pulls
      description: List pull requests in a repository, filtered by state, base branch, and page
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/pulls"
      query:
        state: "{{args.state}}"
        base: "{{args.base}}"
        page: "{{args.page}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          state: { type: string, enum: [open, closed, all] }
          base: { type: string, description: "base branch name, or empty for no base filter" }
          page: { type: integer, minimum: 1 }
        required: [owner, repo, state, base, page]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [number, title, state, html_url, user.login, base.ref, head.ref, draft]

    - name: get_pull
      description: Get one pull request by number
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/pulls/{{args.number}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
        required: [owner, repo, number]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [number, title, state, html_url, user.login, mergeable, merged, additions, deletions, changed_files]

    - name: create_pull
      description: Open a pull request
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/pulls"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          title: { type: string }
          head: { type: string }
          base: { type: string }
          body: { type: string }
          draft: { type: boolean }
        required: [owner, repo, title, head, base]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: merge_pull
      description: Merge a pull request
      method: PUT
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/pulls/{{args.number}}/merge"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          merge_method: { type: string, enum: [merge, squash, rebase] }
          commit_title: { type: string }
          commit_message: { type: string }
          sha: { type: string }
        required: [owner, repo, number]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: request_reviewers
      description: Request one or more reviewers on a pull request
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/pulls/{{args.number}}/requested_reviewers"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          reviewers: { type: array, items: { type: string } }
          team_reviewers: { type: array, items: { type: string } }
        required: [owner, repo, number, reviewers]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: create_review
      description: Submit a pull request review (approve, request changes, or comment)
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/pulls/{{args.number}}/reviews"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          number: { type: integer }
          event: { type: string, enum: [APPROVE, REQUEST_CHANGES, COMMENT] }
          body: { type: string }
          commit_id: { type: string }
        required: [owner, repo, number, event]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: create_release
      description: Create a release
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/releases"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          tag_name: { type: string }
          target_commitish: { type: string }
          name: { type: string }
          body: { type: string }
          draft: { type: boolean }
          prerelease: { type: boolean }
          generate_release_notes: { type: boolean }
        required: [owner, repo, tag_name]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"

    - name: get_release_by_tag
      description: Get a release by its tag name
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/releases/tags/{{args.tag}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          tag: { type: string }
        required: [owner, repo, tag]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [id, tag_name, name, html_url, draft, prerelease, published_at]

    - name: dispatch_workflow
      description: Trigger a workflow_dispatch run of a GitHub Actions workflow
      method: POST
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/actions/workflows/{{args.workflow_file}}/dispatches"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          workflow_file: { type: string, description: "workflow file name, e.g. release.yml, or its numeric id" }
          ref: { type: string, description: "branch or tag to run the workflow on" }
          inputs: { type: object, description: "must match the workflow_dispatch inputs declared in the workflow file" }
        required: [owner, repo, workflow_file, ref]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      examples:
        - args: { owner: acme, repo: site, workflow_file: release.yml, ref: main, inputs: { environment: production } }
          note: "omit inputs entirely when the workflow file declares none."

    - name: list_workflow_runs
      description: List recent runs of a workflow
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/actions/workflows/{{args.workflow_file}}/runs"
      query:
        page: "{{args.page}}"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          workflow_file: { type: string }
          page: { type: integer, minimum: 1 }
        required: [owner, repo, workflow_file, page]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [workflow_runs.id, workflow_runs.name, workflow_runs.status, workflow_runs.conclusion, workflow_runs.html_url, workflow_runs.run_number]

    - name: list_check_runs
      description: List check runs for a commit
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/commits/{{args.ref}}/check-runs"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          ref: { type: string }
        required: [owner, repo, ref]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [check_runs.id, check_runs.name, check_runs.status, check_runs.conclusion, check_runs.html_url]

    - name: get_combined_status
      description: Get the combined commit status (all status checks) for a commit
      read_only: true
      method: GET
      url: "{{account.api_base}}/repos/{{args.owner}}/{{args.repo}}/commits/{{args.ref}}/status"
      args_schema:
        type: object
        properties:
          owner: { type: string }
          repo: { type: string }
          ref: { type: string }
        required: [owner, repo, ref]
      headers:
        Accept: application/vnd.github+json
        X-GitHub-Api-Version: "2022-11-28"
      response_pick: [state, sha, statuses.state, statuses.context, statuses.description, statuses.target_url]
  ```
- [ ] Write `connectors/github/PUBLIC.md`:
  ```markdown
  ---
  display_name: GitHub
  summary: Manage GitHub issues, pull requests, releases, and workflow runs from a playbook.
  tags: [github, issues, pull-requests, ci, releases]
  publisher: apb
  ---

  The GitHub connector covers issue and pull request triage, releases, and
  Actions workflow dispatch over the REST API. Owner, repository, and issue
  or pull request numbers are call arguments, not account fields, so one
  account serves every repository the token can reach.

  ## Account setup

  Two account fields: `api_base` (`https://api.github.com` for github.com,
  or your GitHub Enterprise Server API base for GHES) and `token` (secret).

  The recommended token source is the GitHub CLI, already authenticated on
  most developer machines:

  ```yaml
  accounts:
    - name: default
      api_base: https://api.github.com
      token: "{{cmd:gh auth token}}"
  ```

  Run `gh auth login` first if you have not. Without `gh`, use a personal
  access token in `GITHUB_TOKEN`:

  ```yaml
  accounts:
    - name: default
      api_base: https://api.github.com
      token: "{{env.GITHUB_TOKEN}}"
  ```

  The token needs the `repo` scope (or `public_repo` for public
  repositories only) for issue, pull request, and release functions, and
  the `workflow` scope for `dispatch_workflow`.

  ## Healthcheck

  `get_rate_limit` probes the token and reports the remaining API quota.

  ## Excluded on purpose

  GraphQL-only operations (marking a pull request ready for review),
  reactions, deployments, and every webhook are out of scope for this
  connector; the format is REST-only in this wave.
  ```
- [ ] Run `apb connector test --dir connectors/github` and confirm all 21 cases pass.
- [ ] Run `cargo test -p apb-cli --test main official_connectors_gate` and confirm it now passes for `github` alone.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, warm cache and `code-ranker check .`.
- [ ] Commit: `git commit --signoff` (github connector + CI gate scaffold), acting-model Co-Authored-By trailer.

---

### Task 2: Telegram connector manifest, PUBLIC.md, tests.yaml

**Files:**
- Create: `connectors/telegram/connector.yaml`
- Create: `connectors/telegram/PUBLIC.md`
- Create: `connectors/telegram/tests.yaml`

**Interfaces:** `AuthSpec::Path { value_template }`, the `{{auth}}` URL placeholder.

Steps:

- [ ] Write `connectors/telegram/tests.yaml`:
  ```yaml
  cases:
    - function: get_me
      account: { api_base: https://api.telegram.org }
      args: {}
      expect:
        method: GET
        url: "https://api.telegram.org/{{auth}}/getMe"

    - function: send_message
      account: { api_base: https://api.telegram.org }
      args: { chat_id: "123456", text: "Deploy finished" }
      expect:
        method: POST
        url: "https://api.telegram.org/{{auth}}/sendMessage"
        body_contains: { text: "Deploy finished" }

    - function: edit_message_text
      account: { api_base: https://api.telegram.org }
      args: { chat_id: "123456", message_id: 42, text: "Deploy finished (updated)" }
      expect:
        method: POST
        url: "https://api.telegram.org/{{auth}}/editMessageText"
        body_contains: { message_id: 42 }

    - function: get_chat
      account: { api_base: https://api.telegram.org }
      args: { chat_id: "123456" }
      expect:
        method: POST
        url: "https://api.telegram.org/{{auth}}/getChat"
        body_contains: { chat_id: "123456" }

    - function: get_updates
      account: { api_base: https://api.telegram.org }
      args: {}
      expect:
        method: POST
        url: "https://api.telegram.org/{{auth}}/getUpdates"

    - function: answer_callback_query
      account: { api_base: https://api.telegram.org }
      args: { callback_query_id: abc }
      expect:
        method: POST
        url: "https://api.telegram.org/{{auth}}/answerCallbackQuery"
        body_contains: { callback_query_id: abc }
  ```
- [ ] Run `apb connector test --dir connectors/telegram` and confirm it fails (no manifest yet).
- [ ] Write `connectors/telegram/connector.yaml`:
  ```yaml
  name: telegram
  version: 0.1.0
  healthcheck: get_me
  auth:
    kind: path
    value_template: "bot{{secret.token}}"
  account_fields:
    - name: api_base
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: get_me
      description: Verify the bot token and get the bot's own identity
      read_only: true
      method: GET
      url: "{{account.api_base}}/{{auth}}/getMe"
      args_schema: { type: object, properties: {}, required: [] }
      response_pick: [ok, result.id, result.username, result.first_name, result.is_bot]

    - name: send_message
      description: Send a text message to a chat
      method: POST
      url: "{{account.api_base}}/{{auth}}/sendMessage"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          chat_id: { type: string, description: "numeric chat id (quoted) or @channelusername" }
          text: { type: string }
          parse_mode: { type: string, enum: [MarkdownV2, HTML] }
        required: [chat_id, text]
      examples:
        - args: { chat_id: "-1001234567890", text: "*Build failed* on `main`", parse_mode: MarkdownV2 }
          note: "MarkdownV2 requires escaping reserved punctuation (underscore, asterisk, brackets, and others) per the Telegram Bot API formatting spec; plain text with no parse_mode is safer when the content is not authored by hand."

    - name: edit_message_text
      description: Edit the text of a previously sent message
      method: POST
      url: "{{account.api_base}}/{{auth}}/editMessageText"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          chat_id: { type: string }
          message_id: { type: integer }
          text: { type: string }
          parse_mode: { type: string, enum: [MarkdownV2, HTML] }
        required: [chat_id, message_id, text]

    - name: get_chat
      description: Get information about a chat
      read_only: true
      method: POST
      url: "{{account.api_base}}/{{auth}}/getChat"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          chat_id: { type: string }
        required: [chat_id]
      response_pick: [ok, result.id, result.type, result.title, result.username]

    - name: get_updates
      description: Long-poll for new updates (messages, callback queries) since the last offset
      read_only: true
      timeout_sec: 75
      method: POST
      url: "{{account.api_base}}/{{auth}}/getUpdates"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          offset: { type: integer, description: "update_id of the last processed update, plus one" }
          timeout: { type: integer, minimum: 0, maximum: 60, description: "long-poll seconds, capped below the function timeout" }
        required: []
      response_pick: [ok, result.update_id, result.message.message_id, result.message.text, result.message.chat.id, result.message.from.username]

    - name: answer_callback_query
      description: Acknowledge an inline keyboard button press
      method: POST
      url: "{{account.api_base}}/{{auth}}/answerCallbackQuery"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          callback_query_id: { type: string }
          text: { type: string }
          show_alert: { type: boolean }
        required: [callback_query_id]
  ```
- [ ] Write `connectors/telegram/PUBLIC.md`:
  ```markdown
  ---
  display_name: Telegram
  summary: Send and read messages through a Telegram bot.
  tags: [telegram, messaging, notifications]
  publisher: apb
  ---

  The Telegram connector wraps a Bot API bot: send messages, edit them,
  inspect a chat, poll for updates, and answer inline keyboard callbacks.
  There is no webhook support in this wave; `get_updates` is the
  pull-based way to react to replies inside a playbook node.

  ## Account setup

  Create a bot with [@BotFather](https://t.me/BotFather) in Telegram
  (`/newbot`), then store the token it gives you:

  ```yaml
  accounts:
    - name: default
      api_base: https://api.telegram.org
      token: "{{env.TELEGRAM_BOT_TOKEN}}"
  ```

  `api_base` is overridable for a self-hosted Bot API server; leave it as
  `https://api.telegram.org` otherwise.

  Before `send_message` works on a chat, the bot must already be a member
  of it (added to a group, or the user has started a conversation with it
  directly).

  ## Healthcheck

  `get_me` confirms the token resolves to a real bot.
  ```
- [ ] Run `apb connector test --dir connectors/telegram` and confirm all 6 cases pass.
- [ ] Run `cargo test -p apb-cli --test main official_connectors_gate` (now covers github + telegram).
- [ ] `cargo fmt`, `cargo clippy`, `code-ranker check .`.
- [ ] Commit.

---

### Task 3: SMTP connector manifest, PUBLIC.md, tests.yaml

**Files:**
- Create: `connectors/smtp/connector.yaml`
- Create: `connectors/smtp/PUBLIC.md`
- Create: `connectors/smtp/tests.yaml`

**Interfaces:** `SmtpSpec { connection: SmtpConnection, message: Option<SmtpMessage>, verify: bool }`.

Steps:

- [ ] Write `connectors/smtp/tests.yaml`:
  ```yaml
  cases:
    - function: verify
      account: { host: smtp.example.com, port: "587", from_email: releases@example.com, from_name: Release Bot, use_tls: "true" }
      args: {}
      expect:
        envelope: {}

    - function: send_email
      account: { host: smtp.example.com, port: "587", from_email: releases@example.com, from_name: Release Bot, use_tls: "true" }
      args: { to: ops@example.com, subject: "Release 1.4.0 is live", body_text: "Release shipped." }
      expect:
        envelope: { from: releases@example.com, to: [ops@example.com], subject: "Release 1.4.0 is live" }
  ```
  Note: `verify` carries no message, so its case checks an empty `envelope: {}` (connection renders and the offline test harness runs the function without error); reconcile the empty-envelope form with slice 4's `Envelope` struct at execution time (make its fields optional or add a dedicated verify expectation).
- [ ] Run `apb connector test --dir connectors/smtp` and confirm it fails (no manifest yet).
- [ ] Write `connectors/smtp/connector.yaml`:
  ```yaml
  name: smtp
  version: 0.1.0
  healthcheck: verify
  account_fields:
    - name: host
      required: true
    - name: port
      required: true
    - name: username
      required: false
    - name: password
      required: false
      secret: true
    - name: from_email
      required: true
    - name: from_name
      required: false
    - name: use_tls
      required: false
  functions:
    - name: verify
      description: Probe the SMTP connection (connect, EHLO, STARTTLS, AUTH) without sending a message
      read_only: true
      smtp:
        connection:
          host: "{{account.host}}"
          port: "{{account.port}}"
          use_tls: "{{account.use_tls}}"
          username: "{{account.username}}"
          password: "{{secret.password}}"
        verify: true
      args_schema: { type: object, properties: {}, required: [] }

    - name: send_email
      description: Send an email over SMTP with STARTTLS
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
      args_schema:
        type: object
        properties:
          to: { type: string, description: "comma-separated list of recipient addresses" }
          cc: { type: string, description: "comma-separated list, optional" }
          bcc: { type: string, description: "comma-separated list, optional" }
          subject: { type: string }
          body_text: { type: string }
          body_html: { type: string, description: "optional; combines with body_text into a multipart message when both are present" }
        required: [to, subject, body_text]
      examples:
        - args: { to: ops@example.com, subject: "Release 1.4.0 is live", body_text: "Release 1.4.0 shipped. See the changelog for details.", cc: team@example.com }
          note: "to, cc, and bcc each accept a comma-separated list of addresses; add body_html alongside body_text for a richer rendering, leaving body_text as the plain-text fallback."
  ```
- [ ] Write `connectors/smtp/PUBLIC.md`:
  ```markdown
  ---
  display_name: SMTP Email
  summary: Send transactional email over SMTP with STARTTLS.
  tags: [email, smtp, notifications]
  publisher: apb
  ---

  A single-purpose connector: one account, two functions (`verify` and
  `send_email`). Any SMTP relay works, including a transactional email
  provider's SMTP endpoint or a mailbox provider's app-password relay.

  ## Account setup

  ```yaml
  accounts:
    - name: default
      host: smtp.example.com
      port: "587"
      username: releases@example.com
      from_email: releases@example.com
      from_name: Release Bot
      use_tls: true
      password: "{{env.SMTP_PASSWORD}}"
  ```

  `use_tls` and `username`/`password` are schema-optional (a local
  unauthenticated relay needs neither), but set `use_tls` explicitly:
  the account field carries no engine-level default, so an account that
  omits it fails a call cleanly rather than assuming STARTTLS. Set it to
  `true` for the common case (STARTTLS on port 587) and only to `false`
  for a trusted local relay with no encryption.

  For Gmail, generate an app password and use `smtp.gmail.com` port 587.

  ## Healthcheck

  `verify` connects, negotiates STARTTLS when `use_tls` is set, and
  authenticates when credentials are present, without sending a message.
  ```
- [ ] Run `apb connector test --dir connectors/smtp` and confirm both cases pass.
- [ ] Run `cargo test -p apb-cli --test main official_connectors_gate` (now covers github + telegram + smtp).
- [ ] `cargo fmt`, `cargo clippy`, `code-ranker check .`.
- [ ] Commit.

---

### Task 4: Sentry connector manifest, PUBLIC.md, tests.yaml

**Files:**
- Create: `connectors/sentry/connector.yaml`
- Create: `connectors/sentry/PUBLIC.md`
- Create: `connectors/sentry/tests.yaml`

Steps:

- [ ] Write `connectors/sentry/tests.yaml`:
  ```yaml
  cases:
    - function: list_projects
      account: { base_url: https://sentry.io, org: acme }
      args: {}
      expect:
        method: GET
        url: https://sentry.io/api/0/organizations/acme/projects/

    - function: list_issues
      account: { base_url: https://sentry.io, org: acme }
      args: { query: "is:unresolved", project: "-1", cursor: "" }
      expect:
        method: GET
        url: "https://sentry.io/api/0/organizations/acme/issues/?cursor=&project=-1&query=is%3Aunresolved"

    - function: get_issue
      account: { base_url: https://sentry.io, org: acme }
      args: { issue_id: "123456" }
      expect:
        method: GET
        url: https://sentry.io/api/0/issues/123456/

    - function: update_issue
      account: { base_url: https://sentry.io, org: acme }
      args: { issue_id: "123456", status: resolved }
      expect:
        method: PUT
        url: https://sentry.io/api/0/issues/123456/
        body_contains: { status: resolved }

    - function: create_release
      account: { base_url: https://sentry.io, org: acme }
      args: { version: 1.4.0, projects: [site] }
      expect:
        method: POST
        url: https://sentry.io/api/0/organizations/acme/releases/
        body_contains: { version: 1.4.0, projects: [site] }

    - function: create_deploy
      account: { base_url: https://sentry.io, org: acme }
      args: { version: 1.4.0, environment: production }
      expect:
        method: POST
        url: https://sentry.io/api/0/organizations/acme/releases/1.4.0/deploys/
        body_contains: { environment: production }
  ```
- [ ] Run `apb connector test --dir connectors/sentry` and confirm it fails (no manifest yet).
- [ ] Write `connectors/sentry/connector.yaml`:
  ```yaml
  name: sentry
  version: 0.1.0
  healthcheck: list_projects
  auth:
    kind: header
    header: Authorization
    value_template: "Bearer {{secret.token}}"
  account_fields:
    - name: base_url
      required: true
    - name: org
      required: true
    - name: token
      required: true
      secret: true
  functions:
    - name: list_projects
      description: List projects in the organization
      read_only: true
      method: GET
      url: "{{account.base_url}}/api/0/organizations/{{account.org}}/projects/"
      args_schema: { type: object, properties: {}, required: [] }
      response_pick: [id, slug, name, platform]

    - name: list_issues
      description: Search issues in the organization, filtered by project, query, and cursor
      read_only: true
      method: GET
      url: "{{account.base_url}}/api/0/organizations/{{account.org}}/issues/"
      query:
        query: "{{args.query}}"
        project: "{{args.project}}"
        cursor: "{{args.cursor}}"
      args_schema:
        type: object
        properties:
          query: { type: string, description: "Sentry search syntax, e.g. is:unresolved" }
          project: { type: string, description: "numeric project id or slug; -1 searches every project the token can access" }
          cursor: { type: string, description: "pass the cursor from the previous response's link field; empty on the first call" }
        required: [query, project, cursor]
      examples:
        - args: { query: "is:unresolved", project: "-1", cursor: "" }
          note: "project accepts a numeric project id or -1 for every project the token can access; leave cursor empty on the first call and pass the value from the previous response's link field afterward."
      response_pick: [id, shortId, title, status, level, count, culprit, permalink]

    - name: get_issue
      description: Get one issue by internal id
      read_only: true
      method: GET
      url: "{{account.base_url}}/api/0/issues/{{args.issue_id}}/"
      args_schema:
        type: object
        properties:
          issue_id: { type: string }
        required: [issue_id]
      response_pick: [id, shortId, title, status, level, count, culprit, permalink, firstSeen, lastSeen]

    - name: update_issue
      description: Update an issue's status or assignee
      method: PUT
      url: "{{account.base_url}}/api/0/issues/{{args.issue_id}}/"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          issue_id: { type: string }
          status: { type: string, enum: [unresolved, resolved, ignored] }
          assignedTo: { type: string, description: "username, email, or team slug" }
        required: [issue_id]
      examples:
        - args: { issue_id: "123456", status: resolved }
          note: "status and assignedTo can be set in the same call."

    - name: create_release
      description: Create a release
      method: POST
      url: "{{account.base_url}}/api/0/organizations/{{account.org}}/releases/"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          version: { type: string }
          projects: { type: array, items: { type: string } }
          ref: { type: string }
          url: { type: string }
        required: [version, projects]

    - name: create_deploy
      description: Record a deploy of a release to an environment
      method: POST
      url: "{{account.base_url}}/api/0/organizations/{{account.org}}/releases/{{args.version}}/deploys/"
      body: "{{args}}"
      args_schema:
        type: object
        properties:
          version: { type: string }
          environment: { type: string }
          name: { type: string }
          url: { type: string }
        required: [version, environment]
  ```
- [ ] Write `connectors/sentry/PUBLIC.md`:
  ```markdown
  ---
  display_name: Sentry
  summary: Triage Sentry issues and record releases and deploys from a playbook.
  tags: [sentry, error-tracking, releases]
  publisher: apb
  ---

  Covers issue search and triage plus release and deploy bookkeeping.
  Alert rules, webhooks, and cross-connector issue linking are out of
  scope for this connector; do that orchestration in the playbook.

  ## Account setup

  Three account fields: `base_url` (`https://sentry.io`, or your
  self-hosted URL), `org` (the organization slug), and `token` (secret).

  ```yaml
  accounts:
    - name: default
      base_url: https://sentry.io
      org: acme
      token: "{{env.SENTRY_TOKEN}}"
  ```

  Create the token at Settings > Auth Tokens with scopes `project:read`,
  `event:read`, and `issue:write` for the issue functions, plus
  `project:releases` for `create_release` and `create_deploy`.

  ## Pagination

  `list_issues` takes an explicit `cursor` argument; read the next
  cursor from the call result's `link` field and pass it back on the
  following call.

  ## Healthcheck

  `list_projects` confirms the token and organization slug resolve.
  ```
- [ ] Run `apb connector test --dir connectors/sentry` and confirm all 6 cases pass.
- [ ] Run `cargo test -p apb-cli --test main official_connectors_gate` and confirm it now passes for all four connectors.
- [ ] `cargo fmt`, `cargo clippy`, `code-ranker check .`.
- [ ] Commit.

---

### Task 5: Demo playbooks and their CI validate test

**Files:**
- Create: `examples/playbooks/sentry-triage.yaml`
- Create: `examples/playbooks/release-announce.yaml`
- Create: `crates/apb-cli/tests/suite/demo_playbooks_test.rs`
- Modify: `crates/apb-cli/tests/main.rs`

**Interfaces:** `apb connector install --from-dir <path>`, the connector approve flow, `apb validate <id>`.

Steps:

- [ ] Write `examples/playbooks/sentry-triage.yaml` (schema 2, params for sentry_project / github_owner / github_repo / telegram_chat_id, nodes: start -> fetch_issues (sentry list_issues, max_calls 5) -> triage (agent assessment) -> file_github_issues (github create_issue, max_calls 10) -> notify_telegram (telegram send_message, max_calls 3) -> finish; full YAML as authored in the planning transcript, using grant allowlists on every connector binding).
- [ ] Write `examples/playbooks/release-announce.yaml` (schema 2, params for github_owner / github_repo / release_tag / release_notes / announce_email / telegram_chat_id, nodes: start -> create_release (github, max_calls 3) -> announce_email (smtp send_email, max_calls 3) -> announce_telegram (telegram send_message, max_calls 3) -> finish; effects [network, external, irreversible]).
- [ ] Write `crates/apb-cli/tests/suite/demo_playbooks_test.rs`: a `setup` that runs `apb init`, installs the four repo connectors `--from-dir`, approves each (connector and its fake `default` account - verify the exact approve CLI form against the shipped `apb connector`/trust commands at execution time), writes fake non-secret account configs (github/telegram/smtp/sentry with `{{env.*}}` secrets), a `register_playbook` helper copying the YAML into `.apb/playbooks/<id>/<version>/playbook.yaml` plus a `current` marker, and two tests running `apb validate sentry-triage` / `apb validate release-announce` and asserting success.
- [ ] Register the module in `crates/apb-cli/tests/main.rs`:
  ```rust
  #[path = "suite/demo_playbooks_test.rs"]
  mod demo_playbooks_test;
  ```
- [ ] Run `cargo test -p apb-cli --test main demo_playbooks_test` and confirm both tests pass.
- [ ] `cargo fmt`, `cargo clippy`, `code-ranker check .`.
- [ ] Commit.

---

### Task 6: Live smoke tests (tier 3)

**Files:**
- Create: `crates/apb-cli/tests/suite/live_smoke_test.rs`
- Modify: `crates/apb-cli/tests/main.rs`

**Interfaces:** `apb connector install --from-dir`, the connector approve flow, `apb run`, `apb connector call` (invoked from the stub agent script).

Steps:

- [ ] Write `crates/apb-cli/tests/suite/live_smoke_test.rs`. Each test is `#[ignore]`, gated by its own `APB_LIVE_TEST_*` flag plus the real credentials the account needs; skips (prints a message, returns) rather than failing when the flag or credentials are absent. Each test installs the connector `--from-dir`, approves it and a real account sourced from env vars, then runs a one-node playbook whose stub agent script shells out to `apb connector call` for the healthcheck and one read-only function, asserting both calls report `"ok":true`. Four tests:
  - `live_github_healthcheck_and_list_issues` - gate `APB_LIVE_TEST_GITHUB`; needs `GITHUB_TOKEN`, `GITHUB_TEST_OWNER`, `GITHUB_TEST_REPO`; calls `get_rate_limit` then `list_issues`.
  - `live_telegram_healthcheck_and_get_chat` - gate `APB_LIVE_TEST_TELEGRAM`; needs `TELEGRAM_BOT_TOKEN`, `TELEGRAM_TEST_CHAT_ID`; calls `get_me` then `get_chat`.
  - `live_smtp_verify_and_send` - gate `APB_LIVE_TEST_SMTP`; needs `SMTP_TEST_HOST/_PORT/_USERNAME/_PASSWORD/_FROM/_TO`; calls `verify` then `send_email`.
  - `live_sentry_healthcheck_and_list_issues` - gate `APB_LIVE_TEST_SENTRY`; needs `SENTRY_TOKEN`, `SENTRY_TEST_ORG`, `SENTRY_TEST_PROJECT`; calls `list_projects` then `list_issues`.
  Shared helpers: `run_live_probe(connector, account_yaml, healthcheck, function, args)` that does the install/approve/config/stub-agent/one-node-playbook/run sequence (full code as authored in the planning transcript), `write_stub_agent` producing a `#!/bin/sh` script that loops over the two functions via `apb connector call` and greps `"ok":true`, and a seeded `main` profile. Verify the stub-agent invocation mechanism (`APB_AGENT_CMD` or the repo's actual stub-adapter hook) against the shipped adapter at execution time.
- [ ] Register the module in `crates/apb-cli/tests/main.rs`:
  ```rust
  #[path = "suite/live_smoke_test.rs"]
  mod live_smoke_test;
  ```
- [ ] Run `cargo test -p apb-cli --test main live_smoke_test` (no `--ignored`) and confirm all four tests are skipped/pass trivially (compiles, `#[ignore]` respected).
- [ ] `cargo fmt`, `cargo clippy`, `code-ranker check .`.
- [ ] Commit.

---

### Task 7: docs/CONNECTORS.md updates

**Files:**
- Modify: `docs/CONNECTORS.md`

Steps:

- [ ] Append a new `## Official connectors` section after the existing `## The apb connector CLI` section:
  ```markdown
  ## Official connectors

  Four official connectors ship inside the `apb` binary and install with
  `apb connector install <name>`: `github`, `telegram`, `smtp`, `sentry`.
  Installing from the binary records trust for the connector's tree digest
  in the same action, since the bytes are already part of the binary you
  are running; `apb connector install --from-dir <path>` (the development
  loop for this repository, `connectors/<name>/`) keeps the normal approve
  flow.

  ### github

  Account fields: `api_base` (`https://api.github.com`, or your GHES API
  base) and `token` (secret). Prefer `token: "{{cmd:gh auth token}}"` when
  `gh auth login` has already run; otherwise `{{env.GITHUB_TOKEN}}` with a
  personal access token scoped `repo` (or `public_repo`) and `workflow`
  for `dispatch_workflow`. Healthcheck: `get_rate_limit`.

  ### telegram

  Account fields: `api_base` (`https://api.telegram.org`, overridable for
  a self-hosted Bot API server) and `token` (secret) - the token
  [@BotFather](https://t.me/BotFather) gives you for a new bot. The bot
  must already be a member of a chat before `send_message` reaches it.
  Healthcheck: `get_me`.

  ### smtp

  Account fields: `host`, `port`, `from_email` (all required), and
  `username`, `password` (secret), `from_name`, `use_tls` (all optional).
  Set `use_tls` explicitly (there is no engine-level default for account
  fields): `true` for STARTTLS on port 587, the common case. Healthcheck:
  `verify` (connects, negotiates STARTTLS, authenticates, sends nothing).

  ### sentry

  Account fields: `base_url` (`https://sentry.io`, or self-hosted),
  `org` (the organization slug), and `token` (secret). Create the token at
  Settings > Auth Tokens with scopes `project:read`, `event:read`,
  `issue:write` for issue functions and `project:releases` for
  `create_release`/`create_deploy`. `list_issues` paginates through the
  call result's `link` field: pass the cursor it returns back into the
  next call's `cursor` argument. Healthcheck: `list_projects`.

  ### Demo playbooks

  `examples/playbooks/sentry-triage.yaml` and
  `examples/playbooks/release-announce.yaml` exercise the four connectors
  end to end and double as reference examples for grant allowlists and
  `max_calls`. They validate in CI against fake accounts and are not run
  against real services there; run them manually once your own accounts
  are configured and approved.
  ```
- [ ] `cargo fmt`, `cargo clippy`, `code-ranker check .` (docs-only change; run to confirm nothing regressed).
- [ ] Commit.

---

## Authoring decisions recorded

- **response_pick selections:** github - on every `read_only` function only (9 of 21); list-wrapper keys (`workflow_runs.*`, `check_runs.*`) used where GitHub wraps lists in an object. telegram - on `get_me`, `get_chat`, `get_updates`, every path prefixed `result.*` (Telegram wraps responses in `{ok, result}`). smtp - none (forbidden on smtp functions). sentry - on the three read_only functions.
- **API shapes:** the spec's method/path tables were verified against real GitHub REST v3, Telegram Bot API, and Sentry `/api/0/` and copied exactly; body/query fields the tables left unlisted were derived from the real APIs.
- **Whole-args bodies and required query filters** follow from the renderer's no-optional-template rule (see the load-bearing findings section above).
- **Slice-4 seed handoff:** this slice removes `connectors/example/` and re-points the slice-4 CLI install/list/test integration tests at `github` (or a `--dir` fixture).
