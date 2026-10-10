//! Familiar application menus, rendered with OpenIDE's native context menus.
//! Commands are scoped to the document visible when the menu was painted.
use super::*;
use crate::ide_workspace::IdeActivity;
use crate::keymap::ShortcutAction;
use openide_ui::{MenuBar, MenuBarMenu};

const MENUS: [(&str, &str, f32); 7] = [
    ("file", "Archivo", 60.0),
    ("edit", "Editar", 52.0),
    ("selection", "Selección", 76.0),
    ("view", "Ver", 36.0),
    ("go", "Ir", 28.0),
    ("run", "Ejecutar", 64.0),
    ("terminal", "Terminal", 64.0),
];

fn command(state: &ViewState, action: &str, label: &str) -> DropdownMenuItem {
    let editor = state.code_editor.as_ref().and_then(|editor| {
        let snapshot = editor.controller.as_ref()?.snapshot().ok()?;
        Some((editor.tab_id, snapshot.document_id()))
    });
    let id = format!(
        "ide_menu_action:{}",
        serde_json::to_string(&(editor, action)).expect("menu action tuple")
    );
    DropdownMenuItem::new(id, label)
}

fn editor_command(
    state: &ViewState,
    action: &str,
    label: &str,
    shortcut: &str,
) -> DropdownMenuEntry {
    let mut item = command(state, action, label);
    if !shortcut.is_empty() {
        item = item.shortcut(shortcut);
    }
    let ready = state.side_tab == SideTab::FilePreview
        && state
            .document_preview
            .as_ref()
            .is_none_or(|view| view.is_source_visible())
        && state
            .code_editor
            .as_ref()
            .is_some_and(|editor| !editor.loading && editor.controller.is_some());
    if !ready {
        item = item.disabled();
    }
    if matches!(action, "cut" | "copy")
        && !state
            .code_editor
            .as_ref()
            .and_then(|editor| editor.controller.as_ref())
            .and_then(|controller| controller.snapshot().ok())
            .is_some_and(|snapshot| snapshot.selections().any(|selection| !selection.is_empty()))
    {
        item = item.disabled();
    }
    if matches!(
        action,
        "editor_definition" | "editor_references" | "editor_format"
    ) && !state.editor_workbench.language_ready
    {
        item = item.disabled();
    }
    if action == "editor_wrap" {
        item = item.selected(state.editor_workbench.soft_wrap);
    }
    if action == "editor_minimap" {
        item = item.selected(state.editor_workbench.minimap);
    }
    item.into()
}

