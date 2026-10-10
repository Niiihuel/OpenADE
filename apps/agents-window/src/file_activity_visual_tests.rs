//! File activity uses the real native tree, bundled fonts and scoped actions.
//! Fixtures describe recorded observations; no tools or network are executed.
use super::{build_session_surface, tests::state};
use crate::{
    chat_diff_ui::{DiffFileView, ToolDiffEntry},
    transcript::{ActivityStatus, ModelStreamStatus, TranscriptItem, TranscriptProjection},
    view::ViewState,
};
use eframe::egui;
use openide_core::{AgentEventKind, AgentMessage, ModelStreamEvent, ToolCall};
use openide_ui::{
    CodeDiffLine, CodeDiffLineKind, DrawCommand, Element, FontFamily, FontMetrics, Frame, HitKind,
    Key, Point, Rect, Size, UiAction, UiEvent, UiTree,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const OWNER: &str = "file-activity-fixture";
const CALL: &str = "write:saludo";
const DRAFT: &str = "Conservá mi borrador.\nRevisar antes de enviar.";
const BEFORE: &str = "Primero reviso el archivo y preparo una mejora pequeña.";
const AFTER: &str = "La actualización terminó. El resto del proyecto se conservó.";
const CODE: &str = "fn saludo() {\n    println!(\"Hola 🦀\");\n}\n";
const VIEWPORTS: [Size; 3] = [
    Size::new(360.0, 900.0),
    Size::new(900.0, 1000.0),
    Size::new(1700.0, 1000.0),
];

fn fixture() -> ViewState {
    let mut state = state();
    state.workspace = "/projects/openADE".into();
    state.selected_session = Some(OWNER.into());
    state.selected_session_title = Some("Cambios del proyecto".into());
    state.prompt = DRAFT.into();
    state.native_key_available = true;
    let mut route = crate::composer_ui::tests::configured_effort_route();
    route.execution = crate::composer_selection::ExecutionSupport::Verified;
    state.native_provider_id = route.provider_id.clone();
    state.model = route.model_id.clone();
    state.composer_model = route.model_id.clone();
    state.composer_effort_options.clear();
    route.effort_options.clear();
    state.composer_routes = vec![route];
    state.show_side_pane = false;
    state.transcript.clear();
    state.conversation_details.sync(Some(OWNER));
    state.chat_diffs.workspace = Some(PathBuf::from(&state.workspace));
    state.chat_diffs.session_id = Some(OWNER.into());
    state
}

fn root(state: &ViewState, size: Size) -> Element {
    Element::panel(build_session_surface(state, size.width, false, false))
        .background(crate::theme::BACKGROUND)
}

pub(super) fn native_tree(root: Element) -> (egui::Context, UiTree) {
    native_tree_at_scale(root, 1.0)
}

fn native_tree_at_scale(root: Element, pixels_per_point: f32) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.set_pixels_per_point(pixels_per_point);
    context.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    let height_context = context.clone();
    tree.set_text_height_measurer(move |size, family| {
        height_context.fonts_mut(|fonts| {
            fonts.row_height(&egui::FontId::new(
                size,
                match family {
                    FontFamily::Sans => egui::FontFamily::Proportional,
                    FontFamily::Monospace => egui::FontFamily::Monospace,
                },
            ))
        })
    });
    for family in [FontFamily::Monospace, FontFamily::Sans] {
        let font = match family {
            FontFamily::Monospace => egui::FontId::monospace(14.0),
            FontFamily::Sans => egui::FontId::proportional(14.0),
        };
        let metrics = context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts
                .glyph_width(&font, if family == FontFamily::Sans { 'a' } else { 'M' }),
            line_height: fonts.row_height(&font),
        });
        match family {
            FontFamily::Monospace => tree.set_metrics(metrics),
            FontFamily::Sans => tree.set_sans_metrics(metrics),
        }
    }
    let measure = context.clone();
    tree.set_text_measurer(move |text, size, family| {
        measure.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    (context, tree)
}

fn text(frame: &Frame) -> String {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind != HitKind::Hover)
        .unwrap_or_else(|| panic!("missing file activity control {id}"))
        .rect
}

fn center(rect: Rect) -> Point {
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}
fn disclosure(call: &str) -> String {
    format!("conversation_detail:{OWNER}:tool:{call}")
}

fn assert_one_disclosure(frame: &Frame, call: &str) {
    assert_eq!(
        frame
            .regions
            .iter()
            .filter(|region| region.kind == HitKind::Button && region.id == disclosure(call))
            .count(),
        1
    );
    let visible = text(frame);
    assert!(
        !visible.contains("Cambios del archivo"),
        "duplicate diff disclosure: {visible}"
    );
    assert!(
        !visible.contains("\"path\""),
        "raw JSON must stay in the inspector: {visible}"
    );
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id.starts_with("chat_diff_toggle:")));
}

fn code_regions(frame: &Frame) -> Vec<Rect> {
    frame
        .regions
        .iter()
        .filter(|region| region.kind == HitKind::CodeDiff)
        .map(|region| region.rect)
        .collect()
}

fn assert_stable_composer(before: &Frame, after: &Frame) {
    for id in [
        "transcript",
        "prompt",
        "run_toggle",
        "composer_add_context",
        "composer_model_toggle",
    ] {
        assert_eq!(
            rect(before, id),
            rect(after, id),
            "{id} must not move with file activity"
        );
    }
}

fn projected_start() -> TranscriptProjection {
    let mut projection = TranscriptProjection::new();
    projection.apply_kind(&AgentEventKind::TurnStarted {
        prompt: "Actualizá el saludo".into(),
    });
    projection.apply_kind(&AgentEventKind::ModelRequestStarted {
        step: 1,
        model: "fixture".into(),
    });
    projection.apply_kind(&AgentEventKind::AssistantDelta {
        content: format!("**Revisión**\n\n{BEFORE}"),
    });
    projection
}

fn delta(projection: &mut TranscriptProjection, fragment: &str) {
    projection.apply_kind(&AgentEventKind::ModelStreamUpdate {
        step: 1,
        update: ModelStreamEvent::ToolCallDelta {
            item_id: "file-input".into(),
            call_id: Some(CALL.into()),
            name: Some("write_file".into()),
            arguments_delta: fragment.into(),
        },
    });
}

fn write_tool(status: ActivityStatus, error: bool) -> TranscriptItem {
    TranscriptItem::Tool {
        id: CALL.into(),
        name: "write_file".into(),
        arguments: json!({"path":"src/saludo.rs", "content":CODE}),
        output: match status {
            ActivityStatus::Completed => Some("file written".into()),
            ActivityStatus::Failed => Some("No se pudo escribir: permiso denegado".into()),
            _ => None,
        },
        is_error: error,
        activity: status,
    }
}

