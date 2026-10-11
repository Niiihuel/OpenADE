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
    raster(side, alt, |data| {
        if strength < 1.0 {
            for channel in data {
                *channel = (f32::from(*channel) * strength).round() as u8;
            }
        }
    })
}

/// The brand pressed into the surface, as Code OSS's letterpress: one ink,
/// black, whose depth follows each facet, so it only ever darkens the
/// background and never reads as a picture placed on it.
fn letterpress(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        let alpha = f32::from(pixel[3]);
        if alpha == 0.0 {
            continue;
        }
        // Premultiplied channels: their sum over three alphas is the luminance.
        let light = (f32::from(pixel[0]) + f32::from(pixel[1]) + f32::from(pixel[2])) / (3.0 * alpha);
        pixel[..3].fill(0);
        pixel[3] = (alpha * (0.16 + 0.30 * (1.0 - light.min(1.0)))).round() as u8;
    }
}

fn raster(side: u32, alt: &str, shade: impl FnOnce(&mut [u8])) -> Option<RasterImage> {
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
    shade(pixmap.data_mut());
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
    // 2x the largest size shown.
    IMAGE
        .get_or_init(|| raster(512, "OpenIDE · editor vacío", letterpress))
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
    use openide_ui::{DrawCommand, Point, Size, UiAction, UiEvent, UiTree};

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
    #[ignore = "exports the empty editor as PNG under docs/ui-separation/shots/empty-editor"]
    fn empty_editor_shot() {
        let (width, height) = (900.0, 700.0);
        let root = Element::column(vec![build(KeymapProfile::VsCode, width, height)])
            .background(theme::BACKGROUND)
            .size(Some(width), Some(height));
        let frame = UiTree::new(root).layout(Size::new(width, height));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ui-separation/shots/empty-editor");
        std::fs::create_dir_all(&dir).unwrap();
        let svg = dir.join("empty-editor.svg");
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &svg, width, height).unwrap();
        let parsed = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&svg).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32 * 2, height as u32 * 2).unwrap();
        resvg::render(&parsed, resvg::tiny_skia::Transform::from_scale(2.0, 2.0), &mut pixmap.as_mut());
        pixmap.save_png(dir.join("empty-editor.png")).unwrap();
        std::fs::remove_file(svg).unwrap();
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
}
