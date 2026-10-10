use super::*;
use openide_ui::{Key, Size, UiEvent, UiTree};

fn fixture() -> (tempfile::TempDir, AgentsWindow, ApprovalTarget) {
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
        request_id: "pending:request".into(),
        tool_name: "apply_patch".into(),
        arguments: r#"{"patch":"*** Begin Patch\n*** Delete File: draft.md\n*** End Patch"}"#
            .into(),
    });
    let target = ApprovalTarget {
        owner_session_id: owner,
        workspace: app.view.workspace.clone(),
        child_session_id: None,
        request_id: "pending:request".into(),
    };
    (root, app, target)
}

fn click(app: &mut AgentsWindow, id: impl Into<String>) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

#[test]
fn review_and_escape_never_resolve_the_request_or_modify_the_journal() {
    let (_root, mut app, target) = fixture();
    let policy = app.agent_tool_settings;
    let events = app
        .store
        .as_ref()
        .unwrap()
        .load(&target.owner_session_id)
        .unwrap()
        .unwrap()
        .events;
    click(&mut app, target.review_action());
    assert_eq!(app.view.approval_ui.selected.as_ref(), Some(&target));
    click(&mut app, "approval_details");
    assert!(app.view.approval_ui.details_open);
    let mut tree = UiTree::new(crate::view::build_window_view(&app.view, 900.0, 720.0));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(900.0, 720.0));
    let actions = tree.handle_event(UiEvent::Key {
        key: Key::Escape,
        shift: false,
        ctrl: false,
    });
    assert!(actions
        .iter()
        .any(|action| matches!(action, UiAction::Clicked { id } if id == "approval_close")));
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(app.view.approval_ui.selected.is_none());
    assert!(!app.view.approval_ui.details_open);
    assert_eq!(app.view.pending_permissions.len(), 1);
    assert_eq!(app.agent_tool_settings, policy);
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load(&target.owner_session_id)
            .unwrap()
            .unwrap()
            .events,
        events
    );
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn approval_actions_reject_unreviewed_foreign_or_obsolete_requests() {
    let (_root, mut app, target) = fixture();
    let status = app.view.status.clone();
    for choice in ["allow_once", "allow_session", "deny"] {
        click(&mut app, target.decision_action(choice));
        assert_eq!(app.view.pending_permissions.len(), 1);
        assert_eq!(
            app.view.status, status,
            "no dispatch without an explicit review"
        );
    }
    for foreign in [
        ApprovalTarget {
            owner_session_id: "other".into(),
            ..target.clone()
        },
        ApprovalTarget {
            workspace: "/other/workspace".into(),
            ..target.clone()
        },
        ApprovalTarget {
            request_id: "obsolete".into(),
            ..target.clone()
        },
        ApprovalTarget {
            child_session_id: Some("other-child".into()),
            ..target.clone()
        },
    ] {
        click(&mut app, foreign.review_action());
        assert!(app.view.approval_ui.selected.is_none());
    }
    click(&mut app, target.review_action());
    click(&mut app, target.decision_action("unrecognized"));
    assert_eq!(app.view.pending_permissions.len(), 1);
    app.view.workspace = "/different/workspace".into();
    click(&mut app, target.decision_action("allow_once"));
    assert_eq!(app.view.pending_permissions.len(), 1);
    assert!(app.sync_approval_ui());
    assert!(app.view.approval_ui.selected.is_none());
    assert_eq!(app.view.status, status);
}

#[test]
fn resolved_request_closes_review_and_clears_private_details() {
    let (_root, mut app, target) = fixture();
    click(&mut app, target.review_action());
    click(&mut app, "approval_details");
    app.view.pending_permissions.clear();
    assert!(app.sync_approval_ui());
    assert_eq!(app.view.approval_ui, Default::default());
    assert!(!app.sync_approval_ui());
}

#[test]
fn reviewed_one_time_decisions_reach_the_existing_durable_controller() {
    for (choice, expected) in [
        ("allow_once", PermissionDecision::Allow),
        ("deny", PermissionDecision::Deny),
    ] {
        let (_root, mut app, target) = fixture();
        click(&mut app, target.review_action());
        click(&mut app, target.decision_action(choice));
        let records = app
            .store
            .as_ref()
            .unwrap()
            .load_permission_decisions(&target.owner_session_id)
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, target.request_id);
        assert_eq!(records[0].decision, expected);
        assert!(
            app.active_run.is_none() && app.active_resume.is_none(),
            "a review never invents a worker or starts an operation"
        );
    }
}
