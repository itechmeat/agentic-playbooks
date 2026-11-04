//! The run inventory: a compact summary per run directory, sorted newest
//! first, and the filtered listing with each run's search fields
//! ([`list_runs_filtered`], `apb_core::run_search`). Shares the parent
//! module's imports via `use super::*`.

use super::*;

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub playbook: String,
    pub status: String,
    pub started_ts: u128,
    /// Active milliseconds from `run_started` to the run's end (`run_finished`
    /// or `run_aborted`), paused time left out: the same duration `apb stats`
    /// counts. Absent while the run has not ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub duration_ms: Option<u128>,
    /// Milliseconds a run that has not ended was inactive so far (paused,
    /// or ended before a resume brought it back), so a listing can count its
    /// active time up from `started_ts` without them. Absent once the run
    /// ended (see `duration_ms`) and for a run that was never inactive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub paused_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub progress: Option<crate::progress::ProgressSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// The run has a drive claim and that claim's process is provably gone: the
    /// journal still reads `running` because the only thing that writes a
    /// terminal event is the drive loop that no longer exists. The single-run
    /// surfaces (`run_status`, `apb doctor --run`) have answered this for a
    /// while; a listing could not, so a killed driver looked healthy in a table.
    /// `false` also covers "no drive claim at all", which is not a problem,
    /// and a run that has ended ([`crate::run_view::RunView::driver_dead`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub driver_dead: bool,
    /// The run's driver let go while a deferred retry waits: a waker
    /// resumes it at `wake_at_ms` ([`crate::run_view::RunView::released`]).
    /// Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub released: Option<crate::state::Released>,
    /// Events of a type this binary does not know, skipped when the journal
    /// was read: a newer apb wrote them. Zero (and absent) for a journal this
    /// binary reads in full.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<usize>", optional))]
    pub unknown_events: usize,
    /// The run was a trial of the playbook's candidate version (issue #192),
    /// with its verdict; absent for every other run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub candidate_trial: Option<crate::candidate::CandidateTrial>,
    /// The run's pause (issue #200): who paused it, why, since and until
    /// when, and `pausing` while a live driver drains toward a queued pause.
    /// Absent for a run that is neither paused nor pausing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub pause: Option<crate::pause::PauseInfo>,
    /// Why the run needs a person's look: a baseline regression, or
    /// findings or warnings its attempts reported. Absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub needs_look: Option<crate::run_outcome::NeedsLook>,
    /// Where the run's playbook came from: `project` or `global`
    /// (`run_provenance.origin`; a run without provenance is `project`).
    /// Only in a filtered listing ([`list_runs_filtered`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
    /// The run's search fields; only in a filtered listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub search: Option<apb_core::run_search::SearchFields>,
    /// The run's own `run.yaml` could not be read, with the error: the
    /// run still lists with its journal facts, and only this run carries
    /// the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub config_error: Option<String>,
}

/// What a filtered listing keeps. Every filter given is an AND.
#[derive(Debug, Clone, Default)]
pub struct RunFilter {
    pub playbook: Option<String>,
    /// `project` or `global`.
    pub scope: Option<String>,
    /// Equality terms over the runs' search fields.
    pub fields: Vec<apb_core::run_search::Term>,
    /// Give every row its `scope` and `search` even with no filter (the
    /// dashboard's filter row offers the field names present).
    pub with_fields: bool,
}

/// A filtered listing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize)]
pub struct RunListing {
    pub runs: Vec<RunSummary>,
    /// Runs a field term dropped because they have no search fields; zero
    /// when the filter has no field terms, because then nothing is left
    /// out of `runs`.
    pub not_indexed: usize,
    /// Runs left out because a term names a field whose value is withheld
    /// or too long (a non-match a query cannot probe).
    pub withheld: usize,
}

/// The longest start definition a listing reads for search fields.
const MAX_DEFINITION_BYTES: usize = 1 << 20;

