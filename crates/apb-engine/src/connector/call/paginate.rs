//! The bounded page walk (D2): one GET per page, the item arrays of every
//! page concatenated in order, the result built from the LAST page's body
//! around the merged items, and three hard bounds: `max_pages`, the byte
//! cap over all pages, and the call's one deadline (held by the caller's
//! `fetch`). Pure over its closures, so the offline contract runner drives
//! it with scripted responses.

use apb_core::connector::paginate::{PAGES_BYTE_CAP, Paginate};
use serde_json::Value;

use crate::connector::call::response::lookup_path;
use crate::connector::call::shape::{RawResponse, ShapeRules, shape};
use crate::connector::call::wire::BODY_CAP;
use crate::connector::completeness::BodyIncomplete;
use crate::connector::result::{CallError, CallErrorCode, CallOk};

/// What the walk did, on success and on failure alike: the pages attempted
/// (the failed one included) and the last service status that arrived.
#[derive(Debug)]
pub(crate) struct WalkStats {
    pub pages: u32,
    pub last_status: Option<u16>,
}

/// A finished walk over read-only pages.
#[derive(Debug)]
pub(crate) struct Walked {
    pub ok: CallOk,
    /// Another page exists past what this call fetched.
    pub more: bool,
}

/// `fetch(url, allowance)` performs one GET of `url`, reading its body with
/// `allowance` bytes; `next_url(token)` renders the next request URL of a
/// token page. Pure over both.
pub(crate) fn walk(
    first_url: &str,
    spec: &Paginate,
    rules: &ShapeRules<'_>,
    fetch: &mut dyn FnMut(&str, usize) -> Result<RawResponse, CallError>,
    next_url: &mut dyn FnMut(&str) -> Result<String, CallError>,
) -> (Result<Walked, CallError>, WalkStats) {
    let mut stats = WalkStats {
        pages: 0,
        last_status: None,
    };
    let mut total = 0usize;
    let mut url = first_url.to_string();
    // The last page's facts and body: the call's answer is built from them
    // once the walk stops.
    /// The facts the final answer keeps from the last page.
    type PageFacts = (u16, Option<BodyIncomplete>, Option<String>);
    let mut last: Option<(PageFacts, Value)> = None;
    let mut all_items: Vec<Value> = Vec::new();
    let mut more = false;

    loop {
        stats.pages += 1;
        let page = stats.pages;
        let allowance = BODY_CAP.min(PAGES_BYTE_CAP.saturating_sub(total));
        let raw = match fetch(&url, allowance) {
            Ok(raw) => raw,
            Err(e) => return (Err(prefixed(page, e)), stats),
        };
        total += raw.bytes;
        stats.last_status = Some(raw.status);
        let ok = match shape(raw, rules) {
            Ok(ok) => ok,
            Err(e) => return (Err(prefixed(page, e)), stats),
        };
        // shape only ever produces an HTTP result; a non-Http here cannot
        // happen, so the page is treated as ended.
        let CallOk::Http {
            body,
            link,
            status,
            incomplete,
            ..
        } = ok
        else {
            break;
        };
        // The pieces the final answer keeps from this page when it is the
        // last one.
        let page_facts = (status, incomplete, link.clone());
        let items = match take_items(spec, &body, page) {
            Ok(items) => items,
            Err(e) => return (Err(prefixed(page, e)), stats),
        };
        all_items.extend(items);

        // The next page: a Link header URL or a body token. Absent, null or
        // empty means the listing ended here.
        let next: Option<NextRef> = if spec.next.link_header {
            link.as_deref().and_then(parse_link_next).map(NextRef::Url)
        } else {
            spec.next
                .token_path
                .as_deref()
                .and_then(|path| lookup_path(&body, path))
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
                .map(|t| NextRef::Token(t.to_string()))
        };
        let Some(next) = next else {
            last = Some((page_facts, body));
            more = false;
            break;
        };
        // The page just fetched fills the budget: a further page would read
        // no bytes, so the walk stops here and says more.
        if page == spec.max_pages || total >= PAGES_BYTE_CAP {
            last = Some((page_facts, body));
            more = true;
            break;
        }
        url = match next {
            NextRef::Url(next) => {
                if !same_origin(first_url, &next) {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Service,
                            "the service pointed the next page at another host".to_string(),
                        )),
                        stats,
                    );
                }
                next
            }
            NextRef::Token(token) => match next_url(&token) {
                Ok(u) => u,
                Err(e) => return (Err(prefixed(page + 1, e)), stats),
            },
        };
    }

    let Some(((status, incomplete, link), body)) = last else {
        // Unreachable: every loop exit above either broke with `last` set
        // or returned.
        return (
            Err(CallError::new(
                CallErrorCode::Service,
                "the listing produced no page".to_string(),
            )),
            stats,
        );
    };
    let mut body = body;
    if !set_at_path(&mut body, spec, all_items) {
        return (
            Err(CallError::new(
                CallErrorCode::Service,
                format!(
                    "the last page of the listing does not hold the item list at `{}`",
                    spec.items
                ),
            )),
            stats,
        );
    }
    (
        Ok(Walked {
            ok: CallOk::Http {
                status,
                body,
                incomplete,
                link,
                picked: false,
                pages: None,
            },
            more,
        }),
        stats,
    )
}

