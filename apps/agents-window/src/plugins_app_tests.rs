//! Host lifecycle tests use only temporary local packages and private data roots.

use super::*;
use serde_json::json;
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn prepare(root: &Path, workspace: &Path) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.to_path_buf());
    app.view.workspace = workspace.display().to_string();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.show_settings = true;
    app.view.settings_page = SettingsPage::Plugins;
    app.view.plugins.workspace = app.view.workspace.clone();
    app
}

fn package(source: &Path) -> PluginPreview {
    fs::write(
        source.join("openide-plugin.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "id": "echo",
            "name": "Echo fixture",
            "version": "1.0.0",
            "description": "Host lifecycle fixture",
            "publisher": "OpenIDE tests",
            "program": "sh",
            "entrypoint": "server.sh"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(source.join("server.sh"), "exit 91\n").unwrap();
    PluginStore::inspect(source).unwrap()
}

fn click(app: &mut AgentsWindow, id: &str) {
    assert!(app.apply_plugin_action(&UiAction::Clicked { id: id.into() }));
}

fn input(app: &mut AgentsWindow, id: &str, value: &str) {
    assert!(app.apply_plugin_action(&UiAction::InputChanged {
        id: id.into(),
        value: value.into()
    }));
}

fn finish(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.plugin_response.is_some() {
        assert!(Instant::now() < deadline, "plugin worker timed out");
        app.poll_plugins();
        if app.plugin_response.is_some() {
            thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(!app.view.plugins.busy);
    assert!(!app.plugin_removal_pending);
}

fn install(app: &mut AgentsWindow, source: &Path) {
    click(app, "plugins_install_open");
    input(app, "plugins_source", &source.display().to_string());
    click(app, "plugins_install_confirm");
    assert!(app.plugin_response.is_none(), "review is mandatory");
    click(app, "plugins_inspect");
    assert!(app.view.plugins.busy);
    finish(app);
    assert!(app.view.plugins.preview.is_some());
    click(app, "plugins_install_confirm");
    finish(app);
    assert!(!app.view.plugins.error, "{:?}", app.view.plugins.status);
    assert!(!app.view.plugins.install_open);
}

#[test]
fn plugins_host_installs_inactive_persists_activation_and_removes_only_after_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    package(source.path());
    let data = root.path().join("data");
    let mut app = prepare(&data, workspace.path());
    install(&mut app, source.path());
    assert_eq!(app.view.plugins.installed.len(), 1);
    assert!(!app.view.plugins.installed[0].enabled);
    let installed_path = app.view.plugins.installed[0].path.clone();
    assert_ne!(installed_path, source.path());
    click(&mut app, "plugins_toggle:echo");
    finish(&mut app);
    assert!(app.view.plugins.installed[0].enabled);

    let mut reopened = prepare(&data, workspace.path());
    click(&mut reopened, "plugins_refresh");
    finish(&mut reopened);
    assert!(reopened.view.plugins.installed[0].enabled);
    click(&mut reopened, "plugins_remove:echo");
    assert!(
        reopened.view.plugins.pending_remove.is_none(),
        "removal requires the details surface"
    );
    click(&mut reopened, "plugins_details:echo");
    click(&mut reopened, "plugins_remove:echo");
    assert!(reopened.view.plugins.pending_remove.is_some());
    assert!(installed_path.exists());
    click(&mut reopened, "plugins_remove_cancel");
    assert!(reopened.view.plugins.pending_remove.is_none());
    assert!(installed_path.exists());

    click(&mut reopened, "plugins_details:echo");
    click(&mut reopened, "plugins_remove:echo");
    click(&mut reopened, "plugins_remove_confirm");
    assert!(reopened.plugin_removal_pending);
    finish(&mut reopened);
    assert!(reopened.view.plugins.installed.is_empty());
    assert!(!installed_path.exists());
    assert!(source.path().join("openide-plugin.json").exists());
    assert!(source.path().join("server.sh").exists());
}

#[test]
fn closing_inspection_does_not_reopen_it_and_changed_review_cannot_be_installed() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    package(source.path());
    let mut app = prepare(&root.path().join("data"), workspace.path());
    click(&mut app, "plugins_install_open");
    input(
        &mut app,
        "plugins_source",
        &source.path().display().to_string(),
    );
    click(&mut app, "plugins_inspect");
    click(&mut app, "plugins_dialog_close");
    finish(&mut app);
    assert!(!app.view.plugins.install_open);
    assert!(app.view.plugins.preview.is_none());

    click(&mut app, "plugins_install_open");
    click(&mut app, "plugins_inspect");
    finish(&mut app);
    assert!(app.view.plugins.preview.is_some());
    fs::write(source.path().join("server.sh"), "exit 92\n").unwrap();
    click(&mut app, "plugins_install_confirm");
    finish(&mut app);
    assert!(app.view.plugins.error);
    assert!(app.view.plugins.installed.is_empty());
    assert!(PluginStore::list(&app.data_root, workspace.path())
        .unwrap()
        .is_empty());
    assert!(
        app.view.plugins.install_open,
        "failure should preserve the review surface"
    );
    input(&mut app, "plugins_source", "/a/different/source");
    assert!(app.view.plugins.preview.is_none());
    click(&mut app, "plugins_install_confirm");
    assert!(app.plugin_response.is_none());
}

#[test]
fn plugin_results_from_another_workspace_are_discarded_and_new_project_is_reloaded() {
    let root = tempfile::tempdir().unwrap();
    let old_workspace = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let preview = package(source.path());
    let mut app = prepare(&root.path().join("data"), workspace.path());
    app.view.plugins.workspace = old_workspace.path().display().to_string();
    app.view.plugins.install_open = true;
    app.view.plugins.busy = true;
    let (sender, receiver) = mpsc::channel();
    app.plugin_response = Some(receiver);
    sender
        .send((
            old_workspace.path().display().to_string(),
            false,
            Ok(PluginResult {
                installed: Vec::new(),
                preview: Some(preview),
                message: Some("STALE PREVIEW".into()),
                close_dialogs: false,
            }),
        ))
        .unwrap();
    assert!(app.poll_plugins());
    assert!(!app.view.plugins.install_open);
    assert!(app.view.plugins.preview.is_none());
    assert_eq!(app.view.plugins.workspace, app.view.workspace);
    finish(&mut app);
    assert!(app.view.plugins.installed.is_empty());
    assert!(app.view.plugins.status.is_none());
}

#[test]
fn active_turn_blocks_removal_and_pending_removal_preserves_run_and_resume_drafts() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let preview = package(source.path());
    let mut app = prepare(&root.path().join("data"), workspace.path());
    PluginStore::install(&app.data_root, workspace.path(), &preview).unwrap();
    click(&mut app, "plugins_refresh");
    finish(&mut app);
    let observed = app.view.plugins.installed[0].clone();
    app.view.running = true;
    app.plugin_operation(PluginOperation::Remove(observed.clone()));
    assert!(app.view.plugins.error);
    assert!(app.plugin_response.is_none());
    assert!(observed.path.exists());
    assert!(!app.plugin_removal_pending);

    app.view.running = false;
    app.plugin_operation(PluginOperation::Remove(observed));
    assert!(app.plugin_removal_pending);
    app.view.prompt = "Preservar este borrador".into();
    let before = app.store.as_ref().unwrap().list().unwrap();
    app.start_run();
    assert_eq!(app.view.prompt, "Preservar este borrador");
    assert!(app.view.status.contains("desinstalación"));
    assert!(app.active_run.is_none());
    app.start_resume();
    assert_eq!(app.view.prompt, "Preservar este borrador");
    assert!(app.view.status.contains("desinstalación"));
    assert!(app.active_resume.is_none());
    assert_eq!(
        app.store.as_ref().unwrap().list().unwrap().len(),
        before.len()
    );
    finish(&mut app);
}

#[test]
fn plugin_inventory_errors_keep_actions_closed_and_disconnected_worker_releases_removal_guard() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut app = prepare(&root.path().join("data"), workspace.path());
    let (sender, receiver) = mpsc::channel();
    app.plugin_response = Some(receiver);
    app.view.plugins.busy = true;
    sender
        .send((
            app.view.workspace.clone(),
            true,
            Err("Malformed inventory fixture".into()),
        ))
        .unwrap();
    app.poll_plugins();
    assert!(app.view.plugins.load_error && app.view.plugins.error);
    assert!(!app.view.plugins.busy);
    let (sender, receiver) = mpsc::channel::<PluginResponse>();
    app.plugin_response = Some(receiver);
    app.plugin_removal_pending = true;
    app.view.plugins.busy = true;
    drop(sender);
    app.poll_plugins();
    assert!(!app.plugin_removal_pending);
    assert!(!app.view.plugins.busy);
    assert!(app.view.plugins.error);
}

#[test]
fn composer_plugins_entry_opens_management_without_installing_or_activating_anything() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut app = prepare(&root.path().join("data"), workspace.path());
    app.view.show_settings = false;
    app.view.composer_add_menu_open = true;
    app.apply_action(
        UiAction::Clicked {
            id: "plugins_open".into(),
        },
        &egui::Context::default(),
    );
    assert!(app.view.show_settings);
    assert_eq!(app.view.settings_page, SettingsPage::Plugins);
    assert!(!app.view.marketplace.sources_open);
    assert!(app.view.shows(SettingsPage::Plugins));
    assert!(!app.view.composer_add_menu_open);
    assert!(app.plugin_response.is_none());
    assert!(PluginStore::list(&app.data_root, workspace.path())
        .unwrap()
        .is_empty());
}
