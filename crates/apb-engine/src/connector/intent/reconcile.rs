//! Finding an earlier `marker` send again: read the connector's own verified
//! account, list the recent items, and look for one by that account that
//! ends with exactly the marker the earlier send carried.
//!
//! The matching ([`match_items`]) is pure. The flow ([`reconcile`]) gets its
//! two reads through a fetch function the call path supplies, so this module
//! never calls the call path itself.

use apb_core::connector::common::{scalar_at_path, value_at_path};
use apb_core::connector::idempotency::{IdentitySpec, ReconcileSpec};
use apb_core::connector::marker::{self, MarkerFields, MarkerFormat};
use serde_json::{Map, Value};

use super::ledger::IntentRecord;

/// How far before the intent the listing starts: clocks differ, and a
/// service may stamp an item a little before the request ends.
const SINCE_SLACK_MS: u64 = 300_000;

/// One page a read returned: its body, and whether the service said a
/// further page exists.
#[derive(Debug, Clone, Copy)]
pub struct Listing<'a> {
    pub body: &'a Value,
    pub next_page: bool,
}

/// What a listing says about one marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// An item by the connector's own account carries exactly the marker.
    Found { item_ref: Option<String> },
    /// The whole listing was read and no such item is in it.
    Absent { seen: usize },
    /// The listing cannot settle it either way.
    Inconclusive { why: String },
}

/// Looks for the item that carries `marker` and was written by `self_id`.
/// A further page with no match is inconclusive: the item may sit there.
pub fn match_items(
    spec: &ReconcileSpec,
    listing: Listing<'_>,
    self_id: &str,
    marker: &str,
) -> Match {
    let Some(items) = value_at_path(listing.body, &spec.items).and_then(Value::as_array) else {
        return Match::Inconclusive {
            why: "the listing response holds no item list where the connector says it is"
                .to_string(),
        };
    };
    let found = items.iter().find(|item| {
        scalar_at_path(item, &spec.author).as_deref() == Some(self_id)
            && value_at_path(item, &spec.text)
                .and_then(Value::as_str)
                .is_some_and(|text| marker::carries_exactly(text, marker))
    });
    match found {
        Some(item) => Match::Found {
            item_ref: spec
                .item_ref
                .as_deref()
                .and_then(|path| scalar_at_path(item, path)),
        },
        None if listing.next_page => Match::Inconclusive {
            why: format!(
                "no match among the {} items of the first page, and more pages exist",
                items.len()
            ),
        },
        None => Match::Absent { seen: items.len() },
    }
}

/// The connector's own account id from the identity read.
pub fn self_id(identity: &IdentitySpec, body: &Value) -> Option<String> {
    scalar_at_path(body, &identity.path)
}

/// The listing's arguments: the fixed ones, the ones copied from the send,
/// and the "since" argument set a little before the intent.
pub fn listing_args(spec: &ReconcileSpec, send_args: &Value, intent_ts_ms: u64) -> Value {
    let mut args: Map<String, Value> = spec.fixed_args.clone();
    for name in &spec.args_from_send {
        if let Some(v) = send_args.get(name) {
            args.insert(name.clone(), v.clone());
        }
    }
    if let Some(since) = &spec.since_arg {
        let at = intent_ts_ms.saturating_sub(SINCE_SLACK_MS);
        args.insert(since.clone(), Value::String(apb_core::clock::iso_utc(at)));
    }
    Value::Object(args)
}

/// The marker an earlier send of `rec` carried.
pub fn marker_of(format: MarkerFormat, run_id: &str, rec: &IntentRecord) -> String {
    marker::render(
        format,
        &MarkerFields {
            run: run_id.to_string(),
            node: rec.key.node.clone(),
            attempt: rec.attempt,
            nonce: rec.nonce.clone(),
        },
    )
}

/// A read the flow asks for: a function of the same connector and its
/// arguments. The answer is the raw response body and whether a further
/// page exists, or why the read failed.
pub type Fetch<'a> = dyn FnMut(&str, Value) -> Result<(Value, bool), String> + 'a;

/// What reconciliation settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    Confirmed { effect_ref: Option<String> },
    Unknown { detail: String },
}

/// What the flow needs about the earlier send.
pub struct ReconcileInput<'a> {
    pub identity: &'a IdentitySpec,
    pub spec: &'a ReconcileSpec,
    pub format: MarkerFormat,
    pub run_id: &'a str,
    pub record: &'a IntentRecord,
    /// The arguments of the repeat that triggered the lookup.
    pub send_args: &'a Value,
}

