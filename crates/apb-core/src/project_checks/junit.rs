//! The pure reader of one JUnit XML report: the failing test cases as
//! `<classname>::<name>`, or why the report cannot be read, and the one
//! rule a failing test becomes an observation list item by
//! ([`junit_items`]).
//!
//! The principle: the reader returns a list only for a report it
//! understood completely; anything else is unknown with a named reason,
//! never a partial or guessed list. A report is understood when it is
//! UTF-8 (a UTF-8 byte order mark is skipped; a UTF-16 one, or an XML
//! declaration naming another encoding, is unknown), well formed as far
//! as a JUnit report needs (one root element, every element closed in
//! order, end tags with optional whitespace before `>`, attribute values
//! in single or double quotes, every entity one of the five XML names or
//! a valid character reference), with nothing after the root but
//! whitespace, comments and processing instructions, and no DOCTYPE or
//! other declaration (an entity declaration could change what a name
//! means). A failing test whose identity is too long for an observation
//! item, or holds a character the observation text rules refuse, makes
//! the list unknown too ([`junit_items`]): an identity is never cut,
//! so two different tests never compare equal.
//!
//! A scanner over the bytes, no XML crate (the workspace has none). A test
//! case is a `<testcase>` element; it failed when an element directly
//! inside it is `<failure>` or `<error>` (`<skipped>` is not a failure).
//! No input can make it panic: it moves over the byte slice, matches only
//! ASCII markers there and turns into text only values it cut at ASCII
//! delimiters of input it checked is UTF-8. Its work is linear in the
//! input (one forward pass, character references included, whatever their
//! leading zeros), and every loop advances through one cursor that runs
//! the caller's check (its deadline and cancel flag) every
//! [`CHECK_EVERY`] bytes, and once more before a list is returned.

use crate::baseline::extract::checked_text;
use crate::baseline::{MAX_ITEM_CHARS, MAX_LIST_ITEMS};

/// The largest report read; a larger one makes the list unknown.
pub const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;
/// The most report files one command may write.
pub const MAX_REPORT_FILES: usize = 64;
/// How many bytes the scan moves between two calls of the caller's check.
pub const CHECK_EVERY: usize = 64 * 1024;
/// The deepest element nesting read; a deeper report reads unknown.
const MAX_DEPTH: usize = 256;

const CUT_SHORT: &str = "the JUnit report is cut short";

/// What the reason of a report with more failing tests than the reader
/// was allowed to keep starts with, so the caller can name its own cap.
pub const TOO_MANY_FAILING: &str = "too many failing tests";

/// Whether `c` is a glob wildcard (`**` is two of them). V320 and the
/// engine's walk read a `junit` glob's components by this one set.
pub fn is_wildcard(c: char) -> bool {
    matches!(c, '*' | '?' | '[' | '{')
}

/// The matcher of a `junit` glob, built the one way V320 checks it and
/// the engine matches with: `globset` with a literal separator, so `*`
/// never crosses `/`.
pub fn matcher(glob: &str) -> Result<globset::GlobMatcher, globset::Error> {
    Ok(globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()?
        .compile_matcher())
}

/// One failing test case of a report: its fields as the report gave them,
/// `classname` absent kept apart from empty. Compared and sorted by its
/// fields, never by a joined text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FailingTest {
    pub classname: Option<String>,
    pub name: String,
}

