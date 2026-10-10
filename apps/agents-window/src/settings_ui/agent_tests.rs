use super::*;
use crate::composer_selection::{
    CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource,
};
use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};

fn text_source(frame: &openide_ui::Frame) -> String {
    let paragraphs = frame
        .text_layouts()
        .iter()
        .map(|block| block.source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let controls = frame
        .commands
        .iter()
        .filter_map(|command| match command {
            openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{paragraphs}\n{controls}")
}

#[test]
fn settings_agent_exposes_native_harness_and_preserves_legacy_history_origin() {
    let mut state = crate::view::tests::empty_state();
    state.selected_backend = "external-engine".into();
    state
        .backend_options
        .push(("external-engine".into(), "Mi CLI".into()));
    state.selected_session = Some("legacy-session".into());
    state.sessions.push(crate::view::SessionView {
        id: "legacy-session".into(),
        title: "Historial importado".into(),
        project: state.workspace.clone(),
        backend: "external-engine".into(),
        native_automation: None,
        pinned: false,
        archived: false,
        unread: false,
    });
    state.native_provider_id = "tenant.provider".into();
    state.model = "generation/v1:exact".into();
    state.composer_routes = vec![ModelRouteOption {
        route_id: "opaque-id".into(),
        backend: "native".into(),
        provider_id: state.native_provider_id.clone(),
        provider_name: "Tenant".into(),
        model_id: state.model.clone(),
        model_name: "Generation".into(),
        credential: CredentialAvailability::MissingCredential,
        execution: ExecutionSupport::UnverifiedUserChoice,
        source: RouteSource::UserConfigured,
        catalog_present: false,
        provider_listed: None,
        effort_options: vec![],
    }];
    for width in [360.0, 900.0] {
        for (write, run, expected) in [
            (false, false, "Escritura y comandos desactivados"),
            (true, false, "Personalizado: modificar archivos"),
            (false, true, "Personalizado: ejecutar comandos"),
            (true, true, "Escritura y comandos activos"),
        ] {
            state.native_tool_permissions.write_file = write;
            state.native_tool_permissions.run_command = run;
            // The view derives the switch from the two actual permissions.
            state.workspace_writes_enabled = !(write && run);
            let mut tree = UiTree::new(Element::column(agent_page(&state, width < 640.0)));
            let frame = tree.layout(Size::new(width, 10000.0));
            let copy = text_source(&frame);
            for value in [
                "Agente nativo de OpenIDE",
                "Proveedor y modelo",
                "tenant.provider",
                "generation/v1:exact",
                "Falta la credencial",
                expected,
                "Los permisos de lectura",
            ] {
                if value == "Los permisos de lectura" && (write || run) {
                    continue;
                }
                assert!(copy.contains(value), "{value} at {width}: {copy}");
            }
            assert!(!copy.contains("Elegir motor") && !copy.contains("Motor seleccionado"));
            assert!(!copy.contains("Origen del historial"));
            assert!(
                copy.contains("Elegir modelo") && copy.contains("Proveedores"),
                "{width}: {copy}"
            );
            assert!(state.native_tool_permissions.read_file);
            let region = frame
                .regions
                .iter()
                .find(|region| region.id == "write_mode")
                .unwrap();
            let position = Point::new(
                region.rect.x + region.rect.width / 2.0,
                region.rect.y + region.rect.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(position));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(position)),
                vec![UiAction::Clicked {
                    id: "write_mode".into()
                }]
            );
            for region in &frame.regions {
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1,
                    "{} at {width}",
                    region.id
                );
            }
        }
        state.selected_backend = "native".into();
        state.selected_session = None;
        state.composer_routes[0].credential = CredentialAvailability::EnvPresentUnverified;
        let mut tree = UiTree::new(Element::column(agent_page(&state, width < 640.0)));
        let frame = tree.layout(Size::new(width, 10000.0));
        let copy = text_source(&frame);
        assert!(copy.contains("Agente nativo de OpenIDE") && copy.contains("Proveedor y modelo"));
        assert!(!copy.contains("Mi CLI") && !copy.contains("Origen del historial"));
        assert!(!copy.contains("Elegir motor") && !copy.contains("CLI externas"));
        assert!(copy.contains("Credencial detectada") && copy.contains("aún debe verificarse"));
        let button = frame
            .regions
            .iter()
            .find(|region| region.id == "settings_engine_open")
            .unwrap()
            .rect;
        let position = Point::new(
            button.x + button.width / 2.0,
            button.y + button.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(position));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(position)),
            vec![UiAction::Clicked {
                id: "settings_engine_open".into()
            }]
        );
        state.running = true;
        tree.set_root(Element::column(agent_page(&state, width < 640.0)));
        tree.layout(Size::new(width, 10000.0));
        assert!(!tree.focus("settings_engine_open"));
        state.running = false;
        state.selected_backend = "external-engine".into();
        // A stale saved engine preference without a selected historical
        // conversation must not expose an alternative engine or fake origin.
        tree.set_root(Element::column(agent_page(&state, width < 640.0)));
        let frame = tree.layout(Size::new(width, 10000.0));
        let copy = text_source(&frame);
        assert!(copy.contains("Agente nativo de OpenIDE"));
        assert!(!copy.contains("Mi CLI") && !copy.contains("Origen del historial"));
        state.selected_session = Some("legacy-session".into());
        state.composer_routes[0].credential = CredentialAvailability::MissingCredential;
    }
}

