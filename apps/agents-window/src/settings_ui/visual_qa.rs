//! Test-only export of the native tree's paint commands for visual inspection.
//!
//! Geometry, paint order and clip scopes come exclusively from `Frame`. SVG text
//! uses system fonts, and SVG blur/icon coverage can differ from the egui host;
//! this is a layout inspection aid, not a pixel comparison with that renderer.
//! Native-font exports opt into the host's Ubuntu/Hack faces explicitly; the
//! legacy writer keeps its system faces for fixtures using approximate metrics.

#![cfg(test)]

use std::{fmt::Write as _, fs, io, path::Path};

use openide_ui::{
    Color, CornerRadii, DrawCommand, FontFamily, Frame, Icon, RasterImageFormat, Rect,
    SpinnerVariant,
};

/// A frame's paint commands and hit regions with every decimal number taken
/// out, and those numbers in order.
fn paint_numbers(frame: &Frame) -> (String, Vec<f32>) {
    let source = format!("{:?}{:?}", frame.commands, frame.regions);
    let (mut shape, mut numbers) = (String::with_capacity(source.len()), Vec::new());
    let mut number = String::new();
    for character in source.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_digit() || character == '.' || (character == '-' && number.is_empty()) {
            number.push(character);
            continue;
        }
        match number.parse::<f32>() {
            Ok(value) if number.contains('.') => {
                numbers.push(value);
                shape.push('#');
            }
            _ => shape.push_str(&number),
        }
        number.clear();
        shape.push(character);
    }
    (shape, numbers)
}

/// Asserts that two frames paint and hit-test the same once a field's rows
/// are nested in a column of their own. Everything but the coordinates must
/// be exactly equal, and no coordinate may differ by more than a thousandth
/// of a pixel: summing the same heights in another order moves an `f32` by
/// its last digit, which is all this allows.
pub(crate) fn assert_same_paint_when_nested(current: &Frame, legacy: &Frame, context: &str) {
    let (current_shape, current_numbers) = paint_numbers(current);
    let (legacy_shape, legacy_numbers) = paint_numbers(legacy);
    assert_eq!(current_shape, legacy_shape, "{context}");
    for (current, legacy) in current_numbers.iter().zip(&legacy_numbers) {
        assert!((current - legacy).abs() <= 0.001, "{context}: {current} against {legacy}");
    }
}

#[test]
fn nested_paint_comparison_allows_a_rounding_error_and_nothing_else() {
    let frame = |y: f32, label: &str| {
        let mut tree = openide_ui::UiTree::new(
            openide_ui::Element::text(label).padding(y),
        );
        tree.set_reduced_motion(true);
        tree.layout(openide_ui::Size::new(200.0, 100.0))
    };
    assert_same_paint_when_nested(&frame(7.14285, "a"), &frame(7.14288, "a"), "noise");
    let differs = |current: Frame, legacy: Frame| {
        std::panic::catch_unwind(|| assert_same_paint_when_nested(&current, &legacy, "")).is_err()
    };
    assert!(differs(frame(7.14285, "a"), frame(7.15, "a")));
    assert!(differs(frame(7.14285, "a"), frame(7.14285, "b")));
}

pub(crate) fn write_frame_svg(
    frame: &Frame,
    path: &Path,
    width: f32,
    height: f32,
) -> io::Result<()> {
    write_frame_svg_with_families(
        frame,
        path,
        width,
        height,
        "DejaVu Sans,sans-serif",
        "DejaVu Sans Mono,monospace",
    )
}

/// Match the native font measurements used by interactive layout fixtures.
/// Parse this SVG with `native_svg_options` to use the bundled font bytes.
pub(crate) fn write_native_frame_svg(
    frame: &Frame,
    path: &Path,
    width: f32,
    height: f32,
) -> io::Result<()> {
    write_frame_svg_with_families(
        frame,
        path,
        width,
        height,
        "Ubuntu,sans-serif",
        "Hack,monospace",
    )
}

pub(crate) fn native_svg_options() -> resvg::usvg::Options<'static> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    for font in eframe::egui::FontDefinitions::default().font_data.values() {
        options.fontdb_mut().load_font_data(font.font.to_vec());
    }
    options
        .fontdb_mut()
        .load_font_data(include_bytes!("../../assets/fonts/Ubuntu-Regular.ttf").to_vec());
    options
}

