//! The read-only views of the start rules: the rule list every surface
//! shows, the doctor's lines, the listener heartbeat of the sweep host,
//! the rule a run was started from, and the fixed words of a journal
//! line's code.

use std::collections::HashSet;
use std::path::PathBuf;

use apb_core::doctor::CheckStatus;
use apb_core::launch::TriggeredBy;
use apb_core::scope::Scope;
use apb_core::trigger::{LogEntry, RulesFile, TriggerStateFile, Verdict};
use apb_core::validate::IssueView;

use crate::EngineError;

/// One rule as the list shows it.
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RuleView {
    pub name: String,
    pub enabled: bool,
    /// `inbox github/main [issues]`.
    pub source: String,
    pub playbook: String,
    pub version: String,
    pub scope: String,
    pub incarnation: String,
    pub seen_through: u64,
    pub starts_last_hour: u32,
    /// Approved for its current digest; an unapproved rule starts nothing.
    pub approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last: Option<LogEntry>,
    /// Filled by the tools layer's list from `approval_need`.
    #[serde(default)]
    pub needs_approval: bool,
    /// A journal the reader refuses, as the one sentence a sweep answers
    /// the rule with: the problem once, then the remedy. A rule whose
    /// journal reads carries none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub journal_error: Option<String>,
    /// `check_rule` now (the playbook or connector may have changed).
    pub issues: Vec<IssueView>,
}

/// The rules of the project at `root`, as the list shows them. The signed
/// files only: a rules file that does not verify is an error (an unsigned
/// file's entries are never shown), an unsigned state reads as positions
/// absent. `needs_approval` stays `false` here; the tools layer's list
/// fills it from `approval_need`.
pub fn list(root: &std::path::Path) -> Result<Vec<RuleView>, EngineError> {
    let now = apb_core::clock::Millis::now();
    let rules_path = signed_path::<RulesFile>(root, "rules");
    let rules = apb_core::signed_store::read::<RulesFile>(root, Scope::Project, "rules")
        .map_err(store_error)?;
    if !rules.signed {
        return Err(EngineError::Invalid(
            apb_core::trigger::rules_unverified_text(&rules_path),
        ));
    }
    let state = apb_core::signed_store::read::<TriggerStateFile>(root, Scope::Project, "state")
        .map_err(store_error)?;
    // An unsigned state's entries are never read: every position shows 0.
    let empty: Vec<apb_core::trigger::RuleState> = Vec::new();
    let states = if state.signed {
        &state.file.rules
    } else {
        &empty
    };
    let mut views = Vec::with_capacity(rules.file.rules.len());
    for entry in &rules.file.rules {
        // A position of another incarnation belongs to another creation of
        // this rule's name and is not this rule's position.
        let seen_through = states
            .iter()
            .find(|s| s.rule == entry.rule.name && s.incarnation == entry.incarnation)
            .map_or(0, |s| s.seen_through);
        // The cap counted as reached is shown, never as free.
        let starts_last_hour =
            crate::trigger::starts_in_last_hour(root, &entry.incarnation, now).unwrap_or(0);
        let approved = entry
            .approval
            .as_ref()
            .is_some_and(|a| a.rule_digest == apb_core::trigger::rule_digest(entry));
        // The newest line, through the reader that dedupes and folds, and
        // the refusal that stopped it: one read answers both, so a
        // listing never reads a journal twice.
        let (mut lines, refusal) =
            apb_core::trigger::read_with_refusal(root, &entry.incarnation, 1);
        let last = lines.pop();
        // A journal the reader refuses is the rule's sentence, as the sweep
        // answers it; a journal that reads carries none.
        let journal_error = refusal.map(|e| {
            apb_core::trigger::journal_refused_text(&entry.rule.name, &entry.incarnation, &e)
        });
        let issues = apb_core::trigger::check_rule(
            &entry.rule,
            &apb_core::trigger::RuleContext::load(root, &entry.rule),
        )
        .iter()
        .map(IssueView::from)
        .collect();
        views.push(RuleView {
            name: entry.rule.name.clone(),
            enabled: entry.enabled,
            source: source_text(entry),
            playbook: entry.rule.playbook.id.clone(),
            version: entry.rule.playbook.version.clone(),
            scope: scope_text(entry.rule.playbook.scope),
            incarnation: entry.incarnation.clone(),
            seen_through,
            starts_last_hour,
            approved,
            last,
            needs_approval: false,
            journal_error,
            issues,
        });
    }
    Ok(views)
}

