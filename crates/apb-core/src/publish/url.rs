//! The push URL a publish node pins. Only `https`, `ssh` (including the
//! scp-like `host:path`) and `file` are pushed to, and a URL carrying
//! credentials is refused: a token in it would sit in the process argv and
//! in the journal. So is an `https` URL with a query or a fragment (a
//! signed URL carries its token there), user info with a `:` in a scp-like
//! URL, and a host or user led by `-`. A URL that passes is therefore safe
//! to journal and show.

use std::fmt;

/// The transport a push URL uses; exactly this one is allowed for the push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Https,
    Ssh,
    File,
}

impl Scheme {
    /// The name git's `protocol.<name>.allow` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Https => "https",
            Scheme::Ssh => "ssh",
            Scheme::File => "file",
        }
    }
}

/// Why a URL is not pushed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlRefusal {
    /// Empty, a control character, whitespace, or a leading `-`.
    Malformed,
    /// User info that can carry a token, or a credential-shaped part.
    Credentials,
    /// `http`, `git://`, a remote helper (`ext::`) or another scheme.
    Scheme(String),
    /// A relative local path.
    RelativePath,
}

impl fmt::Display for UrlRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlRefusal::Malformed => write!(f, "the URL is malformed"),
            UrlRefusal::Credentials => write!(f, "the URL carries credentials"),
            UrlRefusal::Scheme(s) => write!(f, "the scheme `{s}` is not pushed to"),
            UrlRefusal::RelativePath => write!(f, "a relative path is not pushed to"),
        }
    }
}

/// A push URL that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushUrl {
    text: String,
    scheme: Scheme,
}

