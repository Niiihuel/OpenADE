//! The same saved-server inspector is used by Settings and Marketplace.
use super::{outline, page_note, SettingsPage};
use crate::settings_parts::FeedbackTone;
use crate::{
    mcp_presentation::BrandChoice,
    mcp_settings::{McpServerSettings, McpSettingsScope},
    settings_parts, theme,
    view::ViewState,
};
use openide_ui::{
    ButtonGroup, ButtonVariant, Checkbox, Collapsible, Component, ControlSize, DescriptionItem,
    DescriptionList, DropdownOption, Element, Icon, IconButton, ScrollArea, SearchField, Select,
    SelectVariant, SelectorSizing, Switch, Text, TextRole,
};

fn muted(text: impl Into<String>) -> Element {
    Text::new(text, theme::component_palette())
        .role(TextRole::Small)
        .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
        .render()
}

fn actions(buttons: Vec<Element>, compact: bool) -> Element {
    if compact && buttons.len() > 2 {
        let mut buttons = buttons.into_iter();
        let mut rows = Vec::new();
        while let Some(first) = buttons.next() {
            let mut pair = vec![first];
            pair.extend(buttons.next());
            rows.push(
                ButtonGroup::new(pair)
                    .sizing(crate::ui_sizing::ENTITY_ACTIONS)
                    .render(),
            );
        }
        Element::column(rows).gap(8.0)
    } else {
        ButtonGroup::new(buttons)
            .sizing(crate::ui_sizing::ENTITY_ACTIONS)
            .render()
    }
}

fn search(id: &str, value: &str, label: &str, width: f32) -> Element {
    SearchField::new(id, value, label, theme::control_palette())
        .accessibility_label(label)
        .size(ControlSize::Regular)
        .width(width.max(0.0))
        .render()
}

fn activation(state: &ViewState, server: &McpServerSettings) -> Element {
    Switch::new(
        format!("mcp_toggle:{}", server.id),
        server.enabled,
        theme::control_palette(),
    )
    .accessibility_label(format!("Habilitar servidor {}", server.id))
    .size(ControlSize::Compact)
    .enabled(state.mcp_load_error.is_none())
    .render()
}

fn transport(server: &McpServerSettings) -> String {
    match &server.url {
        Some(url) => reqwest::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(|host| format!("HTTP · {host}")))
            .unwrap_or_else(|| "Streamable HTTP".into()),
        None => std::path::Path::new(&server.program)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("stdio · {name}"))
            .unwrap_or_else(|| "Proceso local · stdio".into()),
    }
}

fn technical_value(label: &str, value: impl Into<String>) -> DescriptionItem {
    DescriptionItem::with(
        label,
        Text::new(value, theme::component_palette())
            .role(TextRole::Small)
            .mono()
            .render()
            .flex(1.0),
    )
}

fn facts(items: Vec<DescriptionItem>, compact: bool, width: f32) -> Element {
    let list = DescriptionList::new(items, theme::component_palette())
        .size(ControlSize::Compact)
        .label_width(112.0)
        .dividers(false)
        .width(width.max(0.0));
    if compact {
        list.stacked().render()
    } else {
        list.render()
    }
}

fn status(state: &ViewState, server: &McpServerSettings) -> String {
    use crate::mcp_inspection::McpServerStatus;
    let last = state.mcp_inspection.last.get(&server.id);
    let status = match (server.enabled, last) {
        (false, _) => "Deshabilitado".to_owned(),
        (true, Some(McpServerStatus::Connected { tools })) => {
            format!("Última conexión correcta · {tools} herramientas")
        }
        (true, Some(McpServerStatus::Failed(_))) => "Último intento falló".into(),
        (true, None) => "Habilitado · sin probar".into(),
    };
    if state.mcp_settings_scope == McpSettingsScope::User
        && state.mcp_shadowed_user_ids.contains(&server.id)
    {
        format!("{status} · Reemplazado por este proyecto")
    } else {
        status
    }
}

