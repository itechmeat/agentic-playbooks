//! The person-file freeze: profiles, connector manifests, connector
//! account files of both scopes, the settings (`policy.yaml`,
//! `config.yaml`, `decisions.yaml`) and start rule documents. Each frozen
//! document parses, keeps every key under the schema freeze's rule 3
//! comparison, and meets its hand-written expectation: named typed values
//! of the parsed file and the decisions its reader makes from them. See
//! `openspec/changes/one-point-oh-freeze` (design D1.5 item 3) and
//! `docs/COMPATIBILITY.md`.
//!
//! The writer behind `APB_WRITE_FREEZE=1` copies the official connectors'
//! manifests verbatim into `connectors.yaml` and only ever adds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MARKER: &str = "# apb-freeze-document: ";

/// Set to `1` to let the test `write_freeze` add what this build has and
/// the freeze lacks. The writer only ever adds: it never removes an entry,
/// never changes a frozen error code, line or argument, and writes
/// nothing when any freeze rule fails.
const WRITE_FREEZE_ENV: &str = "APB_WRITE_FREEZE";

/// Whether the switch is set. Named `write_freeze_enabled`, never
/// `write_freeze`: that name is the writer test's.
fn write_freeze_enabled() -> bool {
    std::env::var(WRITE_FREEZE_ENV).as_deref() == Ok("1")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/files-1.0")
}

fn fixture(file: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(file))
        .unwrap_or_else(|e| panic!("cannot read the freeze fixture {file}: {e}"))
}

fn connectors_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../connectors")
}

