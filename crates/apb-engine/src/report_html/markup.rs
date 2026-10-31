//! The only way text gets into the report: [`Markup`] holds HTML built from
//! fixed tag and class names and text that went through [`escape`]. No
//! constructor takes markup from a string, so a journal field, a playbook
//! name or an answer can never become a tag, an attribute or a script.

/// A piece of the report's HTML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markup(String);

impl Markup {
    /// `text`, escaped.
    pub fn text(text: &str) -> Self {
        Markup(escape(text))
    }

    /// `<tag class="class">body</tag>`; tag and class are fixed names.
    pub fn el(tag: &'static str, class: Option<&'static str>, body: Markup) -> Self {
        let class = class.map(|c| format!(" class=\"{c}\"")).unwrap_or_default();
        Markup(format!("<{tag}{class}>{}</{tag}>", body.0))
    }

    /// A link to `url`, labelled `label`.
    pub fn link(url: &SafeUrl, label: &str) -> Self {
        Markup(format!(
            "<a href=\"{}\" rel=\"noopener noreferrer\">{}</a>",
            escape(&url.0),
            escape(label)
        ))
    }

    /// The pieces one after another.
    pub fn concat(parts: impl IntoIterator<Item = Markup>) -> Self {
        Markup(parts.into_iter().map(|m| m.0).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// `text` with the five characters HTML treats specially replaced, so it
/// reads as text in an element and inside a quoted attribute alike.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// An absolute `http` or `https` URL: the only kind of link the report
/// carries, so no `javascript:` or `data:` URL can ever be one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeUrl(String);

impl SafeUrl {
    pub fn parse(url: &str) -> Option<Self> {
        let lower = url.to_ascii_lowercase();
        let ok = (lower.starts_with("https://") || lower.starts_with("http://"))
            && !url.chars().any(|c| c.is_whitespace() || c.is_control());
        ok.then(|| SafeUrl(url.to_string()))
    }
}

/// The Content Security Policy of a report whose only stylesheet is
/// `stylesheet`: nothing loads, nothing runs, and only that exact
/// stylesheet applies.
pub fn csp(stylesheet: &str) -> String {
    format!(
        "default-src 'none'; style-src 'sha256-{}'; base-uri 'none'; form-action 'none'",
        apb_core::content::sha256_base64(stylesheet.as_bytes())
    )
}
