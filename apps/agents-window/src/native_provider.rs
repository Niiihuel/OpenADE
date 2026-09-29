//! Persistent connection choice and local provider registry for the native harness.
//!
//! Provider endpoints and credential variable names come only from the bundled
//! definitions or an explicit local `provider-registry.json`. Model listings
//! cannot introduce an endpoint or authorize an effort level by model prefix.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use openide_harness::TokenBudgetPolicy;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SETTINGS_FILE: &str = "native-provider.json";
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const DEFAULT_MODEL: &str = "gpt-4.1";
const DEFAULT_CUSTOM_ENDPOINT: &str = "https://api.openai.com/v1";
const REGISTRY_FILE: &str = "provider-registry.json";
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_PROVIDERS: usize = 128;
const MAX_DISCOVERY_BYTES: u64 = 4 * 1024 * 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Adapter {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub key_env: String,
    /// The request protocol implemented by this adapter, not a promise that
    /// every model exposed by its provider supports our tool loop.
    pub wire_api: NativeWireApi,
    pub model_list_path: Option<String>,
    #[serde(default)]
    pub catalog_list_path: Option<String>,
    #[serde(default)]
    pub catalog_anonymous: bool,
    #[serde(default)]
    pub chat_request_fields: BTreeMap<String, Value>,
    /// Optional Chat Completions field for an explicitly supported effort
    /// control. A model listing alone does not authorize sending this field.
    #[serde(default)]
    pub reasoning_effort_field: Option<String>,
    #[serde(default)]
    pub user_endpoint: bool,
    #[serde(default)]
    pub key_optional: bool,
    #[serde(default)]
    pub catalog: CatalogMapping,
}

/// Declarative paths in a provider-owned JSON model listing. A missing path
/// means the listing makes no claim; it never means a capability is supported.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CatalogMapping {
    pub entries_path: Option<String>,
    pub id_path: Option<String>,
    pub name_path: Option<String>,
    pub context_window_path: Option<String>,
    pub max_output_path: Option<String>,
    pub tool_call_path: Option<String>,
    pub tool_call_contains: Option<String>,
    pub effort_levels_path: Option<String>,
    pub input_modalities_path: Option<String>,
    pub output_modalities_path: Option<String>,
    pub pricing_input_path: Option<String>,
    pub pricing_output_path: Option<String>,
    pub price_unit: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRegistryFile {
    version: u32,
    providers: Vec<Adapter>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderRegistry {
    adapters: Vec<Adapter>,
}

impl ProviderRegistry {
    pub fn builtin() -> Self {
        let file: ProviderRegistryFile =
            serde_json::from_str(include_str!("provider-definitions.json"))
                .expect("bundled provider definitions must be valid");
        assert_eq!(file.version, 1, "bundled provider registry version");
        Self {
            adapters: file.providers,
        }
    }

    pub fn load(root: &Path) -> Result<Self, String> {
        let mut registry = Self::builtin();
        let path = root.join(REGISTRY_FILE);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(registry),
            Err(error) => {
                return Err(format!(
                    "No se pudo leer el registro de proveedores: {error}"
                ))
            }
        };
        if metadata.len() > MAX_REGISTRY_BYTES {
            return Err("El registro de proveedores excede el tamaño permitido".to_owned());
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("No se pudo leer el registro de proveedores: {error}"))?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err("El registro de proveedores excede el tamaño permitido".to_owned());
        }
        let file: ProviderRegistryFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Registro de proveedores inválido: {error}"))?;
        if file.version != 1 {
            return Err(format!(
                "Versión de registro de proveedores no compatible: {}",
                file.version
            ));
        }
        if registry.adapters.len() + file.providers.len() > MAX_PROVIDERS {
            return Err("Demasiados proveedores configurados".to_owned());
        }
        let mut ids: HashSet<String> = registry
            .adapters
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        let mut used_keys: HashSet<String> = registry
            .adapters
            .iter()
            .map(|entry| entry.key_env.clone())
            .collect();
        for mut entry in file.providers {
            validate_provider_definition(&entry)?;
            if entry.user_endpoint || entry.key_optional {
                return Err(format!(
                    "El proveedor {} debe fijar un endpoint y una variable de credencial propios",
                    entry.id
                ));
            }
            if !ids.insert(entry.id.clone()) {
                return Err(format!("Proveedor duplicado o reservado: {}", entry.id));
            }
            if !used_keys.insert(entry.key_env.clone()) {
                return Err(format!(
                    "La variable {} ya pertenece a otro proveedor",
                    entry.key_env
                ));
            }
            entry.endpoint = validate_custom_endpoint(&entry.endpoint)?;
            registry.adapters.push(entry);
        }
        Ok(registry)
    }

    pub fn adapters(&self) -> &[Adapter] {
        &self.adapters
    }

    pub fn adapter(&self, provider_id: &str) -> Option<&Adapter> {
        self.adapters.iter().find(|entry| entry.id == provider_id)
    }
}

/// Compatibility lookup for built-ins. Runtime user additions require the
/// explicit data root through `ProviderRegistry::load` or `adapter_at`.
pub(crate) fn adapter(provider_id: &str) -> Option<Adapter> {
    ProviderRegistry::builtin().adapter(provider_id).cloned()
}

pub(crate) fn adapter_at(root: &Path, provider_id: &str) -> Result<Option<Adapter>, String> {
    Ok(ProviderRegistry::load(root)?.adapter(provider_id).cloned())
}