fn write_frame_svg_with_families(
    frame: &Frame,
    path: &Path,
    width: f32,
    height: f32,
    sans_family: &str,
    mono_family: &str,
) -> io::Result<()> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SVG viewport",
        ));
    }
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" overflow=\"hidden\">\n"
    );
    let mut clip_depth = 0usize;
    for (index, command) in frame.commands.iter().enumerate() {
        match command {
            DrawCommand::PushClip(rect) => {
                writeln!(
                    svg,
                    "<defs><clipPath id=\"clip-{index}\" clipPathUnits=\"userSpaceOnUse\"><rect {}/></clipPath></defs><g clip-path=\"url(#clip-{index})\">",
                    rect_attributes(*rect)
                ).unwrap();
                clip_depth += 1;
            }
            DrawCommand::PopClip => {
                if clip_depth > 0 {
                    svg.push_str("</g>\n");
                    clip_depth -= 1;
                }
            }
            DrawCommand::FillRect {
                rect,
                color,
                radius,
            } => {
                writeln!(
                    svg,
                    "<rect {} rx=\"{radius}\" fill=\"{}\" fill-opacity=\"{}\"/>",
                    rect_attributes(*rect),
                    css_color(*color),
                    alpha(*color)
                )
                .unwrap();
            }
            DrawCommand::FillCornerRect { rect, color, radii } => {
                writeln!(
                    svg,
                    "<path d=\"{}\" fill=\"{}\" fill-opacity=\"{}\"/>",
                    corner_rect_path(*rect, *radii),
                    css_color(*color),
                    alpha(*color)
                )
                .unwrap();
            }
            DrawCommand::CornerMask { rect, color, radii } => {
                let x = rect.x;
                let y = rect.y;
                let right = x + rect.width;
                let bottom = y + rect.height;
                let limit = rect.width.min(rect.height) / 2.0;
                let mut path = String::new();
                for (corner, radius) in [
                    radii.top_left,
                    radii.top_right,
                    radii.bottom_right,
                    radii.bottom_left,
                ]
                .into_iter()
                .enumerate()
                {
                    let r = radius.clamp(0.0, limit);
                    if r <= 0.0 {
                        continue;
                    }
                    let (origin, start, end) = match corner {
                        0 => ((x, y), (x + r, y), (x, y + r)),
                        1 => ((right, y), (right, y + r), (right - r, y)),
                        2 => ((right, bottom), (right - r, bottom), (right, bottom - r)),
                        _ => ((x, bottom), (x, bottom - r), (x + r, bottom)),
                    };
                    write!(
                        path,
                        "M{} {} L{} {} A{r} {r} 0 0 0 {} {} Z ",
                        origin.0, origin.1, start.0, start.1, end.0, end.1
                    )
                    .unwrap();
                }
                writeln!(
                    svg,
                    "<path d=\"{path}\" fill=\"{}\" fill-opacity=\"{}\"/>",
                    css_color(*color),
                    alpha(*color)
                )
                .unwrap();
            }
            DrawCommand::SelectionContour { points, color } => {
                if points.len() >= 3 && points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
                    let mut path = format!("M{} {}", points[0].x, points[0].y);
                    for point in &points[1..] {
                        write!(path, " L{} {}", point.x, point.y).unwrap();
                    }
                    writeln!(
                        svg,
                        "<path d=\"{path} Z\" fill=\"{}\" fill-opacity=\"{}\"/>",
                        css_color(*color),
                        alpha(*color)
                    )
                    .unwrap();
                }
            }
            DrawCommand::StrokeRect {
                rect,
                color,
                width,
                radius,
            } => {
                // The host paints StrokeKind::Inside. Inset SVG's centered
                // stroke to keep its outside edge on the native rectangle.
                let inset = rect.inset(*width / 2.0);
                writeln!(
                    svg,
                    "<rect {} rx=\"{}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{width}\"/>",
                    rect_attributes(inset), (radius - width / 2.0).max(0.0),
                    css_color(*color), alpha(*color)
                ).unwrap();
            }
            DrawCommand::StrokeCornerRect {
                rect,
                color,
                width,
                radii,
            } => {
                let inset = *width / 2.0;
                let corners = CornerRadii {
                    top_left: (radii.top_left - inset).max(0.0),
                    top_right: (radii.top_right - inset).max(0.0),
                    bottom_right: (radii.bottom_right - inset).max(0.0),
                    bottom_left: (radii.bottom_left - inset).max(0.0),
                };
                writeln!(svg,"<path d=\"{}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{width}\"/>",
                    corner_rect_path(rect.inset(inset),corners),css_color(*color),alpha(*color)).unwrap();
            }
            DrawCommand::Text {
                position,
                text,
                color,
                font_size,
                font_family,
            } => {
                let family = match font_family {
                    FontFamily::Sans => sans_family,
                    FontFamily::Monospace => mono_family,
                };
                writeln!(
                    svg,
                    "<text x=\"{}\" y=\"{}\" font-family=\"{family}\" font-size=\"{font_size}\" dominant-baseline=\"text-before-edge\" fill=\"{}\" fill-opacity=\"{}\" xml:space=\"preserve\">{}</text>",
                    position.x, position.y, css_color(*color), alpha(*color), xml_escape(text)
                ).unwrap();
            }
            DrawCommand::Icon { rect, icon, tint } => {
                // Match the renderer's centered, at-most-16px icon at 1x DPI.
                let side = rect
                    .width
                    .min(rect.height)
                    .min(openide_ui::ICON_SIZE)
                    .round();
                if side > 0.0 {
                    let area = Rect::new(
                        (rect.x + (rect.width - side) / 2.0).round(),
                        (rect.y + (rect.height - side) / 2.0).round(),
                        side,
                        side,
                    );
                    let mut artwork = icon_svg(*icon).replace("currentColor", &css_color(*tint));
                    set_svg_root_attribute(&mut artwork, "width", &area.width.to_string());
                    set_svg_root_attribute(&mut artwork, "height", &area.height.to_string());
                    set_svg_root_attribute(&mut artwork, "x", &area.x.to_string());
                    set_svg_root_attribute(&mut artwork, "y", &area.y.to_string());
                    set_svg_root_attribute(&mut artwork, "opacity", &alpha(*tint).to_string());
                    svg.push_str(&artwork);
                    svg.push('\n');
                }
            }
            DrawCommand::FillVerticalGradient { rect, top, bottom } => {
                writeln!(
                    svg,
                    "<defs><linearGradient id=\"gradient-{index}\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\"><stop offset=\"0\" stop-color=\"{}\" stop-opacity=\"{}\"/><stop offset=\"1\" stop-color=\"{}\" stop-opacity=\"{}\"/></linearGradient></defs><rect {} fill=\"url(#gradient-{index})\"/>",
                    css_color(*top), alpha(*top), css_color(*bottom), alpha(*bottom), rect_attributes(*rect)
                ).unwrap();
            }
            DrawCommand::FillTriangle { points, color } => {
                writeln!(
                    svg,
                    "<polygon points=\"{},{} {},{} {},{}\" fill=\"{}\" fill-opacity=\"{}\"/>",
                    points[0].x,
                    points[0].y,
                    points[1].x,
                    points[1].y,
                    points[2].x,
                    points[2].y,
                    css_color(*color),
                    alpha(*color)
                )
                .unwrap();
            }
            DrawCommand::Shadow {
                rect,
                shadow,
                radius,
            } => {
                let spread = shadow.spread.round();
                let area = Rect::new(
                    rect.x + shadow.offset.x.round() - spread,
                    rect.y + shadow.offset.y.round() - spread,
                    rect.width + 2.0 * spread,
                    rect.height + 2.0 * spread,
                );
                writeln!(
                    svg,
                    "<defs><filter id=\"shadow-{index}\" x=\"-100%\" y=\"-100%\" width=\"300%\" height=\"300%\"><feGaussianBlur stdDeviation=\"{}\"/></filter></defs><rect {} rx=\"{}\" fill=\"{}\" fill-opacity=\"{}\" filter=\"url(#shadow-{index})\"/>",
                    shadow.blur.round() / 2.0, rect_attributes(area), radius + spread,
                    css_color(shadow.color), alpha(shadow.color)
                ).unwrap();
            }
            DrawCommand::ProgressRing {
                rect,
                fraction,
                track,
                fill,
            } => {
                let side = rect.width.min(rect.height);
                let radius = (side / 2.0 - 1.0).max(0.0);
                let cx = rect.x + rect.width / 2.0;
                let cy = rect.y + rect.height / 2.0;
                writeln!(svg, "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{radius}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"1.7\"/>", css_color(*track), alpha(*track)).unwrap();
                if let Some(fraction) = fraction
                    .filter(|value| value.is_finite())
                    .map(|value| value.clamp(0.0, 1.0))
                    .filter(|value| *value > 0.0)
                {
                    let length = std::f32::consts::TAU * radius;
                    writeln!(svg, "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{radius}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"1.7\" stroke-linecap=\"round\" stroke-dasharray=\"{} {}\" transform=\"rotate(-90 {cx} {cy})\"/>", css_color(*fill), alpha(*fill), fraction * length, length).unwrap();
                }
            }
            DrawCommand::Spinner {
                rect,
                color,
                angle,
                variant,
            } => {
                write_spinner(&mut svg, *rect, *color, *angle, *variant);
            }
            DrawCommand::RasterImage {
                rect,
                image,
                opacity,
                radius,
            } => {
                let mime = match image.format() {
                    RasterImageFormat::Png => "image/png",
                    RasterImageFormat::Jpeg => "image/jpeg",
                    RasterImageFormat::Webp => "image/webp",
                    RasterImageFormat::Gif => "image/gif",
                };
                writeln!(
                    svg,
                    "<defs><clipPath id=\"raster-{index}\"><rect {} rx=\"{}\"/></clipPath></defs><image {} opacity=\"{}\" preserveAspectRatio=\"xMidYMid meet\" clip-path=\"url(#raster-{index})\" xlink:href=\"data:{mime};base64,{}\"><title>{}</title></image>",
                    rect_attributes(*rect), radius.max(0.0), rect_attributes(*rect), opacity.clamp(0.0, 1.0),
                    xml_escape(image.data_base64()), xml_escape(image.alt())
                ).unwrap();
            }
            DrawCommand::EffortSlider { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "SVG inspection does not implement the animated effort shader",
                ));
            }
        }
    }
    for _ in 0..clip_depth {
        svg.push_str("</g>\n");
    }
    svg.push_str("</svg>\n");
    fs::write(path, svg)
}

