//! Desktop acceptance for the explicitly selected Linux command profile.
//! These tests use real processes, stdin and OS-denied filesystem/network calls.

use super::*;
use crate::native_command_control_ui::NativeCommandControlPhase;
use openide_core::{
    NativeCommandOperationOutcome, NativeCommandOperationState, NativeCommandOperationStore,
};
use openide_harness::{LocalTool, PersistentCommandEvent, PersistentCommandStream};
use openide_ui::{HitKind, Key, Point};

const OFFLINE_CHOICE: &str = "command_isolation:workspace_write_offline";
const ISOLATION_SELECTOR: &str = "settings_choice_toggle:isolation";
const LINUX_DESCRIPTION: &str = "Linux: Proyecto, sin red limita la escritura al proyecto y bloquea sockets de red. Requiere Bubblewrap y espacios de nombres de usuario habilitados; si no se pueden preparar, el comando se bloquea. Sin aislamiento usa los permisos de tu cuenta.";

fn page_tree(app: &AgentsWindow, width: f32) -> UiTree {
    let mut tree = UiTree::new(settings_ui::build_settings_page(
        &app.view,
        SettingsPage::Agent,
        width,
        true,
    ));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 720.0));
    tree
}

fn control_tree(app: &AgentsWindow, width: f32) -> UiTree {
    let mut tree = UiTree::new(build_view(&app.view, width));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 720.0));
    tree
}

fn visible_point(tree: &mut UiTree, width: f32, id: &str, kind: HitKind, scroll_id: &str) -> Point {
    for _ in 0..40 {
        let frame = tree.layout(Size::new(width, 720.0));
        if let Some(region) = frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == kind)
        {
            let point = Point::new(
                region.rect.x + region.rect.width / 2.0,
                region.rect.y + region.rect.height / 2.0,
            );
            if frame.hit_test(point) == Some(id) {
                assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1);
                return point;
            }
        }
        let scroll = frame
            .regions
            .iter()
            .find(|region| region.id == scroll_id && region.kind == HitKind::Scroll)
            .unwrap()
            .rect;
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(
                scroll.x + scroll.width / 2.0,
                scroll.y + scroll.height / 2.0,
            ),
            delta_y: -160.0,
        });
    }
    panic!("{id} must be reachable at {width}px");
}

fn click(app: &mut AgentsWindow, tree: &mut UiTree, width: f32, id: &str, scroll_id: &str) {
    let point = visible_point(tree, width, id, HitKind::Button, scroll_id);
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

fn activate_by_keyboard(app: &mut AgentsWindow, tree: &mut UiTree, id: &str) {
    for _ in 0..80 {
        if tree.focused_id() == Some(id) {
            break;
        }
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
    }
    assert_eq!(
        tree.focused_id(),
        Some(id),
        "Isolation control is in the real keyboard focus order"
    );
    let actions = tree.handle_event(UiEvent::Key {
        key: Key::Enter,
        shift: false,
        ctrl: false,
    });
    assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

fn open_isolation_selector(app: &mut AgentsWindow, tree: &mut UiTree, width: f32, keyboard: bool) {
    visible_point(
        tree,
        width,
        ISOLATION_SELECTOR,
        HitKind::Button,
        "settings_page:Agent",
    );
    if keyboard {
        activate_by_keyboard(app, tree, ISOLATION_SELECTOR);
    } else {
        click(app, tree, width, ISOLATION_SELECTOR, "settings_page:Agent");
    }
    assert_eq!(
        app.view.settings_choice_menu_open.as_deref(),
        Some("isolation")
    );
    // Rebuild the existing tree so the trigger's scroll position is preserved.
    tree.set_root(settings_ui::build_settings_page(
        &app.view,
        SettingsPage::Agent,
        width,
        true,
    ));
    tree.layout(Size::new(width, 720.0));
}

fn paste(app: &mut AgentsWindow, tree: &mut UiTree, width: f32, exact: &str) {
    let id = "native_command_control_input";
    let point = visible_point(
        tree,
        width,
        id,
        HitKind::TextArea,
        "modal_body:native_command_control_dismiss",
    );
    tree.handle_event(UiEvent::PointerDown(point));
    tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(tree.focused_id(), Some(id));
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

fn apply(app: &mut AgentsWindow, id: &str) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
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

fn assert_policy(settings: AgentToolSettings, workspace: &Path, owner: &str, enabled: bool) {
    let policy = settings.permission_policy();
    for (name, mode) in [
        ("run_command", PermissionMode::Ask),
        ("start_command", PermissionMode::Ask),
        ("poll_command", PermissionMode::Allow),
        ("write_stdin", PermissionMode::Ask),
        ("terminate_command", PermissionMode::Ask),
        ("resize_command", PermissionMode::Deny),
        ("future_unknown_tool", PermissionMode::Deny),
    ] {
        let call = ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: serde_json::json!({"command":"probe","session_id":Uuid::new_v4().to_string(),"input":"check\r\n"}),
        };
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                owner,
                workspace,
                workspace,
                &policy.digest_hex(),
                &call,
                &[]
            ),
            if enabled { mode } else { PermissionMode::Deny },
            "Fresh saved permission for {name}",
        );
    }
}

