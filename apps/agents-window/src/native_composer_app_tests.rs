use super::*;

fn click(id: &str) -> UiAction {
    UiAction::Clicked { id: id.into() }
}

#[test]
fn running_composer_changes_are_replaceable_and_do_not_resolve_live_permissions() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
    let next = crate::composer_permissions_ui::settings_for_preset(
        app.agent_tool_settings,
        crate::composer_permissions_ui::ComposerPermissionPreset::Ask,
    );
    assert!(app.commit_agent_tool_settings(next, "fixture"));
    let owner = app
        .store
        .as_ref()
        .unwrap()
        .create(root.path(), "native")
        .unwrap()
        .id;
    app.view.workspace = root.path().display().to_string();
    app.view.selected_session = Some(owner.clone());
    app.view.running = true;
    app.view.pending_permissions.push(PermissionView {
        request_id: "live-write-permission".into(),
        tool_name: "write_file".into(),
        arguments: "{}".into(),
    });
    let baseline = app.agent_tool_settings;
    let saved = std::fs::read(app.data_root.join("agent-tool-settings.json")).unwrap();
    let ctx = egui::Context::default();
    app.apply_action(click("composer:permissions"), &ctx);
    assert!(app.view.composer_permissions_open);
    app.apply_action(click("composer_permissions:full_access"), &ctx);
    assert!(
        app.view.composer_pending.is_none(),
        "full access is not consented by opening its dialog"
    );
    let token = app
        .view
        .composer_full_access_confirm
        .as_ref()
        .unwrap()
        .token;
    app.apply_action(
        click(&format!("composer_full_access_confirm:{token}")),
        &ctx,
    );
    assert!(app
        .view
        .composer_pending
        .as_ref()
        .unwrap()
        .permissions
        .unwrap()
        .full_access());
    assert_eq!(app.agent_tool_settings, baseline);
    assert_eq!(
        app.view.pending_permissions[0].request_id,
        "live-write-permission"
    );
    assert_eq!(app.view.pending_permissions.len(), 1);
    assert_eq!(
        std::fs::read(app.data_root.join("agent-tool-settings.json")).unwrap(),
        saved
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

    app.apply_action(click("composer_permissions:read_only"), &ctx);
    let pending = app.view.composer_pending.as_ref().unwrap();
    assert!(!pending.permissions.unwrap().full_access());
    assert!(!pending.permissions.unwrap().permissions().write_file);
    let undo = format!("composer_pending_undo:{}", pending.token);
    app.apply_action(click(&undo), &ctx);
    assert!(app.view.composer_pending.is_none());
    assert_eq!(app.agent_tool_settings, baseline);
    assert!(app.view.running);
    assert_eq!(app.view.pending_permissions.len(), 1);
}

