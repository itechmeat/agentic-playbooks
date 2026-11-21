//! The Streamable HTTP client: apb's own `StreamableHttpClient` for rmcp's
//! worker, over ureq.
//!
//! rmcp's worker hands typed messages to this client and takes typed
//! answers back, so the client calls the same `LineTap` functions on the
//! bytes it sends and receives as the stdio tap does on the pipes: every
//! outgoing message is classified before its POST, every answer (a JSON
//! body, or each event of an event stream) is read whole by the strict
//! reader before anything of that answer reaches rmcp, and a `tools/call`
//! is counted as handed over before its connection is opened. No fault
//! message carries a byte the server chose (a body, a header value, a
//! content type, a session id).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::stream::BoxStream;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_client::{
    SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use serde_json::Value;
use ureq::http::{HeaderName, HeaderValue};

use crate::stdio::EnvValue;
use crate::tap::{LineTap, MAX_LINE_BYTES};

/// An HTTP server to reach. No process is started.
pub struct HttpServer {
    /// The rendered endpoint (https; the engine checked it with
    /// `mcp::url_scheme_ok`).
    pub url: String,
    /// Extra request headers from the definition and the account; a
    /// secret value is exposed only into the request.
    pub headers: Vec<(String, EnvValue)>,
    /// An access token sent as `Authorization: Bearer <token>`.
    pub bearer: Option<apb_core::secret::Secret>,
}

/// Why the HTTP client answered a message with an error. No variant holds
/// a byte the server chose.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpFault {
    /// Nothing was posted: the gate was sealed, or a message was refused
    /// before its request was built.
    #[error("the MCP session sent nothing more to the server")]
    NotSent,
    /// The transport broke, or the answer was one this client does not
    /// read; apb's own words.
    #[error("{0}")]
    Transport(&'static str),
    /// HTTP 401 or 403.
    #[error("the MCP server refused the authorization (HTTP {status})")]
    Auth { status: u16 },
    /// A status outside 2xx without a JSON-RPC answer for the request.
    #[error("the MCP server answered with HTTP {0}")]
    Status(u16),
    /// The answer passed a limit (`mcp_message_too_large:`).
    #[error("{0}")]
    TooLarge(String),
    /// A frame the strict reader refused, or a request this client does
    /// not send (`mcp_message_invalid:`, `mcp_header_unsupported:` and the
    /// like).
    #[error("{0}")]
    Invalid(String),
    /// The deadline passed during the request.
    #[error("the MCP request did not end before the deadline")]
    Timeout,
}

/// The three request headers rmcp names from the classified message; the
/// only ones of its `custom_headers` this client sends.
const RMCP_HEADERS: [&str; 3] = ["mcp-protocol-version", "mcp-method", "mcp-name"];

const HEADER_UNSUPPORTED: &str = "mcp_header_unsupported: the protocol library asked for a request header this client does not send";
const SERVER_HEADER_RESERVED: &str = "mcp_header_unsupported: a request header of this connector is one the HTTP transport sets itself";
const ANNOTATION_UNSUPPORTED: &str = "mcp_header_annotation_unsupported: a tool of this server declares x-mcp-header annotations, which this apb does not support over HTTP";
const SESSION_REFUSED: &str =
    "the server started a session; this apb speaks the stateless revision only";
const ACCEPTED_REQUEST: &str = "the MCP server accepted a request without answering it";
const CONTENT_TYPE_UNREAD: &str =
    "the MCP server answered with a content type this client does not read";
const NOT_THE_ANSWER: &str = "mcp_message_invalid: the MCP server answered a request with a body that is not its answer (a server request or another request's response); nothing of it was passed on and the session ends";
const STREAM_ENDED: &str = "the event stream ended without the answer";
const NO_GET_STREAM: &str = "no event stream in the stateless revision";
const UNREACHABLE: &str = "the MCP server could not be reached, or the connection broke";
const REQUEST_UNBUILT: &str = "the HTTP request to the MCP server could not be built";
const DECODE_FAILED: &str =
    "mcp_message_invalid: the MCP server's answer is not a message the protocol library reads";