fn confirmed_diff(state: &mut ViewState, call: &str) {
    state.chat_diffs.entries.insert(
        call.into(),
        ToolDiffEntry {
            files: vec![DiffFileView {
                path: "src/saludo.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 1,
                truncated: false,
                unavailable: None,
                can_open: true,
                lines: Arc::from([
                    CodeDiffLine {
                        kind: CodeDiffLineKind::Context,
                        old_line: Some(1),
                        new_line: Some(1),
                        text: "fn saludo() {".into(),
                    },
                    CodeDiffLine {
                        kind: CodeDiffLineKind::Removed,
                        old_line: Some(2),
                        new_line: None,
                        text: "    println!(\"Hola\");".into(),
                    },
                    CodeDiffLine {
                        kind: CodeDiffLineKind::Added,
                        old_line: None,
                        new_line: Some(2),
                        text: "    println!(\"Hola 🦀\");".into(),
                    },
                    CodeDiffLine {
                        kind: CodeDiffLineKind::Context,
                        old_line: Some(3),
                        new_line: Some(3),
                        text: "}".into(),
                    },
                ]),
            }],
            unavailable: None,
            notes: vec![],
            limited: false,
        },
    );
}

const PARTIAL_DIFF_NOTE: &str =
    "Cobertura limitada: algunos archivos de esta edición no tienen un diff completo";
const CONFIRMED_SNAPSHOT_NOTE: &str = "Contenido escrito confirmado · versión anterior no disponible. Dato no disponible: inventario inicial incompleto";
const COMPACT_SNAPSHOT_NOTE: &str = "Contenido guardado · sin versión anterior para comparar.";

fn confirmed_write_fixture(before_known: bool) -> ViewState {
    let mut state = fixture();
    state
        .transcript
        .push(write_tool(ActivityStatus::Completed, false));
    state.chat_diffs.entries.insert(
        CALL.into(),
        ToolDiffEntry {
            files: vec![DiffFileView {
                path: "src/saludo.rs".into(),
                old_path: None,
                additions: if before_known {
                    CODE.lines().count()
                } else {
                    0
                },
                deletions: 0,
                truncated: false,
                unavailable: (!before_known).then(|| CONFIRMED_SNAPSHOT_NOTE.into()),
                can_open: true,
                lines: CODE
                    .lines()
                    .enumerate()
                    .map(|(index, content)| CodeDiffLine {
                        kind: if before_known {
                            CodeDiffLineKind::Added
                        } else {
                            CodeDiffLineKind::Context
                        },
                        old_line: None,
                        new_line: Some(index + 1),
                        text: content.into(),
                    })
                    .collect::<Vec<_>>()
                    .into(),
            }],
            unavailable: None,
            notes: if before_known {
                vec![]
            } else {
                vec![PARTIAL_DIFF_NOTE.into()]
            },
            limited: !before_known,
        },
    );
    state
}

