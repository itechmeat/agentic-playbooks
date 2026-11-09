//! Decisions outside a run (issue #165 Part 16): the MCP catalog ranking,
//! host tasks, the model grader and the known-problem recheck ask through
//! the same configuration, provider chain and redaction as a run, but have
//! no run journal. Each decision is logged as one JSON line in
//! `<root>/.apb/decisions.jsonl` (git-ignored, `workspace::LOCAL_ENTRIES`)
//! with the `decision_made` fields minus node and attempt, and a use's
//! `max_requests_per_day` caps the requests per project and UTC day.
//!
//! Every step fails open: the caller gets an outcome and keeps today's
//! behaviour on anything but `Answered`. Nothing here is ever applied by the
//! engine on its own: each caller applies its own mode rule.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use apb_core::decisions::{DecisionMode, EffectiveDecisions, SendClass};
use apb_decide::{Answer, DecisionRequest, ProviderChain, Question, UseSite};
use serde_json::{Value, json};

use super::{compact_answer, list_price_per_million, providers};
use crate::redact;

/// The log file under `<root>/.apb/`.
pub const DECISIONS_LOG: &str = "decisions.jsonl";

/// The `use` of a judge sandbox line (`judge_eval::sandbox`): no cap here
/// counts such a line.
pub(crate) const JUDGE_SANDBOX_USE: &str = "judge_sandbox";

/// Why a read-only process skipped a decision.
pub const READ_ONLY_SKIP: &str = "read_only";

/// The tail of the log read to count today's requests.
const MAX_LOG_SCAN_BYTES: u64 = 8 * 1024 * 1024;

const DAY_MS: u64 = 86_400_000;
/// The lock serializing the log's count, request and line.
const LOG_LOCK: &str = "decisions.jsonl.lock";
/// Added to the provider chain's worst case in [`StandaloneDecider::lock_lifetime`].
const LOCK_MARGIN_MS: u64 = 10_000;

/// An answered standalone decision.
#[derive(Debug, Clone, PartialEq)]
pub struct StandaloneAnswer {
    pub provider: String,
    pub model: String,
    pub calibrated: bool,
    /// The effective mode the decision was asked in.
    pub mode: DecisionMode,
    pub answers: BTreeMap<String, Answer>,
    pub latency_ms: u64,
}

/// What a standalone decision came to. The caller keeps today's behaviour
/// on anything but `Answered`.
#[derive(Debug, Clone, PartialEq)]
pub enum StandaloneOutcome {
    Answered(StandaloneAnswer),
    /// Nothing asked: the use is off, prompts may not be sent, the state is
    /// over the byte budget, or the daily cap is reached.
    Skipped {
        reason: &'static str,
    },
    /// Asked and failed; logged with this error kind.
    Failed {
        error_kind: String,
    },
}

/// Asks decisions for one project outside any run. `Debug` never prints a
/// key or a secret value.
pub struct StandaloneDecider {
    settings: EffectiveDecisions,
    /// The project whose content the requests carry: the redactor's root.
    root: PathBuf,
    /// Where the log and the daily cap live (`<log_root>/.apb/`).
    log_root: PathBuf,
    chain: OnceLock<(ProviderChain, Vec<apb_core::secret::Secret>)>,
    redactor: OnceLock<redact::Redactor>,
    /// The declared secret values of the run a request speaks about
    /// ([`StandaloneDecider::for_run`]), removed from every request whatever
    /// `privacy.redact` says. Empty for a request about no run.
    run_values: crate::run_redact::RunValues,
    /// The run a request speaks about processed external data
    /// ([`crate::external_data`]): its log lines say so, kept for the daily
    /// cap only.
    external_data: bool,
}

impl std::fmt::Debug for StandaloneDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandaloneDecider")
            .field("root", &self.root)
            .field("providers", &self.chain.get().map(|c| &c.0))
            .finish_non_exhaustive()
    }
}

impl StandaloneDecider {
    /// The decider for the project at `root`, or `None` when the layer is
    /// off for it (no file, an invalid file, the kill switch, the project
    /// opted out, every use off).
    pub fn for_project(root: &Path) -> Option<Self> {
        apb_core::decisions::resolve(root)
            .active()
            .map(|s| Self::with_settings(s, root))
    }

