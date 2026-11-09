//! A provider's catalog as this machine has it: the cache a refresh wrote
//! (`<config_dir>/state/cloud-models/<id>.json`, the response body and its
//! fetch time), else the snapshot embedded in the binary. [`refresh`] is the
//! only code in apb that fetches a listing; [`load`] never touches the
//! network.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::*;

/// The cap on a listing body.
pub const MAX_BODY: u64 = 4 * 1024 * 1024;
/// The time a refresh may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The finding code of a refresh that failed.
pub const REFRESH_FAILED: &str = "cloud_catalog_refresh_failed";

/// Where a snapshot came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    Embedded,
    Refreshed,
}

/// One provider's catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogSnapshot {
    pub provider: String,
    pub source: CatalogSource,
    /// RFC 3339; the asset's date for the embedded snapshot.
    pub fetched_at: String,
    /// `sha256:<hex>` of the body bytes.
    pub digest: String,
    /// Rows the parser refused.
    pub dropped: u32,
    pub models: Vec<CloudModel>,
    /// Why a cache file was not used, when one exists and did not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_problem: Option<String>,
}

/// The cache file: the body as the provider sent it and its fetch time.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    fetched_at: String,
    body: serde_json::Value,
}

/// The cache file of `provider`.
pub fn cache_path(provider: &CloudProvider) -> Option<PathBuf> {
    Some(
        crate::config::config_dir()?
            .join("state")
            .join("cloud-models")
            .join(format!("{}.json", provider.id)),
    )
}

fn snapshot(
    provider: &CloudProvider,
    source: CatalogSource,
    fetched_at: &str,
    body: &[u8],
) -> Result<CatalogSnapshot, String> {
    let (models, dropped) = (provider.parse)(body)?;
    Ok(CatalogSnapshot {
        provider: provider.id.to_string(),
        source,
        fetched_at: fetched_at.to_string(),
        digest: crate::content::sha256_hex(body),
        dropped,
        models,
        cache_problem: None,
    })
}

/// The embedded snapshot of `provider`, parsed once per process.
pub fn embedded(provider: &CloudProvider) -> CatalogSnapshot {
    static CACHE: OnceLock<std::sync::Mutex<Vec<CatalogSnapshot>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut held = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = held.iter().find(|s| s.provider == provider.id) {
        return s.clone();
    }
    let snap = snapshot(
        provider,
        CatalogSource::Embedded,
        provider.embedded_fetched_at,
        provider.embedded.as_bytes(),
    )
    .unwrap_or_else(|_| CatalogSnapshot {
        provider: provider.id.to_string(),
        source: CatalogSource::Embedded,
        fetched_at: provider.embedded_fetched_at.to_string(),
        digest: crate::content::sha256_hex(provider.embedded.as_bytes()),
        dropped: 0,
        models: Vec::new(),
        cache_problem: None,
    });
    held.push(snap.clone());
    snap
}

/// Reads the cache; `Ok(None)` when there is none.
fn read_cache(provider: &CloudProvider) -> Result<Option<CatalogSnapshot>, String> {
    let Some(path) = cache_path(provider) else {
        return Ok(None);
    };
    let text = match crate::fsutil::read_settings_file(&path, MAX_BODY * 2) {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("the catalog cache does not read ({e})")),
    };
    let file: CacheFile = serde_json::from_str(&text)
        .map_err(|_| "the catalog cache is not a cache file apb wrote".to_string())?;
    if crate::duration::parse_rfc3339_ms(&file.fetched_at).is_none() {
        return Err("the catalog cache has no valid fetch time".to_string());
    }
    let body = serde_json::to_vec(&file.body).map_err(|e| e.to_string())?;
    snapshot(provider, CatalogSource::Refreshed, &file.fetched_at, &body)
        .map(Some)
        .map_err(|e| format!("the catalog cache does not parse ({e})"))
}

