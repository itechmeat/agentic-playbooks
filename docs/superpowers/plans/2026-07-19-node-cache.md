# Node Result Cache (Incremental Execution) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A content-addressed cache of node results so a repeated playbook run skips nodes whose inputs did not change, per the approved spec `docs/superpowers/specs/2026-07-19-node-cache-design.md`.

**Architecture:** New `cache` and `fingerprint` modules in `apb-core` (types, key builder, file-based CAS store, git-aware and file-set fingerprints); lookup and admission wrapped around the existing `execute_node` call in the `apb-engine` scheduler drive loop; hybrid trust (node-level `cache: auto` is intent, admission requires post-hoc verification that the workspace did not change and all connector calls were read-only); four new event variants; CLI flags and `apb cache` subcommands; a `cached` badge in the web RunView.

**Tech Stack:** Rust workspace edition 2024 (serde, thiserror, sha2 via `apb_core::content`), `globset` (new dependency in apb-core), svelte 5 + vitest in `web/`.

## Global Constraints

- No em-dashes (U+2014), no exclamation marks, no CJK in code, docs, or user-facing strings.
- New fields on existing `EventPayload` variants only with `#[serde(default)]`. New variants are additive and safe.
- All state files written via `apb_core::fsutil` (atomic temp + rename; use `atomic_write` for cache files).
- Machine-facing fields are English.
- Gates before every commit: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo metadata --format-version 1 >/dev/null && code-ranker check .` (fix violations via `code-ranker docs base <ID>` before proceeding).
- Every commit: `git commit --signoff` and end the message with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Never commit to local main. Do all work on a new branch `feat/node-cache` cut from `origin/main`.
- The cache never fails a run: every cache-layer error degrades to a miss or a rejected admission.
- Spec deviations already decided: record timestamp is `created_at_unix: u64` (unix seconds; avoids a chrono dependency), and cacheable node kinds are exactly `agent_task` and `script`.

---

### Task 1: Core schema, `cache:` / `inputs:` / `outputs:` on Node, day suffix in durations

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (Node struct at ~line 470, add types near `Effect`)
- Modify: `crates/apb-core/src/duration.rs` (add `d` suffix)
- Test: inline `#[cfg(test)]` in the same files (repo convention)

**Interfaces:**
- Produces: `CacheSpec` (untagged: `CacheMode` shorthand or `CacheConfig`), `CacheMode { Auto, Off }` (default `Off`), `CacheConfig { mode: CacheMode, ttl: Option<String> }`, `NodeFiles { files: Vec<String> }`; `Node` gains `pub cache: Option<CacheSpec>`, `pub inputs: Option<NodeFiles>`, `pub outputs: Option<NodeFiles>`; helpers `Node::cache_mode() -> CacheMode` and `Node::cache_ttl_seconds() -> Option<u64>`; `parse_duration_str("2d") == Some(172800)`.

- [ ] **Step 1: Write failing tests** in `schema.rs` tests module:

```rust
#[test]
fn parses_cache_shorthand_and_full_form() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    cache: auto
    inputs: { files: ["src/**", "Cargo.toml"] }
    outputs: { files: ["findings.json"] }
  - id: b
    type: script
    script: lint.sh
    runner: sh
    cache: { mode: auto, ttl: 2d }
edges: []
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let a = pb.node("a").unwrap();
    assert_eq!(a.cache_mode(), CacheMode::Auto);
    assert_eq!(a.cache_ttl_seconds(), None);
    assert_eq!(a.inputs.as_ref().unwrap().files.len(), 2);
    assert_eq!(a.outputs.as_ref().unwrap().files, vec!["findings.json"]);
    let b = pb.node("b").unwrap();
    assert_eq!(b.cache_mode(), CacheMode::Auto);
    assert_eq!(b.cache_ttl_seconds(), Some(172_800));
    assert_eq!(pb.node("s").unwrap().cache_mode(), CacheMode::Off);
}
```

And in `duration.rs` tests: `assert_eq!(parse_duration_str("2d"), Some(172_800));`

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p apb-core parses_cache_shorthand_and_full_form`
Expected: compile error (types do not exist yet).

- [ ] **Step 3: Implement.** In `duration.rs` add the day arm to the suffix match:

```rust
        b'd' => (&s[..s.len() - 1], 86_400u64),
```

In `schema.rs`, next to `Effect`:

```rust
/// Node-level cache declaration (spec 2026-07-19-node-cache-design). Intent
/// only: admission additionally requires the engine's post-hoc verification.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CacheSpec {
    Mode(CacheMode),
    Config(CacheConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    Auto,
    #[default]
    Off,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub mode: CacheMode,
    #[serde(default)]
    pub ttl: Option<String>,
}

/// File globs a node declares as inputs (fingerprint refinement) or outputs
/// (captured artifacts, excluded from the post-run fingerprint comparison).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeFiles {
    #[serde(default)]
    pub files: Vec<String>,
}
```

On `Node` (after `expected_duration`, before the flattened `kind`):

```rust
    #[serde(default)]
    pub inputs: Option<NodeFiles>,
    #[serde(default)]
    pub outputs: Option<NodeFiles>,
    #[serde(default)]
    pub cache: Option<CacheSpec>,
