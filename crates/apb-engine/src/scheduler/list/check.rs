//! An item's check once its step succeeded: the item's declared outputs
//! must be there, then the list's one check source decides. A check that
//! cannot tell (an unreadable connector read, a script that could not run,
//! a judge set to `needs_look` without an answer) decides nothing, which the
//! state machine turns into `needs_look(check_unknown)`, never an accept.

use std::collections::BTreeMap;

use apb_core::list::verdict::ItemVerdict;
use apb_core::list::{
    CheckSource, ConnectorCheck, JudgeCheck, OnUnavailable, VERDICT_QUESTION, check_site,
    items::Item,
};
use serde_json::{Map, Value};

use super::super::journal::Journal;
use super::super::judge::{Asked, ask, state_field};
use super::run::{ListRun, output_object};
use crate::baseline::read::{
    ConnectorRead, ReadCtx, ScriptJson, call_connector_json, run_script_json,
};
use crate::decision::{Judgement, StateParts};

/// How long one connector or script item check may take.
const CHECK_TIMEOUT_SECONDS: u64 = 120;

/// What an item's check concluded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Checked {
    /// `None`: the check could not tell.
    pub verdict: Option<ItemVerdict>,
    pub note: Option<String>,
    /// The check source that decided, `None` without a check.
    pub kind: Option<&'static str>,
}

impl Checked {
    fn of(kind: &'static str, verdict: Option<ItemVerdict>, note: impl Into<String>) -> Self {
        Checked {
            verdict,
            note: Some(note.into()),
            kind: Some(kind),
        }
    }
}

/// Checks `item` whose step succeeded with `output`.
pub(super) fn run(
    run: &ListRun<'_, '_, '_>,
    item: &Item,
    output: &str,
    journal: &Journal,
) -> Checked {
    let spec = run.spec;
    let outputs = output_object(output).unwrap_or_default();
    let missing: Vec<&str> = spec
        .item_outputs
        .iter()
        .filter(|f| !outputs.contains_key(f.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Checked {
            verdict: Some(ItemVerdict::Reject),
            note: Some(format!(
                "the step did not write its item outputs: {}",
                missing.join(", ")
            )),
            kind: None,
        };
    }
    let Some(source) = spec.check.as_ref().and_then(|c| c.source()) else {
        return Checked {
            verdict: Some(ItemVerdict::Accept),
            note: None,
            kind: None,
        };
    };
    let kind = source.kind();
    match source {
        CheckSource::Output(o) => {
            let got = apb_core::schema::output_field_value(output, &o.field);
            if got.as_deref() == Some(o.equals.as_str()) {
                Checked {
                    verdict: Some(ItemVerdict::Accept),
                    note: None,
                    kind: Some(kind),
                }
            } else {
                let got = got.unwrap_or_else(|| "absent".to_string());
                Checked::of(
                    kind,
                    Some(ItemVerdict::Reject),
                    format!("`{}` is {got}, not {}", o.field, o.equals),
                )
            }
        }
        CheckSource::Script(path) => script(run, item, output, path),
        CheckSource::Connector(c) => connector(run, item, &outputs, c),
        CheckSource::Judge(j) => judge(run, item, &outputs, j, journal),
    }
}

/// Everything a check's process or call needs from the run.
fn read_ctx<'b>(run: &'b ListRun<'_, '_, '_>, events: &'b [crate::event::Event]) -> ReadCtx<'b> {
    let ctx = run.ctx;
    ReadCtx {
        run_dir: ctx.run_dir,
        root: &run.root,
        workdir: ctx.workdir,
        params: &ctx.cfg.params,
        spawn_env: &run.spawn_env,
        scrub: ctx.env_scrub,
        cancel: ctx.cancel,
        phase: "item",
        events,
        // Sibling items change the tree while this one is checked.
        guard_tree: run.workers == 1,
        scripts_label: "item check scripts",
    }
}

