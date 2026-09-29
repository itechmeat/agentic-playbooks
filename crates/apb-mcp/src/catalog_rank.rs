//! Opt-in catalog ranking and suppression coverage (issue #165 Part 16).
//!
//! When the machine's `decisions.yaml` enables `uses.catalog_rank` for the
//! project, a provider key resolves and the caller passes a `query` (the
//! task in a sentence), `playbook_catalog` asks one decision request:
//!
//! - `best`: a `choice` over the catalog's playbooks (short option ids
//!   `p1`, `p2`, ... plus `none_of_these`), each option described by that
//!   playbook's structured trigger (`when`, `avoid_when`, `examples`,
//!   clipped) in the state;
//! - `needs_playbook`: a `noul`, whether the task is a doable action a saved
//!   procedure could perform;
//! - `covered_<n>`: one `noul` per active suppression record, whether its
//!   synopsis describes the same procedure as the task (the capture offer's
//!   "a covering record means no offer" check).
//!
//! Above 254 playbooks the catalog is ranked in chunks and the chunk leaders
//! are ranked again. Advise (enforce is treated as advise: the ranking is
//! advisory ordering, never applied) adds `ranked: [{ref, p}]` (top five),
//! `confidence`, `needs_playbook_p`, `covered_by` (at `p` at or above
//! `uses.catalog_rank.thresholds.covered`) and `ranking: {provider, model,
//! calibrated}` to the unchanged catalog; nothing is filtered or reordered.
//! `revision` is bypassed, since the answer depends on the query. Shadow asks
//! and logs but returns the plain catalog. Any failure returns the full
//! catalog with `ranking: {error}`.
//!
//! Without a configuration, with the use off, without a resolving key or
//! without a query the response is the plain catalog, byte for byte.
//! Answers are cached for the server's lifetime per project root, catalog
//! revision and query digest.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

use apb_core::decisions::DecisionMode;
use apb_decide::{Answer, ChoiceCriteria, Question, UseSite};
use apb_engine::decision::standalone::{StandaloneDecider, StandaloneOutcome};
use serde_json::{Value, json};

use crate::tools::ToolError;

/// Most options a `choice` may carry, `none_of_these` included.
/// The most silenced suggestions asked about in one request.
const MAX_COVERAGE: usize = 16;

/// A suggestion synopsis, at most, in the state.
const SYNOPSIS_BYTES: usize = 600;

const MAX_OPTIONS: usize = 255;
/// How many ranked refs the response carries.
const TOP: usize = 5;
/// How much of the budget the playbooks may take (redaction can lengthen a
/// short value, so the state aims below the cap).
const STATE_SHARE: f64 = 0.9;
const NONE: &str = "none_of_these";
/// The task is clipped to this many bytes.
const TASK_BYTES: usize = 2_000;

/// Answers per `(root, revision, query digest)`, for the server's lifetime.
#[derive(Debug, Default)]
pub struct RankCache(Mutex<HashMap<(String, String, String), Value>>);

impl RankCache {
    fn get(&self, key: &(String, String, String)) -> Option<Value> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    fn put(&self, key: (String, String, String), v: Value) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, v);
    }
}

/// `playbook_catalog` with a query. Falls back to the plain catalog
/// whenever ranking is not enabled. `caller` is the project the asking agent
/// works in; for a cross-workspace catalog it differs from `root`, and the
/// stricter of both projects' decision settings applies.
pub fn playbook_catalog_ranked(
    caller: &Path,
    root: &Path,
    workspace_id: Option<&str>,
    revision: Option<&str>,
    limit: Option<usize>,
    query: &str,
    cache: &RankCache,
) -> Result<Value, ToolError> {
    let plain = || crate::tools::playbook_catalog(root, workspace_id, revision, limit);
    let query = query.trim();
    if query.is_empty() {
        return plain();
    }
    let Some(decider) = StandaloneDecider::for_projects(caller, root) else {
        return plain();
    };
    let mode = decider.mode_for(UseSite::CatalogRank);
    if mode == DecisionMode::Off || !decider.available() {
        return plain();
    }
    // The whole catalog, revision bypassed: the ranking depends on the query.
    let mut full = crate::tools::playbook_catalog(root, workspace_id, None, None)?;
    let key = (
        root.to_string_lossy().into_owned(),
        full["catalog_revision"].as_str().unwrap_or("").to_string(),
        apb_core::scope::digest_str(query),
    );
    let fields = match cache.get(&key) {
        Some(hit) => hit,
        None => {
            let fields = rank(&decider, &full, query);
            // A failure is not cached: the next call asks again.
            if fields.get("ranking").and_then(|r| r.get("error")).is_none() {
                cache.put(key, fields.clone());
            }
            fields
        }
    };
    if mode == DecisionMode::Shadow {
        return plain();
    }
    if let (Some(n), Some(entries)) = (limit, full["entries"].as_array_mut()) {
        entries.truncate(n);
    }
    if let (Value::Object(out), Value::Object(add)) = (&mut full, fields) {
        out.extend(add);
    }
    Ok(full)
}

