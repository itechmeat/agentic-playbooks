# Agent Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement agent profiles per the spec `docs/superpowers/specs/2026-07-12-agent-profiles-design.md` (rev 3): a profile is the single executor binding for a node (agent+model+fallbacks+SOUL+skills), bundle trust, run snapshots, declarative agent invocations, schema 2 migration, then the advisory layer (detection, models table, subscriptions).

**Architecture:** Phase 1 (Tasks 1-9) - the core: profile types and digest procedures in wf-core, the scoped resolver, invocations and manifest in wf-engine, profile_* tools in wf-mcp, the schema migrator, removal of executors. Phase 2 (Tasks 10-14) - the additive advisory layer: agent detection, models table, subscriptions, howto/adoption. The transition is incremental: profiles first coexist with executors (the workspace compiles after every task), removing executors is a separate task after the migrator.

**Tech Stack:** Rust workspace (edition 2024): wf-core, wf-engine, wf-mcp (rmcp 2.2.0), wf-cli, wf-server. No new external dependencies (sha2, serde, tempfile are already in the tree).

## Global Constraints

- No em dashes and no exclamation marks in documentation or user-visible strings.
- Machine-facing fields are English; messages to the user are in the chat language (the tier-0 rule already applies).
- Commits happen only after owner approval; the Commit steps in tasks mark boundaries, but executing the step means preparation (git add is fine); the actual `git commit` happens after approval.
- All new MCP tools carry rmcp annotations `read_only_hint`/`destructive_hint`, following the pattern of the existing ones in `crates/wf-mcp/src/server.rs`.
- Tests are hermetic: global state via `WF_CONFIG_DIR` (see `wf_core::config::config_dir()`), directories via `tempfile`; env mutations under a shared `Mutex` (pattern: `crates/wf-mcp/tests/policy_test.rs`).
- Event backward compatibility: new `EventPayload` fields only with `#[serde(default)]`.
- State files: atomic write via temp+rename, 0600 (unix) through `wf_core::fsutil`.
- Digest formats: `profile_digest`/`skill_digest`/`bundle_digest` are `sha256:<hex>`; encoding is a domain tag plus length-prefixed fields (exact tags in Task 2).
- Profile names: `[a-z0-9][a-z0-9-]*`, at most 64 characters, `name` in profile.yaml == the directory name, case-fold collisions are forbidden.
- Secret values from auth files are never returned, logged, or cached.
- Embedding skill content into the prompt is forbidden at any delivery level.

---

## Phase 1: core

### Task 1: wf-core - profile types, QualifiedProfileRef, profile_digest

**Files:**
- Create: `crates/wf-core/src/profile.rs`
- Modify: `crates/wf-core/src/lib.rs` (add `pub mod profile;`)
- Test: unit tests inside `profile.rs`

**Interfaces:**
- Produces:
  - `ProfileDoc { name, description, executor: ProfileExecutor, soul: SoulRequirement, skills: Vec<SkillRef> }`, `ProfileExecutor { agent, model, fallbacks: Vec<ProfileFallback> }`, `ProfileFallback { agent, model }`;
  - `SoulRequirement { Any (default) | NativeRequired }` (serde: `any` | `native_required`, field `soul` in YAML);
  - `ProfileScope { Project, Global, Auto }` (serde snake_case);
  - `QualifiedProfileRef { name: String, scope: ProfileScope }` - serde accepts a string (shorthand for scope=auto) and an object; serializes as an object;
  - `SkillRef { name: String, scope: ProfileScope }` - the same dual serde form;
  - `validate_profile_name(&str) -> Result<(), String>`;
  - `ProfileDoc::from_yaml(&str) -> Result<Self, String>`; `profile_digest(profile_yaml: &str, soul_md: &str) -> String` (sha256 of `profile.yaml` + `\0` + `SOUL.md`).

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "name: architect\ndescription: d\nexecutor:\n  agent: claude\n  model: claude-opus-4-8\n  fallbacks:\n    - { agent: opencode, model: opencode/claude-opus-4-8 }\nskills:\n  - coding-standards\n  - { name: writing-plans, scope: global }\n";

    #[test]
    fn parses_profile_and_skill_ref_forms() {
        let p = ProfileDoc::from_yaml(P).unwrap();
        assert_eq!(p.executor.fallbacks.len(), 1);
        assert_eq!(p.skills[0], SkillRef { name: "coding-standards".into(), scope: ProfileScope::Auto });
        assert_eq!(p.skills[1].scope, ProfileScope::Global);
        assert_eq!(p.soul, SoulRequirement::Any); // default
    }

    #[test]
    fn profile_ref_accepts_string_and_object() {
        let s: QualifiedProfileRef = serde_yaml_ng::from_str("architect").unwrap();
        assert_eq!(s.scope, ProfileScope::Auto);
        let o: QualifiedProfileRef = serde_yaml_ng::from_str("{ name: reviewer, scope: project }").unwrap();
        assert_eq!(o.scope, ProfileScope::Project);
    }

    #[test]
    fn name_rules() {
        assert!(validate_profile_name("architect").is_ok());
        assert!(validate_profile_name("a1-b2").is_ok());
        assert!(validate_profile_name("Architect").is_err()); // uppercase
        assert!(validate_profile_name("-x").is_err());        // doesn't start with a letter/digit
        assert!(validate_profile_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn profile_digest_stable_and_covers_soul() {
        let d1 = profile_digest(P, "role text");
        assert!(d1.starts_with("sha256:"));
        assert_eq!(d1, profile_digest(P, "role text"));
        assert_ne!(d1, profile_digest(P, "other soul"));
        assert_ne!(d1, profile_digest(P, "")); // empty SOUL - different digest
    }
}
```

- [ ] **Step 2: Confirm they fail**

Run: `cargo test -p wf-core profile` - compile error (module doesn't exist).

- [ ] **Step 3: Implement**

The dual serde form - via a `#[serde(untagged)]` intermediate enum:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileScope { Project, Global, #[default] Auto }

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualifiedProfileRef { pub name: String, pub scope: ProfileScope }

#[derive(Deserialize)]
#[serde(untagged)]
enum RefForm { Short(String), Full { name: String, #[serde(default)] scope: ProfileScope } }

impl<'de> Deserialize<'de> for QualifiedProfileRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match RefForm::deserialize(d)? {
            RefForm::Short(name) => Self { name, scope: ProfileScope::Auto },
            RefForm::Full { name, scope } => Self { name, scope },
        })
    }
}
```

`SkillRef` - the same trick (no shared macro needed, YAGNI - two copy-pasted impls with a comment). `ProfileDoc`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDoc {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub executor: ProfileExecutor,
    #[serde(default)]
    pub soul: SoulRequirement,
    #[serde(default)]
    pub skills: Vec<SkillRef>,
}
```