/// Reads the own identity, lists the recent items, and matches.
pub fn reconcile(input: &ReconcileInput<'_>, fetch: &mut Fetch<'_>) -> Reconciled {
    let unknown = |detail: String| Reconciled::Unknown { detail };
    let identity = match fetch(&input.identity.function, Value::Object(Map::new())) {
        Ok((body, _)) => body,
        Err(why) => return unknown(format!("own identity could not be read: {why}")),
    };
    let Some(me) = self_id(input.identity, &identity) else {
        return unknown("own identity could not be read: no id at the identity path".to_string());
    };
    let args = listing_args(input.spec, input.send_args, input.record.ts_ms);
    let (body, next_page) = match fetch(&input.spec.function, args) {
        Ok(page) => page,
        Err(why) => return unknown(format!("the recent items could not be listed: {why}")),
    };
    let marker = marker_of(input.format, input.run_id, input.record);
    match match_items(
        input.spec,
        Listing {
            body: &body,
            next_page,
        },
        &me,
        &marker,
    ) {
        Match::Found { item_ref } => Reconciled::Confirmed {
            effect_ref: item_ref,
        },
        Match::Absent { seen } => unknown(format!(
            "no matching item among {seen} recent items by {me}"
        )),
        Match::Inconclusive { why } => unknown(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(items: &str) -> ReconcileSpec {
        ReconcileSpec {
            function: "list".into(),
            args_from_send: vec!["owner".into()],
            since_arg: Some("since".into()),
            fixed_args: serde_json::Map::from_iter([("per_page".to_string(), json!(100))]),
            items: items.into(),
            text: "body".into(),
            author: "user.login".into(),
            item_ref: Some("html_url".into()),
        }
    }

    const M: &str = "<!-- apb-intent v1 run=r node=n attempt=1 nonce=abc -->";

    fn item(author: &str, body: &str) -> Value {
        json!({ "user": { "login": author }, "body": body, "html_url": format!("u/{author}") })
    }

    fn whole(body: &Value) -> Listing<'_> {
        Listing {
            body,
            next_page: false,
        }
    }

    #[test]
    fn an_own_item_ending_with_the_exact_marker_is_found() {
        let body = json!([item("bot", "x"), item("bot", &format!("Summary.\n\n{M}\n"))]);
        assert_eq!(
            match_items(&spec(""), whole(&body), "bot", M),
            Match::Found {
                item_ref: Some("u/bot".into())
            }
        );
        let nested = json!({ "data": { "items": [item("bot", M)] } });
        assert!(matches!(
            match_items(&spec("data.items"), whole(&nested), "bot", M),
            Match::Found { .. }
        ));
    }

    #[test]
    fn near_misses_are_not_the_earlier_send() {
        let other_nonce = M.replace("abc", "abd");
        let neutralized = apb_core::connector::marker::neutralize(M).into_owned();
        for body in [
            item("someone", M),
            item("bot", &other_nonce),
            item("bot", &format!("{M} and then more text")),
            item("bot", &format!("{M}\n\n{M}")),
            item("bot", &neutralized),
            item("bot", &M.replace("-->", "--&gt;")),
        ] {
            let list = json!([body]);
            assert_eq!(
                match_items(&spec(""), whole(&list), "bot", M),
                Match::Absent { seen: 1 }
            );
        }
    }

    #[test]
    fn a_further_page_or_a_missing_list_is_inconclusive() {
        let list = json!([item("bot", "x")]);
        let paged = Listing {
            body: &list,
            next_page: true,
        };
        assert!(matches!(
            match_items(&spec(""), paged, "bot", M),
            Match::Inconclusive { .. }
        ));
        assert!(matches!(
            match_items(&spec("items"), whole(&json!({ "error": "x" })), "bot", M),
            Match::Inconclusive { .. }
        ));
        // A match on a page with more pages is still a match.
        let found = json!([item("bot", M)]);
        let paged = Listing {
            body: &found,
            next_page: true,
        };
        assert!(matches!(
            match_items(&spec(""), paged, "bot", M),
            Match::Found { .. }
        ));
    }

    #[test]
    fn the_listing_args_combine_fixed_copied_and_since() {
        let args = listing_args(&spec(""), &json!({ "owner": "acme", "body": "x" }), 600_000);
        assert_eq!(
            args,
            json!({ "per_page": 100, "owner": "acme", "since": "1970-01-01T00:05:00Z" })
        );
    }
}