/// A candidate: its ref (as the catalog prints it) and trigger.
struct Candidate {
    reference: Value,
    title: String,
    trigger: Value,
    /// The entry's `trusted`, `lifecycle` and `ambiguous`, repeated on a
    /// ranked item so a caller reads the facts next to the advice.
    facts: serde_json::Map<String, Value>,
}

fn candidates(catalog: &Value) -> Vec<Candidate> {
    catalog["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|e| e["shadowed"] != true)
                .map(|e| Candidate {
                    reference: e["ref"].clone(),
                    title: e["name"].as_str().unwrap_or("").to_string(),
                    trigger: e.get("trigger").cloned().unwrap_or(Value::Null),
                    facts: ["trusted", "lifecycle", "ambiguous"]
                        .into_iter()
                        .filter_map(|k| e.get(k).map(|v| (k.to_string(), v.clone())))
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `text` cut to at most `max` bytes on a char boundary.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// One playbook's state object within `share` bytes: examples go first,
/// then `avoid_when` items, then `when` items beyond the first, then every
/// string is shortened.
fn playbook_state(decider: &StandaloneDecider, c: &Candidate, share: usize) -> Value {
    let list = |k: &str| -> Vec<String> {
        c.trigger[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|t| decider.redact(t))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (mut when, mut avoid, mut examples) = (list("when"), list("avoid_when"), list("examples"));
    let build = |when: &[String], avoid: &[String], examples: &[String], title: &str| {
        let mut o = serde_json::Map::new();
        o.insert("title".into(), json!(title));
        o.insert("when".into(), json!(when));
        if !avoid.is_empty() {
            o.insert("avoid_when".into(), json!(avoid));
        }
        if !examples.is_empty() {
            o.insert("examples".into(), json!(examples));
        }
        Value::Object(o)
    };
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |s| s.len());
    let mut title = decider.redact(&c.title);
    loop {
        let v = build(&when, &avoid, &examples, &title);
        if size(&v) <= share {
            return v;
        }
        if examples.pop().is_some() || avoid.pop().is_some() {
            continue;
        }
        if when.len() > 1 {
            when.pop();
            continue;
        }
        // One `when` line and a title left: shorten both to fit.
        let each = share.saturating_sub(48) / 2;
        title = clip(&title, each.max(16));
        when = when.iter().map(|w| clip(w, each.max(16))).collect();
        return build(&when, &[], &[], &title);
    }
}

/// One request over a group of candidates; the final round (`with_extras`)
/// also asks `needs_playbook` and the coverage nouls.
fn ask(
    decider: &StandaloneDecider,
    query: &str,
    group: &[&Candidate],
    suppressed: &[Value],
    with_extras: bool,
) -> StandaloneOutcome {
    let budget = (decider.max_state_bytes() as f64 * STATE_SHARE) as usize;
    // Redacted before any clip (a cut can split a secret).
    let task = clip(&decider.redact(query), TASK_BYTES);
    // A suggestion's synopsis is repository text (the project store is
    // committed): it travels in the state, like every other untrusted text,
    // never in a question, and at most MAX_COVERAGE of them.
    let mut suggestions = serde_json::Map::new();
    if with_extras {
        for (i, s) in suppressed.iter().take(MAX_COVERAGE).enumerate() {
            let synopsis = s["synopsis"]
                .as_str()
                .filter(|t| !t.is_empty())
                .or_else(|| s["pattern"].as_str())
                .unwrap_or("");
            suggestions.insert(
                format!("s{i}"),
                json!(clip(&decider.redact(synopsis), SYNOPSIS_BYTES)),
            );
        }
    }
    let suggestions_bytes = serde_json::to_string(&suggestions).map_or(0, |t| t.len());
    let share = budget
        .saturating_sub(task.len() + suggestions_bytes + 64)
        .checked_div(group.len().max(1))
        .unwrap_or(0);
    let mut playbooks = serde_json::Map::new();
    let mut criteria = ChoiceCriteria::new();
    for (i, c) in group.iter().enumerate() {
        let id = format!("p{}", i + 1);
        playbooks.insert(id.clone(), playbook_state(decider, c, share));
        criteria = criteria.with(
            id.clone(),
            Some(json!(format!(
                "The playbook `playbooks.{id}` fits `task`: a `when` line matches it and no `avoid_when` line does."
            ))),
        );
    }
    criteria = criteria.with(NONE, Some(json!("No playbook in `playbooks` fits `task`.")));
    let mut questions = BTreeMap::from([(
        "best".to_string(),
        Question::Choice {
            instructions: json!("Which saved playbook should perform `task`?"),
            criteria,
        },
    )]);
    if with_extras {
        questions.insert(
            "needs_playbook".into(),
            Question::Noul {
                instructions: json!(
                    "Is `task` a doable action that a saved procedure could perform?"
                ),
                criteria: None,
            },
        );
        for i in 0..suggestions.len() {
            questions.insert(
                format!("covered_{i}"),
                Question::Noul {
                    instructions: json!(format!(
                        "Does the suggestion `suggestions.s{i}` describe the same procedure as `task`?"
                    )),
                    criteria: None,
                },
            );
        }
    }
    let (state, order) = if suggestions.is_empty() {
        (
            json!({"task": task, "playbooks": playbooks}),
            vec!["task".into(), "playbooks".into()],
        )
    } else {
        (
            json!({"task": task, "playbooks": playbooks, "suggestions": suggestions}),
            vec!["task".into(), "playbooks".into(), "suggestions".into()],
        )
    };
    decider.decide(UseSite::CatalogRank, state, order, questions)
}

fn error_fields(kind: &str) -> Value {
    json!({"ranking": {"error": kind}})
}

/// The probabilities of a `best` answer by candidate, `none_of_these` left
/// out, best first.
fn scored<'a>(
    answer: &Answer,
    group: &[&'a Candidate],
) -> Option<(Vec<(&'a Candidate, f64)>, f64)> {
    let Answer::Choice {
        probabilities,
        confidence,
        ..
    } = answer
    else {
        return None;
    };
    let mut out: Vec<(&Candidate, f64)> = group
        .iter()
        .enumerate()
        .map(|(i, c)| {
            (
                *c,
                probabilities
                    .get(&format!("p{}", i + 1))
                    .copied()
                    .unwrap_or(0.0),
            )
        })
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some((out, *confidence))
}

/// The advisory fields for one query.
fn rank(decider: &StandaloneDecider, catalog: &Value, query: &str) -> Value {
    let all = candidates(catalog);
    let suppressed: Vec<Value> = catalog["suppressed_suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut pool: Vec<&Candidate> = all.iter().collect();
    // Chunked first rounds: each chunk's leaders go on to the final round.
    while pool.len() > MAX_OPTIONS - 1 {
        let mut next = Vec::new();
        for chunk in pool.chunks(MAX_OPTIONS - 1) {
            match ask(decider, query, chunk, &[], false) {
                StandaloneOutcome::Answered(a) => {
                    match a.answers.get("best").and_then(|b| scored(b, chunk)) {
                        Some((s, _)) => next.extend(s.into_iter().take(TOP).map(|(c, _)| c)),
                        None => return error_fields("invalid"),
                    }
                }
                StandaloneOutcome::Skipped { reason } => return error_fields(reason),
                StandaloneOutcome::Failed { error_kind } => return error_fields(&error_kind),
            }
        }
        pool = next;
    }
    let answer = match ask(decider, query, &pool, &suppressed, true) {
        StandaloneOutcome::Answered(a) => a,
        StandaloneOutcome::Skipped { reason } => return error_fields(reason),
        StandaloneOutcome::Failed { error_kind } => return error_fields(&error_kind),
    };
    let Some((ranked, confidence)) = answer.answers.get("best").and_then(|b| scored(b, &pool))
    else {
        return error_fields("invalid");
    };
    let round = |x: f64| (x * 1e4).round() / 1e4;
    let mut out = json!({
        "ranked": ranked
            .iter()
            .take(TOP)
            .map(|(c, p)| {
                let mut item = c.facts.clone();
                item.insert("ref".into(), c.reference.clone());
                item.insert("p".into(), json!(round(*p)));
                Value::Object(item)
            })
            .collect::<Vec<_>>(),
        "confidence": round(confidence),
        "ranking": {
            "provider": answer.provider,
            "model": answer.model,
            "calibrated": answer.calibrated,
        },
    });
    if let Some(Answer::Noul { p }) = answer.answers.get("needs_playbook") {
        out["needs_playbook_p"] = json!(round(*p));
    }
    let cut = decider
        .threshold(UseSite::CatalogRank, "covered")
        .unwrap_or(apb_core::decisions::CATALOG_COVERED_CUT);
    let covering = suppressed
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match answer.answers.get(&format!("covered_{i}")) {
            Some(Answer::Noul { p }) if *p >= cut => Some((s, *p)),
            _ => None,
        })
        .max_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((s, p)) = covering {
        out["covered_by"] = json!({"pattern": s["pattern"], "scope": s["scope"], "p": round(p)});
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::decisions::{
        Budget, DataClass, EffectiveDecisions, Privacy, ProviderKind, ProviderSpec, UseSettings,
    };

    fn decider(root: &Path, best: Value) -> StandaloneDecider {
        let settings = EffectiveDecisions {
            mode: DecisionMode::Advise,
            timeout_ms: 1000,
            providers: vec![ProviderSpec {
                id: "fake".into(),
                kind: ProviderKind::Fake,
                base_url: None,
                model: None,
                data_class: DataClass::Local,
                key: None,
                answers: BTreeMap::from([("best".to_string(), best)]),
                account_id: None,
                zero_data_retention: false,
                structured_output: None,
            }],
            budget: Budget::default(),
            privacy: Privacy::default(),
            uses: BTreeMap::from([(
                "catalog_rank".to_string(),
                UseSettings {
                    allow_uncalibrated: false,
                    max_actions: None,
                    mode: DecisionMode::Advise,
                    thresholds: BTreeMap::new(),
                    max_requests_per_day: None,
                },
            )]),
        };
        StandaloneDecider::with_settings(settings, root)
    }

    fn catalog(n: usize) -> Value {
        let entries: Vec<Value> = (0..n)
            .map(|i| {
                json!({
                    "ref": {"origin": "project", "id": format!("pb-{i:03}")},
                    "name": format!("pb-{i:03}"),
                    "shadowed": false,
                    "trigger": {"when": [format!("do thing {i}")], "examples": ["x".repeat(400)]},
                })
            })
            .collect();
        json!({"entries": entries, "suppressed_suggestions": []})
    }

    fn requests(root: &Path) -> usize {
        std::fs::read_to_string(root.join(".apb/decisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    #[test]
    fn more_playbooks_than_options_are_ranked_in_chunks_then_leaders() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        let d = decider(
            root.path(),
            json!({"type": "choice", "choice": "p1", "probabilities": {"p1": 1.0}}),
        );
        let out = rank(&d, &catalog(300), "do thing 0");
        // 300 = 254 + 46: two chunk requests, then one over the ten leaders.
        assert_eq!(requests(root.path()), 3);
        assert_eq!(
            out["ranked"][0],
            json!({"ref": {"origin": "project", "id": "pb-000"}, "p": 1.0})
        );
        assert_eq!(out["ranked"].as_array().unwrap().len(), TOP);
    }

    #[test]
    fn the_state_stays_within_the_byte_budget() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        let d = decider(
            root.path(),
            json!({"type": "choice", "choice": "p1", "probabilities": {"p1": 1.0}}),
        );
        // 200 entries with long examples would be far over 24 kB unclipped.
        let out = rank(&d, &catalog(200), &"long task ".repeat(1_000));
        assert!(out["ranked"].is_array(), "{out}");
        let line: Value = serde_json::from_str(
            std::fs::read_to_string(root.path().join(".apb/decisions.jsonl"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert!(line["state_bytes"].as_u64().unwrap() <= 24_000);
    }

    #[test]
    fn an_invalid_best_answer_fails_open() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        let d = decider(root.path(), json!({"type": "choice", "choice": "zz"}));
        assert_eq!(
            rank(&d, &catalog(3), "task"),
            json!({"ranking": {"error": "invalid"}})
        );
    }
}