/// The doctor's lines about files, registration, leftovers and the sweep;
/// the approval lines come from the tools layer.
pub fn doctor_line(root: &std::path::Path) -> Vec<(CheckStatus, String)> {
    let now = apb_core::clock::now_ms_u64();
    let mut lines: Vec<(CheckStatus, String)> = Vec::new();
    let rules_path = signed_path::<RulesFile>(root, "rules");
    let state_path = signed_path::<TriggerStateFile>(root, "state");
    let rules = match apb_core::signed_store::read::<RulesFile>(root, Scope::Project, "rules") {
        Ok(loaded) => Some(loaded),
        Err(e) => {
            lines.push((
                CheckStatus::Warn,
                format!("trigger rules unreadable: {}: {e}", rules_path.display()),
            ));
            None
        }
    };
    if rules.as_ref().is_some_and(|loaded| !loaded.signed) {
        lines.push((
            CheckStatus::Warn,
            apb_core::trigger::rules_unverified_text(&rules_path),
        ));
    }
    let state =
        match apb_core::signed_store::read::<TriggerStateFile>(root, Scope::Project, "state") {
            Ok(loaded) => Some(loaded),
            Err(e) => {
                lines.push((
                    CheckStatus::Warn,
                    format!("trigger state unreadable: {}: {e}", state_path.display()),
                ));
                None
            }
        };
    if state.as_ref().is_some_and(|loaded| !loaded.signed) {
        lines.push((
            CheckStatus::Warn,
            format!(
                "trigger state unreadable: {p}; remove it (rm {p}): every approved rule continues from the current head of its source",
                p = state_path.display()
            ),
        ));
    }
    // A file that does not read shows no entries, so the checks below see
    // no rules rather than invented ones.
    let entries = rules
        .as_ref()
        .filter(|loaded| loaded.signed)
        .map(|l| &l.file.rules);
    let enabled = entries.map_or(0, |rules| rules.iter().filter(|e| e.enabled).count());
    // Item 2. Registration first: without it no listener sweeps this
    // project, whatever the heartbeat says. A project with no rules file in
    // force has no rules to leave unswept, so it stays quiet about it.
    if rules_path.exists() && apb_core::projects::registered_id(root).is_none() {
        lines.push((
            CheckStatus::Warn,
            "rules in a project that is not registered: no listener sweeps it; see apb trigger set"
                .to_string(),
        ));
    }
    if enabled > 0 && !matches!(status(now), ListenerStatus::Live { .. }) {
        lines.push((
            CheckStatus::Warn,
            format!(
                "{enabled} enabled rule(s) and no trigger sweep runs: start apb ingest, or apb dashboard with ingest.enabled"
            ),
        ));
    }
    // Item 4: what a crashed `remove` or a source change leaves. Both files
    // must read, or nothing can be judged an orphan.
    if let Some(message) = entries.and_then(|rules| {
        state
            .as_ref()
            .filter(|loaded| loaded.signed)
            .map(|l| &l.file)
            .and_then(|state| orphans(root, rules, &state.rules, &state_path))
    }) {
        lines.push((CheckStatus::Warn, message));
    }
    // Item 5, only when nothing above found a problem.
    if lines.is_empty() {
        let live = matches!(status(now), ListenerStatus::Live { .. });
        lines.push((
            CheckStatus::Ok,
            format!(
                "{} rule(s), {enabled} enabled, {}",
                entries.map_or(0, |rules| rules.len()),
                if live {
                    "sweep live"
                } else {
                    "sweep not running"
                },
            ),
        ));
    }
    lines
}

/// The file the listener heartbeat writes under the config dir.
pub const LISTENER_FILE: &str = "trigger-listener.json";

/// How long a beat stays live while its pid answers.
const LIVE_FOR_MS: u64 = 60_000;

/// One listener heartbeat.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ListenerBeat {
    pid: u32,
    at_ms: u64,
}

/// The path of the heartbeat under the config dir; `None` without one.
fn listener_path() -> Option<PathBuf> {
    apb_core::config::config_dir().map(|dir| dir.join(LISTENER_FILE))
}

/// Writes `{ pid, at_ms }` atomically under the config dir. Best effort: a
/// sweep that cannot mark itself alive still ran, and the doctor reads the
/// file as stale, which is the truth it can prove.
pub fn beat() {
    let Some(path) = listener_path() else {
        return;
    };
    let beat = ListenerBeat {
        pid: std::process::id(),
        at_ms: apb_core::clock::now_ms_u64(),
    };
    let Ok(bytes) = serde_json::to_vec(&beat) else {
        return;
    };
    let _ = apb_core::fsutil::atomic_write_private(&path, &bytes);
}

/// Whether the sweep's listener heartbeat is alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerStatus {
    Live { age_ms: u64 },
    Stale { age_ms: u64 },
    Absent,
}

/// Live: the file is younger than 60 s and its pid is alive. A file that is
/// missing, unreadable or does not parse is Absent: there is nothing to
/// trust and nothing to accuse.
pub fn status(now_ms: u64) -> ListenerStatus {
    let Some(path) = listener_path() else {
        return ListenerStatus::Absent;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return ListenerStatus::Absent;
    };
    let Ok(beat) = serde_json::from_slice::<ListenerBeat>(&bytes) else {
        return ListenerStatus::Absent;
    };
    let age_ms = now_ms.saturating_sub(beat.at_ms);
    if age_ms < LIVE_FOR_MS && crate::liveness::pid_alive(beat.pid) {
        ListenerStatus::Live { age_ms }
    } else {
        ListenerStatus::Stale { age_ms }
    }
}

/// The rule that started the run at `run_dir`: its manifest's launch
/// record's `triggered_by`; `None` for a run no rule started and for one
/// whose record lost the field.
pub fn triggered_by(run_dir: &std::path::Path) -> Option<TriggeredBy> {
    crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .and_then(|m| m.launch)
        .and_then(|l| l.triggered_by)
}

