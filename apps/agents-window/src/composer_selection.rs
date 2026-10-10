//! Model and reasoning controls shared by the home and session composers.
//! Choices are stored separately from the visible tree so changing a dock
//! does not silently reset the configuration of the selected runner.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::model_catalog::CatalogSnapshot;
use crate::native_provider::{
    self, NativeProviderSettings, NativeRouteExecution, ProviderModelDiscovery,
};

const DEFAULT_SELECTION: &str = "default";
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ComposerChoice {
    pub model: String,
    pub effort: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ComposerPreferences {
    #[serde(default)]
    choices: BTreeMap<String, ComposerChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_backend: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    collapsed_model_providers: BTreeSet<String>,
}

impl ComposerPreferences {
    pub fn load(root: &Path) -> Self {
        let path = root.join("composer-preferences.json");
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 64 * 1024) {
            return Self::default();
        }
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn choice(&self, backend: &str, native_model: &str) -> ComposerChoice {
        self.choices.get(backend).cloned().unwrap_or_else(|| {
            if backend == "codex" {
                codex_local_defaults()
            } else if backend == "native" {
                ComposerChoice {
                    model: native_model.to_owned(),
                    effort: DEFAULT_SELECTION.to_owned(),
                }
            } else {
                ComposerChoice::default()
            }
        })
    }

    pub fn set(&mut self, backend: &str, choice: ComposerChoice) {
        self.choices.insert(backend.to_owned(), choice);
    }

    #[cfg(test)]
    pub fn selected_backend(&self) -> Option<&str> {
        self.selected_backend.as_deref()
    }

    pub fn set_selected_backend(&mut self, backend: &str) {
        self.selected_backend = Some(backend.to_owned());
    }

    pub fn collapsed_model_providers(&self) -> &BTreeSet<String> {
        &self.collapsed_model_providers
    }

    pub fn toggle_model_provider(&mut self, provider: &str) {
        if !self.collapsed_model_providers.remove(provider) {
            self.collapsed_model_providers.insert(provider.to_owned());
        }
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        let payload = serde_json::to_vec_pretty(self)?;
        let temp = root.join(format!(
            ".composer-preferences-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            use std::io::Write;
            file.write_all(&payload)?;
            file.sync_all()?;
            std::fs::rename(&temp, root.join("composer-preferences.json"))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".codex")))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Reads only the two top-level scalar defaults. The Codex CLI remains the
/// authority for model availability and can reject a stale local preference.
fn codex_local_defaults() -> ComposerChoice {
    let Some(path) = codex_home().map(|home| home.join("config.toml")) else {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    };
    if metadata.len() > 256 * 1024 {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    }
    let Ok(source) = std::fs::read_to_string(path) else {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    };
    let mut choice = ComposerChoice {
        model: DEFAULT_SELECTION.to_owned(),
        effort: DEFAULT_SELECTION.to_owned(),
    };
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.split_once('"'))
            .map(|(value, _)| value);
        let Some(value) = value.filter(|value| {
            !value.is_empty()
                && value.len() <= 100
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
        }) else {
            continue;
        };
        match key.trim() {
            "model" => choice.model = value.to_owned(),
            "model_reasoning_effort" => choice.effort = value.to_owned(),
            _ => {}
        }
    }
    choice
}

pub(crate) fn model_options(backend: &str, selected: &str) -> Vec<(String, String)> {
    let mut options = if backend == "codex" {
        let mut options = vec![(DEFAULT_SELECTION.to_owned(), "Predeterminado".to_owned())];
        options.extend(
            openide_cli::codex_known_models()
                .iter()
                .map(|(id, label)| ((*id).to_owned(), (*label).to_owned())),
        );
        options
    } else if backend == "native" {
        vec![
            (selected.to_owned(), display_model(selected)),
            (
                "openide:configure-model".to_owned(),
                "Configurar modelo…".to_owned(),
            ),
        ]
    } else {
        Vec::new()
    };
    if !selected.is_empty() && !options.iter().any(|(id, _)| id == selected) {
        options.push((selected.to_owned(), display_model(selected)));
    }
    options
}

/// An available route is different from a catalog entry: it has an executable
/// adapter or an existing external runner. No secret value is retained here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CredentialAvailability {
    /// The environment variable exists; acceptance by the provider is unknown.
    EnvPresentUnverified,
    MissingCredential,
    /// A custom endpoint may accept requests without a bearer key; the
    /// connection remains unverified until a request succeeds.
    NoKeyRequired,
    /// Codex owns login and subscription validation outside OpenIDE.
    ExternalCliManaged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionSupport {
    /// The native adapter's exact provider/model/tool contract was reviewed.
    Verified,
    /// User explicitly configured this provider/model route.
    UnverifiedUserChoice,
    /// Codex CLI validates the selected model and account when it runs.
    DelegatedCli,
    /// The authenticated provider listing includes this model, but tool and
    /// streaming support may still require a live request to verify.
    ProviderListedUnverified,
    /// The provider listing explicitly reports tools, but execution has not
    /// been verified with this account and transport.
    ProviderAdvertisedUnverified,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteSource {
    AdapterReview,
    UserConfigured,
    CliCatalog,
    CliLiveCatalog,
    ProviderDiscovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelRouteOption {
    /// Opaque action ID; compare against this field instead of parsing it.
    pub route_id: String,
    pub backend: String,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub credential: CredentialAvailability,
    pub execution: ExecutionSupport,
    pub source: RouteSource,
    /// Whether the local provider catalog has display metadata for this exact pair. This
    /// never changes `execution` or adds effort levels.
    pub catalog_present: bool,
    /// `Some(false)` means an authenticated provider listing did not include
    /// this model. `None` means discovery has not run or does not apply.
    pub provider_listed: Option<bool>,
    pub effort_options: Vec<(String, String)>,
}

impl ModelRouteOption {
    pub fn selectable(&self) -> bool {
        if self.provider_listed == Some(false) {
            return false;
        }
        match self.backend.as_str() {
            "codex" => {
                self.credential == CredentialAvailability::ExternalCliManaged
                    && self.execution == ExecutionSupport::DelegatedCli
            }
            "native" => {
                matches!(
                    self.credential,
                    CredentialAvailability::EnvPresentUnverified
                        | CredentialAvailability::NoKeyRequired
                ) && matches!(
                    self.execution,
                    ExecutionSupport::Verified
                        | ExecutionSupport::UnverifiedUserChoice
                        | ExecutionSupport::ProviderListedUnverified
                        | ExecutionSupport::ProviderAdvertisedUnverified
                )
            }
            _ => false,
        }
    }
}

pub(crate) fn model_route_option_by_id<'a>(
    options: &'a [ModelRouteOption],
    route_id: &str,
) -> Option<&'a ModelRouteOption> {
    options.iter().find(|option| option.route_id == route_id)
}