#[test]
fn queued_model_and_permissions_apply_after_success_failure_or_cancel_and_worker_release() {
    for terminal in [
        AgentEventKind::TurnCompleted {
            response: "Listo".into(),
            steps: 1,
        },
        AgentEventKind::TurnFailed {
            error: "fixture failure".into(),
        },
        AgentEventKind::TurnCancelled,
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
        let next = crate::composer_permissions_ui::settings_for_preset(
            app.agent_tool_settings,
            crate::composer_permissions_ui::ComposerPermissionPreset::Ask,
        );
        assert!(app.commit_agent_tool_settings(next, "fixture"));
        let owner = app
            .store
            .as_ref()
            .unwrap()
            .create(root.path(), "native")
            .unwrap()
            .id;
        app.view.workspace = root.path().display().to_string();
        app.view.selected_session = Some(owner.clone());
        app.view.running = true;
        let current_model = app.view.model.clone();
        let current_permissions = app.agent_tool_settings;
        let ctx = egui::Context::default();
        app.apply_action(click("composer_model_toggle"), &ctx);
        assert!(app.view.composer_model_menu_open);
        let route = app
            .view
            .composer_routes
            .iter()
            .find(|route| route.model_id == "gpt-5.6-luna")
            .unwrap()
            .route_id
            .clone();
        app.apply_action(click(&format!("composer_route:{route}")), &ctx);
        app.apply_action(click("composer_effort:low"), &ctx);
        app.apply_action(click("composer_permissions:read_only"), &ctx);
        assert_eq!(app.view.model, current_model);
        assert_eq!(app.agent_tool_settings, current_permissions);
        assert_eq!(
            NativeProviderSettings::load(root.path()).model_id,
            current_model
        );
        let projected = crate::composer_ui::ComposerProps::from_state(
            &app.view, 480.0, false, None, "Mensaje", None,
        );
        assert_eq!(projected.selected_model, "gpt-5.6-luna");
        assert_eq!(projected.selected_effort, "low");
        assert!(!projected.permission_controls.unwrap().0.write_file);
        assert!(app.view.composer_pending.is_some());

        // A terminal journal event can precede worker cleanup. Keep a real
        // worker channel alive to verify the common admission gate stays shut.
        let (_sender, receiver) = mpsc::channel();
        app.active_compact = Some(ActiveCompact {
            session_id: owner.clone(),
            receiver,
            cancel: CancellationToken::new(),
        });
        app.process_event(AgentEvent::new(&owner, 1, terminal));
        assert!(!app.view.running);
        app.poll_pending_composer_selection();
        assert_eq!(app.view.model, current_model);
        assert_eq!(app.agent_tool_settings, current_permissions);
        app.active_compact = None;
        assert!(app.poll_pending_composer_selection());
        assert!(app.view.composer_pending.is_none());
        assert_eq!(app.view.model, "gpt-5.6-luna");
        assert_eq!(app.view.composer_effort, "low");
        assert!(!app.agent_tool_settings.permissions().write_file);
        assert_eq!(
            NativeProviderSettings::load(root.path()).model_id,
            "gpt-5.6-luna"
        );
        assert_eq!(
            AgentToolSettings::load(root.path()).unwrap(),
            app.agent_tool_settings
        );
    }
}

#[test]
fn queued_selection_cannot_apply_or_be_cancelled_in_another_workspace_or_conversation() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
    let next = crate::composer_permissions_ui::settings_for_preset(
        app.agent_tool_settings,
        crate::composer_permissions_ui::ComposerPermissionPreset::Ask,
    );
    assert!(app.commit_agent_tool_settings(next, "fixture"));
    app.view.workspace = root.path().display().to_string();
    app.view.selected_session = Some("owner-a".into());
    app.view.background_run_selected = true;
    let baseline = app.agent_tool_settings;
    app.apply_action(
        click("composer_permissions:read_only"),
        &egui::Context::default(),
    );
    let pending = app.view.composer_pending.clone().unwrap();
    app.view.background_run_selected = false;
    for (workspace, owner) in [
        (app.view.workspace.clone(), "owner-b"),
        ("/different/project".into(), "owner-a"),
    ] {
        app.view.workspace = workspace;
        app.view.selected_session = Some(owner.into());
        app.poll_pending_composer_selection();
        app.undo_pending_composer_selection(pending.token);
        assert!(app.view.composer_pending.is_none());
        assert_eq!(app.agent_tool_settings, baseline);
        assert_eq!(app.pending_composer_selections.len(), 1);
    }
    app.view.workspace = pending.scope.workspace;
    app.view.selected_session = pending.scope.session;
    app.poll_pending_composer_selection();
    assert!(app.pending_composer_selections.is_empty());
    assert!(!app.agent_tool_settings.permissions().write_file);
}

#[test]
fn queued_full_access_requires_unchanged_consent_and_saved_settings_at_application() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
    app.agent_tool_settings.save(&app.data_root).unwrap();
    app.view.selected_session = Some("owner".into());
    app.view.running = true;
    let ctx = egui::Context::default();
    app.apply_action(click("composer_permissions:full_access"), &ctx);
    let token = app
        .view
        .composer_full_access_confirm
        .as_ref()
        .unwrap()
        .token;
    app.apply_action(
        click(&format!("composer_full_access_confirm:{token}")),
        &ctx,
    );
    let mut external_settings = app.agent_tool_settings;
    external_settings.set(openide_harness::LocalTool::ReadFile, false);
    external_settings.save(&app.data_root).unwrap();
    app.view.running = false;
    app.poll_pending_composer_selection();
    assert!(!app.agent_tool_settings.full_access());
    assert!(!AgentToolSettings::load(&app.data_root)
        .unwrap()
        .full_access());
    assert!(app
        .view
        .composer_pending
        .as_ref()
        .unwrap()
        .error
        .as_ref()
        .unwrap()
        .contains("permisos cambiaron"));
    assert!(
        !app.poll_pending_composer_selection(),
        "invalid consent is not retried every frame"
    );
}

