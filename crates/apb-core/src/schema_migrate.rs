//! Migrator from schema 1 to 2 (spec 2026-07-12, section 10): named and inline
//! executors are converted into profiles, the global config's
//! `default_executor` is materialized into `defaults.profile`. History is not
//! rewritten - a new schema-2 version of each affected playbook is created,
//! and `current` is moved to it.
//!
//! Legacy types here are self-contained (do not depend on `schema::Executor`),
//! so the migrator survives removal of executors from the main schema
//! (Task 9).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum MigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(String),
    #[error("cannot allocate a free version for playbook `{0}` (1000 candidates occupied)")]
    VersionExhausted(String),
    #[error(
        "profile `{0}` already exists with different content; resolve it manually before migrating"
    )]
    ProfileConflict(String),
    #[error("playbook `{0}` references executor `{1}` that is not defined locally or globally")]
    UnresolvedExecutor(String, String),
    #[error("playbook `{0}` has an unrecognized executor form at {1}")]
    UnrecognizedExecutor(String, String),
    #[error("cannot materialize global profile `{0}`: no config dir (set HOME/APB_CONFIG_DIR)")]
    NoConfigDir(String),
}

/// Legacy schema-1 executor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
struct LegacyExec {
    agent: String,
    model: String,
    #[serde(default)]
    fallbacks: Vec<LegacyFallback>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
struct LegacyFallback {
    agent: String,
    model: String,
}

/// Legacy surface of the global config: executors and default_executor are no
/// longer part of `GlobalConfig`, but the migrator must read them from the
/// raw config.yaml in order to materialize `default_executor` into
/// `defaults.profile`.
#[derive(Debug, Default, Deserialize)]
struct LegacyGlobal {
    #[serde(default)]
    default_executor: Option<String>,
    #[serde(default)]
    executors: BTreeMap<String, LegacyExec>,
}

fn read_legacy_global() -> Result<LegacyGlobal, MigError> {
    let Some(dir) = crate::config::config_dir() else {
        return Ok(LegacyGlobal::default());
    };
    // A missing file means empty; but a real I/O error (permissions, a
    // directory instead of a file) or malformed YAML must NOT be swallowed
    // into an empty default, otherwise the migration would silently fail to
    // materialize default_executor and would skip affected playbooks.
    let raw = match std::fs::read_to_string(dir.join("config.yaml")) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LegacyGlobal::default());
        }
        Err(e) => {
            return Err(MigError::Yaml(format!(
                "read legacy global config.yaml: {e}"
            )));
        }
    };
    serde_yaml_ng::from_str(&raw)
        .map_err(|e| MigError::Yaml(format!("legacy global config.yaml: {e}")))
}

/// A profile planned for creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedProfile {
    pub name: String,
    pub scope: String,
    pub from: String,
    pub empty_soul: bool,
}

/// A playbook update: new version + rewritten node references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPlaybookUpdate {
    pub id: String,
    pub from_version: String,
    pub new_version: String,
}

/// Full migration plan (dry-run by default).
#[derive(Debug, Clone, Default)]
pub struct MigrationPlan {
    pub new_profiles: Vec<PlannedProfile>,
    pub playbook_updates: Vec<PlannedPlaybookUpdate>,
    pub diagnostics: Vec<String>,
    // Internal: profile content (name -> profile.yaml) for apply.
    profile_yaml: BTreeMap<String, String>,
    // Internal: rewritten playbook.yaml by (id, from_version).
    rewritten: BTreeMap<(String, String), (String, String)>, // -> (new_version, yaml)
}

impl MigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.new_profiles.is_empty() && self.playbook_updates.is_empty()
    }
}

fn hash6(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    crate::content::hex_lower(&h.finalize())[..6].to_string()
}

/// Canonical content of an executor for deduplication. Structural JSON
/// serialization (rather than gluing via `:`/`|`) rules out key collisions
/// caused by delimiter characters inside agent/model.
fn exec_canonical(ex: &LegacyExec) -> String {
    serde_json::to_string(ex).unwrap_or_else(|_| format!("{}:{}", ex.agent, ex.model))
}