/// "started by rule `triage-new-issues` (inbox github/main, event 42)". A
/// record that lost its connector, account or event renders the parts that
/// exist.
pub fn triggered_by_line(by: &TriggeredBy) -> String {
    let mut line = format!("started by rule `{}`", by.rule);
    let mut parts: Vec<String> = Vec::new();
    // The source word qualifies its connector and account: it renders only
    // with a pair, so a record that lost both does not show a bare "inbox".
    let pair = match (by.connector.as_deref(), by.account.as_deref()) {
        (Some(c), Some(a)) if !c.is_empty() && !a.is_empty() => Some(format!("{c}/{a}")),
        (Some(c), _) if !c.is_empty() => Some(c.to_string()),
        (None, Some(a)) if !a.is_empty() => Some(a.to_string()),
        _ => None,
    };
    if let Some(pair) = pair {
        if by.source.is_empty() {
            parts.push(pair);
        } else {
            parts.push(format!("{} {pair}", by.source));
        }
    }
    if let Some(seq) = by.event_seq {
        parts.push(format!("event {seq}"));
    }
    if !parts.is_empty() {
        line.push_str(&format!(" ({})", parts.join(", ")));
    }
    line
}

/// The words of one journal line for every surface: a fixed sentence per
/// known code, `refused (<code>)` for any other. The fields a line's
/// closed set does not check are never read, so an engine sentence, a body
/// value or a param value cannot come back through them.
pub fn verdict_words(line: &LogEntry) -> String {
    if let Some(code) = line.code.as_deref() {
        if let Some(words) = words_for_code(code) {
            return words.to_string();
        }
        // Only text that passes the closed pattern is echoed back.
        return if apb_core::trigger::is_code_text(code) {
            format!("refused ({code})")
        } else {
            "refused".to_string()
        };
    }
    match line.verdict {
        Verdict::Gap => match line.range {
            Some((from, to)) => {
                format!("events {from} to {to} were removed before the rule saw them")
            }
            None => "events were removed before the rule saw them".to_string(),
        },
        Verdict::Positioned => "positioned at the current head of the source".to_string(),
        Verdict::Started => "started a run".to_string(),
        Verdict::AlreadyStarted => "a run for this event was already started".to_string(),
        Verdict::Held => "held for the next pass".to_string(),
        Verdict::Refused => "refused".to_string(),
        Verdict::Skipped => "skipped".to_string(),
        Verdict::Failed => "failed".to_string(),
    }
}

/// The fixed sentence of every code the engine records, in one table the
/// generated TypeScript constants carry too (`TRIGGER_CODE_WORDS`), so the
/// dashboard words a line exactly as the CLI does.
pub const CODE_WORDS: &[(&str, &str)] = &[
    ("type", "the event's type is not one the rule listens for"),
    (
        "where",
        "the event did not pass the rule's where conditions",
    ),
    ("own_send", "the delivery is this installation's own send"),
    (
        "key_missing",
        "the start key template found nothing in the event",
    ),
    (
        "key_not_scalar",
        "the start key template found a value that is not text",
    ),
    ("key_too_long", "the rendered start key is too long"),
    (
        "key_invalid",
        "the rendered start key is not a valid start key",
    ),
    (
        "param_missing",
        "a param of the rule found nothing in the event",
    ),
    (
        "param_not_scalar",
        "a param of the rule found a value that is not text",
    ),
    ("param_too_long", "a rendered param is too long"),
    (
        "param_pattern_mismatch",
        "a rendered param did not match its pattern",
    ),
    ("rate_cap", "the rule's hourly start cap is reached"),
    (
        "rate_cap_unknown",
        "the hourly cap could not be counted, so the rule starts nothing this pass",
    ),
    (
        "start_key_busy",
        "the start key names a run still being prepared; the event is tried again next pass",
    ),
    (
        "start_key_conflict",
        "the start key names a run started with other params",
    ),
    ("check_rule", "the rule no longer passes its own checks"),
    (
        "binding_changed",
        "the resolved binding is not the approval's; the approval is revoked",
    ),
];

/// The fixed sentence of a code the engine records.
fn words_for_code(code: &str) -> Option<&'static str> {
    CODE_WORDS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, words)| *words)
}

/// One journal line as every text surface prints it (`apb trigger log`,
/// `apb trigger show`): the event's seq, the verdict words, the condition
/// marks (each condition's index and whether it passed, never its
/// literal), the param name, the validator codes and the run id, each part
/// only when the line has it. The dashboard renders the same parts in the
/// same order (`journalLineText` in `web/src/lib/triggers.ts`).
pub fn journal_line_text(line: &LogEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    let words = verdict_words(line);
    parts.push(match line.seq {
        Some(seq) => format!("event {seq}: {words}"),
        None => words,
    });
    if !line.conditions.is_empty() {
        let marks: Vec<String> = line
            .conditions
            .iter()
            .map(|m| {
                format!(
                    "#{} {}",
                    m.index,
                    if m.passed { "passed" } else { "failed" }
                )
            })
            .collect();
        parts.push(format!("conditions {}", marks.join(", ")));
    }
    if let Some(name) = line.name.as_deref() {
        parts.push(format!("param {name}"));
    }
    if !line.codes.is_empty() {
        parts.push(format!("codes {}", line.codes.join(", ")));
    }
    if let Some(run) = line.run_id.as_deref() {
        parts.push(format!("run {run}"));
    }
    parts.join("; ")
}

