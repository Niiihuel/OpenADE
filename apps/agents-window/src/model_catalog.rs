//! OpenIDE's bounded, provider-owned model metadata catalog.
//!
//! Listings come directly from each declared provider. Provider metadata is
//! descriptive; the request adapter decides which controls it can send.

use crate::native_provider::{self, Adapter, CatalogMapping, ProviderRegistry};
use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, ETAG, IF_NONE_MATCH};
use reqwest::redirect::Policy;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const CACHE_FILE: &str = "provider-catalog.json";
const CATALOG_VERSION: u16 = 3;
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_MODELS_PER_PROVIDER: usize = 5_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    #[serde(default)]
    pub version: u16,
    pub providers: BTreeMap<String, ProviderInfo>,
    /// Last successful official check among providers; zero for a seed alone.
    pub fetched_at_ms: u64,
    #[serde(default)]
    pub provider_status: BTreeMap<String, ProviderCatalogStatus>,
}

impl CatalogSnapshot {
    pub fn is_stale(&self, ttl: Duration) -> bool {
        let ttl_ms = ttl.as_millis().min(u128::from(u64::MAX)) as u64;
        self.provider_status.values().any(|status| {
            status.verified_at_ms == 0 || now_ms().saturating_sub(status.verified_at_ms) >= ttl_ms
        })
    }

