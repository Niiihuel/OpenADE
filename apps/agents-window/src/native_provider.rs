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
use std::time::{Duration, Instant};

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
    #[serde(default)]
    pub auth: ProviderAuth,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
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
    /// Exact, locally reviewed routes for gateways with several protocols.
    #[serde(default)]
    pub model_transports: BTreeMap<String, NativeWireApi>,
    #[serde(default)]
    pub opencode_go_transports: BTreeMap<String, NativeWireApi>,
}

impl Adapter {
    pub(crate) fn use_opencode_go(&mut self) {
        if self.id == "opencode" {
            self.endpoint = "https://opencode.ai/zen/go/v1".into();
            self.name = "OpenCode Go".into();
            self.model_transports = self.opencode_go_transports.clone();
        }
    }

    pub(crate) fn for_model(&self, model: &str) -> Result<Self, String> {
        let mut selected = self.clone();
        // Z.ai documents incremental function arguments for these exact
        // families. An arbitrary custom model does not inherit that option.
        if self.id == "zai"
            && matches!(model, "glm-5.3" | "glm-5.2" | "glm-5.1" | "glm-5" | "glm-4.7" | "glm-4.6")
        {
            selected.chat_request_fields.insert("tool_stream".into(), Value::Bool(true));
        }
        if !self.model_transports.is_empty() {
            selected.wire_api = *self.model_transports.get(model).ok_or(
                "Este modelo requiere un protocolo que todavía no está disponible en OpenIDE",
            )?;
            if selected.wire_api == NativeWireApi::AnthropicMessages {
                selected.auth = ProviderAuth::Header {
                    name: "x-api-key".into(),
                };
                selected.headers =
                    BTreeMap::from([("anthropic-version".into(), "2023-06-01".into())]);
            }
        }
        Ok(selected)
    }
}