#[test]
fn confirmed_write_has_one_header_and_evidence_details_in_its_tooltip() {
    for width in [240.0, 360.0, 900.0] {
        for before_known in [true, false] {
            let state = confirmed_write_fixture(before_known);
            let card =
                crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
                    .unwrap();
            let (context, mut tree) = native_tree(Element::column(vec![card]).padding(12.0));
            let frame = tree.layout(Size::new(width, 700.0));
            let header = rect(&frame, &disclosure(CALL));
            let code = rect(&frame, &crate::chat_diff_ui::view_id(OWNER, CALL, 0));
            assert!(code.x >= header.x + 8.0);
            assert!(code.x + code.width <= header.x + header.width - 8.0 + 0.01);
            assert!(code.y >= header.y + header.height + 8.0);
            let footer = rect(&frame, &format!("{}:collapse", disclosure(CALL)));
            assert_eq!((footer.width, footer.height), (24.0, 24.0));
            assert!(
                (footer.y - code.y - code.height - 4.0).abs() < 0.01,
                "code bottom inset and footer top inset total four pixels: {code:?} {footer:?}"
            );
            let card = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::StrokeRect { rect, .. }
                        if rect.x <= header.x
                            && rect.width >= header.width
                            && rect.y <= header.y =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .expect("file card border");
            assert!(
                (card.y + card.height - footer.y - footer.height - 3.0).abs() < 0.01,
                "the footer target stays inside the card border: {card:?} {footer:?}"
            );
            let below = Point::new(footer.x + footer.width / 2.0, card.y + card.height - 1.5);
            assert_ne!(
                frame.hit_test(below),
                Some(format!("{}:collapse", disclosure(CALL)).as_str())
            );
            let visible = text(&frame);
            assert!(
                visible.contains("fn saludo()"),
                "confirmed bytes are visible at {width}: {visible}"
            );
            assert!(!visible.contains("Sin diferencias de texto"));
            assert_eq!(header.height, 28.0);
            assert!(!visible.contains("Escrito"));
            if before_known {
                assert!(!visible.contains("Cobertura limitada"));
                assert!(visible.contains("+3"));
            } else {
                assert!(
                    !visible.contains("+3"),
                    "unknown before cannot imply file creation"
                );
                assert!(!visible.contains("Cobertura limitada"));
                assert!(!visible.contains("inventario inicial"));
                assert!(!visible.contains(COMPACT_SNAPSHOT_NOTE));
                // Focus discloses the same evidence as pointer hover without
                // landing on the nested filename navigation target.
                assert!(tree.focus(&disclosure(CALL)));
                let now = Instant::now();
                let hovered = tree.layout_at(Size::new(width, 700.0), now + Duration::from_secs(1));
                assert!(
                    text(&hovered)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .contains(CONFIRMED_SNAPSHOT_NOTE),
                    "technical evidence remains available in the tooltip"
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn compact_confirmed_snapshot_note_preserves_other_partial_or_missing_evidence() {
    for (truncated, binary, extra_note) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut state = confirmed_write_fixture(false);
        let entry = state.chat_diffs.entries.get_mut(CALL).unwrap();
        entry.files[0].truncated = truncated;
        if binary {
            entry.files[0].lines = Arc::from([]);
            entry.files[0].unavailable = Some("Dato no disponible: archivo binario".into());
        }
        if extra_note {
            entry
                .notes
                .push("Cobertura limitada: se omitieron archivos de esta edición".into());
        }
        let card = crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
            .unwrap();
        let (context, mut tree) = native_tree(card);
        let frame = tree.layout(Size::new(900.0, 700.0));
        let visible = text(&frame);
        if truncated {
            assert!(visible.contains(PARTIAL_DIFF_NOTE));
            assert!(visible.contains("Vista parcial"));
        }
        if binary {
            assert!(visible.contains(PARTIAL_DIFF_NOTE));
            assert!(visible.contains("archivo binario"));
            assert!(!visible.contains(COMPACT_SNAPSHOT_NOTE));
        }
        if extra_note {
            assert!(visible.contains("se omitieron archivos de esta edición"));
            assert!(!visible.contains(COMPACT_SNAPSHOT_NOTE));
        }
        assert!(!visible.contains("+3"));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn streamed_file_lifecycle_reuses_one_disclosure_and_preserves_markdown_draft_and_dock() {
    for viewport in VIEWPORTS {
        let mut state = fixture();
        let mut projection = projected_start();
        projection.sync_into(&mut state.transcript);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let before = tree.layout(viewport);
        assert!(tree.focus("prompt"));
        tree.set_input_cursor("prompt", 9);
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowRight,
            shift: true,
            ctrl: false,
        });
        let selection = tree.input_selection("prompt");
        for fragment in [
            r#"{"path":"src/saludo.rs","content":"fn saludo() {\n"#,
            r#"    println!(\"Hola 🦀\");\n}\n"}"#,
        ] {
            delta(&mut projection, fragment);
            projection.sync_into(&mut state.transcript);
            tree.set_root(root(&state, viewport));
            let frame = tree.layout(viewport);
            assert_one_disclosure(&frame, CALL);
            assert_stable_composer(&before, &frame);
            assert!(!code_regions(&frame).is_empty());
            assert_eq!(tree.input_selection("prompt"), selection);
            assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        }
        let tool = ToolCall {
            id: CALL.into(),
            name: "write_file".into(),
            arguments: json!({"path":"src/saludo.rs", "content":CODE}),
        };
        projection.apply_kind(&AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant(
                format!("**Revisión**\n\n{BEFORE}"),
                vec![tool.clone()],
            ),
            response_id: None,
            usage: None,
        });
        projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: tool });
        projection.sync_into(&mut state.transcript);
        tree.set_root(root(&state, viewport));
        let running = tree.layout(viewport);
        assert_one_disclosure(&running, CALL);
        assert!(text(&running).contains("Escribiendo"));
        assert!(tree.focus(&disclosure(CALL)));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            [UiAction::Clicked {
                id: disclosure(CALL)
            }]
        );
        state
            .conversation_details
            .toggle(&format!("tool:{CALL}"), true);
        projection.apply_kind(&AgentEventKind::ToolFinished {
            tool_call_id: CALL.into(),
            output: "file written".into(),
            is_error: false,
        });
        projection.apply_kind(&AgentEventKind::ModelRequestStarted {
            step: 2,
            model: "fixture".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantMessage {
            content: format!("**Resultado**\n\n{AFTER}"),
        });
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: format!("**Resultado**\n\n{AFTER}"),
            steps: 2,
        });
        projection.sync_into(&mut state.transcript);
        confirmed_diff(&mut state, CALL);
        tree.set_root(root(&state, viewport));
        let collapsed = tree.layout(viewport);
        assert_one_disclosure(&collapsed, CALL);
        assert!(
            code_regions(&collapsed).is_empty(),
            "explicit collapse survives completion"
        );
        state
            .conversation_details
            .toggle(&format!("tool:{CALL}"), true);
        tree.set_root(root(&state, viewport));
        let completed = tree.layout(viewport);
        assert_stable_composer(&before, &completed);
        assert_one_disclosure(&completed, CALL);
        assert!(completed
            .regions
            .iter()
            .any(|region| region.id == crate::chat_diff_ui::view_id(OWNER, CALL, 0)));
        assert!(!text(&completed).contains("Escrito"));
        assert!(text(&completed).contains("+1"));
        assert!(text(&completed).contains("−1"));
        assert!(!text(&completed).contains("pendiente de confirmación"));
        let plain = completed
            .text_layouts()
            .iter()
            .map(|row| row.source.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(plain.contains(BEFORE) && plain.contains(AFTER));
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn file_actions_are_independent_scoped_and_have_descriptive_tooltips() {
    let mut state = fixture();
    state.transcript = vec![write_tool(ActivityStatus::Completed, false)];
    confirmed_diff(&mut state, CALL);
    for width in [360.0, 900.0, 1700.0] {
        let card = crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
            .unwrap();
        let (context, mut tree) = native_tree(card);
        let start = Instant::now();
        let size = Size::new(width, 500.0);
        let frame = tree.layout_at(size, start);
        let navigate = crate::file_activity_ui::open_action(OWNER, CALL, 0);
        for (id, action, hint) in [(navigate.clone(), navigate.clone(), "src/saludo.rs")] {
            let point = center(rect(&frame, &id));
            assert_eq!(frame.hit_test(point), Some(id.as_str()));
            tree.handle_event_at(UiEvent::PointerDown(point), start);
            assert_eq!(
                tree.handle_event_at(UiEvent::PointerUp(point), start),
                [UiAction::Clicked { id: action }]
            );
            tree.handle_event_at(UiEvent::PointerMove(point), start);
            let hovered = tree.layout_at(size, start + Duration::from_secs(1));
            assert!(
                text(&hovered).contains(hint),
                "missing {hint} tooltip at {width}"
            );
            assert!(!text(&hovered).contains("Abrir en el editor"));
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == format!("tool:{CALL}")));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == format!("{navigate}:button")
                || region.id == crate::file_activity_ui::inspect_action(OWNER, CALL)));
        let bounds = rect(&frame, &disclosure(CALL));
        for point in [
            Point::new(bounds.x + 2.0, bounds.y + 2.0),
            Point::new(bounds.x + bounds.width - 16.0, bounds.y + 16.0),
            Point::new(
                bounds.x + bounds.width / 2.0,
                bounds.y + bounds.height - 8.0,
            ),
        ] {
            assert_eq!(frame.hit_test(point), Some(disclosure(CALL).as_str()));
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                [UiAction::Clicked {
                    id: disclosure(CALL)
                }]
            );
        }
        assert_one_disclosure(&frame, CALL);
        context.end_pass().textures_delta.clear();
    }
}

const READ_CALL: &str = "read:captured";
const READ_CONTENT: &str = "let a = 1;\n\nlet b = 2;\n";

#[derive(Clone, Copy)]
enum ReadCardState {
    Complete,
    Waiting,
    Partial,
    Failed,
    Empty,
}

