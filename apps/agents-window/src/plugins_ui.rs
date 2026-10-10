//! Native plugin management. Rendering never reads packages or starts processes.
use crate::{
    plugins::{InstalledPlugin, PluginManifest, PluginPreview},
    settings_parts,
    settings_ui::SettingsPage,
    theme,
    view::ViewState,
};
use openide_ui::{
    AlertDialog, Button, ButtonVariant, Card, Component, ControlSize, Element, FontFamily, Icon, Modal,
    SearchField, Separator, TextField,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct PluginsUiState {
    pub workspace: String,
    pub installed: Vec<InstalledPlugin>,
    pub query: String,
    pub busy: bool,
    pub status: Option<String>,
    pub error: bool,
    pub load_error: bool,
    pub install_open: bool,
    pub source: String,
    pub preview: Option<PluginPreview>,
    pub selected: Option<String>,
    pub pending_remove: Option<InstalledPlugin>,
    pub removal_blocked: bool,
}

impl PluginsUiState {
    pub fn close_dialogs(&mut self) {
        self.install_open = false;
        self.preview = None;
        self.selected = None;
        self.pending_remove = None;
    }
}

fn label(text: impl Into<String>) -> Element {
    Element::text(text)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(theme::FOREGROUND)
}
fn detail(text: impl Into<String>) -> Element {
    label(text)
        .font_size(11.0)
        .foreground(theme::FOREGROUND_SUBTLE)
}
fn metadata(manifest: &PluginManifest) -> String {
    format!("{} · {}", manifest.publisher, manifest.version)
}

pub(crate) fn page(state: &ViewState, compact: bool) -> Vec<Element> {
    let plugins = &state.plugins;
    let ready = !plugins.busy && !plugins.load_error;
    let mut rows = vec![Element::row(vec![
        Button::new(
            "plugins_install_open",
            "Instalar desde carpeta",
            theme::component_palette(),
        )
        .icon(Icon::Add)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .enabled(!plugins.busy)
        .render(),
        Element::spacer().flex(1.0),
        Button::new("plugins_refresh", "Actualizar", theme::component_palette())
            .icon(Icon::Refresh)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!plugins.busy)
            .render(),
    ])
    .gap(6.0)];
    rows.push(detail("Las instalaciones se guardan en este equipo. Activa cada plugin para el proyecto actual; sus herramientas estarán disponibles en los próximos turnos de ejecución."));
    if let Some(message) = &plugins.status {
        rows.push(settings_parts::feedback_banner(
            message,
            if plugins.error {
                settings_parts::FeedbackTone::Error
            } else {
                settings_parts::FeedbackTone::Info
            },
            None,
            compact,
        ));
    }
    if plugins.installed.is_empty() {
        if !plugins.busy && !plugins.load_error {
            rows.push(settings_parts::empty_state_with_icon(Icon::Puzzle, "Todavía no hay plugins instalados", "Selecciona una carpeta de plugin de OpenIDE para revisar su autor, versión y programa antes de instalarla. Las extensiones VSIX y WASM de otros editores usan formatos distintos.", None));
        }
        return rows;
    }
    rows.push(
        SearchField::new(
            "plugins_search",
            &plugins.query,
            "Buscar plugins instalados…",
            theme::component_palette(),
        )
        .size(ControlSize::Compact)
        .render(),
    );
    let query = plugins.query.trim().to_lowercase();
    let mut cards = Vec::new();
    for plugin in &plugins.installed {
        let manifest = &plugin.manifest;
        if !query.is_empty()
            && !format!(
                "{} {} {} {}",
                manifest.name, manifest.id, manifest.description, manifest.publisher
            )
            .to_lowercase()
            .contains(&query)
        {
            continue;
        }
        let mut content = vec![
            Element::row(vec![
                Element::icon(Icon::Puzzle)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(18.0), Some(18.0)),
                Element::column(vec![label(&manifest.name), detail(metadata(manifest))])
                    .gap(3.0)
                    .flex(1.0),
            ])
            .gap(8.0),
            detail(&manifest.description),
        ];
        let actions = Element::row(vec![
            Button::new(
                format!("plugins_details:{}", manifest.id),
                "Detalles",
                theme::component_palette(),
            )
            .icon(Icon::InfoCircle)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!plugins.busy)
            .render(),
            Button::new(
                format!("plugins_toggle:{}", manifest.id),
                if plugin.enabled {
                    "Desactivar"
                } else {
                    "Activar"
                },
                theme::component_palette(),
            )
            .icon(if plugin.enabled {
                Icon::Pause
            } else {
                Icon::Play
            })
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(ready)
            .render(),
        ])
        .gap(6.0);
        let status = detail(if plugin.enabled {
            "Activo en este proyecto"
        } else {
            "Inactivo en este proyecto"
        });
        content.push(if compact {
            Element::column(vec![status, actions]).gap(8.0)
        } else {
            Element::row(vec![status.flex(1.0), actions]).gap(10.0)
        });
        cards.push(
            Card::new(Element::column(content).gap(10.0), theme::card_palette()).render(),
        );
    }
    if cards.is_empty() {
        rows.push(settings_parts::empty_state(
            "Sin coincidencias",
            "Prueba con otro nombre o autor.",
            None,
        ));
    } else {
        rows.push(Element::column(cards).gap(8.0));
    }
    rows
}

