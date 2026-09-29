//! Persistent connection choice for the native harness.
//!
//! The local provider catalog supplies discovery metadata, not trusted connection
//! targets. Only adapters listed here may use provider credentials. A custom
//! endpoint is an explicit user choice and uses the legacy OpenIDE_API_KEY.

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
const MAX_DISCOVERY_BYTES: u64 = 4 * 1024 * 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Adapter {
    pub id: &'static str,
    pub name: &'static str,
    pub endpoint: &'static str,
    pub key_env: &'static str,
    /// The request protocol implemented by this adapter, not a promise that
    /// every model exposed by its provider supports our tool loop.
    pub wire_api: NativeWireApi,
}

const ADAPTERS: [Adapter; 4] = [
    Adapter {
        id: "openai",
        name: "OpenAI",
        endpoint: "https://api.openai.com/v1",
        key_env: "OPENAI_API_KEY",
        wire_api: NativeWireApi::Responses,
    },
    Adapter {
        id: "openrouter",
        name: "OpenRouter",
        endpoint: "https://openrouter.ai/api/v1",
        key_env: "OPENROUTER_API_KEY",
        wire_api: NativeWireApi::ChatCompletions,
    },
    Adapter {
        id: "deepseek",
        name: "DeepSeek",
        endpoint: "https://api.deepseek.com",
        key_env: "DEEPSEEK_API_KEY",
        wire_api: NativeWireApi::ChatCompletions,
    },
    Adapter {
        id: "custom",
        name: "Personalizado",
        endpoint: DEFAULT_CUSTOM_ENDPOINT,
        key_env: "OPENIDE_API_KEY",
        wire_api: NativeWireApi::ChatCompletions,
    },
];

pub(crate) fn adapter(provider_id: &str) -> Option<Adapter> {
    ADAPTERS
        .iter()
        .copied()
        .find(|entry| entry.id == provider_id)
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
        .and_then(|selected| read_key(selected.key_env))
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
// therefore are intentionally not advertised here.
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
/// OpenRouter routes to upstream providers and custom endpoints are explicit
/// user choices, so neither inherits native model limits from discovery metadata.
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
    /// A previously configured OpenRouter/custom model, whose capabilities
    /// must be checked by the endpoint when a request is sent.
    ExplicitUnverified,
    Unsupported,
}

pub(crate) fn native_route_execution(
    provider_id: &str,
    model_id: &str,
    explicitly_configured: bool,
) -> NativeRouteExecution {
    let Some(selected) = adapter(provider_id) else {
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
    } else if explicitly_configured && matches!(provider_id, "openrouter" | "custom") {
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
    /// OpenRouter declares `tools` in `supported_parameters` for this model.
    /// Other providers do not expose an equivalent field in `/models`.
    pub provider_reports_tools: bool,
    pub text_input_output: bool,
}

/// Call on a worker, never from rendering. OpenRouter's `/models/user` filters
/// the list by the authenticated account's preferences and guardrails; OpenAI
/// and DeepSeek expose `/models`. Key presence, a successful listing, and
/// actual permission to generate are distinct states.
pub(crate) fn discover_connected_models(
    provider_id: &str,
) -> Result<ProviderModelDiscovery, String> {
    let selected = adapter(provider_id)
        .filter(|adapter| adapter.id != "custom")
        .ok_or_else(|| format!("No hay catálogo autenticado para {provider_id}"))?;
    let key = std::env::var(selected.key_env)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Falta {}", selected.key_env))?;
    if key.len() > 4_096 || key.chars().any(char::is_control) {
        return Err(format!("{} tiene un formato inválido", selected.key_env));
    }
    let path = if provider_id == "openrouter" {
        "models/user"
    } else {
        "models"
    };
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
    parse_connected_models(provider_id, &bytes)
}

fn parse_connected_models(
    provider_id: &str,
    bytes: &[u8],
) -> Result<ProviderModelDiscovery, String> {
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Catálogo de {provider_id} inválido: {error}"))?;
    let entries = document
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Catálogo de {provider_id} sin lista de modelos"))?;
    if entries.len() > 5_000 {
        return Err("El catálogo autenticado contiene demasiados modelos".to_owned());
    }
    let mut models = std::collections::BTreeMap::new();
    for entry in entries {
        let Some(id) = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        else {
            continue;
        };
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| {
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
            })
            .unwrap_or(id);
        let provider_reports_tools = provider_id == "openrouter"
            && entry
                .get("supported_parameters")
                .and_then(Value::as_array)
                .is_some_and(|parameters| {
                    parameters
                        .iter()
                        .any(|value| value.as_str() == Some("tools"))
                });
        let text_input_output = provider_id == "openrouter"
            && entry.get("architecture").is_some_and(|architecture| {
                ["input_modalities", "output_modalities"]
                    .iter()
                    .all(|field| {
                        architecture
                            .get(*field)
                            .and_then(Value::as_array)
                            .is_some_and(|modalities| {
                                modalities
                                    .iter()
                                    .any(|value| value.as_str() == Some("text"))
                            })
                    })
            });
        models.insert(
            id.to_owned(),
            DiscoveredModel {
                id: id.to_owned(),
                name: name.to_owned(),
                provider_reports_tools,
                text_input_output,
            },
        );
    }
    Ok(ProviderModelDiscovery {
        provider_id: provider_id.to_owned(),
        models,
    })
}

