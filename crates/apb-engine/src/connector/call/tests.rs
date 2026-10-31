//! Unit tests of the call path's pure stages: rendering, projection,
//! encoding, URL hardening, status mapping and redaction.

use super::encode::{base64_encode, encode_form_body, project};
use super::response::{map_status, reclassify_error_when, redact_value};
use super::*;

// -- error_when reclassification (spec
// 2026-07-22-official-connectors-wave-3, section 4.2) --

fn rule(
    path: &str,
    equals: Value,
    message_path: Option<&str>,
) -> apb_core::connector::def::ErrorWhen {
    apb_core::connector::def::ErrorWhen {
        path: path.to_string(),
        equals,
        message_path: message_path.map(str::to_string),
    }
}

#[test]
fn error_when_match_reclassifies_to_service_with_extracted_message() {
    let r = rule("ok", json!(false), Some("error"));
    let body = json!({ "ok": false, "error": "missing_scope" });
    let err = reclassify_error_when(&r, &body, 200).unwrap();
    assert_eq!(err.code, CallErrorCode::Service);
    assert_eq!(err.message, "missing_scope");
    // The error carries the real HTTP status of the reclassified response.
    assert_eq!(err.http_status, Some(200));
}

#[test]
fn error_when_no_match_passes_through() {
    let r = rule("ok", json!(false), Some("error"));
    assert!(reclassify_error_when(&r, &json!({ "ok": true }), 200).is_none());
}

#[test]
fn error_when_path_resolving_to_nothing_means_no_match() {
    let r = rule("ok", json!(false), Some("error"));
    assert!(reclassify_error_when(&r, &json!({ "status": "fine" }), 200).is_none());
    // A non-object midway through the chain resolves to nothing too.
    let nested = rule("a.b", json!(false), None);
    assert!(reclassify_error_when(&nested, &json!({ "a": 1 }), 200).is_none());
}

#[test]
fn error_when_nested_paths_resolve_over_objects() {
    let r = rule("result.ok", json!(false), Some("result.detail.message"));
    let body = json!({ "result": { "ok": false, "detail": { "message": "bad" } } });
    let err = reclassify_error_when(&r, &body, 201).unwrap();
    assert_eq!(err.message, "bad");
    assert_eq!(err.http_status, Some(201));
}

#[test]
fn error_when_fallback_message_when_message_path_absent_or_not_string() {
    // No message_path declared at all.
    let none = rule("ok", json!(false), None);
    let fallback = reclassify_error_when(&none, &json!({ "ok": false }), 200)
        .unwrap()
        .message;
    assert!(!fallback.is_empty());
    // message_path resolves to nothing.
    let r = rule("ok", json!(false), Some("error"));
    let missing = reclassify_error_when(&r, &json!({ "ok": false }), 200).unwrap();
    assert_eq!(missing.message, fallback);
    // message_path resolves to a non-string.
    let non_string = reclassify_error_when(&r, &json!({ "ok": false, "error": 42 }), 200).unwrap();
    assert_eq!(non_string.message, fallback);
}

#[test]
fn error_when_equals_compares_by_json_equality() {
    let s = rule("status", json!("error"), None);
    assert!(reclassify_error_when(&s, &json!({ "status": "error" }), 200).is_some());
    assert!(reclassify_error_when(&s, &json!({ "status": "fine" }), 200).is_none());
    // A number does not equal its string form.
    let n = rule("code", json!(1), None);
    assert!(reclassify_error_when(&n, &json!({ "code": 1 }), 200).is_some());
    assert!(reclassify_error_when(&n, &json!({ "code": "1" }), 200).is_none());
}

