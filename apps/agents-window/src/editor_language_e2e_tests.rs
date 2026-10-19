use super::*;
use crate::editor_workbench_ui::LanguageResult;

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("main.rs"), "use x;\nfo").unwrap();
    std::fs::write(project.join("other.rs"), "other\n").unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.canonicalize().unwrap().display().to_string();
    let scope = app.dock_scope();
    app.dock.open(
        &scope,
        DockTabKind::FilePreview {
            path: "main.rs".into(),
        },
    );
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.active_editor().is_none() {
        app.poll_code_editor(&egui::Context::default());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    (root, app)
}
fn text(controller: &openide_ui::EditorController) -> String {
    let snapshot = controller.snapshot().unwrap();
    snapshot.text_range(0..snapshot.char_count()).unwrap()
}
fn edit(start: usize, end: usize, new_text: &str) -> Value {
    json!({"range":{"start":{"line":0,"character":start},"end":{"line":0,"character":end}},"newText":new_text})
}
fn ready(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&egui::Context::default());
        app.poll_workspace_edit().unwrap();
        assert_eq!(
            app.active_editor().unwrap().1,
            "main.rs",
            "loading edit targets must not select their dock tab"
        );
        if app
            .editor_services
            .language
            .edit
            .as_ref()
            .is_some_and(|edit| edit.ready)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn assert_diagnostic_notice_visible(app: &AgentsWindow, expected: &str) {
    let rows = crate::editor_workbench_ui::tools(
        app.view.code_editor.as_ref().unwrap(),
        &app.view.editor_workbench,
        500.0,
    );
    let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(
        openide_ui::Element::column(rows),
    );
    let frame = tree.layout(openide_ui::Size::new(500.0, 280.0));
    assert!(frame.regions.iter().any(|region| region.id == "editor_results_close"));
    assert!(!frame.regions.iter().any(|region| region.id.starts_with("editor_result:")));
    assert!(frame.commands.iter().any(|command| matches!(command,
        openide_ui::DrawCommand::Text { text, .. } if text.contains(expected)
    )), "missing diagnostic notice {expected}");
    ctx.end_pass().textures_delta.clear();
}

#[test]
fn local_diagnostics_show_empty_and_pending_states_as_dismissible_content() {
    let (_root, mut app) = fixture();
    let (_, _, _, snapshot) = app.active_editor().unwrap();
    app.show_editor_diagnostics();
    assert!(app.view.editor_workbench.result_document.is_none());
    assert_diagnostic_notice_visible(&app, "Todavía no hay diagnósticos");

    app.view.editor_workbench.diagnostic_revision = Some((snapshot.document_id(), snapshot.revision()));
    app.view.editor_workbench.diagnostics_current = true;
    app.show_editor_diagnostics();
    assert_eq!(app.view.editor_workbench.result_document, Some((snapshot.document_id(), snapshot.revision())));
    assert_diagnostic_notice_visible(&app, "No hay problemas");
    app.apply_editor_workbench_action(
        &UiAction::Clicked { id: "editor_results_close".into() },
        &egui::Context::default(),
    );
    assert!(app.view.editor_workbench.hover.is_none());
}

#[test]
fn local_diagnostics_preserve_stale_reports_without_mislabeling_navigation_scope() {
    let (_root, mut app) = fixture();
    let (_, _, controller, snapshot) = app.active_editor().unwrap();
    let identity = (snapshot.document_id(), snapshot.revision());
    app.view.editor_workbench.diagnostics = vec![lsp::Diagnostic {
        range: openide_language::Range::default(),
        severity: Some(1),
        code: None,
        source: Some("fixture".into()),
        message: "Reporte anterior".into(),
    }];
    for (current, revision) in [
        (false, Some(identity)),
        (true, Some((identity.0, identity.1.wrapping_add(1)))),
        (true, Some((identity.0.wrapping_add(1), identity.1))),
        (true, None),
    ] {
        app.view.editor_workbench.diagnostics_current = current;
        app.view.editor_workbench.diagnostic_revision = revision;
        app.show_editor_diagnostics();
        assert!(app.view.editor_workbench.result_document.is_none());
        assert!(app.view.editor_workbench.results.is_empty());
        assert!(app.view.editor_workbench.hover.as_ref().unwrap().contains("Reporte anterior"));
        assert_diagnostic_notice_visible(&app, "Diagnósticos de una versión anterior");
        assert!(app.apply_editor_language_result("0", &egui::Context::default()).is_err());
        assert_eq!(controller.snapshot().unwrap().selection(), snapshot.selection());
        assert!(app.editor_services.pending_navigation.is_none());
    }
}

