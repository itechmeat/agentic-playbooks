//! The dashboard's refresh of the public cloud model catalogs
//! (`apb_core::cloud_models`): at start and then every 24 hours, for each
//! provider whose `cloud_models.<provider>.auto_refresh` is not `false`. A
//! failure is logged once per run of the task and leaves the catalog as it
//! was; no credential is ever sent.

use std::time::Duration;

use apb_core::cloud_models::{self, catalog};
use apb_core::config::CloudModelsConfig;

/// How often the catalogs are refreshed.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The providers the task refreshes under `cfg`.
fn providers(cfg: &CloudModelsConfig) -> Vec<&'static cloud_models::CloudProvider> {
    cloud_models::all()
        .iter()
        .copied()
        .filter(|p| cfg.auto_refresh(p.id))
        .collect()
}

/// Starts the task; nothing when no provider refreshes on its own.
pub(crate) fn spawn(cfg: &CloudModelsConfig) {
    let providers = providers(cfg);
    if providers.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut logged = false;
        loop {
            let ps = providers.clone();
            let failures = tokio::task::spawn_blocking(move || {
                ps.iter()
                    .filter_map(|p| catalog::refresh(p).err())
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            if !failures.is_empty() && !logged {
                eprintln!("apb dashboard: {}", failures.join("; "));
                logged = true;
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::config::CloudProviderConfig;

    #[test]
    fn auto_refresh_false_leaves_a_provider_out() {
        assert_eq!(providers(&CloudModelsConfig::default()).len(), 1);
        let off = CloudModelsConfig {
            near_ai_cloud: Some(CloudProviderConfig {
                auto_refresh: Some(false),
            }),
        };
        assert!(providers(&off).is_empty());
    }
}