impl ReadCardState {
    fn name(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Waiting => "waiting",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Empty => "empty",
        }
    }
    fn partial(self) -> Option<(&'static str, bool)> {
        matches!(self, Self::Partial).then_some((READ_CONTENT, true))
    }
}

fn read_card_fixture(scenario: ReadCardState, expanded: bool) -> ViewState {
    let mut state = fixture();
    let (activity, output, is_error) = match scenario {
        ReadCardState::Complete => (ActivityStatus::Completed, Some(READ_CONTENT.into()), false),
        ReadCardState::Waiting | ReadCardState::Partial => (ActivityStatus::Running, None, false),
        ReadCardState::Failed => (
            ActivityStatus::Failed,
            Some("No se pudo leer: permiso denegado".into()),
            true,
        ),
        ReadCardState::Empty => (ActivityStatus::Completed, Some(String::new()), false),
    };
    state.transcript = vec![TranscriptItem::Tool {
        id: READ_CALL.into(),
        name: "read_file".into(),
        arguments: json!({"path":"src/captura.rs", "start_line":41}),
        output,
        is_error,
        activity,
    }];
    if expanded {
        state
            .conversation_details
            .toggle(&format!("tool:{READ_CALL}"), false);
    }
    state
}

fn read_card_root(state: &ViewState, scenario: ReadCardState) -> Element {
    let card = crate::file_activity_ui::build(
        &state.transcript[0],
        state,
        false,
        scenario.partial(),
        None,
    )
    .unwrap();
    Element::column(vec![card])
        .padding(12.0)
        .background(crate::theme::BACKGROUND)
}

fn painted_read_source(frame: &Frame) -> String {
    // Syntax highlighting may split a row into several native text commands.
    // Preserve their authored spacing instead of injecting line breaks between spans.
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

fn enclosing_file_border(frame: &Frame, header: Rect) -> Option<Rect> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::StrokeRect { rect, .. }
            if rect.x <= header.x
                && rect.x + rect.width >= header.x + header.width
                && rect.y <= header.y
                && rect.y + rect.height > header.y + header.height =>
        {
            Some(*rect)
        }
        _ => None,
    })
}

