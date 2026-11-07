//! Run search fields (U45): the params a playbook marks `search: true`, as
//! the values of one run, normalized by type and matched by equality.
//!
//! Pure: no IO. The engine computes a run's fields from the run's own start
//! definition and its `run.yaml` in the same pass that lists the runs
//! (`apb_engine::scheduler::listing`), so nothing is persisted.
//!
//! A value is listed and matched only when its param is declared
//! searchable, is not secret and its name does not look secret, the value
//! is not credential-shaped, not over [`MAX_VALUE_CHARS`] characters, and
//! the run redactor leaves it unchanged (the last check is the engine's).
//! A withheld or too long value is never returned or matched, so a query
//! cannot probe it.

use serde::Serialize;

use crate::schema::Param;

/// The most params one playbook may declare searchable (V162).
pub const MAX_SEARCH_PARAMS: usize = 8;
/// The most terms one query may carry.
pub const MAX_TERMS: usize = 8;
/// The longest value, in characters, that is listed or matched.
pub const MAX_VALUE_CHARS: usize = 200;

/// Whether `p` may be searchable at all: declared `search: true`, not
/// secret, and a name that does not look secret (V160, V161, checked again
/// at read time for a definition no validator saw).
pub fn searchable(p: &Param) -> bool {
    p.search && !p.secret && !crate::token_shape::name_looks_secret(&p.name)
}

/// What a listing shows of one field.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldState {
    /// Listed and matched.
    Value,
    /// Longer than [`MAX_VALUE_CHARS`]: never returned or matched.
    TooLong,
    /// Credential-shaped, or changed by the run redactor: never returned
    /// or matched.
    Withheld,
    /// The run has no value for the param.
    Absent,
    /// The value does not read as the param's type (an `int` that is not a
    /// number): never matched.
    Invalid,
}

/// One search field of a run as a listing shows it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchField {
    pub name: String,
    pub state: FieldState,
    /// The value as the run has it, only in state `value`.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// A run's search fields.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SearchFields {
    /// The run's start definition declares searchable params.
    Indexed { fields: Vec<SearchField> },
    /// No start definition, no `run.yaml`, or no searchable param.
    NotIndexed,
}

/// One field of a run with its comparison key, as matching reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedField {
    pub name: String,
    /// The param's `type`: `text`, `enum`, `int` or `bool`.
    pub kind: String,
    pub state: FieldState,
    /// The value shown, in state `value`.
    pub shown: Option<String>,
    /// The normalized key compared, in state `value`.
    pub key: Option<String>,
}

impl IndexedField {
    pub fn view(&self) -> SearchField {
        SearchField {
            name: self.name.clone(),
            state: self.state,
            value: self.shown.clone(),
        }
    }
}

/// The comparison key of `raw` as a value of a param of type `kind`, or
/// the state that keeps it out of every match. `text` is trimmed and
/// compared after Unicode lowercasing; `enum` exactly; `int` as an `i64`;
/// `bool` as `true` or `false`.
pub fn normalize(kind: &str, raw: &str) -> Result<String, FieldState> {
    if crate::token_shape::looks_like_credential(raw) {
        return Err(FieldState::Withheld);
    }
    match kind {
        "int" => raw
            .trim()
            .parse::<i64>()
            .map(|n| n.to_string())
            .map_err(|_| FieldState::Invalid),
        "bool" => match raw.trim() {
            "true" => Ok("true".into()),
            "false" => Ok("false".into()),
            _ => Err(FieldState::Invalid),
        },
        "enum" => {
            let t = raw.trim();
            if t.chars().count() > MAX_VALUE_CHARS {
                Err(FieldState::TooLong)
            } else {
                Ok(t.to_string())
            }
        }
        _ => {
            let t = raw.trim();
            if t.chars().count() > MAX_VALUE_CHARS {
                Err(FieldState::TooLong)
            } else {
                Ok(t.to_lowercase())
            }
        }
    }
}

