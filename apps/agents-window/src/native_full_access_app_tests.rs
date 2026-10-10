use super::*;
use crate::agent_tool_settings::ALL_TOOLS;
use crate::composer_permissions_ui::{self, ComposerPermissionPreset};
use openide_harness::LocalTool;
use openide_ui::{DrawCommand, HitKind, Key, Point, UiEvent};
use serde_json::json;

fn fixture(root: &Path) -> AgentsWindow {
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let data = root.join("data");
    let mut settings = AgentToolSettings::default();
    settings.set(LocalTool::ReadFile, false);
    settings.set_command_isolation(CommandIsolation::WorkspaceWriteOffline);
    settings.save(&data).unwrap();
    let mut app = AgentsWindow::with_data_root(data);
    let owner = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap()
        .id;
    app.view.workspace = workspace.display().to_string();
    app.view.selected_backend = "native".into();
    app.view.selected_session = Some(owner);
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app
}

fn apply(app: &mut AgentsWindow, id: &str) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

fn prepare(app: &mut AgentsWindow) -> (String, String) {
    apply(app, "composer_permissions:full_access");
    let token = app
        .view
        .composer_full_access_confirm
        .as_ref()
        .expect("full access requires an explicit confirmation")
        .token;
    (
        format!("composer_full_access_confirm:{token}"),
        format!("composer_full_access_cancel:{token}"),
    )
}

fn settings_bytes(app: &AgentsWindow) -> Vec<u8> {
    std::fs::read(app.data_root.join("agent-tool-settings.json")).unwrap()
}

fn assert_policy_unchanged(app: &AgentsWindow, settings: AgentToolSettings, bytes: &[u8]) {
    assert_eq!(app.agent_tool_settings, settings);
    assert_eq!(app.view.native_tool_permissions, settings.permissions());
    assert_eq!(app.view.native_full_access, settings.full_access());
    assert_eq!(
        app.view.native_command_isolation,
        settings.command_isolation()
    );
    assert_eq!(settings_bytes(app), bytes);
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn full_access_opens_a_scoped_alert_without_changing_policy_disk_or_journal() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let baseline = app.agent_tool_settings;
    let bytes = settings_bytes(&app);
    let owner = app.view.selected_session.clone().unwrap();
    let events = app
        .store
        .as_ref()
        .unwrap()
        .load(&owner)
        .unwrap()
        .unwrap()
        .events;
    app.view.composer_permissions_open = true;
    let (confirm, cancel) = prepare(&mut app);
    assert!(!app.view.composer_permissions_open);
    assert_policy_unchanged(&app, baseline, &bytes);
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load(&owner)
            .unwrap()
            .unwrap()
            .events,
        events
    );

    for width in [360.0, 900.0] {
        let viewport = Size::new(width, 720.0);
        let mut tree = UiTree::new(crate::view::build_view(&app.view, width));
        tree.set_reduced_motion(true);
        let frame = tree.layout(viewport);
        let surface = frame
            .regions
            .iter()
            .find(|region| region.id == format!("popover_surface:{cancel}"))
            .expect("the app mounts its own openide-ui AlertDialog")
            .rect;
        assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "¿Permitir todo?"
        )));
        for id in [&confirm, &cancel] {
            let target = frame
                .regions
                .iter()
                .find(|region| region.id == *id && region.kind == HitKind::Button)
                .expect("tokenized confirmation and cancellation are visible")
                .rect;
            assert!(target.x >= surface.x && target.x + target.width <= surface.x + surface.width);
            assert!(
                target.y >= surface.y && target.y + target.height <= surface.y + surface.height
            );
            let point = Point::new(
                target.x + target.width / 2.0,
                target.y + target.height / 2.0,
            );
            assert_eq!(frame.hit_test(point), Some(id.as_str()));
        }
        assert!(tree
            .handle_event(UiEvent::PointerDown(Point::new(4.0, 4.0)))
            .is_empty());
        assert!(tree
            .handle_event(UiEvent::PointerUp(Point::new(4.0, 4.0)))
            .is_empty());
    }
}

#[test]
fn full_access_cancel_and_escape_dismiss_without_changing_permissions() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let baseline = app.agent_tool_settings;
    let bytes = settings_bytes(&app);
    let (_, cancel) = prepare(&mut app);
    apply(&mut app, &cancel);
    assert!(app.view.composer_full_access_confirm.is_none());
    assert_policy_unchanged(&app, baseline, &bytes);

    let (_, cancel) = prepare(&mut app);
    let mut tree = UiTree::new(crate::view::build_view(&app.view, 900.0));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(900.0, 720.0));
    let actions = tree.handle_event(UiEvent::Key {
        key: Key::Escape,
        shift: false,
        ctrl: false,
    });
    assert_eq!(actions, vec![UiAction::Clicked { id: cancel }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(app.view.composer_full_access_confirm.is_none());
    assert_policy_unchanged(&app, baseline, &bytes);
}

#[test]
fn full_access_invalid_or_stale_confirmation_and_cancel_tokens_are_inert() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let baseline = app.agent_tool_settings;
    let bytes = settings_bytes(&app);
    let (old_confirm, old_cancel) = prepare(&mut app);
    let (confirm, _) = prepare(&mut app);
    assert_ne!(old_confirm, confirm);
    let pending = app
        .view
        .composer_full_access_confirm
        .as_ref()
        .unwrap()
        .token;
    for id in [
        "composer_full_access_confirm:invalid",
        "composer_full_access_cancel:invalid",
        "composer_full_access_confirm:18446744073709551616",
        old_confirm.as_str(),
        old_cancel.as_str(),
    ] {
        apply(&mut app, id);
        assert_eq!(
            app.view
                .composer_full_access_confirm
                .as_ref()
                .unwrap()
                .token,
            pending
        );
        assert_policy_unchanged(&app, baseline, &bytes);
    }
    apply(&mut app, &confirm);
    assert!(app.agent_tool_settings.full_access());
    let saved = settings_bytes(&app);
    apply(&mut app, &confirm);
    assert_eq!(
        settings_bytes(&app),
        saved,
        "a consumed confirmation cannot apply twice"
    );
}

