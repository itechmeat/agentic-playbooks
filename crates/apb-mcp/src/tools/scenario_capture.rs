//! Capturing a finished production run as a scenario on the surfaces: the
//! preview (`run_scenario_capture` on MCP, `apb scenario capture` without
//! `--save`, `POST /api/runs/{id}/scenario-capture` without `save`) and the
//! save. The fold itself is [`apb_engine::scenario::capture`]; this layer
//! adds the one guard (a run step never captures, the journal text must
//! not come back to the step that wrote it), the person-only save bound to
//! the previewed digest, and the no-clobber write.

use std::path::{Path, PathBuf};

use apb_core::launch::floor::SaveChannel;
use apb_core::scenario::write_new;
use apb_core::step_env::StepCaller;
use apb_engine::scenario::capture::{CaptureError, CaptureRequest};
use serde::{Deserialize, Serialize};

use super::{ToolError, outside_run};

/// What the caller asks for: a name, the nodes to assert, and, for a save,
/// the digest of the preview the person read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CaptureArgs {
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Write the scenario. Requires `digest`. Refused on MCP.
    #[serde(default)]
    pub save: bool,
    /// The digest of the preview the person read.
    #[serde(default)]
    pub digest: Option<String>,
}

/// One capture as the surfaces show it: the YAML, its digest, and what a
/// person should read before saving.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CapturePreview {
    pub run_id: String,
    pub playbook: String,
    pub version: String,
    /// `project` or `global`: the scope the run's playbook was started
    /// from, where a save lands. Absent for a journal that records no
    /// origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
    pub name: String,
    pub yaml: String,
    pub digest: String,
    pub redacted: usize,
    pub warnings: Vec<String>,
    /// Set after a save: where the file was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub saved: Option<String>,
}

