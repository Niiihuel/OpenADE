//! Native font geometry for code rows; the fixtures never access project files.
use eframe::egui;
use openide_editor::{Selection, TextBuffer};
use openide_ui::{
    CodeDiff, CodeDiffLine, CodeDiffLineKind, CodeEditor, CodeLanguage, Color, Component,
    DrawCommand, EditorController, Element, FileActivity, FontFamily, Frame, HitKind, Point, Rect,
    Size, UiEvent, UiTree,
};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const ROW_HEIGHT: f32 = 20.0;
const CODE_SIZE: f32 = 13.0;
const SCROLL: f32 = 37.0;
const SELECTION_COLOR: Color = Color::rgba(75, 130, 210, 100);

fn source_line(row: usize) -> String {
    format!("const VALUE_{row:03}: &str = \"Texto en español\";")
}

fn source() -> String {
    (0..40).map(source_line).collect::<Vec<_>>().join("\n")
}

fn controller(selected: bool) -> (EditorController, Selection) {
    let mut buffer = TextBuffer::new(&source()).unwrap();
    let snapshot = buffer.snapshot();
    let selection = Selection {
        anchor: snapshot.line_range(1).unwrap().start + 6,
        head: snapshot.line_range(4).unwrap().start + 13,
    };
    buffer
        .set_selection(if selected {
            selection
        } else {
            Selection::caret(selection.head)
        })
        .unwrap();
    (EditorController::new(buffer), selection)
}

fn diff_rows() -> Arc<[CodeDiffLine]> {
    (0..40)
        .map(|row| {
            let kind = match row % 3 {
                0 => CodeDiffLineKind::Context,
                1 => CodeDiffLineKind::Removed,
                _ => CodeDiffLineKind::Added,
            };
            CodeDiffLine {
                kind,
                old_line: (kind != CodeDiffLineKind::Added).then_some(1000 + row),
                new_line: (kind != CodeDiffLineKind::Removed).then_some(2000 + row),
                text: source_line(row),
            }
        })
        .collect::<Vec<_>>()
        .into()
}

fn font_id(size: f32, family: FontFamily) -> egui::FontId {
    egui::FontId::new(
        size,
        match family {
            FontFamily::Sans | FontFamily::SansItalic => egui::FontFamily::Proportional,
            FontFamily::Monospace => egui::FontFamily::Monospace,
        },
    )
}

fn native_height(ctx: &egui::Context, size: f32, family: FontFamily) -> f32 {
    ctx.fonts_mut(|fonts| fonts.row_height(&font_id(size, family)))
}

fn native_tree(root: Element, scale: f32) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.set_pixels_per_point(scale);
    ctx.begin_pass(egui::RawInput::default());
    let width_ctx = ctx.clone();
    let height_ctx = ctx.clone();
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    tree.set_text_measurer(move |text, size, family| {
        width_ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.into(), font_id(size, family), egui::Color32::WHITE)
                .size()
                .x
        })
    });
    tree.set_text_height_measurer(move |size, family| native_height(&height_ctx, size, family));
    (ctx, tree)
}

fn region(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| {
            region.id == id && matches!(region.kind, HitKind::CodeEditor | HitKind::CodeDiff)
        })
        .unwrap_or_else(|| panic!("missing code region {id}"))
        .rect
}

#[derive(Clone, Copy, Debug)]
enum Surface {
    Editor,
    Diff,
    Inline,
}

impl Surface {
    fn id(self) -> &'static str {
        match self {
            Self::Editor => "alignment:editor",
            Self::Diff => "alignment:diff",
            Self::Inline => "alignment:inline",
        }
    }

    fn root(self, controller: EditorController, language: CodeLanguage) -> Element {
        match self {
            Self::Editor => CodeEditor::new(self.id(), controller)
                .language(language)
                .height(188.0)
                .palette(crate::theme::component_palette())
                .render(),
            Self::Diff => CodeDiff::new(self.id(), diff_rows())
                .language(language)
                .height(188.0)
                .palette(crate::theme::component_palette())
                .render(),
            Self::Inline => FileActivity::new(
                "alignment:disclosure",
                "prueba-raiz.rs",
                CodeDiff::new(self.id(), diff_rows())
                    .single_gutter(true)
                    .language(language)
                    .height(188.0)
                    .palette(crate::theme::component_palette())
                    .render(),
                crate::theme::component_palette(),
            )
            .status("Vista previa · contenido capturado")
            .expanded(true)
            .render(),
        }
    }
}

