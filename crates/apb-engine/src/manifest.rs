//! RunExecutionManifest (spec 2026-07-12, section 3.6): the immutable
//! snapshot of a run's profiles, skills, and invocations.
//!
//! Written once at start. All profile/SOUL/skill reads after start (retry,
//! fallback, resume, server restart) come from the run snapshot and this
//! manifest, not from live directories - editing a profile/skill after start
//! does not affect the run. The binary fingerprint in the chain lets resume
//! catch a swapped executable (environment drift).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use apb_core::profile::SoulRequirement;
use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::invocation::ResolvedInvocation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestSkill {
    pub name: String,
    pub scope: String,
    pub digest: String,
}

/// A profile recorded in the manifest: identity + digests + role content +
/// executor chain (already filtered by SOUL requirement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestProfile {
    pub scope: String,
    pub name: String,
    /// The digest of the profile's `profile.yaml` and SOUL: the profile
    /// content the trust gate and the run permit compare. For an
    /// `ephemeral` entry it is the base profile's, whose SOUL and skills the
    /// override keeps, so it does not describe the executor that ran; that
    /// is [`ManifestProfile::executor_digest`].
    pub profile_digest: String,
    pub bundle_digest: String,
    pub soul: String,
    pub soul_requirement: SoulRequirement,
    pub skills: Vec<ManifestSkill>,
    pub chain: Vec<ResolvedInvocation>,
    /// Run-local ephemeral executor override (completion-plan Task 4): the
    /// chain is replaced by a single ad-hoc invocation (agent+model), while
    /// SOUL and skills are taken from the node's profile. Such an entry is
    /// per-node (not deduplicated by `<scope>/<name>`) and is excluded from
    /// bundle trust (the executor is ad-hoc, not part of the profile).
    #[serde(default)]
    pub ephemeral: bool,
    /// Whether the profile runs with the minimal agent environment (its
    /// `environment`, issue #136 item 4; historically the `hermetic` flag),
    /// snapshotted so post-start reads (retry, fallback, resume) use the run's
    /// value, not the live profile. Old manifests written before this field
    /// parse as `false` (serde default), so a run started with the old full
    /// environment keeps it on resume.
    #[serde(default)]
    pub hermetic: bool,
    /// The profile's `zcode_mode`, snapshotted like `hermetic`: the mode its
    /// zcode steps get when the run grants autonomy. Absent (and in old
    /// manifests) means `yolo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zcode_mode: Option<apb_core::profile::ZcodeMode>,
    /// The profile's `process_env`, snapshotted like `hermetic`: which
    /// operator variables its agent processes inherit for the whole run,
    /// resume included. Absent (and in old manifests) means `inherit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_env: Option<apb_core::process_env::ProcessEnvPolicy>,
    // --- tier routing (issue #165 Part 12) ---
    /// The profile's executor tiers, lightest first, resolved at run start
    /// like the chain. Empty for a profile without tiers (and old
    /// manifests), which is never routed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<ManifestTier>,
    /// Set only on a rebind-overlay entry written by tier routing: the tier
    /// the node was routed to. A supervisor's own rebind never has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_tier: Option<String>,
    /// How many leading chain steps of a routed entry are tiers below the
    /// profile's own executor: an agent failure there goes up a tier at
    /// once instead of spending same-executor retries.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cascade: u32,
    // --- end tier routing ---
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A profile tier as a run snapshots it (issue #165 Part 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTier {
    pub name: String,
    /// The work the tier is for (the routing question's criterion).
    #[serde(rename = "for")]
    pub for_work: String,
    /// The tier's own executor; `None` for `use: executor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<ResolvedInvocation>,
}

impl ManifestProfile {
    /// The `<scope>/<name>` key - the profile's identity across all surfaces (spec 3.3).
    pub fn key(&self) -> String {
        format!("{}/{}", self.scope, self.name)
    }

    /// What ran, as opposed to the profile content: `x1:` plus the sha256
    /// over the chain in order, each invocation as `(agent_id, model, spec,
    /// soul_delivery)`; the tiers in manifest order, each as `(name, for,
    /// invocation or the marker "executor")`; the `ephemeral` flag. Machine
    /// fields (`canonical_executable`, `executable_fingerprint`) are left
    /// out. A pure function of the entry, so old manifests get it too.
    pub fn executor_digest(&self) -> String {
        executor_digest_of(&self.chain, &self.tiers, self.ephemeral)
    }
}

/// The algorithm prefix of the executor digest: a later change of the
/// canonical form starts new groups instead of mixing.
pub const EXECUTOR_DIGEST_PREFIX: &str = "x1:";

