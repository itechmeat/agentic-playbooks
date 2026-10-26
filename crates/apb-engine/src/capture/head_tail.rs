//! The bounded in-memory copy of one output stream: its first bytes (the
//! head) and its last bytes (the tail ring), with a count of what fell out
//! between them.

use std::collections::VecDeque;

/// A stream's bounded copy while it is being read.
#[derive(Debug)]
pub(crate) struct HeadTail {
    head_cap: usize,
    tail_cap: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: u64,
    /// Whether the dropped span ended at a line boundary, so the tail
    /// starts with a whole line.
    cut_at_line: bool,
}

impl HeadTail {
    pub(crate) fn new(head_cap: usize, tail_cap: usize) -> Self {
        Self {
            head_cap,
            tail_cap,
            head: Vec::new(),
            tail: VecDeque::new(),
            dropped: 0,
            cut_at_line: true,
        }
    }

    /// Appends `bytes` read from the stream.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        let room = self.head_cap.saturating_sub(self.head.len());
        let (to_head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(to_head);
        if rest.is_empty() {
            return;
        }
        self.tail.extend(rest);
        let excess = self.tail.len().saturating_sub(self.tail_cap);
        if excess > 0 {
            let last = self.tail.drain(..excess).next_back();
            self.dropped += excess as u64;
            self.cut_at_line = last == Some(b'\n');
        }
    }

    /// Counts `n` bytes the reader dropped itself (a line too long to keep
    /// whole) at a line boundary. The head takes nothing after a drop, so the
    /// kept copy stays one head, one gap, one tail.
    pub(crate) fn skip_line(&mut self, n: u64) {
        self.head_cap = self.head.len();
        // Everything in the tail so far precedes the gap: it is dropped with
        // it, since a copy with two gaps could not say where they are.
        self.dropped += self.tail.len() as u64 + n;
        self.tail.clear();
        self.cut_at_line = true;
    }

    pub(crate) fn finish(self) -> Kept {
        Kept {
            head: self.head,
            tail: self.tail.into(),
            dropped: self.dropped,
            cut_at_line: self.cut_at_line,
        }
    }
}

/// A stream's bounded copy once it was read to the end.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    head: Vec<u8>,
    tail: Vec<u8>,
    dropped: u64,
    cut_at_line: bool,
}

impl Kept {
    /// Bytes held in memory.
    pub fn kept_bytes(&self) -> u64 {
        (self.head.len() + self.tail.len()) as u64
    }

    /// Bytes that fell out between the head and the tail.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped
    }

    /// The kept text. Whole when nothing was dropped; otherwise the head up
    /// to its last complete line and the tail from its first complete line,
    /// so a line torn by the cut never reads as a record.
    pub fn text(&self) -> String {
        if self.dropped == 0 {
            let mut all = self.head.clone();
            all.extend_from_slice(&self.tail);
            return String::from_utf8_lossy(&all).into_owned();
        }
        let head = match self.head.iter().rposition(|b| *b == b'\n') {
            Some(i) => &self.head[..=i],
            None => &[][..],
        };
        let tail = if self.cut_at_line {
            &self.tail[..]
        } else {
            match self.tail.iter().position(|b| *b == b'\n') {
                Some(i) => &self.tail[i + 1..],
                None => &[][..],
            }
        };
        let mut out = String::from_utf8_lossy(head).into_owned();
        out.push_str(&String::from_utf8_lossy(tail));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(head: usize, tail: usize, chunks: &[&[u8]]) -> Kept {
        let mut b = HeadTail::new(head, tail);
        for c in chunks {
            b.push(c);
        }
        b.finish()
    }

    #[test]
    fn a_stream_within_the_limits_is_kept_whole() {
        let k = kept(4, 8, &[b"ab", b"cdefg", b"h\n"]);
        assert_eq!(k.text(), "abcdefgh\n");
        assert_eq!(k.dropped_bytes(), 0);
    }

    #[test]
    fn the_middle_is_dropped_and_torn_lines_are_left_out() {
        let k = kept(6, 10, &[b"first\nsecond\nmiddle\n", b"last-line\n"]);
        // The head keeps `first\n`; the tail keeps `last-line\n` whole
        // because the cut fell right after a newline.
        assert_eq!(k.text(), "first\nlast-line\n");
        assert_eq!(k.kept_bytes(), 16);
        assert_eq!(k.dropped_bytes(), 30 - 16);
        let torn = kept(3, 6, &[b"one\ntwo three\nfour\n"]);
        // Head `one` has no newline yet, the tail starts mid-line.
        assert_eq!(torn.text(), "four\n");
    }

    #[test]
    fn a_skipped_line_closes_the_head_and_counts_as_dropped() {
        let mut b = HeadTail::new(100, 100);
        b.push(b"a\n");
        b.skip_line(500);
        b.push(b"z\n");
        let k = b.finish();
        assert_eq!(k.text(), "a\nz\n");
        assert_eq!(k.dropped_bytes(), 500);
    }
}
