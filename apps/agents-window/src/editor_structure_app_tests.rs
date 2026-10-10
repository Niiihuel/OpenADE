use super::*;
use openide_editor::{EditKind, TextEdit};
use openide_language::{Cancellation, Language, ParserEngine};
use openide_ui::{DrawCommand, Frame, HitKind, Point};

fn fixture(source: &str) -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("main.rs"), source).unwrap();
    std::fs::write(workspace.join("other.rs"), source).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("state"));
    app.view.workspace = workspace
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    open(&mut app, "main.rs");
    (root, app)
}

fn open(app: &mut AgentsWindow, path: &str) {
    let scope = app.dock_scope();
    let tab = app
        .dock
        .open(&scope, DockTabKind::FilePreview { path: path.into() });
    app.dock.activate(&scope, tab);
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app
        .view
        .code_editor
        .as_ref()
        .is_none_or(|view| view.loading)
    {
        app.poll_code_editor(&egui::Context::default());
        assert!(
            Instant::now() < deadline,
            "editor file worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    app.poll_editor_workbench(&egui::Context::default());
    analyze(app);
}

fn analyze(app: &mut AgentsWindow) {
    let snapshot = app.active_editor().unwrap().3;
    let analysis = ParserEngine::default()
        .analyze(&snapshot, Language::Rust, &Cancellation::default())
        .unwrap();
    app.view.editor_workbench.analysis = Some(Arc::new(analysis));
    app.sync_editor_structure();
}

fn frame(app: &mut AgentsWindow, height: f32) -> Frame {
    let view = app.view.code_editor.as_ref().unwrap();
    app.tree.set_root(crate::code_editor_ui::build(
        view,
        &app.view.editor_workbench,
        620.0,
        app.view.keymap_profile,
        app.view.vim_mode,
    ));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(620.0, height))
}

fn dispatch(app: &mut AgentsWindow, event: UiEvent) {
    let ctx = egui::Context::default();
    if !app.handle_editor_workbench_key(&event, &ctx) {
        for action in app.tree.handle_event(event) {
            app.apply_action(action, &ctx);
        }
    }
}

fn click(app: &mut AgentsWindow, id: &str, height: f32) {
    let painted = frame(app, height);
    let rect = painted
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing editor control {id}"))
        .rect;
    let position = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert_eq!(painted.hit_test(position), Some(id));
    dispatch(app, UiEvent::PointerDown(position));
    dispatch(app, UiEvent::PointerUp(position));
}

#[test]
fn parsed_gutter_fold_toggles_without_editing_or_losing_directed_selection() {
    let (_root, mut app) =
        fixture("fn café() {\r\n    let α = 1;\r\n    println!(\"{α}\");\r\n}\r\n");
    let (_, _, controller, snapshot) = app.active_editor().unwrap();
    let selection = Selection { anchor: 7, head: 3 };
    controller
        .execute(EditorCommand::SetSelection(selection))
        .unwrap();
    let candidate = app
        .view
        .editor_workbench
        .fold_candidates
        .first()
        .unwrap()
        .clone();
    let cached = app.view.editor_workbench.fold_candidates.clone();
    assert!(
        !app.sync_editor_structure(),
        "unchanged immutable analysis should reuse mapping"
    );
    assert!(Arc::ptr_eq(
        &cached,
        &app.view.editor_workbench.fold_candidates
    ));
    click(&mut app, &candidate.action_id, 420.0);
    assert_eq!(
        &*app.view.editor_workbench.folds,
        std::slice::from_ref(&(candidate.start_row..candidate.end_row))
    );
    let folded = controller.snapshot().unwrap();
    assert_eq!(folded.revision(), snapshot.revision());
    assert_eq!(folded.selection(), selection);
    assert!(!folded.is_dirty());
    click(&mut app, &candidate.action_id, 420.0);
    assert!(app.view.editor_workbench.folds.is_empty());
    assert_eq!(controller.snapshot().unwrap().selection(), selection);
}

#[test]
fn sticky_scope_click_returns_to_the_source_header_without_changing_text() {
    let source = format!(
        "fn outer() {{\n{}\n}}\n",
        (0..60)
            .map(|row| format!("    let value_{row} = {row};"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (_root, mut app) = fixture(&source);
    let (tab, _, controller, snapshot) = app.active_editor().unwrap();
    let scope = app
        .view
        .editor_workbench
        .sticky_scopes
        .iter()
        .find(|scope| scope.start_row == 0)
        .unwrap()
        .clone();
    let painted = frame(&mut app, 290.0);
    let editor_id = crate::code_editor_ui::view_id(tab);
    let rect = painted
        .regions
        .iter()
        .find(|region| region.id == editor_id)
        .unwrap()
        .rect;
    dispatch(
        &mut app,
        UiEvent::ScrollDirect {
            position: Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
            delta_y: -400.0,
        },
    );
    click(&mut app, &scope.action_id, 290.0);
    let after = controller.snapshot().unwrap();
    assert_eq!(after.selection(), Selection::caret(0));
    assert_eq!(after.revision(), snapshot.revision());
    assert_eq!(after.text_range(0..after.char_count()).unwrap(), source);
}

#[test]
fn pending_breakpoints_use_real_f9_and_gutter_and_are_scoped_to_the_document() {
    let (_root, mut app) = fixture("fn one() {\n    let x = 1;\n}\n");
    let (tab, _, controller, snapshot) = app.active_editor().unwrap();
    frame(&mut app, 420.0);
    assert!(app.tree.focus(&crate::code_editor_ui::view_id(tab)));
    dispatch(
        &mut app,
        UiEvent::Key {
            key: Key::F9,
            shift: false,
            ctrl: false,
        },
    );
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[0]);
    let second = format!(
        "editor_breakpoint:{tab}:{}:{}:1",
        snapshot.document_id(),
        snapshot.revision()
    );
    click(&mut app, &second, 420.0);
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[0, 1]);
    assert_eq!(
        controller.snapshot().unwrap().revision(),
        snapshot.revision()
    );
    open(&mut app, "other.rs");
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());
    assert!(app.apply_editor_structure_action(&second, &egui::Context::default()));
    assert!(
        app.view.editor_workbench.breakpoint_rows.is_empty(),
        "old tab must not mark another file"
    );
    open(&mut app, "main.rs");
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[0, 1]);
    click(&mut app, &second, 420.0);
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[0]);
}