fn manifest_fields(manifest: &PluginManifest) -> Vec<Element> {
    let mut fields = vec![
        label(&manifest.name),
        detail(metadata(manifest)),
        detail(&manifest.description),
        detail(format!("Identificador: {}", manifest.id)),
        label("Programa"),
        label(&manifest.program).font_family(FontFamily::Monospace),
    ];
    if let Some(entrypoint) = &manifest.entrypoint {
        fields.push(detail(format!("Entrada: {entrypoint}")));
    }
    if !manifest.args.is_empty() {
        fields.push(detail(format!(
            "Argumentos: {}",
            serde_json::to_string(&manifest.args).unwrap_or_default()
        )));
    }
    if !manifest.credential_env_names.is_empty() {
        fields.push(detail(format!(
            "Variables requeridas: {}",
            manifest.credential_env_names.join(", ")
        )));
    }
    fields
}

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings || state.settings_page != SettingsPage::Plugins {
        return base;
    }
    let plugins = &state.plugins;
    let width = (viewport_width - 24.0).clamp(240.0, 600.0);
    if let Some(plugin) = &plugins.pending_remove {
        return AlertDialog::new(base, true, "Desinstalar plugin", format!("Se quitará «{}» de este equipo y se desactivará en todos los proyectos. La carpeta original se conserva.", plugin.manifest.name), "plugins_remove_confirm", "plugins_remove_cancel", theme::component_palette())
            .confirm_label("Desinstalar").confirm_enabled(!plugins.busy && !plugins.removal_blocked).destructive(theme::DESTRUCTIVE).width(width.min(440.0)).render();
    }
    let (title, description, body, footer, focus, return_focus) = if plugins.install_open {
        let mut fields = vec![
            detail("Carpeta que contiene openide-plugin.json"),
            TextField::new(
                "plugins_source",
                &plugins.source,
                "/ruta/al/plugin",
                theme::component_palette(),
            )
            .size(ControlSize::Compact)
            .render(),
            Element::row(vec![
                Button::new(
                    "plugins_pick_folder",
                    "Elegir carpeta",
                    theme::component_palette(),
                )
                .icon(Icon::Folder)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(!plugins.busy)
                .render(),
                Button::new("plugins_inspect", "Revisar", theme::component_palette())
                    .icon(Icon::Search)
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .enabled(!plugins.busy && !plugins.source.trim().is_empty())
                    .render(),
            ])
            .gap(6.0),
        ];
        if let Some(preview) = &plugins.preview {
            fields.push(Separator::horizontal(theme::component_palette()).render());
            fields.extend(manifest_fields(&preview.manifest));
            fields.push(detail(format!(
                "{} archivos · {} KiB",
                preview.file_count,
                preview.total_bytes.div_ceil(1024)
            )));
            fields.push(
                detail(format!("SHA-256: {}", preview.digest)).font_family(FontFamily::Monospace),
            );
            fields.push(detail("Se instalará inactivo. Al activarlo, este programa podrá ejecutarse en los próximos turnos del agente; las llamadas a sus herramientas conservarán los permisos de MCP."));
        }
        if let Some(message) = &plugins.status {
            fields.push(detail(message).foreground(if plugins.error {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND_SUBTLE
            }));
        }
        let footer = Element::row(vec![
            Element::spacer().flex(1.0),
            Button::new(
                "plugins_dialog_close",
                "Cancelar",
                theme::component_palette(),
            )
            .icon(Icon::Close)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
            Button::new(
                "plugins_install_confirm",
                "Instalar",
                theme::component_palette(),
            )
            .icon(Icon::Add)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!plugins.busy && plugins.preview.is_some())
            .render(),
        ])
        .gap(6.0);
        (
            "Instalar plugin",
            "Revisa el paquete antes de guardarlo en OpenIDE.",
            fields,
            footer,
            "plugins_source",
            "plugins_install_open".to_owned(),
        )
    } else if let Some(plugin) = plugins.selected.as_ref().and_then(|id| {
        plugins
            .installed
            .iter()
            .find(|plugin| &plugin.manifest.id == id)
    }) {
        let mut fields = manifest_fields(&plugin.manifest);
        fields.extend([detail(if plugin.enabled { "Activo en el proyecto actual" } else { "Inactivo en el proyecto actual" }), detail(format!("Copia instalada: {}", plugin.path.display())), detail(format!("SHA-256: {}", plugin.digest)).font_family(FontFamily::Monospace), detail("La versión instalada se conserva hasta que la desinstales. No se descargan ni ejecutan actualizaciones al abrir esta pantalla.")]);
        if plugins.removal_blocked {
            fields.push(detail(
                "Espera a que terminen los agentes para desinstalar plugins.",
            ));
        }
        let footer = Element::row(vec![
            Button::new(
                format!("plugins_remove:{}", plugin.manifest.id),
                "Desinstalar",
                theme::component_palette(),
            )
            .icon(Icon::Trash)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!plugins.busy && !plugins.removal_blocked)
            .render(),
            Element::spacer().flex(1.0),
            Button::new("plugins_dialog_close", "Cerrar", theme::component_palette())
                .icon(Icon::Close)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
        ])
        .gap(6.0);
        (
            "Detalles del plugin",
            "Identidad y ejecución del paquete instalado.",
            fields,
            footer,
            "plugins_dialog_close",
            format!("plugins_details:{}", plugin.manifest.id),
        )
    } else {
        return base;
    };
    Modal::new(
        base,
        true,
        title,
        description,
        "plugins_dialog_close",
        theme::component_palette(),
    )
    .width(width)
    .height(540.0)
    .body(
        Element::column(body)
            .gap(10.0)
            .size(Some((width - 54.0).max(0.0)), None),
    )
    .footer(footer)
    .initial_focus(focus)
    .return_focus(return_focus)
    .render()
}

#[cfg(test)]
#[path = "plugins_ui_tests.rs"]
mod tests;
