//! The line tap between the server's pipes and rmcp's transport.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::frame::{self, Incoming, Outgoing};

/// The longest line the client accepts from a server, its own limit next
/// to the engine's body cap (`BODY_CAP`, 1 MiB): seven times the body cap
/// plus 64 KiB for the JSON-RPC envelope, 7 405 568 bytes, so the worst
/// escaping a JSON encoder writes of a body at the cap fits.
pub const MAX_LINE_BYTES: usize = 7 * 1024 * 1024 + 64 * 1024;

/// The most lines one session reads from a server.
pub(crate) const MAX_LINES: usize = 10_000;

/// The most bytes one session reads from a server, over all its lines:
/// four of the longest lines (a session reads a handshake, at most 2 MiB
/// of listing pages and one answer).
pub const MAX_SESSION_BYTES: usize = 4 * MAX_LINE_BYTES;

#[derive(Default)]
struct TapState {
    /// The ids of the `tools/list` requests written so far.
    listing: HashSet<String>,
    /// The id key of the latest `tools/call` request and, once the first
    /// frame answering it was read, whether that frame was no call result
    /// (`frame::is_untyped_call_answer`); rmcp takes that first frame.
    call: Option<(String, Option<bool>)>,
    /// The listing ids whose answer was not read yet: a line is parsed
    /// only while one is awaited.
    awaiting: HashSet<String>,
    /// The id of the latest `tools/list` request.
    last_listing: Option<String>,
    /// The raw `result` of each listing answer read, with the byte length
    /// of its line: only from a frame that is a whole successful response.
    results: HashMap<String, (Value, usize)>,
    /// How many frames that rmcp would not take as a server request
    /// carried each listing id still watched (awaited, or captured and not
    /// yet taken).
    frames: HashMap<String, usize>,
    /// The listing ids two frames answered: the listing fails closed.
    ambiguous: HashSet<String>,
    /// The message of the read that failed for a line limit.
    too_large: Option<String>,
    /// The named reason the writer refused a line it could not classify;
    /// nothing of that line was forwarded.
    refused: Option<String>,
    /// The `mcp_message_invalid:` message of a line from the server the
    /// reader could not read completely; nothing of it reached rmcp.
    invalid: Option<String>,
    /// Whole lines from the server that did not start a JSON object and
    /// were dropped unread; a count only.
    dropped: usize,
    /// Whether the reader ended with a line still without its newline: the
    /// close's drain starts counting after that line's newline.
    partial_line: bool,
    /// The bytes the reader read before it was dropped: the session's own
    /// count, which the close's drain continues from.
    reader_bytes: usize,
    /// The bytes the close's drain read from the pipe, toward
    /// `MAX_SESSION_BYTES`: the last bytes of a session are accounted too.
    drained_bytes: usize,
    /// The frames an HTTP session read (`read_frame`), toward `MAX_LINES`:
    /// an HTTP session has no `TapReader` whose own counter holds them.
    http_frames: usize,
    /// The answer bytes an HTTP session read (`read_body`), toward
    /// `MAX_SESSION_BYTES`.
    http_bytes: usize,
}

/// The gate of every write to the server's stdin, under its own lock (held
/// across one non-blocking pipe write, so nothing else takes it there).
#[derive(Default)]
struct WriteGate {
    /// How many `tools/call` request lines had at least one byte accepted
    /// by the server's stdin.
    calls_handed: usize,
    /// Set by `seal`: the writer forwards no byte after it.
    sealed: bool,
}

/// The state the reader and the writer of one session share: the ids of
/// the `tools/list` requests, the raw listing answers, and the flag of a
/// line limit.
#[derive(Clone, Default)]
pub(crate) struct LineTap {
    state: Arc<Mutex<TapState>>,
    gate: Arc<Mutex<WriteGate>>,
}

impl LineTap {
    pub(crate) fn new() -> LineTap {
        LineTap::default()
    }

    /// The bounded line reader over the server's stdout.
    pub(crate) fn reader<R>(&self, inner: R) -> TapReader<R> {
        TapReader {
            inner,
            tap: self.clone(),
            line: Vec::new(),
            ready: Vec::new(),
            ready_pos: 0,
            scratch: Vec::new(),
            lines: 0,
            bytes: 0,
            failed: None,
        }
    }

    /// The writer over the server's stdin that registers a listing request
    /// before any byte of it is forwarded.
    pub(crate) fn writer<W>(&self, inner: W) -> TapWriter<W> {
        TapWriter {
            inner,
            tap: self.clone(),
            line: Vec::new(),
            out: Vec::new(),
            out_pos: 0,
            call_starts: std::collections::VecDeque::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_registered(&self, id: &Value) -> bool {
        self.state
            .lock()
            .map(|s| s.listing.contains(&frame::id_key(id)))
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn registered(&self) -> usize {
        self.state.lock().map(|s| s.listing.len()).unwrap_or(0)
    }

    /// How many `tools/call` request lines had at least one byte accepted
    /// by the server's stdin so far.
    pub(crate) fn calls_handed(&self) -> usize {
        self.gate
            .lock()
            .map(|s| s.calls_handed)
            .unwrap_or(usize::MAX)
    }

    /// Stops every further write to the server's stdin and answers how
    /// many `tools/call` lines had a byte accepted by then (`usize::MAX`
    /// when the state is poisoned, the unknown direction).
    ///
    /// Race-free against rmcp's writer task: the tap writer is the only
    /// holder of the child's stdin, and it forwards bytes only inside
    /// `forward`, which checks `sealed`, writes and counts under the same
    /// lock (`WriteGate`) `seal` takes. So once `seal` returns, either a write that
    /// started before it is fully counted in the answer, or no byte is
    /// ever written again, whatever rmcp still holds in its queue or hands
    /// the writer later.
    pub(crate) fn seal(&self) -> usize {
        match self.gate.lock() {
            Ok(mut s) => {
                s.sealed = true;
                s.calls_handed
            }
            Err(_) => usize::MAX,
        }
    }

    /// One write to the server's stdin, under the state lock: refused once
    /// sealed; the `tools/call` lines it hands over (any byte of them
    /// accepted) are counted before the lock is released.
    fn forward<W: AsyncWrite + Unpin>(
        &self,
        inner: Pin<&mut W>,
        cx: &mut Context<'_>,
        buf: &[u8],
        out_pos: usize,
        call_starts: &mut std::collections::VecDeque<usize>,
    ) -> Poll<std::io::Result<usize>> {
        let Ok(mut s) = self.gate.lock() else {
            return Poll::Ready(Err(std::io::Error::other("the MCP line tap is poisoned")));
        };
        if s.sealed {
            return Poll::Ready(Err(sealed_error()));
        }
        let n = ready!(inner.poll_write(cx, buf))?;
        let reached = out_pos + n;
        // A `tools/call` line is handed over once the pipe took any byte
        // of it.
        while call_starts.front().is_some_and(|at| *at < reached) {
            call_starts.pop_front();
            s.calls_handed = s.calls_handed.saturating_add(1);
        }
        Poll::Ready(Ok(n))
    }

    pub(crate) fn is_sealed(&self) -> bool {
        self.gate.lock().map_or(true, |s| s.sealed)
    }

    /// The `mcp_message_too_large:` message once a line limit ended the
    /// reads.
    pub(crate) fn too_large(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.too_large.clone())
    }

    /// The named reason the writer refused a line, once it did.
    pub(crate) fn refused(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.refused.clone())
    }

    /// The raw `result` of the latest `tools/list` request and the byte
    /// length of its line, when the reader saw that answer; `Err` with
    /// `LISTING_AMBIGUOUS` when two frames answered that id.
    pub(crate) fn take_last_listing(&self) -> Result<Option<(Value, usize)>, String> {
        let Ok(mut s) = self.state.lock() else {
            return Ok(None);
        };
        let Some(id) = s.last_listing.clone() else {
            return Ok(None);
        };
        if s.ambiguous.contains(&id) {
            return Err(LISTING_AMBIGUOUS.to_string());
        }
        s.frames.remove(&id);
        s.awaiting.remove(&id);
        Ok(s.results.remove(&id))
    }

    /// Whether rmcp's answer to the latest `tools/call` may not be taken
    /// as a call result: the first frame answering it was no call result
    /// (`frame::is_untyped_call_answer`), or the tap cannot vouch for it,
    /// failing closed like every reader of its state: the state is
    /// poisoned, or no frame answering that call was noted (rmcp reads
    /// only frames the tap read first, on both transports, so its answer
    /// then came from somewhere the tap did not see). Only a noted call
    /// result answers `false`.
    pub(crate) fn call_answer_untyped(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |s| !matches!(s.call, Some((_, Some(false)))))
    }

