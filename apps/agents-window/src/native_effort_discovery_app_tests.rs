use super::*;
use crate::composer_selection::ComposerChoice;
use crate::native_provider::{DiscoveredModel, ProviderModelDiscovery};
use openide_ui::{DrawCommand, HitKind, Point, UiEvent};

const MODEL: &str = "gpt-5.6-luna";
const OTHER_MODEL: &str = "gpt-5.6-sol";

fn fixture(root: &Path) -> AgentsWindow {
    let data = root.join("private-data");
    let directory = data.join("provider-credentials");
    std::fs::create_dir_all(&directory).unwrap();
    let credential = directory.join("codex.json");
    std::fs::write(
        &credential,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "provider_id": "codex",
            "access_token": "synthetic-effort-fixture-not-a-real-key",
            "scopes": ["chatgpt.tokens.use.direct"],
            "oauth": true
        }))
        .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&data, &directory] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    NativeProviderSettings {
        provider_id: "codex".into(),
        model_id: MODEL.into(),
        ..NativeProviderSettings::default()
    }
    .save(&data)
    .unwrap();
    let mut preferences = ComposerPreferences::default();
    preferences.set_selected_backend("native");
    preferences.set(
        "native",
        ComposerChoice {
            model: MODEL.into(),
            effort: "max".into(),
        },
    );
    preferences.save(&data).unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(data);
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap()
        .id;
    app.view.workspace = workspace.display().to_string();
    app.view.selected_session = Some(session);
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.prompt = "Keep this draft while the provider catalog arrives".into();
    assert!(app.model_discovery_response.is_none());
    assert!(app.model_discoveries.is_empty());
    assert_eq!(app.view.selected_backend, "native");
    assert_eq!(app.view.native_provider_id, "codex");
    assert_eq!(app.view.model, MODEL);
    app
}

fn model(id: &str, levels: &[&str], effort_capability: Option<bool>) -> DiscoveredModel {
    DiscoveredModel {
        listing_index: None,
        id: id.into(),
        name: crate::composer_selection::display_model(id),
        provider_reports_tools: true,
        tool_call_declared: Some(true),
        text_input_output: true,
        supported_efforts: levels.iter().map(|level| (*level).into()).collect(),
        efforts_declared: true,
        context_window_tokens: Some(100_000),
        max_output_tokens: Some(8_000),
        max_input_tokens: None,
        limits_declared: true,
        declared_capabilities: effort_capability
            .map(|supported| [("effort".into(), supported)].into_iter().collect())
            .unwrap_or_default(),
        capabilities_declared: effort_capability.is_some(),
    }
}

fn discovery(models: Vec<DiscoveredModel>) -> ProviderModelDiscovery {
    ProviderModelDiscovery {
        provider_id: "codex".into(),
        models: models
            .into_iter()
            .map(|model| (model.id.clone(), model))
            .collect(),
    }
}

fn supported_discovery() -> ProviderModelDiscovery {
    discovery(vec![model(
        MODEL,
        &["low", "medium", "high", "max"],
        Some(true),
    )])
}

fn deliver(app: &mut AgentsWindow, listing: ProviderModelDiscovery) {
    let (sender, receiver) = mpsc::channel();
    sender.send(("codex".into(), Ok(listing))).unwrap();
    app.model_discovery_response = Some(receiver);
    app.model_discovery_pending = 1;
    app.view
        .composer_provider_status
        .insert("codex".into(), ProviderModelStatus::Checking);
    assert!(app.poll_model_discovery());
    assert!(app.model_discovery_response.is_none());
    assert_eq!(app.model_discovery_pending, 0);
    assert!(!app.view.composer_provider_status.contains_key("codex"));
}

fn ids(options: &[(String, String)]) -> Vec<&str> {
    options.iter().map(|(id, _)| id.as_str()).collect()
}

fn apply(app: &mut AgentsWindow, id: &str) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

