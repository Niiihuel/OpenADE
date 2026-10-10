//! Native chat fixtures driven by local transcript events. No providers, tools,
//! shell commands, permission decisions or filesystem writes are dispatched.

use super::*;
use crate::{
    chat_diff_ui::{DiffFileView, ToolDiffEntry},
    ide_workspace::WindowMode,
    transcript::TranscriptProjection,
    view::{self, ide_visual_tests},
};
use openide_core::{AgentEventKind, AgentMessage, ModelStreamEvent, ToolCall};
use openide_ui::{CodeDiffLine, CodeDiffLineKind, DrawCommand, Frame, HitKind, Rect, Size, UiTree};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

const DIRECTORY: &str = "/tmp/openade-chat-dock-review";
const OWNER: &str = "chat-dock-review";
const READ: &str = "read-workspace";
const COMMAND: &str = "test-workspace";
const WRITE: &str = "write-workspace-test";
const REQUEST: &str = "approve-workspace-test";
const PROMPT: &str = "Revisá cómo se abren los archivos y agregá una prueba que evite duplicados.";
const REASONING: &str =
    "Voy a leer el módulo del workspace y comprobar cómo conserva los archivos abiertos.";
const SOURCE: &str = "pub fn open_file(&mut self, path: &str) {\n    if !self.files.iter().any(|file| file == path) {\n        self.files.push(path.to_owned());\n    }\n}\n";
const PATCH: &str = "#[test]\nfn opening_the_same_file_keeps_one_entry() {\n    let mut workspace = Workspace::default();\n    workspace.open_file(\"README.md\");\n    workspace.open_file(\"README.md\");\n    assert_eq!(workspace.files.len(), 1);\n}\n";
const PARTIAL: &str =
    "   Compiling openide-workspace\n    Finished test profile in 0.42s\nrunning 3 tests\n";
const RESULT: &str = "running 3 tests\ntest opens_a_file ... ok\ntest keeps_insertion_order ... ok\ntest opening_the_same_file_keeps_one_entry ... ok\n\ntest result: ok. 3 passed; 0 failed\n";
const FINAL: &str = "### El workspace evita duplicados\n\n`open_file` comprueba si la ruta ya está abierta antes de agregarla. La prueba nueva abre `README.md` dos veces y verifica que quede una sola entrada.\n\n- **Archivo:** `tests/workspace.rs`\n- **Validación:** las tres pruebas finalizaron correctamente.\n\n```rust\nassert_eq!(workspace.files.len(), 1);\n```\n\nPodés seguir trabajando desde el editor sin perder los archivos abiertos.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    User,
    Thinking,
    ToolsRunning,
    WritePreview,
    Permission,
    Error,
    Cancelled,
    Final,
}

const PHASES: [Phase; 8] = [
    Phase::User,
    Phase::Thinking,
    Phase::ToolsRunning,
    Phase::WritePreview,
    Phase::Permission,
    Phase::Error,
    Phase::Cancelled,
    Phase::Final,
];

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Thinking => "thinking",
            Self::ToolsRunning => "tools-running",
            Self::WritePreview => "write-preview",
            Self::Permission => "permission",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Final => "final",
        }
    }

    fn running(self) -> bool {
        matches!(
            self,
            Self::Thinking | Self::ToolsRunning | Self::WritePreview | Self::Permission
        )
    }
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }
}

fn model_start(projection: &mut TranscriptProjection, step: usize) {
    projection.apply_kind(&AgentEventKind::ModelRequestStarted {
        step,
        model: "native-visual-fixture".into(),
    });
}

fn commit_calls(projection: &mut TranscriptProjection, step: usize, calls: Vec<ToolCall>) {
    projection.apply_kind(&AgentEventKind::ModelResponseCommitted {
        step,
        assistant: AgentMessage::assistant("", calls),
        response_id: None,
        usage: None,
    });
}

fn finish_tool(projection: &mut TranscriptProjection, id: &str, output: &str, is_error: bool) {
    projection.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: id.into(),
        output: output.into(),
        is_error,
    });
}

