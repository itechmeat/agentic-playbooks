//! The server of a call. A stdio server (design D4 "The server's
//! environment"): the pinned program file, its rendered arguments and an
//! environment from a fixed source, never from the calling process; its
//! working directory is a fresh empty private directory the client makes
//! and removes. An http server: the endpoint and the token URL rendered
//! from the account's non-secret fields and checked (`http_target`) before
//! any secret is resolved.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::config::AccountEntry;
use apb_core::connector::config;
use apb_core::connector::def::{AuthSpec, ConnectorDoc};
use apb_core::connector::mcp::{McpServerSpec, McpTransportKind};
use apb_core::connector::template::{Namespace, RenderCtx, placeholders, render_raw};
use apb_core::secret::Secret;
use apb_mcp_client::{EnvValue, HttpServer, ProgramFile, StdioServer};
use serde_json::Value;

use crate::connector::call::render::validate_url;
use crate::connector::result::{CallError, CallErrorCode};

/// What the server would be started with, rendered from the definition
/// and the account's non-secret fields only, for a person's confirmation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartPreview {
    /// Each `args` entry, rendered.
    pub args: Vec<String>,
    /// Each declared `env` override, its secret parts shown as
    /// `[secret <field> from <source>]`.
    pub env: Vec<(String, String)>,
    /// Every non-secret field of the account.
    pub fields: Vec<(String, String)>,
    /// Each command-sourced secret field with the command line its
    /// resolution will run, extracted and not run.
    pub secret_commands: Vec<(String, String)>,
    /// What an http server would be reached with; `None` for stdio.
    pub http: Option<HttpPreview>,
}

/// What an http server would be reached with, for a person's
/// confirmation: rendered from the definition and the account's
/// non-secret fields, secret parts masked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpPreview {
    /// The rendered endpoint.
    pub endpoint: String,
    /// Its endpoint digest (`digest::endpoint_digest`).
    pub endpoint_digest: String,
    /// The auth kind: `header`, `oauth2_refresh` or `none`.
    pub auth: String,
    /// The rendered token endpoint (`oauth2_refresh`).
    pub token_url: Option<String>,
    /// The rendered client id (`oauth2_refresh`).
    pub client_id: Option<String>,
    /// The header name and its template with secret parts masked
    /// (`header`).
    pub header: Option<(String, String)>,
}

/// The checked target of an http server: the rendered endpoint, the
/// rendered token URL (`oauth2_refresh`) and their endpoint digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpTarget {
    pub(crate) endpoint: String,
    pub(crate) token_url: Option<String>,
    pub(crate) digest: String,
}

