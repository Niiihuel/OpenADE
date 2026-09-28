use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect as EguiRect, Stroke, StrokeKind,
};
use openide_ui::{
    Color, DrawCommand, FontFamily, Frame, Icon, Key, Point, Rect, UiEvent, ICON_SIZE,
};
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct IconKey {
    icon: Icon,
    pixels: u16,
}

#[derive(Default)]
struct IconCache {
    context: Option<egui::Context>,
    textures: HashMap<IconKey, egui::TextureHandle>,
}

thread_local! {
    // Textures belong to an egui context on this UI thread. Keying the
    // rasterization keeps SVG parsing out of subsequent paint passes.
    static ICON_CACHE: RefCell<IconCache> = RefCell::new(IconCache::default());
}

fn icon_svg(icon: Icon) -> &'static str {
    match icon {
        Icon::PanelLeft => include_str!("../assets/solar/panel-left.svg"),
        Icon::PanelLeftOpen => include_str!("../assets/solar/panel-left-open.svg"),
        Icon::PanelLeftClose => include_str!("../assets/solar/panel-left-close.svg"),
        Icon::PanelRight => include_str!("../assets/solar/panel-right.svg"),
        Icon::PanelRightOpen => include_str!("../assets/solar/panel-right-open.svg"),
        Icon::PanelRightClose => include_str!("../assets/solar/panel-right-close.svg"),
        Icon::Add => include_str!("../assets/solar/add.svg"),
        Icon::Check => include_str!("../assets/solar/check.svg"),
        Icon::Settings => include_str!("../assets/solar/settings.svg"),
        Icon::Github => include_str!("../assets/octicons/github-mark.svg"),
        Icon::Chat => include_str!("../assets/solar/chat-round-dots.svg"),
        Icon::Folder => include_str!("../assets/solar/folder.svg"),
        Icon::Laptop => include_str!("../assets/solar/laptop-minimalistic.svg"),
        Icon::GitBranch => include_str!("../assets/solar/git-branch.svg"),
        Icon::Grid => include_str!("../assets/solar/widget.svg"),
        Icon::Hash => include_str!("../assets/solar/hashtag.svg"),
        Icon::Code => include_str!("../assets/solar/code.svg"),
        Icon::Terminal => include_str!("../assets/openide/terminal.svg"),
        Icon::ArrowUp => include_str!("../assets/solar/arrow-up.svg"),
        Icon::Send => include_str!("../assets/openide/send-arrow.svg"),
        Icon::ArrowDown => include_str!("../assets/solar/arrow-down.svg"),
        Icon::ArrowLeft => include_str!("../assets/solar/arrow-left.svg"),
        Icon::ArrowRight => include_str!("../assets/solar/arrow-right.svg"),
        Icon::ChevronDown => include_str!("../assets/solar/alt-arrow-down.svg"),
        Icon::ChevronRight => include_str!("../assets/solar/alt-arrow-right.svg"),
        Icon::Search => include_str!("../assets/solar/magnifier.svg"),
        Icon::Stop => include_str!("../assets/solar/stop.svg"),
        Icon::History => include_str!("../assets/solar/history.svg"),
        Icon::Close => include_str!("../assets/solar/close.svg"),
        Icon::Refresh => include_str!("../assets/solar/refresh.svg"),
        Icon::Menu => include_str!("../assets/solar/hamburger-menu.svg"),
        Icon::More => include_str!("../assets/solar/menu-dots.svg"),
        Icon::Pin => include_str!("../assets/solar/pin.svg"),
        Icon::Archive => include_str!("../assets/solar/archive.svg"),
        Icon::ArchiveUp => include_str!("../assets/solar/archive-up.svg"),
        Icon::Trash => include_str!("../assets/solar/trash-bin-minimalistic.svg"),
        Icon::Edit => include_str!("../assets/solar/pen.svg"),
        Icon::Unread => include_str!("../assets/solar/chat-unread.svg"),
        Icon::Filter => include_str!("../assets/solar/filter.svg"),
        Icon::Sort => include_str!("../assets/solar/sort.svg"),
        Icon::Collapse => include_str!("../assets/solar/minimize.svg"),
        Icon::Expand => include_str!("../assets/solar/maximize.svg"),
        Icon::Clock => include_str!("../assets/solar/clock-circle.svg"),
    }
}