```

In `impl Node`:

```rust
    pub fn cache_mode(&self) -> CacheMode {
        match &self.cache {
            None => CacheMode::Off,
            Some(CacheSpec::Mode(m)) => *m,
            Some(CacheSpec::Config(c)) => c.mode,
        }
    }
    pub fn cache_ttl_seconds(&self) -> Option<u64> {
        match &self.cache {
            Some(CacheSpec::Config(c)) => {
                c.ttl.as_deref().and_then(crate::duration::parse_duration_str)
            }
            _ => None,
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p apb-core parses_cache_shorthand_and_full_form && cargo test -p apb-core duration`
Expected: PASS.

- [ ] **Step 5: Gates, then commit**

```bash
git commit --signoff -m "core: add node cache/inputs/outputs schema and day durations" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Validator codes V27, V28, V29

**Files:**
- Modify: `crates/apb-core/src/validate.rs` (add `check_cache` next to `check_connectors`, called from `validate()` at line ~58)
- Modify: `crates/apb-core/Cargo.toml` (add `globset = "0.4"`)
- Test: inline tests in `validate.rs`

**Interfaces:**
- Consumes: `Node::cache_mode()`, `CacheMode`, `NodeFiles` from Task 1.
- Produces: V27 error (`cache: auto` on a kind other than `agent_task` or `script`), V28 warning (`ttl` set while mode is `off`), V29 error (invalid glob in `inputs.files` or `outputs.files`). Also a public helper other tasks reuse: `pub fn build_globset(globs: &[String]) -> Result<globset::GlobSet, String>` (returns the offending glob as the error).

- [ ] **Step 1: Write failing tests** (yaml snippets through `Playbook::from_yaml` + `validate`, following the pattern of the existing V26 tests in `validate.rs`):

```rust
#[test]
fn v27_cache_on_uncacheable_kind() {
    let pb = pb_yaml(r#"
nodes:
  - { id: s, type: start }
  - { id: c, type: condition, cache: auto }
edges: []"#);
    assert!(codes(&pb).contains(&("V27", Severity::Error)));
}

#[test]
fn v28_ttl_without_auto_mode() {
    let pb = pb_yaml(r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: x, cache: { mode: off, ttl: 1h } }
edges: []"#);
    assert!(codes(&pb).contains(&("V28", Severity::Warning)));
}

#[test]
fn v29_invalid_glob() {
    let pb = pb_yaml(r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: x, cache: auto, inputs: { files: ["src/[**"] } }
edges: []"#);
    assert!(codes(&pb).contains(&("V29", Severity::Error)));
}
```

Reuse or add the local helpers `pb_yaml` / `codes` in the same style the existing validator tests use (build the playbook, run `validate`, collect `(code, severity)` pairs).

- [ ] **Step 2: Run to verify failure.** `cargo test -p apb-core v27_` etc. Expected: FAIL (codes absent).

- [ ] **Step 3: Implement** `check_cache` and call it from `validate()`:

```rust
/// V27: cache on a node kind the engine never caches. V28: ttl that can
/// never apply. V29: invalid glob in inputs/outputs.
fn check_cache(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        let cacheable = matches!(node.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. });
        if node.cache_mode() == CacheMode::Auto && !cacheable {
            r.push(Issue::error("V27", &node.id,
                "cache: auto is only supported on agent_task and script nodes"));
        }
        if let Some(CacheSpec::Config(c)) = &node.cache
            && c.ttl.is_some()
            && c.mode == CacheMode::Off
        {
            r.push(Issue::warning("V28", &node.id,
                "cache ttl has no effect while cache mode is off"));
        }
        for nf in [&node.inputs, &node.outputs].into_iter().flatten() {
            if let Err(bad) = build_globset(&nf.files) {
                r.push(Issue::error("V29", &node.id,
                    &format!("invalid glob `{bad}` in inputs/outputs files")));
            }
        }
    }
}

pub fn build_globset(globs: &[String]) -> Result<globset::GlobSet, String> {
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        b.add(globset::Glob::new(g).map_err(|_| g.clone())?);
    }
    b.build().map_err(|_| globs.join(","))
}
```

Match the actual `Issue` constructor names used by neighbouring checks in `validate.rs` (adapt `Issue::error` / `Issue::warning` to the real API in that file).

- [ ] **Step 4: Run tests.** `cargo test -p apb-core validate`. Expected: PASS.
- [ ] **Step 5: Gates, then commit** (`core: add cache validator codes V27-V29`).

---

### Task 3: Fingerprint module

**Files:**
- Create: `crates/apb-core/src/fingerprint.rs`
- Modify: `crates/apb-core/src/lib.rs` (add `pub mod fingerprint;`)
- Test: inline tests using `tempfile` (already a dev-dependency of apb-core; if not, add it) plus `std::process::Command` git fixtures

**Interfaces:**
- Consumes: `content::sha256_hex`, `validate::build_globset`.
- Produces:
  - `pub fn git_fingerprint(root: &Path, exclude: &[String]) -> Option<String>` (None when not a git work tree, git unavailable, or no HEAD yet)
  - `pub fn files_fingerprint(root: &Path, include: &[String], exclude: &[String]) -> Result<String, FingerprintError>`
  - `pub enum FingerprintError { Io(std::io::Error), Glob(String) }` (thiserror)

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn git_fingerprint_tracks_dirty_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::write(root.join("a.txt"), "one").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
    let clean = git_fingerprint(root, &[]).unwrap();
    assert_eq!(clean, git_fingerprint(root, &[]).unwrap()); // stable
    std::fs::write(root.join("a.txt"), "two").unwrap(); // unstaged edit
    let dirty = git_fingerprint(root, &[]).unwrap();
    assert_ne!(clean, dirty);
    std::fs::write(root.join("new.txt"), "x").unwrap(); // untracked
    assert_ne!(dirty, git_fingerprint(root, &[]).unwrap());
}

#[test]
fn git_fingerprint_none_outside_git() {
    let dir = tempfile::tempdir().unwrap();
    assert!(git_fingerprint(dir.path(), &[]).is_none());
}

#[test]
fn git_fingerprint_exclude_ignores_declared_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::write(root.join("a.txt"), "one").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
    let exclude = vec!["out.json".to_string()];
    let clean = git_fingerprint(root, &exclude).unwrap();
    std::fs::write(root.join("out.json"), "artifact").unwrap(); // declared output
    assert_eq!(clean, git_fingerprint(root, &exclude).unwrap());
    std::fs::write(root.join("undeclared.txt"), "x").unwrap();
    assert_ne!(clean, git_fingerprint(root, &exclude).unwrap());
}