fn frame(app: &mut AgentsWindow, width: f32) -> openide_ui::Frame {
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
    let Some(directory) = std::env::var_os("OPENIDE_EFFORT_VISUAL_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("native-effort-{stage}-{width:.0}.svg"));
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

fn click(app: &mut AgentsWindow, frame: &openide_ui::Frame, id: &str) {
    let target = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing action {id}"))
        .rect;
    let point = Point::new(
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    assert_eq!(frame.hit_test(point), Some(id));
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

#[test]
fn native_codex_async_discovery_restores_saved_max_and_renders_effort_and_advanced_controls() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let preference_bytes = std::fs::read(app.data_root.join("composer-preferences.json")).unwrap();
    assert!(app.view.composer_effort_options.is_empty());
    assert_eq!(app.view.composer_effort, "default");
    deliver(&mut app, supported_discovery());
    assert_eq!(
        ids(&app.view.composer_effort_options),
        ["default", "low", "medium", "high", "max"]
    );
    assert_eq!(app.view.composer_effort, "max");
    assert_eq!(
        std::fs::read(app.data_root.join("composer-preferences.json")).unwrap(),
        preference_bytes
    );
    assert!(app.active_run.is_none() && app.active_resume.is_none());
    for width in [360.0, 900.0] {
        let closed = frame(&mut app, width);
        export_visual(&closed, width, "closed");
        apply(&mut app, "composer_model_toggle");
        let menu = frame(&mut app, width);
        export_visual(&menu, width, "menu");
        apply(&mut app, "composer_model_dismiss");
    }
    for width in [900.0, 1_400.0] {
        let closed = frame(&mut app, width);
        assert!(
            closed.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Máximo"
            )),
            "selected effort is visible in the composer pill at {width}: {:?}",
            closed
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
        click(&mut app, &closed, "composer_model_toggle");
        let menu = frame(&mut app, width);
        assert!(menu.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Esfuerzo"
        )));
        assert!(menu.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Avanzado"
        )));
        assert!(menu
            .regions
            .iter()
            .any(|region| region.id == "composer_advanced_toggle"));
        assert!(menu
            .regions
            .iter()
            .any(|region| region.id == "composer_advanced_chip_toggle"));
        click(&mut app, &menu, "composer_advanced_toggle");
        let advanced = frame(&mut app, width);
        assert!(advanced
            .regions
            .iter()
            .any(|region| region.id == "composer_effort_slider"));
        apply(&mut app, "composer_model_dismiss");
    }
}

#[test]
fn native_codex_effort_action_persists_low_across_new_session_and_async_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    deliver(&mut app, supported_discovery());
    apply(&mut app, "composer_effort:low");
    assert_eq!(app.view.composer_effort, "low");
    let saved = ComposerPreferences::load(&app.data_root).choice("native", MODEL);
    assert_eq!(
        saved,
        ComposerChoice {
            model: MODEL.into(),
            effort: "low".into()
        }
    );
    apply(&mut app, "new_session");
    assert!(app.view.selected_session.is_none());
    assert_eq!(app.view.composer_effort, "low");
    assert_eq!(
        ids(&app.view.composer_effort_options),
        ["default", "low", "medium", "high", "max"]
    );
    let data = app.data_root.clone();
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(data);
    assert!(reopened.model_discovery_response.is_none());
    deliver(&mut reopened, supported_discovery());
    assert_eq!(reopened.view.composer_effort, "low");
    assert_eq!(reopened.view.model, MODEL);
    assert_eq!(reopened.view.native_provider_id, "codex");
    assert!(reopened.active_run.is_none() && reopened.active_resume.is_none());
}

