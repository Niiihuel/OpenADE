//! Offline integration fixtures for the real dock editor and host actions.
use super::*;
use openide_editor::{BufferSnapshot, EditKind, Selection};
use openide_ui::{Frame, HitKind, Key, Point, UiEvent};

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("main.rs"), "// café 🌎\r\nfn main() {}\r\n").unwrap();
    std::fs::write(workspace.join("other.rs"), "// other\n").unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.viewport_width = 900.0;
    app.view.workspace = workspace
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.project_branch = None;
    app.view.sidebar_collapsed = true;
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    (root, app)
}

fn open(app: &mut AgentsWindow, path: &str) -> DockTabId {
    let scope = app.dock_scope();
    let tab = app
        .dock
        .open(&scope, DockTabKind::FilePreview { path: path.into() });
    assert!(app.dock.activate(&scope, tab));
    app.dock.set_collapsed(&scope, false);
    app.dock.set_width_px(&scope.workspace_id, 360.0);
    app.sync_dock_view();
    wait(app);
    assert_eq!(app.view.side_tab, SideTab::FilePreview);
    assert_eq!(app.view.code_editor.as_ref().unwrap().tab_id, tab.get());
    tab
}

fn wait(app: &mut AgentsWindow) {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&ctx);
        let view = app.view.code_editor.as_ref();
        if view.is_none_or(|view| !view.loading && !view.saving)
            && !app.view.code_editor_pending_saves
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "editor fixture worker did not complete"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn snapshot(app: &AgentsWindow) -> BufferSnapshot {
    app.view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap()
}

fn text(app: &AgentsWindow) -> String {
    let snapshot = snapshot(app);
    snapshot.text_range(0..snapshot.char_count()).unwrap()
}

fn append(app: &mut AgentsWindow, value: &str, timestamp: u64) {
    let controller = app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .clone()
        .unwrap();
    let shared = controller.shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let length = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(length)).unwrap();
    buffer.insert(value, EditKind::Paste, timestamp).unwrap();
    let id = buffer.document_id();
    let revision = buffer.revision();
    drop(buffer);
    let view_id = crate::code_editor_ui::view_id(app.view.code_editor.as_ref().unwrap().tab_id);
    apply(
        app,
        UiAction::EditorChanged {
            id: view_id,
            document_id: id,
            revision,
        },
    );
}

fn apply(app: &mut AgentsWindow, action: UiAction) {
    app.apply_action(action, &egui::Context::default());
}

fn clicked(app: &mut AgentsWindow, id: impl Into<String>) {
    apply(app, UiAction::Clicked { id: id.into() });
}

fn frame(app: &mut AgentsWindow, width: f32) -> Frame {
    app.viewport_width = width;
    app.tree.set_root(build_view(&app.view, width));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(width, 720.0))
}

fn click(app: &mut AgentsWindow, painted: &Frame, id: &str) -> Vec<UiAction> {
    // File persistence actions now live in the real header menu. Exercise its
    // pointer trigger rather than manufacturing the save/reload action.
    let expanded;
    let painted = if (id.starts_with("code_editor_save:") || id.starts_with("code_editor_reload:"))
        && !painted
            .regions
            .iter()
            .any(|r| r.id == id && r.kind == HitKind::Button)
    {
        click(app, painted, "editor_menu");
        let width = app.viewport_width;
        expanded = frame(app, width);
        &expanded
    } else {
        painted
    };
    let region = painted
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing clickable editor action {id}"));
    let point = Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    assert_eq!(painted.hit_test(point), Some(id));
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    for action in &actions {
        apply(app, action.clone());
    }
    actions
}

