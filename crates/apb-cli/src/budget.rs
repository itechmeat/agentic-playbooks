//! The run budget and estimate on the CLI: the budget flags of `apb run`
//! and `apb resume`, the estimate printed before a start (and alone with
//! `--estimate`), the question a person at a terminal answers when the
//! budget's `on_no_estimate` is `ask`, and the budget lines of `apb runs`
//! and `apb wait`.

use std::path::Path;
use std::process::ExitCode;

use apb_core::budget::{BudgetRequest, EffectiveBudget, OnNoEstimate, parse_tokens, parse_usd};
use apb_engine::budget::kind::{tokens_text, usd_text};
use apb_engine::budget::{BUDGET_PAUSED, BudgetResume, BudgetStatus, StartCheck};
use apb_engine::run_estimate::{RunEstimate, render_lines};

/// The run budget flags, shared by `apb run`, `apb resume` (a new limit) and
/// the detached supervised drive.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct BudgetFlags {
    /// Token limit (input plus output tokens of the run's agent attempts):
    /// 300000, 300k or 1.5m. Overrides the playbook's `budget.tokens`
    #[arg(long = "budget-tokens", value_name = "N")]
    pub budget_tokens: Option<String>,
    /// Dollar limit (reported cost): 3, 3.50 or $3.50. Overrides the
    /// playbook's `budget.usd`
    #[arg(long = "budget-usd", value_name = "X")]
    pub budget_usd: Option<String>,
    /// What the run does before a node with no spend estimate: ask, pause
    /// or continue
    #[arg(long = "on-no-estimate", value_name = "POLICY")]
    pub on_no_estimate: Option<String>,
}

/// The budget flags of `apb resume`.
#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct ResumeBudgetFlags {
    /// Tokens added to the run's token limit
    #[arg(long = "raise-budget-tokens", value_name = "N")]
    pub raise_tokens: Option<String>,
    /// Dollars added to the run's dollar limit
    #[arg(long = "raise-budget-usd", value_name = "X")]
    pub raise_usd: Option<String>,
    #[command(flatten)]
    pub set: BudgetFlags,
    /// Let the node the budget paused before run once over the budget
    #[arg(long = "over-budget-once")]
    pub once: bool,
}

fn policy(s: Option<&str>) -> Result<Option<OnNoEstimate>, String> {
    s.map(|v| {
        OnNoEstimate::parse(v)
            .ok_or_else(|| "bad --on-no-estimate (expected ask, pause or continue)".to_string())
    })
    .transpose()
}

fn tokens(flag: &str, s: Option<&str>) -> Result<Option<u64>, String> {
    s.map(|v| parse_tokens(v).map_err(|e| format!("bad --{flag}: {e}")))
        .transpose()
}

fn usd(flag: &str, s: Option<&str>) -> Result<Option<f64>, String> {
    s.map(|v| parse_usd(v).map_err(|e| format!("bad --{flag}: {e}")))
        .transpose()
}

impl BudgetFlags {
    /// The run's budget argument; `None` when no flag is set.
    pub(crate) fn request(&self) -> Result<Option<BudgetRequest>, String> {
        let req = BudgetRequest {
            tokens: tokens("budget-tokens", self.budget_tokens.as_deref())?,
            usd: usd("budget-usd", self.budget_usd.as_deref())?,
            on_no_estimate: policy(self.on_no_estimate.as_deref())?,
        };
        Ok((req != BudgetRequest::default()).then_some(req))
    }

    /// The flags that pass `req` on to a detached child.
    pub(crate) fn args_of(req: &BudgetRequest) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(t) = req.tokens {
            out.extend(["--budget-tokens".to_string(), t.to_string()]);
        }
        if let Some(u) = req.usd {
            out.extend(["--budget-usd".to_string(), u.to_string()]);
        }
        if let Some(p) = req.on_no_estimate {
            out.extend(["--on-no-estimate".to_string(), p.as_str().to_string()]);
        }
        out
    }
}