#[test]
fn stale_structure_and_markers_are_invalidated_on_edit_and_not_resurrected_by_undo() {
    let (_root, mut app) = fixture("fn café() {\n    let α = 1;\n}\n");
    let (tab, _, controller, snapshot) = app.active_editor().unwrap();
    let old_fold = app
        .view
        .editor_workbench
        .fold_candidates
        .first()
        .unwrap()
        .action_id
        .clone();
    let old_scope = app
        .view
        .editor_workbench
        .sticky_scopes
        .first()
        .unwrap()
        .action_id
        .clone();
    let old_marker = format!(
        "editor_breakpoint:{tab}:{}:{}:1",
        snapshot.document_id(),
        snapshot.revision()
    );
    assert!(app.apply_editor_structure_action(&old_marker, &egui::Context::default()));
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[1]);
    controller
        .shared_buffer()
        .lock()
        .unwrap()
        .apply_transaction(
            &[TextEdit {
                range: 0..0,
                replacement: "// new line\n".into(),
            }],
            Selection::caret(0),
            EditKind::Paste,
            1,
        )
        .unwrap();
    assert!(app.sync_editor_structure());
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());
    assert!(app.view.editor_workbench.fold_candidates.is_empty());
    assert!(app.view.editor_workbench.sticky_scopes.is_empty());
    for stale in [&old_fold, &old_scope, &old_marker] {
        assert!(app.apply_editor_structure_action(stale, &egui::Context::default()));
    }
    assert!(app.view.editor_workbench.folds.is_empty());
    assert_eq!(
        controller.snapshot().unwrap().selection(),
        Selection::caret(0)
    );
    controller.execute(EditorCommand::Undo).unwrap();
    app.sync_editor_structure();
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());
    analyze(&mut app);
    assert!(!app.view.editor_workbench.fold_candidates.is_empty());
    assert!(!app
        .view
        .editor_workbench
        .fold_candidates
        .iter()
        .any(|fold| fold.action_id == old_fold));
}

#[test]
fn forged_ranges_and_other_workspace_actions_do_not_change_editor_state() {
    let (_root, mut app) = fixture("fn one() {\n    let x = 1;\n}\n");
    let (tab, _, _, snapshot) = app.active_editor().unwrap();
    let prefix = format!("{tab}:{}:{}", snapshot.document_id(), snapshot.revision());
    for action in [
        format!("editor_fold_range:{prefix}:0:999999"),
        format!("editor_scope_jump:{prefix}:999999"),
        format!("editor_breakpoint:{prefix}:999999"),
        format!("editor_breakpoint:{prefix}:0:unexpected"),
    ] {
        assert!(app.apply_editor_structure_action(&action, &egui::Context::default()));
    }
    assert!(app.view.editor_workbench.folds.is_empty());
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());
    app.view.workspace.push_str("-other");
    assert!(app.apply_editor_structure_action(
        &format!("editor_breakpoint:{prefix}:0"),
        &egui::Context::default()
    ));
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());
    assert_eq!(
        app.active_editor().unwrap().3.selection(),
        snapshot.selection()
    );
}

