//! The earlier successful run of a routine: which earlier run counts, the
//! checked lookup over the runs directory, the snapshot a start journals
//! on `run_provenance`, and the patch refusal that keeps a patch from
//! adding an unrecorded read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::schema::Playbook;

use crate::error::EngineError;
use crate::event::{Event, EventPayload, PreviousSuccess};
use crate::run_history::started_checked;
use crate::state::RunState;

/// Bytes one snapshotted output value may hold, on a char boundary.
pub(crate) const OUTPUT_MAX_BYTES: usize = 16 * 1024;

/// A run directory the lookup could not classify. Its message names the
/// directory and what did not read, never a value from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryUnreadable {
    pub run: String,
    pub what: &'static str,
}

/// The run directory of the latest earlier successful top-level run of
/// `playbook` in scope `scope_label` under `root`; `Ok(None)` when there
/// is none. `Err` when a stamped run directory that could be it cannot be
/// classified. Checked, not best effort: skipping an unreadable latest
/// success would reopen an older or the first-run window and repeat the
/// routine's outside actions.
pub(crate) fn find(
    root: &Path,
    playbook: &str,
    scope_label: &str,
) -> Result<Option<PathBuf>, HistoryUnreadable> {
    let runs_dir = root.join(".apb/runs");
    // A real directory, as every reader of the runs directory requires.
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    if !real_dir(&runs_dir) {
        return Ok(None);
    }
    let entries = std::fs::read_dir(&runs_dir).map_err(|_| HistoryUnreadable {
        run: runs_dir.display().to_string(),
        what: "runs directory",
    })?;
    let mut heads: Vec<(u128, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| HistoryUnreadable {
            run: runs_dir.display().to_string(),
            what: "runs directory",
        })?;
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let d = entry.path();
        // A run directory that arrived with the repository is never
        // evidence: skip it without refusing.
        if !crate::decision::report::created_here(&d) {
            continue;
        }
        let name = || {
            d.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let head = match started_checked(&d) {
            Ok(h) => h,
            Err(what) => {
                return Err(HistoryUnreadable { run: name(), what });
            }
        };
        let Some(head) = head else {
            continue;
        };
        // Same playbook id in the same scope (an absent label reads as
        // `project`, as `run_origin::parent_run_origin` does). Another
        // scope is a different window, never counted.
        if head.playbook != playbook || head.origin.as_deref().unwrap_or("project") != scope_label {
            continue;
        }
        heads.push((head.ts, d));
    }
    heads.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, d) in heads {
        let name = || {
            d.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        // The manifest must read (a corrupt one refuses) and name a
        // production run (a non-production one is skipped; an absent one
        // counts as production, as `production_dir` reads it).
        match crate::manifest::read(&d) {
            Err(_) => {
                return Err(HistoryUnreadable {
                    run: name(),
                    what: "manifest",
                });
            }
            Ok(None) => {}
            Ok(Some(m)) => {
                // An absent origin reads as production, as
                // `RunOrigin::of_read` does for every reader of earlier
                // runs.
                if !m
                    .origin
                    .as_ref()
                    .map(crate::manifest::RunOrigin::is_production)
                    .unwrap_or(true)
                {
                    continue;
                }
            }
        }
        // A top-level run only: a child start records the "none" window
        // because two parent routines of one child would share one
        // history.
        match crate::run_config::read_run_config(&d) {
            Err(_) => {
                return Err(HistoryUnreadable {
                    run: name(),
                    what: "run.yaml",
                });
            }
            Ok(cfg) => {
                if cfg.parent_run.is_some() {
                    continue;
                }
            }
        }
        let events = crate::event::read_all(&d).map_err(|_| HistoryUnreadable {
            run: name(),
            what: "journal",
        })?;
        if settled_succeeded(&events) {
            return Ok(Some(d));
        }
    }
    Ok(None)
}

