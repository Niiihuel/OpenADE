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
    let more = DropdownMenu::new(
        UiIconButton::new("ide_git_more", Icon::More, theme::sidebar_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Más acciones de Git").placement(TooltipPlacement::Below),
            )
            .render(),
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
    let header = PanelHeader::new("Control de versiones", theme::sidebar_palette())
        .trailing(vec![
            UiIconButton::new("review_refresh", Icon::Refresh, theme::sidebar_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .tooltip_with(
                    TooltipSpec::new("Actualizar cambios").placement(TooltipPlacement::Below),
                )
                .render(),
            UiIconButton::new("ide_git_branch", Icon::GitBranch, theme::sidebar_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .dispatch_as("environment_git_open_branch")
                .tooltip_with(TooltipSpec::new("Crear rama").placement(TooltipPlacement::Below))
                .render(),
            more,
        ])
        .width(inner)
        .sizing(crate::ui_sizing::SIDEBAR_HEADER)
        .render();
    let commit = Button::new("ide_git_commit", "Commit y publicación", crate::theme::sidebar_palette())
        .icon(Icon::Check)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render()
        .dispatch_as("environment_git_open_commit")
        .size(Some(inner), Some(28.0));
    SidebarPanel::new(
        vec![
            header,
            commit,
            git_panel::navigator_body(state, inner).flex(1.0),
        ],
        theme::sidebar_palette(),
    )
    .size(width, height)
    .render()
}

/// The list of the extensions activity. It is the only place the catalog is
/// enumerated: selecting a row opens that item in the editor area.
pub(crate) fn extensions_sidebar(state: &ViewState, width: f32, height: f32) -> Element {
    let inventory = &state.marketplace;
    let inner = (width - chrome::INSET * 2.0).max(0.0);
    let mut blocks = vec![PanelHeader::new("Complementos", theme::sidebar_palette())
        .trailing(vec![
            UiIconButton::new(
                "ide_extensions_refresh",
                Icon::Refresh,
                theme::sidebar_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .enabled(!inventory.busy)
            .loading(inventory.busy && inventory.busy_item.is_none())
            .dispatch_as("marketplace_refresh")
            .tooltip_with(
                TooltipSpec::new("Actualizar orígenes")
                    .description("Vuelve a leer los orígenes y busca actualizaciones.")
                    .placement(TooltipPlacement::Below),
            )
            .render(),
            UiIconButton::new(
                "ide_extensions_open_catalog",
                Icon::Book,
                theme::sidebar_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .active(state.ide.extensions_open && inventory.selected_item_id.is_none())
            .dispatch_as("ide_extensions_catalog")
            .tooltip_with(
                TooltipSpec::new("Inicio y orígenes")
                    .description("Recomendadas, instaladas y de dónde viene el catálogo.")
                    .placement(TooltipPlacement::Below),
            )
            .render(),
        ])
        .width(inner)
        .sizing(crate::ui_sizing::SIDEBAR_HEADER)
        .render()];
    blocks.extend(marketplace_view::catalog_list(
        inventory,
        inner,
        "ide_extensions_search",
        "ide_extension:",
        theme::SIDEBAR,
        None,
    ));
    SidebarPanel::new(blocks, theme::sidebar_palette())
        .size(width, height)
        .render()
}

/// The editor area of the extensions activity: the selected item, or the
/// landing page. With the sidebar hidden it carries the list as well, so
/// nothing is out of reach. Closing it reveals the same editor buffers.
pub(super) fn extensions_center(
    state: &ViewState,
    width: f32,
    height: f32,
    sidebar_list_visible: bool,
) -> Element {
    let body = if sidebar_list_visible {
        marketplace_view::with_source_modal(
            &state.marketplace,
            marketplace_view::detail_or_landing(
                &state.marketplace,
                width,
                "ide_extension:",
                Some(("Complementos", None)),
                None,
            ),
            width,
        )
    } else {
        marketplace_view::with_list(
            &state.marketplace,
            width,
            "Complementos",
            None,
            "ide_extensions_search",
        )
    };
    Element::column(vec![
        PanelHeader::new("", theme::sidebar_palette())
            .leading(
                UiText::new(
                    state
                        .marketplace
                        .selected_item_id
                        .as_deref()
                        .and_then(|id| state.marketplace.find_item(id))
                        .map_or("Complementos".to_owned(), |item| {
                            format!("Complementos · {}", item.name)
                        }),
                    theme::component_palette(),
                )
                .truncate()
                .render(),
            )
            .dismiss("ide_extensions_close", "Volver al editor")
            .width(width)
            .sizing(crate::ui_sizing::PANE_HEADER)
            .render(),
        body.flex(1.0),
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

    fn review_skill() -> MarketplaceItem {
        MarketplaceItem {
            id: "source:review".into(),
            name: "review".into(),
            description: "Review changes".into(),
            license: None,
            source_name: "Local".into(),
            source_kind: SourceKind::Local,
            installed_scope: None,
            enabled: false,
            source_available: true,
        }
    }

    fn has(frame: &openide_ui::Frame, id: &str) -> bool {
        frame.regions.iter().any(|region| region.id == id)
    }

    #[test]
    fn the_sidebar_is_the_only_list_and_the_center_shows_the_selection() {
        let mut state = super::super::tests::empty_state();
        state.marketplace.snapshot.items.push(review_skill());
        state.ide.extensions_open = true;
        let mut tree = UiTree::new(extensions_sidebar(&state, 240.0, 600.0));
        let sidebar = tree.layout(Size::new(240.0, 600.0));
        assert!(has(&sidebar, "ide_extension:source:review"));
        assert!(has(&sidebar, "ide_extensions_search"));
        assert!(has(&sidebar, "marketplace_row_menu:source:review"));

        // Nothing selected: the landing page, which lists sources and no rows.
        tree.set_root(extensions_center(&state, 800.0, 700.0, true));
        let landing = tree.layout(Size::new(800.0, 700.0));
        assert!(has(&landing, "marketplace_add_menu"));
        assert!(has(&landing, "ide_extensions_close"));
        assert!(!has(&landing, "ide_extension:source:review"));
        assert!(!has(&landing, "ide_extensions_search"));
        assert!(!landing.regions.iter().any(|r| r.id.contains("page_next")));

        state.marketplace.selected_item_id = Some("source:review".into());
        tree.set_root(extensions_center(&state, 800.0, 700.0, true));
        let detail = tree.layout(Size::new(800.0, 700.0));
        assert!(has(&detail, "marketplace_install:source:review"));
        assert!(has(&detail, "marketplace_detail_menu:source:review"));
        assert!(!has(&detail, "ide_extensions_search"));
    }

    #[test]
    fn a_hidden_sidebar_leaves_the_list_in_the_center() {
        let mut state = super::super::tests::empty_state();
        state.marketplace.snapshot.items.push(review_skill());
        for width in [420.0, 900.0] {
            let mut tree = UiTree::new(extensions_center(&state, width, 700.0, false));
            let frame = tree.layout(Size::new(width, 700.0));
            assert!(has(&frame, "ide_extensions_search"), "{width}");
            assert!(has(&frame, "marketplace_details:source:review"), "{width}");
            assert!(has(&frame, "marketplace_add_menu"), "{width}");
        }
    }

    #[test]
    fn the_sidebar_list_fits_from_240_to_480_without_crossing_its_bounds() {
        let mut state = super::super::tests::empty_state();
        let mut item = review_skill();
        item.name = "a-skill-with-a-very-long-descriptive-name".into();
        item.description = "A long description that cannot fit in one line of a sidebar".into();
        state.marketplace.snapshot.items.push(item);
        for width in [240.0, 320.0, 480.0] {
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                extensions_sidebar(&state, width, 600.0),
            );
            let frame = tree.layout(Size::new(width, 600.0));
            for region in frame.regions.iter().filter(|region| {
                region.id.starts_with("ide_extension")
                    || region.id.starts_with("marketplace_")
            }) {
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01,
                    "{} at {width}: {:?}",
                    region.id,
                    region.rect
                );
            }
            let field = frame
                .regions
                .iter()
                .find(|region| region.id == "ide_extensions_search")
                .unwrap()
                .rect;
            let filter = frame
                .regions
                .iter()
                .find(|region| region.id == "marketplace_filter_toggle")
                .unwrap()
                .rect;
            assert!(field.x + field.width <= filter.x);
            assert_eq!(field.height, 28.0);
            context.end_pass().textures_delta.clear();
        }
    }
}