#[test]
fn actual_editor_pointer_text_ctrl_s_and_save_menu_persist_the_private_file() {
    let (_root, mut app) = fixture();
    let tab = open(&mut app, "main.rs");
    let view_id = crate::code_editor_ui::view_id(tab.get());
    let painted = frame(&mut app, 900.0);
    let region = painted
        .regions
        .iter()
        .find(|region| region.id == view_id)
        .expect("real editor body in dock");
    let point = Point::new(region.rect.x + 72.0, region.rect.y + 12.0);
    app.tree.handle_event(UiEvent::PointerDown(point));
    for action in app.tree.handle_event(UiEvent::PointerUp(point)) {
        apply(&mut app, action);
    }
    assert!(app.tree.focus(&view_id));
    app.tree.handle_event(UiEvent::Key {
        key: Key::End,
        ctrl: true,
        shift: false,
    });
    let actions = app
        .tree
        .handle_event(UiEvent::Paste("// escrito desde UI 🧑🏽‍💻\r\n".into()));
    assert!(matches!(
        actions.as_slice(),
        [UiAction::EditorChanged { .. }]
    ));
    for action in actions {
        apply(&mut app, action);
    }
    assert!(snapshot(&app).is_dirty());
    assert!(app
        .view
        .dock_tabs
        .iter()
        .find(|t| t.id == tab.get())
        .unwrap()
        .title
        .ends_with(" •"));
    let actions = app.tree.handle_event(UiEvent::Key {
        key: Key::S,
        ctrl: true,
        shift: false,
    });
    assert!(matches!(
        actions.as_slice(),
        [UiAction::EditorSaveRequested { .. }]
    ));
    for action in actions {
        apply(&mut app, action);
    }
    assert!(app.view.code_editor.as_ref().unwrap().saving);
    wait(&mut app);
    assert!(!snapshot(&app).is_dirty());
    let path = PathBuf::from(&app.view.workspace).join("main.rs");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text(&app));
    assert!(text(&app).ends_with("// escrito desde UI 🧑🏽‍💻\r\n"));
    append(&mut app, "// botón\r\n", 20);
    let painted = frame(&mut app, 900.0);
    assert_eq!(
        click(
            &mut app,
            &painted,
            &format!("code_editor_save:{}", tab.get())
        ),
        vec![UiAction::Clicked {
            id: format!("code_editor_save:{}", tab.get())
        }]
    );
    wait(&mut app);
    assert!(!snapshot(&app).is_dirty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), text(&app));
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn stale_document_revision_view_and_inactive_actions_do_not_save() {
    let (_root, mut app) = fixture();
    let tab = open(&mut app, "main.rs");
    append(&mut app, "// unsaved\r\n", 1);
    let current = snapshot(&app);
    let id = crate::code_editor_ui::view_id(tab.get());
    for action in [
        UiAction::EditorSaveRequested {
            id: id.clone(),
            document_id: current.document_id() + 1,
            revision: current.revision(),
        },
        UiAction::EditorSaveRequested {
            id: id.clone(),
            document_id: current.document_id(),
            revision: current.revision() - 1,
        },
        UiAction::EditorSaveRequested {
            id: "code_editor:999999".into(),
            document_id: current.document_id(),
            revision: current.revision(),
        },
        UiAction::Clicked {
            id: "code_editor_save:999999".into(),
        },
    ] {
        apply(&mut app, action);
    }
    assert!(!app.view.code_editor.as_ref().unwrap().saving);
    assert!(snapshot(&app).is_dirty());
    assert_eq!(
        std::fs::read_to_string(PathBuf::from(&app.view.workspace).join("main.rs")).unwrap(),
        "// café 🌎\r\nfn main() {}\r\n"
    );
    open(&mut app, "other.rs");
    apply(
        &mut app,
        UiAction::EditorSaveRequested {
            id,
            document_id: current.document_id(),
            revision: current.revision(),
        },
    );
    assert!(!app.view.code_editor.as_ref().unwrap().saving);
    assert!(!snapshot(&app).is_dirty());
    assert_eq!(text(&app), "// other\n");
}

