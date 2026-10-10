use super::*;
use openide_ui::{HitKind, Point, Size, UiEvent, UiTree};

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

fn decisions(app: &AgentsWindow, target: &ApprovalTarget) -> Vec<PermissionDecision> {
    app.store
        .as_ref()
        .unwrap()
        .load_permission_decisions(&target.owner_session_id)
        .unwrap()
        .into_iter()
        .map(|record| record.decision)
        .collect()
}

// Press the card's own button in the rendered window: no review step exists.
fn press_inline(app: &mut AgentsWindow, target: &ApprovalTarget, choice: &str) {
    let size = Size::new(900.0, 720.0);
    let mut tree = UiTree::new(crate::view::build_window_view(&app.view, size.width, size.height));
    tree.set_reduced_motion(true);
    let frame = tree.layout(size);
    let id = target.decision_action(choice);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing inline {choice}"))
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    for action in tree.handle_event(UiEvent::PointerUp(point)) {
        app.apply_action(action, &egui::Context::default());
    }
}

#[test]
fn inline_one_time_decisions_reach_the_existing_durable_controller() {
    for (choice, expected) in [
        ("allow_once", PermissionDecision::Allow),
        ("deny", PermissionDecision::Deny),
    ] {
        let (_root, mut app, target) = fixture();
        press_inline(&mut app, &target, choice);
        assert_eq!(decisions(&app, &target), [expected]);
        assert!(
            app.active_run.is_none() && app.active_resume.is_none(),
            "a decision never invents a worker or starts an operation"
        );
    }
}

#[test]
fn inline_session_decision_reaches_the_session_grant_path() {
    let (_root, mut app, target) = fixture();
    press_inline(&mut app, &target, "allow_session");
    // The grant is saved by the worker that owns the request; without one the
    // session path reports it and records no one-time decision.
    assert_eq!(app.view.status, "La solicitud de permiso ya no está activa");
    assert!(decisions(&app, &target).is_empty());
}

#[test]
fn approval_actions_reject_foreign_obsolete_oversized_or_malformed_requests() {
    let (_root, mut app, target) = fixture();
    let status = app.view.status.clone();
    let mut ignored: Vec<String> = [
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
    ]
    .iter()
    .flat_map(|foreign| ["allow_once", "allow_session", "deny"].map(|choice| foreign.decision_action(choice)))
    .collect();
    ignored.push(target.decision_action("unrecognized"));
    ignored.push("approval_decide:".into());
    ignored.push("approval_decide:{not json".into());
    ignored.push(format!(
        "approval_decide:{}",
        serde_json::to_string(&target).unwrap()
    ));
    // A valid, pending decision padded past the 16 KiB cap.
    ignored.push(format!(
        "{}{}",
        target.decision_action("allow_once"),
        " ".repeat(16 * 1024)
    ));
    for id in ignored {
        click(&mut app, id.clone());
        assert_eq!(app.view.pending_permissions.len(), 1, "{id}");
        assert_eq!(app.view.status, status, "no dispatch for {id}");
        assert!(decisions(&app, &target).is_empty(), "{id}");
    }
}

#[test]
fn decision_for_a_request_that_is_no_longer_pending_is_ignored() {
    for choice in ["allow_once", "allow_session", "deny"] {
        let (_root, mut app, target) = fixture();
        let status = app.view.status.clone();
        let stale = target.decision_action(choice);
        app.view.workspace = "/different/workspace".into();
        click(&mut app, stale.clone());
        app.view.workspace = target.workspace.clone();
        app.view.pending_permissions.clear();
        click(&mut app, stale);
        assert_eq!(app.view.status, status);
        assert!(decisions(&app, &target).is_empty());
    }
}
