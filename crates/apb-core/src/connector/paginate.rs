//! Declared, bounded paging for read-only HTTP functions: the `paginate:`
//! block of a `connector.yaml` function, its manifest rules, and the shared
//! constants of the walk. The walk itself is the engine's
//! `apb_engine::connector::call::paginate`.

use serde::{Deserialize, Serialize};

/// The default page limit of a `paginate` block.
pub const DEFAULT_MAX_PAGES: u32 = 5;
/// The largest page limit a manifest may declare.
pub const MAX_PAGES_CAP: u32 = 20;
/// The raw bytes all pages of one call may read together.
pub const PAGES_BYTE_CAP: usize = 4 * 1024 * 1024;

fn default_max_pages() -> u32 {
    DEFAULT_MAX_PAGES
}

/// How the walk finds the next page.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Paginate {
    /// Dot path of the item array in each page; "" means the body itself.
    #[serde(default)]
    pub items: String,
    pub next: PageNext,
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
    /// An absent `items` field reads as an empty page (Gmail leaves
    /// `messages` out when nothing matched). A present non-array is still
    /// an error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub items_optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageNext {
    /// Follow the `Link` header's `rel="next"` URL.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub link_header: bool,
    /// Dot path of the next-page token in the body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_path: Option<String>,
    /// The string arg the token is passed back in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_arg: Option<String>,
}

/// The manifest rules of a `paginate` block, called from
/// `ConnectorDoc::from_yaml` per function.
/// The facts `validate` needs about the declaring function and connector,
/// gathered by the caller (`ConnectorDoc::from_yaml`). A plain-data struct
/// (ADP): `paginate.rs` stays a leaf (serde only) and never imports
/// `def.rs`, which holds the `FunctionSpec.paginate` field.
#[derive(Debug, Clone, Copy)]
pub struct FunctionFacts<'a> {
    /// The function's name, for the error messages.
    pub function: &'a str,
    /// The function declares `method` and `url` (an HTTP function).
    pub is_http: bool,
    /// The method is `GET` (case-insensitive).
    pub is_get: bool,
    /// The function is `read_only: true`.
    pub read_only: bool,
    /// The connector's `identity` names this function.
    pub is_identity: bool,
    /// Some function's `reconcile` names this function.
    pub is_reconcile: bool,
    /// The connector's auth kind is `path` or `query` (the credential
    /// lives in the URL).
    pub url_auth: bool,
    /// The function's `args_schema`, for the token-arg property check.
    pub args_schema: Option<&'a serde_json::Value>,
    /// The function's `query` values (placeholders included), in map order.
    pub query_values: &'a [String],
}

