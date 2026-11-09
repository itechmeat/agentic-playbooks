//! The one judgement of a cloud model, shared by every surface: the profile
//! write, `apb validate`, the doctor, fit and adoption, the run gate and the
//! node rules.

use super::catalog::CatalogSnapshot;
use super::*;

/// Why a cloud model will not run an agent step, or may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudIssue {
    /// The catalog does not list it; `as_of` is the catalog's fetch time.
    Unknown { provider: String, as_of: String },
    /// A chat model without tool calling.
    NoTools,
    /// Not text in and text out.
    NotChat,
    /// An effort the row does not offer; `allowed` is what it offers.
    EffortUnsupported { allowed: Vec<String> },
}

impl CloudIssue {
    /// The stable finding code.
    pub fn code(&self) -> &'static str {
        match self {
            CloudIssue::Unknown { .. } => "cloud_model_unknown",
            CloudIssue::NoTools => "cloud_model_no_tools",
            CloudIssue::NotChat => "cloud_model_not_chat",
            CloudIssue::EffortUnsupported { .. } => "cloud_effort_unsupported",
        }
    }

    /// Whether an agent step on the model cannot work. An unknown model may
    /// be newer than the catalog, so it is only a warning while authoring.
    pub fn is_blocking(&self) -> bool {
        !matches!(self, CloudIssue::Unknown { .. })
    }

    /// One sentence naming the model.
    pub fn describe(&self, r: &CloudModelRef) -> String {
        let display = lookup(r.provider).map_or(r.provider, |p| p.display);
        match self {
            CloudIssue::Unknown { as_of, .. } => format!(
                "`{}` is not in the {display} catalog fetched {}; check the id or run `apb models --refresh`",
                r.id,
                date(as_of)
            ),
            CloudIssue::NoTools => format!(
                "`{}` has no tool calling; agent steps need it, so a cli run on it is refused at start",
                r.id
            ),
            CloudIssue::NotChat => format!(
                "`{}` is not a chat model (text in and text out); agent steps cannot run on it",
                r.id
            ),
            CloudIssue::EffortUnsupported { allowed } => {
                let effort = r.effort.as_deref().unwrap_or_default();
                if allowed.is_empty() {
                    format!("`{}` takes no reasoning effort, not `{effort}`", r.id)
                } else {
                    format!(
                        "`{}` takes the efforts {}, not `{effort}`",
                        r.id,
                        allowed.join(", ")
                    )
                }
            }
        }
    }
}

/// The first issue with running an agent step on `r`, or `None`.
pub fn check(r: &CloudModelRef, snaps: &[CatalogSnapshot]) -> Option<CloudIssue> {
    let unknown = |as_of: &str| CloudIssue::Unknown {
        provider: r.provider.to_string(),
        as_of: as_of.to_string(),
    };
    let Some(snap) = snaps.iter().find(|s| s.provider == r.provider) else {
        return Some(unknown(""));
    };
    let Some(row) = catalog::find(&r.id, snap) else {
        return Some(unknown(&snap.fetched_at));
    };
    if !row.is_chat() {
        return Some(CloudIssue::NotChat);
    }
    if !row.features.tools {
        return Some(CloudIssue::NoTools);
    }
    if let Some(e) = &r.effort
        && !row.efforts().contains(&e.as_str())
    {
        return Some(CloudIssue::EffortUnsupported {
            allowed: row.efforts().iter().map(|s| s.to_string()).collect(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::near_ai::NEAR_AI;
    use super::*;

    fn r(id: &str, effort: Option<&str>) -> CloudModelRef {
        CloudModelRef {
            provider: "near-ai-cloud",
            prefix: "nearai",
            id: id.into(),
            effort: effort.map(str::to_string),
        }
    }

    #[test]
    fn each_issue_and_a_good_model() {
        let snaps = vec![catalog::embedded(&NEAR_AI)];
        assert_eq!(check(&r("Qwen/Qwen3.8-27B", Some("high")), &snaps), None);
        assert_eq!(check(&r("Qwen/Qwen3.8-27B", None), &snaps), None);
        let unknown = check(&r("Qwen/Qwen3.8-72B", None), &snaps).unwrap();
        assert_eq!(unknown.code(), "cloud_model_unknown");
        assert!(!unknown.is_blocking());
        assert!(
            unknown
                .describe(&r("Qwen/Qwen3.8-72B", None))
                .contains("2026-11-09")
        );
        let vl = check(&r("Qwen/Qwen3-VL-30B-A3B-Instruct", None), &snaps).unwrap();
        assert_eq!(vl, CloudIssue::NoTools);
        assert!(vl.is_blocking());
        assert!(vl.describe(&r("x", None)).contains("no tool calling"));
        assert_eq!(
            check(&r("typesafe/jev-1.13", None), &snaps),
            Some(CloudIssue::NotChat)
        );
        assert_eq!(
            check(&r("deepseek/deepseek-v3.2", Some("high")), &snaps)
                .unwrap()
                .code(),
            "cloud_effort_unsupported"
        );
        assert_eq!(
            check(&r("Qwen/Qwen3.8-27B", Some("max")), &snaps),
            Some(CloudIssue::EffortUnsupported {
                allowed: vec!["low".into(), "medium".into(), "high".into()]
            })
        );
        assert_eq!(
            check(&r("x", None), &[]).unwrap().code(),
            "cloud_model_unknown"
        );
    }
}
