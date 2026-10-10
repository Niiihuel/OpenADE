//! Codex-only metadata refresh. The desktop owns its catalog file; the remote
//! host stores only its own bounded projection under the private state root.
use super::*;
use reqwest::{blocking::Client, redirect::Policy};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) const CACHE_FILE: &str = "remote-model-catalog.json";
const ENDPOINT: &str = "https://api.openai.com/v1/models";
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const FAILURE_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct CatalogRefresh {
    // This lock is independent of Host.runtime. A concurrent reader can use
    // the last verified snapshot immediately while one reader refreshes.
    failed_at: Mutex<Option<Instant>>,
    account: Option<String>,
    account_guarded: bool,
}

impl CatalogRefresh {
    pub fn new(config: &ProviderConfig) -> Self {
        Self {
            failed_at: Mutex::new(None),
            account: account_identity(config),
            account_guarded: ModelCatalog::supports_discovery(config),
        }
    }

    pub fn account_matches(&self, config: &ProviderConfig) -> bool {
        !self.account_guarded
            || self
                .account
                .as_ref()
                .is_some_and(|identity| account_identity(config).as_ref() == Some(identity))
    }

    pub fn load(&self, config: &ProviderConfig, state: &Path) -> ModelCatalog {
        if !self.account_matches(config) {
            return ModelCatalog::configured(config);
        }
        ModelCatalog::load(config, state)
    }

    pub fn refresh(&self, config: &ProviderConfig, state: &Path) {
        if !ModelCatalog::supports_discovery(config)
            || !self.account_matches(config)
            || ModelCatalog::load_verified(config, state).is_some()
        {
            return;
        }
        self.refresh_with(
            config,
            state,
            || {
                let source = openide_connections::codex_token_source(
                    config.desktop_data_root.clone().ok_or(())?,
                )
                .map_err(|_| ())?;
                source().map_err(|_| ())
            },
            |key| fetch(ENDPOINT, key),
        );
    }

    fn refresh_with(
        &self,
        config: &ProviderConfig,
        state: &Path,
        token_source: impl FnOnce() -> Result<String, ()>,
        fetch: impl FnOnce(&str) -> Result<Vec<u8>, ()>,
    ) {
        if !ModelCatalog::supports_discovery(config) {
            return;
        }
        if !self.account_matches(config) {
            return;
        }
        let Ok(mut failed_at) = self.failed_at.try_lock() else {
            return;
        };
        if failed_at.is_some_and(|time| time.elapsed() < FAILURE_BACKOFF) {
            return;
        }
        let result = (|| {
            // Uses the exact credential renewal and cross-process OAuth lock
            // shared with the desktop, including account identity validation.
            let key = token_source()?;
            let now = now_ms();
            if ModelCatalog::load_for_key(config, state, &key, now).is_some() {
                return Ok(());
            }
            let bytes = fetch(&key)?;
            if !self.account_matches(config) {
                return Err(());
            }
            let cache = parse_codex(&bytes, &key, now_ms())?;
            // Never copy raw provider JSON, descriptions, account metadata,
            // response errors or credentials into this cache or the API.
            crate::storage::write_json(&state.join(CACHE_FILE), &cache).map_err(|_| ())
        })();
        *failed_at = result.err().map(|()| Instant::now());
    }
}

fn account_identity(config: &ProviderConfig) -> Option<String> {
    if !ModelCatalog::supports_discovery(config) {
        return None;
    }
    let record = openide_connections::load(config.desktop_data_root.as_ref()?, "codex")
        .ok()
        .flatten()?;
    if record.disabled || !record.oauth {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"OpenIDE remote Codex account\0");
    digest.update(record.subject?.as_bytes());
    digest.update(b"\0");
    digest.update(record.client_id?.as_bytes());
    Some(format!("{:x}", digest.finalize()))
}

fn fetch(endpoint: &str, key: &str) -> Result<Vec<u8>, ()> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::none())
        .build()
        .map_err(|_| ())?;
    let response = client
        .get(endpoint)
        .header("Accept", "application/json")
        .bearer_auth(key)
        .send()
        .map_err(|_| ())?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(());
    }
    Ok(bytes)
}

/// Matches the Codex mapping in desktop provider-definitions.json and the
/// bounded effort/modality helpers in native_provider.rs. It deliberately
/// supports only that fixed official route, not user-defined provider mapping.
fn parse_codex(bytes: &[u8], key: &str, now: u64) -> Result<Cache, ()> {
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(());
    }
    let document: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let entries = document
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| document.as_array())
        .ok_or(())?;
    if entries.len() > 5000 {
        return Err(());
    }
    let mut models = BTreeMap::new();
    for entry in entries {
        if entry.get("visibility").and_then(Value::as_str) != Some("list") {
            continue;
        }
        let id = entry
            .get("slug")
            .and_then(Value::as_str)
            .filter(|id| valid_text(id, 256))
            .ok_or(())?;
        let name = entry
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|name| valid_text(name, 256))
            .unwrap_or(id);
        let mut unique = HashSet::new();
        let presets = entry
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .filter(|values| {
                values.len() <= 32
                    && values.iter().all(|value| {
                        value
                            .get("effort")
                            .and_then(Value::as_str)
                            .is_some_and(|level| {
                                valid_text(level, 64)
                                    && !level.chars().any(char::is_whitespace)
                                    && unique.insert(level)
                            })
                    })
            });
        let efforts = presets
            .into_iter()
            .flatten()
            .filter_map(|value| value.get("effort").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        let modalities = entry
            .get("input_modalities")
            .and_then(Value::as_array)
            .filter(|values| {
                values.len() <= 16
                    && values
                        .iter()
                        .all(|value| value.as_str().is_some_and(|text| valid_text(text, 64)))
            });
        let mut capabilities = BTreeMap::new();
        if let Some(values) = modalities {
            capabilities.insert(
                "image_input".into(),
                values.iter().any(|value| value == "image"),
            );
        }
        let model = CachedModel {
            id: id.into(),
            name: name.into(),
            listing_index: Some(models.len()),
            // The official Codex listing has no configured tool_call mapping.
            tool_call_declared: None,
            provider_efforts: efforts,
            provider_efforts_declared: presets.is_some(),
            declared_capabilities: capabilities,
            modalities: modalities
                .filter(|values| !values.is_empty())
                .map(|values| serde_json::json!({"input": values})),
        };
        if models.insert(id.into(), model).is_some() {
            return Err(());
        }
    }
    Ok(Cache {
        version: 3,
        providers: BTreeMap::from([(
            "codex".into(),
            Provider {
                id: "codex".into(),
                api: Some(ENDPOINT.into()),
                models,
            },
        )]),
        provider_status: BTreeMap::from([(
            "codex".into(),
            Status {
                source: "official_api".into(),
                verified_at_ms: now,
                credential_fingerprint: Some(credential_fingerprint(key)),
            },
        )]),
    })
}

#[cfg(test)]
#[path = "models_refresh_tests.rs"]
mod tests;
