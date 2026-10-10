use super::*;
use crate::mcp_inspection::tests::{sample_snapshot, wait_until, HttpFixture};
use crate::mcp_inspection::{McpInspectionPhase, McpInspectionResult};
use openide_ui::{Point, Size, UiEvent};

fn click_inspection(app: &mut AgentsWindow, width: f32) {
    let id = "mcp_inspect:inspection-local";
    let mut tree = UiTree::new(build_view(&app.view, width));
    let viewport = Size::new(width, 700.0);
    tree.layout(viewport);
    assert!(tree.focus(id));
    let frame = tree.layout(viewport);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap()
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert!(actions.contains(&UiAction::Clicked { id: id.into() }));
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

fn prepare_app(root: &Path, workspace: &Path, fixture: &HttpFixture) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.to_path_buf());
    app.change_workspace(workspace.to_string_lossy().into_owned());
    McpSettings {
        servers: vec![fixture.server.clone()],
    }
    .save(root, workspace)
    .unwrap();
    app.refresh_mcp_settings();
    app.view.show_settings = true;
    app.view.settings_page = SettingsPage::Mcp;
    app
}

#[test]
fn mcp_inspection_pointer_runs_real_http_preserves_native_journal_and_reopens_without_connecting() {
    for width in [420.0, 900.0] {
        let fixture = HttpFixture::new(true, false);
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut app = prepare_app(root.path(), workspace.path(), &fixture);
        let store = app.store.as_ref().unwrap().clone();
        let session = store.create(workspace.path(), "native").unwrap();
        let admission = store
            .admit_native_turn(
                &session.id,
                "frozen-turn",
                "Journal fixture only",
                "exact-frozen-model",
                TurnMode::Build,
            )
            .unwrap();
        let frozen_instructions = "Original instructions stay frozen";
        for (index, kind) in [
            AgentEventKind::ToolCatalogCommitted {
                digest: "a".repeat(64),
            },
            AgentEventKind::SystemInstructionsCommitted {
                version: openide_core::SYSTEM_INSTRUCTIONS_VERSION,
                digest: openide_core::system_instructions_digest(frozen_instructions).unwrap(),
                content: frozen_instructions.into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            store
                .append_native_event(&AgentEvent::new(
                    &session.id,
                    admission.sequence + index as u64 + 1,
                    kind,
                ))
                .unwrap();
        }
        let journal = store.storage_root().join(format!("{}.jsonl", session.id));
        let original_journal = std::fs::read(&journal).unwrap();
        let original_settings = std::fs::read(root.path().join("mcp-servers.json")).unwrap();
        let original_routes = app.view.composer_routes.clone();
        let original_model = app.view.model.clone();
        assert!(fixture.methods().is_empty());
        click_inspection(&mut app, width);
        assert!(app.active_mcp_inspection.is_some());
        wait_until(|| {
            app.poll_mcp_inspection();
            app.active_mcp_inspection.is_none()
        });
        assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Completed);
        let snapshot = app.view.mcp_inspection.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.server_info.protocol_version, "2025-06-18");
        assert_eq!(snapshot.tools.len(), 2);
        assert!(snapshot.details_json.contains("FIN_SCHEMA"));
        assert!(fixture.deleted.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(std::fs::read(&journal).unwrap(), original_journal);
        assert_eq!(
            std::fs::read(root.path().join("mcp-servers.json")).unwrap(),
            original_settings
        );
        assert_eq!(app.view.composer_routes, original_routes);
        assert_eq!(app.view.model, original_model);
        let methods = fixture.methods();
        drop(app);
        let mut reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
        reopened.change_workspace(workspace.path().to_string_lossy().into_owned());
        assert!(reopened.active_mcp_inspection.is_none());
        assert_eq!(reopened.view.mcp_inspection.phase, McpInspectionPhase::Idle);
        assert!(reopened.view.mcp_inspection.snapshot.is_none());
        assert_eq!(fixture.methods(), methods);
        assert_eq!(std::fs::read(&journal).unwrap(), original_journal);
    }
}

