//! Immutable read-only authority for one native automation admission.
//!
//! Reconstruction reads neither Settings, the provider registry nor the catalog.
//! Credentials are resolved only after the complete recorded contract validates.
use crate::{
    app::{native_model_backend, native_output_maximum, native_request_capabilities},
    model_catalog::CatalogSnapshot,
    native_provider::{
        self, NativeOutputLimit, NativeWireApi, ProviderAuth, ProviderModelDiscovery,
        RuntimeConnection,
    },
};
use openide_core::{
    native_automation_first_turn_id, native_automation_request_digest, system_instructions_digest,
    CheckpointCompatibility, NativeAutomationOrigin, Session, TurnMode,
};
use openide_harness::{
    update_plan_tool_definition, AnthropicMessagesBackend, CommandHooks, HostedWebSearchConfig,
    HostedWebSearchContextSize, HostedWebSearchLocation, HostedWebSearchMode, LocalTools,
    ModelBackend, ModelRequestCapabilities, OpenAiCompatibleBackend, PermissionPolicy,
    ResponsesBackend, RunLimits, TokenBudgetPolicy, ToolCatalog, ToolExecutor, ToolPermissions,
    DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::{Component as PathComponent, Path, PathBuf},
    sync::Arc,
};

const MAX_AUTHORITY_BYTES: usize = 512 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;

// These belong to the persisted authority profile, not mutable host defaults.
fn readonly_v1_limits() -> RunLimits {
    RunLimits {
        max_model_steps: 24,
        max_tool_calls: 64,
        max_tool_output_chars: 16_000,
    }
}

const READ_ONLY_V1: &str = "read_only_v1";
/// Edits inside the workspace; commands only in the host's sandbox, which
/// writes nowhere else and opens no socket. Opted into per automation.
const WORKSPACE_WRITE_V1: &str = "workspace_write_v1";

fn workspace_write_v1_limits() -> RunLimits {
    RunLimits {
        max_model_steps: 48,
        max_tool_calls: 128,
        max_tool_output_chars: 16_000,
    }
}

