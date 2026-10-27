//! Reading an agent's pipes into bounded copies.
//!
//! Every stream is read on its own thread from the moment the agent is
//! spawned: reading only after the process exits deadlocked any agent that
//! wrote more than a pipe buffer (64 KiB on Linux), since the write blocked
//! and the process never exited. The readers keep reading whatever a limit
//! says, so a cap never blocks the agent either.

use std::io::Read;
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use apb_core::output_limits::{OutputLimits, as_usize};

use super::capped_file::CappedFile;
use super::head_tail::{HeadTail, Kept};
use crate::event::OutputLoss;

/// The counts of a stream's file copy.
#[derive(Debug, Clone, Copy, Default)]
struct FileCounts {
    open: bool,
    written: u64,
    dropped: u64,
}

impl CappedFile {
    fn counts(&self) -> FileCounts {
        FileCounts {
            open: self.is_open(),
            written: self.written(),
            dropped: self.dropped(),
        }
    }
}

/// One stream read to its end (or as far as the reader got).
#[derive(Debug, Clone)]
pub(crate) struct StreamCapture {
    pub(crate) kept: Kept,
    file: FileCounts,
}

impl StreamCapture {
    /// The kept text (see [`Kept::text`]).
    pub(crate) fn text(&self) -> String {
        self.kept.text()
    }

    /// What this stream lost, named `stream`: `None` when the memory copy
    /// or the file copy holds all of it.
    pub(crate) fn loss(&self, stream: &str) -> Option<OutputLoss> {
        let file_whole = self.file.open && self.file.dropped == 0;
        if self.kept.dropped_bytes() == 0 || file_whole {
            return None;
        }
        let (kept, total) = if self.file.open {
            (
                self.file.written.max(self.kept.kept_bytes()),
                self.file.written + self.file.dropped,
            )
        } else {
            (
                self.kept.kept_bytes(),
                self.kept.kept_bytes() + self.kept.dropped_bytes(),
            )
        };
        super::loss(stream, kept, total)
    }
}

