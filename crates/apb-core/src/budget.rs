//! The run budget: a predictive spend limit in tokens and US dollars.
//!
//! A playbook declares a default with its top-level `budget:` block; a run
//! argument overrides it per dimension. The engine's gate admits a gated
//! node only while the remaining amount covers the node's median spend from
//! comparable past runs, and pauses the run before it otherwise. A budget
//! is a predictive stop, not a security control: a node already admitted
//! may still spend past the limit.
//!
//! This module is the schema and the pure parts: the YAML shape
//! ([`BudgetBlock`]), the precedence of a run argument over the block
//! ([`effective`]) and the amount parsers shared by every surface. Which
//! nodes the gate weighs is [`crate::schema::NodeKind::budget_gated`].

use serde::{Deserialize, Serialize};

/// The playbook's `budget:` block. A block without any limit is invalid
/// (validator V96).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetBlock {
    /// Input plus output tokens over the run's agent attempts (cache
    /// excluded, as in `apb stats`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Reported cost in US dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
    /// What the gate does before a node with no spend estimate.
    #[serde(default)]
    pub on_no_estimate: OnNoEstimate,
}

/// What the gate does before a gated node it cannot estimate, or when the
/// remaining amount is unknown.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnNoEstimate {
    /// The person decides before the run starts; left unresolved, it acts
    /// as `pause` at runtime.
    #[default]
    Ask,
    /// Pause the run before the node.
    Pause,
    /// Run the node anyway.
    Continue,
}

impl OnNoEstimate {
    pub fn as_str(self) -> &'static str {
        match self {
            OnNoEstimate::Ask => "ask",
            OnNoEstimate::Pause => "pause",
            OnNoEstimate::Continue => "continue",
        }
    }

    /// Parses `ask`, `pause` or `continue` (case-insensitive, trimmed).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(OnNoEstimate::Ask),
            "pause" => Some(OnNoEstimate::Pause),
            "continue" => Some(OnNoEstimate::Continue),
            _ => None,
        }
    }
}

/// The limits in force, per dimension. `None` leaves a dimension unlimited.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub usd: Option<f64>,
}

impl BudgetLimit {
    /// Whether at least one dimension is limited.
    pub fn is_limited(&self) -> bool {
        self.tokens.is_some() || self.usd.is_some()
    }
}

/// A budget passed as a run argument. Each field it sets overrides the
/// playbook's block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_no_estimate: Option<OnNoEstimate>,
}

/// Where the budget in force came from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetSource {
    /// The playbook's `budget:` block alone.
    Playbook,
    /// A run argument (alone or over the playbook's block).
    Run,
    /// An operator set or raised it at resume.
    Resume,
}

/// The budget a run starts with: limits, policy and where they came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveBudget {
    pub limit: BudgetLimit,
    pub on_no_estimate: OnNoEstimate,
    pub source: BudgetSource,
}

/// The budget in force for a run: the run argument overrides the playbook's
/// block per dimension, and its `on_no_estimate` the same way. `None` when
/// neither limits anything: a policy alone is no budget.
pub fn effective(
    block: Option<&BudgetBlock>,
    req: Option<&BudgetRequest>,
) -> Option<EffectiveBudget> {
    let limit = BudgetLimit {
        tokens: req.and_then(|r| r.tokens).or(block.and_then(|b| b.tokens)),
        usd: req.and_then(|r| r.usd).or(block.and_then(|b| b.usd)),
    };
    if !limit.is_limited() {
        return None;
    }
    let on_no_estimate = req
        .and_then(|r| r.on_no_estimate)
        .or(block.map(|b| b.on_no_estimate))
        .unwrap_or_default();
    let from_run =
        req.is_some_and(|r| r.tokens.is_some() || r.usd.is_some() || r.on_no_estimate.is_some());
    Some(EffectiveBudget {
        limit,
        on_no_estimate,
        source: if from_run {
            BudgetSource::Run
        } else {
            BudgetSource::Playbook
        },
    })
}

