//! The secret values of a run, and the one redactor of everything that
//! leaves a run towards anyone who is not its executor.
//!
//! A run's sensitive values are its param values that
//! [`apb_core::run_params::sensitive`] flags, the same values of every
//! other run of its tree (its ancestors, followed through `parent_run` at
//! most [`MAX_ANCESTORS`] deep, and every run the topmost of them started,
//! directly or below: a child's answer or run outputs become its parent's
//! node output and from there a sibling's input) and the connector secrets
//! the workspace scrubber resolves. An input that names or holds such a
//! value and cannot be read (a named run without a readable config, a
//! journal that does not parse) never reads as "no secret": the run's
//! surfaces withhold its text ([`RunRedactor::withholds`]). The run's own
//! config is the one exception: when it cannot be read, its text is not
//! withheld for it, and the surfaces show the config error
//! ([`RunRedactor::config_error`]) instead, so one unreadable `run.yaml`
//! hides neither its own run nor any other run of a listing; storing
//! text still fails closed while the error stands. A param counts as declared secret when the run's manifest
//! records it (`secret_params`, written at start and only ever added to by
//! a current-run patch) or when the run's current snapshot declares it, so
//! a value classified secret once stays secret for the run and its
//! children. A run started by an older apb has no `secret_params` and uses
//! its snapshot alone.
//!
//! The journal, `run.yaml` and node outputs keep what the executor
//! produced: routing, conditions, resume and children read them. Redaction
//! happens on read, where data leaves apb: [`RunRedactor::redact_json`] for
//! structured answers (values only, paths stay usable) and
//! [`RunRedactor::redact_text`] for text that leaves the machine (the full
//! workspace scrubber plus the run's values).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use apb_core::run_params;
use apb_core::schema::Playbook;
use apb_core::scrub::Scrubber;
use apb_core::secret::Secret;

use crate::error::EngineError;

/// How many ancestors a run's redaction follows through `parent_run`.
pub const MAX_ANCESTORS: usize = 8;

/// The names of the params `playbook` declares secret.
pub fn declared_secret(playbook: &Playbook) -> Vec<String> {
    playbook
        .params
        .iter()
        .filter(|p| p.secret)
        .map(|p| p.name.clone())
        .collect()
}

/// Records in the run's manifest every param any of `playbooks` declares
/// secret, so a later snapshot that drops a declaration does not drop the
/// classification (a current-run patch calls this before it writes the new
/// snapshot).
pub(crate) fn keep_secret_declarations(
    run_dir: &Path,
    playbooks: &[&Playbook],
) -> Result<(), EngineError> {
    let names: BTreeSet<String> = playbooks.iter().flat_map(|p| declared_secret(p)).collect();
    if names.is_empty() {
        return Ok(());
    }
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::manifest::add_secret_params(run_dir, &run_id, names)
}

/// The params a run counts as declared secret: its manifest's
/// `secret_params` and its current snapshot's declarations.
pub fn secret_names(run_dir: &Path) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .map(|m| m.secret_params.into_iter().collect())
        .unwrap_or_default();
    names.extend(snapshot_declarations(run_dir));
    names
}

/// The params the run's snapshot (`playbook.yaml`) declares secret. Only
/// the `params` list is read, so a snapshot this binary would not load
/// whole (an older schema, a newer field) still gives its declarations.
fn snapshot_declarations(run_dir: &Path) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Snapshot {
        #[serde(default)]
        params: Vec<Declared>,
    }
    #[derive(serde::Deserialize)]
    struct Declared {
        name: String,
        #[serde(default)]
        secret: bool,
    }
    std::fs::read_to_string(run_dir.join("playbook.yaml"))
        .ok()
        .and_then(|yaml| yaml_serde::from_str::<Snapshot>(&yaml).ok())
        .map(|s| {
            s.params
                .into_iter()
                .filter(|p| p.secret)
                .map(|p| p.name)
                .collect()
        })
        .unwrap_or_default()
}

/// The sensitive param values of the run's tree a decision request loses,
/// failing closed: when the tree's secret-discovery inputs could not all be
/// read, every string of the request reads [`WITHHELD`] instead.
#[derive(Debug, Default)]
pub struct RunValues {
    scrubber: Scrubber,
    withhold: bool,
}

