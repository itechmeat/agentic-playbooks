//! One server conversation on its own worker thread.

use std::collections::HashSet;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig, Implementation,
    PaginatedRequestParams, ProtocolVersion,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService,
    ServiceError,
};
use serde_json::{Map, Value};
use tokio::process::Child;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use std::path::{Path, PathBuf};

use std::sync::{Arc, Mutex};

use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};

use crate::http::{HttpFault, HttpServer, UreqClient};
use crate::stdio::{EnvValue, StartFrom, StdioServer};
use crate::tap::{LineTap, MAX_LINE_BYTES};

/// The cleanup a failure runs inside the worker before it answers (3 s for
/// the exit after end of input, 2 s for the reap with the close's drain
/// inside it, 1 s to see the group empty) plus 1 s for the reply to cross
/// the channel.
pub const REPLY_CLEANUP_ALLOWANCE: Duration = Duration::from_secs(7);

pub enum Server {
    Stdio(StdioServer),
    /// Reached over Streamable HTTP in the modern revision only; nothing
    /// is started.
    Http(HttpServer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Auto,
    Modern,
    Legacy,
}

/// The revision the conversation runs on, with the protocol version: one
/// of `SUPPORTED_VERSIONS`, never a string the server sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Era {
    /// `server/discover` answered; the negotiated version.
    Modern(&'static str),
    /// `initialize`; the version the server answered with.
    Legacy(&'static str),
}

/// One `tools/call` answer the server completed.
#[derive(Debug, Clone)]
pub enum ToolAnswer {
    /// `content` blocks as JSON objects, `structuredContent` when present
    /// (an explicit `null` is `Some(Value::Null)`), `isError` (absent is
    /// `false`).
    Complete {
        content: Vec<serde_json::Value>,
        structured: Option<serde_json::Value>,
        is_error: bool,
    },
    /// `resultType: input_required`: the server needs input apb does not give.
    InputRequired,
    /// The server answered with a task handle (Tasks extension).
    Task,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// The program could not be started.
    Spawn,
    /// The deadline passed.
    Timeout,
    /// The transport broke (the process exited, a pipe closed, a read failed).
    Transport,
    /// The server answered with a JSON-RPC error.
    Rejected { code: i64 },
    /// The server is legacy under `Lifecycle::Modern`, or speaks no
    /// version this client knows.
    Unsupported,
    /// A list went past its bounds (`list_tools` limits), or a line from
    /// the server passed `MAX_LINE_BYTES` (the message then starts
    /// `mcp_message_too_large:`).
    TooLarge,
    /// `apb_core::process_mode::guard_program_start` refused the spawn (a
    /// read-only or a scenario process).
    Refused,
    /// `StartFrom::PathCheckedAgain`: the program's path no longer held the
    /// bytes that were hashed (or no regular file) right before the spawn;
    /// nothing was started.
    ProgramChanged,
    /// The server refused the authorization (HTTP 401 or 403).
    Auth { status: u16 },
}

/// What the close saw of the server's process group after the server was
/// reaped. The server is started as the leader of its own group; the close
/// signals that group while the leader is still unreaped, so the group id
/// cannot have been reused when the signal goes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupEnd {
    /// No process of the group was left once the server was reaped.
    Empty,
    /// The group was not seen empty within the close's bound: a process
    /// the server started may still run. Nothing is claimed about a
    /// process that left the group.
    NotShownEmpty,
}

/// How a session's server process ended. Only `Reaped` says it is gone:
/// the child handle's `wait` returned its exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The server exited after its stdin closed, or after the group kill,
    /// and was reaped; `killed` when it had not exited by itself. `group`
    /// is what the close saw of the rest of its process group.
    Reaped { killed: bool, group: GroupEnd },
    /// The kill was sent through the handle but `wait` did not return
    /// within its bound; whether the process is gone is not known.
    Unconfirmed,
    /// No process was started (a spawn failure, an HTTP server) or `close`
    /// already ran.
    NotStarted,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    /// Plain text; never contains an argument or a secret the caller passed.
    pub message: String,
    /// What the bounded close after a failure proved. `NotStarted` when
    /// the failure came before the spawn or the session stays open (a
    /// `list_tools` failure, which the engine closes through the session).
    pub ended: Ended,
}

/// Why a `tools/call` has no answer.
#[derive(Debug, Clone)]
pub enum CallFailure {
    /// No byte of the request reached the server's stdin: the session was
    /// closed, its worker gone, or the pipe refused the first write.
    NotSent(Failure),
    /// The server answered the call with a JSON-RPC error.
    Answered(Failure),
    /// The request was handed to the transport and no answer came: a
    /// timeout, a closed or broken transport, an unexpected answer.
    Lost(Failure),
}

/// How long the end of input may take before the kill.
const EXIT_WAIT: Duration = Duration::from_secs(3);
/// How long the reap may take after the group kill.
const REAP_WAIT: Duration = Duration::from_secs(2);
/// How long the close waits to see the server's group empty after the reap.
const GROUP_WAIT: Duration = Duration::from_secs(1);
/// The most time the close spends counting what the reader never got to:
/// the drain sits inside the reap wait, so the close's documented length
/// and the reply wait keep their distance.
const DRAIN_WAIT: Duration = Duration::from_millis(250);
/// How long a caller waits for the worker thread to end after it answered.
const JOIN_WAIT: Duration = Duration::from_secs(1);
/// The reply wait of `close`: its 6 s bound plus 1 s for the reply.
const CLOSE_REPLY_WAIT: Duration = Duration::from_secs(7);

type Reply<T> = std::sync::mpsc::Sender<T>;

enum Command {
    List {
        deadline: Instant,
        max_pages: u32,
        max_tools: usize,
        max_bytes: usize,
        reply: Reply<Result<Vec<Value>, Failure>>,
    },
    Call {
        name: String,
        arguments: Map<String, Value>,
        deadline: Instant,
        reply: Reply<Result<ToolAnswer, CallFailure>>,
    },
    Close {
        reply: Reply<Ended>,
    },
}