/// The catalog of `provider`: the cache when it reads and parses, else the
/// embedded snapshot (with the cache's problem noted for the doctor).
pub fn load(provider: &CloudProvider) -> CatalogSnapshot {
    match read_cache(provider) {
        Ok(Some(s)) => s,
        Ok(None) => embedded(provider),
        Err(problem) => CatalogSnapshot {
            cache_problem: Some(problem),
            ..embedded(provider)
        },
    }
}

/// Every provider's catalog.
pub fn load_all() -> Vec<CatalogSnapshot> {
    all().iter().map(|p| load(p)).collect()
}

/// The row `model_id` names in `snap`.
pub fn find<'a>(model_id: &str, snap: &'a CatalogSnapshot) -> Option<&'a CloudModel> {
    snap.models.iter().find(|m| m.id == model_id)
}

/// A refresh that did not replace the cache: the HTTP status or the
/// parser's sentence, never the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshError {
    pub provider: String,
    pub reason: String,
}

impl RefreshError {
    pub fn code(&self) -> &'static str {
        REFRESH_FAILED
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{REFRESH_FAILED}: the {} catalog was not refreshed: {}",
            self.provider, self.reason
        )
    }
}

impl std::error::Error for RefreshError {}

/// The listing URL: the provider's, or a test stub's.
fn models_url(provider: &CloudProvider) -> String {
    #[cfg(any(test, feature = "testing"))]
    if let Ok(u) = std::env::var("APB_TEST_CLOUD_CATALOG_URL")
        && !u.is_empty()
    {
        return u;
    }
    provider.models_url.to_string()
}

fn fetch(provider: &CloudProvider, url: &str) -> Result<Vec<u8>, String> {
    // HTTPS only, outside apb's own tests.
    #[cfg(not(any(test, feature = "testing")))]
    if !url.starts_with("https://") {
        return Err("the listing URL is not HTTPS".to_string());
    }
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    // No credential ever: the listing is public, and the key is never read
    // here.
    let response = agent
        .get(url)
        .header("accept", "application/json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Timeout(_) => "the request timed out".to_string(),
            ureq::Error::HostNotFound => "host not found".to_string(),
            ureq::Error::ConnectionFailed => "connection failed".to_string(),
            _ => "transport error".to_string(),
        })?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("{} answered HTTP {status}", provider.display));
    }
    response
        .into_body()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                format!("the listing is larger than {} bytes", MAX_BODY)
            }
            _ => "the listing body did not read".to_string(),
        })
}

/// Fetches `provider`'s listing and replaces the cache with it. Refused in
/// a read-only process before any request; a failure leaves the cache as it
/// was.
pub fn refresh(provider: &CloudProvider) -> Result<CatalogSnapshot, RefreshError> {
    let fail = |reason: String| RefreshError {
        provider: provider.id.to_string(),
        reason,
    };
    crate::process_mode::guard().map_err(|e| fail(e.to_string()))?;
    let path = cache_path(provider).ok_or_else(|| fail("no apb config directory".into()))?;
    let body = fetch(provider, &models_url(provider)).map_err(fail)?;
    let fetched_at = crate::clock::iso_utc(crate::clock::now_ms_u64());
    let snap = snapshot(provider, CatalogSource::Refreshed, &fetched_at, &body).map_err(fail)?;
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| fail(e.to_string()))?;
    let file = CacheFile {
        fetched_at: fetched_at.clone(),
        body: value,
    };
    let bytes = serde_json::to_vec(&file).map_err(|e| fail(e.to_string()))?;
    crate::fsutil::atomic_write_private(&path, &bytes).map_err(|e| fail(e.to_string()))?;
    // The digest a later `load` computes: over the bytes it reads back.
    Ok(read_cache(provider).ok().flatten().unwrap_or(snap))
}

/// Refreshes every provider.
pub fn refresh_all() -> Vec<Result<CatalogSnapshot, RefreshError>> {
    all().iter().map(|p| refresh(p)).collect()
}