impl RunValues {
    /// The values of the run at `run_dir` in the workspace at `root`.
    pub fn of(run_dir: &Path, root: &Path) -> Self {
        let found = tree_secrets(run_dir);
        RunValues {
            scrubber: Scrubber::new(found.values, root),
            withhold: !found.complete,
        }
    }

    /// `text` without the run's values.
    pub fn scrub_values(&self, text: &str) -> String {
        if self.withhold {
            return WITHHELD.to_string();
        }
        self.scrubber.scrub_values(text)
    }

    /// Every string of `value` without the run's values, in place.
    pub fn scrub_values_json(&self, value: &mut serde_json::Value) {
        if self.withhold {
            withhold_strings(value);
            return;
        }
        self.scrubber.scrub_values_json(value);
    }
}

/// Every string of `value` (keys stay) read as [`WITHHELD`].
fn withhold_strings(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::String(s) => *s = WITHHELD.to_string(),
        Value::Array(items) => items.iter_mut().for_each(withhold_strings),
        Value::Object(map) => map.values_mut().for_each(withhold_strings),
        _ => {}
    }
}

/// What the redaction of one run found in its tree.
struct TreeSecrets {
    /// The sensitive param values of every run of the tree.
    values: Vec<Secret>,
    /// The params the run itself counts as declared secret.
    own: BTreeSet<String>,
    /// Whether every input naming or holding a value was read: a run the
    /// tree names whose config is missing or does not parse, a journal
    /// that cannot be read, or a child id that is not a safe segment make
    /// it `false`, and the run's surfaces then withhold.
    complete: bool,
    /// The error of the run's own config, when the file that is there
    /// does not parse. It does not withhold the run's own text: the
    /// surfaces show this error instead, so one unreadable `run.yaml`
    /// neither hides its own run behind `[secrets_unresolved]` nor any
    /// other run of a listing. A config that is missing though the run
    /// must have one still withholds. Storing text still fails closed
    /// while the error stands.
    own_config_error: Option<String>,
}

/// The run's tree as its redaction sees it: the ancestors followed through
/// `parent_run` (at most [`MAX_ANCESTORS`]), and every run the topmost of
/// them started, directly or through its children (the runs their
/// `child_run_started` events name, at most [`MAX_ANCESTORS`] levels). Data
/// moves along the whole tree: a child's answer or run outputs become its
/// parent's node output, and from there a sibling's params or instruction,
/// so every run of the tree hides the values of every other.
fn tree_secrets(run_dir: &Path) -> TreeSecrets {
    let own = secret_names(run_dir);
    let mut found = TreeSecrets {
        values: Vec::new(),
        own: own.clone(),
        complete: true,
        own_config_error: None,
    };
    let mut seen = BTreeSet::new();
    let mut dir = run_dir.to_path_buf();
    let mut top = dir.clone();
    for depth in 0..=MAX_ANCESTORS {
        seen.insert(dir.clone());
        let cfg = match named_config(&dir, depth > 0) {
            Ok(cfg) => cfg,
            Err(e) if depth == 0 && !e.missing => {
                found.own_config_error = Some(e.message);
                break;
            }
            Err(_) => {
                found.complete = false;
                break;
            }
        };
        let names = if depth == 0 {
            own.clone()
        } else {
            secret_names(&dir)
        };
        found.values.extend(sensitive_values(&cfg.params, &names));
        top = dir.clone();
        let Some(parent) = cfg.parent_run else {
            break;
        };
        let Some(runs) = dir.parent() else {
            break;
        };
        if !apb_core::registry::is_safe_segment(&parent) || depth == MAX_ANCESTORS {
            found.complete = false;
            break;
        }
        dir = runs.join(parent);
        if seen.contains(&dir) {
            break;
        }
    }
    descendants(&top, &mut found);
    found
}

/// A run config that could not be read, and whether the file is missing
/// though the run must have one.
struct ConfigReadError {
    message: String,
    missing: bool,
}

/// The run config of the run at `dir`, or the error when it cannot be
/// read: when the run has none though it must (a run `named` by another
/// run of the tree: a child's is written before its parent journals it;
/// a run [`config_expected`] says was prepared), or when the file that
/// is there does not parse.
fn named_config(dir: &Path, named: bool) -> Result<crate::run_config::RunConfig, ConfigReadError> {
    if !dir.join("run.yaml").is_file() && (named || config_expected(dir)) {
        return Err(ConfigReadError {
            message: "run.yaml is missing".to_string(),
            missing: true,
        });
    }
    crate::run_config::read_run_config(dir).map_err(|e| ConfigReadError {
        message: e.to_string(),
        missing: false,
    })
}

