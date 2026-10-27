//! The part of a playbook's `params` that remembered values depend on, and
//! the check of one remembered value against the params as they are now.

use serde::{Deserialize, Serialize};

use crate::schema::Param;

/// Why a remembered value no longer fits its param.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incompat {
    /// The param is gone from the playbook.
    Removed,
    /// The value does not parse as the param's type now.
    TypeChanged,
    /// The param is an enum now, and the value is not one of its options.
    OptionGone,
    /// The param is a secret now: its value is never used from memory.
    NowSecret,
}

impl Incompat {
    pub fn as_str(self) -> &'static str {
        match self {
            Incompat::Removed => "removed",
            Incompat::TypeChanged => "type_changed",
            Incompat::OptionGone => "option_gone",
            Incompat::NowSecret => "now_secret",
        }
    }
}

/// The digest of what remembered values depend on: each param's name,
/// type, options and secret flag, in name order. Labels and defaults do not
/// count, nor does the order of declaration.
pub fn params_schema_digest(params: &[Param]) -> String {
    let mut shape: Vec<(&str, &str, Option<&Vec<String>>, bool)> = params
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.kind.as_str(),
                p.options.as_ref(),
                p.secret,
            )
        })
        .collect();
    shape.sort();
    let json = serde_json::to_string(&shape).unwrap_or_default();
    crate::scope::digest_str(&json)
}

/// Whether `v` is a usable value of `p` now.
pub fn check_value(p: &Param, v: &str) -> Result<(), Incompat> {
    if p.secret {
        return Err(Incompat::NowSecret);
    }
    match p.kind.as_str() {
        "int" if v.trim().parse::<i64>().is_err() => Err(Incompat::TypeChanged),
        "bool" if !matches!(v.trim(), "true" | "false") => Err(Incompat::TypeChanged),
        "enum" if !p.options.as_ref().is_some_and(|o| o.iter().any(|x| x == v)) => {
            Err(Incompat::OptionGone)
        }
        _ => Ok(()),
    }
}
