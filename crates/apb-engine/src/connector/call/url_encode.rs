//! URL string helpers (ADP leaf): assembling a pre-auth URL with its query,
//! percent-encoding one component, and pre-encoding the args substituted
//! into a URL template (spec 6.1). No imports from the call tree: render.rs
//! and encode.rs use it, never the reverse.
use serde_json::Value;

pub(crate) fn encode_args_for_url(args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => Value::String(encode_component(s)),
                        other => other.clone(),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn encode_component(s: &str) -> String {
    apb_core::url_component::encode(s)
}

pub(crate) fn assemble_url(base: &str, query: &str) -> String {
    if query.is_empty() {
        return base.to_string();
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}{query}")
}