impl ResumeBudgetFlags {
    /// The resume's budget change; `None` when it asks for none.
    pub(crate) fn request(&self) -> Result<Option<BudgetResume>, String> {
        let req = BudgetResume {
            raise_tokens: tokens("raise-budget-tokens", self.raise_tokens.as_deref())?,
            raise_usd: usd("raise-budget-usd", self.raise_usd.as_deref())?,
            tokens: tokens("budget-tokens", self.set.budget_tokens.as_deref())?,
            usd: usd("budget-usd", self.set.budget_usd.as_deref())?,
            on_no_estimate: policy(self.set.on_no_estimate.as_deref())?,
            once: self.once,
        };
        Ok((!req.is_empty()).then_some(req))
    }
}

/// `400k tokens, $4.00, on no estimate: ask`.
fn budget_line(b: &EffectiveBudget) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = b.limit.tokens {
        parts.push(format!("{} tokens", tokens_text(t as f64)));
    }
    if let Some(u) = b.limit.usd {
        parts.push(usd_text(u));
    }
    format!(
        "budget: {}, on no estimate: {}",
        parts.join(", "),
        b.on_no_estimate.as_str()
    )
}

fn resolve(
    root: &Path,
    name: &str,
    version: Option<&str>,
) -> Result<apb_core::store::ResolvedPlaybook, String> {
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: name.to_string(),
        version: version.map(str::to_string),
    };
    apb_core::store::resolve(root, &wref).map_err(|e| e.to_string())
}

