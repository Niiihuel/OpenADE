//! A continuous horizontally scrolling tab strip with a fixed utility toolbar.
//! Selection reveals the full tab; manual scroll never substitutes a page.
use super::*;

use crate::dock_tab_ui::{self, HEADER_SIDE_INSET, TOOL_SIZE};
#[cfg(test)]
use crate::dock_tab_ui::{CLOSE_SIZE, TAB_GAP, TAB_HEIGHT};
use openide_ui::{
    Component, ContextMenu, ContextMenuEntry, ContextMenuItem, Separator, TabItem, TabStrip,
};

#[cfg(test)]
const HEADER_PADDING: f32 = 2.0;
#[cfg(test)]
const TOOLBAR_WIDTH: f32 = TOOL_SIZE * 5.0;

/// The control that maximizes the right dock over the conversation and
/// restores it.
pub(crate) const MAXIMIZE: &str = "dock_maximize";

pub(crate) fn file_action(state: &ViewState, tab: u64, operation: &str) -> String {
    format!(
        "dock_file_action:{}",
        serde_json::to_string(&(
            &state.workspace,
            state.selected_session.as_deref().unwrap_or(""),
            tab,
            operation,
        ))
        .expect("file tab action is serializable")
    )
}

pub(super) fn visible_tabs(state: &ViewState) -> Vec<&DockTabView> {
    state
        .dock_tabs
        .iter()
        .filter(|tab| {
            !crate::ide_workspace::active(state)
                || !matches!(tab.kind, SideTab::Files | SideTab::Context)
        })
        .collect()
}

pub(super) fn icon(kind: SideTab) -> Icon {
    match kind {
        SideTab::Context => Icon::PanelRight,
        SideTab::Files => Icon::Folder,
        SideTab::Activity => Icon::History,
        SideTab::CodeReview => Icon::GitBranch,
        SideTab::SessionChanges | SideTab::FilePreview | SideTab::CodeSnapshot => Icon::Code,
        SideTab::Browser => Icon::Globe,
        SideTab::Terminal => Icon::Terminal,
    }
}

fn resource_icon(tab: &DockTabView) -> Icon {
    if matches!(tab.kind, SideTab::FilePreview | SideTab::CodeSnapshot) {
        openide_ui::file_icon(tab.full_path.as_deref().unwrap_or(&tab.title))
    } else {
        icon(tab.kind)
    }
}

/// The tab a file tab's surface or its close button belongs to.
pub(crate) fn tab_of(id: &str) -> Option<u64> {
    id.strip_prefix("dock_tab:")
        .or_else(|| id.strip_prefix("dock_close:"))?
        .parse()
        .ok()
}

fn tab(
    state: &ViewState,
    decorations: Option<&crate::git_decorations::GitDecorations>,
    tab: &DockTabView,
    width: f32,
) -> TabItem {
    // shortcut: the host marks an unsaved document in the title it gives the
    // tab lists; a flag on `DockTabView` would say it without the suffix.
    let (title, unsaved) = match tab.title.strip_suffix(" •") {
        Some(title) => (title, true),
        None => (tab.title.as_str(), false),
    };
    let path = tab.full_path.as_deref().unwrap_or(title);
    let mark = decorations
        .filter(|_| tab.kind == SideTab::FilePreview)
        .and_then(|decorations| decorations.file_at(&state.workspace, path));
    let mut item = TabItem::new(
        format!("dock_tab:{}", tab.id),
        format!("dock_tab_extent:{}", tab.id),
        resource_icon(tab),
        title,
        width,
    )
    .tooltip(match mark {
        Some(mark) => format!("{path}\n{}", mark.label),
        None => path.to_owned(),
    })
    .tooltip_placement(TooltipPlacement::Below)
    .active(tab.active);
    if let Some(mark) = mark {
        item = item.title_ink(mark.color);
    }
    if !tab.closable {
        return item;
    }
    if unsaved && state.git_panel.tab_revealed != Some(tab.id) {
        // Unsaved changes hold the close button's place until the pointer
        // or the focus is on the tab.
        return item.close(
            Element::spacer()
                .background(theme::FOREGROUND)
                .radius(UNSAVED_MARK / 2.0)
                .size(Some(UNSAVED_MARK), Some(UNSAVED_MARK)),
        );
    }
    item.close(
        UiIconButton::new(
            format!("dock_close:{}", tab.id),
            Icon::Close,
            theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Ghost)
        .sizing(crate::ui_sizing::TAB_CLOSE)
        .tooltip_with(
            TooltipSpec::new(format!("Cerrar {title}")).placement(TooltipPlacement::Below),
        )
        .render(),
    )
}

/// Diameter of the mark of unsaved changes in a tab's close slot.
const UNSAVED_MARK: f32 = 8.0;

/// A file tab in the IDE's editor group opens the file menu on right click.
fn editor_tab(state: &ViewState, tab_view: &DockTabView, surface: Element) -> Element {
    if !crate::ide_workspace::active(state) || tab_view.kind != SideTab::FilePreview {
        return surface;
    }
    let action = |operation| file_action(state, tab_view.id, operation);
    let item =
        |operation, title| ContextMenuEntry::Item(ContextMenuItem::new(action(operation), title));
    let mut entries = vec![item("save", "Guardar"), ContextMenuEntry::Separator];
    entries.push(item("copy_relative", "Copiar ruta relativa"));
    entries.push(item("copy_absolute", "Copiar ruta absoluta"));
    entries.push(item("reveal", "Mostrar en el explorador"));
    if tab_view.full_path.as_deref().is_some_and(|path| {
        openide_preview::kind_for_path(std::path::Path::new(path))
            == Some(openide_preview::PreviewKind::Markdown)
    }) {
        entries.push(item("preview", "Abrir vista previa al costado"));
    }
    entries.extend([
        ContextMenuEntry::Separator,
        ContextMenuEntry::Item(ContextMenuItem::new(action("close"), "Cerrar").shortcut("Ctrl+W")),
        item("close_others", "Cerrar las demás pestañas"),
        item("close_all", "Cerrar todas las pestañas del grupo"),
    ]);
    ContextMenu::new(
        format!("dock_file_context:{}", tab_view.id),
        surface,
        entries,
        theme::menu_palette(),
    )
    .render()
}