#[test]
fn local_diagnostics_navigate_current_positions_and_reject_edits_after_opening() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    app.poll_editor_workbench(&ctx);
    let (_, _, controller, snapshot) = app.active_editor().unwrap();
    app.view.editor_workbench.diagnostics_current = true;
    app.view.editor_workbench.diagnostic_revision = Some((snapshot.document_id(), snapshot.revision()));
    app.view.editor_workbench.diagnostics = vec![lsp::Diagnostic {
        range: openide_language::Range {
            start: Position { line: 1, character: 1 },
            end: Position { line: 1, character: 2 },
        },
        severity: Some(1),
        code: None,
        source: None,
        message: "Diagnóstico actual".into(),
    }];
    app.show_editor_diagnostics();
    assert!(app.view.editor_workbench.hover.is_none());
    app.apply_editor_language_result("0", &ctx).unwrap();
    app.poll_editor_workbench(&ctx);
    assert_eq!(controller.snapshot().unwrap().selection(), Selection::caret(8));
    assert!(app.editor_services.pending_navigation.is_none());
    assert!(!app.view.ide.bottom_open);

    app.show_editor_diagnostics();
    controller.execute(EditorCommand::InsertText("change".into())).unwrap();
    let after_edit = controller.snapshot().unwrap();
    assert!(app.apply_editor_language_result("0", &ctx).is_err());
    assert_eq!(controller.snapshot().unwrap().selection(), after_edit.selection());
    assert!(app.editor_services.pending_navigation.is_none());
}

