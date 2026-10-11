//! Synthetic fixtures for the cards a streamed call is drawn as; no provider,
//! journal or process access.

use super::*;
use openide_core::{PlanSnapshot, PlanStep};
use openide_ui::{DrawCommand, Frame, Size, UiTree};
use serde_json::json;
use std::time::{Duration, Instant};

const OWNER: &str = "skeleton-fixture";

fn state() -> ViewState {
    let mut state = super::tests::state();
    state.selected_session = Some(OWNER.into());
    state.conversation_details.sync(Some(OWNER));
    state.transcript.clear();
    state
}

pub(super) fn preview(name: Option<&str>, arguments: &str, status: ModelStreamStatus) -> TranscriptItem {
    TranscriptItem::ToolPreview {
        id: "model:preview".into(),
        step: 1,
        item_id: "item".into(),
        call_id: name.map(|_| "call".into()),
        name: name.map(str::to_owned),
        arguments: arguments.into(),
        input_complete: status == ModelStreamStatus::Committed,
        status,
        truncated: false,
    }
}

/// Skeleton bars: the body lines and the header title.
fn bars(frame: &Frame) -> Vec<(openide_ui::Rect, openide_ui::Color)> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRect {
                rect,
                radius,
                color,
            } if (rect.height, *radius) == (tool_card_skeleton::BAR, tool_card_skeleton::BAR / 2.0)
                || (rect.height, *radius) == (12.0, 6.0) =>
            {
                Some((*rect, *color))
            }
            _ => None,
        })
        .collect()
}

/// The card over a marker row, so the marker's top is the card's height.
fn measured(card: Element, width: f32) -> (f32, Frame) {
    let root = Element::column(vec![
        card,
        Element::clickable("after", Element::spacer().size(None, Some(4.0))),
    ]);
    let (context, mut tree) = super::file_activity_visual_tests::native_tree(root);
    let frame = tree.layout(Size::new(width, 600.0));
    context.end_pass().textures_delta.clear();
    let after = frame
        .regions
        .iter()
        .find(|region| region.id == "after")
        .unwrap()
        .rect
        .y;
    (after, frame)
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
        .join(" ")
}

#[test]
fn a_streamed_command_is_the_terminal_card_it_becomes_at_the_same_height() {
    let state = state();
    for width in [360.0, 900.0] {
        let empty = preview(Some("run_command"), "{\"comm", ModelStreamStatus::Receiving);
        let (skeleton_height, skeleton) =
            measured(model_stream_item(&empty, 0, &state, width), width);
        // The title and the prompt line are both still to come.
        assert_eq!(bars(&skeleton).len(), 2);
        assert!(text(&skeleton).contains("Preparando"));
        assert!(skeleton.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::Terminal,
                ..
            }
        )));

        let known = preview(
            Some("run_command"),
            r#"{"command":"cargo test --workspace","cwd":"#,
            ModelStreamStatus::Receiving,
        );
        let (known_height, known_frame) =
            measured(model_stream_item(&known, 0, &state, width), width);
        assert!(bars(&known_frame).is_empty(), "received text replaces its bar");
        assert!(text(&known_frame).contains("$ cargo test --workspace"));

        let running = TranscriptItem::Tool {
            id: "call".into(),
            name: "run_command".into(),
            arguments: json!({"command": "cargo test --workspace"}),
            output: None,
            is_error: false,
            activity: ActivityStatus::Running,
        };
        let (running_height, _) = measured(tool_line(&running, &state, width < 470.0), width);
        assert_eq!(skeleton_height, known_height);
        assert_eq!(known_height, running_height, "no jump when the command starts");
    }
}