fn monochrome_svg(source: &str) -> String {
    // Solar Linear draws with currentColor. Rasterize a white coverage mask
    // once per icon and physical size, then tint it per DrawCommand. This
    // keeps theme and hover colors out of the texture cache, like WarpUI's
    // monochrome Icon element does for its size-cached image.
    source.replace("currentColor", "#FFFFFF")
}

fn raster_icon(key: IconKey) -> Option<egui::ColorImage> {
    let source = monochrome_svg(icon_svg(key.icon));
    let tree = resvg::usvg::Tree::from_str(&source, &resvg::usvg::Options::default()).ok()?;
    let pixels = u32::from(key.pixels);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixels, pixels)?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        pixels as f32 / tree.size().width(),
        pixels as f32 / tree.size().height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied(
        [pixels as usize, pixels as usize],
        pixmap.data(),
    ))
}

fn icon_texture(ui: &egui::Ui, key: IconKey) -> Option<egui::TextureId> {
    ICON_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.context.as_ref() != Some(ui.ctx()) {
            cache.textures.clear();
            cache.context = Some(ui.ctx().clone());
        }
        if let Some(texture) = cache.textures.get(&key) {
            return Some(texture.id());
        }
        let image = raster_icon(key)?;
        let texture = ui.ctx().load_texture(
            format!("solar:{:?}:{}", key.icon, key.pixels),
            image,
            egui::TextureOptions::NEAREST,
        );
        let id = texture.id();
        cache.textures.insert(key, texture);
        Some(id)
    })
}

fn color(value: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(value.r, value.g, value.b, value.a)
}

fn pos(origin: Pos2, value: Point) -> Pos2 {
    Pos2::new(origin.x + value.x, origin.y + value.y)
}

fn rect(origin: Pos2, value: Rect) -> EguiRect {
    EguiRect::from_min_size(
        Pos2::new(origin.x + value.x, origin.y + value.y),
        egui::vec2(value.width, value.height),
    )
}

// A texture rendered at its final physical size needs integer pixel edges.
// Fractional UI layout coordinates otherwise make the GPU resample a sharp
// raster even when the texture itself has exactly the right resolution.
fn aligned_icon_rect(area: EguiRect, pixels_per_point: f32) -> Option<(EguiRect, u16)> {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return None;
    }
    // The tree reserves a 16 point box. Keep that upper bound when painting
    // DrawCommands supplied by another caller of the backend, too.
    let side = area.width().min(area.height()).min(ICON_SIZE);
    if !side.is_finite() || side <= 0.0 {
        return None;
    }
    // Decimal DPI factors and egui rectangles can land just below a half
    // pixel through f32 rounding (for example 14 * 1.25 as 17.499998).
    let pixels = (side * pixels_per_point + 0.0001).round().clamp(1.0, 256.0) as u16;
    let center = area.center();
    let x = (center.x * pixels_per_point - f32::from(pixels) / 2.0).round();
    let y = (center.y * pixels_per_point - f32::from(pixels) / 2.0).round();
    let min = Pos2::new(x / pixels_per_point, y / pixels_per_point);
    let size = egui::Vec2::splat(f32::from(pixels) / pixels_per_point);
    Some((EguiRect::from_min_size(min, size), pixels))
}

pub fn paint(ui: &egui::Ui, frame: &Frame) {
    paint_at(ui, frame, ui.max_rect().min);
}