pub(crate) fn openai_native_compatible_model(model_id: &str) -> bool {
    native_model_capabilities("openai", model_id).is_some()
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
        normalized.validated_endpoint()?;
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

    fn resolve_with_key(
        &self,
        read_key: impl FnOnce(&str) -> Option<String>,
    ) -> Result<RuntimeConnection, String> {
        let endpoint = self.validated_endpoint()?;
        let selected = adapter(self.provider_id.trim()).expect("validated provider");
        Ok(RuntimeConnection {
            endpoint,
            api_key: read_key(selected.key_env)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.trim().is_empty()),
            model_id: self.model_id.trim().to_owned(),
            wire_api: selected.wire_api,
        })
    }

    pub fn key_available(&self) -> bool {
        self.resolve()
            .is_ok_and(|connection| connection.api_key.is_some())
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

    fn validated_endpoint(&self) -> Result<String, String> {
        if self.model_id.trim().is_empty() {
            return Err("Selecciona un modelo para el agente nativo".to_owned());
        }
        let provider_id = self.provider_id.trim();
        let selected = adapter(provider_id)
            .ok_or_else(|| format!("Proveedor nativo no compatible: {provider_id}"))?;
        if provider_id == "openai" && !openai_native_compatible_model(self.model_id.trim()) {
            return Err(
                "Este modelo de OpenAI aún no está verificado para herramientas y streaming de Responses"
                    .to_owned(),
            );
        }
        if provider_id == "deepseek" && !deepseek_native_compatible_model(self.model_id.trim()) {
            return Err(
                "Este modelo de DeepSeek no es un ID canónico verificado para el harness nativo"
                    .to_owned(),
            );
        }
        if let Some(capabilities) = native_model_capabilities(provider_id, self.model_id.trim()) {
            if capabilities.wire_api != selected.wire_api
                || !capabilities.function_tools
                || !capabilities.streaming
            {
                return Err("Este modelo aún no está verificado para herramientas y streaming del adaptador nativo".to_owned());
            }
        }
        if provider_id == "custom" {
            validate_custom_endpoint(&self.custom_endpoint)
        } else {
            Ok(selected.endpoint.to_owned())
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
    fn adapter_allowlist_keeps_credentials_provider_specific() {
        assert_eq!(ADAPTERS.len(), 4);
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
            NativeRouteExecution::Unsupported
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
    fn openai_models_need_explicit_responses_stream_review() {
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
            assert!(settings.resolve().is_err());
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
    fn deepseek_aliases_are_visible_but_not_native_connections() {
        assert!(deepseek_native_compatible_model("deepseek-flash"));
        assert!(deepseek_native_compatible_model("deepseek-v4-pro"));
        assert!(!deepseek_native_compatible_model("deepseek-v4-flash"));
    }
}