/// `apb run --estimate`: prints the estimate of playbook `name` and starts
/// nothing.
pub(crate) fn estimate_cmd(
    root: &Path,
    name: &str,
    version: Option<&str>,
    execution: &str,
    req: Option<&BudgetRequest>,
) -> ExitCode {
    let check = resolve(root, name, version).and_then(|r| {
        apb_engine::budget::check_resolved(&r, execution, req).map_err(|e| e.to_string())
    });
    match check {
        Ok(c) => {
            for line in render_lines(&c.estimate) {
                println!("{line}");
            }
            if let Some(b) = &c.budget {
                println!("{}", budget_line(b));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("estimate failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// What a start takes from its budget check: the budget argument to start
/// with, and the estimate printed for the person (the run's snapshot), when
/// the run has a budget.
#[derive(Debug, Default)]
pub(crate) struct Prestart {
    pub(crate) budget: Option<BudgetRequest>,
    pub(crate) shown: Option<RunEstimate>,
}

/// The budget check before a start: prints the estimate and the budget to
/// stderr when the run has a budget, asks a person at a terminal what to do
/// before unestimated nodes when the policy is `ask`, and returns the
/// budget argument to start with. Without a terminal `ask` stays, and the
/// run pauses before such a node. A check that cannot run refuses a start
/// with budget flags, and is said for one without.
pub(crate) fn prestart(
    root: &Path,
    name: &str,
    version: Option<&str>,
    execution: &str,
    req: Option<BudgetRequest>,
) -> Result<Prestart, String> {
    let check = match resolve(root, name, version).and_then(|r| {
        apb_engine::budget::check_resolved(&r, execution, req.as_ref()).map_err(|e| e.to_string())
    }) {
        Ok(c) => c,
        Err(e) if req.is_some() => return Err(format!("cannot check the budget: {e}")),
        Err(e) => {
            eprintln!("warning: budget check skipped: {e}");
            return Ok(Prestart::default());
        }
    };
    let Some(budget) = check.budget else {
        return match req {
            Some(_) => Err(
                "a budget needs --budget-tokens or --budget-usd; --on-no-estimate alone sets none"
                    .into(),
            ),
            None => Ok(Prestart::default()),
        };
    };
    for line in render_lines(&check.estimate) {
        eprintln!("{line}");
    }
    eprintln!("{}", budget_line(&budget));
    for w in &check.warnings {
        eprintln!("warning: {w}");
    }
    let shown = Some(check.estimate.clone());
    if !check.needs_decision {
        return Ok(Prestart { budget: req, shown });
    }
    let budget = match ask_policy(&check) {
        Some(p) => Some(BudgetRequest {
            on_no_estimate: Some(p),
            ..req.unwrap_or_default()
        }),
        None => {
            let one = check.undecided.len() == 1;
            eprintln!(
                "note: {} ha{} no estimate and on_no_estimate is ask: the run pauses before {}; pass --on-no-estimate pause or continue to decide now",
                check.undecided.join(", "),
                if one { "s" } else { "ve" },
                if one { "it" } else { "them" },
            );
            req
        }
    };
    Ok(Prestart { budget, shown })
}

/// Asks on the controlling terminal whether the run pauses before an
/// unestimated node or runs it, again until the answer is one of them;
/// `None` without a person to ask or when the input ends.
fn ask_policy(check: &StartCheck) -> Option<OnNoEstimate> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }
    let mut tty = std::io::BufReader::new(crate::consent::open_tty()?);
    let mut err = std::io::stderr();
    loop {
        let _ = write!(
            err,
            "no estimate for {}: pause before such a node or continue? [p/c] ",
            check.undecided.join(", ")
        );
        let _ = err.flush();
        let mut line = String::new();
        if tty.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if let Some(p) = policy_answer(&line) {
            return Some(p);
        }
        let _ = writeln!(err, "answer p (pause) or c (continue)");
    }
}

/// A terminal answer to the `ask` question; `None` for anything else.
fn policy_answer(line: &str) -> Option<OnNoEstimate> {
    match line.trim().to_ascii_lowercase().as_str() {
        "c" | "continue" => Some(OnNoEstimate::Continue),
        "p" | "pause" => Some(OnNoEstimate::Pause),
        _ => None,
    }
}

/// How `apb wait` says a budget pause continues: the raise of each limited
/// dimension, and `--on-no-estimate continue` for a pause the gate could not
/// estimate.
pub(crate) fn resume_hint(run_id: &str, b: &BudgetStatus) -> String {
    use apb_engine::budget::GateBasis;
    let mut flags: Vec<&str> = Vec::new();
    if b.paused
        .as_ref()
        .is_some_and(|p| matches!(p.basis, GateBasis::NoEstimate | GateBasis::UnknownSpend))
    {
        flags.push("--on-no-estimate continue");
    }
    if b.limit.tokens.is_some() {
        flags.push("--raise-budget-tokens <N>");
    }
    if b.limit.usd.is_some() {
        flags.push("--raise-budget-usd <X>");
    }
    flags.push("--over-budget-once");
    format!(
        "`apb resume {run_id} {}` (or {}) continues it",
        flags[0],
        flags[1..].join(", ")
    )
}

/// The resume refusal text, with the flags that change the budget, when the
/// engine refused a resume because of the budget.
pub(crate) fn resume_refusal(detail: &str) -> Option<String> {
    detail.starts_with(BUDGET_PAUSED).then(|| {
        format!(
            "resume refused: {detail}\n  continue with --raise-budget-tokens <N>, --raise-budget-usd <X>, --budget-tokens <N>, --budget-usd <X>, --on-no-estimate <P> or --over-budget-once"
        )
    })
}

/// The budget lines of `apb runs <id>` and `apb wait`.
pub(crate) fn status_lines(b: &BudgetStatus) -> Vec<String> {
    let dim = |name: &str,
               limit: String,
               d: &apb_engine::budget::DimStatus,
               f: &dyn Fn(f64) -> String| {
        let mut s = format!("  {name:<6} {} of {limit} spent", f(d.spent));
        if d.unknown_attempts > 0 {
            s.push_str(&format!(
                ", {} attempt{} without usage ({})",
                d.unknown_attempts,
                if d.unknown_attempts == 1 { "" } else { "s" },
                d.imputed
                    .map_or("no estimate".to_string(), |i| format!("~{}", f(i)))
            ));
        }
        s.push_str(&match d.remaining {
            Some(r) if r < 0.0 => format!(", over by {}", f(-r)),
            Some(r) => format!(", {} left", f(r)),
            None => ", left unknown".to_string(),
        });
        s
    };
    let mut out = vec![format!(
        "budget (on no estimate: {}):",
        b.on_no_estimate.as_str()
    )];
    if let (Some(l), Some(d)) = (b.limit.tokens, &b.tokens) {
        out.push(dim("tokens", tokens_text(l as f64), d, &tokens_text));
    }
    if let (Some(l), Some(d)) = (b.limit.usd, &b.usd) {
        out.push(dim("usd", usd_text(l), d, &usd_text));
    }
    if let Some(p) = &b.paused {
        out.push(format!(
            "  paused before `{}`{} ({}; needs {})",
            p.node,
            p.batch.map_or(String::new(), |n| format!(" batch {n}")),
            p.basis.as_str(),
            p.need.unwrap_or_default().text()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_amounts_and_refuse_bad_ones() {
        let f = BudgetFlags {
            budget_tokens: Some("300k".into()),
            budget_usd: Some("$2.50".into()),
            on_no_estimate: Some("continue".into()),
        };
        let req = f.request().unwrap().unwrap();
        assert_eq!(req.tokens, Some(300_000));
        assert_eq!(req.usd, Some(2.5));
        assert_eq!(
            BudgetFlags::args_of(&req),
            [
                "--budget-tokens",
                "300000",
                "--budget-usd",
                "2.5",
                "--on-no-estimate",
                "continue"
            ]
        );
        assert_eq!(BudgetFlags::default().request().unwrap(), None);
        let bad = BudgetFlags {
            budget_tokens: Some("0".into()),
            ..BudgetFlags::default()
        };
        assert!(
            bad.request()
                .unwrap_err()
                .starts_with("bad --budget-tokens")
        );
        let resume = ResumeBudgetFlags {
            raise_tokens: Some("1m".into()),
            once: true,
            ..ResumeBudgetFlags::default()
        };
        let r = resume.request().unwrap().unwrap();
        assert_eq!((r.raise_tokens, r.once), (Some(1_000_000), true));
    }

    #[test]
    fn a_terminal_answer_is_pause_or_continue_and_nothing_else() {
        assert_eq!(policy_answer(" C\n"), Some(OnNoEstimate::Continue));
        assert_eq!(policy_answer("pause"), Some(OnNoEstimate::Pause));
        assert_eq!(policy_answer("\n"), None);
        assert_eq!(policy_answer("yes"), None);
    }

    #[test]
    fn the_resume_hint_names_the_limited_dimensions_and_the_policy() {
        use apb_engine::budget::{GateBasis, PausedAt};
        let status = |tokens: Option<u64>, usd: Option<f64>, basis| BudgetStatus {
            limit: apb_core::budget::BudgetLimit { tokens, usd },
            on_no_estimate: OnNoEstimate::Pause,
            source: None,
            tokens: None,
            usd: None,
            sets: 1,
            paused: Some(PausedAt {
                node: "b".into(),
                batch: None,
                basis,
                need: None,
                remaining: None,
            }),
            waiver: None,
        };
        let usd_only = resume_hint("r1", &status(None, Some(2.0), GateBasis::Median));
        assert!(usd_only.contains("--raise-budget-usd"), "{usd_only}");
        assert!(!usd_only.contains("--raise-budget-tokens"), "{usd_only}");
        let no_estimate = resume_hint("r1", &status(Some(10), None, GateBasis::NoEstimate));
        assert!(no_estimate.starts_with("`apb resume r1 --on-no-estimate continue`"));
    }

    #[test]
    fn an_overspent_dimension_says_by_how_much() {
        let b = BudgetStatus {
            limit: apb_core::budget::BudgetLimit {
                tokens: Some(1_000),
                usd: None,
            },
            on_no_estimate: OnNoEstimate::Pause,
            source: None,
            tokens: Some(apb_engine::budget::DimStatus {
                spent: 1_500.0,
                unknown_attempts: 0,
                imputed: None,
                remaining: Some(-500.0),
            }),
            usd: None,
            sets: 1,
            paused: None,
            waiver: None,
        };
        assert!(
            status_lines(&b)[1].ends_with("over by 500"),
            "{:?}",
            status_lines(&b)
        );
    }
}