/// `project` or `global`: the run's `run_provenance.origin`, `project`
/// without one (the only kind before provenance existed).
fn scope_of(events: &[crate::event::Event]) -> String {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { origin, .. } => origin.clone(),
            _ => None,
        })
        .unwrap_or_else(|| "project".to_string())
}

/// The run's search fields from its own start definition (the digest its
/// `run_started` journaled, read from the run's stored definitions, never
/// the `playbook.yaml` a patch may have replaced) and its params. `None`
/// when it is not indexed: no stored start definition, no params, or no
/// searchable param.
fn search_fields(
    run_dir: &Path,
    events: &[crate::event::Event],
    params: Option<&BTreeMap<String, String>>,
    redaction: &crate::run_redact::WorkspaceRedaction,
) -> Option<Vec<apb_core::run_search::IndexedField>> {
    let digest = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { definition, .. } => Some(definition.clone()),
        _ => None,
    })??;
    // Length first: a definition over the cap is not indexed, and the
    // check must come before anything reads and hashes the whole file.
    if crate::run_definitions::stored_len(run_dir, &digest)
        .is_some_and(|len| len > MAX_DEFINITION_BYTES as u64)
    {
        return None;
    }
    let bytes = crate::run_definitions::read_stored(run_dir, &digest)?;
    if bytes.len() > MAX_DEFINITION_BYTES {
        return None;
    }
    let playbook = Playbook::from_yaml(std::str::from_utf8(&bytes).ok()?).ok()?;
    let declared: Vec<&apb_core::schema::Param> = playbook
        .params
        .iter()
        .filter(|p| apb_core::run_search::searchable(p))
        .take(apb_core::run_search::MAX_SEARCH_PARAMS)
        .collect();
    if declared.is_empty() {
        return None;
    }
    let params = params?;
    let redactor = redaction.run_dir(run_dir);
    Some(
        declared
            .into_iter()
            .map(|p| {
                let raw = params.get(&p.name).map(String::as_str);
                // The run's own sensitive rule (a param the run counts
                // secret, one named like a secret, a credential-shaped
                // value) keeps the field out of the listing even when the
                // value is too short for the scrubber to replace by shape.
                let sensitive = raw.is_some_and(|v| {
                    apb_core::run_params::sensitive(redactor.declares_secret(&p.name), &p.name, v)
                        .is_some()
                });
                let kept = !sensitive
                    && raw.is_some_and(|v| {
                        !redactor.withholds()
                            && !redactor.holds_value(v)
                            && redactor.redact_str(v) == v
                    });
                apb_core::run_search::field(p, raw, kept)
            })
            .collect(),
    )
}

