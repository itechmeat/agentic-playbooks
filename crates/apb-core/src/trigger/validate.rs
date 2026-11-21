//! The checks of one rule, codes V260 to V267. The codes check a rule, not
//! a playbook: a person writes a rule outside the playbook, so an agent
//! that edits a playbook can never add one. A rule with an error is never
//! saved; warnings are saved and shown.

use std::collections::BTreeMap;
use std::path::Path;

use super::model::{MAX_PARAMS, NAME_MAX, Rule};
use crate::schema::Playbook;
use crate::validate::Issue;

pub const V260: &str = "V260";
pub const V261: &str = "V261";
pub const V262: &str = "V262";
pub const V263: &str = "V263";
pub const V264: &str = "V264";
pub const V265: &str = "V265";
pub const V266: &str = "V266";
pub const V267: &str = "V267";

/// The connector facts of one binding, for V265: the idempotency of the
/// granted functions a playbook calls through a connector.
#[derive(Debug, Clone, Default)]
pub struct ConnectorFacts {
    /// Function id to its `idempotency` declaration (`marker`, `key`,
    /// `none`); a function that declares none is absent.
    pub idempotency: BTreeMap<String, String>,
}

/// What validation reads beyond the rule.
pub struct RuleContext {
    /// The pinned playbook, `None` when it does not resolve.
    pub playbook: Option<Playbook>,
    /// Its lifecycle, `None` when it does not resolve.
    pub lifecycle: Option<crate::trust::Lifecycle>,
    /// The connector document, `None` when it is not installed.
    pub connector: Option<crate::connector::def::ConnectorDoc>,
    /// The connector's configured account names.
    pub accounts: Vec<String>,
    /// The connector facts of the playbook's bindings, for V265.
    pub facts: BTreeMap<String, ConnectorFacts>,
}

impl RuleContext {
    /// Loads it for `rule` in the project at `root`, reading nothing else.
    pub fn load(root: &Path, rule: &Rule) -> RuleContext {
        let mut cx = RuleContext {
            playbook: None,
            lifecycle: None,
            connector: None,
            accounts: Vec::new(),
            facts: BTreeMap::new(),
        };
        // The facts of every installed connector, for V265. A mode an older
        // reading cannot name still has its wire name.
        for (name, facts) in crate::connector::resolve::validation_facts() {
            cx.facts.insert(
                name,
                ConnectorFacts {
                    idempotency: facts
                        .idempotency
                        .into_iter()
                        .map(|(f, mode)| (f, mode.as_str().to_string()))
                        .collect(),
                },
            );
        }
        // The source connector and its configured accounts, the way the
        // ingest listener resolves a delivery's target: global accounts only,
        // a hook path can never name a project-scoped one.
        let inbox = inbox(rule);
        cx.connector = crate::connector::store::read_doc_contained(&inbox.connector);
        cx.accounts = configured_accounts(&inbox.connector);
        // The pinned playbook, resolved and parsed.
        let origin = match rule.playbook.scope {
            crate::trigger::model::RuleScope::Project => {
                crate::scope::Origin::Project { workspace_id: None }
            }
            crate::trigger::model::RuleScope::Global => crate::scope::Origin::Global,
        };
        let wref = crate::scope::PlaybookRef {
            origin,
            id: rule.playbook.id.clone(),
            version: Some(rule.playbook.version.clone()),
        };
        if let Ok(resolved) = crate::store::resolve(root, &wref) {
            // The lifecycle of the resolved playbook directory: `store::
            // resolve` does not read it, and the run gate checks it on its
            // own, so `load` reads it to refuse a rule when it is saved,
            // not only at every start. A missing file reads as active.
            cx.lifecycle = Some(crate::trust::read_lifecycle(
                &resolved
                    .definition_parent
                    .join("playbooks")
                    .join(&resolved.id),
            ));
            if let Ok(loaded) = crate::registry::Registry::open_dir(&resolved.definition_parent)
                .and_then(|reg| reg.load(&resolved.id, Some(&resolved.version)))
            {
                cx.playbook = Some(loaded.playbook);
            }
        }
        cx
    }
}

/// The rule's inbox source. A later source kind gets its own arm here.
fn inbox(rule: &Rule) -> &crate::trigger::model::InboxSource {
    match &rule.source {
        crate::trigger::model::RuleSource::Inbox(source) => source,
    }
}