    /// Discovery metadata only; the adapter matrix decides execution support.
    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&ModelInfo> {
        self.providers.get(provider_id)?.models.get(model_id)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    #[default]
    ReviewedSeed,
    OfficialApi,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderCatalogStatus {
    pub source: CatalogSource,
    /// Last completed attempt, including an error or missing key.
    pub attempted_at_ms: u64,
    /// Last successful HTTP 200 or 304. Zero means unverified offline seed.
    pub verified_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// One-way identity of the key used for this listing; never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    /// Hash of the registry definition that produced this provider's data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub npm: Option<String>,
    /// Fixed, provider-owned listing URL. Never an execution endpoint.
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelInfo>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub tool_call: bool,
    /// `None` means the listing did not make a tool capability claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_declared: Option<bool>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub open_weights: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub limit: Option<ModelLimit>,
    #[serde(default)]
    pub modalities: Option<ModelModalities>,
    /// USD per one million tokens, not the provider's raw per-token values.
    #[serde(default)]
    pub cost: Option<ModelCost>,
    /// Provider-reported effort values for this exact model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelLimit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone)]
struct SourceSpec {
    id: String,
    name: String,
    url: String,
    key: Option<String>,
    key_env: String,
    anonymous: bool,
    mapping: CatalogMapping,
    definition_fingerprint: String,
}

fn official_specs(root: &Path) -> Result<Vec<SourceSpec>, String> {
    specs_from_registry(&ProviderRegistry::load(root)?)
}

fn specs_from_registry(registry: &ProviderRegistry) -> Result<Vec<SourceSpec>, String> {
    registry
        .adapters()
        .iter()
        .filter_map(|adapter| {
            adapter
                .catalog_list_path
                .as_ref()
                .or(adapter.model_list_path.as_ref())
                .map(|path| spec_from_adapter(adapter, path))
        })
        .collect()
}

fn spec_from_adapter(adapter: &Adapter, path: &str) -> Result<SourceSpec, String> {
    let definition = serde_json::to_vec(adapter)
        .map_err(|error| format!("invalid provider definition: {error}"))?;
    let mut hash = Sha256::new();
    hash.update(b"OpenIDE provider catalog definition\0");
    hash.update(&definition);
    let anonymous = adapter.catalog_anonymous;
    Ok(SourceSpec {
        id: adapter.id.clone(),
        name: adapter.name.clone(),
        url: format!("{}/{}", adapter.endpoint.trim_end_matches('/'), path),
        key: if anonymous {
            None
        } else {
            std::env::var(&adapter.key_env).ok()
        },
        key_env: adapter.key_env.clone(),
        anonymous,
        mapping: adapter.catalog.clone(),
        definition_fingerprint: format!("{:x}", hash.finalize()),
    })
}

fn credential_fingerprint(spec: &SourceSpec) -> Option<String> {
    let key = spec
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())?;
    let mut hash = Sha256::new();
    hash.update(b"OpenIDE provider catalog credential\0");
    hash.update(spec.id.as_bytes());
    hash.update(b"\0");
    hash.update(key.as_bytes());
    Some(format!("{:x}", hash.finalize()))
}

/// A listing may contain private model IDs. Changing or removing the API key
/// drops that provider's account-specific models and ETag before any refresh.
fn rebind_credentials(snapshot: &mut CatalogSnapshot, specs: &[SourceSpec]) -> Result<(), String> {
    for spec in specs {
        let expected = credential_fingerprint(spec);
        let status = snapshot
            .provider_status
            .get_mut(&spec.id)
            .ok_or("provider catalog status is missing")?;
        if status.credential_fingerprint == expected
            && status.definition_fingerprint.as_deref() == Some(&spec.definition_fingerprint)
        {
            continue;
        }
        snapshot
            .providers
            .get_mut(&spec.id)
            .ok_or("provider catalog entry is missing")?
            .models = reviewed_models(&spec.id);
        *status = ProviderCatalogStatus {
            credential_fingerprint: expected,
            definition_fingerprint: Some(spec.definition_fingerprint.clone()),
            ..ProviderCatalogStatus::default()
        };
    }
    snapshot.fetched_at_ms = snapshot
        .provider_status
        .values()
        .map(|status| status.verified_at_ms)
        .max()
        .unwrap_or(0);
    Ok(())
}

/// Keep only current registry sources. A removed provider or changed listing
/// cannot survive in a stale cache and reappear in the model picker.
fn reconcile_sources(snapshot: &mut CatalogSnapshot, specs: &[SourceSpec]) {
    snapshot
        .providers
        .retain(|id, _| specs.iter().any(|spec| &spec.id == id));
    snapshot
        .provider_status
        .retain(|id, _| specs.iter().any(|spec| &spec.id == id));
    for spec in specs {
        let entry = snapshot
            .providers
            .entry(spec.id.clone())
            .or_insert_with(|| provider(spec));
        entry.name = spec.name.clone();
        entry.api = Some(spec.url.clone());
        entry.env = vec![spec.key_env.clone()];
        snapshot.provider_status.entry(spec.id.clone()).or_default();
    }
    snapshot.fetched_at_ms = snapshot
        .provider_status
        .values()
        .map(|status| status.verified_at_ms)
        .max()
        .unwrap_or(0);
}

fn provider(spec: &SourceSpec) -> ProviderInfo {
    ProviderInfo {
        id: spec.id.clone(),
        name: spec.name.clone(),
        npm: None,
        api: Some(spec.url.clone()),
        env: vec![spec.key_env.clone()],
        doc: None,
        models: reviewed_models(&spec.id),
        extra: BTreeMap::new(),
    }
}

fn reviewed_models(provider_id: &str) -> BTreeMap<String, ModelInfo> {
    native_provider::reviewed_native_models()
        .iter()
        .filter(|reviewed| reviewed.provider_id == provider_id)
        .map(|reviewed| (reviewed.model_id.to_owned(), reviewed_model(reviewed)))
        .collect()
}

fn seed(specs: &[SourceSpec]) -> CatalogSnapshot {
    CatalogSnapshot {
        version: CATALOG_VERSION,
        providers: specs
            .iter()
            .map(|spec| (spec.id.clone(), provider(spec)))
            .collect(),
        fetched_at_ms: 0,
        provider_status: specs
            .iter()
            .map(|spec| {
                (
                    spec.id.clone(),
                    ProviderCatalogStatus {
                        credential_fingerprint: credential_fingerprint(spec),
                        definition_fingerprint: Some(spec.definition_fingerprint.clone()),
                        ..ProviderCatalogStatus::default()
                    },
                )
            })
            .collect(),
    }
}

pub(crate) fn reviewed_seed_snapshot() -> CatalogSnapshot {
    let registry = ProviderRegistry::builtin();
    seed(&specs_from_registry(&registry).expect("bundled provider catalog definitions"))
}

fn empty_model(id: &str, name: &str) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        name: name.into(),
        description: None,
        family: None,
        tool_call: false,
        tool_call_declared: None,
        reasoning: false,
        attachment: false,
        structured_output: false,
        temperature: false,
        open_weights: false,
        status: None,
        release_date: None,
        last_updated: None,
        limit: None,
        modalities: None,
        cost: None,
        provider_efforts: Vec::new(),
        extra: BTreeMap::new(),
    }
}

