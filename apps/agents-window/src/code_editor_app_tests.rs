use super::*;
use openide_editor::{EditKind, Selection};
use openide_ui::{DrawCommand, Key, UiEvent, UiTree};

fn wait(host: &mut CodeEditorHost) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while host
        .documents
        .values()
        .any(|d| d.loading || d.saving.is_some())
    {
        host.poll();
        assert!(Instant::now() < deadline, "file worker did not complete");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn fixture(text: &str) -> (tempfile::TempDir, CodeEditorHost) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), text).unwrap();
    std::fs::create_dir(dir.path().join("history")).unwrap();
    let mut host = CodeEditorHost::default();
    host.start_load(7, dir.path().canonicalize().unwrap(), "main.rs".into());
    wait(&mut host);
    assert!(host.active_document().unwrap().error.is_none());
    (dir, host)
}
fn append(host: &CodeEditorHost, value: &str, timestamp: u64) {
    let shared = host
        .active_document()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let length = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(length)).unwrap();
    buffer.insert(value, EditKind::Paste, timestamp).unwrap();
}
fn text(host: &CodeEditorHost) -> String {
    let snapshot = host
        .active_document()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    snapshot.text_range(0..snapshot.char_count()).unwrap()
}

#[test]
fn activity_reopen_checks_cached_clean_content_without_resetting_unchanged_documents() {
    let (dir, mut host) = fixture("initial\n");
    let identity = host
        .view()
        .unwrap()
        .controller
        .unwrap()
        .snapshot()
        .unwrap()
        .document_id();
    host.refresh_clean();
    wait(&mut host);
    assert_eq!(
        host.view()
            .unwrap()
            .controller
            .unwrap()
            .snapshot()
            .unwrap()
            .document_id(),
        identity
    );
    std::fs::write(dir.path().join("main.rs"), "updated by agent\r\n").unwrap();
    host.refresh_clean();
    wait(&mut host);
    assert_eq!(text(&host), "updated by agent\r\n");
    assert_ne!(
        host.view()
            .unwrap()
            .controller
            .unwrap()
            .snapshot()
            .unwrap()
            .document_id(),
        identity
    );
    append(&host, "my draft", 10);
    std::fs::write(dir.path().join("main.rs"), "external again\n").unwrap();
    host.refresh_clean();
    assert!(!host.active_document().unwrap().loading);
    assert_eq!(text(&host), "updated by agent\r\nmy draft");
}

#[test]
fn activity_refresh_never_overwrites_typing_that_arrives_during_the_check() {
    let (dir, mut host) = fixture("original\n");
    std::fs::write(dir.path().join("main.rs"), "external\n").unwrap();
    host.refresh_clean();
    append(&host, "typed while checking", 10);
    wait(&mut host);
    assert_eq!(text(&host), "original\ntyped while checking");
    assert!(host.active_document().unwrap().notice.is_some());
    assert!(host.has_pending_changes());
}

#[test]
fn editing_paint_keyboard_and_real_save_preserve_unicode_crlf() {
    let (dir, mut host) = fixture("fn main() {\r\n    // café 👩‍💻\r\n}\r\n");
    let view = host.view().unwrap();
    let mut tree = UiTree::new(code_editor_ui::build(
        &view,
        &Default::default(),
        360.0,
        Default::default(),
        false,
    ));
    tree.layout(Size::new(360.0, 480.0));
    let id = code_editor_ui::view_id(7);
    assert!(tree.focus(&id));
    tree.handle_event(UiEvent::Key {
        key: Key::End,
        ctrl: true,
        shift: false,
    });
    let actions = tree.handle_event(UiEvent::Paste("// añadido 🌎\r\n".into()));
    assert!(matches!(
        actions.as_slice(),
        [UiAction::EditorChanged { .. }]
    ));
    let actions = tree.handle_event(UiEvent::Key {
        key: Key::S,
        ctrl: true,
        shift: false,
    });
    let snapshot = view.controller.unwrap().snapshot().unwrap();
    assert_eq!(
        actions,
        vec![UiAction::EditorSaveRequested {
            id,
            document_id: snapshot.document_id(),
            revision: snapshot.revision()
        }]
    );
    host.save(Some(dir.path().join("history")));
    wait(&mut host);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        text(&host)
    );
    assert!(text(&host).ends_with("// añadido 🌎\r\n"));
    assert!(!host.has_pending_changes());
}

