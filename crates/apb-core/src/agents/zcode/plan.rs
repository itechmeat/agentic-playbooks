//! zcode model strings: the plans ZCode knows, apb's allowlist on the paid
//! Individual plan, and how an apb model string (`[<plan>/]<model>[@<effort>]`)
//! parses, is spelled and is checked. Pure: nothing here reads a file.

/// The agent id.
pub const AGENT_ID: &str = "zcode";

/// The plan kinds ZCode knows, in apb's preference order: paid plans first, so
/// that a listing and an unqualified model both favor the paid plan.
/// `(provider id suffix, short alias suffix)`.
const PLAN_KINDS: &[(&str, &str)] = &[
    ("individual-coding-plan", "individual"),
    ("team-coding-plan", "team"),
    ("start-plan", "start"),
    ("offpeak-idle-plan", "idle"),
];

/// The zcode models apb lists and accepts on ZCode's own plans, in offer
/// order, all on the paid Individual plan (the only one the headless CLI can
/// use). ZCode's provider config enables more (GLM-5.2, GLM-5-Turbo, and the
/// same ids on the free Start plan), but the allowlist pins apb's supported
/// set: anything else is refused with a clear error instead of being handed
/// to ZCode. The ids are the canonical spelling; input is matched
/// case-insensitively and rewritten to it.
pub const ALLOWED_MODELS: &[&str] = &["GLM-5.3", "GLM-5.3-Flash"];

/// Whether `model_id` is on [`ALLOWED_MODELS`] (case-insensitive: ZCode ids
/// are matched the same way everywhere else here).
pub fn is_allowed_model(model_id: &str) -> bool {
    ALLOWED_MODELS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(model_id))
}

/// What the allowlist admits, for error messages.
pub(super) fn allowlist_message() -> String {
    format!(
        "apb offers only {} for zcode (the paid Individual plan)",
        ALLOWED_MODELS.join(" and ")
    )
}

/// The model list apb offers for zcode: exactly [`ALLOWED_MODELS`], as bare
/// model ids in offer order. A bare id resolves to the paid Individual plan
/// (see [`parse_model`]); no other plan is ever listed, because the headless
/// CLI cannot use one ([`check_plan_usable`](super::provider::check_plan_usable)).
pub fn model_list() -> Vec<String> {
    ALLOWED_MODELS.iter().map(|m| (*m).to_string()).collect()
}

/// Account families (`providerFamilyDomain` in ZCode's settings).
pub(super) const FAMILIES: &[&str] = &["zai", "bigmodel"];

/// Family used when the settings file does not name one.
pub const DEFAULT_FAMILY: &str = "zai";

/// A parsed apb model string for zcode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    /// ZCode provider id, e.g. `account:zai-individual-coding-plan`.
    pub provider_id: String,
    /// ZCode model id, e.g. `GLM-5.3`.
    pub model_id: String,
    /// Reasoning level (ZCode's "effort"), e.g. `max`. `None` lets ZCode pick
    /// its default (the highest level the model offers).
    pub effort: Option<String>,
}

impl ModelSelection {
    /// The value ZCode's `defaultModelSelection` key takes.
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "providerId": self.provider_id,
            "modelId": self.model_id,
        });
        if let Some(e) = &self.effort {
            v["options"] = serde_json::json!({ "reasoningLevel": e });
        }
        v
    }

    /// Billing account this selection spends: one per plan. A quota or auth
    /// failure blocks this key, not the whole agent, so a fallback onto the
    /// same model on another plan stays allowed.
    pub fn account_key(&self) -> String {
        format!("{AGENT_ID}:{}", self.provider_id)
    }
}

/// Short alias of a ZCode provider id: `account:zai-individual-coding-plan`
/// -> `zai-individual`. `None` for a provider id that is not a known plan
/// (custom providers keep their own id).
pub fn plan_alias(provider_id: &str) -> Option<String> {
    let rest = provider_id.strip_prefix("account:")?;
    for family in FAMILIES {
        if let Some(kind) = rest.strip_prefix(&format!("{family}-")) {
            for (suffix, short) in PLAN_KINDS {
                if kind == *suffix {
                    return Some(format!("{family}-{short}"));
                }
            }
        }
    }
    None
}

/// Resolves the plan part of a model string to a ZCode provider id. Accepts
/// the short alias (`zai-individual`, `zai-start`), the provider id without
/// its `account:` prefix (`zai-start-plan`), the full provider id
/// (`account:zai-start-plan`), and a bare kind (`individual`, `start`,
/// `start-plan`) which takes `family`. Anything else is passed through
/// verbatim, so a custom provider from the user's personal config works too.
pub fn resolve_plan(plan: &str, family: &str) -> String {
    let p = plan.trim();
    let lower = p.to_ascii_lowercase();
    if lower.starts_with("account:") {
        // A known plan in any case resolves to its one spelling, so every
        // `account:` gate (the allowlist, the login check, the plan's
        // models) sees it; an unknown one keeps its own spelling.
        let rest = &p["account:".len()..];
        let resolved = resolve_plan(rest, family);
        return if resolved.starts_with("account:") {
            resolved
        } else {
            format!("account:{rest}")
        };
    }
    for fam in FAMILIES {
        if let Some(kind) = lower.strip_prefix(&format!("{fam}-")) {
            for (suffix, short) in PLAN_KINDS {
                if kind == *suffix || kind == *short {
                    return format!("account:{fam}-{suffix}");
                }
            }
        }
    }
    for (suffix, short) in PLAN_KINDS {
        if lower == *suffix || lower == *short {
            return format!("account:{family}-{suffix}");
        }
    }
    p.to_string()
}