fn reviewed_model(reviewed: &native_provider::NativeModelCapabilities) -> ModelInfo {
    let mut model = empty_model(reviewed.model_id, reviewed.model_id);
    model.tool_call = reviewed.function_tools;
    model.tool_call_declared = Some(reviewed.function_tools);
    model.reasoning = !reviewed.supported_efforts.is_empty();
    model.attachment = reviewed.image_input;
    model.limit = Some(ModelLimit {
        context: reviewed.context_window_tokens,
        input: None,
        output: reviewed.max_output_tokens,
        extra: BTreeMap::new(),
    });
    model.modalities = Some(ModelModalities {
        input: if reviewed.image_input {
            vec!["text".into(), "image".into()]
        } else {
            vec!["text".into()]
        },
        output: vec!["text".into()],
        extra: BTreeMap::new(),
    });
    model
}

/// A missing cache immediately yields the bundled provider seed.
pub fn load_cache(root: &Path) -> Result<Option<CatalogSnapshot>, String> {
    let specs = official_specs(root)?;
    let path = cache_path(root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(seed(&specs)))
        }
        Err(error) => return Err(format!("cannot open provider catalog cache: {error}")),
    };
    if file
        .metadata()
        .map_err(|error| format!("cannot inspect provider catalog cache: {error}"))?
        .len()
        > MAX_CACHE_BYTES
    {
        return Err("provider catalog cache exceeds the size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read provider catalog cache: {error}"))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err("provider catalog cache exceeds the size limit".into());
    }
    let mut snapshot: CatalogSnapshot = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid provider catalog cache: {error}"))?;
    validate_snapshot(&snapshot)?;
    reconcile_sources(&mut snapshot, &specs);
    rebind_credentials(&mut snapshot, &specs)?;
    validate_snapshot(&snapshot)?;
    Ok(Some(snapshot))
}

/// Refreshes each declared provider independently. A failed provider keeps
/// its previous models and ETag; the status records a bounded, credential-free
/// error. Disk replacement occurs only after the merged snapshot validates.
pub fn refresh(root: &Path, previous: Option<&CatalogSnapshot>) -> Result<CatalogSnapshot, String> {
    let specs = official_specs(root)?;
    refresh_with_specs(root, previous, &specs)
}

fn refresh_with_specs(
    root: &Path,
    previous: Option<&CatalogSnapshot>,
    specs: &[SourceSpec],
) -> Result<CatalogSnapshot, String> {
    let mut snapshot = previous.cloned().unwrap_or_else(|| seed(specs));
    validate_snapshot(&snapshot)?;
    reconcile_sources(&mut snapshot, specs);
    rebind_credentials(&mut snapshot, specs)?;
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("cannot create provider catalog client: {error}"))?;
    for spec in specs {
        let attempt = now_ms();
        let status = snapshot
            .provider_status
            .get(&spec.id)
            .ok_or("provider catalog status is missing")?
            .clone();
        let result = fetch_provider(&client, spec, &status).and_then(|response| match response {
            FetchResult::NotModified => {
                if status.source != CatalogSource::OfficialApi || status.verified_at_ms == 0 {
                    return Err("HTTP 304 without a verified provider catalog".into());
                }
                Ok((None, status.etag.clone()))
            }
            FetchResult::Models { models, etag } => Ok((Some(models), etag)),
        });
        let entry = snapshot
            .provider_status
            .get_mut(&spec.id)
            .ok_or("provider catalog status is missing")?;
        entry.attempted_at_ms = attempt;
        entry.credential_fingerprint = credential_fingerprint(spec);
        entry.definition_fingerprint = Some(spec.definition_fingerprint.clone());
        match result {
            Ok((models, etag)) => {
                if let Some(models) = models {
                    let provider = snapshot
                        .providers
                        .get_mut(&spec.id)
                        .ok_or("provider catalog entry is missing")?;
                    provider.models = models;
                }
                entry.source = CatalogSource::OfficialApi;
                entry.verified_at_ms = attempt;
                entry.etag = etag;
                entry.last_error = None;
            }
            Err(error) => entry.last_error = Some(bounded_error(&error)),
        }
    }
    snapshot.fetched_at_ms = snapshot
        .provider_status
        .values()
        .map(|status| status.verified_at_ms)
        .max()
        .unwrap_or(0);
    validate_snapshot(&snapshot)?;
    write_cache(root, &snapshot)?;
    Ok(snapshot)
}

