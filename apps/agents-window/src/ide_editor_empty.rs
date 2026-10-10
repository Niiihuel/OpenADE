//! Editor-only watermark and shortcuts, inspired by Code OSS's empty group.
use std::sync::{Arc, OnceLock};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use openide_ui::{Component, Element, Kbd, ListRow, RasterImage, RasterImageFormat};
use sha2::{Digest, Sha256};

use crate::{keymap::{KeymapProfile, ShortcutAction}, theme};

/// The embedded brand vector as a square PNG of `side` pixels with every
/// channel scaled by `strength`. Callers cache the result: painting never
/// rasterizes, and never reads a disk or a network.
pub(crate) fn brand_raster(side: u32, strength: f32, alt: &str) -> Option<RasterImage> {
    let tree = resvg::usvg::Tree::from_str(
        include_str!("../assets/brand/openide-editor.svg"),
        &Default::default(),
    )
    .ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            side as f32 / tree.size().width(),
            side as f32 / tree.size().height(),
        ),
        &mut pixmap.as_mut(),
    );
    if strength < 1.0 {
        for channel in pixmap.data_mut() {
            *channel = (f32::from(*channel) * strength).round() as u8;
        }
    }
    let bytes = pixmap.encode_png().ok()?;
    RasterImage::new(
        format!("{:x}", Sha256::digest(&bytes)),
        RasterImageFormat::Png,
        bytes.len() as u32,
        Arc::from(STANDARD.encode(bytes)),
        alt,
    )
}

fn watermark() -> Option<RasterImage> {
    static IMAGE: OnceLock<Option<RasterImage>> = OnceLock::new();
    // 2x the largest size shown. Subdued facets keep the logo's shape without
    // competing with the editor or turning the empty group into a welcome card.
    IMAGE
        .get_or_init(|| brand_raster(512, 0.18, "OpenIDE · editor vacío"))
        .clone()
}