#[test]
fn files_fingerprint_matches_only_globs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "a").unwrap();
    std::fs::write(root.join("other.md"), "m").unwrap();
    let fp = files_fingerprint(root, &["src/**".into()], &[]).unwrap();
    std::fs::write(root.join("other.md"), "changed").unwrap();
    assert_eq!(fp, files_fingerprint(root, &["src/**".into()], &[]).unwrap());
    std::fs::write(root.join("src/a.rs"), "b").unwrap();
    assert_ne!(fp, files_fingerprint(root, &["src/**".into()], &[]).unwrap());
}
```

Fill in the elided third test body fully in the actual test file (fixture, `git_fingerprint(root, &["out.json".into()])`, equality assertion).

- [ ] **Step 2: Run to verify failure.** `cargo test -p apb-core fingerprint`. Expected: compile error.

- [ ] **Step 3: Implement:**

```rust
//! Workspace fingerprints for the node cache (spec 2026-07-19).
use crate::content::sha256_hex;
use crate::validate::build_globset;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid glob `{0}`")]
    Glob(String),
}

/// Git-aware fingerprint: HEAD + staged/unstaged diff + untracked contents.
/// `exclude` globs (declared node outputs) are filtered out of the dirty
/// state so a node's own products do not count as workspace changes.
pub fn git_fingerprint(root: &Path, exclude: &[String]) -> Option<String> {
    let ex = build_globset(exclude).ok()?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let mut diff_args = vec!["diff", "HEAD", "--binary", "--", "."];
    let pathspecs: Vec<String> =
        exclude.iter().map(|g| format!(":(exclude){g}")).collect();
    diff_args.extend(pathspecs.iter().map(String::as_str));
    let diff = git(root, &diff_args)?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut acc = Vec::new();
    acc.extend_from_slice(head.as_bytes());
    acc.extend_from_slice(sha256_hex(diff.as_bytes()).as_bytes());
    let mut files: Vec<&str> =
        untracked.split('\0').filter(|p| !p.is_empty() && !ex.is_match(p)).collect();
    files.sort_unstable();
    for path in files {
        acc.extend_from_slice(path.as_bytes());
        let bytes = std::fs::read(root.join(path)).ok()?;
        acc.extend_from_slice(sha256_hex(&bytes).as_bytes());
    }
    Some(sha256_hex(&acc))
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Hash of exactly the files matching `include` minus `exclude`, as sorted
/// (relative path, content digest) pairs. Skips `.git` and `.apb`.
pub fn files_fingerprint(
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<String, FingerprintError> {
    let inc = build_globset(include).map_err(FingerprintError::Glob)?;
    let ex = build_globset(exclude).map_err(FingerprintError::Glob)?;
    let mut paths = Vec::new();
    walk(root, root, &mut paths)?;
    paths.sort_unstable();
    let mut acc = Vec::new();
    for rel in paths {
        if inc.is_match(&rel) && !ex.is_match(&rel) {
            acc.extend_from_slice(rel.as_bytes());
            let bytes = std::fs::read(root.join(&rel))?;
            acc.extend_from_slice(sha256_hex(&bytes).as_bytes());
        }
    }
    Ok(sha256_hex(&acc))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name == ".git" || name == ".apb" {
                continue;
            }
            walk(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}
```

Adjust `sha256_hex` calls to its real signature in `content.rs` (it takes `&[u8]`; if it takes a different input, adapt).

- [ ] **Step 4: Run tests.** `cargo test -p apb-core fingerprint`. Expected: PASS.
- [ ] **Step 5: Gates, then commit** (`core: add workspace fingerprint module`).

---

### Task 4: Cache store module (key builder, record, CAS)

**Files:**
- Create: `crates/apb-core/src/cache.rs`
- Modify: `crates/apb-core/src/lib.rs` (add `pub mod cache;`)
- Test: inline tests with `tempfile`

**Interfaces:**
- Consumes: `content::sha256_hex`, `fsutil::atomic_write`.
- Produces (used verbatim by Tasks 5-8):

```rust
pub const CACHE_FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactScope { Run, Workspace }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub digest: String,   // "sha256:<hex>"
    pub scope: ArtifactScope,
    pub path: String,     // relative to the scope root
}

#[derive(Serialize)]
pub struct KeyParts<'a> {
    pub format: u32,
    pub node_def: &'a str,               // canonical JSON of the Node
    pub script_digest: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub rendered_prompt: Option<&'a str>,
    pub bundle_digest: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
    pub connector_digests: Vec<String>,  // sorted by caller
    pub workspace_fingerprint: &'a str,
}
pub fn cache_key(parts: &KeyParts) -> String;   // "sha256:<hex of canonical JSON>"

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub run_id: String,
    pub playbook_id: String,
    pub playbook_version: String,
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub workspace_unchanged: bool,
    pub connector_calls: String,  // "read_only" | "none"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRecord {
    pub format_version: u32,
    pub key: String,
    pub created_at_unix: u64,
    pub node_type: String,
    pub provenance: Provenance,
    #[serde(default)]
    pub profile_bundle_digest: Option<String>,
    pub workspace_fingerprint: String,
    pub verification: Verification,
    pub output_digest: String,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

pub struct CachedEntry { pub record: CacheRecord, pub output: String }

pub struct CacheStore { /* root: PathBuf */ }
impl CacheStore {
    pub fn open(project_root: &Path) -> CacheStore;          // root/.apb/cache
    pub fn load(&self, key: &str, now_unix: u64) -> Option<CachedEntry>;
    pub fn read_object(&self, digest: &str) -> Option<Vec<u8>>; // digest-verified
    pub fn store(
        &self,
        record: &CacheRecord,
        output: &str,
        artifacts: &[(ArtifactRef, Vec<u8>)],
    ) -> std::io::Result<()>;
    pub fn status(&self) -> StoreStatus;                     // records, objects, bytes
    pub fn inspect(&self, key: &str) -> Option<CacheRecord>;
    pub fn prune(
        &self,
        older_than_secs: Option<u64>,
        max_bytes: Option<u64>,
        now_unix: u64,
    ) -> PruneReport;
    pub fn clear(&self) -> std::io::Result<()>;
}
pub struct StoreStatus { pub records: usize, pub objects: usize, pub total_bytes: u64 }
pub struct PruneReport { pub removed_records: usize, pub removed_objects: usize }
```

Layout inside the store: `format` (one line `1`), `records/<hex[0..2]>/<hex>.json`, `objects/<hex[0..2]>/<hex>` where `<hex>` strips the `sha256:` prefix.

- [ ] **Step 1: Write failing tests:**

```rust
fn record(key: &str, output: &str, ttl: Option<u64>) -> CacheRecord {
    CacheRecord {
        format_version: CACHE_FORMAT,
        key: key.into(),
        created_at_unix: 1000,
        node_type: "script".into(),
        provenance: Provenance {
            run_id: "r1".into(), playbook_id: "p".into(),
            playbook_version: "1.0.0".into(), node_id: "n".into(),
        },
        profile_bundle_digest: None,
        workspace_fingerprint: "sha256:ws".into(),
        verification: Verification { workspace_unchanged: true, connector_calls: "none".into() },
        output_digest: format!("sha256:{}", crate::content::sha256_hex(output.as_bytes())),
        artifacts: vec![],
        ttl_seconds: ttl,
    }
}

#[test]
fn store_and_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = CacheStore::open(dir.path());
    let key = "sha256:aaaa";
    store.store(&record(key, "hello", None), "hello", &[]).unwrap();
    let entry = store.load(key, 2000).unwrap();
    assert_eq!(entry.output, "hello");
    assert_eq!(entry.record.provenance.run_id, "r1");
}