#[test]
fn reopening_drops_unconfirmed_full_access_tokens_and_never_replays_them() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let baseline = app.agent_tool_settings;
    let bytes = settings_bytes(&app);
    let (confirm, cancel) = prepare(&mut app);
    let data = app.data_root.clone();
    let workspace = app.view.workspace.clone();
    let owner = app.view.selected_session.clone();
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(data);
    reopened.view.workspace = workspace;
    reopened.view.selected_session = owner;
    assert!(reopened.view.composer_full_access_confirm.is_none());
    apply(&mut reopened, &confirm);
    apply(&mut reopened, &cancel);
    assert_policy_unchanged(&reopened, baseline, &bytes);
    assert!(reopened.view.composer_full_access_confirm.is_none());
}

#[test]
fn full_access_exact_confirmation_persists_all_five_tools_and_host_isolation() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let (confirm, _) = prepare(&mut app);
    let mut tree = UiTree::new(crate::view::build_view(&app.view, 900.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 720.0));
    let target = frame
        .regions
        .iter()
        .find(|region| region.id == confirm)
        .unwrap()
        .rect;
    let point = Point::new(
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: confirm }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(app.view.composer_full_access_confirm.is_none());
    let saved = AgentToolSettings::load(&app.data_root).unwrap();
    assert_eq!(saved, app.agent_tool_settings);
    assert!(saved.full_access() && app.view.native_full_access);
    assert_eq!(
        saved.command_isolation(),
        CommandIsolation::HostUnrestricted
    );
    assert_eq!(
        app.view.native_command_isolation,
        CommandIsolation::HostUnrestricted
    );
    assert_eq!(app.view.native_tool_permissions, saved.permissions());
    for tool in ALL_TOOLS {
        assert!(saved.enabled(tool));
        assert_eq!(saved.mode_for(tool), PermissionMode::Allow);
    }
    let reopened = AgentsWindow::with_data_root(app.data_root.clone());
    assert_eq!(reopened.agent_tool_settings, saved);
    assert!(reopened.view.native_full_access);
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn full_access_confirmation_rejects_changed_owner_activity_navigation_or_settings() {
    for changed in [
        "workspace",
        "session",
        "running",
        "background",
        "backend",
        "settings_page",
        "main_page",
        "in_memory_settings",
        "saved_settings",
        "corrupt_saved_settings",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        let baseline = app.agent_tool_settings;
        let (confirm, _) = prepare(&mut app);
        match changed {
            "workspace" => app.view.workspace.push_str("-different"),
            "session" => app.view.selected_session = None,
            "running" => app.view.running = true,
            "background" => app.view.background_run_selected = true,
            "backend" => app.view.selected_backend = "codex".into(),
            "settings_page" => app.view.show_settings = true,
            "main_page" => app.view.main_page = MainPage::Marketplace,
            "in_memory_settings" => app.agent_tool_settings.set(LocalTool::SearchFiles, false),
            "saved_settings" => {
                let mut saved = baseline;
                saved.set(LocalTool::SearchFiles, false);
                saved.save(&app.data_root).unwrap();
            }
            "corrupt_saved_settings" => {
                std::fs::write(app.data_root.join("agent-tool-settings.json"), b"{broken").unwrap();
            }
            _ => unreachable!(),
        }
        let current = app.agent_tool_settings;
        let bytes = settings_bytes(&app);
        let visible_permissions = app.view.native_tool_permissions;
        let visible_full_access = app.view.native_full_access;
        let visible_isolation = app.view.native_command_isolation;
        apply(&mut app, &confirm);
        assert!(app.view.composer_full_access_confirm.is_none(), "{changed}");
        assert_eq!(app.agent_tool_settings, current, "{changed}");
        assert_eq!(
            app.view.native_tool_permissions, visible_permissions,
            "{changed}"
        );
        assert_eq!(
            app.view.native_full_access, visible_full_access,
            "{changed}"
        );
        assert_eq!(
            app.view.native_command_isolation, visible_isolation,
            "{changed}"
        );
        assert_eq!(settings_bytes(&app), bytes, "{changed}");
        assert!(app.active_run.is_none() && app.active_resume.is_none());
        assert!(!app.agent_tool_settings.full_access(), "{changed}");
    }
}