fn entries(state: &ViewState, menu: &str) -> Vec<DropdownMenuEntry> {
    let sep = || DropdownMenuEntry::Separator;
    let editor = |action, label, shortcut| editor_command(state, action, label, shortcut);
    let action = |id, label| command(state, id, label);
    let bound = |id, label, shortcut_action| {
        let item = action(id, label);
        let shortcut = state.keymap_profile.shortcut(shortcut_action);
        if shortcut.is_empty() { item } else { item.shortcut(shortcut) }
    };
    match menu {
        "file" => {
            let mut folder = action("composer_project_browse", "Abrir carpeta…");
            if state.running || state.composer_branches.switching() {
                folder = folder.disabled();
            }
            let mut save = action("save", "Guardar").shortcut("Ctrl+S");
            if state.side_tab != SideTab::FilePreview
                || !state.code_editor.as_ref().is_some_and(|editor| {
                    !editor.loading
                        && !editor.saving
                        && editor
                            .controller
                            .as_ref()
                            .and_then(|c| c.snapshot().ok())
                            .is_some_and(|snapshot| snapshot.is_dirty())
                })
            {
                save = save.disabled();
            }
            let mut reopen =
                action("dock_reopen_last_file", "Reabrir archivo cerrado").shortcut("Ctrl+Shift+T");
            if !state
                .dock_recently_closed
                .iter()
                .any(|tab| tab.kind == SideTab::FilePreview && tab.restorable)
            {
                reopen = reopen.disabled();
            }
            vec![
                bound("ide_new_file", "Nuevo archivo…", ShortcutAction::NewFile).into(),
                bound("ide_quick_open", "Abrir archivo…", ShortcutAction::QuickOpen)
                    .into(),
                folder.into(),
                reopen.into(),
                sep(),
                save.into(),
                editor("editor_save_as", "Guardar como…", ""),
                sep(),
                editor("editor_copy_path", "Copiar ruta completa", ""),
                editor("editor_copy_relative", "Copiar ruta relativa", ""),
            ]
        }
        "edit" => vec![
            editor("editor_undo", "Deshacer", "Ctrl+Z"),
            editor("editor_redo", "Rehacer", "Ctrl+Shift+Z"),
            sep(),
            editor("cut", "Cortar", "Ctrl+X"),
            editor("copy", "Copiar", "Ctrl+C"),
            editor("paste", "Pegar", "Ctrl+V"),
            sep(),
            editor("editor_find", "Buscar", "Ctrl+F"),
            editor("editor_replace", "Reemplazar", "Ctrl+H"),
            sep(),
            editor("editor_comment", "Comentar / descomentar", ""),
            editor("editor_format", "Formatear documento", ""),
        ],
        "selection" => vec![
            editor("select_all", "Seleccionar todo", "Ctrl+A"),
            editor("editor_select_next", "Siguiente coincidencia", "Ctrl+D"),
            editor(
                "editor_select_all_matches",
                "Todas las coincidencias",
                "Ctrl+Shift+L",
            ),
            sep(),
            editor("editor_duplicate", "Duplicar líneas", state.keymap_profile.shortcut(ShortcutAction::DuplicateLines)),
            editor("editor_move_up", "Mover líneas arriba", ""),
            editor("editor_move_down", "Mover líneas abajo", ""),
            sep(),
            editor("editor_cursor_above", "Añadir cursor arriba", ""),
            editor("editor_cursor_below", "Añadir cursor abajo", ""),
        ],
        "view" => vec![
            bound("commands", "Paleta de comandos…", ShortcutAction::CommandPalette)
                .into(),
            sep(),
            bound("ide_activity:files", "Archivos", ShortcutAction::Files)
                .selected(state.ide.activity == IdeActivity::Files && state.ide.explorer_open)
                .into(),
            bound("ide_activity:search", "Buscar", ShortcutAction::Search)
                .selected(state.ide.activity == IdeActivity::Search && state.ide.explorer_open)
                .into(),
            bound("ide_activity:git", "Control de versiones", ShortcutAction::Git)
                .selected(state.ide.activity == IdeActivity::Git && state.ide.explorer_open)
                .into(),
            bound("ide_activity:debug", "Depuración", ShortcutAction::Debug)
                .selected(state.ide.activity == IdeActivity::Debug && state.ide.explorer_open)
                .into(),
            bound("ide_activity:extensions", "Complementos", ShortcutAction::Extensions)
                .selected(state.ide.activity == IdeActivity::Extensions && state.ide.explorer_open)
                .into(),
            action("dock_open:browser", "Navegador").icon(Icon::Globe).into(),
            sep(),
            bound("ide_explorer_toggle", "Mostrar barra lateral", ShortcutAction::ToggleSidebar)
                .selected(state.ide.explorer_open)
                .into(),
            action("ide_agent_toggle", "Mostrar panel del agente")
                .selected(state.ide.agent_open)
                .into(),
            bound("ide_bottom_toggle", "Mostrar panel inferior", ShortcutAction::ToggleBottomPanel)
                .selected(state.show_terminal_panel || state.ide.bottom_open)
                .into(),
            sep(),
            bound("ide_bottom:problems", "Problemas", ShortcutAction::Problems).into(),
            bound("ide_bottom:output", "Salida", ShortcutAction::Output).into(),
            bound("ide_bottom:console", "Consola de depuración", ShortcutAction::DebugConsole).into(),
            sep(),
            editor("editor_outline", "Símbolos del archivo", ""),
            editor("editor_wrap", "Ajustar líneas al ancho", ""),
            editor("editor_minimap", "Mostrar minimapa", ""),
            sep(),
            bound("toggle_settings_header", "Configuración", ShortcutAction::Settings).into(),
        ],
        "go" => vec![
            bound("ide_quick_open", "Ir a archivo…", ShortcutAction::QuickOpen)
                .into(),
            editor("editor_goto", "Ir a línea…", "Ctrl+G"),
            editor("editor_outline", "Ir a símbolo del archivo…", ""),
            sep(),
            editor("editor_definition", "Ir a definición", "F12"),
            editor("editor_references", "Buscar referencias", "Shift+F12"),
        ],
        "run" => {
            let mut actions = vec![
                action("dock_open:debugger", "Abrir depurador").into(),
                sep(),
            ];
            let controls = &state.debug.controls;
            for (operation, label, enabled) in [
                ("start", "Iniciar depuración", controls.start),
                ("resume", "Continuar", controls.resume),
                ("pause", "Pausar", controls.pause),
                ("step_over", "Paso por encima", controls.step_over),
                ("step_into", "Paso a paso", controls.step_into),
                ("step_out", "Salir de la función", controls.step_out),
                ("stop", "Detener depuración", controls.stop),
            ] {
                let action_id = crate::debug_ui::action(&state.debug.scope, operation);
                let item = command(state, &action_id, label);
                actions.push(if enabled { item } else { item.disabled() }.into());
            }
            actions.push(sep());
            actions.push(editor("editor_breakpoint", "Alternar breakpoint", "F9"));
            actions
        }
        "terminal" => {
            let visible = state.show_terminal_panel;
            let mut split = action(
                if state.terminal_controls.panes.len() == 2 {
                    "terminal_unsplit"
                } else {
                    "terminal_split"
                },
                if state.terminal_controls.panes.len() == 2 {
                    "Quitar división"
                } else {
                    "Dividir terminal"
                },
            );
            let mut maximize = action("terminal_panel_maximize", "Maximizar panel")
                .selected(state.terminal_maximized);
            if !visible {
                split = split.disabled();
                maximize = maximize.disabled();
            }
            vec![
                action("terminal_new", "Nueva terminal").into(),
                bound("terminal_toggle", "Mostrar terminal", ShortcutAction::ToggleTerminal)
                    .selected(visible)
                    .into(),
                split.into(),
                maximize.into(),
                sep(),
                action("terminal_settings", "Configurar terminal…").into(),
            ]
        }
        _ => Vec::new(),
    }
}

