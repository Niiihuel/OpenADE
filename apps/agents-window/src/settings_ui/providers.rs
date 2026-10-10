//! Provider preferences: boxed lists, explicit connection flows, exact model data.

use super::{control_palette, outline, subtle, text};
use crate::{
    model_catalog::ModelInfo,
    provider_connections::{self, ConnectPhase},
    provider_icons::icon_for_provider,
    provider_settings::{self, CatalogModelsFilter},
    settings_parts::{self, FeedbackTone},
    theme,
    view::ViewState,
};
use openide_ui::{
    ActionRow, Button, ButtonVariant, Component, ControlSize, Element, FontFamily,
    Icon, Modal, ScrollArea, SearchField, SkeletonGroup, Spinner, TextField,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

fn connected(state: &ViewState, id: &str) -> bool {
    if !provider_connections::supported(id) {
        return state
            .native_provider_definitions
            .get(id)
            .is_some_and(|definition| definition.key_available || definition.key_optional);
    }
    state
        .provider_connections
        .get(id)
        .is_some_and(|c| c.configured && !c.needs_sign_in)
}
fn status(state: &ViewState, id: &str) -> String {
    if !provider_connections::supported(id) {
        return state.native_provider_definitions.get(id).map_or_else(
            || "Proveedor sin registrar".into(),
            |definition| {
                if definition.key_available {
                    "Credencial disponible · acceso sin verificar".into()
                } else if definition.key_optional {
                    "Conexión sin clave · acceso sin verificar".into()
                } else {
                    "Credencial requerida".into()
                }
            },
        );
    }
    let Some(c) = state.provider_connections.get(id) else {
        return "Sin conectar".into();
    };
    if c.needs_sign_in {
        return "Necesita volver a conectarse".into();
    }
    if !c.configured {
        return "Sin conectar".into();
    }
    if let Some(account) = &c.account {
        return account.clone();
    }
    if c.from_environment {
        return "Conexión configurada en el equipo".into();
    }
    if id == "opencode" {
        return format!(
            "OpenCode {} · clave guardada",
            if c.service == "go" { "Go" } else { "Zen" }
        );
    }
    if c.oauth {
        "Cuenta conectada".into()
    } else {
        "Clave guardada".into()
    }
}
fn refresh(state: &ViewState) -> Element {
    outline("catalog_refresh", "Actualizar modelos")
        .icon(Icon::Refresh)
        .loading(state.catalog_refreshing)
        .render()
}
fn overview(state: &ViewState, compact: bool) -> Vec<Element> {
    let mut sections = vec![
        SearchField::new(
            "catalog_provider_query",
            &state.catalog_provider_query,
            "Buscar proveedor…",
            control_palette(),
        )
        .size(ControlSize::Compact)
        .render(),
    ];
    let query = state.catalog_provider_query.trim().to_lowercase();
    for (title, active) in [("Conectados", true), ("Disponibles", false)] {
        let providers = provider_connections::PROVIDERS
            .iter()
            .filter(|(id, name, _)| {
                connected(state, id) == active
                    && (query.is_empty() || name.to_lowercase().contains(&query))
            })
            .collect::<Vec<_>>();
        let rows = providers
            .into_iter()
            .map(|(id, name, description)| {
                let subtitle = if active {
                    status(state, id)
                } else {
                    (*description).into()
                };
                ActionRow::new(*name, control_palette())
                    .subtitle(subtitle)
                    .icon(icon_for_provider(id))
                    .trailing(
                        Element::row(vec![
                            subtle(if active { "Ver modelos" } else { "Conectar" }),
                            Element::icon(Icon::ChevronRight).foreground(theme::FOREGROUND_SUBTLE),
                        ])
                        .gap(6.0),
                    )
                    .action(format!("catalog_browse:{id}"))
                    .render()
            })
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            sections.push(settings_parts::settings_group(title, rows));
        }
    }
    if sections.len() == 1 {
        sections.push(subtle(if query.is_empty() {
            "Conecta una cuenta para elegir sus modelos."
        } else {
            "No se encontraron proveedores."
        }));
    }
    if let Some(error) = &state.native_provider_registry_error {
        sections.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    sections.push(refresh(state));
    sections
}
fn catalog_builtin_adapter(provider: &str) -> Option<crate::native_provider::Adapter> {
    #[cfg(test)]
    CATALOG_REGISTRY_LOADS.with(|loads| loads.set(loads.get() + 1));
    crate::native_provider::ProviderRegistry::builtin()
        .adapter(provider)
        .cloned()
}

#[cfg(test)]
thread_local! {
    static CATALOG_REGISTRY_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Resolve the exact provider once per list. The first positive listing owns
/// capability filtering, while any explicit denial still blocks selection.
/// Rebuilding this small index from ViewState keeps current account discovery
/// authoritative without scanning every route for every catalog model.
struct CatalogModelContext<'a> {
    provider: &'a str,
    listed: HashMap<&'a str, &'a crate::composer_selection::ModelRouteOption>,
    blocked: HashSet<&'a str>,
    connection_usable: bool,
    adapter: Option<crate::native_provider::Adapter>,
    wire_api: Option<crate::native_provider::NativeWireApi>,
    filter: CatalogModelsFilter,
}

impl<'a> CatalogModelContext<'a> {
    fn new(state: &'a ViewState, provider: &'a str) -> Self {
        use crate::composer_selection::ExecutionSupport;
        let mut listed = HashMap::new();
        let mut blocked = HashSet::new();
        for route in &state.composer_routes {
            if route.backend != "native" || route.provider_id != provider {
                continue;
            }
            if route.provider_listed == Some(true) {
                listed.entry(route.model_id.as_str()).or_insert(route);
            }
            if route.execution == ExecutionSupport::Unsupported
                || route.provider_listed == Some(false)
            {
                blocked.insert(route.model_id.as_str());
            }
        }
        let definition = state.native_provider_definitions.get(provider);
        let connection_usable = provider_connections::supported(provider)
            && connected(state, provider)
            && definition.is_some();
        let adapter = connection_usable
            .then(|| catalog_builtin_adapter(provider))
            .flatten()
            .map(|mut adapter| {
                if provider == "opencode"
                    && state
                        .provider_connections
                        .get(provider)
                        .is_some_and(|c| c.service == "go")
                {
                    adapter.use_opencode_go();
                }
                adapter
            });
        Self {
            provider,
            listed,
            blocked,
            connection_usable,
            adapter,
            wire_api: definition.map(|definition| definition.wire_api),
            filter: state.catalog_models_filter,
        }
    }

    fn usable(&self, model: &ModelInfo) -> bool {
        let model_supported = self.listed.get(model.id.as_str()).map_or_else(
            || provider_settings::catalog_model_usable(self.provider, model),
            |route| {
                !model.id.is_empty()
                    && model.status.as_deref() != Some("deprecated")
                    && route.execution != crate::composer_selection::ExecutionSupport::Unsupported
            },
        );
        self.connection_usable
            && model_supported
            && !self.blocked.contains(model.id.as_str())
            && self
                .adapter
                .as_ref()
                .is_none_or(|adapter| adapter.for_model(&model.id).is_ok())
    }

    fn matches(&self, model: &ModelInfo, query: &str) -> bool {
        if !provider_settings::catalog_model_matches(
            self.provider,
            model,
            query,
            CatalogModelsFilter::All,
        ) {
            return false;
        }
        if self.filter != CatalogModelsFilter::AgentReady {
            return true;
        }
        self.listed.get(model.id.as_str()).map_or_else(
            || {
                provider_settings::catalog_model_agent_ready_for_wire(
                    self.provider,
                    self.wire_api,
                    model,
                )
            },
            |route| {
                model.status.as_deref() != Some("deprecated")
                    && matches!(
                        route.execution,
                        crate::composer_selection::ExecutionSupport::Verified
                            | crate::composer_selection::ExecutionSupport::ProviderAdvertisedUnverified
                    )
            },
        )
    }
}

/// A connected provider and the models of it that can run a turn, for a
/// picker outside the Providers page.
pub(crate) struct ProviderModelChoices {
    pub id: String,
    pub name: String,
    /// `(model id, display name)`, in the provider's own listing order.
    pub models: Vec<(String, String)>,
    /// Usable models that did not fit the list.
    pub omitted: usize,
}

/// The most models one picker lists. A provider with more is reached through
/// the composer's searchable picker instead.
pub(crate) const MAX_PICKER_MODELS: usize = 80;

/// Every connected provider that has at least one usable model, judged the
/// same way the Providers page judges "can this model be selected".
pub(crate) fn connected_model_choices(state: &ViewState) -> Vec<ProviderModelChoices> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Vec::new();
    };
    catalog
        .providers
        .iter()
        .filter(|(id, _)| connected(state, id))
        .filter_map(|(id, provider)| {
            let context = CatalogModelContext::new(state, id);
            let mut usable: Vec<&ModelInfo> = provider
                .models
                .values()
                .filter(|model| context.usable(model))
                .collect();
            usable.sort_by(|left, right| {
                left.listing_index
                    .unwrap_or(usize::MAX)
                    .cmp(&right.listing_index.unwrap_or(usize::MAX))
                    .then_with(|| left.name.cmp(&right.name))
            });
            let omitted = usable.len().saturating_sub(MAX_PICKER_MODELS);
            (!usable.is_empty()).then(|| ProviderModelChoices {
                id: id.clone(),
                name: provider.name.clone(),
                models: usable
                    .into_iter()
                    .take(MAX_PICKER_MODELS)
                    .map(|model| (model.id.clone(), model.name.clone()))
                    .collect(),
                omitted,
            })
        })
        .collect()
}

fn model_usable(state: &ViewState, id: &str, model: &ModelInfo) -> bool {
    CatalogModelContext::new(state, id).usable(model)
}

