//! Stored eval results, their configuration key, and the comparison with
//! the previous stored result (design C2 section 9).
//!
//! Layout: `<config-dir>/evals/<playbook-id>/<key>.json`, one file per
//! configuration key holding every invocation that ran under it, newest
//! last. Files are written atomically with mode 0600. The key is the
//! SHA-256 of the canonical JSON of [`ConfigKey`]: the playbook digest, the
//! profile bundle digests, the executor (agent and model) each agent node
//! resolved to, and the overrides digest. A model alias that resolves
//! differently is therefore a different key.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::checks::{CheckResult, GoalResult, RepUsage, failing_kinds};
use crate::event::{Event, EventPayload};

/// What a configuration is made of.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigKey {
    pub playbook_digest: String,
    /// `<scope>/<name>` -> bundle digest.
    pub profile_bundles: BTreeMap<String, String>,
    /// node -> `agent/model` of the first executor of its chain.
    pub executors: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_digest: Option<String>,
}

impl ConfigKey {
    /// `sha256:<hex>` over the canonical JSON (sorted maps).
    pub fn key(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        apb_core::content::sha256_hex(json.as_bytes())
    }

    /// The file name for this key: the first 24 hex characters.
    pub fn file_stem(&self) -> String {
        let k = self.key();
        k.trim_start_matches("sha256:").chars().take(24).collect()
    }

    /// The key parts of a stored run: the playbook digest from
    /// `run_provenance`, bundles and executors from the run manifest.
    pub fn from_run(run_dir: &Path, events: &[Event], overrides_digest: Option<String>) -> Self {
        let playbook_digest = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunProvenance { digest, .. } => digest.clone(),
                _ => None,
            })
            .unwrap_or_default();
        let mut key = ConfigKey {
            playbook_digest,
            overrides_digest,
            ..Default::default()
        };
        if let Ok(Some(m)) = crate::manifest::read(run_dir) {
            for p in m.profiles.iter().filter(|p| !p.ephemeral) {
                key.profile_bundles.insert(p.key(), p.bundle_digest.clone());
            }
            for node in m.node_bindings.keys() {
                if let Some(first) = m.for_node(node).and_then(|p| p.chain.first()) {
                    key.executors
                        .insert(node.clone(), format!("{}/{}", first.agent_id, first.model));
                }
            }
        }
        key
    }

    /// What differs from `other`, one line per moved part.
    pub fn moved_from(&self, other: &ConfigKey) -> Vec<String> {
        let mut out = Vec::new();
        if self.playbook_digest != other.playbook_digest {
            out.push(format!(
                "playbook digest {} -> {}",
                short(&other.playbook_digest),
                short(&self.playbook_digest)
            ));
        }
        map_diff(
            "profile bundle",
            &other.profile_bundles,
            &self.profile_bundles,
            true,
            &mut out,
        );
        map_diff(
            "executor",
            &other.executors,
            &self.executors,
            false,
            &mut out,
        );
        if self.overrides_digest != other.overrides_digest {
            out.push(format!(
                "overrides {} -> {}",
                other
                    .overrides_digest
                    .as_deref()
                    .map(short)
                    .unwrap_or("none"),
                self.overrides_digest
                    .as_deref()
                    .map(short)
                    .unwrap_or("none")
            ));
        }
        out
    }
}

fn short(d: &str) -> &str {
    let d = d.trim_start_matches("sha256:");
    &d[..d.len().min(12)]
}

fn map_diff(
    what: &str,
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
    digests: bool,
    out: &mut Vec<String>,
) {
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let show = |v: Option<&String>| match v {
        None => "none".to_string(),
        Some(v) if digests => short(v).to_string(),
        Some(v) => v.clone(),
    };
    for k in keys {
        let (a, b) = (old.get(k), new.get(k));
        if a != b {
            out.push(format!("{what} {k}: {} -> {}", show(a), show(b)));
        }
    }
}