impl PushUrl {
    /// Parses and checks `text` exactly as git would read it.
    pub fn parse(text: &str) -> Result<PushUrl, UrlRefusal> {
        if text.is_empty()
            || text.starts_with('-')
            || text.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(UrlRefusal::Malformed);
        }
        if crate::token_shape::looks_like_credential(text) {
            return Err(UrlRefusal::Credentials);
        }
        let scheme = classify(text)?;
        Ok(PushUrl {
            text: text.to_string(),
            scheme,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The local path a `file` URL names, percent-decoded as git reads a
    /// `file://` URL; `None` for any other scheme.
    pub fn local_path(&self) -> Option<std::path::PathBuf> {
        if self.scheme != Scheme::File {
            return None;
        }
        let rest = match self.text.split_once("://") {
            Some((_, rest)) => percent_encoding::percent_decode_str(rest)
                .decode_utf8_lossy()
                .into_owned(),
            None => self.text.clone(),
        };
        Some(std::path::PathBuf::from(rest))
    }
}

/// The scheme of `text`, refusing user info that can carry a secret.
fn classify(text: &str) -> Result<Scheme, UrlRefusal> {
    // A remote helper: `<transport>::<address>` (`ext::`, `fd::`).
    if let Some((head, _)) = text.split_once("::")
        && !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    {
        return Err(UrlRefusal::Scheme(head.to_ascii_lowercase()));
    }
    if let Some((scheme, rest)) = text.split_once("://") {
        let authority = rest.split('/').next().unwrap_or("");
        let userinfo = authority.rsplit_once('@').map(|(u, _)| u);
        return match scheme.to_ascii_lowercase().as_str() {
            // Any user info over https may be a token (`https://<token>@`),
            // and so may a query or a fragment (a signed URL).
            "https" if userinfo.is_some() || rest.contains(['?', '#']) => {
                Err(UrlRefusal::Credentials)
            }
            "https" if authority.is_empty() => Err(UrlRefusal::Malformed),
            "https" => Ok(Scheme::Https),
            "ssh" if userinfo.is_some_and(|u| u.contains(':')) => Err(UrlRefusal::Credentials),
            "ssh" if authority.is_empty() || !host_ok(authority) => Err(UrlRefusal::Malformed),
            "ssh" => Ok(Scheme::Ssh),
            // git reads everything after `file://` as the path, so
            // `file://localhost/r` is the relative path `localhost/r`.
            "file" if !rest.starts_with('/') => Err(UrlRefusal::RelativePath),
            "file" => Ok(Scheme::File),
            other => Err(UrlRefusal::Scheme(other.to_string())),
        };
    }
    if is_local_path(text) {
        return if is_absolute(text) {
            Ok(Scheme::File)
        } else {
            Err(UrlRefusal::RelativePath)
        };
    }
    // scp-like `[user@]host:path`: a colon before any slash. git splits at
    // the first colon, so `user:secret@host:path` would contact the host
    // `user` and carry `secret` in the path: a `@` after a colon before the
    // first slash is user info with a password. Conservative on purpose: a
    // path like `host:team@org.git` reads the same way and is refused too
    // (write it as `ssh://host/team@org.git`).
    let head = text.split('/').next().unwrap_or(text);
    if let Some(colon) = head.find(':')
        && head[colon..].contains('@')
    {
        return Err(UrlRefusal::Credentials);
    }
    let (host_part, path) = text.split_once(':').ok_or(UrlRefusal::Malformed)?;
    let host = host_part.rsplit_once('@').map_or(host_part, |(_, h)| h);
    if host.is_empty() || path.is_empty() || !host_ok(host_part) {
        return Err(UrlRefusal::Malformed);
    }
    if host_part
        .rsplit_once('@')
        .is_some_and(|(u, _)| u.is_empty())
    {
        return Err(UrlRefusal::Malformed);
    }
    Ok(Scheme::Ssh)
}

/// Whether an ssh authority (`[user@]host[:port]`) has neither a host nor a
/// user led by `-`, which ssh would read as an option.
fn host_ok(authority: &str) -> bool {
    let (user, host) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(u, h)| (Some(u), h));
    !host.starts_with('-') && !user.is_some_and(|u| u.starts_with('-'))
}

/// Whether git reads `text` as a local path rather than scp-like: no colon,
/// or a slash before the first colon, or a Windows drive.
fn is_local_path(text: &str) -> bool {
    match text.find(':') {
        None => true,
        Some(colon) => text[..colon].contains('/') || is_windows_drive(text),
    }
}

fn is_windows_drive(text: &str) -> bool {
    let b = text.as_bytes();
    cfg!(windows)
        && b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && matches!(b[2], b'/' | b'\\')
}

fn is_absolute(text: &str) -> bool {
    text.starts_with('/') || is_windows_drive(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme(t: &str) -> Result<Scheme, UrlRefusal> {
        PushUrl::parse(t).map(|u| u.scheme())
    }

    #[test]
    fn allowed_schemes() {
        assert_eq!(scheme("https://github.com/o/r.git"), Ok(Scheme::Https));
        assert_eq!(scheme("ssh://git@github.com/o/r.git"), Ok(Scheme::Ssh));
        assert_eq!(scheme("ssh://host:22/o/r"), Ok(Scheme::Ssh));
        assert_eq!(scheme("git@github.com:o/r.git"), Ok(Scheme::Ssh));
        assert_eq!(scheme("host:r.git"), Ok(Scheme::Ssh));
        assert_eq!(scheme("file:///srv/r.git"), Ok(Scheme::File));
        assert_eq!(scheme("/srv/r.git"), Ok(Scheme::File));
    }

    #[test]
    fn a_file_url_names_its_decoded_local_path() {
        let path = |t: &str| PushUrl::parse(t).unwrap().local_path();
        assert_eq!(path("file:///srv/a%20b.git"), Some("/srv/a b.git".into()));
        assert_eq!(path("/srv/r.git"), Some("/srv/r.git".into()));
        assert_eq!(path("ssh://h/r.git"), None);
    }

    #[test]
    fn refused_schemes() {
        assert_eq!(
            scheme("http://example.com/r.git"),
            Err(UrlRefusal::Scheme("http".into()))
        );
        assert_eq!(
            scheme("git://example.com/r.git"),
            Err(UrlRefusal::Scheme("git".into()))
        );
        assert_eq!(
            scheme("ext::sh -c touch% /tmp/x"),
            Err(UrlRefusal::Malformed)
        );
        assert_eq!(
            scheme("ext::ssh%20h"),
            Err(UrlRefusal::Scheme("ext".into()))
        );
        assert_eq!(scheme("fd::3"), Err(UrlRefusal::Scheme("fd".into())));
        assert_eq!(scheme("ftp://h/r"), Err(UrlRefusal::Scheme("ftp".into())));
        assert_eq!(scheme("./r.git"), Err(UrlRefusal::RelativePath));
        assert_eq!(scheme("r.git"), Err(UrlRefusal::RelativePath));
        assert_eq!(
            scheme("file://localhost/srv/r.git"),
            Err(UrlRefusal::RelativePath)
        );
        assert_eq!(scheme("file://sub/r.git"), Err(UrlRefusal::RelativePath));
    }

    #[test]
    fn credentials_are_refused() {
        for t in [
            "https://user@github.com/o/r.git",
            "https://x-access-token:abc@github.com/o/r.git",
            "https://ghp_abcdefghijklmnopqrstuvwxyz0123456789@github.com/o/r",
            "ssh://user:pass@host/r.git",
            "user:secret@host:r.git",
            "user:secret@host:/srv/r.git",
            // Indistinguishable from user info with a password.
            "git@host:team@org.git",
            "https://h/r.git?access_token=abc123",
            "https://h/r.git?sig=deadbeefcafe",
            "https://h/r#s3cr3t",
        ] {
            assert_eq!(scheme(t), Err(UrlRefusal::Credentials), "case {}", t.len());
        }
        // A token-shaped path part is refused too.
        assert_eq!(
            scheme("https://example.com/ghp_abcdefghijklmnopqrstuvwxyz0123456789/r"),
            Err(UrlRefusal::Credentials)
        );
    }

    #[test]
    fn malformed_urls_are_refused() {
        for t in [
            "",
            "-uhttps://h/r",
            "https://h/r\n",
            "https://h/r x",
            "https:///r",
            "ssh:///r",
            "@h:r",
            "h:",
            "ssh://-oProxyCommand=x/r",
            "ssh://-u@h/r",
            "user@-oProxyCommand=x:r",
            "-u@h:r",
        ] {
            assert_eq!(scheme(t), Err(UrlRefusal::Malformed), "{t:?}");
        }
    }
}
