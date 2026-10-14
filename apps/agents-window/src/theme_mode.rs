//! The window is designed dark: every color in the code is a dark one. A
//! light theme is that same frame with its colors carried over, each one's
//! lightness put where its role sits on a light surface and its hue kept.
//! It is done to the commands of a frame on their way to being painted, so
//! nothing else knows there are two themes.
//!
//! Pictures are left as they are: a page of the browser, an image.

use openide_ui::{Color, DrawCommand, Frame};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const FILE: &str = "theme";

/// What the user asked for; the system's own theme decides the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeChoice {
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }
}

// Dark until told otherwise: what every test of the window draws.
static CHOICE: AtomicU8 = AtomicU8::new(2);
static SYSTEM_LIGHT: AtomicBool = AtomicBool::new(false);

pub(crate) fn choice() -> ThemeChoice {
    ThemeChoice::ALL[usize::from(CHOICE.load(Ordering::Relaxed)).min(2)]
}

pub(crate) fn set_choice(choice: ThemeChoice) {
    CHOICE.store(ThemeChoice::ALL.iter().position(|known| *known == choice).unwrap_or(2) as u8, Ordering::Relaxed);
}

/// What the desktop says its theme is, as the window learns it each frame.
pub(crate) fn set_system_light(light: bool) {
    SYSTEM_LIGHT.store(light, Ordering::Relaxed);
}

/// Whether what is painted now is the light theme.
pub(crate) fn light() -> bool {
    match choice() {
        ThemeChoice::Light => true,
        ThemeChoice::Dark => false,
        ThemeChoice::System => SYSTEM_LIGHT.load(Ordering::Relaxed),
    }
}

/// The theme chosen on this machine; dark when none was.
pub(crate) fn load(root: &Path) {
    let saved = std::fs::read_to_string(root.join(FILE)).ok();
    set_choice(saved.as_deref().map(str::trim).and_then(ThemeChoice::from_key).unwrap_or(ThemeChoice::Dark));
}

pub(crate) fn save(root: &Path, choice: ThemeChoice) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join(FILE), choice.key())
}

/// Where a lightness of the dark design sits in the light one: the canvas,
/// the panels, the raised surfaces, the borders, the muted and the plain
/// text, each kept apart from its neighbours as it is in the dark.
const CURVE: [(f32, f32); 12] = [
    (0.0, 255.0),
    (22.0, 250.0),
    (32.0, 244.0),
    (43.0, 238.0),
    (54.0, 228.0),
    (78.0, 205.0),
    (110.0, 150.0),
    (136.0, 112.0),
    (160.0, 92.0),
    (212.0, 32.0),
    (235.0, 20.0),
    (255.0, 12.0),
];

fn along(points: impl Iterator<Item = (f32, f32)> + Clone, at: f32) -> f32 {
    let mut before = points.clone().next().unwrap_or((0.0, 0.0));
    for point in points {
        if at <= point.0 {
            let span = point.0 - before.0;
            return if span <= 0.0 { point.1 } else { before.1 + (point.1 - before.1) * (at - before.0) / span };
        }
        before = point;
    }
    before.1
}

fn lightness(dark: f32) -> f32 {
    along(CURVE.into_iter(), dark.clamp(0.0, 255.0))
}

/// The gray of the dark design that the light theme paints as `light`: for
/// a picture of a theme that must look the same in either.
pub(crate) fn gray_painted_as(light: u8) -> u8 {
    along(CURVE.into_iter().rev().map(|(dark, light)| (light, dark)), f32::from(light)).round() as u8
}

/// A color of the dark design as the light theme paints it.
pub(crate) fn lit(color: Color) -> Color {
    let (r, g, b) = (f32::from(color.r), f32::from(color.g), f32::from(color.b));
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let was = (high + low) / 2.0;
    let now = lightness(was);
    if high - low < 1.0 {
        let gray = now.round() as u8;
        return Color::rgba(gray, gray, gray, color.a);
    }
    // The hue and how vivid it is stay; only how light it is moves.
    let chroma = |lightness: f32| 255.0 - (2.0 * lightness - 255.0).abs();
    let saturation = (high - low) / chroma(was).max(1.0);
    let spread = saturation * chroma(now);
    let floor = now - spread / 2.0;
    let channel = |value: f32| (floor + (value - low) / (high - low) * spread).round().clamp(0.0, 255.0) as u8;
    Color::rgba(channel(r), channel(g), channel(b), color.a)
}

/// Carries every color of a frame over to the light theme.
pub(crate) fn recolor(frame: &mut Frame) {
    for command in &mut frame.commands {
        match command {
            DrawCommand::FillRect { color, .. }
            | DrawCommand::FillCornerRect { color, .. }
            | DrawCommand::SelectionContour { color, .. }
            | DrawCommand::CornerMask { color, .. }
            | DrawCommand::StrokeRect { color, .. }
            | DrawCommand::StrokeCornerRect { color, .. }
            | DrawCommand::FillTriangle { color, .. }
            | DrawCommand::Polyline { color, .. }
            | DrawCommand::Text { color, .. }
            | DrawCommand::Spinner { color, .. } => *color = lit(*color),
            DrawCommand::Icon { tint, .. } => *tint = lit(*tint),
            DrawCommand::FillVerticalGradient { top, bottom, .. } => {
                *top = lit(*top);
                *bottom = lit(*bottom);
            }
            DrawCommand::Record { fill, .. } => *fill = lit(*fill),
            DrawCommand::ProgressRing { track, fill, .. } => {
                *track = lit(*track);
                *fill = lit(*fill);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_light_theme_keeps_surfaces_apart_text_readable_and_hues_where_they_were() {
        let gray = |value: u8| lit(Color::rgb(value, value, value)).r;
        // Canvas, panel, raised, selected: still four surfaces, now light.
        let surfaces = [22, 32, 43, 54].map(gray);
        assert!(surfaces.windows(2).all(|pair| pair[0] > pair[1]) && surfaces[3] > 220, "{surfaces:?}");
        // Text and muted text are dark on them, the plain one darker.
        assert!(gray(212) < 40 && gray(136) < 130 && gray(212) < gray(136));
        // A link stays blue and becomes dark enough to read on a light surface.
        let link = lit(Color::rgb(120, 169, 255));
        assert!(link.b > link.g && link.g > link.r && link.b < 200, "{link:?}");
        // Transparency is the color's own.
        assert_eq!(lit(Color::rgba(0, 0, 0, 125)).a, 125);
        // A picture of a theme asks for the gray that ends as the one it wants.
        for wanted in [250u8, 238, 112, 32] {
            assert!(gray(gray_painted_as(wanted)).abs_diff(wanted) <= 2, "{wanted}");
        }
    }

    #[test]
    fn a_choice_is_named_and_read_back() {
        for choice in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_key(choice.key()), Some(choice));
        }
        assert_eq!(ThemeChoice::from_key("sepia"), None);
    }
}