/// Whether `events` ends settled and succeeded: the last `RunFinished`
/// says succeeded and nothing after it reopened the run (a revival, a
/// resume, a node or attempt start or finish, a pause or an abort). A run
/// revived after a finish and not finished again is not succeeded.
fn settled_succeeded(events: &[Event]) -> bool {
    let mut last: Option<&str> = None;
    let mut reopened = false;
    for e in events {
        match &e.payload {
            EventPayload::RunFinished { outcome } => {
                last = Some(outcome);
                reopened = false;
            }
            EventPayload::NodesRevived { .. }
            | EventPayload::RunResumed { .. }
            | EventPayload::NodeStarted { .. }
            | EventPayload::AttemptStarted { .. }
            | EventPayload::NodeFinished { .. }
            | EventPayload::RunPaused { .. }
            | EventPayload::RunAborted { .. } => reopened = true,
            _ => {}
        }
    }
    last == Some("succeeded") && !reopened
}

/// The times a run without an earlier success renders, over `params`
/// only, as `scheduler/prepare.rs` renders a task type over params.
fn when_none_times(
    playbook: &Playbook,
    params: &BTreeMap<String, String>,
) -> Result<(Option<String>, Option<String>, bool), EngineError> {
    let Some(block) = playbook
        .previous_success
        .as_ref()
        .and_then(|b| b.when_none.as_ref())
    else {
        return Ok((None, None, false));
    };
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let render_time = |field: &str, value: Option<&str>| -> Result<Option<String>, EngineError> {
        let Some(v) = value else { return Ok(None) };
        let rendered = crate::context::render(
            v,
            params,
            None,
            &empty,
            &BTreeMap::new(),
            &empty,
            &BTreeMap::new(),
            "",
            &BTreeMap::new(),
            &BTreeMap::new(),
            &crate::context::OutputClip {
                run_dir: Path::new("."),
                max_bytes: 0,
                retro: None,
            },
        );
        let trimmed = rendered.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if apb_core::duration::parse_rfc3339_ms(trimmed).is_none() {
            return Err(EngineError::Invalid(format!(
                "previous_success.when_none.{field} does not render an RFC 3339 time"
            )));
        }
        Ok(Some(trimmed.to_string()))
    };
    let started_at = render_time("started_at", block.started_at.as_deref())?;
    let finished_at = render_time("finished_at", block.finished_at.as_deref())?;
    Ok((started_at, finished_at, true))
}

/// The snapshot of `earlier` (or the `when_none` fallback when there is
/// none), as the start journals it. Every snapshotted value is redacted
/// with the earlier run's own redactor and clipped on a char boundary.
pub(crate) fn snapshot(
    root: &Path,
    earlier: Option<&Path>,
    playbook: &Playbook,
    params: &BTreeMap<String, String>,
) -> Result<PreviousSuccess, EngineError> {
    let Some(earlier) = earlier else {
        let (started_at, finished_at, fallback) = when_none_times(playbook, params)?;
        return Ok(PreviousSuccess {
            started_at,
            finished_at,
            fallback,
            ..Default::default()
        });
    };
    let name = earlier
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let head = crate::run_history::started(earlier);
    let started_at = head.as_ref().map(|h| apb_core::clock::iso_utc(h.ts as u64));
    // The time of its last `run_finished`, same format.
    // `find` just read this journal; one that no longer reads refuses the
    // start rather than record the run with empty outputs.
    let events = crate::event::read_all(earlier).map_err(|_| {
        EngineError::Invalid(format!(
            "previous_success_unreadable: run `{name}`: its journal does not read; repair or remove it, then start again"
        ))
    })?;
    let finished_at = events
        .iter()
        .rev()
        .find_map(|e| matches!(&e.payload, EventPayload::RunFinished { .. }).then_some(e.ts as u64))
        .map(apb_core::clock::iso_utc);
    let fold = RunState::fold(&events);
    let redactor = crate::run_redact::RunRedactor::for_run(root, earlier);
    let mut outputs = BTreeMap::new();
    let mut withheld = Vec::new();
    for r in apb_core::previous_success::refs(playbook) {
        let apb_core::previous_success::PrevRef::Output { node, field } = r else {
            continue;
        };
        let key = apb_core::previous_success::key(&apb_core::previous_success::PrevRef::Output {
            node: node.clone(),
            field: field.clone(),
        });
        let Some(output) = fold.outputs.get(&node) else {
            continue;
        };
        let text = match &field {
            Some(f) => apb_core::schema::output_field_text(output, f).unwrap_or_default(),
            None => output.clone(),
        };
        if text.is_empty() {
            continue;
        }
        match redactor.secret_text(&text) {
            Err(_) => withheld.push(key),
            Ok(redacted) => {
                outputs.insert(key, clip_value(&redacted));
            }
        }
    }
    Ok(PreviousSuccess {
        run_id: Some(name),
        started_at,
        finished_at,
        outputs,
        withheld,
        fallback: false,
    })
}