/// Build UI choices from actual adapters and the Codex CLI runner. Catalog metadata
/// can enrich names, but cannot authorize a provider, model, credential, or
/// reasoning level. Unknown native routes require an explicit user choice.
pub(crate) fn model_route_options(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
) -> Vec<ModelRouteOption> {
    model_route_options_with_discovery(catalog, settings, &[])
}

/// Use a worker-produced, authenticated model listing to narrow reviewed
/// routes and add advertised routes for this account. Never pass a
/// catalog snapshot without authentication as `discovery`: it cannot establish
/// account access.
pub(crate) fn model_route_options_with_discovery(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    discoveries: &[ProviderModelDiscovery],
) -> Vec<ModelRouteOption> {
    model_route_options_with_keys_and_registry(
        catalog,
        settings,
        discoveries,
        &native_provider::ProviderRegistry::builtin(),
        native_provider::adapter_key_present,
    )
}

pub(crate) fn model_route_options_with_discovery_at(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    discoveries: &[ProviderModelDiscovery],
    root: &std::path::Path,
) -> Result<Vec<ModelRouteOption>, String> {
    let registry = native_provider::ProviderRegistry::load(root)?;
    let mut options = model_route_options_with_keys_and_registry(
        catalog,
        settings,
        discoveries,
        &registry,
        |provider| {
            registry.adapter(provider).is_some_and(|adapter| {
                crate::provider_connections::read_key(root, &adapter.key_env).is_some()
            })
        },
    );
    // The legacy native defaults are execution fallbacks, not a connection
    // the user added to the picker. Explicit custom connections stay visible.
    if !settings.has_explicit_configuration(root) {
        options.retain(|route| {
            route.provider_id != "custom" || route.source != RouteSource::UserConfigured
        });
    }
    Ok(options)
}

/// Replace the bundled Codex fallback with the authenticated app-server
/// catalog while keeping native routes resolved independently. The default
/// route inherits the listed default model's name and effort levels, but still
/// omits `--model` when selected.
pub(crate) fn model_route_options_with_codex_discovery(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    native_discoveries: &[ProviderModelDiscovery],
    codex_models: &[openide_cli::DiscoveredCodexModel],
) -> Vec<ModelRouteOption> {
    let mut options = model_route_options_with_discovery(catalog, settings, native_discoveries);
    if codex_models.is_empty() {
        return options;
    }
    options.retain(|option| option.backend != "codex");
    let default = codex_models.iter().find(|model| model.is_default);
    let mut live = Vec::new();
    live.push(ModelRouteOption {
        route_id: "codex:codex:default".to_owned(),
        backend: "codex".to_owned(),
        provider_id: "codex".to_owned(),
        provider_name: "Codex CLI".to_owned(),
        model_id: DEFAULT_SELECTION.to_owned(),
        model_name: default.map_or_else(
            || "Predeterminado".to_owned(),
            |model| format!("Predeterminado · {}", model.display_name),
        ),
        credential: CredentialAvailability::ExternalCliManaged,
        execution: ExecutionSupport::DelegatedCli,
        source: RouteSource::CliLiveCatalog,
        catalog_present: false,
        provider_listed: Some(true),
        effort_options: default.map_or_else(Vec::new, |model| {
            effort_options_from_levels(model.supported_efforts.iter().map(String::as_str))
        }),
    });
    for model in codex_models {
        if live.iter().any(|option| option.model_id == model.id) {
            continue;
        }
        live.push(ModelRouteOption {
            route_id: format!("codex:codex:{}", model.id),
            backend: "codex".to_owned(),
            provider_id: "codex".to_owned(),
            provider_name: "Codex CLI".to_owned(),
            model_id: model.id.clone(),
            model_name: model.display_name.clone(),
            credential: CredentialAvailability::ExternalCliManaged,
            execution: ExecutionSupport::DelegatedCli,
            source: RouteSource::CliLiveCatalog,
            catalog_present: false,
            provider_listed: Some(true),
            effort_options: effort_options_from_levels(
                model.supported_efforts.iter().map(String::as_str),
            ),
        });
    }
    live.extend(options);
    live
}

pub(crate) fn model_route_options_with_codex_discovery_at(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    native_discoveries: &[ProviderModelDiscovery],
    codex_models: &[openide_cli::DiscoveredCodexModel],
    root: &std::path::Path,
) -> Result<Vec<ModelRouteOption>, String> {
    let mut options =
        model_route_options_with_discovery_at(catalog, settings, native_discoveries, root)?;
    if codex_models.is_empty() {
        return Ok(options);
    }
    // Keep the existing CLI presentation, replacing only its catalog entries.
    let mut live = model_route_options_with_codex_discovery(
        None,
        &NativeProviderSettings::default(),
        &[],
        codex_models,
    );
    live.retain(|route| route.backend == "codex");
    options.retain(|route| route.backend != "codex");
    live.extend(options);
    Ok(live)
}