/// Whether the run at `dir` was prepared, so it wrote its `run.yaml`: it
/// has a manifest, or its journal's `run_started` names the definition it
/// executes (both are written after the config). The one legacy exception
/// is a run that shows neither: a start refused before its config was
/// written, or a run of an apb that journaled no definition. A journal
/// that cannot be read cannot tell, so the config is expected.
fn config_expected(dir: &Path) -> bool {
    if dir.join("manifest.yaml").is_file() {
        return true;
    }
    let bytes = match std::fs::read(dir.join("events.jsonl")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    let Ok(journal) = crate::event::parse_lossy_tail(&bytes) else {
        return true;
    };
    journal.events.iter().any(|e| {
        matches!(
            &e.payload,
            crate::event::EventPayload::RunStarted {
                definition: Some(_),
                ..
            }
        )
    })
}

/// The values of every run under `top` (see [`tree_secrets`]) into
/// `found`. A run met twice (the ancestors already counted, a cycle a
/// forged event names) is walked once.
fn descendants(top: &Path, found: &mut TreeSecrets) {
    let mut seen = BTreeSet::from([top.to_path_buf()]);
    let mut level = vec![top.to_path_buf()];
    for _ in 0..MAX_ANCESTORS {
        let mut next = Vec::new();
        for dir in &level {
            let Some(runs) = dir.parent() else {
                continue;
            };
            let Some(children) = child_runs(dir) else {
                found.complete = false;
                continue;
            };
            for child in children {
                let child_dir = runs.join(&child);
                if !seen.insert(child_dir.clone()) {
                    continue;
                }
                match named_config(&child_dir, true) {
                    Ok(cfg) => found
                        .values
                        .extend(sensitive_values(&cfg.params, &secret_names(&child_dir))),
                    Err(_) => found.complete = false,
                }
                next.push(child_dir);
            }
        }
        if next.is_empty() {
            return;
        }
        level = next;
    }
    // Runs deeper than the walk goes: a tree no sub-playbook depth allows.
    if level
        .iter()
        .any(|dir| child_runs(dir).is_none_or(|c| !c.is_empty()))
    {
        found.complete = false;
    }
}

/// The child run ids the journal at `run_dir` names, read with the same
/// tolerance as every run surface ([`crate::event::read_journal`]: a torn
/// last line is dropped): `None` when the journal cannot be read or names
/// a child by an id that is not a safe segment. A run without a journal
/// started no child.
fn child_runs(run_dir: &Path) -> Option<Vec<String>> {
    let bytes = match std::fs::read(run_dir.join("events.jsonl")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    const KIND: &[u8] = b"child_run_started";
    if !bytes.windows(KIND.len()).any(|w| w == KIND) {
        return Some(Vec::new());
    }
    let journal = crate::event::parse_lossy_tail(&bytes).ok()?;
    let mut ids = Vec::new();
    for e in journal.events {
        if let crate::event::EventPayload::ChildRunStarted { run_id, .. } = e.payload {
            if !apb_core::registry::is_safe_segment(&run_id) {
                return None;
            }
            ids.push(run_id);
        }
    }
    Some(ids)
}

fn sensitive_values(params: &BTreeMap<String, String>, declared: &BTreeSet<String>) -> Vec<Secret> {
    params
        .iter()
        .filter(|(name, value)| {
            run_params::sensitive(declared.contains(*name), name, value).is_some()
        })
        .map(|(_, value)| Secret::new(value.clone()))
        .collect()
}

/// The top-level keys of a run answer that deliver to the executor and are
/// never redacted: the host tasks a host driving the run executes (their
/// prompts carry the run's input as the agent's prompt would).
pub const DELIVERY_KEYS: &[&str] = &["pending_tasks"];

/// The redaction of many runs of one workspace: its connector secrets are
/// resolved once, each run's values per run.
pub struct WorkspaceRedaction {
    root: std::path::PathBuf,
    workspace: Vec<Secret>,
    /// Whether this read-only process could not resolve every referenced
    /// connector secret (see [`RunRedactor::withholds`]).
    withhold: bool,
}

impl WorkspaceRedaction {
    pub fn new(root: &Path) -> Self {
        let referenced = apb_core::connector::resolve::resolve_referenced_checked(root);
        WorkspaceRedaction {
            root: root.to_path_buf(),
            workspace: referenced.values,
            withhold: referenced.unresolved > 0 && crate::read_only::active(),
        }
    }

    /// The redactor of the run at `run_dir`.
    pub fn run_dir(&self, run_dir: &Path) -> RunRedactor {
        let mut r = RunRedactor::with_workspace(self.workspace.clone(), &self.root, run_dir);
        r.withhold |= self.withhold;
        r
    }

    /// The redactor of the run `run_id` of this workspace.
    pub fn run(&self, run_id: &str) -> RunRedactor {
        self.run_dir(&self.root.join(".apb/runs").join(run_id))
    }
}

/// The redactor of one run. `Debug` never prints a value.
#[derive(Debug)]
pub struct RunRedactor {
    /// The workspace's connector secrets plus the run's values.
    workspace: Scrubber,
    /// The run's own values only (see [`RunRedactor::run_values`]).
    run: Scrubber,
    /// The params the run counts as declared secret.
    declared: BTreeSet<String>,
    /// A read-only process that could not resolve a referenced connector
    /// secret (a command source it may not run, a variable missing from
    /// its environment) cannot tell whether the run's text holds it: it
    /// withholds the run's text instead of showing it. So does any process
    /// that could not read the tree's secret-discovery inputs.
    withhold: bool,
    /// The run's own config could not be read, with its error. Display
    /// redaction does not withhold for it (the surfaces show the error),
    /// but storing text still fails closed while it is present.
    config_error: Option<String>,
}

/// What a withheld string reads as ([`RunRedactor::withholds`]).
pub const WITHHELD: &str = "[secrets_unresolved]";

/// A redactor that withholds was asked for its scrubber: the secret values
/// of the run's tree could not all be read, so no text of the run may be
/// handed on ([`RunRedactor::scrubber`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretsUnresolved;

impl From<SecretsUnresolved> for EngineError {
    fn from(_: SecretsUnresolved) -> Self {
        EngineError::Conflict(
            "secrets_unresolved: the secret values of this run or a run of its tree could not all be read (a run config or journal does not parse), so its text is withheld; repair or remove the unreadable file and try again".into(),
        )
    }
}

/// Where a withheld answer stays readable: by its position in apb's own
/// answer shapes, never by a key name met anywhere. At the root of an answer
/// (a run's status, report or events answer, or one row of a run listing)
/// the fields below, and in the containers below the fields of each item.
/// Every other string, object or array reads [`WITHHELD`] as a whole, keys
/// included; numbers and booleans stay.
const ROOT_FIELDS: &[&str] = &[
    "run_id",
    "parent_run",
    "playbook",
    "version",
    "status",
    "run_status",
    "driver_alive",
    "stop",
    "execution",
];

/// The fields of each item of a root container, by container.
const CONTAINERS: &[(&str, &[&str])] = &[
    ("events", &["type", "node", "ts"]),
    ("next_actions", &["code", "run_id", "tool"]),
    ("children", &["run_id", "node_id", "status"]),
];

/// A kept value still has the shape of apb's own token (an id, a state, a
/// version): anything else is withheld even at a kept position.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
}

impl RunRedactor {
    /// The redactor of the run at `run_dir` in the workspace at `root`.
    pub fn for_run(root: &Path, run_dir: &Path) -> Self {
        WorkspaceRedaction::new(root).run_dir(run_dir)
    }

    /// The redactor of the run at `run_dir`, its workspace found from the
    /// directory (`<root>/.apb/runs/<id>`).
    pub fn for_run_dir(run_dir: &Path) -> Self {
        let root =
            crate::run_config::project_root_of(run_dir).unwrap_or_else(|| run_dir.to_path_buf());
        Self::for_run(&root, run_dir)
    }

    /// [`RunRedactor::for_run`] with the workspace's connector secrets
    /// already resolved, so a listing of many runs resolves them once.
    pub fn with_workspace(workspace: Vec<Secret>, root: &Path, run_dir: &Path) -> Self {
        let TreeSecrets {
            values,
            own,
            complete,
            own_config_error,
        } = tree_secrets(run_dir);
        let mut all = workspace;
        all.extend(values.iter().cloned());
        RunRedactor {
            workspace: Scrubber::new(all, root),
            run: Scrubber::new(values, root),
            declared: own,
            withhold: !complete,
            config_error: own_config_error,
        }
    }

    /// Whether this redactor withholds the run's text: a read-only process
    /// that could not resolve every connector secret the workspace
    /// references, or any process that could not read an input naming or
    /// holding a value of the run's tree (see the module docs). [`RunRedactor::redact_json`] and
    /// [`RunRedactor::redact_answer`] then keep only apb's own ids, states
    /// and numbers at known positions ([`ROOT_FIELDS`], [`CONTAINERS`]),
    /// every other value reads [`WITHHELD`], and every object answer (each
    /// row of a listing) carries `secrets_unresolved: true`.
    pub fn withholds(&self) -> bool {
        self.withhold
    }

    /// The error of the run's own `run.yaml`, when it cannot be read: a
    /// surface shows it for its own run instead of withheld text.
    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// Whether storing a text must fail closed: the redactor withholds,
    /// or the run's own config could not be read and a secret value of
    /// the run may be among its unread params.
    fn storing_refused(&self) -> bool {
        self.withhold || self.config_error.is_some()
    }

    /// Text that leaves the machine (an HTML report, a notice): the full
    /// workspace scrubber, the run's values included.
    pub fn redact_text(&self, text: &str) -> String {
        if self.withhold {
            return WITHHELD.to_string();
        }
        self.workspace.scrub(text)
    }

    /// Text inside a structured answer: the secret values replaced, paths
    /// and addresses kept.
    pub fn redact_str(&self, text: &str) -> String {
        if self.withhold {
            return WITHHELD.to_string();
        }
        self.workspace.scrub_values(text)
    }

    /// The text of a result snapshot as it may be stored: the secret
    /// values of the workspace and of the run's tree (ancestors included)
    /// and token-shaped strings replaced, paths and addresses kept
    /// ([`Scrubber::scrub_secrets`]). Refused when this redactor withholds:
    /// a value it could not read would be stored, so nothing is.
    pub fn secret_text(&self, text: &str) -> Result<String, SecretsUnresolved> {
        if self.storing_refused() {
            return Err(SecretsUnresolved);
        }
        Ok(self.workspace.scrub_secrets(text))
    }

    /// [`RunRedactor::secret_text`] over a JSON value, in place
    /// ([`Scrubber::scrub_secrets_json`]): it stays one document, and a
    /// number that spells a secret value becomes a string. Refused when
    /// this redactor withholds.
    pub fn secret_json(&self, value: &mut serde_json::Value) -> Result<(), SecretsUnresolved> {
        if self.storing_refused() {
            return Err(SecretsUnresolved);
        }
        self.workspace.scrub_secrets_json(value);
        Ok(())
    }

    /// Every string of a structured answer, in place: the secret values
    /// replaced, paths, addresses, numbers and the shape kept.
    pub fn redact_json(&self, value: &mut serde_json::Value) {
        if self.withhold {
            withhold_root(value);
            return;
        }
        self.workspace.scrub_values_json(value);
    }

    /// A run surface's answer, in place: [`RunRedactor::redact_json`] over
    /// everything but the top-level [`DELIVERY_KEYS`].
    pub fn redact_answer(&self, value: &mut serde_json::Value) {
        if self.withhold {
            withhold_root(value);
            return;
        }
        match value.as_object_mut() {
            Some(map) => map
                .iter_mut()
                .filter(|(k, _)| !DELIVERY_KEYS.contains(&k.as_str()))
                .for_each(|(_, v)| self.redact_json(v)),
            None => self.redact_json(value),
        }
    }

    /// The run's params as a surface shows them: every sensitive value
    /// replaced by [`run_params::REDACTED`], with the replaced names.
    pub fn params(
        &self,
        params: &BTreeMap<String, String>,
    ) -> (BTreeMap<String, String>, Vec<String>) {
        run_params::redacted(params, &self.declared)
    }

    /// Whether the run counts the param `name` declared secret (its
    /// manifest's `secret_params` and its current snapshot's declarations).
    pub fn declares_secret(&self, name: &str) -> bool {
        self.declared.contains(name)
    }

    /// Whether `text` holds a secret value of the run or the workspace.
    pub fn holds_value(&self, text: &str) -> bool {
        self.workspace.holds_value(text)
    }

    /// The full workspace scrubber with the run's values, for a reader
    /// that takes a [`Scrubber`] and hands its text on (a grading session,
    /// a known problem, a report). Refused when this redactor withholds
    /// ([`RunRedactor::withholds`]): the scrubber would miss the values
    /// discovery could not read, so the text is not handed on at all.
    pub fn scrubber(&self) -> Result<&Scrubber, SecretsUnresolved> {
        if self.storing_refused() {
            return Err(SecretsUnresolved);
        }
        Ok(&self.workspace)
    }

    /// [`RunRedactor::scrubber`], owned.
    pub fn into_scrubber(self) -> Result<Scrubber, SecretsUnresolved> {
        if self.storing_refused() {
            return Err(SecretsUnresolved);
        }
        Ok(self.workspace)
    }

    /// Every value discovery could read, possibly not all of them. Only for
    /// text written back into the run's own private files (its diagnostics,
    /// its grades), which every surface reads back through a [`RunRedactor`]
    /// that withholds, or handed to a path that withholds on its own (a
    /// decision request, [`RunValues`]). A text clipped after scrubbing
    /// takes [`RunRedactor::scrubber`] instead: a clipped value is no longer
    /// found once the tree reads again.
    pub fn known_values(&self) -> &Scrubber {
        &self.workspace
    }

    /// The run's own values (no connector secrets), which every decision
    /// request loses whatever `privacy.redact` says.
    pub fn run_values(&self) -> &Scrubber {
        &self.run
    }
}

/// `value` withheld as the root of an answer ([`ROOT_FIELDS`]): a root
/// array (a run listing) withholds each item as a root. Every object root
/// carries `secrets_unresolved: true`.
fn withhold_root(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(withhold_root),
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                match CONTAINERS.iter().find(|(name, _)| name == k) {
                    Some((_, fields)) => withhold_items(v, fields),
                    None => withhold_field(v, ROOT_FIELDS.contains(&k.as_str())),
                }
            }
            map.insert("secrets_unresolved".into(), Value::Bool(true));
        }
        other => withhold_field(other, false),
    }
}

