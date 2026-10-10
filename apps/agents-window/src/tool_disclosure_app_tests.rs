use super::*;
use openide_core::{AgentMessage, ModelStreamEvent};
use serde_json::json;

#[test]
fn inline_tool_disclosure_survives_preview_commit_and_is_scoped_to_selected_session() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
    let owner = "tool-disclosure-fixture";
    let call_id = "call:árbol🦀";
    app.view.selected_session = Some(owner.into());
    app.view.show_side_pane = false;
    app.projection.apply_kind(&AgentEventKind::TurnStarted {
        prompt: "Inspeccioná".into(),
    });
    app.projection
        .apply_kind(&AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "fixture".into(),
        });
    app.projection
        .apply_kind(&AgentEventKind::ModelStreamUpdate {
            step: 1,
            update: ModelStreamEvent::ToolCallDelta {
                item_id: "item".into(),
                call_id: Some(call_id.into()),
                name: Some("read_file".into()),
                arguments_delta: r#"{"path":"main.rs"}"#.into(),
            },
        });
    app.projection.sync_into(&mut app.view.transcript);
    let action = |owner: &str, call_id: &str| UiAction::Clicked {
        id: format!("conversation_detail:{owner}:tool:{call_id}"),
    };
    let context = egui::Context::default();
    let detail = format!("tool:{call_id}");
    let before = app.view.transcript.clone();
    app.apply_action(action(owner, call_id), &context);
    assert!(app.view.conversation_details.is_expanded(&detail, false));
    assert!(!app.view.show_side_pane);
    assert_eq!(
        app.view.transcript, before,
        "disclosure has no model/tool effects"
    );
    let call = ToolCall {
        id: call_id.into(),
        name: "read_file".into(),
        arguments: json!({"path":"main.rs"}),
    };
    app.projection
        .apply_kind(&AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call.clone()]),
            response_id: None,
            usage: None,
        });
    app.projection
        .apply_kind(&AgentEventKind::ToolStarted { tool_call: call });
    app.projection.sync_into(&mut app.view.transcript);
    assert!(app.view.conversation_details.is_expanded(&detail, false));
    app.apply_action(action(owner, call_id), &context);
    assert!(!app.view.conversation_details.is_expanded(&detail, false));
    app.apply_action(action("other-owner", call_id), &context);
    app.apply_action(action(owner, "not-in-transcript"), &context);
    assert!(!app.view.conversation_details.is_expanded(&detail, false));
    assert!(!app
        .view
        .conversation_details
        .is_expanded("tool:not-in-transcript", false));
    assert!(!app.view.show_side_pane);
    assert!(app.active_run.is_none());
    assert!(app.active_resume.is_none());
}

#[test]
fn provisional_disclosure_choice_migrates_when_call_identity_arrives_in_a_burst() {
    for manually_open in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.selected_session = Some("owner".into());
        app.projection
            .apply_kind(&AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "fixture".into(),
            });
        app.projection
            .apply_kind(&AgentEventKind::ModelStreamUpdate {
                step: 1,
                update: ModelStreamEvent::ToolCallDelta {
                    item_id: "item".into(),
                    call_id: None,
                    name: Some("read_file".into()),
                    arguments_delta: "{".into(),
                },
            });
        app.publish_transcript();
        let provisional = app
            .view
            .transcript
            .iter()
            .find_map(|row| match row {
                TranscriptItem::ToolPreview { id, .. } => Some(id.clone()),
                _ => None,
            })
            .unwrap();
        app.view.conversation_details.toggle(&provisional, false);
        if !manually_open {
            app.view.conversation_details.toggle(&provisional, false);
        }
        let call = ToolCall {
            id: "call".into(),
            name: "read_file".into(),
            arguments: json!({}),
        };
        // The call ID, model commit and execution can all arrive before the
        // next paint. The alias must survive replacement of the preview row.
        for kind in [
            AgentEventKind::ModelStreamUpdate {
                step: 1,
                update: ModelStreamEvent::ToolCallReady {
                    item_id: "item".into(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                },
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted { tool_call: call },
        ] {
            app.projection.apply_kind(&kind);
        }
        app.publish_transcript();
        assert_eq!(
            app.view
                .conversation_details
                .is_expanded("tool:call", !manually_open),
            manually_open
        );
        assert_eq!(
            app.view
                .conversation_details
                .is_expanded(&provisional, !manually_open),
            !manually_open,
            "old alias removed"
        );
        app.view.selected_session = Some("other".into());
        app.publish_transcript();
        assert!(!app
            .view
            .conversation_details
            .is_expanded("tool:call", false));
    }
}

#[test]
fn expanded_command_output_follows_its_tail_and_preserves_manual_scroll() {
    use openide_core::OutputStream;
    use openide_ui::{Point, UiEvent};
    let temp = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
    // Exercise delivery/presentation independently of persistence (real durable
    // command/Resume tests live in the harness and block process exit).
    app.store = None;
    app.view.selected_session = Some("owner".into());
    app.tree.set_reduced_motion(true);
    app.projection.apply_kind(&AgentEventKind::ToolStarted {
        tool_call: ToolCall {
            id: "command".into(),
            name: "run_command".into(),
            arguments: json!({"command":"fixture"}),
        },
    });
    app.publish_transcript();
    app.view.conversation_details.toggle("tool:command", false);
    let layout = |app: &mut AgentsWindow| {
        let output = crate::session_surface::tool_partial_output(&app.view, 0, "command");
        app.tree
            .set_root(crate::session_surface::tool_line_with_output(
                &app.view.transcript[0],
                &app.view,
                false,
                false,
                output,
                None,
            ));
        app.tree.layout(Size::new(680.0, 700.0))
    };
    let id = "tool-result:owner:command:capture";
    layout(&mut app);
    let deliver = |app: &mut AgentsWindow, sequence, text: String| {
        assert!(app.process_event(AgentEvent::new(
            "owner",
            sequence,
            AgentEventKind::ToolOutputDelta {
                tool_call_id: "command".into(),
                stream: OutputStream::Stdout,
                text,
            }
        )));
        app.publish_transcript();
    };
    deliver(
        &mut app,
        1,
        (0..70).map(|i| format!("line {i}\n")).collect(),
    );
    layout(&mut app);
    let before = app.tree.scroll_offset(id);
    assert!(before > 0.0, "the first visible block follows its tail");
    assert!(app.tree.is_scroll_at_end(id, 1.0));
    deliver(&mut app, 2, "new line\n".repeat(12));
    let frame = layout(&mut app);
    assert!(app.tree.is_scroll_at_end(id, 1.0));
    assert!(app.tree.scroll_offset(id) > before);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap()
        .rect;
    app.tree.handle_event(UiEvent::Scroll {
        position: Point::new(rect.x + 20.0, rect.y + 20.0),
        delta_y: 180.0,
    });
    layout(&mut app);
    let reading_offset = app.tree.scroll_offset(id);
    assert!(!app.tree.is_scroll_at_end(id, 24.0));
    deliver(&mut app, 3, "another new line\n".repeat(12));
    layout(&mut app);
    assert!((app.tree.scroll_offset(id) - reading_offset).abs() < 0.1);
}