pub(super) fn menubar(state: &ViewState, compact: bool) -> MenuBar {
    let current = state.ide.menu_open.as_deref().map(|menu| {
        if !compact && menu == "all" {
            "file"
        } else {
            menu
        }
    });
    let open_id = current.map(|menu| format!("ide_menu:{menu}"));
    let bar = MenuBar::new(
        MENUS
            .iter()
            .map(|(id, label, width)| {
                // Only what is shown is built: every menu behind the compact
                // button, one under the full bar.
                let items = if current == Some(*id) || (compact && current.is_some()) {
                    entries(state, id)
                } else {
                    Vec::new()
                };
                MenuBarMenu::new(
                    format!("ide_menu:{id}"),
                    *label,
                    *width,
                    format!("ide_menu_dismiss:{id}"),
                    items,
                )
                .compact_id(format!("ide_menu_category:{id}"))
            })
            .collect(),
        open_id.as_deref(),
        theme::action_palette(theme::WINDOW, theme::FOREGROUND),
    )
    .menu_palette(theme::menu_palette());
    if compact {
        bar.compact(
            "ide_menu:all",
            "ide_menu_dismiss",
            TooltipSpec::new("Menú principal").placement(TooltipPlacement::Below),
        )
    } else {
        bar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{ButtonSize, ControlState, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn navigation_menu_accelerators_follow_the_profile_without_blank_key_labels() {
        for profile in crate::keymap::KeymapProfile::ALL {
            let mut state = crate::view::tests::empty_state();
            state.keymap_profile = profile;
            for (menu, target, action) in [
                ("file", "ide_new_file", ShortcutAction::NewFile),
                ("file", "ide_quick_open", ShortcutAction::QuickOpen),
                ("view", "commands", ShortcutAction::CommandPalette),
                ("view", "ide_activity:debug", ShortcutAction::Debug),
                ("view", "ide_explorer_toggle", ShortcutAction::ToggleSidebar),
                ("view", "ide_bottom_toggle", ShortcutAction::ToggleBottomPanel),
                ("view", "ide_bottom:problems", ShortcutAction::Problems),
                ("view", "ide_bottom:output", ShortcutAction::Output),
                ("view", "ide_bottom:console", ShortcutAction::DebugConsole),
                ("view", "toggle_settings_header", ShortcutAction::Settings),
                ("terminal", "terminal_toggle", ShortcutAction::ToggleTerminal),
                ("selection", "editor_duplicate", ShortcutAction::DuplicateLines),
            ] {
                let item = entries(&state, menu).into_iter().find_map(|entry| {
                    let DropdownMenuEntry::Item(item) = entry else { return None; };
                    let data = item.action_id.strip_prefix("ide_menu_action:")?;
                    let (_, id) = serde_json::from_str::<(Option<(u64, u64)>, String)>(data).ok()?;
                    (id == target).then_some(item)
                }).expect("navigation action remains available in the menu");
                let expected = profile.shortcut(action);
                assert_eq!(item.shortcut.as_deref(), (!expected.is_empty()).then_some(expected), "{profile:?} {target}");
            }
        }
    }

    #[test]
    fn ide_view_menu_keeps_activity_destinations_separate_from_panel_visibility() {
        let state = crate::view::tests::empty_state();
        let targets = entries(&state, "view")
            .into_iter()
            .filter_map(|entry| {
                let DropdownMenuEntry::Item(item) = entry else {
                    return None;
                };
                let data = item.action_id.strip_prefix("ide_menu_action:")?;
                serde_json::from_str::<(Option<(u64, u64)>, String)>(data)
                    .ok()
                    .map(|(_, target)| target)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            targets
                .iter()
                .filter_map(|target| target.strip_prefix("ide_activity:"))
                .collect::<Vec<_>>(),
            vec!["files", "search", "git", "debug", "extensions"]
        );
        for toggle in [
            "ide_explorer_toggle",
            "ide_agent_toggle",
            "ide_bottom_toggle",
        ] {
            assert!(targets.iter().any(|target| target == toggle));
        }
        assert!(!targets
            .iter()
            .any(|target| target == "dock_open:review" || target == "ide_debug_toggle"));
        assert!(targets.iter().any(|target| target == "dock_open:browser"));
    }

    #[test]
    fn run_menu_uses_session_capabilities_even_when_debug_panel_is_hidden() {
        let mut state = crate::view::tests::empty_state();
        state.debug_open = false;
        state.debug.controls.resume = true;
        state.debug.controls.stop = true;
        let mut operations = std::collections::BTreeMap::new();
        for entry in entries(&state, "run") {
            let DropdownMenuEntry::Item(item) = entry else {
                continue;
            };
            let data = item.action_id.strip_prefix("ide_menu_action:").unwrap();
            let (_, target) = serde_json::from_str::<(Option<(u64, u64)>, String)>(data).unwrap();
            let Some(data) = target.strip_prefix("debug_control:") else {
                continue;
            };
            let (_, operation) = serde_json::from_str::<(String, String)>(data).unwrap();
            operations.insert(operation, item.state == ControlState::Enabled);
        }
        assert_eq!(operations.len(), 7);
        for (operation, enabled) in operations {
            assert_eq!(
                enabled,
                matches!(operation.as_str(), "resume" | "stop"),
                "{operation}"
            );
        }
    }

    #[test]
    fn ide_menu_unavailable_editor_commands_are_disabled_but_open_file_remains_available() {
        let state = crate::view::tests::empty_state();
        let file = entries(&state, "file");
        for entry in file {
            let DropdownMenuEntry::Item(item) = entry else {
                continue;
            };
            let data = item.action_id.strip_prefix("ide_menu_action:").unwrap();
            let (_, target) = serde_json::from_str::<(Option<(u64, u64)>, String)>(data).unwrap();
            assert_eq!(
                item.state == ControlState::Enabled,
                matches!(
                    target.as_str(),
                    "ide_new_file" | "ide_quick_open" | "composer_project_browse"
                ),
                "{target}"
            );
        }
    }

    #[test]
    fn ide_menu_compact_categories_use_native_submenus_and_escape_dismissal() {
        let mut state = crate::view::tests::empty_state();
        state.ide.menu_open = Some("all".into());
        let mut tree = UiTree::new(menubar(&state, true).render());
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(360.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "ide_menu_category:file"));
        assert!(tree.has_open_popover());
        assert!(tree.focus("ide_menu_category:file"));
        tree.layout(Size::new(360.0, 600.0));
        tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::ArrowRight,
            ctrl: false,
            shift: false,
        });
        let actions = tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Enter,
            ctrl: false,
            shift: false,
        });
        assert!(actions
            .iter()
            .any(|action| matches!(action, UiAction::Clicked { id }
            if id.contains("ide_new_file"))));
        // A fresh root also verifies Escape before entering a submenu.
        let mut tree = UiTree::new(menubar(&state, true).render());
        tree.set_reduced_motion(true);
        tree.layout(Size::new(360.0, 600.0));
        let actions = tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Escape,
            ctrl: false,
            shift: false,
        });
        assert!(actions.contains(&UiAction::Clicked {
            id: "ide_menu_dismiss".into()
        }));
    }

    // The bar as it was built by hand before limn's `MenuBar`, kept to prove
    // the two paint the same frames. Delete with the final audit (S18).
    fn legacy_menubar(state: &ViewState, compact: bool) -> Element {
        let current = state.ide.menu_open.as_deref().map(|menu| {
            if !compact && menu == "all" {
                "file"
            } else {
                menu
            }
        });
        if compact {
            let items = if current.is_some() {
                MENUS
                    .iter()
                    .map(|(id, label, _)| {
                        DropdownMenuItem::new(format!("ide_menu_category:{id}"), *label)
                            .submenu(entries(state, id))
                            .into()
                    })
                    .collect()
            } else {
                Vec::new()
            };
            return DropdownMenu::new(
                Element::panel(
                    UiIconButton::new(
                        "ide_menu:all",
                        Icon::Menu,
                        theme::action_palette(theme::WINDOW, theme::FOREGROUND),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .active(current.is_some())
                    .active_hover(ActiveHover::Palette)
                    .tooltip_with(TooltipSpec::new("Menú principal").placement(TooltipPlacement::Below))
                    .render(),
                )
                .size(Some(32.0), Some(24.0)),
                items,
                current.is_some(),
                "ide_menu_dismiss",
                theme::menu_palette(),
            )
            .context_menu_style()
            .render();
        }
        Element::row(
            MENUS
                .iter()
                .map(|(id, label, width)| {
                    let palette = theme::action_palette(theme::WINDOW, theme::FOREGROUND);
                    let anchor = palette.interactive_surface(
                        Button::new(format!("ide_menu:{id}"), *label, palette)
                            .variant(ButtonVariant::Ghost)
                            .button_size(ButtonSize::Small)
                            .render()
                            .font_size(12.0)
                            .padding(4.0)
                            .min_size(0.0, 0.0)
                            .background(if current == Some(*id) {
                                theme::SELECTED
                            } else {
                                theme::WINDOW
                            })
                            .size(Some(*width), Some(24.0)),
                    );
                    let items = if current == Some(*id) {
                        entries(state, id)
                    } else {
                        Vec::new()
                    };
                    let initial_focus = items.iter().find_map(|entry| match entry {
                        DropdownMenuEntry::Item(item)
                            if item.state == openide_ui::ControlState::Enabled =>
                        {
                            Some(item.action_id.clone())
                        }
                        _ => None,
                    });
                    let menu = DropdownMenu::new(
                        anchor,
                        items,
                        current == Some(*id),
                        format!("ide_menu_dismiss:{id}"),
                        theme::menu_palette(),
                    )
                    .context_menu_style()
                    .render();
                    if let Some(initial_focus) = initial_focus {
                        menu.popover_initial_focus(initial_focus)
                    } else {
                        menu
                    }
                })
                .collect(),
        )
        .gap(2.0)
        .size(Some(392.0), Some(24.0))
    }

    #[test]
    fn the_menu_bar_paints_the_frames_it_did_by_hand() {
        let frame = |bar: Element, width: f32| {
            let mut tree = UiTree::new(Element::column(vec![
                Element::row(vec![bar, Element::spacer().flex(1.0)]),
                Element::spacer().flex(1.0),
            ]));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(width, 600.0))
        };
        let mut state = crate::view::tests::empty_state();
        let open = ["all", "file", "edit", "selection", "view", "go", "run", "terminal"];
        for menu in std::iter::once(None).chain(open.into_iter().map(Some)) {
            state.ide.menu_open = menu.map(str::to_owned);
            for compact in [false, true] {
                assert_eq!(
                    menubar(&state, compact).width(),
                    if compact { 32.0 } else { 392.0 }
                );
                for width in [360.0, 900.0, 1500.0] {
                    assert_eq!(
                        frame(menubar(&state, compact).render(), width),
                        frame(legacy_menubar(&state, compact), width),
                        "menu={menu:?} compact={compact} width={width}"
                    );
                }
                crate::icon_button_parity_tests::assert_same(
                    "menu bar",
                    legacy_menubar(&state, compact),
                    menubar(&state, compact).render(),
                );
            }
        }
    }
}