#[test]
fn save_acknowledges_its_snapshot_without_cleaning_newer_edits() {
    let (dir, mut host) = fixture("original\n");
    append(&host, "first\n", 10);
    host.save(Some(dir.path().join("history")));
    append(&host, "second\n", 20);
    wait(&mut host);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        "original\nfirst\n"
    );
    assert_eq!(text(&host), "original\nfirst\nsecond\n");
    assert!(host.has_pending_changes());
    host.save(Some(dir.path().join("history")));
    wait(&mut host);
    assert!(!host.has_pending_changes());
}

#[test]
fn external_conflict_retains_buffer_and_original_disk_data() {
    let (dir, mut host) = fixture("original\n");
    append(&host, "my edit\n", 10);
    std::fs::write(dir.path().join("main.rs"), "external edit\n").unwrap();
    host.save(Some(dir.path().join("history")));
    wait(&mut host);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        "external edit\n"
    );
    assert_eq!(text(&host), "original\nmy edit\n");
    assert!(host.has_pending_changes());
    assert!(
        host.active_document()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("disco")
    );
    host.reload();
    wait(&mut host);
    assert_eq!(text(&host), "external edit\n");
    assert!(!host.has_pending_changes());
}

#[test]
fn unavailable_history_never_falls_back_to_an_untracked_save() {
    let (dir, mut host) = fixture("original\n");
    append(&host, "my edit\n", 10);
    host.save(None);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        "original\n"
    );
    assert_eq!(text(&host), "original\nmy edit\n");
    assert!(host.has_pending_changes());
    assert!(host.active_document().unwrap().saving.is_none());
    assert!(
        host.active_document()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("historial")
    );
}

#[test]
fn inactive_saved_document_and_reopened_tab_keep_ownership() {
    let (dir, mut host) = fixture("original\n");
    let controller = host.active_document().unwrap().controller.clone().unwrap();
    let original_id = controller.snapshot().unwrap().document_id();
    append(&host, "mine\n", 10);
    host.save(Some(dir.path().join("history")));
    let second = tempfile::tempdir().unwrap();
    std::fs::write(second.path().join("main.rs"), "other workspace\n").unwrap();
    host.start_load(9, second.path().canonicalize().unwrap(), "main.rs".into());
    wait(&mut host);
    assert_eq!(text(&host), "other workspace\n");
    assert!(!controller.snapshot().unwrap().is_dirty());
    host.active = None;
    host.start_load(12, dir.path().canonicalize().unwrap(), "main.rs".into());
    assert_eq!(host.view().unwrap().tab_id, 12);
    assert_eq!(
        host.active_document()
            .unwrap()
            .controller
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .document_id(),
        original_id
    );
    assert_eq!(text(&host), "original\nmine\n");
    append(&host, "still here", 40);
    host.active = None;
    host.start_load(13, dir.path().canonicalize().unwrap(), "main.rs".into());
    assert!(host.has_pending_changes());
    assert!(text(&host).ends_with("still here"));
}

#[test]
fn document_limit_never_evicts_unsaved_buffers() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = CodeEditorHost::default();
    for id in 1..=MAX_OPEN_DOCUMENTS {
        let path = format!("{id}.rs");
        std::fs::write(dir.path().join(&path), "original").unwrap();
        host.start_load(id as u64, dir.path().canonicalize().unwrap(), path.into());
        wait(&mut host);
        append(&host, "dirty", id as u64);
    }
    std::fs::write(dir.path().join("overflow.rs"), "new").unwrap();
    host.start_load(99, dir.path().canonicalize().unwrap(), "overflow.rs".into());
    assert_eq!(host.documents.len(), MAX_OPEN_DOCUMENTS);
    assert!(host.view().is_none());
    assert!(host.documents.values().all(|d| {
        d.controller
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .is_dirty()
    }));
}

