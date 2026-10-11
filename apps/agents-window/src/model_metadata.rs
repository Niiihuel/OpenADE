//! Read-only model details for the composer. Metadata is bound to the exact
//! provider/model route and never changes execution or budget policies.

//!
//! Each known field records where it came from. The order is: what the
//! provider's own API or account listing reports, the reviewed adapter table,
//! the bundled models.dev snapshot and, for a ChatGPT plan, the Codex CLI's
//! own table. A later source only fills what the earlier ones left unknown.

use std::{collections::BTreeMap, sync::LazyLock};

use serde::Deserialize;

use crate::{
    composer_selection::ModelRouteOption,
    model_catalog::{CatalogSnapshot, CatalogSource, ModelCost, ModelLimit, ModelModalities},
    native_provider::{self, ProviderModelDiscovery},
};

/// The public models.dev data kept by `scripts/update-model-catalog.py`.
#[derive(Deserialize)]
struct BundledCatalog {
    date: String,
    providers: BTreeMap<String, BTreeMap<String, BundledModel>>,
    codex_cli: CodexCliTable,
}

#[derive(Deserialize)]
struct CodexCliTable {
    models: BTreeMap<String, ModelLimit>,
}

#[derive(Default, Deserialize)]
struct BundledModel {
    description: Option<String>,
    knowledge: Option<String>,
    release_date: Option<String>,
    tool_call: Option<bool>,
    reasoning: Option<bool>,
    structured_output: Option<bool>,
    modalities: ModelModalities,
    limit: ModelLimit,
    cost: Option<ModelCost>,
}

static BUNDLED: LazyLock<BundledCatalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../assets/model-catalog/models-dev.json"))
        .expect("bundled model catalog")
});

/// Day the bundled snapshot was taken, as `YYYY-MM-DD`.
pub(crate) fn catalog_date() -> &'static str {
    &BUNDLED.date
}

/// The exact id, or else the same id in another ASCII case when only one
/// entry spells it so (`MiniMax-M3`). Never a prefix, a family or a dated
/// snapshot: those are other models with their own numbers.
fn bundled_model(provider_id: &str, model_id: &str) -> Option<&'static BundledModel> {
    let models = BUNDLED.providers.get(provider_id)?;
    models.get(model_id).or_else(|| {
        let mut same = models
            .iter()
            .filter(|(id, _)| id.eq_ignore_ascii_case(model_id));
        let first = same.next()?;
        same.next().is_none().then_some(first.1)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataSource {
    ProviderApi,
    ProviderAccount,
    Reviewed,
    Catalog,
    CodexCli,
}

impl MetadataSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::ProviderApi => "API del proveedor",
            Self::ProviderAccount => "listado de tu cuenta",
            Self::Reviewed => "referencia revisada",
            Self::Catalog => "catálogo models.dev",
            Self::CodexCli => "Codex CLI",
        }
    }
}

/// A fact the card can show, for its provenance.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum MetadataField {
    Description,
    Tools,
    Vision,
    Reasoning,
    Structured,
    Modalities,
    Context,
    Input,
    Output,
    Prices,
    Knowledge,
    Release,
}

impl MetadataField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Description => "descripción",
            Self::Tools => "herramientas",
            Self::Vision => "visión",
            Self::Reasoning => "razonamiento",
            Self::Structured => "salida estructurada",
            Self::Modalities => "modalidades",
            Self::Context => "contexto",
            Self::Input => "entrada máx.",
            Self::Output => "salida máx.",
            Self::Prices => "precios",
            Self::Knowledge => "conocimiento",
            Self::Release => "lanzamiento",
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
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    /// A ChatGPT plan bills the route; a per-token price would mislead.
    pub included_in_plan: bool,
    /// Where each known field came from. An unknown field has no entry.
    pub field_sources: BTreeMap<MetadataField, MetadataSource>,
}

impl ModelMetadataView {
    fn facts(&self) -> BTreeMap<MetadataField, String> {
        use MetadataField::*;
        let flag = |value: Option<bool>| value.map(|value| value.to_string());
        let count = |value: Option<u64>| value.map(|value| value.to_string());
        [
            (Description, self.description.clone()),
            (Tools, flag(self.tool_call)),
            (Vision, flag(self.image_input)),
            (Reasoning, flag(self.reasoning)),
            (Structured, flag(self.structured_output)),
            (
                Modalities,
                (!self.input_modalities.is_empty() || !self.output_modalities.is_empty())
                    .then(|| format!("{:?}{:?}", self.input_modalities, self.output_modalities)),
            ),
            (Context, count(self.context_tokens)),
            (Input, count(self.max_input_tokens)),
            (Output, count(self.max_output_tokens)),
            (Prices, self.cost.as_ref().map(|cost| format!("{cost:?}"))),
            (Knowledge, self.knowledge.clone()),
            (Release, self.release_date.clone()),
        ]
        .into_iter()
        .filter_map(|(field, value)| Some((field, value?)))
        .collect()
    }