/// Paint a tree laid out in its own coordinate space at a window position.
pub fn paint_at(ui: &egui::Ui, frame: &Frame, origin: Pos2) {
    let mut clips = vec![ui.clip_rect()];
    for command in &frame.commands {
        match command {
            DrawCommand::PushClip(area) => {
                let next = clips
                    .last()
                    .copied()
                    .unwrap_or(ui.clip_rect())
                    .intersect(rect(origin, *area));
                clips.push(next);
            }
            DrawCommand::PopClip => {
                if clips.len() > 1 {
                    clips.pop();
                }
            }
            DrawCommand::Shadow {
                rect: area,
                shadow,
                radius,
            } => {
                let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                let offset = [
                    shadow
                        .offset
                        .x
                        .round()
                        .clamp(i8::MIN as f32, i8::MAX as f32) as i8,
                    shadow
                        .offset
                        .y
                        .round()
                        .clamp(i8::MIN as f32, i8::MAX as f32) as i8,
                ];
                let shape = egui::epaint::Shadow {
                    offset,
                    blur: shadow.blur.round().clamp(0.0, u8::MAX as f32) as u8,
                    spread: shadow.spread.round().clamp(0.0, u8::MAX as f32) as u8,
                    color: color(shadow.color),
                }
                .as_shape(rect(origin, *area), CornerRadius::same(*radius as u8));
                painter.add(shape);
            }
            DrawCommand::FillRect {
                rect: area,
                color: fill,
                radius,
            } => {
                let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                painter.rect_filled(
                    rect(origin, *area),
                    CornerRadius::same(*radius as u8),
                    color(*fill),
                );
            }
            DrawCommand::FillVerticalGradient {
                rect: area,
                top,
                bottom,
            } => {
                let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                let area = rect(origin, *area);
                let mut mesh = egui::Mesh::default();
                let uv = egui::epaint::WHITE_UV;
                mesh.vertices.extend([
                    egui::epaint::Vertex {
                        pos: area.left_top(),
                        uv,
                        color: color(*top),
                    },
                    egui::epaint::Vertex {
                        pos: area.right_top(),
                        uv,
                        color: color(*top),
                    },
                    egui::epaint::Vertex {
                        pos: area.right_bottom(),
                        uv,
                        color: color(*bottom),
                    },
                    egui::epaint::Vertex {
                        pos: area.left_bottom(),
                        uv,
                        color: color(*bottom),
                    },
                ]);
                mesh.indices.extend([0, 1, 2, 0, 2, 3]);
                painter.add(egui::Shape::mesh(mesh));
            }
            DrawCommand::StrokeRect {
                rect: area,
                color: ink,
                width,
                radius,
            } => {
                let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                painter.rect_stroke(
                    rect(origin, *area),
                    CornerRadius::same(*radius as u8),
                    Stroke::new(*width, color(*ink)),
                    StrokeKind::Inside,
                );
            }
            DrawCommand::FillTriangle {
                points,
                color: fill,
            } => {
                let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                painter.add(egui::Shape::convex_polygon(
                    points.iter().map(|point| pos(origin, *point)).collect(),
                    color(*fill),
                    Stroke::NONE,
                ));
            }
            DrawCommand::Text {
                position,
                text,
                color: ink,
                font_size,
                font_family,
            } => {
                let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                let font = match font_family {
                    FontFamily::Monospace => FontId::monospace(*font_size),
                    FontFamily::Sans => FontId::proportional(*font_size),
                };
                painter.text(
                    pos(origin, *position),
                    Align2::LEFT_TOP,
                    text,
                    font,
                    color(*ink),
                );
            }
            DrawCommand::Icon {
                rect: area,
                icon,
                tint,
            } => {
                if let Some((aligned, pixels)) =
                    aligned_icon_rect(rect(origin, *area), ui.ctx().pixels_per_point())
                {
                    let key = IconKey {
                        icon: *icon,
                        pixels,
                    };
                    if let Some(texture) = icon_texture(ui, key) {
                        let painter = ui.painter().with_clip_rect(*clips.last().unwrap());
                        painter.image(
                            texture,
                            aligned,
                            EguiRect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            color(*tint),
                        );
                    }
                }
            }
        }
    }
}

fn relative(origin: Pos2, position: Pos2) -> Point {
    Point::new(position.x - origin.x, position.y - origin.y)
}