/// Parses a token amount: `300000`, `300k` or `1.5m` (case-insensitive,
/// `_` separators ignored). Zero is refused, and so is a `,`: as a decimal
/// comma (`1,5m`) it would read ten times larger than meant. A whole
/// number is read exactly.
pub fn parse_tokens(s: &str) -> Result<u64, String> {
    let t: String = s
        .trim()
        .chars()
        .filter(|c| *c != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    if let Ok(n) = t.parse::<u64>() {
        return match n {
            0 => Err("a token budget must be greater than 0".into()),
            n => Ok(n),
        };
    }
    let (num, mult) = match t.strip_suffix('k') {
        Some(n) => (n, 1_000f64),
        None => match t.strip_suffix('m') {
            Some(n) => (n, 1_000_000f64),
            None => (t.as_str(), 1f64),
        },
    };
    let bad = || "expected a token amount such as 300000, 300k or 1.5m".to_string();
    let v: f64 = num.trim().parse().map_err(|_| bad())?;
    if !v.is_finite() || v < 0.0 {
        return Err(bad());
    }
    let n = (v * mult).round();
    if n < 1.0 {
        return Err("a token budget must be greater than 0".into());
    }
    // `u64::MAX as f64` rounds up to 2^64, itself out of range.
    if n >= u64::MAX as f64 {
        return Err(bad());
    }
    Ok(n as u64)
}

/// Parses a dollar amount: `3`, `3.50` or `$3.50`. Zero, negative and
/// non-finite amounts are refused.
pub fn parse_usd(s: &str) -> Result<f64, String> {
    let t = s.trim();
    let t = t.strip_prefix('$').unwrap_or(t).trim();
    let v: f64 = t
        .parse()
        .map_err(|_| "expected a dollar amount such as 3, 3.50 or $3.50".to_string())?;
    if !v.is_finite() || v <= 0.0 {
        return Err("a dollar budget must be a finite amount greater than 0".into());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tokens_accepts_k_and_m_and_refuses_zero_and_text() {
        assert_eq!(parse_tokens("300000"), Ok(300_000));
        assert_eq!(parse_tokens("300k"), Ok(300_000));
        assert_eq!(parse_tokens("1.5M"), Ok(1_500_000));
        assert_eq!(parse_tokens(" 250_000 "), Ok(250_000));
        assert!(parse_tokens("0").is_err());
        assert!(parse_tokens("0k").is_err());
        assert!(parse_tokens("lots").is_err());
        assert!(parse_tokens("-5k").is_err());
        // A decimal comma would read ten times larger: refused.
        assert!(parse_tokens("1,5m").is_err());
        assert!(parse_tokens("1,000").is_err());
        assert_eq!(parse_tokens("18446744073709551615"), Ok(u64::MAX));
        assert!(parse_tokens("18446744073709551616").is_err());
        assert!(parse_tokens("18446744073709.551616m").is_err());
        assert_eq!(parse_usd("$3.50"), Ok(3.5));
        assert_eq!(parse_usd("3"), Ok(3.0));
        assert!(parse_usd("0").is_err());
        assert!(parse_usd("NaN").is_err());
        assert!(parse_usd("inf").is_err());
    }

    #[test]
    fn run_argument_overrides_per_dimension_and_policy_alone_is_no_budget() {
        let block = BudgetBlock {
            tokens: Some(400_000),
            usd: Some(4.0),
            on_no_estimate: OnNoEstimate::Pause,
        };
        let req = BudgetRequest {
            tokens: Some(100_000),
            usd: None,
            on_no_estimate: None,
        };
        let eff = effective(Some(&block), Some(&req)).unwrap();
        assert_eq!(eff.limit.tokens, Some(100_000));
        assert_eq!(eff.limit.usd, Some(4.0));
        assert_eq!(eff.on_no_estimate, OnNoEstimate::Pause);
        assert_eq!(eff.source, BudgetSource::Run);

        let only_block = effective(Some(&block), None).unwrap();
        assert_eq!(only_block.source, BudgetSource::Playbook);

        let policy_only = BudgetRequest {
            on_no_estimate: Some(OnNoEstimate::Continue),
            ..BudgetRequest::default()
        };
        assert_eq!(effective(None, Some(&policy_only)), None);
        assert_eq!(effective(None, None), None);
        let higher = BudgetRequest {
            tokens: Some(900_000),
            on_no_estimate: Some(OnNoEstimate::Continue),
            ..BudgetRequest::default()
        };
        let eff = effective(Some(&block), Some(&higher)).unwrap();
        assert_eq!(eff.limit.tokens, Some(900_000));
        assert_eq!(eff.on_no_estimate, OnNoEstimate::Continue);
    }
}
