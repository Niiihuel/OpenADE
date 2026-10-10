//! Durable plan history and Build authorization are separate projections.
use super::*;
use crate::native_context_ui::NativeContextUiState;
use openide_core::{ModelRouteSnapshot, ModelTokenUsage, PlanSnapshot, PlanStepStatus};

fn fixture() -> (tempfile::TempDir, AgentsWindow, String) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap();
    app.refresh_sessions();
    app.open_session(&session.id);
    (root, app, session.id)
}

fn admit(app: &mut AgentsWindow, owner: &str, turn: &str, mode: TurnMode) {
    let store = app.store.as_ref().unwrap();
    store
        .admit_native_turn(owner, turn, turn, "plan-fixture-model", mode)
        .unwrap();
    let event = store
        .load(owner)
        .unwrap()
        .unwrap()
        .events
        .last()
        .unwrap()
        .clone();
    app.last_sequence = event.sequence;
    app.projection.apply_event(&event);
    app.publish_transcript();
    app.refresh_sessions();
}

fn append(app: &mut AgentsWindow, owner: &str, kind: AgentEventKind) {
    let event = AgentEvent::new(owner, app.last_sequence + 1, kind);
    app.store
        .as_ref()
        .unwrap()
        .append_native_event(&event)
        .unwrap();
    app.last_sequence = event.sequence;
    app.projection.apply_event(&event);
    app.publish_transcript();
    app.refresh_sessions();
    assert!(
        !matches!(
            app.view.session_recovery.get(owner),
            Some(SessionRecoveryView::InvalidJournal(_))
        ),
        "{:?}",
        app.view.session_recovery
    );
}