#[test]
fn queued_model_is_revalidated_and_never_silently_replaced_with_a_fallback() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
    app.view.running = true;
    let baseline = app.view.model.clone();
    let route = app
        .view
        .composer_routes
        .iter()
        .find(|route| route.model_id == "gpt-5.6-luna")
        .unwrap()
        .route_id
        .clone();
    app.select_composer_route(&route);
    app.model_discoveries[0].models.remove("gpt-5.6-luna");
    app.view.running = false;
    app.poll_pending_composer_selection();
    assert_eq!(app.view.model, baseline);
    assert!(app.view.composer_pending.as_ref().unwrap().error.is_some());
    assert_eq!(
        NativeProviderSettings::load(&app.data_root).model_id,
        baseline
    );
}

#[test]
fn queued_effort_persistence_failure_keeps_pending_until_a_successful_retry() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
    let original_effort = app.view.composer_effort.clone();
    app.view.running = true;
    assert!(app.select_composer_effort("high"));
    let preferences_path = app.data_root.join("composer-preferences.json");
    if preferences_path.exists() {
        std::fs::remove_file(&preferences_path).unwrap();
    }
    std::fs::create_dir(&preferences_path).unwrap();
    app.view.running = false;
    app.poll_pending_composer_selection();
    assert_eq!(app.view.composer_effort, original_effort);
    assert!(app
        .view
        .composer_pending
        .as_ref()
        .unwrap()
        .error
        .as_ref()
        .unwrap()
        .contains("esfuerzo"));
    assert_ne!(
        app.view.status,
        "Selección aplicada para el próximo mensaje"
    );
    std::fs::remove_dir(&preferences_path).unwrap();
    assert!(app.select_composer_effort("high"));
    app.poll_pending_composer_selection();
    assert!(app.view.composer_pending.is_none());
    assert_eq!(app.view.composer_effort, "high");
    assert_eq!(
        ComposerPreferences::load(&app.data_root)
            .choice("native", "")
            .effort,
        "high"
    );
}

#[test]
fn default_effort_remains_valid_for_a_model_without_reasoning_controls() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _credential) = super::tests::native_composer_test_fixture(root.path());
    app.view.composer_effort_options.clear();
    assert!(app.select_composer_effort("default"));
    assert_eq!(app.view.composer_effort, "default");
    assert!(!app.select_composer_effort("invented-effort"));
}

#[test]
fn a_dictated_phrase_is_written_at_the_caret_and_replaced_by_its_next_version() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.tree.set_input_value("prompt", "antes después");
    app.tree.set_input_cursor("prompt", 6);
    app.write_dictation_interim("hola ".into());
    assert_eq!(app.view.prompt, "antes hola después");
    assert_eq!(app.view.composer_transcript_at, 6);
    app.write_dictation_interim("hola mundo ".into());
    assert_eq!(app.tree.input_value("prompt"), Some("antes hola mundo después"));
    assert_eq!(app.tree.input_cursor("prompt"), Some(17));
    app.write_dictation_interim(String::new());
    assert_eq!(app.view.prompt, "antes después");
    assert_eq!(app.tree.input_cursor("prompt"), Some(6));
    // Typing over the phrase makes it the user's: it is no longer taken back.
    app.write_dictation_interim("uno".into());
    app.tree.set_input_value("prompt", "antes unX después");
    app.write_dictation_interim(String::new());
    assert_eq!(app.tree.input_value("prompt"), Some("antes unX después"));
}

