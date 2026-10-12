//! Opt-in rendering of the provider page's real account rows and refresh action.
//! Fixtures contain presentation data only, without credentials or HTTP calls.

use super::{control_palette, page, refresh, tests::visible_fixture};
use crate::view::ViewState;
use eframe::egui;
use openide_ui::{
    CornerRadii, DrawCommand, Element, FontFamily, FontMetrics, Frame, HitKind, Point, Rect, Size,
    SpinnerVariant, UiAction, UiEvent, UiTree,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const VIEWPORTS: [Size; 2] = [Size::new(900.0, 330.0), Size::new(360.0, 400.0)];
const ACCOUNT_ID: &str = "provider_account:nvidia";
const DISCONNECT_ID: &str = "provider_disconnect:nvidia";

fn fixture() -> ViewState {
    let mut state = visible_fixture();
    state.catalog_browse_provider = "nvidia".into();
    state
}

fn controls(state: &ViewState, viewport: Size) -> Element {
    // The account group is the final section of a connected provider's page.
    // Use that production section rather than restating its row definitions.
    let account = page(state, viewport.width < 640.0, viewport.width - 24.0)
        .pop()
        .expect("connected provider's account group");
    Element::column(vec![
        Element::row(vec![Element::spacer().flex(1.0), refresh(state)]),
        account,
    ])
    .gap(12.0)
    .padding(12.0)
    .background(crate::theme::BACKGROUND)
}

fn native_tree(state: &ViewState, viewport: Size) -> (egui::Context, UiTree) {
    native_tree_for(controls(state, viewport))
}

fn native_tree_for(root: Element) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.begin_pass(egui::RawInput::default());
    let measure = context.clone();
    let mut tree = UiTree::new(root);
    let mono_font = egui::FontId::monospace(14.0);
    tree.set_metrics(context.fonts_mut(|fonts| FontMetrics {
        char_width: fonts.glyph_width(&mono_font, 'M'),
        line_height: fonts.row_height(&mono_font),
    }));
    let sans_font = egui::FontId::proportional(14.0);
    tree.set_sans_metrics(context.fonts_mut(|fonts| {
        FontMetrics {
            char_width: "abcdefghijklmnopqrstuvwxyz"
                .chars()
                .map(|character| fonts.glyph_width(&sans_font, character))
                .sum::<f32>()
                / 26.0,
            line_height: fonts.row_height(&sans_font),
        }
    }));
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
    (context, tree)
}

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing provider control {id}"))
        .rect
}

fn in_viewport(rect: Rect, viewport: Size) -> bool {
    rect.x >= 0.0
        && rect.y >= 0.0
        && rect.x + rect.width <= viewport.width + 0.1
        && rect.y + rect.height <= viewport.height + 0.1
}

fn rounded_account(frame: &Frame, bounds: Rect, color: openide_ui::Color) {
    assert!(
        frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillCornerRect { rect, color: ink, radii }
                if *rect == bounds && *ink == color && *radii == CornerRadii::top(8.0)
        )),
        "account row paints the group's round top corners"
    );
}

fn spinner(frame: &Frame) -> (Rect, f32) {
    frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Spinner {
                rect,
                angle,
                variant: SpinnerVariant::Circle,
                ..
            } => Some((*rect, *angle)),
            _ => None,
        })
        .expect("native circular refresh spinner")
}

