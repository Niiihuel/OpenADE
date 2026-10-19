//! Native Marketplace surface for portable `SKILL.md` bundles.
//!
//! The full page browses responsive cards and opens their detail in place.
//! The IDE keeps a compact catalog in its sidebar, sharing the same rows,
//! filters and item details. Source management lives in a secondary modal.
//! Installation is explicit; JavaScript plugins and MCP servers are different
//! runtimes and are not offered as installable SKILL.md bundles here.

use openide_ui::{
    Alert, AlertTone, Button, ButtonGroup, ButtonVariant, Card, CardDensity, CardGrid, Chip, Color,
    Component, ControlSize, DropdownMenu, DropdownMenuEntry, DropdownMenuItem, Element, FormField,
    Icon, IconButton, Modal, Notice, PageColumn, PageHeader, ScrollArea, SearchField,
    SectionHeader, SectionHeaderSizing, Separator, SkeletonGroup, Text, TextField, TextRole,
    Tooltip, TooltipPlacement, TooltipSpec,
};

use std::{borrow::Cow, collections::BTreeMap};

use crate::{
    marketplace::{
        InstallScope, MarketplaceItem, MarketplaceSnapshot, RemoteSearchPage, SkillProvenance,
        SkillUpdate, SourceKind,
    },
    theme,
    ui_sizing::{
        EXTENSION_ICON, EXTENSION_ROW_HEIGHT, MARKETPLACE_HEADER, MARKETPLACE_HEADER_NARROW,
    },
};

/// Rows a group shows before "Mostrar más" adds as many again. The whole
/// catalog is in memory; this bounds the elements built each frame.
pub const GROUP_STEP: usize = 30;
/// The detail sets its facts beside the instructions from this width.
const DETAIL_SPLIT_WIDTH: f32 = 680.0;
const FACTS_WIDTH: f32 = 232.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MarketplaceFilter {
    #[default]
    All,
    Installed,
    Updates,
    Available,
}

impl MarketplaceFilter {
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Installed => "installed",
            Self::Updates => "updates",
            Self::Available => "available",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [Self::All, Self::Installed, Self::Updates, Self::Available]
            .into_iter()
            .find(|filter| filter.key() == key)
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "Todas",
            Self::Installed => "Instaladas",
            Self::Updates => "Con actualización",
            Self::Available => "No instaladas",
        }
    }
}

/// The last skills.sh search: `query` is the text that was submitted.
#[derive(Clone, Debug, Default)]
pub struct MarketplaceExploreState {
    pub query: String,
    pub page: Option<RemoteSearchPage>,
    pub busy: bool,
    pub error: Option<String>,
}

/// The instructions of the selected item. `body` is `None` while a worker
/// reads them, and `Ok(None)` when they are not on this machine yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketplaceReadme {
    pub item_id: String,
    pub body: Option<Result<Option<String>, String>>,
}

#[derive(Clone, Debug)]
pub struct MarketplaceViewState {
    /// Source management is secondary to the skill catalog.
    pub sources_open: bool,
    pub snapshot: MarketplaceSnapshot,
    pub filter: MarketplaceFilter,
    pub category: Option<String>,
    pub filter_menu_open: bool,
    /// Rows shown per group; see `GROUP_STEP`.
    pub visible: usize,
    /// Groups asked to show more than `visible`, each with how many.
    pub visible_by: std::collections::BTreeMap<String, usize>,
    pub explore: MarketplaceExploreState,
    pub updates: BTreeMap<String, SkillUpdate>,
    pub update_checking: bool,
    pub updates_checked: bool,
    pub update_error: Option<String>,
    pub provenance: BTreeMap<String, SkillProvenance>,
    pub query: String,
    pub add_menu_open: bool,
    pub add_source_kind: Option<SourceKind>,
    pub source_draft: String,
    /// The scope the detail's primary button installs to: the last one chosen.
    pub install_scope: InstallScope,
    pub status: Option<String>,
    pub status_error: bool,
    pub busy: bool,
    /// The item the running operation installs, updates, toggles or removes.
    pub busy_item: Option<String>,
    /// The work on `busy_item` is its update, not its switch.
    pub busy_update: bool,
    /// The open action menu, as `row:<id>` or `detail:<id>`.
    pub menu_item: Option<String>,
    pub pending_uninstall_id: Option<String>,
    pub pending_remove_source_id: Option<String>,
    pub selected_item_id: Option<String>,
    pub readme: Option<MarketplaceReadme>,
}

impl Default for MarketplaceViewState {
    fn default() -> Self {
        Self {
            snapshot: MarketplaceSnapshot::default(),
            filter: MarketplaceFilter::All,
            category: None,
            filter_menu_open: false,
            sources_open: false,
            visible: GROUP_STEP,
            visible_by: Default::default(),
            explore: MarketplaceExploreState::default(),
            updates: BTreeMap::new(),
            update_checking: false,
            updates_checked: false,
            update_error: None,
            provenance: BTreeMap::new(),
            query: String::new(),
            add_menu_open: false,
            add_source_kind: None,
            source_draft: String::new(),
            install_scope: InstallScope::User,
            status: None,
            status_error: false,
            busy: false,
            busy_item: None,
            busy_update: false,
            menu_item: None,
            pending_uninstall_id: None,
            pending_remove_source_id: None,
            selected_item_id: None,
            readme: None,
        }
    }
}

impl MarketplaceViewState {
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

    fn selected(&self) -> Option<Cow<'_, MarketplaceItem>> {
        self.find_item(self.selected_item_id.as_deref()?)
    }

    fn filtered(&self) -> bool {
        self.filter != MarketplaceFilter::All || self.category.is_some()
    }

    /// skills.sh results belong to the unfiltered list and to "No instaladas".
    fn remote_shown(&self) -> bool {
        self.category.is_none()
            && matches!(
                self.filter,
                MarketplaceFilter::All | MarketplaceFilter::Available
            )
    }
}

pub(super) struct CatalogGroup<'a> {
    pub key: &'static str,
    pub title: &'static str,
    pub items: Vec<Cow<'a, MarketplaceItem>>,
}

fn is_starter(id: &str) -> bool {
    id.split_once(':')
        .is_some_and(|(source, _)| crate::marketplace::is_starter_source(source))
}

