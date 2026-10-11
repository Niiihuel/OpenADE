use super::*;
use openide_ui::{HitKind, Point, Size, UiEvent, UiTree};

const REQUEST: &str = "pending-question";

fn arguments() -> String {
    serde_json::json!({"questions": [
        {
            "header": "Almacenamiento",
            "question": "¿Dónde guardamos las sesiones?",
            "options": [
                {"label": "SQLite", "description": "Un archivo, consultas rápidas"},
                {"label": "Archivos JSONL"}
            ],
            "multi_select": false
        },
        {
            "header": "Plataformas",
            "question": "¿Qué plataformas cubrimos primero?",
            "options": [{"label": "Linux"}, {"label": "Windows"}, {"label": "macOS"}],
            "multi_select": true
        }
    ]})
    .to_string()
}

fn fixture() -> (tempfile::TempDir, AgentsWindow, String) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let owner = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap()
        .id;
    app.view.selected_session = Some(owner.clone());
    app.view.workspace = workspace.display().to_string();
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.pending_permissions.push(PermissionView {
        request_id: REQUEST.into(),
        tool_name: question_ui::TOOL.into(),
        arguments: serde_json::to_string_pretty(
            &serde_json::from_str::<serde_json::Value>(&arguments()).unwrap(),
        )
        .unwrap(),
    });
    (root, app, owner)
}

// Press the control in the rendered window, as the user does.
fn press(app: &mut AgentsWindow, id: &str) {
    let size = Size::new(900.0, 720.0);
    let mut tree = UiTree::new(crate::view::build_window_view(&app.view, size.width, size.height));
    tree.set_reduced_motion(true);
    let frame = tree.layout(size);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    for action in tree.handle_event(UiEvent::PointerUp(point)) {
        app.apply_action(action, &egui::Context::default());
    }
}

fn recorded(app: &AgentsWindow, owner: &str) -> Vec<openide_core::RecordedPermissionDecision> {
    app.store
        .as_ref()
        .unwrap()
        .load_permission_decisions(owner)
        .unwrap()
}

#[test]
fn the_answer_built_on_the_card_is_recorded_as_the_users_before_the_request_resolves() {
    let (_root, mut app, owner) = fixture();
    // Nothing chosen yet: the question cannot be sent empty.
    press(&mut app, &question_ui::action(REQUEST, "next"));
    app.apply_action(
        UiAction::Clicked {
            id: question_ui::action(REQUEST, "send"),
        },
        &egui::Context::default(),
    );
    assert!(recorded(&app, &owner).is_empty());
    press(&mut app, &question_ui::action(REQUEST, "back"));

    press(&mut app, &question_ui::pick_action(REQUEST, 0, 0));
    press(&mut app, &question_ui::pick_action(REQUEST, 0, 1));
    press(&mut app, &question_ui::action(REQUEST, "next"));
    press(&mut app, &question_ui::pick_action(REQUEST, 1, 0));
    press(&mut app, &question_ui::pick_action(REQUEST, 1, 2));
    app.apply_action(
        UiAction::InputChanged {
            id: question_ui::text_id(REQUEST, 1),
            value: "  y FreeBSD más adelante ".into(),
        },
        &egui::Context::default(),
    );
    press(&mut app, &question_ui::action(REQUEST, "send"));

    let decisions = recorded(&app, &owner);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].request_id, REQUEST);
    assert_eq!(decisions[0].decision, PermissionDecision::Allow);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(decisions[0].answer.as_deref().unwrap()).unwrap(),
        serde_json::json!({
            "answered_by": "user",
            "answers": [
                {"header": "Almacenamiento", "selected": ["Archivos JSONL"]},
                {
                    "header": "Plataformas",
                    "selected": ["Linux", "macOS"],
                    "other_answer": "y FreeBSD más adelante"
                }
            ]
        })
    );
    assert!(app.view.pending_permissions.is_empty());
    assert!(app.view.question_drafts.is_empty());
}

#[test]
fn skipping_records_a_refusal_without_an_answer_and_a_stale_card_does_nothing() {
    let (_root, mut app, owner) = fixture();
    press(&mut app, &question_ui::pick_action(REQUEST, 0, 0));
    press(&mut app, &question_ui::action(REQUEST, "skip"));
    let decisions = recorded(&app, &owner);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].decision, PermissionDecision::Deny);
    assert_eq!(decisions[0].answer, None);
    assert!(app.view.pending_permissions.is_empty());

    // The request is gone: a late click from an old frame changes nothing.
    for id in [
        question_ui::action(REQUEST, "send"),
        question_ui::pick_action(REQUEST, 0, 1),
        question_ui::action("another-request", "skip"),
    ] {
        app.apply_action(UiAction::Clicked { id }, &egui::Context::default());
    }
    assert_eq!(recorded(&app, &owner).len(), 1);
    assert!(app.view.question_drafts.is_empty());
}

