use super::*;
use crate::transcript::{ActivityStatus, ModelStreamStatus, TranscriptItem};
use openide_ui::{DrawCommand, Frame, Size, UiTree};

const OWNER: &str = "question-fixture";
const REQUEST: &str = "request-1";

fn arguments(count: usize) -> Value {
    let all = [
        json!({
            "header": "Almacenamiento",
            "question": "¿Dónde guardamos las sesiones del agente para poder reanudarlas después de reiniciar la aplicación?",
            "options": [
                {"label": "SQLite (recomendado)", "description": "Un solo archivo con consultas rápidas e índices"},
                {"label": "Archivos JSONL", "description": "Legibles y fáciles de versionar"},
                {"label": "Memoria", "description": "Sin persistencia entre reinicios"}
            ],
            "multi_select": false
        }),
        json!({
            "header": "Plataformas",
            "question": "¿Qué plataformas cubrimos primero?",
            "options": [{"label": "Linux"}, {"label": "Windows"}, {"label": "macOS"}],
            "multi_select": true
        }),
        json!({
            "header": "Pruebas",
            "question": "¿Agrego pruebas de integración?",
            "options": [{"label": "Sí"}, {"label": "No"}],
            "multi_select": false
        }),
    ];
    json!({"questions": all[..count].to_vec()})
}

fn state(count: usize) -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.selected_session = Some(OWNER.into());
    state.conversation_details.sync(Some(OWNER));
    state.pending_permissions.push(PermissionView {
        request_id: REQUEST.into(),
        tool_name: TOOL.into(),
        arguments: serde_json::to_string_pretty(&arguments(count)).unwrap(),
    });
    state
}

fn texts(frame: &Frame) -> String {
    let mut all = frame
        .text_layouts()
        .iter()
        .map(|layout| layout.source.clone())
        .collect::<Vec<_>>();
    all.extend(frame.commands.iter().filter_map(|command| match command {
        DrawCommand::Text { text, .. } => Some(text.clone()),
        _ => None,
    }));
    all.join(" | ")
}

fn frame(element: Element, width: f32) -> Frame {
    let mut tree = UiTree::new(element);
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 700.0))
}

#[test]
fn one_question_is_shown_at_a_time_with_its_position_and_the_right_controls() {
    for width in [360.0, 900.0] {
        let mut state = state(3);
        let first = frame(card(&state.pending_permissions[0], &state, width < 470.0), width);
        let text = texts(&first);
        assert!(text.contains("Almacenamiento") && text.contains("1 de 3"));
        assert!(text.contains("SQLite (recomendado)") && text.contains("Un solo archivo"));
        assert!(!text.contains("Plataformas"));
        let has = |frame: &Frame, id: String| frame.regions.iter().any(|region| region.id == id);
        assert!(has(&first, action(REQUEST, "next")));
        assert!(has(&first, action(REQUEST, "skip")));
        assert!(!has(&first, action(REQUEST, "back")));
        assert!(!has(&first, action(REQUEST, "send")));
        assert!(has(&first, text_id(REQUEST, 0)));
        for region in &first.regions {
            assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.5);
        }

        let draft = state.question_drafts.entry(REQUEST.into()).or_default();
        draft.turn(3, true);
        draft.turn(3, true);
        draft.turn(3, true);
        let last = frame(card(&state.pending_permissions[0], &state, width < 470.0), width);
        assert!(texts(&last).contains("3 de 3") && texts(&last).contains("Pruebas"));
        assert!(has(&last, action(REQUEST, "back")));
        // Nothing answered yet: sending is disabled until something is.
        assert!(!has(&last, action(REQUEST, "send")));
        state
            .question_drafts
            .get_mut(REQUEST)
            .unwrap()
            .pick(&parse(&arguments(3)), 2, 0);
        let last = frame(card(&state.pending_permissions[0], &state, width < 470.0), width);
        assert!(has(&last, action(REQUEST, "send")));
        assert!(!has(&last, action(REQUEST, "next")));
    }
    // A single question needs no position or paging.
    let state = state(1);
    let only = frame(card(&state.pending_permissions[0], &state, false), 900.0);
    assert!(!texts(&only).contains("1 de 1"));
    assert!(only.regions.iter().any(|region| region.id == pick_action(REQUEST, 0, 0)));
    assert!(!only.regions.iter().any(|region| region.id == action(REQUEST, "next")));
}

