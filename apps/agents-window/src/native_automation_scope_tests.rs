//! Execution-only scope checks must not make retained automation history unreadable.
//! A loopback listener observes attempted transport without account credentials.

use super::*;
use crate::native_automation::FrozenNativeAutomationAuthorityV1;
use std::{collections::HashMap, fs, net::TcpListener, os::unix::fs::symlink};

const MODEL: &str = "automation-scope/exact-model";
const PROMPT: &str = "Read report.txt from the original registered project.";

fn assert_no_transport(listener: &TcpListener) {
    match listener.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(_) => panic!("automation attempted transport after its workspace changed"),
        Err(error) => panic!("scope fixture accept failed: {error}"),
    }
}

#[test]
fn automation_rejects_replaced_workspace_before_admission_and_worker_effects_but_keeps_history_readable(
) {
    for admitted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("data");
        let workspace = directory.path().join("original-project");
        let moved = directory.path().join("retained-original-project");
        let foreign = directory.path().join("foreign-project");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        fs::write(workspace.join("report.txt"), b"ORIGINAL_PROJECT_REPORT").unwrap();
        fs::write(
            foreign.join("report.txt"),
            b"FOREIGN_PROJECT_MUST_NOT_BE_READ",
        )
        .unwrap();
        let original_workspace = workspace.canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let settings = NativeProviderSettings {
            provider_id: "custom".into(),
            model_id: MODEL.into(),
            custom_endpoint: endpoint.clone(),
        };
        settings.save(&root).unwrap();
        let mut app = AgentsWindow::with_data_root(root.clone());
        app.change_workspace(workspace.display().to_string());
        app.view.selected_backend = "native".into();
        app.view.native_provider_id = "custom".into();
        app.view.model = MODEL.into();
        app.view.composer_model = MODEL.into();
        app.view.composer_effort = "default".into();
        app.view.endpoint = endpoint;
        let automation_id = Uuid::new_v4().to_string();
        let at_ms = 2_000_000;
        let run_id = format!("{automation_id}:{at_ms}");
        let frozen = FrozenNativeAutomationAuthorityV1::freeze(
            settings.resolve_at(&root).unwrap(),
            "custom",
            &root,
            &workspace,
            None,
            &[],
            None,
            None,
            "ORIGINAL_FROZEN_PROJECT_INSTRUCTIONS".into(),
            PROMPT.into(),
            &run_id,
        )
        .unwrap();
        // Build the model before replacing the directory, so the direct worker
        // below must enforce its own scope gate, independent of model().
        let model = frozen.model().unwrap();
        let store = app.store.clone().unwrap();
        let session = store
            .create_native_automation(
                &workspace,
                frozen
                    .to_origin(&automation_id, &run_id, at_ms, false)
                    .unwrap(),
            )
            .unwrap();
        if admitted {
            store
                .admit_native_turn(
                    &session.id,
                    frozen.first_turn_id(),
                    frozen.prompt(),
                    frozen.model_id(),
                    TurnMode::Build,
                )
                .unwrap();
        }
        let before = store.load(&session.id).unwrap().unwrap();
        let metadata_path = root
            .join("sessions")
            .join(format!("{}.session.json", session.id));
        // StoredSession equality below also verifies the complete immutable
        // origin and the journal; the metadata file comparison adds raw bytes.
        let metadata_before = fs::read(&metadata_path).unwrap();
        let legacy_objective = "LEGACY_META_MUST_NOT_CHANGE_THIS_AUTOMATION";
        let legacy_goals = HashMap::from([(
            (
                session.workspace.to_string_lossy().into_owned(),
                Some(session.id.clone()),
            ),
            legacy_objective.to_owned(),
        )]);
        save_composer_goals(&root, &legacy_goals).unwrap();
        let goals_path = root.join("composer-goals.json");
        let goals_before = fs::read(&goals_path).unwrap();
        drop(app);
        fs::rename(&workspace, &moved).unwrap();
        symlink(&foreign, &workspace).unwrap();
        assert_ne!(workspace.canonicalize().unwrap(), original_workspace);

        let loaded = store.load(&session.id).unwrap().unwrap();
        assert_eq!(loaded, before);
        assert_eq!(
            FrozenNativeAutomationAuthorityV1::from_session(&loaded.session).unwrap(),
            Some(frozen.clone()),
            "retained history must not reread or reinterpret a replaced workspace"
        );
        let scope_error = frozen.validate_workspace_for_execution().unwrap_err();
        assert!(frozen.model().is_err());
        assert_no_transport(&listener);

        let mut reopened = AgentsWindow::with_data_root(root.clone());
        reopened.open_session(&session.id);
        assert_eq!(
            reopened.view.selected_session.as_deref(),
            Some(session.id.as_str())
        );
        assert!(reopened
            .view
            .sessions
            .iter()
            .any(|visible| visible.id == session.id && visible.native_automation.is_some()));
        if !admitted {
            assert_eq!(
                reopened.view.session_recovery.get(&session.id),
                Some(&SessionRecoveryView::StartAutomation)
            );
        }
        // Delayed actions from an earlier interactive view must not mutate
        // legacy goal text or announce a change to frozen automation authority.
        reopened.view.composer_goal_editor_open = true;
        reopened.view.composer_goal_draft = "DELAYED_GOAL_MUST_NOT_BE_SAVED".into();
        let ctx = egui::Context::default();
        for id in ["composer_goal_clear", "composer_goal_save"] {
            reopened.apply_action(UiAction::Clicked { id: id.into() }, &ctx);
            assert_eq!(reopened.composer_goals, legacy_goals);
            assert_eq!(fs::read(&goals_path).unwrap(), goals_before);
            assert_eq!(
                reopened.view.composer_goal.as_deref(),
                Some(legacy_objective)
            );
            assert!(
                reopened.view.status.contains("perfil de lectura"),
                "{}",
                reopened.view.status
            );
            assert_eq!(store.load(&session.id).unwrap().unwrap(), before);
            assert_eq!(fs::read(&metadata_path).unwrap(), metadata_before);
            assert_no_transport(&listener);
        }
        reopened.start_resume();
        assert!(reopened.active_run.is_none(), "{}", reopened.view.status);
        assert!(reopened.active_resume.is_none(), "{}", reopened.view.status);
        assert!(
            reopened.view.status.contains(&scope_error),
            "{}",
            reopened.view.status
        );
        assert_eq!(store.load(&session.id).unwrap().unwrap(), before);
        assert_eq!(fs::read(&metadata_path).unwrap(), metadata_before);
        assert_no_transport(&listener);

        if admitted {
            let checkpoints = Arc::new(CheckpointFileStore::new(root.join("sessions")).unwrap());
            let worker = ResumeWorkerHandle::start_with_command_actor(
                ResumeWorkerRequest {
                    session_id: session.id.clone(),
                    provider: frozen.provider_id().into(),
                    endpoint: frozen.endpoint().into(),
                    compatibility: frozen.compatibility(),
                    model,
                    base_tools: frozen.tools(),
                    child_tools: frozen.tools(),
                    child_command_profile: None,
                    event_bridge: None,
                    tool_preparation: None,
                    permission_policy: frozen.policy(),
                    command_isolation: CommandIsolation::HostUnrestricted,
                    command_hooks: Arc::new(CommandHooks::default()),
                    budget_policy: frozen.budget(),
                    system_instructions: frozen.instructions().into(),
                    pre_context_instructions: None,
                    limits: frozen.limits(),
                    native_automation: Some(frozen.clone()),
                    sessions: store.clone(),
                    checkpoints,
                },
                None,
            )
            .unwrap();
            match worker.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                ResumeUpdate::Finished(Err(error)) => {
                    assert!(error.contains(&scope_error), "{error}")
                }
                update => panic!("worker published effects instead of rejecting scope: {update:?}"),
            }
            assert_eq!(store.load(&session.id).unwrap().unwrap(), before);
            assert_eq!(fs::read(&metadata_path).unwrap(), metadata_before);
            assert!(store.try_acquire_native_lease(&session.id).is_ok());
            assert_no_transport(&listener);
        }
        assert_eq!(
            fs::read(foreign.join("report.txt")).unwrap(),
            b"FOREIGN_PROJECT_MUST_NOT_BE_READ"
        );
        assert_eq!(
            fs::read(moved.join("report.txt")).unwrap(),
            b"ORIGINAL_PROJECT_REPORT"
        );
    }
}