#[test]
#[ignore = "requires trusted Bubblewrap on PATH; run the Linux desktop acceptance with nixpkgs#bubblewrap"]
fn linux_offline_settings_pointer_keyboard_save_reopen_and_exact_capabilities() {
    assert!(CommandIsolation::WorkspaceWriteOffline.is_available_on_host());
    for width in [360.0, 900.0] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let data = root.path().join("data");
        let mut app = AgentsWindow::with_data_root(data.clone());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Agent;
        assert_eq!(
            app.agent_tool_settings.command_isolation(),
            CommandIsolation::HostUnrestricted,
            "Linux isolation remains an explicit preference"
        );
        let mut tree = page_tree(&app, width);
        open_isolation_selector(&mut app, &mut tree, width, width == 900.0);
        visible_point(
            &mut tree,
            width,
            OFFLINE_CHOICE,
            HitKind::Button,
            "settings_page:Agent",
        );
        let frame = tree.layout(Size::new(width, 720.0));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|layout| layout.source == LINUX_DESCRIPTION));
        if width == 360.0 {
            click(
                &mut app,
                &mut tree,
                width,
                OFFLINE_CHOICE,
                "settings_page:Agent",
            );
        } else {
            activate_by_keyboard(&mut app, &mut tree, OFFLINE_CHOICE);
        }
        assert_eq!(
            app.view.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline
        );
        assert_eq!(
            AgentToolSettings::load(&data).unwrap(),
            app.agent_tool_settings
        );
        let mut tree = page_tree(&app, width);
        click(
            &mut app,
            &mut tree,
            width,
            "agent_tool:run_command",
            "settings_page:Agent",
        );
        let owner = app
            .store
            .as_ref()
            .unwrap()
            .create(&workspace, "native")
            .unwrap()
            .id;
        let manager = app
            .native_command_sessions
            .for_session(
                &owner,
                &workspace,
                app.agent_tool_settings.command_isolation(),
                true,
            )
            .unwrap()
            .unwrap();
        assert_eq!(manager.isolation(), CommandIsolation::WorkspaceWriteOffline);
        assert!(!manager.pty_tools_enabled() && !manager.pty_resize_tools_enabled());
        let definitions = LocalTools::new()
            .with_permissions(app.agent_tool_settings.permissions())
            .with_command_isolation(CommandIsolation::WorkspaceWriteOffline)
            .with_persistent_commands(manager.clone())
            .definitions();
        let names = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            BTreeSet::from([
                "read_file",
                "list_files",
                "search_files",
                "run_command",
                "start_command",
                "poll_command",
                "write_stdin",
                "terminate_command"
            ])
        );
        let start = definitions
            .iter()
            .find(|definition| definition.name == "start_command")
            .unwrap();
        assert!(start.parameters["properties"].get("pty").is_none());
        assert!(start.parameters["properties"].get("columns").is_none());
        assert_policy(app.agent_tool_settings, &workspace, &owner, true);
        let reopened = AgentsWindow::with_data_root(data.clone());
        assert_eq!(reopened.agent_tool_settings, app.agent_tool_settings);
        assert_eq!(
            reopened.view.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline
        );
        assert_policy(reopened.agent_tool_settings, &workspace, &owner, true);
        let mut tree = page_tree(&app, width);
        click(
            &mut app,
            &mut tree,
            width,
            "agent_tool:run_command",
            "settings_page:Agent",
        );
        assert!(app.native_command_sessions.manager(&owner).is_none());
        assert!(manager.list_sessions().unwrap().is_empty());
        assert_policy(app.agent_tool_settings, &workspace, &owner, false);
        let disabled = LocalTools::new()
            .with_permissions(app.agent_tool_settings.permissions())
            .with_command_isolation(CommandIsolation::WorkspaceWriteOffline)
            .with_persistent_commands(manager)
            .definitions();
        assert_eq!(
            disabled
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["read_file", "list_files", "search_files"])
        );
        let reopened = AgentsWindow::with_data_root(data);
        assert_eq!(reopened.agent_tool_settings, app.agent_tool_settings);
        assert!(!reopened.view.native_tool_permissions.run_command);
        assert_eq!(
            reopened.view.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline,
            "Permission downgrade preserves the selected isolation profile"
        );
    }
}