#[cfg(test)]
fn model_route_options_with_keys(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    discoveries: &[ProviderModelDiscovery],
    has_key: impl Fn(&str) -> bool,
) -> Vec<ModelRouteOption> {
    model_route_options_with_keys_and_registry(
        catalog,
        settings,
        discoveries,
        &native_provider::ProviderRegistry::builtin(),
        has_key,
    )
}

fn model_route_options_with_keys_and_registry(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    discoveries: &[ProviderModelDiscovery],
    registry: &native_provider::ProviderRegistry,
    has_key: impl Fn(&str) -> bool,
) -> Vec<ModelRouteOption> {
    let credential_for = |provider_id: &str, model_id: &str| {
        if has_key(provider_id) {
            return CredentialAvailability::EnvPresentUnverified;
        }
        let mut route_settings = settings.clone();
        route_settings.provider_id = provider_id.to_owned();
        route_settings.model_id = model_id.to_owned();
        if registry
            .adapter(provider_id)
            .is_some_and(|adapter| adapter.key_optional)
            && route_settings
                .resolve_with_registry(registry, |name| std::env::var(name).ok())
                .is_ok()
        {
            CredentialAvailability::NoKeyRequired
        } else {
            CredentialAvailability::MissingCredential
        }
    };
    let mut options = Vec::new();
    options.push(ModelRouteOption {
        route_id: "codex:codex:default".to_owned(),
        backend: "codex".to_owned(),
        provider_id: "codex".to_owned(),
        provider_name: "Codex CLI".to_owned(),
        model_id: DEFAULT_SELECTION.to_owned(),
        model_name: "Predeterminado".to_owned(),
        credential: CredentialAvailability::ExternalCliManaged,
        execution: ExecutionSupport::DelegatedCli,
        source: RouteSource::CliCatalog,
        catalog_present: false,
        provider_listed: None,
        effort_options: Vec::new(),
    });
    for (model_id, model_name) in openide_cli::codex_known_models() {
        options.push(ModelRouteOption {
            route_id: format!("codex:codex:{model_id}"),
            backend: "codex".to_owned(),
            provider_id: "codex".to_owned(),
            provider_name: "Codex CLI".to_owned(),
            model_id: (*model_id).to_owned(),
            model_name: (*model_name).to_owned(),
            credential: CredentialAvailability::ExternalCliManaged,
            execution: ExecutionSupport::DelegatedCli,
            source: RouteSource::CliCatalog,
            catalog_present: false,
            provider_listed: None,
            effort_options: effort_options_for_provider("codex", model_id, ""),
        });
    }

    for capabilities in native_provider::reviewed_native_models() {
        let provider_id = capabilities.provider_id;
        let model_id = capabilities.model_id;
        let adapter = registry
            .adapter(provider_id)
            .expect("reviewed adapter exists");
        let metadata = catalog.and_then(|catalog| catalog.model(provider_id, model_id));
        let discovery = discoveries
            .iter()
            .find(|entry| entry.provider_id == provider_id);
        // The account response is the complete Codex picker, including its
        // order. A reviewed seed must not reintroduce an omitted/hidden ID.
        if provider_id == "codex" && discovery.is_some() {
            continue;
        }
        options.push(ModelRouteOption {
            route_id: format!("native:{provider_id}:{model_id}"),
            backend: "native".to_owned(),
            provider_id: provider_id.to_owned(),
            provider_name: adapter.name.to_owned(),
            model_id: model_id.to_owned(),
            model_name: discovery
                .and_then(|entry| entry.models.get(model_id))
                .map(|model| model.name.clone())
                .or_else(|| metadata.map(|model| model.name.clone()))
                .unwrap_or_else(|| display_model(model_id)),
            credential: credential_for(provider_id, model_id),
            execution: if discovery
                .and_then(|entry| entry.models.get(model_id))
                .is_some_and(|model| {
                    model.tool_call_declared == Some(false) || !model.text_input_output
                }) {
                ExecutionSupport::Unsupported
            } else if native_provider::native_route_execution(provider_id, model_id, false)
                == NativeRouteExecution::Verified
            {
                ExecutionSupport::Verified
            } else {
                ExecutionSupport::Unsupported
            },
            source: RouteSource::AdapterReview,
            catalog_present: metadata.is_some(),
            provider_listed: discovery.map(|entry| entry.models.contains_key(model_id)),
            effort_options: effort_options_for_discovered_provider_with_registry(
                "native",
                model_id,
                provider_id,
                discoveries,
                catalog,
                registry,
            ),
        });
    }

    for discovery in discoveries {
        let Some(adapter) = registry.adapter(&discovery.provider_id) else {
            continue;
        };
        let mut models = discovery.models.values().collect::<Vec<_>>();
        if discovery.provider_id == "codex" {
            models.sort_by_key(|model| model.listing_index.unwrap_or(usize::MAX));
        }
        for model in models {
            if options.iter().any(|route| {
                route.backend == "native"
                    && route.provider_id == discovery.provider_id
                    && route.model_id == model.id
            }) {
                continue;
            }
            let metadata =
                catalog.and_then(|catalog| catalog.model(&discovery.provider_id, &model.id));
            options.push(ModelRouteOption {
                route_id: format!("native:{}:{}", discovery.provider_id, model.id),
                backend: "native".to_owned(),
                provider_id: discovery.provider_id.clone(),
                provider_name: adapter.name.clone(),
                model_id: model.id.clone(),
                model_name: model.name.clone(),
                credential: credential_for(&discovery.provider_id, &model.id),
                execution: if model.tool_call_declared == Some(false)
                    || !model.text_input_output
                    || adapter.for_model(&model.id).is_err()
                {
                    ExecutionSupport::Unsupported
                } else if model.provider_reports_tools {
                    ExecutionSupport::ProviderAdvertisedUnverified
                } else {
                    ExecutionSupport::ProviderListedUnverified
                },
                source: RouteSource::ProviderDiscovery,
                catalog_present: metadata.is_some(),
                provider_listed: Some(true),
                effort_options: effort_options_for_discovered_provider_with_registry(
                    "native",
                    &model.id,
                    &discovery.provider_id,
                    discoveries,
                    catalog,
                    registry,
                ),
            });
        }
    }

    let provider_id = settings.provider_id.trim();
    let model_id = settings.model_id.trim();
    if let Some(adapter) = registry.adapter(provider_id) {
        if !model_id.is_empty()
            && !options.iter().any(|option| {
                option.backend == "native"
                    && option.provider_id == provider_id
                    && option.model_id == model_id
            })
        {
            let metadata = catalog.and_then(|catalog| catalog.model(provider_id, model_id));
            let discovery = discoveries
                .iter()
                .find(|entry| entry.provider_id == provider_id);
            let execution = if discovery
                .and_then(|entry| entry.models.get(model_id))
                .is_some_and(|model| {
                    model.tool_call_declared == Some(false) || !model.text_input_output
                }) {
                ExecutionSupport::Unsupported
            } else {
                match native_provider::native_route_execution_with_registry(
                    registry,
                    provider_id,
                    model_id,
                    true,
                ) {
                    NativeRouteExecution::Verified => ExecutionSupport::Verified,
                    NativeRouteExecution::ExplicitUnverified
                        if credential_for(provider_id, model_id)
                            != CredentialAvailability::MissingCredential
                            && settings.validated_endpoint_in(registry).is_ok() =>
                    {
                        ExecutionSupport::UnverifiedUserChoice
                    }
                    _ => ExecutionSupport::Unsupported,
                }
            };
            options.push(ModelRouteOption {
                route_id: format!("native:{provider_id}:{model_id}"),
                backend: "native".to_owned(),
                provider_id: provider_id.to_owned(),
                provider_name: adapter.name.to_owned(),
                model_id: model_id.to_owned(),
                model_name: metadata
                    .map(|model| model.name.clone())
                    .unwrap_or_else(|| display_model(model_id)),
                credential: credential_for(provider_id, model_id),
                execution,
                source: RouteSource::UserConfigured,
                catalog_present: metadata.is_some(),
                provider_listed: discovery.map(|entry| entry.models.contains_key(model_id)),
                effort_options: effort_options_for_discovered_provider_with_registry(
                    "native",
                    model_id,
                    provider_id,
                    discoveries,
                    catalog,
                    registry,
                ),
            });
        }
    }
    options
}

