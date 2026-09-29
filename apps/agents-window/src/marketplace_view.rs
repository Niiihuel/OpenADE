//! Native Marketplace surface for portable `SKILL.md` bundles.
//!
//! The view is deliberately honest about its sources: the user adds a local
//! folder or an explicit GitHub repository. JavaScript plugins and MCP servers
//! are different runtimes and are not offered as installable items here.

use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, DropdownMenu, DropdownMenuItem, Element,
    Empty, FontFamily, Icon, IconButton, Modal, ScrollArea, SearchField, Switch, Tab, Tabs,
    TextField, TextSpan, Tooltip,
};

use crate::{
    marketplace::{InstallScope, MarketplaceItem, MarketplaceSnapshot, SourceKind},
    theme,
};

// Keep the center-dock measure in sync with Automations.
const MAX_WIDTH: f32 = 960.0;
const CARD_GAP: f32 = 8.0;

#[derive(Clone, Debug)]
pub struct MarketplaceViewState {
    pub snapshot: MarketplaceSnapshot,
    pub query: String,
    pub installed_only: bool,
    pub add_menu_open: bool,
    pub add_source_kind: Option<SourceKind>,
    pub source_draft: String,
    pub install_scope: InstallScope,
    pub status: Option<String>,
    pub busy: bool,
    pub pending_uninstall_id: Option<String>,
    pub pending_remove_source_id: Option<String>,
}

impl Default for MarketplaceViewState {
    fn default() -> Self {
        Self {
            snapshot: MarketplaceSnapshot::default(),
            query: String::new(),
            installed_only: false,
            add_menu_open: false,
            add_source_kind: None,
            source_draft: String::new(),
            install_scope: InstallScope::User,
            status: None,
            busy: false,
            pending_uninstall_id: None,
            pending_remove_source_id: None,
        }
    }
}

fn text(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn clipped(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::truncated_text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn title(value: &str) -> Element {
    text(value, theme::FOREGROUND, 15.0)
}

fn divider() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
}

fn capsule(value: impl Into<String>) -> Element {
    Element::panel(text(value, theme::FOREGROUND_SUBTLE, 10.0))
        .padding(5.0)
        .background(theme::SELECTED)
        .radius(6.0)
        .size(None, Some(20.0))
}

fn source_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local => "Carpeta local",
        SourceKind::Github => "GitHub",
    }
}

fn scope_label(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::User => "Usuario",
        InstallScope::Workspace => "Proyecto",
    }
}

fn matches_query(item: &MarketplaceItem, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || item.name.to_lowercase().contains(&query)
        || item.description.to_lowercase().contains(&query)
        || item.source_name.to_lowercase().contains(&query)
}

fn section_heading(label: &str, count: usize) -> Element {
    Element::row(vec![
        title(label),
        Element::spacer().flex(1.0),
        capsule(count.to_string()),
    ])
    .gap(10.0)
    .size(None, Some(24.0))
}