fn with_hints<T>(
    state: &ViewState,
    server: &McpServerSettings,
    f: impl FnOnce(&crate::brand_icons::BrandHints<'_>) -> T,
) -> T {
    let identity = state
        .mcp_presentation
        .discovery
        .get(&server.id)
        .map(|found| found.identity_hints(server));
    let packages: Vec<&str> = identity
        .as_ref()
        .and_then(|h| h.package_name.as_deref())
        .into_iter()
        .collect();
    let websites: Vec<&str> = identity
        .as_ref()
        .and_then(|h| h.homepage.as_deref())
        .into_iter()
        .collect();
    f(&crate::brand_icons::BrandHints {
        display_name: &server.id,
        package_names: &packages,
        executable: identity.as_ref().and_then(|h| h.executable_name.as_deref()),
        website_urls: &websites,
        endpoint_url: server.url.as_deref(),
    })
}

fn brand(
    state: &ViewState,
    server: &McpServerSettings,
) -> Option<&'static crate::brand_icons::BrandEntry> {
    match state
        .mcp_presentation
        .preferences
        .icons
        .get(&server.id)
        .unwrap_or(&BrandChoice::Auto)
    {
        BrandChoice::None => None,
        BrandChoice::Brand(id) => crate::brand_icons::get(id),
        BrandChoice::Auto => with_hints(state, server, crate::brand_icons::detect),
    }
}

fn identity(state: &ViewState, server: &McpServerSettings) -> Element {
    let icon = brand(state, server)
        .map(|entry| entry.element(!crate::theme_mode::light()))
        .unwrap_or_else(|| {
            Element::icon(SettingsPage::Mcp.icon())
                .foreground(theme::ICON_PRIMARY)
                .size(Some(20.0), Some(20.0))
        });
    Element::row(vec![
        icon,
        Element::column(vec![
            Text::new(&server.id, theme::component_palette())
                .role(TextRole::Label)
                .render(),
            muted(status(state, server)),
        ])
        .gap(4.0)
        .flex(1.0),
    ])
    .gap(12.0)
    .center_content()
}

fn inspect_button(state: &ViewState, server: &McpServerSettings) -> Element {
    let failed = matches!(
        state.mcp_inspection.last.get(&server.id),
        Some(crate::mcp_inspection::McpServerStatus::Failed(_))
    );
    let busy = state.mcp_inspection.is_busy()
        && state
            .mcp_inspection
            .scope
            .as_ref()
            .is_some_and(|scope| scope.server.id == server.id);
    outline(
        format!("mcp_inspect:{}", server.id),
        if failed { "Reintentar" } else { "Inspeccionar" },
    )
    .icon(Icon::Refresh)
    .loading(busy)
    .enabled(
        state.mcp_load_error.is_none()
            && server.enabled
            && !state.running
            && !state.mcp_inspection.is_busy(),
    )
    .render()
}

fn toolbar(state: &ViewState, width: f32) -> Element {
    let scope = Select::new(
        "settings_choice_toggle:mcp_scope",
        state.mcp_settings_scope.label(),
        [McpSettingsScope::User, McpSettingsScope::Project]
            .into_iter()
            .map(|scope| {
                DropdownOption::new(format!("mcp_scope:{}", scope.key()), scope.label())
                    .selected(scope == state.mcp_settings_scope)
            })
            .collect(),
        state.settings_choice_menu_open.as_deref() == Some("mcp_scope"),
        "settings_choice_dismiss",
        theme::control_palette(),
    )
    .variant(SelectVariant::Field)
    .accessibility_label("Ámbito de los servidores MCP")
    .sizing(SelectorSizing::COMPACT)
    .width((width - ControlSize::Compact.height() - 16.0).clamp(0.0, 200.0))
    .menu_width(width.min(280.0))
    .menu_palette(theme::menu_palette())
    .render();
    Element::column(vec![
        muted("Ámbito"),
        Element::row(vec![
            scope,
            Element::spacer().flex(1.0),
            IconButton::new("mcp_refresh", Icon::Refresh, theme::control_palette())
                .accessibility_label("Recargar configuración")
                .tooltip("Recargar configuración")
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
        ])
        .gap(8.0),
        muted(&state.mcp_effective_summary),
    ])
    .gap(8.0)
}

fn server_row(state: &ViewState, server: &McpServerSettings, width: f32) -> Element {
    let mut description = transport(server);
    if server.url.is_none()
        && state
            .mcp_presentation
            .discovery
            .get(&server.id)
            .is_some_and(|found| found.executable.is_none())
    {
        description.push_str(" · Ejecutable no encontrado en PATH");
    }
    let details = outline(format!("mcp_details:{}", server.id), "Herramientas")
        .icon(Icon::ArrowRight)
        .render();
    let body = if width < 520.0 {
        Element::column(vec![
            Element::row(vec![
                identity(state, server).flex(1.0),
                activation(state, server).centered_y(),
            ])
            .gap(12.0),
            Element::row(vec![muted(description).flex(1.0), details.centered_y()]).gap(12.0),
        ])
        .gap(8.0)
    } else {
        Element::row(vec![
            Element::column(vec![identity(state, server), muted(description)])
                .gap(4.0)
                .flex(1.0),
            details.centered_y(),
            activation(state, server).centered_y(),
        ])
        .gap(12.0)
    };
    body.padding(settings_parts::ROW_PADDING)
}