/// The bound of one HTTP answer, a JSON body or a whole event stream up to
/// its answer: the line cap of the stdio reader.
fn too_large_answer() -> HttpFault {
    HttpFault::TooLarge(format!(
        "mcp_message_too_large: an HTTP answer passed {MAX_LINE_BYTES} bytes"
    ))
}

/// apb's `StreamableHttpClient`: one POST per message, over ureq, through
/// the session's tap.
#[derive(Clone)]
pub(crate) struct UreqClient {
    headers: Arc<Vec<(String, EnvValue)>>,
    bearer: Option<apb_core::secret::Secret>,
    /// The deadline of the operation in progress; the session moves it
    /// before each connect, listing and call.
    deadline: Arc<Mutex<Instant>>,
    tap: LineTap,
    /// The fault of the latest message the client refused or could not
    /// answer, for the session's mapping.
    last_fault: Arc<Mutex<Option<HttpFault>>>,
}

impl UreqClient {
    pub(crate) fn new(
        server: &mut HttpServer,
        deadline: Arc<Mutex<Instant>>,
        tap: LineTap,
        last_fault: Arc<Mutex<Option<HttpFault>>>,
    ) -> UreqClient {
        UreqClient {
            headers: Arc::new(std::mem::take(&mut server.headers)),
            bearer: server.bearer.take(),
            deadline,
            tap,
            last_fault,
        }
    }

    fn fault(&self, fault: HttpFault) -> StreamableHttpError<HttpFault> {
        if let Ok(mut last) = self.last_fault.lock() {
            *last = Some(fault.clone());
        }
        StreamableHttpError::Client(fault)
    }

    fn deadline(&self) -> Instant {
        self.deadline.lock().map_or_else(|_| Instant::now(), |d| *d)
    }

