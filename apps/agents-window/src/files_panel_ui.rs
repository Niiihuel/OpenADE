//! Project files inside the right dock: compact tools and a virtual tree.
use crate::{
    theme,
    view::{FileDraftKind, FileDraftView, ViewState},
};
use openide_ui::{
    guides_for_depths, Button, ButtonSize, ButtonVariant, Color, Component, ContextMenu,
    ContextMenuEntry, ContextMenuItem, ControlSize, Element, FontFamily, Icon, IconButton, Notice,
    ScrollArea, Text, TextField, Tooltip, TooltipPlacement, TooltipSpec, TreeGuides, TreeRow,
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
/// On a file these create beside it; on a folder or the root, inside it.
const NEW: &[(&str, &str)] = &[
    ("new_file", "Nuevo archivo…"),
    ("new_folder", "Nueva carpeta…"),
];
const EDIT: &[(&str, &str)] = &[("rename", "Renombrar…"), ("delete", "Eliminar")];

fn row_menu(child: Element, path: &str, entry: &crate::view::FileRowView) -> Element {
    if entry.is_dir {
        let toggle = if entry.expanded { "Contraer" } else { "Expandir" };
        return tree_menu(
            child,
            path,
            &[
                &[("toggle", toggle)],
                NEW,
                MENTION,
                PATHS,
                &[("collapse_all", "Contraer todo")],
                EDIT,
            ],
        );
    }
    let markdown = openide_preview::kind_for_path(std::path::Path::new(path))
        == Some(openide_preview::PreviewKind::Markdown);
    let mut open = vec![("open", "Abrir")];
    if markdown {
        open.push(("preview", "Abrir vista previa al costado"));
    }
    tree_menu(child, path, &[&open, NEW, MENTION, PATHS, EDIT])
}

/// The header and the empty part of the tree act on the workspace root.
fn root_menu(child: Element) -> Element {
    tree_menu(
        child,
        "",
        &[
            NEW,
            &PATHS[..1],
            &[("collapse_all", "Contraer todo"), ("refresh", "Actualizar")],
        ],
    )
}

/// What a position of the list shows: a row of the tree, the field of the
/// name being typed, or what was wrong with that name.
#[derive(Clone, Copy)]
enum Shown {
    Entry(usize),
    Draft { depth: usize, is_dir: bool },
    Error { depth: usize },
}

/// The list with the draft's rows in place: a rename takes its entry's place,
/// a new entry goes first in its folder. `paths` are the rows' relative paths.
fn shown_rows(state: &ViewState, paths: &[String]) -> Vec<Shown> {
    let mut shown: Vec<Shown> = (0..paths.len()).map(Shown::Entry).collect();
    let Some(draft) = &state.file_draft else {
        return shown;
    };
    let rows = &state.file_rows;
    let at = paths.iter().position(|path| *path == draft.target);
    let is_dir = draft.kind == FileDraftKind::NewFolder;
    let (index, replaced, depth, is_dir) = match (draft.kind, at) {
        (FileDraftKind::Rename, Some(index)) => (index, 1, rows[index].depth, rows[index].is_dir),
        (FileDraftKind::Rename, None) => return shown,
        (_, Some(index)) if rows[index].is_dir => (index + 1, 0, rows[index].depth + 1, is_dir),
        (_, None) if draft.target.is_empty() => (0, 0, 0, is_dir),
        _ => return shown,
    };
    let mut field = vec![Shown::Draft { depth, is_dir }];
    if draft.error.is_some() {
        field.push(Shown::Error { depth });
    }
    shown.splice(index..index + replaced, field);
    shown
}

/// A row shaped like its neighbours whose title is `title`, with no action.
fn plain_row(
    title: Element,
    guides: TreeGuides,
    palette: openide_ui::ComponentPalette,
    width: f32,
) -> Element {
    TreeRow::new(
        |content| content,
        Element::spacer().size(Some(14.0), Some(14.0)),
        title,
        guides,
        palette,
    )
    .width(width)
    .height(ROW_HEIGHT)
    .guide_color(theme::TREE_GUIDE)
    .render()
    .size(Some(width), Some(ROW_HEIGHT))
}

fn draft_field(draft: &FileDraftView, is_dir: bool) -> Element {
    Element::row(vec![
        Element::icon(if is_dir {
            openide_ui::folder_icon(&draft.value, false)
        } else {
            openide_ui::file_icon(&draft.value)
        })
        .size(Some(16.0), Some(ROW_HEIGHT)),
        TextField::new(
            "file_tree_draft",
            &draft.value,
            if is_dir {
                "Nombre de la carpeta"
            } else {
                "Nombre del archivo"
            },
            theme::component_palette(),
        )
        .size(ControlSize::Dense)
        // It opens focused, so the placeholder has to outlive the focus.
        .persistent_placeholder(true)
        .render()
        // A field is 28 high with 8 of padding by itself; the row is 24.
        .padding(4.0)
        .min_size(80.0, ROW_HEIGHT)
        .size(None, Some(ROW_HEIGHT))
        .flex(1.0),
    ])
    .gap(6.0)
}

/// Deleting asks first, naming what goes to the trash.
pub(crate) fn delete_dialog(state: &ViewState, base: Element, width: f32) -> Element {
    let Some((path, is_dir)) = &state.file_delete else {
        return base;
    };
    openide_ui::AlertDialog::new(
        base,
        true,
        "¿Enviar a la papelera?",
        if *is_dir {
            format!("La carpeta «{path}» y todo su contenido irán a la papelera del sistema.")
        } else {
            format!("«{path}» irá a la papelera del sistema.")
        },
        "file_tree_delete_confirm",
        "file_tree_delete_cancel",
        theme::component_palette(),
    )
    .width((width - 24.0).min(420.0))
    .confirm_label("Enviar a la papelera")
    .cancel_label("Cancelar")
    .destructive(theme::DESTRUCTIVE)
    .render()
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
    // A name being typed in an empty folder still needs its row.
    if state.file_rows.is_empty() && state.file_draft.is_none() {
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
        let shown = shown_rows(state, &paths);
        let guides = Arc::new(guides_for_depths(
            &shown
                .iter()
                .map(|shown| match *shown {
                    Shown::Entry(index) => rows[index].depth,
                    Shown::Draft { depth, .. } | Shown::Error { depth } => depth,
                })
                .collect::<Vec<_>>(),
        ));
        let draft = state.file_draft.clone();
        let count = shown.len() + usize::from(state.file_root_can_load_more);
        content.push(
            ScrollArea::uniform_list("file_tree", count, ROW_HEIGHT, move |position| {
                if position == shown.len() {
                    return Button::new("file_more_root", "Cargar más archivos…", palette)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Small)
                        .render()
                        .size(Some(inner), Some(ROW_HEIGHT));
                }
                let guide = guides[position].clone();
                let index = match (shown[position], &draft) {
                    (Shown::Entry(index), _) => index,
                    (Shown::Draft { is_dir, .. }, Some(draft)) => {
                        return plain_row(draft_field(draft, is_dir), guide, palette, inner);
                    }
                    (_, draft) => {
                        let error = draft.as_ref().and_then(|draft| draft.error.as_deref());
                        let text = Element::faded_text(error.unwrap_or_default(), 12.0)
                            .font_family(FontFamily::Sans)
                            .font_size(12.0)
                            .foreground(theme::DESTRUCTIVE)
                            .flex(1.0)
                            .size(None, Some(ROW_HEIGHT));
                        return plain_row(text, guide, palette, inner);
                    }
                };
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
                    guide,
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
            let new = ["—", "Nuevo archivo…", "Nueva carpeta…", "—"];
            let edit = ["—", "Renombrar…", "Eliminar"];
            assert_eq!(
                labels,
                [
                    &["Abrir"],
                    &new[..],
                    &["Mencionar en el chat", "—"],
                    &paths[..],
                    &edit[..]
                ]
                .concat()
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
                    &["Contraer"],
                    &new[..],
                    &["Mencionar en el chat", "—"],
                    &paths[..],
                    &["—", "Contraer todo"],
                    &edit[..]
                ]
                .concat()
            );
            assert_eq!(
                choose(&mut tree, &menu, "Mencionar en el chat"),
                clicked("mention", "src")
            );
            for (label, operation, row, path) in [
                ("Nuevo archivo…", "new_file", "file_row:1", "src/commands.rs"),
                ("Nueva carpeta…", "new_folder", "file_row:0", "src"),
                ("Renombrar…", "rename", "file_row:2", "Cargo.toml"),
                ("Eliminar", "delete", "file_row:0", "src"),
            ] {
                let (mut tree, frame) = build();
                let (menu, _) = menu_at(&mut tree, size, center(bounds(&frame, row)));
                assert_eq!(choose(&mut tree, &menu, label), clicked(operation, path));
            }

            let root = [
                "Nuevo archivo…",
                "Nueva carpeta…",
                "—",
                "Copiar ruta",
                "—",
                "Contraer todo",
                "Actualizar",
            ];
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

    fn drafting(kind: FileDraftKind, target: &str, value: &str, error: Option<&str>) -> ViewState {
        let mut state = state();
        state.file_draft = Some(FileDraftView {
            target: target.into(),
            kind,
            value: value.into(),
            error: error.map(str::to_owned),
        });
        state
    }

    #[test]
    fn a_name_is_typed_in_a_row_at_its_place_and_indent_in_both_trees() {
        let size = Size::new(320.0, 400.0);
        let layout = |state: &ViewState, compact: bool| {
            let root = if compact {
                build_ide(state, size.width)
            } else {
                build(state, size.width)
            };
            UiTree::new(root).layout(size)
        };
        let has = |frame: &Frame, id: &str| frame.regions.iter().any(|region| region.id == id);
        for compact in [false, true] {
            let plain = layout(&state(), compact);
            let top = bounds(&plain, "file_row:0").y;
            let row = |frame: &Frame, id: &str| ((bounds(frame, id).y - top) / ROW_HEIGHT).round();
            // New in `src`: its first child, indented like `commands.rs`.
            let new = drafting(FileDraftKind::NewFile, "src", "", None);
            let frame = layout(&new, compact);
            assert_eq!(row(&frame, "file_tree_draft"), 1.0);
            assert_eq!(row(&frame, "file_row:1"), 2.0);
            assert_eq!(row(&frame, "file_row:2"), 3.0);
            let field = bounds(&frame, "file_tree_draft");
            assert!(field.x > bounds(&plain, "file_row:0").x + 30.0, "{field:?}");
            assert!(field.x + field.width <= size.width && field.height <= ROW_HEIGHT);
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Nombre del archivo")));
            // New at the root: the first row. A folder shows a folder's icon.
            let frame = layout(&drafting(FileDraftKind::NewFolder, "", "docs", None), compact);
            assert_eq!(row(&frame, "file_tree_draft"), 0.0);
            assert_eq!(row(&frame, "file_row:0"), 1.0);
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Icon { icon, .. } if *icon == openide_ui::folder_icon("docs", false))));
            // Rename: the field takes the entry's own row.
            let rename = drafting(FileDraftKind::Rename, "Cargo.toml", "Cargo.toml", None);
            let frame = layout(&rename, compact);
            assert_eq!(row(&frame, "file_tree_draft"), 2.0);
            assert!(!has(&frame, "file_row:2") && has(&frame, "file_row:1"));
            // The error is the row below the field, and pushes the rest down.
            let error = "Ya existe un elemento con ese nombre";
            let frame = layout(
                &drafting(FileDraftKind::NewFile, "src", "commands.rs", Some(error)),
                compact,
            );
            assert_eq!(row(&frame, "file_row:1"), 3.0);
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, color, .. }
                    if text == error && *color == theme::DESTRUCTIVE)));
            // A target that is not shown gets no stray field.
            let frame = layout(&drafting(FileDraftKind::NewFile, "gone", "", None), compact);
            assert!(!has(&frame, "file_tree_draft"));
            // An empty project still shows the row of the name being typed.
            let mut empty = drafting(FileDraftKind::NewFile, "", "", None);
            empty.file_rows.clear();
            assert!(has(&layout(&empty, compact), "file_tree_draft"));
        }
    }

    #[test]
    fn the_delete_dialog_names_the_path_and_a_folders_contents() {
        let size = Size::new(480.0, 400.0);
        for (is_dir, expected) in [
            (true, "La carpeta «src/deep» y todo su contenido irán a la papelera del sistema."),
            (false, "«src/deep» irá a la papelera del sistema."),
        ] {
            let mut state = state();
            state.file_delete = Some(("src/deep".into(), is_dir));
            let mut tree = UiTree::new(delete_dialog(&state, build(&state, size.width), size.width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(size);
            let text = frame
                .text_layouts()
                .into_iter()
                .map(|block| block.source)
                .collect::<Vec<_>>();
            assert!(text.iter().any(|text| text == expected), "{text:?}");
            for id in ["file_tree_delete_confirm", "file_tree_delete_cancel"] {
                assert!(tree.focus(id), "{id}");
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: openide_ui::Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
        }
        let state = state();
        let frame = UiTree::new(delete_dialog(&state, build(&state, 480.0), 480.0)).layout(size);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "file_tree_delete_confirm"));
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
        let review = || {
            let mut state = state();
            state.file_rows.push(FileRowView {
                name: "README.md".into(),
                depth: 0,
                is_dir: false,
                expanded: false,
                can_load_more: false,
            });
            state
        };
        let state = review();
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
            let rendered = |name: &str, root: Element| {
                let (ctx, mut tree) = native_tree(root, size);
                tree.layout(size);
                tree.focus("file_tree_draft");
                export(&tree.layout(size), size, &format!("{name}-{surface}"));
                ctx.end_pass().textures_delta.clear();
            };
            let with_draft = |kind, target: &str, value: &str, error: Option<&str>| {
                let mut state = review();
                state.file_draft = Some(FileDraftView {
                    target: target.into(),
                    kind,
                    value: value.into(),
                    error: error.map(str::to_owned),
                });
                if surface == "ide" {
                    build_ide(&state, size.width)
                } else {
                    super::build(&state, size.width)
                }
            };
            rendered("new-file", with_draft(FileDraftKind::NewFile, "src", "", None));
            rendered(
                "new-file-error",
                with_draft(
                    FileDraftKind::NewFolder,
                    "src",
                    "commands.rs",
                    Some("Ya existe un elemento con ese nombre"),
                ),
            );
            rendered(
                "rename",
                with_draft(FileDraftKind::Rename, "Cargo.toml", "Cargo.toml", None),
            );
            let mut deleting = review();
            deleting.file_delete = Some(("src".into(), true));
            rendered("delete", delete_dialog(&deleting, build(), size.width));
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