/// Only row keys and selection availability are copied. The virtual row
/// factory retains the immutable catalog snapshot and reads ModelInfo from it.
fn model_rows(state: &ViewState, id: &str, query: &str) -> Vec<(String, bool)> {
    let Some(provider) = state.catalog.as_ref().and_then(|c| c.providers.get(id)) else {
        return Vec::new();
    };
    let context = CatalogModelContext::new(state, id);
    let mut models = provider
        .models
        .iter()
        .filter(|(_, model)| context.matches(model, query))
        .collect::<Vec<_>>();
    if id == "codex" {
        // OpenAI's account catalog defines picker order. Selection only
        // decorates the row; it must not reorder the server's recommendation.
        models.sort_by_key(|(_, model)| model.listing_index.unwrap_or(usize::MAX));
    } else {
        // Existing provider lists reveal the selected model before details.
        models
            .sort_by_key(|(_, model)| !(state.native_provider_id == id && state.model == model.id));
    }
    models
        .into_iter()
        .map(|(key, model)| (key.clone(), context.usable(model)))
        .collect()
}

pub(crate) fn model_row_index(
    state: &ViewState,
    provider_id: &str,
    model_id: &str,
) -> Option<usize> {
    let provider = state.catalog.as_ref()?.providers.get(provider_id)?;
    model_rows(
        state,
        provider_id,
        &state.catalog_model_query.trim().to_lowercase(),
    )
    .iter()
    .position(|(key, _)| {
        provider
            .models
            .get(key)
            .is_some_and(|model| model.id == model_id)
    })
}

fn model_list(state: &ViewState, id: &str, width: f32) -> Element {
    let query = state.catalog_model_query.trim().to_lowercase();
    let rows = model_rows(state, id, &query);
    if rows.is_empty() {
        if connected(state, id) && state.catalog_refreshing {
            return SkeletonGroup::rows("catalog_models_progress", theme::component_palette(), 4)
                .row_height(30.0)
                .render()
                .padding(10.0);
        }
        return subtle(if !connected(state, id) {
            "Conecta este proveedor para consultar los modelos de tu cuenta."
        } else if query.is_empty() {
            "No hay modelos disponibles. Actualiza el listado para volver a consultar."
        } else {
            "No hay modelos que coincidan con la búsqueda."
        })
        .padding(10.0);
    }
    let catalog = Arc::clone(state.catalog.as_ref().expect("nonempty catalog rows"));
    let rows = Arc::new(rows);
    let active_provider = state.native_provider_id.clone();
    let active_model = state.model.clone();
    let provider = id.to_owned();
    let running = state.running;
    let row_height = if width < 440.0 { 88.0 } else { 56.0 };
    let height = (rows.len() as f32 * row_height).min(392.0);
    ScrollArea::uniform_list(
        format!("catalog_models:{id}"),
        rows.len(),
        row_height,
        move |index| {
            let (key, available) = &rows[index];
            let m = &catalog.providers[&provider].models[key];
            let selected = active_provider == provider && active_model == m.id;
            let name = Element::column(vec![
                Element::truncated_text(&m.name)
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND),
                Element::truncated_text(&m.id)
                    .font_family(FontFamily::Monospace)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE),
            ])
            .gap(4.0)
            .flex(1.0);
            let info = Button::new(
                format!("catalog_model_details:{}", m.id),
                "",
                control_palette(),
            )
            .icon(Icon::InfoCircle)
            .tooltip("Detalles del modelo")
            .size(ControlSize::Compact)
            .variant(ButtonVariant::Ghost)
            .render();
            // The model in use is a state, not an action: plain text in the
            // column where the other rows offer their button.
            let select = if selected {
                Element::truncated_text("En uso")
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::SETTINGS_FOREGROUND_SUBTLE)
                    .centered_x()
                    .size(Some(64.0), Some(ControlSize::Compact.height()))
            } else {
                outline(format!("catalog_select:{provider}/{}", m.id), "Usar")
                    .width(64.0)
                    .enabled(*available && !running)
                    .render()
            };
            let controls = Element::row(vec![info, select]).gap(6.0);
            let content = if width < 440.0 {
                Element::column(vec![name, controls]).gap(6.0)
            } else {
                Element::row(vec![name, controls.centered_y()]).gap(10.0)
            };
            content
                .padding(10.0)
                .size(None, Some(row_height))
                .border(theme::BORDER, 0.5)
        },
    )
    .render()
    .size(None, Some(height))
}

fn catalog_check_time(timestamp: u64) -> String {
    i64::try_from(timestamp)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%d/%m/%Y %H:%M:%S %Z")
                .to_string()
        })
        .unwrap_or_else(|| "fecha no disponible".into())
}

fn catalog_status_lines(state: &ViewState, id: &str) -> Vec<String> {
    use crate::model_catalog::CatalogSource;
    let catalog = state.catalog.as_deref();
    let status = catalog.and_then(|catalog| catalog.provider_status.get(id));
    let provider_name = catalog
        .and_then(|catalog| catalog.providers.get(id))
        .map(|provider| provider.name.as_str())
        .unwrap_or(id);
    let mut lines = vec![if id == "codex" {
        "Catálogo de tu cuenta".into()
    } else {
        format!("Modelos de {provider_name}")
    }];
    if let Some(status) = status
        .filter(|status| status.source == CatalogSource::OfficialApi && status.verified_at_ms > 0)
    {
        lines.push(format!(
            "{} · {}",
            if status.last_error.is_some() {
                "Última actualización"
            } else {
                "Actualizado"
            },
            catalog_check_time(status.verified_at_ms)
        ));
    } else {
        lines.push("Catálogo sin verificar".into());
    }
    if let Some(status) =
        status.filter(|status| status.last_error.is_some() && status.attempted_at_ms > 0)
    {
        lines.push(format!(
            "No se pudo actualizar · {}",
            catalog_check_time(status.attempted_at_ms)
        ));
    }
    lines
}

fn catalog_status(state: &ViewState, id: &str) -> Element {
    Element::column(
        catalog_status_lines(state, id)
            .into_iter()
            .map(|line| subtle(&line))
            .collect(),
    )
    .gap(6.0)
    .padding(10.0)
}

