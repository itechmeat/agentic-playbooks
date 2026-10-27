//! Reads one baseline check's source into a [`Snapshot`].
//!
//! Whatever cannot be read honestly reads as unknown, never as a value: a
//! connector call that failed, came back cut or with an inbox gap, or does
//! not satisfy `complete_when`; a script that was tampered with, exited
//! non-zero, timed out, printed too much, said `"complete": false`, or
//! changed the working tree (or ran in one that cannot be checked); a
//! project command that could not run.
//!
//! A connector read is bounded by the check's `timeout_seconds` as well as
//! the function's own `timeout_sec`: the smaller one applies.

use std::collections::BTreeMap;
use std::time::Duration;

use apb_core::baseline::{BaselineCheck, BaselineConnector, BaselineSource, extract};
use apb_core::content::sha256_hex;
use serde_json::Value;

pub(crate) use super::ctx::ReadCtx;
use super::ctx::unknown;
use super::{Snapshot, status};
use crate::state::NodeStatus;

/// The most stdout a baseline script may print; more reads as unknown.
const SCRIPT_STDOUT_CAP: usize = 1024 * 1024;

/// A snapshot that read `doc`, with every observation of `check` taken out
/// of it.
fn observed(
    check: &BaselineCheck,
    doc: &Value,
    input_digest: String,
    inputs: Vec<String>,
    ctx: &ReadCtx<'_>,
) -> Snapshot {
    Snapshot {
        status: status::OBSERVED.into(),
        observations: check
            .observe
            .iter()
            .map(|o| (o.id.clone(), extract(doc, o)))
            .collect(),
        input_digest,
        inputs,
        workdir: Some(ctx.workdir_text()),
        ..Snapshot::default()
    }
}

/// Reads `check`. `prior` is the before snapshot when taking the after one:
/// project checks rerun exactly the commands it recorded.
pub(crate) fn read(check: &BaselineCheck, ctx: &ReadCtx<'_>, prior: Option<&Snapshot>) -> Snapshot {
    match check.source() {
        Some(BaselineSource::Connector(conn)) => connector(check, conn, ctx),
        Some(BaselineSource::Script(path)) => script(check, path, ctx),
        Some(BaselineSource::ProjectChecks(spec)) => super::project::read(
            spec,
            Duration::from_secs(check.timeout_seconds),
            ctx,
            prior
                .map(|p| p.commands.as_slice())
                .filter(|c| !c.is_empty()),
        ),
        None => unknown(
            "the check does not name exactly one source",
            String::new(),
            Vec::new(),
            ctx,
        ),
    }
}