/// The one entry point every surface goes through. `channel` is `Person`
/// for the CLI on a terminal or with `--yes`, and for the dashboard;
/// `Agent` for MCP, which previews only.
pub fn capture(
    root: &Path,
    run_id: &str,
    args: &CaptureArgs,
    channel: SaveChannel,
    step: &StepCaller,
) -> Result<CapturePreview, ToolError> {
    // The capture reads the run's journal text and hands it back as a tool
    // answer: a run step must not be handed its own run's text (it could
    // plant a file with it), for a preview and a save alike.
    outside_run(step, "capturing a scenario")?;
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(ToolError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let req = CaptureRequest {
        name: args.name.clone(),
        nodes: args.nodes.clone(),
    };
    let c = match apb_engine::scenario::capture::capture(root, &run_dir, &req) {
        Ok(c) => c,
        Err(CaptureError::NotCapturable(gaps)) => {
            let lines: Vec<String> = gaps
                .iter()
                .map(|g| match &g.node {
                    Some(node) => format!("{} ({node}): {}", g.code, g.detail),
                    None => format!("{}: {}", g.code, g.detail),
                })
                .collect();
            return Err(ToolError::Conflict(format!(
                "not_capturable: {}",
                lines.join("; ")
            )));
        }
        Err(CaptureError::Engine(e)) => return Err(e.into()),
    };
    let mut preview = CapturePreview {
        run_id: run_id.to_string(),
        playbook: c.playbook.clone(),
        version: c.version.clone(),
        scope: c.scope.map(|s| s.as_str().to_string()),
        name: args.name.clone(),
        yaml: c.yaml.clone(),
        digest: c.digest.clone(),
        redacted: c.redacted,
        warnings: c.warnings.clone(),
        saved: None,
    };
    if !args.save {
        return Ok(preview);
    }
    // A save is a person's act: an agent outside a run can read the
    // digest, so a digest proves nothing about consent.
    if channel == SaveChannel::Agent {
        return Err(ToolError::Conflict(
            "save_by_person: saving a captured scenario is a person's act; show the person the YAML and let them save it with `apb scenario capture --save` or on the dashboard".to_string(),
        ));
    }
    let Some(digest) = args.digest.as_deref() else {
        return Err(ToolError::Conflict(
            "stale_preview: a save names the digest of the preview the person read".to_string(),
        ));
    };
    if digest != c.digest {
        return Err(ToolError::Conflict(format!(
            "stale_preview: the preview the person read had digest {digest}, the run captures to {} now; preview again and save that",
            c.digest
        )));
    }
    let dir = playbook_dir_for(root, &c.playbook, c.scope)?;
    let path = write_new(&dir, &args.name, &c.yaml).map_err(|e| match e.as_str() {
        "scenario_exists" => ToolError::Conflict(format!(
            "scenario_exists: scenario `{}` already exists; pick another name",
            args.name
        )),
        _ => ToolError::Engine(e),
    })?;
    preview.saved = Some(path.display().to_string());
    Ok(preview)
}

/// The directory of playbook `id` in `scope`, the scope the run was
/// started from: a global run never saves into a project playbook of the
/// same id, nor the other way round. A journal that records no origin
/// (`None`) takes the project's playbook when the project has it, else the
/// global store's. The CLI reads it for the terminal question's path.
pub fn playbook_dir_for(
    root: &Path,
    id: &str,
    scope: Option<apb_core::scope::Scope>,
) -> Result<PathBuf, ToolError> {
    use apb_core::scope::Scope;
    if !apb_core::registry::is_safe_segment(id) {
        return Err(ToolError::NotFound(format!("playbook `{id}`")));
    }
    if scope != Some(Scope::Global) {
        let project = apb_core::versioning::playbooks_dir(root).join(id);
        if project.is_dir() {
            return Ok(project);
        }
        if scope == Some(Scope::Project) {
            return Err(ToolError::NotFound(format!(
                "playbook `{id}` in the project"
            )));
        }
    }
    let parent = apb_core::store::global_playbooks_parent().ok_or_else(|| {
        ToolError::NotFound(format!(
            "playbook `{id}`: neither the project nor a global store has it"
        ))
    })?;
    let global = parent.join("playbooks").join(id);
    if global.is_dir() {
        return Ok(global);
    }
    Err(ToolError::NotFound(format!("playbook `{id}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// One env change at a time in this binary's tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const PLAYBOOK: &str = "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Fix {{run.instruction}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n";

    /// A finished production run of `cap`, hand journaled.
    fn seed_run(root: &Path) -> String {
        let pb = root.join(".apb/playbooks/cap/1.0.0");
        fs::create_dir_all(&pb).unwrap();
        fs::write(pb.join("playbook.yaml"), PLAYBOOK).unwrap();
        fs::write(root.join(".apb/playbooks/cap/current"), "1.0.0").unwrap();
        let dir = root.join(".apb/runs/cap-1");
        fs::create_dir_all(&dir).unwrap();
        let lines = [
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
            r#"{"seq":2,"ts":1,"type":"node_started","node":"start","attempt":1}"#,
            r#"{"seq":3,"ts":1,"type":"node_finished","node":"start","status":"succeeded","attempt":1,"output":""}"#,
            r#"{"seq":4,"ts":1,"type":"node_started","node":"work","attempt":1}"#,
            r#"{"seq":5,"ts":1,"type":"attempt_started","node":"work","attempt":1,"agent":"claude-code"}"#,
            r#"{"seq":6,"ts":1,"type":"attempt_finished","node":"work","attempt":1,"status":"succeeded","output":"fixed the parser"}"#,
            r#"{"seq":7,"ts":1,"type":"node_finished","node":"work","status":"succeeded","attempt":1,"output":"fixed the parser"}"#,
            r#"{"seq":8,"ts":1,"type":"node_finished","node":"done","status":"succeeded","attempt":1,"output":""}"#,
            r#"{"seq":9,"ts":1,"type":"run_finished","outcome":"succeeded"}"#,
        ];
        fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
        fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
        fs::write(
            dir.join("run.yaml"),
            "params: {}\ninstruction: the parser\nunattended: false\n",
        )
        .unwrap();
        "cap-1".to_string()
    }

    fn args(name: &str) -> CaptureArgs {
        CaptureArgs {
            name: name.to_string(),
            nodes: Vec::new(),
            save: false,
            digest: None,
        }
    }

    /// A preview returns the YAML and writes nothing.
    #[test]
    fn a_preview_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = seed_run(tmp.path());
        let p = capture(
            tmp.path(),
            &run_id,
            &args("green-after-one-fix"),
            SaveChannel::Person,
            &StepCaller::from_env(),
        )
        .unwrap();
        assert_eq!(p.playbook, "cap");
        assert_eq!(p.version, "1.0.0");
        assert!(p.yaml.contains("id: green-after-one-fix"));
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let scenarios = tmp.path().join(".apb/playbooks/cap/evals/scenarios");
        assert!(
            !scenarios.exists(),
            "a preview wrote a file: {:?}",
            fs::read_dir(scenarios.parent().unwrap())
        );
    }

    /// A save with the preview's digest writes the file; a second save of
    /// the name is `scenario_exists`, and a digest of another capture is
    /// `stale_preview`.
    #[test]
    fn a_save_needs_the_preview_digest_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = seed_run(tmp.path());
        let preview = capture(
            tmp.path(),
            &run_id,
            &args("green-after-one-fix"),
            SaveChannel::Person,
            &StepCaller::from_env(),
        )
        .unwrap();
        // Without the digest: refused.
        let mut a = args("green-after-one-fix");
        a.save = true;
        match capture(
            tmp.path(),
            &run_id,
            &a,
            SaveChannel::Person,
            &StepCaller::from_env(),
        ) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("stale_preview"), "{m}"),
            other => panic!("expected a conflict, got {:?}", other.is_ok()),
        }
        // With another capture's digest: refused, nothing written.
        a.digest = Some("sha256:other".into());
        match capture(
            tmp.path(),
            &run_id,
            &a,
            SaveChannel::Person,
            &StepCaller::from_env(),
        ) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("stale_preview"), "{m}"),
            other => panic!("expected a conflict, got {:?}", other.is_ok()),
        }
        assert!(
            !tmp.path()
                .join(".apb/playbooks/cap/evals/scenarios/green-after-one-fix.yaml")
                .exists()
        );
        // With the preview's digest: written.
        a.digest = Some(preview.digest.clone());
        let saved = capture(
            tmp.path(),
            &run_id,
            &a,
            SaveChannel::Person,
            &StepCaller::from_env(),
        )
        .unwrap();
        let path = saved.saved.clone().expect("a save path");
        assert!(path.ends_with("evals/scenarios/green-after-one-fix.yaml"));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("id: green-after-one-fix")
        );
        // A second save of the name is a refusal, the file unchanged.
        let before = fs::read(&path).unwrap();
        match capture(
            tmp.path(),
            &run_id,
            &a,
            SaveChannel::Person,
            &StepCaller::from_env(),
        ) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("scenario_exists"), "{m}"),
            other => panic!("expected a conflict, got {:?}", other.is_ok()),
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    /// An agent channel never saves.
    #[test]
    fn an_agent_channel_never_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = seed_run(tmp.path());
        let preview = capture(
            tmp.path(),
            &run_id,
            &args("green-after-one-fix"),
            SaveChannel::Agent,
            &StepCaller::from_env(),
        )
        .unwrap();
        let mut a = args("green-after-one-fix");
        a.save = true;
        a.digest = Some(preview.digest);
        match capture(
            tmp.path(),
            &run_id,
            &a,
            SaveChannel::Agent,
            &StepCaller::from_env(),
        ) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("save_by_person"), "{m}"),
            other => panic!("expected a conflict, got {:?}", other.is_ok()),
        }
        assert!(
            !tmp.path()
                .join(".apb/playbooks/cap/evals/scenarios")
                .exists()
        );
    }

    /// A run step is refused, preview and save alike.
    #[test]
    fn a_run_step_is_refused() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("APB_RUN_ID");
        // SAFETY: under ENV_LOCK, the one env change at a time.
        unsafe { std::env::set_var("APB_RUN_ID", "cap-1") };
        let tmp = tempfile::tempdir().unwrap();
        let run_id = seed_run(tmp.path());
        let step = StepCaller::from_env();
        let a = args("green-after-one-fix");
        let refused = capture(tmp.path(), &run_id, &a, SaveChannel::Person, &step);
        // SAFETY: under ENV_LOCK.
        unsafe {
            match &old {
                Some(v) => std::env::set_var("APB_RUN_ID", v),
                None => std::env::remove_var("APB_RUN_ID"),
            }
        }
        match refused {
            Err(ToolError::Conflict(m)) => assert!(m.contains("capturing a scenario"), "{m}"),
            other => panic!("expected a conflict, got {:?}", other.is_ok()),
        }
    }

    /// A playbook the project does not have, the global store does: the
    /// scenario lands in the global store.
    #[test]
    fn a_global_playbooks_scenario_lands_in_the_global_store() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("APB_CONFIG_DIR");
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: under ENV_LOCK, the one env change at a time.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        let global = cfg.path().join("playbooks/gcap/1.0.0");
        fs::create_dir_all(&global).unwrap();
        fs::write(
            global.join("playbook.yaml"),
            PLAYBOOK.replace("id: cap", "id: gcap"),
        )
        .unwrap();
        fs::write(cfg.path().join("playbooks/gcap/current"), "1.0.0").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let run_id = seed_run(tmp.path());
        // The run executed the global playbook `gcap`.
        let dir = tmp.path().join(".apb/runs/cap-1");
        let text = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        fs::write(
            dir.join("events.jsonl"),
            text.replace("\"playbook\":\"cap\"", "\"playbook\":\"gcap\""),
        )
        .unwrap();
        fs::write(
            dir.join("playbook.yaml"),
            PLAYBOOK.replace("id: cap", "id: gcap"),
        )
        .unwrap();
        let step = StepCaller::from_env();
        let preview = capture(
            tmp.path(),
            &run_id,
            &args("global-green"),
            SaveChannel::Person,
            &step,
        );
        let preview = match preview {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: under ENV_LOCK.
                unsafe {
                    match &old {
                        Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                        None => std::env::remove_var("APB_CONFIG_DIR"),
                    }
                }
                panic!("the capture failed: {e}");
            }
        };
        let mut a = args("global-green");
        a.save = true;
        a.digest = Some(preview.digest);
        let saved = capture(tmp.path(), &run_id, &a, SaveChannel::Person, &step);
        // SAFETY: under ENV_LOCK.
        unsafe {
            match &old {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        let saved = saved.unwrap();
        let path = saved.saved.expect("a save path");
        assert!(
            path.starts_with(cfg.path().join("playbooks/gcap").to_str().unwrap()),
            "the scenario landed outside the global store: {path}"
        );
    }
    /// A run started from the global playbook, finished before the project
    /// gained a playbook of the same id: its scenario lands in the global
    /// store, never in the project's playbook.
    #[test]
    fn a_global_run_saves_into_the_global_store_when_the_project_has_the_id() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("APB_CONFIG_DIR");
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: under ENV_LOCK, the one env change at a time.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        let global = cfg.path().join("playbooks/cap/1.0.0");
        fs::create_dir_all(&global).unwrap();
        fs::write(global.join("playbook.yaml"), PLAYBOOK).unwrap();
        fs::write(cfg.path().join("playbooks/cap/current"), "1.0.0").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // The project has `cap` too (seed_run writes it).
        let run_id = seed_run(tmp.path());
        let dir = tmp.path().join(".apb/runs/cap-1");
        let text = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        fs::write(
            dir.join("events.jsonl"),
            text.replace(
                "\n{\"seq\":2,",
                "\n{\"seq\":100,\"ts\":1,\"type\":\"run_provenance\",\"origin\":\"global\"}\n{\"seq\":2,",
            ),
        )
        .unwrap();
        let step = StepCaller::from_env();
        let result = capture(
            tmp.path(),
            &run_id,
            &args("global-green"),
            SaveChannel::Person,
            &step,
        )
        .and_then(|preview| {
            assert_eq!(preview.scope.as_deref(), Some("global"));
            let mut a = args("global-green");
            a.save = true;
            a.digest = Some(preview.digest);
            capture(tmp.path(), &run_id, &a, SaveChannel::Person, &step)
        });
        // SAFETY: under ENV_LOCK.
        unsafe {
            match &old {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        let path = result.unwrap().saved.expect("a save path");
        assert!(
            path.starts_with(cfg.path().join("playbooks/cap").to_str().unwrap()),
            "the scenario landed outside the global store: {path}"
        );
        assert!(
            !tmp.path()
                .join(".apb/playbooks/cap/evals/scenarios/global-green.yaml")
                .exists()
        );
    }
}