/// profile.yaml via the typed `ProfileDoc` + serde (correct escaping: names/
/// models like `null`, `*alias`, `a: b` do not break the YAML or change
/// type).
fn profile_yaml_for(name: &str, ex: &LegacyExec) -> String {
    use crate::profile::{ProfileDoc, ProfileExecutor, ProfileFallback, SoulRequirement};
    let doc = ProfileDoc {
        name: name.to_string(),
        description: "migrated from executor".into(),
        executor: ProfileExecutor {
            agent: ex.agent.clone(),
            model: ex.model.clone(),
            fallbacks: ex
                .fallbacks
                .iter()
                .map(|f| ProfileFallback {
                    agent: f.agent.clone(),
                    model: f.model.clone(),
                })
                .collect(),
        },
        soul: SoulRequirement::Any,
        skills: Vec::new(),
    };
    serde_yaml_ng::to_string(&doc).unwrap_or_default()
}

/// Converts an arbitrary legacy name (executor mapping key, `<id>-<node>`)
/// into the profile name format `[a-z0-9][a-z0-9-]*`, at most 64 chars. A
/// name that cannot be converted is replaced with a deterministic slug
/// derived from the content - otherwise `..`/`/`/special characters could
/// become a directory name outside the profiles root (path traversal).
fn safe_profile_name(hint: &str, content: &str) -> String {
    let mut s: String = hint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    let mut s = if s.len() > 64 {
        s[..64].trim_end_matches('-').to_string()
    } else {
        s
    };
    if crate::profile::validate_profile_name(&s).is_err() {
        s = format!("p-{}", hash6(content));
    }
    s
}

/// Name with a hash suffix on collision, guaranteed <=64 chars and valid:
/// base is truncated so that `-<hash6>` (7 chars) fits; if validation still
/// fails, a deterministic slug derived from the content is used instead.
fn suffixed_name(base: &str, content: &str) -> String {
    let suffix = format!("-{}", hash6(content)); // 7 characters
    let cap = 64usize.saturating_sub(suffix.len());
    let head = base
        .get(..cap.min(base.len()))
        .unwrap_or(base)
        .trim_end_matches('-');
    let candidate = format!("{head}{suffix}");
    if crate::profile::validate_profile_name(&candidate).is_ok() {
        candidate
    } else {
        format!("p-{}", hash6(content))
    }
}

/// Deduplication registry of profiles by executor content, aware of scope:
/// global executors are materialized as global profiles (in
/// `<config_dir>/profiles`), project-local ones (local named + inline) as
/// project profiles. Deduplication and name uniqueness are scoped within a
/// single scope (project/arch and global/arch are distinct profiles).
/// `profile_yaml` is keyed by `<scope>/<name>`.
#[derive(Default)]
struct Reg {
    by_content: BTreeMap<(String, String), String>, // (scope, content) -> name
    used: BTreeMap<(String, String), String>,       // (scope, name) -> content
}

impl Reg {
    /// Registers a profile in the given scope; returns its name.
    fn register(
        &mut self,
        hint: &str,
        ex: &LegacyExec,
        mp: &mut MigrationPlan,
        from: String,
        scope: &str,
    ) -> String {
        let content = exec_canonical(ex);
        if let Some(existing) = self.by_content.get(&(scope.to_string(), content.clone())) {
            return existing.clone();
        }
        let base = safe_profile_name(hint, &content);
        let existing = self.used.get(&(scope.to_string(), base.clone())).cloned();
        let name = match existing {
            Some(taken) if taken == content => base.clone(),
            // Collision: resolve a free hash-suffixed name, CHECKING used -
            // the suffixed candidate itself could already be taken by other
            // content, in which case the insert below would overwrite a
            // different profile.
            Some(_) => self.resolve_free_name(scope, &base, &content),
            None => base.clone(),
        };
        self.used
            .insert((scope.to_string(), name.clone()), content.clone());
        self.by_content
            .insert((scope.to_string(), content), name.clone());
        mp.profile_yaml
            .insert(format!("{scope}/{name}"), profile_yaml_for(&name, ex));
        mp.new_profiles.push(PlannedProfile {
            name: name.clone(),
            scope: scope.to_string(),
            from,
            empty_soul: true,
        });
        name
    }

