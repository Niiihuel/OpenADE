use super::*;
use crate::transcript::TranscriptProjection;
use openide_core::{
    AgentEvent, AgentEventKind, AgentMessage, Checkpoint, CheckpointBudgetSnapshot,
    CheckpointCompatibility, CheckpointFileStore, PermissionResolution, SessionStore, ToolCall,
};
use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};
use serde_json::json;
use std::time::{Duration, Instant};

fn request(step: usize) -> AgentEventKind {
    AgentEventKind::ModelRequestStarted {
        step,
        model: "exact-model-id".into(),
    }
}

fn response(step: usize) -> AgentEventKind {
    AgentEventKind::ModelResponseCommitted {
        step,
        assistant: AgentMessage::assistant("Respuesta", vec![]),
        response_id: Some(format!("response-{step}")),
        usage: None,
    }
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: json!({"path":"src/archivo.rs"}),
    }
}

fn apply(projection: &mut TranscriptProjection, sequence: u64, kind: AgentEventKind) -> AgentEvent {
    let event = AgentEvent::new("marker-session", sequence, kind);
    projection.apply_event(&event);
    event
}

fn current_model(projection: &TranscriptProjection) -> &TranscriptActivity {
    projection
        .items()
        .iter()
        .rev()
        .find_map(|item| match item {
            TranscriptItem::Activity(activity @ TranscriptActivity::Model { .. }) => Some(activity),
            _ => None,
        })
        .unwrap()
}

#[test]
fn markers_model_loading_tracks_request_stream_response_and_terminal_exactly() {
    let mut live = TranscriptProjection::new();
    let mut events = vec![
        apply(
            &mut live,
            1,
            AgentEventKind::TurnStarted {
                prompt: "Revisar".into(),
            },
        ),
        apply(&mut live, 2, request(1)),
    ];
    let mut tree = UiTree::new(activity_marker(current_model(&live)));
    let clock = Instant::now();
    let frame = tree.layout_at(Size::new(420.0, 100.0), clock);
    assert!(tree.is_animating());
    assert!(super::tests::visible_text(&frame).contains("Pensando"));
    assert!(
        matches!(current_model(&live), TranscriptActivity::Model { id, status: ActivityStatus::Running, has_text: false, .. } if id == "model-request:marker-session:2:1")
    );
    events.push(apply(
        &mut live,
        3,
        AgentEventKind::AssistantDelta {
            content: "Res".into(),
        },
    ));
    tree.set_root(activity_marker(current_model(&live)));
    let frame = tree.layout_at(Size::new(420.0, 100.0), clock + Duration::from_millis(200));
    assert!(tree.is_animating());
    assert!(super::tests::visible_text(&frame).contains("Generando respuesta"));
    events.push(apply(&mut live, 4, response(1)));
    tree.set_root(activity_marker(current_model(&live)));
    let frame = tree.layout_at(Size::new(420.0, 100.0), clock + Duration::from_millis(400));
    assert!(!tree.is_animating());
    assert!(super::tests::visible_text(&frame).contains("Respuesta recibida"));
    events.push(apply(
        &mut live,
        5,
        AgentEventKind::AssistantMessage {
            content: "Respuesta".into(),
        },
    ));
    events.push(apply(&mut live, 6, request(2)));
    // A response for the previous request cannot finish the current spinner.
    events.push(apply(&mut live, 7, response(1)));
    assert!(matches!(
        current_model(&live),
        TranscriptActivity::Model {
            step: 2,
            status: ActivityStatus::Running,
            ..
        }
    ));
    events.push(apply(&mut live, 8, AgentEventKind::TurnCancelled));
    assert!(matches!(
        current_model(&live),
        TranscriptActivity::Model {
            status: ActivityStatus::Cancelled,
            ..
        }
    ));
    assert_eq!(
        live.items(),
        TranscriptProjection::from_events(&events).items()
    );
}

