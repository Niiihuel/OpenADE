//! Real-composer context review with synthetic usage and the application's fonts.
//! No provider, persisted conversation, credentials, or user documents are read.

use crate::composer_ui::{build_composer, ComposerProps};
use crate::native_context_ui::{
    NativeContextCategory, NativeContextMeasurementStatus, NativeContextRow, NativeContextSection,
    NativeContextUiState,
};
use crate::{theme, view};
use eframe::egui;
use openide_core::ModelTokenUsage;
use openide_ui::{
    DrawCommand, Element, FontFamily, Frame, Key, Rect, Size, UiAction, UiEvent, UiTree,
};
use std::path::Path;

const CASES: [&str; 11] = [
    "unknown",
    "unknown-window",
    "measured-30",
    "tiny",
    "near-full-95",
    "compacted",
    "input-only",
    "five-long-sources",
    "ready-to-compact",
    "compacting",
    "error",
];
const SOURCES: [&str; 7] = [
    "Sistema",
    "Proyecto",
    "Skills",
    "Herramientas",
    "MCP",
    "Subagentes",
    "Conversación",
];
const DRAFT: &str = "Revisá el uso de contexto sin modificar los archivos.";
const DETAIL_MARKER: &str = "DIAGNOSTIC_FINGERPRINT_NOT_SUMMARY";

fn usage(input: u64, output: u64) -> ModelTokenUsage {
    ModelTokenUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: input + output,
        cached_input_tokens: Some(input / 2),
        reasoning_tokens: Some(output / 2),
    }
}