/// The catalog as the list shows it: what is installed, the curated entries,
/// the entries of the user's sources and the last skills.sh search. Every
/// match is counted; the list builds only the first `visible` of each group.
pub(super) fn catalog_groups(state: &MarketplaceViewState) -> Vec<CatalogGroup<'_>> {
    let query = state.query.trim().to_lowercase();
    let matches = |item: &MarketplaceItem| {
        (query.is_empty()
            || item.name.to_lowercase().contains(&query)
            || item.description.to_lowercase().contains(&query)
            || item.source_name.to_lowercase().contains(&query))
            && state.category.as_deref().is_none_or(|category| {
                crate::marketplace::starter_category(&item.id) == Some(category)
            })
    };
    let local = |keep: &dyn Fn(&MarketplaceItem) -> bool| -> Vec<Cow<'_, MarketplaceItem>> {
        state
            .snapshot
            .items
            .iter()
            .filter(|item| keep(item) && matches(item))
            .map(Cow::Borrowed)
            .collect()
    };
    let mut groups = Vec::new();
    if state.filter != MarketplaceFilter::Available {
        groups.push(CatalogGroup {
            key: "installed",
            title: "Instaladas",
            items: local(&|item| {
                item.installed_scope.is_some()
                    && (state.filter != MarketplaceFilter::Updates
                        || state.updates.contains_key(&item.id))
            }),
        });
    }
    if matches!(
        state.filter,
        MarketplaceFilter::All | MarketplaceFilter::Available
    ) {
        groups.push(CatalogGroup {
            key: "recommended",
            title: "Recomendadas",
            items: local(&|item| item.installed_scope.is_none() && is_starter(&item.id)),
        });
        groups.push(CatalogGroup {
            key: "sources",
            title: "De tus orígenes",
            items: local(&|item| item.installed_scope.is_none() && !is_starter(&item.id)),
        });
    }
    if state.remote_shown() {
        if let Some(page) = &state.explore.page {
            groups.push(CatalogGroup {
                key: "remote",
                title: "skills.sh",
                items: page
                    .skills
                    .iter()
                    // An installed result is already in "Instaladas".
                    .filter(|skill| {
                        !state
                            .snapshot
                            .items
                            .iter()
                            .any(|item| item.id == skill.id && item.installed_scope.is_some())
                    })
                    .map(|skill| Cow::Owned(skill.item()))
                    .collect(),
            });
        }
    }
    groups.retain(|group| !group.items.is_empty());
    groups
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

fn install_label(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::User => "Instalar para el usuario",
        InstallScope::Workspace => "Instalar en este proyecto",
    }
}

/// Who publishes an item: the curated entries carry "Publisher · revision".
fn publisher(item: &MarketplaceItem) -> &str {
    item.source_name
        .split(" · ")
        .next()
        .unwrap_or(&item.source_name)
}

fn remote_installs(state: &MarketplaceViewState, id: &str) -> Option<u64> {
    state
        .explore
        .page
        .as_ref()?
        .skills
        .iter()
        .find(|skill| skill.id == id)
        .map(|skill| skill.installs)
}

fn has_source_link(state: &MarketplaceViewState, item: &MarketplaceItem) -> bool {
    state.provenance.contains_key(&item.id)
        || is_starter(&item.id)
        || remote_installs(state, &item.id).is_some()
}

/// What an install does and what it checks, said where the decision is made.
fn trust_note(item: &MarketplaceItem) -> String {
    if let Some(details) = crate::marketplace::starter_details(&item.id) {
        format!(
            "Se descarga de GitHub en la revisión fijada {}. Cada archivo se verifica con SHA-256 al instalar y no se ejecuta código.",
            &details.revision[..7.min(details.revision.len())]
        )
    } else if item.id.starts_with("skills-sh:") {
        format!(
            "Publicada por {} en skills.sh, sin revisar por openADE. La licencia y las instrucciones se verifican al instalar.",
            item.source_name
        )
    } else {
        "Las instrucciones se copian a la ubicación de instalación elegida. No se ejecuta código."
            .to_owned()
    }
}

/// The item's mark: the publisher's initial for a repository, a folder for a
/// local source. The catalog carries no artwork.
fn item_icon(item: &MarketplaceItem, side: f32) -> Element {
    let glyph = match item.source_kind {
        SourceKind::Local => Element::icon(Icon::Folder)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(16.0)),
        SourceKind::Github => theme::text_px(
            publisher(item)
                .chars()
                .find(|character| character.is_alphanumeric())
                .map(|character| character.to_uppercase().to_string())
                .unwrap_or_else(|| "?".into()),
            (side * 0.46).round(),
            theme::EMPHASIS_INK,
        ),
    };
    Element::panel(glyph.centered())
        .background(theme::SELECTED)
        .radius((side * 0.22).round())
        .size(Some(side), Some(side))
}

fn small(value: impl Into<String>) -> Text {
    Text::new(value, theme::component_palette())
        .role(TextRole::Small)
        .muted()
}