fn project(phase: Phase) -> TranscriptProjection {
    let mut projection = TranscriptProjection::new();
    projection.apply_kind(&AgentEventKind::TurnStarted {
        prompt: PROMPT.into(),
    });
    if phase == Phase::User {
        return projection;
    }
    model_start(&mut projection, 1);
    projection.apply_kind(&AgentEventKind::ModelStreamUpdate {
        step: 1,
        update: ModelStreamEvent::ReasoningDelta {
            item_id: "workspace-summary".into(),
            content: REASONING.into(),
        },
    });
    if phase == Phase::Thinking {
        return projection;
    }
    let read = call(READ, "read_file", json!({"path":"src/workspace.rs"}));
    commit_calls(&mut projection, 1, vec![read.clone()]);
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: read });
    finish_tool(&mut projection, READ, SOURCE, false);

    model_start(&mut projection, 2);
    let write = call(
        WRITE,
        "write_file",
        json!({"path":"tests/workspace.rs", "content":PATCH}),
    );
    // A streamed write is provisional. It must never acquire a completed tool
    // result or claim the source file changed before a committed invocation.
    projection.apply_kind(&AgentEventKind::ModelStreamUpdate {
        step: 2,
        update: ModelStreamEvent::ToolCallDelta {
            item_id: "workspace-test-input".into(),
            call_id: Some(WRITE.into()),
            name: Some("write_file".into()),
            arguments_delta: write.arguments.to_string(),
        },
    });
    if phase == Phase::WritePreview {
        return projection;
    }
    projection.apply_kind(&AgentEventKind::ModelStreamUpdate {
        step: 2,
        update: ModelStreamEvent::ToolCallReady {
            item_id: "workspace-test-input".into(),
            call_id: WRITE.into(),
            name: "write_file".into(),
        },
    });
    commit_calls(&mut projection, 2, vec![write.clone()]);
    if phase == Phase::Permission {
        projection.apply_kind(&AgentEventKind::PermissionRequested {
            request_id: REQUEST.into(),
            tool_call: write,
        });
        return projection;
    }
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: write });
    finish_tool(
        &mut projection,
        WRITE,
        "Archivo escrito: tests/workspace.rs",
        false,
    );

    model_start(&mut projection, 3);
    let command = call(
        COMMAND,
        "run_command",
        json!({"command":"cargo test -p openide-workspace"}),
    );
    commit_calls(&mut projection, 3, vec![command.clone()]);
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: command });
    projection.apply_kind(&AgentEventKind::ToolOutputDelta {
        tool_call_id: COMMAND.into(),
        stream: OutputStream::Stdout,
        text: PARTIAL.into(),
    });
    match phase {
        Phase::ToolsRunning => {}
        Phase::Error => {
            finish_tool(
                &mut projection,
                COMMAND,
                "error[E0433]: no se pudo resolver Workspace en tests/workspace.rs:3",
                true,
            );
            projection.apply_kind(&AgentEventKind::TurnFailed {
                error: "La validación se detuvo porque falta importar Workspace en la prueba. El archivo escrito se conserva.".into(),
            });
        }
        Phase::Cancelled => {
            projection.apply_kind(&AgentEventKind::TurnCancelled);
        }
        Phase::Final => {
            finish_tool(&mut projection, COMMAND, RESULT, false);
            model_start(&mut projection, 4);
            projection.apply_kind(&AgentEventKind::AssistantMessage {
                content: FINAL.into(),
            });
            projection.apply_kind(&AgentEventKind::TurnCompleted {
                response: FINAL.into(),
                steps: 4,
            });
        }
        _ => unreachable!(),
    }
    projection
}

fn fixture(phase: Phase, workspace: bool) -> ViewState {
    let mut state = if workspace {
        ide_visual_tests::fixture()
    } else {
        super::tests::state()
    };
    state.selected_session = Some(OWNER.into());
    state.selected_session_title = Some("Evitar archivos duplicados".into());
    for session in &mut state.sessions {
        session.id = OWNER.into();
        session.title = "Evitar archivos duplicados".into();
    }
    state.prompt.clear();
    state.running = phase.running();
    state.background_run_selected = false;
    state.show_jump_to_bottom = false;
    state.pending_permissions.clear();
    state.conversation_details.sync(Some(OWNER));
    project(phase).sync_into(&mut state.transcript);
    if phase == Phase::Permission {
        state.pending_permissions.push(PermissionView {
            request_id: REQUEST.into(),
            tool_name: "write_file".into(),
            arguments: json!({"path":"tests/workspace.rs", "content":PATCH}).to_string(),
        });
    }
    // The unrelated terminal fixture normally contains a successful test log.
    // Keep this review focused on the chat's own authoritative tool snapshots.
    state.show_terminal_panel = false;
    state.ide.bottom_open = false;
    if !workspace {
        state.show_side_pane = false;
        state.workspace = "/workspace/openADE".into();
        state.workspace_draft = state.workspace.clone();
        state.project_branch = Some("feature/workspace-tests".into());
    }
    cache_confirmed_write_diff(&mut state);
    state
}