#[test]
fn hidden_editor_does_not_accept_stale_keyboard_or_header_save_actions() {
    for hidden_scope in ["settings", "other-page", "collapsed"] {
        let (_root, mut app) = fixture();
        let tab = open(&mut app, "main.rs");
        append(&mut app, "// sin guardar\r\n", 1);
        let current = snapshot(&app);
        match hidden_scope {
            "settings" => app.view.show_settings = true,
            "other-page" => app.view.main_page = MainPage::Automations,
            "collapsed" => {
                let scope = app.dock_scope();
                app.dock.set_collapsed(&scope, true);
                app.sync_dock_view();
            }
            _ => unreachable!(),
        }
        apply(
            &mut app,
            UiAction::EditorSaveRequested {
                id: crate::code_editor_ui::view_id(tab.get()),
                document_id: current.document_id(),
                revision: current.revision(),
            },
        );
        clicked(&mut app, format!("code_editor_save:{}", tab.get()));
        assert!(
            !app.view.code_editor.as_ref().unwrap().saving,
            "stale save accepted with {hidden_scope}"
        );
        assert!(snapshot(&app).is_dirty());
        assert_eq!(
            std::fs::read_to_string(PathBuf::from(&app.view.workspace).join("main.rs")).unwrap(),
            "// café 🌎\r\nfn main() {}\r\n"
        );
    }
}

#[test]
fn switching_closing_reopening_retains_dirty_document_and_manual_sidebar_state() {
    let (_root, mut app) = fixture();
    let first = open(&mut app, "main.rs");
    append(&mut app, "// permanece\r\n", 1);
    let original_id = snapshot(&app).document_id();
    let original_text = text(&app);
    open(&mut app, "other.rs");
    clicked(&mut app, format!("dock_tab:{}", first.get()));
    assert_eq!(app.view.code_editor.as_ref().unwrap().tab_id, first.get());
    assert_eq!(snapshot(&app).document_id(), original_id);
    assert_eq!(text(&app), original_text);
    assert!(snapshot(&app).is_dirty());
    clicked(&mut app, format!("dock_close:{}", first.get()));
    assert!(!app.view.dock_tabs.iter().any(|t| t.id == first.get()));
    let reopened = open(&mut app, "main.rs");
    assert_ne!(reopened.get(), first.get());
    assert_eq!(snapshot(&app).document_id(), original_id);
    assert_eq!(text(&app), original_text);
    assert!(snapshot(&app).is_dirty());
    let scope = app.dock_scope();
    app.dock.set_collapsed(&scope, true);
    app.sync_dock_view();
    assert!(!app.view.show_side_pane);
    app.poll_code_editor(&egui::Context::default());
    let current_revision = snapshot(&app).revision();
    apply(
        &mut app,
        UiAction::EditorChanged {
            id: crate::code_editor_ui::view_id(reopened.get()),
            document_id: original_id,
            revision: current_revision,
        },
    );
    assert!(
        !app.view.show_side_pane,
        "editing and worker polls never open the sidebar"
    );
    assert_eq!(
        std::fs::read_to_string(PathBuf::from(&app.view.workspace).join("main.rs")).unwrap(),
        "// café 🌎\r\nfn main() {}\r\n"
    );
}

#[test]
fn dirty_reload_uses_real_alert_dialog_cancel_escape_stale_scope_and_confirm() {
    let (_root, mut app) = fixture();
    let first = open(&mut app, "main.rs");
    append(&mut app, "// draft\r\n", 1);
    let original_id = snapshot(&app).document_id();
    let original_text = text(&app);
    let painted = frame(&mut app, 900.0);
    click(
        &mut app,
        &painted,
        &format!("code_editor_reload:{}", first.get()),
    );
    assert!(app.view.code_editor_reload_confirm.is_some());
    let modal = frame(&mut app, 360.0);
    assert!(modal
        .regions
        .iter()
        .any(|r| r.id == "code_editor_reload_confirm"));
    let actions = app.tree.handle_event(UiEvent::Key {
        key: Key::Escape,
        ctrl: false,
        shift: false,
    });
    assert_eq!(
        actions,
        vec![UiAction::Clicked {
            id: "code_editor_reload_cancel".into()
        }]
    );
    for action in actions {
        apply(&mut app, action);
    }
    assert!(app.view.code_editor_reload_confirm.is_none());
    assert_eq!(text(&app), original_text);
    clicked(&mut app, format!("code_editor_reload:{}", first.get()));
    let stale = app.view.code_editor_reload_confirm.clone().unwrap();
    open(&mut app, "other.rs");
    app.view.code_editor_reload_confirm = Some(stale);
    clicked(&mut app, "code_editor_reload_confirm");
    assert_eq!(text(&app), "// other\n");
    assert!(!snapshot(&app).is_dirty());
    let scope = app.dock_scope();
    app.dock.activate(&scope, first);
    app.sync_dock_view();
    assert_eq!(snapshot(&app).document_id(), original_id);
    assert_eq!(text(&app), original_text);
    clicked(&mut app, format!("code_editor_reload:{}", first.get()));
    let modal = frame(&mut app, 900.0);
    click(&mut app, &modal, "code_editor_reload_cancel");
    assert!(app.view.code_editor_reload_confirm.is_none());
    assert_eq!(text(&app), original_text);
    clicked(&mut app, format!("code_editor_reload:{}", first.get()));
    let modal = frame(&mut app, 900.0);
    click(&mut app, &modal, "code_editor_reload_confirm");
    wait(&mut app);
    assert!(!snapshot(&app).is_dirty());
    assert_ne!(snapshot(&app).document_id(), original_id);
    assert_eq!(text(&app), "// café 🌎\r\nfn main() {}\r\n");
}