fn assert_text_centers(ctx: &egui::Context, frame: &Frame, surface: Surface, scroll: f32) {
    let code = region(frame, surface.id());
    let mut source_rows = 0;
    let mut gutter_rows = 0;
    for command in &frame.commands {
        let DrawCommand::Text {
            position,
            text,
            font_size,
            font_family,
            ..
        } = command
        else {
            continue;
        };
        if *font_family != FontFamily::Monospace || (*font_size - CODE_SIZE).abs() > 0.01 {
            continue;
        }
        let row = if let Some(number) = text
            .strip_prefix("const VALUE_")
            .and_then(|tail| tail.get(..3))
            .and_then(|number| number.parse::<usize>().ok())
        {
            source_rows += 1;
            Some(number)
        } else if let Ok(number) = text.parse::<usize>() {
            gutter_rows += 1;
            match surface {
                Surface::Editor => number.checked_sub(1),
                Surface::Diff | Surface::Inline if number >= 2000 => Some(number - 2000),
                Surface::Diff | Surface::Inline => number.checked_sub(1000),
            }
        } else {
            None
        };
        let native = native_height(ctx, *font_size, *font_family);
        let actual_center = position.y + native / 2.0;
        if let Some(row) = row {
            let expected_center =
                code.y + 4.0 + row as f32 * ROW_HEIGHT - scroll + ROW_HEIGHT / 2.0;
            assert!((actual_center - expected_center).abs() < 0.05,
                "{surface:?} scroll={scroll} row={row}: {text:?} center={actual_center}, expected={expected_center}, native_height={native}");
        } else if text == "+" || text == "−" {
            let first_center = code.y + 4.0 - scroll + ROW_HEIGHT / 2.0;
            let row = ((actual_center - first_center) / ROW_HEIGHT).round();
            assert!(
                (actual_center - (first_center + row * ROW_HEIGHT)).abs() < 0.05,
                "diff marker must share the text baseline"
            );
        }
    }
    assert!(
        source_rows >= 7,
        "fixture must measure visible code, {surface:?}: {source_rows}"
    );
    assert!(
        gutter_rows >= source_rows,
        "line numbers must be measured too"
    );
}

fn assert_selection(frame: &Frame, scroll: f32) {
    let code = region(frame, Surface::Editor.id());
    let contours: Vec<_> = frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::SelectionContour { points, color } if *color == SELECTION_COLOR => {
                Some(points)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        contours.len(),
        1,
        "four selected rows share a continuous contour"
    );
    let points = contours[0];
    let top = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let bottom = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let expected_top = code.y + 4.0 + ROW_HEIGHT - scroll;
    let expected_bottom = expected_top + 4.0 * ROW_HEIGHT;
    assert!((top - expected_top - 0.75).abs() < 0.05);
    assert!((bottom - expected_bottom + 0.75).abs() < 0.05);
    // Rounded decoration does not change which row centers are selected.
    for row in 0..6 {
        let y = code.y + 4.0 + (row as f32 + 0.5) * ROW_HEIGHT - scroll;
        let crossings = points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .take(points.len())
            .filter(|(a, b)| (a.y <= y && b.y > y) || (b.y <= y && a.y > y))
            .count();
        assert_eq!(crossings, if (1..=4).contains(&row) { 2 } else { 0 });
    }
}

fn assert_caret(frame: &Frame, scroll: f32) {
    let code = region(frame, Surface::Editor.id());
    let caret = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::FillRect { rect, color, .. }
                if (rect.width - 1.5).abs() < 0.01
                    && *color == crate::theme::component_palette().text =>
            {
                Some(*rect)
            }
            _ => None,
        })
        .expect("the focused caret stays visible in reduced motion");
    assert_eq!(caret.height, ROW_HEIGHT);
    assert!((caret.y - (code.y + 4.0 + 4.0 * ROW_HEIGHT - scroll)).abs() < 0.05);
}

