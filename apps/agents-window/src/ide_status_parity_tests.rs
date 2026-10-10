//! The status bar as it was built by hand before limn's `StatusBar` and
//! `StatusBarItem`, kept to prove the two paint the same frames. Delete with
//! the final audit (S18).

use super::*;
use openide_ui::{DropdownMenu, PopoverPlacement, Separator, Tooltip, TooltipPlacement};

fn text(value: &str) -> Element {
    Element::faded_text(value, 6.0)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(theme::FOREGROUND_SUBTLE)
}

fn control(
    id: &str,
    action: &str,
    value: &str,
    icon: Option<Icon>,
    hint: &str,
    width: f32,
    surface: Color,
) -> Element {
    control_tooltip(id, action, value, icon, hint, width, surface).render()
}

fn control_tooltip(
    id: &str,
    action: &str,
    value: &str,
    icon: Option<Icon>,
    hint: &str,
    width: f32,
    surface: Color,
) -> Tooltip {
    let mut children = Vec::new();
    if let Some(icon) = icon {
        children.push(
            Element::icon(icon)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(12.0), Some(16.0)),
        );
    }
    if !value.is_empty() {
        children.push(text(value).size(None, Some(16.0)).flex(1.0));
    }
    Tooltip::new(
        theme::action_palette(surface, theme::FOREGROUND_SUBTLE).bar_item_surface(
            Element::clickable(id, Element::row(children).gap(4.0))
                .dispatch_as(action)
                .padding(2.0)
                .size(Some(width), Some(STATUS_HEIGHT)),
        ),
        id,
        hint,
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Above)
}

fn menu(anchor: Element, entries: Vec<DropdownMenuEntry>, open: bool, dismiss: &str) -> Element {
    DropdownMenu::new(anchor, entries, open, dismiss, theme::menu_palette())
        .context_menu_style()
        .placement(PopoverPlacement::TopEnd)
        .width(256.0)
        .render()
}

fn document_control(
    state: &ViewState,
    status: &DocumentStatus,
    kind: StatusMenu,
    id: &str,
    value: &str,
    hint: &str,
    width: f32,
    surface: Color,
) -> Element {
    let key = match kind {
        StatusMenu::Indent => "indent",
        StatusMenu::Ending => "ending",
        StatusMenu::Language => "language",
        StatusMenu::Overflow => "overflow",
        StatusMenu::Branch | StatusMenu::Services => unreachable!(),
    };
    let prefix = status.scope.prefix();
    let open = state.ide_status.menu == Some(kind)
        && state.ide_status.scope.as_ref() == Some(&status.scope);
    menu(
        control(
            id,
            &format!("{prefix}:open:{key}"),
            value,
            (kind == StatusMenu::Overflow).then_some(Icon::More),
            hint,
            width,
            surface,
        ),
        scoped_entries(state, status, kind),
        open,
        &format!("{prefix}:dismiss"),
    )
}

fn services_control(
    state: &ViewState,
    status: &DocumentStatus,
    width: f32,
    viewport_width: f32,
    surface: Color,
) -> Element {
    let prefix = status.scope.prefix();
    let anchor = control_tooltip(
        "ide_editor_language_service",
        &format!("{prefix}:open:services"),
        if width > 20.0 { "LSP" } else { "" },
        Some(Icon::Server),
        "Servicios de lenguaje",
        width,
        surface,
    )
    .description(&state.editor_workbench.language_status)
    .render();
    if !services_open(state, status) {
        return anchor;
    }
    services_popover(state, status, anchor, viewport_width)
}

fn branch(state: &ViewState, width: f32) -> Element {
    let value = state.project_branch.as_deref().unwrap_or("Git");
    let anchor = control(
        "ide_git_review",
        "ide_status_branch_open",
        if width > 24.0 { value } else { "" },
        Some(Icon::GitBranch),
        &format!("{value} · Ramas y cambios del repositorio"),
        width,
        theme::WINDOW,
    );
    menu(
        anchor,
        branch_entries(),
        branch_open(state),
        "ide_status_branch_dismiss",
    )
}