#[test]
fn markers_reopen_each_effect_boundary_preserves_permissions_errors_and_uncertainty() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::open(root.path()).unwrap();
    let kinds = vec![
        AgentEventKind::TurnStarted {
            prompt: "Cambiar archivo".into(),
        },
        request(1),
        AgentEventKind::AssistantDelta {
            content: "Voy a revisar".into(),
        },
        response(1),
        AgentEventKind::ToolStarted {
            tool_call: call("change", "write_file"),
        },
        AgentEventKind::PermissionRequested {
            request_id: "approval".into(),
            tool_call: call("change", "write_file"),
        },
        AgentEventKind::PermissionResolved {
            request_id: "approval".into(),
            tool_call_id: "change".into(),
            resolution: PermissionResolution::Allowed,
        },
        AgentEventKind::ToolFinished {
            tool_call_id: "change".into(),
            output: "ERROR real del executor".into(),
            is_error: true,
        },
        AgentEventKind::TurnFailed {
            error: "Falló el turno".into(),
        },
    ];
    for cut in 2..=kinds.len() {
        let session = store.create(root.path(), "native").unwrap();
        for (index, kind) in kinds[..cut].iter().cloned().enumerate() {
            store
                .append_event(
                    &session.id,
                    &AgentEvent::new(&session.id, index as u64 + 1, kind),
                )
                .unwrap();
        }
        let reopened = SessionStore::open(root.path()).unwrap();
        let projection =
            TranscriptProjection::from_events(&reopened.load(&session.id).unwrap().unwrap().events);
        let mut tree = UiTree::new(Element::column(
            projection
                .items()
                .iter()
                .filter_map(|item| match item {
                    TranscriptItem::Activity(activity) => Some(activity_marker(activity)),
                    TranscriptItem::Tool {
                        id,
                        name,
                        activity,
                        is_error,
                        ..
                    } => Some(tool_activity_marker(
                        Some(&session.id),
                        id,
                        name,
                        *activity,
                        false,
                        *is_error,
                    )),
                    _ => None,
                })
                .collect(),
        ));
        let frame = tree.layout(Size::new(360.0, 700.0));
        assert!(
            !tree.is_animating(),
            "cut {cut} must never animate a reopened operation"
        );
        if cut < 4 {
            assert!(matches!(
                current_model(&projection),
                TranscriptActivity::Model {
                    status: ActivityStatus::Interrupted,
                    ..
                }
            ));
        } else {
            assert!(matches!(
                current_model(&projection),
                TranscriptActivity::Model {
                    status: ActivityStatus::Completed,
                    ..
                }
            ));
        }
        if cut >= 5 {
            let tool = projection
                .items()
                .iter()
                .find(|item| matches!(item, TranscriptItem::Tool { .. }))
                .unwrap();
            let expected = match cut {
                6 => ActivityStatus::PermissionInterrupted,
                8.. => ActivityStatus::Failed,
                _ => ActivityStatus::Interrupted,
            };
            assert!(matches!(tool, TranscriptItem::Tool { activity, .. } if *activity == expected));
            let text = frame
                .text_layouts()
                .iter()
                .map(|layout| layout.source.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if cut == 6 {
                assert!(text.contains("permiso pendiente"), "{text}");
            }
            if cut >= 8 {
                assert!(
                    matches!(tool, TranscriptItem::Tool { output: Some(output), is_error: true, .. } if output == "ERROR real del executor")
                );
            }
        }
    }
}