    /// Resolves a free name on a `base` collision: takes a hash-suffixed
    /// name, and if that is also taken by DIFFERENT content, deterministically
    /// varies the suffix until the candidate is free (or already maps to the
    /// same content). This way a suffixed name can never overwrite a
    /// different profile. `used` is finite and each salt yields a new suffix,
    /// so the loop terminates.
    fn resolve_free_name(&self, scope: &str, base: &str, content: &str) -> String {
        let mut salt = 0u32;
        loop {
            let candidate = if salt == 0 {
                suffixed_name(base, content)
            } else {
                suffixed_name(base, &format!("{content}#{salt}"))
            };
            match self.used.get(&(scope.to_string(), candidate.clone())) {
                None => return candidate,
                Some(taken) if taken == content => return candidate,
                Some(_) => salt += 1,
            }
        }
    }
}

/// YAML form of a profile reference: global scope requires the explicit
/// `{ name, scope: global }` (otherwise `scope: auto` would resolve to a
/// project profile first); project scope uses the short string form
/// (`scope: auto` will find the project profile).
fn profile_ref_value(name: &str, scope: &str) -> serde_yaml_ng::Value {
    use serde_yaml_ng::Value;
    if scope == "global" {
        let mut m = serde_yaml_ng::Mapping::new();
        m.insert(Value::from("name"), Value::from(name));
        m.insert(Value::from("scope"), Value::from("global"));
        Value::Mapping(m)
    } else {
        Value::from(name)
    }
}

