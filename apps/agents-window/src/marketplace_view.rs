//! Native Marketplace surface for portable `SKILL.md` bundles.
//!
//! The catalog identifies bundled recommendations and user-added sources.
//! Installation is explicit; JavaScript plugins and MCP servers are different
//! runtimes and are not offered as installable SKILL.md bundles here.

use openide_ui::{
    Alert, AlertTone, Button, ButtonVariant, Chip, Color, Component, ControlSize, DropdownMenu,
    CardGrid, DropdownMenuItem, Element, FontFamily, Icon, IconButton, Modal, Notice, PageColumn,
    PageHeader, ScrollArea, SearchField, SectionHeader, Separator, Skeleton, Switch, Tab, Tabs,
    TextField, Tooltip, TooltipPlacement, TooltipSpec,
};

use std::{borrow::Cow, collections::BTreeMap};

use crate::{
    marketplace::{
        InstallScope, MarketplaceItem, MarketplaceSnapshot, RemoteSearchPage, SkillProvenance,
        SkillUpdate, SourceKind,
    },
    theme,
    ui_sizing::{MARKETPLACE_HEADER, MARKETPLACE_HEADER_NARROW},
};

const CARD_PADDING: f32 = 12.0;
const INSTALL_WIDTH: f32 = 88.0;

pub const PAGE_SIZE: usize = 24;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MarketplaceTab {
    #[default]
    Recommended,
    Explore,
    Installed,
}

#[derive(Clone, Debug, Default)]
pub struct MarketplaceExploreState {
    pub query: String,
    pub page: Option<RemoteSearchPage>,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MarketplaceViewState {
    pub snapshot: MarketplaceSnapshot,
    pub tab: MarketplaceTab,
    pub category: Option<String>,
    pub category_menu_open: bool,
    pub page: usize,
    pub explore: MarketplaceExploreState,
    pub updates: BTreeMap<String, SkillUpdate>,
    pub update_checking: bool,
    pub updates_checked: bool,
    pub update_error: Option<String>,
    pub provenance: BTreeMap<String, SkillProvenance>,
    pub query: String,
    pub installed_only: bool,
    pub add_menu_open: bool,
    pub add_source_kind: Option<SourceKind>,
    pub source_draft: String,
    pub install_scope: InstallScope,
    pub status: Option<String>,
    pub status_error: bool,
    pub busy: bool,
    pub pending_uninstall_id: Option<String>,
    pub pending_remove_source_id: Option<String>,
    pub selected_item_id: Option<String>,
}

impl Default for MarketplaceViewState {
    fn default() -> Self {
        Self {
            snapshot: MarketplaceSnapshot::default(),
            tab: MarketplaceTab::Recommended,
            category: None,
            category_menu_open: false,
            page: 0,
            explore: MarketplaceExploreState::default(),
            updates: BTreeMap::new(),
            update_checking: false,
            updates_checked: false,
            update_error: None,
            provenance: BTreeMap::new(),
            query: String::new(),
            installed_only: false,
            add_menu_open: false,
            add_source_kind: None,
            source_draft: String::new(),
            install_scope: InstallScope::User,
            status: None,
            status_error: false,
            busy: false,
            pending_uninstall_id: None,
            pending_remove_source_id: None,
            selected_item_id: None,
        }
    }
}

impl MarketplaceViewState {
    pub fn active_tab(&self) -> MarketplaceTab {
        self.tab
    }

    pub fn active_query(&self) -> &str {
        if self.active_tab() == MarketplaceTab::Explore {
            &self.explore.query
        } else {
            &self.query
        }
    }

    pub fn find_item(&self, id: &str) -> Option<Cow<'_, MarketplaceItem>> {
        self.snapshot
            .items
            .iter()
            .find(|item| item.id == id)
            .map(Cow::Borrowed)
            .or_else(|| {
                self.explore
                    .page
                    .as_ref()?
                    .skills
                    .iter()
                    .find(|skill| skill.id == id)
                    .map(|skill| Cow::Owned(skill.item()))
            })
    }
}