#[test]
fn render_http_builds_method_url_body_and_headers_offline() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: api_base\n    required: true\nfunctions:\n  - name: create\n    description: d\n    method: POST\n    url: \"{{account.api_base}}/items/{{args.id}}\"\n    body: \"{{args}}\"\n    headers: { X-Trace: \"{{args.id}}\" }\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("create").unwrap();
    let mut account = BTreeMap::new();
    account.insert(
        "api_base".to_string(),
        "https://api.example.com".to_string(),
    );
    let args = json!({ "id": "4 2", "title": "Hi" });
    let secrets = BTreeMap::new();
    let r = render_http(f, &account, &args, &secrets, None).unwrap();
    assert_eq!(r.method, "POST");
    // args are percent-encoded into the URL path (space -> %20).
    assert_eq!(r.pre_auth_url, "https://api.example.com/items/4%202");
    assert_eq!(r.rendered_body, Some(json!({ "id": "4 2", "title": "Hi" })));
    // The function header renders, and the default User-Agent is applied.
    assert_eq!(r.headers.get("X-Trace").map(String::as_str), Some("4 2"));
    assert_eq!(
        r.headers.get("User-Agent").map(String::as_str),
        Some(USER_AGENT)
    );
}

#[test]
fn render_http_lets_a_function_header_override_the_default_user_agent() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: g\n    description: d\n    method: GET\n    url: \"https://api.example.com/x\"\n    headers: { User-Agent: custom-ua }\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("g").unwrap();
    let r = render_http(f, &BTreeMap::new(), &json!({}), &BTreeMap::new(), None).unwrap();
    assert_eq!(
        r.headers.get("User-Agent").map(String::as_str),
        Some("custom-ua")
    );
}

#[test]
fn render_query_drops_absent_single_placeholder_pair() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { offset: \"{{args.offset}}\", limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("list").unwrap();
    let args = json!({ "limit": 50 });
    let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None).unwrap();
    assert_eq!(r.pre_auth_url, "https://api.example.com/items?limit=50");
}

#[test]
fn render_query_typed_scalars() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { active: \"{{args.active}}\", limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("list").unwrap();
    let args = json!({ "limit": 50, "active": true });
    let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None).unwrap();
    assert_eq!(
        r.pre_auth_url,
        "https://api.example.com/items?active=true&limit=50"
    );
}

#[test]
fn render_query_non_scalar_single_placeholder_is_config_error() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("list").unwrap();
    let args = json!({ "limit": [1] });
    match render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None) {
        Ok(_) => panic!("expected a config error for a non-scalar query arg"),
        Err(err) => assert_eq!(err.code, CallErrorCode::Config),
    }
}

#[test]
fn render_query_null_arg_drops_pair() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list\n    description: d\n    method: GET\n    url: \"https://api.example.com/items\"\n    query: { offset: \"{{args.offset}}\", limit: \"{{args.limit}}\" }\n    args_schema: { type: object }\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("list").unwrap();
    // `offset` is explicitly null (not merely absent): it must drop the
    // pair exactly like an absent arg, not render a literal `null`.
    let args = json!({ "offset": null, "limit": 50 });
    let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None).unwrap();
    assert_eq!(r.pre_auth_url, "https://api.example.com/items?limit=50");
}

// -- body_form rendering (spec 2026-07-22-official-connectors-wave-3, 4.1) --

/// A POST function whose `body_form` mixes a literal, single placeholders,
/// and a mixed-content template.
fn form_function(body_form_yaml: &str) -> apb_core::connector::def::ConnectorDoc {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = format!(
        "name: x\nversion: 0.1.0\nfunctions:\n  - name: send\n    description: d\n    method: POST\n    url: \"https://api.example.com/messages\"\n    body_form:\n{body_form_yaml}    args_schema: {{ type: object }}\n"
    );
    ConnectorDoc::from_yaml(&yaml, "x").unwrap()
}

#[test]
fn render_http_without_body_form_renders_no_form() {
    use apb_core::connector::def::ConnectorDoc;
    let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: g\n    description: d\n    method: GET\n    url: \"https://api.example.com/x\"\n";
    let doc = ConnectorDoc::from_yaml(yaml, "x").unwrap();
    let f = doc.function("g").unwrap();
    let r = render_http(f, &BTreeMap::new(), &json!({}), &BTreeMap::new(), None).unwrap();
    assert!(r.rendered_form.is_none());
}

