use anyhow::{bail, Context, Result};
use openide_harness::{
    AnthropicMessagesBackend, ModelBackend, OpenAiCompatibleBackend, ResponsesBackend,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub desktop_data_root: Option<PathBuf>,
    /// Explicit capability declaration for this exact Chat Completions model.
    /// Responses/Anthropic have their own tool-capable transport contracts.
    #[serde(default)]
    pub tool_calls: bool,
    /// Explicit declaration for this exact model, independently checked against transport support.
    #[serde(default, skip_serializing_if = "is_false")]
    pub image_input: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub bind: SocketAddr,
    pub provider: ProviderConfig,
    pub projects: Vec<Project>,
    #[serde(default)]
    pub allow_write: bool,
    #[serde(default)]
    pub allow_commands: bool,
    #[serde(default)]
    pub command_isolation: openide_harness::CommandIsolation,
}

impl Config {
    pub fn load(state: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&crate::storage::read_private(
            &state.join("config.json"),
            1024 * 1024,
        )?)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 || self.projects.is_empty() || self.projects.len() > 128 {
            bail!("unsupported config version or project count");
        }
        let mut ids = HashSet::new();
        for project in &self.projects {
            crate::storage::validate_id(&project.id)?;
            if !ids.insert(&project.id) || project.name.is_empty() || project.name.len() > 256 {
                bail!("invalid project name or duplicate id");
            }
            if !project.path.is_absolute()
                || !project.path.is_dir()
                || fs::canonicalize(&project.path)? != project.path
            {
                bail!("project paths must be canonical existing directories");
            }
        }
        crate::storage::validate_id(&self.provider.id)?;
        if self.provider.model.trim().is_empty() || self.provider.model.len() > 256 {
            bail!("an explicit model is required");
        }
        if !matches!(
            self.provider.protocol.as_str(),
            "responses" | "chat_completions" | "anthropic"
        ) {
            bail!("unsupported provider protocol");
        }
        if self.provider.protocol == "chat_completions" && !self.provider.tool_calls {
            bail!("Chat Completions agent routes require explicit tool_calls=true for a compatible model");
        }
        let url = self.provider.endpoint.as_str();
        if self.provider.desktop_data_root.is_some()
            && (self.provider.id != "codex"
                || self.provider.protocol != "responses"
                || url != "https://api.openai.com/v1")
        {
            bail!("saved desktop credentials require the official Codex Responses endpoint");
        }
        // Public-provider transport must be HTTPS. Plain HTTP is only for an
        // explicitly configured local model or local acceptance fixture.
        if !url.starts_with("https://")
            && !url.starts_with("http://127.0.0.1:")
            && !url.starts_with("http://[::1]:")
        {
            bail!("provider endpoint requires HTTPS (loopback HTTP is allowed)");
        }
        if url.contains('@') || url.contains('?') || url.contains('#') {
            bail!("provider endpoint must not contain credentials, query or fragment");
        }
        if let Some(name) = &self.provider.api_key_env {
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                bail!("invalid API key environment variable name");
            }
        }
        Ok(())
    }
}

/// Environment credentials are read at startup; saved Codex credentials use
/// the shared refreshing source on each request. Neither config nor HTTP
/// replies contain their values. A trusted source can also inject its backend
/// through Host::open without exposing credentials to the mobile client.
pub fn build_backend(config: &ProviderConfig) -> Result<Arc<dyn ModelBackend>> {
    build_backend_with_options(config, &config.model, None)
}

/// The host validates these options against its exact catalog before building.
pub(crate) fn build_backend_with_options(
    config: &ProviderConfig,
    model: &str,
    effort: Option<&str>,
) -> Result<Arc<dyn ModelBackend>> {
    if effort.is_some() && config.protocol != "responses" {
        bail!("reasoning selection is not enabled for this transport");
    }
    let responses = |key| -> Result<ResponsesBackend> {
        let mut backend = ResponsesBackend::new(&config.endpoint, key, model);
        if let Some(effort) = effort {
            backend = backend.with_reasoning_effort(effort)?;
        }
        Ok(backend)
    };
    if let Some(root) = &config.desktop_data_root {
        if config.id != "codex"
            || config.protocol != "responses"
            || config.endpoint != "https://api.openai.com/v1"
        {
            bail!("saved desktop credentials require the official Codex Responses endpoint");
        }
        let source =
            openide_connections::codex_token_source(root.clone()).map_err(anyhow::Error::msg)?;
        return Ok(Arc::new(
            responses(None)?
                .with_chatgpt_plan()
                .with_bearer_token_provider(source),
        ));
    }
    let key = config
        .api_key_env
        .as_ref()
        .map(|name| {
            std::env::var(name)
                .with_context(|| format!("credential environment variable {name} is not set"))
        })
        .transpose()?;
    Ok(match config.protocol.as_str() {
        "responses" => Arc::new(responses(key)?),
        "chat_completions" => Arc::new(
            OpenAiCompatibleBackend::new(&config.endpoint, key, model).with_capabilities(
                openide_harness::ModelRequestCapabilities {
                    tool_calls: config.tool_calls,
                    ..Default::default()
                },
            )?,
        ),
        "anthropic" => Arc::new(AnthropicMessagesBackend::new(&config.endpoint, key, model)),
        _ => bail!("unsupported provider protocol"),
    })
}