fn fixture(name: &str) -> view::ViewState {
    let mut state = view::tests::empty_state();
    let workspace = std::env::temp_dir().canonicalize().unwrap();
    state.workspace = workspace.display().to_string();
    state.selected_backend = "native".into();
    state.native_provider_id = "fixture-provider".into();
    state.model = "context-review-model".into();
    state.composer_model = state.model.clone();
    state.composer_model_options = vec![(state.model.clone(), "Modelo de revisión".into())];
    state.prompt = DRAFT.into();
    let mut context = NativeContextUiState {
        workspace: Some(workspace),
        provider: state.native_provider_id.clone(),
        model: state.model.clone(),
        window: Some(100_000),
        conversation_usage: Some(usage(240_000, 60_000)),
        reported_responses: 10,
        measurement_status: NativeContextMeasurementStatus::Measured,
        last_usage: Some(match name {
            "tiny" => usage(300, 100),
            "near-full-95" => usage(80_000, 15_000),
            "input-only" => usage(30_000, 8_000),
            _ => usage(24_000, 6_000),
        }),
        ..Default::default()
    };
    match name {
        "unknown" => {
            context.last_usage = None;
            context.measurement_status = NativeContextMeasurementStatus::NoResponse;
        }
        // A measured response on a route whose window nobody verified.
        "unknown-window" => {
            context.window = None;
            context.measurement_status = NativeContextMeasurementStatus::UnverifiedLimit;
        }
        "compacted" => {
            context.last_usage = None;
            context.checkpoint_sequence = Some(42);
            context.measurement_status = NativeContextMeasurementStatus::AfterCheckpoint;
        }
        "error" => {
            context.error = Some("No se puede verificar el journal de esta conversación".into());
        }
        "input-only" => {
            context.window = None;
            context.input_limit = Some(100_000);
        }
        _ => {}
    }
    context.remaining_percent = context
        .used_fraction()
        .map(|fraction| ((1.0 - fraction) * 100.0).round() as u8);
    let values = if name == "five-long-sources" {
        [
            "84296 bytes de instrucciones combinadas y verificadas",
            "12 documentos · 96512 bytes del proyecto seleccionado",
            "24 skills seleccionadas · 188765 bytes renderizados",
            "Catálogo congelado · cantidad y tokens no registrados",
            "12 servidores habilitados · 384 herramientas observadas",
            "Sin desglose",
            "240000 entrada · 60000 salida",
        ]
    } else {
        [
            "8192 bytes",
            "3 docs · 4096 bytes",
            "4 locales · 2048 bytes",
            "Cantidad desconocida",
            "2 servidores",
            "Sin desglose",
            "240000 entrada · 60000 salida",
        ]
    };
    for ((id, label), value) in [
        "system",
        "project",
        "skills",
        "tools",
        "mcp",
        "subagents",
        "conversation",
    ]
    .into_iter()
        .zip(SOURCES)
        .zip(values)
    {
        context.breakdown.push(NativeContextCategory::new(id, label, value,
            "Observación con unidades propias. No representa una atribución de tokens ni se suma al contexto medido."));
    }
    context.sections.push(NativeContextSection {
        title: "Procedencia de la medición".into(),
        rows: vec![NativeContextRow::new(
            "Identidad de la ruta",
            DETAIL_MARKER,
            "Coincidencia exacta de proveedor, modelo, endpoint y perfil de petición.",
        )],
    });
    state.native_context = context;
    if matches!(name, "ready-to-compact" | "compacting") {
        state.selected_session = Some("context-conversation".into());
        state.native_context.owner = state.selected_session.clone();
        state.sessions.push(view::SessionView {
            id: "context-conversation".into(),
            title: "Conversación con historial".into(),
            project: state.workspace.clone(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
            native_automation: None,
        });
        if name == "compacting" {
            state
                .conversation_activity
                .begin_compaction("context-conversation", Path::new(&state.workspace));
        }
    }
    state
}

fn root(state: &view::ViewState, size: Size) -> Element {
    let props =
        ComposerProps::from_state(state, size.width, false, None, "Escribí una tarea", None);
    Element::column(vec![Element::spacer().flex(1.0), build_composer(&props)])
        .background(theme::BACKGROUND)
        .size(Some(size.width), Some(size.height))
}

fn native_tree(element: Element, size: Size) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.width, size.height),
        )),
        ..Default::default()
    });
    let measure = ctx.clone();
    let mut tree = UiTree::new(element);
    tree.set_reduced_motion(true);
    tree.set_text_measurer(move |text, size, family| {
        measure.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans => egui::FontFamily::Proportional,
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

fn region(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing context control {id}"))
        .rect
}

fn assert_popup_bounds(frame: &Frame, size: Size) {
    let popup = region(frame, "popover_surface:composer_context_dismiss");
    assert!(popup.width > 0.0 && popup.height > 0.0);
    assert!(popup.x >= 0.0 && popup.y >= 0.0, "{popup:?}");
    assert!(
        popup.x + popup.width <= size.width + 0.1 && popup.y + popup.height <= size.height + 0.1,
        "popup {popup:?} must fit {size:?}"
    );
    for scroll in frame.regions.iter().filter(|region| {
        matches!(
            region.id.as_str(),
            "composer_context_summary"
        )
    }) {
        let rect = scroll.rect;
        assert!(
            rect.x >= popup.x
                && rect.y >= popup.y
                && rect.x + rect.width <= popup.x + popup.width + 0.1
                && rect.y + rect.height <= popup.y + popup.height + 0.1,
            "scroll {rect:?} must remain inside popup {popup:?}"
        );
    }
}

fn visible_text(frame: &Frame, size: Size) -> Vec<String> {
    let mut clips = vec![Rect::new(0.0, 0.0, size.width, size.height)];
    let mut text = Vec::new();
    for command in &frame.commands {
        match command {
            DrawCommand::PushClip(rect) => {
                let parent = clips.last().unwrap();
                let x = parent.x.max(rect.x);
                let y = parent.y.max(rect.y);
                clips.push(Rect::new(
                    x,
                    y,
                    (parent.x + parent.width).min(rect.x + rect.width) - x,
                    (parent.y + parent.height).min(rect.y + rect.height) - y,
                ));
            }
            DrawCommand::PopClip if clips.len() > 1 => {
                clips.pop();
            }
            DrawCommand::Text {
                position,
                text: value,
                font_size,
                ..
            } => {
                let clip = clips.last().unwrap();
                let middle_y = position.y + font_size / 2.0;
                if position.x >= clip.x
                    && position.x < clip.x + clip.width
                    && middle_y >= clip.y
                    && middle_y < clip.y + clip.height
                {
                    text.push(value.clone());
                }
            }
            _ => {}
        }
    }
    text
}

fn activate(tree: &mut UiTree, id: &str) {
    assert!(tree.focus(id), "focus {id}");
    assert_eq!(
        tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false
        }),
        vec![UiAction::Clicked { id: id.into() }]
    );
}