#[test]
fn codex_usage_failures_have_distinct_notices_live_and_after_reopen() {
    for (code, expected_notice) in [
        (
            "subscription_sharing_usage_limit_exceeded",
            "Se alcanzó un límite de uso de ChatGPT en OpenIDE. Revisa los límites de esta app en ChatGPT → Configuración → Uso; abre esa página desde Configuración de proveedores → Ver uso.",
        ),
        (
            "subscription_sharing_usage_unavailable",
            "ChatGPT no pudo comprobar la disponibilidad de uso para este turno. Inténtalo de nuevo más tarde.",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let mut app = AgentsWindow::with_data_root(data.clone());
        app.view.workspace = root.path().display().to_string();
        let session = app
            .store
            .as_ref()
            .unwrap()
            .create(root.path(), "native")
            .unwrap();
        app.open_session(&session.id);
        app.view.native_provider_id = "codex".into();
        app.view.running = true;
        app.pending_prompt = Some("Revisar el proyecto".into());
        let raw = format!("model request failed: Responses stream failed: {code}: provider detail");
        for (index, kind) in [
            AgentEventKind::TurnAdmitted {
                turn_id: "quota-turn".into(),
                prompt: "Revisar el proyecto".into(),
                model: "gpt-5.6-luna".into(),
                mode: TurnMode::Build,
            },
            AgentEventKind::TurnStarted {
                prompt: "Revisar el proyecto".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-5.6-luna".into(),
            },
            AgentEventKind::TurnFailed { error: raw.clone() },
        ]
        .into_iter()
        .enumerate()
        {
            app.process_event(AgentEvent::new(&session.id, index as u64 + 1, kind));
        }
        app.view.transcript = app.projection.items().to_vec();
        assert!(!app.view.running);
        assert_eq!(app.view.prompt, "Revisar el proyecto");
        assert!(app.view.composer_error.is_none());
        assert_eq!(
            crate::session_surface::conversation_notice_message(&raw, true),
            expected_notice
        );
        assert_eq!(
            crate::session_surface::conversation_notice_message(&raw, false),
            raw
        );
        let assert_one_notice = |app: &AgentsWindow| {
            let frame = UiTree::new(crate::session_surface::build_session_surface(
                &app.view, 960.0, false, false,
            ))
            .layout(Size::new(960.0, 800.0));
            assert_eq!(
                frame
                    .text_layouts()
                    .iter()
                    .filter(|block| block.source == expected_notice)
                    .count(),
                1
            );
            assert_eq!(
                frame
                    .text_layouts()
                    .iter()
                    .filter(|block| block.source == "Error en la conversación")
                    .count(),
                1
            );
        };
        assert_one_notice(&app);
        let before = app
            .store
            .as_ref()
            .unwrap()
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events;
        assert!(matches!(&before.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error == &raw));
        drop(app);
        let mut reopened = AgentsWindow::with_data_root(data);
        reopened.open_session(&session.id);
        assert!(reopened.view.composer_error.is_none());
        assert_one_notice(&reopened);
        assert_eq!(
            reopened
                .store
                .as_ref()
                .unwrap()
                .load(&session.id)
                .unwrap()
                .unwrap()
                .events,
            before
        );
    }
}

#[test]
fn local_composer_error_stays_visible_beside_a_historical_failure_without_new_event() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view
        .transcript
        .push(crate::transcript::TranscriptItem::Notice {
            message: "Error de un turno anterior".into(),
            is_error: true,
        });
    let before = app.store.as_ref().unwrap().list().unwrap();
    app.show_composer_error("El directorio de trabajo no existe");
    assert_eq!(
        app.view.composer_error.as_deref(),
        Some("El directorio de trabajo no existe")
    );
    let frame = UiTree::new(crate::session_surface::build_session_surface(
        &app.view, 960.0, false, false,
    ))
    .layout(Size::new(960.0, 800.0));
    for message in [
        "Error de un turno anterior",
        "El directorio de trabajo no existe",
    ] {
        assert_eq!(
            frame
                .text_layouts()
                .iter()
                .filter(|block| block.source == message)
                .count(),
            1
        );
    }
    assert_eq!(app.store.as_ref().unwrap().list().unwrap(), before);
}