/// Builds a migration plan without writing to disk. Legacy global executors
/// are read from the raw config.yaml (they are no longer part of
/// `GlobalConfig`).
pub fn plan(root: &Path) -> Result<MigrationPlan, MigError> {
    let global = read_legacy_global()?;
    let mut mp = MigrationPlan::default();
    let mut reg = Reg::default();

    // Legacy executors/default_executor in the global config.yaml are NOT
    // touched: they are shared across all projects and global playbooks, so
    // mutating them as a side effect of migrating one project is not allowed
    // (it would break neighboring projects). `GlobalConfig` already ignores
    // them; here we only emit diagnostics.
    if !global.executors.is_empty() || global.default_executor.is_some() {
        mp.diagnostics.push(
            "global config.yaml still carries legacy executors/default_executor; they are ignored - remove them manually once no playbook needs them".into(),
        );
    }

    let apb_dir = root.join(".apb/playbooks");
    let Ok(ids) = std::fs::read_dir(&apb_dir) else {
        return Ok(mp);
    };
    let mut id_list: Vec<String> = ids
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    id_list.sort();

    for id in id_list {
        // id and version are used in a join - validate them as safe segments,
        // otherwise `current: ../../../x` or an id containing `..` could
        // steer reads/writes outside `.apb/playbooks` (path traversal during
        // `apb migrate`).
        if !crate::registry::is_safe_segment(&id) {
            mp.diagnostics
                .push(format!("skipped playbook dir `{id}`: unsafe name"));
            continue;
        }
        let current_path = apb_dir.join(&id).join("current");
        let Ok(cur_version) = std::fs::read_to_string(&current_path) else {
            continue;
        };
        let cur_version = cur_version.trim().to_string();
        if !crate::registry::is_safe_segment(&cur_version) {
            mp.diagnostics.push(format!(
                "skipped `{id}`: unsafe current version `{cur_version}`"
            ));
            continue;
        }
        let playbook_yaml_path = apb_dir.join(&id).join(&cur_version).join("playbook.yaml");
        let Ok(raw) = std::fs::read_to_string(&playbook_yaml_path) else {
            continue;
        };
        let mut doc: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&raw).map_err(|e| MigError::Yaml(e.to_string()))?;

        let has_executors = doc.get("executors").is_some()
            || doc
                .get("defaults")
                .and_then(|d| d.get("executor"))
                .is_some()
            || doc
                .get("supervisor")
                .and_then(|s| s.get("executor"))
                .is_some()
            || node_has_executor(&doc);
        let uses_default_executor = doc
            .get("defaults")
            .and_then(|d| d.get("executor"))
            .is_none()
            && doc.get("defaults").and_then(|d| d.get("profile")).is_none()
            && global.default_executor.is_some()
            && node_needs_default(&doc);
        if !has_executors && !uses_default_executor {
            continue;
        }

        let mut name_map: BTreeMap<String, (String, String)> = BTreeMap::new();
        if let Some(execs) = doc.get("executors").and_then(|v| v.as_mapping()).cloned() {
            for (k, v) in &execs {
                let ename = k.as_str().unwrap_or("exec").to_string();
                if let Ok(ex) = serde_yaml_ng::from_value::<LegacyExec>(v.clone()) {
                    let pname = reg.register(
                        &ename,
                        &ex,
                        &mut mp,
                        format!("{id}.executors.{ename}"),
                        "project",
                    );
                    name_map.insert(ename, (pname, "project".into()));
                }
            }
        }
        // References by name to GLOBAL executors (defaults/supervisor/node) -
        // materialize them as GLOBAL profiles (in config_dir), and rewrite the
        // reference to `{ name, scope: global }`: this way the migrated
        // reference resolves globally and does not depend on a same-named
        // project profile, and neighboring projects are unaffected.
        for rname in referenced_executor_names(&doc) {
            if !name_map.contains_key(&rname)
                && let Some(lex) = global.executors.get(&rname)
            {
                let pname = reg.register(
                    &rname,
                    lex,
                    &mut mp,
                    format!("global.executors.{rname}"),
                    "global",
                );
                name_map.insert(rname, (pname, "global".into()));
            }
        }

        // Materializing default_executor (global) -> global profile. If a
        // playbook relies on default_executor but it references a missing
        // `executors` entry, this is NOT a silent skip: otherwise we would
        // end up with a schema-2 playbook lacking its required binding. An
        // explicit error is raised BEFORE any writes.
        let mut default_profile: Option<(String, String)> = None;
        if uses_default_executor && let Some(defname) = &global.default_executor {
            let Some(lex) = global.executors.get(defname) else {
                return Err(MigError::UnresolvedExecutor(id.clone(), defname.clone()));
            };
            let pname = reg.register(
                defname,
                lex,
                &mut mp,
                "global.default_executor".into(),
                "global",
            );
            mp.diagnostics.push(format!("playbook `{id}` relied on default_executor `{defname}` -> defaults.profile: {{ name: {pname}, scope: global }}"));
            default_profile = Some((pname, "global".into()));
        }

        // New version is collision-safe (first free patch bump): we never
        // pick an already-existing directory, which could carry someone
        // else's version.
        let new_version = next_free_version(&apb_dir.join(&id), &cur_version)
            .ok_or_else(|| MigError::VersionExhausted(id.clone()))?;

        // Rewrite the document: schema 2, drop executors, convert references,
        // and MUST update `version` to match the new directory - otherwise
        // `Registry::load` would return `VersionMismatch` and the migrated
        // `current` would fail to load.
        rewrite_doc(&mut doc, &id, &name_map, default_profile, &mut reg, &mut mp)?;
        if let Some(map) = doc.as_mapping_mut() {
            map.insert(
                serde_yaml_ng::Value::from("version"),
                serde_yaml_ng::Value::from(new_version.clone()),
            );
        }
        let new_yaml = serde_yaml_ng::to_string(&doc).map_err(|e| MigError::Yaml(e.to_string()))?;

        mp.diagnostics.push(format!(
            "playbook `{id}` migrates to schema 2 (SOUL.md files created empty - fill role text)"
        ));
        mp.rewritten.insert(
            (id.clone(), cur_version.clone()),
            (new_version.clone(), new_yaml),
        );
        mp.playbook_updates.push(PlannedPlaybookUpdate {
            id: id.clone(),
            from_version: cur_version,
            new_version,
        });
    }

    Ok(mp)
}

