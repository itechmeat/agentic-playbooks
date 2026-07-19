# Connectors Implementation Plan

> **Status: COMPLETED (2026-07-19).** All 19 tasks (plus a 17.5 follow-up for
> usage stats) are implemented, reviewed, and merged into the feature branch;
> the design spec is marked implemented. This document is kept as an archival
> record of the plan as executed - the unchecked step boxes below reflect the
> plan template, not pending work. Progress ledger:
> `.superpowers/sdd/progress.md`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connector infrastructure for apb: folder-based declarative HTTP connectors, layered account configs with env-referenced secrets, node-level grants, trust gating, `apb connector call` channel, dashboard UI, and a fake `mock-tracker` connector for tests.

**Architecture:** New `connector` domain module in apb-core (schema, templates, configs, secrets, store, digests - no async, no network). apb-engine snapshots connectors and grants into the write-once run manifest, scrubs agent env, generates the agent instruction, and executes calls (HTTP via ureq, mock, dry-run) with structured errors and events. apb-mcp extends the policy gate (permit maps for connector and account digests). apb-cli adds `apb connector` subcommands. apb-server plus web add connectors pages and a node-form binding editor.

**Tech Stack:** Rust workspace edition 2024 (serde, serde_yaml_ng, thiserror, sha2), ureq + jsonschema + percent-encoding (new deps), svelte 5 + shadcn-svelte + Tailwind v4, vitest.

**Spec:** `docs/superpowers/specs/2026-07-18-connectors-design.md` - read it before starting any task.

## Global Constraints

- Dependency direction core <- engine <- mcp, cli and server on top; no import cycles (code-ranker enforces).
- No em-dashes (U+2014), no exclamation marks in docs or user-facing strings. No CJK. Machine-facing fields are English.
- New `EventPayload` fields only with `#[serde(default)]`.
- State files via `apb_core::fsutil` (atomic write, 0600 for private files).
- Names are slugs `[a-z0-9][a-z0-9-]*` max 64, validated with `apb_core::profile::validate_profile_name`.
- Secret values are never returned, logged, cached, or written into manifests/events/CLI output. Only env variable NAMES appear there.
- Every commit: `git commit --signoff` (DCO bot blocks PRs without it) plus trailer `Co-Authored-By: <acting model> <noreply@anthropic.com>`.
- Execution mode: implementer and fix subagents NEVER run git commands (no add, commit, stash, checkout, rm). The controller performs every git operation after the task review approves. A task's Commit step is the controller's.
- Test layout is one integration binary per crate (docs/TESTING-GUIDELINES.md): a new integration test file goes to `crates/<c>/tests/suite/<name>.rs` plus one `#[path = "suite/<name>.rs"] mod <name>;` line in `crates/<c>/tests/main.rs`. NEVER create a new `tests/<name>.rs` at the top level. Env-mutating or env-reading tests take the ONE shared lock from `tests/suite/common` with Drop-guard restore; `include_str!` from `tests/suite/` needs `../fixtures/...`.
- Gate scoping per docs/BUILD-OPTIMIZATION.md: while iterating run `cargo test -p <crate>` (plus dependents for engine changes) and `cargo clippy -p <crate> --all-targets -- -D warnings`; the FULL `cargo test --workspace` + workspace clippy run at task-closing commits of core/engine tasks and at phase boundaries. One cargo invocation at a time, never in the background. No redundant `cargo build`.
- Before the final task: `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
- Unit tests inside apb-core that touch `APB_CONFIG_DIR` or `HOME` take `apb_core::env_test_lock()` and restore env with a Drop guard (see `trust.rs` tests).
- SOLID, KISS, DRY. No meaningless fallbacks that mask errors; fail with a clear message instead. No hardcoded values where a constant or config exists.
- Commit only within the feature branch. Never push without the owner's explicit approval.

---

### Task 1: Branch and docs commit

**Files:**
- Commit: `docs/superpowers/specs/2026-07-18-connectors-design.md` (already on disk, uncommitted)
- Commit: `docs/superpowers/plans/2026-07-18-connectors.md` (this file)

**Steps** (controller-owned: branch creation, pulling, staging, and committing
are git operations, which per Global Constraints belong to the controller,
never to implementer subagents):

- [ ] **Step 1: Create the story branch from main**

```bash
git checkout main && git pull --ff-only && git checkout -b feat/connectors
```

- [ ] **Step 2: Commit the spec and plan**

```bash
git add docs/superpowers/specs/2026-07-18-connectors-design.md docs/superpowers/plans/2026-07-18-connectors.md
git commit --signoff -m "docs: connectors design spec and implementation plan"
```

---

### Task 2: Core connector definition schema

**Files:**
- Create: `crates/apb-core/src/connector/mod.rs`
- Create: `crates/apb-core/src/connector/def.rs`
- Modify: `crates/apb-core/src/lib.rs` (add `pub mod connector;` alphabetically)

**Interfaces:**
- Consumes: `apb_core::profile::validate_profile_name`.
- Produces (later tasks rely on these exact names):

```rust
pub enum ConnectorError {          // thiserror, in def.rs, re-exported from mod.rs
    Invalid(String), NotFound(String), Io(std::io::Error), Yaml(String),
}
pub enum AuthSpec {                // serde tag "kind", snake_case
    Header { header: String, value_template: String },
    Query { param: String, value_template: String },
    Basic { username_template: String, password_template: String },
}
pub struct AccountField { pub name: String, pub required: bool, pub secret: bool } // required/secret default false
pub struct MockSpec { pub status: u16, pub body: serde_json::Value }
pub struct FunctionSpec {
    pub name: String, pub description: String,
    pub read_only: bool,                    // default false
    pub deprecated: Option<String>,
    pub method: Option<String>, pub url: Option<String>,
    pub query: std::collections::BTreeMap<String, String>, // default empty
    pub body: Option<serde_json::Value>,    // YAML value; string leaves may hold placeholders
    pub args_schema: Option<serde_json::Value>,
    pub timeout_sec: u64,                   // default 30 via fn default_timeout
    pub mock: Option<MockSpec>,
}
pub struct ConnectorDoc {
    pub name: String, pub version: String,
    pub healthcheck: Option<String>,
    pub auth: Option<AuthSpec>,
    pub account_fields: Vec<AccountField>,
    pub functions: Vec<FunctionSpec>,
}
impl ConnectorDoc {
    pub fn from_yaml(yaml: &str, expected_name: &str) -> Result<Self, ConnectorError>;
    pub fn function(&self, name: &str) -> Option<&FunctionSpec>;
    pub fn read_only_functions(&self) -> Vec<String>;
    pub fn secret_fields(&self) -> Vec<String>;   // names of account_fields with secret: true
}
impl FunctionSpec { pub fn is_mock(&self) -> bool; }
```

All structs `#[serde(deny_unknown_fields)]`, mirroring `ProfileDoc` style.

