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

/// The route a bare model runs on: the official DeepSeek API.
pub const DEFAULT_ROUTE: &str = "deepseek-official";

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

/// The patch file's bytes for `sel`: the `agent-default-model` row alone.
pub fn patch_body(sel: &Selection) -> String {
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
pub fn patch_args(model: &str, scoped_dir: Option<&Path>) -> std::io::Result<Vec<String>> {
    let Some(sel) = parse(model).map_err(std::io::Error::other)? else {
        return Ok(Vec::new());
    };
    let body = patch_body(&sel);
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
        let body = patch_body(&sel(DEFAULT_ROUTE, "deepseek-v4-pro", Some("high")));
        assert_eq!(
            body,
            "# Written by apb for one dsh step: the model the step runs on.\n\
             - id: agent-default-model\n  name: \"@deepseek-ai/dsh-agent-default-model\"\n  config:\n    \
             provider: \"deepseek-official\"\n    model: \"deepseek-v4-pro\"\n    reasoningEffort: \"high\"\n"
        );
        let parsed: serde_json::Value = yaml_serde::from_str(&body).unwrap();
        assert_eq!(parsed[0]["config"]["model"], "deepseek-v4-pro");
        assert!(!patch_body(&sel("zai", "glm-5.3", None)).contains("reasoningEffort"));
    }

    /// Items of one list node share the node's agent home and may run at
    /// once on different fallback models: each model gets its own file, and
    /// a later launch never rewrites the file an earlier one named.
    #[test]
    fn overlapping_launches_on_different_routes_keep_their_own_patch() {
        let dir = tempfile::tempdir().unwrap();
        let a = patch_args("deepseek-v4-pro@high", Some(dir.path())).unwrap();
        let b = patch_args("zai/glm-5.3-flash@low", Some(dir.path())).unwrap();
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
            patch_args("deepseek-v4-pro@high", Some(dir.path())).unwrap(),
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
        let args = patch_args("deepseek-flash@low", Some(dir.path())).unwrap();
        let body = patch_body(&parse("deepseek-flash@low").unwrap().unwrap());
        let file = dir.path().join(patch_file(&body));
        assert_eq!(
            args,
            ["--patch".to_string(), file.to_string_lossy().into_owned()]
        );
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.contains("model: \"deepseek-flash\""));
        assert!(patch_args("", Some(dir.path())).unwrap().is_empty());
        assert!(patch_args("bad model", Some(dir.path())).is_err());
    }
}
