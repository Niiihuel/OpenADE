use super::*;
use crate::ide_workspace::IdeBottomPanel;
use openide_ui::{AlertColors, AlertTone};

const HEADER: f32 = crate::dock_tab_ui::HEADER_HEIGHT;

fn header_tooltip_placement(state: &ViewState) -> TooltipPlacement {
    if state.terminal_maximized {
        TooltipPlacement::Below
    } else {
        TooltipPlacement::Above
    }
}

fn sections(state: &ViewState, width: f32) -> (Element, f32) {
    let compact = width < 640.0;
    let active = if state.show_terminal_panel {
        IdeBottomPanel::Terminal
    } else {
        state.ide.bottom_panel
    };
    let mut items = Vec::new();
    let mut total = 0.0;
    for (panel, key, title, icon, wide) in [
        (
            IdeBottomPanel::Terminal,
            "terminal",
            "Terminal",
            Icon::Terminal,
            68.0,
        ),
        (
            IdeBottomPanel::Problems,
            "problems",
            "Problemas",
            Icon::WarningTriangle,
            82.0,
        ),
        (
            IdeBottomPanel::Output,
            "output",
            "Salida",
            Icon::History,
            56.0,
        ),
        (
            IdeBottomPanel::DebugConsole,
            "console",
            "Consola",
            Icon::Play,
            68.0,
        ),
    ] {
        let id = format!("ide_bottom:{key}");
        let size = if compact { 28.0 } else { wide };
        let ink = if active == panel {
            theme::FOREGROUND
        } else {
            theme::FOREGROUND_SUBTLE
        };
        let content = if compact {
            Element::icon(icon).foreground(ink)
        } else {
            Element::truncated_text(title)
                .foreground(ink)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .center_content()
        }
        .size(None, Some(crate::dock_tab_ui::TOOL_SIZE));
        let item = Tooltip::new(
            crate::dock_tab_ui::tab_surface(&id, content, active == panel, size),
            &id,
            title,
            theme::component_palette(),
        )
        .placement(header_tooltip_placement(state))
        .render();
        total += size;
        items.push(item);
    }
    (
        crate::dock_tab_ui::align_tabs(
            Element::row(items)
                .gap(0.0)
                .size(Some(total), Some(crate::dock_tab_ui::TAB_HEIGHT)),
        )
        .size(Some(total), Some(HEADER)),
        total,
    )
}

fn diagnostic_style(severity: Option<u32>) -> (Icon, openide_ui::Color, &'static str) {
    let (icon, tone, title) = match severity {
        Some(1) => (Icon::ErrorCircle, AlertTone::Error, "Error"),
        Some(2) => (Icon::WarningTriangle, AlertTone::Warning, "Advertencia"),
        Some(3) => (Icon::InfoCircle, AlertTone::Info, "Información"),
        Some(4) => (Icon::InfoCircle, AlertTone::Neutral, "Sugerencia"),
        _ => (Icon::InfoCircle, AlertTone::Neutral, "Diagnóstico"),
    };
    (
        icon,
        AlertColors::for_tone(tone, theme::component_palette()).accent,
        title,
    )
}

fn problem_status(icon: Icon, title: &str, detail: &str, width: f32) -> Element {
    let content = Element::row(vec![
        Element::icon(icon)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(16.0)),
        Element::column(vec![
            Element::truncated_text(title)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
                .size(None, Some(17.0)),
            Element::truncated_text(detail)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(None, Some(16.0)),
        ])
        .gap(2.0)
        .flex(1.0),
    ])
    .gap(8.0)
    .padding(8.0)
    .size(Some(width), Some(51.0));
    Tooltip::new(
        content,
        "ide_problems_status",
        title,
        theme::component_palette(),
    )
    .description(detail)
    .placement(TooltipPlacement::Above)
    .render()
}