#[test]
fn save_conflict_retains_unsaved_buffer_and_external_disk_file_until_confirmed_reload() {
    let (_root, mut app) = fixture();
    let tab = open(&mut app, "main.rs");
    append(&mut app, "// local draft\r\n", 1);
    let expected = text(&app);
    let path = PathBuf::from(&app.view.workspace).join("main.rs");
    std::fs::write(&path, "// external change\r\n").unwrap();
    let painted = frame(&mut app, 900.0);
    click(
        &mut app,
        &painted,
        &format!("code_editor_save:{}", tab.get()),
    );
    wait(&mut app);
    assert!(snapshot(&app).is_dirty());
    assert_eq!(text(&app), expected);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "// external change\r\n"
    );
    assert!(app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .error
        .as_ref()
        .unwrap()
        .contains("disco"));
    clicked(&mut app, format!("code_editor_reload:{}", tab.get()));
    assert!(app.view.code_editor_reload_confirm.is_some());
    clicked(&mut app, "code_editor_reload_cancel");
    assert_eq!(text(&app), expected);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "// external change\r\n"
    );
}

#[test]
fn failed_save_keeps_unsaved_text_and_missing_disk_file_until_explicit_retry() {
    let (_root, mut app) = fixture();
    let tab = open(&mut app, "main.rs");
    append(&mut app, "// draft after load\r\n", 1);
    let expected = text(&app);
    let document_id = snapshot(&app).document_id();
    let path = PathBuf::from(&app.view.workspace).join("main.rs");
    std::fs::remove_file(&path).unwrap();
    let painted = frame(&mut app, 900.0);
    click(
        &mut app,
        &painted,
        &format!("code_editor_save:{}", tab.get()),
    );
    wait(&mut app);
    assert!(!path.exists());
    assert_eq!(text(&app), expected);
    assert_eq!(snapshot(&app).document_id(), document_id);
    assert!(snapshot(&app).is_dirty());
    assert!(app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .error
        .as_ref()
        .unwrap()
        .contains("archivo"));
    // Restoring the last known disk bytes is a private fixture recovery. The
    // worker does not recreate a deleted file or retry a failed save itself.
    std::fs::write(&path, "// café 🌎\r\nfn main() {}\r\n").unwrap();
    let painted = frame(&mut app, 900.0);
    click(
        &mut app,
        &painted,
        &format!("code_editor_save:{}", tab.get()),
    );
    wait(&mut app);
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    assert!(!snapshot(&app).is_dirty());
    assert!(app.view.code_editor.as_ref().unwrap().error.is_none());
}