#[cfg(test)]
pub(crate) fn effort_options(backend: &str, model: &str) -> Vec<(String, String)> {
    effort_options_for_provider(backend, model, "")
}

/// Only advertise effort levels reviewed for this exact adapter/model pair.
/// Catalog metadata and model-name prefixes are insufficient evidence.
pub(crate) fn effort_options_for_provider(
    backend: &str,
    model: &str,
    native_provider_id: &str,
) -> Vec<(String, String)> {
    let levels = if backend == "codex" {
        openide_cli::codex_supported_efforts(model)
    } else if backend == "native" {
        crate::native_provider::native_model_capabilities(native_provider_id, model)
            .map(|capabilities| capabilities.supported_efforts)
            .filter(|levels| !levels.is_empty())
    } else {
        None
    };
    let Some(levels) = levels else {
        return Vec::new();
    };
    effort_options_from_levels(levels.iter().copied())
}

/// A provider may expose exact model levels in its authenticated listing.
/// This augments the reviewed local matrix without deriving levels from IDs.
#[cfg(test)]
pub(crate) fn effort_options_for_discovered_provider(
    backend: &str,
    model: &str,
    native_provider_id: &str,
    discoveries: &[ProviderModelDiscovery],
) -> Vec<(String, String)> {
    effort_options_for_discovered_provider_with_registry(
        backend,
        model,
        native_provider_id,
        discoveries,
        None,
        &native_provider::ProviderRegistry::builtin(),
    )
}

fn effort_options_for_discovered_provider_with_registry(
    backend: &str,
    model: &str,
    native_provider_id: &str,
    discoveries: &[ProviderModelDiscovery],
    catalog: Option<&CatalogSnapshot>,
    registry: &native_provider::ProviderRegistry,
) -> Vec<(String, String)> {
    if backend != "native" {
        return effort_options_for_provider(backend, model, native_provider_id);
    }
    let wire_supports_effort = registry
        .adapter(native_provider_id)
        .and_then(|adapter| adapter.for_model(model).ok())
        .is_some_and(|adapter| {
            matches!(
                adapter.wire_api,
                native_provider::NativeWireApi::Responses
                    | native_provider::NativeWireApi::AnthropicMessages
            ) || adapter.reasoning_effort_field.is_some()
        });
    let discovered_model = discoveries
        .iter()
        .find(|d| d.provider_id == native_provider_id)
        .and_then(|d| d.models.get(model));
    let effort_supported = match discovered_model {
        Some(discovered)
            if discovered.capabilities_declared
                || !discovered.declared_capabilities.is_empty()
                || registry.adapter(native_provider_id).is_some_and(|adapter| {
                    !adapter.catalog.capability_support_paths.is_empty()
                }) =>
        {
            discovered.declared_capabilities.get("effort").copied()
        }
        _ => discovered_model
            .and_then(|m| m.declared_capabilities.get("effort"))
            .copied()
            .or_else(|| {
                catalog
                    .and_then(|s| s.model(native_provider_id, model))
                    .and_then(|m| m.declared_capabilities.get("effort"))
                    .copied()
            }),
    };
    if effort_supported == Some(false) {
        return Vec::new();
    }
    let listed = discoveries
        .iter()
        .find(|discovery| discovery.provider_id == native_provider_id)
        .and_then(|discovery| discovery.models.get(model))
        .filter(|model| {
            wire_supports_effort
                && (model.efforts_declared
                    || model.capabilities_declared
                    || registry.adapter(native_provider_id).is_some_and(|adapter| {
                        !adapter.catalog.capability_support_paths.is_empty()
                    })
                    || model.declared_capabilities.contains_key("effort")
                    || !model.supported_efforts.is_empty())
        });
    if let Some(listed) = listed {
        effort_options_from_levels(listed.supported_efforts.iter().map(String::as_str))
    } else if let Some(model) = catalog
        .and_then(|catalog| catalog.model(native_provider_id, model))
        .filter(|model| {
            wire_supports_effort
                && (model.provider_efforts_declared
                    || model.capabilities_declared
                    || model.declared_capabilities.contains_key("effort")
                    || !model.provider_efforts.is_empty())
        })
    {
        effort_options_from_levels(model.provider_efforts.iter().map(String::as_str))
    } else if wire_supports_effort {
        effort_options_for_provider(backend, model, native_provider_id)
    } else {
        Vec::new()
    }
}

