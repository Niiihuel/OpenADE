//! Native sidebar presentations of services already used by the workspace.
use super::*;
use crate::sidebar_chrome as chrome;
use openide_ui::{PanelHeader, SidebarPanel, TooltipPlacement};

pub(super) fn git_sidebar(state: &ViewState, width: f32, height: f32) -> Element {
    use crate::environment_git_ui::{self, EnvironmentGitPanel};
    let inner = (width - chrome::INSET * 2.0).max(0.0);
    if state.environment.workflow.panel != EnvironmentGitPanel::Overview {
        return ScrollArea::new(
            "ide_git_workflow",
            environment_git_ui::build_sidebar(
                &state.environment.workflow,
                // Keep a stable rail budget so long forms never place a
                // commit/remote control beneath the reserved scrollbar.
                (width - 14.0).max(0.0),
                state.running || state.git_action_pending || state.composer_branches.switching(),
            ),
        )
        .render()
        .background(theme::SIDEBAR)
        .size(Some(width), Some(height));
    }
    let workflow = &state.environment.workflow;
    let snapshot = workflow.snapshot.as_ref();
    let idle = workflow.busy.is_none() && !state.git_action_pending;
    let locked = state.running || state.composer_branches.switching();
    // Every entry is an operation the worker has; one it cannot run now is
    // listed and off.
    let item = |action: &str, label: &str, icon: Icon, enabled: bool| -> DropdownMenuEntry {
        DropdownMenuItem::new(action, label)
            .icon(icon)
            .enabled(enabled)
            .into()
    };
    let repository = matches!(state.git_review, Some(Ok(_)));
    let has_remote = snapshot.is_some_and(|snapshot| !snapshot.remotes.is_empty());
    let can_push = snapshot.is_some_and(|snapshot| {
        environment_git_ui::push_blocked_reason(workflow, snapshot, locked || !idle).is_none()
    });
    let more = DropdownMenu::new(
        UiIconButton::new("ide_git_more", Icon::More, theme::sidebar_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Más acciones de Git")
                    .placement(TooltipPlacement::Below)
                    .suppressed(state.ide.git_actions_open),
            )
            .render(),
        vec![
            item(
                "environment_git_fetch",
                "Buscar cambios (fetch)",
                crate::icons::SYNC,
                idle && has_remote,
            ),
            item(
                "environment_git_pull",
                "Recibir (pull, solo avance rápido)",
                Icon::ArrowDown,
                idle && !locked && snapshot.is_some_and(|snapshot| snapshot.upstream.is_some()),
            ),
            item("environment_git_push", "Enviar (push)", Icon::ArrowUp, can_push),
            item(
                "environment_git_open_commit",
                "Enviar a otro destino…",
                Icon::Globe,
                has_remote,
            ),
            DropdownMenuEntry::Separator,
            item("environment_git_open_branch", "Crear rama…", Icon::Add, repository),
            item("environment_git_open_compare", "Comparar rama…", Icon::GitBranch, repository),
            item("environment_git_open_worktree", "Crear worktree…", Icon::Folder, repository),
        ],
        state.ide.git_actions_open,
        "ide_git_more_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .placement(PopoverPlacement::BottomEnd)
    .render();
    let header = PanelHeader::new("Control de versiones", theme::sidebar_palette())
        .trailing(vec![
            UiIconButton::new("review_refresh", Icon::Refresh, theme::sidebar_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .tooltip_with(
                    TooltipSpec::new("Actualizar cambios").placement(TooltipPlacement::Below),
                )
                .render(),
            more,
        ])
        .width(inner)
        .sizing(crate::ui_sizing::SIDEBAR_HEADER)
        .render();
    SidebarPanel::new(
        vec![header, git_panel::navigator_body(state, inner).flex(1.0)],
        theme::sidebar_palette(),
    )
    .size(width, height)
    .render()
}

/// An entry of the extensions sidebar: its name over its state or source.
fn extension_row(
    action: &str,
    name: &str,
    subtitle: &str,
    description: &str,
    selected: bool,
    width: f32,
) -> Element {
    // The open extension keeps the sidebar's hover over its selected fill.
    let palette = openide_ui::ComponentPalette {
        surface: if selected {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        },
        ..theme::action_palette(theme::SIDEBAR, theme::FOREGROUND)
    };
    Tooltip::new(
        ListRow::new(action, name, palette)
            .icon(Icon::Grid)
            // A catalog entry without a source keeps its second line.
            .subtitle(if subtitle.is_empty() { " " } else { subtitle })
            .filled()
            .sizing(crate::ui_sizing::EXTENSION)
            .render()
            .size(Some(width), crate::ui_sizing::EXTENSION.two_line_height),
        action,
        name,
        theme::component_palette(),
    )
    .description(description)
    .placement(TooltipPlacement::Right)
    .render()
}

pub(super) fn extensions_sidebar(state: &ViewState, width: f32, height: f32) -> Element {
    let inventory = &state.marketplace;
    let inner = (width - chrome::INSET * 2.0).max(0.0);
    let list_width = (inner - 14.0).max(0.0);
    let (items, total, page) = marketplace_view::catalog_page(inventory);
    let entries: Vec<_> = items
        .iter()
        .map(|item| {
            let action = format!("ide_extension:{}", item.id);
            let subtitle =
                if inventory.updates.contains_key(&item.id) && item.installed_scope.is_some() {
                    "Actualización disponible"
                } else if item.installed_scope.is_some() {
                    if item.enabled {
                        "Instalada · Activa"
                    } else {
                        "Instalada · Inactiva"
                    }
                } else {
                    &item.source_name
                };
            extension_row(
                &action,
                &item.name,
                subtitle,
                &item.description,
                state.ide.extensions_open
                    && state.ide.extensions_item.as_deref() == Some(item.id.as_str()),
                list_width,
            )
        })
        .collect();
    let list = if entries.is_empty() {
        let explore = inventory.active_tab() == marketplace_view::MarketplaceTab::Explore;
        let message = if explore {
            if inventory.explore.busy {
                "Buscando en skills.sh…"
            } else if inventory.explore.error.is_some() {
                "No se pudo buscar. Volvé a intentarlo con Actualizar."
            } else if inventory.explore.page.is_some() {
                "No se encontraron skills para esta búsqueda."
            } else {
                "Escribí al menos 2 caracteres y presioná Enter."
            }
        } else if inventory.busy {
            "Cargando complementos…"
        } else {
            "No hay skills para mostrar"
        };
        let mut content = vec![UiText::new(message, theme::component_palette())
            .muted()
            .render()];
        if !state.ide.extensions_open {
            content.push(
                Button::new(
                    "ide_extensions_catalog",
                    "Abrir catálogo",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            );
        }
        Element::column(content)
            .gap(10.0)
            .padding(6.0)
            .size(Some(list_width), None)
    } else {
        Element::column(entries).gap(2.0)
    };
    let selected_tab = match inventory.active_tab() {
        marketplace_view::MarketplaceTab::Recommended => "recommended",
        marketplace_view::MarketplaceTab::Explore => "explore",
        marketplace_view::MarketplaceTab::Installed => "installed",
    };
    let filters = Element::horizontal_scroll(
        "ide_marketplace_tabs",
        marketplace_view::tab_strip(inventory, true),
    )
    .reveal_horizontal_item(format!("ide_marketplace_tab:{selected_tab}"))
    .horizontal_edge_fade(theme::SIDEBAR, 12.0)
    .size(Some(inner), Some(32.0));
    SidebarPanel::new(
        vec![
            PanelHeader::new("Complementos", theme::sidebar_palette())
                .trailing(vec![
                    UiIconButton::new(
                        "ide_extensions_refresh",
                        Icon::Refresh,
                        theme::sidebar_palette(),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .dispatch_as(
                        if inventory.active_tab() == marketplace_view::MarketplaceTab::Explore {
                            "marketplace_explore_retry"
                        } else {
                            "marketplace_refresh"
                        },
                    )
                    .tooltip_with(
                        TooltipSpec::new("Actualizar catálogo").placement(TooltipPlacement::Below),
                    )
                    .render(),
                    if state.ide.extensions_open {
                        Element::spacer().size(Some(0.0), Some(24.0))
                    } else {
                        UiIconButton::new(
                            "ide_extensions_open_catalog",
                            Icon::Grid,
                            theme::sidebar_palette(),
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Dense)
                        .dispatch_as("ide_extensions_catalog")
                        .tooltip_with(
                            TooltipSpec::new("Abrir catálogo").placement(TooltipPlacement::Below),
                        )
                        .render()
                    },
                ])
                .width(inner)
                .sizing(crate::ui_sizing::SIDEBAR_HEADER)
                .render(),
            filters,
            marketplace_view::search_controls(inventory, inner, true),
            marketplace_view::pagination_controls(total, page, true),
            ScrollArea::new("ide_extensions_list", list)
                .render()
                .flex(1.0),
        ],
        theme::sidebar_palette(),
    )
    .size(width, height)
    .render()
}

/// The existing catalog supplies installation scopes, source management,
/// confirmations and native forms. Closing it reveals the same editor buffers.
pub(super) fn extensions_center(
    state: &ViewState,
    width: f32,
    height: f32,
    sidebar_controls_visible: bool,
) -> Element {
    Element::column(vec![
        PanelHeader::new("", theme::sidebar_palette())
            .leading(
                UiText::new("Complementos", theme::component_palette())
                    .truncate()
                    .render(),
            )
            .dismiss("ide_extensions_close", "Volver al editor")
            .width(width)
            .sizing(crate::ui_sizing::PANE_HEADER)
            .render(),
        match state.ide.extensions_item.as_deref() {
            Some(item) if state.marketplace.find_item(item).is_some() => {
                marketplace_view::ide_item_detail(&state.marketplace, item, width)
            }
            _ => marketplace_view::ide_catalog_page(
                &state.marketplace,
                width,
                sidebar_controls_visible,
            ),
        }
        .flex(1.0),
    ])
    .background(theme::BACKGROUND)
    .size(Some(width), Some(height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git_review::DiffStage,
        marketplace::{MarketplaceItem, SourceKind},
    };
    use openide_ui::{Size, UiTree};

    #[test]
    fn ide_git_sidebar_keeps_file_navigation_while_the_center_has_a_diff() {
        let mut state = super::super::tests::empty_state();
        state.git_review = Some(Ok(Arc::new(GitReview {
            untracked: vec!["new.rs".into()],
            ..Default::default()
        })));
        state.review_detail = Some(ReviewDetailView {
            path: "new.rs".into(),
            stage: DiffStage::Unstaged,
            page_index: 0,
            content: None,
        });
        let mut tree = UiTree::new(git_sidebar(&state, 240.0, 600.0));
        let frame = tree.layout(Size::new(240.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:0"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "review_detail_back"));
    }

    #[test]
    fn ide_extensions_reuse_real_catalog_install_routes_without_leaving_the_shell() {
        let mut state = super::super::tests::empty_state();
        state.marketplace.snapshot.items.push(MarketplaceItem {
            id: "source:review".into(),
            name: "review".into(),
            description: "Review changes".into(),
            license: None,
            source_name: "Local".into(),
            source_kind: SourceKind::Local,
            installed_scope: None,
            enabled: false,
            source_available: true,
        });
        let mut tree = UiTree::new(extensions_sidebar(&state, 240.0, 600.0));
        let sidebar = tree.layout(Size::new(240.0, 600.0));
        assert!(sidebar
            .regions
            .iter()
            .any(|region| region.id == "ide_extension:source:review"));
        tree.set_root(extensions_center(&state, 800.0, 700.0, true));
        let center = tree.layout(Size::new(800.0, 700.0));
        assert!(center
            .regions
            .iter()
            .any(|region| region.id == "marketplace_install:source:review"));
        assert!(center
            .regions
            .iter()
            .any(|region| region.id == "ide_extensions_close"));
    }
    #[test]
    fn extensions_explore_sidebar_has_one_search_row_and_honest_empty_states() {
        use openide_ui::{DrawCommand, HitKind};
        let mut state = super::super::tests::empty_state();
        state.ide.extensions_open = true;
        state.marketplace.tab = marketplace_view::MarketplaceTab::Explore;
        state.marketplace.explore.query = "quality".into();
        for width in [176.0, 240.0, 320.0] {
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                extensions_sidebar(&state, width, 600.0),
            );
            let frame = tree.layout(Size::new(width, 600.0));
            let field = frame
                .regions
                .iter()
                .find(|r| r.id == "ide_extensions_search")
                .unwrap()
                .rect;
            let submit = frame
                .regions
                .iter()
                .find(|r| r.id == "ide_marketplace_explore_search")
                .unwrap()
                .rect;
            assert_eq!(field.height, 28.0);
            assert_eq!(submit.height, 24.0);
            assert!((field.y + 14.0 - submit.y - 12.0).abs() < 0.01);
            assert!(field.x + field.width <= submit.x);
            assert!(submit.x + submit.width <= width);
            assert!(!frame.regions.iter().any(|r| matches!(
                r.id.as_str(),
                "ide_extensions_catalog" | "ide_extensions_open_catalog"
            )));
            assert!(!frame.commands.iter().any(|cmd| matches!(cmd, DrawCommand::Text { text, .. } if text == "skills.sh" || text == "Buscar" || text == "No hay skills para mostrar")));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == "Escribí al menos 2 caracteres y presioná Enter."));
            context.end_pass().textures_delta.clear();
        }
        state.marketplace.explore.busy = true;
        let frame =
            UiTree::new(extensions_sidebar(&state, 240.0, 600.0)).layout(Size::new(240.0, 600.0));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|block| block.source == "Buscando en skills.sh…"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "ide_marketplace_explore_search" && r.kind == HitKind::Button));
        state.ide.extensions_open = false;
        let frame =
            UiTree::new(extensions_sidebar(&state, 240.0, 600.0)).layout(Size::new(240.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "ide_extensions_open_catalog"));
    }

    #[test]
    fn narrow_extension_sidebar_reveals_selected_tab_without_crossing_panel_bounds() {
        let mut state = super::super::tests::empty_state();
        let width = 176.0;
        let mut tree = UiTree::new(extensions_sidebar(&state, width, 600.0));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(width, 600.0));
        state.marketplace.tab = marketplace_view::MarketplaceTab::Installed;
        tree.set_root(extensions_sidebar(&state, width, 600.0));
        let frame = tree.layout(Size::new(width, 600.0));
        let selected = frame
            .regions
            .iter()
            .find(|region| {
                region.id == "ide_marketplace_tab:installed"
                    && region.kind == openide_ui::HitKind::Button
            })
            .unwrap();
        assert!(selected.rect.x >= 0.0 && selected.rect.x + selected.rect.width <= width);
        let point = openide_ui::Point::new(
            selected.rect.x + selected.rect.width / 2.0,
            selected.rect.y + selected.rect.height / 2.0,
        );
        assert_eq!(frame.hit_test(point), Some("ide_marketplace_tab:installed"));
        tree.handle_event(openide_ui::UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(openide_ui::UiEvent::PointerUp(point)),
            vec![openide_ui::UiAction::Clicked {
                id: "ide_marketplace_tab:installed".into(),
            }]
        );
        let scroll = frame
            .regions
            .iter()
            .find(|region| region.id == "ide_marketplace_tabs")
            .unwrap();
        assert!(scroll.rect.x >= 0.0 && scroll.rect.x + scroll.rect.width <= width);
    }
}