`validate_profile_name`: skip regex - a manual byte check (`[a-z0-9]` first, then `[a-z0-9-]`, len<=64). `profile_digest` - sha2 following the pattern of `scope::digest_str`.

- [ ] **Step 4: Run** `cargo test -p wf-core profile` - PASS.

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-core
git commit -m "feat(core): profile document types, qualified refs, profile digest"
```

---

### Task 2: wf-core - content snapshot procedure and bundle_digest

Spec 3.5: copy-to-staging -> digest of the copy -> verify -> publish; content limits; spec 3.1: bundle_digest.

**Files:**
- Create: `crates/wf-core/src/content.rs`
- Modify: `crates/wf-core/src/lib.rs`
- Test: `crates/wf-core/tests/content_snapshot_test.rs`

**Interfaces:**
- Consumes: `fsutil::atomic_write` (exists).
- Produces:
  - `snapshot_tree(src: &Path, staging_dst: &Path, limits: &TreeLimits) -> Result<String, ContentError>` - copies the tree into staging and returns the digest OF THE COPY;
  - `tree_digest(root: &Path, limits: &TreeLimits) -> Result<String, ContentError>` - digest of an already-copied tree (reused by snapshot_tree);
  - `TreeLimits { max_total_bytes: u64, max_files: u32, max_depth: u32, max_file_bytes: u64 }` with `Default` (64 MiB / 512 / 16 / 8 MiB);
  - `ContentError::{Unsupported(path), Escape(path), TooLarge(what), Io(..)}` - maps to the codes `skill_unsupported` / `skill_escape` / `skill_too_large`;
  - `bundle_digest(profile_digest: &str, skills: &[(String, String)]) -> String` - skills as sorted pairs (qualified ref `scope/name`, digest).

- [ ] **Step 1: Failing tests**

```rust
use std::fs;
use std::os::unix::fs::symlink;
use wf_core::content::{snapshot_tree, bundle_digest, TreeLimits, ContentError};

#[test]
fn snapshot_copies_and_digest_is_of_the_copy() {
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join("SKILL.md"), "v1").unwrap();
    let dst = tempfile::tempdir().unwrap();
    let d1 = snapshot_tree(src.path(), &dst.path().join("s"), &TreeLimits::default()).unwrap();
    // Source drift AFTER the snapshot does not change the copy's digest.
    fs::write(src.path().join("SKILL.md"), "v2").unwrap();
    let d2 = wf_core::content::tree_digest(&dst.path().join("s"), &TreeLimits::default()).unwrap();
    assert_eq!(d1, d2);
}

#[test]
fn digest_covers_paths_and_bytes_with_domain_separation() {
    // {a: "xy"} and {ax: "y"} must produce different digests (length-prefixed).
    let t1 = tempfile::tempdir().unwrap();
    fs::write(t1.path().join("a"), "xy").unwrap();
    let t2 = tempfile::tempdir().unwrap();
    fs::write(t2.path().join("ax"), "y").unwrap();
    let l = TreeLimits::default();
    let s1 = tempfile::tempdir().unwrap();
    let s2 = tempfile::tempdir().unwrap();
    assert_ne!(
        snapshot_tree(t1.path(), &s1.path().join("s"), &l).unwrap(),
        snapshot_tree(t2.path(), &s2.path().join("s"), &l).unwrap()
    );
}

#[test]
fn symlink_escape_is_rejected_and_inner_symlink_ok() {
    let outer = tempfile::tempdir().unwrap();
    fs::write(outer.path().join("secret"), "s").unwrap();
    let skill = outer.path().join("skill");
    fs::create_dir(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "x").unwrap();
    symlink(outer.path().join("secret"), skill.join("leak")).unwrap();
    let st = tempfile::tempdir().unwrap();
    let err = snapshot_tree(&skill, &st.path().join("s"), &TreeLimits::default()).unwrap_err();
    assert!(matches!(err, ContentError::Escape(_)));

    // An internal link (to a file inside the root) is allowed and hashed as a link.
    fs::remove_file(skill.join("leak")).unwrap();
    symlink("SKILL.md", skill.join("alias")).unwrap();
    let st2 = tempfile::tempdir().unwrap();
    snapshot_tree(&skill, &st2.path().join("s"), &TreeLimits::default()).unwrap();
}

#[test]
fn limits_enforced() {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("big"), vec![0u8; 100]).unwrap();
    let l = TreeLimits { max_file_bytes: 10, ..Default::default() };
    let st = tempfile::tempdir().unwrap();
    assert!(matches!(
        snapshot_tree(t.path(), &st.path().join("s"), &l).unwrap_err(),
        ContentError::TooLarge(_)
    ));
}

#[test]
fn bundle_digest_is_order_independent() {
    let a = ("project/x".to_string(), "sha256:aa".to_string());
    let b = ("global/y".to_string(), "sha256:bb".to_string());
    assert_eq!(
        bundle_digest("sha256:pp", &[a.clone(), b.clone()]),
        bundle_digest("sha256:pp", &[b, a])
    );
}
```

- [ ] **Step 2: Confirm they fail** - `cargo test -p wf-core --test content_snapshot_test`.

- [ ] **Step 3: Implement**

Key implementation points:

- traversal: an iterative stack with a depth check; sort directory entry names;
- for each entry: `symlink_metadata`; a regular file -> limit check, copy, into the hash: tag `F` + lp(rel_path) + lp(bytes); a directory -> tag `D` + lp(rel_path); a symlink -> read the target, canonicalize from the parent, check `starts_with(canonical_root)` else `Escape`; into the hash: tag `L` + lp(rel_path) + lp(target_as_bytes); copy as a symlink; FIFO/socket/device -> `Unsupported`;
- `lp(x)` = u64 LE length + bytes; the tree's shared domain tag: `wf-tree-v1\0` as the first block; bundle: `wf-bundle-v1\0` + lp(profile_digest) + sorted lp(ref)+lp(digest);
- the digest is computed while writing the COPY (we read the source file once, writing to staging and to the hasher at the same time) - the TOCTOU window is closed by construction;
- `snapshot_tree` creates `staging_dst` itself and removes it entirely on any error.

- [ ] **Step 4: Run** - PASS. Also `cargo clippy -p wf-core` with no new warnings.

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-core
git commit -m "feat(core): TOCTOU-safe content snapshot, tree digest, bundle digest"
```