    /// Everything of one POST up to the answer rmcp receives.
    async fn post(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<HttpFault>> {
        // The fault of an earlier message never names this one's failure.
        if let Ok(mut last) = self.last_fault.lock() {
            *last = None;
        }
        // 0. Nothing is posted after the seal, a notification included.
        if self.tap.is_sealed() {
            return Err(self.fault(HttpFault::NotSent));
        }
        // 1. The stateless revision has no session.
        if session_id.is_some() {
            return Err(self.fault(HttpFault::Transport(SESSION_REFUSED)));
        }
        // 2. Serialized and classified before anything else.
        let Ok(body) = serde_json::to_vec(&message) else {
            self.tap.seal();
            return Err(self.fault(HttpFault::Invalid(crate::frame::UNCLASSIFIED.to_string())));
        };
        if body.len() > MAX_LINE_BYTES {
            self.tap.seal();
            return Err(self.fault(HttpFault::Invalid(
                crate::tap::REQUEST_TOO_LARGE.to_string(),
            )));
        }
        let is_call = match self.tap.classify_message(&body) {
            Ok(is_call) => is_call,
            Err(e) => return Err(self.fault(HttpFault::Invalid(e.to_string()))),
        };
        let shape = Shape::of(&body);
        // 3. The header names, before the hand-over: only the three rmcp
        // names, and no server header the transport sets itself.
        let mut sent: Vec<(String, String)> = Vec::new();
        for (name, value) in &custom_headers {
            let lower = name.as_str().to_ascii_lowercase();
            if !RMCP_HEADERS.contains(&lower.as_str()) {
                self.tap.seal();
                return Err(self.fault(HttpFault::Invalid(HEADER_UNSUPPORTED.to_string())));
            }
            let Ok(value) = value.to_str() else {
                self.tap.seal();
                return Err(self.fault(HttpFault::Invalid(HEADER_UNSUPPORTED.to_string())));
            };
            sent.push((lower, value.to_string()));
        }
        let oauth = self.bearer.is_some();
        if self
            .headers
            .iter()
            .any(|(name, _)| apb_core::connector::mcp::reserved_http_header(name, oauth))
        {
            self.tap.seal();
            return Err(self.fault(HttpFault::Invalid(SERVER_HEADER_RESERVED.to_string())));
        }
        // 4. A passed deadline and a request that does not build fail here,
        // in memory, before the hand-over: no byte of them can leave, so a
        // `tools/call` among them settles not sent.
        let deadline = self.deadline();
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Err(self.fault(HttpFault::Timeout));
        }
        let Ok(request) = self.request(&uri, body, &sent) else {
            return Err(self.fault(HttpFault::Transport(REQUEST_UNBUILT)));
        };
        // 5. A `tools/call` is handed over from here, before the
        // connection opens; a sealed gate starts nothing.
        if is_call && !self.tap.start_call() {
            return Err(self.fault(HttpFault::NotSent));
        }
        let client = self.clone();
        let answered =
            tokio::task::spawn_blocking(move || client.exchange(request, &shape, deadline)).await;
        match answered {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(fault)) => Err(self.fault(fault)),
            Err(_) => Err(self.fault(HttpFault::Transport(UNREACHABLE))),
        }
    }

    /// The request of one message, built in memory: nothing is sent.
    fn request(
        &self,
        uri: &str,
        body: Vec<u8>,
        rmcp_headers: &[(String, String)],
    ) -> Result<ureq::http::Request<Vec<u8>>, ureq::http::Error> {
        let mut builder = ureq::http::Request::builder()
            .method(ureq::http::Method::POST)
            .uri(uri)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in rmcp_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        for (name, value) in self.headers.iter() {
            let value = match value {
                EnvValue::Plain(v) => v.as_str(),
                EnvValue::Secret(s) => s.expose_secret(),
            };
            builder = builder.header(name.as_str(), value);
        }
        if let Some(bearer) = &self.bearer {
            builder = builder.header(
                "authorization",
                format!("Bearer {}", bearer.expose_secret()),
            );
        }
        builder.body(body)
    }

    /// The blocking request and the read of its answer.
    fn exchange(
        &self,
        request: ureq::http::Request<Vec<u8>>,
        shape: &Shape,
        deadline: Instant,
    ) -> Result<StreamableHttpPostResponse, HttpFault> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HttpFault::Timeout);
        }
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(remaining))
            .http_status_as_error(false)
            .proxy(None)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let response = agent.run(request).map_err(|e| match e {
            ureq::Error::Timeout(_) => HttpFault::Timeout,
            _ => HttpFault::Transport(UNREACHABLE),
        })?;
        let status = response.status().as_u16();
        if response.headers().contains_key("mcp-session-id") {
            return Err(HttpFault::Transport(SESSION_REFUSED));
        }
        let media = media_type(response.headers().get("content-type"));
        let mut reader = response.into_body().into_reader();
        if status == 401 || status == 403 {
            return Err(HttpFault::Auth { status });
        }
        if (200..300).contains(&status) && shape.request_key.is_none() {
            // A notification: nothing of an answer is read or handed on.
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == 202 || status == 204 {
            return Err(HttpFault::Transport(ACCEPTED_REQUEST));
        }
        let Some(key) = shape.request_key.as_deref() else {
            return Err(HttpFault::Status(status));
        };
        if !(200..300).contains(&status) {
            if media.as_deref() != Some("application/json") {
                return Err(HttpFault::Status(status));
            }
            let bytes = self.read_json_body(&mut reader)?;
            // Only an answer for this request is read as one; anything
            // else is the status alone.
            if !answers(&bytes, key) {
                return Err(HttpFault::Status(status));
            }
            return self.answer(&bytes, shape);
        }
        match media.as_deref() {
            Some("application/json") => {
                let bytes = self.read_json_body(&mut reader)?;
                read_frame(&self.tap, &bytes)?;
                // The frame the strict reader accepted must answer this
                // request, as an event stream's answering frame and a
                // non-2xx body must: a server request or another id's
                // response is never handed to rmcp, so a server request
                // over http is never answered.
                if !answers(&bytes, key) {
                    return Err(HttpFault::Invalid(NOT_THE_ANSWER.to_string()));
                }
                self.decode(&bytes, shape)
            }
            Some("text/event-stream") => {
                let bytes = read_event_stream(&self.tap, reader, key)?;
                self.decode(&bytes, shape)
            }
            _ => Err(HttpFault::Transport(CONTENT_TYPE_UNREAD)),
        }
    }

    /// A JSON body, read whole up to the cap and counted.
    fn read_json_body(&self, reader: &mut impl Read) -> Result<Vec<u8>, HttpFault> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(body_fault)?;
        self.tap
            .read_body(bytes.len())
            .map_err(HttpFault::TooLarge)?;
        if bytes.len() > MAX_LINE_BYTES {
            return Err(too_large_answer());
        }
        Ok(bytes)
    }

    /// One JSON body as the answer: read by the strict reader, then
    /// decoded.
    fn answer(&self, bytes: &[u8], shape: &Shape) -> Result<StreamableHttpPostResponse, HttpFault> {
        read_frame(&self.tap, bytes)?;
        self.decode(bytes, shape)
    }

    /// The decode of a frame the strict reader accepted; a listing whose
    /// tools carry header annotations is refused first.
    fn decode(&self, bytes: &[u8], shape: &Shape) -> Result<StreamableHttpPostResponse, HttpFault> {
        if shape.listing && has_header_annotation(bytes) {
            self.tap.seal();
            return Err(HttpFault::Invalid(ANNOTATION_UNSUPPORTED.to_string()));
        }
        let message: ServerJsonRpcMessage = serde_json::from_slice(bytes)
            .map_err(|_| HttpFault::Invalid(DECODE_FAILED.to_string()))?;
        Ok(StreamableHttpPostResponse::Json(message, None))
    }
}