/// Declarative paths in a provider-owned JSON model listing. A missing path
/// means the listing makes no claim; it never means a capability is supported.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CatalogMapping {
    pub entries_path: Option<String>,
    pub id_path: Option<String>,
    pub name_path: Option<String>,
    pub visibility_path: Option<String>,
    pub visibility_value: Option<String>,
    pub context_window_path: Option<String>,
    pub input_limit_path: Option<String>,
    pub max_output_path: Option<String>,
    pub tool_call_path: Option<String>,
    pub tool_call_contains: Option<String>,
    pub effort_levels_path: Option<String>,
    pub effort_format: EffortLevelsFormat,
    pub capability_support_paths: BTreeMap<String, String>,
    pub pagination: Option<CatalogPagination>,
    pub input_modalities_path: Option<String>,
    pub output_modalities_path: Option<String>,
    pub pricing_input_path: Option<String>,
    pub pricing_output_path: Option<String>,
    pub price_unit: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ProviderAuth {
    #[default]
    Bearer,
    Header {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffortLevelsFormat {
    #[default]
    Array,
    PresetArray,
    SupportedObject,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogPagination {
    pub cursor_query: String,
    pub cursor_path: String,
    pub has_more_path: String,
    pub limit_query: Option<String>,
    pub limit: Option<u16>,
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
        if crate::provider_connections::service(root, "opencode") == "go" {
            if let Some(adapter) = registry.adapters.iter_mut().find(|a| a.id == "opencode") {
                adapter.use_opencode_go();
            }
        }
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

pub(crate) fn adapter_for_model_at(
    root: &Path,
    provider_id: &str,
    model: &str,
) -> Result<Option<Adapter>, String> {
    adapter_at(root, provider_id)?
        .map(|adapter| adapter.for_model(model))
        .transpose()
}

fn validate_provider_definition(entry: &Adapter) -> Result<(), String> {
    validate_transport_settings(entry.wire_api, &entry.auth, &entry.headers)?;
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
        &entry.catalog.visibility_path,
        &entry.catalog.context_window_path,
        &entry.catalog.input_limit_path,
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
        if !valid_metadata_path(path) {
            return Err(format!("Ruta de metadatos inválida para {}", entry.id));
        }
    }
    if entry.catalog.visibility_path.is_some() != entry.catalog.visibility_value.is_some()
        || entry
            .catalog
            .visibility_value
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 64 || v.chars().any(char::is_control))
    {
        return Err(format!(
            "Visibilidad de catálogo inválida para {}",
            entry.id
        ));
    }
    for map in [&entry.model_transports, &entry.opencode_go_transports] {
        if map.len() > 1_000
            || map
                .keys()
                .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_whitespace))
        {
            return Err(format!("Protocolos por modelo inválidos para {}", entry.id));
        }
    }
    if entry.catalog.capability_support_paths.len() > 32
        || entry
            .catalog
            .capability_support_paths
            .iter()
            .any(|(name, path)| !valid_metadata_name(name) || !valid_metadata_path(path))
    {
        return Err(format!(
            "Capacidades declaradas inválidas para {}",
            entry.id
        ));
    }
    if let Some(page) = &entry.catalog.pagination {
        if !valid_metadata_name(&page.cursor_query)
            || !valid_metadata_path(&page.cursor_path)
            || !valid_metadata_path(&page.has_more_path)
            || page.limit_query.is_some() != page.limit.is_some()
            || page
                .limit_query
                .as_ref()
                .is_some_and(|name| !valid_metadata_name(name) || name == &page.cursor_query)
            || page.limit.is_some_and(|limit| !(1..=1000).contains(&limit))
        {
            return Err(format!("Paginación inválida para {}", entry.id));
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
                || (entry.wire_api == NativeWireApi::AnthropicMessages
                    && (!matches!(field.as_str(), "thinking" | "output_config")
                        || !value.is_object()))
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

fn valid_metadata_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_metadata_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 128 && path.split('.').all(valid_metadata_name)
}

/// Secrets are supplied only by the selected credential environment variable.
/// Until other transports implement static headers, reject unsupported schemes
/// rather than silently dropping part of the declared request profile.
pub(crate) fn validate_transport_settings(
    wire_api: NativeWireApi,
    auth: &ProviderAuth,
    headers: &BTreeMap<String, String>,
) -> Result<(), String> {
    match (wire_api, auth) {
        (NativeWireApi::Responses | NativeWireApi::ChatCompletions, ProviderAuth::Bearer)
            if headers.is_empty() =>
        {
            Ok(())
        }
        (NativeWireApi::AnthropicMessages, ProviderAuth::Header { name })
            if name == "x-api-key" =>
        {
            if headers.len() > 2
                || headers.iter().any(|(name, value)| {
                    !matches!(
                        name.as_str(),
                        "anthropic-version" | "anthropic-workspace-id"
                    ) || (name == "anthropic-version" && value != "2023-06-01")
                        || value.is_empty()
                        || value.len() > 256
                        || !value.is_ascii()
                        || value.chars().any(char::is_control)
                        || reqwest::header::HeaderValue::from_str(value).is_err()
                })
            {
                Err("Cabeceras de Anthropic inválidas o no implementadas".into())
            } else {
                Ok(())
            }
        }
        _ => Err(
            "El esquema de autenticación o las cabeceras no corresponden al protocolo implementado"
                .into(),
        ),
    }
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

macro_rules! chat_model {
    ($provider:literal, $id:literal, $context:expr, $output:expr, $efforts:expr, $source:literal) => {
        NativeModelCapabilities {
            provider_id: $provider,
            model_id: $id,
            wire_api: NativeWireApi::ChatCompletions,
            function_tools: true,
            streaming: true,
            image_input: false,
            context_window_tokens: Some($context),
            max_output_tokens: $output,
            supported_efforts: $efforts,
            source_url: $source,
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
    // Reviewed 2026-10-06. MiniMax's M3 API ceiling is explicit; for M2.x
    // the published API ceiling equals the whole context. Leave the output
    // reserve unknown rather than inventing an executable input budget.
    chat_model!("minimax", "MiniMax-M3", 1_000_000, Some(524_288), &[], "https://platform.minimax.io/docs/api-reference/text-chat-openai"),
    chat_model!("minimax", "MiniMax-M2.7", 204_800, None, &[], "https://platform.minimax.io/docs/api-reference/text-openai-api"),
    chat_model!("minimax", "MiniMax-M2.7-highspeed", 204_800, None, &[], "https://platform.minimax.io/docs/api-reference/text-openai-api"),
    chat_model!("minimax", "MiniMax-M2.5", 204_800, None, &[], "https://platform.minimax.io/docs/api-reference/text-openai-api"),
    chat_model!("minimax", "MiniMax-M2.5-highspeed", 204_800, None, &[], "https://platform.minimax.io/docs/api-reference/text-openai-api"),
    chat_model!("zai", "glm-5.3", 1_000_000, Some(131_072), &["low", "high", "max"], "https://docs.z.ai/guides/llm/glm-5.3"),
    chat_model!("zai", "glm-5.2", 1_000_000, Some(131_072), &["none", "minimal", "low", "medium", "high", "xhigh", "max"], "https://docs.z.ai/guides/llm/glm-5.2"),
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
    let Ok(selected) = selected.for_model(model_id) else {
        return NativeRouteExecution::Unsupported;
    };
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
    /// Position in the provider's visible listing, across all response pages.
    /// None is reserved for fixtures/legacy data whose order is unknown.
    pub listing_index: Option<usize>,
    /// The provider listing explicitly declares the configured tool field.
    pub provider_reports_tools: bool,
    /// Distinguishes an explicit negative claim from a missing field.
    pub tool_call_declared: Option<bool>,
    pub text_input_output: bool,
    /// Exact levels present in this provider-owned listing. An absent field
    /// leaves the control on Automatic rather than guessing by model name.
    pub supported_efforts: Vec<String>,
    /// This response declares an effort family, including a valid empty list.
    /// Empty declared levels cannot be filled from an older source.
    pub efforts_declared: bool,
    /// Exact declared limits from this account-specific model entry. Missing
    /// or invalid fields stay unknown, never inferred from the model's name.
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// Provider-reported input ceiling; never added to or inferred as a window.
    pub max_input_tokens: Option<u64>,
    /// The adapter maps this response's limits. Missing/null/zero values remain
    /// unknown instead of inheriting an older catalog or reviewed limit.
    pub limits_declared: bool,
    pub declared_capabilities: BTreeMap<String, bool>,
    /// This response is authoritative for the adapter's configured capability
    /// fields, including unknowns when the entire family is omitted or invalid.
    pub capabilities_declared: bool,
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
    let key = crate::provider_connections::read_key(root, &selected.key_env);
    if key.is_none() && !selected.key_optional {
        return Err(format!(
            "Conecta {} para consultar sus modelos",
            selected.name
        ));
    }
    discover_connected_models_with_key(&selected, key.as_deref())
}

pub(crate) fn discover_connected_models_with_key(
    selected: &Adapter,
    key: Option<&str>,
) -> Result<ProviderModelDiscovery, String> {
    let provider_id = &selected.id;
    let path = selected.model_list_path.as_deref()
        .ok_or_else(|| format!("No hay catálogo autenticado para {provider_id}"))?;
    let url = format!("{}/{path}", selected.endpoint.trim_end_matches('/'));
    let client = Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("No se pudo preparar el catálogo del proveedor: {error}"))?;
    let ModelListingFetch::Pages { pages, .. } = fetch_model_listing_pages(
        &client,
        &url,
        &selected.auth,
        &selected.headers,
        key,
        &selected.catalog,
        None,
        MAX_DISCOVERY_BYTES,
        DISCOVERY_TIMEOUT,
    )?
    else {
        return Err("El catálogo autenticado no devolvió sus modelos".into());
    };
    let mut models = BTreeMap::new();
    for bytes in pages {
        let listing = parse_connected_models_for_adapter(selected, &bytes)?;
        let page_offset = models.len();
        for (id, mut model) in listing.models {
            model.listing_index = model.listing_index.map(|index| page_offset + index);
            if models.insert(id, model).is_some() {
                return Err("El catálogo autenticado contiene IDs repetidos".into());
            }
        }
    }
    Ok(ProviderModelDiscovery {
        provider_id: provider_id.clone(),
        models,
    })
}

pub(crate) enum ModelListingFetch {
    NotModified,
    Pages {
        pages: Vec<Vec<u8>>,
        etag: Option<String>,
    },
}

/// Shared bounded transport for descriptive and account-specific catalogs.
/// Pagination can change query values only; it never supplies a next URL.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_model_listing_pages(
    client: &Client,
    url: &str,
    auth: &ProviderAuth,
    headers: &BTreeMap<String, String>,
    key: Option<&str>,
    mapping: &CatalogMapping,
    etag: Option<&str>,
    max_bytes: u64,
    timeout: Duration,
) -> Result<ModelListingFetch, String> {
    validate_transport_settings(
        match auth {
            ProviderAuth::Bearer => NativeWireApi::ChatCompletions,
            ProviderAuth::Header { .. } => NativeWireApi::AnthropicMessages,
        },
        auth,
        headers,
    )?;
    let key = key.map(str::trim).filter(|key| !key.is_empty());
    if key.is_some_and(|key| key.len() > 32768 || key.chars().any(char::is_control)) {
        return Err("La credencial del proveedor tiene un formato inválido".into());
    }
    let mut page_url = reqwest::Url::parse(url).map_err(|_| "URL de catálogo inválida")?;
    let started = Instant::now();
    let mut pages = Vec::new();
    let mut total_bytes = 0_u64;
    let mut total_models = 0_usize;
    let mut cursors = HashSet::new();
    let mut observed_etag = None;
    if let Some(page) = &mapping.pagination {
        if let (Some(name), Some(limit)) = (&page.limit_query, page.limit) {
            replace_query(&mut page_url, name, &limit.to_string());
        }
    }
    loop {
        if pages.len() >= 64 {
            return Err("El catálogo excede el límite de páginas".into());
        }
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|value| !value.is_zero())
            .ok_or("La consulta completa del catálogo excedió el tiempo permitido")?;
        let mut request = client
            .get(page_url.clone())
            .timeout(remaining)
            .header("Accept", "application/json");
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if !headers.contains_key("anthropic-version")
            && matches!(auth, ProviderAuth::Header { name } if name == "x-api-key")
        {
            request = request.header("anthropic-version", "2023-06-01");
        }
        if let Some(key) = key {
            request = match auth {
                ProviderAuth::Bearer => request.bearer_auth(key),
                ProviderAuth::Header { name } => request.header(name.as_str(), key),
            };
        }
        let conditional = mapping
            .pagination
            .is_none()
            .then_some(etag)
            .flatten()
            .filter(|value| value.len() <= 1024 && !value.chars().any(char::is_control));
        if let Some(etag) = conditional {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let response = request
            .send()
            .map_err(|_| "La consulta del proveedor falló o excedió el tiempo permitido")?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return if pages.is_empty() && conditional.is_some() {
                Ok(ModelListingFetch::NotModified)
            } else {
                Err("El proveedor devolvió un catálogo incompleto sin contenido".into())
            };
        }
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status().as_u16()));
        }
        let allowance = max_bytes
            .checked_sub(total_bytes)
            .ok_or("Catálogo demasiado grande")?;
        if response
            .content_length()
            .is_some_and(|length| length > allowance)
        {
            return Err("El catálogo excede el límite agregado de bytes".into());
        }
        if mapping.pagination.is_none() {
            observed_etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 1024 && !value.chars().any(char::is_control))
                .map(str::to_owned);
        }
        let mut bytes = Vec::new();
        response
            .take(allowance + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "No se pudo leer el catálogo del proveedor")?;
        if bytes.len() as u64 > allowance {
            return Err("El catálogo excede el límite agregado de bytes".into());
        }
        total_bytes += bytes.len() as u64;
        let document: Value =
            serde_json::from_slice(&bytes).map_err(|_| "JSON de catálogo inválido")?;
        let entries = mapped_value(&document, mapping.entries_path.as_deref().unwrap_or("data"))
            .and_then(Value::as_array)
            .or_else(|| document.as_array())
            .ok_or("El catálogo no tiene una lista de modelos")?;
        total_models = total_models
            .checked_add(entries.len())
            .ok_or("Demasiados modelos")?;
        if total_models > 5000 {
            return Err("El catálogo contiene demasiados modelos".into());
        }
        let next = if let Some(page) = &mapping.pagination {
            let has_more = mapped_value(&document, &page.has_more_path)
                .and_then(Value::as_bool)
                .ok_or("La paginación del catálogo no informa si está completa")?;
            if has_more {
                let cursor = mapped_value(&document, &page.cursor_path)
                    .and_then(Value::as_str)
                    .filter(|cursor| {
                        !cursor.is_empty()
                            && cursor.len() <= 512
                            && !cursor.chars().any(char::is_control)
                    })
                    .ok_or("Falta un cursor válido para continuar el catálogo")?;
                if entries.is_empty() || !cursors.insert(cursor.to_owned()) {
                    return Err("La paginación del catálogo no avanza".into());
                }
                Some((page.cursor_query.clone(), cursor.to_owned()))
            } else {
                None
            }
        } else {
            None
        };
        pages.push(bytes);
        match next {
            Some((name, cursor)) => replace_query(&mut page_url, &name, &cursor),
            None => {
                return Ok(ModelListingFetch::Pages {
                    pages,
                    etag: observed_etag,
                })
            }
        }
    }
}