/// Each object of the array `value`, its `fields` kept.
fn withhold_items(value: &mut serde_json::Value, fields: &[&str]) {
    let Some(items) = value.as_array_mut() else {
        return withhold_field(value, false);
    };
    for item in items {
        match item.as_object_mut() {
            Some(map) => map
                .iter_mut()
                .for_each(|(k, v)| withhold_field(v, fields.contains(&k.as_str()))),
            None => withhold_field(item, false),
        }
    }
}

/// One value at a position: a number, a boolean or null stays; a string
/// stays when the position is kept and it reads as a token; anything else
/// (an object or an array, keys and all) reads [`WITHHELD`].
fn withhold_field(value: &mut serde_json::Value, kept: bool) {
    use serde_json::Value;
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
        Value::String(s) if kept && is_token(s) => {}
        other => *other = Value::String(WITHHELD.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_run(
        runs: &Path,
        id: &str,
        params: &[(&str, &str)],
        parent: Option<&str>,
        snapshot: &str,
    ) {
        let dir = runs.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = crate::run_config::RunConfig {
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            parent_run: parent.map(str::to_string),
            ..Default::default()
        };
        crate::run_config::write_run_config(&dir, &cfg).unwrap();
        std::fs::write(dir.join("playbook.yaml"), snapshot).unwrap();
    }

    const SECRET_SNAPSHOT: &str = "schema: 2\nid: demo\nname: demo\nversion: 1.0.0\nparams:\n  - name: message\n    type: text\n    secret: true\nnodes:\n  - id: start\n    type: start\nedges: []\n";
    const PLAIN_SNAPSHOT: &str = "schema: 2\nid: demo\nname: demo\nversion: 1.0.0\nparams:\n  - name: message\n    type: text\nnodes:\n  - id: start\n    type: start\nedges: []\n";

    #[test]
    fn a_child_redacts_its_parents_declared_value_through_a_plain_param() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join(".apb/runs");
        write_run(
            &runs,
            "parent-1",
            &[("message", "violet hedgehog")],
            None,
            SECRET_SNAPSHOT,
        );
        write_run(
            &runs,
            "child-1",
            &[("note", "violet hedgehog")],
            Some("parent-1"),
            PLAIN_SNAPSHOT,
        );
        let r = RunRedactor::with_workspace(Vec::new(), root.path(), &runs.join("child-1"));
        let mut v = serde_json::json!({ "out": "said violet hedgehog", "path": "/x/y", "n": 3 });
        r.redact_json(&mut v);
        assert_eq!(v["out"], "said [redacted]");
        assert_eq!(v["path"], "/x/y");
        assert_eq!(v["n"], 3);
        // The child's own param is not declared secret: shown as is.
        let (shown, names) = r.params(&[("note".to_string(), "garden".to_string())].into());
        assert_eq!(shown["note"], "garden");
        assert!(names.is_empty());
    }

    /// The stored text of a result loses the parent's declared value and
    /// token shapes, keeps paths; a redactor that withholds stores nothing.
    #[test]
    fn secret_text_scrubs_the_tree_and_refuses_when_withholding() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join(".apb/runs");
        write_run(
            &runs,
            "parent-1",
            &[("message", "violet hedgehog")],
            None,
            SECRET_SNAPSHOT,
        );
        write_run(&runs, "child-1", &[], Some("parent-1"), PLAIN_SNAPSHOT);
        let mut r = RunRedactor::with_workspace(Vec::new(), root.path(), &runs.join("child-1"));
        let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
        assert_eq!(
            r.secret_text(&format!("violet hedgehog in /x/y.md, {token}"))
                .unwrap(),
            "[redacted] in /x/y.md, [redacted-token]"
        );
        r.withhold = true;
        assert_eq!(r.secret_text("plain"), Err(SecretsUnresolved));
    }

    // A declaration a patch dropped keeps redacting: the engine's
    // `run_secrets_test` drives that end to end.

    /// The run's own `run.yaml` does not parse: its text is not withheld
    /// for secrets (the error is shown by the surfaces instead), while a
    /// tree run's unreadable config still withholds.
    #[test]
    fn an_unreadable_own_config_does_not_withhold_its_own_text() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join(".apb/runs");
        let dir = runs.join("bad-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.yaml"), "mode: guarded\n").unwrap();
        std::fs::write(dir.join("playbook.yaml"), PLAIN_SNAPSHOT).unwrap();
        let r = RunRedactor::with_workspace(Vec::new(), root.path(), &dir);
        assert!(!r.withholds(), "own config error must not withhold");
        assert_eq!(r.redact_text("plain text"), "plain text");
        assert!(
            r.config_error().is_some(),
            "the config error is kept for the surface"
        );
    }

    /// A withholding redactor keeps apb's own ids and states at known
    /// positions only: a structural-looking key inside step output, a
    /// non-token value at a kept position, and every key of a withheld
    /// object read withheld.
    #[test]
    fn withholding_keeps_ids_and_states_at_known_positions_only() {
        let root = tempfile::tempdir().unwrap();
        let mut r = RunRedactor::with_workspace(Vec::new(), root.path(), root.path());
        r.withhold = true;
        let token = "ghp_0123456789abcdefABCDEF";
        let mut v = serde_json::json!({
            "run_id": "r-1",
            "run_status": "failed",
            "status": "said violet hedgehog",
            "outputs": { "echo": "said something", "id": token },
            "lists": [{ "id": token, "verdict": token }],
            "events": [
                { "type": "node_finished", "node": "echo", "output": "x", "seq": 3 },
                { "type": "host_task_requested", "outputs": { "id": token, "status": token } },
            ],
        });
        r.redact_answer(&mut v);
        assert_eq!(v["run_id"], "r-1");
        assert_eq!(v["run_status"], "failed");
        assert_eq!(v["status"], WITHHELD, "not a token");
        assert_eq!(v["outputs"], WITHHELD);
        assert_eq!(v["lists"], WITHHELD);
        assert_eq!(v["events"][0]["type"], "node_finished");
        assert_eq!(v["events"][0]["node"], "echo");
        assert_eq!(v["events"][0]["output"], WITHHELD);
        assert_eq!(v["events"][0]["seq"], 3);
        assert_eq!(v["events"][1]["outputs"], WITHHELD);
        assert!(!v.to_string().contains(token), "{v}");
        assert_eq!(v["secrets_unresolved"], true);
        // A listing: every row is a root, flagged.
        let mut rows = serde_json::json!([{ "run_id": "r-1", "instruction": token }]);
        r.redact_json(&mut rows);
        assert_eq!(rows[0]["run_id"], "r-1");
        assert_eq!(rows[0]["instruction"], WITHHELD);
        assert_eq!(rows[0]["secrets_unresolved"], true);
        assert_eq!(r.redact_text("anything"), WITHHELD);
    }
}
