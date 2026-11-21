//! The rule a person writes, the signed per-project store of
//! `.apb/triggers/project/rules.json`, the incarnation that binds a
//! creation's state, journal and start keys, and the locked writes every
//! mutation goes through. Only a person changes a rule; this module does
//! not check who calls (the CLI and HTTP layers do).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::state::TriggerStateFile;
use crate::scope::Scope;
use crate::signed_store::{Loaded, StoreError, Unsigned};
use crate::validate::{Issue, Severity};

// The data types live in `model.rs`, a leaf both this store, the checks
// and the state import from; re-exported here under the paths the design
// names, so the public surface is unchanged.
pub use super::model::{
    Approval, InboxSource, MAX_PARAMS, NAME_MAX, RULES_UNVERIFIED, Rule, RuleEntry, RuleLimits,
    RulePlaybook, RuleScope, RuleSource, RulesFile, SetOutcome, TriggerError, rule_digest,
    rules_unverified_detail, rules_unverified_text,
};
/// A rendered param or key longer than this is refused. Defined in
/// `render.rs` (its only enforcer), re-exported here under the path the
/// design names.
pub use super::render::RENDERED_MAX_CHARS;

/// The two locks of one rule, held together by a mutation or an approval.
pub struct RuleLock {
    /// The change lock, taken first.
    #[allow(dead_code)]
    pub(crate) change: crate::fsutil::HeldLock,
    /// The sweep lock, taken after the change lock.
    #[allow(dead_code)]
    pub(crate) sweep: crate::fsutil::HeldLock,
}

/// How long a mutation waits for each of the rule's two locks before it
/// answers `Busy`.
const LOCK_WAIT: Duration = Duration::from_secs(60);

/// The rule name as a lock or store boundary takes it: one rule name, so
/// one safe path segment, checked before any filesystem operation. A name
/// that is not one cannot name a rule: `NotFound`.
pub fn checked_rule_name(name: &str) -> Result<&str, TriggerError> {
    if super::validate::name_ok(name) {
        Ok(name)
    } else {
        Err(TriggerError::NotFound(name.to_string()))
    }
}

/// Both locks of the rule, waited for (60 s each); what the HTTP route and
/// `approve` hold while they compare and write. The name is checked first
/// ([`checked_rule_name`]): every mutation takes its locks here.
pub fn hold_rule_lock(root: &Path, name: &str) -> Result<RuleLock, TriggerError> {
    let name = checked_rule_name(name)?;
    let dir = trigger_state_dir(root);
    let change = crate::fsutil::hold_lock_waiting(&dir, &format!("{name}.change.lock"), LOCK_WAIT)
        .map_err(|e| busy_or_store(name, e))?;
    let sweep = crate::fsutil::hold_lock_waiting(&dir, &format!("{name}.sweep.lock"), LOCK_WAIT)
        .map_err(|e| busy_or_store(name, e))?;
    Ok(RuleLock { change, sweep })
}

/// A lock that expired its wait is `Busy`; anything else is a store error.
fn busy_or_store(name: &str, error: io::Error) -> TriggerError {
    if error.kind() == io::ErrorKind::WouldBlock {
        TriggerError::Busy(name.to_string())
    } else {
        TriggerError::Store(StoreError::Io(error))
    }
}

/// A rule error of a rules write: the read cap's refusal, which the store
/// wraps in its own error, is the rules store's `rules_store_full`,
/// everything else stays as it is.
fn store_error(error: TriggerError) -> TriggerError {
    match error {
        TriggerError::Store(StoreError::TooLarge { limit, .. }) => TriggerError::StoreFull(limit),
        other => other,
    }
}

/// The directory holding both locks of every rule of the project.
fn trigger_state_dir(root: &Path) -> PathBuf {
    root.join(".apb").join("trigger-state")
}

/// A fresh incarnation: 32 lowercase hex, minted per creation and again at
/// every change of the source (I5).
fn fresh_incarnation() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The connector and account a source reads: the identity a source change
/// is judged by (I5). A later source kind gets its own arm here.
fn source_id(source: &RuleSource) -> Option<(&str, &str)> {
    match source {
        RuleSource::Inbox(inbox) => Some((&inbox.connector, &inbox.account)),
    }
}

/// The rules file, only as this installation signed it for this project
/// (I5): a file that does not verify, was written by another installation,
/// is for another project, or is reached through a link, reads unsigned,
/// and unsigned is the same refusal for reads and writes.
fn read_rules(root: &Path) -> Result<Loaded<RulesFile>, TriggerError> {
    let loaded = crate::signed_store::read::<RulesFile>(root, Scope::Project, "rules")?;
    if !loaded.signed {
        return Err(TriggerError::Store(StoreError::Unverified(
            crate::signed_store::file::<RulesFile>(root, Scope::Project, "rules"),
        )));
    }
    Ok(loaded)
}

/// One state transaction removing the rule's entry (what `approve`,
/// `enable` and `remove` do); an absent entry writes nothing.
fn delete_state_entry(root: &Path, name: &str, project: Option<&str>) -> Result<(), TriggerError> {
    crate::signed_store::transact(
        root,
        Scope::Project,
        "state",
        Unsigned::Refuse,
        |file: &mut TriggerStateFile| -> Result<((), bool), TriggerError> {
            if let Some(id) = project {
                file.project = id.to_string();
            }
            let before = file.rules.len();
            super::state::delete_entry(file, name);
            Ok(((), file.rules.len() != before))
        },
    )?;
    Ok(())
}

/// The state entry's deletion after the rules transaction committed: its
/// failure is an outcome, never an error (W5). The rule is gone either
/// way, and the leftover state entry is harmless: the sweep matches the
/// state by name and incarnation, and a new save gets a new incarnation.
fn delete_state_entry_left(root: &Path, name: &str, project: Option<&str>) -> Option<String> {
    match delete_state_entry(root, name, project) {
        Ok(()) => None,
        Err(e) => Some(format!(".apb/trigger-state/project/state.json ({e})")),
    }
}

/// The journal file of one incarnation (only `remove` deletes one). A file
/// that is not there is already as good as deleted. The removal goes
/// beneath `.apb` with no link followed at any component (`fsutil::beneath`
/// over the project's trigger state directory): a link at `log`, at the
/// journal or at `.apb` is refused, and what it points at stays as it is.
/// Whatever stays behind is the removal's answer, never its error (W5):
/// `Some(journal_left)` names the file and the reason.
fn delete_journal(root: &Path, incarnation: &str) -> Option<String> {
    let left = |why: String| {
        Some(format!(
            ".apb/trigger-state/log/{incarnation}.jsonl ({why})"
        ))
    };
    if !crate::registry::is_safe_segment(incarnation) {
        return left(format!("`{incarnation}` is not an incarnation"));
    }
    let beneath = match crate::fsutil::beneath::Beneath::open(&root.join(".apb")) {
        Ok(beneath) => beneath,
        // No `.apb` at all: no journal of this project was ever written
        // beneath it.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return left(e.to_string()),
    };
    match beneath.remove(&format!("trigger-state/log/{incarnation}.jsonl")) {
        Ok(()) => None,
        Err(e) => left(e.to_string()),
    }
}

/// What a removal answers: whether the rule was removed, and, when its
/// state entry or its journal could not be deleted beneath the anchor,
/// the `state_left` and `journal_left` sentences naming what stays
/// behind and why. Neither fails the removal: the rule is gone either
/// way (W5).
#[derive(Debug, Clone)]
pub struct RemoveOutcome {
    pub removed: bool,
    pub state_left: Option<String>,
    pub journal_left: Option<String>,
}

/// Any parse failure of the document, an unknown field among them, is one
/// V260 error issue carrying the parser's message: a rule with an error is
/// never saved, and the design's V260 covers a rule that cannot mean
/// anything.
fn parse_issue(error: impl std::fmt::Display) -> TriggerError {
    TriggerError::Invalid(vec![Issue::new(
        super::validate::V260,
        Severity::Error,
        None::<String>,
        format!("the rule document does not parse: {error}"),
    )])
}