fn replace_query(url: &mut reqwest::Url, name: &str, value: &str) {
    let pairs = url
        .query_pairs()
        .filter(|(key, _)| key != name)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(pairs)
        .append_pair(name, value);
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
        if !catalog_entry_visible(entry, &selected.catalog) {
            continue;
        }
        let id = mapped_value(entry, selected.catalog.id_path.as_deref().unwrap_or("id"))
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.trim() == *id
                    && id.len() <= 256
                    && !id.chars().any(char::is_control)
            })
            .ok_or("El catálogo autenticado contiene un ID inválido")?;
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
        let supported_efforts = mapped_effort_levels(entry, &selected.catalog);
        let listing_index = Some(models.len());
        let previous = models.insert(
            id.to_owned(),
            DiscoveredModel {
                id: id.to_owned(),
                name: name.to_owned(),
                listing_index,
                provider_reports_tools,
                tool_call_declared,
                text_input_output,
                supported_efforts,
                efforts_declared: mapped_efforts_declared(entry, &selected.catalog),
                context_window_tokens: selected
                    .catalog
                    .context_window_path
                    .as_deref()
                    .and_then(|path| mapped_value(entry, path))
                    .and_then(Value::as_u64)
                    .filter(|limit| *limit > 0),
                max_output_tokens: selected
                    .catalog
                    .max_output_path
                    .as_deref()
                    .and_then(|path| mapped_value(entry, path))
                    .and_then(Value::as_u64)
                    .filter(|limit| *limit > 0),
                max_input_tokens: selected
                    .catalog
                    .input_limit_path
                    .as_deref()
                    .and_then(|path| mapped_value(entry, path))
                    .and_then(Value::as_u64)
                    .filter(|limit| *limit > 0),
                limits_declared: mapped_limits_declared(&selected.catalog),
                declared_capabilities: mapped_capabilities(entry, &selected.catalog),
                capabilities_declared: mapped_capabilities_declared(entry, &selected.catalog),
            },
        );
        if previous.is_some() {
            return Err("El catálogo autenticado contiene IDs repetidos".into());
        }
    }
    Ok(ProviderModelDiscovery {
        provider_id: provider_id.to_owned(),
        models,
    })
}

pub(crate) fn catalog_entry_visible(entry: &Value, mapping: &CatalogMapping) -> bool {
    mapping.visibility_path.as_deref().is_none_or(|path| {
        mapped_value(entry, path).and_then(Value::as_str) == mapping.visibility_value.as_deref()
    })
}

pub(crate) fn mapped_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, segment| current.get(segment))
}

pub(crate) fn mapped_capabilities(
    entry: &Value,
    mapping: &CatalogMapping,
) -> BTreeMap<String, bool> {
    let mut capabilities = mapping
        .capability_support_paths
        .iter()
        .filter_map(|(name, path)| {
            mapped_value(entry, path)
                .and_then(Value::as_bool)
                .map(|value| (name.clone(), value))
        })
        .collect::<BTreeMap<_, _>>();
    if !capabilities.contains_key("image_input") {
        if let Some(values) = mapping
            .input_modalities_path
            .as_deref()
            .and_then(|path| mapped_value(entry, path))
            .and_then(Value::as_array)
            .filter(|values| {
                values.len() <= 16
                    && values.iter().all(|value| {
                        value.as_str().is_some_and(|modality| {
                            !modality.is_empty()
                                && modality.len() <= 64
                                && !modality.chars().any(char::is_control)
                        })
                    })
            })
        {
            capabilities.insert(
                "image_input".into(),
                values.iter().any(|value| value.as_str() == Some("image")),
            );
        }
    }
    capabilities
}

pub(crate) fn mapped_capabilities_declared(_entry: &Value, mapping: &CatalogMapping) -> bool {
    // Only provider-configured mappings create authority. For example, OpenAI's
    // models listing has no capability mapping and keeps the reviewed fallback.
    mapping.input_modalities_path.is_some() || !mapping.capability_support_paths.is_empty()
}

pub(crate) fn mapped_limits_declared(mapping: &CatalogMapping) -> bool {
    mapping.context_window_path.is_some()
        || mapping.input_limit_path.is_some()
        || mapping.max_output_path.is_some()
}

pub(crate) fn mapped_efforts_declared(entry: &Value, mapping: &CatalogMapping) -> bool {
    let Some(value) = mapping
        .effort_levels_path
        .as_deref()
        .and_then(|path| mapped_value(entry, path))
    else {
        return false;
    };
    match mapping.effort_format {
        EffortLevelsFormat::Array => value
            .as_array()
            .is_some_and(|values| valid_effort_array(values)),
        EffortLevelsFormat::PresetArray => value
            .as_array()
            .is_some_and(|values| valid_effort_presets(values)),
        EffortLevelsFormat::SupportedObject => value.as_object().is_some_and(|values| {
            values.len() <= 33 && values.get("supported").and_then(Value::as_bool).is_some()
        }),
    }
}