/// `(name, text)` of every document, in file order; text without its
/// marker line. Panics on a repeated name.
fn documents(file: &str) -> Vec<(String, String)> {
    let text = fixture(file);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some(doc) = current.take() {
                out.push(doc);
            }
            current = Some((name.to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(doc) = current.take() {
        out.push(doc);
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in &out {
        assert!(seen.insert(name.as_str()), "duplicate document name {name}");
    }
    out
}

fn document(file: &str, name: &str) -> String {
    documents(file)
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no document named {name} in {file}"))
        .1
}

// ---------------------------------------------------------------------------
// Key retention (the schema freeze's rule 3, over these files)
// ---------------------------------------------------------------------------

fn raw_value(text: &str) -> yaml_serde::Value {
    yaml_serde::from_str(text).expect("the document is YAML")
}

/// `(document, exact path, written form, serialized form, reason)`.
const NORMALIZED: &[(&str, &str, &str, &str, &str)] = &[];

/// Every place `raw` holds that `serialized` does not, walked structurally:
/// each mapping key (a user-named key is data, one that holds brackets and
/// dots included) and each sequence index must exist at its exact place in
/// both trees. The paths name the place in the failure only.
fn dropped_paths(
    raw: &yaml_serde::Value,
    serialized: &yaml_serde::Value,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    match (raw, serialized) {
        (yaml_serde::Value::Mapping(m), yaml_serde::Value::Mapping(s)) => {
            for (k, v) in m {
                let key = k.as_str().unwrap_or_default().to_string();
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match s.get(k) {
                    None => out.push(path),
                    Some(sv) => out.extend(dropped_paths(v, sv, &path)),
                }
            }
        }
        (yaml_serde::Value::Sequence(a), yaml_serde::Value::Sequence(b)) => {
            for (i, v) in a.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                match b.get(i) {
                    None => out.push(path),
                    Some(sv) => out.extend(dropped_paths(v, sv, &path)),
                }
            }
        }
        _ => {}
    }
    out
}

/// The schema freeze's rule 3 over a person file: every place the written
/// document holds is a place of `typed`, the production reader's result
/// serialized again. A reader that drops a key fails here, because the key
/// is checked against what the reader kept, not against the YAML text.
fn keys_kept(file: &str, name: &str, text: &str, typed: &yaml_serde::Value) {
    let raw = raw_value(text);
    keys_kept_value(file, name, &raw, typed);
}

fn keys_kept_value(file: &str, name: &str, raw: &yaml_serde::Value, typed: &yaml_serde::Value) {
    for path in dropped_paths(raw, typed, "") {
        let entry = NORMALIZED
            .iter()
            .find(|(doc, p, _, _, _)| doc == &name && p == &path);
        if let Some((_, _, written, as_serialized, reason)) = entry {
            let w: yaml_serde::Value = yaml_serde::from_str(written)
                .unwrap_or_else(|e| panic!("NORMALIZED {name} {path}: {reason}: {e}"));
            let s: yaml_serde::Value = yaml_serde::from_str(as_serialized)
                .unwrap_or_else(|e| panic!("NORMALIZED {name} {path}: {reason}: {e}"));
            assert_eq!(
                w, s,
                "NORMALIZED {name} {path}: {reason}: the two forms disagree"
            );
        } else {
            panic!(
                "the frozen document {name} (in {file}) holds the key {path}, which its reader's result does not keep: a reader that drops a frozen key breaks the freeze"
            );
        }
    }
}

/// The reader's result as the value the key check compares with.
fn typed<T: serde::Serialize>(value: &T) -> yaml_serde::Value {
    yaml_serde::to_value(value).expect("the reader's result serializes")
}

// ---------------------------------------------------------------------------
// Expectations (the meaning rule)
// ---------------------------------------------------------------------------

type Expectation = fn();

/// Every hand-written frozen document with its hand-written expectation.
/// Each expectation reads the document through its production reader,
/// checks the reader kept every key (`keys_kept` over the reader's result)
/// and asserts named typed values and the decisions the reader makes. A
/// document without one fails `every_document_has_an_expectation`; the
/// writer never writes an expectation. `every_expectation_holds` runs them
/// all.
const EXPECTATIONS: &[(&str, Expectation)] = &[
    ("profile/main", expect_profile_main),
    ("profile/reviewer", expect_profile_reviewer),
    ("profile/agy", expect_profile_agy),
    ("profile/opencode", expect_profile_opencode),
    ("profile/pi", expect_profile_pi),
    ("profile/hermes", expect_profile_hermes),
    ("profile/grok", expect_profile_grok),
    ("profile/cursor", expect_profile_cursor),
    ("profile/qoder", expect_profile_qoder),
    ("profile/zcode", expect_profile_zcode),
    ("profile/dsh", expect_profile_dsh),
    (
        "connector/missing-auth@0.1.0",
        expect_connector_missing_auth,
    ),
    ("connector/query-auth@0.1.0", expect_connector_query_auth),
    ("connector/mcp-stdio@0.1.0", expect_connector_mcp_stdio),
    ("connector/mcp-http@0.1.0", expect_connector_mcp_http),
    (
        "accounts/global-only-with-cmd",
        expect_accounts_global_only_with_cmd,
    ),
    (
        "accounts/project-replaces-global",
        expect_accounts_project_replaces_global,
    ),
    (
        "accounts/project-default-clears-global",
        expect_accounts_project_default_clears_global,
    ),
    (
        "accounts/project-cmd-refused",
        expect_accounts_project_cmd_refused,
    ),
    ("accounts/global-header", expect_accounts_global_header),
    ("accounts/global-query", expect_accounts_global_query),
    ("accounts/global-basic", expect_accounts_global_basic),
    ("accounts/global-path", expect_accounts_global_path),
    (
        "accounts/global-oauth2_refresh",
        expect_accounts_global_oauth2_refresh,
    ),
    ("accounts/project-header", expect_accounts_project_header),
    ("accounts/project-query", expect_accounts_project_query),
    ("accounts/project-basic", expect_accounts_project_basic),
    ("accounts/project-path", expect_accounts_project_path),
    (
        "accounts/project-oauth2_refresh",
        expect_accounts_project_oauth2_refresh,
    ),
    ("settings/policy", expect_settings_policy),
    ("settings/config", expect_settings_config),
    ("settings/decisions", expect_settings_decisions),
    ("trigger-rule/inbox", expect_trigger_rule_inbox),
    (
        "trigger-rule/inbox-unkeyed",
        expect_trigger_rule_inbox_unkeyed,
    ),
];

/// The official connector manifests frozen verbatim, each with what its
/// reader must decide: `(document, auth kind, account fields, function
/// names)`. The auth kind is `none` for a manifest without `auth`; the
/// lists are in manifest order.
const OFFICIAL_CONNECTORS: &[(&str, &str, &str, &str)] = &[
    (
        "connectors/amocrm@0.1.0",
        "header",
        "base_url drive_url access_token",
        "get_account list_users get_user list_roles list_event_types list_loss_reasons list_leads get_lead create_leads update_lead update_leads create_leads_complex list_unsorted get_unsorted_summary accept_unsorted decline_unsorted list_pipelines get_pipeline create_pipelines update_pipeline delete_pipeline list_statuses create_statuses update_status delete_status list_contacts get_contact create_contacts update_contact list_companies get_company create_companies update_company update_entities list_customers get_customer create_customers update_customer list_customer_statuses list_customer_segments list_transactions add_transactions delete_transaction set_customers_mode list_tasks get_task create_tasks update_task complete_task list_notes get_note create_notes update_note pin_note unpin_note list_tags create_tags list_links link_entities unlink_entities list_events get_event list_custom_fields get_custom_field create_custom_fields update_custom_field delete_custom_field list_custom_field_groups create_custom_field_groups list_catalog_custom_fields create_catalog_custom_fields list_catalogs get_catalog create_catalogs update_catalog list_catalog_elements get_catalog_element create_catalog_elements update_catalog_element get_products_settings enable_products list_webhooks subscribe_webhook unsubscribe_webhook list_bots run_bot stop_bot list_sources create_sources list_chat_templates list_talks close_talk list_subscriptions create_short_links add_calls list_files get_file list_entity_files attach_files detach_files list_file_links",
    ),
    (
        "connectors/asana@0.2.0",
        "header",
        "api_base token",
        "get_me list_workspaces list_projects list_sections list_tasks get_task create_task update_task add_comment list_comments list_subtasks create_subtask add_task_to_section search_tasks",
    ),
    (
        "connectors/atrip@0.1.0",
        "header",
        "client_id client_secret base_url search_base_url",
        "search verify get_offers get_offer_price order order_commit pay query_order_details seat_availability get_luggage void_quotation void query_void_orders refund_quotation refund query_refund_orders stop_ticket regenerate_order pnr_claim extract_pnr post_booking_ancillary_search post_booking_ancillary_order balance route_export query_mail get_atrip_token update_webhook_url event_get_page_list",
    ),
    (
        "connectors/discord@0.1.0",
        "header",
        "api_base token",
        "get_me list_channels get_messages send_message reply_to_message",
    ),
    (
        "connectors/github@0.4.0",
        "header",
        "api_base token webhook_secret",
        "get_rate_limit get_authenticated_user list_issues get_issue create_issue update_issue comment_issue list_recent_issues list_issue_comments add_labels remove_label add_assignees list_pulls get_pull create_pull merge_pull request_reviewers create_review create_release get_release_by_tag dispatch_workflow list_workflow_runs list_check_runs get_combined_status inbox_read inbox_ack inbox_peek_depth",
    ),
    (
        "connectors/gitlab@0.3.0",
        "header",
        "api_base token",
        "get_user list_issues get_issue create_issue update_issue comment_issue list_merge_requests get_merge_request create_merge_request merge_merge_request approve_merge_request comment_merge_request create_release get_release_by_tag list_releases list_pipelines get_pipeline list_pipeline_jobs trigger_pipeline",
    ),
    (
        "connectors/gmail@0.1.0",
        "oauth2_refresh",
        "api_base token_url client_id client_secret refresh_token granted_scopes email",
        "verify list_labels search_messages get_message get_thread create_draft send_draft add_labels remove_labels archive",
    ),
    (
        "connectors/google-calendar@0.1.0",
        "oauth2_refresh",
        "api_base token_url client_id client_secret refresh_token granted_scopes",
        "verify list_calendars list_events get_event query_free_busy create_event update_event delete_event",
    ),
    (
        "connectors/google-drive@0.1.0",
        "oauth2_refresh",
        "api_base token_url client_id client_secret refresh_token granted_scopes",
        "verify search_files get_file_metadata export_text read_text_file",
    ),
    (
        "connectors/imap@0.2.0",
        "oauth2_refresh",
        "host port use_tls auth_method username password from_email from_name oauth_token_url oauth_authorize_url oauth_client_id oauth_client_secret oauth_refresh_token oauth_scopes oauth_access_type oauth_prompt oauth_granted_scopes",
        "verify list_folders search_messages get_message mark_read mark_unread append_draft save_attachments",
    ),
    (
        "connectors/sentry@0.1.0",
        "header",
        "base_url org token",
        "list_projects list_issues get_issue update_issue create_release create_deploy",
    ),
    (
        "connectors/slack@0.2.0",
        "header",
        "api_base token",
        "auth_test list_channels get_messages get_thread send_message reply_in_thread",
    ),
    (
        "connectors/smtp@0.2.0",
        "none",
        "host port username password from_email from_name use_tls",
        "verify send_email",
    ),
    (
        "connectors/telegram@0.2.0",
        "path",
        "api_base token",
        "get_me send_message edit_message_text get_chat get_updates answer_callback_query",
    ),
    (
        "connectors/twenty@0.1.0",
        "header",
        "base_url api_key",
        "list_companies get_company create_company update_company delete_company list_people get_person create_person update_person delete_person list_opportunities get_opportunity create_opportunity update_opportunity delete_opportunity list_notes get_note create_note update_note delete_note list_tasks get_task create_task update_task delete_task create_note_target create_task_target list_note_targets list_task_targets find_duplicate_companies find_duplicate_people list_records get_record create_record update_record delete_record restore_record list_webhooks create_webhook delete_webhook list_objects",
    ),
    (
        "connectors/web-search@0.1.0",
        "header",
        "api_base api_key",
        "verify search",
    ),
    (
        "connectors/whatsapp@0.2.0",
        "header",
        "base_url graph_version phone_number_id waba_id access_token app_secret verify_token",
        "send_text send_template send_media mark_read list_templates create_template delete_template get_business_profile update_business_profile list_phone_numbers get_phone_number get_media_url delete_media inbox_read inbox_ack inbox_peek_depth",
    ),
    (
        "connectors/youtrack@0.1.1",
        "header",
        "api_base token",
        "get_me list_projects search_issues get_issue create_issue update_issue list_comments add_comment apply_command",
    ),
    (
        "connectors/zulip@0.2.0",
        "basic",
        "api_base email api_key",
        "get_me list_streams list_topics get_messages send_stream_message send_direct_message",
    ),
];

fn expectations_of(file: &str) -> Vec<&'static str> {
    let prefix = match file {
        "profiles.yaml" => "profile/",
        "connectors.yaml" => "connector/",
        "connector-accounts.yaml" => "accounts/",
        "settings.yaml" => "settings/",
        "trigger-rules.yaml" => "trigger-rule/",
        other => panic!("no expectation table for {other}"),
    };
    let mut out: Vec<&'static str> = EXPECTATIONS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| n.starts_with(prefix))
        .collect();
    if file == "connectors.yaml" {
        out.extend(OFFICIAL_CONNECTORS.iter().map(|(n, _, _, _)| *n));
    }
    out
}

const FILES: &[&str] = &[
    "profiles.yaml",
    "connectors.yaml",
    "connector-accounts.yaml",
    "settings.yaml",
    "trigger-rules.yaml",
];

/// The meaning rule: a frozen document without an expectation fails, the
/// official connector manifests the writer copies included.
#[test]
fn every_document_has_an_expectation() {
    for file in FILES {
        let known = expectations_of(file);
        for (name, _) in documents(file) {
            assert!(
                known.contains(&name.as_str()),
                "the frozen document {name} ({file}) has no expectation: a document without one asserts nothing about what its reader decides; write its expectation"
            );
        }
        for name in known {
            assert!(
                documents(file).iter().any(|(n, _)| n == name),
                "the expectation {name} names no document of {file}"
            );
        }
    }
}

/// Every expectation runs: each reads its document through the production
/// reader, checks the kept keys and asserts the frozen meaning.
#[test]
fn every_expectation_holds() {
    for (name, check) in EXPECTATIONS {
        eprintln!("checking {name}");
        check();
    }
}

/// Every official connector manifest reads with the frozen auth kind,
/// account fields and function names, and its reader keeps every key.
#[test]
fn every_official_connector_meets_its_expectation() {
    for (name, auth, fields, functions) in OFFICIAL_CONNECTORS {
        let doc = connector(name);
        // A bool first and a message that carries no auth or account data:
        // the label is the manifest's name from the table.
        let auth_ok = auth_kind(&doc) == *auth;
        assert!(auth_ok, "{name}: the auth kind is not the frozen one");
        let fields_ok = doc
            .account_fields
            .iter()
            .map(|f| f.name.as_str())
            .eq(fields.split_whitespace());
        assert!(
            fields_ok,
            "{name}: the account fields are not the frozen ones"
        );
        let got: Vec<&str> = doc.functions.iter().map(|f| f.name.as_str()).collect();
        let want: Vec<&str> = functions.split_whitespace().collect();
        assert_eq!(got, want, "the functions of {name}");
    }
}

/// Every executor kind the agent registry knows has a frozen profile.
#[test]
fn every_executor_kind_has_a_profile() {
    let agents: Vec<String> = documents("profiles.yaml")
        .iter()
        .map(|(_, text)| {
            let doc = apb_core::profile::ProfileDoc::from_yaml(text).expect("parses");
            apb_core::agents::canonical_id(&doc.executor.agent).to_string()
        })
        .collect();
    for agent in apb_core::agents::all() {
        let id = agent.descriptor().id;
        assert!(
            agents.iter().any(|a| a == id),
            "no frozen profile runs the executor kind {id}: add one to profiles.yaml with its expectation"
        );
    }
}

/// Every auth kind a connector can declare (the `kind` tags of `AuthSpec`,
/// as serde lists them) has a frozen account of both scopes.
#[test]
fn every_auth_kind_has_an_account_in_each_scope() {
    let error = serde_json::from_value::<apb_core::connector::def::AuthSpec>(
        serde_json::json!({ "kind": "__apb_freeze_no_such_kind__" }),
    )
    .expect_err("an unknown auth kind is refused")
    .to_string();
    let kinds: Vec<String> = error
        .split_once("expected ")
        .map(|(_, rest)| rest.strip_prefix("one of ").unwrap_or(rest))
        .expect("serde lists the auth kinds")
        .split(", ")
        .flat_map(|s| s.split(" or "))
        .map(|s| s.trim().trim_matches('`').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(kinds.len() >= 5, "the auth kinds read: {kinds:?}");
    let names: Vec<String> = documents("connector-accounts.yaml")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for kind in &kinds {
        for scope in ["global", "project"] {
            let doc = format!("accounts/{scope}-{kind}");
            assert!(
                names.contains(&doc),
                "no frozen account of the auth kind {kind} in the {scope} scope ({doc})"
            );
        }
        assert!(
            ACCOUNT_KINDS.iter().any(|(k, _, _)| k == kind),
            "ACCOUNT_KINDS names no connector for the auth kind {kind}"
        );
    }
}

// ---------------------------------------------------------------------------
// Parsing per file
// ---------------------------------------------------------------------------

/// Each profile document parses and its reader's structural check passes.
#[test]
fn every_profile_document_parses() {
    for (name, _) in documents("profiles.yaml") {
        profile(&name);
    }
}

/// Each connector manifest parses under its own folder name and keeps
/// every key.
#[test]
fn every_connector_manifest_parses() {
    for (name, _) in documents("connectors.yaml") {
        let doc = connector(&name);
        let expected_name = name
            .rsplit_once('/')
            .and_then(|(_, n)| n.rsplit_once('@').map(|(n, _)| n.to_string()))
            .unwrap_or_default();
        assert_eq!(doc.name, expected_name, "the manifest's name is its folder");
    }
}

/// The official connectors' manifests are frozen verbatim under
/// `connectors/<name>@<version>`, byte for byte.
#[test]
fn connectors_yaml_holds_every_official_manifest() {
    for (name, bytes) in official_manifests() {
        let frozen = documents("connectors.yaml")
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| {
                panic!(
                    "connectors.yaml lacks {name}: run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-core --test main files_freeze_test::write_freeze"
                )
            });
        assert_eq!(
            frozen.1.trim_end(),
            bytes.trim_end(),
            "the frozen copy of {name} differs from the manifest on disk: bump the connector's version"
        );
    }
}

/// The settings documents parse through their readers.
#[test]
fn every_settings_document_parses() {
    policy();
    drop(global_config());
    decisions();
}

/// The decisions file reader is strict at the top level and in every
/// provider: an unknown member is refused, so a member it stopped knowing
/// cannot be dropped silently.
#[test]
fn the_decisions_reader_refuses_an_unknown_key_at_every_level() {
    let text = document("settings.yaml", "settings/decisions");
    let raw = raw_value(&text);
    let count = raw["providers"].as_sequence().expect("providers").len();
    for target in std::iter::once(None).chain((0..count).map(Some)) {
        let mut doc = raw.clone();
        let place = match target {
            None => &mut doc,
            Some(i) => &mut doc["providers"][i],
        };
        place
            .as_mapping_mut()
            .expect("a mapping")
            .insert("apb_freeze_unknown".into(), 1.into());
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(
            dir.path().join("decisions.yaml"),
            yaml_serde::to_string(&doc).expect("renders"),
        )
        .expect("writes");
        let lock = crate::common::env_lock();
        let _fake = crate::common::EnvVarGuard::set(&lock, "APB_DECISIONS_ALLOW_FAKE", "1");
        assert!(
            apb_core::decisions::load_file(dir.path()).is_err(),
            "the decisions reader accepts an unknown key at {target:?}"
        );
    }
}

/// The policy reader is strict at every level the frozen document holds:
/// an unknown key in any of its sections is refused, so a key the reader
/// stopped knowing cannot be dropped silently. `PolicyFile` has no
/// serializer to compare kept keys with; this is its key check.
#[test]
fn the_policy_reader_refuses_an_unknown_key_at_every_level() {
    let text = document("settings.yaml", "settings/policy");
    let raw = raw_value(&text);
    let sections: Vec<String> = raw
        .as_mapping()
        .expect("the policy is a mapping")
        .keys()
        .map(|k| k.as_str().expect("a section name").to_string())
        .collect();
    assert_eq!(
        sections,
        [
            "notify",
            "launch",
            "billing",
            "runs",
            "health",
            "unattended"
        ],
        "the frozen policy holds every section"
    );
    for section in std::iter::once(None).chain(sections.iter().map(Some)) {
        let mut doc = raw.clone();
        let target = match section {
            None => &mut doc,
            Some(s) => doc.get_mut(s.as_str()).expect("the section"),
        };
        target
            .as_mapping_mut()
            .expect("a section is a mapping")
            .insert("apb_freeze_unknown".into(), 1.into());
        let text = yaml_serde::to_string(&doc).expect("renders");
        assert!(
            apb_core::policy_file::parse(&text).is_err(),
            "the policy reader accepts an unknown key in {}",
            section.map_or("the top level", String::as_str)
        );
    }
}

/// Every start rule document parses and passes its own checks without an
/// error, so no expectation of codes is needed.
#[test]
fn every_trigger_rule_parses_and_checks() {
    for (name, _) in documents("trigger-rules.yaml") {
        rule(&name);
    }
}

// ---------------------------------------------------------------------------
// The readers, each with its key check
// ---------------------------------------------------------------------------

fn profile(name: &str) -> apb_core::profile::ProfileDoc {
    let text = document("profiles.yaml", name);
    let doc = apb_core::profile::ProfileDoc::from_yaml(&text)
        .unwrap_or_else(|e| panic!("the frozen document {name} does not parse: {e}"));
    let problems = doc.problems();
    assert!(
        problems.is_empty(),
        "the frozen document {name} has structural problems: {problems:?}"
    );
    keys_kept("profiles.yaml", name, &text, &typed(&doc));
    doc
}

fn connector(name: &str) -> apb_core::connector::ConnectorDoc {
    let text = document("connectors.yaml", name);
    let folder = name
        .rsplit_once('/')
        .and_then(|(_, n)| n.rsplit_once('@').map(|(n, _)| n.to_string()))
        .unwrap_or_default();
    let doc = apb_core::connector::ConnectorDoc::from_yaml(&text, &folder)
        .unwrap_or_else(|e| panic!("the frozen document {name} does not parse: {e}"));
    keys_kept("connectors.yaml", name, &text, &typed(&doc));
    doc
}

/// The serde tag of a manifest's auth block, `none` without one.
fn auth_kind(doc: &apb_core::connector::ConnectorDoc) -> String {
    doc.auth.as_ref().map_or_else(
        || "none".to_string(),
        |a| {
            serde_json::to_value(a).expect("the auth serializes")["kind"]
                .as_str()
                .expect("the auth has a kind")
                .to_string()
        },
    )
}

fn policy() -> apb_core::policy_file::PolicyFile {
    let text = document("settings.yaml", "settings/policy");
    apb_core::policy_file::parse(&text)
        .unwrap_or_else(|e| panic!("settings/policy does not parse: {e}"))
}

/// The frozen `config.yaml` read by `GlobalConfig::load` under a sandboxed
/// `APB_CONFIG_DIR`, its keys checked against the loaded value.
fn global_config() -> (crate::common::ConfigSandbox, apb_core::config::GlobalConfig) {
    let sandbox = crate::common::config_sandbox();
    let config_dir = PathBuf::from(std::env::var_os("APB_CONFIG_DIR").expect("the sandbox"));
    let text = document("settings.yaml", "settings/config");
    std::fs::write(config_dir.join("config.yaml"), &text).expect("the config writes");
    let config = apb_core::config::GlobalConfig::load().expect("the frozen config loads");
    keys_kept("settings.yaml", "settings/config", &text, &typed(&config));
    (sandbox, config)
}

/// The frozen `decisions.yaml` read by `decisions::load_file`, its keys
/// checked against the effective settings it yields. `version` is read and
/// checked by the loader (only 1 is accepted) and is not part of the
/// effective settings, so it is compared by value instead.
fn decisions() -> apb_core::decisions::EffectiveDecisions {
    let dir = tempfile::tempdir().expect("the decisions temp dir");
    let text = document("settings.yaml", "settings/decisions");
    std::fs::write(dir.path().join("decisions.yaml"), &text).expect("the file writes");
    let effective = {
        let lock = crate::common::env_lock();
        let _fake = crate::common::EnvVarGuard::set(&lock, "APB_DECISIONS_ALLOW_FAKE", "1");
        apb_core::decisions::load_file(dir.path())
            .expect("the frozen decisions load")
            .expect("a decisions file is there")
    };
    // The loader translates three members, each checked by value here and
    // then left out of the key comparison: `version` (only 1 reads) is not
    // part of the effective settings, a provider's `api_key` becomes its
    // `key` reference, and `via` (only `openai_compatible` reads) is
    // consumed by the check. Every other key must be kept. The file reader
    // is strict (`the_decisions_reader_refuses_an_unknown_key_at_every_level`),
    // so a member it stopped knowing is refused, never dropped.
    let mut raw = raw_value(&text);
    let version = raw
        .as_mapping_mut()
        .expect("a mapping")
        .remove("version")
        .expect("the frozen file names its version");
    assert_eq!(version.as_u64(), Some(1), "the frozen version");
    let providers = raw
        .get_mut("providers")
        .and_then(|p| p.as_sequence_mut())
        .expect("the frozen providers");
    for (raw_provider, provider) in providers.iter_mut().zip(&effective.providers) {
        let map = raw_provider.as_mapping_mut().expect("a provider mapping");
        if let Some(key) = map.remove("api_key") {
            let var = key
                .as_str()
                .and_then(|k| k.strip_prefix("{{env."))
                .and_then(|k| k.strip_suffix("}}"))
                .expect("the frozen api_key is an env reference");
            assert_eq!(
                provider.key,
                Some(apb_core::decisions::KeyRef::Env(var.to_string())),
                "the api_key of {} reads as its key reference",
                provider.id
            );
        }
        if let Some(via) = map.remove("via") {
            assert_eq!(via.as_str(), Some("openai_compatible"), "the frozen via");
        }
    }
    keys_kept_value(
        "settings.yaml",
        "settings/decisions",
        &raw,
        &typed(&effective),
    );
    effective
}

fn rule(name: &str) -> apb_core::trigger::Rule {
    let text = document("trigger-rules.yaml", name);
    let rule = apb_core::trigger::parse_rule_document(&text)
        .unwrap_or_else(|e| panic!("the frozen document {name} does not parse: {e}"));
    let issues = apb_core::trigger::check_rule(&rule, &rule_context());
    let errors: Vec<_> = issues
        .iter()
        .filter(|i| i.severity == apb_core::validate::Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "the frozen document {name} fails the start rule checks: {errors:?}"
    );
    keys_kept("trigger-rules.yaml", name, &text, &typed(&rule));
    rule
}

fn rule_context() -> apb_core::trigger::RuleContext {
    // The frozen rules name the `github` connector, its accounts and the
    // playbook the resolution rule (V260, V261) checks against; the
    // context holds both, so the documents' own shape is what the checks
    // judge.
    let playbook: apb_core::schema::Playbook = yaml_serde::from_str(
        "schema: 2\nid: dependency-security-fix\nname: Dependency security fix\nversion: 1.0.0\nparams:\n  - { name: title, type: text, untrusted: true }\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: \"{{params.title}}\", expected_duration: 5m, connectors: [{ name: github, functions: [list_issues] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the rule-check playbook parses");
    let connector = apb_core::connector::ConnectorDoc::from_yaml(
        "name: github\nversion: 0.4.0\naccount_fields:\n  - { name: token, required: true, secret: true }\nwebhook:\n  event_type_header: X-GitHub-Event\n  signature:\n    scheme: hmac_sha256_hex\n    header: X-Hub-Signature-256\n    secret: \"{{secret.token}}\"\nfunctions:\n  - name: list_issues\n    description: List issues\n    read_only: true\n    method: GET\n    url: \"https://api.github.com/issues\"\n  - name: inbox_read\n    description: Read delivered events\n    read_only: true\n    inbox:\n      op: read\n",
        "github",
    )
    .expect("the rule-check connector parses");
    apb_core::trigger::RuleContext {
        playbook: Some(playbook),
        lifecycle: Some(apb_core::trust::Lifecycle::Active),
        connector: Some(connector),
        accounts: vec!["acme".to_string(), "main".to_string()],
        facts: std::collections::BTreeMap::from([(
            "github".to_string(),
            apb_core::trigger::ConnectorFacts {
                idempotency: std::collections::BTreeMap::from([(
                    "create_issue".to_string(),
                    "none".to_string(),
                )]),
            },
        )]),
    }
}

// ---------------------------------------------------------------------------
// The account files of both scopes, read through load_merged
// ---------------------------------------------------------------------------

/// A temporary `APB_CONFIG_DIR` and project with one account file placed
/// at its path; the merged list `load_merged` answers.
struct AccountsEnv {
    _dir: tempfile::TempDir,
    root: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for AccountsEnv {
    fn drop(&mut self) {
        // SAFETY: env mutation is serialized by ENV_LOCK, held by the guard.
        unsafe { std::env::remove_var("APB_CONFIG_DIR") };
    }
}

fn place_accounts(global: Option<&str>, project: Option<&str>, connector: &str) -> AccountsEnv {
    let dir = tempfile::tempdir().expect("the accounts temp dir");
    let config_dir = dir.path().join("config");
    let root = dir.path().join("project");
    std::fs::create_dir_all(config_dir.join("connector-config")).expect("config dir");
    std::fs::create_dir_all(root.join(".apb/connector-config")).expect("project dir");
    let guard = crate::common::env_lock();
    // SAFETY: env mutation is serialized by ENV_LOCK, held by the guard.
    unsafe { std::env::set_var("APB_CONFIG_DIR", &config_dir) };
    if let Some(text) = global {
        let path = config_dir
            .join("connector-config")
            .join(format!("{connector}.yaml"));
        std::fs::write(path, text).expect("the global account file writes");
    }
    if let Some(text) = project {
        let path = root
            .join(".apb/connector-config")
            .join(format!("{connector}.yaml"));
        std::fs::write(path, text).expect("the project account file writes");
    }
    AccountsEnv {
        _dir: dir,
        root,
        _guard: guard,
    }
}

/// The two halves of an account document, split at its
/// `# apb-freeze-scope:` markers.
fn halves(case: &str) -> (Option<String>, Option<String>) {
    let text = document("connector-accounts.yaml", case);
    let mut global = None;
    let mut project = None;
    let mut section = "";
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# apb-freeze-scope: ") {
            section = rest.trim();
            continue;
        }
        let target = match section {
            "global" => &mut global,
            "project" => &mut project,
            _ => panic!("the document {case} names no scope for its lines"),
        };
        let buffer: &mut String = target.get_or_insert_with(String::new);
        buffer.push_str(line);
        buffer.push('\n');
    }
    (global, project)
}