pub(crate) fn footer(state: &ViewState, width: f32) -> Element {
    if width < 240.0 {
        return status_cells(state, width, true);
    }
    let unread = state.notifications.unread();
    let mut cell = vec![
        Element::icon(crate::notifications_ui::bell_icon(&state.notifications))
            .size(Some(15.0), Some(16.0)),
    ];
    if unread > 0 {
        cell.push(text(&unread.min(99).to_string()).size(None, Some(16.0)));
    }
    // Kept clear of the window's rounded corner so the hover cell is whole.
    let corner_inset = 12.0;
    let bell_width = if unread > 0 {
        label_width(&unread.min(99).to_string()) + 28.0
    } else {
        28.0
    };
    let ink = if unread > 0 {
        theme::FOREGROUND
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let bell = Tooltip::new(
        theme::action_palette(theme::WINDOW, theme::FOREGROUND_SUBTLE).bar_item_surface(
            Element::clickable(
                "notifications_toggle",
                Element::row(cell).gap(3.0).foreground(ink).centered_x(),
            )
            .padding(2.0)
            .size(Some(bell_width), Some(STATUS_HEIGHT)),
        ),
        "notifications_toggle",
        "Notificaciones",
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Above)
    .render();
    Element::row(vec![
        status_cells(state, width - bell_width - corner_inset, true),
        Element::spacer().flex(1.0),
        crate::notifications_ui::popover(&state.notifications, bell, true),
        Element::spacer().size(Some(corner_inset), None),
    ])
    .size(Some(width), Some(STATUS_HEIGHT))
}

/// The dock owns one compact status row. It shares the IDE's document cells,
/// current snapshot, scoped actions and overflow; repository state stays in
/// the workspace chrome instead of being repeated under each document.
pub(crate) fn document_footer(state: &ViewState, width: f32) -> Element {
    let width = width.max(0.0);
    Element::column(vec![
        Separator::horizontal(theme::component_palette())
            .length(width)
            .render(),
        status_cells(state, width, false),
    ])
    .gap(0.0)
    .background(theme::BACKGROUND)
    .size(Some(width), Some(crate::ide_workspace::STATUSBAR_HEIGHT))
    .clip_children()
}

fn status_cells(state: &ViewState, width: f32, workspace: bool) -> Element {
    let width = width.max(0.0);
    let surface = if workspace {
        theme::WINDOW
    } else {
        theme::BACKGROUND
    };
    if width < 24.0 {
        return Element::spacer().size(Some(width), Some(STATUS_HEIGHT));
    }
    let status = document_status(state);
    // At an unusually narrow window width, document actions take precedence
    // over the repository shortcut so both LSP and overflow remain reachable.
    let branch_width = if !workspace || (status.is_some() && width < 144.0) {
        0.0
    } else if width >= 640.0 {
        (label_width(state.project_branch.as_deref().unwrap_or("Git")) + 16.0).min(180.0)
    } else if width >= 480.0 {
        100.0
    } else {
        20.0
    };
    let mut items = if branch_width > 0.0 {
        vec![branch(state, branch_width)]
    } else {
        Vec::new()
    };
    let Some(status) = status else {
        items.push(Element::spacer().flex(1.0));
        return Element::row(items)
            .gap(4.0)
            .size(Some(width), Some(STATUS_HEIGHT));
    };
    let prefix = status.scope.prefix();
    let diagnostic_hint = format!(
        "{} errores · {} advertencias · {} informativos · {} sugerencias · {} sin severidad{} · Mostrar problemas del archivo",
        status.errors,
        status.warnings,
        status.information,
        status.hints,
        status.unclassified,
        if status.stale {
            " · Diagnósticos desactualizados"
        } else {
            ""
        }
    );
    let problem_width =
        label_width(&status.errors.to_string()) + label_width(&status.warnings.to_string()) + 24.0;
    let problem = Element::row(vec![
        Element::icon(Icon::ErrorCircle).size(Some(12.0), Some(16.0)),
        text(&status.errors.to_string()).size(None, Some(16.0)),
        Element::icon(Icon::WarningTriangle).size(Some(12.0), Some(16.0)),
        text(&status.warnings.to_string()).size(None, Some(16.0)),
    ])
    .gap(3.0)
    .foreground(theme::FOREGROUND_SUBTLE);
    items.push(
        Tooltip::new(
            theme::action_palette(surface, theme::FOREGROUND_SUBTLE).bar_item_surface(
                Element::clickable("ide_editor_diagnostics", problem)
                    .dispatch_as(format!("{prefix}:diagnostics"))
                    .padding(2.0)
                    .size(Some(problem_width), Some(STATUS_HEIGHT)),
            ),
            "ide_editor_diagnostics",
            diagnostic_hint,
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Above)
        .render(),
    );
    let service_width = if width >= 520.0 { 44.0 } else { 20.0 };
    items.push(services_control(
        state,
        &status,
        service_width,
        width,
        surface,
    ));
    items.push(Element::spacer().flex(1.0));
    let budget = (width - branch_width - problem_width - service_width - 20.0).max(0.0);
    let full_cursor = if status.selection > 0 {
        format!("{} · {} sel.", status.cursor, status.selection)
    } else {
        status.cursor.clone()
    };
    let cursor = if budget < 150.0 {
        &status.compact_cursor
    } else {
        &full_cursor
    };
    let cursor_width = label_width(cursor);
    let candidates = [
        ("cursor", cursor_width),
        ("language", label_width(status.language)),
        ("indent", label_width(&status.indent)),
        ("ending", label_width(status.ending)),
        ("encoding", label_width("UTF-8")),
        ("save", label_width(status.save)),
    ];
    let full_width = candidates.iter().map(|(_, size)| size + 4.0).sum::<f32>();
    let overflow = full_width > budget;
    let mut remaining = (budget - if overflow { 24.0 } else { 0.0 }).max(0.0);
    let mut visible = Vec::new();
    for (key, size) in candidates {
        if size + 4.0 <= remaining {
            visible.push(key);
            remaining -= size + 4.0;
        }
    }
    for key in ["save", "cursor", "indent", "encoding", "ending", "language"] {
        if !visible.contains(&key) {
            continue;
        }
        let element = match key {
            "cursor" => control_tooltip(
                "ide_editor_cursor",
                &format!("{prefix}:goto"),
                cursor,
                None,
                &format!("{full_cursor} · Ir a línea y columna"),
                cursor_width,
                surface,
            )
            .shortcut("Ctrl+G")
            .render(),
            "indent" => document_control(
                state,
                &status,
                StatusMenu::Indent,
                "ide_editor_indent",
                &status.indent,
                "Sangría y ancho de tabulación",
                label_width(&status.indent),
                surface,
            ),
            "ending" => document_control(
                state,
                &status,
                StatusMenu::Ending,
                "ide_editor_ending",
                status.ending,
                "Cambiar finales de línea · Se puede deshacer",
                label_width(status.ending),
                surface,
            ),
            "language" => document_control(
                state,
                &status,
                StatusMenu::Language,
                "ide_editor_language",
                status.language,
                &format!("{} · Cambiar modo de lenguaje", status.language),
                label_width(status.language),
                surface,
            ),
            "encoding" => Tooltip::new(
                Element::panel(text("UTF-8"))
                    .padding(2.0)
                    .size(Some(label_width("UTF-8")), Some(STATUS_HEIGHT)),
                "ide_editor_encoding",
                "UTF-8 · Codificación validada al abrir; los archivos se guardan en UTF-8",
                theme::component_palette(),
            )
            .placement(TooltipPlacement::Above)
            .render(),
            _ => Tooltip::new(
                Element::panel(text(status.save))
                    .padding(2.0)
                    .size(Some(label_width(status.save)), Some(STATUS_HEIGHT)),
                "ide_editor_save_status",
                format!("{} · Guardar archivo", status.save),
                theme::component_palette(),
            )
            .shortcut("Ctrl+S")
            .placement(TooltipPlacement::Above)
            .render(),
        };
        items.push(element);
    }
    if overflow && budget >= 20.0 {
        items.push(document_control(
            state,
            &status,
            StatusMenu::Overflow,
            "ide_editor_status_more",
            "",
            "Estado y opciones del archivo",
            20.0,
            surface,
        ));
    }
    Element::row(items)
        .gap(4.0)
        .size(Some(width), Some(STATUS_HEIGHT))
        .clip_children()
}


mod tests {
    use super::super::tests::{editor_state, services_state};
    use super::*;
    use openide_ui::{DrawCommand, Frame, Size, UiTree};

    const WIDTHS: [f32; 10] = [
        20.0, 120.0, 144.0, 160.0, 240.0, 320.0, 520.0, 700.0, 900.0, 1440.0,
    ];

    fn frame(bar: Element, width: f32) -> Frame {
        let mut tree = UiTree::new(Element::column(vec![Element::spacer().flex(1.0), bar]));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(width, 460.0))
    }

    // `StatusBar` always clips its row; the hand-built bar clipped it only
    // with a document open.
    fn unclipped(frame: Frame) -> (Vec<DrawCommand>, Vec<String>) {
        (
            frame
                .commands
                .into_iter()
                .filter(|command| !matches!(command, DrawCommand::PushClip(_) | DrawCommand::PopClip))
                .collect(),
            frame.regions.iter().map(|region| format!("{region:?}")).collect(),
        )
    }

    fn assert_bars(name: &str, state: &ViewState) {
        let clipped = document_status(state).is_some();
        for width in WIDTHS {
            for (kind, legacy, current) in [
                ("footer", footer(state, width), super::super::footer(state, width)),
                (
                    "document_footer",
                    document_footer(state, width),
                    super::super::document_footer(state, width),
                ),
            ] {
                let (legacy, current) = (frame(legacy, width), frame(current, width));
                if clipped && width >= 24.0 {
                    assert_eq!(current, legacy, "{name} {kind} width={width}");
                } else {
                    assert_eq!(unclipped(current), unclipped(legacy), "{name} {kind} width={width}");
                }
            }
        }
    }

    #[test]
    fn the_bars_paint_the_frames_they_did_by_hand() {
        let state = editor_state();
        assert_bars("document", &state);

        let mut busy = editor_state();
        busy.code_editor.as_mut().unwrap().saving = true;
        busy.editor_workbench.language_status = "rust-analyzer conectado".into();
        busy.editor_workbench.diagnostics = [Some(1), Some(2), Some(2)]
            .into_iter()
            .map(|severity| openide_language::lsp::Diagnostic {
                range: Default::default(),
                severity,
                code: None,
                source: None,
                message: "diagnóstico".into(),
            })
            .collect();
        assert_bars("saving with problems", &busy);

        let mut empty = crate::view::tests::empty_state();
        empty.ide.mode = crate::ide_workspace::WindowMode::Ide;
        assert_bars("no document", &empty);
        empty.project_branch = Some("feature/footer".into());
        empty.ide_status = IdeStatusView {
            menu: Some(StatusMenu::Branch),
            workspace: Some(empty.workspace.clone()),
            ..Default::default()
        };
        assert_bars("branch menu", &empty);

        let scope = document_scope(&state).unwrap();
        for kind in [
            StatusMenu::Indent,
            StatusMenu::Ending,
            StatusMenu::Language,
            StatusMenu::Overflow,
        ] {
            let mut open = editor_state();
            open.ide_status = IdeStatusView {
                menu: Some(kind),
                scope: Some(scope.clone()),
                workspace: Some(open.workspace.clone()),
                ..Default::default()
            };
            assert_bars(&format!("{kind:?} menu"), &open);
        }
        assert_bars("services form", &services_state());
    }

    #[test]
    fn the_bell_and_one_cell_hover_hint_and_press_as_they_did() {
        let mut state = editor_state();
        let prefix = document_scope(&state).unwrap().prefix();
        crate::icon_button_parity_tests::assert_same(
            "cursor cell",
            control_tooltip(
                "ide_editor_cursor",
                &format!("{prefix}:goto"),
                "Ln 2, Col 3",
                None,
                "Ln 2, Col 3 · Ir a línea y columna",
                label_width("Ln 2, Col 3"),
                theme::WINDOW,
            )
            .shortcut("Ctrl+G")
            .render(),
            super::super::control(
                "ide_editor_cursor",
                &format!("{prefix}:goto"),
                "Ln 2, Col 3",
                None,
                hint("Ln 2, Col 3 · Ir a línea y columna").shortcut("Ctrl+G"),
                label_width("Ln 2, Col 3"),
                theme::WINDOW,
            )
            .render(),
        );
        // The whole footer, so the pointer finds the first cell: the branch.
        for unread in [false, true] {
            if unread {
                state.notifications.push(
                    crate::notifications::Severity::Info,
                    "Listo",
                    "",
                    None,
                );
            }
            crate::icon_button_parity_tests::assert_same(
                "footer",
                footer(&state, 700.0),
                super::super::footer(&state, 700.0),
            );
            for width in WIDTHS.into_iter().filter(|width| *width >= 24.0) {
                assert_eq!(
                    frame(super::super::footer(&state, width), width),
                    frame(footer(&state, width), width),
                    "unread={unread} width={width}"
                );
            }
        }
    }
}
