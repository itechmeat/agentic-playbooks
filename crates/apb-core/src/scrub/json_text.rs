//! A small tokenizer over JSON text as written: whether a text is one JSON
//! document, and where its string tokens (keys and values) lie, so a scrub
//! rewrites only those tokens and keeps every other byte as written.

/// How deep a document's objects and arrays may nest; a deeper one is not
/// read as JSON (the scrub then treats it as plain text).
const MAX_NESTING: usize = 128;

/// What a document is at its top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Top {
    /// An object or an array.
    Container,
    /// A string.
    Str,
    /// A number, `true`, `false` or `null`.
    Scalar,
}

/// One JSON document: its top and the byte span of every string token,
/// quotes included, in order.
pub(super) struct Document {
    pub top: Top,
    pub strings: Vec<(usize, usize)>,
}

/// `text` as one JSON document (surrounding whitespace allowed), or `None`
/// when it is not one or nests deeper than [`MAX_NESTING`].
pub(super) fn document(text: &str) -> Option<Document> {
    let mut t = Tokens {
        b: text.as_bytes(),
        i: 0,
        strings: Vec::new(),
    };
    t.ws();
    let top = match t.peek()? {
        b'{' | b'[' => Top::Container,
        b'"' => Top::Str,
        _ => Top::Scalar,
    };
    t.value(0)?;
    t.ws();
    (t.i == t.b.len()).then_some(Document {
        top,
        strings: t.strings,
    })
}

struct Tokens<'a> {
    b: &'a [u8],
    i: usize,
    strings: Vec<(usize, usize)>,
}

impl Tokens<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.i += 1)
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        self.ws();
        match self.peek()? {
            b'{' => self.container(depth, b'}', true),
            b'[' => self.container(depth, b']', false),
            b'"' => self.string(),
            b't' => self.word(b"true"),
            b'f' => self.word(b"false"),
            b'n' => self.word(b"null"),
            _ => self.number(),
        }
    }

    /// An object (`keyed`) or an array, its opening byte next.
    fn container(&mut self, depth: usize, close: u8, keyed: bool) -> Option<()> {
        if depth >= MAX_NESTING {
            return None;
        }
        self.i += 1;
        self.ws();
        if self.eat(close).is_some() {
            return Some(());
        }
        loop {
            if keyed {
                self.ws();
                self.string()?;
                self.ws();
                self.eat(b':')?;
            }
            self.value(depth + 1)?;
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                c if c == close => {
                    self.i += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    /// A string token, its quote next; its span is recorded. Escapes are
    /// only skipped here: the scrub decodes the token, which checks them.
    fn string(&mut self) -> Option<()> {
        let start = self.i;
        self.eat(b'"')?;
        loop {
            match self.peek()? {
                b'"' => {
                    self.i += 1;
                    self.strings.push((start, self.i));
                    return Some(());
                }
                b'\\' => self.i += 2,
                c if c < 0x20 => return None,
                _ => self.i += 1,
            }
        }
    }

    fn word(&mut self, w: &[u8]) -> Option<()> {
        self.b
            .get(self.i..self.i + w.len())
            .filter(|s| *s == w)
            .map(|_| self.i += w.len())
    }

    fn number(&mut self) -> Option<()> {
        let digits = |t: &mut Self| {
            let from = t.i;
            while t.peek().is_some_and(|c| c.is_ascii_digit()) {
                t.i += 1;
            }
            (t.i > from).then_some(())
        };
        let _ = self.eat(b'-');
        digits(self)?;
        if self.eat(b'.').is_some() {
            digits(self)?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            digits(self)?;
        }
        Some(())
    }
}