pub(crate) fn build(profile: KeymapProfile, width: f32, height: f32) -> Element {
    let content_width = (width - 40.0).clamp(0.0, 300.0);
    let logo_size = content_width.min(256.0).min((height - 148.0).max(0.0));
    let palette = theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE);
    let mut children = Vec::new();
    if logo_size >= 64.0 {
        if let Some(image) = watermark() {
            children.push(
                Element::raster_image(image)
                    .size(Some(logo_size), Some(logo_size))
                    .centered_x()
                    .size(None, Some(logo_size)),
            );
            children.push(Element::spacer().size(None, Some(24.0)));
        }
    }
    for (id, action, title, shortcut) in [
        (
            "ide_empty_file",
            "ide_quick_open",
            "Buscar archivo",
            ShortcutAction::QuickOpen,
        ),
        (
            "ide_empty_commands",
            "open_command_center",
            "Buscar y ejecutar",
            ShortcutAction::CommandPalette,
        ),
        (
            "ide_empty_terminal",
            "terminal_toggle",
            "Mostrar terminal",
            ShortcutAction::ToggleTerminal,
        ),
    ] {
        let row = ListRow::new(id, title, palette)
            .sizing(crate::ui_sizing::EMPTY_EDITOR_ROW)
            .width(content_width);
        let row = if content_width >= 220.0 {
            row.trailing(Kbd::new(profile.shortcut(shortcut), palette).render())
        } else {
            row
        };
        children.push(row.render().dispatch_as(action));
    }
    let content = Element::column(children).size(Some(content_width), None);
    content.centered().clip_children().flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, FontFamily, Point, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn changing_profile_replaces_the_empty_editor_palette_keycaps() {
        let size = Size::new(460.0, 700.0);
        let mut tree = UiTree::new(build(KeymapProfile::OpenIde, size.width, size.height));
        for profile in [KeymapProfile::OpenIde, KeymapProfile::VsCode, KeymapProfile::Zed, KeymapProfile::OpenIde] {
            tree.set_root(build(profile, size.width, size.height));
            let frame = tree.layout(size);
            let palette = frame.regions.iter().find(|region| region.id == "ide_empty_commands").unwrap().rect;
            let labels = frame.commands.iter().filter_map(|command| match command {
                DrawCommand::Text { position, text, .. } if palette.contains(*position) => Some(text.as_str()),
                _ => None,
            }).collect::<Vec<_>>();
            assert_eq!(labels.contains(&"K"), profile == KeymapProfile::OpenIde);
            assert_eq!(labels.contains(&"P"), profile != KeymapProfile::OpenIde);
            assert_eq!(labels.contains(&"Shift"), profile != KeymapProfile::OpenIde);
            let point = Point::new(palette.x + 8.0, palette.y + palette.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(tree.handle_event(UiEvent::PointerUp(point)), [UiAction::Clicked { id: "open_command_center".into() }]);
        }
    }

    #[test]
    fn editor_empty_actions_and_watermark_fit_small_and_large_groups() {
        for (width, height) in [
            (240.0, 260.0),
            (460.0, 700.0),
            (980.0, 800.0),
            (200.0, 140.0),
        ] {
            let mut tree = UiTree::new(build(KeymapProfile::default(), width, height));
            let frame = tree.layout(Size::new(width, height));
            for (id, action) in [
                ("ide_empty_file", "ide_quick_open"),
                ("ide_empty_commands", "open_command_center"),
                ("ide_empty_terminal", "terminal_toggle"),
            ] {
                let rect = frame.regions.iter().find(|r| r.id == id).unwrap().rect;
                assert!(rect.x >= 0.0 && rect.y >= 0.0);
                assert!(rect.x + rect.width <= width && rect.y + rect.height <= height);
                let point = Point::new(rect.x + 10.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    [UiAction::Clicked { id: action.into() }]
                );
            }
            if width >= 460.0 {
                assert!(frame.commands.iter().any(|c| matches!(c, DrawCommand::RasterImage { image, .. } if image.alt() == "OpenIDE · editor vacío")));
                for key in ["Ctrl", "P", "Shift", "`"] {
                    assert!(
                        frame
                            .commands
                            .iter()
                            .any(|c| matches!(c, DrawCommand::Text { text, .. } if text == key)),
                        "missing key {key}"
                    );
                }
            }
        }
    }

    // A shortcut row as it was built by hand before limn's `ListRow`. Delete
    // with the final audit (S18).
    fn legacy_row(id: &str, action: &str, title: &str, keys: Option<&str>, width: f32) -> Element {
        let palette = theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE);
        let mut row = vec![
            Element::truncated_text(title)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(palette.text)
                .flex(1.0),
        ];
        if let Some(keys) = keys {
            row.push(Kbd::new(keys, palette).render());
        }
        palette
            .interactive_surface(Element::clickable(
                id,
                Element::row(row).gap(20.0).size(None, Some(28.0)),
            ))
            .dispatch_as(action)
            .radius(4.0)
            .padding(4.0)
            .size(Some(width), Some(32.0))
    }

    #[test]
    fn shortcut_rows_paint_what_they_did_by_hand_under_the_rows_clip() {
        let palette = theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE);
        for (width, keys) in [(200.0, None), (300.0, Some("Ctrl+Shift+P"))] {
            let row = ListRow::new("ide_empty_commands", "Buscar y ejecutar", palette)
                .sizing(crate::ui_sizing::EMPTY_EDITOR_ROW)
                .width(width);
            let row = match keys {
                Some(keys) => row.trailing(Kbd::new(keys, palette).render()),
                None => row,
            };
            let current = row.render().dispatch_as("open_command_center");
            let legacy = legacy_row("ide_empty_commands", "open_command_center", "Buscar y ejecutar", keys, width);
            // `ListRow` clips a row of fixed width; nothing here reaches the clip.
            let unclipped = |frames: Vec<openide_ui::Frame>| -> Vec<(Vec<DrawCommand>, String)> {
                frames
                    .into_iter()
                    .map(|frame| {
                        let regions = format!("{:?}", frame.regions);
                        let commands = frame
                            .commands
                            .into_iter()
                            .filter(|command| !matches!(command, DrawCommand::PushClip(_) | DrawCommand::PopClip))
                            .collect();
                        (commands, regions)
                    })
                    .collect()
            };
            for window in [360.0, 900.0] {
                let (frames, actions) = crate::icon_button_parity_tests::states(current.clone(), window);
                let (legacy_frames, legacy_actions) = crate::icon_button_parity_tests::states(legacy.clone(), window);
                assert_eq!(frames.len(), 4);
                assert_eq!(unclipped(frames), unclipped(legacy_frames), "width={width} window={window}");
                assert_eq!(actions, legacy_actions);
            }
        }
    }
}