/// The account case read through `load_merged` under its temporary paths.
/// On success every key of every account that wins the merge (each
/// project account, and each global one no project account replaces) is
/// checked against the merged account the reader kept.
///
/// Every assertion over account data computes a `bool` first and fails
/// with a static message naming the case (a `&'static str` from the test
/// table), so no failure message carries anything read from an account
/// document: no value, no field name, no account name, no reader error.
/// A refusal comes back as its text for the caller to test, never print.
fn merged_of(
    case: &'static str,
    connector: &str,
) -> Result<Vec<apb_core::connector::config::Account>, String> {
    let (global, project) = halves(case);
    let merged = {
        let env = place_accounts(global.as_deref(), project.as_deref(), connector);
        apb_core::connector::config::load_merged(&env.root, connector).map_err(|e| e.to_string())?
    };
    let accounts_of = |text: &Option<String>| -> Vec<yaml_serde::Value> {
        text.as_deref()
            .map(|t| {
                raw_value(t)["accounts"]
                    .as_sequence()
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    };
    let project_accounts = accounts_of(&project);
    let name_of = |a: &yaml_serde::Value| a["name"].as_str().unwrap_or_default().to_string();
    let replaced: Vec<String> = project_accounts.iter().map(name_of).collect();
    let winners = accounts_of(&global)
        .into_iter()
        .filter(|a| !replaced.contains(&name_of(a)))
        .chain(project_accounts.iter().cloned());
    for raw in winners {
        let name = name_of(&raw);
        let kept = merged.iter().find(|a| a.name == name);
        assert!(
            kept.is_some(),
            "{case}: an account that wins the merge is not in the merged list"
        );
        let Some(kept) = kept else { continue };
        // A cleared default is the merge's decision, not a dropped key:
        // compare the account's own keys with the reader's account. No
        // account key is normalized (`NORMALIZED` names none), so every
        // key must be kept as written.
        let all_kept = dropped_paths(
            &raw,
            &typed(&apb_core::connector::config::Account {
                default: raw["default"].as_bool().unwrap_or(kept.default),
                ..kept.clone()
            }),
            "",
        )
        .is_empty();
        assert!(
            all_kept,
            "{case}: the account reader drops a frozen key, which breaks the freeze"
        );
    }
    Ok(merged)
}

/// One account per auth kind: `(auth kind, connector manifest in
/// connectors.yaml, fields)`, the same fields in both scopes.
/// `(auth kind, connector manifest, fields)`.
type AccountKind = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const ACCOUNT_KINDS: &[AccountKind] = &[
    (
        "header",
        "connectors/asana@0.2.0",
        &[
            ("api_base", "https://app.asana.com/api/1.0"),
            ("token", "{{env.ASANA_TOKEN}}"),
        ],
    ),
    (
        "query",
        "connector/query-auth@0.1.0",
        &[
            ("base_url", "https://query.example.test"),
            ("token", "{{env.QUERY_TOKEN}}"),
        ],
    ),
    (
        "basic",
        "connectors/zulip@0.2.0",
        &[
            ("api_base", "https://chat.example.test/api/v1"),
            ("email", "bot@example.test"),
            ("api_key", "{{env.ZULIP_KEY}}"),
        ],
    ),
    (
        "path",
        "connectors/telegram@0.2.0",
        &[
            ("api_base", "https://api.telegram.org"),
            ("token", "{{env.TELEGRAM_TOKEN}}"),
        ],
    ),
    (
        "oauth2_refresh",
        "connectors/google-drive@0.1.0",
        &[
            ("api_base", "https://www.googleapis.com/drive/v3"),
            ("token_url", "https://oauth2.googleapis.com/token"),
            ("client_id", "client-123.apps.example.test"),
            ("client_secret", "{{env.DRIVE_CLIENT_SECRET}}"),
            ("refresh_token", "{{env.DRIVE_REFRESH_TOKEN}}"),
            (
                "granted_scopes",
                "https://www.googleapis.com/auth/drive.readonly",
            ),
        ],
    ),
];

/// The account of one auth kind in one scope merges as frozen, and the
/// connector of that kind accepts it (`validate_accounts`: declared
/// fields, required ones present, secrets as references only). `case` is
/// the document's name in the test table; every message names it alone.
fn expect_account_kind(case: &'static str, scope: &str, kind: &str) {
    let entry = ACCOUNT_KINDS.iter().find(|(k, _, _)| *k == kind);
    assert!(entry.is_some(), "{case}: ACCOUNT_KINDS has no entry for it");
    let Some((_, manifest, fields)) = entry else {
        return;
    };
    let doc = connector(manifest);
    let auth_ok = auth_kind(&doc) == kind;
    assert!(auth_ok, "{case}: the connector has another auth kind");
    let merged = merged_of(case, &doc.name);
    assert!(merged.is_ok(), "{case}: the account does not merge");
    let Ok(merged) = merged else { return };
    assert!(merged.len() == 1, "{case}: not exactly one account merges");
    let account = &merged[0];
    let name_ok = account.name == format!("{scope}-{}", kind.replace('_', "-"));
    assert!(name_ok, "{case}: the merged account has another name");
    assert!(account.default, "{case}: the account is not the default");
    let want: BTreeMap<String, String> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let fields_ok = account.fields == want;
    assert!(
        fields_ok,
        "{case}: the merged fields are not the frozen ones"
    );
    let accepted = apb_core::connector::config::validate_accounts(&doc, &merged).is_empty();
    assert!(
        accepted,
        "{case}: the connector of its auth kind refuses the frozen account"
    );
}

fn expect_accounts_global_header() {
    expect_account_kind("accounts/global-header", "global", "header");
}
fn expect_accounts_global_query() {
    expect_account_kind("accounts/global-query", "global", "query");
}
fn expect_accounts_global_basic() {
    expect_account_kind("accounts/global-basic", "global", "basic");
}
fn expect_accounts_global_path() {
    expect_account_kind("accounts/global-path", "global", "path");
}
fn expect_accounts_global_oauth2_refresh() {
    expect_account_kind("accounts/global-oauth2_refresh", "global", "oauth2_refresh");
}
fn expect_accounts_project_header() {
    expect_account_kind("accounts/project-header", "project", "header");
}
fn expect_accounts_project_query() {
    expect_account_kind("accounts/project-query", "project", "query");
}
fn expect_accounts_project_basic() {
    expect_account_kind("accounts/project-basic", "project", "basic");
}
fn expect_accounts_project_path() {
    expect_account_kind("accounts/project-path", "project", "path");
}
fn expect_accounts_project_oauth2_refresh() {
    expect_account_kind(
        "accounts/project-oauth2_refresh",
        "project",
        "oauth2_refresh",
    );
}

/// The global-only account (`{{cmd:...}}` in a field, which only the
/// global file may use) merges: the merged list holds it with its command
/// field untouched.
fn expect_accounts_global_only_with_cmd() {
    const CASE: &str = "accounts/global-only-with-cmd";
    let merged = merged_of(CASE, "case-global-cmd");
    assert!(
        merged.is_ok(),
        "{CASE}: the global-only account does not merge"
    );
    let Ok(merged) = merged else { return };
    assert!(merged.len() == 1, "{CASE}: not exactly one account merges");
    assert!(
        merged[0].name == "main",
        "{CASE}: the account has another name"
    );
    assert!(
        merged[0].default,
        "{CASE}: the global account is not the default"
    );
    let passes = merged[0].fields.get("token").map(String::as_str) == Some("{{cmd:op read token}}");
    assert!(
        passes,
        "{CASE}: the command field does not pass through the global file"
    );
}

/// A project account of the same name replaces the global one entirely.
fn expect_accounts_project_replaces_global() {
    const CASE: &str = "accounts/project-replaces-global";
    let merged = merged_of(CASE, "case-replace");
    assert!(merged.is_ok(), "{CASE}: the accounts do not merge");
    let Ok(merged) = merged else { return };
    let order_ok = merged.iter().map(|a| a.name.as_str()).eq(["other", "main"]);
    assert!(
        order_ok,
        "{CASE}: not global-only first, then project order"
    );
    let Some(main) = merged.iter().find(|a| a.name == "main") else {
        panic!("{CASE}: the project account is not merged");
    };
    let project_wins = main.fields.get("token").map(String::as_str) == Some("project-token");
    assert!(
        project_wins,
        "{CASE}: the project account's field does not win"
    );
    assert!(
        !main.fields.contains_key("api_base"),
        "{CASE}: the global account is not replaced entirely, its other fields remain"
    );
    assert!(
        !main.default,
        "{CASE}: the replaced global default carries over to the project account"
    );
}

/// A project `default: true` clears a global default.
fn expect_accounts_project_default_clears_global() {
    const CASE: &str = "accounts/project-default-clears-global";
    let merged = merged_of(CASE, "case-default");
    assert!(merged.is_ok(), "{CASE}: the accounts do not merge");
    let Ok(merged) = merged else { return };
    let default_of = |n: &str| merged.iter().find(|a| a.name == n).map(|a| a.default);
    assert!(
        default_of("global-keep") == Some(false),
        "{CASE}: the global default is not cleared while a project default exists"
    );
    assert!(
        default_of("main") == Some(true),
        "{CASE}: the project default does not hold"
    );
}

/// A project account with a `{{cmd:...}}` field is refused, naming the
/// account.
fn expect_accounts_project_cmd_refused() {
    const CASE: &str = "accounts/project-cmd-refused";
    let Err(error) = merged_of(CASE, "case-project-cmd") else {
        panic!("{CASE}: a project command field merges instead of a hard error");
    };
    let names_field = error.contains("cmd");
    assert!(
        names_field,
        "{CASE}: the refusal does not name the command field"
    );
    let names_account = error.contains("main");
    assert!(
        names_account,
        "{CASE}: the refusal does not name the account"
    );
}

// ---------------------------------------------------------------------------
// Profiles and hand-written connectors
// ---------------------------------------------------------------------------

fn fallbacks(doc: &apb_core::profile::ProfileDoc) -> Vec<(String, String)> {
    doc.executor
        .fallbacks
        .iter()
        .map(|f| (f.agent.clone(), f.model.clone()))
        .collect()
}

fn expect_profile_main() {
    let doc = profile("profile/main");
    assert_eq!(doc.name, "main");
    assert_eq!(doc.description, "The routine worker of the freeze");
    assert_eq!(doc.executor.agent, "claude");
    assert_eq!(doc.executor.model, "opus");
    assert_eq!(
        fallbacks(&doc),
        vec![("claude".to_string(), "sonnet".to_string())]
    );
    let env = doc.process_env.as_ref().expect("process_env is frozen");
    assert_eq!(
        env.policy,
        apb_core::process_env::ProcessEnvMode::Allowlist,
        "the main profile's process_env is an allowlist"
    );
    let allow: Vec<String> = env.allow.iter().map(|p| p.as_str().to_string()).collect();
    assert_eq!(allow, vec!["HOME".to_string(), "APB_*".to_string()]);
}

fn expect_profile_reviewer() {
    let doc = profile("profile/reviewer");
    assert_eq!(doc.name, "reviewer");
    assert_eq!(doc.executor.agent, "codex");
    assert_eq!(doc.executor.model, "gpt-5.1-codex");
    assert!(doc.executor.fallbacks.is_empty(), "no fallbacks are frozen");
    assert!(doc.process_env.is_none(), "no process_env is frozen");
}

fn expect_profile_agy() {
    let doc = profile("profile/agy");
    assert_eq!(doc.name, "agy-worker");
    assert_eq!(doc.executor.agent, "agy");
    assert_eq!(doc.executor.model, "gemini-3-pro");
    assert_eq!(
        fallbacks(&doc),
        vec![("agy".to_string(), "gemini-3-flash".to_string())]
    );
    assert_eq!(doc.soul, apb_core::profile::SoulRequirement::NativeRequired);
    let skills: Vec<(String, apb_core::profile::ProfileScope)> = doc
        .skills
        .iter()
        .map(|s| (s.name.clone(), s.scope))
        .collect();
    assert_eq!(
        skills,
        vec![
            (
                "review-notes".to_string(),
                apb_core::profile::ProfileScope::Auto
            ),
            (
                "release-notes".to_string(),
                apb_core::profile::ProfileScope::Global
            ),
        ],
        "a short skill reads as auto, a mapping keeps its scope"
    );
    assert_eq!(
        doc.environment,
        Some(apb_core::profile::AgentEnvironment::Full)
    );
}

fn expect_profile_opencode() {
    let doc = profile("profile/opencode");
    assert_eq!(doc.executor.agent, "opencode");
    assert_eq!(doc.executor.model, "zai/glm-5.3-flash");
    let tiers: Vec<(String, Option<String>, bool, String)> = doc
        .tiers
        .iter()
        .map(|(name, t)| {
            (
                name.clone(),
                t.model.clone(),
                t.use_executor.is_some(),
                t.for_work.clone(),
            )
        })
        .collect();
    assert_eq!(
        tiers,
        vec![
            (
                "quick".to_string(),
                Some("zai/glm-5.3-flash".to_string()),
                false,
                "small mechanical edits".to_string()
            ),
            (
                "deep".to_string(),
                None,
                true,
                "everything else".to_string()
            ),
        ],
        "the tiers keep their order, their own executor and `use: executor`"
    );
}

fn expect_profile_pi() {
    let doc = profile("profile/pi");
    assert_eq!(doc.executor.agent, "pi");
    assert_eq!(doc.executor.model, "anthropic/claude-sonnet-5-5");
    assert_eq!(
        fallbacks(&doc),
        vec![("claude".to_string(), "sonnet".to_string())],
        "a fallback may name another agent"
    );
    let env = doc.process_env.as_ref().expect("process_env is frozen");
    assert_eq!(env.policy, apb_core::process_env::ProcessEnvMode::Inherit);
    assert!(!env.is_allowlist(), "an inherit policy is no allowlist");
}

/// A profile of one executor kind with only its executor set.
fn expect_plain_profile(doc_name: &str, name: &str, agent: &str, model: &str) {
    let doc = profile(doc_name);
    assert_eq!(doc.name, name);
    assert_eq!(doc.executor.agent, agent);
    assert_eq!(doc.executor.model, model);
    assert!(
        apb_core::agents::lookup(&doc.executor.agent).is_some(),
        "{agent} is a built-in agent"
    );
}

fn expect_profile_hermes() {
    expect_plain_profile("profile/hermes", "hermes-worker", "hermes", "hermes-5");
}

fn expect_profile_grok() {
    expect_plain_profile("profile/grok", "grok-worker", "grok", "grok-5");
}

fn expect_profile_cursor() {
    expect_plain_profile("profile/cursor", "cursor-worker", "cursor", "auto");
}

fn expect_profile_qoder() {
    expect_plain_profile("profile/qoder", "qoder-worker", "qoder", "auto");
}

fn expect_profile_zcode() {
    expect_plain_profile("profile/zcode", "zcode-worker", "zcode", "glm-5.3-flash");
    let doc = profile("profile/zcode");
    assert_eq!(
        doc.zcode_mode,
        Some(apb_core::profile::ZcodeMode::Edit),
        "the zcode mode is frozen"
    );
}

fn expect_profile_dsh() {
    expect_plain_profile(
        "profile/dsh",
        "dsh-worker",
        "dsh",
        "nearai/zai-org/GLM-5.3@high",
    );
}

fn expect_connector_missing_auth() {
    let doc = connector("connector/missing-auth@0.1.0");
    assert!(doc.auth.is_none(), "the manifest names no auth");
    assert_eq!(doc.functions.len(), 1, "one function is frozen");
    assert_eq!(doc.functions[0].name, "probe");
    assert!(doc.functions[0].read_only, "the function is read-only");
}

fn expect_connector_query_auth() {
    let doc = connector("connector/query-auth@0.1.0");
    assert_eq!(
        doc.auth,
        Some(apb_core::connector::def::AuthSpec::Query {
            param: "api_key".to_string(),
            value_template: "{{secret.token}}".to_string(),
        }),
        "the query auth block reads as frozen"
    );
    let fields: Vec<(&str, bool, bool)> = doc
        .account_fields
        .iter()
        .map(|f| (f.name.as_str(), f.required, f.secret))
        .collect();
    assert_eq!(
        fields,
        vec![("base_url", true, false), ("token", true, true)],
        "the account fields with their required and secret flags"
    );
}

fn expect_connector_mcp_stdio() {
    let doc = connector("connector/mcp-stdio@0.1.0");
    let mcp = doc.mcp.as_ref().expect("the mcp block is frozen");
    assert_eq!(
        mcp.transport,
        apb_core::connector::mcp::McpTransportKind::Stdio
    );
    assert_eq!(mcp.command.as_deref(), Some("tracker-mcp"));
    assert_eq!(mcp.args, vec!["--stdio".to_string()]);
    let tools: Vec<&str> = doc
        .functions
        .iter()
        .map(|f| {
            f.mcp
                .as_ref()
                .expect("an mcp function names its tool")
                .tool
                .as_str()
        })
        .collect();
    assert_eq!(
        tools,
        vec!["list_issues", "get_issue"],
        "the mapped tools are frozen"
    );
    assert!(
        doc.functions.iter().all(|f| f.read_only),
        "the mapped tools are read-only"
    );
}

fn expect_connector_mcp_http() {
    let doc = connector("connector/mcp-http@0.1.0");
    let mcp = doc.mcp.as_ref().expect("the mcp block is frozen");
    assert_eq!(
        mcp.transport,
        apb_core::connector::mcp::McpTransportKind::Http
    );
    assert_eq!(
        mcp.url.as_deref(),
        Some("https://mcp.example.com/{{account.subdomain}}/mcp")
    );
    assert!(mcp.command.is_none(), "an http server names no command");
    assert!(
        matches!(
            doc.auth,
            Some(apb_core::connector::def::AuthSpec::Header { .. })
        ),
        "the http connector authenticates with the connector-level header auth"
    );
}

// ---------------------------------------------------------------------------
// Settings and start rules
// ---------------------------------------------------------------------------

fn expect_settings_policy() {
    let policy = policy();
    // notify: the outcomes a finished run notifies about, and the reminder
    // cadence.
    assert_eq!(
        policy.notify.finished,
        Some(vec![
            apb_core::notify::FinishOutcome::Failed,
            apb_core::notify::FinishOutcome::Aborted
        ]),
        "the frozen notify outcomes"
    );
    assert_eq!(
        policy.notify.remind.as_deref(),
        Some(["1h".to_string()].as_slice()),
        "the reminder cadence is frozen"
    );
    // launch: the machine's agent policy and one playbook's override.
    assert_eq!(
        policy.launch.agent,
        Some(apb_core::launch::LaunchPolicy::Ask),
        "the agent launch policy is frozen"
    );
    assert_eq!(
        policy
            .launch
            .playbooks
            .get("global/dependency-security-fix"),
        Some(&apb_core::launch::LaunchPolicy::Allow),
        "the playbook launch override is frozen"
    );
    // billing: what a billed run does.
    assert_eq!(
        policy.billing.billed,
        Some(apb_core::launch::LaunchPolicy::Ask),
        "the billed policy is frozen"
    );
    assert_eq!(
        policy.billing.budget_counts_subscription,
        Some(true),
        "the subscription counting is frozen"
    );
    // runs, health and unattended values.
    assert_eq!(policy.runs.start_key_days, Some(14));
    assert_eq!(policy.runs.keep_days, Some(45));
    assert_eq!(policy.runs.keep_last, Some(30));
    assert_eq!(policy.runs.keep_failed_days, Some(120));
    assert_eq!(
        policy.health.playbooks,
        Some(vec!["deploy".to_string(), "global:triage".to_string()]),
        "the watched playbooks are frozen"
    );
    assert_eq!(policy.unattended.after_attended_successes, Some(3));
    assert!(policy.unattended.waive_probation);
    // The effective settings the machine file yields with no project file.
    let policies = apb_core::policy_file::Policies {
        machine: policy,
        project: Default::default(),
    };
    assert_eq!(policies.start_key_days(), 14, "the start key days in force");
    assert_eq!(
        policies.retention(),
        apb_core::policy_file::RetentionSettings {
            keep_days: 45,
            keep_last: 30,
            keep_failed_days: 120,
        },
        "the retention in force"
    );
    let key = |id, global| apb_core::policy_file::PlaybookKey { id, global };
    assert!(policies.health_watches(&key("deploy", false)));
    assert!(policies.health_watches(&key("triage", true)));
    assert!(
        !policies.health_watches(&key("other", false)),
        "a playbook the list does not name is not watched"
    );
    assert_eq!(
        policies.probation_floor(),
        Some((3, apb_core::policy_file::PolicySource::Machine)),
        "the probation floor in force"
    );
    assert!(policies.waives_probation(), "the machine waives probation");
    assert_eq!(
        policies.finished(),
        vec![
            apb_core::notify::FinishOutcome::Failed,
            apb_core::notify::FinishOutcome::Aborted
        ],
        "the outcomes notified"
    );
    assert_eq!(
        policies.remind_ms(),
        vec![3_600_000],
        "one reminder after an hour"
    );
}

fn expect_settings_config() {
    let (_sandbox, config) = global_config();
    assert_eq!(config.port, Some(8080), "the port is frozen");
    let agent = config
        .agents
        .get("claude")
        .expect("the claude agent is frozen");
    assert_eq!(
        agent.program.as_deref(),
        Some("claude"),
        "the agent's program is frozen"
    );
    assert_eq!(
        config.runners.get("ts").map(|r| r.as_slice()),
        Some(&["bun".to_string(), "deno".to_string()][..]),
        "the runners are frozen"
    );
    assert_eq!(config.registry, Some(false), "the registry flag is frozen");
}

fn expect_settings_decisions() {
    let effective = decisions();
    assert_eq!(
        effective.mode,
        apb_core::decisions::DecisionMode::Shadow,
        "the frozen ceiling"
    );
    assert_eq!(effective.timeout_ms, 2500, "the frozen timeout");
    let providers: Vec<(&str, &str)> = effective
        .providers
        .iter()
        .map(|p| (p.id.as_str(), p.kind.as_str()))
        .collect();
    assert_eq!(
        providers,
        vec![
            ("sys", "systemone"),
            ("stub", "fake"),
            ("near", "llm_emulation"),
            ("vercel", "vercel_evaluate"),
            ("openrouter", "openrouter_decisions"),
            ("cf", "cloudflare"),
        ],
        "every provider kind, in the frozen order"
    );
    assert_eq!(effective.budget.max_requests_per_run, 5);
    assert_eq!(effective.budget.max_usd_per_run, 0.01);
}

fn expect_trigger_rule_inbox() {
    let rule = rule("trigger-rule/inbox");
    assert_eq!(rule.name, "triage-opened-issues");
    let apb_core::trigger::RuleSource::Inbox(inbox) = &rule.source;
    assert_eq!(inbox.connector, "github");
    // A bool and a static message: the account name is not printed.
    let account_ok = inbox.account == "acme";
    assert!(
        account_ok,
        "trigger-rule/inbox: the inbox account is not the frozen one"
    );
    assert_eq!(inbox.types, vec!["issues".to_string()]);
    assert_eq!(inbox.conditions.len(), 1, "one where condition is frozen");
    assert_eq!(
        inbox.key,
        "{{body.repository.full_name}}#{{body.issue.number}}"
    );
    assert_eq!(
        rule.params.get("title").map(String::as_str),
        Some("{{body.issue.title}}"),
        "the param template is frozen"
    );
    assert_eq!(rule.limits.max_per_hour, 5, "the hourly cap is frozen");
    assert_eq!(
        rule.budget.as_ref().and_then(|b| b.tokens),
        Some(100_000),
        "the budget is frozen"
    );
}

/// The second filter shape: types only, no `where`, so the filter keeps
/// both forms.
fn expect_trigger_rule_inbox_unkeyed() {
    let rule = rule("trigger-rule/inbox-unkeyed");
    let apb_core::trigger::RuleSource::Inbox(inbox) = &rule.source;
    assert!(inbox.conditions.is_empty(), "no where condition is frozen");
    assert_eq!(inbox.types, vec!["messages".to_string()]);
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

fn official_manifests() -> Vec<(String, String)> {
    let root = connectors_root();
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("connectors reads")
        .map(|e| e.expect("connector entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let manifest = path.join("connector.yaml");
        if !manifest.is_file() {
            continue;
        }
        let id = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read_to_string(&manifest).expect("the manifest reads");
        let version = raw_value(&bytes)
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        out.push((format!("connectors/{id}@{version}"), bytes));
    }
    out
}

/// `(name, text)` of a text, without the panic of `documents` (the writer
/// may see an empty file).
fn documents_text(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some(doc) = current.take() {
                out.push(doc);
            }
            current = Some((name.to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(doc) = current.take() {
        out.push(doc);
    }
    out
}

/// The merge: the frozen documents kept byte for byte, the official
/// manifests that are missing appended.
fn merge_connectors(frozen: &str, disk: &[(String, String)]) -> String {
    let mut existing: std::collections::BTreeSet<String> =
        documents_text(frozen).into_iter().map(|(n, _)| n).collect();
    let mut out = frozen.to_string();
    for (name, bytes) in disk {
        if existing.insert(name.clone()) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARKER);
            out.push_str(name);
            out.push('\n');
            out.push_str(bytes);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// The one writer of the person-files group: the official connectors'
/// manifests copied verbatim into `connectors.yaml`.
#[test]
fn write_freeze() {
    if !write_freeze_enabled() {
        return;
    }
    let disk = official_manifests();
    let frozen = fixture("connectors.yaml");
    let proposed = merge_connectors(&frozen, &disk);
    // Every check over the proposed snapshot.
    let docs = documents_text(&proposed);
    let mut names = std::collections::BTreeSet::new();
    for (name, text) in &docs {
        assert!(names.insert(name.clone()), "duplicate document {name}");
        let expected_name = name
            .rsplit_once('/')
            .and_then(|(_, n)| n.rsplit_once('@').map(|(n, _)| n.to_string()))
            .unwrap_or_default();
        apb_core::connector::ConnectorDoc::from_yaml(text.trim_end(), &expected_name)
            .unwrap_or_else(|e| panic!("the document {name} does not parse: {e}"));
    }
    for (name, bytes) in &disk {
        let doc = docs
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("connectors.yaml lacks {name}"));
        assert_eq!(doc.1.trim_end(), bytes.trim_end(), "{name} differs");
    }
    // Write, atomically.
    let tmp = fixtures_dir().join("connectors.yaml.tmp-freeze");
    std::fs::write(&tmp, &proposed).expect("the freeze temp file writes");
    std::fs::rename(&tmp, fixtures_dir().join("connectors.yaml")).expect("the freeze rename");
    panic!(
        "freeze written: connectors.yaml under {}; review the diff and run the test again without APB_WRITE_FREEZE",
        fixtures_dir().display()
    );
}
