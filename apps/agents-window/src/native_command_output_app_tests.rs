use super::*;
use openide_harness::{PersistentCommandEvent, PersistentCommandStream};

fn probe(root: &Path) -> PathBuf {
    let source = root.join("ui_output_probe.rs");
    let binary = root.join(if cfg!(windows) {
        "ui_output_probe.exe"
    } else {
        "ui_output_probe"
    });
    std::fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
    std::io::stdout().write_all("Salida real ñ\n".as_bytes()).unwrap();
    std::io::stderr().write_all(&[255, 0, 27, 65]).unwrap();
}
"#,
    )
    .unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let result = std::process::Command::new(compiler)
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    binary
}

async fn apply_output_copy(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.view.native_command_output.loading {
        app.poll_native_command_sessions();
        assert!(Instant::now() < deadline, "output worker did not answer");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn action(owner: &str, command_id: &str) -> UiAction {
    UiAction::Clicked {
        id: format!(
            "native_command_output_open:{}",
            serde_json::to_string(&(owner, command_id)).unwrap()
        ),
    }
}

#[tokio::test]
async fn pending_output_inspection_host_observes_real_process_without_consumption_and_replaces_copies(
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let binary = probe(root.path());
    let data = root.path().join("data");
    let mut app = AgentsWindow::with_data_root(data.clone());
    // This test is about the pipe contract; see `keep_pipes`.
    app.native_command_sessions.keep_pipes();
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap();
    app.view.workspace = workspace.display().to_string();
    app.view.selected_session = Some(session.id.clone());
    app.view.selected_backend = "native".into();
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    let manager = app
        .native_command_sessions
        .for_session(
            &session.id,
            &workspace,
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    let started = manager
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if manager
            .inspect_pending_output(&started.id)
            .unwrap()
            .exit
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    app.view.native_commands = manager.list_sessions().unwrap();
    let policy_before = app.native_permission_policy().unwrap().digest_hex();
    let stored_before = app
        .store
        .as_ref()
        .unwrap()
        .load(&session.id)
        .unwrap()
        .unwrap();
    let open = format!(
        "native_command_output_open:{}",
        serde_json::to_string(&(&session.id, &started.id)).unwrap()
    );
    let mut tree = UiTree::new(crate::view::build_view(&app.view, 1200.0));
    let frame = tree.layout(Size::new(1200.0, 900.0));
    let entry = frame
        .regions
        .iter()
        .find(|region| region.id == open)
        .unwrap()
        .rect;
    let point = openide_ui::Point::new(entry.x + entry.width / 2.0, entry.y + entry.height / 2.0);
    tree.handle_event(openide_ui::UiEvent::PointerDown(point));
    let actions = tree.handle_event(openide_ui::UiEvent::PointerUp(point));
    assert!(actions
        .iter()
        .any(|action| matches!(action, UiAction::Clicked { id } if id == &open)));
    let ctx = egui::Context::default();
    for action in actions {
        app.apply_action(action, &ctx);
    }
    assert!(app.view.native_command_output.loading);
    assert!(app.native_command_output_response.is_some());
    apply_output_copy(&mut app).await;
    let original = app.view.native_command_output.snapshot.clone().unwrap();
    assert!(app.view.native_command_output.error.is_none());
    assert!(original.inspection.events.iter().any(|event| matches!(event, PersistentCommandEvent::Output { stream: PersistentCommandStream::Stdout, bytes } if bytes == "Salida real ñ\n".as_bytes())));
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        original.inspection
    );
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_output_refresh".into(),
        },
        &ctx,
    );
    apply_output_copy(&mut app).await;
    assert_eq!(
        app.view
            .native_command_output
            .snapshot
            .as_ref()
            .unwrap()
            .inspection,
        original.inspection
    );
    assert_eq!(
        app.view
            .native_command_output
            .snapshot
            .as_ref()
            .unwrap()
            .details_json,
        original.details_json,
        "refresh appended duplicate copies"
    );
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load(&session.id)
            .unwrap()
            .unwrap(),
        stored_before
    );
    assert_eq!(
        app.native_permission_policy().unwrap().digest_hex(),
        policy_before
    );
    assert!(app.active_run.is_none());
    assert!(app.active_resume.is_none());
    let polled = manager
        .poll(&started.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(polled.events, original.inspection.events);
    assert!(polled.finished);
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_output_refresh".into(),
        },
        &ctx,
    );
    apply_output_copy(&mut app).await;
    assert!(app.view.native_command_output.error.is_some());
    assert!(app.view.native_command_output.snapshot.is_none());
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_output_dismiss".into(),
        },
        &ctx,
    );
    assert!(app.view.native_command_output.owner_session_id.is_none());
    assert!(app.native_command_output_manager.is_none());
    drop(app);
    let reopened = AgentsWindow::with_data_root(data);
    assert!(reopened
        .native_command_sessions
        .manager(&session.id)
        .is_none());
    assert!(reopened
        .view
        .native_command_output
        .owner_session_id
        .is_none());
}