    /// Classifies one whole line the client is about to write (design D3
    /// "The line tap"): registers a `tools/list` request, answers whether
    /// it is a `tools/call`, and refuses a line it cannot classify, so
    /// nothing reaches the server unclassified.
    fn classify_line(&self, line: &[u8]) -> std::io::Result<bool> {
        match frame::classify(line) {
            Ok(Outgoing::Call(key)) => {
                // A call whose answer the tap cannot note is never handed
                // over (`call_answer_untyped` needs that note).
                let noted = match self.state.lock() {
                    Ok(mut s) => {
                        s.call = key.map(|key| (key, None));
                        true
                    }
                    Err(_) => false,
                };
                if !noted {
                    return Err(self.refuse(TAP_POISONED));
                }
                Ok(true)
            }
            Ok(Outgoing::Other) => Ok(false),
            Ok(Outgoing::Listing(key)) => {
                if let Ok(mut s) = self.state.lock() {
                    s.listing.insert(key.clone());
                    s.awaiting.insert(key.clone());
                    s.last_listing = Some(key);
                }
                Ok(false)
            }
            Err(reason) => Err(self.refuse(reason)),
        }
    }

    /// Refuses a line before any byte of it is forwarded: keeps the
    /// reason and seals the writer, so nothing after it goes out either.
    fn refuse(&self, reason: &str) -> std::io::Error {
        if let Ok(mut s) = self.state.lock() {
            s.refused.get_or_insert_with(|| reason.to_string());
        }
        self.seal();
        std::io::Error::other(reason.to_string())
    }

    /// Reads one whole line from the server by the strict rule
    /// (`frame::incoming`) before any byte of it is passed on; `Err` with
    /// the `mcp_message_invalid:` message for a line it cannot read
    /// completely, which is then never passed on. Keeps the raw `result` of
    /// an awaited listing answer, only from a whole successful response; a
    /// second frame for a watched id makes that listing fail closed. rmcp
    /// reads only lines this accepted, byte for byte, so both read the same
    /// frames, and when rmcp's listing returns every frame up to the one it
    /// took was counted here.
    fn observe(&self, line: &[u8]) -> Result<(), String> {
        let read = frame::incoming(line).map_err(|why| {
            format!(
                "mcp_message_invalid: the MCP server sent a line apb could not read as exactly one JSON-RPC 2.0 message ({why}); nothing of it was passed on and the session ends"
            )
        })?;
        let (key, result) = match read {
            Incoming::Unrelated => return Ok(()),
            Incoming::Other { id } => (id, None),
            Incoming::Response { id, result } => (id, Some(result)),
        };
        let Ok(mut s) = self.state.lock() else {
            return Ok(());
        };
        if let Some((id, first @ None)) = s.call.as_mut()
            && *id == key
        {
            *first = Some(frame::is_untyped_call_answer(line));
        }
        if !s.awaiting.contains(&key) && !s.results.contains_key(&key) {
            return Ok(());
        }
        let seen = s.frames.entry(key.clone()).or_insert(0);
        *seen += 1;
        if *seen > 1 {
            s.results.remove(&key);
            s.ambiguous.insert(key);
            return Ok(());
        }
        if let Some(result) = result {
            s.awaiting.remove(&key);
            s.results.insert(key, (result, line.len()));
        }
        Ok(())
    }

    /// Classifies one whole message an HTTP session is about to post, as
    /// the writer classifies a line (`classify_line`): a `tools/list` id
    /// is registered before the request is built, and a message the tap
    /// cannot classify is refused and seals the gate. `Ok(true)` for a
    /// `tools/call`.
    pub(crate) fn classify_message(&self, bytes: &[u8]) -> std::io::Result<bool> {
        self.classify_line(bytes)
    }

    /// Counts a `tools/call` POST as handed over, under the gate's lock,
    /// before its connection is opened: `false` (and nothing counted) once
    /// the gate is sealed, so a sealed session starts no POST.
    pub(crate) fn start_call(&self) -> bool {
        let Ok(mut g) = self.gate.lock() else {
            return false;
        };
        if g.sealed {
            return false;
        }
        g.calls_handed = g.calls_handed.saturating_add(1);
        true
    }

    /// Reads one frame of an HTTP answer (a JSON body, or the joined data
    /// of one event) by the strict rule (`observe`), after counting it
    /// toward `MAX_LINES`. `Err` with the message of the limit (also kept
    /// as `too_large`) or of the refusal (kept as `invalid`); nothing of a
    /// refused frame is handed on.
    pub(crate) fn read_frame(&self, bytes: &[u8]) -> Result<(), String> {
        let frames = match self.state.lock() {
            Ok(mut s) => {
                s.http_frames = s.http_frames.saturating_add(1);
                s.http_frames
            }
            Err(_) => usize::MAX,
        };
        if frames > MAX_LINES {
            return Err(self
                .fail(format!(
                    "mcp_message_too_large: the server sent more than {MAX_LINES} messages"
                ))
                .to_string());
        }
        self.observe(bytes)
            .map_err(|message| self.refuse_incoming(message).to_string())
    }

