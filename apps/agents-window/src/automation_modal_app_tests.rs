//! Modal editing exercises the real action router and durable local store.
//! These tests never poll due tasks or launch an agent/provider.
use super::*;
use crate::automation_view::AutomationEditorStep;
use crate::automations::{AutomationDraft, AutomationSchedule};
use crate::composer_selection::{
    CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource,
};
use openide_ui::{Key, Point, Size, UiEvent};

struct Fixture {
    _directory: tempfile::TempDir,
    app: AgentsWindow,
    workspace: PathBuf,
    context: egui::Context,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("project-a");
        std::fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
        app.change_workspace(workspace.to_string_lossy().into_owned());
        app.view.selected_backend = "codex".into();
        app.view.backend_options = vec![
            ("codex".into(), "Codex".into()),
            ("native".into(), "Nativo".into()),
        ];
        app.tree.set_reduced_motion(true);
        let context = egui::Context::default();
        app.apply_action(
            UiAction::Clicked {
                id: "open_automations".into(),
            },
            &context,
        );
        Self {
            _directory: directory,
            app,
            workspace,
            context,
        }
    }

    fn action(&mut self, id: &str) {
        self.app
            .apply_action(UiAction::Clicked { id: id.into() }, &self.context);
    }

    fn input(&mut self, id: &str, value: &str) {
        self.app.apply_action(
            UiAction::InputChanged {
                id: id.into(),
                value: value.into(),
            },
            &self.context,
        );
    }

    fn content(&mut self, title: &str) {
        self.input("automation_title", title);
        self.input(
            "automation_prompt",
            "Resume los cambios sin modificar archivos.",
        );
    }

    fn agent_step(&mut self) {
        self.action("automation_next");
        self.action("automation_next");
        assert_eq!(
            self.app.view.automations.editor_step,
            AutomationEditorStep::Agent
        );
    }

    fn click(&mut self, id: &str, width: f32) {
        let viewport = Size::new(width, 780.0);
        self.app.layout_workspace_frame(viewport);
        assert!(self.app.tree.focus(id), "{id} must be keyboard reachable");
        let frame = self.app.tree.layout(viewport);
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .rect;
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        assert_eq!(
            frame.hit_test(point),
            Some(id),
            "{id} must receive its click"
        );
        self.app.tree.handle_event(UiEvent::PointerDown(point));
        let actions = self.app.tree.handle_event(UiEvent::PointerUp(point));
        assert!(actions.iter().any(|action| {
            matches!(action, UiAction::Clicked { id: clicked } if clicked == id)
        }));
        for action in actions {
            self.app.apply_action(action, &self.context);
        }
    }

    fn saved(&self) -> AutomationStore {
        AutomationStore::open(&self.app.data_root, &self.workspace).unwrap()
    }

    fn assert_no_execution(&self) {
        assert!(self.app.active_run.is_none());
        assert!(self.app.active_resume.is_none());
        assert!(self.app.automation_queue.is_empty());
        assert!(self.app.background_automation_runs.is_empty());
        assert!(self.saved().runs().is_empty());
    }
}

#[test]
fn automation_modal_validates_each_step_and_saves_once_without_dispatch() {
    let mut fixture = Fixture::new();
    fixture.action("automation_new");
    fixture.action("automation_next");
    assert_eq!(
        fixture.app.view.automations.editor_step,
        AutomationEditorStep::Content
    );
    assert!(fixture.app.view.automations.error.is_some());
    assert!(fixture.saved().automations().is_empty());

    fixture.content("Resumen del proyecto");
    fixture.action("automation_next");
    fixture.input("automation_time", "25:61");
    fixture.action("automation_next");
    assert_eq!(
        fixture.app.view.automations.editor_step,
        AutomationEditorStep::Schedule
    );
    assert!(fixture.app.view.automations.error.is_some());
    fixture.input("automation_time", "09:15");
    fixture.action("automation_next");
    assert_eq!(
        fixture.app.view.automations.editor_step,
        AutomationEditorStep::Agent
    );
    fixture.click("automation_save", 1100.0);
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    fixture.app.layout_workspace_frame(Size::new(1100.0, 780.0));
    assert_eq!(fixture.app.tree.focused_id(), Some("automation_new"));
    fixture.action("automation_save");

    let saved = fixture.saved();
    assert_eq!(saved.automations().len(), 1);
    assert_eq!(saved.automations()[0].title, "Resumen del proyecto");
    assert_eq!(
        saved.automations()[0].prompt,
        "Resume los cambios sin modificar archivos."
    );
    assert_eq!(saved.automations()[0].backend, "codex");
    assert_eq!(
        saved.automations()[0].schedule,
        AutomationSchedule::Daily {
            hour: 9,
            minute: 15
        }
    );
    fixture.assert_no_execution();
}