pub(super) fn render(state: &ViewState, compact: bool, width: f32) -> Vec<Element> {
    let mut content = vec![toolbar(state, width)];
    for error in [
        state.mcp_load_error.as_ref(),
        state.mcp_presentation.preferences_error.as_ref(),
        state.mcp_presentation.selection_error.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        content.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    if let Some(server) = state
        .mcp_presentation
        .details
        .as_ref()
        .and_then(|id| state.mcp_servers.iter().find(|server| &server.id == id))
    {
        content.extend(detail(state, server, compact, width));
        return content;
    }
    let editable = state.mcp_load_error.is_none();
    let manage = actions(
        vec![
            outline("mcp_import", "Importar")
                .icon(Icon::ArrowRight)
                .enabled(editable)
                .tooltip("Busca configuraciones locales. Los servidores importados quedan deshabilitados para revisarlos.")
                .render(),
            outline("mcp_new", "Nuevo")
                .icon(Icon::Add)
                .enabled(editable)
                .tooltip("Nuevo servidor")
                .render(),
        ],
        false,
    );
    let saved = muted(format!(
        "{} configurados · {}",
        state.mcp_servers.len(),
        state.mcp_settings_scope.label()
    ));
    content.push(if compact {
        Element::column(vec![saved, manage]).gap(8.0)
    } else {
        Element::row(vec![saved.flex(1.0).centered_y(), manage]).gap(12.0)
    });
    if !state.mcp_servers.is_empty() {
        content.push(search(
            "mcp_search",
            &state.mcp_presentation.search,
            "Buscar servidores",
            width,
        ));
    }
    let mut rows = Vec::new();
    if state.mcp_servers.is_empty() {
        rows.push(page_note(
            "Sin servidores",
            "Agrega un proceso local o un endpoint HTTP para conectar herramientas al agente nativo.",
            SettingsPage::Mcp.icon(),
        ));
    }
    let query = state.mcp_presentation.search.trim().to_lowercase();
    for server in &state.mcp_servers {
        if !query.is_empty()
            && !format!(
                "{} {} {}",
                server.id,
                server.program,
                server.url.as_deref().unwrap_or("")
            )
            .to_lowercase()
            .contains(&query)
        {
            continue;
        }
        rows.push(server_row(state, server, width));
    }
    if rows.is_empty() {
        rows.push(page_note(
            "Sin resultados",
            "Prueba otro nombre o programa.",
            Icon::Search,
        ));
    }
    content.push(settings_parts::settings_group("Servidores MCP", rows));
    content.push(Element::column(vec![
        muted("Los servidores habilitados se conectan al iniciar un turno Build. La inspección consulta metadatos y cierra la conexión."),
        muted("Elegir herramientas no cambia sus permisos. Las llamadas siguen solicitando aprobación; OAuth requiere autorización."),
    ]).gap(8.0));
    content
}

fn detail(
    state: &ViewState,
    server: &McpServerSettings,
    compact: bool,
    width: f32,
) -> Vec<Element> {
    let editable = state.mcp_load_error.is_none();
    let mut result = vec![actions(
        vec![outline("mcp_details_back", "Servidores MCP")
            .icon(Icon::ArrowLeft)
            .render()],
        false,
    )];
    let summary = vec![
        Element::row(vec![
            identity(state, server).flex(1.0),
            activation(state, server).centered_y(),
        ])
        .gap(12.0),
        muted(transport(server)),
        actions(
            vec![
                outline(format!("mcp_select:{}", server.id), "Editar")
                    .icon(Icon::Settings)
                    .enabled(editable)
                    .render(),
                inspect_button(state, server),
                IconButton::new(
                    format!("mcp_delete:{}", server.id),
                    Icon::Trash,
                    theme::control_palette(),
                )
                .accessibility_label("Eliminar servidor")
                .tooltip("Eliminar servidor")
                .size(ControlSize::Compact)
                .variant(ButtonVariant::Ghost)
                .enabled(editable)
                .render(),
            ],
            false,
        ),
    ];
    let header = vec![Element::column(summary)
        .gap(12.0)
        .padding(settings_parts::ROW_PADDING)];
    result.push(settings_parts::settings_card(header));
    result.extend(authentication(state, server, compact));
    // Canonicalization belongs to refresh/actions/polling, never painting.
    let workspace = if state.mcp_presentation.workspace == state.workspace {
        state
            .mcp_presentation
            .canonical_workspace
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(&state.workspace))
    } else {
        std::path::Path::new(&state.workspace)
    };
    let current = state
        .mcp_inspection
        .scope
        .as_ref()
        .is_some_and(|scope| scope.workspace == workspace && scope.server == *server);
    let inner_width = (width - settings_parts::ROW_PADDING * 2.0).max(0.0);
    let mut tools = Vec::new();
    if let Err(error) = state.mcp_presentation.selections.selected_for(server) {
        tools.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    if current {
        if let Some(snapshot) = &state.mcp_inspection.snapshot {
            let ui = &state.mcp_presentation;
            let selected = ui
                .selection_draft
                .as_ref()
                .map_or(snapshot.tools.len(), |names| names.len());
            tools.push(muted(format!(
                "{} herramientas recibidas · {} seleccionadas{}",
                snapshot.tools.len(),
                selected,
                if ui.selection_dirty {
                    " · cambios sin guardar"
                } else {
                    ""
                }
            )));
            tools.push(muted(format!(
                "{} · versión {}",
                snapshot.server_info.name, snapshot.server_info.version
            )));
            tools.push(search(
                "mcp_tool_query",
                &ui.tool_query,
                "Buscar herramientas",
                inner_width,
            ));
            tools.push(actions(
                vec![
                    outline("mcp_tools_all", "Todas").render(),
                    outline("mcp_tools_none", "Ninguna").render(),
                    outline("mcp_tools_revert", "Descartar")
                        .enabled(ui.selection_dirty)
                        .render(),
                    outline("mcp_tools_save", "Guardar")
                        .enabled(
                            ui.selection_dirty
                                && ui.selection_error.is_none()
                                && (ui.selection_draft.is_none()
                                    || selected <= crate::mcp_tool_selection::MAX_SELECTED_TOOLS),
                        )
                        .render(),
                ],
                compact,
            ));
            tools.push(muted("El turno admite hasta 128 herramientas entre locales y MCP. Si el conjunto no cabe, se informa qué servidor requiere una selección menor."));
            let query = ui.tool_query.trim().to_lowercase();
            let mut entries = Vec::new();
            for tool in &snapshot.tools {
                if !query.is_empty()
                    && !format!(
                        "{} {}",
                        tool.name,
                        tool.description.as_deref().unwrap_or("")
                    )
                    .to_lowercase()
                    .contains(&query)
                {
                    continue;
                }
                let checked = ui
                    .selection_draft
                    .as_ref()
                    .is_none_or(|names| names.contains(&tool.name));
                let mut entry = vec![Element::row(vec![
                    Checkbox::new(
                        format!("mcp_tool_check:{}", tool.name),
                        checked,
                        theme::control_palette(),
                    )
                    .label(&tool.name)
                    .size(ControlSize::Compact)
                    .enabled(ui.selection_error.is_none())
                    .render()
                    .flex(1.0),
                    IconButton::new(
                        format!("mcp_tool_expand:{}", tool.name),
                        if ui.expanded_tool.as_deref() == Some(&tool.name) {
                            Icon::ChevronUp
                        } else {
                            Icon::ChevronDown
                        },
                        theme::control_palette(),
                    )
                    .accessibility_label(format!("Detalles de {}", tool.name))
                    .tooltip(format!("Detalles de {}", tool.name))
                    .size(ControlSize::Compact)
                    .variant(ButtonVariant::Ghost)
                    .render(),
                ])
                .gap(10.0)];
                if ui.expanded_tool.as_deref() == Some(&tool.name) {
                    if let Some(description) = &tool.description {
                        entry.push(muted(description));
                    }
                    let json = serde_json::to_string_pretty(tool).unwrap_or_default();
                    entry.push(
                        ScrollArea::new(
                            format!("mcp_tool_schema:{}", tool.name),
                            Text::new(json, theme::component_palette())
                                .mono()
                                .render()
                                .padding(10.0),
                        )
                        .render()
                        .max_height(260.0),
                    );
                }
                entries.push(Element::column(entry).gap(8.0).padding(10.0));
            }
            if entries.is_empty() {
                entries.push(muted("No hay herramientas que coincidan con la búsqueda."));
            }
            tools.push(
                ScrollArea::new("mcp_tools_list", Element::column(entries).gap(4.0))
                    .render()
                    .max_height(420.0),
            );
        } else if state.mcp_inspection.is_busy() {
            tools.push(muted("Conectando y consultando herramientas"));
            tools.push(
                outline("mcp_inspection_cancel", "Cancelar inspección")
                    .enabled(
                        state.mcp_inspection.phase
                            == crate::mcp_inspection::McpInspectionPhase::Running,
                    )
                    .render(),
            );
        } else {
            tools.push(muted("Sin catálogo inspeccionado. Usa Inspeccionar para consultar las herramientas de esta configuración."));
        }
        for (index, error) in [
            state.mcp_inspection.error.as_ref(),
            state.mcp_inspection.shutdown_error.as_ref(),
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(i, e)| e.map(|e| (i, e)))
        {
            tools.push(
                ScrollArea::new(
                    format!("mcp_detail_error:{index}"),
                    Text::new(error, theme::component_palette())
                        .role(TextRole::Small)
                        .mono()
                        .ink(theme::DESTRUCTIVE)
                        .render()
                        .padding(settings_parts::ROW_PADDING),
                )
                .render()
                .max_height(130.0),
            );
        }
    } else {
        tools.push(page_note("Herramientas del servidor","Inspecciona esta configuración para consultar sus herramientas y elegir cuáles ofrecer al agente.",SettingsPage::Mcp.icon()));
    }
    result.push(settings_parts::settings_group(
        "Herramientas",
        vec![Element::column(tools)
            .gap(12.0)
            .padding(settings_parts::ROW_PADDING)],
    ));
    result.push(
        Collapsible::new(
            "mcp_brand_picker",
            "Icono del servidor",
            if state.mcp_presentation.icon_picker {
                icon_picker(state, server, compact, (width - 39.0).max(0.0))
            } else {
                Element::spacer()
            },
            theme::component_palette(),
        )
        .detail(
            brand(state, server)
                .map(|entry| entry.title)
                .unwrap_or("Icono genérico"),
        )
        .expanded(state.mcp_presentation.icon_picker)
        .enabled(state.mcp_presentation.preferences_error.is_none())
        .render(),
    );
    result.push(
        Collapsible::new(
            "mcp_technical",
            "Conexión y detalles técnicos",
            if state.mcp_presentation.technical {
                technical(state, server, current, compact, (width - 39.0).max(0.0))
            } else {
                Element::spacer()
            },
            theme::component_palette(),
        )
        .expanded(state.mcp_presentation.technical)
        .render(),
    );
    result
}

fn technical(
    state: &ViewState,
    server: &McpServerSettings,
    current: bool,
    compact: bool,
    width: f32,
) -> Element {
    let inner_width = (width - settings_parts::ROW_PADDING * 2.0).max(0.0);
    let mut items = vec![technical_value("Transporte", transport(server))];
    if let Some(url) = &server.url {
        items.push(technical_value("Endpoint", url));
    } else {
        items.push(technical_value("Programa", &server.program));
    }
    if let Some(oauth) = &server.http_oauth {
        items.push(technical_value("OAuth 2.1", &oauth.issuer));
    }
    let mut notes = Vec::new();
    if let Some(found) = state.mcp_presentation.discovery.get(&server.id) {
        if let Some(path) = &found.executable {
            items.push(technical_value("Ejecutable", path.display().to_string()));
        }
        if let Some(package) = &found.package {
            items.push(technical_value("Paquete local", &package.name));
            if let Some(version) = &package.version {
                items.push(technical_value("Versión", version));
            }
            items.push(technical_value(
                "Manifiesto",
                package.manifest.display().to_string(),
            ));
        }
        notes.extend(found.warnings.iter().map(muted));
    }
    notes.push(muted("La detección local lee PATH y metadatos instalados. No ejecuta ni instala servidores; una marca no verifica su identidad."));
    let mut rows = vec![Element::column(vec![
        facts(items, compact, inner_width),
        Element::column(notes).gap(8.0),
    ])
    .gap(12.0)
    .padding(settings_parts::ROW_PADDING)];
    if current && !state.mcp_inspection.is_busy() {
        rows.push(super::mcp_inspection::metadata(
            &state.mcp_inspection,
            compact,
        ));
    }
    Element::column(rows).gap(12.0)
}

fn icon_picker(
    state: &ViewState,
    server: &McpServerSettings,
    compact: bool,
    width: f32,
) -> Element {
    let ui = &state.mcp_presentation;
    let choice = ui
        .preferences
        .icons
        .get(&server.id)
        .unwrap_or(&BrandChoice::Auto);
    let mut rows=vec![
        muted("Iconos de SVGL, disponibles sin conexión. Automático usa un dominio único o una web oficial corroborada por el paquete o ejecutable local. La marca es un indicio, no verifica al servidor."),
        ButtonGroup::new(vec![outline("mcp_brand:auto","Automático").active(*choice==BrandChoice::Auto).render(),outline("mcp_brand:none","Sin marca").active(*choice==BrandChoice::None).render(),outline("mcp_brand_cancel","Cerrar selector").render()]).stacked(compact).render(),
        search("mcp_brand_query",&ui.icon_query,"Buscar empresa o producto",width - settings_parts::ROW_PADDING * 2.0),
    ];
    let mut options = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if ui.icon_query.trim().is_empty() {
        for candidate in with_hints(state, server, crate::brand_icons::candidates) {
            if let Some(entry) = crate::brand_icons::get(candidate.id) {
                options.push((entry, Some(candidate.reason.label())));
                seen.insert(entry.id);
            }
        }
    }
    for entry in crate::brand_icons::search(&ui.icon_query, 80) {
        if seen.insert(entry.id) {
            options.push((entry, None));
        }
    }
    let entries: Vec<_> = options
        .into_iter()
        .map(|(entry, reason)| {
            Element::row(vec![
                entry.element(!crate::theme_mode::light()),
                Element::column(vec![
                    outline(format!("mcp_brand:{}", entry.id), entry.title)
                        .active(matches!(choice,BrandChoice::Brand(id) if id==entry.id))
                        .render(),
                    muted(reason.unwrap_or(entry.website)),
                ])
                .gap(2.0)
                .flex(1.0),
            ])
            .gap(10.0)
            .padding(6.0)
        })
        .collect();
    rows.push(
        ScrollArea::new(
            "mcp_brand_options",
            Element::column(if entries.is_empty() {
                vec![muted("No hay iconos que coincidan.")]
            } else {
                entries
            })
            .gap(4.0),
        )
        .render()
        .max_height(320.0),
    );
    Element::column(rows)
        .gap(12.0)
        .padding(settings_parts::ROW_PADDING)
}

fn authentication(state: &ViewState, server: &McpServerSettings, compact: bool) -> Vec<Element> {
    use crate::view::{McpOAuthViewPhase, McpOAuthViewStatus};
    if server.http_oauth.is_none() {
        return Vec::new();
    }
    let status = state
        .mcp_oauth_status
        .as_ref()
        .filter(|status| status.server_id == server.id);
    let busy = status.is_some_and(|status| {
        matches!(
            status.phase,
            McpOAuthViewPhase::Discovering | McpOAuthViewPhase::AwaitingBrowser { .. }
        )
    });
    let mut rows = vec![Element::row(vec![
        Text::new("OAuth 2.1", theme::component_palette())
            .role(TextRole::Label)
            .render()
            .flex(1.0)
            .centered_y(),
        outline(format!("mcp_oauth_authorize:{}", server.id), "Autorizar")
            .enabled(!busy && state.mcp_load_error.is_none())
            .loading(busy)
            .render(),
    ])
    .gap(12.0)];
    if let Some(McpOAuthViewStatus { phase, .. }) = status {
        match phase {
            McpOAuthViewPhase::Discovering => {
                rows.push(muted("Buscando el servidor de autorización"))
            }
            McpOAuthViewPhase::AwaitingBrowser { url } => {
                rows.push(muted(
                    "Esperando autorización en el navegador. La URL es temporal:",
                ));
                rows.push(
                    Element::panel(Text::new(url, theme::component_palette()).mono().render())
                        .padding(10.0)
                        .background(theme::INPUT)
                        .border(theme::BORDER, 1.0)
                        .radius(6.0),
                );
                rows.push(outline("mcp_oauth_open", "Abrir autorización").render());
            }
            McpOAuthViewPhase::Authorized => rows.push(muted(
                "Autorización guardada. El próximo turno usará esta credencial.",
            )),
            McpOAuthViewPhase::Failed(message) => rows.push(settings_parts::feedback_banner(
                message,
                FeedbackTone::Error,
                None,
                compact,
            )),
        }
        if busy {
            rows.push(outline("mcp_oauth_cancel", "Cancelar autorización").render());
        }
    }
    vec![settings_parts::settings_group(
        "Autenticación",
        vec![Element::column(rows)
            .gap(12.0)
            .padding(settings_parts::ROW_PADDING)],
    )]
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