/// One repetition of one case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repetition {
    pub repetition: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Where the run directory was kept, `<config-dir>/evals/runs/...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<String>,
    /// `passed`, `failed`, `error` (a check could not run, or the run could
    /// not start) or `incomplete` (a limit stopped it).
    pub verdict: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    pub checks: Vec<CheckResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal: Vec<GoalResult>,
    pub usage: RepUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The worktree kept because the run did not end cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_worktree: Option<String>,
}

/// Every repetition of one case in one invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub case: String,
    pub case_digest: String,
    pub passes: u32,
    pub of: u32,
    /// Wilson 95% interval of the pass rate, for information only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wilson95: Option<(f64, f64)>,
    pub repetitions: Vec<Repetition>,
}

impl CaseResult {
    pub fn new(case: &str, case_digest: &str, repetitions: Vec<Repetition>) -> Self {
        let of = repetitions.len() as u32;
        let passes = repetitions.iter().filter(|r| r.verdict == "passed").count() as u32;
        CaseResult {
            case: case.to_string(),
            case_digest: case_digest.to_string(),
            passes,
            of,
            wilson95: crate::decision::report::stats::wilson95(passes as usize, of as usize),
            repetitions,
        }
    }

    fn median_cost(&self) -> Option<f64> {
        let v: Vec<f64> = self
            .repetitions
            .iter()
            .filter_map(|r| r.usage.cost_usd)
            .collect();
        median_f(v)
    }

    fn median_duration(&self) -> Option<u64> {
        let v: Vec<f64> = self
            .repetitions
            .iter()
            .filter_map(|r| r.duration_ms.map(|d| d as f64))
            .collect();
        median_f(v).map(|m| m as u64)
    }
}

fn median_f(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// One `apb eval` invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    pub eval_id: String,
    pub playbook: String,
    pub version: String,
    pub started_at_ms: u128,
    pub finished_at_ms: u128,
    pub apb_version: String,
    /// The project the suite came from.
    pub workspace: String,
    pub config_key: String,
    pub config: ConfigKey,
    pub cases: Vec<CaseResult>,
    /// Why the invocation did not run every planned repetition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<String>,
    pub total_cost_usd: f64,
    pub total_tokens: u64,
}

impl EvalResult {
    pub fn passes(&self) -> (u32, u32) {
        self.cases
            .iter()
            .fold((0, 0), |(p, o), c| (p + c.passes, o + c.of))
    }
}

/// The on-disk file of one configuration key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredConfig {
    pub config_key: String,
    pub config: ConfigKey,
    pub results: Vec<EvalResult>,
}

/// `<evals_home>/<playbook>`.
pub fn playbook_dir(evals_home: &Path, playbook: &str) -> PathBuf {
    evals_home.join(playbook)
}

/// Appends `result` to its key's file.
pub fn store(evals_home: &Path, result: &EvalResult) -> std::io::Result<PathBuf> {
    let dir = playbook_dir(evals_home, &result.playbook);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", result.config.file_stem()));
    let mut stored = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StoredConfig>(&raw).ok())
        .unwrap_or_else(|| StoredConfig {
            config_key: result.config_key.clone(),
            config: result.config.clone(),
            results: Vec::new(),
        });
    stored.results.push(result.clone());
    let body = serde_json::to_string_pretty(&stored).map_err(std::io::Error::other)?;
    apb_core::fsutil::atomic_write_private(&path, body.as_bytes())?;
    Ok(path)
}

/// Every stored result of `playbook`, oldest first.
pub fn load_all(evals_home: &Path, playbook: &str) -> Vec<EvalResult> {
    let Ok(entries) = std::fs::read_dir(playbook_dir(evals_home, playbook)) else {
        return Vec::new();
    };
    let mut out: Vec<EvalResult> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|raw| serde_json::from_str::<StoredConfig>(&raw).ok())
        .flat_map(|s| s.results)
        .collect();
    out.sort_by_key(|r| (r.finished_at_ms, r.eval_id.clone()));
    out
}

/// One case side by side.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseDelta {
    pub case: String,
    pub baseline: String,
    pub candidate: String,
    /// Candidate pass rate minus baseline pass rate.
    pub delta: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_failed_check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_median_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_median_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_median_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_median_duration_ms: Option<u64>,
}

/// The comparison of a candidate result with a baseline result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    pub baseline_eval: String,
    pub candidate_eval: String,
    pub same_configuration: bool,
    /// What moved between the two configurations (empty when the same).
    pub configuration_changes: Vec<String>,
    pub cases: Vec<CaseDelta>,
    /// Totals over the cases compared.
    pub baseline_passes: String,
    pub candidate_passes: String,
    /// Present on one side only, or with a changed case digest.
    pub only_baseline: Vec<String>,
    pub only_candidate: Vec<String>,
    pub case_changed: Vec<String>,
}

fn rate(p: u32, of: u32) -> f64 {
    if of == 0 { 0.0 } else { p as f64 / of as f64 }
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Compares `candidate` with `baseline` over the cases present on both
/// sides with an unchanged case digest.
pub fn compare(baseline: &EvalResult, candidate: &EvalResult) -> Comparison {
    let base: BTreeMap<&str, &CaseResult> = baseline
        .cases
        .iter()
        .map(|c| (c.case.as_str(), c))
        .collect();
    let cand: BTreeMap<&str, &CaseResult> = candidate
        .cases
        .iter()
        .map(|c| (c.case.as_str(), c))
        .collect();
    let mut cmp = Comparison {
        baseline_eval: baseline.eval_id.clone(),
        candidate_eval: candidate.eval_id.clone(),
        same_configuration: baseline.config_key == candidate.config_key,
        configuration_changes: candidate.config.moved_from(&baseline.config),
        cases: Vec::new(),
        baseline_passes: String::new(),
        candidate_passes: String::new(),
        only_baseline: base
            .keys()
            .filter(|k| !cand.contains_key(*k))
            .map(|k| k.to_string())
            .collect(),
        only_candidate: cand
            .keys()
            .filter(|k| !base.contains_key(*k))
            .map(|k| k.to_string())
            .collect(),
        case_changed: Vec::new(),
    };
    let (mut bp, mut bo, mut cp, mut co) = (0, 0, 0, 0);
    for (name, c) in &cand {
        let Some(b) = base.get(name) else { continue };
        if b.case_digest != c.case_digest {
            cmp.case_changed.push(name.to_string());
            continue;
        }
        bp += b.passes;
        bo += b.of;
        cp += c.passes;
        co += c.of;
        cmp.cases.push(CaseDelta {
            case: name.to_string(),
            baseline: format!("{}/{}", b.passes, b.of),
            candidate: format!("{}/{}", c.passes, c.of),
            delta: round4(rate(c.passes, c.of) - rate(b.passes, b.of)),
            most_failed_check: failing_kinds(c.repetitions.iter().map(|r| r.checks.as_slice()))
                .into_iter()
                .next()
                .map(|(k, n)| format!("{k} ({n}x)")),
            baseline_median_cost_usd: b.median_cost(),
            candidate_median_cost_usd: c.median_cost(),
            baseline_median_duration_ms: b.median_duration(),
            candidate_median_duration_ms: c.median_duration(),
        });
    }
    cmp.baseline_passes = format!("{bp}/{bo}");
    cmp.candidate_passes = format!("{cp}/{co}");
    cmp
}

/// The stored result `candidate` is compared with: the most recent other
/// invocation of the same playbook.
pub fn baseline_for<'a>(all: &'a [EvalResult], candidate: &EvalResult) -> Option<&'a EvalResult> {
    all.iter()
        .filter(|r| r.eval_id != candidate.eval_id)
        .max_by_key(|r| (r.finished_at_ms, r.eval_id.clone()))
}

fn money(v: Option<f64>) -> String {
    v.map(|c| format!("${c:.4}"))
        .unwrap_or_else(|| "n/a".into())
}

fn secs(v: Option<u64>) -> String {
    v.map(|d| format!("{:.1}s", d as f64 / 1000.0))
        .unwrap_or_else(|| "n/a".into())
}

/// The comparison as text, one line per fact.
pub fn render_comparison(c: &Comparison) -> String {
    let mut s = format!("compared with {}\n", c.baseline_eval);
    if c.same_configuration {
        s.push_str("  configuration: unchanged\n");
    } else {
        s.push_str("  configuration changed:\n");
        for m in &c.configuration_changes {
            s.push_str(&format!("    {m}\n"));
        }
    }
    for d in &c.cases {
        s.push_str(&format!(
            "  {}: baseline {} -> candidate {} (delta {:+.2}); cost {} -> {}; duration {} -> {}{}\n",
            d.case,
            d.baseline,
            d.candidate,
            d.delta,
            money(d.baseline_median_cost_usd),
            money(d.candidate_median_cost_usd),
            secs(d.baseline_median_duration_ms),
            secs(d.candidate_median_duration_ms),
            d.most_failed_check
                .as_deref()
                .map(|k| format!("; most failed: {k}"))
                .unwrap_or_default()
        ));
    }
    s.push_str(&format!(
        "  suite: baseline {} -> candidate {}\n",
        c.baseline_passes, c.candidate_passes
    ));
    for (label, list) in [
        ("only in the baseline", &c.only_baseline),
        ("only in the candidate", &c.only_candidate),
        ("case changed, not compared", &c.case_changed),
    ] {
        if !list.is_empty() {
            s.push_str(&format!("  {label}: {}\n", list.join(", ")));
        }
    }
    s
}

/// One invocation's result as text.
pub fn render_result(r: &EvalResult) -> String {
    let (p, o) = r.passes();
    let mut s = format!(
        "eval {} {} {} (key {}): {p}/{o} passed\n",
        r.eval_id,
        r.playbook,
        r.version,
        short(&r.config_key)
    );
    for c in &r.cases {
        let wilson = c
            .wilson95
            .map(|(lo, hi)| format!(" [95% {lo:.2}-{hi:.2}]"))
            .unwrap_or_default();
        s.push_str(&format!(
            "  {}: {}/{}{wilson}; median cost {}, median duration {}\n",
            c.case,
            c.passes,
            c.of,
            money(c.median_cost()),
            secs(c.median_duration())
        ));
        for rep in &c.repetitions {
            s.push_str(&format!(
                "    #{} {} (outcome {}{}), {} tokens, cost {}\n",
                rep.repetition,
                rep.verdict,
                rep.outcome,
                rep.stopped
                    .as_deref()
                    .map(|x| format!(", {x}"))
                    .unwrap_or_default(),
                rep.usage.tokens(),
                money(rep.usage.cost_usd)
            ));
            for ch in rep
                .checks
                .iter()
                .filter(|c| c.status != super::checks::CheckStatus::Passed)
            {
                s.push_str(&format!(
                    "      {} {:?}: {}\n",
                    ch.kind,
                    ch.status,
                    ch.detail.as_deref().unwrap_or("")
                ));
            }
            if let Some(w) = &rep.kept_worktree {
                s.push_str(&format!("      worktree kept: {w}\n"));
            }
        }
    }
    s.push_str(&format!(
        "  total: {} tokens, cost ${:.4}\n",
        r.total_tokens, r.total_cost_usd
    ));
    if let Some(why) = &r.incomplete {
        s.push_str(&format!("  incomplete: {why}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::checks::CheckStatus;

    fn key(model: &str) -> ConfigKey {
        ConfigKey {
            playbook_digest: "sha256:aaaa".into(),
            profile_bundles: [("project/rev".to_string(), "sha256:bbbb".to_string())].into(),
            executors: [("review".to_string(), format!("claude/{model}"))].into(),
            overrides_digest: None,
        }
    }

    fn rep(n: u32, pass: bool) -> Repetition {
        Repetition {
            repetition: n,
            run_id: Some(format!("r{n}")),
            run_dir: None,
            verdict: if pass { "passed" } else { "failed" }.into(),
            outcome: "succeeded".into(),
            stopped: None,
            checks: vec![CheckResult {
                kind: "files[x]".into(),
                status: if pass {
                    CheckStatus::Passed
                } else {
                    CheckStatus::Failed
                },
                detail: None,
            }],
            goal: vec![],
            usage: RepUsage {
                input_tokens: 10,
                output_tokens: 5,
                cost_usd: Some(0.01),
            },
            duration_ms: Some(2000),
            kept_worktree: None,
        }
    }

    fn result(id: &str, at: u128, config: ConfigKey, passes: &[bool]) -> EvalResult {
        let reps = passes
            .iter()
            .enumerate()
            .map(|(i, p)| rep(i as u32 + 1, *p))
            .collect();
        EvalResult {
            eval_id: id.into(),
            playbook: "pb".into(),
            version: "1.0.0".into(),
            started_at_ms: at,
            finished_at_ms: at + 1,
            apb_version: "0.24.0".into(),
            workspace: "/w".into(),
            config_key: config.key(),
            config,
            cases: vec![CaseResult::new("c1", "sha256:case", reps)],
            incomplete: None,
            total_cost_usd: 0.0,
            total_tokens: 0,
        }
    }

    #[test]
    fn the_key_is_stable_and_moves_with_each_part() {
        let a = key("haiku");
        assert_eq!(a.key(), key("haiku").key());
        // Pinned: a change of the canonical form would orphan stored results.
        assert_eq!(
            a.key(),
            "sha256:fa9d9247d1ad734b1bb8e86fc817575ea5488915edc61abb7891f7e1e5ab43f8"
        );
        assert_eq!(a.file_stem().len(), 24);
        assert_ne!(a.key(), key("sonnet").key());
        let mut b = key("haiku");
        b.overrides_digest = Some("sha256:cc".into());
        assert_ne!(a.key(), b.key());
        let mut c = key("haiku");
        c.profile_bundles
            .insert("project/rev".into(), "sha256:dddd".into());
        assert_eq!(
            c.moved_from(&a),
            vec!["profile bundle project/rev: bbbb -> dddd".to_string()]
        );
        assert_eq!(
            key("sonnet").moved_from(&a),
            vec!["executor review: claude/haiku -> claude/sonnet".to_string()]
        );
    }

    #[test]
    fn store_appends_per_key_and_the_baseline_is_the_previous_invocation() {
        let home = tempfile::tempdir().unwrap();
        let first = result("e1", 100, key("haiku"), &[true, true]);
        let second = result("e2", 200, key("haiku"), &[true, false]);
        let third = result("e3", 300, key("sonnet"), &[false, false]);
        for r in [&first, &second, &third] {
            store(home.path(), r).unwrap();
        }
        let files = std::fs::read_dir(home.path().join("pb")).unwrap().count();
        assert_eq!(files, 2, "one file per configuration key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = home
                .path()
                .join("pb")
                .join(format!("{}.json", key("haiku").file_stem()));
            assert_eq!(
                std::fs::metadata(f).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let all = load_all(home.path(), "pb");
        assert_eq!(
            all.iter().map(|r| r.eval_id.as_str()).collect::<Vec<_>>(),
            ["e1", "e2", "e3"]
        );
        assert_eq!(baseline_for(&all, &third).unwrap().eval_id, "e2");
        assert_eq!(all[1].cases[0].passes, 1);
        assert_eq!(all[1].cases[0].wilson95, Some((0.0945, 0.9055)));
    }

    #[test]
    fn compare_reports_the_delta_and_what_moved() {
        let base = result("e1", 100, key("haiku"), &[true, true]);
        let cand = result("e2", 200, key("sonnet"), &[true, false]);
        let c = compare(&base, &cand);
        assert!(!c.same_configuration);
        assert_eq!(c.cases[0].baseline, "2/2");
        assert_eq!(c.cases[0].candidate, "1/2");
        assert_eq!(c.cases[0].delta, -0.5);
        assert_eq!(
            c.cases[0].most_failed_check.as_deref(),
            Some("files[x] (1x)")
        );
        let text = render_comparison(&c);
        assert_eq!(
            text,
            "compared with e1\n  configuration changed:\n    executor review: claude/haiku -> claude/sonnet\n  c1: baseline 2/2 -> candidate 1/2 (delta -0.50); cost $0.0100 -> $0.0100; duration 2.0s -> 2.0s; most failed: files[x] (1x)\n  suite: baseline 2/2 -> candidate 1/2\n"
        );
        let same = compare(&base, &result("e3", 300, key("haiku"), &[true, true]));
        assert!(same.same_configuration);
        assert!(render_comparison(&same).contains("configuration: unchanged"));
        let mut changed = result("e4", 400, key("haiku"), &[true]);
        changed.cases[0].case_digest = "sha256:other".into();
        let c = compare(&base, &changed);
        assert!(c.cases.is_empty());
        assert_eq!(c.case_changed, vec!["c1".to_string()]);
    }
}