#[test]
fn single_choice_replaces_and_multiple_choice_toggles() {
    let questions = parse(&arguments(2));
    let mut draft = QuestionDraft::default();
    draft.pick(&questions, 0, 0);
    draft.pick(&questions, 0, 2);
    assert_eq!(draft.picked[0], BTreeSet::from([2]));
    draft.pick(&questions, 0, 2);
    assert!(draft.picked[0].is_empty());
    draft.pick(&questions, 1, 0);
    draft.pick(&questions, 1, 1);
    draft.pick(&questions, 1, 0);
    assert_eq!(draft.picked[1], BTreeSet::from([1]));
    // Out of range picks and pages are ignored.
    draft.pick(&questions, 1, 9);
    draft.pick(&questions, 7, 0);
    draft.turn(2, false);
    assert_eq!((draft.page, draft.picked.len()), (0, 2));
    assert!(draft.any_answer(2));
    draft.write(2, 0, &"ñ".repeat(MAX_TEXT_BYTES));
    assert!(draft.text[0].len() <= MAX_TEXT_BYTES);
}

#[test]
fn an_untouched_question_is_reported_as_skipped_never_as_an_empty_choice() {
    let questions = parse(&arguments(2));
    let mut draft = QuestionDraft::default();
    draft.write(2, 1, " sólo Linux por ahora ");
    let answer: Value = serde_json::from_str(&answer_json(&questions, &draft)).unwrap();
    assert_eq!(answer["answered_by"], "user");
    assert_eq!(answer["answers"][0]["skipped"], true);
    assert!(answer["answers"][0].get("selected").is_none());
    assert_eq!(answer["answers"][1]["selected"], json!([]));
    assert_eq!(answer["answers"][1]["other_answer"], "sólo Linux por ahora");
}

#[test]
fn the_finished_call_shows_what_the_user_answered() {
    let output = json!({"answered_by": "user", "answers": [
        {"header": "Almacenamiento", "selected": ["SQLite (recomendado)"]},
        {"header": "Plataformas", "selected": ["Linux", "macOS"], "other_answer": "FreeBSD después"},
        {"header": "Pruebas", "skipped": true}
    ]})
    .to_string();
    let text = texts(&frame(answered(&arguments(3), &output), 360.0));
    assert!(text.contains("Tu respuesta"));
    assert!(text.contains("SQLite (recomendado)"));
    assert!(text.contains("Linux · macOS · FreeBSD después"));
    assert!(text.contains("Omitida"));
    let skipped = texts(&frame(
        answered(&arguments(1), "The user skipped this question without answering."),
        360.0,
    ));
    assert!(skipped.contains("Pregunta omitida"));
}

fn preview(arguments: &str, status: ModelStreamStatus) -> TranscriptItem {
    TranscriptItem::ToolPreview {
        id: "model:preview".into(),
        step: 1,
        item_id: "item".into(),
        call_id: Some("call".into()),
        name: Some(TOOL.into()),
        arguments: arguments.into(),
        input_complete: status == ModelStreamStatus::Committed,
        status,
        truncated: false,
    }
}

fn tool(activity: ActivityStatus, output: Option<String>) -> TranscriptItem {
    TranscriptItem::Tool {
        id: "call".into(),
        name: TOOL.into(),
        arguments: arguments(3),
        output,
        is_error: false,
        activity,
    }
}