`from_yaml` validation rules (each failure is `ConnectorError::Invalid` with a message naming the offender):
1. `name` passes `validate_profile_name` and equals `expected_name`.
2. `version` is non-empty.
3. Function names pass `validate_snake_name` (new fn in def.rs: `[a-z0-9][a-z0-9_]*`, max 64 - snake_case for machine-facing identifiers, matching the spec examples `list_issues`, `base_url`) and are unique.
4. Each function is HTTP xor mock: (`method` and `url` both present, `mock` absent) or (`mock` present, `method`/`url`/`query`/`body` absent).
5. `healthcheck`, when present, names an existing function.
6. Account field names pass `validate_snake_name` and are unique.

Do NOT validate secret placement here - that needs the template parser (Task 3 adds `def.rs::validate_templates`).

- [ ] **Step 1: Write failing tests** in `def.rs` `#[cfg(test)] mod tests` covering: a full valid document parses (use the jira example from spec section 3.1, with `mock` ping); name mismatch rejected; unknown field rejected; function with both `url` and `mock` rejected; duplicate function name rejected; `healthcheck: missing` rejected; defaults (`read_only` false, `timeout_sec` 30, empty `query`).

```rust
#[test]
fn parses_valid_doc_with_defaults() {
    let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
    assert_eq!(doc.functions.len(), 3);
    let f = doc.function("list_issues").unwrap();
    assert!(f.read_only);
    assert_eq!(f.timeout_sec, 30);
    assert!(doc.function("ping").unwrap().is_mock());
    assert_eq!(doc.read_only_functions(), vec!["list_issues".to_string()]);
}
#[test]
fn rejects_function_that_is_both_http_and_mock() {
    let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    mock: { status: 200, body: {} }\n";
    assert!(ConnectorDoc::from_yaml(y, "x").is_err());
}
```

- [ ] **Step 2: Run and verify failure**: `cargo test -p apb-core connector::def` - FAIL (module does not exist / compile error).
- [ ] **Step 3: Implement** `def.rs` per the interface above; `mod.rs` starts as `pub mod def; pub use def::*;`.
- [ ] **Step 4: Run and verify pass**: `cargo test -p apb-core connector::def` - PASS. Then `cargo fmt --all` and `cargo clippy -p apb-core --all-targets -- -D warnings`.
- [ ] **Step 5: Commit**: `git add -A && git commit --signoff -m "feat(core): connector definition schema and parsing"`

---

### Task 3: Core template renderer

**Files:**
- Create: `crates/apb-core/src/connector/template.rs`
- Modify: `crates/apb-core/src/connector/def.rs` (add `validate_templates`)
- Modify: `crates/apb-core/src/connector/mod.rs` (`pub mod template;`)
- Modify: `crates/apb-core/Cargo.toml` (+ `percent-encoding = "2"`; add to `[workspace.dependencies]` and reference with `.workspace = true`)

**Interfaces:**

```rust
pub enum Namespace { Account, Args, Secret }        // template.rs
pub struct RenderCtx<'a> {
    pub account: &'a std::collections::BTreeMap<String, String>, // non-secret fields
    pub args: &'a serde_json::Value,                             // validated args object
    pub secrets: &'a std::collections::BTreeMap<String, String>, // resolved secret fields
}
/// Placeholders `{{ns.name}}` found in a template string. `{{args}}` bare form
/// is returned as (Args, ""). Unknown namespace or malformed braces -> Err.
pub fn placeholders(template: &str) -> Result<Vec<(Namespace, String)>, ConnectorError>;
/// Renders with percent-encoding of substituted values (URL path/query context).
pub fn render_encoded(template: &str, ctx: &RenderCtx) -> Result<String, ConnectorError>;
/// Renders raw (auth header values, body string leaves).
pub fn render_raw(template: &str, ctx: &RenderCtx) -> Result<String, ConnectorError>;
/// Renders a body value: `"{{args}}"` string -> ctx.args clone; otherwise walk
/// the JSON value and render_raw every string leaf.
pub fn render_body(body: &serde_json::Value, ctx: &RenderCtx) -> Result<serde_json::Value, ConnectorError>;
```

Rules: placeholder syntax is exactly `{{ns.name}}` (no spaces, no nesting, no filters); an unresolved name is an error naming it; percent-encode with `percent_encoding::utf8_percent_encode(value, NON_ALPHANUMERIC)` minus `-._~` (define an `AsciiSet` constant `URL_VALUE`); args lookup only supports top-level keys of the args object and renders scalars (string as-is, number/bool via `to_string`), a non-scalar arg in a string template is an error.

`def.rs::validate_templates(doc: &ConnectorDoc) -> Result<(), ConnectorError>` (called at the end of `from_yaml`): collect placeholders from every function `url`, `query` values, and `body` string leaves - `Namespace::Secret` there is an error ("secret placeholders are allowed only in auth"); auth templates may use Secret and Account only (Args in auth is an error); every `{{secret.x}}` / `{{account.x}}` must name a declared account field (secret ones for Secret, non-secret for Account).

- [ ] **Step 1: Write failing tests**: encode vs raw (`render_encoded` turns `a b/c` into `a%20b%2Fc`, `render_raw` leaves it); `{{args}}` body passthrough; string-leaf body rendering; unresolved placeholder error; secret in url rejected by `from_yaml`; args in auth rejected; secret in auth accepted.

```rust
#[test]
fn encodes_url_substitutions() {
    let account = std::collections::BTreeMap::new();
    let args = serde_json::json!({"jql": "project = APB"});
    let secrets = std::collections::BTreeMap::new();
    let ctx = RenderCtx { account: &account, args: &args, secrets: &secrets };
    assert_eq!(render_encoded("q={{args.jql}}", &ctx).unwrap(), "q=project%20%3D%20APB");
}
```

- [ ] **Step 2: Verify failure**: `cargo test -p apb-core connector::template` - FAIL.
- [ ] **Step 3: Implement** parser (single scan for `{{`..`}}`), the three render fns, `validate_templates`, wire into `from_yaml`.
- [ ] **Step 4: Verify pass** + fmt + clippy as in Task 2.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): connector template renderer with secret placement policy"`

---

### Task 4: Core account config, merge, digest

**Files:**
- Create: `crates/apb-core/src/connector/config.rs`
- Modify: `crates/apb-core/src/connector/mod.rs`

**Interfaces:**

