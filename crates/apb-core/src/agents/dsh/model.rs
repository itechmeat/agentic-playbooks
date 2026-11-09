//! dsh's model strings and the run-scoped patch that carries the model.
//!
//! dsh has no model flag: the model is the `agent-default-model` row
//! (`provider`, `model`, `reasoningEffort`) of the composed profile, and a
//! `--patch <file>` overlay replaces that row's whole config. apb writes a
//! patch with exactly that row for each spawn ([`patch_args`]).
//!
//! An apb dsh model string is `[<route>/]<model>[@<effort>]`. The route is a
//! dsh provider route: `deepseek-official` (the official DeepSeek API, key
//! `DEEPSEEK_API_KEY`) when the string names none, or a provider profile the
//! person configured for dsh's `llm-pi-ai` adapter (`zai`, `openrouter`, ...).
//! The model may hold `/` itself (`openrouter/deepseek/deepseek-v4-pro`): only
//! the first `/` ends the route. Every part is checked against a small
//! character set, and the patch writes each value JSON-quoted, so no model
//! string can add a YAML tag, an expression or another row.

use std::path::{Path, PathBuf};

use crate::cloud_models::{CloudRoute, CloudSnapshotRef, near_ai};

/// The route a bare model runs on: the official DeepSeek API.
pub const DEFAULT_ROUTE: &str = "deepseek-official";

/// The route apb owns for NEAR AI Cloud: the profile spelling
/// `nearai/<catalog id>[@<effort>]` and the `llm-pi-ai` provider key apb
/// writes into the patch of a spawn on it.
pub const NEAR_ROUTE: &str = "nearai";

/// The cloud catalogs dsh reaches through apb's own `llm-pi-ai` routes.
pub static CLOUD_ROUTES: &[CloudRoute] = &[CloudRoute {
    provider: near_ai::ID,
    prefix: NEAR_ROUTE,
}];

/// The key variable of the cloud provider a route serves, when the route is
/// one of [`CLOUD_ROUTES`].
pub fn cloud_key_env(route: &str) -> &'static [&'static str] {
    CLOUD_ROUTES
        .iter()
        .find(|r| r.prefix == route)
        .and_then(|r| crate::cloud_models::lookup(r.provider))
        .map_or(&[], |p| std::slice::from_ref(&p.key_env))
}

/// The models the official DeepSeek API serves, DeepSeek's own listing
/// (dsh 0.2.2-rc.1 `llm-deepseek` catalog, and the API's refusal of any
/// other name): apb's closed list for dsh.
pub const DEEPSEEK_MODELS: &[&str] = &["deepseek-flash", "deepseek-v4-pro"];

/// The file name of a patch: `model-<digest>.patch.yml`, named after its
/// content.
fn patch_file(body: &str) -> String {
    let digest = crate::content::sha256_hex(body.as_bytes());
    let short: String = digest
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect();
    format!("model-{short}.patch.yml")
}

/// One model selection, as dsh's `agent-default-model` row takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub route: String,
    pub model: String,
    pub effort: Option<String>,
}

impl Selection {
    /// The billing account the selection spends: one route, one key.
    pub fn account_key(&self) -> String {
        format!("{}:{}", super::ID, self.route)
    }

    /// The model as apb's lists name it: the bare id on the DeepSeek route,
    /// `<route>/<model>` elsewhere, without the effort.
    pub fn list_id(&self) -> String {
        if self.route == DEFAULT_ROUTE {
            self.model.clone()
        } else {
            format!("{}/{}", self.route, self.model)
        }
    }

    /// The spelling a profile stores: [`Self::list_id`] with the effort.
    pub fn canonical(&self) -> String {
        match &self.effort {
            Some(e) => format!("{}@{e}", self.list_id()),
            None => self.list_id(),
        }
    }
}

fn route_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn model_char(c: char) -> bool {
    route_char(c) || matches!(c, '/' | ':')
}

fn effort_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

fn checked<'a>(what: &str, raw: &'a str, ok: fn(char) -> bool) -> Result<&'a str, String> {
    if raw.is_empty() {
        return Err(format!("dsh model: empty {what}"));
    }
    match raw.chars().find(|c| !ok(*c)) {
        Some(c) => Err(format!(
            "dsh model: {what} `{raw}` holds `{c}`, which dsh ids never do"
        )),
        None => Ok(raw),
    }
}

