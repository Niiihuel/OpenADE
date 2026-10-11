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
