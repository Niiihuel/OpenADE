use super::*;
use crate::native_command_control_ui::NativeCommandControlPhase;
use openide_core::{
    NativeCommandOperationAction, NativeCommandOperationOutcome, NativeCommandOperationPhase,
    NativeCommandOperationRequest, NativeCommandOperationState, NativeCommandOperationStore,
};
use openide_harness::{LocalTool, PersistentCommandEvent, PersistentCommandStream};
use openide_ui::{HitKind, Key, Point, UiEvent};

#[test]
fn missing_manual_command_owner_invalidates_error_and_backs_off_before_retrying() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.native_command_poll_owners
        .insert("missing-owner".into());
    let previous_status = app.view.status.clone();

    assert!(app.poll_native_manual_commands());
    assert_ne!(app.view.status, previous_status);
    assert!(app.view.status.contains("session does not exist"));
    assert!(app.native_manual_command_response.is_none());
    let first_attempt = app.last_native_manual_command_poll.unwrap();
    assert!(!app.poll_native_manual_commands());
    assert_eq!(app.last_native_manual_command_poll, Some(first_attempt));

    app.last_native_manual_command_poll = Some(Instant::now() - Duration::from_secs(1));
    assert!(
        !app.poll_native_manual_commands(),
        "an unchanged error needs no new frame"
    );
    assert!(app.last_native_manual_command_poll.unwrap() > first_attempt);
}

pub(super) fn probe(root: &Path) -> PathBuf {
    let source = root.join("manual_control_probe.rs");
    let binary = root.join(if cfg!(windows) {
        "manual_control_probe.exe"
    } else {
        "manual_control_probe"
    });
    std::fs::write(
        &source,
        r#"
use std::io::{Read, Write};
fn main() {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut bytes = [0u8; 4096];
    loop {
        let count = stdin.read(&mut bytes).unwrap();
        if count == 0 { stdout.write_all(b"<EOF>").unwrap(); stdout.flush().unwrap(); return; }
        stdout.write_all(&bytes[..count]).unwrap(); stdout.flush().unwrap();
    }
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

fn enabled_settings(root: &Path) -> AgentToolSettings {
    let mut settings = AgentToolSettings::default();
    settings.set(LocalTool::RunCommand, true);
    settings.set_command_isolation(CommandIsolation::HostUnrestricted);
    settings.save(root).unwrap();
    settings
}

fn select_owner(app: &mut AgentsWindow, owner: &str, workspace: &Path) {
    app.view.workspace = workspace.display().to_string();
    app.view.selected_session = Some(owner.into());
    app.view.selected_backend = "native".into();
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
}

fn fixture(
    root: &Path,
) -> (
    AgentsWindow,
    PathBuf,
    String,
    Arc<PersistentCommandManager>,
    String,
) {
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let binary = probe(root);
    let data = root.join("data");
    let settings = enabled_settings(&data);
    let mut app = AgentsWindow::with_data_root(data);
    // This test is about the pipe contract; see `keep_pipes`.
    app.native_command_sessions.keep_pipes();
    app.view.native_tool_permissions = settings.permissions();
    let owner = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap()
        .id;
    select_owner(&mut app, &owner, &workspace);
    let manager = app
        .native_command_sessions
        .for_session(&owner, &workspace, CommandIsolation::HostUnrestricted, true)
        .unwrap()
        .unwrap();
    let command = manager
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap()
        .id;
    app.view.native_commands = manager.list_sessions().unwrap();
    (app, workspace, owner, manager, command)
}

pub(super) fn apply(app: &mut AgentsWindow, id: &str) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

async fn poll_until(app: &mut AgentsWindow, condition: impl Fn(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_native_manual_commands();
        if condition(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Manual command host did not reach expected state: {:?}",
            app.view.native_command_control
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub(super) async fn open(app: &mut AgentsWindow, owner: &str, command: &str) {
    apply(
        app,
        &format!(
            "native_command_control_open:{}",
            serde_json::to_string(&(owner, command)).unwrap()
        ),
    );
    poll_until(app, |app| {
        app.native_command_control_response.is_none()
            && app.view.native_command_control.observed.is_some()
    })
    .await;
    assert!(app.view.native_command_control.error.is_none());
}

pub(super) fn tree(app: &AgentsWindow, width: f32) -> UiTree {
    let mut tree = UiTree::new(crate::view::build_view(&app.view, width));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 720.0));
    tree
}

pub(super) fn click(app: &mut AgentsWindow, tree: &mut UiTree, width: f32, id: &str) {
    let viewport = Size::new(width, 720.0);
    for _ in 0..35 {
        let frame = tree.layout(viewport);
        if let Some(region) = frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
        {
            let point = Point::new(
                region.rect.x + region.rect.width / 2.0,
                region.rect.y + region.rect.height / 2.0,
            );
            if frame.hit_test(point) == Some(id) {
                assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1);
                tree.handle_event(UiEvent::PointerDown(point));
                let actions = tree.handle_event(UiEvent::PointerUp(point));
                assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
                for action in actions {
                    app.apply_action(action, &egui::Context::default());
                }
                return;
            }
        }
        let scroll = frame
            .regions
            .iter()
            .find(|region| {
                region.id == "modal_body:native_command_control_dismiss"
                    && region.kind == HitKind::Scroll
            })
            .unwrap()
            .rect;
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(
                scroll.x + scroll.width / 2.0,
                scroll.y + scroll.height / 2.0,
            ),
            delta_y: -150.0,
        });
    }
    panic!("Action {id} was not reachable at {width}");
}

pub(super) fn paste(app: &mut AgentsWindow, tree: &mut UiTree, width: f32, id: &str, exact: &str) {
    let viewport = Size::new(width, 720.0);
    let mut reachable = false;
    for _ in 0..35 {
        let frame = tree.layout(viewport);
        if let Some(region) = frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::TextArea)
        {
            let point = Point::new(
                region.rect.x + region.rect.width / 2.0,
                region.rect.y + region.rect.height / 2.0,
            );
            if frame.hit_test(point) == Some(id) {
                tree.handle_event(UiEvent::PointerDown(point));
                tree.handle_event(UiEvent::PointerUp(point));
                reachable = true;
                break;
            }
        }
        let scroll = frame
            .regions
            .iter()
            .find(|region| {
                region.id == "modal_body:native_command_control_dismiss"
                    && region.kind == HitKind::Scroll
            })
            .unwrap()
            .rect;
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(
                scroll.x + scroll.width / 2.0,
                scroll.y + scroll.height / 2.0,
            ),
            delta_y: -150.0,
        });
    }
    assert!(reachable, "Editor {id} must be reachable at {width}");
    assert!(tree.focus(id));
    let actions = tree.handle_event(UiEvent::Text(exact.into()));
    assert_eq!(
        actions,
        vec![UiAction::InputChanged {
            id: id.into(),
            value: exact.into()
        }]
    );
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