fn failure(kind: FailureKind, message: impl Into<String>, ended: Ended) -> Failure {
    Failure {
        kind,
        message: message.into(),
        ended,
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// One server conversation. Every method blocks the caller on a channel
/// to the session's own worker thread, never on a runtime, so a session
/// works from inside an async handler.
pub struct Session {
    era: Era,
    commands: Option<UnboundedSender<Command>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// The worker's line tap, shared for its counts.
    tap: LineTap,
}

impl Session {
    /// Starts the server and opens the conversation in `lifecycle`, as
    /// client `client_name` / `client_version` with no client
    /// capabilities. Bounded by `deadline`. Every failure after the spawn
    /// runs the bounded close inside the worker before `Failure` is
    /// returned; `Failure.ended` says what that close proved.
    pub fn connect(
        server: Server,
        lifecycle: Lifecycle,
        client_name: &str,
        client_version: &str,
        deadline: Instant,
    ) -> Result<Session, Failure> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (commands, inbox) = unbounded_channel();
        let info = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(client_name, client_version),
        );
        let tap = LineTap::new();
        let worker_tap = tap.clone();
        let worker = std::thread::Builder::new()
            .name("apb-mcp-client".into())
            .spawn(move || {
                run_worker(
                    server, lifecycle, info, deadline, ready_tx, inbox, worker_tap,
                )
            })
            .map_err(|e| {
                failure(
                    FailureKind::Spawn,
                    format!("the MCP client thread could not be started: {:?}", e.kind()),
                    Ended::NotStarted,
                )
            })?;
        match ready_rx.recv_timeout(remaining(deadline) + REPLY_CLEANUP_ALLOWANCE) {
            Ok(Ok(era)) => Ok(Session {
                era,
                commands: Some(commands),
                worker: Some(worker),
                tap,
            }),
            Ok(Err(f)) => {
                join_bounded(worker);
                Err(f)
            }
            Err(RecvTimeoutError::Timeout) => Err(failure(
                FailureKind::Timeout,
                "the MCP client did not answer in time",
                Ended::Unconfirmed,
            )),
            Err(RecvTimeoutError::Disconnected) => Err(failure(
                FailureKind::Transport,
                "the MCP client ended without an answer",
                Ended::Unconfirmed,
            )),
        }
    }

    /// The revision the conversation runs on.
    pub fn era(&self) -> &Era {
        &self.era
    }

    /// How many whole lines the server wrote that did not start a JSON
    /// object (log text, blank lines) and were dropped unread: never passed
    /// on, never parsed. A count only; their text is kept nowhere.
    pub fn dropped_lines(&self) -> usize {
        self.tap.dropped()
    }

    /// Every tool, following `nextCursor`, as the raw JSON objects the
    /// server sent (read off the wire by the line tap). At most
    /// `max_pages` pages, `max_tools` tools and `max_bytes` bytes summed
    /// over the pages' answer lines, else `TooLarge`.
    pub fn list_tools(
        &self,
        deadline: Instant,
        max_pages: u32,
        max_tools: usize,
        max_bytes: usize,
    ) -> Result<Vec<Value>, Failure> {
        let (reply, answer) = std::sync::mpsc::channel();
        let sent = self.send(Command::List {
            deadline,
            max_pages,
            max_tools,
            max_bytes,
            reply,
        });
        if !sent {
            return Err(closed());
        }
        match answer.recv_timeout(remaining(deadline) + REPLY_CLEANUP_ALLOWANCE) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err(failure(
                FailureKind::Timeout,
                "the MCP client did not answer in time",
                Ended::NotStarted,
            )),
            Err(RecvTimeoutError::Disconnected) => Err(closed()),
        }
    }

    /// One `tools/call` with no multi round-trip follow-up.
    pub fn call_tool(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        deadline: Instant,
    ) -> Result<ToolAnswer, CallFailure> {
        // The edge's own check again: arguments the tap could not parse
        // inside the request line are refused before anything is queued.
        if let Err(refusal) = crate::frame::check_arguments(&arguments) {
            return Err(CallFailure::NotSent(failure(
                FailureKind::Refused,
                refusal.message(),
                Ended::NotStarted,
            )));
        }
        let (reply, answer) = std::sync::mpsc::channel();
        let sent = self.send(Command::Call {
            name: name.to_string(),
            arguments,
            deadline,
            reply,
        });
        if !sent {
            // Nothing was handed to the transport.
            return Err(CallFailure::NotSent(closed()));
        }
        match answer.recv_timeout(remaining(deadline) + REPLY_CLEANUP_ALLOWANCE) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err(CallFailure::Lost(failure(
                FailureKind::Timeout,
                "the MCP client did not answer in time",
                Ended::NotStarted,
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(CallFailure::Lost(closed())),
        }
    }

    /// Ends the conversation: closes the server's stdin and waits up to
    /// 3 s for the server to exit (without reaping it); then, with the
    /// server still unreaped, kills its whole process group; reaps the
    /// server within 2 s, with the drain of the pipe's last lines inside
    /// that wait (at most 250 ms of it), and looks up to 1 s for its group
    /// to be empty. Idempotent (a second call answers `NotStarted`).
    pub fn close(&mut self) -> Ended {
        let Some(commands) = self.commands.take() else {
            return Ended::NotStarted;
        };
        let (reply, answer) = std::sync::mpsc::channel();
        let ended = if commands.send(Command::Close { reply }).is_ok() {
            answer
                .recv_timeout(CLOSE_REPLY_WAIT)
                .unwrap_or(Ended::Unconfirmed)
        } else {
            Ended::Unconfirmed
        };
        if let Some(worker) = self.worker.take() {
            join_bounded(worker);
        }
        ended
    }

    fn send(&self, command: Command) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|c| c.send(command).is_ok())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn closed() -> Failure {
    failure(
        FailureKind::Transport,
        "the MCP session is closed",
        Ended::NotStarted,
    )
}

/// Joins the worker thread for at most `JOIN_WAIT`, then leaves it
/// detached.
fn join_bounded(worker: std::thread::JoinHandle<()>) {
    let until = Instant::now() + JOIN_WAIT;
    while !worker.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    if worker.is_finished() {
        let _ = worker.join();
    }
}

fn run_worker(
    server: Server,
    lifecycle: Lifecycle,
    info: ClientConfig,
    deadline: Instant,
    ready: Reply<Result<Era, Failure>>,
    mut inbox: UnboundedReceiver<Command>,
    tap: LineTap,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready.send(Err(failure(
                FailureKind::Transport,
                format!("the MCP client runtime could not be built: {:?}", e.kind()),
                Ended::NotStarted,
            )));
            return;
        }
    };
    runtime.block_on(async move {
        let mut conn = match Conn::open(server, lifecycle, info, deadline, tap).await {
            Ok(conn) => conn,
            Err(failed) => {
                let (f, mut closed) = *failed;
                let _ = ready.send(Err(f));
                closed.reap_late().await;
                return;
            }
        };
        if ready.send(Ok(conn.era.clone())).is_err() {
            // The caller stopped waiting: close, and keep reaping like
            // every other cleanup branch.
            conn.finish().await;
            return;
        }
        while let Some(command) = inbox.recv().await {
            match command {
                Command::List {
                    deadline,
                    max_pages,
                    max_tools,
                    max_bytes,
                    reply,
                } => {
                    let r = conn
                        .list_tools(deadline, max_pages, max_tools, max_bytes)
                        .await;
                    let _ = reply.send(r);
                }
                Command::Call {
                    name,
                    arguments,
                    deadline,
                    reply,
                } => {
                    let r = conn.call_tool(name, arguments, deadline).await;
                    let _ = reply.send(r);
                }
                Command::Close { reply } => {
                    let ended = conn.close().await;
                    let _ = reply.send(ended);
                    conn.reap_late().await;
                    return;
                }
            }
        }
        conn.finish().await;
    });
}

/// The worker's side of a session: the child handle, the rmcp service
/// over the tapped pipes, the tap, and the private working directory.
struct Conn {
    child: Option<Child>,
    /// A child whose reap the close could not confirm: the worker keeps
    /// waiting for it after the close answered (`reap_late`).
    late: Option<Child>,
    service: Option<RunningService<RoleClient, ClientConfig>>,
    tap: LineTap,
    era: Era,
    workdir: Option<WorkDir>,
    /// The HTTP client's shared state, for an HTTP session.
    http: Option<HttpParts>,
    /// A duplicate of the server's stdout descriptor: the close reads from
    /// it what the reader, dropped with the service, never got to, and
    /// counts it. `None` for an HTTP session, which has no reader.
    stdout_drain: Option<std::fs::File>,
}

