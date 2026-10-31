//! The base variable set every agent process keeps under `allowlist`. Each
//! agent's own set lives in its module (`crate::agents`, `HostSetup::env_set`),
//! checked against the variable names its installed CLI reads.

/// What every agent process keeps: the login basics, the locale (the POSIX
/// category names one by one, never an `LC_` prefix that would also pass an
/// `LC_DEPLOY_TOKEN`), temp dirs, XDG
/// dirs, proxies, certificate bundles and `APB_CONFIG_DIR` (a nested
/// `apb connector call` or `apb __ask-server` must resolve the same config,
/// trust store and global secrets as the run).
pub fn base_set() -> &'static [&'static str] {
    &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_CTYPE",
        "LC_COLLATE",
        "LC_MESSAGES",
        "LC_MONETARY",
        "LC_NUMERIC",
        "LC_TIME",
        "LC_ADDRESS",
        "LC_IDENTIFICATION",
        "LC_MEASUREMENT",
        "LC_NAME",
        "LC_PAPER",
        "LC_TELEPHONE",
        "TZ",
        "TERM",
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "all_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "APB_CONFIG_DIR",
        "SYSTEMROOT",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
        "COMSPEC",
    ]
}

pub use crate::agents::AgentEnvSet;