/// The menu behind an item's action button. Installing names its scope and
/// states what is verified; an installed item is managed from the same place.
fn action_menu(
    state: &MarketplaceViewState,
    item: &MarketplaceItem,
    key: &str,
    trigger: Element,
) -> Element {
    let id = &item.id;
    let entries: Vec<DropdownMenuEntry> = if item.installed_scope.is_some() {
        let mut entries = Vec::new();
        if state.updates.contains_key(id) {
            entries.push(
                DropdownMenuItem::new(format!("marketplace_update:{id}"), "Actualizar")
                    .icon(Icon::Refresh)
                    .into(),
            );
        }
        entries.push(
            DropdownMenuItem::new(
                format!("marketplace_toggle:{id}"),
                if item.enabled {
                    "Desactivar"
                } else {
                    "Activar"
                },
            )
            .icon(if item.enabled {
                Icon::Pause
            } else {
                Icon::Play
            })
            .into(),
        );
        if has_source_link(state, item) {
            entries.push(
                DropdownMenuItem::new(format!("marketplace_open_source:{id}"), "Abrir origen")
                    .icon(Icon::Github)
                    .into(),
            );
        }
        entries.push(DropdownMenuEntry::Separator);
        entries.push(
            DropdownMenuItem::new(format!("marketplace_uninstall:{id}"), "Desinstalar…")
                .icon(Icon::Trash)
                .destructive(theme::DESTRUCTIVE)
                .into(),
        );
        entries
    } else {
        vec![
            DropdownMenuItem::new(
                format!("marketplace_install_user:{id}"),
                install_label(InstallScope::User),
            )
            .description("Disponible en todos tus proyectos")
            .selected(state.install_scope == InstallScope::User)
            .enabled(item.source_available)
            .into(),
            DropdownMenuItem::new(
                format!("marketplace_install_workspace:{id}"),
                install_label(InstallScope::Workspace),
            )
            .description("Sólo en el proyecto abierto")
            .selected(state.install_scope == InstallScope::Workspace)
            .enabled(item.source_available)
            .into(),
        ]
    };
    let mut menu = DropdownMenu::new(
        trigger,
        entries,
        state.menu_item.as_deref() == Some(key) && !state.busy,
        "marketplace_menu_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(264.0);
    if item.installed_scope.is_none() {
        menu = menu.footer(
            small(trust_note(item))
                .render()
                .padding(8.0)
                .size(Some(264.0), None),
        );
    }
    menu.render()
}

/// One entry of the list: mark, name and state, one line of description,
/// publisher and source, and one action that never changes the row's size.
fn item_row(
    state: &MarketplaceViewState,
    item: &MarketplaceItem,
    width: f32,
    select_prefix: &str,
    selected: bool,
    surface: Color,
) -> Element {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    let select = format!("{select_prefix}{}", item.id);
    let toggle = format!("marketplace_row_menu:{}", item.id);
    let update = state.updates.contains_key(&item.id) && item.installed_scope.is_some();
    let (badge, badge_ink) = match item.installed_scope {
        _ if !item.source_available && item.installed_scope.is_none() => {
            ("No disponible", theme::DESTRUCTIVE)
        }
        Some(_) if update => ("Actualización", theme::WARNING),
        Some(_) if !item.enabled => ("Inactiva", theme::FOREGROUND_SUBTLE),
        Some(scope) => (scope_label(scope), theme::FOREGROUND_SUBTLE),
        None => ("", theme::FOREGROUND_SUBTLE),
    };
    let origin = match remote_installs(state, &item.id) {
        Some(installs) if item.installed_scope.is_none() => {
            format!("{} · {installs} instalaciones", item.source_name)
        }
        _ => format!("{} · {}", publisher(item), source_label(item.source_kind)),
    };
    // The state sits on the origin's line, so the name keeps the whole width.
    let mut origin = vec![
        Text::new(origin, palette)
            .role(TextRole::Micro)
            .muted()
            .truncate()
            .render()
            .flex(1.0),
    ];
    if !badge.is_empty() {
        origin.push(
            Text::new(badge, palette)
                .role(TextRole::Micro)
                .ink(badge_ink)
                .render(),
        );
    }
    let copy = Element::column(vec![
        Text::new(&item.name, palette)
            .role(TextRole::Label)
            .truncate()
            .render(),
        small(&item.description).truncate().render(),
        Element::row(origin).gap(6.0),
    ])
    .gap(2.0)
    .flex(1.0)
    .centered_y();
    let target = Tooltip::new(
        Element::clickable(
            &select,
            Element::row(vec![item_icon(item, EXTENSION_ICON).centered_y(), copy]).gap(8.0),
        )
        .background(Color::TRANSPARENT)
        .transition_ms(0)
        .padding(6.0)
        .size(None, Some(EXTENSION_ROW_HEIGHT)),
        &select,
        &item.name,
        theme::component_palette(),
    )
    .description(&item.description)
    .placement(TooltipPlacement::Right)
    .render()
    .flex(1.0);
    let installed = item.installed_scope.is_some();
    let action = IconButton::new(
        &toggle,
        if installed {
            Icon::Settings
        } else {
            Icon::ArrowDown
        },
        theme::in_row(palette),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .loading(state.busy_item.as_deref() == Some(item.id.as_str()))
    .enabled(!state.busy && (installed || item.source_available))
    .tooltip_with(
        TooltipSpec::new(if installed {
            "Administrar"
        } else {
            "Instalar…"
        })
        .placement(TooltipPlacement::Below)
        .suppressed(state.menu_item.is_some()),
    )
    .render();
    let action = action_menu(state, item, &format!("row:{}", item.id), action).centered_y();
    Element::row(vec![
        target,
        action,
        Element::spacer().size(Some(4.0), None),
    ])
    .gap(0.0)
    .background(if selected {
        theme::SELECTED
    } else {
        Color::TRANSPARENT
    })
    .hover_background(if selected {
        theme::SELECTED
    } else {
        palette.hover
    })
    .pressed_background(palette.selected)
    .hover_surface(format!("{select}:surface"), [select.clone(), toggle])
    .transition_ms(80)
    .radius(6.0)
    .clip_children()
    .size(Some(width), Some(EXTENSION_ROW_HEIGHT))
}

fn filter_control(state: &MarketplaceViewState) -> Element {
    let mut categories = std::collections::BTreeSet::new();
    for item in &state.snapshot.items {
        if let Some(category) = crate::marketplace::starter_category(&item.id) {
            categories.insert(category);
        }
    }
    let mut entries = vec![DropdownMenuEntry::Heading("Mostrar".into())];
    entries.extend(
        [
            MarketplaceFilter::All,
            MarketplaceFilter::Installed,
            MarketplaceFilter::Updates,
            MarketplaceFilter::Available,
        ]
        .into_iter()
        .map(|filter| {
            DropdownMenuItem::new(
                format!("marketplace_filter:{}", filter.key()),
                filter.label(),
            )
            .selected(state.filter == filter)
            .into()
        }),
    );
    if !categories.is_empty() {
        entries.push(DropdownMenuEntry::Separator);
        entries.push(DropdownMenuEntry::Heading("Categoría".into()));
        entries.push(
            DropdownMenuItem::new("marketplace_category:all", "Todas las categorías")
                .selected(state.category.is_none())
                .into(),
        );
        entries.extend(categories.into_iter().map(|category| {
            DropdownMenuItem::new(format!("marketplace_category:{category}"), category)
                .selected(state.category.as_deref() == Some(category))
                .into()
        }));
    }
    DropdownMenu::new(
        IconButton::new(
            "marketplace_filter_toggle",
            Icon::Filter,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .active(state.filtered())
        .tooltip_with(
            TooltipSpec::new("Filtrar")
                .placement(TooltipPlacement::Below)
                .suppressed(state.filter_menu_open),
        )
        .render(),
        entries,
        state.filter_menu_open,
        "marketplace_filter_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(216.0)
    .render()
}

/// The skills.sh part of the list: the prompt to search, the search in
/// course, its failure with a retry, or a note about the copy shown.
fn remote_status(state: &MarketplaceViewState, width: f32) -> Vec<Element> {
    let explore = &state.explore;
    let query = state.query.trim();
    let palette = theme::component_palette();
    let mut rows = Vec::new();
    if explore.busy {
        rows.push(
            small(format!("Buscando «{}» en skills.sh…", explore.query))
                .render()
                .padding(6.0),
        );
        rows.extend(
            SkeletonGroup::rows("marketplace_explore_loading", palette, 3)
                .row_height(EXTENSION_ROW_HEIGHT)
                .icon(true)
                .into_rows(),
        );
    } else if let Some(error) = &explore.error {
        rows.push(
            Notice::new("No se pudo buscar en skills.sh", palette)
                .icon(Icon::WarningTriangle)
                .description(error)
                .action(Element::row(vec![
                    Button::new("marketplace_explore_retry", "Reintentar", palette)
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .render(),
                    Element::spacer().flex(1.0),
                ]))
                .width(width)
                .render(),
        );
    } else if let Some(page) = &explore.page {
        if page.skills.is_empty() {
            rows.push(
                small(format!(
                    "skills.sh no tiene resultados para «{}».",
                    page.query
                ))
                .render()
                .padding(6.0),
            );
        }
        if let Some(warning) = &page.warning {
            rows.push(
                Notice::new(
                    if page.stale {
                        "Copia guardada sin conexión"
                    } else {
                        "Copia guardada"
                    },
                    palette,
                )
                .description(warning)
                .width(width)
                .render(),
            );
        }
    } else if query.chars().count() >= 2 {
        rows.push(
            Button::new(
                "marketplace_explore_search",
                format!("Buscar «{query}» en skills.sh"),
                palette,
            )
            .icon(Icon::Globe)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .width(width)
            .render(),
        );
    } else {
        rows.push(
            small("Escribe al menos 2 caracteres y pulsa Enter para buscar en skills.sh.")
                .render()
                .padding(6.0)
                .size(Some(width), None),
        );
    }
    rows
}

/// What the list says when no local group has anything to show.
fn empty_list(state: &MarketplaceViewState, width: f32) -> Element {
    let palette = theme::component_palette();
    let outline = |id: &str, label: &str| {
        Element::row(vec![
            Button::new(id, label, palette)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
            Element::spacer().flex(1.0),
        ])
    };
    if state.busy && state.snapshot.items.is_empty() {
        return Element::column(
            SkeletonGroup::rows("marketplace_loading", palette, 6)
                .row_height(EXTENSION_ROW_HEIGHT)
                .icon(true)
                .into_rows(),
        )
        .gap(2.0)
        .size(Some(width), None);
    }
    let notice = if state.status_error && state.snapshot.items.is_empty() {
        Notice::new("No se pudo cargar el catálogo", palette)
            .icon(Icon::WarningTriangle)
            .description(state.status.as_deref().unwrap_or_default())
            .action(outline("marketplace_reload", "Reintentar"))
    } else if !state.query.trim().is_empty() {
        Notice::new("Sin resultados", palette)
            .icon(Icon::Search)
            .description(format!(
                "Ninguna skill de tu catálogo coincide con «{}».",
                state.query.trim()
            ))
    } else if state.filter == MarketplaceFilter::Updates {
        Notice::new(
            if state.update_error.is_some() {
                "No se pudieron buscar actualizaciones"
            } else if state.updates_checked {
                "Sin actualizaciones disponibles"
            } else {
                "Todavía no se buscaron actualizaciones"
            },
            palette,
        )
        .icon(Icon::Refresh)
        .description("Se comparan las revisiones instaladas con las de su origen.")
        .action(outline(
            "marketplace_check_updates",
            "Buscar actualizaciones",
        ))
    } else if state.filter == MarketplaceFilter::Installed {
        Notice::new("Todavía no instalaste skills", palette)
            .icon(Icon::Puzzle)
            .description("Elige una del catálogo y dónde instalarla.")
            .action(outline("marketplace_filter:all", "Ver el catálogo"))
    } else {
        Notice::new("Sin skills disponibles", palette)
            .icon(Icon::Puzzle)
            .description(if state.filtered() {
                "Ninguna skill coincide con el filtro."
            } else {
                "Actualiza el catálogo o agrega un origen con archivos SKILL.md."
            })
    };
    notice.stacked().width(width).render()
}

/// The same query and filters drive both the full catalog and the sidebar.
fn catalog_controls(state: &MarketplaceViewState, width: f32, search_id: &str) -> Vec<Element> {
    let palette = theme::component_palette();
    let installed = state
        .snapshot
        .items
        .iter()
        .any(|item| item.installed_scope.is_some());
    let mut controls = vec![
        SearchField::new(search_id, &state.query, "Buscar skills o autores", palette)
            .size(ControlSize::Compact)
            .width((width - if installed { 64.0 } else { 32.0 }).max(0.0))
            .render(),
        filter_control(state).centered_y(),
    ];
    if installed {
        controls.push(
            IconButton::new("marketplace_updates_check", Icon::Refresh, palette)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .enabled(!state.busy && !state.update_checking)
                .loading(state.update_checking)
                .tooltip_with(
                    TooltipSpec::new("Buscar actualizaciones").placement(TooltipPlacement::Below),
                )
                .dispatch_as("marketplace_check_updates")
                .render()
                .centered_y(),
        );
    }
    let search = Element::row(controls)
        .gap(4.0)
        .size(Some(width), Some(28.0));
    let mut blocks = vec![search];
    if state.filtered() {
        let label = [
            (state.filter != MarketplaceFilter::All).then(|| state.filter.label()),
            state.category.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        blocks.push(
            Element::row(vec![
                small(label).truncate().render().flex(1.0).centered_y(),
                Button::new("marketplace_filter_clear", "Quitar filtro", palette)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .render(),
            ])
            .gap(6.0)
            .size(Some(width), Some(24.0)),
        );
    }
    blocks
}

pub(crate) fn catalog_list(
    state: &MarketplaceViewState,
    width: f32,
    search_id: &str,
    select_prefix: &str,
    surface: Color,
    footer: Option<Element>,
) -> Vec<Element> {
    let palette = theme::component_palette();
    let list_width = (width - 14.0).max(0.0);
    let mut blocks = catalog_controls(state, width, search_id);
    let heading = |title: &str, count: Option<usize>| {
        let mut header = SectionHeader::new(title, palette)
            .sizing(SectionHeaderSizing::LIST)
            .width(list_width);
        if let Some(count) = count {
            header = header.count(count);
        }
        header.render()
    };
    let mut rows = Vec::new();
    if !state.snapshot.diagnostics.is_empty() {
        rows.push(
            Notice::new(
                match state.snapshot.diagnostics.len() {
                    1 => "Un origen no se pudo leer".to_owned(),
                    count => format!("{count} orígenes no se pudieron leer"),
                },
                palette,
            )
            .icon(Icon::WarningTriangle)
            .description("Revisa sus errores en Orígenes.")
            .width(list_width)
            .render(),
        );
    }
    let groups = catalog_groups(state);
    if groups.iter().all(|group| group.key == "remote") {
        rows.push(empty_list(state, list_width));
    }
    for group in &groups {
        rows.push(heading(group.title, Some(group.items.len())));
        if group.key == "remote" {
            rows.extend(remote_status(state, list_width));
        }
        let visible = state
            .visible_by
            .get(group.key)
            .copied()
            .unwrap_or(state.visible);
        rows.extend(group.items.iter().take(visible).map(|item| {
            item_row(
                state,
                item,
                list_width,
                select_prefix,
                state.selected_item_id.as_deref() == Some(item.id.as_str()),
                surface,
            )
        }));
        if group.items.len() > visible {
            rows.push(
                Button::new(
                    format!("marketplace_more:{}", group.key),
                    format!("Mostrar más ({} restantes)", group.items.len() - visible),
                    palette,
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .width(list_width)
                .render(),
            );
        }
    }
    if state.remote_shown() && !groups.iter().any(|group| group.key == "remote") {
        rows.push(heading("skills.sh", None));
        rows.extend(remote_status(state, list_width));
    }
    if let Some(footer) = footer {
        rows.push(Separator::horizontal(palette).render());
        rows.push(footer);
    }
    blocks.push(
        ScrollArea::new(
            format!("{search_id}:list"),
            Element::column(rows).gap(2.0).size(Some(list_width), None),
        )
        .render()
        .flex(1.0),
    );
    blocks
}

fn refresh_button(state: &MarketplaceViewState, id: &str, size: ControlSize) -> Element {
    IconButton::new(id, Icon::Refresh, theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(size)
        .enabled(!state.busy)
        .loading(state.busy && state.busy_item.is_none() && state.add_source_kind.is_none())
        .dispatch_as("marketplace_refresh")
        .tooltip_with(
            TooltipSpec::new("Actualizar orígenes")
                .description("Vuelve a leer los orígenes y busca actualizaciones.")
                .placement(TooltipPlacement::Below),
        )
        .render()
}

/// "Agregar origen": the two kinds of source a user can add.
fn add_source_menu(state: &MarketplaceViewState, compact: bool) -> Element {
    let palette = theme::component_palette();
    let trigger = if compact {
        IconButton::new("marketplace_add_menu", Icon::Add, palette)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .tooltip("Agregar origen")
            .tooltip_placement(TooltipPlacement::Below)
            .enabled(!state.busy)
            .render()
    } else {
        Button::new("marketplace_add_menu", "Agregar origen", palette)
            .icon(Icon::Add)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!state.busy)
            .render()
    };
    DropdownMenu::new(
        trigger,
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
    .render()
}

fn sources_button(width: f32) -> Element {
    if width < 430.0 {
        IconButton::new("marketplace_sources_open", Icon::Folder, theme::component_palette())
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .tooltip("Orígenes")
            .render()
    } else {
        Button::new("marketplace_sources_open", "Orígenes", theme::component_palette())
            .icon(Icon::Folder)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render()
    }
}

fn page_header(
    title: &str,
    description: &str,
    navigation: Option<Element>,
    width: f32,
) -> Element {
    // Beside the host's sidebar control a narrow title gives up two pixels.
    let sizing = if navigation.is_some() && width < 320.0 {
        MARKETPLACE_HEADER_NARROW
    } else {
        MARKETPLACE_HEADER
    };
    let mut header = PageHeader::new(title, theme::component_palette())
        .description(description)
        .trailing(sources_button(width))
        .truncate(true)
        .sizing(sizing);
    if let Some(navigation) = navigation {
        header = header.leading(navigation);
    }
    header.render()
}

/// One source fact: an icon, a name over a cut detail, an
/// error under them and one control at the end.
fn info_card(
    icon: Icon,
    name: &str,
    detail: &str,
    error: Option<&str>,
    trailing: Element,
    width: f32,
) -> Element {
    let palette = theme::component_palette();
    let mut copy = vec![
        Text::new(name, palette).role(TextRole::Label).render(),
        small(detail).truncate().render(),
    ];
    if let Some(error) = error {
        copy.push(
            Text::new(error, palette)
                .role(TextRole::Small)
                .ink(theme::DESTRUCTIVE)
                .render(),
        );
    }
    Card::new(
        Element::row(vec![
            Element::icon(icon)
                .foreground(theme::FOREGROUND_SUBTLE)
                .centered_y(),
            Element::column(copy).gap(2.0).flex(1.0),
            trailing.centered_y(),
        ])
        .gap(10.0),
        theme::panel_card_palette(),
    )
    .density(CardDensity::Compact)
    .width(width)
    .render()
}

/// Every origin of the catalog in one place: the two built in, then the
/// user's own with their errors and the control that removes them.
fn sources_section(state: &MarketplaceViewState, width: f32) -> Element {
    let palette = theme::component_palette();
    let row = |icon: Icon, name: &str, detail: &str, error: Option<&str>, trailing: Element| {
        info_card(icon, name, detail, error, trailing, width)
    };
    let starters = state
        .snapshot
        .items
        .iter()
        .filter(|item| is_starter(&item.id))
        .count();
    let mut rows = vec![
        SectionHeader::new("Orígenes", palette)
            .count(state.snapshot.sources.len() + 2)
            .width(width)
            .render(),
        row(
            Icon::Book,
            "Catálogo recomendado",
            &format!("{starters} skills incluidas en openADE, fijadas a una revisión"),
            None,
            Chip::new("Incluido", palette).outline().render(),
        ),
        row(
            Icon::Globe,
            "skills.sh",
            "Búsqueda en línea de la comunidad; sólo consulta al pulsar Enter",
            None,
            Chip::new("Incluido", palette).outline().render(),
        ),
    ];
    for source in &state.snapshot.sources {
        let prefix = format!("{}: ", source.name);
        let error = state
            .snapshot
            .diagnostics
            .iter()
            .find_map(|diagnostic| diagnostic.strip_prefix(&prefix));
        rows.push(row(
            match source.kind {
                SourceKind::Local => Icon::Folder,
                SourceKind::Github => Icon::Github,
            },
            &source.name,
            &source.location,
            error,
            IconButton::new(
                format!("marketplace_remove_source:{}", source.id),
                Icon::Trash,
                theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(!state.busy)
            .tooltip_with(TooltipSpec::new("Quitar origen").placement(TooltipPlacement::Above))
            .render(),
        ));
    }
    Element::column(rows).gap(6.0).size(Some(width), None)
}

fn status_alerts(state: &MarketplaceViewState) -> Vec<Element> {
    let mut alerts = Vec::new();
    if let Some(status) = state
        .status
        .as_deref()
        .filter(|value| *value != "Catálogo cargado" && state.add_source_kind.is_none())
        .filter(|_| state.status_error || !state.busy)
    {
        alerts.push(
            Alert::new("Aviso de Marketplace", status, theme::component_palette())
                .tone(if state.status_error {
                    AlertTone::Error
                } else {
                    AlertTone::Neutral
                })
                .compact(true)
                .dismiss("marketplace_status_dismiss")
                .render(),
        );
    }
    if let Some(error) = &state.update_error {
        alerts.push(
            Alert::new(
                "No se pudo buscar actualizaciones",
                error,
                theme::component_palette(),
            )
            .tone(AlertTone::Error)
            .compact(true)
            .render(),
        );
    }
    alerts
}

fn source_manager_body(state: &MarketplaceViewState, width: f32) -> Element {
    let mut sections = status_alerts(state);
    sections.push(sources_section(state, width));
    Element::column(sections).gap(12.0).size(Some(width), None)
}

fn detail_field(label: &str, value: &str) -> Element {
    Element::column(vec![
        small(label).render(),
        Text::new(value, theme::component_palette())
            .role(TextRole::Caption)
            .render(),
    ])
    .gap(2.0)
}

/// The facts the catalog and the registry hold about an item. Nothing here
/// is estimated: a field with no data is left out.
fn detail_facts(state: &MarketplaceViewState, item: &MarketplaceItem, width: f32) -> Element {
    let palette = theme::component_palette();
    let mut facts = vec![
        SectionHeader::new("Detalles", palette)
            .sizing(SectionHeaderSizing::LIST)
            .width(width)
            .render(),
        detail_field("Tipo", "Skill (SKILL.md) para el agente nativo"),
    ];
    let starter = crate::marketplace::starter_details(&item.id);
    if let Some(details) = &starter {
        facts.extend([
            detail_field("Origen", &details.repository_url),
            detail_field("Revisión fijada", &details.revision),
            detail_field("Carpeta", &details.directory),
            detail_field("Licencia", &details.license),
            detail_field("Categoría", &details.category),
            detail_field("Compatibilidad", &details.compatibility),
        ]);
    } else {
        let source = state
            .snapshot
            .sources
            .iter()
            .find(|source| item.id.starts_with(&format!("{}:", source.id)));
        facts.push(detail_field(
            "Origen",
            source.map_or(item.source_name.as_str(), |source| source.location.as_str()),
        ));
        facts.push(detail_field(
            "Licencia",
            item.license
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("No declarada por el origen"),
        ));
    }
    if let Some(installs) = remote_installs(state, &item.id) {
        facts.push(detail_field("Descubierta en", "skills.sh"));
        facts.push(detail_field(
            "Instalaciones",
            &format!("{installs} registradas en skills.sh"),
        ));
    }
    if let Some(scope) = item.installed_scope {
        facts.push(detail_field(
            "Instalada en",
            match scope {
                InstallScope::User => "Usuario: todos tus proyectos",
                InstallScope::Workspace => "Proyecto: sólo el proyecto abierto",
            },
        ));
    }
    if let Some(provenance) = state.provenance.get(&item.id) {
        facts.extend([
            detail_field("Repositorio instalado", &provenance.repository),
            detail_field("Revisión instalada", &provenance.revision),
            detail_field("Carpeta instalada", &provenance.directory),
        ]);
    }
    if let Some(update) = state.updates.get(&item.id) {
        if !state.provenance.contains_key(&item.id) {
            facts.push(detail_field(
                "Revisión instalada",
                update
                    .installed_revision
                    .as_deref()
                    .unwrap_or("Sin revisión registrada"),
            ));
        }
        facts.push(detail_field(
            "Revisión disponible",
            &update.available_revision,
        ));
        facts.push(small("Actualizar reemplaza la copia instalada. La skill conserva su ubicación y su estado de activación.").render());
    }
    if let Some(details) = &starter {
        facts.push(
            SectionHeader::new("Qué instala", palette)
                .sizing(SectionHeaderSizing::LIST)
                .detail(format!(
                    "{} archivos · {} KiB",
                    details.file_count,
                    details.download_bytes.div_ceil(1024)
                ))
                .width(width)
                .render(),
        );
        facts.extend(details.files.iter().take(12).map(|(path, bytes)| {
            Element::row(vec![
                Text::new(path, palette)
                    .role(TextRole::Small)
                    .mono()
                    .truncate()
                    .render()
                    .flex(1.0),
                small(format!("{} KiB", bytes.div_ceil(1024))).render(),
            ])
            .gap(8.0)
        }));
        if details.files.len() > 12 {
            facts.push(small(format!("y {} más", details.files.len() - 12)).render());
        }
    }
    Element::column(facts).gap(8.0).size(Some(width), None)
}

/// The item's SKILL.md when it is on this machine. Browsing never downloads,
/// so an entry that is not installed says where its instructions can be read.
fn detail_readme(state: &MarketplaceViewState, item: &MarketplaceItem, width: f32) -> Element {
    let palette = theme::component_palette();
    let body = state
        .readme
        .as_ref()
        .filter(|readme| readme.item_id == item.id)
        .and_then(|readme| readme.body.as_ref());
    let content = match body {
        None => Element::column(
            SkeletonGroup::rows("marketplace_readme_loading", palette, 5)
                .fractions(&[0.9, 0.72, 0.84, 0.4, 0.66])
                .into_rows(),
        ),
        Some(Ok(Some(source))) if !source.trim().is_empty() => {
            crate::rich_text::render_document_at_width(source, width).element
        }
        Some(Ok(Some(_))) => {
            small("Este SKILL.md no tiene instrucciones además de su descripción.").render()
        }
        Some(Ok(None)) => {
            let mut notice = Notice::new(
                "Las instrucciones se descargan al instalar",
                theme::panel_card_palette(),
            )
            .icon(Icon::Book)
            .description(
                "openADE no descarga nada mientras exploras el catálogo. Puedes leer el SKILL.md en su origen antes de instalar.",
            )
            .framed()
            .width(width);
            if has_source_link(state, item) {
                notice = notice.action(Element::row(vec![
                    Button::new(
                        format!("marketplace_readme_source:{}", item.id),
                        "Abrir origen",
                        palette,
                    )
                    .icon(Icon::Github)
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .render()
                    .dispatch_as(format!("marketplace_open_source:{}", item.id)),
                    Element::spacer().flex(1.0),
                ]));
            }
            notice.render()
        }
        Some(Err(error)) => Notice::new("No se pudieron leer las instrucciones", palette)
            .icon(Icon::WarningTriangle)
            .ink(theme::DESTRUCTIVE)
            .description(error)
            .width(width)
            .render(),
    };
    Element::column(vec![
        SectionHeader::new("Instrucciones", palette)
            .sizing(SectionHeaderSizing::LIST)
            .detail("SKILL.md")
            .width(width)
            .render(),
        content,
    ])
    .gap(8.0)
    .size(Some(width), None)
}

fn detail_actions(state: &MarketplaceViewState, item: &MarketplaceItem) -> Element {
    let palette = theme::component_palette();
    let id = &item.id;
    let working = state.busy_item.as_deref() == Some(id.as_str());
    let menu_key = format!("detail:{id}");
    let menu_button = |icon: Icon, hint: &str| {
        IconButton::new(format!("marketplace_detail_menu:{id}"), icon, palette)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!state.busy)
            .tooltip_with(
                TooltipSpec::new(hint)
                    .placement(TooltipPlacement::Below)
                    .suppressed(state.menu_item.is_some()),
            )
            .render()
    };
    let mut actions = Vec::new();
    if item.installed_scope.is_some() {
        if state.updates.contains_key(id) {
            actions.push(
                theme::primary(format!("marketplace_details_update:{id}"), "Actualizar")
                    .size(ControlSize::Compact)
                    .enabled(!state.busy)
                    .loading(working && state.busy_update)
                    .render()
                    .dispatch_as(format!("marketplace_update:{id}")),
            );
        }
        actions.push(
            Button::new(
                format!("marketplace_details_toggle:{id}"),
                if item.enabled {
                    "Desactivar"
                } else {
                    "Activar"
                },
                palette,
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(!state.busy)
            .loading(working && !state.busy_update)
            .render()
            .dispatch_as(format!("marketplace_toggle:{id}")),
        );
        if item.enabled {
            actions.push(
                Button::new(format!("marketplace_try:{id}"), "Probar", palette)
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .tooltip("Abrir una tarea nueva que pide usar esta skill")
                    .render(),
            );
        }
        actions.push(action_menu(
            state,
            item,
            &menu_key,
            menu_button(Icon::Settings, "Administrar"),
        ));
    } else {
        actions.push(
            theme::primary(
                format!("marketplace_install:{id}"),
                if working {
                    "Instalando…"
                } else {
                    install_label(state.install_scope)
                },
            )
            .size(ControlSize::Compact)
            .enabled(!state.busy && item.source_available)
            .loading(working)
            .render(),
        );
        actions.push(action_menu(
            state,
            item,
            &menu_key,
            menu_button(Icon::ChevronDown, "Elegir dónde instalar"),
        ));
        if has_source_link(state, item) {
            actions.push(
                IconButton::new(
                    format!("marketplace_open_source:{id}"),
                    Icon::Github,
                    palette,
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .tooltip_with(TooltipSpec::new("Abrir origen").placement(TooltipPlacement::Below))
                .render(),
            );
        }
    }
    actions.push(Element::spacer().flex(1.0));
    Element::row(actions).gap(6.0).size(None, Some(28.0))
}

/// The page of one item: who publishes it and what state it is in, the
/// action with its scope and what installing verifies, then its instructions
/// beside the facts. `back` returns to the full catalog; its header also
/// preserves the host's sidebar control and access to source management.
pub(crate) fn item_detail(
    state: &MarketplaceViewState,
    item: &MarketplaceItem,
    available_width: f32,
    back: Option<(&str, &str)>,
    navigation: Option<Element>,
) -> Element {
    let palette = theme::component_palette();
    let page = PageColumn::new("marketplace_detail", available_width, palette)
        .max_width(1040.0)
        .gutter(14.0, 28.0, 640.0)
        .gap(14.0)
        .tint_scrollbar(false);
    let width = page.content_width();
    let mut children = Vec::new();
    if let Some((id, label)) = back {
        let mut controls = Vec::new();
        if let Some(navigation) = navigation {
            controls.push(navigation);
        }
        controls.extend([
            Button::new(id, label, palette)
                .icon(Icon::ArrowLeft)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            Element::spacer().flex(1.0),
            sources_button(width),
        ]);
        children.push(Element::row(controls).gap(8.0));
    }
    let starter = crate::marketplace::starter_details(&item.id);
    let mut byline = vec![
        publisher(item).to_owned(),
        if item.id.starts_with("skills-sh:") {
            "skills.sh".to_owned()
        } else {
            source_label(item.source_kind).to_owned()
        },
    ];
    if let Some(details) = &starter {
        byline.push(format!(
            "rev. {}",
            &details.revision[..7.min(details.revision.len())]
        ));
    }
    byline.push(
        item.license
            .as_deref()
            .filter(|value| !value.is_empty())
            .map_or("Licencia no declarada".to_owned(), str::to_owned),
    );
    let mut chips = vec![Chip::new("Skill", palette).render()];
    if let Some(details) = &starter {
        chips.push(Chip::new(&details.category, palette).outline().render());
    }
    if let Some(scope) = item.installed_scope {
        chips.push(Chip::new(format!("Instalada · {}", scope_label(scope)), palette).render());
        chips.push(
            Chip::new(if item.enabled { "Activa" } else { "Inactiva" }, palette)
                .outline()
                .render(),
        );
        if state.updates.contains_key(&item.id) {
            chips.push(Chip::new("Actualización disponible", palette).render());
        }
    }
    chips.push(Element::spacer().flex(1.0));
    children.push(
        Element::row(vec![
            item_icon(item, 44.0),
            Element::column(vec![
                Text::new(&item.name, palette)
                    .role(TextRole::Display)
                    .truncate()
                    .render(),
                small(byline.join(" · ")).truncate().render(),
                Element::horizontal_scroll(
                    format!("marketplace_detail_chips:{}", item.id),
                    Element::row(chips).gap(6.0),
                )
                .horizontal_edge_fade(theme::BACKGROUND, 12.0)
                .size(None, Some(22.0)),
            ])
            .gap(4.0)
            .flex(1.0),
        ])
        .gap(12.0),
    );
    children.push(
        Text::new(&item.description, palette)
            .role(TextRole::Label)
            .render(),
    );
    children.push(detail_actions(state, item));
    if !item.source_available {
        children.push(
            Text::new("El origen ya no está disponible", palette)
                .ink(theme::DESTRUCTIVE)
                .render(),
        );
    }
    if item.installed_scope.is_none() {
        children.push(
            Notice::new("Antes de instalar", theme::panel_card_palette())
                .icon(Icon::InfoCircle)
                .description(trust_note(item))
                .framed()
                .width(width)
                .render(),
        );
    }
    if !state.sources_open {
        children.extend(status_alerts(state));
    }
    children.push(Separator::horizontal(palette).render());
    if width >= DETAIL_SPLIT_WIDTH {
        let main = width - FACTS_WIDTH - 28.0;
        children.push(
            Element::row(vec![
                detail_readme(state, item, main),
                detail_facts(state, item, FACTS_WIDTH),
            ])
            .gap(28.0),
        );
    } else {
        children.push(detail_readme(state, item, width));
        children.push(Separator::horizontal(palette).render());
        children.push(detail_facts(state, item, width));
    }
    page.children(children).render()
}

/// The detail of the selection, or the landing page with nothing selected.
/// This is the editor area of the IDE while its sidebar holds the list.
pub(crate) fn detail_or_landing(
    state: &MarketplaceViewState,
    width: f32,
    select_prefix: &str,
    header: Option<(&str, Option<Element>)>,
    back: Option<(&str, &str)>,
) -> Element {
    match state.selected() {
        Some(item) => item_detail(state, &item, width, back, None),
        // The IDE sidebar owns its catalog; the center waits for a selection.
        None => {
            let _ = (select_prefix, header);
            Element::column(vec![
                openide_ui::Empty::new(
                    "Elegí una skill de la lista",
                    "Consulta sus instrucciones, procedencia y opciones de instalación.",
                    theme::component_palette(),
                )
                .icon(Icon::Puzzle)
                .compact()
                .width((width - 48.0).max(200.0))
                .height(188.0)
                .render(),
            ])
            .padding(24.0)
        }
    }
}

#[cfg(test)]
pub fn marketplace_page(state: &MarketplaceViewState, available_width: f32) -> Element {
    marketplace_page_with_navigation(state, available_width, None)
}

/// The standalone page. The host can place its sidebar control in the same
/// header row, keeping the title clear of independent top-left overlays.
pub(crate) fn marketplace_page_with_navigation(
    state: &MarketplaceViewState,
    available_width: f32,
    navigation: Option<Element>,
) -> Element {
    with_list(
        state,
        available_width,
        "Marketplace",
        navigation,
        "marketplace_search",
    )
}

/// The full catalog uses the available width; selection opens its detail
/// in the same pane and preserves the host-owned query and filters.
pub(crate) fn with_list(
    state: &MarketplaceViewState,
    available_width: f32,
    title: &str,
    navigation: Option<Element>,
    search_id: &str,
) -> Element {
    const SELECT: &str = "marketplace_details:";
    let palette = theme::component_palette();
    let back = Some(("marketplace_details_close", "Volver al catálogo"));
    let content = if let Some(item) = state.selected() {
        item_detail(state, &item, available_width, back, navigation)
    } else {
        let page = PageColumn::new("marketplace_catalog", available_width, palette)
            .max_width(1280.0)
            .gutter(14.0, 28.0, 640.0)
            .gap(14.0)
            .reserve_rail(true);
        let width = page.content_width();
        let mut sections = vec![page_header(title, "Skills para tu agente", navigation, width)];
        sections.extend(catalog_controls(state, width, search_id));
        if !state.sources_open {
            sections.extend(status_alerts(state));
        }
        if !state.snapshot.diagnostics.is_empty() {
            sections.push(Notice::new("Un origen necesita atención", palette)
                .icon(Icon::WarningTriangle)
                .description("Revisa sus errores en Orígenes.")
                .width(width).render());
        }
        let groups = catalog_groups(state);
        if groups.iter().all(|group| group.key == "remote") {
            sections.push(empty_list(state, width));
        }
        for group in &groups {
            sections.push(SectionHeader::new(group.title, palette)
                .count(group.items.len()).width(width).render());
            if group.key == "remote" {
                sections.extend(remote_status(state, width));
            }
            let grid = CardGrid::new(width, 620.0).columns(if width >= 980.0 { 3 } else { 2 });
            let card_width = grid.item_width();
            let visible = state.visible_by.get(group.key).copied().unwrap_or(state.visible);
            let cards = group.items.iter().take(visible).map(|item| {
                Card::new(item_row(state, item, (card_width - 20.0).max(0.0), SELECT, false, theme::PANEL),
                    theme::panel_card_palette())
                    .density(CardDensity::Regular).width(card_width).render()
            }).collect();
            sections.push(grid.items(cards).render());
            if group.items.len() > visible {
                sections.push(Button::new(format!("marketplace_more:{}", group.key),
                    format!("Mostrar más ({} restantes)", group.items.len() - visible), palette)
                    .variant(ButtonVariant::Ghost).size(ControlSize::Compact).render());
            }
        }
        if state.remote_shown() && !groups.iter().any(|group| group.key == "remote") {
            sections.push(SectionHeader::new("skills.sh", palette).width(width).render());
            sections.extend(remote_status(state, width));
        }
        page.children(sections).render()
    };
    with_source_modal(state, content, available_width)
}

fn source_form_body(state: &MarketplaceViewState) -> Element {
    let kind = state.add_source_kind.expect("source form requires a kind");
    // The field's lines join the status in one column: this card is sized
    // from its body, and a nested column would round its height apart.
    let mut contents = FormField::new(
        match kind {
            SourceKind::Local => "Ruta de la carpeta",
            SourceKind::Github => "URL del repositorio",
        },
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
        theme::component_palette(),
    )
    .hint(match kind {
        SourceKind::Local => "Se detectarán las carpetas que contengan SKILL.md.",
        SourceKind::Github => "Usa una URL de GitHub que contenga archivos SKILL.md.",
    })
    .rows();
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
    ButtonGroup::new(vec![
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
    .align_end()
    .sizing(crate::ui_sizing::MODAL_FOOTER)
    .render()
}

/// Source management and its add form overlay whichever pane owns the opener.
pub(crate) fn with_source_modal(
    state: &MarketplaceViewState,
    page: Element,
    available_width: f32,
) -> Element {
    let page = if state.sources_open {
        let width = Modal::fit_width(available_width, 720.0).max(240.0);
        Modal::new(page, true, "Orígenes", "Carpetas y repositorios donde se descubren skills.",
            "marketplace_sources_close", theme::component_palette())
            .width(width)
            .body(source_manager_body(state, Modal::body_width(width)))
            .footer(Element::row(vec![
                refresh_button(state, "marketplace_refresh", ControlSize::Compact),
                Element::spacer().flex(1.0),
                add_source_menu(state, width < 430.0),
            ]).gap(8.0))
            .initial_focus(if state.busy { "marketplace_sources_close:close" } else { "marketplace_add_menu" })
            .return_focus("marketplace_sources_open")
            .backdrop_dismiss(true)
            .render()
    } else {
        page
    };
    let Some(kind) = state.add_source_kind else {
        return page;
    };
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
    .width(Modal::fit_width(available_width, 460.0).max(240.0))
    .body(source_form_body(state))
    .footer(source_form_footer(state))
    .initial_focus("marketplace_source_draft")
    .return_focus("marketplace_add_menu")
    .backdrop_dismiss(!state.busy)
    .render()
}

#[cfg(test)]
#[path = "marketplace_view_tests.rs"]
mod tests;