    /// The decider for a request made from `caller`'s project about
    /// `target`'s content: both projects' `decisions:` sections narrow it
    /// (the stricter of each wins, either opting out turns it off). Requests
    /// are journaled in `target`, whose content they carry.
    pub fn for_projects(caller: &Path, target: &Path) -> Option<Self> {
        apb_core::decisions::resolve_for_all(&[caller, target])
            .active()
            .map(|s| Self::with_settings(s, target))
    }

    /// A decider over explicit settings (already resolved and capped).
    pub fn with_settings(settings: EffectiveDecisions, root: &Path) -> Self {
        Self::with_settings_logged_in(settings, root, root)
    }

    /// [`StandaloneDecider::with_settings`] for the content of the project
    /// at `root`, logged and capped under `log_root` instead (a caller that
    /// must not create a `.apb` folder in the project).
    pub fn with_settings_logged_in(
        settings: EffectiveDecisions,
        root: &Path,
        log_root: &Path,
    ) -> Self {
        StandaloneDecider {
            settings,
            root: root.to_path_buf(),
            log_root: log_root.to_path_buf(),
            chain: OnceLock::new(),
            redactor: OnceLock::new(),
            run_values: crate::run_redact::RunValues::default(),
            external_data: false,
        }
    }

    /// This decider for requests about the run at `run_dir`: every request
    /// loses the run's secret values (`crate::run_redact`), before any
    /// clip, whatever `privacy.redact` says.
    pub fn for_run(mut self, run_dir: &Path) -> Self {
        self.run_values = crate::run_redact::RunValues::of(run_dir, &self.root);
        self.external_data = crate::external_data::is_flagged(run_dir);
        self
    }

    /// The use's mode now: the settings, unless the kill switch is set.
    pub fn mode_for(&self, site: UseSite) -> DecisionMode {
        if apb_core::decisions::killed_by_switch() {
            DecisionMode::Off
        } else {
            self.settings.mode_for(site.as_str())
        }
    }

    /// A use's threshold by name.
    pub fn threshold(&self, site: UseSite, name: &str) -> Option<f64> {
        self.settings.threshold(site.as_str(), name)
    }

    /// Whether an `enforce` use may act on an uncalibrated answer.
    pub fn allows_uncalibrated(&self, site: UseSite) -> bool {
        self.settings.allows_uncalibrated(site.as_str())
    }

    /// Whether any configured provider resolved (its key and account id):
    /// without one nothing can be asked, and a caller keeps today's
    /// behaviour without saying so.
    pub fn available(&self) -> bool {
        !self.chain().0.is_empty()
    }

    /// Whether `privacy.send` allows material of `class`.
    pub fn sends(&self, class: SendClass) -> bool {
        self.settings.sends(class)
    }

    /// The byte budget of one request's state.
    pub fn max_state_bytes(&self) -> usize {
        self.settings.privacy.max_state_bytes
    }

    fn chain(&self) -> &(ProviderChain, Vec<apb_core::secret::Secret>) {
        self.chain
            .get_or_init(|| providers::build_chain(&self.settings))
    }

    /// The same secrets a run of this project redacts: the provider keys
    /// and every variable an installed connector references.
    /// The run's secret values are applied on their own
    /// ([`StandaloneDecider::for_run`]), so the order of `for_run` and a
    /// first request never matters.
    fn redactor(&self) -> &redact::Redactor {
        self.redactor
            .get_or_init(|| redact::Redactor::for_workspace(&self.root, self.chain().1.clone()))
    }

    /// `v` as a request carries it: [`StandaloneDecider::redact`] over its
    /// every string.
    fn redact_json(&self, mut v: Value) -> Value {
        self.run_values.scrub_values_json(&mut v);
        if self.settings.privacy.redact {
            self.redactor().redact_value(v)
        } else {
            v
        }
    }

    /// `text` as a request would carry it: redacted when `privacy.redact` is
    /// on, and without the run's secret values either way. A caller that
    /// clips redacts first, so a cut never splits a
    /// secret into a shape the redactor no longer knows.
    pub fn redact(&self, text: &str) -> String {
        let text = self.run_values.scrub_values(text);
        if self.settings.privacy.redact {
            self.redactor().redact(&text)
        } else {
            text
        }
    }

    fn log(&self) -> DecisionLog {
        DecisionLog::of(&self.log_root)
    }