#[test]
fn native_composer_without_explicit_model_rejects_before_preparing_or_admitting() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::native_composer_app_tests::native_composer_unconfigured_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("OPENIDE_UNCONFIGURED_COMPOSER_FIXTURE", "1")
        .env_remove("OPENIDE_MODEL")
        .env_remove("OPENIDE_API_BASE")
        .env_remove("OPENIDE_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "executed in a clean environment by the parent regression"]
fn native_composer_unconfigured_fixture() {
    if std::env::var_os("OPENIDE_UNCONFIGURED_COMPOSER_FIXTURE").is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut app = AgentsWindow::with_data_root(data.clone());
    // An invalid workspace prevents a network request even if this guard
    // regresses. The intended error must win before workspace/attachment
    // preparation, session creation or durable turn admission.
    app.view.workspace = root.path().join("missing-project").display().to_string();
    app.view.prompt = "Conservar el borrador sin elegir un modelo".into();
    let before = app.store.as_ref().unwrap().list().unwrap();
    assert!(!app.native_settings().has_explicit_configuration(&data));
    app.start_run();
    assert_eq!(
        app.view.composer_error.as_deref(),
        Some("No hay modelos disponibles. Conecta un proveedor en Configuración.")
    );
    assert_eq!(
        app.view.prompt,
        "Conservar el borrador sin elegir un modelo"
    );
    assert!(app.active_run.is_none());
    assert!(!app.view.running);
    assert!(app.view.selected_session.is_none());
    assert_eq!(app.store.as_ref().unwrap().list().unwrap(), before);
    assert!(!data.join("native-provider.json").exists());
    assert!(!root.path().join("missing-project").exists());
}

#[test]
fn native_composer_retired_engine_actions_and_legacy_history_cannot_start_external_work() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut preferences = ComposerPreferences::default();
    preferences.set_selected_backend("codex");
    preferences.save(&data).unwrap();
    let mut app = AgentsWindow::with_data_root(data.clone());
    let ctx = egui::Context::default();
    assert_eq!(app.view.selected_backend, "native");
    assert_eq!(
        app.view.backend_options,
        vec![("native".into(), "Nativo".into())]
    );
    assert!(app
        .view
        .composer_routes
        .iter()
        .all(|route| route.backend == "native"));
    let original_preferences = std::fs::read(data.join("composer-preferences.json")).unwrap();
    for id in [
        "backend:codex",
        "backend:claude",
        "backend_next",
        "backend_menu_toggle",
    ] {
        app.apply_action(click(id), &ctx);
        assert_eq!(app.view.selected_backend, "native");
        assert!(!app.view.backend_menu_open);
    }
    assert_eq!(
        std::fs::read(data.join("composer-preferences.json")).unwrap(),
        original_preferences
    );

    // Preserve actual historic records; opening them never reassigns their
    // engine, starts a CLI process, or appends a native turn to their journal.
    let store = app.store.as_ref().unwrap().clone();
    let legacy = store.create(root.path(), "codex").unwrap();
    store
        .append_event(
            &legacy.id,
            &AgentEvent::new(
                &legacy.id,
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Historic CLI prompt".into(),
                },
            ),
        )
        .unwrap();
    store
        .append_event(
            &legacy.id,
            &AgentEvent::new(
                &legacy.id,
                2,
                AgentEventKind::TurnCompleted {
                    response: "Historic response".into(),
                    steps: 1,
                },
            ),
        )
        .unwrap();
    let before = store.load(&legacy.id).unwrap().unwrap();
    app.open_session(&legacy.id);
    assert_eq!(app.view.selected_backend, "codex");
    assert!(!app.view.transcript.is_empty());
    app.view.prompt = "Continue this task".into();
    app.start_run();
    assert!(app.active_run.is_none());
    assert!(!app.view.running);
    assert!(app
        .view
        .composer_error
        .as_deref()
        .unwrap()
        .contains("historial"));
    assert_eq!(
        store.load(&legacy.id).unwrap().unwrap().events,
        before.events
    );
    app.apply_action(click("composer_context_toggle"), &ctx);
    assert!(!app.view.composer_context_open);
    app.apply_action(click("new_session"), &ctx);
    assert_eq!(app.view.selected_backend, "native");
    assert!(app.view.selected_session.is_none());
    assert!(!app.view.composer_context_open);
    assert_eq!(
        store.load(&legacy.id).unwrap().unwrap().session.backend,
        "codex"
    );

    app.apply_action(click("composer_context_toggle"), &ctx);
    assert!(app.view.composer_context_open);
    assert!(app.active_run.is_none());
    assert_eq!(
        store.load(&legacy.id).unwrap().unwrap().events,
        before.events
    );
    app.apply_action(click("composer_context_dismiss"), &ctx);
    assert!(!app.view.composer_context_open);
}