fn valid_effort_level(level: &str) -> bool {
    !level.is_empty()
        && level.len() <= 64
        && !level
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn valid_effort_array(values: &[Value]) -> bool {
    let mut unique = HashSet::new();
    values.len() <= 32
        && values.iter().all(|value| {
            value
                .as_str()
                .is_some_and(|level| valid_effort_level(level) && unique.insert(level))
        })
}

fn valid_effort_presets(values: &[Value]) -> bool {
    let mut unique = HashSet::new();
    values.len() <= 32
        && values.iter().all(|value| {
            value
                .get("effort")
                .and_then(Value::as_str)
                .is_some_and(|level| valid_effort_level(level) && unique.insert(level))
        })
}

pub(crate) fn mapped_effort_levels(entry: &Value, mapping: &CatalogMapping) -> Vec<String> {
    let Some(value) = mapping
        .effort_levels_path
        .as_deref()
        .and_then(|path| mapped_value(entry, path))
    else {
        return Vec::new();
    };
    let levels = match mapping.effort_format {
        EffortLevelsFormat::PresetArray => {
            let Some(values) = value
                .as_array()
                .filter(|values| valid_effort_presets(values))
            else {
                return Vec::new();
            };
            values
                .iter()
                .filter_map(|value| value.get("effort").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        }
        EffortLevelsFormat::Array => {
            let Some(values) = value.as_array().filter(|values| valid_effort_array(values)) else {
                return Vec::new();
            };
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        }
        EffortLevelsFormat::SupportedObject => {
            let Some(values) = value.as_object().filter(|values| values.len() <= 33) else {
                return Vec::new();
            };
            if values.get("supported").and_then(Value::as_bool) != Some(true) {
                return Vec::new();
            }
            let mut levels = values
                .iter()
                .filter(|(_, value)| value.get("supported").and_then(Value::as_bool) == Some(true))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            levels.sort();
            levels
        }
    };
    let mut unique = HashSet::new();
    if levels
        .iter()
        .any(|level| !valid_effort_level(level) || !unique.insert(level.clone()))
    {
        return Vec::new();
    }
    levels
}

#[cfg(test)]
pub(crate) fn openai_native_compatible_model(model_id: &str) -> bool {
    native_model_capabilities("openai", model_id).is_some()
}

#[cfg(test)]
pub(crate) fn deepseek_native_compatible_model(model_id: &str) -> bool {
    native_model_capabilities("deepseek", model_id).is_some()
}

/// Default reserve is the documented maximum output. A host-selected cap
/// sent by the backend may lower it. Unknown pairs remain unbudgeted.
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
        input_limit_tokens: None,
        output_reserve_tokens,
        tool_schema_reserve_tokens: 1_000,
        auto_compact_token_limit: None,
    })
}

/// Host-selected generation limits scoped to an exact route. No remote
/// listing can choose these wire fields or alter the provider endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeOutputLimit {
    pub provider_id: String,
    pub model_id: String,
    pub wire_api: NativeWireApi,
    pub field: String,
    pub tokens: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NativeOutputLimitsFile {
    version: u32,
    limits: Vec<NativeOutputLimit>,
}

fn load_output_limits(root: &Path) -> Result<NativeOutputLimitsFile, String> {
    let file = match fs::File::open(root.join("native-output-limits.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NativeOutputLimitsFile {
                version: 1,
                limits: vec![],
            })
        }
        Err(error) => return Err(format!("No se pudieron leer límites de salida: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("No se pudieron leer límites de salida: {error}"))?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("Límites de salida demasiado grandes".into());
    }
    let file: NativeOutputLimitsFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Límites de salida inválidos: {error}"))?;
    validate_output_limits(&file)?;
    Ok(file)
}

fn validate_output_limits(file: &NativeOutputLimitsFile) -> Result<(), String> {
    if file.version != 1 || file.limits.len() > 128 {
        return Err("Versión o cantidad de límites de salida inválida".into());
    }
    let mut routes = HashSet::new();
    for limit in &file.limits {
        if [&limit.provider_id, &limit.model_id]
            .into_iter()
            .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
            || limit.tokens == 0
            || limit.tokens > i64::MAX as u64
            || match limit.wire_api {
                NativeWireApi::Responses => limit.field != "max_output_tokens" || limit.tokens < 16,
                NativeWireApi::AnthropicMessages => {
                    limit.field != "max_tokens" || limit.tokens > u64::from(u32::MAX)
                }
                NativeWireApi::ChatCompletions => {
                    !matches!(limit.field.as_str(), "max_tokens" | "max_completion_tokens")
                }
            }
            || !routes.insert((&limit.provider_id, &limit.model_id))
        {
            return Err("Ruta, parámetro o valor de límite de salida inválido o duplicado".into());
        }
    }
    Ok(())
}

pub(crate) fn configured_output_limit(
    root: &Path,
    provider_id: &str,
    model_id: &str,
    wire_api: NativeWireApi,
) -> Result<Option<NativeOutputLimit>, String> {
    let file = load_output_limits(root)?;
    if let Some(limit) = file
        .limits
        .into_iter()
        .find(|limit| limit.provider_id == provider_id && limit.model_id == model_id)
    {
        if limit.wire_api != wire_api {
            return Err("El protocolo del límite de salida no coincide con la ruta".into());
        }
        return Ok(Some(limit));
    }
    Ok(None)
}

