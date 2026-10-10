//! A public allowlist projected from verified account metadata.
//! Credentials, raw metadata and arbitrary provider routes never cross the API.
use crate::{
    config::ProviderConfig,
    host::{ApiError, ApiResult},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::{fs::OpenOptions, io::Read, path::Path};

#[path = "models_refresh.rs"]
mod refresh;
pub use refresh::CatalogRefresh;

const MAX_CACHE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Serialize)]
pub struct ModelChoice {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub reasoning_efforts: Vec<String>,
    pub supports_images: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelCatalog {
    pub models: Vec<ModelChoice>,
    pub default_model_id: String,
    pub selection_enabled: bool,
}

impl ModelCatalog {
    fn configured(config: &ProviderConfig) -> Self {
        Self {
            models: vec![ModelChoice {
                id: config.model.clone(),
                name: config.model.clone(),
                provider: config.id.clone(),
                reasoning_efforts: Vec::new(),
                supports_images: config.image_input,
            }],
            default_model_id: config.model.clone(),
            selection_enabled: false,
        }
    }

    fn supports_discovery(config: &ProviderConfig) -> bool {
        config.id == "codex"
            && config.protocol == "responses"
            && config.endpoint == "https://api.openai.com/v1"
            && config.desktop_data_root.is_some()
    }

    pub fn load(config: &ProviderConfig, state: &Path) -> Self {
        Self::load_verified(config, state).unwrap_or_else(|| Self::configured(config))
    }

    fn load_verified(config: &ProviderConfig, state: &Path) -> Option<Self> {
        // Account model selection never changes provider, endpoint or billing route.
        if !Self::supports_discovery(config) {
            return None;
        }
        let root = config.desktop_data_root.as_ref()?;
        // Match the cache to the credential currently used by the shared desktop
        // loader. Reading this key does not refresh it or contact the provider.
        let key = openide_connections::read_key(root, "OPENIDE_CODEX_ACCESS_TOKEN")?;
        Self::load_for_key(config, state, &key, now_ms())
    }

    fn load_for_key(config: &ProviderConfig, state: &Path, key: &str, now: u64) -> Option<Self> {
        let fingerprint = credential_fingerprint(key);
        let remote =
            crate::storage::read_private(&state.join(refresh::CACHE_FILE), MAX_CACHE_BYTES)
                .ok()
                .and_then(|bytes| from_verified_cache(config, &bytes, &fingerprint, now));
        remote.or_else(|| {
            let root = config.desktop_data_root.as_ref()?;
            let bytes = read_catalog(&root.join("provider-catalog.json"))?;
            from_verified_cache(config, &bytes, &fingerprint, now)
        })
    }

    pub fn select(&self, model_id: Option<&str>, effort: Option<&str>) -> ApiResult<&ModelChoice> {
        let id = model_id.unwrap_or(&self.default_model_id);
        let choice = self
            .models
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| {
                ApiError::bad(
                    "Model is not available in this host's verified catalog; refresh models",
                )
            })?;
        if effort.is_some_and(|value| {
            !choice
                .reasoning_efforts
                .iter()
                .any(|allowed| allowed == value)
        }) {
            return Err(ApiError::bad(
                "Reasoning effort is not available for this exact model; refresh models",
            ));
        }
        Ok(choice)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

// The desktop catalog contains metadata, not credentials, and is legitimately
// written 0644. Require owner-controlled regular files, not private-key mode.
fn read_catalog(path: &Path) -> Option<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_CACHE_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return None;
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_CACHE_BYTES).then_some(bytes)
}

fn credential_fingerprint(key: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"OpenIDE provider catalog credential\0codex\0");
    hash.update(key.trim().as_bytes());
    format!("{:x}", hash.finalize())
}