#[test]
fn a_streamed_plan_is_the_plan_card_with_step_rows_of_the_real_height() {
    let state = state();
    let steps = |count: usize| PlanSnapshot {
        explanation: None,
        plan: (0..count)
            .map(|index| PlanStep {
                step: format!("Paso {index}"),
                status: PlanStepStatus::Pending,
            })
            .collect(),
    };
    for width in [360.0, 900.0] {
        let writing = preview(Some("update_plan"), r#"{"plan":[{"step":"Pa"#, ModelStreamStatus::Receiving);
        let (skeleton_height, skeleton) =
            measured(model_stream_item(&writing, 0, &state, width), width);
        assert!(text(&skeleton).contains("Plan actualizado"));
        // Three step lines and the count.
        assert_eq!(bars(&skeleton).len(), 4);
        let real = session_plan_card(
            OWNER,
            &SessionPlanView {
                snapshot: steps(3),
                can_build: false,
            },
            "Plan actualizado",
        );
        let (real_height, _) = measured(real, width);
        assert_eq!(skeleton_height, real_height);

        // Once the list is complete the preview already is the final card.
        let complete = preview(
            Some("update_plan"),
            &serde_json::to_string(&steps(2)).unwrap(),
            ModelStreamStatus::Committed,
        );
        let (_, frame) = measured(model_stream_item(&complete, 0, &state, width), width);
        assert!(bars(&frame).is_empty());
        assert!(text(&frame).contains("2. Paso 1") || frame
            .text_layouts()
            .iter()
            .any(|layout| layout.source == "2. Paso 1"));
    }
}

#[test]
fn an_unnamed_call_holds_its_title_and_other_tools_stay_the_row_they_become() {
    let state = state();
    let unnamed = preview(None, "", ModelStreamStatus::Receiving);
    let (height, frame) = measured(model_stream_item(&unnamed, 0, &state, 900.0), 900.0);
    assert_eq!(bars(&frame).len(), 1);
    assert!(!text(&frame).contains("Herramienta"));

    let mcp = preview(Some("mcp__docs__lookup"), "{\"que", ModelStreamStatus::Receiving);
    let (mcp_height, mcp_frame) = measured(model_stream_item(&mcp, 0, &state, 900.0), 900.0);
    assert!(bars(&mcp_frame).is_empty());
    assert_eq!(height, mcp_height);
    let running = TranscriptItem::Tool {
        id: "call".into(),
        name: "mcp__docs__lookup".into(),
        arguments: json!({"query": "x"}),
        output: None,
        is_error: false,
        activity: ActivityStatus::Running,
    };
    let (running_height, _) = measured(tool_line(&running, &state, false), 900.0);
    assert_eq!(mcp_height, running_height);

    // A call that will never run is not drawn as a loading card.
    for status in [
        ModelStreamStatus::Cancelled,
        ModelStreamStatus::Failed,
        ModelStreamStatus::Interrupted,
        ModelStreamStatus::Discarded,
    ] {
        for (name, arguments) in [(Some("run_command"), "{"), (Some("update_plan"), "{"), (None, "")] {
            let item = preview(name, arguments, status);
            let (_, frame) = measured(model_stream_item(&item, 0, &state, 900.0), 900.0);
            assert!(bars(&frame).is_empty(), "{status:?} {name:?}");
        }
    }
}

#[test]
fn skeleton_bars_keep_their_identity_across_rebuilds_and_respect_reduced_motion() {
    let state = state();
    let now = Instant::now();
    let viewport = Size::new(360.0, 300.0);
    let first_item = preview(Some("update_plan"), "{", ModelStreamStatus::Receiving);
    let mut tree = UiTree::new(model_stream_item(&first_item, 0, &state, 360.0));
    let first = tree.layout_at(viewport, now);
    assert!(tree.is_animating());
    // More of the call arrives: the rebuilt bars continue the same pulse
    // instead of starting it again.
    let later_item = preview(Some("update_plan"), r#"{"plan":[{"step":"Uno"#, ModelStreamStatus::Receiving);
    tree.set_root(model_stream_item(&later_item, 0, &state, 360.0));
    let second = tree.layout_at(viewport, now + Duration::from_millis(400));
    let colors = |frame: &Frame| bars(frame).into_iter().map(|(_, color)| color).collect::<Vec<_>>();
    assert_ne!(colors(&first), colors(&second));
    let mut fresh = UiTree::new(model_stream_item(&later_item, 0, &state, 360.0));
    let restarted = fresh.layout_at(viewport, now + Duration::from_millis(400));
    assert_ne!(colors(&second), colors(&restarted));
    tree.set_reduced_motion(true);
    tree.layout_at(viewport, now + Duration::from_millis(500));
    assert!(!tree.is_animating());
}

#[test]
#[ignore = "export tool card skeletons for visual review"]
fn export_tool_card_skeletons_visual_review() {
    let state = state();
    let cases = [
        ("unnamed", preview(None, "", ModelStreamStatus::Receiving)),
        ("command-skeleton", preview(Some("run_command"), "{", ModelStreamStatus::Receiving)),
        (
            "command-received",
            preview(
                Some("run_command"),
                r#"{"command":"cargo test -p agents-window --no-fail-fast"}"#,
                ModelStreamStatus::Committed,
            ),
        ),
        ("plan-skeleton", preview(Some("update_plan"), "{", ModelStreamStatus::Receiving)),
        (
            "plan-received",
            preview(
                Some("update_plan"),
                r#"{"plan":[{"step":"Leer el módulo de sesiones","status":"completed"},{"step":"Agregar la herramienta","status":"in_progress"},{"step":"Probar la reanudación","status":"pending"}]}"#,
                ModelStreamStatus::Committed,
            ),
        ),
        ("mcp", preview(Some("mcp__docs__lookup"), r#"{"query":"limn skeleton"}"#, ModelStreamStatus::Receiving)),
        ("subagent", preview(Some("spawn_agent"), "{", ModelStreamStatus::Receiving)),
        ("file-edit", preview(Some("write_file"), r#"{"path":"src/main.rs","content":""#, ModelStreamStatus::Receiving)),
    ];
    for width in [360.0, 900.0] {
        let mut expanded = self::state();
        expanded.conversation_details.set_expanded("tool:call", true);
        let rows = cases
            .iter()
            .map(|(_, item)| {
                let state = if matches!(item, TranscriptItem::ToolPreview { name: Some(name), .. } if name == "write_file") {
                    &expanded
                } else {
                    &state
                };
                model_stream_item(item, 0, state, width - 32.0)
            })
            .collect();
        let size = Size::new(width, 640.0);
        let root = Element::panel(Element::column(rows).gap(14.0).padding(16.0))
            .background(theme::BACKGROUND);
        let (context, mut tree) = super::file_activity_visual_tests::native_tree(root);
        let frame = tree.layout(size);
        crate::view::ide_visual_tests::export_shot(
            &frame,
            size,
            "agent-tools",
            &format!("skeletons-{width:.0}"),
        );
        context.end_pass().textures_delta.clear();
    }
}