    /// Counts `len` bytes of an HTTP answer toward `MAX_SESSION_BYTES`;
    /// `Err` (kept as `too_large`) once the session passes it.
    pub(crate) fn read_body(&self, len: usize) -> Result<(), String> {
        let bytes = match self.state.lock() {
            Ok(mut s) => {
                s.http_bytes = s.http_bytes.saturating_add(len);
                s.http_bytes
            }
            Err(_) => usize::MAX,
        };
        if bytes > MAX_SESSION_BYTES {
            return Err(self
                .fail(format!(
                    "mcp_message_too_large: the server sent more than {MAX_SESSION_BYTES} bytes"
                ))
                .to_string());
        }
        Ok(())
    }

    /// How many lines the reader dropped unread (`DROP` rule).
    pub(crate) fn dropped(&self) -> usize {
        self.state.lock().map_or(0, |s| s.dropped)
    }

    /// Counts one dropped line; `false` once the count passes
    /// `MAX_DROPPED_LINES`.
    pub(crate) fn drop_line(&self) -> bool {
        self.state.lock().is_ok_and(|mut s| {
            s.dropped = s.dropped.saturating_add(1);
            s.dropped <= MAX_DROPPED_LINES
        })
    }

    /// Whether the reader ended with a line still without its newline.
    pub(crate) fn partial_line(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.partial_line)
    }

    /// Counts `n` bytes the close's drain read from the pipe toward
    /// `MAX_SESSION_BYTES`, together with the bytes the reader read: every
    /// read, before its lines are classified, the bytes of a dropped
    /// overlong line included. Answers whether the session is still within
    /// the limit; past it the drain counts no line any more.
    pub(crate) fn count_drained(&self, n: usize) -> bool {
        let Ok(mut s) = self.state.lock() else {
            return false;
        };
        s.drained_bytes = s.drained_bytes.saturating_add(n);
        s.reader_bytes.saturating_add(s.drained_bytes) <= MAX_SESSION_BYTES
    }

    /// Classifies what the close's drain read from the pipe, by the reader's
    /// own rule: with `partial_prefix` (the reader ended with a line still
    /// without its newline) counting starts after the drained bytes' first
    /// newline, because those first bytes are that line's continuation and
    /// never a dropped line; a last piece without a newline is an
    /// unfinished line, which the reader refuses and never counts; a whole
    /// line that does not open a JSON object is a dropped line
    /// (`drop_line`); a whole line over `MAX_LINE_BYTES` is what the reader
    /// refuses as too large, not what it drops, and is not counted; and
    /// the bytes were counted toward `MAX_SESSION_BYTES` by
    /// `count_drained` as they were read.
    pub(crate) fn drain_bytes(&self, bytes: &[u8], partial_prefix: bool) {
        let mut rest = bytes;
        if partial_prefix {
            let Some(at) = rest.iter().position(|b| *b == b'\n') else {
                return;
            };
            rest = &rest[at + 1..];
        }
        for line in rest.split_inclusive(|b| *b == b'\n') {
            let Some(whole) = line.strip_suffix(b"\n") else {
                break;
            };
            if whole.len() > MAX_LINE_BYTES {
                continue;
            }
            let starts_json = whole.starts_with(frame::UTF8_BOM) || whole.first() == Some(&b'{');
            if !starts_json {
                self.drop_line();
            }
        }
    }

    /// The `mcp_message_invalid:` message once the reader refused a line.
    pub(crate) fn invalid(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.invalid.clone())
    }

    fn refuse_incoming(&self, message: String) -> std::io::Error {
        if let Ok(mut s) = self.state.lock() {
            s.invalid.get_or_insert_with(|| message.clone());
        }
        std::io::Error::other(message)
    }

    fn fail(&self, message: String) -> std::io::Error {
        if let Ok(mut s) = self.state.lock() {
            s.too_large.get_or_insert_with(|| message.clone());
        }
        std::io::Error::other(message)
    }
}

/// The failure of a listing two frames answered.
pub(crate) const LISTING_AMBIGUOUS: &str = "mcp_listing_ambiguous: the MCP server sent more than one answer for one tools/list request; the listing is not used";

/// The refusal of a request line past the line limit; nothing of it is
/// sent.
pub(crate) const REQUEST_TOO_LARGE: &str = "mcp_request_too_large: the message apb was about to send to the MCP server passed the line limit, so nothing of it was sent";

/// The refusal of a `tools/call` the tap could not note because its state
/// is poisoned; nothing of it is sent.
pub(crate) const TAP_POISONED: &str = "mcp_tap_poisoned: apb could not note the tools/call it was about to send to the MCP server, so nothing of it was sent";

fn sealed_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "the MCP session stopped writing to the server",
    )
}

/// The server's stdout as rmcp reads it: only whole lines the tap read
/// completely (`LineTap::observe`), byte for byte, one line at most
/// `MAX_LINE_BYTES` long and at most `MAX_LINES` lines a session. A line
/// still without its newline is held back; a line the tap refuses ends the
/// reads with an error, and nothing of it, or after it, reaches rmcp.
pub(crate) struct TapReader<R> {
    inner: R,
    tap: LineTap,
    /// The current line, up to its newline.
    line: Vec<u8>,
    /// Whole accepted lines, with their newlines, not yet handed to rmcp,
    /// from `ready_pos`.
    ready: Vec<u8>,
    ready_pos: usize,
    /// What one read from the pipe takes in.
    scratch: Vec<u8>,
    lines: usize,
    bytes: usize,
    failed: Option<String>,
}

/// The size of one read from the server's stdout.
const READ_CHUNK: usize = 64 * 1024;

/// The most lines a session drops unread (lines that do not start a JSON
/// object: log text, blank lines) before the reader ends the session.
pub(crate) const MAX_DROPPED_LINES: usize = 1000;

/// The refusal past `MAX_DROPPED_LINES`; it carries no line text.
const TOO_MANY_DROPPED: &str = "mcp_message_invalid: the MCP server wrote more than 1000 lines that are not JSON-RPC messages; the session ends";

/// The refusal of a stream that ended inside a line.
const UNFINISHED: &str = "mcp_message_invalid: the MCP server's output ended inside a line; nothing of that line was passed on";