/// What the session shares with its HTTP client: the deadline of the
/// operation in progress and the client's latest fault.
struct HttpParts {
    deadline: Arc<Mutex<Instant>>,
    fault: Arc<Mutex<Option<HttpFault>>>,
}

/// The refusal of an HTTP server outside the modern revision.
const HTTP_MODERN_ONLY: &str =
    "an MCP server over HTTP is reached in the stateless 2026-07-28 revision only";

/// The protocol versions this client speaks, the only strings an `Era`
/// holds. A server's version is compared with them before anything keeps,
/// cuts, shows or stores it.
pub const SUPPORTED_VERSIONS: [&str; 5] = [
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// The first version of the modern revision (`server/discover`).
const MODERN_FROM: &str = "2026-07-28";

/// The supported constant equal to the version a server reported, else
/// `None`. Only the constant leaves this function.
fn supported_version(reported: &str) -> Option<&'static str> {
    SUPPORTED_VERSIONS.iter().copied().find(|v| *v == reported)
}

/// The failure of a listing whose raw result does not read as what rmcp
/// returned.
const LISTING_MISMATCH: &str = "mcp_listing_mismatch: the tool list rmcp read differs from the answer apb kept for hashing; the listing is not used";

/// Whether the raw `result` the tap kept reads, as rmcp's own type, as
/// the tools and the cursor rmcp returned.
fn same_listing(raw: &Value, typed: &rmcp::model::ListToolsResult) -> bool {
    serde_json::from_value::<rmcp::model::ListToolsResult>(raw.clone())
        .is_ok_and(|parsed| parsed.tools == typed.tools && parsed.next_cursor == typed.next_cursor)
}

/// The refusal of a version this client does not speak; it never echoes
/// what the server sent.
const VERSION_UNSUPPORTED: &str = "mcp_protocol_unsupported: the MCP server answered with a protocol version this client does not support";

impl Conn {
    fn new(child: Option<Child>, workdir: Option<WorkDir>) -> Conn {
        Conn {
            child,
            late: None,
            service: None,
            tap: LineTap::new(),
            era: Era::Legacy(SUPPORTED_VERSIONS[1]),
            workdir,
            http: None,
            stdout_drain: None,
        }
    }

    async fn open(
        server: Server,
        lifecycle: Lifecycle,
        info: ClientConfig,
        deadline: Instant,
        tap: LineTap,
    ) -> Result<Conn, Box<(Failure, Conn)>> {
        match server {
            Server::Stdio(server) => Conn::open_stdio(server, lifecycle, info, deadline, tap).await,
            Server::Http(server) => Conn::open_http(server, lifecycle, info, deadline, tap).await,
        }
    }

    /// Reaches an HTTP server: apb's `UreqClient` under rmcp's Streamable
    /// HTTP worker, in the modern revision only, with the stdio
    /// handshake's checks. Nothing is started, so the program-start guard
    /// is not asked.
    async fn open_http(
        mut server: HttpServer,
        lifecycle: Lifecycle,
        info: ClientConfig,
        deadline: Instant,
        tap: LineTap,
    ) -> Result<Conn, Box<(Failure, Conn)>> {
        let mut conn = Conn::new(None, None);
        conn.tap = tap;
        if lifecycle != Lifecycle::Modern {
            return Err(Box::new((
                failure(
                    FailureKind::Unsupported,
                    HTTP_MODERN_ONLY,
                    Ended::NotStarted,
                ),
                conn,
            )));
        }
        if Instant::now() >= deadline {
            return Err(Box::new((
                failure(
                    FailureKind::Timeout,
                    "the deadline passed before the MCP server was reached; nothing was sent",
                    Ended::NotStarted,
                ),
                conn,
            )));
        }
        let parts = HttpParts {
            deadline: Arc::new(Mutex::new(deadline)),
            fault: Arc::new(Mutex::new(None)),
        };
        let client = UreqClient::new(
            &mut server,
            parts.deadline.clone(),
            conn.tap.clone(),
            parts.fault.clone(),
        );
        conn.http = Some(parts);
        let config = StreamableHttpClientTransportConfig::with_uri(server.url.as_str())
            .max_concurrent_requests(1)
            .reinit_on_expired_session(false);
        let transport = StreamableHttpClientTransport::with_client(client, config);
        match conn.handshake(transport, lifecycle, info, deadline).await {
            Ok(()) => Ok(conn),
            Err((kind, message)) => {
                let ended = conn.close().await;
                Err(Box::new((failure(kind, message, ended), conn)))
            }
        }
    }

