//! The JSON-RPC frames the line tap reads on both pipes, and the shape of
//! a call's arguments (design D3 "The line tap"). Pure: no I/O, no state.
//!
//! The writer side closes one class: every line the client writes is
//! parsed and classified before any byte of it reaches the server, and a
//! line that does not parse as one JSON object with no member named twice
//! is refused, never forwarded unclassified. The reader side takes a
//! listing answer only from a frame that is a successful response by the
//! rules rmcp applies to it.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// The deepest arguments object a call takes: an object directly holding
/// only scalars is depth 1. The request line adds two levels (the message
/// and its `params`), far below serde_json's limit of 128, so a call the
/// edge admits always parses in the tap and in rmcp.
pub const MAX_ARGUMENT_DEPTH: usize = 64;

/// The longest arguments object a call takes, as compact JSON: the 1 MiB
/// body cap. The request line stays far below the tap's line limit
/// (`MAX_LINE_BYTES`).
pub const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;

/// Why a call's arguments are refused at the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentsRefusal {
    /// Nested deeper than `MAX_ARGUMENT_DEPTH`.
    TooDeep,
    /// Longer than `MAX_ARGUMENT_BYTES` as compact JSON.
    TooLarge,
}

impl ArgumentsRefusal {
    /// The named refusal; it carries no argument value.
    pub fn message(self) -> String {
        match self {
            ArgumentsRefusal::TooDeep => format!(
                "mcp_arguments_too_deep: the arguments are nested deeper than {MAX_ARGUMENT_DEPTH} levels"
            ),
            ArgumentsRefusal::TooLarge => format!(
                "mcp_arguments_too_large: the arguments are longer than {MAX_ARGUMENT_BYTES} bytes as JSON"
            ),
        }
    }
}

/// Checks a call's arguments against `MAX_ARGUMENT_DEPTH` and
/// `MAX_ARGUMENT_BYTES`, the one check the engine's edge and the session
/// share. The depth walk keeps its own stack, so no input can exhaust the
/// thread's.
pub fn check_arguments(arguments: &Map<String, Value>) -> Result<(), ArgumentsRefusal> {
    let mut stack: Vec<(&Value, usize)> = arguments.values().map(|v| (v, 2)).collect();
    while let Some((value, depth)) = stack.pop() {
        let is_container = matches!(value, Value::Array(_) | Value::Object(_));
        if is_container && depth > MAX_ARGUMENT_DEPTH {
            return Err(ArgumentsRefusal::TooDeep);
        }
        match value {
            Value::Array(items) => stack.extend(items.iter().map(|c| (c, depth + 1))),
            Value::Object(map) => stack.extend(map.values().map(|c| (c, depth + 1))),
            _ => {}
        }
    }
    let size = serde_json::to_vec(arguments).map_or(usize::MAX, |b| b.len());
    if size > MAX_ARGUMENT_BYTES {
        return Err(ArgumentsRefusal::TooLarge);
    }
    Ok(())
}

/// A JSON value with no member named twice in any object at any depth,
/// read by the same serde_json grammar rmcp reads with.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Strict;
        impl<'de> Visitor<'de> for Strict {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value with each member named once")
            }
            fn visit_bool<E>(self, v: bool) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::from(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::from(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::from(v)))
            }
            fn visit_str<E>(self, v: &str) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::String(v.to_string())))
            }
            fn visit_string<E>(self, v: String) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::String(v)))
            }
            fn visit_unit<E>(self) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E>(self) -> Result<StrictValue, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<StrictValue, A::Error> {
                let mut out = Vec::new();
                while let Some(StrictValue(v)) = seq.next_element::<StrictValue>()? {
                    out.push(v);
                }
                Ok(StrictValue(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StrictValue, A::Error> {
                let mut out = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    let StrictValue(value) = map.next_value()?;
                    if out.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("a member is named twice"));
                    }
                }
                Ok(StrictValue(Value::Object(out)))
            }
        }
        deserializer.deserialize_any(Strict)
    }
}