fn rect_attributes(rect: Rect) -> String {
    format!(
        "x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
        rect.x,
        rect.y,
        rect.width.max(0.0),
        rect.height.max(0.0)
    )
}

fn css_color(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn alpha(color: Color) -> f32 {
    f32::from(color.a) / 255.0
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn corner_rect_path(rect: Rect, radii: CornerRadii) -> String {
    let clamp = |radius: f32| radius.clamp(0.0, rect.width.min(rect.height) / 2.0);
    let tl = clamp(radii.top_left);
    let tr = clamp(radii.top_right);
    let br = clamp(radii.bottom_right);
    let bl = clamp(radii.bottom_left);
    let left = rect.x;
    let right = rect.x + rect.width;
    let top = rect.y;
    let bottom = rect.y + rect.height;
    format!(
        "M {} {top} H {} A {tr} {tr} 0 0 1 {right} {} V {} A {br} {br} 0 0 1 {} {bottom} H {} A {bl} {bl} 0 0 1 {left} {} V {} A {tl} {tl} 0 0 1 {} {top} Z",
        left + tl,right - tr,top + tr,bottom - br,right - br,left + bl,bottom - bl,top + tl,left + tl,
    )
}

// Embedded artwork can specify only a viewBox (Material Icon Theme), or carry
// explicit dimensions (the UI glyphs). Size the root in both cases; never change
// a descendant rect's width/height, or let a nested SVG inherit the full canvas.
fn set_svg_root_attribute(svg: &mut String, name: &str, value: &str) {
    let root = svg
        .find("<svg")
        .expect("embedded icon must have an SVG root")
        + 4;
    let bytes = svg.as_bytes();
    let mut cursor = root;
    while cursor < bytes.len() {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if matches!(bytes.get(cursor), None | Some(b'>' | b'/')) {
            break;
        }
        let attribute_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'>' | b'/'))
        {
            cursor += 1;
        }
        let attribute_end = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        assert_eq!(
            bytes.get(cursor),
            Some(&b'='),
            "invalid embedded SVG attribute"
        );
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let quote = bytes[cursor];
        assert!(
            matches!(quote, b'\'' | b'"'),
            "unquoted embedded SVG attribute"
        );
        cursor += 1;
        let value_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
            cursor += 1;
        }
        assert!(cursor < bytes.len(), "unterminated embedded SVG attribute");
        if &svg[attribute_start..attribute_end] == name {
            svg.replace_range(value_start..cursor, value);
            return;
        }
        cursor += 1;
    }
    svg.insert_str(root, &format!(" {name}=\"{value}\""));
}