pub fn validate(f: &FunctionFacts<'_>, p: &Paginate) -> Result<(), super::common::ConnectorError> {
    use super::common::ConnectorError;

    let name = f.function;
    let refused = |why: String| ConnectorError::Invalid(format!("function `{name}`: {why}"));

    // Rule 1: a read-only HTTP GET function, and never a function the
    // connector reads its own sends back through (an identity lookup or a
    // reconcile reads one page and says next_page; paging it would multiply
    // a lookup, not a listing).
    if !f.is_http {
        return Err(refused(
            "`paginate` needs an HTTP function (method + url)".to_string(),
        ));
    }
    if !f.read_only {
        return Err(refused(
            "`paginate` needs a `read_only` function; repeating a write per page would multiply its effect".to_string(),
        ));
    }
    if !f.is_get {
        return Err(refused(
            "`paginate` needs a GET function; the lists it serves are all GET".to_string(),
        ));
    }
    if f.is_identity || f.is_reconcile {
        return Err(refused(
            "`paginate` is refused on the function the connector reads its own sends back through (identity or reconcile)".to_string(),
        ));
    }

    // Rule 2: exactly one of a Link header or a body token pair.
    let link = p.next.link_header;
    let token = p.next.token_path.is_some() || p.next.token_arg.is_some();
    if link == token {
        return Err(refused(
            "`paginate.next` needs exactly one of `link_header: true` or both `token_path` and `token_arg`".to_string(),
        ));
    }
    if !link && (p.next.token_path.is_none() || p.next.token_arg.is_none()) {
        return Err(refused(
            "`paginate.next.token_path` needs `token_arg` beside it".to_string(),
        ));
    }

    // Rule 3: the token arg is a string property of args_schema wired into
    // the query as exactly `{{args.<token_arg>}}`.
    if let Some(arg) = &p.next.token_arg {
        let string_prop = f
            .args_schema
            .and_then(|s| s.get("properties"))
            .and_then(|props| props.get(arg))
            .and_then(|prop| prop.get("type"))
            .and_then(|t| t.as_str())
            .is_some_and(|t| t == "string");
        if !string_prop {
            return Err(refused(format!(
                "`paginate.next.token_arg` `{arg}` must be a string property of `args_schema`"
            )));
        }
        let wanted = format!("{{{{args.{arg}}}}}");
        let wired = f.query_values.iter().any(|v| v.as_str() == wanted);
        if !wired {
            return Err(refused(format!(
                "`paginate.next.token_arg` `{arg}` must appear in `query` as exactly the value `{{{{args.{arg}}}}}`, so the next page renders like the first"
            )));
        }
    }

    // Rule 4: `link_header` is refused when the connector's auth is `path`
    // or `query`: the next URL is taken from the service, and it must not
    // be sent a credential that lives in the URL.
    if p.next.link_header && f.url_auth {
        return Err(refused(
            "`paginate.next.link_header` is refused when the connector carries its credential in the URL (auth `path` or `query`)".to_string(),
        ));
    }

    // Rule 5: `items` and `token_path` segments are non-empty when the
    // path is not "".
    if !p.items.is_empty() && p.items.split('.').any(str::is_empty) {
        return Err(refused(format!(
            "`paginate.items` path `{}` has an empty segment",
            p.items
        )));
    }
    if let Some(path) = &p.next.token_path
        && path.split('.').any(str::is_empty)
    {
        return Err(refused(format!(
            "`paginate.next.token_path` path `{path}` has an empty segment"
        )));
    }

    // Rule 6: the page limit is bounded.
    if p.max_pages == 0 || p.max_pages > MAX_PAGES_CAP {
        return Err(refused(format!(
            "`paginate.max_pages` must be 1 to {MAX_PAGES_CAP}, not {}",
            p.max_pages
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::def::ConnectorDoc;
    use super::*;

    /// A minimal paging manifest: `fn_lines` carries the function's extra
    /// lines (the paginate block, a schema, query wiring), `auth` and
    /// `fields` vary per rule, and `url_line` replaces the default so a
    /// path-auth connector can carry its `{{auth}}` placeholder.
    fn yaml_for(fn_lines: &str, auth: &str, fields: &str, head_lines: &str) -> String {
        format!(
            r#"
name: pager
version: 0.1.0
{auth}
account_fields:
{fields}
functions:
  - name: list_things
    description: d
    read_only: true
{head_lines}
    response_pick: [items]
{fn_lines}
"#
        )
    }

    fn link_yaml(fn_lines: &str, auth: &str, fields: &str) -> String {
        yaml_for(
            fn_lines,
            auth,
            fields,
            "    method: GET\n    url: \"http://a/things\"",
        )
    }

    const NO_AUTH: &str = "";
    const PLAIN_FIELDS: &str = "  - name: token\n    required: true\n    secret: true\n";
    const LINK_BLOCK: &str =
        "    paginate:\n      items: \"\"\n      next: { link_header: true }\n";
    const TOKEN_WIRING: &str = "    args_schema: { type: object, properties: { offset: { type: string } } }\n    query: { offset: \"{{args.offset}}\" }\n";

    #[test]
    fn a_valid_link_block_and_a_valid_token_block_load() {
        let y = link_yaml(LINK_BLOCK, NO_AUTH, "");
        let doc = ConnectorDoc::from_yaml(&y, "pager").unwrap();
        let p = doc
            .function("list_things")
            .unwrap()
            .paginate
            .clone()
            .unwrap();
        assert_eq!(p.items, "");
        assert!(p.next.link_header);
        assert_eq!(p.max_pages, DEFAULT_MAX_PAGES);
        assert!(!p.items_optional);

        let token_block = "    paginate:\n      items: items\n      next: { token_path: next_page.offset, token_arg: offset }\n      max_pages: 9\n      items_optional: true\n";
        let y = link_yaml(&format!("{token_block}{TOKEN_WIRING}"), NO_AUTH, "");
        let doc = ConnectorDoc::from_yaml(&y, "pager").unwrap();
        let p = doc
            .function("list_things")
            .unwrap()
            .paginate
            .clone()
            .unwrap();
        assert_eq!(p.next.token_path.as_deref(), Some("next_page.offset"));
        assert_eq!(p.next.token_arg.as_deref(), Some("offset"));
        assert_eq!(p.max_pages, 9);
        assert!(p.items_optional);
        // Round trip through a full-document serialize.
        let again =
            ConnectorDoc::from_yaml(&yaml_serde::to_string(&doc).unwrap(), "pager").unwrap();
        assert_eq!(
            again.function("list_things").unwrap().paginate,
            doc.function("list_things").unwrap().paginate
        );
        // items_optional serializes only when true.
        let y = link_yaml(LINK_BLOCK, NO_AUTH, "");
        let text = yaml_serde::to_string(
            ConnectorDoc::from_yaml(&y, "pager")
                .unwrap()
                .function("list_things")
                .unwrap(),
        )
        .unwrap();
        assert!(!text.contains("items_optional"), "{text}");
    }

    #[test]
    fn rule_1_paging_needs_a_read_only_http_get() {
        // read_only kept, method POST: refused, and the message names GET.
        let y = yaml_for(
            LINK_BLOCK,
            NO_AUTH,
            "",
            "    method: POST\n    url: \"http://a/things\"",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("GET"), "{err}");

        // Not read_only: refused as well.
        let y = link_yaml(LINK_BLOCK, NO_AUTH, "").replace("    read_only: true\n", "");
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("read_only"), "{err}");
    }

    #[test]
    fn rule_2_exactly_one_next_kind() {
        // Neither.
        let y = link_yaml(
            "    paginate:\n      items: items\n      next: {}\n",
            NO_AUTH,
            "",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("next"), "{err}");

        // Both.
        let y = link_yaml(
            "    paginate:\n      items: items\n      next: { link_header: true, token_path: t, token_arg: offset }\n",
            NO_AUTH,
            "",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("next"), "{err}");

        // Token path without the arg.
        let y = link_yaml(
            "    paginate:\n      items: items\n      next: { token_path: t }\n",
            NO_AUTH,
            "",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("token_arg"), "{err}");
    }

    #[test]
    fn rule_3_token_arg_needs_schema_and_query_wiring() {
        // Not a string property of args_schema.
        let y = link_yaml(
            "    paginate:\n      items: items\n      next: { token_path: t, token_arg: offset }\n",
            NO_AUTH,
            "",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("offset"), "{err}");

        // Wired schema but no query pair.
        let y = link_yaml(
            "    paginate:\n      items: items\n      next: { token_path: t, token_arg: offset }\n    args_schema: { type: object, properties: { offset: { type: string } } }\n",
            NO_AUTH,
            "",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("query"), "{err}");
    }

    #[test]
    fn rule_4_link_header_is_refused_for_url_carrying_auth() {
        for (auth, head) in [
            (
                "auth:\n  kind: path\n  value_template: \"{{secret.token}}\"\n",
                "    method: GET\n    url: \"http://a/{{auth}}\"",
            ),
            (
                "auth:\n  kind: query\n  param: api_key\n  value_template: \"{{secret.token}}\"\n",
                "    method: GET\n    url: \"http://a/things\"",
            ),
        ] {
            let y = yaml_for(LINK_BLOCK, auth, PLAIN_FIELDS, head);
            let err = ConnectorDoc::from_yaml(&y, "pager")
                .unwrap_err()
                .to_string();
            assert!(err.contains("link_header"), "{err}: {y}");
            // A token block on the same connector is fine.
            let token = "    paginate:\n      items: \"\"\n      next: { token_path: t, token_arg: offset }\n";
            let y = yaml_for(&format!("{token}{TOKEN_WIRING}"), auth, PLAIN_FIELDS, head);
            assert!(
                ConnectorDoc::from_yaml(&y, "pager").is_ok(),
                "token paging must be fine for {auth}: {y}"
            );
        }
    }

    #[test]
    fn rule_5_paths_have_non_empty_segments() {
        let y = link_yaml(
            "    paginate:\n      items: items..x\n      next: { link_header: true }\n",
            NO_AUTH,
            "",
        );
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("items"), "{err}");
        let fn_lines = format!(
            "{TOKEN_WIRING}    paginate:\n      items: items\n      next: {{ token_path: a..b, token_arg: offset }}\n"
        );
        let y = link_yaml(&fn_lines, NO_AUTH, "");
        let err = ConnectorDoc::from_yaml(&y, "pager")
            .unwrap_err()
            .to_string();
        assert!(err.contains("token_path"), "{err}");
    }

    #[test]
    fn rule_6_max_pages_is_bounded() {
        for n in [0, 21] {
            let y = link_yaml(
                &format!(
                    "    paginate:\n      items: \"\"\n      next: {{ link_header: true }}\n      max_pages: {n}\n"
                ),
                NO_AUTH,
                "",
            );
            let err = ConnectorDoc::from_yaml(&y, "pager")
                .unwrap_err()
                .to_string();
            assert!(err.contains("max_pages"), "{err}");
        }
    }
}
