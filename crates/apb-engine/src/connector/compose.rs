//! The e-mail composer the SMTP path, the IMAP drafts and the `mime`
//! request bodies share (D4): one render of a `SmtpMessage` into the
//! envelope, the formatted RFC 5322 bytes, the Message-ID and a
//! deterministic preview. The caller picks the behavior with
//! [`ComposeOpts`]; the label prefixes every error ("smtp" for the SMTP
//! path, "message" for the others).

use apb_core::connector::def::SmtpMessage;
use apb_core::connector::marker;
use apb_core::connector::template::{Namespace, RenderCtx, placeholders, render_raw};
use lettre::Address;
use lettre::message::{Mailbox, Message, MultiPart, SinglePart};
use serde_json::{Value, json};

use crate::connector::result::{CallError, CallErrorCode};

/// How the caller wants the message built.
#[derive(Clone, Debug, Default)]
pub(crate) struct ComposeOpts {
    /// Keep the `Bcc:` header in the formatted bytes. lettre always strips
    /// it (the envelope still delivers to every bcc recipient); a body
    /// that travels as data (`mime`, an IMAP draft) wants the header
    /// visible, an SMTP send does not.
    pub keep_bcc: bool,
    /// Set the `Message-ID` from this domain instead of letting lettre
    /// generate one: `<apb.{nonce}@{domain}>`, deterministic in shape.
    pub message_id_domain: Option<String>,
}

/// The composed message: what the SMTP envelope needs, what a `mime` body
/// carries, and what a dry-run shows.
#[derive(Debug)]
pub(crate) struct Composed {
    pub from: String,
    pub subject: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    /// Every envelope recipient: to, cc and bcc together.
    pub recipients: Vec<String>,
    pub envelope: lettre::address::Envelope,
    /// The RFC 5322 bytes. With `keep_bcc` the stripped `Bcc:` header is
    /// put back before the body.
    pub formatted: Vec<u8>,
    /// The generated Message-ID when `message_id_domain` was set.
    #[allow(dead_code)]
    pub message_id: Option<String>,
    /// The readable view a dry-run or a contract case shows: every present
    /// field of the message, absent ones left out.
    pub preview: Value,
}

/// Builds a render error naming the label and the field.
fn render_err(label: &str, field: &str, err: impl std::fmt::Display) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!("{label} {field} render failed: {err}"),
    )
}

/// Renders one OPTIONAL message field leniently (cross-slice obligation 4).
/// An unresolved `{{args.*}}`/`{{account.*}}` placeholder means "field
/// absent", not an error; a structural render error still fails loudly.
fn render_optional(
    label: &str,
    field: &str,
    template: &str,
    ctx: &RenderCtx,
) -> Result<Option<String>, CallError> {
    for (ns, name) in placeholders(template).map_err(|e| render_err(label, field, e))? {
        let absent = match ns {
            Namespace::Args => !name.is_empty() && ctx.args.get(name.as_str()).is_none(),
            Namespace::Account => !ctx.account.contains_key(name.as_str()),
            _ => false,
        };
        if absent {
            return Ok(None);
        }
    }
    let value = render_raw(template, ctx).map_err(|e| render_err(label, field, e))?;
    Ok(if value.is_empty() { None } else { Some(value) })
}

/// Renders and parses a REQUIRED comma-separated address list into lettre
/// mailboxes.
fn render_addresses(
    label: &str,
    field: &str,
    template: &str,
    ctx: &RenderCtx,
) -> Result<Vec<Mailbox>, CallError> {
    let rendered = render_raw(template, ctx).map_err(|e| render_err(label, field, e))?;
    parse_addresses(label, field, &rendered)
}

/// Renders one OPTIONAL address list (cc/bcc) leniently; an absent arg
/// yields an empty list.
fn render_optional_addresses(
    label: &str,
    field: &str,
    template: Option<&str>,
    ctx: &RenderCtx,
) -> Result<Vec<Mailbox>, CallError> {
    match template {
        Some(t) => match render_optional(label, field, t, ctx)? {
            Some(rendered) => parse_addresses(label, field, &rendered),
            None => Ok(Vec::new()),
        },
        None => Ok(Vec::new()),
    }
}