/// Parses an apb dsh model string; `Ok(None)` for an empty one (the
/// profile's own default model then runs).
pub fn parse(raw: &str) -> Result<Option<Selection>, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let (body, effort) = match s.rsplit_once('@') {
        Some((b, e)) => (b, Some(checked("effort", e, effort_char)?.to_string())),
        None => (s, None),
    };
    let (route, model) = match body.split_once('/') {
        Some((r, m)) => (checked("route", r, route_char)?, m),
        None => (DEFAULT_ROUTE, body),
    };
    let model = checked("model", model, model_char)?;
    if model.starts_with('/') || model.ends_with('/') || model.contains("//") {
        return Err(format!("dsh model: `{s}` has an empty path segment"));
    }
    Ok(Some(Selection {
        route: route.to_string(),
        model: model.to_string(),
        effort,
    }))
}

/// The patch file's bytes for `sel`: the `agent-default-model` row, and,
/// for a step on apb's NEAR route with the run's catalog row, the
/// `llm-pi-ai` row with that one route. A `--patch` overlay replaces the
/// whole `config` of the entry it names, so the person's own `llm-pi-ai`
/// routes are not mounted in such a spawn. Without a row (a run an older apb
/// started) the model row alone is written, and the person's own `nearai`
/// route, if any, serves it.
pub fn patch_body(sel: &Selection, cloud: Option<&CloudSnapshotRef>) -> String {
    let mut out = model_row(sel);
    if let Some(c) = cloud.filter(|_| sel.route == NEAR_ROUTE)
        && let Some(provider) = crate::cloud_models::lookup(&c.provider)
    {
        out.push_str(&provider_row(provider, c));
    }
    out
}

/// The `llm-pi-ai` row with one route for the run's catalog row: no price,
/// no secret (the key's name only), no person's setting.
fn provider_row(provider: &crate::cloud_models::CloudProvider, c: &CloudSnapshotRef) -> String {
    let q = |s: &str| serde_json::Value::from(s).to_string();
    let mut out = String::from(
        "- id: llm-pi-ai\n  name: \"@deepseek-ai/dsh-llm-pi-ai\"\n  config:\n    providers:\n",
    );
    out.push_str(&format!("      {}:\n", q(NEAR_ROUTE)));
    out.push_str(&format!("        displayName: {}\n", q(provider.display)));
    out.push_str("        api: \"openai-completions\"\n");
    out.push_str(&format!("        baseURL: {}\n", q(provider.base_url)));
    out.push_str(&format!("        apiKeyEnv: {}\n", q(provider.key_env)));
    out.push_str("        models:\n");
    out.push_str(&format!("          - id: {}\n", q(&c.model.id)));
    if let Some(n) = c.model.context_tokens {
        out.push_str(&format!("            contextWindow: {n}\n"));
    }
    if let Some(n) = c.model.max_output_tokens {
        out.push_str(&format!("            maxTokens: {n}\n"));
    }
    let efforts = c.model.efforts();
    if efforts.is_empty() {
        out.push_str("            reasoningEfforts: false\n");
    } else {
        let pairs: Vec<String> = efforts
            .iter()
            .map(|e| format!("{}: {}", q(e), q(e)))
            .collect();
        out.push_str(&format!(
            "            reasoningEfforts: {{ {} }}\n",
            pairs.join(", ")
        ));
    }
    out
}

/// The `agent-default-model` row for `sel`.
fn model_row(sel: &Selection) -> String {
    let q = |s: &str| serde_json::Value::from(s).to_string();
    let mut out = String::from(
        "# Written by apb for one dsh step: the model the step runs on.\n\
         - id: agent-default-model\n  name: \"@deepseek-ai/dsh-agent-default-model\"\n  config:\n",
    );
    out.push_str(&format!("    provider: {}\n", q(&sel.route)));
    out.push_str(&format!("    model: {}\n", q(&sel.model)));
    if let Some(e) = &sel.effort {
        out.push_str(&format!("    reasoningEffort: {}\n", q(e)));
    }
    out
}