#[test]
fn the_open_question_is_one_card_and_the_streamed_call_is_its_skeleton() {
    let mut state = state(3);
    state.transcript = vec![tool(ActivityStatus::WaitingPermission, None)];
    state.running = true;
    let surface = frame(
        crate::session_surface::build_session_surface(&state, 900.0, false, false),
        900.0,
    );
    let text = texts(&surface);
    assert_eq!(
        surface
            .text_layouts()
            .iter()
            .filter(|layout| layout.source == "Almacenamiento")
            .count(),
        1
    );
    assert!(!text.contains("Permiso pendiente"));
    assert!(!text.contains("Aprobación pendiente"));

    // While the call streams: the card's frame, with what has arrived.
    let streaming = crate::view::tests::empty_state();
    let empty = frame(
        crate::session_surface::model_stream_item(
            &preview("{\"quest", ModelStreamStatus::Receiving),
            0,
            &streaming,
            900.0,
        ),
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
    assert_eq!(bars(&empty), 4, "title, question and two options");
    let partial = frame(
        crate::session_surface::model_stream_item(
            &preview(
                r#"{"questions":[{"header":"Almacenamiento","question":"¿Dónde?","options":[{"label":"SQLite"}"#,
                ModelStreamStatus::Receiving,
            ),
            0,
            &streaming,
            900.0,
        ),
        900.0,
    );
    // Arguments arrive as one top-level field, so the card stays a skeleton
    // until the whole list is in.
    assert_eq!(bars(&partial), 4);
}

#[test]
#[ignore = "export the question card states for visual review"]
fn export_question_card_visual_review() {
    let answered_output = json!({"answered_by": "user", "answers": [
        {"header": "Almacenamiento", "selected": ["SQLite (recomendado)"]},
        {"header": "Plataformas", "selected": ["Linux", "macOS"], "other_answer": "FreeBSD más adelante"},
        {"header": "Pruebas", "skipped": true}
    ]})
    .to_string();
    for width in [360.0_f32, 900.0] {
        let user = TranscriptItem::User {
            content: "Agregá persistencia a las sesiones.".into(),
            images: vec![],
        };
        let mut shots: Vec<(&str, ViewState)> = Vec::new();

        let mut streaming = crate::view::tests::empty_state();
        streaming.selected_session = Some(OWNER.into());
        streaming.running = true;
        streaming.transcript = vec![user.clone(), preview("{\"quest", ModelStreamStatus::Receiving)];
        shots.push(("question-streaming", streaming));

        let mut pending = state(3);
        pending.running = true;
        pending.transcript = vec![user.clone(), tool(ActivityStatus::WaitingPermission, None)];
        pending
            .question_drafts
            .entry(REQUEST.into())
            .or_default()
            .pick(&parse(&arguments(3)), 0, 0);
        shots.push(("question-pending", pending));

        let mut multi = state(3);
        multi.running = true;
        multi.transcript = vec![user.clone(), tool(ActivityStatus::WaitingPermission, None)];
        let draft = multi.question_drafts.entry(REQUEST.into()).or_default();
        draft.turn(3, true);
        draft.pick(&parse(&arguments(3)), 1, 0);
        draft.pick(&parse(&arguments(3)), 1, 2);
        draft.write(3, 1, "FreeBSD más adelante");
        shots.push(("question-pending-multi", multi));

        let mut done = crate::view::tests::empty_state();
        done.selected_session = Some(OWNER.into());
        done.transcript = vec![
            user.clone(),
            tool(ActivityStatus::Completed, Some(answered_output.clone())),
        ];
        shots.push(("question-answered", done));

        let mut skipped = crate::view::tests::empty_state();
        skipped.selected_session = Some(OWNER.into());
        skipped.transcript = vec![
            user.clone(),
            tool(
                ActivityStatus::Completed,
                Some("The user skipped this question without answering.".into()),
            ),
        ];
        shots.push(("question-skipped", skipped));

        // After a restart the turn is interrupted: the recovery card offers
        // to resume, and resuming brings the same question back.
        let mut restarted = crate::view::tests::empty_state();
        restarted.selected_session = Some(OWNER.into());
        restarted.transcript = vec![user.clone(), tool(ActivityStatus::PermissionInterrupted, None)];
        restarted
            .session_recovery
            .insert(OWNER.into(), crate::view::SessionRecoveryView::Question);
        shots.push(("question-after-restart", restarted));

        for (name, state) in shots {
            let size = Size::new(width, 760.0);
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
