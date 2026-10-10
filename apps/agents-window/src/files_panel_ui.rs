//! Project files inside the right dock: compact tools and a virtual tree.
use crate::{theme, view::ViewState};
use openide_ui::{
    guides_for_depths, Button, ButtonSize, ButtonVariant, Color, Component, ControlSize, Element,
    FontFamily, Icon, IconButton, Notice, ScrollArea, Text, Tooltip, TooltipPlacement, TooltipSpec,
    TreeRow,
};
use std::sync::Arc;

const ROW_HEIGHT: f32 = 24.0;
const TOOL_SIZE: f32 = crate::dock_tab_ui::TOOL_SIZE;
const INSET: f32 = 8.0;

/// What the panel says in place of the tree, with the one way out.
fn empty_state(status: &str, palette: openide_ui::ComponentPalette) -> Element {
    Notice::new(
        if status.starts_with("No se pudo") {
            "No se pudieron cargar los archivos"
        } else {
            "Carpeta vacía"
        },
        theme::component_palette(),
    )
    .description(if status.is_empty() {
        "Abre un proyecto para ver sus archivos."
    } else {
        status
    })
    .action(
        Button::new("files_refresh", "Volver a cargar", palette)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Small)
            .render(),
    )
    .sizing(crate::ui_sizing::FILES_EMPTY)
    .render()
}

pub(crate) fn build(state: &ViewState, width: f32) -> Element {
    build_contents(state, width, theme::BACKGROUND, false)
}

/// The IDE owns the explorer's single compact header on the gray sidebar.
pub(crate) fn build_ide(state: &ViewState, width: f32) -> Element {
    build_contents(state, width, theme::SIDEBAR, true)
}

