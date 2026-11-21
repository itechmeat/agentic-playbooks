//! The template gallery: the example playbooks apb ships, embedded from
//! `examples/playbooks/` the way the bundled content is embedded
//! (`crate::bundled`). A template is a starting point a person installs
//! as a draft and adapts; it is never approved, never upgraded and never
//! managed by apb. Only the top-level `<id>.yaml` files and their `<id>/evals/`
//! are read.

use std::path::Path;

#[derive(rust_embed::Embed)]
#[folder = "../../examples/playbooks"]
struct TemplateAssets;

/// The `created_by` of a draft installed from a template.
pub const CREATED_BY: &str = "template";

/// One template, as every surface lists it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TemplateSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// The effective effects (`crate::effects::effective`), as that
    /// function's values serialize.
    pub effects: Vec<String>,
    /// How many eval scenario files ship with it (the files under
    /// `evals/scenarios/`).
    pub scenarios: usize,
    /// `sha256:<hex>` of its yaml bytes.
    pub digest: String,
}

/// One template with its content.
#[derive(Debug, Clone)]
pub struct Template {
    pub summary: TemplateSummary,
    pub yaml: String,
    /// Its `evals/`, paths relative to that folder.
    pub evals: crate::tree_install::FileMap,
}

/// Every template, sorted by id. A file that does not parse, or whose id
/// is not its file name, is skipped (the test `every_example_is_a_template`
/// keeps that from happening in a release).
pub fn all() -> Vec<Template> {
    select(
        TemplateAssets::iter()
            .filter_map(|path| {
                let file = TemplateAssets::get(path.as_ref())?;
                Some((path.to_string(), file.data.into_owned()))
            })
            .collect(),
    )
}

/// The templates in an inventory of `(path, bytes)` pairs, sorted by id:
/// every top-level `<id>.yaml` that parses and names the id of its stem,
/// with the files under `<id>/evals/` as its eval scenarios. Nested
/// folders and files whose id differs from their stem are left out.
/// [`all`] is this over the embedded folder.
pub(crate) fn select(files: Vec<(String, Vec<u8>)>) -> Vec<Template> {
    let by_folder = crate::tree_install::group_by_folder(
        files
            .iter()
            .filter(|(p, _)| p.contains('/'))
            .map(|(p, b)| (p.clone(), b.clone())),
    );
    let mut out = Vec::new();
    let mut tops: Vec<(String, Vec<u8>)> = files
        .iter()
        .filter(|(p, _)| !p.contains('/') && p.ends_with(".yaml"))
        .cloned()
        .collect();
    tops.sort();
    for (name, bytes) in tops {
        let stem = name.strip_suffix(".yaml").unwrap_or(&name).to_string();
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let Ok(pb) = crate::schema::Playbook::from_yaml(text) else {
            continue;
        };
        if pb.id != stem {
            continue;
        }
        let folder = by_folder.get(&stem);
        let evals = folder
            .map(|f| crate::bundled::sub_tree(f, crate::eval::EVALS_DIR))
            .unwrap_or_default();
        let effects: Vec<String> = crate::effects::effective(&pb)
            .into_iter()
            .filter_map(|e| match serde_json::to_value(e) {
                Ok(serde_json::Value::String(s)) => Some(s),
                _ => None,
            })
            .collect();
        let summary = TemplateSummary {
            id: pb.id.clone(),
            name: pb.name.clone(),
            description: pb.description.clone(),
            version: pb.version.clone(),
            scenarios: evals.keys().filter(|k| k.starts_with("scenarios/")).count(),
            digest: crate::content::sha256_hex(&bytes),
            effects,
        };
        out.push(Template {
            summary,
            yaml: text.to_string(),
            evals,
        });
    }
    out
}

/// The template `id`.
pub fn get(id: &str) -> Option<Template> {
    if !crate::registry::is_safe_segment(id) {
        return None;
    }
    all().into_iter().find(|t| t.summary.id == id)
}

/// Writes the template's `evals/` into the new draft's `playbook_dir`
/// (`<playbook_dir>/evals/`). Nothing to write, or an identical tree
/// already there, is `Ok(0)`; the count of files written otherwise. An
/// existing different `evals/` is never replaced: that is an `Err`.
pub fn install_evals(playbook_dir: &Path, template: &Template) -> Result<usize, String> {
    if template.evals.is_empty() {
        return Ok(0);
    }
    match crate::tree_install::install_tree(
        playbook_dir,
        crate::eval::EVALS_DIR,
        &template.evals,
        |_| false,
    ) {
        Ok(installed) if installed.no_op => Ok(0),
        Ok(_) => Ok(template.evals.len()),
        Err(e) => Err(format!("{e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `a_nested_folder_is_not_a_template`: only a top-level `<id>.yaml`
    /// whose id is its stem is a template, with the eval files of the
    /// folder of its name. A nested folder without such a file, and a
    /// top-level file whose id differs, are never selected.
    #[test]
    fn a_nested_folder_is_not_a_template() {
        let a = r#"
schema: 2
id: a
name: A
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;
        let b = a.replace("id: a", "id: b").replace("name: A", "name: B");
        let c = a.replace("id: a", "id: other");
        let files = vec![
            ("a.yaml".to_string(), a.as_bytes().to_vec()),
            ("a/evals/scenarios/s.yaml".to_string(), b"x".to_vec()),
            ("nested/b.yaml".to_string(), b.as_bytes().to_vec()),
            ("c.yaml".to_string(), c.as_bytes().to_vec()),
        ];
        let out = select(files);
        assert_eq!(
            out.len(),
            1,
            "only `a` is selected: {:?}",
            out.iter().map(|t| t.summary.id.clone()).collect::<Vec<_>>()
        );
        let t = &out[0];
        assert_eq!(t.summary.id, "a");
        assert_eq!(t.evals.len(), 1, "its one eval file");
        assert!(t.evals.contains_key("scenarios/s.yaml"));
    }
}