#[test]
fn ttl_expiry_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let store = CacheStore::open(dir.path());
    store.store(&record("sha256:bbbb", "x", Some(10)), "x", &[]).unwrap();
    assert!(store.load("sha256:bbbb", 1005).is_some()); // within ttl
    assert!(store.load("sha256:bbbb", 2000).is_none()); // expired
}

#[test]
fn corrupt_object_is_a_miss_and_record_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let store = CacheStore::open(dir.path());
    let rec = record("sha256:cccc", "good", None);
    store.store(&rec, "good", &[]).unwrap();
    // overwrite the object with different content, digest now mismatches
    let hex = rec.output_digest.trim_start_matches("sha256:");
    let obj = dir.path().join(".apb/cache/objects").join(&hex[..2]).join(hex);
    std::fs::write(&obj, "tampered").unwrap();
    assert!(store.load("sha256:cccc", 2000).is_none());
    assert!(store.inspect("sha256:cccc").is_none()); // record removed
}

#[test]
fn key_changes_when_any_part_changes() {
    let base = KeyParts {
        format: CACHE_FORMAT, node_def: "{}", script_digest: None, runner: None,
        rendered_prompt: Some("p"), bundle_digest: Some("b"), agent: Some("claude"),
        model: Some("m"), connector_digests: vec![], workspace_fingerprint: "w",
    };
    let k1 = cache_key(&base);
    let k2 = cache_key(&KeyParts { rendered_prompt: Some("p2"), ..base });
    assert_ne!(k1, k2);
    assert!(k1.starts_with("sha256:"));
}
```

Note: `KeyParts` needs `#[derive(Clone)]` for the struct-update test; add it.

- [ ] **Step 2: Run to verify failure.** `cargo test -p apb-core cache`. Expected: compile error.

- [ ] **Step 3: Implement.** Core mechanics (fill in the obvious helpers):

```rust
pub fn cache_key(parts: &KeyParts) -> String {
    let json = serde_json::to_string(parts).expect("key parts serialize");
    format!("sha256:{}", crate::content::sha256_hex(json.as_bytes()))
}

impl CacheStore {
    pub fn open(project_root: &Path) -> CacheStore {
        CacheStore { root: project_root.join(".apb/cache") }
    }
    fn shard(&self, kind: &str, digest: &str) -> PathBuf {
        let hex = digest.trim_start_matches("sha256:");
        self.root.join(kind).join(&hex[..2.min(hex.len())]).join(hex)
    }
    pub fn store(&self, record: &CacheRecord, output: &str,
                 artifacts: &[(ArtifactRef, Vec<u8>)]) -> std::io::Result<()> {
        crate::fsutil::atomic_write(&self.root.join("format"),
            format!("{CACHE_FORMAT}\n").as_bytes())?;
        crate::fsutil::atomic_write(&self.shard("objects", &record.output_digest),
            output.as_bytes())?;
        for (a, bytes) in artifacts {
            crate::fsutil::atomic_write(&self.shard("objects", &a.digest), bytes)?;
        }
        let json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
        let mut rec_path = self.shard("records", &record.key);
        rec_path.set_extension("json");
        crate::fsutil::atomic_write(&rec_path, &json)
    }
    pub fn load(&self, key: &str, now_unix: u64) -> Option<CachedEntry> {
        let mut rec_path = self.shard("records", key);
        rec_path.set_extension("json");
        let bytes = std::fs::read(&rec_path).ok()?;
        let Ok(record) = serde_json::from_slice::<CacheRecord>(&bytes) else {
            let _ = std::fs::remove_file(&rec_path); // corrupt record
            return None;
        };
        if record.format_version != CACHE_FORMAT {
            return None;
        }
        if let Some(ttl) = record.ttl_seconds
            && now_unix.saturating_sub(record.created_at_unix) > ttl
        {
            return None;
        }
        let Some(output) = self.read_object(&record.output_digest) else {
            let _ = std::fs::remove_file(&rec_path); // object missing or tampered
            return None;
        };
        Some(CachedEntry { record, output: String::from_utf8_lossy(&output).into_owned() })
    }
    pub fn read_object(&self, digest: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.shard("objects", digest)).ok()?;
        let actual = format!("sha256:{}", crate::content::sha256_hex(&bytes));
        (actual == digest).then_some(bytes)
    }
}
```