/// Where the patches of spawns outside a run live: apb's own state
/// directory, never a shared temporary directory.
fn unscoped_dir() -> std::io::Result<PathBuf> {
    Ok(crate::config::config_dir()
        .ok_or_else(|| std::io::Error::other("no apb config directory for dsh's model patch"))?
        .join("state")
        .join("dsh-patch"))
}

/// The leading launcher arguments of one spawn on `model`: `--patch <file>`
/// with the model's row, written into `scoped_dir` (the node's agent home)
/// or apb's state directory outside a run; none for an empty model.
///
/// The file is named after its content and never rewritten with other
/// bytes, so items of one list node that run at once on different fallback
/// models each read their own model, whatever order they start in.
pub fn patch_args(
    model: &str,
    scoped_dir: Option<&Path>,
    cloud: Option<&CloudSnapshotRef>,
) -> std::io::Result<Vec<String>> {
    let Some(sel) = parse(model).map_err(std::io::Error::other)? else {
        return Ok(Vec::new());
    };
    let body = patch_body(&sel, cloud);
    let dir = match scoped_dir {
        Some(dir) => dir.to_path_buf(),
        None => unscoped_dir()?,
    };
    let dest = dir.join(patch_file(&body));
    if !std::fs::read(&dest).is_ok_and(|cur| cur == body.as_bytes()) {
        crate::fsutil::atomic_write_private(&dest, body.as_bytes())?;
    }
    let path = dest
        .to_str()
        .ok_or_else(|| std::io::Error::other("dsh model patch path is not valid UTF-8"))?;
    Ok(vec!["--patch".to_string(), path.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(route: &str, model: &str, effort: Option<&str>) -> Selection {
        Selection {
            route: route.into(),
            model: model.into(),
            effort: effort.map(str::to_string),
        }
    }

    #[test]
    fn a_bare_model_runs_on_the_deepseek_route() {
        assert_eq!(
            parse("deepseek-v4-pro@high").unwrap(),
            Some(sel(DEFAULT_ROUTE, "deepseek-v4-pro", Some("high")))
        );
        assert_eq!(
            parse(" deepseek-flash ").unwrap(),
            Some(sel(DEFAULT_ROUTE, "deepseek-flash", None))
        );
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn only_the_first_slash_ends_the_route() {
        let s = parse("openrouter/deepseek/deepseek-v4-pro:free@low")
            .unwrap()
            .unwrap();
        assert_eq!(
            s,
            sel("openrouter", "deepseek/deepseek-v4-pro:free", Some("low"))
        );
        assert_eq!(s.list_id(), "openrouter/deepseek/deepseek-v4-pro:free");
        assert_eq!(s.account_key(), "dsh:openrouter");
    }

    #[test]
    fn the_deepseek_route_is_stored_bare() {
        let s = parse("deepseek-official/deepseek-v4-pro@max")
            .unwrap()
            .unwrap();
        assert_eq!(s.canonical(), "deepseek-v4-pro@max");
        assert_eq!(s.list_id(), "deepseek-v4-pro");
        let z = parse("zai/glm-5.3").unwrap().unwrap();
        assert_eq!(z.canonical(), "zai/glm-5.3");
    }

    /// Nothing that could change the YAML shape of the patch gets through.
    #[test]
    fn strings_that_could_reshape_the_patch_are_refused() {
        for bad in [
            "deepseek\"flash",
            "deep seek",
            "a/b\nc",
            "x@HIGH",
            "!!js process.exit()",
            "zai/",
            "/m",
            "r/a//b",
            "m@",
            "{a}",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_patch_holds_one_row_with_quoted_values() {
        let body = patch_body(&sel(DEFAULT_ROUTE, "deepseek-v4-pro", Some("high")), None);
        assert_eq!(
            body,
            "# Written by apb for one dsh step: the model the step runs on.\n\
             - id: agent-default-model\n  name: \"@deepseek-ai/dsh-agent-default-model\"\n  config:\n    \
             provider: \"deepseek-official\"\n    model: \"deepseek-v4-pro\"\n    reasoningEffort: \"high\"\n"
        );
        let parsed: serde_json::Value = yaml_serde::from_str(&body).unwrap();
        assert_eq!(parsed[0]["config"]["model"], "deepseek-v4-pro");
        assert!(!patch_body(&sel("zai", "glm-5.3", None), None).contains("reasoningEffort"));
    }

    /// Items of one list node share the node's agent home and may run at
    /// once on different fallback models: each model gets its own file, and
    /// a later launch never rewrites the file an earlier one named.
    #[test]
    fn overlapping_launches_on_different_routes_keep_their_own_patch() {
        let dir = tempfile::tempdir().unwrap();
        let a = patch_args("deepseek-v4-pro@high", Some(dir.path()), None).unwrap();
        let b = patch_args("zai/glm-5.3-flash@low", Some(dir.path()), None).unwrap();
        assert_ne!(a[1], b[1]);
        assert!(
            std::fs::read_to_string(&a[1])
                .unwrap()
                .contains("provider: \"deepseek-official\"")
        );
        assert!(
            std::fs::read_to_string(&b[1])
                .unwrap()
                .contains("provider: \"zai\"")
        );
        assert_eq!(
            patch_args("deepseek-v4-pro@high", Some(dir.path()), None).unwrap(),
            a
        );
        assert!(
            std::fs::read_to_string(&a[1])
                .unwrap()
                .contains("deepseek-v4-pro")
        );
    }

    #[test]
    fn the_scoped_patch_is_written_once_and_named_in_front() {
        let dir = tempfile::tempdir().unwrap();
        let args = patch_args("deepseek-flash@low", Some(dir.path()), None).unwrap();
        let body = patch_body(&parse("deepseek-flash@low").unwrap().unwrap(), None);
        let file = dir.path().join(patch_file(&body));
        assert_eq!(
            args,
            ["--patch".to_string(), file.to_string_lossy().into_owned()]
        );
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.contains("model: \"deepseek-flash\""));
        assert!(patch_args("", Some(dir.path()), None).unwrap().is_empty());
        assert!(patch_args("bad model", Some(dir.path()), None).is_err());
    }

    fn near(id: &str, effort: Option<&str>) -> CloudSnapshotRef {
        let snap = crate::cloud_models::catalog::embedded(&near_ai::NEAR_AI);
        CloudSnapshotRef {
            provider: near_ai::ID.into(),
            route: NEAR_ROUTE.into(),
            model: crate::cloud_models::catalog::find(id, &snap)
                .unwrap()
                .clone(),
            effort: effort.map(str::to_string),
            catalog_digest: snap.digest.clone(),
            fetched_at: snap.fetched_at.clone(),
        }
    }

    /// The golden patch of a NEAR step: both rows, every value JSON-quoted,
    /// the key by name only, no price.
    #[test]
    fn a_near_step_patch_carries_the_provider_row() {
        let c = near("Qwen/Qwen3.8-27B", Some("high"));
        let s = parse("nearai/Qwen/Qwen3.8-27B@high").unwrap().unwrap();
        let body = patch_body(&s, Some(&c));
        assert_eq!(
            body,
            "# Written by apb for one dsh step: the model the step runs on.\n\
             - id: agent-default-model\n  name: \"@deepseek-ai/dsh-agent-default-model\"\n  config:\n    \
             provider: \"nearai\"\n    model: \"Qwen/Qwen3.8-27B\"\n    reasoningEffort: \"high\"\n\
             - id: llm-pi-ai\n  name: \"@deepseek-ai/dsh-llm-pi-ai\"\n  config:\n    providers:\n      \
             \"nearai\":\n        displayName: \"NEAR AI Cloud\"\n        api: \"openai-completions\"\n        \
             baseURL: \"https://cloud-api.near.ai/v1\"\n        apiKeyEnv: \"NEARAI_API_KEY\"\n        \
             models:\n          - id: \"Qwen/Qwen3.8-27B\"\n            contextWindow: 262144\n            \
             maxTokens: 65536\n            reasoningEfforts: { \"low\": \"low\", \"medium\": \"medium\", \"high\": \"high\" }\n"
        );
        let parsed: serde_json::Value = yaml_serde::from_str(&body).unwrap();
        let route = &parsed[1]["config"]["providers"]["nearai"];
        assert_eq!(route["apiKeyEnv"], "NEARAI_API_KEY");
        assert_eq!(route["models"][0]["reasoningEfforts"]["high"], "high");
        assert!(!body.contains("1.35") && !body.contains("price"));
    }

    #[test]
    fn a_plain_row_writes_no_efforts_and_leaves_absent_sizes_out() {
        let mut c = near("deepseek/deepseek-v3.2", None);
        let s = parse("nearai/deepseek/deepseek-v3.2").unwrap().unwrap();
        let body = patch_body(&s, Some(&c));
        assert!(body.contains("reasoningEfforts: false"));
        assert!(!body.contains("maxTokens"), "the row has no output cap");
        c.model.context_tokens = None;
        assert!(!patch_body(&s, Some(&c)).contains("contextWindow"));
        // A row for another route is never written.
        let z = parse("zai/glm-5.3").unwrap().unwrap();
        assert!(!patch_body(&z, Some(&c)).contains("llm-pi-ai"));
    }

    /// A run an older apb started has no row: the model row alone, byte for
    /// byte as before.
    #[test]
    fn a_near_step_without_a_row_writes_the_model_row_alone() {
        let s = parse("nearai/Qwen/Qwen3.8-27B@low").unwrap().unwrap();
        let body = patch_body(&s, None);
        assert_eq!(body, model_row(&s));
        assert!(!body.contains("llm-pi-ai"));
        let dir = tempfile::tempdir().unwrap();
        let args = patch_args("nearai/Qwen/Qwen3.8-27B@low", Some(dir.path()), None).unwrap();
        assert_eq!(std::fs::read_to_string(&args[1]).unwrap(), body);
    }

    #[test]
    fn two_near_models_get_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let qa = near("Qwen/Qwen3.8-27B", None);
        let qb = near("z-ai/glm-5.3-flash", None);
        let a = patch_args("nearai/Qwen/Qwen3.8-27B", Some(dir.path()), Some(&qa)).unwrap();
        let b = patch_args("nearai/z-ai/glm-5.3-flash", Some(dir.path()), Some(&qb)).unwrap();
        assert_ne!(a[1], b[1]);
        let before = std::fs::metadata(&a[1]).unwrap().modified().unwrap();
        patch_args("nearai/Qwen/Qwen3.8-27B", Some(dir.path()), Some(&qa)).unwrap();
        assert_eq!(
            std::fs::metadata(&a[1]).unwrap().modified().unwrap(),
            before
        );
        assert!(
            std::fs::read_to_string(&b[1])
                .unwrap()
                .contains("glm-5.3-flash")
        );
    }

    /// Composition against a real dsh, with no model call: skipped when dsh
    /// is not on `PATH`.
    #[test]
    #[ignore = "needs a local dsh; run with --ignored where dsh is installed"]
    fn the_near_patch_composes_in_a_real_dsh() {
        let Ok(found) = std::process::Command::new("dsh").arg("--version").output() else {
            return;
        };
        assert!(found.status.success());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let c = near("Qwen/Qwen3.8-27B", Some("high"));
        let args = patch_args("nearai/Qwen/Qwen3.8-27B@high", Some(dir.path()), Some(&c)).unwrap();
        let out = std::process::Command::new("dsh")
            .args([
                "--profile",
                "headless",
                "--patch",
                &args[1],
                "--dump-config",
            ])
            .env("DSH_HOME", &home)
            .env_remove("NEARAI_API_KEY")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("provider: nearai"), "{text}");
        assert!(
            text.contains("baseURL: https://cloud-api.near.ai/v1"),
            "{text}"
        );
        assert!(text.contains("apiKeyEnv: NEARAI_API_KEY"), "{text}");
    }

    /// The route's parser and dsh's own agree on every catalog id.
    #[test]
    fn the_cloud_route_and_dshs_parser_agree_on_every_catalog_id() {
        let snap = crate::cloud_models::catalog::embedded(&near_ai::NEAR_AI);
        for m in &snap.models {
            let spelled = format!("{NEAR_ROUTE}/{}", m.id);
            let s = parse(&spelled).unwrap().unwrap();
            assert_eq!(
                (s.route.as_str(), s.model.as_str()),
                (NEAR_ROUTE, m.id.as_str())
            );
            let r = crate::cloud_models::CloudModelRef::read(&spelled, CLOUD_ROUTES).unwrap();
            assert_eq!(r.id, m.id);
        }
    }
}