fn all_tabs_menu(state: &ViewState) -> Element {
    let mut entries: Vec<DropdownMenuEntry> = visible_tabs(state)
        .into_iter()
        .map(|tab| {
            DropdownMenuItem::new(format!("dock_tab_menu:{}", tab.id), &tab.title)
                .icon(resource_icon(tab))
                .annotation(if tab.active { "Actual" } else { "" })
                .into()
        })
        .collect();
    entries.push(
        DropdownMenuItem::new("dock_overview_toggle", "Administrar pestañas")
            .icon(Icon::Menu)
            .into(),
    );
    let anchor = UiIconButton::new(
        "dock_tabs_menu",
        Icon::Menu,
        theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .tooltip_with(
        TooltipSpec::new("Todas las pestañas")
            .shortcut("Ctrl+Tab")
            .placement(TooltipPlacement::Below),
    )
    .render();
    DropdownMenu::new(
        anchor,
        entries,
        state.show_dock_tabs_menu,
        "dock_tabs_menu_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::BottomEnd)
    .context_menu_style()
    .width(260.0)
    .render()
}

pub(super) fn header(state: &ViewState, width: f32) -> Element {
    let ide = crate::ide_workspace::active(state);
    let visible_tabs = visible_tabs(state);
    let mut controls = Vec::new();
    let mut editor_controls = Vec::new();
    if ide && !state.show_dock_overview && state.side_tab == SideTab::FilePreview {
        if let Some(editor) = state.code_editor.as_ref().filter(|editor| {
            visible_tabs
                .iter()
                .any(|tab| tab.active && tab.id == editor.tab_id)
        }) {
            editor_controls.extend(crate::code_editor_ui::titlebar_actions(
                editor,
                &state.editor_workbench,
                state.keymap_profile,
            ));
        }
    }
    if !ide {
        controls.push(
            UiIconButton::new(
                "terminal_toggle",
                Icon::Terminal,
                theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .active(state.show_terminal_panel)
            .tooltip_with(
                TooltipSpec::new(if state.show_terminal_panel {
                    "Ocultar terminal"
                } else {
                    "Mostrar terminal"
                })
                .shortcut(
                    state
                        .keymap_profile
                        .shortcut(crate::keymap::ShortcutAction::ToggleTerminal),
                )
                .placement(TooltipPlacement::Below),
            )
            .render(),
        );
    }
    // The resource launcher and overview belong to the Agents dock. IDE
    // exposes activities and browser navigation in its own rail and menus.
    if !ide {
        controls.push(dock_add_menu(state));
        controls.push(all_tabs_menu(state));
    }
    if !ide {
        controls.push(
            UiIconButton::new(
                MAXIMIZE,
                if state.dock_maximized {
                    Icon::Collapse
                } else {
                    Icon::Expand
                },
                theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            // A dock without tabs has nothing to show any larger.
            .enabled(!state.dock_tabs.is_empty())
            .tooltip_with(
                TooltipSpec::new(if state.dock_maximized {
                    "Restaurar panel"
                } else {
                    "Maximizar panel"
                })
                .shortcut(
                    state
                        .keymap_profile
                        .shortcut(crate::keymap::ShortcutAction::MaximizeDock),
                )
                .placement(TooltipPlacement::Below),
            )
            .render(),
        );
        controls.push(
            UiIconButton::new(
                "toggle_side_pane",
                Icon::PanelRightClose,
                theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Ocultar panel derecho").placement(TooltipPlacement::Below),
            )
            .render(),
        );
    }
    let group_separator = if editor_controls.is_empty() || controls.is_empty() {
        0.0
    } else {
        9.0
    };
    let toolbar_width =
        TOOL_SIZE * (controls.len() + editor_controls.len()) as f32 + group_separator;
    let toolbar_gap = if toolbar_width > 0.0 { 8.0 } else { 0.0 };
    let available = (width - HEADER_SIDE_INSET * 2.0 - toolbar_gap - toolbar_width).max(0.0);
    let tabs = if state.show_dock_overview {
        UiText::new("Pestañas", theme::component_palette())
            .role(TextRole::Label)
            .render()
            .size(None, Some(RIGHT_HEADER_HEIGHT))
            .flex(1.0)
    } else {
        let decorations = crate::git_decorations::for_state(state);
        let items = visible_tabs
            .iter()
            .map(|item| {
                // Fit ordinary filenames before fading; the close target
                // and fixed group actions always retain their own space.
                let max = if ide && item.kind == SideTab::FilePreview {
                    360.0
                } else {
                    dock_tab_ui::TAB_MAX_WIDTH
                };
                tab(state, decorations.as_deref(), item, dock_tab_ui::tab_width(item.title.trim_end_matches(" •"), available, max))
            })
            .collect();
        TabStrip::new("right_dock_tabs", items, theme::tab_strip_palette())
            .each_tab(|index, surface| editor_tab(state, visible_tabs[index], surface))
            .render()
    };
    if !editor_controls.is_empty() {
        if group_separator > 0.0 {
            editor_controls.push(
                Separator::vertical(theme::component_palette())
                    .length(14.0)
                    .centered_in(TOOL_SIZE)
                    .margin(4.0)
                    .render()
                    .size(Some(group_separator), Some(TOOL_SIZE)),
            );
        }
        editor_controls.append(&mut controls);
        controls = editor_controls;
    }
    let controls = Element::row(controls).size(Some(toolbar_width), Some(TOOL_SIZE));
    // Leave room for the workspace's rounded outer frame without separating
    // the active tab from the editor surface below it.
    TabStrip::header(tabs, controls, width, theme::tab_strip_palette())
        .gap(toolbar_gap)
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn tab_state() -> ViewState {
        let mut state = super::super::tests::empty_state();
        state.dock_tabs = [
            ("commands.rs", SideTab::FilePreview),
            ("Terminal", SideTab::Terminal),
            ("native_streaming_app_tests.rs", SideTab::FilePreview),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (title, kind))| DockTabView {
            full_path: None,
            id: index as u64 + 1,
            title: title.into(),
            kind,
            active: index == 0,
            closable: true,
        })
        .collect();
        state
    }

    /// A modified file, an untracked one and a clean one with unsaved edits.
    fn decorated_tabs(ide: bool) -> ViewState {
        let mut state = super::super::tests::empty_state();
        state.workspace = "/workspace/openADE".into();
        if ide {
            state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        }
        state.dock_tabs = ["src/commands.rs", "notes.md", "src/main.rs •"]
            .into_iter()
            .enumerate()
            .map(|(index, title)| DockTabView {
                full_path: Some(format!("/workspace/openADE/{}", title.trim_end_matches(" •"))),
                id: index as u64 + 1,
                title: title.rsplit('/').next().unwrap().into(),
                kind: SideTab::FilePreview,
                active: index == 0,
                closable: true,
            })
            .collect();
        state.git_review = Some(Ok(std::sync::Arc::new(crate::git_decorations::tests::review(
            &[],
            &[("src/commands.rs", "Modificado")],
            &["notes.md"],
        ))));
        state
    }

    #[test]
    fn a_file_tab_takes_its_status_ink_and_unsaved_changes_hold_the_close_slot_until_pointed() {
        assert_eq!((tab_of("dock_tab:3"), tab_of("dock_close:3")), (Some(3), Some(3)));
        assert_eq!(tab_of("dock_tab_extent:3"), None);
        for ide in [false, true] {
            let frame = |change: &dyn Fn(&mut ViewState)| {
                let mut state = decorated_tabs(ide);
                change(&mut state);
                let mut tree = UiTree::new(header(&state, 900.0));
                tree.set_reduced_motion(true);
                tree.layout(Size::new(900.0, RIGHT_HEADER_HEIGHT))
            };
            let ink = |frame: &openide_ui::Frame, title: &str| {
                frame.commands.iter().find_map(|command| match command {
                    DrawCommand::Text { text, color, .. } if text == title => Some(*color),
                    _ => None,
                })
            };
            let region = |frame: &openide_ui::Frame, id: &str| {
                frame.regions.iter().find(|hit| hit.id == id).map(|hit| hit.rect)
            };
            let marks = |frame: &openide_ui::Frame| -> Vec<openide_ui::Rect> {
                frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::FillRect { rect, color, .. }
                            if *color == theme::FOREGROUND && rect.width == UNSAVED_MARK =>
                        {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .collect()
            };
            let rest = frame(&|_| {});
            assert_eq!(ink(&rest, "commands.rs"), Some(theme::WARNING), "ide={ide}");
            assert_eq!(ink(&rest, "notes.md"), Some(theme::DIFF_ADDED), "ide={ide}");
            // Unsaved is the document's state, not the repository's: the
            // title keeps the tab's ink and loses the bullet it had.
            assert_eq!(ink(&rest, "main.rs"), Some(theme::FOREGROUND_SUBTLE), "ide={ide}");
            assert_eq!(ink(&rest, "main.rs •"), None);
            assert!(region(&rest, "dock_close:1").is_some() && region(&rest, "dock_close:2").is_some());
            assert_eq!(region(&rest, "dock_close:3"), None);
            let [dot] = marks(&rest)[..] else { panic!("one mark: {:?}", marks(&rest)) };
            let pointed = frame(&|state| state.git_panel.tab_revealed = Some(3));
            assert_eq!(marks(&pointed), Vec::new());
            let close = region(&pointed, "dock_close:3").unwrap();
            // The button appears where the mark was, and nothing else moves.
            assert_eq!(
                (dot.x + dot.width / 2.0, dot.y + dot.height / 2.0),
                (close.x + close.width / 2.0, close.y + close.height / 2.0)
            );
            for id in ["dock_tab:1", "dock_tab:2", "dock_tab:3", "dock_close:1"] {
                assert_eq!(region(&rest, id), region(&pointed, id), "{id}");
            }
            // Without a repository the titles keep the tab's own ink.
            let plain = frame(&|state| state.git_review = None);
            assert_eq!(ink(&plain, "commands.rs"), Some(theme::FOREGROUND));
            assert_eq!(ink(&plain, "notes.md"), Some(theme::FOREGROUND_SUBTLE));
            assert_eq!(marks(&plain).len(), 1);
        }
    }

    #[test]
    #[ignore = "exports decorated file tabs as PNG under docs/ui-separation/shots/git-decorations"]
    fn export_git_decorations_tabs_visual_review() {
        for (window, ide) in [("agents", false), ("ide", true)] {
            for (name, hovered) in [("idle", None), ("hover", Some(3))] {
                let mut state = decorated_tabs(ide);
                state.git_panel.tab_revealed = hovered;
                let size = Size::new(640.0, 72.0);
                let root = Element::column(vec![
                    header(&state, size.width),
                    Element::spacer().flex(1.0),
                ])
                .background(theme::BACKGROUND);
                let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
                tree.layout(size);
                // The second frame knows the strip fits and drops its edge fade.
                let mut frame = tree.layout(size);
                if hovered.is_some() {
                    let tab = frame.regions.iter().find(|hit| hit.id == "dock_tab:3").unwrap().rect;
                    tree.handle_event(UiEvent::PointerMove(Point::new(tab.x + 12.0, tab.y + 12.0)));
                    frame = tree.layout(size);
                }
                crate::view::ide_visual_tests::export_shot(
                    &frame,
                    size,
                    "git-decorations",
                    &format!("tabs-{window}-{name}"),
                );
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn ide_editor_actions_share_the_tab_row_and_preserve_the_active_close() {
        let mut state = tab_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        state.side_tab = SideTab::FilePreview;
        state.dock_tabs[0].title = "README.md".into();
        state.dock_tabs[0].full_path = Some("docs/README.md".into());
        state.code_editor = Some(crate::code_editor_ui::CodeEditorView {
            tab_id: 1,
            path: "docs/README.md".into(),
            controller: Some(openide_ui::EditorController::new(
                openide_editor::TextBuffer::new("# Proyecto\n").unwrap(),
            )),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            conflict: false,
            recovered: false,
            missing: false,
        });
        for width in [240.0, 272.0, 320.0, 520.0, 900.0] {
            let size = Size::new(width, RIGHT_HEADER_HEIGHT);
            let mut tree = UiTree::new(header(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(size);
            let mut previous_right = 0.0;
            for id in [
                "dock_close:1",
                "editor_find",
                "markdown_preview_open:1",
                "editor_menu",
            ] {
                let matches = frame
                    .regions
                    .iter()
                    .filter(|region| region.id == id)
                    .collect::<Vec<_>>();
                assert_eq!(matches.len(), 1, "{id} must have one titlebar action");
                let rect = matches[0].rect;
                let side = if id == "dock_close:1" { CLOSE_SIZE } else { TOOL_SIZE };
                assert_eq!((rect.width, rect.height), (side, side));
                assert_eq!(rect.y + rect.height / 2.0, RIGHT_HEADER_HEIGHT / 2.0);
                assert!(
                    rect.x >= previous_right && rect.x + rect.width <= width - HEADER_SIDE_INSET
                );
                previous_right = rect.x + rect.width;
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                assert_eq!(frame.hit_test(point), Some(id));
                tree.handle_event(UiEvent::PointerDown(point));
                // A normal frame rebuild cannot invalidate a held preview action.
                tree.set_root(header(&state, width));
                tree.layout(size);
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            assert!(!frame.regions.iter().any(|region| matches!(
                region.id.as_str(),
                "dock_add_tab" | "dock_tabs_menu" | "terminal_toggle" | "toggle_side_pane"
            )));
        }
        state.code_editor.as_mut().unwrap().path = "src/main.rs".into();
        let frame =
            UiTree::new(header(&state, 320.0)).layout(Size::new(320.0, RIGHT_HEADER_HEIGHT));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("markdown_preview_open:")));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "editor_find"));
        state.dock_tabs[0].active = false;
        state.dock_tabs[1].active = true;
        let frame =
            UiTree::new(header(&state, 320.0)).layout(Size::new(320.0, RIGHT_HEADER_HEIGHT));
        assert!(!frame
            .regions
            .iter()
            .any(|region| matches!(region.id.as_str(), "editor_find" | "editor_menu")));
    }

    #[test]
    fn agents_resource_menus_remain_available_only_in_the_agents_dock() {
        for ide in [false, true] {
            for menu in ["add", "tabs"] {
                let mut state = tab_state();
                if ide {
                    state.ide.mode = crate::ide_workspace::WindowMode::Ide;
                }
                state.show_dock_add_menu = menu == "add";
                state.show_dock_tabs_menu = menu == "tabs";
                let frame = UiTree::new(header(&state, 520.0)).layout(Size::new(520.0, 400.0));
                for id in [
                    "dock_add_tab",
                    "dock_tabs_menu",
                    "terminal_toggle",
                    "toggle_side_pane",
                ] {
                    assert_eq!(
                        frame.regions.iter().any(|region| region.id == id),
                        !ide,
                        "{id} with {menu} menu, IDE={ide}"
                    );
                }
                let menu_item = if menu == "add" {
                    "dock_open:browser"
                } else {
                    "dock_tab_menu:1"
                };
                assert_eq!(
                    frame.regions.iter().any(|region| region.id == menu_item),
                    !ide
                );
                assert!(frame.regions.iter().any(|region| region.id == "dock_tab:1"));
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id == "dock_close:1"));
            }
        }
    }

    fn editor_header_review_state(mode: &str) -> ViewState {
        let mut state = crate::view::ide_visual_tests::fixture();
        state.show_terminal_panel = false;
        state.ide.bottom_open = false;
        if mode != "document" {
            state.code_editor = None;
            state.dock_tabs.clear();
            state.side_tab = SideTab::Files;
        }
        if mode == "browser" {
            state.side_tab = SideTab::Browser;
            state.dock_tabs.push(DockTabView {
                id: 73,
                title: "Navegador".into(),
                full_path: None,
                active: true,
                kind: SideTab::Browser,
                closable: true,
            });
        }
        state
    }

    #[test]
    fn empty_ide_group_uses_the_full_height_and_keeps_any_visible_tabs() {
        for (width, height) in [(240.0, 260.0), (460.0, 700.0), (900.0, 420.0)] {
            let size = Size::new(width, height);
            let mut state = editor_header_review_state("empty");
            let expected = UiTree::new(crate::ide_editor_empty::build(
                state.keymap_profile,
                width,
                height,
            ))
            .layout(size);
            for hidden_tabs in [false, true] {
                state.dock_tabs = if hidden_tabs {
                    [SideTab::Files, SideTab::Context]
                        .into_iter()
                        .enumerate()
                        .map(|(index, kind)| DockTabView {
                            id: index as u64 + 1,
                            title: "Vista de Agentes".into(),
                            full_path: None,
                            kind,
                            active: index == 0,
                            closable: true,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let frame =
                    UiTree::new(side_pane_primary_sized(&state, width, height)).layout(size);
                assert!(
                    !frame
                        .regions
                        .iter()
                        .any(|region| region.id.starts_with("dock_tab")
                            || region.id == "dock_add_tab")
                );
                for id in ["ide_empty_file", "ide_empty_commands", "ide_empty_terminal"] {
                    let actual = frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect;
                    let expected = expected
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect;
                    assert_eq!(actual, expected, "{id} must use the full group height");
                }
            }
            // A selected Files surface can coexist with open editor tabs. Its
            // empty body must not erase those tabs or their close targets.
            state.dock_tabs.push(DockTabView {
                id: 71,
                title: "workspace.rs".into(),
                full_path: Some("src/workspace.rs".into()),
                active: false,
                kind: SideTab::FilePreview,
                closable: true,
            });
            let frame = UiTree::new(side_pane_primary_sized(&state, width, height)).layout(size);
            for id in ["dock_tab:71", "dock_close:71"] {
                assert!(
                    frame.regions.iter().any(|region| region.id == id),
                    "{id} remains reachable"
                );
            }
        }
    }

    #[test]
    fn ide_browser_keeps_its_tab_and_navigation_without_agents_launchers() {
        let state = editor_header_review_state("browser");
        for width in [360.0, 900.0] {
            let frame = UiTree::new(side_pane_primary_sized(&state, width, 420.0))
                .layout(Size::new(width, 420.0));
            for id in ["dock_tab:73", "dock_close:73", "browser_url"] {
                assert!(
                    frame.regions.iter().any(|region| region.id == id),
                    "missing {id}"
                );
            }
            assert!(!frame.regions.iter().any(|region| matches!(
                region.id.as_str(),
                "dock_add_tab" | "dock_tabs_menu" | "editor_find" | "editor_menu"
            )));
            let address = frame
                .regions
                .iter()
                .find(|region| region.id == "browser_url")
                .unwrap()
                .rect;
            assert!(address.y >= RIGHT_HEADER_HEIGHT);
        }
    }

    #[test]
    #[ignore = "native empty, document and browser IDE groups without Agents dock launchers"]
    fn export_ide_editor_header_visual_review() {
        let directory = std::env::var_os("OPENIDE_EDITOR_HEADER_VISUAL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/tmp/openide-editor-header-review".into());
        std::fs::create_dir_all(&directory).unwrap();
        for width in [360.0, 900.0] {
            for mode in ["empty", "document", "browser"] {
                let state = editor_header_review_state(mode);
                let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                    side_pane_primary_sized(&state, width, 420.0).background(theme::BACKGROUND),
                );
                let frame = tree.layout(Size::new(width, 420.0));
                let path = directory.join(format!("editor-header-{mode}-{width:.0}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 420.0)
                    .unwrap();
                let parsed = resvg::usvg::Tree::from_str(
                    &std::fs::read_to_string(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixels = resvg::tiny_skia::Pixmap::new(width as u32, 420).unwrap();
                resvg::render(
                    &parsed,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixels.as_mut(),
                );
                pixels.save_png(path.with_extension("png")).unwrap();
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn compact_tabs_have_even_gaps_and_one_keyboard_focus_box() {
        let state = tab_state();
        let mut tree = UiTree::new(header(&state, 900.0));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(900.0, RIGHT_HEADER_HEIGHT));
        let region = |id: &str| frame.regions.iter().find(|hit| hit.id == id).unwrap().rect;
        for (left, right) in [(1, 2), (2, 3)] {
            let left = region(&format!("dock_tab_extent:{left}"));
            let right = region(&format!("dock_tab_extent:{right}"));
            assert!((left.x + left.width + TAB_GAP - right.x).abs() < 0.01);
        }
        assert!(region("dock_tab_extent:3").width > region("dock_tab_extent:2").width);
        let active = region("dock_tab_extent:1");
        assert_eq!(active.y + active.height, RIGHT_HEADER_HEIGHT);
        let center = RIGHT_HEADER_HEIGHT / 2.0;
        for id in [
            "dock_close:1",
            "terminal_toggle",
            "dock_add_tab",
            "dock_tabs_menu",
            "toggle_side_pane",
        ] {
            let rect = region(id);
            let target_size = if id.starts_with("dock_close:") {
                CLOSE_SIZE
            } else {
                TOOL_SIZE
            };
            assert_eq!((rect.width, rect.height), (target_size, target_size));
            assert!(
                (rect.y + rect.height / 2.0 - center).abs() < 0.01,
                "{id}: {rect:?}"
            );
        }
        assert!(tree.focus("dock_tab:1"));
        let focused = tree.layout(Size::new(900.0, RIGHT_HEADER_HEIGHT));
        let outlines: Vec<_> = focused
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::StrokeCornerRect {
                    rect, color, radii, ..
                } if *color == Color::rgb(170, 170, 170) => Some((*rect, *radii)),
                _ => None,
            })
            .collect();
        assert_eq!(
            outlines,
            vec![(region("dock_tab:1"), openide_ui::CornerRadii::top(8.0))]
        );
        let event = |key| UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        };
        for (key, target) in [
            (Key::ArrowRight, "dock_tab:2"),
            (Key::End, "dock_tab:3"),
            (Key::Home, "dock_tab:1"),
        ] {
            assert_eq!(
                tree.handle_event(event(key)),
                vec![UiAction::Clicked { id: target.into() }]
            );
            assert_eq!(tree.focused_id(), Some(target));
        }
    }

    #[test]
    fn common_filenames_fit_without_fading_with_the_native_ui_font() {
        use eframe::egui;
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        for width in [320.0, 520.0, 900.0] {
            for title in [
                "commands.rs",
                "Cargo.toml",
                "terminal.rs",
                "main.rs",
                "README.md",
            ] {
                let mut state = tab_state();
                state.dock_tabs[0].title = title.into();
                let mut tree = UiTree::new(header(&state, width));
                let measurer = ctx.clone();
                tree.set_text_measurer(move |text, size, family| {
                    measurer.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                text.to_owned(),
                                egui::FontId::new(
                                    size,
                                    match family {
                                        FontFamily::Monospace => egui::FontFamily::Monospace,
                                        FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                                    },
                                ),
                                egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    })
                });
                let frame = tree.layout(Size::new(width, RIGHT_HEADER_HEIGHT));
                let shaped_width = ctx.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            title.into(),
                            egui::FontId::proportional(13.0),
                            egui::Color32::WHITE,
                        )
                        .size()
                        .x
                });
                let mut clips = Vec::new();
                let mut title_commands = 0;
                for command in &frame.commands {
                    match command {
                        DrawCommand::PushClip(rect) => clips.push(*rect),
                        DrawCommand::PopClip => {
                            clips.pop();
                        }
                        DrawCommand::Text {
                            text,
                            color,
                            position,
                            ..
                        } if text == title => {
                            title_commands += 1;
                            assert_eq!(color.a, u8::MAX, "{title} at {width}px must not fade");
                            assert!(
                                clips.iter().all(|clip| {
                                    position.x >= clip.x - 0.01
                                        && position.x + shaped_width <= clip.x + clip.width + 0.01
                                }),
                                "{title} at {width}px must fit its native shaped width"
                            );
                        }
                        _ => {}
                    }
                }
                assert_eq!(
                    title_commands, 1,
                    "{title} at {width}px needs one opaque text run"
                );
            }
        }
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn closing_a_tab_does_not_activate_its_parent_and_hover_geometry_is_stable() {
        let state = tab_state();
        let mut tree = UiTree::new(header(&state, 520.0));
        tree.set_reduced_motion(true);
        let size = Size::new(520.0, RIGHT_HEADER_HEIGHT);
        let frame = tree.layout(size);
        let close = frame
            .regions
            .iter()
            .find(|region| region.id == "dock_close:1")
            .unwrap()
            .rect;
        let point = Point::new(close.x + close.width / 2.0, close.y + close.height / 2.0);
        tree.handle_event(UiEvent::PointerMove(point));
        let hovered = tree.layout(size);
        assert_eq!(hovered.hit_test(point), Some("dock_close:1"));
        assert_eq!(
            hovered
                .regions
                .iter()
                .find(|region| region.id == "dock_close:1")
                .unwrap()
                .rect,
            close
        );
        assert!(tree.handle_event(UiEvent::PointerDown(point)).is_empty());
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "dock_close:1".into()
            }]
        );
        assert!(hovered.commands.iter().any(|command| matches!(command,
            DrawCommand::Icon { rect, icon: Icon::Menu, .. }
                if rect.x > 400.0
        )));
    }

    #[test]
    fn terminal_toggle_remains_discoverable_and_keeps_creation_separate() {
        for visible in [false, true] {
            let mut state = tab_state();
            state.show_terminal_panel = visible;
            // Tooltips use the window viewport, while the bar retains its
            // compact explicit height inside the column.
            let size = Size::new(320.0, 400.0);
            let root = |state: &ViewState| {
                Element::column(vec![header(state, size.width), Element::spacer().flex(1.0)])
            };
            let mut tree = UiTree::new(root(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(size);
            let toggle = frame
                .regions
                .iter()
                .find(|r| r.id == "terminal_toggle")
                .unwrap()
                .rect;
            let point = Point::new(
                toggle.x + toggle.width / 2.0,
                toggle.y + toggle.height / 2.0,
            );
            assert_eq!((toggle.width, toggle.height), (TOOL_SIZE, TOOL_SIZE));
            assert!(toggle.y >= 0.0 && toggle.y + toggle.height <= RIGHT_HEADER_HEIGHT);
            assert_eq!(frame.hit_test(point), Some("terminal_toggle"));
            for (id, expected_icon) in [
                ("terminal_toggle", Icon::Terminal),
                ("dock_add_tab", Icon::Add),
                ("dock_tabs_menu", Icon::Menu),
                ("toggle_side_pane", Icon::PanelRightClose),
            ] {
                let action = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Icon { icon, rect, tint }
                        if *icon == expected_icon && *tint == theme::FOREGROUND
                            && (rect.x + rect.width / 2.0 - action.rect.x - action.rect.width / 2.0).abs() < 0.01
                            && (rect.y + rect.height / 2.0 - action.rect.y - action.rect.height / 2.0).abs() < 0.01
                )), "available dock action: {id}");
            }
            assert_eq!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, color, .. }
                        if *rect == toggle && *color == theme::SELECTED
                )),
                visible
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "terminal_toggle".into(),
                }]
            );
            assert!(tree.focus("terminal_toggle"));
            let hint = tree.layout(size);
            let hint_text = hint
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let hint_text = hint_text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                hint_text.contains(if visible {
                    "Ocultar terminal Ctrl + `"
                } else {
                    "Mostrar terminal Ctrl + `"
                }),
                "missing readable terminal action hint: {hint_text}"
            );
            state.show_dock_add_menu = true;
            tree.set_root(root(&state));
            let menu = tree.layout(Size::new(size.width, 400.0));
            assert!(menu
                .regions
                .iter()
                .any(|region| region.id == "dock_open:terminal"));
        }
    }

    #[test]
    #[ignore = "opt-in compact dock strip idle, keyboard, close-hover and overflow visual review"]
    fn export_flat_dock_tab_strip_visual_review() {
        use eframe::egui;
        use std::time::{Duration, Instant};
        let directory = std::path::Path::new("/tmp/openide-flat-dock-tabs-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [320.0, 520.0, 900.0] {
            for mode in ["idle", "keyboard", "close-hover", "overflow"] {
                let mut state = tab_state();
                state.show_dock_tabs_menu = mode == "overflow";
                let height = if mode == "overflow" { 300.0 } else { 84.0 };
                let ctx = egui::Context::default();
                crate::fonts::install(&ctx);
                ctx.begin_pass(egui::RawInput::default());
                let mut tree = UiTree::new(
                    Element::column(vec![header(&state, width), Element::spacer().flex(1.0)])
                        .background(theme::BACKGROUND)
                        .size(Some(width), Some(height)),
                );
                tree.set_reduced_motion(true);
                let font = egui::FontId::monospace(14.0);
                tree.set_metrics(ctx.fonts_mut(|fonts| openide_ui::FontMetrics {
                    char_width: fonts.glyph_width(&font, 'M'),
                    line_height: fonts.row_height(&font),
                }));
                let font = egui::FontId::proportional(14.0);
                tree.set_sans_metrics(ctx.fonts_mut(|fonts| openide_ui::FontMetrics {
                    char_width: fonts.glyph_width(&font, 'M'),
                    line_height: fonts.row_height(&font),
                }));
                let measurer = ctx.clone();
                tree.set_text_measurer(move |text, size, family| {
                    measurer.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                text.to_owned(),
                                egui::FontId::new(
                                    size,
                                    match family {
                                        FontFamily::Monospace => egui::FontFamily::Monospace,
                                        FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                                    },
                                ),
                                egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    })
                });
                let start = Instant::now();
                let size = Size::new(width, height);
                let initial = tree.layout_at(size, start);
                if mode == "keyboard" {
                    assert!(tree.focus("dock_tab:1"));
                }
                if mode == "close-hover" {
                    let rect = initial
                        .regions
                        .iter()
                        .find(|region| region.id == "dock_close:1")
                        .unwrap()
                        .rect;
                    tree.handle_event_at(
                        UiEvent::PointerMove(Point::new(
                            rect.x + rect.width / 2.0,
                            rect.y + rect.height / 2.0,
                        )),
                        start,
                    );
                }
                let frame = tree.layout_at(size, start + Duration::from_millis(120));
                let path = directory.join(format!("tabs-{width}-{mode}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                    .unwrap();
                let parsed = resvg::usvg::Tree::from_str(
                    &std::fs::read_to_string(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixels =
                    resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
                resvg::render(
                    &parsed,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixels.as_mut(),
                );
                pixels.save_png(path.with_extension("png")).unwrap();
                ctx.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn selected_tab_and_its_close_remain_visible_at_every_width() {
        let mut state = super::super::tests::empty_state();
        state.dock_tabs = (0..24)
            .map(|index| DockTabView {
                full_path: None,
                id: index + 1,
                title: format!("archivo-{index}.rs"),
                active: false,
                kind: SideTab::FilePreview,
                closable: true,
            })
            .collect();
        for width in [320.0, 430.0, 540.0, 900.0] {
            for selected in [0, 7, 23] {
                for (index, tab) in state.dock_tabs.iter_mut().enumerate() {
                    tab.active = index == selected;
                }
                let frame = UiTree::new(header(&state, width)).layout(Size::new(width, 48.0));
                for id in [
                    format!("dock_tab:{}", selected + 1),
                    format!("dock_close:{}", selected + 1),
                    "dock_add_tab".into(),
                    "terminal_toggle".into(),
                    "dock_tabs_menu".into(),
                    "toggle_side_pane".into(),
                ] {
                    let region = frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap_or_else(|| panic!("missing {id}"));
                    assert!(region.rect.width >= 16.0, "{id}: {:?}", region.rect);
                    if id.starts_with("dock_close:") {
                        assert_eq!(region.rect.width, CLOSE_SIZE);
                        assert_eq!(region.rect.height, CLOSE_SIZE);
                    }
                    assert_eq!(
                        frame.hit_test(Point::new(
                            region.rect.x + region.rect.width / 2.0,
                            region.rect.y + region.rect.height / 2.0
                        )),
                        Some(id.as_str()),
                        "{id} hit belongs to its control"
                    );
                    assert!(
                        region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1,
                        "{id}: {:?}",
                        region.rect
                    );
                }
                assert!(!frame
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("dock_tab_page")));
            }
        }
    }

    #[test]
    fn wheel_scroll_moves_continuously_keeps_toolbar_fixed_and_reveals_new_selection() {
        let mut state = super::super::tests::empty_state();
        state.dock_tabs = (1..=8)
            .map(|id| DockTabView {
                full_path: None,
                id,
                title: format!("Terminal {id}"),
                active: id == 1,
                kind: SideTab::Terminal,
                closable: true,
            })
            .collect();
        let mut tree = UiTree::new(header(&state, 320.0));
        tree.set_reduced_motion(true);
        let first = tree.layout(Size::new(320.0, 48.0));
        let button = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .map(|region| region.rect)
        };
        let toolbar = button(&first, "dock_add_tab").unwrap();
        tree.handle_event(UiEvent::Scroll2D {
            position: Point::new(90.0, 22.0),
            delta_x: -80.0,
            delta_y: 0.0,
            shift: false,
            direct: true,
        });
        let moved = tree.layout(Size::new(320.0, 48.0));
        assert_eq!(button(&moved, "dock_add_tab"), Some(toolbar));
        assert!(
            button(&moved, "dock_tab:2").unwrap().x
                < button(&first, "dock_tab:2").unwrap().x - 60.0
        );
        tree.set_root(header(&state, 320.0));
        let unchanged = tree.layout(Size::new(320.0, 48.0));
        assert_eq!(
            button(&unchanged, "dock_tab:2"),
            button(&moved, "dock_tab:2")
        );
        for tab in &mut state.dock_tabs {
            tab.active = tab.id == 7;
        }
        tree.set_root(header(&state, 320.0));
        let selected = tree.layout(Size::new(320.0, 48.0));
        for id in ["dock_tab:7", "dock_close:7"] {
            let rect = button(&selected, id).unwrap();
            assert!(rect.x >= 0.0 && rect.x + rect.width <= toolbar.x - 3.0);
            assert_eq!(
                selected.hit_test(Point::new(
                    rect.x + rect.width / 2.0,
                    rect.y + rect.height / 2.0
                )),
                Some(id)
            );
        }
    }

    #[test]
    fn overflow_menu_has_unique_actions_and_overview_has_no_duplicate_tabs() {
        let mut state = super::super::tests::empty_state();
        state.show_dock_tabs_menu = true;
        let frame = UiTree::new(header(&state, 400.0)).layout(Size::new(400.0, 600.0));
        let mut ids = std::collections::HashSet::new();
        for region in &frame.regions {
            assert!(ids.insert(&region.id), "duplicate region {}", region.id);
        }
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("dock_tab_menu:")));
        state.show_dock_tabs_menu = false;
        state.show_dock_overview = true;
        let frame = UiTree::new(header(&state, 400.0)).layout(Size::new(400.0, 600.0));
        assert!(!frame.regions.iter().any(
            |region| region.id.starts_with("dock_tab:") || region.id.starts_with("dock_close:")
        ));
    }

    #[test]
    fn overflow_fades_are_passive_and_reveal_keeps_active_close_clear() {
        let mut state = tab_state();
        state.dock_tabs.extend((4..14).map(|id| DockTabView {
            full_path: None,
            id,
            title: format!("file-{id}.rs"),
            active: false,
            kind: SideTab::FilePreview,
            closable: true,
        }));
        let mut tree = UiTree::new(header(&state, 320.0));
        let size = Size::new(320.0, RIGHT_HEADER_HEIGHT);
        let first = tree.layout(size);
        let fades = |frame: &openide_ui::Frame| {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. }
                        if color.r == dock_tab_ui::STRIP_BACKGROUND.r
                            && color.g == dock_tab_ui::STRIP_BACKGROUND.g
                            && color.b == dock_tab_ui::STRIP_BACKGROUND.b
                            && color.a < 255
                            && rect.height == TAB_HEIGHT =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(fades(&first).len(), 16);
        let strip = first
            .regions
            .iter()
            .find(|region| region.id == "right_dock_tabs")
            .unwrap()
            .rect;
        // Local decoration must retain the scroll clip instead of becoming a
        // free overlay that can paint over the fixed tools after scrolling.
        let mut clips = Vec::new();
        for command in &first.commands {
            match command {
                DrawCommand::PushClip(rect) => clips.push(*rect),
                DrawCommand::PopClip => {
                    clips.pop();
                }
                DrawCommand::FillRect { rect, color, .. }
                    if *color == theme::BORDER && rect.width == 1.0 && rect.height == 14.0 =>
                {
                    assert!(
                        clips.iter().any(|clip| {
                            clip.x >= strip.x && clip.x + clip.width <= strip.x + strip.width + 0.01
                        }),
                        "inactive separators must stay inside the tab viewport"
                    );
                }
                _ => {}
            }
        }
        let edge = Point::new(strip.x + strip.width - 2.0, strip.y + strip.height / 2.0);
        assert_eq!(first.hit_test(edge), Some("dock_tab:2"));
        for tab in &mut state.dock_tabs {
            tab.active = tab.id == 8;
        }
        tree.set_root(header(&state, 320.0));
        let frame = tree.layout(size);
        assert_eq!(fades(&frame).len(), 32);
        let close = frame
            .regions
            .iter()
            .find(|region| region.id == "dock_close:8")
            .unwrap()
            .rect;
        assert!(close.x >= HEADER_PADDING + 16.0);
        assert!(close.x + close.width <= 320.0 - HEADER_PADDING - TOOLBAR_WIDTH - 8.0 - 16.0);
        assert_eq!(
            frame.hit_test(Point::new(close.x + 12.0, close.y + 12.0)),
            Some("dock_close:8")
        );
    }
}
