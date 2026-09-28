//! Native Marketplace surface for portable `SKILL.md` bundles.
//!
//! The view is deliberately honest about its sources: the user adds a local
//! folder or an explicit GitHub repository. JavaScript plugins and MCP servers
//! are different runtimes and are not offered as installable items here.

use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon, IconButton,
    ScrollArea, SearchField, Switch, Tab, Tabs,
};

use crate::{
    marketplace::{InstallScope, MarketplaceItem, MarketplaceSnapshot, SourceKind},
    theme,
};

const MAX_WIDTH: f32 = 1040.0;
const CARD_GAP: f32 = 12.0;

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
    text(value, theme::FOREGROUND, 20.0)
}

fn divider() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
}

fn button(id: impl Into<String>, label: impl Into<String>, prominent: bool) -> Element {
    Button::new(
        id,
        label,
        theme::action_palette(
            if prominent { theme::BRAND } else { theme::CARD },
            if prominent {
                theme::PRIMARY_FOREGROUND
            } else {
                theme::FOREGROUND
            },
        ),
    )
    .variant(if prominent {
        ButtonVariant::Solid
    } else {
        ButtonVariant::Outline
    })
    .size(ControlSize::Regular)
    .render()
}

fn icon_button(id: impl Into<String>, icon: Icon) -> Element {
    IconButton::new(
        id,
        icon,
        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .render()
    .size(Some(32.0), Some(32.0))
}

fn capsule(value: impl Into<String>) -> Element {
    Element::panel(text(value, theme::FOREGROUND_SUBTLE, 11.0))
        .padding(6.0)
        .background(theme::SELECTED)
        .radius(7.0)
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
    .size(None, Some(32.0))
}

fn item_card(item: &MarketplaceItem, width: f32, busy: bool) -> Element {
    let marker = Element::panel(
        Element::icon(if item.source_kind == SourceKind::Github {
            Icon::Github
        } else {
            Icon::Folder
        })
        .foreground(theme::FOREGROUND)
        .size(Some(16.0), Some(16.0)),
    )
    .background(theme::SELECTED)
    .padding(12.0)
    .radius(11.0)
    .size(Some(40.0), Some(40.0));
    let header = Element::row(vec![
        marker,
        Element::column(vec![
            clipped(&item.name, theme::FOREGROUND, 14.0),
            clipped(
                format!("{} · {}", item.source_name, source_label(item.source_kind)),
                theme::FOREGROUND_SUBTLE,
                11.0,
            ),
        ])
        .gap(3.0)
        .flex(1.0),
    ])
    .gap(10.0)
    .size(None, Some(42.0));

    let mut details = vec![
        header,
        clipped(&item.description, theme::FOREGROUND_SUBTLE, 12.0).size(None, Some(31.0)),
    ];
    if !item.source_available {
        details.push(text(
            "El origen ya no está disponible",
            theme::FOREGROUND_SUBTLE,
            11.0,
        ));
    }
    let bottom = if let Some(scope) = item.installed_scope {
        let badge = capsule(format!("Instalada · {}", scope_label(scope)));
        let toggle = Switch::new(
            format!("marketplace_toggle:{}", item.id),
            item.enabled,
            theme::component_palette(),
        )
        .enabled(!busy)
        .render();
        let uninstall = IconButton::new(
            format!("marketplace_uninstall:{}", item.id),
            Icon::Trash,
            theme::action_palette(theme::CARD, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Regular)
        .enabled(!busy)
        .render()
        .size(Some(30.0), Some(30.0));
        if width < 390.0 {
            Element::column(vec![
                Element::row(vec![badge, Element::spacer().flex(1.0)]),
                Element::row(vec![
                    text("Activa", theme::FOREGROUND_SUBTLE, 11.0),
                    toggle,
                    Element::spacer().flex(1.0),
                    uninstall,
                ])
                .gap(7.0)
                .size(None, Some(30.0)),
            ])
            .gap(6.0)
        } else {
            Element::row(vec![
                badge,
                Element::spacer().flex(1.0),
                text("Activa", theme::FOREGROUND_SUBTLE, 11.0),
                toggle,
                uninstall,
            ])
            .gap(7.0)
            .size(None, Some(32.0))
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
            .gap(7.0)
            .size(None, Some(32.0))
    };
    details.push(Element::spacer().size(None, Some(3.0)));
    details.push(bottom);
    Element::panel(Element::column(details).gap(12.0))
        .padding(16.0)
        .background(theme::PANEL)
        .border(theme::BORDER, 1.0)
        .radius(13.0)
        .size(Some(width), None)
}

fn item_grid(items: &[&MarketplaceItem], width: f32, busy: bool) -> Element {
    let two_columns = width >= 700.0;
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

fn source_editor(state: &MarketplaceViewState, width: f32) -> Option<Element> {
    if !state.add_menu_open {
        return None;
    }
    let kind = state.add_source_kind;
    let choices = vec![
        Button::new(
            "marketplace_source_kind:local",
            "Carpeta local",
            theme::action_palette(
                if kind == Some(SourceKind::Local) {
                    theme::SELECTED
                } else {
                    theme::CARD
                },
                theme::FOREGROUND,
            ),
        )
        .icon(Icon::Folder)
        .variant(ButtonVariant::Subtle)
        .active(kind == Some(SourceKind::Local))
        .render(),
        Button::new(
            "marketplace_source_kind:github",
            "GitHub",
            theme::action_palette(
                if kind == Some(SourceKind::Github) {
                    theme::SELECTED
                } else {
                    theme::CARD
                },
                theme::FOREGROUND,
            ),
        )
        .icon(Icon::Github)
        .variant(ButtonVariant::Subtle)
        .active(kind == Some(SourceKind::Github))
        .render(),
    ];
    let choice = if width < 390.0 {
        Element::column(choices).gap(8.0)
    } else {
        Element::row(choices).gap(8.0)
    };
    let mut contents = vec![
        Element::row(vec![
            text("Agregar origen", theme::FOREGROUND, 16.0),
            Element::spacer().flex(1.0),
            icon_button("marketplace_source_cancel", Icon::Close),
        ]),
        text(
            "Se buscan carpetas con SKILL.md. Revisa el origen antes de instalar.",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ),
        choice,
    ];
    if let Some(kind) = kind {
        contents.push(
            Element::text_input(
                "marketplace_source_draft",
                &state.source_draft,
                match kind {
                    SourceKind::Local => "Ruta absoluta a la carpeta de skills",
                    SourceKind::Github => "https://github.com/usuario/repositorio",
                },
            )
            .background(theme::INPUT)
            .foreground(theme::FOREGROUND)
            .focus_border(theme::FOREGROUND_SUBTLE, 1.0)
            .border(theme::BORDER, 1.0)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .radius(9.0)
            .min_size(80.0, 38.0),
        );
        contents.push(
            Element::row(vec![
                Element::spacer().flex(1.0),
                Button::new(
                    "marketplace_source_submit",
                    "Agregar origen",
                    theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
                )
                .variant(ButtonVariant::Solid)
                .enabled(!state.busy && !state.source_draft.trim().is_empty())
                .render(),
            ])
            .gap(8.0),
        );
    }
    Some(
        Element::panel(Element::column(contents).gap(12.0))
            .padding(16.0)
            .background(theme::CARD)
            .border(theme::BORDER, 1.0)
            .radius(13.0)
            .size(Some(width), None),
    )
}

fn source_list(state: &MarketplaceViewState, width: f32) -> Element {
    let mut rows = vec![section_heading("Orígenes", state.snapshot.sources.len())];
    if state.snapshot.sources.is_empty() {
        rows.push(text(
            "No hay orígenes agregados. Puedes empezar con una carpeta local o un repositorio GitHub.",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ));
    } else {
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
                        clipped(&source.name, theme::FOREGROUND, 13.0),
                        clipped(&source.location, theme::FOREGROUND_SUBTLE, 11.0),
                    ])
                    .gap(4.0)
                    .flex(1.0),
                    IconButton::new(
                        format!("marketplace_remove_source:{}", source.id),
                        Icon::Trash,
                        theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Regular)
                    .enabled(!state.busy)
                    .render()
                    .size(Some(32.0), Some(32.0)),
                ])
                .gap(12.0)
                .padding(10.0)
                .background(theme::PANEL)
                .radius(10.0)
                .size(None, Some(58.0)),
            );
        }
    }
    Element::column(rows).gap(8.0).size(Some(width), None)
}

