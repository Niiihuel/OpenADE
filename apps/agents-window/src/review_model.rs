//! Which model a review runs on. The choice never changes the model the user
//! selected for their own work: it is handed to one turn and discarded.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model_catalog::{CatalogSnapshot, ModelInfo};
use crate::native_provider::NativeProviderSettings;

const FILE_NAME: &str = "review-model.json";
const VERSION: u32 = 1;
/// Automatic only switches for a real saving: the candidate must cost at most
/// this share of the current model.
const AUTOMATIC_MAX_COST_SHARE: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewModelMode {
    /// The model selected in the composer. Nothing changes.
    #[default]
    Same,
    /// A cheaper model of the same provider, when the catalog can justify one.
    Automatic,
    /// The provider and model the user saved.
    Manual,
}

impl ReviewModelMode {
    pub const ALL: [Self; 3] = [Self::Same, Self::Automatic, Self::Manual];

    pub const fn action_id(self) -> &'static str {
        match self {
            Self::Same => "review_model:same",
            Self::Automatic => "review_model:automatic",
            Self::Manual => "review_model:manual",
        }
    }

    pub fn from_action_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.action_id() == id)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Same => "El mismo modelo",
            Self::Automatic => "Automático",
            Self::Manual => "Manual",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewModelSettings {
    version: u32,
    pub mode: ReviewModelMode,
    /// The saved manual choice. Kept when another mode is selected.
    #[serde(default)]
    pub manual: Option<NativeProviderSettings>,
}

impl ReviewModelSettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(root.join(FILE_NAME)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("No se pudo leer el modelo de revisión: {error}")),
        };
        let settings: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("El modelo de revisión no es válido: {error}"))?;
        if settings.version != VERSION {
            return Err(format!("Versión de modelo de revisión no compatible: {}", settings.version));
        }
        Ok(settings)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let document = Self { version: VERSION, ..self.clone() };
        let temporary = root.join(format!(".{FILE_NAME}-{}.tmp", std::process::id()));
        std::fs::create_dir_all(root)
            .and_then(|()| std::fs::write(&temporary, serde_json::to_vec_pretty(&document).unwrap_or_default()))
            .and_then(|()| crate::agent_tool_settings::replace_atomically(&temporary, &root.join(FILE_NAME)))
            .map_err(|error| {
                let _ = std::fs::remove_file(&temporary);
                format!("No se pudo guardar el modelo de revisión: {error}")
            })
    }

    /// The model a review should run on, or `None` for the selected one.
    /// `Err` means the user's manual choice cannot be used as saved; a review
    /// must then not start on some other model without saying so.
    pub fn resolve(
        &self,
        current: &NativeProviderSettings,
        catalog: Option<&CatalogSnapshot>,
    ) -> Result<Option<NativeProviderSettings>, String> {
        match self.mode {
            ReviewModelMode::Same => Ok(None),
            ReviewModelMode::Automatic => Ok(catalog.and_then(|catalog| cheaper_model(current, catalog))),
            ReviewModelMode::Manual => match &self.manual {
                Some(manual) if !manual.provider_id.is_empty() && !manual.model_id.is_empty() => {
                    Ok((manual != current).then(|| manual.clone()))
                }
                _ => Err("Elegí el modelo de revisión en Ajustes → Agente antes de usar el modo manual".into()),
            },
        }
    }
}

/// Price of one million input plus one million output tokens, when both are
/// published. A model with an unknown price is never compared.
fn total_cost(model: &ModelInfo) -> Option<f64> {
    let cost = model.cost.as_ref()?;
    let total = cost.input? + cost.output?;
    (total.is_finite() && total > 0.0).then_some(total)
}