fn effort_options_from_levels<'a>(
    levels: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let levels: Vec<_> = levels.into_iter().collect();
    if levels.is_empty() {
        return Vec::new();
    }
    let mut options = vec![(DEFAULT_SELECTION.to_owned(), "Automático".to_owned())];
    options.extend(levels.into_iter().map(|level| {
        (
            level.to_owned(),
            match level {
                "none" => "Ninguno",
                "minimal" => "Mínimo",
                "low" => "Bajo",
                "medium" => "Medio",
                "high" => "Alto",
                "xhigh" => "Muy alto",
                "max" => "Máximo",
                "ultra" => "Ultra",
                _ => level,
            }
            .to_owned(),
        )
    }));
    options
}

pub(crate) fn display_model(id: &str) -> String {
    if id == DEFAULT_SELECTION || id.is_empty() {
        return "Modelo del agente".to_owned();
    }
    openide_cli::codex_known_models()
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, label)| (*label).to_owned())
        .unwrap_or_else(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_local_endpoint_can_be_selected_without_a_bearer_key() {
        let settings = NativeProviderSettings {
            custom_endpoint: "http://localhost:1234/v1".to_owned(),
            ..NativeProviderSettings::default()
        };
        let routes = model_route_options_with_keys(None, &settings, &[], |_| false);
        let route = routes
            .iter()
            .find(|route| route.provider_id == "custom" && route.model_id == settings.model_id)
            .unwrap();
        assert_eq!(route.credential, CredentialAvailability::NoKeyRequired);
        assert!(route.selectable());
    }

    #[test]
    fn discovered_local_model_keeps_declared_optional_credentials_and_valid_endpoint() {
        let model_id = "local/model:exact";
        let discovery = ProviderModelDiscovery {
            provider_id: "custom".into(),
            models: BTreeMap::from([(
                model_id.into(),
                DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: model_id.into(),
                    name: model_id.into(),
                    provider_reports_tools: true,
                    tool_call_declared: Some(true),
                    text_input_output: true,
                    supported_efforts: Vec::new(),
                    context_window_tokens: None,
                    max_output_tokens: None,
                },
            )]),
        };
        for (endpoint, expected) in [
            (
                "http://127.0.0.1:1234/v1",
                CredentialAvailability::NoKeyRequired,
            ),
            (
                "not a valid endpoint",
                CredentialAvailability::MissingCredential,
            ),
        ] {
            let settings = NativeProviderSettings {
                provider_id: "custom".into(),
                model_id: model_id.into(),
                custom_endpoint: endpoint.into(),
            };
            let routes = model_route_options_with_keys(
                None,
                &settings,
                std::slice::from_ref(&discovery),
                |_| false,
            );
            let matches: Vec<_> = routes
                .iter()
                .filter(|route| {
                    route.backend == "native"
                        && route.provider_id == "custom"
                        && route.model_id == model_id
                })
                .collect();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].credential, expected);
            assert_eq!(matches[0].source, RouteSource::ProviderDiscovery);
            assert_eq!(
                matches[0].selectable(),
                expected == CredentialAvailability::NoKeyRequired
            );
        }
    }
    use crate::native_provider::DiscoveredModel;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn preferences_preserve_model_and_effort_per_backend() {
        let root = tempfile::tempdir().unwrap();
        let mut prefs = ComposerPreferences::default();
        prefs.set(
            "codex",
            ComposerChoice {
                model: "gpt-6-sol".into(),
                effort: "ultra".into(),
            },
        );
        prefs.save(root.path()).unwrap();
        prefs.set(
            "native",
            ComposerChoice {
                model: "gpt-4.1".into(),
                effort: "default".into(),
            },
        );
        prefs.set_selected_backend("native");
        prefs.save(root.path()).unwrap();
        let loaded = ComposerPreferences::load(root.path());
        assert_eq!(loaded.choice("codex", ""), prefs.choice("codex", ""));
        assert_eq!(loaded.choice("native", "gpt-4.1").model, "gpt-4.1");
        assert_eq!(loaded.selected_backend(), Some("native"));
    }

    #[test]
    fn preferences_without_backend_keep_legacy_model_choices() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("composer-preferences.json"),
            r#"{"choices":{"codex":{"model":"gpt-6-sol","effort":"high"}}}"#,
        )
        .unwrap();
        let loaded = ComposerPreferences::load(root.path());
        assert_eq!(loaded.selected_backend(), None);
        assert_eq!(loaded.choice("codex", "").model, "gpt-6-sol");
    }

    #[test]
    fn unknown_selected_model_remains_visible() {
        let options = model_options("codex", "custom-model");
        assert!(options.iter().any(|(id, _)| id == "custom-model"));
    }

    #[test]
    fn responses_effort_choices_include_reasoning_tools_for_gpt6() {
        let astra = effort_options_for_provider("native", "gpt-6-astra", "openai");
        assert!(astra.iter().any(|(id, _)| id == "max"));
        assert!(!astra.iter().any(|(id, _)| id == "none"));
        let sol = effort_options_for_provider("native", "gpt-6-sol", "openai");
        assert!(sol.iter().any(|(id, _)| id == "medium"));
        assert!(sol.iter().any(|(id, _)| id == "none"));
        assert!(effort_options("native", "gpt-6-sol")
            .iter()
            .all(|(id, _)| id != "medium"));
    }

    #[test]
    fn native_efforts_follow_exact_provider_model_capabilities() {
        let gpt54 = effort_options_for_provider("native", "gpt-5.4", "openai");
        assert!(gpt54.iter().any(|(id, _)| id == "xhigh"));
        assert!(!gpt54.iter().any(|(id, _)| id == "max"));
        let gpt56 = effort_options_for_provider("native", "gpt-5.6-terra", "openai");
        assert!(gpt56.iter().any(|(id, _)| id == "max"));
        for (provider, model) in [
            ("deepseek", "deepseek-flash"),
            ("openrouter", "openai/gpt-5.6-sol"),
            ("custom", "gpt-5.6-sol"),
            ("openai", "future-model"),
        ] {
            assert!(
                effort_options_for_provider("native", model, provider).is_empty(),
                "{provider}/{model}"
            );
        }
    }

    #[test]
    fn routes_require_native_credentials_and_keep_codex_login_external() {
        let settings = NativeProviderSettings {
            provider_id: "openrouter".into(),
            model_id: "vendor/selected".into(),
            ..NativeProviderSettings::default()
        };
        let missing = model_route_options_with_keys(None, &settings, &[], |_| false);
        assert!(missing
            .iter()
            .filter(|route| route.backend == "native")
            .all(|route| {
                route.credential == CredentialAvailability::MissingCredential && !route.selectable()
            }));
        let codex = model_route_option_by_id(&missing, "codex:codex:gpt-6-sol").unwrap();
        assert_eq!(codex.credential, CredentialAvailability::ExternalCliManaged);
        assert_eq!(codex.execution, ExecutionSupport::DelegatedCli);
        assert!(codex.selectable());

        let connected = model_route_options_with_keys(None, &settings, &[], |provider| {
            provider == "openrouter"
        });
        let selected =
            model_route_option_by_id(&connected, "native:openrouter:vendor/selected").unwrap();
        assert_eq!(
            selected.credential,
            CredentialAvailability::EnvPresentUnverified
        );
        assert_eq!(selected.execution, ExecutionSupport::UnverifiedUserChoice);
        assert!(selected.selectable());
        assert!(
            !model_route_option_by_id(&connected, "native:openai:gpt-6-sol")
                .unwrap()
                .selectable()
        );
    }

    #[test]
    fn catalog_enriches_labels_without_authorizing_models_or_efforts() {
        let catalog: CatalogSnapshot = serde_json::from_value(json!({
            "fetched_at_ms": 1,
            "providers": {
                "openai": {
                    "id": "openai", "name": "OpenAI catalog", "models": {
                        "gpt-6-sol": {"id": "gpt-6-sol", "name": "Catalog label"},
                        "future-model": {"id": "future-model", "name": "Future", "reasoning": true, "tool_call": true}
                    }
                },
                "openrouter": {
                    "id": "openrouter", "name": "OpenRouter catalog", "models": {
                        "vendor/future": {"id": "vendor/future", "name": "Future route", "reasoning": true, "tool_call": true}
                    }
                }
            }
        }))
        .unwrap();
        let settings = NativeProviderSettings {
            provider_id: "openai".into(),
            model_id: "gpt-6-sol".into(),
            ..NativeProviderSettings::default()
        };
        let routes = model_route_options_with_keys(Some(&catalog), &settings, &[], |_| true);
        let reviewed = model_route_option_by_id(&routes, "native:openai:gpt-6-sol").unwrap();
        assert_eq!(reviewed.model_name, "Catalog label");
        assert!(reviewed.catalog_present);
        assert_eq!(reviewed.source, RouteSource::AdapterReview);
        assert!(reviewed.effort_options.iter().any(|(id, _)| id == "max"));
        assert!(model_route_option_by_id(&routes, "native:openai:future-model").is_none());
        assert!(model_route_option_by_id(&routes, "native:openrouter:vendor/future").is_none());
    }

    #[test]
    fn exact_provider_pair_controls_efforts_and_selected_unsupported_route() {
        let settings = NativeProviderSettings {
            provider_id: "openai".into(),
            model_id: "future-model".into(),
            ..NativeProviderSettings::default()
        };
        let routes = model_route_options_with_keys(None, &settings, &[], |_| true);
        let unsupported = model_route_option_by_id(&routes, "native:openai:future-model").unwrap();
        assert_eq!(
            unsupported.execution,
            ExecutionSupport::UnverifiedUserChoice
        );
        assert!(unsupported.selectable());
        assert!(unsupported.effort_options.is_empty());
        assert!(model_route_option_by_id(&routes, "native:openai:gpt-6-sol")
            .unwrap()
            .effort_options
            .iter()
            .any(|(id, _)| id == "max"));
        assert!(model_route_option_by_id(&routes, "codex:codex:gpt-6-sol")
            .unwrap()
            .effort_options
            .iter()
            .any(|(id, _)| id == "ultra"));
    }

    #[test]
    fn authenticated_listing_keeps_unknown_capabilities_but_disables_explicit_denials() {
        let settings = NativeProviderSettings {
            provider_id: "openrouter".into(),
            model_id: "vendor/manual".into(),
            ..NativeProviderSettings::default()
        };
        let models = [
            (
                "vendor/agent",
                DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: "vendor/agent".into(),
                    name: "Agent".into(),
                    provider_reports_tools: true,
                    tool_call_declared: Some(true),
                    text_input_output: true,
                    supported_efforts: Vec::new(),
                    context_window_tokens: None,
                    max_output_tokens: None,
                },
            ),
            (
                "vendor/no-tools",
                DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: "vendor/no-tools".into(),
                    name: "No tools".into(),
                    provider_reports_tools: false,
                    tool_call_declared: Some(false),
                    text_input_output: true,
                    supported_efforts: Vec::new(),
                    context_window_tokens: None,
                    max_output_tokens: None,
                },
            ),
            (
                "vendor/unknown-tools",
                DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: "vendor/unknown-tools".into(),
                    name: "Unknown tools".into(),
                    provider_reports_tools: false,
                    tool_call_declared: None,
                    text_input_output: true,
                    supported_efforts: Vec::new(),
                    context_window_tokens: None,
                    max_output_tokens: None,
                },
            ),
        ]
        .into_iter()
        .map(|(id, model)| (id.to_owned(), model))
        .collect::<BTreeMap<_, _>>();
        let discovery = ProviderModelDiscovery {
            provider_id: "openrouter".into(),
            models,
        };
        let routes = model_route_options_with_keys(None, &settings, &[discovery], |_| true);
        let agent = model_route_option_by_id(&routes, "native:openrouter:vendor/agent").unwrap();
        assert_eq!(agent.source, RouteSource::ProviderDiscovery);
        assert_eq!(
            agent.execution,
            ExecutionSupport::ProviderAdvertisedUnverified
        );
        assert_eq!(agent.provider_listed, Some(true));
        assert!(agent.selectable());
        assert!(agent.effort_options.is_empty());
        let denied =
            model_route_option_by_id(&routes, "native:openrouter:vendor/no-tools").unwrap();
        assert_eq!(denied.execution, ExecutionSupport::Unsupported);
        assert_eq!(denied.provider_listed, Some(true));
        assert!(!denied.selectable());
        let unknown =
            model_route_option_by_id(&routes, "native:openrouter:vendor/unknown-tools").unwrap();
        assert_eq!(
            unknown.execution,
            ExecutionSupport::ProviderListedUnverified
        );
        assert!(unknown.selectable());
        let stale = model_route_option_by_id(&routes, "native:openrouter:vendor/manual").unwrap();
        assert_eq!(stale.provider_listed, Some(false));
        assert!(!stale.selectable());
    }

    #[test]
    fn codex_native_routes_preserve_official_order_and_do_not_resurrect_hidden_or_selected_ids() {
        let discovery = native_provider::parse_connected_models_for_adapter(
            &native_provider::adapter("codex").unwrap(), &serde_json::to_vec(&native_provider::tests::codex_ordered_listing()).unwrap(),
        ).unwrap();
        for selected in ["model-beta", "hidden-first", "model-omitted"] {
            let settings = NativeProviderSettings {
                provider_id: "codex".into(), model_id: selected.into(),
                ..NativeProviderSettings::default()
            };
            let routes = model_route_options_with_keys(None, &settings, std::slice::from_ref(&discovery), |_| true);
            let visible = routes.iter().filter(|route| route.backend == "native" && route.provider_id == "codex" && route.provider_listed != Some(false)).collect::<Vec<_>>();
            assert_eq!(visible.iter().map(|route| route.model_id.as_str()).collect::<Vec<_>>(),
                ["model-zulu", "model-alpha", "model-middle", "model-beta"]);
            assert_eq!(visible[0].model_name, "Modelo Zulu");
            assert!(visible.iter().all(|route| route.execution == ExecutionSupport::ProviderListedUnverified));
            assert!(routes.iter().filter(|route| route.backend == "native" && route.provider_id == "codex" && route.provider_listed == Some(false)).all(|route| !route.selectable()));
        }
    }

    #[test]
    fn account_listing_can_narrow_reviewed_native_routes() {
        let settings = NativeProviderSettings {
            provider_id: "openai".into(),
            model_id: "gpt-6-sol".into(),
            ..NativeProviderSettings::default()
        };
        let discovery = ProviderModelDiscovery {
            provider_id: "openai".into(),
            models: [(
                "gpt-6-sol".into(),
                DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: "gpt-6-sol".into(),
                    name: "Provider Sol".into(),
                    provider_reports_tools: false,
                    tool_call_declared: None,
                    text_input_output: false,
                    supported_efforts: Vec::new(),
                    context_window_tokens: None,
                    max_output_tokens: None,
                },
            )]
            .into_iter()
            .collect(),
        };
        let routes = model_route_options_with_keys(None, &settings, &[discovery], |_| true);
        let listed = model_route_option_by_id(&routes, "native:openai:gpt-6-sol").unwrap();
        assert_eq!(listed.model_name, "Provider Sol");
        assert_eq!(listed.execution, ExecutionSupport::Unsupported);
        assert!(!listed.selectable());
        let unlisted = model_route_option_by_id(&routes, "native:openai:gpt-6-astra").unwrap();
        assert_eq!(unlisted.execution, ExecutionSupport::Verified);
        assert_eq!(unlisted.provider_listed, Some(false));
        assert!(!unlisted.selectable());
    }

    #[test]
    fn account_discovery_denials_override_reviewed_and_explicit_current_models() {
        for model_id in ["gpt-4.1", "gpt-4.1-future"] {
            let settings = NativeProviderSettings {
                provider_id: "openai".into(),
                model_id: model_id.into(),
                ..NativeProviderSettings::default()
            };
            for (tool_call_declared, text_input_output) in
                [(Some(false), true), (None, false), (None, true)]
            {
                let discovery = ProviderModelDiscovery {
                    provider_id: "openai".into(),
                    models: [(
                        model_id.to_owned(),
                        DiscoveredModel {
                            listing_index: None,
                            limits_declared: false,
                            efforts_declared: false,
                            capabilities_declared: false,
                            max_input_tokens: None,
                            declared_capabilities: Default::default(),
                            id: model_id.into(),
                            name: model_id.into(),
                            provider_reports_tools: false,
                            tool_call_declared,
                            text_input_output,
                            supported_efforts: Vec::new(),
                            context_window_tokens: None,
                            max_output_tokens: None,
                        },
                    )]
                    .into_iter()
                    .collect(),
                };
                let routes = model_route_options_with_keys(None, &settings, &[discovery], |_| true);
                let route = model_route_option_by_id(&routes, &format!("native:openai:{model_id}"))
                    .unwrap();
                assert_eq!(route.provider_listed, Some(true));
                if tool_call_declared == Some(false) || !text_input_output {
                    assert_eq!(route.execution, ExecutionSupport::Unsupported);
                    assert!(!route.selectable());
                } else {
                    assert!(route.selectable());
                    assert_eq!(
                        route.execution,
                        if model_id == "gpt-4.1" {
                            ExecutionSupport::Verified
                        } else {
                            ExecutionSupport::ProviderListedUnverified
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn discovered_efforts_and_user_provider_routes_are_data_driven() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("provider-registry.json"), r#"{
            "version":1,
            "providers":[{
                "id":"new-provider","name":"New Provider",
                "endpoint":"https://api.example.test/v1","key_env":"NEW_PROVIDER_API_KEY",
                "wire_api":"chat_completions","model_list_path":"models",
                "reasoning_effort_field":"reasoning_effort",
                "catalog":{"tool_call_path":"capabilities.tools","effort_levels_path":"capabilities.efforts","input_modalities_path":"input_modalities","output_modalities_path":"output_modalities"}
            }]
        }"#).unwrap();
        let registry = native_provider::ProviderRegistry::load(root.path()).unwrap();
        let settings = NativeProviderSettings {
            provider_id: "new-provider".into(),
            model_id: "reasoner-123".into(),
            ..NativeProviderSettings::default()
        };
        let discovery = native_provider::parse_connected_models_for_adapter(
            registry.adapter("new-provider").unwrap(),
            br#"{"data":[{"id":"reasoner-123","input_modalities":["text"],"output_modalities":["text"],"capabilities":{"tools":true,"efforts":["low","high"]}}]}"#,
        ).unwrap();
        let routes = model_route_options_with_keys_and_registry(
            None,
            &settings,
            std::slice::from_ref(&discovery),
            &registry,
            |_| true,
        );
        let route = model_route_option_by_id(&routes, "native:new-provider:reasoner-123").unwrap();
        assert!(route.selectable());
        assert_eq!(route.source, RouteSource::ProviderDiscovery);
        assert_eq!(
            route.execution,
            ExecutionSupport::ProviderAdvertisedUnverified
        );
        assert_eq!(
            route
                .effort_options
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["default", "low", "high"]
        );

        let mut catalog = crate::model_catalog::reviewed_seed_snapshot();
        catalog.providers.insert(
            "new-provider".into(),
            serde_json::from_value(serde_json::json!({
                "id":"new-provider", "name":"New Provider",
                "models":{"reasoner-123":{
                    "id":"reasoner-123", "name":"Reasoner",
                    "provider_efforts":["tier1", "tier2"],
                    "declared_capabilities":{"effort":false}
                }}
            }))
            .unwrap(),
        );
        let refreshed = model_route_options_with_keys_and_registry(
            Some(&catalog),
            &settings,
            &[discovery],
            &registry,
            |_| true,
        );
        let route =
            model_route_option_by_id(&refreshed, "native:new-provider:reasoner-123").unwrap();
        assert_eq!(
            route
                .effort_options
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["default", "low", "high"]
        );
        catalog
            .providers
            .get_mut("new-provider")
            .unwrap()
            .models
            .get_mut("reasoner-123")
            .unwrap()
            .declared_capabilities
            .clear();
        let basic_listing = native_provider::parse_connected_models_for_adapter(
            registry.adapter("new-provider").unwrap(),
            br#"{"data":[{"id":"reasoner-123","input_modalities":["text"],"output_modalities":["text"],"capabilities":{"tools":true}}]}"#,
        )
        .unwrap();
        let fresh_unknown = model_route_options_with_keys_and_registry(
            Some(&catalog),
            &settings,
            &[basic_listing],
            &registry,
            |_| true,
        );
        let route =
            model_route_option_by_id(&fresh_unknown, "native:new-provider:reasoner-123").unwrap();
        assert!(route.effort_options.is_empty());
        let from_catalog = model_route_options_with_keys_and_registry(
            Some(&catalog),
            &settings,
            &[],
            &registry,
            |_| true,
        );
        let route =
            model_route_option_by_id(&from_catalog, "native:new-provider:reasoner-123").unwrap();
        assert_eq!(
            route
                .effort_options
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["default", "tier1", "tier2"]
        );
    }

    #[test]
    fn listing_levels_do_not_enable_an_unverified_effort_transport() {
        let listing = native_provider::parse_connected_models_for_adapter(
            &native_provider::adapter("xai").unwrap(),
            br#"{"models":[{"id":"grok-new","capabilities":{"reasoning_effort":["low","high"]},"input_modalities":["text"],"output_modalities":["text"]}]}"#,
        ).unwrap();
        assert_eq!(
            listing.models["grok-new"].supported_efforts,
            ["low", "high"]
        );
        assert!(
            effort_options_for_discovered_provider("native", "grok-new", "xai", &[listing])
                .is_empty()
        );
    }

    #[test]
    fn live_codex_catalog_replaces_static_models_and_sets_default_efforts() {
        let models = [openide_cli::DiscoveredCodexModel {
            id: "gpt-6-luna".into(),
            display_name: "Luna actual".into(),
            description: None,
            input_modalities: Vec::new(),
            supported_efforts: vec!["low".into(), "ultra".into()],
            is_default: true,
        }];
        let routes = model_route_options_with_codex_discovery(
            None,
            &NativeProviderSettings::default(),
            &[],
            &models,
        );
        assert!(model_route_option_by_id(&routes, "codex:codex:gpt-6-sol").is_none());
        let default = model_route_option_by_id(&routes, "codex:codex:default").unwrap();
        assert_eq!(default.model_name, "Predeterminado · Luna actual");
        assert_eq!(default.source, RouteSource::CliLiveCatalog);
        assert_eq!(default.effort_options[0].0, "default");
        assert_eq!(default.effort_options[2].0, "ultra");
        let luna = model_route_option_by_id(&routes, "codex:codex:gpt-6-luna").unwrap();
        assert_eq!(luna.effort_options, default.effort_options);
        assert!(luna.selectable());
    }
}