#[test]
fn pending_marker_limit_still_allows_removing_and_replacing_an_existing_marker() {
    let (_root, mut app) = fixture(&"// line\n".repeat(MAX_MARKERS_PER_FILE + 2));
    let (tab, _, _, snapshot) = app.active_editor().unwrap();
    let prefix = format!(
        "editor_breakpoint:{tab}:{}:{}",
        snapshot.document_id(),
        snapshot.revision()
    );
    for row in 0..=MAX_MARKERS_PER_FILE {
        assert!(app
            .apply_editor_structure_action(&format!("{prefix}:{row}"), &egui::Context::default()));
    }
    assert_eq!(
        app.view.editor_workbench.breakpoint_rows.len(),
        MAX_MARKERS_PER_FILE
    );
    assert!(!app
        .view
        .editor_workbench
        .breakpoint_rows
        .contains(&MAX_MARKERS_PER_FILE));
    app.apply_editor_structure_action(&format!("{prefix}:0"), &egui::Context::default());
    app.apply_editor_structure_action(
        &format!("{prefix}:{MAX_MARKERS_PER_FILE}"),
        &egui::Context::default(),
    );
    assert_eq!(
        app.view.editor_workbench.breakpoint_rows.len(),
        MAX_MARKERS_PER_FILE
    );
    assert!(!app.view.editor_workbench.breakpoint_rows.contains(&0));
    assert!(app
        .view
        .editor_workbench
        .breakpoint_rows
        .contains(&MAX_MARKERS_PER_FILE));
}

#[test]
fn pending_analysis_keeps_source_and_caret_gutter_width_without_stale_controls() {
    let (_root, mut app) = fixture("fn café() {\n    let α = 1;\n}\n");
    let controller = app.active_editor().unwrap().2;
    let source_x = |frame: &Frame| {
        frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text.starts_with("fn") => {
                    Some(position.x)
                }
                _ => None,
            })
            .expect("visible function source")
    };
    let previous = frame(&mut app, 420.0);
    let before = source_x(&previous);
    let old_actions: Vec<_> = app
        .view
        .editor_workbench
        .fold_candidates
        .iter()
        .map(|fold| fold.action_id.clone())
        .collect();
    controller
        .shared_buffer()
        .lock()
        .unwrap()
        .apply_transaction(
            &[TextEdit {
                range: 0..0,
                replacement: "// new line\n".into(),
            }],
            Selection::caret(0),
            EditKind::Paste,
            1,
        )
        .unwrap();
    app.sync_editor_structure();
    assert!(app.view.editor_workbench.structure_revision.is_none());
    let pending = frame(&mut app, 420.0);
    assert_eq!(source_x(&pending), before);
    assert!(!pending
        .regions
        .iter()
        .any(|region| old_actions.contains(&region.id)));
    analyze(&mut app);
    let ready = frame(&mut app, 420.0);
    assert_eq!(source_x(&ready), before);
    assert!(!ready
        .regions
        .iter()
        .any(|region| old_actions.contains(&region.id)));
    assert!(ready
        .regions
        .iter()
        .any(|region| region.id.starts_with("editor_fold_range:")));
}

#[test]
fn gutter_expands_the_current_block_when_nested_folds_share_its_header_row() {
    let (_root, mut app) = fixture(
        "fn outer() { if true {\n    let inner = 1;\n    println!(\"{inner}\");\n}\nlet tail = 2;\n}\n",
    );
    let controller = app.active_editor().unwrap().2;
    let snapshot = controller.snapshot().unwrap();
    let row = snapshot.line_range(1).unwrap();
    controller
        .execute(EditorCommand::SetSelection(Selection::caret(row.start)))
        .unwrap();
    let candidates: Vec<_> = app
        .view
        .editor_workbench
        .fold_candidates
        .iter()
        .filter(|fold| fold.start_row == 0)
        .cloned()
        .collect();
    let narrow = candidates.iter().min_by_key(|fold| fold.end_row).unwrap();
    let outer = candidates.iter().max_by_key(|fold| fold.end_row).unwrap();
    assert!(
        narrow.end_row < outer.end_row,
        "real parser must report nested blocks sharing a row"
    );
    assert!(app.apply_editor_workbench_action(
        &UiAction::Clicked {
            id: "editor_fold".into()
        },
        &egui::Context::default()
    ));
    assert_eq!(
        &*app.view.editor_workbench.folds,
        std::slice::from_ref(&(0..narrow.end_row))
    );
    click(&mut app, &outer.action_id, 420.0);
    assert!(
        app.view.editor_workbench.folds.is_empty(),
        "the expanded-state glyph must actually expand the visible header"
    );
    assert_eq!(
        controller.snapshot().unwrap().revision(),
        snapshot.revision()
    );
}

