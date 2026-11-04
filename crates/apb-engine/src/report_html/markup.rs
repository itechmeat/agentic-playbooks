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

    /// `<tag class="class" name="value">body</tag>` for the SVG the graph
    /// draws: tag, class and attribute names are fixed names, and every
    /// value must be one the report built from numbers and fixed tokens
    /// (a coordinate, a marker reference). A class or value that carries a
    /// character a quoted attribute cannot hold is dropped, so nothing from
    /// the run can reach an attribute; text reaches the body only through
    /// [`Markup::text`].
    pub fn svg(tag: &'static str, class: Option<&str>, attrs: &[SvgAttr], body: Markup) -> Self {
        let mut out = format!("<{tag}");
        if let Some(class) = class
            && !attr_hostile(class)
        {
            out.push_str(&format!(" class=\"{class}\""));
        }
        for SvgAttr(name, value) in attrs {
            if attr_hostile(value) {
                continue;
            }
            out.push_str(&format!(" {name}=\"{value}\""));
        }
        out.push('>');
        out.push_str(&body.0);
        out.push_str(&format!("</{tag}>"));
        Markup(out)
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

/// One attribute of [`Markup::svg`]: a fixed name over a value the report
/// built itself.
#[derive(Debug, Clone)]
pub struct SvgAttr(pub &'static str, pub String);

/// Whether `text` carries a character a quoted HTML attribute cannot hold.
fn attr_hostile(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, '&' | '<' | '>' | '"' | '\''))
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
