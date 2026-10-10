//! Immutable chat evidence, deliberately separate from editable disk documents.
use crate::{code_editor_ui, theme};
use openide_ui::{
    CodeDiff, CodeDiffLine, CodeEditor, CodeLanguage, Component, EditorController, Element,
    FontFamily,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) enum CodeSnapshotContent {
    /// Complete recorded write input; never a claim about current disk contents.
    Text {
        controller: EditorController,
        language: CodeLanguage,
    },
    /// Read ranges, patches and provisional input retain their recorded numbering.
    Diff {
        lines: Arc<[CodeDiffLine]>,
        language: CodeLanguage,
        single_gutter: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct CodeSnapshotView {
    pub tab_id: u64,
    pub source: String,
    pub title: String,
    pub path: String,
    pub caption: String,
    pub review: Option<crate::editor_review_ui::EditorReviewView>,
    pub content: CodeSnapshotContent,
}

pub(crate) fn view_id(tab_id: u64) -> String {
    format!("code_snapshot:{tab_id}")
}

pub(crate) fn build(view: &CodeSnapshotView, width: f32) -> Element {
    let available = (width - 12.0).max(0.0);
    let inline_review = view.review.as_ref();
    let budget = crate::editor_review_ui::controls_budget((available - 4.0).max(0.0));
    let review_width = inline_review.map_or(0.0, |review| {
        crate::editor_review_ui::width(review, budget) + 4.0
    });
    let header_height = if inline_review.is_some() {
        crate::editor_review_ui::HEIGHT
    } else {
        code_editor_ui::BREADCRUMB_HEIGHT
    };
    let mut top = vec![
        Element::spacer().size(Some(6.0), None),
        code_editor_ui::path_breadcrumb_with_height(
            &format!("code_snapshot_path:{}", view.tab_id),
            &view.path,
            false,
            (available - review_width).max(0.0),
            header_height,
        )
        .flex(1.0),
    ];
    if let Some(review) = inline_review {
        top.push(Element::spacer().size(Some(4.0), None));
        top.push(crate::editor_review_ui::controls(review, budget));
    }
    top.push(Element::spacer().size(Some(6.0), None));
    let header = Element::row(top).size(None, Some(header_height));
    let caption = Element::text(&view.caption)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(theme::FOREGROUND_SUBTLE)
        .padding(8.0);
    let content = match &view.content {
        CodeSnapshotContent::Text {
            controller,
            language,
        } => CodeEditor::new(view_id(view.tab_id), controller.clone())
            .read_only(true)
            .status_bar(true)
            .language(*language)
            .palette(theme::component_palette())
            .render(),
        CodeSnapshotContent::Diff {
            lines,
            language,
            single_gutter,
        } => CodeDiff::new(view_id(view.tab_id), lines.clone())
            .focusable(true)
            .single_gutter(*single_gutter)
            .language(*language)
            .palette(theme::component_palette())
            .render(),
    };
    Element::column(vec![
        header,
        caption,
        content.top_fade(theme::BACKGROUND, 12.0).flex(1.0),
    ])
    .size(Some(width.max(0.0)), None)
    .flex(1.0)
    .clip_children()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_editor::TextBuffer;
    use openide_ui::{Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn recorded_write_is_copyable_but_never_editable_or_saveable_from_the_snapshot_view() {
        let controller = EditorController::new(TextBuffer::new("let café = 1;\r\n").unwrap());
        let view = CodeSnapshotView {
            tab_id: 7,
            source: "owner:call:0".into(),
            title: "main.rs".into(),
            path: "src/main.rs".into(),
            caption: "Contenido enviado · solo lectura · no es el archivo en disco".into(),
            review: None,
            content: CodeSnapshotContent::Text {
                controller: controller.clone(),
                language: CodeLanguage::Rust,
            },
        };
        for width in [350.0, 900.0] {
            let mut tree = UiTree::new(build(&view, width));
            let frame = tree.layout(Size::new(width, 400.0));
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|hit| hit.id.starts_with("code_editor_save:"))
            );
            tree.focus(&view_id(7));
            let _ = tree.handle_event(UiEvent::Key {
                key: Key::A,
                shift: false,
                ctrl: true,
            });
            let copy = tree.handle_event(UiEvent::Key {
                key: Key::C,
                shift: false,
                ctrl: true,
            });
            assert!(copy.iter().any(|action| matches!(action,
                UiAction::CopyRequested { text } if text == "let café = 1;\r\n"
            )));
            let _ = tree.handle_event(UiEvent::Text("overwrite".into()));
            let _ = tree.handle_event(UiEvent::Paste("overwrite".into()));
            let _ = tree.handle_event(UiEvent::Key {
                key: Key::Backspace,
                shift: false,
                ctrl: false,
            });
            let snapshot = controller.snapshot().unwrap();
            assert_eq!(snapshot.revision(), 0);
            assert_eq!(
                snapshot.text_range(0..snapshot.char_count()).unwrap(),
                "let café = 1;\r\n"
            );
        }
    }

    #[test]
    fn deleted_file_review_keeps_recorded_diff_and_scoped_decisions_without_navigation() {
        use openide_ui::{CodeDiffLineKind, DrawCommand, HitKind};
        let token = "deleted-file-review";
        let view = CodeSnapshotView {
            tab_id: 9,
            source: "session-1:call-3:0".into(),
            title: "old.rs".into(),
            path: "src/old.rs".into(),
            caption: "Archivo eliminado · cambios pendientes".into(),
            review: Some(crate::editor_review_ui::EditorReviewView {
                token: token.into(),
                deletions: 1,
                can_accept: true,
                can_reject: true,
                ..Default::default()
            }),
            content: CodeSnapshotContent::Diff {
                lines: vec![CodeDiffLine {
                    kind: CodeDiffLineKind::Removed,
                    old_line: Some(1),
                    new_line: None,
                    text: "fn old() {}".into(),
                }]
                .into(),
                language: CodeLanguage::Rust,
                single_gutter: false,
            },
        };
        for width in [240.0, 360.0, 460.0, 600.0, 900.0] {
            let mut tree = UiTree::new(build(&view, width));
            let frame = tree.layout(Size::new(width, 400.0));
            let painted = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            assert!(
                painted.contains("fn old() {}"),
                "syntax spans retain all recorded source"
            );
            for operation in ["previous", "next"] {
                assert!(!frame.regions.iter().any(|hit| hit.id
                    == crate::editor_review_ui::action(token, operation)
                    && hit.kind == HitKind::Button));
            }
            let breadcrumb = frame
                .regions
                .iter()
                .find(|region| region.id == "code_snapshot_path:9:item:1")
                .expect("the deleted file retains its path tooltip")
                .rect;
            assert_eq!(breadcrumb.height, crate::editor_review_ui::HEIGHT);
            for (operation, label) in [("reject_file", "Deshacer"), ("accept_file", "Conservar")] {
                assert!(
                    painted.contains(label),
                    "deleted-file decisions use complete text labels"
                );
                let id = crate::editor_review_ui::action(token, operation);
                let decision = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .expect("the file decision remains independently actionable")
                    .rect;
                assert!(decision.x >= 0.0 && decision.x + decision.width <= width + 0.01);
                assert!(
                    (decision.y + decision.height / 2.0 - breadcrumb.y - breadcrumb.height / 2.0)
                        .abs()
                        < 0.01,
                    "snapshot breadcrumb and review decisions always share one row"
                );
                assert!(tree.focus(&id));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        ctrl: false,
                        shift: false
                    }),
                    vec![UiAction::Clicked { id }]
                );
            }
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|hit| hit.id == view_id(view.tab_id))
            );
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|hit| hit.id.starts_with("code_editor_save:"))
            );
        }
    }
}