fn bytes(manager: &PersistentCommandManager, command: &str) -> Vec<u8> {
    manager
        .inspect_pending_output(command)
        .unwrap()
        .events
        .into_iter()
        .filter_map(|event| match event {
            PersistentCommandEvent::Output {
                stream: PersistentCommandStream::Stdout,
                bytes,
            } => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect()
}

pub(super) async fn wait_bytes(manager: &PersistentCommandManager, command: &str, exact: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while bytes(manager, command) != exact {
        assert!(
            Instant::now() < deadline,
            "Probe bytes differ: {:?}",
            bytes(manager, command)
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub(super) async fn acknowledge(app: &mut AgentsWindow, count: usize) {
    poll_until(app, |app| {
        app.native_manual_command_response.is_none()
            && app.view.native_command_control.operations.len() == count
            && app
                .view
                .native_command_control
                .operations
                .iter()
                .all(|operation| {
                    matches!(
                        operation.state,
                        NativeCommandOperationState::Finished { .. }
                    )
                })
            && matches!(
                app.view.native_command_control.phase,
                NativeCommandControlPhase::Draft
            )
    })
    .await;
    assert!(app
        .view
        .native_command_control
        .operations
        .iter()
        .all(|operation| matches!(
            operation.state,
            NativeCommandOperationState::Finished {
                outcome: NativeCommandOperationOutcome::Succeeded { .. }
            }
        )));
}

#[tokio::test]
async fn command_controls_real_process_exact_input_pending_dismiss_eof_and_terminate_receipts() {
    for width in [360.0, 900.0] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, workspace, owner, manager, command) = fixture(root.path());
        open(&mut app, &owner, &command).await;
        let mut editor = tree(&app, width);
        let exact = "  ñ🦀\r\nCR\rTAB\tNUL\0sin salto final  ";
        paste(
            &mut app,
            &mut editor,
            width,
            "native_command_control_input",
            exact,
        );
        assert_eq!(
            app.view.native_command_control.input.as_bytes(),
            exact.as_bytes()
        );
        editor.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        assert_eq!(
            editor.handle_event(UiEvent::Key {
                key: Key::C,
                shift: false,
                ctrl: true
            }),
            vec![UiAction::CopyRequested { text: exact.into() }]
        );
        let mut draft = tree(&app, width);
        click(
            &mut app,
            &mut draft,
            width,
            "native_command_control_prepare_input",
        );
        assert!(matches!(
            app.view.native_command_control.phase,
            NativeCommandControlPhase::Confirm { .. }
        ));
        assert!(bytes(&manager, &command).is_empty());
        assert!(
            NativeCommandOperationStore::new(app.store.as_ref().unwrap().clone())
                .list(&owner)
                .unwrap()
                .is_empty()
        );
        let lease = app
            .store
            .as_ref()
            .unwrap()
            .try_acquire_native_lease(&owner)
            .unwrap();
        let mut confirmation = tree(&app, width);
        click(
            &mut app,
            &mut confirmation,
            width,
            "native_command_control_confirm",
        );
        assert!(matches!(
            app.view.native_command_control.phase,
            NativeCommandControlPhase::Pending { .. }
        ));
        let operation_id = app.view.native_command_control.operations[0]
            .request
            .operation_id
            .clone();
        assert!(
            bytes(&manager, &command).is_empty(),
            "Busy native lease cannot authorize a UI effect"
        );
        let mut pending = tree(&app, width);
        click(
            &mut app,
            &mut pending,
            width,
            "native_command_control_dismiss",
        );
        assert!(!app.view.native_command_control.open);
        assert!(matches!(
            app.view.native_command_control.phase,
            NativeCommandControlPhase::Pending { .. }
        ));
        drop(lease);
        acknowledge(&mut app, 1).await;
        wait_bytes(&manager, &command, exact.as_bytes()).await;
        let receipt = &app.view.native_command_control.operations[0];
        assert_eq!(receipt.request.operation_id, operation_id);
        assert_eq!(receipt.request.manager_generation, manager.generation());
        assert_eq!(receipt.request.workspace, workspace.canonicalize().unwrap());
        assert_eq!(
            receipt.request.action,
            NativeCommandOperationAction::Stdin {
                input: exact.into(),
                close_after: false
            }
        );
        assert!(receipt.effect_result.is_some());
        apply(&mut app, "native_command_control_confirm");
        assert_eq!(
            NativeCommandOperationStore::new(app.store.as_ref().unwrap().clone())
                .list(&owner)
                .unwrap()
                .len(),
            1,
            "A stale Confirm cannot send twice"
        );

        open(&mut app, &owner, &command).await;
        let mut draft = tree(&app, width);
        click(
            &mut app,
            &mut draft,
            width,
            "native_command_control_prepare_eof",
        );
        let mut confirmation = tree(&app, width);
        click(
            &mut app,
            &mut confirmation,
            width,
            "native_command_control_confirm",
        );
        acknowledge(&mut app, 2).await;
        wait_bytes(&manager, &command, format!("{exact}<EOF>").as_bytes()).await;
        assert_eq!(
            app.view.native_command_control.operations[1].request.action,
            NativeCommandOperationAction::Stdin {
                input: String::new(),
                close_after: true
            }
        );
        assert!(!manager.control_snapshot(&command).unwrap().stdin_open());

        let binary = probe(root.path());
        let second = manager
            .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
            .unwrap()
            .id;
        open(&mut app, &owner, &second).await;
        let mut draft = tree(&app, width);
        click(
            &mut app,
            &mut draft,
            width,
            "native_command_control_prepare_terminate",
        );
        assert!(manager.control_snapshot(&second).unwrap().exit.is_none());
        let mut confirmation = tree(&app, width);
        click(
            &mut app,
            &mut confirmation,
            width,
            "native_command_control_confirm",
        );
        acknowledge(&mut app, 3).await;
        assert_eq!(
            app.view.native_command_control.operations[2].request.action,
            NativeCommandOperationAction::Terminate
        );
        assert!(manager.control_snapshot(&second).unwrap().exit.is_some());
        assert!(
            app.store
                .as_ref()
                .unwrap()
                .load(&owner)
                .unwrap()
                .unwrap()
                .events
                .is_empty(),
            "Human operations do not invent model/tool events"
        );
        assert!(app.active_run.is_none() && app.active_resume.is_none());
    }
}

#[tokio::test]
async fn command_controls_fresh_saved_permission_denial_between_prepare_and_confirm_has_no_effect()
{
    let root = tempfile::tempdir().unwrap();
    let (mut app, _workspace, owner, manager, command) = fixture(root.path());
    open(&mut app, &owner, &command).await;
    let mut editor = tree(&app, 360.0);
    paste(
        &mut app,
        &mut editor,
        360.0,
        "native_command_control_input",
        "must not reach stdin\r\n",
    );
    let mut draft = tree(&app, 360.0);
    click(
        &mut app,
        &mut draft,
        360.0,
        "native_command_control_prepare_input",
    );
    assert!(matches!(
        app.view.native_command_control.phase,
        NativeCommandControlPhase::Confirm { .. }
    ));
    let mut settings = AgentToolSettings::load(&app.data_root).unwrap();
    settings.set(LocalTool::RunCommand, false);
    settings.save(&app.data_root).unwrap();
    assert!(
        app.view.native_tool_permissions.run_command,
        "The test deliberately keeps stale UI permissions"
    );
    let mut confirmation = tree(&app, 360.0);
    click(
        &mut app,
        &mut confirmation,
        360.0,
        "native_command_control_confirm",
    );
    assert!(app.view.native_command_control.error.is_some());
    assert!(app.native_manual_command_response.is_none());
    assert!(
        NativeCommandOperationStore::new(app.store.as_ref().unwrap().clone())
            .list(&owner)
            .unwrap()
            .is_empty()
    );
    assert!(bytes(&manager, &command).is_empty());
    assert!(manager.control_snapshot(&command).unwrap().stdin_open());
    manager.stop_all();
}

#[tokio::test]
async fn command_controls_cancel_queue_during_busy_owner_has_durable_ack_and_never_writes_input() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _workspace, owner, manager, command) = fixture(root.path());
    open(&mut app, &owner, &command).await;
    let mut editor = tree(&app, 900.0);
    paste(
        &mut app,
        &mut editor,
        900.0,
        "native_command_control_input",
        "must stay queued ñ\r\n",
    );
    let mut draft = tree(&app, 900.0);
    click(
        &mut app,
        &mut draft,
        900.0,
        "native_command_control_prepare_input",
    );
    let lease = app
        .store
        .as_ref()
        .unwrap()
        .try_acquire_native_lease(&owner)
        .unwrap();
    let mut confirmation = tree(&app, 900.0);
    click(
        &mut app,
        &mut confirmation,
        900.0,
        "native_command_control_confirm",
    );
    poll_until(&mut app, |app| {
        app.native_manual_command_response.is_none()
            && app.view.native_command_control.operations.len() == 1
    })
    .await;
    let operation_id = app.view.native_command_control.operations[0]
        .request
        .operation_id
        .clone();
    assert_eq!(
        app.view.native_command_control.operations[0].state,
        NativeCommandOperationState::Requested
    );
    let cancel_id = format!("native_command_control_cancel:{operation_id}");
    let mut pending = tree(&app, 900.0);
    click(&mut app, &mut pending, 900.0, &cancel_id);
    assert!(app.view.native_command_control.error.is_none());
    let store = NativeCommandOperationStore::new(app.store.as_ref().unwrap().clone());
    poll_until(&mut app, |app| {
        app.native_command_control_response.is_none()
            && matches!(
                app.view.native_command_control.phase,
                NativeCommandControlPhase::Draft
            )
    })
    .await;
    let saved = store.snapshot(&owner, &operation_id).unwrap().unwrap();
    assert!(matches!(
        saved.state,
        NativeCommandOperationState::Cancelled { .. }
    ));
    assert!(saved.effect_result.is_none());
    assert!(
        saved.phase_diagnostics().is_empty(),
        "A cancelled intention never started a pre-hook or process effect"
    );
    assert!(bytes(&manager, &command).is_empty());
    assert!(!app.native_command_poll_owners.contains(&owner));
    assert!(
        app.store
            .as_ref()
            .unwrap()
            .native_session_for_lease(&lease)
            .is_ok(),
        "Withdrawing a queued intention neither acquires nor revokes the active owner's lease"
    );
    drop(lease);
    app.poll_native_manual_commands();
    assert!(bytes(&manager, &command).is_empty());
    assert!(app
        .store
        .as_ref()
        .unwrap()
        .load(&owner)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    manager.stop_all();
}

#[tokio::test]
async fn command_controls_late_requested_actor_reply_cannot_regress_cancelled_receipt_or_requeue() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _workspace, owner, manager, command) = fixture(root.path());
    open(&mut app, &owner, &command).await;
    let mut editor = tree(&app, 360.0);
    paste(
        &mut app,
        &mut editor,
        360.0,
        "native_command_control_input",
        "never repeat these bytes ñ\r\n",
    );
    let mut draft = tree(&app, 360.0);
    click(
        &mut app,
        &mut draft,
        360.0,
        "native_command_control_prepare_input",
    );
    let lease = app
        .store
        .as_ref()
        .unwrap()
        .try_acquire_native_lease(&owner)
        .unwrap();
    let mut confirmation = tree(&app, 360.0);
    click(
        &mut app,
        &mut confirmation,
        360.0,
        "native_command_control_confirm",
    );
    poll_until(&mut app, |app| {
        app.native_manual_command_response.is_none()
            && app.view.native_command_control.operations.len() == 1
    })
    .await;
    let store = NativeCommandOperationStore::new(app.store.as_ref().unwrap().clone());
    let older_operations = store.list(&owner).unwrap();
    assert_eq!(
        older_operations[0].state,
        NativeCommandOperationState::Requested
    );
    let operation_id = older_operations[0].request.operation_id.clone();
    let cancel_id = format!("native_command_control_cancel:{operation_id}");
    let mut pending = tree(&app, 360.0);
    click(&mut app, &mut pending, 360.0, &cancel_id);
    poll_until(&mut app, |app| {
        app.native_command_control_response.is_none()
            && matches!(
                app.view.native_command_control.operations[0].state,
                NativeCommandOperationState::Cancelled { .. }
            )
    })
    .await;
    let acknowledged = app.view.native_command_control.operations[0].clone();
    assert!(acknowledged.revision > older_operations[0].revision);
    assert!(matches!(
        app.view.native_command_control.phase,
        NativeCommandControlPhase::Draft
    ));
    assert!(app.view.native_command_control.input.is_empty());
    assert!(!app.native_command_poll_owners.contains(&owner));
    let journal_before = app.store.as_ref().unwrap().load(&owner).unwrap().unwrap();

    // Deliver the actual older list through the actor response channel after
    // the independent inspection has applied the durable cancellation ACK.
    let (tx, rx) = mpsc::channel();
    tx.send((owner.clone(), Ok((older_operations, None))))
        .unwrap();
    app.native_manual_command_response = Some(rx);
    assert!(app.poll_native_manual_commands());
    assert!(app.native_manual_command_response.is_none());
    assert_eq!(
        app.view.native_command_control.operations,
        vec![acknowledged.clone()]
    );
    assert_eq!(
        store.snapshot(&owner, &operation_id).unwrap(),
        Some(acknowledged.clone())
    );
    assert!(matches!(
        app.view.native_command_control.phase,
        NativeCommandControlPhase::Draft
    ));
    assert!(app.view.native_command_control.input.is_empty());
    assert!(
        !app.native_command_poll_owners.contains(&owner),
        "A late Requested snapshot cannot requeue a cancelled intention"
    );
    for width in [360.0, 900.0] {
        let mut view = tree(&app, width);
        assert!(!view.focus(&cancel_id));
        assert!(!view.focus("native_command_control_confirm"));
        assert!(
            !view.focus("native_command_control_prepare_input"),
            "Cancelled input is no longer an unsent draft"
        );
    }
    assert!(acknowledged.effect_result.is_none() && acknowledged.phase_diagnostics().is_empty());
    assert_eq!(
        app.store.as_ref().unwrap().load(&owner).unwrap().unwrap(),
        journal_before
    );
    assert!(bytes(&manager, &command).is_empty());
    drop(lease);
    app.poll_native_manual_commands();
    assert!(bytes(&manager, &command).is_empty());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
    manager.stop_all();
}