/// One JSON object with no member named twice at any depth, else `None`.
fn strict_object(line: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<StrictValue>(line).ok()?.0 {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// The key a request id is compared by: its JSON text, so `1` and `"1"`
/// stay apart as they do in rmcp.
pub(crate) fn id_key(id: &Value) -> String {
    id.to_string()
}

/// What one line the client is about to write is.
#[derive(Debug, PartialEq)]
pub(crate) enum Outgoing {
    /// A `tools/call` request (or notification) with its id key, when it
    /// has one: counted when handed over.
    Call(Option<String>),
    /// A `tools/list` request with its id key.
    Listing(String),
    /// A classified line that is neither (another request, a notification,
    /// a response to the server).
    Other,
}

/// The refusal of a line the tap cannot classify; nothing of it is sent.
pub(crate) const UNCLASSIFIED: &str = "mcp_request_unclassified: apb could not read the message it was about to send to the MCP server as one JSON-RPC object, so nothing of it was sent";

/// Classifies one outgoing line (without its newline). `Err` for a line
/// that is not one JSON object with each member named once, or whose
/// `method` is not a string: a parse failure is never read as "not a
/// call".
pub(crate) fn classify(line: &[u8]) -> Result<Outgoing, &'static str> {
    let msg = strict_object(line).ok_or(UNCLASSIFIED)?;
    let method = match msg.get("method") {
        None => return Ok(Outgoing::Other),
        Some(Value::String(m)) => m.as_str(),
        Some(_) => return Err(UNCLASSIFIED),
    };
    match (method, msg.get("id")) {
        ("tools/call", id) => Ok(Outgoing::Call(id.map(id_key))),
        ("tools/list", Some(id)) => Ok(Outgoing::Listing(id_key(id))),
        _ => Ok(Outgoing::Other),
    }
}

/// What one line from the server is, for the listing capture. Every
/// variant is a frame the tap read completely; a line it could not read is
/// never one of them (`incoming` refuses it).
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming {
    /// A successful response with exactly the members a response has:
    /// `jsonrpc` `"2.0"`, an id (an integer or a string) and a `result`
    /// object.
    Response { id: String, result: Value },
    /// Any other response or error frame carrying an id: an error, a
    /// response with other members or a result that is not an object.
    Other { id: String },
    /// A request or notification from the server, or an error without an
    /// id: a whole frame, nothing for the capture.
    Unrelated,
}

/// The refusals of an incoming line; none echoes the server's bytes.
pub(crate) const INCOMING_BOM: &str = "it starts with a byte order mark";
pub(crate) const INCOMING_NOT_UTF8: &str = "it is not UTF-8";
pub(crate) const INCOMING_AROUND: &str = "it holds bytes before or after the JSON object";
pub(crate) const INCOMING_NOT_JSON: &str = "it is not one JSON object with each member named once";
pub(crate) const INCOMING_NOT_FRAME: &str =
    "it is not a JSON-RPC 2.0 request, notification, response or error";

/// The UTF-8 byte order mark rmcp strips before it parses; the tap refuses
/// it instead.
pub(crate) const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Reads one line from the server (without its newline) by the strict
/// rule every incoming byte passes before rmcp sees it (design D4 "The
/// client", item 3.3): one optional carriage return before the newline,
/// which rmcp also strips, and nothing else around the object; no byte
/// order mark; valid UTF-8; one JSON object with no member named twice at
/// any depth; `jsonrpc` `"2.0"`; and one frame shape by the pinned rmcp's
/// rule (`src/model.rs` lines 853 to 910, the same in 3.9.0 and 3.11.0)
/// with a well-typed id. `Err` names why; the caller then forwards nothing
/// of the line and ends the session.
pub(crate) fn incoming(line: &[u8]) -> Result<Incoming, &'static str> {
    let body = line.strip_suffix(b"\r").unwrap_or(line);
    if body.starts_with(UTF8_BOM) {
        return Err(INCOMING_BOM);
    }
    if std::str::from_utf8(body).is_err() {
        return Err(INCOMING_NOT_UTF8);
    }
    if body.first() != Some(&b'{') || body.last() != Some(&b'}') {
        return Err(INCOMING_AROUND);
    }
    let mut msg = strict_object(body).ok_or(INCOMING_NOT_JSON)?;
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(INCOMING_NOT_FRAME);
    }
    let typed_id = |id: &Value| id.is_i64() || id.is_string();
    let (method, result, error) = (
        msg.get("method"),
        msg.contains_key("result"),
        msg.contains_key("error"),
    );
    let id = msg.get("id");
    match (method, result, error) {
        // A request or a notification.
        (Some(Value::String(_)), false, false) => match id {
            None => Ok(Incoming::Unrelated),
            Some(id) if typed_id(id) => Ok(Incoming::Unrelated),
            Some(_) => Err(INCOMING_NOT_FRAME),
        },
        // A response.
        (None, true, false) => {
            let id = id.filter(|id| typed_id(id)).ok_or(INCOMING_NOT_FRAME)?;
            let id = id_key(id);
            let whole = msg.len() == 3 && msg.get("result").is_some_and(Value::is_object);
            if !whole {
                return Ok(Incoming::Other { id });
            }
            let result = msg.remove("result").unwrap_or(Value::Null);
            Ok(Incoming::Response { id, result })
        }
        // An error, its id optional.
        (None, false, true) => match id {
            None | Some(Value::Null) => Ok(Incoming::Unrelated),
            Some(id) if typed_id(id) => Ok(Incoming::Other { id: id_key(id) }),
            Some(_) => Err(INCOMING_NOT_FRAME),
        },
        _ => Err(INCOMING_NOT_FRAME),
    }
}