/// Reads `pipe` to EOF on a thread into a bounded copy, teeing every chunk
/// into `file` as it arrives. The copy is delivered over the returned
/// channel, so the caller can wait for it with a deadline.
pub(crate) fn drain_stream(
    pipe: Option<impl Read + Send + 'static>,
    limits: &OutputLimits,
    mut file: CappedFile,
) -> mpsc::Receiver<StreamCapture> {
    let (tx, rx) = mpsc::channel();
    let mut kept = HeadTail::new(as_usize(limits.head_bytes), as_usize(limits.tail_bytes));
    std::thread::spawn(move || {
        if let Some(mut p) = pipe {
            let mut chunk = [0u8; 8192];
            loop {
                match p.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        kept.push(&chunk[..n]);
                        file.write(&chunk[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        let _ = tx.send(StreamCapture {
            kept: kept.finish(),
            file: file.counts(),
        });
    });
    rx
}

/// A spawned agent's stdout and stderr, drained from the start (see the
/// module docs). With a transcript directory, every chunk is also written to
/// `stdout.log` and `stderr.log` there as it arrives, so the files hold what
/// the agent printed even when the attempt is killed.
pub(crate) struct Pipes {
    stdout: mpsc::Receiver<StreamCapture>,
    stderr: mpsc::Receiver<StreamCapture>,
}

/// Both streams of an exited agent, and its status.
pub(crate) struct Collected {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: StreamCapture,
    pub(crate) stderr: StreamCapture,
}

/// Why the streams of an exited agent could not be collected.
#[derive(Debug)]
pub(crate) enum PipesError {
    /// A descendant that outlived the agent still held the pipes open when
    /// the budget ran out.
    HeldOpen,
    /// Reaping the agent failed.
    Wait(std::io::Error),
}

impl Pipes {
    pub(crate) fn start(child: &mut Child, tee: Option<&Path>, limits: &OutputLimits) -> Self {
        let file = |name: &str| match tee {
            Some(dir) => CappedFile::create(&dir.join(name), limits.log_file_bytes),
            None => CappedFile::none(),
        };
        Pipes {
            stdout: drain_stream(child.stdout.take(), limits, file("stdout.log")),
            stderr: drain_stream(child.stderr.take(), limits, file("stderr.log")),
        }
    }

    /// The collected streams once both reached EOF, within `budget`, so a
    /// grandchild holding the pipes open cannot stall the drive. The reading
    /// threads are abandoned on a timeout rather than joined: they own
    /// nothing the caller needs.
    pub(crate) fn finish(
        self,
        mut child: Child,
        budget: Duration,
    ) -> Result<Collected, PipesError> {
        let deadline = Instant::now() + budget;
        let left = || deadline.saturating_duration_since(Instant::now());
        let stdout = self
            .stdout
            .recv_timeout(left())
            .map_err(|_| PipesError::HeldOpen)?;
        let stderr = self
            .stderr
            .recv_timeout(left())
            .map_err(|_| PipesError::HeldOpen)?;
        let status = child.wait().map_err(PipesError::Wait)?;
        Ok(Collected {
            status,
            stdout,
            stderr,
        })
    }
}

/// One line of a line-oriented stream, as the reader thread hands it over.
enum StreamLine {
    Text(String),
    /// A line longer than the tail limit: never kept whole, only counted.
    TooLong(u64),
}

impl StreamLine {
    /// The bytes this line holds in the queue.
    fn weight(&self) -> usize {
        match self {
            StreamLine::Text(text) => text.len(),
            StreamLine::TooLong(_) => 0,
        }
    }
}

/// What the queue between the reader thread and the engine holds now.
#[derive(Default)]
struct QueueState {
    lines: usize,
    bytes: usize,
    /// The engine side is gone: the reader stops instead of waiting.
    closed: bool,
}

/// The bound of the queue between a [`LineCapture`]'s reader thread and the
/// engine: at most `max_lines` lines and `max_bytes` bytes wait in it. The
/// reader blocks while it is full, so a slow engine bounds the memory the
/// queue takes, not only its line count. A line is always admitted into an
/// empty queue; the reader never queues a line longer than `max_bytes`.
struct QueueBound {
    max_lines: usize,
    max_bytes: usize,
    state: Mutex<QueueState>,
    room: Condvar,
}

impl QueueBound {
    fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            max_lines: max_lines.max(1),
            max_bytes,
            state: Mutex::new(QueueState::default()),
            room: Condvar::new(),
        }
    }

    /// Waits until a line of `bytes` fits, then counts it in. `false` when
    /// the engine side is gone.
    fn admit(&self, bytes: usize) -> bool {
        let Ok(mut s) = self.state.lock() else {
            return false;
        };
        while !s.closed
            && s.lines > 0
            && (s.lines >= self.max_lines || s.bytes.saturating_add(bytes) > self.max_bytes)
        {
            s = match self.room.wait(s) {
                Ok(s) => s,
                Err(_) => return false,
            };
        }
        if s.closed {
            return false;
        }
        s.lines += 1;
        s.bytes += bytes;
        true
    }

    /// Counts out a line the engine took.
    fn release(&self, bytes: usize) {
        if let Ok(mut s) = self.state.lock() {
            s.lines = s.lines.saturating_sub(1);
            s.bytes = s.bytes.saturating_sub(bytes);
        }
        self.room.notify_all();
    }

    fn close(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.closed = true;
        }
        self.room.notify_all();
    }
}

/// A line-oriented stream (the streaming transport's NDJSON stdout), read on
/// a thread that tees the raw bytes into a capped log file and hands whole
/// lines over a queue bounded in lines and bytes ([`QueueBound`]); the lines
/// land in a bounded copy here.
pub(crate) struct LineCapture {
    rx: mpsc::Receiver<StreamLine>,
    bound: Arc<QueueBound>,
    kept: HeadTail,
    file: Arc<Mutex<FileCounts>>,
}

impl Drop for LineCapture {
    fn drop(&mut self) {
        self.bound.close();
    }
}

/// The stream reached its end and every line was taken.
#[derive(Debug)]
pub(crate) struct Ended;

impl LineCapture {
    pub(crate) fn start(
        pipe: impl Read + Send + 'static,
        limits: &OutputLimits,
        log: CappedFile,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let file = Arc::new(Mutex::new(log.counts()));
        let counts = Arc::clone(&file);
        // A kept line is pushed with a `\n` after it and must fit the tail
        // whole, so the longest line kept is one byte under the tail.
        let max_line = as_usize(limits.tail_bytes).saturating_sub(1);
        let bound = Arc::new(QueueBound::new(
            as_usize(limits.stream_line_queue),
            as_usize(limits.tail_bytes),
        ));
        let queue = Queue {
            tx,
            bound: Arc::clone(&bound),
        };
        std::thread::spawn(move || read_lines(pipe, log, max_line, &queue, &counts));
        Self {
            rx,
            bound,
            kept: HeadTail::new(as_usize(limits.head_bytes), as_usize(limits.tail_bytes)),
            file,
        }
    }

    /// Waits up to `wait` for a line, then takes every line already queued.
    pub(crate) fn pump(&mut self, wait: Duration) -> Result<(), Ended> {
        let first = match self.rx.recv_timeout(wait) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Ended),
        };
        self.take(first);
        while let Ok(line) = self.rx.try_recv() {
            self.take(line);
        }
        Ok(())
    }

    /// Takes every line that arrives before the stream ends or `deadline`.
    pub(crate) fn drain_until(&mut self, deadline: Instant) {
        while Instant::now() < deadline {
            if self
                .pump(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                return;
            }
        }
    }

    fn take(&mut self, line: StreamLine) {
        self.bound.release(line.weight());
        match line {
            StreamLine::Text(text) => {
                self.kept.push(text.as_bytes());
                self.kept.push(b"\n");
            }
            StreamLine::TooLong(n) => self.kept.skip_line(n),
        }
    }

    pub(crate) fn finish(mut self) -> StreamCapture {
        let file = self.file.lock().map(|c| *c).unwrap_or_default();
        let kept = std::mem::replace(&mut self.kept, HeadTail::new(0, 0));
        StreamCapture {
            kept: kept.finish(),
            file,
        }
    }
}