fn write_spinner(svg: &mut String, rect: Rect, color: Color, angle: f32, variant: SpinnerVariant) {
    let side = rect.width.min(rect.height);
    if !side.is_finite() || side <= 0.0 {
        return;
    }
    let width = (side * 0.12).clamp(1.5, 2.8);
    let radius = (side / 2.0 - width / 2.0 - 1.0).max(0.0);
    let center_x = rect.x + rect.width / 2.0;
    let center_y = rect.y + rect.height / 2.0;
    let mut points = String::new();
    match variant {
        SpinnerVariant::Circle => {
            let sweep = std::f32::consts::TAU * 5.0 / 6.0;
            let steps = ((radius * sweep / 1.4).ceil() as usize).clamp(16, 80);
            for step in 0..=steps {
                let theta =
                    angle - std::f32::consts::FRAC_PI_2 + sweep * step as f32 / steps as f32;
                write!(
                    points,
                    "{},{} ",
                    center_x + radius * theta.cos(),
                    center_y + radius * theta.sin()
                )
                .unwrap();
            }
            writeln!(svg, "<polyline points=\"{points}\" fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{width}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>", css_color(color), alpha(color)).unwrap();
        }
        SpinnerVariant::Spokes => {
            for index in 0..8 {
                let theta = angle + std::f32::consts::TAU * index as f32 / 8.0;
                let (dx, dy) = (theta.cos(), theta.sin());
                writeln!(svg, "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{width}\" stroke-linecap=\"round\"/>", center_x + radius * 0.55 * dx, center_y + radius * 0.55 * dy, center_x + radius * dx, center_y + radius * dy, css_color(color), alpha(color)).unwrap();
            }
        }
    }
}