/// Rewrites the playbook document into schema 2: schema=2, removes
/// `executors` and `defaults.executor`, sets `defaults.profile`, converts
/// node references (name -> profile; inline executor -> generated profile).
fn rewrite_doc(
    doc: &mut serde_yaml_ng::Value,
    id: &str,
    name_map: &BTreeMap<String, (String, String)>,
    default_profile: Option<(String, String)>,
    reg: &mut Reg,
    mp: &mut MigrationPlan,
) -> Result<(), MigError> {
    use serde_yaml_ng::Value;
    let Some(map) = doc.as_mapping_mut() else {
        return Ok(());
    };
    map.insert(Value::from("schema"), Value::from(2u32));
    map.remove(Value::from("executors"));

    if let Some(defaults) = map.get_mut("defaults").and_then(|d| d.as_mapping_mut()) {
        if let Some(exec) = defaults.remove(Value::from("executor")) {
            let (pname, pscope) = resolve_exec_ref(
                exec,
                id,
                name_map,
                reg,
                mp,
                &format!("{id}-default"),
                format!("{id}.defaults.executor"),
            )?;
            defaults.insert(Value::from("profile"), profile_ref_value(&pname, &pscope));
        } else if let Some((pname, pscope)) = &default_profile {
            defaults.insert(Value::from("profile"), profile_ref_value(pname, pscope));
        }
    } else if let Some((pname, pscope)) = &default_profile {
        let mut d = serde_yaml_ng::Mapping::new();
        d.insert(Value::from("profile"), profile_ref_value(pname, pscope));
        map.insert(Value::from("defaults"), Value::Mapping(d));
    }

    if let Some(sup) = map.get_mut("supervisor").and_then(|s| s.as_mapping_mut())
        && let Some(exec) = sup.remove(Value::from("executor"))
    {
        let (pname, pscope) = resolve_exec_ref(
            exec,
            id,
            name_map,
            reg,
            mp,
            &format!("{id}-supervisor"),
            format!("{id}.supervisor.executor"),
        )?;
        sup.insert(Value::from("profile"), profile_ref_value(&pname, &pscope));
    }

    if let Some(nodes) = map.get_mut("nodes").and_then(|n| n.as_sequence_mut()) {
        for node in nodes.iter_mut() {
            let Some(nm) = node.as_mapping_mut() else {
                continue;
            };
            if nm.get("type").and_then(|t| t.as_str()) != Some("agent_task") {
                continue;
            }
            if let Some(exec) = nm.remove(Value::from("executor")) {
                let node_id = nm
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("node")
                    .to_string();
                let (pname, pscope) = resolve_exec_ref(
                    exec,
                    id,
                    name_map,
                    reg,
                    mp,
                    &format!("{id}-{node_id}"),
                    format!("{id}.{node_id}.executor"),
                )?;
                nm.insert(Value::from("profile"), profile_ref_value(&pname, &pscope));
            }
        }
    }
    Ok(())
}

/// Resolves a single executor reference into `(profile name, scope)`: a
/// string -> `name_map` (either a local project profile or a materialized
/// global one), an inline object -> a new project profile. An unrecognized
/// form or unknown name is an error (not a silent skip that drops the
/// field).
fn resolve_exec_ref(
    exec: serde_yaml_ng::Value,
    id: &str,
    name_map: &BTreeMap<String, (String, String)>,
    reg: &mut Reg,
    mp: &mut MigrationPlan,
    inline_hint: &str,
    from: String,
) -> Result<(String, String), MigError> {
    if let Some(name) = exec.as_str() {
        name_map
            .get(name)
            .cloned()
            .ok_or_else(|| MigError::UnresolvedExecutor(id.to_string(), name.to_string()))
    } else if let Ok(ex) = serde_yaml_ng::from_value::<LegacyExec>(exec) {
        // Inline executor defined in a project playbook - a project profile.
        Ok((
            reg.register(inline_hint, &ex, mp, from, "project"),
            "project".into(),
        ))
    } else {
        Err(MigError::UnrecognizedExecutor(id.to_string(), from))
    }
}

