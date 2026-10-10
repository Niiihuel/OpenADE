//! Native sidebar presentations of services already used by the workspace.
use super::*;
use crate::sidebar_chrome as chrome;
use openide_ui::TooltipPlacement;

fn caption(text: impl Into<String>) -> Element {
    Element::truncated_text(text)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(theme::FOREGROUND)
}

fn tool(id: &str, action: &str, icon: Icon, hint: &str) -> Element {
    chrome::tool(id, action, icon, hint)
}

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
    let more = DropdownMenu::new(
        tool(
            "ide_git_more",
            "ide_git_more",
            Icon::More,
            "Más acciones de Git",
        ),
        vec![
            DropdownMenuItem::new("environment_git_open_compare", "Comparar rama")
                .icon(Icon::GitBranch)
                .into(),
            DropdownMenuItem::new("environment_git_open_worktree", "Crear worktree…")
                .icon(Icon::Folder)
                .into(),
        ],
        state.ide.git_actions_open,
        "ide_git_more_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .placement(PopoverPlacement::BottomEnd)
    .render();
    let header = chrome::header(
        chrome::title("Control de versiones"),
        vec![
            tool(
                "review_refresh",
                "review_refresh",
                Icon::Refresh,
                "Actualizar cambios",
            ),
            tool(
                "ide_git_branch",
                "environment_git_open_branch",
                Icon::GitBranch,
                "Crear rama",
            ),
            more,
        ],
        inner,
    );
    let commit = Button::new("ide_git_commit", "Commit y publicación", crate::theme::sidebar_palette())
        .icon(Icon::Check)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render()
        .dispatch_as("environment_git_open_commit")
        .size(Some(inner), Some(28.0));
    chrome::panel(
        vec![
            header,
            commit,
            git_panel::navigator_body(state, inner).flex(1.0),
        ],
        width,
        height,
    )
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
            Tooltip::new(
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND).interactive_surface(
                    Element::clickable(
                        &action,
                        Element::row(vec![
                            Element::icon(Icon::Grid)
                                .foreground(theme::FOREGROUND_SUBTLE)
                                .size(Some(16.0), Some(16.0)),
                            Element::column(vec![
                                caption(&item.name),
                                caption(subtitle)
                                    .font_size(10.0)
                                    .foreground(theme::FOREGROUND_SUBTLE),
                            ])
                            .gap(2.0)
                            .flex(1.0),
                        ])
                        .gap(7.0)
                        .padding(5.0),
                    )
                    .background(
                        if state.ide.extensions_open
                            && state.ide.extensions_item.as_deref() == Some(item.id.as_str())
                        {
                            theme::SELECTED
                        } else {
                            theme::SIDEBAR
                        },
                    )
                    .size(Some(list_width), Some(42.0))
                    .radius(4.0),
                ),
                action,
                &item.name,
                theme::component_palette(),
            )
            .description(&item.description)
            .placement(TooltipPlacement::Right)
            .render()
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
        let mut content = vec![Element::text(message)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND_SUBTLE)];
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
    chrome::panel(
        vec![
            chrome::header(
                chrome::title("Complementos"),
                vec![
                    tool(
                        "ide_extensions_refresh",
                        if inventory.active_tab() == marketplace_view::MarketplaceTab::Explore {
                            "marketplace_explore_retry"
                        } else {
                            "marketplace_refresh"
                        },
                        Icon::Refresh,
                        "Actualizar catálogo",
                    ),
                    if state.ide.extensions_open {
                        Element::spacer().size(Some(0.0), Some(24.0))
                    } else {
                        tool(
                            "ide_extensions_open_catalog",
                            "ide_extensions_catalog",
                            Icon::Grid,
                            "Abrir catálogo",
                        )
                    },
                ],
                inner,
            ),
            filters,
            marketplace_view::search_controls(inventory, inner, true),
            marketplace_view::pagination_controls(total, page, true),
            ScrollArea::new("ide_extensions_list", list)
                .render()
                .flex(1.0),
        ],
        width,
        height,
    )
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
        Element::row(vec![
            caption("Complementos").flex(1.0),
            tool(
                "ide_extensions_close",
                "ide_extensions_close",
                Icon::Close,
                "Volver al editor",
            ),
        ])
        .gap(4.0)
        .padding(4.0)
        .size(Some(width), Some(32.0)),
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