#[tokio::test]
async fn command_controls_reopen_unknown_history_without_manager_resolve_requires_lease_and_exact_revision(
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let data = root.path().join("data");
    let app = AgentsWindow::with_data_root(data.clone());
    let sessions = app.store.as_ref().unwrap().clone();
    let owner = sessions.create(&workspace, "native").unwrap().id;
    let store = NativeCommandOperationStore::new(sessions.clone());
    let request = NativeCommandOperationRequest {
        operation_id: uuid::Uuid::new_v4().to_string(),
        session_id: owner.clone(),
        workspace: workspace.canonicalize().unwrap(),
        manager_generation: uuid::Uuid::new_v4().to_string(),
        command_id: uuid::Uuid::new_v4().to_string(),
        action: NativeCommandOperationAction::Stdin {
            input: "original ñ🦀\r\n\r\0".into(),
            close_after: false,
        },
        policy_digest: "a".repeat(64),
        hooks_digest: "b".repeat(64),
    };
    let lease = sessions.try_acquire_native_lease(&owner).unwrap();
    store.enqueue_requested(request.clone()).unwrap();
    store
        .preflight_capacity(&lease, &request.operation_id, 1024 * 1024)
        .unwrap();
    store
        .begin_started(&lease, &request.operation_id, 0)
        .unwrap();
    drop(lease);
    drop(app);
    let mut app = AgentsWindow::with_data_root(data);
    select_owner(&mut app, &owner, &workspace);
    assert!(app.native_command_sessions.manager(&owner).is_none());
    apply(&mut app, "native_command_operation_history");
    poll_until(&mut app, |app| {
        app.native_command_control_response.is_none()
            && app.view.native_command_control.operations.len() == 1
    })
    .await;
    assert!(
        app.view.native_command_control.scope.is_none()
            && app.view.native_command_control.observed.is_none()
    );
    apply(
        &mut app,
        &format!("native_command_control_cancel:{}", request.operation_id),
    );
    assert!(
        app.view.native_command_control.error.is_some(),
        "A stale cancel action cannot reinterpret Started as a cancelled intention"
    );
    assert!(store
        .snapshot(&owner, &request.operation_id)
        .unwrap()
        .unwrap()
        .state
        .requires_verification());
    app.view.native_command_control.error = None;
    let mut history = tree(&app, 360.0);
    click(
        &mut app,
        &mut history,
        360.0,
        &format!("native_command_operation_select:{}", request.operation_id),
    );
    assert!(!app.view.native_command_control.can_resolve_operation());
    let mut history = tree(&app, 360.0);
    paste(
        &mut app,
        &mut history,
        360.0,
        "native_command_operation_acknowledgment",
        "Inspección humana externa: el proceso no se restauró; no reenvío bytes",
    );
    let busy = sessions.try_acquire_native_lease(&owner).unwrap();
    let mut history = tree(&app, 360.0);
    click(
        &mut app,
        &mut history,
        360.0,
        "native_command_operation_resolve",
    );
    assert!(app.view.native_command_control.error.is_some());
    assert!(store
        .snapshot(&owner, &request.operation_id)
        .unwrap()
        .unwrap()
        .state
        .requires_verification());
    store
        .record_phase(
            &busy,
            &request.operation_id,
            1,
            NativeCommandOperationPhase::BeforeCommand,
            "A durable phase was recorded after the displayed revision",
        )
        .unwrap();
    drop(busy);
    app.view.native_command_control.error = None;
    let mut history = tree(&app, 900.0);
    click(
        &mut app,
        &mut history,
        900.0,
        "native_command_operation_resolve",
    );
    assert!(
        app.view.native_command_control.error.is_some(),
        "Stale exact revision must be rejected"
    );
    assert_eq!(
        store
            .snapshot(&owner, &request.operation_id)
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    apply(&mut app, "native_command_control_refresh");
    poll_until(&mut app, |app| {
        app.native_command_control_response.is_none()
            && app.view.native_command_control.operations[0].revision == 2
    })
    .await;
    let mut history = tree(&app, 900.0);
    click(
        &mut app,
        &mut history,
        900.0,
        "native_command_operation_resolve",
    );
    poll_until(&mut app, |app| {
        app.native_command_control_response.is_none()
            && matches!(
                app.view.native_command_control.operations[0].state,
                NativeCommandOperationState::Resolved { .. }
            )
    })
    .await;
    let saved = store
        .snapshot(&owner, &request.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.request, request);
    assert!(
        saved.effect_result.is_none(),
        "Acknowledgment cannot manufacture an effect result"
    );
    assert!(app.native_command_sessions.manager(&owner).is_none());
    assert!(sessions.load(&owner).unwrap().unwrap().events.is_empty());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}
