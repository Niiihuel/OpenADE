//! Native command-center fixtures over the real IDE, with bundled fonts.
use std::path::Path;

use eframe::egui;
use openide_ui::{Color, DrawCommand, Element, Frame, Point, Rect, Size, UiAction, UiEvent};

use crate::{
    command_center::{self, FileCandidate, Scope, SessionCandidate, Target},
    view::{self, ViewState},
};

const DIRECTORY: &str = "/tmp/openade-command-center-review";
const FILES: [&str; 10] = [
    ".env.example",
    ".gitignore",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "prueba.md",
    "THIRD_PARTY_NOTICES.md",
    "apps/agents-window/src/workspace.rs",
    "docs/ide-workspace.md",
    "crates/openide-ui/src/tree.rs",
];

fn fixture(scope: Scope) -> ViewState {
    let mut state = view::ide_visual_tests::fixture();
    state.show_command_center = true;
    state.command_scope = scope;
    state.command_query.clear();
    state.command_selected_index = 0;
    let files: Vec<_> = FILES
        .iter()
        .map(|path| FileCandidate {
            path: Path::new(path),
            name: Path::new(path).file_name().unwrap().to_str().unwrap(),
            is_dir: false,
        })
        .collect();
    state.command_results = command_center::search(
        "",
        scope,
        &[SessionCandidate {
            id: "ide-review",
            title: "Revisar el workspace",
            subtitle: "openADE",
        }],
        &files,
        command_center::DEFAULT_RESULT_LIMIT,
    );
    state
}

fn dialog_rect(size: Size) -> Rect {
    // Match the host overlay's centered, bounded command_center_rect.
    let width = (size.width - 32.0).clamp(0.0, 560.0);
    let height = (size.height - 48.0).clamp(0.0, 420.0);
    Rect::new(
        (size.width - width) / 2.0,
        (size.height - height) / 2.0,
        width,
        height,
    )
}

fn scene(state: &ViewState, size: Size) -> Element {
    let dialog = dialog_rect(size);
    let floating = Element::column(vec![
        Element::spacer().flex(1.0),
        Element::row(vec![
            Element::spacer().flex(1.0),
            view::build_command_center(state, dialog.width, dialog.height),
            Element::spacer().flex(1.0),
        ])
        .size(None, Some(dialog.height)),
        Element::spacer().flex(1.0),
    ])
    .background(Color::rgba(0, 0, 0, 180))
    .size(Some(size.width), Some(size.height));
    Element::overlay(
        view::build_window_view(state, size.width, size.height),
        floating,
    )
}

fn result_bounds(frame: &Frame, index: usize) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == format!("command_result:{index}"))
        .unwrap_or_else(|| panic!("missing result {index}"))
        .rect
}

fn control_bounds(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing control {id}"))
        .rect
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x - 0.1
        && inner.y >= outer.y - 0.1
        && inner.x + inner.width <= outer.x + outer.width + 0.1
        && inner.y + inner.height <= outer.y + outer.height + 0.1
}

fn text_bounds<'a>(context: &egui::Context, command: &'a DrawCommand) -> Option<(&'a str, Rect)> {
    let DrawCommand::Text {
        text,
        position,
        font_size,
        font_family,
        ..
    } = command
    else {
        return None;
    };
    let family = match font_family {
        openide_ui::FontFamily::Sans | openide_ui::FontFamily::SansItalic => egui::FontFamily::Proportional,
        openide_ui::FontFamily::Monospace => egui::FontFamily::Monospace,
    };
    let (width, height) = context.fonts_mut(|fonts| {
        (
            fonts
                .layout_no_wrap(
                    text.clone(),
                    egui::FontId::new(*font_size, family.clone()),
                    egui::Color32::WHITE,
                )
                .size()
                .x,
            fonts.row_height(&egui::FontId::new(14.0, family)) * font_size / 14.0,
        )
    });
    Some((text, Rect::new(position.x, position.y, width, height)))
}