fn profile_limits(profile: &str) -> RunLimits {
    if profile == WORKSPACE_WRITE_V1 {
        workspace_write_v1_limits()
    } else {
        readonly_v1_limits()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenCapabilities {
    tool_calls: bool,
    reasoning_efforts: Vec<String>,
    tool_call_reasoning_efforts: Option<Vec<String>>,
    default_tool_call_effort: Option<String>,
}

impl From<ModelRequestCapabilities> for FrozenCapabilities {
    fn from(cap: ModelRequestCapabilities) -> Self {
        Self {
            tool_calls: cap.tool_calls,
            reasoning_efforts: cap.reasoning_efforts,
            tool_call_reasoning_efforts: cap.tool_call_reasoning_efforts,
            default_tool_call_effort: cap.default_tool_call_effort,
        }
    }
}

impl FrozenCapabilities {
    fn runtime(&self) -> ModelRequestCapabilities {
        ModelRequestCapabilities {
            tool_calls: self.tool_calls,
            reasoning_efforts: self.reasoning_efforts.clone(),
            tool_call_reasoning_efforts: self.tool_call_reasoning_efforts.clone(),
            default_tool_call_effort: self.default_tool_call_effort.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_limit_tokens: Option<u64>,
    context_window_tokens: u64,
    output_reserve_tokens: u64,
    tool_schema_reserve_tokens: u64,
    auto_compact_token_limit: Option<u64>,
}

impl From<TokenBudgetPolicy> for FrozenBudget {
    fn from(b: TokenBudgetPolicy) -> Self {
        Self {
            input_limit_tokens: b.input_limit_tokens,
            context_window_tokens: b.context_window_tokens,
            output_reserve_tokens: b.output_reserve_tokens,
            tool_schema_reserve_tokens: b.tool_schema_reserve_tokens,
            auto_compact_token_limit: b.auto_compact_token_limit,
        }
    }
}

impl FrozenBudget {
    fn runtime(&self) -> TokenBudgetPolicy {
        TokenBudgetPolicy {
            input_limit_tokens: self.input_limit_tokens,
            context_window_tokens: self.context_window_tokens,
            output_reserve_tokens: self.output_reserve_tokens,
            tool_schema_reserve_tokens: self.tool_schema_reserve_tokens,
            auto_compact_token_limit: self.auto_compact_token_limit,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenLimits {
    max_model_steps: usize,
    max_tool_calls: usize,
    max_tool_output_chars: usize,
}

impl From<RunLimits> for FrozenLimits {
    fn from(l: RunLimits) -> Self {
        Self {
            max_model_steps: l.max_model_steps,
            max_tool_calls: l.max_tool_calls,
            max_tool_output_chars: l.max_tool_output_chars,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenLocation {
    country: Option<String>,
    region: Option<String>,
    city: Option<String>,
    timezone: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenHostedSearch {
    mode: HostedWebSearchMode,
    context_size: Option<HostedWebSearchContextSize>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    location: Option<FrozenLocation>,
    include_sources: bool,
}

impl From<HostedWebSearchConfig> for FrozenHostedSearch {
    fn from(c: HostedWebSearchConfig) -> Self {
        Self {
            mode: c.mode,
            context_size: c.context_size,
            allowed_domains: c.allowed_domains,
            blocked_domains: c.blocked_domains,
            include_sources: c.include_sources,
            location: c.location.map(|l| FrozenLocation {
                country: l.country,
                region: l.region,
                city: l.city,
                timezone: l.timezone,
            }),
        }
    }
}

impl FrozenHostedSearch {
    fn runtime(&self) -> HostedWebSearchConfig {
        HostedWebSearchConfig {
            mode: self.mode,
            context_size: self.context_size,
            allowed_domains: self.allowed_domains.clone(),
            blocked_domains: self.blocked_domains.clone(),
            include_sources: self.include_sources,
            location: self.location.as_ref().map(|l| HostedWebSearchLocation {
                country: l.country.clone(),
                region: l.region.clone(),
                city: l.city.clone(),
                timezone: l.timezone.clone(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenNativeAutomationAuthorityV1 {
    version: u16,
    authority_profile: String,
    mode: TurnMode,
    provider_id: String,
    credential_env: String,
    credential_optional: bool,
    wire_api: NativeWireApi,
    #[serde(default, skip_serializing_if = "is_bearer")]
    auth: ProviderAuth,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_input_enabled: Option<bool>,
    endpoint: String,
    canonical_workspace: PathBuf,
    model_id: String,
    capabilities: FrozenCapabilities,
    chat_request_fields: BTreeMap<String, Value>,
    configured_output_limit: Option<NativeOutputLimit>,
    model_output_maximum: Option<u64>,
    effort: Option<String>,
    hosted_web_search: Option<FrozenHostedSearch>,
    budget: Option<FrozenBudget>,
    limits: FrozenLimits,
    instructions: String,
    instructions_digest: String,
    prompt: String,
    prompt_digest: String,
    first_turn_id: String,
    backend_profile_fingerprint: String,
    catalog_digest: String,
    permission_policy_digest: String,
    hooks_digest: String,
}

fn is_bearer(auth: &ProviderAuth) -> bool {
    matches!(auth, ProviderAuth::Bearer)
}

fn readonly_tools() -> Arc<dyn ToolExecutor> {
    Arc::new(LocalTools::new().with_permissions(ToolPermissions {
        read_file: true,
        list_files: true,
        search_files: true,
        write_file: false,
        run_command: false,
    }))
}

fn readonly_catalog() -> Result<ToolCatalog, String> {
    profile_catalog(READ_ONLY_V1)
}

fn workspace_write_tools() -> Arc<dyn ToolExecutor> {
    Arc::new(
        LocalTools::new()
            .with_permissions(ToolPermissions {
                read_file: true,
                list_files: true,
                search_files: true,
                write_file: true,
                run_command: true,
            })
            .with_command_isolation(openide_harness::CommandIsolation::WorkspaceWriteOffline),
    )
}

fn profile_tools(profile: &str) -> Arc<dyn ToolExecutor> {
    if profile == WORKSPACE_WRITE_V1 {
        workspace_write_tools()
    } else {
        readonly_tools()
    }
}

fn profile_catalog(profile: &str) -> Result<ToolCatalog, String> {
    let mut definitions = profile_tools(profile).definitions();
    definitions.push(update_plan_tool_definition());
    ToolCatalog::new(definitions).map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn prompt_digest(prompt: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"OpenIDE.NativeAutomationPrompt.v1\0");
    hash.update((prompt.len() as u64).to_be_bytes());
    hash.update(prompt.as_bytes());
    hex(&hash.finalize())
}

impl FrozenNativeAutomationAuthorityV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn freeze(
        connection: RuntimeConnection,
        provider_id: &str,
        data_root: &Path,
        workspace: &Path,
        catalog: Option<&CatalogSnapshot>,
        discoveries: &[ProviderModelDiscovery],
        hosted_web_mode: Option<HostedWebSearchMode>,
        budget: Option<TokenBudgetPolicy>,
        instructions: String,
        prompt: String,
        run_id: &str,
        allow_changes: bool,
    ) -> Result<Self, String> {
        // Never wider than asked: without the sandbox there is no write profile.
        if allow_changes && !openide_harness::CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
            return Err("Este equipo no tiene el aislamiento de comandos: la automatización no puede hacer cambios".into());
        }
        let profile = if allow_changes { WORKSPACE_WRITE_V1 } else { READ_ONLY_V1 };
        let adapter = native_provider::adapter_at(data_root, provider_id)?
            .ok_or("Proveedor nativo no registrado")?;
        if adapter.wire_api != connection.wire_api {
            return Err("El protocolo del proveedor cambió".into());
        }
        let capabilities =
            native_request_capabilities(&adapter, &connection.model_id, catalog, discoveries);
        let configured_output_limit = native_provider::configured_output_limit(
            data_root,
            provider_id,
            &connection.model_id,
            connection.wire_api,
        )?;
        let model_output_maximum =
            native_output_maximum(&adapter, &connection.model_id, catalog, discoveries);
        let configured = native_model_backend(
            connection.clone(),
            provider_id,
            None,
            hosted_web_mode,
            data_root,
            catalog,
            discoveries,
        )?;
        let fingerprint = configured
            .request_profile_fingerprint()
            .ok_or("El backend no declara su perfil exacto")?;
        let canonical_workspace = std::fs::canonicalize(workspace)
            .map_err(|e| format!("No se pudo validar el proyecto de la automatización: {e}"))?;
        if !canonical_workspace.is_dir() {
            return Err("El proyecto de la automatización no es un directorio".into());
        }
        let snapshot = Self {
            version: 1,
            authority_profile: profile.into(),
            mode: TurnMode::Build,
            provider_id: adapter.id,
            credential_env: adapter.key_env,
            credential_optional: adapter.key_optional,
            wire_api: connection.wire_api,
            auth: connection.auth,
            headers: connection.headers,
            image_input_enabled: (connection.wire_api == NativeWireApi::AnthropicMessages)
                .then(|| configured.supports_user_images()),
            endpoint: connection.endpoint,
            canonical_workspace,
            model_id: connection.model_id,
            capabilities: capabilities.into(),
            chat_request_fields: adapter.chat_request_fields,
            configured_output_limit,
            model_output_maximum,
            effort: None,
            hosted_web_search: (connection.wire_api == NativeWireApi::Responses)
                .then_some(hosted_web_mode)
                .flatten()
                .map(HostedWebSearchConfig::new)
                .map(Into::into),
            budget: budget.map(Into::into),
            limits: profile_limits(profile).into(),
            instructions_digest: system_instructions_digest(&instructions)
                .ok_or("Las instrucciones superan el límite")?,
            instructions,
            prompt_digest: prompt_digest(&prompt),
            prompt,
            first_turn_id: native_automation_first_turn_id(run_id).map_err(|e| e.to_string())?,
            backend_profile_fingerprint: hex(&fingerprint),
            catalog_digest: profile_catalog(profile)?.digest_hex(),
            permission_policy_digest: PermissionPolicy::new().digest_hex(),
            hooks_digest: CommandHooks::default()
                .digest_hex()
                .map_err(|e| e.to_string())?,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !matches!(self.authority_profile.as_str(), READ_ONLY_V1 | WORKSPACE_WRITE_V1)
            || self.mode != TurnMode::Build
            || self.effort.is_some()
        {
            return Err("Autoridad nativa de automatización incompatible".into());
        }
        // Recovery compares the already-canonical, recorded path without
        // consulting mutable files or requiring a still-existing project.
        if !self.canonical_workspace.is_absolute()
            || self
                .canonical_workspace
                .to_str()
                .is_none_or(|path| path.len() > 32 * 1024)
            || self
                .canonical_workspace
                .components()
                .any(|part| matches!(part, PathComponent::CurDir | PathComponent::ParentDir))
        {
            return Err("Proyecto congelado de automatización inválido".into());
        }
        if self.provider_id.is_empty()
            || self.provider_id.len() > 64
            || !self
                .provider_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
            || self.credential_env.is_empty()
            || self.credential_env.len() > 128
            || !self
                .credential_env
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || self.model_id.is_empty()
            || self.model_id.len() > 256
            || self.model_id.trim() != self.model_id
            || self.model_id.chars().any(char::is_control)
        {
            return Err("Ruta o variable de credencial inválida en la automatización".into());
        }
        let url = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| "Endpoint de automatización inválido")?;
        if self.endpoint.len() > 2048
            || self.endpoint.trim() != self.endpoint
            || self.endpoint.chars().any(char::is_whitespace)
            || !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "El endpoint debe ser HTTP(S) sin credenciales, búsqueda ni fragmento".into(),
            );
        }
        if url.scheme() == "http" {
            let host = url
                .host_str()
                .unwrap_or_default()
                .trim_start_matches('[')
                .trim_end_matches(']');
            if !host.eq_ignore_ascii_case("localhost")
                && !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
            {
                return Err("HTTP sólo se permite en localhost".into());
            }
        }
        if self.prompt.trim().is_empty()
            || self.prompt.len() > MAX_PROMPT_BYTES
            || prompt_digest(&self.prompt) != self.prompt_digest
            || system_instructions_digest(&self.instructions).as_deref()
                != Some(self.instructions_digest.as_str())
        {
            return Err("Texto o digest de la automatización inválido".into());
        }
        if !self
            .first_turn_id
            .strip_prefix("automation-")
            .is_some_and(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            })
        {
            return Err("ID inicial de automatización inválido".into());
        }
        if !self.capabilities.tool_calls {
            return Err("La automatización requiere un modelo con herramientas declaradas".into());
        }
        // Validate full capability structure on both protocols before credentials.
        let mut chat_validator = OpenAiCompatibleBackend::new(&self.endpoint, None, &self.model_id)
            .with_capabilities(self.capabilities.runtime())
            .map_err(|e| e.to_string())?;
        for (field, value) in self
            .chat_request_fields
            .iter()
            .filter(|_| self.wire_api == NativeWireApi::ChatCompletions)
        {
            chat_validator = chat_validator
                .with_request_field(field, value.clone())
                .map_err(|e| e.to_string())?;
        }
        if self.model_output_maximum == Some(0) {
            return Err("Máximo de salida congelado inválido".into());
        }
        if let Some(limit) = &self.configured_output_limit {
            if limit.provider_id != self.provider_id
                || limit.model_id != self.model_id
                || limit.wire_api != self.wire_api
                || limit.tokens == 0
                || limit.tokens > i64::MAX as u64
                || self
                    .model_output_maximum
                    .is_some_and(|maximum| limit.tokens > maximum)
                || match self.wire_api {
                    NativeWireApi::Responses => {
                        limit.field != "max_output_tokens" || limit.tokens < 16
                    }
                    NativeWireApi::ChatCompletions => {
                        !matches!(limit.field.as_str(), "max_tokens" | "max_completion_tokens")
                    }
                    NativeWireApi::AnthropicMessages => {
                        limit.field != "max_tokens" || limit.tokens > u64::from(u32::MAX)
                    }
                }
            {
                return Err("Límite de salida inválido para la ruta congelada".into());
            }
        }
        if self.wire_api != NativeWireApi::Responses && self.hosted_web_search.is_some() {
            return Err("Búsqueda hospedada incompatible con el protocolo".into());
        }
        let valid_auth = match (&self.wire_api, &self.auth) {
            (NativeWireApi::AnthropicMessages, ProviderAuth::Header { name }) => {
                name.eq_ignore_ascii_case("x-api-key") && self.image_input_enabled.is_some()
            }
            (NativeWireApi::Responses | NativeWireApi::ChatCompletions, ProviderAuth::Bearer) => {
                self.headers.is_empty() && self.image_input_enabled.is_none()
            }
            _ => false,
        };
        if !valid_auth {
            return Err(
                "Autenticación o capacidades congeladas incompatibles con el protocolo".into(),
            );
        }
        native_provider::validate_transport_settings(self.wire_api, &self.auth, &self.headers)?;
        if self.limits != FrozenLimits::from(profile_limits(&self.authority_profile)) {
            return Err("Límites de ejecución incompatibles con el perfil de la automatización".into());
        }
        if let Some(budget) = &self.budget {
            if budget.input_limit_tokens == Some(0)
                || (budget.context_window_tokens == 0 && budget.input_limit_tokens.is_none())
                || budget.output_reserve_tokens == 0
                || budget.auto_compact_token_limit == Some(0)
                || (budget.context_window_tokens > 0
                    && budget
                        .output_reserve_tokens
                        .checked_add(budget.tool_schema_reserve_tokens)
                        .is_none_or(|sum| sum >= budget.context_window_tokens))
            {
                return Err("Presupuesto de contexto congelado inválido".into());
            }
        }
        if self.catalog_digest != profile_catalog(&self.authority_profile)?.digest_hex()
            || self.permission_policy_digest != PermissionPolicy::new().digest_hex()
            || self.hooks_digest
                != CommandHooks::default()
                    .digest_hex()
                    .map_err(|e| e.to_string())?
        {
            return Err("El catálogo, los permisos o hooks no corresponden al perfil de la automatización".into());
        }
        let model = self.build_model(None)?;
        if model
            .request_profile_fingerprint()
            .map(|fingerprint| hex(&fingerprint))
            .as_deref()
            != Some(self.backend_profile_fingerprint.as_str())
        {
            return Err("El perfil exacto del backend no coincide".into());
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_AUTHORITY_BYTES {
            return Err("La autoridad de automatización supera el límite".into());
        }
        Ok(())
    }

    fn build_model(&self, key: Option<String>) -> Result<Arc<dyn ModelBackend>, String> {
        match self.wire_api {
            NativeWireApi::Responses => {
                let mut model = ResponsesBackend::new(&self.endpoint, key, &self.model_id);
                if let Some(limit) = &self.configured_output_limit {
                    model = model
                        .with_max_output_tokens(limit.tokens)
                        .map_err(|e| e.to_string())?;
                }
                if let Some(config) = &self.hosted_web_search {
                    model = model
                        .with_hosted_web_search(config.runtime())
                        .map_err(|e| e.to_string())?;
                }
                Ok(Arc::new(model))
            }
            NativeWireApi::ChatCompletions => {
                let mut model = OpenAiCompatibleBackend::new(&self.endpoint, key, &self.model_id)
                    .with_capabilities(self.capabilities.runtime())
                    .map_err(|e| e.to_string())?;
                if let Some(limit) = &self.configured_output_limit {
                    if self.chat_request_fields.keys().any(|field| {
                        matches!(field.as_str(), "max_tokens" | "max_completion_tokens")
                    }) {
                        return Err("Hay dos configuraciones de límite de salida".into());
                    }
                    model = model
                        .with_request_field(&limit.field, serde_json::json!(limit.tokens))
                        .map_err(|e| e.to_string())?;
                }
                for (field, value) in &self.chat_request_fields {
                    model = model
                        .with_request_field(field, value.clone())
                        .map_err(|e| e.to_string())?;
                }
                Ok(Arc::new(model))
            }
            NativeWireApi::AnthropicMessages => {
                let tokens = self
                    .configured_output_limit
                    .as_ref()
                    .map(|limit| limit.tokens)
                    // The same rule as an interactive turn: what the budget reserves.
                    .or_else(|| self.budget.as_ref().map(|budget| budget.output_reserve_tokens))
                    .unwrap_or_else(|| {
                        self.model_output_maximum
                            .map_or(DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS, |max| {
                                max.min(DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS)
                            })
                    });
                let model = AnthropicMessagesBackend::new(&self.endpoint, key, &self.model_id)
                    .with_max_output_tokens(tokens)
                    .map_err(|e| e.to_string())?
                    .with_headers(self.headers.clone())
                    .map_err(|e| e.to_string())?
                    .with_image_input_enabled(
                        self.image_input_enabled
                            .ok_or("Capacidad de imágenes ausente")?,
                    )
                    .with_request_fields(self.chat_request_fields.clone())
                    .map_err(|e| e.to_string())?;
                Ok(Arc::new(model))
            }
        }
    }

    pub(crate) fn model(&self) -> Result<Arc<dyn ModelBackend>, String> {
        self.validate_workspace_for_execution()?;
        self.model_with_credential_reader(|name| std::env::var(name).ok())
    }

    pub(crate) fn validate_workspace_for_execution(&self) -> Result<(), String> {
        self.validate()?;
        let current = std::fs::canonicalize(&self.canonical_workspace)
            .map_err(|error| format!("No se pudo verificar el proyecto registrado: {error}"))?;
        if current != self.canonical_workspace || !current.is_dir() {
            return Err("El proyecto registrado cambió de ubicación o destino; no se ampliará el ámbito de la automatización".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fixture_model_with_key(
        &self,
        key: Option<String>,
    ) -> Result<Arc<dyn ModelBackend>, String> {
        self.model_with_credential_reader(|_| key)
    }

    pub(crate) fn verify_model(&self, model: &dyn ModelBackend) -> Result<(), String> {
        self.validate()?;
        if model.model_id() != self.model_id
            || model
                .request_profile_fingerprint()
                .map(|fingerprint| hex(&fingerprint))
                .as_deref()
                != Some(self.backend_profile_fingerprint.as_str())
        {
            return Err("El modelo no corresponde al perfil exacto de la automatización".into());
        }
        Ok(())
    }

    fn model_with_credential_reader(
        &self,
        read: impl FnOnce(&str) -> Option<String>,
    ) -> Result<Arc<dyn ModelBackend>, String> {
        self.validate()?;
        let key = read(&self.credential_env)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if key.is_none() && !self.credential_optional {
            return Err(format!(
                "Define {} para ejecutar la automatización nativa",
                self.credential_env
            ));
        }
        self.build_model(key)
    }

    pub(crate) fn to_origin(
        &self,
        automation_id: &str,
        run_id: &str,
        scheduled_at_ms: i64,
        manual: bool,
    ) -> Result<NativeAutomationOrigin, String> {
        self.validate()?;
        if native_automation_first_turn_id(run_id).map_err(|e| e.to_string())? != self.first_turn_id
        {
            return Err("La autoridad corresponde a otro dispatch".into());
        }
        NativeAutomationOrigin::new_with_profile(
            if self.allows_changes() {
                openide_core::NativeAutomationAuthorityProfile::WorkspaceWriteV1
            } else {
                openide_core::NativeAutomationAuthorityProfile::ReadOnlyV1
            },
            automation_id,
            run_id,
            scheduled_at_ms,
            manual,
            serde_json::to_string(self).map_err(|e| e.to_string())?,
            &self.prompt,
            &self.model_id,
        )
        .map_err(|e| e.to_string())
    }

    pub(crate) fn from_session(session: &Session) -> Result<Option<Self>, String> {
        let Some(origin) = &session.native_automation else {
            return Ok(None);
        };
        if session.backend != "native" || session.subagent.is_some() {
            return Err("El origen de automatización exige una sesión nativa raíz".into());
        }
        origin
            .validate_session(session)
            .map_err(|e| e.to_string())?;
        let snapshot: Self = serde_json::from_str(&origin.authority_snapshot_json)
            .map_err(|e| format!("Autoridad congelada inválida: {e}"))?;
        snapshot.validate()?;
        if snapshot.canonical_workspace != session.workspace
            || snapshot.first_turn_id != origin.first_turn_id
            || native_automation_request_digest(
                &snapshot.prompt,
                &snapshot.model_id,
                TurnMode::Build,
            )
            .map_err(|e| e.to_string())?
                != origin.first_request_digest
        {
            return Err("La autoridad congelada no coincide con la admisión de origen".into());
        }
        Ok(Some(snapshot))
    }

    pub(crate) fn tools(&self) -> Arc<dyn ToolExecutor> {
        profile_tools(&self.authority_profile)
    }
    pub(crate) fn catalog(&self) -> Result<ToolCatalog, String> {
        self.validate()?;
        profile_catalog(&self.authority_profile)
    }
    /// Whether the run may edit the workspace and run sandboxed commands.
    pub(crate) fn allows_changes(&self) -> bool {
        self.authority_profile == WORKSPACE_WRITE_V1
    }
    pub(crate) fn policy(&self) -> PermissionPolicy {
        PermissionPolicy::new()
    }
    pub(crate) fn limits(&self) -> RunLimits {
        RunLimits {
            max_model_steps: self.limits.max_model_steps,
            max_tool_calls: self.limits.max_tool_calls,
            max_tool_output_chars: self.limits.max_tool_output_chars,
        }
    }
    pub(crate) fn budget(&self) -> Option<TokenBudgetPolicy> {
        self.budget.as_ref().map(FrozenBudget::runtime)
    }
    pub(crate) fn compatibility(&self) -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: self.provider_id.clone(),
            model: self.model_id.clone(),
            context_format_key: match self.wire_api {
                NativeWireApi::Responses => "responses-v1",
                NativeWireApi::ChatCompletions => "chat-completions-v1",
                NativeWireApi::AnthropicMessages => "anthropic-messages-v1",
            }
            .into(),
        }
    }
    pub(crate) fn instructions(&self) -> &str {
        &self.instructions
    }
    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }
    pub(crate) fn first_turn_id(&self) -> &str {
        &self.first_turn_id
    }
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub(crate) fn provider_id(&self) -> &str {
        &self.provider_id
    }
    pub(crate) fn model_id(&self) -> &str {
        &self.model_id
    }
    pub(crate) fn route_snapshot(&self) -> Result<openide_core::ModelRouteSnapshot, String> {
        self.validate()?;
        let profile = self
            .build_model(None)?
            .request_profile_fingerprint()
            .ok_or("El backend congelado no declara un perfil exacto")?;
        openide_core::ModelRouteSnapshot::new(
            &self.provider_id,
            &self.model_id,
            &self.endpoint,
            profile,
        )
        .ok_or_else(|| "Ruta congelada de automatización inválida".into())
    }

    pub(crate) fn recorded_context_digest(&self) -> Result<String, String> {
        self.validate()?;
        let profile = self
            .build_model(None)?
            .request_profile_fingerprint()
            .ok_or("El backend congelado no declara un perfil exacto")?;
        openide_harness::native_model_context_digest_for_recorded_workspace(
            &self.provider_id,
            &self.endpoint,
            &self.model_id,
            TurnMode::Build,
            &self.canonical_workspace,
            &self.catalog()?,
            &self.instructions,
            profile,
        )
        .map_err(|error| error.to_string())
    }
    pub(crate) fn max_output(&self) -> Option<u64> {
        self.model_output_maximum
    }
}

#[cfg(test)]
#[path = "native_automation_tests.rs"]
mod tests;
