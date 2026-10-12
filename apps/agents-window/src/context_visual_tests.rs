//! Real-composer context review with synthetic usage and the application's fonts.
//! No provider, persisted conversation, credentials, or user documents are read.

use crate::composer_ui::{build_composer, ComposerProps};
use crate::native_context_ui::{
    NativeContextCategory, NativeContextMeasurementStatus, NativeContextRow, NativeContextSection,
    NativeContextUiState,
};
use crate::composer_context_ui::CONTEXT_CARD;
use crate::{theme, view};
use eframe::egui;
use openide_core::ModelTokenUsage;
use openide_ui::{
    Color, ContextWindow, DrawCommand, Element, FontFamily, Frame, Key, Point, Rect, Size,
    UiAction, UiEvent, UiTree,
};
use std::path::Path;
use std::time::{Duration, Instant};

const CASES: [&str; 13] = [
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
    "compaction-unavailable",
    "category-in-focus",
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
const UNKNOWN_WINDOW: &str =
    "Ventana no publicada: context-review-model";
/// A route as long as the ones providers publish, for the card's badge.
const LONG_MODEL: &str = "fixture-account/models/context-review-2026-10-preview";
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
    state.model = if name == "five-long-sources" {
        LONG_MODEL
    } else {
        "context-review-model"
    }
    .into();
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
        "category-in-focus" => context.hovered_category = Some("conversation".into()),
        _ => {}
    }
    context.remaining_percent = context
        .used_fraction()
        .map(|fraction| ((1.0 - fraction) * 100.0).round() as u8);
    let values = if name == "five-long-sources" {
        [
            "384296 bytes de instrucciones combinadas y verificadas",
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
    // The sizes the first three values state; the system copy holds the others.
    let sizes = if name == "five-long-sources" {
        [384_296, 96_512, 188_765]
    } else {
        [8_192, 4_096, 2_048]
    };
    for (index, ((id, label), value)) in [
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
        .enumerate()
    {
        let mut category = NativeContextCategory::new(id, label, value,
            "Observación con unidades propias. No representa una atribución de tokens ni se suma al contexto medido.");
        category.bytes = sizes.get(index).copied();
        context.breakdown.push(category);
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
    if matches!(name, "ready-to-compact" | "compacting" | "compaction-unavailable") {
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
        if name == "compaction-unavailable" {
            state.pending_permissions.push(view::PermissionView {
                request_id: "approval".into(),
                tool_name: "run_command".into(),
                arguments: "{}".into(),
            });
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
            let expected = state
                .native_context
                .occupancy()
                .map(|occupancy| occupancy.fraction());
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
            // Tall enough for the card, its seven rows and the actions under it.
            let size = Size::new(width, 640.0);
            let mut state = fixture(name);
            state.composer_context_open = true;
            let (ctx, mut tree) = native_tree(root(&state, size), size);
            let frame = tree.layout(size);
            assert_popup_bounds(&frame, size);
            let text = visible_text(&frame, size);
            let shown = |value: &str| text.iter().filter(|text| *text == value).count();
            for label in SOURCES {
                assert_eq!(shown(label), 1, "{label}: {text:?}");
            }
            // The figure, its caption, the percentage and what is left of the window.
            for value in ["30K", "de 100K tokens", "30%", "70K libres", "Copiar diagnóstico"] {
                assert_eq!(shown(value), 1, "{value}: {text:?}");
            }
            assert_eq!(shown("Detalles"), 0, "there is no second level");
            // Raw units stay in the dump.
            assert!(!text.iter().any(|text| text.contains("bytes")));
            if name == "measured-30" {
                // The 8192-byte system copy holds the project's 4096 and the
                // skills' 2048: 2048, 4096 and 2048 bytes at four a token, and
                // the rest of the 30 000 measured is conversation. Tools, MCP
                // and subagents state no size and count as zero.
                for (figure, times) in [("512", 2), ("1K", 1), ("28K", 1), ("0", 3)] {
                    assert_eq!(shown(figure), times, "{figure}: {text:?}");
                }
                assert!(crate::composer_context_ui::diagnostic(&state.native_context)
                    .contains("Proyecto: 3 docs · 4096 bytes — "));
            } else {
                // Sizes that add up to 96 074 tokens are brought down to the
                // 30 000 measured, so the figure stays the one the ring shows.
                for (figure, times) in [("7.7K", 1), ("7.5K", 1), ("14.7K", 1), ("0", 4)] {
                    assert_eq!(shown(figure), times, "{figure}: {text:?}");
                }
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

/// The card's field of cells inside the popover, in reading order.
fn cells(frame: &Frame) -> Vec<(Rect, Color)> {
    let popup = region(frame, "popover_surface:composer_context_dismiss");
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRect { rect, color, .. }
                if rect.width == rect.height
                    && rect.width < 8.0
                    && popup.contains(Point::new(rect.x, rect.y)) =>
            {
                Some((*rect, *color))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn context_card_shares_its_cells_among_the_measured_categories() {
    let ink = theme::context_category_ink;
    let empty = theme::menu_palette().hover;
    for width in [360.0, 900.0] {
        for (name, inks, fills_field) in [
            (
                "measured-30",
                vec!["system", "project", "skills", "conversation"],
                false,
            ),
            // 95 000 of 100 000 tokens leave a twentieth of the field empty.
            (
                "near-full-95",
                vec!["system", "project", "skills", "conversation"],
                false,
            ),
            // Without a window the measured shares fill the field between them.
            (
                "unknown-window",
                vec!["system", "project", "skills", "conversation"],
                true,
            ),
            // Nothing measured: only what states its bytes, against the window.
            ("unknown", vec!["system", "project", "skills"], false),
            ("compacted", vec!["system", "project", "skills"], false),
            // A failed query has no occupancy, so no window to set them against.
            ("error", vec!["system", "project", "skills"], true),
        ] {
            let size = Size::new(width, 640.0);
            let mut state = fixture(name);
            state.composer_context_open = true;
            let (ctx, mut tree) = native_tree(root(&state, size), size);
            let frame = tree.layout(size);
            let cells = cells(&frame);
            let popup = region(&frame, "popover_surface:composer_context_dismiss");
            // The component's own field: 36 by 6 cells inside the card's inset.
            assert_eq!(cells.len(), 216, "{name}");
            assert!(cells.iter().all(|(rect, _)| rect.x >= popup.x + 16.0
                && rect.x + rect.width <= popup.x + popup.width - 16.0));
            let mut painted: Vec<_> = cells
                .iter()
                .map(|(_, color)| *color)
                .filter(|color| *color != empty)
                .collect();
            painted.dedup();
            assert_eq!(
                painted,
                inks.iter().map(|id| ink(id)).collect::<Vec<_>>(),
                "{name}"
            );
            let free = cells.iter().filter(|(_, color)| *color == empty).count();
            assert_eq!(free == 0, fills_field, "{name}: {free}");
            if name == "near-full-95" {
                assert!((free as f32 / 216.0 - 0.05).abs() < 0.01, "{free}");
            }
            let text = visible_text(&frame, size);
            let has = |value: &str| text.iter().any(|text| text == value);
            match name {
                "near-full-95" => {
                    assert!(has("95K") && has("de 100K tokens") && has("95%") && has("5K libres"))
                }
                "unknown-window" => {
                    assert!(has("30K") && has("tokens") && has("Ventana desconocida"));
                    assert!(has(UNKNOWN_WINDOW));
                    assert!(!text.iter().any(|text| text.contains('%')));
                }
                // Nothing measured yet: the sized instructions alone, as an estimate.
                "unknown" => assert!(
                    has("2K")
                        && has("de 100K tokens")
                        && has("Estimado: aún no hay una respuesta medida.")
                ),
                "compacted" => assert!(has("Compactada")),
                "error" => {
                    assert!(has("2K") && has("tokens") && has("Ventana desconocida"));
                    assert!(!text.iter().any(|text| text.contains('%')));
                }
                _ => {}
            }
            assert!(
                !frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, .. } if rect.width == 5.0)),
                "{name} at {width}: content that fits paints no scrollbar thumb"
            );
            // Every row carries its 8 px swatch.
            let swatches = frame
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::FillRect { rect, radius, .. }
                    if rect.width == 8.0 && rect.height == 8.0 && *radius == 2.0))
                .count();
            assert_eq!(swatches, SOURCES.len(), "{name}");
            ctx.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn context_figures_count_down_to_a_smaller_conversation() {
    let size = Size::new(360.0, 640.0);
    let mut state = fixture("measured-30");
    state.composer_context_open = true;
    let (ctx, mut tree) = native_tree(root(&state, size), size);
    tree.set_reduced_motion(false);
    let start = Instant::now() - Duration::from_secs(10);
    let roll = Duration::from_millis(u64::from(openide_ui::tokens().motion.move_ms) * 3);
    let conversation = theme::context_category_ink("conversation");
    let taken = |frame: &Frame| cells(frame).iter().filter(|(_, color)| *color == conversation).count();
    let has = |frame: &Frame, value: &str| visible_text(frame, size).iter().any(|text| text == value);
    tree.layout_at(size, start);
    // The popover has opened by now.
    let opened = start + Duration::from_secs(2);
    let frame = tree.layout_at(size, opened);
    assert!(has(&frame, "30K") && has(&frame, "28K") && has(&frame, "70K libres"));
    assert_eq!(taken(&frame), 60);
    // A response that reports 12 000 tokens leaves 9952 to the conversation.
    state.native_context.last_usage = Some(usage(9_000, 3_000));
    tree.set_root(root(&state, size));
    tree.layout_at(size, opened);
    let frame = tree.layout_at(size, opened + roll / 3);
    // Midway the figures read values in between and the share has given cells up.
    assert!(!has(&frame, "30K") && !has(&frame, "12K"));
    assert!(!has(&frame, "28K") && !has(&frame, "10K"));
    let midway = taken(&frame);
    assert!(midway < 60 && midway > 21, "{midway}");
    let frame = tree.layout_at(size, opened + roll * 2);
    assert!(has(&frame, "12K") && has(&frame, "10K") && has(&frame, "12%"));
    assert!(has(&frame, "88K libres"));
    assert_eq!(taken(&frame), 21);
    ctx.end_pass().textures_delta.clear();
}

#[test]
fn a_category_under_the_pointer_takes_the_figure_and_the_status_line() {
    let size = Size::new(360.0, 640.0);
    let mut state = fixture("measured-30");
    state.composer_context_open = true;
    let (ctx, mut tree) = native_tree(root(&state, size), size);
    let frame = tree.layout(size);
    let row = region(&frame, &format!("{CONTEXT_CARD}:share:skills"));
    let moved = Instant::now();
    tree.handle_event_at(
        UiEvent::PointerMove(Point::new(row.x + 4.0, row.y + row.height / 2.0)),
        moved,
    );
    // The app mirrors the control under the pointer into the usage it draws.
    let hovered = tree
        .hovered_id()
        .and_then(|id| ContextWindow::share_under(CONTEXT_CARD, id))
        .map(str::to_owned);
    assert_eq!(hovered.as_deref(), Some("skills"));
    state.native_context.hovered_category = hovered;
    tree.set_root(root(&state, size));
    // Long enough under the pointer for the row's hint to open.
    let frame = tree.layout_at(size, moved + Duration::from_secs(1));
    let text = visible_text(&frame, size);
    let shown = |value: &str| text.iter().filter(|text| *text == value).count();
    // The figure reads the share's count beside its own row and the system's.
    assert_eq!(shown("512"), 3, "{text:?}");
    assert_eq!(shown("de 100K tokens"), 0);
    assert_eq!(shown("Skills · 1% de la ventana"), 1, "{text:?}");
    assert_eq!(shown("30%"), 1, "the percentage stays the window's");
    // The hint states what was measured, in the app's own units.
    assert_eq!(shown("4 locales · 2048 bytes"), 1, "{text:?}");
    // On the field the share keeps its ink and the others recede.
    let inks: Vec<_> = cells(&frame).into_iter().map(|(_, color)| color).collect();
    let full = |id: &str| inks.iter().filter(|color| **color == theme::context_category_ink(id)).count();
    assert_eq!((full("skills"), full("system"), full("conversation")), (1, 0, 0));
    ctx.end_pass().textures_delta.clear();
}

#[test]
fn context_shows_one_status_line_only_when_the_measurement_needs_it() {
    const STATUSES: [&str; 5] = [
        UNKNOWN_WINDOW,
        "Estimado: aún no hay una respuesta medida.",
        "Queda poco espacio. Puedes compactar para continuar.",
        "El resumen está guardado. El uso se medirá en la próxima respuesta.",
        "Consulta no disponible: No se puede verificar el journal de esta conversación",
    ];
    for (name, expected) in [
        ("measured-30", None),
        ("tiny", None),
        ("unknown", Some(STATUSES[1])),
        ("unknown-window", Some(UNKNOWN_WINDOW)),
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
    // The card names its control; activating it dispatches the scoped action.
    const ACTION: &str = "composer_context_compact:context-conversation";
    let control = format!("{CONTEXT_CARD}:action:conversation");
    for width in [280.0, 360.0, 900.0] {
        let size = Size::new(width, 640.0);
        let mut state = fixture("ready-to-compact");
        state.composer_context_open = true;
        let (ctx, mut tree) = native_tree(root(&state, size), size);
        let frame = tree.layout(size);
        assert_popup_bounds(&frame, size);
        let compact = region(&frame, &control);
        let row = region(&frame, &format!("{CONTEXT_CARD}:share:conversation"));
        let details = region(&frame, "composer_context_copy_diagnostic");
        let popup = region(&frame, "popover_surface:composer_context_dismiss");
        assert!(compact.width > 0.0 && compact.height >= 28.0);
        // In the conversation's row, after its label and inside the card's inset.
        assert!(compact.y == row.y && compact.x >= row.x + row.width);
        assert!(compact.x + compact.width <= popup.x + popup.width - 16.0);
        assert!(compact.y + compact.height <= details.y);
        // A narrow card keeps the action's icon and leaves its word out.
        let roomy = width >= 360.0;
        let text = visible_text(&frame, size);
        assert_eq!(text.iter().any(|text| text == "Compactar"), roomy);
        assert!(text.iter().any(|text| text == "70K libres"));
        assert!(tree.focus(&control), "focus {control}");
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked { id: ACTION.into() }]
        );
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));

        state
            .conversation_activity
            .begin_compaction("context-conversation", Path::new(&state.workspace));
        tree.set_root(root(&state, size));
        let busy = tree.layout(size);
        let text = visible_text(&busy, size);
        assert_eq!(text.iter().any(|text| text == "Compactando"), roomy);
        assert!(text.iter().any(|text| text == "Compactando…"));
        assert!(
            !tree.focus(&control),
            "compaction in progress cannot receive keyboard activation"
        );
        let actions = tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            ctrl: false,
            shift: false,
        });
        assert!(!actions.contains(&UiAction::Clicked { id: ACTION.into() }));
        activate(&mut tree, "composer_context_copy_diagnostic");

        // Unavailable: nothing to activate, and the status line says why.
        state = fixture("compaction-unavailable");
        state.composer_context_open = true;
        tree.set_root(root(&state, size));
        let unavailable = tree.layout(size);
        assert!(!unavailable.regions.iter().any(|region| region.id == control));
        assert!(visible_text(&unavailable, size)
            .iter()
            .any(|text| text.starts_with("Resuelve los permisos")));

        state = fixture("unknown");
        state.composer_context_open = true;
        tree.set_root(root(&state, size));
        let home = tree.layout(size);
        assert!(!home.regions.iter().any(|region| region.id == control));
        assert!(!tree.focus(&control));
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
        for width in [280.0, 360.0, 900.0] {
            for height in [320.0, 420.0, 640.0] {
                let size = Size::new(width, height);
                let mut state = fixture(name);
                state.composer_context_open = true;
                let (ctx, mut tree) = native_tree(root(&state, size), size);
                let mut frame = tree.layout(size);
                assert_popup_bounds(&frame, size);
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::ProgressRing { fraction, .. } if *fraction == state.native_context.occupancy().map(|occupancy| occupancy.fraction()))));
                // The category in focus is under the pointer, long enough for its hint.
                let row = format!("{CONTEXT_CARD}:share:conversation");
                if let Some(row) = frame.regions.iter().find(|region| name == "category-in-focus" && region.id == row) {
                    let moved = Instant::now();
                    tree.handle_event_at(UiEvent::PointerMove(Point::new(row.rect.x + 4.0, row.rect.y + 4.0)), moved);
                    frame = tree.layout_at(size, moved + Duration::from_secs(1));
                }
                export(&frame, &directory, name, size);
                ctx.end_pass().textures_delta.clear();
            }
        }
    }
}

