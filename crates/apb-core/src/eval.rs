//! Playbook eval suites (0.24.0, first half of design C2): the case format,
//! its loader, the suite validation codes V80 to V83, the case and suite
//! digests, and the rule that keeps effects out of eval runs.
//!
//! A suite lives next to a playbook's versions, never inside one:
//!
//! ```text
//! .apb/playbooks/<id>/evals/
//!   suite.yaml        optional defaults (env, limits, repeat, tags, budget)
//!   <case>.yaml       one case per file, `id` = file stem
//!   scripts/          case check scripts
//!   fixtures/         fixture directories
//! ```
//!
//! The registry lists only `major.minor.patch` directories as versions, so
//! `evals/` is never taken for one, and editing a case never changes a
//! version's trust digest. The runner (`apb eval`) lives in the CLI; this
//! module is what every surface shares: parsing, validation and digests.
//!
//! Not in this release (they come with mock connectors): `connectors`,
//! `answers`, `stop_before`, `connector_calls` and `pushed`. A case that
//! uses one is refused by V80 with a message naming the field, so a suite
//! written for a later apb never runs half-understood.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::schema::{Effect, NodeKind, Playbook};
use crate::validate::{Issue, Severity};

/// The suite directory under a playbook directory.
pub const EVALS_DIR: &str = "evals";
/// The optional suite defaults file.
pub const SUITE_FILE: &str = "suite.yaml";
/// The only case schema this binary reads.
pub const CASE_SCHEMA: u32 = 1;
/// Default wall clock for one repetition when neither the case nor the
/// suite sets `limits.timeout`.
pub const DEFAULT_TIMEOUT_SECS: u64 = 45 * 60;
/// Default invocation budget (design Q5), lowered in `suite.yaml`.
pub const DEFAULT_MAX_USD_PER_INVOCATION: f64 = 10.0;
/// The branch a `fixture.change` overlay is committed on.
pub const DEFAULT_CHANGE_BRANCH: &str = "eval-change";

/// Case fields reserved for the second half of the eval suite. A case that
/// sets one is refused (V80) instead of running without it.
const LATER_FIELDS: [&str; 3] = ["connectors", "answers", "stop_before"];
const LATER_CHECKS: [&str; 2] = ["connector_calls", "pushed"];

/// One eval case (`evals/<id>.yaml`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub schema: u32,
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Free text shown in reports (an issue, a finding).
    #[serde(default)]
    pub source: Option<String>,
    /// Optional semver range the case applies to (`">=1.4.0, <2.0.0"`).
    #[serde(default)]
    pub versions: Option<String>,
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    pub fixture: Fixture,
    /// Overlay for the agents and scripts the run spawns and for the case
    /// scripts, never for apb itself; merged over the suite's. Keys that
    /// reconfigure apb, the shell, the loader or git are V80
    /// ([`env_key_problem`]). `{{eval.scratch}}` expands to the
    /// repetition's scratch dir.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub checks: CaseChecks,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub repeat: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Where the repetition's tree comes from. Exactly one of `git` and `dir`.
/// Either way the tree is a fresh repository under the scratch directory,
/// with its own `main` and a local bare repository as `origin`, so no push
/// reaches a real remote and "the diff against main" means the fixture's.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// A commit, tag or branch of this repository; its tree is exported
    /// (`git archive`) and committed as `main` of the fresh repository.
    #[serde(default)]
    pub git: Option<String>,
    /// A directory under `evals/fixtures/`, committed as `main`.
    #[serde(default)]
    pub dir: Option<String>,
    /// Optional directory under `evals/fixtures/` copied over the base and
    /// committed on `branch`, which stays checked out: a branch to review.
    #[serde(default)]
    pub change: Option<String>,
    /// The branch `change` is committed on (default `eval-change`).
    #[serde(default)]
    pub branch: Option<String>,
}