#[tokio::test]
async fn pending_output_inspection_host_discards_stale_owner_project_request_and_manager() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let binary = probe(root.path());
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap();
    app.view.workspace = workspace.display().to_string();
    app.view.selected_session = Some(session.id.clone());
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    let manager = app
        .native_command_sessions
        .for_session(
            &session.id,
            &workspace,
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    let started = manager
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    app.view.native_commands = manager.list_sessions().unwrap();
    let ctx = egui::Context::default();

    app.apply_action(action(&session.id, &started.id), &ctx);
    app.view.selected_session = Some("foreign-owner".into());
    app.poll_native_command_output();
    assert!(app.view.native_command_output.owner_session_id.is_none());
    assert!(app.native_command_output_response.is_none());
    app.apply_action(action(&session.id, &started.id), &ctx);
    assert!(app.view.native_command_output.owner_session_id.is_none());
    app.view.selected_session = Some(session.id.clone());
    app.apply_action(action(&session.id, &started.id), &ctx);
    app.view.workspace = root.path().display().to_string();
    app.poll_native_command_output();
    assert!(app.view.native_command_output.owner_session_id.is_none());
    app.view.workspace = workspace.display().to_string();
    app.apply_action(action(&session.id, &started.id), &ctx);
    let expected_request = app.view.native_command_output.request_id.clone();
    // A controlled channel delivers a real process copy and manager with only
    // its request identity stale; the previously captured worker is detached.
    let copied = crate::native_command_output_ui::NativeCommandOutputSnapshot::new(
        manager.inspect_pending_output(&started.id).unwrap(),
    );
    let (sender, receiver) = mpsc::channel();
    sender
        .send((
            session.id.clone(),
            started.id.clone(),
            "old-request".into(),
            manager.clone(),
            Ok(copied),
        ))
        .unwrap();
    app.native_command_output_response = Some(receiver);
    app.poll_native_command_output();
    assert_eq!(app.view.native_command_output.request_id, expected_request);
    assert!(app.view.native_command_output.snapshot.is_none());
    assert!(!app.view.native_command_output.loading);
    assert!(app.view.native_command_output.error.is_some());
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_output_refresh".into(),
        },
        &ctx,
    );
    apply_output_copy(&mut app).await;
    assert!(app.view.native_command_output.error.is_none());
    assert!(app.view.native_command_output.snapshot.is_some());

    app.native_command_sessions.revoke_session(&session.id);
    let fresh = app
        .native_command_sessions
        .for_session(
            &session.id,
            &workspace,
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&fresh, &manager));
    app.poll_native_command_output();
    assert!(app.view.native_command_output.owner_session_id.is_none());
    assert!(app.native_command_output_manager.is_none());
    assert!(
        fresh.list_sessions().unwrap().is_empty(),
        "observation created a command in the replacement manager"
    );
}