    /// Attribute to `source` every field this stage set or changed, and
    /// forget the source of a field it cleared.
    fn stamp(&mut self, before: &BTreeMap<MetadataField, String>, source: MetadataSource) {
        let now = self.facts();
        self.field_sources.retain(|field, _| now.contains_key(field));
        for (field, value) in now {
            if before.get(&field) != Some(&value) {
                self.field_sources.insert(field, source);
            }
        }
    }

    /// Fill what is still unknown from the bundled catalog. `traits_only`
    /// leaves limits and prices alone: they belong to a route, and a plan's
    /// route is not the API route the catalog describes.
    fn fill_from_catalog(&mut self, entry: &BundledModel, traits_only: bool) {
        let before = self.facts();
        if self.description.is_none() {
            self.description = entry.description.clone();
        }
        if self.tool_call.is_none() {
            self.tool_call = entry.tool_call;
            self.tools_source = entry.tool_call.map(|_| MetadataSource::Catalog);
        }
        self.reasoning = self.reasoning.or(entry.reasoning);
        self.structured_output = self.structured_output.or(entry.structured_output);
        if self.image_input.is_none() && !entry.modalities.input.is_empty() {
            self.image_input = Some(entry.modalities.input.iter().any(|kind| kind == "image"));
        }
        if self.input_modalities.is_empty() && self.output_modalities.is_empty() {
            self.input_modalities = entry.modalities.input.clone();
            self.output_modalities = entry.modalities.output.clone();
        }
        self.knowledge = entry.knowledge.clone();
        self.release_date = entry.release_date.clone();
        if !traits_only {
            self.context_tokens = self.context_tokens.or(entry.limit.context);
            self.max_input_tokens = self.max_input_tokens.or(entry.limit.input);
            self.max_output_tokens = self.max_output_tokens.or(entry.limit.output);
            if self.limits_source.is_none() {
                self.limits_source = Some(MetadataSource::Catalog);
            }
            if self.cost.is_none() {
                self.cost = entry.cost.clone();
                self.prices_source = entry.cost.as_ref().map(|_| MetadataSource::Catalog);
            }
        }
        self.stamp(&before, MetadataSource::Catalog);
    }

    /// A model of the ChatGPT plan: the model's own traits from the catalog's
    /// OpenAI entry of the same id, and the window the Codex CLI ships for it.
    fn fill_for_chatgpt_plan(&mut self, model_id: &str) {
        if let Some(entry) = bundled_model("openai", model_id) {
            self.fill_from_catalog(entry, true);
        }
        let before = self.facts();
        if self.context_tokens.is_none() {
            self.context_tokens = BUNDLED
                .codex_cli
                .models
                .get(model_id)
                .and_then(|limit| limit.context);
            if self.context_tokens.is_some() && self.limits_source.is_none() {
                self.limits_source = Some(MetadataSource::CodexCli);
            }
        }
        self.stamp(&before, MetadataSource::CodexCli);
    }
}

/// Details of one native route, for the card and for the context window the
/// composer shows. Display only: the engine's budget has its own resolver.
pub(crate) fn native_route(
    provider_id: &str,
    model_id: &str,
    catalog: Option<&CatalogSnapshot>,
    discoveries: &[ProviderModelDiscovery],
) -> ModelMetadataView {
    let mut details = native_details(provider_id, model_id, catalog, discoveries);
    if provider_id == "codex" {
        details.fill_for_chatgpt_plan(model_id);
        details.included_in_plan = true;
    } else if let Some(entry) = bundled_model(provider_id, model_id) {
        details.fill_from_catalog(entry, false);
    }
    details
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
                let mut details = ModelMetadataView {
                    resolved_model_id: model.map(|model| model.id.clone()),
                    description: model.and_then(|model| model.description.clone()),
                    input_modalities: model
                        .map_or_else(Vec::new, |model| model.input_modalities.clone()),
                    reasoning: model
                        .and_then(|model| (!model.supported_efforts.is_empty()).then_some(true)),
                    source: model.map(|_| MetadataSource::CodexCli),
                    ..Default::default()
                };
                details.stamp(&BTreeMap::new(), MetadataSource::CodexCli);
                if let Some(model) = model {
                    details.fill_for_chatgpt_plan(&model.id);
                }
                details
            } else if route.backend == "native" {
                native_route(&route.provider_id, &route.model_id, catalog, discoveries)
            } else {
                ModelMetadataView::default()
            };
            (route.route_id.clone(), details)
        })
        .collect()
}