/// The document as a `Rule`: read as a map first, so a document naming
/// `enabled` is refused before any rule field is read, then parsed through
/// a JSON-compatible value, whose enum form is the design's
/// `source: { inbox: { .. } }` for the YAML and the JSON document a
/// surface hands over alike (yaml_serde's document deserializer knows
/// enums only through YAML tags, the map form is what a person writes and
/// what the HTTP API serves).
pub fn parse_rule_document(doc: &str) -> Result<Rule, TriggerError> {
    let value: yaml_serde::Value = yaml_serde::from_str(doc).map_err(parse_issue)?;
    if value.get("enabled").is_some() {
        return Err(TriggerError::EnabledNotSettable);
    }
    let json = serde_json::to_value(plain(value)).map_err(parse_issue)?;
    serde_json::from_value(json).map_err(parse_issue)
}

/// The YAML value with every tag resolved: a tagged value becomes the
/// one-key map of its tag without the leading bang, the externally tagged
/// form an enum parses from. This is the shape `yaml_serde::to_string`
/// renders a variant tag back into.
fn plain(value: yaml_serde::Value) -> yaml_serde::Value {
    match value {
        yaml_serde::Value::Tagged(tagged) => {
            let tag = tagged.tag.to_string();
            let tag = tag.strip_prefix('!').unwrap_or(&tag).to_string();
            let mut map = yaml_serde::Mapping::new();
            map.insert(yaml_serde::Value::String(tag), plain(tagged.value));
            yaml_serde::Value::Mapping(map)
        }
        yaml_serde::Value::Sequence(seq) => {
            yaml_serde::Value::Sequence(seq.into_iter().map(plain).collect())
        }
        yaml_serde::Value::Mapping(map) => {
            yaml_serde::Value::Mapping(map.into_iter().map(|(k, v)| (plain(k), plain(v))).collect())
        }
        other => other,
    }
}

/// What `set` needs before it takes the locks: the parsed rule and the
/// validation warnings.
struct ParsedRule {
    rule: Rule,
    warnings: Vec<Issue>,
}

/// Validates the parsed rule (`check_rule` over `RuleContext::load`): any
/// error refuses with every issue, errors and warnings alike; warnings
/// come back with the outcome.
fn prepare_set(root: &Path, doc: &str) -> Result<ParsedRule, TriggerError> {
    let rule = parse_rule_document(doc)?;
    let issues =
        super::validate::check_rule(&rule, &super::validate::RuleContext::load(root, &rule));
    if issues.iter().any(|i| i.severity == Severity::Error) {
        return Err(TriggerError::Invalid(issues));
    }
    let warnings = issues
        .into_iter()
        .filter(|i| i.severity == Severity::Warning)
        .collect();
    Ok(ParsedRule { rule, warnings })
}

/// Validates (`check_rule` over `RuleContext::load`), then creates or
/// replaces the entry; the warnings come back with the outcome. `doc` is
/// the YAML or JSON document as given, read as a map first so a document
/// naming `enabled` is `EnabledNotSettable`.
pub fn set_rule(
    root: &Path,
    doc: &str,
    by: &str,
    now: crate::clock::Millis,
) -> Result<(SetOutcome, Vec<Issue>), TriggerError> {
    let parsed = prepare_set(root, doc)?;
    let project = crate::projects::registered_id(root).ok_or(TriggerError::Unregistered)?;
    let _lock = hold_rule_lock(root, &parsed.rule.name)?;
    commit_set(root, parsed, by, now, &project)
}

/// `set` under both rule locks the caller already holds.
pub fn set_rule_locked(
    root: &Path,
    doc: &str,
    by: &str,
    now: crate::clock::Millis,
    _lock: &RuleLock,
) -> Result<(SetOutcome, Vec<Issue>), TriggerError> {
    let parsed = prepare_set(root, doc)?;
    let project = crate::projects::registered_id(root).ok_or(TriggerError::Unregistered)?;
    commit_set(root, parsed, by, now, &project)
}

/// The one rules transaction of `set`, under both locks: a rule whose
/// digest did not change writes nothing; otherwise the rule is replaced
/// and its approval dropped, and a change of the source is a new creation,
/// incarnation included (I5).
fn commit_set(
    root: &Path,
    parsed: ParsedRule,
    by: &str,
    now: crate::clock::Millis,
    project: &str,
) -> Result<(SetOutcome, Vec<Issue>), TriggerError> {
    let name = parsed.rule.name.clone();
    let warnings = parsed.warnings;
    let outcome = crate::signed_store::transact(
        root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<(SetOutcome, bool), TriggerError> {
            // Every write stamps the project the file belongs to.
            file.project = project.to_string();
            match file.rules.iter().position(|e| e.rule.name == name) {
                Some(idx) => {
                    let old = &file.rules[idx];
                    // The digest covers the incarnation and the rule alone,
                    // so the placeholder bookkeeping fields never matter.
                    let probe = RuleEntry {
                        rule: parsed.rule.clone(),
                        incarnation: old.incarnation.clone(),
                        enabled: false,
                        approval: None,
                        created_at: 0,
                        updated_at: 0,
                        updated_by: String::new(),
                    };
                    if rule_digest(&probe) == rule_digest(old) {
                        return Ok((SetOutcome::Unchanged, false));
                    }
                    let source_changed =
                        source_id(&old.rule.source) != source_id(&parsed.rule.source);
                    let entry = RuleEntry {
                        rule: parsed.rule.clone(),
                        incarnation: if source_changed {
                            fresh_incarnation()
                        } else {
                            old.incarnation.clone()
                        },
                        enabled: old.enabled,
                        approval: None,
                        created_at: old.created_at,
                        updated_at: now.0,
                        updated_by: by.to_string(),
                    };
                    let approval_dropped = old.approval.is_some();
                    file.rules[idx] = entry;
                    Ok((SetOutcome::Updated { approval_dropped }, true))
                }
                None => {
                    file.rules.push(RuleEntry {
                        rule: parsed.rule.clone(),
                        incarnation: fresh_incarnation(),
                        enabled: true,
                        approval: None,
                        created_at: now.0,
                        updated_at: now.0,
                        updated_by: by.to_string(),
                    });
                    Ok((SetOutcome::Created, true))
                }
            }
        },
    )
    .map_err(store_error)?;
    Ok((outcome, warnings))
}

/// Whether a rule is enabled or disabled; enabling deletes the rule's
/// state entry first, so the next pass positions the rule at the head it
/// reads then (I5). Neither direction reads the inbox.
pub fn set_enabled(
    root: &Path,
    name: &str,
    enabled: bool,
    by: &str,
    now: crate::clock::Millis,
) -> Result<(), TriggerError> {
    let _lock = hold_rule_lock(root, name)?;
    set_enabled_locked(root, name, enabled, by, now, &_lock)
}

/// `enable` and `disable` under both rule locks the caller already holds.
pub fn set_enabled_locked(
    root: &Path,
    name: &str,
    enabled: bool,
    by: &str,
    now: crate::clock::Millis,
    _lock: &RuleLock,
) -> Result<(), TriggerError> {
    let project = crate::projects::registered_id(root).ok_or(TriggerError::Unregistered)?;
    let loaded = read_rules(root)?;
    if !loaded.file.rules.iter().any(|e| e.rule.name == name) {
        return Err(TriggerError::NotFound(name.to_string()));
    }
    if enabled {
        delete_state_entry(root, name, Some(&project))?;
        after_state_delete()?;
    }
    crate::signed_store::transact(
        root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<((), bool), TriggerError> {
            file.project = project.clone();
            let Some(entry) = file.rules.iter_mut().find(|e| e.rule.name == name) else {
                return Ok(((), false));
            };
            entry.enabled = enabled;
            entry.updated_at = now.0;
            entry.updated_by = by.to_string();
            Ok(((), true))
        },
    )
    .map_err(store_error)?;
    Ok(())
}

