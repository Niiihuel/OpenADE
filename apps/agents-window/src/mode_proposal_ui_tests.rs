use super::*;
use crate::transcript::{ActivityStatus, ModelStreamStatus, TranscriptItem};
use crate::view::ViewState;
use openide_ui::{DrawCommand, Frame, Size, UiTree};
use serde_json::json;

const OWNER: &str = "mode-fixture";
const REQUEST: &str = "request-1";
const REASON: &str = "El cambio toca el formato del journal y conviene acordar el diseño antes de editar.";

fn arguments(mode: &str) -> Value {
    json!({"mode": mode, "reason": REASON})
}

fn permission(mode: &str) -> PermissionView {
    PermissionView {
        request_id: REQUEST.into(),
        tool_name: TOOL.into(),
        arguments: serde_json::to_string_pretty(&arguments(mode)).unwrap(),
    }
}

fn frame(element: Element, width: f32) -> Frame {
    let mut tree = UiTree::new(element);
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 500.0))
}

fn sources(frame: &Frame) -> Vec<String> {
    frame
        .text_layouts()
        .iter()
        .map(|layout| layout.source.clone())
        .collect()
}

#[test]
fn the_card_names_the_mode_gives_the_reason_and_offers_exactly_two_decisions() {
    for width in [360.0, 900.0] {
        for (mode, title) in [
            ("plan", "El agente propone pasar a Modo Plan"),
            ("build", "El agente propone salir de Modo Plan"),
        ] {
            let frame = frame(card(&permission(mode)), width);
            let text = sources(&frame);
            assert!(text.iter().any(|source| source == title));
            assert!(text.iter().any(|source| source == REASON));
            assert!(text.iter().any(|source| source.contains("Los permisos no cambian")));
            let buttons = frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with("mode_proposal_"))
                .map(|region| region.id.clone())
                .collect::<Vec<_>>();
            assert_eq!(buttons.len(), 2);
            assert!(buttons.contains(&action(REQUEST, "accept")));
            assert!(buttons.contains(&action(REQUEST, "reject")));
            for region in &frame.regions {
                assert!(region.rect.x + region.rect.width <= width + 0.5);
            }
        }
    }
    assert_eq!(proposed(&json!({"mode": "allow_all"})), None);
    assert_eq!(proposed(&json!({})), None);
}

fn preview(arguments: &str) -> TranscriptItem {
    TranscriptItem::ToolPreview {
        id: "model:preview".into(),
        step: 1,
        item_id: "item".into(),
        call_id: Some("call".into()),
        name: Some(TOOL.into()),
        arguments: arguments.into(),
        input_complete: false,
        status: ModelStreamStatus::Receiving,
        truncated: false,
    }
}

fn tool(mode: &str, activity: ActivityStatus, output: Option<&str>) -> TranscriptItem {
    TranscriptItem::Tool {
        id: "call".into(),
        name: TOOL.into(),
        arguments: arguments(mode),
        output: output.map(str::to_owned),
        is_error: false,
        activity,
    }
}

#[test]
fn the_streamed_proposal_is_the_cards_skeleton_and_the_decision_is_one_line() {
    let state = crate::view::tests::empty_state();
    let streaming = frame(
        crate::session_surface::model_stream_item(&preview("{\"mo"), 0, &state, 900.0),
        900.0,
    );
    let bars = |frame: &Frame| {
        frame
            .commands
            .iter()
            .filter(|command| matches!(command, DrawCommand::FillRect { rect, .. }
                if rect.height == crate::tool_card_skeleton::BAR))
            .count()
    };
    assert_eq!(bars(&streaming), 3);
    let named = frame(
        crate::session_surface::model_stream_item(
            &preview(r#"{"mode":"plan","reason":"El cam"#),
            0,
            &state,
            900.0,
        ),
        900.0,
    );
    assert_eq!(bars(&named), 2, "the mode has arrived, the reason has not");
    assert!(sources(&named)
        .iter()
        .any(|source| source == "El agente propone pasar a Modo Plan"));

    let text = |element: Element| {
        frame(element, 360.0)
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<String>()
    };
    let accepted = "The user accepted. Plan mode starts with the user's next message.";
    assert!(text(resolved(&arguments("plan"), accepted)).contains("Modo Plan aceptado"));
    assert!(text(resolved(&arguments("build"), "The user rejected the proposal."))
        .contains("rechazada"));
}

fn state(items: Vec<TranscriptItem>) -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.selected_session = Some(OWNER.into());
    state.conversation_details.sync(Some(OWNER));
    state.transcript = vec![TranscriptItem::User {
        content: "Cambiá el formato del journal.".into(),
        images: vec![],
    }];
    state.transcript.extend(items);
    state
}

#[test]
#[ignore = "export the mode proposal card states for visual review"]
fn export_mode_proposal_visual_review() {
    for width in [360.0_f32, 900.0] {
        let mut shots = Vec::new();
        let mut streaming = state(vec![preview("{\"mo")]);
        streaming.running = true;
        shots.push(("mode-streaming", streaming));
        let mut pending = state(vec![tool("plan", ActivityStatus::WaitingPermission, None)]);
        pending.running = true;
        pending.pending_permissions.push(permission("plan"));
        shots.push(("mode-pending", pending));
        shots.push((
            "mode-accepted",
            state(vec![tool(
                "plan",
                ActivityStatus::Completed,
                Some("The user accepted. Plan mode starts with the user's next message."),
            )]),
        ));
        shots.push((
            "mode-rejected",
            state(vec![tool(
                "build",
                ActivityStatus::Completed,
                Some("The user rejected the proposal."),
            )]),
        ));
        let mut restarted = state(vec![tool("plan", ActivityStatus::PermissionInterrupted, None)]);
        restarted
            .session_recovery
            .insert(OWNER.into(), crate::view::SessionRecoveryView::Question);
        shots.push(("mode-after-restart", restarted));
        for (name, state) in shots {
            let size = Size::new(width, 520.0);
            let root = Element::panel(crate::session_surface::build_session_surface(
                &state, width, false, false,
            ))
            .background(theme::BACKGROUND);
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            let frame = tree.layout(size);
            crate::view::ide_visual_tests::export_shot(
                &frame,
                size,
                "agent-tools",
                &format!("{name}-{width:.0}"),
            );
            context.end_pass().textures_delta.clear();
        }
    }
}
