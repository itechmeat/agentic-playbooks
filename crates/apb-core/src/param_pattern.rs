//! The anchored matcher of a text param's `pattern`: the whole value must
//! match, with no trim and no case folding, an empty value included. The
//! validator (V177) owns the shape of the pattern; this module is the
//! check every start and the remembered input share.

use crate::schema::Param;

/// How many characters a pattern may hold.
pub const PATTERN_MAX_CHARS: usize = 512;
/// The compiled program's size limit (`RegexBuilder::size_limit`).
pub const PATTERN_SIZE_LIMIT: usize = 256 * 1024;

/// Why a value does not match its param's pattern. Holds the param name
/// and the pattern, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub name: String,
    pub pattern: String,
}

/// The anchored matcher of `pattern`: `\A(?:<pattern>)\z`, built with
/// `RegexBuilder` and [`PATTERN_SIZE_LIMIT`]. `Err` holds the first line
/// of the regex crate's error, or the length rule.
pub fn compile(pattern: &str) -> Result<regex::Regex, String> {
    if pattern.chars().count() > PATTERN_MAX_CHARS {
        return Err(format!(
            "the pattern is over {PATTERN_MAX_CHARS} characters"
        ));
    }
    regex::RegexBuilder::new(&format!("\\A(?:{pattern})\\z"))
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|e| {
            e.to_string()
                .lines()
                .next()
                .unwrap_or("the pattern does not compile")
                .to_string()
        })
}

/// `Ok` when `p` has no pattern, is not `text`, or `value` matches; an
/// unusable pattern is a mismatch (the validator reports it as V177).
pub fn check(p: &Param, value: &str) -> Result<(), Mismatch> {
    let Some(pattern) = &p.pattern else {
        return Ok(());
    };
    if p.kind != "text" {
        return Ok(());
    }
    let re = match compile(pattern) {
        Ok(re) => re,
        Err(_) => {
            return Err(Mismatch {
                name: p.name.clone(),
                pattern: pattern.clone(),
            });
        }
    };
    if re.is_match(value) {
        Ok(())
    } else {
        Err(Mismatch {
            name: p.name.clone(),
            pattern: pattern.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(name: &str, pattern: &str) -> Param {
        Param {
            name: name.into(),
            kind: "text".into(),
            label: None,
            options: None,
            default: None,
            secret: false,
            search: false,
            pattern: Some(pattern.into()),
            untrusted: false,
        }
    }

    fn plain(name: &str) -> Param {
        Param {
            name: name.into(),
            kind: "text".into(),
            label: None,
            options: None,
            default: None,
            secret: false,
            search: false,
            pattern: None,
            untrusted: false,
        }
    }

    #[test]
    fn a_partial_match_is_not_enough_and_the_case_is_kept() {
        let p = text("repo", "[a-z]+");
        assert!(check(&p, "abc").is_ok());
        assert!(check(&p, "abc; rm").is_err());
        assert!(check(&p, "ABC").is_err());
    }

    #[test]
    fn the_whole_pattern_is_one_group_and_the_empty_value_is_checked() {
        let p = text("pick", "a|b");
        assert!(check(&p, "a").is_ok());
        assert!(check(&p, "b").is_ok());
        assert!(check(&p, "ab").is_err());
        let p = text("tag", "[a-z]+");
        assert!(check(&p, "").is_err());
        let p = text("tag", ".*");
        assert!(check(&p, "").is_ok());
    }

    #[test]
    fn a_param_without_a_pattern_or_not_text_passes() {
        assert!(check(&plain("any"), "abc; rm").is_ok());
        let mut p = text("n", "[a-z]+");
        p.kind = "int".into();
        assert!(check(&p, "anything goes").is_ok());
    }

    #[test]
    fn an_overlong_or_uncompilable_pattern_is_err_from_compile() {
        let long = "a".repeat(PATTERN_MAX_CHARS + 1);
        assert!(compile(&long).is_err());
        assert!(compile("(unclosed").is_err());
        assert!(compile("[a-z]+").is_ok());
    }

    #[test]
    fn the_mismatch_holds_the_name_and_the_pattern_only() {
        let p = text("repo", "[a-z]+");
        let m = check(&p, "ABC").unwrap_err();
        assert_eq!(m.name, "repo");
        assert_eq!(m.pattern, "[a-z]+");
        let text = format!("{m:?}");
        assert!(!text.contains("ABC"), "{text}");
    }
}