#[test]
fn markers_new_turn_does_not_reactivate_old_uncertain_tool_and_recovery_confirms_exact_result() {
    let mut live = TranscriptProjection::new();
    apply(
        &mut live,
        1,
        AgentEventKind::TurnStarted {
            prompt: "Primero".into(),
        },
    );
    apply(
        &mut live,
        2,
        AgentEventKind::ToolStarted {
            tool_call: call("uncertain", "run_command"),
        },
    );
    apply(
        &mut live,
        3,
        AgentEventKind::TurnFailed {
            error: "Corte tras efecto externo".into(),
        },
    );
    apply(
        &mut live,
        4,
        AgentEventKind::TurnStarted {
            prompt: "Segundo".into(),
        },
    );
    assert!(matches!(
        &live.items()[1],
        TranscriptItem::Tool {
            activity: ActivityStatus::Interrupted,
            ..
        }
    ));
    let mut state = super::tests::state();
    state.running = true;
    state.transcript = live.items().to_vec();
    let mut tree = UiTree::new(work_group(&state.transcript[1..2], &state, false));
    tree.layout(Size::new(420.0, 240.0));
    assert!(!tree.is_animating());
    let events = vec![
        AgentEvent::new(
            "recovery",
            1,
            AgentEventKind::TurnStarted {
                prompt: "Cambiar".into(),
            },
        ),
        AgentEvent::new(
            "recovery",
            2,
            AgentEventKind::ToolStarted {
                tool_call: call("change", "write_file"),
            },
        ),
        AgentEvent::new(
            "recovery",
            3,
            AgentEventKind::ToolFinished {
                tool_call_id: "change".into(),
                output: "Verificado por el usuario".into(),
                is_error: false,
            },
        ),
    ];
    let recovered = TranscriptProjection::from_events(&events);
    assert!(
        matches!(&recovered.items()[1], TranscriptItem::Tool { activity: ActivityStatus::Completed, output: Some(output), .. } if output == "Verificado por el usuario")
    );
}

#[test]
fn markers_compaction_success_requires_committed_checkpoint_and_survives_store_reopen() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::open(root.path().join("history")).unwrap();
    let session = store.create(root.path(), "native").unwrap();
    let checkpoints = CheckpointFileStore::new(root.path().join("checkpoints")).unwrap();
    let checkpoint = Checkpoint {
        session_id: session.id.clone(),
        checkpoint_id: "checkpoint-marker".into(),
        covered_event_sequence: 3,
        compatibility: CheckpointCompatibility {
            provider: "fixture".into(),
            model: "exact-model-id".into(),
            context_format_key: "fixture-v1".into(),
        },
        summary: "Resumen guardado".into(),
        retained_user_messages: vec![],
        budget: CheckpointBudgetSnapshot::unverified(400, None),
    };
    let reference = checkpoints.write_candidate(&checkpoint).unwrap();
    let mut live = TranscriptProjection::new();
    for (index, kind) in [
        AgentEventKind::TurnStarted {
            prompt: "Compactar".into(),
        },
        AgentEventKind::AssistantMessage {
            content: "Contexto compactado".into(),
        },
        AgentEventKind::TurnCompleted {
            response: "Contexto compactado".into(),
            steps: 1,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let event = AgentEvent::new(&session.id, index as u64 + 1, kind);
        store.append_event(&session.id, &event).unwrap();
        live.apply_event(&event);
    }
    assert!(!live.items().iter().any(|item| matches!(
        item,
        TranscriptItem::Activity(TranscriptActivity::Checkpoint { .. })
    )));
    let event = AgentEvent::new(
        &session.id,
        4,
        AgentEventKind::CheckpointCommitted { reference },
    );
    store.append_event(&session.id, &event).unwrap();
    live.apply_event(&event);
    drop(store);
    let reopened = SessionStore::open(root.path().join("history")).unwrap();
    let events = reopened.load(&session.id).unwrap().unwrap().events;
    let replayed = TranscriptProjection::from_events(&events);
    assert_eq!(live.items(), replayed.items());
    assert!(
        matches!(replayed.items().last(), Some(TranscriptItem::Activity(TranscriptActivity::Checkpoint { id, covered_event_sequence: 3 })) if id == "checkpoint-marker")
    );
}

#[test]
fn markers_tool_actions_and_complete_labels_fit_narrow_and_wide_with_reduced_motion() {
    let name = "mcp_arbitrary.tool_Δ";
    for width in [360.0, 900.0] {
        for activity in [
            ActivityStatus::Running,
            ActivityStatus::WaitingPermission,
            ActivityStatus::PermissionInterrupted,
            ActivityStatus::Interrupted,
            ActivityStatus::Failed,
            ActivityStatus::Cancelled,
            ActivityStatus::Completed,
        ] {
            let mut tree = UiTree::new(tool_activity_marker(
                Some("owner"),
                "call-exact",
                name,
                activity,
                false,
                activity == ActivityStatus::Failed,
            ));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 150.0));
            assert!(!tree.is_animating());
            assert!(frame
                .text_layouts()
                .iter()
                .any(|text| text.source.contains(name)));
            for layout in frame.text_layouts() {
                for run in &layout.runs {
                    assert!(
                        run.rect.x >= -0.1 && run.rect.x + run.rect.width <= width + 0.1,
                        "{run:?}"
                    );
                }
            }
            let target = frame
                .regions
                .iter()
                .find(|region| region.id == "tool:call-exact")
                .unwrap()
                .rect;
            let point = Point::new(
                target.x + target.width / 2.0,
                target.y + target.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "tool:call-exact".into()
                }]
            );
        }
    }
}

