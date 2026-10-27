//! A file copy of an output stream with a size cap.
//!
//! Once the cap is reached the file gets one plain marker line and nothing
//! more, while the caller keeps reading its source: a cap never blocks or
//! kills the agent. A file that cannot be created or written only loses the
//! copy, never the attempt, and a lost copy is not a truncation.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;

#[derive(Debug)]
pub(crate) struct CappedFile {
    file: Option<File>,
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
        Self::over(File::create(path).ok(), cap)
    }

    /// `path` opened for appending (created if missing). What the file
    /// already holds counts against `cap`, so a file appended to more than
    /// once still stops at its cap.
    pub(crate) fn append(path: &Path, cap: u64) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        let base = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            base,
            ..Self::over(file, cap)
        }
    }

    /// No file: every write is a no-op and nothing is ever truncated.
    pub(crate) fn none() -> Self {
        Self::over(None, 0)
    }

    fn over(file: Option<File>, cap: u64) -> Self {
        Self {
            file,
            cap,
            base: 0,
            written: 0,
            dropped: 0,
        }
    }

    /// Whether a file is being written.
    pub(crate) fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// Writes as much of `bytes` as the cap allows; the rest is counted.
    pub(crate) fn write(&mut self, bytes: &[u8]) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