/// Whether any node has an `executor` (either a string reference OR an
/// inline object) - both require migration (the schema loader would
/// otherwise reject them).
fn node_has_executor(doc: &serde_yaml_ng::Value) -> bool {
    doc.get("nodes")
        .and_then(|n| n.as_sequence())
        .is_some_and(|nodes| {
            nodes.iter().any(|n| {
                n.get("type").and_then(|t| t.as_str()) == Some("agent_task")
                    && n.get("executor").is_some()
            })
        })
}

/// Names referenced by executors as strings (defaults/supervisor/node) - used
/// to materialize the corresponding GLOBAL executors into profiles.
fn referenced_executor_names(doc: &serde_yaml_ng::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_str = |v: Option<&serde_yaml_ng::Value>| {
        if let Some(s) = v.and_then(|x| x.as_str()) {
            out.push(s.to_string());
        }
    };
    push_str(doc.get("defaults").and_then(|d| d.get("executor")));
    push_str(doc.get("supervisor").and_then(|s| s.get("executor")));
    if let Some(nodes) = doc.get("nodes").and_then(|n| n.as_sequence()) {
        for n in nodes {
            if let Some(s) = n.get("executor").and_then(|e| e.as_str()) {
                out.push(s.to_string());
            }
        }
    }
    out
}

fn node_needs_default(doc: &serde_yaml_ng::Value) -> bool {
    doc.get("nodes")
        .and_then(|n| n.as_sequence())
        .is_some_and(|nodes| {
            nodes.iter().any(|n| {
                n.get("type").and_then(|t| t.as_str()) == Some("agent_task")
                    && n.get("executor").is_none()
                    && n.get("profile").is_none()
            })
        })
}

fn bump_patch(v: &str) -> String {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() == 3
        && let Ok(patch) = parts[2].parse::<u64>()
    {
        return format!("{}.{}.{}", parts[0], parts[1], patch + 1);
    }
    format!("{v}-migrated")
}

/// First free (not present on disk) patch bump of the version - so the
/// migration never picks an already-existing directory with someone else's
/// content. `None` if all 1000 candidates are taken (we never silently
/// reuse a taken directory).
fn next_free_version(id_dir: &Path, cur_version: &str) -> Option<String> {
    let mut v = bump_patch(cur_version);
    for _ in 0..1000 {
        if !id_dir.join(&v).exists() {
            return Some(v);
        }
        v = bump_patch(&v);
    }
    None
}