fn probe(workspace: &Path) -> PathBuf {
    let source = workspace.join("linux_offline_probe.rs");
    let binary = workspace.join("linux_offline_probe");
    std::fs::write(&source, r#"
use std::io::{Read, Write};
fn main() {
    let outside = std::env::args().nth(1).unwrap();
    let address = std::env::args().nth(2).unwrap();
    std::fs::write("process-started", b"started").unwrap();
    println!("<READY>");
    std::io::stdout().flush().unwrap();
    let mut input = Vec::new();
    let mut byte = [0u8];
    loop {
        if std::io::stdin().read(&mut byte).unwrap() == 0 {
            println!("<EOF>"); std::io::stdout().flush().unwrap(); return;
        }
        input.push(byte[0]);
        if byte[0] == b'\n' {
            std::fs::write("inside-effect", &input).unwrap();
            let outside_write = std::fs::write(&outside, b"ESCAPED").is_err();
            let symlink_write = std::fs::write("escape/outside-effect", b"ESCAPED").is_err();
            let network_errno = std::net::TcpStream::connect(&address).err().and_then(|error| error.raw_os_error());
            println!("<CHECK outside-write-denied={outside_write} symlink-write-denied={symlink_write} socket-errno={network_errno:?}>");
            std::io::stdout().flush().unwrap(); input.clear();
        }
    }
}
"#).unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(compiler)
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

fn stdout(manager: &PersistentCommandManager, command: &str) -> Vec<u8> {
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

async fn poll_until(app: &mut AgentsWindow, condition: impl Fn(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        app.poll_native_manual_commands();
        if condition(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Linux desktop actor did not reach expected state: {:?}",
            app.view.native_command_control
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_stdout(manager: &PersistentCommandManager, command: &str, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while stdout(manager, command) != expected {
        assert!(
            Instant::now() < deadline,
            "Probe output: {:?}; sessions: {:?}",
            String::from_utf8_lossy(&stdout(manager, command)),
            manager.list_sessions()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn finish(app: &mut AgentsWindow, count: usize) {
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
                        NativeCommandOperationState::Finished {
                            outcome: NativeCommandOperationOutcome::Succeeded { .. }
                        }
                    )
                })
            && matches!(
                app.view.native_command_control.phase,
                NativeCommandControlPhase::Draft
            )
    })
    .await;
}

#[tokio::test]
#[ignore = "requires trusted Bubblewrap and working user namespaces/seccomp/pidfd; run with nixpkgs#bubblewrap"]
async fn linux_offline_manual_actor_real_pipe_blocks_external_writes_and_sockets_and_reopens_receipts(
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("escape")).unwrap();
    let binary = probe(&workspace);
    let outside_effect = outside.join("outside-effect");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let data = root.path().join("data");
    let mut app = AgentsWindow::with_data_root(data.clone());
    let mut settings = page_tree(&app, 360.0);
    click(
        &mut app,
        &mut settings,
        360.0,
        OFFLINE_CHOICE,
        "settings_page:Agent",
    );
    let mut settings = page_tree(&app, 360.0);
    click(
        &mut app,
        &mut settings,
        360.0,
        "agent_tool:run_command",
        "settings_page:Agent",
    );
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
        .for_session(
            &owner,
            &workspace,
            CommandIsolation::WorkspaceWriteOffline,
            true,
        )
        .unwrap()
        .unwrap();
    let command = manager.start(binary.to_str().unwrap(), &[outside_effect.display().to_string(), address], &CancellationToken::new()).expect("This acceptance requires Bubblewrap and working user namespaces; run with nixpkgs#bubblewrap").id;
    wait_stdout(&manager, &command, b"<READY>\n").await;
    app.view.native_commands = manager.list_sessions().unwrap();
    apply(
        &mut app,
        &format!(
            "native_command_control_open:{}",
            serde_json::to_string(&(owner.as_str(), command.as_str())).unwrap()
        ),
    );
    poll_until(&mut app, |app| {
        app.native_command_control_response.is_none()
            && app.view.native_command_control.observed.is_some()
    })
    .await;
    let observed = app.view.native_command_control.observed.as_ref().unwrap();
    assert_eq!(
        observed.stdin,
        crate::native_command_control_ui::NativeCommandInputState::Open
    );
    assert!(!observed.resize_supported && !observed.pty);
    let mut editor = control_tree(&app, 360.0);
    let exact = "check\r\n";
    paste(&mut app, &mut editor, 360.0, exact);
    let mut draft = control_tree(&app, 360.0);
    click(
        &mut app,
        &mut draft,
        360.0,
        "native_command_control_prepare_input",
        "modal_body:native_command_control_dismiss",
    );
    assert!(matches!(
        app.view.native_command_control.phase,
        NativeCommandControlPhase::Confirm { .. }
    ));
    assert!(!workspace.join("inside-effect").exists());
    let lease = app
        .store
        .as_ref()
        .unwrap()
        .try_acquire_native_lease(&owner)
        .unwrap();
    let mut confirmation = control_tree(&app, 900.0);
    click(
        &mut app,
        &mut confirmation,
        900.0,
        "native_command_control_confirm",
        "modal_body:native_command_control_dismiss",
    );
    assert!(matches!(
        app.view.native_command_control.phase,
        NativeCommandControlPhase::Pending { .. }
    ));
    assert!(!workspace.join("inside-effect").exists() && !outside_effect.exists());
    assert_eq!(stdout(&manager, &command), b"<READY>\n");
    drop(lease);
    finish(&mut app, 1).await;
    let checked = b"<READY>\n<CHECK outside-write-denied=true symlink-write-denied=true socket-errno=Some(1)>\n";
    wait_stdout(&manager, &command, checked).await;
    assert_eq!(
        std::fs::read(workspace.join("inside-effect")).unwrap(),
        exact.as_bytes()
    );
    assert!(
        !outside_effect.exists(),
        "Absolute and symlink paths cannot write outside the project"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "No socket reached the host listener"
    );
    let request = &app.view.native_command_control.operations[0].request;
    assert_eq!(request.manager_generation, manager.generation());
    assert_eq!(request.workspace, workspace.canonicalize().unwrap());
    assert_eq!(request.session_id, owner);
    assert!(
        matches!(&request.action, openide_core::NativeCommandOperationAction::Stdin { input, close_after: false } if input.as_bytes() == exact.as_bytes())
    );
    let mut draft = control_tree(&app, 360.0);
    click(
        &mut app,
        &mut draft,
        360.0,
        "native_command_control_prepare_eof",
        "modal_body:native_command_control_dismiss",
    );
    let mut confirmation = control_tree(&app, 360.0);
    click(
        &mut app,
        &mut confirmation,
        360.0,
        "native_command_control_confirm",
        "modal_body:native_command_control_dismiss",
    );
    finish(&mut app, 2).await;
    let mut complete = checked.to_vec();
    complete.extend_from_slice(b"<EOF>\n");
    wait_stdout(&manager, &command, &complete).await;
    assert!(!manager.control_snapshot(&command).unwrap().stdin_open());
    let sessions = app.store.as_ref().unwrap().clone();
    let saved = NativeCommandOperationStore::new(sessions.clone())
        .list(&owner)
        .unwrap();
    assert_eq!(saved.len(), 2);
    assert!(
        sessions.load(&owner).unwrap().unwrap().events.is_empty(),
        "Human controls do not invent model journal events"
    );
    manager.stop_all();
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(data);
    select_owner(&mut reopened, &owner, &workspace);
    assert_eq!(
        reopened.view.native_command_isolation,
        CommandIsolation::WorkspaceWriteOffline
    );
    assert!(reopened.native_command_sessions.manager(&owner).is_none());
    apply(&mut reopened, "native_command_operation_history");
    poll_until(&mut reopened, |app| {
        app.native_command_control_response.is_none()
            && app.view.native_command_control.operations.len() == 2
    })
    .await;
    assert_eq!(reopened.view.native_command_control.operations, saved);
    assert!(reopened.view.native_command_control.observed.is_none());
    assert!(reopened.active_run.is_none() && reopened.active_resume.is_none());
    assert_eq!(
        std::fs::read(workspace.join("inside-effect")).unwrap(),
        exact.as_bytes()
    );
    assert!(!outside_effect.exists());
}

#[tokio::test]
async fn linux_offline_missing_helper_saved_profile_denies_before_payload_without_host_fallback() {
    const CHILD_ROOT: &str = "OPENIDE_TEST_MISSING_BWRAP_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let workspace = root.join("workspace").canonicalize().unwrap();
        let data = root.join("data");
        let binary = workspace.join("linux_offline_probe");
        assert!(CommandIsolation::WorkspaceWriteOffline.is_supported_on_host());
        assert!(!CommandIsolation::WorkspaceWriteOffline.is_available_on_host());
        let mut saved = AgentToolSettings::default();
        saved.set(LocalTool::RunCommand, true);
        saved.set_command_isolation(CommandIsolation::WorkspaceWriteOffline);
        saved.save(&data).unwrap();
        let before = std::fs::read(data.join("agent-tool-settings.json")).unwrap();
        let mut app = AgentsWindow::with_data_root(data.clone());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Agent;
        assert_eq!(app.agent_tool_settings, saved);
        assert_eq!(
            app.view.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline
        );
        assert!(
            app.view.native_tool_permissions.run_command,
            "The stored preference is preserved while its unavailable profile is blocked"
        );
        let owner = app
            .store
            .as_ref()
            .unwrap()
            .create(&workspace, "native")
            .unwrap()
            .id;
        assert_policy(saved, &workspace, &owner, false);
        for width in [360.0, 900.0] {
            let mut tree = page_tree(&app, width);
            open_isolation_selector(&mut app, &mut tree, width, false);
            visible_point(
                &mut tree,
                width,
                "command_isolation:host_unrestricted",
                HitKind::Button,
                "settings_page:Agent",
            );
            let frame = tree.layout(Size::new(width, 720.0));
            assert!(frame.text_layouts().iter().any(|layout| layout
                .source
                .contains("No se detectó un ejecutable compatible")));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == OFFLINE_CHOICE && region.kind == HitKind::Button));
            assert!(
                !tree.focus(OFFLINE_CHOICE),
                "Unavailable choice cannot receive keyboard activation"
            );
            let actions = tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            });
            assert_eq!(
                actions,
                vec![UiAction::Clicked {
                    id: "settings_choice_dismiss".into()
                }]
            );
            for action in actions {
                app.apply_action(action, &egui::Context::default());
            }
            assert!(app.view.settings_choice_menu_open.is_none());
            assert_eq!(app.agent_tool_settings, saved);
            assert_eq!(
                std::fs::read(data.join("agent-tool-settings.json")).unwrap(),
                before
            );
        }
        apply(&mut app, OFFLINE_CHOICE);
        assert!(
            app.view.settings_notice_error,
            "A stale unavailable UI action is rejected"
        );
        assert_eq!(app.agent_tool_settings, saved);
        assert_eq!(
            std::fs::read(data.join("agent-tool-settings.json")).unwrap(),
            before
        );
        assert!(app
            .native_command_sessions
            .for_session(
                &owner,
                &workspace,
                CommandIsolation::WorkspaceWriteOffline,
                true
            )
            .unwrap()
            .is_none());
        let manager = PersistentCommandManager::new(
            workspace.clone(),
            CommandIsolation::WorkspaceWriteOffline,
            Default::default(),
        )
        .unwrap();
        let args = [
            root.join("outside-effect").display().to_string(),
            "127.0.0.1:9".into(),
        ];
        let error = manager
            .start(binary.to_str().unwrap(), &args, &CancellationToken::new())
            .unwrap_err();
        assert!(
            matches!(&error, openide_harness::AgentError::Tool(message) if message.contains("Bubblewrap") && message.contains("not installed")),
            "Actual launch must report unavailable trusted helper: {error}"
        );
        assert!(manager.list_sessions().unwrap().is_empty());
        let output = LocalTools::new()
            .with_permissions(saved.permissions())
            .with_command_isolation(CommandIsolation::WorkspaceWriteOffline)
            .execute(
                &ToolCall {
                    id: "missing-helper-one-shot".into(),
                    name: "run_command".into(),
                    arguments: serde_json::json!({"command":binary,"args":args,"timeout_ms":1000}),
                },
                &openide_harness::ToolContext {
                    workspace: workspace.clone(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&output, openide_harness::AgentError::Tool(message) if message.contains("Bubblewrap") && message.contains("not installed")),
            "One-shot launch also fails before payload: {output}"
        );
        assert!(!workspace.join("process-started").exists());
        assert!(!workspace.join("inside-effect").exists());
        assert!(!root.join("outside-effect").exists());
        assert!(
            NativeCommandOperationStore::new(app.store.as_ref().unwrap().clone())
                .list(&owner)
                .unwrap()
                .is_empty()
        );
        assert!(app
            .store
            .as_ref()
            .unwrap()
            .load(&owner)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
        let reopened = AgentsWindow::with_data_root(data);
        assert_eq!(reopened.agent_tool_settings, saved);
        assert_eq!(
            reopened.view.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline
        );
        assert!(reopened.native_command_sessions.manager(&owner).is_none());
        return;
    }
    // Per-process PATH isolation avoids changing the environment of parallel tests.
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let unavailable_path = root.path().join("empty-executable-directory");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&unavailable_path).unwrap();
    probe(&workspace);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("app::native_linux_command_app_tests::linux_offline_missing_helper_saved_profile_denies_before_payload_without_host_fallback")
        .arg("--nocapture")
        .env(CHILD_ROOT, root.path())
        .env("PATH", &unavailable_path)
        .output().unwrap();
    assert!(
        output.status.success(),
        "Missing-helper desktop subprocess failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.join("process-started").exists());
    assert!(!workspace.join("inside-effect").exists());
}