fn validate_provider_definition(entry: &Adapter) -> Result<(), String> {
    if entry.id.is_empty()
        || entry.id.len() > 64
        || !entry.id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
    {
        return Err("ID de proveedor inválido".to_owned());
    }
    if entry.name.is_empty() || entry.name.len() > 128 || entry.name.chars().any(char::is_control) {
        return Err(format!("Nombre inválido para {}", entry.id));
    }
    if entry.key_env.is_empty()
        || entry.key_env.len() > 128
        || !entry
            .key_env
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(format!("Variable de credencial inválida para {}", entry.id));
    }
    if let Some(path) = &entry.model_list_path {
        if path.is_empty()
            || path.len() > 256
            || path.starts_with('/')
            || path.split('/').any(|segment| {
                segment.is_empty()
                    || segment == "."
                    || segment == ".."
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            })
        {
            return Err(format!("Ruta de listado inválida para {}", entry.id));
        }
    }
    if let Some(path) = &entry.catalog_list_path {
        let (resource_path, query) = path
            .split_once('?')
            .map_or((path.as_str(), None), |(path, query)| (path, Some(query)));
        if resource_path.is_empty()
            || resource_path.len() > 256
            || resource_path.starts_with('/')
            || resource_path.split('/').any(|segment| {
                segment.is_empty()
                    || segment == "."
                    || segment == ".."
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            })
            || query.is_some_and(|query| {
                query.is_empty()
                    || query.len() > 128
                    || !query.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'=' | b'&')
                    })
            })
        {
            return Err(format!(
                "Ruta pública de listado inválida para {}",
                entry.id
            ));
        }
    }
    for path in [
        &entry.catalog.entries_path,
        &entry.catalog.id_path,
        &entry.catalog.name_path,
        &entry.catalog.context_window_path,
        &entry.catalog.max_output_path,
        &entry.catalog.tool_call_path,
        &entry.catalog.effort_levels_path,
        &entry.catalog.input_modalities_path,
        &entry.catalog.output_modalities_path,
        &entry.catalog.pricing_input_path,
        &entry.catalog.pricing_output_path,
    ]
    .into_iter()
    .flatten()
    {
        if path.is_empty()
            || path.len() > 128
            || path.split('.').any(|segment| {
                segment.is_empty()
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        {
            return Err(format!("Ruta de metadatos inválida para {}", entry.id));
        }
    }
    if entry
        .catalog
        .tool_call_contains
        .as_ref()
        .is_some_and(|value| {
            value.is_empty() || value.len() > 64 || value.chars().any(char::is_control)
        })
    {
        return Err(format!("Valor de herramienta inválido para {}", entry.id));
    }
    if entry.catalog.price_unit.as_ref().is_some_and(|unit| {
        !matches!(
            unit.as_str(),
            "per_token" | "per_million_tokens" | "usd_cents_per_100m_tokens"
        )
    }) {
        return Err(format!("Unidad de precio inválida para {}", entry.id));
    }
    if entry.chat_request_fields.len() > 16
        || entry.chat_request_fields.iter().any(|(field, value)| {
            field.is_empty()
                || field.len() > 64
                || !field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || matches!(
                    field.as_str(),
                    "model" | "messages" | "tools" | "stream" | "reasoning_effort"
                )
                || serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() > 8 * 1024)
        })
    {
        return Err(format!("Campos de petición inválidos para {}", entry.id));
    }
    if entry
        .reasoning_effort_field
        .as_deref()
        .is_some_and(|field| {
            field != "reasoning_effort" || entry.wire_api != NativeWireApi::ChatCompletions
        })
    {
        return Err(format!("Campo de esfuerzo inválido para {}", entry.id));
    }
    validate_custom_endpoint(&entry.endpoint)?;
    Ok(())
}

/// Presence is only a local preflight check. It does not prove that the key
/// is valid or that the account can use a particular model.
pub(crate) fn adapter_key_present(provider_id: &str) -> bool {
    adapter_key_present_with(provider_id, |name| std::env::var(name).ok())
}

fn adapter_key_present_with(
    provider_id: &str,
    read_key: impl FnOnce(&str) -> Option<String>,
) -> bool {
    adapter(provider_id)
        .and_then(|selected| read_key(&selected.key_env))
        .is_some_and(|key| !key.trim().is_empty())
}

/// Reviewed behavior of an exact provider/model pair in this native adapter.
/// An empty effort list means the UI must leave the provider default alone;
/// it does not assert that the model has no reasoning modes. The URLs are
/// provider-owned documentation, never values inferred from a model listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeModelCapabilities {
    pub provider_id: &'static str,
    pub model_id: &'static str,
    pub wire_api: NativeWireApi,
    pub function_tools: bool,
    pub streaming: bool,
    pub image_input: bool,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub supported_efforts: &'static [&'static str],
    pub source_url: &'static str,
}

const GPT5_EFFORTS: &[&str] = &["minimal", "low", "medium", "high"];
const GPT51_EFFORTS: &[&str] = &["none", "low", "medium", "high"];
const GPT52_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh"];
const GPT56_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const GPT6_ASTRA_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

macro_rules! openai_model {
    ($id:literal, $context:expr, $output:expr, $efforts:expr) => {
        NativeModelCapabilities {
            provider_id: "openai",
            model_id: $id,
            wire_api: NativeWireApi::Responses,
            function_tools: true,
            streaming: true,
            image_input: true,
            context_window_tokens: Some($context),
            max_output_tokens: Some($output),
            supported_efforts: $efforts,
            source_url: concat!("https://developers.openai.com/api/docs/models/", $id),
        }
    };
    ($id:literal, $context:expr, $output:expr, $efforts:expr, $source:literal) => {
        NativeModelCapabilities {
            provider_id: "openai",
            model_id: $id,
            wire_api: NativeWireApi::Responses,
            function_tools: true,
            streaming: true,
            image_input: true,
            context_window_tokens: Some($context),
            max_output_tokens: Some($output),
            supported_efforts: $efforts,
            source_url: concat!("https://developers.openai.com/api/docs/models/", $source),
        }
    };
}