/// The path of the project's signed `rules` or `state` file.
fn signed_path<T: apb_core::signed_store::SignedDoc>(root: &std::path::Path, id: &str) -> PathBuf {
    apb_core::signed_store::file::<T>(root, Scope::Project, id)
}

/// A store error in the engine's shape.
fn store_error(error: apb_core::signed_store::StoreError) -> EngineError {
    match error {
        apb_core::signed_store::StoreError::Io(e) => EngineError::Io(e),
        other => EngineError::Invalid(other.to_string()),
    }
}

/// `inbox github/acme [issues]`, from the rule itself. A later source kind
/// gets its own arm here.
fn source_text(entry: &apb_core::trigger::RuleEntry) -> String {
    match &entry.rule.source {
        apb_core::trigger::RuleSource::Inbox(inbox) => {
            let mut text = format!("inbox {}/{}", inbox.connector, inbox.account);
            if !inbox.types.is_empty() {
                text.push_str(&format!(" [{}]", inbox.types.join(", ")));
            }
            text
        }
    }
}

/// `project` or `global`.
fn scope_text(scope: apb_core::trigger::RuleScope) -> String {
    match scope {
        apb_core::trigger::RuleScope::Project => "project".to_string(),
        apb_core::trigger::RuleScope::Global => "global".to_string(),
    }
}

