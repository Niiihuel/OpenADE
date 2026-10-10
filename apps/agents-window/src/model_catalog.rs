//! OpenIDE's bounded, provider-owned model metadata catalog.
//!
//! Listings come directly from each declared provider. Provider metadata is
//! descriptive; the request adapter decides which controls it can send.

use crate::native_provider::{self, Adapter, CatalogMapping, ProviderAuth, ProviderRegistry};
use reqwest::blocking::Client;
use reqwest::header::HeaderValue;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
    Arc,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const CACHE_FILE: &str = "provider-catalog.json";
const CATALOG_VERSION: u16 = 3;
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_MODELS_PER_PROVIDER: usize = 5_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const REFRESH_CANCELLED: &str = "provider catalog refresh cancelled";

/// Owned by the window requesting a refresh. Dropping it cancels remaining
/// providers and persistence without waiting for a request already in flight.
/// That request retains the transport's twelve-second aggregate deadline.
pub(crate) struct CatalogRefreshJob {
    receiver: Receiver<Result<CatalogSnapshot, String>>,
    cancel: Arc<AtomicBool>,
}

impl CatalogRefreshJob {
    pub(crate) fn start(
        root: PathBuf,
        previous: Option<Arc<CatalogSnapshot>>,
    ) -> Result<Self, String> {
        Self::spawn(move |cancel| {
            check_refresh_cancelled(cancel)?;
            let specs = official_specs(&root)?;
            refresh_with_specs_cancellable(&root, previous.as_deref(), &specs, cancel)
        })
    }

    fn spawn(
        work: impl FnOnce(&AtomicBool) -> Result<CatalogSnapshot, String> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        thread::Builder::new()
            .name("openide-provider-catalog".into())
            .spawn(move || {
                let result = work(&worker_cancel);
                if !worker_cancel.load(Ordering::Acquire) {
                    let _ = sender.send(result);
                }
            })
            .map_err(|error| format!("cannot start provider catalog refresh: {error}"))?;
        Ok(Self { receiver, cancel })
    }

    pub(crate) fn try_recv(&self) -> Result<Result<CatalogSnapshot, String>, TryRecvError> {
        self.receiver.try_recv()
    }

    #[cfg(test)]
    pub(crate) fn from_test_receiver(
        receiver: Receiver<Result<CatalogSnapshot, String>>,
    ) -> (Self, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        (Self { receiver, cancel: cancel.clone() }, cancel)
    }
}

impl Drop for CatalogRefreshJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn check_refresh_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err(REFRESH_CANCELLED.into())
    } else {
        Ok(())
    }
}

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
        self.provider_status.iter().any(|(id, status)| {
            // A reviewed-only source has no remote listing to refresh. It
            // stays explicitly unverified without scheduling useless probes.
            self.providers.get(id).is_some_and(|provider| provider.api.is_some())
                && (status.verified_at_ms == 0
                    || self.providers.get(id).is_some_and(needs_codex_listing_order)
                    || now_ms().saturating_sub(status.verified_at_ms) >= ttl_ms)
        })
    }

    /// Exact provider metadata. The adapter decides which controls and
    /// declared limits it can enforce for execution.
    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&ModelInfo> {
        self.providers.get(provider_id)?.models.get(model_id)
    }

    /// Remove account-specific metadata immediately when its credential
    /// changes. The next worker rebinds the persisted cache before using it.
    pub(crate) fn clear_account_models(&mut self, provider_id: &str) {
        if let Some(provider) = self.providers.get_mut(provider_id) {
            provider.models = reviewed_models(provider_id);
        }
        if let Some(status) = self.provider_status.get_mut(provider_id) {
            *status = ProviderCatalogStatus::default();
        }
        self.fetched_at_ms = self.provider_status.values()
            .map(|status| status.verified_at_ms).max().unwrap_or(0);
    }
}