fn snapshots(app: &AgentsWindow) -> Vec<(String, Option<u64>, PlanSnapshot)> {
    app.view
        .transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::PlanSnapshot {
                id, sequence, plan, ..
            } => Some((id.clone(), *sequence, plan.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn successive_plan_updates_keep_history_after_finish_and_reopen_without_restoring_build_authority()
{
    let (root, mut app, owner) = fixture();
    let proposed_text = "1. Inspect files\n2. Implement the change";
    let proposal = PlanSnapshot::from_plan_response(proposed_text);
    admit(&mut app, &owner, "plan-turn", TurnMode::Plan);
    for kind in [
        AgentEventKind::TurnStarted {
            prompt: "plan-turn".into(),
        },
        AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "plan-fixture-model".into(),
        },
        AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant(proposed_text, vec![]),
            response_id: None,
            usage: None,
        },
        AgentEventKind::AssistantMessage {
            content: proposed_text.into(),
        },
        AgentEventKind::PlanProposed {
            plan: proposal.clone(),
        },
    ] {
        append(&mut app, &owner, kind);
    }
    assert!(!app.view.session_plans[&owner].can_build);
    assert_eq!(snapshots(&app)[0].2, proposal);
    append(
        &mut app,
        &owner,
        AgentEventKind::TurnCompleted {
            response: proposed_text.into(),
            steps: 1,
        },
    );
    assert!(app.view.session_plans[&owner].can_build);
    let original = snapshots(&app)[0].clone();

    admit(&mut app, &owner, "build-turn", TurnMode::Build);
    // A stale actionable frame must not authorize another Build after admission.
    app.view.session_plans.get_mut(&owner).unwrap().can_build = true;
    app.view.prompt = "keep this draft".into();
    let journal_before = app.store.as_ref().unwrap().load(&owner).unwrap().unwrap();
    app.apply_action(
        UiAction::Clicked {
            id: format!("session_build_plan:{owner}"),
        },
        &egui::Context::default(),
    );
    assert!(!app.view.session_plans[&owner].can_build);
    assert!(app.active_run.is_none());
    assert_eq!(app.view.prompt, "keep this draft");
    assert_eq!(
        app.store.as_ref().unwrap().load(&owner).unwrap().unwrap(),
        journal_before
    );

    append(
        &mut app,
        &owner,
        AgentEventKind::TurnStarted {
            prompt: "build-turn".into(),
        },
    );
    let mut progress = proposal.clone();
    let mut expected = vec![proposal];
    for step in 1..=2 {
        progress.plan[0].status = PlanStepStatus::Completed;
        progress.plan[1].status = if step == 1 {
            PlanStepStatus::InProgress
        } else {
            PlanStepStatus::Completed
        };
        progress.explanation = Some(format!("Progress snapshot {step}"));
        let call = ToolCall {
            id: format!("update-{step}"),
            name: "update_plan".into(),
            arguments: serde_json::to_value(&progress).unwrap(),
        };
        for kind in [
            AgentEventKind::ModelRequestStarted {
                step,
                model: "plan-fixture-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
            AgentEventKind::PlanUpdated {
                tool_call_id: call.id.clone(),
                plan: progress.clone(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: call.id,
                output: "Plan updated".into(),
                is_error: false,
            },
        ] {
            append(&mut app, &owner, kind);
        }
        expected.push(progress.clone());
        assert_eq!(
            snapshots(&app)
                .into_iter()
                .map(|(_, _, plan)| plan)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(snapshots(&app)[0], original);
        assert_eq!(app.view.session_plans[&owner].snapshot, progress);
        assert!(!app.view.session_plans[&owner].can_build);
    }
    for kind in [
        AgentEventKind::ModelRequestStarted {
            step: 3,
            model: "plan-fixture-model".into(),
        },
        AgentEventKind::ModelResponseCommitted {
            step: 3,
            assistant: AgentMessage::assistant("Done", vec![]),
            response_id: None,
            usage: None,
        },
        AgentEventKind::AssistantMessage {
            content: "Done".into(),
        },
        AgentEventKind::TurnCompleted {
            response: "Done".into(),
            steps: 3,
        },
    ] {
        append(&mut app, &owner, kind);
    }
    let finished = snapshots(&app);
    assert_eq!(finished.len(), 3);
    assert!(finished.windows(2).all(|pair| pair[0].1 < pair[1].1));
    assert!(!app.view.session_plans[&owner].can_build);
    let journal_before = app.store.as_ref().unwrap().load(&owner).unwrap().unwrap();
    drop(app);

    let mut reopened = AgentsWindow::with_data_root(root.path().join("data"));
    reopened.open_session(&owner);
    assert_eq!(snapshots(&reopened), finished);
    assert_eq!(reopened.view.session_plans[&owner].snapshot, progress);
    assert!(!reopened.view.session_plans[&owner].can_build);
    reopened.view.prompt = "keep after reopen".into();
    reopened.apply_action(
        UiAction::Clicked {
            id: format!("session_build_plan:{owner}"),
        },
        &egui::Context::default(),
    );
    assert!(reopened.active_run.is_none());
    assert_eq!(reopened.view.prompt, "keep after reopen");
    assert_eq!(
        reopened
            .store
            .as_ref()
            .unwrap()
            .load(&owner)
            .unwrap()
            .unwrap()
            .events,
        journal_before.events
    );
}

#[test]
fn hidden_usage_in_the_timeline_keeps_reported_counts_in_context_and_journal() {
    let (_root, mut app, owner) = fixture();
    let route = ModelRouteSnapshot::new(
        "custom",
        "plan-fixture-model",
        "http://127.0.0.1:4567/v1",
        [7; 32],
    )
    .unwrap();
    let usage = ModelTokenUsage {
        input_tokens: 123,
        output_tokens: 45,
        total_tokens: 168,
        cached_input_tokens: Some(67),
        reasoning_tokens: Some(8),
    };
    admit(&mut app, &owner, "usage-turn", TurnMode::Build);
    for kind in [
        AgentEventKind::TurnStarted {
            prompt: "usage-turn".into(),
        },
        AgentEventKind::ToolCatalogCommitted {
            digest: "a".repeat(64),
        },
        AgentEventKind::HooksCommitted {
            version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
            digest: "b".repeat(64),
        },
        AgentEventKind::ExecPolicyCommitted {
            version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
            digest: "c".repeat(64),
        },
        AgentEventKind::ModelContextCommitted {
            version: openide_core::MODEL_CONTEXT_VERSION,
            digest: "d".repeat(64),
        },
        AgentEventKind::ModelRouteCommitted {
            version: route.version,
            provider_id: route.provider_id.clone(),
            model_id: route.model_id.clone(),
            endpoint_digest: route.endpoint_digest.clone(),
            request_profile_digest: route.request_profile_digest.clone(),
        },
        AgentEventKind::ModelRequestStarted {
            step: 1,
            model: route.model_id.clone(),
        },
        AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("Usage is retained in Context", vec![]),
            response_id: None,
            usage: Some(usage.clone()),
        },
        AgentEventKind::AssistantMessage {
            content: "Usage is retained in Context".into(),
        },
        AgentEventKind::TurnCompleted {
            response: "Usage is retained in Context".into(),
            steps: 1,
        },
    ] {
        append(&mut app, &owner, kind);
    }
    let stored = app.store.as_ref().unwrap().load(&owner).unwrap().unwrap();
    app.view.native_context = NativeContextUiState::selected_route(
        &route.provider_id,
        &route.model_id,
        stored.session.workspace.canonicalize().unwrap(),
        Some(TokenBudgetPolicy {
            input_limit_tokens: None,
            context_window_tokens: 1_000,
            output_reserve_tokens: 100,
            tool_schema_reserve_tokens: 0,
            auto_compact_token_limit: None,
        }),
        Some(200),
        vec![],
    )
    .apply_validated(
        &stored,
        replay_stored_native(&app.data_root, &stored),
        Some(&route),
    );
    assert!(
        app.view.native_context.error.is_none(),
        "{:?}",
        app.view.native_context.error
    );
    assert_eq!(app.view.native_context.last_usage, Some(usage.clone()));
    assert_eq!(
        app.view.native_context.conversation_usage,
        Some(usage.clone())
    );
    assert_eq!(app.view.native_context.remaining_percent, Some(83));
    let usage_id = app
        .view
        .transcript
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ModelUsage {
                id,
                usage: recorded,
                ..
            } => {
                assert_eq!(recorded, &usage);
                Some(id.clone())
            }
            _ => None,
        })
        .expect("the token evidence remains in the underlying projection");
    let mut tree = UiTree::new(crate::session_surface::build_session_surface(
        &app.view, 900.0, false, false,
    ));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 900.0));
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id.contains(&usage_id)));
    assert!(!frame.commands.iter().any(|command| matches!(command, openide_ui::DrawCommand::Text { text, .. } if text.contains("Uso de tokens"))));
    assert_eq!(app.view.native_context.last_usage, Some(usage.clone()));
    assert_eq!(app.view.native_context.conversation_usage, Some(usage));
    assert_eq!(
        app.store.as_ref().unwrap().load(&owner).unwrap().unwrap(),
        stored
    );
}
