//! Transient Settings catalog presentation, independent from the active route.
//!
//! Connections describe the trusted provider registry; model metadata never
//! supplies endpoints or credential names, and secret values stay out of the view.

use std::{collections::BTreeMap, path::Path};

use crate::{
    composer_selection::{ExecutionSupport, ModelRouteOption},
    model_catalog::ModelInfo,
    native_provider::{self, NativeProviderSettings, NativeWireApi, ProviderRegistry},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CatalogModelsFilter {
    #[default]
    All,
    AgentReady,
}

impl CatalogModelsFilter {
    pub fn from_action(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "agent_ready" => Some(Self::AgentReady),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CatalogSettingsTab {
    #[default]
    Models,
    Connection,
}

impl CatalogSettingsTab {
    pub fn from_action(value: &str) -> Option<Self> {
        match value {
            "models" => Some(Self::Models),
            "connection" => Some(Self::Connection),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderConnectionView {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub key_env: String,
    pub wire_api: NativeWireApi,
    /// A nonempty environment value exists; provider acceptance is unknown.
    pub key_available: bool,
    pub key_optional: bool,
    /// The fixed catalog resource, distinct from the execution endpoint.
    pub listing_url: Option<String>,
    pub catalog_anonymous: bool,
}

impl ProviderConnectionView {
    #[cfg(test)]
    pub fn protocol_label(&self) -> &'static str {
        match self.wire_api {
            NativeWireApi::Responses => "Responses",
            NativeWireApi::ChatCompletions => "Chat Completions",
            NativeWireApi::AnthropicMessages => "Messages",
        }
    }
}

pub(crate) fn connection_views(
    root: &Path,
    settings: &NativeProviderSettings,
) -> Result<BTreeMap<String, ProviderConnectionView>, String> {
    let registry = ProviderRegistry::load(root)?;
    Ok(connection_views_with_registry(
        &registry,
        settings,
        |name| crate::provider_connections::read_key(root, name).is_some(),
    ))
}

pub(crate) fn connection_views_with_registry(
    registry: &ProviderRegistry,
    settings: &NativeProviderSettings,
    has_key: impl Fn(&str) -> bool,
) -> BTreeMap<String, ProviderConnectionView> {
    registry
        .adapters()
        .iter()
        .map(|adapter| {
            let endpoint = if adapter.user_endpoint {
                settings.custom_endpoint.clone()
            } else {
                adapter.endpoint.clone()
            };
            let listing_url = adapter
                .catalog_list_path
                .as_ref()
                .or(adapter.model_list_path.as_ref())
                .map(|path| format!("{}/{path}", endpoint.trim_end_matches('/')));
            (
                adapter.id.clone(),
                ProviderConnectionView {
                    id: adapter.id.clone(),
                    name: adapter.name.clone(),
                    endpoint,
                    key_env: adapter.key_env.clone(),
                    wire_api: adapter.wire_api,
                    key_available: has_key(&adapter.key_env),
                    key_optional: adapter.key_optional,
                    listing_url,
                    catalog_anonymous: adapter.catalog_anonymous,
                },
            )
        })
        .collect()
}

pub(crate) fn catalog_model_usable(_provider_id: &str, model: &ModelInfo) -> bool {
    !model.id.is_empty()
        && model.tool_call_declared != Some(false)
        && model.status.as_deref() != Some("deprecated")
        && model.modalities.as_ref().is_none_or(|modalities| {
            (modalities.input.is_empty() || modalities.input.iter().any(|kind| kind == "text"))
                && (modalities.output.is_empty()
                    || modalities.output.iter().any(|kind| kind == "text"))
        })
}

/// Account discovery may narrow a catalog or reviewed route. A missing route
/// remains an explicit user choice with unknown capabilities, never a denial.
pub(crate) fn catalog_route_blocked(
    routes: &[ModelRouteOption],
    provider_id: &str,
    model_id: &str,
) -> bool {
    routes.iter().any(|route| {
        route.backend == "native"
            && route.provider_id == provider_id
            && route.model_id == model_id
            && (route.execution == ExecutionSupport::Unsupported
                || route.provider_listed == Some(false))
    })
}

/// This filter reports declared/reviewed tools, never proven account generation.
pub(crate) fn catalog_model_agent_ready(provider_id: &str, model: &ModelInfo) -> bool {
    catalog_model_agent_ready_for_wire(
        provider_id,
        native_provider::adapter(provider_id).map(|adapter| adapter.wire_api),
        model,
    )
}

pub(crate) fn catalog_model_agent_ready_for_wire(
    provider_id: &str,
    wire_api: Option<NativeWireApi>,
    model: &ModelInfo,
) -> bool {
    catalog_model_usable(provider_id, model)
        && (model.tool_call_declared == Some(true)
            || native_provider::native_model_capabilities(provider_id, &model.id).is_some_and(
                |capabilities| {
                    capabilities.function_tools && Some(capabilities.wire_api) == wire_api
                },
            ))
}

pub(crate) fn catalog_model_matches(
    provider_id: &str,
    model: &ModelInfo,
    query: &str,
    filter: CatalogModelsFilter,
) -> bool {
    if filter == CatalogModelsFilter::AgentReady && !catalog_model_agent_ready(provider_id, model) {
        return false;
    }
    let query = query.trim().to_lowercase();
    query.is_empty()
        || [&model.id, &model.name]
            .into_iter()
            .chain(model.description.as_ref())
            .chain(model.family.as_ref())
            .any(|value| value.to_lowercase().contains(&query))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> ModelInfo {
        let mut model = crate::model_catalog::reviewed_seed_snapshot()
            .model("openai", "gpt-4.1")
            .unwrap()
            .clone();
        model.id = id.to_owned();
        model
    }

    #[test]
    fn connection_views_keep_registry_credentials_and_custom_endpoint_distinct() {
        let settings = NativeProviderSettings {
            custom_endpoint: "https://local.example/v1".into(),
            ..NativeProviderSettings::default()
        };
        let views =
            connection_views_with_registry(&ProviderRegistry::builtin(), &settings, |name| {
                name == "OPENROUTER_API_KEY"
            });
        assert!(views["openrouter"].key_available);
        assert!(!views["openai"].key_available);
        assert_eq!(views["custom"].endpoint, "https://local.example/v1");
        assert_eq!(views["openai"].protocol_label(), "Responses");
        assert_eq!(views["openrouter"].protocol_label(), "Chat Completions");
    }

    #[test]
    fn tools_filter_preserves_unknown_and_explicit_negative_capabilities() {
        let mut unknown = model("new-model");
        unknown.tool_call = false;
        unknown.tool_call_declared = None;
        assert!(catalog_model_matches(
            "openai",
            &unknown,
            "NEW",
            CatalogModelsFilter::All
        ));
        assert!(!catalog_model_matches(
            "openai",
            &unknown,
            "",
            CatalogModelsFilter::AgentReady
        ));
        unknown.tool_call_declared = Some(true);
        assert!(catalog_model_agent_ready("openai", &unknown));
        unknown.tool_call_declared = Some(false);
        unknown.tool_call = true;
        assert!(!catalog_model_agent_ready("openai", &unknown));
        assert!(!catalog_model_usable("openai", &unknown));
        unknown.tool_call_declared = None;
        assert!(!catalog_model_agent_ready("openai", &unknown));

        let mut reviewed = model("gpt-4.1");
        reviewed.tool_call = false;
        reviewed.tool_call_declared = None;
        assert!(catalog_model_agent_ready("openai", &reviewed));
        assert!(!catalog_model_agent_ready_for_wire(
            "openai",
            Some(NativeWireApi::ChatCompletions),
            &reviewed
        ));
        assert!(!catalog_model_agent_ready_for_wire(
            "openai", None, &reviewed
        ));
        reviewed.tool_call_declared = Some(false);
        assert!(!catalog_model_agent_ready("openai", &reviewed));
    }
}