fn assert_controls(frame: &Frame, viewport: Size, loading: bool) {
    assert_eq!(rect(frame, DISCONNECT_ID).height, 28.0);
    if viewport.width >= 900.0 {
        assert!(
            rect(frame, ACCOUNT_ID).height <= 56.0,
            "single-line account copy stays compact"
        );
    }
    for id in [ACCOUNT_ID, DISCONNECT_ID] {
        assert!(
            in_viewport(rect(frame, id), viewport),
            "{id} stays in viewport"
        );
    }
    for label in [
        "Cuenta",
        "Administrar cuenta",
        "Desconectar",
        "Actualizar modelos",
    ] {
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == label
            )),
            "actual provider label {label}"
        );
    }
    if loading {
        assert!(
            !frame
                .regions
                .iter()
                .any(|region| region.id == "catalog_refresh")
        );
        let (spinner_bounds, _) = spinner(frame);
        assert_eq!(spinner_bounds.width, 16.0);
        assert_eq!(spinner_bounds.height, 16.0);
        assert!(in_viewport(spinner_bounds, viewport));
        assert!(spinner_bounds.y + spinner_bounds.height < rect(frame, ACCOUNT_ID).y);
    } else {
        assert!(in_viewport(rect(frame, "catalog_refresh"), viewport));
        assert_eq!(rect(frame, "catalog_refresh").height, 28.0);
        assert!(
            !frame
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Spinner { .. }))
        );
    }
}

#[test]
fn compact_model_rows_keep_native_text_and_actions_separate() {
    let state = fixture();
    for width in [280.0, 360.0, 900.0] {
        let (context, mut tree) = native_tree_for(super::model_list(&state, "nvidia", width));
        let viewport = Size::new(width, 500.0);
        let frame = tree.layout(viewport);
        let info = frame
            .regions
            .iter()
            .find(|region| region.id.starts_with("catalog_model_details:"))
            .expect("fixture's model details action");
        assert_eq!(info.rect.height, 28.0);
        assert!(in_viewport(info.rect, viewport));
        let first_row_height = if width < 440.0 { 88.0 } else { 56.0 };
        // Truncated labels are direct text draw commands, not selectable text
        // blocks. Measure their actual native glyphs, including any ellipsis.
        let labels = frame
            .commands
            .iter()
            .filter_map(|command| {
                let DrawCommand::Text {
                    position,
                    text,
                    font_size,
                    font_family,
                    ..
                } = command
                else {
                    return None;
                };
                if position.y >= first_row_height
                    || matches!(text.as_str(), "Usar" | "En uso")
                    || !matches!(
                        (*font_size, *font_family),
                        (12.0, FontFamily::Sans) | (11.0, FontFamily::Monospace)
                    )
                {
                    return None;
                }
                let size = context.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            text.clone(),
                            egui::FontId::new(
                                *font_size,
                                match font_family {
                                    FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                                    FontFamily::Monospace => egui::FontFamily::Monospace,
                                },
                            ),
                            egui::Color32::WHITE,
                        )
                        .size()
                });
                Some(Rect::new(position.x, position.y, size.x, size.y))
            })
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 2, "model name and identifier stay visible");
        for label in labels {
            if width < 440.0 {
                assert!(
                    label.y + label.height <= info.rect.y + 0.1,
                    "model text must finish before the stacked actions: {:?} vs {:?}",
                    label,
                    info.rect
                );
            } else {
                assert!(
                    label.x + label.width <= info.rect.x + 0.1,
                    "model text must end before the actions"
                );
            }
        }
        let point = Point::new(
            info.rect.x + info.rect.width / 2.0,
            info.rect.y + info.rect.height / 2.0,
        );
        let action = info.id.clone();
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id: action }]
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn native_provider_account_hover_and_press_stay_rounded_with_a_passive_refresh_spinner() {
    for viewport in VIEWPORTS {
        let mut state = fixture();
        let (context, mut tree) = native_tree(&state, viewport);
        let start = Instant::now();
        let idle = tree.layout_at(viewport, start);
        assert_controls(&idle, viewport, false);
        let account = rect(&idle, ACCOUNT_ID);
        let point = Point::new(account.x + 1.0, account.y + 1.0);
        tree.handle_event_at(UiEvent::PointerMove(point), start);
        tree.layout_at(viewport, start);
        let hovered = tree.layout_at(viewport, start + Duration::from_millis(250));
        rounded_account(&hovered, account, control_palette().hover);
        tree.handle_event_at(
            UiEvent::PointerDown(point),
            start + Duration::from_millis(250),
        );
        tree.layout_at(viewport, start + Duration::from_millis(250));
        let pressed = tree.layout_at(viewport, start + Duration::from_millis(500));
        rounded_account(&pressed, account, control_palette().selected);
        assert_eq!(
            tree.handle_event_at(
                UiEvent::PointerUp(point),
                start + Duration::from_millis(500)
            ),
            vec![UiAction::Clicked {
                id: ACCOUNT_ID.into()
            }]
        );
        state.catalog_refreshing = true;
        tree.set_root(controls(&state, viewport));
        let loading_start = start + Duration::from_secs(1);
        let loading = tree.layout_at(viewport, loading_start);
        assert_controls(&loading, viewport, true);
        assert!(!tree.focus("catalog_refresh"));
        assert!(tree.animation_repaint_after().is_some());
        let next = tree.layout_at(viewport, loading_start + Duration::from_millis(75));
        assert_ne!(spinner(&loading).1, spinner(&next).1);
        assert_eq!(rect(&next, ACCOUNT_ID), account);
        context.end_pass().textures_delta.clear();
    }
}