/// The executor digest over the parts of an entry: the one algorithm of
/// `ManifestProfile::executor_digest` and of a pre-start key.
pub fn executor_digest_of(
    chain: &[ResolvedInvocation],
    tiers: &[ManifestTier],
    ephemeral: bool,
) -> String {
    let invocation = |ri: &ResolvedInvocation| {
        serde_json::json!([ri.agent_id, ri.model, ri.spec, ri.soul_delivery])
    };
    let chain: Vec<serde_json::Value> = chain.iter().map(invocation).collect();
    let tiers: Vec<serde_json::Value> = tiers
        .iter()
        .map(|t| {
            serde_json::json!([
                t.name,
                t.for_work,
                t.invocation
                    .as_ref()
                    .map_or_else(|| serde_json::json!("executor"), invocation)
            ])
        })
        .collect();
    let form = serde_json::json!([chain, tiers, ephemeral]);
    // A `serde_json::Value` serializes its object keys sorted, so the bytes
    // are canonical; the form above holds arrays and the spec's own objects.
    let bytes = serde_json::to_vec(&form).unwrap_or_default();
    format!(
        "{EXECUTOR_DIGEST_PREFIX}{}",
        apb_core::content::sha256_hex(&bytes)
    )
}

/// The executor of a profile as a run start binds it: the chain (the
/// ephemeral override's one invocation in place of the profile's executor
/// and fallbacks), filtered by the SOUL requirement, and the tiers. The one
/// resolver of the run start, a rebind and
/// `crate::execution_identity::digest::expected_exec_key`, so a pre-start
/// key and a started run's manifest cannot diverge. A profile with a
/// problem refuses, as the start does.
pub fn resolve_executor(
    loaded: &apb_core::profile_store::LoadedProfile,
    eph: Option<&apb_core::overrides::EphemeralExecutor>,
    global: &apb_core::config::GlobalConfig,
) -> Result<(Vec<ResolvedInvocation>, Vec<ManifestTier>), EngineError> {
    let mut chain = Vec::new();
    if let Some(e) = eph {
        let program = crate::invocation::program_for(&e.agent, global);
        chain.push(crate::invocation::resolve_invocation(
            &e.agent, &e.model, &program, global,
        )?);
    } else {
        let ex = &loaded.doc.executor;
        let program = crate::invocation::program_for(&ex.agent, global);
        chain.push(crate::invocation::resolve_invocation(
            &ex.agent, &ex.model, &program, global,
        )?);
        for f in &ex.fallbacks {
            let p = crate::invocation::program_for(&f.agent, global);
            chain.push(crate::invocation::resolve_invocation(
                &f.agent, &f.model, &p, global,
            )?);
        }
    }
    let soul_empty = loaded.soul.trim().is_empty();
    let chain = crate::invocation::filter_chain(chain, loaded.doc.soul, soul_empty)?;
    // Executor tiers (issue #165 Part 12), resolved like the chain so an
    // unknown agent fails the run start the same way. An ephemeral executor
    // replaces the chain, so its entry is never routed and only the
    // profile's other problems (its `process_env`, which applies to an
    // ephemeral executor too) refuse the start.
    let mut tiers = Vec::new();
    let problems: Vec<String> = loaded
        .doc
        .problems()
        .iter()
        .filter(|p| eph.is_none() || p.code != apb_core::profile::PROFILE_TIERS_INVALID)
        .map(ToString::to_string)
        .collect();
    if !problems.is_empty() {
        return Err(EngineError::Invalid(format!(
            "profile `{}/{}`: {}",
            apb_core::profile_store::scope_str(loaded.scope),
            loaded.name,
            problems.join("; ")
        )));
    }
    if eph.is_none() {
        for (name, t) in loaded.doc.tiers.iter() {
            let invocation = match t.executor() {
                Some((agent, model)) => {
                    let program = crate::invocation::program_for(agent, global);
                    let ri = crate::invocation::resolve_invocation(agent, model, &program, global)?;
                    // A tier whose agent cannot carry a required native
                    // SOUL is left out, as `filter_chain` drops such a step.
                    if loaded.doc.soul == apb_core::profile::SoulRequirement::NativeRequired
                        && !soul_empty
                        && ri.soul_delivery != apb_core::config::SoulDelivery::Native
                    {
                        continue;
                    }
                    Some(ri)
                }
                None => None,
            };
            tiers.push(ManifestTier {
                name: name.clone(),
                for_work: t.for_work.clone(),
                invocation,
            });
        }
    }
    Ok((chain, tiers))
}

/// A connector account recorded in the manifest: identity + non-secret field
/// values + the env-var names holding secret field values + its content
/// digest. Secret fields never carry the raw secret value into the manifest -
/// `env` maps the field name to the ENV VAR NAME that holds it at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestAccount {
    pub name: String,
    pub default: bool,
    /// Non-secret field values.
    pub fields: BTreeMap<String, String>,
    /// Secret field name -> ENV VAR NAME (not the secret value itself).
    pub env: BTreeMap<String, String>,
    /// Secret field name -> the shell command line that produces the secret
    /// at call time (spec 4.1), never the secret value itself. Empty for an
    /// env-sourced or non-secret account; disjoint from `env` (a secret field
    /// is exactly one of the two forms).
    #[serde(default)]
    pub cmd: BTreeMap<String, String>,
    /// `account_digest`.
    pub digest: String,
}

