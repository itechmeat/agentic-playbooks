//! The models table overlay and the onboarding-state roundtrip. The env is
//! global - guarded by a shared Mutex, with APB_CONFIG_DIR set for the test's duration.

use apb_core::models_table::{self, Coverage, OnboardingState};

use crate::common::env_lock as lock;

fn with_cfg(dir: &std::path::Path) {
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir);
    }
}

// Restores APB_CONFIG_DIR on drop (including on panic/early-return, unlike a
// plain statement at the end of the test body). Formerly this crate's
// models-table tests ran in their own process (one file = one binary), so
// leaving APB_CONFIG_DIR pointed at a (by-then-dropped) tempdir at test end
// was harmless - the process exited right after. Now that this module shares
// a process with every other module in the consolidated integration binary,
// an unrestored APB_CONFIG_DIR would leak into whichever test runs next.
struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

#[test]
fn overlay_adds_model_overrides_price_and_brings_subscriptions() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    std::fs::write(
        cfg.path().join("models.yaml"),
        "models:\n  - { id: claude-opus-4-8, vendor: anthropic, cost_in_usd_mtok: 1.5 }\n  - { id: local-llm, vendor: self, reasoning: medium }\nsubscriptions:\n  - { agent: claude, plan: max, coverage: full }\n  - { agent: opencode }\n",
    )
    .unwrap();

    let t = models_table::load_merged().unwrap();
    // Field-wise merge: overriding ONE price doesn't reset the existing
    // model's other fields to their defaults.
    let opus = t.models.iter().find(|m| m.id == "claude-opus-4-8").unwrap();
    assert_eq!(opus.cost_in_usd_mtok, Some(1.5));
    assert_eq!(
        opus.cost_out_usd_mtok,
        Some(25.0),
        "untouched output price must be preserved"
    );
    assert_eq!(
        opus.context_tokens,
        Some(1000000),
        "untouched context must be preserved"
    );
    assert_eq!(opus.vendor, "anthropic", "untouched vendor preserved");
    assert!(
        !opus.source_url.is_empty(),
        "untouched provenance preserved"
    );
    // The new model was added.
    assert!(t.models.iter().any(|m| m.id == "local-llm"));
    // Subscriptions come only from the overlay.
    assert_eq!(t.subscriptions.len(), 2);
    assert_eq!(t.subscriptions[0].coverage, Coverage::Full);
    // Default coverage is unknown.
    assert_eq!(t.subscriptions[1].coverage, Coverage::Unknown);
}

#[test]
fn overlay_null_clears_nullable_builtin_field() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    // An explicit `null` clears a builtin value to unknown (different from "the
    // field is absent from the patch", which leaves it untouched).
    std::fs::write(
        cfg.path().join("models.yaml"),
        "models:\n  - { id: claude-opus-4-8, reasoning: null, context_tokens: null }\n",
    )
    .unwrap();
    let t = models_table::load_merged().unwrap();
    let opus = t.models.iter().find(|m| m.id == "claude-opus-4-8").unwrap();
    assert_eq!(opus.reasoning, None, "explicit null must clear reasoning");
    assert_eq!(
        opus.context_tokens, None,
        "explicit null must clear context_tokens"
    );
    // A price untouched by the patch is preserved (absent != null).
    assert!(opus.cost_in_usd_mtok.is_some(), "untouched price preserved");
}

#[test]
fn overlay_creates_new_row_field_wise() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    // The patch creates a new model, setting only some fields: the rest fall
    // back to defaults (no panic, no borrowing of unrelated values), and the
    // ones that are set take effect.
    std::fs::write(
        cfg.path().join("models.yaml"),
        "models:\n  - { id: my-local, vendor: self, cost_in_usd_mtok: 0.0, reasoning: medium }\n",
    )
    .unwrap();
    let t = models_table::load_merged().unwrap();
    let m = t
        .models
        .iter()
        .find(|m| m.id == "my-local")
        .expect("new row created");
    assert_eq!(m.vendor, "self");
    assert_eq!(m.cost_in_usd_mtok, Some(0.0));
    assert_eq!(m.reasoning.as_deref(), Some("medium"));
    // Unset fields fall back to defaults, not a panic or garbage.
    assert_eq!(m.cost_out_usd_mtok, None);
    assert_eq!(m.context_tokens, None);
    assert!(!m.vision);
    assert!(m.source_url.is_empty());
}

#[test]
fn no_overlay_returns_builtin_without_subscriptions() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    let t = models_table::load_merged().unwrap();
    assert!(!t.models.is_empty());
    assert!(t.subscriptions.is_empty());
}

#[test]
fn corrupt_overlay_yields_error_not_builtin() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    // A present but malformed overlay must not silently fall back to builtin.
    std::fs::write(
        cfg.path().join("models.yaml"),
        "models: [ this is: : broken",
    )
    .unwrap();
    let r = models_table::load_merged();
    assert!(
        r.is_err(),
        "corrupt overlay must surface an error, not builtin"
    );
}

#[test]
fn corrupt_onboarding_state_yields_error() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    let state_dir = cfg.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("onboarding.json"), "{not json").unwrap();
    assert!(
        models_table::onboarding::read().is_err(),
        "corrupt onboarding state must surface an error"
    );
}

#[test]
fn onboarding_roundtrip_and_declined() {
    let _l = lock();
    let _g = EnvGuard;
    let cfg = tempfile::tempdir().unwrap();
    with_cfg(cfg.path());
    assert_eq!(
        models_table::onboarding::read().unwrap(),
        OnboardingState::Uninitialized
    );
    models_table::onboarding::write(OnboardingState::Configured).unwrap();
    assert_eq!(
        models_table::onboarding::read().unwrap(),
        OnboardingState::Configured
    );
    models_table::onboarding::write(OnboardingState::Declined).unwrap();
    assert_eq!(
        models_table::onboarding::read().unwrap(),
        OnboardingState::Declined
    );
}