#[test]
fn painted_clean_documents_release_evicted_controllers_after_sixteen_files() {
    let (_root, mut app) = fixture();
    let workspace = PathBuf::from(&app.view.workspace);
    let mut controllers = Vec::new();
    for index in 0..32 {
        let name = format!("cache-{index:02}.rs");
        std::fs::write(workspace.join(&name), format!("// file {index}\n")).unwrap();
        let tab = open(&mut app, &name);
        let painted = frame(&mut app, 900.0);
        assert!(painted
            .regions
            .iter()
            .any(|region| region.id == crate::code_editor_ui::view_id(tab.get())));
        let weak = {
            let shared = app
                .view
                .code_editor
                .as_ref()
                .unwrap()
                .controller
                .as_ref()
                .unwrap()
                .shared_buffer();
            Arc::downgrade(&shared)
        };
        assert!(weak.upgrade().is_some());
        controllers.push(weak);
        // All tabs remain open: removing the previous element from the current
        // layout alone must not retain an evicted document through UiTree.
        assert_eq!(
            controllers
                .iter()
                .filter(|weak| weak.upgrade().is_some())
                .count(),
            controllers.len().min(16),
            "painted editor states retained an evicted clean document"
        );
    }
    assert!(controllers[..16]
        .iter()
        .all(|weak| weak.upgrade().is_none()));
    assert!(controllers[16..]
        .iter()
        .all(|weak| weak.upgrade().is_some()));
    assert_eq!(app.view.dock_tabs.len(), 32);
    assert!(!app.view.code_editor_pending_saves);
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn scoped_document_alias_views_are_bounded_and_reload_releases_every_old_alias() {
    let (_root, mut app) = fixture();
    let mut views = Vec::new();
    let mut identity = None;
    let mut old_controller = None;
    for index in 0..32 {
        // Different task scopes have distinct live tabs for the same shared
        // document. Closing a tab would explicitly forget its view, which
        // would not exercise the independent cache limit.
        app.view.selected_session = Some(format!("editor-cache-scope-{index}"));
        let tab = open(&mut app, "main.rs");
        let document_id = snapshot(&app).document_id();
        assert_eq!(*identity.get_or_insert(document_id), document_id);
        let painted = frame(&mut app, 900.0);
        let id = crate::code_editor_ui::view_id(tab.get());
        assert!(painted.regions.iter().any(|region| region.id == id));
        if old_controller.is_none() {
            let shared = app
                .view
                .code_editor
                .as_ref()
                .unwrap()
                .controller
                .as_ref()
                .unwrap()
                .shared_buffer();
            old_controller = Some(Arc::downgrade(&shared));
        }
        views.push(id);
        // This public viewport operation reports whether the view is still
        // retained; it does not open a tab or create a missing editor state.
        assert_eq!(
            views
                .iter()
                .filter(|id| app.tree.reveal_code_caret(id))
                .count(),
            views.len().min(16),
            "scoped tabs for one document accumulated more than sixteen cached views"
        );
        assert!(old_controller.as_ref().unwrap().upgrade().is_some());
    }
    assert!(views[..16].iter().all(|id| !app.tree.reveal_code_caret(id)));
    assert_eq!(app.dock.tabs(&app.view.workspace).len(), 32);
    let tab = open(&mut app, "main.rs");
    let painted = frame(&mut app, 900.0);
    assert_eq!(
        views.last().unwrap(),
        &crate::code_editor_ui::view_id(tab.get())
    );
    click(
        &mut app,
        &painted,
        &format!("code_editor_reload:{}", tab.get()),
    );
    assert!(app.view.code_editor_reload_confirm.is_none());
    wait(&mut app);
    assert_ne!(snapshot(&app).document_id(), identity.unwrap());
    let _new_document_frame = frame(&mut app, 900.0);
    assert!(old_controller.unwrap().upgrade().is_none());
    for id in &views[..views.len() - 1] {
        assert!(!app.tree.reveal_code_caret(id));
    }
    assert!(app.tree.reveal_code_caret(views.last().unwrap()));
    assert!(!snapshot(&app).is_dirty());
}

#[test]
fn actual_save_marks_every_workspace_chat_without_fabricating_tool_history() {
    const OWNER: &str = "editor-history-active";
    const OTHER: &str = "editor-history-background";
    const ORIGINAL: &str = "fn original() {}\n";
    const MIDDLE: &str = "fn middle() {}\n";
    let (_root, mut app) = fixture();
    let workspace = PathBuf::from(&app.view.workspace);
    let path = workspace.join("main.rs");
    let storage_root = app.store.as_ref().unwrap().storage_root().to_path_buf();
    std::fs::write(&path, ORIGINAL).unwrap();
    let write_journal = |owner: &str, events: &[AgentEvent]| {
        let mut bytes = Vec::new();
        for event in events {
            serde_json::to_writer(&mut bytes, event).unwrap();
            bytes.push(b'\n');
        }
        let path = storage_root.join(format!("{owner}.jsonl"));
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    };
    let mut journals = Vec::new();
    for owner in [OWNER, OTHER] {
        let mut events = vec![AgentEvent::new(
            owner,
            1,
            AgentEventKind::SessionStarted {
                workspace: workspace.clone(),
            },
        )];
        crate::session_changes::capture_baseline(&storage_root, owner, &workspace, &events)
            .unwrap();
        events.push(AgentEvent::new(
            owner,
            2,
            AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: "confirmed-write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path":"main.rs","content":MIDDLE}),
                },
            },
        ));
        events.push(AgentEvent::new(
            owner,
            3,
            AgentEventKind::ToolFinished {
                tool_call_id: "confirmed-write".into(),
                output: "File written".into(),
                is_error: false,
            },
        ));
        let (journal_path, bytes) = write_journal(owner, &events);
        journals.push((owner, events, journal_path, bytes));
    }
    std::fs::write(&path, MIDDLE).unwrap();
    app.view.selected_session = Some(OWNER.into());
    let tab = open(&mut app, "main.rs");
    append(&mut app, "// manual editor change 🌎\n", 1);
    let painted = frame(&mut app, 900.0);
    click(
        &mut app,
        &painted,
        &format!("code_editor_save:{}", tab.get()),
    );
    wait(&mut app);
    assert!(app.view.code_editor.as_ref().unwrap().error.is_none());
    assert!(!snapshot(&app).is_dirty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text(&app));
    assert!(text(&app).contains("manual editor change"));
    for (owner, _, journal_path, original_bytes) in &journals {
        assert_eq!(std::fs::read(journal_path).unwrap(), *original_bytes);
        assert_eq!(
            crate::session_changes_editor::editor_save_boundary(&storage_root, owner, &workspace)
                .unwrap(),
            Some(3),
            "the save must cover both selected and background chats"
        );
        let projection = crate::session_changes::load_stored_tool_diff_projection(
            &storage_root,
            owner,
            &workspace,
            "confirmed-write",
        )
        .unwrap();
        assert!(projection.unavailable.is_none());
        assert_eq!(projection.sequence, 3);
        assert_eq!(projection.files.len(), 1);
        assert!(projection.files[0].unavailable.is_none());
        let lines = &projection.files[0].file.lines;
        assert!(lines.iter().any(|line| {
            line.kind == crate::git_review::ReviewLineKind::Removed
                && line.text == "-fn original() {}"
        }));
        assert!(lines.iter().any(|line| {
            line.kind == crate::git_review::ReviewLineKind::Added && line.text == "+fn middle() {}"
        }));
        assert!(lines
            .iter()
            .all(|line| !line.text.contains("manual editor")));
    }

    let (_, mut events, _, _) = journals.remove(0);
    events.push(AgentEvent::new(
        OWNER,
        4,
        AgentEventKind::ToolStarted {
            tool_call: ToolCall {
                id: "post-save-patch".into(),
                name: "apply_patch".into(),
                arguments: serde_json::json!({"patch":"*** Begin Patch\n*** Update File: main.rs\n@@\n-fn middle() {}\n+fn final_version() {}\n*** End Patch\n"}),
            },
        },
    ));
    events.push(AgentEvent::new(
        OWNER,
        5,
        AgentEventKind::ToolFinished {
            tool_call_id: "post-save-patch".into(),
            output: "Patch applied".into(),
            is_error: false,
        },
    ));
    write_journal(OWNER, &events);
    // The historical projection must not reconstruct the unknown post-save
    // before state from today's file, even if a later tool reported success.
    std::fs::write(&path, "fn final_version() {}\n// manual editor change 🌎\n").unwrap();
    let projection = crate::session_changes::load_stored_tool_diff_projection(
        &storage_root,
        OWNER,
        &workspace,
        "post-save-patch",
    )
    .unwrap();
    assert!(
        projection.unavailable.is_some()
            || projection
                .files
                .iter()
                .any(|file| file.unavailable.is_some()),
        "a patch after a manual save has no verified historical before state"
    );
    assert!(projection
        .files
        .iter()
        .flat_map(|file| &file.file.lines)
        .all(|line| !line.text.contains("manual editor change")));
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn close_guard_uses_viewport_close_event_and_disables_discard_during_pending_save() {
    let (_root, mut app) = fixture();
    let tab = open(&mut app, "main.rs");
    append(&mut app, "// draft\r\n", 1);
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .events
        .push(egui::ViewportEvent::Close);
    let output = ctx.run_ui(input, |ui| app.guard_editor_close(ui.ctx()));
    assert!(app.view.code_editor_quit_confirm);
    assert!(output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .unwrap()
        .commands
        .contains(&egui::ViewportCommand::CancelClose));
    output.drop_without_applying_deltas();
    let modal = frame(&mut app, 900.0);
    click(&mut app, &modal, "code_editor_quit_cancel");
    assert!(!app.view.code_editor_quit_confirm);
    clicked(&mut app, format!("code_editor_save:{}", tab.get()));
    assert!(app.view.code_editor_pending_saves);
    app.view.code_editor_quit_confirm = true;
    let modal = frame(&mut app, 360.0);
    assert!(!modal
        .regions
        .iter()
        .any(|r| r.id == "code_editor_quit_discard" && r.kind == HitKind::Button));
    clicked(&mut app, "code_editor_quit_discard");
    assert!(
        app.view.code_editor_quit_confirm,
        "host also rejects a forged click while saving"
    );
    wait(&mut app);
    assert!(!app.view.code_editor_pending_saves);
    let modal = frame(&mut app, 900.0);
    assert!(modal
        .regions
        .iter()
        .any(|r| r.id == "code_editor_quit_discard" && r.kind == HitKind::Button));
    click(&mut app, &modal, "code_editor_quit_cancel");
    assert!(!app.view.code_editor_quit_confirm);
    assert!(!snapshot(&app).is_dirty());
}