#[test]
fn automation_cancel_and_navigation_make_late_editor_actions_inert() {
    let mut fixture = Fixture::new();
    fixture.action("automation_new");
    fixture.content("Borrador cancelado");
    fixture.agent_step();
    fixture.action("automation_choice_toggle:model");
    fixture.action("automation_cancel");
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.editor_menu.is_none());
    let title = fixture.app.view.automations.form.title.clone();
    fixture.input("automation_title", "Entrada atrasada");
    fixture.input("automation_model_query", "consulta atrasada");
    for id in [
        "automation_frequency:once",
        "automation_next",
        "automation_save_all",
        "automation_save",
    ] {
        fixture.action(id);
    }
    assert_eq!(fixture.app.view.automations.form.title, title);
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.model_query.is_empty());
    assert!(fixture.saved().automations().is_empty());

    fixture.action("automation_new");
    fixture.content("Borrador al navegar");
    fixture.agent_step();
    fixture.action("new_session");
    assert_eq!(fixture.app.view.main_page, MainPage::Workspace);
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    fixture.action("automation_save");
    fixture.action("automation_new");
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    fixture.action("open_automations");
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.saved().automations().is_empty());
    fixture.assert_no_execution();
}

#[test]
fn automation_project_change_discards_the_old_modal_and_never_cross_saves() {
    let mut fixture = Fixture::new();
    fixture.action("automation_new");
    fixture.content("Sólo proyecto A");
    fixture.agent_step();
    let project_b = fixture._directory.path().join("project-b");
    std::fs::create_dir(&project_b).unwrap();
    fixture
        .app
        .change_workspace(project_b.to_string_lossy().into_owned());
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.editor_menu.is_none());
    fixture.action("automation_save");
    assert!(fixture.saved().automations().is_empty());
    assert!(AutomationStore::open(&fixture.app.data_root, &project_b)
        .unwrap()
        .automations()
        .is_empty());

    fixture.action("automation_new");
    fixture.content("Sólo proyecto B");
    fixture.agent_step();
    fixture.action("automation_save");
    assert!(fixture.saved().automations().is_empty());
    let saved_b = AutomationStore::open(&fixture.app.data_root, &project_b).unwrap();
    assert_eq!(saved_b.automations().len(), 1);
    assert_eq!(saved_b.automations()[0].title, "Sólo proyecto B");
    assert!(saved_b.runs().is_empty());
}

#[test]
fn automation_window_mode_navigation_closes_the_editor_before_its_early_return() {
    let mut fixture = Fixture::new();
    fixture.action("automation_new");
    fixture.content("Borrador antes de cambiar de ventana");
    fixture.agent_step();
    fixture.action("automation_choice_toggle:backend");
    assert_eq!(
        fixture.app.view.automations.editor_menu.as_deref(),
        Some("backend")
    );
    fixture.action("window_mode_toggle");
    assert_eq!(fixture.app.view.main_page, MainPage::Workspace);
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.editor_menu.is_none());
    assert!(!fixture.app.automation_editor_active());
    fixture.app.layout_workspace_frame(Size::new(1100.0, 780.0));
    assert!(!crate::view::dialog_open(&fixture.app.view, 1100.0));

    // The closed editor must not leave the workbench's real shortcut route
    // blocked, even though window_mode_toggle bypasses apply_action's tail.
    let modal = fixture.app.automation_editor_active();
    let (handled, remaining) = super::keymap_app::tests::dispatch(
        &mut fixture.app,
        egui::Key::Comma,
        false,
        false,
        modal,
        false,
    );
    assert!(handled && remaining.is_empty());
    assert!(fixture.app.view.show_settings);
    fixture.action("open_automations");
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    fixture.action("automation_save");
    assert!(fixture.saved().automations().is_empty());
    fixture.assert_no_execution();
}