fn item_card(item: &MarketplaceItem, width: f32, busy: bool) -> Element {
    let marker = Element::panel(
        Element::icon(if item.source_kind == SourceKind::Github {
            Icon::Github
        } else {
            Icon::Folder
        })
        .foreground(theme::FOREGROUND_SUBTLE)
        .size(Some(16.0), Some(16.0)),
    )
    .background(theme::SELECTED)
    .padding(8.0)
    .radius(8.0)
    .size(Some(32.0), Some(32.0));
    let header = Element::row(vec![
        marker,
        Element::column(vec![
            clipped(&item.name, theme::FOREGROUND, 13.0),
            clipped(
                format!("{} · {}", item.source_name, source_label(item.source_kind)),
                theme::FOREGROUND_SUBTLE,
                10.0,
            ),
        ])
        .gap(2.0)
        .flex(1.0),
    ])
    .gap(8.0)
    .size(None, Some(34.0));

    let mut details = vec![
        header,
        clipped(&item.description, theme::FOREGROUND_SUBTLE, 12.0).size(None, Some(18.0)),
    ];
    if !item.source_available {
        details.push(text(
            "El origen ya no está disponible",
            theme::DESTRUCTIVE,
            10.0,
        ));
    }
    let bottom = if let Some(scope) = item.installed_scope {
        let badge = capsule(format!("Instalada · {}", scope_label(scope)));
        let toggle_id = format!("marketplace_toggle:{}", item.id);
        let toggle = Tooltip::new(
            Switch::new(&toggle_id, item.enabled, theme::component_palette())
                .enabled(!busy)
                .render(),
            toggle_id,
            if item.enabled {
                "Desactivar skill"
            } else {
                "Activar skill"
            },
            theme::component_palette(),
        )
        .render();
        let uninstall = IconButton::new(
            format!("marketplace_uninstall:{}", item.id),
            Icon::Trash,
            theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .tooltip("Desinstalar skill")
        .enabled(!busy)
        .render();
        if width < 350.0 {
            Element::column(vec![
                Element::row(vec![badge, Element::spacer().flex(1.0)]),
                Element::row(vec![
                    text("Habilitada", theme::FOREGROUND_SUBTLE, 10.0),
                    toggle,
                    Element::spacer().flex(1.0),
                    uninstall,
                ])
                .gap(6.0)
                .size(None, Some(28.0)),
            ])
            .gap(4.0)
        } else {
            Element::row(vec![
                badge,
                Element::spacer().flex(1.0),
                text("Habilitada", theme::FOREGROUND_SUBTLE, 10.0),
                toggle,
                uninstall,
            ])
            .gap(6.0)
            .size(None, Some(28.0))
        }
    } else {
        let license = capsule(
            item.license
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("Licencia no declarada"),
        );
        let install = Button::new(
            format!("marketplace_install:{}", item.id),
            "Instalar",
            theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
        )
        .variant(ButtonVariant::Solid)
        .size(ControlSize::Compact)
        .enabled(!busy && item.source_available)
        .render();
        Element::row(vec![license, Element::spacer().flex(1.0), install])
            .gap(6.0)
            .size(None, Some(28.0))
    };
    details.push(bottom);
    Element::panel(Element::column(details).gap(5.0))
        .padding(10.0)
        .background(theme::PANEL)
        .border(theme::BORDER, 1.0)
        .radius(10.0)
        .size(Some(width), None)
}

fn item_grid(items: &[&MarketplaceItem], width: f32, busy: bool) -> Element {
    let two_columns = width >= 690.0;
    let card_width = if two_columns {
        (width - CARD_GAP) / 2.0
    } else {
        width
    };
    let mut rows = Vec::new();
    for pair in items.chunks(if two_columns { 2 } else { 1 }) {
        let mut cards = pair
            .iter()
            .map(|item| item_card(item, card_width, busy))
            .collect::<Vec<_>>();
        if two_columns && cards.len() == 1 {
            cards.push(Element::spacer().size(Some(card_width), None));
        }
        rows.push(Element::row(cards).gap(CARD_GAP));
    }
    Element::column(rows).gap(CARD_GAP)
}

fn source_form_body(state: &MarketplaceViewState, width: f32) -> Element {
    let kind = state.add_source_kind.expect("source form requires a kind");
    let mut contents = vec![
        text(
            match kind {
                SourceKind::Local => "Ruta de la carpeta",
                SourceKind::Github => "URL del repositorio",
            },
            theme::FOREGROUND,
            12.0,
        ),
        TextField::new(
            "marketplace_source_draft",
            &state.source_draft,
            match kind {
                SourceKind::Local => "Ruta absoluta a la carpeta de skills",
                SourceKind::Github => "https://github.com/usuario/repositorio",
            },
            theme::component_palette(),
        )
        .size(ControlSize::Regular)
        .width((width - 48.0).max(80.0))
        .enabled(!state.busy)
        .render(),
        text(
            match kind {
                SourceKind::Local => "Se detectarán las carpetas que contengan SKILL.md.",
                SourceKind::Github => "Usa una URL de GitHub que contenga archivos SKILL.md.",
            },
            theme::FOREGROUND_SUBTLE,
            11.0,
        ),
    ];
    if let Some(status) = state.status.as_deref() {
        contents.push(
            Element::formatted_text(vec![TextSpan::new(
                status,
                if state.busy {
                    theme::FOREGROUND_SUBTLE
                } else {
                    theme::DESTRUCTIVE
                },
                11.0,
                FontFamily::Sans,
            )])
            .size(Some((width - 40.0).max(80.0)), None),
        );
    }
    Element::column(contents).gap(8.0)
}

fn source_form_footer(state: &MarketplaceViewState) -> Element {
    Element::row(vec![
        Element::spacer().flex(1.0),
        Button::new(
            "marketplace_source_cancel",
            "Cancelar",
            theme::component_palette(),
        )
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .enabled(!state.busy)
        .render(),
        Button::new(
            "marketplace_source_submit",
            if state.busy {
                "Agregando…"
            } else {
                "Agregar origen"
            },
            theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
        )
        .variant(ButtonVariant::Solid)
        .size(ControlSize::Compact)
        .enabled(!state.busy && !state.source_draft.trim().is_empty())
        .render(),
    ])
    .gap(8.0)
}

fn source_list(state: &MarketplaceViewState, width: f32) -> Element {
    let mut rows = vec![section_heading("Orígenes", state.snapshot.sources.len())];
    for source in &state.snapshot.sources {
        rows.push(
            Element::row(vec![
                Element::icon(match source.kind {
                    SourceKind::Local => Icon::Folder,
                    SourceKind::Github => Icon::Github,
                })
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(16.0), Some(16.0)),
                Element::column(vec![
                    clipped(&source.name, theme::FOREGROUND, 12.0),
                    clipped(&source.location, theme::FOREGROUND_SUBTLE, 10.0),
                ])
                .gap(2.0)
                .flex(1.0),
                IconButton::new(
                    format!("marketplace_remove_source:{}", source.id),
                    Icon::Trash,
                    theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .tooltip("Quitar origen")
                .enabled(!state.busy)
                .render(),
            ])
            .gap(8.0)
            .padding(8.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(8.0)
            .size(None, Some(48.0)),
        );
    }
    Element::column(rows).gap(6.0).size(Some(width), None)
}

/// Build the Marketplace's scrollable center-dock surface. The host owns all
/// state changes and should refresh the snapshot after a completed operation.
pub fn marketplace_page(state: &MarketplaceViewState, available_width: f32) -> Element {
    let gutter = if available_width < 640.0 { 14.0 } else { 28.0 };
    let width = (available_width - 2.0 * gutter).clamp(240.0, MAX_WIDTH);
    let compact = width < 600.0;
    let items: Vec<_> = state
        .snapshot
        .items
        .iter()
        .filter(|item| !state.installed_only || item.installed_scope.is_some())
        .filter(|item| matches_query(item, &state.query))
        .collect();

    let refresh = Button::new(
        "marketplace_refresh",
        if state.busy {
            "Actualizando…"
        } else {
            "Actualizar"
        },
        theme::action_palette(theme::PANEL, theme::FOREGROUND),
    )
    .icon(Icon::Refresh)
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .enabled(!state.busy)
    .render();
    let add = Button::new(
        "marketplace_add_menu",
        "Agregar origen",
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .icon(Icon::Add)
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .enabled(!state.busy)
    .render();
    let add = DropdownMenu::new(
        add,
        vec![
            DropdownMenuItem::new("marketplace_source_kind:local", "Carpeta local")
                .icon(Icon::Folder)
                .into(),
            DropdownMenuItem::new("marketplace_source_kind:github", "Repositorio GitHub")
                .icon(Icon::Github)
                .into(),
        ],
        state.add_menu_open && !state.busy,
        "marketplace_add_menu_dismiss",
        theme::menu_palette(),
    )
    .width(200.0)
    .render();
    let header = if compact {
        Element::column(vec![
            Element::row(vec![
                text("Marketplace", theme::FOREGROUND, 22.0),
                Element::spacer().flex(1.0),
                refresh,
            ])
            .gap(6.0),
            Element::row(vec![
                text(
                    "Skills para el agente nativo",
                    theme::FOREGROUND_SUBTLE,
                    11.0,
                )
                .flex(1.0),
                add,
            ])
            .gap(6.0),
        ])
        .gap(4.0)
    } else {
        Element::row(vec![
            Element::column(vec![
                text("Marketplace", theme::FOREGROUND, 22.0),
                text(
                    "Skills para el agente nativo",
                    theme::FOREGROUND_SUBTLE,
                    11.0,
                ),
            ])
            .gap(2.0)
            .flex(1.0),
            refresh,
            add,
        ])
        .gap(8.0)
    };

    let tabs = Tabs::new(
        vec![
            Tab::new("marketplace_filter_all", "Todas", 76.0).active(!state.installed_only),
            Tab::new("marketplace_filter_installed", "Instaladas", 90.0)
                .active(state.installed_only),
        ],
        theme::segmented_palette(),
    )
    .id("marketplace_filters")
    .size(ControlSize::Compact)
    .render();
    let scope = Tabs::new(
        vec![
            Tab::new("marketplace_scope_user", "Usuario", 72.0)
                .active(state.install_scope == InstallScope::User),
            Tab::new("marketplace_scope_workspace", "Proyecto", 78.0)
                .active(state.install_scope == InstallScope::Workspace),
        ],
        theme::segmented_palette(),
    )
    .id("marketplace_scope")
    .size(ControlSize::Compact)
    .render();

    let search = SearchField::new(
        "marketplace_search",
        &state.query,
        "Buscar skills, descripciones u orígenes…",
        theme::component_palette(),
    )
    .size(ControlSize::Regular)
    .render()
    .size(Some(width), Some(ControlSize::Regular.height()));
    let filters = if compact {
        Element::column(vec![
            Element::row(vec![tabs, Element::spacer().flex(1.0)]),
            Element::row(vec![
                clipped("Instalar en", theme::FOREGROUND_SUBTLE, 10.0),
                Element::spacer().flex(1.0),
                scope,
            ])
            .gap(8.0),
        ])
        .gap(6.0)
    } else {
        Element::row(vec![
            tabs,
            Element::spacer().flex(1.0),
            clipped("Instalar en", theme::FOREGROUND_SUBTLE, 10.0),
            scope,
        ])
        .gap(8.0)
        .size(None, Some(28.0))
    };

    let mut sections = vec![header, search, filters, divider()];
    if let Some(status) = state
        .status
        .as_deref()
        .filter(|value| *value != "Catálogo cargado" && state.add_source_kind.is_none())
    {
        sections.push(text(status, theme::FOREGROUND, 11.0));
    }
    for diagnostic in &state.snapshot.diagnostics {
        sections.push(
            Element::panel(
                Element::row(vec![
                    Element::icon(Icon::CloseCircle)
                        .foreground(theme::DESTRUCTIVE)
                        .size(Some(16.0), Some(16.0)),
                    text(diagnostic, theme::FOREGROUND, 11.0),
                ])
                .gap(7.0),
            )
            .padding(8.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(8.0),
        );
    }
    if items.is_empty() {
        let (heading, explanation) = if !state.query.trim().is_empty() {
            ("Sin resultados", "Prueba otra búsqueda o cambia el filtro.")
        } else if state.installed_only {
            (
                "Sin skills instaladas",
                "Explora el catálogo para instalar una skill.",
            )
        } else if state.snapshot.sources.is_empty() {
            (
                "Empieza con un origen",
                "Agrega un origen que contenga SKILL.md.",
            )
        } else {
            (
                "No hay skills disponibles",
                "Actualiza el catálogo o revisa los orígenes.",
            )
        };
        let mut empty = Empty::new(heading, explanation, theme::component_palette())
            .icon(Icon::Grid)
            .width(width);
        if state.installed_only && state.query.trim().is_empty() {
            empty = empty.action(
                "marketplace_filter_all",
                "Ver todas",
                ButtonVariant::Secondary,
            );
        }
        // The scroll child fills the viewport even when its measured content is
        // shorter. Flexible space places the empty state in that remaining
        // area while keeping the header, filters, and any origins scrollable.
        sections.push(Element::spacer().flex(1.0));
        sections.push(empty.render());
        sections.push(Element::spacer().flex(1.0));
    } else {
        sections.push(section_heading("Skills", items.len()));
        sections.push(item_grid(&items, width, state.busy));
    }
    if !state.snapshot.sources.is_empty() {
        sections.push(divider());
        sections.push(source_list(state, width));
    }
    let content = Element::column(sections).gap(10.0).size(Some(width), None);
    let centered = Element::row(vec![
        Element::spacer().flex(1.0),
        content,
        Element::spacer().flex(1.0),
    ])
    .padding(gutter);
    let page = ScrollArea::new("marketplace_page", centered)
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .flex(1.0);
    if let Some(kind) = state.add_source_kind {
        let modal_width = (available_width - 24.0).clamp(240.0, 460.0);
        Modal::new(
            page,
            true,
            match kind {
                SourceKind::Local => "Agregar carpeta local",
                SourceKind::Github => "Agregar repositorio GitHub",
            },
            "Agrega un origen para descubrir skills del catálogo.",
            "marketplace_source_cancel",
            theme::component_palette(),
        )
        .width(modal_width)
        .body(source_form_body(state, modal_width))
        .footer(source_form_footer(state))
        .initial_focus("marketplace_source_draft")
        .return_focus("marketplace_add_menu")
        .backdrop_dismiss(!state.busy)
        .render()
    } else {
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::MarketplaceSource;
    use openide_ui::{DrawCommand, Key, Size, UiAction, UiEvent, UiTree};

    fn sample_state() -> MarketplaceViewState {
        MarketplaceViewState {
            snapshot: MarketplaceSnapshot {
                sources: vec![MarketplaceSource {
                    id: "source-1".into(),
                    name: "team-skills".into(),
                    location: "C:/team-skills".into(),
                    kind: SourceKind::Local,
                }],
                items: vec![MarketplaceItem {
                    id: "source-1:git-review".into(),
                    name: "git-review".into(),
                    description: "Review code changes".into(),
                    license: Some("MIT".into()),
                    source_name: "team-skills".into(),
                    source_kind: SourceKind::Local,
                    installed_scope: None,
                    enabled: false,
                    source_available: true,
                }],
                diagnostics: Vec::new(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn marketplace_exposes_real_source_install_and_scope_actions() {
        let state = sample_state();
        let frame = UiTree::new(marketplace_page(&state, 920.0)).layout(Size::new(920.0, 800.0));
        for id in [
            "marketplace_search",
            "marketplace_add_menu",
            "marketplace_filter_all",
            "marketplace_filter_installed",
            "marketplace_scope_user",
            "marketplace_scope_workspace",
            "marketplace_install:source-1:git-review",
            "marketplace_remove_source:source-1",
        ] {
            assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
        }
    }

    #[test]
    fn install_scope_caption_is_centered_with_segmented_tabs() {
        for width in [560.0, 920.0] {
            let frame = UiTree::new(marketplace_page(&sample_state(), width))
                .layout(Size::new(width, 800.0));
            let text_y = |target: &str| {
                frame.commands.iter().find_map(|command| match command {
                    DrawCommand::Text { text, position, .. } if text == target => Some(position.y),
                    _ => None,
                })
            };
            let caption = text_y("Instalar en").expect("scope caption");
            let tab = text_y("Usuario").expect("scope tab");
            assert!(
                (caption - tab).abs() < 2.0,
                "scope caption y={caption}, tab y={tab}, width={width}"
            );
        }
    }

    #[test]
    fn installed_item_has_toggle_and_uninstall() {
        let mut state = sample_state();
        state.snapshot.items[0].installed_scope = Some(InstallScope::Workspace);
        state.snapshot.items[0].enabled = true;
        let frame = UiTree::new(marketplace_page(&state, 560.0)).layout(Size::new(560.0, 800.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_toggle:source-1:git-review"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_uninstall:source-1:git-review"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_install:source-1:git-review"));
    }

    #[test]
    fn source_form_only_submits_a_selected_nonempty_location() {
        let mut state = MarketplaceViewState {
            add_menu_open: true,
            ..Default::default()
        };
        let frame = UiTree::new(marketplace_page(&state, 600.0)).layout(Size::new(600.0, 800.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_kind:local"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_submit"));
        // Selecting a menu option closes the menu and opens a centered form.
        state.add_menu_open = false;
        state.add_source_kind = Some(SourceKind::Github);
        state.source_draft = "https://github.com/owner/repo".into();
        let frame = UiTree::new(marketplace_page(&state, 600.0)).layout(Size::new(600.0, 800.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_draft"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_submit"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_cancel"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_kind:local"));
    }

    #[test]
    fn adding_source_is_a_centered_modal_without_reflowing_catalog() {
        let mut state = sample_state();
        let base = UiTree::new(marketplace_page(&state, 920.0)).layout(Size::new(920.0, 800.0));
        let base_item_y = base
            .regions
            .iter()
            .find(|region| region.id == "marketplace_install:source-1:git-review")
            .unwrap()
            .rect
            .y;
        state.add_source_kind = Some(SourceKind::Local);
        state.source_draft = "C:/team-skills".into();
        let mut tree = UiTree::new(marketplace_page(&state, 920.0));
        let frame = tree.layout(Size::new(920.0, 800.0));
        let input = frame
            .regions
            .iter()
            .find(|region| region.id == "marketplace_source_draft")
            .unwrap();
        assert!((input.rect.x + input.rect.width / 2.0 - 460.0).abs() < 8.0);
        assert!((input.rect.y + input.rect.height / 2.0 - 400.0).abs() < 180.0);
        assert_eq!(tree.focused_id(), Some("marketplace_source_draft"));
        let open_item_y = frame
            .regions
            .iter()
            .find(|region| region.id == "marketplace_install:source-1:git-review")
            .unwrap()
            .rect
            .y;
        assert!((base_item_y - open_item_y).abs() < 1.0);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == Color::rgba(0, 0, 0, 180)
        )));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "marketplace_source_cancel".into(),
            }]
        );
    }

    #[test]
    fn catalog_refresh_has_a_visible_label_instead_of_an_icon_tooltip() {
        let state = sample_state();
        let frame = UiTree::new(marketplace_page(&state, 920.0)).layout(Size::new(920.0, 800.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_refresh"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Actualizar"
        )));
    }

    #[test]
    fn source_modal_restores_focus_to_add_source_after_closing() {
        let mut state = sample_state();
        state.add_menu_open = true;
        let mut tree = UiTree::new(marketplace_page(&state, 920.0));
        tree.layout(Size::new(920.0, 800.0));
        assert!(tree.focus("marketplace_source_kind:local"));

        state.add_menu_open = false;
        state.add_source_kind = Some(SourceKind::Local);
        tree.set_root(marketplace_page(&state, 920.0));
        tree.layout(Size::new(920.0, 800.0));
        assert_eq!(tree.focused_id(), Some("marketplace_source_draft"));

        state.add_source_kind = None;
        tree.set_root(marketplace_page(&state, 920.0));
        tree.layout(Size::new(920.0, 800.0));
        assert_eq!(tree.focused_id(), Some("marketplace_add_menu"));
    }

    #[test]
    fn query_matches_name_description_and_source() {
        let item = &sample_state().snapshot.items[0].clone();
        for query in ["git", "CODE", "team"] {
            assert!(matches_query(item, query), "{query}");
        }
        assert!(!matches_query(item, "calendar"));
    }

    #[test]
    fn empty_catalog_has_one_add_source_action() {
        let state = MarketplaceViewState::default();
        let frame = UiTree::new(marketplace_page(&state, 920.0)).layout(Size::new(920.0, 800.0));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == "marketplace_add_menu")
                .count(),
            1
        );
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_empty_add_menu"));
    }

    #[test]
    fn narrow_marketplace_keeps_actions_inside_center_dock() {
        let mut state = sample_state();
        state.add_menu_open = true;
        state.add_source_kind = Some(SourceKind::Local);
        let frame = UiTree::new(marketplace_page(&state, 320.0)).layout(Size::new(320.0, 900.0));
        for region in frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("marketplace_"))
        {
            assert!(region.rect.x >= -0.5, "{} {:?}", region.id, region.rect);
            assert!(
                region.rect.x + region.rect.width <= 320.5,
                "{} {:?}",
                region.id,
                region.rect
            );
        }
    }

    #[test]
    fn marketplace_uses_the_same_center_dock_measure_as_automations() {
        let state = MarketplaceViewState::default();
        for (viewport_width, expected_gutter) in [(920.0, 28.0), (320.0, 14.0)] {
            let frame = UiTree::new(marketplace_page(&state, viewport_width))
                .layout(Size::new(viewport_width, 800.0));
            let search = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_search")
                .unwrap();
            assert!(
                (search.rect.x - expected_gutter).abs() <= 1.0,
                "search at {} with viewport {viewport_width}",
                search.rect.x
            );
        }
        let wide = UiTree::new(marketplace_page(&state, 1600.0)).layout(Size::new(1600.0, 800.0));
        let search = wide
            .regions
            .iter()
            .find(|region| region.id == "marketplace_search")
            .unwrap();
        assert!((search.rect.width - MAX_WIDTH).abs() <= 1.0);
    }

    #[test]
    fn empty_catalog_centers_in_the_available_viewport_height() {
        let state = MarketplaceViewState::default();
        let title_y = |height| {
            UiTree::new(marketplace_page(&state, 920.0))
                .layout(Size::new(920.0, height))
                .commands
                .into_iter()
                .find_map(|command| match command {
                    DrawCommand::Text { position, text, .. } if text == "Empieza con un origen" => {
                        Some(position.y)
                    }
                    _ => None,
                })
                .unwrap()
        };
        let short_y = title_y(800.0);
        let tall_y = title_y(1200.0);
        assert!(short_y > 320.0 && short_y < 600.0, "title at {short_y}");
        assert!(tall_y > short_y + 150.0, "title at {tall_y}");
    }
}