    async fn open_stdio(
        server: StdioServer,
        lifecycle: Lifecycle,
        info: ClientConfig,
        deadline: Instant,
        tap: LineTap,
    ) -> Result<Conn, Box<(Failure, Conn)>> {
        // A deadline already passed starts nothing: the timeout below
        // bounds the handshake only, and no close undoes a start.
        if Instant::now() >= deadline {
            return Err(Box::new((
                failure(
                    FailureKind::Timeout,
                    "the deadline passed before the MCP server was started; it was not started",
                    Ended::NotStarted,
                ),
                Conn::new(None, None),
            )));
        }
        let (mut child, workdir) = match spawn(server) {
            Ok(started) => started,
            Err(f) => return Err(Box::new((f, Conn::new(None, None)))),
        };
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        let mut conn = Conn::new(Some(child), Some(workdir));
        conn.tap = tap;
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            let ended = conn.close().await;
            return Err(Box::new((
                failure(
                    FailureKind::Transport,
                    "the MCP server's pipes were not opened",
                    ended,
                ),
                conn,
            )));
        };
        let tap = conn.tap.clone();
        // The close counts what the reader, dropped with the service,
        // never got to: the same pipe through a duplicate descriptor. The
        // duplicate is close-on-exec from its creation (`dup` never sets
        // the flag), so no process apb spawns while the session lives
        // inherits the server's output.
        #[cfg(unix)]
        {
            use std::os::fd::{AsFd as _, AsRawFd as _};
            if let Ok(fd) = stdout.as_fd().try_clone_to_owned() {
                let file = std::fs::File::from(fd);
                // The drain reads with nonblocking semantics of its own;
                // the flag lives on the pipe's open description, which the
                // reader keeps nonblocking too. Without it, no drain: the
                // close counts only what the reader got to.
                let raw = file.as_raw_fd();
                let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
                if flags >= 0
                    && unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0
                {
                    conn.stdout_drain = Some(file);
                }
            }
        }
        let transport = (tap.reader(stdout), tap.writer(stdin));
        match conn.handshake(transport, lifecycle, info, deadline).await {
            Ok(()) => Ok(conn),
            Err(refused) => {
                let ended = conn.close().await;
                Err(Box::new((failure(refused.0, refused.1, ended), conn)))
            }
        }
    }

    /// The handshake in `lifecycle` over either transport, with the
    /// version check, the response cache turned off; `Err` with the
    /// failure's kind and message (the caller closes).
    async fn handshake<T, E, A>(
        &mut self,
        transport: T,
        lifecycle: Lifecycle,
        info: ClientConfig,
        deadline: Instant,
    ) -> Result<(), (FailureKind, String)>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let conn = self;
        let (mode, info) = match lifecycle {
            Lifecycle::Auto => (
                ClientLifecycleMode::Auto {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                    legacy_version: Some(ProtocolVersion::V_2025_11_25),
                },
                info,
            ),
            Lifecycle::Modern => (
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
                info,
            ),
            Lifecycle::Legacy => (
                ClientLifecycleMode::Initialize,
                info.with_protocol_version(ProtocolVersion::V_2025_11_25),
            ),
        };
        let opened = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            info.serve_with_lifecycle(transport, mode),
        )
        .await;
        let refused = match opened {
            Ok(Ok(service)) => {
                // Compared whole with the supported set before anything
                // else reads it; only the constant is kept.
                let version = service
                    .peer()
                    .peer_info()
                    .map(|p| supported_version(p.protocol_version.as_str()));
                // No response cache: a listing a server marked with a
                // `ttlMs` would be answered from it without a request, so
                // without the raw capture every listing is read from.
                service
                    .peer()
                    .set_response_cache_config(rmcp::ClientCacheConfig::disabled())
                    .await;
                conn.service = Some(service);
                match version {
                    Some(Some(v)) => {
                        conn.era = if v >= MODERN_FROM {
                            Era::Modern(v)
                        } else {
                            Era::Legacy(v)
                        };
                        return Ok(());
                    }
                    Some(None) => (FailureKind::Unsupported, VERSION_UNSUPPORTED.to_string()),
                    None => (
                        FailureKind::Transport,
                        "the MCP server's version is not known after the handshake".to_string(),
                    ),
                }
            }
            Ok(Err(e)) => conn.handshake_failure(lifecycle, &e),
            Err(_) => match conn.read_refused() {
                Some(refused) => refused,
                None => (
                    FailureKind::Timeout,
                    "the MCP handshake did not end before the deadline".to_string(),
                ),
            },
        };
        Err(refused)
    }

    /// Moves the deadline the HTTP client's next request runs under.
    fn set_deadline(&self, deadline: Instant) {
        if let Some(parts) = &self.http
            && let Ok(mut d) = parts.deadline.lock()
        {
            *d = deadline;
        }
    }

    /// The HTTP client's latest fault, mapped (design D7 "The client" item
    /// 5): `Auth` is `Auth`, `TooLarge` is `TooLarge`, every other fault
    /// `Transport` (an elapsed deadline `Timeout`), its message apb's own.
    fn http_fault(&self) -> Option<(FailureKind, String)> {
        let fault = self.http.as_ref()?.fault.lock().ok()?.clone()?;
        let kind = match &fault {
            HttpFault::Auth { status } => FailureKind::Auth { status: *status },
            HttpFault::TooLarge(_) => FailureKind::TooLarge,
            HttpFault::Timeout => FailureKind::Timeout,
            HttpFault::NotSent
            | HttpFault::Transport(_)
            | HttpFault::Status(_)
            | HttpFault::Invalid(_) => FailureKind::Transport,
        };
        Some((kind, fault.to_string()))
    }

    fn handshake_failure(
        &self,
        lifecycle: Lifecycle,
        e: &ClientInitializeError,
    ) -> (FailureKind, String) {
        if let Some(refused) = self.read_refused() {
            return refused;
        }
        match e {
            // rmcp's text lists the versions the server offered; it is not
            // kept.
            ClientInitializeError::NoCompatibleProtocolVersion { .. } => {
                (FailureKind::Unsupported, VERSION_UNSUPPORTED.to_string())
            }
            ClientInitializeError::JsonRpcError(_) if lifecycle == Lifecycle::Modern => (
                FailureKind::Unsupported,
                format!("the MCP server does not speak the 2026-07-28 revision: {e}"),
            ),
            ClientInitializeError::LegacyFallbackFailed { fallback, .. } => match &**fallback {
                ClientInitializeError::NoCompatibleProtocolVersion { .. } => {
                    (FailureKind::Unsupported, VERSION_UNSUPPORTED.to_string())
                }
                other => (FailureKind::Transport, other.to_string()),
            },
            _ => (FailureKind::Transport, e.to_string()),
        }
    }

    async fn list_tools(
        &self,
        deadline: Instant,
        max_pages: u32,
        max_tools: usize,
        max_bytes: usize,
    ) -> Result<Vec<Value>, Failure> {
        let Some(service) = self.service.as_ref() else {
            return Err(closed());
        };
        self.set_deadline(deadline);
        let pages = async {
            let mut tools: Vec<Value> = Vec::new();
            let mut cursor: Option<String> = None;
            let mut seen: HashSet<String> = HashSet::new();
            let mut bytes = 0usize;
            let mut page = 0u32;
            loop {
                if page >= max_pages {
                    return Err(failure(
                        FailureKind::TooLarge,
                        format!("the server's tool list has more than {max_pages} pages"),
                        Ended::NotStarted,
                    ));
                }
                page += 1;
                let params = PaginatedRequestParams::default().with_cursor(cursor.clone());
                let typed = match service.peer().list_tools(Some(params)).await {
                    Ok(typed) => typed,
                    Err(e) => return Err(self.service_failure(e)),
                };
                let (raw, len) = match self.tap.take_last_listing() {
                    Ok(Some(taken)) => taken,
                    Ok(None) => return Err(not_read()),
                    Err(m) => return Err(failure(FailureKind::Transport, m, Ended::NotStarted)),
                };
                // The raw result hashed is the one rmcp accepted: the tap
                // keeps the only whole response for the request's id, and
                // the tools and cursor rmcp returned must read the same
                // from it, else the listing is not used.
                if !same_listing(&raw, &typed) {
                    return Err(failure(
                        FailureKind::Transport,
                        LISTING_MISMATCH,
                        Ended::NotStarted,
                    ));
                }
                bytes = bytes.saturating_add(len);
                if bytes > max_bytes {
                    return Err(failure(
                        FailureKind::TooLarge,
                        format!("the server's tool list is longer than {max_bytes} bytes"),
                        Ended::NotStarted,
                    ));
                }
                let Some(Value::Array(listed)) = raw.get("tools") else {
                    return Err(not_read());
                };
                tools.extend(listed.iter().cloned());
                if tools.len() > max_tools {
                    return Err(failure(
                        FailureKind::TooLarge,
                        format!("the server lists more than {max_tools} tools"),
                        Ended::NotStarted,
                    ));
                }
                match raw.get("nextCursor").and_then(Value::as_str) {
                    None => return Ok(tools),
                    Some(next) => {
                        if !seen.insert(next.to_string()) {
                            return Err(failure(
                                FailureKind::TooLarge,
                                "the server's tool list did not end",
                                Ended::NotStarted,
                            ));
                        }
                        cursor = Some(next.to_string());
                    }
                }
            }
        };
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), pages).await {
            Ok(r) => r,
            Err(_) => Err(failure(
                FailureKind::Timeout,
                "the tool list did not arrive before the deadline",
                Ended::NotStarted,
            )),
        }
    }

    /// A failed request that was not a `tools/call`.
    fn service_failure(&self, e: ServiceError) -> Failure {
        if let Some((kind, m)) = self.read_refused() {
            return failure(kind, m, Ended::NotStarted);
        }
        if let Some(m) = self.tap.refused() {
            return failure(FailureKind::Refused, m, Ended::NotStarted);
        }
        let kind = match &e {
            ServiceError::McpError(data) => FailureKind::Rejected {
                code: i64::from(data.code.0),
            },
            ServiceError::Timeout { .. } => FailureKind::Timeout,
            _ => FailureKind::Transport,
        };
        let message = match &e {
            ServiceError::McpError(data) => data.message.to_string(),
            _ => e.to_string(),
        };
        failure(kind, message, Ended::NotStarted)
    }

    /// Why the reader ended the server's output, when it did: a line limit
    /// (`TooLarge`) or a line it could not read completely (`Transport`,
    /// `mcp_message_invalid:`; nothing of that line reached rmcp).
    fn read_refused(&self) -> Option<(FailureKind, String)> {
        if let Some(refused) = self.http_fault() {
            return Some(refused);
        }
        if let Some(m) = self.tap.too_large() {
            return Some((FailureKind::TooLarge, m));
        }
        self.tap.invalid().map(|m| (FailureKind::Transport, m))
    }

    /// One `tools/call`.
    ///
    /// Invariant: `CallFailure::NotSent` is answered only with proof that
    /// no byte of the request can still reach the server. Every outcome
    /// without an answer first seals the line tap (`LineTap::seal`), which
    /// stops every further write to the server's stdin, and reads the
    /// hand-over count in the same locked step; only a count that did not
    /// move since before the request proves it unwritten. A request rmcp
    /// still holds in its queue, or hands the writer later (the deadline
    /// passed while it was queued, the future was dropped), can then never
    /// be written. Without that proof the outcome is `Lost` (unknown). A
    /// sealed session writes nothing more; the engine closes it.
    async fn call_tool(
        &self,
        name: String,
        arguments: Map<String, Value>,
        deadline: Instant,
    ) -> Result<ToolAnswer, CallFailure> {
        let Some(service) = self.service.as_ref() else {
            return Err(CallFailure::NotSent(closed()));
        };
        self.set_deadline(deadline);
        let handed_before = self.tap.calls_handed();
        let params = CallToolRequestParams::new(name).with_arguments(arguments);
        let answered = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            service.call_tool_once(params),
        )
        .await;
        let lost = |kind: FailureKind, message: String| {
            // Seal first, then compare: see the invariant above.
            let handed = self.tap.seal() != handed_before;
            let (kind, message) = match (self.read_refused(), self.tap.refused()) {
                // A refused authorization keeps its kind; every other
                // refusal of the reader or the client is a transport
                // failure naming its text.
                (Some((k @ FailureKind::Auth { .. }, m)), _) => (k, m),
                (Some((FailureKind::Timeout, m)), _) => (FailureKind::Timeout, m),
                (Some((_, m)), _) => (FailureKind::Transport, m),
                // The writer refused a line it could not classify: none
                // of it was forwarded, and the seal decides as always.
                (None, Some(m)) => (FailureKind::Refused, m),
                (None, None) => (kind, message),
            };
            let f = failure(kind, message, Ended::NotStarted);
            if handed {
                CallFailure::Lost(f)
            } else {
                CallFailure::NotSent(f)
            }
        };
        match answered {
            // rmcp 3.11 reads an answer that is no call result as an empty
            // successful call; apb 1.0 (rmcp 3.9) read it as unexpected,
            // and still does.
            Ok(Ok(CallToolResponse::Complete(_))) if self.tap.call_answer_untyped() => Err(lost(
                FailureKind::Transport,
                ServiceError::UnexpectedResponse.to_string(),
            )),
            Ok(Ok(CallToolResponse::Complete(r))) => Ok(ToolAnswer::Complete {
                content: r
                    .content
                    .iter()
                    .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
                    .collect(),
                structured: r.structured_content,
                is_error: r.is_error == Some(true),
            }),
            Ok(Ok(CallToolResponse::InputRequired(_))) => Ok(ToolAnswer::InputRequired),
            Ok(Ok(CallToolResponse::Task(_))) => Ok(ToolAnswer::Task),
            Ok(Ok(_)) => Err(lost(
                FailureKind::Transport,
                "the tools/call answer has a shape this client does not know".into(),
            )),
            Ok(Err(ServiceError::McpError(data))) => match self.read_refused() {
                Some((_, m)) => Err(CallFailure::Lost(failure(
                    FailureKind::Transport,
                    m,
                    Ended::NotStarted,
                ))),
                None => Err(CallFailure::Answered(failure(
                    FailureKind::Rejected {
                        code: i64::from(data.code.0),
                    },
                    data.message.to_string(),
                    Ended::NotStarted,
                ))),
            },
            Ok(Err(e @ ServiceError::Timeout { .. })) => {
                Err(lost(FailureKind::Timeout, e.to_string()))
            }
            Ok(Err(e)) => Err(lost(FailureKind::Transport, e.to_string())),
            Err(_) => Err(lost(
                FailureKind::Timeout,
                "the tools/call answer did not arrive before the deadline".into(),
            )),
        }
    }

    /// Cancels the service, so its write half drops and the server reads
    /// end of input; waits up to 3 s for the server to exit, without
    /// reaping it; then, the server still unreaped (so its pid, which is
    /// its group's id, cannot have been reused), kills its whole process
    /// group; reaps the server within 2 s; and looks up to 1 s for its
    /// group to be empty. A reap that does not come in time answers
    /// `Unconfirmed` and leaves the child to `reap_late`.
    async fn close(&mut self) -> Ended {
        if self.http.is_some() {
            // No process: the gate is sealed, the service cancelled and
            // the transport dropped; a POST still on its blocking thread
            // ends at its own deadline. Nothing claims a server stopped.
            self.tap.seal();
            if let Some(service) = self.service.take() {
                let _ = tokio::time::timeout(EXIT_WAIT, service.cancel()).await;
            }
            return Ended::NotStarted;
        }
        let Some(mut child) = self.child.take() else {
            return Ended::NotStarted;
        };
        let pid = child.id().and_then(|p| libc::pid_t::try_from(p).ok());
        let until = tokio::time::Instant::now() + EXIT_WAIT;
        if let Some(service) = self.service.take() {
            let _ = tokio::time::timeout_at(until, service.cancel()).await;
        }
        let mut exited = false;
        if let Some(pid) = pid {
            loop {
                exited = group::exited_unreaped(pid);
                if exited || tokio::time::Instant::now() >= until {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            group::kill(pid);
        } else {
            let _ = child.start_kill();
        }
        // The drain's bound sits inside the old reap wait, so the close
        // keeps its documented length and the reply wait its distance.
        let reap_wait = if self.stdout_drain.is_some() {
            REAP_WAIT.saturating_sub(DRAIN_WAIT)
        } else {
            REAP_WAIT
        };
        let reap_until = tokio::time::Instant::now() + reap_wait;
        match tokio::time::timeout_at(reap_until, child.wait()).await {
            Ok(Ok(_)) => {
                let group = match pid {
                    Some(pid) => group::wait_empty(pid, GROUP_WAIT).await,
                    None => GroupEnd::NotShownEmpty,
                };
                self.workdir = None;
                // The reader is done: it was dropped with the service, and
                // the server is reaped, so no writer of its group is left.
                // Whatever the reader never got to poll is counted from the
                // pipe itself, so the dropped-line count is final when this
                // answers (a line written right after the last answer is
                // in). The drain's own bound keeps a writer outside the
                // group from stretching the close.
                if let Some(drain) = self.stdout_drain.take()
                    && tokio::time::timeout(DRAIN_WAIT, drain_remaining(drain, &self.tap))
                        .await
                        .is_err()
                {
                    // No end of output in the bound: a process outside the
                    // group holds the pipe, and the count stays what the
                    // reader got to.
                }
                Ended::Reaped {
                    killed: !exited,
                    group,
                }
            }
            _ => {
                self.late = Some(child);
                Ended::Unconfirmed
            }
        }
    }

    /// The close of a session nobody waits for: `close`, then `reap_late`.
    async fn finish(&mut self) -> Ended {
        let ended = self.close().await;
        self.reap_late().await;
        ended
    }

    /// After a close that answered `Unconfirmed`: waits on the child
    /// handle until it is reaped, so it never stays a zombie, then removes
    /// the working directory. The caller no longer waits for this.
    async fn reap_late(&mut self) {
        if let Some(mut child) = self.late.take() {
            let _ = child.wait().await;
        }
        self.workdir = None;
    }
}

/// Reads the whole lines the server wrote that its reader never got to:
/// the pipe's remaining bytes, read once the server is reaped and no
/// writer of its group is left. Never blocks the runtime: the descriptor
/// was made nonblocking at its creation, a read answers at once, the
/// runtime is yielded after every chunk, and the drain's own deadline is
/// checked on every read, so a writer that keeps the pipe full cannot
/// stretch the drain past its bound. The counting runs as the bytes
/// arrive, so a drain the bound cuts short still counts what it read:
/// every read's bytes count toward the session's byte limit first
/// (`count_drained`), then whole lines go to the tap's rule (a partial
/// line's continuation counted from its newline, an unfinished last piece
/// never, only the line over the cap dropped), and a line still without its newline
/// when the read ends stays uncounted, like the reader's own unfinished
/// line.
async fn drain_remaining(drain: std::fs::File, tap: &LineTap) {
    use std::io::Read as _;
    let mut file = drain;
    let mut buf = [0u8; 16 * 1024];
    // The bytes of the line still without its newline, and whether its head
    // is the continuation the reader left behind.
    let mut carry: Vec<u8> = Vec::new();
    let mut partial = tap.partial_line();
    // Whether the line being carried is over the line cap: it cannot be
    // counted, so its bytes are dropped up to the next newline.
    let mut overlong = false;
    // The bound of the drain itself: it holds whether or not bytes flow.
    let deadline = tokio::time::Instant::now() + DRAIN_WAIT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // Every byte read counts toward the session's limit before
                // its lines are classified, the bytes of a dropped overlong
                // line included; past the limit nothing more is counted.
                if !tap.count_drained(n) {
                    break;
                }
                // The newline search covers the bytes just read: the
                // carried piece is never scanned a second time.
                let mut chunk = &buf[..n];
                if overlong {
                    // Only the line over the cap is dropped: it ends at the
                    // first newline, and what follows it in the same read
                    // is new lines. Its bytes are no dropped line.
                    let Some(at) = chunk.iter().position(|b| *b == b'\n') else {
                        tokio::task::yield_now().await;
                        continue;
                    };
                    overlong = false;
                    partial = false;
                    chunk = &chunk[at + 1..];
                }
                if let Some(at) = chunk.iter().rposition(|b| *b == b'\n') {
                    // The carried piece and the bytes up to the last
                    // newline are whole lines.
                    let mut whole = Vec::with_capacity(carry.len() + at + 1);
                    whole.extend_from_slice(&carry);
                    whole.extend_from_slice(&chunk[..=at]);
                    tap.drain_bytes(&whole, partial);
                    partial = false;
                    carry.clear();
                    carry.extend_from_slice(&chunk[at + 1..]);
                } else {
                    carry.extend_from_slice(chunk);
                }
                // The carried piece is never longer than the line cap: a
                // longer line cannot be counted, so its bytes are dropped up
                // to its newline.
                if carry.len() > MAX_LINE_BYTES + 1 {
                    carry.clear();
                    overlong = true;
                }
                // The runtime is yielded after every chunk, so a writer
                // that keeps the pipe full cannot hold it.
                tokio::task::yield_now().await;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(_) => return,
        }
    }
    // Whatever `carry` holds has no newline: an unfinished line, never
    // counted.
}