#[test]
fn editor_controls_fit_compact_and_wide_viewports_without_preview_clipping() {
    let (_, mut host) = fixture("fn main() {\n    let greeting = \"Hola 🌎\";\n}\n");
    host.documents
        .values_mut()
        .next()
        .unwrap()
        .scope
        .relative_path = "very-long-real-project-filename-with-many-letters.rs".into();
    for width in [320.0, 360.0, 900.0] {
        let mut tree = UiTree::new(code_editor_ui::build(
            &host.view().unwrap(),
            &Default::default(),
            width,
            Default::default(),
            false,
        ));
        let frame = tree.layout(Size::new(width, 480.0));
        for region in &frame.regions {
            assert!(
                region.rect.x >= -0.01 && region.rect.x + region.rect.width <= width + 0.01,
                "{}: {:?}",
                region.id,
                region.rect
            );
        }
        assert!(
            frame
                .regions
                .iter()
                .any(|r| r.id == "code_editor:7" && r.rect.height > 360.0)
        );
        assert!(!frame.regions.iter().any(|r| r.id == "side_pane_preview"));
        assert!(
            frame
                .commands
                .iter()
                .any(|c| matches!(c, DrawCommand::Text { text, .. } if text.contains("greeting")))
        );
    }
}

#[test]
#[ignore = "opt-in visual review exports"]
fn export_native_editor_visual_review() {
    let directory = std::env::var_os("OPENIDE_EDITOR_VISUAL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/openide-editor-visual-qa"));
    std::fs::create_dir_all(&directory).unwrap();
    let (_, host) = fixture(
        "// Editor propio de openADE\nfn main() {\n    let saludo = \"Hola, mundo 🌎\";\n    println!(\"{}\", saludo);\n}\n",
    );
    for width in [320.0, 360.0, 900.0] {
        let mut tree = UiTree::new(code_editor_ui::build(
            &host.view().unwrap(),
            &Default::default(),
            width,
            Default::default(),
            false,
        ));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(width, 480.0));
        let path = directory.join(format!("editor-{width:.0}.svg"));
        crate::settings_ui::visual_qa::write_frame_svg(&frame, &path, width, 480.0).unwrap();
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        let svg = std::fs::read_to_string(&path).unwrap();
        let svg = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 480).unwrap();
        resvg::render(
            &svg,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap.save_png(path.with_extension("png")).unwrap();
    }
}

fn history_app_fixture() -> (tempfile::TempDir, tempfile::TempDir, AgentsWindow) {
    let (project, host) = fixture("middle\n");
    let data = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(data.path().join("data"));
    app.view.workspace = project
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.selected_session = Some("editor-save-current".into());
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.environment.open = false;
    app.view.show_side_pane = false;
    app.code_editor = host;
    let scope = app.dock_scope();
    let tab = app.dock.open(
        &scope,
        DockTabKind::FilePreview {
            path: "main.rs".into(),
        },
    );
    assert!(app.dock.activate(&scope, tab));
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    app.environment_dirty = false;
    (project, data, app)
}