#[test]
fn completion_additional_edit_before_primary_remaps_unicode_caret_and_undo_is_atomic() {
    let (_root, mut app) = fixture();
    let (_, _, controller, snapshot) = app.active_editor().unwrap();
    controller
        .execute(EditorCommand::SetSelection(Selection::caret(
            snapshot.char_count(),
        )))
        .unwrap();
    let item = json!({"label":"foo","textEdit":{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":2}},"newText":"foo"},"additionalTextEdits":[edit(0,0,"// 🧠\n")]});
    app.view.editor_workbench.result_document = Some((snapshot.document_id(), snapshot.revision()));
    app.view.editor_workbench.results = vec![LanguageResult {
        label: "foo".into(),
        detail: String::new(),
        value: json!({"completion":item}),
    }];
    app.apply_editor_language_result("0", &egui::Context::default())
        .unwrap();
    assert_eq!(text(&controller), "// 🧠\nuse x;\nfoo");
    let after = controller.snapshot().unwrap();
    assert_eq!(after.selection().head, after.char_count());
    controller.execute(EditorCommand::Undo).unwrap();
    assert_eq!(text(&controller), "use x;\nfo");
}

#[test]
fn completion_without_server_range_preserves_previous_code_and_handles_unicode_prefixes() {
    for (source, start) in [
        ("let value = ", 12),
        ("foo.nin\u{303}o", 4),
        ("object.$name", 7),
        ("foo.", 4),
    ] {
        let snapshot = openide_editor::TextBuffer::new(source).unwrap().snapshot();
        assert_eq!(
            completion_start(&snapshot, snapshot.char_count()).unwrap(),
            start,
            "{source}"
        );
    }
    let (_root, mut app) = fixture();
    let (_, _, controller, _) = app.active_editor().unwrap();
    let source = "let value = ";
    let old = controller.snapshot().unwrap();
    controller
        .execute(EditorCommand::SetSelection(Selection {
            anchor: 0,
            head: old.char_count(),
        }))
        .unwrap();
    controller
        .execute(EditorCommand::InsertText(source.into()))
        .unwrap();
    let snapshot = controller.snapshot().unwrap();
    app.view.editor_workbench.result_document = Some((snapshot.document_id(), snapshot.revision()));
    app.view.editor_workbench.results = vec![LanguageResult {
        label: "example".into(),
        detail: String::new(),
        value: json!({"completion":{"label":"example"}}),
    }];
    app.apply_editor_language_result("0", &egui::Context::default())
        .unwrap();
    assert_eq!(text(&controller), "let value = example");
}

#[test]
fn workspace_edit_loads_in_background_previews_then_applies_without_saving_disk() {
    let (_root, mut app) = fixture();
    let (_, _, source, snapshot) = app.active_editor().unwrap();
    let workspace = PathBuf::from(&app.view.workspace);
    let source_uri = lsp::file_uri(&workspace.join("main.rs")).unwrap();
    let other_uri = lsp::file_uri(&workspace.join("other.rs")).unwrap();
    app.prepare_workspace_edit(
        json!({"changes":{source_uri:[edit(0,3,"USE")],other_uri:[edit(0,5,"OTHER")]}}),
        &snapshot,
    )
    .unwrap();
    ready(&mut app);
    let other = app.code_editor_controller(Path::new("other.rs")).unwrap();
    assert_eq!(text(&source), "use x;\nfo");
    assert_eq!(text(&other), "other\n");
    app.commit_workspace_edit(&egui::Context::default())
        .unwrap();
    assert_eq!(text(&source), "USE x;\nfo");
    assert_eq!(text(&other), "OTHER\n");
    assert_eq!(
        std::fs::read_to_string(workspace.join("other.rs")).unwrap(),
        "other\n"
    );
    source.execute(EditorCommand::Undo).unwrap();
    other.execute(EditorCommand::Undo).unwrap();
    assert_eq!(text(&source), "use x;\nfo");
    assert_eq!(text(&other), "other\n");
}

#[test]
fn workspace_edit_rejects_lsp_version_mismatch_and_source_change_even_when_source_not_a_target() {
    let (_root, mut app) = fixture();
    let (_, _, source, snapshot) = app.active_editor().unwrap();
    let workspace = PathBuf::from(&app.view.workspace);
    let source_uri = lsp::file_uri(&workspace.join("main.rs")).unwrap();
    app.editor_services.language.opened.insert(
        workspace.join("main.rs"),
        lsp::DocumentVersion {
            uri: source_uri.clone(),
            document_id: snapshot.document_id(),
            revision: snapshot.revision(),
            version: 7,
        },
    );
    assert!(app.prepare_workspace_edit(json!({"documentChanges":[{"textDocument":{"uri":source_uri,"version":6},"edits":[edit(0,3,"USE")]}]}),&snapshot).is_err());
    let other_uri = lsp::file_uri(&workspace.join("other.rs")).unwrap();
    app.prepare_workspace_edit(
        json!({"changes":{other_uri:[edit(0,5,"OTHER")]}}),
        &snapshot,
    )
    .unwrap();
    ready(&mut app);
    source
        .execute(EditorCommand::InsertText("changed ".into()))
        .unwrap();
    assert!(app
        .commit_workspace_edit(&egui::Context::default())
        .is_err());
    assert_eq!(
        text(&app.code_editor_controller(Path::new("other.rs")).unwrap()),
        "other\n"
    );
}

#[test]
fn workspace_edit_rejects_changed_target_before_any_other_target_is_mutated() {
    let (_root, mut app) = fixture();
    let (_, _, source, snapshot) = app.active_editor().unwrap();
    let workspace = PathBuf::from(&app.view.workspace);
    let source_uri = lsp::file_uri(&workspace.join("main.rs")).unwrap();
    let other_uri = lsp::file_uri(&workspace.join("other.rs")).unwrap();
    app.prepare_workspace_edit(
        json!({"changes":{source_uri:[edit(0,3,"USE")],other_uri:[edit(0,5,"OTHER")]}}),
        &snapshot,
    )
    .unwrap();
    ready(&mut app);
    let other = app.code_editor_controller(Path::new("other.rs")).unwrap();
    other
        .execute(EditorCommand::InsertText("mine ".into()))
        .unwrap();
    assert!(app
        .commit_workspace_edit(&egui::Context::default())
        .is_err());
    assert_eq!(text(&source), "use x;\nfo");
    assert!(text(&other).contains("mine"));
}

#[test]
fn explicit_server_restart_scopes_configuration_and_does_not_spawn_on_the_action_frame() {
    let (_root, mut app) = fixture();
    let root = PathBuf::from(&app.view.workspace);
    app.restart_editor_language("/path with spaces/custom-language-server")
        .unwrap();
    assert_eq!(
        app.editor_services
            .language
            .overrides
            .get(&(root, Language::Rust))
            .unwrap(),
        Path::new("/path with spaces/custom-language-server")
    );
    assert!(app.editor_services.language.startup.is_none());
    assert!(app.editor_services.language.server.is_none());
    assert!(app.restart_editor_language("bad\npath").is_err());
    app.restart_editor_language("").unwrap();
    assert!(app.editor_services.language.overrides.is_empty());
}

#[test]
fn hover_and_diagnostic_display_budgets_are_global_and_keep_utf8_boundaries() {
    let value = json!([{"language":"rust","value":"🧠".repeat(4000)},{"value":"界".repeat(4000)}]);
    let hover = excerpt(&value);
    assert!(hover.ends_with(TRUNCATED_TEXT));
    assert!(hover.chars().count() <= 6000 + TRUNCATED_TEXT.chars().count());
    assert!(hover.contains('🧠') && hover.contains('界'));
    let diagnostic = bounded_text(&"á".repeat(10_000), 4096);
    assert!(diagnostic.ends_with(TRUNCATED_TEXT));
    assert_eq!(
        diagnostic.chars().count(),
        4096 + TRUNCATED_TEXT.chars().count()
    );
    assert_eq!(bounded_text("short message", 4096), "short message");
}

/// A language server that answers the handshake and writes down, beside
/// itself, each document notification it receives.
#[cfg(unix)]
const RECORDING_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
def send(value):
    data = json.dumps(dict(jsonrpc="2.0", **value)).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
    sys.stdout.buffer.flush()
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    return json.loads(sys.stdin.buffer.read(length))
while (msg := read()) is not None:
    method = msg.get("method", "")
    if method == "initialize":
        send({"id": msg["id"], "result": {"capabilities": {
            "positionEncoding": "utf-16", "textDocumentSync": 2}}})
    elif method.startswith("textDocument/did"):
        document = msg["params"]["textDocument"]
        with open(sys.argv[0] + ".log", "a") as log:
            log.write(" ".join([method[13:], document["uri"].rsplit("/", 1)[1],
                                document.get("text", "").replace("\n", "|")]).strip() + "\n")
    elif "id" in msg and method:
        send({"id": msg["id"], "result": None})
"#;

#[cfg(unix)]
#[test]
fn renaming_the_open_file_closes_its_old_uri_and_opens_the_new_one_with_its_text() {
    use std::os::unix::fs::PermissionsExt;
    let (root, mut app) = fixture();
    let server = root.path().join("recording-server");
    std::fs::write(&server, RECORDING_SERVER).unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scope = (PathBuf::from(&app.view.workspace), Language::Rust);
    app.editor_services.language.overrides.insert(scope, server.clone());
    app.refresh_file_rows();
    let ctx = egui::Context::default();
    let log = server.with_extension("log");
    let received = |app: &mut AgentsWindow, count: usize| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            app.poll_file_op(&ctx);
            app.poll_code_editor(&ctx);
            app.poll_editor_language(&ctx);
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            if lines.len() >= count && app.file_op.is_none() {
                return lines;
            }
            assert!(
                Instant::now() < deadline,
                "{lines:?} · {}",
                app.view.editor_workbench.language_status
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    assert_eq!(received(&mut app, 1), ["didOpen main.rs use x;|fo"]);
    for action in [
        UiAction::Clicked {
            id: crate::files_panel_ui::tree_action("rename", "main.rs"),
        },
        UiAction::Submitted {
            id: "file_tree_draft".into(),
            value: "app.rs".into(),
        },
    ] {
        assert!(app.apply_file_tree_action(&action, &ctx));
    }
    assert_eq!(
        received(&mut app, 3),
        [
            "didOpen main.rs use x;|fo",
            "didClose main.rs",
            "didOpen app.rs use x;|fo"
        ]
    );
    assert!(app.view.editor_workbench.language_ready, "the same connection");
}

fn pump_real(
    app: &mut AgentsWindow,
    ctx: &egui::Context,
    predicate: impl Fn(&AgentsWindow) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        app.poll_code_editor(ctx);
        app.poll_editor_workbench(ctx);
        if predicate(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real language timeout: status={} error={:?} notice={:?}",
            app.view.editor_workbench.language_status,
            app.view.editor_workbench.error,
            app.view.editor_workbench.notice
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn editor_key(
    app: &mut AgentsWindow,
    ctx: &egui::Context,
    key: openide_ui::Key,
    shift: bool,
    ctrl: bool,
) {
    let (tab, _, _, _) = app.active_editor().unwrap();
    app.tree.set_root(crate::code_editor_ui::build(
        app.view.code_editor.as_ref().unwrap(),
        &app.view.editor_workbench,
        800.0,
        app.view.keymap_profile,
        app.view.vim_mode,
    ));
    app.tree.layout(openide_ui::Size::new(800.0, 500.0));
    assert!(app.tree.focus(&crate::code_editor_ui::view_id(tab)));
    assert!(app.handle_editor_workbench_key(&openide_ui::UiEvent::Key { key, shift, ctrl }, ctx));
}
fn click(app: &mut AgentsWindow, ctx: &egui::Context, id: &str) {
    app.apply_action(UiAction::Clicked { id: id.into() }, ctx);
}
fn real_hover_after_indexing(app: &mut AgentsWindow, ctx: &egui::Context, symbol: &str) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        click(app, ctx, "editor_hover");
        pump_real(app, ctx, |app| {
            app.editor_services.language.requests.is_empty()
        });
        if app
            .view
            .editor_workbench
            .hover
            .as_ref()
            .is_some_and(|text| text.contains(symbol))
        {
            return;
        }
        // initialize acknowledges protocol readiness, not completion of Cargo/project
        // indexing. rust-analyzer can legitimately return null during this interval.
        assert!(
            Instant::now() < deadline,
            "rust-analyzer did not index {symbol}: hover={:?}, error={:?}, status={}",
            app.view.editor_workbench.hover,
            app.view.editor_workbench.error,
            app.view.editor_workbench.language_status
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
#[ignore = "requires OPENIDE_RUST_ANALYZER; runs a real project through native host actions, previews and saving"]
fn real_rust_analyzer_host_toolbar_keyboard_navigation_rename_format_and_save() {
    let executable = std::env::var("OPENIDE_RUST_ANALYZER")
        .expect("set OPENIDE_RUST_ANALYZER to a working binary");
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("Cargo.toml"),"[package]\nname=\"openide_language_e2e\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[lib]\npath=\"main.rs\"\n").unwrap();
    let original = "fn greet()->i32{42}\nfn main(){let value=greet();println!(\"{}\",value);}\n";
    std::fs::write(project.join("main.rs"), original).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let ctx = egui::Context::default();
    app.view.workspace = project.canonicalize().unwrap().display().to_string();
    let scope = app.dock_scope();
    app.dock.open(
        &scope,
        DockTabKind::FilePreview {
            path: "main.rs".into(),
        },
    );
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    pump_real(&mut app, &ctx, |app| app.active_editor().is_some());
    app.apply_action(
        UiAction::InputChanged {
            id: "editor_server_path".into(),
            value: executable,
        },
        &ctx,
    );
    click(&mut app, &ctx, "editor_server_connect");
    pump_real(&mut app, &ctx, |app| {
        app.view.editor_workbench.language_ready
    });
    let (_, _, controller, _) = app.active_editor().unwrap();
    let call = original.rfind("greet").unwrap();
    app.editor_command(
        EditorCommand::SetSelection(Selection::caret(call + 2)),
        &ctx,
    )
    .unwrap();
    real_hover_after_indexing(&mut app, &ctx, "greet");
    assert!(app
        .view
        .editor_workbench
        .hover
        .as_ref()
        .unwrap()
        .contains("greet"));

    editor_key(&mut app, &ctx, openide_ui::Key::F12, false, false);
    pump_real(&mut app, &ctx, |app| {
        !app.view.editor_workbench.results.is_empty()
            && app.editor_services.language.requests.is_empty()
    });
    assert_eq!(app.view.editor_workbench.results_title, "Definiciones");
    click(&mut app, &ctx, "editor_result:0");
    pump_real(&mut app, &ctx, |app| {
        app.active_editor()
            .is_some_and(|(_, _, _, snapshot)| snapshot.selection().head == 3)
    });
    editor_key(&mut app, &ctx, openide_ui::Key::F12, true, false);
    pump_real(&mut app, &ctx, |app| {
        app.view.editor_workbench.results_title == "Referencias"
            && app.view.editor_workbench.results.len() >= 2
            && app.editor_services.language.requests.is_empty()
    });

    app.editor_command(
        EditorCommand::SetSelection(Selection::caret(call + 3)),
        &ctx,
    )
    .unwrap();
    editor_key(&mut app, &ctx, openide_ui::Key::Space, false, true);
    pump_real(&mut app, &ctx, |app| {
        app.view.editor_workbench.results_title == "Completar código"
            && app
                .view
                .editor_workbench
                .results
                .iter()
                .any(|item| item.label.contains("greet"))
            && app.editor_services.language.requests.is_empty()
    });
    click(&mut app, &ctx, "editor_results_close");
    app.editor_command(EditorCommand::SetSelection(Selection::caret(4)), &ctx)
        .unwrap();
    editor_key(&mut app, &ctx, openide_ui::Key::F2, false, false);
    app.apply_action(
        UiAction::InputChanged {
            id: "editor_form_value".into(),
            value: "saludar".into(),
        },
        &ctx,
    );
    click(&mut app, &ctx, "editor_form_apply");
    pump_real(&mut app, &ctx, |app| {
        app.editor_services
            .language
            .edit
            .as_ref()
            .is_some_and(|edit| edit.ready)
    });
    assert_eq!(
        text(&controller),
        original,
        "rename is a preview until Apply"
    );
    click(&mut app, &ctx, "editor_result:0");
    assert_eq!(text(&controller).matches("saludar").count(), 2);
    assert!(!text(&controller).contains("greet"));
    assert_eq!(
        std::fs::read_to_string(project.join("main.rs")).unwrap(),
        original,
        "LSP edits never save implicitly"
    );

    click(&mut app, &ctx, "editor_format");
    pump_real(&mut app, &ctx, |app| {
        app.editor_services
            .language
            .edit
            .as_ref()
            .is_some_and(|edit| edit.ready)
    });
    click(&mut app, &ctx, "editor_result:0");
    assert!(text(&controller).contains("fn saludar() -> i32 {"));
    // Ctrl+S is translated by the real editor component and routed through the host action.
    let (tab, _, _, _) = app.active_editor().unwrap();
    app.tree.set_root(crate::code_editor_ui::build(
        app.view.code_editor.as_ref().unwrap(),
        &app.view.editor_workbench,
        800.0,
        app.view.keymap_profile,
        app.view.vim_mode,
    ));
    app.tree.layout(openide_ui::Size::new(800.0, 500.0));
    assert!(app.tree.focus(&crate::code_editor_ui::view_id(tab)));
    for action in app.tree.handle_event(openide_ui::UiEvent::Key {
        key: openide_ui::Key::S,
        shift: false,
        ctrl: true,
    }) {
        app.apply_action(action, &ctx);
    }
    pump_real(&mut app, &ctx, |app| {
        app.active_editor()
            .is_some_and(|(_, _, _, snapshot)| !snapshot.is_dirty())
    });
    assert_eq!(
        std::fs::read_to_string(project.join("main.rs")).unwrap(),
        text(&controller)
    );
    assert!(
        app.view.editor_workbench.error.is_none(),
        "{:?}",
        app.view.editor_workbench.error
    );
}