fn cache_confirmed_write_diff(state: &mut ViewState) {
    state.chat_diffs.workspace = Some(Path::new(&state.workspace).to_path_buf());
    state.chat_diffs.session_id = state.selected_session.clone();
    state.chat_diffs.entries.clear();
    // This fixture creates tests/workspace.rs from an absent file. Derive the
    // added lines from the committed call itself, only after its successful
    // result exists. Provisional input and pending permission have no evidence.
    let Some((path, content)) = state.transcript.iter().find_map(|item| match item {
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            output: Some(_),
            is_error: false,
            activity: ActivityStatus::Completed,
        } if id == WRITE && name == "write_file" => Some((
            arguments.get("path")?.as_str()?.to_owned(),
            arguments.get("content")?.as_str()?.to_owned(),
        )),
        _ => None,
    }) else {
        return;
    };
    let lines: Vec<_> = content
        .lines()
        .enumerate()
        .map(|(index, text)| CodeDiffLine {
            kind: CodeDiffLineKind::Added,
            old_line: None,
            new_line: Some(index + 1),
            text: text.into(),
        })
        .collect();
    state.chat_diffs.entries.insert(
        WRITE.into(),
        ToolDiffEntry {
            files: vec![DiffFileView {
                path,
                old_path: None,
                additions: lines.len(),
                deletions: 0,
                truncated: false,
                unavailable: None,
                can_open: true,
                lines: lines.into(),
            }],
            unavailable: None,
            notes: Vec::new(),
            limited: false,
        },
    );
}

#[test]
fn chat_dock_diff_evidence_requires_the_confirmed_write() {
    for phase in PHASES {
        let state = fixture(phase, false);
        let entry = crate::chat_diff_ui::entry(&state, WRITE);
        if matches!(
            phase,
            Phase::ToolsRunning | Phase::Error | Phase::Cancelled | Phase::Final
        ) {
            let files = &entry
                .expect("successful fixture write has its exact diff")
                .files;
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, "tests/workspace.rs");
            assert_eq!(files[0].deletions, 0);
            assert_eq!(files[0].additions, PATCH.lines().count());
            assert_eq!(
                files[0]
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>(),
                PATCH.lines().collect::<Vec<_>>()
            );
            for (index, line) in files[0].lines.iter().enumerate() {
                assert_eq!(line.kind, CodeDiffLineKind::Added);
                assert_eq!(line.old_line, None);
                assert_eq!(line.new_line, Some(index + 1));
            }
        } else {
            assert!(
                entry.is_none(),
                "{phase:?}: provisional input must not become a confirmed edit"
            );
        }
    }
}

fn standalone(state: &ViewState, size: Size) -> Element {
    Element::panel(build_session_surface_sized(
        state,
        size.width,
        size.height,
        false,
        false,
    ))
    .background(theme::BACKGROUND)
}

fn visible_text(frame: &Frame) -> String {
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
        .unwrap_or_else(|| panic!("missing chat control {id}"))
        .rect
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x - 0.1
        && inner.y >= outer.y - 0.1
        && inner.x + inner.width <= outer.x + outer.width + 0.1
        && inner.y + inner.height <= outer.y + outer.height + 0.1
}

fn settled_frame(tree: &mut UiTree, size: Size) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut frame = tree.layout(size);
    while tree.is_animating() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(8));
        frame = tree.layout(size);
    }
    assert!(!tree.is_animating(), "native chat fixture did not settle");
    frame
}

