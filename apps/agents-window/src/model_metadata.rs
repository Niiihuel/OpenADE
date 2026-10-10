//! Read-only model details for the composer. Metadata is bound to the exact
//! provider/model route and never changes execution or budget policies.

use std::collections::BTreeMap;

use crate::{
    composer_selection::ModelRouteOption,
    model_catalog::{CatalogSnapshot, CatalogSource, ModelCost},
    native_provider::{self, ProviderModelDiscovery},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataSource {
    ProviderApi,
    ProviderAccount,
    Reviewed,
    CodexCli,
}

impl MetadataSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::ProviderApi => "API del proveedor",
            Self::ProviderAccount => "listado de tu cuenta",
            Self::Reviewed => "referencia revisada",
            Self::CodexCli => "Codex CLI",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ModelMetadataView {
    pub resolved_model_id: Option<String>,
    pub description: Option<String>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub tool_call: Option<bool>,
    pub reasoning: Option<bool>,
    pub structured_output: Option<bool>,
    pub image_input: Option<bool>,
    pub context_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub cost: Option<ModelCost>,
    pub source: Option<MetadataSource>,
    pub tools_source: Option<MetadataSource>,
    pub limits_source: Option<MetadataSource>,
    pub prices_source: Option<MetadataSource>,
    pub refresh_failed: bool,
}

pub(crate) fn for_routes(
    routes: &[ModelRouteOption],
    catalog: Option<&CatalogSnapshot>,
    discoveries: &[ProviderModelDiscovery],
    codex_models: &[openide_cli::DiscoveredCodexModel],
) -> BTreeMap<String, ModelMetadataView> {
    routes
        .iter()
        .map(|route| {
            let details = if route.backend == "codex" {
                // A ChatGPT-authenticated CLI model is a different route from an
                // OpenAI API model. Neither API prices nor limits can be borrowed.
                let model = codex_models.iter().find(|model| {
                    if route.model_id == "default" {
                        model.is_default
                    } else {
                        model.id == route.model_id
                    }
                });
                ModelMetadataView {
                    resolved_model_id: model.map(|model| model.id.clone()),
                    description: model.and_then(|model| model.description.clone()),
                    input_modalities: model
                        .map_or_else(Vec::new, |model| model.input_modalities.clone()),
                    reasoning: model
                        .and_then(|model| (!model.supported_efforts.is_empty()).then_some(true)),
                    source: model.map(|_| MetadataSource::CodexCli),
                    ..Default::default()
                }
            } else if route.backend == "native" {
                native_details(route, catalog, discoveries)
            } else {
                ModelMetadataView::default()
            };
            (route.route_id.clone(), details)
        })
        .collect()
}

fn native_details(
    route: &ModelRouteOption,
    catalog: Option<&CatalogSnapshot>,
    discoveries: &[ProviderModelDiscovery],
) -> ModelMetadataView {
    let status = catalog.and_then(|catalog| catalog.provider_status.get(&route.provider_id));
    let source = status.map(|status| match status.source {
        CatalogSource::OfficialApi => MetadataSource::ProviderApi,
        CatalogSource::ReviewedSeed => MetadataSource::Reviewed,
    });
    let model = catalog.and_then(|catalog| catalog.model(&route.provider_id, &route.model_id));
    let discovered = discoveries
        .iter()
        .find(|d| d.provider_id == route.provider_id)
        .and_then(|d| d.models.get(&route.model_id));
    let authoritative_capabilities = discovered
        .is_some_and(|m| m.capabilities_declared || !m.declared_capabilities.is_empty())
        || model.is_some_and(|m| m.capabilities_declared);
    let authoritative_limits = discovered.is_some_and(|m| m.limits_declared)
        || model.is_some_and(|m| m.provider_limits_declared);
    let mut details = ModelMetadataView::default();
    if let Some(model) = model {
        details.description = model.description.clone();
        if let Some(modalities) = &model.modalities {
            details.input_modalities = modalities.input.clone();
            details.output_modalities = modalities.output.clone();
        }
        details.tool_call = model.tool_call_declared;
        details.tools_source = model.tool_call_declared.and(source);
        // Most legacy booleans use false for a missing field. Preserve that
        // distinction instead of calling an unpublished capability unsupported.
        details.reasoning = model.reasoning.then_some(true);
        details.structured_output = model.structured_output.then_some(true);
        if model.capabilities_declared {
            details.reasoning = model.declared_capabilities.get("thinking").copied();
            details.structured_output = model
                .declared_capabilities
                .get("structured_outputs")
                .copied();
        }
        details.image_input = model.declared_capabilities.get("image_input").copied();
        details.reasoning = model
            .declared_capabilities
            .get("thinking")
            .copied()
            .or(details.reasoning);
        details.structured_output = model
            .declared_capabilities
            .get("structured_outputs")
            .copied()
            .or(details.structured_output);
        if model.provider_limits_declared || model.limit.is_some() {
            details.context_tokens = model.limit.as_ref().and_then(|limit| limit.context);
            details.max_input_tokens = model.limit.as_ref().and_then(|limit| limit.input);
            details.max_output_tokens = model.limit.as_ref().and_then(|limit| limit.output);
            details.limits_source = source;
        }
        details.cost = model.cost.clone();
        details.prices_source = model.cost.as_ref().and(source);
        details.source = source;
        details.refresh_failed = status.is_some_and(|status| status.last_error.is_some());
    }
    if let Some(listed) = discoveries
        .iter()
        .find(|discovery| discovery.provider_id == route.provider_id)
        .and_then(|discovery| discovery.models.get(&route.model_id))
    {
        if listed.capabilities_declared || !listed.declared_capabilities.is_empty() {
            details.image_input = listed.declared_capabilities.get("image_input").copied();
            details.reasoning = listed.declared_capabilities.get("thinking").copied();
            details.structured_output = listed
                .declared_capabilities
                .get("structured_outputs")
                .copied();
            // Boolean capability reports do not certify a stale modality list.
            details.input_modalities.clear();
            details.output_modalities.clear();
        }
        details.image_input = listed
            .declared_capabilities
            .get("image_input")
            .copied()
            .or(details.image_input);
        // This exact account row supersedes its cached tools observation even
        // when the new API makes no claim. Only an explicit reviewed fallback
        // below may then supply a separately attributed tools guarantee.
        details.tool_call = listed.tool_call_declared;
        details.tools_source = listed
            .tool_call_declared
            .map(|_| MetadataSource::ProviderAccount);
        details.reasoning = listed
            .declared_capabilities
            .get("thinking")
            .copied()
            .or(details.reasoning);
        details.structured_output = listed
            .declared_capabilities
            .get("structured_outputs")
            .copied()
            .or(details.structured_output);
        if listed.limits_declared
            || listed.context_window_tokens.is_some()
            || listed.max_input_tokens.is_some()
            || listed.max_output_tokens.is_some()
        {
            // Keep one source's complete observation, including an all-unknown
            // mapped response. Unknown current limits cannot borrow an older
            // value from a different catalog or reviewed exact-model row.
            details.context_tokens = listed.context_window_tokens;
            details.max_input_tokens = listed.max_input_tokens;
            details.max_output_tokens = listed.max_output_tokens;
            details.limits_source = Some(MetadataSource::ProviderAccount);
        }
    }
    if let Some(reviewed) =
        native_provider::native_model_capabilities(&route.provider_id, &route.model_id)
    {
        if !authoritative_capabilities {
            details.image_input = details.image_input.or(Some(reviewed.image_input));
        }
        if !authoritative_limits
            && details.context_tokens.is_none()
            && details.max_input_tokens.is_none()
            && details.max_output_tokens.is_none()
        {
            details.context_tokens = reviewed.context_window_tokens;
            details.max_output_tokens = reviewed.max_output_tokens;
            details.limits_source = Some(MetadataSource::Reviewed);
        }
        if details.tool_call.is_none() {
            details.tool_call = Some(reviewed.function_tools);
            details.tools_source = Some(MetadataSource::Reviewed);
        }
    }
    details
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer_selection::{CredentialAvailability, ExecutionSupport, RouteSource};
    use serde_json::json;

    fn route(provider: &str, model: &str) -> ModelRouteOption {
        ModelRouteOption {
            route_id: format!("native:{provider}:{model}"),
            backend: "native".into(),
            provider_id: provider.into(),
            provider_name: provider.into(),
            model_id: model.into(),
            model_name: model.into(),
            credential: CredentialAvailability::EnvPresentUnverified,
            execution: ExecutionSupport::ProviderListedUnverified,
            source: RouteSource::ProviderDiscovery,
            catalog_present: true,
            provider_listed: Some(true),
            effort_options: Vec::new(),
        }
    }

    fn cache(provider: &str, models: serde_json::Value) -> CatalogSnapshot {
        serde_json::from_value(json!({
            "version":3,
            "fetched_at_ms":1,
            "provider_status":{provider:{"source":"official_api","attempted_at_ms":1,"verified_at_ms":1}},
            "providers":{provider:{"id":provider,"name":provider,"models":models}}
        }))
        .unwrap()
    }

    #[test]
    fn fresh_mapped_unknown_limits_replace_stale_cache_for_the_exact_route() {
        let (endpoint, requests, server) = native_provider::tests::listing_server(vec![(
            200,
            json!({"data":[
                {"id":"opaque-null","max_input_tokens":null,"max_tokens":0},
                {"id":"opaque-absent"},
                {"id":"opaque-partial","max_input_tokens":65536,"max_tokens":null}
            ],"has_more":false})
            .to_string(),
        )]);
        let adapter = native_provider::tests::anthropic_adapter(endpoint);
        let discovery =
            native_provider::discover_connected_models_with_key(&adapter, Some("fixture-key"))
                .unwrap();
        for model in discovery.models.values() {
            assert!(model.limits_declared);
        }
        let mut old_models = serde_json::Map::new();
        for id in [
            "opaque-null",
            "opaque-absent",
            "opaque-partial",
            "opaque-untouched",
        ] {
            old_models.insert(
                id.into(),
                json!({
                "id":id,"name":id,"provider_limits_declared":true,"tool_call_declared":false,
                    "limit":{"context":101000,"input":100000,"output":5000}
                }),
            );
        }
        let saved = cache(&adapter.id, serde_json::Value::Object(old_models));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.json");
        std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
        let reopened: CatalogSnapshot =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let routes: Vec<_> = [
            "opaque-null",
            "opaque-absent",
            "opaque-partial",
            "opaque-untouched",
        ]
        .into_iter()
        .map(|id| route(&adapter.id, id))
        .collect();
        let projected = for_routes(&routes, Some(&reopened), &[discovery], &[]);
        for route in &routes[..2] {
            let details = &projected[&route.route_id];
            assert_eq!(details.context_tokens, None);
            assert_eq!(details.max_input_tokens, None);
            assert_eq!(details.max_output_tokens, None);
            assert_eq!(details.limits_source, Some(MetadataSource::ProviderAccount));
            assert_eq!(details.tool_call, None);
            assert_eq!(details.tools_source, None);
        }
        let partial = &projected[&routes[2].route_id];
        assert_eq!(partial.context_tokens, None);
        assert_eq!(partial.max_input_tokens, Some(65536));
        assert_eq!(partial.max_output_tokens, None);
        assert_eq!(partial.limits_source, Some(MetadataSource::ProviderAccount));
        assert_eq!(partial.tool_call, None);
        assert_eq!(partial.tools_source, None);
        let untouched = &projected[&routes[3].route_id];
        assert_eq!(untouched.context_tokens, Some(101000));
        assert_eq!(untouched.max_input_tokens, Some(100000));
        assert_eq!(untouched.max_output_tokens, Some(5000));
        assert_eq!(untouched.limits_source, Some(MetadataSource::ProviderApi));
        assert_eq!(untouched.tool_call, Some(false));
        assert_eq!(untouched.tools_source, Some(MetadataSource::ProviderApi));
        assert!(requests
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .starts_with("GET /v1/models?limit=1000 "));
        server.join().unwrap();
    }

    #[test]
    fn reopened_catalog_unknown_limits_do_not_borrow_reviewed_limits() {
        let exact = route("openai", "gpt-4o");
        assert!(
            native_provider::native_model_capabilities("openai", "gpt-4o")
                .unwrap()
                .context_window_tokens
                .is_some()
        );
        let declared = cache(
            "openai",
            json!({"gpt-4o":{
                "id":"gpt-4o","name":"current exact row","provider_limits_declared":true
            }}),
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.json");
        std::fs::write(&path, serde_json::to_vec(&declared).unwrap()).unwrap();
        let reopened: CatalogSnapshot =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            reopened
                .model("openai", "gpt-4o")
                .unwrap()
                .provider_limits_declared
        );
        let current = native_details(&exact, Some(&reopened), &[]);
        assert_eq!(current.context_tokens, None);
        assert_eq!(current.max_input_tokens, None);
        assert_eq!(current.max_output_tokens, None);
        assert_eq!(current.limits_source, Some(MetadataSource::ProviderApi));

        // Old caches lacking the new observation marker keep the existing
        // reviewed fallback instead of gaining authority retroactively.
        let legacy = cache(
            "openai",
            json!({"gpt-4o":{"id":"gpt-4o","name":"legacy row"}}),
        );
        let prior = native_details(&exact, Some(&legacy), &[]);
        assert!(prior.context_tokens.is_some());
        assert!(prior.max_output_tokens.is_some());
        assert_eq!(prior.limits_source, Some(MetadataSource::Reviewed));
    }
}
