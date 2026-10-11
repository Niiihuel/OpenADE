//! Project files inside the right dock: compact tools and a virtual tree.
use crate::{theme, view::ViewState};
use openide_ui::{
    guides_for_depths, Button, ButtonSize, ButtonVariant, Color, Component, ContextMenu,
    ContextMenuEntry, ContextMenuItem, ControlSize, Element, FontFamily, Icon, IconButton, Notice,
    ScrollArea, Text, Tooltip, TooltipPlacement, TooltipSpec, TreeRow,
};
use std::sync::Arc;

const ROW_HEIGHT: f32 = 24.0;
const TOOL_SIZE: f32 = crate::dock_tab_ui::TOOL_SIZE;
const INSET: f32 = 8.0;

/// A menu action names its target by workspace-relative path, never by row:
/// the watcher reorders rows while a menu is open. The root is the empty path.
pub(crate) fn tree_action(operation: &str, path: &str) -> String {
    format!("file_tree:{operation}:{path}")
}

/// The menu of `path`: groups of (operation, label) with a rule between them.
fn tree_menu(child: Element, path: &str, groups: &[&[(&str, &str)]]) -> Element {
    let mut entries = Vec::new();
    for group in groups.iter().filter(|group| !group.is_empty()) {
        if !entries.is_empty() {
            entries.push(ContextMenuEntry::Separator);
        }
        entries.extend(group.iter().map(|(operation, label)| {
            ContextMenuEntry::Item(ContextMenuItem::new(tree_action(operation, path), *label))
        }));
    }
    ContextMenu::new(
        format!("file_tree_menu:{path}"),
        child,
        entries,
        theme::menu_palette(),
    )
    .render()
}

const PATHS: &[(&str, &str)] = &[
    ("copy_path", "Copiar ruta"),
    ("copy_relative", "Copiar ruta relativa"),
];
const MENTION: &[(&str, &str)] = &[("mention", "Mencionar en el chat")];

fn row_menu(child: Element, path: &str, entry: &crate::view::FileRowView) -> Element {
    if entry.is_dir {
        let toggle = if entry.expanded { "Contraer" } else { "Expandir" };
        return tree_menu(
            child,
            path,
            &[
                &[("toggle", toggle)],
                MENTION,
                PATHS,
                &[("collapse_all", "Contraer todo")],
            ],
        );
    }
    let markdown = openide_preview::kind_for_path(std::path::Path::new(path))
        == Some(openide_preview::PreviewKind::Markdown);
    let mut open = vec![("open", "Abrir")];
    if markdown {
        open.push(("preview", "Abrir vista previa al costado"));
    }
    tree_menu(child, path, &[&open, MENTION, PATHS])
}

