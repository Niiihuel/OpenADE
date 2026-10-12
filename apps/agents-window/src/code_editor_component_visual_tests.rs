use eframe::egui;
use openide_editor::{Selection, TextBuffer};
use openide_ui::{
    CodeDiff, CodeDiffLine, CodeDiffLineKind, CodeEditor, CodeFoldCandidate, CodeLanguage,
    Component, DrawCommand, EditorController, Element, FontFamily, Frame, Key, Point, Size,
    UiAction, UiEvent, UiTree,
};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn source() -> String {
    format!("use std::sync::Arc;\r\n\r\n/* Comentario multilínea\r\n   El cursor y la selección conservan Unicode.\r\n*/\r\npub fn saludo(nombre: &str) -> String {{\r\n\tlet café = \"Hola, 🧠 {{nombre}} — 終\";\r\n\tlet mensaje = r###\"Una cadena\r\ncon otra línea\"###;\r\n\tlet larga = \"{}\";\r\n\tformat!(\"{{café}} {{mensaje}}\")\r\n}}\r\n", "texto exacto ".repeat(100))
}
fn markdown_source() -> String {
    let mut lines = vec![
        "# Revisión del editor".to_owned(),
        String::new(),
        "Documento de trabajo con texto, listas y ejemplos de código.".to_owned(),
        String::new(),
    ];
    for section in 1..=8 {
        lines.push(format!("## Paso {section}: revisar documento"));
        lines.extend(
            [
                "",
                "Una selección conserva columnas, tabs y Unicode: café, 東京 y 🧠.",
                "- [x] Revisar el encabezado del archivo.",
                "- [ ] Comparar la vista con el código completo.",
                "",
                "```rust",
                "fn saludar(nombre: &str) {",
                "\tprintln!(\"Hola, {nombre} — 終\");",
                "}",
                "```",
                "",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    lines.join("\n")
}
fn native_tree(root: Element, motion: bool) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput::default());
    let measurer = ctx.clone();
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(!motion);
    let height_ctx = ctx.clone();
    tree.set_text_height_measurer(move |size, family| {
        height_ctx.fonts_mut(|fonts| {
            fonts.row_height(&egui::FontId::new(
                size,
                match family {
                    FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                    FontFamily::Monospace => egui::FontFamily::Monospace,
                },
            ))
        })
    });
    tree.set_text_measurer(move |text, size, family| {
        measurer.fonts_mut(|fonts| {
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
fn settle(tree: &mut UiTree, size: Size) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let frame = tree.layout(size);
        if !tree.is_animating() {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "bounded lexical work did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn write_png(frame: &Frame, path: &Path, width: f32, height: f32) {
    let svg_path = path.with_extension("svg");
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &svg_path, width, height).unwrap();
    let options = crate::settings_ui::visual_qa::native_svg_options();
    let parsed =
        resvg::usvg::Tree::from_str(&std::fs::read_to_string(svg_path).unwrap(), &options).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path).unwrap();
}
fn diff_rows() -> Arc<[CodeDiffLine]> {
    use CodeDiffLineKind::{Added, Context, Removed};
    vec![
        CodeDiffLine {
            kind: Context,
            old_line: None,
            new_line: None,
            text: "@@ -24,6 +24,9 @@".into(),
        },
        CodeDiffLine {
            kind: Context,
            old_line: Some(24),
            new_line: Some(24),
            text: "pub fn saludo(nombre: &str) -> String {".into(),
        },
        CodeDiffLine {
            kind: Removed,
            old_line: Some(25),
            new_line: None,
            text: "\tlet saludo = \"Hola\";".into(),
        },
        CodeDiffLine {
            kind: Added,
            old_line: None,
            new_line: Some(25),
            text: "\tlet café = \"Hola, 🧠 {nombre} — 終\";".into(),
        },
        CodeDiffLine {
            kind: Added,
            old_line: None,
            new_line: Some(26),
            text: "\t/* Comentario multilínea".into(),
        },
        CodeDiffLine {
            kind: Added,
            old_line: None,
            new_line: Some(27),
            text: "\t   visible con estado de la línea anterior */".into(),
        },
        CodeDiffLine {
            kind: Removed,
            old_line: Some(26),
            new_line: None,
            text: "\tsaludo.to_owned()".into(),
        },
        CodeDiffLine {
            kind: Added,
            old_line: None,
            new_line: Some(28),
            text: "\tformat!(\"{café}\")".into(),
        },
        CodeDiffLine {
            kind: Context,
            old_line: Some(27),
            new_line: Some(29),
            text: "}".into(),
        },
    ]
    .into()
}

#[test]
fn standalone_code_editor_and_diff_use_native_font_geometry_at_360_and_900() {
    for width in [360.0, 900.0] {
        let buffer = TextBuffer::new(&source()).unwrap();
        let controller = EditorController::new(buffer);
        let (ctx, mut tree) = native_tree(
            CodeEditor::new("editor:qa", controller.clone())
                .language(CodeLanguage::Rust)
                .height(320.0)
                .render(),
            false,
        );
        let frame = settle(&mut tree, Size::new(width, 320.0));
        let region = frame
            .regions
            .iter()
            .find(|region| region.id == "editor:qa")
            .unwrap();
        assert_eq!(region.rect.width, width);
        assert_eq!(region.rect.height, 320.0);
        assert!(frame
            .commands
            .iter()
            .filter_map(|command| if let DrawCommand::Text { text, .. } = command {
                Some(text.len())
            } else {
                None
            })
            .all(|length| length < 512));
        assert!(tree.focus("editor:qa"));
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::C,
                shift: false,
                ctrl: true
            }),
            vec![UiAction::CopyRequested { text: source() }]
        );
        let texts: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| {
                if let DrawCommand::Text { text, color, .. } = command {
                    Some((text, color))
                } else {
                    None
                }
            })
            .collect();
        assert!(texts.iter().any(
            |(text, color)| text.contains("Comentario") && **color != crate::theme::FOREGROUND
        ));
        ctx.end_pass().textures_delta.clear();

        let (ctx, mut tree) = native_tree(
            CodeDiff::new("review:qa", diff_rows())
                .language(CodeLanguage::Rust)
                .height(320.0)
                .render(),
            false,
        );
        let frame = settle(&mut tree, Size::new(width, 320.0));
        assert!(frame
            .commands
            .iter()
            .any(|command| matches!(command,DrawCommand::Text {text,..} if text=="+")));
        assert!(frame
            .commands
            .iter()
            .any(|command| matches!(command,DrawCommand::Text {text,..} if text=="−")));
        assert!(tree
            .handle_event(UiEvent::Paste("forbidden".into()))
            .is_empty());
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn additive_editor_events_preserve_legacy_wheel_and_textarea_paste_contract() {
    for direct in [true, false] {
        let root = Element::scroll("legacy", Element::text("row\n".repeat(100)))
            .size(Some(300.0), Some(100.0));
        let mut legacy = UiTree::new(root.clone());
        let mut additive = UiTree::new(root);
        legacy.layout(Size::new(300.0, 100.0));
        additive.layout(Size::new(300.0, 100.0));
        let position = Point::new(50.0, 50.0);
        let now = Instant::now();
        let event = if direct {
            UiEvent::ScrollDirect {
                position,
                delta_y: -75.0,
            }
        } else {
            UiEvent::Scroll {
                position,
                delta_y: -75.0,
            }
        };
        legacy.handle_event_at(event, now);
        additive.handle_event_at(
            UiEvent::Scroll2D {
                position,
                delta_x: 0.0,
                delta_y: -75.0,
                direct,
                shift: true,
            },
            now,
        );
        assert_eq!(
            legacy
                .layout_at(Size::new(300.0, 100.0), now + Duration::from_millis(300))
                .commands,
            additive
                .layout_at(Size::new(300.0, 100.0), now + Duration::from_millis(300))
                .commands
        );
    }
    let mut tree = UiTree::new(Element::text_area("legacy:text", "", ""));
    tree.layout(Size::new(300.0, 140.0));
    assert!(tree.focus("legacy:text"));
    assert_eq!(
        tree.handle_event(UiEvent::Paste("ñ\r\n🧠".into())),
        vec![UiAction::InputChanged {
            id: "legacy:text".into(),
            value: "ñ\n🧠".into()
        }]
    );
}

#[test]
#[ignore = "export standalone native editor and journal diff at both viewport widths"]
fn export_code_editor_component_visual_review() {
    let directory = std::env::var_os("OPENIDE_CODE_EDITOR_VISUAL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/openide-code-editor-visual-qa"));
    std::fs::create_dir_all(&directory).unwrap();
    for width in [360.0, 900.0] {
        for (name, read_only, focused, scrolled) in [
            ("editor", false, true, false),
            ("editor-readonly", true, false, false),
            ("editor-scrolled", false, true, true),
        ] {
            let mut buffer = TextBuffer::new(&source()).unwrap();
            let snapshot = buffer.snapshot();
            let range = snapshot.line_range(6).unwrap();
            buffer
                .set_selection(Selection {
                    anchor: range.start + 5,
                    head: snapshot.line_range(8).unwrap().end,
                })
                .unwrap();
            let controller = EditorController::new(buffer);
            let (ctx, mut tree) = native_tree(
                CodeEditor::new("editor:qa", controller)
                    .language(CodeLanguage::Rust)
                    .read_only(read_only)
                    .minimap(true)
                    .structure(
                        snapshot.document_id(),
                        snapshot.revision(),
                        vec![CodeFoldCandidate {
                            start_row: 5,
                            end_row: 11,
                            action_id: "fold:saludo".into(),
                        }],
                        Vec::new(),
                    )
                    .breakpoints(
                        snapshot.document_id(),
                        snapshot.revision(),
                        "breakpoint",
                        vec![6],
                    )
                    .height(320.0)
                    .render(),
                false,
            );
            settle(&mut tree, Size::new(width, 320.0));
            if focused {
                assert!(tree.focus("editor:qa"));
            }
            if scrolled {
                tree.handle_event(UiEvent::Scroll2D {
                    position: Point::new(width / 2.0, 100.0),
                    delta_x: -300.0,
                    delta_y: -120.0,
                    direct: true,
                    shift: false,
                });
            }
            let frame = settle(&mut tree, Size::new(width, 320.0));
            write_png(
                &frame,
                &directory.join(format!("{name}-{width:.0}.png")),
                width,
                320.0,
            );
            if name == "editor" {
                let gutter = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "breakpoint:7")
                    .expect("visible empty breakpoint row")
                    .rect;
                tree.handle_event(UiEvent::PointerMove(Point::new(
                    gutter.x + gutter.width / 2.0,
                    gutter.y + gutter.height / 2.0,
                )));
                let frame = settle(&mut tree, Size::new(width, 320.0));
                write_png(
                    &frame,
                    &directory.join(format!("editor-gutter-hover-{width:.0}.png")),
                    width,
                    320.0,
                );
            }
            ctx.end_pass().textures_delta.clear();
        }
        let mut buffer = TextBuffer::new(&markdown_source()).unwrap();
        let snapshot = buffer.snapshot();
        buffer
            .set_selection(Selection {
                anchor: snapshot.line_range(42).unwrap().start + 4,
                head: snapshot.line_range(49).unwrap().end,
            })
            .unwrap();
        let (ctx, mut tree) = native_tree(
            CodeEditor::new("editor:markdown:qa", EditorController::new(buffer))
                .language(CodeLanguage::Markdown)
                .minimap(true)
                .height(560.0)
                .render(),
            false,
        );
        settle(&mut tree, Size::new(width, 560.0));
        assert!(tree.focus("editor:markdown:qa"));
        // Set the viewport independently of any initial caret reveal. The
        // selection spans prose, an empty row, and code in the document middle.
        for delta_y in [10_000.0, -720.0] {
            tree.handle_event(UiEvent::Scroll2D {
                position: Point::new(width / 2.0, 280.0),
                delta_x: 0.0,
                delta_y,
                direct: true,
                shift: false,
            });
        }
        let frame = settle(&mut tree, Size::new(width, 560.0));
        write_png(
            &frame,
            &directory.join(format!("editor-markdown-selection-{width:.0}.png")),
            width,
            560.0,
        );
        ctx.end_pass().textures_delta.clear();
        let (ctx, mut tree) = native_tree(
            CodeDiff::new("review:qa", diff_rows())
                .language(CodeLanguage::Rust)
                .height(320.0)
                .render(),
            false,
        );
        let frame = settle(&mut tree, Size::new(width, 320.0));
        write_png(
            &frame,
            &directory.join(format!("diff-{width:.0}.png")),
            width,
            320.0,
        );
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "local CPU benchmark of virtualized 150k-row editor frames and typing"]
fn benchmark_code_editor_visible_frames_and_typing() {
    let source = (0..150_000)
        .map(|row| format!("row {row} unchanged source\n"))
        .collect::<String>();
    let controller = EditorController::new(TextBuffer::new(&source).unwrap());
    let (ctx, mut tree) = native_tree(
        CodeEditor::new("bench", controller).height(560.0).render(),
        false,
    );
    tree.layout(Size::new(900.0, 560.0));
    assert!(tree.focus("bench"));
    let mut frames = Vec::with_capacity(100);
    for _ in 0..100 {
        let now = Instant::now();
        tree.layout(Size::new(900.0, 560.0));
        frames.push(now.elapsed().as_secs_f64() * 1000.0);
    }
    let mut typing = Vec::with_capacity(100);
    for _ in 0..100 {
        let now = Instant::now();
        tree.handle_event(UiEvent::Text("x".into()));
        tree.layout(Size::new(900.0, 560.0));
        typing.push(now.elapsed().as_secs_f64() * 1000.0);
    }
    for (phase, mut samples) in [("static", frames), ("edit+frame", typing)] {
        samples.sort_by(f64::total_cmp);
        eprintln!("150k rows / native monospace / component CPU / {phase}: mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",samples.iter().sum::<f64>()/samples.len() as f64,samples[50],samples[95],samples[99]);
    }
    ctx.end_pass().textures_delta.clear();
}