pub fn events(ctx: &egui::Context, origin: Pos2) -> Vec<UiEvent> {
    ctx.input(|input| {
        let mut events = Vec::new();
        let mut pointer = input.pointer.hover_pos();
        let enter_pressed = input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                }
            )
        });
        for event in &input.events {
            match event {
                egui::Event::PointerMoved(pos) => {
                    pointer = Some(*pos);
                    events.push(UiEvent::PointerMove(relative(origin, *pos)));
                }
                egui::Event::PointerGone => {
                    pointer = None;
                    events.push(UiEvent::PointerLeave);
                }
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    ..
                } => {
                    pointer = Some(*pos);
                    events.push(if *pressed {
                        UiEvent::PointerDown(relative(origin, *pos))
                    } else {
                        UiEvent::PointerUp(relative(origin, *pos))
                    });
                }
                egui::Event::MouseWheel { unit, delta, .. } => {
                    if let Some(pos) = pointer {
                        let scale = match unit {
                            egui::MouseWheelUnit::Point => 1.0,
                            egui::MouseWheelUnit::Line => 30.0,
                            egui::MouseWheelUnit::Page => 500.0,
                        };
                        events.push(UiEvent::Scroll {
                            position: relative(origin, pos),
                            delta_y: delta.y * scale,
                        });
                    }
                }
                egui::Event::Text(value) => {
                    if !(enter_pressed && matches!(value.as_str(), "\n" | "\r" | "\r\n")) {
                        events.push(UiEvent::Text(value.clone()));
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let mapped = match key {
                        egui::Key::Escape => Some(Key::Escape),
                        egui::Key::ArrowLeft => Some(Key::ArrowLeft),
                        egui::Key::ArrowRight => Some(Key::ArrowRight),
                        egui::Key::ArrowUp => Some(Key::ArrowUp),
                        egui::Key::ArrowDown => Some(Key::ArrowDown),
                        egui::Key::Home => Some(Key::Home),
                        egui::Key::End => Some(Key::End),
                        egui::Key::Backspace => Some(Key::Backspace),
                        egui::Key::Delete => Some(Key::Delete),
                        egui::Key::Enter => Some(Key::Enter),
                        egui::Key::Tab => Some(Key::Tab),
                        egui::Key::Space => Some(Key::Space),
                        egui::Key::A => Some(Key::A),
                        egui::Key::B => Some(Key::B),
                        egui::Key::C => Some(Key::C),
                        egui::Key::D => Some(Key::D),
                        egui::Key::E => Some(Key::E),
                        egui::Key::F => Some(Key::F),
                        egui::Key::G => Some(Key::G),
                        egui::Key::H => Some(Key::H),
                        egui::Key::I => Some(Key::I),
                        egui::Key::J => Some(Key::J),
                        egui::Key::K => Some(Key::K),
                        egui::Key::L => Some(Key::L),
                        egui::Key::M => Some(Key::M),
                        egui::Key::N => Some(Key::N),
                        egui::Key::O => Some(Key::O),
                        egui::Key::P => Some(Key::P),
                        egui::Key::Q => Some(Key::Q),
                        egui::Key::R => Some(Key::R),
                        egui::Key::S => Some(Key::S),
                        egui::Key::T => Some(Key::T),
                        egui::Key::U => Some(Key::U),
                        egui::Key::V => Some(Key::V),
                        egui::Key::W => Some(Key::W),
                        egui::Key::X => Some(Key::X),
                        egui::Key::Y => Some(Key::Y),
                        egui::Key::Z => Some(Key::Z),
                        _ => None,
                    };
                    if let Some(key) = mapped {
                        events.push(UiEvent::Key {
                            key,
                            shift: modifiers.shift,
                            ctrl: modifiers.ctrl || modifiers.command,
                        });
                    }
                }
                _ => {}
            }
        }
        events
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_solar_linear_icons_rasterize_as_monochrome_masks() {
        let icons = [
            Icon::PanelLeft,
            Icon::PanelLeftOpen,
            Icon::PanelLeftClose,
            Icon::PanelRight,
            Icon::PanelRightOpen,
            Icon::PanelRightClose,
            Icon::Add,
            Icon::Check,
            Icon::Settings,
            Icon::Chat,
            Icon::Folder,
            Icon::Laptop,
            Icon::GitBranch,
            Icon::Grid,
            Icon::Hash,
            Icon::Code,
            Icon::ArrowUp,
            Icon::ArrowDown,
            Icon::ArrowLeft,
            Icon::ArrowRight,
            Icon::ChevronDown,
            Icon::ChevronRight,
            Icon::Search,
            Icon::Stop,
            Icon::History,
            Icon::Close,
            Icon::Refresh,
            Icon::Menu,
            Icon::More,
            Icon::Pin,
            Icon::Archive,
            Icon::ArchiveUp,
            Icon::Trash,
            Icon::Edit,
            Icon::Unread,
            Icon::Filter,
            Icon::Sort,
            Icon::Collapse,
            Icon::Expand,
            Icon::Clock,
        ];
        for icon in icons {
            let source = icon_svg(icon);
            assert!(
                source.contains("viewBox=\"0 0 24 24\""),
                "Solar source needs a canonical square viewBox: {icon:?}"
            );
            assert!(source.contains("-linear"), "wrong Solar style: {icon:?}");
            assert!(!source.contains("duotone"), "wrong Solar style: {icon:?}");
            let key = IconKey { icon, pixels: 48 };
            let svg = monochrome_svg(source);
            assert!(svg.contains("#FFFFFF"), "missing white mask: {icon:?}");
            assert!(!svg.contains("currentColor"), "unresolved color: {icon:?}");
            let image = raster_icon(key).unwrap_or_else(|| panic!("invalid SVG: {icon:?}"));
            assert_eq!(image.size, [48, 48]);
            assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
            assert!(
                image
                    .pixels
                    .iter()
                    .all(|pixel| pixel.r() == pixel.g() && pixel.g() == pixel.b()),
                "mask contains chromatic pixels: {icon:?}"
            );
        }
    }

    #[test]
    fn github_mark_rasterizes_as_a_tintable_monochrome_mask() {
        let source = icon_svg(Icon::Github);
        assert!(source.contains("viewBox=\"0 0 16 16\""));
        let image = raster_icon(IconKey {
            icon: Icon::Github,
            pixels: 48,
        })
        .expect("GitHub mark should render");
        assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
        assert!(image
            .pixels
            .iter()
            .all(|pixel| pixel.r() == pixel.g() && pixel.g() == pixel.b()));
    }

    #[test]
    fn send_arrow_rasterizes_with_a_centered_stroke() {
        let source = icon_svg(Icon::Send);
        assert!(source.contains("viewBox=\"0 0 24 24\""));
        let image = raster_icon(IconKey {
            icon: Icon::Send,
            pixels: 48,
        })
        .expect("send arrow SVG");
        assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
    }

    #[test]
    fn linear_icon_mask_preserves_stroke_coverage() {
        let image = raster_icon(IconKey {
            icon: Icon::PanelLeft,
            pixels: 48,
        })
        .expect("panel-left SVG should render");
        assert!(image.pixels.iter().any(|pixel| pixel.a() == 255));
        assert!(image
            .pixels
            .iter()
            .any(|pixel| (1..255).contains(&pixel.a())));
        assert!(image.pixels.iter().any(|pixel| pixel.a() == 0));
    }

    #[test]
    fn icon_texture_and_destination_share_physical_pixel_grid() {
        let area = EguiRect::from_min_size(Pos2::new(11.3, 20.7), egui::vec2(14.0, 14.0));
        let (aligned, pixels) = aligned_icon_rect(area, 1.25).expect("valid icon rectangle");
        assert_eq!(pixels, 18);
        for edge in [aligned.min.x, aligned.min.y, aligned.max.x, aligned.max.y] {
            let physical = edge * 1.25;
            assert!((physical - physical.round()).abs() < 0.0001);
        }
        assert!((aligned.center().x - area.center().x).abs() <= 0.8);
        assert!((aligned.center().y - area.center().y).abs() <= 0.8);
        assert!(aligned_icon_rect(area, 0.0).is_none());
    }

    #[test]
    fn canonical_logical_icon_uses_current_display_scale() {
        let area = EguiRect::from_min_size(Pos2::new(3.2, 4.4), egui::Vec2::splat(32.0));
        for (scale, expected_pixels) in [(1.0, 16), (1.25, 20), (1.5, 24), (2.0, 32)] {
            let (aligned, pixels) = aligned_icon_rect(area, scale).unwrap();
            assert_eq!(pixels, expected_pixels);
            assert!((aligned.width() * scale - f32::from(expected_pixels)).abs() < 0.0001);
            assert!((aligned.height() * scale - f32::from(expected_pixels)).abs() < 0.0001);
        }
    }
}