/// How the playbook's own goal criteria count.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalMode {
    /// Every script and marker criterion must pass (the default).
    #[default]
    Required,
    /// Recorded and shown, not part of the verdict.
    Report,
    Ignore,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CaseChecks {
    #[serde(default)]
    pub goal: GoalMode,
    #[serde(default)]
    pub run: Option<RunCheck>,
    #[serde(default)]
    pub route: Option<RouteCheck>,
    #[serde(default)]
    pub outputs: Vec<OutputCheck>,
    #[serde(default)]
    pub files: Vec<FileCheck>,
    #[serde(default)]
    pub events: Option<EventsCheck>,
    /// `complete`: no `deliverable_missing` or `output_fields_missing`.
    #[serde(default)]
    pub deliverables: Option<DeliverablesCheck>,
    /// Scripts under `evals/scripts/`, run with `sh` in the tree after the run.
    #[serde(default)]
    pub scripts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunCheck {
    /// Accepted terminal outcomes: `succeeded`, `failed`, `aborted`,
    /// `stopped`. Default: `[succeeded]`.
    pub outcome: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteCheck {
    #[serde(default)]
    pub visits: Vec<String>,
    #[serde(default)]
    pub in_order: bool,
    #[serde(default)]
    pub not_visits: Vec<String>,
    #[serde(default)]
    pub max_visits: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutputCheck {
    pub node: String,
    /// A named output field (`outputs.fields`) instead of the output text.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub equals: Option<String>,
    #[serde(default)]
    pub matches: Option<String>,
    #[serde(default)]
    pub not_matches: Option<String>,
    #[serde(default)]
    pub non_empty: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FileCheck {
    /// Relative to the tree root.
    pub path: String,
    #[serde(default)]
    pub exists: Option<bool>,
    #[serde(default)]
    pub matches: Option<String>,
    /// The file is byte-identical to the fixture's committed tip (or absent
    /// from both).
    #[serde(default)]
    pub unchanged_from_fixture: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventsCheck {
    #[serde(default)]
    pub absent: Vec<String>,
    #[serde(default)]
    pub max: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliverablesCheck {
    Complete,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Wall clock for one repetition (`45m`, `1h30m`, seconds).
    #[serde(default)]
    pub timeout: Option<String>,
    /// Per repetition, when the agent CLIs report cost.
    #[serde(default)]
    pub max_usd: Option<f64>,
    /// Per repetition, input plus output tokens from attempt usage.
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

impl Limits {
    /// `self` over `base`, field by field.
    pub fn over(&self, base: &Limits) -> Limits {
        Limits {
            timeout: self.timeout.clone().or_else(|| base.timeout.clone()),
            max_usd: self.max_usd.or(base.max_usd),
            max_tokens: self.max_tokens.or(base.max_tokens),
        }
    }

    /// The timeout in seconds, the default when unset.
    pub fn timeout_secs(&self) -> Option<u64> {
        match &self.timeout {
            None => Some(DEFAULT_TIMEOUT_SECS),
            Some(t) => crate::duration::parse_duration_str(t),
        }
    }
}

/// `suite.yaml`: defaults every case inherits.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    #[serde(default)]
    pub schema: Option<u32>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub repeat: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub budget: Budget,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    #[serde(default)]
    pub max_usd_per_invocation: Option<f64>,
}

/// A case as loaded: the parsed case, its file, and its raw bytes.
#[derive(Debug, Clone)]
pub struct LoadedCase {
    pub case: EvalCase,
    pub path: PathBuf,
    pub raw: String,
}

impl LoadedCase {
    /// Repetitions: the case's, else the suite's, else 1.
    pub fn repeat(&self, suite: &Suite) -> u32 {
        self.case.repeat.or(suite.repeat).unwrap_or(1).max(1)
    }

    /// The case env over the suite env.
    pub fn env(&self, suite: &Suite) -> BTreeMap<String, String> {
        let mut env = suite.env.clone();
        env.extend(self.case.env.clone());
        env
    }

    /// The case tags plus the suite tags.
    pub fn tags(&self, suite: &Suite) -> BTreeSet<String> {
        self.case.tags.iter().chain(&suite.tags).cloned().collect()
    }
}

/// A suite as loaded: its defaults, the cases that parsed, and one V80 issue
/// per file that did not.
#[derive(Debug, Clone, Default)]
pub struct LoadedSuite {
    pub dir: PathBuf,
    pub suite: Suite,
    pub cases: Vec<LoadedCase>,
    pub issues: Vec<Issue>,
}

fn issue(code: &'static str, severity: Severity, message: String) -> Issue {
    Issue {
        code,
        severity,
        message,
        node: None,
    }
}

/// `<playbook_dir>/evals`.
pub fn suite_dir(playbook_dir: &Path) -> PathBuf {
    playbook_dir.join(EVALS_DIR)
}

/// Whether the playbook has a suite at all.
pub fn has_suite(playbook_dir: &Path) -> bool {
    suite_dir(playbook_dir).is_dir()
}

/// The field of a later release a case sets, if any.
fn later_field(raw: &str) -> Option<String> {
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(raw).ok()?;
    let map = v.as_mapping()?;
    for f in LATER_FIELDS {
        if map.contains_key(f) {
            return Some(format!("`{f}`"));
        }
    }
    let checks = map.get("checks")?.as_mapping()?;
    LATER_CHECKS
        .iter()
        .find(|f| checks.contains_key(**f))
        .map(|f| format!("`checks.{f}`"))
}

/// Parses one case file's text. `stem` is the file stem the id must equal.
fn parse_case(raw: &str, stem: &str) -> Result<EvalCase, String> {
    if let Some(f) = later_field(raw) {
        return Err(format!(
            "{f} is not supported by this apb (it needs mock connectors, a later release)"
        ));
    }
    let case: EvalCase = serde_yaml_ng::from_str(raw).map_err(|e| e.to_string())?;
    if case.schema != CASE_SCHEMA {
        return Err(format!(
            "schema {} is not supported (expected {CASE_SCHEMA})",
            case.schema
        ));
    }
    if case.id != stem {
        return Err(format!(
            "id `{}` does not match the file name `{stem}.yaml`",
            case.id
        ));
    }
    if !valid_id(&case.id) {
        return Err(format!("id `{}` must match [a-z0-9-]+", case.id));
    }
    Ok(case)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Loads `<playbook_dir>/evals`: `suite.yaml` and every other `*.yaml` in
/// it (sorted by name). Unparsable files become V80 issues, never a panic
/// or a silent skip. A missing directory is an empty suite.
pub fn load_suite(playbook_dir: &Path) -> LoadedSuite {
    let dir = suite_dir(playbook_dir);
    let mut out = LoadedSuite {
        dir: dir.clone(),
        ..Default::default()
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "yaml" || e == "yml"))
        .collect();
    files.sort();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                out.issues.push(issue(
                    "V80",
                    Severity::Error,
                    format!("eval `{name}` cannot be read: {e}"),
                ));
                continue;
            }
        };
        if name == SUITE_FILE {
            match serde_yaml_ng::from_str::<Suite>(&raw) {
                Ok(s) => out.suite = s,
                Err(e) => out.issues.push(issue(
                    "V80",
                    Severity::Error,
                    format!("eval `{SUITE_FILE}` does not parse: {e}"),
                )),
            }
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match parse_case(&raw, &stem) {
            Ok(case) => out.cases.push(LoadedCase { case, path, raw }),
            Err(e) => out.issues.push(issue(
                "V80",
                Severity::Error,
                format!("eval case `{name}`: {e}"),
            )),
        }
    }
    out
}

/// A relative path that stays under `prefix/` of the suite directory.
fn contained(rel: &str, prefix: &str) -> bool {
    let p = Path::new(rel);
    let mut comps = p.components();
    matches!(comps.next(), Some(Component::Normal(first)) if first == prefix)
        && comps.clone().next().is_some()
        && comps.all(|c| matches!(c, Component::Normal(_)))
}

/// A relative path inside the tree (for `files` checks).
fn tree_relative(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Why the playbook cannot be evaluated at all: every effect an eval run
/// cannot neutralize in this release. Never run irreversible playbooks in
/// evals, and there is no case-level escape: the effective effects must not
/// contain `irreversible`, no node may declare `irreversible` or `secrets`
/// effects or look like a merge, push, deploy or publish step (the V73
/// evidence chain), no node may bind a connector (mock connectors come in a
/// later release, and a real connector is never called from an eval) and no
/// node may start a sub-playbook (whose effects the case cannot see).
pub fn refusal(playbook: &Playbook) -> Vec<String> {
    let mut why = Vec::new();
    if crate::effects::effective(playbook).contains(&Effect::Irreversible) {
        why.push("the playbook's effects contain `irreversible`".to_string());
    }
    for n in &playbook.nodes {
        if let Some(reason) = crate::validate::node_shipping_reason(n) {
            why.push(reason);
        }
        if !n.kind.connector_bindings().is_empty() {
            why.push(format!(
                "node `{}` binds a connector; eval runs never call real connectors and mocks are not supported yet",
                n.id
            ));
        }
        if matches!(n.kind, NodeKind::Playbook { .. }) {
            why.push(format!(
                "node `{}` starts a sub-playbook, which evals do not run",
                n.id
            ));
        }
    }
    why
}

/// Env names an overlay may not set: they reconfigure apb itself (`APB_*`,
/// the config-directory variables), the program search path, the dynamic
/// loader, git's idea of the repository and config, or the shell.
const ENV_EXACT: [&str; 22] = [
    "HOME",
    "PATH",
    "USERPROFILE",
    "APPDATA",
    "SHELL",
    "ENV",
    "BASH_ENV",
    "IFS",
    "CDPATH",
    "ZDOTDIR",
    "PROMPT_COMMAND",
    "PS4",
    "SHELLOPTS",
    "BASHOPTS",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_EXEC_PATH",
    "GIT_SSH_COMMAND",
];
const ENV_PREFIXES: [&str; 6] = ["APB_", "XDG_", "LD_", "DYLD_", "GIT_CONFIG", "BASH_FUNC_"];

/// Why an overlay may not set `name`, if it may not.
pub fn env_key_problem(name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['=', '\0']) || name.chars().any(char::is_whitespace) {
        return Some(format!("env `{name}` is not a variable name"));
    }
    let upper = name.to_ascii_uppercase();
    if ENV_EXACT.contains(&upper.as_str()) || ENV_PREFIXES.iter().any(|p| upper.starts_with(p)) {
        return Some(format!(
            "env `{name}` cannot be set by an eval suite (it reconfigures apb, the shell, the loader or git)"
        ));
    }
    None
}

/// Nodes of `playbook` a case names that do not exist.
fn unknown_nodes(case: &EvalCase, playbook: &Playbook) -> Vec<String> {
    let known: BTreeSet<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut named: Vec<&str> = Vec::new();
    if let Some(r) = &case.checks.route {
        named.extend(r.visits.iter().map(String::as_str));
        named.extend(r.not_visits.iter().map(String::as_str));
        named.extend(r.max_visits.keys().map(String::as_str));
    }
    named.extend(case.checks.outputs.iter().map(|o| o.node.as_str()));
    let mut out: Vec<String> = named
        .into_iter()
        .filter(|n| !known.contains(n))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

const OUTCOMES: [&str; 4] = ["succeeded", "failed", "aborted", "stopped"];

/// Whether the case file sets `checks.goal` itself (the field defaults to
/// `required`, which only a case that says so is held to).
fn goal_set_explicitly(raw: &str) -> bool {
    serde_yaml_ng::from_str::<serde_yaml_ng::Value>(raw)
        .ok()
        .and_then(|v| v.get("checks").and_then(|c| c.get("goal")).cloned())
        .is_some()
}

/// V80 problems of one parsed case against the version it targets.
/// `is_event_type` tells a journal event type this apb writes from a typo.
fn case_problems(
    lc: &LoadedCase,
    playbook: &Playbook,
    is_event_type: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let c = &lc.case;
    let mut p = Vec::new();
    match (&c.fixture.git, &c.fixture.dir) {
        (Some(_), Some(_)) | (None, None) => {
            p.push("the fixture needs exactly one of `git` and `dir`".to_string())
        }
        (Some(r), None) if r.trim().is_empty() || r.starts_with('-') => {
            p.push(format!("fixture.git `{r}` is not a ref"))
        }
        _ => {}
    }
    for (field, v) in [("dir", &c.fixture.dir), ("change", &c.fixture.change)] {
        if let Some(v) = v {
            if !contained(v, "fixtures") {
                p.push(format!("fixture.{field} `{v}` must stay under fixtures/"));
            } else if !lc.path.parent().is_some_and(|d| d.join(v).is_dir()) {
                p.push(format!("fixture.{field} `{v}` is not a directory"));
            }
        }
    }
    if let Some(b) = &c.fixture.branch
        && (b.is_empty() || b.starts_with('-') || b.contains(char::is_whitespace) || b == "main")
    {
        p.push(format!("fixture.branch `{b}` is not a usable branch name"));
    }
    for s in &c.checks.scripts {
        if !contained(s, "scripts") {
            p.push(format!("check script `{s}` must stay under scripts/"));
        } else if !lc.path.parent().is_some_and(|d| d.join(s).is_file()) {
            p.push(format!("check script `{s}` does not exist"));
        }
    }
    for f in &c.checks.files {
        if !tree_relative(&f.path) {
            p.push(format!("files check path `{}` must be relative", f.path));
        }
    }
    let regexes = c
        .checks
        .outputs
        .iter()
        .flat_map(|o| [&o.matches, &o.not_matches])
        .chain(c.checks.files.iter().map(|f| &f.matches))
        .flatten();
    for r in regexes {
        if let Err(e) = regex::Regex::new(r) {
            p.push(format!("regex `{r}` does not compile: {e}"));
        }
    }
    if let Some(run) = &c.checks.run {
        for o in &run.outcome {
            if !OUTCOMES.contains(&o.as_str()) {
                p.push(format!(
                    "run.outcome `{o}` is not one of {}",
                    OUTCOMES.join(", ")
                ));
            }
        }
    }
    if c.limits.timeout.is_some() && c.limits.timeout_secs().is_none() {
        p.push(format!(
            "limits.timeout `{}` is not a duration",
            c.limits.timeout.as_deref().unwrap_or_default()
        ));
    }
    if let Some(r) = &c.versions
        && VersionRange::parse(r).is_none()
    {
        p.push(format!(
            "versions `{r}` is not a range like `>=1.2.0, <2.0.0`"
        ));
    }
    if let Some(ev) = &c.checks.events {
        let mut bad: Vec<&str> = ev
            .absent
            .iter()
            .chain(ev.max.keys())
            .map(String::as_str)
            .filter(|t| !is_event_type(t))
            .collect();
        bad.sort();
        bad.dedup();
        for t in bad {
            p.push(format!("events: `{t}` is not an event type of this apb"));
        }
    }
    // The node, param and goal checks judge the case against the loaded
    // version; a case kept for other versions is not held to this one.
    let excluded = c
        .versions
        .as_deref()
        .and_then(VersionRange::parse)
        .is_some_and(|r| !r.contains(&playbook.version));
    if excluded {
        return p;
    }
    if c.checks.goal == GoalMode::Required && goal_set_explicitly(&lc.raw) {
        let criteria = playbook.goal.as_ref().map_or(0, |g| {
            g.criteria
                .iter()
                .filter(|c| !matches!(c.check, crate::schema::GoalCheck::Manual))
                .count()
        });
        if criteria == 0 {
            p.push(format!(
                "checks.goal is `required` but version {} has no script or marker goal criterion",
                playbook.version
            ));
        }
    }
    let unknown = unknown_nodes(c, playbook);
    if !unknown.is_empty() {
        p.push(format!(
            "checks name node(s) {} that version {} does not have",
            unknown
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", "),
            playbook.version
        ));
    }
    for k in c.env.keys() {
        if let Some(why) = env_key_problem(k) {
            p.push(why);
        }
    }
    let declared: BTreeSet<&str> = playbook.params.iter().map(|p| p.name.as_str()).collect();
    for k in c.params.keys() {
        if !declared.contains(k.as_str()) {
            p.push(format!("param `{k}` is not declared by the playbook"));
        }
    }
    p
}

/// Validates a playbook's suite against the loaded version (`apb validate`):
///
/// - **V80** (error): a case does not parse, its id is not its file stem, it
///   uses a field of a later release, a check names a node the version does
///   not have, a path leaves `evals/scripts/` or `evals/fixtures/`, a regex
///   does not compile; the suite holds a symlink or cannot be digested.
/// - **V81** (error): the playbook has a suite but cannot be evaluated at
///   all ([`refusal`]): irreversible effects, shipping steps, connectors or
///   sub-playbooks.
/// - **V82** (warning): the version has a `human_review` or interactive node;
///   an eval run cannot answer it in this release, so the run is stopped
///   when it waits and the case must accept `stopped`.
/// - **V83** (warning): no case applies to the version (`versions` ranges).
///
/// A playbook without `evals/` yields nothing.
pub fn validate_suite(playbook_dir: &Path, playbook: &Playbook) -> Vec<Issue> {
    validate_suite_with(playbook_dir, playbook, &|_| true)
}

/// [`validate_suite`] that also refuses event type names `is_event_type`
/// does not know (the engine's journal types; `apb_engine::eval::checks::
/// validate_suite` passes them).
pub fn validate_suite_with(
    playbook_dir: &Path,
    playbook: &Playbook,
    is_event_type: &dyn Fn(&str) -> bool,
) -> Vec<Issue> {
    if !has_suite(playbook_dir) {
        return Vec::new();
    }
    let loaded = load_suite(playbook_dir);
    let mut out = loaded.issues.clone();
    let dir = suite_dir(playbook_dir);
    for link in symlinks_under(&dir) {
        out.push(issue(
            "V80",
            Severity::Error,
            format!("`evals/{link}` is a symlink; an eval suite may not contain symlinks"),
        ));
    }
    for k in loaded.suite.env.keys() {
        if let Some(why) = env_key_problem(k) {
            out.push(issue(
                "V80",
                Severity::Error,
                format!("eval `{SUITE_FILE}`: {why}"),
            ));
        }
    }
    if let Err(e) = suite_digest(&dir) {
        out.push(issue(
            "V80",
            Severity::Error,
            format!("the suite cannot be digested: {e}"),
        ));
    }
    for lc in &loaded.cases {
        for problem in case_problems(lc, playbook, is_event_type) {
            out.push(issue(
                "V80",
                Severity::Error,
                format!("eval case `{}`: {problem}", lc.case.id),
            ));
        }
    }
    for why in refusal(playbook) {
        out.push(issue(
            "V81",
            Severity::Error,
            format!("the eval suite cannot run: {why}"),
        ));
    }
    let waits: Vec<&str> = playbook
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::HumanReview { .. }
                    | NodeKind::AgentTask {
                        interactive: true,
                        ..
                    }
            )
        })
        .map(|n| n.id.as_str())
        .collect();
    if !waits.is_empty() {
        out.push(issue(
            "V82",
            Severity::Warning,
            format!(
                "node(s) {} wait for a person; an eval run stops there (outcome `stopped`)",
                waits
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    if !loaded.cases.is_empty()
        && !loaded
            .cases
            .iter()
            .any(|lc| applies_to(&lc.case, &playbook.version))
    {
        out.push(issue(
            "V83",
            Severity::Warning,
            format!("no eval case applies to version {}", playbook.version),
        ));
    }
    out
}

/// Whether a case's `versions` range admits `version` (no range: always).
pub fn applies_to(case: &EvalCase, version: &str) -> bool {
    match &case.versions {
        None => true,
        Some(r) => VersionRange::parse(r).is_some_and(|range| range.contains(version)),
    }
}

/// A conjunction of `op major.minor.patch` comparators, comma-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRange(Vec<(Op, (u32, u32, u32))>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

impl VersionRange {
    pub fn parse(s: &str) -> Option<Self> {
        let mut out = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            let (op, rest) = [
                (">=", Op::Ge),
                ("<=", Op::Le),
                (">", Op::Gt),
                ("<", Op::Lt),
                ("=", Op::Eq),
            ]
            .into_iter()
            .find_map(|(p, op)| part.strip_prefix(p).map(|r| (op, r)))
            .unwrap_or((Op::Eq, part));
            out.push((op, crate::registry::parse_version(rest.trim())?));
        }
        (!out.is_empty()).then_some(VersionRange(out))
    }

    pub fn contains(&self, version: &str) -> bool {
        let Some(v) = crate::registry::parse_version(version) else {
            return false;
        };
        self.0.iter().all(|(op, b)| match op {
            Op::Ge => v >= *b,
            Op::Gt => v > *b,
            Op::Le => v <= *b,
            Op::Lt => v < *b,
            Op::Eq => v == *b,
        })
    }
}

/// The walk limits of an eval suite and its fixtures. Fixtures are real
/// repositories, far bigger than the skill bundles the default
/// [`crate::content::TreeLimits`] are sized for; a suite over these limits
/// cannot be digested and is refused, never approved under a shared value.
pub fn suite_limits() -> crate::content::TreeLimits {
    crate::content::TreeLimits {
        max_total_bytes: 1024 * 1024 * 1024,
        max_files: 50_000,
        max_depth: 64,
        max_file_bytes: 128 * 1024 * 1024,
    }
}

fn file_sha(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| crate::content::sha256_hex(&b))
        .map_err(|e| format!("`{}`: {e}", path.display()))
}

fn dir_digest(path: &Path) -> Result<String, String> {
    crate::content::tree_digest(path, &suite_limits())
        .map_err(|e| format!("`{}`: {e}", path.display()))
}

/// The case digest: the case file, every fixture directory and check script
/// it references, and the fixture ref resolved to a commit (`resolved_git`,
/// when the fixture is `git:`). A stored result is valid for a case only
/// while this digest is unchanged. A part that cannot be read or digested
/// is an error, never a placeholder shared with other cases.
pub fn case_digest(lc: &LoadedCase, resolved_git: Option<&str>) -> Result<String, String> {
    let dir = lc.path.parent().unwrap_or(Path::new("."));
    let mut parts: Vec<String> = vec![format!(
        "case:{}",
        crate::content::sha256_hex(lc.raw.as_bytes())
    )];
    for d in [&lc.case.fixture.dir, &lc.case.fixture.change]
        .into_iter()
        .flatten()
    {
        parts.push(format!("dir:{d}:{}", dir_digest(&dir.join(d))?));
    }
    for s in &lc.case.checks.scripts {
        parts.push(format!("script:{s}:{}", file_sha(&dir.join(s))?));
    }
    if let Some(c) = resolved_git {
        parts.push(format!("git:{c}"));
    }
    Ok(crate::content::sha256_hex(parts.join("\n").as_bytes()))
}

/// The suite digest: the whole `evals/` tree. It is what a person approves
/// before the suite's scripts run on this machine (`apb eval --yes`). A
/// suite that cannot be digested (a symlink, a special file, a name that is
/// not UTF-8, over [`suite_limits`]) is an error: the run is refused and
/// nothing is approved.
pub fn suite_digest(suite_dir: &Path) -> Result<String, String> {
    dir_digest(suite_dir)
}

/// Every symlink under `dir`, relative to it, sorted. An eval suite may not
/// carry one (V80): a link re-resolves differently once the suite is copied
/// into a scratch tree, and content reached through it is not what the
/// digest covers.
fn symlinks_under(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                out.push(
                    p.strip_prefix(base)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .into_owned(),
                );
            } else if ft.is_dir() {
                walk(base, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

// --- eval suite approvals ------------------------------------------------------

/// Approvals of suite digests, `<config-dir>/evals/approved.json`. Kept out
/// of the trust store on purpose: an older apb reading a trust store with a
/// kind it does not know would drop the whole store.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SuiteApprovals {
    #[serde(default)]
    pub approved: BTreeMap<String, SuiteApproval>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SuiteApproval {
    pub playbook: String,
    pub approved_at_ms: u128,
}

/// `<config-dir>/evals`.
pub fn evals_home() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("evals"))
}

fn approvals_path() -> Option<PathBuf> {
    evals_home().map(|d| d.join("approved.json"))
}

/// The value an older apb stored for every suite it could not digest. It
/// never names content, so it is dropped on load and can never match.
const LEGACY_UNDIGESTED: &str = "missing";

impl SuiteApprovals {
    fn read(path: &Path) -> Self {
        let mut a: SuiteApprovals = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        a.approved.remove(LEGACY_UNDIGESTED);
        a
    }

    pub fn load() -> Self {
        approvals_path().map(|p| Self::read(&p)).unwrap_or_default()
    }

    pub fn is_approved(&self, digest: &str) -> bool {
        digest != LEGACY_UNDIGESTED && self.approved.contains_key(digest)
    }

    /// Records the approval of `digest` for `playbook` and writes the file.
    /// The file is re-read under a lock, so an approval another invocation
    /// recorded in the meantime is kept.
    pub fn approve(&mut self, digest: &str, playbook: &str) -> std::io::Result<()> {
        if digest == LEGACY_UNDIGESTED {
            return Err(std::io::Error::other("not a suite digest"));
        }
        let path =
            approvals_path().ok_or_else(|| std::io::Error::other("no apb config directory"))?;
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("no apb config directory"))?;
        std::fs::create_dir_all(parent)?;
        let _lock = crate::fsutil::lock_dir(parent, "approved.lock")?;
        *self = Self::read(&path);
        self.approved.insert(
            digest.to_string(),
            SuiteApproval {
                playbook: playbook.to_string(),
                approved_at_ms: crate::clock::now_ms(),
            },
        );
        let body = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::fsutil::atomic_write(&path, body.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.2.0\nnodes:\n  - { id: s, type: start }\n  - { id: w, type: prompt, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: w }\n  - { from: w, to: f }\n";

    fn pb(yaml: &str) -> Playbook {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    fn suite_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let evals = dir.path().join(EVALS_DIR);
        std::fs::create_dir_all(evals.join("fixtures/base")).unwrap();
        std::fs::create_dir_all(evals.join("scripts")).unwrap();
        std::fs::write(evals.join("fixtures/base/a.txt"), "a").unwrap();
        std::fs::write(evals.join("scripts/ok.sh"), "exit 0").unwrap();
        for (name, body) in files {
            std::fs::write(evals.join(name), body).unwrap();
        }
        dir
    }

    const GOOD: &str = "schema: 1\nid: good\nfixture: { dir: fixtures/base }\nchecks:\n  run: { outcome: [succeeded] }\n  route: { visits: [w, f], in_order: true }\n  outputs: [{ node: w, matches: \"h.\" }]\n  files: [{ path: a.txt, unchanged_from_fixture: true }]\n  scripts: [scripts/ok.sh]\nlimits: { timeout: 5m }\nrepeat: 2\n";

    fn codes(issues: &[Issue]) -> Vec<(&'static str, String)> {
        issues.iter().map(|i| (i.code, i.message.clone())).collect()
    }

    #[test]
    fn a_valid_case_parses_and_validates_clean() {
        let dir = suite_with(&[("good.yaml", GOOD)]);
        let loaded = load_suite(dir.path());
        assert!(loaded.issues.is_empty(), "{:?}", codes(&loaded.issues));
        assert_eq!(loaded.cases.len(), 1);
        let c = &loaded.cases[0];
        assert_eq!(c.repeat(&loaded.suite), 2);
        assert_eq!(c.case.limits.timeout_secs(), Some(300));
        assert!(validate_suite(dir.path(), &pb(PB)).is_empty());
    }

    #[test]
    fn v80_names_every_problem_of_a_case() {
        let bad_id = "schema: 1\nid: other\nfixture: { dir: fixtures/base }\n";
        let later = "schema: 1\nid: later\nfixture: { dir: fixtures/base }\nconnectors: {}\n";
        let later_check =
            "schema: 1\nid: pushed\nfixture: { dir: fixtures/base }\nchecks: { pushed: [] }\n";
        let unknown_field = "schema: 1\nid: typo\nfixture: { dir: fixtures/base }\nrepat: 3\n";
        let escapes = "schema: 1\nid: escapes\nfixture: { dir: ../x, git: HEAD }\nchecks:\n  scripts: [scripts/../../evil.sh, other/x.sh]\n  route: { visits: [nope] }\n  files: [{ path: /etc/passwd }]\n  outputs: [{ node: w, matches: \"(\" }]\n  run: { outcome: [won] }\nlimits: { timeout: soon }\nversions: \"~1\"\nparams: { who: x }\n";
        let dir = suite_with(&[
            ("bad-id.yaml", bad_id),
            ("later.yaml", later),
            ("pushed.yaml", later_check),
            ("typo.yaml", unknown_field),
            ("escapes.yaml", escapes),
        ]);
        let issues = validate_suite(dir.path(), &pb(PB));
        let text: Vec<String> = issues
            .iter()
            .map(|i| format!("{} {}", i.code, i.message))
            .collect();
        let has = |needle: &str| {
            assert!(
                text.iter()
                    .any(|t| t.starts_with("V80") && t.contains(needle)),
                "missing `{needle}` in {text:#?}"
            )
        };
        has("does not match the file name `bad-id.yaml`");
        has("`connectors` is not supported");
        has("`checks.pushed` is not supported");
        has("unknown field `repat`");
        has("exactly one of `git` and `dir`");
        has("must stay under fixtures/");
        has("`scripts/../../evil.sh` must stay under scripts/");
        has("`other/x.sh` must stay under scripts/");
        has("`/etc/passwd` must be relative");
        has("does not compile");
        has("run.outcome `won`");
        has("limits.timeout `soon`");
        has("versions `~1`");
        has("node(s) `nope`");
        has("param `who` is not declared");
    }

    /// V2: one row per reason a playbook cannot be evaluated, each with its
    /// V81 message; a plain playbook has none.
    #[test]
    fn v81_refuses_every_effect_an_eval_cannot_neutralize() {
        let dir = suite_with(&[("good.yaml", GOOD)]);
        let w = "{ id: w, type: prompt, prompt: hi }";
        let rows: [(&str, String, &str); 5] = [
            (
                "irreversible",
                PB.replace("nodes:", "effects: [irreversible]\nnodes:"),
                "the playbook's effects contain `irreversible`",
            ),
            (
                "node secrets effect",
                PB.replace(w, "{ id: w, type: prompt, prompt: hi, effects: [secrets] }"),
                "`w`",
            ),
            (
                "shipping step",
                PB.replace(w, "{ id: push_branch, type: prompt, prompt: hi }")
                    .replace("to: w }", "to: push_branch }")
                    .replace("from: w,", "from: push_branch,"),
                "push_branch",
            ),
            (
                "connector",
                PB.replace(
                    w,
                    "{ id: w, type: agent_task, prompt: hi, profile: x, connectors: [jira] }",
                ),
                "node `w` binds a connector",
            ),
            (
                "sub-playbook",
                PB.replace(w, "{ id: w, type: playbook, playbook: child }"),
                "node `w` starts a sub-playbook",
            ),
        ];
        for (what, yaml, needle) in rows {
            let got = codes(&validate_suite(dir.path(), &pb(&yaml)));
            assert!(
                got.iter().any(|(c, m)| *c == "V81"
                    && m.starts_with("the eval suite cannot run: ")
                    && m.contains(needle)),
                "{what}: {got:?}"
            );
        }
        assert!(refusal(&pb(PB)).is_empty());
    }

    /// V82 names a gate and an interactive agent step (a warning); V83 says
    /// which version no case applies to.
    #[test]
    fn v82_warns_on_a_gate_and_v83_on_no_applicable_case() {
        let pinned = GOOD.replace("repeat: 2", "versions: \">=2.0.0\"");
        let dir = suite_with(&[("good.yaml", &pinned)]);
        let gated = PB.replace(
            "{ id: w, type: prompt, prompt: hi }",
            "{ id: w, type: prompt, prompt: hi }\n  - { id: g, type: human_review }\n  - { id: i, type: agent_task, prompt: ask, profile: x, interactive: true }",
        );
        let issues = validate_suite(dir.path(), &pb(&gated));
        let v82 = issues.iter().find(|i| i.code == "V82").expect("V82");
        assert_eq!(v82.severity, Severity::Warning);
        assert_eq!(
            v82.message,
            "node(s) `g`, `i` wait for a person; an eval run stops there (outcome `stopped`)"
        );
        let v83 = issues.iter().find(|i| i.code == "V83").expect("V83");
        assert_eq!(v83.severity, Severity::Warning);
        assert_eq!(v83.message, "no eval case applies to version 1.2.0");
    }

    /// A case kept for other versions is not judged by the loaded one: its
    /// nodes and params may be gone there (OCR 10).
    #[test]
    fn a_case_for_other_versions_is_not_held_to_the_loaded_version() {
        let old = "schema: 1\nid: old\nversions: \"<1.0.0\"\nfixture: { dir: fixtures/base }\nparams: { gone: x }\nchecks:\n  route: { visits: [gone] }\n";
        let dir = suite_with(&[("old.yaml", old)]);
        let got = codes(&validate_suite(dir.path(), &pb(PB)));
        assert!(got.iter().all(|(c, _)| *c != "V80"), "{got:?}");
    }

    /// K1: an explicit `goal: required` needs a script or marker criterion
    /// to check; the default does not.
    #[test]
    fn v80_refuses_a_required_goal_the_playbook_cannot_check() {
        let explicit = "schema: 1\nid: explicit\nfixture: { dir: fixtures/base }\nchecks: { goal: required }\n";
        let implicit = "schema: 1\nid: implicit\nfixture: { dir: fixtures/base }\n";
        let dir = suite_with(&[("explicit.yaml", explicit), ("implicit.yaml", implicit)]);
        let got = codes(&validate_suite(dir.path(), &pb(PB)));
        let v80: Vec<&String> = got
            .iter()
            .filter(|(c, _)| *c == "V80")
            .map(|(_, m)| m)
            .collect();
        assert_eq!(
            v80,
            [&"eval case `explicit`: checks.goal is `required` but version 1.2.0 has no script or marker goal criterion".to_string()]
        );
        let with_goal = PB.replace(
            "nodes:",
            "goal:\n  statement: s\n  criteria:\n    - { description: d, check: { type: marker, marker: OK } }\nnodes:",
        );
        assert!(validate_suite(dir.path(), &pb(&with_goal)).is_empty());
    }

    #[test]
    fn version_ranges_compare_semver_not_text() {
        let r = VersionRange::parse(">=1.9.0, <1.10.1").unwrap();
        assert!(r.contains("1.10.0"));
        assert!(r.contains("1.9.0"));
        assert!(!r.contains("1.10.1"));
        assert!(!r.contains("1.8.9"));
        assert!(VersionRange::parse("1.2.0").unwrap().contains("1.2.0"));
        assert!(VersionRange::parse("").is_none());
    }

    #[test]
    fn the_case_digest_moves_with_the_case_its_fixture_and_its_scripts() {
        let dir = suite_with(&[("good.yaml", GOOD)]);
        let load = || load_suite(dir.path()).cases.remove(0);
        let a = case_digest(&load(), None).unwrap();
        assert_eq!(
            a,
            case_digest(&load(), None).unwrap(),
            "stable across loads"
        );
        assert_ne!(a, case_digest(&load(), Some("abc")).unwrap());
        let evals = dir.path().join(EVALS_DIR);
        std::fs::write(evals.join("fixtures/base/a.txt"), "b").unwrap();
        let b = case_digest(&load(), None).unwrap();
        assert_ne!(a, b, "fixture change");
        std::fs::write(evals.join("scripts/ok.sh"), "exit 1").unwrap();
        let c = case_digest(&load(), None).unwrap();
        assert_ne!(b, c, "script change");
        std::fs::create_dir_all(evals.join("fixtures/change")).unwrap();
        std::fs::write(
            evals.join("good.yaml"),
            GOOD.replace(
                "dir: fixtures/base }",
                "dir: fixtures/base, change: fixtures/change }",
            ),
        )
        .unwrap();
        let d = case_digest(&load(), None).unwrap();
        std::fs::write(evals.join("fixtures/change/new.txt"), "n").unwrap();
        assert_ne!(d, case_digest(&load(), None).unwrap(), "change overlay");
    }

    /// Two suites over the skill-bundle limits (more than 512 files) are
    /// digested under the suite limits and never share a value; a suite that
    /// cannot be digested is an error and a V80, never a placeholder.
    #[test]
    fn oversized_suites_get_their_own_digests_and_undigestable_ones_are_refused() {
        let a = suite_with(&[("good.yaml", GOOD)]);
        let b = suite_with(&[("good.yaml", GOOD)]);
        for (dir, tag) in [(&a, "a"), (&b, "b")] {
            let big = dir.path().join(EVALS_DIR).join("fixtures/big");
            std::fs::create_dir_all(&big).unwrap();
            for i in 0..600 {
                std::fs::write(big.join(format!("f{i}.txt")), format!("{tag}{i}")).unwrap();
            }
        }
        let da = suite_digest(&suite_dir(a.path())).unwrap();
        let db = suite_digest(&suite_dir(b.path())).unwrap();
        assert_ne!(da, db);
        #[cfg(unix)]
        {
            let evals = suite_dir(a.path());
            std::os::unix::fs::symlink("../../outside", evals.join("fixtures/base/out")).unwrap();
            assert!(suite_digest(&evals).is_err());
            assert!(case_digest(&load_suite(a.path()).cases[0], None).is_err());
            let got = codes(&validate_suite(a.path(), &pb(PB)));
            for needle in [
                "`evals/fixtures/base/out` is a symlink",
                "the suite cannot be digested",
            ] {
                assert!(
                    got.iter().any(|(c, m)| *c == "V80" && m.contains(needle)),
                    "missing `{needle}` in {got:?}"
                );
            }
        }
    }

    /// The approval an older apb stored for every undigestable suite is
    /// dropped on load and matches nothing.
    #[test]
    fn a_stored_missing_approval_is_dropped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approved.json");
        std::fs::write(
            &path,
            r#"{"approved":{"missing":{"playbook":"p","approved_at_ms":1},"sha256:ab":{"playbook":"p","approved_at_ms":2}}}"#,
        )
        .unwrap();
        let a = SuiteApprovals::read(&path);
        assert_eq!(a.approved.keys().collect::<Vec<_>>(), ["sha256:ab"]);
        assert!(!a.is_approved("missing"));
        assert!(a.is_approved("sha256:ab"));
    }

    /// The env overlay cannot reconfigure apb, the loader, git or the
    /// shell, in a case or in `suite.yaml`; an ordinary variable passes.
    #[test]
    fn v80_refuses_env_keys_that_reconfigure_apb_the_loader_or_git() {
        let case = GOOD.replace(
            "repeat: 2\n",
            "repeat: 2\nenv: { APB_CONFIG_DIR: x, HOME: x, LD_PRELOAD: x, GIT_CONFIG_GLOBAL: x, xdg_config_home: x, GH_TOKEN: \"\" }\n",
        );
        let dir = suite_with(&[
            ("good.yaml", &case),
            ("suite.yaml", "env: { PATH: /x, GH_CONFIG_DIR: /y }\n"),
        ]);
        let got: Vec<String> = validate_suite(dir.path(), &pb(PB))
            .iter()
            .filter(|i| i.code == "V80")
            .map(|i| i.message.clone())
            .collect();
        for key in [
            "APB_CONFIG_DIR",
            "HOME",
            "LD_PRELOAD",
            "GIT_CONFIG_GLOBAL",
            "xdg_config_home",
        ] {
            assert!(
                got.iter().any(|m| m.starts_with("eval case `good`: env `")
                    && m.contains(&format!("`{key}` cannot be set"))),
                "{key}: {got:#?}"
            );
        }
        assert!(
            got.iter()
                .any(|m| m.starts_with("eval `suite.yaml`: env `PATH` cannot be set")),
            "{got:#?}"
        );
        assert_eq!(got.len(), 6, "GH_TOKEN and GH_CONFIG_DIR pass: {got:#?}");
    }
}