// The test exporter uses the same artwork mapping as the native host. These
// assets are embedded so a snapshot never depends on a machine-local URL.
fn icon_svg(icon: Icon) -> &'static str {
    match icon {
        Icon::PanelLeft => include_str!("../../assets/bootstrap/layout-sidebar.svg"),
        Icon::PanelLeftOpen => include_str!("../../assets/bootstrap/layout-sidebar-inset.svg"),
        Icon::PanelLeftClose => include_str!("../../assets/bootstrap/layout-sidebar-inset.svg"),
        Icon::PanelBottom => include_str!("../../assets/bootstrap/window-dock.svg"),
        Icon::PanelRight => include_str!("../../assets/bootstrap/layout-sidebar-reverse.svg"),
        Icon::PanelRightOpen => include_str!("../../assets/bootstrap/layout-sidebar-inset-reverse.svg"),
        Icon::PanelRightClose => include_str!("../../assets/bootstrap/layout-sidebar-inset-reverse.svg"),
        Icon::Add => include_str!("../../assets/bootstrap/plus-lg.svg"),
        Icon::Minus => include_str!("../../assets/bootstrap/dash.svg"),
        Icon::Microphone => include_str!("../../assets/bootstrap/mic.svg"),
        Icon::Text => include_str!("../../assets/bootstrap/card-text.svg"),
        Icon::Image => include_str!("../../assets/bootstrap/image.svg"),
        Icon::Audio => include_str!("../../assets/bootstrap/soundwave.svg"),
        Icon::Video => include_str!("../../assets/bootstrap/camera-video.svg"),
        Icon::File => include_str!("../../assets/bootstrap/file-earmark-text.svg"),
        Icon::FileType(file) => file.svg(),
        Icon::Infinity => include_str!("../../assets/bootstrap/infinity.svg"),
        Icon::Paperclip => include_str!("../../assets/bootstrap/paperclip.svg"),
        Icon::Target => include_str!("../../assets/bootstrap/bullseye.svg"),
        Icon::Plan => include_str!("../../assets/bootstrap/list-check.svg"),
        Icon::Brain => include_str!("../../assets/bootstrap/lightbulb.svg"),
        Icon::Check => include_str!("../../assets/bootstrap/check-lg.svg"),
        Icon::InfoCircle => include_str!("../../assets/bootstrap/info-circle.svg"),
        Icon::CheckCircle => include_str!("../../assets/bootstrap/check-circle.svg"),
        Icon::WarningTriangle => include_str!("../../assets/bootstrap/exclamation-triangle.svg"),
        Icon::ErrorCircle => include_str!("../../assets/bootstrap/x-circle.svg"),
        Icon::Settings => include_str!("../../assets/bootstrap/gear.svg"),
        Icon::Github => include_str!("../../assets/bootstrap/github.svg"),
        // Application artwork carries its own source.
        Icon::Custom(source) => source,
        Icon::Chat => include_str!("../../assets/bootstrap/chat-dots.svg"),
        Icon::Folder => include_str!("../../assets/bootstrap/folder.svg"),
        Icon::Laptop => include_str!("../../assets/bootstrap/laptop.svg"),
        Icon::Globe => include_str!("../../assets/bootstrap/globe.svg"),
        Icon::GitBranch => include_str!("../../assets/bootstrap/git.svg"),
        Icon::Grid => include_str!("../../assets/bootstrap/grid.svg"),
        Icon::Palette => include_str!("../../assets/bootstrap/palette.svg"),
        Icon::Users => include_str!("../../assets/bootstrap/people.svg"),
        Icon::Puzzle => include_str!("../../assets/bootstrap/puzzle.svg"),
        Icon::Book => include_str!("../../assets/bootstrap/book.svg"),
        Icon::Server => include_str!("../../assets/bootstrap/hdd-stack.svg"),
        Icon::Bolt => include_str!("../../assets/bootstrap/lightning-charge.svg"),
        Icon::ChartBar => include_str!("../../assets/bootstrap/bar-chart.svg"),
        Icon::Keyboard => include_str!("../../assets/bootstrap/keyboard.svg"),
        Icon::Hash => include_str!("../../assets/bootstrap/hash.svg"),
        Icon::Code => include_str!("../../assets/bootstrap/code-slash.svg"),
        Icon::Terminal => include_str!("../../assets/bootstrap/terminal.svg"),
        Icon::ArrowUp => include_str!("../../assets/bootstrap/arrow-up.svg"),
        Icon::Send => include_str!("../../assets/bootstrap/arrow-up.svg"),
        Icon::ArrowDown => include_str!("../../assets/bootstrap/arrow-down.svg"),
        Icon::ArrowLeft => include_str!("../../assets/bootstrap/arrow-left.svg"),
        Icon::ArrowRight => include_str!("../../assets/bootstrap/arrow-right.svg"),
        Icon::ChevronDown => include_str!("../../assets/bootstrap/chevron-down.svg"),
        Icon::ChevronRight => include_str!("../../assets/bootstrap/chevron-right.svg"),
        Icon::Search => include_str!("../../assets/bootstrap/search.svg"),
        Icon::Play => include_str!("../../assets/bootstrap/play.svg"),
        Icon::Pause => include_str!("../../assets/bootstrap/pause.svg"),
        Icon::Stop => include_str!("../../assets/bootstrap/stop.svg"),
        Icon::StopFill => include_str!("../../assets/bootstrap/stop-fill.svg"),
        Icon::History => include_str!("../../assets/bootstrap/clock-history.svg"),
        Icon::Close => include_str!("../../assets/bootstrap/x-lg.svg"),
        Icon::CloseBold => include_str!("../../assets/bootstrap/x.svg"),
        Icon::CloseCircle => include_str!("../../assets/bootstrap/x-circle.svg"),
        Icon::Refresh => include_str!("../../assets/bootstrap/arrow-clockwise.svg"),
        Icon::Menu => include_str!("../../assets/bootstrap/list.svg"),
        Icon::More => include_str!("../../assets/bootstrap/three-dots.svg"),
        Icon::Pin => include_str!("../../assets/bootstrap/pin-angle.svg"),
        Icon::Archive => include_str!("../../assets/bootstrap/archive.svg"),
        Icon::ArchiveUp => include_str!("../../assets/bootstrap/box-arrow-up.svg"),
        Icon::Trash => include_str!("../../assets/bootstrap/trash3.svg"),
        Icon::Edit => include_str!("../../assets/bootstrap/pencil.svg"),
        Icon::Unread => include_str!("../../assets/bootstrap/chat-left-dots.svg"),
        Icon::Filter => include_str!("../../assets/bootstrap/funnel.svg"),
        Icon::Sort => include_str!("../../assets/bootstrap/sort-down.svg"),
        Icon::Collapse => include_str!("../../assets/bootstrap/arrows-angle-contract.svg"),
        Icon::Expand => include_str!("../../assets/bootstrap/arrows-angle-expand.svg"),

        Icon::Clock => include_str!("../../assets/bootstrap/clock.svg"),
        Icon::Bell => include_str!("../../assets/bootstrap/bell.svg"),
        Icon::BellFill => include_str!("../../assets/bootstrap/bell-fill.svg"),
        Icon::BellSlash => include_str!("../../assets/bootstrap/bell-slash.svg"),
        Icon::FolderFill => include_str!("../../assets/bootstrap/folder-fill.svg"),
        Icon::PlayFill => include_str!("../../assets/bootstrap/play-fill.svg"),
        Icon::GridFill => include_str!("../../assets/bootstrap/grid-fill.svg"),
        Icon::SettingsFill => include_str!("../../assets/bootstrap/gear-fill.svg"),
        Icon::PaletteFill => include_str!("../../assets/bootstrap/palette-fill.svg"),
        Icon::LaptopFill => include_str!("../../assets/bootstrap/laptop-fill.svg"),
        Icon::TerminalFill => include_str!("../../assets/bootstrap/terminal-fill.svg"),
        Icon::KeyboardFill => include_str!("../../assets/bootstrap/keyboard-fill.svg"),
        Icon::ChatFill => include_str!("../../assets/bootstrap/chat-dots-fill.svg"),
        Icon::UsersFill => include_str!("../../assets/bootstrap/people-fill.svg"),
        Icon::PuzzleFill => include_str!("../../assets/bootstrap/puzzle-fill.svg"),
        Icon::ServerFill => include_str!("../../assets/bootstrap/hdd-stack-fill.svg"),
        Icon::BookFill => include_str!("../../assets/bootstrap/book-fill.svg"),
        Icon::ClockFill => include_str!("../../assets/bootstrap/clock-fill.svg"),
        Icon::BoltFill => include_str!("../../assets/bootstrap/lightning-charge-fill.svg"),
        Icon::ArchiveFill => include_str!("../../assets/bootstrap/archive-fill.svg"),
        Icon::ChartBarFill => include_str!("../../assets/bootstrap/bar-chart-fill.svg"),
    }
}