fn wait_app_saved(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&egui::Context::default());
        if app
            .code_editor
            .documents
            .values()
            .all(|document| document.saving.is_none())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "editor save acknowledgement timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn editor_save_invalidates_environment_and_reloads_only_open_diffs_without_opening_dock() {
    let (project, _data, mut app) = history_app_fixture();
    let workspace = project.path().canonicalize().unwrap();
    let store = app.store.as_ref().unwrap().storage_root().to_path_buf();
    let owner = "editor-save-current";
    let events = vec![
        openide_core::AgentEvent::new(
            owner,
            1,
            openide_core::AgentEventKind::SessionStarted {
                workspace: workspace.clone(),
            },
        ),
        openide_core::AgentEvent::new(
            owner,
            2,
            openide_core::AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: "confirmed-edit".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path":"main.rs","content":"middle\n"}),
                },
            },
        ),
        openide_core::AgentEvent::new(
            owner,
            3,
            openide_core::AgentEventKind::ToolFinished {
                tool_call_id: "confirmed-edit".into(),
                output: "ok".into(),
                is_error: false,
            },
        ),
    ];
    crate::session_changes::capture_baseline(&store, owner, &workspace, &events[..1]).unwrap();
    let mut journal = Vec::new();
    for event in &events {
        serde_json::to_writer(&mut journal, event).unwrap();
        journal.push(b'\n');
    }
    std::fs::write(store.join(format!("{owner}.jsonl")), journal).unwrap();
    app.view.transcript = vec![TranscriptItem::Tool {
        id: "confirmed-edit".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path":"main.rs"}),
        output: Some("ok".into()),
        is_error: false,
        activity: crate::transcript::ActivityStatus::Completed,
    }];
    app.apply_chat_diff_action(&UiAction::Clicked {
        id: crate::chat_diff_ui::action(owner, "confirmed-edit", false),
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.view.chat_diffs.loading.is_empty() {
        app.poll_chat_diffs(&egui::Context::default());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.view.chat_diffs.entries.contains_key("confirmed-edit"));
    app.environment_dirty = false;
    append(&app.code_editor, "manual\n", 10);
    app.code_editor.save(Some(store.clone()));
    // The save was already admitted; hiding the editor must not hide its effect.
    let scope = app.dock_scope();
    app.dock.set_collapsed(&scope, true);
    app.sync_dock_view();
    wait_app_saved(&mut app);
    assert!(app.environment_dirty);
    assert!(!app.view.show_side_pane);
    assert!(!app.view.environment.open);
    assert_eq!(app.view.selected_session.as_deref(), Some(owner));
    assert_eq!(app.view.workspace, workspace.to_string_lossy());
    assert!(app.view.chat_diffs.open.contains("confirmed-edit"));
    assert!(app.view.chat_diffs.entries.is_empty());
    assert_eq!(
        app.view
            .chat_diffs
            .loading
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        ["confirmed-edit"]
    );
    while !app.view.chat_diffs.loading.is_empty() {
        app.poll_chat_diffs(&egui::Context::default());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.view.chat_diffs.entries.contains_key("confirmed-edit"));
    assert!(!app.view.show_side_pane);
    assert!(!app.view.environment.open);
    let changes =
        crate::session_changes::load_stored_session_changes(&store, owner, &workspace).unwrap();
    assert!(
        changes
            .coverage_notes
            .iter()
            .any(|note| note.contains("guardados manuales"))
    );
}

#[test]
fn editor_save_obsolete_document_ack_still_invalidates_its_current_workspace() {
    let (project, _data, mut app) = history_app_fixture();
    append(&app.code_editor, "manual", 10);
    let store = app.store.as_ref().unwrap().storage_root().to_path_buf();
    app.code_editor.save(Some(store));
    // Retire the view before acknowledgement. The durable write still happened.
    app.code_editor.documents.clear();
    app.code_editor.active = None;
    let scope = app.dock_scope();
    app.dock.set_collapsed(&scope, true);
    app.sync_dock_view();
    app.environment_dirty = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.environment_dirty {
        app.poll_code_editor(&egui::Context::default());
        assert!(Instant::now() < deadline, "obsolete Saved was not observed");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read_to_string(project.path().join("main.rs")).unwrap(),
        "middle\nmanual"
    );
    assert!(!app.view.environment.open);
    assert!(!app.view.show_side_pane);
}

#[test]
fn editor_save_inactive_workspace_does_not_invalidate_or_replace_the_current_chat_scope() {
    let (project, _data, mut app) = history_app_fixture();
    append(&app.code_editor, "manual", 10);
    let store = app.store.as_ref().unwrap().storage_root().to_path_buf();
    app.code_editor.save(Some(store));
    let other = tempfile::tempdir().unwrap();
    let workspace = other.path().canonicalize().unwrap();
    app.view.workspace = workspace.to_string_lossy().into_owned();
    app.view.selected_session = Some("different-chat".into());
    let inactive_scope = app.dock_scope();
    app.dock.set_collapsed(&inactive_scope, true);
    app.sync_dock_view();
    app.view.chat_diffs.workspace = Some(workspace.clone());
    app.view.chat_diffs.session_id = app.view.selected_session.clone();
    app.view.chat_diffs.entries.insert(
        "cached-current".into(),
        crate::chat_diff_ui::ToolDiffEntry::from_result(Err("sentinel".into())),
    );
    app.environment_dirty = false;
    wait_app_saved(&mut app);
    assert!(!app.environment_dirty);
    assert!(app.view.chat_diffs.entries.contains_key("cached-current"));
    assert_eq!(
        app.view.chat_diffs.session_id.as_deref(),
        Some("different-chat")
    );
    assert_eq!(
        app.view.chat_diffs.workspace.as_deref(),
        Some(workspace.as_path())
    );
    assert_eq!(app.view.workspace, workspace.to_string_lossy());
    assert_eq!(
        std::fs::read_to_string(project.path().join("main.rs")).unwrap(),
        "middle\nmanual"
    );
    assert!(!app.view.environment.open);
    assert!(!app.view.show_side_pane);
}