/// Older caches preserved IDs but lost the server's ranking in a BTreeMap.
/// Keep their verified content available, while requesting a full new body.
fn needs_codex_listing_order(provider: &ProviderInfo) -> bool {
    provider.id == "codex"
        && provider.models.values().any(|model| model.listing_index.is_none())
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
    /// Server order among visible entries, not alphabetical or selected-first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_index: Option<usize>,
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
    /// Provider mappings own this response's limits even when all are unknown.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provider_limits_declared: bool,
    #[serde(default)]
    pub modalities: Option<ModelModalities>,
    /// USD per one million tokens, not the provider's raw per-token values.
    #[serde(default)]
    pub cost: Option<ModelCost>,
    /// Provider-reported effort values for this exact model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub provider_efforts_declared: bool,
    /// Partial provider declarations are not an exclusive modality list.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub declared_capabilities: BTreeMap<String, bool>,
    /// The response owns its configured capability fields even when they are
    /// omitted or invalid. Empty unknowns cannot inherit older capabilities.
    #[serde(default, skip_serializing_if = "is_false")]
    pub capabilities_declared: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

fn is_false(value: &bool) -> bool {
    !*value
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
    auth: ProviderAuth,
    headers: BTreeMap<String, String>,
    definition_fingerprint: String,
    /// A documented local seed is never presented as an authenticated listing.
    fetch_listing: bool,
}

fn official_specs(root: &Path) -> Result<Vec<SourceSpec>, String> {
    let mut specs = specs_from_registry(&ProviderRegistry::load(root)?)?;
    for spec in &mut specs {
        if !spec.anonymous {
            spec.key = crate::provider_connections::read_key(root, &spec.key_env);
        }
    }
    Ok(specs)
}

fn specs_from_registry(registry: &ProviderRegistry) -> Result<Vec<SourceSpec>, String> {
    registry
        .adapters()
        .iter()
        .filter_map(|adapter| {
            let path = adapter
                .catalog_list_path
                .as_ref()
                .or(adapter.model_list_path.as_ref());
            if let Some(path) = path {
                Some(spec_from_adapter(adapter, path))
            } else if native_provider::reviewed_native_models().iter().any(|model| model.provider_id == adapter.id) {
                Some(spec_from_adapter(adapter, "").map(|mut spec| {
                    spec.fetch_listing = false;
                    spec.url = adapter.endpoint.clone();
                    spec
                }))
            } else {
                None
            }
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
        auth: adapter.auth.clone(),
        headers: adapter.headers.clone(),
        definition_fingerprint: format!("{:x}", hash.finalize()),
        fetch_listing: true,
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
        entry.api = spec.fetch_listing.then(|| spec.url.clone());
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
        api: spec.fetch_listing.then(|| spec.url.clone()),
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
        listing_index: None,
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
        provider_limits_declared: false,
        modalities: None,
        cost: None,
        provider_efforts: Vec::new(),
        provider_efforts_declared: false,
        declared_capabilities: BTreeMap::new(),
        capabilities_declared: false,
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
    load_cache_with_specs(root, &specs)
}

fn load_cache_with_specs(
    root: &Path,
    specs: &[SourceSpec],
) -> Result<Option<CatalogSnapshot>, String> {
    let path = cache_path(root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(seed(specs))),
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
    reconcile_sources(&mut snapshot, specs);
    rebind_credentials(&mut snapshot, specs)?;
    validate_snapshot(&snapshot)?;
    Ok(Some(snapshot))
}

#[cfg(test)]
fn refresh_with_specs(
    root: &Path,
    previous: Option<&CatalogSnapshot>,
    specs: &[SourceSpec],
) -> Result<CatalogSnapshot, String> {
    refresh_with_specs_cancellable(root, previous, specs, &AtomicBool::new(false))
}

fn refresh_with_specs_cancellable(
    root: &Path,
    previous: Option<&CatalogSnapshot>,
    specs: &[SourceSpec],
    cancel: &AtomicBool,
) -> Result<CatalogSnapshot, String> {
    check_refresh_cancelled(cancel)?;
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
        check_refresh_cancelled(cancel)?;
        if !spec.fetch_listing {
            // No model-list operation is published for this route. Preserve
            // reviewed public IDs without sending a probe or claiming access.
            continue;
        }
        let attempt = now_ms();
        let mut status = snapshot
            .provider_status
            .get(&spec.id)
            .ok_or("provider catalog status is missing")?
            .clone();
        let needs_order = snapshot.providers.get(&spec.id).is_some_and(needs_codex_listing_order);
        if needs_order {
            status.etag = None;
        }
        let result = fetch_provider(&client, spec, &status).and_then(|response| match response {
            FetchResult::NotModified => {
                if needs_order {
                    return Err("HTTP 304 without the requested model ordering".into());
                }
                if status.source != CatalogSource::OfficialApi || status.verified_at_ms == 0 {
                    return Err("HTTP 304 without a verified provider catalog".into());
                }
                Ok((None, status.etag.clone()))
            }
            FetchResult::Models { models, etag } => Ok((Some(models), etag)),
        });
        check_refresh_cancelled(cancel)?;
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
    check_refresh_cancelled(cancel)?;
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
        return Err(format!("Conecta {} para actualizar sus modelos.", spec.name));
    }
    if key.is_some_and(|key| key.len() > 32_768 || key.chars().any(char::is_control)) {
        return Err(format!("La conexión de {} no es válida. Vuelve a conectarla.", spec.name));
    }
    match native_provider::fetch_model_listing_pages(
        client,
        &spec.url,
        &spec.auth,
        &spec.headers,
        key,
        &spec.mapping,
        previous.etag.as_deref().filter(|value| valid_etag(value)),
        MAX_RESPONSE_BYTES,
        REQUEST_TIMEOUT,
    )? {
        native_provider::ModelListingFetch::NotModified => Ok(FetchResult::NotModified),
        native_provider::ModelListingFetch::Pages { pages, etag } => {
            let mut models = BTreeMap::new();
            for page in pages {
                let page_offset = models.len();
                for (id, mut model) in parse_provider_models(spec, &page)? {
                    model.listing_index = model.listing_index.map(|index| page_offset + index);
                    if models.insert(id, model).is_some() {
                        return Err(
                            "provider model list contains duplicate IDs across pages".into()
                        );
                    }
                }
            }
            Ok(FetchResult::Models { models, etag })
        }
    }
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
        if !native_provider::catalog_entry_visible(entry, &spec.mapping) {
            continue;
        }
        let id = path_value(entry, spec.mapping.id_path.as_deref(), "id")
            .and_then(Value::as_str)
            .filter(|id| valid_text(id, 256))
            .ok_or("provider model ID is invalid")?;
        let mut model = parse_model(spec, id, entry);
        model.listing_index = Some(models.len());
        if models.insert(id.to_owned(), model).is_some()
        {
            return Err("provider model list contains duplicate IDs".into());
        }
    }
    Ok(models)
}

fn parse_model(spec: &SourceSpec, id: &str, entry: &Value) -> ModelInfo {
    let mut model = empty_model(id, id);
    model.provider_limits_declared = native_provider::mapped_limits_declared(&spec.mapping);
    model.name = path_value(entry, spec.mapping.name_path.as_deref(), "name")
        .and_then(Value::as_str)
        .filter(|name| valid_text(name, 256))
        .unwrap_or(id)
        .to_owned();
    model.description = entry
        .get("description")
        .and_then(Value::as_str)
        .filter(|text| {
            !text.trim().is_empty()
                && text.len() <= 4_096
                && text
                    .chars()
                    .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        })
        .map(str::to_owned);
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
    let input = positive_u64(path_value(
        entry,
        spec.mapping.input_limit_path.as_deref(),
        "",
    ));
    if context.is_some() || output.is_some() || input.is_some() {
        model.limit = Some(ModelLimit {
            context,
            input,
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
    model.provider_efforts = native_provider::mapped_effort_levels(entry, &spec.mapping);
    model.provider_efforts_declared =
        native_provider::mapped_efforts_declared(entry, &spec.mapping);
    model.declared_capabilities = native_provider::mapped_capabilities(entry, &spec.mapping);
    model.capabilities_declared =
        native_provider::mapped_capabilities_declared(entry, &spec.mapping);
    model.reasoning = !model.provider_efforts.is_empty()
        || model.declared_capabilities.get("thinking") == Some(&true);
    model.structured_output = model.declared_capabilities.get("structured_outputs") == Some(&true);
    model.attachment = model.declared_capabilities.get("image_input") == Some(&true)
        || model.declared_capabilities.get("pdf_input") == Some(&true);
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
                .any(|(id, model)| id != &model.id || !valid_text(id, 256)
                    || model.listing_index.is_some_and(|index| index >= MAX_MODELS_PER_PROVIDER))
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
    fn codex_catalog_order_survives_cache_and_legacy_order_refresh_does_not_reintroduce_omitted_models() {
        let first_body = native_provider::tests::codex_ordered_listing();
        let next_body = serde_json::json!({"models":[
            {"slug":"model-middle","display_name":"Updated Middle","visibility":"list"},
            {"slug":"model-alpha","display_name":"Updated Alpha","visibility":"list"},
            {"slug":"model-zulu","display_name":"Now hidden","visibility":"hide"}
        ]});
        let (endpoint, requests, server) = native_provider::tests::listing_server(vec![
            (200, first_body.to_string()),
            (200, next_body.to_string()),
            (200, serde_json::json!({"models":[]}).to_string()),
        ]);
        let mut adapter = native_provider::adapter("codex").unwrap();
        adapter.endpoint = endpoint;
        let mut spec = spec_from_adapter(&adapter, "models").unwrap();
        spec.key = Some("fixture-catalog-token".into());
        let specs = std::slice::from_ref(&spec);
        let root = tempfile::tempdir().unwrap();
        let first = refresh_with_specs(root.path(), None, specs).unwrap();
        let restored = load_cache_with_specs(root.path(), specs).unwrap().unwrap();
        assert_eq!(first, restored);
        assert!(!restored.is_stale(Duration::from_secs(3600)));
        for (index, id) in ["model-zulu", "model-alpha", "model-middle", "model-beta"].iter().enumerate() {
            assert_eq!(restored.providers["codex"].models[*id].listing_index, Some(index));
        }
        assert_eq!(restored.providers["codex"].models.len(), 4);
        assert_eq!(restored.provider_status["codex"].source, CatalogSource::OfficialApi);

        // Simulate an on-disk cache written before ordering was persisted.
        // It stays readable, but a conditional 304 cannot repair its order.
        let mut legacy_json = serde_json::to_value(&restored).unwrap();
        for model in legacy_json["providers"]["codex"]["models"].as_object_mut().unwrap().values_mut() {
            model.as_object_mut().unwrap().remove("listing_index");
        }
        fs::write(cache_path(root.path()), serde_json::to_vec(&legacy_json).unwrap()).unwrap();
        let legacy = load_cache_with_specs(root.path(), specs).unwrap().unwrap();
        assert!(legacy.is_stale(Duration::from_secs(3600)));
        assert!(legacy.provider_status["codex"].etag.is_some());
        let updated = refresh_with_specs(root.path(), Some(&legacy), specs).unwrap();
        let models = &updated.providers["codex"].models;
        assert_eq!(models.len(), 2);
        assert_eq!(models["model-middle"].listing_index, Some(0));
        assert_eq!(models["model-alpha"].listing_index, Some(1));
        assert_eq!(models["model-middle"].name, "Updated Middle");
        assert!(!models.contains_key("model-zulu"));
        assert!(!models.contains_key("model-beta"));
        let empty = refresh_with_specs(root.path(), Some(&updated), specs).unwrap();
        assert!(empty.providers["codex"].models.is_empty());
        assert_eq!(empty.provider_status["codex"].source, CatalogSource::OfficialApi);
        assert!(empty.provider_status["codex"].verified_at_ms > 0);
        assert!(load_cache_with_specs(root.path(), specs).unwrap().unwrap().providers["codex"].models.is_empty());
        let _first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let migrated = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!migrated.to_ascii_lowercase().contains("if-none-match:"));
        let _last = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
        assert!(!fs::read_to_string(cache_path(root.path())).unwrap().contains("fixture-catalog-token"));
    }

    #[test]
    fn zai_reviewed_catalog_never_probes_models_or_claims_authenticated_availability() {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut spec = builtin_spec("zai");
        assert!(!spec.fetch_listing);
        spec.url = format!("http://{}/must-not-request-models", listener.local_addr().unwrap());
        spec.key = Some("fixture-key".into());
        let snapshot = refresh_with_specs(root.path(), None, &[spec.clone()]).unwrap();
        assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        let status = &snapshot.provider_status["zai"];
        assert_eq!(status.source, CatalogSource::ReviewedSeed);
        assert_eq!(status.verified_at_ms, 0);
        assert_eq!(status.attempted_at_ms, 0);
        assert!(status.last_error.is_none());
        assert!(snapshot.providers["zai"].api.is_none());
        assert!(!snapshot.is_stale(Duration::from_secs(3_600)));
        assert!(snapshot.model("zai", "glm-5.3").unwrap().tool_call);
        assert_eq!(snapshot.model("zai", "glm-5.2").unwrap().limit.as_ref().unwrap().context, Some(1_000_000));
        let restored = load_cache_with_specs(root.path(), &[spec]).unwrap().unwrap();
        assert_eq!(restored.providers["zai"].models, snapshot.providers["zai"].models);
        assert_eq!(restored.provider_status["zai"].verified_at_ms, 0);
    }

    #[test]
    fn catalog_is_stale_until_all_provider_checks_are_recent() {
        let mut snapshot = seed(&[builtin_spec("groq"), builtin_spec("mistral"), builtin_spec("zai")]);
        let ttl = Duration::from_secs(3_600);
        assert!(snapshot.is_stale(ttl));
        for (id, status) in &mut snapshot.provider_status {
            if id != "zai" {
                status.verified_at_ms = now_ms();
            }
        }
        assert!(!snapshot.is_stale(ttl));
        snapshot
            .provider_status
            .get_mut("groq")
            .unwrap()
            .verified_at_ms = now_ms().saturating_sub(3_600_001);
        assert!(snapshot.is_stale(ttl));
    }

    #[test]
    fn dropping_refresh_job_skips_remaining_provider_and_preserves_cache_after_gated_http() {
        let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        second_listener.set_nonblocking(true).unwrap();
        let mut first = builtin_spec("groq");
        first.url = format!("http://{}/models", first_listener.local_addr().unwrap());
        first.key = Some("fixture-catalog-key".into());
        let mut second = builtin_spec("mistral");
        second.url = format!("http://{}/models", second_listener.local_addr().unwrap());
        second.key = Some("fixture-catalog-key".into());
        let specs = vec![first, second];
        let root = tempfile::tempdir().unwrap();
        let previous = seed(&specs);
        write_cache(root.path(), &previous).unwrap();
        let before = fs::read(cache_path(root.path())).unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = first_listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /models "));
            accepted_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let body = r#"{"data":[{"id":"new-model"}]}"#;
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ).as_bytes()).unwrap();
        });
        let (finished_tx, finished_rx) = mpsc::channel();
        let refresh_root = root.path().to_owned();
        let job = CatalogRefreshJob::spawn(move |cancel| {
            let result =
                refresh_with_specs_cancellable(&refresh_root, Some(&previous), &specs, cancel);
            finished_tx.send(result.clone()).unwrap();
            result
        })
        .unwrap();
        accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let cancel = job.cancel.clone();
        // HTTP remains blocked by the server until after drop has returned.
        // No runtime join, socket wait, or network timeout belongs to Drop.
        drop(job);
        assert!(cancel.load(Ordering::Acquire));
        assert!(matches!(finished_rx.try_recv(), Err(TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        assert_eq!(
            finished_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(REFRESH_CANCELLED.into()),
        );
        server.join().unwrap();
        assert!(matches!(
            second_listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert_eq!(fs::read(cache_path(root.path())).unwrap(), before);
        assert!(!fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .any(|entry| { entry.file_name().to_string_lossy().ends_with(".tmp") }));
    }

    #[test]
    fn cancelled_refresh_does_not_start_transport_or_create_cache_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("not-created");
        let cancel = AtomicBool::new(true);
        assert_eq!(
            refresh_with_specs_cancellable(&directory, None, &[builtin_spec("groq")], &cancel),
            Err(REFRESH_CANCELLED.into()),
        );
        assert!(!directory.exists());
    }

    #[test]
    fn active_refresh_job_delivers_catalog_and_persists_valid_cache() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut spec = builtin_spec("groq");
        spec.url = format!("http://{}/models", listener.local_addr().unwrap());
        spec.key = Some("fixture-catalog-key".into());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            let body = r#"{"data":[{"id":"fresh-model"}]}"#;
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ).as_bytes()).unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let refresh_root = root.path().to_owned();
        let job = CatalogRefreshJob::spawn(move |cancel| {
            refresh_with_specs_cancellable(&refresh_root, None, &[spec], cancel)
        })
        .unwrap();
        let result = job
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(result.providers["groq"].models.contains_key("fresh-model"));
        assert_eq!(
            result.provider_status["groq"].source,
            CatalogSource::OfficialApi
        );
        let reopened: CatalogSnapshot =
            serde_json::from_slice(&fs::read(cache_path(root.path())).unwrap()).unwrap();
        assert_eq!(reopened, result);
        server.join().unwrap();
    }

    #[test]
    fn registry_provider_additions_refresh_and_reopen_without_a_name_allowlist() {
        let (endpoint, requests, server) = native_provider::tests::listing_server(
            native_provider::tests::complete_listing_pages(),
        );
        let mut adapter = native_provider::tests::anthropic_adapter(endpoint);
        // This gateway explicitly publishes anonymous model metadata. The
        // registry, rather than a built-in account name, determines that policy.
        adapter.catalog_anonymous = true;
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("provider-registry.json"),
            serde_json::to_vec(&serde_json::json!({
                "version":1,"providers":[adapter]
            }))
            .unwrap(),
        )
        .unwrap();
        let spec = official_specs(root.path())
            .unwrap()
            .into_iter()
            .find(|spec| spec.id == "new-gateway")
            .expect("a valid registered provider must be discovered independently of its name");
        assert!(spec.anonymous);
        assert_eq!(spec.key, None);
        assert_eq!(spec.auth, adapter.auth);
        assert_eq!(spec.headers, adapter.headers);
        let snapshot = refresh_with_specs(root.path(), None, &[spec]).unwrap();
        assert_eq!(
            snapshot.provider_status["new-gateway"].source,
            CatalogSource::OfficialApi
        );
        assert_eq!(snapshot.providers["new-gateway"].models.len(), 2);
        // Exercise the normal loader's current-registry reconciliation, rather
        // than reopening with a manually supplied SourceSpec.
        let reopened = load_cache(root.path()).unwrap().unwrap();
        assert_eq!(
            reopened.providers["new-gateway"],
            snapshot.providers["new-gateway"]
        );
        assert_eq!(
            reopened.provider_status["new-gateway"],
            snapshot.provider_status["new-gateway"]
        );
        for _ in 0..2 {
            let request = requests
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .to_ascii_lowercase();
            assert!(!request.contains("x-api-key:"));
            assert!(!request.contains("authorization:"));
        }
        server.join().unwrap();
    }

    #[test]
    fn anthropic_paginated_catalog_reopens_exact_capabilities_without_serialized_credentials() {
        let (endpoint, requests, server) = native_provider::tests::listing_server(
            native_provider::tests::complete_listing_pages(),
        );
        let adapter = native_provider::tests::anthropic_adapter(endpoint);
        let mut spec = spec_from_adapter(&adapter, "models").unwrap();
        spec.key = Some("catalog-fixture-secret".into());
        let root = tempfile::tempdir().unwrap();
        let snapshot = refresh_with_specs(root.path(), None, std::slice::from_ref(&spec)).unwrap();
        let status = &snapshot.provider_status["new-gateway"];
        assert_eq!(status.source, CatalogSource::OfficialApi);
        assert!(status.verified_at_ms > 0);
        assert!(status.last_error.is_none());
        // A page-local ETag cannot certify the other pages on a later refresh.
        assert_eq!(status.etag, None);
        let models = &snapshot.providers["new-gateway"].models;
        assert_eq!(models.len(), 2);
        let model = &models["opaque-model"];
        assert_eq!(model.limit.as_ref().unwrap().input, Some(65536));
        assert_eq!(model.limit.as_ref().unwrap().context, None);
        assert_eq!(model.limit.as_ref().unwrap().output, Some(4096));
        assert_eq!(model.provider_efforts, ["low", "turbo_2028", "xhigh"]);
        assert!(model.provider_efforts_declared);
        assert!(model.capabilities_declared);
        assert!(model.reasoning);
        assert!(model.attachment);
        assert!(model.structured_output);
        assert!(model.declared_capabilities["image_input"]);
        assert!(!model.declared_capabilities["pdf_input"]);
        assert_eq!(model.tool_call_declared, None);
        assert_eq!(model.modalities, None);
        assert_eq!(model.cost, None);
        let adjacent = &models["opaque-model-preview"];
        assert_eq!(adjacent.limit, None);
        assert!(adjacent.provider_efforts.is_empty());
        assert!(adjacent.provider_efforts_declared);
        assert!(adjacent.capabilities_declared);
        assert!(!adjacent.reasoning);
        assert!(!adjacent.attachment);
        assert!(!models.contains_key("opaque-model-latest"));
        let budget =
            crate::app::native_token_budget(&adapter, "opaque-model", Some(&snapshot), &[])
                .unwrap();
        assert_eq!(budget.input_limit_tokens, Some(65536));
        assert_eq!(budget.context_window_tokens, 0);
        assert_eq!(budget.output_reserve_tokens, 4096);
        assert!(crate::app::native_token_budget(
            &adapter,
            "opaque-model-preview",
            Some(&snapshot),
            &[]
        )
        .is_none());
        assert!(crate::app::native_image_input_supported(
            &adapter,
            "opaque-model",
            Some(&snapshot),
            &[]
        ));
        assert!(!crate::app::native_image_input_supported(
            &adapter,
            "opaque-model-preview",
            Some(&snapshot),
            &[]
        ));
        for page in 0..2 {
            let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            let request = request.to_ascii_lowercase();
            assert!(request.contains("x-api-key: catalog-fixture-secret\r\n"));
            assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("if-none-match:"));
            assert_eq!(request.contains("after_id=opaque-model"), page == 1);
        }
        server.join().unwrap();
        let cache = fs::read(cache_path(root.path())).unwrap();
        assert!(!String::from_utf8_lossy(&cache).contains("catalog-fixture-secret"));
        let reopened = load_cache_with_specs(root.path(), std::slice::from_ref(&spec))
            .unwrap()
            .unwrap();
        assert_eq!(reopened.providers, snapshot.providers);
        assert_eq!(reopened.provider_status, snapshot.provider_status);
    }

    #[test]
    fn paginated_refresh_failure_preserves_complete_prior_catalog_after_reopen() {
        let mut responses = native_provider::tests::complete_listing_pages();
        responses.extend([
            (
                200,
                serde_json::json!({"data":[{"id":"partial-new-model"}],"has_more":true,
                    "last_id":"partial-new-model"})
                .to_string(),
            ),
            (
                401,
                "body must never be published: catalog-fixture-secret".into(),
            ),
        ]);
        let (endpoint, requests, server) = native_provider::tests::listing_server(responses);
        let adapter = native_provider::tests::anthropic_adapter(endpoint);
        let mut spec = spec_from_adapter(&adapter, "models").unwrap();
        spec.key = Some("catalog-fixture-secret".into());
        let root = tempfile::tempdir().unwrap();
        let first = refresh_with_specs(root.path(), None, std::slice::from_ref(&spec)).unwrap();
        let prior_models = first.providers["new-gateway"].models.clone();
        let prior_verified = first.provider_status["new-gateway"].verified_at_ms;
        let updated =
            refresh_with_specs(root.path(), Some(&first), std::slice::from_ref(&spec)).unwrap();
        assert_eq!(updated.providers["new-gateway"].models, prior_models);
        assert!(!updated.providers["new-gateway"]
            .models
            .contains_key("partial-new-model"));
        let status = &updated.provider_status["new-gateway"];
        assert_eq!(status.source, CatalogSource::OfficialApi);
        assert_eq!(status.verified_at_ms, prior_verified);
        assert_eq!(status.last_error.as_deref(), Some("HTTP 401"));
        assert_eq!(status.etag, None);
        for _ in 0..4 {
            assert!(!requests
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .to_ascii_lowercase()
                .contains("if-none-match:"));
        }
        server.join().unwrap();
        let reopened = load_cache_with_specs(root.path(), std::slice::from_ref(&spec))
            .unwrap()
            .unwrap();
        assert_eq!(reopened.providers["new-gateway"].models, prior_models);
        assert_eq!(reopened.provider_status["new-gateway"], *status);
        assert!(!fs::read_to_string(cache_path(root.path()))
            .unwrap()
            .contains("catalog-fixture-secret"));
    }

    #[test]
    fn refreshed_empty_declarations_replace_stale_capabilities_and_reopen_as_unknown() {
        let mut responses = native_provider::tests::complete_listing_pages();
        responses.push(native_provider::tests::authoritative_empty_listing());
        responses.push((
            200,
            serde_json::json!({"data":[{"id":"opaque-model"}],"has_more":false}).to_string(),
        ));
        let (endpoint, _, server) = native_provider::tests::listing_server(responses);
        let adapter = native_provider::tests::anthropic_adapter(endpoint);
        let mut spec = spec_from_adapter(&adapter, "models").unwrap();
        spec.key = Some("catalog-fixture-secret".into());
        let root = tempfile::tempdir().unwrap();
        let first = refresh_with_specs(root.path(), None, std::slice::from_ref(&spec)).unwrap();
        assert!(
            first.providers["new-gateway"].models["opaque-model"].declared_capabilities
                ["image_input"]
        );
        let updated =
            refresh_with_specs(root.path(), Some(&first), std::slice::from_ref(&spec)).unwrap();
        let fresh = &updated.providers["new-gateway"].models["opaque-model"];
        assert!(fresh.provider_efforts_declared);
        assert!(fresh.capabilities_declared);
        assert!(fresh.provider_limits_declared);
        assert_eq!(fresh.limit, None);
        assert!(fresh.provider_efforts.is_empty());
        assert!(!fresh.declared_capabilities.contains_key("image_input"));
        assert!(!fresh.attachment);
        assert!(
            crate::app::native_token_budget(&adapter, "opaque-model", Some(&updated), &[])
                .is_none()
        );
        assert_eq!(
            crate::app::native_output_maximum(&adapter, "opaque-model", Some(&updated), &[]),
            None
        );
        assert!(crate::app::native_request_capabilities(
            &adapter,
            "opaque-model",
            Some(&updated),
            &[]
        )
        .reasoning_efforts
        .is_empty());
        assert!(!crate::app::native_image_input_supported(
            &adapter,
            "opaque-model",
            Some(&updated),
            &[]
        ));
        let reopened = load_cache_with_specs(root.path(), std::slice::from_ref(&spec))
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.providers["new-gateway"].models["opaque-model"],
            *fresh
        );
        let omitted =
            refresh_with_specs(root.path(), Some(&updated), std::slice::from_ref(&spec)).unwrap();
        let unknown = &omitted.providers["new-gateway"].models["opaque-model"];
        assert!(unknown.provider_limits_declared);
        assert!(unknown.capabilities_declared);
        assert!(!unknown.provider_efforts_declared);
        assert_eq!(unknown.limit, None);
        assert!(unknown.declared_capabilities.is_empty());
        assert!(
            crate::app::native_token_budget(&adapter, "opaque-model", Some(&omitted), &[])
                .is_none()
        );
        assert_eq!(
            crate::app::native_output_maximum(&adapter, "opaque-model", Some(&omitted), &[]),
            None
        );
        assert!(crate::app::native_request_capabilities(
            &adapter,
            "opaque-model",
            Some(&omitted),
            &[]
        )
        .reasoning_efforts
        .is_empty());
        assert!(!crate::app::native_image_input_supported(
            &adapter,
            "opaque-model",
            Some(&omitted),
            &[]
        ));
        let reopened = load_cache_with_specs(root.path(), std::slice::from_ref(&spec))
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.providers["new-gateway"].models["opaque-model"],
            *unknown
        );
        assert!(!fs::read_to_string(cache_path(root.path()))
            .unwrap()
            .contains("catalog-fixture-secret"));
        // Old cache rows deserialize conservatively and never pretend that an
        // empty family was declared by a provider that supplied no such field.
        let old_row: ModelInfo =
            serde_json::from_value(serde_json::json!({"id":"old","name":"Old"})).unwrap();
        assert!(!old_row.provider_efforts_declared);
        assert!(!old_row.capabilities_declared);
        assert!(!old_row.provider_limits_declared);
        server.join().unwrap();
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
    #[ignore = "calls the public OpenRouter metadata API; no inference or credentials"]
    fn live_openrouter_catalog_exposes_exact_limits_and_reopens_cache() {
        let root = tempfile::tempdir().unwrap();
        let spec = builtin_spec("openrouter");
        let snapshot = refresh_with_specs(root.path(), None, &[spec]).unwrap();
        let status = &snapshot.provider_status["openrouter"];
        assert_eq!(
            status.source,
            CatalogSource::OfficialApi,
            "{:?}",
            status.last_error
        );
        assert!(status.verified_at_ms > 0);
        assert!(status.last_error.is_none());
        let reopened = load_cache(root.path()).unwrap().unwrap();
        assert_eq!(
            reopened.providers["openrouter"],
            snapshot.providers["openrouter"]
        );
        let adapter = ProviderRegistry::builtin()
            .adapter("openrouter")
            .unwrap()
            .clone();
        let budgets: Vec<_> = reopened.providers["openrouter"]
            .models
            .keys()
            .filter_map(|id| {
                crate::app::native_token_budget(&adapter, id, Some(&reopened), &[])
                    .map(|budget| (id, budget))
            })
            .collect();
        assert!(!budgets.is_empty());
        println!(
            "OpenRouter metadata: {} exact model IDs; {} tool-capable IDs with context/output pairs",
            reopened.providers["openrouter"].models.len(),
            budgets.len()
        );
        for (id, budget) in budgets.iter().take(3) {
            println!(
                "{id}: context={} output={}",
                budget.context_window_tokens, budget.output_reserve_tokens
            );
        }
        assert!(crate::app::native_token_budget(
            &adapter,
            "openide/nonexistent-test-model",
            Some(&reopened),
            &[]
        )
        .is_none());
    }

    #[test]
    fn refresh_accepts_removals_and_retains_last_valid_models_after_bad_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for body in [
                r#"{"data":[{"id":"fresh-model","context_window":131072,"max_completion_tokens":8192},{"id":"retired-model"}]}"#,
                r#"{"data":[{"id":"fresh-model","context_window":65536,"max_completion_tokens":4096}]}"#,
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
        let adapter = ProviderRegistry::builtin().adapter("groq").unwrap().clone();
        let first_budget =
            crate::app::native_token_budget(&adapter, "fresh-model", Some(&first), &[]).unwrap();
        assert_eq!(first_budget.context_window_tokens, 131072);
        assert_eq!(first_budget.output_reserve_tokens, 8192);
        let mut unverified = first.clone();
        unverified.provider_status.get_mut("groq").unwrap().source = CatalogSource::ReviewedSeed;
        assert!(
            crate::app::native_token_budget(&adapter, "fresh-model", Some(&unverified), &[])
                .is_none()
        );
        let second = refresh_with_specs(root.path(), Some(&first), &[spec.clone()]).unwrap();
        assert_eq!(second.providers["groq"].models.len(), 1);
        assert!(!second.providers["groq"]
            .models
            .contains_key("retired-model"));
        let second_budget =
            crate::app::native_token_budget(&adapter, "fresh-model", Some(&second), &[]).unwrap();
        assert_eq!(second_budget.context_window_tokens, 65536);
        assert_eq!(second_budget.output_reserve_tokens, 4096);
        // Read the serialized cache from a newly opened file; no in-memory
        // snapshot participates in recovery of these exact limits.
        let reopened: CatalogSnapshot =
            serde_json::from_slice(&fs::read(cache_path(root.path())).unwrap()).unwrap();
        assert_eq!(
            crate::app::native_token_budget(&adapter, "fresh-model", Some(&reopened), &[]),
            Some(second_budget)
        );
        let third = refresh_with_specs(root.path(), Some(&second), &[spec]).unwrap();
        assert!(third.providers["groq"].models.contains_key("fresh-model"));
        assert!(third.provider_status["groq"].last_error.is_some());
        assert_eq!(
            crate::app::native_token_budget(&adapter, "fresh-model", Some(&third), &[]),
            Some(second_budget)
        );
        server.join().unwrap();

        // The same text request is accepted with the first API limits and
        // stopped before HTTP with the smaller limits reopened from cache.
        let inference = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", inference.local_addr().unwrap());
        let inference_server = thread::spawn(move || {
            let (mut socket, _) = inference.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            std::io::BufRead::read_line(&mut reader, &mut first).unwrap();
            assert!(first.starts_with("POST /chat/completions "));
            let mut length = 0;
            loop {
                let mut header = String::new();
                std::io::BufRead::read_line(&mut reader, &mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["model"], "fresh-model");
            assert_eq!(
                request["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .len(),
                80_000
            );
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Accepted\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let model = std::sync::Arc::new(
            openide_harness::OpenAiCompatibleBackend::new(endpoint, None, "fresh-model")
                .with_capabilities(crate::app::native_request_capabilities(
                    &adapter,
                    "fresh-model",
                    Some(&first),
                    &[],
                ))
                .unwrap(),
        );
        for (policy, should_complete) in [(first_budget, true), (second_budget, false)] {
            let runner = openide_harness::AgentRunner::new(
                model.clone(),
                std::sync::Arc::new(openide_harness::LocalTools::new()),
            )
            .with_token_budget_policy(policy);
            let handle = runner.start(openide_core::AgentRunRequest {
                session_id: None,
                sequence_start: 0,
                workspace: root.path().to_path_buf(),
                history: vec![],
                prompt: "x".repeat(80_000),
                mode: openide_core::TurnMode::Plan,
            });
            loop {
                let event = handle.events.recv_timeout(Duration::from_secs(15)).unwrap();
                match event.kind {
                    openide_core::AgentEventKind::TurnCompleted { .. } => {
                        assert!(should_complete);
                        break;
                    }
                    openide_core::AgentEventKind::TurnFailed { error } => {
                        assert!(!should_complete, "{error}");
                        assert!(error.contains("semantic compaction"), "{error}");
                        break;
                    }
                    _ => {}
                }
            }
        }
        inference_server.join().unwrap();
    }
}
