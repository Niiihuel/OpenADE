//! Paths resolve consistently through actual file, editor, diff and chat UI.
use crate::{code_editor_ui, file_activity_ui, git_review, search_activity_ui, transcript, view};
use openide_ui::{DrawCommand, Element, Frame, HitKind, Icon, Size, UiTree};

fn state(path: &str, kind: view::SideTab) -> view::ViewState {
    let mut state = view::tests::empty_state();
    state.workspace = "/fixture/project".into();
    state.selected_session = Some("file-icons-owner".into());
    state.sidebar_collapsed = true;
    state.show_side_pane = true;
    state.right_dock_width = Some(440.0);
    state.side_tab = kind;
    state.dock_tabs = vec![view::DockTabView {
        id: 61,
        title: std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        full_path: Some(path.into()),
        active: true,
        kind,
        closable: true,
    }];
    state.file_rows = vec![view::FileRowView {
        name: path.rsplit('/').next().unwrap().into(),
        depth: 0,
        is_dir: false,
        expanded: false,
        can_load_more: false,
    }];
    state.git_review = Some(Ok(std::sync::Arc::new(git_review::GitReview {
        untracked: vec![path.into()],
        ..Default::default()
    })));
    state.code_editor = Some(code_editor_ui::CodeEditorView {
        tab_id: 61,
        path: path.into(),
        controller: Some(openide_ui::EditorController::new(
            openide_editor::TextBuffer::new("// captured example\n").unwrap(),
        )),
        loading: false,
        saving: false,
        error: None,
        notice: None,
        conflict: false,
        recovered: false,
        missing: false,
    });
    state
}

fn layout(element: Element, width: f32, height: f32) -> Frame {
    let mut tree = UiTree::new(element);
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, height))
}

fn assert_icon(frame: &Frame, expected: Icon, action: Option<&str>, surface: &str) {
    let target = action.map(|id| {
        frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .unwrap_or_else(|| panic!("{surface}: missing {id}"))
            .rect
    });
    assert!(
        frame.commands.iter().any(|command| match command {
            DrawCommand::Icon { icon, rect, .. } if *icon == expected => {
                assert!(
                    rect.width > 0.0 && rect.width <= 16.01,
                    "{surface}: {rect:?}"
                );
                target.is_none_or(|target| {
                    target.contains(openide_ui::Point::new(
                        rect.x + rect.width / 2.0,
                        rect.y + rect.height / 2.0,
                    ))
                })
            }
            _ => false,
        }),
        "{surface}: expected {expected:?} inside {action:?}"
    );
}

fn activity(path: &str, state: &view::ViewState) -> Element {
    let item = transcript::TranscriptItem::Tool {
        id: "read-file".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": path}),
        output: Some("// captured example\n".into()),
        is_error: false,
        activity: transcript::ActivityStatus::Completed,
    };
    file_activity_ui::build(&item, state, false, None, None).unwrap()
}

#[test]
fn the_same_path_uses_identical_artwork_in_files_editor_chat_git_search_and_dock() {
    for path in [
        "src/main.rs",
        "client/index.tsx",
        "package.json",
        "notes.unknown",
    ] {
        let expected = openide_ui::file_icon(path);
        for (kind, action, surface) in [
            (view::SideTab::Files, "file_row:0", "file explorer"),
            (
                view::SideTab::CodeReview,
                "review_untracked_open:0",
                "Git change row",
            ),
            (view::SideTab::FilePreview, "dock_tab:61", "file tab"),
            (
                view::SideTab::CodeSnapshot,
                "dock_tab:61",
                "captured file tab",
            ),
        ] {
            let state = state(path, kind);
            let frame = layout(view::build_view(&state, 1280.0), 1280.0, 800.0);
            assert_icon(&frame, expected, Some(action), surface);
        }
        let state = state(path, view::SideTab::FilePreview);
        for width in [320.0, 900.0] {
            let frame = layout(
                code_editor_ui::build(
                    state.code_editor.as_ref().unwrap(),
                    &state.editor_workbench,
                    width,
                    state.keymap_profile,
                    state.vim_mode,
                ),
                width,
                500.0,
            );
            assert_icon(&frame, expected, None, "editor breadcrumb");
            let frame = layout(activity(path, &state), width, 450.0);
            assert_icon(&frame, expected, None, "chat file activity");
            let open = file_activity_ui::open_action("file-icons-owner", "read-file", 0);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == open && region.kind == HitKind::Button));
        }
        let result = search_activity_ui::SearchResult {
            title: path.into(),
            target: path.into(),
            detail: String::new(),
            web: false,
            directory: false,
        };
        let frame = layout(
            search_activity_ui::result_row("owner", "search", 0, &result, false),
            320.0,
            80.0,
        );
        assert_icon(
            &frame,
            expected,
            Some(&search_activity_ui::result_action(
                "owner", "search", 0, false,
            )),
            "search result",
        );
        assert_eq!(
            crate::composer_attachments::fallback_visual(path, false).icon,
            expected
        );
    }
}

#[test]
fn folder_expansion_uses_the_paired_catalog_artwork_and_preserves_tree_actions() {
    for expanded in [false, true] {
        let mut state = state("src/main.rs", view::SideTab::Files);
        state.file_rows[0].name = "src".into();
        state.file_rows[0].is_dir = true;
        state.file_rows[0].expanded = expanded;
        let frame = layout(view::build_view(&state, 1280.0), 1280.0, 800.0);
        assert_icon(
            &frame,
            openide_ui::folder_icon("src", expanded),
            Some("file_row:0"),
            "folder row",
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Icon { icon, .. } if *icon == if expanded {Icon::ChevronDown} else {Icon::ChevronRight})));
    }
}

#[test]
#[ignore = "opt-in native-font visual review of shared file icon consumers"]
fn export_file_icon_surface_visual_review() {
    let output = std::path::Path::new("/tmp/openade-file-icon-review");
    std::fs::create_dir_all(output).unwrap();
    let context = eframe::egui::Context::default();
    crate::fonts::install(&context);
    context.begin_pass(Default::default());
    for (name, kind) in [
        ("files", view::SideTab::Files),
        ("git", view::SideTab::CodeReview),
        ("editor", view::SideTab::FilePreview),
    ] {
        let state = state("src/main.rs", kind);
        let mut tree = UiTree::new(view::build_view(&state, 1280.0));
        tree.set_reduced_motion(true);
        let native = context.clone();
        tree.set_text_measurer(move |text, size, family| {
            native.fonts_mut(|fonts| {
                let font = match family {
                    openide_ui::FontFamily::Sans | openide_ui::FontFamily::SansItalic => eframe::egui::FontId::proportional(size),
                    openide_ui::FontFamily::Monospace => eframe::egui::FontId::monospace(size),
                };
                fonts
                    .layout_no_wrap(text.into(), font, eframe::egui::Color32::WHITE)
                    .size()
                    .x
            })
        });
        let frame = tree.layout(Size::new(1280.0, 800.0));
        assert_icon(&frame, openide_ui::file_icon("src/main.rs"), None, name);
        let path = output.join(format!("file-icons-{name}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, 1280.0, 800.0)
            .unwrap();
        let svg = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&path).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut image = resvg::tiny_skia::Pixmap::new(1280, 800).unwrap();
        resvg::render(
            &svg,
            resvg::tiny_skia::Transform::identity(),
            &mut image.as_mut(),
        );
        image.save_png(path.with_extension("png")).unwrap();
    }
    let mut output = context.end_pass();
    output.textures_delta.clear();
}