#[test]
fn folds_follow_the_text_through_edits_and_are_dropped_with_the_document() {
    let source = "use std::fmt;\n\nfn first() {\n    let a = 1;\n    let b = 2;\n}\n\nfn second() {\n    let c = 3;\n    let d = 4;\n}\n";
    let (_root, mut app) = fixture(source);
    let ctx = egui::Context::default();
    let (_, _, controller, _) = app.active_editor().unwrap();
    let second = app
        .view
        .editor_workbench
        .fold_candidates
        .iter()
        .find(|candidate| candidate.start_row == 7)
        .expect("the second function is foldable")
        .clone();
    click(&mut app, &second.action_id, 420.0);
    assert_eq!(&*app.view.editor_workbench.folds, &[7..second.end_row]);

    // Two lines typed above the folded block: it stays folded, two rows lower.
    controller
        .execute(EditorCommand::SetSelection(Selection::caret(0)))
        .unwrap();
    controller
        .execute(EditorCommand::InsertText("// uno\n// dos\n".into()))
        .unwrap();
    app.poll_editor_workbench(&ctx);
    assert_eq!(&*app.view.editor_workbench.folds, &[9..second.end_row + 2]);

    // The new analysis agrees: that range is still a real block.
    analyze(&mut app);
    assert!(app
        .view
        .editor_workbench
        .fold_candidates
        .iter()
        .any(|candidate| (candidate.start_row, candidate.end_row) == (9, second.end_row + 2)));
    assert_eq!(&*app.view.editor_workbench.folds, &[9..second.end_row + 2]);

    // Retyping the header of the folded block keeps it folded.
    let snapshot = controller.snapshot().unwrap();
    let header = snapshot.line_range(9).unwrap().start;
    controller
        .execute(EditorCommand::SetSelection(Selection {
            anchor: header + 3,
            head: header + 9,
        }))
        .unwrap();
    controller
        .execute(EditorCommand::InsertText("renamed".into()))
        .unwrap();
    app.poll_editor_workbench(&ctx);
    assert_eq!(&*app.view.editor_workbench.folds, &[9..second.end_row + 2]);
    assert!(controller.snapshot().unwrap().line_text(9).unwrap().contains("fn renamed()"));

    // Undoing both edits brings the fold back to where it started.
    controller.execute(EditorCommand::Undo).unwrap();
    app.poll_editor_workbench(&ctx);
    controller.execute(EditorCommand::Undo).unwrap();
    app.poll_editor_workbench(&ctx);
    assert_eq!(controller.snapshot().unwrap().text_range(0..13).unwrap(), "use std::fmt;");
    assert_eq!(&*app.view.editor_workbench.folds, &[7..second.end_row]);

    // Deleting the folded block's closing line drops the fold.
    let snapshot = controller.snapshot().unwrap();
    let closing = snapshot.line_range(second.end_row - 1).unwrap();
    controller
        .execute(EditorCommand::SetSelection(Selection {
            anchor: closing.start,
            head: closing.end,
        }))
        .unwrap();
    controller
        .execute(EditorCommand::InsertText(String::new()))
        .unwrap();
    app.poll_editor_workbench(&ctx);
    assert!(app.view.editor_workbench.folds.is_empty());

    // Folds never travel to another document.
    controller.execute(EditorCommand::Undo).unwrap();
    app.poll_editor_workbench(&ctx);
    analyze(&mut app);
    let again = app
        .view
        .editor_workbench
        .fold_candidates
        .iter()
        .find(|candidate| candidate.start_row == 7)
        .unwrap()
        .clone();
    click(&mut app, &again.action_id, 420.0);
    assert!(!app.view.editor_workbench.folds.is_empty());
    open(&mut app, "other.rs");
    assert!(app.view.editor_workbench.folds.is_empty());
}