fn problems(state: &ViewState, width: f32, height: f32) -> Element {
    let workbench = &state.editor_workbench;
    let editor = state
        .code_editor
        .as_ref()
        .filter(|_| state.side_tab == SideTab::FilePreview);
    let snapshot = editor.and_then(|editor| editor.controller.as_ref()?.snapshot().ok());
    let current = snapshot.as_ref().is_some_and(|snapshot| {
        workbench.diagnostics_current
            && workbench.diagnostic_revision == Some((snapshot.document_id(), snapshot.revision()))
    });
    let row_width = (width - 16.0).max(0.0);
    let mut rows = Vec::new();
    if let Some(editor) = editor {
        let path = std::path::Path::new(&editor.path);
        let file_name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let count = if current {
            workbench.diagnostics.len().to_string()
        } else {
            "—".into()
        };
        let file = Element::row(vec![
            Element::icon(openide_ui::file_icon(&editor.path)).size(Some(14.0), Some(16.0)),
            Element::truncated_text(file_name)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
                .flex(1.0),
            label(count, theme::FOREGROUND_SUBTLE)
                .font_size(11.0)
                .size(None, Some(16.0)),
        ])
        .gap(6.0)
        .padding(5.0)
        .size(Some(row_width), Some(26.0));
        rows.push(
            Tooltip::new(
                file,
                "ide_problems_file",
                &editor.path,
                theme::component_palette(),
            )
            .placement(TooltipPlacement::Above)
            .render(),
        );
        rows.push(
            Element::spacer()
                .background(theme::BORDER)
                .size(Some(row_width), Some(1.0)),
        );
    } else {
        rows.push(problem_status(
            Icon::InfoCircle,
            "No hay un archivo activo",
            "Abrí un archivo para ver sus problemas.",
            row_width,
        ));
    }
    if current && workbench.diagnostics.is_empty() {
        rows.push(problem_status(
            Icon::CheckCircle,
            "Sin problemas en este archivo",
            "Los diagnósticos están actualizados.",
            row_width,
        ));
    } else if editor.is_some() && !current {
        let stale = !workbench.diagnostics.is_empty() || workbench.diagnostic_revision.is_some();
        rows.push(problem_status(
            Icon::History,
            if stale {
                "Diagnósticos desactualizados"
            } else {
                "Esperando diagnósticos"
            },
            if stale {
                "Se actualizarán para la versión actual del archivo."
            } else {
                "El servicio de lenguaje todavía no envió resultados."
            },
            row_width,
        ));
    }
    if let Some(snapshot) = snapshot.filter(|_| current) {
        for (index, diagnostic) in workbench.diagnostics.iter().take(80).enumerate() {
            let id = format!(
                "ide_diagnostic:{}",
                serde_json::to_string(&(
                    &state.workspace,
                    snapshot.document_id(),
                    snapshot.revision(),
                    index
                ))
                .unwrap()
            );
            let position = format!(
                "{}:{}",
                diagnostic.range.start.line.saturating_add(1),
                diagnostic.range.start.character.saturating_add(1)
            );
            let (icon, accent, severity) = diagnostic_style(diagnostic.severity);
            let mut content = vec![
                Element::icon(icon)
                    .foreground(accent)
                    .size(Some(14.0), Some(16.0)),
                Element::truncated_text(diagnostic.message.replace(['\n', '\r'], " "))
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND)
                    .flex(1.0),
            ];
            if width >= 700.0 {
                if let Some(source) = diagnostic
                    .source
                    .as_deref()
                    .filter(|source| !source.is_empty())
                {
                    content.push(
                        Element::truncated_text(source)
                            .font_family(FontFamily::Sans)
                            .font_size(11.0)
                            .foreground(theme::FOREGROUND_SUBTLE)
                            .size(Some(100.0), Some(16.0)),
                    );
                }
            }
            content.push(
                Element::text(&position)
                    .font_family(FontFamily::Monospace)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .size(Some(position.len() as f32 * 7.0), Some(16.0)),
            );
            let metadata = match (&diagnostic.source, &diagnostic.code) {
                (Some(source), Some(code)) => format!("{source} · {code}"),
                (Some(source), None) => source.clone(),
                (None, Some(code)) => code.to_string(),
                (None, None) => String::new(),
            };
            rows.push(
                Tooltip::new(
                    theme::interactive_surface(Element::clickable(
                        &id,
                        Element::row(content).gap(6.0),
                    ))
                    .padding(5.0)
                    .size(Some(row_width), Some(26.0)),
                    &id,
                    format!("{severity}: {}", diagnostic.message),
                    theme::component_palette(),
                )
                .description(format!(
                    "{} · {position}{}",
                    editor.unwrap().path,
                    if metadata.is_empty() {
                        String::new()
                    } else {
                        format!("\n{metadata}")
                    }
                ))
                .placement(TooltipPlacement::Above)
                .render(),
            );
        }
        if workbench.diagnostics.len() > 80 {
            rows.push(
                label(
                    "Se muestran los primeros 80 problemas del archivo",
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0),
            );
        }
    }
    ScrollArea::new("ide_problems", Element::column(rows).padding(8.0))
        .render()
        .size(Some(width), Some(height))
        .clip_children()
}