/// Days since the snapshot was fetched, when its date reads.
pub fn age_days(snap: &CatalogSnapshot) -> Option<u64> {
    let at = crate::duration::parse_rfc3339_ms(&snap.fetched_at)?;
    Some(crate::clock::now_ms_u64().saturating_sub(at) / 86_400_000)
}

/// The age past which the doctor calls a catalog stale.
pub const STALE_DAYS: u64 = 30;

/// The doctor's line of `snap`: `cloud_catalog_stale` past [`STALE_DAYS`],
/// a cache that did not read, and whether the key variable is set (by name;
/// the value is never read).
/// `(warn, name, detail)`: `warn` is true for a stale catalog or a cache
/// that did not read.
pub fn doctor_line(snap: &CatalogSnapshot, key_present: bool) -> (bool, String, String) {
    let p = lookup(&snap.provider);
    let display = p.map_or(snap.provider.as_str(), |p| p.display);
    let key_env = p.map_or("", |p| p.key_env);
    let source = match snap.source {
        CatalogSource::Embedded => "embedded",
        CatalogSource::Refreshed => "refreshed",
    };
    let age = age_days(snap);
    let mut warn = false;
    let mut detail = format!(
        "{source} catalog fetched {}{}, {} models; {key_env} {}",
        date(&snap.fetched_at),
        age.map_or_else(String::new, |d| format!(" ({d} days old)")),
        snap.models.len(),
        if key_present { "is set" } else { "is not set" },
    );
    if age.is_some_and(|d| d > STALE_DAYS) {
        warn = true;
        detail.push_str(&format!(
            "; cloud_catalog_stale: older than {STALE_DAYS} days, run `apb models --refresh`"
        ));
    }
    if let Some(problem) = &snap.cache_problem {
        warn = true;
        detail.push_str(&format!("; {problem}, so the embedded catalog is used"));
    }
    (warn, format!("cloud models {display}"), detail)
}