enum FetchResult {
    NotModified,
    Models {
        models: BTreeMap<String, ModelInfo>,
        etag: Option<String>,
    },
}

fn fetch_provider(
    client: &Client,
    spec: &SourceSpec,
    previous: &ProviderCatalogStatus,
) -> Result<FetchResult, String> {
    let key = spec
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    if key.is_none() && !spec.anonymous {
        return Err(format!("{} is not configured", spec.key_env));
    }
    if key.is_some_and(|key| key.len() > 4_096 || key.chars().any(char::is_control)) {
        return Err(format!("{} has an invalid format", spec.key_env));
    }
    let mut request = client.get(&spec.url).header("Accept", "application/json");
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    if let Some(etag) = previous
        .etag
        .as_deref()
        .filter(|etag| valid_etag(etag))
        .and_then(|etag| HeaderValue::from_str(etag).ok())
    {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let response = request
        .send()
        .map_err(|_| "network request failed or timed out".to_owned())?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(FetchResult::NotModified);
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err("response exceeds the size limit".into());
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_etag(value))
        .map(str::to_owned);
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read provider response".to_owned())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("response exceeds the size limit".into());
    }
    Ok(FetchResult::Models {
        models: parse_provider_models(spec, &bytes)?,
        etag,
    })
}

fn parse_provider_models(
    spec: &SourceSpec,
    bytes: &[u8],
) -> Result<BTreeMap<String, ModelInfo>, String> {
    let document: Value =
        serde_json::from_slice(bytes).map_err(|_| "invalid provider JSON".to_owned())?;
    let entries = path_value(&document, spec.mapping.entries_path.as_deref(), "data")
        .and_then(Value::as_array)
        .or_else(|| document.as_array())
        .ok_or("provider JSON has no model list")?;
    if entries.len() > MAX_MODELS_PER_PROVIDER {
        return Err("provider model count is invalid".into());
    }
    let mut models = BTreeMap::new();
    for entry in entries {
        let id = path_value(entry, spec.mapping.id_path.as_deref(), "id")
            .and_then(Value::as_str)
            .filter(|id| valid_text(id, 256))
            .ok_or("provider model ID is invalid")?;
        if models
            .insert(id.to_owned(), parse_model(spec, id, entry))
            .is_some()
        {
            return Err("provider model list contains duplicate IDs".into());
        }
    }
    Ok(models)
}