#[test]
fn automation_navigation_history_restores_pages_without_restoring_the_editor() {
    let mut fixture = Fixture::new();
    fixture.action("new_session");
    fixture.action("open_automations");
    fixture.action("automation_new");
    fixture.content("Borrador fuera del historial de páginas");
    fixture.agent_step();
    fixture.action("automation_choice_toggle:backend");

    // The host also invokes these routes directly for history shortcuts,
    // without passing through apply_action's navigation cleanup.
    fixture.app.go_back();
    assert_eq!(fixture.app.view.main_page, MainPage::Workspace);
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.editor_menu.is_none());
    fixture.app.go_forward();
    assert_eq!(fixture.app.view.main_page, MainPage::Automations);
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.selected_id.is_none());
    fixture.action("automation_save");
    assert!(fixture.saved().automations().is_empty());
    fixture.assert_no_execution();
}

#[test]
fn automation_stale_agent_tab_close_does_not_leave_an_editor_behind_the_new_session() {
    let mut fixture = Fixture::new();
    let store = fixture.app.store.as_ref().unwrap().clone();
    let session = store.create(&fixture.workspace, "native").unwrap();
    fixture.app.open_session(&session.id);
    assert_eq!(
        fixture.app.view.selected_session.as_deref(),
        Some(session.id.as_str())
    );
    fixture.app.view.ide.agent_tabs = vec![session.id.clone()];
    fixture.action("open_automations");
    fixture.action("automation_new");
    fixture.content("Borrador que no debe sobrevivir a la pestaña");
    fixture.agent_step();

    fixture.action(&format!("ide_agent_tab_close:{}", session.id));
    assert_eq!(fixture.app.view.main_page, MainPage::Workspace);
    assert!(fixture.app.view.selected_session.is_none());
    assert!(fixture.app.view.ide.agent_tabs.is_empty());
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    assert!(fixture.app.view.automations.editor_menu.is_none());
    fixture.action("open_automations");
    assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
    fixture.action("automation_save");
    assert!(fixture.saved().automations().is_empty());
    assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
    fixture.assert_no_execution();
}

#[test]
fn automation_modal_rename_preserves_exact_once_and_interval_schedule() {
    for schedule in [
        AutomationSchedule::Once {
            at_ms: 1_700_000_037_123,
        },
        AutomationSchedule::EveryMinutes {
            interval: 47,
            anchor_ms: 1_700_000_037_123,
        },
    ] {
        let mut fixture = Fixture::new();
        let index = fixture.app.current_automation_store_index().unwrap();
        let id = fixture.app.automation_stores[index]
            .create(
                AutomationDraft {
                    title: "Nombre original".into(),
                    prompt: "Resume los cambios sin modificar archivos.".into(),
                    backend: "codex".into(),
                    native_provider: None,
                    schedule: schedule.clone(),
                },
                1_700_000_000_000,
            )
            .unwrap();
        let before = fixture.app.automation_stores[index].automations()[0].clone();
        fixture.app.refresh_automation_view();
        fixture.action(&format!("automation_edit:{id}"));
        fixture.input("automation_title", "Nombre actualizado");
        fixture.agent_step();
        fixture.click("automation_save", 1100.0);
        assert_eq!(
            fixture.app.view.automations.screen,
            AutomationScreen::List,
            "{:?}",
            fixture.app.view.automations.error
        );
        fixture.app.layout_workspace_frame(Size::new(1100.0, 780.0));
        assert_eq!(
            fixture.app.tree.focused_id(),
            Some(format!("automation_edit:{id}").as_str())
        );
        let saved = fixture.saved();
        let after = &saved.automations()[0];
        assert_eq!(after.id, id);
        assert_eq!(after.title, "Nombre actualizado");
        assert_eq!(after.schedule, schedule);
        assert_eq!(after.next_run_at, before.next_run_at);
        assert_eq!(after.run_count, before.run_count);
        assert_eq!(after.last_run_at, before.last_run_at);
        fixture.assert_no_execution();
    }
}

fn route(model: &str) -> ModelRouteOption {
    ModelRouteOption {
        route_id: format!("native:openai:{model}"),
        backend: "native".into(),
        provider_id: "openai".into(),
        provider_name: "OpenAI".into(),
        model_id: model.into(),
        model_name: model.into(),
        credential: CredentialAvailability::EnvPresentUnverified,
        execution: ExecutionSupport::Verified,
        source: RouteSource::AdapterReview,
        catalog_present: true,
        provider_listed: Some(true),
        effort_options: Vec::new(),
    }
}

