//! Model availability and explicit connection navigation through the real host.

use super::*;
use openide_ui::{DrawCommand, HitKind, Point, Size};

#[cfg(target_os = "linux")]
#[test]
#[ignore = "diagnostic startup resource probe"]
fn repeated_temporary_hosts_resource_probe() {
    fn counts() -> (usize, usize) {
        (
            std::fs::read_dir("/proc/self/fd").unwrap().count(),
            std::fs::read_dir("/proc/self/task").unwrap().count(),
        )
    }
    let baseline = counts();
    eprintln!("resource baseline {baseline:?}");
    for index in 0..64 {
        let root = tempfile::tempdir().unwrap();
        let app = AgentsWindow::with_data_root(root.path().join("data"));
        assert!(app.store.is_some());
        assert!(app.catalog_response.is_none());
        assert!(!app.view.catalog_refreshing);
        drop(app);
        if index % 8 == 7 {
            eprintln!("resource after {} hosts {:?}", index + 1, counts());
        }
    }
    let mut descriptors = BTreeMap::new();
    for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let rendered = target.to_string_lossy();
            let category = if rendered.starts_with("socket:") {
                "socket".to_owned()
            } else if rendered.starts_with("pipe:") {
                "pipe".to_owned()
            } else {
                target.to_string_lossy().into_owned()
            };
            *descriptors.entry(category).or_insert(0) += 1;
        }
    }
    eprintln!("resource descriptors {descriptors:?}");
    std::thread::sleep(Duration::from_secs(2));
    let after_grace = counts();
    eprintln!("resource after grace {after_grace:?}");
    assert!(
        after_grace.0 <= baseline.0 + 2,
        "file descriptors retained after host drop"
    );
    assert!(
        after_grace.1 <= baseline.1 + 2,
        "threads retained after host drop"
    );
}

#[test]
fn host_fixture_keeps_catalog_and_account_discovery_idle_until_explicit_request() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    assert!(app.catalog_response.is_none());
    assert!(app.model_discovery_response.is_none());
    assert!(app.codex_model_response.is_none());
    assert!(!app.view.catalog_refreshing);
    assert_eq!(app.view.github_account, GithubAccountStatus::Checking);
    let worker = app.github_account_worker.as_ref().unwrap();
    assert!(worker.poll_latest().is_none());
    // Polling an idle fixture immediately is also local. It must not create
    // an implicit catalog job or consume a real saved GitHub account.
    assert!(!app.poll_catalog());
    assert!(app.catalog_response.is_none());
    assert!(app
        .github_account_worker
        .as_ref()
        .unwrap()
        .poll_latest()
        .is_none());
}

