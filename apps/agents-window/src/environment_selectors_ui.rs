//! Snapshot-backed workspace menus shared by the composer strip and environment.
//! Anchors belong to their host; all choices keep the original snapshot indices.
use crate::{composer_ui::ComposerProps, environment_ui::EnvironmentUiState, theme};
use openide_ui::{
    ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry, DropdownMenuItem,
    Element, FontFamily, Icon, IconButton, MenuHeader, MenuItem, PopoverPlacement, ScrollArea,
    SearchField, SelectorSizing, Skeleton, Tooltip, TooltipPlacement, TooltipSpec,
};

/// Shared project menu. The caller owns the anchor and open scope; item IDs
/// retain their indices in the frozen project snapshot when search filters it.
pub(crate) fn project_selector(
    props: &ComposerProps<'_>,
    trigger: Element,
    placement: PopoverPlacement,
    return_focus: &str,
) -> Element {
    project_selector_presentation(props, trigger, placement, return_focus)
}

fn project_selector_presentation(
    props: &ComposerProps<'_>,
    trigger: Element,
    placement: PopoverPlacement,
    return_focus: &str,
) -> Element {
    let switching = props.branch_picker.is_some_and(|picker| picker.switching());
    if !props.project_menu_open || props.running || switching {
        return trigger;
    }
    let sizing = SelectorSizing::CATALOG;
    let menu_width = sizing.fit_width(props.width - 16.0);
    let inner_width = menu_width - 12.0;
    // Reserve the scroll rail for custom rows even in a short menu, so the
    // path and selection check keep a stable alignment as search changes.
    let row_width = inner_width - sizing.scrollbar_gutter;
    let query = props.project_query.trim().to_lowercase();
    let mut entries = Vec::new();
    for (index, (path, label)) in props.project_options.iter().enumerate() {
        if !query.is_empty()
            && !label.to_lowercase().contains(&query)
            && !path.to_lowercase().contains(&query)
        {
            continue;
        }
        let id = format!("composer_project_select:{index}");
        entries.push(DropdownMenuEntry::custom(
            MenuItem::new(&id, label, theme::menu_palette())
                .icon(Icon::Folder)
                .description(path)
                .selected(path == props.workspace)
                .width(row_width)
                .tooltip_with(
                    TooltipSpec::new(label)
                        .description(path)
                        .placement(TooltipPlacement::Left)
                        .max_width(sizing.tooltip_width),
                )
                .render()
                .size(Some(row_width), Some(sizing.detail_row_height)),
        ));
    }
    if entries.is_empty() {
        entries.push(DropdownMenuEntry::custom(
            Element::text(if query.is_empty() {
                "No hay carpetas recientes."
            } else {
                "No hay carpetas que coincidan con la búsqueda."
            })
            .font_family(FontFamily::Sans)
            .font_size(sizing.label_size)
            .foreground(theme::MENU_MUTED)
            .padding(sizing.padding)
            .size(Some(row_width), None),
        ));
    }
    entries.push(DropdownMenuEntry::Separator);
    entries.push(
        DropdownMenuItem::new("composer_project_browse", "Abrir otra carpeta…")
            .icon(Icon::Folder)
            .into(),
    );
    DropdownMenu::new(
        trigger,
        entries,
        true,
        "composer_project_dismiss",
        theme::menu_palette(),
    )
    .header(
        MenuHeader::new(theme::menu_palette())
            .title("Proyectos")
            .search(
                "composer_project_search",
                props.project_query,
                "Buscar proyecto o ruta",
            )
            .width(inner_width)
            .render(),
    )
    .context_menu_style()
    .sizing(sizing)
    .width(menu_width)
    .placement(placement)
    .max_visible_items(sizing.max_visible_items)
    .render()
    .popover_initial_focus("composer_project_search")
    .popover_return_focus(return_focus)
}

/// Same branch snapshot and revision/index actions in either composer or
/// Environment. Only the caller's origin owns the visible menu and focus.
pub(crate) fn branch_selector(
    props: &ComposerProps<'_>,
    trigger: Element,
    placement: PopoverPlacement,
    return_focus: &str,
) -> Element {
    branch_selector_presentation(props, trigger, placement, return_focus)
}