/// The reader thread's end of a [`LineCapture`] queue.
struct Queue {
    tx: mpsc::Sender<StreamLine>,
    bound: Arc<QueueBound>,
}

impl Queue {
    /// Hands `line` over once it fits the bound; `false` when the engine
    /// side is gone.
    fn send(&self, line: StreamLine) -> bool {
        self.bound.admit(line.weight()) && self.tx.send(line).is_ok()
    }
}

/// The reader thread of a [`LineCapture`].
fn read_lines(
    mut pipe: impl Read,
    mut log: CappedFile,
    max_line: usize,
    tx: &Queue,
    counts: &Mutex<FileCounts>,
) {
    let mut line: Vec<u8> = Vec::new();
    // Bytes of the current line once it outgrew `max_line`.
    let mut too_long: Option<u64> = None;
    let mut chunk = [0u8; 8192];
    let emit = |line: &mut Vec<u8>, too_long: &mut Option<u64>, newline: bool| {
        let out = match too_long.take() {
            Some(n) => StreamLine::TooLong(n + u64::from(newline)),
            None => {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                StreamLine::Text(String::from_utf8_lossy(line).into_owned())
            }
        };
        line.clear();
        tx.send(out)
    };
    loop {
        let n = match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        log.write(&chunk[..n]);
        if let Ok(mut c) = counts.lock() {
            *c = log.counts();
        }
        for part in chunk[..n].split_inclusive(|b| *b == b'\n') {
            let (body, newline) = match part.strip_suffix(b"\n") {
                Some(body) => (body, true),
                None => (part, false),
            };
            match too_long.as_mut() {
                Some(count) => *count += body.len() as u64,
                None => {
                    line.extend_from_slice(body);
                    if line.len() > max_line {
                        too_long = Some(line.len() as u64);
                        line.clear();
                    }
                }
            }
            if newline && !emit(&mut line, &mut too_long, true) {
                return;
            }
        }
    }
    if !line.is_empty() || too_long.is_some() {
        emit(&mut line, &mut too_long, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(head: u64, tail: u64) -> OutputLimits {
        OutputLimits {
            head_bytes: head,
            tail_bytes: tail,
            stream_line_queue: 2,
            ..OutputLimits::default()
        }
    }

    fn capture_lines(input: &'static [u8], limits: &OutputLimits) -> StreamCapture {
        let mut lines = LineCapture::start(input, limits, CappedFile::none());
        while lines.pump(Duration::from_secs(5)).is_ok() {}
        lines.finish()
    }

    #[test]
    fn whole_lines_are_kept_through_a_small_queue() {
        let c = capture_lines(b"a\r\nb\nc\nd\ne", &limits(100, 100));
        assert_eq!(c.text(), "a\nb\nc\nd\ne\n");
        assert_eq!(c.loss("stdout"), None);
    }

    #[test]
    fn a_line_longer_than_the_tail_is_dropped_and_counted() {
        let c = capture_lines(
            b"{\"a\":1}\n0123456789abcdef\n{\"z\":2}\n",
            &limits(100, 10),
        );
        assert_eq!(c.text(), "{\"a\":1}\n{\"z\":2}\n");
        let loss = c.loss("stdout").unwrap();
        assert_eq!(loss.dropped_bytes_at_least, 17);
    }

    #[test]
    fn a_line_one_byte_under_the_tail_is_the_longest_kept_whole() {
        // No head: every line lands in the tail with its `\n`.
        let fits = capture_lines(b"123456789\n", &limits(0, 10));
        assert_eq!(fits.text(), "123456789\n");
        assert_eq!(fits.loss("stdout"), None);
        // A line as long as the tail cannot fit with its newline: it is
        // counted as dropped whole, not cut by one byte.
        let too_long = capture_lines(b"0123456789\n", &limits(0, 10));
        assert_eq!(too_long.text(), "");
        assert_eq!(too_long.kept.dropped_bytes(), 11);
    }

    #[test]
    fn the_queue_holds_no_more_bytes_than_the_tail_while_the_engine_waits() {
        let line = [b'x'; 29];
        let mut input = Vec::new();
        for _ in 0..50 {
            input.extend_from_slice(&line);
            input.push(b'\n');
        }
        let input: &'static [u8] = Box::leak(input.into_boxed_slice());
        let limits = OutputLimits {
            stream_line_queue: 1_000,
            ..limits(10_000, 100)
        };
        let mut lines = LineCapture::start(input, &limits, CappedFile::none());
        let queued = |lines: &LineCapture| {
            let s = lines.bound.state.lock().unwrap();
            (s.lines, s.bytes)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while queued(&lines).0 < 3 {
            assert!(Instant::now() < deadline, "the reader queued nothing");
            std::thread::yield_now();
        }
        // Nothing is taken: the reader must stay blocked at three 29-byte
        // lines, since a fourth would put 116 bytes in a 100-byte queue.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(queued(&lines), (3, 87));
        while lines.pump(Duration::from_secs(5)).is_ok() {}
        let c = lines.finish();
        assert_eq!(c.text().lines().count(), 50);
        assert_eq!(c.loss("stdout"), None);
    }

    #[test]
    fn a_capped_file_that_holds_everything_means_no_loss() {
        let dir = tempfile::tempdir().unwrap();
        let file = CappedFile::create(&dir.path().join("out"), 1_000);
        let rx = drain_stream(Some(&b"0123456789"[..]), &limits(2, 2), file);
        let c = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(c.kept.dropped_bytes(), 6);
        assert_eq!(c.loss("stdout"), None);
    }
}