#[test]
fn search_uses_full_header_width_and_footer_hints_stay_complete_at_compact_sizes() {
    let state = fixture(Scope::Commands);
    for width in [300.0, 488.0, 560.0] {
        let size = Size::new(width, 420.0);
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_command_center(
            &state,
            size.width,
            size.height,
        ));
        tree.layout(size);
        assert!(tree.focus("command_query"));
        let frame = tree.layout(size);
        let input = control_bounds(&frame, "command_query");
        let close = control_bounds(&frame, "command_close");
        let content = Rect::new(10.0, 10.0, width - 20.0, size.height - 20.0);
        assert!((input.x - content.x).abs() < 0.1);
        assert!((input.width - content.width).abs() < 0.1);
        assert!(contains_rect(content, input));
        assert!(contains_rect(content, close));
        assert!(close.y > input.y + input.height);
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, position, .. }
                    if text == "Buscar una acción…" && input.contains(*position)
            )),
            "the empty, focused input keeps its search guidance"
        );

        let footer = Rect::new(content.x, close.y, content.width, close.height);
        let mut caps: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, radius, .. }
                    if contains_rect(footer, *rect) && *radius == 3.0 && rect.height == 18.0 =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .collect();
        caps.sort_by(|left, right| left.x.total_cmp(&right.x));
        assert_eq!(
            caps.len(),
            4,
            "both arrows, Enter and Escape remain visible at {width}px"
        );
        for pair in caps.windows(2) {
            assert!(pair[0].x + pair[0].width < pair[1].x);
            assert!(
                (pair[0].y + pair[0].height / 2.0 - pair[1].y - pair[1].height / 2.0).abs() < 0.1
            );
        }
        for (icon, cap) in [openide_ui::Icon::ArrowUp, openide_ui::Icon::ArrowDown]
            .into_iter()
            .zip(&caps)
        {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Icon { rect, icon: actual, .. }
                        if *actual == icon && contains_rect(*cap, *rect)
                )),
                "arrow hints use complete native icons inside their shared keycaps"
            );
        }
        let labels: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| text_bounds(&context, command))
            .filter(|(_, rect)| rect.y >= footer.y && rect.y < footer.y + footer.height)
            .collect();
        for label in ["Navegar", "Abrir", "Cerrar"] {
            let matches: Vec<_> = labels.iter().filter(|(text, _)| *text == label).collect();
            assert_eq!(
                matches.len(),
                1,
                "complete footer label {label} at {width}px"
            );
            assert!(contains_rect(footer, matches[0].1));
        }
        let key_labels = if cfg!(target_os = "macos") {
            ["↵", "⎋"]
        } else {
            ["Enter", "Esc"]
        };
        for (label, cap) in key_labels.into_iter().zip(caps.iter().skip(2)) {
            let (_, bounds) = labels.iter().find(|(text, _)| *text == label).unwrap();
            assert!(
                contains_rect(*cap, *bounds),
                "key label {label} must fit its cap"
            );
        }
        let mut visible = caps.clone();
        visible.extend(
            labels
                .iter()
                .filter_map(|(label, rect)| (!key_labels.contains(label)).then_some(*rect)),
        );
        visible.sort_by(|left, right| left.x.total_cmp(&right.x));
        for pair in visible.windows(2) {
            assert!(
                pair[0].x + pair[0].width <= pair[1].x + 0.1,
                "footer controls and hints overlap at {width}px: {pair:?}"
            );
        }
        assert!(contains_rect(close, caps[3]));
        let (_, close_label) = labels.iter().find(|(label, _)| *label == "Cerrar").unwrap();
        assert!(contains_rect(close, *close_label));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn category_labels_are_centered_and_hover_keeps_one_stable_selected_surface() {
    let categories = [
        ("all", "Todo", Scope::All),
        ("commands", "Acciones", Scope::Commands),
        ("sessions", "Tareas", Scope::Sessions),
        ("files", "Archivos", Scope::Files),
    ];
    for width in [300.0, 488.0, 560.0] {
        for (_, _, scope) in categories {
            let state = fixture(scope);
            let size = Size::new(width, 420.0);
            let (context, mut tree) = view::ide_visual_tests::native_tree(
                view::build_command_center(&state, size.width, size.height),
            );
            let initial = tree.layout(size);
            let first = control_bounds(&initial, "command_scope:all");
            let last = control_bounds(&initial, "command_scope:files");
            let strip = Rect::new(
                first.x,
                first.y,
                last.x + last.width - first.x,
                first.height,
            );
            let surfaces = |frame: &Frame| -> Vec<(Rect, Color)> {
                frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::FillRect { rect, color, .. }
                        | DrawCommand::FillCornerRect { rect, color, .. }
                            if color.a > 0 && contains_rect(strip, *rect) =>
                        {
                            Some((*rect, *color))
                        }
                        _ => None,
                    })
                    .collect()
            };
            let initial_surfaces = surfaces(&initial);
            assert_eq!(
                initial_surfaces.len(),
                1,
                "only the selected category has a filled surface"
            );
            for (id, title, _) in categories {
                let id = format!("command_scope:{id}");
                let bounds = control_bounds(&initial, &id);
                let center = Point::new(
                    bounds.x + bounds.width / 2.0,
                    bounds.y + bounds.height / 2.0,
                );
                tree.handle_event(UiEvent::PointerMove(center));
                let hovered = tree.layout(size);
                assert_eq!(control_bounds(&hovered, &id), bounds);
                assert_eq!(
                    surfaces(&hovered),
                    initial_surfaces,
                    "hover must not add a second filled tab"
                );
                let label_rect = |frame: &Frame| {
                    frame
                        .commands
                        .iter()
                        .filter_map(|command| text_bounds(&context, command))
                        .find(|(text, rect)| *text == title && contains_rect(bounds, *rect))
                        .unwrap_or_else(|| panic!("complete category label {title} at {width}px"))
                        .1
                };
                let label = label_rect(&initial);
                assert_eq!(
                    label_rect(&hovered),
                    label,
                    "hover must not move category text"
                );
                assert!(
                    (label.x + label.width / 2.0 - center.x).abs() < 0.1,
                    "{title}: horizontal center"
                );
                assert!(
                    (label.y + label.height / 2.0 - center.y).abs() < 0.1,
                    "{title}: vertical center"
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn file_results_keep_one_inline_filename_icon_and_scoped_full_row_actions() {
    let state = fixture(Scope::Files);
    assert_eq!(state.command_results.len(), 10);
    for width in [300.0, 488.0, 560.0] {
        let size = Size::new(width, 420.0);
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_command_center(
            &state,
            size.width,
            size.height,
        ));
        tree.layout(size);
        for (index, result) in state.command_results.iter().enumerate() {
            tree.scroll_to_index("command_results", view::command_result_row(&state, index));
            let frame = tree.layout(size);
            let bounds = result_bounds(&frame, index);
            assert_eq!(bounds.height, 30.0, "entire row {index} must be revealed");
            assert!(bounds.x >= 10.0 && bounds.x + bounds.width <= width - 10.0);
            let Target::OpenFile(path) = &result.target else {
                panic!("Files scope contains a non-file result");
            };
            let icon = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon { rect, icon, .. }
                        if *icon == openide_ui::file_icon(&path.to_string_lossy())
                            && rect.y >= bounds.y
                            && rect.y + rect.height <= bounds.y + bounds.height =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .expect("each file uses its shared native file icon");
            assert_eq!((icon.width, icon.height), (16.0, 16.0));
            assert!((icon.y + icon.height / 2.0 - bounds.y - bounds.height / 2.0).abs() < 0.01);
            let labels: Vec<_> = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, position, .. }
                        if position.y >= bounds.y && position.y < bounds.y + bounds.height =>
                    {
                        Some((text.as_str(), *position))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                labels
                    .iter()
                    .filter(|(text, _)| *text == result.title)
                    .count(),
                1
            );
            let parent = path.parent().unwrap().to_string_lossy();
            assert_eq!(labels.len(), if parent.is_empty() { 1 } else { 2 });
            assert!(labels[0].1.x >= icon.x + icon.width + 8.0);
            if !parent.is_empty() {
                assert!(labels[1].1.x > labels[0].1.x);
                assert!(
                    (labels[1].1.y - labels[0].1.y).abs() < 2.0,
                    "directory belongs on the filename line, not a second row"
                );
                assert!(parent.starts_with(labels[1].0.trim_end_matches('…')));
                assert!(
                    !labels[1].0.contains(&result.title),
                    "directory must not repeat the name"
                );
            }
            let point = Point::new(
                bounds.x + bounds.width - 4.0,
                bounds.y + bounds.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerMove(point));
            assert_eq!(
                tree.hovered_id(),
                Some(format!("command_result:{index}").as_str())
            );
            let hovered = tree.layout(size);
            assert_eq!(
                result_bounds(&hovered, index),
                bounds,
                "hover must not move the row"
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(tree
                .handle_event(UiEvent::PointerUp(point))
                .contains(&UiAction::Clicked {
                    id: view::command_result_action(&state, index),
                }));
        }
        context.end_pass().textures_delta.clear();
    }
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new(DIRECTORY);
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}x{:.0}.svg", size.width, size.height));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let parsed = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.save_png(path.with_extension("png")).unwrap();
}

#[test]
#[ignore = "exports the compact command center over the native IDE with real fonts"]
fn export_command_center_visual_review() {
    for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
        for (name, scope) in [
            ("files", Scope::Files),
            ("all", Scope::All),
            ("commands", Scope::Commands),
        ] {
            let state = fixture(scope);
            let (context, mut tree) = view::ide_visual_tests::native_tree(scene(&state, size));
            tree.layout(size);
            assert!(tree.focus("command_query"));
            let frame = tree.layout(size);
            let dialog = dialog_rect(size);
            for region in frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with("command_"))
            {
                assert!(region.rect.x >= dialog.x && region.rect.y >= dialog.y);
                assert!(region.rect.x + region.rect.width <= dialog.x + dialog.width + 0.01);
                assert!(region.rect.y + region.rect.height <= dialog.y + dialog.height + 0.01);
            }
            export(&frame, size, name);
            if scope == Scope::Files {
                tree.scroll_to_end("command_results");
                export(&tree.layout(size), size, "files-scrolled");
            }
            context.end_pass().textures_delta.clear();
        }
    }
}