#[test]
#[ignore = "opt-in actual host visual review exports"]
fn export_actual_dock_editor_and_dirty_reload_alert() {
    let directory = std::env::var_os("OPENIDE_EDITOR_HOST_VISUAL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/openide-editor-host-visual-qa"));
    std::fs::create_dir_all(&directory).unwrap();
    let (_root, mut app) = fixture();
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.begin_pass(egui::RawInput::default());
    for family in [FontFamily::Monospace, FontFamily::Sans] {
        let font = match family {
            FontFamily::Monospace => egui::FontId::monospace(14.0),
            FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontId::proportional(14.0),
        };
        let metrics = context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts
                .glyph_width(&font, if family == FontFamily::Sans { 'a' } else { 'M' }),
            line_height: fonts.row_height(&font),
        });
        match family {
            FontFamily::Monospace => app.tree.set_metrics(metrics),
            FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => app.tree.set_sans_metrics(metrics),
        }
    }
    install_text_measurer(&mut app.tree, &context);
    let tab = open(&mut app, "main.rs");
    append(
        &mut app,
        "\r\nfn saludo() {\r\n    println!(\"Hola, mundo 🌎\");\r\n}\r\n",
        1,
    );
    for (phase, width) in [
        ("dock-editor", 900.0),
        ("dirty-reload", 360.0),
        ("dirty-reload", 900.0),
    ] {
        if phase == "dirty-reload" {
            clicked(&mut app, format!("code_editor_reload:{}", tab.get()));
        }
        let mut rendered = frame(&mut app, width);
        let deadline = Instant::now() + Duration::from_secs(1);
        while app.tree.animation_repaint_after().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(16));
            rendered = frame(&mut app, width);
        }
        let path = directory.join(format!("host-{phase}-{width:.0}.svg"));
        crate::settings_ui::visual_qa::write_frame_svg(&rendered, &path, width, 720.0).unwrap();
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        let svg = std::fs::read_to_string(&path).unwrap();
        let tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 720).unwrap();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap.save_png(path.with_extension("png")).unwrap();
    }
    context.end_pass().textures_delta.clear();
}