/// Active time from the start to the last `run_finished` / `run_aborted`, or
/// `None` for a run that has not ended (or never recorded its start). A
/// `run_resumed` after the end makes the run live again, as in
/// `run_retro::run_end`, so it has no duration until it ends once more, and
/// the span it sat ended before that resume is not run time either.
fn run_duration_ms(events: &[crate::event::Event], started_ts: u128) -> Option<u128> {
    if started_ts == 0 {
        return None;
    }
    let end = events.iter().rev().find_map(|e| match e.payload {
        EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => Some(Some(e.ts)),
        EventPayload::RunResumed { .. } => Some(None),
        _ => None,
    })??;
    Some(crate::pause::active_run_ms(events, started_ts, end, end))
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

pub fn list_runs(root: &Path) -> Result<Vec<RunSummary>, EngineError> {
    Ok(list_runs_with(root, None)?
        .into_iter()
        .map(|(summary, _)| summary)
        .collect())
}

/// The runs `filter` keeps, each with its `scope` and `search` fields.
pub fn list_runs_filtered(root: &Path, filter: &RunFilter) -> Result<RunListing, EngineError> {
    use apb_core::run_search::{Match, SearchFields, matches};
    let redaction = crate::run_redact::WorkspaceRedaction::new(root);
    let mut out = RunListing {
        runs: Vec::new(),
        not_indexed: 0,
        withheld: 0,
    };
    for (mut summary, fields) in list_runs_with(root, Some(&redaction))? {
        if filter
            .playbook
            .as_ref()
            .is_some_and(|p| *p != summary.playbook)
            || filter
                .scope
                .as_ref()
                .is_some_and(|s| Some(s) != summary.scope.as_ref())
        {
            continue;
        }
        match fields {
            None => {
                if !filter.fields.is_empty() {
                    out.not_indexed += 1;
                    continue;
                }
                summary.search = Some(SearchFields::NotIndexed);
            }
            Some(fields) => {
                match matches(&fields, &filter.fields) {
                    Match::Yes => {}
                    Match::No => continue,
                    Match::Withheld => {
                        out.withheld += 1;
                        continue;
                    }
                }
                summary.search = Some(SearchFields::Indexed {
                    fields: fields.iter().map(|f| f.view()).collect(),
                });
            }
        }
        out.runs.push(summary);
    }
    Ok(out)
}

/// Every run's summary, with its scope and search fields when `search`
/// gives the workspace's redaction.
type Listed = (RunSummary, Option<Vec<apb_core::run_search::IndexedField>>);

fn list_runs_with(
    root: &Path,
    search: Option<&crate::run_redact::WorkspaceRedaction>,
) -> Result<Vec<Listed>, EngineError> {
    let runs_dir = root.join(".apb/runs");
    let mut out = Vec::new();
    // One reference time for the whole listing.
    let now = apb_core::clock::now_ms();
    if !runs_dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&runs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        // One corrupted/legacy run (for example, events.jsonl with old schema,
        // where `ts` was a string or a truncated record) should not crash
        // the entire listing - skip such a directory and show the rest. A
        // torn LAST line is not that: it is a line the driver is still
        // writing, and the run view reads through it.
        let Ok(view) = crate::run_view::RunView::load(&entry.path(), &run_id) else {
            continue;
        };
        if view.events.is_empty() {
            continue;
        }
        let (playbook, started_ts) = view
            .events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { playbook, .. } => Some((playbook.clone(), e.ts)),
                _ => None,
            })
            .unwrap_or_else(|| (run_id.clone(), 0));
        let duration_ms = run_duration_ms(&view.events, started_ts);
        let paused_ms = if duration_ms.is_none() && started_ts > 0 {
            Some(crate::pause::inactive_ms_between(
                &view.events,
                started_ts,
                now,
                now,
            ))
            .filter(|ms| *ms > 0)
        } else {
            None
        };
        let (cfg, config_error) = match crate::run_config::read_run_config(&entry.path()) {
            Ok(cfg) => (Some(cfg), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let parent_run = cfg.as_ref().and_then(|c| c.parent_run.clone());
        let continued_from = cfg.as_ref().and_then(|c| c.continued_from.clone());
        let superseded_by = cfg.as_ref().and_then(|c| c.superseded_by.clone());
        let driver_dead = view.driver_dead();
        let released = view.released().cloned();
        let unknown_events = view.unknown.len();
        let status = view.run_status.as_str().into();
        let candidate_trial = crate::candidate::trial_of(&entry.path(), &view.events);
        let pause = view.pause().cloned();
        let needs_look = crate::run_outcome::NeedsLook::for_run(&entry.path(), &view.events);
        let (scope, fields) = match search {
            Some(redaction) => (
                Some(scope_of(&view.events)),
                search_fields(
                    &entry.path(),
                    &view.events,
                    cfg.as_ref().map(|c| &c.params),
                    redaction,
                ),
            ),
            None => (None, None),
        };
        let progress = view.progress;
        out.push((
            RunSummary {
                scope,
                search: None,
                needs_look,
                pause,
                run_id,
                playbook,
                status,
                started_ts,
                duration_ms,
                paused_ms,
                progress,
                parent_run,
                continued_from,
                superseded_by,
                driver_dead,
                released,
                unknown_events,
                candidate_trial,
                config_error,
            },
            fields,
        ));
    }
    out.sort_by_key(|(s, _)| std::cmp::Reverse(s.started_ts));
    Ok(out)
}