#[test]
fn chat_dock_headers_and_composer_keep_separate_hit_areas() {
    for size in [Size::new(360.0, 780.0), Size::new(480.0, 900.0)] {
        for phase in [
            Phase::Thinking,
            Phase::ToolsRunning,
            Phase::WritePreview,
            Phase::Permission,
        ] {
            let state = fixture(phase, false);
            let (context, mut tree) = ide_visual_tests::native_tree(standalone(&state, size));
            let frame = tree.layout(size);
            let viewport = rect(&frame, "transcript");
            let prompt = rect(&frame, "prompt");
            assert!(viewport.height >= 100.0);
            assert!(
                viewport.y + viewport.height <= prompt.y,
                "{phase:?}: transcript overlaps composer"
            );
            assert!(contains(
                Rect::new(0.0, 0.0, size.width, size.height),
                prompt
            ));
            let headers: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| {
                    region.kind == HitKind::Button
                        && region
                            .id
                            .starts_with(&format!("conversation_detail:{OWNER}:"))
                        && !region.id.ends_with(":collapse")
                })
                .collect();
            assert!(
                !headers.is_empty(),
                "{phase:?}: missing activity disclosures"
            );
            for header in headers {
                assert!(
                    contains(viewport, header.rect),
                    "{phase:?}: {} escaped {viewport:?}: {:?}",
                    header.id,
                    header.rect
                );
                // A write keeps its observed status/diff counts in a second
                // 28px metadata row under the 32px filename row. Reads and
                // other activities remain single-line disclosure headers.
                let max_height = if header.id == format!("conversation_detail:{OWNER}:tool:{WRITE}") {
                    60.0
                } else {
                    32.0
                };
                assert!(
                    header.rect.height <= max_height,
                    "{phase:?}: oversized activity header: {:?}",
                    header.rect
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn chat_dock_partial_and_provisional_tools_have_no_invented_result() {
    for phase in [
        Phase::ToolsRunning,
        Phase::WritePreview,
        Phase::Permission,
        Phase::Cancelled,
    ] {
        let state = fixture(phase, false);
        let expected = if phase == Phase::ToolsRunning {
            ActivityStatus::Running
        } else if phase == Phase::Permission {
            ActivityStatus::WaitingPermission
        }
        // Cancelling a turn does not prove the dispatched process stopped.
        else {
            ActivityStatus::Interrupted
        };
        if phase == Phase::WritePreview {
            assert!(state.transcript.iter().any(|item| matches!(item,
                TranscriptItem::ToolPreview { call_id: Some(id), status: ModelStreamStatus::Receiving, .. } if id == WRITE)));
            assert!(!state.transcript.iter().any(|item| matches!(item,
                TranscriptItem::Tool { id, .. } if id == WRITE)));
        } else {
            let id = if phase == Phase::Permission {
                WRITE
            } else {
                COMMAND
            };
            assert!(state.transcript.iter().any(|item| matches!(item,
                TranscriptItem::Tool { id: actual, output: None, activity, .. } if actual == id && *activity == expected)));
        }
        let size = Size::new(480.0, 900.0);
        let (context, mut tree) = ide_visual_tests::native_tree(standalone(&state, size));
        let frame = tree.layout(size);
        let text = visible_text(&frame);
        assert!(
            !text.contains("Ejecutado"),
            "{phase:?}: a missing result became success"
        );
        if phase == Phase::ToolsRunning {
            assert!(text.contains("Compiling"));
            assert!(frame.commands.iter().any(|command| matches!(command, DrawCommand::Spinner { .. })));
            assert!(!text.contains("Salida en curso"));
            assert!(!text.contains("3 passed"));
        }
        if phase == Phase::WritePreview || phase == Phase::Permission {
            assert!(!text.contains("Escrito"));
        }
        if phase == Phase::Permission {
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("approval_review:")));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("permission_allow")
                    || region.id.starts_with("approval_decide:")));
        }
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn chat_dock_live_reasoning_animates_but_final_response_and_reduced_motion_do_not() {
    let size = Size::new(480.0, 900.0);
    let live = fixture(Phase::Thinking, false);
    assert!(live.transcript.iter().any(|item| matches!(
        item,
        TranscriptItem::Activity(TranscriptActivity::Model {
            has_text: false,
            status: ActivityStatus::Running,
            ..
        })
    )));
    assert!(live.transcript.iter().any(|item| matches!(
        item,
        TranscriptItem::Reasoning {
            status: ModelStreamStatus::Receiving,
            ..
        }
    )));
    let (context, mut tree) = ide_visual_tests::native_tree(standalone(&live, size));
    tree.set_reduced_motion(false);
    let frame = tree.layout(size);
    assert!(visible_text(&frame).contains("Pensando"));
    assert!(
        tree.is_animating(),
        "live reasoning needs a native activity indicator"
    );
    tree.set_reduced_motion(true);
    settled_frame(&mut tree, size);
    let final_state = fixture(Phase::Final, false);
    tree.set_root(standalone(&final_state, size));
    tree.set_reduced_motion(false);
    let final_frame = settled_frame(&mut tree, size);
    assert!(!visible_text(&final_frame).contains("Pensando"));
    assert!(final_state.transcript.iter().any(|item| matches!(item,
        TranscriptItem::Assistant { content, streaming: false } if content == FINAL)));
    context.end_pass().textures_delta.clear();
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new(DIRECTORY);
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}x{:.0}.svg", size.width, size.height));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let svg = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &svg,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.save_png(path.with_extension("png")).unwrap();
}

#[test]
#[ignore = "exports native chat PNG/SVG fixtures under /tmp without executing tools"]
fn export_chat_dock_visual_review() {
    for phase in PHASES {
        for (surface, size, mode) in [
            ("chat", Size::new(360.0, 780.0), None),
            ("chat", Size::new(480.0, 900.0), None),
            ("ide", Size::new(1440.0, 900.0), Some(WindowMode::Ide)),
            ("agents", Size::new(1440.0, 900.0), Some(WindowMode::Agents)),
        ] {
            let mut state = fixture(phase, mode.is_some());
            let root = if let Some(mode) = mode {
                state.ide.mode = mode;
                state.show_side_pane = true;
                view::build_window_view(&state, size.width, size.height)
            } else {
                standalone(&state, size)
            };
            let (context, mut tree) = ide_visual_tests::native_tree(root);
            let first = settled_frame(&mut tree, size);
            let name = format!("{surface}-{}", phase.name());
            export(&first, size, &name);
            if !tree.is_scroll_at_end("transcript", 1.0) {
                tree.scroll_to_end("transcript");
                export(
                    &settled_frame(&mut tree, size),
                    size,
                    &format!("{name}-tail"),
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
}