/// A connector recorded in the manifest: identity + digest + the accounts
/// snapshotted for this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestConnector {
    pub name: String,
    pub digest: String,
    pub accounts: Vec<ManifestAccount>,
}

/// A single node's grant against one connector: which accounts, which
/// functions, an optional call budget counted per executor attempt
/// (`connector::call::attempt`), and what a repeat of a send with an unknown
/// outcome does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestConnectorGrant {
    pub connector: String,
    /// Account names granted to this node.
    pub accounts: Vec<String>,
    pub functions: Vec<String>,
    pub max_calls: Option<u32>,
    /// `None` (and every older manifest) reads as `refuse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_unknown: Option<apb_core::schema::OnUnknown>,
}

/// A node's digests in a run: its profile (`profile.yaml` plus SOUL), its
/// bundle (plus skills) and its executor (the chain, the tiers and the
/// `ephemeral` flag, [`ManifestProfile::executor_digest`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeDigests {
    pub profile: String,
    pub bundle: String,
    /// Recomputed from the manifest entry, so an old manifest has it too.
    #[serde(default)]
    pub executor: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunExecutionManifest {
    /// Profiles used, one per unique `(scope, name)`.
    pub profiles: Vec<ManifestProfile>,
    /// Binding from `node_id` (or `supervisor`) -> profile key `<scope>/<name>`.
    pub node_bindings: BTreeMap<String, String>,
    /// Connectors used, one per unique connector name.
    #[serde(default)]
    pub connectors: Vec<ManifestConnector>,
    /// Binding from `node_id` -> the grants that node holds.
    #[serde(default)]
    pub connector_grants: BTreeMap<String, Vec<ManifestConnectorGrant>>,
    /// The decision-model settings of the run (issue #165 Part 3), present
    /// only when at least one use is above off at start: providers (ids,
    /// kinds, URLs, pinned models, data class, the key REFERENCE, never a
    /// key), per-use modes and thresholds, budget and privacy. Retry and
    /// resume read it from here, so a later edit of `decisions.yaml` does not
    /// reach a started run. Absent in older manifests and in every run on a
    /// machine without `decisions.yaml`, which therefore serializes as before.
    ///
    /// Read leniently: the settings types refuse unknown fields, so a block a
    /// newer apb wrote (a new privacy knob, a new provider kind) reads as
    /// `None` here (the layer off for the run) instead of making the whole
    /// manifest unreadable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_decisions"
    )]
    pub decisions: Option<apb_core::decisions::EffectiveDecisions>,
    // --- host execution mode (0.23.0) ---
    /// Who executes the run's agent steps, resolved once at start (see
    /// `apb_core::execution`). Absent means `cli`, so a CLI run's manifest is
    /// byte-identical to one written before the field existed. A resume keeps
    /// the mode because it is read from here, never re-resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ManifestExecution>,
    // --- end host execution mode ---
    // --- 0.24.0 irreversible consent ---
    /// Who consented to the run's irreversible effects, present only when the
    /// run's tree declares `irreversible` (see [`crate::consent`]). Written
    /// once at start: a resume keeps it, a sub-playbook inherits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<crate::consent::RunConsent>,
    // --- end 0.24.0 irreversible consent ---
    /// The run is a trial of the playbook's candidate version (issue #192):
    /// it started on the version the `candidate` pointer named. Its outcome
    /// promotes or rejects the candidate (see [`crate::candidate`]). Absent
    /// (false) for every other run, so their manifests read as before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub candidate_trial: bool,
    /// Why the start ran `current` although a candidate was waiting
    /// (`untrusted`: its trust is not inherited, see
    /// [`crate::gate::RunPermit::candidate_skipped`]). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_skipped: Option<String>,
    /// The kind of task the run does: the playbook's `quality.task_type`
    /// rendered over the start's params (`default` without one), present
    /// only for a playbook with a `quality:` block. Fixed at start, so a
    /// later edit cannot change which runs a past run is compared with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    /// The names of the params declared `secret: true` when the run
    /// started, plus any a current-run patch declared later
    /// ([`add_secret_params`]): a value classified secret once stays secret
    /// for the run and its children, whatever a later snapshot says. Absent
    /// in a run started by an older apb, which falls back to its snapshot's
    /// declarations (see `crate::run_redact`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_params: Vec<String>,
    /// The launch authorization the run was admitted with (see
    /// [`crate::gate::launch`]): who launched it, how an `ask` or `deny`
    /// launch was confirmed, the policy and its sources. Written once at
    /// start; it covers the run for its whole lifetime. Never a nonce.
    /// Absent in a run an older apb started, which a resume checks again.
    /// Read leniently, so a shape a newer apb wrote reads as absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_launch"
    )]
    pub launch: Option<apb_core::launch::LaunchRecord>,
    /// The run is unattended (`apb run --unattended`): every wait without a
    /// declared outcome fails the run. Written once at start and the one
    /// authoritative record of the mode: `run.yaml` carries a copy for the
    /// drive, which an older apb's rewrite may drop, so a drive reads the
    /// mode from here. Absent (false) for every other run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// What kind of run this is: absent for a production run, `scenario`
    /// for a scenario run, `eval` for an `apb eval` repetition. Only
    /// production runs count as history (stats, estimates, quality
    /// comparison, candidate trials, the retro). Written once at start.
    /// Unrelated to the HMAC origin stamp of [`apb_core::run_origin`].
    ///
    /// Read leniently: an origin a newer apb wrote reads as
    /// [`RunOrigin::Unknown`], which is not production either.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_origin"
    )]
    pub origin: Option<RunOrigin>,
}

