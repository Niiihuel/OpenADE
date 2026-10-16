//! A live native diff sample: the same syntax, fonts and change rendering used
//! by real code reviews, with no additional editor controls or simulated pixels.

use crate::{appearance_preferences::AppearancePreferences, theme};
use openide_ui::{
    CodeDiff, CodeDiffLine, CodeDiffLineKind, CodeLanguage, Component, Element, Text, TextRole,
};
use std::sync::{Arc, OnceLock};

const PREVIEW_ID: &str = "appearance_typography_preview";

pub(crate) fn typography_preview(preferences: &AppearancePreferences, compact: bool) -> Element {
    static LINES: OnceLock<Arc<[CodeDiffLine]>> = OnceLock::new();
    let lines = LINES.get_or_init(|| {
        vec![
            CodeDiffLine {
                kind: CodeDiffLineKind::Context,
                old_line: Some(1),
                new_line: Some(1),
                text: "let a = 2;".into(),
            },
            CodeDiffLine {
                kind: CodeDiffLineKind::Removed,
                old_line: Some(2),
                new_line: None,
                text: "let sum = a + 1;".into(),
            },
            CodeDiffLine {
                kind: CodeDiffLineKind::Added,
                old_line: None,
                new_line: Some(2),
                text: "let sum = a + 2;".into(),
            },
        ]
        .into()
    });
    // The app zooms its entire frame by UI size / 13. Cancel that factor so
    // preview code, like the editor, retains the requested physical code size.
    let font_size =
        f32::from(preferences.code_font_size) * 13.0 / f32::from(preferences.ui_font_size);
    let diff = CodeDiff::new(PREVIEW_ID, lines.clone())
        .language(CodeLanguage::Rust)
        .font_size(font_size)
        .height(lines.len() as f32 * 20.0 * font_size / 13.0 + 16.0)
        .diff_backgrounds(preferences.themed_diff_backgrounds)
        .palette(theme::code_palette())
        .render()
        .radius(openide_ui::tokens().radius.surface)
        .clip_children();
    Element::column(vec![
        Text::new("Vista previa de código", theme::card_palette())
            .role(TextRole::Caption)
            .muted()
            .render(),
        diff,
    ])
    .gap(8.0)
    .padding(if compact { 10.0 } else { 12.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;
    use openide_ui::{DrawCommand, FontFamily, Frame, Size, UiTree};
    use std::time::{Duration, Instant};

    fn native_frame(preferences: &AppearancePreferences, width: f32) -> (egui::Context, Frame) {
        let context = egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(Default::default());
        assert!(!crate::fonts::update(&context, preferences));
        context.end_pass().textures_delta.clear();
        context.begin_pass(Default::default());
        assert!(crate::fonts::update(&context, preferences));
        let mut tree = UiTree::new(typography_preview(preferences, width < 640.0));
        tree.set_reduced_motion(true);
        limn_egui::attach_fonts(&mut tree, &context);
        let viewport = Size::new(width, 400.0);
        let mut frame = tree.layout(viewport);
        assert!(
            tree.animation_repaint_after().is_some(),
            "the first native diff frame must request another paint while syntax runs"
        );
        // Syntax is analysed on a worker. Mirror the app's scheduled paints,
        // rather than mistaking the initial plain frame for the final preview.
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(delay) = tree.animation_repaint_after() {
            assert!(
                Instant::now() < deadline,
                "native preview syntax did not finish: {:?}",
                frame.commands
            );
            std::thread::sleep(delay.clamp(Duration::from_millis(1), Duration::from_millis(16)));
            frame = tree.layout(viewport);
        }
        assert!(
            !tree.focus(PREVIEW_ID),
            "a sample must not add a code-editing tab stop"
        );
        (context, frame)
    }

    #[test]
    fn preview_paints_real_monospace_syntax_at_the_selected_independent_code_size() {
        for (ui_font_size, code_font_size) in [(7, 22), (13, 12), (38, 10)] {
            let preferences = AppearancePreferences {
                ui_font_size,
                code_font_size,
                ..Default::default()
            };
            for width in [240.0, 360.0, 900.0] {
                let (context, frame) = native_frame(&preferences, width);
                let expected = f32::from(code_font_size) * 13.0 / f32::from(ui_font_size);
                let code = frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text {
                            text,
                            font_size,
                            font_family: FontFamily::Monospace,
                            color,
                            ..
                        } => Some((text, font_size, color)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert!(!code.is_empty());
                assert!(
                    code.iter()
                        .all(|(_, size, _)| (**size - expected).abs() < 0.001)
                );
                assert!(
                    code.iter()
                        .any(|(text, _, color)| text.as_str() == "let"
                            && **color != theme::FOREGROUND),
                    "native Rust syntax must be highlighted: {code:?}"
                );
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == PREVIEW_ID)
                    .unwrap()
                    .rect;
                assert!(region.x >= 0.0 && region.x + region.width <= width);
                assert!(
                    region.height < 210.0,
                    "only three native diff rows, including at large code sizes"
                );
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn preview_uses_the_installed_code_family_for_native_measurement() {
        let preferences = AppearancePreferences {
            code_font_family: crate::fonts::families_for("code")
                .first()
                .cloned()
                .unwrap_or_default(),
            code_font_size: 18,
            ..Default::default()
        };
        let (context, frame) = native_frame(&preferences, 900.0);
        if !preferences.code_font_family.is_empty() {
            context.fonts(|fonts| {
                assert_eq!(
                    fonts.definitions().families[&egui::FontFamily::Monospace][0],
                    format!("openade:{}:400", preferences.code_font_family),
                )
            });
        }
        let drawn = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text {
                    text,
                    position,
                    font_family: FontFamily::Monospace,
                    ..
                } => Some((text.as_str(), *position)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let (_, keyword) = drawn
            .iter()
            .find(|(text, _)| *text == "let")
            .unwrap_or_else(|| panic!("native keyword span is missing: {drawn:?}"));
        let (_, next) = drawn
            .iter()
            .filter(|(_, position)| (position.y - keyword.y).abs() < 0.01 && position.x > keyword.x)
            .min_by(|(_, a), (_, b)| a.x.total_cmp(&b.x))
            .unwrap();
        let expected = context.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    "let".into(),
                    egui::FontId::monospace(18.0),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        });
        assert!(
            (next.x - keyword.x - expected).abs() < 0.1,
            "syntax advances must use the active font's measured width"
        );
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn preview_diff_background_switch_keeps_real_added_and_removed_markers() {
        for enabled in [false, true] {
            let preferences = AppearancePreferences {
                themed_diff_backgrounds: enabled,
                ..Default::default()
            };
            let (context, frame) = native_frame(&preferences, 500.0);
            let fills = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect { color, rect, .. }
                        if color.a == 20 && rect.width > 2.0 =>
                    {
                        Some(color)
                    }
                    _ => None,
                })
                .count();
            assert_eq!(fills, if enabled { 2 } else { 0 });
            for marker in ["+", "−"] {
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == marker
                )));
            }
            context.end_pass().textures_delta.clear();
        }
    }
}