/// The ring and its popover in every state the meter has: empty, a provider
/// count, an estimate, the warning, full, and a model with no known window.
#[test]
#[ignore = "export the context ring states under docs/ui-separation/shots/model-metadata"]
fn export_context_ring_states_visual_review() {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ui-separation/shots/model-metadata");
    std::fs::create_dir_all(&directory).unwrap();
    for (name, measured, appended, label) in [
        ("ring-0", Some((0, 0)), 0, Some("0%")),
        ("ring-exact-42", Some((40_000, 2_000)), 0, Some("42%")),
        ("ring-estimated-42", Some((30_000, 2_000)), 40_000, Some("~42%")),
        ("ring-warning-85", Some((80_000, 5_000)), 0, Some("85%")),
        ("ring-full-100", Some((95_000, 5_000)), 0, Some("100%")),
        ("ring-unknown-window", None, 0, None),
    ] {
        for width in [360.0, 900.0] {
            let size = Size::new(width, 420.0);
            let mut state = fixture(if measured.is_some() { "measured-30" } else { "unknown-window" });
            if let Some((input, output)) = measured {
                state.native_context.last_usage = Some(usage(input, output));
            }
            state.native_context.appended_bytes = appended;
            state.native_context.window_source = "Codex CLI";
            state.composer_context_open = true;
            let (ctx, mut tree) = native_tree(root(&state, size), size);
            let frame = tree.layout(size);
            let occupancy = state.native_context.occupancy();
            assert_eq!(occupancy.map(|occupancy| occupancy.label()).as_deref(), label, "{name}");
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::ProgressRing { fraction, .. }
                    if *fraction == occupancy.map(|occupancy| occupancy.fraction()))));
            let text = visible_text(&frame, size);
            // With a ring that has a figure the card states a percentage too;
            // it rounds, so under 1% it reads "0%" where the ring says "<1%".
            if label.is_some() {
                assert!(text.iter().any(|text| text.ends_with('%')), "{name}: {text:?}");
            } else {
                assert!(text.iter().all(|text| !text.contains('%')), "{text:?}");
            }
            export(&frame, &directory, name, size);
            std::fs::remove_file(directory.join(format!("context-{name}-{width:.0}x420.svg"))).unwrap();
            ctx.end_pass().textures_delta.clear();
        }
    }
}