/// What kind of run a manifest belongs to. Absent ([`None`]) reads as
/// [`RunOrigin::Production`]; a production run never writes the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunOrigin {
    Production,
    /// A scenario run: the drive with every outside answer scripted.
    Scenario {
        level: apb_core::scenario::ScenarioLevel,
        /// The scenario's name (its file stem).
        scenario: String,
        scenario_digest: String,
    },
    /// A repetition of `apb eval`.
    Eval,
    /// An origin this apb does not know (a newer apb wrote it).
    #[serde(other)]
    Unknown,
}

impl RunOrigin {
    /// Whether a run of this origin counts as history.
    pub fn is_production(&self) -> bool {
        matches!(self, RunOrigin::Production)
    }

    /// The origin of a run from the [`read`] of its manifest: no manifest
    /// (an older run's layout) or no `origin` in it is a production run,
    /// and a manifest that does not read is [`RunOrigin::Unknown`], which
    /// counts as no history: a run whose kind cannot be told never becomes
    /// evidence.
    pub fn of_read(read: &Result<Option<RunExecutionManifest>, EngineError>) -> RunOrigin {
        match read {
            Ok(Some(m)) => m.origin(),
            Ok(None) => RunOrigin::Production,
            Err(_) => RunOrigin::Unknown,
        }
    }
}

/// An origin that does not read (a newer apb's shape) is
/// [`RunOrigin::Unknown`], never production.
fn lenient_origin<'de, D>(d: D) -> Result<Option<RunOrigin>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<yaml_serde::Value>::deserialize(d)?;
    Ok(v.map(|v| yaml_serde::from_value(v).unwrap_or(RunOrigin::Unknown)))
}

/// A launch record that does not read (a newer apb's shape) is absent.
fn lenient_launch<'de, D>(d: D) -> Result<Option<apb_core::launch::LaunchRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<yaml_serde::Value>::deserialize(d)?;
    Ok(v.and_then(|v| yaml_serde::from_value(v).ok()))
}

// --- host execution mode (0.23.0) ---
/// The execution block of a run manifest. `mode` is kept as the raw string so
/// a mode a newer apb writes does not make the whole manifest unreadable:
/// [`RunExecutionManifest::execution_mode`] refuses to drive such a run with a
/// version message instead of silently executing it in another mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestExecution {
    pub mode: String,
    /// Where the mode came from (`apb_core::execution::ExecutionSource`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The MCP client name of the session that started the run, the host
    /// every submission is attributed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// A `cli` run started by an MCP host session: an agent step whose CLI
    /// chain cannot start at all becomes a host task (`execution_fallback`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback_to_host: bool,
}

impl ManifestExecution {
    /// The suffix a guarded run's mode carries (`cli+guard`, `host+guard`),
    /// so an apb that does not know it refuses the run with its "written
    /// by a newer apb" message while this apb reads the real mode from
    /// the part before the suffix.
    pub const GUARD_SUFFIX: &str = "+guard";

    /// The block for a resolved execution; `None` for a plain `cli` run
    /// without the fallback, which keeps the manifest unchanged.
    pub fn from_resolved(r: &apb_core::execution::ResolvedExecution) -> Option<Self> {
        Self::from_resolved_guarded(r, false)
    }