impl<R> TapReader<R> {
    /// Takes in `bytes` the reader just read; `Err` with the message of
    /// the limit they pass or of the line the tap refused.
    fn take_in(&mut self, mut bytes: &[u8]) -> Result<(), std::io::Error> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > MAX_SESSION_BYTES {
            return Err(self.tap.fail(format!(
                "mcp_message_too_large: the server sent more than {MAX_SESSION_BYTES} bytes"
            )));
        }
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|b| *b == b'\n');
            let piece = &bytes[..newline.unwrap_or(bytes.len())];
            if self.line.len() + piece.len() > MAX_LINE_BYTES {
                return Err(self.tap.fail(format!(
                    "mcp_message_too_large: a line from the server passed {MAX_LINE_BYTES} bytes"
                )));
            }
            self.line.extend_from_slice(piece);
            let Some(at) = newline else {
                break;
            };
            self.lines += 1;
            if self.lines > MAX_LINES {
                return Err(self.tap.fail(format!(
                    "mcp_message_too_large: the server sent more than {MAX_LINES} messages"
                )));
            }
            bytes = &bytes[at + 1..];
            // The drop rule (fix round 4): a whole line whose first byte
            // does not open a JSON object is log text or a blank line.
            // Neither parser reads it: it is not parsed, not passed on and
            // only counted. A byte order mark is checked first and refused
            // by `observe`, never dropped.
            if !self.line.starts_with(frame::UTF8_BOM) && self.line.first() != Some(&b'{') {
                self.line.clear();
                if !self.tap.drop_line() {
                    return Err(self.tap.refuse_incoming(TOO_MANY_DROPPED.to_string()));
                }
                continue;
            }
            if let Err(message) = self.tap.observe(&self.line) {
                return Err(self.tap.refuse_incoming(message));
            }
            self.ready.extend_from_slice(&self.line);
            self.ready.push(b'\n');
            self.line.clear();
        }
        Ok(())
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for TapReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        loop {
            if let Some(message) = &this.failed {
                return Poll::Ready(Err(std::io::Error::other(message.clone())));
            }
            if this.ready_pos < this.ready.len() {
                let n = buf.remaining().min(this.ready.len() - this.ready_pos);
                buf.put_slice(&this.ready[this.ready_pos..this.ready_pos + n]);
                this.ready_pos += n;
                if this.ready_pos == this.ready.len() {
                    this.ready.clear();
                    this.ready_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }
            if this.scratch.len() != READ_CHUNK {
                this.scratch.resize(READ_CHUNK, 0);
            }
            let mut chunk = ReadBuf::new(&mut this.scratch);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut chunk))?;
            let n = chunk.filled().len();
            if n == 0 {
                // End of output: a line without its newline is never
                // passed on.
                if this.line.is_empty() {
                    return Poll::Ready(Ok(()));
                }
                this.failed = Some(UNFINISHED.to_string());
                return Poll::Ready(Err(this.tap.refuse_incoming(UNFINISHED.to_string())));
            }
            let read = std::mem::take(&mut this.scratch);
            let taken = this.take_in(&read[..n]);
            this.scratch = read;
            if let Err(e) = taken {
                // Nothing of the line refused, or of what came with it,
                // reaches rmcp.
                this.ready.clear();
                this.ready_pos = 0;
                this.failed = Some(e.to_string());
                return Poll::Ready(Err(e));
            }
        }
    }
}

/// The reader ends only with the service: what it still held of a line
/// without its newline is recorded on the tap, so the close's drain knows
/// where counting starts (nothing of that line is a dropped line), and so
/// is the total it read, which the drain's own bytes count with.
impl<R> Drop for TapReader<R> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.tap.state.lock() {
            s.partial_line = !self.line.is_empty();
            s.reader_bytes = s.reader_bytes.saturating_add(self.bytes);
        }
    }
}

/// Holds the bytes rmcp writes until their line is whole, registers a
/// `tools/list` request, and only then forwards the line to the server.
pub(crate) struct TapWriter<W> {
    inner: W,
    tap: LineTap,
    /// The bytes of the line not yet whole.
    line: Vec<u8>,
    /// Whole lines not yet forwarded, from `out_pos`.
    out: Vec<u8>,
    out_pos: usize,
    /// Where each `tools/call` line not yet handed over starts in `out`.
    call_starts: std::collections::VecDeque<usize>,
}