#[test]
fn direct_route_refresh_updates_effort_metadata_without_reselecting_route_or_writing_user_state() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let preferences = std::fs::read(app.data_root.join("composer-preferences.json")).unwrap();
    let provider = std::fs::read(app.data_root.join("native-provider.json")).unwrap();
    let credential = std::fs::read(app.data_root.join("provider-credentials/codex.json")).unwrap();
    let draft = app.view.prompt.clone();
    let owner = app.view.selected_session.clone();
    let journal = app
        .store
        .as_ref()
        .unwrap()
        .load(owner.as_deref().unwrap())
        .unwrap()
        .unwrap();
    app.model_discoveries = vec![supported_discovery()];
    app.refresh_composer_routes();
    assert_eq!(
        ids(&app.view.composer_effort_options),
        ["default", "low", "medium", "high", "max"]
    );
    assert_eq!(app.view.composer_effort, "max");
    assert_eq!(app.view.composer_model, MODEL);
    assert_eq!(app.view.model, MODEL);
    assert_eq!(app.view.native_provider_id, "codex");
    assert_eq!(app.view.selected_backend, "native");
    assert_eq!(app.view.selected_session, owner);
    assert_eq!(app.view.prompt, draft);
    assert_eq!(
        std::fs::read(app.data_root.join("composer-preferences.json")).unwrap(),
        preferences
    );
    assert_eq!(
        std::fs::read(app.data_root.join("native-provider.json")).unwrap(),
        provider
    );
    assert_eq!(
        std::fs::read(app.data_root.join("provider-credentials/codex.json")).unwrap(),
        credential
    );
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load(owner.as_deref().unwrap())
            .unwrap()
            .unwrap(),
        journal
    );
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn native_codex_declared_empty_or_negative_effort_metadata_hides_controls_and_uses_default() {
    for discovered in [
        model(MODEL, &[], None),
        model(MODEL, &["low", "max"], Some(false)),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        deliver(&mut app, supported_discovery());
        assert_eq!(app.view.composer_effort, "max");
        let preferences = std::fs::read(app.data_root.join("composer-preferences.json")).unwrap();
        deliver(&mut app, discovery(vec![discovered]));
        assert!(app.view.composer_effort_options.is_empty());
        assert_eq!(app.view.composer_effort, "default");
        apply(&mut app, "composer_model_toggle");
        let menu = frame(&mut app, 900.0);
        for id in [
            "composer_advanced_toggle",
            "composer_advanced_chip_toggle",
            "composer_effort_slider",
        ] {
            assert!(!menu.regions.iter().any(|region| region.id == id), "{id}");
        }
        assert!(!menu.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Esfuerzo" || text == "Avanzado"
        )));
        apply(&mut app, "composer_effort:max");
        assert_eq!(app.view.composer_effort, "default");
        assert_eq!(
            std::fs::read(app.data_root.join("composer-preferences.json")).unwrap(),
            preferences
        );
        assert!(app.active_run.is_none() && app.active_resume.is_none());
    }
}