pub(crate) fn save_output_limit(
    root: &Path,
    provider_id: &str,
    model_id: &str,
    limit: Option<NativeOutputLimit>,
) -> Result<(), String> {
    let mut file = load_output_limits(root)?;
    file.limits
        .retain(|entry| entry.provider_id != provider_id || entry.model_id != model_id);
    if let Some(limit) = limit {
        if limit.provider_id != provider_id || limit.model_id != model_id {
            return Err("El límite de salida pertenece a otra ruta".into());
        }
        file.limits.push(limit);
    }
    validate_output_limits(&file)?;
    let payload = serde_json::to_vec_pretty(&file).map_err(|error| error.to_string())?;
    if payload.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("Límites de salida demasiado grandes".into());
    }
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let temp = root.join(format!(
        ".native-output-limits-{}-{}.tmp",
        std::process::id(),
        SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        let mut temporary = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        temporary.write_all(&payload)?;
        temporary.sync_all()?;
        drop(temporary);
        replace_settings_atomically(&temp, &root.join("native-output-limits.json"))?;
        #[cfg(unix)]
        fs::File::open(root)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|error| format!("No se pudo guardar el límite de salida: {error}"))
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
    AnthropicMessages,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeConnection {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model_id: String,
    pub wire_api: NativeWireApi,
    pub auth: ProviderAuth,
    pub headers: BTreeMap<String, String>,
}

impl NativeProviderSettings {
    /// Distinguish an intentional connection from the original app defaults.
    /// Merely starting OpenIDE does not connect the default custom endpoint.
    pub(crate) fn has_explicit_configuration(&self, root: &Path) -> bool {
        if self != &Self::default()
            || ["OPENIDE_MODEL", "OPENIDE_API_BASE", "OPENIDE_API_KEY"]
                .iter()
                .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
        {
            return true;
        }
        let path = root.join(SETTINGS_FILE);
        fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_SETTINGS_BYTES)
            && fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
                .is_some()
    }

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

    #[cfg(test)]
    pub fn resolve(&self) -> Result<RuntimeConnection, String> {
        self.resolve_with_key(|name| std::env::var(name).ok())
    }

    pub fn resolve_at(&self, root: &Path) -> Result<RuntimeConnection, String> {
        self.resolve_with_registry(&ProviderRegistry::load(root)?, |name| {
            crate::provider_connections::read_key(root, name)
        })
    }

    #[cfg(test)]
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
            .expect("validated provider")
            .for_model(&self.model_id)?;
        Ok(RuntimeConnection {
            endpoint,
            api_key: read_key(&selected.key_env)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.trim().is_empty()),
            model_id: self.model_id.trim().to_owned(),
            wire_api: selected.wire_api,
            auth: selected.auth.clone(),
            headers: selected.headers.clone(),
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

    pub(crate) fn validated_endpoint_in(
        &self,
        registry: &ProviderRegistry,
    ) -> Result<String, String> {
        if self.model_id.trim().is_empty() {
            return Err("Selecciona un modelo para el agente nativo".to_owned());
        }
        let provider_id = self.provider_id.trim();
        let selected = registry
            .adapter(provider_id)
            .ok_or_else(|| format!("Proveedor no compatible: {provider_id}"))?
            .for_model(self.model_id.trim())?;
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
pub(crate) fn replace_settings_atomically(
    temporary: &Path,
    destination: &Path,
) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
pub(crate) fn replace_settings_atomically(
    temporary: &Path,
    destination: &Path,
) -> std::io::Result<()> {
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
pub(crate) mod tests {
    use super::*;

    pub(crate) fn codex_ordered_listing() -> serde_json::Value {
        serde_json::json!({"models":[
            {"slug":"model-zulu","display_name":"Modelo Zulu","visibility":"list"},
            {"slug":"hidden-first","display_name":"Hidden","visibility":"hide"},
            {"slug":"model-alpha","display_name":"Modelo Alpha","visibility":"list"},
            {"slug":"model-middle","display_name":"Modelo Middle","visibility":"list"},
            {"slug":"hidden-second","display_name":"Hidden","visibility":"hide"},
            {"slug":"model-beta","display_name":"Modelo Beta","visibility":"list"},
            {"slug":"hidden-third","display_name":"Hidden","visibility":"hide"}
        ]})
    }

    #[test]
    fn codex_account_listing_preserves_server_order_and_filters_visibility_without_a_four_model_cap() {
        let original = codex_ordered_listing();
        let mut current = original.clone();
        current["models"].as_array_mut().unwrap().insert(0,
            serde_json::json!({"slug":"model-new","display_name":"Current first","visibility":"list"}));
        let (endpoint, requests, server) = listing_server(vec![
            (200, original.to_string()), (200, current.to_string()),
        ]);
        let mut selected = adapter("codex").unwrap();
        selected.endpoint = endpoint;
        for expected in [
            vec!["model-zulu", "model-alpha", "model-middle", "model-beta"],
            vec!["model-new", "model-zulu", "model-alpha", "model-middle", "model-beta"],
        ] {
            let listing = discover_connected_models_with_key(&selected, Some("fixture-chatgpt-token")).unwrap();
            let mut models = listing.models.values().collect::<Vec<_>>();
            models.sort_by_key(|model| model.listing_index);
            assert_eq!(models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), expected);
            assert_eq!(models.iter().map(|model| model.listing_index).collect::<Vec<_>>(),
                (0..models.len()).map(Some).collect::<Vec<_>>());
            assert_eq!(listing.models["model-zulu"].name, "Modelo Zulu");
            assert!(listing.models.values().all(|model| model.tool_call_declared.is_none()));
            let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(request.starts_with("GET /v1/models "));
            assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-chatgpt-token\r\n"));
        }
        server.join().unwrap();
    }

    #[test]
    fn native_listing_order_spans_pages_instead_of_restarting_or_sorting_ids() {
        let responses = vec![
            (200, serde_json::json!({"data":[{"id":"zulu"},{"id":"yellow"}],"has_more":true,"last_id":"yellow"}).to_string()),
            (200, serde_json::json!({"data":[{"id":"alpha"},{"id":"beta"}],"has_more":false}).to_string()),
        ];
        let (endpoint, _, server) = listing_server(responses);
        let listing = discover_connected_models_with_key(&anthropic_adapter(endpoint), Some("fixture-key")).unwrap();
        for (index, id) in ["zulu", "yellow", "alpha", "beta"].iter().enumerate() {
            assert_eq!(listing.models[*id].listing_index, Some(index));
        }
        server.join().unwrap();
    }

    pub(crate) fn listing_server(
        responses: Vec<(u16, String)>,
    ) -> (
        String,
        std::sync::mpsc::Receiver<String>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let (sender, receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for (status, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "catalog request did not arrive");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("catalog accept failed: {error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    request.push_str(&line);
                    assert!(request.len() < 16 * 1024);
                    if line == "\r\n" {
                        break;
                    }
                }
                let _ = sender.send(request);
                // A size/status rejection may close before reading the body.
                let _ = write!(socket,
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nETag: \"page-only\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (endpoint, receiver, server)
    }

    pub(crate) fn anthropic_adapter(endpoint: String) -> Adapter {
        let mut selected = adapter("anthropic").unwrap();
        // Protocol support must work for a provider and model ID with no
        // Anthropic-specific name or reviewed static model row.
        selected.id = "new-gateway".into();
        selected.name = "New Gateway".into();
        selected.key_env = "NEW_GATEWAY_FIXTURE_KEY".into();
        selected.endpoint = endpoint;
        selected
    }

    pub(crate) fn complete_listing_pages() -> Vec<(u16, String)> {
        vec![
            (200, serde_json::json!({"data":[{"id":"opaque-model","display_name":"First exact model",
                "max_input_tokens":65536,"max_tokens":4096,"capabilities":{
                    "effort":{"supported":true,"low":{"supported":true},"high":{"supported":false},
                        "turbo_2028":{"supported":true},"xhigh":{"supported":true}},
                    "image_input":{"supported":true},"pdf_input":{"supported":false},
                    "thinking":{"supported":true,"types":{"adaptive":{"supported":true}}},
                    "structured_outputs":{"supported":true}}}],"has_more":true,"last_id":"opaque-model"}).to_string()),
            (200, serde_json::json!({"data":[{"id":"opaque-model-preview","display_name":"Second exact model",
                "max_input_tokens":null,"max_tokens":0,"capabilities":{
                    "effort":{"supported":false,"low":{"supported":true}},
                    "image_input":{"supported":false},"thinking":{"supported":false}}}],
                "has_more":false,"last_id":"opaque-model-preview"}).to_string()),
        ]
    }

    pub(crate) fn authoritative_empty_listing() -> (u16, String) {
        (
            200,
            serde_json::json!({"data":[{"id":"opaque-model","display_name":"Current exact model",
            "max_input_tokens":null,"max_tokens":0,"capabilities":{
                "effort":{"supported":true,"low":{"supported":false},"high":{"supported":false}},
                "image_input":null,"thinking":null,"structured_outputs":null}}],
            "has_more":false,"last_id":"opaque-model"})
            .to_string(),
        )
    }

    #[test]
    fn current_declared_empty_efforts_and_unknown_images_do_not_inherit_stale_catalog() {
        let mut responses = complete_listing_pages();
        responses.push(authoritative_empty_listing());
        responses.push((
            200,
            serde_json::json!({"data":[{"id":"opaque-model",
            "capabilities":null}],"has_more":false})
            .to_string(),
        ));
        responses.push((
            200,
            serde_json::json!({"data":[{"id":"opaque-model"}],
            "has_more":false})
            .to_string(),
        ));
        responses.push((
            200,
            serde_json::json!({"data":[{"id":"opaque-model",
            "capabilities":42}],"has_more":false})
            .to_string(),
        ));
        let (endpoint, _, server) = listing_server(responses);
        let selected = anthropic_adapter(endpoint);
        let prior = discover_connected_models_with_key(&selected, Some("fixture-key")).unwrap();
        let old = &prior.models["opaque-model"];
        assert!(old.efforts_declared);
        assert!(old.capabilities_declared);
        assert!(!old.supported_efforts.is_empty());
        assert!(old.declared_capabilities["image_input"]);
        let current = discover_connected_models_with_key(&selected, Some("fixture-key")).unwrap();
        let exact = &current.models["opaque-model"];
        assert!(exact.efforts_declared);
        assert!(exact.capabilities_declared);
        assert!(exact.limits_declared);
        assert_eq!(exact.max_input_tokens, None);
        assert_eq!(exact.max_output_tokens, None);
        assert!(exact.supported_efforts.is_empty());
        assert!(!exact.declared_capabilities.contains_key("image_input"));
        assert_eq!(exact.declared_capabilities.get("effort"), Some(&true));
        let stale: crate::model_catalog::CatalogSnapshot = serde_json::from_value(serde_json::json!({
            "version":3,"fetched_at_ms":1,"providers":{"new-gateway":{"id":"new-gateway",
                "name":"New Gateway","models":{"opaque-model":{"id":"opaque-model","name":"Old model",
                    "provider_efforts":old.supported_efforts,"provider_efforts_declared":true,
                    "limit":{"input":old.max_input_tokens,"output":old.max_output_tokens},
                    "provider_limits_declared":true,
                    "declared_capabilities":old.declared_capabilities,"capabilities_declared":true}}}},
            "provider_status":{"new-gateway":{"source":"official_api","attempted_at_ms":1,"verified_at_ms":1}}
        })).unwrap();
        assert_eq!(
            crate::app::native_token_budget(&selected, "opaque-model", Some(&stale), &[])
                .unwrap()
                .input_limit_tokens,
            Some(65536)
        );
        assert_eq!(
            crate::app::native_output_maximum(&selected, "opaque-model", Some(&stale), &[]),
            Some(4096)
        );
        assert!(crate::app::native_token_budget(
            &selected,
            "opaque-model",
            Some(&stale),
            std::slice::from_ref(&current)
        )
        .is_none());
        assert_eq!(
            crate::app::native_output_maximum(
                &selected,
                "opaque-model",
                Some(&stale),
                std::slice::from_ref(&current)
            ),
            None
        );
        assert!(crate::app::native_image_input_supported(
            &selected,
            "opaque-model",
            Some(&stale),
            &[]
        ));
        assert!(!crate::app::native_request_capabilities(
            &selected,
            "opaque-model",
            Some(&stale),
            &[]
        )
        .reasoning_efforts
        .is_empty());
        assert!(!crate::app::native_image_input_supported(
            &selected,
            "opaque-model",
            Some(&stale),
            std::slice::from_ref(&current)
        ));
        assert!(crate::app::native_request_capabilities(
            &selected,
            "opaque-model",
            Some(&stale),
            &[current]
        )
        .reasoning_efforts
        .is_empty());
        for case in ["null", "absent", "invalid"] {
            let unknown =
                discover_connected_models_with_key(&selected, Some("fixture-key")).unwrap();
            assert!(
                unknown.models["opaque-model"].capabilities_declared,
                "{case}"
            );
            assert!(unknown.models["opaque-model"].limits_declared, "{case}");
            assert!(
                crate::app::native_token_budget(
                    &selected,
                    "opaque-model",
                    Some(&stale),
                    std::slice::from_ref(&unknown)
                )
                .is_none(),
                "{case}"
            );
            assert_eq!(
                crate::app::native_output_maximum(
                    &selected,
                    "opaque-model",
                    Some(&stale),
                    std::slice::from_ref(&unknown)
                ),
                None,
                "{case}"
            );
            assert!(!unknown.models["opaque-model"].efforts_declared, "{case}");
            assert!(
                unknown.models["opaque-model"]
                    .declared_capabilities
                    .is_empty(),
                "{case}"
            );
            assert!(
                !crate::app::native_image_input_supported(
                    &selected,
                    "opaque-model",
                    Some(&stale),
                    std::slice::from_ref(&unknown)
                ),
                "{case}"
            );
            assert!(
                crate::app::native_request_capabilities(
                    &selected,
                    "opaque-model",
                    Some(&stale),
                    &[unknown]
                )
                .reasoning_efforts
                .is_empty(),
                "{case}"
            );
        }
        server.join().unwrap();
    }

    #[test]
    fn current_exclusive_modalities_and_empty_arrays_have_explicit_declaration_flags() {
        let body = serde_json::json!({"data":[
            {"id":"text-only","input_modalities":["text"],"efforts":[]},
            {"id":"images","input_modalities":["text","image"],"efforts":["high"]},
            {"id":"negative","input_modalities":["text","image"],
                "capabilities":{"image_input":{"supported":false}}},
            {"id":"unknown","input_modalities":null},
            {"id":"invalid","input_modalities":["text",true],"efforts":["high",42]},
            {"id":"absent"}
        ],"has_more":false})
        .to_string();
        let (endpoint, _, server) = listing_server(vec![(200, body)]);
        let mut selected = anthropic_adapter(endpoint);
        selected.catalog.input_modalities_path = Some("input_modalities".into());
        selected.catalog.effort_levels_path = Some("efforts".into());
        selected.catalog.effort_format = EffortLevelsFormat::Array;
        let listing = discover_connected_models_with_key(&selected, Some("fixture-key")).unwrap();
        let text = &listing.models["text-only"];
        assert!(text.efforts_declared);
        assert!(text.supported_efforts.is_empty());
        assert!(text.capabilities_declared);
        assert_eq!(text.declared_capabilities.get("image_input"), Some(&false));
        assert_eq!(
            listing.models["images"]
                .declared_capabilities
                .get("image_input"),
            Some(&true)
        );
        assert_eq!(
            listing.models["negative"]
                .declared_capabilities
                .get("image_input"),
            Some(&false)
        );
        for id in ["unknown", "invalid"] {
            assert!(listing.models[id].capabilities_declared);
            assert!(!listing.models[id]
                .declared_capabilities
                .contains_key("image_input"));
        }
        assert!(!listing.models["invalid"].efforts_declared);
        assert!(listing.models["invalid"].supported_efforts.is_empty());
        assert!(listing.models["absent"].capabilities_declared);
        let absent = serde_json::json!({"capabilities":{"image_input":{"supported":true}}});
        let unmapped = CatalogMapping::default();
        assert!(!mapped_capabilities_declared(&absent, &unmapped));
        assert!(!mapped_limits_declared(&unmapped));
        server.join().unwrap();
    }

    #[test]
    fn anthropic_direct_discovery_reads_all_pages_with_exact_capabilities_and_header_auth() {
        let (endpoint, requests, server) = listing_server(complete_listing_pages());
        let selected = anthropic_adapter(endpoint);
        validate_provider_definition(&selected).unwrap();
        let listing = discover_connected_models_with_key(&selected, Some("fixture-key")).unwrap();
        assert_eq!(listing.provider_id, "new-gateway");
        assert_eq!(listing.models.len(), 2);
        let first = &listing.models["opaque-model"];
        assert_eq!(first.name, "First exact model");
        assert_eq!(first.max_input_tokens, Some(65536));
        assert_eq!(first.context_window_tokens, None);
        assert_eq!(first.max_output_tokens, Some(4096));
        assert_eq!(first.supported_efforts, ["low", "turbo_2028", "xhigh"]);
        assert!(first.efforts_declared);
        assert!(first.capabilities_declared);
        assert!(first.declared_capabilities["image_input"]);
        assert!(!first.declared_capabilities["pdf_input"]);
        assert_eq!(first.tool_call_declared, None);
        let adjacent = &listing.models["opaque-model-preview"];
        assert_eq!(adjacent.max_input_tokens, None);
        assert_eq!(adjacent.max_output_tokens, None);
        assert!(adjacent.supported_efforts.is_empty());
        assert!(adjacent.efforts_declared);
        assert!(adjacent.capabilities_declared);
        assert!(!adjacent.declared_capabilities["image_input"]);
        assert!(!listing.models.contains_key("opaque-model-latest"));
        for (index, expected) in [
            "GET /v1/models?limit=1000 ",
            "GET /v1/models?limit=1000&after_id=opaque-model ",
        ]
        .iter()
        .enumerate()
        {
            let request = requests
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .to_ascii_lowercase();
            assert!(
                request.starts_with(&expected.to_ascii_lowercase()),
                "page {index}"
            );
            assert!(request.contains("x-api-key: fixture-key\r\n"));
            assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("if-none-match:"));
        }
        server.join().unwrap();
        let saved = serde_json::to_string(&selected).unwrap();
        assert!(!saved.contains("fixture-key"));
    }

    #[test]
    fn anthropic_catalog_redirect_does_not_forward_header_credentials() {
        use std::io::{BufRead, BufReader};
        let destination = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let location = format!("http://{}/stolen", destination.local_addr().unwrap());
        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", source.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = source.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                request.push_str(&line);
                assert!(request.len() < 16 * 1024);
                if line == "\r\n" {
                    break;
                }
            }
            write!(socket,
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            request
        });
        let error = discover_connected_models_with_key(
            &anthropic_adapter(endpoint),
            Some("redirect-fixture-secret"),
        )
        .unwrap_err();
        assert_eq!(error, "HTTP 302");
        assert!(!error.contains("redirect-fixture-secret"));
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("x-api-key: redirect-fixture-secret\r\n"));
        assert_eq!(
            destination.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn anthropic_listing_rejects_incomplete_repeated_and_duplicate_pages() {
        for (first, second) in [
            (r#"{"data":[{"id":"a"}],"has_more":true}"#, None),
            (
                r#"{"data":[{"id":"a"}],"has_more":"true","last_id":"a"}"#,
                None,
            ),
            (r#"{"data":[],"has_more":true,"last_id":"a"}"#, None),
            (
                r#"{"data":[{"id":"a"}],"has_more":true,"last_id":"a"}"#,
                Some(r#"{"data":[{"id":"b"}],"has_more":true,"last_id":"a"}"#),
            ),
            (
                r#"{"data":[{"id":"a"}],"has_more":true,"last_id":"a"}"#,
                Some(r#"{"data":[{"id":"a"}],"has_more":false,"last_id":"a"}"#),
            ),
        ] {
            let mut pages = vec![(200, first.to_owned())];
            pages.extend(second.map(|body| (200, body.to_owned())));
            let (endpoint, _, server) = listing_server(pages);
            assert!(discover_connected_models_with_key(
                &anthropic_adapter(endpoint),
                Some("fixture-key")
            )
            .is_err());
            server.join().unwrap();
        }
    }

    #[test]
    fn catalog_cursor_is_encoded_and_total_response_quota_applies_across_pages() {
        let bodies = vec![
            (
                200,
                r#"{"data":[{"id":"a"}],"has_more":true,"last_id":"a&limit=1/../?"}"#.to_owned(),
            ),
            (
                200,
                r#"{"data":[{"id":"b"}],"has_more":false,"last_id":"b"}"#.to_owned(),
            ),
        ];
        let max_bytes = bodies
            .iter()
            .map(|(_, body)| body.len() as u64)
            .sum::<u64>()
            - 1;
        let (endpoint, requests, server) = listing_server(bodies);
        let selected = anthropic_adapter(endpoint);
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        assert!(fetch_model_listing_pages(
            &client,
            &format!("{}/models", selected.endpoint),
            &selected.auth,
            &selected.headers,
            Some("fixture-key"),
            &selected.catalog,
            Some("\"old-page-etag\""),
            max_bytes,
            Duration::from_secs(3)
        )
        .is_err());
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("GET /v1/models?limit=1000&after_id=a%26limit%3D1%2F..%2F%3F "));
        assert!(!request.to_ascii_lowercase().contains("if-none-match:"));
        server.join().unwrap();
    }

    #[test]
    fn registry_transport_contract_rejects_cross_protocol_auth_and_literal_secrets() {
        let selected = adapter("anthropic").unwrap();
        for auth in [
            ProviderAuth::Bearer,
            ProviderAuth::Header {
                name: "authorization".into(),
            },
        ] {
            assert!(validate_transport_settings(
                NativeWireApi::AnthropicMessages,
                &auth,
                &selected.headers
            )
            .is_err());
        }
        assert!(validate_transport_settings(
            NativeWireApi::ChatCompletions,
            &selected.auth,
            &BTreeMap::new()
        )
        .is_err());
        assert!(validate_transport_settings(
            NativeWireApi::Responses,
            &ProviderAuth::Bearer,
            &selected.headers
        )
        .is_err());
        for (name, value) in [
            ("x-api-key", "literal-secret"),
            ("authorization", "Bearer literal-secret"),
            ("anthropic-beta", "unsupported-options"),
            ("anthropic-version", "2099-01-01"),
            ("anthropic-workspace-id", "workspace\r\nx-api-key: leaked"),
        ] {
            let mut headers = selected.headers.clone();
            headers.insert(name.into(), value.into());
            assert!(
                validate_transport_settings(selected.wire_api, &selected.auth, &headers).is_err()
            );
        }
        let root = tempfile::tempdir().unwrap();
        let custom = anthropic_adapter("https://gateway.example/v1".into());
        fs::write(
            root.path().join(REGISTRY_FILE),
            serde_json::to_vec(&serde_json::json!({"version":1,"providers":[custom]})).unwrap(),
        )
        .unwrap();
        let registry = ProviderRegistry::load(root.path()).unwrap();
        let settings = NativeProviderSettings {
            provider_id: "new-gateway".into(),
            model_id: "opaque-model".into(),
            ..Default::default()
        };
        let connection = settings
            .resolve_with_registry(&registry, |env| {
                assert_eq!(env, "NEW_GATEWAY_FIXTURE_KEY");
                Some("fixture-key".into())
            })
            .unwrap();
        assert_eq!(connection.wire_api, NativeWireApi::AnthropicMessages);
        assert_eq!(connection.auth, selected.auth);
        assert_eq!(connection.headers, selected.headers);
        assert!(!fs::read_to_string(root.path().join(REGISTRY_FILE))
            .unwrap()
            .contains("fixture-key"));

        let mut fields = selected.clone();
        fields
            .chat_request_fields
            .insert("thinking".into(), serde_json::json!({"type":"adaptive"}));
        fields
            .chat_request_fields
            .insert("output_config".into(), serde_json::json!({"effort":"high"}));
        validate_provider_definition(&fields).unwrap();
        for (name, value) in [
            ("system", serde_json::json!("override")),
            ("metadata", serde_json::json!({"user_id":"not-implemented"})),
            ("thinking", serde_json::json!(true)),
            ("output_config", serde_json::json!("high")),
        ] {
            let mut invalid = fields.clone();
            invalid.chat_request_fields.insert(name.into(), value);
            assert!(validate_provider_definition(&invalid).is_err());
        }
    }

    #[test]
    fn provider_registry_keeps_credentials_provider_specific() {
        let registry = ProviderRegistry::builtin();
        assert_eq!(adapter("openai").unwrap().key_env, "OPENAI_API_KEY");
        let anthropic = registry.adapter("anthropic").unwrap();
        assert_eq!(anthropic.key_env, "ANTHROPIC_API_KEY");
        assert_eq!(
            anthropic.auth,
            ProviderAuth::Header {
                name: "x-api-key".into()
            }
        );
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
            if let Some(output) = row.max_output_tokens {
                assert!(row.context_window_tokens.unwrap() > output);
            }
            assert!(row.source_url.starts_with(match row.provider_id {
                "openai" => "https://developers.openai.com/api/docs/models/",
                "deepseek" => "https://api-docs.deepseek.com/api/",
                "minimax" => "https://platform.minimax.io/docs/",
                "zai" => "https://docs.z.ai/",
                _ => panic!("unreviewed provider in executable capability matrix"),
            }));
            let Some(budget) = verified_token_budget(row.provider_id, row.model_id) else {
                assert!(row.max_output_tokens.is_none());
                continue;
            };
            assert_eq!(
                budget.context_window_tokens,
                row.context_window_tokens.unwrap()
            );
            assert_eq!(budget.output_reserve_tokens, row.max_output_tokens.unwrap());
        }
        assert_eq!(seen.len(), MODEL_CAPABILITIES.len());
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
        // The separately reviewed DeepSeek route retains its explicit
        // disabled-thinking policy; new providers do not change that contract.
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
    fn minimax_and_zai_routes_are_explicit_general_api_contracts() {
        let minimax = adapter("minimax").unwrap();
        let zai = adapter("zai").unwrap();
        for entry in [&minimax, &zai] {
            validate_provider_definition(entry).unwrap();
            assert_eq!(entry.auth, ProviderAuth::Bearer);
            assert_eq!(entry.wire_api, NativeWireApi::ChatCompletions);
            assert!(!entry.catalog_anonymous && !entry.key_optional && !entry.user_endpoint);
        }
        assert_eq!(minimax.endpoint, "https://api.minimax.io/v1");
        assert_eq!(minimax.key_env, "MINIMAX_API_KEY");
        assert_eq!(minimax.model_list_path.as_deref(), Some("models"));
        assert_eq!(minimax.chat_request_fields["reasoning_split"], true);
        assert_eq!(zai.endpoint, "https://api.z.ai/api/paas/v4");
        assert_eq!(zai.key_env, "ZAI_API_KEY");
        assert!(zai.model_list_path.is_none());
        assert!(discover_connected_models_with_key(&zai, Some("fake-no-request")).unwrap_err().contains("No hay catálogo"));
        assert_eq!(zai.chat_request_fields["thinking"], serde_json::json!({"type":"enabled","clear_thinking":false}));
        assert_eq!(zai.for_model("glm-5.3").unwrap().chat_request_fields["tool_stream"], true);
        assert!(!zai.for_model("glm-future").unwrap().chat_request_fields.contains_key("tool_stream"));
        assert_eq!(native_route_execution("zai", "glm-5.3", false), NativeRouteExecution::Verified);
        assert_eq!(native_route_execution("minimax", "MiniMax-M3", false), NativeRouteExecution::Verified);
        assert!(native_model_capabilities("zai", "glm-5.3-future").is_none());
        assert!(native_model_capabilities("minimax", "MiniMax-M3.1-Flash-Preview").is_none(), "subscription-only models are not seeded in the general API");
        assert_eq!(native_model_capabilities("zai", "glm-5.3").unwrap().supported_efforts, &["low", "high", "max"]);
    }

    #[test]
    fn minimax_models_contract_uses_authenticated_official_shape_without_invented_capabilities() {
        let (endpoint, requests, server) = listing_server(vec![(200, r#"{"object":"list","data":[{"id":"MiniMax-M3","object":"model","created":1780272000,"owned_by":"minimax"},{"id":"MiniMax-M2.7","object":"model","created":1773799200,"owned_by":"minimax"}]}"#.into())]);
        let mut selected = adapter("minimax").unwrap();
        selected.endpoint = endpoint;
        let models = discover_connected_models_with_key(&selected, Some("fixture-minimax-key")).unwrap();
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("GET /v1/models "));
        assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-minimax-key"));
        assert_eq!(models.models.len(), 2);
        let model = &models.models["MiniMax-M3"];
        assert!(model.context_window_tokens.is_none());
        assert!(model.max_output_tokens.is_none());
        assert!(model.supported_efforts.is_empty());
        assert!(model.tool_call_declared.is_none());
        server.join().unwrap();
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
    fn output_limits_reopen_and_match_exact_route() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native-output-limits.json");
        fs::write(&path, br#"{"version":1,"limits":[{"provider_id":"new-provider","model_id":"new-model","wire_api":"chat_completions","field":"max_completion_tokens","tokens":4096}]}"#).unwrap();
        let read = || {
            configured_output_limit(
                root.path(),
                "new-provider",
                "new-model",
                NativeWireApi::ChatCompletions,
            )
        };
        assert_eq!(read().unwrap().unwrap().tokens, 4096);
        assert_eq!(read().unwrap().unwrap().tokens, 4096);
        assert!(configured_output_limit(
            root.path(),
            "new-provider",
            "new-model-preview",
            NativeWireApi::ChatCompletions
        )
        .unwrap()
        .is_none());
        assert!(configured_output_limit(
            root.path(),
            "other",
            "new-model",
            NativeWireApi::ChatCompletions
        )
        .unwrap()
        .is_none());
        assert!(configured_output_limit(
            root.path(),
            "new-provider",
            "new-model",
            NativeWireApi::Responses
        )
        .is_err());
        for (field, tokens) in [("max_completion_tokens", 0), ("messages", 4096)] {
            fs::write(&path, serde_json::to_vec(&serde_json::json!({"version":1,"limits":[{"provider_id":"new-provider","model_id":"new-model","wire_api":"chat_completions","field":field,"tokens":tokens}]})).unwrap()).unwrap();
            assert!(read().is_err());
        }
        fs::write(&path, "{truncated").unwrap();
        assert!(read().is_err());
    }

    #[test]
    fn anthropic_output_limit_rejects_unusable_values_without_overwriting_saved_route() {
        let root = tempfile::tempdir().unwrap();
        let valid = NativeOutputLimit {
            provider_id: "new-gateway".into(),
            model_id: "opaque-model".into(),
            wire_api: NativeWireApi::AnthropicMessages,
            field: "max_tokens".into(),
            tokens: 512,
        };
        save_output_limit(
            root.path(),
            "new-gateway",
            "opaque-model",
            Some(valid.clone()),
        )
        .unwrap();
        let path = root.path().join("native-output-limits.json");
        let original = fs::read(&path).unwrap();
        for (field, tokens) in [
            ("max_tokens", 0),
            ("max_tokens", u64::from(u32::MAX) + 1),
            ("max_output_tokens", 512),
        ] {
            let invalid = NativeOutputLimit {
                field: field.into(),
                tokens,
                ..valid.clone()
            };
            assert!(
                save_output_limit(root.path(), "new-gateway", "opaque-model", Some(invalid))
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(
                configured_output_limit(
                    root.path(),
                    "new-gateway",
                    "opaque-model",
                    NativeWireApi::AnthropicMessages
                )
                .unwrap(),
                Some(valid.clone())
            );
        }
        let upper_bound = NativeOutputLimit {
            tokens: u64::from(u32::MAX),
            ..valid
        };
        save_output_limit(
            root.path(),
            "new-gateway",
            "opaque-model",
            Some(upper_bound.clone()),
        )
        .unwrap();
        assert_eq!(
            configured_output_limit(
                root.path(),
                "new-gateway",
                "opaque-model",
                NativeWireApi::AnthropicMessages
            )
            .unwrap(),
            Some(upper_bound)
        );
    }

    #[test]
    fn connected_listing_preserves_exact_limits_without_name_inference() {
        let adapter = ProviderRegistry::builtin().adapter("groq").unwrap().clone();
        let listing = parse_connected_models_for_adapter(
            &adapter,
            br#"{"data":[
            {"id":"unknown-new","context_window":65536,"max_completion_tokens":4096},
            {"id":"unknown-new-preview"},
            {"id":"invalid","context_window":-1,"max_completion_tokens":"4096"},
            {"id":"zero","context_window":0,"max_completion_tokens":0}
        ]}"#,
        )
        .unwrap();
        assert_eq!(
            listing.models["unknown-new"].context_window_tokens,
            Some(65536)
        );
        assert_eq!(listing.models["unknown-new"].max_output_tokens, Some(4096));
        for id in ["unknown-new-preview", "invalid", "zero"] {
            assert_eq!(listing.models[id].context_window_tokens, None);
            assert_eq!(listing.models[id].max_output_tokens, None);
        }
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