`status`, `inspect`, `prune`, `clear` are straightforward directory walks over `records/` and `objects/`; `prune` first removes records older than `older_than_secs`, then, while total size still exceeds `max_bytes`, removes the oldest remaining records (by `created_at_unix`), and finally removes objects no surviving record references; `clear` is `remove_dir_all` on `records/` and `objects/`.

- [ ] **Step 4: Run tests.** `cargo test -p apb-core cache`. Expected: PASS.
- [ ] **Step 5: Gates, then commit** (`core: add content-addressed node cache store`).

---

### Task 5: Engine events, run cache mode, script node lookup and admission

**Files:**
- Modify: `crates/apb-engine/src/event.rs` (new `EventPayload` variants)
- Modify: `crates/apb-engine/src/run_config.rs` (`RunConfig` gains `cache: CacheRunMode`)
- Create: `crates/apb-engine/src/scheduler/cache.rs` (lookup/admission helpers; wire `mod cache;` where `mod node;` is declared for the scheduler module)
- Modify: `crates/apb-engine/src/scheduler.rs` (the drive-loop call site at the `execute_node` branch, ~line 1045-1072)
- Test: create `crates/apb-engine/tests/suite/cache_test.rs`, register it exactly like the neighbouring `mod event_test;` line in `crates/apb-engine/tests/main.rs`

**Interfaces:**
- Consumes: `apb_core::cache::{CacheStore, CacheRecord, KeyParts, cache_key, Provenance, Verification, CachedEntry}`, `apb_core::fingerprint::{git_fingerprint, files_fingerprint}`, `apb_core::schema::CacheMode`, `Node::cache_mode`, `Node::cache_ttl_seconds`.
- Produces:
  - `EventPayload::{NodeCacheHit, NodeCacheMiss, NodeCacheStored, NodeCacheRejected}`
  - `CacheRunMode { Auto, Off, Refresh }` (serde snake_case, `#[default] Auto`) on `RunConfig` as `#[serde(default)] pub cache: CacheRunMode`
  - `scheduler::cache::prepare(...) -> Option<NodeCacheCtx>` and `NodeCacheCtx::{lookup, admit}` used by the drive loop (exact signatures below)

- [ ] **Step 1: Add the event variants** (no test first; the log format is covered by the integration test below):

```rust
    /// Node cache (spec 2026-07-19-node-cache-design). A lookup always ends
    /// in exactly one of hit/miss; stored/rejected reports admission.
    NodeCacheHit {
        node: String,
        key: String,
        source_run: String,
    },
    NodeCacheMiss {
        node: String,
        key: String,
    },
    NodeCacheStored {
        node: String,
        key: String,
    },
    NodeCacheRejected {
        node: String,
        reason: String,
    },
```

- [ ] **Step 2: Add `CacheRunMode`** in `run_config.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheRunMode {
    #[default]
    Auto,     // honor node-level cache declarations
    Off,      // --no-cache: no lookup, no admission
    Refresh,  // --refresh-cache: no lookup, admission overwrites
}
```

and on `RunConfig`: `#[serde(default)] pub cache: CacheRunMode,`

- [ ] **Step 3: Write the failing integration test** in `cache_test.rs`. Build on the harness helpers in `crates/apb-engine/tests/suite/common/` the way `background_run_test.rs` and `connector_e2e.rs` do (temp project dir, playbook YAML, drive the run, read events). The scenario, with a git-initialized workdir:

```rust
// Playbook: start -> lint(script, cache: auto) -> finish.
// scripts/lint.sh: `echo linted` (no workspace writes).
//
// Run 1: assert events contain NodeCacheMiss{node:"lint"} and
//        NodeCacheStored{node:"lint"}; NodeFinished output == "linted".
// Run 2 (fresh run, same workdir): assert NodeCacheHit{node:"lint",
//        source_run: <run 1 id>} and NO AttemptStarted for "lint";
//        NodeFinished output == "linted", status succeeded.
// Run 3 with cfg.cache = CacheRunMode::Off: assert no cache events at all.
// Run 4: touch a tracked workspace file, run again with Auto:
//        assert NodeCacheMiss (fingerprint changed).
// Also: a script that writes a workspace file gets NodeCacheRejected
//        with reason containing "workspace" and no NodeCacheStored.
```

Write these as separate `#[test]` functions sharing a fixture builder; assert by scanning `read_all(run_dir)` events.

- [ ] **Step 4: Run to verify failure.** `cargo test -p apb-engine --test main cache_`. Expected: FAIL (no cache behavior yet).

- [ ] **Step 5: Implement `scheduler/cache.rs`:**