/// Parses a comma-separated rendered address list into mailboxes.
fn parse_addresses(label: &str, field: &str, rendered: &str) -> Result<Vec<Mailbox>, CallError> {
    let mut out = Vec::new();
    for token in rendered.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let mb = token.parse::<Mailbox>().map_err(|_| {
            CallError::new(
                CallErrorCode::InvalidArgs,
                format!("{label} {field} address is not valid: `{token}`"),
            )
        })?;
        out.push(mb);
    }
    Ok(out)
}

/// Renders and builds the message: from mailbox, recipient mailboxes,
/// subject, the threading headers, and the multipart or single body. At
/// least one of `body_text` or `body_html` must resolve (`invalid_args`
/// otherwise); an empty `to` list is `invalid_args`.
pub(crate) fn compose(
    label: &str,
    msg: &SmtpMessage,
    ctx: &RenderCtx,
    opts: &ComposeOpts,
) -> Result<Composed, CallError> {
    let from_email =
        render_raw(&msg.from_email, ctx).map_err(|e| render_err(label, "from_email", e))?;
    let from_name = match &msg.from_name {
        Some(t) => render_optional(label, "from_name", t, ctx)?,
        None => None,
    };
    let from_addr = from_email.parse::<Address>().map_err(|_| {
        CallError::new(
            CallErrorCode::InvalidArgs,
            format!("{label} from_email is not valid: `{from_email}`"),
        )
    })?;
    let from_mb = Mailbox::new(from_name, from_addr);

    let subject = render_raw(&msg.subject, ctx).map_err(|e| render_err(label, "subject", e))?;
    let to = render_addresses(label, "to", &msg.to, ctx)?;
    let cc = render_optional_addresses(label, "cc", msg.cc.as_deref(), ctx)?;
    let bcc = render_optional_addresses(label, "bcc", msg.bcc.as_deref(), ctx)?;
    if to.is_empty() {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            format!("{label} `to` has no recipients"),
        ));
    }
    let in_reply_to = match &msg.in_reply_to {
        Some(t) => render_optional(label, "in_reply_to", t, ctx)?,
        None => None,
    };
    let references = match &msg.references {
        Some(t) => render_optional(label, "references", t, ctx)?,
        None => None,
    };

    let body_text = match &msg.body_text {
        Some(t) => render_optional(label, "body_text", t, ctx)?,
        None => None,
    };
    let body_html = match &msg.body_html {
        Some(t) => render_optional(label, "body_html", t, ctx)?,
        None => None,
    };

    let mut generated_id: Option<String> = None;
    let mut builder = Message::builder().from(from_mb.clone()).subject(&subject);
    for mb in &to {
        builder = builder.to(mb.clone());
    }
    for mb in &cc {
        builder = builder.cc(mb.clone());
    }
    for mb in &bcc {
        builder = builder.bcc(mb.clone());
    }
    if let Some(mid) = &opts.message_id_domain {
        generated_id = Some(format!("apb.{}@{mid}", marker::new_nonce()));
        builder = builder.message_id(Some(generated_id.clone().unwrap()));
    }
    if let Some(irt) = &in_reply_to {
        builder = builder.in_reply_to(irt.clone());
    }
    if let Some(references) = &references {
        builder = builder.references(references.clone());
    }

    let email = match (body_text, body_html) {
        (Some(t), Some(h)) => builder.multipart(MultiPart::alternative_plain_html(t, h)),
        (Some(t), None) => builder.singlepart(SinglePart::plain(t)),
        (None, Some(h)) => builder.singlepart(SinglePart::html(h)),
        (None, None) => {
            return Err(CallError::new(
                CallErrorCode::InvalidArgs,
                format!("{label} message needs body_text or body_html"),
            ));
        }
    }
    .map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("{label} message build failed: {e}"),
        )
    })?;

    // lettre strips the Bcc header from the formatted bytes (the envelope
    // keeps every bcc recipient); a body that travels as data puts the
    // header back before the body.
    let mut formatted = email.formatted();
    if opts.keep_bcc && !bcc.is_empty() {
        let line = format!(
            "Bcc: {}\r\n",
            bcc.iter()
                .map(|mb| mb.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(at) = find_header_end(&formatted) {
            let mut out = Vec::with_capacity(formatted.len() + line.len());
            out.extend_from_slice(&formatted[..at]);
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(&formatted[at..]);
            formatted = out;
        }
    }

    let to_strings: Vec<String> = to.iter().map(|mb| mb.email.to_string()).collect();
    let cc_strings: Vec<String> = cc.iter().map(|mb| mb.email.to_string()).collect();
    let bcc_strings: Vec<String> = bcc.iter().map(|mb| mb.email.to_string()).collect();
    let recipients: Vec<String> = to_strings
        .iter()
        .chain(cc_strings.iter())
        .chain(bcc_strings.iter())
        .cloned()
        .collect();
    let envelope = email.envelope().clone();

    // The deterministic preview: every present field, absent ones left
    // out. Not a parse of the formatted bytes; a structured view of the
    // same rendered values.
    let mut preview = json!({
        "from": from_mb.to_string(),
        "to": to_strings,
        "subject": subject,
    });
    let map = preview.as_object_mut().unwrap();
    if !cc_strings.is_empty() {
        map.insert("cc".to_string(), json!(cc_strings));
    }
    if !bcc_strings.is_empty() {
        map.insert("bcc".to_string(), json!(bcc_strings));
    }
    if let Some(irt) = in_reply_to {
        map.insert("in_reply_to".to_string(), json!(irt));
    }
    if let Some(references) = references {
        map.insert("references".to_string(), json!(references));
    }
    if let Some(t) = &msg.body_text
        && let Some(v) = render_optional(label, "body_text", t, ctx)?
    {
        map.insert("body_text".to_string(), json!(v));
    }
    if let Some(h) = &msg.body_html
        && let Some(v) = render_optional(label, "body_html", h, ctx)?
    {
        map.insert("body_html".to_string(), json!(v));
    }

    Ok(Composed {
        from: from_mb.email.to_string(),
        subject,
        to: to_strings,
        cc: cc_strings,
        bcc: bcc_strings,
        recipients,
        envelope,
        formatted,
        message_id: generated_id,
        preview,
    })
}

/// The offset just after the last header line (before the empty line that
/// ends the header block), so a header inserted there stays a header.
fn find_header_end(bytes: &[u8]) -> Option<usize> {
    for at in 0..bytes.len() {
        if bytes[at..].starts_with(b"\r\n\r\n") {
            return Some(at + 2);
        }
        if bytes[at..].starts_with(b"\n\n") {
            return Some(at + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx() -> RenderCtx<'static> {
        RenderCtx {
            account: &*Box::leak(Box::new(BTreeMap::from([
                ("from_email".to_string(), "agent@acme.example".to_string()),
                ("from_name".to_string(), "The Agent".to_string()),
            ]))),
            args: &*Box::leak(Box::new(json!({
                "to": "peer@example.org",
                "subject": "Plan review",
                "body_text": "Please look at the plan.",
                "cc": "watcher@example.org",
                "bcc": "quiet@example.org",
                "in_reply_to": "<orig@example.org>",
                "references": "<first@example.org> <orig@example.org>",
            }))),
            secrets: &*Box::leak(Box::new(BTreeMap::new())),
        }
    }

    fn message() -> SmtpMessage {
        SmtpMessage {
            from_email: "{{account.from_email}}".to_string(),
            from_name: Some("{{account.from_name}}".to_string()),
            to: "{{args.to}}".to_string(),
            cc: Some("{{args.cc}}".to_string()),
            bcc: Some("{{args.bcc}}".to_string()),
            subject: "{{args.subject}}".to_string(),
            in_reply_to: Some("{{args.in_reply_to}}".to_string()),
            references: Some("{{args.references}}".to_string()),
            body_text: Some("{{args.body_text}}".to_string()),
            body_html: None,
        }
    }

    fn text(composed: &Composed) -> String {
        String::from_utf8_lossy(&composed.formatted).into_owned()
    }

    #[test]
    fn the_threading_headers_reach_the_message() {
        let composed = compose("smtp", &message(), &ctx(), &ComposeOpts::default()).unwrap();
        let t = text(&composed);
        assert!(t.contains("In-Reply-To: <orig@example.org>"), "{t}");
        assert!(t.contains("References: <first@example.org>"), "{t}");
    }

    #[test]
    fn keep_bcc_keeps_the_header_and_the_default_drops_it() {
        let composed = compose(
            "smtp",
            &message(),
            &ctx(),
            &ComposeOpts {
                keep_bcc: true,
                message_id_domain: None,
            },
        )
        .unwrap();
        assert!(text(&composed).contains("Bcc:"), "{}", text(&composed));
        // The envelope delivered to every bcc recipient either way.
        assert_eq!(composed.bcc, vec!["quiet@example.org".to_string()]);

        let composed = compose("smtp", &message(), &ctx(), &ComposeOpts::default()).unwrap();
        assert!(!text(&composed).contains("Bcc:"), "{}", text(&composed));
        assert_eq!(composed.bcc, vec!["quiet@example.org".to_string()]);
        assert!(
            composed
                .recipients
                .contains(&"quiet@example.org".to_string())
        );
    }

    #[test]
    fn a_kept_bcc_header_sits_in_the_header_block_not_the_body() {
        let composed = compose(
            "smtp",
            &message(),
            &ctx(),
            &ComposeOpts {
                keep_bcc: true,
                message_id_domain: None,
            },
        )
        .unwrap();
        let t = text(&composed);
        let (headers, body) = t.split_once("\r\n\r\n").expect("a header block");
        assert!(
            headers.lines().any(|l| l == "Bcc: quiet@example.org"),
            "{t}"
        );
        assert!(!body.contains("Bcc"), "{t}");
        assert!(body.starts_with("Please look at the plan."), "{t}");
    }

    #[test]
    fn the_message_id_domain_comes_from_the_caller() {
        let composed = compose(
            "smtp",
            &message(),
            &ctx(),
            &ComposeOpts {
                keep_bcc: false,
                message_id_domain: Some("acme.example".to_string()),
            },
        )
        .unwrap();
        let id = composed.message_id.clone().unwrap();
        assert!(id.starts_with("apb."), "{id}");
        assert!(id.ends_with("@acme.example"), "{id}");
        assert!(text(&composed).contains(&id), "{}", text(&composed));
    }

    #[test]
    fn the_preview_leaves_absent_fields_out() {
        let mut msg = message();
        msg.cc = None;
        msg.bcc = None;
        msg.in_reply_to = None;
        msg.references = None;
        msg.body_html = None;
        let composed = compose("smtp", &msg, &ctx(), &ComposeOpts::default()).unwrap();
        let map = composed.preview.as_object().unwrap();
        for absent in ["in_reply_to", "references", "body_html"] {
            assert!(!map.contains_key(absent), "{map:?}");
        }
        assert_eq!(map["subject"], json!("Plan review"));
        assert_eq!(map["body_text"], json!("Please look at the plan."));
        // A template whose arg is absent drops the field from the preview.
        msg.cc = Some("{{args.cc}}".to_string());
        let mut args_only_some = ctx();
        let args = json!({
            "to": "peer@example.org",
            "subject": "Plan review",
            "body_text": "Please look at the plan.",
        });
        args_only_some.args = &*Box::leak(Box::new(args));
        let composed = compose("smtp", &msg, &args_only_some, &ComposeOpts::default()).unwrap();
        let map = composed.preview.as_object().unwrap();
        assert!(!map.contains_key("cc"), "{map:?}");
    }
}