/// What the client needs of the message it posts: the id key of a request
/// (none for a notification or a response) and whether it is a
/// `tools/list`.
struct Shape {
    request_key: Option<String>,
    listing: bool,
}

impl Shape {
    fn of(body: &[u8]) -> Shape {
        let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let method = value.get("method").and_then(Value::as_str);
        let request_key = match (method, value.get("id")) {
            (Some(_), Some(id)) => Some(crate::frame::id_key(id)),
            _ => None,
        };
        Shape {
            request_key,
            listing: method == Some("tools/list"),
        }
    }
}

/// The media type of a `Content-Type` value: before the first `;`,
/// trimmed, lower-cased; its parameters ignored.
fn media_type(value: Option<&HeaderValue>) -> Option<String> {
    let text = value?.to_str().ok()?;
    let media = text.split(';').next().unwrap_or("").trim();
    Some(media.to_ascii_lowercase())
}

/// The fault of a failed body read, the one mapping the JSON and the
/// event-stream readers share: a timeout however it arrives (an io
/// `TimedOut`, or ureq's own timeout, which its body reader hands on
/// wrapped in an io error of another kind) is `Timeout`, anything else
/// the transport's.
fn body_fault(e: std::io::Error) -> HttpFault {
    fn timed_out(e: &std::io::Error) -> bool {
        if e.kind() == std::io::ErrorKind::TimedOut {
            return true;
        }
        match e
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
        {
            Some(ureq::Error::Timeout(_)) => true,
            Some(ureq::Error::Io(io)) => timed_out(io),
            _ => false,
        }
    }
    if timed_out(&e) {
        HttpFault::Timeout
    } else {
        HttpFault::Transport(UNREACHABLE)
    }
}

/// `tap.read_frame`, its refusal kept apart: a limit is `TooLarge`, a
/// frame the reader refused `Invalid`.
fn read_frame(tap: &LineTap, bytes: &[u8]) -> Result<(), HttpFault> {
    tap.read_frame(bytes).map_err(|message| {
        if tap.too_large().is_some() {
            HttpFault::TooLarge(message)
        } else {
            HttpFault::Invalid(message)
        }
    })
}

/// Whether a frame the strict reader accepts is the response or the error
/// for the request whose id key is `key`. Reads nothing into the tap.
fn answers(bytes: &[u8], key: &str) -> bool {
    match crate::frame::incoming(bytes) {
        Ok(crate::frame::Incoming::Response { id, .. } | crate::frame::Incoming::Other { id }) => {
            id == key
        }
        _ => false,
    }
}