#[test]
fn a_question_is_not_answerable_through_the_permission_card_actions() {
    let (_root, mut app, owner) = fixture();
    // The question draws its own card, never Allow / Deny buttons.
    let size = Size::new(900.0, 720.0);
    let frame = UiTree::new(crate::view::build_window_view(&app.view, size.width, size.height))
        .layout(size);
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id.starts_with("approval_decide:")));
    assert!(frame
        .regions
        .iter()
        .any(|region| region.id == question_ui::action(REQUEST, "skip")));
    assert!(recorded(&app, &owner).is_empty());
    let _ = &mut app;
}

fn propose(app: &mut AgentsWindow, mode: &str) {
    app.view.pending_permissions.clear();
    app.view.pending_permissions.push(PermissionView {
        request_id: REQUEST.into(),
        tool_name: crate::mode_proposal_ui::TOOL.into(),
        arguments: serde_json::to_string_pretty(&serde_json::json!({
            "mode": mode,
            "reason": "El cambio toca el formato del journal."
        }))
        .unwrap(),
    });
}

#[test]
fn an_accepted_mode_is_set_by_the_composers_own_selection_only_after_the_turn() {
    let (_root, mut app, owner) = fixture();
    app.view.selected_backend = "native".into();
    app.view.turn_mode = TurnMode::Build;
    let permissions = app.agent_tool_settings.clone();
    propose(&mut app, "plan");
    app.view.running = true;
    press(&mut app, &crate::mode_proposal_ui::action(REQUEST, "accept"));
    let decisions = recorded(&app, &owner);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].decision, PermissionDecision::Allow);
    assert!(app.view.pending_permissions.is_empty());
    // The turn that proposed it is still running in its own mode.
    assert!(!app.apply_accepted_turn_mode());
    assert_eq!(app.view.turn_mode, TurnMode::Build);
    app.view.running = false;
    assert!(app.apply_accepted_turn_mode());
    assert_eq!(app.view.turn_mode, TurnMode::Plan);
    assert!(app.view.status.starts_with("Modo Plan"));
    assert!(!app.apply_accepted_turn_mode(), "applied once");
    // A mode is all a proposal can change.
    assert_eq!(app.agent_tool_settings, permissions);
}

#[test]
fn a_rejected_or_malformed_proposal_changes_no_mode() {
    let (_root, mut app, owner) = fixture();
    app.view.selected_backend = "native".into();
    app.view.turn_mode = TurnMode::Plan;
    propose(&mut app, "build");
    press(&mut app, &crate::mode_proposal_ui::action(REQUEST, "reject"));
    assert_eq!(recorded(&app, &owner)[0].decision, PermissionDecision::Deny);
    assert!(!app.apply_accepted_turn_mode());
    assert_eq!(app.view.turn_mode, TurnMode::Plan);

    // Anything that is not one of the two modes draws no decision at all.
    propose(&mut app, "full_access");
    let size = Size::new(900.0, 720.0);
    let frame = UiTree::new(crate::view::build_window_view(&app.view, size.width, size.height))
        .layout(size);
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id.starts_with("mode_proposal_")
            || region.id.starts_with("approval_decide:")));
    app.apply_action(
        UiAction::Clicked {
            id: crate::mode_proposal_ui::action(REQUEST, "accept"),
        },
        &egui::Context::default(),
    );
    assert_eq!(recorded(&app, &owner).len(), 1);
    assert!(app.accepted_turn_mode.is_none());

    // An accepted mode does not follow the user to another session.
    propose(&mut app, "build");
    app.view.pending_permissions[0].request_id = "second".into();
    app.apply_action(
        UiAction::Clicked {
            id: crate::mode_proposal_ui::action("second", "accept"),
        },
        &egui::Context::default(),
    );
    assert!(app.accepted_turn_mode.is_some());
    app.view.selected_session = Some("another-session".into());
    assert!(!app.apply_accepted_turn_mode());
    assert!(app.accepted_turn_mode.is_none());
    assert_eq!(app.view.turn_mode, TurnMode::Plan);
}