#[test]
#[ignore = "export native alert variants for manual visual review"]
fn export_alert_visual_review() {
    use openide_ui::{
        Alert, AlertTone, Button, Component, ComponentPalette, Element, Size, UiTree,
    };

    let output = std::path::PathBuf::from(format!(
        "{}/../../target/alert-visual-qa",
        env!("CARGO_MANIFEST_DIR")
    ));
    fs::create_dir_all(&output).unwrap();
    let light = ComponentPalette {
        surface: Color::rgb(245, 245, 245),
        raised: Color::rgb(255, 255, 255),
        hover: Color::rgb(234, 234, 234),
        selected: Color::rgb(225, 225, 225),
        border: Color::rgb(205, 205, 205),
        text: Color::rgb(35, 35, 35),
        muted: Color::rgb(100, 100, 100),
        shadow: Color::rgba(0, 0, 0, 30),
    };
    let cases = [
        (AlertTone::Neutral, "Aviso", "Los cambios se aplicarán a las próximas sesiones."),
        (AlertTone::Info, "Información del proyecto", "La carpeta activa define los archivos disponibles para esta tarea."),
        (AlertTone::Success, "Cambios guardados", "Las preferencias de la terminal se actualizaron correctamente."),
        (AlertTone::Warning, "Se requiere revisión", "La sesión se interrumpió antes de completar la operación. Revisa los cambios antes de continuar."),
        (AlertTone::Error, "No se pudo guardar", "No se pudo escribir el archivo de configuración. Comprueba que la carpeta exista y que tengas permiso para modificarla; después vuelve a intentar."),
    ];
    for (name, palette) in [
        ("dark", crate::theme::component_palette()),
        ("light", light),
    ] {
        for width in [360.0, 760.0] {
            let mut rows = vec![Element::text(format!("Alert · {name} · {width:.0} px"))
                .font_family(FontFamily::Sans)
                .font_size(20.0)
                .foreground(palette.text)];
            for (index, (tone, title, message)) in cases.iter().enumerate() {
                let mut alert = Alert::new(*title, *message, palette)
                    .tone(*tone)
                    .compact(width < 520.0);
                if *tone == AlertTone::Error {
                    alert =
                        alert.action(Button::new("alert_retry", "Reintentar", palette).render());
                }
                if matches!(tone, AlertTone::Info | AlertTone::Success) {
                    alert = alert.dismiss(format!("alert_dismiss_{index}"));
                }
                rows.push(alert.render());
            }
            let mut tree = UiTree::new(
                Element::panel(Element::column(rows).gap(16.0))
                    .padding(20.0)
                    .background(palette.surface),
            );
            tree.set_reduced_motion(true);
            let height = 800.0;
            let frame = tree.layout(Size::new(width, height));
            let path = output.join(format!("alert-{name}-{width:.0}.svg"));
            write_frame_svg(&frame, &path, width, height).unwrap();
            let mut options = resvg::usvg::Options::default();
            options.fontdb_mut().load_system_fonts();
            let parsed =
                resvg::usvg::Tree::from_str(&fs::read_to_string(&path).unwrap(), &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            eprintln!("{}", path.display());
        }
    }
}

#[test]
fn svg_root_dimensions_are_inserted_without_changing_descendant_geometry() {
    for mut svg in [
        "<svg viewBox=\"0 0 32 32\"><rect width=\"24\" height=\"18\"/></svg>".to_owned(),
        "<svg\nwidth = '32'\theight=\"32\" viewBox=\"0 0 32 32\"><rect width=\"24\" height=\"18\"/></svg>".to_owned(),
    ] {
        set_svg_root_attribute(&mut svg, "width", "16");
        set_svg_root_attribute(&mut svg, "height", "16");
        set_svg_root_attribute(&mut svg, "x", "7");
        set_svg_root_attribute(&mut svg, "x", "11");
        assert!(svg.contains("<rect width=\"24\" height=\"18\"/>"));
        assert!(svg.contains("viewBox=\"0 0 32 32\""));
        assert_eq!(svg.matches(" x=").count(), 1);
        assert!(svg.contains(" x=\"11\""));
        let parsed = resvg::usvg::Tree::from_str(&svg, &Default::default()).unwrap();
        assert_eq!(parsed.size().width(), 16.0);
        assert_eq!(parsed.size().height(), 16.0);
    }
}

#[test]
fn exported_file_icons_stay_inside_native_icon_bounds_at_both_scales() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("icon.svg");
    let icons = openide_ui::FileIcon::catalog()
        .map(Icon::FileType)
        .chain([Icon::File])
        .chain(crate::provider_icons::PROVIDER_ICONS);
    for icon in icons {
        let mut frame = Frame::default();
        frame.commands.push(DrawCommand::Icon {
            rect: Rect::new(31.0, 17.0, 30.0, 46.0),
            icon,
            tint: Color::WHITE,
        });
        write_native_frame_svg(&frame, &path, 96.0, 80.0).unwrap();
        let svg = fs::read_to_string(&path).unwrap();
        let parsed = resvg::usvg::Tree::from_str(&svg, &Default::default()).unwrap();
        for scale in [1, 2] {
            let mut image = resvg::tiny_skia::Pixmap::new(96 * scale, 80 * scale).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::from_scale(scale as f32, scale as f32),
                &mut image.as_mut(),
            );
            let mut visible = 0;
            let mut colored = 0;
            for (index, pixel) in image.pixels().iter().enumerate() {
                if pixel.alpha() == 0 {
                    continue;
                }
                visible += 1;
                colored +=
                    usize::from(pixel.red() != pixel.green() || pixel.green() != pixel.blue());
                let x = index as u32 % image.width();
                let y = index as u32 / image.width();
                // Native centering in the larger layout box yields 38,32 and
                // a 16px square. Pixel bounds must not inherit the page size.
                assert!(
                    (38 * scale..54 * scale).contains(&x) && (32 * scale..48 * scale).contains(&y),
                    "{icon:?} painted outside its native bounds at {scale}x: {x},{y}"
                );
            }
            assert!(visible > 0, "{icon:?} vanished at {scale}x");
            if icon == openide_ui::file_icon("main.rs") {
                assert!(
                    colored > 0,
                    "Material artwork must retain its source colors"
                );
            }
        }
    }
}

