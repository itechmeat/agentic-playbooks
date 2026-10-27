//! The percent-encodings of a value placed in a URL: a query component or a
//! form value, and a path segment. Rendering and secret redaction share
//! them, so a redaction catches exactly the forms a request carried.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Every byte except the RFC 3986 unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`).
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// RFC 3986 pchar: unreserved, sub-delims, ':' and '@' stay literal.
const PCHAR: &AsciiSet = &COMPONENT
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=')
    .remove(b':')
    .remove(b'@');

/// `s` percent-encoded as a query component or form value.
pub fn encode(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// `s` percent-encoded as one URL path segment (pchar rules, so a ':' as in
/// a Telegram token stays literal).
pub fn encode_path_segment(s: &str) -> String {
    utf8_percent_encode(s, PCHAR).to_string()
}