/// The model automatic mode would review with: same provider, usable for an
/// agent, with a published price of at most half the current model's. Among
/// those it takes the most expensive one, the step down rather than the
/// bottom of the list, because price is the only quality signal here.
pub(crate) fn cheaper_model(
    current: &NativeProviderSettings,
    catalog: &CatalogSnapshot,
) -> Option<NativeProviderSettings> {
    let provider = catalog.providers.get(&current.provider_id)?;
    let ceiling = total_cost(provider.models.get(&current.model_id)?)? * AUTOMATIC_MAX_COST_SHARE;
    provider
        .models
        .values()
        .filter(|model| model.id != current.model_id)
        .filter(|model| crate::provider_settings::catalog_model_usable(&current.provider_id, model))
        // An agent review needs tools; "not declared" is not good enough here.
        .filter(|model| model.tool_call || model.tool_call_declared == Some(true))
        .filter_map(|model| Some((total_cost(model)?, model)))
        .filter(|(cost, _)| *cost <= ceiling)
        // Highest price first; the ID breaks ties so the pick is stable.
        .max_by(|(left_cost, left), (right_cost, right)| {
            left_cost
                .total_cmp(right_cost)
                .then_with(|| right.id.cmp(&left.id))
        })
        .map(|(_, model)| NativeProviderSettings {
            model_id: model.id.clone(),
            ..current.clone()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_catalog::{ModelCost, ProviderInfo};

    fn model(id: &str, input: Option<f64>, output: Option<f64>, tools: Option<bool>) -> ModelInfo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "tool_call": tools == Some(true),
            "tool_call_declared": tools,
            "cost": ModelCost { input, output, cache_read: None, cache_write: None, extra: Default::default() },
        }))
        .unwrap()
    }

    fn catalog(models: Vec<ModelInfo>) -> CatalogSnapshot {
        let provider: ProviderInfo = serde_json::from_value(serde_json::json!({
            "id": "acme",
            "name": "Acme",
            "models": models.into_iter().map(|model| (model.id.clone(), model)).collect::<std::collections::BTreeMap<_, _>>(),
        }))
        .unwrap();
        serde_json::from_value(serde_json::json!({ "providers": { "acme": provider }, "fetched_at_ms": 0 })).unwrap()
    }

    fn selected(model: &str) -> NativeProviderSettings {
        NativeProviderSettings { provider_id: "acme".into(), model_id: model.into(), custom_endpoint: "https://e".into() }
    }

    #[test]
    fn automatic_takes_the_step_down_not_the_cheapest_and_only_for_a_real_saving() {
        let catalog = catalog(vec![
            model("big", Some(10.0), Some(30.0), Some(true)),     // 40
            model("almost", Some(9.0), Some(27.0), Some(true)),   // 36: no real saving
            model("mid", Some(4.0), Some(12.0), Some(true)),      // 16: the step down
            model("small", Some(1.0), Some(3.0), Some(true)),     // 4
            model("tiny", Some(0.1), Some(0.1), Some(true)),      // 0.2: bottom of the list
        ]);
        let picked = cheaper_model(&selected("big"), &catalog).unwrap();
        assert_eq!(picked.model_id, "mid");
        assert_eq!((picked.provider_id.as_str(), picked.custom_endpoint.as_str()), ("acme", "https://e"));
        // From the mid model, half is 8: only small and tiny qualify; small is the step down.
        assert_eq!(cheaper_model(&selected("mid"), &catalog).unwrap().model_id, "small");
        // Nothing costs at most half of the cheapest.
        assert_eq!(cheaper_model(&selected("tiny"), &catalog), None);
    }

    #[test]
    fn automatic_never_guesses_without_prices_tools_or_a_known_current_model() {
        let catalog = catalog(vec![
            model("big", Some(10.0), Some(30.0), Some(true)),
            model("no-price", None, None, Some(true)),
            model("half-price", Some(1.0), None, Some(true)),
            model("free", Some(0.0), Some(0.0), Some(true)),
            model("no-tools", Some(1.0), Some(1.0), Some(false)),
            model("undeclared", Some(1.0), Some(1.0), None),
            model("unpriced-current", None, None, Some(true)),
        ]);
        assert_eq!(cheaper_model(&selected("big"), &catalog), None);
        assert_eq!(cheaper_model(&selected("unpriced-current"), &catalog), None);
        assert_eq!(cheaper_model(&selected("not-in-catalog"), &catalog), None);
        let mut elsewhere = selected("big");
        elsewhere.provider_id = "other".into();
        assert_eq!(cheaper_model(&elsewhere, &catalog), None);
    }

    /// Situation 2, against the catalog the installed app really has. Reads
    /// that file only. Skipped when it is absent unless
    /// `OPENIDE_REQUIRE_LIVE_PLUGINS` is set.
    #[test]
    fn real_catalog_automatic_mode_only_switches_where_prices_and_tools_are_published() {
        let path = std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join(".local/share/openide/provider-catalog.json"))
            .filter(|path| path.is_file());
        let Some(path) = path else {
            assert!(std::env::var_os("OPENIDE_REQUIRE_LIVE_PLUGINS").is_none(), "no installed catalog to check");
            return;
        };
        let catalog: CatalogSnapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut switched = Vec::new();
        let mut unpriced_providers = Vec::new();
        for (provider_id, provider) in &catalog.providers {
            let priced = provider.models.values().filter(|model| total_cost(model).is_some()).count();
            if priced == 0 {
                unpriced_providers.push(provider_id.clone());
            }
            for model in provider.models.values() {
                let current = NativeProviderSettings {
                    provider_id: provider_id.clone(),
                    model_id: model.id.clone(),
                    custom_endpoint: String::new(),
                };
                let Some(picked) = cheaper_model(&current, &catalog) else {
                    continue;
                };
                // Every pick honours the rule on real data.
                let chosen = &provider.models[&picked.model_id];
                assert_eq!(picked.provider_id, *provider_id);
                assert!(chosen.tool_call || chosen.tool_call_declared == Some(true), "{}", chosen.id);
                assert!(total_cost(chosen).unwrap() <= total_cost(model).unwrap() * AUTOMATIC_MAX_COST_SHARE);
                switched.push(provider_id.clone());
            }
        }
        switched.dedup();
        // A provider that publishes no prices can never be switched automatically.
        for provider in &unpriced_providers {
            assert!(!switched.contains(provider), "{provider} switched without prices");
        }
        eprintln!("automatic can switch within: {switched:?}; never within (no prices): {unpriced_providers:?}");
    }

    #[test]
    fn modes_resolve_to_no_override_an_override_or_an_explicit_error() {
        let catalog = catalog(vec![
            model("big", Some(10.0), Some(30.0), Some(true)),
            model("mid", Some(4.0), Some(12.0), Some(true)),
        ]);
        let current = selected("big");
        let mut settings = ReviewModelSettings::default();
        assert_eq!(settings.resolve(&current, Some(&catalog)), Ok(None));

        settings.mode = ReviewModelMode::Automatic;
        assert_eq!(settings.resolve(&current, Some(&catalog)).unwrap().unwrap().model_id, "mid");
        // No catalog yet: the selected model, not a failure.
        assert_eq!(settings.resolve(&current, None), Ok(None));

        settings.mode = ReviewModelMode::Manual;
        assert!(settings.resolve(&current, Some(&catalog)).is_err(), "manual with nothing saved must not fall back silently");
        let other = NativeProviderSettings { provider_id: "zeta".into(), model_id: "z1".into(), custom_endpoint: String::new() };
        settings.manual = Some(other.clone());
        assert_eq!(settings.resolve(&current, None), Ok(Some(other)));
        // The saved choice is the selected model: nothing to override.
        settings.manual = Some(current.clone());
        assert_eq!(settings.resolve(&current, None), Ok(None));
    }

    #[test]
    fn settings_persist_and_keep_the_manual_choice_across_mode_changes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ReviewModelSettings::load(dir.path()).unwrap().mode, ReviewModelMode::Same);
        let mut settings = ReviewModelSettings::default();
        settings.manual = Some(selected("mid"));
        settings.mode = ReviewModelMode::Automatic;
        settings.save(dir.path()).unwrap();
        let loaded = ReviewModelSettings::load(dir.path()).unwrap();
        assert_eq!(loaded.mode, ReviewModelMode::Automatic);
        assert_eq!(loaded.manual, Some(selected("mid")));
        for mode in ReviewModelMode::ALL {
            assert_eq!(ReviewModelMode::from_action_id(mode.action_id()), Some(mode));
        }
        assert_eq!(ReviewModelMode::from_action_id("review_model:cheapest"), None);
        std::fs::write(dir.path().join(FILE_NAME), r#"{"version":7,"mode":"same"}"#).unwrap();
        assert!(ReviewModelSettings::load(dir.path()).is_err());
    }
}