#[test]
fn settings_agent_exposes_unavailable_saved_isolation_without_silent_fallback() {
    if CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
        return;
    }
    let mut state = crate::view::tests::empty_state();
    state.native_command_isolation = CommandIsolation::WorkspaceWriteOffline;
    state.native_tool_permissions.run_command = true;
    state.settings_choice_menu_open = Some("isolation".into());
    for width in [360.0, 900.0] {
        let mut tree = UiTree::new(Element::column(agent_page(&state, width < 640.0)));
        let frame = tree.layout(Size::new(width, 10000.0));
        let copy = text_source(&frame);
        assert!(copy.contains("Los comandos quedan bloqueados"));
        assert!(copy.contains("no se cambia automáticamente"));
        assert!(!tree.focus("command_isolation:workspace_write_offline"));
        assert!(tree.focus("command_isolation:host_unrestricted"));
        assert_eq!(
            state.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline
        );
    }
}

fn route(provider: &str, model: &str) -> ModelRouteOption {
    ModelRouteOption {
        route_id: "opaque-configured-route".into(),
        backend: "native".into(),
        provider_id: provider.into(),
        provider_name: "Configured provider".into(),
        model_id: model.into(),
        model_name: "Configured model".into(),
        credential: CredentialAvailability::MissingCredential,
        execution: ExecutionSupport::UnverifiedUserChoice,
        source: RouteSource::UserConfigured,
        catalog_present: false,
        provider_listed: None,
        effort_options: Vec::new(),
    }
}

#[test]
fn settings_agent_without_native_route_hides_defaults_and_connects_to_provider_settings() {
    let mut state = crate::view::tests::empty_state();
    state.selected_backend = "native".into();
    state.native_provider_id = "custom".into();
    state.model = "gpt-4.1".into();
    for with_foreign_route in [false, true] {
        state.composer_routes = if with_foreign_route {
            let mut foreign = route("custom", "gpt-4.1");
            foreign.backend = "codex".into();
            foreign.credential = CredentialAvailability::ExternalCliManaged;
            foreign.execution = ExecutionSupport::DelegatedCli;
            vec![foreign]
        } else {
            Vec::new()
        };
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(Element::column(agent_page(&state, width < 640.0)));
            let frame = tree.layout(Size::new(width, 10000.0));
            let copy = text_source(&frame);
            assert!(copy.contains("No hay modelos disponibles"));
            assert!(!copy.contains("Proveedor: custom") && !copy.contains("Modelo: gpt-4.1"));
            assert!(!copy.contains("Ruta sin verificación disponible"));
            let button = frame
                .regions
                .iter()
                .find(|region| region.id == "composer_connect_provider")
                .unwrap();
            assert!(button.rect.x >= 0.0 && button.rect.x + button.rect.width <= width + 0.1);
            let position = Point::new(
                button.rect.x + button.rect.width / 2.0,
                button.rect.y + button.rect.height / 2.0,
            );
            assert_eq!(frame.hit_test(position), Some("composer_connect_provider"));
            tree.handle_event(UiEvent::PointerDown(position));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(position)),
                vec![UiAction::Clicked {
                    id: "composer_connect_provider".into()
                }]
            );
            assert!(tree.focus("composer_connect_provider"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::Enter,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked {
                    id: "composer_connect_provider".into()
                }]
            );
        }
    }
    assert_eq!(state.native_provider_id, "custom");
    assert_eq!(state.model, "gpt-4.1");
}

#[test]
fn settings_agent_requires_an_exact_native_pair_before_displaying_profile() {
    let mut state = crate::view::tests::empty_state();
    state.selected_backend = "native".into();
    state.native_provider_id = "custom".into();
    state.model = "gpt-4.1".into();
    for candidate in [
        route("another-provider", "gpt-4.1"),
        route("custom", "another-exact-model"),
    ] {
        state.composer_routes = vec![candidate];
        let frame = UiTree::new(Element::column(agent_page(&state, true)))
            .layout(Size::new(360.0, 10000.0));
        let copy = text_source(&frame);
        assert!(copy.contains("No hay un modelo nativo seleccionado"));
        assert!(!copy.contains("Proveedor: custom") && !copy.contains("Modelo: gpt-4.1"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "settings_engine_open"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_connect_provider"));
    }
}

#[test]
fn settings_agent_keeps_complete_explicit_profile_with_missing_credentials_or_unsupported_model() {
    let provider = "tenant.provider.integration.with.a.complete.exact.id";
    let model = "organization/future-model:v42-with-complete-exact-identity-ñ🙂";
    let mut state = crate::view::tests::empty_state();
    state.selected_backend = "native".into();
    state.native_provider_id = provider.into();
    state.model = model.into();
    for (credential, execution, expected) in [
        (
            CredentialAvailability::MissingCredential,
            ExecutionSupport::UnverifiedUserChoice,
            "Falta la credencial del proveedor",
        ),
        (
            CredentialAvailability::NoKeyRequired,
            ExecutionSupport::Unsupported,
            "Esta ruta no está disponible para ejecutar el agente",
        ),
        (
            CredentialAvailability::EnvPresentUnverified,
            ExecutionSupport::UnverifiedUserChoice,
            "el acceso de generación aún debe verificarse",
        ),
    ] {
        let mut candidate = route(provider, model);
        candidate.credential = credential;
        candidate.execution = execution;
        state.composer_routes = vec![candidate];
        for width in [360.0, 900.0] {
            let frame = UiTree::new(Element::column(agent_page(&state, width < 640.0)))
                .layout(Size::new(width, 10000.0));
            let copy = text_source(&frame);
            assert!(copy.contains(&format!("Proveedor: {provider}")));
            assert!(copy.contains(&format!("Modelo: {model}")));
            assert!(copy.contains(expected));
            assert!(
                !copy.contains("No hay modelos disponibles")
                    && !copy.contains("No hay un modelo nativo seleccionado")
            );
            for region in &frame.regions {
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1,
                    "{} at {width}",
                    region.id
                );
            }
        }
    }
}