```rust
//! Node cache lookup and admission (spec 2026-07-19). Wraps execute_node in
//! the drive loop; every failure here degrades to a miss or a rejection,
//! never a run error.
use super::*;
use apb_core::cache::{
    cache_key, CacheRecord, CacheStore, CachedEntry, KeyParts, Provenance, Verification,
};
use apb_core::fingerprint::{files_fingerprint, git_fingerprint};
use apb_core::schema::CacheMode;

pub(crate) struct NodeCacheCtx {
    pub key: String,
    store: CacheStore,
    ttl: Option<u64>,
    pre_fingerprint: String,
    node_id: String,
    node_type: &'static str,
    bundle_digest: Option<String>,
    exclude: Vec<String>, // declared outputs.files
}

/// None when the node is not cache-eligible (kind, declaration, run mode,
/// or no computable fingerprint).
pub(crate) fn prepare(
    playbook: &Playbook,
    node_id: &str,
    workdir: &Path,
    run_dir: &Path,
    cfg: &RunConfig,
    rendered_prompt: Option<&str>,
    bundle_digest: Option<&str>,
    agent_model: Option<(&str, &str)>,
    connector_digests: Vec<String>,
) -> Option<NodeCacheCtx> {
    if cfg.cache == CacheRunMode::Off {
        return None;
    }
    let node = playbook.node(node_id)?;
    if node.cache_mode() != CacheMode::Auto {
        return None;
    }
    let exclude = node.outputs.as_ref().map(|o| o.files.clone()).unwrap_or_default();
    let fingerprint = match node.inputs.as_ref() {
        Some(inp) if !inp.files.is_empty() => {
            files_fingerprint(workdir, &inp.files, &[]).ok()?
        }
        _ => git_fingerprint(workdir, &[])?,
    };
    let (script_digest, runner) = match &node.kind {
        NodeKind::Script { script, runner, .. } => {
            let bytes = std::fs::read(run_dir.join("scripts").join(script)).ok()?;
            (Some(format!("sha256:{}", apb_core::content::sha256_hex(&bytes))), Some(runner.clone()))
        }
        _ => (None, None),
    };
    let node_def = serde_json::to_string(node).ok()?;
    let key = cache_key(&KeyParts {
        format: apb_core::cache::CACHE_FORMAT,
        node_def: &node_def,
        script_digest: script_digest.as_deref(),
        runner: runner.as_deref(),
        rendered_prompt,
        bundle_digest,
        agent: agent_model.map(|(a, _)| a),
        model: agent_model.map(|(_, m)| m),
        connector_digests,
        workspace_fingerprint: &fingerprint,
    });
    Some(NodeCacheCtx {
        key,
        store: CacheStore::open(workdir),
        ttl: node.cache_ttl_seconds(),
        pre_fingerprint: fingerprint,
        node_id: node_id.to_string(),
        node_type: node.kind_label(), // "script" | "agent_task"; add a small helper if absent
        bundle_digest: bundle_digest.map(str::to_string),
        exclude,
    })
}

impl NodeCacheCtx {
    pub(crate) fn lookup(&self, cfg: &RunConfig) -> Option<CachedEntry> {
        if cfg.cache == CacheRunMode::Refresh {
            return None;
        }
        self.store.load(&self.key, unix_now())
    }

    /// Post-hoc verification + store. Returns the event to append.
    pub(crate) fn admit(
        &self,
        workdir: &Path,
        run_id: &str,
        playbook: &Playbook,
        output: &str,
        connector_calls_ok: bool,
        had_connector_calls: bool,
        artifacts: &[(apb_core::cache::ArtifactRef, Vec<u8>)],
    ) -> EventPayload {
        if !connector_calls_ok {
            return EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: "connector call outside the read_only set".into(),
            };
        }
        let post = match playbook.node(&self.node_id).and_then(|n| n.inputs.as_ref()) {
            Some(inp) if !inp.files.is_empty() => {
                files_fingerprint(workdir, &inp.files, &self.exclude).ok()
            }
            _ => git_fingerprint(workdir, &self.exclude),
        };
        if post.as_deref() != Some(self.pre_fingerprint.as_str()) {
            return EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: "workspace changed during node execution".into(),
            };
        }
        let record = CacheRecord {
            format_version: apb_core::cache::CACHE_FORMAT,
            key: self.key.clone(),
            created_at_unix: unix_now(),
            node_type: self.node_type.into(),
            provenance: Provenance {
                run_id: run_id.into(),
                playbook_id: playbook.id.clone(),
                playbook_version: playbook.version.clone(),
                node_id: self.node_id.clone(),
            },
            profile_bundle_digest: self.bundle_digest.clone(),
            workspace_fingerprint: self.pre_fingerprint.clone(),
            verification: Verification {
                workspace_unchanged: true,
                connector_calls: if had_connector_calls { "read_only" } else { "none" }.into(),
            },
            output_digest: format!(
                "sha256:{}",
                apb_core::content::sha256_hex(output.as_bytes())
            ),
            artifacts: artifacts.iter().map(|(a, _)| a.clone()).collect(),
            ttl_seconds: self.ttl,
        };
        match self.store.store(&record, output, artifacts) {
            Ok(()) => EventPayload::NodeCacheStored {
                node: self.node_id.clone(),
                key: self.key.clone(),
            },
            Err(e) => EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: format!("store error: {e}"),
            },
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

In this task wire it for `script` nodes only: in the drive loop's `execute_node` branch of `scheduler.rs`, call `prepare(...)` with `rendered_prompt: None, bundle_digest: None, agent_model: None, connector_digests: vec![]`, and:

```rust
let cache_ctx = cache::prepare(&playbook, &current, &workdir, run_dir, cfg,
    None, None, None, vec![]);