    fn daily_cap(&self, site: UseSite) -> u32 {
        self.settings
            .uses
            .get(site.as_str())
            .and_then(|u| u.max_requests_per_day)
            .or_else(|| apb_core::decisions::default_daily_cap(site.as_str()))
            .unwrap_or(0)
    }

    /// Requests sent today (UTC) for `site`, per the log. A judge sandbox
    /// line never counts here, whatever its `use_site`: the sandbox has its
    /// own cap.
    fn requests_today(&self, site: UseSite, now_ms: u64) -> u32 {
        self.log().count_today(now_ms, |v| {
            v["use_site"] == site.as_str() && v["use"] != JUDGE_SANDBOX_USE
        })
    }

    /// How long the log lock may be held before another caller treats it as
    /// abandoned ([`lock_lifetime`] of one request).
    fn lock_lifetime(&self) -> std::time::Duration {
        lock_lifetime(&self.settings, 1)
    }

    /// Asks one decision over `state` (material class [`apb_core::decisions::send_class`]
    /// of the use, `prompts` but for the save check): redacted
    /// when `privacy.redact` is on, without the run's secret values either
    /// way ([`StandaloneDecider::for_run`]), refused when over `max_state_bytes`
    /// (the caller clips), logged, capped per day.
    pub fn decide(
        &self,
        site: UseSite,
        state: Value,
        state_order: Vec<String>,
        questions: BTreeMap<String, Question>,
    ) -> StandaloneOutcome {
        // A read-only process sends no request and logs nothing.
        if crate::read_only::active() {
            return StandaloneOutcome::Skipped {
                reason: READ_ONLY_SKIP,
            };
        }
        let mode = self.mode_for(site);
        if mode == DecisionMode::Off {
            return StandaloneOutcome::Skipped { reason: "off" };
        }
        if !self
            .settings
            .sends(apb_core::decisions::send_class(site.as_str()))
        {
            return StandaloneOutcome::Skipped { reason: "send" };
        }
        // The questions carry text too (a suggestion's synopsis).
        // Questions that do not survive the redaction are not sent at all:
        // the original may hold a secret value.
        let Some(questions) = serde_json::to_value(&questions)
            .ok()
            .map(|v| self.redact_json(v))
            .and_then(|v| serde_json::from_value(v).ok())
        else {
            return StandaloneOutcome::Skipped { reason: "redact" };
        };
        let state = self.redact_json(state);
        let state_bytes = serde_json::to_string(&state).map_or(0, |s| s.len());
        if state_bytes > self.settings.privacy.max_state_bytes {
            return StandaloneOutcome::Skipped {
                reason: "state_too_large",
            };
        }
        let now_ms = apb_core::clock::now_ms_u64();
        let questions_digest = apb_decide::digest::questions_digest(&questions);
        let state_digest = apb_decide::digest::digest(&state);
        let mut line = json!({
            "ts_ms": now_ms,
            "use_site": site.as_str(),
            "provider": null,
            "model": null,
            "calibrated": false,
            "mode": mode.as_str(),
            "questions_digest": questions_digest,
            "state_digest": state_digest,
            "state_bytes": state_bytes,
            "answers": {},
            "applied": false,
            "latency_ms": 0,
            "input_tokens": null,
            "cost_usd": null,
            "cost_estimated": false,
            "cached": false,
            "error": null,
        });
        if self.external_data {
            line["external_data"] = json!(true);
        }
        // The count, the request and its log line under the log's lock: MCP
        // servers of several agents share the log, and a count read before
        // another's reply is logged would let each of them past the cap. A
        // lock still held after its wait (a slow request elsewhere) skips
        // this one: fail-open, the caller keeps its plain answer. The holder
        // may legitimately hold it for a whole provider chain, so the lock
        // counts as stale only past that bound ([`Self::lock_lifetime`]); a
        // stale one (a holder that died) is broken and the break is logged.
        let lifetime = self.lock_lifetime();
        let Some((_lock, broke_stale)) = self.log().lock(lifetime) else {
            return StandaloneOutcome::Skipped { reason: "busy" };
        };
        if broke_stale {
            self.log().note_stale_lock(now_ms, lifetime);
        }
        if self.requests_today(site, now_ms) >= self.daily_cap(site) {
            line["error"] = json!("budget");
            self.log().append(&line);
            return StandaloneOutcome::Skipped { reason: "budget" };
        }
        let request = DecisionRequest {
            use_site: site,
            state,
            state_order,
            questions,
        };
        let started = std::time::Instant::now();
        let result = self.chain().0.decide(&request, None);
        match result {
            Ok(resp) => {
                let compact: BTreeMap<String, _> = resp
                    .answers
                    .iter()
                    .map(|(k, a)| (k.clone(), compact_answer(a)))
                    .collect();
                let (cost, estimated) = match resp.usage.cost_usd {
                    Some(c) => (Some(c), false),
                    None => match (list_price_per_million(&resp.model), resp.usage.input_tokens) {
                        (Some(price), Some(t)) => (Some(price * t as f64 / 1_000_000.0), true),
                        _ => (None, false),
                    },
                };
                line["provider"] = json!(resp.provider);
                line["model"] = json!(resp.model);
                line["calibrated"] = json!(resp.calibrated);
                line["answers"] = json!(compact);
                line["latency_ms"] = json!(resp.latency_ms.max(1));
                line["input_tokens"] = json!(resp.usage.input_tokens);
                line["cost_usd"] = json!(cost);
                line["cost_estimated"] = json!(estimated);
                self.log().append(&line);
                StandaloneOutcome::Answered(StandaloneAnswer {
                    provider: resp.provider,
                    model: resp.model,
                    calibrated: resp.calibrated,
                    mode,
                    answers: resp.answers,
                    latency_ms: resp.latency_ms,
                })
            }
            Err(e) => {
                line["provider"] = json!(self.chain().0.ids().last());
                line["latency_ms"] = json!(started.elapsed().as_millis() as u64);
                line["error"] = json!(e.kind());
                self.log().append(&line);
                StandaloneOutcome::Failed {
                    error_kind: e.kind().to_string(),
                }
            }
        }
    }
}