fn script(run: &ListRun<'_, '_, '_>, item: &Item, output: &str, path: &str) -> Checked {
    let files = match run.files(item, Some(output)) {
        Ok(f) => f,
        Err(e) => {
            return Checked::of(
                "script",
                None,
                format!("the item files cannot be written: {e}"),
            );
        }
    };
    let events = crate::event::read_all_lossy_tail(run.ctx.run_dir).unwrap_or_default();
    let mut env = match run.item_env(item, &files) {
        Ok(env) => env,
        Err(why) => return Checked::of("script", None, why),
    };
    if let Some(out) = &files.output {
        let Some(out) = out.to_str() else {
            return Checked::of(
                "script",
                None,
                "the item output file path is not valid UTF-8".to_string(),
            );
        };
        env.push(("APB_ITEM_OUTPUT_FILE", out.to_string()));
    }
    match run_script_json(path, CHECK_TIMEOUT_SECONDS, &env, &read_ctx(run, &events)) {
        ScriptJson::Doc(doc) => {
            let note = doc.get("note").and_then(Value::as_str).map(clip_note);
            match doc
                .get("verdict")
                .and_then(Value::as_str)
                .and_then(ItemVerdict::parse)
            {
                Some(v) => Checked {
                    verdict: Some(v),
                    note,
                    kind: Some("script"),
                },
                None => Checked::of(
                    "script",
                    None,
                    format!("`{path}` printed no verdict of accept, accept_with_note or reject"),
                ),
            }
        }
        ScriptJson::Exited { stderr } => Checked::of(
            "script",
            Some(ItemVerdict::Reject),
            if stderr.trim().is_empty() {
                format!("`{path}` exited non-zero")
            } else {
                clip_note(stderr.trim())
            },
        ),
        ScriptJson::Unknown(why) => Checked::of("script", None, why),
    }
}

/// The most bytes of a check script's note an item keeps.
const NOTE_MAX_BYTES: usize = 500;

/// A check script's note, clipped: it lands in every settlement, the node
/// output and the next round's briefing.
fn clip_note(note: &str) -> String {
    crate::list::prompt::clip(note, NOTE_MAX_BYTES).to_string()
}

fn connector(
    run: &ListRun<'_, '_, '_>,
    item: &Item,
    outputs: &Map<String, Value>,
    c: &ConnectorCheck,
) -> Checked {
    let args = render_value(
        &Value::Object(c.args.clone()),
        item,
        outputs,
        &run.ctx.cfg.params,
    );
    let events = crate::event::read_all_lossy_tail(run.ctx.run_dir).unwrap_or_default();
    let site = check_site(&run.node.id);
    let read = ConnectorRead {
        site: &site,
        name: &c.name,
        account: c.account.as_deref(),
        function: &c.function,
        args,
        timeout_seconds: CHECK_TIMEOUT_SECONDS,
    };
    let call = crate::connector::call::CallContext::item_check(&item.id);
    let body = match call_connector_json(read, &read_ctx(run, &events), call) {
        Ok(b) => b,
        Err(why) => return Checked::of("connector", None, why),
    };
    let w = &c.accept_when;
    let holds = match (&w.equals, w.exists) {
        (Some(equals), _) => apb_core::baseline::extract::holds(
            &body,
            &w.path,
            &render_value(equals, item, outputs, &run.ctx.cfg.params),
        ),
        (None, Some(want)) => {
            let found = apb_core::connector::common::value_at_path(&body, &w.path)
                .is_some_and(|v| !v.is_null());
            found == want
        }
        (None, None) => false,
    };
    let target = format!("{}.{}", c.name, c.function);
    if holds {
        Checked::of(
            "connector",
            Some(ItemVerdict::Accept),
            format!("{target} confirmed it"),
        )
    } else {
        Checked::of(
            "connector",
            Some(ItemVerdict::Reject),
            format!("{target} did not confirm it: `{}` does not hold", w.path),
        )
    }
}

