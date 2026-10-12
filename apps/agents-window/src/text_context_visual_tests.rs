//! Opt-in native-font review; fixtures never read the clipboard or start an agent.

use crate::{theme, view};
use eframe::egui;
use openide_ui::{DrawCommand, Element, FontFamily, Frame, Key, Point, Size, UiEvent, UiTree};
use std::time::{Duration, Instant};

const DRAFT: &str =
    "Revisá el proyecto y conservá este borrador.\nTexto seleccionado: español, ñ y 🦀.";
const READ_ONLY: &str = "Texto de sólo lectura: conservar esta selección.";

fn native_tree(root: Element) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(root);
    tree.set_context_menu_palette(theme::menu_palette());
    let fonts = ctx.clone();
    tree.set_text_measurer(move |text, size, family| {
        fonts.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    (ctx, tree)
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = std::path::Path::new("/tmp/openade-text-context-review");
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}.svg"));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let svg = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut image = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &svg,
        resvg::tiny_skia::Transform::identity(),
        &mut image.as_mut(),
    );
    image.save_png(path.with_extension("png")).unwrap();
}

fn assert_menu(frame: &Frame, size: Size) {
    let menu = frame
        .regions
        .iter()
        .find(|region| region.id == "__context_menu:surface")
        .expect("context menu must be visible")
        .rect;
    assert!(menu.width > 0.0 && menu.height > 0.0);
    assert!(menu.x >= 0.0 && menu.y >= 0.0);
    assert!(menu.x + menu.width <= size.width + 0.5);
    assert!(menu.y + menu.height <= size.height + 0.5);
    for label in ["Copiar", "Cortar", "Pegar", "Seleccionar todo"] {
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == label
            )),
            "missing menu label {label}"
        );
    }
}

fn has_caret(frame: &Frame) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command,
            DrawCommand::FillRect { rect, .. } if rect.width == 1.5 && rect.height >= 16.0
        )
    })
}

#[test]
#[ignore = "opt-in text context menu and caret review with native fonts"]
fn export_text_context_visual_review() {
    for width in [520.0, 1100.0] {
        let size = Size::new(width, 800.0);
        let mut state = view::tests::empty_state();
        state.workspace = "/projects/openADE".into();
        state.prompt = DRAFT.into();
        state.show_side_pane = false;
        state.sidebar_collapsed = width < 700.0;
        let (ctx, mut tree) = native_tree(view::build_window_view(&state, width, size.height));
        let now = Instant::now();
        tree.layout_at(size, now);
        assert!(tree.focus("prompt"));
        tree.handle_event_at(
            UiEvent::Key {
                key: Key::A,
                shift: false,
                ctrl: true,
            },
            now,
        );
        let selected = tree.input_selection("prompt");
        assert_eq!(selected, Some((0, DRAFT.chars().count())));
        let before = tree.layout_at(size, now);
        let editor = before
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap()
            .rect;
        tree.handle_event_at(
            UiEvent::ContextMenu {
                position: Point::new(
                    editor.x + editor.width - 8.0,
                    editor.y + editor.height / 2.0,
                ),
            },
            now,
        );
        let frame = tree.layout_at(size, now + Duration::from_millis(150));
        assert_menu(&frame, size);
        assert_eq!(tree.input_selection("prompt"), selected);
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        assert_eq!(tree.focused_id(), Some("prompt"));
        assert_eq!(
            frame
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap()
                .rect,
            editor
        );
        export(&frame, size, &format!("composer-context-{width:.0}"));
        ctx.end_pass().textures_delta.clear();
    }

    let size = Size::new(520.0, 420.0);
    let label = |text| {
        Element::text(text)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND_SUBTLE)
    };
    let field = |element: Element, height| {
        element
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .foreground(theme::FOREGROUND)
            .background(theme::INPUT)
            .padding(9.0)
            .radius(8.0)
            .size(None, Some(height))
    };
    let root = Element::column(vec![
        label("Campo de texto"),
        field(Element::text_input("field", "Proyecto local", ""), 38.0),
        label("Editor de texto"),
        field(
            Element::text_area(
                "editor",
                "El cursor conserva su posición.\nEscribí aquí.",
                "",
            ),
            96.0,
        ),
        label("Contenido de sólo lectura"),
        Element::text(READ_ONLY)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .foreground(theme::FOREGROUND),
    ])
    .gap(12.0)
    .padding(20.0)
    .background(theme::BACKGROUND)
    .size(Some(size.width), Some(size.height));
    let (ctx, mut tree) = native_tree(root);
    let now = Instant::now();
    tree.layout_at(size, now);
    assert!(tree.focus("editor"));
    let visible = tree.layout_at(size, now);
    assert!(has_caret(&visible));
    export(&visible, size, "text-caret-visible");
    let hidden = tree.layout_at(size, now + Duration::from_millis(500));
    assert!(!has_caret(&hidden));
    assert_eq!(
        tree.input_cursor("editor"),
        Some(
            "El cursor conserva su posición.\nEscribí aquí."
                .chars()
                .count()
        )
    );
    export(&hidden, size, "text-caret-hidden");

    let layouts = hidden.text_layouts();
    let text = layouts
        .iter()
        .find(|block| block.source == READ_ONLY)
        .unwrap();
    let line = text.runs.first().unwrap().rect;
    let point = Point::new(line.x + 1.0, line.y + line.height / 2.0);
    let end = Point::new(line.x + 150.0_f32.min(line.width - 1.0), point.y);
    let at = now + Duration::from_millis(550);
    tree.handle_event_at(UiEvent::PointerDown(point), at);
    tree.handle_event_at(UiEvent::PointerMove(end), at);
    tree.handle_event_at(UiEvent::PointerUp(end), at);
    let selection = tree
        .selected_text()
        .expect("read-only text selection")
        .to_owned();
    tree.handle_event_at(UiEvent::ContextMenu { position: point }, at);
    let frame = tree.layout_at(size, at);
    assert_menu(&frame, size);
    assert_eq!(tree.selected_text(), Some(selection.as_str()));
    export(&frame, size, "readonly-context");
    ctx.end_pass().textures_delta.clear();
}