fn native_details(
    provider_id: &str,
    model_id: &str,
    catalog: Option<&CatalogSnapshot>,
    discoveries: &[ProviderModelDiscovery],
) -> ModelMetadataView {
    let status = catalog.and_then(|catalog| catalog.provider_status.get(provider_id));
    let source = status.map(|status| match status.source {
        CatalogSource::OfficialApi => MetadataSource::ProviderApi,
        CatalogSource::ReviewedSeed => MetadataSource::Reviewed,
    });
    let model = catalog.and_then(|catalog| catalog.model(provider_id, model_id));
    let discovered = discoveries
        .iter()
        .find(|d| d.provider_id == provider_id)
        .and_then(|d| d.models.get(model_id));
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
    // A listed image input is a statement; a list without one is not a denial.
    if details.image_input.is_none() && details.input_modalities.iter().any(|kind| kind == "image") {
        details.image_input = Some(true);
    }
    details.stamp(&BTreeMap::new(), source.unwrap_or(MetadataSource::Reviewed));
    let cached = details.facts();
    if let Some(listed) = discovered {
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
    details.stamp(&cached, MetadataSource::ProviderAccount);
    let listed = details.facts();
    if let Some(reviewed) = native_provider::native_model_capabilities(provider_id, model_id) {
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
    details.stamp(&listed, MetadataSource::Reviewed);
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
        let current = native_details("openai", "gpt-4o", Some(&reopened), &[]);
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
        let prior = native_details("openai", "gpt-4o", Some(&legacy), &[]);
        assert!(prior.context_tokens.is_some());
        assert!(prior.max_output_tokens.is_some());
        assert_eq!(prior.limits_source, Some(MetadataSource::Reviewed));
    }

    #[test]
    fn catalog_fills_only_what_the_provider_left_unknown_and_each_field_keeps_its_source() {
        let bundled = bundled_model("openai", "gpt-6-luna").unwrap();
        assert_ne!(bundled.limit.context, Some(500_000));
        let declared = cache(
            "openai",
            json!({"gpt-6-luna":{
                "id":"gpt-6-luna","name":"GPT-6 Luna","provider_limits_declared":true,
                "limit":{"context":500000},"tool_call_declared":false
            }}),
        );
        let details = native_route("openai", "gpt-6-luna", Some(&declared), &[]);
        // The provider's own figures win and are never credited to the catalog.
        assert_eq!(details.context_tokens, Some(500_000));
        assert_eq!(details.tool_call, Some(false));
        for field in [MetadataField::Context, MetadataField::Tools] {
            assert_eq!(details.field_sources[&field], MetadataSource::ProviderApi);
        }
        // What it left unknown comes from the catalog, and says so.
        assert_eq!(details.max_output_tokens, bundled.limit.output);
        assert_eq!(details.cost, bundled.cost);
        assert_eq!(details.knowledge, bundled.knowledge);
        for field in [
            MetadataField::Output,
            MetadataField::Prices,
            MetadataField::Knowledge,
            MetadataField::Reasoning,
        ] {
            assert_eq!(details.field_sources[&field], MetadataSource::Catalog, "{field:?}");
        }
        assert!(!details.included_in_plan);
    }

    #[test]
    fn chatgpt_plan_models_never_borrow_api_limits_or_prices_and_unknown_ids_stay_unknown() {
        let api = bundled_model("openai", "gpt-6-luna").unwrap();
        let plan = native_route("codex", "gpt-6-luna", None, &[]);
        assert_eq!(plan.context_tokens, Some(272_000));
        assert_ne!(plan.context_tokens, api.limit.context);
        assert_eq!(plan.field_sources[&MetadataField::Context], MetadataSource::CodexCli);
        assert_eq!((plan.max_input_tokens, plan.max_output_tokens), (None, None));
        assert!(plan.cost.is_none() && plan.included_in_plan);
        assert_eq!(plan.tool_call, Some(true));
        assert_eq!(plan.field_sources[&MetadataField::Tools], MetadataSource::Catalog);

        // The account listing outranks the CLI table for the same field.
        let listed = cache(
            "codex",
            json!({"gpt-6-luna":{
                "id":"gpt-6-luna","name":"GPT-6-Luna","provider_limits_declared":true,
                "limit":{"context":400000}
            }}),
        );
        let account = native_route("codex", "gpt-6-luna", Some(&listed), &[]);
        assert_eq!(account.context_tokens, Some(400_000));
        assert_eq!(account.field_sources[&MetadataField::Context], MetadataSource::ProviderApi);

        let unknown = native_route("codex", "gpt-99-nova", None, &[]);
        assert!(unknown.field_sources.is_empty(), "{:?}", unknown.field_sources);
        assert!(unknown.context_tokens.is_none() && unknown.cost.is_none());
    }

    #[test]
    fn catalog_ids_match_exactly_or_by_case_and_never_by_prefix() {
        assert!(bundled_model("minimax", "MiniMax-M3").is_some());
        assert!(bundled_model("minimax", "minimax-m3").is_some());
        for near in ["gpt-6", "gpt-6-luna-2026-09-22", "gpt-6-luna-mini", "openai/gpt-6-luna"] {
            assert!(bundled_model("openai", near).is_none(), "{near}");
        }
        assert!(bundled_model("custom", "gpt-6-luna").is_none());
    }

    #[test]
    fn bundled_snapshot_is_dated_and_only_covers_registered_providers() {
        let date = catalog_date();
        assert!(date.len() == 10 && date.split('-').all(|part| part.parse::<u16>().is_ok()));
        let registry = native_provider::ProviderRegistry::builtin();
        for provider in BUNDLED.providers.keys() {
            assert!(
                registry.adapters().iter().any(|adapter| &adapter.id == provider),
                "{provider}"
            );
        }
        assert!(BUNDLED.providers.values().all(|models| !models.is_empty()));
    }
}
