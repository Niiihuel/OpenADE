//! Model and reasoning controls shared by the home and session composers.
//! Choices are stored separately from the visible tree so changing a dock
//! does not silently reset the configuration of the selected runner.

use std::collections::BTreeMap;
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

    pub fn selected_backend(&self) -> Option<&str> {
        self.selected_backend.as_deref()
    }

    pub fn set_selected_backend(&mut self, backend: &str) {
        self.selected_backend = Some(backend.to_owned());
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
    /// User explicitly configured this OpenRouter or custom route.
    UnverifiedUserChoice,
    /// Codex CLI validates the selected model and account when it runs.
    DelegatedCli,
    /// The authenticated provider catalog advertises a text and tool route,
    /// but the native harness has not verified its complete wire contract.
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
/// reasoning level. The only unreviewed native route is the user's existing
/// explicit OpenRouter/custom selection.
pub(crate) fn model_route_options(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
) -> Vec<ModelRouteOption> {
    model_route_options_with_discovery(catalog, settings, &[])
}

/// Use a worker-produced, authenticated `/models` result to narrow reviewed
/// routes and add OpenRouter routes advertised for this account. Never pass a
/// catalog snapshot without authentication as `discovery`: it cannot establish
/// account access.
pub(crate) fn model_route_options_with_discovery(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    discoveries: &[ProviderModelDiscovery],
) -> Vec<ModelRouteOption> {
    model_route_options_with_keys(
        catalog,
        settings,
        discoveries,
        native_provider::adapter_key_present,
    )
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

fn model_route_options_with_keys(
    catalog: Option<&CatalogSnapshot>,
    settings: &NativeProviderSettings,
    discoveries: &[ProviderModelDiscovery],
    has_key: impl Fn(&str) -> bool,
) -> Vec<ModelRouteOption> {
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
        let adapter = native_provider::adapter(provider_id).expect("reviewed adapter exists");
        let metadata = catalog.and_then(|catalog| catalog.model(provider_id, model_id));
        let discovery = discoveries
            .iter()
            .find(|entry| entry.provider_id == provider_id);
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
            credential: if has_key(provider_id) {
                CredentialAvailability::EnvPresentUnverified
            } else {
                CredentialAvailability::MissingCredential
            },
            execution: if native_provider::native_route_execution(provider_id, model_id, false)
                == NativeRouteExecution::Verified
            {
                ExecutionSupport::Verified
            } else {
                ExecutionSupport::Unsupported
            },
            source: RouteSource::AdapterReview,
            catalog_present: metadata.is_some(),
            provider_listed: discovery.map(|entry| entry.models.contains_key(model_id)),
            effort_options: effort_options_for_provider("native", model_id, provider_id),
        });
    }

    if let Some(discovery) = discoveries
        .iter()
        .find(|entry| entry.provider_id == "openrouter")
    {
        for model in discovery.models.values() {
            if !model.provider_reports_tools || !model.text_input_output {
                continue;
            }
            let metadata = catalog.and_then(|catalog| catalog.model("openrouter", &model.id));
            options.push(ModelRouteOption {
                route_id: format!("native:openrouter:{}", model.id),
                backend: "native".to_owned(),
                provider_id: "openrouter".to_owned(),
                provider_name: "OpenRouter".to_owned(),
                model_id: model.id.clone(),
                model_name: model.name.clone(),
                credential: if has_key("openrouter") {
                    CredentialAvailability::EnvPresentUnverified
                } else {
                    CredentialAvailability::MissingCredential
                },
                execution: ExecutionSupport::ProviderAdvertisedUnverified,
                source: RouteSource::ProviderDiscovery,
                catalog_present: metadata.is_some(),
                provider_listed: Some(true),
                effort_options: Vec::new(),
            });
        }
    }

    let provider_id = settings.provider_id.trim();
    let model_id = settings.model_id.trim();
    if let Some(adapter) = native_provider::adapter(provider_id) {
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
            let execution =
                match native_provider::native_route_execution(provider_id, model_id, true) {
                    NativeRouteExecution::Verified => ExecutionSupport::Verified,
                    NativeRouteExecution::ExplicitUnverified if settings.resolve().is_ok() => {
                        ExecutionSupport::UnverifiedUserChoice
                    }
                    _ => ExecutionSupport::Unsupported,
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
                credential: if has_key(provider_id) {
                    CredentialAvailability::EnvPresentUnverified
                } else if provider_id == "custom" && settings.resolve().is_ok() {
                    CredentialAvailability::NoKeyRequired
                } else {
                    CredentialAvailability::MissingCredential
                },
                execution,
                source: RouteSource::UserConfigured,
                catalog_present: metadata.is_some(),
                provider_listed: discovery.map(|entry| entry.models.contains_key(model_id)),
                effort_options: effort_options_for_provider("native", model_id, provider_id),
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
        assert_eq!(unsupported.execution, ExecutionSupport::Unsupported);
        assert!(!unsupported.selectable());
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
    fn authenticated_listing_adds_only_provider_advertised_text_tool_routes() {
        let settings = NativeProviderSettings {
            provider_id: "openrouter".into(),
            model_id: "vendor/manual".into(),
            ..NativeProviderSettings::default()
        };
        let models = [
            (
                "vendor/agent",
                DiscoveredModel {
                    id: "vendor/agent".into(),
                    name: "Agent".into(),
                    provider_reports_tools: true,
                    text_input_output: true,
                },
            ),
            (
                "vendor/no-tools",
                DiscoveredModel {
                    id: "vendor/no-tools".into(),
                    name: "No tools".into(),
                    provider_reports_tools: false,
                    text_input_output: true,
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
        assert!(model_route_option_by_id(&routes, "native:openrouter:vendor/no-tools").is_none());
        let stale = model_route_option_by_id(&routes, "native:openrouter:vendor/manual").unwrap();
        assert_eq!(stale.provider_listed, Some(false));
        assert!(!stale.selectable());
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
                    id: "gpt-6-sol".into(),
                    name: "Provider Sol".into(),
                    provider_reports_tools: false,
                    text_input_output: false,
                },
            )]
            .into_iter()
            .collect(),
        };
        let routes = model_route_options_with_keys(None, &settings, &[discovery], |_| true);
        let listed = model_route_option_by_id(&routes, "native:openai:gpt-6-sol").unwrap();
        assert_eq!(listed.model_name, "Provider Sol");
        assert!(listed.selectable());
        let unlisted = model_route_option_by_id(&routes, "native:openai:gpt-6-astra").unwrap();
        assert_eq!(unlisted.execution, ExecutionSupport::Verified);
        assert_eq!(unlisted.provider_listed, Some(false));
        assert!(!unlisted.selectable());
    }

    #[test]
    fn live_codex_catalog_replaces_static_models_and_sets_default_efforts() {
        let models = [openide_cli::DiscoveredCodexModel {
            id: "gpt-6-luna".into(),
            display_name: "Luna actual".into(),
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