/// The server's process group: the server is its leader (`process_group(0)`
/// at the spawn), so the group's id is the server's pid.
mod group {
    use std::time::Duration;

    use super::GroupEnd;

    /// Whether the server `pid` has exited, without reaping it
    /// (`waitid` with `WNOWAIT`): the zombie keeps its pid, and so the
    /// group id, reserved.
    pub(super) fn exited_unreaped(pid: libc::pid_t) -> bool {
        let Ok(id) = libc::id_t::try_from(pid) else {
            return false;
        };
        // SAFETY: a zeroed siginfo_t is a valid out-parameter for waitid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waitid writes only into `info`; WNOWAIT leaves the child
        // waitable, so the child handle still reaps it.
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // SAFETY: `si_pid` reads the field waitid filled in.
        r == 0 && unsafe { info.si_pid() } == pid
    }

    /// SIGKILL to the group whose id is `pid`. Called only while the
    /// server `pid` is unreaped, so the group id is still the server's.
    pub(super) fn kill(pid: libc::pid_t) {
        if pid > 1 {
            // SAFETY: killpg sends a signal; it touches no memory.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }

    /// Looks for the group `pid` to have no process left, up to `within`.
    /// `ESRCH` proves it empty; anything else within the bound is
    /// `NotShownEmpty` (a reused group id reads as not empty, the safe
    /// direction).
    pub(super) async fn wait_empty(pid: libc::pid_t, within: Duration) -> GroupEnd {
        let until = tokio::time::Instant::now() + within;
        loop {
            // SAFETY: the null signal checks existence and sends nothing.
            let r = unsafe { libc::killpg(pid, 0) };
            if r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return GroupEnd::Empty;
            }
            if tokio::time::Instant::now() >= until {
                return GroupEnd::NotShownEmpty;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn not_read() -> Failure {
    failure(
        FailureKind::Transport,
        "the tools/list answer was not read off the wire",
        Ended::NotStarted,
    )
}

/// The fresh, empty, private (0700) working directory of one server
/// start, under the caller's temporary directory; removed with everything
/// in it when it drops (after the server was reaped).
pub(crate) struct WorkDir(PathBuf);

impl WorkDir {
    fn create() -> std::io::Result<WorkDir> {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "apb-mcp-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // `create`, not `create_all`: an existing path is an error, so the
        // directory is always new.
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(WorkDir(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Starts `server` after apb's program-start guard: the inherited
/// environment cleared, exactly `env` and `args`, a fresh private working
/// directory, stdin and stdout piped, stderr null, as the leader of its
/// own process group. The program is the file that was hashed: executed
/// through `/proc/self/fd/<n>` of its descriptor (`StartFrom::Descriptor`),
/// or by its path after the path was hashed again
/// (`StartFrom::PathCheckedAgain`). The child is killed if its handle
/// drops while it runs.
pub(crate) fn spawn(server: StdioServer) -> Result<(Child, WorkDir), Failure> {
    let refused = |kind: FailureKind, message: String| Failure {
        kind,
        message,
        ended: Ended::NotStarted,
    };
    if let Err(e) = apb_core::process_mode::guard_program_start() {
        return Err(refused(
            FailureKind::Refused,
            format!("this process starts no MCP server: {e}"),
        ));
    }
    let name = server
        .program
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the MCP server".into());
    let mut cmd = match server.program.start_from() {
        StartFrom::Descriptor => {
            let fd = server.program.raw_fd();
            let mut cmd = tokio::process::Command::new(format!("/proc/self/fd/{fd}"));
            cmd.arg0(server.program.path());
            // SAFETY: fcntl(2) is async-signal-safe; between fork and exec
            // it clears close-on-exec on the program's descriptor in the
            // child only, so the exec (and an interpreter that opens
            // `/proc/self/fd/<n>` for a script) reads the hashed file.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            cmd
        }
        // The path is hashed again right before the spawn, below.
        StartFrom::PathCheckedAgain => tokio::process::Command::new(server.program.path()),
    };
    // Linux: the server gets SIGKILL when the thread that started it ends
    // (the session's worker, which lives until the server is reaped), so a
    // caller killed mid-call (an attempt stopped) does not leave it
    // running in its own group. Other platforms have no such signal.
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id();
        // SAFETY: prctl(2) and getppid(2) are async-signal-safe and touch
        // only the child between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The parent may have died before the prctl.
                if u32::try_from(libc::getppid()).ok() != Some(parent) {
                    // A raw OS error: nothing between fork and exec
                    // allocates.
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
    let workdir = WorkDir::create().map_err(|e| {
        refused(
            FailureKind::Spawn,
            format!(
                "the private working directory of {name} could not be made: {:?}",
                e.kind()
            ),
        )
    })?;
    cmd.env_clear();
    for (name, value) in &server.env {
        match value {
            EnvValue::Plain(v) => cmd.env(name, v),
            EnvValue::Secret(s) => cmd.env(name, s.expose_secret()),
        };
    }
    cmd.args(&server.args)
        .current_dir(workdir.path())
        .process_group(0)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // Hashed again with nothing but the spawn left to do, so no setup
    // work widens the interval in which the file could be replaced.
    if matches!(server.program.start_from(), StartFrom::PathCheckedAgain)
        && server.program.check_path_again().is_err()
    {
        return Err(refused(
            FailureKind::ProgramChanged,
            format!("{name} no longer holds the approved bytes; it was not started"),
        ));
    }
    let child = cmd.spawn().map_err(|e| {
        refused(
            FailureKind::Spawn,
            format!("{name} could not be started: {:?}", e.kind()),
        )
    })?;
    // The descriptor is no longer needed once the child holds its copy.
    drop(server.program);
    Ok((child, workdir))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::stdio::ProgramFile;
    use crate::tap::MAX_SESSION_BYTES;
    use crate::testing::{StubEra, StubServer};

    fn stub_server(stub: &StubServer) -> StdioServer {
        StdioServer {
            program: ProgramFile::open(&stub.program(), StartFrom::platform())
                .expect("the program opens"),
            args: Vec::new(),
            env: vec![("PATH".into(), EnvValue::Plain("/usr/bin:/bin".into()))],
        }
    }

    fn info() -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("apb-test", "1.0.0"),
        )
    }

    /// The orphan the stub left behind, read right after the session
    /// connected: the stub starts it before it reads its first request and
    /// waits until its pid is recorded, so by the handshake's answer the
    /// orphan is a writer of its own session, out of the server's group,
    /// and the close is timed against a writer that really holds the pipe.
    /// No wait here: a missing pid is a broken stub, never a slow machine.
    #[cfg(target_os = "linux")]
    fn started_orphan(stub: &StubServer) -> u32 {
        stub.orphan_pid()
            .expect("the stub records its orphan before it answers the handshake")
    }

    /// Kills the orphan the stub left behind, so a test leaves no process
    /// behind. The pid is the one the stub recorded; nothing else is
    /// touched.
    #[cfg(target_os = "linux")]
    fn kill_orphan(stub: &StubServer) {
        if let Some(pid) = stub.orphan_pid() {
            let _ = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("kill -9 {pid} 2>/dev/null"))
                .status();
        }
    }

    /// The caller stopped waiting before the handshake's answer reached it
    /// (its reply timeout passed): the worker's abandoned branch closes
    /// the server through `finish`, so it is reaped and its working
    /// directory removed before the worker ends.
    #[test]
    fn an_abandoned_connect_reaps_the_server() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_ignore_eof(true).unwrap();
        let (ready, answer) = std::sync::mpsc::channel();
        drop(answer);
        let (_commands, inbox) = unbounded_channel();
        run_worker(
            Server::Stdio(stub_server(&stub)),
            Lifecycle::Auto,
            info(),
            Instant::now() + Duration::from_secs(10),
            ready,
            inbox,
            LineTap::new(),
        );
        let pid = stub.last_pid().expect("the server started");
        assert!(!StubServer::is_alive(pid), "the server is reaped");
        let cwd = stub.cwd_seen().expect("a working directory");
        assert!(!cwd.exists(), "the working directory is removed");
    }

    /// `finish` keeps waiting for a child the close could not reap
    /// (`late`), so it never stays a zombie, then removes the working
    /// directory: the late reap every cleanup branch runs.
    #[test]
    fn finish_reaps_a_late_child() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let workdir = WorkDir::create().unwrap();
            let path = workdir.path().to_path_buf();
            let mut conn = Conn::new(None, Some(workdir));
            let child = tokio::process::Command::new("sleep")
                .arg("0.3")
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            conn.late = Some(child);
            assert_eq!(conn.finish().await, Ended::NotStarted);
            assert!(!StubServer::is_alive(pid), "the late child is reaped");
            assert!(!path.exists(), "the working directory is removed");
        });
    }

    #[test]
    fn only_a_supported_version_is_kept() {
        assert_eq!(supported_version("2026-07-28"), Some("2026-07-28"));
        assert_eq!(supported_version("2025-06-18"), Some("2025-06-18"));
        for refused in [
            "",
            "2025-11-25 sk-secret",
            "2025-11-25\n",
            "2099-01-01",
            " 2025-11-25",
        ] {
            assert_eq!(supported_version(refused), None, "{refused:?}");
        }
    }

    /// No process this crate spawns while a session lives inherits the
    /// server's stdout: the duplicate descriptor the close drains is
    /// close-on-exec from its creation, so a child started by the same
    /// process holds no read end of the pipe.
    ///
    /// Linux only: the check reads `/proc/self/fd`, which macOS has not.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_spawned_while_a_session_lives_holds_no_server_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.record_stdout_link().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut session = Session::connect(
            Server::Stdio(stub_server(&stub)),
            Lifecycle::Auto,
            "apb-test",
            "1.0.0",
            deadline,
        )
        .expect("the session connects");
        // Both ends of a pipe name the same inode: this is the inode a
        // leaked duplicate would show.
        let link = stub
            .stdout_link()
            .expect("the stub recorded its stdout link");
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("for f in /proc/self/fd/*; do readlink \"$f\"; done")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .expect("the child runs");
        let seen = String::from_utf8_lossy(&child.stdout);
        assert!(
            !seen.contains(&link),
            "a child of this process holds the server's stdout: {seen}"
        );
        assert_eq!(
            session.close(),
            Ended::Reaped {
                killed: false,
                group: GroupEnd::Empty,
            }
        );
    }

    /// The close's drain has its own bound: a process outside the server's
    /// group that keeps the pipe open (no end of output) cannot stretch the
    /// close past it, and what sat in the pipe is still counted.
    ///
    /// Linux only: the stub starts its orphan with `setsid`, which macOS
    /// has not, and a test without its orphan proves nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pipe_that_never_ends_its_output_stretches_no_close() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_eof_log(Some("LOGSECRET-5 late log")).unwrap();
        stub.set_orphan_writer(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut session = Session::connect(
            Server::Stdio(stub_server(&stub)),
            Lifecycle::Auto,
            "apb-test",
            "1.0.0",
            deadline,
        )
        .expect("the session connects");
        let orphan = started_orphan(&stub);
        let started = Instant::now();
        let ended = session.close();
        // The quiet orphan sleeps and writes nothing, so the close's end
        // of the pipe going away does not end it: it still holds the pipe.
        assert!(StubServer::is_alive(orphan), "the orphan held the pipe");
        kill_orphan(&stub);
        assert!(
            matches!(ended, Ended::Reaped { .. }),
            "the close answers a reaped server: {ended:?}"
        );
        assert_eq!(session.dropped_lines(), 1);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the close stayed well under two seconds: {:?}",
            started.elapsed()
        );
    }