#[test]
fn render_form_typed_single_placeholders_render_canonical_strings() {
    let doc = form_function(
        "      active: \"{{args.active}}\"\n      limit: \"{{args.limit}}\"\n      name: \"{{args.name}}\"\n",
    );
    let f = doc.function("send").unwrap();
    let args = json!({ "active": true, "limit": 50, "name": "hi" });
    let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None).unwrap();
    assert_eq!(
        r.rendered_form,
        Some(vec![
            ("active".to_string(), "true".to_string()),
            ("limit".to_string(), "50".to_string()),
            ("name".to_string(), "hi".to_string()),
        ])
    );
}

#[test]
fn render_form_drops_absent_and_null_single_placeholder_pairs() {
    let doc = form_function(
        "      content: \"{{args.content}}\"\n      queue_id: \"{{args.queue_id}}\"\n      topic: \"{{args.topic}}\"\n",
    );
    let f = doc.function("send").unwrap();
    // `queue_id` is absent, `topic` is explicitly null: both pairs drop.
    let args = json!({ "content": "hello", "topic": null });
    let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None).unwrap();
    assert_eq!(
        r.rendered_form,
        Some(vec![("content".to_string(), "hello".to_string())])
    );
}

#[test]
fn render_form_non_scalar_single_placeholder_is_config_error() {
    let doc = form_function("      content: \"{{args.content}}\"\n");
    let f = doc.function("send").unwrap();
    let args = json!({ "content": [1, 2] });
    match render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None) {
        Ok(_) => panic!("expected a config error for a non-scalar form arg"),
        Err(err) => assert_eq!(err.code, CallErrorCode::Config),
    }
}

#[test]
fn render_form_mixed_template_absent_arg_is_config_error() {
    // Mixed content keeps string interpolation with absent-arg-is-error;
    // only an exact single placeholder gets the drop rule.
    let doc = form_function("      content: \"v={{args.gone}}\"\n");
    let f = doc.function("send").unwrap();
    match render_http(f, &BTreeMap::new(), &json!({}), &BTreeMap::new(), None) {
        Ok(_) => panic!("expected a config error for an absent arg in mixed content"),
        Err(err) => assert_eq!(err.code, CallErrorCode::Config),
    }
}

#[test]
fn render_form_pairs_are_decoded_and_literals_kept() {
    // The rendered pairs stay DECODED (encoding happens at send time), and
    // a literal (placeholder-free) value renders verbatim.
    let doc = form_function("      content: \"{{args.content}}\"\n      type: stream\n");
    let f = doc.function("send").unwrap();
    let args = json!({ "content": "hello world & more" });
    let r = render_http(f, &BTreeMap::new(), &args, &BTreeMap::new(), None).unwrap();
    assert_eq!(
        r.rendered_form,
        Some(vec![
            ("content".to_string(), "hello world & more".to_string()),
            ("type".to_string(), "stream".to_string()),
        ])
    );
}

#[test]
fn encode_form_body_percent_encodes_keys_and_values() {
    let pairs = vec![
        ("a key".to_string(), "b&c=d".to_string()),
        ("content".to_string(), "hello world/100%".to_string()),
    ];
    assert_eq!(
        encode_form_body(&pairs),
        "a%20key=b%26c%3Dd&content=hello%20world%2F100%25"
    );
}

#[test]
fn project_keeps_named_object_fields_and_nested_paths() {
    let body = json!({
        "number": 7, "title": "t", "extra": "drop",
        "user": { "login": "octo", "id": 1 }
    });
    let picked = project(
        &body,
        &["number".into(), "title".into(), "user.login".into()],
    );
    assert_eq!(
        picked,
        json!({ "number": 7, "title": "t", "user": { "login": "octo" } })
    );
}