impl<W: AsyncWrite + Unpin> TapWriter<W> {
    /// Forwards every whole line still pending.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while self.out_pos < self.out.len() {
            let this = &mut *self;
            let n = ready!(this.tap.forward(
                Pin::new(&mut this.inner),
                cx,
                &this.out[this.out_pos..],
                this.out_pos,
                &mut this.call_starts,
            ))?;
            if n == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            self.out_pos += n;
        }
        self.out.clear();
        self.out_pos = 0;
        Poll::Ready(Ok(()))
    }

    fn take_in(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|b| *b == b'\n');
            let piece = &bytes[..newline.map_or(bytes.len(), |at| at + 1)];
            if self.line.len() + piece.len() > MAX_LINE_BYTES + 1 {
                self.line.clear();
                return Err(self.tap.refuse(REQUEST_TOO_LARGE));
            }
            self.line.extend_from_slice(piece);
            bytes = &bytes[piece.len()..];
            if newline.is_some() {
                let body = &self.line[..self.line.len() - 1];
                let is_call = match self.tap.classify_line(body) {
                    Ok(is_call) => is_call,
                    Err(e) => {
                        self.line.clear();
                        return Err(e);
                    }
                };
                if is_call {
                    self.call_starts.push_back(self.out.len());
                }
                self.out.extend_from_slice(&self.line);
                self.line.clear();
            }
        }
        Ok(())
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for TapWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Earlier lines go out first, so the buffer stays one request.
        ready!(self.poll_drain(cx))?;
        self.take_in(buf)?;
        if let Poll::Ready(Err(e)) = self.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        ready!(self.poll_drain(cx))?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // A sealed writer forwards nothing it still holds; the shutdown
        // only closes the pipe.
        if !self.tap.is_sealed() {
            ready!(self.poll_drain(cx))?;
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    /// A sink that keeps what it receives, takes at most `max` bytes per
    /// write, and on its first byte records whether `id` was registered.
    struct Sink {
        got: Arc<Mutex<Vec<u8>>>,
        max: usize,
        tap: LineTap,
        id: Value,
        first_saw_registered: Arc<Mutex<Option<bool>>>,
    }

    impl AsyncWrite for Sink {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let mut first = self.first_saw_registered.lock().unwrap();
            if first.is_none() && !buf.is_empty() {
                *first = Some(self.tap.is_registered(&self.id));
            }
            let n = buf.len().min(self.max);
            self.got.lock().unwrap().extend_from_slice(&buf[..n]);
            Poll::Ready(Ok(n))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct Rig {
        tap: LineTap,
        got: Arc<Mutex<Vec<u8>>>,
        first: Arc<Mutex<Option<bool>>>,
        writer: TapWriter<Sink>,
    }

    fn rig(id: Value, max: usize) -> Rig {
        let tap = LineTap::new();
        let got = Arc::new(Mutex::new(Vec::new()));
        let first = Arc::new(Mutex::new(None));
        let writer = tap.writer(Sink {
            got: got.clone(),
            max,
            tap: tap.clone(),
            id,
            first_saw_registered: first.clone(),
        });
        Rig {
            tap,
            got,
            first,
            writer,
        }
    }

    fn listing(id: u64) -> String {
        format!(
            "{}\n",
            json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} })
        )
    }

    #[tokio::test]
    async fn the_id_is_registered_before_any_byte_is_forwarded() {
        let mut r = rig(json!(7), usize::MAX);
        let line = listing(7);
        r.writer.write_all(line.as_bytes()).await.unwrap();
        r.writer.flush().await.unwrap();
        assert_eq!(*r.first.lock().unwrap(), Some(true));
        assert_eq!(r.got.lock().unwrap().as_slice(), line.as_bytes());
        assert!(r.tap.is_registered(&json!(7)));
    }

    #[tokio::test]
    async fn a_split_request_is_registered_once_whole() {
        let mut r = rig(json!(3), usize::MAX);
        let line = listing(3);
        let bytes = line.as_bytes();
        let (a, rest) = bytes.split_at(10);
        let (b, c) = rest.split_at(15);
        r.writer.write_all(a).await.unwrap();
        r.writer.flush().await.unwrap();
        assert!(
            r.got.lock().unwrap().is_empty(),
            "nothing after the first piece"
        );
        r.writer.write_all(b).await.unwrap();
        r.writer.flush().await.unwrap();
        assert!(
            r.got.lock().unwrap().is_empty(),
            "nothing after the second piece"
        );
        r.writer.write_all(c).await.unwrap();
        r.writer.flush().await.unwrap();
        assert_eq!(r.got.lock().unwrap().as_slice(), bytes);
        assert_eq!(*r.first.lock().unwrap(), Some(true));
        assert_eq!(r.tap.registered(), 1);
    }

    #[tokio::test]
    async fn a_non_listing_request_is_forwarded_unregistered() {
        let mut r = rig(json!(4), usize::MAX);
        let line = format!(
            "{}\n",
            json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "t" } })
        );
        r.writer.write_all(line.as_bytes()).await.unwrap();
        r.writer.flush().await.unwrap();
        assert_eq!(r.got.lock().unwrap().as_slice(), line.as_bytes());
        assert_eq!(r.tap.registered(), 0);
        assert_eq!(*r.first.lock().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn flush_forwards_pending_lines() {
        // A pipe that takes three bytes per write: the writer loops over
        // partial writes, and a flush leaves nothing pending.
        let mut r = rig(json!(1), 3);
        let first = listing(1);
        let second = listing(2);
        r.writer.write_all(first.as_bytes()).await.unwrap();
        r.writer.write_all(second.as_bytes()).await.unwrap();
        r.writer.write_all(b"{\"partial\"").await.unwrap();
        r.writer.flush().await.unwrap();
        let want = format!("{first}{second}");
        assert_eq!(r.got.lock().unwrap().as_slice(), want.as_bytes());
        assert_eq!(r.tap.registered(), 2);
    }

    fn call_line(id: u64) -> String {
        format!(
            "{}\n",
            json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "t" } })
        )
    }

    #[tokio::test]
    async fn a_call_line_the_pipe_took_is_handed_over() {
        let mut r = rig(json!(9), 3);
        assert_eq!(r.tap.calls_handed(), 0);
        r.writer.write_all(call_line(9).as_bytes()).await.unwrap();
        r.writer.flush().await.unwrap();
        assert_eq!(r.tap.calls_handed(), 1);
    }

    /// A pipe whose reader is gone: every write fails.
    struct Broken;

    impl AsyncWrite for Broken {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_call_line_the_pipe_refused_is_not_handed_over() {
        let tap = LineTap::new();
        let mut writer = tap.writer(Broken);
        assert!(writer.write_all(call_line(2).as_bytes()).await.is_err());
        assert_eq!(tap.calls_handed(), 0);
    }

    /// A pipe that takes nothing while `held`, else at most `max` bytes
    /// in one write after which it is held again; keeps what it took.
    struct Held {
        held: Arc<Mutex<bool>>,
        max: usize,
        got: Arc<Mutex<Vec<u8>>>,
    }

    impl AsyncWrite for Held {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let mut held = self.held.lock().unwrap();
            if *held {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            *held = true;
            let n = buf.len().min(self.max);
            self.got.lock().unwrap().extend_from_slice(&buf[..n]);
            Poll::Ready(Ok(n))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A held writer and what a test reads of it.
    struct HeldRig {
        tap: LineTap,
        writer: TapWriter<Held>,
        hold: Arc<Mutex<bool>>,
        got: Arc<Mutex<Vec<u8>>>,
    }

    fn held(max: usize) -> HeldRig {
        let tap = LineTap::new();
        let hold = Arc::new(Mutex::new(true));
        let got = Arc::new(Mutex::new(Vec::new()));
        let writer = tap.writer(Held {
            held: hold.clone(),
            max,
            got: got.clone(),
        });
        HeldRig {
            tap,
            writer,
            hold,
            got,
        }
    }

    /// One `poll_write` of `bytes`, as rmcp's writer task makes it.
    fn write_once(writer: &mut TapWriter<Held>, bytes: &[u8]) -> Poll<std::io::Result<usize>> {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        Pin::new(writer).poll_write(&mut cx, bytes)
    }

    /// A request queued behind a held pipe when the session gives up (the
    /// deadline passed): the seal proves no byte of it went out, and
    /// nothing of it, or of a request rmcp hands the writer later, is ever
    /// written once the pipe frees up.
    #[tokio::test]
    async fn a_sealed_writer_never_sends_a_queued_call() {
        let HeldRig {
            tap,
            mut writer,
            hold,
            got,
        } = held(usize::MAX);
        assert!(matches!(
            write_once(&mut writer, call_line(5).as_bytes()),
            Poll::Ready(Ok(_))
        ));
        assert!(got.lock().unwrap().is_empty(), "the pipe is held");
        assert_eq!(tap.seal(), 0, "no byte of the call was handed over");
        *hold.lock().unwrap() = false;
        assert!(writer.flush().await.is_err(), "a sealed writer refuses");
        *hold.lock().unwrap() = false;
        assert!(writer.write_all(call_line(6).as_bytes()).await.is_err());
        assert!(writer.shutdown().await.is_ok(), "the pipe still closes");
        assert!(got.lock().unwrap().is_empty(), "no tools/call went out");
        assert_eq!(tap.calls_handed(), 0);
    }

    /// A call the pipe took part of before the seal counts as handed over:
    /// the session cannot prove it unwritten, so the outcome is unknown.
    #[tokio::test]
    async fn a_call_partly_written_before_the_seal_is_handed_over() {
        let HeldRig {
            tap,
            mut writer,
            hold,
            got,
        } = held(4);
        *hold.lock().unwrap() = false;
        assert!(matches!(
            write_once(&mut writer, call_line(7).as_bytes()),
            Poll::Ready(Ok(_))
        ));
        let taken = got.lock().unwrap().len();
        assert!(taken > 0 && taken < call_line(7).len(), "{taken}");
        assert_eq!(tap.seal(), 1, "the call was handed over");
        assert!(writer.flush().await.is_err());
        assert_eq!(got.lock().unwrap().len(), taken, "nothing more went out");
    }

    fn deep_call_line(levels: usize) -> String {
        let mut v = json!(0);
        for _ in 0..levels {
            v = json!([v]);
        }
        format!(
            "{}\n",
            json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/call",
                    "params": { "name": "t", "arguments": { "x": v } } })
        )
    }

    /// The review's line: arguments of 126 nested arrays put the request
    /// past the parser's limit. The writer refuses it before any byte is
    /// forwarded, names why, and seals, so no call after it goes out and
    /// the hand-over count never moves: the seal's `NotSent` is true.
    #[tokio::test]
    async fn a_line_the_tap_cannot_classify_is_never_forwarded() {
        let mut r = rig(json!(8), usize::MAX);
        let err = r
            .writer
            .write_all(deep_call_line(126).as_bytes())
            .await
            .expect_err("refused");
        assert!(
            err.to_string().starts_with("mcp_request_unclassified:"),
            "{err}"
        );
        assert!(r.got.lock().unwrap().is_empty(), "nothing was forwarded");
        assert!(
            r.tap
                .refused()
                .is_some_and(|m| m.starts_with("mcp_request_unclassified:"))
        );
        assert!(r.writer.write_all(call_line(9).as_bytes()).await.is_err());
        assert!(r.got.lock().unwrap().is_empty(), "nothing after it either");
        assert_eq!(r.tap.seal(), 0, "no call was handed over");
    }

    fn watched(id: u64) -> LineTap {
        let tap = LineTap::new();
        let line = listing(id);
        assert!(!tap.classify_line(line.trim_end().as_bytes()).unwrap());
        tap
    }

    /// The same listing answer read over HTTP (`read_frame`, after the
    /// listing was classified by `classify_message`) and through the stdio
    /// reader: the same captured result and length.
    #[tokio::test]
    async fn a_frame_read_over_http_is_captured_as_a_line_is() {
        let answer =
            br#"{"jsonrpc":"2.0","id":8,"result":{"tools":[{"name":"t","execution":{"x":1}}]}}"#;
        let http = LineTap::new();
        assert!(
            !http
                .classify_message(listing(8).trim_end().as_bytes())
                .unwrap()
        );
        http.read_frame(answer).unwrap();
        let over_http = http
            .take_last_listing()
            .unwrap()
            .expect("captured over http");

        let stdio = watched(8);
        let mut line = answer.to_vec();
        line.push(b'\n');
        let mut reader = stdio.reader(tokio_test_stream(vec![line]));
        let (_, failed) = drain(&mut reader).await;
        assert_eq!(failed, None);
        let over_stdio = stdio
            .take_last_listing()
            .unwrap()
            .expect("captured over stdio");
        assert_eq!(over_http, over_stdio);
    }

    /// Poisons the tap's state lock as a panic inside one of its critical
    /// sections would.
    fn poison(tap: &LineTap) {
        let state = tap.state.clone();
        let _ = std::thread::spawn(move || {
            let _held = state.lock().unwrap();
            panic!("a panic while the tap's state is held");
        })
        .join();
        assert!(tap.state.is_poisoned());
    }

    /// The first frame answering the call, a call result: vouched for.
    const CALL_RESULT: &[u8] = br#"{"jsonrpc":"2.0","id":4,"result":{"content":[]}}"#;

    /// A poisoned state vouches for no answer to a `tools/call`, not even
    /// one noted as a call result before the poison: rmcp's success then
    /// settles as the unexpected answer apb 1.0 read, a write `unknown`.
    #[test]
    fn a_poisoned_tap_vouches_for_no_call_answer() {
        let tap = LineTap::new();
        assert!(
            tap.classify_line(call_line(4).trim_end().as_bytes())
                .unwrap()
        );
        tap.observe(CALL_RESULT).unwrap();
        assert!(!tap.call_answer_untyped(), "a noted call result");
        poison(&tap);
        assert!(tap.call_answer_untyped(), "a poisoned state");
    }

    /// Without a noted first frame answering the latest call (no call, or
    /// only another id's answer read) the tap vouches for nothing either.
    #[test]
    fn a_call_answer_the_tap_did_not_note_is_not_vouched_for() {
        let tap = LineTap::new();
        assert!(tap.call_answer_untyped(), "no call was classified");
        assert!(
            tap.classify_line(call_line(4).trim_end().as_bytes())
                .unwrap()
        );
        assert!(tap.call_answer_untyped(), "no answer was read");
        tap.observe(br#"{"jsonrpc":"2.0","id":40,"result":{"content":[]}}"#)
            .unwrap();
        assert!(tap.call_answer_untyped(), "only another id's answer");
        tap.observe(CALL_RESULT).unwrap();
        assert!(!tap.call_answer_untyped(), "the call's own answer");
    }

    /// A `tools/call` the tap cannot note (its state is poisoned) is
    /// refused before any byte of it is forwarded, and the refusal seals
    /// the writer: the call settles not sent instead of reaching the
    /// server with an answer nobody can vouch for.
    #[tokio::test]
    async fn a_poisoned_tap_forwards_no_call() {
        let mut r = rig(json!(9), usize::MAX);
        poison(&r.tap);
        let err = r
            .writer
            .write_all(call_line(9).as_bytes())
            .await
            .expect_err("refused");
        assert_eq!(err.to_string(), TAP_POISONED);
        assert!(r.got.lock().unwrap().is_empty(), "nothing was forwarded");
        assert!(r.tap.is_sealed());
        assert_eq!(r.tap.seal(), 0, "no call was handed over");
    }

    /// Once the gate is sealed, `start_call` starts nothing and the
    /// hand-over count stays where it was.
    #[test]
    fn start_call_after_the_seal_starts_nothing() {
        let tap = LineTap::new();
        assert!(tap.start_call());
        assert_eq!(tap.seal(), 1);
        assert!(!tap.start_call());
        assert_eq!(tap.calls_handed(), 1);
    }

    /// Two frames for one listing id fail the listing closed, whichever
    /// comes first; the frame that is not a whole response is never
    /// captured.
    #[test]
    fn two_frames_for_one_listing_fail_closed() {
        let tap = watched(5);
        tap.observe(br#"{"jsonrpc":"2.0","id":5,"result":{"tools":[{"name":"old"}]},"x":1}"#)
            .unwrap();
        tap.observe(br#"{"jsonrpc":"2.0","id":5,"result":{"tools":[{"name":"new"}]}}"#)
            .unwrap();
        assert_eq!(tap.take_last_listing(), Err(LISTING_AMBIGUOUS.to_string()));
        let tap = watched(6);
        tap.observe(br#"{"jsonrpc":"2.0","id":6,"result":{"tools":[]}}"#)
            .unwrap();
        tap.observe(br#"{"jsonrpc":"2.0","id":6,"error":{"code":1,"message":"x"}}"#)
            .unwrap();
        assert_eq!(tap.take_last_listing(), Err(LISTING_AMBIGUOUS.to_string()));
    }

    /// Only a whole successful response is captured; a server request
    /// that reuses the id and a frame for another id are not counted.
    #[test]
    fn only_a_whole_response_is_captured() {
        let tap = watched(7);
        tap.observe(br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
            .unwrap();
        tap.observe(br#"{"jsonrpc":"2.0","id":70,"result":{"tools":[]}}"#)
            .unwrap();
        tap.observe(br#"{"jsonrpc":"2.0","id":7,"result":{"tools":[{"name":"t"}]}}"#)
            .unwrap();
        let (raw, _) = tap.take_last_listing().unwrap().expect("captured");
        assert_eq!(raw, json!({ "tools": [{ "name": "t" }] }));
        let tap = watched(8);
        tap.observe(br#"{"jsonrpc":"2.0","id":8,"result":{"tools":[]},"x":1}"#)
            .unwrap();
        assert_eq!(tap.take_last_listing(), Ok(None), "not a whole envelope");
    }

    /// Reads everything rmcp would get from `reader`: the bytes handed
    /// over and the error that ended the reads, if any.
    async fn drain<R: AsyncRead + Unpin>(reader: &mut TapReader<R>) -> (Vec<u8>, Option<String>) {
        use tokio::io::AsyncReadExt;
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => return (got, None),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => return (got, Some(e.to_string())),
            }
        }
    }

    /// The review's trigger, both answers in one read: the first with a
    /// byte order mark (which rmcp would strip and accept) and a changed
    /// unknown tool member, the second without it and with the pinned
    /// value. The tap refuses the first line, so rmcp receives no byte of
    /// either answer and nothing is captured: the listing cannot match a
    /// pin.
    #[tokio::test]
    async fn the_reviews_trigger_reaches_rmcp_with_no_byte() {
        let tap = watched(1);
        let tool = |support: &str| {
            json!({ "name": "t", "inputSchema": { "type": "object" },
                    "execution": { "taskSupport": support } })
        };
        let first = json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": [tool("required")] } });
        let second =
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": [tool("optional")] } });
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(format!("{first}\n{second}\n").as_bytes());
        let mut reader = tap.reader(std::io::Cursor::new(bytes));
        let (got, err) = drain(&mut reader).await;
        assert!(got.is_empty(), "rmcp got {}", String::from_utf8_lossy(&got));
        let err = err.expect("the reads end with a refusal");
        assert!(err.starts_with("mcp_message_invalid:"), "{err}");
        assert!(err.contains(frame::INCOMING_BOM), "{err}");
        assert!(tap.invalid().is_some());
        assert_eq!(tap.take_last_listing(), Ok(None), "nothing was captured");
    }

    /// Every refused shape ends the reads before any byte of its line, or
    /// of a line after it, reaches rmcp; the lines before it went through
    /// whole.
    #[tokio::test]
    async fn a_refused_line_reaches_rmcp_with_no_byte() {
        let good = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}\n";
        for bad in [
            &b"\xEF\xBB\xBF{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n"[..],
            b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{},\"x\":1,\"x\":2}\n",
            b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[{\"name\":\"t\",\"name\":\"u\"}]}}\n",
            b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}} trailing\n",
            b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}",
            b"{not json\n",
        ] {
            let tap = LineTap::new();
            let mut bytes = good.as_bytes().to_vec();
            bytes.extend_from_slice(bad);
            bytes.extend_from_slice(good.as_bytes());
            let mut reader = tap.reader(std::io::Cursor::new(bytes));
            let (got, err) = drain(&mut reader).await;
            assert!(
                got.is_empty() || got == good.as_bytes(),
                "{}",
                String::from_utf8_lossy(&got)
            );
            let err = err.expect("refused");
            assert!(err.starts_with("mcp_message_invalid:"), "{err}");
        }
    }

    /// Whole frames go through byte for byte, a line split across reads
    /// included, and a carriage return before the newline is kept for
    /// rmcp to strip.
    #[tokio::test]
    async fn whole_frames_go_through_byte_for_byte() {
        let tap = LineTap::new();
        let text = "{\"jsonrpc\":\"2.0\",\"method\":\"n\"}\r\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n";
        let (a, b) = text.as_bytes().split_at(20);
        let stream = tokio_test_stream(vec![a.to_vec(), b.to_vec()]);
        let mut reader = tap.reader(stream);
        let (got, err) = drain(&mut reader).await;
        assert_eq!(err, None);
        assert_eq!(got, text.as_bytes());
    }

    /// A stream that yields each of `chunks` in its own read.
    fn tokio_test_stream(chunks: Vec<Vec<u8>>) -> impl AsyncRead + Unpin {
        struct Chunks(std::collections::VecDeque<Vec<u8>>);
        impl AsyncRead for Chunks {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if let Some(c) = self.0.pop_front() {
                    buf.put_slice(&c);
                }
                Poll::Ready(Ok(()))
            }
        }
        Chunks(chunks.into())
    }

    /// The drop rule: log text, blank lines and lines of spaces between
    /// frames are dropped unread and counted, the frames go through byte
    /// for byte, and nothing of a dropped line reaches rmcp.
    #[tokio::test]
    async fn log_lines_between_frames_are_dropped_and_counted() {
        let tap = LineTap::new();
        let frames = "{\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n";
        let (a, b) = frames.split_at(frames.find('\n').unwrap() + 1);
        let text = format!("starting server LOGSECRET-1\n\n{a}   \n\r\n[info] LOGSECRET-2\n{b}");
        let mut reader = tap.reader(std::io::Cursor::new(text.into_bytes()));
        let (got, err) = drain(&mut reader).await;
        assert_eq!(err, None);
        assert_eq!(got, frames.as_bytes());
        assert_eq!(tap.dropped(), 5);
    }

    /// Past `MAX_DROPPED_LINES` dropped lines the session ends, with a
    /// message that holds none of their text; at the cap it does not.
    #[tokio::test]
    async fn the_dropped_line_cap_ends_the_session() {
        let log = "LOGSECRET-3 some log text\n";
        let at_cap = log.repeat(MAX_DROPPED_LINES);
        let tap = LineTap::new();
        let mut reader = tap.reader(std::io::Cursor::new(at_cap.clone().into_bytes()));
        assert_eq!(drain(&mut reader).await, (Vec::new(), None));
        let tap = LineTap::new();
        let past = format!("{at_cap}{log}{{\"jsonrpc\":\"2.0\",\"method\":\"n\"}}\n");
        let mut reader = tap.reader(std::io::Cursor::new(past.into_bytes()));
        let (got, err) = drain(&mut reader).await;
        assert!(got.is_empty());
        let err = err.expect("the session ends");
        assert_eq!(err, TOO_MANY_DROPPED);
        assert!(!err.contains("LOGSECRET"));
        assert_eq!(tap.invalid().as_deref(), Some(TOO_MANY_DROPPED));
    }

    /// A line that starts with a byte order mark is not log text: it is
    /// refused and ends the session, never dropped.
    #[tokio::test]
    async fn a_byte_order_mark_line_is_refused_not_dropped() {
        let tap = LineTap::new();
        let mut reader = tap.reader(std::io::Cursor::new(
            b"\xEF\xBB\xBFlog text\n{\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n".to_vec(),
        ));
        let (got, err) = drain(&mut reader).await;
        assert!(got.is_empty());
        assert!(err.expect("refused").contains(frame::INCOMING_BOM));
        assert_eq!(tap.dropped(), 0);
    }

    #[test]
    fn the_session_byte_cap_ends_the_reads() {
        let tap = LineTap::new();
        let mut reader = tap.reader(tokio::io::empty());
        // One whole notification of about 1 MiB per chunk, so neither the
        // line length nor the line count limit comes first.
        let head = br#"{"jsonrpc":"2.0","method":"n","params":{"p":""#;
        let tail = b"\"}}\n";
        let mut chunk = head.to_vec();
        chunk.resize(1024 * 1024 - tail.len(), b'a');
        chunk.extend_from_slice(tail);
        let mut read = 0usize;
        let err = loop {
            match reader.take_in(&chunk) {
                Ok(()) => read += chunk.len(),
                Err(e) => break e.to_string(),
            }
            reader.ready.clear();
            assert!(read <= MAX_SESSION_BYTES, "read past the cap");
        };
        assert!(err.contains(&MAX_SESSION_BYTES.to_string()), "{err}");
    }

    /// The close's drain counts by the reader's own rule: whole lines that
    /// do not open a JSON object are dropped lines, JSON lines are not.
    #[test]
    fn the_drain_counts_whole_log_lines_and_not_json_lines() {
        let tap = LineTap::new();
        tap.drain_bytes(b"boot log\n\n{\"jsonrpc\":\"2.0\"}\n", false);
        assert_eq!(tap.dropped(), 2);
    }

    /// The reader died holding a line without its newline: the drain's
    /// first bytes are that line's continuation, and counting starts after
    /// its first newline. With no newline at all, nothing is counted.
    #[test]
    fn the_drain_starts_counting_after_a_partial_lines_newline() {
        let tap = LineTap::new();
        tap.drain_bytes(b"of one long json answer\nlog after it\n", true);
        assert_eq!(tap.dropped(), 1);
        let tap = LineTap::new();
        tap.drain_bytes(b"more of the same long answer", true);
        assert_eq!(tap.dropped(), 0);
    }

    /// A last piece without its newline is an unfinished line: the reader
    /// refuses such a line and never counts it, and neither does the drain.
    #[test]
    fn the_drain_never_counts_an_unfinished_last_piece() {
        let tap = LineTap::new();
        tap.drain_bytes(b"whole log line\nlog line cut off", false);
        assert_eq!(tap.dropped(), 1);
    }

    /// A whole line over `MAX_LINE_BYTES` is what the reader refuses as
    /// too large, not what it drops: the drain does not count it.
    #[test]
    fn the_drain_does_not_count_a_line_over_the_line_cap() {
        let tap = LineTap::new();
        let mut bytes = vec![b'x'; MAX_LINE_BYTES + 1];
        bytes.push(b'\n');
        bytes.extend_from_slice(b"real log line\n");
        tap.drain_bytes(&bytes, false);
        assert_eq!(tap.dropped(), 1);
    }

    /// The bytes the drain reads count toward the session's byte limit:
    /// the reader's own total counts with them, so a session that read up
    /// to the limit takes no drained byte more.
    #[test]
    fn the_drains_bytes_count_toward_the_session_byte_limit() {
        let tap = LineTap::new();
        let mut reader = tap.reader(tokio::io::empty());
        // One whole notification per chunk, so neither the line length nor
        // the line count limit comes first.
        let head = br#"{"jsonrpc":"2.0","method":"n","params":{"p":""#;
        let tail = b"\"}}\n";
        let mut read = 0usize;
        while read < MAX_SESSION_BYTES {
            let size = (MAX_SESSION_BYTES - read).min(1024 * 1024);
            let mut chunk = head.to_vec();
            chunk.resize(size - tail.len(), b'a');
            chunk.extend_from_slice(tail);
            reader.take_in(&chunk).expect("the line is read");
            reader.ready.clear();
            read += size;
        }
        // The reader is gone: it recorded the total it read, which the
        // drain continues from.
        drop(reader);
        assert!(
            !tap.count_drained(b"one more log line\n".len()),
            "a drained byte past the limit is refused"
        );
    }

    /// The reader records whether it ended with a line still without its
    /// newline, so the drain knows where counting starts.
    #[test]
    fn the_reader_records_its_partial_line_for_the_drain() {
        let tap = LineTap::new();
        {
            let mut reader = tap.reader(tokio::io::empty());
            reader
                .take_in(b"{\"jsonrpc\":\"2.0\",\"method\":\"n\"}")
                .unwrap();
        }
        assert!(tap.partial_line());
        let tap = LineTap::new();
        {
            let mut reader = tap.reader(tokio::io::empty());
            reader
                .take_in(b"{\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n")
                .unwrap();
        }
        assert!(!tap.partial_line());
    }
}