#[test]
fn full_access_failed_settings_commit_preserves_current_permissions_and_original_settings() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let baseline = app.agent_tool_settings;
    let original_root = app.data_root.clone();
    let bytes = settings_bytes(&app);
    let blocked = root.path().join("not-a-settings-directory");
    std::fs::write(&blocked, b"must stay a file").unwrap();
    app.data_root = blocked.clone();
    let next = composer_permissions_ui::settings_for_preset(
        baseline,
        ComposerPermissionPreset::FullAccess,
    );
    assert!(!app.commit_agent_tool_settings(next, "Access should not commit"));
    assert_eq!(app.agent_tool_settings, baseline);
    assert_eq!(app.view.native_tool_permissions, baseline.permissions());
    assert_eq!(app.view.native_full_access, baseline.full_access());
    assert_eq!(
        app.view.native_command_isolation,
        baseline.command_isolation()
    );
    assert!(app.view.settings_notice_error);
    assert_eq!(std::fs::read(blocked).unwrap(), b"must stay a file");
    assert_eq!(
        std::fs::read(original_root.join("agent-tool-settings.json")).unwrap(),
        bytes
    );
}

#[test]
fn read_only_and_ask_disable_full_access_and_restore_per_operation_policy() {
    for preset in ["composer_permissions:read_only", "composer_permissions:ask"] {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        let (confirm, _) = prepare(&mut app);
        apply(&mut app, &confirm);
        assert!(app.agent_tool_settings.full_access());
        apply(&mut app, preset);
        let saved = AgentToolSettings::load(&app.data_root).unwrap();
        assert_eq!(saved, app.agent_tool_settings);
        assert!(!saved.full_access() && !app.view.native_full_access);
        let expected = if preset == "composer_permissions:ask" {
            PermissionMode::Ask
        } else {
            PermissionMode::Deny
        };
        for tool in [LocalTool::WriteFile, LocalTool::RunCommand] {
            assert_eq!(saved.mode_for(tool), expected);
        }
    }
}

#[test]
fn full_access_keeps_saved_exec_ask_and_deny_rules_authoritative() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let settings = ExecPolicySettings {
        rules: vec![
            openide_harness::ExecRule::new(
                vec![openide_harness::ExecPatternToken::Literal(
                    "ask-probe".into(),
                )],
                ExecDecision::Ask,
                Some("Still require approval".into()),
            )
            .unwrap(),
            openide_harness::ExecRule::new(
                vec![openide_harness::ExecPatternToken::Literal(
                    "deny-probe".into(),
                )],
                ExecDecision::Deny,
                Some("Still forbidden".into()),
            )
            .unwrap(),
        ],
    };
    settings.save(&app.data_root).unwrap();
    let original_rules = std::fs::read(app.data_root.join("native-exec-rules.json")).unwrap();
    let (confirm, _) = prepare(&mut app);
    apply(&mut app, &confirm);
    let policy = app.native_permission_policy().unwrap();
    let workspace = PathBuf::from(&app.view.workspace);
    let owner = app.view.selected_session.as_deref().unwrap();
    for (command, expected) in [
        ("ask-probe", PermissionMode::Ask),
        ("deny-probe", PermissionMode::Deny),
        ("unmatched-probe", PermissionMode::Allow),
    ] {
        let call = ToolCall {
            id: command.into(),
            name: "run_command".into(),
            arguments: json!({"command": command, "args": []}),
        };
        assert_eq!(
            policy.mode_for_call_with_session_grants(
                owner,
                &workspace,
                &workspace,
                &policy.digest_hex(),
                &call,
                &[]
            ),
            expected
        );
    }
    assert_eq!(
        std::fs::read(app.data_root.join("native-exec-rules.json")).unwrap(),
        original_rules
    );
    assert!(app
        .store
        .as_ref()
        .unwrap()
        .load(owner)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn full_access_confirmation_blocks_message_admission_before_model_or_network_preparation() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let owner = app.view.selected_session.clone().unwrap();
    app.view.prompt = "Keep this draft while confirming permissions".into();
    app.view.composer_error = Some("Existing composer notice".into());
    let before = app.store.as_ref().unwrap().load(&owner).unwrap().unwrap();
    let (confirm, _) = prepare(&mut app);
    let baseline = app.agent_tool_settings;
    let bytes = settings_bytes(&app);
    app.start_run();
    assert_eq!(
        app.view.prompt,
        "Keep this draft while confirming permissions"
    );
    assert_eq!(
        app.view.composer_error.as_deref(),
        Some("Existing composer notice")
    );
    assert_eq!(
        app.view
            .composer_full_access_confirm
            .as_ref()
            .unwrap()
            .token
            .to_string(),
        confirm
            .strip_prefix("composer_full_access_confirm:")
            .unwrap()
    );
    assert_eq!(
        app.store.as_ref().unwrap().load(&owner).unwrap().unwrap(),
        before
    );
    assert_policy_unchanged(&app, baseline, &bytes);
    assert!(!app.view.running);
}