/// The field of `param` for a run whose value is `raw` (`None`: absent).
/// `kept` says whether the run redactor left the value unchanged.
pub fn field(param: &Param, raw: Option<&str>, kept: bool) -> IndexedField {
    let mut f = IndexedField {
        name: param.name.clone(),
        kind: param.kind.clone(),
        state: FieldState::Absent,
        shown: None,
        key: None,
    };
    let Some(raw) = raw else {
        return f;
    };
    if !kept {
        f.state = FieldState::Withheld;
        return f;
    }
    match normalize(&param.kind, raw) {
        Ok(key) => {
            f.state = FieldState::Value;
            f.shown = Some(raw.trim().to_string());
            f.key = Some(key);
        }
        Err(state) => f.state = state,
    }
    f
}

/// One `name=value` term of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub name: String,
    pub value: String,
}

/// Why a query was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("at most {MAX_TERMS} search terms")]
    TooMany,
    #[error("a search term is `name=value` (or `name:value`), not `{0}`")]
    Malformed(String),
    #[error("a search value is at most {MAX_VALUE_CHARS} characters")]
    TooLong,
}

/// Parses `name=value` (or `name:value`, the HTTP form) terms.
pub fn parse_terms<S: AsRef<str>>(raw: &[S]) -> Result<Vec<Term>, QueryError> {
    if raw.len() > MAX_TERMS {
        return Err(QueryError::TooMany);
    }
    raw.iter()
        .map(|t| {
            let t = t.as_ref();
            // The earliest separator splits, so either one may appear in
            // the value.
            let at = t
                .find(['=', ':'])
                .ok_or_else(|| QueryError::Malformed(t.to_string()))?;
            let (name, value) = (&t[..at], &t[at + 1..]);
            let name = name.trim();
            if name.is_empty() || value.trim().is_empty() {
                return Err(QueryError::Malformed(t.to_string()));
            }
            if value.chars().count() > MAX_VALUE_CHARS {
                return Err(QueryError::TooLong);
            }
            Ok(Term {
                name: name.to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

/// How a run reads against a query's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Every term equals a listed field.
    Yes,
    /// Some term does not.
    No,
    /// Some term names a field whose value is withheld or too long: a
    /// non-match the listing counts apart.
    Withheld,
}

/// Matches `fields` against every term (AND, equality only).
pub fn matches(fields: &[IndexedField], terms: &[Term]) -> Match {
    let mut withheld = false;
    for t in terms {
        let Some(f) = fields.iter().find(|f| f.name == t.name) else {
            return Match::No;
        };
        match (&f.state, &f.key) {
            (FieldState::Value, Some(key)) => {
                if normalize(&f.kind, &t.value).ok().as_ref() != Some(key) {
                    return Match::No;
                }
            }
            (FieldState::Withheld | FieldState::TooLong, _) => withheld = true,
            _ => return Match::No,
        }
    }
    if withheld {
        Match::Withheld
    } else {
        Match::Yes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, kind: &str) -> Param {
        Param {
            name: name.into(),
            kind: kind.into(),
            label: None,
            options: None,
            default: None,
            secret: false,
            search: true,
            pattern: None,
            untrusted: false,
        }
    }

    fn term(name: &str, value: &str) -> Term {
        Term {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn text_is_trimmed_and_compared_without_case() {
        let f = field(&param("customer", "text"), Some("  Acme GmbH "), true);
        assert_eq!(f.state, FieldState::Value);
        assert_eq!(f.shown.as_deref(), Some("Acme GmbH"));
        assert_eq!(
            matches(std::slice::from_ref(&f), &[term("customer", "acme gmbh")]),
            Match::Yes
        );
        assert_eq!(
            matches(std::slice::from_ref(&f), &[term("customer", "ACME GMBH ")]),
            Match::Yes
        );
        assert_eq!(matches(&[f], &[term("customer", "acme")]), Match::No);
        // Unicode lowercasing, not ASCII only.
        let f = field(&param("city", "text"), Some("MÜNCHEN"), true);
        assert_eq!(matches(&[f], &[term("city", "münchen")]), Match::Yes);
    }

    #[test]
    fn int_enum_and_bool_compare_by_type() {
        let n = field(&param("ticket", "int"), Some("042"), true);
        assert_eq!(
            matches(std::slice::from_ref(&n), &[term("ticket", "42")]),
            Match::Yes
        );
        assert_eq!(matches(&[n], &[term("ticket", "43")]), Match::No);
        let e = field(&param("tier", "enum"), Some("Gold"), true);
        assert_eq!(
            matches(std::slice::from_ref(&e), &[term("tier", "Gold")]),
            Match::Yes
        );
        assert_eq!(matches(&[e], &[term("tier", "gold")]), Match::No);
        // A stored enum value is trimmed like the int, bool and text
        // branches, so the value the listing shows is the value a query
        // matches.
        let padded = field(&param("tier", "enum"), Some(" Gold "), true);
        assert_eq!(padded.shown.as_deref(), Some("Gold"));
        assert_eq!(
            matches(std::slice::from_ref(&padded), &[term("tier", "Gold")]),
            Match::Yes
        );
        let b = field(&param("urgent", "bool"), Some("true"), true);
        assert_eq!(
            matches(std::slice::from_ref(&b), &[term("urgent", "true")]),
            Match::Yes
        );
        assert_eq!(matches(&[b], &[term("urgent", "false")]), Match::No);
        let bad = field(&param("ticket", "int"), Some("forty"), true);
        assert_eq!(bad.state, FieldState::Invalid);
        assert_eq!(matches(&[bad], &[term("ticket", "forty")]), Match::No);
    }

    #[test]
    fn a_long_or_credential_shaped_value_is_never_shown_or_matched() {
        let long = "a".repeat(MAX_VALUE_CHARS + 1);
        let f = field(&param("note", "text"), Some(&long), true);
        assert_eq!((f.state, f.shown.as_deref()), (FieldState::TooLong, None));
        assert_eq!(matches(&[f], &[term("note", "a")]), Match::Withheld);
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let f = field(&param("ref", "text"), Some(secret), true);
        assert_eq!((f.state, f.shown.as_deref()), (FieldState::Withheld, None));
        assert_eq!(matches(&[f], &[term("ref", secret)]), Match::Withheld);
        // A value the run redactor changed is withheld too.
        let f = field(&param("ref", "text"), Some("plain"), false);
        assert_eq!(f.state, FieldState::Withheld);
    }

    #[test]
    fn terms_join_with_and_and_a_missing_field_is_no_match() {
        let fields = [
            field(&param("customer", "text"), Some("Acme"), true),
            field(&param("tier", "enum"), Some("gold"), true),
        ];
        let both = [term("customer", "acme"), term("tier", "gold")];
        assert_eq!(matches(&fields, &both), Match::Yes);
        let one_wrong = [term("customer", "acme"), term("tier", "silver")];
        assert_eq!(matches(&fields, &one_wrong), Match::No);
        assert_eq!(matches(&fields, &[term("region", "eu")]), Match::No);
        let absent = [field(&param("customer", "text"), None, true)];
        assert_eq!(matches(&absent, &[term("customer", "acme")]), Match::No);
    }

    #[test]
    fn terms_parse_in_both_forms_within_limits() {
        assert_eq!(
            parse_terms(&["customer=Acme", "tier:gold"]).unwrap(),
            vec![term("customer", "Acme"), term("tier", "gold")]
        );
        // `=` wins, so a value may hold a colon.
        assert_eq!(parse_terms(&["url=a:b"]).unwrap(), vec![term("url", "a:b")]);
        // The earliest separator splits: a `name:value` term keeps an `=`
        // inside its value.
        assert_eq!(
            parse_terms(&["document:https://x/?a=b"]).unwrap(),
            vec![term("document", "https://x/?a=b")]
        );
        assert!(matches!(
            parse_terms(&["nothing"]),
            Err(QueryError::Malformed(_))
        ));
        assert!(matches!(
            parse_terms(&["=x"]),
            Err(QueryError::Malformed(_))
        ));
        let nine: Vec<String> = (0..9).map(|i| format!("p{i}=v")).collect();
        assert_eq!(parse_terms(&nine), Err(QueryError::TooMany));
        let long = format!("p={}", "v".repeat(MAX_VALUE_CHARS + 1));
        assert_eq!(parse_terms(&[long]), Err(QueryError::TooLong));
    }

    #[test]
    fn a_secret_or_secret_named_param_is_never_searchable() {
        let mut p = param("customer", "text");
        assert!(searchable(&p));
        p.secret = true;
        assert!(!searchable(&p));
        assert!(!searchable(&param("api_token", "text")));
        let mut off = param("customer", "text");
        off.search = false;
        assert!(!searchable(&off));
    }
}