let (st, out) = if let Some(hit) = cache_ctx.as_ref().and_then(|c| c.lookup(cfg)) {
    log.append(EventPayload::NodeCacheHit {
        node: current.clone(),
        key: cache_ctx.as_ref().unwrap().key.clone(),
        source_run: hit.record.provenance.run_id.clone(),
    })?;
    (NodeStatus::Succeeded, hit.output)
} else {
    if let Some(ctx) = &cache_ctx {
        log.append(EventPayload::NodeCacheMiss {
            node: current.clone(),
            key: ctx.key.clone(),
        })?;
    }
    let (st, out, evs) = execute_node(/* unchanged args */)?;
    for ev in &evs { /* existing append loop */ }
    if let Some(ctx) = &cache_ctx
        && st == NodeStatus::Succeeded
    {
        log.append(ctx.admit(&workdir, &run_id, &playbook, &out, true, false, &[]))?;
    }
    (st, out)
};
```

Keep the existing `NodeStarted` / `NodeFinished` appends around this block untouched. Restrict `prepare` to `NodeKind::Script` in this task (add a kind guard; the agent_task arm lands in Task 6).

- [ ] **Step 6: Run tests.** `cargo test -p apb-engine --test main cache_` then `cargo test --workspace`. Expected: PASS.
- [ ] **Step 7: Gates, then commit** (`engine: cache script node results with post-hoc verification`).

---

### Task 6: agent_task caching with bundle digest and connector verification

**Files:**
- Modify: `crates/apb-engine/src/scheduler/cache.rs` (drop the script-only guard; agent key parts)
- Modify: `crates/apb-engine/src/scheduler.rs` (pass agent key parts into `prepare`; scan node events for connector calls)
- Test: extend `crates/apb-engine/tests/suite/cache_test.rs`

**Interfaces:**
- Consumes: everything Task 5 produced; the run manifest (profile bundle digests and connector digests are snapshotted there at run start; read them the same way the drive loop's profile resolution does); `EventPayload::ConnectorCall { node_id, connector, function, .. }`; `apb_core::connector::def::ConnectorDef::read_only_functions()` resolved from the run's connector snapshot (the same resolution path `connector_call.rs` uses during execution).
- Produces: agent_task nodes participate in the cache; a connector call outside the read-only set blocks admission.

- [ ] **Step 1: Write failing tests** (same harness as Task 5, using the mock-agent pattern from the existing profile-run tests in the suite):

```rust
// 1. agent node, cache: auto, mock agent echoes a fixed output, no
//    workspace writes: run twice, second run has NodeCacheHit and no
//    AttemptStarted for the node.
// 2. change the profile SOUL (bundle digest changes): next run is a miss.
// 3. node bound to mock-tracker making a read_only call: admitted
//    (record verification.connector_calls == "read_only").
// 4. node making a non-read_only connector call: NodeCacheRejected with
//    reason containing "connector".
```

- [ ] **Step 2: Run to verify failure.** Expected: FAIL (agent nodes not cached yet).

- [ ] **Step 3: Implement.**
  - In the drive loop, for `NodeKind::AgentTask`, compute before `prepare`: the rendered prompt (call the same `render(prompt, &cfg.params, cfg.instruction.as_deref(), &state.outputs, &state.reviews, &hooks, &context)` that `execute_node` uses; hooks and context built the same way as in `scheduler/node.rs` lines 30-40, factored into a small shared helper so the two call sites cannot drift), the resolved profile bundle digest plus agent and model from the run manifest snapshot, and the sorted connector digests for the node's bindings from the manifest.
  - A prompt that references volatile run context simply produces a different rendered string per run and never hits; that is the intended safe behavior, no special-casing.
  - After `execute_node`, scan the returned `evs` for `EventPayload::ConnectorCall { connector, function, .. }` entries for this node; resolve each connector's `read_only_functions()` from the run's connector snapshot; `connector_calls_ok = all calls are read-only`, `had_connector_calls = any call seen`. Pass both to `admit`.
  - Remove the Task 5 kind guard so `prepare` accepts `AgentTask` and `Script`.

- [ ] **Step 4: Run tests.** `cargo test -p apb-engine --test main cache_ && cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Gates, then commit** (`engine: cache agent_task results with connector verification`).

---

### Task 7: Artifacts, capture and restore

**Files:**
- Modify: `crates/apb-engine/src/event.rs` (`NodeFinished` gains `#[serde(default)] pub artifacts: Vec<apb_core::cache::ArtifactRef>`)
- Modify: `crates/apb-engine/src/scheduler/cache.rs` (capture and restore helpers)
- Modify: `crates/apb-engine/src/scheduler.rs` (capture after execute, restore on hit, thread artifacts into `NodeFinished`)
- Test: extend `crates/apb-engine/tests/suite/cache_test.rs`

**Interfaces:**
- Consumes: `ArtifactRef`, `ArtifactScope`, `CacheStore::read_object`, `validate::build_globset`.
- Produces:
  - `cache::capture_artifacts(node: &Node, run_dir: &Path, workdir: &Path) -> Vec<(ArtifactRef, Vec<u8>)>`: for each glob in `outputs.files`, match files under `run_dir` (scope `Run`) and under `workdir` (scope `Workspace`); artifact `name` is the file name, `path` the scope-relative path, `digest` the content digest.
  - `cache::restore_artifacts(entry: &CachedEntry, store: &CacheStore, run_dir: &Path, workdir: &Path) -> Result<(), String>`: for each `ArtifactRef`, `read_object` (digest-verified) and `fsutil::atomic_write` to the scope root joined with `path`; any failure returns Err and the caller degrades the hit to a miss.
  - `NodeFinished.artifacts` populated on both the executed and the restored path.

- [ ] **Step 1: Write failing tests:**

```rust
// 1. script node writes findings.json into the workdir and declares
//    outputs: { files: ["findings.json"] }; cache: auto.
//    Run 1: admitted (NodeCacheStored) despite the workspace write,
//    because the declared output is excluded from the fingerprint
//    comparison; NodeFinished.artifacts has one entry with the right digest.
// 2. delete findings.json; run 2: NodeCacheHit and findings.json exists
//    again with identical content.
// 3. corrupt the artifact object in .apb/cache/objects: run 3 degrades to
//    NodeCacheMiss (restore failure is not a run error) and re-executes.
```