#[test]
fn expanded_read_uses_one_frame_captured_line_numbers_and_an_inset_collapse_hover() {
    for width in [240.0, 360.0, 900.0] {
        let state = read_card_fixture(ReadCardState::Complete, true);
        let preview = crate::file_activity_preview::preview(&state.transcript[0], None).unwrap();
        assert_eq!(preview.files.len(), 1);
        let lines = &preview.files[0].lines;
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["let a = 1;", "", "let b = 2;"]
        );
        assert_eq!(
            lines.iter().map(|line| line.new_line).collect::<Vec<_>>(),
            [Some(41), Some(42), Some(43)]
        );
        assert!(lines
            .iter()
            .all(|line| line.kind == CodeDiffLineKind::Context && line.old_line == line.new_line));
        let (context, mut tree) = native_tree(read_card_root(&state, ReadCardState::Complete));
        let viewport = Size::new(width, 420.0);
        let frame = tree.layout(viewport);
        assert_one_disclosure(&frame, READ_CALL);
        let mut clips = Vec::new();
        let mut filename_bounds = None;
        let mut status_left = None;
        for command in &frame.commands {
            match command {
                DrawCommand::PushClip(clip) => clips.push(*clip),
                DrawCommand::PopClip => {
                    clips.pop();
                }
                DrawCommand::Text {
                    text,
                    position,
                    font_size,
                    ..
                } if text == "captura.rs" => {
                    let text_width = context.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                text.clone(),
                                egui::FontId::proportional(*font_size),
                                egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    });
                    let right = position.x + text_width;
                    let fully_visible = clips.iter().all(|clip| {
                        position.x >= clip.x - 0.1 && right <= clip.x + clip.width + 0.1
                    });
                    filename_bounds = Some((right, fully_visible));
                }
                DrawCommand::Text { text, position, .. } if text == "Leído" => {
                    status_left = Some(position.x);
                }
                _ => {}
            }
        }
        assert!(
            matches!((filename_bounds, status_left), (Some((right, true)), Some(left)) if left - right >= 8.0 - 0.1),
            "the complete filename stays visible with space before its status at {width}px: {filename_bounds:?}, {status_left:?}"
        );
        let visible = text(&frame);
        assert!(!visible.contains("Lectura capturada"));
        assert!(!visible.contains("Esperando"));
        let source = painted_read_source(&frame);
        assert!(source.contains("let a = 1;") && source.contains("let b = 2;"));
        for number in ["41", "42", "43"] {
            assert!(visible.lines().any(|line| line == number));
        }
        let header = rect(&frame, &disclosure(READ_CALL));
        let code = rect(&frame, &format!("file-preview:{OWNER}:{READ_CALL}:0"));
        let footer_id = format!("{}:collapse", disclosure(READ_CALL));
        let footer = rect(&frame, &footer_id);
        let card = enclosing_file_border(&frame, header)
            .expect("expanded read has the same subtle card as writes");
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. } if *rect == card && *color == crate::theme::CODE_SURFACE
        )), "the frame contains a continuous code surface");
        assert!((code.x - header.x - 8.0).abs() < 0.1);
        assert!((header.x + header.width - code.x - code.width - 8.0).abs() < 0.1);
        assert!(
            (code.y - header.y - header.height - 9.0).abs() < 0.1,
            "separator plus eight-pixel content inset"
        );
        assert_eq!((footer.width, footer.height), (24.0, 24.0));
        assert!(
            (footer.y - code.y - code.height - 4.0).abs() < 0.1,
            "no spare row between read content and collapse target"
        );
        assert!((card.y + card.height - footer.y - footer.height - 3.0).abs() < 0.1);
        let below = Point::new(center(footer).x, card.y + card.height - 1.5);
        assert_ne!(
            frame.hit_test(below),
            Some(footer_id.as_str()),
            "the footer does not reach the card edge"
        );
        let now = Instant::now();
        tree.handle_event_at(UiEvent::PointerMove(center(footer)), now);
        let hover = tree.layout_at(viewport, now + Duration::from_millis(140));
        let fills = hover
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if *color == crate::theme::component_palette().hover =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            fills,
            [footer],
            "only the compact footer button gets hover paint"
        );
        assert!(!text(&hover).contains("Lectura capturada"));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn expanded_read_filename_disclosure_and_footer_keep_separate_scoped_actions() {
    for width in [240.0, 360.0, 900.0] {
        let mut state = read_card_fixture(ReadCardState::Complete, true);
        let (context, mut tree) = native_tree(read_card_root(&state, ReadCardState::Complete));
        let viewport = Size::new(width, 420.0);
        let frame = tree.layout(viewport);
        let navigate = crate::file_activity_ui::open_action(OWNER, READ_CALL, 0);
        let toggle = disclosure(READ_CALL);
        let footer_id = format!("{toggle}:collapse");
        let filename = rect(&frame, &navigate);
        let header = rect(&frame, &toggle);
        assert!(filename.x >= header.x && filename.x + filename.width <= header.x + header.width);
        for (visual, expected) in [(&navigate, &navigate), (&footer_id, &toggle)] {
            let point = center(rect(&frame, visual));
            assert_eq!(frame.hit_test(point), Some(visual.as_str()));
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                [UiAction::Clicked {
                    id: expected.clone()
                }]
            );
            assert!(tree.focus(visual));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                [UiAction::Clicked {
                    id: expected.clone()
                }]
            );
        }
        // The right-hand chevron is a disclosure; it never opens the file.
        let chevron = Point::new(header.x + header.width - 18.0, center(header).y);
        assert_eq!(frame.hit_test(chevron), Some(toggle.as_str()));
        tree.handle_event(UiEvent::PointerDown(chevron));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(chevron)),
            [UiAction::Clicked { id: toggle.clone() }]
        );
        state
            .conversation_details
            .toggle(&format!("tool:{READ_CALL}"), false);
        tree.set_root(read_card_root(&state, ReadCardState::Complete));
        let collapsed = tree.layout(viewport);
        assert_one_disclosure(&collapsed, READ_CALL);
        assert!(code_regions(&collapsed).is_empty());
        assert!(!collapsed
            .regions
            .iter()
            .any(|region| region.id == footer_id));
        assert!(collapsed.regions.iter().any(|region| region.id == navigate));
        assert!(
            enclosing_file_border(&collapsed, rect(&collapsed, &toggle)).is_none(),
            "closed reads keep their quiet bare row"
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn expanded_read_waiting_failed_empty_and_partial_states_keep_their_evidence() {
    for width in [240.0, 360.0, 900.0] {
        for scenario in [
            ReadCardState::Waiting,
            ReadCardState::Failed,
            ReadCardState::Empty,
            ReadCardState::Partial,
        ] {
            let state = read_card_fixture(scenario, true);
            let (context, mut tree) = native_tree(read_card_root(&state, scenario));
            let frame = tree.layout(Size::new(width, 480.0));
            let visible = text(&frame)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(!visible.contains("Lectura capturada"));
            assert!(enclosing_file_border(&frame, rect(&frame, &disclosure(READ_CALL))).is_some());
            match scenario {
                ReadCardState::Waiting => {
                    assert!(
                        visible.contains("Esperando el contenido del archivo"),
                        "{visible}"
                    );
                    assert!(code_regions(&frame).is_empty());
                }
                ReadCardState::Failed => {
                    assert!(
                        visible.contains("No se obtuvo contenido de esta lectura"),
                        "{visible}"
                    );
                    assert!(
                        visible.contains("permiso denegado"),
                        "error details stay visible: {visible}"
                    );
                    assert!(
                        code_regions(&frame).is_empty(),
                        "failure diagnostics are never source code"
                    );
                }
                ReadCardState::Empty => {
                    assert!(
                        visible.contains("La lectura no devolvió contenido"),
                        "{visible}"
                    );
                    assert!(code_regions(&frame).is_empty());
                }
                ReadCardState::Partial => {
                    assert!(visible.contains("Vista parcial"), "{visible}");
                    assert!(
                        !visible.contains("Esperando"),
                        "received bytes replace the waiting state"
                    );
                    assert!(painted_read_source(&frame).contains("let a = 1;"));
                    assert_eq!(code_regions(&frame).len(), 1);
                }
                ReadCardState::Complete => unreachable!(),
            }
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == format!("{}:collapse", disclosure(READ_CALL))));
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn read_preview_virtualizes_lines_and_nested_scroll_does_not_move_the_composer() {
    for viewport in VIEWPORTS {
        let mut state = fixture();
        state.transcript = vec![TranscriptItem::Tool {
            id: "read-config".into(),
            name: "read_file".into(),
            arguments: json!({"path":"config/settings.txt", "start_line":41}),
            output: Some(
                (0..160)
                    .map(|index| format!("CAPTURED_LINE_{index:03}\n"))
                    .collect(),
            ),
            is_error: false,
            activity: ActivityStatus::Completed,
        }];
        state.conversation_details.toggle("tool:read-config", false);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let first = tree.layout(viewport);
        let code = code_regions(&first);
        assert_eq!(code.len(), 1);
        assert!(code[0].height <= 240.0);
        assert!(code[0].x >= 0.0 && code[0].x + code[0].width <= viewport.width + 0.1);
        let visible = text(&first);
        assert!(visible.contains("CAPTURED_LINE_000"));
        assert!(!visible.contains("CAPTURED_LINE_159"));
        assert!(visible.lines().any(|line| line == "41"));
        let outer_offset = tree.scroll_offset("transcript");
        tree.handle_event(UiEvent::ScrollDirect {
            position: center(code[0]),
            delta_y: -400.0,
        });
        let middle = tree.layout(viewport);
        assert!(!text(&middle).contains("CAPTURED_LINE_000"));
        assert!(!text(&middle).contains("CAPTURED_LINE_159"));
        assert_eq!(tree.scroll_offset("transcript"), outer_offset);
        assert_stable_composer(&first, &middle);

        // Reach the last captured row at every responsive preview height.
        // Surplus scroll at the inner boundary may correctly reach the parent;
        // only the fully consumed gesture above must leave its offset untouched.
        tree.handle_event(UiEvent::ScrollDirect {
            position: center(code_regions(&middle)[0]),
            delta_y: -10_000.0,
        });
        let last = tree.layout(viewport);
        assert!(text(&last).contains("CAPTURED_LINE_159"));
        assert!(!text(&last).contains("CAPTURED_LINE_000"));
        assert_stable_composer(&first, &last);
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn read_identity_stays_visible_from_preparation_through_result_without_auto_expanding() {
    for width in [360.0, 900.0, 1700.0] {
        let mut state = fixture();
        for item in [
            TranscriptItem::ToolPreview {
                id: "prepare-read".into(),
                step: 1,
                item_id: "read-item".into(),
                call_id: Some("read-config".into()),
                name: Some("read_file".into()),
                arguments: r#"{"path":"config/settings.txt"}"#.into(),
                input_complete: true,
                status: ModelStreamStatus::Receiving,
                truncated: false,
            },
            TranscriptItem::Tool {
                id: "read-config".into(),
                name: "read_file".into(),
                arguments: json!({"path":"config/settings.txt"}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Running,
            },
            TranscriptItem::Tool {
                id: "read-config".into(),
                name: "read_file".into(),
                arguments: json!({"path":"config/settings.txt"}),
                output: Some("READ_RESULT_VISIBLE_ONLY_WHEN_EXPANDED".into()),
                is_error: false,
                activity: ActivityStatus::Completed,
            },
        ] {
            state.transcript = vec![item];
            let card =
                crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
                    .unwrap();
            let (context, mut tree) = native_tree(card);
            let frame = tree.layout(Size::new(width, 400.0));
            assert_one_disclosure(&frame, "read-config");
            assert!(
                text(&frame).contains("settings.txt"),
                "read identity missing at {width}"
            );
            assert!(!text(&frame).contains("READ_RESULT_VISIBLE_ONLY_WHEN_EXPANDED"));
            assert!(code_regions(&frame).is_empty());
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn multifile_patch_uses_one_disclosure_and_bounded_code_viewports() {
    let mut state = fixture();
    state.transcript = vec![TranscriptItem::Tool {
        id: "patch-two".into(),
        name: "apply_patch".into(),
        arguments: json!({"patch":"*** Begin Patch\n*** Update File: src/a.rs\n@@\n-old\n+new\n*** Add File: src/b.rs\n+fn b() {}\n*** End Patch\n"}),
        output: None,
        is_error: false,
        activity: ActivityStatus::Running,
    }];
    for width in [360.0, 900.0, 1700.0] {
        let card = crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
            .unwrap();
        let (context, mut tree) = native_tree(card);
        let frame = tree.layout(Size::new(width, 700.0));
        assert_one_disclosure(&frame, "patch-two");
        assert!(text(&frame).contains("src/a.rs") && text(&frame).contains("src/b.rs"));
        let code = code_regions(&frame);
        assert_eq!(code.len(), 2);
        assert!(code.iter().all(|rect| rect.x >= 0.0
            && rect.x + rect.width <= width + 0.1
            && rect.height <= 240.0));
        assert!(!text(&frame).contains("Cambios aplicados"));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn failures_cancellations_and_foreign_cached_diffs_never_look_like_confirmed_writes() {
    for (activity, error, note) in [
        (ActivityStatus::Failed, true, "permiso denegado"),
        (ActivityStatus::Cancelled, false, "cancelada"),
        (ActivityStatus::Interrupted, false, "incierto"),
    ] {
        let mut state = fixture();
        state.transcript = vec![write_tool(activity, error)];
        confirmed_diff(&mut state, CALL);
        state
            .conversation_details
            .toggle(&format!("tool:{CALL}"), true);
        let (context, mut tree) = native_tree(root(&state, VIEWPORTS[0]));
        let frame = tree.layout(VIEWPORTS[0]);
        assert_one_disclosure(&frame, CALL);
        assert!(text(&frame).contains(note));
        assert!(!text(&frame).contains("Escrito"));
        assert!(code_regions(&frame).is_empty());
        context.end_pass().textures_delta.clear();
    }
    let mut state = fixture();
    state.transcript = vec![write_tool(ActivityStatus::Completed, false)];
    confirmed_diff(&mut state, CALL);
    state.chat_diffs.session_id = Some("another-session".into());
    let (context, mut tree) = native_tree(root(&state, VIEWPORTS[0]));
    let frame = tree.layout(VIEWPORTS[0]);
    assert!(text(&frame).contains("Preparando la vista de la edición confirmada"));
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id.starts_with("chat-diff:")));
    context.end_pass().textures_delta.clear();
}

#[test]
fn appended_file_activity_preserves_manual_history_scroll_and_draft_selection() {
    let viewport = VIEWPORTS[0];
    let mut state = fixture();
    for index in 0..32 {
        state.transcript.push(TranscriptItem::Assistant {
            content: format!(
                "### Revisión {index}\n\n{}",
                "Historia conservada y explicada con detalle. ".repeat(8)
            ),
            streaming: false,
        });
    }
    let (context, mut tree) = native_tree(root(&state, viewport));
    tree.layout(viewport);
    tree.scroll_to_end("transcript");
    let end = tree.layout(viewport);
    tree.handle_event(UiEvent::ScrollDirect {
        position: center(rect(&end, "transcript")),
        delta_y: 420.0,
    });
    let before = tree.layout(viewport);
    let offset = tree.scroll_offset("transcript");
    assert!(!tree.is_scroll_at_end("transcript", 24.0));
    state
        .transcript
        .push(write_tool(ActivityStatus::Running, false));
    tree.set_root(root(&state, viewport));
    let after = tree.layout(viewport);
    assert_eq!(tree.scroll_offset("transcript"), offset);
    assert_stable_composer(&before, &after);
    assert_eq!(tree.input_value("prompt"), Some(DRAFT));
    context.end_pass().textures_delta.clear();
}

fn export(frame: &Frame, name: &str, viewport: Size) {
    let directory = Path::new("/tmp/openade-file-activity-review");
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}.svg", viewport.width));
    crate::settings_ui::visual_qa::write_native_frame_svg(
        frame,
        &path,
        viewport.width,
        viewport.height,
    )
    .unwrap();
    let svg = std::fs::read_to_string(&path).unwrap();
    let parsed =
        resvg::usvg::Tree::from_str(&svg, &crate::settings_ui::visual_qa::native_svg_options())
            .unwrap();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(viewport.width as u32, viewport.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
}

fn settled_export_frame(tree: &mut UiTree, viewport: Size) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut frame = tree.layout(viewport);
    // Syntax highlighting completes on a bounded worker. Reduced motion keeps
    // streaming indicators static, so pending work can settle before capture.
    while tree.is_animating() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(8));
        frame = tree.layout(viewport);
    }
    assert!(
        !tree.is_animating(),
        "file activity export did not settle within three seconds"
    );
    frame
}

#[test]
#[ignore = "exports confirmed new-file and unknown-before content with native fonts under /tmp"]
fn export_confirmed_file_evidence_visual_review() {
    for width in [240.0, 360.0, 900.0] {
        for (scenario, before_known) in [
            ("confirmed-new-file", true),
            ("confirmed-content-unknown-before", false),
        ] {
            let state = confirmed_write_fixture(before_known);
            let card =
                crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
                    .unwrap();
            let viewport = Size::new(width, 500.0);
            let (context, mut tree) = native_tree(
                Element::column(vec![card])
                    .padding(12.0)
                    .background(crate::theme::BACKGROUND),
            );
            export(
                &settled_export_frame(&mut tree, viewport),
                scenario,
                viewport,
            );
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
#[ignore = "exports native-font file activity PNG/SVG fixtures under /tmp"]
fn export_file_activity_visual_review() {
    for viewport in VIEWPORTS {
        for scenario in [
            "streaming",
            "confirmed-edit",
            "patch-multifile",
            "read-search-web",
            "failed",
        ] {
            let mut state = fixture();
            state.transcript.push(TranscriptItem::Assistant {
                content: format!("### Revisión del proyecto\n\n{BEFORE}"),
                streaming: false,
            });
            match scenario {
                "streaming" => state.transcript.push(TranscriptItem::ToolPreview {
                    id: "stream-file".into(), step: 1, item_id: "file-1".into(), call_id: Some(CALL.into()), name: Some("write_file".into()),
                    arguments: r#"{"path":"src/saludo.rs","content":"pub fn saludo() {\n    println!(\"Hola 🦀\");\n}\n\n#[test]\nfn saluda_en_español() {\n    saludo();"#.into(),
                    input_complete: false, status: ModelStreamStatus::Receiving, truncated: false,
                }),
                "confirmed-edit" => { state.transcript.push(write_tool(ActivityStatus::Completed, false)); confirmed_diff(&mut state, CALL); },
                "patch-multifile" => state.transcript.push(TranscriptItem::Tool {
                    id: "patch-1".into(), name: "apply_patch".into(), arguments: json!({"patch":"*** Begin Patch\n*** Update File: src/saludo.rs\n@@\n-    println!(\"Hola\");\n+    println!(\"Hola 🦀\");\n*** Add File: tests/saludo.rs\n+#[test]\n+fn saluda() {\n+    assert!(true);\n+}\n*** End Patch\n"}), output: None, activity: ActivityStatus::Running, is_error: false,
                }),
                "failed" => state.transcript.push(write_tool(ActivityStatus::Failed, true)),
                _ => {
                    for (id, name, args, output) in [
                        ("read", "read_file", json!({"path":"src/saludo.rs"}), CODE.to_owned()),
                        ("search", "search_files", json!({"pattern":"saludo", "path":"src"}), "src/saludo.rs:1:pub fn saludo() {\nsrc/main.rs:5:saludo();".into()),
                        ("web", "web_search", json!({"query":"Rust testing documentation"}), json!({"results":[{"title":"The Rust Book: Writing Tests", "url":"https://doc.rust-lang.org/book/ch11-01-writing-tests.html", "snippet":"Organización de pruebas y aserciones."}]}).to_string()),
                    ] {
                        state.transcript.push(TranscriptItem::Tool { id: id.into(), name: name.into(), arguments: args, output: Some(output), activity: ActivityStatus::Completed, is_error: false });
                        state.conversation_details.toggle(&format!("tool:{id}"), false);
                    }
                }
            }
            state.transcript.push(TranscriptItem::Assistant {
                content: format!("**Resultado**\n\n{AFTER}"),
                streaming: false,
            });
            let (context, mut tree) = native_tree(root(&state, viewport));
            let first = settled_export_frame(&mut tree, viewport);
            export(&first, scenario, viewport);
            if !tree.is_scroll_at_end("transcript", 1.0) {
                tree.scroll_to_end("transcript");
                export(
                    &settled_export_frame(&mut tree, viewport),
                    &format!("{scenario}-end"),
                    viewport,
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
#[ignore = "exports compact local-hover file headers with native fonts under /tmp"]
fn export_file_activity_header_visual_review() {
    for width in [240.0, 360.0, 900.0] {
        let viewport = Size::new(width, 380.0);
        let mut state = fixture();
        state.transcript.push(TranscriptItem::ToolPreview {
            id: "cancelled-file-preview".into(), step: 1, item_id: "pending-file".into(),
            call_id: Some(CALL.into()), name: Some("write_file".into()),
            arguments: json!({"path":"prueba-raiz.md", "content":"# Prueba\n\nContenido propuesto; no se escribió en disco.\n"}).to_string(),
            input_complete: true, status: ModelStreamStatus::Cancelled, truncated: false,
        });
        let card = crate::file_activity_ui::build(&state.transcript[0], &state, false, None, None)
            .unwrap();
        let (context, mut tree) = native_tree(
            Element::column(vec![card])
                .padding(16.0)
                .background(crate::theme::BACKGROUND),
        );
        let first = settled_export_frame(&mut tree, viewport);
        export(&first, "header-cancelled", viewport);
        let action = crate::file_activity_ui::open_action(OWNER, "cancelled-file-preview", 0);
        for (target, id) in [("title", action.clone()), ("disclosure", disclosure(CALL))] {
            let now = Instant::now();
            tree.handle_event_at(UiEvent::PointerMove(center(rect(&first, &id))), now);
            let hovered = tree.layout_at(viewport, now + Duration::from_millis(140));
            export(&hovered, &format!("header-{target}-hover"), viewport);
        }
        context.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "exports expanded read cards, real captured rows and footer hover with native fonts under /tmp"]
fn export_expanded_read_activity_visual_review() {
    for width in [240.0, 360.0, 900.0] {
        let viewport = Size::new(width, 420.0);
        for scenario in [
            ReadCardState::Complete,
            ReadCardState::Waiting,
            ReadCardState::Partial,
            ReadCardState::Failed,
            ReadCardState::Empty,
        ] {
            let state = read_card_fixture(scenario, true);
            let (context, mut tree) = native_tree(read_card_root(&state, scenario));
            let frame = settled_export_frame(&mut tree, viewport);
            export(
                &frame,
                &format!("read-expanded-{}", scenario.name()),
                viewport,
            );
            if matches!(scenario, ReadCardState::Complete) {
                for (name, id) in [
                    (
                        "filename-hover",
                        crate::file_activity_ui::open_action(OWNER, READ_CALL, 0),
                    ),
                    (
                        "footer-hover",
                        format!("{}:collapse", disclosure(READ_CALL)),
                    ),
                ] {
                    let now = Instant::now();
                    tree.handle_event_at(UiEvent::PointerMove(center(rect(&frame, &id))), now);
                    export(
                        &tree.layout_at(viewport, now + Duration::from_millis(140)),
                        &format!("read-expanded-{name}"),
                        viewport,
                    );
                }
            }
            context.end_pass().textures_delta.clear();
        }
        let state = read_card_fixture(ReadCardState::Complete, false);
        let (context, mut tree) = native_tree(read_card_root(&state, ReadCardState::Complete));
        export(
            &settled_export_frame(&mut tree, viewport),
            "read-collapsed",
            viewport,
        );
        context.end_pass().textures_delta.clear();
    }
}

#[derive(Clone, Copy)]
enum FileFrameWorkload {
    Streaming,
    Hover,
    Scroll,
}

impl FileFrameWorkload {
    fn name(self) -> &'static str {
        match self {
            Self::Streaming => "streaming_rebuild",
            Self::Hover => "hover_retained",
            Self::Scroll => "scroll_retained",
        }
    }
}

fn percentile(samples: &[Duration], percent: usize) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)].as_secs_f64() * 1000.0
}

#[test]
#[ignore = "bounded native-font CPU build/layout probe; excludes paint, GPU and compositor"]
fn file_activity_frame_benchmark() {
    const HISTORY: usize = 48;
    const WARMUP: usize = 8;
    const FRAMES: usize = 96;
    let viewport = Size::new(1280.0, 800.0);
    for workload in [
        FileFrameWorkload::Streaming,
        FileFrameWorkload::Hover,
        FileFrameWorkload::Scroll,
    ] {
        let mut state = fixture();
        for index in 0..HISTORY {
            state.transcript.push(TranscriptItem::User {
                content: format!("Revisá el archivo de configuración {index}."),
                images: vec![],
            });
            state.transcript.push(TranscriptItem::Tool {
                id: format!("history-read:{index}"),
                name: "read_file".into(),
                arguments: json!({"path":format!("src/config_{index}.rs")}),
                output: Some(
                    (0..60)
                        .map(|line| format!("pub const VALUE_{line}: usize = {line};\n"))
                        .collect(),
                ),
                is_error: false,
                activity: ActivityStatus::Completed,
            });
            state.transcript.push(TranscriptItem::Assistant {
                content: format!("### Revisión {index}\n\nLa configuración conserva sus valores existentes. **Resultado:** se verificaron las opciones y sus referencias.\n\n- Comprobar el archivo.\n- Conservar el contenido.\n- Documentar el resultado."),
                streaming: false,
            });
        }
        state.transcript.push(TranscriptItem::User {
            content: "Prepará la nueva configuración".into(),
            images: vec![],
        });
        state.transcript.push(TranscriptItem::ToolPreview {
            id: "bench-file-input".into(),
            step: 1,
            item_id: "bench-file".into(),
            call_id: Some(CALL.into()),
            name: Some("write_file".into()),
            arguments: r#"{"path":"src/config.rs","content":"// Configuración incremental\n"#
                .into(),
            input_complete: false,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        });
        let (context, mut tree) = native_tree_at_scale(root(&state, viewport), 2.0);
        tree.set_reduced_motion(false);
        let start = Instant::now();
        let mut frame = tree.layout_at(viewport, start);
        let mut builds = Vec::with_capacity(FRAMES);
        let mut layouts = Vec::with_capacity(FRAMES);
        let mut totals = Vec::with_capacity(FRAMES);
        let mut commands = 0usize;
        for step in 0..WARMUP + FRAMES {
            let measured = step >= WARMUP;
            let index = step.saturating_sub(WARMUP);
            let now = start + Duration::from_secs_f64((step + 1) as f64 / 120.0);
            if measured && matches!(workload, FileFrameWorkload::Streaming) {
                let fragment = serde_json::to_string(&format!(
                    "pub const STREAM_VALUE_{index}: usize = {index}; // 🦀\n"
                ))
                .unwrap();
                if let Some(TranscriptItem::ToolPreview { arguments, .. }) =
                    state.transcript.last_mut()
                {
                    // Append the encoded string body, leaving the input string
                    // genuinely unfinished throughout this measured stream.
                    arguments.push_str(&fragment[1..fragment.len() - 1]);
                }
            }
            let mut build_time = Duration::ZERO;
            if !measured || matches!(workload, FileFrameWorkload::Streaming) {
                let began = Instant::now();
                tree.set_root(root(&state, viewport));
                build_time = began.elapsed();
                tree.scroll_to_end("transcript");
            } else {
                match workload {
                    FileFrameWorkload::Hover => {
                        let id = if index % 2 == 0 {
                            disclosure(CALL)
                        } else {
                            "run_toggle".into()
                        };
                        tree.handle_event_at(UiEvent::PointerMove(center(rect(&frame, &id))), now);
                    }
                    FileFrameWorkload::Scroll => {
                        let transcript = rect(&frame, "transcript");
                        // The outer reading-column gutter avoids the nested
                        // code viewport; only the transcript is scrolled here.
                        tree.handle_event_at(
                            UiEvent::ScrollDirect {
                                position: Point::new(
                                    transcript.x + 2.0,
                                    transcript.y + transcript.height / 2.0,
                                ),
                                delta_y: if (index / 16) % 2 == 0 { 72.0 } else { -72.0 },
                            },
                            now,
                        );
                    }
                    FileFrameWorkload::Streaming => unreachable!(),
                }
            }
            let began = Instant::now();
            frame = tree.layout_at(viewport, now);
            let layout_time = began.elapsed();
            if matches!(workload, FileFrameWorkload::Streaming) {
                assert!(
                    tree.is_scroll_at_end("transcript", 1.0),
                    "measure the visible live card"
                );
                assert!(
                    frame
                        .regions
                        .iter()
                        .any(|region| region.kind == HitKind::CodeDiff),
                    "the live source preview must be painted"
                );
            }
            if measured {
                builds.push(build_time);
                layouts.push(layout_time);
                totals.push(build_time + layout_time);
                commands += frame.commands.len();
            }
            std::hint::black_box(&frame);
        }
        eprintln!(
            "FILE_ACTIVITY_FRAME workload={} history={} viewport=1280x800 pixels_per_point=2 warmup={} frames={} mean_ms={:.2} p50_ms={:.2} p95_ms={:.2} worst_ms={:.2} build_p50_ms={:.2} build_p95_ms={:.2} layout_p50_ms={:.2} layout_p95_ms={:.2} budget_120hz_ms=8.33 draw_commands={} excludes=paint_gpu_compositor",
            workload.name(),
            HISTORY,
            WARMUP,
            FRAMES,
            totals.iter().sum::<Duration>().as_secs_f64() * 1000.0 / FRAMES as f64,
            percentile(&totals, 50),
            percentile(&totals, 95),
            percentile(&totals, 100),
            percentile(&builds, 50),
            percentile(&builds, 95),
            percentile(&layouts, 50),
            percentile(&layouts, 95),
            commands,
        );
        context.end_pass().textures_delta.clear();
    }
}
