//! The one RFC 822 parse every mail read shares (D7): raw message bytes in,
//! the JSON view out. `get_message` builds its result on it, and
//! `save_attachments` reads the attachment list from the same parse, so both
//! agree on what a message contains and only one place knows the caps.

use mail_parser::MessageParser;
use serde_json::{Map, Value, json};

/// The per-part cap for a fetched message's `text`/`html` body (spec 3.3): a
/// larger part is cut to this many bytes and the result marked `truncated`.
pub(crate) const BODY_PART_CAP: usize = 262144;

/// Parses the raw RFC 822 bytes of a message into the JSON view (spec 3.3,
/// D7): from/to/cc/subject/date, the threading ids `message_id`,
/// `in_reply_to` and `references` (absent when the header is, angle brackets
/// kept so a reply can pass them straight back), the capped `text`/`html`
/// bodies, the attachment list, and the `truncated` flag. The caller adds
/// what the bytes do not carry (`uid`, `seen`).
pub(crate) fn message_json(raw: &[u8]) -> Map<String, Value> {
    let mut m = Map::new();
    let mut truncated = false;

    if let Some(msg) = MessageParser::default().parse(raw) {
        if let Some(from) = msg.from().and_then(mail_address_first) {
            m.insert("from".into(), json!(from));
        }
        if let Some(to) = msg.to().map(mail_address_join).filter(|s| !s.is_empty()) {
            m.insert("to".into(), json!(to));
        }
        if let Some(cc) = msg.cc().map(mail_address_join).filter(|s| !s.is_empty()) {
            m.insert("cc".into(), json!(cc));
        }
        if let Some(subject) = msg.subject() {
            m.insert("subject".into(), json!(subject));
        }
        if let Some(date) = msg.date() {
            m.insert("date".into(), json!(date.to_rfc3339()));
        }
        if let Some(id) = msg.message_id() {
            m.insert("message_id".into(), json!(format!("<{id}>")));
        }
        if let Some(list) = msg.in_reply_to().filter(|l| !l.is_empty()) {
            m.insert("in_reply_to".into(), json!(angle_join(list.iter())));
        }
        if let Some(list) = msg.references().filter(|l| !l.is_empty()) {
            m.insert("references".into(), json!(angle_join(list.iter())));
        }
        if let Some(text) = msg.body_text(0) {
            let (capped, cut) = cap_body(&text);
            truncated |= cut;
            m.insert("text".into(), json!(capped));
        }
        if let Some(html) = msg.body_html(0) {
            let (capped, cut) = cap_body(&html);
            truncated |= cut;
            m.insert("html".into(), json!(capped));
        }
        let attachments: Vec<Value> = msg
            .attachments()
            .map(|part| {
                json!({
                    "filename": part.attachment_name(),
                    "mime": mime_of(&part),
                    "size": part_size(&part),
                })
            })
            .collect();
        m.insert("attachments".into(), json!(attachments));
    } else {
        m.insert("attachments".into(), json!([]));
    }

    m.insert("truncated".into(), json!(truncated));
    m
}

/// Joins bare message ids as the header carries them: each wrapped in angle
/// brackets, space-separated (`<a@x> <b@x>`).
fn angle_join<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    ids.map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Caps a body part at [`BODY_PART_CAP`] bytes on a UTF-8 boundary, reporting
/// whether anything was cut (spec 3.3).
pub(crate) fn cap_body(s: &str) -> (String, bool) {
    if s.len() <= BODY_PART_CAP {
        return (s.to_string(), false);
    }
    let mut end = BODY_PART_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// The `type/subtype` MIME string of a message part, or `application/octet-stream`.
pub(crate) fn mime_of(part: &mail_parser::MessagePart<'_>) -> String {
    match part.content_type() {
        Some(ct) => match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_string(),
        },
        None => "application/octet-stream".to_string(),
    }
}

/// The size of an attachment part: the length of its text after charset
/// conversion for a text part, 0 for a multipart, otherwise the length of its
/// transfer-decoded body.
pub(crate) fn part_size(part: &mail_parser::MessagePart<'_>) -> usize {
    if part.is_multipart() {
        0
    } else if let Some(text) = part.text() {
        text.len()
    } else {
        part.decoded_len()
    }
}

/// The first address of a mail-parser address header, as its email string.
fn mail_address_first(addr: mail_parser::AddressList) -> Option<String> {
    addr.first()
        .and_then(|a| a.address())
        .map(|c| c.to_string())
}