/// Stores a person's approval of the rule as it stands: the rule's state
/// entry is deleted first, so the rule acts only on what arrives once it
/// is approved (I5). No inbox read.
pub fn store_approval(
    root: &Path,
    name: &str,
    approval: Approval,
    now: crate::clock::Millis,
) -> Result<(), TriggerError> {
    let _lock = hold_rule_lock(root, name)?;
    store_approval_locked(root, name, approval, now, &_lock)
}

/// `approve` under both rule locks the caller already holds; what a
/// caller compares the approval against before calling this is the tools
/// layer's concern.
pub fn store_approval_locked(
    root: &Path,
    name: &str,
    approval: Approval,
    now: crate::clock::Millis,
    _lock: &RuleLock,
) -> Result<(), TriggerError> {
    let project = crate::projects::registered_id(root).ok_or(TriggerError::Unregistered)?;
    let loaded = read_rules(root)?;
    if !loaded.file.rules.iter().any(|e| e.rule.name == name) {
        return Err(TriggerError::NotFound(name.to_string()));
    }
    delete_state_entry(root, name, Some(&project))?;
    after_state_delete()?;
    let approved_by = approval.approved_by.clone();
    crate::signed_store::transact(
        root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<((), bool), TriggerError> {
            file.project = project.clone();
            let Some(entry) = file.rules.iter_mut().find(|e| e.rule.name == name) else {
                return Ok(((), false));
            };
            entry.approval = Some(approval);
            entry.updated_at = now.0;
            entry.updated_by = approved_by;
            Ok(((), true))
        },
    )
    .map_err(store_error)?;
    Ok(())
}

/// The entry, its state entry and its journal file. `false` when there was
/// none.
pub fn remove_rule(root: &Path, name: &str) -> Result<RemoveOutcome, TriggerError> {
    let _lock = hold_rule_lock(root, name)?;
    remove_rule_locked(root, name, &_lock)
}

/// `remove` under both rule locks the caller already holds: the entry
/// deleted in one rules transaction, then, still under the locks, the
/// rule's state entry deleted in one state transaction and its journal
/// file deleted.
pub fn remove_rule_locked(
    root: &Path,
    name: &str,
    _lock: &RuleLock,
) -> Result<RemoveOutcome, TriggerError> {
    let loaded = read_rules(root)?;
    let Some(entry) = loaded.file.rules.iter().find(|e| e.rule.name == name) else {
        return Ok(RemoveOutcome {
            removed: false,
            state_left: None,
            journal_left: None,
        });
    };
    let incarnation = entry.incarnation.clone();
    crate::signed_store::transact(
        root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<((), bool), TriggerError> {
            file.rules.retain(|e| e.rule.name != name);
            if let Some(id) = crate::projects::registered_id(root) {
                file.project = id;
            }
            Ok(((), true))
        },
    )
    .map_err(store_error)?;
    // The rules transaction committed: the state entry and the journal
    // are deleted now, and a failure over either is an outcome, never an
    // error (W5).
    let state_left =
        delete_state_entry_left(root, name, crate::projects::registered_id(root).as_deref());
    let journal_left = delete_journal(root, &incarnation);
    Ok(RemoveOutcome {
        removed: true,
        state_left,
        journal_left,
    })
}

/// Clears `approval` only while the entry still has that incarnation, that
/// `rule_digest` and an approval whose `binding` is `approved_binding`
/// (compare and clear; anything else is left as it is). The one write to
/// `rules.json` that is not a person's: it can only take an approval away.
/// Called under the sweep lock the pass holds.
pub fn revoke_approval_locked(
    root: &Path,
    name: &str,
    incarnation: &str,
    digest: &str,
    approved_binding: &str,
) -> Result<(), TriggerError> {
    crate::signed_store::transact(
        root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<((), bool), TriggerError> {
            if let Some(id) = crate::projects::registered_id(root) {
                file.project = id;
            }
            let Some(entry) = file
                .rules
                .iter_mut()
                .find(|e| e.rule.name == name && e.incarnation == incarnation)
            else {
                return Ok(((), false));
            };
            let stale = rule_digest(entry) == digest
                && entry
                    .approval
                    .as_ref()
                    .is_some_and(|a| a.binding == approved_binding);
            if stale {
                entry.approval = None;
                return Ok(((), true));
            }
            Ok(((), false))
        },
    )
    .map_err(store_error)?;
    Ok(())
}

// The fault point the crash tests use: right after a mutation deleted the
// rule's state entry and before the rules write, where a power cut leaves
// an unapproved or disabled rule without state, which nothing sweeps (I5).
// Outside test builds the point is a plain Ok (the
// `versioning/current/fault.rs` pattern).
#[cfg(test)]
thread_local! {
    static FAIL_AFTER_STATE_DELETE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the next mutation on this thread fail right after it deletes the
/// rule's state entry (`false` clears it again).
#[cfg(test)]
pub(crate) fn fail_after_state_delete(fail: bool) {
    FAIL_AFTER_STATE_DELETE.with(|f| f.set(fail));
}

#[cfg(test)]
fn after_state_delete() -> Result<(), TriggerError> {
    if FAIL_AFTER_STATE_DELETE.with(std::cell::Cell::get) {
        return Err(TriggerError::Store(crate::signed_store::StoreError::Io(
            std::io::Error::other("injected fault after the state deletion"),
        )));
    }
    Ok(())
}

#[cfg(not(test))]
fn after_state_delete() -> Result<(), TriggerError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetRequest, OnNoEstimate};
    use crate::clock::Millis;
    use crate::connector::event_filter::BodyCondition;
    use crate::scope::Scope;
    use crate::signed_store::{self, SignedDoc, StoreError, Unsigned};
    use crate::trigger::state::{TriggerStateFile, position_at_head};
    use crate::trigger::validate::V260;
    use crate::validate::Severity;
    use std::collections::BTreeMap;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// The rule's name in every fixture document.
    const NAME: &str = "triage-new-issues";

    /// The connector manifest of the fixture, with a webhook block naming
    /// the event type (the shape `validate.rs`'s tests parse).
    const GITHUB_YAML: &str = r#"
name: github
version: 1.0.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Signature
    secret: "{{secret.app_secret}}"
  event_type_header: X-GitHub-Event
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
"#;

    /// A second connector, so a source change stays a valid document.
    const GITLAB_YAML: &str = r#"
name: gitlab
version: 1.0.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Signature
    secret: "{{secret.app_secret}}"
  event_type_header: X-GitLab-Event
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
"#;

    /// The pinned playbook of the fixture: no connector bindings, one
    /// untrusted text param the rule maps, so validation is clean.
    const MINI_YAML: &str = r#"
schema: 2
id: mini
name: Mini
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: act, type: agent_task, prompt: do it }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: act }
  - { from: act, to: done }
params:
  - { name: title, type: text, untrusted: true }