#[test]
fn automation_model_menu_uses_original_indices_and_a_stable_discovery_snapshot() {
    let mut fixture = Fixture::new();
    let mut cli = route("cli-model-must-not-become-native");
    cli.route_id = "codex:codex:cli-model-must-not-become-native".into();
    cli.backend = "codex".into();
    cli.provider_id = "codex".into();
    cli.credential = CredentialAvailability::ExternalCliManaged;
    cli.execution = ExecutionSupport::DelegatedCli;
    cli.source = RouteSource::CliCatalog;
    assert!(cli.selectable(), "the decoy is a valid CLI route");
    let mut unavailable = route("unavailable-native-model");
    unavailable.provider_listed = Some(false);
    assert!(!unavailable.selectable());
    fixture.app.view.composer_routes = vec![
        route("first-model"),
        cli,
        unavailable,
        route("second-model"),
    ];
    fixture.app.refresh_automation_view();
    assert_eq!(
        fixture
            .app
            .view
            .automations
            .models
            .iter()
            .map(|(_, model, _)| model.as_str())
            .collect::<Vec<_>>(),
        ["first-model", "second-model"]
    );
    let current_native = fixture.app.native_settings();
    fixture.action("automation_new");
    fixture.content("Modelo propio de la tarea");
    fixture.action("automation_choice_toggle:model");
    assert!(fixture.app.view.automations.editor_menu.is_none());
    fixture.agent_step();
    fixture.click("automation_choice_toggle:backend", 1100.0);
    fixture.click("automation_backend:native", 1100.0);
    let before = fixture.app.view.automations.form.native_provider.clone();
    fixture.action("automation_model:1");
    assert_eq!(fixture.app.view.automations.form.native_provider, before);
    fixture.click("automation_choice_toggle:model", 1100.0);
    fixture.input("automation_model_query", "second");
    let models = fixture.app.view.automations.models.clone();
    fixture.app.view.composer_routes.reverse();
    fixture.app.refresh_automation_view();
    assert_eq!(fixture.app.view.automations.models, models);
    let frame = fixture.app.layout_workspace_frame(Size::new(1100.0, 780.0));
    assert!(frame
        .regions
        .iter()
        .any(|region| region.id == "automation_model:1"));
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id == "automation_model:0"));
    fixture.click("automation_model:1", 1100.0);
    let selected = fixture
        .app
        .view
        .automations
        .form
        .native_provider
        .clone()
        .unwrap();
    assert_eq!(selected.provider_id, "openai");
    assert_eq!(selected.model_id, "second-model");
    assert!(fixture.app.view.automations.editor_menu.is_none());
    fixture.action("automation_model:0");
    fixture.action("automation_native_current");
    assert_eq!(
        fixture.app.view.automations.form.native_provider.as_ref(),
        Some(&selected)
    );
    assert_eq!(fixture.app.native_settings(), current_native);
    fixture.action("automation_save");
    assert_eq!(
        fixture.saved().automations()[0].native_provider.as_ref(),
        Some(&selected)
    );
    fixture.assert_no_execution();
}

#[test]
fn automation_modal_centers_over_the_window_and_escape_restores_the_trigger() {
    for width in [420.0, 1100.0] {
        let mut fixture = Fixture::new();
        let viewport = Size::new(width, 780.0);
        fixture.app.layout_workspace_frame(viewport);
        assert!(fixture.app.tree.focus("automation_new"));
        fixture.action("automation_new");
        let frame = fixture.app.layout_workspace_frame(viewport);
        assert_eq!(fixture.app.tree.focused_id(), Some("automation_title"));
        let card = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:automation_cancel")
            .expect("full-window editor overlay")
            .rect;
        assert!((card.x + card.width / 2.0 - width / 2.0).abs() <= 1.0);
        let actions = fixture.app.tree.handle_event(UiEvent::Key {
            key: Key::Escape,
            shift: false,
            ctrl: false,
        });
        assert!(actions
            .iter()
            .any(|action| matches!(action, UiAction::Clicked { id } if id == "automation_cancel")));
        for action in actions {
            fixture.app.apply_action(action, &fixture.context);
        }
        fixture.app.layout_workspace_frame(viewport);
        assert_eq!(fixture.app.view.automations.screen, AutomationScreen::List);
        assert_eq!(fixture.app.tree.focused_id(), Some("automation_new"));
        assert!(fixture.saved().automations().is_empty());
    }
}