#[test]
fn renewed_chatgpt_connection_restarts_catalog_and_discovery_without_waiting_for_ttl() {
    for catalog_in_flight in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let previous_catalog = app.view.catalog.clone();
        let previous_settings = app.native_settings();
        if catalog_in_flight {
            // This startup job may have read an expired credential. Its result
            // must not prevent a new request using the renewed connection.
            app.begin_catalog_refresh();
        }
        let (stale_sender, stale_receiver) = mpsc::channel();
        app.model_discovery_response = Some(stale_receiver);
        app.model_discovery_pending = 1;
        app.catalog_last_refresh = Instant::now() - Duration::from_secs(100);
        // An absent test connection makes this worker complete locally, with
        // the same Refreshed event as a successful OAuth renewal.
        app.provider_refresh_worker = Some(crate::provider_connections::ConnectionWorker::refresh(
            app.data_root.clone(),
        ));
        let context = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.provider_refresh_worker.is_some() {
            assert!(Instant::now() < deadline, "local refresh did not finish");
            app.poll_provider_connections(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        // Stop the new metadata job before waiting for any public catalog.
        let catalog_job = app.catalog_response.take();
        drop(catalog_job);
        assert!(app.view.catalog_refreshing);
        assert!(app.catalog_last_refresh.elapsed() < Duration::from_secs(5));
        assert!(stale_sender
            .send(("codex".into(), Err("expired startup token".into())))
            .is_err());
        assert!(app.model_discovery_response.is_none());
        assert_eq!(app.model_discovery_pending, 0);
        assert_eq!(app.view.catalog, previous_catalog);
        assert_eq!(app.native_settings(), previous_settings);
        assert!(app.active_run.is_none());
    }
}

#[test]
fn replacing_or_disconnecting_a_key_rejects_late_account_catalog_and_discovery() {
    for disconnect in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        openide_connections::save(&app.data_root, &openide_connections::Credential {
            version: 1,
            provider_id: "zai".into(),
            access_token: "synthetic-account-a-key".into(),
            service: "api".into(),
            ..Default::default()
        }).unwrap();
        // A private model represents account-bound metadata. No actual
        // provider request is needed to reproduce the receiver ownership race.
        let mut old_catalog = model_catalog::reviewed_seed_snapshot();
        let provider = old_catalog.providers.get_mut("zai").unwrap();
        let mut private_model = provider.models["glm-5.3"].clone();
        private_model.id = "private-account-a".into();
        private_model.name = "Private account A".into();
        provider.models.insert(private_model.id.clone(), private_model);
        let old_listing = native_provider::parse_connected_models_for_adapter(
            &native_provider::adapter("zai").unwrap(),
            br#"{"data":[{"id":"private-account-a"}]}"#,
        ).unwrap();
        app.view.catalog = Some(Arc::new(old_catalog.clone()));
        app.model_discoveries = vec![old_listing.clone()];
        let (catalog_sender, catalog_receiver) = mpsc::channel();
        let (old_job, cancelled) = model_catalog::CatalogRefreshJob::from_test_receiver(catalog_receiver);
        app.catalog_response = Some(old_job);
        let (discovery_sender, discovery_receiver) = mpsc::channel();
        app.model_discovery_response = Some(discovery_receiver);
        app.model_discovery_pending = 1;
        app.view.composer_provider_status.insert("zai".into(), ProviderModelStatus::Checking);
        let (release, gate) = mpsc::channel();
        let late_worker = std::thread::spawn(move || {
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            let discovery_rejected = discovery_sender.send(("zai".into(), Ok(old_listing))).is_err();
            let catalog_rejected = catalog_sender.send(Ok(old_catalog)).is_err();
            (discovery_rejected, catalog_rejected)
        });

        app.provider_connection_id = Some("zai".into());
        app.provider_connection_worker = Some(if disconnect {
            crate::provider_connections::ConnectionWorker::disconnect(app.data_root.clone(), "zai".into())
        } else {
            // Z.ai's local save exercises the real Connected event without
            // an authenticated network operation or a synthetic success flag.
            crate::provider_connections::ConnectionWorker::api_key(
                app.data_root.clone(), "zai".into(), "synthetic-account-b-key".into(), "api".into(),
            )
        });
        let context = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.provider_connection_worker.is_some() {
            assert!(Instant::now() < deadline, "local credential transition did not finish");
            app.poll_provider_connections(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert!(app.catalog_response.is_some(), "refresh uses the new credential owner");
        assert!(app.catalog_last_refresh.elapsed() < Duration::from_secs(5));
        // Stop the fresh public catalog refresh just as the existing renewal
        // test does. The stale jobs are independent and still gated here.
        drop(app.catalog_response.take());
        release.send(()).unwrap();
        assert_eq!(late_worker.join().unwrap(), (true, true));
        app.poll_model_discovery();
        app.poll_catalog();
        let catalog = app.view.catalog.as_ref().unwrap();
        assert!(catalog.model("zai", "private-account-a").is_none());
        assert!(catalog.model("zai", "glm-5.3").is_some());
        assert_eq!(catalog.provider_status["zai"].verified_at_ms, 0);
        assert!(!app.model_discoveries.iter().any(|listing| listing.provider_id == "zai"));
        assert!(!app.view.composer_provider_status.contains_key("zai"));
        assert_eq!(app.view.provider_connections["zai"].configured, !disconnect);
        assert!(!app.view.composer_routes.iter().any(|route| route.model_id == "private-account-a"));
        if !disconnect {
            assert!(app.view.composer_routes.iter().any(|route| {
                route.provider_id == "zai" && route.model_id == "glm-5.3"
                    && route.provider_listed.is_none() && route.selectable()
            }));
        }
        assert!(app.active_run.is_none());
    }
}

fn composer_frame(app: &mut AgentsWindow, width: f32) -> openide_ui::Frame {
    let props = crate::composer_ui::ComposerProps::from_state(
        &app.view,
        width,
        false,
        None,
        "Continuar",
        None,
    );
    app.tree
        .set_root(crate::composer_ui::build_composer(&props));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(width, 620.0))
}

fn export_visual(frame: &openide_ui::Frame, width: f32, stage: &str) {
    let Some(directory) = std::env::var_os("OPENIDE_MODEL_VISUAL_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("model-{stage}-{width:.0}.svg"));
    crate::settings_ui::visual_qa::write_frame_svg(frame, &path, width, 620.0).unwrap();
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let svg = std::fs::read_to_string(&path).unwrap();
    let tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 620).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
}

fn click_frame_action(app: &mut AgentsWindow, frame: &openide_ui::Frame, id: &str) {
    let region = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap();
    let point = Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
    let ctx = egui::Context::default();
    for action in actions {
        app.apply_action(action, &ctx);
    }
}

#[test]
fn empty_picker_connect_click_opens_provider_connection_without_changing_choice_or_admitting_turn()
{
    for width in [360.0, 900.0] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let mut app = AgentsWindow::with_data_root(data.clone());
        app.view.workspace = root.path().display().to_string();
        app.view.selected_backend = "native".into();
        app.view.model = "gpt-4.1".into();
        app.view.composer_model = "gpt-4.1".into();
        app.view.composer_model_options = vec![("gpt-4.1".into(), "gpt-4.1".into())];
        app.view.composer_routes.clear();
        app.view.prompt = "Conservar este borrador".into();
        let before_native = std::fs::read(data.join("native-provider.json")).ok();
        let before_preferences = std::fs::read(data.join("composer-preferences.json")).ok();
        let before_sessions = app.store.as_ref().unwrap().list().unwrap().len();
        let ctx = egui::Context::default();
        let closed = composer_frame(&mut app, width);
        export_visual(&closed, width, "empty-chip");
        assert!(!closed.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("gpt-4.1")
        )));
        assert!(!closed
            .regions
            .iter()
            .any(|region| region.id == "composer_connect_provider"));
        assert!(!app.tree.focus("run_toggle"));
        click_frame_action(&mut app, &closed, "composer_model_toggle");
        assert!(app.view.composer_model_menu_open);
        assert!(!app.view.composer_model_list_open);
        let first_view = composer_frame(&mut app, width);
        export_visual(&first_view, width, "empty-first-view");
        assert!(first_view.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "No hay modelos disponibles"
        )));
        assert!(!first_view
            .regions
            .iter()
            .any(|region| region.id == "composer_connect_provider"));
        click_frame_action(&mut app, &first_view, "composer_model_list_toggle");
        assert!(app.view.composer_model_list_open);
        let catalog = composer_frame(&mut app, width);
        export_visual(&catalog, width, "empty-catalog");
        assert!(catalog.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "No hay modelos disponibles"
        )));
        click_frame_action(&mut app, &catalog, "composer_connect_provider");
        assert!(app.view.show_settings);
        assert_eq!(app.view.settings_page, SettingsPage::Providers);
        assert_eq!(
            app.view.catalog_settings_tab,
            CatalogSettingsTab::Connection
        );
        assert!(!app.view.composer_model_menu_open);
        assert!(!app.view.composer_model_list_open);
        assert!(!app.view.composer_advanced_open);
        assert!(!app.view.composer_effort_menu_open);
        assert!(app.view.composer_model_query.is_empty());
        assert_eq!(app.view.prompt, "Conservar este borrador");
        assert_eq!(app.view.model, "gpt-4.1");
        assert_eq!(app.view.composer_model, "gpt-4.1");
        assert_eq!(
            std::fs::read(data.join("native-provider.json")).ok(),
            before_native
        );
        assert_eq!(
            std::fs::read(data.join("composer-preferences.json")).ok(),
            before_preferences
        );
        assert_eq!(
            app.store.as_ref().unwrap().list().unwrap().len(),
            before_sessions
        );
        assert!(app.active_run.is_none());
        assert!(app.active_resume.is_none());
        assert!(app.model_discovery_response.is_none());
        assert!(app.view.selected_session.is_none());
        // A stale action from a historic external session cannot open the
        // native connection workflow or reassign its model.
        app.view.show_settings = false;
        app.view.selected_backend = "codex".into();
        app.apply_action(
            UiAction::Clicked {
                id: "composer_connect_provider".into(),
            },
            &ctx,
        );
        assert!(!app.view.show_settings);
        assert_eq!(app.view.selected_backend, "codex");
        assert_eq!(app.view.model, "gpt-4.1");
    }
}