fn parse_model(spec: &SourceSpec, id: &str, entry: &Value) -> ModelInfo {
    let mut model = empty_model(id, id);
    model.name = path_value(entry, spec.mapping.name_path.as_deref(), "name")
        .and_then(Value::as_str)
        .filter(|name| valid_text(name, 256))
        .unwrap_or(id)
        .to_owned();
    model.description = text_field(entry, "description", 4_096).map(str::to_owned);
    let context = positive_u64(path_value(
        entry,
        spec.mapping.context_window_path.as_deref(),
        "",
    ));
    let output = positive_u64(path_value(
        entry,
        spec.mapping.max_output_path.as_deref(),
        "",
    ));
    if context.is_some() || output.is_some() {
        model.limit = Some(ModelLimit {
            context,
            input: None,
            output,
            extra: BTreeMap::new(),
        });
    }
    if let Some(value) = path_value(entry, spec.mapping.tool_call_path.as_deref(), "") {
        let listed = if let Some(needle) = spec.mapping.tool_call_contains.as_deref() {
            value
                .as_array()
                .filter(|items| {
                    items.len() <= 128
                        && items
                            .iter()
                            .all(|item| item.as_str().is_some_and(|text| valid_text(text, 64)))
                })
                .map(|items| items.iter().any(|item| item.as_str() == Some(needle)))
        } else {
            value.as_bool()
        };
        if let Some(listed) = listed {
            model.tool_call = listed;
            model.tool_call_declared = Some(listed);
        }
    }
    model.provider_efforts = bounded_string_list(
        path_value(entry, spec.mapping.effort_levels_path.as_deref(), ""),
        16,
    );
    model.reasoning = model.reasoning || !model.provider_efforts.is_empty();
    let input = bounded_string_list(
        path_value(entry, spec.mapping.input_modalities_path.as_deref(), ""),
        16,
    );
    let output = bounded_string_list(
        path_value(entry, spec.mapping.output_modalities_path.as_deref(), ""),
        16,
    );
    if !input.is_empty() || !output.is_empty() {
        model.modalities = Some(ModelModalities {
            input,
            output,
            extra: BTreeMap::new(),
        });
    }
    model.attachment = model.attachment
        || model
            .modalities
            .as_ref()
            .is_some_and(|modalities| modalities.input.iter().any(|kind| kind != "text"));
    let unit = spec
        .mapping
        .price_unit
        .as_deref()
        .unwrap_or("per_million_tokens");
    let input_price = usd_per_million(
        path_value(entry, spec.mapping.pricing_input_path.as_deref(), ""),
        unit,
    );
    let output_price = usd_per_million(
        path_value(entry, spec.mapping.pricing_output_path.as_deref(), ""),
        unit,
    );
    if input_price.is_some() || output_price.is_some() {
        model.cost = Some(ModelCost {
            input: input_price,
            output: output_price,
            cache_read: None,
            cache_write: None,
            extra: BTreeMap::new(),
        });
    }
    model
}

fn path_value<'a>(value: &'a Value, path: Option<&str>, default: &str) -> Option<&'a Value> {
    let path = path.unwrap_or(default);
    if path.is_empty() {
        return None;
    }
    path.split('.')
        .try_fold(value, |current, part| current.get(part))
}

fn text_field<'a>(value: &'a Value, field: &str, limit: usize) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| valid_text(text, limit))
}

fn valid_text(text: &str, limit: usize) -> bool {
    !text.trim().is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|value| *value > 0)
}

