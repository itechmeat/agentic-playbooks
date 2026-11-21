//! The template gallery's one install entry, shared by the CLI and the HTTP
//! API: a template installs as a draft (`created_by: template`) with its
//! eval scenarios and the fit report of the new draft. It never creates,
//! revokes or changes an approval. Listing and showing are allowed anywhere;
//! an install is refused from a run step (`template_in_run_step`).

use std::path::Path;

use serde_json::{Value, json};

use super::ToolError;

/// The refusal code of a template install inside a run step.
pub const IN_RUN_STEP: &str = "template_in_run_step";

/// One template install.
pub struct InstallRequest<'a> {
    pub template: &'a str,
    /// `project` or `global` (`super::scope_parent`).
    pub scope: &'a str,
    /// The playbook id to install as; the template's own id when `None`.
    pub as_id: Option<&'a str>,
    /// Whether `root` is a project the person named: the fit report is
    /// made only then.
    pub with_fit: bool,
}

/// Every template's summary: `{"templates": [TemplateSummary, ..]}`.
pub fn list() -> Value {
    json!({ "templates": apb_core::templates::all().iter().map(|t| &t.summary).collect::<Vec<_>>() })
}

/// One template: `{"template": TemplateSummary, "yaml": "<text>"}`.
pub fn show(id: &str) -> Result<Value, ToolError> {
    let t = apb_core::templates::get(id)
        .ok_or_else(|| ToolError::NotFound(format!("template `{id}`")))?;
    Ok(json!({ "template": t.summary, "yaml": t.yaml }))
}

/// Installs a template as a draft: the playbook (version 1.0.0, lifecycle
/// `draft`, provenance `created_by: template`), its eval files, and the
/// answer with the fit report of the new draft. It never creates, revokes
/// or changes an approval.
pub fn install(root: &Path, req: InstallRequest<'_>) -> Result<Value, ToolError> {
    // A step of a run does not leave definitions behind: an install only
    // ever makes a draft, so an agent session outside a marked run is
    // allowed; the code goes in front of the guard's sentence.
    if let Err(e) = super::outside_run(
        &apb_core::step_env::StepCaller::from_env(),
        "installing a template",
    ) {
        return Err(match e {
            ToolError::Conflict(why) => ToolError::Conflict(format!("{IN_RUN_STEP}: {why}")),
            other => other,
        });
    }
    let template = apb_core::templates::get(req.template)
        .ok_or_else(|| ToolError::NotFound(format!("template `{}`", req.template)))?;
    let id = req.as_id.unwrap_or(req.template);
    if !apb_core::registry::is_safe_segment(id) {
        return Err(ToolError::Engine(format!("invalid id `{id}`")));
    }
    if apb_core::bundled::is_reserved_id(id) {
        return Err(ToolError::Engine(apb_core::bundled::reserved_id_message(
            id,
        )));
    }
    let (_, parent) = super::scope_parent(root, req.scope)?;

    // The draft write shared with the capture: version 1.0.0, lifecycle
    // draft, provenance created_by template. The template's own content is
    // apb's, so the capture's secret scan does not run, and a trigger of a
    // template matching a person's playbook is no reason to refuse (the id
    // decides): no trigger dedup either.
    let (version, _provenance, provenance_written) = match super::capture::write_draft(
        &parent,
        id,
        &template.yaml,
        req.scope == "global",
        super::capture::CaptureOrigin::Template {
            id: req.template,
            digest: &template.summary.digest,
        },
        &template.summary.name,
    )? {
        Err(rejected) => return Ok(rejected),
        Ok(w) => w,
    };

    // The eval files: a failure is a warning, never a failed install. The
    // store path is checked again right before the eval tree is written.
    let playbook_dir = parent.join("playbooks").join(id);
    let eval = super::capture::refuse_linked_store(&parent, id)
        .map_err(|e| e.to_string())
        .and_then(|()| apb_core::templates::install_evals(&playbook_dir, &template));

    // The fit report of the new draft, only with a project named.
    let origin = if req.scope == "global" {
        apb_core::scope::Origin::Global
    } else {
        apb_core::scope::Origin::Project { workspace_id: None }
    };
    let fit = req.with_fit.then(|| {
        #[cfg(feature = "testing")]
        if super::install_faults::fails(super::install_faults::Fault::Fit) {
            return Err("injected fit failure".to_string());
        }
        apb_engine::fit::assess(
            root,
            &apb_core::scope::PlaybookRef {
                origin,
                id: id.to_string(),
                version: None,
            },
        )
        .map(|report| {
            let needs: Vec<String> = apb_engine::fit::problems(&report)
                .map(|i| match &i.subject {
                    Some(s) => format!("{} {s}: {}", i.code, i.detail),
                    None => format!("{}: {}", i.code, i.detail),
                })
                .collect();
            (serde_json::to_value(&report).unwrap_or(Value::Null), needs)
        })
        .map_err(|e| e.to_string())
    });

    let warnings = warnings_of(&eval, fit.as_ref(), provenance_written);
    let scenarios = eval.unwrap_or(0);
    let (fit, needs) = fit
        .and_then(Result::ok)
        .unwrap_or((Value::Null, Vec::new()));
    Ok(answer(
        id,
        &version,
        req.scope,
        req.template,
        scenarios,
        &fit,
        needs,
        &warnings,
    ))
}

/// The warnings of an install, from its raw results: the eval copy, the
/// fit report (`None` when no project was named) and whether the
/// provenance was written. None of them undoes the install.
fn warnings_of<T>(
    eval: &Result<usize, String>,
    fit: Option<&Result<T, String>>,
    provenance_written: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Err(e) = eval {
        warnings.push(format!("eval scenarios not installed: {e}"));
    }
    match fit {
        None => warnings.push("fit report not made: no project was named".into()),
        Some(Err(e)) => warnings.push(format!("fit report not made: {e}")),
        Some(Ok(_)) => {}
    }
    if !provenance_written {
        warnings.push("provenance not written".into());
    }
    warnings
}

/// The install answer, from the install's results and the warnings
/// [`warnings_of`] built from them.
#[allow(clippy::too_many_arguments)]
fn answer(
    id: &str,
    version: &str,
    scope: &str,
    template: &str,
    scenarios: usize,
    fit: &Value,
    needs: Vec<String>,
    warnings: &[String],
) -> Value {
    json!({
        "id": id,
        "version": version,
        "scope": scope,
        "template": template,
        "lifecycle": apb_core::trust::Lifecycle::Draft.as_str(),
        "scenarios": scenarios,
        "fit": fit,
        "needs": needs,
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failed_result_is_its_warning() {
        let failed: Result<(), String> = Err("registry".into());
        assert_eq!(
            warnings_of(&Err("Io(\"busy\")".into()), Some(&failed), false),
            [
                "eval scenarios not installed: Io(\"busy\")",
                "fit report not made: registry",
                "provenance not written",
            ]
        );
        assert_eq!(
            warnings_of::<()>(&Ok(3), None, true),
            ["fit report not made: no project was named"]
        );
        assert!(warnings_of(&Ok(3), Some(&Ok(())), true).is_empty());
    }
}
