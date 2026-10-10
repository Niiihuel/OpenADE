//! Composer policy changes never revoke another conversation's live commands.

use super::*;
use async_trait::async_trait;
use openide_harness::{AgentError, ModelOutput, ToolDefinition};
use tokio::sync::Notify;

struct HeldChildModel {
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl ModelBackend for HeldChildModel {
    fn model_id(&self) -> &str {
        "composer-child-fixture"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([91; 32])
    }

    async fn complete(
        &self,
        _messages: &[AgentMessage],
        _tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        self.entered.notify_one();
        tokio::select! {
            _ = cancel.cancelled() => Err(AgentError::Cancelled),
            _ = self.release.notified() => Ok(ModelOutput { content: "Listo".into(), tool_calls: Vec::new() }),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_live_child_queues_policy_until_its_worker_releases() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut initial_settings = AgentToolSettings::default();
    initial_settings.set(openide_harness::LocalTool::WriteFile, true);
    initial_settings.set(openide_harness::LocalTool::RunCommand, true);
    initial_settings.save(&data).unwrap();
    let mut app = AgentsWindow::with_data_root(data.clone());
    let sessions = app.store.clone().unwrap();
    let parent = sessions.create(root.path(), "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap());
    let model = Arc::new(HeldChildModel {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let worker = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model.clone(),
        Arc::new(LocalTools::new().with_write_access(false)),
        "fixture",
        "https://fixture.invalid/v1",
    )
    .with_event_bridge(app.subagent_bridge.clone())
    .with_system_instructions("In-process composer scope fixture; no network or shell.");
    let child = worker
        .prepare_child(&lease, "composer-child", "Inspect", TurnMode::Build)
        .unwrap();
    let child_id = child.session.id.clone();
    let control = coordinator.issue_control(&lease).unwrap();
    drop(lease);
    let running_id = child_id.clone();
    let running = tokio::spawn(async move {
        worker
            .run_prepared(
                control,
                running_id,
                "composer-child".into(),
                "Inspect".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), model.entered.notified())
        .await
        .unwrap();
    app.open_session(&child_id);
    assert!(
        !app.view.running,
        "child streams do not occupy the root worker slot"
    );
    assert!(app.active_run.is_none());
    assert!(app.composer_selection_busy());
    let baseline = app.agent_tool_settings;
    let saved = std::fs::read(data.join("agent-tool-settings.json")).unwrap();
    app.apply_action(
        UiAction::Clicked {
            id: "composer_permissions:read_only".into(),
        },
        &egui::Context::default(),
    );
    assert!(app.view.composer_pending.is_some());
    assert_eq!(app.agent_tool_settings, baseline);
    assert_eq!(
        std::fs::read(data.join("agent-tool-settings.json")).unwrap(),
        saved
    );
    assert!(!app.poll_pending_composer_selection());
    assert!(app.subagent_bridge.active_child_ids().contains(&child_id));
    model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!app.composer_selection_busy());
    assert!(app.poll_pending_composer_selection());
    assert!(app.view.composer_pending.is_none());
    assert!(!app.agent_tool_settings.permissions().run_command);
    assert!(!app.agent_tool_settings.permissions().write_file);
}

#[test]
fn immediate_composer_policy_only_revokes_the_selected_command_owner() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut settings = AgentToolSettings::default();
    settings.set(openide_harness::LocalTool::RunCommand, true);
    settings.set_command_isolation(CommandIsolation::HostUnrestricted);
    settings.save(&data).unwrap();
    let mut app = AgentsWindow::with_data_root(data);
    let sessions = app.store.clone().unwrap();
    let selected = sessions.create(root.path(), "native").unwrap();
    let other = sessions.create(root.path(), "native").unwrap();
    app.open_session(&selected.id);
    let selected_manager = app
        .native_command_sessions
        .for_session(
            &selected.id,
            root.path(),
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    let other_manager = app
        .native_command_sessions
        .for_session(
            &other.id,
            root.path(),
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&selected_manager, &other_manager));
    app.apply_action(
        UiAction::Clicked {
            id: "composer_permissions:read_only".into(),
        },
        &egui::Context::default(),
    );
    assert!(!app.agent_tool_settings.permissions().run_command);
    assert!(app.native_command_sessions.manager(&selected.id).is_none());
    assert!(Arc::ptr_eq(
        &other_manager,
        &app.native_command_sessions
            .manager(&other.id)
            .expect("other conversation keeps its admitted authority")
    ));
    assert!(app.view.composer_pending.is_none());
}

#[test]
fn failed_composer_policy_save_preserves_settings_and_command_ownership() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut app = AgentsWindow::with_data_root(data.clone());
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(root.path(), "native")
        .unwrap();
    app.open_session(&session.id);
    let manager = app
        .native_command_sessions
        .for_session(
            &session.id,
            root.path(),
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    let baseline = app.agent_tool_settings;
    let mut next = baseline;
    next.set(openide_harness::LocalTool::RunCommand, false);
    // A directory at the file destination deterministically rejects atomic
    // replacement, even when the test process has elevated filesystem rights.
    let settings_path = data.join("agent-tool-settings.json");
    if settings_path.is_file() {
        std::fs::remove_file(&settings_path).unwrap();
    }
    std::fs::create_dir(settings_path).unwrap();
    assert!(!app.commit_composer_tool_settings(next, "fixture"));
    assert_eq!(app.agent_tool_settings, baseline);
    assert!(Arc::ptr_eq(
        &manager,
        &app.native_command_sessions.manager(&session.id).unwrap()
    ));
}