```rust
pub struct Account {
    pub name: String,
    pub default: bool,                                   // serde default false
    pub fields: std::collections::BTreeMap<String, String>, // #[serde(flatten)]
}
pub struct AccountsFile { pub accounts: Vec<Account> }   // deny_unknown_fields on Account is impossible with flatten; validate fields against account_fields instead
/// Paths: global `<config_dir>/connector-config/<name>.yaml`,
/// project `<root>/.apb/connector-config/<name>.yaml`.
pub fn global_config_path(name: &str) -> Option<std::path::PathBuf>;   // via config::config_dir()
pub fn project_config_path(root: &Path, name: &str) -> std::path::PathBuf;
/// Global then project; a project account with the same name replaces the
/// global one entirely; order: global-only accounts first (file order), then
/// project accounts (file order). Missing files are fine (empty).
pub fn load_merged(root: &Path, name: &str) -> Result<Vec<Account>, ConnectorError>;
/// Validation against the doc: unknown field names, missing required fields,
/// literal value in a secret field (must be exactly one `{{env.VAR}}`),
/// invalid account name slug, duplicate names within one file, more than one
/// `default: true` in the merged list. Returns human-readable errors.
pub fn validate_accounts(doc: &ConnectorDoc, accounts: &[Account]) -> Vec<String>;
/// Canonical digest of the non-secret identity of an account:
/// sha256 over domain tag "apb-account-v1\0", then lp(name), lp(default as "0"/"1"),
/// then for every field sorted by key: lp(key), lp(value). Reuses content::hex_lower.
/// Secret fields participate with their RAW config value (the `{{env.VAR}}` ref,
/// not the resolved secret) - renaming the env var is a change the user approves.
pub fn account_digest(account: &Account) -> String;
/// The env var names an account's secret fields reference, keyed by field name.
pub fn env_refs(doc: &ConnectorDoc, account: &Account) -> std::collections::BTreeMap<String, String>;
pub fn default_account<'a>(accounts: &'a [Account]) -> Option<&'a Account>;
```

`lp` length-prefix helper: copy the two-line pattern from `content.rs::lp` locally (it is private there; do not make it public just for this).

- [ ] **Step 1: Write failing tests**: merge replace-by-name and additivity (global `[a, b]`, project `[b', c]` gives `[a, b', c]` where `b'` is the project one); digest stability under field insertion order; digest changes when `default` flips; `validate_accounts` catches literal secret, missing required field, unknown field, double default; `env_refs` extracts `VAR` from `{{env.VAR}}`; project default wins (merged list has one default).

```rust
#[test]
fn project_account_replaces_global_same_name() {
    let root = tempfile::tempdir().unwrap();
    // write global + project YAML under APB_CONFIG_DIR/root, then:
    let merged = load_merged(root.path(), "jira").unwrap();
    assert_eq!(merged.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    assert_eq!(merged[1].fields["base_url"], "https://project.example");
}
```

- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy** (same commands as Task 2, filter `connector::config`).
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): connector account config with scope merge and canonical digest"`

---

### Task 5: Core dotenv and secret resolution

**Files:**
- Create: `crates/apb-core/src/connector/secrets.rs`
- Modify: `crates/apb-core/src/connector/mod.rs`

**Interfaces:**

```rust
/// "{{env.VAR}}" -> Some("VAR"); anything else -> None. VAR must match [A-Z0-9_]+.
pub fn parse_env_ref(value: &str) -> Option<String>;
/// KEY=value lines; '#' comments and blank lines skipped; no quoting logic
/// (value is everything after the first '='), CRLF tolerated.
pub fn parse_dotenv(content: &str) -> std::collections::BTreeMap<String, String>;
pub fn project_secrets_path(root: &Path) -> std::path::PathBuf;   // <root>/.apb/secrets.env
pub fn global_secrets_path() -> Option<std::path::PathBuf>;       // <config_dir>/secrets.env
/// Resolution order: process env, project dotenv, global dotenv.
pub fn resolve_var(root: &Path, var: &str) -> Option<String>;
pub fn missing_vars(root: &Path, vars: &[String]) -> Vec<String>;
/// Ensures `.apb/secrets.env` is listed in the project `.gitignore` (appends
/// a line if the file exists but lacks it; creates .gitignore when absent).
pub fn ensure_gitignored(root: &Path) -> std::io::Result<()>;
/// True when the project secrets file exists but .gitignore does not cover it.
pub fn gitignore_gap(root: &Path) -> bool;
```

- [ ] **Step 1: Write failing tests**: dotenv parsing (comments, first `=` split, CRLF); resolve order (process env beats project file beats global file - use `env_test_lock` and a uniquely named var); `missing_vars`; `ensure_gitignored` idempotence; `gitignore_gap` cases.
- [ ] **Step 2: Verify failure**, **Step 3: Implement** (write files in tests with plain `std::fs`; production creation of secrets.env itself is Task 12/CLI territory, this module only reads plus the gitignore helpers).
- [ ] **Step 4: Verify pass + fmt + clippy**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): dotenv secret resolution chain and gitignore guard"`

---

### Task 6: Core connector store (folders, digest, PUBLIC.md)

**Files:**
- Create: `crates/apb-core/src/connector/store.rs`
- Modify: `crates/apb-core/src/connector/mod.rs`

**Interfaces:**

```rust
pub struct PublicMeta {           // PUBLIC.md frontmatter; all serde-default
    pub display_name: String, pub summary: String,
    pub tags: Vec<String>, pub publisher: String, pub homepage: String, pub icon: String,
}
pub struct ConnectorSummary { pub name: String, pub version: String, pub meta: PublicMeta }
pub struct LoadedConnector {
    pub name: String, pub dir: std::path::PathBuf,
    pub yaml: String,             // raw connector.yaml content (snapshotted by the engine)
    pub doc: ConnectorDoc, pub digest: String, // tree digest of the whole folder
}
pub fn connectors_dir() -> Option<std::path::PathBuf>;   // config::config_dir()?.join("connectors")
pub fn list() -> Vec<ConnectorSummary>;                  // sorted by name; unparsable entries skipped
pub fn load(name: &str) -> Result<LoadedConnector, ConnectorError>;
pub fn public_meta(dir: &Path) -> PublicMeta;            // frontmatter between leading "---" lines; missing file/frontmatter -> default with display_name = folder name
pub fn public_body(dir: &Path) -> String;                // markdown after frontmatter, "" when absent
```

`load`: validate name slug first (`validate_profile_name`), canonicalize and containment-check against `connectors_dir` exactly like `skills::resolve_skill` does, read `connector.yaml`, `ConnectorDoc::from_yaml`, digest with `content::tree_digest(&dir, &TreeLimits::default())`.

- [ ] **Step 1: Write failing tests** (set `APB_CONFIG_DIR` to a tempdir under `env_test_lock`): load a valid folder and get a stable digest; digest changes when PUBLIC.md changes; bad name rejected; `list` skips a folder with broken YAML and parses frontmatter.
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): connector store with tree digest and PUBLIC.md metadata"`