#[test]
fn mcp_inspection_discards_changed_saved_registration_workspace_and_corrupt_store_after_real_http_start(
) {
    for mutation in ["settings", "workspace", "corrupt"] {
        let fixture = HttpFixture::new(true, true);
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let other_workspace = tempfile::tempdir().unwrap();
        let mut app = prepare_app(root.path(), workspace.path(), &fixture);
        click_inspection(&mut app, 420.0);
        wait_until(|| fixture.blocked.load(std::sync::atomic::Ordering::Acquire));
        match mutation {
            "settings" => {
                let mut changed = fixture.server.clone();
                changed.enabled = false;
                McpSettings {
                    servers: vec![changed],
                }
                .save(root.path(), workspace.path())
                .unwrap();
            }
            "workspace" => {
                app.change_workspace(other_workspace.path().to_string_lossy().into_owned())
            }
            "corrupt" => std::fs::write(
                root.path().join("mcp-servers.json"),
                b"malformed MCP settings",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let saved_bytes = std::fs::read(root.path().join("mcp-servers.json")).unwrap();
        wait_until(|| {
            app.poll_mcp_inspection();
            app.active_mcp_inspection.is_none()
        });
        assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Discarded);
        assert!(app.view.mcp_inspection.snapshot.is_none());
        assert!(app.view.mcp_inspection.error.is_some());
        assert!(app.view.mcp_inspection.shutdown_error.is_none());
        assert!(fixture.deleted.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            std::fs::read(root.path().join("mcp-servers.json")).unwrap(),
            saved_bytes
        );
        assert!(!fixture
            .methods()
            .iter()
            .any(|method| method == "tools/call"));
    }
}

#[test]
fn mcp_inspection_reloads_saved_settings_before_launch_and_invalidates_completed_review_after_change(
) {
    let fixture = HttpFixture::new(false, false);
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut app = prepare_app(root.path(), workspace.path(), &fixture);
    let settings_path = root.path().join("mcp-servers.json");
    let original = std::fs::read(&settings_path).unwrap();
    std::fs::write(&settings_path, b"invalid saved settings").unwrap();
    click_inspection(&mut app, 900.0);
    assert!(app.active_mcp_inspection.is_none());
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Failed);
    assert!(fixture.methods().is_empty());
    assert_eq!(
        std::fs::read(&settings_path).unwrap(),
        b"invalid saved settings"
    );
    std::fs::write(&settings_path, original).unwrap();
    click_inspection(&mut app, 900.0);
    wait_until(|| {
        app.poll_mcp_inspection();
        app.active_mcp_inspection.is_none()
    });
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Completed);
    let mut changed = fixture.server.clone();
    changed.enabled = false;
    McpSettings {
        servers: vec![changed],
    }
    .save(root.path(), workspace.path())
    .unwrap();
    assert!(app.poll_mcp_inspection());
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Discarded);
    assert!(app.view.mcp_inspection.snapshot.is_none());
    assert!(!app.poll_mcp_inspection());
}

#[test]
fn mcp_inspection_window_reopen_during_pending_http_does_not_inherit_or_restart_client() {
    let fixture = HttpFixture::new(true, true);
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut app = prepare_app(root.path(), workspace.path(), &fixture);
    click_inspection(&mut app, 900.0);
    wait_until(|| fixture.blocked.load(std::sync::atomic::Ordering::Acquire));
    let started = Instant::now();
    drop(app);
    assert!(started.elapsed() < Duration::from_millis(100));
    let mut reopened = AgentsWindow::with_data_root(root.path().to_path_buf());
    reopened.change_workspace(workspace.path().to_string_lossy().into_owned());
    assert!(reopened.active_mcp_inspection.is_none());
    assert_eq!(reopened.view.mcp_inspection.phase, McpInspectionPhase::Idle);
    assert!(reopened.view.mcp_inspection.scope.is_none());
    wait_until(|| fixture.deleted.load(std::sync::atomic::Ordering::Acquire));
    let methods = fixture.methods();
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "initialize")
            .count(),
        1
    );
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "tools/list")
            .count(),
        1
    );
    assert_eq!(methods.last().unwrap(), "DELETE");
}

#[test]
fn mcp_inspection_older_generation_cannot_publish_over_newer_scoped_ui_state() {
    let fixture = HttpFixture::new(false, false);
    let workspace = tempfile::tempdir().unwrap();
    let scope = crate::mcp_inspection::McpInspectionScope {
        request_id: 1,
        workspace: workspace.path().canonicalize().unwrap(),
        server: fixture.server.clone(),
    };
    let mut newer = scope.clone();
    newer.request_id = 2;
    let mut state = crate::mcp_inspection::McpInspectionUiState::default();
    state.begin(newer.clone());
    let result = McpInspectionResult {
        scope,
        snapshot: Some(sample_snapshot()),
        error: None,
        shutdown_error: None,
        cancelled: false,
    };
    assert!(!state.apply_result(
        result,
        workspace.path(),
        &McpSettings {
            servers: vec![fixture.server.clone()]
        }
    ));
    assert_eq!(state.scope, Some(newer));
    assert_eq!(state.phase, McpInspectionPhase::Running);
    assert!(state.snapshot.is_none());
    assert!(fixture.methods().is_empty());
}