/// The doctor's item 4: the orphan state entries and journal files of rules
/// no longer in force, each with its path. `None` when nothing is orphaned.
fn orphans(
    root: &std::path::Path,
    rules: &[apb_core::trigger::RuleEntry],
    states: &[apb_core::trigger::RuleState],
    state_path: &std::path::Path,
) -> Option<String> {
    let named_rules: HashSet<&str> = rules.iter().map(|e| e.rule.name.as_str()).collect();
    let named_incarnations: HashSet<&str> = rules.iter().map(|e| e.incarnation.as_str()).collect();
    let mut paths: Vec<String> = Vec::new();
    // The orphan state entries share the one state file; the path is
    // listed once, removing the entries is a person's `apb trigger set`
    // rewrite or a state rewrite away.
    if states
        .iter()
        .any(|s| !named_rules.contains(s.rule.as_str()))
    {
        paths.push(state_path.display().to_string());
    }
    let log_dir = root.join(".apb").join("trigger-state").join("log");
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if !named_incarnations.contains(stem) {
                paths.push(path.display().to_string());
            }
        }
    }
    if paths.is_empty() {
        return None;
    }
    paths.sort();
    Some(format!(
        "{} orphan file(s) of rules no longer in force (inert, decide nothing); delete: {}",
        paths.len(),
        paths.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::scope::Scope;
    use apb_core::signed_store::{self, Unsigned};
    use apb_core::trigger::{
        Approval, InboxSource, LogEntry, Rule, RuleEntry, RuleLimits, RulePlaybook, RuleScope,
        RuleSource, RulesFile, TriggerStateFile, Verdict, append, rule_digest,
    };

    /// A body value an event carried: no view may repeat it.
    const BODY_VALUE: &str = "SECRET-BODY-VALUE";
    /// A param value a rule rendered: no view may repeat it.
    const PARAM_VALUE: &str = "SECRET-PARAM-VALUE";
    /// A long sentence an engine refusal carries: a journal line holds the
    /// code, never this.
    const ENGINE_SENTENCE: &str = "another run of key triage holds the concurrency slot already, its display names the params";

    /// Runs `body` in a child process of its own with `APB_CONFIG_DIR` in a
    /// fresh directory: every test that touches the config dir (the
    /// heartbeat, the installation key, the projects registry) runs in one,
    /// so env mutation stays serial and off the developer's machine.
    fn with_config(test_path: &str, body: impl FnOnce(&std::path::Path)) {
        apb_core::process_mode::testing::in_plain_child(test_path, || {
            let cfg = tempfile::tempdir().unwrap();
            // SAFETY: the child runs this one test alone on one thread.
            unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
            apb_core::run_origin::ensure_key().unwrap();
            body(cfg.path());
        });
    }

    /// A registered project of its own.
    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        apb_core::projects::bind_id(root.path()).unwrap();
        root
    }

    /// One rule entry, enabled, unapproved.
    fn entry(name: &str, incarnation: &str) -> RuleEntry {
        RuleEntry {
            rule: Rule {
                name: name.to_string(),
                playbook: RulePlaybook {
                    id: "demo-report".to_string(),
                    version: "1.0.0".to_string(),
                    scope: RuleScope::Project,
                },
                source: RuleSource::Inbox(InboxSource {
                    connector: "github".to_string(),
                    account: "acme".to_string(),
                    types: vec!["issues".to_string()],
                    conditions: Vec::new(),
                    key: "{{ event.id }}".to_string(),
                }),
                params: Default::default(),
                limits: RuleLimits { max_per_hour: 3 },
                budget: None,
                external_data: true,
                require_ready: true,
                notify: None,
            },
            incarnation: incarnation.to_string(),
            enabled: true,
            approval: None,
            created_at: 1,
            updated_at: 1,
            updated_by: "terminal".to_string(),
        }
    }

    /// Writes `entries` as the project's signed rules file.
    fn write_rules(root: &std::path::Path, entries: &[RuleEntry]) {
        let id = apb_core::projects::registered_id(root).unwrap();
        signed_store::transact(
            root,
            Scope::Project,
            "rules",
            Unsigned::Replace,
            |file: &mut RulesFile| -> Result<((), bool), apb_core::signed_store::StoreError> {
                file.project = id.clone();
                file.rules = entries.to_vec();
                Ok(((), true))
            },
        )
        .unwrap();
    }

    /// Writes `rules` as the project's signed state file.
    fn write_state(root: &std::path::Path, rules: Vec<apb_core::trigger::RuleState>) {
        let id = apb_core::projects::registered_id(root).unwrap();
        signed_store::transact(
            root,
            Scope::Project,
            "state",
            Unsigned::Replace,
            |file: &mut TriggerStateFile| -> Result<((), bool), apb_core::signed_store::StoreError> {
                file.project = id.clone();
                file.rules = rules;
                Ok(((), true))
            },
        )
        .unwrap()
    }

    /// The path of the project's journal file for `incarnation`.
    fn log_path(root: &std::path::Path, incarnation: &str) -> std::path::PathBuf {
        root.join(".apb")
            .join("trigger-state")
            .join("log")
            .join(format!("{incarnation}.jsonl"))
    }

    /// A rule approved for `digest`.
    fn approved_with(entry: RuleEntry, digest: &str) -> RuleEntry {
        let mut entry = entry;
        entry.approval = Some(Approval {
            rule_digest: digest.to_string(),
            playbook_digest: "sha256:p".to_string(),
            binding: "sha256:b".to_string(),
            permit_digest: "sha256:permit".to_string(),
            launch_policy: "allow".to_string(),
            consent_nonce: None,
            approved_at: 2,
            approved_by: "terminal".to_string(),
        });
        entry
    }

    /// A rule approved for its current digest.
    fn approved(entry: RuleEntry) -> RuleEntry {
        let digest = rule_digest(&entry);
        approved_with(entry, &digest)
    }

    /// A fresh beat reads Live; an old one and a dead pid read Stale; a
    /// missing file reads Absent.
    #[test]
    fn a_beat_reads_live_and_an_old_one_stale() {
        with_config(
            "trigger::view::tests::a_beat_reads_live_and_an_old_one_stale",
            |cfg| {
                super::beat();
                let now = apb_core::clock::now_ms_u64();
                match super::status(now) {
                    ListenerStatus::Live { age_ms } => assert!(age_ms < 1_000, "{age_ms}"),
                    other => panic!("a fresh beat with a live pid reads Live, got {other:?}"),
                }
                let path = cfg.join(super::LISTENER_FILE);
                // An old at_ms is stale even while the pid lives.
                let old = serde_json::json!({
                    "pid": std::process::id(),
                    "at_ms": now.saturating_sub(120_000),
                });
                std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
                match super::status(now) {
                    ListenerStatus::Stale { age_ms } => assert!(age_ms >= 60_000, "{age_ms}"),
                    other => panic!("an old beat reads Stale, got {other:?}"),
                }
                // A dead pid is stale even while the file is fresh (a pid
                // above every pid_max this kernel can hand out).
                let dead = serde_json::json!({ "pid": u32::MAX, "at_ms": now });
                std::fs::write(&path, serde_json::to_vec(&dead).unwrap()).unwrap();
                assert_eq!(super::status(now), ListenerStatus::Stale { age_ms: 0 });
                // A missing file (and an unreadable one) is absent.
                let empty = tempfile::tempdir().unwrap();
                // SAFETY: still the one thread of this child process.
                unsafe { std::env::set_var("APB_CONFIG_DIR", empty.path()) };
                assert_eq!(super::status(now), ListenerStatus::Absent);
            },
        );
    }

    /// A state file that does not verify is the doctor's item 1, with the
    /// path and the recovery.
    #[test]
    fn doctor_line_names_an_unverified_state() {
        with_config(
            "trigger::view::tests::doctor_line_names_an_unverified_state",
            |_| {
                let root = project();
                write_rules(root.path(), &[entry("issues", "inc1")]);
                let path =
                    signed_store::file::<TriggerStateFile>(root.path(), Scope::Project, "state");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, r#"{"schema":1,"scope":"project","id":"state","rules":[],"mac":"0123456789abcdef"}"#).unwrap();
                let lines = super::doctor_line(root.path());
                let (_, message) = lines
                    .iter()
                    .find(|(s, m)| {
                        *s == CheckStatus::Warn && m.contains("trigger state unreadable")
                    })
                    .expect("the doctor names the unreadable state");
                assert!(
                    message.contains(path.display().to_string().as_str()),
                    "{message}"
                );
                assert!(
                    message.contains(&format!("remove it (rm {})", path.display())),
                    "{message}"
                );
                assert!(
                    message.contains(
                        "every approved rule continues from the current head of its source"
                    ),
                    "{message}"
                );
            },
        );
    }

    /// Rules in a project with no registered id are the doctor's item 2.
    #[test]
    fn doctor_line_names_an_unregistered_project() {
        with_config(
            "trigger::view::tests::doctor_line_names_an_unregistered_project",
            |_| {
                let root = tempfile::tempdir().unwrap();
                // No `bind_id`: the project never registered. A rules file
                // is there all the same (a project that lost its registry
                // entry, say).
                let path = signed_store::file::<RulesFile>(root.path(), Scope::Project, "rules");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, r#"{"schema":1}"#).unwrap();
                let lines = super::doctor_line(root.path());
                assert!(lines.iter().any(|(s, m)| *s == CheckStatus::Warn
                    && m == "rules in a project that is not registered: no listener sweeps it; see apb trigger set"),
                    "{lines:?}");
            },
        );
    }

    /// A state entry no rule names and a journal file whose incarnation no
    /// rule names are the doctor's item 4, each with its path.
    #[test]
    fn doctor_line_names_leftovers_of_a_crashed_remove() {
        with_config(
            "trigger::view::tests::doctor_line_names_leftovers_of_a_crashed_remove",
            |_| {
                let root = project();
                write_rules(root.path(), &[entry("issues", "inc1")]);
                // The state holds an entry of a rule the rules file lost
                // (a crashed remove), and `log/` a journal of an incarnation
                // no rule names (a source change).
                write_state(
                    root.path(),
                    vec![
                        apb_core::trigger::RuleState {
                            rule: "issues".to_string(),
                            incarnation: "inc1".to_string(),
                            source: Default::default(),
                            seen_through: 4,
                        },
                        apb_core::trigger::RuleState {
                            rule: "gone".to_string(),
                            incarnation: "incgone".to_string(),
                            source: Default::default(),
                            seen_through: 9,
                        },
                    ],
                );
                let mut ids = std::collections::HashSet::new();
                append(
                    root.path(),
                    "deadinc",
                    &mut ids,
                    &[LogEntry::for_event(
                        "deadinc",
                        1,
                        1,
                        Verdict::Skipped,
                        Some("type"),
                        None,
                    )],
                )
                .unwrap();
                let lines = super::doctor_line(root.path());
                let (_, message) = lines
                    .iter()
                    .find(|(s, m)| *s == CheckStatus::Warn && m.contains("orphan file(s)"))
                    .expect("the doctor names the leftovers");
                assert!(message.starts_with("2 orphan file(s) of rules no longer in force (inert, decide nothing); delete: "), "{message}");
                let state_path =
                    signed_store::file::<TriggerStateFile>(root.path(), Scope::Project, "state");
                assert!(
                    message.contains(state_path.display().to_string().as_str()),
                    "{message}"
                );
                assert!(
                    message.contains(
                        log_path(root.path(), "deadinc")
                            .display()
                            .to_string()
                            .as_str()
                    ),
                    "{message}"
                );
            },
        );
    }

    /// The line names the rule, and the source and event parts that exist.
    #[test]
    fn triggered_by_line_names_the_rule_and_event() {
        let by = TriggeredBy {
            rule: "triage-new-issues".to_string(),
            incarnation: "inc1".to_string(),
            source: "inbox".to_string(),
            connector: Some("github".to_string()),
            account: Some("main".to_string()),
            event_seq: Some(42),
        };
        assert_eq!(
            triggered_by_line(&by),
            "started by rule `triage-new-issues` (inbox github/main, event 42)"
        );
        let no_account = TriggeredBy {
            connector: Some("github".to_string()),
            account: None,
            ..by.clone()
        };
        assert_eq!(
            triggered_by_line(&no_account),
            "started by rule `triage-new-issues` (inbox github, event 42)"
        );
        let no_source = TriggeredBy {
            connector: None,
            account: None,
            ..by.clone()
        };
        assert_eq!(
            triggered_by_line(&no_source),
            "started by rule `triage-new-issues` (event 42)"
        );
        let no_event = TriggeredBy {
            event_seq: None,
            ..by.clone()
        };
        assert_eq!(
            triggered_by_line(&no_event),
            "started by rule `triage-new-issues` (inbox github/main)"
        );
        let bare = TriggeredBy {
            connector: None,
            account: None,
            event_seq: None,
            ..by
        };
        assert_eq!(
            triggered_by_line(&bare),
            "started by rule `triage-new-issues`"
        );
    }

    /// The words of a line come from its code alone: a known code its fixed
    /// sentence, any other `refused (<code>)`; no field's unchecked text is
    /// ever repeated.
    #[test]
    fn verdict_words_never_repeat_a_stored_text() {
        // Every verdict, with a body value, a param value and an engine
        // sentence in every field the journal's closed set does not check:
        // none of them may reach the words.
        for verdict in [
            Verdict::Skipped,
            Verdict::Held,
            Verdict::Started,
            Verdict::AlreadyStarted,
            Verdict::Refused,
            Verdict::Failed,
            Verdict::Gap,
            Verdict::Positioned,
        ] {
            let line = LogEntry {
                id: format!("inc:1:{BODY_VALUE}"),
                at: 1,
                verdict,
                seq: Some(1),
                range: Some((1, 2)),
                run_id: Some(PARAM_VALUE.to_string()),
                code: Some(ENGINE_SENTENCE.to_string()),
                name: Some(BODY_VALUE.to_string()),
                binding: Some(PARAM_VALUE.to_string()),
                approved: Some(ENGINE_SENTENCE.to_string()),
                codes: vec![BODY_VALUE.to_string()],
                conditions: Vec::new(),
            };
            let words = verdict_words(&line);
            assert!(!words.contains(BODY_VALUE), "{verdict:?}: {words}");
            assert!(!words.contains(PARAM_VALUE), "{verdict:?}: {words}");
            assert!(!words.contains(ENGINE_SENTENCE), "{verdict:?}: {words}");
        }
        // A hostile code that slipped past the storage check is not echoed
        // (built as a raw struct: `for_event` would have normalized it).
        let mut line = LogEntry {
            id: "inc1:1:refused".to_string(),
            at: 1,
            verdict: Verdict::Refused,
            seq: Some(1),
            range: None,
            run_id: None,
            code: Some(ENGINE_SENTENCE.to_string()),
            name: None,
            binding: None,
            approved: None,
            codes: Vec::new(),
            conditions: Vec::new(),
        };
        assert_eq!(verdict_words(&line), "refused", "{line:?}");
        // `refused_other` and an unknown code render `refused (<code>)`.
        line.code = Some("refused_other".to_string());
        assert_eq!(verdict_words(&line), "refused (refused_other)");
        line.code = Some("a_code_a_future_apb_writes".to_string());
        assert_eq!(verdict_words(&line), "refused (a_code_a_future_apb_writes)");
        // Every code the engine records renders its own fixed sentence.
        for code in [
            "type",
            "where",
            "own_send",
            "key_missing",
            "key_not_scalar",
            "key_too_long",
            "key_invalid",
            "param_missing",
            "param_not_scalar",
            "param_too_long",
            "param_pattern_mismatch",
            "rate_cap",
            "rate_cap_unknown",
            "start_key_busy",
            "start_key_conflict",
            "check_rule",
            "binding_changed",
        ] {
            line.code = Some(code.to_string());
            let words = verdict_words(&line);
            assert!(!words.is_empty(), "{code}: no words");
            assert_ne!(
                words, code,
                "{code}: the words are a sentence, not the bare code"
            );
            assert_ne!(
                words,
                format!("refused ({code})"),
                "{code}: not a known code"
            );
        }
        // The verdicts without a code have words of their own.
        let mut gap = LogEntry::for_gap("inc1", 2, 3, 5);
        assert_eq!(
            verdict_words(&gap),
            "events 3 to 5 were removed before the rule saw them"
        );
        gap.range = None;
        assert!(!verdict_words(&gap).is_empty());
        assert!(!verdict_words(&LogEntry::for_position("inc1", 3, 7)).is_empty());
        let started = LogEntry::for_event("inc1", 4, 4, Verdict::Started, None, None);
        assert!(!verdict_words(&started).is_empty());
    }

    /// `list` reads the signed rules, each rule's state, its journal tail
    /// and its fresh check, and never repeats a stored value.
    #[test]
    fn list_reads_signed_rules_and_their_state() {
        with_config(
            "trigger::view::tests::list_reads_signed_rules_and_their_state",
            |_| {
                let root = project();
                // `quiet` holds an approval for an older digest: it shows
                // unapproved, since only the current digest approves.
                let stale = approved_with(entry("quiet", "inc2"), "sha256:stale");
                write_rules(root.path(), &[approved(entry("issues", "inc1")), stale]);
                write_state(
                    root.path(),
                    vec![apb_core::trigger::RuleState {
                        rule: "issues".to_string(),
                        incarnation: "inc1".to_string(),
                        source: Default::default(),
                        seen_through: 7,
                    }],
                );
                let mut ids = std::collections::HashSet::new();
                append(
                    root.path(),
                    "inc1",
                    &mut ids,
                    &[LogEntry::for_event(
                        "inc1",
                        7,
                        9,
                        Verdict::Skipped,
                        Some("type"),
                        None,
                    )],
                )
                .unwrap();
                let views = super::list(root.path()).unwrap();
                assert_eq!(views.len(), 2, "{views:?}");
                let issues = views.iter().find(|v| v.name == "issues").unwrap();
                assert!(issues.enabled);
                assert_eq!(issues.source, "inbox github/acme [issues]");
                assert_eq!(issues.playbook, "demo-report");
                assert_eq!(issues.version, "1.0.0");
                assert_eq!(issues.scope, "project");
                assert_eq!(issues.incarnation, "inc1");
                assert_eq!(issues.seen_through, 7);
                assert_eq!(issues.starts_last_hour, 0);
                assert!(issues.approved, "approved for its current digest");
                assert!(!issues.needs_approval, "the tools layer fills it");
                let last = issues.last.as_ref().expect("the journal tail");
                assert_eq!(last.id, "inc1:7:skipped");
                // `check_rule` ran: the fixture connector is not installed,
                // and the rule says so.
                assert!(
                    issues
                        .issues
                        .iter()
                        .any(|i| i.code == "V260" && i.message.contains("github")),
                    "{:?}",
                    issues.issues
                );
                let quiet = views.iter().find(|v| v.name == "quiet").unwrap();
                assert!(!quiet.approved, "an unapproved rule shows as such");
                assert_eq!(quiet.seen_through, 0, "no state entry: 0");
                assert!(quiet.last.is_none(), "no journal: no last line");
            },
        );
    }

    /// A rules file that does not verify is an error, never an empty list.
    #[test]
    fn list_refuses_unsigned_rules() {
        with_config("trigger::view::tests::list_refuses_unsigned_rules", |_| {
            let root = tempfile::tempdir().unwrap();
            let path = signed_store::file::<RulesFile>(root.path(), Scope::Project, "rules");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, r#"{"schema":1,"rules":[]}"#).unwrap();
            let err = super::list(root.path()).unwrap_err();
            assert!(
                err.to_string()
                    .contains("move it aside to write rules again"),
                "{err}"
            );
        });
    }

    /// A journal the reader refuses is the view's `journal_error`: the one
    /// sentence a sweep answers the rule with, the problem named once, then
    /// the remedy with the path to delete. A journal that reads carries
    /// none.
    #[test]
    fn a_refused_journal_is_the_views_journal_error() {
        with_config(
            "trigger::view::tests::a_refused_journal_is_the_views_journal_error",
            |_| {
                let root = project();
                write_rules(root.path(), &[approved(entry("news", "inc1"))]);
                let log = log_path(root.path(), "inc1");
                std::fs::create_dir_all(log.parent().unwrap()).unwrap();
                // A directory at the journal's path: the bounded read
                // refuses it, never opens it.
                std::fs::create_dir(&log).unwrap();
                let views = super::list(root.path()).unwrap();
                let view = &views[0];
                let sentence = view
                    .journal_error
                    .as_ref()
                    .expect("the refusal is the view's");
                assert!(sentence.contains("journal_refused"), "{sentence}");
                assert!(sentence.contains("`news`"), "{sentence}");
                assert!(
                    sentence.contains(
                        "; remove the rule and save it again, or delete .apb/trigger-state/log/inc1.jsonl"
                    ),
                    "{sentence}"
                );
                assert!(
                    !sentence.contains("cannot be read: the trigger journal of"),
                    "the problem is named once: {sentence}"
                );
                // A journal that reads carries no error.
                std::fs::remove_dir(&log).unwrap();
                let mut ids = std::collections::HashSet::new();
                append(
                    root.path(),
                    "inc1",
                    &mut ids,
                    &[LogEntry::for_event(
                        "inc1",
                        1,
                        1,
                        Verdict::Skipped,
                        Some("type"),
                        None,
                    )],
                )
                .unwrap();
                let views = super::list(root.path()).unwrap();
                assert!(
                    views[0].journal_error.is_none(),
                    "{:?}",
                    views[0].journal_error
                );
            },
        );
    }

    /// A listing reads every journal once, not twice: the newest line and
    /// the refusal that stopped it come from the same read.
    #[test]
    fn a_listing_reads_every_journal_once() {
        with_config(
            "trigger::view::tests::a_listing_reads_every_journal_once",
            |_| {
                let root = project();
                write_rules(
                    root.path(),
                    &[
                        approved(entry("news", "inc1")),
                        approved(entry("other", "inc2")),
                    ],
                );
                let mut ids = std::collections::HashSet::new();
                append(
                    root.path(),
                    "inc1",
                    &mut ids,
                    &[LogEntry::for_event(
                        "inc1",
                        1,
                        1,
                        Verdict::Skipped,
                        Some("type"),
                        None,
                    )],
                )
                .unwrap();
                apb_core::trigger::reset_journal_reads();
                let views = super::list(root.path()).unwrap();
                assert_eq!(views.len(), 2, "{views:?}");
                assert_eq!(
                    apb_core::trigger::journal_reads_count(),
                    2,
                    "one read per journal"
                );
                assert!(views[0].last.is_some(), "{views:?}");
            },
        );
    }

    /// `triggered_by` reads the manifest's launch record; a run no rule
    /// started, and one whose record lost the field, read `None`.
    #[test]
    fn triggered_by_reads_the_launch_record() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(super::triggered_by(&dir), None, "no manifest at all");
        let by = TriggeredBy {
            rule: "triage-new-issues".to_string(),
            incarnation: "inc1".to_string(),
            source: "inbox".to_string(),
            connector: Some("github".to_string()),
            account: Some("main".to_string()),
            event_seq: Some(42),
        };
        let manifest = crate::manifest::RunExecutionManifest {
            launch: Some(apb_core::launch::LaunchRecord {
                kind: "start".to_string(),
                initiator: "engine:trigger".to_string(),
                confirmation: None,
                policy: apb_core::launch::LaunchPolicy::Allow,
                sources: Vec::new(),
                billing: None,
                started_by: None,
                triggered_by: Some(by.clone()),
            }),
            ..Default::default()
        };
        crate::manifest::write(&dir, &manifest).unwrap();
        assert_eq!(super::triggered_by(&dir), Some(by));
        // A record without the field reads as None.
        let dir2 = root.path().join(".apb/runs/r2");
        std::fs::create_dir_all(&dir2).unwrap();
        let plain = crate::manifest::RunExecutionManifest {
            launch: Some(apb_core::launch::LaunchRecord {
                kind: "start".to_string(),
                initiator: "cli_terminal".to_string(),
                confirmation: None,
                policy: apb_core::launch::LaunchPolicy::Allow,
                sources: Vec::new(),
                billing: None,
                started_by: None,
                triggered_by: None,
            }),
            ..Default::default()
        };
        crate::manifest::write(&dir2, &plain).unwrap();
        assert_eq!(super::triggered_by(&dir2), None);
    }
}