#[test]
fn markers_manual_compaction_progress_is_owner_scoped_and_branch_is_an_observation() {
    let workspace = tempfile::tempdir().unwrap();
    let mut state = super::tests::state();
    state.workspace = workspace.path().to_string_lossy().into_owned();
    state.selected_session = Some("owner".into());
    state
        .conversation_activity
        .begin_compaction("owner", workspace.path());
    state
        .conversation_activity
        .observe_branch(workspace.path(), Some("owner"), "before");
    state
        .conversation_activity
        .observe_branch(workspace.path(), Some("owner"), "after");
    let mut tree = UiTree::new(Element::column(host_activity_markers(&state)));
    let frame = tree.layout(Size::new(360.0, 250.0));
    assert!(tree.is_animating());
    let text = frame
        .text_layouts()
        .iter()
        .map(|layout| layout.source.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("Compactando contexto…"), "{text}");
    assert!(text.contains("Rama cambiada: before → after"), "{text}");
    assert!(frame.regions.is_empty());
    state.selected_session = Some("other".into());
    assert!(host_activity_markers(&state).is_empty());
    state.selected_session = Some("owner".into());
    let other_workspace = tempfile::tempdir().unwrap();
    state.workspace = other_workspace.path().to_string_lossy().into_owned();
    assert!(host_activity_markers(&state).is_empty());
    state.workspace = workspace.path().to_string_lossy().into_owned();
    state.conversation_activity.end_compaction();
    tree.set_root(Element::column(host_activity_markers(&state)));
    tree.layout(Size::new(360.0, 250.0));
    assert!(!tree.is_animating());
}

