//! Files dropped on the window, through the composer's real attachment worker.

use super::*;

fn window() -> (tempfile::TempDir, AgentsWindow) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("notes.md"), "# Notes\n").unwrap();
    let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
    app.view.workspace = workspace.to_string_lossy().into_owned();
    (temp, app)
}

fn drop_files(app: &mut AgentsWindow, paths: Vec<PathBuf>, ctx: &egui::Context) {
    app.apply_action(UiAction::FilesDropped { position: openide_ui::Point::new(10.0, 10.0), target: None, paths }, ctx);
}

#[test]
fn a_file_dropped_on_the_window_is_attached_to_the_composer() {
    let (_temp, mut app) = window();
    let ctx = egui::Context::default();
    let file = PathBuf::from(&app.view.workspace).join("notes.md");
    drop_files(&mut app, vec![file], &ctx);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.view.composer_attachments.is_empty() && Instant::now() < deadline {
        app.poll_composer_attachments(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        app.view.composer_attachments.iter().map(|file| file.label.as_str()).collect::<Vec<_>>(),
        ["notes.md"]
    );
}

#[test]
fn settings_and_other_pages_take_no_drop() {
    let (_temp, mut app) = window();
    let ctx = egui::Context::default();
    let file = PathBuf::from(&app.view.workspace).join("notes.md");
    app.view.show_settings = true;
    drop_files(&mut app, vec![file.clone()], &ctx);
    app.view.show_settings = false;
    app.view.main_page = MainPage::Marketplace;
    drop_files(&mut app, vec![file], &ctx);
    assert!(app.attachment_pastes_pending.is_empty());
    assert!(!app.view.composer_attachment_loading);
}