---

### Task 7: Trust record kinds for connectors and accounts

**Files:**
- Modify: `crates/apb-core/src/trust.rs` (extend `Kind`)
- Test: same file, `mod tests`

**Interfaces:**

```rust
pub enum Kind { Playbook, ProfileBundle, Connector, ConnectorAccount } // two new variants
/// Key helper for account approvals (id field of the record):
/// format "connector/account", e.g. "jira/project-board".
pub fn account_trust_id(connector: &str, account: &str) -> String;
```

Approval keying stays digest-based (`approved: BTreeMap<digest, record>`), so no store format change; the `Kind` only labels records. Existing `approve_kind` and `is_approved` are reused as-is by later tasks.

- [ ] **Step 1: Write failing test**: approve a digest with `Kind::Connector` and another with `Kind::ConnectorAccount` (id from `account_trust_id`), reload, both `is_approved`; serialization roundtrip keeps the kind (serde snake_case: `connector`, `connector_account`).
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy**: `cargo test -p apb-core trust`.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): connector and account trust record kinds"`

---

### Task 8: Playbook schema binding and effects

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (ConnectorBinding + field on AgentTask)
- Modify: `crates/apb-core/src/effects.rs` (connectors imply effects)
- Modify: `crates/apb-core/src/validate.rs` (static checks)

**Interfaces:**

```rust
// schema.rs
pub enum FunctionsAllow { All, ReadOnly, List(Vec<String>) }
// serde: absent field -> All; string "read_only" -> ReadOnly; sequence -> List.
// Custom Deserialize like SkillRef's RefForm; serialize All as skipped field.
pub struct ConnectorBinding {
    pub name: String,
    pub accounts: Option<Vec<String>>,       // None = all
    pub functions: FunctionsAllow,
    pub max_calls: Option<u32>,
}
// Deserialize: bare string "jira" -> ConnectorBinding { name, accounts: None, functions: All, max_calls: None };
// object form with deny_unknown_fields.
// NodeKind::AgentTask gains: #[serde(default)] pub connectors: Vec<ConnectorBinding>
// Node accessor used by engine/mcp:
impl NodeKind { pub fn connector_bindings(&self) -> &[ConnectorBinding]; } // empty slice for other kinds
```

`effects.rs::effective`: when any node has a non-empty `connectors`, insert `Effect::Network` and `Effect::External` into the set (this is spec section 12's automatic catalog flag; it also feeds `preflight` for free).

`validate.rs` static checks, continuing after V18 (do not renumber existing codes; check the actual last code in the file before assigning):
- `V19` error: connector binding name is not a valid slug.
- `V20` error: duplicate connector name within one node's `connectors`.
- `V21` error: `accounts` or `functions` list entry is empty or duplicated.
- `V22` error: `max_calls` is 0.

(FS-dependent checks - connector installed, account/function exists - are Task 9.)

- [ ] **Step 1: Write failing tests** in `schema.rs` and `validate.rs` mods: YAML with shorthand string and full object parses into the expected bindings; `functions: read_only` parses to `ReadOnly`; unknown key in binding rejected; effects union gains network+external for a playbook with a bound connector; each V code fires on its minimal bad playbook and stays silent on a good one.

```yaml
# parsing fixture (inline str in test)
schema: 2
name: t
nodes:
  - { id: a, type: agent_task, prompt: hi, profile: x, connectors: [jira] }
  - id: b
    type: agent_task
    prompt: hi
    profile: x
    connectors:
      - { name: telegram, accounts: [team-bot], functions: [send_message], max_calls: 50 }
      - { name: github, functions: read_only }
```

- [ ] **Step 2: Verify failure**: `cargo test -p apb-core schema connector` and `cargo test -p apb-core validate`.
- [ ] **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy + `cargo test --workspace`** (schema change touches every crate).
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): node connector bindings, implied effects, static validation"`

---

### Task 9: Context-ful binding validation and grant resolution

**Files:**
- Create: `crates/apb-core/src/connector/resolve.rs`
- Modify: `crates/apb-core/src/connector/mod.rs`

**Interfaces:**

```rust
/// A node's fully resolved grant, ready for the manifest.
pub struct ResolvedGrant {
    pub connector: String,
    pub accounts: Vec<Account>,          // full non-secret account objects (merged config)
    pub functions: Vec<String>,          // explicit, read_only shorthand already expanded
    pub max_calls: Option<u32>,
}
/// Everything the gate and the engine need about one connector, resolved once.
pub struct ResolvedConnector {
    pub loaded: LoadedConnector,
    pub accounts: Vec<Account>,          // merged, validated
    pub required_env: Vec<String>,       // union of env refs across merged accounts, sorted
}
/// Resolves every connector a playbook binds. Errors are strings suitable both
/// for validator output and policy refusal details:
/// - connector not installed / fails to load
/// - account allowlist entry not in the merged config
/// - function allowlist entry not in the manifest
/// - `functions: read_only` with zero read-only functions
/// - config-level errors from validate_accounts
/// Warnings (non-fatal): allowlisted function is deprecated; gitignore gap.
pub struct ResolutionOutput {
    pub connectors: std::collections::BTreeMap<String, ResolvedConnector>,
    pub grants: std::collections::BTreeMap<String, Vec<ResolvedGrant>>, // node_id -> grants
    pub warnings: Vec<String>,
}
pub fn resolve_playbook(root: &Path, playbook: &Playbook) -> Result<ResolutionOutput, Vec<String>>;
/// Env names referenced by EVERY installed connector's merged configs (both
/// scopes) - the adapter scrub list (spec 4.3). Best-effort: unparsable
/// connectors or configs are skipped.
pub fn all_referenced_env_names(root: &Path) -> Vec<String>;
```

Grant expansion rules: `accounts: None` -> all merged accounts; `functions: All` -> all function names; `ReadOnly` -> `doc.read_only_functions()`; `List` -> verify each exists.