// Reviewed against provider documentation on 2026-09-28. Every OpenAI row has
// a model page documenting Responses, streaming, function
// calling, image input, and the two token limits. The two GPT-4o snapshots are named on the
// GPT-4o page and inherit its published limits. DeepSeek's official /models
// reference gives exact integer limits for both canonical IDs:
// https://api-docs.deepseek.com/api/list-models/
// Its Chat Completions reference documents streamed function tool calls:
// https://api-docs.deepseek.com/api/create-chat-completion/
// DeepSeek runs with thinking disabled in our backend; its effort controls
// therefore are intentionally not advertised here. The provider may list
// image input for a model, but this Chat Completions adapter does not send
// image parts yet, so image_input remains false for the native route.
const MODEL_CAPABILITIES: &[NativeModelCapabilities] = &[
    openai_model!("gpt-4.1", 1_047_576, 32_768, &[]),
    openai_model!("gpt-4.1-mini", 1_047_576, 32_768, &[]),
    openai_model!("gpt-4o", 128_000, 16_384, &[]),
    openai_model!("gpt-4o-mini", 128_000, 16_384, &[]),
    openai_model!("gpt-4o-2024-08-06", 128_000, 16_384, &[], "gpt-4o"),
    openai_model!("gpt-4o-2024-11-20", 128_000, 16_384, &[], "gpt-4o"),
    openai_model!("gpt-5", 400_000, 128_000, GPT5_EFFORTS),
    openai_model!("gpt-5-mini", 400_000, 128_000, &[]),
    openai_model!("gpt-5-nano", 400_000, 128_000, &[]),
    openai_model!("gpt-5.1", 400_000, 128_000, GPT51_EFFORTS),
    openai_model!("gpt-5.2", 400_000, 128_000, GPT52_EFFORTS),
    openai_model!("gpt-5.3-codex", 400_000, 128_000, &[]),
    openai_model!("gpt-5.4", 1_050_000, 128_000, GPT52_EFFORTS),
    openai_model!("gpt-5.4-mini", 400_000, 128_000, GPT52_EFFORTS),
    openai_model!("gpt-5.4-nano", 400_000, 128_000, GPT52_EFFORTS),
    openai_model!("gpt-5.5", 1_050_000, 128_000, GPT52_EFFORTS),
    openai_model!("gpt-5.6", 1_050_000, 128_000, GPT56_EFFORTS),
    openai_model!("gpt-5.6-sol", 1_050_000, 128_000, GPT56_EFFORTS),
    openai_model!("gpt-5.6-terra", 1_050_000, 128_000, GPT56_EFFORTS),
    openai_model!("gpt-5.6-luna", 1_050_000, 128_000, GPT56_EFFORTS),
    openai_model!("o3", 200_000, 100_000, &[]),
    openai_model!("gpt-6-sol", 1_050_000, 128_000, GPT56_EFFORTS),
    openai_model!("gpt-6-luna", 1_050_000, 128_000, GPT56_EFFORTS),
    openai_model!("gpt-6-astra", 1_050_000, 128_000, GPT6_ASTRA_EFFORTS),
    NativeModelCapabilities {
        provider_id: "deepseek",
        model_id: "deepseek-flash",
        wire_api: NativeWireApi::ChatCompletions,
        function_tools: true,
        streaming: true,
        image_input: false,
        context_window_tokens: Some(1_048_576),
        max_output_tokens: Some(393_216),
        supported_efforts: &[],
        source_url: "https://api-docs.deepseek.com/api/list-models/",
    },
    NativeModelCapabilities {
        provider_id: "deepseek",
        model_id: "deepseek-v4-pro",
        wire_api: NativeWireApi::ChatCompletions,
        function_tools: true,
        streaming: true,
        image_input: false,
        context_window_tokens: Some(1_048_576),
        max_output_tokens: Some(393_216),
        supported_efforts: &[],
        source_url: "https://api-docs.deepseek.com/api/list-models/",
    },
];

/// None means that this exact pair has no reviewed execution guarantee.
/// An unknown model can still be chosen explicitly on any registered adapter;
/// it never inherits native model limits from a listing or its ID prefix.
pub(crate) fn native_model_capabilities(
    provider_id: &str,
    model_id: &str,
) -> Option<&'static NativeModelCapabilities> {
    MODEL_CAPABILITIES
        .iter()
        .find(|entry| entry.provider_id == provider_id && entry.model_id == model_id)
}

/// The executable native catalog is owned by the adapters. Provider model
/// listings may supply labels, but cannot authorize a new route or effort.
pub(crate) fn reviewed_native_models() -> &'static [NativeModelCapabilities] {
    MODEL_CAPABILITIES
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeRouteExecution {
    /// Exact provider/model pair reviewed for this adapter's tool loop.
    Verified,
    /// A model explicitly configured by the user, whose capabilities
    /// must be checked by the endpoint when a request is sent.
    ExplicitUnverified,
    Unsupported,
}

pub(crate) fn native_route_execution(
    provider_id: &str,
    model_id: &str,
    explicitly_configured: bool,
) -> NativeRouteExecution {
    native_route_execution_with_adapter(
        adapter(provider_id).as_ref(),
        provider_id,
        model_id,
        explicitly_configured,
    )
}

pub(crate) fn native_route_execution_with_registry(
    registry: &ProviderRegistry,
    provider_id: &str,
    model_id: &str,
    explicitly_configured: bool,
) -> NativeRouteExecution {
    native_route_execution_with_adapter(
        registry.adapter(provider_id),
        provider_id,
        model_id,
        explicitly_configured,
    )
}

