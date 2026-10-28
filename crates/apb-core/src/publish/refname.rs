//! Branch and remote names a push may carry: git's `check-ref-format` rules
//! for a branch, and a plain grammar for a remote name. Both refuse a
//! leading `-`, so neither can read as an option.

/// The prefix of a branch's full ref.
pub const HEADS: &str = "refs/heads/";

/// A branch's short name: `refs/heads/x` and `x` both read as `x`.
pub fn short(branch: &str) -> &str {
    branch.strip_prefix(HEADS).unwrap_or(branch)
}

/// A branch's full ref, `refs/heads/<short>`.
pub fn full(branch: &str) -> String {
    format!("{HEADS}{}", short(branch))
}

/// Whether `branch` (short or full form) is a valid branch name under
/// `git check-ref-format --branch`: no empty, `.`-led or `.lock` component,
/// no `..`, `@{`, `//`, control or space character, none of `~^:?*[\`, no
/// leading or trailing `/`, no trailing `.`, not `@` or `HEAD`, not led by
/// `-`.
pub fn branch_ok(branch: &str) -> bool {
    let name = short(branch);
    if name.is_empty()
        || name == "@"
        || name == "HEAD"
        || name.starts_with('-')
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
        || name.starts_with("refs/")
    {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        return false;
    }
    name.split('/')
        .all(|c| !c.is_empty() && !c.starts_with('.') && !c.ends_with(".lock"))
}

/// Whether `name` is a remote name a publish node may pin:
/// `[A-Za-z0-9._-]+`, not led by `-`, and not `.` or `..`.
pub fn remote_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_follow_check_ref_format() {
        for ok in [
            "main",
            "apb/run-1",
            "refs/heads/feat/x",
            "a.b",
            "release-1.2",
            "x@y",
        ] {
            assert!(branch_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "@",
            "HEAD",
            "refs/heads/HEAD",
            "-x",
            "/x",
            "x/",
            "x.",
            "a..b",
            "a@{b",
            "a//b",
            "a b",
            "a\tb",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
            ".x",
            "a/.x",
            "a.lock",
            "a/b.lock/c",
            "refs/heads/",
            "refs/tags/v1",
        ] {
            assert!(!branch_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn short_and_full_forms() {
        assert_eq!(short("refs/heads/a/b"), "a/b");
        assert_eq!(short("a/b"), "a/b");
        assert_eq!(full("a/b"), "refs/heads/a/b");
        assert_eq!(full("refs/heads/a"), "refs/heads/a");
    }

    #[test]
    fn remote_names_are_plain() {
        for ok in ["origin", "up.stream", "fork_2", "a-b"] {
            assert!(remote_ok(ok), "{ok}");
        }
        for bad in ["", "-o", ".", "..", "a/b", "a b", "a:b", "ü"] {
            assert!(!remote_ok(bad), "{bad:?}");
        }
    }
}