fn bounded_string_list(value: Option<&Value>, max: usize) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .filter(|items| items.len() <= max)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| valid_text(item, 64))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn usd_per_million(value: Option<&Value>, unit: &str) -> Option<f64> {
    let raw = match value? {
        Value::String(raw) => raw.parse::<f64>().ok()?,
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    let price = match unit {
        "per_token" => raw * 1_000_000.0,
        "per_million_tokens" => raw,
        "usd_cents_per_100m_tokens" => raw / 10_000.0,
        _ => return None,
    };
    price
        .is_finite()
        .then_some(price)
        .filter(|price| (0.0..=1_000_000_000.0).contains(price))
}

fn valid_etag(etag: &str) -> bool {
    !etag.is_empty()
        && etag.len() <= 256
        && !etag.chars().any(char::is_control)
        && HeaderValue::from_str(etag).is_ok()
}

fn bounded_error(error: &str) -> String {
    error
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect()
}

fn validate_snapshot(snapshot: &CatalogSnapshot) -> Result<(), String> {
    if snapshot.version != CATALOG_VERSION {
        return Err("unsupported provider catalog version".into());
    }
    if snapshot.providers.len() > 128
        || snapshot.providers.len() != snapshot.provider_status.len()
        || snapshot
            .providers
            .keys()
            .ne(snapshot.provider_status.keys())
    {
        return Err("provider catalog source set is invalid".into());
    }
    for (id, provider) in &snapshot.providers {
        let status = snapshot
            .provider_status
            .get(id)
            .ok_or("provider catalog status is incomplete")?;
        if !valid_text(id, 64)
            || provider.id != *id
            || !valid_text(&provider.name, 128)
            || provider.models.len() > MAX_MODELS_PER_PROVIDER
            || provider
                .models
                .iter()
                .any(|(id, model)| id != &model.id || !valid_text(id, 256))
            || status.etag.as_deref().is_some_and(|etag| !valid_etag(etag))
            || status
                .credential_fingerprint
                .as_deref()
                .is_some_and(|fingerprint| !valid_fingerprint(fingerprint))
            || status
                .definition_fingerprint
                .as_deref()
                .is_some_and(|fingerprint| !valid_fingerprint(fingerprint))
            || status
                .last_error
                .as_deref()
                .is_some_and(|error| error.len() > 256 || error.chars().any(char::is_control))
            || (status.source == CatalogSource::ReviewedSeed && status.verified_at_ms != 0)
        {
            return Err("provider catalog contains invalid data".into());
        }
    }
    let verified = snapshot
        .provider_status
        .values()
        .map(|status| status.verified_at_ms)
        .max()
        .unwrap_or(0);
    if snapshot.fetched_at_ms != verified {
        return Err("provider catalog verification time is inconsistent".into());
    }
    Ok(())
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cache_path(root: &Path) -> PathBuf {
    root.join(CACHE_FILE)
}

fn write_cache(root: &Path, snapshot: &CatalogSnapshot) -> Result<(), String> {
    validate_snapshot(snapshot)?;
    let bytes = serde_json::to_vec(snapshot)
        .map_err(|error| format!("cannot encode provider catalog cache: {error}"))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err("provider catalog cache exceeds the size limit".into());
    }
    fs::create_dir_all(root)
        .map_err(|error| format!("cannot create provider catalog directory: {error}"))?;
    let temporary = root.join(format!(".{CACHE_FILE}.{}.tmp", Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_cache_atomically(&temporary, &cache_path(root))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("cannot update provider catalog cache: {error}"))
}

#[cfg(not(windows))]
fn replace_cache_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_cache_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[link(name = "Kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file: *const u16,
            replacement_file: *const u16,
            backup_file: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    if !destination.exists() {
        return fs::rename(temporary, destination);
    }
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temporary_wide: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if replaced != 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if !destination.exists() {
            fs::rename(temporary, destination)
        } else {
            Err(error)
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn builtin_spec(id: &str) -> SourceSpec {
        specs_from_registry(&ProviderRegistry::builtin())
            .unwrap()
            .into_iter()
            .find(|spec| spec.id == id)
            .unwrap()
    }

    #[test]
    fn declared_paths_drive_model_metadata_without_provider_branches() {
        let router = builtin_spec("openrouter");
        let models = parse_provider_models(
            &router,
            br#"{"data":[{"id":"vendor/new","name":"New","context_length":123456,"supported_parameters":["tools"],"architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},"pricing":{"prompt":"0.000002","completion":"0.000003"}}]}"#,
        )
        .unwrap();
        let model = &models["vendor/new"];
        assert_eq!(model.tool_call_declared, Some(true));
        assert_eq!(model.limit.as_ref().unwrap().context, Some(123456));
        assert_eq!(model.cost.as_ref().unwrap().input, Some(2.0));
        assert!(model.attachment);

        let malformed = parse_provider_models(
            &router,
            br#"{"data":[{"id":"vendor/malformed","supported_parameters":{"tools":false}}]}"#,
        )
        .unwrap();
        assert_eq!(malformed["vendor/malformed"].tool_call_declared, None);

        let xai = builtin_spec("xai");
        let models = parse_provider_models(
            &xai,
            br#"{"models":[{"id":"future","capabilities":{"reasoning_effort":["low","high"]},"prompt_text_token_price":25000,"completion_text_token_price":50000}]}"#,
        )
        .unwrap();
        let model = &models["future"];
        assert_eq!(model.provider_efforts, ["low", "high"]);
        assert_eq!(model.cost.as_ref().unwrap().input, Some(2.5));

        let mistral = builtin_spec("mistral");
        let models = parse_provider_models(
            &mistral,
            br#"[{"id":"new-mistral","capabilities":{"function_calling":true},"max_context_length":32768}]"#,
        )
        .unwrap();
        assert_eq!(models["new-mistral"].tool_call_declared, Some(true));
        assert_eq!(
            models["new-mistral"].limit.as_ref().unwrap().context,
            Some(32768)
        );
    }

    #[test]
    fn official_listing_does_not_reinsert_reviewed_ids() {
        let openai = builtin_spec("openai");
        let models =
            parse_provider_models(&openai, br#"{"data":[{"id":"future-model"}]}"#).unwrap();
        assert_eq!(
            models.keys().map(String::as_str).collect::<Vec<_>>(),
            ["future-model"]
        );
        assert_eq!(models["future-model"].tool_call_declared, None);
        assert!(parse_provider_models(&openai, br#"{"data":[]}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn account_or_definition_change_purges_cached_private_models() {
        let mut source = builtin_spec("openai");
        source.key = Some("first-secret".into());
        let mut snapshot = seed(&[source.clone()]);
        snapshot.providers.get_mut("openai").unwrap().models.insert(
            "private-fine-tune".into(),
            empty_model("private-fine-tune", "Private"),
        );
        let status = snapshot.provider_status.get_mut("openai").unwrap();
        status.source = CatalogSource::OfficialApi;
        status.verified_at_ms = 42;
        status.etag = Some("\"private\"".into());
        snapshot.fetched_at_ms = 42;
        source.key = Some("second-secret".into());
        rebind_credentials(&mut snapshot, &[source.clone()]).unwrap();
        assert!(!snapshot.providers["openai"]
            .models
            .contains_key("private-fine-tune"));
        assert_eq!(snapshot.provider_status["openai"].verified_at_ms, 0);
        assert_eq!(snapshot.provider_status["openai"].etag, None);

        snapshot.providers.get_mut("openai").unwrap().models.insert(
            "other-private".into(),
            empty_model("other-private", "Private"),
        );
        source.definition_fingerprint = "a".repeat(64);
        rebind_credentials(&mut snapshot, &[source]).unwrap();
        assert!(!snapshot.providers["openai"]
            .models
            .contains_key("other-private"));
    }

    #[test]
    fn refresh_accepts_removals_and_retains_last_valid_models_after_bad_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for body in [
                r#"{"data":[{"id":"fresh-model","context_window":131072},{"id":"retired-model"}]}"#,
                r#"{"data":[{"id":"fresh-model","context_window":131072}]}"#,
                "{malformed",
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0u8; 4096];
                let read = socket.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /models "));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        let mut spec = builtin_spec("groq");
        spec.url = format!("http://{address}/models");
        spec.key = Some("test-key".to_owned());
        let root = tempfile::tempdir().unwrap();
        let first = refresh_with_specs(root.path(), None, &[spec.clone()]).unwrap();
        assert_eq!(
            first.provider_status["groq"].source,
            CatalogSource::OfficialApi
        );
        assert_eq!(
            first.providers["groq"].models["fresh-model"]
                .limit
                .as_ref()
                .unwrap()
                .context,
            Some(131072)
        );
        let second = refresh_with_specs(root.path(), Some(&first), &[spec.clone()]).unwrap();
        assert_eq!(second.providers["groq"].models.len(), 1);
        assert!(!second.providers["groq"]
            .models
            .contains_key("retired-model"));
        let third = refresh_with_specs(root.path(), Some(&second), &[spec]).unwrap();
        assert!(third.providers["groq"].models.contains_key("fresh-model"));
        assert!(third.provider_status["groq"].last_error.is_some());
        server.join().unwrap();
    }
}