#[test]
#[ignore = "opt-in SVG layout export; no claim of live GUI or external provider acceptance"]
fn export_chat_marker_visual_review() {
    let output = std::path::PathBuf::from(
        std::env::var_os("OPENIDE_MARKER_VISUAL_DIR")
            .unwrap_or_else(|| "/tmp/openide-marker-visual".into()),
    );
    std::fs::create_dir_all(&output).unwrap();
    let clock = Instant::now();
    let mut projection = TranscriptProjection::new();
    apply(
        &mut projection,
        1,
        AgentEventKind::TurnStarted {
            prompt: "Revisá el proyecto y aplicá el cambio acordado".into(),
        },
    );
    apply(&mut projection, 2, request(1));
    let mut state = super::tests::state();
    state.selected_session = Some("marker-session".into());
    state.selected_session_title = Some("Revisión del proyecto".into());
    state.selected_backend = "native".into();
    state.model = "exact-model-id".into();
    state.native_provider_id = "custom".into();
    state.running = true;
    let checkpoint_root = tempfile::tempdir().unwrap();
    let checkpoints = CheckpointFileStore::new(checkpoint_root.path()).unwrap();
    let reference = checkpoints
        .write_candidate(&Checkpoint {
            session_id: "marker-session".into(),
            checkpoint_id: "review-checkpoint".into(),
            covered_event_sequence: 8,
            compatibility: CheckpointCompatibility {
                provider: "fixture".into(),
                model: "exact-model-id".into(),
                context_format_key: "fixture".into(),
            },
            summary: "Revisión conservada".into(),
            retained_user_messages: vec![],
            budget: CheckpointBudgetSnapshot::unverified(400, None),
        })
        .unwrap();
    for phase in ["thinking", "permission", "tool-error", "checkpoint"] {
        match phase {
            "permission" => {
                apply(&mut projection, 3, response(1));
                apply(
                    &mut projection,
                    4,
                    AgentEventKind::ToolStarted {
                        tool_call: call("write", "write_file"),
                    },
                );
                apply(
                    &mut projection,
                    5,
                    AgentEventKind::PermissionRequested {
                        request_id: "approve".into(),
                        tool_call: call("write", "write_file"),
                    },
                );
                state.pending_permissions.push(PermissionView {
                    request_id: "approve".into(),
                    tool_name: "write_file".into(),
                    arguments: serde_json::to_string_pretty(&call("write", "write_file").arguments)
                        .unwrap(),
                });
            }
            "tool-error" => {
                state.pending_permissions.clear();
                state.selected_tool_id = Some("write".into());
                apply(
                    &mut projection,
                    6,
                    AgentEventKind::PermissionResolved {
                        request_id: "approve".into(),
                        tool_call_id: "write".into(),
                        resolution: PermissionResolution::Allowed,
                    },
                );
                apply(
                    &mut projection,
                    7,
                    AgentEventKind::ToolFinished {
                        tool_call_id: "write".into(),
                        output: "No se pudo guardar: archivo modificado externamente".into(),
                        is_error: true,
                    },
                );
                apply(
                    &mut projection,
                    8,
                    AgentEventKind::TurnFailed {
                        error: "La herramienta devolvió un error; revisá el resultado".into(),
                    },
                );
                state.running = false;
            }
            "checkpoint" => {
                apply(
                    &mut projection,
                    9,
                    AgentEventKind::CheckpointCommitted {
                        reference: reference.clone(),
                    },
                );
            }
            _ => {}
        }
        state.transcript = projection.items().to_vec();
        for width in [420.0, 900.0] {
            let mut tree = UiTree::new(build_session_surface(&state, width, false, false));
            tree.set_reduced_motion(true);
            let frame = tree.layout_at(Size::new(width, 900.0), clock);
            crate::settings_ui::visual_qa::write_frame_svg(
                &frame,
                &output.join(format!("marker-{phase}-{}.svg", width as u32)),
                width,
                900.0,
            )
            .unwrap();
        }
    }
    // A reopened session whose second request never finished, after an
    // exploration with one failed read: both take the destructive ink.
    let mut sequence = 0;
    let events = [
        AgentEventKind::TurnStarted {
            prompt: "Buscá dónde se define el tema".into(),
        },
        request(1),
        response(1),
        AgentEventKind::ToolStarted {
            tool_call: call("read", "read_file"),
        },
        AgentEventKind::ToolFinished {
            tool_call_id: "read".into(),
            output: "pub const PANEL".into(),
            is_error: false,
        },
        AgentEventKind::ToolStarted {
            tool_call: call("search", "search_text"),
        },
        AgentEventKind::ToolFinished {
            tool_call_id: "search".into(),
            output: "No se pudo leer el directorio".into(),
            is_error: true,
        },
        request(2),
    ]
    .map(|kind| {
        sequence += 1;
        AgentEvent::new("marker-session", sequence, kind)
    });
    state.pending_permissions.clear();
    state.selected_tool_id = None;
    state.running = false;
    state.transcript = TranscriptProjection::from_events(&events).items().to_vec();
    // A failed request is hidden when the turn's error notice follows it;
    // without one it shows its own title.
    state
        .transcript
        .push(TranscriptItem::Activity(TranscriptActivity::Model {
            id: "model:failed".into(),
            step: 3,
            model: "exact-model-id".into(),
            has_text: false,
            status: ActivityStatus::Failed,
        }));
    for width in [420.0, 900.0] {
        let mut tree = UiTree::new(build_session_surface(&state, width, false, false));
        tree.set_reduced_motion(true);
        let frame = tree.layout_at(Size::new(width, 900.0), clock);
        let text = super::tests::visible_text(&frame);
        for expected in [
            "Solicitud al modelo interrumpida",
            "Solicitud al modelo fallida",
            "1 con error",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        crate::settings_ui::visual_qa::write_frame_svg(
            &frame,
            &output.join(format!("marker-failed-{}.svg", width as u32)),
            width,
            900.0,
        )
        .unwrap();
    }
    println!("{}", output.display());
}