---

### Task 3: wf-core - profile resolver, skill scopes, bundle trust, claude bridge

**Files:**
- Create: `crates/wf-core/src/profile_store.rs`
- Create: `crates/wf-core/src/skills.rs`
- Modify: `crates/wf-core/src/trust.rs` (kind: profile_bundle)
- Modify: `crates/wf-core/src/lib.rs`
- Test: `crates/wf-core/tests/profile_resolve_test.rs`

**Interfaces:**
- Consumes: Task 1 (types), Task 2 (digest), `config::config_dir()`, `trust::TrustStore`.
- Produces:
  - `profile_store::project_dir(root) -> PathBuf` (`<root>/.wf/profiles`), `global_dir() -> Option<PathBuf>` (`<config_dir>/profiles`);
  - `WorkflowOrigin { Project, Global }` (workflow origin for the auto rules);
  - `resolve_profile(root: &Path, wref_origin: WorkflowOrigin, r: &QualifiedProfileRef) -> Result<LoadedProfile, ProfileError>` where `LoadedProfile { scope: ProfileScope /*actual*/, name, dir: PathBuf, doc: ProfileDoc, soul: String, profile_digest: String }` - rules from spec 3.3 (auto: project->global for project-origin; global only for global-origin; scope:project in a global workflow is an error);
  - `skills::skills_dir(root, scope) -> PathBuf` (project: `<root>/.agents/skills` honoring the project config's `skills_dir`, global: `~/.agents/skills`);
  - `skills::resolve_skill(root, profile_scope: ProfileScope /*actual*/, s: &SkillRef) -> Result<ResolvedSkillPath, ProfileError>` - a global profile sees only global skills; project: auto = project then global; returns `{ name, scope, canonical_path }`;
  - `skills::ensure_claude_bridge(skills_parent: &Path, claude_parent: &Path) -> Vec<String>` - idempotent symlinks `.claude/skills/<name>` -> canonical; returns diagnostics (a real directory with the same name - a warning, we don't touch it);
  - `trust::Kind { Workflow, ProfileBundle }` + TrustStore entries gain a `kind` (`#[serde(default)]` = Workflow for old ones);
  - `profile_store::compute_bundle(root, origin, r) -> Result<(LoadedProfile, Vec<(String /*scope/name*/, String /*digest*/)>, String /*bundle_digest*/), ProfileError>` - resolves skills and computes their digest over the live tree (for the gate; the run snapshot recomputes it over the copy).

- [ ] **Step 1: Failing tests** (env-lock as in policy_test.rs):

```rust
#[test]
fn auto_prefers_project_then_global_for_project_origin() { /* seed both, expect project */ }
#[test]
fn global_origin_never_sees_project_profiles() { /* seed only project, expect NotFound */ }
#[test]
fn explicit_scope_project_in_global_workflow_is_error() { /* ProfileError::ScopeForbidden */ }
#[test]
fn global_profile_cannot_use_project_skills() { /* skill resolution yields ScopeForbidden */ }
#[test]
fn project_skill_shadows_global_same_name() { /* actual scope = project */ }
#[test]
fn same_name_two_scopes_coexist_in_one_resolution() {
    // {reviewer, project} and {reviewer, global} resolve to DIFFERENT LoadedProfile values.
}
#[test]
fn name_dir_mismatch_and_casefold_collision_rejected() { /* Name(name!=dir), two directories Reviewer/reviewer */ }
#[test]
fn bundle_changes_when_skill_changes() {
    // compute_bundle twice: between calls edit SKILL.md -> bundle_digest differs,
    // profile_digest stays the same.
}
#[test]
fn claude_bridge_idempotent_and_respects_real_dirs() { /* two calls = one symlink; a real directory -> a diagnostic */ }
```

Seeds: the helper `seed_profile(dir, name, yaml, soul)` writes `profiles/<name>/{profile.yaml,SOUL.md}`; `seed_skill(parent, name)` writes `<parent>/<name>/SKILL.md`.

- [ ] **Step 2: They fail** - `cargo test -p wf-core --test profile_resolve_test`.

- [ ] **Step 3: Implement** `profile_store.rs` + `skills.rs`; the case-fold check - when listing the profiles directory, compare `to_lowercase()` of the names. In `trust.rs` - a `kind` field on the entry + `approve_kind(digest, name, kind, origin)`; the existing `approve` delegates with `Kind::Workflow`.

- [ ] **Step 4: Run** - PASS, plus `cargo test -p wf-core` in full (trust tests are not broken).

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-core
git commit -m "feat(core): profile resolver with scopes, skill resolution, bundle trust, claude bridge"
```

---

### Task 4: wf-engine - declarative invocations and SoulDelivery

Spec 6.2/6.3. Replace the hardcoded `-p/--model` form with data; ClaudeAdapter becomes a generic process adapter.

**Files:**
- Modify: `crates/wf-core/src/config.rs` (AgentDef.invocation)
- Create: `crates/wf-engine/src/invocation.rs`
- Modify: `crates/wf-engine/src/adapter.rs`
- Test: unit in `invocation.rs` + existing adapter tests must not break

**Interfaces:**
- Consumes: `SoulRequirement` (Task 1).
- Produces:
  - in `config.rs`: `InvocationDef { argv: Vec<String>, prompt_via: PromptVia (argv|stdin), soul: SoulDelivery (native|prefix), soul_flag: Option<String>, transport: Transport }`, the field `AgentDef.invocation: Option<InvocationDef>`; `InvocationDef::validate()` - load-time invariants: exactly one `{prompt}` slot (or stdin and zero slots), `{model}` at most once, placeholders only as whole argv elements, `soul: native` requires `soul_flag`;
  - `SoulDelivery { Native, Prefix }` (in config.rs, serde snake_case);
  - `invocation::builtin(agent_id: &str) -> Option<InvocationDef>` - the five from spec 6.2 (claude: `["-p","{prompt}","--model","{model}"]`, native, soul_flag `--append-system-prompt`; agy: same argv, prefix; codex: `["exec","{prompt}","-m","{model}"]`, prefix; opencode: `["run","{prompt}","-m","{model}"]`, prefix; pi - None);
  - `invocation::spec_for(agent_id, global: &GlobalConfig) -> Result<InvocationDef, EngineError>` - config overrides builtin;
  - `ResolvedInvocation { agent_id, model, spec: InvocationDef, soul_delivery: SoulDelivery, canonical_executable: PathBuf, executable_fingerprint: String /*"size:mtime_ms"*/ }` + `resolve_invocation(agent_id, model, global) -> Result<ResolvedInvocation, EngineError>` (path canonicalization as in detection: `std::fs::canonicalize`);
  - `AgentTask` (adapter.rs) gains the fields `soul: Option<&str>` and `spec: &InvocationDef`; `build_command(spec, prompt, model, soul) -> (Command setup, Option<StdinPayload>)`: placeholder substitution, SOUL native -> `soul_flag <soul>`, prefix -> `"<soul>\n\n---\n\n<prompt>"` (the prefix is NOT added if soul is empty);
  - `filter_chain(chain: Vec<ResolvedInvocation>, req: SoulRequirement, soul_empty: bool) -> Result<Vec<ResolvedInvocation>, EngineError>` - native_required removes prefix elements; an empty SOUL means no filtering; an empty chain -> `EngineError::Invalid`.

- [ ] **Step 1: Failing tests** (in `invocation.rs`):

```rust
#[test]
fn validate_rejects_two_prompt_slots_and_partial_placeholders() {
    // argv ["{prompt}","{prompt}"] -> Err; ["x{model}"] (not a whole element) -> Err;
    // prompt_via: stdin + "{prompt}" in argv -> Err; native without soul_flag -> Err.
}
#[test]
fn builtin_five_agents_present_and_valid() {
    for id in ["claude", "agy", "codex", "opencode"] {
        builtin(id).unwrap().validate().unwrap();
    }
    assert!(builtin("pi").is_none());
}
#[test]
fn build_command_substitutes_and_prefixes_soul() {
    // prefix: the prompt becomes "SOUL\n\n---\n\nprompt"; native: argv contains soul_flag and SOUL as a separate element.
    // empty SOUL with prefix: the prompt has no prefix.
}
#[test]
fn native_required_filters_prefix_but_not_when_soul_empty() { /* filter_chain */ }
```

- [ ] **Step 2: They fail.** `cargo test -p wf-engine invocation`.

- [ ] **Step 3: Implement.** In `adapter.rs`: `run_headless`/`run_acp` build the command via `build_command` instead of manual `.arg("-p")...`; `adapter_for(agent)` now returns an adapter with an `InvocationDef` (via `spec_for`); the `WF_AGENT_CMD` override is preserved (it only replaces the program, the spec remains builtin claude - the test path is unchanged). The REPORT_INSTRUCTION tail is kept as-is.

- [ ] **Step 4: Run** `cargo test -p wf-engine` - all existing adapter and mcp_cli tests are green (the claude invocation form is unchanged byte-for-byte).

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-core crates/wf-engine
git commit -m "feat(engine): declarative agent invocations, soul delivery, chain filtering"
```

---

### Task 5: schema 2 (coexistence) - profile refs in the workflow and the validator

Profiles are added ALONGSIDE executors; the engine switches over in Task 6, removal happens in Task 8.

**Files:**
- Modify: `crates/wf-core/src/schema.rs`
- Modify: `crates/wf-core/src/validate.rs`
- Test: `crates/wf-core/tests/validate_structure_test.rs` (extend), unit in schema

**Interfaces:**
- Consumes: `QualifiedProfileRef` (Task 1).
- Produces:
  - `Workflow.defaults.profile: Option<QualifiedProfileRef>`, `Supervisor.profile: Option<QualifiedProfileRef>`, `NodeKind::AgentTask` gains `profile_ref: Option<QualifiedProfileRef>` in the `profile` field (a TYPE CHANGE from the old decorative `profile: Option<String>` - the string form parses into a ref with scope auto, backward compatible on serialization);
  - `ValidationContext` gains a `profile_resolver: Option<&dyn Fn(&QualifiedProfileRef) -> Result<(), String>>` and `workflow_origin: WorkflowOrigin`;
  - V14 is redefined: if a node has a `profile`, it must resolve (via the callback); `scope: project` when `workflow_origin: Global` is a V14 error without invoking the callback;
  - New rule V18: a node without `profile` and without `executor`, when `defaults.profile`/`defaults.executor` is also absent, is an error (for now EITHER one is allowed).

- [ ] **Step 1: Tests** - in the validate tests: a workflow with `profile: ghost` and a resolver that refuses -> V14; `profile: {name: x, scope: project}` with origin Global -> V14; a node with a profile and a valid resolver -> ok without executors.

- [ ] **Step 2: They fail.**

- [ ] **Step 3: Implement.** The old V14 (`profile not found in .wf/profiles`) is replaced; `reg.profiles()` is no longer the source (the resolver from Task 3 is supplied by callers: doctor, versioning, scheduler - in this task update their `ValidationContext` calls, passing a closure over `resolve_profile`).

- [ ] **Step 4:** `cargo test -p wf-core` PASS; `cargo test --workspace` PASS (the schema was extended compatibly).

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-core crates/wf-engine crates/wf-server
git commit -m "feat(core): schema 2 profile refs coexisting with executors, validator rework"
```

---

### Task 6: wf-engine - resolving profiles into a run, manifest, snapshots, environment_drift

The core of the phase. Spec 3.4-3.6, 6.1, 6.4 (skill delivery), 6.5.

**Files:**
- Create: `crates/wf-engine/src/manifest.rs`
- Modify: `crates/wf-engine/src/scheduler.rs`
- Modify: `crates/wf-engine/src/executor.rs` (resolve on top of the profile)
- Modify: `crates/wf-engine/src/event.rs`
- Modify: `crates/wf-engine/src/run_config.rs` (profile snapshot alongside scripts)
- Test: `crates/wf-engine/tests/profile_run_test.rs`

**Interfaces:**
- Consumes: Tasks 1-5.
- Produces:
  - `ResolvedProfileSnapshot { r#ref: QualifiedProfileRef /*actual scope*/, chain: Vec<ResolvedInvocation>, soul: String, soul_requirement: SoulRequirement, skills: Vec<ResolvedSkillEntry { name, scope, digest }>, profile_digest: String, bundle_digest: String }`;
  - `manifest::write(run_dir, &[ResolvedProfileSnapshot]) -> Result<(), EngineError>` in `runs/<id>/manifest.yaml`; `manifest::read(run_dir)`; the manifest is immutable - a repeat write when the file already exists is an error;
  - snapshot layout: `runs/<id>/profiles/<scope>/<name>/{profile.yaml,SOUL.md,skills/<skill>/...}` via `content::snapshot_tree` (digest of the copy, checked against the verified value from the gate; a mismatch is `EngineError::Invalid("profile digest mismatch...")`);
  - `prepare_run_target`: if nodes have profile refs, it resolves all of them (nodes + supervisor + defaults), snapshots, writes the manifest; the executor path without profiles keeps working the old way (coexistence);
  - node executor selection: `node.profile -> defaults.profile -> node.executor -> defaults.executor` (profile takes priority);
  - resume: before continuing - `manifest::read`; for each chain element, verify `executable_fingerprint` against the current file; a mismatch -> `EngineError::Invalid("environment drift: ...")`; `RunOptions.allow_environment_drift: bool` (default false) proceeds anyway and writes an `EnvironmentDriftAccepted { agent_id, was, now }` event;
  - events: `AttemptStarted` gains `#[serde(default)] soul_delivery: Option<String>`, `FallbackTriggered` gains `#[serde(default)] profile: Option<String>` (`"<scope>/<name>"`); `RunProvenance` gains `#[serde(default)] profiles: Vec<ProfileProvenance { scope, name, bundle_digest }>`;
  - skill delivery: an isolated workdir (see `workdir.rs`) - materialized as copies from the snapshot into `<workdir>/.agents/skills/` and `<workdir>/.claude/skills/`; a shared workdir - a line is appended to the node prompt: `Relevant skills: <names> - use them via your skills mechanism` (a single line, no content); the level is recorded in the attempt event (`skills_mode: materialized|advisory`).

- [ ] **Step 1: Failing tests** (`profile_run_test.rs`, a stub agent as in `mcp_cli_test.rs` via `WF_AGENT_CMD`):

```rust
#[test]
fn run_with_profile_snapshots_and_writes_manifest() { /* profiles/<scope>/<name>/ exists, manifest.yaml parses, bundle_digest is non-empty */ }
#[test]
fn live_profile_edit_after_start_does_not_affect_resume() { /* human_review pause, edit SOUL, resume: the chain comes from the manifest */ }
#[test]
fn manifest_is_write_once() { }
#[test]
fn environment_drift_stops_resume_unless_allowed() { /* touch the stub binary (mtime) between start and resume */ }
#[test]
fn fallback_event_carries_profile_ref() { }
#[test]
fn advisory_skills_line_appended_in_shared_workdir() { /* the stub writes the received prompt to a file; we check for one line with names and no SKILL.md content */ }
#[test]
fn isolated_workdir_materializes_skill_copies() { /* isolation: full -> the workdir has .agents/skills/<name>/SKILL.md, and it is NOT a symlink */ }
```

- [ ] **Step 2: They fail.** `cargo test -p wf-engine --test profile_run_test`.

- [ ] **Step 3: Implement.** `executor::resolve` gains a profile branch: `resolve_node_executor(wf, node) -> Either<ResolvedProfileSnapshot-idx, legacy ResolvedExecutor>`; for an agent node with a profile, the scheduler takes the chain from the manifest (by index), SOUL and skills from the run snapshot. `native_required` filtering happens while building the snapshot (Task 4's `filter_chain`).

- [ ] **Step 4:** `cargo test -p wf-engine` in full PASS (the executor path is unaffected).

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-engine crates/wf-core
git commit -m "feat(engine): profile resolution into runs, execution manifest, env drift guard, skills delivery"
```

---

### Task 7: wf-mcp - profile_* tools, bundle gate, PlanPayload.profiles

**Files:**
- Create: `crates/wf-mcp/src/profile_tools.rs`
- Modify: `crates/wf-mcp/src/server.rs` (tool registration)
- Modify: `crates/wf-mcp/src/policy.rs` (bundle gate in check_run)
- Modify: `crates/wf-mcp/src/plan.rs` (profiles in PlanPayload)
- Test: `crates/wf-mcp/tests/profile_tools_test.rs`

**Interfaces:**
- Consumes: Tasks 1-3, 6; `fsutil::lock_dir`, `TrustStore`.
- Produces (all tools reject a foreign `workspace` - cross-workspace profile mutations are forbidden, spec 9.4):
  - `profile_list(root) -> {profiles: [{name, scope, description, trusted: bool, skills: [..], agents: [..], profile_digest, bundle_digest}]}` (read_only);
  - `profile_get(root, name, scope) -> {profile_yaml, soul_md, digests}` (read_only);
  - `profile_write(root, {name, scope, description, soul_md, skills, executor, soul?, expected_digest?}) -> {digest, warnings[], trust_write_failed?}`:
    1) `lock_dir(<profiles-parent>, "<name>.lock")`; 2) under the lock: create - the directory is absent, update - `expected_digest == profile_digest` of the current one, otherwise `{error: "conflict"}`; 3) validation (Task 1 name rules, Task 3 skills resolve, the agent is known: builtin invocation or agents config; an unknown model - a warning); 4) staging `profiles/.staging-<pid>-<n>/` + journal `profiles/.journal-<name>` (json: op, staging, target) -> swap rename -> delete the journal; 5) auto-approve the bundle (`approve_kind(bundle, name, ProfileBundle, LocallyApproved)`); an approve error leaves the profile in place, the response carries `trust_write_failed: true`;
    recovery: `profile_store::resolve_profile`, on seeing a journal before reading, completes the swap (or rolls back unfinished staging);
  - `profile_move(root, name, from_scope, to_scope) -> {copied: true, warnings[]}` - copy semantics (spec 4.2): a name conflict is a refusal; project->global with references to project skills is a refusal with alternatives; after the copy, the bundle is re-checked, the untrusted status is in the response;
  - `profile_delete(root, name, scope, force?)`: project - scans ALL versions of the project's workflows (parsing the YAML of each version, searching for refs accounting for auto-resolvability) - blocked with a list; global - global workflows + a best-effort registry, references require `force: true`;
  - policy: `check_run` additionally resolves the workflow's profiles and collects the bundle digest; an untrusted bundle -> `{policy: "untrusted_profile_requires_acknowledge", profiles: ["<scope>/<name>", ...]}`; `acknowledge_untrusted: true` covers it; verified bundles are passed to the engine for verification at snapshot time;
  - `PlanPayload.profiles: Vec<PlanProfile { scope, name, bundle_digest }>` (`#[serde(default)]`) - prepare fills it in, execute re-resolves and checks it; drift -> `{error: "plan_mismatch"}`.

- [ ] **Step 1: Failing tests**:

```rust
#[test]
fn write_create_then_conflict_on_double_create() { }
#[test]
fn write_update_requires_matching_expected_digest() { /* stale -> conflict */ }
#[test]
fn concurrent_writes_one_wins() {
    // Two threads, one expected_digest: exactly one Ok, the other conflict (under the lock).
}
#[test]
fn crash_recovery_journal_completes_swap() { /* manually leave a journal+staging, resolve_profile completes it */ }
#[test]
fn write_autoapproves_bundle_and_skill_edit_untrusts_next_run() {
    // after write check_run is ok; edit SKILL.md -> check_run: untrusted_profile_requires_acknowledge.
}
#[test]
fn delete_blocked_by_explicit_project_ref_in_old_version() { }
#[test]
fn cross_workspace_profile_write_rejected() { }
#[test]
fn plan_breaks_on_skill_drift_between_prepare_and_execute() { }
```

- [ ] **Step 2: They fail.**

- [ ] **Step 3: Implement.** Register the tools in server.rs with annotations (`profile_list`/`profile_get` read-only; write/move/delete destructive). Authorization boundaries (spec 9.4) are strings in the tool descriptions, not code.

- [ ] **Step 4:** `cargo test -p wf-mcp` PASS.

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-mcp crates/wf-core
git commit -m "feat(mcp): profile tools with CAS write, bundle trust gate, plan profile binding"
```

---

### Task 8: schema migrator and legacy surfaces

Spec 10. A separate module, NOT `migration.rs`.

**Files:**
- Create: `crates/wf-core/src/schema_migrate.rs`
- Modify: `crates/wf-engine/src/scheduler.rs` (legacy resume shim, removal of merge_global_config - in Task 9)
- Test: `crates/wf-core/tests/schema_migrate_test.rs`, extend `crates/wf-engine/tests/profile_run_test.rs`

**Interfaces:**
- Consumes: Tasks 1, 3, 5.
- Produces:
  - `schema_migrate::plan(root: &Path, global: &GlobalConfig) -> Result<MigrationPlan, MigError>` - read only; `MigrationPlan { new_profiles: Vec<PlannedProfile { name, scope, from: String /*description of the source*/, empty_soul: bool }>, workflow_updates: Vec<PlannedWorkflowUpdate { id, from_version, new_version, ref_rewrites: Vec<(node_id, profile_name)> }>, diagnostics: Vec<String> }`; `Display` - a human-readable plan;
  - `schema_migrate::apply(root, global, &MigrationPlan) -> Result<(), MigError>` - backup into `.wf/backup-<unix_ts>/`, creating profiles (SOUL.md empty), a NEW version of each affected workflow (a patch bump, old versions untouched), moving `current`; idempotency: `plan` on an already-migrated tree returns an empty plan;
  - dedup: the key is the executor's canonical YAML; identical content -> a single profile with the original name (or `<name>-<hash6>` on a conflict with different content); inline -> `<workflow-id>-<node-id>-<hash6>`;
  - default_executor: workflows without `defaults.executor` get a `defaults.profile` pointing at a profile derived from the global config's `default_executor` (materializing the implicit behavior of `merge_global_config`, scheduler.rs:567);
  - a conflict with the old string `profile` (decorative): the new version uses it as a ref if the directory exists, otherwise replaces it with the migrated executor profile with a warning in the plan;
  - legacy shim (engine): resuming a run whose `runs/<id>/workflow.yaml` contains executors and has no manifest.yaml - the snapshot's executors are converted into an ephemeral `ResolvedProfileSnapshot` (name `legacy-<executor>`, empty skills, empty SOUL, invocations via `resolve_invocation`), the manifest is written on first resume; marked `// TODO(remove after transition)`.

- [ ] **Step 1: Failing tests**:

```rust
#[test]
fn dedup_same_content_merges_different_content_suffixes() { }
#[test]
fn default_executor_materialized_into_defaults_profile() { }
#[test]
fn history_untouched_new_version_created_current_moved() { }
#[test]
fn plan_is_idempotent_after_apply() { }
#[test]
fn dry_run_writes_nothing() { /* plan + compare the tree's mtime */ }
// in profile_run_test.rs:
#[test]
fn legacy_run_resume_via_ephemeral_snapshot() { }
```

- [ ] **Step 2: They fail.**  - [ ] **Step 3: Implement.**  - [ ] **Step 4:** `cargo test -p wf-core -p wf-engine` PASS.

- [ ] **Step 5: Commit (after approval)**

```bash
git add crates/wf-core crates/wf-engine
git commit -m "feat(core): schema 2 migrator with dedup and history preservation, legacy resume shim"
```

---

### Task 9: removing executors, overrides v2, CLI, minimal web

Point of no return for phase 1: executors leave the schema; workflows containing them are read only in run snapshots (the legacy shim).

**Files:**
- Modify: `crates/wf-core/src/schema.rs` (remove Executor/ExecutorRef/executors/defaults.executor/supervisor.executor; a `schema: 1` file with executors -> a load error "run wf migrate")
- Modify: `crates/wf-core/src/overrides.rs` (v2)
- Modify: `crates/wf-core/src/config.rs` (remove executors/default_executor from GlobalConfig)
- Modify: `crates/wf-engine/src/scheduler.rs` (remove merge_global_config, the executor branch of resolve)
- Modify: `crates/wf-cli/src/main.rs` (`wf profile list|show|write|edit|move|delete`, `wf migrate [--apply]`)
- Modify: `crates/wf-server/src/lib.rs` (node form: a profile select; a profiles page - list+form)
- Test: fixes across the whole test tree + `crates/wf-cli/tests/profile_cli_test.rs`

**Interfaces:**
- Produces:
  - `RunOverrides v2`: `nodes: BTreeMap<String, NodeOverride { profile: Option<QualifiedProfileRef>, ephemeral_executor: Option<EphemeralExecutor { agent, model }> }>`; the `executors` section is removed; ephemeral is a run-local snapshot: role/skills are inherited from the node's profile, the chain is a single invocation, the manifest marks `ephemeral: true`;
  - loading a schema-1 file with executors outside a run snapshot: a `SchemaError` with the text `workflow uses schema 1 executors: run wf migrate`;
  - the legacy shim from Task 8 is the ONLY place where the old format is parsed (a local serde type in schema_migrate, not the public schema);
  - context compaction: `context_compact_model` remains; an exception comment next to it (spec 6.1);
  - the CLI `wf profile ...` - wrappers around the same logic as the MCP tools (we don't pull shared functions from wf-mcp: the logic lives in wf-core's `profile_store`/`skills`, MCP and CLI both call it); `wf profile edit` - `$EDITOR` + digest check before writing;
  - web: GET `/api/profiles` (list), POST `/api/profiles` (write), the agent node form uses a select.

- [ ] **Step 1:** take inventory of the failures: `cargo build --workspace 2>&1 | head -50` after removing the fields - a list of call sites.
- [ ] **Step 2:** rewrite the call sites (executor branches of resolution, tests with executors in YAML - switch to profiles; update test seeds with the `seed_profile` helper).
- [ ] **Step 3:** overrides v2 + tests (`override_selects_other_profile`, `ephemeral_executor_recorded_in_manifest`).
- [ ] **Step 4:** CLI + test (`profile_cli_test.rs`: write -> list -> show -> edit conflict on digest).
- [ ] **Step 5:** minimal web + smoke test (the existing api_test pattern).
- [ ] **Step 6:** `cargo test --workspace` PASS; `cargo clippy --workspace` - no new warnings in the affected files.

- [ ] **Step 7: Commit (after approval)**

```bash
git add -A
git commit -m "feat!: schema 2 - profiles replace executors everywhere, overrides v2, profile CLI and web"
```

---

## Phase 2: advisory layer

### Task 10: detection - probes, sanitation, cache

**Files:**
- Create: `crates/wf-core/src/detect.rs`
- Test: `crates/wf-core/tests/detect_test.rs`

**Interfaces:**
- Produces:
  - `Probe { id, bins: Vec<String>, category: AgentCategory (Vendor|Aggregator), version_args, models_source: ModelsSource, providers_source, auth_source }`; `builtin_probes() -> Vec<Probe>` (the five: claude, codex, agy, opencode, pi);
  - `detect(refresh: bool) -> Vec<AgentInfo>` where `AgentInfo { agent, installed: bool, canonical_path: Option<PathBuf>, version: Option<String>, category, models: Option<ModelsInventory { items: Vec<String>, authority: Authority (Full|Partial|Display|Static) }>, providers: Option<Vec<String>>, auth: Option<AuthHint { kind: oauth|api_key|none }>, notes: Vec<String> }`;
  - presence: scan PATH with canonicalization; PATH entries inside `std::env::current_dir()` are ignored (project-local protection);
  - spawning probes: absolute canonical path, argv without a shell, `env_clear()` + minimal PATH/HOME, a 10s timeout, a 256 KiB output limit (truncation with a note), kill by process group (pattern: `adapter::kill_process_tree`);
  - cache `<config_dir>/state/agents-detect.json`: the key is canonical_path + size + mtime_ms; TTL 24h; config sources - by their mtime; `refresh=true` ignores the cache;
  - model sources: opencode `models` (Full), agy `models` (Display), codex config.toml (Partial: `model`, `model_reasoning_effort`, `[model_providers.*]` sections - names only), claude - Static (the list comes from Task 11's data); auth: a `~/.claude.json` signal for oauth vs. env api-key (a best-effort hint), `~/.codex/auth.json` - the type, opencode's auth.json - provider names; secret values never leave the parser.

- [ ] **Step 1: Failing tests** - using stub scripts: a temporary bin directory on the test's PATH with executable sh scripts `fake-agent` (`echo 1.2.3` for --version, a list for models); tests: presence/absent; symlink canonicalization; project-local PATH is ignored; cache: a second call does not spawn (the stub writes a counter to a file); changing the binary's mtime triggers a respawn; timeout (a stub with `sleep 60`) - installed with a note, no hang; output limit.
- [ ] **Step 2: They fail.**  - [ ] **Step 3: Implement.**  - [ ] **Step 4:** PASS.
- [ ] **Step 5: Commit (after approval)** `git commit -m "feat(core): free agent detection with sanitized probes and metadata cache"`

---

### Task 11: models table, subscriptions, onboarding store

**Files:**
- Create: `assets/models.yaml`
- Create: `crates/wf-core/src/models_table.rs`
- Test: `crates/wf-core/tests/models_table_test.rs`

**Interfaces:**
- Produces:
  - `assets/models.yaml`: `as_of`, `models:` (20-30 entries per spec 8.2: vendor, cost_in_usd_mtok, cost_out_usd_mtok, reasoning, context_tokens, vision, stt, tts), `purposes:` (a starter set: coding, frontend, review, planning, brainstorming, writing, translation, cheap-glue, vision-tasks, research - extended via PRs), `claude_static_models:` (a list of CLI identifiers for the claude probe from Task 10);
  - `models_table::builtin() -> ModelsTable` (include_str! + parse, a panic is impossible - a CI test); `load_merged() -> ModelsTable` (an overlay of `<config_dir>/models.yaml` on top: models/purposes by name, `subscriptions` only from the overlay, `coverage` defaults to `unknown`);
  - `OnboardingState { Uninitialized, Configured, Declined }` + `onboarding::read()/write()` in `<config_dir>/state/onboarding.json`;
  - an integrity CI test: every `purposes[*].model` exists in `models`.

- [ ] **Step 1: Tests** (integrity, merge overlay: adding a model, overriding a price, subscriptions, default coverage; onboarding roundtrip + declined).
- [ ] **Step 2-4:** fail -> implement -> PASS.
- [ ] **Step 5: Commit (after approval)** `git commit -m "feat(core): curated models table with purposes, subscriptions overlay, onboarding state"`

---

### Task 12: MCP advisory tools + adoption

**Files:**
- Create: `crates/wf-mcp/src/advisory_tools.rs`
- Modify: `crates/wf-mcp/src/server.rs`, `crates/wf-mcp/src/instructions.rs` (tier 0 - three lines from spec 9.2)
- Modify: `crates/wf-mcp/src/catalog.rs` (profiles_hint outside the revision)
- Test: `crates/wf-mcp/tests/advisory_tools_test.rs`

**Interfaces:**
- Consumes: Tasks 3, 10, 11.
- Produces:
  - `agents_detect(refresh?)` (read_only) - output per 7.6;
  - `profile_howto()` (read_only) - the profile format + selection rules + models table + purposes + subscriptions + auth hints + detection + `subscriptions_uninitialized` when Uninitialized; the howto instructions include the rules from spec 8.4 (coverage semantics) and 9.4 (authorization boundaries);
  - `subscriptions_set({subscriptions: [...]} | {declined: true})` - writes the overlay section and the onboarding state;
  - `workflow_adopt_report(root, id?)` (read_only) - codes from spec 5.2; the environment part relies on detection: `model_not_available` only when `authority: Full`, otherwise `model_unverifiable`;
  - `workflow_catalog`: `profiles_hint: {count}` is added to the response AFTER computing the revision (not part of `compute_revision` - a test);
  - tier 0: three lines in TIER0 (profiles on nodes; call profile_list first; mention created profiles in the final message).

- [ ] **Step 1: Tests** (howto assembles the package and the uninitialized flag; subscriptions_set declined - a second howto without the flag; adopt report: profile_missing/skill_missing/untrusted/model_unverifiable on seeds; catalog revision does not depend on the number of profiles).
- [ ] **Step 2-4:** fail -> implement -> PASS.
- [ ] **Step 5: Commit (after approval)** `git commit -m "feat(mcp): advisory layer - detect, models table, subscriptions, adoption report"`

---

### Task 13: phase 2 CLI and the onboarding survey

**Files:**
- Modify: `crates/wf-cli/src/main.rs` (`wf detect [--refresh]`, `wf subscriptions`, `wf adopt`)
- Modify: `crates/wf-core/src/doctor.rs` (the agent section of doctor takes its data from detection: installed/version/authority instead of a manual check; spec 5.2)
- Test: `crates/wf-cli/tests/advisory_cli_test.rs`

**Interfaces:**
- Produces: `wf detect` - a table of agents; `wf adopt` - a report of codes; `wf subscriptions` - an interactive survey (prefilled from auth hints, written via the models_table overlay + onboarding state); onboarding trigger: `wf profile *`, `wf adopt`, `wf detect`, when state is Uninitialized and stdin is a TTY (`std::io::stdin().is_terminal()`), offer the survey; non-TTY skips without changing the state; `declined` is never offered again.

- [ ] **Step 1: Tests** (non-TTY path: commands don't hang and don't change the state; declined is never asked; adopt prints codes on a seed with a missing profile).
- [ ] **Step 2-4:** fail -> implement -> PASS.
- [ ] **Step 5: Commit (after approval)** `git commit -m "feat(cli): detect, adopt, subscriptions survey with onboarding states"`

---

### Task 14: docs, e2e, final check

**Files:**
- Modify: `docs/HOWTO-authoring.md` (profiles instead of executors), `docs/MCP.md` (new tools)
- Create: `docs/PROFILES.md` (a short one: format, scopes, trust, moving, migration)
- Test: `crates/wf-cli/tests/profile_e2e_test.rs`

**Steps:**
- [ ] **Step 1: e2e test** - via stdio MCP (pattern: `mcp_supervise_test.rs`): profile_write -> create a workflow with a profile reference -> workflow_run on a stub -> succeeded; then edit SKILL.md on disk -> a repeat run without acknowledge is rejected with `untrusted_profile_requires_acknowledge`.
- [ ] **Step 2: Docs.** HOWTO: the executors section is replaced with profiles (a YAML example from spec 3.1/3.2); MCP.md: profile_*, agents_detect, subscriptions_set, workflow_adopt_report, profile_howto, the profiles_hint contract; PROFILES.md - a single page.
- [ ] **Step 3: Full check.** `cargo test --workspace` PASS; `cargo clippy --workspace` - clean for the affected files; grep-scan docs and user-facing strings: no em dashes or exclamation marks (outside of code), no CJK.
- [ ] **Step 4: Commit (after approval)** `git commit -m "docs: profiles guide, MCP tool reference, e2e profile flow"`

---

## Order and dependencies

```
Task 1 -> 2 -> 3 ---------\
              \-> 4 -------+-> 6 -> 7 -> 8 -> 9   (phase 1)
              5 -----------/
9 -> 10 -> 11 -> 12 -> 13 -> 14                    (phase 2)
```

Task 5 depends only on Task 1; Tasks 4 and 5 can be done between 3 and 6 in either order. After Task 9 the workspace no longer contains executors; phase 2 is additive.