    /// [`ManifestExecution::from_resolved`] for a run whose definition
    /// uses a person-only gate or an approval: the block is always
    /// written, with the mode suffixed by [`ManifestExecution::GUARD_SUFFIX`].
    /// An apb from 0.23 on refuses to resume such a run (it does not know
    /// the suffixed mode), while it still reads and stops it; this apb
    /// reads, resumes, pauses and stops it as before.
    pub fn from_resolved_guarded(
        r: &apb_core::execution::ResolvedExecution,
        guarded: bool,
    ) -> Option<Self> {
        (guarded || r.mode != apb_core::execution::ExecutionMode::Cli || r.fallback_to_host).then(
            || Self {
                mode: if guarded {
                    format!("{}{}", r.mode.as_str(), Self::GUARD_SUFFIX)
                } else {
                    r.mode.as_str().to_string()
                },
                source: Some(r.source.as_str().to_string()),
                client: r.client.clone(),
                fallback_to_host: r.fallback_to_host,
            },
        )
    }
}

/// Whether a run of `playbook` writes the manifest guard: its definition
/// uses a person-only gate or an approval, the runs the journal
/// `run_guard` follows today. An evidence-only run keeps the journal
/// guard alone: the manifest stays as it was, so an older apb that
/// stopped such a run can still resume it past the ended guard.
pub fn manifest_guard(playbook: &apb_core::schema::Playbook) -> bool {
    apb_core::validate::guarded_features(playbook)
        .into_iter()
        .any(|f| {
            matches!(
                f,
                apb_core::validate::GuardedFeature::ApprovalBinding
                    | apb_core::validate::GuardedFeature::PersonOnly
                    | apb_core::validate::GuardedFeature::RequiredApproval
            )
        })
}

impl RunExecutionManifest {
    /// The run's execution mode. An unknown mode (written by a newer apb) is
    /// an error naming the version mismatch: a binary that does not know the
    /// mode must not drive the run in another one.
    pub fn execution_mode(&self) -> Result<apb_core::execution::ExecutionMode, EngineError> {
        match &self.execution {
            None => Ok(apb_core::execution::ExecutionMode::Cli),
            Some(e) => {
                let base = e
                    .mode
                    .strip_suffix(ManifestExecution::GUARD_SUFFIX)
                    .unwrap_or(&e.mode);
                apb_core::execution::ExecutionMode::parse(base).ok_or_else(|| {
                    EngineError::Conflict(format!(
                        "the run manifest names execution mode `{}`, which this apb {} does not know; it was written by a newer apb: upgrade apb and retry",
                        e.mode,
                        env!("CARGO_PKG_VERSION"),
                    ))
                })
            }
        }
    }

    /// Whether the run's manifest carries the guard suffix: a run whose
    /// definition uses a person-only gate or an approval.
    pub fn is_guarded(&self) -> bool {
        self.execution
            .as_ref()
            .is_some_and(|e| e.mode.ends_with(ManifestExecution::GUARD_SUFFIX))
    }

    /// The execution mode a surface shows: the part before the guard
    /// suffix, so the internal suffix never reaches a person or a host
    /// that matches on the mode ([`Self::is_guarded`] carries it as its
    /// own attribute instead). `cli` for a run without an execution block.
    pub fn base_mode(&self) -> &str {
        let mode = self
            .execution
            .as_ref()
            .map(|e| e.mode.as_str())
            .unwrap_or(apb_core::execution::ExecutionMode::Cli.as_str());
        mode.strip_suffix(ManifestExecution::GUARD_SUFFIX)
            .unwrap_or(mode)
    }

    /// Whether the run executes its agent steps as host tasks.
    pub fn is_host_mode(&self) -> Result<bool, EngineError> {
        Ok(self.execution_mode()? == apb_core::execution::ExecutionMode::Host)
    }

    /// Whether a `cli` step whose CLI chain cannot start becomes a host task.
    pub fn falls_back_to_host(&self) -> bool {
        self.execution.as_ref().is_some_and(|e| e.fallback_to_host)
    }

    /// The MCP client name of the host session that started the run.
    pub fn host_client(&self) -> Option<&str> {
        self.execution.as_ref().and_then(|e| e.client.as_deref())
    }
}

/// The execution mode of the run in `run_dir`: `cli` without a manifest.
pub fn run_execution_mode(
    run_dir: &Path,
) -> Result<apb_core::execution::ExecutionMode, EngineError> {
    match read(run_dir)? {
        Some(m) => m.execution_mode(),
        None => Ok(apb_core::execution::ExecutionMode::Cli),
    }
}
// --- end host execution mode ---

fn lenient_decisions<'de, D>(
    d: D,
) -> Result<Option<apb_core::decisions::EffectiveDecisions>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = Option::<yaml_serde::Value>::deserialize(d)?;
    Ok(v.and_then(|v| yaml_serde::from_value(v).ok()))
}