- [ ] **Step 1: Write failing tests** (tempdir config dir + project root, a small `mock-tracker`-like fixture written inline by a helper fn `write_fixture_connector(cfg: &Path)` - reuse it in later engine tests via copy, keep it in this file's test mod): happy path resolves grants with shorthand expansion; unknown account errors; unknown function errors; read_only with none errors; deprecated warning; `all_referenced_env_names` unions across connectors.
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(core): playbook connector resolution and grant expansion"`

---

### Task 10: Engine manifest snapshot of connectors and grants

**Files:**
- Modify: `crates/apb-engine/src/manifest.rs`
- Test: `crates/apb-engine/tests/suite/connector_manifest.rs` (+ `mod` line in `crates/apb-engine/tests/main.rs`)

**Interfaces:**

```rust
// manifest.rs additions (all #[serde(default)] so old manifests still read):
pub struct ManifestAccount {
    pub name: String, pub default: bool,
    pub fields: std::collections::BTreeMap<String, String>, // non-secret fields; secret fields hold the RAW env ref string
    pub env: std::collections::BTreeMap<String, String>,    // secret field -> ENV NAME
    pub digest: String,                                     // account_digest
}
pub struct ManifestConnectorGrant {
    pub connector: String,
    pub accounts: Vec<String>,     // account names granted to this node
    pub functions: Vec<String>,
    pub max_calls: Option<u32>,
}
pub struct ManifestConnector {
    pub name: String, pub digest: String,
    pub accounts: Vec<ManifestAccount>,
}
// RunExecutionManifest gains:
#[serde(default)] pub connectors: Vec<ManifestConnector>,
#[serde(default)] pub connector_grants: BTreeMap<String, Vec<ManifestConnectorGrant>>, // node_id -> grants
impl RunExecutionManifest {
    pub fn connector(&self, name: &str) -> Option<&ManifestConnector>;
    pub fn grants_for(&self, node_id: &str) -> &[ManifestConnectorGrant];
}
```

Also adjust `is_empty()` to `self.profiles.is_empty() && self.connectors.is_empty()` so a connector-only manifest still gets written.

- [ ] **Step 1: Write failing test**: build a manifest with one connector, write, read back, assert roundtrip and that a manifest YAML WITHOUT the new keys (hand-written string from before this change) still parses with empty defaults.
- [ ] **Step 2: Verify failure**: `cargo test -p apb-engine --test main connector_manifest::`.
- [ ] **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(engine): connector snapshot structures in the run manifest"`

---

### Task 11: Engine run-start wiring (permit verification, snapshot, grants)

**Files:**
- Create: `crates/apb-engine/src/connector_run.rs`
- Modify: `crates/apb-engine/src/lib.rs` (export), `crates/apb-engine/src/run_config.rs` (expected maps), and the run-start path that builds the manifest (locate with `codegraph_search "build_run_manifest"` - extend where profiles are snapshotted)
- Test: `crates/apb-engine/tests/suite/connector_run.rs` (+ `mod` line in `tests/main.rs`)

**Interfaces:**

```rust
// run_config.rs: alongside expected profile bundles (follow the existing field style):
#[serde(default)] pub expected_connectors: std::collections::BTreeMap<String, String>,        // name -> tree digest
#[serde(default)] pub expected_connector_accounts: std::collections::BTreeMap<String, String>, // "connector/account" -> account digest

// connector_run.rs:
/// Re-resolves connectors at start, verifies BOTH expected maps verbatim
/// (any drift -> EngineError::Invalid naming the drifted item), verifies all
/// required env vars resolve, copies each used connector.yaml to
/// runs/<id>/connectors/<name>.yaml, and returns the manifest pieces.
pub fn snapshot_connectors(
    root: &Path, run_dir: &Path, playbook: &Playbook,
    expected_connectors: &BTreeMap<String, String>,
    expected_accounts: &BTreeMap<String, String>,
) -> Result<(Vec<ManifestConnector>, BTreeMap<String, Vec<ManifestConnectorGrant>>), EngineError>;
```

Wire into the manifest build: when the playbook binds connectors, call `snapshot_connectors` and set the two new manifest fields. When `expected_connectors` is empty but the playbook binds connectors, refuse (`EngineError::Invalid("connector bindings present but no connector permit")`) - the CLI/MCP callers must always pass the gate result (mirrors the profile-bundle invariant).

- [ ] **Step 1: Write failing integration test** (pattern-match the existing sub-playbook run module in `tests/suite/` for run-dir setup): with a fixture connector installed under a temp `APB_CONFIG_DIR` and a playbook binding it, `snapshot_connectors` writes `runs/<id>/connectors/mock-tracker.yaml`, returns grants with the read_only shorthand expanded; a tampered expected digest fails with a message naming `mock-tracker`; a missing env var fails naming the var and account.
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy + `cargo test -p apb-engine`**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(engine): connector permit verification and run snapshot"`

---

### Task 12: Adapter env scrubbing, run-context env, agent instruction

**Files:**
- Modify: `crates/apb-engine/src/adapter.rs` (every `Command::new(&self.program)` spawn site for agent processes: lines around 296, 392, 568 - apply to all three)
- Create: `crates/apb-engine/src/connector_prompt.rs`
- Test: adapter env in `crates/apb-engine/tests/suite/connector_run.rs` (extend), prompt in `connector_prompt.rs` unit tests

**Interfaces:**

```rust
// adapter.rs: the adapter gains (plumb through the existing invocation/spawn config):
pub struct ConnectorEnvPolicy {
    pub scrub: Vec<String>,        // env names to remove (core::connector::resolve::all_referenced_env_names)
    pub run_dir: Option<std::path::PathBuf>, // sets APB_RUN_DIR
    pub node_id: Option<String>,             // sets APB_NODE_ID
}
// At each spawn site: for name in &policy.scrub { cmd.env_remove(name); }
// plus cmd.env("APB_RUN_DIR", ...) / cmd.env("APB_NODE_ID", ...) when set.

// connector_prompt.rs:
/// The instruction block appended to a node prompt when the node has grants.
/// Content: for each grant - connector name, granted account names with their
/// non-secret fields (base_url etc.), function list with description, args
/// schema (compact JSON), deprecated marks; then the exact call syntax line:
///   apb connector call <connector> <function> [--account <name>] --args '<json>'
/// and a note that --args - reads stdin and --dry-run previews the request.
pub fn instruction_block(grants: &[ManifestConnectorGrant], connectors: &[ManifestConnector]) -> String;
```

Find where the node prompt is assembled (skills/SOUL delivery in scheduler or adapter; `codegraph_search "prompt"` in apb-engine) and append `instruction_block` when `manifest.grants_for(node_id)` is non-empty.

- [ ] **Step 1: Write failing tests**: `instruction_block` lists granted functions only (not ungranted ones), includes account names and never any `{{env.*}}` value or resolved secret, marks deprecated functions; adapter test: spawn `/usr/bin/env` as the fake agent program with a scrub list and assert the captured output lacks the scrubbed var and has `APB_RUN_DIR`/`APB_NODE_ID` (follow the existing adapter tests' fake-program pattern in `adapter.rs` tests or `tests/`).
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(engine): agent env scrubbing, run context env, connector instruction block"`

---

### Task 13: Engine call executor

**Files:**
- Create: `crates/apb-engine/src/connector_call.rs`
- Modify: `crates/apb-engine/src/lib.rs`, `crates/apb-engine/src/event.rs` (new payload variant), `crates/apb-engine/Cargo.toml` (+ `ureq = "2"` with default features minus tls if rustls preferred: use `ureq = { version = "2", features = ["json"] }`; + `jsonschema = { version = "0.26", default-features = false }`)
- Test: `crates/apb-engine/tests/suite/connector_call.rs` (+ `mod` line in `tests/main.rs`)

**Interfaces:**

```rust
// event.rs: new variant (all fields serde(default) where optional):
ConnectorCall {
    node_id: String, connector: String, function: String, account: String,
    url: String,            // pre-auth rendered URL ("" for mock)
    outcome: String,        // "ok" | error code
    #[serde(default)] http_status: Option<u16>,
    duration_ms: u64,
},

// connector_call.rs:
pub struct CallRequest<'a> {
    pub run_dir: &'a Path, pub root: &'a Path,
    pub node_id: &'a str, pub connector: &'a str, pub function: &'a str,
    pub account: Option<&'a str>, pub args: serde_json::Value,
    pub dry_run: bool,
}
pub enum CallErrorCode { Config, Permission, InvalidArgs, Auth, NotFound, RateLimited, Service, Network, Timeout }
pub struct CallError { pub code: CallErrorCode, pub message: String, pub http_status: Option<u16>, pub retry_after_sec: Option<u64> }
pub struct CallOk { pub status: u16, pub body: serde_json::Value, pub truncated: bool }
/// The whole call pipeline of spec section 6 step 4 plus 6.1/6.2.
/// Returns the JSON document to print (both ok and error shapes), plus the
/// exit-code hint. Appends the ConnectorCall event itself (events dir under run_dir).
pub fn execute(req: CallRequest) -> (serde_json::Value, bool /* ok */);
```

Pipeline inside `execute` (each numbered check exits early with the matching code):
1. read manifest (`manifest::read`); missing -> `Config`.
2. grant lookup: `grants_for(node_id)` must contain connector+function; account selection: explicit must be granted; None -> single granted account, else the manifest account with `default: true` among granted, else `Config` listing choices. Violations -> `Permission`.
3. `max_calls`: count prior `ConnectorCall` events for this node (read the event log; locate the reader API in `event.rs`); at limit -> `Permission` naming the budget.
4. load snapshot `runs/<id>/connectors/<name>.yaml` -> `ConnectorDoc::from_yaml`; digest mismatch with the manifest connector digest -> `Config` ("snapshot drift").
5. args: validate against `args_schema` when present via `jsonschema::validate` -> `InvalidArgs` with the first error path.
6. mock function: return `MockSpec` status/body (status >= 400 maps through the same status->code table below).
7. secrets: `resolve_var` per env ref; missing -> `Config` naming var. Skipped entirely for `dry_run`.
8. render URL (`render_encoded`), query pairs (encoded), body (`render_body`); enforce spec 6.1: parse with `url::Url`? NO new dep - hand checks: scheme prefix `http://`/`https://` case-insensitive, reject `@` before the first `/` after scheme (userinfo), build final URL with query appended. `dry_run` stops here printing `{ok:true, dry_run:true, method, url, body}`.
9. ureq agent: `ureq::AgentBuilder::new().redirects(0).timeout(Duration::from_secs(timeout_sec)).build()`; inject auth per `AuthSpec` (header / query param appended post-log / basic via base64 - use the existing base64 dep if present, else implement RFC 4648 encode locally in ~10 lines); send; map transport errors -> `Network` (or `Timeout` on `ureq::Error::Transport` with timeout kind - check `e.to_string().contains("timed out")`, acceptable).
10. read body up to 1 MiB cap (`take(1024*1024 + 1)`), set `truncated`; parse JSON when content-type contains `application/json`.
11. status map: 2xx ok; 3xx -> `Service` ("redirects are not followed"); 401/403 -> `Auth`; 404 -> `NotFound`; 429 -> `RateLimited` + `Retry-After` header parse; other 4xx -> `Service`; 5xx -> `Service`.
12. redaction: TODO(redaction-layer): interim literal scrub per spec 6.2 - for every resolved secret value, replace occurrences in the outgoing body JSON string and in the printed result with `[redacted:<ENV_NAME>]`. Mark the code comment exactly `// TODO(redaction-layer): interim literal redaction, replaced by the dedicated LLM-output redaction story.`
13. append `ConnectorCall` event (pre-auth URL, outcome, duration); never log bodies at full length - truncate the event's stored body fields to 2 KiB if you record them at all (recording bodies is optional; outcome metadata is required).

- [ ] **Step 1: Write failing tests** against a local ephemeral HTTP server: use `std::net::TcpListener` bound to `127.0.0.1:0` and a thread answering canned HTTP responses (no new dev-dep; ~40 lines helper in the test file). Cases: ok JSON roundtrip with auth header injected; 401 -> `auth`; 429 with `Retry-After: 30` -> `rate_limited` + `retry_after_sec: 30`; 302 -> `service` with redirect message; oversized body -> `truncated: true`; mock function needs no server; dry-run resolves without secrets set; unknown function -> `permission`; wrong account -> `permission`; `max_calls: 1` second call -> `permission`; secret value echoed by the server body comes back as `[redacted:VAR]`.
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy + `cargo test -p apb-engine`**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(engine): connector call executor with hardening, redaction, events"`

---

### Task 14: CLI `apb connector` subcommands

**Files:**
- Create: `crates/apb-cli/src/connector.rs`
- Modify: `crates/apb-cli/src/main.rs` (new `Connector { #[command(subcommand)] action: ConnectorAction }` variant + dispatch arm, style-matched to `Profile`)
- Test: `crates/apb-cli/tests/suite/connector_cli.rs` (+ `mod` line in `crates/apb-cli/tests/main.rs`; follow the existing cli suite modules' binary-invocation pattern via `env!("CARGO_BIN_EXE_apb")` with child-process env, which needs no lock)

**Interfaces:**

```rust
pub enum ConnectorAction {   // clap Subcommand
    List,
    Show { name: String },
    Call {
        name: String, function: String,
        #[arg(long)] account: Option<String>,
        #[arg(long)] args: Option<String>,     // JSON or "-" for stdin; default "{}"
        #[arg(long)] dry_run: bool,
    },
    Doctor,
    Env { name: Option<String> },
    Init { name: String },
}
pub fn connector_cmd(root: &Path, action: ConnectorAction) -> ExitCode;
```

Behavior:
- `List`: table of name, version, trust (approved / changed / unapproved via `TrustStore` + `store::load` digest), accounts configured count.
- `Show`: doc summary - functions (name, read_only/deprecated marks, description), account fields, merged accounts with per-var fill status (names only).
- `Call`: run context from env `APB_RUN_DIR` + `APB_NODE_ID`; both unset -> a debug mode is still useful, but grants live in manifests, so REQUIRE them and exit with `config` error JSON explaining the vars (users get debugging via `--dry-run` inside a run or the doctor healthcheck path). Print `execute()` JSON; exit 0 on ok else 1.
- `Doctor`: per installed connector run the checks from spec section 10 (parse, digest, config validation via `resolve` pieces, `missing_vars`, trust states, healthcheck execution when declared and env resolves - healthcheck runs OUTSIDE a run: build a synthetic single-account call using the live (not snapshotted) definition, clearly labeled).
- `Env`: print `KEY=` lines for unresolved vars (optionally filtered by connector name).
- `Init`: scaffold `<connectors_dir>/<name>/` with `connector.yaml` (one HTTP function `get_item`, one mock `ping`, `healthcheck: ping`, one account field pair `base_url` + secret `token`) and `PUBLIC.md`; refuse when the folder exists.

- [ ] **Step 1: Write failing test**: `init` then `doctor` on the scaffold (with a temp `APB_CONFIG_DIR` and the token var set) reports no errors; `call` without run context prints a `config` error JSON and exits non-zero; `env` lists the scaffold token var when unset.
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(cli): apb connector subcommands (list, show, call, doctor, env, init)"`

---

### Task 15: MCP policy gate and connectors_list tool

**Files:**
- Modify: `crates/apb-mcp/src/policy.rs` (RunPermit + check), `crates/apb-mcp/src/server.rs` (tool registration; follow an existing read-only tool like `profile_list` for shape)
- Test: `crates/apb-mcp/tests/suite/connector_policy.rs` (+ `mod` line in `crates/apb-mcp/tests/main.rs`)

**Interfaces:**

```rust
// RunPermit gains:
pub connectors: std::collections::BTreeMap<String, String>,          // name -> tree digest
pub connector_accounts: std::collections::BTreeMap<String, String>,  // "connector/account" -> digest
// policy.rs new fn, called from check_run after check_profile_bundles:
fn check_connectors(root: &Path, playbook: &Playbook, acknowledge_untrusted: bool)
    -> Result<(BTreeMap<String, String>, BTreeMap<String, String>), Value>;
```

`check_connectors`: `connector::resolve::resolve_playbook`; resolution errors -> `json!({"policy": "connector_unresolved", "errors": [...]})`; unresolved env vars -> `{"policy": "connector_env_missing", "missing": [...]}` (fail early per spec 6 step 1); trust: connector digest not approved -> `{"policy": "untrusted_connector_requires_approve", "connectors": [names]}`; account digest not approved -> `{"policy": "unapproved_connector_account", "accounts": ["jira/project-board"], "fields": {...non-secret fields for display...}}`. NOTE: unlike playbook trust, `acknowledge_untrusted` does NOT bypass connector or account trust (they guard secret egress, not content taste) - approval happens via the trust store (CLI/UI approve flows), the refusal message says so. Sub-playbook children: extend `collect_children` to run `check_connectors` per child and merge maps (a child binding connectors gets the same gate).

Callers of `check_run` that hand `expected_*` to the engine must pass the two new maps into `run_config` (find them via `grep -rn "profile_bundles" crates/apb-mcp crates/apb-cli` and mirror the plumbing).

`connectors_list` MCP tool: read-only, returns `store::list()` plus per-connector function names and account names (no fields, no env values) - enough for an authoring agent to write bindings.

Approve flow: extend the existing approve surface (`playbook_approve` tool and/or `apb` approve CLI - locate with `grep -rn "approve" crates/apb-mcp/src crates/apb-cli/src`) with `kind: connector` and `kind: connector_account` inputs that call `TrustStore::approve_kind` with the right `Kind` and id (`account_trust_id`).

- [ ] **Step 1: Write failing tests** (pattern-match the existing sub-playbook policy module in `tests/suite/`): run with unapproved connector refused with that policy code; approving the tree digest passes the connector check; changed account field refuses with `unapproved_connector_account`; approved everything yields a permit whose maps match the store digests exactly; missing env var refuses with `connector_env_missing`.
- [ ] **Step 2: Verify failure**, **Step 3: Implement**, **Step 4: Verify pass + fmt + clippy + `cargo test -p apb-mcp`**.
- [ ] **Step 5: Commit**: `git commit --signoff -m "feat(mcp): connector trust gate, permit maps, connectors_list tool"`

---

### Task 16: Server API

**Files:**
- Modify: `crates/apb-server/src/lib.rs` (routes `GET /api/connectors`, `GET /api/connectors/:name`, `POST /api/connectors/:name/healthcheck/:account`, `POST /api/connectors/approve`)
- Test: existing server test pattern (locate `#[cfg(test)]` or `tests/` in apb-server and mirror)

**Interfaces (JSON shapes the web consumes):**

```json
// GET /api/connectors -> [{ "name", "version", "display_name", "summary", "tags",
//   "trust": "approved|changed|unapproved", "accounts_total": 2, "accounts_ready": 1 }]
// GET /api/connectors/:name -> { "name", "version", "meta": {...}, "body_md": "...",
//   "functions": [{ "name", "description", "read_only", "deprecated" }],
//   "accounts": [{ "name", "default", "fields": {...}, "missing_env": ["VAR"],
//                  "trust": "approved|changed|unapproved" }] }
// POST /api/connectors/:name/healthcheck/:account -> the call executor's JSON
// POST /api/connectors/approve { "name", "account": null | "acc" } -> { "ok": true }
```

Never include env values or secret material; `missing_env` is names only.

- [ ] **Step 1: Write failing test**: list endpoint returns the fixture connector with trust `unapproved`; approve endpoint flips it; detail endpoint carries `missing_env`.
- [ ] **Step 2: Verify failure**, **Step 3: Implement** (handlers call apb-core/store + resolve + TrustStore; healthcheck reuses `connector_call::execute`-adjacent live path from CLI doctor - factor that helper into `apb-engine::connector_call::healthcheck(name, account, root)` if not already shaped that way in Task 14).
- [ ] **Step 4: Verify pass + fmt + clippy**, **Step 5: Commit**: `git commit --signoff -m "feat(server): connectors API (list, detail, healthcheck, approve)"`

---

### Task 17: Web connectors pages

**Files:**
- Create: `web/src/pages/ConnectorList.svelte`, `web/src/pages/ConnectorView.svelte`
- Create: `web/src/lib/connectors.ts` (+ `web/src/lib/connectors.test.ts`)
- Modify: `web/src/App.svelte` (route), `web/src/lib/api.ts` (fetch helpers), `web/src/lib/components/Topbar.svelte` (nav item)

**Interfaces:**

```ts
// lib/connectors.ts
export interface ConnectorCard { name: string; version: string; displayName: string; summary: string; tags: string[]; trust: 'approved'|'changed'|'unapproved'; accountsTotal: number; accountsReady: number }
export interface ConnectorAccount { name: string; default: boolean; fields: Record<string,string>; missingEnv: string[]; trust: 'approved'|'changed'|'unapproved' }
export function trustBadge(t: ConnectorCard['trust']): { label: string; tone: 'ok'|'warn'|'muted' }
export function accountReady(a: ConnectorAccount): boolean   // missingEnv.length === 0
```

Pages follow `ProfileList.svelte` / `ProfileEdit.svelte` structure and shadcn-svelte components (Card, Table, Badge equivalents already in `lib/components/ui`). ConnectorView: markdown body rendered with whatever the project already uses for markdown (grep `marked|markdown` in web/src; if nothing exists, render the body in a `<pre class="whitespace-pre-wrap">` - do not add a markdown dep in this task), functions table, accounts table with per-account healthcheck button (POST, show outcome code) and approve button when trust is not `approved`.

- [ ] **Step 1: Write failing vitest** for `trustBadge` and `accountReady`: `cd web && bun run test` - FAIL.
- [ ] **Step 2: Implement** lib + pages + route + nav.
- [ ] **Step 3: Verify**: `bun run test` PASS, `bun run check` clean, `bun run build` succeeds.
- [ ] **Step 4: Commit**: `git commit --signoff -m "feat(web): connectors pages with trust, accounts, healthcheck"`

---

### Task 18: Web node-form connector bindings

**Files:**
- Modify: `web/src/lib/NodePanel.svelte` (agent_task section)
- Create: `web/src/lib/connectorbinding.ts` (+ `.test.ts`)
- Modify: `web/src/lib/playbookyaml.ts` if node YAML serialization lives there (check where `profile`/`skills` node fields serialize)

**Interfaces:**

```ts
// connectorbinding.ts - mirror profileref.ts style
export type FunctionsAllow = 'all' | 'read_only' | string[]
export interface ConnectorBinding { name: string; accounts?: string[]; functions?: FunctionsAllow; maxCalls?: number }
export function parseBinding(yaml: unknown): ConnectorBinding      // string shorthand or object
export function serializeBinding(b: ConnectorBinding): unknown     // shorthand when everything default
export function toggleListEntry(list: string[] | undefined, entry: string, all: string[]): string[] | undefined
// checked-all collapses back to undefined (absent allowlist form)
```

UI: "Connectors" block in the agent_task panel; add via the existing `Combobox.svelte` searching `GET /api/connectors` names; per added connector - account checkboxes and function checkboxes (all checked by default; `read_only` preset button), `max_calls` numeric input, untrusted badge from list data, remove button.

- [ ] **Step 1: Write failing vitest** for parse/serialize roundtrips including the `read_only` string form and the shorthand collapse.
- [ ] **Step 2: Implement**, **Step 3: Verify** (`bun run test`, `bun run check`, `bun run build`).
- [ ] **Step 4: Commit**: `git commit --signoff -m "feat(web): node connector bindings editor"`

---

### Task 19: mock-tracker fixture, end-to-end test, docs, final gates

**Files:**
- Create: `crates/apb-engine/tests/fixtures/connectors/mock-tracker/connector.yaml`
- Create: `crates/apb-engine/tests/fixtures/connectors/mock-tracker/PUBLIC.md`
- Create: `crates/apb-engine/tests/suite/connector_e2e.rs` (+ `mod` line in `tests/main.rs`)
- Modify: `docs/superpowers/specs/2026-07-18-connectors-design.md` (status: implemented, note deviations if any)
- Modify: authoring docs where profiles/skills are documented (locate: `grep -rln "profile_howto\|skills" docs/ | head`) with a short connectors section

**mock-tracker connector.yaml (complete):**

```yaml
name: mock-tracker
version: 0.1.0
healthcheck: ping
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
  - name: ping
    description: Fake reachability check
    mock: { status: 200, body: { ok: true } }
  - name: fail_auth
    description: Always answers like an expired token
    mock: { status: 401, body: { error: unauthorized } }
  - name: rate_limit
    description: Always answers like a throttled client
    mock: { status: 429, body: { error: slow_down } }
  - name: list_items
    description: List items over HTTP
    read_only: true
    method: GET
    url: "{{account.base_url}}/items"
    query: { q: "{{args.q}}" }
    args_schema: { type: object, properties: { q: { type: string } }, required: [q] }
  - name: create_item
    description: Create an item over HTTP
    method: POST
    url: "{{account.base_url}}/items"
    body: "{{args}}"
    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }
```

**E2E test flow** (single suite module; the ephemeral HTTP server helper from Task 13 lives in `tests/suite/common` so both modules share it): install mock-tracker into temp `APB_CONFIG_DIR`; write project config with two accounts (one default) pointing `base_url` at the local server; set the token via project `secrets.env`; resolve + snapshot into a run dir with a grant `{functions: read_only, max_calls: 2}`; assert `execute` ok for `list_items`, `permission` for `create_item` (not granted), `permission` on the third call (budget), redacted token in an echo response, and that editing the live connector.yaml after snapshot changes nothing.

- [ ] **Step 1: Write the failing e2e test**, **Step 2: Verify failure**, **Step 3: Fill gaps it exposes**, **Step 4: Verify pass**.
- [ ] **Step 5: Full gates**:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd web && bun run test && bun run check && bun run build && cd ..
cargo metadata --format-version 1 >/dev/null && code-ranker check .
```

Fix every finding (for code-ranker violations read `code-ranker docs base <ID>` first) and re-run until clean.

- [ ] **Step 6: Commit**: `git commit --signoff -m "test(engine): mock-tracker fixture and connectors end-to-end coverage; docs"`

---

## Self-review notes (spec coverage map)

- Spec 3 (folder, manifest, PUBLIC.md): Tasks 2, 3, 6.
- Spec 4 (configs, secrets, isolation): Tasks 4, 5, 12.
- Spec 5 (binding, validation, read_only, max_calls): Tasks 8, 9.
- Spec 6 (pipeline, hardening, redaction TODO, dry-run): Tasks 10, 11, 12, 13.
- Spec 7 (trust, account pinning): Tasks 7, 15 (+ UI approve in 16/17).
- Spec 8 (errors): Task 13.
- Spec 9 (UI/API incl. healthcheck, stats): Tasks 16, 17, 18. Usage stats live in ConnectorView reading existing run events via existing run APIs; if no aggregate endpoint exists, add a `GET /api/connectors/:name/stats` handler in Task 16 aggregating `ConnectorCall` events from recent runs.
- Spec 10 (CLI): Task 14.
- Spec 11 (fixture, tests): Task 19 plus per-task tests.
- Spec 12 (crates, catalog effects): effects in Task 8; `connectors_list` in Task 15.