fn native_route_execution_with_adapter(
    selected: Option<&Adapter>,
    provider_id: &str,
    model_id: &str,
    explicitly_configured: bool,
) -> NativeRouteExecution {
    let Some(selected) = selected else {
        return NativeRouteExecution::Unsupported;
    };
    if model_id.trim().is_empty() {
        return NativeRouteExecution::Unsupported;
    }
    if native_model_capabilities(provider_id, model_id).is_some_and(|capabilities| {
        capabilities.wire_api == selected.wire_api
            && capabilities.function_tools
            && capabilities.streaming
    }) {
        NativeRouteExecution::Verified
    } else if explicitly_configured {
        NativeRouteExecution::ExplicitUnverified
    } else {
        NativeRouteExecution::Unsupported
    }
}

/// Account-specific listing returned by a provider-owned endpoint. It stays
/// in memory. Credentials are never written to the local provider catalog.
/// A listed model still needs adapter compatibility checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderModelDiscovery {
    pub provider_id: String,
    pub models: std::collections::BTreeMap<String, DiscoveredModel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredModel {
    pub id: String,
    pub name: String,
    /// The provider listing explicitly declares the configured tool field.
    pub provider_reports_tools: bool,
    /// Distinguishes an explicit negative claim from a missing field.
    pub tool_call_declared: Option<bool>,
    pub text_input_output: bool,
    /// Exact levels present in this provider-owned listing. An absent field
    /// leaves the control on Automatic rather than guessing by model name.
    pub supported_efforts: Vec<String>,
}

/// Call on a worker, never from rendering. Listing paths and mappings come
/// from local definitions. Key presence, a successful listing, and actual
/// permission to generate are distinct states.
pub(crate) fn discover_connected_models_at(
    root: &Path,
    provider_id: &str,
) -> Result<ProviderModelDiscovery, String> {
    let selected = adapter_at(root, provider_id)?
        .filter(|adapter| adapter.model_list_path.is_some())
        .ok_or_else(|| format!("No hay catálogo autenticado para {provider_id}"))?;
    discover_connected_models_with_adapter(&selected)
}

fn discover_connected_models_with_adapter(
    selected: &Adapter,
) -> Result<ProviderModelDiscovery, String> {
    let provider_id = &selected.id;
    let key = std::env::var(&selected.key_env)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Falta {}", selected.key_env))?;
    if key.len() > 4_096 || key.chars().any(char::is_control) {
        return Err(format!("{} tiene un formato inválido", selected.key_env));
    }
    let path = selected
        .model_list_path
        .as_deref()
        .expect("discovery path checked");
    let url = format!("{}/{path}", selected.endpoint.trim_end_matches('/'));
    let client = Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("No se pudo preparar el catálogo del proveedor: {error}"))?;
    let response = client
        .get(url)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| format!("No se pudo consultar {provider_id}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "{provider_id} rechazó el catálogo de modelos (HTTP {})",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_BYTES)
    {
        return Err("El catálogo autenticado excede el tamaño permitido".to_owned());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_DISCOVERY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("No se pudo leer el catálogo de {provider_id}: {error}"))?;
    if bytes.len() as u64 > MAX_DISCOVERY_BYTES {
        return Err("El catálogo autenticado excede el tamaño permitido".to_owned());
    }
    parse_connected_models_for_adapter(selected, &bytes)
}

#[cfg(test)]
fn parse_connected_models(
    provider_id: &str,
    bytes: &[u8],
) -> Result<ProviderModelDiscovery, String> {
    let selected =
        adapter(provider_id).ok_or_else(|| format!("Proveedor desconocido: {provider_id}"))?;
    parse_connected_models_for_adapter(&selected, bytes)
}

pub(crate) fn parse_connected_models_for_adapter(
    selected: &Adapter,
    bytes: &[u8],
) -> Result<ProviderModelDiscovery, String> {
    let provider_id = &selected.id;
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Catálogo de {provider_id} inválido: {error}"))?;
    let entries = mapped_value(
        &document,
        selected.catalog.entries_path.as_deref().unwrap_or("data"),
    )
    .and_then(Value::as_array)
    .or_else(|| document.as_array())
    .ok_or_else(|| format!("Catálogo de {provider_id} sin lista de modelos"))?;
    if entries.len() > 5_000 {
        return Err("El catálogo autenticado contiene demasiados modelos".to_owned());
    }
    let mut models = std::collections::BTreeMap::new();
    for entry in entries {
        let Some(id) = mapped_value(entry, selected.catalog.id_path.as_deref().unwrap_or("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        else {
            continue;
        };
        let name = mapped_value(
            entry,
            selected.catalog.name_path.as_deref().unwrap_or("name"),
        )
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
        .unwrap_or(id);
        let tool_call_declared = selected
            .catalog
            .tool_call_path
            .as_deref()
            .and_then(|path| mapped_value(entry, path))
            .and_then(|value| {
                selected.catalog.tool_call_contains.as_ref().map_or_else(
                    || value.as_bool(),
                    |needle| {
                        value
                            .as_array()
                            .map(|values| values.iter().any(|value| value.as_str() == Some(needle)))
                    },
                )
            });
        let provider_reports_tools = tool_call_declared == Some(true);
        let text_input_output = [
            selected.catalog.input_modalities_path.as_deref(),
            selected.catalog.output_modalities_path.as_deref(),
        ]
        .into_iter()
        .all(|path| {
            path.and_then(|path| mapped_value(entry, path))
                .and_then(Value::as_array)
                .is_none_or(|modalities| {
                    modalities
                        .iter()
                        .any(|value| value.as_str() == Some("text"))
                })
        });
        let supported_efforts = selected
            .catalog
            .effort_levels_path
            .as_deref()
            .and_then(|path| mapped_value(entry, path))
            .and_then(Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|level| {
                        !level.is_empty()
                            && level.len() <= 64
                            && !level.chars().any(char::is_whitespace)
                            && !level.chars().any(char::is_control)
                    })
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        models.insert(
            id.to_owned(),
            DiscoveredModel {
                id: id.to_owned(),
                name: name.to_owned(),
                provider_reports_tools,
                tool_call_declared,
                text_input_output,
                supported_efforts,
            },
        );
    }
    Ok(ProviderModelDiscovery {
        provider_id: provider_id.to_owned(),
        models,
    })
}

pub(crate) fn mapped_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, segment| current.get(segment))
}