impl RunExecutionManifest {
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
            && self.connectors.is_empty()
            && self.decisions.is_none()
            // host execution mode (0.23.0): a run without agent steps of its
            // own still passes its execution on to its sub-playbooks.
            && self.execution.is_none()
            // 0.24.0: the consent a sub-playbook inherits.
            && self.consent.is_none()
            && !self.candidate_trial
            && self.candidate_skipped.is_none()
            && self.task_type.is_none()
            && self.secret_params.is_empty()
            && self.launch.is_none()
            && !self.unattended
            && self.origin.is_none()
    }

    /// The run's origin; absent is production.
    pub fn origin(&self) -> RunOrigin {
        self.origin.clone().unwrap_or(RunOrigin::Production)
    }

    /// Each bound node's profile, bundle and executor digests (the
    /// supervisor's binding included), through the profiles the bindings
    /// name.
    pub fn node_digests(&self) -> BTreeMap<String, NodeDigests> {
        self.node_bindings
            .keys()
            .filter_map(|node| {
                let p = self.for_node(node)?;
                Some((
                    node.clone(),
                    NodeDigests {
                        profile: p.profile_digest.clone(),
                        bundle: p.bundle_digest.clone(),
                        executor: p.executor_digest(),
                    },
                ))
            })
            .collect()
    }

    pub fn for_node(&self, node_id: &str) -> Option<&ManifestProfile> {
        let key = self.node_bindings.get(node_id)?;
        self.profiles.iter().find(|p| &p.key() == key)
    }

    pub fn connector(&self, name: &str) -> Option<&ManifestConnector> {
        self.connectors.iter().find(|c| c.name == name)
    }

    pub fn grants_for(&self, node_id: &str) -> &[ManifestConnectorGrant] {
        self.connector_grants
            .get(node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

fn manifest_path(run_dir: &Path) -> std::path::PathBuf {
    run_dir.join("manifest.yaml")
}

/// Writes the manifest exactly once, crash-safe. We write the FULL content
/// to a temp file (0600 on unix), fsync it, then publish it at the target
/// path via a hard link: `link()` is atomic and does NOT overwrite an
/// existing path (no-clobber - a concurrent/repeat writer gets
/// AlreadyExists), and by the time of publishing the file is already intact.
/// Finally we fsync the directory so the directory-entry write survives a
/// crash. An interruption BEFORE the link leaves only the temp file (cleaned
/// up by the next writer), never an empty/corrupt immutable manifest at the
/// target path (spec 3.6).
pub fn write(run_dir: &Path, manifest: &RunExecutionManifest) -> Result<(), EngineError> {
    let path = manifest_path(run_dir);
    let dir = path.parent().unwrap_or(run_dir);
    std::fs::create_dir_all(dir)?;
    let yaml = yaml_serde::to_string(manifest).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let tmp = dir.join(format!(".manifest.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(yaml.as_bytes())?;
        f.sync_all()?;
    }

    let publish = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp); // the temp file is no longer needed either way
    match publish {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(EngineError::Invalid(
                "run manifest already exists (immutable)".into(),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    // fsync the directory: the manifest's directory entry must survive a crash.
    if let Ok(dir_f) = std::fs::File::open(dir) {
        let _ = dir_f.sync_all();
    }
    Ok(())
}

// --- 0.24.0 irreversible consent ---
/// Replaces the consent a run's manifest records: an exception to the
/// write-once rule, for a resume that asked the person (a run of an
/// irreversible tree that has no valid consent, such as one started by an
/// older apb) or one that drops a consent it cannot honour. The rest of the
/// manifest is kept as read (see [`amend`]).
pub fn replace_consent(
    run_dir: &Path,
    run_id: &str,
    consent: Option<crate::consent::RunConsent>,
) -> Result<(), EngineError> {
    amend(run_dir, run_id, |manifest| {
        if manifest.is_none() && consent.is_none() {
            return false;
        }
        manifest.get_or_insert_with(Default::default).consent = consent;
        true
    })
}
// --- end 0.24.0 irreversible consent ---

/// Adds `names` to the secret params the manifest records: the other
/// exception to the write-once rule, for a current-run patch that declares
/// a param secret. Names are only ever added, never removed, so a value
/// classified secret once stays secret for the rest of the run.
pub fn add_secret_params(
    run_dir: &Path,
    run_id: &str,
    names: impl IntoIterator<Item = String>,
) -> Result<(), EngineError> {
    let names: Vec<String> = names.into_iter().collect();
    amend(run_dir, run_id, |manifest| {
        let known = manifest.as_ref().map_or(&[][..], |m| &m.secret_params[..]);
        let new: Vec<String> = names
            .iter()
            .filter(|n| !known.contains(n))
            .cloned()
            .collect();
        if new.is_empty() {
            return false;
        }
        let m = manifest.get_or_insert_with(Default::default);
        m.secret_params.extend(new);
        m.secret_params.sort();
        m.secret_params.dedup();
        true
    })
}

/// Rewrites a run's manifest through `change`, which says whether it
/// changed anything. The run-origin stamp covers the manifest, so it is
/// renewed, but only for a directory that carried a valid stamp before: a
/// run directory apb did not create here does not become one.
fn amend(
    run_dir: &Path,
    run_id: &str,
    change: impl FnOnce(&mut Option<RunExecutionManifest>) -> bool,
) -> Result<(), EngineError> {
    let was_local = apb_core::run_origin::verify(run_dir, run_id);
    let mut manifest = read(run_dir)?;
    if !change(&mut manifest) {
        return Ok(());
    }
    let Some(manifest) = manifest else {
        return Ok(());
    };
    let yaml = yaml_serde::to_string(&manifest).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&manifest_path(run_dir), yaml.as_bytes())?;
    if was_local {
        apb_core::run_origin::stamp(run_dir, run_id)?;
    }
    Ok(())
}

/// Reads the run manifest. `Ok(None)` means there is no manifest (the
/// executor path without profiles).
pub fn read(run_dir: &Path) -> Result<Option<RunExecutionManifest>, EngineError> {
    let path = manifest_path(run_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let m = yaml_serde::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))?;
    Ok(Some(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unattended` is write-once and authoritative: absent from an older
    /// manifest, which reads as attended and writes back without it; a set
    /// one round trips and alone makes the manifest worth writing.
    #[test]
    fn the_unattended_record_keeps_old_manifests_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\n",
        )
        .unwrap();
        let old = read(dir.path()).unwrap().unwrap();
        assert!(!old.unattended && old.is_empty());
        assert!(!yaml_serde::to_string(&old).unwrap().contains("unattended"));
        let m = RunExecutionManifest {
            unattended: true,
            ..Default::default()
        };
        assert!(!m.is_empty());
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &m).unwrap();
        assert!(read(dir.path()).unwrap().unwrap().unattended);
    }

    /// The origin is absent from a production manifest (older manifests
    /// and every production run read and write as before), a scenario
    /// origin round trips and alone makes the manifest worth writing, and
    /// an origin a newer apb wrote reads as unknown, never production.
    #[test]
    fn the_run_origin_keeps_production_manifests_and_never_reads_a_newer_one_as_production() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\n",
        )
        .unwrap();
        let old = read(dir.path()).unwrap().unwrap();
        assert_eq!(old.origin(), RunOrigin::Production);
        assert!(old.is_empty());
        assert!(!yaml_serde::to_string(&old).unwrap().contains("origin"));

        let m = RunExecutionManifest {
            origin: Some(RunOrigin::Scenario {
                level: apb_core::scenario::ScenarioLevel::Quick,
                scenario: "green".into(),
                scenario_digest: "sha256:s".into(),
            }),
            ..Default::default()
        };
        assert!(!m.is_empty());
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &m).unwrap();
        let back = read(dir.path()).unwrap().unwrap();
        assert_eq!(back.origin, m.origin);
        assert!(!back.origin().is_production());

        for newer in [
            "origin:\n  kind: replay\n  source: x\n",
            "origin:\n  kind: scenario\n  level: deep2\n  scenario: g\n  scenario_digest: d\n",
            "origin: 7\n",
        ] {
            std::fs::write(
                manifest_path(dir.path()),
                format!("profiles: []\nnode_bindings: {{}}\n{newer}"),
            )
            .unwrap();
            let m = read(dir.path()).unwrap().unwrap();
            assert_eq!(m.origin(), RunOrigin::Unknown, "{newer}");
            assert!(!m.origin().is_production());
        }
    }

    #[test]
    fn a_cli_manifest_carries_no_execution_block() {
        let m = RunExecutionManifest::default();
        assert!(!yaml_serde::to_string(&m).unwrap().contains("execution"));
        assert_eq!(
            m.execution_mode().unwrap(),
            apb_core::execution::ExecutionMode::Cli
        );
    }

    #[test]
    fn a_host_manifest_round_trips_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let m = RunExecutionManifest {
            execution: Some(ManifestExecution {
                mode: "host".into(),
                source: Some("argument".into()),
                client: Some("some-host".into()),
                fallback_to_host: false,
            }),
            ..Default::default()
        };
        write(dir.path(), &m).unwrap();
        assert_eq!(
            run_execution_mode(dir.path()).unwrap(),
            apb_core::execution::ExecutionMode::Host
        );
    }

    /// A person-only gate, an approval binding or a required approval
    /// writes the manifest guard; an evidence-only run and a plain run
    /// do not.
    #[test]
    fn manifest_guard_names_the_approval_features_only() {
        let base = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
        let with = |node: &str| {
            apb_core::schema::Playbook::from_yaml(&base.replace(
                "  - { id: done, type: finish, outcome: success }",
                &format!("{node}\n  - {{ id: done, type: finish, outcome: success }}"),
            ))
            .unwrap()
        };
        assert!(!manifest_guard(
            &apb_core::schema::Playbook::from_yaml(base).unwrap()
        ));
        for node in [
            "  - id: gate\n    type: human_review\n    person_only: true\n    options: [approve]\n",
            "  - id: gate\n    type: human_review\n    approves: { results: [work] }\n    options: [approve]\n",
            "  - id: work\n    type: agent_task\n    profile: x\n    requires_approval: { gate: gate }\n    prompt: p\n",
        ] {
            assert!(manifest_guard(&with(node)), "{node}");
        }
        assert!(
            !manifest_guard(&with(
                "  - id: work\n    type: agent_task\n    profile: x\n    evidence: { from: [work] }\n    prompt: p\n"
            )),
            "evidence only keeps the journal guard"
        );
    }

    #[test]
    fn a_guarded_manifest_mode_reads_as_its_real_mode() {
        for (mode, want) in [
            ("cli+guard", apb_core::execution::ExecutionMode::Cli),
            ("host+guard", apb_core::execution::ExecutionMode::Host),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                manifest_path(dir.path()),
                format!("profiles: []\nnode_bindings: {{}}\nexecution:\n  mode: {mode}\n"),
            )
            .unwrap();
            let m = read(dir.path()).unwrap().unwrap();
            assert_eq!(m.execution_mode().unwrap(), want, "{mode}");
            assert_eq!(
                m.is_host_mode().unwrap(),
                want == apb_core::execution::ExecutionMode::Host
            );
            assert_eq!(
                m.base_mode(),
                mode.strip_suffix("+guard").unwrap(),
                "{mode}"
            );
            assert_eq!(m.is_guarded(), mode.ends_with("+guard"), "{mode}");
        }
        // A bare `guarded` names no real mode, so it still refuses.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\nexecution:\n  mode: guarded\n",
        )
        .unwrap();
        let m = read(dir.path()).unwrap().unwrap();
        assert!(m.execution_mode().is_err());
    }

    #[test]
    fn an_execution_mode_from_a_newer_apb_refuses_with_a_version_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\nexecution:\n  mode: warp\n  future: 1\n",
        )
        .unwrap();
        let m = read(dir.path()).unwrap().unwrap();
        let err = m.execution_mode().unwrap_err().to_string();
        assert!(err.contains("`warp`") && err.contains("newer apb"), "{err}");
    }

    #[test]
    fn a_decisions_block_from_a_newer_apb_leaves_the_manifest_readable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\ndecisions:\n  mode: shadow\n  timeout_ms: 3000\n  providers: []\n  budget: { max_requests_per_run: 5, max_usd_per_run: 1.0 }\n  privacy: { send: [prompts], redact: true, max_state_bytes: 24000, debug_state: false, future_knob: 1 }\n  uses: {}\n",
        )
        .unwrap();
        let m = read(dir.path()).unwrap().unwrap();
        assert!(m.decisions.is_none());
    }

    #[test]
    fn manifest_account_cmd_defaults_to_empty_and_roundtrips() {
        let acct = ManifestAccount {
            name: "a".to_string(),
            default: false,
            fields: BTreeMap::from([("base_url".to_string(), "https://x".to_string())]),
            env: BTreeMap::new(),
            cmd: BTreeMap::from([("token".to_string(), "gh auth token".to_string())]),
            digest: "sha256:x".to_string(),
        };
        let yaml = yaml_serde::to_string(&acct).unwrap();
        let back: ManifestAccount = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(back, acct);
        // An older manifest without `cmd` still parses (serde default).
        let legacy = "name: a\ndefault: false\nfields: {}\nenv: {}\ndigest: sha256:x\n";
        let parsed: ManifestAccount = yaml_serde::from_str(legacy).unwrap();
        assert!(parsed.cmd.is_empty());
    }

    #[test]
    fn manifest_profile_hermetic_defaults_false_and_roundtrips() {
        let mp = ManifestProfile {
            scope: "project".into(),
            name: "architect".into(),
            profile_digest: "sha256:a".into(),
            bundle_digest: "sha256:b".into(),
            soul: "role".into(),
            soul_requirement: SoulRequirement::Any,
            skills: Vec::new(),
            chain: Vec::new(),
            ephemeral: false,
            hermetic: true,
            zcode_mode: None,
            process_env: None,
            tiers: Vec::new(),
            routed_tier: None,
            cascade: 0,
        };
        let yaml = yaml_serde::to_string(&mp).unwrap();
        let back: ManifestProfile = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(back, mp);
        // An older manifest without `hermetic` still parses (serde default).
        let legacy = "scope: project\nname: architect\nprofile_digest: sha256:a\nbundle_digest: sha256:b\nsoul: role\nsoul_requirement: any\nskills: []\nchain: []\n";
        let parsed: ManifestProfile = yaml_serde::from_str(legacy).unwrap();
        assert!(!parsed.hermetic);
    }
}