pub(super) fn page(state: &ViewState, compact: bool, width: f32) -> Vec<Element> {
    let id = state.catalog_browse_provider.as_str();
    if !provider_connections::supported(id) {
        let mut sections = overview(state, compact);
        if show_output_settings(state) {
            sections.push(output_settings(state, compact));
        }
        return sections;
    }
    let is_connected = connected(state, id);
    let mut sections = vec![
        Element::row(vec![
            Button::new("catalog_overview", "Proveedores", control_palette())
                .icon(Icon::ArrowLeft)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            Element::spacer().flex(1.0),
            refresh(state),
        ])
        .gap(12.0),
    ];
    sections.push(settings_parts::settings_group(
        "Conexión",
        vec![
            ActionRow::new(provider_connections::name(id), control_palette())
                .subtitle(status(state, id))
                .icon(icon_for_provider(id))
                .trailing(
                    outline(
                        format!("provider_connect:{id}"),
                        if is_connected {
                            "Reconectar"
                        } else {
                            "Conectar"
                        },
                    )
                    .width(96.0)
                    .enabled(!state.running)
                    .render(),
                )
                .render(),
        ],
    ));
    if show_output_settings(state) {
        sections.push(output_settings(state, compact));
    }
    if id == "codex" && is_connected && !state.provider_usage.contains_key(id) {
        sections.push(settings_parts::settings_group(
            "Plan de ChatGPT",
            vec![ActionRow::new("Uso de tu plan", control_palette())
                .subtitle("Esta conexión usa los límites de tu cuenta de ChatGPT.")
                .trailing(outline("provider_usage", "Ver uso").render())
                .render()],
        ));
    }
    let search = SearchField::new(
        "catalog_model_query",
        &state.catalog_model_query,
        "Buscar modelo…",
        control_palette(),
    )
    .size(ControlSize::Compact)
    .render();
    let filters = Element::row(vec![
        outline("catalog_models_filter:all", "Todos")
            .active(state.catalog_models_filter == CatalogModelsFilter::All)
            .render(),
        outline("catalog_models_filter:agent_ready", "Con herramientas")
            .active(state.catalog_models_filter == CatalogModelsFilter::AgentReady)
            .render(),
    ])
    .gap(6.0);
    sections.push(settings_parts::settings_group(
        "Modelos",
        vec![
            catalog_status(state, id),
            Element::column(vec![search, filters])
                .gap(6.0)
                .padding(10.0),
            model_list(state, id, width),
        ],
    ));
    if let Some(error) = state
        .catalog
        .as_ref()
        .and_then(|c| c.provider_status.get(id))
        .and_then(|s| s.last_error.as_ref())
    {
        sections.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    if is_connected && state.provider_usage.contains_key(id) {
        if let Some(card) = crate::provider_usage_ui::provider_card(state, id, width) {
            sections.push(card);
        }
    }
    if is_connected && provider_connections::supported(id) {
        sections.push(settings_parts::settings_group(
            "Cuenta",
            vec![
                ActionRow::new("Administrar cuenta", control_palette())
                    .subtitle("Abre la página del proveedor en tu navegador.")
                    .action(format!("provider_account:{id}"))
                    .trailing(Element::icon(Icon::ArrowRight).foreground(theme::FOREGROUND_SUBTLE))
                    .render(),
                ActionRow::new("Desconectar", control_palette())
                    .subtitle("Quita esta conexión de OpenIDE.")
                    .trailing(
                        outline(format!("provider_disconnect:{id}"), "Desconectar")
                            .width(96.0)
                            .enabled(!state.running)
                            .render(),
                    )
                    .render(),
            ],
        ));
    }
    sections
}

fn show_output_settings(state: &ViewState) -> bool {
    state.selected_backend == "native"
        && provider_connections::supported(&state.native_provider_id)
        && state.native_provider_id != "codex"
        && connected(state, &state.native_provider_id)
        && !state.model.trim().is_empty()
}

pub(super) fn output_settings(state: &ViewState, compact: bool) -> Element {
    let editable = !state.running && state.native_output_limit_load_error.is_none();
    let mut controls = vec![
        subtle(format!("{} / {}", state.native_provider_id, state.model))
            .font_family(FontFamily::Monospace),
        text("Tope de salida por respuesta", theme::FOREGROUND, 12.0),
        subtle(
            "Incluye razonamiento. Se guarda para este proveedor e ID exactos; un tope bajo puede interrumpir respuestas y herramientas.",
        ),
        TextField::new(
            "native_output_limit_tokens",
            &state.native_output_limit_draft,
            "Sin tope propio",
            control_palette(),
        )
        .size(ControlSize::Compact)
        .enabled(editable)
        .render(),
    ];
    if state.native_output_limit_responses {
        controls.push(subtle("Responses · max_output_tokens"));
    } else if state.native_output_limit_messages {
        controls.push(subtle("Messages · max_tokens"));
        controls.push(subtle("Sin tope propio se solicitan hasta 4096 tokens, acotados por el máximo declarado. Es una política de la solicitud, no el límite del modelo."));
    } else {
        controls.push(subtle(
            "Elige el parámetro documentado para esta ruta Chat Completions.",
        ));
        let buttons = ["max_completion_tokens", "max_tokens"]
            .into_iter()
            .map(|parameter| {
                outline(format!("native_output_limit_field:{parameter}"), parameter)
                    .active(state.native_output_limit_field == parameter)
                    .enabled(editable)
                    .render()
            })
            .collect();
        controls.push(if compact {
            Element::column(buttons).gap(8.0)
        } else {
            Element::row(buttons).gap(8.0)
        });
    }
    if let Some(error) = &state.native_output_limit_load_error {
        controls.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    if state.running {
        controls.push(subtle(
            "La ruta y sus límites están bloqueados mientras hay un turno en curso.",
        ));
    }
    let save = Button::new(
        "native_output_limit_save",
        "Guardar tope",
        control_palette(),
    )
    .size(ControlSize::Compact)
    .enabled(
        editable
            && !state.native_output_limit_draft.trim().is_empty()
            && !state.native_output_limit_field.is_empty(),
    )
    .render();
    let reset = outline("native_output_limit_reset", "Usar política automática")
        .enabled(editable)
        .render();
    controls.push(if compact {
        Element::column(vec![save, reset]).gap(8.0)
    } else {
        Element::row(vec![save, reset]).gap(8.0)
    });
    settings_parts::settings_group(
        "Salida de la ruta nativa activa",
        vec![Element::column(controls).gap(10.0).padding(10.0)],
    )
}

pub(super) fn value(label: &str, val: impl Into<String>) -> Element {
    ActionRow::new(label, control_palette()).value(val).render()
}
fn tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "Dato no disponible".into(), |v| format!("{v} tokens"))
}
fn cost(value: Option<f64>) -> String {
    value
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map_or_else(|| "Dato no disponible".into(), |v| format!("US$ {v:.4}"))
}
fn modalities(kinds: &[String]) -> Element {
    if kinds.is_empty() {
        return subtle("Dato no disponible");
    }
    Element::row(
        kinds
            .iter()
            .map(|kind| {
                let (icon, label) = match kind.as_str() {
                    "text" => (Icon::Text, "Texto"),
                    "image" => (Icon::Image, "Imagen"),
                    "audio" => (Icon::Audio, "Audio"),
                    "video" => (Icon::Video, "Video"),
                    "file" | "pdf" => (Icon::File, "Archivo"),
                    _ => (Icon::File, kind.as_str()),
                };
                Element::row(vec![
                    Element::icon(icon).foreground(theme::FOREGROUND_SUBTLE),
                    subtle(label),
                ])
                .gap(5.0)
            })
            .collect(),
    )
    .gap(10.0)
}
fn capability(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "Sí",
        Some(false) => "No",
        None => "Dato no disponible",
    }
}
pub(super) fn details_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings || state.settings_page != super::SettingsPage::Providers {
        return base;
    }
    let id = state.catalog_browse_provider.as_str();
    let Some(model) = state
        .catalog_expanded_model
        .as_deref()
        .and_then(|m| state.catalog.as_ref()?.model(id, m))
    else {
        return base;
    };
    let metadata = state
        .composer_routes
        .iter()
        .find(|route| {
            route.backend == "native" && route.provider_id == id && route.model_id == model.id
        })
        .and_then(|route| state.composer_model_metadata.get(&route.route_id));
    let catalog_input = model
        .modalities
        .as_ref()
        .map(|m| m.input.as_slice())
        .unwrap_or_default();
    let catalog_output = model
        .modalities
        .as_ref()
        .map(|m| m.output.as_slice())
        .unwrap_or_default();
    let input = metadata.map_or(catalog_input, |value| value.input_modalities.as_slice());
    let output = metadata.map_or(catalog_output, |value| value.output_modalities.as_slice());
    let tools = metadata.map_or(model.tool_call_declared, |value| value.tool_call);
    let images = metadata.map_or_else(
        || model.declared_capabilities.get("image_input").copied(),
        |value| value.image_input,
    );
    let thinking = metadata.map_or_else(
        || {
            model
                .declared_capabilities
                .get("thinking")
                .copied()
                .or_else(|| (!model.capabilities_declared && model.reasoning).then_some(true))
        },
        |value| value.reasoning,
    );
    let structured = metadata.map_or_else(
        || {
            model
                .declared_capabilities
                .get("structured_outputs")
                .copied()
                .or_else(|| {
                    (!model.capabilities_declared && model.structured_output).then_some(true)
                })
        },
        |value| value.structured_output,
    );
    let context = metadata.map_or_else(
        || model.limit.as_ref().and_then(|limit| limit.context),
        |value| value.context_tokens,
    );
    let max_input = metadata.map_or_else(
        || model.limit.as_ref().and_then(|limit| limit.input),
        |value| value.max_input_tokens,
    );
    let max_output = metadata.map_or_else(
        || model.limit.as_ref().and_then(|limit| limit.output),
        |value| value.max_output_tokens,
    );
    let mut groups = vec![
        settings_parts::settings_group(
            "Capacidades",
            vec![
                ActionRow::new("Entradas", control_palette())
                    .trailing(modalities(input))
                    .render(),
                ActionRow::new("Salidas", control_palette())
                    .trailing(modalities(output))
                    .render(),
                value("Herramientas", capability(tools)),
                value("Imágenes", capability(images)),
                value("Thinking", capability(thinking)),
                value("Salida estructurada", capability(structured)),
            ],
        ),
        settings_parts::settings_group(
            "Límites",
            vec![
                value("Contexto", tokens(context)),
                value("Entrada máxima", tokens(max_input)),
                value("Salida máxima", tokens(max_output)),
            ],
        ),
    ];
    if id == "codex" {
        groups.push(settings_parts::settings_group(
            "Uso",
            vec![ActionRow::new("Plan de ChatGPT", control_palette())
                .subtitle("Consulta el consumo y los límites de tu cuenta.")
                .trailing(outline("provider_usage", "Ver uso").render())
                .render()],
        ));
    } else {
        groups.push(settings_parts::settings_group(
            "Precio por millón de tokens · USD",
            vec![
                value("Entrada", cost(model.cost.as_ref().and_then(|c| c.input))),
                value("Salida", cost(model.cost.as_ref().and_then(|c| c.output))),
                value(
                    "Lectura de caché",
                    cost(model.cost.as_ref().and_then(|c| c.cache_read)),
                ),
            ],
        ));
    }
    groups.push(subtle(
        "Datos disponibles para este ID exacto. Un modelo listado no garantiza acceso a generación.",
    ));
    let selected = state.native_provider_id == id && state.model == model.id;
    let use_model = Button::new(
        "provider_details_select",
        if selected { "En uso" } else { "Usar modelo" },
        control_palette(),
    )
    .size(ControlSize::Compact)
    .enabled(!state.running && !selected && model_usable(state, id, model))
    .render()
    .dispatch_as(format!("catalog_select:{id}/{}", model.id));
    Modal::new(
        base,
        true,
        &model.name,
        &model.id,
        "catalog_model_details_dismiss",
        theme::component_palette(),
    )
    .width((viewport_width - 32.0).clamp(280.0, 640.0))
    .height(600.0)
    .body(Element::column(groups).gap(12.0))
    .footer(
        Element::row(vec![
            Element::spacer().flex(1.0),
            outline("catalog_model_details_dismiss", "Cerrar").render(),
            use_model,
        ])
        .gap(8.0),
    )
    .initial_focus("catalog_model_details_dismiss")
    .return_focus(format!("catalog_model_details:{}", model.id))
    .backdrop_dismiss(true)
    .render()
}
pub(super) fn connect_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let Some(connect) = &state.provider_connect else {
        return base;
    };
    let id = connect.provider_id.as_str();
    let oauth = matches!(id, "codex" | "openrouter");
    let entry = matches!(connect.phase, ConnectPhase::Entry | ConnectPhase::Error);
    let welcome = connect.phase == ConnectPhase::Welcome;
    let description = if welcome {
        "Estás usando tu plan de ChatGPT."
    } else if oauth {
        "Continúa en el navegador para autorizar tu cuenta."
    } else {
        "Conecta la clave de API de tu cuenta."
    };
    let mut body = Vec::new();
    if let Some(error) = &connect.error {
        body.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            true,
        ));
    }
    if welcome {
        body.push(subtle(
            "OpenIDE usa el plan y los límites de tu cuenta de ChatGPT para estos modelos.",
        ));
        body.push(outline("provider_usage", "Ver uso del plan").render());
    } else if entry {
        if id == "opencode" {
            body.push(settings_parts::settings_group(
                "Servicio",
                [
                    ("zen", "Zen", "Paga por el uso de cada modelo"),
                    ("go", "Go", "Modelos incluidos en tu suscripción"),
                ]
                .into_iter()
                .map(|(service, label, subtitle)| {
                    let row = ActionRow::new(label, control_palette())
                        .subtitle(subtitle)
                        .action(format!("provider_service:{service}"));
                    if connect.service == service {
                        row.trailing(Element::icon(Icon::Check).foreground(theme::FOREGROUND))
                            .render()
                    } else {
                        row.render()
                    }
                })
                .collect(),
            ));
        }
        if !oauth {
            if id == "zai" {
                body.push(subtle("Usa una clave de la API general de Z.ai. Los modelos se muestran según su documentación; el acceso se comprueba al enviar una solicitud."));
            } else if id == "minimax" {
                body.push(subtle("Usa la clave de tu cuenta de MiniMax. OpenIDE consulta sus modelos disponibles antes de guardar la conexión."));
            }
            body.push(subtle("Clave de API"));
            body.push(
                TextField::new("provider_api_key", "", "Pega tu clave…", control_palette())
                    .size(ControlSize::Compact)
                    .password(true)
                    .render(),
            );
            body.push(
                Button::new(
                    format!("provider_account:{id}"),
                    "Obtener clave de API",
                    control_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            );
            body.push(subtle("La clave se guarda de forma privada en este equipo. El acceso a generación se comprueba al usar un modelo."));
        } else {
            body.push(subtle(if id == "codex" { "Selecciona tu cuenta de ChatGPT y permite el acceso a OpenIDE." }
            else { "Autoriza una clave de OpenRouter para OpenIDE. Puedes administrarla luego desde tu cuenta." }));
        }
    } else {
        body.push(
            Element::row(vec![
                Spinner::new("provider_connect_progress", theme::FOREGROUND_SUBTLE)
                    .size(16.0)
                    .render(),
                subtle(match connect.phase {
                    ConnectPhase::Browser => "Esperando tu autorización…",
                    ConnectPhase::Checking if id == "zai" => "Guardando la clave…",
                    ConnectPhase::Checking => "Consultando tu cuenta y sus modelos…",
                    _ => "Preparando la conexión…",
                }),
            ])
            .gap(10.0)
            .padding(10.0),
        );
        if connect.phase == ConnectPhase::Browser {
            body.push(outline("provider_browser", "Abrir navegador").render());
        }
    }
    let mut footer = vec![
        Element::spacer().flex(1.0),
        Button::new(
            "provider_connect_cancel",
            if welcome { "Entendido" } else { "Cancelar" },
            control_palette(),
        )
        .size(ControlSize::Compact)
        .variant(if welcome {
            ButtonVariant::Solid
        } else {
            ButtonVariant::Outline
        })
        .render(),
    ];
    if entry {
        footer.push(
            Button::new(
                "provider_connect_start",
                if id == "codex" {
                    "Continuar con ChatGPT"
                } else if oauth {
                    "Continuar con OpenRouter"
                } else {
                    "Conectar"
                },
                control_palette(),
            )
            .size(ControlSize::Compact)
            .enabled(oauth || connect.key_entered)
            .render(),
        );
    }
    let focus = if entry && !oauth {
        "provider_api_key"
    } else if entry {
        "provider_connect_start"
    } else {
        "provider_connect_cancel"
    };
    Modal::new(
        base,
        true,
        if welcome {
            "Cuenta conectada".into()
        } else {
            format!("Conectar {}", provider_connections::name(id))
        },
        description,
        "provider_connect_cancel",
        theme::component_palette(),
    )
    .width((viewport_width - 32.0).clamp(280.0, 540.0))
    .height(if id == "opencode" && entry {
        500.0
    } else if matches!(id, "minimax" | "zai") && entry {
        400.0
    } else {
        340.0
    })
    .body(Element::column(body).gap(10.0))
    .footer(Element::row(footer).gap(8.0))
    .initial_focus(focus)
    .return_focus(format!("provider_connect:{id}"))
    .backdrop_dismiss(false)
    .render()
}