/// `{{params.*}}` rendered into every string of `v`; nothing else is
/// templated (validator V65).
fn render_params(v: &Value, ctx: &ReadCtx<'_>) -> Value {
    match v {
        Value::String(s) => Value::String(crate::context::render(
            s,
            ctx.params,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "",
            &crate::context::OutputClip {
                run_dir: ctx.run_dir,
                max_bytes: 0,
                retro: None,
            },
        )),
        Value::Array(items) => Value::Array(items.iter().map(|i| render_params(i, ctx)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, i)| (k.clone(), render_params(i, ctx)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Why a call result cannot be read as a whole, current answer, or the body
/// it carries.
fn readable_body(result: &Value) -> Result<&Value, String> {
    if result.get("ok") != Some(&Value::Bool(true)) {
        let err = &result["error"];
        return Err(format!(
            "the call failed ({}): {}",
            err["code"].as_str().unwrap_or("error"),
            err["message"].as_str().unwrap_or("no message")
        ));
    }
    if result.get("response_complete") == Some(&Value::Bool(false))
        || result.get("truncated") == Some(&Value::Bool(true))
    {
        return Err("the response was not read whole".into());
    }
    if result.get("gap").is_some() {
        return Err("the inbox reported a gap".into());
    }
    Ok(&result["body"])
}

fn connector(check: &BaselineCheck, conn: &BaselineConnector, ctx: &ReadCtx<'_>) -> Snapshot {
    let args = render_params(&Value::Object(conn.args.clone()), ctx);
    let manifest = std::fs::read(
        ctx.run_dir
            .join("connectors")
            .join(format!("{}.yaml", conn.name)),
    )
    .unwrap_or_default();
    let input_digest = sha256_hex(
        serde_json::json!({
            "connector": conn.name,
            "account": conn.account,
            "function": conn.function,
            "args": args,
            "manifest": sha256_hex(&manifest),
        })
        .to_string()
        .as_bytes(),
    );
    let inputs = vec![format!("{}.{}", conn.name, conn.function)];
    let (result, _) = crate::connector::call::execute_within(
        crate::connector::call::CallRequest {
            run_dir: ctx.run_dir,
            root: ctx.root,
            node_id: apb_core::baseline::BASELINE_SITE,
            connector: &conn.name,
            function: &conn.function,
            account: conn.account.as_deref(),
            args,
            dry_run: false,
            full: false,
        },
        check.timeout_seconds,
    );
    let body = match readable_body(&result) {
        Ok(b) => b,
        Err(why) => return unknown(why, input_digest, inputs, ctx),
    };
    if let Some(cw) = &check.complete_when {
        let equals = render_params(&cw.equals, ctx);
        if !apb_core::baseline::extract::holds(body, &cw.path, &equals) {
            return unknown(
                format!(
                    "the response is not complete: `{}` is not {equals}",
                    cw.path
                ),
                input_digest,
                inputs,
                ctx,
            );
        }
    }
    observed(check, body, input_digest, inputs, ctx)
}

/// The last non-empty stdout line as one JSON object.
fn last_object(stdout: &str) -> Result<Value, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or("the script printed nothing")?;
    match serde_json::from_str::<Value>(line.trim()) {
        Ok(v @ Value::Object(_)) => Ok(v),
        _ => Err("the last line the script printed is not one JSON object".into()),
    }
}

fn script(check: &BaselineCheck, path: &str, ctx: &ReadCtx<'_>) -> Snapshot {
    let inputs = vec![path.to_string()];
    let content = match std::fs::read(ctx.run_dir.join(path)) {
        Ok(c) => c,
        Err(e) => {
            return unknown(
                format!("`{path}` cannot be read: {e}"),
                String::new(),
                inputs,
                ctx,
            );
        }
    };
    let input_digest = sha256_hex(&content);
    if let Some(why) = crate::run_scripts::tampered(ctx.run_dir, ctx.events, "baseline scripts") {
        return unknown(why, input_digest, inputs, ctx);
    }
    let mut env = crate::script::run_env(ctx.run_dir, None);
    env.push(("APB_BASELINE_PHASE", ctx.phase.to_string()));
    env.extend(ctx.spawn_env.iter().map(|(k, v)| (k.as_str(), v.clone())));
    let ran = ctx.tree_guarded(|| {
        crate::script::run_script_with_env_removed(
            ctx.run_dir,
            ctx.workdir,
            path,
            "sh",
            Some(Duration::from_secs(check.timeout_seconds)),
            Some(ctx.cancel),
            &env,
            &ctx.scrub_names(),
        )
    });
    let ran = match ran {
        Ok(r) => r,
        Err(why) => return unknown(why, input_digest, inputs, ctx),
    };
    let out = match ran {
        Ok(r) if r.status == NodeStatus::Succeeded => r.stdout,
        Ok(r) if r.status == NodeStatus::Failed => {
            return unknown(
                format!("`{path}` exited non-zero"),
                input_digest,
                inputs,
                ctx,
            );
        }
        Ok(r) if r.status == NodeStatus::TimedOut => {
            return unknown(
                format!("`{path}` ran past {} s", check.timeout_seconds),
                input_digest,
                inputs,
                ctx,
            );
        }
        Ok(r) => {
            return unknown(
                format!("`{path}` ended {}", r.status.as_str()),
                input_digest,
                inputs,
                ctx,
            );
        }
        Err(e) => return unknown(e.to_string(), input_digest, inputs, ctx),
    };
    if out.len() > SCRIPT_STDOUT_CAP {
        return unknown(
            format!("`{path}` printed more than {SCRIPT_STDOUT_CAP} bytes"),
            input_digest,
            inputs,
            ctx,
        );
    }
    let doc = match last_object(&out) {
        Ok(d) => d,
        Err(why) => return unknown(why, input_digest, inputs, ctx),
    };
    if doc.get("complete") == Some(&Value::Bool(false)) {
        return unknown(
            format!("`{path}` reported its read incomplete"),
            input_digest,
            inputs,
            ctx,
        );
    }
    observed(check, &doc, input_digest, inputs, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_call_result_is_read_only_when_whole_and_ok() {
        assert!(
            readable_body(
                &json!({"ok": true, "status": 200, "body": {"n": 1}, "truncated": false})
            )
            .is_ok()
        );
        for bad in [
            json!({"ok": false, "error": {"code": "incomplete_response", "message": "cut"}}),
            json!({"ok": true, "body": {}, "truncated": true}),
            json!({"ok": true, "body": {}, "response_complete": false}),
            json!({"ok": true, "body": {}, "gap": {"from": 1, "through": 3}}),
        ] {
            assert!(readable_body(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_a_json_object_on_the_last_line_is_read() {
        assert_eq!(last_object("noise\n{\"n\": 2}\n\n").unwrap()["n"], 2);
        assert!(last_object("{\"n\": 2}\nnot json").is_err());
        assert!(last_object("[1, 2]").is_err());
        assert!(last_object("").is_err());
    }
}