#[test]
fn project_maps_over_arrays_at_top_level_and_midway() {
    let body = json!([
        { "number": 1, "labels": [ { "name": "bug", "color": "red" }, { "name": "p1" } ], "x": 9 },
        { "number": 2, "labels": [] }
    ]);
    let picked = project(&body, &["number".into(), "labels.name".into()]);
    assert_eq!(
        picked,
        json!([
            { "number": 1, "labels": [ { "name": "bug" }, { "name": "p1" } ] },
            { "number": 2, "labels": [] }
        ])
    );
}

#[test]
fn project_drops_missing_paths_silently() {
    let body = json!({ "a": 1 });
    let picked = project(&body, &["a".into(), "b.c".into()]);
    assert_eq!(picked, json!({ "a": 1 }));
}

#[test]
fn base64_matches_known_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
}

#[test]
fn validate_url_accepts_http_and_https() {
    assert!(validate_url("http://example.com/x").is_ok());
    assert!(validate_url("https://EXAMPLE.com/x?a=b").is_ok());
}

#[test]
fn validate_url_rejects_non_http_scheme_and_userinfo() {
    assert!(validate_url("ftp://example.com").is_err());
    assert!(validate_url("https://user:pass@example.com/x").is_err());
    assert!(validate_url("relative/path").is_err());
    // An `@` only in the path (after the authority) is allowed.
    assert!(validate_url("https://example.com/a@b").is_ok());
}

#[test]
fn map_status_classifies_the_taxonomy() {
    assert!(map_status(200, json!({}), None, None, false).is_ok());
    assert_eq!(
        map_status(302, json!({}), None, None, false)
            .unwrap_err()
            .code,
        CallErrorCode::Service
    );
    assert_eq!(
        map_status(401, json!({}), None, None, false)
            .unwrap_err()
            .code,
        CallErrorCode::Auth
    );
    assert_eq!(
        map_status(404, json!({}), None, None, false)
            .unwrap_err()
            .code,
        CallErrorCode::NotFound
    );
    let rl = map_status(429, json!({}), None, Some(30), false).unwrap_err();
    assert_eq!(rl.code, CallErrorCode::RateLimited);
    assert_eq!(rl.retry_after_sec, Some(30));
    assert_eq!(
        map_status(503, json!({}), None, None, false)
            .unwrap_err()
            .code,
        CallErrorCode::Service
    );
}

#[test]
fn map_status_fails_a_read_only_incomplete_body_without_returning_it() {
    let cut = BodyIncomplete::cut_at_cap(BODY_CAP);
    let err = map_status(200, json!({"partial": true}), Some(cut), None, true).unwrap_err();
    assert_eq!(err.code, CallErrorCode::IncompleteResponse);
    assert_eq!(err.http_status, Some(200));
    assert_eq!(err.incomplete.as_ref().unwrap().reason.as_str(), "body_cap");

    // The same incompleteness on a function that may have changed the
    // world stays a success and carries it on the result instead.
    let cut = BodyIncomplete::cut_at_cap(BODY_CAP);
    let ok = map_status(200, json!({"partial": true}), Some(cut), None, false).unwrap();
    assert!(ok.response_complete() == Some(false));

    // A whole body is a success either way.
    assert!(map_status(200, json!({}), None, None, true).is_ok());
}

#[test]
fn redact_replaces_secret_in_string_leaves() {
    let redactions = vec![Redaction::new(Secret::new("shh-1"), "MOCK_TOKEN")];
    let body = json!({ "echo": "prefix shh-1 suffix", "n": 3, "list": ["shh-1"] });
    let out = redact_value(body, &redactions);
    assert_eq!(
        out,
        json!({ "echo": "prefix [redacted:MOCK_TOKEN] suffix", "n": 3, "list": ["[redacted:MOCK_TOKEN]"] })
    );
}