#[test]
#[ignore = "prints timings; run with --profile desktop -- --ignored --nocapture"]
fn measure_editor_cost_per_keystroke_by_document_size() {
    for functions in [40usize, 700, 7_000, 20_000] {
        let source: String = (0..functions)
            .map(|index| {
                format!(
                    "/// Item {index}.\nfn item_{index}(input: &str) -> usize {{\n    let total = input.len() + {index};\n    if total > 10 {{\n        return total * 2;\n    }}\n    total\n}}\n\n"
                )
            })
            .collect();
        let lines = source.lines().count();
        let (_root, mut app) = fixture(&source);
        let ctx = egui::Context::default();
        let (_, _, controller, snapshot) = app.active_editor().unwrap();
        let timed = |work: &mut dyn FnMut()| {
            let started = Instant::now();
            work();
            started.elapsed()
        };
        let parse = timed(&mut || {
            ParserEngine::default()
                .analyze(&snapshot, Language::Rust, &Cancellation::default())
                .unwrap();
        });
        analyze(&mut app);
        let analysis = app.view.editor_workbench.analysis.take();
        app.sync_editor_structure();
        app.view.editor_workbench.analysis = analysis;
        let mut structure = timed(&mut || drop(app.sync_editor_structure()));
        let waited = Instant::now();
        while app.view.editor_workbench.structure_revision.is_none() {
            std::thread::sleep(Duration::from_millis(1));
            structure = structure.max(timed(&mut || drop(app.sync_editor_structure())));
        }
        let waited = waited.elapsed();
        let fold = app.view.editor_workbench.fold_candidates[0].clone();
        click(&mut app, &fold.action_id, 420.0);
        let steady = (0..10).map(|_| timed(&mut || drop(frame(&mut app, 420.0)))).min().unwrap();
        let mut report = Vec::new();
        for (name, row) in [("top", 0), ("middle", lines / 2), ("bottom", lines - 1)] {
            let at = controller.snapshot().unwrap().line_range(row).unwrap().start;
            controller
                .execute(EditorCommand::SetSelection(Selection::caret(at)))
                .unwrap();
            let edit = timed(&mut || {
                controller
                    .execute(EditorCommand::InsertText("x".into()))
                    .unwrap();
            });
            let poll = timed(&mut || {
                app.poll_editor_workbench(&ctx);
            });
            let paint = timed(&mut || drop(frame(&mut app, 420.0)));
            report.push(format!("{name}: edit {edit:.2?} poll {poll:.2?} frame {paint:.2?}"));
        }
        println!(
            "{lines:>7} lines | parse {parse:.2?} | structure {structure:.2?} on the interface, ready after {waited:.2?} ({} blocks, {} scopes) | frame {steady:.2?} | {}",
            app.view.editor_workbench.fold_candidates.len(),
            app.view.editor_workbench.sticky_scopes.len(),
            report.join(" | "),
        );
    }
}

#[test]
fn a_large_document_gets_the_same_structure_built_away_from_the_interface() {
    let source: String = (0..1_500)
        .map(|index| format!("fn item_{index}() {{\n    let value = {index};\n    value;\n}}\n\n"))
        .collect();
    let (_root, mut app) = fixture(&source);
    let (tab, _, _, snapshot) = app.active_editor().unwrap();
    let analysis = app.view.editor_workbench.analysis.clone().unwrap();
    assert!(analysis.folds.len() + analysis.symbols.len() > super::INLINE_STRUCTURE_ITEMS);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.view.editor_workbench.structure_revision.is_none() {
        assert!(app.sync_editor_structure(), "a pending build keeps the poll alive");
        assert!(Instant::now() < deadline, "structure was never delivered");
        std::thread::sleep(Duration::from_millis(1));
    }
    let (folds, scopes) = super::build_structure(&analysis, &snapshot, tab);
    let state = &app.view.editor_workbench;
    assert_eq!(state.fold_candidates.len(), 1_500);
    let ids = |folds: &[openide_ui::CodeFoldCandidate]| {
        folds.iter().map(|fold| fold.action_id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&state.fold_candidates), ids(&folds));
    assert_eq!(state.sticky_scopes.len(), scopes.len());
    assert!(!app.sync_editor_structure());

    // An edit while a build is in flight discards it instead of showing stale rows.
    let (_, _, controller, _) = app.active_editor().unwrap();
    controller
        .execute(EditorCommand::InsertText("// x\n".into()))
        .unwrap();
    analyze(&mut app);
    controller
        .execute(EditorCommand::InsertText("// y\n".into()))
        .unwrap();
    app.sync_editor_structure();
    std::thread::sleep(Duration::from_millis(200));
    app.sync_editor_structure();
    assert!(app.view.editor_workbench.fold_candidates.is_empty());
    assert!(app.view.editor_workbench.structure_revision.is_none());
}
