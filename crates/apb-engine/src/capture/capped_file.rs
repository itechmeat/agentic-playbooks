//! A file copy of an output stream with a size cap.
//!
//! Once the cap is reached the file gets one plain marker line and nothing
//! more, while the caller keeps reading its source: a cap never blocks or
//! kills the agent. A file that cannot be created or written only loses the
//! copy, never the attempt, and a lost copy is not a truncation.
//!
//! A copy may carry a [`Redact`]: the value is replaced as the bytes arrive,
//! before they reach the file, so a driver killed mid-attempt leaves no
//! plain value behind. Only the longest tail of a write that could start the
//! value is held back until the next write or the end of the stream.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;

/// A captured stream may carry a secret the agent echoed: a new file is
/// created 0600 on unix.
fn private_mode(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = options;
}

/// A value replaced in a stream's file copy as it is written, and its
/// marker (`adapter::key_scrub`).
#[derive(Clone)]
pub(crate) struct Redact {
    key: apb_core::secret::Secret,
    marker: String,
}

impl std::fmt::Debug for Redact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redact")
            .field("marker", &self.marker)
            .finish_non_exhaustive()
    }
}

impl Redact {
    pub(crate) fn new(key: apb_core::secret::Secret, marker: String) -> Self {
        Self { key, marker }
    }

    /// `buf` with every whole occurrence of the key replaced, and the tail
    /// held back: the longest suffix that is a proper prefix of the key.
    fn apply(&self, buf: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let needle = self.key.expose_secret().as_bytes();
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        while let Some(pos) = buf[i..].windows(needle.len()).position(|w| w == needle) {
            out.extend_from_slice(&buf[i..i + pos]);
            out.extend_from_slice(self.marker.as_bytes());
            i += pos + needle.len();
        }
        let rest = &buf[i..];
        let hold = (1..needle.len().min(rest.len() + 1))
            .rev()
            .find(|&n| rest.ends_with(&needle[..n]))
            .unwrap_or(0);
        out.extend_from_slice(&rest[..rest.len() - hold]);
        (out, rest[rest.len() - hold..].to_vec())
    }
}

#[derive(Debug)]
pub(crate) struct CappedFile {
    file: Option<File>,
    redact: Option<Redact>,
    /// Bytes held back by `redact` that may start the value.
    carry: Vec<u8>,
    cap: u64,
    /// Bytes the file already held when it was opened for appending: the
    /// cap is on the file, so they count against it.
    base: u64,
    written: u64,
    dropped: u64,
}

impl CappedFile {
    /// A fresh file at `path` (truncated if it exists).
    pub(crate) fn create(path: &Path, cap: u64) -> Self {
        // A read-only process creates no file: every write is a no-op.
        if apb_core::process_mode::is_read_only() {
            return Self::none();
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        private_mode(&mut options);
        let file = options.open(path).ok();
        // A file an older apb left readable keeps its mode through the open.
        file.iter().for_each(apb_core::fsutil::narrow_open);
        Self::over(file, cap)
    }

    /// `path` opened for appending (created if missing). What the file
    /// already holds counts against `cap`, so a file appended to more than
    /// once still stops at its cap.
    pub(crate) fn append(path: &Path, cap: u64) -> Self {
        let file = apb_core::fsutil::open_append_private(path).ok();
        let base = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        let mut out = Self::over(file, cap);
        out.base = base;
        out
    }

    /// No file: every write is a no-op and nothing is ever truncated.
    pub(crate) fn none() -> Self {
        Self::over(None, 0)
    }

    fn over(file: Option<File>, cap: u64) -> Self {
        Self {
            file,
            redact: None,
            carry: Vec::new(),
            cap,
            base: 0,
            written: 0,
            dropped: 0,
        }
    }

    /// The copy with `redact` applied to every write.
    pub(crate) fn redacting(mut self, redact: Option<Redact>) -> Self {
        self.redact = redact;
        self
    }

    /// Writes what `redact` held back: the stream ended.
    pub(crate) fn end(&mut self) {
        let carry = std::mem::take(&mut self.carry);
        if !carry.is_empty() {
            self.write_raw(&carry);
        }
    }

    /// Whether a file is being written.
    pub(crate) fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// Writes as much of `bytes` as the cap allows; the rest is counted.
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        match &self.redact {
            Some(r) => {
                let mut buf = std::mem::take(&mut self.carry);
                buf.extend_from_slice(bytes);
                let (out, carry) = r.apply(&buf);
                self.carry = carry;
                self.write_raw(&out);
            }
            None => self.write_raw(bytes),
        }
    }