/// All addresses of a mail-parser address header joined by `, `.
fn mail_address_join(addr: mail_parser::AddressList) -> String {
    addr.mailboxes()
        .filter_map(|a| a.address().map(|c| c.to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_body_truncates_large_part() {
        // A part just over the cap is cut and reported truncated; the returned
        // text is exactly the cap length (on a UTF-8 boundary of ASCII input).
        let big = "a".repeat(BODY_PART_CAP + 10);
        let (capped, truncated) = cap_body(&big);
        assert!(truncated, "an over-cap part must report truncated");
        assert_eq!(capped.len(), BODY_PART_CAP);
    }

    #[test]
    fn cap_body_keeps_small_part() {
        let (capped, truncated) = cap_body("hello");
        assert!(!truncated);
        assert_eq!(capped, "hello");
    }

    #[test]
    fn message_json_caps_large_text() {
        // A raw message with an over-cap text part parses and the whole
        // result is marked truncated.
        let big = "x".repeat(BODY_PART_CAP + 100);
        let raw = format!(
            "From: a@b.c\r\nTo: d@e.f\r\nSubject: Big\r\nContent-Type: text/plain\r\n\r\n{big}"
        );
        let body = Value::Object(message_json(raw.as_bytes()));
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(body["text"].as_str().unwrap().len(), BODY_PART_CAP);
    }

    #[test]
    fn message_json_reads_addresses_and_attachments() {
        // Groups are flattened into the address lists; an attachment reports
        // its name, its MIME type and the length of its decoded body.
        let raw = concat!(
            "From: Ann <ann@x.org>\r\n",
            "To: bob@x.org, team: cy@x.org, dee@x.org;\r\n",
            "Cc: eve@x.org\r\n",
            "Subject: Files\r\n",
            "Content-Type: multipart/mixed; boundary=\"b1\"\r\n",
            "\r\n",
            "--b1\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "body text\r\n",
            "--b1\r\n",
            "Content-Type: application/pdf; name=\"a.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"a.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "aGVsbG8=\r\n",
            "--b1\r\n",
            "Content-Type: text/plain; name=\"n.txt\"\r\n",
            "Content-Disposition: attachment; filename=\"n.txt\"\r\n",
            "\r\n",
            "hi there\r\n",
            "--b1--\r\n",
        );
        let body = Value::Object(message_json(raw.as_bytes()));
        assert_eq!(body["from"], json!("ann@x.org"));
        assert_eq!(body["to"], json!("bob@x.org, cy@x.org, dee@x.org"));
        assert_eq!(body["cc"], json!("eve@x.org"));
        assert_eq!(body["subject"], json!("Files"));
        assert_eq!(
            body["attachments"],
            json!([
                { "filename": "a.pdf", "mime": "application/pdf", "size": 5 },
                { "filename": "n.txt", "mime": "text/plain", "size": 8 },
            ])
        );
    }

    #[test]
    fn message_json_carries_the_threading_ids_with_brackets() {
        let raw = concat!(
            "From: a@x.org\r\n",
            "To: b@x.org\r\n",
            "Subject: Re: Thread\r\n",
            "Message-ID: <new@x.org>\r\n",
            "In-Reply-To: <orig@x.org>\r\n",
            "References: <first@x.org> <orig@x.org>\r\n",
            "\r\n",
            "reply body\r\n",
        );
        let body = Value::Object(message_json(raw.as_bytes()));
        assert_eq!(body["message_id"], json!("<new@x.org>"));
        assert_eq!(body["in_reply_to"], json!("<orig@x.org>"));
        assert_eq!(body["references"], json!("<first@x.org> <orig@x.org>"));
    }

    #[test]
    fn message_json_leaves_absent_threading_headers_out() {
        let raw = "From: a@x.org\r\nSubject: Plain\r\n\r\nbody\r\n";
        let map = message_json(raw.as_bytes());
        for absent in ["message_id", "in_reply_to", "references"] {
            assert!(!map.contains_key(absent), "{map:?}");
        }
    }

    #[test]
    fn part_decoded_gives_transfer_decoded_bytes() {
        let raw = concat!(
            "From: a@x.org\r\nSubject: Parts\r\n",
            "Content-Type: multipart/mixed; boundary=\"b1\"\r\n",
            "\r\n",
            "--b1\r\n",
            "Content-Type: text/plain; name=\"n.txt\"\r\n",
            "Content-Disposition: attachment; filename=\"n.txt\"\r\n",
            "\r\n",
            "hi there\r\n",
            "--b1\r\n",
            "Content-Type: application/pdf; name=\"a.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"a.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "aGVsbG8=\r\n",
            "--b1--\r\n",
        );
        let msg = mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .unwrap();
        let parts: Vec<_> = msg.attachments().collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(&parts[0].decoded()[..], b"hi there");
        assert_eq!(&parts[1].decoded()[..], b"hello");
    }
}