/// The header and the empty part of the tree act on the workspace root.
fn root_menu(child: Element) -> Element {
    tree_menu(
        child,
        "",
        &[
            &PATHS[..1],
            &[("collapse_all", "Contraer todo"), ("refresh", "Actualizar")],
        ],
    )
}

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
                let row = Tooltip::for_path(
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
                .size(Some(inner), Some(ROW_HEIGHT));
                row_menu(row, &paths[index], entry)
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
    root_menu(content.padding(inset).background(surface))
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

    /// Right-clicks `point` and returns the open menu's labels, rules as "—".
    fn menu_at(tree: &mut UiTree, size: Size, point: Point) -> (Frame, Vec<String>) {
        tree.handle_event(UiEvent::ContextMenu { position: point });
        let frame = tree.layout(size);
        let surface = bounds(&frame, "__context_menu:surface");
        // The menu is painted last, after its own shadow.
        let overlay = frame
            .commands
            .iter()
            .rposition(|command| matches!(command, DrawCommand::Shadow { .. }))
            .unwrap();
        let mut rows: Vec<(f32, String)> = frame.commands[overlay..]
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { position, text, .. } if surface.contains(*position) => {
                    Some((position.y, text.clone()))
                }
                DrawCommand::FillRect { rect, .. }
                    if rect.height == 1.0 && surface.contains(center(*rect)) =>
                {
                    Some((rect.y, "—".into()))
                }
                _ => None,
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        (frame, rows.into_iter().map(|(_, label)| label).collect())
    }

    /// Chooses the menu item labelled `label` and returns what it dispatched.
    fn choose(tree: &mut UiTree, frame: &Frame, label: &str) -> Vec<UiAction> {
        let text = frame
            .commands
            .iter()
            .rev()
            .find_map(|command| match command {
                DrawCommand::Text { position, text, .. } if text == label => Some(*position),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing menu item {label}"));
        let point = Point::new(text.x + 2.0, text.y + 4.0);
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }

    fn clicked(operation: &str, path: &str) -> Vec<UiAction> {
        vec![UiAction::Clicked {
            id: tree_action(operation, path),
        }]
    }

    #[test]
    fn right_click_opens_the_menu_of_the_row_the_header_or_the_empty_area() {
        let mut state = state();
        state.file_rows.push(FileRowView {
            name: "README.md".into(),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        });
        let size = Size::new(320.0, 400.0);
        for compact in [false, true] {
            let build = || {
                let root = if compact {
                    build_ide(&state, size.width)
                } else {
                    build(&state, size.width)
                };
                let mut tree = UiTree::new(root);
                tree.set_reduced_motion(true);
                let frame = tree.layout(size);
                (tree, frame)
            };
            let paths = ["Copiar ruta", "Copiar ruta relativa"];
            let (mut tree, frame) = build();
            let (menu, labels) = menu_at(&mut tree, size, center(bounds(&frame, "file_row:1")));
            assert_eq!(
                labels,
                [&["Abrir", "—", "Mencionar en el chat", "—"], &paths[..]].concat()
            );
            // The action carries the path, never the row's position.
            assert_eq!(
                choose(&mut tree, &menu, "Abrir"),
                clicked("open", "src/commands.rs")
            );

            let (mut tree, frame) = build();
            let (menu, labels) = menu_at(&mut tree, size, center(bounds(&frame, "file_row:3")));
            assert_eq!(labels[..2], ["Abrir", "Abrir vista previa al costado"]);
            assert_eq!(
                choose(&mut tree, &menu, "Abrir vista previa al costado"),
                clicked("preview", "README.md")
            );

            let (mut tree, frame) = build();
            let (menu, labels) = menu_at(&mut tree, size, center(bounds(&frame, "file_row:0")));
            assert_eq!(
                labels,
                [
                    &["Contraer", "—", "Mencionar en el chat", "—"],
                    &paths[..],
                    &["—", "Contraer todo"]
                ]
                .concat()
            );
            assert_eq!(
                choose(&mut tree, &menu, "Mencionar en el chat"),
                clicked("mention", "src")
            );

            let root = ["Copiar ruta", "—", "Contraer todo", "Actualizar"];
            let tree_area = bounds(&frame, "file_tree");
            for point in [
                center(bounds(&frame, "files_workspace")),
                // Below the last of the four rows the tree is empty.
                Point::new(center(tree_area).x, tree_area.y + tree_area.height - 8.0),
            ] {
                let (mut tree, _) = build();
                let (menu, labels) = menu_at(&mut tree, size, point);
                assert_eq!(labels, root, "compact={compact} at {point:?}");
                assert_eq!(
                    choose(&mut tree, &menu, "Actualizar"),
                    clicked("refresh", "")
                );
            }
        }
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

    /// A tree measured with the application's fonts, for the exported review.
    fn native_tree(root: Element, size: Size) -> (eframe::egui::Context, UiTree) {
        use eframe::egui;
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        let measure = ctx.clone();
        let mut tree = UiTree::new(
            Element::panel(root)
                .background(theme::BACKGROUND)
                .size(Some(size.width), Some(size.height)),
        );
        tree.set_reduced_motion(true);
        tree.set_text_measurer(move |text, size, family| {
            let family = match family {
                FontFamily::Sans => egui::FontFamily::Proportional,
                FontFamily::Monospace => egui::FontFamily::Monospace,
            };
            measure.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.to_owned(), egui::FontId::new(size, family), egui::Color32::WHITE)
                    .size()
                    .x
            })
        });
        (ctx, tree)
    }

    fn export(frame: &Frame, size: Size, name: &str) {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/ui-separation/shots/file-tree");
        std::fs::create_dir_all(&directory).unwrap();
        let svg = std::env::temp_dir().join(format!("file-tree-{name}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(frame, &svg, size.width, size.height)
            .unwrap();
        let tree = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&svg).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut image =
            resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut image.as_mut(),
        );
        image.save_png(directory.join(format!("{name}.png"))).unwrap();
    }

    #[test]
    #[ignore = "opt-in PNG review of the file tree's menus with native fonts"]
    fn export_file_tree_visual_review() {
        let mut state = state();
        state.file_rows.push(FileRowView {
            name: "README.md".into(),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        });
        for (surface, size) in [
            ("dock", Size::new(360.0, 460.0)),
            ("ide", Size::new(280.0, 460.0)),
        ] {
            let build = || {
                if surface == "ide" {
                    build_ide(&state, size.width)
                } else {
                    build(&state, size.width)
                }
            };
            for (name, target) in [
                ("menu-file", "file_row:3"),
                ("menu-folder", "file_row:0"),
                ("menu-root", "files_workspace"),
            ] {
                let (ctx, mut tree) = native_tree(build(), size);
                let frame = tree.layout(size);
                let (frame, _) = menu_at(&mut tree, size, center(bounds(&frame, target)));
                export(&frame, size, &format!("{name}-{surface}"));
                ctx.end_pass().textures_delta.clear();
            }
        }
    }
}