#[cfg(test)]
#[path = "provider_controls_visual_tests.rs"]
mod controls_visual_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model_catalog::CatalogSnapshot, native_provider::NativeWireApi};
    use openide_ui::{DrawCommand, Frame, Key, Point, Size, UiAction, UiEvent, UiTree};
    use serde_json::json;

    fn ordered_codex_fixture() -> ViewState {
        let mut state = visible_fixture();
        state.native_provider_id = "codex".into();
        state.catalog_browse_provider = "codex".into();
        state.model = "model-beta".into();
        state.catalog_models_filter = CatalogModelsFilter::All;
        let mut catalog = crate::model_catalog::reviewed_seed_snapshot();
        let provider = catalog.providers.get_mut("codex").unwrap();
        for (index, (id, name)) in [
            ("model-zulu", "Modelo Zulu"),
            ("model-alpha", "Modelo Alpha"),
            ("model-middle", "Modelo Middle"),
            ("model-beta", "Modelo Beta"),
        ]
        .into_iter()
        .enumerate()
        {
            provider.models.insert(
                id.into(),
                serde_json::from_value(json!({
                    "id":id,"name":name,"listing_index":index,
                }))
                .unwrap(),
            );
        }
        let status = catalog.provider_status.get_mut("codex").unwrap();
        status.source = crate::model_catalog::CatalogSource::OfficialApi;
        status.verified_at_ms = 1_800_000_000_000;
        status.attempted_at_ms = status.verified_at_ms;
        catalog.fetched_at_ms = status.verified_at_ms;
        state.catalog = Some(Arc::new(catalog));
        state.provider_connections.insert(
            "codex".into(),
            provider_connections::ConnectionSummary {
                configured: true,
                oauth: true,
                ..Default::default()
            },
        );
        state
    }

    #[test]
    fn codex_settings_preserve_server_order_and_reveal_the_actual_selected_row() {
        let mut state = ordered_codex_fixture();
        let expected = ["model-zulu", "model-alpha", "model-middle", "model-beta"];
        assert_eq!(
            model_rows(&state, "codex", "")
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(model_row_index(&state, "codex", "model-beta"), Some(3));
        for width in [360.0, 900.0] {
            let mut tree = list_tree(model_list(&state, "codex", width));
            let frame = tree.layout(Size::new(width, 600.0));
            let rows = expected
                .iter()
                .map(|id| {
                    frame
                        .regions
                        .iter()
                        .find(|region| region.id == format!("catalog_model_details:{id}"))
                        .unwrap()
                        .rect
                })
                .collect::<Vec<_>>();
            assert!(rows.windows(2).all(|rows| rows[0].y < rows[1].y));
            assert!(
                rows.iter()
                    .all(|rect| rect.x >= 0.0 && rect.x + rect.width <= width + 0.1)
            );
        }
        state.catalog_model_query = "MODEL-BETA".into();
        assert_eq!(model_row_index(&state, "codex", "model-beta"), Some(0));
        assert_eq!(model_row_index(&state, "codex", "model-zulu"), None);
    }

    #[test]
    fn model_source_distinguishes_verified_saved_failed_and_unverified_without_claiming_inference()
    {
        let mut state = ordered_codex_fixture();
        let verified = catalog_status_lines(&state, "codex");
        assert_eq!(verified[0], "Catálogo de tu cuenta");
        assert!(verified[1].starts_with("Actualizado · "));
        assert_eq!(verified.len(), 2);
        let catalog = Arc::make_mut(state.catalog.as_mut().unwrap());
        let status = catalog.provider_status.get_mut("codex").unwrap();
        status.last_error = Some("HTTP 503".into());
        status.attempted_at_ms += 60_000;
        let failed = catalog_status_lines(&state, "codex");
        assert!(failed[1].starts_with("Última actualización · "));
        assert!(failed[2].starts_with("No se pudo actualizar · "));
        assert_ne!(failed[1].split('·').nth(1), failed[2].split('·').nth(1));
        for width in [360.0, 900.0] {
            let mut tree = list_tree(catalog_status(&state, "codex"));
            let frame = tree.layout(Size::new(width, 350.0));
            let rendered = frame_text(&frame)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(rendered.contains("Última actualización"), "{rendered}");
            assert!(rendered.contains("No se pudo actualizar"), "{rendered}");
            assert!(
                frame
                    .regions
                    .iter()
                    .all(|region| region.rect.x + region.rect.width <= width + 0.1)
            );
        }
        Arc::make_mut(state.catalog.as_mut().unwrap()).clear_account_models("codex");
        let cleared = catalog_status_lines(&state, "codex");
        assert!(cleared.iter().any(|line| line == "Catálogo sin verificar"));
        assert!(!cleared.iter().any(|line| line.starts_with("Actualizado")));
    }

    #[test]
    fn refreshing_models_uses_an_animated_spinner_and_restores_the_action() {
        let mut state = visible_fixture();
        let viewport = Size::new(400.0, 40.0);
        let start = std::time::Instant::now();
        let root =
            |state: &ViewState| Element::row(vec![refresh(state), Element::spacer().flex(1.0)]);
        let mut tree = UiTree::new(root(&state));
        let idle = tree.layout_at(viewport, start);
        let idle_rect = idle
            .regions
            .iter()
            .find(|region| region.id == "catalog_refresh")
            .unwrap()
            .rect;
        state.catalog_refreshing = true;
        tree.set_root(root(&state));
        let loading = tree.layout_at(viewport, start);
        assert!(frame_text(&loading).contains("Actualizar modelos"));
        assert!(!frame_text(&loading).contains('…'));
        assert!(
            !loading
                .regions
                .iter()
                .any(|region| region.id == "catalog_refresh")
        );
        let angle = |frame: &Frame| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Spinner {
                        angle,
                        variant: openide_ui::SpinnerVariant::Circle,
                        ..
                    } => Some(*angle),
                    _ => None,
                })
                .expect("model refresh paints the standard loading spinner")
        };
        assert!(tree.animation_repaint_after().is_some());
        let next = tree.layout_at(viewport, start + std::time::Duration::from_millis(200));
        assert_ne!(angle(&loading), angle(&next));
        state.catalog_refreshing = false;
        tree.set_root(root(&state));
        let restored = tree.layout_at(viewport, start + std::time::Duration::from_secs(1));
        assert_eq!(
            restored
                .regions
                .iter()
                .find(|region| region.id == "catalog_refresh")
                .unwrap()
                .rect,
            idle_rect
        );
        assert_eq!(tree.animation_repaint_after(), None);
    }

    // Frozen pre-optimization implementation: the equivalence tests compare
    // the full draw commands, hit regions and scroll state against this path.
    fn legacy_current_model_route<'a>(
        state: &'a ViewState,
        provider: &str,
        model: &str,
    ) -> Option<&'a crate::composer_selection::ModelRouteOption> {
        state.composer_routes.iter().find(|route| {
            route.backend == "native"
                && route.provider_id == provider
                && route.model_id == model
                && route.provider_listed == Some(true)
        })
    }
    fn legacy_model_usable(state: &ViewState, id: &str, model: &ModelInfo) -> bool {
        let supported = legacy_current_model_route(state, id, &model.id).map_or_else(
            || provider_settings::catalog_model_usable(id, model),
            |route| {
                !model.id.is_empty()
                    && model.status.as_deref() != Some("deprecated")
                    && route.execution != crate::composer_selection::ExecutionSupport::Unsupported
            },
        );
        provider_connections::supported(id)
            && connected(state, id)
            && supported
            && !provider_settings::catalog_route_blocked(&state.composer_routes, id, &model.id)
            && state.native_provider_definitions.contains_key(id)
            && catalog_builtin_adapter(id).is_none_or(|mut adapter| {
                if id == "opencode"
                    && state
                        .provider_connections
                        .get(id)
                        .is_some_and(|c| c.service == "go")
                {
                    adapter.use_opencode_go();
                }
                adapter.for_model(&model.id).is_ok()
            })
    }
    fn legacy_model_matches(state: &ViewState, id: &str, model: &ModelInfo, query: &str) -> bool {
        if !provider_settings::catalog_model_matches(id, model, query, CatalogModelsFilter::All) {
            return false;
        }
        if state.catalog_models_filter != CatalogModelsFilter::AgentReady {
            return true;
        }
        legacy_current_model_route(state, id, &model.id).map_or_else(
            || provider_settings::catalog_model_agent_ready_for_wire(id, state.native_provider_definitions.get(id).map(|definition| definition.wire_api), model),
            |route| model.status.as_deref() != Some("deprecated") && matches!(route.execution, crate::composer_selection::ExecutionSupport::Verified | crate::composer_selection::ExecutionSupport::ProviderAdvertisedUnverified))
    }

    fn legacy_model_list(state: &ViewState, id: &str, width: f32) -> Element {
        let query = state.catalog_model_query.trim().to_lowercase();
        let mut models = state
            .catalog
            .as_ref()
            .and_then(|c| c.providers.get(id))
            .map(|p| {
                p.models
                    .values()
                    .filter(|m| legacy_model_matches(state, id, m, &query))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        models.sort_by_key(|model| !(state.native_provider_id == id && state.model == model.id));
        if models.is_empty() {
            return subtle(if !connected(state, id) {
                "Conecta este proveedor para consultar los modelos de tu cuenta."
            } else if state.catalog_refreshing {
                "Consultando modelos…"
            } else if query.is_empty() {
                "No hay modelos disponibles. Actualiza el listado para volver a consultar."
            } else {
                "No hay modelos que coincidan con la búsqueda."
            })
            .padding(10.0);
        }
        let available = models
            .iter()
            .map(|model| legacy_model_usable(state, id, model))
            .collect::<Vec<_>>();
        let models = Arc::new(models);
        let active_provider = state.native_provider_id.clone();
        let active_model = state.model.clone();
        let provider = id.to_owned();
        let running = state.running;
        let row_height = if width < 440.0 { 88.0 } else { 56.0 };
        let height = (models.len() as f32 * row_height).min(392.0);
        ScrollArea::uniform_list(
            format!("catalog_models:{id}"),
            models.len(),
            row_height,
            move |index| {
                let m = &models[index];
                let selected = active_provider == provider && active_model == m.id;
                let name = Element::column(vec![
                    Element::truncated_text(&m.name)
                        .font_family(FontFamily::Sans)
                        .font_size(12.0)
                        .foreground(theme::FOREGROUND),
                    Element::truncated_text(&m.id)
                        .font_family(FontFamily::Monospace)
                        .font_size(11.0)
                        .foreground(theme::FOREGROUND_SUBTLE),
                ])
                .gap(4.0)
                .flex(1.0);
                let info = Button::new(
                    format!("catalog_model_details:{}", m.id),
                    "",
                    control_palette(),
                )
                .icon(Icon::InfoCircle)
                .tooltip("Detalles del modelo")
                .size(ControlSize::Compact)
                .variant(ButtonVariant::Ghost)
                .render();
                let select = if selected {
                    Element::row(vec![
                        Element::spacer().flex(1.0),
                        Element::truncated_text("En uso")
                            .font_family(FontFamily::Sans)
                            .font_size(12.0)
                            .foreground(theme::SETTINGS_FOREGROUND_SUBTLE),
                        Element::spacer().flex(1.0),
                    ])
                    .size(Some(64.0), Some(ControlSize::Compact.height()))
                } else {
                    outline(format!("catalog_select:{provider}/{}", m.id), "Usar")
                        .width(64.0)
                        .enabled(available[index] && !running)
                        .render()
                };
                let controls = Element::row(vec![info, select]).gap(6.0);
                let content = if width < 440.0 {
                    Element::column(vec![name, controls]).gap(6.0)
                } else {
                    Element::row(vec![
                        name,
                        Element::column(vec![
                            Element::spacer().flex(1.0),
                            controls,
                            Element::spacer().flex(1.0),
                        ]),
                    ])
                    .gap(10.0)
                };
                content
                    .padding(10.0)
                    .size(None, Some(row_height))
                    .border(theme::BORDER, 0.5)
            },
        )
        .render()
        .size(None, Some(height))
    }

    const PROVIDER: &str = "registered-messages-route-arbitrary";
    const VISIBLE_PROVIDER: &str = "nvidia";
    const MODEL: &str = "opaque-exact-model-ñ";

    fn large_provider_fixture(count: usize, mixed: bool) -> ViewState {
        use crate::composer_selection::{
            CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource,
        };
        let mut state = visible_fixture();
        state.native_provider_id = "openrouter".into();
        state.catalog_browse_provider = "openrouter".into();
        state.model = format!("fixture/model-{:04}", count.saturating_sub(1));
        state.composer_routes.clear();
        let catalog = Arc::make_mut(state.catalog.as_mut().unwrap());
        let mut provider = catalog.providers.remove(VISIBLE_PROVIDER).unwrap();
        let template = provider.models.remove(MODEL).unwrap();
        provider.id = "openrouter".into();
        provider.name = "OpenRouter offline fixture".into();
        for index in 0..count {
            let mut model = template.clone();
            model.id = format!("fixture/model-{index:04}");
            model.name = format!("Modelo {index:04} ñ");
            model.description = Some(format!("Descripción del modelo {index:04}"));
            model.family = Some("Família de prueba".into());
            model.extra.insert(
                "provider_fixture".into(),
                json!({"release":"offline","notes":["Synthetic metadata", "No account or request"]}),
            );
            if mixed {
                match index % 7 {
                    0 => model.status = Some("deprecated".into()),
                    1 => model.tool_call_declared = Some(false),
                    2 => model.tool_call_declared = None,
                    3 => {
                        model.modalities = Some(crate::model_catalog::ModelModalities {
                            input: vec!["image".into()],
                            output: vec!["image".into()],
                            ..Default::default()
                        });
                    }
                    _ => {}
                }
            }
            let mut route = ModelRouteOption {
                route_id: format!("exact-fixture-{index:04}"),
                backend: "native".into(),
                provider_id: "openrouter".into(),
                provider_name: provider.name.clone(),
                model_id: model.id.clone(),
                model_name: model.name.clone(),
                credential: CredentialAvailability::EnvPresentUnverified,
                execution: ExecutionSupport::ProviderAdvertisedUnverified,
                source: RouteSource::ProviderDiscovery,
                catalog_present: true,
                provider_listed: Some(true),
                effort_options: Vec::new(),
            };
            if mixed {
                match index % 5 {
                    0 => route.execution = ExecutionSupport::Unsupported,
                    1 => route.execution = ExecutionSupport::ProviderListedUnverified,
                    2 => route.provider_listed = None,
                    3 => route.provider_listed = Some(false),
                    _ => {}
                }
            }
            state.composer_routes.push(route.clone());
            if mixed && index % 4 == 0 {
                route.route_id.push_str("-duplicate-positive");
                route.execution = ExecutionSupport::Verified;
                route.provider_listed = Some(true);
                state.composer_routes.push(route.clone());
            }
            if mixed && index % 6 == 0 {
                route.route_id.push_str("-other-provider-denial");
                route.execution = ExecutionSupport::Unsupported;
                route.provider_id = "nvidia".into();
                state.composer_routes.push(route.clone());
                route.provider_id = "openrouter".into();
                route.backend = "codex".into();
                state.composer_routes.push(route);
            }
            // Keys deliberately differ from exact model IDs. Borrowed rows
            // must resolve the original map entry instead of guessing a key.
            provider.models.insert(format!("entry-{index:04}"), model);
        }
        catalog.providers.insert("openrouter".into(), provider);
        state.provider_connections.insert(
            "openrouter".into(),
            provider_connections::ConnectionSummary {
                configured: true,
                from_environment: true,
                ..Default::default()
            },
        );
        state
    }

    fn list_tree(element: Element) -> UiTree {
        let mut tree = UiTree::new(element);
        tree.set_reduced_motion(true);
        tree
    }

    fn assert_legacy_rows(state: &ViewState, provider: &str) {
        let query = state.catalog_model_query.trim().to_lowercase();
        let rows = model_rows(state, provider, &query);
        let models = &state.catalog.as_ref().unwrap().providers[provider].models;
        let mut legacy = models
            .iter()
            .filter(|(_, model)| legacy_model_matches(state, provider, model, &query))
            .collect::<Vec<_>>();
        legacy.sort_by_key(|(_, model)| {
            !(state.native_provider_id == provider && state.model == model.id)
        });
        assert_eq!(rows.len(), legacy.len());
        for ((key, usable), (legacy_key, model)) in rows.iter().zip(legacy) {
            assert_eq!(key, legacy_key);
            assert_eq!(*usable, legacy_model_usable(state, provider, model));
        }
    }

    #[test]
    fn prepared_model_rows_preserve_order_queries_filters_and_account_authority() {
        for filter in [CatalogModelsFilter::All, CatalogModelsFilter::AgentReady] {
            for query in ["", " MODELO 002 ", "DEsCrIpCiÓN", "FamÍlia", "no-match"] {
                for connection in 0..4 {
                    let mut state = large_provider_fixture(36, true);
                    state.catalog_models_filter = filter;
                    state.catalog_model_query = query.into();
                    match connection {
                        1 => state.provider_connections.clear(),
                        2 => {
                            state
                                .provider_connections
                                .get_mut("openrouter")
                                .unwrap()
                                .needs_sign_in = true
                        }
                        3 => {
                            assert!(
                                state
                                    .native_provider_definitions
                                    .remove("openrouter")
                                    .is_some()
                            );
                        }
                        _ => {}
                    }
                    assert_legacy_rows(&state, "openrouter");
                }
            }
        }
    }

    #[test]
    fn prepared_model_list_draw_and_interaction_frames_equal_previous_rendering() {
        for width in [360.0, 900.0] {
            for filter in [CatalogModelsFilter::All, CatalogModelsFilter::AgentReady] {
                for query in ["", "FamÍlia", "no-match"] {
                    for running in [false, true] {
                        let mut state = large_provider_fixture(24, true);
                        state.catalog_models_filter = filter;
                        state.catalog_model_query = query.into();
                        state.running = running;
                        let mut current = list_tree(model_list(&state, "openrouter", width));
                        let mut legacy = list_tree(legacy_model_list(&state, "openrouter", width));
                        for _ in 0..3 {
                            let current_frame = current.layout(Size::new(width, 600.0));
                            let legacy_frame = legacy.layout(Size::new(width, 600.0));
                            assert_eq!(
                                current_frame, legacy_frame,
                                "width={width} filter={filter:?} query={query} running={running}"
                            );
                            let point = Point::new(width / 2.0, 240.0);
                            let event = UiEvent::Scroll {
                                position: point,
                                delta_y: -280.0,
                            };
                            assert_eq!(
                                current.handle_event(event.clone()),
                                legacy.handle_event(event)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn large_model_list_keeps_snapshot_and_virtualized_end_rows() {
        for width in [360.0, 900.0] {
            let mut state = large_provider_fixture(635, false);
            let mut current = list_tree(model_list(&state, "openrouter", width));
            let mut legacy = list_tree(legacy_model_list(&state, "openrouter", width));
            let initial = current.layout(Size::new(width, 600.0));
            assert_eq!(initial, legacy.layout(Size::new(width, 600.0)));
            assert!(
                initial.regions.len() < 40,
                "only viewport rows are materialized"
            );
            assert!(
                frame_text(&initial).contains("Modelo 0634"),
                "selected exact model is first"
            );
            assert!(Arc::strong_count(state.catalog.as_ref().unwrap()) > 1);
            // Replacing the current discovery snapshot cannot invalidate the
            // closure of a frame already prepared from the previous snapshot.
            Arc::make_mut(state.catalog.as_mut().unwrap())
                .providers
                .clear();
            for _ in 0..50 {
                let event = UiEvent::Scroll {
                    position: Point::new(width / 2.0, 240.0),
                    delta_y: -1800.0,
                };
                current.handle_event(event.clone());
                legacy.handle_event(event);
                assert_eq!(
                    current.layout(Size::new(width, 600.0)),
                    legacy.layout(Size::new(width, 600.0))
                );
            }
            let last = current.layout(Size::new(width, 600.0));
            assert!(
                last.regions
                    .iter()
                    .any(|region| region.id == "catalog_select:openrouter/fixture/model-0633")
            );
        }
    }

    #[test]
    fn prepared_model_list_preserves_details_tooltip_hover_focus_and_activation() {
        use std::time::{Duration, Instant};
        for width in [360.0, 900.0] {
            let state = large_provider_fixture(24, true);
            let mut current = list_tree(model_list(&state, "openrouter", width));
            let mut legacy = list_tree(legacy_model_list(&state, "openrouter", width));
            let frame = current.layout(Size::new(width, 600.0));
            assert_eq!(frame, legacy.layout(Size::new(width, 600.0)));
            let region = frame
                .regions
                .iter()
                .find(|region| region.id.starts_with("catalog_model_details:"))
                .unwrap();
            let point = Point::new(
                region.rect.x + region.rect.width / 2.0,
                region.rect.y + region.rect.height / 2.0,
            );
            assert_eq!(
                current.handle_event(UiEvent::PointerMove(point)),
                legacy.handle_event(UiEvent::PointerMove(point))
            );
            let now = Instant::now() + Duration::from_secs(1);
            let hovered = current.layout_at(Size::new(width, 600.0), now);
            assert_eq!(hovered, legacy.layout_at(Size::new(width, 600.0), now));
            assert!(frame_text(&hovered).contains("Detalles del modelo"));
            assert_eq!(current.focus(&region.id), legacy.focus(&region.id));
            assert_eq!(
                current.layout_at(Size::new(width, 600.0), now),
                legacy.layout_at(Size::new(width, 600.0), now)
            );
            let event = UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            };
            assert_eq!(
                current.handle_event(event.clone()),
                legacy.handle_event(event)
            );
        }
    }

    #[test]
    fn prepared_adapter_preserves_opencode_zen_go_exact_model_protocols() {
        let registry = crate::native_provider::ProviderRegistry::builtin();
        for service in ["zen", "go"] {
            let mut state = visible_fixture();
            state.provider_connections.insert(
                "opencode".into(),
                provider_connections::ConnectionSummary {
                    configured: true,
                    service: service.into(),
                    ..Default::default()
                },
            );
            let mut expected = registry.adapter("opencode").unwrap().clone();
            if service == "go" {
                expected.use_opencode_go();
            }
            let context = CatalogModelContext::new(&state, "opencode");
            assert_eq!(context.adapter.as_ref(), Some(&expected));
            let template =
                state.catalog.as_ref().unwrap().providers[VISIBLE_PROVIDER].models[MODEL].clone();
            for id in expected
                .model_transports
                .keys()
                .chain(std::iter::once(&"not-a-reviewed-model".to_owned()))
            {
                let mut model = template.clone();
                model.id = id.clone();
                assert_eq!(
                    context.usable(&model),
                    legacy_model_usable(&state, "opencode", &model)
                );
                assert_eq!(
                    context.usable(&model),
                    expected.for_model(&model.id).is_ok()
                );
            }
        }
    }

    #[test]
    fn model_list_loads_registry_once_instead_of_once_per_model() {
        let state = large_provider_fixture(635, false);
        CATALOG_REGISTRY_LOADS.with(|loads| loads.set(0));
        let previous = legacy_model_list(&state, "openrouter", 900.0);
        assert_eq!(CATALOG_REGISTRY_LOADS.with(|loads| loads.get()), 635);
        CATALOG_REGISTRY_LOADS.with(|loads| loads.set(0));
        let current = model_list(&state, "openrouter", 900.0);
        assert_eq!(CATALOG_REGISTRY_LOADS.with(|loads| loads.get()), 1);
        assert_eq!(
            list_tree(current).layout(Size::new(900.0, 600.0)),
            list_tree(previous).layout(Size::new(900.0, 600.0))
        );
    }

    #[test]
    #[ignore = "Manual offline timing: 635 synthetic models, no credentials or requests"]
    fn benchmark_provider_model_list_635_models() {
        use std::{hint::black_box, time::Instant};
        let state = large_provider_fixture(635, false);
        let iterations = 20;
        for width in [360.0, 900.0] {
            // Warm the common UI paths before comparing list preparation.
            black_box(
                list_tree(model_list(&state, "openrouter", width)).layout(Size::new(width, 600.0)),
            );
            let measure = |build: fn(&ViewState, &str, f32) -> Element, layout: bool| {
                CATALOG_REGISTRY_LOADS.with(|loads| loads.set(0));
                let start = Instant::now();
                for _ in 0..iterations {
                    let element = black_box(build(&state, "openrouter", width));
                    if layout {
                        black_box(list_tree(element).layout(Size::new(width, 600.0)));
                    }
                }
                (
                    start.elapsed().as_secs_f64() * 1000.0 / f64::from(iterations),
                    CATALOG_REGISTRY_LOADS.with(|loads| loads.get()) / iterations as usize,
                )
            };
            for layout in [false, true] {
                let (before, old_loads) = measure(legacy_model_list, layout);
                let (after, new_loads) = measure(model_list, layout);
                assert_eq!(old_loads, 635);
                assert_eq!(new_loads, 1);
                println!(
                    "provider_benchmark width={width:.0} layout={layout} models=635 routes=635 iterations={iterations} before_ms={before:.3} after_ms={after:.3} speedup={:.2} registry_loads_before={old_loads} registry_loads_after={new_loads} ModelInfo_clones_before=635 ModelInfo_clones_after=0",
                    before / after
                );
            }
        }
    }

    fn fixture(wire_api: NativeWireApi) -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.show_settings = true;
        state.settings_page = super::super::SettingsPage::Providers;
        state.catalog_browse_provider.clear();
        state.native_provider_id = PROVIDER.into();
        state.model = "current-exact-route".into();
        state.native_output_limit_draft = "512".into();
        state.native_output_limit_field = if wire_api == NativeWireApi::Responses {
            "max_output_tokens"
        } else {
            "max_tokens"
        }
        .into();
        state.native_output_limit_responses = wire_api == NativeWireApi::Responses;
        state.native_output_limit_messages = wire_api == NativeWireApi::AnthropicMessages;
        state.native_provider_definitions.clear();
        state.native_provider_definitions.insert(
            PROVIDER.into(),
            provider_settings::ProviderConnectionView {
                id: PROVIDER.into(),
                name: "Registered native route".into(),
                endpoint: "http://127.0.0.1:1/v1".into(),
                key_env: "OPENIDE_REGISTERED_FIXTURE_KEY".into(),
                wire_api,
                key_available: false,
                key_optional: false,
                listing_url: Some("http://127.0.0.1:1/v1/models".into()),
                catalog_anonymous: false,
            },
        );
        state.catalog = Some(Arc::new(serde_json::from_value::<CatalogSnapshot>(json!({
            "version":3,"fetched_at_ms":1,"providers":{PROVIDER:{"id":PROVIDER,"name":"Registered native route",
                "models":{MODEL:{"id":MODEL,"name":"Native exact model","tool_call_declared":true,
                    "provider_limits_declared":true,"limit":{"input":100000,"output":8192}}}}}
        })).unwrap()));
        state
    }

    fn page_tree(state: &ViewState, width: f32) -> UiTree {
        let mut tree = UiTree::new(crate::settings_ui::build_settings_page(
            state,
            super::super::SettingsPage::Providers,
            width,
            true,
        ));
        tree.set_reduced_motion(true);
        tree
    }

    fn output_tree(state: &ViewState, width: f32) -> UiTree {
        // This tests the native policy component for each engine protocol. It
        // does not add that protocol/provider to the four-account navigation.
        let mut tree = UiTree::new(
            ScrollArea::new("test_native_output", output_settings(state, width < 640.0)).render(),
        );
        tree.set_reduced_motion(true);
        tree
    }

    pub(super) fn visible_fixture() -> ViewState {
        // Controlled presentation data, without a credential or network
        // request: this is not acceptance of NVIDIA authentication or models.
        let mut state = fixture(NativeWireApi::ChatCompletions);
        state.native_provider_id = VISIBLE_PROVIDER.into();
        let catalog = Arc::make_mut(state.catalog.as_mut().unwrap());
        let mut provider = catalog.providers.remove(PROVIDER).unwrap();
        provider.id = VISIBLE_PROVIDER.into();
        provider.name = "NVIDIA NIM".into();
        catalog.providers.insert(VISIBLE_PROVIDER.into(), provider);
        state.native_provider_definitions = provider_settings::connection_views_with_registry(
            &crate::native_provider::ProviderRegistry::builtin(),
            &crate::native_provider::NativeProviderSettings::default(),
            |name| name == "NVIDIA_API_KEY",
        );
        state.provider_connections.insert(
            VISIBLE_PROVIDER.into(),
            provider_connections::ConnectionSummary {
                configured: true,
                from_environment: true,
                ..Default::default()
            },
        );
        state
    }

    fn frame_text(frame: &Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn reach(tree: &mut UiTree, width: f32, id: &str) -> (Frame, String) {
        let mut observed = String::new();
        for _ in 0..60 {
            let frame = tree.layout(Size::new(width, 720.0));
            observed.push_str(&frame_text(&frame));
            observed.push('\n');
            if frame.regions.iter().any(|region| region.id == id) {
                return (frame, observed);
            }
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(width * 0.55, 360.0),
                delta_y: -180.0,
            });
        }
        panic!("{id} was not reachable at {width}: {observed}");
    }

    fn click(tree: &mut UiTree, frame: &Frame, id: &str) -> Vec<UiAction> {
        let region = frame.regions.iter().find(|region| region.id == id).unwrap();
        let point = Point::new(
            region.rect.x + region.rect.width / 2.0,
            region.rect.y + region.rect.height / 2.0,
        );
        assert_eq!(frame.hit_test(point), Some(id));
        assert!(tree.handle_event(UiEvent::PointerDown(point)).is_empty());
        tree.handle_event(UiEvent::PointerUp(point))
    }

    fn raster(frame: &Frame, width: u32, height: u32) -> resvg::tiny_skia::Pixmap {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frame.svg");
        crate::settings_ui::visual_qa::write_frame_svg(frame, &path, width as f32, height as f32)
            .unwrap();
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        let parsed =
            resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                .unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).unwrap();
        resvg::render(
            &parsed,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap
    }

    #[test]
    fn independent_corner_background_has_no_raster_seams_or_alpha_overdraw() {
        let mut tree = UiTree::new(
            Element::spacer()
                .size(Some(101.0), Some(51.0))
                .background(openide_ui::Color::rgba(66, 66, 66, 128))
                .corner_radii(openide_ui::CornerRadii::top(12.0)),
        );
        let frame = tree.layout(Size::new(101.0, 51.0));
        let pixmap = raster(&frame, 101, 51);
        // Odd dimensions put potential quadrant boundaries through pixel
        // centers. The native frame now emits one corner-aware paint command,
        // and its exporter paints one path rather than overlapping clip masks.
        for y in 14..38 {
            for x in 14..87 {
                let pixel = pixmap.pixel(x, y).unwrap();
                assert_eq!(pixel.alpha(), 128, "alpha seam at ({x}, {y}): {pixel:?}");
            }
        }
        assert_eq!(pixmap.pixel(0, 0).unwrap().alpha(), 0);
        assert_eq!(pixmap.pixel(0, 50).unwrap().alpha(), 128);
    }

    #[test]
    fn provider_row_hover_press_and_focus_follow_filtered_group_corners() {
        for width in [360.0, 900.0] {
            let mut state = visible_fixture();
            state.provider_connections.clear();
            state.provider_connections.insert(
                "codex".into(),
                provider_connections::ConnectionSummary {
                    configured: true,
                    ..Default::default()
                },
            );
            for (query, expected) in [
                (
                    "",
                    vec![
                        ("openrouter", vec![8.0, 8.0, 0.0, 0.0]),
                        ("opencode", vec![0.0]),
                        ("nvidia", vec![0.0]),
                        ("minimax", vec![0.0]),
                        ("zai", vec![0.0, 0.0, 8.0, 8.0]),
                    ],
                ),
                ("nvidia", vec![("nvidia", vec![8.0])]),
                ("minimax", vec![("minimax", vec![8.0])]),
                ("glm", vec![("zai", vec![8.0])]),
            ] {
                state.catalog_provider_query = query.into();
                for (id, expected_radii) in expected {
                    let action = format!("catalog_browse:{id}");
                    let mut tree = page_tree(&state, width);
                    let (frame, _) = reach(&mut tree, width, &action);
                    let rect = frame
                        .regions
                        .iter()
                        .find(|region| region.id == action)
                        .unwrap()
                        .rect;
                    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                    let background_radii = |frame: &Frame, color| {
                        frame
                            .commands
                            .iter()
                            .flat_map(|command| match command {
                                DrawCommand::FillRect {
                                    rect: painted,
                                    color: ink,
                                    radius,
                                } if painted.contains(point) && *ink == color => vec![*radius],
                                DrawCommand::FillCornerRect {
                                    rect: painted,
                                    color: ink,
                                    radii,
                                } if painted.contains(point) && *ink == color => vec![
                                    radii.top_left,
                                    radii.top_right,
                                    radii.bottom_right,
                                    radii.bottom_left,
                                ],
                                _ => Vec::new(),
                            })
                            .collect::<Vec<_>>()
                    };
                    tree.handle_event(UiEvent::PointerMove(point));
                    let hovered = tree.layout(Size::new(width, 720.0));
                    assert_eq!(hovered.hit_test(point), Some(action.as_str()));
                    assert_eq!(
                        background_radii(&hovered, control_palette().hover),
                        expected_radii
                    );
                    tree.handle_event(UiEvent::PointerDown(point));
                    let pressed = tree.layout(Size::new(width, 720.0));
                    assert_eq!(
                        background_radii(&pressed, control_palette().selected),
                        expected_radii
                    );
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        vec![UiAction::Clicked { id: action.clone() }]
                    );
                    assert!(tree.focus(&action));
                    let focused = tree.layout(Size::new(width, 720.0));
                    let focus_rect = focused
                        .regions
                        .iter()
                        .find(|region| region.id == action)
                        .unwrap()
                        .rect;
                    let focus_point = Point::new(
                        focus_rect.x + focus_rect.width / 2.0,
                        focus_rect.y + focus_rect.height / 2.0,
                    );
                    let focus_radii = focused
                        .commands
                        .iter()
                        .flat_map(|command| match command {
                            DrawCommand::StrokeRect {
                                rect: painted,
                                width: stroke,
                                radius,
                                ..
                            } if painted.contains(focus_point) && *stroke == 2.0 => vec![*radius],
                            DrawCommand::StrokeCornerRect {
                                rect: painted,
                                width: stroke,
                                radii,
                                ..
                            } if painted.contains(focus_point) && *stroke == 2.0 => vec![
                                radii.top_left,
                                radii.top_right,
                                radii.bottom_right,
                                radii.bottom_left,
                            ],
                            _ => Vec::new(),
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(focus_radii, expected_radii);
                    assert_eq!(
                        tree.handle_event(UiEvent::Key {
                            key: Key::Enter,
                            shift: false,
                            ctrl: false
                        }),
                        vec![UiAction::Clicked { id: action }]
                    );
                    if query.is_empty() {
                        if let Some(directory) = std::env::var_os("OPENIDE_PROVIDER_HOVER_QA_DIR") {
                            let directory = std::path::PathBuf::from(directory);
                            std::fs::create_dir_all(&directory).unwrap();
                            let path =
                                directory.join(format!("providers-hover-{width:.0}-{id}.svg"));
                            crate::settings_ui::visual_qa::write_frame_svg(
                                &hovered, &path, width, 720.0,
                            )
                            .unwrap();
                            raster(&hovered, width as u32, 720)
                                .save_png(path.with_extension("png"))
                                .unwrap();
                            eprintln!("{}", path.with_extension("png").display());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn native_output_component_renders_each_exact_protocol_without_expanding_account_navigation() {
        for wire in [
            NativeWireApi::AnthropicMessages,
            NativeWireApi::Responses,
            NativeWireApi::ChatCompletions,
        ] {
            let state = fixture(wire);
            for width in [360.0, 900.0] {
                let mut navigation = page_tree(&state, width);
                navigation.layout(Size::new(width, 720.0));
                assert!(!navigation.focus(&format!("catalog_browse:{PROVIDER}")));
                assert!(!navigation.focus("catalog_browse:custom"));
                let mut tree = output_tree(&state, width);
                let (frame, observed) = reach(&mut tree, width, "native_output_limit_save");
                // Narrow layout may wrap the IDs; each character remains
                // present without an ellipsis or truncated identifier.
                let identifiers = observed.split_whitespace().collect::<String>();
                assert!(identifiers.contains(PROVIDER), "{observed}");
                assert!(identifiers.contains("current-exact-route"), "{observed}");
                assert_eq!(
                    click(&mut tree, &frame, "native_output_limit_save"),
                    vec![UiAction::Clicked {
                        id: "native_output_limit_save".into()
                    }]
                );
                assert!(tree.focus("native_output_limit_reset"));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked {
                        id: "native_output_limit_reset".into()
                    }]
                );
                if wire == NativeWireApi::ChatCompletions {
                    for parameter in ["max_tokens", "max_completion_tokens"] {
                        let id = format!("native_output_limit_field:{parameter}");
                        let (frame, _) = reach(&mut tree, width, &id);
                        assert_eq!(
                            click(&mut tree, &frame, &id),
                            vec![UiAction::Clicked { id }]
                        );
                    }
                } else {
                    assert!(
                        observed.contains(if wire == NativeWireApi::AnthropicMessages {
                            "Messages · max_tokens"
                        } else {
                            "Responses · max_output_tokens"
                        })
                    );
                    for id in [
                        "native_output_limit_field:max_tokens",
                        "native_output_limit_field:max_completion_tokens",
                    ] {
                        assert!(!tree.focus(id));
                        assert!(!frame.regions.iter().any(|region| region.id == id));
                    }
                }
            }
        }
    }

    #[test]
    fn featured_provider_models_and_input_limits_remain_reachable_at_both_widths() {
        for width in [360.0, 900.0] {
            let mut state = visible_fixture();
            let mut overview = page_tree(&state, width);
            let browse = format!("catalog_browse:{VISIBLE_PROVIDER}");
            let (frame, _) = reach(&mut overview, width, &browse);
            assert_eq!(
                click(&mut overview, &frame, &browse),
                vec![UiAction::Clicked { id: browse }]
            );
            state.catalog_browse_provider = VISIBLE_PROVIDER.into();
            let mut tree = page_tree(&state, width);
            let select = format!("catalog_select:{VISIBLE_PROVIDER}/{MODEL}");
            let (frame, observed) = reach(&mut tree, width, &select);
            assert!(observed.contains("Conexión"));
            assert!(observed.contains("NVIDIA NIM"));
            assert_eq!(
                click(&mut tree, &frame, &select),
                vec![UiAction::Clicked { id: select.clone() }]
            );
            assert!(tree.focus(&select));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked { id: select.clone() }]
            );

            state.catalog_expanded_model = Some(MODEL.into());
            let mut modal = UiTree::new(details_modal(&state, Element::text("behind"), width));
            modal.set_reduced_motion(true);
            let mut observed = String::new();
            for _ in 0..12 {
                let frame = modal.layout(Size::new(width, 720.0));
                observed.push_str(&frame_text(&frame));
                observed.push('\n');
                modal.handle_event(UiEvent::Scroll {
                    position: Point::new(width / 2.0, 360.0),
                    delta_y: -140.0,
                });
            }
            assert!(observed.contains("Entrada máxima"), "{observed}");
            assert!(observed.contains("100000 tokens"), "{observed}");
            assert!(observed.contains("8192 tokens"), "{observed}");
            let frame = modal.layout(Size::new(width, 720.0));
            assert_eq!(
                click(&mut modal, &frame, "provider_details_select"),
                vec![UiAction::Clicked { id: select }]
            );
        }
    }

    #[test]
    fn current_route_and_metadata_supersede_cached_denial_and_limits_without_filling_unknowns() {
        use crate::composer_selection::{
            CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource,
        };
        for width in [360.0, 900.0] {
            let mut state = visible_fixture();
            state.catalog_browse_provider = VISIBLE_PROVIDER.into();
            let cached = Arc::make_mut(state.catalog.as_mut().unwrap())
                .providers
                .get_mut(VISIBLE_PROVIDER)
                .unwrap()
                .models
                .get_mut(MODEL)
                .unwrap();
            cached.tool_call_declared = Some(false);
            cached.limit.as_mut().unwrap().context = Some(123456);
            cached.limit.as_mut().unwrap().input = Some(200000);
            let current = ModelRouteOption {
                route_id: "opaque-current-route".into(),
                backend: "native".into(),
                provider_id: VISIBLE_PROVIDER.into(),
                provider_name: "Registered native route".into(),
                model_id: MODEL.into(),
                model_name: "Current exact model".into(),
                credential: CredentialAvailability::MissingCredential,
                execution: ExecutionSupport::ProviderListedUnverified,
                source: RouteSource::ProviderDiscovery,
                catalog_present: true,
                provider_listed: Some(true),
                effort_options: Vec::new(),
            };
            state.composer_routes = vec![current.clone()];
            state.composer_model_metadata.insert(
                current.route_id.clone(),
                crate::model_metadata::ModelMetadataView {
                    max_input_tokens: Some(100000),
                    limits_source: Some(crate::model_metadata::MetadataSource::ProviderAccount),
                    ..Default::default()
                },
            );
            let select = format!("catalog_select:{VISIBLE_PROVIDER}/{MODEL}");
            let mut tree = page_tree(&state, width);
            let (frame, _) = reach(&mut tree, width, &select);
            assert_eq!(
                click(&mut tree, &frame, &select),
                vec![UiAction::Clicked { id: select.clone() }]
            );

            state.catalog_expanded_model = Some(MODEL.into());
            let mut modal = UiTree::new(details_modal(&state, Element::text("behind"), width));
            modal.set_reduced_motion(true);
            let mut observed = String::new();
            for _ in 0..12 {
                let frame = modal.layout(Size::new(width, 720.0));
                observed.push_str(&frame_text(&frame));
                observed.push('\n');
                modal.handle_event(UiEvent::Scroll {
                    position: Point::new(width / 2.0, 360.0),
                    delta_y: -140.0,
                });
            }
            assert!(observed.contains("100000 tokens"), "{observed}");
            for old in ["123456 tokens", "200000 tokens", "8192 tokens"] {
                assert!(!observed.contains(old), "{observed}");
            }
            assert!(
                !observed.split_whitespace().any(|word| word == "No"),
                "{observed}"
            );

            // The tools filter uses a current positive claim, never the old
            // denial. Unknown tools remain outside that narrower filter.
            state.catalog_expanded_model = None;
            state.catalog_models_filter = CatalogModelsFilter::AgentReady;
            let mut unknown = page_tree(&state, width);
            unknown.layout(Size::new(width, 720.0));
            assert!(!unknown.focus(&select));
            state.composer_routes[0].execution = ExecutionSupport::ProviderAdvertisedUnverified;
            let mut advertised = page_tree(&state, width);
            let (frame, _) = reach(&mut advertised, width, &select);
            assert_eq!(
                click(&mut advertised, &frame, &select),
                vec![UiAction::Clicked { id: select.clone() }]
            );
            state.catalog_models_filter = CatalogModelsFilter::All;
            state.composer_routes[0].execution = ExecutionSupport::Unsupported;
            let mut unsupported = page_tree(&state, width);
            unsupported.layout(Size::new(width, 720.0));
            assert!(!unsupported.focus(&select));
        }
    }

    #[test]
    fn native_output_mutations_are_disabled_during_run_or_after_load_error() {
        for width in [360.0, 900.0] {
            for running in [false, true] {
                let mut state = fixture(NativeWireApi::AnthropicMessages);
                state.running = running;
                if !running {
                    state.native_output_limit_load_error =
                        Some("Saved output policy is corrupt; preserve the original bytes".into());
                }
                let mut tree = output_tree(&state, width);
                let mut observed = String::new();
                for _ in 0..20 {
                    let frame = tree.layout(Size::new(width, 720.0));
                    observed.push_str(&frame_text(&frame));
                    observed.push('\n');
                    for id in [
                        "native_output_limit_save",
                        "native_output_limit_reset",
                        "native_output_limit_tokens",
                    ] {
                        assert!(!tree.focus(id));
                    }
                    tree.handle_event(UiEvent::Scroll {
                        position: Point::new(width * 0.55, 360.0),
                        delta_y: -180.0,
                    });
                }
                let normalized = observed.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(
                    normalized.contains(if running {
                        "bloqueados mientras hay un turno"
                    } else {
                        "Saved output policy is corrupt; preserve the original bytes"
                    }),
                    "{observed}"
                );
            }
        }
    }
}