#[test]
fn svg_export_preserves_commands_and_parses_with_native_svg_backend() {
    use openide_ui::Point;

    let mut frame = Frame::default();
    frame.commands.extend([
        DrawCommand::PushClip(Rect::new(7.0, 11.0, 160.0, 90.0)),
        DrawCommand::FillRect {
            rect: Rect::new(17.0, 23.0, 89.0, 31.0),
            color: Color::rgba(38, 40, 42, 180),
            radius: 8.0,
        },
        DrawCommand::Text {
            position: Point::new(19.0, 25.0),
            text: "Provider <API> & \"Model\"".into(),
            color: Color::rgb(240, 240, 240),
            font_size: 13.0,
            font_family: FontFamily::Sans,
        },
        DrawCommand::Icon {
            rect: Rect::new(113.0, 29.0, 16.0, 16.0),
            icon: crate::provider_icons::OPENAI,
            tint: Color::rgb(200, 210, 220),
        },
        DrawCommand::PopClip,
    ]);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("frame.svg");
    write_frame_svg(&frame, &path, 200.0, 120.0).unwrap();
    let svg = fs::read_to_string(&path).unwrap();
    assert!(svg.contains("x=\"17\" y=\"23\" width=\"89\" height=\"31\""));
    assert!(svg.contains("Provider &lt;API&gt; &amp; &quot;Model&quot;"));
    assert!(svg.contains("clip-path=\"url(#clip-0)\""));
    assert!(svg.contains("#c8d2dc"));
    let parsed = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
    assert_eq!(parsed.size().width(), 200.0);
    assert_eq!(parsed.size().height(), 120.0);
}