/// The doctor's line of every provider.
pub fn doctor_lines() -> Vec<(bool, String, String)> {
    load_all()
        .iter()
        .map(|s| {
            let present = lookup(&s.provider)
                .is_some_and(|p| std::env::var_os(p.key_env).is_some_and(|v| !v.is_empty()));
            doctor_line(s, present)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::near_ai::NEAR_AI;
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A one-shot stub on the loopback: answers `status` with `body` and
    /// sends the request head it read.
    fn stub(status: u16, body: Vec<u8>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/models", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&buf[..n]);
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        });
        (url, rx)
    }

    fn good_body() -> Vec<u8> {
        serde_json::json!({"data": [
            {"id": "Qwen/Qwen3.8-27B", "owned_by": "nearai", "input_modalities": ["text"],
             "output_modalities": ["text"], "supported_features": ["tools"],
             "pricing": {"input": 0.05, "output": 1.5}}
        ]})
        .to_string()
        .into_bytes()
    }

    struct Env {
        _cfg: crate::TestConfig,
    }

    fn env(url: &str) -> Env {
        let cfg = crate::test_config();
        // SAFETY: env mutation under the test config lock.
        unsafe {
            std::env::set_var("APB_TEST_CLOUD_CATALOG_URL", url);
            std::env::set_var("NEARAI_API_KEY", "fake-key-123");
        }
        Env { _cfg: cfg }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            // SAFETY: still under the test config lock.
            unsafe {
                std::env::remove_var("APB_TEST_CLOUD_CATALOG_URL");
                std::env::remove_var("NEARAI_API_KEY");
            }
        }
    }

    #[test]
    fn no_cache_gives_the_embedded_snapshot() {
        let _e = env("http://127.0.0.1:9/none");
        let s = load(&NEAR_AI);
        assert_eq!(s.source, CatalogSource::Embedded);
        assert_eq!(s.fetched_at, NEAR_AI.embedded_fetched_at);
        assert!(s.cache_problem.is_none());
        assert!(find("Qwen/Qwen3.8-27B", &s).is_some());
    }

    #[test]
    fn a_good_refresh_writes_a_private_cache_with_no_credential_sent() {
        let (url, rx) = stub(200, good_body());
        let _e = env(&url);
        let s = refresh(&NEAR_AI).unwrap();
        let head = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "{head}"
        );
        assert!(!head.contains("fake-key-123"));
        assert_eq!(s.source, CatalogSource::Refreshed);
        let path = cache_path(&NEAR_AI).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("fake-key-123")
        );
        let loaded = load(&NEAR_AI);
        assert_eq!(loaded.source, CatalogSource::Refreshed);
        assert_eq!(loaded.digest, s.digest);
        assert_eq!(
            find("Qwen/Qwen3.8-27B", &loaded).unwrap().price_in_usd_mtok,
            Some(0.05)
        );
    }

    #[test]
    fn a_failed_refresh_leaves_the_cache_byte_for_byte() {
        let (url, _rx) = stub(200, good_body());
        let _e = env(&url);
        refresh(&NEAR_AI).unwrap();
        let path = cache_path(&NEAR_AI).unwrap();
        let before = std::fs::read(&path).unwrap();
        let big = {
            let mut b = b"{\"data\": [], \"pad\": \"".to_vec();
            b.extend(std::iter::repeat_n(b'x', (MAX_BODY + 10) as usize));
            b.extend_from_slice(b"\"}");
            b
        };
        for (status, body) in [
            (500, b"{}".to_vec()),
            (200, big),
            (200, b"not json at all".to_vec()),
        ] {
            let (url, _rx) = stub(status, body);
            // SAFETY: under the test config lock `_e` holds.
            unsafe { std::env::set_var("APB_TEST_CLOUD_CATALOG_URL", &url) };
            let err = refresh(&NEAR_AI).unwrap_err();
            assert_eq!(err.code(), REFRESH_FAILED);
            assert!(!err.reason.contains("not json at all"), "{err}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{status}");
        }
    }

    #[test]
    fn a_corrupt_cache_falls_back_to_the_embedded_snapshot() {
        let _e = env("http://127.0.0.1:9/none");
        let path = cache_path(&NEAR_AI).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"fetched_at\": \"x\"").unwrap();
        let s = load(&NEAR_AI);
        assert_eq!(s.source, CatalogSource::Embedded);
        assert!(s.cache_problem.is_some());
    }

    #[test]
    fn the_doctor_line_names_staleness_a_bad_cache_and_the_key_by_name() {
        let fresh = CatalogSnapshot {
            fetched_at: crate::clock::iso_utc(crate::clock::now_ms_u64()),
            ..embedded(&NEAR_AI)
        };
        let (warn, name, detail) = doctor_line(&fresh, true);
        assert!(!warn);
        assert_eq!(name, "cloud models NEAR AI Cloud");
        assert!(detail.contains("NEARAI_API_KEY is set"), "{detail}");
        let old = CatalogSnapshot {
            fetched_at: "2020-01-01T00:00:00Z".into(),
            ..fresh.clone()
        };
        let (warn, _, detail) = doctor_line(&old, false);
        assert!(warn);
        assert!(detail.contains("cloud_catalog_stale"), "{detail}");
        assert!(detail.contains("is not set"), "{detail}");
        let broken = CatalogSnapshot {
            cache_problem: Some("the catalog cache does not parse".into()),
            ..fresh
        };
        assert!(doctor_line(&broken, true).0);
    }

    #[test]
    fn a_read_only_process_sends_no_request() {
        crate::process_mode::testing::in_read_only_child(
            "cloud_models::catalog::tests::a_read_only_process_sends_no_request",
            || {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let url = format!("http://{}/v1/models", listener.local_addr().unwrap());
                let _e = env(&url);
                let err = refresh(&NEAR_AI).unwrap_err();
                assert!(err.reason.contains("read-only"), "{err}");
                std::thread::sleep(Duration::from_millis(100));
                assert!(listener.accept().is_err(), "a request reached the stub");
            },
        );
    }
}