fn export(frame: &Frame, directory: &Path, name: &str, viewport: Size) {
    let svg_path = directory.join(format!("{name}-{:.0}.svg", viewport.width));
    super::super::visual_qa::write_native_frame_svg(
        frame,
        &svg_path,
        viewport.width,
        viewport.height,
    )
    .unwrap();
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    let parsed =
        resvg::usvg::Tree::from_str(&svg, &super::super::visual_qa::native_svg_options()).unwrap();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(viewport.width as u32, viewport.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(svg_path.with_extension("png")).unwrap();
}

#[test]
#[ignore = "opt-in native provider hover, press and loading SVG/PNG review"]
fn export_provider_controls_visual_review() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("OPENIDE_PROVIDER_CONTROLS_VISUAL_DIR")
            .unwrap_or_else(|| "/tmp/openade-provider-controls-review".into()),
    );
    std::fs::create_dir_all(&directory).unwrap();
    for viewport in VIEWPORTS {
        let mut state = fixture();
        let (context, mut tree) = native_tree(&state, viewport);
        let start = Instant::now();
        let idle = tree.layout_at(viewport, start);
        assert_controls(&idle, viewport, false);
        export(&idle, &directory, "account-idle", viewport);
        let account = rect(&idle, ACCOUNT_ID);
        let point = Point::new(account.x + 1.0, account.y + 1.0);
        tree.handle_event_at(UiEvent::PointerMove(point), start);
        tree.layout_at(viewport, start);
        let hovered = tree.layout_at(viewport, start + Duration::from_millis(250));
        rounded_account(&hovered, account, control_palette().hover);
        export(&hovered, &directory, "account-hover", viewport);
        tree.handle_event_at(
            UiEvent::PointerDown(point),
            start + Duration::from_millis(250),
        );
        tree.layout_at(viewport, start + Duration::from_millis(250));
        let pressed = tree.layout_at(viewport, start + Duration::from_millis(500));
        rounded_account(&pressed, account, control_palette().selected);
        export(&pressed, &directory, "account-pressed", viewport);
        tree.handle_event_at(
            UiEvent::PointerUp(point),
            start + Duration::from_millis(500),
        );
        tree.handle_event_at(
            UiEvent::PointerMove(Point::new(0.0, 0.0)),
            start + Duration::from_millis(500),
        );
        state.catalog_refreshing = true;
        tree.set_root(controls(&state, viewport));
        let loading_start = start + Duration::from_secs(1);
        let mut angles = Vec::new();
        for index in 0..12 {
            let frame = tree.layout_at(viewport, loading_start + Duration::from_millis(index * 75));
            assert_controls(&frame, viewport, true);
            angles.push(spinner(&frame).1);
            if index == 0 {
                export(&frame, &directory, "account-refresh-loading", viewport);
            }
            export(
                &frame,
                &directory,
                &format!("account-refresh-loading-frame-{index:02}"),
                viewport,
            );
        }
        assert!(angles.windows(2).all(|pair| pair[0] != pair[1]));
        context.end_pass().textures_delta.clear();
    }
}