fn output(state: &ViewState, width: f32, height: f32) -> Element {
    let mut rows = vec![label("Servicios del proyecto", theme::FOREGROUND).font_size(12.0)];
    if !state.editor_workbench.language_status.is_empty() {
        rows.push(
            label(
                &state.editor_workbench.language_status,
                theme::FOREGROUND_SUBTLE,
            )
            .font_size(12.0),
        );
    }
    if let Some(notice) = &state.editor_workbench.notice {
        rows.push(label(notice, theme::FOREGROUND_SUBTLE).font_size(12.0));
    }
    if !state.status.is_empty() {
        rows.push(label(&state.status, theme::FOREGROUND_SUBTLE).font_size(12.0));
    }
    if rows.len() == 1 {
        rows.push(label("Sin mensajes de los servicios", theme::FOREGROUND_SUBTLE).font_size(12.0));
    }
    ScrollArea::new(
        "ide_service_output",
        Element::column(rows).gap(8.0).padding(10.0),
    )
    .render()
    .size(Some(width), Some(height))
    .clip_children()
}

pub(crate) fn build(state: &ViewState, width: f32, height: f32) -> Element {
    let (sections, sections_width) = sections(state, width);
    let closing_terminal = !state.ide.bottom_open
        && state.ide.bottom_panel == IdeBottomPanel::Terminal
        && state
            .bottom_presentation_height
            .is_some_and(|height| height > 0.0);
    if state.show_terminal_panel || closing_terminal {
        return crate::terminal_dock_ui::build_ide(state, width, height, sections, sections_width);
    }
    let header = crate::dock_tab_ui::header(
        sections.flex(1.0),
        tool(
            "ide_bottom_hide",
            "ide_bottom_hide",
            Icon::Close,
            "Ocultar panel",
            false,
            crate::dock_tab_ui::TOOL_SIZE,
        )
        .tooltip_placement(header_tooltip_placement(state))
        .size(
            Some(crate::dock_tab_ui::TOOL_SIZE),
            Some(crate::dock_tab_ui::TOOL_SIZE),
        ),
        width,
        crate::dock_tab_ui::HEADER_SIDE_INSET,
        8.0,
    );
    let viewport_height = height.max(0.0);
    let content_height = viewport_height.max(HEADER);
    let body_height = content_height - HEADER;
    let body = match state.ide.bottom_panel {
        IdeBottomPanel::Problems => problems(state, width, body_height),
        IdeBottomPanel::Output => output(state, width, body_height),
        IdeBottomPanel::DebugConsole => {
            crate::debug_ui::build_console(&state.debug, width, body_height)
        }
        IdeBottomPanel::Terminal => Element::spacer().size(Some(width), Some(body_height)),
    };
    Element::column(vec![
        Element::column(vec![header, body]).size(Some(width), Some(content_height))
    ])
    .background(theme::BACKGROUND)
    .size(Some(width), Some(viewport_height))
    .clip_children()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;
    use openide_ui::{DrawCommand, Frame, Point, Rect, Size, UiAction, UiEvent, UiTree};

    fn region(frame: &Frame, id: &str) -> Rect {
        frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap()
            .rect
    }

    fn diagnostic_fixture() -> ViewState {
        let mut state = crate::view::ide_visual_tests::fixture();
        state.ide.mode = crate::ide_workspace::WindowMode::Agents;
        state.ide.bottom_open = true;
        state.ide.bottom_panel = IdeBottomPanel::Problems;
        state.show_terminal_panel = false;
        let snapshot = state
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap();
        state.editor_workbench.diagnostic_revision =
            Some((snapshot.document_id(), snapshot.revision()));
        state.editor_workbench.diagnostics_current = true;
        state.editor_workbench.diagnostics = [Some(1), Some(2), Some(3), Some(4), None]
            .into_iter()
            .enumerate()
            .map(|(index, severity)| openide_language::lsp::Diagnostic {
                range: openide_language::Range {
                    start: openide_language::Position {
                        line: index as u32,
                        character: 2,
                    },
                    end: openide_language::Position {
                        line: index as u32,
                        character: 4,
                    },
                },
                severity,
                code: Some(serde_json::json!("E0308")),
                source: Some("rust-analyzer".into()),
                message: [
                    "Tipos incompatibles: se esperaba String y se encontró &str",
                    "La variable «resultado» todavía no se utiliza",
                    "El análisis está limitado a la configuración activa",
                    "Esta expresión se puede simplificar",
                    "El servidor envió un diagnóstico sin severidad",
                ][index]
                    .into(),
            })
            .collect();
        state
    }

    #[test]
    fn agents_problems_share_the_compact_bottom_strip_and_current_diagnostic_actions() {
        for width in [320.0, 700.0, 1100.0] {
            let state = diagnostic_fixture();
            let expected = state.editor_workbench.diagnostic_revision.unwrap();
            let viewport = Size::new(width, 260.0);
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(build(&state, width, viewport.height));
            let frame = tree.layout(viewport);
            assert_eq!(
                region(&frame, "ide_problems"),
                Rect::new(0.0, HEADER, width, 228.0)
            );
            for key in ["terminal", "problems", "output", "console"] {
                let rect = region(&frame, &format!("ide_bottom:{key}"));
                assert_eq!(rect.y + rect.height, HEADER);
            }
            let diagnostics = frame
                .regions
                .iter()
                .filter(|region| {
                    region.kind == openide_ui::HitKind::Button
                        && region.id.starts_with("ide_diagnostic:")
                })
                .collect::<Vec<_>>();
            assert_eq!(diagnostics.len(), 5);
            for (index, target) in diagnostics.into_iter().enumerate() {
                assert_eq!(target.rect.height, 26.0);
                assert!(target.rect.x >= 8.0 && target.rect.x + target.rect.width <= width - 8.0);
                assert!(
                    target.rect.y >= HEADER
                        && target.rect.y + target.rect.height <= viewport.height
                );
                let (workspace, document, revision, diagnostic): (String, u64, u64, usize) =
                    serde_json::from_str(target.id.strip_prefix("ide_diagnostic:").unwrap())
                        .unwrap();
                assert_eq!(workspace, state.workspace);
                assert_eq!(
                    (document, revision, diagnostic),
                    (expected.0, expected.1, index)
                );
                let (icon, color, _) =
                    diagnostic_style(state.editor_workbench.diagnostics[index].severity);
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Icon { rect, icon: actual, tint: actual_color, .. }
                    if *actual == icon && *actual_color == color && target.rect.contains(Point::new(rect.x, rect.y))
                )));
                let point = Point::new(
                    target.rect.x + target.rect.width - 15.0,
                    target.rect.y + 13.0,
                );
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked {
                        id: target.id.clone()
                    }]
                );
            }
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "workspace.rs"
            )));
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn pending_stale_and_empty_problems_never_offer_outdated_navigation() {
        for (name, expected) in [
            ("pending", "Esperando diagnósticos"),
            ("stale", "Diagnósticos desactualizados"),
            ("empty", "Sin problemas en este archivo"),
            ("missing", "No hay un archivo activo"),
        ] {
            let mut state = diagnostic_fixture();
            match name {
                "pending" => {
                    state.editor_workbench.diagnostics.clear();
                    state.editor_workbench.diagnostic_revision = None;
                    state.editor_workbench.diagnostics_current = false;
                }
                "stale" => state.editor_workbench.diagnostics_current = false,
                "empty" => state.editor_workbench.diagnostics.clear(),
                _ => state.code_editor = None,
            }
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(build(&state, 320.0, 180.0));
            let frame = tree.layout(Size::new(320.0, 180.0));
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("ide_diagnostic:")),
                "{name}"
            );
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == expected
                )),
                "{name} must explain the actual state"
            );
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text.contains("Tipos incompatibles")
            )));
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    #[ignore = "exports native shared Problems dock at narrow and wide widths"]
    fn export_shared_problems_visual_review() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/shared-problems-visual-qa");
        std::fs::create_dir_all(&directory).unwrap();
        for width in [320.0, 700.0, 1100.0] {
            for variant in ["current", "pending", "stale", "empty"] {
                let mut state = diagnostic_fixture();
                match variant {
                    "pending" => {
                        state.editor_workbench.diagnostics.clear();
                        state.editor_workbench.diagnostic_revision = None;
                        state.editor_workbench.diagnostics_current = false;
                    }
                    "stale" => state.editor_workbench.diagnostics_current = false,
                    "empty" => state.editor_workbench.diagnostics.clear(),
                    _ => {}
                }
                let size = Size::new(width, 260.0);
                let (context, mut tree) =
                    crate::view::ide_visual_tests::native_tree(build(&state, width, size.height));
                let frame = tree.layout(size);
                let path = directory.join(format!("problems-{variant}-{width:.0}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(
                    &frame,
                    &path,
                    width,
                    size.height,
                )
                .unwrap();
                let svg = resvg::usvg::Tree::from_data(
                    &std::fs::read(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixels =
                    resvg::tiny_skia::Pixmap::new(width as u32, size.height as u32).unwrap();
                resvg::render(
                    &svg,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixels.as_mut(),
                );
                pixels.save_png(path.with_extension("png")).unwrap();
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn closing_bottom_retains_the_selected_surface_and_clips_below_header_height() {
        for panel in [
            IdeBottomPanel::Terminal,
            IdeBottomPanel::Problems,
            IdeBottomPanel::Output,
            IdeBottomPanel::DebugConsole,
        ] {
            let mut state = crate::view::ide_visual_tests::fixture();
            state.show_terminal_panel = false;
            state.ide.bottom_open = false;
            state.ide.bottom_panel = panel;
            state.debug.output = vec![crate::debug_ui::DebugOutputView {
                category: "stdout".into(),
                text: "Retained debugger output".into(),
            }]
            .into();
            let body_id = match panel {
                IdeBottomPanel::Terminal => "terminal_screen".to_owned(),
                IdeBottomPanel::Problems => "ide_problems".to_owned(),
                IdeBottomPanel::Output => "ide_service_output".to_owned(),
                IdeBottomPanel::DebugConsole => format!("debug_console:{}", state.debug.scope),
            };
            for height in [160.0, 32.0, 31.0, 12.0, 1.0, 0.0] {
                state.bottom_presentation_height = Some(height);
                let width = 640.0;
                let mut tree = UiTree::new(Element::column(vec![
                    build(&state, width, height),
                    Element::clickable("after_bottom", Element::spacer())
                        .size(Some(width), Some(20.0)),
                ]));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, height + 20.0));
                assert_eq!(
                    region(&frame, "after_bottom"),
                    Rect::new(0.0, height, width, 20.0)
                );
                for target in frame
                    .regions
                    .iter()
                    .filter(|target| target.id != "after_bottom")
                {
                    assert!(
                        target.rect.y >= 0.0 && target.rect.y + target.rect.height <= height,
                        "{panel:?} at {height}px: {} escaped presentation bounds",
                        target.id
                    );
                }
                assert_eq!(
                    frame.hit_test(Point::new(width - 8.0, height + 1.0)),
                    Some("after_bottom")
                );
                if height > HEADER {
                    assert_eq!(
                        region(&frame, &body_id),
                        Rect::new(0.0, HEADER, width, height - HEADER)
                    );
                    assert_eq!(
                        frame
                            .regions
                            .iter()
                            .any(|target| target.id == "terminal_new"),
                        panel == IdeBottomPanel::Terminal
                    );
                }
            }
        }
    }

    #[test]
    fn switching_from_terminal_to_another_open_bottom_surface_does_not_retain_terminal_content() {
        let mut state = crate::view::ide_visual_tests::fixture();
        state.show_terminal_panel = false;
        state.ide.bottom_open = true;
        state.ide.bottom_panel = IdeBottomPanel::Problems;
        state.bottom_presentation_height = Some(160.0);
        let frame = UiTree::new(build(&state, 640.0, 160.0)).layout(Size::new(640.0, 160.0));
        assert_eq!(
            region(&frame, "ide_problems"),
            Rect::new(0.0, HEADER, 640.0, 160.0 - HEADER)
        );
        assert!(!frame
            .regions
            .iter()
            .any(|target| target.id == "terminal_screen" || target.id == "terminal_new"));
    }

    #[test]
    fn bottom_section_labels_and_icons_remain_centered_on_hover() {
        let tabs = [
            (
                IdeBottomPanel::Terminal,
                "terminal",
                "Terminal",
                Icon::Terminal,
            ),
            (
                IdeBottomPanel::Problems,
                "problems",
                "Problemas",
                Icon::WarningTriangle,
            ),
            (IdeBottomPanel::Output, "output", "Salida", Icon::History),
            (
                IdeBottomPanel::DebugConsole,
                "console",
                "Consola",
                Icon::Play,
            ),
        ];
        for width in [272.0, 320.0, 520.0, 640.0, 1000.0] {
            let editor_state = crate::view::ide_visual_tests::fixture();
            let (editor_context, mut editor_tree) = crate::view::ide_visual_tests::native_tree(
                crate::view::dock_chrome::header(&editor_state, width),
            );
            let editor_frame = editor_tree.layout(Size::new(width, HEADER));
            let first_editor_tab = region(&editor_frame, "dock_tab:71");
            let editor_menu = region(&editor_frame, "editor_menu");
            editor_context.end_pass().textures_delta.clear();
            let mut terminal_targets = None;
            for (active, _, _, _) in tabs {
                let mut state = crate::view::ide_visual_tests::fixture();
                state.show_terminal_panel = active == IdeBottomPanel::Terminal;
                state.ide.bottom_panel = active;
                state.debug.output = vec![crate::debug_ui::DebugOutputView {
                    category: "stdout".into(),
                    text: "Salida del adaptador".into(),
                }]
                .into();
                let size = Size::new(width, 220.0);
                let (context, mut tree) =
                    crate::view::ide_visual_tests::native_tree(build(&state, width, size.height));
                let initial = tree.layout(size);
                let close_id = if state.show_terminal_panel {
                    "terminal_panel_hide"
                } else {
                    "ide_bottom_hide"
                };
                let mut targets: Vec<_> = tabs
                    .iter()
                    .map(|(_, key, _, _)| region(&initial, &format!("ide_bottom:{key}")))
                    .collect();
                let close = region(&initial, close_id);
                targets.push(close);
                assert_eq!(targets[0].x, first_editor_tab.x);
                assert_eq!(targets[0].y, first_editor_tab.y);
                assert_eq!(targets[0].height, first_editor_tab.height);
                for adjacent in targets[..4].windows(2) {
                    assert_eq!(adjacent[0].x + adjacent[0].width, adjacent[1].x);
                }
                assert_eq!(close.x + close.width, editor_menu.x + editor_menu.width);
                assert_eq!(close.y + close.height / 2.0, HEADER / 2.0);
                if let Some(expected) = &terminal_targets {
                    assert_eq!(
                        &targets, expected,
                        "section and close targets must not jump when switching to {active:?}"
                    );
                } else {
                    terminal_targets = Some(targets);
                }
                let body_id = match active {
                    IdeBottomPanel::Terminal => "terminal_screen".to_owned(),
                    IdeBottomPanel::Problems => "ide_problems".to_owned(),
                    IdeBottomPanel::Output => "ide_service_output".to_owned(),
                    IdeBottomPanel::DebugConsole => format!("debug_console:{}", state.debug.scope),
                };
                let body = region(&initial, &body_id);
                assert_eq!(
                    body.y, HEADER,
                    "{active:?}: body starts directly below the header"
                );
                assert_eq!(body.height, size.height - HEADER);
                for (panel, key, title, icon) in tabs {
                    let id = format!("ide_bottom:{key}");
                    let target = region(&initial, &id);
                    assert_eq!(target.height, crate::dock_tab_ui::TAB_HEIGHT);
                    assert_eq!(target.y + target.height, body.y);
                    let point = Point::new(
                        target.x + target.width / 2.0,
                        target.y + target.height / 2.0,
                    );
                    tree.handle_event(UiEvent::PointerMove(point));
                    let hovered = tree.layout(size);
                    assert_eq!(region(&hovered, &id), target, "hover must not move {title}");
                    for frame in [&initial, &hovered] {
                        if width < 640.0 {
                            let glyph = frame
                                .commands
                                .iter()
                                .find_map(|command| match command {
                                    DrawCommand::Icon {
                                        rect,
                                        icon: candidate,
                                        ..
                                    } if *candidate == icon
                                        && target.contains(Point::new(rect.x, rect.y)) =>
                                    {
                                        Some(*rect)
                                    }
                                    _ => None,
                                })
                                .unwrap();
                            assert!((glyph.x + glyph.width / 2.0 - point.x).abs() < 0.1);
                            assert!((glyph.y + glyph.height / 2.0 - HEADER / 2.0).abs() < 0.1);
                        } else {
                            let (position, font_size) = frame
                                .commands
                                .iter()
                                .find_map(|command| match command {
                                    DrawCommand::Text {
                                        text,
                                        position,
                                        font_size,
                                        ..
                                    } if text == title => Some((*position, *font_size)),
                                    _ => None,
                                })
                                .unwrap();
                            assert_eq!(font_size, 13.0);
                            let (text_width, line_height) = context.fonts_mut(|fonts| {
                                let text_width = fonts
                                    .layout_no_wrap(
                                        title.into(),
                                        egui::FontId::proportional(font_size),
                                        egui::Color32::WHITE,
                                    )
                                    .size()
                                    .x;
                                let line_height =
                                    fonts.row_height(&egui::FontId::proportional(14.0)) * font_size
                                        / 14.0;
                                (text_width, line_height)
                            });
                            assert!(
                                position.x >= target.x
                                    && position.x + text_width <= target.x + target.width + 0.1,
                                "{title}: complete label must fit its tab"
                            );
                            assert!(
                                (position.x + text_width / 2.0 - point.x).abs() < 0.1,
                                "{title}: horizontal center"
                            );
                            assert!(
                                (position.y + line_height / 2.0 - HEADER / 2.0).abs() < 0.1,
                                "{title}: vertical center"
                            );
                        }
                    }
                    if width >= 640.0 {
                        let hover_fill = if active == panel {
                            theme::BACKGROUND
                        } else {
                            theme::HOVER
                        };
                        assert!(hovered.commands.iter().any(|command| matches!(command,
                            DrawCommand::FillCornerRect { rect, color, .. }
                                if *color == hover_fill && rect.contains(point)
                                    && rect.x >= target.x && rect.y >= target.y
                                    && rect.x + rect.width <= target.x + target.width + 0.1
                                    && rect.y + rect.height <= target.y + target.height + 0.1
                        )));
                    }
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        vec![UiAction::Clicked { id }]
                    );
                }
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    #[ignore = "exports native editor and bottom-tab alignment PNG/SVG fixtures"]
    fn export_bottom_tab_alignment_visual_review() {
        let directory = std::path::Path::new("/tmp/openade-bottom-tab-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [320.0, 520.0, 1000.0] {
            for (panel, name) in [
                (IdeBottomPanel::Terminal, "terminal"),
                (IdeBottomPanel::Problems, "problems"),
                (IdeBottomPanel::Output, "output"),
                (IdeBottomPanel::DebugConsole, "console"),
            ] {
                let mut state = crate::view::ide_visual_tests::fixture();
                state.show_terminal_panel = panel == IdeBottomPanel::Terminal;
                state.ide.bottom_panel = panel;
                state.debug.output = vec![crate::debug_ui::DebugOutputView {
                    category: "stdout".into(),
                    text: "Salida del adaptador de depuración".into(),
                }]
                .into();
                let size = Size::new(width, 300.0);
                let root = Element::column(vec![
                    crate::view::dock_chrome::header(&state, width),
                    Element::spacer().size(None, Some(48.0)),
                    build(&state, width, 220.0),
                ])
                .background(theme::BACKGROUND);
                let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
                let frame = tree.layout(size);
                assert_eq!(
                    region(&frame, "dock_tab:71").x,
                    region(&frame, "ide_bottom:terminal").x,
                );
                let path = directory.join(format!("editor-bottom-{name}-{width:.0}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(
                    &frame,
                    &path,
                    size.width,
                    size.height,
                )
                .unwrap();
                let svg = resvg::usvg::Tree::from_str(
                    &std::fs::read_to_string(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixels = resvg::tiny_skia::Pixmap::new(width as u32, 300).unwrap();
                resvg::render(
                    &svg,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixels.as_mut(),
                );
                pixels.save_png(path.with_extension("png")).unwrap();
                context.end_pass().textures_delta.clear();
            }
        }
    }
}