/// How long the log lock may be held for `requests` requests before another
/// caller treats it as abandoned: every provider in the chain taking its
/// whole `timeout_ms` for each, plus a margin for the count and the log
/// write.
pub(crate) fn lock_lifetime(settings: &EffectiveDecisions, requests: u64) -> std::time::Duration {
    let providers = settings.providers.len().max(1) as u64;
    std::time::Duration::from_millis(
        settings
            .timeout_ms
            .saturating_mul(providers)
            .saturating_mul(requests.max(1))
            .saturating_add(LOCK_MARGIN_MS),
    )
}

/// The project's log of decisions outside a run, `<root>/.apb/decisions.jsonl`:
/// one JSON line per decision, counted per UTC day under its lock. Shared by
/// the standalone decider and the judge sandbox.
pub(crate) struct DecisionLog {
    path: PathBuf,
}

impl DecisionLog {
    pub(crate) fn of(root: &Path) -> Self {
        DecisionLog {
            path: root.join(".apb").join(DECISIONS_LOG),
        }
    }

    /// Opens the log without following a link, and only as a regular file:
    /// the log sits in the project tree, which a repository can fill with a
    /// symlink, a FIFO or a directory. `Ok(None)`: no log yet. `Err`: a log
    /// that is not a plain file.
    pub(crate) fn open(&self, append: bool) -> Result<Option<std::fs::File>, ()> {
        let path = &self.path;
        match std::fs::symlink_metadata(path) {
            Ok(m) if !m.file_type().is_file() => return Err(()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && append => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(()),
        }
        let mut options = std::fs::OpenOptions::new();
        if append {
            options.create(true).append(true);
        } else {
            options.read(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|_| ())?;
        match file.metadata() {
            Ok(m) if m.is_file() => Ok(Some(file)),
            _ => Err(()),
        }
    }

    /// Requests sent today (UTC) on the lines `keep` selects: a provider was
    /// asked and the answer was not cached. A log that is not a plain file
    /// counts as a spent cap (fail closed for spend); only the last
    /// [`MAX_LOG_SCAN_BYTES`] are read.
    pub(crate) fn count_today(&self, now_ms: u64, keep: impl Fn(&Value) -> bool) -> u32 {
        self.weigh_today(now_ms, |v| {
            u32::from(keep(v) && !v["provider"].is_null() && v["cached"] != true)
        })
    }

    /// The sum of `weigh` over today's (UTC) lines, saturating. A log that
    /// is not a plain file weighs a spent cap; only the last
    /// [`MAX_LOG_SCAN_BYTES`] are read.
    pub(crate) fn weigh_today(&self, now_ms: u64, weigh: impl Fn(&Value) -> u32) -> u32 {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = match self.open(false) {
            Ok(Some(f)) => f,
            Ok(None) => return 0,
            Err(()) => return u32::MAX,
        };
        let len = file.metadata().map_or(0, |m| m.len());
        let skip_partial = len > MAX_LOG_SCAN_BYTES;
        if skip_partial
            && file
                .seek(SeekFrom::Start(len - MAX_LOG_SCAN_BYTES))
                .is_err()
        {
            return u32::MAX;
        }
        let today = now_ms / DAY_MS;
        std::io::BufReader::new(file.take(MAX_LOG_SCAN_BYTES))
            .lines()
            .skip(usize::from(skip_partial))
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .filter(|v| v["ts_ms"].as_u64().is_some_and(|t| t / DAY_MS == today))
            .fold(0u32, |sum, v| sum.saturating_add(weigh(&v)))
    }

    /// The log's lock, held `lifetime` at most before another caller breaks
    /// it, and whether a stale one was broken. `None`: held elsewhere.
    pub(crate) fn lock(
        &self,
        lifetime: std::time::Duration,
    ) -> Option<(apb_core::fsutil::DirLock, bool)> {
        self.path
            .parent()
            .and_then(|dir| apb_core::fsutil::lock_dir_with_lifetime(dir, LOG_LOCK, lifetime).ok())
    }

    /// Logs that a stale lock was broken.
    pub(crate) fn note_stale_lock(&self, now_ms: u64, lifetime: std::time::Duration) {
        self.append(&json!({
            "ts_ms": now_ms,
            "note": "stale_lock_broken",
            "lock": LOG_LOCK,
            "stale_after_ms": lifetime.as_millis() as u64,
        }));
    }

    /// Appends one line, best effort: a log that cannot be written never
    /// changes an answer. Nothing in a read-only process, nor without the
    /// project's `.apb` directory.
    pub(crate) fn append(&self, line: &Value) {
        let _ = self.try_append(line);
    }

    /// [`DecisionLog::append`], saying whether the line was written.
    pub(crate) fn try_append(&self, line: &Value) -> Result<(), ()> {
        if crate::read_only::active() {
            return Err(());
        }
        if !self.path.parent().is_some_and(Path::is_dir) {
            return Err(());
        }
        let mut text = serde_json::to_string(line).map_err(|_| ())?;
        text.push('\n');
        match self.open(true)? {
            Some(mut f) => f.write_all(text.as_bytes()).map_err(|_| ()),
            None => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use apb_decide::{DecideError, DecisionProvider, DecisionResponse, Limits, Usage};

    use super::*;

    /// A provider that keeps every request and takes `delay` to answer.
    #[derive(Debug, Default)]
    struct Slow {
        seen: Mutex<Vec<DecisionRequest>>,
        delay_ms: u64,
    }

    #[derive(Debug)]
    struct Handle(Arc<Slow>);

    impl DecisionProvider for Handle {
        fn id(&self) -> &str {
            "slow"
        }
        fn model(&self) -> &str {
            "m"
        }
        fn limits(&self) -> Limits {
            Limits::default()
        }
        fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
            self.0.seen.lock().unwrap().push(req.clone());
            std::thread::sleep(std::time::Duration::from_millis(self.0.delay_ms));
            Ok(DecisionResponse {
                provider: "slow".into(),
                model: "m".into(),
                calibrated: false,
                answers: BTreeMap::new(),
                usage: Usage::default(),
                latency_ms: 1,
                cached: false,
                ignored_items: 0,
            })
        }
    }

    fn decider(root: &Path, cap: u32, slow: &Arc<Slow>) -> StandaloneDecider {
        let cfg = tempfile::tempdir().unwrap();
        std::fs::write(
            cfg.path().join(apb_core::decisions::DECISIONS_FILE),
            format!("mode: advise\nproviders: [{{ id: slow, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }}]\nuses:\n  catalog_rank: {{ mode: advise, max_requests_per_day: {cap} }}\n"),
        )
        .unwrap();
        let settings = apb_core::decisions::load_file(cfg.path()).unwrap().unwrap();
        let d = StandaloneDecider::with_settings(settings, root);
        let _ = d.chain.set((
            ProviderChain::new(vec![Box::new(Handle(slow.clone()))]),
            vec![apb_core::secret::Secret::new("sk-live-provider-key-123456")],
        ));
        d
    }

    fn questions(text: &str) -> BTreeMap<String, Question> {
        BTreeMap::from([(
            "covered_0".to_string(),
            Question::Noul {
                instructions: json!({ "question": "same?", "synopsis": text }),
                criteria: None,
            },
        )])
    }

    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        root
    }

    #[test]
    fn question_text_is_redacted_and_debug_hides_keys() {
        let root = project();
        let slow = Arc::new(Slow::default());
        let d = decider(root.path(), 10, &slow);
        let _ = d.decide(
            UseSite::CatalogRank,
            json!({"task": "x"}),
            vec!["task".into()],
            questions("deploy /home/alice/app and mail bob@corp.example"),
        );
        let sent = serde_json::to_string(&slow.seen.lock().unwrap()[0].questions).unwrap();
        assert!(
            !sent.contains("alice") && !sent.contains("bob@corp"),
            "{sent}"
        );
        assert!(!format!("{d:?}").contains("sk-live"), "{d:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_log_link_is_never_followed_and_spends_nothing() {
        // The log sits in the project tree: a repository can commit it as a
        // link (to /dev/null to reset the cap, to a file to corrupt it) or
        // as a directory.
        let root = project();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("victim");
        std::fs::write(&target, "keep\n").unwrap();
        let log = root.path().join(".apb").join(DECISIONS_LOG);
        for plant in ["link", "dir"] {
            let _ = std::fs::remove_file(&log);
            let _ = std::fs::remove_dir(&log);
            match plant {
                "link" => std::os::unix::fs::symlink(&target, &log).unwrap(),
                _ => std::fs::create_dir(&log).unwrap(),
            }
            let slow = Arc::new(Slow::default());
            let d = decider(root.path(), 5, &slow);
            let out = d.decide(
                UseSite::CatalogRank,
                json!({"task": "x"}),
                vec!["task".into()],
                questions("s"),
            );
            assert!(
                matches!(out, StandaloneOutcome::Skipped { reason: "budget" }),
                "{plant}: {out:?}"
            );
            assert!(slow.seen.lock().unwrap().is_empty(), "{plant}");
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
    }

    /// Plants a held log lock whose mtime is `age` in the past.
    fn plant_lock(root: &Path, age: std::time::Duration) {
        let path = root.join(".apb").join(LOG_LOCK);
        let f = std::fs::File::create(&path).unwrap();
        f.set_modified(std::time::SystemTime::now() - age).unwrap();
    }

    fn log_notes(root: &Path) -> Vec<Value> {
        std::fs::read_to_string(root.join(".apb").join(DECISIONS_LOG))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("note").is_some())
            .collect()
    }

    /// The lock's lifetime follows the configured provider timeout and the
    /// chain length, not a fixed minute: a lock younger than a whole chain's
    /// worst case belongs to a live request and is never broken (busy), one
    /// older than it is broken with a logged note and the call proceeds.
    #[test]
    fn the_log_lock_lives_as_long_as_the_provider_chain_can() {
        let root = project();
        let slow = Arc::new(Slow::default());
        let mut d = decider(root.path(), 10, &slow);
        let ask = |d: &StandaloneDecider| {
            d.decide(
                UseSite::CatalogRank,
                json!({"task": "x"}),
                vec!["task".into()],
                questions("s"),
            )
        };

        // Two providers at 60 s each: a 90 s old lock may still be a live
        // chain (a fixed 60 s staleness would have broken it).
        d.settings.timeout_ms = 60_000;
        d.settings.providers.push(d.settings.providers[0].clone());
        assert_eq!(d.lock_lifetime().as_millis(), 130_000);
        plant_lock(root.path(), std::time::Duration::from_secs(90));
        assert!(
            matches!(ask(&d), StandaloneOutcome::Skipped { reason: "busy" }),
            "a lock within the chain's lifetime was broken"
        );
        assert!(slow.seen.lock().unwrap().is_empty());
        assert!(log_notes(root.path()).is_empty());

        // One provider at 100 ms: the lock lives 10.1 s, so a 30 s old lock
        // is abandoned. It is broken, the break is logged, and the call runs.
        d.settings.timeout_ms = 100;
        d.settings.providers.truncate(1);
        plant_lock(root.path(), std::time::Duration::from_secs(30));
        assert!(
            matches!(ask(&d), StandaloneOutcome::Answered(_)),
            "a stale lock kept the call busy"
        );
        assert_eq!(slow.seen.lock().unwrap().len(), 1);
        let notes = log_notes(root.path());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0]["note"], "stale_lock_broken");
        assert_eq!(notes[0]["stale_after_ms"], 10_100);
        assert!(
            !root.path().join(".apb").join(LOG_LOCK).exists(),
            "the broken lock was released"
        );
    }

    /// A day of judge sandbox lines leaves every other use's daily cap
    /// whole: the sandbox counts its own lines, nothing else does.
    #[test]
    fn judge_sandbox_lines_never_count_against_another_cap() {
        let root = project();
        let now = apb_core::clock::now_ms_u64();
        let log = DecisionLog::of(root.path());
        // A whole day of the sandbox (its cap is 100).
        for _ in 0..100 {
            log.append(&json!({
                "ts_ms": now,
                "use_site": "judge_node",
                "use": JUDGE_SANDBOX_USE,
                "provider": "fake",
                "cached": false,
            }));
        }
        let slow = Arc::new(Slow::default());
        let d = decider(root.path(), 1, &slow);
        assert_eq!(d.requests_today(UseSite::CatalogRank, now), 0);
        assert!(matches!(
            d.decide(
                UseSite::CatalogRank,
                json!({"task": "x"}),
                vec!["task".into()],
                questions("s"),
            ),
            StandaloneOutcome::Answered(_)
        ));
    }

    #[test]
    fn concurrent_calls_never_pass_the_daily_cap() {
        let root = project();
        let slow = Arc::new(Slow {
            delay_ms: 300,
            ..Default::default()
        });
        let (a, b) = (
            decider(root.path(), 1, &slow),
            decider(root.path(), 1, &slow),
        );
        let ask = |d: &StandaloneDecider| {
            d.decide(
                UseSite::CatalogRank,
                json!({"task": "x"}),
                vec!["task".into()],
                questions("s"),
            )
        };
        let outcomes = std::thread::scope(|s| {
            let x = s.spawn(|| ask(&a));
            let y = s.spawn(|| ask(&b));
            [x.join().unwrap(), y.join().unwrap()]
        });
        assert_eq!(slow.seen.lock().unwrap().len(), 1, "{outcomes:?}");
        // A third call the same day is refused too: the reservation counts.
        assert!(matches!(
            ask(&a),
            StandaloneOutcome::Skipped { reason: "budget" }
        ));
    }

    /// A request about a run that processed external data logs a line
    /// that says so (for the daily cap only); one about an unflagged run
    /// logs none. The decisions report never reads the log.
    #[test]
    fn a_request_about_a_flagged_run_says_so_in_its_log_line() {
        let root = project();
        let runs = root.path().join(".apb/runs");
        for (id, flagged) in [("plain", false), ("flagged", true)] {
            let dir = runs.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            // A prepared run writes its config before its manifest.
            std::fs::write(dir.join("run.yaml"), "params: {}\n").unwrap();
            crate::manifest::write(
                &dir,
                &crate::manifest::RunExecutionManifest {
                    external_data: flagged,
                    ..Default::default()
                },
            )
            .unwrap();
            let slow = Arc::new(Slow::default());
            let d = decider(root.path(), 10, &slow).for_run(&dir);
            assert!(matches!(
                d.decide(
                    UseSite::CatalogRank,
                    json!({"task": "x"}),
                    vec!["task".into()],
                    questions("s"),
                ),
                StandaloneOutcome::Answered(_)
            ));
        }
        let lines: Vec<Value> =
            std::fs::read_to_string(root.path().join(".apb").join(DECISIONS_LOG))
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].get("external_data").is_none(), "{lines:?}");
        assert_eq!(lines[1]["external_data"], true, "{lines:?}");
        let report = crate::decision::report::report(
            &[root.path().to_path_buf()],
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(report.decisions, 0);
    }
}