#[test]
fn context_popup_real_composer_preserves_exact_usage_and_short_window_bounds() {
    for name in CASES {
        for size in [Size::new(360.0, 320.0), Size::new(900.0, 420.0)] {
            let mut state = fixture(name);
            let expected = state.native_context.used_fraction();
            let (ctx, mut tree) = native_tree(root(&state, size), size);
            let before = tree.layout(size);
            let editor = region(&before, "prompt");
            assert!(
                before.commands.iter().any(|command| matches!(command,
                DrawCommand::ProgressRing { fraction, .. } if *fraction == expected)),
                "{name}"
            );
            assert!(
                expected != Some(1.0),
                "fixture cumulative use is 300%, current use must differ"
            );
            activate(&mut tree, "composer_context_toggle");
            state.composer_context_open = true;
            tree.set_root(root(&state, size));
            let frame = tree.layout(size);
            assert_popup_bounds(&frame, size);
            assert_eq!(
                region(&frame, "prompt"),
                editor,
                "popover cannot move the composer"
            );
            let text = visible_text(&frame, size).join("\n");
            assert!(
                !text.contains(DETAIL_MARKER),
                "provenance is only copied"
            );
            assert!(
                !text.contains("300%"),
                "accumulated consumption is not current occupancy"
            );
            assert!(tree
                .handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                })
                .contains(&UiAction::Clicked {
                    id: "composer_context_dismiss".into()
                }));
            state.composer_context_open = false;
            tree.set_root(root(&state, size));
            let closed = tree.layout(size);
            assert!(!closed
                .regions
                .iter()
                .any(|region| region.id == "popover_surface:composer_context_dismiss"));
            assert_eq!(tree.input_value("prompt"), Some(DRAFT));
            ctx.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn context_lists_every_category_in_one_level_without_scrolling() {
    for width in [360.0, 900.0] {
        for name in ["measured-30", "five-long-sources"] {
            let size = Size::new(width, 420.0);
            let mut state = fixture(name);
            state.composer_context_open = true;
            let (ctx, mut tree) = native_tree(root(&state, size), size);
            let frame = tree.layout(size);
            assert_popup_bounds(&frame, size);
            let text = visible_text(&frame, size);
            for label in SOURCES {
                assert!(text.iter().any(|text| text == label), "{label}: {text:?}");
            }
            assert!(text
                .iter()
                .any(|text| text == "30% · ~30.0K / 100.0K tokens"));
            assert!(text.iter().any(|text| text == "Copiar diagnóstico"));
            assert!(
                !text.iter().any(|text| text == "Detalles"),
                "there is no second level"
            );
            // Tools, MCP and subagents state no size; raw units stay in the dump.
            assert_eq!(text.iter().filter(|text| *text == "–").count(), 3);
            assert!(!text.iter().any(|text| text.contains("bytes")));
            if name == "measured-30" {
                // 8192, 4096 and 2048 bytes at four a token; the rest is conversation.
                for figure in ["~2.0K", "~1.0K", "~512", "~26.4K"] {
                    assert!(text.iter().any(|text| text == figure), "{figure}: {text:?}");
                }
                assert!(crate::composer_context_ui::diagnostic(&state.native_context)
                    .contains("Proyecto: 3 docs · 4096 bytes — "));
            }
            tree.scroll_to_end("composer_context_summary");
            tree.layout(size);
            assert_eq!(tree.scroll_offset("composer_context_summary"), 0.0);
            let copied = crate::composer_context_ui::diagnostic(&state.native_context);
            assert!(!text.join("").contains(DETAIL_MARKER));
            assert!(copied.contains(&format!("Identidad de la ruta: {DETAIL_MARKER} — ")));
            assert!(copied.contains("Herramientas: "));
            assert_eq!(tree.input_value("prompt"), Some(DRAFT));
            ctx.end_pass().textures_delta.clear();
        }
    }
}

/// The bar's painted pieces: its track and, left to right, its segments.
fn bar(frame: &Frame) -> (Rect, Vec<(Rect, openide_ui::Color)>) {
    let fills: Vec<_> = frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRect { rect, color, .. } if rect.height == 6.0 => {
                Some((*rect, *color))
            }
            _ => None,
        })
        .collect();
    let track = fills
        .iter()
        .find(|(_, color)| *color == theme::TRACK)
        .expect("the bar paints its track")
        .0;
    (
        track,
        fills
            .into_iter()
            .filter(|(_, color)| *color != theme::TRACK)
            .collect(),
    )
}