fn build_contents(state: &ViewState, width: f32, surface: Color, compact: bool) -> Element {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    let inset = if compact {
        crate::sidebar_chrome::INSET
    } else {
        INSET
    };
    let inner = (width - inset * 2.0).max(0.0);
    let workspace = state.workspace.trim_end_matches(['/', '\\']);
    let project = workspace
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("Proyecto");
    let workspace_title = Tooltip::for_path(
        Element::row(vec![
            Element::icon(openide_ui::folder_icon(project, true)).size(Some(16.0), Some(TOOL_SIZE)),
            Element::faded_text(project, 16.0)
                .font_family(FontFamily::Sans)
                .font_size(if compact { 12.0 } else { 13.0 })
                .foreground(theme::FOREGROUND)
                .flex(1.0)
                .size(None, Some(TOOL_SIZE)),
        ])
        .gap(6.0)
        .flex(1.0),
        "files_workspace",
        // Without a folder the hint is the placeholder name alone.
        if workspace.is_empty() { project } else { workspace },
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render()
    .flex(1.0);
    let actions = vec![IconButton::new("files_search", Icon::Search, palette)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Buscar archivos del proyecto").placement(TooltipPlacement::Below),
        )
        .render()];
    let header = openide_ui::PanelHeader::new("", palette)
        .leading(workspace_title)
        .trailing(if compact {
            vec![Element::row(actions)]
        } else {
            actions
        })
        .width(inner)
        .sizing(if compact {
            crate::ui_sizing::FILES_HEADER_COMPACT
        } else {
            crate::ui_sizing::FILES_HEADER
        })
        .render();
    let mut content = vec![header];
    if state.file_rows.is_empty() {
        content.push(empty_state(&state.files_status, palette));
    } else {
        if state.files_status.starts_with("Lista parcial")
            || state.files_status.starts_with("No se pudo")
        {
            content.push(
                Text::new(&state.files_status, theme::component_palette())
                    .muted()
                    .render(),
            );
        }
        let rows = Arc::new(state.file_rows.clone());
        let guides = Arc::new(guides_for_depths(
            &rows.iter().map(|entry| entry.depth).collect::<Vec<_>>(),
        ));
        let mut parents: Vec<&str> = Vec::new();
        let paths = rows
            .iter()
            .map(|entry| {
                parents.truncate(entry.depth);
                let path = parents
                    .iter()
                    .copied()
                    .chain(std::iter::once(entry.name.as_str()))
                    .collect::<Vec<_>>()
                    .join("/");
                if entry.is_dir {
                    parents.push(&entry.name);
                }
                path
            })
            .collect::<Vec<_>>();
        drop(parents);
        let count = rows.len() + usize::from(state.file_root_can_load_more);
        content.push(
            ScrollArea::uniform_list("file_tree", count, ROW_HEIGHT, move |index| {
                if index == rows.len() {
                    return Button::new("file_more_root", "Cargar más archivos…", palette)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Small)
                        .render()
                        .size(Some(inner), Some(ROW_HEIGHT));
                }
                let entry = &rows[index];
                let chevron = if entry.is_dir {
                    Element::icon(if entry.expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    })
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(14.0), Some(14.0))
                } else {
                    Element::spacer().size(Some(14.0), Some(14.0))
                };
                let title = Element::row(vec![
                    Element::icon(if entry.is_dir {
                        openide_ui::folder_icon(&entry.name, entry.expanded)
                    } else {
                        openide_ui::file_icon(&entry.name)
                    })
                    .size(Some(16.0), Some(ROW_HEIGHT)),
                    Element::faded_text(&entry.name, 12.0)
                        .font_family(FontFamily::Sans)
                        .font_size(13.0)
                        .foreground(theme::FOREGROUND)
                        .flex(1.0)
                        .size(None, Some(ROW_HEIGHT)),
                ])
                .gap(6.0);
                let action = format!("file_row:{index}");
                let mut row = TreeRow::new(
                    |content| Element::clickable(&action, content),
                    chevron,
                    title,
                    guides[index].clone(),
                    palette,
                )
                .width(inner)
                .height(ROW_HEIGHT)
                .guide_color(theme::TREE_GUIDE);
                if entry.is_dir && entry.expanded && entry.can_load_more {
                    let more = format!("file_more:{index}");
                    row = row.trailing(
                        Tooltip::new(
                            Button::new(&more, "Más", palette)
                                .variant(ButtonVariant::Ghost)
                                .button_size(ButtonSize::Mini)
                                .width(36.0)
                                .render(),
                            &more,
                            "Cargar más archivos",
                            theme::component_palette(),
                        )
                        .description(&paths[index])
                        .placement(if compact {
                            TooltipPlacement::Right
                        } else {
                            TooltipPlacement::Left
                        })
                        .render(),
                    );
                }
                Tooltip::for_path(
                    row.render(),
                    &action,
                    &paths[index],
                    theme::component_palette(),
                )
                .placement(if compact {
                    TooltipPlacement::Right
                } else {
                    TooltipPlacement::Left
                })
                .render()
                .size(Some(inner), Some(ROW_HEIGHT))
            })
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .top_fade(surface, 12.0)
            .flex(1.0),
        );
    }
    let content = Element::column(content)
        .gap(crate::sidebar_chrome::GAP)
        .flex(1.0);
    content.padding(inset).background(surface)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::view::FileRowView;
    use openide_ui::{DrawCommand, Frame, Point, Rect, Size, UiAction, UiEvent, UiTree};

    fn bounds(frame: &Frame, id: &str) -> Rect {
        frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .rect
    }
    fn center(rect: Rect) -> Point {
        Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }
    fn state() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.workspace = "/workspace/openADE".into();
        state.file_rows = vec![
            FileRowView {
                name: "src".into(),
                depth: 0,
                is_dir: true,
                expanded: true,
                can_load_more: true,
            },
            FileRowView {
                name: "commands.rs".into(),
                depth: 1,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            },
            FileRowView {
                name: "Cargo.toml".into(),
                depth: 0,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            },
        ];
        state
    }

    #[test]
    fn file_tooltips_show_the_name_once_and_only_add_the_parent_in_both_shared_trees() {
        let mut state = state();
        state.file_rows.splice(
            2..2,
            [
                FileRowView {
                    name: "tools".into(),
                    depth: 1,
                    is_dir: true,
                    expanded: true,
                    can_load_more: false,
                },
                FileRowView {
                    name: "commands.rs".into(),
                    depth: 2,
                    is_dir: false,
                    expanded: false,
                    can_load_more: false,
                },
            ],
        );
        let size = Size::new(320.0, 400.0);
        for compact in [false, true] {
            for (id, expected) in [
                ("files_workspace", vec!["openADE", "/workspace"]),
                ("file_row:0", vec!["src"]),
                ("file_row:1", vec!["commands.rs", "src"]),
                ("file_row:3", vec!["commands.rs", "src/tools"]),
                ("file_row:4", vec!["Cargo.toml"]),
            ] {
                let root = if compact {
                    build_ide(&state, size.width)
                } else {
                    build(&state, size.width)
                };
                let mut tree = UiTree::new(root);
                tree.set_reduced_motion(true);
                let frame = tree.layout(size);
                tree.handle_event(UiEvent::PointerMove(center(bounds(&frame, id))));
                let hovered = tree.layout_at(
                    size,
                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                );
                let overlay = hovered
                    .commands
                    .iter()
                    .rposition(|command| matches!(command, DrawCommand::Shadow { .. }))
                    .unwrap_or_else(|| panic!("missing tooltip for {id}, compact={compact}"));
                let text = hovered.commands[overlay..]
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(text, expected, "{id}, compact={compact}");
            }
        }
    }

    #[test]
    fn project_tools_have_real_actions_and_centered_icons_without_squeezing_the_tree() {
        for width in [240.0, 320.0, 600.0] {
            let state = state();
            let size = Size::new(width, 400.0);
            let mut tree = UiTree::new(build(&state, width));
            let frame = tree.layout(size);
            assert!(
                !frame.regions.iter().any(|region| region.id == "files_refresh"),
                "the tree follows the folder by itself"
            );
            for (id, icon) in [("files_search", Icon::Search)] {
                let bounds = bounds(&frame, id);
                assert_eq!((bounds.width, bounds.height), (TOOL_SIZE, TOOL_SIZE));
                assert_eq!(frame.hit_test(center(bounds)), Some(id));
                let glyph = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Icon {
                            icon: actual, rect, ..
                        } if *actual == icon && bounds.contains(center(*rect)) => Some(*rect),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!((glyph.width, glyph.height), (16.0, 16.0));
                assert!((center(glyph).x - center(bounds).x).abs() < 0.01);
                assert!((center(glyph).y - center(bounds).y).abs() < 0.01);
                tree.handle_event(UiEvent::PointerDown(center(bounds)));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(center(bounds))),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            assert!(bounds(&frame, "file_row:0").y > bounds(&frame, "files_search").y + TOOL_SIZE);
        }
    }

    #[test]
    fn folder_disclosure_precedes_file_artwork_and_load_more_does_not_toggle_the_row() {
        let state = state();
        let size = Size::new(420.0, 400.0);
        let mut tree = UiTree::new(build(&state, size.width));
        let frame = tree.layout(size);
        let row = bounds(&frame, "file_row:0");
        let glyph = |icon| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon {
                        icon: actual, rect, ..
                    } if *actual == icon && row.contains(center(*rect)) => Some(*rect),
                    _ => None,
                })
                .unwrap()
        };
        assert!(glyph(Icon::ChevronDown).x < glyph(openide_ui::folder_icon("src", true)).x);
        let more = bounds(&frame, "file_more:0");
        tree.handle_event(UiEvent::PointerDown(center(more)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center(more))),
            vec![UiAction::Clicked {
                id: "file_more:0".into()
            }]
        );
        let disclosure = center(glyph(Icon::ChevronDown));
        tree.handle_event(UiEvent::PointerDown(disclosure));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(disclosure)),
            vec![UiAction::Clicked {
                id: "file_row:0".into()
            }]
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Icon { icon, .. } if *icon == openide_ui::file_icon("commands.rs")
        )));
    }

    #[test]
    fn thousands_of_files_remain_virtual_and_the_last_row_can_be_opened() {
        let mut state = state();
        state.file_rows = (0..10_000)
            .map(|index| FileRowView {
                name: format!("file-{index}.rs"),
                depth: 0,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            })
            .collect();
        let size = Size::new(360.0, 400.0);
        let mut tree = UiTree::new(build(&state, size.width));
        tree.set_reduced_motion(true);
        let first = tree.layout(size);
        assert!(
            first
                .regions
                .iter()
                .filter(|region| region.id.starts_with("file_row:"))
                .count()
                < 60
        );
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(160.0, 220.0),
            delta_y: -1_000_000.0,
        });
        let last = tree.layout(size);
        let row = bounds(&last, "file_row:9999");
        assert_eq!(last.hit_test(center(row)), Some("file_row:9999"));
        tree.handle_event(UiEvent::PointerDown(center(row)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center(row))),
            vec![UiAction::Clicked {
                id: "file_row:9999".into()
            }]
        );
    }
}