- [ ] **Step 2: Run to verify failure.** Expected: FAIL.
- [ ] **Step 3: Implement** per the interfaces above. On the hit path, restore before appending `NodeCacheHit`; if restore fails, fall through to the miss branch (append `NodeCacheMiss` and execute). On the store path, pass captured artifacts into `admit`. Path safety: reject (skip with a rejected-admission reason) any matched path that escapes its scope root after normalization.
- [ ] **Step 4: Run tests.** `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Gates, then commit** (`engine: capture and restore declared node artifacts`).

---

### Task 8: CLI flags and `apb cache` subcommands

**Files:**
- Modify: `crates/apb-cli/src/main.rs` (run flags; new `Cache` subcommand in `enum Command` at ~line 34)
- Test: `crates/apb-core` store tests already cover the mechanics; add a smoke test only if the CLI crate has an existing test harness (it is thin dispatch; manual verification below otherwise)

**Interfaces:**
- Consumes: `CacheStore::{status, inspect, prune, clear}`, `CacheRunMode`.
- Produces: `apb run <id> --no-cache | --refresh-cache` (mutually exclusive; map to `RunConfig.cache = Off | Refresh`), and:

```text
apb cache status
apb cache inspect <key>
apb cache prune [--older-than <dur>]
apb cache clear
```

- [ ] **Step 1: Add the flags** to the existing `Run` command variant (`#[arg(long, conflicts_with = "refresh_cache")] no_cache: bool`, `#[arg(long)] refresh_cache: bool`) and set `cfg.cache` accordingly where the run's `RunConfig` is built.

- [ ] **Step 2: Add the subcommand:**

```rust
    /// Inspect and manage the project-local node result cache
    Cache {
        #[command(subcommand)]
        cmd: CacheCmd,
    },

#[derive(Subcommand)]
enum CacheCmd {
    /// Record and object counts and total size
    Status,
    /// Print one record as JSON
    Inspect { key: String },
    /// Remove old records and unreferenced objects
    Prune {
        #[arg(long)]
        older_than: Option<String>, // parse_duration_str format
        #[arg(long)]
        max_size: Option<String>,   // "500k" | "100m" | "1g" | plain bytes
    },
    /// Remove the entire cache
    Clear,
}
```

Handlers resolve the project root the same way the neighbouring commands do, open `CacheStore`, print plain-text results (no exclamation marks). `prune --older-than` parses via `apb_core::duration::parse_duration_str`; `--max-size` parses with a local helper; both error politely on an invalid value:

```rust
fn parse_size_str(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let (num, mult) = match s.as_bytes().last()? {
        b'k' => (&s[..s.len() - 1], 1024u64),
        b'm' => (&s[..s.len() - 1], 1024 * 1024),
        b'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => return None,
    };
    num.trim().parse::<u64>().ok()?.checked_mul(mult)
}
```

- [ ] **Step 3: Manual verification** (in a scratch project):

```bash
cargo run -p apb-cli -- cache status      # empty store: 0 records
cargo run -p apb-cli -- cache clear
```

Plus one full run of the Task 5 fixture playbook via the CLI with and without `--no-cache`, confirming hit/miss events in the run log.

- [ ] **Step 4: Gates, then commit** (`cli: add run cache flags and apb cache subcommands`).

---

### Task 9: Web UI cached badge

**Files:**
- Create: `web/src/lib/runcache.ts` (pure logic)
- Create: `web/src/lib/runcache.test.ts`
- Modify: `web/src/pages/RunView.svelte` (compute cached set from `detail.events`, pass into the flow nodes)
- Modify: the node component used by RunView (`web/src/lib/components/PlaybookNode.svelte` or wherever the status chip is rendered; follow the existing badge styling) to show a `cached` badge when flagged

**Interfaces:**
- Consumes: the run events array RunView already loads (`detail.events`, JSON with an event tag field matching the serde representation of `EventPayload`).
- Produces: `export function cachedNodeIds(events: { [k: string]: unknown }[]): Set<string>` returning node ids that have a `node_cache_hit` event; a `cached` visual badge on those nodes.

- [ ] **Step 1: Write the failing vitest:**

```ts
import { describe, expect, it } from 'vitest'
import { cachedNodeIds } from './runcache'

describe('cachedNodeIds', () => {
  it('collects nodes with a cache hit', () => {
    const events = [
      { node_started: { node: 'a', attempt: 1 } },
      { node_cache_hit: { node: 'b', key: 'sha256:x', source_run: 'r0' } },
      { node_cache_miss: { node: 'c', key: 'sha256:y' } },
    ]
    expect(cachedNodeIds(events)).toEqual(new Set(['b']))
  })
  it('empty on no events', () => {
    expect(cachedNodeIds([])).toEqual(new Set())
  })
})
```

First check the actual serialized shape of one event in a real run's `events.jsonl` (tag style depends on the `EventPayload` serde attributes; adjust the fixture objects in the test to match reality, not the other way around).

- [ ] **Step 2: Run.** `cd web && bun run test runcache`. Expected: FAIL.

- [ ] **Step 3: Implement** `runcache.ts` against the verified event shape, wire the set through RunView into the node data (an optional `cached?: boolean` on the flow node data), and render a small neutral badge labelled `cached` next to the status chip, styled like the existing badges.

- [ ] **Step 4: Run.** `bun run test && bun run check && bun run build`. Expected: PASS.
- [ ] **Step 5: Gates (Rust untouched, still run the full gate set once), then commit** (`web: show cached badge for cache-hit nodes`).

---

## Final verification

- [ ] `cargo test --workspace` green.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `code-ranker check .` all clean.
- [ ] `cd web && bun run test && bun run check && bun run build` green.
- [ ] Manual end-to-end: a playbook with one cached script node and one cached agent node run twice via the CLI; second run shows two `node_cache_hit` events and the RunView badge.
- [ ] Do not push or open a PR without the owner's explicit approval.