/// The names of the connector's configured global accounts; empty when the
/// connector has none or its account file does not read.
fn configured_accounts(name: &str) -> Vec<String> {
    let Some(path) = crate::connector::config::global_config_path(name) else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(file) = yaml_serde::from_str::<crate::connector::config::AccountsFile>(&raw) else {
        return Vec::new();
    };
    file.accounts.into_iter().map(|a| a.name).collect()
}

/// Whether the rule name is `[a-z0-9][a-z0-9-]*` of 1 to `NAME_MAX`
/// characters (like a profile name, tighter bound), and so one safe path
/// segment: the check every lock and store boundary makes before a name
/// reaches the filesystem.
pub(crate) fn name_ok(name: &str) -> bool {
    crate::registry::is_safe_segment(name)
        && !name.is_empty()
        && name.chars().count() <= NAME_MAX
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The upper bound of `max_per_hour`: the run scan that counts to the cap
/// (`run_view::STARTED_RUNS_SCAN` in the engine) never counts past 500, so a
/// higher cap could never be reached.
const MAX_PER_HOUR: u32 = 500;

/// Every issue of `rule`, errors first, each with its code.
pub fn check_rule(rule: &Rule, cx: &RuleContext) -> Vec<Issue> {
    let source = inbox(rule);
    let mut errors: Vec<Issue> = Vec::new();
    let mut warnings: Vec<Issue> = Vec::new();
    let mut error = |message: String| {
        errors.push(Issue::new(
            V260,
            crate::validate::Severity::Error,
            None::<String>,
            message,
        ));
    };

    // V260: the source resolves. The name first, then the connector, the
    // account, the `types`, the filter, the templates.
    if !name_ok(&rule.name) {
        error(format!(
            "the name `{}` must be [a-z0-9][a-z0-9-]* of 1 to {NAME_MAX} characters",
            rule.name
        ));
    }
    let webhook = match cx.connector.as_ref() {
        None => {
            error(format!("connector `{}` is not installed", source.connector));
            None
        }
        Some(doc) => match doc.webhook.as_ref() {
            None => {
                error(format!(
                    "connector `{}` has no `webhook:` block, so nothing can be delivered to it",
                    source.connector
                ));
                None
            }
            Some(hook) => Some(hook),
        },
    };
    if !cx.accounts.contains(&source.account) {
        error(format!(
            "connector `{}` has no configured account `{}`",
            source.connector, source.account
        ));
    }
    if !source.types.is_empty() {
        match webhook {
            // A missing webhook block already errored above.
            None => {}
            Some(hook) if !hook.names_event_type() => {
                error(format!(
                    "connector `{}` does not name an event type, so `types` cannot filter its deliveries",
                    source.connector
                ));
            }
            Some(_) => {}
        }
    }
    if let Err(why) = source.filter().check() {
        error(format!("the rule's filter does not hold: {why}"));
    }
    for (name, template) in &rule.params {
        if let Err(why) = super::render::parse(template) {
            error(format!(
                "the template of param `{name}` does not parse: {why}"
            ));
        }
    }
    if let Err(why) = super::render::parse(&source.key) {
        error(format!("the template of the key does not parse: {why}"));
    }

    // V261: the playbook resolves and is active.
    match (&cx.playbook, cx.lifecycle) {
        (None, _) => {
            errors.push(Issue::new(
                V261,
                crate::validate::Severity::Error,
                None::<String>,
                format!(
                    "playbook `{}` version {} does not resolve in this project",
                    rule.playbook.id, rule.playbook.version
                ),
            ));
        }
        (Some(_), Some(crate::trust::Lifecycle::Draft)) => {
            errors.push(Issue::new(
                V261,
                crate::validate::Severity::Error,
                None::<String>,
                format!(
                    "playbook `{}` is draft; a rule starts only an active playbook",
                    rule.playbook.id
                ),
            ));
        }
        (Some(_), Some(crate::trust::Lifecycle::Retired)) => {
            errors.push(Issue::new(
                V261,
                crate::validate::Severity::Error,
                None::<String>,
                format!(
                    "playbook `{}` is retired; a rule starts only an active playbook",
                    rule.playbook.id
                ),
            ));
        }
        (Some(_), None | Some(crate::trust::Lifecycle::Active)) => {}
    }

    // V262: the mapped params exist, are not secret, and event text only
    // goes into an untrusted one; every param without default is mapped,
    // and a secret one without default (which no rule can fill) is refused.
    if let Some(playbook) = cx.playbook.as_ref() {
        let declared: BTreeMap<&str, &crate::schema::Param> = playbook
            .params
            .iter()
            .map(|p| (p.name.as_str(), p))
            .collect();
        for (name, template) in &rule.params {
            let Some(param) = declared.get(name.as_str()) else {
                errors.push(Issue::new(
                    V262,
                    crate::validate::Severity::Error,
                    None::<String>,
                    format!(
                        "param `{name}` is not declared by playbook `{}`",
                        rule.playbook.id
                    ),
                ));
                continue;
            };
            if param.secret {
                errors.push(Issue::new(
                    V262,
                    crate::validate::Severity::Error,
                    None::<String>,
                    format!(
                        "param `{name}` is secret: a rule cannot fill it, a secret is asked at every start"
                    ),
                ));
                continue;
            }
            // A template that does not parse already errored as V260; the
            // `pattern` field narrows the value but never makes it trusted.
            if let Ok(parts) = super::render::parse(template)
                && super::render::reads_event_text(&parts)
                && !param.untrusted
            {
                errors.push(Issue::new(
                    V262,
                    crate::validate::Severity::Error,
                    None::<String>,
                    format!(
                        "param `{name}` takes text from the event ({template}); declare it untrusted: true (a pattern only narrows the value)"
                    ),
                ));
            }
        }
        // Every param without a default must be filled by the rule. A
        // secret one never can be (mapping it is refused above, and a
        // secret is asked at every start, which an unattended rule start
        // cannot do), so no event could ever start the playbook.
        let mut unmapped: Vec<&crate::schema::Param> = playbook
            .params
            .iter()
            .filter(|p| p.default.is_none() && !rule.params.contains_key(&p.name))
            .collect();
        unmapped.sort_by(|a, b| a.name.cmp(&b.name));
        for param in unmapped {
            let message = if param.secret {
                format!(
                    "param `{}` is secret and has no default: a rule cannot fill it, so no event can start this playbook",
                    param.name
                )
            } else {
                format!(
                    "param `{}` has no default and the rule does not map it",
                    param.name
                )
            };
            errors.push(Issue::new(
                V262,
                crate::validate::Severity::Error,
                None::<String>,
                message,
            ));
        }
    }

    // V263 and V267 read the same budget in force: the rule's argument over
    // the playbook's block. No playbook means no block, the rule's alone.
    let budget = crate::budget::effective(
        cx.playbook.as_ref().and_then(|p| p.budget.as_ref()),
        rule.budget.as_ref(),
    );
    if budget.is_none()
        || budget.is_some_and(|b| b.on_no_estimate == crate::budget::OnNoEstimate::Ask)
    {
        errors.push(Issue::new(
            V263,
            crate::validate::Severity::Error,
            None::<String>,
            "a rule needs a budget with a token or dollar limit and on_no_estimate pause or continue".to_string(),
        ));
    }

    // V264: the limits of `rule.rs`.
    if rule.limits.max_per_hour < 1 || rule.limits.max_per_hour > MAX_PER_HOUR {
        errors.push(Issue::new(
            V264,
            crate::validate::Severity::Error,
            None::<String>,
            format!(
                "max_per_hour must be 1 to {MAX_PER_HOUR}, got {}",
                rule.limits.max_per_hour
            ),
        ));
    }
    if source.key.is_empty() {
        errors.push(Issue::new(
            V264,
            crate::validate::Severity::Error,
            None::<String>,
            "the rule needs a non-empty `key` template".to_string(),
        ));
    }
    if rule.params.len() > MAX_PARAMS {
        errors.push(Issue::new(
            V264,
            crate::validate::Severity::Error,
            None::<String>,
            format!(
                "the rule maps {} params; at most {MAX_PARAMS}",
                rule.params.len()
            ),
        ));
    }

    // V265 (warning): a write through the rule's own connector and account
    // whose sends carry no marker feeds its own listening.
    if let (Some(playbook), Some(doc)) = (&cx.playbook, cx.connector.as_ref()) {
        let facts = cx.facts.get(source.connector.as_str());
        let mut said: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        for crate::connector::sites::Site {
            id: site, bindings, ..
        } in playbook.connector_sites()
        {
            for binding in bindings.iter().filter(|b| b.name == source.connector) {
                let in_scope = binding
                    .accounts
                    .as_ref()
                    .is_none_or(|names| names.contains(&source.account));
                if !in_scope {
                    continue;
                }
                let granted: Vec<&str> = match &binding.functions {
                    crate::schema::FunctionsAllow::All => {
                        doc.functions.iter().map(|f| f.name.as_str()).collect()
                    }
                    crate::schema::FunctionsAllow::ReadOnly => doc
                        .functions
                        .iter()
                        .filter(|f| f.read_only)
                        .map(|f| f.name.as_str())
                        .collect(),
                    crate::schema::FunctionsAllow::List(names) => {
                        names.iter().map(String::as_str).collect()
                    }
                };
                for function in granted {
                    let Some(spec) = doc.functions.iter().find(|f| f.name == function) else {
                        continue;
                    };
                    if spec.read_only {
                        continue;
                    }
                    let marked = facts
                        .and_then(|f| f.idempotency.get(function))
                        .is_some_and(|mode| mode == "marker");
                    if marked || !said.insert((site.to_string(), function.to_string())) {
                        continue;
                    }
                    warnings.push(Issue::new(
                        V265,
                        crate::validate::Severity::Warning,
                        None::<String>,
                        format!(
                            "node `{site}` can write through {}/{} without a send marker: its own writes come back as new events; the hourly cap bounds a loop",
                            source.connector, source.account
                        ),
                    ));
                }
            }
        }
    }

    // V266 (warning): the run learns from what arrives from outside.
    if !rule.external_data {
        warnings.push(Issue::new(
            V266,
            crate::validate::Severity::Warning,
            None::<String>,
            "what arrives from outside is kept for learning; set external_data: true (the default) unless the rule's input is this machine's own".to_string(),
        ));
    }

    // V267 (warning): a pause at an unknown estimate holds every run of a
    // rule, since an external data run never builds estimate history.
    if budget.is_some_and(|b| {
        b.on_no_estimate == crate::budget::OnNoEstimate::Pause && rule.external_data
    }) {
        warnings.push(Issue::new(
            V267,
            crate::validate::Severity::Warning,
            None::<String>,
            "this rule's runs are external data and never build estimate history, so on_no_estimate pause holds each run at its first gated node until a person resumes it; use continue, or a limit the playbook can meet without an estimate".to_string(),
        ));
    }

    errors.extend(warnings);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetRequest, OnNoEstimate};
    use crate::connector::def::ConnectorDoc;
    use crate::trigger::model::{
        InboxSource, MAX_PARAMS, NAME_MAX, RuleLimits, RulePlaybook, RuleScope, RuleSource,
    };
    use crate::trust::Lifecycle;
    use crate::validate::Severity;

    /// A connector manifest with a webhook block that names the event type,
    /// one inbox read and one write function. The write carries no
    /// idempotency: whether a marker is known is what `cx.facts` says.
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
  - name: create_issue
    description: Create an issue
    method: POST
    url: https://example.com/repos/o/r/issues
"#;

    fn github_doc() -> ConnectorDoc {
        ConnectorDoc::from_yaml(GITHUB_YAML, "github").unwrap()
    }

    /// The same connector installed but its webhook block gone (and with it
    /// the inbox functions the manifest cannot keep without the block).
    fn doc_without_webhook() -> ConnectorDoc {
        let mut doc = github_doc();
        doc.webhook = None;
        doc.functions.retain(|f| f.inbox.is_none());
        doc
    }

    /// A webhook that names no event type.
    fn doc_without_event_type() -> ConnectorDoc {
        let mut doc = github_doc();
        if let Some(hook) = doc.webhook.as_mut() {
            hook.event_type_header = None;
        }
        doc
    }

    fn base_rule() -> Rule {
        Rule {
            name: "issue-triage".into(),
            playbook: RulePlaybook {
                id: "mini".into(),
                version: "1.0.0".into(),
                scope: RuleScope::Project,
            },
            source: RuleSource::Inbox(InboxSource {
                connector: "github".into(),
                account: "acme".into(),
                types: vec!["issues".into()],
                conditions: Vec::new(),
                key: "{{body.issue.id}}".into(),
            }),
            params: BTreeMap::from([("title".to_string(), "{{body.issue.title}}".to_string())]),
            limits: RuleLimits { max_per_hour: 10 },
            budget: Some(BudgetRequest {
                tokens: Some(100_000),
                usd: None,
                on_no_estimate: Some(OnNoEstimate::Continue),
            }),
            external_data: true,
            require_ready: true,
            notify: None,
        }
    }

    /// A playbook whose node `act` binds connectors as `bindings` says, with
    /// the given `params:` lines.
    fn mini_yaml(bindings: &str, params: &str) -> String {
        format!(
            "schema: 2\nid: mini\nname: Mini\nversion: 1.0.0\nnodes:\n  \
             - {{ id: start, type: start }}\n  \
             - {{ id: act, type: agent_task, prompt: do it, connectors: [{bindings}] }}\n  \
             - {{ id: done, type: finish, outcome: success }}\nedges:\n  \
             - {{ from: start, to: act }}\n  \
             - {{ from: act, to: done }}\nparams:\n{params}"
        )
    }

    fn playbook_of(bindings: &str, params: &str) -> Playbook {
        Playbook::from_yaml(&mini_yaml(bindings, params)).unwrap()
    }

    fn base_playbook() -> Playbook {
        playbook_of(
            "{ name: github }",
            "  - { name: title, type: text, untrusted: true }\n",
        )
    }

    /// Everything resolves: the base rule alone is clean.
    fn base_cx() -> RuleContext {
        RuleContext {
            playbook: Some(base_playbook()),
            lifecycle: Some(Lifecycle::Active),
            connector: Some(github_doc()),
            accounts: vec!["acme".to_string()],
            facts: BTreeMap::from([(
                "github".to_string(),
                ConnectorFacts {
                    idempotency: BTreeMap::from([(
                        "create_issue".to_string(),
                        "marker".to_string(),
                    )]),
                },
            )]),
        }
    }

    fn codes(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|i| i.code).collect()
    }

    fn one<'a>(issues: &'a [Issue], code: &str) -> &'a Issue {
        let found: Vec<&Issue> = issues.iter().filter(|i| i.code == code).collect();
        assert_eq!(found.len(), 1, "expected one {code}, got {issues:?}");
        found[0]
    }

    #[test]
    fn v260_a_connector_without_webhook() {
        let rule = base_rule();
        // Installed, but the manifest carries no `webhook:` block.
        let mut cx = base_cx();
        cx.connector = Some(doc_without_webhook());
        let issues = check_rule(&rule, &cx);
        let issue = one(&issues, V260);
        assert_eq!(issue.severity, Severity::Error);
        assert!(issue.message.contains("webhook"), "{issue:?}");
        // Not installed at all.
        let mut cx = base_cx();
        cx.connector = None;
        let issues = check_rule(&rule, &cx);
        assert_eq!(codes(&issues), [V260]);
        assert!(issues[0].message.contains("not installed"), "{issues:?}");
        // A good connector raises nothing.
        assert_eq!(codes(&check_rule(&rule, &base_cx())), [] as [&str; 0]);
    }

    /// An MCP tool connector cannot carry a `webhook:` block, so nothing is
    /// ever delivered to its inbox and a rule naming it is refused V260.
    #[test]
    fn v260_an_mcp_tool_connector() {
        const MCP_YAML: &str = r#"name: github
version: 0.1.0
mcp:
  transport: stdio
  command: tracker-mcp-server
account_fields:
  - { name: token, required: true, secret: true }
functions:
  - name: search_issues
    description: Search issues by text
    read_only: true
    mcp: { tool: search_issues }
"#;
        let doc = ConnectorDoc::from_yaml(MCP_YAML, "github").unwrap();
        assert!(doc.mcp.is_some() && doc.webhook.is_none());
        let mut cx = base_cx();
        cx.connector = Some(doc);
        let issues = check_rule(&base_rule(), &cx);
        let issue = one(&issues, V260);
        assert_eq!(issue.severity, Severity::Error);
        assert!(issue.message.contains("no `webhook:` block"), "{issue:?}");
    }

    #[test]
    fn v260_types_without_an_event_type() {
        let mut rule = base_rule();
        let mut cx = base_cx();
        cx.connector = Some(doc_without_event_type());
        let issues = check_rule(&rule, &cx);
        let issue = one(&issues, V260);
        assert!(issue.message.contains("event type"), "{issue:?}");
        // Without `types` the same connector is fine.
        let RuleSource::Inbox(source) = &mut rule.source;
        source.types.clear();
        assert_eq!(codes(&check_rule(&rule, &cx)), [] as [&str; 0]);
    }

    #[test]
    fn v260_a_long_name() {
        let mut rule = base_rule();
        rule.name = "a".repeat(NAME_MAX + 1);
        let issues = check_rule(&rule, &base_cx());
        let issue = one(&issues, V260);
        assert!(issue.message.contains("name"), "{issue:?}");
        // A name with a wrong character is refused too.
        let mut rule = base_rule();
        rule.name = "Issue.Triage".into();
        assert_eq!(codes(&check_rule(&rule, &base_cx())), [V260]);
        // 40 characters is allowed.
        let mut rule = base_rule();
        rule.name = "a".repeat(NAME_MAX);
        assert_eq!(codes(&check_rule(&rule, &base_cx())), [] as [&str; 0]);
    }

    #[test]
    fn v261_an_unknown_version() {
        let rule = base_rule();
        let mut cx = base_cx();
        cx.playbook = None;
        cx.lifecycle = None;
        let issues = check_rule(&rule, &cx);
        let issue = one(&issues, V261);
        assert_eq!(issue.severity, Severity::Error);
        assert_eq!(
            issue.message,
            "playbook `mini` version 1.0.0 does not resolve in this project"
        );
    }

    #[test]
    fn v261_a_draft_target() {
        let rule = base_rule();
        let mut cx = base_cx();
        cx.lifecycle = Some(Lifecycle::Draft);
        let issues = check_rule(&rule, &cx);
        assert_eq!(
            issue_message(&issues, V261),
            "playbook `mini` is draft; a rule starts only an active playbook"
        );
    }

    #[test]
    fn v261_a_retired_target() {
        let rule = base_rule();
        let mut cx = base_cx();
        cx.lifecycle = Some(Lifecycle::Retired);
        let issues = check_rule(&rule, &cx);
        assert_eq!(
            issue_message(&issues, V261),
            "playbook `mini` is retired; a rule starts only an active playbook"
        );
    }

    fn issue_message(issues: &[Issue], code: &str) -> String {
        one(issues, code).message.clone()
    }

    #[test]
    fn v262_a_body_value_into_a_plain_param() {
        let rule = base_rule();
        // The param is declared without `untrusted: true`.
        let mut cx = base_cx();
        cx.playbook = Some(playbook_of(
            "{ name: github }",
            "  - { name: title, type: text }\n",
        ));
        let issues = check_rule(&rule, &cx);
        assert_eq!(
            issue_message(&issues, V262),
            "param `title` takes text from the event ({{body.issue.title}}); declare it untrusted: true (a pattern only narrows the value)"
        );
        // With the declaration the same rule passes.
        let issues = check_rule(&rule, &base_cx());
        assert_eq!(codes(&issues), [] as [&str; 0]);
    }

    #[test]
    fn v262_a_pattern_alone_does_not_pass() {
        let rule = base_rule();
        let mut cx = base_cx();
        cx.playbook = Some(playbook_of(
            "{ name: github }",
            "  - { name: title, type: text, pattern: \".*\" }\n",
        ));
        let issues = check_rule(&rule, &cx);
        assert_eq!(
            issue_message(&issues, V262),
            "param `title` takes text from the event ({{body.issue.title}}); declare it untrusted: true (a pattern only narrows the value)"
        );
    }

    #[test]
    fn v262_a_secret_param() {
        let mut rule = base_rule();
        rule.params.insert("ghost".into(), "static".into());
        rule.params
            .insert("token".into(), "{{body.issue.title}}".into());
        let mut cx = base_cx();
        cx.playbook = Some(playbook_of(
            "{ name: github }",
            "  - { name: title, type: text, untrusted: true }\n  - { name: token, type: text, secret: true }\n",
        ));
        let issues = check_rule(&rule, &cx);
        let both: Vec<&Issue> = issues.iter().filter(|i| i.code == V262).collect();
        assert_eq!(both.len(), 2, "{issues:?}");
        assert_eq!(
            both[0].message, "param `ghost` is not declared by playbook `mini`",
            "params come in name order"
        );
        assert!(
            both[1].message.starts_with("param `token` is secret"),
            "{issues:?}"
        );
    }

    #[test]
    fn v262_an_unmapped_required_param() {
        let rule = base_rule();
        let mut cx = base_cx();
        cx.playbook = Some(playbook_of(
            "{ name: github }",
            "  - { name: title, type: text, untrusted: true }\n  - { name: owner, type: text }\n  - { name: repo, type: text, default: \"apb\" }\n",
        ));
        let issues = check_rule(&rule, &cx);
        // `owner` has no default and is not mapped; `repo` has one.
        assert_eq!(
            issue_message(&issues, V262),
            "param `owner` has no default and the rule does not map it"
        );
        assert_eq!(issues.iter().filter(|i| i.code == V262).count(), 1);
    }

    /// A secret param without a default can never be filled by a rule (a
    /// rule cannot map a secret, and a secret is asked at every start), so
    /// no event could ever start the playbook: refused at save.
    #[test]
    fn v262_an_unfillable_secret_param() {
        let rule = base_rule();
        let mut cx = base_cx();
        cx.playbook = Some(playbook_of(
            "{ name: github }",
            "  - { name: title, type: text, untrusted: true }\n  - { name: token, type: text, secret: true }\n  - { name: spare, type: text, secret: true, default: \"x\" }\n",
        ));
        let issues = check_rule(&rule, &cx);
        let v262: Vec<&Issue> = issues.iter().filter(|i| i.code == V262).collect();
        assert_eq!(v262.len(), 1, "{issues:?}");
        assert_eq!(v262[0].severity, Severity::Error);
        assert_eq!(
            v262[0].message,
            "param `token` is secret and has no default: a rule cannot fill it, so no event can start this playbook"
        );
    }

    #[test]
    fn v263_no_budget() {
        let mut rule = base_rule();
        rule.budget = None;
        let issues = check_rule(&rule, &base_cx());
        assert_eq!(
            issue_message(&issues, V263),
            "a rule needs a budget with a token or dollar limit and on_no_estimate pause or continue"
        );
        // No playbook either: the rule's budget alone decides (V261 says the
        // playbook does not resolve, and V263 still judges the rule's own
        // budget).
        let mut cx = base_cx();
        cx.playbook = None;
        cx.lifecycle = None;
        let issues = check_rule(&rule, &cx);
        assert_eq!(codes(&issues), [V261, V263]);
        assert_eq!(
            issue_message(&issues, V263),
            "a rule needs a budget with a token or dollar limit and on_no_estimate pause or continue"
        );
    }

    #[test]
    fn v263_on_no_estimate_ask() {
        let mut rule = base_rule();
        rule.budget.as_mut().unwrap().on_no_estimate = Some(OnNoEstimate::Ask);
        let issues = check_rule(&rule, &base_cx());
        assert_eq!(
            issue_message(&issues, V263),
            "a rule needs a budget with a token or dollar limit and on_no_estimate pause or continue"
        );
    }

    #[test]
    fn v264_limits_out_of_range() {
        let max_per_hour = |n: u32| {
            let mut rule = base_rule();
            rule.limits.max_per_hour = n;
            check_rule(&rule, &base_cx())
        };
        assert!(one(&max_per_hour(0), V264).message.contains("max_per_hour"));
        assert!(
            one(&max_per_hour(501), V264)
                .message
                .contains("max_per_hour")
        );
        assert_eq!(codes(&max_per_hour(500)), [] as [&str; 0]);

        let mut rule = base_rule();
        let RuleSource::Inbox(source) = &mut rule.source;
        source.key.clear();
        let issues = check_rule(&rule, &base_cx());
        assert!(one(&issues, V264).message.contains("key"));

        let mut rule = base_rule();
        for i in 0..=MAX_PARAMS {
            rule.params.insert(format!("p{i}"), "x".into());
        }
        let issues = check_rule(&rule, &base_cx());
        assert!(one(&issues, V264).message.contains("params"));
        assert_eq!(issues.iter().filter(|i| i.code == V264).count(), 1);
    }

    #[test]
    fn v265_a_write_without_a_marker() {
        let rule = base_rule();
        // The function declares no marker.
        let mut cx = base_cx();
        cx.facts.get_mut("github").unwrap().idempotency.clear();
        let issues = check_rule(&rule, &cx);
        let issue = one(&issues, V265);
        assert_eq!(issue.severity, Severity::Warning);
        assert_eq!(
            issue.message,
            "node `act` can write through github/acme without a send marker: its own writes come back as new events; the hourly cap bounds a loop"
        );
        // A binding narrowed to another account is not the rule's account.
        let mut cx = base_cx();
        cx.facts.get_mut("github").unwrap().idempotency.clear();
        cx.playbook = Some(playbook_of(
            "{ name: github, accounts: [other] }",
            "  - { name: title, type: text, untrusted: true }\n",
        ));
        assert_eq!(codes(&check_rule(&rule, &cx)), [] as [&str; 0]);
        // The base context's marker keeps it silent.
        assert_eq!(codes(&check_rule(&rule, &base_cx())), [] as [&str; 0]);
    }

    #[test]
    fn v266_external_data_off() {
        let mut rule = base_rule();
        rule.external_data = false;
        let issues = check_rule(&rule, &base_cx());
        let issue = one(&issues, V266);
        assert_eq!(issue.severity, Severity::Warning);
        assert_eq!(
            issue.message,
            "what arrives from outside is kept for learning; set external_data: true (the default) unless the rule's input is this machine's own"
        );
    }

    #[test]
    fn v267_pause_with_external_data() {
        let mut rule = base_rule();
        rule.budget.as_mut().unwrap().on_no_estimate = Some(OnNoEstimate::Pause);
        let issues = check_rule(&rule, &base_cx());
        let issue = one(&issues, V267);
        assert_eq!(issue.severity, Severity::Warning);
        assert_eq!(
            issue.message,
            "this rule's runs are external data and never build estimate history, so on_no_estimate pause holds each run at its first gated node until a person resumes it; use continue, or a limit the playbook can meet without an estimate"
        );
        // External data off: V266 says so, V267 does not apply.
        rule.external_data = false;
        let issues = check_rule(&rule, &base_cx());
        assert_eq!(codes(&issues), [V266]);
    }

    #[test]
    fn load_resolves_playbook_connector_accounts_and_facts() {
        let _guard = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: under env_test_lock, the one env change at a time.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        let project = tempfile::tempdir().unwrap();
        let root = project.path();

        // The installed connector and its global account.
        let cdir = cfg.path().join("connectors").join("github");
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::write(cdir.join("connector.yaml"), GITHUB_YAML).unwrap();
        let accdir = cfg.path().join("connector-config");
        std::fs::create_dir_all(&accdir).unwrap();
        std::fs::write(
            accdir.join("github.yaml"),
            "accounts:\n  - name: acme\n    app_secret: \"{{env.S}}\"\n",
        )
        .unwrap();

        // The pinned playbook in the project store, registered.
        let vdir = root.join(".apb/playbooks").join("mini").join("1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join("playbook.yaml"),
            mini_yaml("{ name: github }", ""),
        )
        .unwrap();
        std::fs::write(
            root.join(".apb/playbooks").join("mini").join("current"),
            "1.0.0",
        )
        .unwrap();
        crate::projects::bind_id(root).unwrap();

        let rule = base_rule();
        let cx = RuleContext::load(root, &rule);
        let pb = cx.playbook.expect("playbook resolves");
        assert_eq!(pb.id, "mini");
        assert_eq!(pb.version, "1.0.0");
        assert_eq!(cx.lifecycle, Some(Lifecycle::Active));
        assert_eq!(
            cx.connector.as_ref().map(|d| d.name.as_str()),
            Some("github")
        );
        assert!(cx.connector.as_ref().unwrap().webhook.is_some());
        assert_eq!(cx.accounts, ["acme"]);
        assert!(cx.facts.contains_key("github"), "{:?}", cx.facts.keys());

        // An unknown version does not resolve, and stays None.
        let mut rule = base_rule();
        rule.playbook.version = "9.9.9".into();
        let cx = RuleContext::load(root, &rule);
        assert!(cx.playbook.is_none());

        // SAFETY: still under the lock (the guard drops at the end of the test).
        unsafe { std::env::remove_var("APB_CONFIG_DIR") };
    }
}