/// What the server would be started with, rendered from the definition
/// and the account's non-secret fields only: each `args` entry, and each
/// `env` entry with every `{{secret.<field>}}` replaced by
/// `[secret <field> from <source>]`. Resolves no secret and runs no
/// helper. Also carries `config::cmd_refs(doc, account)`: each
/// command-sourced secret field with the command line its resolution will
/// run, extracted without executing it. Takes the whole definition, since
/// which fields are secret is the definition's `account_fields`.
pub fn preview_start(
    doc: &ConnectorDoc,
    account: &config::Account,
) -> Result<StartPreview, CallError> {
    let Some(spec) = doc.mcp.as_ref() else {
        return Ok(StartPreview::default());
    };
    if spec.transport == McpTransportKind::Http {
        return preview_http(doc, account);
    }
    let secret_fields = doc.secret_fields();
    let env_refs = config::env_refs(doc, account);
    let cmd_refs = config::cmd_refs(doc, account);
    let fields: BTreeMap<String, String> = account
        .fields
        .iter()
        .filter(|(k, _)| !secret_fields.contains(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let masked: BTreeMap<String, Secret> = secret_fields
        .iter()
        .map(|f| {
            let source = match (env_refs.get(f), cmd_refs.contains_key(f)) {
                (Some(var), _) => format!("env {var}"),
                (None, true) => "cmd".to_string(),
                (None, false) => "account".to_string(),
            };
            (
                f.clone(),
                Secret::new(format!("[secret {f} from {source}]")),
            )
        })
        .collect();
    let ctx = RenderCtx {
        account: &fields,
        args: &Value::Null,
        secrets: &masked,
    };
    let env = spec
        .env
        .iter()
        .map(|(name, template)| {
            render_raw(template, &ctx)
                .map(|v| (name.clone(), v))
                .map_err(|e| config_error(name, &e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StartPreview {
        args: render_args(spec, &fields)?,
        env,
        fields: fields.into_iter().collect(),
        secret_commands: cmd_refs.into_iter().collect(),
        http: None,
    })
}

/// The account's non-secret fields and its secret fields masked as
/// `[secret <field> from <source>]`.
fn masked_fields(
    doc: &ConnectorDoc,
    account: &config::Account,
) -> (BTreeMap<String, String>, BTreeMap<String, Secret>) {
    let secret_fields = doc.secret_fields();
    let env_refs = config::env_refs(doc, account);
    let cmd_refs = config::cmd_refs(doc, account);
    let fields = account
        .fields
        .iter()
        .filter(|(k, _)| !secret_fields.contains(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let masked = secret_fields
        .iter()
        .map(|f| {
            let source = match (env_refs.get(f), cmd_refs.contains_key(f)) {
                (Some(var), _) => format!("env {var}"),
                (None, true) => "cmd".to_string(),
                (None, false) => "account".to_string(),
            };
            (
                f.clone(),
                Secret::new(format!("[secret {f} from {source}]")),
            )
        })
        .collect();
    (fields, masked)
}

/// The http form of `preview_start`, over `http_target`: no secret is
/// resolved and no helper runs.
fn preview_http(doc: &ConnectorDoc, account: &config::Account) -> Result<StartPreview, CallError> {
    let (fields, masked) = masked_fields(doc, account);
    let target = http_target(doc, &fields)?;
    let ctx = RenderCtx {
        account: &fields,
        args: &Value::Null,
        secrets: &masked,
    };
    let (auth, client_id, header) = match doc.auth.as_ref() {
        None => ("none", None, None),
        Some(AuthSpec::Oauth2Refresh {
            client_id_template, ..
        }) => (
            "oauth2_refresh",
            Some(
                render_raw(client_id_template, &ctx)
                    .map_err(|e| config_error("client_id_template", &e.to_string()))?,
            ),
            None,
        ),
        Some(AuthSpec::Header {
            header,
            value_template,
        }) => (
            "header",
            None,
            Some((
                header.clone(),
                render_raw(value_template, &ctx)
                    .map_err(|e| config_error("value_template", &e.to_string()))?,
            )),
        ),
        // The manifest refuses every other kind beside an http block.
        Some(_) => {
            return Err(config_error(
                "auth",
                "this auth kind is not an http MCP kind",
            ));
        }
    };
    Ok(StartPreview {
        args: Vec::new(),
        env: Vec::new(),
        fields: fields.into_iter().collect(),
        secret_commands: config::cmd_refs(doc, account).into_iter().collect(),
        http: Some(HttpPreview {
            endpoint: target.endpoint,
            endpoint_digest: target.digest,
            auth: auth.to_string(),
            token_url: target.token_url,
            client_id,
            header,
        }),
    })
}

/// Whether a rendered URL holds a control character or whitespace: an
/// account field can hold one, and the manifest checks only the template.
fn has_control_or_space(url: &str) -> bool {
    url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Whether a checked absolute URL names a host: the authority between the
/// scheme and the first `/`, `?` or `#`, without its port, is not empty.
/// An account field can render `https:///mcp`, which no request reaches.
fn names_a_host(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    };
    !host.is_empty()
}

/// The endpoint of an http server: `url` rendered with the account's
/// non-secret fields (no secret), then checked: no control character or
/// whitespace, an absolute URL without userinfo, no query and no fragment,
/// and `mcp::url_scheme_ok`. Every refusal is `config`.
pub(crate) fn resolve_endpoint(
    spec: &McpServerSpec,
    fields: &BTreeMap<String, String>,
) -> Result<String, CallError> {
    let Some(template) = spec.url.as_deref() else {
        return Err(config_error("url", "mcp transport `http` requires `url`"));
    };
    let none = BTreeMap::new();
    let ctx = RenderCtx {
        account: fields,
        args: &Value::Null,
        secrets: &none,
    };
    let url = render_raw(template, &ctx).map_err(|e| config_error("url", &e.to_string()))?;
    if has_control_or_space(&url) {
        return Err(refused("mcp url holds a control character or whitespace"));
    }
    validate_url(&url)?;
    if !names_a_host(&url) {
        return Err(refused("mcp url names no host"));
    }
    if url.contains(['?', '#']) {
        return Err(refused("mcp url must not carry a query or a fragment"));
    }
    if !apb_core::connector::mcp::url_scheme_ok(&url) {
        return Err(refused("mcp url must use https"));
    }
    Ok(url)
}

/// The checked target of an http server, before any secret: the endpoint
/// (`resolve_endpoint`), for `oauth2_refresh` the token URL rendered the
/// same way and checked (no control character or whitespace, no userinfo,
/// `mcp::url_scheme_ok`), and their endpoint digest. Reads no secret, runs
/// no helper, sends nothing.
pub(crate) fn http_target(
    doc: &ConnectorDoc,
    fields: &BTreeMap<String, String>,
) -> Result<HttpTarget, CallError> {
    let Some(spec) = doc
        .mcp
        .as_ref()
        .filter(|s| s.transport == McpTransportKind::Http)
    else {
        return Err(refused("the connector has no http `mcp:` block"));
    };
    let endpoint = resolve_endpoint(spec, fields)?;
    let token_url = match doc.auth.as_ref() {
        Some(AuthSpec::Oauth2Refresh { token_url, .. }) => {
            let none = BTreeMap::new();
            let ctx = RenderCtx {
                account: fields,
                args: &Value::Null,
                secrets: &none,
            };
            let url = render_raw(token_url, &ctx)
                .map_err(|e| config_error("token_url", &e.to_string()))?;
            if has_control_or_space(&url) {
                return Err(refused(
                    "the token_url of an MCP connector holds a control character or whitespace",
                ));
            }
            validate_url(&url)?;
            if !names_a_host(&url) {
                return Err(refused("the token_url of an MCP connector names no host"));
            }
            if !apb_core::connector::mcp::url_scheme_ok(&url) {
                return Err(refused("the token_url of an MCP connector must use https"));
            }
            Some(url)
        }
        _ => None,
    };
    let digest = apb_core::connector::mcp::digest::endpoint_digest(&endpoint, token_url.as_deref());
    Ok(HttpTarget {
        endpoint,
        token_url,
        digest,
    })
}

/// The `HttpServer` of a checked endpoint, its headers and its bearer.
pub(crate) fn http_server(
    endpoint: &str,
    headers: Vec<(String, EnvValue)>,
    bearer: Option<Secret>,
) -> HttpServer {
    HttpServer {
        url: endpoint.to_string(),
        headers,
        bearer,
    }
}

fn refused(why: &str) -> CallError {
    CallError::new(CallErrorCode::Config, why.to_string())
}

/// The `StdioServer` of a pinned program file, a spec and an account: the
/// program, the `args` rendered with the account's fields, and the fixed
/// environment of design D4 with the declared `env` on top. Nothing of the
/// calling process passes.
pub(crate) fn stdio_server(
    program: ProgramFile,
    spec: &McpServerSpec,
    account_fields: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, Secret>,
    home: &AccountEntry,
) -> Result<StdioServer, CallError> {
    let args = render_args(spec, account_fields)?;
    let mut env = fixed_env(program.path(), home);
    let ctx = RenderCtx {
        account: account_fields,
        args: &Value::Null,
        secrets,
    };
    for (name, template) in &spec.env {
        let holds_secret = placeholders(template)
            .map_err(|e| config_error(name, &e.to_string()))?
            .iter()
            .any(|(ns, _)| *ns == Namespace::Secret);
        let value = render_raw(template, &ctx).map_err(|e| config_error(name, &e.to_string()))?;
        let value = if holds_secret {
            EnvValue::Secret(Secret::new(value))
        } else {
            EnvValue::Plain(value)
        };
        // A declared name overrides a fixed one.
        env.retain(|(n, _)| n != name);
        env.push((name.clone(), value));
    }
    Ok(StdioServer { program, args, env })
}

/// The system directories of the server's `PATH`, in order (macOS adds
/// `/opt/homebrew/bin` after them).
const SYSTEM_PATH: [&str; 3] = ["/usr/local/bin", "/usr/bin", "/bin"];

/// The server's `PATH`: the system directories first, then on macOS
/// `/opt/homebrew/bin`, then the program's own directory last (when it is
/// not one of them), so a name the server looks up never resolves first
/// in the program's directory. A directory that cannot be one `PATH`
/// entry (it holds a `:` or is not UTF-8) is left out rather than split
/// into entries nobody approved.
pub(crate) fn server_path(program: &Path) -> String {
    let mut dirs: Vec<String> = SYSTEM_PATH.iter().map(|d| d.to_string()).collect();
    if cfg!(target_os = "macos") {
        dirs.push("/opt/homebrew/bin".to_string());
    }
    if let Some(dir) = program.parent().and_then(Path::to_str).map(str::to_string)
        && !dir.is_empty()
        && !dir.contains(':')
        && !dirs.contains(&dir)
    {
        dirs.push(dir);
    }
    dirs.join(":")
}

/// The fixed rows of the server's environment: the account database's
/// home and name, the XDG base directories under that home, and the
/// `PATH` of `server_path`.
fn fixed_env(program: &Path, home: &AccountEntry) -> Vec<(String, EnvValue)> {
    let h = home.home.display().to_string();
    let plain = |name: &str, value: String| (name.to_string(), EnvValue::Plain(value));
    vec![
        plain("HOME", h.clone()),
        plain("USER", home.name.clone()),
        plain("LOGNAME", home.name.clone()),
        plain("XDG_CONFIG_HOME", format!("{h}/.config")),
        plain("XDG_DATA_HOME", format!("{h}/.local/share")),
        plain("XDG_STATE_HOME", format!("{h}/.local/state")),
        plain("XDG_CACHE_HOME", format!("{h}/.cache")),
        plain("PATH", server_path(program)),
    ]
}

/// Each `args` entry rendered with the account's non-secret fields (an
/// `args` template takes `{{account.*}}` only).
pub(crate) fn render_args(
    spec: &McpServerSpec,
    account_fields: &BTreeMap<String, String>,
) -> Result<Vec<String>, CallError> {
    let none = BTreeMap::new();
    let ctx = RenderCtx {
        account: account_fields,
        args: &Value::Null,
        secrets: &none,
    };
    spec.args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            render_raw(a, &ctx).map_err(|e| config_error(&format!("args[{i}]"), &e.to_string()))
        })
        .collect()
}

fn config_error(field: &str, why: &str) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!("mcp `{field}` could not be rendered: {why}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_doc(url: &str, auth: &str) -> ConnectorDoc {
        let yaml = format!(
            "name: crm-mcp\nversion: 0.1.0\nmcp:\n  transport: http\n  url: \"{url}\"\n  protocol: modern\n{auth}account_fields:\n  - {{ name: host, required: true }}\n  - {{ name: port }}\n  - {{ name: token_host }}\n  - {{ name: client_id }}\n  - {{ name: token, required: true, secret: true }}\nfunctions:\n  - name: search\n    description: Search\n    read_only: true\n    mcp: {{ tool: search }}\n"
        );
        ConnectorDoc::from_yaml(&yaml, "crm-mcp").expect("the http connector loads")
    }

    const HEADER: &str =
        "auth:\n  kind: header\n  header: X-Api-Key\n  value_template: \"Key {{secret.token}}\"\n";
    const OAUTH: &str = "auth:\n  kind: oauth2_refresh\n  token_url: \"https://{{account.token_host}}/token\"\n  client_id_template: \"{{account.client_id}}\"\n  refresh_token_template: \"{{secret.token}}\"\n";

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn http_target_refuses_plaintext_and_a_query() {
        // A rendered `http://localhost:<port>/mcp`: the manifest refuses a
        // plaintext template, so the spec skips the loader here.
        let spec = McpServerSpec {
            url: Some("http://localhost:{{account.port}}/mcp".into()),
            ..http_doc("https://{{account.host}}/mcp", "").mcp.unwrap()
        };
        let err = resolve_endpoint(&spec, &fields(&[("port", "8080")])).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
        assert!(err.message.contains("must use https"), "{}", err.message);
        let doc = http_doc("https://{{account.host}}/mcp", HEADER);
        let err = http_target(&doc, &fields(&[("host", "crm.example/mcp?x=1#")])).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
        assert!(err.message.contains("query"), "{}", err.message);
        let oauth = http_doc(
            "https://{{account.host}}/mcp",
            &OAUTH.replace(
                "https://{{account.token_host}}",
                "http://localhost:{{account.port}}",
            ),
        );
        let err = http_target(
            &oauth,
            &fields(&[
                ("host", "crm.example"),
                ("port", "8080"),
                ("client_id", "c"),
            ]),
        )
        .unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
        assert!(err.message.contains("must use https"), "{}", err.message);
    }

    /// A rendered endpoint or token URL with an empty host is refused
    /// before any secret, with and without a port.
    #[test]
    fn http_target_refuses_an_empty_host() {
        let doc = http_doc("https://{{account.host}}/mcp", HEADER);
        for host in ["", ":8443"] {
            let err = http_target(&doc, &fields(&[("host", host)])).unwrap_err();
            assert_eq!(err.code, CallErrorCode::Config);
            assert!(err.message.contains("names no host"), "{}", err.message);
        }
        let oauth = http_doc("https://{{account.host}}/mcp", OAUTH);
        let err = http_target(
            &oauth,
            &fields(&[
                ("host", "crm.example"),
                ("token_host", ""),
                ("client_id", "c"),
            ]),
        )
        .unwrap_err();
        assert!(err.message.contains("token_url"), "{}", err.message);
        assert!(err.message.contains("names no host"), "{}", err.message);
    }

    #[test]
    fn resolve_endpoint_renders_account_fields() {
        let doc = http_doc("https://{{account.host}}/v1/mcp", HEADER);
        let spec = doc.mcp.as_ref().unwrap();
        assert_eq!(
            resolve_endpoint(spec, &fields(&[("host", "crm.example:8443")])).unwrap(),
            "https://crm.example:8443/v1/mcp"
        );
        let err = resolve_endpoint(spec, &fields(&[("host", "u:p@crm.example")])).unwrap_err();
        assert!(err.message.contains("userinfo"), "{}", err.message);
    }

    #[test]
    fn preview_start_masks_the_header_secret() {
        let doc = http_doc("https://{{account.host}}/mcp", HEADER);
        let account = config::Account {
            name: "work".into(),
            default: true,
            fields: fields(&[("host", "crm.example"), ("token", "{{cmd:pass show crm}}")]),
        };
        let preview = preview_start(&doc, &account).unwrap();
        let http = preview.http.expect("the http preview");
        assert_eq!(http.endpoint, "https://crm.example/mcp");
        assert_eq!(http.auth, "header");
        assert_eq!(
            http.header,
            Some(("X-Api-Key".into(), "Key [secret token from cmd]".into()))
        );
        assert_eq!(
            preview.secret_commands,
            vec![("token".to_string(), "pass show crm".to_string())]
        );
        assert!(preview.args.is_empty() && preview.env.is_empty());
        let oauth = http_doc("https://{{account.host}}/mcp", OAUTH);
        let account = config::Account {
            name: "work".into(),
            default: true,
            fields: fields(&[
                ("host", "crm.example"),
                ("token_host", "auth.example"),
                ("client_id", "client-7"),
                ("token", "{{env.CRM_REFRESH}}"),
            ]),
        };
        let http = preview_start(&oauth, &account).unwrap().http.unwrap();
        assert_eq!(http.auth, "oauth2_refresh");
        assert_eq!(
            http.token_url.as_deref(),
            Some("https://auth.example/token")
        );
        assert_eq!(http.client_id.as_deref(), Some("client-7"));
        assert_eq!(http.header, None);
    }

    #[test]
    fn http_target_digest_is_the_endpoint_digest() {
        let doc = http_doc("https://{{account.host}}/mcp", OAUTH);
        let target = http_target(
            &doc,
            &fields(&[
                ("host", "crm.example"),
                ("token_host", "auth.example"),
                ("client_id", "c"),
            ]),
        )
        .unwrap();
        assert_eq!(target.endpoint, "https://crm.example/mcp");
        assert_eq!(
            target.token_url.as_deref(),
            Some("https://auth.example/token")
        );
        assert_eq!(
            target.digest,
            apb_core::connector::mcp::digest::endpoint_digest(
                "https://crm.example/mcp",
                Some("https://auth.example/token")
            )
        );
        let header = http_doc("https://{{account.host}}/mcp", HEADER);
        let target = http_target(&header, &fields(&[("host", "crm.example")])).unwrap();
        assert_eq!(target.token_url, None);
        assert_eq!(
            target.digest,
            apb_core::connector::mcp::digest::endpoint_digest("https://crm.example/mcp", None)
        );
    }

    #[test]
    fn http_target_refuses_whitespace_in_a_rendered_url() {
        let doc = http_doc("https://{{account.host}}/mcp", OAUTH);
        for host in ["crm.example /x", "crm.example\n", "crm\u{7}.example"] {
            let err = http_target(
                &doc,
                &fields(&[
                    ("host", host),
                    ("token_host", "auth.example"),
                    ("client_id", "c"),
                ]),
            )
            .unwrap_err();
            assert!(
                err.message.contains("control character or whitespace"),
                "{host:?}"
            );
        }
        let err = http_target(
            &doc,
            &fields(&[
                ("host", "crm.example"),
                ("token_host", "auth .example"),
                ("client_id", "c"),
            ]),
        )
        .unwrap_err();
        assert!(err.message.contains("token_url"), "{}", err.message);
    }

    #[test]
    fn the_path_puts_the_programs_own_directory_last() {
        let path = server_path(Path::new("/home/u/project/bin/server"));
        let dirs: Vec<&str> = path.split(':').collect();
        assert_eq!(&dirs[..3], &SYSTEM_PATH[..]);
        assert_eq!(dirs.last(), Some(&"/home/u/project/bin"));
        assert_eq!(
            dirs.contains(&"/opt/homebrew/bin"),
            cfg!(target_os = "macos")
        );
        let system = server_path(Path::new("/usr/bin/server"));
        assert_eq!(
            system.split(':').filter(|d| *d == "/usr/bin").count(),
            1,
            "no directory twice: {system}"
        );
    }

    #[test]
    fn a_program_directory_with_a_colon_is_left_out_of_the_path() {
        let path = server_path(Path::new("/opt/vendor:tools/server"));
        let system = server_path(Path::new("/usr/bin/server"));
        assert_eq!(path, system, "only the system directories: {path}");
        assert!(
            !path.contains("vendor") && !path.contains("tools"),
            "{path}"
        );
    }
}
