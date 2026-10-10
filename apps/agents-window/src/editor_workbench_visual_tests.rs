//! Real-font layout checks and opt-in inspection of the native editor tools.
use super::*;
use crate::code_editor_ui::CodeEditorView;
use crate::editor_workbench_ui::{EditorForm, EditorWorkbenchView};
use crate::theme;
use openide_editor::{SearchOptions, Selection, TextBuffer};
use openide_language::{Cancellation, Language, ParserEngine};
use openide_ui::{Color, DrawCommand, Frame, Point, Rect};

const SOURCE: &str = "use std::path::Path;\n\n/// Comandos del proyecto.\npub struct CommandRegistry {\n    commands: Vec<String>,\n}\n\nimpl CommandRegistry {\n    pub fn new() -> Self {\n        Self { commands: Vec::new() }\n    }\n\n    pub fn register(&mut self, command: &str) {\n        self.commands.push(command.to_owned());\n    }\n\n    pub fn search(&self, query: &str) -> Vec<&str> {\n        self.commands.iter()\n            .filter(|command| command.contains(query))\n            .map(String::as_str)\n            .collect()\n    }\n}\n\nfn workspace_name(path: &Path) -> String {\n    format!(\"Proyecto: {} 🦀\", path.display())\n}\n";

fn fixture() -> (CodeEditorView, EditorWorkbenchView) {
    let controller = openide_ui::EditorController::new(TextBuffer::new(SOURCE).unwrap());
    let snapshot = controller.snapshot().unwrap();
    let analysis = ParserEngine::default()
        .analyze(&snapshot, Language::Rust, &Cancellation::default())
        .unwrap();
    let highlights = analysis
        .highlights
        .iter()
        .map(|span| openide_ui::CodeHighlight {
            range: span.range.clone(),
            color: match span.kind {
                openide_language::HighlightKind::Comment => Color::rgb(126, 151, 119),
                openide_language::HighlightKind::String => Color::rgb(206, 163, 127),
                openide_language::HighlightKind::Keyword => Color::rgb(184, 143, 216),
                openide_language::HighlightKind::Number => Color::rgb(151, 193, 169),
                openide_language::HighlightKind::Type => Color::rgb(225, 196, 131),
                openide_language::HighlightKind::Function => Color::rgb(143, 185, 218),
                _ => theme::FOREGROUND,
            },
        })
        .collect::<Vec<_>>()
        .into();
    let state = EditorWorkbenchView {
        document_id: Some(snapshot.document_id()),
        analysis: Some(Arc::new(analysis)),
        highlight_spans: highlights,
        language_status: "Rust · análisis local".into(),
        ..Default::default()
    };
    (
        CodeEditorView {
            tab_id: 71,
            path: "apps/agents-window/src/commands.rs".into(),
            controller: Some(controller),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            conflict: false,
            recovered: false,
            missing: false,
        },
        state,
    )
}

fn native_tree(root: Element) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.set_pixels_per_point(2.0);
    context.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    tree.set_context_menu_palette(theme::menu_palette());
    for family in [FontFamily::Monospace, FontFamily::Sans] {
        let font = match family {
            FontFamily::Monospace => egui::FontId::monospace(14.0),
            FontFamily::Sans => egui::FontId::proportional(14.0),
        };
        let metrics = context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts
                .glyph_width(&font, if family == FontFamily::Sans { 'a' } else { 'M' }),
            line_height: fonts.row_height(&font),
        });
        match family {
            FontFamily::Monospace => tree.set_metrics(metrics),
            FontFamily::Sans => tree.set_sans_metrics(metrics),
        }
    }
    install_text_measurer(&mut tree, &context);
    (context, tree)
}

fn populate_search(view: &CodeEditorView, state: &mut EditorWorkbenchView) {
    state.query = "commands".into();
    state.replacement = "entries".into();
    let controller = view.controller.as_ref().unwrap();
    let results = controller
        .snapshot()
        .unwrap()
        .search(&state.query, &SearchOptions::default())
        .unwrap();
    let first = &results.matches[0];
    controller
        .shared_buffer()
        .lock()
        .unwrap()
        .set_selection(Selection {
            anchor: first.start,
            head: first.end,
        })
        .unwrap();
    state.search = Some(results);
}

fn region_rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing visible control: {id}"))
        .rect
}