/// What carries the walk to the next page.
enum NextRef {
    Url(String),
    Token(String),
}

/// Prefixes the error of page `page` and later with its page number.
fn prefixed(page: u32, mut err: CallError) -> CallError {
    if page >= 2 {
        err.message = format!("page {page} of the listing: {}", err.message);
    }
    err
}

/// The item array of one page at the spec's path: `""` means the body
/// itself. A present non-array is an error whatever `items_optional` says;
/// an absent field is an empty page only under `items_optional`.
fn take_items(spec: &Paginate, body: &Value, page: u32) -> Result<Vec<Value>, CallError> {
    let slot = if spec.items.is_empty() {
        Some(body)
    } else {
        lookup_path(body, &spec.items)
    };
    match slot {
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err(CallError::new(
            CallErrorCode::Service,
            format!("page {page} has no item list at `{}`", spec.items),
        )),
        None if spec.items_optional => Ok(Vec::new()),
        None => Err(CallError::new(
            CallErrorCode::Service,
            format!("page {page} has no item list at `{}`", spec.items),
        )),
    }
}

/// Replaces the value at the spec's `items` path with the merged items;
/// `""` replaces the whole body. Under `items_optional` an absent final
/// key is inserted, because `take_items` already read that same absence as
/// an empty page (Gmail leaves `messages` out when nothing matched, and
/// the result still carries the empty list). `false` keeps meaning a
/// segment walks into a non-object, an intermediate key is absent, or a
/// non-optional walk misses the final key.
fn set_at_path(body: &mut Value, spec: &Paginate, items: Vec<Value>) -> bool {
    if spec.items.is_empty() {
        *body = Value::Array(items);
        return true;
    }
    let segments: Vec<&str> = spec.items.split('.').collect();
    let last = segments.len() - 1;
    let mut cur = body;
    for (at, seg) in segments.iter().enumerate() {
        let Some(map) = cur.as_object_mut() else {
            return false;
        };
        if at == last {
            if map.contains_key(*seg) || spec.items_optional {
                map.insert((*seg).to_string(), Value::Array(items));
                return true;
            }
            return false;
        }
        match map.get_mut(*seg) {
            Some(v) => cur = v,
            None => return false,
        }
    }
    unreachable!("the loop returns on the last segment")
}

/// The `rel="next"` URL of a `Link` header: comma-separated entries
/// `<url>; rel="next"`, the rel quoted or not, a space-separated rel list
/// counting when it holds `next`.
pub(crate) fn parse_link_next(link: &str) -> Option<String> {
    for entry in link.split(',') {
        let entry = entry.trim();
        let Some(rest) = entry.strip_prefix('<') else {
            continue;
        };
        let Some((url, params)) = rest.split_once('>') else {
            continue;
        };
        for param in params.split(';') {
            let Some((k, v)) = param.split_once('=') else {
                continue;
            };
            if !k.trim().eq_ignore_ascii_case("rel") {
                continue;
            }
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|r| r.strip_suffix('"'))
                .unwrap_or(v);
            if v.split_whitespace().any(|r| r.eq_ignore_ascii_case("next")) {
                return Some(url.trim().to_string());
            }
        }
    }
    None
}