/// Build the Marketplace's scrollable center-dock surface. The host owns all
/// state changes and should refresh the snapshot after a completed operation.
pub fn marketplace_page(state: &MarketplaceViewState, available_width: f32) -> Element {
    let width = (available_width - 48.0).clamp(260.0, MAX_WIDTH);
    let compact = width < 620.0;
    let installed = state
        .snapshot
        .items
        .iter()
        .filter(|item| item.installed_scope.is_some())
        .count();
    let items: Vec<_> = state
        .snapshot
        .items
        .iter()
        .filter(|item| !state.installed_only || item.installed_scope.is_some())
        .filter(|item| matches_query(item, &state.query))
        .collect();

    let refresh = IconButton::new(
        "marketplace_refresh",
        Icon::Refresh,
        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .enabled(!state.busy)
    .render();
    let add = Button::new(
        "marketplace_add_menu",
        "Agregar origen",
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .icon(Icon::Add)
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Regular)
    .enabled(!state.busy)
    .render();
    let header = if compact {
        Element::column(vec![
            Element::row(vec![
                text("Marketplace", theme::FOREGROUND, 27.0),
                Element::spacer().flex(1.0),
                refresh,
            ])
            .gap(8.0),
            Element::row(vec![
                text(
                    "Skills abiertas para el agente nativo",
                    theme::FOREGROUND_SUBTLE,
                    12.0,
                )
                .flex(1.0),
                add,
            ])
            .gap(8.0),
        ])
        .gap(12.0)
    } else {
        Element::row(vec![
            Element::column(vec![
                text("Marketplace", theme::FOREGROUND, 30.0),
                text(
                    "Skills abiertas para el agente nativo",
                    theme::FOREGROUND_SUBTLE,
                    13.0,
                ),
            ])
            .gap(8.0)
            .flex(1.0),
            refresh,
            add,
        ])
        .gap(10.0)
    };

    let tabs = Tabs::new(
        vec![
            Tab::new("marketplace_filter_all", "Todas", 104.0).active(!state.installed_only),
            Tab::new("marketplace_filter_installed", "Instaladas", 118.0)
                .active(state.installed_only),
        ],
        theme::component_palette(),
    )
    .id("marketplace_filters")
    .height(32.0)
    .render();
    let scope = Tabs::new(
        vec![
            Tab::new("marketplace_scope_user", "Usuario", 92.0)
                .active(state.install_scope == InstallScope::User),
            Tab::new("marketplace_scope_workspace", "Proyecto", 92.0)
                .active(state.install_scope == InstallScope::Workspace),
        ],
        theme::component_palette(),
    )
    .id("marketplace_scope")
    .height(30.0)
    .render();

    let search = SearchField::new(
        "marketplace_search",
        &state.query,
        "Buscar skills, descripciones u orígenes…",
        theme::component_palette(),
    )
    .size(ControlSize::Spacious)
    .render()
    .size(Some(width), Some(40.0));
    let filters = if compact {
        Element::column(vec![
            Element::row(vec![tabs, Element::spacer().flex(1.0)]),
            Element::row(vec![
                text("Instalar para", theme::FOREGROUND_SUBTLE, 11.0),
                Element::spacer().flex(1.0),
                scope,
            ])
            .gap(8.0),
        ])
        .gap(8.0)
    } else {
        Element::row(vec![
            tabs,
            Element::spacer().flex(1.0),
            text("Instalar para", theme::FOREGROUND_SUBTLE, 11.0),
            scope,
        ])
        .gap(8.0)
        .size(None, Some(36.0))
    };

    let mut sections = vec![
        header,
        Element::spacer().size(None, Some(7.0)),
        search,
        filters,
        divider(),
    ];
    if let Some(editor) = source_editor(state, width) {
        sections.push(editor);
    }
    if let Some(status) = state.status.as_deref() {
        sections.push(
            Element::panel(text(status, theme::FOREGROUND, 12.0))
                .padding(12.0)
                .background(theme::SELECTED)
                .radius(9.0),
        );
    }
    for diagnostic in &state.snapshot.diagnostics {
        sections.push(
            Element::panel(text(diagnostic, theme::FOREGROUND_SUBTLE, 12.0))
                .padding(10.0)
                .background(theme::PANEL)
                .border(theme::BORDER, 1.0)
                .radius(9.0),
        );
    }
    if items.is_empty() {
        let (heading, explanation) = if !state.query.trim().is_empty() {
            ("Sin resultados", "Prueba otro término o cambia el filtro.")
        } else if state.installed_only {
            (
                "Todavía no hay skills instaladas",
                "Explora las skills disponibles e instala una para usarla en el agente nativo.",
            )
        } else if state.snapshot.sources.is_empty() {
            (
                "Empieza con un origen abierto",
                "Agrega una carpeta local o un repositorio GitHub que contenga SKILL.md.",
            )
        } else {
            (
                "No hay skills disponibles",
                "Revisa los diagnósticos de los orígenes o agrega otro repositorio.",
            )
        };
        sections.push(
            Element::panel(
                Element::column(vec![
                    Element::icon(Icon::Grid)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(16.0), Some(16.0)),
                    text(heading, theme::FOREGROUND, 18.0),
                    text(explanation, theme::FOREGROUND_SUBTLE, 12.0),
                    button("marketplace_empty_add_menu", "Agregar origen", true),
                ])
                .gap(14.0),
            )
            .padding(28.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(14.0)
            .size(Some(width), Some(208.0)),
        );
    } else {
        sections.push(section_heading("Skills", items.len()));
        sections.push(item_grid(&items, width, state.busy));
    }
    sections.push(Element::spacer().size(None, Some(20.0)));
    sections.push(source_list(state, width));
    sections.push(Element::spacer().size(None, Some(9.0)));
    sections.push(text(
        format!(
            "{installed} instalada(s). Las CLIs externas administran sus propias skills; este catálogo alimenta el agente nativo. No ejecuta plugins JavaScript ni servidores MCP."
        ),
        theme::FOREGROUND_SUBTLE,
        11.0,
    ));
    let content = Element::column(sections)
        .gap(16.0)
        .size(Some(width), None)
        .padding(0.0);
    let centered = Element::row(vec![
        Element::spacer().flex(1.0),
        content,
        Element::spacer().flex(1.0),
    ])
    .padding(24.0);
    ScrollArea::new("marketplace_page", centered)
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::MarketplaceSource;
    use openide_ui::{Size, UiTree};

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
        state.add_source_kind = Some(SourceKind::Github);
        state.source_draft = "https://github.com/owner/repo".into();
        let frame = UiTree::new(marketplace_page(&state, 600.0)).layout(Size::new(600.0, 800.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_source_submit"));
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
}