"#;

    /// The rule as a person writes it, the design's example adapted to the
    /// fixture's playbook and connector.
    fn base_doc() -> String {
        r#"
name: triage-new-issues
playbook: { id: mini, version: 1.0.0 }
source:
  inbox:
    connector: github
    account: acme
    types: [issues]
    where:
      - { path: action, equals: opened }
    key: "{{body.repository.full_name}}#{{body.issue.number}}"
params:
  title: "{{body.issue.title}}"
limits: { max_per_hour: 5 }
budget: { tokens: 100000, on_no_estimate: continue }
"#
        .to_string()
    }

    /// The same rule with every mapping's keys in another order: the
    /// document a person hands over must not matter, only the rule.
    fn reordered_doc() -> String {
        r#"
budget: { on_no_estimate: continue, tokens: 100000 }
limits: { max_per_hour: 5 }
params:
  title: "{{body.issue.title}}"
source:
  inbox:
    key: "{{body.repository.full_name}}#{{body.issue.number}}"
    where:
      - { equals: opened, path: action }
    types: [issues]
    account: acme
    connector: github
playbook: { scope: project, version: 1.0.0, id: mini }
name: triage-new-issues
"#
        .to_string()
    }

    /// The entry `base_doc` writes, built directly: what the pure tests
    /// (digest, revocation) work over.
    fn digest_entry() -> RuleEntry {
        RuleEntry {
            rule: Rule {
                name: NAME.to_string(),
                playbook: RulePlaybook {
                    id: "mini".to_string(),
                    version: "1.0.0".to_string(),
                    scope: RuleScope::Project,
                },
                source: RuleSource::Inbox(InboxSource {
                    connector: "github".to_string(),
                    account: "acme".to_string(),
                    types: vec!["issues".to_string()],
                    conditions: vec![BodyCondition {
                        path: "action".to_string(),
                        equals: serde_json::Value::String("opened".to_string()),
                    }],
                    key: "{{body.repository.full_name}}#{{body.issue.number}}".to_string(),
                }),
                params: BTreeMap::from([("title".to_string(), "{{body.issue.title}}".to_string())]),
                limits: RuleLimits { max_per_hour: 5 },
                budget: Some(BudgetRequest {
                    tokens: Some(100_000),
                    usd: None,
                    on_no_estimate: Some(OnNoEstimate::Continue),
                }),
                external_data: true,
                require_ready: true,
                notify: None,
            },
            incarnation: "inc1".to_string(),
            enabled: true,
            approval: None,
            created_at: 0,
            updated_at: 0,
            updated_by: String::new(),
        }
    }

    /// A full approval of `entry`, every field set.
    fn approval_for(entry: &RuleEntry, at: u64) -> Approval {
        Approval {
            rule_digest: rule_digest(entry),
            playbook_digest: "sha256:playbook".to_string(),
            binding: "binding-1".to_string(),
            permit_digest: "permit-1".to_string(),
            launch_policy: "allow".to_string(),
            consent_nonce: Some("nonce-1".to_string()),
            approved_at: at,
            approved_by: "terminal".to_string(),
        }
    }

    /// The rules file path of the project at `root`.
    fn rules_path(root: &Path) -> PathBuf {
        signed_store::file::<RulesFile>(root, Scope::Project, "rules")
    }

    /// The verified rules file of the project at `root`.
    fn read_rules_file(root: &Path) -> RulesFile {
        let loaded = signed_store::read::<RulesFile>(root, Scope::Project, "rules").unwrap();
        assert!(loaded.signed, "the rules file verifies for this project");
        loaded.file
    }

    /// Whether the state still carries an entry for the rule.
    fn state_has_entry(root: &Path, name: &str) -> bool {
        let loaded = signed_store::read::<TriggerStateFile>(root, Scope::Project, "state").unwrap();
        loaded.file.rules.iter().any(|s| s.rule == name)
    }

    /// Whether the error is the test's injected fault.
    fn is_injected_fault(err: &TriggerError) -> bool {
        matches!(
            err,
            TriggerError::Store(StoreError::Io(e)) if e.to_string().contains("injected fault")
        )
    }

    /// Writes `file` over the rules path, signed with this installation's
    /// key: the shape of a file another project's store wrote.
    fn write_signed_rules(root: &Path, file: &RulesFile) {
        let path = rules_path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut value = serde_json::to_value(file).unwrap();
        let covered = signed_store::bytes_of(&value).unwrap();
        let mac = crate::run_origin::tag(RulesFile::MAC_DOMAIN, &covered).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("mac".into(), serde_json::Value::String(mac));
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    /// Writes `entry` into the signed rules store of the project at `root`.
    fn write_entry(root: &Path, entry: RuleEntry) {
        let project = crate::projects::registered_id(root).unwrap();
        signed_store::transact(
            root,
            Scope::Project,
            "rules",
            Unsigned::Replace,
            |file: &mut RulesFile| -> Result<((), bool), TriggerError> {
                file.project = project.clone();
                file.rules.retain(|e| e.rule.name != entry.rule.name);
                file.rules.push(entry.clone());
                Ok(((), true))
            },
        )
        .unwrap();
    }

    /// A config dir of the test's own and the env lock held for the
    /// fixture's whole life (the pattern of `validate.rs`'s load test).
    fn config_dir() -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
        Option<std::ffi::OsString>,
    ) {
        let lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("APB_CONFIG_DIR");
        // SAFETY: env mutation is serialized by the lock, held until drop.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        (lock, cfg, prev)
    }

    /// The full fixture: two installed connectors with their global
    /// accounts, the playbook `mini` pinned at 1.0.0 in the project store,
    /// and the project registered (CI turns `projects::touch` off). A rule
    /// saved here went through `check_rule` over `RuleContext::load` for
    /// real, so an outcome is not an artifact of a stub context.
    struct Fixture {
        _lock: std::sync::MutexGuard<'static, ()>,
        cfg: tempfile::TempDir,
        prev: Option<std::ffi::OsString>,
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new(registered: bool) -> Self {
            let (_lock, cfg, prev) = config_dir();
            let root = tempfile::tempdir().unwrap();
            for (name, yaml) in [("github", GITHUB_YAML), ("gitlab", GITLAB_YAML)] {
                let cdir = cfg.path().join("connectors").join(name);
                std::fs::create_dir_all(&cdir).unwrap();
                std::fs::write(cdir.join("connector.yaml"), yaml).unwrap();
                let accdir = cfg.path().join("connector-config");
                std::fs::create_dir_all(&accdir).unwrap();
                std::fs::write(
                    accdir.join(format!("{name}.yaml")),
                    "accounts:\n  - name: acme\n    app_secret: \"{{env.S}}\"\n",
                )
                .unwrap();
            }
            let vdir = root
                .path()
                .join(".apb/playbooks")
                .join("mini")
                .join("1.0.0");
            std::fs::create_dir_all(&vdir).unwrap();
            std::fs::write(vdir.join("playbook.yaml"), MINI_YAML).unwrap();
            std::fs::write(
                root.path()
                    .join(".apb/playbooks")
                    .join("mini")
                    .join("current"),
                "1.0.0",
            )
            .unwrap();
            if registered {
                crate::projects::bind_id(root.path()).unwrap();
            }
            Self {
                _lock,
                cfg,
                prev,
                root,
            }
        }

        fn path(&self) -> &Path {
            self.root.path()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // SAFETY: still under the lock (the guard drops after this body).
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                    None => std::env::remove_var("APB_CONFIG_DIR"),
                }
            }
        }
    }

    /// The document round trips: parse, write, parse is the same rule, and
    /// the defaults a person relies on are what the design fixes. Both
    /// parses go through the document parser `set` uses.
    #[test]
    fn a_rule_round_trips_through_yaml() {
        let rule = parse_rule_document(&base_doc()).unwrap();
        assert_eq!(rule.name, NAME);
        assert_eq!(rule.playbook.scope, RuleScope::Project, "the default scope");
        assert!(rule.external_data, "the default");
        assert!(rule.require_ready, "the default");
        assert_eq!(rule.limits.max_per_hour, 5);
        assert_eq!(
            rule.params.get("title").map(String::as_str),
            Some("{{body.issue.title}}")
        );
        let again = parse_rule_document(&yaml_serde::to_string(&rule).unwrap()).unwrap();
        assert_eq!(rule, again);
    }

    /// A field `Rule` does not know is a refusal, never a silent drop, and
    /// nothing is written.
    #[test]
    fn an_unknown_field_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let doc = format!("{}\nattended: true\n", base_doc());
        let err = set_rule(root.path(), &doc, "terminal", Millis(1)).unwrap_err();
        match err {
            TriggerError::Invalid(issues) => {
                assert_eq!(issues.len(), 1, "{issues:?}");
                assert_eq!(issues[0].code, V260);
                assert_eq!(issues[0].severity, Severity::Error);
                assert!(
                    issues[0].message.contains("attended"),
                    "{}",
                    issues[0].message
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        assert!(
            !rules_path(root.path()).exists(),
            "a rule with an error is never saved"
        );
        // An unknown field nested in the source is refused the same way.
        let doc = base_doc().replace("account: acme", "account: acme\n    ghost: 1");
        let err = set_rule(root.path(), &doc, "terminal", Millis(1)).unwrap_err();
        assert!(
            matches!(err, TriggerError::Invalid(ref i) if i.iter().any(|i| i.message.contains("ghost"))),
            "{err:?}"
        );
    }

    /// A rule is enabled or disabled with `apb trigger enable|disable`,
    /// never through `set`, on a new rule and on an existing one, and the
    /// refusal writes nothing.
    #[test]
    fn set_refuses_a_document_naming_enabled() {
        let fx = Fixture::new(true);
        for value in ["true", "false"] {
            let doc = format!("{}\nenabled: {value}\n", base_doc());
            let err = set_rule(fx.path(), &doc, "terminal", Millis(1)).unwrap_err();
            assert!(matches!(err, TriggerError::EnabledNotSettable), "{err:?}");
        }
        assert!(
            !rules_path(fx.path()).exists(),
            "nothing written for a new rule"
        );
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let doc = format!("{}\nenabled: true\n", base_doc());
        let err = set_rule(fx.path(), &doc, "terminal", Millis(2)).unwrap_err();
        assert!(matches!(err, TriggerError::EnabledNotSettable), "{err:?}");
        let file = read_rules_file(fx.path());
        assert_eq!(file.rules.len(), 1);
        assert_eq!(file.rules[0].updated_at, 1, "the refusal changed nothing");
    }

    /// Every creation mints its own incarnation: 32 lowercase hex, new
    /// after a remove, and never shared with another rule.
    #[test]
    fn each_creation_gets_its_own_incarnation() {
        let fx = Fixture::new(true);
        let (outcome, _) = set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        assert_eq!(outcome, SetOutcome::Created);
        let first = read_rules_file(fx.path()).rules[0].incarnation.clone();
        assert_eq!(first.len(), 32);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "32 lowercase hex: {first}"
        );
        // Remove and create again: another incarnation.
        assert!(remove_rule(fx.path(), NAME).unwrap().removed);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(2)).unwrap();
        let second = read_rules_file(fx.path()).rules[0].incarnation.clone();
        assert_ne!(first, second);
        // A second rule has its own.
        let other = base_doc().replace(NAME, "stale-closer");
        set_rule(fx.path(), &other, "terminal", Millis(3)).unwrap();
        let file = read_rules_file(fx.path());
        assert_eq!(file.rules.len(), 2);
        let third = &file
            .rules
            .iter()
            .find(|e| e.rule.name == "stale-closer")
            .unwrap();
        assert_ne!(third.incarnation, first);
        assert_ne!(third.incarnation, second);
    }

    /// The digest covers every field that changes whether or how the rule
    /// fires, and only those: key order of the document does not, and
    /// neither do the entry's bookkeeping fields.
    #[test]
    fn rule_digest_covers_every_firing_field() {
        // The document and the hand-built entry agree, so both shapes are
        // under test.
        let parsed = parse_rule_document(&base_doc()).unwrap();
        assert_eq!(parsed, digest_entry().rule);
        let base = digest_entry();
        let digest = rule_digest(&base);
        assert_eq!(digest.len(), 64, "the full sha256, lowercase hex: {digest}");
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(rule_digest(&base), digest, "stable across calls");
        fn source(e: &mut RuleEntry) -> &mut InboxSource {
            match &mut e.rule.source {
                RuleSource::Inbox(source) => source,
            }
        }
        /// One field change under test: its name and its mutation.
        type FieldCase<'a> = (&'a str, Box<dyn Fn(&mut RuleEntry)>);
        let cases: Vec<FieldCase> = vec![
            (
                "name",
                Box::new(|e: &mut RuleEntry| e.rule.name = "renamed".into()),
            ),
            (
                "playbook id",
                Box::new(|e: &mut RuleEntry| e.rule.playbook.id = "other".into()),
            ),
            (
                "playbook version",
                Box::new(|e: &mut RuleEntry| e.rule.playbook.version = "1.1.0".into()),
            ),
            (
                "playbook scope",
                Box::new(|e: &mut RuleEntry| e.rule.playbook.scope = RuleScope::Global),
            ),
            (
                "connector",
                Box::new(move |e: &mut RuleEntry| source(e).connector = "gitlab".into()),
            ),
            (
                "account",
                Box::new(move |e: &mut RuleEntry| source(e).account = "other".into()),
            ),
            (
                "types",
                Box::new(move |e: &mut RuleEntry| source(e).types = vec!["pull_request".into()]),
            ),
            (
                "where",
                Box::new(move |e: &mut RuleEntry| {
                    source(e).conditions.push(BodyCondition {
                        path: "assignee".into(),
                        equals: serde_json::Value::String("apb".into()),
                    })
                }),
            ),
            (
                "key",
                Box::new(move |e: &mut RuleEntry| source(e).key = "{{ event.id }}".into()),
            ),
            (
                "param value",
                Box::new(|e: &mut RuleEntry| {
                    e.rule
                        .params
                        .insert("title".into(), "{{body.issue.state}}".into());
                }),
            ),
            (
                "param added",
                Box::new(|e: &mut RuleEntry| {
                    e.rule.params.insert("extra".into(), "static".into());
                }),
            ),
            (
                "limits",
                Box::new(|e: &mut RuleEntry| e.rule.limits.max_per_hour = 6),
            ),
            (
                "budget",
                Box::new(|e: &mut RuleEntry| e.rule.budget.as_mut().unwrap().tokens = Some(1)),
            ),
            (
                "external_data",
                Box::new(|e: &mut RuleEntry| e.rule.external_data = false),
            ),
            (
                "require_ready",
                Box::new(|e: &mut RuleEntry| e.rule.require_ready = false),
            ),
            (
                "notify",
                Box::new(|e: &mut RuleEntry| e.rule.notify = Some(false)),
            ),
            (
                "incarnation",
                Box::new(|e: &mut RuleEntry| e.incarnation = "inc2".into()),
            ),
        ];
        for (field, change) in cases {
            let mut e = base.clone();
            change(&mut e);
            assert_ne!(rule_digest(&e), digest, "the {field} changes the digest");
        }
        // Key order of the document does not, and the entry's bookkeeping
        // fields do not either: only the incarnation and the rule count.
        let a = parse_rule_document(&base_doc()).unwrap();
        let b = parse_rule_document(&reordered_doc()).unwrap();
        assert_eq!(a, b);
        let ea = RuleEntry {
            rule: a,
            incarnation: "inc1".into(),
            ..base
        };
        let eb = RuleEntry {
            rule: b,
            incarnation: "inc1".into(),
            enabled: false,
            approval: None,
            created_at: 9,
            updated_at: 9,
            updated_by: "dashboard_session".into(),
        };
        assert_eq!(rule_digest(&ea), rule_digest(&eb));
    }

    /// A set that changes the rule drops the approval; the source change
    /// case is `a_source_change_gives_a_new_incarnation`.
    #[test]
    fn a_changed_rule_drops_its_approval() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let first = read_rules_file(fx.path()).rules[0].clone();
        let digest_before = rule_digest(&first);
        store_approval(fx.path(), NAME, approval_for(&first, 5), Millis(5)).unwrap();
        // An edit that keeps the source: the incarnation stays.
        let doc = base_doc().replace("max_per_hour: 5", "max_per_hour: 6");
        let (outcome, _) = set_rule(fx.path(), &doc, "terminal", Millis(6)).unwrap();
        assert_eq!(
            outcome,
            SetOutcome::Updated {
                approval_dropped: true
            }
        );
        let entry = read_rules_file(fx.path()).rules[0].clone();
        assert_eq!(entry.approval, None, "the approval dropped");
        assert_eq!(entry.incarnation, first.incarnation);
        assert_ne!(rule_digest(&entry), digest_before);
        assert_eq!(entry.updated_at, 6);
        assert_eq!(entry.updated_by, "terminal");
    }

    /// A set of the same rule writes nothing at all: the approval and the
    /// file's bytes stay as they were.
    #[test]
    fn an_unchanged_set_keeps_its_approval() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let first = read_rules_file(fx.path()).rules[0].clone();
        store_approval(fx.path(), NAME, approval_for(&first, 5), Millis(5)).unwrap();
        let before = std::fs::read(rules_path(fx.path())).unwrap();
        let (outcome, _) = set_rule(fx.path(), &base_doc(), "terminal", Millis(7)).unwrap();
        assert_eq!(outcome, SetOutcome::Unchanged);
        assert_eq!(
            std::fs::read(rules_path(fx.path())).unwrap(),
            before,
            "an unchanged set writes nothing"
        );
        let entry = read_rules_file(fx.path()).rules[0].clone();
        assert_eq!(entry.approval, Some(approval_for(&first, 5)));
        assert_eq!(entry.updated_at, 5, "the approval's write is the last one");
    }

    /// A new rule is enabled but not approved: before its first approval it
    /// starts nothing.
    #[test]
    fn a_new_rule_is_not_approved() {
        let fx = Fixture::new(true);
        let (outcome, warnings) = set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        assert_eq!(outcome, SetOutcome::Created);
        assert!(
            warnings.is_empty(),
            "the fixture rule is clean: {warnings:?}"
        );
        let entry = read_rules_file(fx.path()).rules[0].clone();
        assert!(entry.enabled);
        assert_eq!(entry.approval, None);
        assert_eq!(entry.created_at, 1);
        assert_eq!(entry.updated_at, 1);
        assert_eq!(entry.updated_by, "terminal");
    }

    /// The state entry is deleted before the rules write, for `approve`
    /// and for `enable`: a crash at that point leaves an unapproved or
    /// disabled rule without state, which nothing sweeps (I5). No inbox
    /// exists in this fixture, so either command reading it would fail
    /// instead of reaching the fault.
    #[test]
    fn approve_and_enable_delete_the_state_before_the_rules_write() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let entry = read_rules_file(fx.path()).rules[0].clone();
        position_at_head(fx.path(), &entry, 9, 10).unwrap();
        assert!(state_has_entry(fx.path(), NAME));
        assert!(
            !fx.cfg.path().join("connector-inbox").exists(),
            "no inbox at all: a read would fail"
        );

        // approve dies after the state deletion: no state, still unapproved.
        fail_after_state_delete(true);
        let err =
            store_approval(fx.path(), NAME, approval_for(&entry, 11), Millis(11)).unwrap_err();
        fail_after_state_delete(false);
        assert!(is_injected_fault(&err), "{err:?}");
        assert!(!state_has_entry(fx.path(), NAME), "the state entry is gone");
        assert_eq!(read_rules_file(fx.path()).rules[0].approval, None);

        // disable, position again, then enable dies the same way: no state,
        // still disabled.
        set_enabled(fx.path(), NAME, false, "terminal", Millis(12)).unwrap();
        position_at_head(fx.path(), &entry, 12, 13).unwrap();
        assert!(state_has_entry(fx.path(), NAME));
        fail_after_state_delete(true);
        let err = set_enabled(fx.path(), NAME, true, "terminal", Millis(14)).unwrap_err();
        fail_after_state_delete(false);
        assert!(is_injected_fault(&err), "{err:?}");
        assert!(!state_has_entry(fx.path(), NAME), "the state entry is gone");
        assert!(
            !read_rules_file(fx.path()).rules[0].enabled,
            "still disabled"
        );
    }

    /// A change of the source gives a new incarnation and drops the
    /// approval; an edit of any other field keeps the incarnation.
    #[test]
    fn a_source_change_gives_a_new_incarnation() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let first = read_rules_file(fx.path()).rules[0].clone();
        store_approval(fx.path(), NAME, approval_for(&first, 5), Millis(5)).unwrap();
        position_at_head(fx.path(), &first, 2, 3).unwrap();

        // A params edit keeps the incarnation, the approval goes.
        let doc = base_doc().replace("{{body.issue.title}}", "{{body.issue.state}}");
        let (outcome, _) = set_rule(fx.path(), &doc, "terminal", Millis(6)).unwrap();
        assert_eq!(
            outcome,
            SetOutcome::Updated {
                approval_dropped: true
            }
        );
        let entry = read_rules_file(fx.path()).rules[0].clone();
        assert_eq!(entry.incarnation, first.incarnation, "no source change");
        assert_eq!(entry.approval, None);
        // The old state entry is inert but kept: `set` never deletes it,
        // `approve` replaces it.
        let loaded =
            signed_store::read::<TriggerStateFile>(fx.path(), Scope::Project, "state").unwrap();
        assert_eq!(loaded.file.rules.len(), 1);
        assert_eq!(loaded.file.rules[0].rule, NAME);
        assert_eq!(loaded.file.rules[0].incarnation, first.incarnation);

        // A connector change is a new creation: a new incarnation.
        store_approval(fx.path(), NAME, approval_for(&entry, 7), Millis(7)).unwrap();
        let doc = base_doc().replace("connector: github", "connector: gitlab");
        let (outcome, _) = set_rule(fx.path(), &doc, "terminal", Millis(8)).unwrap();
        assert_eq!(
            outcome,
            SetOutcome::Updated {
                approval_dropped: true
            }
        );
        let entry = read_rules_file(fx.path()).rules[0].clone();
        assert_ne!(entry.incarnation, first.incarnation, "a new incarnation");
        assert_eq!(entry.approval, None);
        // The second approve deleted the old incarnation's state entry
        // (every approve does); the next pass positions the new one.
        let loaded =
            signed_store::read::<TriggerStateFile>(fx.path(), Scope::Project, "state").unwrap();
        assert!(loaded.file.rules.is_empty());
    }

    /// The stored approval is exactly what was handed over, binding
    /// included, and the entry names the change.
    #[test]
    fn an_approval_carries_the_binding() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let entry = read_rules_file(fx.path()).rules[0].clone();
        let want = approval_for(&entry, 5);
        store_approval(fx.path(), NAME, want.clone(), Millis(5)).unwrap();
        let got = read_rules_file(fx.path()).rules[0]
            .approval
            .clone()
            .unwrap();
        assert_eq!(got, want);
        assert_eq!(
            got.rule_digest,
            rule_digest(&entry),
            "of the rule it approved"
        );
        assert_eq!(got.binding, "binding-1");
        assert_eq!(got.playbook_digest, "sha256:playbook");
        assert_eq!(got.permit_digest, "permit-1");
        assert_eq!(got.launch_policy, "allow");
        assert_eq!(got.consent_nonce.as_deref(), Some("nonce-1"));
        assert_eq!(got.approved_at, 5);
        assert_eq!(got.approved_by, "terminal");
        let entry = read_rules_file(fx.path()).rules[0].clone();
        assert_eq!(entry.updated_at, 5);
        assert_eq!(entry.updated_by, "terminal");
    }

    /// The revocation clears only the approval it names: another
    /// incarnation, another digest or another approved binding is left as
    /// it is. It runs under the locks the caller holds, over a signed
    /// store written directly.
    #[test]
    fn revoke_clears_only_the_stale_approval() {
        let _cfg = config_dir();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        let entry = digest_entry();
        let mut stored = entry.clone();
        stored.approval = Some(approval_for(&entry, 1));
        write_entry(root.path(), stored);
        let digest = rule_digest(&entry);
        let approved = approval_for(&entry, 1);

        // Another incarnation: left as it is.
        revoke_approval_locked(root.path(), NAME, "inc2", &digest, &approved.binding).unwrap();
        assert_eq!(
            read_rules_file(root.path()).rules[0].approval,
            Some(approved.clone()),
            "another incarnation revokes nothing"
        );
        // Another digest: left as it is.
        revoke_approval_locked(root.path(), NAME, "inc1", "other-digest", &approved.binding)
            .unwrap();
        assert_eq!(
            read_rules_file(root.path()).rules[0].approval,
            Some(approved.clone())
        );
        // Another approved binding: left as it is.
        revoke_approval_locked(root.path(), NAME, "inc1", &digest, "binding-2").unwrap();
        assert_eq!(
            read_rules_file(root.path()).rules[0].approval,
            Some(approved.clone())
        );
        // The exact incarnation, digest and binding: cleared, the rest of
        // the entry intact.
        revoke_approval_locked(root.path(), NAME, "inc1", &digest, &approved.binding).unwrap();
        let entry = read_rules_file(root.path()).rules[0].clone();
        assert_eq!(entry.approval, None);
        assert_eq!(entry.incarnation, "inc1");
        assert!(entry.enabled);
        // Gone is gone: a second revocation is a no-op that answers Ok.
        revoke_approval_locked(root.path(), NAME, "inc1", &digest, &approved.binding).unwrap();
        assert_eq!(read_rules_file(root.path()).rules[0].approval, None);
    }

    /// A disable, then a set: the rule stays disabled, through an unchanged
    /// set and through an edit.
    #[test]
    fn a_disable_then_set_keeps_the_rule_disabled() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        set_enabled(fx.path(), NAME, false, "terminal", Millis(2)).unwrap();
        assert!(!read_rules_file(fx.path()).rules[0].enabled);
        let (outcome, _) = set_rule(fx.path(), &base_doc(), "terminal", Millis(3)).unwrap();
        assert_eq!(outcome, SetOutcome::Unchanged);
        assert!(!read_rules_file(fx.path()).rules[0].enabled);
        let doc = base_doc().replace("max_per_hour: 5", "max_per_hour: 6");
        let (outcome, _) = set_rule(fx.path(), &doc, "terminal", Millis(4)).unwrap();
        assert_eq!(
            outcome,
            SetOutcome::Updated {
                approval_dropped: false
            }
        );
        assert!(!read_rules_file(fx.path()).rules[0].enabled);
    }

    /// A mutation waits for the rule's two locks before it writes: held by
    /// another owner, `set` goes through only once they are released, and
    /// the lock files sit where a pass probes them.
    #[test]
    fn a_mutation_waits_for_the_rule_locks() {
        let fx = Fixture::new(true);
        let dir = fx.path().join(".apb").join("trigger-state");
        let held = dir.clone();
        let (tx, rx) = mpsc::channel();
        let holder = std::thread::spawn(move || {
            let change = crate::fsutil::hold_lock_waiting(
                &held,
                "triage-new-issues.change.lock",
                Duration::from_secs(5),
            )
            .unwrap();
            let sweep = crate::fsutil::hold_lock_waiting(
                &held,
                "triage-new-issues.sweep.lock",
                Duration::from_secs(5),
            )
            .unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(400));
            drop(change);
            drop(sweep);
        });
        rx.recv().unwrap();
        assert!(dir.join("triage-new-issues.change.lock").exists());
        assert!(dir.join("triage-new-issues.sweep.lock").exists());
        let started = Instant::now();
        let (outcome, _) = set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let waited = started.elapsed();
        assert_eq!(outcome, SetOutcome::Created);
        assert!(
            waited >= Duration::from_millis(300),
            "the mutation waited for the holder: {waited:?}"
        );
        holder.join().unwrap();
        // The same locks, taken by the mutation's own path once they are free.
        let _lock = hold_rule_lock(fx.path(), NAME).unwrap();
    }

    /// A project the registry does not know saves no rule: no listener
    /// would sweep it.
    #[test]
    fn an_unregistered_project_cannot_save_a_rule() {
        let fx = Fixture::new(false);
        let err = set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap_err();
        assert!(matches!(err, TriggerError::Unregistered), "{err:?}");
        assert!(!rules_path(fx.path()).exists(), "nothing written");
    }

    /// A rules file another project's store wrote does not belong here: it
    /// reads unsigned and every write refuses it, leaving it untouched.
    #[test]
    fn a_rules_file_of_another_project_is_refused() {
        let fx = Fixture::new(true);
        let other = tempfile::tempdir().unwrap();
        let other_id = crate::projects::bind_id(other.path()).unwrap();
        let mut forged = RulesFile::fresh(Scope::Project, "rules");
        forged.project = other_id;
        forged.rules.push(digest_entry());
        write_signed_rules(fx.path(), &forged);
        let before = std::fs::read(rules_path(fx.path())).unwrap();
        let err = set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap_err();
        assert!(
            matches!(err, TriggerError::Store(StoreError::Unverified(_))),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read(rules_path(fx.path())).unwrap(),
            before,
            "the forged file is untouched"
        );
        assert!(
            !signed_store::read::<RulesFile>(fx.path(), Scope::Project, "rules")
                .unwrap()
                .signed,
            "it reads unsigned: nothing starts from it"
        );
    }

    /// A rules file reached through a link is refused like a forged one.
    #[test]
    fn a_linked_rules_file_is_refused() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let path = rules_path(fx.path());
        let real = fx.path().join(".apb/triggers/project/rules.real.json");
        std::fs::rename(&path, &real).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        let before = std::fs::read(&real).unwrap();
        let doc = base_doc().replace("max_per_hour: 5", "max_per_hour: 6");
        let err = set_rule(fx.path(), &doc, "terminal", Millis(2)).unwrap_err();
        assert!(
            matches!(err, TriggerError::Store(StoreError::Unverified(_))),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            before,
            "the target is untouched"
        );
    }

    /// A name that is not one rule name never reaches the filesystem: no
    /// lock file is created outside `.apb/trigger-state`, on any mutation
    /// path, and the answer is that there is no such rule.
    #[test]
    fn a_name_that_climbs_out_creates_no_lock_file() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let outside = fx.path().join("outside.change.lock");
        let bad = "../../outside";
        let approval = approval_for(&read_rules_file(fx.path()).rules[0], 2);
        let results = [
            set_enabled(fx.path(), bad, false, "terminal", Millis(2)).err(),
            store_approval(fx.path(), bad, approval, Millis(2)).err(),
            remove_rule(fx.path(), bad).err(),
            hold_rule_lock(fx.path(), bad).err(),
            hold_rule_lock(fx.path(), "").err(),
            hold_rule_lock(fx.path(), "UPPER").err(),
        ];
        for result in results {
            assert!(
                matches!(result, Some(TriggerError::NotFound(_))),
                "{result:?}"
            );
        }
        assert!(!outside.exists(), "no lock file outside the trigger state");
        assert!(
            !fx.path().join(".apb").join("outside.sweep.lock").exists()
                && !fx.path().join("outside.sweep.lock").exists(),
            "no sweep lock either"
        );
    }

    /// `remove` never follows a link at the journal's directory: the file
    /// the link points at, another journal's, stays exactly as it is, and
    /// the removal still succeeds (W5), answering `journal_left` with the
    /// path that stays behind and the reason.
    #[cfg(unix)]
    #[test]
    fn a_remove_with_a_link_at_log_still_removes() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let entry = read_rules_file(fx.path()).rules[0].clone();
        position_at_head(fx.path(), &entry, 3, 4).unwrap();
        let log = fx.path().join(".apb").join("trigger-state").join("log");
        let journal = log.join(format!("{}.jsonl", entry.incarnation));
        assert!(journal.is_file(), "the pass left a journal");
        // `log` becomes a link to another directory holding a journal of
        // the same incarnation name.
        let elsewhere = tempfile::tempdir().unwrap();
        let other_log = elsewhere.path().join("log");
        std::fs::create_dir_all(&other_log).unwrap();
        std::fs::rename(&log, other_log.join("log")).unwrap();
        std::os::unix::fs::symlink(other_log.join("log"), &log).unwrap();
        let other_journal = other_log
            .join("log")
            .join(format!("{}.jsonl", entry.incarnation));
        let before = std::fs::read(&other_journal).unwrap();
        let outcome = remove_rule(fx.path(), NAME).unwrap();
        assert!(outcome.removed, "the rule is removed: {outcome:?}");
        let left = outcome.journal_left.expect("the journal stays behind");
        assert!(
            left.starts_with(".apb/trigger-state/log/")
                && left.contains(&format!("{}.jsonl (", entry.incarnation)),
            "{left}"
        );
        assert!(
            read_rules_file(fx.path()).rules.is_empty(),
            "the entry is gone from rules.json"
        );
        assert!(!state_has_entry(fx.path(), NAME), "the state entry is gone");
        assert_eq!(
            std::fs::read(&other_journal).unwrap(),
            before,
            "the other journal is untouched"
        );
    }

    /// The removal's answer names a journal it could not reach because the
    /// whole `.apb` anchor is a link: the rule is still removed.
    #[cfg(unix)]
    #[test]
    fn a_journal_deletion_names_a_link_at_the_anchor() {
        let fx = Fixture::new(true);
        let elsewhere = tempfile::tempdir().unwrap();
        let anchor = fx.path().join(".apb");
        std::fs::rename(&anchor, elsewhere.path().join("apb-real")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("apb-real"), &anchor).unwrap();
        let left = delete_journal(fx.path(), "inc1");
        let left = left.expect("the journal stays behind");
        assert!(left.starts_with(".apb/trigger-state/log/"), "{left}");
        assert!(left.contains('('), "{left} names the reason");
    }

    /// A state entry the removal could not delete is an outcome, never an
    /// error: the rule is gone either way, and the answer names the state
    /// file that stays behind and why (W5).
    #[test]
    fn a_remove_whose_state_entry_stays_behind_answers_state_left() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let entry = read_rules_file(fx.path()).rules[0].clone();
        position_at_head(fx.path(), &entry, 3, 4).unwrap();
        assert!(state_has_entry(fx.path(), NAME), "the state entry is there");
        // The state file stops verifying: the store refuses to write it.
        let state = signed_store::file::<TriggerStateFile>(fx.path(), Scope::Project, "state");
        std::fs::write(&state, b"not a signed state").unwrap();
        let outcome = remove_rule(fx.path(), NAME).unwrap();
        assert!(outcome.removed, "the rule is removed: {outcome:?}");
        let left = outcome.state_left.expect("the state entry stays behind");
        assert!(
            left.starts_with(".apb/trigger-state/project/state.json ("),
            "{left}"
        );
        assert!(left.contains('('), "{left} names the reason");
        assert!(
            read_rules_file(fx.path()).rules.is_empty(),
            "the entry is gone from rules.json"
        );
    }

    /// An approval that would push the rules file past what its reader
    /// reads is refused `rules_store_full` with the file unchanged (W6
    /// item 2), and the rule stays unapproved.
    #[test]
    fn an_approval_past_the_rules_cap_is_refused_with_the_file_unchanged() {
        let fx = Fixture::new(true);
        let cap = crate::signed_store::NO_LINKS_CAP;
        let rules_path = crate::signed_store::file::<RulesFile>(fx.path(), Scope::Project, "rules");
        // Bulk rules (the bulk rides in the rule's `key`) until the next
        // save would cross the cap.
        let bulk = "k".repeat(512 * 1024);
        let mut last = None;
        for n in 1..40 {
            let doc = format!(
                "name: bulk-{n}\nplaybook: {{ id: mini, version: 1.0.0, scope: project }}\nsource:\n  inbox:\n    connector: github\n    account: main\n    types: [issues]\n    where:\n      - {{ path: action, equals: opened }}\n    key: \"{bulk}\"\nparams:\n  title: \"opened issue\"\nlimits: {{ max_per_hour: 5 }}\nbudget: {{ tokens: 100000, on_no_estimate: continue }}\nexternal_data: true\nrequire_ready: false\n"
            );
            let doc = doc.replace("account: main", "account: acme");
            match set_rule(fx.path(), &doc, "test", Millis(1)) {
                Ok(_) => last = Some(format!("bulk-{n}")),
                Err(e) => {
                    if !e.to_string().contains("rules_store_full") {
                        panic!("the bulk save is refused for the wrong reason: {e}");
                    }
                    break;
                }
            }
        }
        let last = last.expect("at least one bulk rule saved");
        // Calibrate the last rule so the file sits ten bytes under the cap,
        // where an approval cannot fit: the pad grows by whatever the first
        // guess came off by.
        let bulk_doc = |key: &str| {
            format!(
                "name: {last}\nplaybook: {{ id: mini, version: 1.0.0, scope: project }}\nsource:\n  inbox:\n    connector: github\n    account: acme\n    types: [issues]\n    where:\n      - {{ path: action, equals: opened }}\n    key: \"{key}\"\nparams:\n  title: \"opened issue\"\nlimits: {{ max_per_hour: 5 }}\nbudget: {{ tokens: 100000, on_no_estimate: continue }}\nexternal_data: true\nrequire_ready: false\n"
            )
        };
        let mut len = std::fs::read(&rules_path).unwrap().len() as u64;
        if cap - len >= 400 {
            let mut pad_extra = 0usize;
            loop {
                let slack = cap - std::fs::read(&rules_path).unwrap().len() as u64;
                let pad = " ".repeat((slack - 10) as usize + pad_extra);
                set_rule(
                    fx.path(),
                    &bulk_doc(&format!("{bulk}{pad}")),
                    "test",
                    Millis(1),
                )
                .unwrap();
                len = std::fs::read(&rules_path).unwrap().len() as u64;
                if len == cap - 10 {
                    break;
                }
                // The re-save moved the file by less than the pad asked
                // for; the difference is what the entry itself shrank by.
                pad_extra += (cap - 10 - len) as usize;
            }
        }
        // The approval does not fit: refused, the file unchanged, the rule
        // unapproved.
        let entry = read_rules_file(fx.path()).rules[0].clone();
        let before = std::fs::read(&rules_path).unwrap();
        let err = store_approval(
            fx.path(),
            &entry.rule.name,
            approval_for(&entry, 2),
            Millis(2),
        )
        .expect_err("the approval is refused");
        assert!(err.to_string().contains("rules_store_full"), "{err}");
        assert_eq!(
            std::fs::read(&rules_path).unwrap(),
            before,
            "nothing written"
        );
        let rules = read_rules_file(fx.path());
        assert!(
            rules.rules.iter().all(|e| e.approval.is_none()),
            "the rule stays unapproved"
        );
    }

    /// `remove` deletes the entry, the rule's state entry and its journal
    /// file, each gone, and a second remove answers false.
    #[test]
    fn remove_deletes_entry_state_and_journal() {
        let fx = Fixture::new(true);
        set_rule(fx.path(), &base_doc(), "terminal", Millis(1)).unwrap();
        let entry = read_rules_file(fx.path()).rules[0].clone();
        position_at_head(fx.path(), &entry, 3, 4).unwrap();
        let journal = fx
            .path()
            .join(".apb")
            .join("trigger-state")
            .join("log")
            .join(format!("{}.jsonl", entry.incarnation));
        assert!(journal.exists(), "the pass left a journal");
        assert!(state_has_entry(fx.path(), NAME));

        let outcome = remove_rule(fx.path(), NAME).unwrap();
        assert!(outcome.removed);
        assert!(
            outcome.journal_left.is_none(),
            "the journal went with it: {outcome:?}"
        );
        assert!(
            read_rules_file(fx.path()).rules.is_empty(),
            "the entry is gone from rules.json"
        );
        assert!(
            !state_has_entry(fx.path(), NAME),
            "the state entry is gone from state.json"
        );
        assert!(!journal.exists(), "the journal file is gone");
        let outcome = remove_rule(fx.path(), NAME).unwrap();
        assert!(!outcome.removed, "a second remove answers false");
        assert!(outcome.journal_left.is_none());
    }
}