/// Whether any tool of a `tools/list` answer holds a member named
/// `x-mcp-header` at any depth of its definition.
fn has_header_annotation(bytes: &[u8]) -> bool {
    fn walk(v: &Value) -> bool {
        match v {
            Value::Object(map) => map
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("x-mcp-header") || walk(v)),
            Value::Array(items) => items.iter().any(walk),
            _ => false,
        }
    }
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    value
        .get("result")
        .and_then(|r| r.get("tools"))
        .is_some_and(walk)
}

/// Reads an event stream event by event up to the event that answers the
/// request `key`, through its terminating blank line, then stops: nothing
/// after it is read, and the caller closes the connection by dropping the
/// reader. Every event is framed and read by the strict reader before the
/// next is read; nothing but the answer's frame is returned, and a refusal
/// anywhere before it hands nothing on.
fn read_event_stream(tap: &LineTap, reader: impl Read, key: &str) -> Result<Vec<u8>, HttpFault> {
    let invalid = |why: &str| {
        HttpFault::Invalid(format!(
            "mcp_message_invalid: the MCP server sent an event stream apb could not read ({why}); nothing of it was passed on and the session ends"
        ))
    };
    let mut reader = BufReader::new(reader.take(MAX_LINE_BYTES as u64 + 1));
    let mut total = 0usize;
    let mut line = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    let mut has_data = false;
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).map_err(body_fault)?;
        total += n;
        tap.read_body(n).map_err(HttpFault::TooLarge)?;
        if total > MAX_LINE_BYTES {
            return Err(too_large_answer());
        }
        if n == 0 || line.last() != Some(&b'\n') {
            return Err(HttpFault::Transport(STREAM_ENDED));
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.contains(&b'\r') {
            return Err(invalid("a line ends with a lone carriage return"));
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            return Err(invalid("it is not UTF-8"));
        };
        if text.is_empty() {
            // The end of an event. One without data (the priming event of
            // a resumable stream) dispatches nothing.
            if has_data && !data.is_empty() {
                read_frame(tap, &data)?;
                if answers(&data, key) {
                    return Ok(data);
                }
            }
            data.clear();
            has_data = false;
            continue;
        }
        if text.starts_with(':') {
            continue;
        }
        let (name, value) = match text.split_once(':') {
            Some((name, value)) => (name, value.strip_prefix(' ').unwrap_or(value)),
            None => (text, ""),
        };
        match name {
            "data" => {
                if has_data {
                    data.push(b'\n');
                }
                data.extend_from_slice(value.as_bytes());
                has_data = true;
            }
            "event" => {
                if !(value.is_empty() || value == "message") {
                    return Err(invalid("an event type this client does not read"));
                }
            }
            "id" | "retry" => {}
            _ => return Err(invalid("an event field this client does not read")),
        }
    }
}

impl StreamableHttpClient for UreqClient {
    type Error = HttpFault;

    fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<StreamableHttpPostResponse, StreamableHttpError<HttpFault>>>
    + Send
    + '_ {
        self.post(uri, message, session_id, custom_headers)
    }

    fn delete_session(
        &self,
        _uri: Arc<str>,
        _session_id: Arc<str>,
        _auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<(), StreamableHttpError<HttpFault>>> + Send + '_ {
        // No session exists in the stateless revision.
        std::future::ready(Ok(()))
    }

    fn get_stream(
        &self,
        _uri: Arc<str>,
        _session_id: Option<Arc<str>>,
        _last_event_id: Option<String>,
        _auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<
        Output = Result<
            BoxStream<'static, Result<sse_stream::Sse, SseError>>,
            StreamableHttpError<HttpFault>,
        >,
    > + Send
    + '_ {
        std::future::ready(Err(self.fault(HttpFault::Transport(NO_GET_STREAM))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rmcp hands `custom_headers` only through its worker, so the client
    /// is fed a map directly: an `Mcp-Param-*` header and any other name
    /// outside the three rmcp names are refused before anything is sent,
    /// and the refusal seals the gate.
    #[test]
    fn an_unknown_rmcp_header_is_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let uri: Arc<str> = format!("http://{}/mcp", listener.local_addr().unwrap()).into();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for name in ["mcp-param-region", "x-other"] {
            let tap = LineTap::new();
            let fault = Arc::new(Mutex::new(None));
            let mut server = HttpServer {
                url: uri.to_string(),
                headers: Vec::new(),
                bearer: None,
            };
            let client = UreqClient::new(
                &mut server,
                Arc::new(Mutex::new(
                    Instant::now() + std::time::Duration::from_secs(5),
                )),
                tap.clone(),
                fault.clone(),
            );
            let message: ClientJsonRpcMessage = serde_json::from_value(serde_json::json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "t", "arguments": { "region": "eu" } }
            }))
            .unwrap();
            let mut headers = HashMap::new();
            headers.insert(
                HeaderName::from_static("mcp-method"),
                HeaderValue::from_static("tools/call"),
            );
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("eu"),
            );
            let answered =
                runtime.block_on(client.post_message(uri.clone(), message, None, None, headers));
            assert!(answered.is_err(), "{name}");
            assert!(
                matches!(&*fault.lock().unwrap(), Some(HttpFault::Invalid(m)) if m.starts_with("mcp_header_unsupported:") && !m.contains(name)),
                "{name}"
            );
            assert_eq!(tap.seal(), 0, "{name}: no call was handed over");
            assert!(listener.accept().is_err(), "{name}: nothing was sent");
        }
    }

    fn client_for(
        uri: &str,
        headers: Vec<(String, EnvValue)>,
        deadline: Instant,
    ) -> (UreqClient, LineTap, Arc<Mutex<Option<HttpFault>>>) {
        let tap = LineTap::new();
        let fault = Arc::new(Mutex::new(None));
        let mut server = HttpServer {
            url: uri.to_string(),
            headers,
            bearer: None,
        };
        let client = UreqClient::new(
            &mut server,
            Arc::new(Mutex::new(deadline)),
            tap.clone(),
            fault.clone(),
        );
        (client, tap, fault)
    }

    fn tools_call() -> ClientJsonRpcMessage {
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "t", "arguments": {} }
        }))
        .unwrap()
    }

    /// A `tools/call` whose deadline passed before its POST, or whose
    /// request does not build, fails before the hand-over: nothing is
    /// counted, so the call settles not sent, and nothing reaches the
    /// server.
    #[test]
    fn a_call_that_never_left_is_not_handed_over() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let uri: Arc<str> = format!("http://{}/mcp", listener.local_addr().unwrap()).into();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let passed = Instant::now() - std::time::Duration::from_secs(1);
        let later = Instant::now() + std::time::Duration::from_secs(5);
        let cases = [
            (Vec::new(), passed, HttpFault::Timeout),
            (
                vec![("x-bad".to_string(), EnvValue::Plain("a\r\nb".into()))],
                later,
                HttpFault::Transport(REQUEST_UNBUILT),
            ),
        ];
        for (headers, deadline, expected) in cases {
            let (client, tap, fault) = client_for(&uri, headers, deadline);
            let answered = runtime.block_on(client.post_message(
                uri.clone(),
                tools_call(),
                None,
                None,
                HashMap::new(),
            ));
            assert!(answered.is_err());
            assert_eq!(fault.lock().unwrap().clone(), Some(expected.clone()));
            assert_eq!(tap.seal(), 0, "{expected:?}: no call was handed over");
            assert!(listener.accept().is_err(), "{expected:?}: nothing was sent");
        }
    }

    /// Every POST starts with no fault: an answered message after a failed
    /// one leaves none behind for the session to misreport.
    #[test]
    fn a_post_clears_the_fault_of_an_earlier_one() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let uri: Arc<str> = format!("http://{}/mcp", listener.local_addr().unwrap()).into();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                seen.extend_from_slice(&buf[..n]);
                let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&seen[..at]).to_ascii_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if seen.len() >= at + 4 + length {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, _tap, fault) = client_for(
            &uri,
            Vec::new(),
            Instant::now() + std::time::Duration::from_secs(5),
        );
        *fault.lock().unwrap() = Some(HttpFault::Auth { status: 401 });
        let notification: ClientJsonRpcMessage = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }))
        .unwrap();
        let answered =
            runtime.block_on(client.post_message(uri, notification, None, None, HashMap::new()));
        assert!(answered.is_ok(), "{:?}", answered.err());
        assert_eq!(*fault.lock().unwrap(), None);
        server.join().unwrap();
    }

    /// A server that answers one request with the response head of
    /// `content_type` and then holds the body back until the client hangs
    /// up (the read of the test's own request ends the hold, so nothing
    /// here waits on a fixed sleep).
    fn stalled_body(content_type: &'static str) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // The whole request: its head, then its body by its length.
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            let body_at = loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => seen.extend_from_slice(&buf[..n]),
                }
                if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&seen[..body_at]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while seen.len() < body_at + length {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => seen.extend_from_slice(&buf[..n]),
                }
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: 64\r\n\r\n{{\"jsonrpc\":"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            // Held open, nothing more written, until the client closes.
            while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
        });
        url
    }

    /// The headers arrive, the body stalls: both readers, through the
    /// real ureq body reader and the client's own exchange, report the
    /// stall as `Timeout` at the configured short deadline, never as a
    /// transport failure.
    #[test]
    fn a_stalled_body_is_a_timeout_for_both_readers() {
        for content_type in ["application/json", "text/event-stream"] {
            let url = stalled_body(content_type);
            let tap = LineTap::new();
            let mut server = HttpServer {
                url: url.clone(),
                headers: Vec::new(),
                bearer: None,
            };
            let deadline = Instant::now() + std::time::Duration::from_millis(500);
            let client = UreqClient::new(
                &mut server,
                Arc::new(Mutex::new(deadline)),
                tap,
                Arc::new(Mutex::new(None)),
            );
            let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })
                .to_string()
                .into_bytes();
            let shape = Shape::of(&body);
            let started = Instant::now();
            let request = client.request(&url, body, &[]).unwrap();
            let answered = client.exchange(request, &shape, deadline);
            assert!(
                matches!(answered, Err(HttpFault::Timeout)),
                "{content_type}: {:?}",
                answered.err()
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "{content_type}: ended at the deadline"
            );
        }
    }

    /// The one mapping: an io timeout and ureq's timeout, bare or wrapped
    /// in an io error of another kind, are `Timeout`; anything else is the
    /// transport's.
    #[test]
    fn the_body_fault_recognizes_every_timeout_shape() {
        use std::io::{Error, ErrorKind};
        let timeouts = [
            Error::from(ErrorKind::TimedOut),
            Error::other(ureq::Error::Timeout(ureq::Timeout::Global)),
            Error::other(ureq::Error::Timeout(ureq::Timeout::PerRead)),
            Error::other(ureq::Error::Io(Error::from(ErrorKind::TimedOut))),
            ureq::Error::Timeout(ureq::Timeout::Global).into_io(),
        ];
        for e in timeouts {
            let shown = e.to_string();
            assert!(matches!(body_fault(e), HttpFault::Timeout), "{shown}");
        }
        let others = [
            Error::from(ErrorKind::ConnectionReset),
            Error::other(ureq::Error::ConnectionFailed),
            Error::other("plain"),
        ];
        for e in others {
            let shown = e.to_string();
            assert!(matches!(body_fault(e), HttpFault::Transport(_)), "{shown}");
        }
    }

    #[test]
    fn the_media_type_ignores_parameters_and_case() {
        let v = |s: &str| HeaderValue::from_str(s).unwrap();
        assert_eq!(
            media_type(Some(&v("application/json; charset=utf-8"))).as_deref(),
            Some("application/json")
        );
        assert_eq!(
            media_type(Some(&v("Text/Event-Stream"))).as_deref(),
            Some("text/event-stream")
        );
        assert_eq!(
            media_type(Some(&v("application/jsonx"))).as_deref(),
            Some("application/jsonx")
        );
    }
}