/// Parses an apb zcode model string: `[<plan>/]<model>[@<effort>]`.
///
/// - `GLM-5.3`, `GLM-5.3-Flash@high` - the canonical spelling: a bare model
///   id resolves deterministically to the PAID individual plan of the account
///   family (`family`, from ZCode's settings, `zai` by default);
/// - `zai-individual/GLM-5.3` - the older plan-qualified spelling, still
///   accepted for backward compatibility and equal to the bare name; apb
///   never emits it (see [`canonical_model`]);
/// - another plan (`zai-start/...`) still parses, but the allowlist
///   ([`check_model_allowed`]) and the login check ([`check_plan_usable`](super::provider::check_plan_usable))
///   refuse it before zcode starts.
///
/// The model id is matched case-insensitively against [`ALLOWED_MODELS`] and
/// then `known_models` (the ids from ZCode's built-in provider config) and
/// rewritten to its canonical spelling, so `glm-5.3` works. An empty string
/// is `None`: ZCode then keeps the user's own default selection.
pub fn parse_model(raw: &str, family: &str, known_models: &[String]) -> Option<ModelSelection> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (body, effort) = match s.rsplit_once('@') {
        Some((b, e)) if !e.trim().is_empty() => (b.trim(), Some(e.trim().to_ascii_lowercase())),
        Some((b, _)) => (b.trim(), None),
        None => (s, None),
    };
    let (provider_id, model) = match body.split_once('/') {
        Some((plan, model)) => (resolve_plan(plan, family), model.trim()),
        None => (resolve_plan("individual", family), body),
    };
    if model.is_empty() {
        return None;
    }
    // Canonical spelling: the allowlist first (static, always available),
    // then the ids from ZCode's built-in config; anything else (a custom
    // provider's model, or a model newer than apb) keeps its input spelling.
    let model_id = ALLOWED_MODELS
        .iter()
        .find(|k| k.eq_ignore_ascii_case(model))
        .map(|k| (*k).to_string())
        .or_else(|| {
            known_models
                .iter()
                .find(|k| k.eq_ignore_ascii_case(model))
                .cloned()
        })
        .unwrap_or_else(|| model.to_string());
    Some(ModelSelection {
        provider_id,
        model_id,
        effort,
    })
}

/// The individual (paid) plan's provider id suffix.
const INDIVIDUAL_PLAN_KIND: &str = "individual-coding-plan";

/// The refusal for a selection outside the allowlist, or `None` when it is
/// fine: a model on [`ALLOWED_MODELS`] on the individual plan. Custom
/// providers (anything that is not an `account:` plan) are not governed.
pub(super) fn allowlist_refusal(sel: &ModelSelection) -> Option<String> {
    if !sel.provider_id.starts_with("account:") {
        return None;
    }
    if let Some(refusal) = model_refusal(sel) {
        return Some(refusal);
    }
    if !sel.provider_id.ends_with(INDIVIDUAL_PLAN_KIND) {
        let plan = plan_alias(&sel.provider_id).unwrap_or_else(|| sel.provider_id.clone());
        return Some(format!(
            "zcode plan `{plan}` is not allowed: {}; write the bare model id",
            allowlist_message()
        ));
    }
    None
}

/// The refusal of a selection whose model is off apb's allowlist, the one
/// text every gate gives; `None` for an allowed model.
pub(super) fn model_refusal(sel: &ModelSelection) -> Option<String> {
    (!is_allowed_model(&sel.model_id)).then(|| {
        format!(
            "zcode model `{}` is not allowed: {}",
            sel.model_id,
            allowlist_message()
        )
    })
}

/// Whether an apb zcode model string names a model apb supports on ZCode's
/// own plans: `Ok(())`, or `Err` with the message that names the allowlist.
/// The bare id, the legacy `zai-individual/` spelling and an effort suffix
/// are all fine; another plan (`zai-start/...`) or a model off the allowlist
/// is refused. Custom providers (anything that does not resolve to an
/// `account:` plan) pass - the allowlist governs ZCode's plans only. This is
/// the advisory half of the gate (profile writes, adopt); [`spawn_env_in`](super::provider::spawn_env_in)
/// enforces the same rule before zcode starts.
pub fn check_model_allowed(raw: &str, family: &str) -> Result<(), String> {
    match parse_model(raw, family, &[]) {
        // Empty: ZCode keeps the user's own default selection.
        None => Ok(()),
        Some(sel) => allowlist_refusal(&sel).map_or(Ok(()), Err),
    }
}

/// The canonical spelling of an apb zcode model string: the bare model id
/// with its effort suffix (`zai-individual/glm-5.3-flash@HIGH` ->
/// `GLM-5.3-Flash@high`) when it resolves to the individual plan; anything
/// else (another plan, a custom provider, an empty string) is returned
/// trimmed but otherwise unchanged, so the allowlist check can name it.
pub fn canonical_model(raw: &str, family: &str) -> String {
    match parse_model(raw, family, &[]) {
        Some(sel)
            if sel.provider_id.ends_with(INDIVIDUAL_PLAN_KIND)
                && sel.provider_id.starts_with("account:") =>
        {
            match &sel.effort {
                Some(e) => format!("{}@{e}", sel.model_id),
                None => sel.model_id,
            }
        }
        _ => raw.trim().to_string(),
    }
}