#[test]
fn editor_actions_match_the_right_click_menu_with_native_fonts() {
    let text_style = |frame: &Frame, label: &str| {
        frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text {
                    text,
                    position,
                    font_size,
                    font_family,
                    color,
                } if text == label => Some((*position, *font_size, *font_family, *color)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing complete menu label: {label}"))
    };
    let surface_style = |frame: &Frame, surface: Rect| {
        let fill = frame.commands.iter().find_map(|command| match command {
            DrawCommand::FillRect {
                rect,
                color,
                radius,
            } if *rect == surface => Some((*color, *radius)),
            _ => None,
        });
        let border = frame.commands.iter().find_map(|command| match command {
            DrawCommand::StrokeRect {
                rect,
                color,
                width,
                radius,
            } if *rect == surface => Some((*color, *width, *radius)),
            _ => None,
        });
        let shadow = frame.commands.iter().find_map(|command| match command {
            DrawCommand::Shadow {
                rect,
                shadow,
                radius,
            } if *rect == surface => Some((*shadow, *radius)),
            _ => None,
        });
        (fill.unwrap(), border.unwrap(), shadow.unwrap())
    };
    for width in [320.0, 520.0, 900.0] {
        let (view, mut state) = fixture();
        view.controller
            .as_ref()
            .unwrap()
            .shared_buffer()
            .lock()
            .unwrap()
            .insert("// Sin guardar\n", openide_editor::EditKind::Paste, 1)
            .unwrap();
        state.menu_open = true;
        let size = Size::new(width, 800.0);
        let (actions_context, mut actions_tree) = native_tree(crate::code_editor_ui::build(
            &view,
            &state,
            width,
            Default::default(),
            false,
        ));
        let actions = actions_tree.layout(size);
        let actions_surface = region_rect(&actions, "popover_surface:editor_menu_close");
        let save = region_rect(&actions, "code_editor_save:71");
        let reload = region_rect(&actions, "code_editor_reload:71");
        assert_eq!(save.height, 26.0);
        assert_eq!(reload.y, save.y + save.height, "menu rows must have no gap");

        state.menu_open = false;
        let (context, mut tree) = native_tree(crate::code_editor_ui::build(
            &view,
            &state,
            width,
            Default::default(),
            false,
        ));
        tree.layout(size);
        assert!(tree.focus("code_editor:71"));
        tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::A,
            ctrl: true,
            shift: false,
        });
        tree.handle_event(UiEvent::ContextMenuKeyboard);
        let contextual = tree.layout(size);
        let context_surface = region_rect(&contextual, "__context_menu:surface");
        let cut = region_rect(&contextual, "__context_menu:item:0");
        assert_eq!(actions_surface.width, context_surface.width);
        assert_eq!(save.height, cut.height);
        let standard = surface_style(&contextual, context_surface);
        assert_eq!(surface_style(&actions, actions_surface), standard);
        // Both menus use the shared compact floating-surface radius (6px).
        assert_eq!(standard.0, (theme::menu_palette().surface, 6.0));
        assert_eq!(standard.1, (theme::menu_palette().border, 1.0, 6.0));
        assert_eq!(standard.2 .0.offset, Point::new(0.0, 6.0));
        assert_eq!(standard.2 .0.blur, 18.0);

        let (save_position, save_size, save_family, save_ink) = text_style(&actions, "Guardar");
        let (cut_position, cut_size, cut_family, cut_ink) = text_style(&contextual, "Cortar");
        assert_eq!(
            (save_size, save_family, save_ink),
            (cut_size, cut_family, cut_ink)
        );
        assert_eq!((save_size, save_family), (13.0, FontFamily::Sans));
        // The actions menu reserves a leading check column for wrap/minimap;
        // the plain text context menu has no leading adornments.
        assert_eq!(save_position.x - actions_surface.x, 36.0);
        assert_eq!(cut_position.x - context_surface.x, 12.0);
        assert!(
            ((save_position.y - save.y) - (cut_position.y - cut.y)).abs() < 0.01,
            "action labels must share the contextual vertical alignment"
        );
        let (_, save_size, save_family, save_ink) = text_style(&actions, "S");
        let (_, cut_size, cut_family, cut_ink) = text_style(&contextual, "X");
        assert_eq!(
            (save_size, save_family, save_ink),
            (cut_size, cut_family, cut_ink)
        );
        assert_eq!((save_size, save_ink), (11.0, theme::menu_palette().text));
        for (frame, row) in [(&actions, save), (&contextual, cut)] {
            let caps: Vec<_> = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect { rect, radius, .. }
                        if *radius == 3.0
                            && rect.height == 18.0
                            && rect.y >= row.y
                            && rect.y + rect.height <= row.y + row.height =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                caps.len(),
                2,
                "both menus use the shared complete Ctrl keycap pair"
            );
            assert!(caps
                .iter()
                .all(|cap| cap.x >= row.x && cap.x + cap.width <= row.x + row.width));
        }

        actions_tree.handle_event(UiEvent::PointerMove(Point::new(
            save.x + 12.0,
            save.y + 16.0,
        )));
        tree.handle_event(UiEvent::PointerMove(Point::new(cut.x + 12.0, cut.y + 16.0)));
        let actions = actions_tree.layout(size);
        let contextual = tree.layout(size);
        let hover = |frame: &Frame, row: Rect| {
            frame.commands.iter().find_map(|command| match command {
                DrawCommand::FillRect {
                    rect,
                    color,
                    radius,
                } if *rect == row => Some((*color, *radius)),
                _ => None,
            })
        };
        assert_eq!(hover(&actions, save), hover(&contextual, cut));
        assert_eq!(
            hover(&actions, save).unwrap().0,
            theme::menu_palette().hover
        );
        actions_context.end_pass().textures_delta.clear();
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn search_floats_inside_each_editor_pane_without_moving_code_or_obscuring_controls() {
    const SURFACE: &str = "popover_surface:editor_search_close";
    for width in [240.0, 320.0, 430.0, 520.0, 900.0] {
        for left in [0.0, 311.0] {
            let (view, mut state) = fixture();
            populate_search(&view, &mut state);
            let root = |state: &EditorWorkbenchView| {
                Element::row(vec![
                    Element::spacer().size(Some(left), None),
                    crate::code_editor_ui::build(&view, state, width, Default::default(), false),
                ])
            };
            let size = Size::new(left + width, 800.0);
            let (context, mut tree) = native_tree(root(&state));
            let initial = tree.layout(size);
            let editor = region_rect(&initial, "code_editor:71");
            assert_eq!(editor.x, left);
            assert!(!initial.regions.iter().any(|region| region.id == SURFACE));

            for (search_open, replace_open) in
                [(true, false), (true, true), (true, false), (false, false)]
            {
                state.search_open = search_open;
                state.replace_open = replace_open;
                tree.set_root(root(&state));
                let frame = tree.layout(size);
                assert_eq!(
                    region_rect(&frame, "code_editor:71"),
                    editor,
                    "opening or closing search/replace moved the editor: {width}, {left}"
                );
                if !search_open {
                    for id in [SURFACE, "editor_query", "editor_replacement"] {
                        assert!(!frame.regions.iter().any(|region| region.id == id));
                    }
                    continue;
                }

                let surface = region_rect(&frame, SURFACE);
                assert!(
                    surface.x >= left && surface.x + surface.width <= left + width,
                    "search escaped its own editor pane: {width}, {left}: {surface:?}"
                );
                assert!(surface.y >= 0.0 && surface.y + surface.height <= size.height);
                let mut controls = vec![
                    "editor_replace_toggle",
                    "editor_query",
                    "editor_case",
                    "editor_word",
                    "editor_regex",
                    "editor_scope",
                    "editor_search_count",
                    "editor_search_prev",
                    "editor_search_next",
                    "editor_search_close",
                ];
                if replace_open {
                    controls.extend([
                        "editor_replacement",
                        "editor_replace_one",
                        "editor_replace_all",
                    ]);
                } else {
                    assert!(!frame
                        .regions
                        .iter()
                        .any(|region| region.id == "editor_replacement"));
                }
                for (index, id) in controls.iter().enumerate() {
                    let rect = region_rect(&frame, id);
                    assert!(rect.width > 0.0 && rect.height > 0.0, "hidden {id}");
                    assert!(
                        rect.x >= surface.x - 0.01
                            && rect.y >= surface.y - 0.01
                            && rect.x + rect.width <= surface.x + surface.width + 0.01
                            && rect.y + rect.height <= surface.y + surface.height + 0.01,
                        "control outside search surface: {width}, {left}: {id}: {rect:?}"
                    );
                    let center = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                    assert_eq!(
                        frame.hit_test(center),
                        Some(*id),
                        "covered control: {width}, {left}: {id}"
                    );
                    for other in &controls[..index] {
                        assert!(
                            rect.intersection(region_rect(&frame, other)).is_none(),
                            "overlapping search controls: {width}, {left}: {id} and {other}"
                        );
                    }
                }
                assert!(region_rect(&frame, "editor_query").width >= 40.0);
                if replace_open {
                    let query = region_rect(&frame, "editor_query");
                    let replacement = region_rect(&frame, "editor_replacement");
                    assert!(replacement.width >= 40.0);
                    assert_eq!(replacement.x, query.x, "input starts must align");
                    assert_eq!(replacement.width, query.width, "input ends must align");
                    assert!(replacement.y >= query.y + query.height);
                    assert!(
                        region_rect(&frame, "editor_case").y >= replacement.y + replacement.height,
                        "options must follow both fields"
                    );
                    assert!(!frame.commands.iter().any(|command| matches!(command,
                        openide_ui::DrawCommand::Text { text, .. } if text == "Una" || text == "Todas"
                    )));
                }

                // Empty padding belongs to the floating surface, so it must
                // never place the caret in code painted underneath it.
                let padding = Point::new(surface.x + 2.0, surface.y + surface.height - 2.0);
                assert!(editor.contains(padding));
                assert_eq!(frame.hit_test(padding), Some(SURFACE));
                // Search is non-modal: visible code outside it stays usable.
                let below = Point::new(editor.x + 40.0, surface.y + surface.height + 24.0);
                assert!(editor.contains(below));
                assert_eq!(frame.hit_test(below), Some("code_editor:71"));
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn search_errors_stay_visible_inside_the_floating_panel_without_moving_code() {
    for width in [240.0, 320.0, 430.0, 520.0, 900.0] {
        let (view, mut state) = fixture();
        state.search_open = true;
        state.query = "[".into();
        state.search_options.regex = true;
        let root = |state: &EditorWorkbenchView| {
            Element::row(vec![
                Element::spacer().size(Some(311.0), None),
                crate::code_editor_ui::build(&view, state, width, Default::default(), false),
            ])
        };
        let size = Size::new(311.0 + width, 800.0);
        let (context, mut tree) = native_tree(root(&state));
        let initial = tree.layout(size);
        let editor = region_rect(&initial, "code_editor:71");
        state.search_error =
            Some("Expresión regular inválida: clase de caracteres sin cerrar".into());
        tree.set_root(root(&state));
        let frame = tree.layout(size);
        assert_eq!(region_rect(&frame, "code_editor:71"), editor);
        let surface = region_rect(&frame, "popover_surface:editor_search_close");
        let error = region_rect(&frame, "editor_search_error");
        assert!(error.width > 0.0 && error.height > 0.0);
        assert!(
            error.x >= surface.x - 0.01
                && error.y >= surface.y - 0.01
                && error.x + error.width <= surface.x + surface.width + 0.01
                && error.y + error.height <= surface.y + surface.height + 0.01,
            "search error escaped its floating surface at {width}: {error:?}"
        );
        assert_eq!(
            frame.hit_test(Point::new(
                error.x + error.width / 2.0,
                error.y + error.height / 2.0,
            )),
            Some("editor_search_error")
        );
        assert!(error
            .intersection(region_rect(&frame, "editor_query"))
            .is_none());
        context.end_pass().textures_delta.clear();
    }
}

fn export(frame: &Frame, width: f32, name: &str) {
    let directory = PathBuf::from("/tmp/openade-editor-workbench-review");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}-{width:.0}.svg"));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, width, 800.0).unwrap();
    let tree = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut image = resvg::tiny_skia::Pixmap::new(width as u32, 800).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut image.as_mut(),
    );
    image.save_png(path.with_extension("png")).unwrap();
}

#[test]
#[ignore = "opt-in actual native editor toolbars, forms and context review"]
fn export_editor_workbench_visual_review() {
    for width in [320.0, 520.0, 900.0] {
        for phase in [
            "toolbar",
            "dirty",
            "saving",
            "save-error",
            "actions-menu",
            "actions-menu-hover",
            "actions-menu-scroll",
            "search",
            "search-empty",
            "search-error",
            "search-replace",
            "search-replace-empty",
            "save-as",
            "new-file",
            "outline",
            "context-menu",
        ] {
            let (mut view, mut state) = fixture();
            if matches!(phase, "dirty" | "saving" | "save-error")
                || phase.starts_with("actions-menu")
            {
                view.controller
                    .as_ref()
                    .unwrap()
                    .shared_buffer()
                    .lock()
                    .unwrap()
                    .insert(
                        "// Cambios sin guardar\n",
                        openide_editor::EditKind::Paste,
                        1,
                    )
                    .unwrap();
            }
            match phase {
                "saving" => view.saving = true,
                "save-error" => {
                    view.error = Some("No se pudo guardar: el archivo cambió en disco.".into())
                }
                "actions-menu" | "actions-menu-hover" | "actions-menu-scroll" => {
                    state.menu_open = true
                }
                "search" | "search-replace" => {
                    state.search_open = true;
                    state.replace_open = phase == "search-replace";
                    populate_search(&view, &mut state);
                }
                "search-empty" => state.search_open = true,
                "search-replace-empty" => {
                    state.search_open = true;
                    state.replace_open = true;
                }
                "search-error" => {
                    state.search_open = true;
                    state.query = "[".into();
                    state.search_options.regex = true;
                    state.search_error =
                        Some("Expresión regular inválida: clase de caracteres sin cerrar".into());
                }
                "save-as" => {
                    state.form = Some(EditorForm::SaveAs);
                    state.form_value = "src/native_commands.rs".into();
                }
                "new-file" => {
                    state.form = Some(EditorForm::NewFile);
                    state.form_value = "src/workspace.rs".into();
                }
                "outline" => state.outline_open = true,
                _ => {}
            }
            let (context, mut tree) = native_tree(crate::code_editor_ui::build(
                &view,
                &state,
                width,
                Default::default(),
                false,
            ));
            let size = Size::new(width, 800.0);
            let mut rendered = tree.layout(size);
            if phase == "actions-menu-hover" {
                let save = region_rect(&rendered, "code_editor_save:71");
                tree.handle_event(UiEvent::PointerMove(Point::new(
                    save.x + 12.0,
                    save.y + 16.0,
                )));
                rendered = tree.layout(size);
            }
            if phase == "actions-menu-scroll" {
                let options = region_rect(&rendered, "editor_menu_close:options");
                tree.handle_event(UiEvent::ScrollDirect {
                    position: Point::new(options.x + 16.0, options.y + 16.0),
                    delta_y: -100_000.0,
                });
                rendered = tree.layout(size);
                // The footer also opens editor settings. Inspect the menu's
                // later-painted row instead of that covered footer control.
                let last = rendered
                    .regions
                    .iter()
                    .rev()
                    .find(|region| region.id == "editor_settings")
                    .unwrap()
                    .rect;
                assert_eq!(
                    rendered.hit_test(Point::new(last.x + 12.0, last.y + last.height / 2.0)),
                    Some("editor_settings"),
                    "{width}: last={last:?}, options={options:?}, surface={:?}, offset={}",
                    region_rect(&rendered, "popover_surface:editor_menu_close"),
                    tree.scroll_offset("editor_menu_close:options"),
                );
            }
            if phase == "search-replace-empty" {
                assert!(tree.focus("editor_replacement"));
                rendered = tree.layout(size);
                assert!(rendered.commands.iter().any(|command| matches!(command,
                    openide_ui::DrawCommand::Text { text, .. } if text == "Reemplazar por"
                )));
            }
            if matches!(phase, "search" | "search-replace") {
                assert!(tree.reveal_code_caret("code_editor:71"));
                rendered = tree.layout(size);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while tree.animation_repaint_after().is_some() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(16));
                rendered = tree.layout(size);
            }
            let editor = rendered
                .regions
                .iter()
                .find(|r| r.id == "code_editor:71")
                .expect("native editor is visible")
                .rect;
            assert!(
                editor.height > 100.0,
                "{phase} {width} left no editor space"
            );
            for id in ["editor_find", "editor_menu"] {
                let rect = rendered.regions.iter().find(|r| r.id == id).unwrap().rect;
                assert!(
                    rect.x >= -0.5 && rect.x + rect.width <= width + 0.5,
                    "{phase} {width}: {id} out of bounds: {rect:?}"
                );
            }
            if !state.menu_open {
                assert!(!rendered
                    .regions
                    .iter()
                    .any(|region| region.id == "editor_outline" || region.id == "editor_wrap"));
            }
            if state.form.is_some() {
                assert!(rendered
                    .regions
                    .iter()
                    .any(|r| r.id == "editor_form_value" && r.rect.width >= 80.0));
            }
            if phase == "context-menu" {
                assert!(tree.focus("code_editor:71"));
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::A,
                    ctrl: true,
                    shift: false,
                });
                tree.handle_event(UiEvent::ContextMenu {
                    position: Point::new(editor.x + 34.0, editor.y + 36.0),
                });
                rendered = tree.layout(size);
                assert!(tree.context_menu_open());
                let menu = rendered
                    .regions
                    .iter()
                    .find(|r| r.id == "__context_menu:surface")
                    .unwrap()
                    .rect;
                assert!(menu.x >= 0.0 && menu.x + menu.width <= width + 0.5);
                assert!(menu.y >= 0.0 && menu.y + menu.height <= size.height + 0.5);
            }
            export(&rendered, width, phase);
            context.end_pass().textures_delta.clear();
        }
    }
}