#[cfg(test)]
pub(crate) fn openai_native_compatible_model(model_id: &str) -> bool {
    native_model_capabilities("openai", model_id).is_some()
}

#[cfg(test)]
pub(crate) fn deepseek_native_compatible_model(model_id: &str) -> bool {
    native_model_capabilities("deepseek", model_id).is_some()
}

/// Reserve the documented maximum output because the backends do not yet set
/// a lower response limit. Unknown provider/model pairs remain unbudgeted.
pub(crate) fn verified_token_budget(
    provider_id: &str,
    model_id: &str,
) -> Option<TokenBudgetPolicy> {
    let capabilities = native_model_capabilities(provider_id, model_id)?;
    if !capabilities.function_tools || !capabilities.streaming {
        return None;
    }
    let context_window_tokens = capabilities.context_window_tokens?;
    let output_reserve_tokens = capabilities.max_output_tokens?;
    Some(TokenBudgetPolicy {
        context_window_tokens,
        output_reserve_tokens,
        tool_schema_reserve_tokens: 1_000,
        auto_compact_token_limit: None,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct NativeProviderSettings {
    pub provider_id: String,
    pub model_id: String,
    pub custom_endpoint: String,
}

impl Default for NativeProviderSettings {
    fn default() -> Self {
        Self {
            provider_id: "custom".to_owned(),
            model_id: DEFAULT_MODEL.to_owned(),
            custom_endpoint: DEFAULT_CUSTOM_ENDPOINT.to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeWireApi {
    Responses,
    ChatCompletions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeConnection {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model_id: String,
    pub wire_api: NativeWireApi,
}

impl NativeProviderSettings {
    /// A persisted choice wins. Without one, import the original native harness
    /// environment variables; migration is written only when `save` is called.
    pub fn load(root: &Path) -> Self {
        let fallback = || {
            Self::from_legacy(
                std::env::var("OPENIDE_MODEL").ok(),
                std::env::var("OPENIDE_API_BASE").ok(),
            )
        };
        let path = root.join(SETTINGS_FILE);
        if !fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_SETTINGS_BYTES) {
            return fallback();
        }
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(fallback)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let normalized = Self {
            provider_id: self.provider_id.trim().to_owned(),
            model_id: self.model_id.trim().to_owned(),
            custom_endpoint: self.custom_endpoint.trim().to_owned(),
        };
        normalized.validated_endpoint_in(&ProviderRegistry::load(root)?)?;
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let payload = serde_json::to_vec_pretty(&normalized)
            .map_err(|error| format!("No se pudo serializar la configuración nativa: {error}"))?;
        let temp = root.join(format!(
            ".native-provider-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            drop(file);
            replace_settings_atomically(&temp, &root.join(SETTINGS_FILE))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|error| format!("No se pudo guardar la configuración nativa: {error}"))
    }

    pub fn resolve(&self) -> Result<RuntimeConnection, String> {
        self.resolve_with_key(|name| std::env::var(name).ok())
    }

    pub fn resolve_at(&self, root: &Path) -> Result<RuntimeConnection, String> {
        self.resolve_with_registry(&ProviderRegistry::load(root)?, |name| {
            std::env::var(name).ok()
        })
    }

    fn resolve_with_key(
        &self,
        read_key: impl FnOnce(&str) -> Option<String>,
    ) -> Result<RuntimeConnection, String> {
        self.resolve_with_registry(&ProviderRegistry::builtin(), read_key)
    }

    pub(crate) fn resolve_with_registry(
        &self,
        registry: &ProviderRegistry,
        read_key: impl FnOnce(&str) -> Option<String>,
    ) -> Result<RuntimeConnection, String> {
        let endpoint = self.validated_endpoint_in(registry)?;
        let selected = registry
            .adapter(self.provider_id.trim())
            .expect("validated provider");
        Ok(RuntimeConnection {
            endpoint,
            api_key: read_key(&selected.key_env)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.trim().is_empty()),
            model_id: self.model_id.trim().to_owned(),
            wire_api: selected.wire_api,
        })
    }

    fn from_legacy(model: Option<String>, endpoint: Option<String>) -> Self {
        let mut settings = Self::default();
        if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
            settings.model_id = model.trim().to_owned();
        }
        if let Some(endpoint) = endpoint.filter(|value| !value.trim().is_empty()) {
            settings.custom_endpoint = endpoint.trim().to_owned();
        }
        settings
    }

    fn validated_endpoint_in(&self, registry: &ProviderRegistry) -> Result<String, String> {
        if self.model_id.trim().is_empty() {
            return Err("Selecciona un modelo para el agente nativo".to_owned());
        }
        let provider_id = self.provider_id.trim();
        let selected = registry
            .adapter(provider_id)
            .ok_or_else(|| format!("Proveedor nativo no compatible: {provider_id}"))?;
        if let Some(capabilities) = native_model_capabilities(provider_id, self.model_id.trim()) {
            if capabilities.wire_api != selected.wire_api
                || !capabilities.function_tools
                || !capabilities.streaming
            {
                return Err("Este modelo aún no está verificado para herramientas y streaming del adaptador nativo".to_owned());
            }
        }
        if selected.user_endpoint {
            validate_custom_endpoint(&self.custom_endpoint)
        } else {
            Ok(selected.endpoint.clone())
        }
    }
}

#[cfg(not(windows))]
fn replace_settings_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_settings_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
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

fn validate_custom_endpoint(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_whitespace) {
        return Err("Ingresa una URL válida para el endpoint personalizado".to_owned());
    }
    let url = reqwest::Url::parse(value)
        .map_err(|_| "Ingresa una URL válida para el endpoint personalizado".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("El endpoint personalizado debe ser una URL HTTP(S) sin credenciales, búsqueda ni fragmento".to_owned());
    }
    if url.scheme() == "http" {
        let host = url.host_str().unwrap_or_default();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !loopback {
            return Err(
                "Usa HTTPS para endpoints remotos; HTTP sólo se permite en localhost".to_owned(),
            );
        }
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_registry_keeps_credentials_provider_specific() {
        let registry = ProviderRegistry::builtin();
        assert_eq!(registry.adapters().len(), 7);
        assert_eq!(adapter("openai").unwrap().key_env, "OPENAI_API_KEY");
        assert_eq!(
            adapter("openrouter").unwrap().endpoint,
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            adapter("deepseek").unwrap().endpoint,
            "https://api.deepseek.com"
        );
        assert_eq!(adapter("custom").unwrap().key_env, "OPENIDE_API_KEY");
        for id in ["groq", "mistral", "xai"] {
            assert!(registry.adapter(id).is_some());
        }
        assert!(adapter("unknown").is_none());
    }

    #[test]
    fn credential_presence_uses_only_the_selected_adapter_env_name() {
        assert!(adapter_key_present_with("openai", |name| {
            assert_eq!(name, "OPENAI_API_KEY");
            Some("  key  ".into())
        }));
        assert!(!adapter_key_present_with("deepseek", |_| Some(
            " \t ".into()
        )));
        assert!(!adapter_key_present_with("unknown", |_| {
            panic!("unknown provider must not read any environment variable")
        }));
    }

    #[test]
    fn route_execution_requires_exact_review_or_explicit_selection() {
        assert_eq!(
            native_route_execution("openai", "gpt-6-sol", false),
            NativeRouteExecution::Verified
        );
        assert_eq!(
            native_route_execution("openrouter", "openai/gpt-6-sol", false),
            NativeRouteExecution::Unsupported
        );
        assert_eq!(
            native_route_execution("openrouter", "openai/gpt-6-sol", true),
            NativeRouteExecution::ExplicitUnverified
        );
        assert_eq!(
            native_route_execution("openai", "future-model", true),
            NativeRouteExecution::ExplicitUnverified
        );
        assert_eq!(
            native_route_execution("unknown", "gpt-6-sol", true),
            NativeRouteExecution::Unsupported
        );
    }

    #[test]
    fn authenticated_model_listing_parses_provider_claims_without_inventing_efforts() {
        let listing = parse_connected_models(
            "openrouter",
            br#"{"data":[
                {"id":"vendor/agent","name":"Agent","supported_parameters":["tools","temperature"],"architecture":{"input_modalities":["text","image"],"output_modalities":["text"]}},
                {"id":"vendor/image","name":"Image","supported_parameters":[],"architecture":{"input_modalities":["text"],"output_modalities":["image"]}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(listing.provider_id, "openrouter");
        assert!(listing.models["vendor/agent"].provider_reports_tools);
        assert!(listing.models["vendor/agent"].text_input_output);
        assert!(!listing.models["vendor/image"].provider_reports_tools);
        assert!(!listing.models["vendor/image"].text_input_output);
        assert!(parse_connected_models("openai", br#"{"models":[]}"#).is_err());

        let deepseek = parse_connected_models(
            "deepseek",
            br#"{"data":[{"id":"deepseek-flash","name":"Flash","effort":{"supported_levels":["low","high","max"]}}]}"#,
        )
        .unwrap();
        assert_eq!(deepseek.models["deepseek-flash"].name, "Flash");
        assert!(!deepseek.models["deepseek-flash"].provider_reports_tools);
        assert_eq!(deepseek.models["deepseek-flash"].tool_call_declared, None);
        assert!(deepseek.models["deepseek-flash"].text_input_output);
    }

    #[test]
    fn legacy_migration_keeps_original_defaults_and_overrides() {
        assert_eq!(
            NativeProviderSettings::from_legacy(None, None),
            NativeProviderSettings::default()
        );
        let migrated = NativeProviderSettings::from_legacy(
            Some(" my-model ".to_owned()),
            Some(" http://localhost:1234/v1 ".to_owned()),
        );
        assert_eq!(migrated.provider_id, "custom");
        assert_eq!(migrated.model_id, "my-model");
        assert_eq!(migrated.custom_endpoint, "http://localhost:1234/v1");
    }

    #[test]
    fn settings_round_trip_without_secrets() {
        let root = tempfile::tempdir().unwrap();
        let settings = NativeProviderSettings {
            provider_id: "openrouter".to_owned(),
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            custom_endpoint: DEFAULT_CUSTOM_ENDPOINT.to_owned(),
        };
        settings.save(root.path()).unwrap();
        assert_eq!(NativeProviderSettings::load(root.path()), settings);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join(SETTINGS_FILE)).unwrap()).unwrap();
        let fields = json.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(fields.contains_key("provider_id"));
        assert!(fields.contains_key("model_id"));
        assert!(fields.contains_key("custom_endpoint"));
        assert!(!fields.contains_key("api_key"));
    }

    #[test]
    fn save_replaces_existing_choice() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = NativeProviderSettings::default();
        settings.save(root.path()).unwrap();
        settings.provider_id = "deepseek".to_owned();
        settings.model_id = "deepseek-flash".to_owned();
        settings.save(root.path()).unwrap();
        assert_eq!(NativeProviderSettings::load(root.path()), settings);
    }

    #[test]
    fn resolve_uses_local_allowlist_instead_of_custom_endpoint_for_known_provider() {
        let settings = NativeProviderSettings {
            provider_id: "openai".to_owned(),
            model_id: "gpt-4.1".to_owned(),
            custom_endpoint: "https://untrusted.example/v1".to_owned(),
        };
        let connection = settings.resolve().unwrap();
        assert_eq!(connection.endpoint, "https://api.openai.com/v1");
        assert_eq!(connection.model_id, "gpt-4.1");
        assert_eq!(connection.wire_api, NativeWireApi::Responses);
    }

    #[test]
    fn resolve_reads_only_the_selected_provider_key() {
        for (provider_id, key_env) in [
            ("openai", "OPENAI_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("custom", "OPENIDE_API_KEY"),
        ] {
            let settings = NativeProviderSettings {
                provider_id: provider_id.to_owned(),
                model_id: if provider_id == "deepseek" {
                    "deepseek-flash".to_owned()
                } else {
                    DEFAULT_MODEL.to_owned()
                },
                ..NativeProviderSettings::default()
            };
            let connection = settings
                .resolve_with_key(|name| {
                    assert_eq!(name, key_env);
                    Some("  secret  ".to_owned())
                })
                .unwrap();
            assert_eq!(connection.api_key.as_deref(), Some("secret"));
        }
    }

    #[test]
    fn custom_endpoint_accepts_secure_remote_and_loopback_http() {
        for value in [
            "https://example.com/v1",
            "http://localhost:1234/v1",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:1234/v1",
        ] {
            assert!(validate_custom_endpoint(value).is_ok(), "{value}");
        }
    }

    #[test]
    fn custom_endpoint_rejects_unsafe_or_ambiguous_urls() {
        for value in [
            "",
            "not-a-url",
            "ftp://example.com/v1",
            "http://example.com/v1",
            "http://192.168.1.10:1234/v1",
            "https://user:password@example.com/v1",
            "https://example.com/v1?token=secret",
            "https://example.com/v1#fragment",
            "https://example.com/a b",
        ] {
            assert!(validate_custom_endpoint(value).is_err(), "{value}");
        }
    }

    #[test]
    fn empty_model_and_unknown_provider_are_rejected() {
        let mut settings = NativeProviderSettings {
            model_id: "  ".to_owned(),
            ..NativeProviderSettings::default()
        };
        assert!(settings.resolve().is_err());
        settings.model_id = "gpt-4.1".to_owned();
        settings.provider_id = "unlisted".to_owned();
        assert!(settings.resolve().is_err());
    }

    #[test]
    fn unknown_openai_models_are_explicit_unverified_routes() {
        for model in [
            "gpt-5.4-pro",
            "gpt-5.4-pro-2026-03-05",
            "o3-pro",
            "gpt-realtime-2.1",
            "gpt-future-unknown",
        ] {
            assert!(!openai_native_compatible_model(model));
            let settings = NativeProviderSettings {
                provider_id: "openai".into(),
                model_id: model.into(),
                ..NativeProviderSettings::default()
            };
            assert!(settings.resolve().is_ok());
        }
        assert!(openai_native_compatible_model("gpt-5.4"));
        assert!(openai_native_compatible_model("gpt-6-astra"));
    }

    #[test]
    fn token_budget_only_uses_reviewed_model_rows() {
        for model in ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
            let budget = verified_token_budget("openai", model).unwrap();
            assert_eq!(budget.context_window_tokens, 1_050_000);
            assert_eq!(budget.output_reserve_tokens, 128_000);
        }
        assert_eq!(
            verified_token_budget("openai", "gpt-4.1")
                .unwrap()
                .context_window_tokens,
            1_047_576
        );
        assert_eq!(
            verified_token_budget("openai", "gpt-4o")
                .unwrap()
                .output_reserve_tokens,
            16_384
        );
        assert_eq!(
            verified_token_budget("openai", "gpt-5")
                .unwrap()
                .context_window_tokens,
            400_000
        );
        assert_eq!(
            verified_token_budget("openai", "gpt-5.4-mini")
                .unwrap()
                .context_window_tokens,
            400_000
        );
        assert_eq!(
            verified_token_budget("deepseek", "deepseek-flash")
                .unwrap()
                .output_reserve_tokens,
            393_216
        );
        assert!(verified_token_budget("openrouter", "gpt-6-astra").is_none());
        assert!(verified_token_budget("custom", "gpt-6-astra").is_none());
        assert!(verified_token_budget("openai", "gpt-6-astra-future").is_none());
    }

    #[test]
    fn capability_matrix_has_unique_exact_pairs_and_provider_evidence() {
        let mut seen = std::collections::HashSet::new();
        for row in MODEL_CAPABILITIES {
            assert!(seen.insert((row.provider_id, row.model_id)));
            assert_eq!(row.wire_api, adapter(row.provider_id).unwrap().wire_api);
            assert!(row.function_tools && row.streaming);
            assert!(row.context_window_tokens.unwrap() > row.max_output_tokens.unwrap());
            assert!(row.source_url.starts_with(match row.provider_id {
                "openai" => "https://developers.openai.com/api/docs/models/",
                "deepseek" => "https://api-docs.deepseek.com/api/",
                _ => panic!("unreviewed provider in executable capability matrix"),
            }));
            let budget = verified_token_budget(row.provider_id, row.model_id).unwrap();
            assert_eq!(
                budget.context_window_tokens,
                row.context_window_tokens.unwrap()
            );
            assert_eq!(budget.output_reserve_tokens, row.max_output_tokens.unwrap());
        }
        assert_eq!(seen.len(), 26);
    }

    #[test]
    fn efforts_match_the_reviewed_responses_contract() {
        assert_eq!(
            native_model_capabilities("openai", "gpt-6-astra")
                .unwrap()
                .supported_efforts,
            &["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            native_model_capabilities("openai", "gpt-6-luna")
                .unwrap()
                .supported_efforts,
            &["none", "low", "medium", "high", "xhigh", "max"]
        );
        // This adapter explicitly disables DeepSeek thinking until its
        // reasoning_content can be journaled and replayed safely.
        assert!(native_model_capabilities("deepseek", "deepseek-v4-pro")
            .unwrap()
            .supported_efforts
            .is_empty());
        assert!(native_model_capabilities("openrouter", "deepseek/deepseek-flash").is_none());
        assert!(native_model_capabilities("custom", "gpt-6-luna").is_none());
    }

    #[test]
    fn other_native_providers_keep_chat_completions() {
        for provider_id in ["openrouter", "deepseek", "custom"] {
            let settings = NativeProviderSettings {
                provider_id: provider_id.into(),
                model_id: if provider_id == "deepseek" {
                    "deepseek-flash".into()
                } else {
                    DEFAULT_MODEL.into()
                },
                ..NativeProviderSettings::default()
            };
            assert_eq!(
                settings.resolve().unwrap().wire_api,
                NativeWireApi::ChatCompletions
            );
        }
    }

    #[test]
    fn deepseek_aliases_are_unverified_native_connections() {
        assert!(deepseek_native_compatible_model("deepseek-flash"));
        assert!(deepseek_native_compatible_model("deepseek-v4-pro"));
        assert!(!deepseek_native_compatible_model("deepseek-v4-flash"));
        let settings = NativeProviderSettings {
            provider_id: "deepseek".into(),
            model_id: "deepseek-v4-flash".into(),
            ..NativeProviderSettings::default()
        };
        assert!(settings.resolve().is_ok());
    }

    #[test]
    fn user_registry_loads_openai_compatible_provider_without_overriding_builtin() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(REGISTRY_FILE),
            br#"{
            "version":1,
            "providers":[{
                "id":"my-provider", "name":"My Provider",
                "endpoint":"https://api.example.test/v1",
                "key_env":"MY_PROVIDER_API_KEY",
                "wire_api":"chat_completions",
                "model_list_path":"models",
                "catalog":{"effort_levels_path":"capabilities.reasoning_effort"}
            }]
        }"#,
        )
        .unwrap();
        let registry = ProviderRegistry::load(root.path()).unwrap();
        assert_eq!(
            registry.adapter("my-provider").unwrap().endpoint,
            "https://api.example.test/v1"
        );
        let settings = NativeProviderSettings {
            provider_id: "my-provider".into(),
            model_id: "my-reasoner".into(),
            ..NativeProviderSettings::default()
        };
        assert!(settings.resolve().is_err());
        assert_eq!(
            settings.resolve_at(root.path()).unwrap().endpoint,
            "https://api.example.test/v1"
        );
        let listing = parse_connected_models_for_adapter(
            registry.adapter("my-provider").unwrap(),
            br#"{"data":[{"id":"my-reasoner","capabilities":{"reasoning_effort":["low","high"]}}]}"#,
        ).unwrap();
        assert_eq!(
            listing.models["my-reasoner"].supported_efforts,
            ["low", "high"]
        );
    }

    #[test]
    fn user_registry_rejects_endpoint_or_credential_takeover() {
        let root = tempfile::tempdir().unwrap();
        for entry in [
            r#"{"id":"openai","name":"Fake","endpoint":"https://evil.example/v1","key_env":"EVIL_KEY","wire_api":"responses","model_list_path":"models"}"#,
            r#"{"id":"other","name":"Other","endpoint":"https://evil.example/v1","key_env":"OPENAI_API_KEY","wire_api":"responses","model_list_path":"models"}"#,
            r#"{"id":"other","name":"Other","endpoint":"http://evil.example/v1","key_env":"OTHER_API_KEY","wire_api":"responses","model_list_path":"models"}"#,
        ] {
            fs::write(
                root.path().join(REGISTRY_FILE),
                format!(r#"{{"version":1,"providers":[{entry}]}}"#),
            )
            .unwrap();
            assert!(ProviderRegistry::load(root.path()).is_err());
        }
    }

    #[test]
    fn xai_listing_exposes_only_advertised_efforts() {
        let listing = parse_connected_models(
            "xai",
            br#"{"models":[
                {"id":"grok-reasoning","input_modalities":["text"],"output_modalities":["text"],"capabilities":{"reasoning_effort":["low","medium","high"]}},
                {"id":"grok-plain","input_modalities":["text"],"output_modalities":["text"]}
            ]}"#,
        ).unwrap();
        assert_eq!(
            listing.models["grok-reasoning"].supported_efforts,
            ["low", "medium", "high"]
        );
        assert!(listing.models["grok-plain"].supported_efforts.is_empty());
        assert!(listing.models["grok-reasoning"].text_input_output);
    }
}