/// Applies the plan: backup, profile creation (empty SOUL.md), new playbook
/// versions. Idempotent: re-running on an already-migrated tree is a no-op
/// (plan will return an empty plan).
pub fn apply(root: &Path, plan: &MigrationPlan, backup_ts: u64) -> Result<(), MigError> {
    if plan.is_empty() {
        return Ok(());
    }
    let backup = root.join(".apb").join(format!("backup-{backup_ts}"));
    std::fs::create_dir_all(&backup)?;
    for u in &plan.playbook_updates {
        let id_dir = root.join(".apb/playbooks").join(&u.id);
        let src_ver = id_dir.join(&u.from_version);
        if src_ver.is_dir() {
            copy_dir(
                &src_ver,
                &backup.join("playbooks").join(&u.id).join(&u.from_version),
            )?;
        }
        let cur = id_dir.join("current");
        if cur.is_file() {
            let bdir = backup.join("playbooks").join(&u.id);
            std::fs::create_dir_all(&bdir)?;
            std::fs::copy(&cur, bdir.join("current"))?;
        }
    }

    // Profiles. An existing profile is NEVER overwritten. But it is also not
    // silently reused: if the disk holds DIFFERENT content under this name,
    // it belongs to a different profile - the playbook reference would then
    // point to the wrong executor. Byte-for-byte match with the plan means an
    // idempotent repeat, which we skip; otherwise it is a conflict.
    for p in &plan.new_profiles {
        let dir = if p.scope == "global" {
            let cfg =
                crate::config::config_dir().ok_or_else(|| MigError::NoConfigDir(p.name.clone()))?;
            cfg.join("profiles").join(&p.name)
        } else {
            root.join(".apb/profiles").join(&p.name)
        };
        let planned = plan.profile_yaml.get(&format!("{}/{}", p.scope, p.name));
        if dir.join("profile.yaml").is_file() {
            let existing = std::fs::read_to_string(dir.join("profile.yaml")).unwrap_or_default();
            // Compare the ENTIRE expected bundle: profile.yaml AND SOUL.md.
            // The plan creates a profile with an EMPTY SOUL; if the disk
            // already holds a non-empty role, an "idempotent" repeat would
            // silently inherit it - that is a different profile, a conflict.
            // Only skip when both the yaml and the empty soul match.
            let existing_soul = std::fs::read_to_string(dir.join("SOUL.md")).unwrap_or_default();
            match planned {
                Some(planned) if planned == &existing && existing_soul.is_empty() => continue,
                _ => return Err(MigError::ProfileConflict(p.name.clone())),
            }
        }
        std::fs::create_dir_all(&dir)?;
        if let Some(yaml) = planned {
            crate::fsutil::atomic_write(&dir.join("profile.yaml"), yaml.as_bytes())?;
        }
        // Empty SOUL.md (a TODO placeholder text would go to the agent as
        // its role).
        crate::fsutil::atomic_write(&dir.join("SOUL.md"), b"")?;
    }

    // New playbook versions: copy the version's content, move current.
    for u in &plan.playbook_updates {
        let id_dir = root.join(".apb/playbooks").join(&u.id);
        let src_ver = id_dir.join(&u.from_version);
        let dst_ver = id_dir.join(&u.new_version);
        // Materialize the new version only if it does not exist yet (a
        // partial repeat of apply never overwrites it). But current is
        // ALWAYS moved - otherwise a repeated apply after a failure would
        // leave current on schema 1.
        if !dst_ver.exists() {
            copy_dir(&src_ver, &dst_ver)?;
            if let Some((_nv, yaml)) = plan.rewritten.get(&(u.id.clone(), u.from_version.clone())) {
                crate::fsutil::atomic_write(&dst_ver.join("playbook.yaml"), yaml.as_bytes())?;
            }
        }
        crate::fsutil::atomic_write(&id_dir.join("current"), u.new_version.as_bytes())?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixed_name_never_exceeds_64_and_is_valid() {
        // base is exactly 64 characters + hash suffix must not produce a
        // 71-character (invalid) name (review P2): base is truncated to fit
        // the suffix, and the result is valid.
        let base = "a".repeat(64);
        let name = suffixed_name(&base, "some-distinct-content");
        assert!(name.len() <= 64, "name too long: {} ({})", name, name.len());
        assert!(
            crate::profile::validate_profile_name(&name).is_ok(),
            "suffixed name must be a valid profile name: {name}"
        );
        assert!(name.contains('-'), "expected hash suffix separator: {name}");
    }

    #[test]
    fn two_distinct_execs_with_same_64_slug_get_distinct_valid_names() {
        let mut reg = Reg::default();
        let mut mp = MigrationPlan::default();
        let long = "a".repeat(70); // slug will be truncated to 64
        let ex1 = LegacyExec {
            agent: "claude".into(),
            model: "haiku".into(),
            fallbacks: vec![],
        };
        let ex2 = LegacyExec {
            agent: "codex".into(),
            model: "o1".into(),
            fallbacks: vec![],
        };
        let n1 = reg.register(&long, &ex1, &mut mp, "a".into(), "project");
        let n2 = reg.register(&long, &ex2, &mut mp, "b".into(), "project");
        assert_ne!(n1, n2, "distinct executors must get distinct names");
        for n in [&n1, &n2] {
            assert!(n.len() <= 64, "name too long: {n}");
            assert!(
                crate::profile::validate_profile_name(n).is_ok(),
                "invalid: {n}"
            );
        }
    }
}