#[test]
fn context_bar_paints_one_segment_per_measured_category_inside_its_track() {
    let ink = theme::context_category_ink;
    for width in [360.0, 900.0] {
        for (name, inks, fills_track) in [
            (
                "measured-30",
                vec!["system", "project", "skills", "conversation"],
                false,
            ),
            // 95 000 of 100 000 tokens leave a twentieth of the track free.
            (
                "near-full-95",
                vec!["system", "project", "skills", "conversation"],
                false,
            ),
            // Without a window the measured segments share the whole track.
            (
                "unknown-window",
                vec!["system", "project", "skills", "conversation"],
                true,
            ),
            // Nothing measured: only what states its bytes, against the window.
            ("unknown", vec!["system", "project", "skills"], false),
            ("compacted", vec!["system", "project", "skills"], false),
            ("error", vec!["system", "project", "skills"], false),
        ] {
            // Tall enough that the seven rows and a status line need no scrollbar.
            let size = Size::new(width, 520.0);
            let mut state = fixture(name);
            state.composer_context_open = true;
            let (ctx, mut tree) = native_tree(root(&state, size), size);
            let frame = tree.layout(size);
            let (track, segments) = bar(&frame);
            let popup = region(&frame, "popover_surface:composer_context_dismiss");
            // The segments are sized for exactly the track the popover lays out.
            assert!((track.width - (popup.width - 32.0)).abs() < 0.1, "{track:?}");
            assert_eq!(
                segments.iter().map(|(_, color)| *color).collect::<Vec<_>>(),
                inks.iter().map(|id| ink(id)).collect::<Vec<_>>(),
                "{name}"
            );
            let painted: f32 = segments.iter().map(|(rect, _)| rect.width).sum();
            assert!(painted <= track.width + 0.1, "{name}: {painted}");
            assert_eq!((painted - track.width).abs() < 0.1, fills_track, "{name}");
            assert!(segments.iter().all(|(rect, _)| rect.width >= 2.0
                && rect.x >= track.x - 0.1
                && rect.x + rect.width <= track.x + track.width + 0.1));
            if name == "near-full-95" {
                assert!((painted / track.width - 0.95).abs() < 0.01, "{painted}");
            }
            let text = visible_text(&frame, size);
            let has = |value: &str| text.iter().any(|text| text == value);
            match name {
                "near-full-95" => assert!(has("95% · ~95.0K / 100.0K tokens")),
                "unknown-window" => {
                    assert!(has("30.0K tokens") && has("Límite del modelo sin verificar"));
                    assert!(!text.iter().any(|text| text.contains('%')));
                }
                "unknown" => assert!(has("Capacidad: 100.0K tokens") && !has("~26.4K")),
                _ => {}
            }
            assert!(
                !frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, .. } if rect.width == 5.0)),
                "{name} at {width}: content that fits paints no scrollbar thumb"
            );
            // Every legend row carries its 10 px swatch.
            let swatches = frame
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::FillRect { rect, radius, .. }
                    if rect.width == 10.0 && rect.height == 10.0 && *radius == 2.0))
                .count();
            assert_eq!(swatches, SOURCES.len(), "{name}");
            ctx.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn context_shows_one_status_line_only_when_the_measurement_needs_it() {
    const STATUSES: [&str; 5] = [
        "Límite del modelo sin verificar",
        "Aún no hay una respuesta medida",
        "Queda poco espacio. Puedes compactar para continuar.",
        "El resumen está guardado. El uso se medirá en la próxima respuesta.",
        "Consulta no disponible: No se puede verificar el journal de esta conversación",
    ];
    for (name, expected) in [
        ("measured-30", None),
        ("tiny", None),
        ("unknown", Some(STATUSES[1])),
        ("unknown-window", Some("Límite del modelo sin verificar")),
        ("near-full-95", Some(STATUSES[2])),
        ("compacted", Some(STATUSES[3])),
        ("error", Some(STATUSES[4])),
    ] {
        let size = Size::new(900.0, 420.0);
        let mut state = fixture(name);
        state.composer_context_open = true;
        let (ctx, mut tree) = native_tree(root(&state, size), size);
        let frame = tree.layout(size);
        let shown: Vec<_> = frame
            .text_layouts()
            .into_iter()
            .map(|block| block.source)
            .filter(|source| STATUSES.contains(&source.as_str()))
            .collect();
        assert_eq!(shown, expected.into_iter().collect::<Vec<_>>(), "{name}");
        let text = visible_text(&frame, size).join("\n");
        for gone in ["tokens disponibles", "reportada", "Sin medición"] {
            assert!(!text.contains(gone), "{name}: {gone}");
        }
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn compaction_action_is_scoped_keyboard_accessible_and_never_invented_at_home() {
    const ACTION: &str = "composer_context_compact:context-conversation";
    for width in [280.0, 360.0, 900.0] {
        let size = Size::new(width, 420.0);
        let mut state = fixture("ready-to-compact");
        state.composer_context_open = true;
        let (ctx, mut tree) = native_tree(root(&state, size), size);
        let frame = tree.layout(size);
        assert_popup_bounds(&frame, size);
        let compact = region(&frame, ACTION);
        let details = region(&frame, "composer_context_copy_diagnostic");
        let popup = region(&frame, "popover_surface:composer_context_dismiss");
        assert!(compact.width > 0.0 && compact.height >= 28.0);
        assert!(compact.x >= popup.x && compact.x + compact.width <= details.x);
        assert!(compact.y + compact.height <= popup.y + popup.height);
        assert!(visible_text(&frame, size)
            .iter()
            .any(|text| text == "Compactar"));
        activate(&mut tree, ACTION);
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));

        state
            .conversation_activity
            .begin_compaction("context-conversation", Path::new(&state.workspace));
        tree.set_root(root(&state, size));
        let busy = tree.layout(size);
        assert!(visible_text(&busy, size)
            .iter()
            .any(|text| text == "Compactando"));
        assert!(
            !tree.focus(ACTION),
            "disabled compaction cannot receive keyboard activation"
        );
        let actions = tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            ctrl: false,
            shift: false,
        });
        assert!(!actions.contains(&UiAction::Clicked { id: ACTION.into() }));
        activate(&mut tree, "composer_context_copy_diagnostic");

        state = fixture("unknown");
        state.composer_context_open = true;
        tree.set_root(root(&state, size));
        let home = tree.layout(size);
        assert!(!home
            .regions
            .iter()
            .any(|region| region.id.starts_with("composer_context_compact:")));
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn failed_context_keeps_its_error_visible_without_reusing_old_measurements() {
    // Seven categories and a wrapped error fit once the window allows the cap.
    let size = Size::new(360.0, 520.0);
    let mut state = fixture("measured-30");
    state.native_context.error =
        Some("No se puede verificar el journal de esta conversación".into());
    state.composer_context_open = true;
    let (ctx, mut tree) = native_tree(root(&state, size), size);
    let frame = tree.layout(size);
    let summary = visible_text(&frame, size).join("\n");
    assert!(summary.contains("Consulta no disponible"));
    assert!(!summary.contains("30%"));
    assert!(frame
        .commands
        .iter()
        .any(|command| matches!(command, DrawCommand::ProgressRing { fraction: None, .. })));
    assert_popup_bounds(&frame, size);
    let error = state.native_context.error.as_deref().unwrap();
    let block = frame
        .text_layouts()
        .into_iter()
        .find(|block| block.source.ends_with(error))
        .expect("the complete error is retained as selectable, wrapped text");
    assert!(!block.runs.is_empty());
    assert!(
        block.runs.iter().all(|run| run
            .visible
            .is_some_and(|visible| visible.width >= run.rect.width - 0.1
                && visible.height >= run.rect.height - 0.1)),
        "every error line must be visible without scrolling"
    );
    let painted = visible_text(&frame, size).join(" ");
    let normalized = painted.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains(error),
        "full error must be painted: {normalized}"
    );
    ctx.end_pass().textures_delta.clear();
}

fn export(frame: &Frame, directory: &Path, name: &str, size: Size) {
    let path = directory.join(format!(
        "context-{name}-{:.0}x{:.0}.svg",
        size.width, size.height
    ));
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

#[test]
#[ignore = "opt-in real-composer context SVG/PNG review with native fonts"]
fn export_context_visual_review() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/context-visual-qa");
    std::fs::create_dir_all(&directory).unwrap();
    for name in CASES {
        for width in [360.0, 900.0] {
            for height in [320.0, 420.0, 520.0] {
                let size = Size::new(width, height);
                let mut state = fixture(name);
                state.composer_context_open = true;
                let (ctx, mut tree) = native_tree(root(&state, size), size);
                let frame = tree.layout(size);
                assert_popup_bounds(&frame, size);
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::ProgressRing { fraction, .. } if *fraction == state.native_context.used_fraction())));
                export(&frame, &directory, name, size);
                ctx.end_pass().textures_delta.clear();
            }
        }
    }
}