/// Whether one line from the server is a response whose `result` is no
/// call result by rmcp's own rule (`CallToolResult`: at least one of
/// `content`, `structuredContent` and `isError`, and a `resultType` of
/// `complete` when there is one), whatever other members the frame holds.
/// rmcp 3.11 reads such an answer to a `tools/call` as an empty successful
/// call; apb keeps it an unexpected answer, as apb 1.0 read it with rmcp
/// 3.9. `false` for a line that is no response.
pub(crate) fn is_untyped_call_answer(line: &[u8]) -> bool {
    let body = line.strip_suffix(b"\r").unwrap_or(line);
    let Some(mut msg) = strict_object(body) else {
        return false;
    };
    if msg.contains_key("method") || msg.contains_key("error") {
        return false;
    }
    msg.remove("result").is_some_and(|result| {
        serde_json::from_value::<rmcp::model::CallToolResult>(result).is_err()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nested(levels: usize) -> Value {
        let mut v = json!(0);
        for _ in 0..levels {
            v = json!([v]);
        }
        v
    }

    fn args(value: Value) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("x".into(), value);
        m
    }

    #[test]
    fn the_edge_takes_the_deepest_and_longest_legal_arguments() {
        let deepest = args(nested(MAX_ARGUMENT_DEPTH - 1));
        assert_eq!(check_arguments(&deepest), Ok(()));
        assert_eq!(
            check_arguments(&args(nested(MAX_ARGUMENT_DEPTH))),
            Err(ArgumentsRefusal::TooDeep)
        );
        let room = MAX_ARGUMENT_BYTES - r#"{"x":""}"#.len();
        assert_eq!(check_arguments(&args(json!("a".repeat(room)))), Ok(()));
        assert_eq!(
            check_arguments(&args(json!("a".repeat(room + 1)))),
            Err(ArgumentsRefusal::TooLarge)
        );
        // Both legal extremes at once still classify as a call in the tap.
        let mut both = deepest.clone();
        both.insert("y".into(), json!("a".repeat(room - 200)));
        let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "t", "arguments": both }
        }))
        .unwrap();
        assert!(line.len() < crate::tap::MAX_LINE_BYTES);
        assert_eq!(classify(&line), Ok(Outgoing::Call(Some("1".into()))));
    }

    /// The review's arguments: 126 arrays around 0 pass the parser alone
    /// but not inside the request line; the edge refuses them, and the tap
    /// refuses the line instead of reading it as "not a call".
    #[test]
    fn arguments_past_the_parser_limit_are_refused_on_both_sides() {
        let deep = args(nested(126));
        assert_eq!(check_arguments(&deep), Err(ArgumentsRefusal::TooDeep));
        let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "t", "arguments": deep }
        }))
        .unwrap();
        assert_eq!(classify(&line), Err(UNCLASSIFIED));
    }

    #[test]
    fn a_line_that_does_not_classify_is_never_other() {
        for line in [
            &b"not json"[..],
            b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\"}]",
            b"{\"method\":\"ping\",\"method\":\"tools/call\",\"id\":1}",
            b"{\"method\":7,\"id\":1}",
            b"\"tools/call\"",
        ] {
            assert_eq!(
                classify(line),
                Err(UNCLASSIFIED),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#),
            Ok(Outgoing::Other)
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":2,"result":{}}"#),
            Ok(Outgoing::Other)
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#),
            Ok(Outgoing::Listing("3".into()))
        );
    }

    fn lossy(line: &[u8]) -> String {
        String::from_utf8_lossy(line).into_owned()
    }

    #[test]
    fn only_a_whole_response_envelope_is_a_response() {
        assert_eq!(
            incoming(br#"{"jsonrpc":"2.0","id":4,"result":{"tools":[]}}"#),
            Ok(Incoming::Response {
                id: "4".into(),
                result: json!({ "tools": [] })
            })
        );
        assert_eq!(
            incoming(b"{\"jsonrpc\":\"2.0\",\"id\":\"4\",\"result\":{}}\r"),
            Ok(Incoming::Response {
                id: "\"4\"".into(),
                result: json!({})
            }),
            "one carriage return before the newline is taken, as rmcp takes it"
        );
        for line in [
            &br#"{"jsonrpc":"2.0","id":4,"result":{"tools":[]},"extra":1}"#[..],
            br#"{"jsonrpc":"2.0","id":4,"result":[]}"#,
            br#"{"jsonrpc":"2.0","id":4,"error":{"code":1,"message":"x"}}"#,
        ] {
            assert!(
                matches!(incoming(line), Ok(Incoming::Other { .. })),
                "{}",
                lossy(line)
            );
        }
        for line in [
            &br#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#[..],
            br#"{"jsonrpc":"2.0","method":"notifications/x"}"#,
            br#"{"jsonrpc":"2.0","error":{"code":1,"message":"x"}}"#,
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":1,"message":"x"}}"#,
        ] {
            assert_eq!(incoming(line), Ok(Incoming::Unrelated), "{}", lossy(line));
        }
    }

    /// The strict rule (fix round 4): every line that is not exactly one
    /// readable JSON-RPC 2.0 frame is refused, never read as "nothing for
    /// the capture" and passed on.
    #[test]
    fn a_line_the_tap_cannot_read_completely_is_refused() {
        let cases: [(&[u8], &str); 16] = [
            (
                b"\xEF\xBB\xBF{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{}}",
                INCOMING_BOM,
            ),
            (
                b"{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"t\":\"\xff\"}}",
                INCOMING_NOT_UTF8,
            ),
            (br#" {"jsonrpc":"2.0","id":4,"result":{}}"#, INCOMING_AROUND),
            (br#"{"jsonrpc":"2.0","id":4,"result":{}} "#, INCOMING_AROUND),
            (
                br#"{"jsonrpc":"2.0","id":4,"result":{}}{}"#,
                INCOMING_NOT_JSON,
            ),
            (
                br#"{"jsonrpc":"2.0","id":4,"result":{}}x}"#,
                INCOMING_NOT_JSON,
            ),
            (
                b"{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{}}\r\r",
                INCOMING_AROUND,
            ),
            (b"", INCOMING_AROUND),
            (b"nope", INCOMING_AROUND),
            (
                br#"{"jsonrpc":"2.0","id":4,"result":{},"x":1,"x":2}"#,
                INCOMING_NOT_JSON,
            ),
            (
                br#"{"jsonrpc":"2.0","id":4,"result":{"tools":[{"name":"t","name":"u"}]}}"#,
                INCOMING_NOT_JSON,
            ),
            (
                br#"{"jsonrpc":"2.0","id":4,"method":"ping","result":{"tools":[]}}"#,
                INCOMING_NOT_FRAME,
            ),
            (
                br#"{"jsonrpc":"1.0","id":4,"result":{}}"#,
                INCOMING_NOT_FRAME,
            ),
            (br#"{"id":4,"result":{}}"#, INCOMING_NOT_FRAME),
            (
                br#"{"jsonrpc":"2.0","id":4.5,"result":{}}"#,
                INCOMING_NOT_FRAME,
            ),
            (
                br#"{"jsonrpc":"2.0","id":4,"result":{},"error":{}}"#,
                INCOMING_NOT_FRAME,
            ),
        ];
        for (line, why) in cases {
            assert_eq!(incoming(line), Err(why), "{}", lossy(line));
        }
    }
}