    /// A writer outside the server's group that keeps the pipe full with
    /// bytes that carry no newline: the drain reads and reads, and the
    /// close still answers inside its bound.
    ///
    /// Linux only: the stub starts its orphan with `setsid`, which macOS
    /// has not, and a test without its orphan proves nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_writer_that_keeps_the_pipe_full_stretches_no_close() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_orphan_flow(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut session = Session::connect(
            Server::Stdio(stub_server(&stub)),
            Lifecycle::Auto,
            "apb-test",
            "1.0.0",
            deadline,
        )
        .expect("the session connects");
        // Recorded before the close: the writer runs out of the server's
        // group, so the close's group kill cannot take it. Once the close
        // drops its end of the pipe the writer dies of SIGPIPE.
        started_orphan(&stub);
        let started = Instant::now();
        let ended = session.close();
        kill_orphan(&stub);
        assert!(
            matches!(ended, Ended::Reaped { .. }),
            "the close answers a reaped server: {ended:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the close stayed well under two seconds: {:?}",
            started.elapsed()
        );
    }

    /// The server wrote half a line and left it unfinished, then logged
    /// one line at end of input: the drain counts exactly one line, the
    /// log line. The half line the reader held is its continuation, so it
    /// is no dropped line.
    #[test]
    fn a_server_that_dies_mid_line_counts_the_log_line_only() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_call_half_line(Some("{\"jsonrpc\":\"2.0\",\"id\":1"))
            .unwrap();
        // The rest of the line the reader held: one read of the drain is
        // 16 KiB, so the line ends in the first read and the log line
        // comes in the second.
        let mut rest = vec![b'x'; 16 * 1024 - 1];
        rest.push(b'\n');
        stub.set_eof_bytes(&rest).unwrap();
        stub.set_eof_log(Some("LOGSECRET-7 one log line")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut session = Session::connect(
            Server::Stdio(stub_server(&stub)),
            Lifecycle::Auto,
            "apb-test",
            "1.0.0",
            deadline,
        )
        .expect("the session connects");
        // A round trip: the reader has read the half line by then. The
        // pause gives it the moment it needs on a loaded machine.
        session
            .call_tool("t", Map::new(), deadline)
            .expect("an answer");
        std::thread::sleep(Duration::from_millis(200));
        let ended = session.close();
        assert!(
            matches!(ended, Ended::Reaped { .. }),
            "the close answers a reaped server: {ended:?}"
        );
        assert_eq!(
            session.dropped_lines(),
            1,
            "the log line is counted once, the half line not at all"
        );
    }

    /// A log line the server wrote right after its answer, while the
    /// reader was still alive: the reader counted it, not the drain.
    #[test]
    fn a_line_written_after_the_answer_is_counted_by_the_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_call_suffix(b"LOGSECRET-8 after the answer")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut session = Session::connect(
            Server::Stdio(stub_server(&stub)),
            Lifecycle::Auto,
            "apb-test",
            "1.0.0",
            deadline,
        )
        .expect("the session connects");
        session
            .call_tool("t", Map::new(), deadline)
            .expect("an answer");
        // The second round trip: the reader has read the line before the
        // second answer.
        session
            .call_tool("t", Map::new(), deadline)
            .expect("an answer");
        assert_eq!(
            session.dropped_lines(),
            1,
            "the reader counted the line it read"
        );
        session.close();
    }

    /// Runs the close's drain over `bytes` as the pipe's remaining bytes,
    /// on a paused clock so the drain's own bound never cuts the test
    /// short on a loaded machine.
    fn drain_over(bytes: &[u8], tap: &LineTap) {
        use std::io::{Seek as _, Write as _};
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        runtime.block_on(drain_remaining(file, tap));
    }

    /// A tap whose reader read `MAX_SESSION_BYTES - left` bytes of whole
    /// notifications through its own path, then ended.
    fn tap_with_reader_left(left: usize) -> LineTap {
        use tokio::io::AsyncReadExt as _;
        let head = br#"{"jsonrpc":"2.0","method":"n","params":{"p":""#;
        let tail = b"\"}}\n";
        let mut input = Vec::new();
        let mut read = 0usize;
        let target = MAX_SESSION_BYTES - left;
        while read < target {
            let size = (target - read).min(1024 * 1024);
            let mut line = head.to_vec();
            line.resize(size - tail.len(), b'a');
            line.extend_from_slice(tail);
            input.extend_from_slice(&line);
            read += size;
        }
        let tap = LineTap::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut reader = tap.reader(std::io::Cursor::new(input));
            let mut sink = Vec::new();
            reader.read_to_end(&mut sink).await.expect("the lines read");
        });
        tap
    }

    /// An overlong line, its newline and one log line in the same read:
    /// only the overlong line is dropped, and the log line after it is
    /// counted.
    #[test]
    fn the_drain_counts_the_log_line_after_an_overlong_line() {
        let tap = LineTap::new();
        // Long enough that the drain marks it overlong a few reads before
        // its newline arrives.
        let mut bytes = vec![b'x'; MAX_LINE_BYTES + 40_000];
        bytes.extend_from_slice(b"\nLOGSECRET-9 one log line\n");
        drain_over(&bytes, &tap);
        assert_eq!(tap.dropped(), 1, "the log line is counted once");
    }

    /// Every byte the drain reads counts toward the session's limit, the
    /// bytes of a discarded overlong line included: with the reader near
    /// its limit, an overlong line takes the session past it, and the log
    /// line after it is not counted.
    #[test]
    fn the_drains_overlong_bytes_count_toward_the_session_byte_limit() {
        // Long enough that the drain marks it overlong a few reads before
        // its newline arrives.
        let mut bytes = vec![b'x'; MAX_LINE_BYTES + 40_000];
        bytes.extend_from_slice(b"\nLOGSECRET-10 one log line\n");
        // The limit is passed only by the last read, the one that carries
        // the overlong line's newline and the log line: the overlong
        // line's bytes are what take the session past it.
        let tap = tap_with_reader_left(bytes.len() - 5);
        drain_over(&bytes, &tap);
        assert_eq!(tap.dropped(), 0, "nothing past the limit is counted");
        // Room for exactly what the drain reads: the log line counts, so the
        // limit, not the overlong line, is what stopped the count.
        let tap = tap_with_reader_left(bytes.len());
        drain_over(&bytes, &tap);
        assert_eq!(tap.dropped(), 1, "with room left the log line counts");
    }
}