#[derive(Deserialize, Serialize)]
struct Cache {
    version: u16,
    providers: BTreeMap<String, Provider>,
    provider_status: BTreeMap<String, Status>,
}
#[derive(Deserialize, Serialize)]
struct Provider {
    id: String,
    api: Option<String>,
    models: BTreeMap<String, CachedModel>,
}
#[derive(Deserialize, Serialize)]
struct Status {
    source: String,
    verified_at_ms: u64,
    credential_fingerprint: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct CachedModel {
    id: String,
    name: String,
    #[serde(default)]
    listing_index: Option<usize>,
    #[serde(default)]
    tool_call_declared: Option<bool>,
    #[serde(default)]
    provider_efforts: Vec<String>,
    #[serde(default)]
    provider_efforts_declared: bool,
    #[serde(default)]
    declared_capabilities: BTreeMap<String, bool>,
    #[serde(default)]
    modalities: Option<Value>,
}

fn valid_text(value: &str, cap: usize) -> bool {
    !value.trim().is_empty() && value.len() <= cap && !value.chars().any(char::is_control)
}

fn from_verified_cache(
    config: &ProviderConfig,
    bytes: &[u8],
    credential: &str,
    now: u64,
) -> Option<ModelCatalog> {
    let cache: Cache = serde_json::from_slice(bytes).ok()?;
    let status = cache.provider_status.get("codex")?;
    let provider = cache.providers.get("codex")?;
    if cache.version != 3
        || status.source != "official_api"
        || status.verified_at_ms == 0
        || status.verified_at_ms > now
        || now - status.verified_at_ms > MAX_AGE_MS
        || status.credential_fingerprint.as_deref() != Some(credential)
        || provider.id != "codex"
        || provider.api.as_deref() != Some("https://api.openai.com/v1/models")
        || provider.models.len() > 5000
    {
        return None;
    }
    let mut entries = provider.models.iter().collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        (a.1.listing_index.unwrap_or(usize::MAX), a.0)
            .cmp(&(b.1.listing_index.unwrap_or(usize::MAX), b.0))
    });
    let mut models = Vec::new();
    for (key, model) in entries {
        if key != &model.id
            || !valid_text(&model.id, 256)
            || !valid_text(&model.name, 256)
            || model.tool_call_declared == Some(false)
        {
            continue;
        }
        if let Some(input) = model
            .modalities
            .as_ref()
            .and_then(|m| m.get("input"))
            .and_then(Value::as_array)
        {
            if !input.iter().any(|value| value.as_str() == Some("text")) {
                continue;
            }
        }
        let mut seen = HashSet::new();
        let efforts = if model.provider_efforts_declared
            && model.provider_efforts.len() <= 32
            && model.provider_efforts.iter().all(|value| {
                valid_text(value, 64)
                    && !value.chars().any(char::is_whitespace)
                    && seen.insert(value)
            }) {
            model.provider_efforts.clone()
        } else {
            Vec::new()
        };
        models.push(ModelChoice {
            id: model.id.clone(),
            name: model.name.clone(),
            provider: "codex".into(),
            reasoning_efforts: efforts,
            supports_images: config.image_input
                && model.declared_capabilities.get("image_input") == Some(&true),
        });
    }
    // Retain the explicitly configured route even if the listing is partial;
    // it receives no inferred effort or extra capability declarations.
    if !models.iter().any(|model| model.id == config.model) {
        models.insert(0, ModelCatalog::configured(config).models.remove(0));
    }
    let selection_enabled = models.len() > 1
        || models
            .iter()
            .any(|model| !model.reasoning_efforts.is_empty());
    Some(ModelCatalog {
        models,
        default_model_id: config.model.clone(),
        selection_enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn config() -> ProviderConfig {
        ProviderConfig {
            id: "codex".into(),
            protocol: "responses".into(),
            endpoint: "https://api.openai.com/v1".into(),
            model: "current".into(),
            api_key_env: None,
            desktop_data_root: None,
            tool_calls: true,
            image_input: true,
        }
    }
    fn cache() -> Value {
        json!({"version":3,"providers":{"codex":{"id":"codex","api":"https://api.openai.com/v1/models","models":{
        "current":{"id":"current","name":"Current","provider_efforts_declared":true,"provider_efforts":["low","high"],"declared_capabilities":{"image_input":true}},
        "alternate":{"id":"alternate","name":"Alternate","provider_efforts_declared":true,"provider_efforts":["medium"]}
    }}},"provider_status":{"codex":{"source":"official_api","verified_at_ms":100,"credential_fingerprint":"expected"}}})
    }
    #[test]
    fn selection_requires_exact_verified_account_metadata() {
        let parsed = from_verified_cache(
            &config(),
            &serde_json::to_vec(&cache()).unwrap(),
            "expected",
            101,
        )
        .unwrap();
        assert!(parsed.selection_enabled);
        assert!(parsed.select(Some("alternate"), Some("medium")).is_ok());
        assert!(parsed.select(Some("alternate"), Some("high")).is_err());
        assert!(parsed.select(Some("unlisted"), None).is_err());
        for (pointer, value) in [
            ("/version", json!(2)),
            ("/provider_status/codex/source", json!("reviewed_seed")),
            (
                "/provider_status/codex/credential_fingerprint",
                json!("other-account"),
            ),
            (
                "/providers/codex/api",
                json!("https://untrusted.invalid/models"),
            ),
        ] {
            let mut data = cache();
            *data.pointer_mut(pointer).unwrap() = value;
            assert!(from_verified_cache(
                &config(),
                &serde_json::to_vec(&data).unwrap(),
                "expected",
                101
            )
            .is_none());
        }
        assert!(from_verified_cache(
            &config(),
            &serde_json::to_vec(&cache()).unwrap(),
            "expected",
            MAX_AGE_MS + 101
        )
        .is_none());
        assert!(from_verified_cache(
            &config(),
            &serde_json::to_vec(&cache()).unwrap(),
            "expected",
            99
        )
        .is_none());
    }
    #[test]
    fn unsupported_and_malformed_metadata_never_invent_controls() {
        let fallback = ModelCatalog::configured(&config());
        assert!(!fallback.selection_enabled);
        assert!(fallback.select(None, Some("low")).is_err());
        let mut data = cache();
        data["providers"]["codex"]["models"]["alternate"]["tool_call_declared"] = json!(false);
        data["providers"]["codex"]["models"]["current"]["provider_efforts"] = json!(["low", "low"]);
        let parsed = from_verified_cache(
            &config(),
            &serde_json::to_vec(&data).unwrap(),
            "expected",
            101,
        )
        .unwrap();
        assert_eq!(parsed.models.len(), 1);
        assert!(parsed.models[0].reasoning_efforts.is_empty());
        assert!(!parsed.selection_enabled);
    }
}