/// The (scheme, host, port) of an absolute http(s) URL: the host
/// lower-cased, the default port filled in.
fn parts(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map(|(_, a)| a)
        .unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h, port),
            Err(_) => (authority, default),
        },
        None => (authority, default),
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme, host.to_ascii_lowercase(), port))
}

/// Whether `next` names the same scheme, host and port as `first`.
pub(crate) fn same_origin(first: &str, next: &str) -> bool {
    match (parts(first), parts(next)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::connector::paginate::{DEFAULT_MAX_PAGES, PageNext};
    use serde_json::json;

    fn rules() -> ShapeRules<'static> {
        static EMPTY: Vec<String> = Vec::new();
        ShapeRules {
            read_only: true,
            error_when: None,
            decode_paths: &EMPTY,
        }
    }

    fn link_spec(items: &str, max_pages: u32) -> Paginate {
        Paginate {
            items: items.to_string(),
            next: PageNext {
                link_header: true,
                token_path: None,
                token_arg: None,
            },
            max_pages,
            items_optional: false,
        }
    }

    fn token_spec(items: &str, token_path: &str, max_pages: u32) -> Paginate {
        Paginate {
            items: items.to_string(),
            next: PageNext {
                link_header: false,
                token_path: Some(token_path.to_string()),
                token_arg: Some("offset".to_string()),
            },
            max_pages,
            items_optional: false,
        }
    }

    fn raw(status: u16, body: Value) -> RawResponse {
        let bytes = serde_json::to_string(&body).unwrap_or_default().len();
        RawResponse {
            status,
            content_type: Some("application/json".to_string()),
            link: None,
            retry_after: None,
            body,
            incomplete: None,
            bytes,
        }
    }

    /// The requests the scripted fetch saw, shared with the test.
    type Seen = std::rc::Rc<std::cell::RefCell<Vec<(String, usize)>>>;

    fn fresh_seen() -> Seen {
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()))
    }

    /// A fetch over a scripted list: each call pops the next answer; a
    /// transport error is spelled `Err`. Records the urls and allowances.
    fn scripted(
        answers: Vec<Result<RawResponse, CallError>>,
        seen: Seen,
    ) -> impl FnMut(&str, usize) -> Result<RawResponse, CallError> {
        let mut answers = answers;
        move |url: &str, allowance: usize| {
            seen.borrow_mut().push((url.to_string(), allowance));
            match answers.is_empty() {
                false => answers.remove(0),
                true => Err(CallError::new(
                    CallErrorCode::Network,
                    "no scripted answer left".to_string(),
                )),
            }
        }
    }

    fn no_token_pages(_token: &str) -> Result<String, CallError> {
        Err(CallError::new(
            CallErrorCode::Config,
            "no token pages in this test".to_string(),
        ))
    }

    fn cut(cap: usize) -> Option<crate::connector::completeness::BodyIncomplete> {
        Some(crate::connector::completeness::BodyIncomplete {
            reason: crate::connector::completeness::IncompleteReason::BodyCap,
            cap_bytes: Some(cap),
            io_error_kind: None,
        })
    }

    #[test]
    fn link_parsing_covers_quoted_unquoted_lists_and_none() {
        assert_eq!(
            parse_link_next("<http://a/2>; rel=\"next\""),
            Some("http://a/2".to_string())
        );
        assert_eq!(
            parse_link_next("<http://a/2>; rel=next"),
            Some("http://a/2".to_string())
        );
        assert_eq!(
            parse_link_next("<http://a/2>; rel=\"next last\""),
            Some("http://a/2".to_string())
        );
        assert_eq!(
            parse_link_next("<http://a/1>; rel=\"first\", <http://a/2>; rel=\"next\""),
            Some("http://a/2".to_string())
        );
        assert_eq!(parse_link_next("<http://a/1>; rel=\"first\""), None);
        assert_eq!(parse_link_next(""), None);
    }

    #[test]
    fn same_origin_checks_scheme_host_and_port() {
        assert!(same_origin("http://a/x", "http://a/y"));
        assert!(same_origin("http://a:80/x", "http://a/y"));
        assert!(same_origin("https://A/x", "https://a/y"));
        assert!(!same_origin("http://a/x", "https://a/x"));
        assert!(!same_origin("http://a/x", "http://b/x"));
        assert!(!same_origin("http://a:8080/x", "http://a/x"));
        assert!(!same_origin("http://a/x", "ftp://a/x"));
    }

    #[test]
    fn two_pages_merge_and_the_last_body_sits_around_the_items() {
        let seen = fresh_seen();
        let mut fetch = scripted(
            vec![
                Ok(raw(
                    200,
                    json!({ "items": [1], "meta": "p1", "cursor": "c2" }),
                )),
                Ok(raw(200, json!({ "items": [2], "meta": "p2" }))),
            ],
            seen.clone(),
        );
        let spec = token_spec("items", "cursor", DEFAULT_MAX_PAGES);
        let urls = std::cell::RefCell::new(Vec::new());
        let mut next_url = |token: &str| -> Result<String, CallError> {
            urls.borrow_mut().push(token.to_string());
            Ok(format!("http://a/things?offset={token}"))
        };
        let (result, stats) = walk(
            "http://a/things",
            &spec,
            &rules(),
            &mut fetch,
            &mut next_url,
        );
        let walked = result.unwrap();
        assert!(!walked.more);
        assert_eq!(stats.pages, 2);
        assert_eq!(stats.last_status, Some(200));
        match walked.ok {
            CallOk::Http { body, picked, .. } => {
                assert_eq!(body["items"], json!([1, 2]));
                assert_eq!(body["meta"], "p2");
                assert!(!picked);
            }
            other => panic!("unexpected result: {other:?}"),
        }
        // The token re-render set the arg on the second request.
        assert_eq!(seen.borrow()[1].0, "http://a/things?offset=c2");
        assert_eq!(urls.borrow().as_slice(), ["c2".to_string()]);
    }

    #[test]
    fn items_empty_concatenates_whole_bodies() {
        let mut answers = vec![
            Ok(RawResponse {
                link: Some("<http://a/2>; rel=\"next\"".to_string()),
                ..raw(200, Value::Array(vec![json!(1)]))
            }),
            Ok(raw(200, Value::Array(vec![json!(2)]))),
        ];
        let mut fetch = move |_url: &str, _allowance: usize| answers.remove(0);
        let spec = link_spec("", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/things", &spec, &rules(), &mut fetch, &mut next);
        let walked = result.unwrap();
        match walked.ok {
            CallOk::Http { body, .. } => assert_eq!(body, json!([1, 2])),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn the_walk_stops_at_max_pages_with_more() {
        let mut answers = Vec::new();
        for n in 0..4u32 {
            let mut page = raw(200, json!({ "items": [n] }));
            if n < 3 {
                page.link = Some(format!("<http://a/{}>; rel=\"next\"", n + 1));
            }
            answers.push(Ok(page));
        }
        let mut fetch = scripted(answers, fresh_seen());
        let spec = link_spec("items", 2);
        let mut next = no_token_pages;
        let (result, stats) = walk("http://a/0", &spec, &rules(), &mut fetch, &mut next);
        let walked = result.unwrap();
        assert!(walked.more);
        assert_eq!(stats.pages, 2);
        match walked.ok {
            CallOk::Http { body, .. } => assert_eq!(body["items"], json!([0, 1])),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn the_byte_cap_stops_the_walk_with_more() {
        // One page whose read is already the whole cap: the next page would
        // get no allowance.
        let answers = vec![Ok(RawResponse {
            bytes: PAGES_BYTE_CAP,
            link: Some("<http://a/2>; rel=\"next\"".to_string()),
            ..raw(200, json!({ "items": [1] }))
        })];
        let mut fetch = scripted(answers, fresh_seen());
        let spec = link_spec("items", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let walked = result.unwrap();
        assert!(walked.more);
        assert_eq!(stats.pages, 1);
    }

    #[test]
    fn an_empty_or_null_token_stops_the_walk() {
        for token in [json!(""), json!(null)] {
            let answers = vec![Ok(raw(200, json!({ "items": [1], "cursor": token })))];
            let mut fetch = scripted(answers, fresh_seen());
            let spec = token_spec("items", "cursor", DEFAULT_MAX_PAGES);
            let mut next = no_token_pages;
            let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
            let walked = result.unwrap();
            assert!(!walked.more, "token {token} must stop the walk");
            assert_eq!(stats.pages, 1);
        }
    }

    #[test]
    fn a_page_without_an_item_array_fails() {
        // Present but not an array.
        let answers = vec![Ok(raw(200, json!({ "items": 3 })))];
        let mut fetch = scripted(answers, fresh_seen());
        let spec = link_spec("items", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let err = result.unwrap_err();
        assert_eq!(err.code, CallErrorCode::Service);
        assert!(err.message.contains("no item list"), "{}", err.message);

        // Absent without items_optional.
        let answers = vec![Ok(raw(200, json!({ "resultSizeEstimate": 0 })))];
        let mut fetch = scripted(answers, fresh_seen());
        let spec = token_spec("messages", "nextPageToken", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        assert_eq!(result.unwrap_err().code, CallErrorCode::Service);
    }

    #[test]
    fn items_optional_reads_an_absent_list_as_empty() {
        // An absent field reads as an empty page; a next page continues.
        let answers = vec![
            Ok(raw(
                200,
                json!({ "resultSizeEstimate": 0, "nextPageToken": "t2" }),
            )),
            Ok(raw(200, json!({ "messages": ["a", "b"] }))),
        ];
        let mut fetch = scripted(answers, fresh_seen());
        let mut spec = token_spec("messages", "nextPageToken", DEFAULT_MAX_PAGES);
        spec.items_optional = true;
        let mut seen_urls = Vec::new();
        let mut next = |token: &str| -> Result<String, CallError> {
            seen_urls.push(token.to_string());
            Ok(format!("http://a/2?offset={token}"))
        };
        let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let walked = result.unwrap();
        assert!(!walked.more);
        assert_eq!(stats.pages, 2);
        match walked.ok {
            CallOk::Http { body, .. } => {
                assert_eq!(body["messages"], json!(["a", "b"]));
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert_eq!(seen_urls, vec!["t2".to_string()]);

        // A present non-array still fails under items_optional.
        let answers = vec![Ok(raw(200, json!({ "messages": 0 })))];
        let mut fetch = scripted(answers, fresh_seen());
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        assert_eq!(result.unwrap_err().code, CallErrorCode::Service);
    }

    #[test]
    fn items_optional_writes_the_empty_list_when_the_last_page_omits_the_key() {
        // Gmail leaves `messages` out when nothing matched: the walk read
        // that absence as an empty page, so the result carries the empty
        // list at the items path instead of failing.
        let answers = vec![Ok(raw(200, json!({ "resultSizeEstimate": 0 })))];
        let mut fetch = scripted(answers, fresh_seen());
        let mut spec = token_spec("messages", "nextPageToken", DEFAULT_MAX_PAGES);
        spec.items_optional = true;
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let walked = result.unwrap();
        assert!(!walked.more);
        match walked.ok {
            CallOk::Http { body, .. } => {
                assert_eq!(body["messages"], json!([]));
                assert_eq!(body["resultSizeEstimate"], 0);
            }
            other => panic!("unexpected result: {other:?}"),
        }

        // Without items_optional the absence stays an error (it fails in
        // take_items, before the merged list is written back).
        let answers = vec![Ok(raw(200, json!({ "resultSizeEstimate": 0 })))];
        let mut fetch = scripted(answers, fresh_seen());
        let spec = token_spec("messages", "nextPageToken", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let err = result.unwrap_err();
        assert_eq!(err.code, CallErrorCode::Service);
        assert!(err.message.contains("no item list"), "{}", err.message);
    }

    #[test]
    fn a_page_2_error_carries_the_prefix_and_the_facts() {
        // A page 2 status error.
        let answers = vec![
            Ok(RawResponse {
                link: Some("<http://a/2>; rel=\"next\"".to_string()),
                ..raw(200, json!({ "items": [1] }))
            }),
            Ok(raw(500, json!({ "message": "backend" }))),
        ];
        let mut fetch = scripted(answers, fresh_seen());
        let spec = link_spec("items", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let err = result.unwrap_err();
        assert!(
            err.message.starts_with("page 2 of the listing:"),
            "{}",
            err.message
        );
        assert_eq!(err.code, CallErrorCode::Service);
        assert_eq!(stats.pages, 2);
        assert_eq!(stats.last_status, Some(500));

        // A page 2 transport error keeps the first page's status.
        let answers = vec![
            Ok(RawResponse {
                link: Some("<http://a/2>; rel=\"next\"".to_string()),
                ..raw(200, json!({ "items": [1] }))
            }),
            Err(CallError::new(
                CallErrorCode::Timeout,
                "stalled".to_string(),
            )),
        ];
        let mut fetch = scripted(answers, fresh_seen());
        let mut next = no_token_pages;
        let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let err = result.unwrap_err();
        assert!(
            err.message.starts_with("page 2 of the listing:"),
            "{}",
            err.message
        );
        assert_eq!(stats.pages, 2);
        assert_eq!(stats.last_status, Some(200));

        // A cut page 2 fails incomplete_response, prefixed, with no items.
        let answers = vec![
            Ok(RawResponse {
                link: Some("<http://a/2>; rel=\"next\"".to_string()),
                ..raw(200, json!({ "items": [1] }))
            }),
            Ok(RawResponse {
                incomplete: cut(10),
                ..raw(200, json!({ "items": [2] }))
            }),
        ];
        let mut fetch = scripted(answers, fresh_seen());
        let mut next = no_token_pages;
        let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let err = result.unwrap_err();
        assert_eq!(err.code, CallErrorCode::IncompleteResponse);
        assert!(
            err.message.starts_with("page 2 of the listing:"),
            "{}",
            err.message
        );
        assert_eq!(stats.pages, 2);
    }

    #[test]
    fn the_fetch_sees_the_byte_allowance_and_a_cut_page_fails_with_no_items() {
        // Page 1 reads 3 "MiB" (as recorded bytes), page 2 gets the rest.
        let allowances: Seen = fresh_seen();
        let mut answers = vec![
            Ok(RawResponse {
                bytes: 3 * 1024 * 1024,
                link: Some("<http://a/2>; rel=\"next\"".to_string()),
                ..raw(200, json!({ "items": [1] }))
            }),
            Ok(RawResponse {
                incomplete: cut(1024 * 1024),
                ..raw(200, json!({ "items": [2] }))
            }),
        ];
        let log = allowances.clone();
        let mut fetch = move |url: &str, allowance: usize| -> Result<RawResponse, CallError> {
            log.borrow_mut().push((url.to_string(), allowance));
            answers.remove(0)
        };
        let spec = link_spec("items", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, _stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        // The cut page 2 fails the call: no item of page 2 is returned.
        assert_eq!(result.unwrap_err().code, CallErrorCode::IncompleteResponse);
        let allowances = allowances.borrow();
        assert_eq!(allowances.len(), 2);
        assert_eq!(allowances[0].1, BODY_CAP);
        assert_eq!(allowances[1].1, PAGES_BYTE_CAP - 3 * 1024 * 1024);
    }

    #[test]
    fn a_link_to_another_host_is_refused_without_a_request() {
        let answers = vec![Ok(RawResponse {
            link: Some("<http://other.example/2>; rel=\"next\"".to_string()),
            ..raw(200, json!({ "items": [1] }))
        })];
        let mut fetch = scripted(answers, fresh_seen());
        let spec = link_spec("items", DEFAULT_MAX_PAGES);
        let mut next = no_token_pages;
        let (result, stats) = walk("http://a/1", &spec, &rules(), &mut fetch, &mut next);
        let err = result.unwrap_err();
        assert_eq!(err.code, CallErrorCode::Service);
        assert!(err.message.contains("another host"), "{}", err.message);
        assert_eq!(stats.pages, 1, "no second request was made");
    }
}