#[test]
fn native_font_svg_export_loads_the_bundled_sans_and_monospace_faces() {
    let options = native_svg_options();
    for expected in ["Ubuntu", "Hack"] {
        assert!(
            options
                .fontdb
                .faces()
                .any(|face| { face.families.iter().any(|(family, _)| family == expected) }),
            "missing native SVG face: {expected}"
        );
    }
    let mut frame = Frame::default();
    for (family, y) in [(FontFamily::Sans, 10.0), (FontFamily::Monospace, 32.0)] {
        frame.commands.push(DrawCommand::Text {
            position: openide_ui::Point::new(8.0, y),
            text: "main.rs · completada".into(),
            color: Color::WHITE,
            font_size: 13.0,
            font_family: family,
        });
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.svg");
    write_native_frame_svg(&frame, &path, 200.0, 80.0).unwrap();
    let svg = fs::read_to_string(path).unwrap();
    assert!(svg.contains("font-family=\"Ubuntu,sans-serif\""));
    assert!(svg.contains("font-family=\"Hack,monospace\""));
    let parsed = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
    assert_eq!(parsed.size().width(), 200.0);
    assert_eq!(parsed.size().height(), 80.0);
}

/// Presentation-only route review. Real Settings builders and native font
/// measurements are used; no account, filesystem discovery or provider request
/// is started by these synthetic snapshots. The SVG renderer remains a layout
/// aid rather than a pixel-identical capture of egui.
#[test]
#[ignore = "export all Settings routes with native fonts at compact and desktop widths"]
fn export_settings_density_visual_review() {
    use super::{build_settings_page, build_settings_sidebar, SettingsPage, SETTINGS_GROUPS};
    use openide_ui::{Element, Size};

    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/settings-density-review");
    fs::create_dir_all(&output).unwrap();
    let options = native_svg_options();
    let height = 840.0;
    let mut index = String::from(
        "<!doctype html><meta charset=\"utf-8\"><title>Settings density review</title>\
         <style>body{font:14px system-ui;background:#171717;color:#ddd;padding:24px}\
         a{color:#a8d4ff}li{margin:8px 0}img{max-width:100%;border:1px solid #555}</style>\
         <h1>Settings · native font layout review</h1>\
         <p>Ubuntu/Hack metrics; synthetic data, no services. SVG/PNG layout exports, not host pixel screenshots.</p><ul>",
    );
    let mut export = |frame: &Frame, width: f32, name: &str| {
        let path = output.join(format!("{name}.svg"));
        write_native_frame_svg(frame, &path, width, height).unwrap();
        let parsed =
            resvg::usvg::Tree::from_str(&fs::read_to_string(&path).unwrap(), &options).unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
        resvg::render(
            &parsed,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap.save_png(path.with_extension("png")).unwrap();
        writeln!(
            index,
            "<li><a href=\"{name}.png\">{name}</a> · <a href=\"{name}.svg\">SVG</a></li>"
        )
        .unwrap();
    };
    let scene = |state: &crate::view::ViewState, width: f32| {
        let sidebar = (width >= 1000.0).then_some(244.0);
        let center_width = width - sidebar.unwrap_or(0.0);
        let page = build_settings_page(state, state.settings_page, center_width, sidebar.is_some());
        let body = if let Some(sidebar_width) = sidebar {
            Element::row(vec![build_settings_sidebar(state, sidebar_width), page])
        } else {
            page
        };
        body.background(crate::theme::BACKGROUND)
            .size(Some(width), Some(height))
    };

    for width in [360.0, 620.0, 1300.0] {
        for page in SETTINGS_GROUPS
            .iter()
            .flat_map(|(_, pages)| pages.iter().copied())
        {
            let mut state = crate::view::tests::empty_state();
            state.show_settings = true;
            state.settings_page = page;
            state.workspace = "/workspace/openADE".into();
            state.workspace_draft = state.workspace.clone();
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(scene(&state, width));
            let viewport = Size::new(width, height);
            export(
                &tree.layout(viewport),
                width,
                &format!("{page:?}-{width:.0}-top"),
            );
            if matches!(
                page,
                SettingsPage::Agent
                    | SettingsPage::Providers
                    | SettingsPage::Shortcuts
                    | SettingsPage::Usage
                    | SettingsPage::Memory
                    | SettingsPage::Subagents
            ) {
                tree.scroll_to_end(&format!("settings_page:{page:?}"));
                export(
                    &tree.layout(viewport),
                    width,
                    &format!("{page:?}-{width:.0}-bottom"),
                );
            }
            // These exports consume draw commands, not a GPU texture atlas.
            let mut output = context.end_pass();
            output.textures_delta.clear();
        }
        let mut state = crate::view::tests::empty_state();
        state.show_settings = true;
        state.settings_nav_open = true;
        state.settings_page = SettingsPage::General;
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(scene(&state, width));
        let viewport = Size::new(width, height);
        export(
            &tree.layout(viewport),
            width,
            &format!("navigation-{width:.0}-top"),
        );
        tree.scroll_to_end(if width >= 1000.0 {
            "settings_navigation"
        } else {
            "settings_page:General"
        });
        export(
            &tree.layout(viewport),
            width,
            &format!("navigation-{width:.0}-bottom"),
        );
        state.settings_nav_query = "atajos".into();
        tree.set_root(scene(&state, width));
        tree.scroll_to_start("settings_navigation");
        tree.scroll_to_start("settings_page:General");
        export(
            &tree.layout(viewport),
            width,
            &format!("navigation-{width:.0}-filtered"),
        );
        let mut output = context.end_pass();
        output.textures_delta.clear();
    }
    drop(export);
    index.push_str("</ul>");
    fs::write(output.join("index.html"), index).unwrap();
    eprintln!("{}", output.join("index.html").display());
}