    fn write_raw(&mut self, bytes: &[u8]) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let room = usize::try_from(self.cap.saturating_sub(self.base + self.written))
            .unwrap_or(usize::MAX);
        let (keep, rest) = bytes.split_at(room.min(bytes.len()));
        if !keep.is_empty() {
            if file.write_all(keep).is_err() {
                self.file = None;
                return;
            }
            self.written += keep.len() as u64;
        }
        if rest.is_empty() {
            return;
        }
        // One marker per file: a file reopened already at or past its cap
        // ended with a marker from an earlier writer, so this one only counts.
        if self.dropped == 0 && self.base < self.cap {
            let marker = format!(
                "\n[apb] output limit reached: {} bytes written, the rest of this stream was not kept\n",
                self.base + self.written
            );
            let _ = file.write_all(marker.as_bytes());
        }
        self.dropped += rest.len() as u64;
    }

    /// Bytes of this writer's stream the file holds.
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// Bytes of the stream the cap kept out of the file.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }
}

impl Drop for CappedFile {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact() -> Redact {
        Redact::new(
            apb_core::secret::Secret::new("fake-key-1234567".to_string()),
            "[redacted:NEARAI_API_KEY]".into(),
        )
    }

    #[test]
    fn a_key_split_across_writes_never_reaches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stdout.log");
        let mut f = CappedFile::create(&path, 1_000).redacting(Some(redact()));
        f.write(b"one fake-key-12");
        // Mid-stream, as a killed driver would leave the file: only the
        // text before a possible key is there.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one ");
        f.write(b"34567 two fake-key-1234567\nthree fake");
        f.write(b"-ke\n");
        let mid = std::fs::read_to_string(&path).unwrap();
        assert!(!mid.contains("fake-key-1234567"), "{mid}");
        assert_eq!(
            mid,
            "one [redacted:NEARAI_API_KEY] two [redacted:NEARAI_API_KEY]\nthree fake-ke\n"
        );
        f.write(b"tail fake");
        drop(f);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("tail fake")
        );
    }

    /// A capture file an older apb left readable by others is narrowed when
    /// it is opened again, whether truncated or appended to.
    #[cfg(unix)]
    #[test]
    fn a_reopened_capture_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for (name, open) in [
            (
                "out.log",
                CappedFile::create as fn(&Path, u64) -> CappedFile,
            ),
            ("err.log", CappedFile::append),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, "old").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let _f = open(&path, 10);
            assert_eq!(mode(&path), 0o600, "{name}");
        }
    }

    #[test]
    fn the_cap_ends_the_copy_with_one_marker_and_counts_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.log");
        let mut f = CappedFile::create(&path, 5);
        f.write(b"abc");
        f.write(b"defgh");
        f.write(b"ijk");
        assert_eq!((f.written(), f.dropped()), (5, 6));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("abcde\n[apb] output limit reached: 5 bytes written"));
        assert_eq!(text.matches("[apb]").count(), 1);
    }

    #[test]
    fn appending_to_a_file_again_keeps_the_whole_file_under_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.log");
        let mut first = CappedFile::append(&path, 6);
        first.write(b"abcd");
        drop(first);
        let mut again = CappedFile::append(&path, 6);
        again.write(b"efgh");
        // Two bytes of room were left; this writer kept those and counted
        // the rest of its own stream.
        assert_eq!((again.written(), again.dropped()), (2, 2));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("abcdef\n[apb] output limit reached: 6 bytes written"),
            "{text}"
        );
    }

    #[test]
    fn a_file_reopened_at_its_cap_gets_no_second_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.log");
        let mut first = CappedFile::append(&path, 3);
        first.write(b"abcd");
        drop(first);
        for _ in 0..2 {
            let mut again = CappedFile::append(&path, 3);
            again.write(b"efgh");
            assert_eq!((again.written(), again.dropped()), (0, 4));
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("abc\n[apb] output limit reached"),
            "{text}"
        );
        assert_eq!(text.matches("[apb]").count(), 1, "{text}");
    }

    #[test]
    fn a_file_that_cannot_be_created_loses_only_the_copy() {
        let mut f = CappedFile::create(Path::new("/nonexistent-dir/out.log"), 1);
        f.write(b"abc");
        assert!(!f.is_open());
        assert_eq!(f.dropped(), 0);
    }
}
