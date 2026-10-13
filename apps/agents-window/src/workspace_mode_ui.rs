//! The workspace handoff follows the titlebar affordance used by VS Code:
//! a quiet icon at rest, with its destination revealed on hover or focus.
//! Geometry is driven by the host's native ScalarTransition, not by a web view.

use super::*;
use std::sync::OnceLock;

use openide_ui::RasterImage;

// The cropped vector of the empty editor, at full opacity and cached once.
fn brand_icon() -> Option<RasterImage> {
    static IMAGE: OnceLock<Option<RasterImage>> = OnceLock::new();
    IMAGE
        .get_or_init(|| crate::ide_editor_empty::brand_raster(64, 1.0, "openADE"))
        .clone()
}

const COLLAPSED_WIDTH: f32 = crate::dock_tab_ui::TOOL_SIZE;
const HEIGHT: f32 = crate::dock_tab_ui::TOOL_SIZE;

pub(super) fn render(state: &ViewState, compact: bool) -> Element {
    let ide = state.ide.is_active();
    let progress = state.ide.mode_reveal.clamp(0.0, 1.0);
    let reveal = if compact { 0.0 } else { progress };
    let label_width = if ide { 54.0 } else { 30.0 };
    let width = COLLAPSED_WIDTH + (label_width + 6.0) * reveal;
    let surface = if ide {
        theme::WINDOW
    } else {
        theme::BACKGROUND
    };
    let ink = theme::FOREGROUND;
    let palette = theme::action_palette(surface, ink);
    let label = if ide { "Agentes" } else { "IDE" };
    let hint = if ide {
        "Cambiar a la ventana de agentes"
    } else {
        "Cambiar a la ventana IDE"
    };
    let mut children = vec![brand_icon()
        .map(Element::raster_image)
        .unwrap_or_else(Element::spacer)
        .size(Some(16.0), Some(16.0))];
    if reveal > 0.0 {
        children.push(
            Element::panel(
                // A reveal clips one unwrapped line; it must not reflow the
                // destination into multiple lines at intermediate widths.
                Element::faded_text(label, 0.0)
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(Color::rgba(ink.r, ink.g, ink.b, (255.0 * reveal) as u8))
                    .size(Some(label_width), Some(16.0)),
            )
            .size(Some(label_width * reveal), Some(16.0))
            .clip_children(),
        );
    }
    Tooltip::new(
        palette.interactive_surface(
            Element::clickable(
                "window_mode_toggle",
                Element::row(children).gap(6.0 * reveal),
            )
            .padding(4.0)
            .radius(6.0)
            .size(Some(width), Some(HEIGHT)),
        ),
        "window_mode_toggle",
        hint,
        theme::component_palette(),
    )
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide_workspace::WindowMode;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use openide_ui::{
        DrawCommand, Point, RasterImageFormat, Rect, Size, UiAction, UiEvent, UiTree,
    };
    use sha2::{Digest, Sha256};

    #[test]
    fn workspace_mode_reveal_preserves_click_target_and_clips_one_label_line() {
        for (mode, label, expanded) in [
            (WindowMode::Ide, "Agentes", 84.0),
            (WindowMode::Agents, "IDE", 60.0),
        ] {
            for progress in [0.0, 0.5, 1.0] {
                for compact in [false, true] {
                    let mut state = crate::view::tests::empty_state();
                    state.ide.mode = mode;
                    state.ide.mode_reveal = progress;
                    let expected_width = if compact {
                        COLLAPSED_WIDTH
                    } else {
                        COLLAPSED_WIDTH + (expanded - COLLAPSED_WIDTH) * progress
                    };
                    let viewport = Size::new(expected_width, HEIGHT);
                    let mut tree = UiTree::new(render(&state, compact));
                    tree.set_reduced_motion(true);
                    let frame = tree.layout(viewport);
                    let control = frame
                        .regions
                        .iter()
                        .find(|region| region.id == "window_mode_toggle")
                        .expect("workspace handoff action")
                        .rect;
                    assert_eq!(control, Rect::new(0.0, 0.0, expected_width, HEIGHT));

                    let glyph = frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            DrawCommand::RasterImage { rect, image, .. }
                                if image.alt() == "openADE" && image == &brand_icon().unwrap() =>
                            {
                                Some(*rect)
                            }
                            _ => None,
                        })
                        .expect("shared existing openADE brand");
                    assert_eq!(glyph, Rect::new(4.0, 4.0, 16.0, 16.0));

                    let mut clips = Vec::new();
                    let mut label_lines = 0;
                    for command in &frame.commands {
                        match command {
                            DrawCommand::PushClip(rect) => clips.push(*rect),
                            DrawCommand::PopClip => {
                                clips.pop();
                            }
                            DrawCommand::Text { text, color, .. } => {
                                assert_eq!(text, label, "reveal must preserve an unwrapped label");
                                label_lines += 1;
                                let clip = clips.last().expect("label must remain clipped");
                                assert!(clip.x >= glyph.x + glyph.width);
                                assert!(clip.x + clip.width <= control.width - 4.0 + 0.01);
                                assert!(clip.y >= control.y);
                                assert!(clip.y + clip.height <= control.y + control.height);
                                assert_eq!(color.a, (255.0 * progress) as u8);
                            }
                            _ => {}
                        }
                    }
                    assert_eq!(label_lines, usize::from(!compact && progress > 0.0));
                    let point = Point::new(control.width / 2.0, control.height / 2.0);
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        vec![UiAction::Clicked {
                            id: "window_mode_toggle".into()
                        }]
                    );
                }
            }
        }
    }

    #[test]
    fn workspace_mode_brand_keeps_tooltip_keyboard_and_reduced_motion_behavior() {
        let brand = brand_icon().expect("embedded openADE vector must decode");
        assert_eq!(
            brand,
            brand_icon().unwrap(),
            "both modes share one cached asset"
        );
        assert_eq!(brand.format(), RasterImageFormat::Png);
        let bytes = STANDARD.decode(brand.data_base64()).unwrap();
        assert_eq!(brand.sha256(), format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(bytes.len(), brand.size_bytes() as usize);
        assert!(bytes.len() < 32 * 1024);
        for (mode, hint) in [
            (WindowMode::Ide, "Cambiar a la ventana de agentes"),
            (WindowMode::Agents, "Cambiar a la ventana IDE"),
        ] {
            let mut state = crate::view::tests::empty_state();
            state.ide.mode = mode;
            let mut tree = UiTree::new(Element::column(vec![
                render(&state, true),
                Element::spacer().flex(1.0),
            ]));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(360.0, 160.0));
            assert!(tree.focus("window_mode_toggle"));
            let frame = tree.layout(Size::new(360.0, 160.0));
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == hint
            )));
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::RasterImage { image, .. } if image == &brand
            )));
            assert!(tree.animation_repaint_after().is_none());
            for key in [openide_ui::Key::Enter, openide_ui::Key::Space] {
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key,
                        ctrl: false,
                        shift: false
                    }),
                    vec![UiAction::Clicked {
                        id: "window_mode_toggle".into()
                    }]
                );
            }
        }
    }

    #[test]
    #[ignore = "opt-in native review of the shared openADE mode-switch brand"]
    fn export_workspace_mode_brand_visual_review() {
        let directory = std::path::Path::new("/tmp/openade-workspace-mode-brand-review");
        std::fs::create_dir_all(directory).unwrap();
        for (mode, name) in [(WindowMode::Ide, "ide"), (WindowMode::Agents, "agents")] {
            for width in [520.0, 1440.0] {
                for (progress, phase) in [(0.0, "idle"), (0.5, "revealing"), (1.0, "expanded")] {
                    let mut state = crate::view::ide_visual_tests::fixture();
                    state.ide.mode = mode;
                    state.ide.mode_reveal = progress;
                    let size = Size::new(width, 900.0);
                    let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                        crate::view::build_window_view(&state, size.width, size.height),
                    );
                    let frame = tree.layout(size);
                    assert!(frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::RasterImage { image, .. } if image.alt() == "openADE"
                    )));
                    let path = directory.join(format!("{name}-{width:.0}-{phase}.svg"));
                    crate::settings_ui::visual_qa::write_native_frame_svg(
                        &frame,
                        &path,
                        size.width,
                        size.height,
                    )
                    .unwrap();
                    let svg = resvg::usvg::Tree::from_str(
                        &std::fs::read_to_string(&path).unwrap(),
                        &crate::settings_ui::visual_qa::native_svg_options(),
                    )
                    .unwrap();
                    let mut pixels =
                        resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32)
                            .unwrap();
                    resvg::render(
                        &svg,
                        resvg::tiny_skia::Transform::identity(),
                        &mut pixels.as_mut(),
                    );
                    pixels.save_png(path.with_extension("png")).unwrap();
                    context.end_pass().textures_delta.clear();
                }
            }
        }
    }
}