/// Return only one page of cards, including the sidebar. Filtering counts all
/// matches, but never constructs UI elements for the rest of the catalog.
pub(super) fn catalog_page(
    state: &MarketplaceViewState,
) -> (Vec<Cow<'_, MarketplaceItem>>, usize, usize) {
    if state.active_tab() == MarketplaceTab::Explore {
        let Some(results) = &state.explore.page else {
            return (vec![], 0, 0);
        };
        let total = results.skills.len();
        let page = state.page.min(total.saturating_sub(1) / PAGE_SIZE);
        let items = results
            .skills
            .iter()
            .skip(page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .filter_map(|skill| state.find_item(&skill.id))
            .collect();
        return (items, total, page);
    }
    let query = state.query.trim().to_lowercase();
    let matches = |item: &&MarketplaceItem| {
        let tab = state.active_tab();
        (tab != MarketplaceTab::Installed || item.installed_scope.is_some())
            && (tab != MarketplaceTab::Recommended || !item.id.starts_with("skills-sh:"))
            && (tab != MarketplaceTab::Recommended
                || state.category.as_ref().is_none_or(|category| {
                    crate::marketplace::starter_details(&item.id)
                        .map_or(category == "Tus orígenes", |details| {
                            details.category == *category
                        })
                }))
            && (query.is_empty()
                || item.name.to_lowercase().contains(&query)
                || item.description.to_lowercase().contains(&query)
                || item.source_name.to_lowercase().contains(&query))
    };
    let total = state.snapshot.items.iter().filter(matches).count();
    let page = state.page.min(total.saturating_sub(1) / PAGE_SIZE);
    let items = state
        .snapshot
        .items
        .iter()
        .filter(matches)
        .skip(page * PAGE_SIZE)
        .take(PAGE_SIZE)
        .map(Cow::Borrowed)
        .collect();
    (items, total, page)
}

pub(super) fn tab_strip(state: &MarketplaceViewState, sidebar: bool) -> Element {
    let prefix = if sidebar {
        "ide_marketplace_tab"
    } else {
        "marketplace_tab"
    };
    let items = [
        (
            "recommended",
            "Recomendadas",
            MarketplaceTab::Recommended,
            if sidebar { 90.0 } else { 94.0 },
        ),
        (
            "explore",
            "Explorar",
            MarketplaceTab::Explore,
            if sidebar { 58.0 } else { 60.0 },
        ),
        ("installed", "Instaladas", MarketplaceTab::Installed, 72.0),
    ]
    .into_iter()
    .map(|(key, label, tab, width)| {
        Tab::new(format!("{prefix}:{key}"), label, width).active(state.active_tab() == tab)
    })
    .collect();
    // Sidebar items total 220px; shared 2px gaps and track padding make 228px.
    // Full labels fit the native 11.5px type; narrower panels keep scroll/reveal.
    Tabs::new(items, theme::segmented_palette())
        .id(format!("{prefix}:strip"))
        .size(ControlSize::Compact)
        .render()
}

pub(super) fn pagination_controls(total: usize, page: usize, sidebar: bool) -> Element {
    if total <= PAGE_SIZE {
        return Element::spacer().size(None, Some(0.0));
    }
    let pages = total.div_ceil(PAGE_SIZE);
    let prefix = if sidebar {
        "ide_marketplace_page"
    } else {
        "marketplace_page"
    };
    let previous_id = format!("{prefix}_prev");
    let next_id = format!("{prefix}_next");
    let previous = Tooltip::new(
        IconButton::new(&previous_id, Icon::ArrowLeft, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(page > 0)
            .render()
            .dispatch_as("marketplace_page_prev"),
        &previous_id,
        "Página anterior",
        theme::component_palette(),
    )
    .render();
    let next = Tooltip::new(
        IconButton::new(&next_id, Icon::ArrowRight, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(page + 1 < pages)
            .render()
            .dispatch_as("marketplace_page_next"),
        &next_id,
        "Página siguiente",
        theme::component_palette(),
    )
    .render();
    Element::row(vec![
        text(
            format!(
                "{}–{} de {}",
                page * PAGE_SIZE + 1,
                ((page + 1) * PAGE_SIZE).min(total),
                total
            ),
            theme::FOREGROUND_SUBTLE,
            11.0,
        )
        .flex(1.0),
        previous,
        next,
    ])
    .gap(4.0)
    .size(None, Some(28.0))
}

fn category_control(state: &MarketplaceViewState) -> Element {
    let mut categories = std::collections::BTreeSet::new();
    for item in &state.snapshot.items {
        if let Some(details) = crate::marketplace::starter_details(&item.id) {
            categories.insert(details.category);
        }
    }
    if !state.snapshot.sources.is_empty() {
        categories.insert("Tus orígenes".to_owned());
    }
    let mut entries =
        vec![
            DropdownMenuItem::new("marketplace_category:all", "Todas las categorías")
                .selected(state.category.is_none())
                .into(),
        ];
    entries.extend(categories.into_iter().map(|category| {
        DropdownMenuItem::new(format!("marketplace_category:{category}"), &category)
            .selected(state.category.as_deref() == Some(category.as_str()))
            .into()
    }));
    DropdownMenu::new(
        Button::new(
            "marketplace_category_toggle",
            state.category.as_deref().unwrap_or("Todas las categorías"),
            theme::component_palette(),
        )
        .icon(Icon::ChevronDown)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .width(208.0)
        .render(),
        entries,
        state.category_menu_open,
        "marketplace_category_dismiss",
        theme::menu_palette(),
    )
    .width(232.0)
    .render()
}

/// One bounded input/submit row for both catalog and sidebar. Enter uses the
/// existing Submitted route; the secondary physical button dispatches that same
/// explicit search action without starting work during rendering.
pub(super) fn search_controls(state: &MarketplaceViewState, width: f32, sidebar: bool) -> Element {
    let explore = state.active_tab() == MarketplaceTab::Explore;
    let id = if sidebar {
        "ide_extensions_search"
    } else {
        "marketplace_search"
    };
    let button_width = if sidebar { 24.0 } else { 76.0 };
    let gap = if explore { 6.0 } else { 0.0 };
    let field_width = (width - if explore { button_width + gap } else { 0.0 }).max(0.0);
    let search = SearchField::new(
        id,
        state.active_query(),
        if explore {
            "Buscar en skills.sh…"
        } else {
            "Buscar skills…"
        },
        theme::component_palette(),
    )
    .size(ControlSize::Compact)
    .width(field_width)
    .render();
    if !explore {
        return search;
    }
    let enabled = !state.explore.busy && state.explore.query.trim().chars().count() >= 2;
    let submit = if sidebar {
        IconButton::new(
            "ide_marketplace_explore_search",
            Icon::ArrowRight,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .loading(state.explore.busy)
        .enabled(enabled)
        .dispatch_as("marketplace_explore_search")
        .tooltip_with(
            TooltipSpec::new("Buscar en skills.sh")
                .shortcut("Enter")
                .placement(TooltipPlacement::Below),
        )
        .render()
    } else {
        Button::new(
            "marketplace_explore_search",
            "Buscar",
            theme::component_palette(),
        )
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .width(button_width)
        .enabled(enabled)
        .loading(state.explore.busy)
        .render()
    };
    Element::row(vec![search, submit.centered_y()])
        .gap(gap)
        .size(Some(width), Some(28.0))
}

fn explore_status(state: &MarketplaceViewState, width: f32) -> Element {
    let explore = &state.explore;
    let mut children = vec![text(
        "Descubre skills de la comunidad en skills.sh",
        theme::FOREGROUND_SUBTLE,
        12.0,
    )];
    if explore.busy {
        children.push(text("Buscando…", theme::FOREGROUND_SUBTLE, 11.0));
        if explore.page.is_none() {
            for index in 0..3 {
                children.push(
                    Element::column(vec![
                        Skeleton::line(
                            format!("marketplace_explore_loading:{index}:name"),
                            theme::component_palette(),
                            (width * 0.55).max(0.0),
                        )
                        .render(),
                        Skeleton::line(
                            format!("marketplace_explore_loading:{index}:source"),
                            theme::component_palette(),
                            (width - 24.0).max(0.0),
                        )
                        .render(),
                    ])
                    .gap(8.0)
                    .padding(12.0)
                    .background(theme::PANEL)
                    .radius(8.0),
                );
            }
        }
    }
    if let Some(page) = &explore.page {
        children.push(text(
            format!(
                "Resultados para «{}»{}",
                page.query,
                if page.stale {
                    " · Copia guardada sin conexión"
                } else if page.from_cache {
                    " · Copia guardada"
                } else {
                    ""
                }
            ),
            theme::FOREGROUND_SUBTLE,
            11.0,
        ));
        if let Some(warning) = &page.warning {
            children.push(
                Alert::new("Resultados guardados", warning, theme::component_palette())
                    .compact(true)
                    .tone(AlertTone::Neutral)
                    .render(),
            );
        }
    }
    if let Some(error) = &explore.error {
        children.push(
            Alert::new("No se pudo buscar", error, theme::component_palette())
                .compact(true)
                .tone(AlertTone::Error)
                .render(),
        );
        children.push(Element::row(vec![
            Button::new(
                "marketplace_explore_retry",
                "Reintentar",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!explore.busy)
            .render(),
            Element::spacer().flex(1.0),
        ]));
    } else if explore.page.is_none() && !explore.busy {
        children.push(text(
            "Escribí al menos 2 caracteres y presioná Enter para buscar.",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ));
    }
    Element::column(children).gap(8.0)
}

fn update_status(state: &MarketplaceViewState) -> Element {
    let mut children = vec![Element::row(vec![
        text(
            if state.update_checking {
                "Buscando actualizaciones…"
            } else if state.updates_checked
                && state.update_error.is_none()
                && state.updates.is_empty()
            {
                "Sin actualizaciones disponibles"
            } else {
                "Gestiona tus skills instaladas"
            },
            theme::FOREGROUND_SUBTLE,
            11.0,
        )
        .flex(1.0),
        IconButton::new(
            "marketplace_check_updates",
            Icon::Refresh,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .tooltip("Buscar actualizaciones")
        .tooltip_placement(TooltipPlacement::Below)
        .loading(state.update_checking)
        .enabled(!state.update_checking && !state.busy)
        .render(),
    ])
    .gap(6.0)];
    if let Some(error) = &state.update_error {
        children.push(
            Alert::new(
                "No se pudo comprobar el catálogo",
                error,
                theme::component_palette(),
            )
            .tone(AlertTone::Error)
            .compact(true)
            .render(),
        );
    }
    Element::column(children).gap(8.0)
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

#[cfg(test)]
fn matches_query(item: &MarketplaceItem, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || item.name.to_lowercase().contains(&query)
        || item.description.to_lowercase().contains(&query)
        || item.source_name.to_lowercase().contains(&query)
}

#[cfg(test)]
fn item_card(item: &MarketplaceItem, width: f32, busy: bool) -> Element {
    item_card_content(item, width, busy, false, None)
}

fn item_card_content(
    item: &MarketplaceItem,
    width: f32,
    busy: bool,
    detailed: bool,
    state: Option<&MarketplaceViewState>,
) -> Element {
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
        marker.centered_y(),
        Element::column(vec![
            Tooltip::new(
                clipped(&item.name, theme::FOREGROUND, 14.0),
                format!("marketplace_name:{}", item.id),
                "Skill",
                theme::component_palette(),
            )
            .description(&item.name)
            .render(),
            clipped(
                format!("{} · {}", item.source_name, source_label(item.source_kind)),
                theme::FOREGROUND_SUBTLE,
                11.0,
            ),
        ])
        .gap(3.0)
        .flex(1.0),
        (if detailed {
            Element::spacer()
        } else {
            IconButton::new(
                format!("marketplace_details:{}", item.id),
                Icon::InfoCircle,
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .tooltip("Detalles de la skill")
            .render()
        })
        .centered_y(),
    ])
    .gap(8.0);

    let mut details = vec![header, skill_description(item, detailed)];
    if !item.source_available {
        details.push(text(
            "El origen ya no está disponible",
            theme::DESTRUCTIVE,
            12.0,
        ));
    }
    let bottom = if let Some(scope) = item.installed_scope {
        let badge = Chip::new(
            format!("Instalada · {}", scope_label(scope)),
            theme::component_palette(),
        )
        .render();
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
        .enabled(!busy)
        .tooltip_with(TooltipSpec::new("Desinstalar skill").placement(TooltipPlacement::Above))
        .render();
        let activation = Element::row(vec![
            text(
                if item.enabled { "Activa" } else { "Inactiva" },
                theme::FOREGROUND_SUBTLE,
                11.0,
            )
            .centered_y(),
            toggle.centered_y(),
        ])
        .gap(8.0)
        .size(None, Some(ControlSize::Regular.height()));
        if width < 380.0 {
            Element::column(vec![
                Element::row(vec![badge, Element::spacer().flex(1.0)]),
                Element::row(vec![
                    activation,
                    Element::spacer().flex(1.0),
                    uninstall.centered_y(),
                ])
                .gap(8.0)
                .size(None, Some(ControlSize::Regular.height())),
            ])
            .gap(8.0)
        } else {
            Element::row(vec![
                badge.centered_y(),
                Element::spacer().flex(1.0),
                activation,
                uninstall.centered_y(),
            ])
            .gap(10.0)
            .size(None, Some(ControlSize::Regular.height()))
        }
    } else {
        let license = item
            .license
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("Licencia no declarada");
        let license = Tooltip::new(
            clipped(license, theme::FOREGROUND_SUBTLE, 11.0),
            format!("marketplace_license:{}", item.id),
            "Licencia",
            theme::component_palette(),
        )
        .description(license)
        .render()
        .flex(1.0);
        let install = Button::new(
            format!("marketplace_install:{}", item.id),
            "Instalar",
            theme::component_palette(),
        )
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .width(INSTALL_WIDTH)
        .enabled(!busy && item.source_available)
        .render();
        Element::row(vec![license.centered_y(), install.centered_y()])
            .gap(8.0)
            .size(None, Some(ControlSize::Compact.height()))
    };
    if let Some(update) = state
        .and_then(|state| state.updates.get(&item.id))
        .filter(|_| item.installed_scope.is_some())
    {
        details.push(
            Element::row(vec![
                clipped("Actualización disponible", theme::FOREGROUND_SUBTLE, 11.0).flex(1.0),
                Tooltip::new(
                    Button::new(
                        format!("marketplace_update:{}", item.id),
                        "Actualizar",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .width(INSTALL_WIDTH)
                    .enabled(!busy)
                    .render(),
                    format!("marketplace_update:{}", item.id),
                    "Actualizar skill",
                    theme::component_palette(),
                )
                .description(format!(
                    "De {} a {}",
                    update
                        .installed_revision
                        .as_deref()
                        .unwrap_or("la versión instalada"),
                    update.available_revision
                ))
                .render(),
            ])
            .gap(8.0),
        );
    }
    details.push(bottom);
    Element::panel(Element::column(details).gap(8.0))
        .padding(CARD_PADDING)
        .background(theme::PANEL)
        .border(theme::BORDER, 1.0)
        .radius(8.0)
        .size(Some(width), None)
}

fn skill_description(item: &MarketplaceItem, detailed: bool) -> Element {
    // The complete instructions remain in Details; a catalog row must not
    // expand to the height of an arbitrary SKILL.md description.
    let mut chars = item.description.char_indices();
    let cutoff = chars.nth(180).map(|(offset, _)| offset);
    let preview = if !detailed {
        cutoff.map(|offset| format!("{}…", item.description[..offset].trim_end()))
    } else {
        None
    };
    let body = text(
        preview.as_deref().unwrap_or(&item.description),
        theme::FOREGROUND_SUBTLE,
        12.0,
    );
    if preview.is_some() {
        Tooltip::new(
            body,
            format!("marketplace_description:{}", item.id),
            "Descripción",
            theme::component_palette(),
        )
        .description(&item.description)
        .render()
    } else {
        body
    }
}

fn item_grid(
    items: &[Cow<'_, MarketplaceItem>],
    width: f32,
    state: &MarketplaceViewState,
) -> Element {
    // A lone card keeps the whole row.
    let grid = CardGrid::new(width, 740.0).columns(items.len().min(2));
    let card_width = grid.item_width();
    grid.items(
        items
            .iter()
            .map(|item| item_card_content(item, card_width, state.busy, false, Some(state)))
            .collect(),
    )
    .render()
}

fn source_form_body(state: &MarketplaceViewState) -> Element {
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
        .size(ControlSize::Compact)
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
            Alert::new("Aviso de la fuente", status, theme::component_palette())
                .tone(if state.status_error {
                    AlertTone::Error
                } else {
                    AlertTone::Neutral
                })
                .compact(true)
                .render(),
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
        theme::primary("marketplace_source_submit", "Agregar origen")
            .size(ControlSize::Compact)
            .enabled(!state.busy && !state.source_draft.trim().is_empty())
            .loading(state.busy)
            .render(),
    ])
    .gap(8.0)
}

fn source_list(state: &MarketplaceViewState, width: f32) -> Element {
    let mut rows = vec![SectionHeader::new("Orígenes", theme::component_palette())
        .count(state.snapshot.sources.len())
        .render()];
    for source in &state.snapshot.sources {
        rows.push(
            Element::row(vec![
                Element::icon(match source.kind {
                    SourceKind::Local => Icon::Folder,
                    SourceKind::Github => Icon::Github,
                })
                .foreground(theme::FOREGROUND_SUBTLE)
                .centered_y(),
                Element::column(vec![
                    clipped(&source.name, theme::FOREGROUND, 13.0),
                    clipped(&source.location, theme::FOREGROUND_SUBTLE, 11.0),
                ])
                .gap(3.0)
                .flex(1.0),
                IconButton::new(
                    format!("marketplace_remove_source:{}", source.id),
                    Icon::Trash,
                    theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .enabled(!state.busy)
                .tooltip_with(TooltipSpec::new("Quitar origen").placement(TooltipPlacement::Above))
                .render()
                .centered_y(),
            ])
            .gap(10.0)
            .padding(12.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(10.0)
            .min_size(0.0, 56.0),
        );
    }
    Element::column(rows).gap(6.0).size(Some(width), None)
}

fn empty_catalog(state: &MarketplaceViewState) -> Element {
    let (heading, description) = if state.busy {
        ("Cargando catálogo", "Consultando las skills disponibles…")
    } else if !state.active_query().trim().is_empty() {
        ("Sin resultados", "Prueba otra búsqueda o cambia el filtro.")
    } else if state.active_tab() == MarketplaceTab::Installed {
        (
            "Todavía no instalaste skills",
            "Elige una del catálogo y dónde instalarla.",
        )
    } else if state.snapshot.sources.is_empty() {
        (
            "Explora skills",
            "Actualiza el catálogo o agrega un origen con archivos SKILL.md.",
        )
    } else {
        (
            "Sin skills disponibles",
            "Actualiza el catálogo o revisa los orígenes.",
        )
    };
    let mut notice = Notice::new(
        heading,
        openide_ui::ComponentPalette {
            surface: theme::PANEL,
            ..theme::component_palette()
        },
    )
    .icon(Icon::Grid)
    .description(description)
    .framed();
    if state.active_tab() == MarketplaceTab::Installed
        && state.query.trim().is_empty()
        && !state.busy
    {
        notice = notice.action(Element::row(vec![
            Button::new(
                "marketplace_empty_all",
                "Explorar catálogo",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render()
            .dispatch_as("marketplace_tab:recommended"),
            Element::spacer().flex(1.0),
        ]));
    }
    notice.render()
}

#[cfg(test)]
fn legacy_empty_catalog(state: &MarketplaceViewState) -> Element {
    let (heading, description) = if state.busy {
        ("Cargando catálogo", "Consultando las skills disponibles…")
    } else if !state.active_query().trim().is_empty() {
        ("Sin resultados", "Prueba otra búsqueda o cambia el filtro.")
    } else if state.active_tab() == MarketplaceTab::Installed {
        (
            "Todavía no instalaste skills",
            "Elige una del catálogo y dónde instalarla.",
        )
    } else if state.snapshot.sources.is_empty() {
        (
            "Explora skills",
            "Actualiza el catálogo o agrega un origen con archivos SKILL.md.",
        )
    } else {
        (
            "Sin skills disponibles",
            "Actualiza el catálogo o revisa los orígenes.",
        )
    };
    let mut copy = vec![
        text(heading, theme::FOREGROUND, 14.0),
        text(description, theme::FOREGROUND_SUBTLE, 12.0),
    ];
    if state.active_tab() == MarketplaceTab::Installed
        && state.query.trim().is_empty()
        && !state.busy
    {
        copy.push(Element::row(vec![
            Button::new(
                "marketplace_empty_all",
                "Explorar catálogo",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render()
            .dispatch_as("marketplace_tab:recommended"),
            Element::spacer().flex(1.0),
        ]));
    }
    Element::row(vec![
        Element::icon(Icon::Grid)
            .size(Some(20.0), Some(20.0))
            .foreground(theme::FOREGROUND_SUBTLE),
        Element::column(copy).gap(6.0).flex(1.0),
    ])
    .gap(12.0)
    .padding(16.0)
    .background(theme::PANEL)
    .radius(8.0)
}

fn detail_field(label: &str, value: &str) -> Element {
    Element::column(vec![
        text(label, theme::FOREGROUND_SUBTLE, 11.0),
        text(value, theme::FOREGROUND, 12.0),
    ])
    .gap(3.0)
}

fn detail_body(state: &MarketplaceViewState, item: &MarketplaceItem, description: bool) -> Element {
    let mut body = Vec::new();
    if let Some(status) = state
        .status
        .as_deref()
        .filter(|value| *value != "Catálogo cargado")
    {
        body.push(
            Alert::new("Marketplace", status, theme::component_palette())
                .tone(if state.status_error {
                    AlertTone::Error
                } else {
                    AlertTone::Neutral
                })
                .compact(true)
                .render(),
        );
    }
    if description {
        body.push(skill_description(item, true));
    }
    body.push(Separator::horizontal(theme::component_palette()).render());
    if let Some(details) = crate::marketplace::starter_details(&item.id) {
        body.extend([
            detail_field("Origen", &details.repository_url),
            Element::row(vec![
                Button::new(
                    format!("marketplace_open_source:{}", item.id),
                    "Abrir origen",
                    theme::component_palette(),
                )
                .icon(Icon::Github)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
                Element::spacer().flex(1.0),
            ]),
            detail_field("Revisión fijada", &details.revision),
            detail_field("Carpeta", &details.directory),
            detail_field("Licencia", &details.license),
            detail_field("Compatibilidad", &details.compatibility),
            text(
                format!(
                    "{} archivos · {} KiB de descarga",
                    details.file_count,
                    details.download_bytes.div_ceil(1024)
                ),
                theme::FOREGROUND_SUBTLE,
                11.0,
            ),
        ]);
    } else {
        let source = state
            .snapshot
            .sources
            .iter()
            .find(|source| item.id.starts_with(&format!("{}:", source.id)));
        body.push(detail_field(
            "Origen",
            source.map_or(item.source_name.as_str(), |source| source.location.as_str()),
        ));
        body.push(detail_field(
            "Licencia",
            item.license
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("No declarada por el origen"),
        ));
        body.push(text(
            "Las instrucciones se copian a la ubicación de instalación elegida.",
            theme::FOREGROUND_SUBTLE,
            11.0,
        ));
    }
    if let Some(remote) = state
        .explore
        .page
        .as_ref()
        .and_then(|page| page.skills.iter().find(|skill| skill.id == item.id))
    {
        body.push(detail_field("Descubierta en", "skills.sh"));
        body.push(text(
            format!(
                "{} instalaciones registradas en el catálogo",
                remote.installs
            ),
            theme::FOREGROUND_SUBTLE,
            11.0,
        ));
        body.push(Element::row(vec![
            Button::new(
                format!("marketplace_open_source:{}", item.id),
                "Abrir origen",
                theme::component_palette(),
            )
            .icon(Icon::Github)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
            Element::spacer().flex(1.0),
        ]));
    }
    if let Some(provenance) = state.provenance.get(&item.id) {
        body.extend([
            detail_field("Repositorio instalado", &provenance.repository),
            detail_field("Revisión instalada", &provenance.revision),
            detail_field("Carpeta instalada", &provenance.directory),
        ]);
    }
    if let Some(update) = state.updates.get(&item.id) {
        if !state.provenance.contains_key(&item.id) {
            body.push(detail_field(
                "Revisión instalada",
                update
                    .installed_revision
                    .as_deref()
                    .unwrap_or("Sin revisión registrada"),
            ));
        }
        body.push(detail_field(
            "Revisión disponible",
            &update.available_revision,
        ));
        body.push(text("Actualizar reemplaza la copia instalada. La skill conserva su ubicación y su estado de activación.", theme::FOREGROUND_SUBTLE, 11.0));
    }
    if !item.source_available {
        body.push(text(
            "El origen ya no está disponible",
            theme::DESTRUCTIVE,
            12.0,
        ));
    }
    Element::column(body).gap(10.0)
}

fn detail_footer(state: &MarketplaceViewState, item: &MarketplaceItem) -> Element {
    if let Some(scope) = item.installed_scope {
        let mut rows = vec![
            text(
                format!("Instalada · {}", scope_label(scope)),
                theme::FOREGROUND_SUBTLE,
                11.0,
            ),
            Element::row(vec![
                Button::new(
                    format!("marketplace_details_toggle:{}", item.id),
                    if item.enabled {
                        "Desactivar"
                    } else {
                        "Activar"
                    },
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(!state.busy)
                .render()
                .dispatch_as(format!("marketplace_toggle:{}", item.id)),
                Element::spacer().flex(1.0),
                Button::new(
                    format!("marketplace_details_uninstall:{}", item.id),
                    "Desinstalar",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(!state.busy)
                .render()
                .dispatch_as(format!("marketplace_uninstall:{}", item.id)),
            ])
            .gap(8.0),
        ];
        if state.updates.contains_key(&item.id) {
            rows.push(Element::row(vec![
                Element::spacer().flex(1.0),
                Button::new(
                    format!("marketplace_details_update:{}", item.id),
                    "Actualizar",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .width(INSTALL_WIDTH)
                .enabled(!state.busy)
                .render()
                .dispatch_as(format!("marketplace_update:{}", item.id)),
            ]));
        }
        return Element::column(rows).gap(6.0);
    }
    Element::column(vec![
        text("Instalar en", theme::FOREGROUND_SUBTLE, 11.0),
        Element::row(vec![
            Button::new(
                "marketplace_details_scope_user",
                "Usuario",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .active(state.install_scope == InstallScope::User)
            .enabled(!state.busy)
            .render()
            .dispatch_as("marketplace_scope_user"),
            Button::new(
                "marketplace_details_scope_workspace",
                "Proyecto",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .active(state.install_scope == InstallScope::Workspace)
            .enabled(!state.busy)
            .render()
            .dispatch_as("marketplace_scope_workspace"),
            Element::spacer().flex(1.0),
        ])
        .gap(4.0),
        Element::row(vec![
            Element::spacer().flex(1.0),
            Button::new(
                format!("marketplace_details_install:{}", item.id),
                "Instalar",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .width(INSTALL_WIDTH)
            .enabled(!state.busy && item.source_available)
            .render()
            .dispatch_as(format!("marketplace_install:{}", item.id)),
        ]),
    ])
    .gap(6.0)
}

/// Build the Marketplace's scrollable center-dock surface. The host owns all
/// state changes and should refresh the snapshot after a completed operation.
/// The IDE detail reuses the same installation controls and confirmation routes
/// as the full catalog while leaving the sidebar query and editor intact.
pub(crate) fn ide_item_detail(
    state: &MarketplaceViewState,
    item_id: &str,
    available_width: f32,
) -> Element {
    let Some(item) = state.find_item(item_id) else {
        return marketplace_page(state, available_width);
    };
    let width = (available_width - 24.0).max(0.0).min(760.0);
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
    let body = Element::column(vec![
        Button::new(
            "ide_extensions_catalog",
            "Todos los complementos",
            theme::component_palette(),
        )
        .icon(Icon::ArrowLeft)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render(),
        text(
            "Skill para el agente nativo",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ),
        Element::row(vec![
            text("Instalar en", theme::FOREGROUND_SUBTLE, 11.0),
            scope,
        ])
        .gap(8.0),
        item_card_content(&item, width, state.busy, true, Some(state)),
        detail_body(state, &item, false),
    ])
    .gap(12.0)
    .size(Some(width), None);
    ScrollArea::new("ide_extension_detail", body.centered_x().padding(12.0))
        .render()
        .flex(1.0)
}

pub fn marketplace_page(state: &MarketplaceViewState, available_width: f32) -> Element {
    marketplace_page_with_navigation(state, available_width, None)
}

/// The host can place its sidebar control in the same header row, keeping the
/// title and catalog clear of independent top-left overlays.
pub(crate) fn marketplace_page_with_navigation(
    state: &MarketplaceViewState,
    available_width: f32,
    navigation: Option<Element>,
) -> Element {
    catalog_surface(state, available_width, navigation, false)
}

/// In the IDE the visible sidebar owns query, filters and list navigation.
/// A hidden sidebar leaves the full catalog controls available in the center.
pub(crate) fn ide_catalog_page(
    state: &MarketplaceViewState,
    available_width: f32,
    sidebar_controls_visible: bool,
) -> Element {
    catalog_surface(state, available_width, None, sidebar_controls_visible)
}

fn page_header(navigation: Option<Element>, width: f32, refresh: Element, add: Element) -> Element {
    // Beside the host's sidebar control a narrow title gives up two pixels.
    let sizing = if navigation.is_some() && width < 320.0 {
        MARKETPLACE_HEADER_NARROW
    } else {
        MARKETPLACE_HEADER
    };
    let mut header = PageHeader::new("Marketplace", theme::component_palette())
        .description("Skills para tu agente")
        .trailing(Element::row(vec![refresh.centered_y(), add.centered_y()]).gap(sizing.row_gap))
        .truncate(true)
        .sizing(sizing);
    if let Some(navigation) = navigation {
        header = header.leading(navigation);
    }
    header.render()
}

fn catalog_surface(
    state: &MarketplaceViewState,
    available_width: f32,
    navigation: Option<Element>,
    sidebar_controls_visible: bool,
) -> Element {
    let page = PageColumn::new(
        "marketplace_page",
        available_width,
        theme::component_palette(),
    )
    .reserve_rail(true);
    let width = page.content_width();
    let compact = width < 600.0;
    let (items, total, page_index) = catalog_page(state);
    let explore = state.active_tab() == MarketplaceTab::Explore;
    let installed = state.active_tab() == MarketplaceTab::Installed;

    let refresh = IconButton::new(
        "marketplace_refresh",
        Icon::Refresh,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .enabled(!state.busy && !state.explore.busy)
    .loading(if explore {
        state.explore.busy
    } else {
        state.busy && state.add_source_kind.is_none()
    })
    .render()
    .dispatch_as(if explore {
        "marketplace_explore_retry"
    } else {
        "marketplace_refresh"
    });
    let refresh = Tooltip::new(
        refresh,
        "marketplace_refresh",
        if explore {
            "Actualizar búsqueda"
        } else {
            "Actualizar catálogo"
        },
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render();
    let add = if width < 430.0 {
        IconButton::new(
            "marketplace_add_menu",
            Icon::Add,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Regular)
        .tooltip("Agregar origen")
        .tooltip_placement(TooltipPlacement::Below)
        .enabled(!state.busy)
        .render()
    } else {
        Button::new(
            "marketplace_add_menu",
            "Agregar origen",
            theme::component_palette(),
        )
        .icon(Icon::Add)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Regular)
        .width(136.0)
        .enabled(!state.busy)
        .render()
    };
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
    .context_menu_style()
    .width(200.0)
    .render();
    let header = page_header(navigation, width, refresh, add);

    let tabs = tab_strip(state, false);
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

    let search = search_controls(state, width, false);
    let scope_row = if installed {
        Element::spacer().size(None, Some(0.0))
    } else {
        Element::row(vec![
            clipped("Instalar en", theme::FOREGROUND_SUBTLE, 10.0).centered_y(),
            Element::spacer().flex(1.0),
            scope,
        ])
        .gap(8.0)
        .size(None, Some(28.0))
    };
    let mut sections = vec![header];
    if sidebar_controls_visible {
        if !installed {
            sections.push(scope_row);
        }
    } else {
        let filters = if compact || installed {
            Element::column(vec![
                Element::row(vec![tabs, Element::spacer().flex(1.0)]),
                scope_row,
            ])
            .gap(if installed { 0.0 } else { 6.0 })
        } else {
            Element::row(vec![
                tabs,
                Element::spacer().flex(1.0),
                scope_row.size(Some(228.0), None),
            ])
            .gap(8.0)
            .size(None, Some(28.0))
        };
        sections.push(filters);
        sections.push(search);
    }
    match state.active_tab() {
        MarketplaceTab::Recommended => sections.push(Element::row(vec![
            category_control(state),
            Element::spacer().flex(1.0),
        ])),
        MarketplaceTab::Explore => sections.push(explore_status(state, width)),
        MarketplaceTab::Installed => sections.push(update_status(state)),
    }
    sections.push(Separator::horizontal(theme::component_palette()).render());
    if let Some(status) = state
        .status
        .as_deref()
        .filter(|value| *value != "Catálogo cargado" && state.add_source_kind.is_none())
    {
        sections.push(
            Alert::new("Aviso de Marketplace", status, theme::component_palette())
                .tone(if state.status_error {
                    AlertTone::Error
                } else {
                    AlertTone::Neutral
                })
                .compact(true)
                .render(),
        );
    }
    for diagnostic in &state.snapshot.diagnostics {
        sections.push(
            Alert::new(
                "Diagnóstico del catálogo",
                diagnostic,
                theme::component_palette(),
            )
            .tone(AlertTone::Error)
            .compact(true)
            .render(),
        );
    }
    if items.is_empty() {
        // The initial Explore prompt and loading skeleton already explain the
        // empty surface; do not stack a contradictory catalog empty state.
        if !explore || (state.explore.page.is_some() && !state.explore.busy) {
            sections.push(empty_catalog(state));
        }
    } else {
        sections.push(
            SectionHeader::new(
                match state.active_tab() {
                    MarketplaceTab::Recommended => "Recomendadas",
                    MarketplaceTab::Explore => "Resultados",
                    MarketplaceTab::Installed => "Instaladas",
                },
                theme::component_palette(),
            )
            .count(total)
            .render(),
        );
        sections.push(pagination_controls(total, page_index, false));
        sections.push(item_grid(&items, width, state));
    }
    if state.active_tab() == MarketplaceTab::Recommended && !state.snapshot.sources.is_empty() {
        sections.push(Separator::horizontal(theme::component_palette()).render());
        sections.push(source_list(state, width));
    }
    let page = page.children(sections).render();
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
        .body(source_form_body(state))
        .footer(source_form_footer(state))
        .initial_focus("marketplace_source_draft")
        .return_focus("marketplace_add_menu")
        .backdrop_dismiss(!state.busy)
        .render()
    } else if let Some(item) = state
        .selected_item_id
        .as_deref()
        .and_then(|id| state.find_item(id))
    {
        Modal::new(
            page,
            true,
            &item.name,
            &item.source_name,
            "marketplace_details_close",
            theme::component_palette(),
        )
        .width((available_width - 24.0).clamp(240.0, 560.0))
        .height(560.0)
        .body(detail_body(state, &item, true))
        .footer(detail_footer(state, &item))
        .initial_focus("marketplace_details_close:close")
        .return_focus(format!("marketplace_details:{}", item.id))
        .render()
    } else {
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::MarketplaceSource;
    use openide_ui::{DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn native_tree(root: Element) -> UiTree {
        native_tree_with_context(root).1
    }

    fn native_tree_with_context(root: Element) -> (eframe::egui::Context, UiTree) {
        use eframe::egui;
        use openide_ui::FontMetrics;
        let context = egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(egui::RawInput::default());
        let mut tree = UiTree::new(root);
        tree.set_reduced_motion(true);
        let mono = egui::FontId::monospace(14.0);
        tree.set_metrics(context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts.glyph_width(&mono, 'M'),
            line_height: fonts.row_height(&mono),
        }));
        let sans = egui::FontId::proportional(14.0);
        tree.set_sans_metrics(context.fonts_mut(|fonts| {
            FontMetrics {
                char_width: "abcdefghijklmnopqrstuvwxyz"
                    .chars()
                    .map(|character| fonts.glyph_width(&sans, character))
                    .sum::<f32>()
                    / 26.0,
                line_height: fonts.row_height(&sans),
            }
        }));
        let measurer = context.clone();
        tree.set_text_measurer(move |text, size, family| {
            measurer.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::new(
                            size,
                            match family {
                                FontFamily::Sans => egui::FontFamily::Proportional,
                                FontFamily::Monospace => egui::FontFamily::Monospace,
                            },
                        ),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        });
        (context, tree)
    }

    fn painted_header_title(
        frame: &openide_ui::Frame,
        context: &eframe::egui::Context,
        minimum_x: f32,
    ) -> openide_ui::Rect {
        use eframe::egui;
        // The bounded title is TruncatedText, so inspect its actual paint
        // command rather than the selectable-paragraph diagnostic registry.
        let titles = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text {
                    position,
                    text,
                    font_size,
                    font_family,
                    ..
                } if text.starts_with("Market") && position.x >= minimum_x => {
                    Some((*position, text, *font_size, *font_family))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(titles.len(), 1, "the header paints exactly one title");
        let (position, text, size, family) = titles[0];
        assert_eq!(text, "Marketplace", "the title fits without truncation");
        let galley = context.fonts_mut(|fonts| {
            fonts.layout_no_wrap(
                text.clone(),
                egui::FontId::new(
                    size,
                    match family {
                        FontFamily::Sans => egui::FontFamily::Proportional,
                        FontFamily::Monospace => egui::FontFamily::Monospace,
                    },
                ),
                egui::Color32::WHITE,
            )
        });
        assert_eq!(galley.rows.len(), 1, "the native title stays on one line");
        openide_ui::Rect::new(position.x, position.y, galley.size().x, galley.size().y)
    }

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
    fn add_source_menu_keeps_its_button_and_catalog_geometry_stable() {
        for width in [320.0, 560.0, 920.0] {
            let mut state = sample_state();
            let mut tree = UiTree::new(marketplace_page(&state, width));
            tree.set_reduced_motion(true);
            let closed = tree.layout(Size::new(width, 900.0));
            let rect = |frame: &openide_ui::Frame, id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect
            };
            let button = rect(&closed, "marketplace_add_menu");
            assert_eq!(button.height, ControlSize::Regular.height());
            let search = rect(&closed, "marketplace_search");
            state.add_menu_open = true;
            tree.set_root(marketplace_page(&state, width));
            let opened = tree.layout(Size::new(width, 900.0));
            assert_eq!(
                rect(&opened, "marketplace_add_menu"),
                button,
                "width={width}"
            );
            assert_eq!(rect(&opened, "marketplace_search"), search);
            let option = rect(&opened, "marketplace_source_kind:github");
            let point = Point::new(
                option.x + option.width / 2.0,
                option.y + option.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "marketplace_source_kind:github".into()
                }]
            );
            state.add_menu_open = false;
            tree.set_root(marketplace_page(&state, width));
            let closed_again = tree.layout(Size::new(width, 900.0));
            assert_eq!(rect(&closed_again, "marketplace_add_menu"), button);
        }
    }

    #[test]
    fn installed_switch_keeps_standard_track_thumb_and_real_state() {
        let id = "marketplace_toggle:source-1:git-review";
        for width in [280.0, 440.0, 900.0] {
            for checked in [false, true] {
                let mut item = sample_state().snapshot.items.remove(0);
                item.installed_scope = Some(InstallScope::Workspace);
                item.enabled = checked;
                let mut tree = UiTree::new(item_card(&item, width, false));
                let frame = tree.layout(Size::new(width, 600.0));
                let track = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert_eq!((track.width, track.height), (36.0, 20.0));
                let thumb = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::FillRect { rect, radius, .. }
                            if rect.width == 14.0 && rect.height == 14.0 && *radius == 7.0 =>
                        {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .expect("standard switch thumb");
                assert!((thumb.x - track.x - if checked { 19.0 } else { 3.0 }).abs() < 0.1);
                assert!((thumb.y - track.y - 3.0).abs() < 0.1);
                assert!(frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == if checked { "Activa" } else { "Inactiva" }
                    )));
                let uninstall = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "marketplace_uninstall:source-1:git-review")
                    .unwrap()
                    .rect;
                assert!((track.y + 10.0 - uninstall.y - uninstall.height / 2.0).abs() < 0.1);
                assert!(track.x >= 0.0 && uninstall.x + uninstall.width <= width);
                let point = Point::new(track.x + 18.0, track.y + 10.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: id.into() }]
                );
                assert!(tree.focus(id));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked { id: id.into() }]
                );

                tree.set_root(item_card(&item, width, true));
                let busy = tree.layout(Size::new(width, 600.0));
                assert!(!busy
                    .regions
                    .iter()
                    .any(|region| region.id == id && region.kind == HitKind::Button));
                assert!(!tree.focus(id));
            }
        }
    }

    #[test]
    fn installed_scope_chips_keep_the_complete_label_on_one_line() {
        for width in [280.0, 440.0] {
            for scope in [InstallScope::User, InstallScope::Workspace] {
                let mut item = sample_state().snapshot.items.remove(0);
                item.installed_scope = Some(scope);
                let frame =
                    UiTree::new(item_card(&item, width, false)).layout(Size::new(width, 400.0));
                let expected = format!("Instalada · {}", scope_label(scope));
                let blocks = frame.text_layouts();
                let copy = blocks
                    .iter()
                    .find(|block| block.source == expected)
                    .unwrap();
                assert_eq!(copy.runs.len(), 1, "{expected} at {width}px");
                assert_eq!(copy.runs[0].text, expected);
                let pill = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::FillRect {
                            rect,
                            radius,
                            color,
                        } if *radius == 11.0 && *color == theme::component_palette().selected => {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .unwrap();
                let text = copy.runs[0].rect;
                assert!(text.x >= pill.x && text.x + text.width <= pill.x + pill.width + 0.1);
                assert!(text.y >= pill.y && text.y + text.height <= pill.y + pill.height + 0.1);
                assert_eq!(pill.height, 22.0);
            }
        }
    }

    // The hand-built shells limn's `PageHeader`, `SectionHeader` and
    // `CardGrid` replaced. Delete with the final audit (S18).
    fn legacy_section_heading(label: &str, count: usize) -> Element {
        Element::row(vec![
            text(label, theme::FOREGROUND, 15.0).centered_y(),
            Chip::new(count.to_string(), theme::component_palette())
                .render()
                .centered_y(),
            Element::spacer().flex(1.0),
        ])
        .gap(8.0)
        .size(None, Some(28.0))
    }

    fn legacy_page_header(
        navigation: Option<Element>,
        width: f32,
        refresh: Element,
        add: Element,
    ) -> Element {
        let has_navigation = navigation.is_some();
        let mut header_items = Vec::new();
        if let Some(navigation) = navigation {
            header_items.push(navigation.centered_y());
        }
        header_items.extend([
            Element::column(vec![
                clipped(
                    "Marketplace",
                    theme::FOREGROUND,
                    if has_navigation && width < 320.0 {
                        18.0
                    } else {
                        20.0
                    },
                ),
                clipped("Skills para tu agente", theme::FOREGROUND_SUBTLE, 11.0),
            ])
            .gap(2.0)
            .flex(1.0),
            refresh.centered_y(),
            add.centered_y(),
        ]);
        Element::row(header_items).gap(6.0).size(None, Some(40.0))
    }

    fn legacy_item_grid(
        items: &[Cow<'_, MarketplaceItem>],
        width: f32,
        state: &MarketplaceViewState,
    ) -> Element {
        let two_columns = items.len() > 1 && width >= 740.0;
        let card_width = if two_columns {
            (width - 8.0) / 2.0
        } else {
            width
        };
        let mut rows = Vec::new();
        for pair in items.chunks(if two_columns { 2 } else { 1 }) {
            let mut cards = pair
                .iter()
                .map(|item| item_card_content(item, card_width, state.busy, false, Some(state)))
                .collect::<Vec<_>>();
            if two_columns && cards.len() == 1 {
                cards.push(Element::spacer().size(Some(card_width), None));
            }
            rows.push(Element::row(cards).gap(8.0));
        }
        Element::column(rows).gap(8.0)
    }

    fn assert_same_frames(name: &str, widths: &[f32], build: impl Fn(f32) -> (Element, Element)) {
        for &width in widths {
            let frame = |element: Element| {
                let mut tree = native_tree(Element::column(vec![
                    element,
                    Element::spacer().flex(1.0),
                ]));
                tree.layout(Size::new(width, 900.0))
            };
            let (legacy, current) = build(width);
            assert_eq!(frame(current), frame(legacy), "{name} width={width}");
        }
    }

    #[test]
    fn limn_shells_paint_what_the_marketplace_helpers_painted() {
        for count in [0, 1, 7, 24, 1280] {
            assert_same_frames("section heading", &[200.0, 640.0], |_| {
                (
                    legacy_section_heading("Recomendadas", count),
                    SectionHeader::new("Recomendadas", theme::component_palette())
                        .count(count)
                        .render(),
                )
            });
        }
        let button = |id: &str| {
            IconButton::new(id, Icon::Refresh, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Regular)
                .render()
        };
        let add = || {
            Button::new("add", "Agregar origen", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Regular)
                .width(136.0)
                .render()
        };
        for navigation in [false, true] {
            assert_same_frames("page header", &[240.0, 300.0, 360.0, 900.0], |width| {
                let navigation = || navigation.then(|| button("navigation"));
                (
                    legacy_page_header(navigation(), width, button("refresh"), add()),
                    page_header(navigation(), width, button("refresh"), add()),
                )
            });
        }
        let mut state = sample_state();
        let first = state.snapshot.items[0].clone();
        for count in 1..=5 {
            state.snapshot.items = (0..count)
                .map(|index| MarketplaceItem {
                    id: format!("source-1:skill-{index}"),
                    name: format!("skill-{index}"),
                    ..first.clone()
                })
                .collect();
            let items: Vec<_> = state.snapshot.items.iter().map(Cow::Borrowed).collect();
            assert_same_frames("item grid", &[360.0, 739.0, 740.0, 932.0], |width| {
                (
                    legacy_item_grid(&items, width, &state),
                    item_grid(&items, width, &state),
                )
            });
        }
    }

    #[test]
    fn marketplace_counts_are_round_and_belong_to_their_heading() {
        let heading = SectionHeader::new("Skills", theme::component_palette()).count(1);
        let frame = UiTree::new(heading.render()).layout(Size::new(640.0, 28.0));
        let circle = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::FillRect {
                    rect,
                    radius,
                    color,
                } if *color == theme::component_palette().selected && *radius == 11.0 => {
                    Some(*rect)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!((circle.width, circle.height), (22.0, 22.0));
        assert_eq!(circle.y, 3.0);
        let layouts = frame.text_layouts();
        let heading = layouts
            .iter()
            .find(|block| block.source == "Skills")
            .unwrap();
        let right = heading
            .runs
            .iter()
            .map(|run| run.rect.x + run.rect.width)
            .fold(0.0, f32::max);
        assert!((circle.x - right - 8.0).abs() < 0.1);
        assert!(frame.regions.is_empty());
    }

    #[test]
    fn card_descriptions_wrap_and_license_does_not_push_install_outside() {
        for width in [280.0, 440.0] {
            let mut item = sample_state().snapshot.items.remove(0);
            item.description = "Encuentra e instala skills para resolver tareas del proyecto. Conserva las instrucciones completas y el contexto de cada origen para elegir la herramienta adecuada.".into();
            item.license = Some("Licencia con condiciones detalladas y un nombre muy extenso que debe ajustarse al ancho disponible".into());
            let frame = UiTree::new(
                Element::column(vec![
                    item_card(&item, width, false),
                    Element::button("following_card", "Siguiente").size(None, Some(28.0)),
                ])
                .gap(8.0),
            )
            .layout(Size::new(width, 1000.0));
            let layouts = frame.text_layouts();
            let body = layouts
                .iter()
                .find(|block| block.source == item.description)
                .unwrap();
            assert!(body.runs.len() > 1);
            assert!(body.runs.iter().all(|run| run.visible.is_some()
                && run.rect.x >= CARD_PADDING
                && run.rect.x + run.rect.width <= width - CARD_PADDING + 0.1));
            let install = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_install:source-1:git-review")
                .unwrap()
                .rect;
            assert_eq!(
                (install.width, install.height),
                (INSTALL_WIDTH, ControlSize::Compact.height())
            );
            assert!(install.x + install.width <= width - CARD_PADDING + 0.1);
            let following = frame
                .regions
                .iter()
                .find(|region| region.id == "following_card")
                .unwrap()
                .rect;
            assert!(following.y >= install.y + install.height + CARD_PADDING + 8.0 - 0.1);
        }
    }

    #[test]
    #[ignore = "export native marketplace controls and cards for manual visual review"]
    fn export_marketplace_visual_review() {
        let output = std::path::PathBuf::from(format!(
            "{}/../../target/marketplace-visual-qa",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&output).unwrap();
        let mut state = sample_state();
        state.snapshot.sources[0].name = "vercel-labs/skills".into();
        state.snapshot.sources[0].location = "https://github.com/vercel-labs/skills.git".into();
        state.snapshot.sources[0].kind = SourceKind::Github;
        state.snapshot.items[0].name = "find-skills".into();
        state.snapshot.items[0].source_name = "vercel-labs/skills".into();
        state.snapshot.items[0].source_kind = SourceKind::Github;
        state.snapshot.items[0].description = "Helps users discover and install agent skills when they ask questions like how do I do X, find a skill for X, or is there a skill that can help with a task.".into();
        state.snapshot.items[0].installed_scope = Some(InstallScope::User);
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        for width in [360.0, 920.0] {
            for open in [false, true] {
                state.add_menu_open = open;
                let mut tree = native_tree(
                    Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND),
                );
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 860.0));
                let path = output.join(format!("single-{width:.0}-menu-{open}.svg"));
                crate::settings_ui::visual_qa::write_frame_svg(&frame, &path, width, 860.0)
                    .unwrap();
                let parsed =
                    resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                        .unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 860).unwrap();
                resvg::render(
                    &parsed,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
                eprintln!("{}", path.display());
            }
        }
        state.add_menu_open = false;
        let mut active = state.snapshot.items[0].clone();
        active.id = "source-1:review-changes".into();
        active.name = "review-changes".into();
        active.description = "Revisa los cambios del proyecto, explica su impacto y verifica las comprobaciones antes de continuar.".into();
        active.enabled = true;
        active.installed_scope = Some(InstallScope::Workspace);
        state.snapshot.items.push(active);
        let mut available = state.snapshot.items[0].clone();
        available.id = "source-1:project-analysis".into();
        available.name = "project-analysis".into();
        available.description = "Organiza el contexto de un proyecto y prepara un análisis de sus módulos, dependencias y decisiones pendientes.".into();
        available.installed_scope = None;
        state.snapshot.items.push(available);
        for width in [360.0, 920.0] {
            let mut tree = native_tree(
                Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND),
            );
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 1100.0));
            let path = output.join(format!("multiple-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_frame_svg(&frame, &path, width, 1100.0).unwrap();
            let parsed =
                resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                    .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 1100).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            eprintln!("{}", path.display());
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let snapshot =
            crate::marketplace::MarketplaceStore::new(root.path().into(), workspace.path().into())
                .snapshot()
                .unwrap();
        for width in [280.0, 520.0, 1000.0] {
            for variant in [
                "empty",
                "starter",
                "details",
                "explore",
                "loading",
                "offline",
                "error",
                "updates",
                "update-details",
            ] {
                let mut fixture = MarketplaceViewState::default();
                if variant != "empty" {
                    fixture.snapshot = snapshot.clone();
                }
                if variant == "details" {
                    fixture.selected_item_id = Some("starter-anthropic:frontend-design".into());
                }
                if matches!(
                    variant,
                    "explore" | "loading" | "offline" | "error" | "updates" | "update-details"
                ) {
                    fixture = remote_state();
                    if variant == "loading" {
                        fixture.explore.page = None;
                        fixture.explore.busy = true;
                    }
                    if variant == "offline" {
                        let page = fixture.explore.page.as_mut().unwrap();
                        page.from_cache = true;
                        page.stale = true;
                        page.warning = Some(
                            "No hay conexión. Estos resultados se guardaron anteriormente.".into(),
                        );
                    }
                    if variant == "error" {
                        fixture.explore.page = None;
                        fixture.explore.error =
                            Some("No se pudo conectar con skills.sh. Intenta de nuevo.".into());
                    }
                    if matches!(variant, "updates" | "update-details") {
                        let mut item = fixture.explore.page.as_ref().unwrap().skills[0].item();
                        item.installed_scope = Some(InstallScope::Workspace);
                        item.enabled = true;
                        fixture.updates.insert(
                            item.id.clone(),
                            SkillUpdate {
                                item_id: item.id.clone(),
                                installed_revision: Some("a".repeat(40)),
                                available_revision: "b".repeat(40),
                            },
                        );
                        if variant == "update-details" {
                            fixture.selected_item_id = Some(item.id.clone());
                        }
                        fixture.snapshot.items.push(item);
                        fixture.tab = MarketplaceTab::Installed;
                    }
                }
                let mut tree = native_tree(
                    Element::panel(marketplace_page(&fixture, width)).background(theme::BACKGROUND),
                );
                let frame = tree.layout(Size::new(width, 800.0));
                let path = output.join(format!("{variant}-{width:.0}.svg"));
                crate::settings_ui::visual_qa::write_frame_svg(&frame, &path, width, 800.0)
                    .unwrap();
                let parsed =
                    resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                        .unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 800).unwrap();
                resvg::render(
                    &parsed,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
                eprintln!("{}", path.display());
            }
        }
    }

    #[test]
    fn source_notice_keeps_modal_submit_and_cancel_actions_reachable() {
        for width in [320.0, 920.0] {
            let mut state = sample_state();
            state.add_source_kind = Some(SourceKind::Local);
            state.source_draft = "C:/team-skills".into();
            state.status = Some("Comprueba la carpeta seleccionada antes de continuar.".into());
            let mut tree = UiTree::new(marketplace_page(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 360.0));
            for id in ["marketplace_source_cancel", "marketplace_source_submit"] {
                let control = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap_or_else(|| panic!("missing {id} at {width}px"));
                assert!(control.rect.x >= 0.0 && control.rect.y >= 0.0);
                assert!(control.rect.x + control.rect.width <= width);
                assert!(control.rect.y + control.rect.height <= 360.0);
                let point = Point::new(control.rect.x + 4.0, control.rect.y + 4.0);
                assert_eq!(frame.hit_test(point), Some(id));
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
        }
    }

    #[test]
    fn marketplace_exposes_real_source_install_and_scope_actions() {
        let state = sample_state();
        let frame = UiTree::new(marketplace_page(&state, 920.0)).layout(Size::new(920.0, 800.0));
        for id in [
            "marketplace_search",
            "marketplace_add_menu",
            "marketplace_tab:recommended",
            "marketplace_tab:installed",
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
        // The dialog is checked settled, not partway through its entrance.
        tree.set_reduced_motion(true);
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
    fn compact_refresh_keeps_an_explanatory_keyboard_tooltip() {
        let state = sample_state();
        let mut tree = UiTree::new(marketplace_page(&state, 920.0));
        tree.set_reduced_motion(true);
        let now = std::time::Instant::now();
        tree.layout_at(Size::new(920.0, 800.0), now);
        assert!(tree.focus("marketplace_refresh"));
        let frame = tree.layout_at(
            Size::new(920.0, 800.0),
            now + std::time::Duration::from_secs(1),
        );
        let button = frame
            .regions
            .iter()
            .find(|region| region.id == "marketplace_refresh")
            .unwrap();
        assert_eq!((button.rect.width, button.rect.height), (32.0, 32.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Actualizar catálogo"
        )));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "marketplace_refresh".into()
            }]
        );
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
    fn empty_catalog_matches_its_legacy_body() {
        let mut searching = MarketplaceViewState::default();
        searching.query = "pdf".into();
        let mut installed = MarketplaceViewState::default();
        installed.tab = MarketplaceTab::Installed;
        for state in [MarketplaceViewState::default(), searching, installed] {
            crate::state_panel_parity_tests::assert_same(
                "empty catalog",
                legacy_empty_catalog(&state),
                empty_catalog(&state),
            );
        }
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
    fn marketplace_centers_dock_with_room_for_the_reserved_scroll_rail() {
        let state = MarketplaceViewState::default();
        for (viewport_width, gutter) in [(920.0, 28.0), (320.0, 14.0)] {
            let frame = UiTree::new(marketplace_page(&state, viewport_width))
                .layout(Size::new(viewport_width, 800.0));
            let search = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_search")
                .unwrap();
            assert!(
                (search.rect.x - gutter - 14.0 / 2.0).abs() <= 1.0,
                "search at {} with viewport {viewport_width}",
                search.rect.x
            );
            assert!(
                (search.rect.width - (viewport_width - 14.0 - 2.0 * gutter)).abs()
                    <= 1.0
            );
        }
        let wide = UiTree::new(marketplace_page(&state, 1600.0)).layout(Size::new(1600.0, 800.0));
        let search = wide
            .regions
            .iter()
            .find(|region| region.id == "marketplace_search")
            .unwrap();
        assert!((search.rect.width - 960.0).abs() <= 1.0);
    }

    #[test]
    fn native_header_and_catalog_controls_are_compact_and_do_not_overlap() {
        for width in [280.0, 520.0, 1000.0, 1600.0] {
            let state = sample_state();
            let (context, mut tree) = native_tree_with_context(marketplace_page(&state, width));
            let frame = tree.layout(Size::new(width, 900.0));
            let region = |id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect
            };
            let add = region("marketplace_add_menu");
            let refresh = region("marketplace_refresh");
            let details = region("marketplace_details:source-1:git-review");
            let install = region("marketplace_install:source-1:git-review");
            assert_eq!((refresh.width, refresh.height), (32.0, 32.0));
            assert_eq!(add.height, 32.0);
            assert!(add.width <= 136.0);
            assert_eq!((details.width, details.height), (28.0, 28.0));
            assert_eq!((install.width, install.height), (INSTALL_WIDTH, 28.0));
            assert!((refresh.y - add.y).abs() < 0.1);
            assert!((refresh.x + refresh.width) <= add.x);
            let title = painted_header_title(&frame, &context, 0.0);
            assert!((title.x + title.width) <= refresh.x);
            for rect in [add, refresh, details, install] {
                assert!(rect.x >= 0.0 && (rect.x + rect.width) <= width && rect.y >= 0.0);
            }
            let point = Point::new(
                details.x + details.width / 2.0,
                details.y + details.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "marketplace_details:source-1:git-review".into()
                }]
            );
        }
    }

    #[test]
    fn standalone_header_integrates_one_sidebar_control_without_covering_its_title() {
        for (width, collapsed, expect_reopen) in [
            (320.0, false, true),
            // At 520 px the 176 px sidebar still fits beside the 320 px
            // minimum center. Its own toggle remains visible until collapsed.
            (520.0, false, false),
            (520.0, true, true),
            (1000.0, true, true),
            (1600.0, true, true),
            (1000.0, false, false),
        ] {
            let mut state = crate::view::tests::empty_state();
            state.main_page = crate::view::MainPage::Marketplace;
            state.show_side_pane = false;
            state.sidebar_collapsed = collapsed;
            state.marketplace = sample_state();
            let (context, mut tree) =
                native_tree_with_context(crate::view::build_view(&state, width));
            let frame = tree.layout(Size::new(width, 720.0));
            let controls = frame
                .regions
                .iter()
                .filter(|region| {
                    region.id == "toggle_sidebar_reopen" && region.kind == HitKind::Button
                })
                .collect::<Vec<_>>();
            assert_eq!(
                controls.len(),
                usize::from(expect_reopen),
                "width={width}, collapsed={collapsed}"
            );
            if !expect_reopen {
                assert!(
                    frame.regions.iter().any(|region| {
                        region.id == "toggle_sidebar" && region.kind == HitKind::Button
                    }),
                    "the visible sidebar keeps its own navigation control"
                );
                continue;
            }
            let reopen = controls[0].rect;
            let refresh = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_refresh")
                .unwrap()
                .rect;
            let search = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_search")
                .unwrap()
                .rect;
            let title = painted_header_title(&frame, &context, search.x);
            assert_eq!((reopen.width, reopen.height), (24.0, 24.0));
            assert!(
                (reopen.y + reopen.height / 2.0 - refresh.y - refresh.height / 2.0).abs() < 0.1
            );
            assert!(
                reopen.x + reopen.width + 5.0 <= title.x,
                "reopen={reopen:?}, title={title:?}"
            );
            assert!(title.x + title.width <= refresh.x);
            assert!(reopen.y + reopen.height <= search.y);
            let point = Point::new(
                reopen.x + reopen.width / 2.0,
                reopen.y + reopen.height / 2.0,
            );
            assert_eq!(frame.hit_test(point), Some("toggle_sidebar_reopen"));
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "toggle_sidebar_reopen".into()
                }]
            );
        }
        let state = sample_state();
        for width in [320.0, 520.0, 1000.0] {
            let without =
                native_tree(marketplace_page(&state, width)).layout(Size::new(width, 720.0));
            let with = native_tree(marketplace_page_with_navigation(
                &state,
                width,
                Some(crate::view::sidebar_reopen_control(theme::BACKGROUND)),
            ))
            .layout(Size::new(width, 720.0));
            let search = |frame: &openide_ui::Frame| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == "marketplace_search")
                    .unwrap()
                    .rect
            };
            assert_eq!(
                search(&without),
                search(&with),
                "navigation must not add a header row at{width}"
            );
        }
    }

    #[test]
    fn long_catalog_descriptions_are_bounded_but_details_keep_full_copy_and_real_actions() {
        for (width, height) in [(280.0, 360.0), (520.0, 600.0), (1000.0, 800.0)] {
            let mut state = sample_state();
            state.snapshot.items[0].description =
                "Revisa cambios y conserva contexto Unicode: café y 日本語. ".repeat(40);
            let mut tree = native_tree(marketplace_page(&state, width));
            let frame = tree.layout(Size::new(width, 900.0));
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|block| block.source == state.snapshot.items[0].description));
            let install = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_install:source-1:git-review")
                .unwrap()
                .rect;
            let card = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } if *color == theme::PANEL
                        && *radius == 8.0
                        && rect.contains(Point::new(install.x + 1.0, install.y + 1.0)) =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .unwrap();
            assert!(card.height < 240.0, "unbounded card: {card:?}");
            assert!(tree.focus("marketplace_details:source-1:git-review"));
            state.selected_item_id = Some("source-1:git-review".into());
            tree.set_root(marketplace_page(&state, width));
            let modal = tree.layout(Size::new(width, height));
            assert!(modal
                .text_layouts()
                .iter()
                .any(|block| block.source == state.snapshot.items[0].description));
            assert_eq!(tree.focused_id(), Some("marketplace_details_close:close"));
            for (id, action) in [
                (
                    "marketplace_details_scope_workspace",
                    "marketplace_scope_workspace",
                ),
                (
                    "marketplace_details_install:source-1:git-review",
                    "marketplace_install:source-1:git-review",
                ),
            ] {
                let rect = modal
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert!(
                    rect.x >= 0.0
                        && (rect.x + rect.width) <= width
                        && rect.y >= 0.0
                        && (rect.y + rect.height) <= height,
                    "{id}: {rect:?}"
                );
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: action.into() }]
                );
            }
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "marketplace_details_close".into()
                }]
            );
            state.selected_item_id = None;
            tree.set_root(marketplace_page(&state, width));
            tree.layout(Size::new(width, height));
            assert_eq!(
                tree.focused_id(),
                Some("marketplace_details:source-1:git-review")
            );
        }
    }

    #[test]
    fn offline_starter_cards_reveal_provenance_without_installing_or_adding_sources() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store =
            crate::marketplace::MarketplaceStore::new(root.path().into(), workspace.path().into());
        let snapshot = store.snapshot().unwrap();
        assert!(snapshot.sources.is_empty());
        assert!(snapshot.items.len() >= 30);
        let item = snapshot
            .items
            .iter()
            .find(|item| item.id == "starter-anthropic:frontend-design")
            .unwrap();
        let expected = crate::marketplace::starter_details(&item.id).unwrap();
        let mut state = MarketplaceViewState {
            snapshot: snapshot.clone(),
            ..Default::default()
        };
        let mut tree = native_tree(marketplace_page(&state, 520.0));
        tree.layout(Size::new(520.0, 800.0));
        state.selected_item_id = Some(item.id.clone());
        tree.set_root(marketplace_page(&state, 520.0));
        let frame = tree.layout(Size::new(520.0, 800.0));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|block| block.source == expected.repository_url));
        let source_id = format!("marketplace_open_source:{}", item.id);
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == source_id && region.kind == HitKind::Button));
        assert!(tree
            .focused_id()
            .is_some_and(|id| id == "marketplace_details_close:close"));
        assert!(store
            .snapshot()
            .unwrap()
            .items
            .iter()
            .all(|item| item.installed_scope.is_none()));
        assert!(store.snapshot().unwrap().sources.is_empty());
    }

    #[test]
    fn installed_empty_filter_has_one_distinct_catalog_action() {
        let mut state = sample_state();
        state.installed_only = true;
        state.tab = MarketplaceTab::Installed;
        let mut tree = native_tree(marketplace_page(&state, 360.0));
        let frame = tree.layout(Size::new(360.0, 700.0));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button
                    && region.id == "marketplace_tab:recommended")
                .count(),
            1
        );
        let action = frame
            .regions
            .iter()
            .find(|region| region.id == "marketplace_empty_all" && region.kind == HitKind::Button)
            .unwrap();
        let point = Point::new(
            action.rect.x + action.rect.width / 2.0,
            action.rect.y + action.rect.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "marketplace_tab:recommended".into()
            }]
        );
    }

    #[test]
    fn empty_catalog_stays_beneath_filters_instead_of_expanding_with_the_viewport() {
        for width in [280.0, 360.0, 920.0] {
            let state = MarketplaceViewState::default();
            let title_y = |height| {
                let frame =
                    native_tree(marketplace_page(&state, width)).layout(Size::new(width, height));
                let layouts = frame.text_layouts();
                let block = layouts
                    .iter()
                    .find(|block| block.source == "Explora skills")
                    .unwrap();
                let y = block.runs[0].rect.y;
                assert!(y < 300.0, "empty message at {y} for {width}px");
                let panel = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::FillRect {
                            rect,
                            radius,
                            color,
                        } if *radius == 8.0
                            && *color == theme::PANEL
                            && rect.contains(Point::new(block.runs[0].rect.x + 1.0, y + 1.0)) =>
                        {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(panel.height < 140.0, "oversized empty state: {panel:?}");
                y
            };
            assert!((title_y(600.0) - title_y(1200.0)).abs() < 0.1);
        }
    }
    fn remote_state() -> MarketplaceViewState {
        use crate::marketplace::RemoteSkill;
        MarketplaceViewState {
            tab: MarketplaceTab::Explore,
            explore: MarketplaceExploreState {
                query: "quality".into(),
                page: Some(RemoteSearchPage {
                    query: "quality".into(),
                    skills: vec![RemoteSkill {
                        id: "skills-sh:example/skills:quality".into(),
                        name: "quality".into(),
                        source: "example/skills".into(),
                        installs: 23,
                    }],
                    from_cache: false,
                    stale: false,
                    warning: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn catalog_pages_construct_only_twenty_four_items_and_clamp_after_filtering() {
        let mut state = sample_state();
        let prototype = state.snapshot.items[0].clone();
        state.snapshot.items = (0..1_500)
            .map(|index| MarketplaceItem {
                id: format!("source-1:skill-{index}"),
                name: format!("skill-{index:04}"),
                ..prototype.clone()
            })
            .collect();
        let (items, total, page) = catalog_page(&state);
        assert_eq!((items.len(), total, page), (24, 1_500, 0));
        assert_eq!(items.first().unwrap().name, "skill-0000");
        state.page = 999;
        let (items, total, page) = catalog_page(&state);
        assert_eq!((items.len(), total, page), (12, 1_500, 62));
        state.query = "skill-1499".into();
        let (items, total, page) = catalog_page(&state);
        assert_eq!((items.len(), total, page), (1, 1, 0));
        state.query.clear();
        state.page = 0;
        let frame =
            native_tree(marketplace_page(&state, 1000.0)).layout(Size::new(1000.0, 20_000.0));
        let installs = frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("marketplace_install:"))
            .count();
        assert_eq!(installs, PAGE_SIZE);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_install:source-1:skill-24"));
    }

    #[test]
    fn recommended_categories_use_catalog_metadata_and_do_not_hide_local_sources() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut state = MarketplaceViewState {
            snapshot: crate::marketplace::MarketplaceStore::new(
                root.path().into(),
                workspace.path().into(),
            )
            .snapshot()
            .unwrap(),
            category: Some("Diseño y frontend".into()),
            ..Default::default()
        };
        let (items, total, _) = catalog_page(&state);
        assert!(total > 0);
        assert!(items
            .iter()
            .all(|item| crate::marketplace::starter_details(&item.id)
                .unwrap()
                .category
                == "Diseño y frontend"));
        state.snapshot.items.extend(sample_state().snapshot.items);
        state.category = Some("Tus orígenes".into());
        let (items, total, _) = catalog_page(&state);
        assert_eq!(total, 1);
        assert_eq!(items[0].id, "source-1:git-review");
    }

    #[test]
    fn explore_has_independent_query_honest_loading_cache_error_and_explicit_install() {
        let mut state = remote_state();
        state.query = "local query".into();
        assert_eq!(state.active_query(), "quality");
        assert_eq!(catalog_page(&state).1, 1);
        state.explore.page = None;
        state.explore.busy = true;
        let frame = native_tree(marketplace_page(&state, 360.0)).layout(Size::new(360.0, 800.0));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|block| block.source == "Buscando…"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("marketplace_install:")));
        assert!(!frame
            .text_layouts()
            .iter()
            .any(|block| block.source == "Sin resultados"));
        state = remote_state();
        let page = state.explore.page.as_mut().unwrap();
        page.from_cache = true;
        page.stale = true;
        page.warning = Some("Sin conexión; se conservan resultados anteriores.".into());
        let frame = native_tree(marketplace_page(&state, 520.0)).layout(Size::new(520.0, 1000.0));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|block| block.source.contains("Copia guardada sin conexión")));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_install:skills-sh:example/skills:quality"));
        state.explore.page = None;
        state.explore.error = Some("No se pudo conectar al catálogo".into());
        let frame = native_tree(marketplace_page(&state, 360.0)).layout(Size::new(360.0, 800.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "marketplace_explore_retry"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("marketplace_install:")));
    }

    #[test]
    fn installed_update_details_show_real_revisions_and_dispatch_once() {
        let mut state = remote_state();
        let mut item = state.explore.page.as_ref().unwrap().skills[0].item();
        item.installed_scope = Some(InstallScope::Workspace);
        item.enabled = false;
        state.snapshot.items.push(item.clone());
        state.tab = MarketplaceTab::Installed;
        state.updates.insert(
            item.id.clone(),
            SkillUpdate {
                item_id: item.id.clone(),
                installed_revision: Some("a".repeat(40)),
                available_revision: "b".repeat(40),
            },
        );
        state.provenance.insert(
            item.id.clone(),
            SkillProvenance {
                repository: "example/skills".into(),
                revision: "a".repeat(40),
                directory: "skills/quality".into(),
                manifest_digest: None,
            },
        );
        let body = native_tree(detail_body(&state, &item, true)).layout(Size::new(520.0, 2000.0));
        assert!(body
            .text_layouts()
            .iter()
            .any(|block| block.source == "a".repeat(40)));
        for width in [280.0, 520.0, 1000.0] {
            state.selected_item_id = Some(item.id.clone());
            let mut tree = native_tree(marketplace_page(&state, width));
            let frame = tree.layout(Size::new(width, 600.0));
            let update_id = format!("marketplace_details_update:{}", item.id);
            let update = frame
                .regions
                .iter()
                .find(|region| region.id == update_id && region.kind == HitKind::Button)
                .unwrap()
                .rect;
            assert!(
                update.x >= 0.0
                    && update.x + update.width <= width
                    && update.y + update.height <= 600.0
            );
            let point = Point::new(
                update.x + update.width / 2.0,
                update.y + update.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: format!("marketplace_update:{}", item.id)
                }]
            );
        }
    }

    #[test]
    fn explore_search_controls_share_a_row_and_enter_keeps_the_host_contract() {
        for sidebar in [false, true] {
            for width in [164.0, 228.0, 478.0, 920.0] {
                // Full catalog margins never allocate less than 238px. The
                // compact sidebar must also work at its176px presentation.
                if !sidebar && width < 228.0 {
                    continue;
                }
                let state = remote_state();
                let id = if sidebar {
                    "ide_extensions_search"
                } else {
                    "marketplace_search"
                };
                let button_id = if sidebar {
                    "ide_marketplace_explore_search"
                } else {
                    "marketplace_explore_search"
                };
                let (context, mut tree) = native_tree_with_context(
                    Element::column(vec![
                        search_controls(&state, width, sidebar),
                        Element::button("following", "Next").size(Some(width), Some(28.0)),
                    ])
                    .gap(8.0),
                );
                let viewport = Size::new(width, 100.0);
                let frame = tree.layout(viewport);
                let bounds = |id: &str| {
                    frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect
                };
                let field = bounds(id);
                let submit = bounds(button_id);
                let following = bounds("following");
                assert_eq!(field.height, 28.0);
                assert_eq!(submit.height, if sidebar { 24.0 } else { 28.0 });
                assert!(
                    (field.y + field.height / 2.0 - submit.y - submit.height / 2.0).abs() < 0.01
                );
                assert!(field.x + field.width + 6.0 <= submit.x + 0.01);
                assert!(submit.x + submit.width <= width + 0.01);
                assert!(following.y >= field.y + field.height + 8.0);
                let point = Point::new(
                    submit.x + submit.width / 2.0,
                    submit.y + submit.height / 2.0,
                );
                assert_eq!(frame.hit_test(point), Some(button_id));
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked {
                        id: "marketplace_explore_search".into()
                    }]
                );
                assert!(tree.focus(id));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Submitted {
                        id: id.into(),
                        value: state.explore.query.clone()
                    }]
                );
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn ide_catalog_owns_search_only_when_its_navigation_sidebar_is_hidden() {
        let state = remote_state();
        for visible in [false, true] {
            let frame = native_tree(ide_catalog_page(&state, 520.0, visible))
                .layout(Size::new(520.0, 900.0));
            for id in [
                "marketplace_search",
                "marketplace_tab:explore",
                "marketplace_explore_search",
            ] {
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .filter(|region| region.id == id)
                        .count(),
                    usize::from(!visible),
                    "{id}"
                );
            }
            for id in [
                "marketplace_scope_user",
                "marketplace_add_menu",
                "marketplace_install:skills-sh:example/skills:quality",
            ] {
                assert!(
                    frame.regions.iter().any(|region| region.id == id),
                    "catalog action {id} stays available"
                );
            }
        }
    }

    #[test]
    #[ignore = "native Marketplace single-query flow and compact controls; fixture data only"]
    fn export_marketplace_flow_visual_review() {
        let output = std::path::Path::new("/tmp/openade-marketplace-flow-review");
        std::fs::create_dir_all(output).unwrap();
        let save = |root: Element, width: f32, height: f32, name: &str| {
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            let frame = tree.layout(Size::new(width, height));
            let path = output.join(format!("{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                .unwrap();
            let parsed = resvg::usvg::Tree::from_str(
                &std::fs::read_to_string(&path).unwrap(),
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixels = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixels.as_mut(),
            );
            pixels.save_png(path.with_extension("png")).unwrap();
            context.end_pass().textures_delta.clear();
        };
        for width in [280.0, 520.0, 1000.0] {
            for phase in ["idle", "results", "loading", "error"] {
                let mut state = remote_state();
                match phase {
                    "idle" => {
                        state.explore.page = None;
                        state.explore.query.clear();
                    }
                    "loading" => {
                        state.explore.page = None;
                        state.explore.busy = true;
                    }
                    "error" => {
                        state.explore.page = None;
                        state.explore.error = Some("No se pudo conectar con el catálogo.".into());
                    }
                    _ => {}
                }
                save(
                    Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND),
                    width,
                    800.0,
                    &format!("catalog-{width:.0}-{phase}"),
                );
            }
        }
        for (width, visible, phase) in [
            (1440.0, true, "idle"),
            (1440.0, true, "results"),
            (1440.0, true, "detail"),
            (900.0, true, "results"),
            (520.0, false, "idle"),
        ] {
            let mut state = crate::view::ide_visual_tests::fixture();
            state.ide.activity = crate::ide_workspace::IdeActivity::Extensions;
            state.ide.explorer_open = visible;
            state.ide.agent_open = false;
            state.ide.extensions_open = true;
            state.marketplace = remote_state();
            if phase == "idle" {
                state.marketplace.explore.page = None;
                state.marketplace.explore.query.clear();
            }
            if phase == "detail" {
                state.ide.extensions_item = Some("skills-sh:example/skills:quality".into());
            }
            save(
                crate::view::build_window_view(&state, width, 900.0),
                width,
                900.0,
                &format!("ide-{width:.0}-{phase}"),
            );
        }
    }

    #[test]
    fn sidebar_tabs_show_complete_native_labels_inside_the_normal_track() {
        let state = remote_state();
        let (context, mut tree) =
            native_tree_with_context(Element::horizontal_scroll("tabs", tab_strip(&state, true)));
        let frame = tree.layout(Size::new(228.0, 32.0));
        for (key, label, width) in [
            ("recommended", "Recomendadas", 90.0),
            ("explore", "Explorar", 58.0),
            ("installed", "Instaladas", 72.0),
        ] {
            let id = format!("ide_marketplace_tab:{key}");
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect;
            assert_eq!(rect.width, width);
            assert!(rect.x >= 0.0 && rect.x + rect.width <= 228.0);
            let matches = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_size,
                        ..
                    } if text == label => Some((*position, *font_size)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "complete label {label} is painted once");
            let (position, font_size) = matches[0];
            let text_width = context.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        label.into(),
                        eframe::egui::FontId::proportional(font_size),
                        eframe::egui::Color32::WHITE,
                    )
                    .size()
                    .x
            });
            assert!(
                position.x >= rect.x + 4.0
                    && position.x + text_width <= rect.x + rect.width - 4.0 + 0.01,
                "{label} must fit inside its tab padding"
            );
        }
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn three_tabs_and_search_remain_distinct_in_narrow_surfaces() {
        for width in [280.0, 360.0, 1000.0] {
            for tab in [
                MarketplaceTab::Recommended,
                MarketplaceTab::Explore,
                MarketplaceTab::Installed,
            ] {
                let mut state = remote_state();
                state.tab = tab;
                let frame =
                    native_tree(marketplace_page(&state, width)).layout(Size::new(width, 800.0));
                for id in [
                    "marketplace_tab:recommended",
                    "marketplace_tab:explore",
                    "marketplace_tab:installed",
                    "marketplace_search",
                ] {
                    let regions = frame
                        .regions
                        .iter()
                        .filter(|region| region.id == id)
                        .collect::<Vec<_>>();
                    assert_eq!(regions.len(), 1, "{id} at{width}");
                    let rect = regions[0].rect;
                    assert!(
                        rect.x >= 0.0 && rect.x + rect.width <= width,
                        "{id}: {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn failed_update_check_does_not_claim_no_updates_after_previous_empty_check() {
        let mut state = MarketplaceViewState::default();
        state.tab = MarketplaceTab::Installed;
        state.updates_checked = true;
        state.update_error = Some("Sin conexión al repositorio".into());
        let frame = native_tree(marketplace_page(&state, 360.0)).layout(Size::new(360.0, 800.0));
        assert!(!frame
            .text_layouts()
            .iter()
            .any(|block| block.source == "Sin actualizaciones disponibles"));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|block| block.source == "Sin conexión al repositorio"));
    }

    #[test]
    fn physical_refresh_dispatches_explore_retry_through_its_tooltip() {
        let mut state = remote_state();
        for tab in [MarketplaceTab::Recommended, MarketplaceTab::Explore] {
            state.tab = tab;
            let mut tree = native_tree(marketplace_page(&state, 520.0));
            let frame = tree.layout(Size::new(520.0, 640.0));
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_refresh" && region.kind == HitKind::Button)
                .unwrap()
                .rect;
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: if tab == MarketplaceTab::Explore {
                        "marketplace_explore_retry"
                    } else {
                        "marketplace_refresh"
                    }
                    .into(),
                }]
            );
        }
    }

    #[test]
    fn sidebar_pagination_dispatches_shared_actions_from_distinct_physical_controls() {
        for (suffix, action) in [
            ("prev", "marketplace_page_prev"),
            ("next", "marketplace_page_next"),
        ] {
            let mut tree = native_tree(pagination_controls(72, 1, true));
            let frame = tree.layout(Size::new(224.0, 32.0));
            let id = format!("ide_marketplace_page_{suffix}");
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == id && region.kind == HitKind::Button)
                .unwrap()
                .rect;
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            assert_eq!(frame.hit_test(point), Some(id.as_str()));
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: action.into() }]
            );
        }
    }

    #[test]
    fn update_details_label_each_revision_once_and_fallback_without_provenance() {
        let mut state = remote_state();
        let item = state.explore.page.as_ref().unwrap().skills[0].item();
        state.updates.insert(
            item.id.clone(),
            SkillUpdate {
                item_id: item.id.clone(),
                installed_revision: Some("a".repeat(40)),
                available_revision: "b".repeat(40),
            },
        );
        for with_provenance in [false, true] {
            if with_provenance {
                state.provenance.insert(
                    item.id.clone(),
                    SkillProvenance {
                        repository: "example/skills".into(),
                        revision: "a".repeat(40),
                        directory: "skills/demo".into(),
                        manifest_digest: None,
                    },
                );
            }
            let frame =
                native_tree(detail_body(&state, &item, false)).layout(Size::new(520.0, 2000.0));
            let layouts = frame.text_layouts();
            for text in [
                "Revisión instalada",
                "Revisión disponible",
                &"a".repeat(40),
                &"b".repeat(40),
            ] {
                assert_eq!(
                    layouts.iter().filter(|block| block.source == text).count(),
                    1,
                    "{text} provenance={with_provenance}"
                );
            }
            assert!(!layouts.iter().any(|block| block.source.contains('→')));
        }
        state.provenance.clear();
        state.updates.get_mut(&item.id).unwrap().installed_revision = None;
        let frame = native_tree(detail_body(&state, &item, false)).layout(Size::new(520.0, 2000.0));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|block| block.source == "Sin revisión registrada"));
    }
}
