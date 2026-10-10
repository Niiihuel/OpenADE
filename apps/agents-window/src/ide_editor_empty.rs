//! Editor-only watermark and shortcuts, inspired by Code OSS's empty group.
use std::sync::{Arc, OnceLock};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use openide_ui::{Component, Element, FontFamily, Kbd, RasterImage, RasterImageFormat};
use sha2::{Digest, Sha256};

use crate::{keymap::{KeymapProfile, ShortcutAction}, theme};

fn watermark() -> Option<RasterImage> {
    static IMAGE: OnceLock<Option<RasterImage>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            // Keep the source vector embedded. Rasterize once at 2x the maximum
            // displayed size, without a disk/network dependency during painting.
            let tree = resvg::usvg::Tree::from_str(
                include_str!("../assets/brand/openide-editor.svg"),
                &Default::default(),
            )
            .ok()?;
            let mut pixmap = resvg::tiny_skia::Pixmap::new(512, 512)?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(
                    512.0 / tree.size().width(),
                    512.0 / tree.size().height(),
                ),
                &mut pixmap.as_mut(),
            );
            // Subdued brand facets retain the logo's shape without competing
            // with the editor or turning the empty workspace into a welcome card.
            for channel in pixmap.data_mut() {
                *channel = (f32::from(*channel) * 0.18).round() as u8;
            }
            let bytes = pixmap.encode_png().ok()?;
            RasterImage::new(
                format!("{:x}", Sha256::digest(&bytes)),
                RasterImageFormat::Png,
                bytes.len() as u32,
                Arc::from(STANDARD.encode(bytes)),
                "OpenIDE · editor vacío",
            )
        })
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
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    Element::raster_image(image).size(Some(logo_size), Some(logo_size)),
                    Element::spacer().flex(1.0),
                ])
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
        let mut row = vec![
            Element::truncated_text(title)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(palette.text)
                .flex(1.0),
        ];
        if content_width >= 220.0 {
            row.push(Kbd::new(profile.shortcut(shortcut), palette).render());
        }
        children.push(
            palette
                .interactive_surface(Element::clickable(
                    id,
                    Element::row(row).gap(20.0).size(None, Some(28.0)),
                ))
                .dispatch_as(action)
                .radius(4.0)
                .padding(4.0)
                .size(Some(content_width), Some(32.0)),
        );
    }
    let content = Element::column(children).size(Some(content_width), None);
    Element::column(vec![
        Element::spacer().flex(1.0),
        Element::row(vec![
            Element::spacer().flex(1.0),
            content,
            Element::spacer().flex(1.0),
        ]),
        Element::spacer().flex(1.0),
    ])
    .clip_children()
    .flex(1.0)
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