#[test]
fn switching_native_codex_models_uses_only_the_new_models_declared_levels() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    deliver(
        &mut app,
        discovery(vec![
            model(MODEL, &["low", "medium", "high", "max"], Some(true)),
            model(OTHER_MODEL, &["minimal", "high"], Some(true)),
        ]),
    );
    assert_eq!(app.view.composer_effort, "max");
    let route = app
        .view
        .composer_routes
        .iter()
        .find(|route| route.model_id == OTHER_MODEL && route.provider_id == "codex")
        .expect("second native Codex route is backed by this synthetic listing");
    assert!(route.selectable(), "{route:?}");
    let route_id = route.route_id.clone();
    apply(&mut app, &format!("composer_route:{route_id}"));
    assert_eq!(app.view.model, OTHER_MODEL);
    assert_eq!(app.view.composer_model, OTHER_MODEL);
    assert_eq!(app.view.native_provider_id, "codex");
    assert_eq!(app.view.selected_backend, "native");
    assert_eq!(
        ids(&app.view.composer_effort_options),
        ["default", "minimal", "high"]
    );
    assert_eq!(app.view.composer_effort, "default");
    apply(&mut app, "composer_effort:max");
    assert_eq!(app.view.composer_effort, "default");
    apply(&mut app, "composer_effort:minimal");
    assert_eq!(app.view.composer_effort, "minimal");
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn native_codex_discovery_during_a_running_turn_preserves_its_selected_model_and_effort() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    deliver(&mut app, supported_discovery());
    assert_eq!(app.view.composer_effort, "max");
    let current_options = app.view.composer_effort_options.clone();
    let draft = app.view.prompt.clone();
    let preferences = std::fs::read(app.data_root.join("composer-preferences.json")).unwrap();
    let owner = app.view.selected_session.clone();
    app.view.running = true;
    deliver(
        &mut app,
        discovery(vec![
            model(MODEL, &["low", "high"], Some(true)),
            model(OTHER_MODEL, &["medium"], Some(true)),
        ]),
    );
    assert_eq!(app.view.composer_effort, "max");
    assert_eq!(app.view.composer_effort_options, current_options);
    assert_eq!(app.view.composer_model, MODEL);
    assert_eq!(app.view.model, MODEL);
    assert_eq!(app.view.selected_session, owner);
    assert_eq!(app.view.prompt, draft);
    assert_eq!(
        std::fs::read(app.data_root.join("composer-preferences.json")).unwrap(),
        preferences
    );
    let new_route = app
        .view
        .composer_routes
        .iter()
        .find(|route| route.model_id == MODEL && route.provider_id == "codex")
        .unwrap();
    assert_eq!(
        ids(&new_route.effort_options),
        ["default", "low", "high"],
        "the catalog still updates independently of the active turn"
    );
    assert!(app
        .store
        .as_ref()
        .unwrap()
        .load(owner.as_deref().unwrap())
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    assert!(
        app.active_run.is_none() && app.active_resume.is_none(),
        "the test never admits a real turn or request"
    );
    app.view.running = false;
    app.refresh_native_composer_choice_metadata();
    assert_eq!(
        ids(&app.view.composer_effort_options),
        ["default", "low", "high"]
    );
    assert_eq!(
        app.view.composer_effort, "default",
        "a removed effort is reconciled only after the turn becomes idle"
    );
    assert_eq!(
        std::fs::read(app.data_root.join("composer-preferences.json")).unwrap(),
        preferences
    );
}

#[test]
fn native_codex_refresh_status_keeps_last_known_effort_visible_without_changing_saved_selection() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    deliver(&mut app, supported_discovery());
    let options = app.view.composer_effort_options.clone();
    let preferences = std::fs::read(app.data_root.join("composer-preferences.json")).unwrap();
    let listing = app.model_discoveries.clone();
    app.view
        .composer_provider_status
        .insert("codex".into(), ProviderModelStatus::Checking);
    app.refresh_composer_routes();
    let checking = frame(&mut app, 900.0);
    assert!(checking.commands.iter().any(|command| matches!(command,
        DrawCommand::Text { text, .. } if text == "Máximo"
    )));
    export_visual(&checking, 900.0, "checking");

    let (sender, receiver) = mpsc::channel();
    sender
        .send((
            "codex".into(),
            Err("Synthetic discovery failure; no request was sent".into()),
        ))
        .unwrap();
    app.model_discovery_response = Some(receiver);
    app.model_discovery_pending = 1;
    assert!(app.poll_model_discovery());
    assert!(app.model_discovery_response.is_none());
    assert_eq!(
        app.view.composer_provider_status.get("codex"),
        Some(&ProviderModelStatus::CheckFailed)
    );
    assert_eq!(app.model_discoveries, listing);
    assert_eq!(app.view.composer_effort_options, options);
    assert_eq!(app.view.composer_effort, "max");
    let failed = frame(&mut app, 900.0);
    assert!(failed.commands.iter().any(|command| matches!(command,
        DrawCommand::Text { text, .. } if text == "Máximo"
    )));
    export_visual(&failed, 900.0, "check-failed");
    assert_eq!(
        std::fs::read(app.data_root.join("composer-preferences.json")).unwrap(),
        preferences
    );
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}
