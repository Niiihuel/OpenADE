//! Integration checks for the real editor group, including its scrolling body.
use super::*;
use crate::document_preview::{PreviewMode, PreviewView};
use openide_ui::{DrawCommand, EditorController, HitKind, Point, UiEvent};

fn state() -> ViewState {
    let mut state = super::super::tests::empty_state();
    state.workspace = "/workspace/openADE".into();
    state.ide.mode = crate::ide_workspace::WindowMode::Agents;
    state.show_side_pane = true;
    state.side_tab = SideTab::FilePreview;
    state.dock_tabs = vec![DockTabView {
        id: 91,
        title: "prueba.md".into(),
        kind: SideTab::FilePreview,
        active: true,
        closable: true,
        full_path: Some("docs/prueba.md".into()),
    }];
    let source = format!(
        "# Documento de prueba\n\n{}\n\n## Objetivos\n\n- Revisar los cambios\n- Conservar el comportamiento del editor\n",
        "Este documento mantiene el desplazamiento horizontal y sus acciones. ".repeat(12),
    );
    let controller = EditorController::new(openide_editor::TextBuffer::new(&source).unwrap());
    state.editor_workbench.document_id = Some(controller.snapshot().unwrap().document_id());
    state.code_editor = Some(crate::code_editor_ui::CodeEditorView {
        tab_id: 91,
        path: "docs/prueba.md".into(),
        controller: Some(controller),
        loading: false,
        saving: false,
        error: None,
        notice: None,
        conflict: false,
        recovered: false,
        missing: false,
    });
    state
}

fn preview(mode: PreviewMode) -> PreviewView {
    PreviewView {
        scope: openide_preview::Scope {
            workspace: "/workspace/openADE".into(),
            path: "docs/prueba.md".into(),
            tab_id: 91,
            generation: 1,
        },
        kind: openide_preview::PreviewKind::Markdown,
        mode,
        document: None,
        raster: None,
        loading: true,
        error: None,
        page: 0,
        pages: 0,
        zoom: None,
    }
}

#[test]
fn dock_document_status_is_one_fixed_row_below_the_entire_editor_viewport() {
    let state = state();
    for width in [320.0, 520.0, 900.0] {
        let size = Size::new(width, 640.0);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
            side_pane_primary_sized(&state, width, size.height),
        );
        let frame = tree.layout(size);
        let editor = frame
            .regions
            .iter()
            .find(|r| r.id == "code_editor:91")
            .unwrap()
            .rect;
        let status = frame
            .regions
            .iter()
            .find(|r| r.id == "ide_editor_diagnostics")
            .unwrap()
            .rect;
        assert_eq!(
            editor.y + editor.height,
            size.height - crate::ide_workspace::STATUSBAR_HEIGHT
        );
        assert_eq!(status.y, editor.y + editor.height + 1.0);
        assert_eq!(status.height, crate::ide_status_ui::STATUS_HEIGHT);
        assert_eq!(status.y + status.height, size.height);
        assert!(!frame.regions.iter().any(|r| r.id == "ide_git_review"));
        assert!(!frame.commands.iter().any(|cmd| matches!(cmd,
            DrawCommand::Text { text, .. } if text == "Lenguaje" || text == "0 problemas"
        )));
        if width == 900.0 {
            assert_eq!(
                frame
                    .commands
                    .iter()
                    .filter(|cmd| matches!(cmd,
                        DrawCommand::Text { text, .. } if text == "Guardado"
                    ))
                    .count(),
                1
            );
        }
        // Horizontal wheel input stays in the editor, not in the status row.
        let controller = state
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .as_ref()
            .unwrap();
        let revision = controller.snapshot().unwrap().revision();
        let horizontal_thumb = |commands: &[DrawCommand]| {
            commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect { rect, .. }
                        if rect.y >= editor.y + editor.height - 8.0
                            && rect.y + rect.height <= editor.y + editor.height
                            && rect.height < 8.0
                            && rect.width > rect.height =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .expect("long lines expose the editor's horizontal scrollbar")
        };
        let thumb_before = horizontal_thumb(&frame.commands);
        tree.handle_event(UiEvent::Scroll2D {
            position: Point::new(editor.x + editor.width / 2.0, editor.y + 70.0),
            delta_x: -180.0,
            delta_y: 0.0,
            direct: true,
            shift: false,
        });
        let scrolled = tree.layout(size);
        assert!(horizontal_thumb(&scrolled.commands).x > thumb_before.x);
        assert_eq!(
            scrolled
                .regions
                .iter()
                .find(|r| r.id == "ide_editor_diagnostics")
                .unwrap()
                .rect,
            status
        );
        assert_eq!(controller.snapshot().unwrap().revision(), revision);
        assert!(
            scrolled
                .regions
                .iter()
                .filter(|r| r.kind == HitKind::Button && r.id.starts_with("ide_editor_"))
                .all(|r| r.rect.y >= status.y)
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn dock_document_status_reserves_space_for_markdown_source_and_split_but_not_preview_only() {
    for mode in [
        PreviewMode::Source,
        PreviewMode::Split,
        PreviewMode::Preview,
    ] {
        let mut state = state();
        state.document_preview = Some(preview(mode));
        for width in [520.0, 900.0] {
            let size = Size::new(width, 640.0);
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                side_pane_primary_sized(&state, width, size.height),
            );
            let frame = tree.layout(size);
            let status = frame
                .regions
                .iter()
                .find(|r| r.id == "ide_editor_diagnostics");
            if mode == PreviewMode::Preview {
                assert!(status.is_none());
            } else {
                let status = status.unwrap().rect;
                assert_eq!(status.y + status.height, size.height);
                let editor = frame
                    .regions
                    .iter()
                    .find(|r| r.id == "code_editor:91")
                    .unwrap()
                    .rect;
                assert!(editor.y + editor.height <= status.y - 1.0);
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn dock_document_status_is_not_duplicated_in_ide() {
    let mut state = state();
    state.ide.mode = crate::ide_workspace::WindowMode::Ide;
    let mut tree = UiTree::new(side_pane_primary_sized(&state, 640.0, 640.0));
    let frame = tree.layout(Size::new(640.0, 640.0));
    assert!(
        !frame
            .regions
            .iter()
            .any(|r| r.id == "ide_editor_diagnostics")
    );
}

#[test]
#[ignore = "native dock editor/footer visual review"]
fn export_dock_document_status_visual_review() {
    let mut state = state();
    for width in [320.0, 520.0, 900.0] {
        export(
            side_pane_primary_sized(&state, width, 460.0),
            width,
            460.0,
            &format!("document-status-{width}"),
        );
        export_with_interaction(
            side_pane_primary_sized(&state, width, 460.0),
            width,
            460.0,
            &format!("document-status-hover-{width}"),
            Some(ExportInteraction::Hover("ide_editor_diagnostics")),
        );
    }
    state.editor_workbench.language_status =
        "Análisis de sintaxis activo · servidor markdown no instalado".into();
    state.ide_status = crate::ide_status_ui::IdeStatusView {
        menu: Some(crate::ide_status_ui::StatusMenu::Services),
        scope: crate::ide_status_ui::document_scope(&state),
        workspace: Some(state.workspace.clone()),
        service_language: Some(openide_ui::CodeLanguage::Markdown),
        ..Default::default()
    };
    for width in [320.0, 900.0] {
        export(
            side_pane_primary_sized(&state, width, 640.0),
            width,
            640.0,
            &format!("document-services-{width}"),
        );
    }
}