fn judge(
    run: &ListRun<'_, '_, '_>,
    item: &Item,
    outputs: &Map<String, Value>,
    j: &JudgeCheck,
    journal: &Journal,
) -> Checked {
    let params = &run.ctx.cfg.params;
    let mut parts = StateParts::default();
    for (name, template) in j.state.iter() {
        parts.fields.push(state_field(
            name,
            render_text(template, item, outputs, params),
            true,
        ));
    }
    let wire = crate::decision::judge::node_questions(&j.questions());
    let decided = |answers: &BTreeMap<String, crate::event::DecisionAnswer>| {
        answers
            .get(VERDICT_QUESTION)
            .filter(|a| a.invalid.is_none())
            .and_then(|a| a.value.as_ref())
            .and_then(Value::as_str)
            .and_then(ItemVerdict::parse)
    };
    let judgement =
        |answers: &BTreeMap<String, crate::event::DecisionAnswer>, enforce: bool| Judgement {
            applied: enforce && decided(answers).is_some(),
            would_change: None,
        };
    let why = match ask(
        run.ctx.decisions,
        journal,
        &run.node.id,
        &parts,
        &wire,
        &judgement,
    ) {
        Asked::Answered { answers, by } => {
            let confidence = answers
                .get(VERDICT_QUESTION)
                .and_then(|a| a.confidence)
                .unwrap_or(0.0);
            match decided(&answers) {
                Some(_) if confidence < j.min_confidence.unwrap_or(0.0) => {
                    format!(
                        "the judge's confidence {confidence:.2} is below {}",
                        j.min_confidence.unwrap_or(0.0)
                    )
                }
                Some(v) => {
                    return Checked::of("judge", Some(v), format!("judged {} by {by}", v.as_str()));
                }
                None => "the judge's answer is not a verdict".to_string(),
            }
        }
        Asked::Unavailable(reason) => format!("the judge gave no usable answer ({reason})"),
    };
    match j.on_unavailable() {
        Some(OnUnavailable::Reject) => Checked::of("judge", Some(ItemVerdict::Reject), why),
        Some(OnUnavailable::NeedsLook) | None => Checked::of("judge", None, why),
        Some(OnUnavailable::AcceptWithNote) => {
            Checked::of("judge", Some(ItemVerdict::AcceptWithNote), why)
        }
    }
}

/// `text` with `{{item.<path>}}`, `{{item_output.<field>}}` and
/// `{{params.<name>}}` filled in; anything else is left as written (V87
/// refuses it). One pass over `text`: a filled-in value is never scanned
/// again, so an item holding `{{params.x}}` stays that text.
pub(crate) fn render_text(
    text: &str,
    item: &Item,
    outputs: &Map<String, Value>,
    params: &BTreeMap<String, String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open..].find("}}") else {
            break;
        };
        let cap = rest[open + 2..open + close].trim();
        out.push_str(&rest[..open]);
        let value = match cap.split_once('.') {
            Some(("item", path)) => {
                apb_core::connector::common::value_at_path(&item.value, path).map(scalar_text)
            }
            Some(("item_output", field)) => outputs.get(field).map(scalar_text),
            Some(("params", name)) => params.get(name).cloned(),
            _ => None,
        };
        match value {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &rest[open + close + 2..];
    }
    out.push_str(rest);
    out
}

/// [`render_text`] into every string of `v`.
fn render_value(
    v: &Value,
    item: &Item,
    outputs: &Map<String, Value>,
    params: &BTreeMap<String, String>,
) -> Value {
    match v {
        Value::String(s) => Value::String(render_text(s, item, outputs, params)),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| render_value(x, item, outputs, params))
                .collect(),
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), render_value(x, item, outputs, params)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_read_the_item_its_outputs_and_params() {
        let item = Item {
            id: "a".into(),
            value: serde_json::json!({"id": "a", "from": {"name": "Ann"}, "n": 3}),
            digest: String::new(),
        };
        let outputs = output_object(r#"{"task_gid":"77"}"#).unwrap();
        let params = BTreeMap::from([("folder".to_string(), "INBOX".to_string())]);
        assert_eq!(
            render_text(
                "{{item.from.name}}/{{ item.n }}/{{item_output.task_gid}}/{{params.folder}}/{{other.x}}",
                &item,
                &outputs,
                &params
            ),
            "Ann/3/77/INBOX/{{other.x}}"
        );
    }

    /// A value is filled in once: an item holding a template stays that
    /// text and never reads params or outputs.
    #[test]
    fn a_filled_in_value_is_never_expanded_again() {
        let item = Item {
            id: "a".into(),
            value: serde_json::json!({"subject": "{{params.secret}} {{item_output.x}}"}),
            digest: String::new(),
        };
        let outputs = output_object(r#"{"x":"out"}"#).unwrap();
        let params = BTreeMap::from([("secret".to_string(), "s3".to_string())]);
        assert_eq!(
            render_text(
                "{{item.subject}}|{{params.secret}}",
                &item,
                &outputs,
                &params
            ),
            "{{params.secret}} {{item_output.x}}|s3"
        );
    }
}