fn branch_selector_presentation(
    props: &ComposerProps<'_>,
    trigger: Element,
    placement: PopoverPlacement,
    return_focus: &str,
) -> Element {
    let Some(picker) = props.branch_picker.filter(|_| props.branch_menu_open) else {
        return trigger;
    };
    let sizing = SelectorSizing::CATALOG;
    let menu_width = sizing.fit_width(props.width - 16.0);
    let inner_width = menu_width - 12.0;
    let message = |text: &str| {
        DropdownMenuEntry::custom(
            Element::text(text)
                .font_family(FontFamily::Sans)
                .font_size(sizing.label_size)
                .foreground(theme::MENU_MUTED)
                .padding(sizing.padding)
                .size(Some(inner_width - sizing.scrollbar_gutter), None),
        )
    };
    let mut entries = Vec::new();
    if let Some(target) = picker.switching.as_deref() {
        entries.push(message("Cambiando de rama…"));
        entries.push(DropdownMenuEntry::Heading(target.to_owned()));
    } else if picker.loading {
        for index in 0..3 {
            entries.push(DropdownMenuEntry::custom(
                Element::panel(
                    Skeleton::block(
                        format!("composer_branch_loading:{index}"),
                        theme::menu_palette(),
                        inner_width - sizing.padding * 2.0,
                        sizing.icon_size,
                    )
                    .render(),
                )
                .padding(sizing.padding)
                .size(Some(inner_width), Some(sizing.row_height)),
            ));
        }
    } else {
        if let Some(error) = picker.error.as_deref() {
            entries.push(DropdownMenuEntry::custom(
                ScrollArea::new(
                    "composer_branch_error",
                    Element::text(error)
                        .font_family(FontFamily::Sans)
                        .font_size(sizing.label_size)
                        .foreground(theme::MENU_MUTED)
                        .padding(sizing.padding)
                        .size(Some(inner_width - sizing.scrollbar_gutter), None),
                )
                .render()
                .size(Some(inner_width), Some(104.0)),
            ));
            entries.push(DropdownMenuEntry::Separator);
        }
        if let Some(reason) = picker.blocked_reason {
            entries.push(message(reason));
        }
        let query = picker.query.trim().to_lowercase();
        let mut count = 0;
        if let Some(snapshot) = picker.snapshot.as_ref() {
            for remote in [false, true] {
                let matching = snapshot
                    .branches
                    .iter()
                    .enumerate()
                    .filter(|(_, branch)| {
                        branch.remote == remote && branch.name.to_lowercase().contains(&query)
                    })
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    continue;
                }
                entries.push(DropdownMenuEntry::Heading(
                    if remote { "Remotas" } else { "Locales" }.into(),
                ));
                for (index, branch) in matching {
                    let mut item = DropdownMenuItem::new(
                        format!("composer_branch_select:{}:{index}", picker.revision),
                        &branch.name,
                    )
                    .icon(Icon::GitBranch)
                    .selected(branch.current);
                    if branch.other_worktree.is_some() {
                        item = item.annotation("En otra carpeta").disabled();
                    } else if picker.blocked_reason.is_some() && !branch.current {
                        item = item.disabled();
                    }
                    entries.push(item.into());
                    count += 1;
                }
            }
            if count == 0 {
                entries.push(message(if query.is_empty() {
                    "Todavía no hay ramas con commits en este repositorio."
                } else {
                    "No hay ramas que coincidan con la búsqueda."
                }));
            }
        }
    }
    entries.push(DropdownMenuEntry::Separator);
    let create =
        DropdownMenuItem::new("environment_git_open_branch", "Crear nueva rama…").icon(Icon::Add);
    let can_create = !props.running
        && !picker.loading
        && !picker.switching()
        && picker.blocked_reason.is_none()
        && picker.snapshot.is_some();
    entries.push(
        if can_create {
            create
        } else {
            create.disabled()
        }
        .into(),
    );
    let mut search_palette = theme::menu_palette();
    search_palette.raised = theme::INPUT;
    let header = Element::column(vec![
        Element::row(vec![
            Element::text("Cambiar rama")
                .font_family(FontFamily::Sans)
                .font_size(sizing.label_size)
                .foreground(theme::FOREGROUND)
                .flex(1.0),
            IconButton::new(
                "composer_branch_refresh",
                Icon::Refresh,
                theme::menu_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(!picker.loading && !picker.switching())
            .tooltip("Actualizar ramas del repositorio")
            .tooltip_placement(TooltipPlacement::Below)
            .render()
            .size(Some(28.0), Some(28.0)),
        ])
        .padding((sizing.header_height - ControlSize::Compact.height()) / 2.0)
        .size(Some(inner_width), Some(sizing.header_height)),
        SearchField::new(
            "composer_branch_search",
            &picker.query,
            "Buscar rama",
            search_palette,
        )
        .size(ControlSize::Compact)
        .width(inner_width)
        .render(),
    ])
    .gap(2.0);
    DropdownMenu::new(
        trigger,
        entries,
        true,
        "composer_branch_dismiss",
        theme::menu_palette(),
    )
    .header(header)
    .context_menu_style()
    .sizing(sizing)
    .width(menu_width)
    .placement(placement)
    .max_visible_items(sizing.max_visible_items)
    .render()
    .popover_initial_focus("composer_branch_search")
    .popover_return_focus(return_focus)
}