/// Clips `text` to `OUTPUT_MAX_BYTES` on a char boundary, appending
/// `(clipped)` on a line of its own when cut.
fn clip_value(text: &str) -> String {
    if text.len() <= OUTPUT_MAX_BYTES {
        return text.to_string();
    }
    let mut end = OUTPUT_MAX_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n(clipped)", &text[..end])
}

/// What `run.previous_success.*` renders in the run whose state this is,
/// as a flat map: `id`, `started_at` and `finished_at` when set, then
/// every `outputs` entry under `nodes.<key>`. A judge state field and a
/// cache key read the same map, so they agree with the prompts.
pub(crate) fn template_values(state: &RunState) -> BTreeMap<String, String> {
    match state.previous_success.as_ref() {
        None => BTreeMap::new(),
        Some(ps) => record_values(ps),
    }
}

/// The same map straight from the record, for readers that hold one
/// instead of a fold.
pub(crate) fn record_values(ps: &PreviousSuccess) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(id) = &ps.run_id {
        out.insert("id".to_string(), id.clone());
    }
    if let Some(t) = &ps.started_at {
        out.insert("started_at".to_string(), t.clone());
    }
    if let Some(t) = &ps.finished_at {
        out.insert("finished_at".to_string(), t.clone());
    }
    // The snapshot keys already carry their `nodes.` prefix: they are the
    // reference text after `run.previous_success.`.
    for (k, v) in &ps.outputs {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Why a patch from `old` to `new` must be refused, if it must: `new`
/// reads the namespace while the run recorded no snapshot, or reads an
/// output key that `old` did not read (the start never snapshotted it).
/// A key the old definition read but the snapshot lacks is not a
/// refusal: it renders empty in both.
pub(crate) fn patch_refusal(old: &Playbook, new: &Playbook, events: &[Event]) -> Option<String> {
    let recorded = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance {
            previous_success, ..
        } => previous_success.clone(),
        _ => None,
    });
    if recorded.is_none() {
        return apb_core::previous_success::reads(new).then(|| {
            "previous_success_unrecorded: the run recorded no run.previous_success at start; start a new run instead".to_string()
        });
    }
    // A key the start snapshotted stays readable even when a patch in
    // between removed its read; `old` adds the keys read at start whose
    // node the earlier run did not run.
    let mut old_keys: std::collections::BTreeSet<String> = apb_core::previous_success::refs(old)
        .iter()
        .map(apb_core::previous_success::key)
        .collect();
    if let Some(ps) = &recorded {
        old_keys.extend(ps.outputs.keys().cloned());
        old_keys.extend(ps.withheld.iter().cloned());
    }
    for r in apb_core::previous_success::refs(new) {
        if matches!(r, apb_core::previous_success::PrevRef::Output { .. }) {
            let k = apb_core::previous_success::key(&r);
            if !old_keys.contains(&k) {
                return Some(format!(
                    "previous_success_unrecorded: run.previous_success.{k} was not read at start; start a new run instead"
                ));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeds a stamped run of playbook `p`: the manifest origin
    /// `manifest`, the `run_provenance` origin label, `parent_run` in its
    /// run.yaml when given, a succeeded `run_finished` when `finish`, a
    /// `run_resumed` after it when `reopen`, and a torn head line when
    /// `torn`.
    #[allow(clippy::too_many_arguments)]
    fn seed(
        root: &Path,
        id: &str,
        ts: u64,
        manifest: crate::manifest::RunOrigin,
        origin_label: &str,
        parent: Option<&str>,
        finish: bool,
        reopen: bool,
        torn: bool,
    ) {
        let dir = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        let mut seq = 0u64;
        let mut push = |body: serde_json::Value| {
            seq += 1;
            let mut v = body;
            v["seq"] = seq.into();
            v["ts"] = ts.into();
            lines.push_str(&format!("{}\n", serde_json::to_string(&v).unwrap()));
        };
        push(serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}));
        push(serde_json::json!({"type": "run_provenance", "origin": origin_label}));
        if finish {
            push(serde_json::json!({"type": "run_finished", "outcome": "succeeded"}));
        }
        if reopen {
            push(serde_json::json!({"type": "run_resumed", "from_node": "a"}));
        }
        if torn {
            lines.push_str("not json\n");
        }
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
        if let Some(parent) = parent {
            let cfg = crate::run_config::RunConfig {
                parent_run: Some(parent.into()),
                ..Default::default()
            };
            crate::run_config::write_run_config(&dir, &cfg).unwrap();
        }
        let m = crate::manifest::RunExecutionManifest {
            origin: Some(manifest),
            ..Default::default()
        };
        crate::manifest::write(&dir, &m).unwrap();
        apb_core::run_origin::stamp(&dir, id).unwrap();
    }

    /// Runs `f` under a fresh config dir in a plain child process, in a
    /// fresh project with an empty runs directory.
    fn child(name: &str, f: impl FnOnce(&Path)) {
        apb_core::process_mode::testing::in_plain_child(name, || {
            let cfg = tempfile::tempdir().unwrap();
            // SAFETY: a child process runs this one test on one thread.
            unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
            apb_core::run_origin::ensure_key().unwrap();
            let project = tempfile::tempdir().unwrap();
            let root = project.path();
            std::fs::create_dir_all(root.join(".apb/runs")).unwrap();
            f(root);
        });
    }

    /// The id of the run directory the lookup found, empty for none.
    fn id_of(found: Option<PathBuf>) -> String {
        found
            .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// A failed newer run is skipped; the older success wins.
    #[test]
    fn a_failed_newer_run_is_skipped() {
        child(
            "previous_success::tests::a_failed_newer_run_is_skipped",
            |root| {
                seed(
                    root,
                    "old",
                    1,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "failed",
                    2,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    false,
                    false,
                    false,
                );
                let found = find(root, "p", "project").unwrap();
                assert_eq!(id_of(found), "old");
            },
        );
    }

    /// Another scope is skipped; a child run is skipped; a scenario and an
    /// eval run are skipped.
    #[test]
    fn another_scope_a_child_a_scenario_and_an_eval_run_are_skipped() {
        child(
            "previous_success::tests::another_scope_a_child_a_scenario_and_an_eval_run_are_skipped",
            |root| {
                seed(
                    root,
                    "globalrun",
                    5,
                    crate::manifest::RunOrigin::Production,
                    "global",
                    None,
                    true,
                    false,
                    false,
                );
                assert_eq!(find(root, "p", "project").unwrap(), None);
                assert_eq!(id_of(find(root, "p", "global").unwrap()), "globalrun");

                seed(
                    root,
                    "childrun",
                    6,
                    crate::manifest::RunOrigin::Production,
                    "global",
                    Some("x"),
                    true,
                    false,
                    false,
                );
                assert_eq!(id_of(find(root, "p", "global").unwrap()), "globalrun");

                seed(
                    root,
                    "scenario",
                    7,
                    crate::manifest::RunOrigin::Scenario {
                        level: apb_core::scenario::ScenarioLevel::Smoke,
                        scenario: "green".into(),
                        scenario_digest: "sha256:s".into(),
                    },
                    "global",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "eval",
                    8,
                    crate::manifest::RunOrigin::Eval,
                    "global",
                    None,
                    true,
                    false,
                    false,
                );
                assert_eq!(id_of(find(root, "p", "global").unwrap()), "globalrun");
            },
        );
    }

    /// A run revived after `run_finished` and not finished again is not
    /// succeeded. An unstamped directory is skipped even when it would be
    /// a candidate, and a start in progress (no `run_started` yet) does
    /// not refuse.
    #[test]
    fn revived_unstamped_and_in_progress_are_not_candidates() {
        child(
            "previous_success::tests::revived_unstamped_and_in_progress_are_not_candidates",
            |root| {
                seed(
                    root,
                    "ok",
                    1,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "revived",
                    2,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    true,
                    false,
                );
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "ok");

                // A run directory that arrived with the repository: never
                // evidence, never a refusal.
                let foreign = root.join(".apb/runs/foreign");
                std::fs::create_dir_all(&foreign).unwrap();
                std::fs::write(foreign.join("events.jsonl"), "{\"seq\":0,\"ts\":9,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n").unwrap();
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "ok");

                // A start in progress: the head has no run_started yet.
                let running = root.join(".apb/runs/inprogress");
                std::fs::create_dir_all(&running).unwrap();
                std::fs::write(
                    running.join("events.jsonl"),
                    "{\"seq\":0,\"ts\":9,\"type\":\"run_queued\",\"reason\":\"workdir\"}\n",
                )
                .unwrap();
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "ok");
            },
        );
    }

    /// A success without a manifest counts as production, and a head line
    /// of an event type a newer apb wrote is skipped, not refused.
    #[test]
    fn a_success_without_a_manifest_and_with_an_unknown_head_event_counts() {
        child(
            "previous_success::tests::a_success_without_a_manifest_and_with_an_unknown_head_event_counts",
            |root| {
                seed(
                    root,
                    "bare",
                    10,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                let dir = root.join(".apb/runs/bare");
                std::fs::remove_file(dir.join("manifest.yaml")).unwrap();
                let journal = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
                // Insert a newer apb's event after `run_started` and
                // renumber, so `seq` stays in order.
                let mut lines: Vec<serde_json::Value> = journal
                    .lines()
                    .map(|l| serde_json::from_str(l).unwrap())
                    .collect();
                lines.insert(1, serde_json::json!({"ts": 10, "type": "a_future_event"}));
                let text: String = lines
                    .iter_mut()
                    .enumerate()
                    .map(|(i, v)| {
                        v["seq"] = (i as u64 + 1).into();
                        format!("{v}\n")
                    })
                    .collect();
                std::fs::write(dir.join("events.jsonl"), text).unwrap();
                apb_core::run_origin::stamp(&dir, "bare").unwrap();
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "bare");
            },
        );
    }

    /// The newest success wins by `run_started` time, not file time.
    #[test]
    fn the_newest_success_wins_by_run_started_time() {
        child(
            "previous_success::tests::the_newest_success_wins_by_run_started_time",
            |root| {
                seed(
                    root,
                    "newer_ts",
                    20,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "older_ts",
                    10,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "newer_ts");
            },
        );
    }

    /// A stamped candidate newer than the latest success whose journal
    /// does not read refuses the lookup, naming the run and what did not
    /// read.
    #[test]
    fn a_journal_that_does_not_read_refuses_the_lookup() {
        child(
            "previous_success::tests::a_journal_that_does_not_read_refuses_the_lookup",
            |root| {
                seed(
                    root,
                    "good",
                    1,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "torn",
                    2,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    true,
                );
                let err = find(root, "p", "project").unwrap_err();
                assert_eq!((err.run.as_str(), err.what), ("torn", "journal"));
            },
        );
    }

    /// A run.yaml that does not read refuses the lookup the same way.
    #[test]
    fn a_run_yaml_that_does_not_read_refuses_the_lookup() {
        child(
            "previous_success::tests::a_run_yaml_that_does_not_read_refuses_the_lookup",
            |root| {
                seed(
                    root,
                    "good",
                    1,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "badrunyaml",
                    2,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                std::fs::write(
                    root.join(".apb/runs/badrunyaml/run.yaml"),
                    "mode: guarded\n",
                )
                .unwrap();
                let err = find(root, "p", "project").unwrap_err();
                assert_eq!((err.run.as_str(), err.what), ("badrunyaml", "run.yaml"));
            },
        );
    }

    /// A manifest corrupted after the stamp breaks the stamp itself: the
    /// directory no longer verifies as created here and is skipped as
    /// foreign, so the older success the walk reaches is the answer.
    #[test]
    fn a_manifest_corrupted_after_its_stamp_is_skipped_as_foreign() {
        child(
            "previous_success::tests::a_manifest_corrupted_after_its_stamp_is_skipped_as_foreign",
            |root| {
                seed(
                    root,
                    "good",
                    1,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                seed(
                    root,
                    "badmanifest",
                    2,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                std::fs::write(
                    root.join(".apb/runs/badmanifest/manifest.yaml"),
                    "origin: [x\n",
                )
                .unwrap();
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "good");
            },
        );
    }

    /// Unreadable runs older than the latest success are never reached:
    /// the walk stops at the success.
    #[test]
    fn an_unreadable_run_older_than_the_success_is_not_reached() {
        child(
            "previous_success::tests::an_unreadable_run_older_than_the_success_is_not_reached",
            |root| {
                // A journal whose defect sits beyond the head lines: the
                // head reads, the full journal would not.
                seed(
                    root,
                    "olddeep",
                    0,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                let old = root.join(".apb/runs/olddeep");
                let mut lines = String::new();
                for n in 0..12 {
                    let mut v = serde_json::json!({"type": "node_finished", "node": "a", "status": "succeeded", "attempt": 1, "output": "", "artifacts": []});
                    v["seq"] = ((n + 3) as u64).into();
                    v["ts"] = 0.into();
                    lines.push_str(&format!("{}\n", serde_json::to_string(&v).unwrap()));
                }
                lines.push_str("not json\n");
                {
                    use std::io::Write as _;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(old.join("events.jsonl"))
                        .unwrap()
                        .write_all(lines.as_bytes())
                        .unwrap();
                }
                seed(
                    root,
                    "newgood",
                    1,
                    crate::manifest::RunOrigin::Production,
                    "project",
                    None,
                    true,
                    false,
                    false,
                );
                assert_eq!(id_of(find(root, "p", "project").unwrap()), "newgood");
            },
        );
    }

    /// Seeds a full earlier run with a node output and a secret param, so
    /// the snapshot reads its times, its fold and its redactor.
    fn seed_earlier(root: &Path, id: &str, started_ms: u64, finished_ms: u64, output: &str) {
        let dir = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (seq, body) in [
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            serde_json::json!({"type": "node_finished", "node": "a", "status": "succeeded", "attempt": 1, "output": output, "artifacts": []}),
            serde_json::json!({"type": "run_finished", "outcome": "succeeded"}),
        ]
        .into_iter()
        .enumerate()
        {
            let mut v = body;
            v["seq"] = (seq as u64).into();
            v["ts"] = if seq == 2 { finished_ms.into() } else { started_ms.into() };
            lines.push_str(&format!("{}\n", serde_json::to_string(&v).unwrap()));
        }
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
        let cfg = crate::run_config::RunConfig {
            params: [("message".to_string(), "violet hedgehog".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        crate::run_config::write_run_config(&dir, &cfg).unwrap();
        std::fs::write(dir.join("playbook.yaml"), "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: message, type: text, secret: true }\nnodes:\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: a, to: done }\n").unwrap();
        let m = crate::manifest::RunExecutionManifest {
            origin: Some(crate::manifest::RunOrigin::Production),
            ..Default::default()
        };
        crate::manifest::write(&dir, &m).unwrap();
        apb_core::run_origin::stamp(&dir, id).unwrap();
    }

    /// The current playbook the snapshot reads with: the earlier run's
    /// times, id and node output.
    fn reading_playbook() -> apb_core::schema::Playbook {
        apb_core::schema::Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 2.0.0\nnodes:\n  - { id: r, type: agent_task, prompt: \"{{run.previous_success.started_at}} {{run.previous_success.id}} {{run.previous_success.nodes.a.output}} {{run.previous_success.nodes.a.output.lead_ids}}\" }\nedges: []\n",
        )
        .unwrap()
    }

    fn snapshot_child(name: &str, f: impl FnOnce(&Path)) {
        apb_core::process_mode::testing::in_plain_child(name, || {
            let project = tempfile::tempdir().unwrap();
            let root = project.path();
            std::fs::create_dir_all(root.join(".apb/runs")).unwrap();
            f(root);
        });
    }

    /// Times are `iso_utc` (milliseconds truncated), an array field is
    /// compact JSON, an absent node adds no key, and a value over 16 KiB
    /// is cut on a char boundary with `(clipped)`.
    #[test]
    fn snapshot_times_fields_and_clip() {
        snapshot_child(
            "previous_success::tests::snapshot_times_fields_and_clip",
            |root| {
                seed_earlier(
                    root,
                    "r1",
                    1_762_419_600_900,
                    1_762_419_605_000,
                    "{\"lead_ids\": [3, 7], \"note\": \"x\"}",
                );
                let pb = reading_playbook();
                let ps = snapshot(
                    root,
                    Some(&root.join(".apb/runs/r1")),
                    &pb,
                    &BTreeMap::new(),
                )
                .unwrap();
                assert_eq!(ps.run_id.as_deref(), Some("r1"));
                assert_eq!(ps.started_at.as_deref(), Some("2025-11-06T09:00:00Z"));
                assert_eq!(ps.finished_at.as_deref(), Some("2025-11-06T09:00:05Z"));
                // The bare output read is the stored text; the field read is
                // compact JSON.
                assert_eq!(
                    ps.outputs.get("nodes.a.output").map(String::as_str),
                    Some("{\"lead_ids\": [3, 7], \"note\": \"x\"}")
                );
                assert_eq!(
                    ps.outputs
                        .get("nodes.a.output.lead_ids")
                        .map(String::as_str),
                    Some("[3,7]")
                );
                assert!(!ps.outputs.contains_key("nodes.absent.output"));

                let long = "h".repeat(OUTPUT_MAX_BYTES + 900);
                seed_earlier(root, "r2", 1_762_419_600_900, 1_762_419_605_000, &long);
                let ps = snapshot(
                    root,
                    Some(&root.join(".apb/runs/r2")),
                    &pb,
                    &BTreeMap::new(),
                )
                .unwrap();
                let text = ps.outputs.get("nodes.a.output").unwrap();
                assert!(text.ends_with("\n(clipped)"));
                assert!(text.len() < OUTPUT_MAX_BYTES + 900);
            },
        );
    }

    /// The earlier run's secret param value is replaced; a run whose
    /// run.yaml does not read puts the key in `withheld`.
    #[test]
    fn snapshot_redacts_and_withholds() {
        snapshot_child(
            "previous_success::tests::snapshot_redacts_and_withholds",
            |root| {
                seed_earlier(
                    root,
                    "r1",
                    1_762_419_600_900,
                    1_762_419_605_000,
                    "said violet hedgehog",
                );
                let pb = reading_playbook();
                let ps = snapshot(
                    root,
                    Some(&root.join(".apb/runs/r1")),
                    &pb,
                    &BTreeMap::new(),
                )
                .unwrap();
                assert_eq!(
                    ps.outputs.get("nodes.a.output").map(String::as_str),
                    Some("said [redacted]")
                );
                assert!(ps.withheld.is_empty());

                std::fs::write(root.join(".apb/runs/r1/run.yaml"), "mode: guarded\n").unwrap();
                let ps = snapshot(
                    root,
                    Some(&root.join(".apb/runs/r1")),
                    &pb,
                    &BTreeMap::new(),
                )
                .unwrap();
                assert!(!ps.outputs.contains_key("nodes.a.output"));
                assert_eq!(ps.withheld, vec!["nodes.a.output"]);
            },
        );
    }

    /// Without an earlier run the `when_none` times render over params
    /// with `fallback: true`; a value that does not render an RFC 3339
    /// time refuses without naming the value.
    #[test]
    fn when_none_renders_and_refuses() {
        snapshot_child(
            "previous_success::tests::when_none_renders_and_refuses",
            |root| {
                let pb = apb_core::schema::Playbook::from_yaml(
                    "schema: 2\nid: p\nname: p\nversion: 2.0.0\nparams:\n  - { name: since, type: text, default: \"2026-01-01T00:00:00Z\" }\nprevious_success:\n  when_none:\n    started_at: \"{{params.since}}\"\nnodes:\n  - { id: r, type: agent_task, prompt: p }\nedges: []\n",
                )
                .unwrap();
                let mut params = BTreeMap::new();
                params.insert("since".to_string(), "2026-01-01T00:00:00Z".to_string());
                let ps = snapshot(root, None, &pb, &params).unwrap();
                assert!(ps.fallback);
                assert_eq!(ps.started_at.as_deref(), Some("2026-01-01T00:00:00Z"));
                assert_eq!(ps.run_id, None);

                let bad = apb_core::schema::Playbook::from_yaml(
                    "schema: 2\nid: p\nname: p\nversion: 2.0.0\nparams:\n  - { name: since, type: text, default: yesterday }\nprevious_success:\n  when_none:\n    started_at: \"{{params.since}}\"\nnodes:\n  - { id: r, type: agent_task, prompt: p }\nedges: []\n",
                )
                .unwrap();
                let mut bad_params = BTreeMap::new();
                bad_params.insert("since".to_string(), "yesterday".to_string());
                let err = snapshot(root, None, &bad, &bad_params).unwrap_err();
                let text = err.to_string();
                assert!(text.contains("does not render an RFC 3339 time"), "{text}");
                assert!(!text.contains("yesterday"), "{text}");
            },
        );
    }
}