impl FailingTest {
    /// The human text of the case, `<classname>::<name>`, or `<name>`
    /// when `classname` is absent or empty. Display only: two cases can
    /// share it, so nothing compares it.
    pub fn display(&self) -> String {
        match self.classname.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => format!("{c}::{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The observation list items of the failing tests of a report set: one
/// item per test, its comparison key, which is the JSON array
/// `[<report>, <classname or null>, <name>, <occurrence>]` (`<occurrence>`
/// counting from 1 among equal report, classname and name). The encoding is
/// injective: two different tests, or two occurrences of one, never give
/// equal items, whatever the fields hold, and the before and after lists
/// are compared on these items alone. A field with a control or invisible
/// format character, or an item longer than [`MAX_ITEM_CHARS`] (the bound
/// held on the encoded form, the occurrence included), is the error,
/// naming the report, never the text; the items are sorted.
/// [`render_item`] gives the human text.
pub fn junit_items(mut tests: Vec<(String, FailingTest)>) -> Result<Vec<String>, String> {
    tests.sort();
    let mut out: Vec<String> = Vec::with_capacity(tests.len());
    let mut occurrence = 0u64;
    for (i, (report, test)) in tests.iter().enumerate() {
        occurrence = if i > 0 && tests[i - 1] == tests[i] {
            occurrence + 1
        } else {
            1
        };
        let fields = [
            Some(report.as_str()),
            test.classname.as_deref(),
            Some(test.name.as_str()),
        ];
        if fields.iter().flatten().any(|f| crate::text::has_hidden(f)) {
            return Err(format!(
                "{report}: a failing test's name contains a control or format character"
            ));
        }
        let item = encode_item(report, test, occurrence);
        if item.chars().count() > MAX_ITEM_CHARS {
            return Err(format!(
                "{report}: a failing test's name is longer than the {MAX_ITEM_CHARS} characters an item keeps"
            ));
        }
        out.push(checked_text(
            &item,
            &format!("{report}: a failing test's name"),
        )?);
    }
    out.sort();
    Ok(out)
}

/// The item of one failing test, the comparison key [`junit_items`]
/// describes.
pub fn encode_item(report: &str, test: &FailingTest, occurrence: u64) -> String {
    serde_json::json!([report, test.classname, test.name, occurrence]).to_string()
}

/// The human text of an item: `<report>: <classname>::<name>`, with
/// ` #<occurrence>` from the second occurrence on, for a JUnit item; any
/// other item (a `cargo test` one) as it is. Display only, never compared.
pub fn render_item(item: &str) -> String {
    let parsed: Option<(String, Option<String>, String, u64)> = serde_json::from_str(item).ok();
    match parsed {
        Some((report, classname, name, occurrence)) => {
            let shown = FailingTest { classname, name }.display();
            if occurrence > 1 {
                format!("{report}: {shown} #{occurrence}")
            } else {
                format!("{report}: {shown}")
            }
        }
        None => item.to_string(),
    }
}

/// One test case being read: its identity and whether it failed.
struct Case {
    name: String,
    classname: Option<String>,
    failed: bool,
}

impl Case {
    fn identity(self) -> FailingTest {
        FailingTest {
            classname: self.classname,
            name: self.name,
        }
    }
}

/// One open element: its name and, for a `<testcase>`, the case.
struct Open<'a> {
    name: &'a [u8],
    case: Option<Case>,
}

/// The one cursor every loop moves: the report, the position, and the
/// caller's check run every [`CHECK_EVERY`] bytes.
struct Parser<'a, 'c> {
    b: &'a [u8],
    /// The most failing identities this report may add; one more is the
    /// report's error at once, before anything is sorted.
    max_failing: usize,
    i: usize,
    doc_start: usize,
    next_check: usize,
    check: &'c mut dyn FnMut() -> Result<(), String>,
}

/// Whether `v` is a character XML allows (the `Char` production).
fn is_xml_char(v: u32) -> bool {
    matches!(v, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

impl<'a> Parser<'a, '_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn at(&self, s: &[u8]) -> bool {
        self.b[self.i.min(self.b.len())..].starts_with(s)
    }

    /// Moves `n` bytes on, running the caller's check at every
    /// [`CHECK_EVERY`] boundary it passes.
    fn advance(&mut self, n: usize) -> Result<(), String> {
        self.i += n;
        if self.i >= self.next_check {
            (self.check)()?;
            self.next_check = self.i.saturating_add(CHECK_EVERY);
        }
        Ok(())
    }

    /// Skips whitespace; whether there was any.
    fn skip_ws(&mut self) -> Result<bool, String> {
        let start = self.i;
        while self.peek().is_some_and(is_ws) {
            self.advance(1)?;
        }
        Ok(self.i > start)
    }

    /// Moves past the next `end`; `what` names the construct a report
    /// that ends first leaves open.
    fn skip_past(&mut self, end: &[u8], what: &str) -> Result<(), String> {
        loop {
            if self.i >= self.b.len() {
                return Err(format!("the JUnit report is cut short inside {what}"));
            }
            if self.at(end) {
                return self.advance(end.len());
            }
            self.advance(1)?;
        }
    }

    /// An element or attribute name: the bytes up to whitespace or a
    /// delimiter; an empty one is malformed.
    fn name(&mut self) -> Result<&'a [u8], String> {
        let b = self.b;
        let start = self.i;
        while let Some(c) = self.peek() {
            if is_ws(c) || matches!(c, b'/' | b'>' | b'=' | b'<' | b'"' | b'\'' | b'&' | b'?') {
                break;
            }
            self.advance(1)?;
        }
        if self.i == start {
            return Err(if self.i >= b.len() {
                CUT_SHORT.into()
            } else {
                "the JUnit report has a tag or attribute without a name".into()
            });
        }
        Ok(&b[start..self.i])
    }

    /// A comment, a processing instruction or (inside the root) a CDATA
    /// section at the cursor, skipped; `false` when none starts here. A
    /// DOCTYPE or any other declaration is refused: an entity declaration
    /// could change what a name means.
    fn misc(&mut self, in_root: bool) -> Result<bool, String> {
        if self.at(b"<!--") {
            self.advance(4)?;
            self.skip_past(b"-->", "a comment")?;
            return Ok(true);
        }
        if self.at(b"<![CDATA[") {
            if !in_root {
                return Err("the JUnit report has a CDATA section outside its root element".into());
            }
            self.advance(9)?;
            self.skip_past(b"]]>", "a CDATA section")?;
            return Ok(true);
        }
        if self.at(b"<?") {
            let at = self.i;
            self.advance(2)?;
            let target = self.name()?;
            let body = self.i;
            self.skip_past(b"?>", "a processing instruction")?;
            if target.eq_ignore_ascii_case(b"xml") {
                if at != self.doc_start {
                    return Err(
                        "the JUnit report has an XML declaration that is not at its start".into(),
                    );
                }
                check_encoding(&self.b[body..self.i - 2])?;
            }
            return Ok(true);
        }
        if self.at(b"<!DOCTYPE") {
            return Err("the JUnit report has a DOCTYPE, which apb does not read".into());
        }
        if self.at(b"<!") {
            return Err("the JUnit report has a declaration apb does not read".into());
        }
        Ok(false)
    }

    /// One entity or character reference at the `&` under the cursor, in
    /// one forward pass: the five XML names, or `&#<decimal>;` and
    /// `&#x<hex>;` with any number of leading zeros. Anything else, or a
    /// reference to a character XML does not allow, is an error.
    fn reference(&mut self) -> Result<char, String> {
        self.advance(1)?;
        if self.at(b"#") {
            self.advance(1)?;
            let radix = if self.at(b"x") {
                self.advance(1)?;
                16
            } else {
                10
            };
            let (mut value, mut digits) = (0u32, 0usize);
            loop {
                match self.peek() {
                    Some(b';') => {
                        self.advance(1)?;
                        break;
                    }
                    Some(c) => {
                        let Some(d) = (c as char).to_digit(radix) else {
                            return Err(
                                "the JUnit report has a malformed character reference".into()
                            );
                        };
                        digits += 1;
                        // Past the last character there is no going back
                        // under it; the value stays out of range.
                        value = value.saturating_mul(radix).saturating_add(d).min(0x11_0000);
                        self.advance(1)?;
                    }
                    None => return Err(CUT_SHORT.into()),
                }
            }
            if digits == 0 {
                return Err("the JUnit report has a malformed character reference".into());
            }
            return match char::from_u32(value).filter(|_| is_xml_char(value)) {
                Some(c) => Ok(c),
                None => Err("the JUnit report has a character reference out of range".into()),
            };
        }
        let start = self.i;
        loop {
            match self.peek() {
                Some(b';') => break,
                Some(c) if c.is_ascii_alphanumeric() => self.advance(1)?,
                Some(_) => return Err("the JUnit report has an & that starts no entity".into()),
                None => return Err(CUT_SHORT.into()),
            }
        }
        let name = &self.b[start..self.i];
        self.advance(1)?;
        match name {
            b"amp" => Ok('&'),
            b"lt" => Ok('<'),
            b"gt" => Ok('>'),
            b"quot" => Ok('"'),
            b"apos" => Ok('\''),
            _ => Err("the JUnit report has an entity apb does not know".into()),
        }
    }

    /// One attribute value from just after its opening quote, references
    /// decoded and literal whitespace normalized to a space as XML does.
    fn attr_value(&mut self, quote: u8) -> Result<String, String> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err("the JUnit report is cut short inside an attribute value".into());
                }
                Some(c) if c == quote => {
                    self.advance(1)?;
                    break;
                }
                Some(b'<') => {
                    return Err("the JUnit report has a < inside an attribute value".into());
                }
                Some(b'&') => {
                    let c = self.reference()?;
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                Some(b'\r') => {
                    out.push(b' ');
                    self.advance(if self.at(b"\r\n") { 2 } else { 1 })?;
                }
                Some(b'\t' | b'\n') => {
                    out.push(b' ');
                    self.advance(1)?;
                }
                Some(c) => {
                    out.push(c);
                    self.advance(1)?;
                }
            }
        }
        // Cut at ASCII quotes of input checked to be UTF-8, references
        // encoded whole: always UTF-8.
        String::from_utf8(out).map_err(|_| "the JUnit report is not UTF-8".into())
    }

    /// The attributes of a start tag from just after its name: `name` and
    /// `classname`, and whether the tag closes itself.
    fn attributes(&mut self) -> Result<(Option<String>, Option<String>, bool), String> {
        let (mut name, mut classname) = (None, None);
        // An attribute named twice is not well-formed XML (and would let a
        // later `name` replace an earlier one): the report is not read.
        let mut seen: std::collections::HashSet<&'a [u8]> = std::collections::HashSet::new();
        loop {
            let spaced = self.skip_ws()?;
            match self.peek() {
                None => return Err("the JUnit report is cut short inside a start tag".into()),
                Some(b'>') => {
                    self.advance(1)?;
                    return Ok((name, classname, false));
                }
                Some(b'/') => {
                    if !self.at(b"/>") {
                        return Err(if self.i + 1 >= self.b.len() {
                            "the JUnit report is cut short inside a start tag".into()
                        } else {
                            "the JUnit report has a malformed start tag".into()
                        });
                    }
                    self.advance(2)?;
                    return Ok((name, classname, true));
                }
                Some(_) if !spaced => {
                    return Err("the JUnit report has a malformed start tag".into());
                }
                Some(_) => {}
            }
            let attr = self.name()?;
            if !seen.insert(attr) {
                return Err("the JUnit report names an attribute twice in one start tag".into());
            }
            self.skip_ws()?;
            if self.peek() != Some(b'=') {
                return Err(match self.peek() {
                    None => "the JUnit report is cut short inside a start tag".into(),
                    Some(_) => "the JUnit report has an attribute without a value".into(),
                });
            }
            self.advance(1)?;
            self.skip_ws()?;
            let quote = match self.peek() {
                Some(q @ (b'"' | b'\'')) => q,
                None => return Err("the JUnit report is cut short inside a start tag".into()),
                Some(_) => {
                    return Err("the JUnit report has an attribute value without quotes".into());
                }
            };
            self.advance(1)?;
            let value = self.attr_value(quote)?;
            match attr {
                b"name" => name = Some(value),
                b"classname" => classname = Some(value),
                _ => {}
            }
        }
    }

    /// The root element and everything inside it, from the `<` of its
    /// start tag to just past its end: the failing identities and the
    /// number of test cases.
    fn root(&mut self) -> Result<(Vec<FailingTest>, usize), String> {
        let mut stack: Vec<Open<'a>> = Vec::new();
        let (mut failed, mut cases) = (Vec::new(), 0usize);
        loop {
            match self.peek() {
                None => return Err(CUT_SHORT.into()),
                Some(b'<') if self.at(b"</") => {
                    self.advance(2)?;
                    let name = self.name()?;
                    self.skip_ws()?;
                    match self.peek() {
                        Some(b'>') => self.advance(1)?,
                        None => return Err(CUT_SHORT.into()),
                        Some(_) => return Err("the JUnit report has a malformed end tag".into()),
                    }
                    let open = stack
                        .pop()
                        .ok_or("the JUnit report has an end tag without its start tag")?;
                    if open.name != name {
                        return Err("the JUnit report's elements are not closed in order".into());
                    }
                    if let Some(case) = open.case {
                        cases += 1;
                        if case.failed {
                            if failed.len() >= self.max_failing {
                                return Err(format!(
                                    "{TOO_MANY_FAILING}: more than {} in one report",
                                    self.max_failing
                                ));
                            }
                            let identity = case.identity();
                            // An identity no item can hold is refused here,
                            // so what the reader keeps is bounded: at most
                            // `max_failing` identities whose fields hold at
                            // most MAX_ITEM_CHARS characters together.
                            let chars = identity.name.chars().count()
                                + identity
                                    .classname
                                    .as_deref()
                                    .map_or(0, |c| c.chars().count());
                            if chars > MAX_ITEM_CHARS {
                                return Err(format!(
                                    "a failing test's name is longer than the {MAX_ITEM_CHARS} characters an item keeps"
                                ));
                            }
                            failed.push(identity);
                        }
                    }
                    if stack.is_empty() {
                        return Ok((failed, cases));
                    }
                }
                Some(b'<') => {
                    if !stack.is_empty() && self.misc(true)? {
                        continue;
                    }
                    self.advance(1)?;
                    let name = self.name()?;
                    let (attr_name, classname, self_closing) = self.attributes()?;
                    let is_case = name == b"testcase";
                    if is_case && stack.iter().any(|o| o.case.is_some()) {
                        return Err("the JUnit report has a testcase inside a testcase".into());
                    }
                    if (name == b"failure" || name == b"error")
                        && let Some(case) = stack.last_mut().and_then(|o| o.case.as_mut())
                    {
                        case.failed = true;
                    }
                    let case = if is_case {
                        let Some(n) = attr_name else {
                            return Err("a testcase has no name".into());
                        };
                        Some(Case {
                            name: n,
                            classname,
                            failed: false,
                        })
                    } else {
                        None
                    };
                    if self_closing {
                        // A self-closing case has no failure inside: passed.
                        cases += usize::from(case.is_some());
                        if stack.is_empty() {
                            return Ok((failed, cases));
                        }
                        continue;
                    }
                    if stack.len() >= MAX_DEPTH {
                        return Err(format!(
                            "the JUnit report nests deeper than {MAX_DEPTH} elements"
                        ));
                    }
                    stack.push(Open { name, case });
                }
                Some(b'&') => {
                    self.reference()?;
                }
                Some(_) => self.advance(1)?,
            }
        }
    }
}