fn scroll(tree: &mut UiTree, frame: &Frame, id: &str, now: Instant) {
    let code = region(frame, id);
    tree.handle_event_at(
        UiEvent::Scroll2D {
            position: Point::new(code.x + code.width / 2.0, code.y + code.height / 2.0),
            delta_x: 0.0,
            delta_y: -SCROLL,
            direct: true,
            shift: false,
        },
        now,
    );
}

#[test]
fn native_code_rows_center_text_and_keep_selection_geometry_after_scroll() {
    for scale in [1.0, 2.0] {
        for surface in [Surface::Editor, Surface::Diff, Surface::Inline] {
            let (controller, selection) = controller(true);
            let (ctx, mut tree) =
                native_tree(surface.root(controller.clone(), CodeLanguage::Plain), scale);
            let viewport = Size::new(520.0, 360.0);
            let now = Instant::now();
            let mut frame = tree.layout_at(viewport, now);
            if matches!(surface, Surface::Editor) {
                assert!(tree.focus(surface.id()));
            }
            for offset in [0.0, SCROLL] {
                if offset > 0.0 {
                    scroll(&mut tree, &frame, surface.id(), now);
                }
                controller
                    .shared_buffer()
                    .lock()
                    .unwrap()
                    .set_selection(selection)
                    .unwrap();
                frame = tree.layout_at(viewport, now);
                assert_text_centers(&ctx, &frame, surface, offset);
                if matches!(surface, Surface::Editor) {
                    assert_selection(&frame, offset);
                    controller
                        .shared_buffer()
                        .lock()
                        .unwrap()
                        .set_selection(Selection::caret(selection.head))
                        .unwrap();
                    frame = tree.layout_at(viewport, now);
                    assert_caret(&frame, offset);
                    assert_text_centers(&ctx, &frame, surface, offset);
                }
            }
            ctx.end_pass().textures_delta.clear();
        }
    }
}

fn caption(text: &str) -> Element {
    Element::text(text)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .foreground(crate::theme::FOREGROUND)
        .size(None, Some(22.0))
}

fn export_root(controller: EditorController) -> Element {
    Element::column(vec![
        caption("Editor nativo · texto centrado en filas de 20 px"),
        Surface::Editor.root(controller.clone(), CodeLanguage::Rust),
        caption("Diferencias · números y marcadores alineados"),
        Surface::Diff.root(controller.clone(), CodeLanguage::Rust),
        caption("Vista previa del chat · una columna de números"),
        Surface::Inline.root(controller, CodeLanguage::Rust),
    ])
    .gap(12.0)
    .padding(16.0)
    .background(crate::theme::BACKGROUND)
}

fn settle(tree: &mut UiTree, viewport: Size) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let frame = tree.layout(viewport);
        if !tree.is_animating() {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "code highlighting did not settle"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn export(frame: &Frame, name: &str, viewport: Size, scale: f32) {
    let directory = Path::new("/tmp/openade-code-line-alignment-review");
    std::fs::create_dir_all(directory).unwrap();
    let svg = directory.join(format!("{name}-{scale:.0}x.svg"));
    crate::settings_ui::visual_qa::write_native_frame_svg(
        frame,
        &svg,
        viewport.width,
        viewport.height,
    )
    .unwrap();
    let parsed = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&svg).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(
        (viewport.width * scale) as u32,
        (viewport.height * scale) as u32,
    )
    .unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(svg.with_extension("png")).unwrap();
}

#[test]
#[ignore = "exports native font code alignment, multiline selection and caret at scales 1x and 2x"]
fn export_code_line_alignment_visual_review() {
    let viewport = Size::new(720.0, 830.0);
    for scale in [1.0, 2.0] {
        for selected in [true, false] {
            let (controller, _) = controller(selected);
            let (ctx, mut tree) = native_tree(export_root(controller), scale);
            settle(&mut tree, viewport);
            assert!(tree.focus(Surface::Editor.id()));
            let first = settle(&mut tree, viewport);
            let scenario = if selected { "selection" } else { "caret" };
            export(&first, scenario, viewport, scale);
            let now = Instant::now();
            for surface in [Surface::Editor, Surface::Diff, Surface::Inline] {
                scroll(&mut tree, &first, surface.id(), now);
            }
            export(
                &settle(&mut tree, viewport),
                &format!("{scenario}-scrolled"),
                viewport,
                scale,
            );
            ctx.end_pass().textures_delta.clear();
        }
    }
}