#[test]
fn hidden_registered_connection_preserves_engine_route_without_exposing_a_fallback_model() {
    for discovered in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let settings = NativeProviderSettings {
            provider_id: "custom".into(),
            model_id: "local/model:exact".into(),
            custom_endpoint: "http://127.0.0.1:1234/v1".into(),
        };
        settings.save(&data).unwrap();
        let mut app = AgentsWindow::with_data_root(data.clone());
        if discovered {
            app.model_discoveries = vec![native_provider::ProviderModelDiscovery {
                provider_id: "custom".into(),
                models: BTreeMap::from([(
                    settings.model_id.clone(),
                    native_provider::DiscoveredModel {
                        listing_index: None,
                        limits_declared: false,
                        efforts_declared: false,
                        capabilities_declared: false,
                        max_input_tokens: None,
                        declared_capabilities: Default::default(),
                        id: settings.model_id.clone(),
                        name: settings.model_id.clone(),
                        provider_reports_tools: true,
                        tool_call_declared: Some(true),
                        text_input_output: true,
                        supported_efforts: Vec::new(),
                        context_window_tokens: None,
                        max_output_tokens: None,
                    },
                )]),
            }];
            app.refresh_composer_routes();
        }
        let engine_routes = composer_selection::model_route_options_with_discovery_at(
            app.view.catalog.as_deref(),
            &app.native_settings(),
            &app.model_discoveries,
            &data,
        )
        .unwrap();
        let exact = engine_routes
            .iter()
            .find(|route| {
                route.backend == "native"
                    && route.provider_id == "custom"
                    && route.model_id == settings.model_id
            })
            .unwrap();
        assert!(exact.selectable());
        assert!(matches!(
            exact.credential,
            composer_selection::CredentialAvailability::NoKeyRequired
                | composer_selection::CredentialAvailability::EnvPresentUnverified
        ));
        for width in [360.0, 900.0] {
            let frame = composer_frame(&mut app, width);
            if !discovered {
                export_visual(&frame, width, "local-chip");
            }
            assert_eq!(app.view.composer_model, settings.model_id);
            assert_eq!(app.native_settings(), settings);
            assert!(!app
                .view
                .composer_routes
                .iter()
                .any(|route| route.provider_id == "custom"));
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text.contains(&settings.model_id)
            )));
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Sin modelos" || text == "No hay modelos disponibles"
            )));
            assert!(!app.tree.focus("run_toggle"));
            click_frame_action(&mut app, &frame, "composer_model_toggle");
            let list = composer_frame(&mut app, width);
            assert!(list.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "No hay modelos disponibles"
            )));
            assert!(!list
                .regions
                .iter()
                .any(|region| region.id
                    == format!("composer_route:native:custom:{}", settings.model_id)));
            assert!(list
                .regions
                .iter()
                .any(|region| region.id == "composer_model_list_toggle"));
            assert!(!list
                .regions
                .iter()
                .any(|region| region.id == "composer_connect_provider"));
            click_frame_action(&mut app, &list, "composer_model_list_toggle");
            let catalog = composer_frame(&mut app, width);
            assert!(!catalog.regions.iter().any(|region| {
                region.id == format!("composer_route:native:custom:{}", settings.model_id)
            }));
            assert!(!catalog.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text.contains(&settings.model_id)
            )));
            click_frame_action(&mut app, &catalog, "composer_connect_provider");
            assert_eq!(app.view.settings_page, SettingsPage::Providers);
            assert_eq!(
                app.view.catalog_settings_tab,
                CatalogSettingsTab::Connection
            );
            app.view.show_settings = false;
        }
        assert_eq!(NativeProviderSettings::load(&data), settings);
        assert!(app.active_run.is_none());
        assert!(app.model_discovery_response.is_none());
        drop(app);
        let mut reopened = AgentsWindow::with_data_root(data);
        let frame = composer_frame(&mut reopened, 900.0);
        assert_eq!(reopened.native_settings(), settings);
        assert_eq!(reopened.view.composer_model, settings.model_id);
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Sin modelos" || text == "No hay modelos disponibles"
        )));
        assert!(!reopened.tree.focus("run_toggle"));
        assert!(reopened.active_run.is_none());
    }
}