/// A compact local-environment control for the strip above the composer.
/// It owns a separate popup from the environment panel; opening one does not
/// duplicate the same menu at two anchors.
pub(crate) fn local_selector(
    environment: &EnvironmentUiState,
    workspace: &str,
    locked: bool,
    width: f32,
) -> Element {
    let width = width.max(80.0);
    let trigger = theme::interactive_surface(Element::clickable(
        "composer_environment_toggle",
        Element::row(vec![
            Element::icon(Icon::Laptop)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(16.0), Some(16.0)),
            Element::text("Local")
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND_SUBTLE)
                .flex(1.0)
                .size(None, Some(16.0)),
            Element::icon(Icon::ChevronDown)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(12.0), Some(16.0)),
        ])
        .gap(5.0)
        .size(Some((width - 12.0).max(0.0)), Some(16.0)),
    ))
    .padding(6.0)
    .size(Some(width), Some(28.0));
    let trigger = Tooltip::new(
        trigger,
        "composer_environment_toggle",
        "Entorno local",
        theme::component_palette(),
    )
    .description(workspace)
    .placement(TooltipPlacement::Above)
    .render();
    if !environment.composer_local_open {
        return trigger;
    }
    let workflow = &environment.workflow;
    let busy = locked || workflow.loading || workflow.busy.is_some();
    let mut entries = Vec::new();
    if workflow.loading {
        entries.push(DropdownMenuEntry::Message(
            "Actualizando carpetas de trabajo…".into(),
        ));
    } else if let Some(snapshot) = &workflow.snapshot {
        entries.push(DropdownMenuEntry::Heading("Carpetas de trabajo".into()));
        for (index, worktree) in snapshot.worktrees.iter().enumerate() {
            let path = worktree.path.to_string_lossy();
            let name = worktree
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Carpeta de trabajo");
            let id = format!(
                "environment_worktree_select:{}:{index}",
                workflow.snapshot_revision
            );
            let label = worktree
                .branch
                .as_deref()
                .map_or_else(|| name.to_owned(), |branch| format!("{name} · {branch}"));
            let enabled = !busy || worktree.current;
            let visual = Element::row(vec![
                Element::icon(Icon::Folder)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(16.0), Some(28.0)),
                Element::column(vec![
                    Element::truncated_text(label)
                        .font_family(FontFamily::Sans)
                        .font_size(12.0)
                        .foreground(if enabled {
                            theme::FOREGROUND
                        } else {
                            theme::MENU_MUTED
                        })
                        .size(None, Some(14.0)),
                    Element::truncated_text(path.as_ref())
                        .font_family(FontFamily::Sans)
                        .font_size(11.0)
                        .foreground(theme::MENU_MUTED)
                        .size(None, Some(14.0)),
                ])
                .flex(1.0),
                if worktree.current {
                    Element::icon(Icon::Check).foreground(theme::FOREGROUND)
                } else {
                    Element::spacer()
                }
                .size(Some(16.0), Some(28.0)),
            ])
            .gap(6.0)
            .clip_children();
            let row = if enabled {
                Element::clickable(&id, visual).action_menu_item()
            } else {
                Element::panel(visual)
            };
            entries.push(DropdownMenuEntry::custom(
                Tooltip::new(
                    theme::menu_palette()
                        .interactive_surface(row)
                        .transition_ms(0)
                        .radius(4.0)
                        .padding(6.0)
                        .size(None, Some(40.0)),
                    id,
                    name,
                    theme::menu_palette(),
                )
                .description(path.as_ref())
                .placement(TooltipPlacement::Right)
                .render()
                .size(None, Some(40.0)),
            ));
        }
    } else {
        entries.push(DropdownMenuEntry::Message(
            if workflow.error.is_some() {
                "No se pudo consultar Git en esta carpeta."
            } else {
                "Trabaja en esta carpeta local."
            }
            .into(),
        ));
        let refresh =
            DropdownMenuItem::new("environment_git_refresh", "Actualizar").icon(Icon::Refresh);
        entries.push(
            if workflow.busy.is_some() {
                refresh.disabled()
            } else {
                refresh
            }
            .into(),
        );
    }
    entries.push(DropdownMenuEntry::Separator);
    let choose =
        DropdownMenuItem::new("environment_choose_project", "Elegir proyecto…").icon(Icon::Folder);
    entries.push(if locked { choose.disabled() } else { choose }.into());
    entries.push(
        DropdownMenuItem::new("environment_local_files", "Explorar archivos")
            .icon(Icon::Folder)
            .into(),
    );
    let copy = DropdownMenuItem::new("environment_copy_path", "Copiar ruta").icon(Icon::File);
    entries.push(
        if workspace.is_empty() {
            copy.disabled()
        } else {
            copy
        }
        .into(),
    );
    entries.push(DropdownMenuEntry::Separator);
    let create =
        DropdownMenuItem::new("environment_git_open_worktree", "Crear worktree…").icon(Icon::Add);
    let can_create = !busy
        && workflow
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.head.is_some());
    entries.push(
        if can_create {
            create
        } else {
            create.disabled()
        }
        .into(),
    );
    DropdownMenu::new(
        trigger,
        entries,
        true,
        "composer_environment_dismiss",
        theme::menu_palette(),
    )
    .header(
        MenuHeader::new(theme::menu_palette())
            .title("Entorno local")
            .render(),
    )
    .context_menu_style()
    .width(340.0)
    .max_visible_items(8)
    .placement(PopoverPlacement::TopStart)
    .render()
    .popover_return_focus("composer_environment_toggle")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{
        DrawCommand, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent, UiTree,
    };

    fn anchored(element: Element, width: f32) -> UiTree {
        let mut tree = UiTree::new(
            Element::column(vec![Element::spacer().size(None, Some(420.0)), element])
                .size(Some(width), None),
        );
        tree.set_reduced_motion(true);
        tree
    }
    fn rect(frame: &Frame, id: &str) -> Rect {
        frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .rect
    }
    fn click(tree: &mut UiTree, rect: Rect) -> Vec<UiAction> {
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }
    fn labels(frame: &Frame) -> Vec<&str> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "Fixture",
            ],
            vec!["branch", "feature/alpha"],
            vec!["branch", "feature/beta"],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        dir
    }

    #[test]
    fn project_search_preserves_snapshot_indices_and_popup_anchor() {
        for width in [280.0, 360.0, 620.0] {
            let mut state = crate::view::tests::empty_state();
            state.composer_project_menu_open = true;
            state.workspace = "/work/a".into();
            state.composer_project_options = vec![
                ("/work/a".into(), "Alpha".into()),
                ("/work/b".into(), "Beta".into()),
                ("/else/b".into(), "Beta".into()),
            ];
            state.environment.project_query = "/else".into();
            let props = ComposerProps::from_state(&state, width, false, None, "", None);
            let trigger = Element::clickable("composer_project_toggle", Element::text("Proyecto"))
                .size(Some(100.0), Some(28.0));
            let mut tree = anchored(
                project_selector(
                    &props,
                    trigger,
                    PopoverPlacement::TopStart,
                    "composer_project_toggle",
                ),
                width,
            );
            let frame = tree.layout(Size::new(width, 500.0));
            let anchor = rect(&frame, "composer_project_toggle");
            let search = rect(&frame, "composer_project_search");
            assert!(search.y + search.height <= anchor.y);
            assert_eq!(tree.focused_id(), Some("composer_project_search"));
            assert!(!frame.regions.iter().any(
                |r| r.id == "composer_project_select:0" || r.id == "composer_project_select:1"
            ));
            let selected = rect(&frame, "composer_project_select:2");
            assert_eq!(
                click(&mut tree, selected),
                vec![UiAction::Clicked {
                    id: "composer_project_select:2".into()
                }]
            );
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|r| r.id == "composer_project_browse")
            );
        }
    }

    #[test]
    fn branch_search_keeps_revision_scoped_action_and_real_creation_footer() {
        let repo = repo();
        let mut state = crate::view::tests::empty_state();
        state.workspace = repo.path().to_string_lossy().into_owned();
        state.composer_branches.snapshot =
            Some(crate::git_branches::load_branches(repo.path()).unwrap());
        state.composer_branches.open = true;
        state.composer_branches.revision = 73;
        state.composer_branches.query = "beta".into();
        let selected = state
            .composer_branches
            .snapshot
            .as_ref()
            .unwrap()
            .branches
            .iter()
            .position(|branch| branch.name == "feature/beta")
            .unwrap();
        let props = ComposerProps::from_state(&state, 360.0, false, None, "", None);
        let trigger = Element::clickable("composer_branch_toggle", Element::text("Rama"))
            .size(Some(100.0), Some(28.0));
        let mut tree = anchored(
            branch_selector(
                &props,
                trigger,
                PopoverPlacement::TopStart,
                "composer_branch_toggle",
            ),
            360.0,
        );
        let frame = tree.layout(Size::new(360.0, 500.0));
        let id = format!("composer_branch_select:73:{selected}");
        assert_eq!(
            click(&mut tree, rect(&frame, &id)),
            vec![UiAction::Clicked { id }]
        );
        assert!(labels(&frame).contains(&"Crear nueva rama…"));
        assert!(
            frame
                .regions
                .iter()
                .any(|r| r.id == "environment_git_open_branch" && r.kind == HitKind::Button)
        );
        assert!(!labels(&frame).contains(&"feature/alpha"));
    }

    #[test]
    fn local_popup_has_one_anchor_no_cloud_and_versioned_worktree_actions() {
        let repo = repo();
        let mut environment = EnvironmentUiState::default();
        environment.composer_local_open = true;
        environment.workflow.snapshot_revision = 31;
        environment.workflow.snapshot =
            Some(crate::environment_git::load_snapshot(repo.path()).unwrap());
        let workspace = repo.path().to_string_lossy();
        for width in [280.0, 360.0, 620.0] {
            let mut tree = anchored(local_selector(&environment, &workspace, false, 88.0), width);
            let frame = tree.layout(Size::new(width, 500.0));
            let anchor = rect(&frame, "composer_environment_toggle");
            assert_eq!(anchor.height, 28.0);
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|r| r.id == "environment_local_toggle")
            );
            let option = rect(&frame, "environment_worktree_select:31:0");
            assert!(option.y + option.height <= anchor.y);
            assert!(option.x >= 0.0 && option.x + option.width <= width + 0.1);
            assert!(!labels(&frame).iter().any(|label| label.contains("Cloud")));
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|r| r.id == "environment_git_open_worktree")
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    ctrl: false,
                    shift: false
                }),
                vec![UiAction::Clicked {
                    id: "composer_environment_dismiss".into()
                }]
            );
        }
    }

    #[test]
    fn local_loading_never_exposes_old_snapshot_actions_and_branch_creation_is_disabled() {
        let mut state = crate::view::tests::empty_state();
        state.environment.composer_local_open = true;
        state.environment.workflow.loading = true;
        let mut tree = anchored(
            local_selector(&state.environment, "/project", true, 88.0),
            360.0,
        );
        let frame = tree.layout(Size::new(360.0, 500.0));
        assert!(!frame.regions.iter().any(|r| r.kind == HitKind::Button
            && (r.id.starts_with("environment_worktree_select:")
                || r.id == "environment_git_open_worktree"
                || r.id == "environment_choose_project")));
        assert!(
            !frame
                .regions
                .iter()
                .any(|r| r.id == "environment_git_refresh")
        );
        state.environment.workflow.loading = false;
        state.environment.workflow.error =
            Some("fatal: technical diagnostics without user context".into());
        let mut tree = anchored(
            local_selector(&state.environment, "/project", false, 88.0),
            360.0,
        );
        let frame = tree.layout(Size::new(360.0, 500.0));
        assert!(labels(&frame).contains(&"No se pudo consultar Git en esta carpeta."));
        assert!(!labels(&frame).iter().any(|label| label.contains("fatal:")));
        assert!(
            !frame
                .regions
                .iter()
                .any(|r| r.id == "environment_git_open_worktree" && r.kind == HitKind::Button)
        );
        assert_eq!(
            click(&mut tree, rect(&frame, "environment_git_refresh")),
            vec![UiAction::Clicked {
                id: "environment_git_refresh".into()
            }],
        );
        state.environment.workflow.error = None;
        let frame = anchored(
            local_selector(&state.environment, "/project", false, 88.0),
            360.0,
        )
        .layout(Size::new(360.0, 500.0));
        assert!(labels(&frame).contains(&"Trabaja en esta carpeta local."));
        assert!(!labels(&frame).contains(&"No se pudo consultar Git en esta carpeta."));
        state.composer_branches.open = true;
        state.composer_branches.loading = true;
        let props = ComposerProps::from_state(&state, 360.0, false, None, "", None);
        let mut tree = anchored(
            branch_selector(
                &props,
                Element::clickable("composer_branch_toggle", Element::text("Rama"))
                    .size(Some(88.0), Some(28.0)),
                PopoverPlacement::TopStart,
                "composer_branch_toggle",
            ),
            360.0,
        );
        let frame = tree.layout(Size::new(360.0, 500.0));
        assert!(
            !frame
                .regions
                .iter()
                .any(|r| r.id == "environment_git_open_branch" && r.kind == HitKind::Button)
        );
    }
}