/// The encoding an XML declaration's body names, when it names one: UTF-8
/// only.
fn check_encoding(body: &[u8]) -> Result<(), String> {
    let Some(at) = body.windows(8).position(|w| w == b"encoding") else {
        return Ok(());
    };
    let rest = &body[at + 8..];
    let rest = rest.trim_ascii_start();
    let malformed = || "the JUnit report has a malformed XML declaration".to_string();
    let rest = rest
        .strip_prefix(b"=")
        .ok_or_else(malformed)?
        .trim_ascii_start();
    let (&quote, rest) = rest.split_first().ok_or_else(malformed)?;
    if quote != b'"' && quote != b'\'' {
        return Err(malformed());
    }
    let end = rest
        .iter()
        .position(|c| *c == quote)
        .ok_or_else(malformed)?;
    let name = &rest[..end];
    if name.eq_ignore_ascii_case(b"utf-8") || name.eq_ignore_ascii_case(b"utf8") {
        Ok(())
    } else {
        Err("the JUnit report declares an encoding other than UTF-8".into())
    }
}

/// [`failed_tests_within`] with the whole observation's cap,
/// [`MAX_LIST_ITEMS`].
pub fn failed_tests(
    xml: &[u8],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Vec<FailingTest>, String> {
    failed_tests_within(xml, MAX_LIST_ITEMS, check)
}

/// Whether `xml` is UTF-8, checked chunk by chunk: `check` runs before
/// each chunk of `chunk` bytes after the first (the caller checks right
/// after the read, before any byte is looked at), and a multi-byte sequence the chunk end cuts is taken
/// whole into the next chunk.
fn validate_utf8(
    xml: &[u8],
    chunk: usize,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    debug_assert!(chunk >= 4, "a chunk holds every UTF-8 sequence");
    let not_utf8 = || "the JUnit report is not UTF-8".to_string();
    // A literal character XML does not allow (a NUL, most C0 controls,
    // U+FFFE, U+FFFF) is refused as its reference `&#0;` is.
    let xml_chars = |text: &str| -> Result<(), String> {
        if text.chars().all(|c| is_xml_char(u32::from(c))) {
            Ok(())
        } else {
            Err("the JUnit report holds a character XML does not allow".into())
        }
    };
    let mut start = 0usize;
    loop {
        if start > 0 {
            check()?;
        }
        if start >= xml.len() {
            return Ok(());
        }
        let end = start.saturating_add(chunk).min(xml.len());
        match std::str::from_utf8(&xml[start..end]) {
            Ok(text) => {
                xml_chars(text)?;
                start = end;
            }
            // A sequence the chunk end cut: the next chunk starts at it.
            Err(e) if e.error_len().is_none() && end < xml.len() && e.valid_up_to() > 0 => {
                let valid = start + e.valid_up_to();
                // Checked UTF-8 up to `valid` by `from_utf8` itself.
                xml_chars(std::str::from_utf8(&xml[start..valid]).map_err(|_| not_utf8())?)?;
                start = valid;
            }
            Err(_) => return Err(not_utf8()),
        }
        if start >= xml.len() {
            return Ok(());
        }
    }
}

/// The failing test cases of one JUnit XML report, each
/// `<classname>::<name>` (or `<name>` when `classname` is absent or
/// empty), sorted, when the report was understood completely (the module
/// doc); else why not. More than `max_failing` failing tests is the
/// error [`TOO_MANY_FAILING`] the moment the next one is read, before any
/// sort, so the reader keeps at most `max_failing` identities of at most
/// `MAX_ITEM_CHARS - 8` characters each. `check` runs right after the
/// read, every [`CHECK_EVERY`] bytes of the UTF-8 pass and of the scan,
/// and once before a list is returned; its error ends the read with that
/// error.
pub fn failed_tests_within(
    xml: &[u8],
    max_failing: usize,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Vec<FailingTest>, String> {
    // The check runs before anything of the content is looked at.
    check()?;
    if xml.starts_with(&[0xFE, 0xFF]) || xml.starts_with(&[0xFF, 0xFE]) {
        return Err("the JUnit report is UTF-16, not UTF-8".into());
    }
    validate_utf8(xml, CHECK_EVERY, check)?;
    let start = if xml.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    let mut p = Parser {
        b: xml,
        max_failing,
        i: start,
        doc_start: start,
        // The UTF-8 pass checked last before its final chunk.
        next_check: CHECK_EVERY,
        check,
    };
    // Before the root: whitespace, comments, processing instructions.
    loop {
        p.skip_ws()?;
        match p.peek() {
            None => return Err("the JUnit report has no root element".into()),
            Some(b'<') if p.misc(false)? => {}
            Some(b'<') => break,
            Some(_) => return Err("the JUnit report has text outside its root element".into()),
        }
    }
    let (mut names, cases) = p.root()?;
    // After the root: the same, nothing else.
    loop {
        p.skip_ws()?;
        match p.peek() {
            None => break,
            Some(b'<') if !p.at(b"<![CDATA[") && p.misc(false)? => {}
            Some(_) => return Err("the JUnit report has content after its root element".into()),
        }
    }
    (p.check)()?;
    if cases == 0 {
        return Err("the JUnit report lists no test case".into());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(body: &str) -> String {
        format!("<testsuite>{body}</testsuite>")
    }

    /// The reader with a check that never stops it.
    fn ft(xml: &str) -> Result<Vec<String>, String> {
        failed_tests(xml.as_bytes(), &mut || Ok(())).map(shown)
    }

    fn shown(tests: Vec<FailingTest>) -> Vec<String> {
        tests.iter().map(FailingTest::display).collect()
    }

    #[test]
    fn a_failure_and_an_error_fail_and_a_skip_passes() {
        let xml = report(
            r#"<testcase name="a"><failure message="x"/></testcase>
               <testcase name="b"><error message="y"/></testcase>
               <testcase name="c"><skipped/></testcase>"#,
        );
        assert_eq!(ft(&xml).unwrap(), ["a", "b"]);
    }

    #[test]
    fn a_self_closing_case_passes() {
        let xml = report(r#"<testcase name="a"/><testcase name="b"><failure/></testcase>"#);
        assert_eq!(ft(&xml).unwrap(), ["b"]);
        let xml = report(r#"<testcase name="a"/>"#);
        assert_eq!(ft(&xml).unwrap(), [""; 0]);
    }

    #[test]
    fn classname_and_name_join_with_two_colons() {
        let xml = report(r#"<testcase classname="suite" name="it_fails"><failure/></testcase>"#);
        assert_eq!(ft(&xml).unwrap(), ["suite::it_fails"]);
        let empty = report(r#"<testcase classname="" name="it_fails"><failure/></testcase>"#);
        assert_eq!(ft(&empty).unwrap(), ["it_fails"]);
    }

    /// Legal XML a runner writes reads as it means: nested suites, errors
    /// and failures, skipped cases, CDATA, entities, single quotes, a UTF-8
    /// byte order mark, whitespace in end tags, a processing instruction, a
    /// comment after the root.
    #[test]
    fn legal_xml_reads_as_it_means() {
        let table: &[(&str, &[&str])] = &[
            (
                r#"<testsuites><testsuite name="s1"><testcase name="a"><failure/></testcase></testsuite><testsuite name="s2"><testcase name="b"><error/></testcase><testcase name="c"/></testsuite></testsuites>"#,
                &["a", "b"],
            ),
            (
                r#"<testsuite><testcase name="a"><skipped/></testcase></testsuite>"#,
                &[],
            ),
            (
                r#"<testsuite><testcase name="a"><system-out><![CDATA[<failure/> </testcase>]]></system-out></testcase></testsuite>"#,
                &[],
            ),
            (
                r#"<testsuite><testcase name="a&amp;b &lt;&gt; &quot;&apos; &#65;&#x42;&#00000000000067;"><failure/></testcase></testsuite>"#,
                &["a&b <> \"' ABC"],
            ),
            (
                r#"<testsuite><testcase classname='m' name='a'><failure/></testcase></testsuite>"#,
                &["m::a"],
            ),
            (
                "\u{feff}<testsuite><testcase name=\"a\"><failure/></testcase></testsuite>",
                &["a"],
            ),
            (
                "<testsuite >\n  <testcase name=\"passes\"></testcase >\n  <testcase name=\"fails\"><failure/></testcase\t>\n</testsuite\n>",
                &["fails"],
            ),
            (
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<?xml-stylesheet href=\"x\"?><testsuite><?pi data?><testcase name=\"a\"><failure/></testcase></testsuite>",
                &["a"],
            ),
            (
                "<!-- before --><testsuite><testcase name=\"a\"><failure/></testcase></testsuite>\n<!-- after -->\n<?after?>\n",
                &["a"],
            ),
            (
                "<testsuite><testcase name=\"a\"><failure message=\"x\">trace with &lt;html&gt; and a > sign</failure></testcase></testsuite>",
                &["a"],
            ),
            // Whitespace inside a value is normalized as XML does.
            (
                "<testsuite><testcase name=\"a\tb\r\nc\"><failure/></testcase></testsuite>",
                &["a b c"],
            ),
        ];
        for (xml, want) in table {
            assert_eq!(ft(xml).unwrap(), *want, "{xml}");
        }
    }

    /// Anything the reader did not understand completely is unknown, each
    /// with its reason, never a partial or guessed list.
    #[test]
    fn what_the_reader_does_not_understand_is_unknown() {
        let table: &[(&[u8], &str)] = &[
            (br#"<testsuite><testcase name="a"><failure/></testcase>"#, "cut short"),
            (br#"<testsuite><testcase name="a"><failure/></testcase></testsuite"#, "cut short"),
            (b"<testsuite><testcase name=\"a\"/><!-- never closed", "inside a comment"),
            (b"<testsuite><testcase name=\"a\"/><![CDATA[ never", "inside a CDATA section"),
            (b"<testsuite><testcase name=\"a\"/><?pi never", "inside a processing instruction"),
            (b"<testsuite><testcase name=\"a", "inside an attribute value"),
            (b"<testsuite><testcase name=\"a\"", "inside a start tag"),
            (b"<!DOCTYPE suite [<!ENTITY x \"y\">]><testsuite><testcase name=\"a\"/></testsuite>", "DOCTYPE"),
            (b"<testsuite><!ENTITY x \"y\"><testcase name=\"a\"/></testsuite>", "declaration apb does not read"),
            (b"\xfe\xff\x00<\x00t", "UTF-16"),
            (b"\xff\xfe<\x00t\x00", "UTF-16"),
            (b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><testsuite><testcase name=\"a\"/></testsuite>", "encoding other than UTF-8"),
            (b"<testsuite><testcase name=\"a\xff\"/></testsuite>", "not UTF-8"),
            (b"<testsuite><testcase name=\"a\"></testsuite></testcase>", "not closed in order"),
            (b"<testsuite><testcase name=\"a\"/></testsuite><testsuite/>", "content after its root"),
            (b"<testsuite><testcase name=\"a\"/></testsuite>trailing", "content after its root"),
            (b"text<testsuite><testcase name=\"a\"/></testsuite>", "text outside its root"),
            (b"<testsuite><testcase name=\"x&nope;\"/></testsuite>", "entity apb does not know"),
            (b"<testsuite><testcase name=\"x&\"/></testsuite>", "starts no entity"),
            (b"<testsuite><testcase name=\"&#;\"/></testsuite>", "malformed character reference"),
            (b"<testsuite><testcase name=\"&#x1G;\"/></testsuite>", "malformed character reference"),
            (b"<testsuite><testcase name=\"&#0;\"/></testsuite>", "out of range"),
            (b"<testsuite><testcase name=\"&#x110000;\"/></testsuite>", "out of range"),
            (b"<testsuite><testcase name=\"&#xD800;\"/></testsuite>", "out of range"),
            (b"<testsuite><testcase name=\"&#99999999999999999999;\"/></testsuite>", "out of range"),
            (b"<testsuite>a &bogus; b<testcase name=\"a\"/></testsuite>", "entity apb does not know"),
            (b"<testsuite><testcase name=a/></testsuite>", "without quotes"),
            (b"<testsuite><testcase name=\"a<b\"/></testsuite>", "< inside an attribute value"),
            (b"<testsuite><testcase name=\"a\"><testcase name=\"b\"/></testcase></testsuite>", "testcase inside a testcase"),
            (b"<testsuite><testcase classname=\"s\"/></testsuite>", "no name"),
            (b"<testsuite></testsuite>", "no test case"),
            (b"", "no root element"),
            (b"  <!-- only a comment -->", "no root element"),
            (b"<?xml version=\"1.0\"?><?xml version=\"1.0\"?><testsuite/>", "not at its start"),
        ];
        for (xml, why) in table {
            let got = failed_tests(xml, &mut || Ok(()));
            match got {
                Err(reason) => assert!(
                    reason.contains(why),
                    "{}: {reason}",
                    String::from_utf8_lossy(xml)
                ),
                Ok(list) => panic!("{} read as {list:?}", String::from_utf8_lossy(xml)),
            }
        }
        let deep = format!(
            "{}{}",
            "<a>".repeat(MAX_DEPTH + 1),
            "</a>".repeat(MAX_DEPTH + 1)
        );
        assert!(ft(&deep).unwrap_err().contains("deeper than"));
    }

    /// The reviewer's example: an end tag with whitespace before \`>\`
    /// closes its case, so the failure is the next case's.
    #[test]
    fn whitespace_in_an_end_tag_closes_its_case() {
        let xml = "<testsuite>\n  <testcase name=\"passes\"></testcase >\n  <testcase name=\"fails\"><failure/></testcase>\n</testsuite>";
        assert_eq!(ft(xml).unwrap(), ["fails"]);
    }

    /// The reviewer's example: a report a runner stopped writing is
    /// unknown, never the cases it held so far.
    #[test]
    fn an_incomplete_report_is_unknown() {
        assert_eq!(
            ft(r#"<testsuite><testcase name="a"><failure/></testcase>"#).unwrap_err(),
            CUT_SHORT
        );
    }

    /// The reviewer's example: a reference with many leading zeros is the
    /// character it names.
    /// An attribute named twice in one start tag is not well-formed XML:
    /// the report is not read, so a later `name` never replaces an earlier
    /// one; one of each still reads.
    #[test]
    fn an_attribute_named_twice_is_refused() {
        let xml = report(r#"<testcase name="new" name="old"><failure/></testcase>"#);
        assert!(ft(&xml).unwrap_err().contains("attribute twice"));
        let xml = report(r#"<testcase classname="a" classname="b" name="n"><failure/></testcase>"#);
        assert!(ft(&xml).unwrap_err().contains("attribute twice"));
        let xml = report(r#"<testcase time="1" time="2" name="n"><failure/></testcase>"#);
        assert!(ft(&xml).unwrap_err().contains("attribute twice"));
        let xml = report(r#"<testcase classname="a" name="n" time="1"><failure/></testcase>"#);
        assert_eq!(ft(&xml).unwrap(), ["a::n"]);
    }

    /// A literal character XML does not allow is refused as its reference
    /// is, in any chunk of the UTF-8 pass; a tab, a newline and characters
    /// outside the BMP still read.
    #[test]
    fn a_literal_character_xml_does_not_allow_is_refused() {
        for bad in ["\u{0}", "\u{1}", "\u{1f}", "\u{fffe}", "\u{ffff}"] {
            let xml = report(&format!(
                r#"<testcase name="n"><failure>x{bad}y</failure></testcase>"#
            ));
            assert!(ft(&xml).unwrap_err().contains("does not allow"), "{bad:?}");
        }
        let xml = report("<testcase name=\"n\"><failure>\tx\n\u{1f600}</failure></testcase>");
        assert_eq!(ft(&xml).unwrap(), ["n"]);
        // Every chunk is checked, the one a cut sequence ends included.
        let mut xml = "<testsuite>".to_string();
        xml.push_str(&" ".repeat(10));
        xml.push('\u{e9}');
        xml.push('\u{0}');
        xml.push_str("<testcase name=\"n\"/></testsuite>");
        for chunk in [4usize, 5, 11, 12, 64] {
            let err = validate_utf8(xml.as_bytes(), chunk, &mut || Ok(())).unwrap_err();
            assert!(err.contains("does not allow"), "chunk {chunk}: {err}");
        }
    }

    #[test]
    fn a_reference_with_leading_zeros_is_its_character() {
        let xml = report(r#"<testcase name="&#00000000000065;"><failure/></testcase>"#);
        assert_eq!(ft(&xml).unwrap(), ["A"]);
        let xml = report(r#"<testcase name="&#x000000000041;"><failure/></testcase>"#);
        assert_eq!(ft(&xml).unwrap(), ["A"]);
    }

    /// The reviewer's example: a large comment after a complete suite, and
    /// a large attribute value, still meet the caller's check inside the
    /// scan itself (after the UTF-8 pass), and the check runs once more
    /// before a list is returned.
    #[test]
    fn the_check_runs_inside_every_large_construct() {
        let big = "x".repeat(3 * CHECK_EVERY);
        for xml in [
            format!(r#"<testsuite><testcase name="a"/></testsuite><!--{big}-->"#),
            format!(r#"<testsuite><testcase name="{big}"/></testsuite>"#),
            format!(r#"<testsuite><testcase name="a"/><![CDATA[{big}]]></testsuite>"#),
            format!(r#"<testsuite><testcase name="a"/>{big}</testsuite>"#),
            format!(r#"<testsuite><testcase name="a"/><?pi {big}?></testsuite>"#),
            format!(
                r#"<testsuite><testcase name="{}"/></testsuite>"#,
                "&amp;".repeat(CHECK_EVERY)
            ),
        ] {
            // The first check, then one before each UTF-8 chunk after the
            // first: what comes after those is the scanner's.
            let before_scan = xml.len().div_ceil(CHECK_EVERY);
            let mut calls = 0;
            failed_tests(xml.as_bytes(), &mut || {
                calls += 1;
                Ok(())
            })
            .unwrap();
            // At least one check inside the scan, besides the one before
            // the list is returned.
            assert!(calls >= before_scan + 2, "{calls}: {}", &xml[..40]);
            // A cancel at the scanner's first check ends the read there.
            let mut calls = 0;
            let got = failed_tests(xml.as_bytes(), &mut || {
                calls += 1;
                if calls > before_scan {
                    Err("cancelled".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(got.unwrap_err(), "cancelled", "{}", &xml[..40]);
            assert_eq!(calls, before_scan + 1);
        }
        // A small report: the first check and the one before returning.
        let mut calls = 0;
        let xml = report(r#"<testcase name="a"/>"#);
        failed_tests(xml.as_bytes(), &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 2);
        let mut calls = 0;
        let got = failed_tests(xml.as_bytes(), &mut || {
            calls += 1;
            if calls == 2 {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(got.unwrap_err(), "cancelled");
    }

    /// Text that is not ASCII, in every place a runner puts it, two-,
    /// three- and four-byte characters included, reads like ASCII text.
    #[test]
    fn non_ascii_text_anywhere_reads() {
        let xml = "<?xml version=\"1.0\"?><!-- caf\u{e9} \u{1f600} -->\
            <testsuite name=\"T\u{eb}st \u{2026} \u{1f600}\">\
            <testcase classname=\"m\u{f6}d\" name=\"t\u{e9}st \u{3b1}\u{1f600}\">\
            <failure message=\"expected \u{2019}x\u{2019}\">\u{e9}\u{1f600} diff</failure>\
            <system-out>\u{e9}\u{1f600}<![CDATA[\u{e9} <failure/> \u{1f600}]]></system-out>\
            </testcase>\
            <testcase name=\"ok\"><system-err>\u{fc}\u{1f600}</system-err></testcase>\
            </testsuite>";
        assert_eq!(ft(xml).unwrap(), ["m\u{f6}d::t\u{e9}st \u{3b1}\u{1f600}"]);
    }

    /// A small deterministic generator.
    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    /// A random balanced report: nested suites, cases that pass, fail, err
    /// or skip, with comments, CDATA and text, and the failing identities
    /// it holds.
    fn balanced(next: &mut impl FnMut() -> u64) -> (String, Vec<String>) {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<testsuites>");
        let mut want = Vec::new();
        for s in 0..(1 + next() % 3) {
            out.push_str(&format!("<testsuite name=\"s{s}\"><!-- \u{e9} -->"));
            for c in 0..(1 + next() % 4) {
                let name = format!("t{s}_{c}\u{1f600}");
                match next() % 5 {
                    0 => out.push_str(&format!("<testcase name=\"{name}\"/>")),
                    1 => {
                        out.push_str(&format!(
                            "<testcase classname='k' name=\"{name}\"><failure message=\"&lt;x&gt;\">trace</failure></testcase >"
                        ));
                        want.push(format!("k::{name}"));
                    }
                    2 => {
                        out.push_str(&format!("<testcase name=\"{name}\"><error/><system-out><![CDATA[</testcase>]]></system-out></testcase>"));
                        want.push(name);
                    }
                    3 => out.push_str(&format!("<testcase name=\"{name}\"><skipped/></testcase>")),
                    _ => out.push_str(&format!(
                        "<testcase name=\"{name}\">text &amp; more</testcase>"
                    )),
                }
            }
            out.push_str("</testsuite>");
        }
        out.push_str("</testsuites>");
        want.sort();
        (out, want)
    }

    /// Every byte prefix of a balanced report returns without panicking,
    /// and only the whole report gives a list: a list only for a balanced
    /// report. Generated balanced reports read their failures exactly, and
    /// generated byte strings of the scanner's markers, multi-byte
    /// characters and arbitrary bytes never panic and give a list only when
    /// they end with a closed root.
    #[test]
    fn a_list_only_for_a_balanced_report_and_no_byte_string_panics() {
        let mut next = rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..200 {
            let (xml, want) = balanced(&mut next);
            if want.is_empty() && !xml.contains("<testcase") {
                continue;
            }
            let mut got = ft(&xml).unwrap();
            got.sort();
            assert_eq!(got, want, "{xml}");
            let bytes = xml.as_bytes();
            for end in 0..bytes.len() {
                let got = failed_tests(&bytes[..end], &mut || Ok(()));
                assert!(got.is_err(), "prefix {end} of {xml} read as {got:?}");
            }
        }
        let pieces: &[&[u8]] = &[
            b"<testcase",
            b" name=\"",
            b"\"",
            b"'",
            b" classname='",
            b"=",
            b">",
            b"/>",
            b"</testcase>",
            b"</testcase >",
            b"<failure",
            b"<error",
            b"<!--",
            b"-->",
            b"<![CDATA[",
            b"]]>",
            b"<?",
            b"?>",
            b"<!DOCTYPE",
            b"&",
            b";",
            b"&#x",
            b"&#",
            b"amp;",
            b"<testsuite>",
            b"</testsuite>",
            "\u{e9}".as_bytes(),
            "\u{1f600}".as_bytes(),
            b"\xf0\x9f",
            b"\xff",
            b"\x80",
            b" ",
            b"a",
        ];
        for _ in 0..20_000 {
            let len = (next() % 40) as usize;
            let mut input: Vec<u8> = Vec::new();
            for _ in 0..len {
                let r = next();
                if r.is_multiple_of(5) {
                    input.push((r >> 8) as u8);
                } else {
                    input.extend_from_slice(pieces[(r >> 8) as usize % pieces.len()]);
                }
            }
            if failed_tests(&input, &mut || Ok(())).is_ok() {
                let end = input.trim_ascii_end();
                assert!(end.ends_with(b">"), "{}", String::from_utf8_lossy(&input));
            }
        }
    }

    fn case(classname: Option<&str>, name: &str) -> FailingTest {
        FailingTest {
            classname: classname.map(str::to_string),
            name: name.to_string(),
        }
    }

    /// The reviewer's examples: the classname "A::B" with the name "C" and
    /// the classname "A" with the name "B::C" are two keys, though both show
    /// as "A::B::C"; two failing "x" are not "x" and "x #2"; an absent
    /// classname is not an empty one.
    #[test]
    fn two_different_tests_never_share_a_key() {
        let r = "r/a.xml".to_string();
        let one = junit_items(vec![(r.clone(), case(Some("A::B"), "C"))]).unwrap();
        let two = junit_items(vec![(r.clone(), case(Some("A"), "B::C"))]).unwrap();
        assert_ne!(one, two);
        assert_eq!(
            render_item(&one[0]),
            render_item(&two[0]),
            "the display may agree"
        );
        let twice = junit_items(vec![
            (r.clone(), case(None, "x")),
            (r.clone(), case(None, "x")),
        ])
        .unwrap();
        let other = junit_items(vec![
            (r.clone(), case(None, "x")),
            (r.clone(), case(None, "x #2")),
        ])
        .unwrap();
        assert_ne!(twice, other);
        assert_eq!(render_item(&twice[1]), "r/a.xml: x #2");
        assert_ne!(
            junit_items(vec![(r.clone(), case(None, "x"))]).unwrap(),
            junit_items(vec![(r.clone(), case(Some(""), "x"))]).unwrap()
        );
        assert_eq!(
            twice,
            [
                encode_item(&r, &case(None, "x"), 1),
                encode_item(&r, &case(None, "x"), 2)
            ]
        );
    }

    /// Distinct (report, classname, name, occurrence) give distinct keys,
    /// over a generated set whose fields are made of the separators any
    /// joined text would use.
    #[test]
    fn distinct_fields_never_give_equal_keys() {
        let alphabet = [
            ":", "::", " ", "#", " #2", "\"", "\\", "[", "]", ",", "null", "a", "1",
        ];
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut word = |n: u64| -> String {
            (0..n)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect()
        };
        let mut seen: std::collections::HashMap<String, (String, Option<String>, String, u64)> =
            std::collections::HashMap::new();
        for i in 0..20_000u64 {
            let report = word(i % 3);
            let classname = (i % 4 != 0).then(|| word(i % 5));
            let name = word(1 + i % 4);
            let occurrence = 1 + i % 3;
            let key = encode_item(
                &report,
                &FailingTest {
                    classname: classname.clone(),
                    name: name.clone(),
                },
                occurrence,
            );
            let fields = (report, classname, name, occurrence);
            if let Some(earlier) = seen.insert(key.clone(), fields.clone()) {
                assert_eq!(earlier, fields, "two different tests share the key {key}");
            }
        }
    }

    /// A key too long for an item, its occurrence included, and a field
    /// with a hidden character are refused, the reason naming the report,
    /// never the text.
    #[test]
    fn a_key_is_never_cut() {
        let r = "reports/a.xml".to_string();
        let a = format!("{}A", "x".repeat(300));
        let why = junit_items(vec![(r.clone(), case(None, &a))]).unwrap_err();
        assert!(
            why.starts_with("reports/a.xml: ") && why.contains("longer than"),
            "{why}"
        );
        assert!(!why.contains("xxxx"), "{why}");
        let fits = "x".repeat(MAX_ITEM_CHARS - encode_item(&r, &case(None, ""), 1).chars().count());
        let items = junit_items(vec![(r.clone(), case(None, &fits))]).unwrap();
        assert_eq!(items[0].chars().count(), MAX_ITEM_CHARS);
        assert!(junit_items(vec![(r.clone(), case(None, &format!("{fits}x")))]).is_err());
        for hidden in ["a\u{1b}[31m", "a\nb", "a\u{202e}b", "a\u{200b}b"] {
            for test in [case(None, hidden), case(Some(hidden), "t")] {
                let why = junit_items(vec![(r.clone(), test)]).unwrap_err();
                assert!(why.contains("control or format character"), "{why}");
                assert!(
                    !why.contains(hidden),
                    "the reason never repeats the text: {why}"
                );
            }
        }
    }

    /// A cancel is seen before the content is looked at, the byte order
    /// mark included.
    #[test]
    fn a_cancel_is_seen_before_the_byte_order_mark() {
        for xml in [&b"\xfe\xff\x00<"[..], b"\xff\xfe<\x00", b"\xef\xbb\xbf<a/>"] {
            let mut calls = 0;
            let got = failed_tests(xml, &mut || {
                calls += 1;
                Err("cancelled".into())
            });
            assert_eq!(got.unwrap_err(), "cancelled");
            assert_eq!(calls, 1);
        }
    }

    /// A cancel set before the read is seen before any byte is scanned: a
    /// report that is not UTF-8 from its first byte says "cancelled", not
    /// "not UTF-8", with one call of the check.
    #[test]
    fn a_cancel_is_seen_before_the_utf8_pass() {
        let mut xml = vec![0xC3, 0x28];
        xml.extend_from_slice(report(&"x".repeat(4 * CHECK_EVERY)).as_bytes());
        let mut calls = 0;
        let got = failed_tests(&xml, &mut || {
            calls += 1;
            Err("cancelled".into())
        });
        assert_eq!(got.unwrap_err(), "cancelled");
        assert_eq!(calls, 1);
        // Without a cancel, the pass checks between its chunks.
        let mut calls = 0;
        let got = failed_tests(&xml, &mut || {
            calls += 1;
            Ok(())
        });
        assert_eq!(got.unwrap_err(), "the JUnit report is not UTF-8");
        assert_eq!(calls, 1);
        // A valid report: the pass checks before each of its chunks.
        let xml = report(&"x".repeat(4 * CHECK_EVERY));
        let mut calls = 0;
        failed_tests(xml.as_bytes(), &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap_err(); // no test case
        assert!(calls >= 5, "{calls}");
    }

    /// The chunked UTF-8 pass takes a sequence a chunk end cuts whole, at
    /// every split position of a two-, three- and four-byte sequence, and
    /// refuses an invalid sequence at a boundary and a sequence the input
    /// itself cuts.
    #[test]
    fn the_utf8_pass_handles_every_split_position() {
        for seq in ["é", "…", "😀"] {
            for chunk in 4..=8 {
                for lead in 0..=chunk {
                    let s = format!("{}{seq}b", "a".repeat(lead));
                    assert!(
                        validate_utf8(s.as_bytes(), chunk, &mut || Ok(())).is_ok(),
                        "{seq:?} after {lead} bytes, chunks of {chunk}"
                    );
                }
            }
        }
        for chunk in 4..=8 {
            for lead in 0..=chunk {
                for bad in [
                    &[0xE2, 0x28, 0xA1][..],
                    &[0xF0, 0x28, 0x8C, 0x28],
                    &[0xC3, 0x28],
                    &[0x80],
                ] {
                    let mut s = "a".repeat(lead).into_bytes();
                    s.extend_from_slice(bad);
                    s.extend_from_slice(b"bbbbbbbb");
                    assert!(
                        validate_utf8(&s, chunk, &mut || Ok(())).is_err(),
                        "{bad:?} after {lead}"
                    );
                }
                // A sequence the input ends inside.
                let mut s = "a".repeat(lead).into_bytes();
                s.extend_from_slice(&[0xF0, 0x9F, 0x98]);
                assert!(validate_utf8(&s, chunk, &mut || Ok(())).is_err());
            }
        }
    }

    /// A value of many references decodes in one pass.
    #[test]
    fn entity_decoding_is_linear() {
        // A passing case: the value is decoded and checked, never kept.
        let value = "&amp;".repeat(1024 * 1024);
        let xml = report(&format!(r#"<testcase name="{value}"/>"#));
        let started = std::time::Instant::now();
        assert_eq!(ft(&xml).unwrap(), [""; 0]);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }
}
